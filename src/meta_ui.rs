//! Background metadata UI integration (P4.F5):
//! - Git status column (`tsubame.column.git`, `GitStatusWorker`)
//! - Folder sizes (`tsubame.ext.folder_sizes`, `FolderSizeWorker`, `folder.calc_sizes`)
//! - File checksums (`tsubame.ext.checksum`, `checksum_file`, `file.checksum`)
//!
//! Zero disk I/O on the UI thread, zero idle wake-ups, and zero work when a
//! feature's extension is disabled.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tsubame_ext_api::{Contributions, Extension, ExtensionInfo};
use tsubame_ui::Theme;
use tsubame_win::enumerate::Wake;
use tsubame_win::meta::{
    self, ChecksumError, ChecksumSet, DEFAULT_FOLDER_SIZE_CACHE_CAP, DEFAULT_FOLDER_SIZE_WORKERS,
    FolderSize, FolderSizeReply, FolderSizeWorker, GitReply, GitState, GitStatusWorker,
};

/// Built-in extension ids for P4.F5 features.
pub const EXT_GIT_COL: &str = "tsubame.column.git";
pub const EXT_FOLDER_SIZES: &str = "tsubame.ext.folder_sizes";
pub const EXT_CHECKSUM: &str = "tsubame.ext.checksum";

/// Cap on entries sent in a single git-status batch so huge folders never stall the UI.
const MAX_GIT_BATCH: usize = 2_000;
/// Cap on subdirectories queued by a single `folder.calc_sizes` run.
const MAX_FOLDER_SIZE_BATCH: usize = 512;
/// Minimum interval between checksum progress messages sent to the UI.
const CHECKSUM_PROGRESS_INTERVAL: Duration = Duration::from_millis(60);

pub struct FolderSizesExtension;

impl Extension<crate::ext::App> for FolderSizesExtension {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(EXT_FOLDER_SIZES, "Folder sizes", false)
    }

    fn contribute(&self, c: &mut Contributions<crate::ext::App>) {
        for spec in crate::commands::FOLDER_SIZE_COMMANDS {
            c.command(spec.clone(), crate::view::View::run_builtin);
        }
    }
}

pub struct ChecksumExtension;

impl Extension<crate::ext::App> for ChecksumExtension {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(EXT_CHECKSUM, "File checksums", false)
    }

    fn contribute(&self, c: &mut Contributions<crate::ext::App>) {
        for spec in crate::commands::CHECKSUM_COMMANDS {
            c.command(spec.clone(), crate::view::View::run_builtin);
        }
    }
}

pub fn git_column_enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_GIT_COL).unwrap_or(false))
}

pub fn folder_sizes_enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_FOLDER_SIZES).unwrap_or(false))
}

/// Compact indicator text and theme colour for a `GitState` in the Git column.
/// Returns `None` for `GitState::Clean` (nothing drawn).
pub fn git_badge(state: GitState, t: &Theme) -> Option<(&'static str, u32, bool)> {
    match state {
        GitState::Clean => None,
        GitState::Modified => Some(("M", t.folder, false)),
        GitState::Added => Some(("A", t.accent, false)),
        GitState::Renamed => Some(("R", t.accent, false)),
        GitState::Untracked => Some(("?", t.text_dim, false)),
        GitState::Ignored => Some(("!", t.text_disabled, false)),
        GitState::Deleted => Some(("D", t.danger, false)),
        GitState::Conflict => Some(("U", t.danger, true)),
    }
}

/// Maps lowercase entry names of the current folder to their computed `GitState`.
#[derive(Default, Clone, Debug)]
pub struct GitMap {
    folder: String,
    states: HashMap<String, GitState>,
}

impl GitMap {
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    pub fn clear(&mut self) {
        self.folder.clear();
        self.states.clear();
    }

    pub fn matches_folder(&self, folder: &str) -> bool {
        !self.folder.is_empty() && self.folder.eq_ignore_ascii_case(folder)
    }

