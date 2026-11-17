//! Streaming directory enumerator.
//!
//! The UI thread never touches the disk: [`Enumerator::start`] spawns one worker
//! thread that opens the directory, asks the kernel for entries in 64 KB chunks
//! and pushes [`Batch`]es into a [`Sink`] (usually an `mpsc::Sender`), calling a
//! cheap wake callback after every send so the UI can `PostMessageW` itself.
//! The last message of every request is [`Done`].
//!
//! Method (decided by `docs/spike-enum.md`, measured on rig):
//! `NtQueryDirectoryFile` with `FileIdExtdDirectoryInformation` (class 60) and a
//! 64 KB query buffer, which returns name, size, allocation size, mtime, the full
//! attribute set, the 128-bit file id and the reparse tag in one syscall. When a
//! filesystem rejects the class (some network shares), the request restarts with
//! `FindFirstFileExW(FindExInfoBasic, FIND_FIRST_EX_LARGE_FETCH)`; that fallback
//! has no file id, so `file_id` is 0 and `alloc_size` is 0.
//!
//! Cancellation: every request carries a `generation` and shares a
//! [`CancelToken`], one `AtomicU64`. The worker re-checks it before every syscall,
//! every 256 entries and before every send, so a new generation stops the old
//! worker as soon as the syscall in flight returns; a cancelled request never
//! flushes a pending batch, it sends only `Done { cancelled: true, .. }`.
//! Dropping the [`Enumerator`] cancels and detaches: the worker closes the
//! directory handle itself on every path.
//!
//! Batch policy: the first batch holds at most [`FIRST_BATCH`] entries (so the
//! first rows can paint while the rest streams), later batches hold at most
//! [`BATCH_MAX`] entries or [`BATCH_INTERVAL`] of new entries, whichever comes
//! first. A batch owns one contiguous UTF-16 name buffer plus fixed-size
//! [`RawEntry`] records that index into it, so nothing is allocated per entry.

mod find;
mod nt;
mod simple;

#[cfg(test)]
mod tests;

pub use simple::{Entry, list_dir};

use std::mem;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Entries in the first batch: flushed as soon as this many are parsed.
pub const FIRST_BATCH: usize = 256;
/// Cap on a later batch.
pub const BATCH_MAX: usize = 4096;
/// A later batch is also flushed once this much time has passed.
pub const BATCH_INTERVAL: Duration = Duration::from_millis(4);
/// `NtQueryDirectoryFile` query buffer. 64 KB is the spike's measured optimum:
/// 16 KB is 4x slower cold, 1 MB costs 100 ms of first-batch latency cold.
pub const NT_BUFFER_BYTES: usize = 64 * 1024;

pub const ATTR_HIDDEN: u32 = 0x0000_0002;
pub const ATTR_DIRECTORY: u32 = 0x0000_0010;
pub const ATTR_REPARSE_POINT: u32 = 0x0000_0400;

/// One entry inside a [`Batch`]. 64 bytes; the name lives in the batch's
/// `names` buffer, so a batch is two allocations no matter how many entries.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawEntry {
    /// 128-bit file id (`FILE_ID_128`); 0 when the method has none.
    pub file_id: u128,
    /// EndOfFile in bytes.
    pub size: u64,
    /// AllocationSize in bytes; 0 when the method does not report it.
    pub alloc_size: u64,
    /// Last write time, FILETIME ticks (100 ns since 1601), no conversion.
    pub mtime: u64,
    /// The full Win32 `FILE_ATTRIBUTE_*` set (32 bits, not a `u16`).
    pub attrs: u32,
    /// Offset of the name in `Batch::names`, in UTF-16 code units.
    pub name_off: u32,
    /// Reparse point tag (0 when the entry is not a reparse point).
    pub reparse_tag: u32,
    /// Name length in UTF-16 code units (no terminator is stored).
    pub name_len: u16,
}

const _: () = assert!(mem::size_of::<RawEntry>() == 64);

impl RawEntry {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.attrs & ATTR_DIRECTORY != 0
    }

    #[inline]
    pub fn is_reparse_point(&self) -> bool {
        self.attrs & ATTR_REPARSE_POINT != 0 || self.reparse_tag != 0
    }
}

