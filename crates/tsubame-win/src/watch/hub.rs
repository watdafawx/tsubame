//! The shared I/O completion port and its one thread.
//!
//! Every watcher's directory handle is overlapped and associated with a single
//! port. One thread blocks in `GetQueuedCompletionStatus`; the timeout is the
//! nearest coalescing deadline, or `INFINITE` when no window is open, so an idle
//! process has no timer and no wake-ups.
//!
//! Memory the kernel writes into (the `OVERLAPPED` and the record buffer) lives
//! in an `Arc<Watch>`. While a read is pending the kernel owns one strong
//! reference (`Arc::into_raw`); the thread takes it back when the completion
//! packet arrives. A closed handle always delivers one last (aborted) packet, so
//! the memory outlives the I/O however the watcher is dropped.

use super::coalesce::{Coalescer, RawKind};
use super::{Gone, Message, Options, Unreliable};
use crate::enumerate::{Win32Error, Wake, extended_wide};
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE, FILE_NOTIFY_CHANGE_ATTRIBUTES,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GETFINALPATHNAMEBYHANDLE_FLAGS, GetDriveTypeW, GetFileAttributesW,
    GetFileInformationByHandle, GetFinalPathNameByHandleW, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
    PostQueuedCompletionStatus,
};
use windows::core::PCWSTR;

const ERROR_NOT_READY: u32 = 21;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_BAD_NETPATH: u32 = 53;
const ERROR_NETNAME_DELETED: u32 = 64;
const ERROR_DIRECTORY: u32 = 267;
const ERROR_INVALID_FUNCTION: u32 = 1;
const ERROR_NOTIFY_CLEANUP: u32 = 745;
const ERROR_OPERATION_ABORTED: u32 = 995;
const ERROR_NOTIFY_ENUM_DIR: u32 = 1022;
const ERROR_UNEXP_NET_ERR: u32 = 59;
const ERROR_DEV_NOT_EXIST: u32 = 55;
const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;
const ERROR_NO_SUCH_DEVICE: u32 = 433;
const ERROR_INVALID_HANDLE: u32 = 6;
const WAIT_TIMEOUT: u32 = 258;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;

const FILTER: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE(
    FILE_NOTIFY_CHANGE_FILE_NAME.0
        | FILE_NOTIFY_CHANGE_DIR_NAME.0
        | FILE_NOTIFY_CHANGE_ATTRIBUTES.0
        | FILE_NOTIFY_CHANGE_SIZE.0
        | FILE_NOTIFY_CHANGE_LAST_WRITE.0,
);

/// Sent by `ReadDirectoryChangesW` over a network handle: buffers above 64 KB fail.
const NETWORK_MAX_BUFFER: usize = 64 * 1024;

