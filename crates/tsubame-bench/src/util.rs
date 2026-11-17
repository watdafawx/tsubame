//! Small path and text helpers shared by the bench commands.

/// Strip trailing separators from a directory path (keeps `C:\` and `\\?\C:\`).
pub fn normalize_dir(path: &str) -> String {
    let mut p = path.trim().to_string();
    while p.len() > 3 && (p.ends_with('\\') || p.ends_with('/')) {
        p.pop();
    }
    p
}

/// Prefix a path with `\\?\` (or `\\?\UNC\`) so it can exceed MAX_PATH.
pub fn extended(path: &str) -> String {
    let p = normalize_dir(path);
    if p.starts_with(r"\\?\") {
        p
    } else if let Some(rest) = p.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{rest}")
    } else {
        format!(r"\\?\{p}")
    }
}

/// `\\?\C:\dir` -> `C:\dir` (for human-readable output).
pub fn unextended(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

/// UTF-16, NUL terminated, `\\?\` prefixed directory path.
pub fn extended_wide(path: &str) -> Vec<u16> {
    let mut s = extended(path);
    s.push('\0');
    s.encode_utf16().collect()
}

/// UTF-16, NUL terminated search pattern for `FindFirstFileExW`.
pub fn search_pattern_wide(path: &str) -> Vec<u16> {
    let mut s = extended(path);
    s.push('\\');
    s.push('*');
    s.push('\0');
    s.encode_utf16().collect()
}

/// Format a UTF-16 name lossily (output only, never the hot path).
pub fn utf16_to_string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_prefixes_local_paths() {
        assert_eq!(extended(r"C:\a\b"), r"\\?\C:\a\b");
        assert_eq!(extended(r"C:\a\b\"), r"\\?\C:\a\b");
        assert_eq!(extended(r"\\?\C:\a"), r"\\?\C:\a");
        assert_eq!(extended(r"\\server\share\x"), r"\\?\UNC\server\share\x");
        assert_eq!(unextended(r"\\?\C:\a"), r"C:\a");
        assert_eq!(unextended(r"\\?\UNC\server\share"), r"\\server\share");
    }

    #[test]
    fn pattern_appends_star() {
        let w = search_pattern_wide(r"C:\a");
        let s = String::from_utf16(&w[..w.len() - 1]).unwrap();
        assert_eq!(s, r"\\?\C:\a\*");
    }
}
