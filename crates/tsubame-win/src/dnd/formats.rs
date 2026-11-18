//! Pure parts of shell data transfer: DROPEFFECT and key-state values,
//! `CF_HDROP` (`DROPFILES`) building and parsing, the `Preferred DropEffect`
//! DWORD, `FileGroupDescriptorW` parsing, path validation and volume keys.
//! No COM and no disk access here, so all of it is unit tested.

use std::path::{Component, Path, PathBuf, Prefix};

/// `DROPEFFECT_*`
pub const EFFECT_NONE: u32 = 0;
pub const EFFECT_COPY: u32 = 1;
pub const EFFECT_MOVE: u32 = 2;
pub const EFFECT_LINK: u32 = 4;

/// `MK_*` bits of the `grfKeyState` that OLE passes to drop targets and sources.
pub const MK_LBUTTON: u32 = 0x1;
pub const MK_RBUTTON: u32 = 0x2;
pub const MK_SHIFT: u32 = 0x4;
pub const MK_CONTROL: u32 = 0x8;
pub const MK_MBUTTON: u32 = 0x10;
pub const MK_ALT: u32 = 0x20;

/// Most paths one clipboard or drop payload may carry. A bigger list is cut
/// (the rest is counted as rejected).
pub const MAX_PATHS: usize = 100_000;

/// `sizeof(DROPFILES)`: pFiles u32, pt (i32, i32), fNC i32, fWide i32.
const DROPFILES_SIZE: usize = 20;

/// `CF_HDROP` bytes: a `DROPFILES` header, then the wide paths, each
/// NUL-terminated, then one more NUL.
pub fn hdrop_bytes(paths: &[PathBuf]) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let mut out = Vec::with_capacity(DROPFILES_SIZE + paths.iter().map(|p| p.as_os_str().len() * 2 + 2).sum::<usize>() + 2);
    out.extend_from_slice(&(DROPFILES_SIZE as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&1i32.to_le_bytes());
    for p in paths {
        for u in p.as_os_str().encode_wide() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// The paths in `CF_HDROP` bytes. `ansi` decodes the narrow form (fWide = 0);
/// pass `None` to refuse it. Garbage gives `None`, never a panic. At most
/// [`MAX_PATHS`] entries.
/// Decodes ANSI (narrow) text, e.g. with `MultiByteToWideChar(CP_ACP)`.
pub type Ansi<'a> = &'a dyn Fn(&[u8]) -> String;

pub fn parse_hdrop(bytes: &[u8], ansi: Option<Ansi>) -> Option<Vec<PathBuf>> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.len() < DROPFILES_SIZE {
        return None;
    }
    let off = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
    let wide = i32::from_le_bytes(bytes[16..20].try_into().ok()?) != 0;
    if off < DROPFILES_SIZE || off > bytes.len() {
        return None;
    }
    let body = &bytes[off..];
    let mut out = Vec::new();
    if wide {
        let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        for s in units.split(|&u| u == 0) {
            if s.is_empty() || out.len() >= MAX_PATHS {
                break;
            }
            out.push(PathBuf::from(std::ffi::OsString::from_wide(s)));
        }
    } else {
        let dec = ansi?;
        for s in body.split(|&b| b == 0) {
            if s.is_empty() || out.len() >= MAX_PATHS {
                break;
            }
            out.push(PathBuf::from(dec(s)));
        }
    }
    Some(out)
}

/// `Preferred DropEffect` / `Performed DropEffect` and friends: one DWORD.
pub fn effect_bytes(effect: u32) -> Vec<u8> {
    effect.to_le_bytes().to_vec()
}

pub fn parse_effect(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?))
}

/// `CF_UNICODETEXT` bytes (NUL-terminated UTF-16).
pub fn text_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect()
}

/// Why a path from the clipboard or a drop was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    Empty,
    /// not absolute (relative, or drive-relative like `C:foo`)
    Relative,
    /// `\\.\`, `\\?\GLOBALROOT`, `\\?\Volume{..}` and other device namespaces
    Device,
    /// a `..` or `.` component
    DotDot,
    /// a `:` after the drive (alternate data stream) or another bad character
    BadChar,
    /// CON, NUL, COM1, ... as a component
    Reserved,
    TooLong,
}

