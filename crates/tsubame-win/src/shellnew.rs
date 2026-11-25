//! The "New" menu's file types: what Windows registers under
//! `HKCR\.ext\ShellNew` (also one level down, `HKCR\.ext\<ProgID>\ShellNew`,
//! where Office keeps them). One scan per session on a background thread;
//! the menu reads the cached answer.
//!
//! A type creates its file one of three ways (the values of the key):
//! - `NullFile`: an empty file;
//! - `FileName`: a copy of a template (a bare name is looked up in the user's
//!   Templates folder, then in `%SystemRoot%\ShellNew`);
//! - `Data`: these bytes are the file (written once to a template file under
//!   the temp folder, so the file-op queue can copy it like any template).
//!
//! `Command` types (Briefcase, Shortcut) run code and are skipped, as are the
//! types the menu has itself or that never worked (`SKIP`). The list is capped.
//!
//! The parser and the naming are pure (unit tested); only [`scan`] touches the
//! registry.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Most types the menu shows.
pub const MAX_TYPES: usize = 20;
/// Largest `Data` blob taken (Windows' own are a few hundred bytes).
pub const MAX_DATA: usize = 1 << 20;
/// Extensions never listed: ours (text), run code or are not files.
pub const SKIP: &[&str] = &[".txt", ".lnk", ".library-ms", ".searchconnector-ms", ".contact", ".group", ".mydocs", ".fldr", ".jnt", ".bfc", ".zfsendtotarget"];

/// The values of one `ShellNew` key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    pub null_file: bool,
    pub file_name: Option<String>,
    pub data: Option<Vec<u8>>,
    /// `Command` (or `Handler`): runs code to make the item
    pub command: bool,
}

/// How a type's file gets its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Empty,
    /// a template name, absolute or relative to the Templates folders
    Template(String),
    Data(Vec<u8>),
}

/// What the key's values mean. `None`: not usable here.
pub fn parse(raw: &Raw) -> Option<Source> {
    if raw.command {
        return None;
    }
    if let Some(f) = raw.file_name.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        return Some(Source::Template(f.to_string()));
    }
    if let Some(d) = raw.data.as_ref().filter(|d| !d.is_empty() && d.len() <= MAX_DATA) {
        return Some(Source::Data(d.clone()));
    }
    raw.null_file.then_some(Source::Empty)
}

/// `.ext` is one the menu never lists.
pub fn is_skipped(ext: &str) -> bool {
    SKIP.iter().any(|s| s.eq_ignore_ascii_case(ext))
}

/// The template file for `name`: itself when absolute, else the first of `dirs`
/// that has it (`exists` says so).
pub fn resolve_template(name: &str, dirs: &[PathBuf], exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() {
        return exists(p).then(|| p.to_path_buf());
    }
    dirs.iter().map(|d| d.join(name)).find(|c| exists(c))
}

/// A type's label for a menu: the friendly name, else ".EXT file"; characters
/// a file name cannot hold are dropped, so "New <label>" is a legal name.
pub fn label_for(ext: &str, friendly: Option<&str>) -> String {
    let clean = |s: &str| -> String { s.chars().filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control()).collect::<String>().trim().trim_end_matches('.').to_string() };
    match friendly.map(clean).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => format!("{} file", ext.trim_start_matches('.').to_uppercase()),
    }
}

/// The first free `base`+`ext` ("Image.png", "Image (2).png", ...). `taken`
/// gets the lower-case candidate.
pub fn unique_name(base: &str, ext: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let first = format!("{base}{ext}");
    if !taken(&first.to_lowercase()) {
        return first;
    }
    for k in 2..10_000 {
        let n = format!("{base} ({k}){ext}");
        if !taken(&n.to_lowercase()) {
            return n;
        }
    }
    format!("{base} ({}){ext}", std::process::id())
}

/// One type in the New menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// ".docx"
    pub ext: String,
    /// "Microsoft Word Document"
    pub label: String,
    pub content: Content,
}

/// What creating the file does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// an empty file
    Empty,
    /// a copy of this file
    Template(PathBuf),
}