/// A streamed group of entries, all from one request `generation`.
#[derive(Clone, Debug, Default)]
pub struct Batch {
    pub generation: u64,
    /// One contiguous UTF-16 name buffer for the whole batch.
    pub names: Vec<u16>,
    /// One record per entry; `name_off`/`name_len` index into `names`.
    pub recs: Vec<RawEntry>,
}

impl Batch {
    #[inline]
    pub fn len(&self) -> usize {
        self.recs.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.recs.is_empty()
    }

    /// Name of entry `i`. Out-of-range offsets are clamped rather than panicking;
    /// a well-formed batch always has `name_off + name_len <= names.len()`.
    pub fn name(&self, i: usize) -> &[u16] {
        let Some(rec) = self.recs.get(i) else {
            return &[];
        };
        let start = (rec.name_off as usize).min(self.names.len());
        let end = start
            .saturating_add(rec.name_len as usize)
            .min(self.names.len());
        &self.names[start..end]
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u16], &RawEntry)> + '_ {
        (0..self.len()).map(move |i| (self.name(i), &self.recs[i]))
    }
}

/// How a request ended. Always the last message of a request.
#[derive(Clone, Debug)]
pub struct Done {
    pub generation: u64,
    /// Entries delivered in batches (`Batch::len()` summed). Partial when
    /// `cancelled` is set.
    pub total: usize,
    /// Wall time from worker start to the end of the request.
    pub elapsed: Duration,
    /// The typed failure, if the directory could not be listed at all.
    pub error: Option<Win32Error>,
    /// True when the request was cancelled (new generation,
    /// [`Enumerator::cancel`] or a dropped [`Enumerator`]); `total` is then a
    /// partial count.
    pub cancelled: bool,
    /// True when the listing came from `FindFirstFileExW`: either forced or
    /// because the NT call was rejected. Such a listing has `file_id == 0` and
    /// `alloc_size == 0`.
    pub find_fallback: bool,
}

/// One message of a request stream.
#[derive(Clone, Debug)]
pub enum Message {
    Batch(Batch),
    Done(Done),
}

impl Message {
    pub fn generation(&self) -> u64 {
        match self {
            Message::Batch(b) => b.generation,
            Message::Done(d) => d.generation,
        }
    }

    pub fn into_batch(self) -> Option<Batch> {
        match self {
            Message::Batch(b) => Some(b),
            Message::Done(_) => None,
        }
    }

    pub fn into_done(self) -> Option<Done> {
        match self {
            Message::Batch(_) => None,
            Message::Done(d) => Some(d),
        }
    }
}

/// A Win32 error code, plus the NTSTATUS when the failure came from ntdll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Win32Error {
    /// Win32 error code (`ERROR_ACCESS_DENIED` = 5, `ERROR_DIRECTORY` = 267, ...);
    /// 0 when the NTSTATUS has no Win32 equivalent.
    pub code: u32,
    /// The NTSTATUS from `NtQueryDirectoryFile`, or 0 for a pure Win32 error.
    pub ntstatus: i32,
}

/// Coarse classification, which is what the UI branches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    AccessDenied,
    NotFound,
    NotADirectory,
    /// The filesystem rejected the information class (the Find fallback is used).
    Unsupported,
    Other,
}

impl Win32Error {
    pub const fn win32(code: u32) -> Self {
        Self { code, ntstatus: 0 }
    }

    /// Map an NTSTATUS to the Win32 code the Win32 error table would give it.
    pub const fn from_nt(status: i32) -> Self {
        let code = match status as u32 {
            0xC000_0022 => 5,   // STATUS_ACCESS_DENIED
            0xC000_0033 => 123, // STATUS_OBJECT_NAME_INVALID
            0xC000_0034 => 2,   // STATUS_OBJECT_NAME_NOT_FOUND
            0xC000_003A => 3,   // STATUS_OBJECT_PATH_NOT_FOUND
            0xC000_0103 => 267, // STATUS_NOT_A_DIRECTORY
            0xC000_0003 | 0xC000_0002 | 0xC000_0010 => 1, // invalid/unsupported info class
            0xC000_000D => 87,  // STATUS_INVALID_PARAMETER
            _ => 0,
        };
        Self { code, ntstatus: status }
    }