/// Longest path accepted (the long-path limit).
pub const MAX_PATH_LEN: usize = 32_000;

const BAD_CHARS: [char; 6] = ['<', '>', '"', '|', '?', '*'];

/// Accepts a plain absolute file system path (`C:\..`, `\\server\share\..`,
/// or their `\\?\` forms) and returns it normalized to the plain form.
/// Refuses device paths, `\\?\GLOBALROOT`, volume GUID paths, `..`, alternate
/// data streams, reserved device names and control characters. Never touches
/// the disk.
pub fn validate(p: &Path) -> Result<PathBuf, PathError> {
    let s = p.as_os_str();
    if s.is_empty() {
        return Err(PathError::Empty);
    }
    if s.len() > MAX_PATH_LEN {
        return Err(PathError::TooLong);
    }
    if s.to_str().is_none() {
        // unpaired surrogate: refuse rather than guess
        return Err(PathError::BadChar);
    }
    let mut comps = p.components();
    let plain = match comps.next() {
        Some(Component::Prefix(pre)) => match pre.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) if d.is_ascii_alphabetic() => {
                let mut b = PathBuf::from(format!("{}:\\", (d as char).to_ascii_uppercase()));
                if comps.next() != Some(Component::RootDir) {
                    return Err(PathError::Relative);
                }
                push_rest(&mut b, comps)?;
                b
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                let (sv, sh) = (server.to_string_lossy(), share.to_string_lossy());
                if sv.is_empty() || sh.is_empty() || sv == "." || sv == "?" || sv.contains(BAD_CHARS) || sh.contains(BAD_CHARS) || sv.contains(':') || sh.contains(':') {
                    return Err(PathError::Device);
                }
                let mut b = PathBuf::from(format!(r"\\{sv}\{sh}\"));
                let _ = comps.next(); // RootDir
                push_rest(&mut b, comps)?;
                b
            }
            Prefix::DeviceNS(_) | Prefix::Verbatim(_) => return Err(PathError::Device),
            _ => return Err(PathError::Device),
        },
        _ => return Err(PathError::Relative),
    };
    Ok(plain)
}

fn push_rest<'a>(b: &mut PathBuf, comps: impl Iterator<Item = Component<'a>>) -> Result<(), PathError> {
    for c in comps {
        match c {
            Component::Normal(n) => {
                let n = n.to_string_lossy();
                check_name(&n)?;
                b.push(&*n);
            }
            Component::CurDir | Component::ParentDir => return Err(PathError::DotDot),
            _ => return Err(PathError::BadChar),
        }
    }
    Ok(())
}

/// One path component: no `:` (streams), wildcards, control characters,
/// or reserved device names.
pub fn check_name(n: &str) -> Result<(), PathError> {
    if n.is_empty() || n == "." || n == ".." {
        return Err(PathError::DotDot);
    }
    if n.chars().any(|c| (c as u32) < 0x20 || c == ':' || c == '/' || c == '\\' || BAD_CHARS.contains(&c)) {
        return Err(PathError::BadChar);
    }
    let stem = n.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0')
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && matches!(&stem[3..], "\u{B9}" | "\u{B2}" | "\u{B3}"));
    if reserved {
        return Err(PathError::Reserved);
    }
    Ok(())
}

/// Validates a list; returns the good paths and how many were refused.
pub fn validate_all(paths: &[PathBuf]) -> (Vec<PathBuf>, usize) {
    let mut ok = Vec::with_capacity(paths.len());
    let mut bad = 0;
    for p in paths.iter().take(MAX_PATHS) {
        match validate(p) {
            Ok(v) => ok.push(v),
            Err(_) => bad += 1,
        }
    }
    (ok, bad + paths.len().saturating_sub(MAX_PATHS))
}

