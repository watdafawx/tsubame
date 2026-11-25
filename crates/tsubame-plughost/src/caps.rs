//! Capabilities: what a plugin may touch, and the path rules that enforce it.
//!
//! Everything is denied unless a grant allows it. This module is the lexical
//! half of the path check (pure string work, no I/O); the host then opens the
//! path one component at a time from the grant's folder and checks the handle
//! it got, so a link inside a granted folder can not lead out of it
//! (`host/fsopen.rs`).
//!
//! Network paths (`\\server\share\...`) are files on another computer: opening
//! one makes Windows connect to that server. A UNC path is only ever allowed
//! under an `$input` path that is itself UNC (the user is on that share), or
//! under a UNC folder grant when the plugin also has `network = true`. `*`
//! never covers one.

use std::fmt;

pub const DEFAULT_MEMORY_MB: u32 = 64;
pub const MIN_MEMORY_MB: u32 = 1;
pub const MAX_MEMORY_MB: u32 = 512;
pub const DEFAULT_TIMEOUT_MS: u32 = 2_000;
pub const MIN_TIMEOUT_MS: u32 = 10;
pub const MAX_TIMEOUT_MS: u32 = 30_000;
/// Most path grants one plugin may hold.
pub const MAX_PATH_GRANTS: usize = 32;
/// Longest path accepted, in bytes (a Windows path is at most 32,767 UTF-16 units).
pub const MAX_PATH_BYTES: usize = 32_767 * 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    Read,
    ReadWrite,
}

impl Access {
    /// True when a grant of `self` is enough for `want`.
    pub fn covers(self, want: Access) -> bool {
        self >= want
    }
}

/// Which paths a grant covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// This folder (normalized, see [`normalize`]) and everything under it.
    Prefix(String),
    /// The paths handed to the current call. What a call hands over depends
    /// on the entry point (see [`INPUT_SCOPE`]): a column or a preview gets only
    /// the path itself, a command its selection and the folders under it, a
    /// search the folder searched and everything under it.
    Input,
    /// Every path.
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathGrant {
    pub scope: Scope,
    pub access: Access,
}

/// What one plugin is allowed. `Default` is the smallest set: no paths, no
/// network, no processes, the default memory and time limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub network: bool,
    pub run_processes: bool,
    pub max_memory_mb: u32,
    pub timeout_ms: u32,
    pub paths: Vec<PathGrant>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities { network: false, run_processes: false, max_memory_mb: DEFAULT_MEMORY_MB, timeout_ms: DEFAULT_TIMEOUT_MS, paths: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapError {
    Memory(u32),
    Timeout(u32),
    TooManyPaths(usize),
    /// A prefix grant that is not a normalized absolute path.
    BadPrefix(String),
    Duplicate(String),
    /// A prefix grant on a network share (UNC) without `network = true`.
    NetworkPath(String),
    /// A grant that reaches into one of Tsubame's own folders ([`Protected`]).
    Protected(String),
}

impl fmt::Display for CapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CapError::Memory(v) => write!(f, "max_memory_mb = {v} is outside {MIN_MEMORY_MB}..={MAX_MEMORY_MB}"),
            CapError::Timeout(v) => write!(f, "timeout_ms = {v} is outside {MIN_TIMEOUT_MS}..={MAX_TIMEOUT_MS}"),
            CapError::TooManyPaths(n) => write!(f, "{n} path grants, at most {MAX_PATH_GRANTS} are allowed"),
            CapError::BadPrefix(p) => write!(f, "path grant `{p}` is not a normalized absolute path"),
            CapError::Duplicate(p) => write!(f, "path grant `{p}` is listed twice"),
            CapError::NetworkPath(p) => write!(f, "path grant `{p}` is on another computer; a network path needs `network = true`"),
            CapError::Protected(p) => write!(f, "path grant `{p}` reaches into a folder of Tsubame's own (its settings, the plugin's package or the compiled plugins)"),
        }
    }
}

impl std::error::Error for CapError {}

