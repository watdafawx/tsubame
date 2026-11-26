//! Archive VFS UI integration (P4.F3): browsing `.zip`, `.tar`, `.tar.gz`, `.tgz`,
//! and `.7z` archives as read-only virtual folders and extracting them on
//! background threads with status-bar progress.
//!
//! Follows `docs/PLAN.md` §3 rule 1 (no disk I/O on the UI thread) and zero idle
//! wake-ups: the listing worker and index cache (`ArchiveIndexCache`) start lazily
//! on the first archive request, and extraction runs on a short-lived worker thread
//! that notifies the UI via the existing coalesced `Wake` callback.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tsubame_core::archive::{
    self, ARCHIVE_SEP, ArchiveEntry, ArchiveIndexCache, ExtractOptions, ExtractReport,
    extract_all_indexed, extract_selection_indexed,
};
use tsubame_ext_api::{Contributions, Extension, ExtensionInfo, VfsEntry, VfsProvider, text};
use tsubame_win::enumerate::Wake;

/// Built-in extension id for archive VFS browsing & extraction.
pub const EXT_ARCHIVE: &str = "tsubame.vfs.archive";

/// Max bytes `ArchiveVfs::read` will return (64 MB cap).
const VFS_READ_CAP: u64 = 64 << 20;

/// Minimum interval between progress updates sent to the UI thread.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(60);

/// Built-in extension registering the archive VFS provider and extract commands.
pub struct ArchiveExtension;

impl Extension<crate::ext::App> for ArchiveExtension {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(EXT_ARCHIVE, "Archive browser & extractor", false)
    }

    fn contribute(&self, c: &mut Contributions<crate::ext::App>) {
        for spec in crate::commands::ARCHIVE_COMMANDS {
            c.command(spec.clone(), crate::view::View::run_builtin);
        }
        c.vfs(text("archive.vfs"), text("Archive VFS"), Box::new(ArchiveVfs));
    }
}

/// `tsubame_ext_api::VfsProvider` adapter over `tsubame_core::archive`.
struct ArchiveVfs;

impl VfsProvider for ArchiveVfs {
    fn accepts(&self, path: &str) -> bool {
        is_archive_path(path) || archive::is_archive_file(path)
    }

    fn list(&self, path: &str, sink: &mut dyn FnMut(&[VfsEntry]) -> bool) -> Result<(), String> {
        let (host, inner) = split_virtual(path).unwrap_or((path, ""));
        let entries = list_cached_sync(Path::new(host), inner).map_err(|e| e.to_string())?;
        let mapped: Vec<VfsEntry> = entries
            .into_iter()
            .map(|e| VfsEntry {
                name: e.name,
                dir: e.is_dir,
                size: e.size,
                mtime: e.modified.max(0) as u64,
            })
            .collect();
        let _ = sink(&mapped);
        Ok(())
    }

    fn read(&self, path: &str, out: &mut Vec<u8>) -> Result<(), String> {
        let Some((host, inner)) = split_virtual(path) else {
            return Err(format!("not an archive entry path: {path}"));
        };
        let bytes = archive::read_entry_bytes(Path::new(host), inner, VFS_READ_CAP)
            .map_err(|e| e.to_string())?;
        out.extend_from_slice(&bytes);
        Ok(())
    }
}

/// True when the built-in archive extension is enabled in the registry.
pub fn is_enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_ARCHIVE).unwrap_or(false))
}

/// True when `path` is a virtual path inside an archive (`host.zip::` or `host.zip::sub`).
pub fn is_archive_path(path: &str) -> bool {
    archive::is_archive_path(path)
}

/// True when `path` is a supported archive file that can be entered as a virtual folder.
pub fn can_browse(path: &str) -> bool {
    is_enabled() && !is_archive_path(path) && archive::is_archive_file(path)
}

/// Splits an archive virtual path into `(host_archive, inner_dir)`.
pub fn split_virtual(path: &str) -> Option<(&str, &str)> {
    archive::split_archive_path(path)
}

