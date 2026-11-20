//! The app's extension registry (`tsubame-ext-api`, `docs/extensions.md`).
//!
//! Built-in features are the first extensions: at start-up they register here
//! exactly like a pack or a plugin will, and the view dispatches through the
//! registry (`View::run_command`). One registry per process, built on first use;
//! only the UI thread changes it. Readers that run every frame or key press
//! copy what they need under a short read lock (no allocation) or cache it
//! against `generation()`.

use crate::view::View;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};
use tsubame_ext_api::{self as api, CmdFlags, Contributions, Extension, ExtensionInfo};
use tsubame_ui::Ui;

/// What the app plugs into the API: commands run as `fn` pointers into the
/// view; host-drawn columns are the details view's own fields.
pub struct App;

impl api::Host for App {
    type Run = fn(&mut View, &mut Ui, u32);
    type Field = crate::cols::Col;
}

pub type Registry = api::Registry<App>;

/// The core extension: every built-in command. Required (can not be switched off).
pub const CORE: &str = "tsubame.core";

struct Core;

impl Extension<App> for Core {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(CORE, "Tsubame core", true)
    }
    fn contribute(&self, c: &mut Contributions<App>) {
        for spec in crate::commands::BUILTIN {
            c.command(spec.clone(), View::run_builtin);
        }
    }
}

/// A registry with the built-in extensions, in their fixed order.
pub fn builtin() -> Registry {
    let mut r = Registry::new();
    // the built-in tables are checked by the tests; a clash is a programming error
    r.add(&Core).expect("built-in commands register");
    r
}

static REG: LazyLock<RwLock<Registry>> = LazyLock::new(|| {
    let r = builtin();
    GEN.store(r.generation(), Ordering::Release);
    RwLock::new(r)
});
static GEN: AtomicU64 = AtomicU64::new(0);

/// Reads the registry. Keep `f` short: no command runs while it is held.
pub fn read<R>(f: impl FnOnce(&Registry) -> R) -> R {
    f(&REG.read().unwrap_or_else(|e| e.into_inner()))
}

/// Changes the registry (UI thread).
pub fn write<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut g = REG.write().unwrap_or_else(|e| e.into_inner());
    let out = f(&mut g);
    GEN.store(g.generation(), Ordering::Release);
    out
}

/// The registry's generation without taking the lock: caches compare it.
pub fn generation() -> u64 {
    LazyLock::force(&REG);
    GEN.load(Ordering::Acquire)
}

/// How to run command `id`, when it is registered and enabled.
pub fn command(id: u32) -> Option<(<App as api::Host>::Run, CmdFlags)> {
    read(|r| r.command(id).map(|c| (c.run, c.spec.flags)))
}

/// Applies the config's `[extensions] disabled` list.
pub fn apply_disabled(ids: &[String]) {
    write(|r| r.set_disabled(ids));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_ins_register_in_table_order_and_dispatch() {
        let r = builtin();
        let ids: Vec<u32> = r.commands.iter().map(|c| c.spec.id).collect();
        let table: Vec<u32> = crate::commands::BUILTIN.iter().map(|c| c.id).collect();
        assert_eq!(ids, table);
        assert_eq!(r.is_enabled(CORE), Some(true));
        assert!(r.command(crate::commands::CMD_RENAME).is_some());
        assert!(command(crate::commands::CMD_NEW_TAB).is_some_and(|(_, f)| f.has(CmdFlags::GLOBAL)));
        assert!(command(9999).is_none());
    }

    #[test]
    fn the_core_can_not_be_switched_off() {
        let mut r = builtin();
        assert!(r.set_enabled(CORE, false).is_err());
        r.set_disabled(&[CORE]);
        assert_eq!(r.commands.len(), crate::commands::BUILTIN.len());
    }
}