    /// A malformed query buffer (a bug, not an OS failure).
    pub const fn malformed() -> Self {
        Self { code: 13, ntstatus: 0 } // ERROR_INVALID_DATA
    }

    pub fn kind(self) -> ErrorKind {
        match self.code {
            5 => ErrorKind::AccessDenied,
            2 | 3 => ErrorKind::NotFound,
            267 => ErrorKind::NotADirectory,
            1 if self.ntstatus != 0 => ErrorKind::Unsupported,
            _ => ErrorKind::Other,
        }
    }

    pub fn is_unsupported(self) -> bool {
        // STATUS_INVALID_INFO_CLASS, STATUS_NOT_IMPLEMENTED and friends, and
        // ERROR_INVALID_FUNCTION mapped through from a Win32 call.
        matches!(
            self.ntstatus as u32,
            0xC000_0002 | 0xC000_0003 | 0xC000_0010 | 0xC000_000D
        ) || self.code == 1
    }

    pub fn message(self) -> String {
        let kind = match self.kind() {
            ErrorKind::AccessDenied => "access denied",
            ErrorKind::NotFound => "not found",
            ErrorKind::NotADirectory => "not a directory",
            ErrorKind::Unsupported => "information class not supported",
            ErrorKind::Other => "enumeration failed",
        };
        if self.ntstatus != 0 {
            format!("{kind} (win32 {}, ntstatus {:#010X})", self.code, self.ntstatus as u32)
        } else {
            format!("{kind} (win32 {})", self.code)
        }
    }

    /// Win32 code of a `windows` crate error, which stores it as
    /// `HRESULT_FROM_WIN32` (the low 16 bits are the original code).
    pub(crate) fn from_win_error(e: &windows::core::Error) -> Self {
        Self::win32((e.code().0 as u32) & 0xFFFF)
    }
}

impl core::fmt::Display for Win32Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message())
    }
}

/// Shared request generation. Clone it to cancel from anywhere; bump it to
/// cancel every request started at an older generation.
///
/// One `AtomicU64` total, as PLAN §3 rule 1 asks: the worker's check is a single
/// relaxed load.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    generation: Arc<AtomicU64>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// The generation currently considered live.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Start a new request generation: every older request stops.
    pub fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Cancel every request started so far (the next `next_generation` value is
    /// therefore also fresh).
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// A request may keep running only while its generation is the live one.
    #[inline]
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Relaxed) == generation
    }
}

/// Consumer of a request stream. Called on the worker thread, so it must be
/// cheap and must not block on the UI thread.
pub trait Sink: Send + 'static {
    /// Deliver one message. Return `false` when the consumer is gone; the worker
    /// then stops and closes the directory handle.
    fn send(&mut self, msg: Message) -> bool;
}

impl Sink for Sender<Message> {
    fn send(&mut self, msg: Message) -> bool {
        Sender::send(self, msg).is_ok()
    }
}

/// Called after every message was delivered. Cheap and non-blocking: the app
/// passes `Arc::new(move || unsafe { PostMessageW(hwnd, WAKE, 0, 0) })`.
pub type Wake = Arc<dyn Fn() + Send + Sync + 'static>;

/// Which listing method to use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Method {
    /// NT first, `FindFirstFileExW` when the class is unsupported. The default.
    #[default]
    Auto,
    /// `NtQueryDirectoryFile` only; an unsupported class is an error.
    Nt,
    /// `FindFirstFileExW` only (no file id, no allocation size).
    Find,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub method: Method,
}

/// A running request. Dropping it cancels; use [`Enumerator::join`] to wait for
/// the worker to finish (tests and shutdown only, never the UI thread).
pub struct Enumerator {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

struct Shared {
    token: CancelToken,
    generation: u64,
    /// Explicit stop (`cancel()` or drop), independent of the generation.
    stop: AtomicBool,
    wake: Wake,
}

impl Shared {
    #[inline]
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || !self.token.is_current(self.generation)
    }

    fn wake(&self) {
        (self.wake)();
    }
}