/// Virtual path for the root of `archive_file` (`C:\dir\a.zip` -> `C:\dir\a.zip::`).
/// Keeps `::` so `is_archive_path` is unambiguous even if a real folder shares the name.
pub fn archive_root_path(archive_file: &str) -> String {
    if is_archive_path(archive_file) {
        archive_file.to_string()
    } else {
        format!("{archive_file}{ARCHIVE_SEP}")
    }
}

/// Joins an entry `name` onto an archive virtual directory path using `archive::join_archive_path`.
pub fn join_child(dir: &str, name: &str) -> Option<String> {
    let (host, inner) = split_virtual(dir)?;
    let inner = inner.trim_matches('/');
    let child_inner = if inner.is_empty() {
        name.to_string()
    } else {
        format!("{inner}/{name}")
    };
    Some(archive::join_archive_path(host, &child_inner))
}

/// Navigates one level up from an archive virtual path using `archive::parent_path`.
/// - `C:\dir\a.zip::sub/deep` -> `Some("C:\\dir\\a.zip::sub")`
/// - `C:\dir\a.zip::sub` -> `Some("C:\\dir\\a.zip::")` (archive root)
/// - `C:\dir\a.zip::` -> `Some("C:\\dir")` (exits archive back to host folder)
pub fn parent_of(path: &str) -> Option<String> {
    let (host, inner) = split_virtual(path)?;
    let trimmed = inner.trim_matches(['/', '\\']);
    if trimmed.is_empty() {
        // At archive root: exit to the host folder containing the archive file.
        archive::parent_path(path)
    } else {
        let up = archive::parent_path(path)?;
        if up == host {
            // One level below root -> land on the archive root (`host::`).
            Some(archive_root_path(host))
        } else {
            Some(up)
        }
    }
}

/// Host folder containing the archive file (extraction destination for "Extract here").
pub fn host_folder_of(archive_or_virtual: &str) -> Option<PathBuf> {
    let host = archive::host_archive_path(archive_or_virtual);
    Path::new(host).parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf)
}

// ---- Shared bounded ArchiveIndexCache + lazy listing worker ------------------

fn shared_cache() -> &'static Arc<Mutex<ArchiveIndexCache>> {
    static CACHE: OnceLock<Arc<Mutex<ArchiveIndexCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(Mutex::new(ArchiveIndexCache::new())))
}

fn list_cached_sync(archive_path: &Path, inner: &str) -> Result<Vec<ArchiveEntry>, archive::ArchiveError> {
    let index = {
        let mut cache = shared_cache().lock().unwrap_or_else(|p| p.into_inner());
        cache.get_or_load(archive_path)?
    };
    index.list_dir(inner)
}

/// Result delivered by the background archive listing worker to a `Tab`.
pub struct ArchiveListReply {
    pub generation: u64,
    pub result: Result<Vec<ArchiveEntry>, String>,
}

struct ListJob {
    generation: u64,
    archive: PathBuf,
    inner: String,
    reply: Sender<ArchiveListReply>,
    wake: Wake,
}

fn list_worker_tx() -> &'static Sender<ListJob> {
    static TX: OnceLock<Sender<ListJob>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<ListJob>();
        let _ = std::thread::Builder::new()
            .name("tsubame-archive-list".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let res = list_cached_sync(&job.archive, &job.inner).map_err(|e| e.to_string());
                    let _ = job.reply.send(ArchiveListReply { generation: job.generation, result: res });
                    (job.wake)();
                }
            });
        tx
    })
}