    /// Look up `name` in `self` (case-insensitive). Returns `GitState::Clean` when absent.
    pub fn get(&self, name: &str) -> GitState {
        if self.states.is_empty() {
            return GitState::Clean;
        }
        // Fast path: most names are ASCII; avoid allocating when already lowercase.
        if !name.bytes().any(|b| b.is_ascii_uppercase()) {
            return self.states.get(name).copied().unwrap_or(GitState::Clean);
        }
        let lower = name.to_ascii_lowercase();
        self.states.get(&lower).copied().unwrap_or(GitState::Clean)
    }

    fn set_from_reply(&mut self, folder: &str, reply: &GitReply) {
        self.folder = folder.to_string();
        self.states.clear();
        for (p, &st) in reply.paths.iter().zip(&reply.states) {
            if st == GitState::Clean {
                continue;
            }
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                self.states.insert(name.to_ascii_lowercase(), st);
            }
        }
    }
}

/// Maps lowercase folder names of the current directory to their computed `FolderSize`.
#[derive(Default, Clone, Debug)]
pub struct FolderSizeMap {
    folder: String,
    sizes: HashMap<String, FolderSize>,
}

impl FolderSizeMap {
    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    pub fn clear(&mut self) {
        self.folder.clear();
        self.sizes.clear();
    }

    pub fn matches_folder(&self, folder: &str) -> bool {
        !self.folder.is_empty() && self.folder.eq_ignore_ascii_case(folder)
    }

    pub fn get(&self, name: &str) -> Option<FolderSize> {
        if self.sizes.is_empty() {
            return None;
        }
        if !name.bytes().any(|b| b.is_ascii_uppercase()) {
            return self.sizes.get(name).copied();
        }
        let lower = name.to_ascii_lowercase();
        self.sizes.get(&lower).copied()
    }

    fn insert_path(&mut self, folder: &str, path: &Path, size: FolderSize) {
        if !self.folder.eq_ignore_ascii_case(folder) {
            self.folder = folder.to_string();
            self.sizes.clear();
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            self.sizes.insert(name.to_ascii_lowercase(), size);
        }
    }
}

enum ChecksumMsg {
    Progress { done: u64, total: u64 },
    Done { name: String, result: Result<ChecksumSet, ChecksumError> },
}

struct ActiveChecksum {
    name: String,
    cancel: Arc<AtomicBool>,
    rx: Receiver<ChecksumMsg>,
}

/// Completed checksum result returned from `MetaUi::pump`.
#[derive(Clone, Debug)]
pub struct ChecksumDone {
    pub sha256: String,
    pub summary: String,
    #[cfg(test)]
    pub set: Option<ChecksumSet>,
}

/// Holds the lazy background workers and current-folder caches for P4.F5.
#[derive(Default)]
pub struct MetaUi {
    // Git status
    git_worker: Option<(GitStatusWorker, Receiver<GitReply>)>,
    git_gen: u64,
    git_folder: String,
    git_requested_key: Option<(u64, String)>,
    git_unavailable: bool,
    pub git_map: GitMap,

    // Folder sizes
    size_worker: Option<(FolderSizeWorker, Receiver<FolderSizeReply>)>,
    size_gen: u64,
    size_folder: String,
    size_pending: usize,
    pub size_map: FolderSizeMap,

    // Checksums
    checksum: Option<ActiveChecksum>,
    pub last_checksum: Option<ChecksumSet>,

    /// Status-bar progress message while folder sizes or checksums are running.
    pub status: Option<String>,
}

impl Drop for MetaUi {
    fn drop(&mut self) {
        if let Some(c) = self.checksum.take() {
            c.cancel.store(true, Ordering::Release);
        }
    }
}

impl MetaUi {
    // ---- Git status ---------------------------------------------------------

    /// Called when the active folder changes or `tsubame.column.git` is toggled.
    /// Immediately cancels stale requests and clears stale badges.
    pub fn reset_git(&mut self) {
        self.git_requested_key = None;
        self.git_folder.clear();
        self.git_map.clear();
        if let Some((worker, _)) = &self.git_worker {
            self.git_gen = worker.begin_generation();
        }
    }

    /// Drops the git worker completely when `tsubame.column.git` is disabled so
    /// disabling the column has literally zero ongoing cost.
    pub fn disable_git(&mut self) {
        self.git_requested_key = None;
        self.git_folder.clear();
        self.git_map.clear();
        self.git_worker = None;
    }

