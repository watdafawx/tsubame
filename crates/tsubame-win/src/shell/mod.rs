//! Shell COM off the UI thread.
//!
//! A [`Pool`] owns N dedicated worker threads. Each worker:
//! - calls `OleInitialize` (an STA, plus OLE for the clipboard verbs of the
//!   context menu: "Copy" and "Cut" call `OleSetClipboard`),
//! - owns one hidden popup window (the context menu owner),
//! - waits in `MsgWaitForMultipleObjectsEx` on the job semaphore **and** its
//!   message queue, so it pumps messages while idle (shell extensions post
//!   messages to their STA and hang if nobody pumps).
//!
//! The UI thread only calls [`Pool::submit`] (a mutex push + `ReleaseSemaphore`,
//! never blocks on a worker) and [`Pool::drain`] (a channel `try_recv`) after
//! the pool posted its wake message (`PostMessageW(hwnd, wake_msg)`, at most
//! one in flight). Every request gets a [`Ticket`]: a unique id plus a cancel
//! flag. A worker skips a job whose flag is set; the UI drops replies whose
//! id it no longer waits for.
//!
//! A worker that spends longer than `hang_after` in one job (not a context
//! menu: that one is modal and waits for the user) is abandoned by
//! [`Pool::check_hung`]: a fresh worker takes its slot, the old thread is
//! leaked and exits on its own if its call ever returns. Its reply is thrown
//! away. See `docs/shell-threading.md`.

mod icon;
mod menu;
mod thumb;

pub use icon::IconSource;
pub use menu::MenuResult;

use crate::bitmap::Bitmap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering::*};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub enum Request {
    /// System icon, `size` px square (the nearest system image list at or
    /// above `size`; the caller scales).
    Icon { src: IconSource, size: u32 },
    /// Thumbnail via `IShellItemImageFactory::GetImage`, `size` px. First
    /// probes the thumbnail cache (`SIIGBF_INCACHEONLY`), then generates.
    /// `cache_only` stops after the probe.
    Thumb { path: String, size: u32, cache_only: bool },
    /// The real shell context menu of `paths` (all in one folder) at screen
    /// point (x, y), shown from the worker. `extended` adds
    /// `CMF_EXTENDEDVERBS` (Shift). `auto_close`: `Some(ms)` ends the menu by
    /// itself `ms` after it is shown (stress tests). `owner` overrides the
    /// menu owner window (wrong-owner experiment: pass a window of another
    /// thread).
    Menu { paths: Vec<String>, x: i32, y: i32, extended: bool, auto_close: Option<u32>, owner: Option<isize> },
    /// Simulates a hung shell call: `Sleep(ms)` without pumping.
    Hang { ms: u32 },
}

impl Request {
    fn kind(&self) -> u8 {
        match self {
            Request::Icon { .. } => K_ICON,
            Request::Thumb { .. } => K_THUMB,
            Request::Menu { .. } => K_MENU,
            Request::Hang { .. } => K_HANG,
        }
    }
}

const K_IDLE: u8 = 0;
const K_ICON: u8 = 1;
const K_THUMB: u8 = 2;
const K_MENU: u8 = 3;
const K_HANG: u8 = 4;

#[derive(Clone)]
pub struct Ticket {
    pub id: u64,
    pub cancel: Arc<AtomicBool>,
}

impl Ticket {
    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }
}

pub enum Payload {
    Icon(Option<Bitmap>),
    /// `probe_us`: the `SIIGBF_INCACHEONLY` attempt alone. `from_cache`: the
    /// probe found it (no generation).
    Thumb { bmp: Option<Bitmap>, from_cache: bool, probe_us: u64, error: Option<String> },
    Menu(MenuResult),
    Hang,
}

pub struct Reply {
    pub id: u64,
    pub worker: usize,
    /// submit to the worker taking it
    pub queued_us: u64,
    /// the shell call(s) themselves
    pub work_us: u64,
    pub payload: Payload,
}

struct Job {
    id: u64,
    cancel: Arc<AtomicBool>,
    req: Request,
    queued: Instant,
}

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    /// count = jobs in `queue` (+ quit tokens)
    sem: SendHandle,
    hwnd: isize,
    wake_msg: u32,
    /// a wake message is in flight (cleared by `drain`)
    posted: AtomicBool,
    quit: AtomicBool,
    /// jobs a worker skipped because the cancel flag was set
    skipped: AtomicU64,
    epoch: Instant,
}

