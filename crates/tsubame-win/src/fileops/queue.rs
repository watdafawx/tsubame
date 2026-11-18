//! The queue worker: FIFO jobs, conflicts, progress throttle, journal updates.
use super::{
    Cancellation, Choice, ConflictReply, Entry, Event, Journal, OpBackend, Operation, Step, Ticket,
    totals::{ScanRequest, Shared, Totals, scan_loop},
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Ole::{OleInitialize, OleUninitialize},
        Threading::{CreateEventW, INFINITE, SetEvent},
    },
    UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
        PeekMessageW, QS_ALLINPUT, TranslateMessage,
    },
};

/// At most 30 progress events per second, queue wide. 34 ms leaves a safety margin
/// (at most 30 events in any one-second window).
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(34);

pub struct Throttle {
    last: Option<Instant>,
    interval: Duration,
}
impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self {
            last: None,
            interval,
        }
    }
    /// True if an event may be sent at `now`; records it when so.
    pub fn allow(&mut self, now: Instant) -> bool {
        match self.last {
            Some(t) if now.saturating_duration_since(t) < self.interval => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

enum Command {
    Run(Arc<Vec<Operation>>),
    Undo,
    Redo,
    UndoGroup,
    RedoGroup,
}
struct Job {
    id: u64,
    cancel: Cancellation,
    command: Command,
}

/// Auto-reset event that wakes the worker out of its message wait.
struct Wake(HANDLE);
// SAFETY: an event HANDLE may be signalled and closed from any thread.
unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}
impl Wake {
    fn new() -> io::Result<Self> {
        unsafe { CreateEventW(None, false, false, None) }
            .map(Self)
            .map_err(|e| io::Error::other(e.to_string()))
    }
    fn set(&self) {
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
}
impl Drop for Wake {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Dispatch pending window messages (shell extensions and COM marshalling need a pump).
fn pump() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn next_job(jobs: &Receiver<Job>, wake: &Wake) -> Option<Job> {
    loop {
        match jobs.try_recv() {
            Ok(j) => return Some(j),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }
        unsafe {
            MsgWaitForMultipleObjectsEx(
                Some(&[wake.0]),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
        }
        pump();
    }
}

fn lock(j: &Mutex<Journal>) -> MutexGuard<'_, Journal> {
    j.lock().unwrap_or_else(|e| e.into_inner())
}

/// Submission never waits for disk or a conflict response. Dropping detaches the worker.
pub struct FileOpQueue {
    tx: Option<Sender<Job>>,
    scan_tx: Option<Sender<ScanRequest>>,
    wake: Arc<Wake>,
    next: AtomicU64,
    journal: Arc<Mutex<Journal>>,
    shared: Shared,
}

impl FileOpQueue {
    pub fn new<B: OpBackend>(backend: B) -> io::Result<(Self, Receiver<Event>)> {
        Self::with_journal(backend, Arc::new(Mutex::new(Journal::default())))
    }

    /// Start with an existing journal (e.g. loaded from disk).
    pub fn with_journal<B: OpBackend>(
        backend: B,
        journal: Arc<Mutex<Journal>>,
    ) -> io::Result<(Self, Receiver<Event>)> {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (events, rx) = mpsc::channel();
        let (scan_tx, scan_rx) = mpsc::channel::<ScanRequest>();
        let wake = Arc::new(Wake::new()?);
        let shared = Shared::default();
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("tsubame-fileops-scan".into())
                .spawn(move || {
                    let _ = scan_loop(scan_rx, shared);
                })?;
        }
        {
            let (journal, shared, wake) = (journal.clone(), shared.clone(), wake.clone());
            std::thread::Builder::new()
                .name("tsubame-fileops".into())
                .spawn(move || {
                    let ole = unsafe { OleInitialize(None) }.is_ok();
                    let mut worker = Worker {
                        backend: Box::new(backend),
                        events,
                        journal,
                        shared,
                        throttle: Throttle::new(PROGRESS_INTERVAL),
                    };
                    while let Some(job) = next_job(&jobs, &wake) {
                        worker.run_job(job, &wake);
                    }
                    if ole {
                        unsafe { OleUninitialize() };
                    }
                })?;
        }
        Ok((
            Self {
                tx: Some(tx),
                scan_tx: Some(scan_tx),
                wake,
                next: AtomicU64::new(1),
                journal,
                shared,
            },
            rx,
        ))
    }

    fn send(&self, command: Command) -> io::Result<Ticket> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let cancel = Cancellation::default();
        let count = match &command {
            Command::Run(ops) => ops.len(),
            _ => 1,
        };
        let scan = match &command {
            Command::Run(ops) => Some(ops.clone()),
            _ => None,
        };
        self.shared.begin_job(id, count);
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| io::Error::other("queue closed"))?;
        if tx
            .send(Job {
                id,
                cancel: cancel.clone(),
                command,
            })
            .is_err()
        {
            self.shared.end_job(id);
            return Err(io::Error::other("queue closed"));
        }
        if let (Some(ops), Some(scan_tx)) = (scan, &self.scan_tx) {
            let _ = scan_tx.send(ScanRequest {
                id,
                ops,
                cancel: cancel.clone(),
            });
        }
        self.wake.set();
        Ok(Ticket {
            id,
            cancellation: cancel,
        })
    }
    pub fn submit(&self, operations: Vec<Operation>) -> io::Result<Ticket> {
        self.send(Command::Run(Arc::new(operations)))
    }
    pub fn undo(&self) -> io::Result<Ticket> {
        self.send(Command::Undo)
    }
    pub fn redo(&self) -> io::Result<Ticket> {
        self.send(Command::Redo)
    }
    /// Undo the newest submission as a whole (every undoable entry of its group,
    /// newest first). One ticket, one `Finished`.
    pub fn undo_group(&self) -> io::Result<Ticket> {
        self.send(Command::UndoGroup)
    }
    /// Redo the submission undone last, as a whole, in its original order.
    pub fn redo_group(&self) -> io::Result<Ticket> {
        self.send(Command::RedoGroup)
    }
    /// The journal shared with the worker. Lock briefly; the worker takes it between items.
    pub fn journal(&self) -> Arc<Mutex<Journal>> {
        self.journal.clone()
    }
    /// Current queue-wide totals (zero when idle).
    pub fn totals(&self) -> Totals {
        self.shared.snapshot()
    }
}

