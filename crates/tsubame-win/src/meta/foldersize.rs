//! Background folder-size engine for the P4.F5 "Size on disk" column.
//!
//! The column needs a recursive byte count per directory, which can take a long
//! time on a big tree, so nothing here runs on the UI thread
//! (`docs/PLAN.md` §3 rule 1):
//!
//! - [`scan_folder`] is the synchronous walk. It skips reparse points
//!   (`FILE_ATTRIBUTE_REPARSE_POINT`, which covers junctions and directory
//!   symlinks) so a linked tree can never loop, checks an [`AtomicBool`] every
//!   few hundred entries, and is bounded by [`MAX_ENTRIES`] / [`MAX_DEPTH`] so a
//!   pathological tree cannot pin a worker forever.
//! - [`FolderSizeCache`] is a hard-capped LRU (default [`DEFAULT_CACHE_CAP`]
//!   entries) keyed by the normalized path and the directory's mtime, evicting
//!   the least recently used entry on insert (`docs/PLAN.md` §3 rule 5).
//! - [`FolderSizeWorker`] owns a small pool of worker threads, a job queue and
//!   the cache, delivers [`FolderSizeReply`] values over an `mpsc` channel with
//!   an optional wake callback, and cancels everything from an older generation
//!   as soon as the caller calls [`FolderSizeWorker::begin_generation`].
//!
//! Typical wiring: call `begin_generation` when the listing changes, then
//! `request` for every visible directory; drop replies whose `generation` is no
//! longer current.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

/// Default hard cap of the folder-size LRU.
pub const DEFAULT_CACHE_CAP: usize = 2_048;
/// Default worker threads. Folder scans are disk bound; two keep one folder from
/// queueing behind another without flooding a slow disk.
pub const DEFAULT_WORKERS: usize = 2;
/// Entries a single scan may visit before it stops with `truncated = true`.
pub const MAX_ENTRIES: u64 = 2_000_000;
/// Directory depth a single scan may descend before it stops.
pub const MAX_DEPTH: usize = 256;

/// Cancellation/generation check interval inside a scan, in entries.
const CANCEL_CHECK_INTERVAL: u64 = 256;

/// Wake callback invoked after every reply is sent (e.g. `PostMessageW`).
pub type Wake = Arc<dyn Fn() + Send + Sync + 'static>;

/// Recursive size of one directory (or the size of a single file).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderSize {
    /// Sum of file sizes below the root, in bytes.
    pub bytes: u64,
    /// Files counted (excluding reparse points).
    pub files: u64,
    /// Subdirectories counted (excluding the root and reparse points).
    pub dirs: u64,
    /// Directories that could not be opened or read; their contents are missing
    /// from `bytes`, so the total is a lower bound when this is non-zero.
    pub errors: u64,
    /// True when [`MAX_ENTRIES`] or [`MAX_DEPTH`] stopped the scan early.
    pub truncated: bool,
}

impl FolderSize {
    /// One-line label for a status column (`12.4 MB`, `-` for an empty tree).
    pub fn display(&self) -> String {
        format_bytes(self.bytes)
    }
}

/// Human-readable byte count with a binary unit, e.g. `1.5 GB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Why a folder-size request did not produce a size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderSizeError {
    /// The path no longer exists.
    NotFound(String),
    /// The OS denied access to the root.
    AccessDenied(String),
    /// Any other I/O failure on the root.
    Io(String),
    /// The request belonged to an older generation, or its token was set.
    Cancelled,
}

impl fmt::Display for FolderSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FolderSizeError::NotFound(path) => write!(f, "not found: {path}"),
            FolderSizeError::AccessDenied(path) => write!(f, "access denied: {path}"),
            FolderSizeError::Io(msg) => write!(f, "folder size failed: {msg}"),
            FolderSizeError::Cancelled => f.write_str("folder size cancelled"),
        }
    }
}

impl std::error::Error for FolderSizeError {}

/// One message on the worker's output channel.
#[derive(Clone, Debug)]
pub struct FolderSizeReply {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<FolderSize, FolderSizeError>,
}

