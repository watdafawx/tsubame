//! Command line: `tsubame [options] [path ...]`.
//!
//! Parsing is pure (`parse` takes the working directory) and the tests drive a
//! table of the odd paths Windows users paste: quotes, trailing backslashes,
//! `.`/`..`, relatives, drive roots, UNC and `\\?\` paths.
//!
//! What a path means is decided later (`targets`): a folder opens, a file opens
//! its folder and selects the file, and a path that no longer exists opens its
//! nearest existing parent (the enumerator would only show an error otherwise).
//!
//! The measurement flags (`--bench`, `--timing`, `--leak-gate`, `--leak-smoke`,
//! `--stress`, `--hang`, ...) keep working: they are passed through untouched
//! and mark the run as a dev run, which skips the single-instance handshake and
//! the config writes.

use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const HELP: &str = concat!(
    "Tsubame ",
    env!("CARGO_PKG_VERSION"),
    ": a very fast Windows file browser.\n",
    "\n",
    "Usage: tsubame [options] [path ...]\n",
    "\n",
    "Arguments:\n",
    "  <path>              a folder to open, or a file to open its folder and select it.\n",
    "                      Several paths open as tabs. Quotes, relative paths, `.`/`..`,\n",
    "                      drive roots (`D:`), UNC (`\\\\server\\share`) and `\\\\?\\` paths work.\n",
    "\n",
    "Options:\n",
    "  --new-tab           open the given paths as new tabs instead of replacing the\n",
    "                      folder of the first tab\n",
    "  --new-window        start a separate instance instead of opening the path in the\n",
    "                      running one\n",
    "  --select <path>     open the folder of <path> and select the entry\n",
    "  --portable          use .\\data\\ next to the exe for config and session\n",
    "  --config <file>     read and write this config file instead of the default\n",
    "  --thumb-workers <n> threads that load thumbnails (default 2, 0-16; 0 shares the icon\n",
    "                      threads)\n",
    "  --help, -h          print this text and exit (attaches to the parent console)\n",
    "  --version, -V       print the version and exit\n",
    "\n",
    "Config: %APPDATA%\\Tsubame\\config.toml (docs/config.md). Portable mode is also\n",
    "switched on by an empty `tsubame.portable` file next to the exe.\n",
    "A second launch forwards its paths to the running Tsubame and exits.\n",
);

/// Flags that take a value. Needed so a value that looks like a flag is not
/// mistaken for a positional path.
const VALUE_FLAGS: &[&str] = &["--config", "--select", "--dir", "--theme", "--density", "--stress", "--workers", "--timing-dirs", "--reps", "--gate-fixture", "--gate-navs", "--gate-tabs", "--gate-sample", "--gate-settle", "--thumb-workers", "--view"];

/// Value-less flags the app reads from the raw arguments.
const BOOL_FLAGS: &[&str] = &["--windowed", "--zebra"];

/// Upper bound of `--thumb-workers`: more threads than this only fight over the disk.
pub const MAX_THUMB_WORKERS: usize = 16;

