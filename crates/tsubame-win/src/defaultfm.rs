//! "Use Tsubame instead of File Explorer for folders": the opt-in Tier 1 registration of
//! `docs/research-default-explorer-defender.md` (R1) and `docs/default-file-manager.md`.
//!
//! The logic (which keys, status, enable, repair, disable, and what the shim does when the app
//! is gone) lives in the `tsubame-handler` crate so the app and the shim share one
//! implementation; this module is the app's door to it, plus [`WinRegistry`], the real
//! `HKCU\Software\Classes` that refuses writes under `cfg(test)` (this crate's tests) or with
//! `TSUBAME_NO_REGISTRY_WRITE=1`.
//!
//! ```text
//! HKCU\Software\Classes\Directory\shell                 (Default) = "Tsubame"
//! HKCU\Software\Classes\Directory\shell\Tsubame         (Default) = "Open in Tsubame", Icon, PreviousDefault
//! HKCU\Software\Classes\Directory\shell\Tsubame\command (Default) = "\"<install>\tsubame-handler.exe\" \"%1\""
//! ```
//! No `DelegateExecute`, no `HKLM`/`HKCR`, nothing under `Folder`, no CLSID.

pub use tsubame_handler::registry::{
    Availability, CMD_KEY, MemRegistry, Registry, Removal, SHELL, SHIM_EXE, Status, VERB, VERB_KEY, availability, command_line, disable, enable, remove_if_ours, repair, shim_path, status,
};
use tsubame_handler::sys::RawRegistry;

/// The real registry. Only the opt-in Settings buttons may call writes on it.
pub struct WinRegistry;

impl WinRegistry {
    fn writes_allowed() -> Result<(), String> {
        if cfg!(test) {
            return Err("registry writes are disabled in tests".into());
        }
        RawRegistry::writes_allowed()
    }
}

impl Registry for WinRegistry {
    fn get(&self, key: &str, name: &str) -> Option<String> {
        RawRegistry.get(key, name)
    }
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        RawRegistry.set(key, name, value)
    }
    fn delete_value(&mut self, key: &str, name: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        RawRegistry.delete_value(key, name)
    }
    fn delete_tree(&mut self, key: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        RawRegistry.delete_tree(key)
    }
    fn subkeys(&self, key: &str) -> Vec<String> {
        RawRegistry.subkeys(key)
    }
    fn key_exists(&self, key: &str) -> bool {
        RawRegistry.key_exists(key)
    }
    fn changed(&mut self) {
        if Self::writes_allowed().is_ok() {
            RawRegistry.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_registry_refuses_writes_in_tests() {
        let mut r = WinRegistry;
        assert!(r.set(r"Directory\shell\TsubameTestMustNotExist", "", "x").is_err());
        assert!(r.delete_tree(r"Directory\shell\TsubameTestMustNotExist").is_err());
        assert!(r.delete_value(r"Directory\shell\TsubameTestMustNotExist", "").is_err());
        // reads are fine and see nothing
        assert!(!r.key_exists(r"Directory\shell\TsubameTestMustNotExist"));
    }

    #[test]
    fn the_facade_still_exposes_the_whole_api() {
        // compile-time check that the settings page's imports keep working
        let d = std::env::temp_dir().join("tsubame-defaultfm-facade-nonexistent");
        assert!(matches!(availability(&d), Availability::Missing { .. }));
        assert_eq!(shim_path(&d), d.join(SHIM_EXE));
        let mut m = MemRegistry::new();
        assert!(disable(&mut m).is_ok());
        assert_eq!(status(&m, &shim_path(&d)), Status::Explorer);
    }
}
