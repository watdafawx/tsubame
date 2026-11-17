//! Portable, allocation-amortized directory listing and view state.
use std::cmp::Ordering;
pub type EntryId = u32;
pub const FOLDER: u16 = 1;
pub const HIDDEN: u16 = 2;
pub const SYSTEM: u16 = 4;
pub const READONLY: u16 = 8;
pub const REPARSE: u16 = 16;
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EntryRecord {
    pub size: u64,
    pub modified: i64,
    offset: u32,
    length: u16,
    pub flags: u16,
}
const _: () = assert!(std::mem::size_of::<EntryRecord>() == 24);
#[derive(Clone, Copy)]
pub struct EntryInput<'a> {
    pub name: &'a [u16],
    pub size: u64,
    pub modified: i64,
    pub flags: u16,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Column {
    #[default]
    Name,
    Size,
    Modified,
    Type,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sort {
    pub column: Column,
    pub descending: bool,
    pub folders_first: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterMode {
    #[default]
    Substring,
    Glob,
}
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    StaleGeneration,
    NameTooLong,
    CapacityExceeded,
}
#[derive(Default)]
pub struct EntryStore {
    names: Vec<u16>,
    records: Vec<EntryRecord>,
    index: Vec<u32>,
    generation: u64,
    sort: Sort,
    query: Vec<u16>,
    mode: FilterMode,
}
impl EntryStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_capacity(entries: usize, name_units: usize) -> Self {
        Self {
            names: Vec::with_capacity(name_units),
            records: Vec::with_capacity(entries),
            index: Vec::with_capacity(entries),
            ..Self::default()
        }
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Invalidates outstanding batches and IDs. Retains allocation for reuse.
    pub fn clear(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("generation exhausted");
        self.names.clear();
        self.records.clear();
        self.index.clear();
        self.query.clear();
    }
    pub fn record(&self, id: EntryId) -> Option<&EntryRecord> {
        self.records.get(id as usize)
    }
    pub fn name(&self, id: EntryId) -> Option<&[u16]> {
        self.record(id)
            .map(|r| &self.names[r.offset as usize..r.offset as usize + r.length as usize])
    }
    pub fn view(&self) -> &[EntryId] {
        &self.index
    }
    pub fn at(&self, position: usize) -> Option<EntryId> {
        self.index.get(position).copied()
    }
    pub fn memory_bytes(&self) -> usize {
        self.names.capacity() * 2
            + self.records.capacity() * 24
            + self.index.capacity() * 4
            + self.query.capacity() * 2
    }
    /// Atomic validation, then append and sort/merge only matching new entries.
    pub fn push_batch(
        &mut self,
        generation: u64,
        batch: &[EntryInput<'_>],
    ) -> Result<(), StoreError> {
        if generation != self.generation {
            return Err(StoreError::StaleGeneration);
        }
        let mut units = self.names.len();
        for e in batch {
            if e.name.len() > u16::MAX as usize {
                return Err(StoreError::NameTooLong);
            }
            units = units
                .checked_add(e.name.len())
                .ok_or(StoreError::CapacityExceeded)?;
        }
        if units > u32::MAX as usize
            || self
                .len()
                .checked_add(batch.len())
                .is_none_or(|n| n > u32::MAX as usize)
        {
            return Err(StoreError::CapacityExceeded);
        }
        let start = self.len();
        for e in batch {
            self.records.push(EntryRecord {
                offset: self.names.len() as u32,
                length: e.name.len() as u16,
                size: e.size,
                modified: e.modified,
                flags: e.flags,
            });
            self.names.extend_from_slice(e.name);
        }
        let mut added: Vec<_> = (start..self.len())
            .map(|i| i as u32)
            .filter(|&i| self.matches(i))
            .collect();
        self.sort_ids(&mut added);
        let old = std::mem::take(&mut self.index);
        self.index = Vec::with_capacity(old.len() + added.len());
        let (mut a, mut b) = (0, 0);
        while a < old.len() && b < added.len() {
            if self.compare(old[a], added[b]) != Ordering::Greater {
                self.index.push(old[a]);
                a += 1;
            } else {
                self.index.push(added[b]);
                b += 1;
            }
        }
        self.index.extend_from_slice(&old[a..]);
        self.index.extend_from_slice(&added[b..]);
        Ok(())
    }
    fn compare(&self, a: u32, b: u32) -> Ordering {
        let ra = &self.records[a as usize];
        let rb = &self.records[b as usize];
        if self.sort.folders_first {
            let o = (rb.flags & FOLDER).cmp(&(ra.flags & FOLDER));
            if o != Ordering::Equal {
                return o;
            }
        }
        let o = match self.sort.column {
            Column::Name => natural(self.name(a).unwrap(), self.name(b).unwrap()),
            Column::Size => ra.size.cmp(&rb.size),
            Column::Modified => ra.modified.cmp(&rb.modified),
            Column::Type => natural(
                extension(self.name(a).unwrap()),
                extension(self.name(b).unwrap()),
            ),
        };
        (if self.sort.descending { o.reverse() } else { o }).then(a.cmp(&b))
    }
    fn sort_ids(&self, ids: &mut [u32]) {
        // Decorate once: avoid random record/name reads during most comparisons.
        let mut keys: Vec<(u64, u32)> = ids
            .iter()
            .map(|&id| {
                let r = &self.records[id as usize];
                let key = match self.sort.column {
                    Column::Size => r.size,
                    Column::Modified => (r.modified as u64) ^ (1 << 63),
                    Column::Name => prefix(self.name(id).unwrap()),
                    Column::Type => prefix(extension(self.name(id).unwrap())),
                };
                (key, id)
            })
            .collect();
        keys.sort_unstable_by(|&(ka, a), &(kb, b)| {
            if self.sort.folders_first {
                let o = (self.records[b as usize].flags & FOLDER)
                    .cmp(&(self.records[a as usize].flags & FOLDER));
                if o != Ordering::Equal {
                    return o;
                }
            }
            let o = ka.cmp(&kb);
            if o == Ordering::Equal {
                self.compare(a, b)
            } else if self.sort.descending {
                o.reverse()
            } else {
                o
            }
        });
        for (id, (_, value)) in ids.iter_mut().zip(keys) {
            *id = value;
        }
    }
    pub fn sort(&mut self, sort: Sort) {
        self.sort = sort;
        let mut ids = std::mem::take(&mut self.index);
        self.sort_ids(&mut ids);
        self.index = ids;
    }
    fn matches(&self, id: u32) -> bool {
        matches_name(self.name(id).unwrap(), &self.query, self.mode)
    }
    pub fn filter(&mut self, query: &str, mode: FilterMode) {
        let next: Vec<u16> = query.encode_utf16().map(fold).collect();
        let refine =
            mode == FilterMode::Substring && self.mode == mode && next.starts_with(&self.query);
        self.query = next;
        self.mode = mode;
        if refine {
            let mut ids = std::mem::take(&mut self.index);
            ids.retain(|&i| self.matches(i));
            self.index = ids;
        } else {
            let mut ids = (0..self.len() as u32)
                .filter(|&i| self.matches(i))
                .collect::<Vec<_>>();
            self.sort_ids(&mut ids);
            self.index = ids;
        }
    }
}
// Digit marker terminates key: numeric run length/value needs full comparator.
fn prefix(mut name: &[u16]) -> u64 {
    let mut bytes = [0u8; 8];
    let mut out = 0;
    while !name.is_empty() && out < 8 {
        let c = fold(name[0]);
        if (48..=57).contains(&c) {
            let n = name.iter().take_while(|&&c| (48..=57).contains(&c)).count();
            let significant = name[..n].iter().position(|&c| c != 48).unwrap_or(n);
            let digits = &name[significant..n];
            for b in [48, (digits.len() >> 8) as u8, digits.len() as u8]
                .into_iter()
                .chain(digits.iter().map(|&c| c as u8))
            {
                if out == 8 {
                    break;
                }
                bytes[out] = b;
                out += 1;
            }
            name = &name[n..];
        } else {
            bytes[out] = c.min(128) as u8;
            out += 1;
            if c >= 128 {
                break;
            }
            name = &name[1..];
        }
    }
    u64::from_be_bytes(bytes)
}
fn extension(name: &[u16]) -> &[u16] {
    name.iter()
        .rposition(|&c| c == 46)
        .map_or(&[], |i| &name[i + 1..])
}
fn fold(c: u16) -> u16 {
    if c < 128 {
        return if (65..=90).contains(&c) { c + 32 } else { c };
    }
    char::from_u32(c as u32)
        .and_then(|c| {
            let mut l = c.to_lowercase();
            let first = l.next()?;
            if l.next().is_none() && first as u32 <= 65535 {
                Some(first as u16)
            } else {
                None
            }
        })
        .unwrap_or(c)
}
/// Numeric runs compare without parsing/overflow; leading zero variants tie.
pub fn natural(mut a: &[u16], mut b: &[u16]) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    while !a.is_empty() && !b.is_empty() {
        if (48..=57).contains(&a[0]) && (48..=57).contains(&b[0]) {
            let na = a.iter().take_while(|&&c| (48..=57).contains(&c)).count();
            let nb = b.iter().take_while(|&&c| (48..=57).contains(&c)).count();
            let aa = &a[..na];
            let bb = &b[..nb];
            let aa = &aa[aa.iter().position(|&c| c != 48).unwrap_or(na)..];
            let bb = &bb[bb.iter().position(|&c| c != 48).unwrap_or(nb)..];
            let o = aa.len().cmp(&bb.len()).then_with(|| aa.cmp(bb));
            if o != Ordering::Equal {
                return o;
            }
            a = &a[na..];
            b = &b[nb..];
        } else {
            let o = fold(a[0]).cmp(&fold(b[0]));
            if o != Ordering::Equal {
                return o;
            }
            a = &a[1..];
            b = &b[1..];
        }
    }
    a.len().cmp(&b.len())
}
fn matches_name(name: &[u16], q: &[u16], mode: FilterMode) -> bool {
    if q.is_empty() {
        return true;
    }
    if mode == FilterMode::Substring {
        return name
            .windows(q.len())
            .any(|w| w.iter().zip(q).all(|(&a, &b)| fold(a) == b));
    }
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while i < name.len() {
        if j < q.len() && (q[j] == 63 || q[j] == fold(name[i])) {
            i += 1;
            j += 1;
        } else if j < q.len() && q[j] == 42 {
            star = Some(j);
            j += 1;
            retry = i;
        } else if let Some(s) = star {
            retry += 1;
            i = retry;
            j = s + 1;
        } else {
            return false;
        }
    }
    while j < q.len() && q[j] == 42 {
        j += 1;
    }
    j == q.len()
}

/// IDs keep selection intact across view edits. Rebinds automatically on listing replacement.
#[derive(Default)]
pub struct Selection {
    bits: Vec<u64>,
    epochs: Vec<u64>,
    epoch: u64,
    generation: Option<u64>,
    pub anchor: Option<EntryId>,
    pub focus: Option<EntryId>,
    count: usize,
    total: u128,
}
impl Selection {
    pub fn new() -> Self {
        Self::default()
    }
    fn bind(&mut self, s: &EntryStore) {
        if self.generation != Some(s.generation()) {
            *self = Self {
                generation: Some(s.generation()),
                ..Self::default()
            };
        }
        self.bits.resize(s.len().div_ceil(64), 0);
        self.epochs.resize(self.bits.len(), u64::MAX);
    }
    pub fn contains(&self, id: EntryId) -> bool {
        self.epochs.get(id as usize / 64) == Some(&self.epoch)
            && self
                .bits
                .get(id as usize / 64)
                .is_some_and(|b| b & (1u64 << (id % 64)) != 0)
    }
    pub fn count(&self) -> usize {
        self.count
    }
    pub fn total_size(&self) -> u128 {
        self.total
    }
    fn set(&mut self, s: &EntryStore, id: u32, on: bool) {
        if self.contains(id) == on {
            return;
        }
        let word = id as usize / 64;
        if self.epochs[word] != self.epoch {
            self.bits[word] = 0;
            self.epochs[word] = self.epoch;
        }
        self.bits[word] ^= 1u64 << (id % 64);
        let size = s.record(id).unwrap().size as u128;
        if on {
            self.count += 1;
            self.total += size;
        } else {
            self.count -= 1;
            self.total -= size;
        }
    }
    pub fn clear(&mut self) {
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("selection epoch exhausted");
        self.count = 0;
        self.total = 0;
        self.anchor = None;
        self.focus = None;
    }
    pub fn single(&mut self, s: &EntryStore, pos: usize) {
        self.bind(s);
        if let Some(id) = s.at(pos) {
            self.clear();
            self.set(s, id, true);
            self.anchor = Some(id);
            self.focus = Some(id);
        }
    }
    pub fn toggle(&mut self, s: &EntryStore, pos: usize) {
        self.bind(s);
        if let Some(id) = s.at(pos) {
            self.set(s, id, !self.contains(id));
            self.anchor = Some(id);
            self.focus = Some(id);
        }
    }
    /// Replaces selection with visible anchor-to-focus range; hidden anchor starts new range.
    pub fn extend(&mut self, s: &EntryStore, pos: usize) {
        self.bind(s);
        let Some(id) = s.at(pos) else {
            return;
        };
        let anchor = self.anchor.unwrap_or(id);
        let start = s.view().iter().position(|&i| i == anchor).unwrap_or(pos);
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("selection epoch exhausted");
        self.count = 0;
        self.total = 0;
        for &i in &s.view()[start.min(pos)..=start.max(pos)] {
            self.set(s, i, true);
        }
        self.anchor = Some(if start == pos { id } else { anchor });
        self.focus = Some(id);
    }
    pub fn select_all(&mut self, s: &EntryStore) {
        self.bind(s);
        for &id in s.view() {
            self.set(s, id, true);
        }
    }
    pub fn invert(&mut self, s: &EntryStore) {
        self.bind(s);
        for &id in s.view() {
            self.set(s, id, !self.contains(id));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn store(names: &[&str]) -> EntryStore {
        let mut s = EntryStore::new();
        for (i, n) in names.iter().enumerate() {
            let n: Vec<_> = n.encode_utf16().collect();
            s.push_batch(
                0,
                &[EntryInput {
                    name: &n,
                    size: i as u64,
                    modified: i as i64,
                    flags: 0,
                }],
            )
            .unwrap();
        }
        s
    }
    #[test]
    fn natural_edges() {
        for (a, b, o) in [
            ("a2", "a10", Ordering::Less),
            ("a02", "A2", Ordering::Equal),
            ("Ä2", "ä10", Ordering::Less),
            ("0", "000", Ordering::Equal),
            (
                "x99999999999999999999",
                "x100000000000000000000",
                Ordering::Less,
            ),
            ("a", "aa", Ordering::Less),
        ] {
            assert_eq!(
                natural(
                    &a.encode_utf16().collect::<Vec<_>>(),
                    &b.encode_utf16().collect::<Vec<_>>()
                ),
                o
            );
        }
    }
    #[test]
    fn merge_full() {
        let mut s = store(&["a10", "a02", "A2", "ä3", "a1"]);
        let merged = s.view().to_vec();
        s.filter("", FilterMode::Glob);
        assert_eq!(s.view(), merged);
        assert_eq!(merged, [4, 1, 2, 0, 3]);
        for col in [Column::Name, Column::Size, Column::Modified, Column::Type] {
            for descending in [false, true] {
                s.sort(Sort {
                    column: col,
                    descending,
                    folders_first: true,
                });
                let expected = s.view().to_vec();
                s.filter("", FilterMode::Substring);
                assert_eq!(s.view(), expected);
            }
        }
    }
    #[test]
    fn filtering() {
        let mut s = store(&["Alpha.txt", "alphabet", "Äpfel", "beta", ""]);
        s.filter("ALP", FilterMode::Substring);
        assert_eq!(s.view().len(), 2);
        s.filter("alpha.", FilterMode::Substring);
        assert_eq!(s.view(), [0]);
        s.filter("äP", FilterMode::Substring);
        assert_eq!(s.view(), [2]);
        s.filter("*a?*.txt", FilterMode::Glob);
        assert_eq!(s.view(), [0]);
        s.filter("?", FilterMode::Glob);
        assert!(s.view().is_empty());
        s.filter("", FilterMode::Substring);
        assert_eq!(s.view().len(), 5);
    }
    #[test]
    fn selection() {
        let mut s = store(&["c", "a", "b"]);
        let mut x = Selection::new();
        x.single(&s, 0);
        assert!(x.contains(1));
        s.sort(Sort {
            descending: true,
            ..Sort::default()
        });
        x.toggle(&s, 0);
        assert_eq!(x.count(), 2);
        x.extend(&s, 2);
        assert_eq!(x.count(), 3);
        assert_eq!(x.total_size(), 3);
        s.filter("b", FilterMode::Substring);
        assert!(x.contains(1));
        x.invert(&s);
        assert_eq!(x.count(), 2);
        x.select_all(&s);
        assert_eq!(x.count(), 3);
        s.clear();
        x.select_all(&s);
        assert_eq!(x.count(), 0);
    }
    #[test]
    fn validation() {
        let mut s = store(&["a"]);
        s.clear();
        assert_eq!(s.generation(), 1);
        assert_eq!(s.push_batch(0, &[]), Err(StoreError::StaleGeneration));
        let n = vec![1; 65536];
        assert_eq!(
            s.push_batch(
                1,
                &[EntryInput {
                    name: &n,
                    size: 0,
                    modified: 0,
                    flags: 0
                }]
            ),
            Err(StoreError::NameTooLong)
        );
        assert!(s.is_empty());
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[test]
    fn prefix_preserves_order_and_merge_matches_full() {
        let text = [
            "a",
            "a0",
            "a00",
            "a2",
            "a10",
            "a02x",
            "a2X",
            "Ä",
            "ä",
            "α10",
            "α2",
            "猫",
            "😀",
            "a.txt",
            "A.TXT",
            "a000000000000000001",
            "a\0",
            "",
            ".git",
            "a.2",
            "a.10",
        ];
        let names: Vec<Vec<u16>> = text.iter().map(|s| s.encode_utf16().collect()).collect();
        for a in &names {
            for b in &names {
                let key = prefix(a).cmp(&prefix(b));
                if key != Ordering::Equal {
                    assert_eq!(key, natural(a, b), "{a:?} {b:?}");
                }
            }
        }
        for column in [Column::Name, Column::Size, Column::Modified, Column::Type] {
            for descending in [false, true] {
                for folders_first in [false, true] {
                    let sort = Sort {
                        column,
                        descending,
                        folders_first,
                    };
                    let batch: Vec<_> = names
                        .iter()
                        .enumerate()
                        .map(|(i, n)| EntryInput {
                            name: n,
                            size: (i % 3) as u64,
                            modified: (i % 4) as i64 - 2,
                            flags: (i % 2) as u16,
                        })
                        .collect();
                    let mut full = EntryStore::new();
                    full.sort(sort);
                    full.push_batch(0, &batch).unwrap();
                    let mut streamed = EntryStore::new();
                    streamed.sort(sort);
                    streamed.filter("a", FilterMode::Substring);
                    for chunk in batch.chunks(3) {
                        streamed.push_batch(0, chunk).unwrap();
                    }
                    full.filter("a", FilterMode::Substring);
                    assert_eq!(full.view(), streamed.view());
                    let mut reference: Vec<_> = (0..full.len() as u32)
                        .filter(|&id| full.matches(id))
                        .collect();
                    reference.sort_by(|&a, &b| full.compare(a, b));
                    assert_eq!(reference, full.view());
                }
            }
        }
    }
    #[test]
    fn glob_backtracking_and_empty() {
        for (n, q, result) in [
            ("abc", "*?c", true),
            ("abc", "a*d", false),
            ("", "*", true),
            ("", "?", false),
            ("abc", "a**?*", true),
            ("abc", "abcd*", false),
        ] {
            assert_eq!(
                matches_name(
                    &n.encode_utf16().collect::<Vec<_>>(),
                    &q.encode_utf16().collect::<Vec<_>>(),
                    FilterMode::Glob
                ),
                result
            );
        }
    }
}
