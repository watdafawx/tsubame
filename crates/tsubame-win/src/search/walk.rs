//! Parallel directory walk fallback search engine.
//!
//! Uses `NtQueryDirectoryFile` (`FileIdExtdDirectoryInformation`, class 60) with
//! a single reusable 64 KB buffer per worker thread, falling back per directory
//! to `FindFirstFileExW(FindExInfoBasic, FIND_FIRST_EX_LARGE_FETCH)` when a
//! filesystem rejects the NT information class (e.g. some network shares).
//!
//! Design properties:
//! - Bounded worker pool (4..=8 threads by default) over a shared work queue
//!   (`Mutex<VecDeque<PathBuf>>` + `Condvar`) inside `std::thread::scope`.
//! - Streams hits to the caller callback up to `max_results`, using an atomic
//!   reservation counter so concurrent workers never exceed the cap.
//! - Checks `&AtomicBool` cancellation both between directories and inside
//!   large directory buffers.
//! - Skips recursing into reparse-point directories (`FILE_ATTRIBUTE_REPARSE_POINT`)
//!   so directory symlinks and NTFS junctions can never cause infinite loops.

use core::ffi::c_void;
use core::ptr;
use core::slice;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_MODE, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch,
    FindFirstFileExW, FindNextFileW, GetDriveTypeW, GetFileInformationByHandle, GetLogicalDrives,
    WIN32_FIND_DATAW,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{PCSTR, PCWSTR};

use crate::search::query::CompiledQuery;
use crate::search::{SearchBackend, SearchError, SearchHit, SearchQuery, SearchSummary};

const NT_BUFFER_BYTES: usize = 64 * 1024;
const FILE_ID_EXTD_DIRECTORY_INFORMATION: u32 = 60;
const NT_HEADER_LEN: usize = 88;

const STATUS_SUCCESS: i32 = 0;
const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005u32 as i32;
const STATUS_NO_MORE_FILES: i32 = 0x8000_0006u32 as i32;

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_SHARE_ALL: u32 = 0x0007;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

const ATTR_DIRECTORY: u32 = 0x0000_0010;
const ATTR_REPARSE_POINT: u32 = 0x0000_0400;

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_NO_MORE_FILES: u32 = 18;
const DRIVE_FIXED: u32 = 3;

const MIN_WORKERS: usize = 4;
const MAX_WORKERS: usize = 8;

/// Default bounded worker thread count (clamped to 4..=8).
pub fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(MIN_WORKERS)
        .clamp(MIN_WORKERS, MAX_WORKERS)
}

/// Run a parallel directory walk search, streaming matching [`SearchHit`]s to
/// `on_hit`.
///
/// Returning `false` from `on_hit` cancels the search immediately.
pub fn search<F>(
    query: &SearchQuery,
    worker_count: Option<usize>,
    cancel: &AtomicBool,
    on_hit: F,
) -> Result<SearchSummary, SearchError>
where
    F: FnMut(SearchHit) -> bool + Send,
{
    let started = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return Err(SearchError::Cancelled);
    }
    if query.regex {
        return Err(SearchError::Unsupported(
            "regex matching is not supported by the parallel walk fallback".into(),
        ));
    }

    let compiled = CompiledQuery::new(query);
    if query.max_results == 0 || compiled.excludes_everything() {
        return Ok(SearchSummary {
            backend: SearchBackend::Walk,
            total_hits: 0,
            cancelled: false,
            elapsed: started.elapsed(),
        });
    }

    let roots = resolve_roots(query.root.as_deref())?;
    if roots.is_empty() {
        return Ok(SearchSummary {
            backend: SearchBackend::Walk,
            total_hits: 0,
            cancelled: false,
            elapsed: started.elapsed(),
        });
    }

    let workers = worker_count
        .map(|w| w.clamp(1, MAX_WORKERS))
        .unwrap_or_else(default_worker_count);

    let queue = WorkQueue::new(roots);
    let hit_count = AtomicUsize::new(0);
    let local_stop = AtomicBool::new(false);
    let callback = Mutex::new(on_hit);

    let ctx = WalkContext {
        compiled: &compiled,
        max_results: query.max_results,
        cancel,
        local_stop: &local_stop,
        hit_count: &hit_count,
        queue: &queue,
        callback: &callback,
    };

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| worker_loop(&ctx));
        }
    });

    let total_hits = hit_count.load(Ordering::Acquire);
    let was_cancelled = cancel.load(Ordering::Relaxed) || local_stop.load(Ordering::Relaxed);
    if was_cancelled && total_hits < query.max_results {
        return Err(SearchError::Cancelled);
    }

    Ok(SearchSummary {
        backend: SearchBackend::Walk,
        total_hits,
        cancelled: false,
        elapsed: started.elapsed(),
    })
}

