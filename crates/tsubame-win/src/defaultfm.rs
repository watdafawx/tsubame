//! "Use Tsubame instead of File Explorer for folders": the opt-in Tier 1 registration of
//! `docs/research-default-explorer-defender.md` (R1) §5.1/§5.4 and nothing else.
//!
//! ```text
//! HKCU\Software\Classes\Directory\shell                 (Default) = "Tsubame"
//! HKCU\Software\Classes\Directory\shell\Tsubame         (Default) = "Open in Tsubame", Icon, PreviousDefault
//! HKCU\Software\Classes\Directory\shell\Tsubame\command (Default) = "\"<install>\tsubame-handler.exe\" \"%1\""
//! ```
//! No `DelegateExecute`, no `HKLM`/`HKCR`, nothing under `Folder`, no CLSID. The command is
//! the fail-safe shim, never `tsubame.exe` itself. Everything goes through the [`Registry`]
//! trait so the logic is tested against [`MemRegistry`]; [`WinRegistry`] is the real thing
//! and refuses to write under `cfg(test)` or with `TSUBAME_NO_REGISTRY_WRITE` set.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The shim that is registered (next to `tsubame.exe`).
pub const SHIM_EXE: &str = "tsubame-handler.exe";
/// Name of our verb key under `Directory\shell`.
pub const VERB: &str = "Tsubame";
const SHELL: &str = r"Directory\shell";
const VERB_KEY: &str = r"Directory\shell\Tsubame";
const CMD_KEY: &str = r"Directory\shell\Tsubame\command";

/// Registry access relative to `HKCU\Software\Classes`. `name == ""` is the (Default) value.
pub trait Registry {
    fn get(&self, key: &str, name: &str) -> Option<String>;
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), String>;
    fn delete_value(&mut self, key: &str, name: &str) -> Result<(), String>;
    /// The key and everything below it.
    fn delete_tree(&mut self, key: &str) -> Result<(), String>;
    fn subkeys(&self, key: &str) -> Vec<String>;
    fn key_exists(&self, key: &str) -> bool;
    /// Tell the shell its association data changed (after a successful write).
    fn changed(&mut self) {}
}

/// Who handles "open a folder" right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Stock behaviour (no default verb set by anyone).
    Explorer,
    /// Our verb is the default and its command points at the shim.
    Tsubame,
    /// Our verb is the default but the command points somewhere else (moved/deleted install).
    TsubameStale { command: String },
    /// Another app owns the default verb.
    Other(String),
}

/// Is the feature available in this install?
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Ready { shim: PathBuf },
    /// What is missing, for the tooltip and the report.
    Missing { shim: PathBuf, what: &'static str },
}

pub fn shim_path(install_dir: &Path) -> PathBuf {
    install_dir.join(SHIM_EXE)
}

pub fn availability(install_dir: &Path) -> Availability {
    let shim = shim_path(install_dir);
    if shim.is_file() {
        Availability::Ready { shim }
    } else {
        Availability::Missing { shim, what: "tsubame-handler.exe (the fail-safe shim) is not built yet" }
    }
}

fn command_line(shim: &Path) -> String {
    format!("\"{}\" \"%1\"", shim.display())
}

fn same_path(a: &str, b: &str) -> bool {
    a.trim_matches('"').eq_ignore_ascii_case(b.trim_matches('"'))
}

/// The exe inside a registered command line (`"C:\x\y.exe" "%1"`).
fn exe_of(cmd: &str) -> &str {
    let c = cmd.trim();
    if let Some(rest) = c.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        c.split(' ').next().unwrap_or("")
    }
}

pub fn status(r: &dyn Registry, shim: &Path) -> Status {
    let def = r.get(SHELL, "").unwrap_or_default();
    let def = def.trim();
    if def.is_empty() {
        return Status::Explorer;
    }
    if !def.eq_ignore_ascii_case(VERB) {
        return Status::Other(def.to_string());
    }
    let cmd = r.get(CMD_KEY, "").unwrap_or_default();
    if same_path(exe_of(&cmd), &shim.display().to_string()) {
        Status::Tsubame
    } else {
        Status::TsubameStale { command: cmd }
    }
}

