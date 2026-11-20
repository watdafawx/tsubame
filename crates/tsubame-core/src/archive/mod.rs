//! Archive VFS: browse and extract zip, tar, tar.gz and 7z without a scratch copy.
//!
//! Everything here is portable (no `windows` dependency), so the app can run it on
//! its own worker threads and a `tsubame-ext-api::VfsProvider` can wrap it as is.
//!
//! A path inside an archive is `host\archive.zip::dir/file.txt`
//! ([`join_archive_path`]); the archive root is the bare archive path. Inner paths
//! always use `/`, never start with a separator and never contain `..`: anything
//! else is rejected by [`sanitize_inner_path`] (zip-slip defense).
//!
//! ```ignore
//! let mut cache = ArchiveIndexCache::new();
//! let rows = list_dir_cached(&mut cache, Path::new(r"C:\a.zip"), "docs")?;
//! extract_entries(Path::new(r"C:\a.zip"), &["docs"], Path::new(r"C:\out"), &cancel, |done, total| {})?;
//! ```
//!
//! Costs: listing reads the archive index once per archive (the zip central
//! directory, the tar headers, the 7z header), then every folder inside it is a
//! lookup. [`ArchiveIndexCache`] keeps up to 8 indexes / 16 MB, keyed by path,
//! length and mtime, so a click in a large archive does not re-parse it.
//! Extraction streams: memory stays at one 64 KB buffer, never the whole entry.
use std::fmt;
use std::path::PathBuf;

mod format;
mod index;
mod path;
mod read;
#[cfg(test)]
mod tests;

pub use format::ArchiveFormat;
pub use index::{
    ArchiveEntry, ArchiveIndex, ArchiveIndexCache, ARCHIVE_CACHE_BYTES, ARCHIVE_CACHE_ENTRIES,
};
pub use path::{
    ARCHIVE_SEP, host_archive_path, is_archive_file, is_archive_path, join_archive_path,
    parent_path, sanitize_entry_path, sanitize_inner_path, split_archive_path,
};
pub use read::{
    ExtractOptions, ExtractReport, Overwrite, extract_all, extract_all_indexed, extract_all_with,
    extract_entries, extract_entries_with, extract_selection_indexed, list_dir, list_dir_cached,
    read_entry_bytes,
};

/// Why an archive could not be listed, read or extracted.
#[derive(Debug)]
pub enum ArchiveError {
    Io(std::io::Error),
    Zip(zip::result::ZipError),
    SevenZ(sevenz_rust2::Error),
    /// The path is not one of the formats this build reads.
    Unsupported(String),
    /// A name inside the archive would escape the destination (zip-slip), or is a
    /// Windows device name, an alternate data stream or otherwise unwritable.
    UnsafePath(String),
    /// No such entry (or folder) in the archive.
    NotFound(String),
    /// The entry (or the requested prefix) is larger than the caller's cap.
    TooLarge { size: u64, limit: u64 },
    /// Extraction target exists and [`Overwrite::Fail`] was asked for.
    Exists(PathBuf),
    /// The caller's [`std::sync::atomic::AtomicBool`] was set.
    Cancelled,
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Zip(e) => write!(f, "{e}"),
            Self::SevenZ(e) => write!(f, "{e}"),
            Self::Unsupported(p) => write!(f, "not a supported archive: {p}"),
            Self::UnsafePath(p) => write!(f, "unsafe path inside the archive: {p}"),
            Self::NotFound(p) => write!(f, "not in the archive: {p}"),
            Self::TooLarge { size, limit } => {
                write!(f, "entry is {size} bytes, over the {limit} byte limit")
            }
            Self::Exists(p) => write!(f, "already exists: {}", p.display()),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for ArchiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Zip(e) => Some(e),
            Self::SevenZ(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<zip::result::ZipError> for ArchiveError {
    fn from(e: zip::result::ZipError) -> Self {
        Self::Zip(e)
    }
}

impl From<sevenz_rust2::Error> for ArchiveError {
    fn from(e: sevenz_rust2::Error) -> Self {
        Self::SevenZ(e)
    }
}
