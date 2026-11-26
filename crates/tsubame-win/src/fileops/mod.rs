//! Sequential, cancellable file operations with undo/redo. See `docs/fileops.md`.
//!
//! One STA worker thread runs every operation in submit order. The UI thread only submits
//! batches and polls events; it never touches the disk or COM.
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::SystemTime,
};

pub mod deletecheck;
mod fsbackend;
pub mod journal;
mod queue;
pub mod recycle;
#[cfg(test)]
mod tests;
mod totals;

pub use fsbackend::FsBackend;
pub use journal::{Entry, Journal, LoadReport};
pub use queue::{FileOpQueue, PROGRESS_INTERVAL, Throttle};
pub use recycle::RecycledItem;
pub use totals::Totals;

#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    Copy {
        source: PathBuf,
        target: PathBuf,
    },
    Move {
        source: PathBuf,
        target: PathBuf,
    },
    Rename {
        source: PathBuf,
        target: PathBuf,
    },
    /// `permanent: false` sends the item to the Recycle Bin (undoable).
    Delete {
        path: PathBuf,
        permanent: bool,
    },
    NewFolder(PathBuf),
    NewFile(PathBuf),
    /// Put a recycled item back where it came from (inverse of a recycle delete).
    Restore(RecycledItem),
    /// Steps run in order; if one fails the earlier ones are rolled back. Used for
    /// Replace (recycle the old target, then copy/move) so the pair undoes as one.
    Sequence(Vec<Operation>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Replace,
    Skip,
    KeepBoth,
    /// Stop the whole operation (the conflict dialog's "Cancel operation").
    Cancel,
}

/// What the conflict dialog shows about one side: size, date, kind. Read by the
/// worker, so the UI thread never touches the disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileMeta {
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub dir: bool,
    /// the path could be read
    pub known: bool,
}

impl FileMeta {
    pub fn of(p: &Path) -> FileMeta {
        match std::fs::symlink_metadata(p) {
            Ok(m) => FileMeta { size: if m.is_dir() { 0 } else { m.len() }, modified: m.modified().ok(), dir: m.is_dir(), known: true },
            Err(_) => FileMeta::default(),
        }
    }
}

pub struct ConflictReply(Sender<(Choice, bool)>);
impl ConflictReply {
    pub fn answer(self, choice: Choice, apply_to_all: bool) {
        let _ = self.0.send((choice, apply_to_all));
    }
}

pub enum Event {
    Started(u64),
    /// `bytes_done` is cumulative for the current item (operation `index` of the
    /// job, estimated at `item_bytes`, 0 when not scanned yet); `totals` is queue
    /// wide, `job` this job's own numbers.
    Progress {
        id: u64,
        bytes_done: u64,
        current: PathBuf,
        index: usize,
        item_bytes: u64,
        totals: Totals,
        job: Totals,
    },
    /// `target` is occupied. `source` is the incoming item; both are described
    /// (`existing`, `incoming`) for the dialog.
    Conflict {
        id: u64,
        source: PathBuf,
        target: PathBuf,
        existing: FileMeta,
        incoming: FileMeta,
        reply: ConflictReply,
    },
    Error {
        id: u64,
        item: Operation,
        message: String,
    },
    /// `totals` is the last queue-wide snapshot before the counters reset; `job`
    /// this job's own numbers. A job cancelled while it waited sends only this
    /// (no `Started`) and runs nothing.
    Finished {
        id: u64,
        cancelled: bool,
        totals: Totals,
        job: Totals,
    },
}

/// Cancel flag plus the pause gate of one job. The worker checks it between
/// operations and between copy chunks (`check`): while paused it blocks there on
/// a condition variable (no polling, no wake-ups) until resumed or cancelled.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<Gate>);

#[derive(Default)]
struct Gate {
    cancelled: AtomicBool,
    paused: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Cancellation {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Relaxed);
        let _g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.0.cv.notify_all();
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Relaxed)
    }
    /// Pause: the worker stops at its next check. Cancel still ends the job.
    pub fn pause(&self) {
        let _g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.0.paused.store(true, Ordering::Relaxed);
    }
    pub fn resume(&self) {
        let _g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.0.paused.store(false, Ordering::Relaxed);
        self.0.cv.notify_all();
    }
    pub fn is_paused(&self) -> bool {
        self.0.paused.load(Ordering::Relaxed)
    }
    /// Blocks while paused; returns at once when running or cancelled.
    pub fn wait_while_paused(&self) {
        if !self.is_paused() {
            return;
        }
        let mut g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        while self.is_paused() && !self.is_cancelled() {
            g = self.0.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        self.wait_while_paused();
        if self.is_cancelled() {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        } else {
            Ok(())
        }
    }
}

pub struct Ticket {
    pub id: u64,
    pub cancellation: Cancellation,
}

/// Work reported by a backend: file data moved and files completed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub bytes: u64,
    pub files: u64,
}

/// Backends run exclusively on the queue worker (an STA thread). Return an inverse only
/// after success. `Sequence` never reaches a backend.
pub trait OpBackend: Send + 'static {
    fn execute(
        &mut self,
        op: &Operation,
        cancel: &Cancellation,
        progress: &mut dyn FnMut(Step),
    ) -> io::Result<Option<Operation>>;
}
