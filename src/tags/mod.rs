//! P4.F6 tags and colours (`docs/f6-mock.md`, `mocks/mock-f6-tags.html`).
//!
//! - `model`: tag definitions, the 8 colours and marks, tri-state checks.
//! - `store`: the central `tags.db` (volume serial + 128-bit file id keys).
//! - `worker`: owns the store off the UI thread (lookups, writes, reconcile).
//! - `marks`: draws marks, clusters and chips.
//! - `ui`: the row cache, picker, filter results, banner and status text.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use tsubame_ext_api::{Contributions, Extension, ExtensionInfo};

pub mod json;
pub mod marks;
pub mod model;
pub mod store;
pub mod ui;
pub mod worker;

/// The tags extension (store, marks, picker, sidebar section, filter).
pub const EXT_TAGS: &str = "tsubame.ext.tags";
/// The optional Tags column of the Details view.
pub const EXT_COLUMN: &str = "tsubame.column.tags";

/// Built-in extension registering `tags.pick` (Alt+T).
pub struct TagsExtension;

impl Extension<crate::ext::App> for TagsExtension {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(EXT_TAGS, "Tags and colours", false)
    }

    fn contribute(&self, c: &mut Contributions<crate::ext::App>) {
        for spec in crate::commands::TAGS_COMMANDS {
            c.command(spec.clone(), crate::view::View::run_builtin);
        }
    }
}

/// The tags extension is enabled.
pub fn enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_TAGS).unwrap_or(false))
}

thread_local! {
    static COLUMN: Cell<bool> = const { Cell::new(false) };
}

/// `[tags] column`: the Details view shows the Tags column.
pub fn column_shown() -> bool {
    COLUMN.with(|c| c.get())
}

pub fn set_column_shown(on: bool) {
    COLUMN.with(|c| c.set(on));
}

/// Listings keep file ids for tag lookups only while tags are on and the
/// store holds an item: no cost otherwise.
static WANT_FIDS: AtomicBool = AtomicBool::new(false);

pub fn want_fids() -> bool {
    WANT_FIDS.load(Ordering::Relaxed)
}

pub fn set_want_fids(on: bool) {
    WANT_FIDS.store(on, Ordering::Relaxed);
}