/// Starts listing an archive virtual path on the background worker thread.
/// Never touches the disk on the calling (UI) thread.
pub fn start_list(virtual_path: &str, generation: u64, wake: Wake) -> Receiver<ArchiveListReply> {
    let (tx, rx) = mpsc::channel();
    let Some((host, inner)) = split_virtual(virtual_path) else {
        let _ = tx.send(ArchiveListReply {
            generation,
            result: Err(format!("Invalid archive path: {virtual_path}")),
        });
        wake();
        return rx;
    };
    let job = ListJob {
        generation,
        archive: PathBuf::from(host),
        inner: inner.to_string(),
        reply: tx,
        wake,
    };
    if list_worker_tx().send(job).is_err() {
        // Worker unavailable (only possible during process teardown).
        return rx;
    }
    rx
}

// ---- Read-only message + extract conflicts -------------------------------------

/// Status-bar text for every edit that cannot happen inside a browsed archive
/// (delete, rename, cut, paste, new file/folder, drop-in).
pub const READ_ONLY_MSG: &str = "Archives are read-only. Extract first.";

/// Asks what to do with an existing file while extracting: the choice, and
/// whether it applies to the rest of the extraction. Runs on the extract thread.
pub type ConflictAsker = fn(&Path) -> archive::ConflictAnswer;

static CONFLICT_ASKER: OnceLock<ConflictAsker> = OnceLock::new();

/// The window layer registers its dialog once; without one (tests) extraction
/// replaces existing files as before.
pub fn set_conflict_asker(f: ConflictAsker) {
    let _ = CONFLICT_ASKER.set(f);
}

// ---- Background extraction manager ------------------------------------------

/// Event streamed from a background archive extraction thread to the UI thread.
pub enum ExtractEvent {
    Progress { job_id: u64, done: u64, total: u64 },
    Done { job_id: u64, archive_name: String, dest: PathBuf, result: Result<ExtractReport, String> },
}

/// What the Operations panel hears about extractions (`Ops::on_extract`).
pub enum ExtractNote {
    Started { id: u64, archive: String, dest: String, cancel: Arc<AtomicBool> },
    Progress { id: u64, done: u64, total: u64 },
    /// `files` extracted; `error` the failure, if any
    Done { id: u64, cancelled: bool, error: Option<String>, files: usize },
}

/// Finished extraction notification for `View` so any tab showing `dest` refreshes.
pub struct ExtractFinished {
    pub dest: String,
    pub message: String,
}

/// Tracks an active background extraction (at most one status-bar line at a time,
/// zero timers, drained inside `View::pump`).
#[derive(Default)]
pub struct ArchiveOps {
    next_id: AtomicU64,
    active: Option<ActiveExtract>,
    pub status: Option<String>,
    /// for the Operations panel: drained by `View::pump` into `Ops::on_extract`
    pub notes: Vec<ExtractNote>,
}

struct ActiveExtract {
    job_id: u64,
    archive_name: String,
    cancel: Arc<AtomicBool>,
    rx: Receiver<ExtractEvent>,
}

impl Drop for ArchiveOps {
    fn drop(&mut self) {
        if let Some(a) = self.active.take() {
            a.cancel.store(true, Ordering::Release);
        }
    }
}