/// The volume a validated path lives on, as a comparable key: `C:` or
/// `\\server\share` (lowercase). Pure: mounted folders and `subst` drives
/// are not resolved, so they count by their letter.
pub fn volume_key(p: &Path) -> Option<String> {
    match p.components().next()? {
        Component::Prefix(pre) => match pre.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some(format!("{}:", (d as char).to_ascii_lowercase())),
            Prefix::UNC(s, h) | Prefix::VerbatimUNC(s, h) => Some(format!(r"\\{}\{}", s.to_string_lossy().to_lowercase(), h.to_string_lossy().to_lowercase())),
            _ => None,
        },
        _ => None,
    }
}

pub fn same_volume(a: &Path, b: &Path) -> bool {
    matches!((volume_key(a), volume_key(b)), (Some(x), Some(y)) if x == y)
}

/// Case-insensitive path equality (ASCII folding plus trailing separators),
/// enough for "is this the same folder" checks on paths from the same source.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
    let (a, b) = (a.trim_end_matches('\\'), b.trim_end_matches('\\'));
    a.len() == b.len() && a.chars().zip(b.chars()).all(|(x, y)| x.to_lowercase().eq(y.to_lowercase()))
}

/// `inner` is `outer` or below it.
pub fn is_within(inner: &Path, outer: &Path) -> bool {
    let (i, o) = (inner.to_string_lossy().to_lowercase(), outer.to_string_lossy().to_lowercase());
    let o = o.trim_end_matches('\\');
    i.trim_end_matches('\\') == o || (i.starts_with(o) && i.as_bytes().get(o.len()) == Some(&b'\\'))
}

/// One entry of a `FileGroupDescriptorW` (virtual files: zip folders, mail
/// attachments).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDescriptor {
    /// relative path as the source gave it (may contain `\`)
    pub name: String,
    pub size: Option<u64>,
    pub is_dir: bool,
}

const FD_ATTRIBUTES: u32 = 0x4;
const FD_FILESIZE: u32 = 0x40;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// `sizeof(FILEDESCRIPTORW)`
pub const FILEDESCRIPTORW_SIZE: usize = 592;

/// Parses `FileGroupDescriptorW` bytes. Truncated input gives the entries that
/// fit. At most [`MAX_PATHS`] entries.
pub fn parse_group_descriptor(bytes: &[u8]) -> Vec<FileDescriptor> {
    let Some(n) = bytes.get(0..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize) else { return Vec::new() };
    let mut out = Vec::new();
    for i in 0..n.min(MAX_PATHS) {
        let base = 4 + i * FILEDESCRIPTORW_SIZE;
        let Some(d) = bytes.get(base..base + FILEDESCRIPTORW_SIZE) else { break };
        let rd = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        let flags = rd(0);
        let attrs = rd(36);
        let size = (flags & FD_FILESIZE != 0).then(|| ((rd(64) as u64) << 32) | rd(68) as u64);
        let units: Vec<u16> = d[72..72 + 520].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        out.push(FileDescriptor { name: String::from_utf16_lossy(&units[..end]), size, is_dir: flags & FD_ATTRIBUTES != 0 && attrs & FILE_ATTRIBUTE_DIRECTORY != 0 });
    }
    out
}

/// Builds `FileGroupDescriptorW` bytes (tests, and a source that offers
/// virtual files).
pub fn group_descriptor_bytes(items: &[FileDescriptor]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + items.len() * FILEDESCRIPTORW_SIZE);
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for it in items {
        let mut d = vec![0u8; FILEDESCRIPTORW_SIZE];
        let mut flags = FD_ATTRIBUTES;
        if let Some(s) = it.size {
            flags |= FD_FILESIZE;
            d[64..68].copy_from_slice(&((s >> 32) as u32).to_le_bytes());
            d[68..72].copy_from_slice(&(s as u32).to_le_bytes());
        }
        d[0..4].copy_from_slice(&flags.to_le_bytes());
        let attrs = if it.is_dir { FILE_ATTRIBUTE_DIRECTORY } else { 0x80 };
        d[36..40].copy_from_slice(&attrs.to_le_bytes());
        for (k, u) in it.name.encode_utf16().take(259).enumerate() {
            d[72 + k * 2..74 + k * 2].copy_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&d);
    }
    out
}