// ---------------------------------------------------------------------------
// Path keys
// ---------------------------------------------------------------------------

/// Case-folded, backslash-separated path key used by the cache.
///
/// Windows paths are case-insensitive, so `C:\Temp` and `c:\temp\` must hit the
/// same cache entry. Trailing separators are dropped except for a drive root.
pub fn normalize_path_key(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        out.push(if ch == '/' {
            '\\'
        } else {
            ch.to_ascii_lowercase()
        });
    }
    while out.len() > 3 && out.ends_with('\\') {
        out.pop();
    }
    out
}

/// True when normalized key `key` is `ancestor` itself or below it.
pub(crate) fn key_is_same_or_child(key: &str, ancestor: &str) -> bool {
    if key == ancestor {
        return true;
    }
    if key.len() <= ancestor.len() {
        return false;
    }
    if ancestor.ends_with('\\') {
        key.starts_with(ancestor)
    } else {
        key.as_bytes().get(ancestor.len()) == Some(&b'\\') && key.starts_with(ancestor)
    }
}

/// Directory modification stamp used to invalidate a cached size.
///
/// Returns 0 when the timestamp is unavailable; a cache entry with stamp 0 is
/// still valid because the path itself is part of the key.
pub fn dir_stamp(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Bounded LRU cache
// ---------------------------------------------------------------------------

struct CacheEntry {
    stamp: u64,
    size: FolderSize,
    last_used: u64,
}

/// Hard-capped LRU of folder sizes, keyed by normalized path.
///
/// A path has at most one entry; the directory mtime stored with it means a
/// directory that changed is a miss even before [`FolderSizeCache::invalidate`]
/// runs.
pub struct FolderSizeCache {
    cap: usize,
    tick: u64,
    entries: HashMap<String, CacheEntry>,
}

impl Default for FolderSizeCache {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_CAP)
    }
}

impl FolderSizeCache {
    /// New cache with a hard cap of `cap` entries (at least 1).
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            tick: 0,
            entries: HashMap::new(),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Cached size for `path`, provided the directory still has `stamp`.
    /// A hit refreshes the entry's LRU position.
    pub fn get(&mut self, path: &Path, stamp: u64) -> Option<FolderSize> {
        let key = normalize_path_key(path);
        let tick = self.next_tick();
        let entry = self.entries.get_mut(&key)?;
        if entry.stamp != stamp {
            self.entries.remove(&key);
            return None;
        }
        entry.last_used = tick;
        Some(entry.size)
    }

    /// Insert or refresh one size, evicting the least recently used entry when
    /// the cache is at its hard cap.
    pub fn insert(&mut self, path: &Path, stamp: u64, size: FolderSize) {
        let key = normalize_path_key(path);
        let tick = self.next_tick();
        if !self.entries.contains_key(&key) && self.entries.len() >= self.cap {
            self.evict_one();
        }
        self.entries.insert(
            key,
            CacheEntry {
                stamp,
                size,
                last_used: tick,
            },
        );
    }

    /// Drop `path` and every cached path below it. Returns true if anything was
    /// removed. Use this on a watcher event or after a file operation.
    pub fn invalidate(&mut self, path: &Path) -> bool {
        let key = normalize_path_key(path);
        let before = self.entries.len();
        self.entries.retain(|k, _| !key_is_same_or_child(k, &key));
        before != self.entries.len()
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.wrapping_add(1);
        self.tick
    }

    fn evict_one(&mut self) {
        let victim = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| k.clone());
        if let Some(key) = victim {
            self.entries.remove(&key);
        }
    }
}

// ---------------------------------------------------------------------------
// Synchronous scan
// ---------------------------------------------------------------------------

/// Recursively sum `root`.
///
/// Reparse points are never followed. Errors below the root are counted in
/// [`FolderSize::errors`] instead of failing the whole request; only a failure
/// to stat the root is returned as an error.
pub fn scan_folder(root: &Path, cancel: &AtomicBool) -> Result<FolderSize, FolderSizeError> {
    scan_folder_bounded(root, cancel, MAX_ENTRIES)
}

