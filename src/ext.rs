//! The app's extension registry (`tsubame-ext-api`, `docs/extensions.md`).
//!
//! Built-in features are the first extensions: at start-up they register here
//! exactly like a pack or a plugin will, and the view dispatches through the
//! registry (`View::run_command`). The registry belongs to the UI thread (one
//! per process in practice; each test thread gets its own), built on first
//! use. Readers that run every frame or key press copy what they need out of
//! a short borrow (no allocation) or cache it against `generation()`.

use crate::view::View;
use std::cell::RefCell;
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

/// The core extension: every built-in command and the Name column. Required
/// (can not be switched off).
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
        for b in crate::cols::BUILTIN.iter().filter(|b| b.ext == CORE) {
            c.order(b.order).column(b.spec.clone(), b.source());
        }
    }
}

/// One optional built-in column (Date modified, Type, Size): an extension of
/// its own, so `[extensions] disabled` can switch it off.
struct Column(&'static crate::cols::Builtin);

impl Extension<App> for Column {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(self.0.ext, self.0.title, self.0.required)
    }
    fn contribute(&self, c: &mut Contributions<App>) {
        c.order(self.0.order).column(self.0.spec.clone(), self.0.source());
    }
}

/// A registry with the built-in extensions, in their fixed order.
pub fn builtin() -> Registry {
    let mut r = Registry::new();
    // the built-in tables are checked by the tests; a clash is a programming error
    r.add(&Core).expect("built-in commands register");
    for b in crate::cols::BUILTIN.iter().filter(|b| b.ext != CORE) {
        r.add(&Column(b)).expect("built-in columns register");
    }
    r.add(&crate::archive_ui::ArchiveExtension).expect("archive extension registers");
    r.add(&crate::meta_ui::FolderSizesExtension).expect("folder-sizes extension registers");
    r.add(&crate::meta_ui::ChecksumExtension).expect("checksum extension registers");
    r.add(&crate::search_ui::GlobalSearchExtension).expect("global-search extension registers");
    r.add(&crate::preview_ui::PreviewExtension).expect("preview extension registers");
    r.add(&crate::batchui::BatchRenameExtension).expect("batch-rename extension registers");
    r
}

thread_local! {
    static REG: RefCell<Registry> = RefCell::new(builtin());
}

/// Reads the registry. Keep `f` short: never run a command inside it.
pub fn read<R>(f: impl FnOnce(&Registry) -> R) -> R {
    REG.with(|r| f(&r.borrow()))
}

/// Changes the registry.
pub fn write<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    REG.with(|r| f(&mut r.borrow_mut()))
}

/// Caches of registry-derived data compare this to know when to rebuild.
pub fn generation() -> u64 {
    read(|r| r.generation())
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
        let table: Vec<u32> = crate::commands::all_builtin().map(|c| c.id).collect();
        assert_eq!(ids, table);
        assert_eq!(r.is_enabled(CORE), Some(true));
        assert_eq!(r.is_enabled(crate::archive_ui::EXT_ARCHIVE), Some(true));
        assert_eq!(r.is_enabled(crate::meta_ui::EXT_GIT_COL), Some(true));
        assert_eq!(r.is_enabled(crate::meta_ui::EXT_FOLDER_SIZES), Some(true));
        assert_eq!(r.is_enabled(crate::meta_ui::EXT_CHECKSUM), Some(true));
        assert_eq!(r.is_enabled(crate::search_ui::EXT_GLOBAL_SEARCH), Some(true));
        assert_eq!(r.is_enabled(crate::preview_ui::EXT_PREVIEW), Some(true));
        assert_eq!(r.is_enabled(crate::batchui::EXT_BATCH_RENAME), Some(true));
        assert!(r.command(crate::commands::CMD_BATCH_RENAME).is_some());
        assert!(r.command(crate::commands::CMD_RENAME).is_some());
        assert!(r.command(crate::commands::CMD_GLOBAL_SEARCH).is_some());
        assert!(r.command(crate::commands::CMD_PREVIEW_TOGGLE).is_some());
        assert!(r.command(crate::commands::CMD_PREVIEW_QUICK_LOOK).is_some());
        assert!(command(crate::commands::CMD_NEW_TAB).is_some_and(|(_, f)| f.has(CmdFlags::GLOBAL)));
        assert!(command(9999).is_none());
    }

    #[test]
    fn the_core_can_not_be_switched_off() {
        let mut r = builtin();
        assert!(r.set_enabled(CORE, false).is_err());
        r.set_disabled(&[CORE]);
        assert_eq!(r.commands.len(), crate::commands::builtin_count());
        // optional P4 extensions switch off cleanly
        assert_eq!(r.set_enabled(crate::search_ui::EXT_GLOBAL_SEARCH, false), Ok(true));
        assert!(r.command(crate::commands::CMD_GLOBAL_SEARCH).is_none());
        assert_eq!(r.set_enabled(crate::archive_ui::EXT_ARCHIVE, false), Ok(true));
        assert!(r.command(crate::commands::CMD_ARCHIVE_EXTRACT_HERE).is_none());
        assert_eq!(r.set_enabled(crate::preview_ui::EXT_PREVIEW, false), Ok(true));
        assert_eq!(r.set_enabled(crate::batchui::EXT_BATCH_RENAME, false), Ok(true));
        assert!(r.command(crate::commands::CMD_BATCH_RENAME).is_none());
        assert!(r.command(crate::commands::CMD_RENAME).is_some(), "plain rename stays core");
        assert!(r.command(crate::commands::CMD_PREVIEW_TOGGLE).is_none());
        assert!(r.command(crate::commands::CMD_PREVIEW_QUICK_LOOK).is_none());
    }
}