#[inline]
fn code_of(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `#[repr(C)]` with the `OVERLAPPED` first: the pointer the kernel hands back
/// is the pointer of the `Watch`.
#[repr(C)]
pub(crate) struct Watch {
    overlapped: UnsafeCell<OVERLAPPED>,
    inner: Mutex<Inner>,
}

// The kernel writes `overlapped` and the record buffer only while a read is
// pending; our code touches them only when none is (start, completion handler).
// `HANDLE` is a plain integer. Everything else is behind the mutex.
unsafe impl Send for Watch {}
unsafe impl Sync for Watch {}

struct Inner {
    handle: Option<HANDLE>,
    /// DWORD-aligned record buffer handed to the kernel.
    buf: Vec<u32>,
    max_buf_bytes: usize,
    /// Copy of the finished read, parsed after the next read is already armed.
    scratch: Vec<u8>,
    recursive: bool,
    co: Coalescer,
    tx: Sender<Message>,
    wake: Wake,
    /// `\\?\`-prefixed, NUL-terminated.
    path: Vec<u16>,
    /// Final path of the handle when it was opened; a change means a rename.
    baseline: Vec<u16>,
    in_dirty: bool,
    /// The `DirWatcher` was dropped: free everything on the next packet.
    closed: bool,
    /// A terminal message was sent and the handle closed.
    done: bool,
    /// The first read is pending: changes from now on are reported.
    armed: bool,
}

impl Inner {
    /// Delivers `msg` and wakes the consumer. A gone receiver closes the watch.
    fn send(&mut self, msg: Message) {
        if self.tx.send(msg).is_err() {
            self.close_handle();
            self.done = true;
            return;
        }
        (self.wake)();
    }

    fn close_handle(&mut self) {
        if let Some(h) = self.handle.take() {
            unsafe {
                let _ = CancelIoEx(h, None);
                let _ = CloseHandle(h);
            }
        }
    }

    /// Terminal: flush what is pending, say why, release the directory.
    fn finish(&mut self, msg: Message) {
        self.emit(Instant::now(), false);
        self.send(msg);
        self.close_handle();
        self.done = true;
    }

    /// Cuts the open window into a `Batch` message. `check_moved` first asks
    /// whether the directory itself was renamed (the handle follows renames).
    fn emit(&mut self, now: Instant, check_moved: bool) {
        if check_moved && !self.done && self.moved() {
            self.co.clear();
            self.finish(Message::Gone(Gone::Renamed));
            return;
        }
        if let Some(batch) = self.co.take_batch(now) {
            self.send(Message::Batch(batch));
        }
    }

    fn moved(&self) -> bool {
        match self.handle.map(final_path) {
            Some(Some(p)) => !self.baseline.is_empty() && p != self.baseline,
            _ => false,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.co.deadline()
    }
}

fn final_path(h: HANDLE) -> Option<Vec<u16>> {
    let mut buf = vec![0u16; 1024];
    loop {
        let n = unsafe { GetFinalPathNameByHandleW(h, &mut buf, GETFINALPATHNAMEBYHANDLE_FLAGS(0)) }
            as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            buf.truncate(n);
            return Some(buf);
        }
        buf.resize(n + 1, 0);
    }
}

fn is_unc(path: &[u16]) -> bool {
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    path.len() >= unc.len() && path[..unc.len()] == unc[..]
}

/// Why change notifications on this path may silently miss changes.
fn reliability(path: &[u16]) -> Option<Unreliable> {
    if is_unc(path) {
        return Some(Unreliable::Network);
    }
    // `\\?\X:\...` -> `X:\`
    if path.len() < 6 || path[5] != b':' as u16 {
        return None;
    }
    let root = [path[4], b':' as u16, b'\\' as u16, 0];
    match unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } {
        DRIVE_REMOTE => Some(Unreliable::Network),
        DRIVE_REMOVABLE => Some(Unreliable::Removable),
        DRIVE_CDROM => Some(Unreliable::CdRom),
        _ => None,
    }
}

// ------------------------------------------------------------------ hub

struct Hub {
    port: usize,
}

fn hub() -> Result<&'static Hub, Win32Error> {
    static HUB: OnceLock<Result<Hub, Win32Error>> = OnceLock::new();
    HUB.get_or_init(|| {
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }
            .map_err(|e| Win32Error::from_win_error(&e))?;
        let raw = port.0 as usize;
        std::thread::Builder::new()
            .name("tsubame-watch".into())
            .spawn(move || run(raw))
            .map_err(|_| Win32Error::win32(8))?; // ERROR_NOT_ENOUGH_MEMORY
        Ok(Hub { port: raw })
    })
    .as_ref()
    .map_err(|e| *e)
}

/// Process-lifetime: blocks in the kernel while idle. Exits only if the port breaks.
fn run(port: usize) {
    let port = HANDLE(port as *mut c_void);
    // Watches with an open window, i.e. the only ones that can need a timeout.
    let mut dirty: Vec<Arc<Watch>> = Vec::new();
    loop {
        let timeout = next_timeout(&dirty);
        let (mut bytes, mut key) = (0u32, 0usize);
        let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
        let r = unsafe { GetQueuedCompletionStatus(port, &mut bytes, &mut key, &mut ov, timeout) };
        let status = match &r {
            Ok(()) => 0,
            Err(e) => code_of(e),
        };
        if !ov.is_null() {
            // SAFETY: only `arm` hands the kernel an OVERLAPPED, and it leaks one
            // strong reference of an `Arc<Watch>` whose first field it is.
            let w = unsafe { Arc::from_raw(ov as *const Watch) };
            complete(&w, bytes, status, &mut dirty);
        } else if r.is_err() && status != WAIT_TIMEOUT {
            return; // the port itself failed: nothing sensible left to do
        }
        flush_due(&mut dirty, Instant::now());
    }
}

fn next_timeout(dirty: &[Arc<Watch>]) -> u32 {
    let now = Instant::now();
    let mut soonest: Option<Instant> = None;
    for w in dirty {
        let g = lock(&w.inner);
        let d = if g.closed || g.done { Some(now) } else { g.deadline() };
        if let Some(d) = d {
            soonest = Some(soonest.map_or(d, |s| s.min(d)));
        }
    }
    match soonest {
        None => u32::MAX, // INFINITE
        Some(d) => {
            let left = d.saturating_duration_since(now);
            // round up: waking a hair late beats waking just before the deadline
            (left.as_micros().div_ceil(1000) as u32).max(1)
        }
    }
}

