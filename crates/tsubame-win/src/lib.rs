//! Windows platform: enumerate, watch (ReadDirectoryChangesW), IFileOperation, recycle bin, shell icons/thumbnails/context menu, OLE drag-drop, clipboard.
//!
//! S4 spike contents:
//! - [`shell`]: a pool of STA worker threads that own every shell COM call
//!   (icons, thumbnails, context menus). The UI thread only submits requests
//!   and drains replies. See `docs/shell-threading.md`.
//! - [`bitmap`]: HICON / HBITMAP to premultiplied BGRA.
//! - [`enumerate`]: the streaming directory enumerator (NT query + Find
//!   fallback on one worker thread, batches with generation cancel). See
//!   `docs/enumerator.md`.
//! - [`procinfo`]: process counters for leak checks.
//! - [`meta`]: background metadata engines (folder sizes, git status, file
//!   checksums) for the P4.F5 columns and checksum command.
//! - [`preview`]: the P4.F2 preview pane and Quick Look backend — image, text,
//!   media metadata and hex decoders behind one async worker.

pub mod bitmap;
pub mod defaultfm;
pub mod dnd;
pub mod enumerate;
pub mod lagtrace;
pub mod meta;
pub mod places;
pub mod preview;
pub mod procinfo;
pub mod search;
pub mod shell;
pub mod tz;
pub mod uia;
pub mod watch;

/// `s` as a NUL-terminated UTF-16 string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
pub mod fileops;
