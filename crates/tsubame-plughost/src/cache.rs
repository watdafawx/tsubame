//! A bounded cache of column values, keyed by path.
//!
//! Two generations: new values go into the current one; when it is half the
//! cap (entries or bytes) the older generation is dropped and the current one
//! takes its place. A value that is read from the older generation moves back
//! into the current one. So the cache never holds more than the cap, recently
//! used values survive, and every operation is one or two hash lookups.

use std::collections::HashMap;
use tsubame_ext_api::Cell;

/// Fixed cost counted per entry on top of its strings.
const ENTRY_OVERHEAD: usize = 64;

pub struct CellCache {
    half_entries: usize,
    half_bytes: usize,
    cur: HashMap<String, Cell>,
    cur_bytes: usize,
    old: HashMap<String, Cell>,
    old_bytes: usize,
}

fn cost(path: &str, cell: &Cell) -> usize {
    ENTRY_OVERHEAD + path.len() + if let Cell::Text(s) = cell { s.len() } else { 0 }
}

impl CellCache {
    /// At most `max_entries` values and about `max_bytes` bytes. Zero entries
    /// switches the cache off.
    pub fn new(max_entries: usize, max_bytes: usize) -> CellCache {
        CellCache { half_entries: max_entries / 2, half_bytes: max_bytes / 2, cur: HashMap::new(), cur_bytes: 0, old: HashMap::new(), old_bytes: 0 }
    }

    pub fn len(&self) -> usize {
        self.cur.len() + self.old.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes counted against the cap.
    pub fn bytes(&self) -> usize {
        self.cur_bytes + self.old_bytes
    }

    /// Makes room in the current generation for one more value of `n` bytes.
    fn rotate_if_full(&mut self, n: usize) {
        if self.cur.len() >= self.half_entries || self.cur_bytes + n > self.half_bytes {
            self.old = std::mem::take(&mut self.cur);
            self.old_bytes = std::mem::take(&mut self.cur_bytes);
        }
    }

    pub fn get(&mut self, path: &str) -> Option<Cell> {
        if let Some(c) = self.cur.get(path) {
            return Some(c.clone());
        }
        let (k, c) = self.old.remove_entry(path)?;
        let n = cost(&k, &c);
        self.old_bytes -= n;
        self.rotate_if_full(n);
        self.cur_bytes += n;
        self.cur.insert(k, c.clone());
        Some(c)
    }

    pub fn put(&mut self, path: &str, cell: Cell) {
        let n = cost(path, &cell);
        // A value that would fill a generation alone is not worth keeping.
        if self.half_entries == 0 || n > self.half_bytes {
            return;
        }
        self.remove(path);
        self.rotate_if_full(n);
        self.cur_bytes += n;
        self.cur.insert(path.to_string(), cell);
    }

    pub fn remove(&mut self, path: &str) {
        if let Some((k, c)) = self.cur.remove_entry(path) {
            self.cur_bytes -= cost(&k, &c);
        }
        if let Some((k, c)) = self.old.remove_entry(path) {
            self.old_bytes -= cost(&k, &c);
        }
    }

    pub fn clear(&mut self) {
        *self = CellCache { half_entries: self.half_entries, half_bytes: self.half_bytes, cur: HashMap::new(), cur_bytes: 0, old: HashMap::new(), old_bytes: 0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_at_the_entry_cap() {
        let mut c = CellCache::new(100, usize::MAX);
        for i in 0..10_000 {
            c.put(&format!("C:\\f{i}"), Cell::Int(i));
            assert!(c.len() <= 100, "{} entries after {i}", c.len());
        }
        // the newest are there, the oldest are gone
        assert_eq!(c.get("C:\\f9999"), Some(Cell::Int(9999)));
        assert_eq!(c.get("C:\\f0"), None);
        assert_eq!(c.get("C:\\f9000"), None);
    }

    #[test]
    fn evicts_at_the_byte_cap() {
        let mut c = CellCache::new(usize::MAX, 64 * 1024);
        for i in 0..5_000 {
            c.put(&format!("C:\\f{i}"), Cell::Text("x".repeat(500)));
            assert!(c.bytes() <= 64 * 1024, "{} bytes after {i}", c.bytes());
        }
        assert!(c.len() < 200);
        assert!(c.get("C:\\f4999").is_some());
        // a value larger than half the cap is not stored at all
        c.put("C:\\big", Cell::Text("y".repeat(40 * 1024)));
        assert_eq!(c.get("C:\\big"), None);
        assert!(c.bytes() <= 64 * 1024);
    }

    #[test]
    fn a_read_keeps_a_value_alive() {
        let mut c = CellCache::new(8, usize::MAX);
        c.put("keep", Cell::Int(1));
        for i in 0..100 {
            c.put(&format!("f{i}"), Cell::Empty);
            assert_eq!(c.get("keep"), Some(Cell::Int(1)), "lost after {i}");
            assert!(c.len() <= 8);
        }
    }

    #[test]
    fn replace_remove_clear_and_off() {
        let mut c = CellCache::new(8, usize::MAX);
        c.put("a", Cell::Int(1));
        c.put("a", Cell::Text("two".into()));
        assert_eq!(c.len(), 1);
        assert_eq!(c.get("a"), Some(Cell::Text("two".into())));
        assert_eq!(c.bytes(), ENTRY_OVERHEAD + 1 + 3);
        c.remove("a");
        assert_eq!((c.len(), c.bytes()), (0, 0));
        c.put("b", Cell::Empty);
        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.get("b"), None);

        let mut off = CellCache::new(0, usize::MAX);
        off.put("a", Cell::Int(1));
        assert_eq!(off.get("a"), None);
        assert!(off.is_empty());
    }
}
