//! Directory watcher with event coalescing (C4). See `docs/watcher.md`.
//!
//! ```ignore
//! let wake: Wake = Arc::new(move || unsafe { PostMessageW(hwnd, WM_APP_WATCH, 0, 0) });
//! let (watcher, rx) = DirWatcher::start(dir, Options::default(), wake)?;
//!
//! // UI thread, on the wake message:
//! while let Ok(msg) = rx.try_recv() {
//!     match msg {
//!         Message::Batch(b) => { apply_batch(&mut tab.store, Some(&mut tab.sel), dir, &b)?; }
//!         Message::Overflow | Message::Unreliable(_) => { /* relist / relist on focus */ }
//!         Message::Gone(_) | Message::Failed(_) => { /* folder is gone: navigate away or show */ }
//!     }
//! }
//! ```
//!
//! All watchers share one I/O completion port and one thread (`hub`). A watcher
//! costs a directory handle and a 64 KB buffer, not a thread, and an idle
//! watcher costs nothing at all.

mod apply;
mod coalesce;
mod hub;

pub use apply::{ApplyStats, Resolved, apply_batch, apply_resolved, flags_of, resolve};
pub use coalesce::{Batch, ChangeKind, ChangeRef, Coalescer, RawKind};

pub use crate::enumerate::{Wake, Win32Error};

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// Watcher settings. The defaults suit a tab's folder.
#[derive(Clone, Debug)]
pub struct Options {
    /// A window closes this long after its last event (default 60 ms). This is
    /// also the latency of a lone event.
    pub window: Duration,
    /// A window never stays open longer than this after its first event
    /// (default 250 ms), so a long burst arrives as a few batches.
    pub max_delay: Duration,
    /// Kernel record buffer (default 64 KB). Records are about 40 bytes each.
    pub buffer_bytes: usize,
    /// After an overflow the buffer doubles up to this (default 1 MB). A network
    /// path is always held to 64 KB, the most the redirector accepts.
    pub max_buffer_bytes: usize,
    /// Watch the whole subtree. Names are then relative paths, which
    /// [`apply_batch`] skips; use it only for your own bookkeeping.
    pub recursive: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            window: Duration::from_millis(60),
            max_delay: Duration::from_millis(250),
            buffer_bytes: 64 * 1024,
            max_buffer_bytes: 1024 * 1024,
            recursive: false,
        }
    }
}

/// Why notifications for this folder may miss changes. The watcher still runs
/// where it can; the caller should add relist-on-focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreliable {
    /// SMB or other redirected path: servers may not forward every change.
    Network,
    /// USB stick and similar.
    Removable,
    CdRom,
    /// The filesystem rejected change notifications: no watching happens.
    NotSupported,
}

/// Why watching ended. Terminal: no further messages follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gone {
    /// The folder was deleted.
    Deleted,
    /// The folder was renamed or moved (the handle follows it; the path shown is stale).
    Renamed,
    /// The drive or share went away.
    Unavailable,
}

/// What a watcher sends. Always followed by the wake callback.
#[derive(Debug)]
pub enum Message {
    /// One coalesced window. Feed it to [`apply_batch`].
    Batch(Batch),
    /// The kernel buffer overflowed and records were lost: relist the folder.
    /// The watcher carries on (with a bigger buffer).
    Overflow,
    /// Informational, sent once, first: notifications here are unreliable.
    /// `NotSupported` also means the watcher has stopped.
    Unreliable(Unreliable),
    /// The watched folder is gone. Terminal.
    Gone(Gone),
    /// An unexpected OS error (for example access denied). Terminal.
    Failed(Win32Error),
}

/// Watches one directory. Dropping it cancels the pending read and closes the
/// directory handle before `drop` returns; the folder opens with
/// `FILE_SHARE_DELETE`, so it can always be deleted or renamed meanwhile.
pub struct DirWatcher {
    watch: Arc<hub::Watch>,
}

impl DirWatcher {
    /// Starts watching `dir`. `wake` runs on the watcher thread after every
    /// message is queued (the app's version posts a window message and nothing else).
    ///
    /// Fails if the folder cannot be opened (missing, not a directory, access
    /// denied). Everything later arrives as a [`Message`].
    pub fn start(
        dir: impl AsRef<Path>,
        options: Options,
        wake: Wake,
    ) -> Result<(DirWatcher, Receiver<Message>), Win32Error> {
        let (watch, rx) = hub::open(dir.as_ref(), &options, wake)?;
        Ok((DirWatcher { watch }, rx))
    }

    /// The folder is still where it was: not deleted, renamed or unplugged. One
    /// cheap syscall; call it when the window regains focus, because a rename of
    /// the watched folder itself raises no event.
    pub fn still_present(&self) -> bool {
        hub::still_present(&self.watch)
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        hub::close(&self.watch);
    }
}