impl ArchiveOps {
    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }

    /// Starts extracting the entire `archive` into `dest` on a background thread
    /// using `ArchiveIndexCache` + `extract_all_indexed`.
    pub fn extract_all(&mut self, archive_path: PathBuf, dest: PathBuf, wake: Wake) {
        self.spawn_extract(archive_path, None, dest, wake);
    }

    /// Starts extracting `selection` (canonical inner paths) from `archive` into
    /// `dest` on a background thread using `ArchiveIndexCache` + `extract_selection_indexed`.
    pub fn extract_selected(&mut self, archive_path: PathBuf, selection: Vec<String>, dest: PathBuf, wake: Wake) {
        self.spawn_extract(archive_path, Some(selection), dest, wake);
    }

    fn spawn_extract(&mut self, archive_path: PathBuf, selection: Option<Vec<String>>, dest: PathBuf, wake: Wake) {
        if let Some(prev) = self.active.take() {
            prev.cancel.store(true, Ordering::Release);
            self.notes.push(ExtractNote::Done { id: prev.job_id, cancelled: true, error: None, files: 0 });
        }
        let job_id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let archive_name = archive_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| archive_path.display().to_string());
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        let name_w = archive_name.clone();

        self.status = Some(format!("Extracting {archive_name}\u{2026}"));
        self.notes.push(ExtractNote::Started {
            id: job_id,
            archive: archive_path.to_string_lossy().into_owned(),
            dest: dest.to_string_lossy().into_owned(),
            cancel: Arc::clone(&cancel),
        });
        self.active = Some(ActiveExtract { job_id, archive_name, cancel, rx });

        let _ = std::thread::Builder::new()
            .name("tsubame-archive-extract".into())
            .spawn(move || {
                let res = (|| -> Result<ExtractReport, archive::ArchiveError> {
                    let index = {
                        let mut cache = shared_cache().lock().unwrap_or_else(|p| p.into_inner());
                        cache.get_or_load(&archive_path)?
                    };
                    let opts = ExtractOptions::default();
                    let mut last_tick = Instant::now() - PROGRESS_INTERVAL;
                    let mut on_progress = |done: u64, total: u64| {
                        if last_tick.elapsed() >= PROGRESS_INTERVAL {
                            last_tick = Instant::now();
                            let _ = tx.send(ExtractEvent::Progress { job_id, done, total });
                            wake();
                        }
                    };
                    match CONFLICT_ASKER.get() {
                        Some(asker) => {
                            archive::extract_resolving(&index, selection.as_deref(), &dest, &opts, &cancel_w, &mut on_progress, &mut |p| asker(p))
                        }
                        None => match selection.as_deref() {
                            Some(sel) => extract_selection_indexed(&index, sel, &dest, &opts, &cancel_w, &mut on_progress),
                            None => extract_all_indexed(&index, &dest, &opts, &cancel_w, &mut on_progress),
                        },
                    }
                })();
                let _ = tx.send(ExtractEvent::Done {
                    job_id,
                    archive_name: name_w,
                    dest,
                    result: res.map_err(|e| e.to_string()),
                });
                wake();
            });
    }

    /// Drains pending progress/completion messages. Returns `(changed, finished)`.
    pub fn pump(&mut self) -> (bool, Option<ExtractFinished>) {
        let Some(active) = self.active.as_ref() else {
            return (false, None);
        };
        let expected_id = active.job_id;
        let name = active.archive_name.clone();
        let mut changed = false;
        let mut finished = None;
        let mut drop_active = false;

        loop {
            match active.rx.try_recv() {
                Ok(ExtractEvent::Progress { job_id, done, total }) if job_id == expected_id => {
                    self.notes.push(ExtractNote::Progress { id: job_id, done, total });
                    let msg = if total > 0 {
                        let pct = ((done.min(total) as f64 / total as f64) * 100.0).round() as u32;
                        format!("Extracting {name}\u{2026} {pct}%")
                    } else {
                        format!("Extracting {name}\u{2026}")
                    };
                    self.status = Some(msg);
                    changed = true;
                }
                Ok(ExtractEvent::Done { job_id, archive_name, dest, result }) if job_id == expected_id => {
                    drop_active = true;
                    changed = true;
                    let cancelled = active.cancel.load(Ordering::Acquire);
                    self.notes.push(match &result {
                        Ok(rep) => ExtractNote::Done { id: job_id, cancelled, error: None, files: rep.files },
                        Err(e) => ExtractNote::Done { id: job_id, cancelled, error: Some(e.clone()), files: 0 },
                    });
                    let msg = match result {
                        Ok(rep) => {
                            let mut sz = String::new();
                            crate::data::fmt_size(rep.bytes, &mut sz);
                            let mut s = format!(
                                "Extracted {} {} ({sz}) from {archive_name}",
                                rep.files,
                                if rep.files == 1 { "file" } else { "files" }
                            );
                            if !rep.unsafe_paths.is_empty() {
                                s.push_str(&format!(" ({} unsafe skipped)", rep.unsafe_paths.len()));
                            }
                            s
                        }
                        Err(e) => format!("Extract failed ({archive_name}): {e}"),
                    };
                    self.status = None;
                    finished = Some(ExtractFinished { dest: dest.to_string_lossy().into_owned(), message: msg });
                    break;
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.notes.push(ExtractNote::Done { id: expected_id, cancelled: true, error: None, files: 0 });
                    drop_active = true;
                    self.status = None;
                    changed = true;
                    break;
                }
            }
        }
        if drop_active {
            self.active = None;
        }
        (changed, finished)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("tsubame-archive-ui-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Creates a minimal valid uncompressed (stored) `.zip` archive in pure Rust
    /// so tests need no extra dev-dependency in the root app crate.
    pub(crate) fn write_test_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut out = Vec::new();
        let mut central = Vec::new();

        for (name, data) in entries {
            let offset = out.len() as u32;
            let name_bytes = name.as_bytes();
            let crc = tsubame_win::meta::checksum_bytes(data).crc32;
            let len = data.len() as u32;

            // Local file header
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method: store
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&0u16.to_le_bytes()); // date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(data);

            // Central directory header
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes()); // version made by
            central.extend_from_slice(&20u16.to_le_bytes()); // version needed
            central.extend_from_slice(&0u16.to_le_bytes()); // flags
            central.extend_from_slice(&0u16.to_le_bytes()); // method: store
            central.extend_from_slice(&0u16.to_le_bytes()); // time
            central.extend_from_slice(&0u16.to_le_bytes()); // date
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&len.to_le_bytes());
            central.extend_from_slice(&len.to_le_bytes());
            central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // extra len
            central.extend_from_slice(&0u16.to_le_bytes()); // comment len
            central.extend_from_slice(&0u16.to_le_bytes()); // disk start
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name_bytes);
        }

        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);

        // End of central directory
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len

        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&out).unwrap();
    }

    #[test]
    fn archive_path_navigation_and_up_exit() {
        let root = archive_root_path(r"C:\work\demo.zip");
        assert_eq!(root, r"C:\work\demo.zip::");
        assert!(is_archive_path(&root));

        let sub = join_child(&root, "docs").unwrap();
        assert_eq!(sub, r"C:\work\demo.zip::docs");

        let deep = join_child(&sub, "specs").unwrap();
        assert_eq!(deep, r"C:\work\demo.zip::docs/specs");

        assert_eq!(parent_of(&deep).as_deref(), Some(r"C:\work\demo.zip::docs"));
        assert_eq!(parent_of(&sub).as_deref(), Some(r"C:\work\demo.zip::"));
        assert_eq!(parent_of(&root).as_deref(), Some(r"C:\work"));
    }

    #[test]
    fn background_list_and_extract_work() {
        let dir = temp_dir("list-extract");
        let zip_path = dir.join("sample.zip");
        write_test_zip(
            &zip_path,
            &[("hello.txt", b"hello archive"), ("sub/nested.txt", b"nested content")],
        );

        let wake: Wake = Arc::new(|| {});
        let vroot = archive_root_path(zip_path.to_str().unwrap());
        let rx = start_list(&vroot, 1, wake.clone());
        let reply = rx.recv_timeout(Duration::from_secs(5)).expect("list reply");
        assert_eq!(reply.generation, 1);
        let entries = reply.result.expect("listed ok");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "sub");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "hello.txt");

        let dest = dir.join("out");
        let mut ops = ArchiveOps::default();
        ops.extract_selected(zip_path.clone(), vec!["sub/nested.txt".into()], dest.clone(), wake);
        let t0 = Instant::now();
        let fin = loop {
            let (_, f) = ops.pump();
            if let Some(f) = f {
                break f;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "extract timed out");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(fin.message.contains("Extracted 1 file"), "{}", fin.message);
        assert_eq!(std::fs::read_to_string(dest.join("sub").join("nested.txt")).unwrap(), "nested content");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