impl Enumerator {
    /// Enumerate `dir` with the default method. `generation` tags every message;
    /// the request stops as soon as `token` moves past it.
    pub fn start<S: Sink>(
        dir: impl AsRef<Path>,
        generation: u64,
        token: CancelToken,
        sink: S,
        wake: Wake,
    ) -> Self {
        Self::start_with(dir, generation, token, sink, wake, Options::default())
    }

    /// [`Enumerator::start`] with an explicit [`Options`] (tests force a method).
    pub fn start_with<S: Sink>(
        dir: impl AsRef<Path>,
        generation: u64,
        token: CancelToken,
        sink: S,
        wake: Wake,
        options: Options,
    ) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let shared = Arc::new(Shared {
            token,
            generation,
            stop: AtomicBool::new(false),
            wake,
        });
        // The sink only passes through a Mutex so a failed thread spawn can still
        // deliver a Done instead of leaving the consumer waiting forever.
        let slot = Arc::new(Mutex::new(Some(sink)));
        let worker_slot = Arc::clone(&slot);
        let worker_shared = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("tsubame-enumerate".to_string())
            .spawn(move || {
                let Some(mut sink) = worker_slot.lock().ok().and_then(|mut g| g.take()) else {
                    return;
                };
                let started = Instant::now();
                let outcome = run(&dir, &worker_shared, &mut sink, options);
                let done = Done {
                    generation: worker_shared.generation,
                    total: outcome.total,
                    elapsed: started.elapsed(),
                    error: outcome.error,
                    cancelled: outcome.cancelled,
                    find_fallback: outcome.find_fallback,
                };
                if sink.send(Message::Done(done)) {
                    worker_shared.wake();
                }
            });
        let worker = match spawned {
            Ok(handle) => Some(handle),
            Err(_) => {
                // ERROR_NOT_ENOUGH_MEMORY: no thread, so report it right here.
                if let Some(mut sink) = slot.lock().ok().and_then(|mut g| g.take()) {
                    let done = Done {
                        generation,
                        total: 0,
                        elapsed: Duration::ZERO,
                        error: Some(Win32Error::win32(8)),
                        cancelled: false,
                        find_fallback: false,
                    };
                    if sink.send(Message::Done(done)) {
                        shared.wake();
                    }
                }
                None
            }
        };
        Self { shared, worker }
    }

    /// Start a request whose messages arrive on a channel (the shape the UI and
    /// the tests use). `wake` runs after every send.
    pub fn start_channel(
        dir: impl AsRef<Path>,
        generation: u64,
        token: CancelToken,
        wake: Wake,
    ) -> (Self, Receiver<Message>) {
        let (tx, rx) = channel();
        (Self::start(dir, generation, token, tx, wake), rx)
    }

    /// The generation this request was started with.
    pub fn generation(&self) -> u64 {
        self.shared.generation
    }

    /// Stop now. The worker closes the directory handle and sends
    /// `Done { cancelled: true, .. }` as soon as the syscall in flight returns.
    pub fn cancel(&self) {
        self.shared.stop.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.shared.cancelled()
    }

    /// Cancel and wait for the worker thread to exit. Not for the UI thread: a
    /// hung network share can hold the worker in one syscall for seconds.
    pub fn join(mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Enumerator {
    fn drop(&mut self) {
        // Cancel and detach (the JoinHandle drops): the worker sees the flag,
        // closes its own handle and exits. Joining here would freeze the UI
        // thread behind a slow network share.
        self.cancel();
    }
}

/// Result of one worker run, before it becomes a [`Done`].
struct Outcome {
    total: usize,
    cancelled: bool,
    find_fallback: bool,
    error: Option<Win32Error>,
}

fn run<S: Sink>(dir: &Path, shared: &Shared, sink: &mut S, options: Options) -> Outcome {
    let mut lister = Lister::new(shared, sink);
    let mut find_fallback = false;
    let error = match options.method {
        Method::Nt => nt::list(&mut lister, dir).err().map(Failure::into_error),
        Method::Find => {
            find_fallback = true;
            find::list(&mut lister, dir).err()
        }
        Method::Auto => match nt::list(&mut lister, dir) {
            Ok(()) => None,
            // The class is rejected before any entry arrives; anything else would
            // mean a partial listing, which must not be silently re-listed.
            Err(Failure::Unsupported) if lister.total > 0 => {
                Some(Win32Error::from_nt(0xC000_0003u32 as i32))
            }
            Err(Failure::Unsupported) => {
                find_fallback = true;
                find::list(&mut lister, dir).err()
            }
            Err(Failure::Error(e)) => Some(e),
        },
    };
    if !lister.stopped && !lister.consumer_gone {
        lister.finish();
    }
    Outcome {
        total: lister.sent,
        cancelled: lister.stopped || lister.consumer_gone,
        find_fallback,
        error,
    }
}

/// A method failed: either the info class is unsupported (fall back) or a real
/// typed error.
pub(crate) enum Failure {
    Unsupported,
    Error(Win32Error),
}

impl Failure {
    fn into_error(self) -> Win32Error {
        match self {
            Failure::Unsupported => Win32Error::from_nt(0xC000_0003u32 as i32),
            Failure::Error(e) => e,
        }
    }
}

/// The metadata of one entry, before it becomes a [`RawEntry`]. Bundled so the
/// per-method parsers do not pass seven arguments around.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EntryData {
    pub attrs: u32,
    pub size: u64,
    pub alloc_size: u64,
    pub mtime: u64,
    pub file_id: u128,
    pub reparse_tag: u32,
}