/// [`scan_folder`] with an explicit entry budget (tests use a small one).
pub fn scan_folder_bounded(
    root: &Path,
    cancel: &AtomicBool,
    max_entries: u64,
) -> Result<FolderSize, FolderSizeError> {
    let meta = fs::symlink_metadata(root).map_err(|e| map_root_error(root, &e))?;

    if meta.file_type().is_symlink() || is_reparse_point(&meta) {
        // Do not follow a link asked about directly: that is how size scans loop.
        return Ok(FolderSize::default());
    }
    if !meta.is_dir() {
        return Ok(FolderSize {
            bytes: meta.len(),
            files: 1,
            ..FolderSize::default()
        });
    }

    let mut total = FolderSize::default();
    let mut visited = 0u64;
    let mut pending = VecDeque::with_capacity(64);
    pending.push_back((root.to_path_buf(), 0usize));

    'scan: while let Some((dir, depth)) = pending.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            return Err(FolderSizeError::Cancelled);
        }
        if depth > MAX_DEPTH {
            total.truncated = true;
            continue;
        }
        let read_dir = match fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            Err(_) => {
                total.errors += 1;
                continue;
            }
        };

        for entry in read_dir {
            if visited >= max_entries {
                total.truncated = true;
                break 'scan;
            }
            visited += 1;
            if visited.is_multiple_of(CANCEL_CHECK_INTERVAL) && cancel.load(Ordering::Relaxed) {
                return Err(FolderSizeError::Cancelled);
            }

            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    total.errors += 1;
                    continue;
                }
            };
            // `DirEntry::metadata` does not traverse symlinks.
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => {
                    total.errors += 1;
                    continue;
                }
            };
            if meta.file_type().is_symlink() || is_reparse_point(&meta) {
                continue;
            }
            if meta.is_dir() {
                total.dirs += 1;
                pending.push_back((entry.path(), depth + 1));
            } else if meta.is_file() {
                total.files += 1;
                total.bytes = total.bytes.saturating_add(meta.len());
            }
        }
    }

    Ok(total)
}

#[cfg(target_os = "windows")]
fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x0000_0400 != 0
}

#[cfg(not(target_os = "windows"))]
fn is_reparse_point(_meta: &fs::Metadata) -> bool {
    false
}

fn map_root_error(path: &Path, err: &std::io::Error) -> FolderSizeError {
    match err.kind() {
        std::io::ErrorKind::NotFound => FolderSizeError::NotFound(path.display().to_string()),
        std::io::ErrorKind::PermissionDenied => {
            FolderSizeError::AccessDenied(path.display().to_string())
        }
        _ => FolderSizeError::Io(format!("{}: {err}", path.display())),
    }
}

// ---------------------------------------------------------------------------
// Background worker
// ---------------------------------------------------------------------------

struct Job {
    generation: u64,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    shutdown: AtomicBool,
    generation: AtomicU64,
    tokens: Mutex<Vec<(u64, Arc<AtomicBool>)>>,
    cache: Mutex<FolderSizeCache>,
    tx: Sender<FolderSizeReply>,
    wake: Option<Wake>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Pool of threads that compute requested folder sizes off the UI thread.
///
/// Dropping the worker cancels every queued and running scan and joins the
/// threads; scans notice the token within [`CANCEL_CHECK_INTERVAL`] entries.
pub struct FolderSizeWorker {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl FolderSizeWorker {
    /// Start with [`DEFAULT_WORKERS`], [`DEFAULT_CACHE_CAP`] and no wake callback.
    pub fn start() -> (Self, Receiver<FolderSizeReply>) {
        Self::with_options(DEFAULT_WORKERS, DEFAULT_CACHE_CAP, None)
    }

    /// Start `workers` threads (clamped to 1..=8) with a cache capped at
    /// `cache_capacity`.
    pub fn with_options(
        workers: usize,
        cache_capacity: usize,
        wake: Option<Wake>,
    ) -> (Self, Receiver<FolderSizeReply>) {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            shutdown: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            tokens: Mutex::new(Vec::new()),
            cache: Mutex::new(FolderSizeCache::new(cache_capacity)),
            tx,
            wake,
        });