/// Register the verb. Refuses when the shim does not exist (a registration pointing at a
/// missing file is the one thing that can lock a user out) and verifies by reading back.
pub fn enable(r: &mut dyn Registry, shim: &Path, app_exe: &Path) -> Result<(), String> {
    if !shim.is_file() {
        return Err(format!("{} does not exist", shim.display()));
    }
    // keep whatever the default was, once
    if !matches!(status(r, shim), Status::Tsubame | Status::TsubameStale { .. }) {
        let prev = r.get(SHELL, "").unwrap_or_default();
        r.set(VERB_KEY, "", "Open in Tsubame")?;
        if prev.trim().is_empty() {
            let _ = r.delete_value(VERB_KEY, "PreviousDefault");
        } else {
            r.set(VERB_KEY, "PreviousDefault", prev.trim())?;
        }
    } else {
        r.set(VERB_KEY, "", "Open in Tsubame")?;
    }
    r.set(VERB_KEY, "Icon", &format!("{},0", app_exe.display()))?;
    r.set(CMD_KEY, "", &command_line(shim))?;
    // the default verb flips last: until now nothing changed for the user
    r.set(SHELL, "", VERB)?;
    r.changed();
    match status(r, shim) {
        Status::Tsubame => Ok(()),
        s => Err(format!("registration did not stick ({s:?})")),
    }
}

/// The exact inverse (R1 §5.4 steps 1–3, 5, 6). Idempotent: nothing registered is fine.
pub fn disable(r: &mut dyn Registry) -> Result<(), String> {
    let ours = r.get(SHELL, "").is_some_and(|d| d.trim().eq_ignore_ascii_case(VERB));
    if ours {
        match r.get(VERB_KEY, "PreviousDefault") {
            Some(p) if !p.trim().is_empty() => r.set(SHELL, "", &p)?,
            _ => r.delete_value(SHELL, "")?,
        }
    }
    if r.key_exists(VERB_KEY) {
        r.delete_tree(VERB_KEY)?;
    }
    // drop `Directory\shell` only when we leave it empty
    if r.key_exists(SHELL) && r.subkeys(SHELL).is_empty() && r.get(SHELL, "").is_none() {
        r.delete_tree(SHELL)?;
    }
    r.changed();
    let still_ours = r.get(SHELL, "").is_some_and(|d| d.trim().eq_ignore_ascii_case(VERB)) || r.key_exists(VERB_KEY);
    if still_ours {
        return Err("could not remove the Tsubame folder verb".to_string());
    }
    Ok(())
}

// ---- in-memory fake (tests and UI tests) ----------------------------------------------

/// A registry in a map. Keys are compared case-insensitively like the real one.
#[derive(Clone, Debug, Default)]
pub struct MemRegistry {
    /// lowercase key -> (value name lowercase -> value)
    keys: BTreeMap<String, BTreeMap<String, String>>,
    /// how many times `changed` was called (SHChangeNotify)
    pub notified: u32,
    /// make every write fail (error paths)
    pub fail_writes: bool,
}

fn norm(k: &str) -> String {
    k.trim_matches('\\').to_ascii_lowercase()
}

impl MemRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    /// Test helper: create a key with a default value.
    pub fn with(mut self, key: &str, name: &str, value: &str) -> Self {
        let _ = self.set(key, name, value);
        self
    }
    pub fn dump(&self) -> Vec<(String, String, String)> {
        self.keys.iter().flat_map(|(k, v)| v.iter().map(move |(n, val)| (k.clone(), n.clone(), val.clone()))).collect()
    }
    fn ensure_parents(&mut self, key: &str) {
        let mut acc = String::new();
        for part in key.split('\\') {
            if !acc.is_empty() {
                acc.push('\\');
            }
            acc.push_str(part);
            self.keys.entry(acc.clone()).or_default();
        }
    }
}

