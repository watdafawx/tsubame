//! File system backend: std::fs copy/move, Recycle Bin through `IFileOperation`.
use super::{Cancellation, OpBackend, Operation, Step, recycle};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    io::{self, Read, Write},
    path::Path,
};

#[derive(Default)]
pub struct FsBackend;

thread_local! {
    /// Drive letter (lowercase) to "has a usable Recycle Bin". Volumes rarely change.
    static BIN_CACHE: RefCell<HashMap<char, bool>> = RefCell::new(HashMap::new());
}

fn drive_of(path: &Path) -> Option<char> {
    let abs = std::path::absolute(path).ok()?;
    let s = abs.to_string_lossy().into_owned();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let mut c = s.chars();
    let letter = c.next()?;
    (c.next() == Some(':')).then(|| letter.to_ascii_lowercase())
}

/// Cached `recycle::has_recycle_bin` per drive letter.
fn bin_available(path: &Path) -> bool {
    let Some(d) = drive_of(path) else {
        return false;
    };
    BIN_CACHE.with(|c| {
        *c.borrow_mut()
            .entry(d)
            .or_insert_with(|| recycle::has_recycle_bin(path))
    })
}

/// How to undo the creation of `path`: recycle it (recoverable) where a bin exists, else
/// delete it for good. Created items are ours, so the fallback loses nothing of the user's.
fn undo_create(path: &Path) -> Operation {
    Operation::Delete {
        path: path.to_path_buf(),
        permanent: !bin_available(path),
    }
}

fn remove(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn copy(
    source: &Path,
    target: &Path,
    cancel: &Cancellation,
    progress: &mut dyn FnMut(Step),
    created: &mut bool,
) -> io::Result<()> {
    cancel.check()?;
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "reparse points require shell backend",
        ));
    }
    if meta.is_dir() {
        fs::create_dir(target)?;
        *created = true;
        for item in fs::read_dir(source)? {
            let item = item?;
            copy(
                &item.path(),
                &target.join(item.file_name()),
                cancel,
                progress,
                &mut false,
            )?;
        }
    } else {
        let mut input = fs::File::open(source)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        *created = true;
        let mut buffer = vec![0; 256 * 1024];
        loop {
            cancel.check()?;
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            progress(Step {
                bytes: n as u64,
                files: 0,
            });
        }
        output.flush()?;
        progress(Step { bytes: 0, files: 1 });
    }
    Ok(())
}

fn validate(source: &Path, target: &Path) -> io::Result<()> {
    let source = fs::canonicalize(source)?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("target needs parent"))?;
    let target = fs::canonicalize(parent)?.join(
        target
            .file_name()
            .ok_or_else(|| io::Error::other("target needs name"))?,
    );
    // Windows comparisons are case insensitive; preserve spelling for case-only rename.
    let s = source.to_string_lossy().to_lowercase();
    let t = target.to_string_lossy().to_lowercase();
    if s == t || (source.is_dir() && Path::new(&t).starts_with(Path::new(&s))) {
        return Err(io::Error::other(
            "source equals target or target is inside source",
        ));
    }
    Ok(())
}

impl OpBackend for FsBackend {
    fn execute(
        &mut self,
        op: &Operation,
        cancel: &Cancellation,
        progress: &mut dyn FnMut(Step),
    ) -> io::Result<Option<Operation>> {
        cancel.check()?;
        match op {
            Operation::Copy { source, target }
            | Operation::Move { source, target }
            | Operation::Rename { source, target } => {
                let case_rename = matches!(op, Operation::Rename { .. })
                    && source != target
                    && source
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&target.to_string_lossy());
                if !case_rename {
                    validate(source, target)?;
                    if target.exists() {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "target exists",
                        ));
                    }
                }
                if !matches!(op, Operation::Copy { .. }) {
                    match fs::rename(source, target) {
                        Ok(()) => {
                            return Ok(Some(Operation::Move {
                                source: target.clone(),
                                target: source.clone(),
                            }));
                        }
                        Err(e) if e.raw_os_error() == Some(17) => {}
                        Err(e) => return Err(e),
                    }
                }
                if target.exists() && !case_rename {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "target exists",
                    ));
                }
                let mut created = false;
                if let Err(e) = copy(source, target, cancel, progress, &mut created) {
                    if created {
                        let _ = remove(target);
                    }
                    return Err(e);
                }
                if !matches!(op, Operation::Copy { .. }) {
                    remove(source)?;
                    return Ok(Some(Operation::Move {
                        source: target.clone(),
                        target: source.clone(),
                    }));
                }
                Ok(Some(undo_create(target)))
            }
            Operation::Delete {
                path,
                permanent: true,
            } => {
                remove(path)?;
                Ok(None)
            }
            Operation::Delete {
                path,
                permanent: false,
            } => {
                let item = recycle::recycle(path)?;
                Ok(Some(Operation::Restore(item)))
            }
            Operation::Restore(item) => {
                recycle::restore(item)?;
                Ok(Some(Operation::Delete {
                    path: item.original.clone(),
                    permanent: false,
                }))
            }
            Operation::NewFolder(path) => {
                fs::create_dir(path)?;
                Ok(Some(undo_create(path)))
            }
            Operation::NewFile(path) => {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                Ok(Some(undo_create(path)))
            }
            // The queue runs sequences step by step; a backend never sees one.
            Operation::Sequence(_) => Err(io::Error::other("sequence reached the backend")),
        }
    }
}
