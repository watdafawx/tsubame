//! The archive index: one pass over an archive, then O(1) folder lookups, plus
//! the bounded cache that keeps recent indexes alive.
//!
//! Archives usually store only file names (`a/b/c.txt`), so folders are
//! synthesized while the index is built: every missing prefix becomes a directory
//! entry, and its `modified` time is taken from the first child that supplied one.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::read;
use super::{ArchiveError, ArchiveFormat, sanitize_inner_path};

/// Default hard cap on cached archive indexes (PLAN §3 rule 5).
pub const ARCHIVE_CACHE_ENTRIES: usize = 8;
/// Default hard cap on the memory the cached indexes may use.
pub const ARCHIVE_CACHE_BYTES: usize = 16 << 20;

/// One file or folder inside an archive, shaped for the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Display name: the last component of `inner_path`.
    pub name: String,
    /// Canonical path inside the archive (`dir/file.txt`): `/` separators, no
    /// leading or trailing separator. The archive root is `""` and is not an entry.
    pub inner_path: String,
    pub is_dir: bool,
    /// Uncompressed size in bytes (0 for folders, including synthesized ones).
    pub size: u64,
    /// Compressed size when the format reports one per entry (zip), else 0.
    pub compressed_size: u64,
    /// FILETIME ticks (100 ns since 1601-01-01 UTC), 0 when unknown: the same
    /// clock as `EntryRecord::modified`, so it drops straight into `EntryInput`.
    pub modified: i64,
}

impl ArchiveEntry {
    fn new(inner: &str, is_dir: bool, size: u64, compressed_size: u64, modified: i64) -> Self {
        let name = inner.rsplit('/').next().unwrap_or(inner);
        Self {
            name: name.to_string(),
            inner_path: inner.to_string(),
            is_dir,
            size: if is_dir { 0 } else { size },
            compressed_size: if is_dir { 0 } else { compressed_size },
            modified,
        }
    }
}

struct Node {
    entry: ArchiveEntry,
    /// Child IDs, sorted by [`ArchiveIndex::seal`]: folders first, then name.
    children: Vec<u32>,
}

/// Every entry of one archive, plus the folder tree. Build it with a cache
/// lookup ([`ArchiveIndexCache::get_or_load`]) rather than re-parsing per click.
pub struct ArchiveIndex {
    archive_path: PathBuf,
    format: ArchiveFormat,
    nodes: Vec<Node>,
    by_path: HashMap<String, u32>,
    root: Vec<u32>,
    files: usize,
    dirs: usize,
    total_size: u64,
    skipped: usize,
    bytes: usize,
}

impl ArchiveIndex {
    pub(crate) fn new(archive_path: PathBuf, format: ArchiveFormat) -> Self {
        Self {
            archive_path,
            format,
            nodes: Vec::new(),
            by_path: HashMap::new(),
            root: Vec::new(),
            files: 0,
            dirs: 0,
            total_size: 0,
            skipped: 0,
            bytes: 0,
        }
    }

    /// Adds one raw archive name. Unsafe names (zip-slip, device names, ADS) and
    /// names that collide with a folder are counted in [`Self::skipped`] and left
    /// out, so a hostile archive can never surface a path the extractor would write.
    pub(crate) fn add(&mut self, raw_name: &str, is_dir: bool, size: u64, compressed_size: u64, modified: i64) {
        // IDs are u32 like the rest of the core; a hostile archive cannot overflow them
        if self.nodes.len() >= u32::MAX as usize {
            self.skipped += 1;
            return;
        }
        let is_dir = is_dir || raw_name.ends_with(['/', '\\']);
        let inner = match sanitize_inner_path(raw_name) {
            Ok(inner) if !inner.is_empty() => inner,
            _ => {
                self.skipped += 1;
                return;
            }
        };
        let mut components: Vec<&str> = inner.split('/').collect();
        if components.pop().is_none() {
            self.skipped += 1;
            return;
        }
        let parent = match self.ensure_dirs(&components, modified) {
            Ok(parent) => parent,
            Err(()) => {
                // a file on disk already owns one of the folder names
                self.skipped += 1;
                return;
            }
        };
        if let Some(&id) = self.by_path.get(&inner) {
            let node = &mut self.nodes[id as usize];
            match (node.entry.is_dir, is_dir) {
                (true, true) => {}
                (true, false) => self.skipped += 1,
                (false, true) => {
                    // a folder wins over a file of the same name
                    self.total_size -= node.entry.size;
                    node.entry = ArchiveEntry::new(&inner, true, 0, 0, modified);
                }
                (false, false) => {
                    // duplicate file: the last one wins, as extractors do
                    self.total_size -= node.entry.size;
                    self.total_size += size;
                    node.entry.size = size;
                    node.entry.compressed_size = compressed_size;
                    node.entry.modified = modified;
                }
            }
            return;
        }
        let entry = ArchiveEntry::new(&inner, is_dir, size, compressed_size, modified);
        let id = self.nodes.len() as u32;
        self.nodes.push(Node { entry, children: Vec::new() });
        self.by_path.insert(inner, id);
        match parent {
            Some(parent) => self.nodes[parent as usize].children.push(id),
            None => self.root.push(id),
        }
        if !is_dir {
            self.total_size += size;
        }
    }

