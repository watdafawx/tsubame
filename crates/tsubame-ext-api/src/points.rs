//! The extension points of `docs/PLAN.md` section 4: `command`, `context-menu
//! item`, `column`, `previewer`, `vfs provider`, `search provider`, `side panel`,
//! `file-type rule`, `theme`.
//!
//! Code points (columns with values, previewers, vfs, search, panels) are
//! traits whose calls are batched and run on worker threads, never per frame
//! (the wasm rule: a call per row per frame would break the paint budget).
//! Data points (commands, menu items, file-type rules, themes) are plain structs.

use crate::{Host, Text};
use std::borrow::Cow;

/// Every registered item has a key (name or id) unique within its point.
pub trait Point {
    /// The point's name in errors: `"command"`, `"column"`, ...
    const POINT: &'static str;
    fn key(&self) -> &str;
}

// ---- command ----------------------------------------------------------------------

/// Command flags (bits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CmdFlags(pub u8);

impl CmdFlags {
    pub const NONE: CmdFlags = CmdFlags(0);
    /// The key runs even while a text field has the keyboard focus.
    pub const GLOBAL: CmdFlags = CmdFlags(1);
    /// Only runs when a folder is in front (not on the Settings tab).
    pub const NEEDS_FOLDER: CmdFlags = CmdFlags(2);

    pub const fn with(self, o: CmdFlags) -> CmdFlags {
        CmdFlags(self.0 | o.0)
    }
    pub const fn has(self, o: CmdFlags) -> bool {
        self.0 & o.0 == o.0
    }
}

impl std::ops::BitOr for CmdFlags {
    type Output = CmdFlags;
    fn bitor(self, o: CmdFlags) -> CmdFlags {
        self.with(o)
    }
}

/// What a command is. The handler lives next to it in [`CommandDef`].
#[derive(Clone, Debug, PartialEq)]
pub struct CommandSpec {
    /// Stable number the shortcut table carries. Built-ins use small numbers;
    /// packs take theirs from [`crate::Registry::next_command_id`].
    pub id: u32,
    /// Dotted `group.action`: what config files, packs and the palette use.
    pub name: Text,
    pub title: Text,
    /// Default chords as text (`"Ctrl+Shift+P"`); the app parses them.
    pub keys: Cow<'static, [Text]>,
    pub flags: CmdFlags,
}

impl CommandSpec {
    /// A built-in row for a `static` table.
    pub const fn builtin(id: u32, name: &'static str, title: &'static str, keys: &'static [Text], flags: CmdFlags) -> CommandSpec {
        CommandSpec { id, name: Cow::Borrowed(name), title: Cow::Borrowed(title), keys: Cow::Borrowed(keys), flags }
    }
}

pub struct CommandDef<H: Host> {
    pub spec: CommandSpec,
    pub run: H::Run,
}

impl<H: Host> Point for CommandDef<H> {
    const POINT: &'static str = "command";
    fn key(&self) -> &str {
        &self.spec.name
    }
}

// ---- context-menu item -------------------------------------------------------------

/// Where a menu item shows (bits). Empty means everywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MenuWhen(pub u8);

impl MenuWhen {
    pub const ANY: MenuWhen = MenuWhen(0);
    pub const FILES: MenuWhen = MenuWhen(1);
    pub const FOLDERS: MenuWhen = MenuWhen(2);
    pub const BACKGROUND: MenuWhen = MenuWhen(4);
    pub const DRIVES: MenuWhen = MenuWhen(8);
    pub const SEVERAL: MenuWhen = MenuWhen(16);
}

/// An entry of the own-drawn context menu that runs a registered command.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItemSpec {
    pub id: Text,
    pub label: Text,
    /// Name of the command it runs (`file.rename`).
    pub command: Text,
    pub when: MenuWhen,
    /// Lower-case file extensions without the dot; empty = any type.
    pub types: Cow<'static, [Text]>,
}

impl Point for MenuItemSpec {
    const POINT: &'static str = "menu item";
    fn key(&self) -> &str {
        &self.id
    }
}

// ---- column ------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A details-view column.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSpec {
    /// `core.name`, `core.size`, `git.status`, ...
    pub id: Text,
    pub label: Text,
    pub align: Align,
    /// Default width in DIPs (ignored for the flexible column).
    pub width: f32,
    /// Takes the width the others leave. The view has one: Name.
    pub flex: bool,
}

/// A cell value from a [`ColumnProvider`]. The app formats it.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Empty,
    Text(String),
    Int(i64),
    /// A byte count (shown like the Size column).
    Bytes(u64),
    /// A FILETIME (100 ns since 1601, UTC).
    Time(u64),
}

/// A column whose values come from code, in batches: N paths in, N values out.
/// Called on a worker thread for the visible rows; results are cached.
pub trait ColumnProvider: Send + Sync {
    /// Most paths per call (wasm hosts cap batches at about 2,000).
    fn max_batch(&self) -> usize {
        2000
    }
    /// One value per path, in order. `out` arrives empty.
    fn values(&self, paths: &[&str], out: &mut Vec<Cell>);
}