/// Receives parsed entries, batches them and sends them to the sink.
pub(crate) struct Lister<'a, S: Sink> {
    shared: &'a Shared,
    sink: &'a mut S,
    batch: Batcher,
    /// Entries parsed into batches (including the pending one).
    total: usize,
    /// Entries actually delivered in a sent batch.
    sent: usize,
    /// The request was cancelled (generation moved on, `cancel()` or drop).
    stopped: bool,
    /// The sink refused a message: nobody is listening, so stop.
    consumer_gone: bool,
}

impl<'a, S: Sink> Lister<'a, S> {
    fn new(shared: &'a Shared, sink: &'a mut S) -> Self {
        Self {
            shared,
            sink,
            batch: Batcher::new(shared.generation),
            total: 0,
            sent: 0,
            stopped: false,
            consumer_gone: false,
        }
    }

    /// Check the request generation. Cheap: one relaxed load.
    #[inline]
    pub(crate) fn cancelled(&mut self) -> bool {
        if !self.stopped && self.shared.cancelled() {
            self.stopped = true;
            self.batch.clear();
        }
        self.stopped || self.consumer_gone
    }

    /// Append one entry. Returns `false` when the worker must stop (cancelled or
    /// the consumer is gone).
    pub(crate) fn push(&mut self, name: &[u16], data: EntryData) -> bool {
        if is_dot(name) || name.len() > u16::MAX as usize {
            return true;
        }
        let off = self.batch.names.len() as u32;
        self.batch.names.extend_from_slice(name);
        self.batch.recs.push(RawEntry {
            file_id: data.file_id,
            size: data.size,
            alloc_size: data.alloc_size,
            mtime: data.mtime,
            attrs: data.attrs,
            name_off: off,
            reparse_tag: data.reparse_tag,
            name_len: name.len() as u16,
        });
        self.total += 1;
        // Cancellation is re-checked every 256 entries so a 64 KB buffer full of
        // entries (~1,300) cannot delay a newer request noticeably.
        if self.total & 0xFF == 0 && self.cancelled() {
            return false;
        }
        if self.batch.due() {
            return self.emit();
        }
        true
    }

    /// Send the pending batch (if any). Returns `false` when the worker must stop.
    fn emit(&mut self) -> bool {
        if self.cancelled() {
            return false;
        }
        if self.batch.recs.is_empty() {
            return true;
        }
        let batch = self.batch.take();
        let count = batch.len();
        let delivered = self.sink.send(Message::Batch(batch));
        if delivered {
            self.sent += count;
            self.shared.wake();
        } else {
            self.consumer_gone = true;
        }
        delivered
    }

    /// Flush the tail batch.
    fn finish(&mut self) {
        let _ = self.emit();
    }
}