    /// Marks an entry the backend refuses to expose (symlink, special file, anti-item).
    pub(crate) fn skip(&mut self) {
        self.skipped += 1;
    }

    /// Creates the folder chain, returning the deepest folder (`None` = root), or
    /// `Err` when a file sits where a folder has to be.
    fn ensure_dirs(&mut self, components: &[&str], modified: i64) -> Result<Option<u32>, ()> {
        let mut parent = None;
        let mut path = String::new();
        for component in components {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(component);
            match self.by_path.get(&path) {
                Some(&id) => {
                    if !self.nodes[id as usize].entry.is_dir {
                        return Err(());
                    }
                    parent = Some(id);
                }
                None => {
                    if self.nodes.len() >= u32::MAX as usize {
                        return Err(());
                    }
                    // synthesized: no size, no compressed size, the first child's time
                    let entry = ArchiveEntry::new(&path, true, 0, 0, modified);
                    let id = self.nodes.len() as u32;
                    self.nodes.push(Node { entry, children: Vec::new() });
                    self.by_path.insert(path.clone(), id);
                    match parent {
                        Some(parent) => self.nodes[parent as usize].children.push(id),
                        None => self.root.push(id),
                    }
                    parent = Some(id);
                }
            }
        }
        Ok(parent)
    }

    /// Sorts every folder's children (folders first, then case-insensitive name)
    /// and measures the index for the cache cap.
    pub(crate) fn seal(&mut self) {
        let keys: Vec<(bool, String)> = self
            .nodes
            .iter()
            .map(|n| (!n.entry.is_dir, n.entry.name.to_lowercase()))
            .collect();
        for node in &mut self.nodes {
            node.children
                .sort_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]).then(a.cmp(&b)));
        }
        self.root
            .sort_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]).then(a.cmp(&b)));
        drop(keys);
        self.files = self.nodes.iter().filter(|n| !n.entry.is_dir).count();
        self.dirs = self.nodes.len() - self.files;
        let mut bytes = self.by_path.capacity() * 48 + self.root.capacity() * 4;
        for node in &self.nodes {
            bytes += std::mem::size_of::<Node>()
                + node.children.capacity() * 4
                + node.entry.inner_path.len() * 2
                + node.entry.name.len()
                + 96; // key string + hash bucket
        }
        self.bytes = bytes;
    }

    pub fn archive_path(&self) -> &Path {
        &self.archive_path
    }

    pub fn format(&self) -> ArchiveFormat {
        self.format
    }

    /// Entries the index holds: files plus folders (synthesized ones included).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn file_count(&self) -> usize {
        self.files
    }

    pub fn dir_count(&self) -> usize {
        self.dirs
    }

    /// Bytes of every file in the archive, uncompressed.
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    /// Names left out: unsafe paths, symlinks and other non-file entries.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// Approximate heap cost, used by the cache cap.
    pub fn memory_bytes(&self) -> usize {
        self.bytes
    }

    /// Direct children of `inner` (the archive root is `""`), folders first, then
    /// by name. The caller may re-sort with its own comparator.
    pub fn list_dir(&self, inner: &str) -> Result<Vec<ArchiveEntry>, ArchiveError> {
        let clean = sanitize_inner_path(inner)?;
        let ids = if clean.is_empty() {
            &self.root
        } else {
            match self.by_path.get(&clean) {
                Some(&id) if self.nodes[id as usize].entry.is_dir => &self.nodes[id as usize].children,
                _ => return Err(ArchiveError::NotFound(inner.to_string())),
            }
        };
        Ok(ids.iter().map(|&id| self.nodes[id as usize].entry.clone()).collect())
    }

    /// One entry by canonical inner path; `""` (the root) is not an entry.
    pub fn entry(&self, inner: &str) -> Option<&ArchiveEntry> {
        let clean = sanitize_inner_path(inner).ok()?;
        if clean.is_empty() {
            return None;
        }
        self.by_path.get(&clean).map(|&id| &self.nodes[id as usize].entry)
    }

    pub fn is_dir(&self, inner: &str) -> bool {
        self.entry(inner).is_some_and(|e| e.is_dir)
    }

    /// Files at or under `inner` (all files for the root). An unsafe path matches
    /// nothing.
    pub fn files_under<'a>(&'a self, inner: &'a str) -> impl Iterator<Item = &'a ArchiveEntry> + 'a {
        let (all, prefix) = match sanitize_inner_path(inner) {
            Ok(clean) if clean.is_empty() => (true, String::new()),
            Ok(clean) => (false, clean),
            // a NUL can never appear in a sanitized path, so this matches nothing
            Err(_) => (false, "\0".to_string()),
        };
        self.nodes.iter().map(|n| &n.entry).filter(move |e| {
            !e.is_dir && (all || e.inner_path == prefix || is_under(&e.inner_path, &prefix))
        })
    }

    /// Uncompressed bytes of the files at or under `inner`, for progress totals.
    pub fn size_under(&self, inner: &str) -> u64 {
        self.files_under(inner).map(|e| e.size).sum()
    }
}