fn flush_due(dirty: &mut Vec<Arc<Watch>>, now: Instant) {
    dirty.retain(|w| {
        let mut g = lock(&w.inner);
        if g.closed || g.done {
            g.co.clear();
            g.in_dirty = false;
            return false;
        }
        match g.deadline() {
            Some(d) if d <= now => {
                g.emit(now, true);
                g.in_dirty = false;
                false
            }
            Some(_) => true,
            None => {
                g.in_dirty = false;
                false
            }
        }
    });
}

fn complete(w: &Arc<Watch>, bytes: u32, status: u32, dirty: &mut Vec<Arc<Watch>>) {
    let mut g = lock(&w.inner);
    if g.closed || g.done {
        return; // the kernel's reference ends here and the memory is freed
    }
    let now = Instant::now();
    let mut parse_len = 0;
    match status {
        0 if bytes > 0 => {
            // Copy out and re-arm first, parse after: the kernel only buffers
            // events (up to the buffer size) while no read is pending.
            let n = (bytes as usize).min(g.buf.len() * 4);
            let Inner { buf, scratch, .. } = &mut *g;
            scratch.clear();
            // SAFETY: `n` bytes of the u32 buffer were just filled by the kernel.
            scratch.extend_from_slice(unsafe {
                std::slice::from_raw_parts(buf.as_ptr() as *const u8, n)
            });
            parse_len = n;
        }
        0 | ERROR_NOTIFY_ENUM_DIR => {
            // buffer overflow: the kernel dropped records, only a relist is safe
            g.co.clear();
            g.send(Message::Overflow);
            let bigger = (g.buf.len() * 16).min(g.max_buf_bytes); // x4 in bytes
            if bigger > g.buf.len() * 4 {
                g.buf.resize(bigger / 4, 0);
            }
        }
        ERROR_OPERATION_ABORTED | ERROR_NOTIFY_CLEANUP => {} // spurious cancel: just re-arm
        code => {
            let msg = classify(&g, code);
            g.finish(msg);
        }
    }
    if !g.done
        && let Err(code) = arm(w, &mut g)
    {
        let msg = classify(&g, code);
        g.finish(msg);
    }
    if parse_len > 0 && !g.done {
        let Inner { scratch, co, .. } = &mut *g;
        parse(&scratch[..parse_len], co, now);
    }
    if g.co.has_pending() && !g.in_dirty && !g.done {
        g.in_dirty = true;
        dirty.push(w.clone());
    }
}

/// Maps a terminal error to the message the caller acts on.
fn classify(g: &Inner, code: u32) -> Message {
    match code {
        ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => Message::Unreliable(Unreliable::NotSupported),
        ERROR_NETNAME_DELETED | ERROR_BAD_NETPATH | ERROR_UNEXP_NET_ERR | ERROR_NOT_READY
        | ERROR_DEV_NOT_EXIST | ERROR_DEVICE_NOT_CONNECTED | ERROR_NO_SUCH_DEVICE
        | ERROR_INVALID_HANDLE => Message::Gone(Gone::Unavailable),
        _ => {
            // The kernel reports a deleted watched folder as access denied or
            // path-not-found; only the path itself tells it from a real denial.
            match attributes_error(&g.path) {
                None => Message::Failed(Win32Error::win32(code)),
                Some(_) if volume_gone(&g.path) => Message::Gone(Gone::Unavailable),
                Some(_) => Message::Gone(Gone::Deleted),
            }
        }
    }
}

/// `None` when the path can be queried, else the error code.
fn attributes_error(path: &[u16]) -> Option<u32> {
    let r = unsafe { GetFileAttributesW(PCWSTR(path.as_ptr())) };
    if r == u32::MAX {
        Some(
            windows::core::Error::from_win32().code().0 as u32 & 0xFFFF,
        )
    } else {
        None
    }
}

/// The drive/share root is unreachable (stick pulled, share dropped).
fn volume_gone(path: &[u16]) -> bool {
    // `\\?\X:\` or `\\?\UNC\server\share\`
    let s = &path[..path.len().saturating_sub(1)];
    let mut root: Vec<u16> = Vec::new();
    if is_unc(s) {
        let mut slashes = 0;
        for &c in s {
            root.push(c);
            if c == b'\\' as u16 {
                slashes += 1;
                if slashes == 6 {
                    break; // \\?\UNC\server\share\
                }
            }
        }
        if slashes < 6 {
            root.push(b'\\' as u16);
        }
    } else if s.len() >= 6 && s[5] == b':' as u16 {
        root.extend_from_slice(&s[..6]);
        root.push(b'\\' as u16);
    } else {
        return false;
    }
    root.push(0);
    unsafe { GetFileAttributesW(PCWSTR(root.as_ptr())) == u32::MAX }
}

