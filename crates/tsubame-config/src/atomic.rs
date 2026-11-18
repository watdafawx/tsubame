//! Atomic file replace, shared by the config and session writers.
//!
//! The temp file lives in the same directory as the target (so the replace is
//! a same-volume rename) and is flushed to disk before the swap. On Windows
//! `std::fs::rename` is `MoveFileExW(.., MOVEFILE_REPLACE_EXISTING)`, i.e. the
//! replace itself is atomic: readers see the old file or the new one, never a
//! half-written one.
//!
//! Failure is safe by construction: if anything before the rename fails, the
//! old file is untouched and the temp file is removed.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Writes `bytes` to `path` through a flushed temp file and an atomic rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, |_, _| Ok(()))
}

/// The full sequence with a hook that runs after the temp file is flushed and
/// before the replace. Tests use it to simulate a failure; the production path
/// passes a no-op.
pub fn write_atomic_with(path: &Path, bytes: &[u8], before_replace: impl FnOnce(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = temp_path(path);
    let result = (|| {
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.flush()?;
            // the bytes are on disk before the name changes
            f.sync_all()?;
        }
        before_replace(&tmp, path)?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `config.toml` -> `config.toml.tmp` in the same directory. A leftover temp
/// file from a crashed process is simply overwritten.
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// `config.toml` -> `config.toml.bad`: where a corrupt file is moved.
pub fn corrupt_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".bad");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-config-atomic-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn replaces_the_old_file() {
        let d = temp_dir("replace");
        let p = d.join("config.toml");
        fs::write(&p, b"old").unwrap();
        write_atomic(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        assert!(!d.join("config.toml.tmp").exists(), "temp file removed");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn creates_the_directory_and_the_file() {
        let d = temp_dir("create");
        let p = d.join("sub").join("deep").join("session.toml");
        write_atomic(&p, b"x = 1\n").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"x = 1\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failure_before_the_replace_keeps_the_old_file() {
        let d = temp_dir("fail");
        let p = d.join("config.toml");
        fs::write(&p, b"old contents\n").unwrap();
        let e = write_atomic_with(&p, b"new contents\n", |tmp, _| {
            assert!(tmp.exists(), "the temp file is written and flushed first");
            Err(io::Error::other("simulated failure"))
        });
        assert!(e.is_err());
        assert_eq!(fs::read(&p).unwrap(), b"old contents\n", "old file intact");
        assert!(!d.join("config.toml.tmp").exists(), "temp file cleaned up");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failure_of_the_replace_itself_keeps_the_old_file() {
        let d = temp_dir("replace-fail");
        // the target is a directory: renaming a file over it must fail
        let p = d.join("config.toml");
        fs::create_dir(&p).unwrap();
        fs::write(p.join("keep"), b"x").unwrap();
        assert!(write_atomic(&p, b"new").is_err());
        assert!(p.is_dir() && p.join("keep").exists(), "nothing was replaced");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_backup_name() {
        assert_eq!(corrupt_path(Path::new("C:\\a\\config.toml")).to_string_lossy(), "C:\\a\\config.toml.bad");
        assert_eq!(corrupt_path(Path::new("session.toml")).to_string_lossy(), "session.toml.bad");
    }
}
