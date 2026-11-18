//! Tsubame settings and session files: versioned TOML, atomic saves and
//! debounced writes. Pure logic, no Win32 (paths come from the environment),
//! no UI, fully unit-tested.
//!
//! # Files
//!
//! - `config.toml` ([`Config`]): user settings. Only values the user changed
//!   are written; every missing key keeps its default.
//! - `session.toml` ([`Session`]): open tabs, view state per tab and the window
//!   geometry. Restored only when `restore_session` is on and the command line
//!   named no path.
//!
//! # Behaviour that must not change
//!
//! - **A corrupt file is never fatal.** It is moved to `<name>.bad` and the
//!   defaults are used ([`LoadStatus::Recovered`]).
//! - **Unknown keys are kept.** [`ConfigFile`] remembers the file it parsed and
//!   merges keys this version does not know back in when it saves, so a newer
//!   Tsubame's settings are not destroyed by an older build.
//! - **A newer schema is not overwritten.** [`LoadStatus::Newer`] tells the app
//!   to read the file but never write it back.
//! - **Writes are atomic** ([`write_atomic`]: temp file in the same directory,
//!   flushed, then `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` through
//!   `std::fs::rename`) and **debounced** ([`Debounce`]: at most one write per
//!   2 s, flushed on exit, no write when nothing changed).
//!
//! See `docs/config.md` for the schema, every key's default, and the CLI.

mod atomic;
mod debounce;
mod locate;
pub mod menu;
pub mod nss;
pub mod session;

pub use atomic::{corrupt_path, write_atomic, write_atomic_with};
pub use debounce::{Debounce, SAVE_INTERVAL};
pub use locate::{locate, locate_now, Locations, LocateOptions, CONFIG_NAME, DATA_ENV, PORTABLE_MARKER, PORTABLE_MARKER_LEGACY, SESSION_NAME};
pub use menu::{EntryKind, MenuEntry, ShowFor, Source};
pub use session::Session;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Schema version of `config.toml`. Bump it and add a migration to
/// [`MIGRATIONS`] when a key changes meaning.
pub const VERSION: u32 = 1;

/// The default theme id (`docs/config.md` lists the accepted names).
pub const DEFAULT_THEME: &str = "tsubame-dark";
pub const DEFAULT_DENSITY: Density = Density::Compact;
pub const DEFAULT_VIEW_MODE: ViewMode = ViewMode::Details;
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 208.0;

/// A lossy enum: unknown or misspelled values fall back to the default instead
/// of making the whole file "corrupt" (the raw text is still ignored, not
/// fatal). `as_str` is what gets written back.
macro_rules! lossy_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($variant:ident => $text:literal),+ $(,)? }
        default $default:ident
        $(, $extra:literal => $extra_variant:ident)*
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl Default for $name {
            fn default() -> Self { $name::$default }
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            /// The spelling written to the file.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
            /// Never fails: an unknown value is the default.
            pub fn parse(s: &str) -> $name {
                match s.trim().to_ascii_lowercase().as_str() {
                    $($text => $name::$variant,)+
                    $($extra => $name::$extra_variant,)*
                    _ => $name::$default,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }

        impl std::str::FromStr for $name {
            type Err = std::convert::Infallible;
            fn from_str(s: &str) -> Result<Self, Self::Err> { Ok(Self::parse(s)) }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Ok(Self::parse(&s))
            }
        }
    };
}

lossy_enum! {
    /// Row height preset. `normal` is the 26 DIP kit preset (the UI kit calls
    /// it `Comfortable`), `comfortable` is the 30 DIP one (`Spacious`).
    Density { Compact => "compact", Normal => "normal", Comfortable => "comfortable" }
    default Compact
    , "spacious" => Comfortable, "normal-row" => Normal
}

lossy_enum! {
    /// How the listing is drawn. The grid modes arrive with the thumbnail view.
    ViewMode { Details => "details", GridSmall => "grid-small", GridLarge => "grid-large" }
    default Details
    , "grid" => GridSmall, "thumbnails" => GridLarge, "list" => Details
}

lossy_enum! {
    /// Sort column of a tab.
    SortColumn { Name => "name", Size => "size", Modified => "modified", Type => "type" }
    default Name
    , "date" => Modified, "date-modified" => Modified, "extension" => Type
}

lossy_enum! {
    /// How the two panes are arranged.
    Split { Vertical => "vertical", Horizontal => "horizontal" }
    default Vertical
    , "columns" => Vertical, "rows" => Horizontal
}

fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}
fn half() -> f32 {
    0.5
}
pub(crate) fn default_version() -> u32 {
    VERSION
}
fn default_theme() -> String {
    DEFAULT_THEME.to_string()
}
fn default_start_folder() -> String {
    "last".to_string()
}
fn default_channel() -> String {
    "stable".to_string()
}
fn default_profile() -> String {
    "default".to_string()
}
fn default_sidebar_width() -> f32 {
    DEFAULT_SIDEBAR_WIDTH
}
fn default_window_w() -> i32 {
    1120
}
fn default_window_h() -> i32 {
    720
}