/// Starts (or restarts) the single pending read. On `Err` no read is pending.
fn arm(w: &Arc<Watch>, g: &mut Inner) -> Result<(), u32> {
    let Some(handle) = g.handle else { return Err(ERROR_INVALID_HANDLE) };
    let ov = w.overlapped.get();
    // SAFETY: no read is pending on this watch, so the kernel is not using `ov`.
    unsafe { *ov = OVERLAPPED::default() };
    let kernel_ref = Arc::into_raw(w.clone());
    let r = unsafe {
        ReadDirectoryChangesW(
            handle,
            g.buf.as_mut_ptr() as *mut c_void,
            (g.buf.len() * 4) as u32,
            g.recursive,
            FILTER,
            None,
            Some(ov),
            None,
        )
    };
    if let Err(e) = r {
        // never queued: take the reference back
        drop(unsafe { Arc::from_raw(kernel_ref) });
        return Err(code_of(&e));
    }
    Ok(())
}

fn parse(buf: &[u8], co: &mut Coalescer, now: Instant) {
    let mut off = 0usize;
    let mut name: Vec<u16> = Vec::new();
    let rd = |at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
    while off + 12 <= buf.len() {
        let (next, action, len) = (rd(off) as usize, rd(off + 4), rd(off + 8) as usize);
        let (start, end) = (off + 12, off + 12 + len);
        if end > buf.len() || len % 2 != 0 {
            break; // malformed: keep what was read
        }
        name.clear();
        name.extend(buf[start..end].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)));
        let kind = match action {
            1 => Some(RawKind::Added),
            2 => Some(RawKind::Removed),
            3 => Some(RawKind::Modified),
            4 => Some(RawKind::RenamedOld),
            5 => Some(RawKind::RenamedNew),
            _ => None,
        };
        if let Some(kind) = kind {
            co.push(kind, &name, now);
        }
        if next == 0 {
            break;
        }
        off += next;
    }
}

// ------------------------------------------------------------ public side

/// Opens `dir`, attaches it to the port and arms the first read.
pub(crate) fn open(
    dir: &Path,
    opts: &Options,
    wake: Wake,
) -> Result<(Arc<Watch>, Receiver<Message>), Win32Error> {
    let hub = hub()?;
    let wide = extended_wide(dir)?;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .map_err(|e| Win32Error::from_win_error(&e))?;
    let close_on_err = |e: Win32Error| {
        unsafe {
            let _ = CloseHandle(handle);
        }
        e
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut info) }
        .map_err(|e| close_on_err(Win32Error::from_win_error(&e)))?;
    if info.dwFileAttributes & 0x10 == 0 {
        return Err(close_on_err(Win32Error::win32(ERROR_DIRECTORY)));
    }
    unsafe { CreateIoCompletionPort(handle, Some(HANDLE(hub.port as *mut c_void)), 0, 0) }
        .map_err(|e| close_on_err(Win32Error::from_win_error(&e)))?;

    let reason = reliability(&wide);
    let mut max = opts.max_buffer_bytes.max(opts.buffer_bytes).max(64);
    let mut start = opts.buffer_bytes.max(64);
    if reason == Some(Unreliable::Network) {
        max = max.min(NETWORK_MAX_BUFFER);
        start = start.min(NETWORK_MAX_BUFFER);
    }
    let (tx, rx) = channel();
    let w = Arc::new(Watch {
        overlapped: UnsafeCell::new(OVERLAPPED::default()),
        inner: Mutex::new(Inner {
            handle: Some(handle),
            buf: vec![0u32; start.div_ceil(4)],
            max_buf_bytes: max,
            scratch: Vec::new(),
            recursive: opts.recursive,
            co: Coalescer::new(opts.window, opts.max_delay),
            tx,
            wake,
            baseline: final_path(handle).unwrap_or_default(),
            path: wide,
            in_dirty: false,
            closed: false,
            done: false,
            armed: false,
        }),
    });
    {
        let mut g = lock(&w.inner);
        if let Some(r) = reason {
            g.send(Message::Unreliable(r));
        }
        match arm(&w, &mut g) {
            Ok(()) => g.armed = true,
            Err(code) => {
                let msg = classify(&g, code);
                g.finish(msg);
            }
        }
    }
    Ok((w, rx))
}