/// The pending batch: one name buffer + one record vector, sent as-is.
struct Batcher {
    generation: u64,
    names: Vec<u16>,
    recs: Vec<RawEntry>,
    sent_first: bool,
    batch_start: Instant,
    /// Entries since the last clock check (the clock is read every 256 entries).
    since_clock: u32,
}

impl Batcher {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            names: Vec::with_capacity(FIRST_BATCH * 24),
            recs: Vec::with_capacity(FIRST_BATCH),
            sent_first: false,
            batch_start: Instant::now(),
            since_clock: 0,
        }
    }

    /// Is the pending batch due?
    #[inline]
    fn due(&mut self) -> bool {
        let n = self.recs.len();
        if n == 0 {
            return false;
        }
        if !self.sent_first {
            return n >= FIRST_BATCH;
        }
        if n >= BATCH_MAX {
            return true;
        }
        self.since_clock += 1;
        if self.since_clock < 256 {
            return false;
        }
        self.since_clock = 0;
        self.batch_start.elapsed() >= BATCH_INTERVAL
    }

    /// Hand the pending batch over and start a new one sized like the old.
    fn take(&mut self) -> Batch {
        let names = mem::take(&mut self.names);
        let recs = mem::take(&mut self.recs);
        let names_cap = (names.len() * 2).clamp(1024, BATCH_MAX * 24);
        let recs_cap = (recs.len() * 2).clamp(64, BATCH_MAX);
        self.names = Vec::with_capacity(names_cap);
        self.recs = Vec::with_capacity(recs_cap);
        self.sent_first = true;
        self.batch_start = Instant::now();
        self.since_clock = 0;
        Batch {
            generation: self.generation,
            names,
            recs,
        }
    }

    fn clear(&mut self) {
        self.names.clear();
        self.recs.clear();
    }
}

#[inline]
fn is_dot(name: &[u16]) -> bool {
    matches!(name, [0x2E] | [0x2E, 0x2E])
}

/// `\\?\` (verbatim) and `\\?\UNC\` prefixes, as UTF-16 units.
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

/// `\\?\`-prefixed, NUL-terminated UTF-16 path, so `MAX_PATH` is not a limit.
/// A UNC path becomes `\\?\UNC\server\share\...`.
pub(crate) fn extended_wide(dir: &Path) -> Result<Vec<u16>, Win32Error> {
    use std::os::windows::ffi::OsStrExt;
    let absolute = std::path::absolute(dir).map_err(|e| {
        Win32Error::win32(e.raw_os_error().unwrap_or(123) as u32) // ERROR_INVALID_NAME
    })?;
    let mut units: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    trim_trailing_separators(&mut units);
    let mut out;
    if starts_with(&units, VERBATIM) {
        out = units;
    } else if starts_with(&units, UNC) {
        out = Vec::with_capacity(units.len() + 8);
        out.extend_from_slice(VERBATIM_UNC);
        out.extend_from_slice(&units[2..]);
    } else {
        out = Vec::with_capacity(units.len() + 4);
        out.extend_from_slice(VERBATIM);
        out.append(&mut units);
    }
    out.push(0);
    Ok(out)
}

/// `dir\*` pattern for `FindFirstFileExW`, also `\\?\`-prefixed.
pub(crate) fn search_pattern_wide(dir: &Path) -> Result<Vec<u16>, Win32Error> {
    let mut wide = extended_wide(dir)?;
    wide.pop(); // NUL
    if wide.last() != Some(&('\\' as u16)) {
        wide.push('\\' as u16);
    }
    wide.push('*' as u16);
    wide.push(0);
    Ok(wide)
}

/// ASCII prefix comparison against a UTF-16 buffer.
fn starts_with(units: &[u16], prefix: &[u16]) -> bool {
    units.len() >= prefix.len() && prefix.iter().zip(units).all(|(&c, &u)| u == c)
}

fn trim_trailing_separators(units: &mut Vec<u16>) {
    // Keep `C:\` and `\\?\C:\` (a separator right after a drive colon).
    while units.len() > 3
        && matches!(units.last(), Some(&c) if c == '\\' as u16 || c == '/' as u16)
        && !(units.len() >= 2 && units[units.len() - 2] == ':' as u16)
    {
        units.pop();
    }
}