fn resolve_roots(root: Option<&Path>) -> Result<Vec<PathBuf>, SearchError> {
    if let Some(dir) = root {
        let meta = std::fs::metadata(dir)
            .map_err(|e| SearchError::Io(format!("{}: {e}", dir.display())))?;
        if !meta.is_dir() {
            return Err(SearchError::Io(format!(
                "{}: not a directory",
                dir.display()
            )));
        }
        return Ok(vec![dir.to_path_buf()]);
    }

    let mask = unsafe { GetLogicalDrives() };
    let mut roots = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root_str = format!("{letter}:\\");
        let w = crate::wide(&root_str);
        if unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) } == DRIVE_FIXED {
            roots.push(PathBuf::from(root_str));
        }
    }
    Ok(roots)
}

// ---- Shared work queue ------------------------------------------------------

struct QueueState {
    dirs: VecDeque<PathBuf>,
    active: usize,
    done: bool,
}

struct WorkQueue {
    state: Mutex<QueueState>,
    cvar: Condvar,
}

impl WorkQueue {
    fn new(roots: Vec<PathBuf>) -> Self {
        let mut dirs = VecDeque::with_capacity(roots.len().max(64));
        dirs.extend(roots);
        Self {
            state: Mutex::new(QueueState {
                dirs,
                active: 0,
                done: false,
            }),
            cvar: Condvar::new(),
        }
    }

    fn pop(&self, should_stop: impl Fn() -> bool) -> Option<PathBuf> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if guard.done || should_stop() {
                guard.done = true;
                self.cvar.notify_all();
                return None;
            }
            if let Some(dir) = guard.dirs.pop_front() {
                guard.active += 1;
                return Some(dir);
            }
            if guard.active == 0 {
                guard.done = true;
                self.cvar.notify_all();
                return None;
            }
            guard = self.cvar.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn finish_dir(&self, subdirs: &mut Vec<PathBuf>, stop_now: bool) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.active = guard.active.saturating_sub(1);
        if stop_now {
            guard.done = true;
            subdirs.clear();
            self.cvar.notify_all();
            return;
        }
        let pushed = !subdirs.is_empty();
        for d in subdirs.drain(..) {
            guard.dirs.push_back(d);
        }
        if pushed || (guard.dirs.is_empty() && guard.active == 0) {
            if guard.dirs.is_empty() && guard.active == 0 {
                guard.done = true;
            }
            self.cvar.notify_all();
        }
    }
}

struct WalkContext<'a, F> {
    compiled: &'a CompiledQuery,
    max_results: usize,
    cancel: &'a AtomicBool,
    local_stop: &'a AtomicBool,
    hit_count: &'a AtomicUsize,
    queue: &'a WorkQueue,
    callback: &'a Mutex<F>,
}

impl<F: FnMut(SearchHit) -> bool> WalkContext<'_, F> {
    #[inline]
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
            || self.local_stop.load(Ordering::Relaxed)
            || self.hit_count.load(Ordering::Relaxed) >= self.max_results
    }

    /// Atomically claim a hit slot without exceeding `max_results`.
    #[inline]
    fn try_claim_hit(&self) -> bool {
        let mut cur = self.hit_count.load(Ordering::Relaxed);
        while cur < self.max_results {
            match self.hit_count.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(next) => cur = next,
            }
        }
        false
    }

    fn emit_hit(&self, hit: SearchHit) -> bool {
        let mut cb = self.callback.lock().unwrap_or_else(|e| e.into_inner());
        if self.cancel.load(Ordering::Relaxed) || self.local_stop.load(Ordering::Relaxed) {
            return false;
        }
        if !(*cb)(hit) {
            self.local_stop.store(true, Ordering::Release);
            return false;
        }
        true
    }
}

