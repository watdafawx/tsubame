//! The registry side of "Use Tsubame instead of File Explorer for folders": the opt-in Tier 1
//! registration of `docs/research-default-explorer-defender.md` (R1) section 5.1/5.4 and
//! nothing else. One implementation, used by the app's Settings page (register, repair,
//! remove) and by the shim itself (remove when the app is gone), so the two can never
//! disagree about which keys are ours.
//!
//! ```text
//! HKCU\Software\Classes\Directory\shell                 (Default) = "Tsubame"
//! HKCU\Software\Classes\Directory\shell\Tsubame         (Default) = "Open in Tsubame", Icon, PreviousDefault
//! HKCU\Software\Classes\Directory\shell\Tsubame\command (Default) = "\"<install>\tsubame-handler.exe\" \"%1\""
//! ```
//! No `DelegateExecute`, no `HKLM`/`HKCR`, nothing under `Folder`, no CLSID. The command is
//! the fail-safe shim, never `tsubame.exe` itself. Everything goes through the [`Registry`]
//! trait so the logic is tested against [`MemRegistry`]; the real registry lives in
//! [`crate::sys`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The shim that is registered (next to `tsubame.exe`).
pub const SHIM_EXE: &str = "tsubame-handler.exe";
/// Name of our verb key under `Directory\shell`.
pub const VERB: &str = "Tsubame";
pub const SHELL: &str = r"Directory\shell";
pub const VERB_KEY: &str = r"Directory\shell\Tsubame";
pub const CMD_KEY: &str = r"Directory\shell\Tsubame\command";

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
    /// Our verb is the default and its command points at the shim, which exists.
    Tsubame,
    /// Our verb is the default but its command points somewhere else, or at a shim file that
    /// is gone (moved/deleted install). Folder opens would end up in the shim's fallback.
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
        Availability::Missing { shim, what: "tsubame-handler.exe was not found next to tsubame.exe" }
    }
}

/// The registered command: boring and absolute (R1 section 2.3 rule 4).
pub fn command_line(shim: &Path) -> String {
    format!("\"{}\" \"%1\"", shim.display())
}

/// Compare two exe paths the way Windows does: case-insensitive, `/` = `\`, quotes and a
/// `\\?\` prefix ignored.
pub fn same_path(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.trim().trim_matches('"');
        let s = s.strip_prefix(r"\\?\").unwrap_or(s);
        s.replace('/', "\\").to_ascii_lowercase()
    }
    norm(a) == norm(b)
}

/// The exe inside a registered command line (`"C:\x\y.exe" "%1"`).
pub fn exe_of(cmd: &str) -> &str {
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
    if same_path(exe_of(&cmd), &shim.display().to_string()) && shim.is_file() {
        Status::Tsubame
    } else {
        Status::TsubameStale { command: cmd }
    }
}