/// `[general]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// A folder path, or `"last"` for the last folder of the previous session.
    #[serde(default = "default_start_folder")]
    pub start_folder: String,
    #[serde(default = "yes")]
    pub restore_session: bool,
    pub show_hidden: bool,
    pub show_system: bool,
    #[serde(default = "yes")]
    pub confirm_delete: bool,
    #[serde(default = "yes")]
    pub delete_to_recycle: bool,
}

impl Default for General {
    fn default() -> Self {
        General { start_folder: default_start_folder(), restore_session: true, show_hidden: false, show_system: false, confirm_delete: true, delete_to_recycle: true }
    }
}

/// `[appearance]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Theme id: `tsubame-dark` (default) or `tsubame-light`; `dark`/`light`
    /// are accepted aliases.
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub density: Density,
    #[serde(default = "yes")]
    pub zebra: bool,
    #[serde(default = "one")]
    pub font_scale: f32,
    /// `#rrggbb` or `#rgb`; empty/absent means the theme's own accent.
    pub accent: Option<String>,
    #[serde(default)]
    pub view_mode: ViewMode,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { theme: default_theme(), density: DEFAULT_DENSITY, zebra: true, font_scale: 1.0, accent: None, view_mode: DEFAULT_VIEW_MODE }
    }
}

impl Appearance {
    /// `tsubame-light` and its alias `light` are the light theme; everything
    /// else (including an unknown id) is dark, the default.
    pub fn is_dark(&self) -> bool {
        let t = self.theme.trim();
        !(t.eq_ignore_ascii_case("light") || t.eq_ignore_ascii_case("tsubame-light") || t.eq_ignore_ascii_case("tsubame light"))
    }
    /// `#rrggbb` / `#rgb` / `rrggbb` to `0xRRGGBB`.
    pub fn accent_rgb(&self) -> Option<u32> {
        let s = self.accent.as_deref()?.trim().trim_start_matches('#');
        let expand = |c: char| c.to_digit(16).map(|d| d * 17);
        match s.len() {
            6 => u32::from_str_radix(s, 16).ok(),
            3 => {
                let mut it = s.chars();
                let (r, g, b) = (expand(it.next()?)?, expand(it.next()?)?, expand(it.next()?)?);
                Some((r << 16) | (g << 8) | b)
            }
            _ => None,
        }
    }
}

/// `[panes]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Panes {
    pub dual: bool,
    #[serde(default)]
    pub split: Split,
    /// Share of the area the first pane takes (0.2 to 0.8).
    #[serde(default = "half")]
    pub ratio: f32,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// The sidebar is the 48 px icon rail (Ctrl+B).
    pub sidebar_collapsed: bool,
}

impl Default for Panes {
    fn default() -> Self {
        Panes { dual: false, split: Split::Vertical, ratio: 0.5, sidebar_width: DEFAULT_SIDEBAR_WIDTH, sidebar_collapsed: false }
    }
}

/// One pinned (bookmarked) folder: `[[sidebar.pinned]]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pinned {
    /// Label shown in the sidebar (the folder name unless renamed).
    pub name: String,
    pub path: String,
}

/// `[sidebar]`: bookmarks and which sections are folded.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sidebar {
    pub pinned: Vec<Pinned>,
    /// Folded sections: any of `"pinned"`, `"quick"`, `"recent"`, `"pc"`, `"network"`.
    pub closed: Vec<String>,
}

/// `[updates]`, read by the updater wiring (`docs/updater.md`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Updates {
    /// Check for a newer version when the app starts (at most once a day).
    /// `check` is accepted as an alias.
    #[serde(default = "yes", alias = "check")]
    pub auto_check: bool,
    /// Download a verified update in the background and apply it at the next
    /// restart. Off by default: the user clicks "Download and restart".
    #[serde(default)]
    pub auto_install: bool,
    /// `stable` today; the field exists so a beta channel needs no schema bump.
    #[serde(default = "default_channel")]
    pub channel: String,
}

impl Default for Updates {
    fn default() -> Self {
        Updates { auto_check: true, auto_install: false, channel: default_channel() }
    }
}

/// `[keys]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Command id -> chord strings (`"Ctrl+Shift+L"`). An empty list unbinds.
    pub overrides: BTreeMap<String, Vec<String>>,
}

impl Default for Keys {
    fn default() -> Self {
        Keys { profile: default_profile(), overrides: BTreeMap::new() }
    }
}

/// `[advanced]`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Advanced {
    /// Write a diagnostics log (`<data dir>/logs/tsubame.log`). Off by default.
    pub diagnostics: bool,
}

/// `[context_menu]`: the editable right-click menu (`docs/u4-settings.md`).
/// Empty `entries` means the built-in default menu.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextMenu {
    pub entries: Vec<MenuEntry>,
}