    /// Invalidates the cached git repository status for `folder` (watcher event)
    /// so the next `sync_git` call re-queries it.
    pub fn invalidate_git(&mut self, folder: &str) {
        if let Some((worker, _)) = &self.git_worker {
            worker.invalidate(Path::new(folder));
        }
        self.git_requested_key = None;
    }

    /// Ensures git status has been requested for `folder` (`nav_key` uniquely
    /// identifies the tab listing). No-op when disabled, unavailable, or already
    /// requested for `(nav_key, folder)`.
    pub fn sync_git(&mut self, nav_key: u64, folder: &str, entries: impl FnOnce(usize) -> Vec<PathBuf>, wake: &Wake) {
        if !git_column_enabled() {
            if self.git_worker.is_some() || !self.git_map.is_empty() {
                self.disable_git();
            }
            return;
        }
        if self.git_unavailable || folder.is_empty() || crate::archive_ui::is_archive_path(folder) {
            return;
        }
        if self.git_requested_key.as_ref().is_some_and(|(k, f)| *k == nav_key && f.eq_ignore_ascii_case(folder)) {
            return;
        }
        let paths = entries(MAX_GIT_BATCH);
        self.git_requested_key = Some((nav_key, folder.to_string()));
        self.git_folder = folder.to_string();
        if !self.git_map.matches_folder(folder) {
            self.git_map.clear();
        }
        if paths.is_empty() {
            return;
        }
        let (worker, _) = self.git_worker.get_or_insert_with(|| GitStatusWorker::start(Some(wake.clone())));
        let generation = worker.begin_generation();
        self.git_gen = generation;
        worker.request(generation, paths);
    }

    // ---- Folder sizes -------------------------------------------------------

    /// Cancels all in-flight folder size scans immediately (called on folder/tab change).
    pub fn cancel_folder_sizes(&mut self) {
        self.size_pending = 0;
        self.size_folder.clear();
        self.size_map.clear();
        if let Some((worker, _)) = &self.size_worker {
            self.size_gen = worker.begin_generation();
        }
        if self.checksum.is_none() {
            self.status = None;
        }
    }

    /// Disables folder sizes completely (zero cost when extension is off).
    pub fn disable_folder_sizes(&mut self) {
        self.cancel_folder_sizes();
        self.size_worker = None;
    }

    /// Invalidates cached folder sizes for `folder` after watcher/file-op changes.
    pub fn invalidate_folder_sizes(&mut self, folder: &str) {
        if let Some((worker, _)) = &self.size_worker {
            worker.invalidate(Path::new(folder));
        }
    }

    /// Queues background recursive size scans for `dirs` in `folder`.
    /// Returns the number of queued directories.
    pub fn request_folder_sizes(&mut self, folder: &str, dirs: Vec<PathBuf>, wake: &Wake) -> usize {
        if !folder_sizes_enabled() {
            self.disable_folder_sizes();
            return 0;
        }
        let dirs: Vec<PathBuf> = dirs.into_iter().take(MAX_FOLDER_SIZE_BATCH).collect();
        let count = dirs.len();
        self.size_folder = folder.to_string();
        self.size_map.clear();
        if count == 0 {
            self.size_pending = 0;
            return 0;
        }
        let (worker, _) = self.size_worker.get_or_insert_with(|| {
            FolderSizeWorker::with_options(DEFAULT_FOLDER_SIZE_WORKERS, DEFAULT_FOLDER_SIZE_CACHE_CAP, Some(wake.clone()))
        });
        let generation = worker.begin_generation();
        self.size_gen = generation;
        self.size_pending = count;
        self.status = Some(format!("Calculating folder sizes (0/{count})\u{2026}"));
        worker.request_many(generation, dirs);
        count
    }

    #[cfg(test)]
    pub fn folder_sizes_pending(&self) -> usize {
        self.size_pending
    }

    // ---- Checksums ----------------------------------------------------------

