//! Tab model and strip geometry, no UI: an ordered list with one active tab,
//! close/reorder rules, and the width/scroll maths of the title-bar strip.

/// Ordered tabs; `active` indexes `items` (0 when empty).
pub struct Tabs<T> {
    pub items: Vec<T>,
    pub active: usize,
}

impl<T> Tabs<T> {
    pub fn new(first: T) -> Tabs<T> {
        Tabs { items: vec![first], active: 0 }
    }
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    #[allow(dead_code)]
    pub fn current(&self) -> Option<&T> {
        self.items.get(self.active)
    }
    #[allow(dead_code)]
    pub fn current_mut(&mut self) -> Option<&mut T> {
        self.items.get_mut(self.active)
    }
    /// Appends a tab at the end and activates it.
    pub fn add(&mut self, t: T) -> usize {
        self.items.push(t);
        self.active = self.items.len() - 1;
        self.active
    }
    /// Inserts a tab at `index` (clamped to the end) and activates it.
    pub fn insert(&mut self, index: usize, t: T) -> usize {
        let i = index.min(self.items.len());
        self.items.insert(i, t);
        self.active = i;
        i
    }
    pub fn activate(&mut self, i: usize) {
        if i < self.items.len() {
            self.active = i;
        }
    }
    /// Removes tab `i`. The active tab stays the same tab when another one
    /// closes; closing the active tab activates its right neighbour (or the
    /// new last one). Empty list afterwards: the window should close.
    pub fn close(&mut self, i: usize) -> Option<T> {
        if i >= self.items.len() {
            return None;
        }
        let t = self.items.remove(i);
        if i < self.active {
            self.active -= 1;
        }
        if self.active >= self.items.len() {
            self.active = self.items.len().saturating_sub(1);
        }
        Some(t)
    }
    /// Ctrl+Tab: next tab, wrapping.
    pub fn next(&mut self) {
        if !self.items.is_empty() {
            self.active = (self.active + 1) % self.items.len();
        }
    }
    /// Ctrl+Shift+Tab: previous tab, wrapping.
    pub fn prev(&mut self) {
        if !self.items.is_empty() {
            self.active = (self.active + self.items.len() - 1) % self.items.len();
        }
    }
    /// Drag reorder: moves tab `from` to slot `to`; the active tab follows
    /// whichever tab it was.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        let n = self.items.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let t = self.items.remove(from);
        self.items.insert(to, t);
        self.active = if self.active == from {
            to
        } else if from < self.active && to >= self.active {
            self.active - 1
        } else if from > self.active && to <= self.active {
            self.active + 1
        } else {
            self.active
        };
    }
}

/// Maximum closed tabs remembered per window for Ctrl+Shift+T (`tab.reopen`).
pub const CLOSED_TAB_CAP: usize = 20;

/// Bounded LIFO stack of recently closed tabs (per window). Pushing past `cap`
/// drops the oldest entry so memory stays bounded.
#[derive(Clone, Debug)]
pub struct ClosedStack<T> {
    items: Vec<T>,
    cap: usize,
}

impl<T> Default for ClosedStack<T> {
    fn default() -> Self {
        Self::with_cap(CLOSED_TAB_CAP)
    }
}

impl<T> ClosedStack<T> {
    pub fn with_cap(cap: usize) -> Self {
        Self { items: Vec::new(), cap }
    }
    pub fn push(&mut self, item: T) {
        if self.cap == 0 {
            return;
        }
        if self.items.len() >= self.cap {
            self.items.remove(0);
        }
        self.items.push(item);
    }
    pub fn pop(&mut self) -> Option<T> {
        self.items.pop()
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(n: usize) -> Tabs<usize> {
        let mut t = Tabs::new(0);
        for i in 1..n {
            t.add(i);
        }
        t
    }

    #[test]
    fn add_activates_and_close_keeps_active() {
        let mut t = tabs(4);
        assert_eq!(t.active, 3);
        t.activate(1);
        // closing a tab left of the active one shifts the index, not the tab
        assert_eq!(t.close(0), Some(0));
        assert_eq!((t.active, t.items[t.active]), (0, 1));
        // closing the active tab activates its right neighbour
        assert_eq!(t.close(0), Some(1));
        assert_eq!(t.items[t.active], 2);
        // closing the last (active) tab activates the new last
        t.activate(1);
        t.close(1);
        assert_eq!((t.active, t.len()), (0, 1));
        assert_eq!(t.close(0), Some(2));
        assert!(t.is_empty() && t.current().is_none());
        assert_eq!(t.close(0), None);
    }

    #[test]
    fn next_prev_wrap() {
        let mut t = tabs(3);
        t.next();
        assert_eq!(t.active, 0);
        t.prev();
        assert_eq!(t.active, 2);
        t.prev();
        assert_eq!(t.active, 1);
    }

    #[test]
    fn reorder_keeps_the_active_tab() {
        let mut t = tabs(5); // 0..4, active 4
        t.activate(2);
        t.move_tab(0, 3); // [1,2,3,0,4]: active tab "2" moved left
        assert_eq!(t.items, vec![1, 2, 3, 0, 4]);
        assert_eq!(t.items[t.active], 2);
        t.move_tab(1, 4); // dragging the active tab itself
        assert_eq!(t.items, vec![1, 3, 0, 4, 2]);
        assert_eq!(t.items[t.active], 2);
        t.move_tab(4, 0); // from the right of a tab to its left
        assert_eq!(t.items, vec![2, 1, 3, 0, 4]);
        assert_eq!(t.items[t.active], 2);
        t.activate(2); // tab "3"
        t.move_tab(4, 1);
        assert_eq!(t.items, vec![2, 4, 1, 3, 0]);
        assert_eq!(t.items[t.active], 3);
        t.move_tab(9, 0);
        t.move_tab(1, 1);
        assert_eq!(t.items, vec![2, 4, 1, 3, 0]);
    }

    #[test]
    fn closed_stack_is_lifo_and_bounded_to_cap() {
        let mut s = ClosedStack::default();
        assert!(s.is_empty());
        for i in 0..25usize {
            s.push(i);
        }
        assert_eq!(s.len(), CLOSED_TAB_CAP);
        assert_eq!(s.pop(), Some(24));
        assert_eq!(s.pop(), Some(23));
        let mut rest = Vec::new();
        while let Some(v) = s.pop() {
            rest.push(v);
        }
        // oldest 0..5 were evicted; last popped is 5
        assert_eq!(rest.last(), Some(&5));
        assert_eq!(rest.len(), CLOSED_TAB_CAP - 2);
        assert!(s.is_empty());
    }

    #[test]
    fn insert_places_at_clamped_index_and_activates() {
        let mut t = tabs(3); // [0, 1, 2]
        assert_eq!(t.insert(1, 99), 1);
        assert_eq!(t.items, vec![0, 99, 1, 2]);
        assert_eq!(t.active, 1);
        assert_eq!(t.insert(99, 100), 4);
        assert_eq!(t.items, vec![0, 99, 1, 2, 100]);
        assert_eq!(t.active, 4);
    }
}