impl Registry for MemRegistry {
    fn get(&self, key: &str, name: &str) -> Option<String> {
        self.keys.get(&norm(key))?.get(&name.to_ascii_lowercase()).cloned()
    }
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), String> {
        if self.fail_writes {
            return Err("access denied".into());
        }
        let k = norm(key);
        self.ensure_parents(&k);
        self.keys.entry(k).or_default().insert(name.to_ascii_lowercase(), value.to_string());
        Ok(())
    }
    fn delete_value(&mut self, key: &str, name: &str) -> Result<(), String> {
        if self.fail_writes {
            return Err("access denied".into());
        }
        if let Some(v) = self.keys.get_mut(&norm(key)) {
            v.remove(&name.to_ascii_lowercase());
        }
        Ok(())
    }
    fn delete_tree(&mut self, key: &str) -> Result<(), String> {
        if self.fail_writes {
            return Err("access denied".into());
        }
        let k = norm(key);
        let prefix = format!("{k}\\");
        self.keys.retain(|name, _| *name != k && !name.starts_with(&prefix));
        Ok(())
    }
    fn subkeys(&self, key: &str) -> Vec<String> {
        let prefix = format!("{}\\", norm(key));
        self.keys.keys().filter_map(|k| k.strip_prefix(&prefix)).filter(|rest| !rest.contains('\\')).map(str::to_string).collect()
    }
    fn key_exists(&self, key: &str) -> bool {
        self.keys.contains_key(&norm(key))
    }
    fn changed(&mut self) {
        self.notified += 1;
    }
}

// ---- the real registry ------------------------------------------------------------------

/// `HKCU\Software\Classes` through the Win32 API. Only the opt-in Settings button may
/// call writes on it.
pub struct WinRegistry;

impl WinRegistry {
    fn writes_allowed() -> Result<(), String> {
        if cfg!(test) {
            return Err("registry writes are disabled in tests".into());
        }
        if std::env::var_os("TSUBAME_NO_REGISTRY_WRITE").is_some() {
            return Err("registry writes are disabled (TSUBAME_NO_REGISTRY_WRITE)".into());
        }
        Ok(())
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::*;

    fn full(key: &str) -> Vec<u16> {
        crate::wide(&format!(r"Software\Classes\{key}"))
    }

    fn ok(e: WIN32_ERROR) -> Result<(), String> {
        if e == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("registry error {}", e.0))
        }
    }

    fn open(key: &str, sam: REG_SAM_FLAGS) -> Option<HKEY> {
        let k = full(key);
        let mut h = HKEY::default();
        let e = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), None, sam, &mut h) };
        (e == ERROR_SUCCESS).then_some(h)
    }

    pub fn get(key: &str, name: &str) -> Option<String> {
        let h = open(key, KEY_READ)?;
        let n = crate::wide(name);
        let mut ty = REG_VALUE_TYPE(0);
        let mut len = 0u32;
        let mut out = None;
        unsafe {
            if RegQueryValueExW(h, PCWSTR(n.as_ptr()), None, Some(&mut ty), None, Some(&mut len)) == ERROR_SUCCESS && (ty == REG_SZ || ty == REG_EXPAND_SZ) {
                let mut buf = vec![0u8; len as usize + 2];
                let mut len2 = buf.len() as u32;
                if RegQueryValueExW(h, PCWSTR(n.as_ptr()), None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len2)) == ERROR_SUCCESS {
                    let w: Vec<u16> = buf[..len2 as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    out = Some(String::from_utf16_lossy(&w).trim_end_matches('\0').to_string());
                }
            }
            let _ = RegCloseKey(h);
        }
        out
    }

    pub fn set(key: &str, name: &str, value: &str) -> Result<(), String> {
        let k = full(key);
        let mut h = HKEY::default();
        unsafe {
            ok(RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut h, None))?;
            let n = crate::wide(name);
            let v: Vec<u8> = crate::wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
            let r = ok(RegSetValueExW(h, PCWSTR(n.as_ptr()), None, REG_SZ, Some(&v)));
            let _ = RegCloseKey(h);
            r
        }
    }

    pub fn delete_value(key: &str, name: &str) -> Result<(), String> {
        let Some(h) = open(key, KEY_WRITE) else { return Ok(()) };
        let n = crate::wide(name);
        let e = unsafe { RegDeleteValueW(h, PCWSTR(n.as_ptr())) };
        unsafe {
            let _ = RegCloseKey(h);
        }
        // "not found" is the state we wanted
        if e == ERROR_SUCCESS || e.0 == 2 {
            Ok(())
        } else {
            ok(e)
        }
    }

    pub fn delete_tree(key: &str) -> Result<(), String> {
        let k = full(key);
        let e = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr())) };
        if e == ERROR_SUCCESS || e.0 == 2 {
            Ok(())
        } else {
            ok(e)
        }
    }

    pub fn subkeys(key: &str) -> Vec<String> {
        let Some(h) = open(key, KEY_READ) else { return Vec::new() };
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            let e = unsafe { RegEnumKeyExW(h, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
            if e != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
            i += 1;
        }
        unsafe {
            let _ = RegCloseKey(h);
        }
        out
    }

    pub fn key_exists(key: &str) -> bool {
        match open(key, KEY_READ) {
            Some(h) => {
                unsafe {
                    let _ = RegCloseKey(h);
                }
                true
            }
            None => false,
        }
    }

    pub fn assoc_changed() {
        use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_FLUSH, SHCNF_IDLIST};
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST | SHCNF_FLUSH, None, None) };
    }
}