impl Shared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64 + 1
    }
}

/// Per worker, shared with the pool for hang detection.
struct WorkerState {
    /// pool-epoch ms when the current job started, 0 = idle
    busy_since: AtomicU64,
    kind: AtomicU8,
    job: AtomicU64,
    abandoned: AtomicBool,
    tid: AtomicU32,
}

struct Slot {
    state: Arc<WorkerState>,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct PoolStats {
    pub submitted: u64,
    pub replies: u64,
    pub skipped_cancelled: u64,
    pub workers_replaced: u64,
}

pub struct Pool {
    shared: Arc<Shared>,
    rx: Receiver<Reply>,
    tx: Sender<Reply>,
    slots: Vec<Slot>,
    /// abandoned (hung) workers, leaked; kept to report how many still run
    abandoned: Vec<Arc<WorkerState>>,
    next_id: u64,
    pub hang_after: Duration,
    /// stop replacing hung workers after this many (each one is a leaked thread)
    pub max_replaced: usize,
    stats: PoolStats,
}

impl Pool {
    /// `workers` STA threads that post `wake_msg` to `hwnd` when replies are ready.
    pub fn new(workers: usize, hwnd: isize, wake_msg: u32) -> Pool {
        let sem = unsafe { CreateSemaphoreW(None, 0, i32::MAX, None).expect("CreateSemaphoreW") };
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            sem: SendHandle(sem),
            hwnd,
            wake_msg,
            posted: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            skipped: AtomicU64::new(0),
            epoch: Instant::now(),
        });
        let (tx, rx) = channel();
        let mut p = Pool { shared, rx, tx, slots: Vec::new(), abandoned: Vec::new(), next_id: 1, hang_after: Duration::from_secs(3), max_replaced: 8, stats: PoolStats::default() };
        for i in 0..workers.max(1) {
            let s = p.spawn(i);
            p.slots.push(s);
        }
        p
    }

    fn spawn(&self, idx: usize) -> Slot {
        let state = Arc::new(WorkerState { busy_since: AtomicU64::new(0), kind: AtomicU8::new(K_IDLE), job: AtomicU64::new(0), abandoned: AtomicBool::new(false), tid: AtomicU32::new(0) });
        let (sh, st, tx) = (self.shared.clone(), state.clone(), self.tx.clone());
        std::thread::Builder::new().name(format!("shell-sta-{idx}")).stack_size(1 << 20).spawn(move || worker(idx, sh, st, tx)).expect("spawn shell worker");
        Slot { state }
    }

    /// Queues a request. Never blocks on a worker. Context menus and
    /// thumbnails go to the front of the queue, icons to the back.
    pub fn submit(&mut self, req: Request) -> Ticket {
        let id = self.next_id;
        self.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let front = matches!(req, Request::Menu { .. } | Request::Thumb { .. });
        let job = Job { id, cancel: cancel.clone(), req, queued: Instant::now() };
        {
            let mut q = self.shared.queue.lock().unwrap();
            if front { q.push_front(job) } else { q.push_back(job) }
        }
        unsafe {
            let _ = ReleaseSemaphore(self.shared.sem.0, 1, None);
        }
        self.stats.submitted += 1;
        Ticket { id, cancel }
    }

    /// All replies that have arrived. Call on the wake message.
    pub fn drain(&mut self) -> Vec<Reply> {
        // clear first: a reply sent after this posts a new wake
        self.shared.posted.store(false, SeqCst);
        let v: Vec<Reply> = self.rx.try_iter().collect();
        self.stats.replies += v.len() as u64;
        v
    }

    /// Jobs queued or running, context menus excluded (a menu waits for the
    /// user). The UI arms its hang watchdog only while this is true, so an
    /// idle app has no timer.
    pub fn busy(&self) -> bool {
        !self.shared.queue.lock().unwrap().is_empty() || self.slots.iter().any(|s| matches!(s.state.kind.load(Relaxed), K_ICON | K_THUMB | K_HANG))
    }

    /// Replaces workers stuck in one job for longer than `hang_after`.
    /// Returns the ids of the jobs they were running (their replies never come).
    pub fn check_hung(&mut self) -> Vec<u64> {
        let now = self.shared.now_ms();
        let mut lost = Vec::new();
        for i in 0..self.slots.len() {
            let st = &self.slots[i].state;
            let since = st.busy_since.load(Acquire);
            let kind = st.kind.load(Relaxed);
            if since == 0 || kind == K_MENU || now.saturating_sub(since) < self.hang_after.as_millis() as u64 {
                continue;
            }
            if self.stats.workers_replaced as usize >= self.max_replaced {
                break;
            }
            st.abandoned.store(true, SeqCst);
            lost.push(st.job.load(Relaxed));
            let fresh = self.spawn(i);
            let old = std::mem::replace(&mut self.slots[i], fresh);
            self.abandoned.push(old.state);
            self.stats.workers_replaced += 1;
        }
        lost
    }

    pub fn stats(&self) -> PoolStats {
        PoolStats { skipped_cancelled: self.shared.skipped.load(Relaxed), ..self.stats }
    }

    /// Abandoned workers whose hung call has not returned yet.
    pub fn leaked_alive(&self) -> usize {
        self.abandoned.iter().filter(|s| s.busy_since.load(Relaxed) != 0).count()
    }

    pub fn workers(&self) -> usize {
        self.slots.len()
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // workers exit when they next wake; hung ones are left to ExitProcess
        self.shared.quit.store(true, SeqCst);
        unsafe {
            let _ = ReleaseSemaphore(self.shared.sem.0, self.slots.len() as i32, None);
        }
        for s in &self.slots {
            let tid = s.state.tid.load(Relaxed);
            if tid != 0 {
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_NULL, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

fn worker(idx: usize, sh: Arc<Shared>, st: Arc<WorkerState>, tx: Sender<Reply>) {
    unsafe {
        let _ = OleInitialize(None);
        st.tid.store(GetCurrentThreadId(), Relaxed);
        let host = menu::host_window();
        icon::warm_up();
        loop {
            let r = MsgWaitForMultipleObjectsEx(Some(&[sh.sem.0]), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            if r == WAIT_OBJECT_0 {
                if sh.quit.load(SeqCst) {
                    break;
                }
                let Some(job) = sh.queue.lock().unwrap().pop_front() else { continue };
                if job.cancel.load(Relaxed) {
                    sh.skipped.fetch_add(1, Relaxed);
                    continue;
                }
                let queued_us = job.queued.elapsed().as_micros() as u64;
                st.job.store(job.id, Relaxed);
                st.kind.store(job.req.kind(), Relaxed);
                st.busy_since.store(sh.now_ms(), Release);
                let t = Instant::now();
                let payload = run(job.req, host);
                let work_us = t.elapsed().as_micros() as u64;
                st.busy_since.store(0, Release);
                st.kind.store(K_IDLE, Relaxed);
                if st.abandoned.load(SeqCst) {
                    // replaced while hung: the pool forgot this job; just go
                    break;
                }
                if tx.send(Reply { id: job.id, worker: idx, queued_us, work_us, payload }).is_ok() && !sh.posted.swap(true, SeqCst) {
                    let _ = PostMessageW(Some(HWND(sh.hwnd as *mut _)), sh.wake_msg, WPARAM(0), LPARAM(0));
                }
            } else {
                // pump: shell extensions post to their STA; DDE, menu helpers
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if sh.quit.load(SeqCst) {
                    break;
                }
            }
        }
        if let Some(h) = host {
            let _ = DestroyWindow(h);
        }
        OleUninitialize();
    }
}

fn run(req: Request, host: Option<HWND>) -> Payload {
    match req {
        Request::Icon { src, size } => Payload::Icon(icon::get(&src, size)),
        Request::Thumb { path, size, cache_only } => thumb::get(&path, size, cache_only),
        Request::Menu { paths, x, y, extended, auto_close, owner } => Payload::Menu(menu::show(host, &paths, x, y, extended, auto_close, owner)),
        Request::Hang { ms } => {
            unsafe { Sleep(ms) };
            Payload::Hang
        }
    }
}

/// The icon call itself, for tests and benches on an STA thread of their own.
/// The app never calls this: it goes through [`Pool`].
pub use icon::get as icon_on_this_thread;