/// Measurement / diagnostics flags: the process is a one-off run, not a window
/// the user is opening, so it neither forwards to a running instance nor writes
/// the config.
const DEV_FLAGS: &[&str] = &["--bench", "--timing", "--leak-gate", "--leak-smoke", "--stress", "--hang", "--memdiag"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cli {
    /// Normalised absolute paths, in command-line order.
    pub paths: Vec<String>,
    pub new_tab: bool,
    pub new_window: bool,
    pub select: Option<String>,
    pub portable: bool,
    pub config: Option<PathBuf>,
    pub help: bool,
    pub version: bool,
    /// A measurement/diagnostics run (`--bench`, `--leak-gate`, ...).
    pub dev: bool,
    /// `--after-update <pid>`: the updater started this copy; wait for that process to exit
    /// before claiming the single-instance lock.
    pub after_update: Option<u32>,
    /// `--thumb-workers <n>`, clamped to `0..=MAX_THUMB_WORKERS`.
    pub thumb_workers: Option<usize>,
    /// One line per ignored or malformed argument. The caller prints them to stderr;
    /// they never stop the launch and never become paths.
    pub warnings: Vec<String>,
    /// Flags this parser does not own, with their values, in order. The app's
    /// existing measurement flags are read from here (and from the raw args).
    pub passthrough: Vec<String>,
}

impl Cli {
    pub fn parse(args: &[String], cwd: &Path) -> Cli {
        let mut c = Cli::default();
        let mut i = 1; // args[0] is the exe
        while i < args.len() {
            let a = args[i].as_str();
            match a {
                "--new-tab" => c.new_tab = true,
                "--new-window" => c.new_window = true,
                "--portable" => c.portable = true,
                "--after-update" => {
                    if let Some(v) = args.get(i + 1) {
                        c.after_update = v.parse().ok();
                        i += 1;
                    }
                }
                "--help" | "-h" | "-?" | "/?" => c.help = true,
                "--version" | "-V" => c.version = true,
                "--config" => {
                    if let Some(v) = args.get(i + 1) {
                        c.config = Some(PathBuf::from(trim_quotes(v)));
                        i += 1;
                    }
                }
                "--select" => {
                    if let Some(v) = args.get(i + 1) {
                        c.select = normalize(v, cwd);
                        i += 1;
                    }
                }
                "--dir" => {
                    // the old spelling of the folder to open
                    if let Some(v) = args.get(i + 1) {
                        if let Some(p) = normalize(v, cwd) {
                            c.paths.push(p);
                        }
                        i += 1;
                    }
                }
                "--thumb-workers" => match args.get(i + 1).filter(|v| !v.starts_with('-')) {
                    Some(v) => {
                        i += 1;
                        match v.trim().parse::<usize>() {
                            Ok(n) => c.thumb_workers = Some(n.min(MAX_THUMB_WORKERS)),
                            Err(_) => c.warnings.push(format!("--thumb-workers needs a number (0-{MAX_THUMB_WORKERS}), got \"{v}\"; ignored")),
                        }
                    }
                    None => c.warnings.push(format!("--thumb-workers needs a number (0-{MAX_THUMB_WORKERS}); ignored")),
                },
                _ if a.starts_with("--") => {
                    if is_value_flag(a) {
                        if let Some(v) = args.get(i + 1) {
                            c.passthrough.push(a.to_string());
                            c.passthrough.push(v.clone());
                            i += 1;
                        } else {
                            c.warnings.push(format!("{a} needs a value; ignored"));
                        }
                    } else {
                        c.passthrough.push(a.to_string());
                        if !BOOL_FLAGS.contains(&a) && !DEV_FLAGS.contains(&a) {
                            c.warnings.push(format!("unknown option {a}; ignored"));
                        }
                    }
                    if DEV_FLAGS.contains(&a) {
                        c.dev = true;
                    }
                }
                // a single dash is a path-ish argument, anything else with a dash
                // is an unknown flag: pass it through rather than opening a folder
                _ if a.starts_with('-') && a.len() > 1 => {
                    c.passthrough.push(a.to_string());
                    c.warnings.push(format!("unknown option {a}; ignored"));
                }
                _ => {
                    if let Some(p) = normalize(a, cwd) {
                        c.paths.push(p);
                    }
                }
            }
            i += 1;
        }
        c
    }

    pub fn is_dev_run(&self) -> bool {
        self.dev
    }
}

fn is_value_flag(a: &str) -> bool {
    VALUE_FLAGS.contains(&a)
}

fn trim_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\''))) {
        t[1..t.len() - 1].trim()
    } else {
        t
    }
}

/// Cleans one command-line path into the form the app uses: absolute, `\`
/// separators, no `.`/`..`, no trailing separator except on a root, drive
/// roots as `C:\`, UNC shares as `\\server\share\`, `\\?\` kept verbatim.
/// `None` for an empty argument.
pub fn normalize(input: &str, cwd: &Path) -> Option<String> {
    let t = trim_quotes(input);
    if t.is_empty() {
        return None;
    }
    let slashed = t.replace('/', "\\");
    // Resolve a relative path (or a root-relative `\foo`) against the working
    // directory first, so `..` can walk above it and the result is absolute.
    let joined;
    let path: &str = if is_absolute(&slashed) {
        &slashed
    } else {
        let base = cwd.to_string_lossy().replace('/', "\\");
        let base = base.trim_end_matches('\\');
        let base = if base.is_empty() { "C:" } else { base };
        let rest = slashed.trim_start_matches('\\');
        joined = if slashed.starts_with('\\') {
            // "\foo" is the root of the working directory's drive
            let drive = base.split('\\').next().unwrap_or("C:");
            if rest.is_empty() {
                format!("{drive}\\")
            } else {
                format!("{drive}\\{rest}")
            }
        } else if rest.is_empty() {
            base.to_string()
        } else {
            format!("{base}\\{rest}")
        };
        &joined
    };
    let (root, rest) = split_root(path)?;
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = root;
    if !out.ends_with('\\') {
        out.push('\\');
    }
    if parts.is_empty() {
        if !is_filesystem_root(&out) {
            out.pop();
        }
    } else {
        out.push_str(&parts.join("\\"));
    }
    Some(out)
}

fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':') || p.starts_with("\\\\")
}

/// `C:\`, `\\server\share\`, `\\?\C:\`, `\\?\UNC\server\share\`: a root that is
/// written with a trailing separator.
fn is_filesystem_root(root: &str) -> bool {
    let r = root.trim_end_matches('\\');
    if r.len() == 2 && r.as_bytes()[1] == b':' {
        return true;
    }
    if let Some(rest) = r.strip_prefix("\\\\?\\") {
        if rest.len() == 2 && rest.as_bytes().get(1) == Some(&b':') {
            return true;
        }
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return unc.split('\\').count() == 2;
        }
        return true; // a device path: passed through
    }
    if r.starts_with("\\\\.\\") {
        return true;
    }
    if let Some(rest) = r.strip_prefix("\\\\") {
        return rest.split('\\').count() == 2;
    }
    false
}

/// `(root, rest)`: the root always ends with `\`. `p` is absolute.
fn split_root(p: &str) -> Option<(String, &str)> {
    // verbatim / device prefixes
    if let Some(rest) = p.strip_prefix("\\\\?\\UNC\\") {
        let mut it = rest.splitn(3, '\\');
        let server = it.next().filter(|s| !s.is_empty())?;
        let share = it.next().filter(|s| !s.is_empty())?;
        return Some((format!("\\\\?\\UNC\\{server}\\{share}\\"), it.next().unwrap_or("")));
    }
    if let Some(rest) = p.strip_prefix("\\\\?\\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            let letter = (b[0] as char).to_ascii_uppercase();
            let tail = if b.len() > 3 { &rest[3..] } else { "" };
            return Some((format!("\\\\?\\{letter}:\\"), tail));
        }
        // \\?\Volume{...} and other device paths: leave as a root
        return Some((format!("\\\\?\\{rest}"), ""));
    }
    if let Some(rest) = p.strip_prefix("\\\\.\\") {
        return Some((format!("\\\\.\\{rest}"), ""));
    }
    if let Some(rest) = p.strip_prefix("\\\\") {
        let mut it = rest.splitn(3, '\\');
        let server = it.next().filter(|s| !s.is_empty())?;
        let share = it.next().filter(|s| !s.is_empty())?;
        return Some((format!("\\\\{server}\\{share}\\"), it.next().unwrap_or("")));
    }
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        let letter = (b[0] as char).to_ascii_uppercase();
        // "C:foo" is drive-relative; treat it as "C:\foo", which is what people mean
        let tail = if b.len() > 2 { p[2..].trim_start_matches('\\') } else { "" };
        return Some((format!("{letter}:\\"), tail));
    }
    // a bare "\" or a relative path is relative to the working directory
    Some(("C:\\".to_string(), p.trim_start_matches('\\')))
}

/// What one command-line path opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub folder: String,
    /// Entry to select once the folder is listed (a file path).
    pub select: Option<String>,
}

/// Resolves the parsed paths and `--select` against the disk:
/// - a folder opens as is,
/// - a file opens its folder, with the file selected,
/// - a missing path opens its nearest existing parent.
///
/// Network paths are not checked (a dead share would block startup).
pub fn targets(c: &Cli) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    for p in &c.paths {
        let t = resolve_one(p, None);
        if !out.iter().any(|o| o.folder.eq_ignore_ascii_case(&t.folder) && o.select == t.select) {
            out.push(t);
        }
    }
    if let Some(s) = &c.select {
        let t = resolve_one(s, Some(()));
        match out.iter_mut().find(|o| o.folder.eq_ignore_ascii_case(&t.folder)) {
            Some(o) => o.select = t.select,
            None => out.push(t),
        }
    }
    out
}

