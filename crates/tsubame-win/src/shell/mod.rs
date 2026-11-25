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
    /// Loads the shell-extension DLLs the background and item menus of
    /// `folder` need (a cold first `QueryContextMenu` takes ~1 s, a warm one
    /// ~15 ms), then drops the menus. Run once after startup at below-normal
    /// priority; answers `Payload::Warm`.
    CtxWarm { folder: String, icon_px: u32 },
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
            Request::CtxMenu { .. } | Request::CtxWarm { .. } => K_CTX,
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
    /// the pre-warm finished (nothing to show)
    Warm,
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

    /// Queues a request. Never blocks on a worker. Context menus (`CtxMenu`
    /// and `Menu`) go to the very front of the icon queue so right-click always
    /// preempts queued icon fetches immediately; icons and clipboard jobs go to
    /// its back. Thumbnails go to the back of the thumbnail lane (FIFO: the
    /// caller orders them, nearest first); if the pool has no thumbnail lane
    /// they queue behind any pending context menus and ahead of icons.
    pub fn submit(&mut self, req: Request) -> Ticket {
        let id = self.next_id;
        self.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let lane_thumb = self.thumb_workers > 0 && matches!(req, Request::Thumb { .. });
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
                enqueue_icon_lane(&mut q, job);
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
                if crate::lagtrace::on()
                    && let Request::CtxMenu { generation, ref paths, ref opts, .. } = job.req
                {
                    crate::lagtrace::record("ctx.worker.start", Some(queued_us as f64 / 1000.0), format_args!("gen={generation} worker={idx} paths={} bg={}", paths.len(), opts.background), false);
                }
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
        Request::CtxWarm { folder, icon_px } => {
            // the worker is a normal-priority thread: warm below it so a first
            // folder's icons and the UI thread's frames win
            unsafe {
                let th = GetCurrentThread();
                let _ = SetThreadPriority(th, THREAD_PRIORITY_BELOW_NORMAL);
                ctxmenu::warm(host, &folder, icon_px);
                let _ = SetThreadPriority(th, THREAD_PRIORITY_NORMAL);
            }
            Payload::Warm
        }
        Request::Hang { ms } => {
            unsafe { Sleep(ms) };
            Payload::Hang
        }
    }
}

/// The icon call itself, for tests and benches on an STA thread of their own.
/// The app never calls this: it goes through [`Pool`].
pub use icon::get as icon_on_this_thread;

/// Priority inside `LANE_ICON`: context menus (`CtxMenu` / `Menu`) always go
/// to the very front, fallback thumbnails (when no thumbnail lane exists) go
/// after any pending menus and before icons, and icons/clipboard/hang go to
/// the back.
fn enqueue_icon_lane(q: &mut VecDeque<Job>, job: Job) {
    match job.req {
        Request::CtxMenu { .. } | Request::Menu { .. } => q.push_front(job),
        Request::Thumb { .. } => {
            let pos = q.iter().position(|j| !matches!(j.req, Request::CtxMenu { .. } | Request::Menu { .. })).unwrap_or(q.len());
            q.insert(pos, job);
        }
        _ => q.push_back(job),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_job(id: u64, req: Request) -> Job {
        Job { id, cancel: Arc::new(AtomicBool::new(false)), req, queued: Instant::now() }
    }

    #[test]
    fn enqueue_icon_lane_prioritizes_ctxmenu_and_menu_ahead_of_icons_and_thumbs() {
        let mut q = VecDeque::new();
        let h = CtxHandle::new();
        enqueue_icon_lane(&mut q, fake_job(1, Request::Icon { src: IconSource::Folder, size: 16 }));
        enqueue_icon_lane(&mut q, fake_job(2, Request::Icon { src: IconSource::Ext(".txt".into()), size: 16 }));
        enqueue_icon_lane(&mut q, fake_job(3, Request::Thumb { path: "a.png".into(), size: 64, cache_only: true }));
        enqueue_icon_lane(&mut q, fake_job(4, Request::ctx(vec![], 1, CtxOpts::default(), &h)));
        // A later thumbnail on the shared lane must not jump ahead of the queued CtxMenu
        enqueue_icon_lane(&mut q, fake_job(5, Request::Thumb { path: "b.png".into(), size: 64, cache_only: true }));
        enqueue_icon_lane(&mut q, fake_job(6, Request::Menu { paths: vec![], x: 0, y: 0, extended: false, auto_close: Some(0), owner: None, opts: MenuOpts::default() }));
        let order: Vec<u64> = q.into_iter().map(|j| j.id).collect();
        assert_eq!(order, vec![6, 4, 5, 3, 1, 2]);
    }

    #[test]
    fn pool_ctxmenu_preempts_queued_icons() {
        let mut pool = Pool::new(1, 0, 0x8001);
        // Hold the single worker so subsequent jobs accumulate in the queue.
        // The hold must really be running before the rest is queued, else a CtxMenu pushed to
        // the front could overtake it. A fixed sleep was not enough on a loaded machine, so
        // wait until the worker took it off the queue; the hold is long enough to outlast the
        // submits below even when this thread is starved.
        let hold = pool.submit(Request::Hang { ms: 1000 });
        let t0 = Instant::now();
        while !pool.shared.queue.lock().unwrap().is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(20), "the worker never took the hold job");
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut icons = Vec::new();
        for ext in [".txt", ".rs", ".md", ".json"] {
            icons.push(pool.submit(Request::Icon { src: IconSource::Ext(ext.into()), size: 16 }).id);
        }
        let h = CtxHandle::new();
        let ctx = pool.submit(Request::ctx(vec![], 77, CtxOpts::default(), &h));

        let mut order = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while order.len() < 1 + icons.len() + 1 && Instant::now() < deadline {
            for r in pool.drain() {
                order.push(r.id);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(order.len(), 6, "all jobs finished: {order:?}");
        assert_eq!(order[0], hold.id, "in-flight job finishes first");
        assert_eq!(order[1], ctx.id, "CtxMenu preempts all queued Icon jobs: {order:?}");
    }
}
