//! The plugin manifest (`plugin.toml`): who the plugin is, which entry points
//! it has and which capabilities it asks for. See `docs/wasm-plugins.md`.
//!
//! Parsing is strict: an unknown key, an unknown capability, a value outside
//! its range or a malformed id rejects the whole manifest. Nothing is granted
//! that is not written down, and what is written down is what the user is
//! shown at install.

use crate::caps::{self, Access, Capabilities, PathGrant, Protected, Scope};
use serde::Deserialize;
use std::fmt;
use std::path::Path;
use tsubame_ext_api::Align;

pub const MANIFEST_FILE: &str = "plugin.toml";
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// The WIT package version (major.minor) this build speaks: `tsubame:plugin@0.1.x`.
pub const API_VERSION: &str = "0.1";
/// Most entry points of one kind.
pub const MAX_ENTRIES: usize = 32;

/// The kinds of entry point a wasm plugin can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Column = 0,
    Previewer = 1,
    Vfs = 2,
    Search = 3,
    Command = 4,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Column, Kind::Previewer, Kind::Vfs, Kind::Search, Kind::Command];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Column => "column",
            Kind::Previewer => "previewer",
            Kind::Vfs => "vfs",
            Kind::Search => "search",
            Kind::Command => "command",
        }
    }

    pub fn from_u8(v: u8) -> Option<Kind> {
        Kind::ALL.get(v as usize).copied()
    }

    /// The kind's bit in a set of kinds.
    pub fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Where a path grant points before it is resolved for one machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// `$plugin`: the folder the plugin is installed in. Read only.
    Plugin,
    /// `$data`: the plugin's own data folder.
    Data,
    /// `$input`: the paths handed to the current call.
    Input,
    /// `*`: every path.
    Any,
    /// An absolute folder, normalized.
    Absolute(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredPath {
    pub location: Location,
    pub access: Access,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDecl {
    pub id: String,
    pub label: String,
    pub align: Align,
    /// Default width in DIPs.
    pub width: f32,
}

/// A previewer, vfs provider or search provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderDecl {
    pub id: String,
    pub title: String,
    /// Lower-case extensions without the dot. Empty for a search provider.
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandDecl {
    pub name: String,
    pub title: String,
    /// Default chords as text (`"Ctrl+Alt+E"`); the app parses them.
    pub keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// File name of the component inside the package (`plugin.wasm`).
    pub component: String,
    pub network: bool,
    pub run_processes: bool,
    pub max_memory_mb: u32,
    pub timeout_ms: u32,
    pub paths: Vec<DeclaredPath>,
    pub columns: Vec<ColumnDecl>,
    pub previewers: Vec<ProviderDecl>,
    pub vfs: Vec<ProviderDecl>,
    pub search: Vec<ProviderDecl>,
    pub commands: Vec<CommandDecl>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{MANIFEST_FILE}: {}", self.0)
    }
}

impl std::error::Error for ManifestError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ManifestError> {
    Err(ManifestError(msg.into()))
}

// ---- the file as written ----------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    plugin: RawPlugin,
    #[serde(default)]
    capabilities: RawCaps,
    #[serde(default)]
    column: Vec<RawColumn>,
    #[serde(default)]
    previewer: Vec<RawProvider>,
    #[serde(default)]
    vfs: Vec<RawProvider>,
    #[serde(default)]
    search: Vec<RawSearch>,
    #[serde(default)]
    command: Vec<RawCommand>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    id: String,
    name: String,
    version: String,
    api: String,
    component: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawCaps {
    #[serde(default)]
    network: bool,
    #[serde(default)]
    run_processes: bool,
    max_memory_mb: Option<i64>,
    timeout_ms: Option<i64>,
    #[serde(default)]
    paths: Vec<RawPath>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPath {
    path: String,
    access: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawColumn {
    id: String,
    label: String,
    align: Option<String>,
    width: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvider {
    id: String,
    title: String,
    extensions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSearch {
    id: String,
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    name: String,
    title: String,
    #[serde(default)]
    keys: Vec<String>,
}

// ---- checks ---------------------------------------------------------------------------------

fn is_segment(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

/// `com.example.exif`: 3 to 64 bytes, at least two dot-separated segments of
/// `a-z 0-9 -`. `tsubame.`, `core.` and `pack.` belong to the app.
fn check_plugin_id(id: &str) -> Result<(), ManifestError> {
    if !(3..=64).contains(&id.len()) || id.split('.').count() < 2 || !id.split('.').all(is_segment) {
        return err(format!("plugin.id `{id}` must be 3 to 64 characters of a-z, 0-9 and `-` in at least two dot-separated parts (`com.example.name`)"));
    }
    if ["tsubame.", "core.", "pack."].iter().any(|p| id.starts_with(p)) {
        return err(format!("plugin.id `{id}` uses a prefix that is reserved for Tsubame"));
    }
    // The id becomes a file name (`<id>-<version>.cwasm`) and a data folder name: on Windows
    // `nul.x-1.0.0.cwasm` is the NUL device (PH-11).
    if id.split('.').any(crate::caps::is_device_name) {
        return err(format!("plugin.id `{id}` has a part that is a reserved Windows device name (`con`, `nul`, `com1` ...)"));
    }
    Ok(())
}

/// An entry id or command name: 1 to 64 bytes of `a-z 0-9 . _ -`, starting and
/// ending with a letter or a digit.
fn check_entry_id(what: &str, id: &str) -> Result<(), ManifestError> {
    let b = id.as_bytes();
    let edge = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let ok = (1..=64).contains(&b.len()) && edge(b[0]) && edge(b[b.len() - 1]) && b.iter().all(|&c| edge(c) || matches!(c, b'.' | b'_' | b'-'));
    if ok { Ok(()) } else { err(format!("{what} `{id}` must be 1 to 64 characters of a-z, 0-9, `.`, `_` and `-`, starting and ending with a letter or a digit")) }
}

fn check_label(what: &str, s: &str, max: usize) -> Result<(), ManifestError> {
    if s.trim().is_empty() {
        return err(format!("{what} is empty"));
    }
    if s.chars().count() > max {
        return err(format!("{what} is longer than {max} characters"));
    }
    if s.chars().any(crate::sanitize::is_spoofing) {
        return err(format!("{what} has a control, invisible, direction-changing or private-use character"));
    }
    Ok(())
}

/// `1.2.3` or `1.2.3-beta.1`.
fn check_version(v: &str) -> Result<(), ManifestError> {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let nums: Vec<&str> = core.split('.').collect();
    let num_ok = |n: &&str| !n.is_empty() && n.len() <= 9 && n.bytes().all(|c| c.is_ascii_digit()) && (n.len() == 1 || !n.starts_with('0'));
    let pre_ok = pre.is_none_or(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-'));
    if v.len() > 32 || nums.len() != 3 || !nums.iter().all(num_ok) || !pre_ok {
        return err(format!("plugin.version `{v}` must look like `1.2.3` or `1.2.3-beta.1`"));
    }
    Ok(())
}

fn check_component(name: &str) -> Result<(), ManifestError> {
    if name.to_ascii_lowercase().ends_with(".cwasm") {
        return err(format!("plugin.component `{name}`: a package ships a `.wasm` component; `.cwasm` files are only ever written by the install step"));
    }
    let ok = name.len() <= 128
        && name.len() > ".wasm".len()
        && name.ends_with(".wasm")
        && !name.starts_with('.')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        && !name.contains("..");
    if ok { Ok(()) } else { err(format!("plugin.component `{name}` must be a plain file name ending in `.wasm` (letters, digits, `.`, `_`, `-`; no folders)")) }
}

fn check_extensions(what: &str, exts: &[String]) -> Result<(), ManifestError> {
    if exts.is_empty() {
        return err(format!("{what}: `extensions` is empty, so it would never be used"));
    }
    if exts.len() > 64 {
        return err(format!("{what}: more than 64 extensions"));
    }
    for (i, e) in exts.iter().enumerate() {
        let ok = (1..=16).contains(&e.len()) && e.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'+' | b'-'));
        if !ok {
            return err(format!("{what}: extension `{e}` must be 1 to 16 characters of a-z, 0-9, `_`, `+`, `-`, without the dot"));
        }
        if exts[..i].contains(e) {
            return err(format!("{what}: extension `{e}` is listed twice"));
        }
    }
    Ok(())
}

fn check_count(kind: Kind, n: usize) -> Result<(), ManifestError> {
    if n > MAX_ENTRIES { err(format!("{n} `{}` entries, at most {MAX_ENTRIES} are allowed", kind.name())) } else { Ok(()) }
}

fn check_unique<'a>(kind: Kind, ids: impl Iterator<Item = &'a str>) -> Result<(), ManifestError> {
    let mut seen: Vec<&str> = Vec::new();
    for id in ids {
        if seen.contains(&id) {
            return err(format!("{} `{id}` is declared twice", kind.name()));
        }
        seen.push(id);
    }
    Ok(())
}

fn range(what: &str, v: Option<i64>, default: u32, min: u32, max: u32) -> Result<u32, ManifestError> {
    match v {
        None => Ok(default),
        Some(v) if v >= i64::from(min) && v <= i64::from(max) => Ok(v as u32),
        Some(v) => err(format!("capabilities.{what} = {v} is outside {min}..={max}")),
    }
}

fn parse_path(p: &RawPath) -> Result<DeclaredPath, ManifestError> {
    let access = match p.access.as_str() {
        "read" => Access::Read,
        "read-write" => Access::ReadWrite,
        other => return err(format!("capabilities.paths: access `{other}` must be `read` or `read-write`")),
    };
    let location = match p.path.as_str() {
        "$plugin" => Location::Plugin,
        "$data" => Location::Data,
        "$input" => Location::Input,
        "*" => Location::Any,
        s if s.starts_with('$') => return err(format!("capabilities.paths: unknown location `{s}` (known: $plugin, $data, $input, *)")),
        s => match caps::normalize(s) {
            Ok(n) => Location::Absolute(n),
            Err(e) => return err(format!("capabilities.paths: `{s}`: {e}")),
        },
    };
    if location == Location::Plugin && access != Access::Read {
        return err("capabilities.paths: `$plugin` is read only");
    }
    Ok(DeclaredPath { location, access })
}

impl Manifest {
    /// Parses and checks a manifest. Any problem rejects all of it.
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        if text.len() as u64 > MAX_MANIFEST_BYTES {
            return err(format!("larger than {MAX_MANIFEST_BYTES} bytes"));
        }
        let raw: Raw = toml::from_str(text).map_err(|e| {
            // Not `e.to_string()`: that repeats the offending source line, unbounded and unescaped.
            let at = e.span().map(|s| format!(" (at byte {})", s.start)).unwrap_or_default();
            ManifestError(format!("{}{at}", crate::sanitize::sanitize(e.message(), 200)))
        })?;

        check_plugin_id(&raw.plugin.id)?;
        check_label("plugin.name", &raw.plugin.name, 80)?;
        check_version(&raw.plugin.version)?;
        if raw.plugin.api != API_VERSION {
            return err(format!("plugin.api `{}` is not supported (this build speaks `{API_VERSION}`)", raw.plugin.api));
        }
        check_component(&raw.plugin.component)?;
        if raw.plugin.description.chars().count() > 400 || raw.plugin.description.chars().any(|c| crate::sanitize::is_spoofing(c) && c != '\n') {
            return err("plugin.description is longer than 400 characters or has a control, invisible, direction-changing or private-use character");
        }

        let c = &raw.capabilities;
        let max_memory_mb = range("max_memory_mb", c.max_memory_mb, caps::DEFAULT_MEMORY_MB, caps::MIN_MEMORY_MB, caps::MAX_MEMORY_MB)?;
        let timeout_ms = range("timeout_ms", c.timeout_ms, caps::DEFAULT_TIMEOUT_MS, caps::MIN_TIMEOUT_MS, caps::MAX_TIMEOUT_MS)?;
        if c.paths.len() > caps::MAX_PATH_GRANTS {
            return err(format!("capabilities.paths has {} entries, at most {} are allowed", c.paths.len(), caps::MAX_PATH_GRANTS));
        }
        let mut paths: Vec<DeclaredPath> = Vec::with_capacity(c.paths.len());
        for p in &c.paths {
            let d = parse_path(p)?;
            let same = |o: &DeclaredPath| match (&o.location, &d.location) {
                (Location::Absolute(a), Location::Absolute(b)) => caps::key(a) == caps::key(b),
                (a, b) => a == b,
            };
            if paths.iter().any(same) {
                return err(format!("capabilities.paths: `{}` is listed twice", p.path));
            }
            paths.push(d);
        }
        // A folder on another computer is network access (PH-01): say so, or it is refused.
        if !c.network
            && let Some(p) = paths.iter().find_map(|d| match &d.location {
                Location::Absolute(p) if caps::is_unc(p) => Some(p),
                _ => None,
            })
        {
            return err(format!("capabilities.paths: `{p}` is on another computer; a network path needs `network = true`"));
        }

        let mut columns = Vec::with_capacity(raw.column.len());
        for r in &raw.column {
            check_entry_id("column id", &r.id)?;
            check_label(&format!("column `{}`: label", r.id), &r.label, 80)?;
            let align = match r.align.as_deref() {
                None | Some("left") => Align::Left,
                Some("right") => Align::Right,
                Some(other) => return err(format!("column `{}`: align `{other}` must be `left` or `right`", r.id)),
            };
            let width = r.width.unwrap_or(120.0);
            if !(20.0..=2000.0).contains(&width) {
                return err(format!("column `{}`: width {width} is outside 20..=2000", r.id));
            }
            columns.push(ColumnDecl { id: r.id.clone(), label: r.label.clone(), align, width: width as f32 });
        }

        let provider = |kind: Kind, r: &RawProvider| -> Result<ProviderDecl, ManifestError> {
            check_entry_id(&format!("{} id", kind.name()), &r.id)?;
            check_label(&format!("{} `{}`: title", kind.name(), r.id), &r.title, 80)?;
            check_extensions(&format!("{} `{}`", kind.name(), r.id), &r.extensions)?;
            Ok(ProviderDecl { id: r.id.clone(), title: r.title.clone(), extensions: r.extensions.clone() })
        };
        let previewers = raw.previewer.iter().map(|r| provider(Kind::Previewer, r)).collect::<Result<Vec<_>, _>>()?;
        let vfs = raw.vfs.iter().map(|r| provider(Kind::Vfs, r)).collect::<Result<Vec<_>, _>>()?;

        let mut search = Vec::with_capacity(raw.search.len());
        for r in &raw.search {
            check_entry_id("search id", &r.id)?;
            check_label(&format!("search `{}`: title", r.id), &r.title, 80)?;
            search.push(ProviderDecl { id: r.id.clone(), title: r.title.clone(), extensions: Vec::new() });
        }

        let mut commands = Vec::with_capacity(raw.command.len());
        for r in &raw.command {
            check_entry_id("command name", &r.name)?;
            check_label(&format!("command `{}`: title", r.name), &r.title, 80)?;
            if r.keys.len() > 8 {
                return err(format!("command `{}`: more than 8 keys", r.name));
            }
            for k in &r.keys {
                check_label(&format!("command `{}`: key", r.name), k, 40)?;
            }
            commands.push(CommandDecl { name: r.name.clone(), title: r.title.clone(), keys: r.keys.clone() });
        }

        check_count(Kind::Column, columns.len())?;
        check_count(Kind::Previewer, previewers.len())?;
        check_count(Kind::Vfs, vfs.len())?;
        check_count(Kind::Search, search.len())?;
        check_count(Kind::Command, commands.len())?;
        check_unique(Kind::Column, columns.iter().map(|c| c.id.as_str()))?;
        check_unique(Kind::Previewer, previewers.iter().map(|c| c.id.as_str()))?;
        check_unique(Kind::Vfs, vfs.iter().map(|c| c.id.as_str()))?;
        check_unique(Kind::Search, search.iter().map(|c| c.id.as_str()))?;
        check_unique(Kind::Command, commands.iter().map(|c| c.name.as_str()))?;
        if columns.is_empty() && previewers.is_empty() && vfs.is_empty() && search.is_empty() && commands.is_empty() {
            return err("no entry points: add at least one [[column]], [[previewer]], [[vfs]], [[search]] or [[command]]");
        }

        Ok(Manifest {
            id: raw.plugin.id,
            name: raw.plugin.name,
            version: raw.plugin.version,
            description: raw.plugin.description,
            component: raw.plugin.component,
            network: c.network,
            run_processes: c.run_processes,
            max_memory_mb,
            timeout_ms,
            paths,
            columns,
            previewers,
            vfs,
            search,
            commands,
        })
    }

    /// Reads `plugin.toml` from a plugin folder (at most [`MAX_MANIFEST_BYTES`],
    /// counted on what is read, not on a size looked up before).
    pub fn load(dir: &Path) -> Result<Manifest, ManifestError> {
        let bytes = crate::install_record::read_capped(&dir.join(MANIFEST_FILE), MAX_MANIFEST_BYTES).map_err(|e| ManifestError(format!("can not read: {e}")))?;
        let text = String::from_utf8(bytes).map_err(|_| ManifestError("not UTF-8".into()))?;
        Manifest::parse(&text)
    }

    /// The capabilities for this machine: `$plugin` and `$data` become the two
    /// folders given (absolute paths). A grant that reaches into a `protected`
    /// folder is refused (security review PH-02).
    pub fn capabilities(&self, plugin_dir: &str, data_dir: &str, protected: &Protected) -> Result<Capabilities, ManifestError> {
        let dir = |what: &str, p: &str| caps::normalize(p).map_err(|e| ManifestError(format!("{what} folder `{p}`: {e}")));
        let mut paths = Vec::with_capacity(self.paths.len());
        for d in &self.paths {
            let scope = match &d.location {
                Location::Plugin => Scope::Prefix(dir("plugin", plugin_dir)?),
                Location::Data => Scope::Prefix(dir("data", data_dir)?),
                Location::Input => Scope::Input,
                Location::Any => Scope::Any,
                Location::Absolute(p) => Scope::Prefix(p.clone()),
            };
            paths.push(PathGrant { scope, access: d.access });
        }
        let c = Capabilities { network: self.network, run_processes: self.run_processes, max_memory_mb: self.max_memory_mb, timeout_ms: self.timeout_ms, paths };
        c.validate().map_err(|e| ManifestError(e.to_string()))?;
        protected.check_grants(&c).map_err(|e| ManifestError(e.to_string()))?;
        Ok(c)
    }

    /// Every declared entry point: kind and id (for a command, its name).
    pub fn entrypoints(&self) -> Vec<(Kind, String)> {
        let mut out = Vec::new();
        out.extend(self.columns.iter().map(|c| (Kind::Column, c.id.clone())));
        out.extend(self.previewers.iter().map(|c| (Kind::Previewer, c.id.clone())));
        out.extend(self.vfs.iter().map(|c| (Kind::Vfs, c.id.clone())));
        out.extend(self.search.iter().map(|c| (Kind::Search, c.id.clone())));
        out.extend(self.commands.iter().map(|c| (Kind::Command, c.name.clone())));
        out
    }

    /// The name an entry point registers under: `<plugin id>.<entry id>`.
    pub fn qualified(&self, entry: &str) -> String {
        format!("{}.{entry}", self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[plugin]
id = "com.example.exif"
name = "EXIF tools"
version = "1.2.0"
api = "0.1"
component = "exif.wasm"
description = "Camera columns and a metadata preview."

[capabilities]
network = false
run_processes = false
max_memory_mb = 96
timeout_ms = 1500
paths = [
    { path = "$input", access = "read" },
    { path = "$plugin", access = "read" },
    { path = "$data", access = "read-write" },
    { path = "D:/Photos/Library", access = "read" },
]

[[column]]
id = "camera"
label = "Camera"
width = 140

[[column]]
id = "iso"
label = "ISO"
align = "right"

[[previewer]]
id = "meta"
title = "EXIF"
extensions = ["jpg", "jpeg", "heic"]

[[vfs]]
id = "zip"
title = "Zip archives"
extensions = ["zip"]

[[search]]
id = "by-camera"
title = "By camera"

[[command]]
name = "strip"
title = "Strip EXIF"
keys = ["Ctrl+Alt+E"]
"#;

    const MIN: &str = "[plugin]\nid = \"a.b\"\nname = \"N\"\nversion = \"0.1.0\"\napi = \"0.1\"\ncomponent = \"p.wasm\"\n[[column]]\nid = \"c\"\nlabel = \"C\"\n";

    #[test]
    fn full_manifest_parses() {
        let m = Manifest::parse(FULL).unwrap();
        assert_eq!(m.id, "com.example.exif");
        assert_eq!(m.version, "1.2.0");
        assert_eq!(m.component, "exif.wasm");
        assert_eq!((m.max_memory_mb, m.timeout_ms), (96, 1500));
        assert_eq!(m.paths.len(), 4);
        assert_eq!(m.paths[3], DeclaredPath { location: Location::Absolute("D:\\Photos\\Library".into()), access: Access::Read });
        assert_eq!(m.columns[0], ColumnDecl { id: "camera".into(), label: "Camera".into(), align: Align::Left, width: 140.0 });
        assert_eq!(m.columns[1].align, Align::Right);
        assert_eq!(m.columns[1].width, 120.0);
        assert_eq!(m.previewers[0].extensions, ["jpg", "jpeg", "heic"]);
        assert_eq!(m.commands[0].keys, ["Ctrl+Alt+E"]);
        assert_eq!(m.qualified("camera"), "com.example.exif.camera");
        assert_eq!(
            m.entrypoints(),
            vec![
                (Kind::Column, "camera".to_string()),
                (Kind::Column, "iso".to_string()),
                (Kind::Previewer, "meta".to_string()),
                (Kind::Vfs, "zip".to_string()),
                (Kind::Search, "by-camera".to_string()),
                (Kind::Command, "strip".to_string()),
            ]
        );

        let c = m.capabilities("C:\\Apps\\Tsubame\\plugins\\com.example.exif", "C:/Users/u/AppData/Roaming/Tsubame/plugin-data/com.example.exif", &Protected::default()).unwrap();
        assert_eq!(c.paths[0], PathGrant { scope: Scope::Input, access: Access::Read });
        assert_eq!(c.paths[1], PathGrant { scope: Scope::Prefix("C:\\Apps\\Tsubame\\plugins\\com.example.exif".into()), access: Access::Read });
        assert_eq!(c.paths[2], PathGrant { scope: Scope::Prefix("C:\\Users\\u\\AppData\\Roaming\\Tsubame\\plugin-data\\com.example.exif".into()), access: Access::ReadWrite });
        assert_eq!(c.paths[3].scope, Scope::Prefix("D:\\Photos\\Library".into()));
        assert!(m.capabilities("relative", "C:\\d", &Protected::default()).is_err());
    }

    #[test]
    fn minimal_manifest_denies_by_default() {
        let m = Manifest::parse(MIN).unwrap();
        assert!(!m.network && !m.run_processes);
        assert!(m.paths.is_empty());
        assert_eq!(m.max_memory_mb, caps::DEFAULT_MEMORY_MB);
        assert_eq!(m.timeout_ms, caps::DEFAULT_TIMEOUT_MS);
        assert_eq!(m.capabilities("C:\\p", "C:\\d", &Protected::default()).unwrap(), Capabilities::default());
    }

    /// `MIN` with one line replaced or a block appended.
    fn with(from: &str, to: &str) -> Result<Manifest, ManifestError> {
        assert!(from.is_empty() || MIN.contains(from), "{from}");
        Manifest::parse(&if from.is_empty() { format!("{MIN}{to}") } else { MIN.replace(from, to) })
    }

    fn rejects(from: &str, to: &str, needle: &str) {
        match with(from, to) {
            Ok(_) => panic!("accepted: {to}"),
            Err(e) => assert!(e.0.contains(needle), "{to:?} gave {:?}, wanted {needle:?}", e.0),
        }
    }

    #[test]
    fn rejects_unknown_keys_and_capabilities() {
        rejects("", "[capabilities]\nfilesystem = true\n", "unknown field `filesystem`");
        rejects("", "[capabilities]\nraw_sockets = true\n", "unknown field");
        rejects("", "[capabilities]\npaths = [{ path = \"$input\", access = \"read\", recursive = true }]\n", "unknown field `recursive`");
        rejects("", "[extra]\nx = 1\n", "unknown field `extra`");
        rejects("name = \"N\"", "name = \"N\"\nauthor = \"me\"", "unknown field `author`");
        rejects("label = \"C\"", "label = \"C\"\nsortable = true", "unknown field `sortable`");
        rejects("", "[[search]]\nid = \"s\"\ntitle = \"S\"\nextensions = [\"txt\"]\n", "unknown field `extensions`");
        rejects("", "[[panel]]\nid = \"s\"\n", "unknown field `panel`");
        // wrong types
        rejects("", "[capabilities]\nnetwork = \"yes\"\n", "invalid type");
        rejects("", "[capabilities]\nmax_memory_mb = \"64\"\n", "invalid type");
    }

    #[test]
    fn a_network_folder_needs_the_network_capability() {
        rejects("", "[capabilities]\npaths = [{ path = \"//nas/share\", access = \"read\" }]\n", "needs `network = true`");
        rejects("", "[capabilities]\nnetwork = false\npaths = [{ path = \"\\\\\\\\?\\\\UNC\\\\nas\\\\share\\\\x\", access = \"read\" }]\n", "on another computer");
        rejects("", "[capabilities]\nnetwork = true\npaths = [{ path = \"//nas@80/dav\", access = \"read\" }]\n", "not allowed");
        let m = with("", "[capabilities]\nnetwork = true\npaths = [{ path = \"//nas/share\", access = \"read\" }]\n").unwrap();
        let c = m.capabilities("C:\\p", "C:\\d", &Protected::default()).unwrap();
        assert_eq!(c.summary()[0], "Read files on the server nas: \\\\nas\\share");
        // `$data` and `$plugin` on a share are network paths too
        let m = with("", "[capabilities]\npaths = [{ path = \"$data\", access = \"read-write\" }]\n").unwrap();
        assert!(m.capabilities("C:\\p", "\\\\nas\\profiles\\data", &Protected::default()).unwrap_err().0.contains("needs `network = true`"));
    }

    #[test]
    fn rejects_out_of_range_limits() {
        rejects("", "[capabilities]\nmax_memory_mb = 0\n", "max_memory_mb = 0 is outside");
        rejects("", "[capabilities]\nmax_memory_mb = 513\n", "max_memory_mb = 513 is outside 1..=512");
        rejects("", "[capabilities]\nmax_memory_mb = -1\n", "outside");
        rejects("", "[capabilities]\ntimeout_ms = 9\n", "timeout_ms = 9 is outside");
        rejects("", "[capabilities]\ntimeout_ms = 30001\n", "timeout_ms = 30001 is outside 10..=30000");
        assert_eq!(with("", "[capabilities]\nmax_memory_mb = 512\ntimeout_ms = 30000\n").unwrap().max_memory_mb, 512);
        assert_eq!(with("", "[capabilities]\nmax_memory_mb = 1\ntimeout_ms = 10\n").unwrap().timeout_ms, 10);
    }

    #[test]
    fn rejects_bad_paths() {
        let p = |path: &str, access: &str| format!("[capabilities]\npaths = [{{ path = '{path}', access = \"{access}\" }}]\n");
        rejects("", &p("$home", "read"), "unknown location `$home`");
        rejects("", &p("$plugin", "read-write"), "`$plugin` is read only");
        rejects("", &p("$input", "write"), "access `write`");
        rejects("", &p("$input", "rw"), "access `rw`");
        rejects("", &p("relative\\dir", "read"), "not an absolute path");
        rejects("", &p("C:\\a\\..\\b", "read"), "`..`");
        rejects("", &p("C:\\a\\x:stream", "read"), "not allowed");
        rejects("", &p("\\\\.\\PhysicalDrive0", "read"), "device");
        rejects("", &p("C:\\a\\NUL", "read"), "device");
        rejects("", "[capabilities]\npaths = [{ path = \"$input\" }]\n", "missing field `access`");
        rejects("", "[capabilities]\npaths = [{ path = \"$input\", access = \"read\" }, { path = \"$input\", access = \"read-write\" }]\n", "listed twice");
        rejects("", "[capabilities]\npaths = [{ path = 'C:\\A', access = \"read\" }, { path = 'c:/a/', access = \"read\" }]\n", "listed twice");
        let many: String = (0..33).map(|i| format!("{{ path = 'C:\\d{i}', access = \"read\" }},")).collect();
        rejects("", &format!("[capabilities]\npaths = [{many}]\n"), "at most 32");
        // the wide grants are valid, and are what the user is shown
        let m = with("", &p("*", "read-write")).unwrap();
        assert_eq!(m.paths[0], DeclaredPath { location: Location::Any, access: Access::ReadWrite });
        assert_eq!(m.capabilities("C:\\p", "C:\\d", &Protected::default()).unwrap().summary()[0], "Read and change any file on this computer: full control of your files");
    }

    #[test]
    fn rejects_bad_identity() {
        for id in ["single", "Com.Example", "a..b", ".a.b", "a.b.", "a.-b", "a.b-", "a b.c", "tsubame.core", "core.x", "pack.mine", "ab", &"a.".repeat(40)] {
            rejects("id = \"a.b\"", &format!("id = \"{id}\""), "plugin.id");
        }
        for v in ["1", "1.2", "1.2.3.4", "v1.2.3", "1.02.3", "1.2.x", "1.2.3-", "1.2.3+build", ""] {
            rejects("version = \"0.1.0\"", &format!("version = \"{v}\""), "plugin.version");
        }
        assert!(with("version = \"0.1.0\"", "version = \"10.20.30-rc.1\"").is_ok());
        rejects("api = \"0.1\"", "api = \"0.2\"", "plugin.api `0.2` is not supported");
        rejects("api = \"0.1\"", "api = \"1\"", "not supported");
        rejects("name = \"N\"", "name = \"  \"", "plugin.name is empty");
        rejects("name = \"N\"", &format!("name = \"{}\"", "x".repeat(81)), "longer than 80");
        rejects("name = \"N\"", "name = \"a\\u0007b\"", "control, invisible");
        // bidi override, zero width space, line separator, private use (PH-11)
        for c in ["202e", "200b", "2028", "e000"] {
            rejects("name = \"N\"", &format!("name = \"a\\u{c}b\""), "plugin.name has a control, invisible");
        }
        // an id part that is a Windows device name (PH-11)
        for id in ["nul.x", "x.con", "com1.tools", "a.lpt9.b", "aux.x"] {
            rejects("id = \"a.b\"", &format!("id = \"{id}\""), "reserved Windows device name");
        }
        assert!(with("id = \"a.b\"", "id = \"console.com10\"").is_ok());
        rejects("api = \"0.1\"\n", "", "missing field `api`");
        rejects("id = \"a.b\"\n", "", "missing field `id`");
    }

    #[test]
    fn rejects_bad_component() {
        for c in ["p.cwasm", "P.CWASM"] {
            rejects("component = \"p.wasm\"", &format!("component = \"{c}\""), "only ever written by the install step");
        }
        for c in ["../p.wasm", "sub/p.wasm", "sub\\\\p.wasm", "C:/p.wasm", "p.dll", ".wasm", ".hidden.wasm", "p..wasm", "p .wasm", "", "p.wasm:s"] {
            rejects("component = \"p.wasm\"", &format!("component = \"{c}\""), "plugin.component");
        }
    }

    #[test]
    fn rejects_bad_entry_points() {
        rejects("[[column]]\nid = \"c\"\nlabel = \"C\"\n", "", "no entry points");
        rejects("id = \"c\"", "id = \"C\"", "column id `C`");
        rejects("id = \"c\"", "id = \"-c\"", "column id");
        rejects("id = \"c\"", "id = \"\"", "column id");
        rejects("label = \"C\"", "label = \"\"", "label is empty");
        rejects("label = \"C\"", "label = \"C\"\nalign = \"center\"", "align `center`");
        rejects("label = \"C\"", "label = \"C\"\nwidth = 5", "width 5 is outside");
        rejects("label = \"C\"", "label = \"C\"\nwidth = 5000", "outside 20..=2000");
        rejects("", "[[column]]\nid = \"c\"\nlabel = \"Again\"\n", "column `c` is declared twice");
        rejects("", "[[previewer]]\nid = \"p\"\ntitle = \"P\"\nextensions = []\n", "`extensions` is empty");
        rejects("", "[[previewer]]\nid = \"p\"\ntitle = \"P\"\n", "missing field `extensions`");
        rejects("", "[[previewer]]\nid = \"p\"\ntitle = \"P\"\nextensions = [\".jpg\"]\n", "extension `.jpg`");
        rejects("", "[[previewer]]\nid = \"p\"\ntitle = \"P\"\nextensions = [\"JPG\"]\n", "extension `JPG`");
        rejects("", "[[vfs]]\nid = \"z\"\ntitle = \"Z\"\nextensions = [\"zip\", \"zip\"]\n", "listed twice");
        rejects("", "[[command]]\nname = \"Run It\"\ntitle = \"R\"\n", "command name `Run It`");
        rejects("", "[[command]]\nname = \"r\"\ntitle = \"R\"\nkeys = [\"\"]\n", "key is empty");
        rejects("", "[[command]]\nname = \"r\"\ntitle = \"R\"\n[[command]]\nname = \"r\"\ntitle = \"R2\"\n", "command `r` is declared twice");
        let many: String = (0..33).map(|i| format!("[[search]]\nid = \"s{i}\"\ntitle = \"S\"\n")).collect();
        rejects("", &many, "33 `search` entries, at most 32");
        // the same id under two kinds is fine: ids are unique per kind
        assert!(with("", "[[search]]\nid = \"c\"\ntitle = \"S\"\n").is_ok());
    }

    #[test]
    fn rejects_malformed_and_oversized_files() {
        assert!(Manifest::parse("not toml at all [").is_err());
        assert!(Manifest::parse("").unwrap_err().0.contains("missing field `plugin`"));
        let big = format!("{MIN}# {}\n", "x".repeat(MAX_MANIFEST_BYTES as usize));
        assert!(Manifest::parse(&big).unwrap_err().0.contains("larger than"));
        assert_eq!(ManifestError("x".into()).to_string(), "plugin.toml: x");
    }

    #[test]
    fn load_reads_the_plugin_folder() {
        let dir = std::env::temp_dir().join(format!("tsubame-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Manifest::load(&dir).unwrap_err().0.contains("can not read"));
        std::fs::write(dir.join(MANIFEST_FILE), MIN).unwrap();
        assert_eq!(Manifest::load(&dir).unwrap().id, "a.b");
        std::fs::write(dir.join(MANIFEST_FILE), vec![b'#'; MAX_MANIFEST_BYTES as usize + 1]).unwrap();
        assert!(Manifest::load(&dir).unwrap_err().0.contains("larger than"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn kinds_round_trip() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_u8(k as u8), Some(k));
        }
        assert_eq!(Kind::from_u8(5), None);
        assert_eq!(Kind::ALL.iter().fold(0, |a, k| a | k.bit()), 0b11111);
    }
}