/// Like [`open`], but the blocking part (open the folder, query it, arm the
/// first read: disk reads on a cold folder, a network round trip on a share)
/// runs on a short-lived thread, so the caller never waits. Until the read is
/// armed, [`armed`] is false; an open error arrives as `Message::Failed`.
pub(crate) fn spawn(
    dir: &Path,
    opts: &Options,
    wake: Wake,
) -> Result<(Arc<Watch>, Receiver<Message>), Win32Error> {
    let hub = hub()?;
    let wide = extended_wide(dir)?;
    let (tx, rx) = channel();
    let w = Arc::new(Watch {
        overlapped: UnsafeCell::new(OVERLAPPED::default()),
        inner: Mutex::new(Inner {
            handle: None,
            buf: Vec::new(),
            max_buf_bytes: 0,
            scratch: Vec::new(),
            recursive: opts.recursive,
            co: Coalescer::new(opts.window, opts.max_delay),
            tx,
            wake,
            baseline: Vec::new(),
            path: wide,
            in_dirty: false,
            closed: false,
            done: false,
            armed: false,
        }),
    });
    let (buffer_bytes, max_buffer_bytes, port) = (opts.buffer_bytes, opts.max_buffer_bytes, hub.port);
    let w2 = w.clone();
    std::thread::Builder::new()
        .name("tsubame-watch-open".into())
        .spawn(move || open_on_thread(&w2, buffer_bytes, max_buffer_bytes, port))
        .map_err(|_| Win32Error::win32(8))?; // ERROR_NOT_ENOUGH_MEMORY
    Ok((w, rx))
}

fn open_on_thread(w: &Arc<Watch>, buffer_bytes: usize, max_buffer_bytes: usize, port: usize) {
    let wide = lock(&w.inner).path.clone();
    let opened = (|| -> Result<HANDLE, Win32Error> {
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| Win32Error::from_win_error(&e))?;
        let close_on_err = |e: Win32Error| {
            unsafe {
                let _ = CloseHandle(handle);
            }
            e
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle, &mut info) }
            .map_err(|e| close_on_err(Win32Error::from_win_error(&e)))?;
        if info.dwFileAttributes & 0x10 == 0 {
            return Err(close_on_err(Win32Error::win32(ERROR_DIRECTORY)));
        }
        unsafe { CreateIoCompletionPort(handle, Some(HANDLE(port as *mut c_void)), 0, 0) }
            .map_err(|e| close_on_err(Win32Error::from_win_error(&e)))?;
        Ok(handle)
    })();
    let reason = reliability(&wide);
    let baseline = opened.as_ref().ok().and_then(|&h| final_path(h)).unwrap_or_default();
    let mut g = lock(&w.inner);
    let handle = match opened {
        Ok(h) => h,
        Err(e) => {
            if !g.closed {
                g.finish(Message::Failed(e));
            }
            return;
        }
    };
    if g.closed {
        // the DirWatcher was dropped meanwhile: no read was ever armed
        unsafe {
            let _ = CloseHandle(handle);
        }
        return;
    }
    let mut max = max_buffer_bytes.max(buffer_bytes).max(64);
    let mut start = buffer_bytes.max(64);
    if reason == Some(Unreliable::Network) {
        max = max.min(NETWORK_MAX_BUFFER);
        start = start.min(NETWORK_MAX_BUFFER);
    }
    g.handle = Some(handle);
    g.buf = vec![0u32; start.div_ceil(4)];
    g.max_buf_bytes = max;
    g.baseline = baseline;
    if let Some(r) = reason {
        g.send(Message::Unreliable(r));
    }
    match arm(w, &mut g) {
        Ok(()) => g.armed = true,
        Err(code) => {
            let msg = classify(&g, code);
            g.finish(msg);
        }
    }
}

/// The first read of the watch is pending (always true for [`open`]).
pub(crate) fn armed(w: &Watch) -> bool {
    let g = lock(&w.inner);
    g.armed && !g.done && !g.closed
}

/// The watched directory is still where it was (not renamed, deleted or unplugged).
pub(crate) fn still_present(w: &Watch) -> bool {
    let g = lock(&w.inner);
    !g.done && !g.closed && !g.moved() && attributes_error(&g.path).is_none()
}

/// Cancels the pending read and closes the directory handle now.
pub(crate) fn close(w: &Watch) {
    let mut g = lock(&w.inner);
    g.closed = true;
    g.close_handle();
    g.co.clear();
    let in_dirty = g.in_dirty;
    drop(g);
    if in_dirty && let Ok(h) = hub() {
        // let the thread drop its reference without waiting for a deadline
        unsafe {
            let _ = PostQueuedCompletionStatus(HANDLE(h.port as *mut c_void), 0, 1, None);
        }
    }
}