impl Capabilities {
    /// Checks the limits and the grants. The manifest parser runs this, and the
    /// host runs it again on what the client sends.
    pub fn validate(&self) -> Result<(), CapError> {
        if !(MIN_MEMORY_MB..=MAX_MEMORY_MB).contains(&self.max_memory_mb) {
            return Err(CapError::Memory(self.max_memory_mb));
        }
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(CapError::Timeout(self.timeout_ms));
        }
        if self.paths.len() > MAX_PATH_GRANTS {
            return Err(CapError::TooManyPaths(self.paths.len()));
        }
        let mut seen: Vec<String> = Vec::with_capacity(self.paths.len());
        for g in &self.paths {
            let k = match &g.scope {
                Scope::Prefix(p) => {
                    if normalize(p).as_deref() != Ok(p.as_str()) {
                        return Err(CapError::BadPrefix(p.clone()));
                    }
                    if is_unc(p) && !self.network {
                        return Err(CapError::NetworkPath(p.clone()));
                    }
                    key(p)
                }
                Scope::Input => "$input".to_string(),
                Scope::Any => "*".to_string(),
            };
            if seen.contains(&k) {
                return Err(CapError::Duplicate(k));
            }
            seen.push(k);
        }
        Ok(())
    }

    /// One line per thing the plugin may do, for the install prompt.
    pub fn summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        for g in &self.paths {
            let verb = if g.access == Access::ReadWrite { "Read and change" } else { "Read" };
            out.push(match &g.scope {
                Scope::Any if g.access == Access::ReadWrite => "Read and change any file on this computer: full control of your files".to_string(),
                Scope::Any => format!("{verb} any file on this computer"),
                Scope::Input => format!("{verb} {INPUT_SCOPE}"),
                Scope::Prefix(p) if is_unc(p) => format!("{verb} files on the server {}: {p}", unc_server(p)),
                Scope::Prefix(p) => format!("{verb} files in {p}"),
            });
        }
        if self.network {
            out.push("Use the network (reserved: no network calls exist yet; only lets folder grants on network shares work)".to_string());
        }
        if self.run_processes {
            out.push("Start other programs (reserved: not possible yet)".to_string());
        }
        out.push(format!("Use up to {} MB of memory and {} ms per call", self.max_memory_mb, self.timeout_ms));
        out
    }
}

/// What an `$input` grant reaches, in the words of the install prompt.
pub const INPUT_SCOPE: &str = "the files you use it on: for a column or a preview only that file or folder (a folder's \
names, not what is in them), for a command what you selected (with everything in selected folders), for a search \
the folder searched (with everything in it)";

// ---- path normalization --------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    /// Empty, too long, or not `X:\...` / `\\server\share\...`.
    NotAbsolute,
    /// A `.` or `..` component.
    Traversal,
    /// A character Windows does not allow in a name, a stream (`name:stream`),
    /// a trailing dot or space, or a control character.
    BadName,
    /// A device name (`NUL`, `COM1`, ...) or a device path (`\\.\`, `\\?\Volume{}`).
    Device,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathError::NotAbsolute => "not an absolute path",
            PathError::Traversal => "`.` and `..` are not allowed",
            PathError::BadName => "a name has a character that is not allowed",
            PathError::Device => "device paths are not allowed",
        })
    }
}

impl std::error::Error for PathError {}

fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
}

pub(crate) fn is_device_name(component: &str) -> bool {
    // Windows looks at the part before the first dot, ignoring trailing spaces: `NUL.txt` is NUL.
    let stem = component.split('.').next().unwrap_or("").trim_end_matches(' ');
    const NAMES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    if NAMES.iter().any(|n| stem.eq_ignore_ascii_case(n)) {
        return true;
    }
    // COM0-COM9 and LPT0-LPT9, and the superscript digits Windows also reserves (`COM¹`).
    let (Some(head), Some(digit)) = (stem.get(..3), stem.get(3..)) else { return false };
    (head.eq_ignore_ascii_case("COM") || head.eq_ignore_ascii_case("LPT"))
        && matches!(digit, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}" | "\u{b3}")
}

fn check_component(c: &str) -> Result<(), PathError> {
    if c == "." || c == ".." {
        return Err(PathError::Traversal);
    }
    if c.chars().any(|ch| (ch as u32) < 0x20 || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
        return Err(PathError::BadName);
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return Err(PathError::BadName);
    }
    if is_device_name(c) {
        return Err(PathError::Device);
    }
    Ok(())
}

/// The one spelling of an absolute Windows path that the checks compare:
/// backslashes, no empty, `.` or `..` components, no trailing separator,
/// `C:\dir\file` or `\\server\share\dir`. A drive root is `C:\`.
///
/// `\\?\C:\x` and `\\?\UNC\server\share\x` are accepted and lose their prefix.
/// Every other `\\?\` or `\\.\` path, a relative or drive-relative path
/// (`C:x`), a stream name (`file:stream`), a device name and a name ending in
/// a dot or a space are refused.
pub fn normalize(path: &str) -> Result<String, PathError> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return Err(PathError::NotAbsolute);
    }
    let p = path.replace('/', "\\");
    let (unc, rest) = if let Some(r) = strip_ci(&p, "\\\\?\\UNC\\") {
        (true, r)
    } else if let Some(r) = p.strip_prefix("\\\\?\\") {
        // Only the drive form is a file path; `\\?\Volume{..}` and `\\?\GLOBALROOT` are not.
        if !is_drive(r) {
            return Err(PathError::Device);
        }
        (false, r)
    } else if p.starts_with("\\\\.\\") {
        return Err(PathError::Device);
    } else if let Some(r) = p.strip_prefix("\\\\") {
        (true, r)
    } else {
        (false, p.as_str())
    };

    let mut out = String::with_capacity(rest.len() + 2);
    let body = if unc {
        rest
    } else {
        if !is_drive(rest) {
            return Err(PathError::NotAbsolute);
        }
        out.push(rest.as_bytes()[0].to_ascii_uppercase() as char);
        out.push(':');
        &rest[2..]
    };
    let mut comps = body.split('\\').filter(|c| !c.is_empty());
    if unc {
        let (Some(server), Some(share)) = (comps.next(), comps.next()) else { return Err(PathError::NotAbsolute) };
        check_component(server)?;
        // `\\host@80\...` and `\\host@SSL\...` go to the WebDAV redirector, which is HTTP.
        if server.contains('@') {
            return Err(PathError::BadName);
        }
        check_component(share)?;
        out.push_str("\\\\");
        out.push_str(server);
        out.push('\\');
        out.push_str(share);
    }

    let mut any = false;
    for c in comps {
        check_component(c)?;
        out.push('\\');
        out.push_str(c);
        any = true;
    }
    if !unc && !any {
        out.push('\\');
    }
    Ok(out)
}

