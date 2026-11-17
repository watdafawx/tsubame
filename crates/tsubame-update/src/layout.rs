//! Where everything lives.
//!
//! ```text
//! <install_dir>\                      %LOCALAPPDATA%\Programs\Tsubame (installed) or wherever the zip was unpacked
//!     tsubame.exe                     the running app
//!     tsubame.exe.old                 previous version (one step of rollback), kept until the next update
//!     tsubame.exe.new / .bad          transient: exist only during apply / after a rollback
//!     portable.txt                    marker: data lives in <install_dir>\data
//! <data_dir>\                         %LOCALAPPDATA%\Tsubame (or <install_dir>\data when portable)
//!     update\state.json               last check, ETag, installed/previous version, trial boot counter
//!     update\cache\                   last verified manifest + signature
//!     update\staging\<version>\       downloaded zip + extracted files, deleted after apply
//! ```
//! Never `%TEMP%`: it is world-writable-ish and the first place malware looks.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

pub const PORTABLE_MARKER: &str = "portable.txt";

#[derive(Clone, Debug)]
pub struct Layout {
    pub install_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Layout {
    pub fn new(install_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Layout {
        Layout { install_dir: install_dir.into(), data_dir: data_dir.into() }
    }

    /// Layout for an app installed in `install_dir`: portable marker -> `<install_dir>\data`,
    /// else `%LOCALAPPDATA%\Tsubame`.
    pub fn for_install_dir(install_dir: &Path) -> Result<Layout> {
        let data = if install_dir.join(PORTABLE_MARKER).is_file() {
            install_dir.join("data")
        } else {
            local_app_data()?.join("Tsubame")
        };
        Ok(Layout::new(install_dir, data))
    }

    /// The directory of the running exe.
    pub fn of_current_exe() -> Result<Layout> {
        let exe = std::env::current_exe().map_err(|e| Error::Config(format!("current_exe: {e}")))?;
        let dir = exe.parent().ok_or_else(|| Error::Config("exe has no parent dir".into()))?;
        Layout::for_install_dir(dir)
    }

    /// The default per-user install (`%LOCALAPPDATA%\Programs\Tsubame`), for the CLI.
    pub fn default_per_user() -> Result<Layout> {
        Layout::for_install_dir(&local_app_data()?.join("Programs").join("Tsubame"))
    }

    pub fn update_dir(&self) -> PathBuf {
        self.data_dir.join("update")
    }
    pub fn state_file(&self) -> PathBuf {
        self.update_dir().join("state.json")
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.update_dir().join("cache")
    }
    pub fn staging_root(&self) -> PathBuf {
        self.update_dir().join("staging")
    }
    pub fn staging_dir(&self, version: &str) -> PathBuf {
        self.staging_root().join(version)
    }
    pub fn staged_files_dir(&self, version: &str) -> PathBuf {
        self.staging_dir(version).join("files")
    }
    pub fn staged_json(&self, version: &str) -> PathBuf {
        self.staging_dir(version).join("staged.json")
    }
    pub fn exe(&self, name: &str) -> PathBuf {
        self.install_dir.join(name)
    }
    pub fn old(&self, name: &str) -> PathBuf {
        self.install_dir.join(format!("{name}.old"))
    }
    pub fn new_(&self, name: &str) -> PathBuf {
        self.install_dir.join(format!("{name}.new"))
    }
    pub fn bad(&self, name: &str) -> PathBuf {
        self.install_dir.join(format!("{name}.bad"))
    }
}

pub fn local_app_data() -> Result<PathBuf> {
    match std::env::var_os("LOCALAPPDATA") {
        Some(p) if !p.is_empty() => Ok(PathBuf::from(p)),
        _ => Err(Error::Config("%LOCALAPPDATA% is not set".into())),
    }
}

/// Write `bytes` to `path` atomically (temp file in the same dir + rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_paths_are_under_update_dir_and_never_temp() {
        let l = Layout::new(r"C:\P\Tsubame", r"C:\Users\u\AppData\Local\Tsubame");
        assert_eq!(l.state_file(), PathBuf::from(r"C:\Users\u\AppData\Local\Tsubame\update\state.json"));
        assert_eq!(l.staged_files_dir("0.2.0"), PathBuf::from(r"C:\Users\u\AppData\Local\Tsubame\update\staging\0.2.0\files"));
        assert!(l.staging_dir("0.2.0").starts_with(l.update_dir()));
        assert!(!l.staging_dir("0.2.0").to_string_lossy().to_ascii_lowercase().contains("\\temp"));
        assert_eq!(l.old("tsubame.exe"), PathBuf::from(r"C:\P\Tsubame\tsubame.exe.old"));
        assert_eq!(l.new_("tsubame.exe"), PathBuf::from(r"C:\P\Tsubame\tsubame.exe.new"));
        assert_eq!(l.bad("tsubame.exe"), PathBuf::from(r"C:\P\Tsubame\tsubame.exe.bad"));
    }

    #[test]
    fn portable_marker_moves_data_next_to_exe() {
        let d = std::env::temp_dir().join(format!("tsu-layout-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let normal = Layout::for_install_dir(&d).unwrap();
        assert!(normal.data_dir.ends_with("Tsubame"));
        std::fs::write(d.join(PORTABLE_MARKER), b"").unwrap();
        let p = Layout::for_install_dir(&d).unwrap();
        assert_eq!(p.data_dir, d.join("data"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn atomic_write_replaces() {
        let d = std::env::temp_dir().join(format!("tsu-atomic-{}", std::process::id()));
        let f = d.join("sub").join("x.json");
        write_atomic(&f, b"one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        assert!(!d.join("sub").join("x.json.tmp").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
