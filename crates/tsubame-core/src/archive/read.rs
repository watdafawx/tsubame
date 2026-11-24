//! Reading archives: one pass to index, one streaming pass to extract.
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::index::is_under;
use super::{
    ArchiveEntry, ArchiveError, ConflictAnswer, ConflictChoice, ConflictState, keep_both_path, ArchiveFormat, ArchiveIndex, ArchiveIndexCache, sanitize_entry_path,
    sanitize_inner_path,
};

/// Streaming copy buffer: extraction never holds a whole entry in memory.
const COPY_BUFFER: usize = 64 << 10;

/// What to do when an extracted file already exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overwrite {
    /// Replace it (read-only files included). The default, as in Explorer.
    #[default]
    Replace,
    /// Keep the existing file and count the entry in [`ExtractReport::skipped`].
    Skip,
    /// Fail with [`ArchiveError::Exists`].
    Fail,
}

/// Tuning for [`extract_all_with`] / [`extract_entries_with`].
///
/// Symlinks stored in an archive are never created. An existing symlink at a file
/// target is replaced by the file instead of being written through, and a
/// pre-existing symlinked folder in the way is refused ([`ArchiveError::UnsafePath`]).
#[derive(Clone, Debug, Default)]
pub struct ExtractOptions {
    pub overwrite: Overwrite,
    /// Turn a zip-slip or otherwise unsafe entry name into an error instead of
    /// skipping it. Off by default: one hostile name should not abort a 10,000
    /// file extraction, and [`ExtractReport::unsafe_paths`] reports it either way.
    pub strict_paths: bool,
}

/// What an extraction did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractReport {
    /// Files written.
    pub files: usize,
    /// Directory entries created (folders implied by a file's path are not counted).
    pub dirs: usize,
    /// Bytes written, uncompressed.
    pub bytes: u64,
    /// Entries deliberately not written: existing files under [`Overwrite::Skip`],
    /// symlinks and special files.
    pub skipped: usize,
    /// Names refused by the path check (zip-slip, device names, alternate data
    /// streams); empty for a clean archive.
    pub unsafe_paths: Vec<String>,
}

/// Lists one folder of an archive. The archive is parsed for this call only: give
/// the app an [`ArchiveIndexCache`] and use [`list_dir_cached`] for clicks.
pub fn list_dir(archive: &Path, inner: &str) -> Result<Vec<ArchiveEntry>, ArchiveError> {
    open_index(archive)?.list_dir(inner)
}

/// [`list_dir`] with the bounded index cache: a hit is a hash lookup, a miss
/// parses the archive once.
pub fn list_dir_cached(
    cache: &mut ArchiveIndexCache,
    archive: &Path,
    inner: &str,
) -> Result<Vec<ArchiveEntry>, ArchiveError> {
    cache.get_or_load(archive)?.list_dir(inner)
}