/// True for a normalized network path (`\\server\share...`).
pub fn is_unc(normalized: &str) -> bool {
    normalized.starts_with("\\\\")
}

/// The server of a normalized UNC path.
fn unc_server(normalized: &str) -> &str {
    normalized[2..].split('\\').next().unwrap_or("")
}

/// `X:\` at the start (so not the drive-relative `X:file`).
fn is_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
}

/// The form two normalized paths are compared in. Only ASCII letters are
/// folded: Windows folds more, so a name that differs only in the case of a
/// non-ASCII letter is treated as a different name and refused, never the
/// other way round.
pub fn key(normalized: &str) -> String {
    normalized.to_ascii_lowercase()
}

/// True when `path` is `root` or lies under it. Both are [`key`]s.
pub fn is_under(path: &str, root: &str) -> bool {
    match path.strip_prefix(root) {
        Some("") => true,
        Some(rest) => root.ends_with('\\') || rest.starts_with('\\'),
        None => false,
    }
}

/// The parent of a [`key`], or None at a drive root or a share root.
fn parent(k: &str) -> Option<&str> {
    if k.ends_with('\\') {
        return None;
    }
    let i = k.rfind('\\')?;
    if k.starts_with("\\\\") {
        // `\\server\share` is the root: its parent would be `\\server`.
        return (k[..i].matches('\\').count() >= 3).then(|| &k[..i]);
    }
    Some(if i == 2 { &k[..3] } else { &k[..i] })
}

// ---- protected folders -----------------------------------------------------------------

/// Folders a plugin may never change, whatever it was granted (security
/// review PH-02): Tsubame's own folders, the plugin's package, the cache of
/// compiled components. Some of them are also **sealed**: the plugin may not
/// even look into them (the cache, which holds the `.cwasm` images and the
/// install records). The host checks the final path of every handle it opens
/// against both lists; [`Protected::check_grants`] refuses grants that would
/// reach into them before anything runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Protected {
    /// Normalized folders.
    no_write: Vec<String>,
    /// Keys of `no_write` plus `sealed`.
    no_write_keys: Vec<String>,
    sealed_keys: Vec<String>,
}

impl Protected {
    /// `no_write` and `sealed` are absolute folders, ideally real paths (links
    /// resolved): the host compares them with the final path of what it opens.
    /// A sealed folder is also not writable.
    pub fn new(no_write: &[String], sealed: &[String]) -> Result<Protected, PathError> {
        let mut p = Protected::default();
        for s in sealed {
            let n = normalize(s)?;
            p.sealed_keys.push(key(&n));
            if !p.no_write.iter().any(|w| key(w) == key(&n)) {
                p.no_write.push(n);
            }
        }
        for w in no_write {
            let n = normalize(w)?;
            if !p.no_write.iter().any(|x| key(x) == key(&n)) {
                p.no_write.push(n);
            }
        }
        p.no_write_keys = p.no_write.iter().map(|n| key(n)).collect();
        Ok(p)
    }

    /// The folders that may not be written (sealed ones included), normalized.
    pub fn no_write(&self) -> &[String] {
        &self.no_write
    }

    /// True when a plugin may not open the file whose final path is `k` (a
    /// [`key`]) for writing (`write`), or at all.
    pub fn denies(&self, k: &str, write: bool) -> bool {
        self.sealed_keys.iter().any(|s| is_under(k, s)) || (write && self.no_write_keys.iter().any(|w| is_under(k, w)))
    }

