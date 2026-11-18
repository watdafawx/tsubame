//! Event coalescing: raw `ReadDirectoryChangesW` records in, one minimal diff out.
//!
//! Pure logic, no OS calls, so every rule is unit-tested without a disk. Each
//! touched name gets a small state (`start`: existed when the window opened,
//! `now`: exists now, `dirty`: contents changed, `origin`: the name the file
//! held when the window opened, after renames). The diff is read off that state:
//!
//! | start | now | result |
//! |---|---|---|
//! | no  | no  | nothing (created and deleted inside the window cancel) |
//! | no  | yes | `Created` (or `Renamed` if the file arrived by rename) |
//! | yes | no  | `Removed` (unless the file only moved: then `Renamed` covers it) |
//! | yes | yes | `Modified` if anything touched it (delete + create of one name lands here) |
//!
//! The diff is advisory: [`super::apply_batch`] stats every changed name and
//! trusts the disk, so a stale or doubtful hint costs a stat, never a wrong row.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A raw record's action, after decoding `FILE_NOTIFY_INFORMATION`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawKind {
    Added,
    Removed,
    Modified,
    RenamedOld,
    RenamedNew,
}

/// What a name did over one window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// New name (not there when the window opened).
    Created,
    Removed,
    /// Same name, new size/time/attributes (or deleted and recreated).
    Modified,
    /// `from` became `name`; one file, so ID and selection can follow it.
    Renamed,
}

#[derive(Clone, Copy, Debug)]
struct Change {
    kind: ChangeKind,
    name_off: u32,
    name_len: u16,
    from_off: u32,
    from_len: u16,
}

/// One change, borrowed from its [`Batch`].
#[derive(Clone, Copy, Debug)]
pub struct ChangeRef<'a> {
    pub kind: ChangeKind,
    /// The affected name (for `Renamed`: the new name).
    pub name: &'a [u16],
    /// The old name for `Renamed`, empty otherwise.
    pub from: &'a [u16],
}

impl ChangeRef<'_> {
    /// `+name`, `-name`, `~name`, `from>name`: handy in tests and logs.
    pub fn describe(&self) -> String {
        let n = String::from_utf16_lossy(self.name);
        match self.kind {
            ChangeKind::Created => format!("+{n}"),
            ChangeKind::Removed => format!("-{n}"),
            ChangeKind::Modified => format!("~{n}"),
            ChangeKind::Renamed => format!("{}>{n}", String::from_utf16_lossy(self.from)),
        }
    }
}

/// The reduced diff of one coalescing window. Two `Vec`s, no per-change allocation.
#[derive(Clone, Debug)]
pub struct Batch {
    names: Vec<u16>,
    changes: Vec<Change>,
    /// Raw records folded into this batch.
    pub raw_events: u32,
    /// When the first of them arrived.
    pub first_event: Instant,
    /// When the batch was cut (`emitted - first_event` is the delivery latency).
    pub emitted: Instant,
}