/// Extracts every entry to `dest`, replacing existing files.
pub fn extract_all(
    archive: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<ExtractReport, ArchiveError> {
    extract_all_with(archive, dest, &ExtractOptions::default(), cancel, progress)
}

/// [`extract_all`] with overwrite/strictness options.
pub fn extract_all_with(
    archive: &Path,
    dest: &Path,
    options: &ExtractOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<ExtractReport, ArchiveError> {
    let index = open_index(archive)?;
    extract_all_indexed(&index, dest, options, cancel, progress)
}

/// [`extract_all`] on an index the caller already has (from the cache), so the
/// archive is parsed once instead of twice.
pub fn extract_all_indexed(
    index: &ArchiveIndex,
    dest: &Path,
    options: &ExtractOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<ExtractReport, ArchiveError> {
    extract_impl(index, None, dest, options, cancel, progress, None)
}

/// Extracts the selected entries, files and folders (recursively), keeping their
/// paths relative to the archive root. `""` selects the whole archive.
pub fn extract_entries(
    archive: &Path,
    inner: &[&str],
    dest: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<ExtractReport, ArchiveError> {
    extract_entries_with(archive, inner, dest, &ExtractOptions::default(), cancel, progress)
}

/// [`extract_entries`] with overwrite/strictness options.
pub fn extract_entries_with(
    archive: &Path,
    inner: &[&str],
    dest: &Path,
    options: &ExtractOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<ExtractReport, ArchiveError> {
    let index = open_index(archive)?;
    let selection = selection_of(&index, inner)?;
    extract_impl(&index, Some(&selection), dest, options, cancel, progress, None)
}

/// [`extract_entries`] on an index the caller already has. `selection` holds
/// canonical inner paths as [`ArchiveIndex::entry`] returns them.
pub fn extract_selection_indexed(
    index: &ArchiveIndex,
    selection: &[String],
    dest: &Path,
    options: &ExtractOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<ExtractReport, ArchiveError> {
    let refs: Vec<&str> = selection.iter().map(String::as_str).collect();
    let selection = selection_of(index, &refs)?;
    extract_impl(index, Some(&selection), dest, options, cancel, progress, None)
}

/// Extracts everything (`selection` `None`) or the canonical `selection` paths,
/// asking `ask` whenever a file target already exists: skip it, replace it or
/// write `name (2)` beside it. An "apply to all" answer is asked once. Every name
/// still goes through the zip-slip check first; a kept-both name stays in the
/// folder the check cleared.
pub fn extract_resolving(
    index: &ArchiveIndex,
    selection: Option<&[String]>,
    dest: &Path,
    options: &ExtractOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
    ask: &mut dyn FnMut(&Path) -> ConflictAnswer,
) -> Result<ExtractReport, ArchiveError> {
    let selection = match selection {
        Some(sel) => {
            let refs: Vec<&str> = sel.iter().map(String::as_str).collect();
            Some(selection_of(index, &refs)?)
        }
        None => None,
    };
    extract_impl(index, selection.as_ref(), dest, options, cancel, progress, Some(ask))
}

/// Bytes of one entry, for the preview pane. Fails with
/// [`ArchiveError::TooLarge`] rather than reading more than `max_bytes`.
pub fn read_entry_bytes(archive: &Path, inner: &str, max_bytes: u64) -> Result<Vec<u8>, ArchiveError> {
    let want = sanitize_inner_path(inner)?;
    if want.is_empty() {
        return Err(ArchiveError::NotFound(inner.to_string()));
    }
    let format = ArchiveFormat::detect(archive)?;
    let mut out = Vec::new();
    match format {
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipArchive::new(BufReader::new(File::open(archive)?))?;
            let mut found = None;
            for index in 0..zip.len() {
                let entry = zip.by_index_data(index)?;
                if entry.is_dir() {
                    continue;
                }
                if sanitize_inner_path(&zip_name(&entry)).ok().as_deref() == Some(want.as_str()) {
                    found = Some((index, entry.size()));
                    break;
                }
            }
            let Some((index, declared)) = found else {
                return Err(ArchiveError::NotFound(inner.to_string()));
            };
            if declared > max_bytes {
                return Err(ArchiveError::TooLarge { size: declared, limit: max_bytes });
            }
            let mut file = zip.by_index(index)?;
            read_capped(&mut file, max_bytes, &mut out)?;
        }
        ArchiveFormat::Tar | ArchiveFormat::TarGz => {
            let reader = BufReader::new(File::open(archive)?);
            let found = if format == ArchiveFormat::TarGz {
                tar_read_entry(&mut tar::Archive::new(flate2::read::GzDecoder::new(reader)), &want, max_bytes, &mut out)?
            } else {
                tar_read_entry(&mut tar::Archive::new(reader), &want, max_bytes, &mut out)?
            };
            if !found {
                return Err(ArchiveError::NotFound(inner.to_string()));
            }
        }
        ArchiveFormat::SevenZ => {
            let mut reader = sevenz_rust2::ArchiveReader::open(archive, sevenz_rust2::Password::empty())?;
            let mut error = None;
            let mut found = false;
            let result = reader.for_each_entries_with_decision(|entry, stream| {
                if found || entry.is_directory() {
                    return Ok(sevenz_rust2::EntryDecision::Continue);
                }
                if sanitize_inner_path(entry.name()).ok().as_deref() != Some(want.as_str()) {
                    return Ok(sevenz_rust2::EntryDecision::Continue);
                }
                found = true;
                let declared = entry.size();
                let outcome = if declared > max_bytes {
                    Err(ArchiveError::TooLarge { size: declared, limit: max_bytes })
                } else {
                    read_capped(stream, max_bytes, &mut out)
                };
                match outcome {
                    Ok(()) => Ok(sevenz_rust2::EntryDecision::Stop),
                    Err(e) => {
                        error = Some(e);
                        Ok(sevenz_rust2::EntryDecision::Stop)
                    }
                }
            });
            if let Some(error) = error {
                return Err(error);
            }
            result?;
            if !found {
                return Err(ArchiveError::NotFound(inner.to_string()));
            }
        }
    }
    Ok(out)
}

/// Parses an archive into an index. Cheap for zip (central directory) and 7z
/// (header); a `.tar.gz` must be decompressed once to read every header.
pub(crate) fn open_index(archive: &Path) -> Result<ArchiveIndex, ArchiveError> {
    let format = ArchiveFormat::detect(archive)?;
    let mut index = ArchiveIndex::new(archive.to_path_buf(), format);
    match format {
        ArchiveFormat::Zip => read_zip_index(archive, &mut index)?,
        ArchiveFormat::Tar => read_tar_index(archive, &mut index, false)?,
        ArchiveFormat::TarGz => read_tar_index(archive, &mut index, true)?,
        ArchiveFormat::SevenZ => read_7z_index(archive, &mut index)?,
    }
    index.seal();
    Ok(index)
}

fn read_zip_index(path: &Path, index: &mut ArchiveIndex) -> Result<(), ArchiveError> {
    let zip = zip::ZipArchive::new(BufReader::new(File::open(path)?))?;
    for i in 0..zip.len() {
        let entry = zip.by_index_data(i)?;
        if entry.is_symlink() {
            index.skip();
            continue;
        }
        index.add(
            &zip_name(&entry),
            entry.is_dir(),
            entry.size(),
            entry.compressed_size(),
            entry.last_modified().map_or(0, zip_time),
        );
    }
    Ok(())
}

fn read_tar_index(path: &Path, index: &mut ArchiveIndex, gzip: bool) -> Result<(), ArchiveError> {
    let reader = BufReader::new(File::open(path)?);
    if gzip {
        tar_index(&mut tar::Archive::new(flate2::read::GzDecoder::new(reader)), index)
    } else {
        tar_index(&mut tar::Archive::new(reader), index)
    }
}

fn tar_index<R: Read>(tar: &mut tar::Archive<R>, index: &mut ArchiveIndex) -> Result<(), ArchiveError> {
    for entry in tar.entries()? {
        let entry = entry?;
        let is_dir = entry.header().entry_type().is_dir();
        if !is_dir && !entry.header().entry_type().is_file() {
            index.skip(); // symlink, hard link, device, fifo
            continue;
        }
        let name = entry.path()?.to_string_lossy().into_owned();
        let size = if is_dir { 0 } else { entry.header().size()? };
        let modified = filetime_from_unix(entry.header().mtime().unwrap_or(0));
        // tar stores no per-entry compressed size
        index.add(&name, is_dir, size, 0, modified);
    }
    Ok(())
}

fn read_7z_index(path: &Path, index: &mut ArchiveIndex) -> Result<(), ArchiveError> {
    let reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())?;
    for entry in &reader.archive().files {
        if entry.is_anti_item() {
            index.skip();
            continue;
        }
        let modified = i64::try_from(u64::from(entry.last_modified_date())).unwrap_or(i64::MAX);
        index.add(entry.name(), entry.is_directory(), entry.size(), 0, modified);
    }
    Ok(())
}

/// Zip names are UTF-8 with the language-encoding flag, CP437 otherwise; a name
/// that will not decode is taken lossily rather than dropped.
fn zip_name(entry: &zip::read::ZipFileEntry<'_>) -> String {
    match entry.name() {
        Ok(name) => name.into_owned(),
        Err(_) => String::from_utf8_lossy(entry.name_raw()).into_owned(),
    }
}

/// 100 ns ticks between 1601-01-01 and 1970-01-01: the FILETIME clock the rest of
/// the app stores timestamps in.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

fn filetime_from_unix(seconds: u64) -> i64 {
    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX / 10_000_000);
    FILETIME_UNIX_EPOCH.saturating_add(seconds.saturating_mul(10_000_000))
}

/// A zip DOS timestamp as FILETIME ticks (UTC). DOS time has no zone: it is the
/// wall clock of whoever wrote the archive, so it is read as local time and
/// converted to UTC, which is the clock tar and 7z entries already carry.
pub(crate) fn zip_time(time: zip::DateTime) -> i64 {
    let local = FILETIME_UNIX_EPOCH.saturating_add(zip_wall_seconds(time).saturating_mul(10_000_000));
    local_to_utc(local)
}

/// DOS date and time as seconds since 1970-01-01 on the same wall clock.
pub(crate) fn zip_wall_seconds(time: zip::DateTime) -> i64 {
    let days = days_from_civil(i64::from(time.year()), u32::from(time.month()), u32::from(time.day()));
    days * 86_400 + i64::from(time.hour()) * 3_600 + i64::from(time.minute()) * 60 + i64::from(time.second())
}

/// Local wall clock (as FILETIME ticks read in UTC calendar maths) to real UTC,
/// using the system zone rules in force on that date. Falls back to the input when
/// the zone API fails (and off Windows, where a zip time is taken as UTC).
#[cfg(windows)]
fn local_to_utc(ticks: i64) -> i64 {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime, TzSpecificLocalTimeToSystemTimeEx};
    let Ok(raw) = u64::try_from(ticks) else { return ticks };
    let ft = FILETIME { dwLowDateTime: raw as u32, dwHighDateTime: (raw >> 32) as u32 };
    let (mut local, mut utc, mut out) = (SYSTEMTIME::default(), SYSTEMTIME::default(), FILETIME::default());
    // SAFETY: plain Win32 calls on locals; a None zone means the current system zone.
    let ok = unsafe {
        FileTimeToSystemTime(&ft, &mut local).is_ok()
            && TzSpecificLocalTimeToSystemTimeEx(None, &local, &mut utc).is_ok()
            && SystemTimeToFileTime(&utc, &mut out).is_ok()
    };
    if !ok {
        return ticks;
    }
    (i64::from(out.dwHighDateTime) << 32) | i64::from(out.dwLowDateTime)
}

#[cfg(not(windows))]
fn local_to_utc(ticks: i64) -> i64 {
    ticks
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`). No dependency, no leap-second table.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted = (month + 9) % 12;
    let day_of_year = (153 * i64::from(shifted) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn tar_read_entry<R: Read>(
    tar: &mut tar::Archive<R>,
    want: &str,
    max_bytes: u64,
    out: &mut Vec<u8>,
) -> Result<bool, ArchiveError> {
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry.path()?.to_string_lossy().into_owned();
        if sanitize_inner_path(&name).ok().as_deref() != Some(want) {
            continue;
        }
        let declared = entry.header().size()?;
        if declared > max_bytes {
            return Err(ArchiveError::TooLarge { size: declared, limit: max_bytes });
        }
        read_capped(&mut entry, max_bytes, out)?;
        return Ok(true);
    }
    Ok(false)
}

/// Reads at most `max_bytes`; a stream that is longer (a lying header) is an
/// error, not a bigger allocation.
pub(crate) fn read_capped(reader: &mut dyn Read, max_bytes: u64, out: &mut Vec<u8>) -> Result<(), ArchiveError> {
    let mut taken = reader.take(max_bytes.saturating_add(1));
    taken.read_to_end(out)?;
    if out.len() as u64 > max_bytes {
        return Err(ArchiveError::TooLarge { size: out.len() as u64, limit: max_bytes });
    }
    Ok(())
}

/// Canonical inner paths of a selection, validated against the index: a path that
/// is not there is an error, never a silent empty extraction.
fn selection_of(index: &ArchiveIndex, inner: &[&str]) -> Result<Selection, ArchiveError> {
    let mut paths: Vec<String> = Vec::with_capacity(inner.len());
    for raw in inner {
        let clean = sanitize_inner_path(raw)?;
        if !clean.is_empty() && index.entry(&clean).is_none() {
            return Err(ArchiveError::NotFound((*raw).to_string()));
        }
        if !paths.contains(&clean) {
            paths.push(clean);
        }
    }
    Ok(Selection { paths })
}

struct Selection {
    /// Canonical paths; `""` is the archive root and matches everything.
    paths: Vec<String>,
}

impl Selection {
    fn matches(&self, clean: &str) -> bool {
        self.paths.iter().any(|p| p.is_empty() || p == clean || is_under(clean, p))
    }
}

fn extract_impl(
    index: &ArchiveIndex,
    selection: Option<&Selection>,
    dest: &Path,
    options: &ExtractOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
    ask: Option<&mut dyn FnMut(&Path) -> ConflictAnswer>,
) -> Result<ExtractReport, ArchiveError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(ArchiveError::Cancelled);
    }
    let total = match selection {
        None => index.total_size(),
        // count each file once, even when a folder and one of its files are selected
        Some(selection) => index
            .files_under("")
            .filter(|entry| selection.matches(&entry.inner_path))
            .map(|entry| entry.size)
            .sum(),
    };
    fs::create_dir_all(dest)?;
    progress(0, total);
    let mut sink = Sink {
        root: dest.to_path_buf(),
        options,
        cancel,
        progress: &mut progress,
        out: ExtractReport::default(),
        done: 0,
        total,
        ask: match ask {
            Some(ask) => Some(ask as &mut dyn FnMut(&Path) -> ConflictAnswer),
            None => None,
        },
        conflicts: ConflictState::default(),
    };
    match index.format() {
        ArchiveFormat::Zip => extract_zip(index.archive_path(), selection, &mut sink)?,
        ArchiveFormat::Tar => extract_tar(index.archive_path(), false, selection, &mut sink)?,
        ArchiveFormat::TarGz => extract_tar(index.archive_path(), true, selection, &mut sink)?,
        ArchiveFormat::SevenZ => extract_7z(index.archive_path(), selection, &mut sink)?,
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(ArchiveError::Cancelled);
    }
    Ok(sink.out)
}

struct Sink<'a> {
    root: PathBuf,
    options: &'a ExtractOptions,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(u64, u64),
    out: ExtractReport,
    done: u64,
    total: u64,
    /// Asked for each existing file target when set; `None` follows `options.overwrite`.
    ask: Option<&'a mut (dyn FnMut(&Path) -> ConflictAnswer + 'a)>,
    conflicts: ConflictState,
}

impl Sink<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Writes one entry (or records why not). Every archive name goes through
    /// here, so the zip-slip check cannot be skipped by a backend.
    fn entry(&mut self, inner: &str, is_dir: bool, reader: Option<&mut dyn Read>) -> Result<(), ArchiveError> {
        if self.cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let relative = match sanitize_entry_path(inner) {
            Ok(relative) => relative,
            Err(_) => {
                self.out.unsafe_paths.push(inner.to_string());
                return if self.options.strict_paths {
                    Err(ArchiveError::UnsafePath(inner.to_string()))
                } else {
                    Ok(())
                };
            }
        };
        let mut target = self.root.join(&relative);
        self.ensure_parents(&relative)?;
        if is_dir {
            self.create_dir(&target)?;
            self.out.dirs += 1;
            return Ok(());
        }
        let Some(reader) = reader else {
            self.out.skipped += 1;
            return Ok(());
        };
        if self.ask.is_some() {
            match self.resolve_conflict(&target)? {
                Some(chosen) => target = chosen,
                None => return Ok(()),
            }
        } else if !self.wants_write(&target)? {
            return Ok(());
        }
        let mut file = replace_file(&target)?;
        let before = self.done;
        let mut buffer = vec![0u8; COPY_BUFFER];
        loop {
            if self.cancelled() {
                drop(file);
                let _ = fs::remove_file(&target);
                return Err(ArchiveError::Cancelled);
            }
            let read = match reader.read(&mut buffer) {
                Ok(read) => read,
                Err(e) => {
                    drop(file);
                    let _ = fs::remove_file(&target);
                    return Err(e.into());
                }
            };
            if read == 0 {
                break;
            }
            if let Err(e) = file.write_all(&buffer[..read]) {
                drop(file);
                let _ = fs::remove_file(&target);
                return Err(e.into());
            }
            self.done += read as u64;
            (self.progress)(self.done.min(self.total), self.total);
        }
        file.flush()?;
        drop(file);
        self.out.files += 1;
        self.out.bytes += self.done - before;
        Ok(())
    }

    /// Asks about an existing file target. `Some(path)` is where to write (the
    /// target itself, or its `name (2)`), `None` means skip. A real folder in the
    /// way is still an error: there is nothing to replace or rename around.
    fn resolve_conflict(&mut self, target: &Path) -> Result<Option<PathBuf>, ArchiveError> {
        let Ok(metadata) = fs::symlink_metadata(target) else {
            return Ok(Some(target.to_path_buf()));
        };
        if metadata.is_dir() {
            return Err(ArchiveError::Exists(target.to_path_buf()));
        }
        let Some(ask) = self.ask.as_mut() else {
            return Ok(Some(target.to_path_buf()));
        };
        match self.conflicts.decide(target, &mut **ask) {
            ConflictChoice::Replace => Ok(Some(target.to_path_buf())),
            ConflictChoice::Skip => {
                self.out.skipped += 1;
                Ok(None)
            }
            ConflictChoice::KeepBoth => Ok(Some(keep_both_path(target, &|p| fs::symlink_metadata(p).is_ok()))),
        }
    }

    fn wants_write(&mut self, target: &Path) -> Result<bool, ArchiveError> {
        let metadata = match fs::symlink_metadata(target) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(true),
        };
        match self.options.overwrite {
            Overwrite::Replace => {
                // a link is replaced (see `replace_file`), a real folder is in the way
                if metadata.is_dir() {
                    return Err(ArchiveError::Exists(target.to_path_buf()));
                }
                Ok(true)
            }
            Overwrite::Skip => {
                self.out.skipped += 1;
                Ok(false)
            }
            Overwrite::Fail => Err(ArchiveError::Exists(target.to_path_buf())),
        }
    }

    /// Creates the folders above `relative`, refusing to pass through a symlinked
    /// folder: a planted link could send the write outside the destination. Also
    /// makes folder/file collisions an error instead of a confusing io error.
    fn ensure_parents(&self, relative: &Path) -> Result<(), ArchiveError> {
        let Some(parent) = relative.parent() else {
            return Ok(());
        };
        let mut current = self.root.clone();
        for part in parent.components() {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(ArchiveError::UnsafePath(current.display().to_string()));
                }
                Ok(_) => return Err(ArchiveError::Exists(current)),
                Err(_) => match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                },
            }
        }
        Ok(())
    }

    /// Creates a folder, replacing an existing symlink instead of writing through
    /// it (it could point anywhere).
    fn create_dir(&self, target: &Path) -> Result<(), ArchiveError> {
        match fs::symlink_metadata(target) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                fs::remove_file(target)?;
                fs::create_dir(target)?;
                Ok(())
            }
            Ok(_) => Err(ArchiveError::Exists(target.to_path_buf())),
            Err(_) => {
                fs::create_dir(target)?;
                Ok(())
            }
        }
    }
}

/// `File::create`, dropping a symlink at the target instead of writing through it
/// and clearing the read-only bit: Explorer replaces read-only files and a failed
/// extraction over one is a bad surprise.
fn replace_file(target: &Path) -> std::io::Result<File> {
    if let Ok(metadata) = fs::symlink_metadata(target) {
        if metadata.file_type().is_symlink() {
            fs::remove_file(target)?;
        } else if metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            clear_readonly(&mut permissions);
            let _ = fs::set_permissions(target, permissions);
        }
    }
    File::create(target)
}

/// Drops the read-only bit. Windows has one bit; Unix means "owner may write",
/// not "everyone may write".
fn clear_readonly(permissions: &mut fs::Permissions) {
    #[cfg(windows)]
    permissions.set_readonly(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions.mode();
        if mode & 0o200 == 0 {
            permissions.set_mode(mode | 0o200);
        }
    }
    #[cfg(not(any(windows, unix)))]
    let _ = permissions;
}

/// Selection check for a raw archive name: unsafe names always go through, so the
/// sink reports them (`extract_all`) or errors (`strict_paths`).
fn selected(selection: Option<&Selection>, raw: &str) -> bool {
    match selection {
        None => true,
        Some(selection) => sanitize_inner_path(raw).map_or(true, |clean| selection.matches(&clean)),
    }
}

fn extract_zip(path: &Path, selection: Option<&Selection>, sink: &mut Sink<'_>) -> Result<(), ArchiveError> {
    let mut zip = zip::ZipArchive::new(BufReader::new(File::open(path)?))?;
    for index in 0..zip.len() {
        if sink.cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let (name, is_dir, is_symlink) = {
            let entry = zip.by_index_data(index)?;
            (zip_name(&entry), entry.is_dir(), entry.is_symlink())
        };
        if is_symlink {
            sink.out.skipped += 1;
            continue;
        }
        if !selected(selection, &name) {
            continue;
        }
        if is_dir {
            sink.entry(&name, true, None)?;
        } else {
            let mut file = zip.by_index(index)?;
            sink.entry(&name, false, Some(&mut file))?;
        }
    }
    Ok(())
}

fn extract_tar(
    path: &Path,
    gzip: bool,
    selection: Option<&Selection>,
    sink: &mut Sink<'_>,
) -> Result<(), ArchiveError> {
    let reader = BufReader::new(File::open(path)?);
    if gzip {
        tar_extract(&mut tar::Archive::new(flate2::read::GzDecoder::new(reader)), selection, sink)
    } else {
        tar_extract(&mut tar::Archive::new(reader), selection, sink)
    }
}

fn tar_extract<R: Read>(
    tar: &mut tar::Archive<R>,
    selection: Option<&Selection>,
    sink: &mut Sink<'_>,
) -> Result<(), ArchiveError> {
    for entry in tar.entries()? {
        if sink.cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let mut entry = entry?;
        let is_dir = entry.header().entry_type().is_dir();
        if !is_dir && !entry.header().entry_type().is_file() {
            sink.out.skipped += 1;
            continue;
        }
        let name = entry.path()?.to_string_lossy().into_owned();
        if !selected(selection, &name) {
            continue;
        }
        if is_dir {
            sink.entry(&name, true, None)?;
        } else {
            sink.entry(&name, false, Some(&mut entry))?;
        }
    }
    Ok(())
}

fn extract_7z(path: &Path, selection: Option<&Selection>, sink: &mut Sink<'_>) -> Result<(), ArchiveError> {
    let mut reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())?;
    let mut error: Option<ArchiveError> = None;
    let result = reader.for_each_entries_with_decision(|entry, stream| {
        if sink.cancelled() {
            return Ok(sevenz_rust2::EntryDecision::Stop);
        }
        let is_dir = entry.is_directory();
        if !selected(selection, entry.name()) {
            // drains (or skips) the entry's bytes so the next header lines up
            return Ok(sevenz_rust2::EntryDecision::Continue);
        }
        let outcome = if is_dir {
            sink.entry(entry.name(), true, None)
        } else {
            sink.entry(entry.name(), false, Some(stream))
        };
        match outcome {
            Ok(()) => Ok(sevenz_rust2::EntryDecision::Continue),
            Err(e) => {
                error = Some(e);
                Ok(sevenz_rust2::EntryDecision::Stop)
            }
        }
    });
    if let Some(error) = error {
        return Err(error);
    }
    result?;
    Ok(())
}