    /// Refuses a folder grant that reaches into a protected folder: a
    /// read-write one that is inside or above any of them, and any one that is
    /// inside or above a sealed folder. `*` can not be refused here; the host
    /// refuses each path under it instead.
    pub fn check_grants(&self, caps: &Capabilities) -> Result<(), CapError> {
        let overlaps = |a: &str, b: &str| is_under(a, b) || is_under(b, a);
        for g in &caps.paths {
            let Scope::Prefix(p) = &g.scope else { continue };
            let k = key(p);
            if self.sealed_keys.iter().any(|s| overlaps(&k, s)) || (g.access == Access::ReadWrite && self.no_write_keys.iter().any(|w| overlaps(&k, w))) {
                return Err(CapError::Protected(p.clone()));
            }
        }
        Ok(())
    }
}

// ---- the policy ------------------------------------------------------------------------

/// A folder that allows a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Root {
    /// An "any path" grant: there is no folder to stay inside, but the path
    /// must stay on a local drive.
    Any,
    /// A folder grant: the normalized folder the path must stay inside. It may
    /// resolve to a network path only when the plugin has `network = true`.
    Dir(String),
    /// An input path of the current call that covers the path: the path must
    /// stay inside it. The user handed it over, so it may be on a network share.
    Input(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// The path itself is refused, see [`normalize`].
    Invalid(PathError),
    /// No grant covers the path with the access asked for.
    NotGranted,
}

/// The lexical path check for one plugin.
pub struct PathPolicy {
    /// key, normalized root, access
    prefixes: Vec<(String, String, Access)>,
    network: bool,
    input: Option<Access>,
    any: Option<Access>,
    /// The current call's paths as handed over; turned into `inputs` on first use.
    pending: Vec<(String, bool)>,
    /// key, normalized path, recursive; sorted by key.
    inputs: Vec<(String, String, bool)>,
}

impl PathPolicy {
    pub fn new(caps: &Capabilities) -> PathPolicy {
        let mut p = PathPolicy { prefixes: Vec::new(), network: caps.network, input: None, any: None, pending: Vec::new(), inputs: Vec::new() };
        for g in &caps.paths {
            match &g.scope {
                Scope::Prefix(root) => {
                    // A prefix that does not normalize grants nothing.
                    if let Ok(n) = normalize(root) {
                        p.prefixes.push((key(&n), n, g.access));
                    }
                }
                Scope::Input => p.input = Some(p.input.map_or(g.access, |a| a.max(g.access))),
                Scope::Any => p.any = Some(p.any.map_or(g.access, |a| a.max(g.access))),
            }
        }
        p
    }

    /// True when some grant refers to the call's paths.
    pub fn uses_inputs(&self) -> bool {
        self.input.is_some()
    }

    /// Replaces the paths of the current call. Each is `(path, recursive)`: a
    /// recursive input covers the path and everything under it, a flat one only
    /// the path itself (a folder can then be looked at and listed, but nothing
    /// in it opened). Cheap: they are only looked at if the plugin touches a file.
    pub fn set_inputs(&mut self, paths: Vec<(String, bool)>) {
        self.pending = paths;
        self.inputs.clear();
    }