fn worker_loop<F: FnMut(SearchHit) -> bool>(ctx: &WalkContext<'_, F>) {
    // Reusable 8-byte aligned 64 KB buffer per worker thread.
    let mut nt_buf = vec![0u64; NT_BUFFER_BYTES.div_ceil(8)];
    let mut subdirs = Vec::with_capacity(64);
    let mut name_scratch = String::with_capacity(260);
    let mut path_scratch = String::with_capacity(512);

    while let Some(dir) = ctx.queue.pop(|| ctx.stopped()) {
        subdirs.clear();
        scan_directory(
            &dir,
            &mut nt_buf,
            &mut subdirs,
            &mut name_scratch,
            &mut path_scratch,
            ctx,
        );
        ctx.queue.finish_dir(&mut subdirs, ctx.stopped());
    }
}

// ---- Fast directory scanner (NT primary + FindFirstFileExW fallback) --------

#[repr(C)]
#[derive(Default)]
struct IoStatusBlock {
    status: isize,
    information: usize,
}

type NtQueryDirectoryFileFn = unsafe extern "system" fn(
    file_handle: HANDLE,
    event: HANDLE,
    apc_routine: *const c_void,
    apc_context: *mut c_void,
    io_status_block: *mut IoStatusBlock,
    file_information: *mut c_void,
    length: u32,
    file_information_class: u32,
    return_single_entry: u8,
    file_name: *const c_void,
    restart_scan: u8,
) -> i32;

fn nt_query_directory_file() -> Option<NtQueryDirectoryFileFn> {
    static MISSING: AtomicBool = AtomicBool::new(false);
    if MISSING.load(Ordering::Relaxed) {
        return None;
    }
    let name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    unsafe {
        let module = GetModuleHandleW(PCWSTR(name.as_ptr())).ok()?;
        let proc = GetProcAddress(
            module,
            PCSTR(c"NtQueryDirectoryFile".as_ptr().cast::<u8>()),
        );
        match proc {
            Some(f) => Some(core::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                NtQueryDirectoryFileFn,
            >(f)),
            None => {
                MISSING.store(true, Ordering::Relaxed);
                None
            }
        }
    }
}

struct DirHandle(HANDLE);
impl Drop for DirHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct FindHandle(HANDLE);
impl Drop for FindHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = FindClose(self.0);
        }
    }
}

fn scan_directory<F: FnMut(SearchHit) -> bool>(
    dir: &Path,
    nt_buf: &mut [u64],
    subdirs: &mut Vec<PathBuf>,
    name_scratch: &mut String,
    path_scratch: &mut String,
    ctx: &WalkContext<'_, F>,
) {
    let dir_str = dir.to_string_lossy();
    let dir_needs_sep = !dir_str.ends_with('\\') && !dir_str.ends_with('/');

    if let Some(query_fn) = nt_query_directory_file() {
        match scan_dir_nt(
            query_fn,
            dir,
            &dir_str,
            dir_needs_sep,
            nt_buf,
            subdirs,
            name_scratch,
            path_scratch,
            ctx,
        ) {
            NtScanOutcome::Done => return,
            NtScanOutcome::Unsupported => {} // fall through to FindFirstFileExW
        }
    }

    scan_dir_find(dir, &dir_str, dir_needs_sep, subdirs, name_scratch, path_scratch, ctx);
}

enum NtScanOutcome {
    Done,
    Unsupported,
}

