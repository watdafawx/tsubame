//! Where the config and session files live.
//!
//! Normal mode: `%APPDATA%\Tsubame\config.toml` and `...\session.toml`.
//! Portable mode: the marker file `tsubame.portable` (empty) next to the exe
//! switches the data directory to `<exe dir>\data\`; `--portable` forces it and
//! `TSUBAME_DATA_DIR` overrides the directory outright (tests use it, so they
//! never touch the real profile).
//!
//! No Win32: `%APPDATA%` comes from the environment, which is what the shell
//! gives every process.

use std::path::{Path, PathBuf};

pub const CONFIG_NAME: &str = "config.toml";
pub const SESSION_NAME: &str = "session.toml";
/// Empty marker file next to the exe.
pub const PORTABLE_MARKER: &str = "tsubame.portable";
/// The updater's marker (`docs/updater.md`); accepted too so one marker file is enough.
pub const PORTABLE_MARKER_LEGACY: &str = "portable.txt";
pub const DATA_ENV: &str = "TSUBAME_DATA_DIR";

/// Everything the location depends on, so it can be tested without the real
/// profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocateOptions {
    /// `%APPDATA%`
    pub appdata: Option<PathBuf>,
    /// the folder of the running exe
    pub exe_dir: Option<PathBuf>,
    /// `--portable` on the command line
    pub portable: bool,
    /// `TSUBAME_DATA_DIR`
    pub data_dir: Option<PathBuf>,
    /// `--config <file>`
    pub config_file: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    pub data_dir: PathBuf,
    pub config: PathBuf,
    pub session: PathBuf,
    pub portable: bool,
}

impl Locations {
    /// Both files of a data directory, for tests and the app.
    pub fn in_dir(dir: &Path) -> Locations {
        Locations { data_dir: dir.to_path_buf(), config: dir.join(CONFIG_NAME), session: dir.join(SESSION_NAME), portable: false }
    }
}

pub fn locate(opts: &LocateOptions) -> Locations {
    let portable_marker = opts.exe_dir.as_ref().is_some_and(|d| d.join(PORTABLE_MARKER).exists() || d.join(PORTABLE_MARKER_LEGACY).exists());
    let portable = portable_marker || opts.portable;
    let data_dir = if let Some(d) = &opts.data_dir {
        d.clone()
    } else if portable {
        match &opts.exe_dir {
            Some(d) => d.join("data"),
            None => PathBuf::from("data"),
        }
    } else {
        opts.appdata.clone().unwrap_or_else(|| PathBuf::from(".")).join("Tsubame")
    };
    Locations {
        config: opts.config_file.clone().unwrap_or_else(|| data_dir.join(CONFIG_NAME)),
        session: data_dir.join(SESSION_NAME),
        data_dir,
        portable,
    }
}

/// `locate` from the real environment (used by the app, never by tests).
pub fn locate_now(portable: bool, config_file: Option<PathBuf>) -> Locations {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| std::env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("AppData").join("Roaming")));
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    locate(&LocateOptions { appdata, exe_dir, portable, data_dir: std::env::var_os(DATA_ENV).map(PathBuf::from), config_file })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-config-locate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn normal_mode_uses_appdata() {
        let appdata = PathBuf::from("C:\\Users\\me\\AppData\\Roaming");
        let l = locate(&LocateOptions { appdata: Some(appdata.clone()), exe_dir: Some(PathBuf::from("C:\\Apps\\Tsubame")), ..Default::default() });
        assert!(!l.portable);
        assert_eq!(l.data_dir, appdata.join("Tsubame"));
        assert_eq!(l.config, appdata.join("Tsubame").join("config.toml"));
        assert_eq!(l.session, appdata.join("Tsubame").join("session.toml"));
    }

    #[test]
    fn the_marker_file_switches_to_portable() {
        let exe = dir("portable");
        std::fs::write(exe.join(PORTABLE_MARKER), b"").unwrap();
        let l = locate(&LocateOptions { appdata: Some(PathBuf::from("C:\\ad")), exe_dir: Some(exe.clone()), ..Default::default() });
        assert!(l.portable);
        assert_eq!(l.data_dir, exe.join("data"));
        assert_eq!(l.config, exe.join("data").join("config.toml"));
        let _ = std::fs::remove_dir_all(&exe);
    }

    #[test]
    fn the_legacy_marker_works_too() {
        let exe = dir("legacy");
        std::fs::write(exe.join(PORTABLE_MARKER_LEGACY), b"").unwrap();
        let l = locate(&LocateOptions { exe_dir: Some(exe.clone()), ..Default::default() });
        assert!(l.portable && l.data_dir == exe.join("data"));
        let _ = std::fs::remove_dir_all(&exe);
    }

    #[test]
    fn the_flag_forces_portable_and_the_env_overrides_everything() {
        let exe = dir("flag");
        let l = locate(&LocateOptions { portable: true, exe_dir: Some(exe.clone()), appdata: Some(PathBuf::from("C:\\ad")), ..Default::default() });
        assert!(l.portable && l.data_dir == exe.join("data"));
        let override_dir = dir("env");
        let l = locate(&LocateOptions { portable: true, data_dir: Some(override_dir.clone()), exe_dir: Some(exe.clone()), ..Default::default() });
        assert_eq!(l.data_dir, override_dir);
        assert_eq!(l.session, override_dir.join("session.toml"));
        let _ = std::fs::remove_dir_all(&exe);
        let _ = std::fs::remove_dir_all(&override_dir);
    }

    #[test]
    fn config_file_override_keeps_the_session_beside_the_data_dir() {
        let l = locate(&LocateOptions { appdata: Some(PathBuf::from("C:\\ad")), config_file: Some(PathBuf::from("D:\\mine\\cfg.toml")), ..Default::default() });
        assert_eq!(l.config, PathBuf::from("D:\\mine\\cfg.toml"));
        assert_eq!(l.session, PathBuf::from("C:\\ad\\Tsubame\\session.toml"));
    }
}