    /// Starts hashing `path` (CRC32, MD5, SHA-1, SHA-256 in one streaming pass)
    /// on a dedicated background thread. Cancels any earlier checksum job.
    pub fn start_checksum(&mut self, path: PathBuf, wake: Wake) {
        if let Some(prev) = self.checksum.take() {
            prev.cancel.store(true, Ordering::Release);
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        let name_w = name.clone();

        self.status = Some(format!("Computing checksums for {name}\u{2026}"));
        self.checksum = Some(ActiveChecksum { name, cancel, rx });

        let _ = std::thread::Builder::new()
            .name("tsubame-checksum".into())
            .spawn(move || {
                let mut last_tick = Instant::now() - CHECKSUM_PROGRESS_INTERVAL;
                let mut on_progress = |done: u64, total: u64| {
                    if last_tick.elapsed() >= CHECKSUM_PROGRESS_INTERVAL {
                        last_tick = Instant::now();
                        let _ = tx.send(ChecksumMsg::Progress { done, total });
                        wake();
                    }
                };
                let result = meta::checksum_file(&path, &cancel_w, Some(&mut on_progress));
                let _ = tx.send(ChecksumMsg::Done { name: name_w, result });
                wake();
            });
    }

    // ---- Pump ---------------------------------------------------------------

    /// Drains replies from git, folder-size, and checksum workers.
    /// Returns `(changed, checksum_done)`.
    pub fn pump(&mut self) -> (bool, Option<ChecksumDone>) {
        let mut changed = false;

        // 1. Git replies
        if !git_column_enabled() {
            if self.git_worker.is_some() || !self.git_map.is_empty() {
                self.disable_git();
                changed = true;
            }
        } else if let Some((_, rx)) = &self.git_worker {
            loop {
                match rx.try_recv() {
                    Ok(reply) => {
                        if reply.generation != self.git_gen {
                            continue;
                        }
                        if reply.unavailable {
                            self.git_unavailable = true;
                            self.git_map.clear();
                        } else {
                            self.git_map.set_from_reply(&self.git_folder, &reply);
                        }
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.git_worker = None;
                        break;
                    }
                }
            }
        }

        // 2. Folder size replies
        if !folder_sizes_enabled() {
            if self.size_worker.is_some() || !self.size_map.is_empty() {
                self.disable_folder_sizes();
                changed = true;
            }
        } else if let Some((_, rx)) = &self.size_worker {
            loop {
                match rx.try_recv() {
                    Ok(reply) => {
                        if reply.generation != self.size_gen {
                            continue;
                        }
                        self.size_pending = self.size_pending.saturating_sub(1);
                        if let Ok(sz) = reply.result {
                            self.size_map.insert_path(&self.size_folder, &reply.path, sz);
                        }
                        if self.checksum.is_none() {
                            if self.size_pending == 0 {
                                self.status = None;
                            } else {
                                let done = self.size_map.sizes.len();
                                let total = done + self.size_pending;
                                self.status = Some(format!("Calculating folder sizes ({done}/{total})\u{2026}"));
                            }
                        }
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.size_worker = None;
                        self.size_pending = 0;
                        break;
                    }
                }
            }
        }

        // 3. Checksum replies
        let mut checksum_done = None;
        let mut clear_checksum = false;
        if let Some(active) = &self.checksum {
            let name = active.name.clone();
            loop {
                match active.rx.try_recv() {
                    Ok(ChecksumMsg::Progress { done, total }) => {
                        let msg = if total > 0 {
                            let pct = ((done.min(total) as f64 / total as f64) * 100.0).round() as u32;
                            format!("Hashing {name}\u{2026} {pct}%")
                        } else {
                            format!("Hashing {name}\u{2026}")
                        };
                        self.status = Some(msg);
                        changed = true;
                    }
                    Ok(ChecksumMsg::Done { name, result }) => {
                        clear_checksum = true;
                        changed = true;
                        self.status = None;
                        match result {
                            Ok(set) => {
                                let sha256 = set.sha256_hex();
                                let sha1 = set.sha1_hex();
                                let md5 = set.md5_hex();
                                let crc32 = set.crc32_hex();
                                let summary = format!(
                                    "SHA-256 copied ({}\u{2026}) \u{00B7} SHA-1 {}\u{2026} \u{00B7} MD5 {}\u{2026} \u{00B7} CRC32 {crc32}",
                                    &sha256[..12],
                                    &sha1[..8],
                                    &md5[..8],
                                );
                                self.last_checksum = Some(set);
                                checksum_done = Some(ChecksumDone {
                                    sha256,
                                    summary,
                                    #[cfg(test)]
                                    set: Some(set),
                                });
                            }
                            Err(ChecksumError::Cancelled) => {}
                            Err(e) => {
                                checksum_done = Some(ChecksumDone {
                                    sha256: String::new(),
                                    summary: format!("Checksum failed ({name}): {e}"),
                                    #[cfg(test)]
                                    set: None,
                                });
                            }
                        }
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        clear_checksum = true;
                        self.status = None;
                        break;
                    }
                }
            }
        }
        if clear_checksum {
            self.checksum = None;
        }

        (changed, checksum_done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("tsubame-meta-ui-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn git_badges_cover_states_with_distinct_indicators() {
        let t = tsubame_ui::DARK;
        assert_eq!(git_badge(GitState::Clean, &t), None);
        assert_eq!(git_badge(GitState::Modified, &t), Some(("M", t.folder, false)));
        assert_eq!(git_badge(GitState::Added, &t), Some(("A", t.accent, false)));
        assert_eq!(git_badge(GitState::Untracked, &t), Some(("?", t.text_dim, false)));
        assert_eq!(git_badge(GitState::Ignored, &t), Some(("!", t.text_disabled, false)));
        assert_eq!(git_badge(GitState::Conflict, &t), Some(("U", t.danger, true)));
    }

    #[test]
    fn checksum_worker_completes_and_matches_known_vectors() {
        let dir = temp_dir("checksum");
        let file = dir.join("abc.txt");
        std::fs::write(&file, b"abc").unwrap();

        let wake: Wake = Arc::new(|| {});
        let mut meta = MetaUi::default();
        meta.start_checksum(file, wake);

        let t0 = Instant::now();
        let done = loop {
            let (_, d) = meta.pump();
            if let Some(d) = d {
                break d;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "checksum timed out");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(done.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(done.summary.contains("SHA-256 copied"), "{}", done.summary);
        assert!(done.summary.contains("CRC32 352441c2"), "{}", done.summary);
        assert_eq!(done.set.unwrap().md5_hex(), "900150983cd24fb0d6963f7d28e17f72");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_size_worker_fills_map_and_cancels_on_folder_change() {
        let dir = temp_dir("fsize");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a.bin"), vec![0u8; 4096]).unwrap();

        let wake: Wake = Arc::new(|| {});
        let mut meta = MetaUi::default();
        let folder = dir.to_str().unwrap();
        assert_eq!(meta.request_folder_sizes(folder, vec![sub.clone()], &wake), 1);

        let t0 = Instant::now();
        while meta.folder_sizes_pending() > 0 {
            meta.pump();
            assert!(t0.elapsed() < Duration::from_secs(5), "folder size timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(meta.size_map.get("sub").map(|s| s.bytes), Some(4096));
        assert_eq!(meta.size_map.get("SUB").map(|s| s.bytes), Some(4096));

        // Cancelling on folder change clears the map immediately.
        meta.cancel_folder_sizes();
        assert!(meta.size_map.is_empty());
        assert_eq!(meta.folder_sizes_pending(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_git_column_does_zero_work() {
        let dir = temp_dir("git-off");
        let wake: Wake = Arc::new(|| {});
        let mut meta = MetaUi::default();
        crate::ext::write(|r| r.set_enabled(EXT_GIT_COL, false).unwrap());
        let mut called = false;
        meta.sync_git(
            1,
            dir.to_str().unwrap(),
            |_| {
                called = true;
                vec![dir.join("x")]
            },
            &wake,
        );
        assert!(!called, "entries closure must not run when git column is disabled");
        assert!(meta.git_worker.is_none(), "no git worker spawned when disabled");
        crate::ext::write(|r| r.set_enabled(EXT_GIT_COL, true).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