/// A rectangle in virtual-screen pixels (a monitor's work area).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Window geometry, in pixels. Restored at startup with
/// [`WindowState::sanitized`] so a window saved on a monitor that is no longer
/// connected cannot end up off-screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    #[serde(default = "default_window_w")]
    pub w: i32,
    #[serde(default = "default_window_h")]
    pub h: i32,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        WindowState { x: 0, y: 0, w: default_window_w(), h: default_window_h(), maximized: false }
    }
}

impl WindowState {
    pub const MIN_W: i32 = 640;
    pub const MIN_H: i32 = 400;
    /// At least this much of the window must be inside a work area.
    const VISIBLE: i32 = 48;

    /// Clamps the size and, when no monitor shows enough of the window, moves
    /// it to the centre of the first work area (the primary monitor).
    pub fn sanitized(self, work_areas: &[WorkArea]) -> WindowState {
        let mut s = self;
        s.w = s.w.clamp(Self::MIN_W, 32_000);
        s.h = s.h.clamp(Self::MIN_H, 32_000);
        if work_areas.is_empty() {
            return s;
        }
        let visible = work_areas.iter().any(|a| {
            let ix = (s.x + s.w).min(a.x + a.w) - s.x.max(a.x);
            let iy = (s.y + s.h).min(a.y + a.h) - s.y.max(a.y);
            ix >= Self::VISIBLE && iy >= Self::VISIBLE
        });
        if !visible {
            let a = work_areas[0];
            s.x = a.x + (a.w - s.w).max(0) / 2;
            s.y = a.y + (a.h - s.h).max(0) / 2;
        }
        s
    }
}

/// The whole `config.toml` schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    pub general: General,
    pub appearance: Appearance,
    pub panes: Panes,
    pub sidebar: Sidebar,
    pub updates: Updates,
    pub keys: Keys,
    pub context_menu: ContextMenu,
    pub advanced: Advanced,
    pub window: WindowState,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: VERSION,
            general: General::default(),
            appearance: Appearance::default(),
            panes: Panes::default(),
            sidebar: Sidebar::default(),
            updates: Updates::default(),
            keys: Keys::default(),
            context_menu: ContextMenu::default(),
            advanced: Advanced::default(),
            window: WindowState::default(),
        }
    }
}

impl Config {
    /// Where a plain start (no path on the command line, no session) opens.
    pub fn start_folder(&self) -> StartFolder {
        let s = self.general.start_folder.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("last") {
            StartFolder::Last
        } else {
            StartFolder::Path(s.to_string())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartFolder {
    /// The last folder of the previous session.
    Last,
    Path(String),
}

/// The parsed config plus the file it came from, so unknown keys survive a save.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigFile {
    pub config: Config,
    /// The file as parsed. Only keys [`Config`] does not know are copied back
    /// on save; known keys always come from `config`.
    origin: toml::Table,
}

impl Default for ConfigFile {
    fn default() -> Self {
        ConfigFile::new(Config::default())
    }
}

impl ConfigFile {
    pub fn new(config: Config) -> ConfigFile {
        ConfigFile { config, origin: toml::Table::new() }
    }
    /// Loads `path`; never fails. See [`LoadStatus`] for what happened.
    pub fn load(path: &Path) -> Loaded<ConfigFile> {
        let defaults = || ConfigFile::new(Config::default());
        let make = |value: ConfigFile, status, bytes| Loaded { value, status, path: path.to_path_buf(), bytes };
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return make(defaults(), LoadStatus::Missing, 0),
            Err(e) => return make(defaults(), LoadStatus::Failed(format!("cannot read {}: {e}", path.display())), 0),
        };
        let len = bytes.len();
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => return recover(path, len, format!("not valid UTF-8: {e}")),
        };
        let mut table: toml::Table = match toml::from_str(&text) {
            Ok(t) => t,
            Err(e) => return recover(path, len, format!("invalid TOML: {e}")),
        };
        let file_version = table.get("version").and_then(|v| v.as_integer()).unwrap_or(VERSION as i64);
        if file_version > VERSION as i64 {
            // best effort: read what we understand, never write it back
            let config = table.clone().try_into::<Config>().unwrap_or_default();
            return make(ConfigFile { config, origin: table }, LoadStatus::Newer { version: file_version as u32 }, len);
        }
        if file_version < VERSION as i64 {
            let from = file_version.max(0) as u32;
            let to = migrate(&mut table, from, MIGRATIONS);
            table.insert("version".to_string(), toml::Value::Integer(to as i64));
        }
        match table.clone().try_into::<Config>() {
            Ok(config) => make(ConfigFile { config, origin: table }, LoadStatus::Loaded, len),
            Err(e) => recover(path, len, format!("invalid config value: {e}")),
        }
    }