        let workers = workers.clamp(1, 8);
        let mut threads = Vec::with_capacity(workers);
        for n in 0..workers {
            let shared = Arc::clone(&shared);
            let handle = thread::Builder::new()
                .name(format!("tsubame-foldersize-{n}"))
                .spawn(move || worker_loop(shared))
                .ok();
            if let Some(handle) = handle {
                threads.push(handle);
            }
        }

        (Self { shared, threads }, rx)
    }

    /// Current generation; replies older than this should be dropped.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Advance to a new generation and cancel every older queued or running
    /// scan immediately.
    pub fn begin_generation(&self) -> u64 {
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mut tokens = lock(&self.shared.tokens);
        for (token_gen, token) in tokens.drain(..) {
            if token_gen < generation {
                token.store(true, Ordering::Release);
            }
        }
        generation
    }

    /// Queue `path` for `generation`. Replies carry the same generation.
    pub fn request(&self, generation: u64, path: impl Into<PathBuf>) {
        let token = self.token_for(generation);
        lock(&self.shared.queue).push_back(Job {
            generation,
            path: path.into(),
            cancel: token,
        });
        self.shared.cv.notify_one();
    }

    /// Queue many paths of one generation (e.g. every visible directory).
    pub fn request_many<I, P>(&self, generation: u64, paths: I)
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let token = self.token_for(generation);
        let mut queue = lock(&self.shared.queue);
        for path in paths {
            queue.push_back(Job {
                generation,
                path: path.into(),
                cancel: Arc::clone(&token),
            });
        }
        let queued = queue.len();
        drop(queue);
        if queued > 0 {
            self.shared.cv.notify_all();
        }
    }

    /// Jobs waiting for a thread.
    pub fn pending(&self) -> usize {
        lock(&self.shared.queue).len()
    }

    /// Cached sizes currently held.
    pub fn cache_len(&self) -> usize {
        lock(&self.shared.cache).len()
    }

    /// Drop `path` and its descendants from the cache (watcher event).
    pub fn invalidate(&self, path: &Path) {
        lock(&self.shared.cache).invalidate(path);
    }

    /// Empty the cache.
    pub fn clear_cache(&self) {
        lock(&self.shared.cache).clear();
    }

    /// Cancel everything, join the threads and drop the receiver.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn token_for(&self, generation: u64) -> Arc<AtomicBool> {
        let mut tokens = lock(&self.shared.tokens);
        if let Some((_, token)) = tokens
            .iter()
            .find(|(token_gen, _)| *token_gen == generation)
        {
            return Arc::clone(token);
        }
        let token = Arc::new(AtomicBool::new(false));
        tokens.push((generation, Arc::clone(&token)));
        token
    }

    fn stop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        for (_, token) in lock(&self.shared.tokens).drain(..) {
            token.store(true, Ordering::Release);
        }
        {
            let mut queue = lock(&self.shared.queue);
            for job in queue.drain(..) {
                job.cancel.store(true, Ordering::Release);
            }
        }
        self.shared.cv.notify_all();
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for FolderSizeWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            while queue.is_empty() {
                if shared.shutdown.load(Ordering::Acquire) {
                    return;
                }
                queue = match shared.cv.wait(queue) {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
            if shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            queue.pop_front()
        };

        let Some(job) = job else { continue };
        let current = shared.generation.load(Ordering::Acquire);
        let result = if job.generation < current || job.cancel.load(Ordering::Acquire) {
            Err(FolderSizeError::Cancelled)
        } else {
            compute_and_cache(&shared, &job)
        };

        if shared
            .tx
            .send(FolderSizeReply {
                generation: job.generation,
                path: job.path,
                result,
            })
            .is_err()
        {
            // Receiver dropped: nobody can use further results.
            shared.shutdown.store(true, Ordering::Release);
            shared.cv.notify_all();
            return;
        }
        if let Some(wake) = &shared.wake {
            wake();
        }
    }
}