pub enum ColumnSource<H: Host> {
    /// The app draws it from its own entry data (the built-in columns).
    Field(H::Field),
    Batched(Box<dyn ColumnProvider>),
}

pub struct ColumnDef<H: Host> {
    pub spec: ColumnSpec,
    pub source: ColumnSource<H>,
}

impl<H: Host> Point for ColumnDef<H> {
    const POINT: &'static str = "column";
    fn key(&self) -> &str {
        &self.spec.id
    }
}

// ---- declared UI (previews and side panels) ------------------------------------------

/// A tree of rows the app draws. Extensions never draw directly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Heading(String),
    Text { text: String, dim: bool },
    /// Monospace text (code, hex).
    Mono(String),
    /// A label and a value (media info, properties).
    Pair(String, String),
    /// Premultiplied BGRA pixels.
    Image { w: u32, h: u32, bgra: Vec<u8> },
    /// Runs a registered command by name.
    Button { label: String, command: Text },
    /// Indents the nodes that follow by one step (until `Outdent`).
    Indent,
    Outdent,
}

/// An extension with code behind it: who it is, plus the code.
pub struct Provided<P: ?Sized> {
    pub id: Text,
    pub title: Text,
    pub provider: Box<P>,
}

impl<P: ?Sized + ProviderKind> Point for Provided<P> {
    const POINT: &'static str = P::POINT;
    fn key(&self) -> &str {
        &self.id
    }
}

/// The point name of a provider trait object (`Provided<dyn Previewer>`).
pub trait ProviderKind {
    const POINT: &'static str;
}

// ---- previewer ---------------------------------------------------------------------

/// How much a previewer may read and return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: u64,
    pub max_px: u32,
}

/// The preview pane: one previewer per file type.
pub trait Previewer: Send + Sync {
    /// `ext`: lower-case extension without the dot ("" for none).
    fn accepts(&self, ext: &str) -> bool;
    fn preview(&self, path: &str, limits: Limits, out: &mut Doc) -> Result<(), String>;
}

impl ProviderKind for dyn Previewer {
    const POINT: &'static str = "previewer";
}

// ---- vfs provider ------------------------------------------------------------------

/// One entry of a virtual folder.
#[derive(Clone, Debug, PartialEq)]
pub struct VfsEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// FILETIME
    pub mtime: u64,
}

/// A folder that is not on disk: an archive, a remote share.
pub trait VfsProvider: Send + Sync {
    /// True for paths this provider lists (`C:\a.zip\inner`).
    fn accepts(&self, path: &str) -> bool;
    /// Streams entries in batches; `sink` returns false to cancel.
    fn list(&self, path: &str, sink: &mut dyn FnMut(&[VfsEntry]) -> bool) -> Result<(), String>;
    /// Reads one file (extract, preview).
    fn read(&self, path: &str, out: &mut Vec<u8>) -> Result<(), String>;
}

impl ProviderKind for dyn VfsProvider {
    const POINT: &'static str = "vfs provider";
}

// ---- search provider ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Query<'a> {
    pub text: &'a str,
    /// Folder to search under; "" = everywhere the provider knows.
    pub root: &'a str,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: String,
    pub dir: bool,
    pub size: u64,
    pub mtime: u64,
}

pub trait SearchProvider: Send + Sync {
    /// Streams hits in batches; `sink` returns false to cancel.
    fn search(&self, q: &Query, sink: &mut dyn FnMut(&[Hit]) -> bool) -> Result<(), String>;
}

impl ProviderKind for dyn SearchProvider {
    const POINT: &'static str = "search provider";
}

// ---- side panel --------------------------------------------------------------------

/// What a side panel is built for.
#[derive(Clone, Copy, Debug)]
pub struct PanelCx<'a> {
    pub folder: &'a str,
    pub selection: &'a [&'a str],
}

/// A panel beside the list, rebuilt when the folder or the selection changes.
pub trait SidePanel: Send + Sync {
    fn build(&self, cx: &PanelCx, out: &mut Doc);
}

impl ProviderKind for dyn SidePanel {
    const POINT: &'static str = "side panel";
}

// ---- file-type rule ----------------------------------------------------------------

/// Colour, icon or opener for a set of file types.
#[derive(Clone, Debug, PartialEq)]
pub struct FileTypeRule {
    pub id: Text,
    /// Lower-case extensions without the dot.
    pub extensions: Cow<'static, [Text]>,
    /// 0xRRGGBB for the name.
    pub color: Option<u32>,
    /// Icon-pack icon name.
    pub icon: Option<Text>,
    /// Command line to open with (`"code.exe" {path}`).
    pub open_with: Option<Text>,
}

impl Point for FileTypeRule {
    const POINT: &'static str = "file-type rule";
    fn key(&self) -> &str {
        &self.id
    }
}

// ---- theme -------------------------------------------------------------------------

/// A colour theme: token name -> 0xRRGGBB, over the built-in dark or light base.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeSpec {
    pub id: Text,
    pub name: Text,
    pub dark: bool,
    pub colors: Cow<'static, [(Text, u32)]>,
}

impl Point for ThemeSpec {
    const POINT: &'static str = "theme";
    fn key(&self) -> &str {
        &self.id
    }
}