    /// The file contents: the typed config first, then every key this version
    /// did not understand, unchanged.
    pub fn to_toml(&self) -> String {
        let mut table = toml::Table::try_from(&self.config).unwrap_or_default();
        merge_unknown(&mut table, self.origin.clone());
        let mut out = String::from("# Tsubame configuration. Written by the app; comments are not kept.\n# Schema and defaults: docs/config.md\n");
        match toml::to_string_pretty(&table) {
            Ok(body) => out.push_str(&body),
            Err(_) => out.clear(),
        }
        out
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_toml().into_bytes()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, &self.to_bytes())
    }
}

/// Keys the typed config does not know, copied back under the keys it does.
fn merge_unknown(typed: &mut toml::Table, origin: toml::Table) {
    merge_unknown_at(typed, origin, "");
}

/// Tables whose keys are user data (a map the app edits): the typed table is
/// complete, so a key the user removed must not come back from the old file.
const OPAQUE_TABLES: &[&str] = &["keys.overrides"];

fn merge_unknown_at(typed: &mut toml::Table, origin: toml::Table, path: &str) {
    for (k, v) in origin {
        match typed.get_mut(&k) {
            Some(toml::Value::Table(t)) => {
                let here = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                if let toml::Value::Table(o) = v
                    && !OPAQUE_TABLES.contains(&here.as_str())
                {
                    merge_unknown_at(t, o, &here);
                }
            }
            // a known key: the typed value wins
            Some(_) => {}
            None => {
                typed.insert(k, v);
            }
        }
    }
}

/// One schema migration: `from` -> `from + 1`. A hook adjusts the raw table;
/// [`migrate`] bumps the version after it runs.
pub type Migration = fn(&mut toml::Table);

/// Registered migrations. Empty at version 1; a key that changes meaning gets
/// an entry here and [`VERSION`] goes up.
pub const MIGRATIONS: &[(u32, Migration)] = &[];

/// Runs every registered migration from `from` up to [`VERSION`] and returns
/// the new version. Missing hooks (no change between two versions) just bump.
pub fn migrate(table: &mut toml::Table, from: u32, hooks: &[(u32, Migration)]) -> u32 {
    let mut v = from;
    while v < VERSION {
        if let Some((_, hook)) = hooks.iter().find(|(from_version, _)| *from_version == v) {
            hook(table);
        }
        v += 1;
    }
    table.insert("version".to_string(), toml::Value::Integer(v as i64));
    v
}

/// What [`ConfigFile::load`] / [`load_toml`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadStatus {
    /// read and parsed
    Loaded,
    /// no file: first start, or the user deleted it
    Missing,
    /// unreadable or unparseable: moved to `backup`, defaults are used
    Recovered { backup: PathBuf, error: String },
    /// written by a newer Tsubame: parsed best effort, do not save over it
    Newer { version: u32 },
    /// could not even be read (permissions, a directory in the way)
    Failed(String),
}

impl LoadStatus {
    /// The app must not write the file back.
    pub fn read_only(&self) -> bool {
        matches!(self, LoadStatus::Newer { .. } | LoadStatus::Failed(_))
    }
    pub fn message(&self) -> Option<String> {
        match self {
            LoadStatus::Recovered { backup, error } => Some(format!("Config was unreadable ({error}); defaults used. The old file is {}.", backup.display())),
            LoadStatus::Failed(e) => Some(format!("Config could not be read: {e}")),
            LoadStatus::Newer { version } => Some(format!("Config schema {version} is newer than this build; it will not be overwritten.")),
            LoadStatus::Loaded | LoadStatus::Missing => None,
        }
    }
}

/// A loaded value with the outcome and the file size.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded<T> {
    pub value: T,
    pub status: LoadStatus,
    pub path: PathBuf,
    /// bytes read (0 when missing)
    pub bytes: usize,
}

fn recover<T>(path: &Path, bytes: usize, error: String) -> Loaded<T>
where
    T: Default,
{
    let backup = corrupt_path(path);
    // std::fs::rename replaces the backup too (MoveFileExW with replace)
    let _ = fs::rename(path, &backup);
    Loaded { value: T::default(), status: LoadStatus::Recovered { backup, error }, path: path.to_path_buf(), bytes }
}

/// Generic versioned-Toml loader used by the session (and by tests). A newer
/// `version` is reported as [`LoadStatus::Newer`]; a broken file is moved
/// aside. Never panics.
pub fn load_toml<T>(path: &Path) -> Loaded<T>
where
    T: DeserializeOwned + Default,
{
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Loaded { value: T::default(), status: LoadStatus::Missing, path: path.to_path_buf(), bytes: 0 },
        Err(e) => return Loaded { value: T::default(), status: LoadStatus::Failed(format!("cannot read {}: {e}", path.display())), path: path.to_path_buf(), bytes: 0 },
    };
    let len = bytes.len();
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => return recover(path, len, format!("not valid UTF-8: {e}")),
    };
    let table: toml::Table = match toml::from_str(&text) {
        Ok(t) => t,
        Err(e) => return recover(path, len, format!("invalid TOML: {e}")),
    };
    let version = table.get("version").and_then(|v| v.as_integer()).unwrap_or(1);
    match table.try_into::<T>() {
        Ok(value) => {
            let status = if version > 1 { LoadStatus::Newer { version: version as u32 } } else { LoadStatus::Loaded };
            Loaded { value, status, path: path.to_path_buf(), bytes: len }
        }
        Err(e) => recover(path, len, format!("invalid values: {e}")),
    }
}