fn resolve_one(p: &str, force_select: Option<()>) -> Target {
    let network = p.starts_with("\\\\") && !p.to_ascii_lowercase().starts_with("\\\\?\\c:");
    if network {
        return Target { folder: p.to_string(), select: None };
    }
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => Target { folder: p.to_string(), select: None },
        Ok(_) => {
            let folder = tsubame_config::session::parent(p).unwrap_or_else(|| p.to_string());
            let name = p.rsplit('\\').next().unwrap_or(p).to_string();
            Target { folder, select: Some(name) }
        }
        Err(_) => {
            // gone: the nearest existing parent, no selection
            let folder = tsubame_config::session::nearest_existing(p).unwrap_or_else(|| p.to_string());
            let _ = force_select;
            Target { folder, select: None }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CWD: &str = "C:\\Users\\me\\project";

    fn n(s: &str) -> String {
        normalize(s, Path::new(CWD)).unwrap()
    }

    fn cli(args: &[&str]) -> Cli {
        let mut v = vec!["tsubame".to_string()];
        v.extend(args.iter().map(|s| s.to_string()));
        Cli::parse(&v, Path::new(CWD))
    }

    #[test]
    fn normalize_table() {
        // absolute paths
        assert_eq!(n("C:\\Users\\me"), "C:\\Users\\me");
        assert_eq!(n("c:\\Users\\me"), "C:\\Users\\me");
        assert_eq!(n("\"C:\\Program Files\""), "C:\\Program Files");
        assert_eq!(n("\"C:\\Users\\me\""), "C:\\Users\\me");
        // trailing separators
        assert_eq!(n("C:\\Users\\me\\"), "C:\\Users\\me");
        assert_eq!(n("C:\\Users\\me\\\\\\"), "C:\\Users\\me");
        // drive roots
        assert_eq!(n("D:"), "D:\\");
        assert_eq!(n("D:\\"), "D:\\");
        assert_eq!(n("d:/"), "D:\\");
        assert_eq!(n("C:"), "C:\\");
        // dot segments
        assert_eq!(n("C:\\a\\.\\b"), "C:\\a\\b");
        assert_eq!(n("C:\\a\\b\\..\\c"), "C:\\a\\c");
        assert_eq!(n("C:\\a\\..\\..\\..\\b"), "C:\\b", ".. at the root stays at the root");
        assert_eq!(n("C:\\a\\.\\..\\"), "C:\\");
        // relative paths resolve against the working directory
        assert_eq!(n("sub\\dir"), "C:\\Users\\me\\project\\sub\\dir");
        assert_eq!(n(".\\sub"), "C:\\Users\\me\\project\\sub");
        assert_eq!(n("."), "C:\\Users\\me\\project");
        assert_eq!(n(".."), "C:\\Users\\me");
        assert_eq!(n("..\\other\\..\\x"), "C:\\Users\\me\\x");
        assert_eq!(n("\\\\server\\share\\x\\..\\y"), "\\\\server\\share\\y");
        // a bare backslash is the working drive's root
        assert_eq!(n("\\"), "C:\\");
        // forward slashes
        assert_eq!(n("D:/games/saves/"), "D:\\games\\saves");
        // UNC shares
        assert_eq!(n("\\\\server\\share"), "\\\\server\\share\\");
        assert_eq!(n("\\\\server\\share\\"), "\\\\server\\share\\");
        assert_eq!(n("\\\\server\\share\\dir\\file.txt"), "\\\\server\\share\\dir\\file.txt");
        assert_eq!(n("//server/share/dir"), "\\\\server\\share\\dir");
        // \\?\ long paths keep their prefix
        assert_eq!(n("\\\\?\\C:\\very\\long\\path"), "\\\\?\\C:\\very\\long\\path");
        assert_eq!(n("\\\\?\\C:\\very\\..\\long"), "\\\\?\\C:\\long");
        assert_eq!(n("\\\\?\\UNC\\server\\share\\dir"), "\\\\?\\UNC\\server\\share\\dir");
        // quotes with a trailing separator inside them
        assert_eq!(n("\"C:\\Users\\me\\\""), "C:\\Users\\me");
        assert_eq!(n("'C:\\Users\\me'"), "C:\\Users\\me");
        // unicode and spaces
        assert_eq!(n("C:\\Ünïcode dir\\ファイル.txt"), "C:\\Ünïcode dir\\ファイル.txt");
        // nothing
        assert_eq!(normalize("", Path::new(CWD)), None);
        assert_eq!(normalize("   ", Path::new(CWD)), None);
        assert_eq!(normalize("\"\"", Path::new(CWD)), None);
    }

    #[test]
    fn plain_launch_has_no_paths() {
        let c = cli(&[]);
        assert_eq!(c, Cli::default());
        assert!(c.paths.is_empty() && !c.new_tab && !c.new_window && c.select.is_none());
        assert!(!c.is_dev_run());
    }

    #[test]
    fn one_path_and_two_paths() {
        let c = cli(&["C:\\a"]);
        assert_eq!(c.paths, ["C:\\a"]);
        let c = cli(&["C:\\a", "D:\\b"]);
        assert_eq!(c.paths, ["C:\\a", "D:\\b"]);
        assert!(!c.new_tab);
    }

    #[test]
    fn options_parse_and_keep_order() {
        let c = cli(&["--new-tab", "C:\\a", "--new-window", "sub"]);
        assert!(c.new_tab && c.new_window);
        assert_eq!(c.paths, ["C:\\a", "C:\\Users\\me\\project\\sub"]);
        let c = cli(&["--portable", "--config", "D:\\cfg\\tsubame.toml"]);
        assert!(c.portable);
        assert_eq!(c.config, Some(PathBuf::from("D:\\cfg\\tsubame.toml")));
    }

    #[test]
    fn quoted_paths_survive_the_shell() {
        let c = cli(&["\"C:\\Program Files\\Some App\"", "\"D:\\trailing\\\""]);
        assert_eq!(c.paths, ["C:\\Program Files\\Some App", "D:\\trailing"]);
    }

    #[test]
    fn select_takes_the_next_argument() {
        let c = cli(&["--select", "C:\\dir\\file.txt"]);
        assert_eq!(c.select.as_deref(), Some("C:\\dir\\file.txt"));
        assert!(c.paths.is_empty());
        // a missing value is ignored, not eaten as a path
        let c = cli(&["--select"]);
        assert_eq!(c.select, None);
        assert!(c.paths.is_empty());
    }

    #[test]
    fn help_and_version() {
        assert!(cli(&["--help"]).help);
        assert!(cli(&["-h"]).help);
        assert!(cli(&["--version"]).version);
        assert!(cli(&["-V"]).version);
        assert!(!cli(&["--help"]).is_dev_run());
    }

    #[test]
    fn dev_flags_pass_through_and_mark_the_run() {
        let c = cli(&["--bench", "--dir", "C:\\a", "--workers", "2", "--gate-fixture", "D:\\gate"]);
        assert!(c.is_dev_run());
        assert_eq!(c.paths, ["C:\\a"], "--dir is the old spelling of a path");
        assert_eq!(c.passthrough, ["--bench", "--workers", "2", "--gate-fixture", "D:\\gate"]);
        for flag in ["--timing", "--leak-gate", "--leak-smoke", "--stress", "--hang"] {
            assert!(cli(&[flag]).is_dev_run(), "{flag} must not forward to a running instance");
        }
        // flags that only steer the normal window are not dev runs
        let c = cli(&["--theme", "light", "--density", "compact", "--zebra"]);
        assert!(!c.is_dev_run());
        assert_eq!(c.passthrough, ["--theme", "light", "--density", "compact", "--zebra"]);
    }

    #[test]
    fn unknown_flags_are_not_paths() {
        let c = cli(&["--nonsense", "-x", "C:\\a"]);
        assert_eq!(c.paths, ["C:\\a"]);
        assert_eq!(c.passthrough, ["--nonsense", "-x"]);
        assert!(!c.is_dev_run());
    }

    #[test]
    fn thumb_workers_takes_its_number_and_never_opens_a_tab() {
        let c = cli(&["--thumb-workers", "20"]);
        assert!(c.paths.is_empty(), "the value must not become a path: {:?}", c.paths);
        assert_eq!(c.thumb_workers, Some(MAX_THUMB_WORKERS), "20 is clamped");
        assert!(c.warnings.is_empty() && c.passthrough.is_empty());
        assert!(targets(&c).is_empty());
        assert_eq!(cli(&["--thumb-workers", "0"]).thumb_workers, Some(0));
        assert_eq!(cli(&["--thumb-workers", "4", "C:\\a"]).paths, ["C:\\a"]);
        assert_eq!(cli(&["C:\\a", "--thumb-workers", "4"]).paths, ["C:\\a"]);
        assert_eq!(cli(&["--thumb-workers", "3"]).thumb_workers, Some(3));
    }

    #[test]
    fn a_bad_thumb_workers_value_warns_and_carries_on() {
        for args in [&["--thumb-workers", "lots"][..], &["--thumb-workers", "1.5"], &["--thumb-workers"]] {
            let c = cli(args);
            assert_eq!(c.thumb_workers, None, "{args:?}");
            assert_eq!(c.warnings.len(), 1, "{args:?}");
            assert!(c.warnings[0].starts_with("--thumb-workers needs a number"), "{:?}", c.warnings);
            assert!(c.paths.is_empty(), "{args:?}");
        }
        // a following flag is not eaten as the value
        let c = cli(&["--thumb-workers", "--new-tab", "C:\\a"]);
        assert!(c.new_tab);
        assert_eq!(c.paths, ["C:\\a"]);
    }

    #[test]
    fn view_value_is_not_a_path() {
        let c = cli(&["--view", "grid-small", "C:\\a"]);
        assert_eq!(c.paths, ["C:\\a"]);
        assert_eq!(c.passthrough, ["--view", "grid-small"]);
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn unknown_flags_warn_and_known_ones_do_not() {
        let c = cli(&["--nonsense", "-x", "--frobnicate=3", "C:\\a"]);
        assert_eq!(c.paths, ["C:\\a"]);
        assert_eq!(c.warnings, ["unknown option --nonsense; ignored", "unknown option -x; ignored", "unknown option --frobnicate=3; ignored"]);
        let c = cli(&["--bench", "--zebra", "--windowed", "--theme", "light", "--workers", "2", "--new-tab", "--portable"]);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        // a value flag at the end of the line has nothing to read
        let c = cli(&["--theme"]);
        assert_eq!(c.warnings, ["--theme needs a value; ignored"]);
        assert!(c.paths.is_empty());
    }

    #[test]
    fn a_path_that_starts_with_a_dash_is_still_a_flag() {
        // Windows cannot make a file starting with '-' at the root of a drive,
        // but a relative one works through .\\
        let c = cli(&[".\\-weird"]);
        assert_eq!(c.paths, ["C:\\Users\\me\\project\\-weird"]);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_folder_file_and_missing_path_become_targets() {
        let d = temp_dir("targets");
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let file = d.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let c = cli(&[d.to_str().unwrap(), file.to_str().unwrap(), sub.join("gone").join("deeper").to_str().unwrap()]);
        let t = targets(&c);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0], Target { folder: d.to_str().unwrap().into(), select: None });
        assert_eq!(t[1], Target { folder: d.to_str().unwrap().into(), select: Some("a.txt".into()) });
        assert_eq!(t[2], Target { folder: sub.to_str().unwrap().into(), select: None }, "missing path falls back to the nearest existing parent");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn select_overrides_the_selection_of_its_folder() {
        let d = temp_dir("select");
        let file = d.join("b.txt");
        std::fs::write(&file, b"x").unwrap();
        let c = cli(&[d.to_str().unwrap(), "--select", file.to_str().unwrap()]);
        let t = targets(&c);
        assert_eq!(t.len(), 1, "the select path joins the folder it names");
        assert_eq!(t[0].select.as_deref(), Some("b.txt"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn duplicate_paths_collapse() {
        let d = temp_dir("dupes");
        let s = d.to_str().unwrap();
        let c = cli(&[s, s]);
        assert_eq!(c.paths.len(), 2);
        assert_eq!(targets(&c).len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn help_text_mentions_the_contract() {
        for needle in ["tsubame [options] [path ...]", "--new-tab", "--new-window", "--select", "--portable", "--config", "--thumb-workers", "--help", "--version", "APPDATA"] {
            assert!(HELP.contains(needle), "help is missing {needle}");
        }
    }
}