impl Batch {
    pub fn len(&self) -> usize {
        self.changes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = ChangeRef<'_>> + '_ {
        self.changes.iter().map(|c| ChangeRef {
            kind: c.kind,
            name: &self.names[c.name_off as usize..c.name_off as usize + c.name_len as usize],
            from: &self.names[c.from_off as usize..c.from_off as usize + c.from_len as usize],
        })
    }
    /// Every change as `describe()` text, in order.
    pub fn describe(&self) -> Vec<String> {
        self.iter().map(|c| c.describe()).collect()
    }
}

#[derive(Debug)]
struct Slot {
    name: Box<[u16]>,
    start: bool,
    now: bool,
    dirty: bool,
    /// Slot of the name this file held when the window opened, if it moved.
    origin: Option<u32>,
}

/// Collects raw events and cuts a [`Batch`] when the window closes.
///
/// The window is a debounce: it closes `window` after the last event, but never
/// later than `max_delay` after the first, so a long burst (an archive being
/// extracted) comes out as a few batches instead of one huge late one.
#[derive(Debug)]
pub struct Coalescer {
    window: Duration,
    max_delay: Duration,
    slots: Vec<Slot>,
    map: HashMap<Box<[u16]>, u32>,
    pending_old: Option<Box<[u16]>>,
    raw: u32,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Coalescer {
    pub fn new(window: Duration, max_delay: Duration) -> Self {
        Self {
            window,
            max_delay: max_delay.max(window),
            slots: Vec::new(),
            map: HashMap::new(),
            pending_old: None,
            raw: 0,
            first: None,
            last: None,
        }
    }

    pub fn has_pending(&self) -> bool {
        self.first.is_some()
    }

    /// When the open window must be cut, or `None` when nothing is pending.
    pub fn deadline(&self) -> Option<Instant> {
        let (first, last) = (self.first?, self.last?);
        Some((last + self.window).min(first + self.max_delay))
    }

    /// Forgets everything (after an overflow the caller relists anyway).
    pub fn clear(&mut self) {
        self.slots.clear();
        self.map.clear();
        self.pending_old = None;
        self.raw = 0;
        self.first = None;
        self.last = None;
    }

    pub fn push(&mut self, kind: RawKind, name: &[u16], now: Instant) {
        self.raw += 1;
        self.first.get_or_insert(now);
        self.last = Some(now);
        if kind != RawKind::RenamedNew
            && let Some(old) = self.pending_old.take()
        {
            self.removed(&old); // a rename out of the folder: only the old name is reported
        }
        match kind {
            RawKind::Added => self.added(name),
            RawKind::Removed => self.removed(name),
            RawKind::Modified => self.modified(name),
            RawKind::RenamedOld => self.pending_old = Some(name.into()),
            RawKind::RenamedNew => match self.pending_old.take() {
                Some(old) => self.renamed(&old, name),
                None => self.added(name), // a rename into the folder
            },
        }
    }

    fn slot(&mut self, name: &[u16], start: bool) -> u32 {
        if let Some(&i) = self.map.get(name) {
            return i;
        }
        let i = self.slots.len() as u32;
        self.slots.push(Slot { name: name.into(), start, now: start, dirty: false, origin: None });
        self.map.insert(name.into(), i);
        i
    }

    fn added(&mut self, name: &[u16]) {
        let i = self.slot(name, false) as usize;
        let s = &mut self.slots[i];
        s.now = true;
        s.dirty = true;
        s.origin = None;
    }

    fn removed(&mut self, name: &[u16]) {
        let i = self.slot(name, true) as usize;
        let s = &mut self.slots[i];
        s.now = false;
        s.origin = None;
    }

    fn modified(&mut self, name: &[u16]) {
        let i = self.slot(name, true) as usize;
        let s = &mut self.slots[i];
        if s.now {
            s.dirty = true;
        } // else: a late record for a name already gone; the Removed wins
    }

    fn renamed(&mut self, from: &[u16], to: &[u16]) {
        let a = self.slot(from, true) as usize;
        let (moved_origin, moved_dirty) = {
            let s = &self.slots[a];
            if s.now { (Some(s.origin.unwrap_or(a as u32)), s.dirty) } else { (None, true) }
        };
        self.slots[a].now = false;
        self.slots[a].origin = None;
        let b = self.slot(to, false) as usize;
        let s = &mut self.slots[b];
        s.now = true;
        s.dirty = moved_dirty;
        // renamed back onto its own starting name: no longer moved
        s.origin = moved_origin.filter(|&o| o as usize != b);
    }

    /// Cuts the open window. `None` when it reduced to nothing (all cancelled).
    pub fn take_batch(&mut self, now: Instant) -> Option<Batch> {
        if let Some(old) = self.pending_old.take() {
            self.removed(&old);
        }
        let first = self.first?;
        let raw = self.raw;
        let mut covered = vec![false; self.slots.len()];
        let renamed_from = |slots: &[Slot], i: usize| -> Option<usize> {
            let s = &slots[i];
            if !s.now {
                return None;
            }
            s.origin.map(|o| o as usize).filter(|&o| o != i && slots[o].start)
        };
        for i in 0..self.slots.len() {
            if let Some(o) = renamed_from(&self.slots, i) {
                covered[o] = true;
            }
        }
        let mut names: Vec<u16> = Vec::new();
        let mut changes = Vec::new();
        let put = |names: &mut Vec<u16>, text: &[u16]| {
            let off = names.len() as u32;
            names.extend_from_slice(text);
            (off, text.len() as u16)
        };
        for (i, s) in self.slots.iter().enumerate() {
            let (kind, from) = if s.now {
                if let Some(o) = renamed_from(&self.slots, i) {
                    (ChangeKind::Renamed, Some(o))
                } else if !s.start {
                    (ChangeKind::Created, None)
                } else if s.dirty {
                    (ChangeKind::Modified, None)
                } else {
                    continue;
                }
            } else if s.start && !covered[i] {
                (ChangeKind::Removed, None)
            } else {
                continue;
            };
            let (name_off, name_len) = put(&mut names, &s.name);
            let (from_off, from_len) = match from {
                Some(o) => put(&mut names, &self.slots[o].name),
                None => (0, 0),
            };
            changes.push(Change { kind, name_off, name_len, from_off, from_len });
        }
        self.clear();
        if changes.is_empty() {
            return None;
        }
        Some(Batch { names, changes, raw_events: raw, first_event: first, emitted: now })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RawKind::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Feeds `events` and returns the diff as text.
    fn run(events: &[(RawKind, &str)]) -> Vec<String> {
        let t = Instant::now();
        let mut c = Coalescer::new(Duration::from_millis(50), Duration::from_millis(250));
        for &(k, n) in events {
            c.push(k, &w(n), t);
        }
        c.take_batch(t).map(|b| b.describe()).unwrap_or_default()
    }

    #[test]
    fn single_events() {
        assert_eq!(run(&[(Added, "a")]), ["+a"]);
        assert_eq!(run(&[(Removed, "a")]), ["-a"]);
        assert_eq!(run(&[(Modified, "a")]), ["~a"]);
    }

    #[test]
    fn create_then_delete_cancels() {
        assert!(run(&[(Added, "a"), (Modified, "a"), (Removed, "a")]).is_empty());
        // and the survivors are untouched by it
        assert_eq!(run(&[(Added, "tmp"), (Added, "keep"), (Removed, "tmp")]), ["+keep"]);
    }

    #[test]
    fn modifies_merge() {
        assert_eq!(run(&[(Modified, "a"), (Modified, "a"), (Modified, "a")]), ["~a"]);
        assert_eq!(run(&[(Added, "a"), (Modified, "a"), (Modified, "a")]), ["+a"]);
        assert_eq!(run(&[(Modified, "a"), (Removed, "a")]), ["-a"]);
        // a late Modified after the delete does not resurrect it
        assert_eq!(run(&[(Removed, "a"), (Modified, "a")]), ["-a"]);
    }

    #[test]
    fn delete_then_create_is_modified() {
        assert_eq!(run(&[(Removed, "a"), (Added, "a")]), ["~a"]);
        assert_eq!(run(&[(Removed, "a"), (Added, "a"), (Modified, "a")]), ["~a"]);
        assert_eq!(run(&[(Removed, "a"), (Added, "a"), (Removed, "a")]), ["-a"]);
        // create, delete, create again: new name overall
        assert_eq!(run(&[(Added, "a"), (Removed, "a"), (Added, "a")]), ["+a"]);
    }

    #[test]
    fn rename_pair() {
        assert_eq!(run(&[(RenamedOld, "a"), (RenamedNew, "b")]), ["a>b"]);
        // modified before and after the rename folds in
        assert_eq!(run(&[(Modified, "a"), (RenamedOld, "a"), (RenamedNew, "b"), (Modified, "b")]), ["a>b"]);
        // other names are unaffected and keep their order
        assert_eq!(run(&[(Added, "x"), (RenamedOld, "a"), (RenamedNew, "b"), (Removed, "y")]), ["+x", "a>b", "-y"]);
    }

    #[test]
    fn rename_chains_and_cancels() {
        assert_eq!(run(&[(RenamedOld, "a"), (RenamedNew, "b"), (RenamedOld, "b"), (RenamedNew, "c")]), ["a>c"]);
        // renamed back: nothing happened
        assert!(run(&[(RenamedOld, "a"), (RenamedNew, "b"), (RenamedOld, "b"), (RenamedNew, "a")]).is_empty());
        // created then renamed: a plain create of the final name
        assert_eq!(run(&[(Added, "a"), (RenamedOld, "a"), (RenamedNew, "b")]), ["+b"]);
        // renamed then deleted: the original is gone
        assert_eq!(run(&[(RenamedOld, "a"), (RenamedNew, "b"), (Removed, "b")]), ["-a"]);
        // new file takes the vacated name: the rename and a modify of the name
        let d = run(&[(RenamedOld, "a"), (RenamedNew, "b"), (Added, "a")]);
        assert_eq!(d, ["~a", "a>b"]);
    }

    #[test]
    fn unpaired_renames() {
        assert_eq!(run(&[(RenamedOld, "a")]), ["-a"]); // moved out of the folder
        assert_eq!(run(&[(RenamedNew, "b")]), ["+b"]); // moved in
        assert_eq!(run(&[(RenamedOld, "a"), (Modified, "c"), (RenamedNew, "b")]), ["-a", "~c", "+b"]);
    }

    #[test]
    fn case_only_rename_and_swap() {
        assert_eq!(run(&[(RenamedOld, "Foo.txt"), (RenamedNew, "foo.txt")]), ["Foo.txt>foo.txt"]);
        // a<->b through a temp name: two renames, no loss
        let d = run(&[
            (RenamedOld, "a"),
            (RenamedNew, "t"),
            (RenamedOld, "b"),
            (RenamedNew, "a"),
            (RenamedOld, "t"),
            (RenamedNew, "b"),
        ]);
        assert_eq!(d, ["b>a", "a>b"]);
    }

    #[test]
    fn burst_collapses() {
        let t = Instant::now();
        let mut c = Coalescer::new(Duration::from_millis(50), Duration::from_millis(250));
        for i in 0..10_000 {
            let n = w(&format!("f{i}"));
            c.push(Added, &n, t);
            c.push(Modified, &n, t);
            c.push(Modified, &n, t);
        }
        let b = c.take_batch(t).unwrap();
        assert_eq!(b.len(), 10_000);
        assert_eq!(b.raw_events, 30_000);
        assert!(b.iter().all(|c| c.kind == ChangeKind::Created));
        assert!(!c.has_pending());
    }

    #[test]
    fn deadline_is_a_debounce_capped_by_max_delay() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut c = Coalescer::new(ms(50), ms(250));
        assert_eq!(c.deadline(), None);
        c.push(Added, &w("a"), t);
        assert_eq!(c.deadline(), Some(t + ms(50)));
        c.push(Added, &w("b"), t + ms(40));
        assert_eq!(c.deadline(), Some(t + ms(90)));
        c.push(Added, &w("c"), t + ms(240));
        assert_eq!(c.deadline(), Some(t + ms(250)), "never later than first + max_delay");
        assert!(c.take_batch(t + ms(250)).is_some());
        assert_eq!(c.deadline(), None);
    }
}