impl Drop for FileOpQueue {
    fn drop(&mut self) {
        self.tx.take();
        self.scan_tx.take();
        self.wake.set();
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Run,
    Undo(usize),
    Redo,
    RedoAt(usize),
}

struct Worker {
    backend: Box<dyn OpBackend>,
    events: Sender<Event>,
    journal: Arc<Mutex<Journal>>,
    shared: Shared,
    throttle: Throttle,
}

fn conflict_target(op: &mut Operation) -> Option<&mut PathBuf> {
    match op {
        Operation::Copy { target, .. }
        | Operation::Move { target, .. }
        | Operation::Rename { target, .. } => Some(target),
        _ => None,
    }
}

fn is_case_rename(op: &Operation) -> bool {
    matches!(op, Operation::Rename { source, target }
        if source != target
            && source.to_string_lossy().eq_ignore_ascii_case(&target.to_string_lossy()))
}

fn occupied(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

fn keep_both_name(original: &Path) -> PathBuf {
    let mut n = 2;
    loop {
        let mut name = original.file_stem().unwrap_or_default().to_os_string();
        name.push(format!(" ({n})"));
        if let Some(ext) = original.extension() {
            name.push(".");
            name.push(ext);
        }
        let candidate = original.with_file_name(name);
        if !occupied(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Replacing deletes the target first; refuse if that could take the source with it.
fn replace_guard(op: &Operation, target: &Path) -> io::Result<()> {
    let source = match op {
        Operation::Copy { source, .. }
        | Operation::Move { source, .. }
        | Operation::Rename { source, .. } => source,
        _ => return Ok(()),
    };
    let s = std::fs::canonicalize(source)?
        .to_string_lossy()
        .to_lowercase();
    let t = std::fs::canonicalize(target)?
        .to_string_lossy()
        .to_lowercase();
    let (s, t) = (Path::new(&s), Path::new(&t));
    if s.starts_with(t) || t.starts_with(s) {
        return Err(io::Error::other(
            "cannot replace: source and target are the same or contain each other",
        ));
    }
    Ok(())
}

fn reason_for(op: &Operation) -> String {
    match op {
        Operation::Delete {
            permanent: true, ..
        } => "permanent delete cannot be undone".into(),
        Operation::Sequence(ops) if ops.iter().any(|o| reason_for(o).contains("permanent")) => {
            "contains a permanent delete, which cannot be undone".into()
        }
        _ => "this operation has no inverse".into(),
    }
}

/// Run one operation. A `Sequence` runs step by step and rolls back on failure; its inverse
/// is the step inverses in reverse order, or `None` if any step is not undoable.
fn run_op(
    backend: &mut dyn OpBackend,
    op: &Operation,
    cancel: &Cancellation,
    progress: &mut dyn FnMut(Step),
) -> io::Result<Option<Operation>> {
    let Operation::Sequence(ops) = op else {
        return backend.execute(op, cancel, progress);
    };
    let mut inverses = Vec::new();
    let mut undoable = true;
    for sub in ops {
        match run_op(backend, sub, cancel, progress) {
            Ok(Some(inv)) => inverses.push(inv),
            Ok(None) => undoable = false,
            Err(e) => {
                // Roll back what already happened (never cancelled: it must finish).
                let fresh = Cancellation::default();
                for inv in inverses.iter().rev() {
                    let _ = run_op(backend, inv, &fresh, &mut |_| {});
                }
                return Err(e);
            }
        }
    }
    Ok(undoable.then(|| {
        inverses.reverse();
        Operation::Sequence(inverses)
    }))
}

impl Worker {
    fn send(&self, e: Event) {
        let _ = self.events.send(e);
    }

    /// Ask the user (through the event channel) what to do with an occupied target.
    /// `None`: cancelled or nobody listening.
    fn ask(&self, id: u64, target: &Path, cancel: &Cancellation) -> Option<(Choice, bool)> {
        let (tx, rx) = mpsc::channel();
        if self
            .events
            .send(Event::Conflict {
                id,
                target: target.to_path_buf(),
                reply: ConflictReply(tx),
            })
            .is_err()
        {
            return None;
        }
        loop {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(a) => return Some(a),
                Err(mpsc::RecvTimeoutError::Timeout) if !cancel.is_cancelled() => pump(),
                _ => return None,
            }
        }
    }

    fn run_job(&mut self, job: Job, _wake: &Wake) {
        let id = job.id;
        self.send(Event::Started(id));
        let ops: Vec<(Operation, Mode)> = match &job.command {
            Command::Run(ops) => ops.iter().map(|o| (o.clone(), Mode::Run)).collect(),
            Command::Undo => lock(&self.journal).next_undo().map(|(i, inv)| (inv, Mode::Undo(i))).into_iter().collect(),
            Command::Redo => lock(&self.journal).next_redo().map(|fwd| (fwd, Mode::Redo)).into_iter().collect(),
            Command::UndoGroup => lock(&self.journal).undo_group().into_iter().map(|(i, inv)| (inv, Mode::Undo(i))).collect(),
            Command::RedoGroup => lock(&self.journal).redo_group().into_iter().map(|(i, fwd)| (fwd, Mode::RedoAt(i))).collect(),
        };
        let mut apply_all: Option<Choice> = None;
        for (index, (mut op, mode)) in ops.into_iter().enumerate() {
            if job.cancel.is_cancelled() {
                break;
            }
            let shown = op.clone();
            let mut replace_target = None;
            if !is_case_rename(&op)
                && let Some(target) = conflict_target(&mut op)
                && occupied(target)
            {
                let choice = match apply_all {
                    Some(c) => c,
                    None => match self.ask(id, target, &job.cancel) {
                        Some((c, all)) => {
                            if all {
                                apply_all = Some(c);
                            }
                            c
                        }
                        None => {
                            job.cancel.cancel();
                            break;
                        }
                    },
                };
                match choice {
                    Choice::Skip => {
                        self.shared.op_finished(id, index, 0, 0);
                        continue;
                    }
                    Choice::KeepBoth => *target = keep_both_name(target),
                    Choice::Replace => replace_target = Some(target.clone()),
                }
            }
            let mut executed = op;
            if let Some(target) = replace_target {
                if let Err(e) = replace_guard(&executed, &target) {
                    self.shared.op_finished(id, index, 0, 0);
                    self.send(Event::Error {
                        id,
                        item: shown,
                        message: e.to_string(),
                    });
                    continue;
                }
                // Old target goes to the Recycle Bin first, so undo can bring it back.
                executed = Operation::Sequence(vec![
                    Operation::Delete {
                        path: target,
                        permanent: false,
                    },
                    executed,
                ]);
            }
            let current = match &executed {
                Operation::Copy { source, .. }
                | Operation::Move { source, .. }
                | Operation::Rename { source, .. } => source.clone(),
                Operation::Sequence(s) => match s.last() {
                    Some(Operation::Copy { source, .. })
                    | Some(Operation::Move { source, .. })
                    | Some(Operation::Rename { source, .. }) => source.clone(),
                    _ => PathBuf::new(),
                },
                _ => PathBuf::new(),
            };
            let (mut op_bytes, mut op_files) = (0u64, 0u64);
            let result = {
                let events = &self.events;
                let shared = &self.shared;
                let throttle = &mut self.throttle;
                run_op(
                    self.backend.as_mut(),
                    &executed,
                    &job.cancel,
                    &mut |s: Step| {
                        op_bytes += s.bytes;
                        op_files += s.files;
                        shared.step(s.files, s.bytes);
                        if throttle.allow(Instant::now()) {
                            let _ = events.send(Event::Progress {
                                id,
                                bytes_done: op_bytes,
                                current: current.clone(),
                                totals: shared.snapshot(),
                            });
                        }
                    },
                )
            };
            self.shared.op_finished(id, index, op_files, op_bytes);
            match result {
                Ok(inverse) => {
                    let reason = inverse.is_none().then(|| reason_for(&executed));
                    let mut journal = lock(&self.journal);
                    match mode {
                        Mode::Run => journal.record(Entry {
                            forward: executed,
                            inverse,
                            reason,
                            group: id,
                        }),
                        Mode::Undo(i) => journal.mark_undone(i),
                        Mode::Redo => journal.mark_redone(inverse, reason),
                        Mode::RedoAt(i) => journal.mark_redone_at(i, inverse, reason),
                    }
                }
                Err(e) => self.send(Event::Error {
                    id,
                    item: shown,
                    message: e.to_string(),
                }),
            }
        }
        let totals = self.shared.snapshot();
        self.shared.end_job(id);
        self.send(Event::Finished {
            id,
            cancelled: job.cancel.is_cancelled(),
            totals,
        });
    }
}