/// Sorts by label (case-insensitive), drops a second type with a label already
/// listed and keeps [`MAX_TYPES`].
pub fn finish(mut v: Vec<Entry>) -> Vec<Entry> {
    v.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then(a.ext.cmp(&b.ext)));
    v.dedup_by(|b, a| a.label.eq_ignore_ascii_case(&b.label));
    v.truncate(MAX_TYPES);
    v
}

// ---- the cache -------------------------------------------------------------

static STARTED: AtomicBool = AtomicBool::new(false);
static CACHE: OnceLock<Arc<Vec<Entry>>> = OnceLock::new();

/// Starts the scan on a background thread, once per session.
pub fn ensure_started() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new().name("tsubame-shellnew".into()).spawn(|| {
        let _ = CACHE.set(Arc::new(scan()));
    });
}

/// The scanned types, or `None` while the scan still runs (or never started).
pub fn entries() -> Option<Arc<Vec<Entry>>> {
    CACHE.get().cloned()
}

// ---- the registry ----------------------------------------------------------

mod reg {
    use crate::wide;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::*;
    use windows::core::{PCWSTR, PWSTR};

    pub fn open(root: HKEY, path: &str) -> Option<HKEY> {
        let w = wide(path);
        let mut hk = HKEY::default();
        unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), Some(0), KEY_READ, &mut hk) }.is_ok().then_some(hk)
    }

    pub fn close(hk: HKEY) {
        unsafe {
            let _ = RegCloseKey(hk);
        }
    }

    pub fn subkeys(hk: HKEY) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 260];
            let mut n = buf.len() as u32;
            if unsafe { RegEnumKeyExW(hk, i, Some(PWSTR(buf.as_mut_ptr())), &mut n, None, None, None, None) }.is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..n as usize]));
        }
        out
    }

    /// A value's raw bytes and type (`name` empty: the default value).
    pub fn value(hk: HKEY, name: &str) -> Option<(REG_VALUE_TYPE, Vec<u8>)> {
        let n = wide(name);
        let mut ty = REG_VALUE_TYPE(0);
        let mut len = 0u32;
        unsafe {
            if RegQueryValueExW(hk, PCWSTR(n.as_ptr()), None, Some(&mut ty), None, Some(&mut len)) != ERROR_SUCCESS {
                return None;
            }
            let mut buf = vec![0u8; len as usize + 2];
            let mut len2 = buf.len() as u32;
            if RegQueryValueExW(hk, PCWSTR(n.as_ptr()), None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len2)) != ERROR_SUCCESS {
                return None;
            }
            buf.truncate(len2 as usize);
            Some((ty, buf))
        }
    }

    pub fn string(hk: HKEY, name: &str) -> Option<String> {
        let (ty, b) = value(hk, name)?;
        if ty != REG_SZ && ty != REG_EXPAND_SZ {
            return None;
        }
        let w: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        let s = String::from_utf16_lossy(&w).trim_end_matches('\0').to_string();
        // REG_EXPAND_SZ: %SystemRoot% and friends
        Some(if ty == REG_EXPAND_SZ { expand(&s) } else { s })
    }

    /// `%NAME%` replaced from the environment (unknown names stay as written).
    fn expand(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('%') {
            out.push_str(&rest[..i]);
            let tail = &rest[i + 1..];
            match tail.find('%') {
                Some(j) if j > 0 => match std::env::var(&tail[..j]) {
                    Ok(v) => {
                        out.push_str(&v);
                        rest = &tail[j + 1..];
                    }
                    Err(_) => {
                        out.push('%');
                        rest = tail;
                    }
                },
                _ => {
                    out.push('%');
                    rest = tail;
                }
            }
        }
        out.push_str(rest);
        out
    }

    pub fn exists(hk: HKEY, name: &str) -> bool {
        value(hk, name).is_some()
    }
}

/// The values of the `ShellNew` key at `path` under HKCR.
fn read_key(path: &str) -> Option<Raw> {
    use windows::Win32::System::Registry::*;
    let hk = reg::open(HKEY_CLASSES_ROOT, path)?;
    let raw = Raw {
        null_file: reg::exists(hk, "NullFile"),
        file_name: reg::string(hk, "FileName"),
        data: reg::value(hk, "Data").filter(|(t, _)| *t == REG_BINARY || *t == REG_SZ).map(|(_, b)| b),
        command: reg::exists(hk, "Command") || reg::exists(hk, "Handler"),
    };
    reg::close(hk);
    Some(raw)
}

