//! Tsubame's extension API: one trait or data type per extension point and the
//! registry they all go through.
//!
//! Every tier registers the same way. Tier 0 (built-ins compiled into the app)
//! calls [`Registry::add`] at start-up exactly like a tier 1 TOML pack or a tier 2
//! wasm plugin will. The crate has no dependencies and no Win32, so the wasm host
//! and a future second platform can share it.
//!
//! The app decides what a command handler and a host-drawn column are through
//! [`Host`]; built-ins use plain `fn` pointers, so dispatch costs one lookup and
//! one indirect call and allocates nothing. See `docs/extensions.md`.

mod points;
mod registry;

pub use points::*;
pub use registry::*;

use std::borrow::Cow;

/// Names, titles and labels. Built-ins borrow `'static` text (no allocation,
/// cloning is a copy); packs and plugins own theirs.
pub type Text = Cow<'static, str>;

/// `Text` from a literal, usable in `const` and `static` tables.
pub const fn text(s: &'static str) -> Text {
    Cow::Borrowed(s)
}

/// Where an extension comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Rust, compiled into the app.
    BuiltIn,
    /// A declarative TOML pack (no code).
    Pack,
    /// A wasm component run in `tsubame-plughost`.
    Wasm,
}

/// Who an extension is. `id` is unique among registered extensions.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionInfo {
    /// Dotted, stable: `tsubame.core`, `tsubame.column.size`, `pack.my-tools`.
    /// This is the name the config's `[extensions] disabled` list uses.
    pub id: Text,
    pub title: Text,
    pub tier: Tier,
    /// Can not be switched off (the core commands and the Name column).
    pub required: bool,
}

impl ExtensionInfo {
    pub const fn builtin(id: &'static str, title: &'static str, required: bool) -> ExtensionInfo {
        ExtensionInfo { id: text(id), title: text(title), tier: Tier::BuiltIn, required }
    }
}

/// What the app plugs in. `Run` is how a command runs (built-ins: a `fn`
/// pointer into the app), `Field` names a column the app draws itself from its
/// own entry data (built-ins: Name, Date modified, Type, Size).
pub trait Host: 'static {
    type Run: Copy + Send + Sync + 'static;
    type Field: Copy + Send + Sync + 'static;
}

/// One extension: says who it is and adds its items to the registry.
/// `contribute` is called once, by [`Registry::add`].
pub trait Extension<H: Host> {
    fn info(&self) -> ExtensionInfo;
    fn contribute(&self, c: &mut Contributions<H>);
}

/// A built-in [`CommandSpec`] for a `static` table: id, dotted name, title,
/// default chords as text, flags.
///
/// ```
/// use tsubame_ext_api::{builtin_command, CmdFlags, CommandSpec};
/// static TABLE: &[CommandSpec] = &[builtin_command!(1, "tab.new", "New tab", ["Ctrl+T"], CmdFlags::GLOBAL)];
/// assert_eq!(TABLE[0].keys[0], "Ctrl+T");
/// ```
#[macro_export]
macro_rules! builtin_command {
    ($id:expr, $name:expr, $title:expr, [$($key:literal),* $(,)?], $flags:expr) => {
        $crate::CommandSpec::builtin($id, $name, $title, {
            const KEYS: &[$crate::Text] = &[$($crate::text($key)),*];
            KEYS
        }, $flags)
    };
}
