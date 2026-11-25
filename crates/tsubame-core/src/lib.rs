//! Portable, allocation-amortized directory listing and view state.
//!
//! [`archive`] is the archive VFS (zip, tar, tar.gz, 7z): browse and extract
//! without a scratch copy, with the zip-slip check and a bounded index cache.
pub mod archive;

use std::cmp::Ordering;
pub type EntryId = u32;
pub const FOLDER: u16 = 1;
pub const HIDDEN: u16 = 2;
pub const SYSTEM: u16 = 4;
pub const READONLY: u16 = 8;
pub const REPARSE: u16 = 16;
/// Cloud Files On-Demand: online-only placeholder (`FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`).
pub const CLOUD_ONLINE: u16 = 32;
/// Cloud Files On-Demand: pinned ("Always keep on this device", `FILE_ATTRIBUTE_PINNED`).
pub const CLOUD_PINNED: u16 = 64;
/// Cloud Files On-Demand: unpinned locally available placeholder (`FILE_ATTRIBUTE_UNPINNED`).
pub const CLOUD_UNPINNED: u16 = 128;
/// Symlink or directory junction (`IO_REPARSE_TAG_SYMLINK` / `IO_REPARSE_TAG_MOUNT_POINT`).
pub const SYMLINK: u16 = 256;
/// Tombstone bit of a removed record (internal; masked off every input).
const DEAD: u16 = 0x8000;
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
/// One change for [`EntryStore::apply`]. `id: Some(live id)` rewrites that entry in
/// place, name included, so a rename keeps its ID and its selection; any other
/// `id` inserts a new entry.
#[derive(Clone, Copy)]
pub struct Upsert<'a> {
    pub id: Option<EntryId>,
    pub input: EntryInput<'a>,
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
    /// Attribute flags (`HIDDEN`, `SYSTEM`) that keep an entry out of the index: the
    /// "show hidden / system files" settings. Survives `clear` like the sort does.
    hide: u16,
    /// IDs of tombstoned records, reused by `apply` inserts.
    free: Vec<u32>,
    /// Name units no live record points at (compacted by `compact_names`).
    garbage: usize,
    /// A streaming listing runs (`begin_stream`): `index[..top]` is sorted and
    /// holds the smallest visible entries so far, `index[top..]` is arrival order.
    streaming: bool,
    top: usize,
    /// The complete sorted view, saved when a filter starts on it: a filter
    /// rebuild (backspace, mode change) then filters it in order instead of
    /// sorting the matches again. Dropped by every mutation and by `sort`.
    unfiltered: Option<Vec<u32>>,
}
/// While streaming, this many leading view rows are kept in final sort order
/// (a few screens); the rest of the view is arrival order until `end_stream`.
pub const STREAM_TOP: usize = 2048;
/// Sorts at least this big run on several threads (scoped, no pool).
const PAR_SORT_MIN: usize = 16 * 1024;
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
    /// Live entries (tombstones from `apply` removals are not counted).
    pub fn len(&self) -> usize {
        self.records.len() - self.free.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Size of the ID space: every ID is `< id_bound()`, live or tombstoned.
    pub fn id_bound(&self) -> usize {
        self.records.len()
    }
    pub fn is_live(&self, id: EntryId) -> bool {
        self.records.get(id as usize).is_some_and(|r| r.flags & DEAD == 0)
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
        self.free.clear();
        self.garbage = 0;
        self.streaming = false;
        self.top = 0;
        self.unfiltered = None;
    }
    /// Starts a streaming listing: `push_batch` then appends without merging
    /// the whole view. Only the first [`STREAM_TOP`] view rows are kept in
    /// sort order (what the user sees while it loads); everything after them
    /// is in arrival order. `end_stream` sorts the whole view once. Cuts the
    /// per-batch O(n) merge (O(n * batches) natural compares per listing).
    pub fn begin_stream(&mut self) {
        if !self.streaming {
            self.streaming = true;
            self.top = self.index.len();
            if self.top > STREAM_TOP {
                // already sorted (an ordinary push before): keep it
                self.top = STREAM_TOP;
            }
        }
    }
    /// Ends a streaming listing: one (parallel) sort of the whole view.
    pub fn end_stream(&mut self) {
        if !std::mem::take(&mut self.streaming) {
            return;
        }
        if self.top < self.index.len() {
            let mut ids = std::mem::take(&mut self.index);
            self.sort_ids(&mut ids);
            self.index = ids;
        }
        self.top = 0;
    }
    /// A streaming listing runs and the view beyond the first
    /// [`STREAM_TOP`] rows is not sorted yet.
    pub fn is_streaming(&self) -> bool {
        self.streaming
    }
    pub fn record(&self, id: EntryId) -> Option<&EntryRecord> {
        self.records.get(id as usize).filter(|r| r.flags & DEAD == 0)
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
            + self.free.capacity() * 4
            + self.unfiltered.as_ref().map_or(0, |u| u.capacity() * 4)
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
        self.unfiltered = None;
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
                .records
                .len()
                .checked_add(batch.len())
                .is_none_or(|n| n > u32::MAX as usize)
        {
            return Err(StoreError::CapacityExceeded);
        }
        let start = self.records.len();
        self.records.reserve(batch.len());
        self.names.reserve(units - self.names.len());
        for e in batch {
            self.records.push(EntryRecord {
                offset: self.names.len() as u32,
                length: e.name.len() as u16,
                size: e.size,
                modified: e.modified,
                flags: e.flags & !DEAD,
            });
            self.names.extend_from_slice(e.name);
        }
        let added: Vec<_> = (start..self.records.len())
            .map(|i| i as u32)
            .filter(|&i| self.matches(i))
            .collect();
        if self.streaming {
            self.stream_ids(added);
        } else {
            self.merge_ids(added);
        }
        Ok(())
    }
    /// Reserves room for `entries` more entries and `name_units` more name
    /// units (a listing that knows its size, e.g. from the first batch).
    pub fn reserve(&mut self, entries: usize, name_units: usize) {
        self.records.reserve(entries);
        self.index.reserve(entries);
        self.names.reserve(name_units);
    }
    /// Streaming insert: new IDs that sort before the last kept top row join
    /// the sorted top (O(top + new)); the others are appended in arrival order.
    fn stream_ids(&mut self, mut added: Vec<u32>) {
        let t = self.top;
        if t == self.index.len() && t < STREAM_TOP {
            // the whole view is still the sorted top
            self.merge_ids(added);
            self.top = self.index.len().min(STREAM_TOP);
            return;
        }
        if t == 0 {
            // a refine emptied the top while the tail has rows: arrival order
            // until `end_stream` (rare: typing a filter while it loads)
            self.index.append(&mut added);
            return;
        }
        let last = self.index[t - 1];
        let lk = self.sort_key(last);
        let (mut cands, mut rest) = (Vec::new(), Vec::with_capacity(added.len()));
        for id in added.drain(..) {
            if self.cmp_keyed((self.sort_key(id), id), (lk, last)) == Ordering::Less {
                cands.push(id);
            } else {
                rest.push(id);
            }
        }
        if !cands.is_empty() {
            self.sort_ids(&mut cands);
            let mut merged = Vec::with_capacity(t + cands.len());
            let (mut a, mut b) = (0, 0);
            while a < t && b < cands.len() {
                if self.compare(self.index[a], cands[b]) != Ordering::Greater {
                    merged.push(self.index[a]);
                    a += 1;
                } else {
                    merged.push(cands[b]);
                    b += 1;
                }
            }
            merged.extend_from_slice(&self.index[a..t]);
            merged.extend_from_slice(&cands[b..]);
            // the top keeps its size; what falls out of it joins the tail
            self.index[..t].copy_from_slice(&merged[..t]);
            self.index.extend_from_slice(&merged[t..]);
        }
        self.index.append(&mut rest);
    }
    /// Sorts `added` and merges it into the index in O(n+m).
    fn merge_ids(&mut self, mut added: Vec<u32>) {
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
    }
    /// Incremental update without a relist: tombstones `removed`, rewrites or
    /// inserts `upserts`, and keeps the sorted/filtered index valid. One O(n)
    /// index pass plus an O(n+m) merge, whatever the number of changes.
    ///
    /// IDs of survivors never change. A removed ID is reused by a later insert,
    /// so the caller must drop it from its `Selection` first (`Selection::forget`).
    /// Returns the ID of each upsert, in order. Out-of-range or dead IDs in
    /// `removed` are ignored; validation happens before any mutation.
    pub fn apply(
        &mut self,
        removed: &[EntryId],
        upserts: &[Upsert<'_>],
    ) -> Result<Vec<EntryId>, StoreError> {
        // the merge below needs a fully sorted view
        self.end_stream();
        self.unfiltered = None;
        let mut units = self.names.len();
        for u in upserts {
            if u.input.name.len() > u16::MAX as usize {
                return Err(StoreError::NameTooLong);
            }
            units = units
                .checked_add(u.input.name.len())
                .ok_or(StoreError::CapacityExceeded)?;
        }
        if units > u32::MAX as usize
            || self
                .records
                .len()
                .checked_add(upserts.len())
                .is_none_or(|n| n > u32::MAX as usize)
        {
            return Err(StoreError::CapacityExceeded);
        }
        // Pull every touched ID out of the index; they are re-merged at the end.
        let mut touched = vec![0u64; self.records.len().div_ceil(64)];
        let mut any = false;
        for id in removed
            .iter()
            .copied()
            .chain(upserts.iter().filter_map(|u| u.id))
        {
            if self.is_live(id) {
                touched[id as usize / 64] |= 1u64 << (id % 64);
                any = true;
            }
        }
        if any {
            self.index
                .retain(|&i| touched[i as usize / 64] & (1u64 << (i % 64)) == 0);
        }
        for &id in removed {
            if let Some(r) = self.records.get_mut(id as usize).filter(|r| r.flags & DEAD == 0) {
                self.garbage += r.length as usize;
                *r = EntryRecord { size: 0, modified: 0, offset: 0, length: 0, flags: DEAD };
                self.free.push(id);
            }
        }
        let mut ids = Vec::with_capacity(upserts.len());
        for u in upserts {
            let e = &u.input;
            let flags = e.flags & !DEAD;
            let id = match u.id.filter(|&i| self.is_live(i)) {
                Some(id) => {
                    let r = &mut self.records[id as usize];
                    let old = r.length as usize;
                    if e.name.len() <= old {
                        let o = r.offset as usize;
                        self.names[o..o + e.name.len()].copy_from_slice(e.name);
                        self.garbage += old - e.name.len();
                    } else {
                        self.garbage += old;
                        r.offset = self.names.len() as u32;
                        self.names.extend_from_slice(e.name);
                    }
                    r.length = e.name.len() as u16;
                    r.size = e.size;
                    r.modified = e.modified;
                    r.flags = flags;
                    id
                }
                None => {
                    let rec = EntryRecord {
                        offset: self.names.len() as u32,
                        length: e.name.len() as u16,
                        size: e.size,
                        modified: e.modified,
                        flags,
                    };
                    self.names.extend_from_slice(e.name);
                    if let Some(id) = self.free.pop() {
                        self.records[id as usize] = rec;
                        id
                    } else {
                        self.records.push(rec);
                        (self.records.len() - 1) as u32
                    }
                }
            };
            ids.push(id);
        }
        let mut added = ids.clone();
        added.sort_unstable();
        added.dedup();
        added.retain(|&i| self.matches(i));
        self.merge_ids(added);
        if self.garbage > 16 * 1024 && self.garbage * 2 > self.names.len() {
            self.compact_names();
        }
        Ok(ids)
    }
    /// Name units that no live entry uses (left behind by removals and renames).
    pub fn garbage_units(&self) -> usize {
        self.garbage
    }
    /// Rewrites the name buffer without garbage. IDs and order are unchanged.
    pub fn compact_names(&mut self) {
        let mut names = Vec::with_capacity(self.names.len().saturating_sub(self.garbage));
        for r in self.records.iter_mut().filter(|r| r.flags & DEAD == 0) {
            let o = r.offset as usize;
            r.offset = names.len() as u32;
            names.extend_from_slice(&self.names[o..o + r.length as usize]);
        }
        self.names = names;
        self.garbage = 0;
    }
    /// ID of the live entry called `name`: an exact match, else a
    /// case-insensitive one (file names compare that way on Windows).
    pub fn find(&self, name: &[u16]) -> Option<EntryId> {
        self.find_many(&[name])[0]
    }
    /// [`find`](Self::find) for many names. A few names scan once each; many
    /// names hash once and scan the store a single time (O(n+m)).
    pub fn find_many(&self, names: &[&[u16]]) -> Vec<Option<EntryId>> {
        let mut out: Vec<Option<EntryId>> = vec![None; names.len()];
        let mut exact = vec![false; names.len()];
        let mut consider = |q: usize, id: u32, rn: &[u16], out: &mut Vec<Option<EntryId>>| {
            let name = names[q];
            if rn == name {
                if !exact[q] {
                    exact[q] = true;
                    out[q] = Some(id);
                }
            } else if out[q].is_none() && rn.iter().zip(name).all(|(&a, &b)| fold(a) == fold(b)) {
                out[q] = Some(id);
            }
        };
        let live = |r: &EntryRecord| r.flags & DEAD == 0;
        if names.len() <= 8 {
            for (q, name) in names.iter().enumerate() {
                for (i, r) in self.records.iter().enumerate() {
                    if live(r) && r.length as usize == name.len() {
                        let o = r.offset as usize;
                        consider(q, i as u32, &self.names[o..o + name.len()], &mut out);
                        if out[q].is_some() && self.names[o..o + name.len()] == **name {
                            break;
                        }
                    }
                }
            }
        } else {
            // query hash -> first query index; `next` chains queries with equal hashes
            let mut heads: std::collections::HashMap<u64, u32> =
                std::collections::HashMap::with_capacity(names.len());
            let mut next = vec![u32::MAX; names.len()];
            for (q, name) in names.iter().enumerate() {
                let h = fold_hash(name);
                next[q] = heads.insert(h, q as u32).unwrap_or(u32::MAX);
            }
            for (i, r) in self.records.iter().enumerate().filter(|(_, r)| live(r)) {
                let o = r.offset as usize;
                let rn = &self.names[o..o + r.length as usize];
                let Some(&head) = heads.get(&fold_hash(rn)) else {
                    continue;
                };
                let mut q = head;
                while q != u32::MAX {
                    if names[q as usize].len() == rn.len() {
                        consider(q as usize, i as u32, rn, &mut out);
                    }
                    q = next[q as usize];
                }
            }
        }
        out
    }
    fn compare(&self, a: u32, b: u32) -> Ordering {        let ra = &self.records[a as usize];
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
    /// Order-preserving 8-byte key of `id` for the current column: a key
    /// difference decides, equal keys fall back to `compare`.
    fn sort_key(&self, id: u32) -> u64 {
        let r = &self.records[id as usize];
        match self.sort.column {
            Column::Size => r.size,
            Column::Modified => (r.modified as u64) ^ (1 << 63),
            Column::Name => prefix(self.name(id).unwrap()),
            Column::Type => prefix(extension(self.name(id).unwrap())),
        }
    }
    /// `compare` on decorated IDs: folder rank, then key, then the full comparator.
    #[inline]
    fn cmp_keyed(&self, (ka, a): (u64, u32), (kb, b): (u64, u32)) -> Ordering {
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
    }
    /// Folder rank and key only (equal keys tie: `sort_ties` orders them).
    #[inline]
    fn cmp_prefix(&self, (ka, a): (u64, u32), (kb, b): (u64, u32)) -> Ordering {
        if self.sort.folders_first {
            let o = (self.records[b as usize].flags & FOLDER)
                .cmp(&(self.records[a as usize].flags & FOLDER));
            if o != Ordering::Equal {
                return o;
            }
        }
        let o = ka.cmp(&kb);
        if self.sort.descending { o.reverse() } else { o }
    }
    fn decorate_sorted(&self, ids: &[u32]) -> Vec<(u64, u32)> {
        // Decorate once: avoid random record/name reads during most comparisons.
        let mut keys: Vec<(u64, u32)> = ids.iter().map(|&id| (self.sort_key(id), id)).collect();
        self.sort_decorated(&mut keys);
        keys
    }
    /// Bucket order of a decorated ID: `cmp_prefix` as one integer.
    #[inline]
    fn rank(&self, (k, id): (u64, u32)) -> u128 {
        let back = self.sort.folders_first && self.records[id as usize].flags & FOLDER == 0;
        ((back as u128) << 64) | (if self.sort.descending { !k } else { k }) as u128
    }
    fn sort_decorated(&self, keys: &mut [(u64, u32)]) {
        keys.sort_unstable_by(|&a, &b| self.cmp_prefix(a, b));
        let mut i = 0;
        while i < keys.len() {
            let mut j = i + 1;
            while j < keys.len() && self.cmp_prefix(keys[i], keys[j]) == Ordering::Equal {
                j += 1;
            }
            if j - i > 1 {
                self.sort_ties(&mut keys[i..j]);
            }
            i = j;
        }
    }
    /// The text a name/type key is built from.
    fn key_text(&self, id: u32) -> &[u16] {
        let n = self.name(id).unwrap();
        if self.sort.column == Column::Type { extension(n) } else { n }
    }
    /// Orders a run whose folder rank and first key are equal, as `compare` would.
    fn sort_ties(&self, run: &mut [(u64, u32)]) {
        match self.sort.column {
            // the key is the whole value: equal keys tie on the ID
            Column::Size | Column::Modified => run.sort_unstable_by_key(|&(_, id)| id),
            Column::Name | Column::Type => {
                const UNCLEAN: u32 = u32::MAX;
                let mut v: Vec<(u64, u32, u32)> = run
                    .iter()
                    .map(|&(k, id)| (k, id, prefix_from(self.key_text(id), 0).1.map_or(UNCLEAN, |e| e as u32)))
                    .collect();
                self.refine_ties(&mut v);
                for (slot, e) in run.iter_mut().zip(v) {
                    slot.1 = e.1;
                }
            }
        }
    }
    /// `v` ties on everything consumed so far (`.2` is where each name's key
    /// ended): sort by the next 8-byte key, recurse into ties; the full
    /// comparator only where a key was cut or every name ran out.
    fn refine_ties(&self, v: &mut [(u64, u32, u32)]) {
        const UNCLEAN: u32 = u32::MAX;
        if v.len() < 2 {
            return;
        }
        let clean = v.iter().all(|e| e.2 != UNCLEAN);
        let mut progress = false;
        if clean {
            for e in v.iter_mut() {
                let text = self.key_text(e.1);
                let (k, end) = prefix_from(text, e.2 as usize);
                progress |= end != Some(e.2 as usize);
                e.0 = k;
                e.2 = end.map_or(UNCLEAN, |x| x as u32);
            }
        }
        if clean && !progress {
            // every text ran out with equal keys all the way: naturally equal
            v.sort_unstable_by_key(|e| e.1);
            return;
        }
        if !progress {
            v.sort_by(|a, b| self.compare(a.1, b.1));
            return;
        }
        let desc = self.sort.descending;
        v.sort_unstable_by(|a, b| {
            let o = a.0.cmp(&b.0);
            if desc { o.reverse() } else { o }
        });
        let mut i = 0;
        while i < v.len() {
            let mut j = i + 1;
            while j < v.len() && v[j].0 == v[i].0 {
                j += 1;
            }
            if j - i > 1 {
                self.refine_ties(&mut v[i..j]);
            }
            i = j;
        }
    }
    fn sort_ids(&self, ids: &mut [u32]) {
        let threads = if ids.len() >= PAR_SORT_MIN {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(8)
                .min(ids.len() / (PAR_SORT_MIN / 2))
        } else {
            1
        };
        if threads <= 1 {
            let keys = self.decorate_sorted(ids);
            for (id, (_, value)) in ids.iter_mut().zip(keys) {
                *id = value;
            }
            return;
        }
        // Parallel sample sort (scoped threads, no pool): splitters from a
        // sample of keys cut the key range into one bucket per thread; equal
        // keys always land in the same bucket, so the sorted buckets simply
        // concatenate (no merge, no cross-bucket tie compares).
        // Scratch: ~16 bytes per ID twice over, freed on return.
        let chunk = ids.len().div_ceil(threads);
        let step = (ids.len() / (threads * 64)).max(1);
        let mut sample: Vec<u128> = ids.iter().step_by(step).map(|&id| self.rank((self.sort_key(id), id))).collect();
        sample.sort_unstable();
        let splitters: Vec<u128> = (1..threads).map(|b| sample[b * sample.len() / threads]).collect();
        let bucket_of = |r: u128| splitters.partition_point(|&s| s <= r);
        // per chunk: decorated IDs split by bucket
        let parts: Vec<Vec<Vec<(u64, u32)>>> = std::thread::scope(|s| {
            let hs: Vec<_> = ids
                .chunks(chunk)
                .map(|c| {
                    let bucket_of = &bucket_of;
                    s.spawn(move || {
                        let mut out: Vec<Vec<(u64, u32)>> = (0..threads).map(|_| Vec::with_capacity(c.len() / threads + 16)).collect();
                        for &id in c {
                            let e = (self.sort_key(id), id);
                            out[bucket_of(self.rank(e))].push(e);
                        }
                        out
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().expect("sort worker")).collect()
        });
        let buckets: Vec<Vec<(u64, u32)>> = std::thread::scope(|s| {
            let parts = &parts;
            let hs: Vec<_> = (0..threads)
                .map(|b| {
                    s.spawn(move || {
                        let mut v: Vec<(u64, u32)> = Vec::with_capacity(parts.iter().map(|p| p[b].len()).sum());
                        for p in parts {
                            v.extend_from_slice(&p[b]);
                        }
                        self.sort_decorated(&mut v);
                        v
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().expect("sort worker")).collect()
        });
        drop(parts);
        let mut at = 0;
        for b in buckets {
            for (slot, (_, id)) in ids[at..at + b.len()].iter_mut().zip(&b) {
                *slot = *id;
            }
            at += b.len();
        }
    }
    pub fn sort(&mut self, sort: Sort) {
        self.sort = sort;
        self.unfiltered = None;
        let mut ids = std::mem::take(&mut self.index);
        self.sort_ids(&mut ids);
        self.index = ids;
        self.top = self.index.len().min(STREAM_TOP);
    }
    fn matches(&self, id: u32) -> bool {
        if self.records[id as usize].flags & self.hide != 0 {
            return false;
        }
        matches_name(self.name(id).unwrap(), &self.query, self.mode)
    }
    /// Which attributes hide an entry (`0` shows everything, `HIDDEN`, `SYSTEM`, or both).
    pub fn hide(&self) -> u16 {
        self.hide
    }
    /// Change the hidden/system rule and rebuild the index (O(n), only when it changed).
    pub fn set_hide(&mut self, mask: u16) {
        let mask = mask & (HIDDEN | SYSTEM);
        if mask == self.hide {
            return;
        }
        self.hide = mask;
        // the saved sorted order was built under the old rule
        self.unfiltered = None;
        let mut ids = (0..self.records.len() as u32)
            .filter(|&i| self.is_live(i) && self.matches(i))
            .collect::<Vec<_>>();
        self.sort_ids(&mut ids);
        self.index = ids;
        self.top = self.index.len().min(STREAM_TOP);
    }
    pub fn filter(&mut self, query: &str, mode: FilterMode) {
        let next: Vec<u16> = query.encode_utf16().map(fold).collect();
        let refine =
            mode == FilterMode::Substring && self.mode == mode && next.starts_with(&self.query);
        if self.query.is_empty() && !next.is_empty() && !self.streaming && self.unfiltered.is_none() {
            // the view is complete and sorted: keep its order for rebuilds
            self.unfiltered = Some(self.index.clone());
        }
        self.query = next;
        self.mode = mode;
        if refine {
            // keeps the order; while streaming, the rows kept from the sorted
            // top are still the smallest (the tail was never below them)
            let top = self.top.min(self.index.len());
            let kept_top = self.index[..top].iter().filter(|&&i| self.matches(i)).count();
            let mut ids = std::mem::take(&mut self.index);
            ids.retain(|&i| self.matches(i));
            self.index = ids;
            self.top = kept_top;
        } else {
            let ids = match self.unfiltered.as_ref().filter(|_| !self.streaming) {
                // in order already: one pass, no sort
                Some(all) => all.iter().copied().filter(|&i| self.matches(i)).collect(),
                None => {
                    let mut ids = (0..self.records.len() as u32)
                        .filter(|&i| self.is_live(i) && self.matches(i))
                        .collect::<Vec<_>>();
                    self.sort_ids(&mut ids);
                    ids
                }
            };
            self.index = ids;
            self.top = self.index.len().min(STREAM_TOP);
        }
        if self.query.is_empty() {
            self.unfiltered = None;
        }
    }
}
// Digit marker terminates key: numeric run length/value needs full comparator.
fn prefix(name: &[u16]) -> u64 {
    prefix_from(name, 0).0
}
/// The 8-byte natural-order key of `name[start..]`, and where it ended when
/// it encoded every unit it consumed exactly (no cut digit run, no unit over
/// 127): then two names with equal keys compare like their rests from there,
/// so ties can be refined with the next 8 bytes instead of the full comparator.
fn prefix_from(name: &[u16], start: usize) -> (u64, Option<usize>) {
    let mut bytes = [0u8; 8];
    let mut out = 0;
    let mut at = start.min(name.len());
    let mut clean = true;
    while at < name.len() && out < 8 {
        let rest = &name[at..];
        let c = fold(rest[0]);
        if (48..=57).contains(&c) {
            let n = rest.iter().take_while(|&&c| (48..=57).contains(&c)).count();
            let significant = rest[..n].iter().position(|&c| c != 48).unwrap_or(n);
            let digits = &rest[significant..n];
            for b in [48, (digits.len() >> 8) as u8, digits.len() as u8]
                .into_iter()
                .chain(digits.iter().map(|&c| c as u8))
            {
                if out == 8 {
                    clean = false;
                    break;
                }
                bytes[out] = b;
                out += 1;
            }
            at += n;
        } else {
            bytes[out] = c.min(128) as u8;
            out += 1;
            // over 127 is a bucket, not a value; 0 looks like the padding
            if c >= 128 || c == 0 {
                clean = false;
                break;
            }
            at += 1;
        }
    }
    (u64::from_be_bytes(bytes), clean.then_some(at))
}
/// FNV-1a over case-folded units: equal for names that differ only in case.
fn fold_hash(name: &[u16]) -> u64 {
    name.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &c| {
        (h ^ fold(c) as u64).wrapping_mul(0x0100_0000_01b3)
    })
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
        self.bits.resize(s.id_bound().div_ceil(64), 0);
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
    /// Call before `EntryStore::apply` removes `id`: deselects it and drops it as
    /// anchor/focus. Removed IDs are reused by later inserts.
    pub fn forget(&mut self, s: &EntryStore, id: EntryId) {
        self.bind(s);
        if s.is_live(id) {
            self.set(s, id, false);
        }
        if self.anchor == Some(id) {
            self.anchor = None;
        }
        if self.focus == Some(id) {
            self.focus = None;
        }
    }
    /// Call after an in-place update changed the size of selected `id` from
    /// `old_size`, so the selected-bytes total stays exact.
    pub fn resized(&mut self, s: &EntryStore, id: EntryId, old_size: u64) {
        self.bind(s);
        if self.contains(id) {
            let new = s.record(id).map_or(old_size, |r| r.size);
            self.total = self.total - old_size as u128 + new as u128;
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
    /// Replaces the selection with exactly `ids` (dead or duplicate ids are
    /// skipped). Anchor and focus are left alone: the caller sets them. Used
    /// by rubber-band selection, which recomputes the set every frame.
    pub fn replace_with(&mut self, s: &EntryStore, ids: impl IntoIterator<Item = EntryId>) {
        self.bind(s);
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("selection epoch exhausted");
        self.count = 0;
        self.total = 0;
        for id in ids {
            if s.is_live(id) {
                self.set(s, id, true);
            }
        }
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
    fn replace_with_sets_exactly() {
        let s = store(&["a", "b", "c", "d"]);
        let mut x = Selection::new();
        x.single(&s, 0);
        let (anchor, focus) = (x.anchor, x.focus);
        x.replace_with(&s, [1, 2, 2, 99]);
        assert!(!x.contains(0) && x.contains(1) && x.contains(2) && !x.contains(3));
        assert_eq!(x.count(), 2, "duplicates and dead ids ignored");
        assert_eq!(x.total_size(), 1 + 2);
        assert_eq!((x.anchor, x.focus), (anchor, focus), "anchor/focus untouched");
        x.replace_with(&s, []);
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

#[cfg(test)]
mod apply_tests {
    use super::*;
    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    fn input(name: &[u16], size: u64) -> EntryInput<'_> {
        EntryInput { name, size, modified: size as i64, flags: 0 }
    }
    fn build(names: &[(&str, u64)], sort: Sort) -> EntryStore {
        let mut s = EntryStore::new();
        s.sort(sort);
        let n: Vec<Vec<u16>> = names.iter().map(|(n, _)| w(n)).collect();
        let b: Vec<_> = n.iter().zip(names).map(|(n, (_, sz))| input(n, *sz)).collect();
        s.push_batch(0, &b).unwrap();
        s
    }
    fn listing(s: &EntryStore) -> Vec<(String, u64)> {
        s.view()
            .iter()
            .map(|&id| {
                (
                    String::from_utf16(s.name(id).unwrap()).unwrap(),
                    s.record(id).unwrap().size,
                )
            })
            .collect()
    }
    const SORTS: [Sort; 3] = [
        Sort { column: Column::Name, descending: false, folders_first: true },
        Sort { column: Column::Size, descending: true, folders_first: false },
        Sort { column: Column::Type, descending: false, folders_first: true },
    ];

    #[test]
    fn apply_matches_full_rebuild() {
        for sort in SORTS {
            for query in ["", "a"] {
                let mut s = build(
                    &[("a10", 1), ("a2", 2), ("b.txt", 3), ("c.rs", 4), ("Aa", 5), ("d", 6)],
                    sort,
                );
                s.filter(query, FilterMode::Substring);
                let b2 = w("b.txt");
                let ids = s.find_many(&[&w("a2"), &w("C.RS"), &w("nope")]);
                assert_eq!(ids[2], None);
                // remove a2, resize b.txt, rename c.rs -> ab.rs, insert x1 + a3
                let (a2, c) = (ids[0].unwrap(), ids[1].unwrap());
                let b = s.find(&b2).unwrap();
                let (n_ab, n_x, n_a3) = (w("ab.rs"), w("x1"), w("a3"));
                let out = s
                    .apply(
                        &[a2],
                        &[
                            Upsert { id: Some(b), input: input(&b2, 99) },
                            Upsert { id: Some(c), input: input(&n_ab, 4) },
                            Upsert { id: None, input: input(&n_x, 7) },
                            Upsert { id: None, input: input(&n_a3, 8) },
                        ],
                    )
                    .unwrap();
                assert_eq!(out[0], b);
                assert_eq!(out[1], c, "rename keeps the ID");
                assert_eq!(out[2], a2, "tombstoned ID is reused");
                let mut fresh = build(
                    &[("a10", 1), ("b.txt", 99), ("ab.rs", 4), ("Aa", 5), ("d", 6), ("x1", 7), ("a3", 8)],
                    sort,
                );
                fresh.filter(query, FilterMode::Substring);
                // equal sort keys tie-break by ID, which differs; compare as multisets per key
                let mut got = listing(&s);
                let mut want = listing(&fresh);
                if sort.column == Column::Size {
                    assert_eq!(got, want);
                } else {
                    got.sort();
                    want.sort();
                    assert_eq!(got, want);
                }
                assert_eq!(s.len(), 7);
            }
        }
    }

    #[test]
    fn tombstones_reuse_and_stay_out_of_views() {
        let mut s = build(&[("a", 1), ("b", 2), ("c", 3)], Sort::default());
        let mut sel = Selection::new();
        sel.single(&s, 1); // b
        sel.toggle(&s, 2); // c
        assert_eq!((sel.count(), sel.total_size()), (2, 5));
        let b = s.find(&w("b")).unwrap();
        sel.forget(&s, b);
        s.apply(&[b, b, 999], &[]).unwrap(); // duplicate and bogus ids are ignored
        assert_eq!((s.len(), s.id_bound()), (2, 3));
        assert!(!s.is_live(b));
        assert!(s.record(b).is_none() && s.name(b).is_none());
        assert_eq!((sel.count(), sel.total_size()), (1, 3));
        assert_eq!(sel.anchor, Some(2)); // toggle moved anchor to c
        // a filter rebuild must not resurrect it
        s.filter("b", FilterMode::Substring);
        assert!(s.view().is_empty());
        s.filter("", FilterMode::Substring);
        assert_eq!(s.view().len(), 2);
        // insert reuses the ID, selection bit was cleared
        let n = w("bb");
        let id = s.apply(&[], &[Upsert { id: None, input: input(&n, 10) }]).unwrap()[0];
        assert_eq!(id, b);
        assert!(!sel.contains(id));
        assert_eq!(s.id_bound(), 3);
        // resize of a selected entry keeps the total exact
        let c = s.find(&w("c")).unwrap();
        let cn = w("c");
        s.apply(&[], &[Upsert { id: Some(c), input: input(&cn, 50) }]).unwrap();
        sel.resized(&s, c, 3);
        assert_eq!(sel.total_size(), 50);
        // upsert with a dead / out of range ID inserts
        s.apply(&[id], &[]).unwrap();
        let z = w("zz");
        let r = s.apply(&[], &[Upsert { id: Some(id), input: input(&z, 1) }]).unwrap();
        assert_eq!(s.name(r[0]).unwrap(), &z[..]);
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn find_paths_agree() {
        let names: Vec<String> = (0..300).map(|i| format!("File{i:03}.TXT")).collect();
        let pairs: Vec<(&str, u64)> = names.iter().map(|n| (n.as_str(), 1)).collect();
        let mut s = build(&pairs, Sort::default());
        // exact beats case-insensitive: two names that differ only in case
        let (upper, lower) = (w("DUP"), w("dup"));
        s.apply(&[], &[Upsert { id: None, input: input(&upper, 1) }, Upsert { id: None, input: input(&lower, 1) }]).unwrap();
        let queries: Vec<Vec<u16>> = (0..300).step_by(7).map(|i| w(&format!("file{i:03}.txt"))).chain([w("dup"), w("DUP"), w("Dup"), w("missing")]).collect();
        let refs: Vec<&[u16]> = queries.iter().map(|q| q.as_slice()).collect();
        let many = s.find_many(&refs);
        for (q, got) in refs.iter().zip(&many) {
            assert_eq!(*got, s.find(q), "{:?}", String::from_utf16_lossy(q));
        }
        assert_eq!(s.name(many[many.len() - 4].unwrap()).unwrap(), &lower[..]);
        assert_eq!(s.name(many[many.len() - 3].unwrap()).unwrap(), &upper[..]);
        assert!(many[many.len() - 2].is_some());
        assert!(many[many.len() - 1].is_none());
    }

    #[test]
    fn compaction_keeps_ids_and_order() {
        let long: Vec<u16> = vec![b'x' as u16; 200];
        let mut s = EntryStore::new();
        let initial: Vec<Vec<u16>> = (0..400).map(|i| [long.clone(), w(&i.to_string())].concat()).collect();
        let b: Vec<_> = initial.iter().map(|n| input(n, 1)).collect();
        s.push_batch(0, &b).unwrap();
        let before: Vec<_> = s.view().to_vec();
        let remove: Vec<u32> = (0..400).step_by(2).collect();
        s.apply(&remove, &[]).unwrap();
        assert!(s.garbage_units() > 0);
        let keep: Vec<_> = s.view().iter().map(|&i| (i, s.name(i).unwrap().to_vec())).collect();
        s.compact_names();
        assert_eq!(s.garbage_units(), 0);
        let after: Vec<_> = s.view().iter().map(|&i| (i, s.name(i).unwrap().to_vec())).collect();
        assert_eq!(keep, after);
        assert_eq!(after.len(), 200);
        assert!(before.len() == 400);
        // churn: repeated create/remove must not grow the id space or names without bound
        for round in 0..200 {
            let n = w(&format!("{}{round}", "t".repeat(50)));
            let id = s.apply(&[], &[Upsert { id: None, input: input(&n, 1) }]).unwrap()[0];
            s.apply(&[id], &[]).unwrap();
        }
        assert_eq!(s.id_bound(), 400);
        assert_eq!(s.len(), 200);
    }

    #[test]
    fn apply_validates_before_mutating() {
        let mut s = build(&[("a", 1)], Sort::default());
        let big = vec![1u16; 65536];
        let ok = w("ok");
        let r = s.apply(
            &[0],
            &[Upsert { id: None, input: input(&ok, 1) }, Upsert { id: None, input: input(&big, 1) }],
        );
        assert_eq!(r.err(), Some(StoreError::NameTooLong));
        assert_eq!(s.len(), 1);
        assert!(s.is_live(0));
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    /// Small xorshift: deterministic random data without a dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }
    const PARTS: [&str; 14] = ["a", "A", "b", "Ä", "ä", "0", "00", "7", "10", "α", "猫", "😀", ".", "_"];
    /// Random names with heavy ties (case and leading-zero variants, shared prefixes).
    fn names(n: usize, seed: u64) -> Vec<Vec<u16>> {
        let mut r = Rng(seed);
        (0..n)
            .map(|i| {
                let mut s = String::new();
                if r.below(3) == 0 {
                    s.push_str("IMG_2026_");
                }
                for _ in 0..1 + r.below(5) {
                    s.push_str(PARTS[r.below(PARTS.len() as u64) as usize]);
                }
                if r.below(4) == 0 {
                    s.push_str(&format!("{}", i % 97));
                }
                s.encode_utf16().collect()
            })
            .collect()
    }
    fn inputs(names: &[Vec<u16>], seed: u64) -> Vec<EntryInput<'_>> {
        let mut r = Rng(seed ^ 0x9e37);
        names
            .iter()
            .map(|n| EntryInput {
                name: n,
                size: r.below(50),
                modified: r.below(40) as i64 - 20,
                flags: if r.below(10) == 0 { FOLDER } else { 0 },
            })
            .collect()
    }
    /// The naive reference: every live, matching ID sorted with the full comparator.
    fn reference(s: &EntryStore) -> Vec<u32> {
        let mut ids: Vec<u32> = (0..s.id_bound() as u32).filter(|&i| s.is_live(i) && s.matches(i)).collect();
        ids.sort_by(|&a, &b| s.compare(a, b));
        ids
    }
    fn all_sorts() -> Vec<Sort> {
        let mut v = Vec::new();
        for column in [Column::Name, Column::Size, Column::Modified, Column::Type] {
            for descending in [false, true] {
                for folders_first in [false, true] {
                    v.push(Sort { column, descending, folders_first });
                }
            }
        }
        v
    }

    #[test]
    fn filter_rebuilds_from_saved_order_match_reference() {
        let n = names(20_000, 11);
        let b = inputs(&n, 11);
        let extra = names(300, 12);
        let eb = inputs(&extra, 12);
        let mut r = Rng(99);
        let queries = ["a", "a0", "a00", "img", "img_2", "ä", "7", "*0?", "a*", ""];
        for sort in all_sorts().into_iter().step_by(3) {
            let mut s = EntryStore::new();
            s.sort(sort);
            s.begin_stream();
            s.push_batch(0, &b).unwrap();
            s.end_stream();
            for step in 0..60 {
                let q = queries[r.below(queries.len() as u64) as usize];
                let mode = if q.contains(['*', '?']) { FilterMode::Glob } else { FilterMode::Substring };
                match r.below(10) {
                    0 => s.sort(all_sorts()[r.below(16) as usize]),
                    1 => {
                        let id = r.below(s.id_bound() as u64) as u32;
                        if s.is_live(id) {
                            s.apply(&[id], &[]).unwrap();
                        }
                    }
                    2 => {
                        let e = eb[r.below(eb.len() as u64) as usize];
                        s.apply(&[], &[Upsert { id: None, input: e }]).unwrap();
                    }
                    _ => s.filter(q, mode),
                }
                assert_eq!(s.view(), &reference(&s)[..], "sort {sort:?} step {step} query {q:?}");
            }
            s.filter("", FilterMode::Substring);
            assert!(s.unfiltered.is_none(), "freed once the filter is cleared");
            assert_eq!(s.view(), &reference(&s)[..]);
        }
    }

    #[test]
    fn streaming_top_is_sorted_and_end_matches_reference() {
        let n = names(9000, 7);
        let b = inputs(&n, 7);
        for sort in all_sorts() {
            let mut s = EntryStore::new();
            s.sort(sort);
            s.begin_stream();
            let mut r = Rng(11);
            let mut at = 0;
            while at < b.len() {
                let k = (1 + r.below(700) as usize).min(b.len() - at);
                s.push_batch(0, &b[at..at + k]).unwrap();
                at += k;
                // invariant: the view's first rows are the final first rows so far
                let want = reference(&s);
                let top = s.view().len().min(STREAM_TOP);
                assert_eq!(&s.view()[..top], &want[..top], "{sort:?} after {at}");
                assert_eq!(s.view().len(), want.len());
            }
            assert!(s.is_streaming());
            s.end_stream();
            assert!(!s.is_streaming());
            assert_eq!(s.view(), reference(&s), "{sort:?}");
            // after the stream, ordinary pushes merge again
            let more = names(50, 99);
            s.push_batch(0, &inputs(&more, 3)).unwrap();
            assert_eq!(s.view(), reference(&s), "{sort:?} after end");
        }
    }

    #[test]
    fn parallel_sort_matches_naive_and_is_stable() {
        let n = names(60_000, 3);
        let b = inputs(&n, 3);
        let mut s = EntryStore::new();
        s.push_batch(0, &b).unwrap();
        for sort in all_sorts() {
            s.sort(sort);
            let want = reference(&s);
            assert_eq!(s.view(), want, "{sort:?}");
            // ties keep insertion (ID) order: equal neighbours ascend by ID
            for w in s.view().windows(2) {
                let (x, y) = (w[0], w[1]);
                let eq = {
                    let ra = s.record(x).unwrap();
                    let rb = s.record(y).unwrap();
                    let same_folder = !sort.folders_first || (ra.flags & FOLDER) == (rb.flags & FOLDER);
                    same_folder
                        && match sort.column {
                            Column::Name => natural(s.name(x).unwrap(), s.name(y).unwrap()) == Ordering::Equal,
                            Column::Size => ra.size == rb.size,
                            Column::Modified => ra.modified == rb.modified,
                            Column::Type => natural(extension(s.name(x).unwrap()), extension(s.name(y).unwrap())) == Ordering::Equal,
                        }
                };
                if eq {
                    assert!(x < y, "{sort:?}: tie {x} before {y} breaks stability");
                }
            }
        }
        // filters rebuild through the same sort
        s.sort(Sort::default());
        s.filter("a", FilterMode::Substring);
        assert_eq!(s.view(), reference(&s));
        s.filter("a*0", FilterMode::Glob);
        assert_eq!(s.view(), reference(&s));
    }

    #[test]
    fn filter_and_sort_while_streaming() {
        let n = names(6000, 5);
        let b = inputs(&n, 5);
        let mut s = EntryStore::new();
        s.begin_stream();
        s.push_batch(0, &b[..2500]).unwrap();
        // refine keeps the kept top valid; a re-sort sorts what is there
        s.filter("a", FilterMode::Substring);
        s.push_batch(0, &b[2500..4000]).unwrap();
        let want = reference(&s);
        let top = s.view().len().min(STREAM_TOP);
        assert_eq!(s.view().len(), want.len());
        // the kept top is a sorted prefix of the reference (it may be shorter)
        let kept = s.top.min(top);
        assert_eq!(&s.view()[..kept], &want[..kept]);
        s.filter("aa", FilterMode::Substring);
        s.sort(Sort { column: Column::Size, descending: true, folders_first: true });
        s.push_batch(0, &b[4000..]).unwrap();
        let want = reference(&s);
        let top = s.view().len().min(STREAM_TOP);
        assert_eq!(&s.view()[..top], &want[..top]);
        s.end_stream();
        assert_eq!(s.view(), reference(&s));
        // apply needs a sorted view: it ends a running stream first
        let mut s2 = EntryStore::new();
        s2.begin_stream();
        s2.push_batch(0, &b).unwrap();
        let x = w_("zz");
        s2.apply(&[0, 1], &[Upsert { id: None, input: EntryInput { name: &x, size: 1, modified: 0, flags: 0 } }]).unwrap();
        assert!(!s2.is_streaming());
        assert_eq!(s2.view(), reference(&s2));
        // clear resets the stream
        s2.begin_stream();
        s2.clear();
        assert!(!s2.is_streaming());
    }
    #[test]
    fn tie_refinement_across_key_boundaries() {
        // long shared prefixes, digit runs that straddle the 8-byte keys,
        // leading-zero and case variants, NUL, non-ASCII after a long prefix
        let base = [
            "IMG_2026_0001.jpg", "IMG_2026_1.jpg", "img_2026_01.JPG", "IMG_2026_10.jpg", "IMG_2026_9.jpg",
            "IMG_2026_0001.jpeg", "IMG_2026_0001", "IMG_2026_", "IMG_2026", "Report final v2.docx",
            "Report final v10.docx", "Report final V2.docx", "Report finalé.docx", "Report finale.docx",
            "abcdefgh", "abcdefgh1", "abcdefgh01", "abcdefghA", "abcdefgh ", "abcdefgh", "a", "a ",
            "x12345678901234567890", "x12345678901234567891", "x012345678901234567890", "файл 10", "файл 9",
        ];
        let mut text: Vec<String> = Vec::new();
        for round in 0..700 {
            for b in base {
                text.push(if round % 3 == 0 { b.to_string() } else { format!("{b}{}", round % 11) });
            }
        }
        let names: Vec<Vec<u16>> = text.iter().map(|t| t.encode_utf16().collect()).collect();
        let b = inputs(&names, 1);
        for take in [base.len(), 600, names.len()] {
            let mut s = EntryStore::new();
            s.push_batch(0, &b[..take]).unwrap();
            for sort in all_sorts() {
                s.sort(sort);
                assert_eq!(s.view(), reference(&s), "{sort:?} n={take}");
            }
        }
    }
    #[test]
    fn hide_rule_filters_hidden_and_system_entries() {
        let mut s = EntryStore::new();
        let g = s.generation();
        let names: Vec<Vec<u16>> = ["a.txt", "b.sys", "c.hid", "d.both"].iter().map(|n| n.encode_utf16().collect()).collect();
        let flags = [0u16, SYSTEM, HIDDEN, HIDDEN | SYSTEM];
        let batch: Vec<EntryInput> = names.iter().zip(flags).map(|(n, f)| EntryInput { name: n, size: 1, modified: 0, flags: f }).collect();
        s.push_batch(g, &batch).unwrap();
        assert_eq!(s.view().len(), 4, "default shows everything");
        s.set_hide(HIDDEN);
        assert_eq!(s.view().len(), 2, "hidden and hidden+system out");
        s.set_hide(HIDDEN | SYSTEM);
        assert_eq!(s.view().len(), 1);
        s.set_hide(SYSTEM);
        assert_eq!(s.view().len(), 2);
        s.set_hide(0);
        assert_eq!(s.view().len(), 4);
        // unknown bits are ignored
        s.set_hide(0xFF00);
        assert_eq!(s.hide(), 0);
    }

    #[test]
    fn hide_rule_applies_to_later_appends_applies_and_filters() {
        let mut s = EntryStore::new();
        s.set_hide(HIDDEN);
        let g = s.generation();
        let n1: Vec<u16> = "keep".encode_utf16().collect();
        let n2: Vec<u16> = "secret".encode_utf16().collect();
        s.push_batch(g, &[EntryInput { name: &n1, size: 1, modified: 0, flags: 0 }, EntryInput { name: &n2, size: 1, modified: 0, flags: HIDDEN }]).unwrap();
        assert_eq!(s.view().len(), 1);
        // an upsert that makes a visible file hidden removes it from the index
        let id = s.find(&n1).unwrap();
        s.apply(&[], &[Upsert { id: Some(id), input: EntryInput { name: &n1, size: 2, modified: 0, flags: HIDDEN } }]).unwrap();
        assert_eq!(s.view().len(), 0);
        // a name filter does not resurrect hidden entries
        s.filter("s", FilterMode::Substring);
        assert_eq!(s.view().len(), 0);
        s.filter("", FilterMode::Substring);
        assert_eq!(s.view().len(), 0);
        s.set_hide(0);
        assert_eq!(s.view().len(), 2);
        // the rule survives clear
        s.set_hide(HIDDEN);
        s.clear();
        assert_eq!(s.hide(), HIDDEN);
    }
    fn w_(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
}