/// The friendly name of what `.ext` opens as (HKCR\<ProgID> default value).
fn friendly_name(ext: &str) -> Option<String> {
    use windows::Win32::System::Registry::*;
    let hk = reg::open(HKEY_CLASSES_ROOT, ext)?;
    let progid = reg::string(hk, "");
    reg::close(hk);
    let progid = progid.filter(|p| !p.is_empty())?;
    let hk = reg::open(HKEY_CLASSES_ROOT, &progid)?;
    let name = reg::string(hk, "").filter(|s| !s.is_empty() && !s.starts_with('@'));
    reg::close(hk);
    name
}

/// The folders a relative `FileName` is looked up in.
fn template_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("APPDATA") {
        v.push(Path::new(&p).join("Microsoft").join("Windows").join("Templates"));
    }
    if let Some(p) = std::env::var_os("USERPROFILE") {
        v.push(Path::new(&p).join("Templates"));
    }
    if let Some(p) = std::env::var_os("SystemRoot") {
        v.push(Path::new(&p).join("ShellNew"));
    }
    v
}

/// Where a `Data` type's bytes are kept as a template file.
fn data_dir() -> PathBuf {
    std::env::temp_dir().join("Tsubame").join("shellnew")
}

/// Reads every registered type. Slow-ish (it walks `HKCR`): call on a thread.
pub fn scan() -> Vec<Entry> {
    use windows::Win32::System::Registry::*;
    let Some(root) = reg::open(HKEY_CLASSES_ROOT, "") else { return Vec::new() };
    let exts: Vec<String> = reg::subkeys(root).into_iter().filter(|k| k.starts_with('.') && k.len() > 1 && !is_skipped(k)).collect();
    reg::close(root);
    let dirs = template_dirs();
    let mut out = Vec::new();
    for ext in exts {
        // `.ext\ShellNew`, else `.ext\<ProgID>\ShellNew`
        let mut raw = read_key(&format!("{ext}\\ShellNew"));
        if raw.is_none()
            && let Some(hk) = reg::open(HKEY_CLASSES_ROOT, &ext)
        {
            let subs = reg::subkeys(hk);
            reg::close(hk);
            raw = subs.iter().find_map(|s| read_key(&format!("{ext}\\{s}\\ShellNew")));
        }
        let Some(source) = raw.as_ref().and_then(parse) else { continue };
        let content = match source {
            Source::Empty => Content::Empty,
            Source::Template(n) => match resolve_template(&n, &dirs, &|p| p.is_file()) {
                Some(p) => Content::Template(p),
                None => continue,
            },
            Source::Data(bytes) => {
                let dir = data_dir();
                let p = dir.join(format!("template{ext}"));
                if std::fs::create_dir_all(&dir).is_err() || std::fs::write(&p, &bytes).is_err() {
                    continue;
                }
                Content::Template(p)
            }
        };
        let label = label_for(&ext, friendly_name(&ext).as_deref());
        out.push(Entry { ext, label, content });
    }
    finish(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw() -> Raw {
        Raw::default()
    }

    #[test]
    fn parse_picks_the_way_a_file_is_made() {
        assert_eq!(parse(&raw()), None, "no value, no type");
        assert_eq!(parse(&Raw { null_file: true, ..raw() }), Some(Source::Empty));
        assert_eq!(parse(&Raw { file_name: Some("tpl.docx".into()), ..raw() }), Some(Source::Template("tpl.docx".into())));
        assert_eq!(parse(&Raw { data: Some(vec![1, 2, 3]), ..raw() }), Some(Source::Data(vec![1, 2, 3])));
        // a template beats a null file; the data beats a null file too
        assert_eq!(parse(&Raw { null_file: true, file_name: Some("t.rtf".into()), ..raw() }), Some(Source::Template("t.rtf".into())));
        assert_eq!(parse(&Raw { null_file: true, data: Some(vec![9]), ..raw() }), Some(Source::Data(vec![9])));
        // blank names and empty data are nothing
        assert_eq!(parse(&Raw { file_name: Some("  ".into()), data: Some(vec![]), ..raw() }), None);
        assert_eq!(parse(&Raw { file_name: Some("  ".into()), null_file: true, ..raw() }), Some(Source::Empty));
        // a command runs code: never
        assert_eq!(parse(&Raw { command: true, null_file: true, ..raw() }), None);
        // far too much data is refused
        assert_eq!(parse(&Raw { data: Some(vec![0; MAX_DATA + 1]), ..raw() }), None);
        assert!(parse(&Raw { data: Some(vec![0; MAX_DATA]), ..raw() }).is_some());
    }

    #[test]
    fn known_bad_types_are_skipped() {
        for e in [".txt", ".LNK", ".library-ms", ".bfc"] {
            assert!(is_skipped(e), "{e}");
        }
        assert!(!is_skipped(".docx") && !is_skipped(".bmp"));
    }

    #[test]
    fn templates_resolve_against_the_folders() {
        let dirs = vec![PathBuf::from(r"C:\T"), PathBuf::from(r"C:\Windows\ShellNew")];
        let has = |p: &Path| p == Path::new(r"C:\Windows\ShellNew\a.docx") || p == Path::new(r"D:\x\b.xlsx");
        assert_eq!(resolve_template("a.docx", &dirs, &has), Some(PathBuf::from(r"C:\Windows\ShellNew\a.docx")));
        assert_eq!(resolve_template(r"D:\x\b.xlsx", &dirs, &has), Some(PathBuf::from(r"D:\x\b.xlsx")));
        assert_eq!(resolve_template(r"D:\x\gone.xlsx", &dirs, &has), None);
        assert_eq!(resolve_template("nope.rtf", &dirs, &has), None);
    }

    #[test]
    fn labels_are_legal_file_names() {
        assert_eq!(label_for(".bmp", Some("Bitmap image")), "Bitmap image");
        assert_eq!(label_for(".bmp", Some("Bitmap: \"image\"?")), "Bitmap image");
        assert_eq!(label_for(".bmp", None), "BMP file");
        assert_eq!(label_for(".bmp", Some("::")), "BMP file");
        assert_eq!(label_for(".rtf", Some("")), "RTF file");
    }

    #[test]
    fn unique_names_count_up() {
        let none = |_: &str| false;
        assert_eq!(unique_name("Image", ".png", &none), "Image.png");
        let some = |n: &str| ["image.png", "image (2).png"].contains(&n);
        assert_eq!(unique_name("Image", ".png", &some), "Image (3).png");
        let all = |_: &str| true;
        assert!(unique_name("Image", ".png", &all).starts_with("Image ("));
        // case-insensitive: the closure gets lower case
        let case = |n: &str| n == "image.png";
        assert_eq!(unique_name("Image", ".png", &case), "Image (2).png");
    }

    #[test]
    fn the_list_is_sorted_deduped_and_capped() {
        let e = |ext: &str, label: &str| Entry { ext: ext.into(), label: label.into(), content: Content::Empty };
        let v = finish(vec![e(".b", "zeta"), e(".a", "Alpha"), e(".c", "alpha"), e(".d", "Mid")]);
        assert_eq!(v.iter().map(|x| x.ext.as_str()).collect::<Vec<_>>(), [".a", ".d", ".b"], "alpha twice: first extension kept");
        let many: Vec<Entry> = (0..40).map(|i| e(&format!(".e{i}"), &format!("Type {i:02}"))).collect();
        assert_eq!(finish(many).len(), MAX_TYPES);
    }

    #[test]
    fn the_registry_scan_runs_and_keeps_its_promises() {
        // machine-dependent content, machine-independent rules
        let v = scan();
        assert!(v.len() <= MAX_TYPES);
        for e in &v {
            assert!(e.ext.starts_with('.') && !is_skipped(&e.ext), "{e:?}");
            assert!(!e.label.is_empty());
            if let Content::Template(p) = &e.content {
                assert!(p.is_file(), "{e:?}");
            }
        }
    }
}