#[allow(clippy::too_many_arguments)]
fn scan_dir_nt<F: FnMut(SearchHit) -> bool>(
    query_fn: NtQueryDirectoryFileFn,
    dir: &Path,
    dir_str: &str,
    dir_needs_sep: bool,
    nt_buf: &mut [u64],
    subdirs: &mut Vec<PathBuf>,
    name_scratch: &mut String,
    path_scratch: &mut String,
    ctx: &WalkContext<'_, F>,
) -> NtScanOutcome {
    let Some(wide) = extended_wide(dir) else {
        return NtScanOutcome::Done;
    };
    let handle = match unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_MODE(FILE_SHARE_ALL),
            None,
            FILE_CREATION_DISPOSITION(OPEN_EXISTING),
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS),
            None,
        )
    } {
        Ok(h) => DirHandle(h),
        // Access denied, vanished dir, etc.: skip quietly.
        Err(_) => return NtScanOutcome::Done,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle.0, &mut info) }.is_err()
        || (info.dwFileAttributes & ATTR_DIRECTORY == 0)
    {
        return NtScanOutcome::Done;
    }

    let buf_len = (nt_buf.len() * 8) as u32;
    let buf_ptr = nt_buf.as_mut_ptr().cast::<c_void>();
    let mut restart = 1u8;
    let mut seen_any = false;

    loop {
        if ctx.stopped() {
            return NtScanOutcome::Done;
        }
        let mut iosb = IoStatusBlock::default();
        let status = unsafe {
            query_fn(
                handle.0,
                HANDLE(ptr::null_mut()),
                ptr::null(),
                ptr::null_mut(),
                &mut iosb,
                buf_ptr,
                buf_len,
                FILE_ID_EXTD_DIRECTORY_INFORMATION,
                0,
                ptr::null(),
                restart,
            )
        };
        restart = 0;

        if status == STATUS_NO_MORE_FILES {
            return NtScanOutcome::Done;
        }
        if status != STATUS_SUCCESS && status != STATUS_BUFFER_OVERFLOW {
            if !seen_any && is_nt_unsupported(status) {
                return NtScanOutcome::Unsupported;
            }
            return NtScanOutcome::Done;
        }

        let bytes = iosb.information.min(buf_len as usize);
        if bytes == 0 {
            return NtScanOutcome::Done;
        }
        seen_any = true;

        let buf = unsafe { slice::from_raw_parts(buf_ptr.cast::<u8>(), bytes) };
        let mut off = 0usize;
        let mut entry_count = 0usize;
        loop {
            if off + NT_HEADER_LEN > buf.len() {
                return NtScanOutcome::Done;
            }
            let next = u32_at(buf, off) as usize;
            let name_len = u32_at(buf, off + 60) as usize;
            if name_len & 1 != 0 || off + NT_HEADER_LEN + name_len > buf.len() {
                return NtScanOutcome::Done;
            }
            let name_u16 = unsafe {
                slice::from_raw_parts(
                    buf.as_ptr().add(off + NT_HEADER_LEN).cast::<u16>(),
                    name_len / 2,
                )
            };

            if !is_dot(name_u16) {
                let attrs = u32_at(buf, off + 56);
                let size = u64_at(buf, off + 40);
                let mtime = u64_at(buf, off + 24) as i64;
                if !handle_entry(
                    dir_str,
                    dir_needs_sep,
                    name_u16,
                    attrs,
                    size,
                    mtime,
                    subdirs,
                    name_scratch,
                    path_scratch,
                    ctx,
                ) {
                    return NtScanOutcome::Done;
                }
            }

            entry_count += 1;
            if entry_count & 0x7F == 0 && ctx.stopped() {
                return NtScanOutcome::Done;
            }
            if next == 0 || next < NT_HEADER_LEN {
                break;
            }
            off += next;
        }
    }
}