impl Registry for WinRegistry {
    fn get(&self, key: &str, name: &str) -> Option<String> {
        win::get(key, name)
    }
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        win::set(key, name, value)
    }
    fn delete_value(&mut self, key: &str, name: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        win::delete_value(key, name)
    }
    fn delete_tree(&mut self, key: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        win::delete_tree(key)
    }
    fn subkeys(&self, key: &str) -> Vec<String> {
        win::subkeys(key)
    }
    fn key_exists(&self, key: &str) -> bool {
        win::key_exists(key)
    }
    fn changed(&mut self) {
        if Self::writes_allowed().is_ok() {
            win::assoc_changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_shim() -> (tempdir_lite::Dir, PathBuf, PathBuf) {
        let d = tempdir_lite::Dir::new();
        let shim = d.path().join(SHIM_EXE);
        std::fs::write(&shim, b"x").unwrap();
        let app = d.path().join("tsubame.exe");
        (d, shim, app)
    }

    /// minimal scratch dir without a new dependency
    mod tempdir_lite {
        use std::path::{Path, PathBuf};
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new() -> Dir {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let p = std::env::temp_dir().join(format!("tsubame-defaultfm-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn fresh_registry_is_explorer() {
        let (_d, shim, _) = tmp_shim();
        assert_eq!(status(&MemRegistry::new(), &shim), Status::Explorer);
    }

    #[test]
    fn enable_writes_exactly_the_tier_one_keys_and_verifies() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new();
        enable(&mut r, &shim, &app).unwrap();
        assert_eq!(status(&r, &shim), Status::Tsubame);
        assert_eq!(r.get(SHELL, ""), Some("Tsubame".into()));
        assert_eq!(r.get(VERB_KEY, ""), Some("Open in Tsubame".into()));
        let cmd = r.get(CMD_KEY, "").unwrap();
        assert_eq!(cmd, format!("\"{}\" \"%1\"", shim.display()));
        assert!(!cmd.to_lowercase().contains("tsubame.exe"), "the shim is registered, not the app");
        assert_eq!(r.notified, 1);
        // nothing outside Directory\shell, no DelegateExecute anywhere
        for (k, n, _) in r.dump() {
            assert!(k.starts_with("directory\\shell") || k == "directory", "{k}");
            assert_ne!(n, "delegateexecute");
            assert_ne!(n, "dropTarget".to_ascii_lowercase());
        }
    }

    #[test]
    fn enable_refuses_without_the_shim() {
        let d = tempdir_lite::Dir::new();
        let shim = d.path().join(SHIM_EXE);
        let mut r = MemRegistry::new();
        let e = enable(&mut r, &shim, &d.path().join("tsubame.exe")).unwrap_err();
        assert!(e.contains("does not exist"), "{e}");
        assert!(r.dump().is_empty(), "nothing written");
    }

    #[test]
    fn disable_restores_the_previous_default() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new().with(SHELL, "", "OpenWithOther");
        enable(&mut r, &shim, &app).unwrap();
        assert_eq!(r.get(VERB_KEY, "PreviousDefault"), Some("OpenWithOther".into()));
        disable(&mut r).unwrap();
        assert_eq!(r.get(SHELL, ""), Some("OpenWithOther".into()));
        assert!(!r.key_exists(VERB_KEY));
        assert_eq!(status(&r, &shim), Status::Other("OpenWithOther".into()));
    }

    #[test]
    fn disable_without_a_previous_default_leaves_no_trace() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new();
        enable(&mut r, &shim, &app).unwrap();
        disable(&mut r).unwrap();
        assert!(!r.key_exists(VERB_KEY));
        assert!(!r.key_exists(SHELL), "empty Directory\\shell removed");
        assert_eq!(status(&r, &shim), Status::Explorer);
    }

    #[test]
    fn disable_keeps_other_apps_verbs() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new().with(r"Directory\shell\Other\command", "", "other.exe");
        enable(&mut r, &shim, &app).unwrap();
        disable(&mut r).unwrap();
        assert!(r.key_exists(r"Directory\shell\Other\command"));
        assert!(r.key_exists(SHELL));
    }

    #[test]
    fn disable_is_idempotent_and_touches_nothing_when_not_ours() {
        let mut r = MemRegistry::new().with(SHELL, "", "Foreign");
        disable(&mut r).unwrap();
        disable(&mut r).unwrap();
        assert_eq!(r.get(SHELL, ""), Some("Foreign".into()), "another app's default is never cleared");
    }

    #[test]
    fn enable_twice_keeps_the_original_previous_default() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new().with(SHELL, "", "Prev");
        enable(&mut r, &shim, &app).unwrap();
        enable(&mut r, &shim, &app).unwrap();
        assert_eq!(r.get(VERB_KEY, "PreviousDefault"), Some("Prev".into()), "must not remember ourselves");
        disable(&mut r).unwrap();
        assert_eq!(r.get(SHELL, ""), Some("Prev".into()));
    }

    #[test]
    fn stale_command_is_reported_and_removable() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new();
        enable(&mut r, &shim, &app).unwrap();
        // the install moved
        let moved = shim.with_file_name("elsewhere").join(SHIM_EXE);
        assert!(matches!(status(&r, &moved), Status::TsubameStale { .. }));
        disable(&mut r).unwrap();
        assert_eq!(status(&r, &moved), Status::Explorer);
    }

    #[test]
    fn another_app_taking_over_shows_as_other_and_is_not_reasserted() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new();
        enable(&mut r, &shim, &app).unwrap();
        r.set(SHELL, "", "Files").unwrap();
        assert_eq!(status(&r, &shim), Status::Other("Files".into()));
    }

    #[test]
    fn failed_writes_report_an_error() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new();
        r.fail_writes = true;
        assert!(enable(&mut r, &shim, &app).is_err());
    }

    #[test]
    fn availability_follows_the_shim_file() {
        let d = tempdir_lite::Dir::new();
        assert!(matches!(availability(d.path()), Availability::Missing { .. }));
        std::fs::write(d.path().join(SHIM_EXE), b"x").unwrap();
        assert!(matches!(availability(d.path()), Availability::Ready { .. }));
    }

    #[test]
    fn the_real_registry_refuses_writes_in_tests() {
        let mut r = WinRegistry;
        assert!(r.set(r"Directory\shell\TsubameTestMustNotExist", "", "x").is_err());
        assert!(r.delete_tree(r"Directory\shell\TsubameTestMustNotExist").is_err());
        // reads are fine and see nothing
        assert!(!r.key_exists(r"Directory\shell\TsubameTestMustNotExist"));
    }
}
