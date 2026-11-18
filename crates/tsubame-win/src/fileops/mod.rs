//! Sequential, cancellable file operations with undo/redo. See `docs/fileops.md`.
//!
//! One STA worker thread runs every operation in submit order. The UI thread only submits
//! batches and polls events; it never touches the disk or COM.
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
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
}

pub struct ConflictReply(Sender<(Choice, bool)>);
impl ConflictReply {
    pub fn answer(self, choice: Choice, apply_to_all: bool) {
        let _ = self.0.send((choice, apply_to_all));
    }
}

pub enum Event {
    Started(u64),
    /// `bytes_done` is cumulative for the current item; `totals` is queue wide.
    Progress {
        id: u64,
        bytes_done: u64,
        current: PathBuf,
        totals: Totals,
    },
    Conflict {
        id: u64,
        target: PathBuf,
        reply: ConflictReply,
    },
    Error {
        id: u64,
        item: Operation,
        message: String,
    },
    /// `totals` is the last queue-wide snapshot before the counters reset.
    Finished {
        id: u64,
        cancelled: bool,
        totals: Totals,
    },
}

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub(crate) fn check(&self) -> io::Result<()> {
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
