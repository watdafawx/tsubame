//! Windows platform: enumerate, watch (ReadDirectoryChangesW), IFileOperation, recycle bin, shell icons/thumbnails/context menu, OLE drag-drop, clipboard.
//!
//! S4 spike contents:
//! - [`shell`]: a pool of STA worker threads that own every shell COM call
//!   (icons, thumbnails, context menus). The UI thread only submits requests
//!   and drains replies. See `docs/shell-threading.md`.
//! - [`bitmap`]: HICON / HBITMAP to premultiplied BGRA.
//! - [`enumerate`]: a minimal `FindFirstFileExW` directory lister (the real
//!   enumerator is a later task).
//! - [`procinfo`]: process counters for leak checks.

pub mod bitmap;
pub mod enumerate;
pub mod procinfo;
pub mod shell;

/// `s` as a NUL-terminated UTF-16 string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