/// A virtual file's relative name made safe to create under a staging
/// folder: every component checked, no `..`, no drive or root.
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['\\', '/']) {
        if part.is_empty() {
            continue;
        }
        check_name(part).ok()?;
        // trailing dots and spaces are dropped by Win32 and could alias
        if part.ends_with('.') || part.ends_with(' ') {
            return None;
        }
        out.push(part);
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn hdrop_round_trip() {
        let v = vec![p(r"C:\a b\x.txt"), p(r"D:\ünï\😀.bin"), p(r"\\srv\share\f")];
        let b = hdrop_bytes(&v);
        assert_eq!(parse_hdrop(&b, None).unwrap(), v);
        // empty list
        assert_eq!(parse_hdrop(&hdrop_bytes(&[]), None).unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn hdrop_garbage_never_panics() {
        let b = hdrop_bytes(&[p(r"C:\a"), p(r"C:\bb")]);
        for n in 0..b.len() {
            let _ = parse_hdrop(&b[..n], None);
        }
        let mut bad = b.clone();
        bad[0] = 0xff;
        assert!(parse_hdrop(&bad, None).is_none());
        // unterminated last string: still parsed
        assert_eq!(parse_hdrop(&b[..b.len() - 4], None).unwrap(), vec![p(r"C:\a"), p(r"C:\bb")]);
    }

    #[test]
    fn hdrop_ansi_needs_a_decoder() {
        let mut b = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        b.extend_from_slice(b"C:\\x\0C:\\y\0\0");
        assert!(parse_hdrop(&b, None).is_none());
        let dec = |s: &[u8]| String::from_utf8_lossy(s).into_owned();
        assert_eq!(parse_hdrop(&b, Some(&dec)).unwrap(), vec![p(r"C:\x"), p(r"C:\y")]);
    }

    #[test]
    fn effect_round_trip() {
        for e in [EFFECT_NONE, EFFECT_COPY, EFFECT_MOVE, EFFECT_LINK, EFFECT_COPY | EFFECT_MOVE] {
            assert_eq!(parse_effect(&effect_bytes(e)), Some(e));
        }
        assert_eq!(parse_effect(&[1, 2]), None);
    }

    #[test]
    fn validation() {
        assert_eq!(validate(&p(r"c:\Users\x.txt")).unwrap(), p(r"C:\Users\x.txt"));
        assert_eq!(validate(&p(r"\\?\C:\long\x")).unwrap(), p(r"C:\long\x"));
        assert_eq!(validate(&p(r"\\server\share\dir\f")).unwrap(), p(r"\\server\share\dir\f"));
        assert_eq!(validate(&p(r"\\?\UNC\server\share\f")).unwrap(), p(r"\\server\share\f"));
        assert_eq!(validate(&p("C:\\")).unwrap(), p("C:\\"));
        assert_eq!(validate(&p("")), Err(PathError::Empty));
        assert_eq!(validate(&p(r"rel\x")), Err(PathError::Relative));
        assert_eq!(validate(&p(r"C:rel")), Err(PathError::Relative));
        assert_eq!(validate(&p(r"\x")), Err(PathError::Relative));
        assert_eq!(validate(&p(r"\\.\PhysicalDrive0")), Err(PathError::Device));
        assert_eq!(validate(&p(r"\\?\GLOBALROOT\Device\HarddiskVolume1\x")), Err(PathError::Device));
        assert_eq!(validate(&p(r"\\?\Volume{12345678-1234-1234-1234-123456789abc}\x")), Err(PathError::Device));
        assert_eq!(validate(&p(r"\\.\C:\x")), Err(PathError::Device));
        assert_eq!(validate(&p(r"C:\a\..\Windows")), Err(PathError::DotDot));
        assert_eq!(validate(&p(r"C:\a\file.txt:stream")), Err(PathError::BadChar));
        assert_eq!(validate(&p(r"C:\a\file.txt::$DATA")), Err(PathError::BadChar));
        assert_eq!(validate(&p(r"C:\a\NUL")), Err(PathError::Reserved));
        assert_eq!(validate(&p(r"C:\a\com1.txt")), Err(PathError::Reserved));
        assert_eq!(validate(&p(r"C:\a\lpt9")), Err(PathError::Reserved));
        assert!(validate(&p(r"C:\a\com10")).is_ok());
        assert!(validate(&p(r"C:\a\console")).is_ok());
        assert_eq!(validate(&p("C:\\a\\x\u{1}y")), Err(PathError::BadChar));
        assert_eq!(validate(&p(r"C:\a\*")), Err(PathError::BadChar));
        let long = format!("C:\\{}", "a".repeat(MAX_PATH_LEN));
        assert_eq!(validate(&p(&long)), Err(PathError::TooLong));
        let (ok, bad) = validate_all(&[p(r"C:\x"), p(r"\\.\nul"), p("rel")]);
        assert_eq!((ok.len(), bad), (1, 2));
    }

    #[test]
    fn volumes() {
        assert!(same_volume(&p(r"C:\a"), &p(r"c:\b\c")));
        assert!(!same_volume(&p(r"C:\a"), &p(r"D:\a")));
        assert!(same_volume(&p(r"\\Srv\Share\a"), &p(r"\\srv\share\b")));
        assert!(!same_volume(&p(r"\\srv\share\a"), &p(r"\\srv\other\a")));
        assert!(!same_volume(&p(r"\\srv\share\a"), &p(r"C:\a")));
        assert!(same_volume(&p(r"\\?\C:\a"), &p(r"C:\b")));
        assert!(!same_volume(&p("rel"), &p("rel")));
    }

    #[test]
    fn paths_compare() {
        assert!(same_path(&p(r"C:\A\b"), &p(r"c:\a\B\")));
        assert!(!same_path(&p(r"C:\a\b"), &p(r"C:\a\bc")));
        assert!(is_within(&p(r"C:\a\b\c"), &p(r"C:\A\b")));
        assert!(is_within(&p(r"C:\a\b"), &p(r"C:\a\b\")));
        assert!(!is_within(&p(r"C:\a\bc"), &p(r"C:\a\b")));
    }

    #[test]
    fn group_descriptor_round_trip() {
        let items = vec![
            FileDescriptor { name: "dir".into(), size: None, is_dir: true },
            FileDescriptor { name: r"dir\ünï.txt".into(), size: Some(5_000_000_000), is_dir: false },
            FileDescriptor { name: "a.eml".into(), size: Some(12), is_dir: false },
        ];
        let b = group_descriptor_bytes(&items);
        assert_eq!(b.len(), 4 + 3 * FILEDESCRIPTORW_SIZE);
        assert_eq!(parse_group_descriptor(&b), items);
        // truncated: what fits
        assert_eq!(parse_group_descriptor(&b[..4 + FILEDESCRIPTORW_SIZE + 10]).len(), 1);
        assert!(parse_group_descriptor(&[1, 2]).is_empty());
    }

    #[test]
    fn safe_relative_names() {
        assert_eq!(safe_relative(r"dir\sub\x.txt"), Some(p(r"dir\sub\x.txt")));
        assert_eq!(safe_relative(r"\lead\x"), Some(p(r"lead\x")));
        assert_eq!(safe_relative(r"..\x"), None);
        assert_eq!(safe_relative(r"C:\x"), None);
        assert_eq!(safe_relative("x.txt:ads"), None);
        assert_eq!(safe_relative("evil. "), None);
        assert_eq!(safe_relative("CON"), None);
        assert_eq!(safe_relative(""), None);
    }

    #[test]
    fn text() {
        assert_eq!(text_bytes("ab"), vec![b'a', 0, b'b', 0, 0, 0]);
    }
}