pub(crate) fn is_under(path: &str, dir: &str) -> bool {
    path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/'
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexKey {
    path: PathBuf,
    len: u64,
    /// Nanoseconds since the Unix epoch: fine enough that rewriting a file within
    /// the same second still invalidates it.
    mtime: i64,
}

impl IndexKey {
    fn of(path: &Path) -> Result<Self, ArchiveError> {
        let metadata = fs::metadata(path)?;
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX));
        Ok(Self { path: path.to_path_buf(), len: metadata.len(), mtime })
    }

    /// A key that cannot be confused with a real file (for an index whose archive
    /// disappeared between parsing and inserting).
    fn untracked(path: &Path) -> Self {
        Self { path: path.to_path_buf(), len: u64::MAX, mtime: -1 }
    }
}

struct Slot {
    key: IndexKey,
    index: Arc<ArchiveIndex>,
    /// Cache tick of the last use: the smallest one is evicted first.
    used: u64,
}

/// Bounded LRU of archive indexes, so clicking through folders of a large archive
/// parses it once. Keyed by path, length and mtime: a changed archive is re-read.
/// Hard caps ([`ARCHIVE_CACHE_ENTRIES`] / [`ARCHIVE_CACHE_BYTES`]) with the oldest
/// index evicted first; an index larger than the byte cap is still returned to the
/// caller but is not kept.
pub struct ArchiveIndexCache {
    slots: Vec<Slot>,
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    tick: u64,
    hits: u64,
    misses: u64,
}

impl Default for ArchiveIndexCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveIndexCache {
    pub fn new() -> Self {
        Self::with_limits(ARCHIVE_CACHE_ENTRIES, ARCHIVE_CACHE_BYTES)
    }

    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            slots: Vec::new(),
            max_entries,
            max_bytes,
            bytes: 0,
            tick: 0,
            hits: 0,
            misses: 0,
        }
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Indexes currently kept.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Heap cost of the kept indexes, as counted against the byte cap.
    pub fn memory_bytes(&self) -> usize {
        self.bytes
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Is a fresh index for this archive kept? Does not count as a hit.
    pub fn contains(&self, archive: &Path) -> bool {
        IndexKey::of(archive).is_ok_and(|key| self.slots.iter().any(|s| s.key == key))
    }

    /// The index for `archive`, from the cache when it is current, else parsed
    /// now. The returned handle stays valid after later evictions.
    pub fn get_or_load(&mut self, archive: &Path) -> Result<Arc<ArchiveIndex>, ArchiveError> {
        let key = IndexKey::of(archive)?;
        self.tick += 1;
        if let Some(slot) = self.slots.iter_mut().find(|s| s.key == key) {
            slot.used = self.tick;
            self.hits += 1;
            return Ok(slot.index.clone());
        }
        self.misses += 1;
        let index = Arc::new(read::open_index(archive)?);
        Ok(self.insert(index))
    }

    /// Keeps an already-built index and returns a handle to it. Replaces an older
    /// index for the same file.
    fn insert(&mut self, index: Arc<ArchiveIndex>) -> Arc<ArchiveIndex> {
        let key = IndexKey::of(index.archive_path()).unwrap_or_else(|_| IndexKey::untracked(index.archive_path()));
        self.tick += 1;
        let bytes = index.memory_bytes();
        if let Some(at) = self.slots.iter().position(|s| s.key == key) {
            let old = self.slots.remove(at);
            self.bytes = self.bytes.saturating_sub(old.index.memory_bytes());
        }
        self.slots.push(Slot { key, index: index.clone(), used: self.tick });
        self.bytes += bytes;
        self.evict();
        index
    }

    /// Drops the index for one archive (its file changed or went away).
    pub fn invalidate(&mut self, archive: &Path) {
        let Some(at) = self.slots.iter().position(|s| s.key.path == archive) else {
            return;
        };
        let slot = self.slots.remove(at);
        self.bytes = self.bytes.saturating_sub(slot.index.memory_bytes());
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        self.bytes = 0;
    }

    fn evict(&mut self) {
        while self.slots.len() > 1 && (self.slots.len() > self.max_entries || self.bytes > self.max_bytes) {
            self.evict_one();
        }
        // one entry over a cap on its own: hand it to the caller, do not keep it
        if self.slots.len() > self.max_entries || self.bytes > self.max_bytes {
            self.evict_one();
        }
    }

    fn evict_one(&mut self) {
        let Some((at, _)) = self.slots.iter().enumerate().min_by_key(|(_, slot)| slot.used) else {
            return;
        };
        let slot = self.slots.remove(at);
        self.bytes = self.bytes.saturating_sub(slot.index.memory_bytes());
    }
}