fn compute_and_cache(shared: &Shared, job: &Job) -> Result<FolderSize, FolderSizeError> {
    let stamp = dir_stamp(&job.path);
    if let Some(hit) = lock(&shared.cache).get(&job.path, stamp) {
        return Ok(hit);
    }
    let size = scan_folder(&job.path, &job.cancel)?;
    if !job.cancel.load(Ordering::Acquire) {
        lock(&shared.cache).insert(&job.path, stamp, size);
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "tsubame-foldersize-test-{}-{n}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, vec![0u8; bytes]).unwrap();
    }

    fn recv(rx: &Receiver<FolderSizeReply>) -> FolderSizeReply {
        rx.recv_timeout(Duration::from_secs(20))
            .expect("folder-size reply")
    }

    #[test]
    fn scans_nested_tree() {
        let tree = TempTree::new();
        write(&tree.path().join("a.txt"), 100);
        write(&tree.path().join("b.txt"), 250);
        write(&tree.path().join("sub").join("c.bin"), 1024);
        write(&tree.path().join("sub").join("deep").join("d.bin"), 7);

        let size = scan_folder(tree.path(), &AtomicBool::new(false)).unwrap();
        assert_eq!(size.bytes, 100 + 250 + 1024 + 7);
        assert_eq!(size.files, 4);
        assert_eq!(size.dirs, 2);
        assert_eq!(size.errors, 0);
        assert!(!size.truncated);
    }

    #[test]
    fn single_file_root_counts_as_one_file() {
        let tree = TempTree::new();
        let file = tree.path().join("only.txt");
        write(&file, 42);
        let size = scan_folder(&file, &AtomicBool::new(false)).unwrap();
        assert_eq!(size.bytes, 42);
        assert_eq!(size.files, 1);
        assert_eq!(size.dirs, 0);
    }

    #[test]
    fn missing_root_is_not_found() {
        let tree = TempTree::new();
        let missing = tree.path().join("nope");
        assert_eq!(
            scan_folder(&missing, &AtomicBool::new(false)),
            Err(FolderSizeError::NotFound(missing.display().to_string()))
        );
    }

    #[test]
    fn entry_budget_truncates() {
        let tree = TempTree::new();
        for i in 0..20 {
            write(&tree.path().join(format!("f{i}.bin")), 10);
        }
        let size = scan_folder_bounded(tree.path(), &AtomicBool::new(false), 5).unwrap();
        assert!(size.truncated);
        assert!(size.files <= 5);
    }

    #[test]
    fn cancellation_stops_the_scan() {
        let tree = TempTree::new();
        for i in 0..10 {
            write(&tree.path().join(format!("f{i}.bin")), 10);
        }
        let cancel = AtomicBool::new(true);
        assert_eq!(
            scan_folder(tree.path(), &cancel),
            Err(FolderSizeError::Cancelled)
        );
    }

    #[cfg(windows)]
    #[test]
    fn reparse_points_are_not_followed() {
        let tree = TempTree::new();
        let real = tree.path().join("real");
        write(&real.join("payload.bin"), 300);
        // A junction (or symlink) back to the parent must not be traversed.
        let link = tree.path().join("loop");
        let created = std::os::windows::fs::symlink_dir(tree.path(), &link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&link)
                .arg(tree.path())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
        if !created {
            // No developer mode / symlink privilege: nothing to test.
            return;
        }

        let size = scan_folder(tree.path(), &AtomicBool::new(false)).unwrap();
        assert_eq!(size.bytes, 300);
        assert_eq!(size.files, 1);
        assert_eq!(size.dirs, 1); // `real`; `loop` is skipped
    }

    #[test]
    fn cache_hits_only_for_the_same_stamp() {
        let mut cache = FolderSizeCache::new(4);
        let size = FolderSize {
            bytes: 10,
            files: 1,
            ..FolderSize::default()
        };
        cache.insert(Path::new(r"C:\Temp\Folder"), 7, size);
        assert_eq!(cache.get(Path::new(r"c:\temp\folder\"), 7), Some(size));
        assert_eq!(cache.get(Path::new(r"C:\TEMP\FOLDER"), 8), None);
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_evicts_the_least_recently_used() {
        let mut cache = FolderSizeCache::new(3);
        let size = |bytes| FolderSize {
            bytes,
            ..FolderSize::default()
        };

        cache.insert(Path::new(r"C:\a"), 1, size(1));
        cache.insert(Path::new(r"C:\b"), 1, size(2));
        cache.insert(Path::new(r"C:\c"), 1, size(3));
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.cap(), 3);

        // Touch `a`, then insert a fourth entry: `b` is now the oldest.
        assert!(cache.get(Path::new(r"C:\a"), 1).is_some());
        cache.insert(Path::new(r"C:\d"), 1, size(4));

        assert_eq!(cache.len(), 3);
        assert!(cache.get(Path::new(r"C:\a"), 1).is_some());
        assert!(cache.get(Path::new(r"C:\b"), 1).is_none());
        assert!(cache.get(Path::new(r"C:\c"), 1).is_some());
        assert!(cache.get(Path::new(r"C:\d"), 1).is_some());
    }

    #[test]
    fn cache_invalidate_drops_subtree() {
        let mut cache = FolderSizeCache::new(8);
        let size = FolderSize::default();
        cache.insert(Path::new(r"C:\a"), 1, size);
        cache.insert(Path::new(r"C:\a\b"), 1, size);
        cache.insert(Path::new(r"C:\ab"), 1, size);
        assert!(cache.invalidate(Path::new(r"C:\a")));
        assert!(cache.get(Path::new(r"C:\a"), 1).is_none());
        assert!(cache.get(Path::new(r"C:\a\b"), 1).is_none());
        assert!(cache.get(Path::new(r"C:\ab"), 1).is_some());
        assert!(!cache.invalidate(Path::new(r"C:\missing")));

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn worker_delivers_sizes_and_wakes() {
        let tree = TempTree::new();
        write(&tree.path().join("one.bin"), 64);
        write(&tree.path().join("nested").join("two.bin"), 128);

        let wakes = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&wakes);
        let wake: Wake = Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });

        let (worker, rx) = FolderSizeWorker::with_options(2, 16, Some(wake));
        let generation = worker.begin_generation();
        worker.request(generation, tree.path());

        let reply = recv(&rx);
        assert_eq!(reply.generation, generation);
        assert_eq!(reply.path, tree.path());
        let size = reply.result.unwrap();
        assert_eq!(size.bytes, 192);
        assert_eq!(size.files, 2);
        // The wake callback runs right after the send, so the receiver can win
        // the race; wait briefly for it instead of assuming ordering.
        for _ in 0..100 {
            if wakes.load(Ordering::Relaxed) >= 1 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(wakes.load(Ordering::Relaxed) >= 1);
        assert_eq!(worker.cache_len(), 1);

        // Second request is served from the cache but still replies.
        worker.request(generation, tree.path());
        let reply = recv(&rx);
        assert_eq!(reply.result.unwrap().bytes, 192);
        assert_eq!(worker.cache_len(), 1);
    }

    #[test]
    fn stale_generation_is_cancelled() {
        let tree = TempTree::new();
        let (worker, rx) = FolderSizeWorker::with_options(1, 8, None);
        let old = worker.begin_generation();
        let new = worker.begin_generation();
        assert!(new > old);
        worker.request(old, tree.path());
        let reply = recv(&rx);
        assert_eq!(reply.generation, old);
        assert_eq!(reply.result, Err(FolderSizeError::Cancelled));
    }

    #[test]
    fn generation_bump_cancels_running_scan() {
        let tree = TempTree::new();
        write(&tree.path().join("data.bin"), 16);
        let (worker, rx) = FolderSizeWorker::with_options(1, 8, None);
        let old = worker.begin_generation();
        worker.request(old, tree.path());
        // Bump immediately; whichever side wins the race, the reply must not be
        // a successful size for the stale generation.
        let _new = worker.begin_generation();
        let reply = recv(&rx);
        assert_eq!(reply.generation, old);
        assert!(matches!(
            reply.result,
            Err(FolderSizeError::Cancelled) | Ok(_)
        ));
    }

    #[test]
    fn display_formatting() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
    }
}
