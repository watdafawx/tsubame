//! Command lines: reading what the shell passed to the shim, and building what the shim passes
//! on. Pure string work, no Win32.
//!
//! Why not `std::env::args`? The shell runs `"<shim>" "%1"`. For a drive root, `%1` is `C:\`, so
//! the line is `"C:\"`, and the standard argument rules read `\"` as an escaped quote: the path
//! comes out as `C:"`. A Windows path can never contain a double quote, so the shim reads the
//! raw line and takes everything between the first pair of quotes literally.

/// What the shim was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Nothing usable was passed (the exe was started by hand).
    Empty,
    /// `--remove`: take the registration out (what the uninstaller calls).
    Remove,
    /// An absolute file-system folder path: the normal case.
    Open(String),
    /// Anything else (`::{CLSID}` shell names, relative text, flags): Explorer gets it untouched.
    Explorer(String),
}

/// The largest line `CreateProcess` accepts is 32,767 UTF-16 units; leave room for the exe path.
pub const MAX_ARG_CHARS: usize = 32_000;

/// Splits the raw command line into (program token, rest). The program token follows the
/// `CommandLineToArgvW` rule for argv[0]: quoted, or up to the first white space.
pub fn split_program(raw: &str) -> (&str, &str) {
    let raw = raw.trim_start();
    if let Some(rest) = raw.strip_prefix('"') {
        match rest.find('"') {
            Some(i) => (&rest[..i], rest[i + 1..].trim()),
            None => (rest, ""),
        }
    } else {
        match raw.find([' ', '\t']) {
            Some(i) => (&raw[..i], raw[i..].trim()),
            None => (raw, ""),
        }
    }
}

/// True for `C:\...`, `C:/...`, bare `C:`, `\\server\share...` and `\\?\...`: paths that mean the
/// same thing whatever the current directory is. Device paths (`\\.\`) are not folders.
pub fn is_absolute_path(p: &str) -> bool {
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return b.len() == 2 || b[2] == b'\\' || b[2] == b'/';
    }
    if let Some(rest) = p.strip_prefix(r"\\") {
        if rest.starts_with(['.', '?']) {
            // `\\?\C:\x` and `\\?\UNC\s\x` are fine, `\\.\` is a device
            return rest.starts_with('?') && rest.as_bytes().get(1) == Some(&b'\\') && rest.len() > 2;
        }
        return rest.chars().next().is_some_and(|c| c != '\\');
    }
    false
}

/// Characters that cannot be in a path and must not reach a child's command line.
fn clean(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= MAX_ARG_CHARS && !s.chars().any(|c| c == '"' || c.is_control())
}

/// Reads the shim's own raw command line (`GetCommandLineW`).
pub fn request_from_command_line(raw: &str) -> Request {
    let (_, rest) = split_program(raw);
    request_from_rest(rest)
}

pub fn request_from_rest(rest: &str) -> Request {
    let rest = rest.trim();
    if rest.is_empty() {
        return Request::Empty;
    }
    if rest.eq_ignore_ascii_case("--remove") || rest.eq_ignore_ascii_case("--restore-explorer") {
        return Request::Remove;
    }
    // first quoted token, taken literally; otherwise the whole text (an unquoted `%1`)
    let arg = match rest.strip_prefix('"') {
        Some(inner) => inner.split('"').next().unwrap_or(""),
        None => rest,
    };
    let arg = arg.trim();
    if !clean(arg) {
        return if arg.is_empty() { Request::Empty } else { Request::Explorer(arg.chars().filter(|c| *c != '"' && !c.is_control()).take(MAX_ARG_CHARS).collect()) };
    }
    if is_absolute_path(arg) {
        // `C:` alone means "the current directory of drive C": the root is the only sane reading
        if arg.len() == 2 { Request::Open(format!("{arg}\\")) } else { Request::Open(arg.to_string()) }
    } else {
        Request::Explorer(arg.to_string())
    }
}

