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

pub mod ctxmenu;
pub mod ctxmodel;
mod icon;
mod menu;
pub mod packaged;
mod thumb;

pub use icon::IconSource;
pub use ctxmenu::{CtxCmd, CtxHandle, CtxOpts};
pub use menu::{MenuOpts, MenuResult};

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
    /// `opts`: background menu of a folder, extra entries on top, verbs the
    /// app handles itself (see [`MenuOpts`]).
    Menu { paths: Vec<String>, x: i32, y: i32, extended: bool, auto_close: Option<u32>, owner: Option<isize>, opts: MenuOpts },
    /// Clipboard work and shortcut creation (`crate::dnd::clipboard`).
    Clip(crate::dnd::clipboard::ClipJob),
    /// The shell's context menu as data for the own-drawn menu (CM1): batches
    /// of [`ctxmodel::MenuBatch`] arrive as `Payload::Ctx`, the last one
    /// `Closed`. The worker keeps the menu until `handle` picks or closes.
    /// Build it with [`Request::ctx`].
    CtxMenu { paths: Vec<String>, generation: u64, opts: CtxOpts, session: Arc<ctxmenu::CtxSession> },
    /// Simulates a hung shell call: `Sleep(ms)` without pumping.
    Hang { ms: u32 },
}

impl Request {
    /// An own-drawn menu request driven by `handle` (see [`CtxHandle`]).
    pub fn ctx(paths: Vec<String>, generation: u64, opts: CtxOpts, handle: &CtxHandle) -> Request {
        Request::CtxMenu { paths, generation, opts, session: handle.session.clone() }
    }

    fn kind(&self) -> u8 {
        match self {
            Request::Icon { .. } => K_ICON,
            Request::Thumb { .. } => K_THUMB,
            Request::Menu { .. } => K_MENU,
            Request::Hang { .. } => K_HANG,
            Request::Clip(_) => K_CLIP,
            Request::CtxMenu { .. } => K_CTX,
        }
    }
}

const K_IDLE: u8 = 0;
const K_ICON: u8 = 1;
const K_THUMB: u8 = 2;
const K_MENU: u8 = 3;
const K_HANG: u8 = 4;
/// clipboard / shortcut jobs: not replaced as hung (a slow source app or a
/// big virtual file is not a hang), like menus
const K_CLIP: u8 = 5;
/// building an own-drawn menu (QueryContextMenu + walk): replaced as hung
/// after `Pool::ctx_hang_after`. Once built it waits for the user as K_MENU.
const K_CTX: u8 = 6;

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
    Clip(crate::dnd::clipboard::ClipReply),
    /// one batch of an own-drawn menu (several per request)
    Ctx(ctxmodel::MenuBatch),
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
pub(crate) struct SendHandle(pub(crate) HANDLE);
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

/// Worker lanes. The icon lane takes icons, menus and the hang test (and
/// thumbnails too while no thumbnail lane is configured); the thumbnail lane
/// takes thumbnails only, so a screen of slow decodes never delays icons or a
/// context menu. Each lane has its own queue and semaphore (a worker only
/// ever waits for jobs it can take).
const LANE_ICON: u8 = 0;
const LANE_THUMB: u8 = 1;

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    /// count = jobs in `queue` (+ quit tokens)
    sem: SendHandle,
    /// the thumbnail lane's queue and semaphore
    tqueue: Mutex<VecDeque<Job>>,
    tsem: SendHandle,
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
    lane: u8,
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
    /// thumbnail decodes legitimately take longer than icons: a thumbnail
    /// worker is replaced after `hang_after * thumb_hang_factor`
    pub thumb_hang_factor: u32,
    /// an own-drawn menu still building after this is given up: its worker
    /// is replaced and the UI shows "Some items could not load" + Retry
    pub ctx_hang_after: Duration,
    /// size of the thumbnail lane; its threads start with the first thumbnail
    /// request (a details-only session never pays for them)
    thumb_workers: usize,
    thumb_started: bool,
    stats: PoolStats,
}