fn scan_dir_find<F: FnMut(SearchHit) -> bool>(
    dir: &Path,
    dir_str: &str,
    dir_needs_sep: bool,
    subdirs: &mut Vec<PathBuf>,
    name_scratch: &mut String,
    path_scratch: &mut String,
    ctx: &WalkContext<'_, F>,
) {
    let Some(pattern) = search_pattern_wide(dir) else {
        return;
    };
    let mut data = WIN32_FIND_DATAW::default();
    let found = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            ptr::addr_of_mut!(data).cast::<c_void>(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let handle = match found {
        Ok(h) => FindHandle(h),
        Err(e) => {
            let code = (e.code().0 as u32) & 0xFFFF;
            let _ = code == ERROR_FILE_NOT_FOUND || code == ERROR_NO_MORE_FILES;
            return;
        }
    };

    let mut entry_count = 0usize;
    loop {
        if entry_count & 0x7F == 0 && ctx.stopped() {
            return;
        }
        let len = data
            .cFileName
            .iter()
            .position(|&u| u == 0)
            .unwrap_or(data.cFileName.len());
        let name_u16 = &data.cFileName[..len];
        if !is_dot(name_u16) {
            let attrs = data.dwFileAttributes;
            let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
            let mtime = (((data.ftLastWriteTime.dwHighDateTime as u64) << 32)
                | data.ftLastWriteTime.dwLowDateTime as u64) as i64;
            if !handle_entry(
                dir_str,
                dir_needs_sep,
                name_u16,
                attrs,
                size,
                mtime,
                subdirs,
                name_scratch,
                path_scratch,
                ctx,
            ) {
                return;
            }
        }
        entry_count += 1;
        if unsafe { FindNextFileW(handle.0, &mut data) }.is_err() {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_entry<F: FnMut(SearchHit) -> bool>(
    dir_str: &str,
    dir_needs_sep: bool,
    name_u16: &[u16],
    attrs: u32,
    size: u64,
    mtime: i64,
    subdirs: &mut Vec<PathBuf>,
    name_scratch: &mut String,
    path_scratch: &mut String,
    ctx: &WalkContext<'_, F>,
) -> bool {
    let is_dir = attrs & ATTR_DIRECTORY != 0;
    let is_reparse = attrs & ATTR_REPARSE_POINT != 0;

    decode_utf16_into(name_u16, name_scratch);
    path_scratch.clear();
    path_scratch.push_str(dir_str);
    if dir_needs_sep {
        path_scratch.push('\\');
    }
    path_scratch.push_str(name_scratch);

    // Never recurse into symlink / junction reparse-point directories so cycles
    // cannot hang or loop infinitely.
    if is_dir && !is_reparse {
        subdirs.push(PathBuf::from(path_scratch.as_str()));
    }

    if ctx.compiled.matches(name_scratch, path_scratch, is_dir) {
        if !ctx.try_claim_hit() {
            return false;
        }
        let hit = SearchHit {
            path: PathBuf::from(path_scratch.as_str()),
            name: name_scratch.clone(),
            is_dir,
            size: if is_dir { 0 } else { size },
            modified: mtime,
        };
        if !ctx.emit_hit(hit) {
            return false;
        }
    }
    true
}

#[inline]
fn decode_utf16_into(units: &[u16], out: &mut String) {
    out.clear();
    // Fast path: pure ASCII UTF-16 code units (common case).
    if units.iter().all(|&u| u < 0x80) {
        out.reserve(units.len());
        for &u in units {
            out.push(u as u8 as char);
        }
    } else {
        for ch in std::char::decode_utf16(units.iter().copied()) {
            out.push(ch.unwrap_or(std::char::REPLACEMENT_CHARACTER));
        }
    }
}

#[inline]
fn is_dot(name: &[u16]) -> bool {
    matches!(name, [0x2E] | [0x2E, 0x2E])
}

#[inline]
fn is_nt_unsupported(status: i32) -> bool {
    matches!(
        status as u32,
        0xC000_0002 | 0xC000_0003 | 0xC000_0010 | 0xC000_000D
    )
}

#[inline]
fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

#[inline]
fn u64_at(buf: &[u8], off: usize) -> u64 {
    u64::from_ne_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
const VERBATIM_UNC: &[u16] = &[
    b'\\' as u16,
    b'\\' as u16,
    b'?' as u16,
    b'\\' as u16,
    b'U' as u16,
    b'N' as u16,
    b'C' as u16,
    b'\\' as u16,
];
const UNC: &[u16] = &[b'\\' as u16, b'\\' as u16];

fn extended_wide(dir: &Path) -> Option<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let absolute = std::path::absolute(dir).ok()?;
    let mut units: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    while units.len() > 3
        && matches!(units.last(), Some(&c) if c == '\\' as u16 || c == '/' as u16)
    {
        if units.len() == 7 && units.starts_with(VERBATIM) && units[5] == ':' as u16 {
            break;
        }
        units.pop();
    }
    let mut out = if units.starts_with(VERBATIM) {
        units
    } else if units.starts_with(UNC) {
        let mut v = Vec::with_capacity(units.len() + 8);
        v.extend_from_slice(VERBATIM_UNC);
        v.extend_from_slice(&units[2..]);
        v
    } else {
        let mut v = Vec::with_capacity(units.len() + 4);
        v.extend_from_slice(VERBATIM);
        v.append(&mut units);
        v
    };
    out.push(0);
    Some(out)
}

fn search_pattern_wide(dir: &Path) -> Option<Vec<u16>> {
    let mut wide = extended_wide(dir)?;
    wide.pop(); // NUL
    if wide.last() != Some(&('\\' as u16)) {
        wide.push('\\' as u16);
    }
    wide.push('*' as u16);
    wide.push(0);
    Some(wide)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use std::os::windows::process::CommandExt;

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let mut root = std::env::temp_dir();
            root.push(format!(
                "tsubame-search-{}-{}-{tag}-{nanos}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).expect("create temp tree");
            Self { root }
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let p = self.root.join(rel);
            fs::create_dir_all(&p).expect("create dir");
            p
        }

        fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.root.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(&p, bytes).expect("write file");
            p
        }

        fn junction(&self, rel_link: &str, target: &Path) -> bool {
            let link = self.root.join(rel_link);
            if let Some(parent) = link.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if std::os::windows::fs::symlink_dir(target, &link).is_ok() {
                return true;
            }
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(target)
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .status();
            matches!(status, Ok(s) if s.success()) && link.exists()
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn walk_finds_nested_hits_and_respects_filters() {
        let tree = TempTree::new("nested");
        tree.file("alpha/report_2026.txt", b"hello");
        tree.file("alpha/sub/report_final.rs", b"fn main() {}");
        tree.file("beta/notes.txt", b"1234567");
        tree.dir("gamma/report_dir");

        let q = SearchQuery {
            text: "report".into(),
            root: Some(tree.root.clone()),
            max_results: 100,
            ..SearchQuery::default()
        };
        let cancel = AtomicBool::new(false);
        let mut hits = Vec::new();
        let summary = search(&q, Some(4), &cancel, |hit| {
            hits.push(hit);
            true
        })
        .expect("walk succeeds");

        assert_eq!(summary.backend, SearchBackend::Walk);
        assert_eq!(summary.total_hits, 3);
        let names: HashSet<String> = hits.iter().map(|h| h.name.clone()).collect();
        assert!(names.contains("report_2026.txt"));
        assert!(names.contains("report_final.rs"));
        assert!(names.contains("report_dir"));

        let txt = hits.iter().find(|h| h.name == "report_2026.txt").unwrap();
        assert!(!txt.is_dir);
        assert_eq!(txt.size, 5);
        assert!(txt.modified > 0);

        let dir_hit = hits.iter().find(|h| h.name == "report_dir").unwrap();
        assert!(dir_hit.is_dir);
    }

    #[test]
    fn walk_respects_max_results_cap() {
        let tree = TempTree::new("cap");
        for d in 0..8 {
            for f in 0..25 {
                tree.file(&format!("d{d}/item_{f:02}.dat"), b"x");
            }
        }

        let q = SearchQuery {
            text: "*.dat".into(),
            root: Some(tree.root.clone()),
            max_results: 17,
            include_dirs: false,
            include_files: true,
            ..SearchQuery::default()
        };
        let cancel = AtomicBool::new(false);
        let mut hits = Vec::new();
        let summary = search(&q, Some(6), &cancel, |hit| {
            hits.push(hit);
            true
        })
        .expect("capped walk succeeds");

        assert_eq!(hits.len(), 17);
        assert_eq!(summary.total_hits, 17);
    }

    #[test]
    fn walk_stops_promptly_on_cancellation() {
        let tree = TempTree::new("cancel");
        for d in 0..10 {
            for f in 0..50 {
                tree.file(&format!("sub{d}/file_{f:03}.txt"), b"a");
            }
        }

        let q = SearchQuery {
            text: "*.txt".into(),
            root: Some(tree.root.clone()),
            max_results: 10_000,
            ..SearchQuery::default()
        };
        let cancel = AtomicBool::new(false);
        let seen = AtomicUsize::new(0);
        let res = search(&q, Some(4), &cancel, |_| {
            if seen.fetch_add(1, Ordering::Relaxed) >= 5 {
                cancel.store(true, Ordering::Release);
                return false;
            }
            true
        });

        assert!(matches!(res, Err(SearchError::Cancelled)));
        assert!(seen.load(Ordering::Relaxed) < 500);
    }

    #[test]
    fn walk_skips_symlink_and_junction_loops() {
        let tree = TempTree::new("loop");
        let a = tree.dir("a");
        tree.file("a/b/real.txt", b"ok");
        // Create a cycle: a/b/loop_back -> a
        let linked = tree.junction("a/b/loop_back", &a);

        let q = SearchQuery {
            text: "real.txt".into(),
            root: Some(tree.root.clone()),
            max_results: 50,
            ..SearchQuery::default()
        };
        let cancel = AtomicBool::new(false);
        let mut hits = Vec::new();
        let summary = search(&q, Some(4), &cancel, |hit| {
            hits.push(hit);
            true
        })
        .expect("loop-safe walk finishes");

        assert_eq!(summary.total_hits, 1);
        assert_eq!(hits[0].name, "real.txt");
        if !linked {
            eprintln!("note: junction creation unavailable in test environment");
        }
    }
}