/// Quotes one argument by the rules every Windows CRT (and Rust's std) reads back exactly:
/// backslashes are literal except in front of a quote, and the run in front of the closing
/// quote is doubled, so `D:\x\` and `C:\` survive.
pub fn quote_arg(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut slashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => {
                slashes += 1;
                out.push('\\');
            }
            '"' => {
                // double the run, then escape the quote
                out.extend(std::iter::repeat_n('\\', slashes + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                slashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes));
    out.push('"');
    out
}

/// `"<exe>" "<path>"`: how Tsubame is started for a folder (`docs/config.md`, command line:
/// `tsubame [options] [path ...]`).
pub fn app_command_line(app: &str, path: &str) -> String {
    format!("{} {}", quote_arg(app), quote_arg(path))
}

/// How the fallback hands a folder to `explorer.exe`. Explorer does not read its line with the
/// CRT rules, so a trailing backslash is handled by dropping it for everything except a root,
/// and roots go unquoted when they have no space (`C:\`), otherwise with a `.` appended
/// (`"\\server\my share\."` names the same folder and ends in no backslash).
pub fn explorer_command_line(explorer: &str, target: &str) -> String {
    let arg = explorer_arg(target);
    format!("{} {}", quote_arg(explorer), arg)
}

fn explorer_arg(target: &str) -> String {
    let t = target.trim();
    if !t.ends_with('\\') {
        return plain_quote(t);
    }
    let trimmed = t.trim_end_matches('\\');
    let is_root = trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' || is_unc_root(t);
    if !is_root {
        return plain_quote(trimmed);
    }
    if t.contains(' ') { plain_quote(&format!("{t}.")) } else { t.to_string() }
}

fn is_unc_root(t: &str) -> bool {
    // \\server\share\  or  \\?\C:\  or  \\?\UNC\server\share\
    let body = t.trim_end_matches('\\');
    if let Some(r) = body.strip_prefix(r"\\?\UNC\") {
        return r.matches('\\').count() == 1;
    }
    if let Some(r) = body.strip_prefix(r"\\?\") {
        return r.len() == 2 && r.as_bytes()[1] == b':';
    }
    if let Some(r) = body.strip_prefix(r"\\") {
        return r.matches('\\').count() == 1;
    }
    false
}

/// Wrap in quotes without any escaping (paths cannot contain quotes; `clean` guarantees it).
fn plain_quote(s: &str) -> String {
    format!("\"{s}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference reader for the CRT quoting rules, to prove `quote_arg` round-trips.
    fn crt_parse_one(quoted: &str) -> String {
        let chars: Vec<char> = quoted.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        assert_eq!(chars[0], '"');
        i += 1;
        while i < chars.len() {
            let mut slashes = 0;
            while i < chars.len() && chars[i] == '\\' {
                slashes += 1;
                i += 1;
            }
            if i < chars.len() && chars[i] == '"' {
                out.extend(std::iter::repeat_n('\\', slashes / 2));
                if slashes % 2 == 1 {
                    out.push('"');
                    i += 1;
                } else {
                    return out; // closing quote
                }
            } else {
                out.extend(std::iter::repeat_n('\\', slashes));
                if i < chars.len() {
                    out.push(chars[i]);
                    i += 1;
                }
            }
        }
        panic!("unterminated: {quoted}");
    }

    #[test]
    fn split_program_quoted_and_bare() {
        assert_eq!(split_program(r#""C:\a b\h.exe" "C:\x""#), (r"C:\a b\h.exe", r#""C:\x""#));
        assert_eq!(split_program(r"h.exe C:\x"), ("h.exe", r"C:\x"));
        assert_eq!(split_program(r"h.exe"), ("h.exe", ""));
        assert_eq!(split_program(r#""h.exe""#), ("h.exe", ""));
        assert_eq!(split_program(""), ("", ""));
        assert_eq!(split_program(r#"  "C:\h.exe"    "C:\x"  "#), (r"C:\h.exe", r#""C:\x""#));
    }

    fn open(s: &str) -> Request {
        request_from_command_line(&format!(r#""C:\i\tsubame-handler.exe" {s}"#))
    }

    #[test]
    fn plain_folder() {
        assert_eq!(open(r#""C:\Users\me\Docs""#), Request::Open(r"C:\Users\me\Docs".into()));
    }

    #[test]
    fn spaces_unicode_and_astral_chars() {
        assert_eq!(open(r#""D:\My Files\日本語 フォルダ\café 😀""#), Request::Open("D:\\My Files\\日本語 フォルダ\\café 😀".into()));
    }

    #[test]
    fn drive_root_with_trailing_backslash_is_not_an_escaped_quote() {
        assert_eq!(open(r#""C:\""#), Request::Open(r"C:\".into()));
        assert_eq!(open(r#""D:\folder\""#), Request::Open(r"D:\folder\".into()));
    }

    #[test]
    fn bare_drive_means_its_root() {
        assert_eq!(open(r#""C:""#), Request::Open(r"C:\".into()));
    }

    #[test]
    fn unc_and_long_prefix_paths() {
        assert_eq!(open(r#""\\server\share\dir""#), Request::Open(r"\\server\share\dir".into()));
        assert_eq!(open(r#""\\?\C:\very\long""#), Request::Open(r"\\?\C:\very\long".into()));
        assert_eq!(open(r#""\\?\UNC\server\share\d""#), Request::Open(r"\\?\UNC\server\share\d".into()));
        assert_eq!(open(r#""\\server\share\""#), Request::Open(r"\\server\share\".into()));
    }

    #[test]
    fn device_and_malformed_unc_go_to_explorer() {
        assert!(matches!(open(r#""\\.\PhysicalDrive0""#), Request::Explorer(_)));
        assert!(matches!(open(r#""\\\\""#), Request::Explorer(_)));
        assert!(matches!(open(r#""\\""#), Request::Explorer(_)));
    }

    #[test]
    fn unquoted_path_with_spaces_is_kept_whole() {
        assert_eq!(open(r"C:\Program Files\x"), Request::Open(r"C:\Program Files\x".into()));
    }

    #[test]
    fn shell_names_and_relative_text_go_to_explorer_untouched() {
        assert_eq!(open(r#""::{20D04FE0-3AEA-1069-A2D8-08002B30309D}""#), Request::Explorer("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".into()));
        assert_eq!(open(r#""shell:Downloads""#), Request::Explorer("shell:Downloads".into()));
        assert_eq!(open(r#""docs\x""#), Request::Explorer(r"docs\x".into()));
    }

    #[test]
    fn a_dash_argument_is_never_handed_to_the_app() {
        // the app would read it as an option
        assert_eq!(open(r#""--new-window""#), Request::Explorer("--new-window".into()));
        assert_eq!(open(r#""-x""#), Request::Explorer("-x".into()));
    }

    #[test]
    fn control_characters_and_overlong_text_never_become_open() {
        assert!(matches!(open("\"C:\\a\u{1}b\""), Request::Explorer(_)));
        let long = format!("\"C:\\{}\"", "a".repeat(MAX_ARG_CHARS + 10));
        assert!(matches!(open(&long), Request::Explorer(_)));
    }

    #[test]
    fn empty_and_remove() {
        assert_eq!(request_from_command_line(r#""C:\i\h.exe""#), Request::Empty);
        assert_eq!(open(r#""""#), Request::Empty);
        assert_eq!(open("--remove"), Request::Remove);
        assert_eq!(open("--RESTORE-EXPLORER"), Request::Remove);
        assert_eq!(open(r#""--remove""#), Request::Explorer("--remove".into()), "a quoted folder named --remove is a folder text, not a command");
    }

    #[test]
    fn quote_arg_round_trips_the_hard_cases() {
        for s in [
            r"C:\",
            r"C:\dir\",
            r"C:\a b\c",
            r"\\server\share\",
            r"C:\x\\",
            "C:\\q\"x",
            "C:\\q\\\"x\\",
            "日本語\\😀\\",
            "",
        ] {
            let q = quote_arg(s);
            assert_eq!(crt_parse_one(&q), s, "quoted as {q}");
        }
    }

    #[test]
    fn app_command_line_shape() {
        assert_eq!(app_command_line(r"C:\i\tsubame.exe", r"D:\x y"), r#""C:\i\tsubame.exe" "D:\x y""#);
        // root: the trailing backslash is doubled so the quote is not escaped
        assert_eq!(app_command_line(r"C:\i\tsubame.exe", r"C:\"), r#""C:\i\tsubame.exe" "C:\\""#);
    }

    #[test]
    fn explorer_command_line_never_ends_in_backslash_quote() {
        let e = r"C:\Windows\explorer.exe";
        assert_eq!(explorer_command_line(e, r"D:\a b\c"), r#""C:\Windows\explorer.exe" "D:\a b\c""#);
        assert_eq!(explorer_command_line(e, r"D:\a b\c\"), r#""C:\Windows\explorer.exe" "D:\a b\c""#);
        assert_eq!(explorer_command_line(e, r"C:\"), r#""C:\Windows\explorer.exe" C:\"#);
        assert_eq!(explorer_command_line(e, r"\\srv\my share\"), r#""C:\Windows\explorer.exe" "\\srv\my share\.""#);
        assert_eq!(explorer_command_line(e, r"\\srv\share\"), r#""C:\Windows\explorer.exe" \\srv\share\"#);
        assert_eq!(explorer_command_line(e, r"\\?\C:\"), r#""C:\Windows\explorer.exe" \\?\C:\"#);
        assert_eq!(explorer_command_line(e, "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"), r#""C:\Windows\explorer.exe" "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}""#);
    }
}