    fn settle(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        for (p, recursive) in self.pending.drain(..) {
            if let Ok(n) = normalize(&p) {
                self.inputs.push((key(&n), n, recursive));
            }
        }
        // The same path twice: the recursive one sorts first and is kept.
        self.inputs.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)));
        self.inputs.dedup_by(|a, b| a.0 == b.0);
    }

    /// Normalizes `path` and returns it with every root that allows `want` on it.
    /// The caller must still check the resolved path against one of the roots.
    pub fn check(&mut self, path: &str, want: Access) -> Result<(String, Vec<Root>), Denied> {
        let norm = normalize(path).map_err(Denied::Invalid)?;
        let k = key(&norm);
        let unc = is_unc(&norm);
        let mut roots = Vec::new();
        // `*` is "any file on this computer": never a network path.
        if !unc && self.any.is_some_and(|a| a.covers(want)) {
            roots.push(Root::Any);
        }
        for (rk, root, access) in &self.prefixes {
            if access.covers(want) && is_under(&k, rk) && (!unc || self.network) {
                roots.push(Root::Dir(root.clone()));
            }
        }
        // A UNC path can only meet an input that is itself UNC: `parent` stops at the share root.
        // The path itself may be any input; a folder above it only a recursive one.
        if self.input.is_some_and(|a| a.covers(want)) {
            self.settle();
            let mut at = Some(k.as_str());
            let mut exact = true;
            while let Some(a) = at {
                if let Ok(i) = self.inputs.binary_search_by(|e| e.0.as_str().cmp(a)) {
                    let (_, n, recursive) = &self.inputs[i];
                    if exact || *recursive {
                        roots.push(Root::Input(n.clone()));
                        break;
                    }
                }
                at = parent(a);
                exact = false;
            }
        }
        if roots.is_empty() { Err(Denied::NotGranted) } else { Ok((norm, roots)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_and_cleans() {
        assert_eq!(normalize("C:\\Users\\a\\file.txt").unwrap(), "C:\\Users\\a\\file.txt");
        assert_eq!(normalize("c:/Users//a/").unwrap(), "C:\\Users\\a");
        assert_eq!(normalize("C:\\").unwrap(), "C:\\");
        assert_eq!(normalize("C:/").unwrap(), "C:\\");
        assert_eq!(normalize("\\\\?\\C:\\x\\y").unwrap(), "C:\\x\\y");
        assert_eq!(normalize("\\\\server\\share").unwrap(), "\\\\server\\share");
        assert_eq!(normalize("\\\\server\\share\\").unwrap(), "\\\\server\\share");
        assert_eq!(normalize("//server/share/a//b").unwrap(), "\\\\server\\share\\a\\b");
        assert_eq!(normalize("\\\\?\\UNC\\server\\share\\a").unwrap(), "\\\\server\\share\\a");
        assert_eq!(normalize("\\\\?\\unc\\server\\share\\a").unwrap(), "\\\\server\\share\\a");
        assert_eq!(normalize("C:\\a.b\\..c\\d.").err(), Some(PathError::BadName));
        assert_eq!(normalize("C:\\a\\.hidden\\..x").unwrap(), "C:\\a\\.hidden\\..x");
        assert_eq!(normalize("C:\\日本\\ファイル.txt").unwrap(), "C:\\日本\\ファイル.txt");
    }

    #[test]
    fn normalize_rejects() {
        use PathError::*;
        for (p, want) in [
            ("", NotAbsolute),
            ("file.txt", NotAbsolute),
            ("\\Windows\\x", NotAbsolute),
            ("C:", NotAbsolute),
            ("C:file.txt", NotAbsolute),
            ("\\\\server", NotAbsolute),
            ("\\\\server\\", NotAbsolute),
            ("C:\\a\\..\\b", Traversal),
            ("C:\\a\\.\\b", Traversal),
            ("C:\\a\\..", Traversal),
            ("C:/a/../../Windows", Traversal),
            ("\\\\server\\share\\..\\other", Traversal),
            ("C:\\a\\file.txt:stream", BadName),
            ("C:\\a\\file.txt::$DATA", BadName),
            ("C:\\a\\b*", BadName),
            ("C:\\a\\b?", BadName),
            ("C:\\a\\b\"c", BadName),
            ("C:\\a\\b|c", BadName),
            ("C:\\a\\b<c", BadName),
            ("C:\\a\\b\u{1}c", BadName),
            ("C:\\a\\b\0c", BadName),
            ("C:\\a\\name.", BadName),
            ("C:\\a\\name ", BadName),
            ("C:\\a\\NUL", Device),
            ("C:\\a\\nul.txt", Device),
            ("C:\\a\\COM1", Device),
            ("C:\\a\\lpt9.log", Device),
            ("C:\\a\\CON", Device),
            ("C:\\a\\conin$", Device),
            ("\\\\.\\PhysicalDrive0", Device),
            ("\\\\.\\C:\\x", Device),
            ("\\\\?\\Volume{0000}\\x", Device),
            ("\\\\?\\GLOBALROOT\\Device\\x", Device),
        ] {
            assert_eq!(normalize(p), Err(want), "{p:?}");
        }
        assert_eq!(normalize(&format!("C:\\{}", "a".repeat(MAX_PATH_BYTES))), Err(NotAbsolute));
        // names that only look like devices are fine
        assert!(normalize("C:\\a\\COM10").is_ok());
        assert!(normalize("C:\\a\\console.txt").is_ok());
        assert!(normalize("C:\\a\\null").is_ok());
    }

    #[test]
    fn under_needs_a_component_boundary() {
        assert!(is_under("c:\\data", "c:\\data"));
        assert!(is_under("c:\\data\\x", "c:\\data"));
        assert!(!is_under("c:\\database", "c:\\data"));
        assert!(!is_under("c:\\dat", "c:\\data"));
        assert!(is_under("c:\\x", "c:\\"));
        assert!(is_under("c:\\", "c:\\"));
        assert!(!is_under("d:\\x", "c:\\"));
        assert!(is_under("\\\\s\\share\\x", "\\\\s\\share"));
        assert!(!is_under("\\\\s\\share2\\x", "\\\\s\\share"));
    }

    #[test]
    fn parent_stops_at_roots() {
        assert_eq!(parent("c:\\a\\b"), Some("c:\\a"));
        assert_eq!(parent("c:\\a"), Some("c:\\"));
        assert_eq!(parent("c:\\"), None);
        assert_eq!(parent("\\\\s\\share\\a\\b"), Some("\\\\s\\share\\a"));
        assert_eq!(parent("\\\\s\\share\\a"), Some("\\\\s\\share"));
        assert_eq!(parent("\\\\s\\share"), None);
    }

    fn caps(paths: Vec<PathGrant>) -> Capabilities {
        Capabilities { paths, ..Capabilities::default() }
    }

    fn prefix(p: &str, access: Access) -> PathGrant {
        PathGrant { scope: Scope::Prefix(p.to_string()), access }
    }

    #[test]
    fn default_denies_everything() {
        let c = Capabilities::default();
        assert!(!c.network && !c.run_processes && c.paths.is_empty());
        assert_eq!(c.validate(), Ok(()));
        let mut p = PathPolicy::new(&c);
        p.set_inputs(vec![("C:\\a\\b.txt".into(), true)]);
        assert_eq!(p.check("C:\\a\\b.txt", Access::Read), Err(Denied::NotGranted));
        assert_eq!(p.check("C:\\", Access::Read), Err(Denied::NotGranted));
    }

    #[test]
    fn prefix_grants() {
        let mut p = PathPolicy::new(&caps(vec![prefix("C:\\Data", Access::Read), prefix("D:\\Out", Access::ReadWrite)]));
        assert_eq!(p.check("c:/data/sub/x.txt", Access::Read), Ok(("C:\\data\\sub\\x.txt".into(), vec![Root::Dir("C:\\Data".into())])));
        assert!(p.check("C:\\DATA", Access::Read).is_ok());
        // read-only grant: no write
        assert_eq!(p.check("C:\\Data\\x", Access::ReadWrite), Err(Denied::NotGranted));
        // a sibling with the same start, and the parent, are outside
        assert_eq!(p.check("C:\\Database\\x", Access::Read), Err(Denied::NotGranted));
        assert_eq!(p.check("C:\\", Access::Read), Err(Denied::NotGranted));
        // escapes are refused before any grant is looked at
        assert_eq!(p.check("C:\\Data\\..\\Windows\\x", Access::Read), Err(Denied::Invalid(PathError::Traversal)));
        assert_eq!(p.check("C:\\Data\\x:stream", Access::Read), Err(Denied::Invalid(PathError::BadName)));
        assert_eq!(p.check("data\\x", Access::Read), Err(Denied::Invalid(PathError::NotAbsolute)));
        // read-write covers read
        assert!(p.check("D:\\Out\\a", Access::Read).is_ok());
        assert!(p.check("D:\\Out\\a", Access::ReadWrite).is_ok());
        // only ASCII case is folded
        let mut p = PathPolicy::new(&caps(vec![prefix("C:\\Work", Access::Read)]));
        assert_eq!(p.check("C:\\wor\u{212A}\\x", Access::Read), Err(Denied::NotGranted));
    }

    #[test]
    fn input_grants_follow_the_call() {
        let mut p = PathPolicy::new(&caps(vec![PathGrant { scope: Scope::Input, access: Access::Read }]));
        assert!(p.uses_inputs());
        assert_eq!(p.check("C:\\a\\one.txt", Access::Read), Err(Denied::NotGranted));
        p.set_inputs(vec![
            ("C:\\a\\one.txt".into(), false),
            ("C:\\dir".into(), true),
            ("not absolute".into(), true),
            ("c:\\A\\ONE.txt".into(), false),
        ]);
        assert_eq!(p.check("c:\\a\\ONE.TXT", Access::Read).unwrap().1, vec![Root::Input("C:\\a\\one.txt".into())]);
        // under an input folder
        assert_eq!(p.check("C:\\dir\\deep\\x", Access::Read).unwrap().1, vec![Root::Input("C:\\dir".into())]);
        // a sibling, the parent and write access are not covered
        assert_eq!(p.check("C:\\a\\two.txt", Access::Read), Err(Denied::NotGranted));
        assert_eq!(p.check("C:\\a", Access::Read), Err(Denied::NotGranted));
        assert_eq!(p.check("C:\\a\\one.txt", Access::ReadWrite), Err(Denied::NotGranted));
        // the next call replaces the set
        p.set_inputs(vec![("C:\\b\\x".into(), true)]);
        assert_eq!(p.check("C:\\a\\one.txt", Access::Read), Err(Denied::NotGranted));
        assert!(p.check("C:\\b\\x", Access::Read).is_ok());
        p.set_inputs(Vec::new());
        assert_eq!(p.check("C:\\b\\x", Access::Read), Err(Denied::NotGranted));
        // a drive root as input covers the drive
        p.set_inputs(vec![("D:\\".into(), true)]);
        assert!(p.check("D:\\x\\y", Access::Read).is_ok());
        assert_eq!(p.check("E:\\x", Access::Read), Err(Denied::NotGranted));
    }

    #[test]
    fn flat_inputs_cover_only_the_path() {
        let mut p = PathPolicy::new(&caps(vec![PathGrant { scope: Scope::Input, access: Access::Read }]));
        p.set_inputs(vec![("C:\\dir".into(), false), ("D:\\".into(), false)]);
        assert_eq!(p.check("C:\\dir", Access::Read).unwrap().1, vec![Root::Input("C:\\dir".into())]);
        assert_eq!(p.check("C:\\dir\\x", Access::Read), Err(Denied::NotGranted));
        assert_eq!(p.check("C:\\dir\\deep\\x", Access::Read), Err(Denied::NotGranted));
        assert!(p.check("D:\\", Access::Read).is_ok());
        assert_eq!(p.check("D:\\x", Access::Read), Err(Denied::NotGranted));
        // a recursive input of a folder above still covers it
        p.set_inputs(vec![("C:\\dir\\sub".into(), false), ("C:\\dir".into(), true)]);
        assert_eq!(p.check("C:\\dir\\sub\\x", Access::Read).unwrap().1, vec![Root::Input("C:\\dir".into())]);
        // the same path flat and recursive: recursive wins
        p.set_inputs(vec![("C:\\dir".into(), false), ("c:\\DIR".into(), true)]);
        assert!(p.check("C:\\dir\\x", Access::Read).is_ok());
    }

    #[test]
    fn any_grant() {
        let mut p = PathPolicy::new(&caps(vec![PathGrant { scope: Scope::Any, access: Access::Read }]));
        assert_eq!(p.check("Z:\\x", Access::Read).unwrap().1, vec![Root::Any]);
        assert_eq!(p.check("Z:\\x", Access::ReadWrite), Err(Denied::NotGranted));
        // still no traversal, streams or devices
        assert!(matches!(p.check("Z:\\x\\..\\y", Access::Read), Err(Denied::Invalid(_))));
        assert!(matches!(p.check("\\\\.\\PhysicalDrive0", Access::Read), Err(Denied::Invalid(_))));
    }

    #[test]
    fn unc_paths_need_the_network_or_a_unc_input() {
        // `*` never covers a network path, read or write.
        let any = |access| PathGrant { scope: Scope::Any, access };
        let mut p = PathPolicy::new(&caps(vec![any(Access::ReadWrite)]));
        assert_eq!(p.check("\\\\server\\share\\x", Access::Read), Err(Denied::NotGranted));
        assert_eq!(p.check("\\\\?\\UNC\\server\\share\\x", Access::ReadWrite), Err(Denied::NotGranted));
        assert_eq!(p.check("C:\\x", Access::Read).unwrap().1, vec![Root::Any]);
        // WebDAV server names are refused before any grant is looked at
        assert_eq!(normalize("\\\\host@80\\dav\\x"), Err(PathError::BadName));
        assert_eq!(normalize("\\\\host@SSL\\DavWWWRoot\\x"), Err(PathError::BadName));

        // a UNC folder grant needs `network = true`, and only then covers the share
        let share = prefix("\\\\server\\share", Access::Read);
        assert_eq!(caps(vec![share.clone()]).validate(), Err(CapError::NetworkPath("\\\\server\\share".into())));
        let mut p = PathPolicy::new(&caps(vec![share.clone()]));
        assert_eq!(p.check("\\\\server\\share\\x", Access::Read), Err(Denied::NotGranted), "a host that skips validate still denies");
        let net = Capabilities { network: true, paths: vec![share], ..Capabilities::default() };
        assert_eq!(net.validate(), Ok(()));
        assert_eq!(net.summary()[0], "Read files on the server server: \\\\server\\share");
        let mut p = PathPolicy::new(&net);
        assert_eq!(p.check("\\\\SERVER\\share\\x", Access::Read).unwrap().1, vec![Root::Dir("\\\\server\\share".into())]);
        assert_eq!(p.check("\\\\server\\other\\x", Access::Read), Err(Denied::NotGranted));

        // an input on a share covers that share's tree, with or without the network
        let mut p = PathPolicy::new(&caps(vec![PathGrant { scope: Scope::Input, access: Access::Read }, any(Access::Read)]));
        p.set_inputs(vec![("\\\\nas\\media\\films".into(), true), ("C:\\local".into(), true)]);
        assert_eq!(p.check("\\\\nas\\media\\films\\a.mkv", Access::Read).unwrap().1, vec![Root::Input("\\\\nas\\media\\films".into())]);
        assert_eq!(p.check("\\\\nas\\media\\other.mkv", Access::Read), Err(Denied::NotGranted));
        assert_eq!(p.check("\\\\nas\\media", Access::Read), Err(Denied::NotGranted));
    }

    #[test]
    fn protected_folders() {
        let p = Protected::new(&["C:\\Plugins\\x".into(), "C:\\App".into()], &["C:\\Cache".into()]).unwrap();
        assert_eq!(p.no_write(), ["C:\\Cache", "C:\\Plugins\\x", "C:\\App"]);
        // sealed: nothing at all
        assert!(p.denies("c:\\cache", false) && p.denies("c:\\cache\\a\\1.0.0\\install.toml", false));
        // no-write: reads are fine
        assert!(!p.denies("c:\\plugins\\x\\plugin.toml", false) && p.denies("c:\\plugins\\x\\plugin.toml", true));
        assert!(!p.denies("c:\\cachex\\a", true) && !p.denies("c:\\plugins\\y", true));

        let g = |path: &str, access| caps(vec![prefix(path, access)]);
        assert_eq!(p.check_grants(&g("C:\\Plugins\\x", Access::Read)), Ok(()), "$plugin read");
        assert_eq!(p.check_grants(&g("C:\\Plugins\\x", Access::ReadWrite)), Err(CapError::Protected("C:\\Plugins\\x".into())));
        assert_eq!(p.check_grants(&g("C:\\Plugins", Access::ReadWrite)), Err(CapError::Protected("C:\\Plugins".into())), "above");
        assert_eq!(p.check_grants(&g("C:\\App\\settings", Access::ReadWrite)), Err(CapError::Protected("C:\\App\\settings".into())), "inside");
        assert_eq!(p.check_grants(&g("C:\\Cache\\a", Access::Read)), Err(CapError::Protected("C:\\Cache\\a".into())));
        assert_eq!(p.check_grants(&g("C:\\", Access::Read)), Err(CapError::Protected("C:\\".into())));
        assert_eq!(p.check_grants(&g("D:\\Work", Access::ReadWrite)), Ok(()));
        assert_eq!(p.check_grants(&caps(vec![PathGrant { scope: Scope::Any, access: Access::ReadWrite }])), Ok(()), "the host refuses per path");
        assert!(Protected::new(&["relative".into()], &[]).is_err());
    }

    #[test]
    fn superscript_device_names_are_refused() {
        for p in ["C:\\a\\COM\u{b9}", "C:\\a\\com\u{b2}.txt", "C:\\a\\LPT\u{b3}", "C:\\a\\COM0", "C:\\a\\lpt0.x"] {
            assert_eq!(normalize(p), Err(PathError::Device), "{p}");
        }
        for p in ["C:\\a\\COM\u{b9}\u{b9}", "C:\\a\\COM\u{2074}", "C:\\a\\COMA", "C:\\a\\LPT"] {
            assert!(normalize(p).is_ok(), "{p}");
        }
    }

    #[test]
    fn validate_limits_and_grants() {
        let ok = Capabilities::default();
        assert_eq!(Capabilities { max_memory_mb: 0, ..ok.clone() }.validate(), Err(CapError::Memory(0)));
        assert_eq!(Capabilities { max_memory_mb: MAX_MEMORY_MB + 1, ..ok.clone() }.validate(), Err(CapError::Memory(MAX_MEMORY_MB + 1)));
        assert_eq!(Capabilities { max_memory_mb: MAX_MEMORY_MB, ..ok.clone() }.validate(), Ok(()));
        assert_eq!(Capabilities { timeout_ms: MIN_TIMEOUT_MS - 1, ..ok.clone() }.validate(), Err(CapError::Timeout(MIN_TIMEOUT_MS - 1)));
        assert_eq!(Capabilities { timeout_ms: MAX_TIMEOUT_MS + 1, ..ok.clone() }.validate(), Err(CapError::Timeout(MAX_TIMEOUT_MS + 1)));
        assert_eq!(caps(vec![prefix("C:\\a\\..\\b", Access::Read)]).validate(), Err(CapError::BadPrefix("C:\\a\\..\\b".into())));
        assert_eq!(caps(vec![prefix("c:/a", Access::Read)]).validate(), Err(CapError::BadPrefix("c:/a".into())));
        assert_eq!(caps(vec![prefix("C:\\a", Access::Read), prefix("C:\\A", Access::ReadWrite)]).validate(), Err(CapError::Duplicate("c:\\a".into())));
        let many = (0..MAX_PATH_GRANTS + 1).map(|i| prefix(&format!("C:\\d{i}"), Access::Read)).collect();
        assert_eq!(caps(many).validate(), Err(CapError::TooManyPaths(MAX_PATH_GRANTS + 1)));
        let input = PathGrant { scope: Scope::Input, access: Access::Read };
        assert_eq!(caps(vec![input.clone(), input]).validate(), Err(CapError::Duplicate("$input".into())));
    }

    #[test]
    fn summary_lines() {
        let c = Capabilities {
            network: true,
            paths: vec![PathGrant { scope: Scope::Input, access: Access::Read }, prefix("C:\\Out", Access::ReadWrite)],
            ..Capabilities::default()
        };
        let s = c.summary();
        assert_eq!(s[0], format!("Read {INPUT_SCOPE}"));
        assert!(s[0].contains("for a column or a preview only that file or folder"));
        assert!(s[0].contains("for a command what you selected"));
        assert!(s[0].contains("for a search the folder searched"));
        assert_eq!(s[1], "Read and change files in C:\\Out");
        assert!(s[2].starts_with("Use the network (reserved"), "{}", s[2]);
        assert_eq!(s.len(), 4);
        assert_eq!(Capabilities::default().summary().len(), 1);
    }
}