impl Pool {
    /// `workers` STA threads that post `wake_msg` to `hwnd` when replies are ready.
    pub fn new(workers: usize, hwnd: isize, wake_msg: u32) -> Pool {
        let sem = unsafe { CreateSemaphoreW(None, 0, i32::MAX, None).expect("CreateSemaphoreW") };
        let tsem = unsafe { CreateSemaphoreW(None, 0, i32::MAX, None).expect("CreateSemaphoreW") };
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            sem: SendHandle(sem),
            tqueue: Mutex::new(VecDeque::new()),
            tsem: SendHandle(tsem),
            hwnd,
            wake_msg,
            posted: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            skipped: AtomicU64::new(0),
            epoch: Instant::now(),
        });
        let (tx, rx) = channel();
        let mut p = Pool {
            shared,
            rx,
            tx,
            slots: Vec::new(),
            abandoned: Vec::new(),
            next_id: 1,
            hang_after: Duration::from_secs(3),
            max_replaced: 8,
            thumb_hang_factor: 3,
            ctx_hang_after: Duration::from_secs(4),
            thumb_workers: 0,
            thumb_started: false,
            stats: PoolStats::default(),
        };
        for i in 0..workers.max(1) {
            let s = p.spawn(i, LANE_ICON);
            p.slots.push(s);
        }
        p
    }

    /// `workers` icon-lane threads (icons, menus) and `thumb_workers`
    /// thumbnail-lane threads (started on the first thumbnail request, below
    /// normal priority). `thumb_workers == 0`: thumbnails share the icon lane.
    pub fn with_lanes(workers: usize, thumb_workers: usize, hwnd: isize, wake_msg: u32) -> Pool {
        let mut p = Pool::new(workers, hwnd, wake_msg);
        p.thumb_workers = thumb_workers;
        p
    }

    fn spawn(&self, idx: usize, lane: u8) -> Slot {
        let state = Arc::new(WorkerState { busy_since: AtomicU64::new(0), kind: AtomicU8::new(K_IDLE), job: AtomicU64::new(0), abandoned: AtomicBool::new(false), tid: AtomicU32::new(0) });
        let (sh, st, tx) = (self.shared.clone(), state.clone(), self.tx.clone());
        std::thread::Builder::new().name(format!("shell-sta-{idx}")).stack_size(1 << 20).spawn(move || worker(idx, lane, sh, st, tx)).expect("spawn shell worker");
        Slot { state, lane }
    }

    /// Queues a request. Never blocks on a worker. Context menus go to the
    /// front of the icon queue, icons to its back. Thumbnails go to the back
    /// of the thumbnail lane (FIFO: the caller orders them, nearest first); if
    /// the pool has no thumbnail lane they take the front of the icon queue.
    pub fn submit(&mut self, req: Request) -> Ticket {
        let id = self.next_id;
        self.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let lane_thumb = self.thumb_workers > 0 && matches!(req, Request::Thumb { .. });
        // clipboard jobs go to the back: they keep their order (the app submits
        // one at a time) and the in-flight icon limit keeps the queue short
        let front = matches!(req, Request::Menu { .. } | Request::CtxMenu { .. } | Request::Thumb { .. }) && !lane_thumb;
        let job = Job { id, cancel: cancel.clone(), req, queued: Instant::now() };
        if lane_thumb {
            if !self.thumb_started {
                self.thumb_started = true;
                for i in 0..self.thumb_workers {
                    let s = self.spawn(self.slots.len() + i, LANE_THUMB);
                    self.slots.push(s);
                }
            }
            self.shared.tqueue.lock().unwrap().push_back(job);
            unsafe {
                let _ = ReleaseSemaphore(self.shared.tsem.0, 1, None);
            }
        } else {
            {
                let mut q = self.shared.queue.lock().unwrap();
                if front { q.push_front(job) } else { q.push_back(job) }
            }
            unsafe {
                let _ = ReleaseSemaphore(self.shared.sem.0, 1, None);
            }
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
        !self.shared.queue.lock().unwrap().is_empty() || !self.shared.tqueue.lock().unwrap().is_empty() || self.slots.iter().any(|s| matches!(s.state.kind.load(Relaxed), K_ICON | K_THUMB | K_HANG | K_CTX))
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
            let limit = if kind == K_CTX { self.ctx_hang_after.as_millis() as u64 } else { self.hang_after.as_millis() as u64 * if kind == K_THUMB { self.thumb_hang_factor.max(1) as u64 } else { 1 } };
            if since == 0 || kind == K_MENU || kind == K_CLIP || now.saturating_sub(since) < limit {
                continue;
            }
            if self.stats.workers_replaced as usize >= self.max_replaced {
                break;
            }
            st.abandoned.store(true, SeqCst);
            lost.push(st.job.load(Relaxed));
            let lane = self.slots[i].lane;
            let fresh = self.spawn(i, lane);
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
            let n = self.slots.len() as i32;
            let _ = ReleaseSemaphore(self.shared.sem.0, n, None);
            let _ = ReleaseSemaphore(self.shared.tsem.0, n, None);
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

fn worker(idx: usize, lane: u8, sh: Arc<Shared>, st: Arc<WorkerState>, tx: Sender<Reply>) {
    unsafe {
        let _ = OleInitialize(None);
        st.tid.store(GetCurrentThreadId(), Relaxed);
        let thumb_lane = lane == LANE_THUMB;
        if thumb_lane {
            // decodes must not compete with the UI thread's frames
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
        let (sem, queue) = if thumb_lane { (sh.tsem.0, &sh.tqueue) } else { (sh.sem.0, &sh.queue) };
        let host = menu::host_window();
        icon::warm_up();
        loop {
            let r = MsgWaitForMultipleObjectsEx(Some(&[sem]), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            if r == WAIT_OBJECT_0 {
                if sh.quit.load(SeqCst) {
                    break;
                }
                let Some(job) = queue.lock().unwrap().pop_front() else { continue };
                if job.cancel.load(Relaxed) {
                    sh.skipped.fetch_add(1, Relaxed);
                    continue;
                }
                let queued_us = job.queued.elapsed().as_micros() as u64;
                st.job.store(job.id, Relaxed);
                st.kind.store(job.req.kind(), Relaxed);
                st.busy_since.store(sh.now_ms(), Release);
                let t = Instant::now();
                let emit = |p: Payload| -> bool {
                    // an abandoned worker's late batches are thrown away
                    if st.abandoned.load(SeqCst) {
                        return false;
                    }
                    if tx.send(Reply { id: job.id, worker: idx, queued_us, work_us: t.elapsed().as_micros() as u64, payload: p }).is_ok() && !sh.posted.swap(true, SeqCst) {
                        let _ = PostMessageW(Some(HWND(sh.hwnd as *mut _)), sh.wake_msg, WPARAM(0), LPARAM(0));
                    }
                    true
                };
                // true: built, waits for the user (not a hang); false: back to
                // shell work that can hang (a lazy submenu), clock restarts
                let waiting = |idle: bool| {
                    if idle {
                        st.kind.store(K_MENU, Relaxed);
                    } else {
                        st.busy_since.store(sh.now_ms(), Release);
                        st.kind.store(K_CTX, Relaxed);
                    }
                };
                let payload = run(job.req, host, HWND(sh.hwnd as *mut _), &emit, &waiting);
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

fn run(req: Request, host: Option<HWND>, ui: HWND, emit: &dyn Fn(Payload) -> bool, waiting: &dyn Fn(bool)) -> Payload {
    match req {
        Request::Icon { src, size } => Payload::Icon(icon::get(&src, size)),
        Request::Thumb { path, size, cache_only } => thumb::get(&path, size, cache_only),
        Request::Menu { paths, x, y, extended, auto_close, owner, opts } => Payload::Menu(menu::show(host, &paths, x, y, extended, auto_close, owner, ui, &opts)),
        Request::Clip(job) => Payload::Clip(crate::dnd::clipboard::run(job)),
        Request::CtxMenu { paths, generation, opts, session } => Payload::Ctx(ctxmenu::run(host, ui, &paths, generation, &opts, &session, &|b| emit(Payload::Ctx(b)), waiting)),
        Request::Hang { ms } => {
            unsafe { Sleep(ms) };
            Payload::Hang
        }
    }
}

/// The icon call itself, for tests and benches on an STA thread of their own.
/// The app never calls this: it goes through [`Pool`].
pub use icon::get as icon_on_this_thread;