/// Serialises any of the file types. The error is only a programming mistake
/// for the schemas here, so `to_toml` treats it as "do not write".
pub fn to_toml_string<T: Serialize>(value: &T) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(value)
}

/// Times a load, for the startup report (`docs/config.md`).
pub fn timed_load_config(path: &Path) -> (Loaded<ConfigFile>, f64) {
    let t = Instant::now();
    let l = ConfigFile::load(path);
    (l, t.elapsed().as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-config-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const TYPICAL: &str = r##"# a typical config
version = 1

[general]
start_folder = "C:\\Users\\me\\Desktop"
restore_session = false
show_hidden = true
show_system = true
confirm_delete = false
delete_to_recycle = false

[appearance]
theme = "tsubame-light"
density = "comfortable"
zebra = false
font_scale = 1.25
accent = "#e0492c"
view_mode = "grid-large"

[panes]
dual = true
split = "horizontal"
sidebar_width = 240.0
sidebar_collapsed = true

[updates]
auto_check = false
channel = "stable"

[keys]
profile = "vim"

[keys.overrides]
"view.refresh" = ["F5", "Ctrl+R"]

[window]
x = 100
y = 50
w = 1400
h = 900
maximized = true
"##;

    #[test]
    fn defaults_are_the_documented_ones() {
        let c = Config::default();
        assert_eq!(c.version, VERSION);
        assert_eq!(c.general.start_folder, "last");
        assert!(c.general.restore_session && c.general.confirm_delete && c.general.delete_to_recycle);
        assert!(!c.general.show_hidden && !c.general.show_system);
        assert_eq!(c.appearance.theme, DEFAULT_THEME);
        assert_eq!(c.appearance.density, Density::Compact);
        assert!(c.appearance.zebra);
        assert_eq!(c.appearance.font_scale, 1.0);
        assert_eq!(c.appearance.accent, None);
        assert_eq!(c.appearance.view_mode, ViewMode::Details);
        assert!(!c.panes.dual && !c.panes.sidebar_collapsed);
        assert_eq!(c.panes.split, Split::Vertical);
        assert_eq!(c.panes.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        assert!(c.updates.auto_check && c.updates.channel == "stable");
        assert_eq!(c.keys.profile, "default");
        assert!(c.keys.overrides.is_empty());
        assert_eq!(c.window, WindowState { x: 0, y: 0, w: 1120, h: 720, maximized: false });
        assert_eq!(c.start_folder(), StartFolder::Last);
    }

    #[test]
    fn an_empty_file_is_all_defaults() {
        let d = temp_dir("empty");
        let p = d.join("config.toml");
        fs::write(&p, b"").unwrap();
        let l = ConfigFile::load(&p);
        assert_eq!(l.status, LoadStatus::Loaded);
        assert_eq!(l.value.config, Config::default());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_file_is_defaults_and_not_an_error() {
        let d = temp_dir("missing");
        let p = d.join("config.toml");
        let l = ConfigFile::load(&p);
        assert_eq!(l.status, LoadStatus::Missing);
        assert_eq!(l.value.config, Config::default());
        assert_eq!(l.bytes, 0);
        assert!(!p.exists(), "loading never creates the file");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn every_key_loads() {
        let d = temp_dir("keys");
        let p = d.join("config.toml");
        fs::write(&p, TYPICAL).unwrap();
        let l = ConfigFile::load(&p);
        assert_eq!(l.status, LoadStatus::Loaded, "{:?}", l.status.message());
        let c = &l.value.config;
        assert_eq!(c.general.start_folder, "C:\\Users\\me\\Desktop");
        assert!(!c.general.restore_session && c.general.show_hidden && c.general.show_system);
        assert!(!c.general.confirm_delete && !c.general.delete_to_recycle);
        assert_eq!(c.appearance.theme, "tsubame-light");
        assert!(!c.appearance.is_dark());
        assert_eq!(c.appearance.density, Density::Comfortable);
        assert!(!c.appearance.zebra);
        assert_eq!(c.appearance.font_scale, 1.25);
        assert_eq!(c.appearance.accent_rgb(), Some(0x00e0_492c));
        assert_eq!(c.appearance.view_mode, ViewMode::GridLarge);
        assert!(c.panes.dual && c.panes.sidebar_collapsed);
        assert_eq!(c.panes.split, Split::Horizontal);
        assert_eq!(c.panes.sidebar_width, 240.0);
        assert!(!c.updates.auto_check);
        assert_eq!(c.keys.profile, "vim");
        assert_eq!(c.keys.overrides["view.refresh"], ["F5", "Ctrl+R"]);
        assert_eq!(c.window, WindowState { x: 100, y: 50, w: 1400, h: 900, maximized: true });
        assert_eq!(c.start_folder(), StartFolder::Path("C:\\Users\\me\\Desktop".into()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn round_trip_keeps_every_value() {
        let d = temp_dir("round");
        let p = d.join("config.toml");
        fs::write(&p, TYPICAL).unwrap();
        let l = ConfigFile::load(&p);
        let text = l.value.to_toml();
        assert!(text.contains("version = 1"));
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, l.value.config);
        // and a second round trip is stable
        let again = ConfigFile::new(back).to_toml();
        let third: Config = toml::from_str(&again).unwrap();
        assert_eq!(third, l.value.config);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn pinned_closed_and_ratio_round_trip() {
        let mut c = Config::default();
        c.panes.dual = true;
        c.panes.ratio = 0.35;
        c.sidebar.pinned = vec![Pinned { name: "Work".into(), path: "D:\\work".into() }, Pinned { name: "x \"q\"".into(), path: "C:\\a b".into() }];
        c.sidebar.closed = vec!["recent".into(), "network".into()];
        let text = ConfigFile::new(c.clone()).to_toml();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.sidebar.pinned.len(), 2);
        // an old file with no [sidebar] and no ratio takes defaults
        let old: Config = toml::from_str("[panes]\ndual = true\n").unwrap();
        assert!(old.sidebar.pinned.is_empty());
        assert!((old.panes.ratio - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn missing_keys_take_defaults_and_partial_sections_do_not_break() {
        let text = "[appearance]\ntheme = \"light\"\n\n[general]\nshow_hidden = true\n";
        let c: Config = toml::from_str(text).unwrap();
        let d = Config::default();
        assert_eq!(c.appearance.theme, "light");
        assert!(!c.appearance.is_dark());
        assert_eq!(c.appearance.zebra, d.appearance.zebra, "missing key keeps its default");
        assert_eq!(c.appearance.density, Density::Compact);
        assert!(c.general.show_hidden);
        assert!(c.general.restore_session, "section default");
        assert_eq!(c.panes, d.panes);
    }

    #[test]
    fn unknown_keys_are_ignored_on_load_and_kept_on_save() {
        let d = temp_dir("unknown");
        let p = d.join("config.toml");
        fs::write(
            &p,
            r#"version = 1
future_top = "keep me"

[general]
show_hidden = true

[general.future_nested]
deep = 7

[appearance]
theme = "tsubame-light"

[future_section]
answer = 42
names = ["a", "b"]
"#,
        )
        .unwrap();
        let l = ConfigFile::load(&p);
        assert_eq!(l.status, LoadStatus::Loaded);
        assert!(l.value.config.general.show_hidden);
        let text = l.value.to_toml();
        assert!(text.contains("future_top = \"keep me\""), "{text}");
        assert!(text.contains("future_section"), "{text}");
        assert!(text.contains("answer = 42"), "{text}");
        assert!(text.contains("future_nested"), "{text}");
        assert!(text.contains("deep = 7"), "{text}");
        assert!(text.contains("theme = \"tsubame-light\""), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, l.value.config, "unknown keys never change the typed values");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_values_fall_back_instead_of_corrupting() {
        let c: Config = toml::from_str("[appearance]\ndensity = \"enormous\"\nview_mode = \"carousel\"\n\n[panes]\nsplit = \"diagonal\"\n").unwrap();
        assert_eq!(c.appearance.density, Density::Compact);
        assert_eq!(c.appearance.view_mode, ViewMode::Details);
        assert_eq!(c.panes.split, Split::Vertical);
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_defaults_are_used() {
        let d = temp_dir("corrupt");
        let p = d.join("config.toml");
        fs::write(&p, b"version = 1\n[general\nstart_folder = ").unwrap();
        let l = ConfigFile::load(&p);
        match &l.status {
            LoadStatus::Recovered { backup, error } => {
                assert!(backup.ends_with("config.toml.bad"));
                assert!(!error.is_empty());
            }
            other => panic!("expected Recovered, got {other:?}"),
        }
        assert_eq!(l.value.config, Config::default());
        assert!(!p.exists());
        assert_eq!(fs::read(d.join("config.toml.bad")).unwrap(), b"version = 1\n[general\nstart_folder = ");
        // the next save starts from the defaults
        l.value.save(&p).unwrap();
        assert!(p.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_wrongly_typed_value_is_also_corrupt_but_a_marker_keeps_it() {
        let d = temp_dir("typed");
        let p = d.join("config.toml");
        fs::write(&p, b"version = 1\n[general]\nrestore_session = \"yes please\"\n").unwrap();
        let l = ConfigFile::load(&p);
        assert!(matches!(l.status, LoadStatus::Recovered { .. }), "{:?}", l.status);
        assert!(d.join("config.toml.bad").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_newer_schema_is_read_but_never_written() {
        let d = temp_dir("newer");
        let p = d.join("config.toml");
        fs::write(&p, b"version = 7\nfuture = true\n[general]\nshow_hidden = true\n").unwrap();
        let before = fs::read(&p).unwrap();
        let l = ConfigFile::load(&p);
        assert_eq!(l.status, LoadStatus::Newer { version: 7 });
        assert!(l.status.read_only());
        assert!(l.value.config.general.show_hidden, "best-effort read");
        assert_eq!(fs::read(&p).unwrap(), before, "nothing rewrote the file");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn migration_hooks_run_in_order_and_bump_the_version() {
        let mut table: toml::Table = toml::from_str("version = 0\nkeep = true\n").unwrap();
        static ORDER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn first(t: &mut toml::Table) {
            ORDER.store(1, std::sync::atomic::Ordering::SeqCst);
            t.insert("first".into(), toml::Value::Boolean(true));
        }
        fn second(t: &mut toml::Table) {
            assert_eq!(ORDER.load(std::sync::atomic::Ordering::SeqCst), 1);
            t.insert("second".into(), toml::Value::Integer(2));
        }
        if VERSION == 1 {
            // only one step exists at version 1; a no-op hook still runs
            let v = migrate(&mut table, 0, &[(0, first)]);
            assert_eq!(v, VERSION);
            assert!(table.contains_key("first"));
            assert_eq!(table.get("version").and_then(|v| v.as_integer()), Some(VERSION as i64));
            assert_eq!(table.get("keep").and_then(|v| v.as_bool()), Some(true));
        } else {
            let v = migrate(&mut table, 0, &[(0, first), (1, second)]);
            assert_eq!(v, VERSION);
            assert!(table.contains_key("first") && table.contains_key("second"));
        }
        // a file already at the current version is not migrated
        let mut current: toml::Table = toml::from_str("version = 1\n").unwrap();
        assert_eq!(migrate(&mut current, VERSION, &[(0, first)]), VERSION);
        assert!(!current.contains_key("first"));
    }

    #[test]
    fn the_default_migration_table_is_well_formed() {
        for (from, _) in MIGRATIONS {
            assert!(*from < VERSION, "migration {from} is not below the current version");
        }
    }

    #[test]
    fn save_is_atomic_and_reloads() {
        let d = temp_dir("save");
        let p = d.join("config.toml");
        let mut cfg = Config::default();
        cfg.appearance.density = Density::Comfortable;
        cfg.keys.overrides.insert("view.refresh".into(), vec!["F5".into()]);
        let cf = ConfigFile::new(cfg);
        cf.save(&p).unwrap();
        assert!(!d.join("config.toml.tmp").exists());
        let l = ConfigFile::load(&p);
        assert_eq!(l.value.config, cf.config);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn accent_parsing() {
        let mut a = Appearance::default();
        assert_eq!(a.accent_rgb(), None);
        a.accent = Some("#e0492c".into());
        assert_eq!(a.accent_rgb(), Some(0x00e0_492c));
        a.accent = Some("E0492C".into());
        assert_eq!(a.accent_rgb(), Some(0x00e0_492c));
        a.accent = Some("#f00".into());
        assert_eq!(a.accent_rgb(), Some(0x00ff_0000));
        a.accent = Some("#nope".into());
        assert_eq!(a.accent_rgb(), None);
    }

    #[test]
    fn window_geometry_is_made_monitor_safe() {
        let a = WorkArea { x: 0, y: 0, w: 1920, h: 1040 };
        let b = WorkArea { x: 1920, y: 0, w: 2560, h: 1400 };
        // fully on a monitor: unchanged
        let mut w = WindowState { x: 100, y: 100, w: 1200, h: 800, maximized: false };
        assert_eq!(w.sanitized(&[a, b]), w);
        // on the second monitor: unchanged
        w.x = 2000;
        assert_eq!(w.sanitized(&[a, b]), w);
        // saved on a monitor that is gone: centred on the primary
        let gone = WindowState { x: 4000, y: -2000, w: 1200, h: 800, maximized: true };
        let s = gone.sanitized(&[a, b]);
        assert_eq!((s.x, s.y), ((1920 - 1200) / 2, (1040 - 800) / 2));
        assert!(s.maximized, "the maximized flag survives");
        // a sliver on screen counts as visible (a monitor changed resolution)
        let sliver = WindowState { x: 1900, y: 1000, w: 1200, h: 800, maximized: false };
        assert_eq!(sliver.sanitized(&[a, b]), sliver);
        // tiny sizes are raised to the minimum
        let tiny = WindowState { x: 10, y: 10, w: 10, h: 10, maximized: false };
        let t = tiny.sanitized(&[a]);
        assert_eq!((t.w, t.h), (WindowState::MIN_W, WindowState::MIN_H));
        // no monitors known yet: only the size is clamped
        let none = WindowState { x: -5, y: -5, w: 10, h: 10, maximized: false }.sanitized(&[]);
        assert_eq!((none.x, none.y, none.w, none.h), (-5, -5, WindowState::MIN_W, WindowState::MIN_H));
    }

    #[test]
    fn density_names_and_aliases() {
        assert_eq!(Density::parse("normal"), Density::Normal);
        assert_eq!(Density::parse(" COMPACT "), Density::Compact);
        assert_eq!(Density::parse("spacious"), Density::Comfortable);
        assert_eq!(Density::parse("nonsense"), Density::Compact);
        assert_eq!(Density::Normal.as_str(), "normal");
        assert_eq!(Density::ALL.len(), 3);
    }

    #[test]
    fn u4_keys_round_trip_through_a_file() {
        let d = temp_dir("u4");
        let p = d.join("config.toml");
        let mut cfg = Config::default();
        cfg.general.show_hidden = true;
        cfg.general.show_system = true;
        cfg.general.confirm_delete = false;
        cfg.general.delete_to_recycle = false;
        cfg.appearance.theme = "nord".into();
        cfg.appearance.accent = Some("#f97316".into());
        cfg.appearance.font_scale = 1.25;
        cfg.updates.auto_check = false;
        cfg.updates.auto_install = true;
        cfg.advanced.diagnostics = true;
        cfg.keys.overrides.insert("tab.new".into(), vec!["Ctrl+N".into(), "Ctrl+T".into()]);
        cfg.keys.overrides.insert("tab.close".into(), vec![]);
        cfg.context_menu.entries = vec![
            MenuEntry { id: "builtin.copy".into(), source: Source::Builtin, name: "Copy".into(), hidden: true, ..Default::default() },
            MenuEntry { id: "custom.1".into(), kind: EntryKind::Submenu, name: "Tools".into(), ..Default::default() },
            MenuEntry {
                id: "custom.2".into(),
                name: "Open in VS Code".into(),
                command: r"C:\Code\Code.exe".into(),
                args: "--reuse-window \"{path}\"".into(),
                extensions: vec!["rs".into(), "toml".into()],
                show_for: ShowFor { files: true, folders: false, background: true },
                parent: "custom.1".into(),
                ..Default::default()
            },
        ];
        let cf = ConfigFile::new(cfg.clone());
        cf.save(&p).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.contains("[[context_menu.entries]]"), "{text}");
        let l = ConfigFile::load(&p);
        assert_eq!(l.status, LoadStatus::Loaded);
        assert_eq!(l.value.config, cfg);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn updates_check_is_an_alias_and_auto_install_is_off_by_default() {
        let c: Config = toml::from_str("[updates]
check = false
").unwrap();
        assert!(!c.updates.auto_check);
        assert!(!c.updates.auto_install);
        let c = Config::default();
        assert!(c.updates.auto_check && !c.updates.auto_install);
        assert!(c.context_menu.entries.is_empty() && !c.advanced.diagnostics);
    }

    #[test]
    fn unknown_menu_values_read_as_defaults_not_errors() {
        let c: Config = toml::from_str("[[context_menu.entries]]
id = \"x\"
kind = \"weird\"
source = \"alien\"
").unwrap();
        let e = &c.context_menu.entries[0];
        assert_eq!((e.kind, e.source), (EntryKind::Item, Source::Custom));
    }

    #[test]
    fn a_removed_key_override_does_not_come_back_from_the_old_file() {
        let d = temp_dir("keys-merge");
        let p = d.join("config.toml");
        fs::write(&p, "version = 1
[keys.overrides]
\"tab.new\" = [\"Ctrl+N\"]
\"nav.up\" = [\"Backspace\"]
").unwrap();
        let mut l = ConfigFile::load(&p);
        assert_eq!(l.value.config.keys.overrides.len(), 2);
        l.value.config.keys.overrides.remove("tab.new");
        l.value.save(&p).unwrap();
        let again = ConfigFile::load(&p);
        assert_eq!(again.value.config.keys.overrides.len(), 1, "{:?}", again.value.config.keys.overrides);
        assert!(again.value.config.keys.overrides.contains_key("nav.up"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_typical_file_loads_in_well_under_a_millisecond() {
        let d = temp_dir("speed");
        let p = d.join("config.toml");
        fs::write(&p, TYPICAL).unwrap();
        // warm up the page cache and the parser
        let _ = ConfigFile::load(&p);
        let n = 200;
        let t = Instant::now();
        for _ in 0..n {
            let l = ConfigFile::load(&p);
            assert_eq!(l.status, LoadStatus::Loaded);
        }
        let per = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        eprintln!("config load: {per:.4} ms per load of {} bytes", fs::metadata(&p).unwrap().len());
        assert!(per < 1.0, "a typical config must load in well under 1 ms, got {per} ms");
        let _ = fs::remove_dir_all(&d);
    }
}
