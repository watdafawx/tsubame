//! Bounded LRU: hard caps on entry count and on bytes (PLAN.md rule 5).
//! Intrusive doubly linked list over a slot vector; O(1) get / insert / evict.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

const NIL: usize = usize::MAX;

struct Slot<K, V> {
    key: K,
    val: V,
    bytes: usize,
    prev: usize,
    next: usize,
}

pub struct Lru<K, V> {
    map: HashMap<K, usize>,
    slots: Vec<Option<Slot<K, V>>>,
    free: Vec<usize>,
    /// most recently used
    head: usize,
    /// least recently used
    tail: usize,
    cap_items: usize,
    cap_bytes: usize,
    bytes: usize,
    pub evictions: u64,
}

impl<K: Hash + Eq + Clone, V> Lru<K, V> {
    pub fn new(cap_items: usize, cap_bytes: usize) -> Self {
        Lru { map: HashMap::new(), slots: Vec::new(), free: Vec::new(), head: NIL, tail: NIL, cap_items: cap_items.max(1), cap_bytes, bytes: 0, evictions: 0 }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn unlink(&mut self, i: usize) {
        let (p, n) = {
            let s = self.slots[i].as_ref().unwrap();
            (s.prev, s.next)
        };
        if p != NIL {
            self.slots[p].as_mut().unwrap().next = n;
        } else {
            self.head = n;
        }
        if n != NIL {
            self.slots[n].as_mut().unwrap().prev = p;
        } else {
            self.tail = p;
        }
    }

    fn push_front(&mut self, i: usize) {
        {
            let s = self.slots[i].as_mut().unwrap();
            s.prev = NIL;
            s.next = self.head;
        }
        if self.head != NIL {
            self.slots[self.head].as_mut().unwrap().prev = i;
        }
        self.head = i;
        if self.tail == NIL {
            self.tail = i;
        }
    }

    /// Looks up and marks as most recently used.
    pub fn get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = *self.map.get(k)?;
        if self.head != i {
            self.unlink(i);
            self.push_front(i);
        }
        self.slots[i].as_ref().map(|s| &s.val)
    }

    pub fn contains<Q>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.contains_key(k)
    }

    /// Inserts (or replaces), then evicts least recently used entries until
    /// both caps hold. An entry bigger than the byte cap is not kept.
    pub fn insert(&mut self, k: K, v: V, bytes: usize) {
        if let Some(&i) = self.map.get(&k) {
            self.unlink(i);
            let s = self.slots[i].take().unwrap();
            self.bytes -= s.bytes;
            self.map.remove(&k);
            self.free.push(i);
        }
        if bytes > self.cap_bytes {
            self.evictions += 1;
            return;
        }
        let slot = Slot { key: k.clone(), val: v, bytes, prev: NIL, next: NIL };
        let i = match self.free.pop() {
            Some(i) => {
                self.slots[i] = Some(slot);
                i
            }
            None => {
                self.slots.push(Some(slot));
                self.slots.len() - 1
            }
        };
        self.map.insert(k, i);
        self.bytes += bytes;
        self.push_front(i);
        while self.map.len() > self.cap_items || self.bytes > self.cap_bytes {
            self.evict_one();
        }
    }

    /// Drops an entry (not counted as an eviction).
    pub fn remove<Q>(&mut self, k: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if let Some(i) = self.map.remove(k) {
            self.unlink(i);
            let s = self.slots[i].take().unwrap();
            self.bytes -= s.bytes;
            self.free.push(i);
        }
    }

    fn evict_one(&mut self) {
        let i = self.tail;
        if i == NIL {
            return;
        }
        self.unlink(i);
        let s = self.slots[i].take().unwrap();
        self.bytes -= s.bytes;
        self.map.remove(&s.key);
        self.free.push(i);
        self.evictions += 1;
    }

}

#[cfg(test)]
mod tests {
    use super::Lru;

    #[test]
    fn evicts_least_recently_used_by_count() {
        let mut l: Lru<String, u32> = Lru::new(3, usize::MAX);
        for (i, k) in ["a", "b", "c"].iter().enumerate() {
            l.insert(k.to_string(), i as u32, 1);
        }
        assert_eq!(l.get("a"), Some(&0)); // a is now most recent; b is oldest
        l.insert("d".into(), 3, 1);
        assert_eq!(l.len(), 3);
        assert!(!l.contains("b"), "b was least recently used");
        assert!(l.contains("a") && l.contains("c") && l.contains("d"));
        assert_eq!(l.evictions, 1);
    }

    #[test]
    fn evicts_by_bytes_and_rejects_oversize() {
        let mut l: Lru<u32, ()> = Lru::new(100, 10);
        for k in 0..5 {
            l.insert(k, (), 4);
        }
        assert!(l.bytes() <= 10);
        assert_eq!(l.len(), 2);
        assert!(l.contains(&3) && l.contains(&4));
        l.insert(9, (), 11);
        assert!(!l.contains(&9), "bigger than the whole cap");
        assert!(l.bytes() <= 10);
    }

    #[test]
    fn remove_frees_bytes_and_slot() {
        let mut l: Lru<u32, u32> = Lru::new(8, 100);
        l.insert(1, 1, 30);
        l.insert(2, 2, 30);
        l.remove(&1);
        l.remove(&1);
        assert_eq!((l.len(), l.bytes(), l.evictions), (1, 30, 0));
        l.insert(3, 3, 30);
        assert_eq!(l.slots.len(), 2, "freed slot reused");
        assert_eq!(l.get(&2), Some(&2));
    }

    #[test]
    fn replace_and_reuse_slots_stay_bounded() {
        let mut l: Lru<u32, u32> = Lru::new(64, usize::MAX);
        for k in 0..10_000u32 {
            l.insert(k % 200, k, 1);
            if k % 3 == 0 {
                let _ = l.get(&(k % 50));
            }
        }
        assert_eq!(l.len(), 64);
        assert!(l.slots.len() <= 65, "slots are reused, not grown: {}", l.slots.len());
        l.insert(5, 1, 1);
        l.insert(5, 2, 1);
        assert_eq!(l.get(&5), Some(&2));
        assert_eq!(l.len(), 64);
    }
}
