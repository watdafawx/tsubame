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

pub mod bitmap;
pub mod enumerate;
pub mod places;
pub mod procinfo;
pub mod shell;
pub mod watch;

/// `s` as a NUL-terminated UTF-16 string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
