//! The virtual path model: `host\archive.zip::inner/dir/file.txt`.
//!
//! Two levels, both plain strings so they survive any UI layer: the archive file
//! on disk (`host`), and the path inside it (`inner`). Inner paths are canonical:
//! `/` separators, no leading or trailing separator, no `.` or `..`, no drive
//! prefix. [`sanitize_inner_path`] is the only way a name enters the model, so an
//! archive cannot talk the extractor into writing outside the destination.
use std::path::{Path, PathBuf};

use super::{ArchiveError, ArchiveFormat};

/// Separator between the archive file and the path inside it (`C:\a.zip::dir`).
/// A colon never appears in a file name on Windows, so this cannot be ambiguous.
pub const ARCHIVE_SEP: &str = "::";

/// True when the path names something *inside* an archive (`a.zip::dir`), which
/// is what the app navigates into. A bare `a.zip` is a file, not this.
pub fn is_archive_path(path: &str) -> bool {
    split_archive_path(path).is_some()
}

/// Whether `path` ends in a supported archive suffix (`a.zip`, `a.tar.gz`,
/// `a.7z`), looking only at the archive part of a virtual path.
pub fn is_archive_file(path: &str) -> bool {
    ArchiveFormat::from_path(Path::new(host_archive_path(path))).is_some()
}

/// Splits `host::inner` at the **last** separator, so nested archives work:
/// `a.zip::dir/b.zip::x` is `("a.zip::dir/b.zip", "x")`. `None` when the path
/// has no separator; a missing inner part is `Some((host, ""))`.
pub fn split_archive_path(path: &str) -> Option<(&str, &str)> {
    path.rsplit_once(ARCHIVE_SEP)
}

/// The archive file part of a virtual path, unchanged when there is none.
pub fn host_archive_path(path: &str) -> &str {
    split_archive_path(path).map_or(path, |(host, _)| host)
}

/// Builds a virtual path. An empty inner path is the archive root, which is the
/// bare archive path, so `split(join(a, "")) == (a, "")`.
pub fn join_archive_path(archive: &str, inner: &str) -> String {
    let inner = inner.trim_matches('/');
    if inner.is_empty() {
        archive.to_string()
    } else {
        format!("{archive}{ARCHIVE_SEP}{inner}")
    }
}

/// One step up. Inside an archive the last component goes; at the archive root
/// the archive itself goes, leaving the host folder that holds it
/// (`C:\dir\a.zip::` -> `C:\dir`). `None` at a drive root or a relative name
/// with nothing above it.
pub fn parent_path(path: &str) -> Option<String> {
    let Some((host, inner)) = split_archive_path(path) else {
        return plain_parent(path);
    };
    let inner = inner.trim_end_matches(['/', '\\']);
    match inner.rfind(['/', '\\']) {
        None if inner.is_empty() => plain_parent(host),
        None => Some(host.to_string()),
        Some(0) => Some(host.to_string()),
        Some(i) => Some(join_archive_path(host, &inner[..i])),
    }
}

fn plain_parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let parent = Path::new(trimmed).parent()?;
    let parent = parent.to_string_lossy();
    (!parent.is_empty()).then(|| parent.into_owned())
}

/// Canonical inner path, or `UnsafePath` when a name escapes or cannot be
/// written on Windows. This is the zip-slip gate; run it on every archive name
/// before the name reaches the index or the disk.
///
/// Rejected: absolute names, drive prefixes (`C:`), `..` anywhere, NUL, alternate
/// data streams (`a.txt:evil`), Windows device names (`CON`, `NUL`, `COM1`), and
/// names that Windows would silently shorten (`a.`, `a `). `.` and empty parts
/// are dropped, `\` is taken as a separator, so `a\b` and `a/b` are the same
/// entry. `""` and `"."` are the archive root.
pub fn sanitize_inner_path(inner: &str) -> Result<String, ArchiveError> {
    if inner.is_empty() {
        return Ok(String::new());
    }
    let bad = || ArchiveError::UnsafePath(inner.to_string());
    if inner.contains('\0') || inner.starts_with(['/', '\\']) {
        return Err(bad());
    }
    let mut out = String::with_capacity(inner.len());
    for (i, part) in inner.split(['/', '\\']).enumerate() {
        if i == 0 && has_drive_prefix(part) {
            return Err(bad());
        }
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return Err(bad());
        }
        // Windows drops trailing dots and spaces, so `evil.txt.` would land on
        // `evil.txt`: reject instead of letting two names collide.
        if part.ends_with(['.', ' ']) || is_device_name(part) {
            return Err(bad());
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }
    Ok(out)
}

/// [`sanitize_inner_path`] as a relative path for joining onto an extraction
/// root. The archive root is not a file, so `""` is rejected here.
pub fn sanitize_entry_path(inner: &str) -> Result<PathBuf, ArchiveError> {
    let clean = sanitize_inner_path(inner)?;
    if clean.is_empty() {
        return Err(ArchiveError::UnsafePath(inner.to_string()));
    }
    let mut path = PathBuf::new();
    for part in clean.split('/') {
        debug_assert!(!part.is_empty() && part != "." && part != "..");
        path.push(part);
    }
    Ok(path)
}

/// `C:` or `C:foo`: a drive-relative name that would resolve against the current
/// directory of that drive, not the extraction folder.
fn has_drive_prefix(part: &str) -> bool {
    let b = part.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// `CON`, `NUL`, `AUX`, `PRN`, `COM1`-`COM9`, `LPT1`-`LPT9`, with or without an
/// extension: Windows writes those to a device, not to a file.
fn is_device_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name);
    let b = base.as_bytes();
    if b.len() == 3 {
        return ["con", "prn", "aux", "nul"]
            .iter()
            .any(|d| b.eq_ignore_ascii_case(d.as_bytes()));
    }
    if b.len() == 4 && b[3].is_ascii_digit() && b[3] != b'0' {
        return ["com", "lpt"].iter().any(|d| b[..3].eq_ignore_ascii_case(d.as_bytes()));
    }
    false
}
