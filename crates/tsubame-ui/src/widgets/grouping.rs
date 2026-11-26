//! Cached flat rows shared by grouped list and grid views.
//! Rebuild after sorting/filtering, never during row painting. Item numbers are
//! source indices, independent of header positions and collapse state.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Header(usize),
    Item(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group<K> {
    pub key: K,
    pub items: Vec<usize>,
    pub collapsed: bool,
}

#[derive(Clone, Debug)]
pub struct Grouping<K> {
    groups: Vec<Group<K>>,
    rows: Vec<Row>,
    item_rows: Vec<Option<usize>>,
    navigable: Vec<usize>,
}

impl<K: Ord + Clone> Default for Grouping<K> {
    fn default() -> Self {
        Self {
            groups: Vec::new(),
            rows: Vec::new(),
            item_rows: Vec::new(),
            navigable: Vec::new(),
        }
    }
}

impl<K: Ord + Clone> Grouping<K> {
    /// Input order is the current item sort. Stable grouping preserves that
    /// order within every group; descending reverses only group keys.
    /// `source_len` bounds source indices, including filtered-out items.
    pub fn rebuild(
        &mut self,
        source_len: usize,
        items: impl IntoIterator<Item = (usize, K)>,
        descending: bool,
    ) {
        let mut buckets = std::collections::BTreeMap::<K, Vec<usize>>::new();
        for (item, key) in items {
            assert!(item < source_len, "group item outside source");
            buckets.entry(key).or_default().push(item);
        }
        let old: std::collections::BTreeSet<K> = std::mem::take(&mut self.groups)
            .into_iter()
            .filter(|g| g.collapsed)
            .map(|g| g.key)
            .collect();
        self.groups = buckets
            .into_iter()
            .map(|(key, items)| {
                let collapsed = old.contains(&key);
                Group {
                    key,
                    items,
                    collapsed,
                }
            })
            .collect();
        if descending {
            self.groups.reverse();
        }
        self.item_rows.clear();
        self.item_rows.resize(source_len, None);
        self.flatten();
    }

    pub fn groups(&self) -> &[Group<K>] {
        &self.groups
    }
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
    pub fn item(&self, row: usize) -> Option<usize> {
        match self.rows.get(row) {
            Some(Row::Item(item)) => Some(*item),
            _ => None,
        }
    }
    pub fn row(&self, item: usize) -> Option<usize> {
        self.item_rows.get(item).copied().flatten()
    }
    /// Visible items only, suitable for marquee, invert and type-ahead.
    /// Ctrl+A must continue using the complete source selection model.
    pub fn visible_items(&self) -> impl Iterator<Item = usize> + '_ {
        self.navigable.iter().map(|&row| match self.rows[row] {
            Row::Item(item) => item,
            Row::Header(_) => unreachable!(),
        })
    }
    /// Next selectable row; headers and collapsed items are skipped.
    pub fn next_item(&self, row: usize, forward: bool) -> Option<usize> {
        if forward {
            self.navigable
                .get(self.navigable.partition_point(|&r| r <= row))
                .copied()
        } else {
            self.navigable
                .partition_point(|&r| r < row)
                .checked_sub(1)
                .map(|i| self.navigable[i])
        }
    }
    pub fn set_collapsed(&mut self, group: usize, collapsed: bool, all: bool) -> bool {
        if group >= self.groups.len() {
            return false;
        }
        let mut changed = false;
        for (i, g) in self.groups.iter_mut().enumerate() {
            if (all || i == group) && g.collapsed != collapsed {
                g.collapsed = collapsed;
                changed = true;
            }
        }
        if changed {
            self.flatten();
        }
        changed
    }
    pub fn toggle(&mut self, group: usize, all: bool) -> bool {
        let Some(g) = self.groups.get(group) else {
            return false;
        };
        self.set_collapsed(group, !g.collapsed, all)
    }
    fn flatten(&mut self) {
        self.rows.clear();
        self.navigable.clear();
        self.item_rows.fill(None);
        for (group, g) in self.groups.iter().enumerate() {
            self.rows.push(Row::Header(group));
            if !g.collapsed {
                for &item in &g.items {
                    self.item_rows[item] = Some(self.rows.len());
                    self.navigable.push(self.rows.len());
                    self.rows.push(Row::Item(item));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_group_sort_and_mapping() {
        let mut g = Grouping::default();
        g.rebuild(5, [(3, "B"), (2, "A"), (0, "B"), (4, "A")], false);
        assert_eq!(
            g.rows(),
            [
                Row::Header(0),
                Row::Item(2),
                Row::Item(4),
                Row::Header(1),
                Row::Item(3),
                Row::Item(0)
            ]
        );
        for i in [0, 2, 3, 4] {
            assert_eq!(g.item(g.row(i).unwrap()), Some(i));
        }
        assert_eq!(g.row(1), None);
        assert_eq!(g.item(0), None);
        g.rebuild(5, [(3, "B"), (2, "A"), (0, "B")], true);
        assert_eq!(g.visible_items().collect::<Vec<_>>(), [3, 0, 2]);
    }
    #[test]
    fn collapse_navigation_and_visible_selection() {
        let mut g = Grouping::default();
        g.rebuild(4, [(0, 'A'), (1, 'A'), (2, 'B'), (3, 'B')], false);
        g.toggle(0, false);
        assert_eq!(
            g.rows(),
            [Row::Header(0), Row::Header(1), Row::Item(2), Row::Item(3)]
        );
        assert_eq!(g.row(0), None);
        assert_eq!(g.next_item(0, true), Some(2));
        assert_eq!(g.next_item(2, false), None);
        assert_eq!(g.next_item(3, false), Some(2));
        assert_eq!(g.visible_items().collect::<Vec<_>>(), [2, 3]);
        g.toggle(1, true);
        assert_eq!(g.visible_items().count(), 0);
        g.toggle(0, true);
        assert_eq!(g.visible_items().collect::<Vec<_>>(), [0, 1, 2, 3]);
    }
    #[test]
    fn rebuild_retains_collapse_by_key_and_drops_stale_groups() {
        let mut g = Grouping::default();
        g.rebuild(2, [(0, 'A'), (1, 'B')], false);
        g.toggle(0, false);
        g.rebuild(2, [(1, 'A'), (0, 'C')], true);
        assert_eq!(g.visible_items().collect::<Vec<_>>(), [0]);
        assert_eq!(g.groups().len(), 2);
        assert!(!g.toggle(2, false));
        g.rebuild(0, [], false);
        assert!(g.rows().is_empty());
    }

    #[test]
    fn hundred_thousand_items_keep_direct_mapping() {
        let mut g = Grouping::default();
        g.rebuild(100_000, (0..100_000).map(|i| (i, i % 100)), false);
        assert_eq!(g.rows().len(), 100_100);
        for i in 0..100_000 {
            assert_eq!(g.item(g.row(i).unwrap()), Some(i));
        }
        g.set_collapsed(50, true, false);
        assert_eq!(g.visible_items().count(), 99_000);
        for i in (50..100_000).step_by(100) {
            assert_eq!(g.row(i), None);
        }
    }
}