/// Register the verb. Refuses when the shim does not exist (a registration pointing at a
/// missing file is the one thing that can lock a user out) and verifies by reading back.
/// Also the repair: on a stale registration it rewrites the command and keeps the original
/// `PreviousDefault`.
pub fn enable(r: &mut dyn Registry, shim: &Path, app_exe: &Path) -> Result<(), String> {
    if !shim.is_file() {
        return Err(format!("{} does not exist", shim.display()));
    }
    // keep whatever the default was, once (never remember ourselves)
    let default_is_ours = r.get(SHELL, "").is_some_and(|d| d.trim().eq_ignore_ascii_case(VERB));
    if !default_is_ours {
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

/// Repair a stale registration (our verb is the default but points at a moved or deleted
/// shim): rewrite it for this install. Anything else is not a repair and is refused, so the
/// button can never take over from another app.
pub fn repair(r: &mut dyn Registry, shim: &Path, app_exe: &Path) -> Result<(), String> {
    match status(r, shim) {
        Status::TsubameStale { .. } => enable(r, shim, app_exe),
        Status::Tsubame => Ok(()),
        s => Err(format!("nothing to repair ({s:?})")),
    }
}

/// The exact inverse (R1 section 5.4 steps 1-3, 5, 6). Idempotent: nothing registered is
/// fine. Never touches another app's default verb or verbs.
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

/// What `remove_if_ours` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Removal {
    /// No Tsubame verb at all.
    NothingRegistered,
    /// The verb exists but its command is another install's shim: left alone.
    NotOurs { command: String },
    Removed,
}

/// What the shim does when the app is gone: remove the registration, but only when the
/// registered command is this very shim (or empty, i.e. a half-written registration). A second
/// install's registration is none of this copy's business.
pub fn remove_if_ours(r: &mut dyn Registry, shim: &Path) -> Result<Removal, String> {
    if !r.key_exists(VERB_KEY) {
        return Ok(Removal::NothingRegistered);
    }
    let cmd = r.get(CMD_KEY, "").unwrap_or_default();
    if !cmd.trim().is_empty() && !same_path(exe_of(&cmd), &shim.display().to_string()) {
        return Ok(Removal::NotOurs { command: cmd });
    }
    disable(r)?;
    Ok(Removal::Removed)
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
    /// Test helper: create a key with a value.
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

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    /// Minimal scratch dir without a new dependency.
    pub struct Dir(PathBuf);
    impl Dir {
        pub fn new() -> Dir {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!("tsubame-handler-test-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
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

#[cfg(test)]
mod tests {
    use super::testutil::Dir;
    use super::*;

    fn tmp_shim() -> (Dir, PathBuf, PathBuf) {
        let d = Dir::new();
        let shim = d.path().join(SHIM_EXE);
        std::fs::write(&shim, b"x").unwrap();
        let app = d.path().join("tsubame.exe");
        (d, shim, app)
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
            assert_ne!(n, "droptarget");
        }
    }

    #[test]
    fn enable_refuses_without_the_shim() {
        let d = Dir::new();
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
    fn enable_is_idempotent_and_reads_back_what_it_wrote() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new();
        enable(&mut r, &shim, &app).unwrap();
        let first = r.dump();
        enable(&mut r, &shim, &app).unwrap();
        assert_eq!(r.dump(), first, "same keys, same values the second time");
        assert_eq!(r.get(CMD_KEY, ""), Some(command_line(&shim)));
        assert_eq!(r.get(VERB_KEY, "Icon"), Some(format!("{},0", app.display())));
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
    fn a_deleted_shim_makes_the_registration_stale() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new();
        enable(&mut r, &shim, &app).unwrap();
        std::fs::remove_file(&shim).unwrap();
        assert!(matches!(status(&r, &shim), Status::TsubameStale { .. }), "command still names the shim but the file is gone");
    }

    #[test]
    fn repair_rewrites_a_moved_install_and_keeps_the_previous_default() {
        let old = Dir::new();
        let old_shim = old.path().join(SHIM_EXE);
        std::fs::write(&old_shim, b"x").unwrap();
        let mut r = MemRegistry::new().with(SHELL, "", "Prev");
        enable(&mut r, &old_shim, &old.path().join("tsubame.exe")).unwrap();
        let (_d, shim, app) = tmp_shim();
        assert!(matches!(status(&r, &shim), Status::TsubameStale { .. }));
        repair(&mut r, &shim, &app).unwrap();
        assert_eq!(status(&r, &shim), Status::Tsubame);
        assert_eq!(r.get(VERB_KEY, "PreviousDefault"), Some("Prev".into()));
        disable(&mut r).unwrap();
        assert_eq!(r.get(SHELL, ""), Some("Prev".into()));
    }

    #[test]
    fn repair_never_takes_over_from_another_app_or_registers_from_nothing() {
        let (_d, shim, app) = tmp_shim();
        let mut other = MemRegistry::new().with(SHELL, "", "Files");
        assert!(repair(&mut other, &shim, &app).is_err());
        assert_eq!(other.get(SHELL, ""), Some("Files".into()));
        let mut none = MemRegistry::new();
        assert!(repair(&mut none, &shim, &app).is_err());
        assert!(none.dump().is_empty());
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
        let d = Dir::new();
        assert!(matches!(availability(d.path()), Availability::Missing { .. }));
        std::fs::write(d.path().join(SHIM_EXE), b"x").unwrap();
        assert!(matches!(availability(d.path()), Availability::Ready { .. }));
    }

    #[test]
    fn remove_if_ours_removes_only_a_registration_that_names_this_shim() {
        let (_d, shim, app) = tmp_shim();
        let mut r = MemRegistry::new().with(SHELL, "", "Prev");
        enable(&mut r, &shim, &app).unwrap();
        assert_eq!(remove_if_ours(&mut r, &shim).unwrap(), Removal::Removed);
        assert_eq!(r.get(SHELL, ""), Some("Prev".into()));
        assert_eq!(remove_if_ours(&mut r, &shim).unwrap(), Removal::NothingRegistered);

        // registered by a different install: left alone
        let (_d2, other_shim, other_app) = tmp_shim();
        let mut r2 = MemRegistry::new();
        enable(&mut r2, &other_shim, &other_app).unwrap();
        let before = r2.dump();
        assert!(matches!(remove_if_ours(&mut r2, &shim).unwrap(), Removal::NotOurs { .. }));
        assert_eq!(r2.dump(), before);
    }

    #[test]
    fn remove_if_ours_clears_a_half_written_registration() {
        let (_d, shim, _app) = tmp_shim();
        let mut r = MemRegistry::new().with(SHELL, "", VERB).with(VERB_KEY, "", "Open in Tsubame");
        assert_eq!(remove_if_ours(&mut r, &shim).unwrap(), Removal::Removed);
        assert!(r.dump().is_empty());
    }

    #[test]
    fn same_path_ignores_case_quotes_slashes_and_the_long_path_prefix() {
        assert!(same_path(r#""C:\A\B.exe""#, r"c:\a\b.EXE"));
        assert!(same_path(r"\\?\C:\A\B.exe", r"C:/A/B.exe"));
        assert!(!same_path(r"C:\A\B.exe", r"C:\A\C.exe"));
    }

    #[test]
    fn exe_of_handles_quoted_and_bare_commands() {
        assert_eq!(exe_of(r#""C:\x y\s.exe" "%1""#), r"C:\x y\s.exe");
        assert_eq!(exe_of(r"C:\x\s.exe %1"), r"C:\x\s.exe");
        assert_eq!(exe_of(""), "");
    }
}
