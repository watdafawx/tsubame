//! Navigation model, no UI and no disk: per-tab history and path parsing
//! (breadcrumbs, parent, normalising what the user types in the path bar).
//! Windows paths only: drive roots, UNC shares and `\\?\` long paths.

/// Back/forward list of one tab. `push` after a `back` drops the forward part.
#[derive(Clone, Debug)]
pub struct History {
    entries: Vec<String>,
    pos: usize,
}

const HISTORY_CAP: usize = 200;

impl History {
    pub fn new(start: &str) -> History {
        History { entries: vec![start.to_string()], pos: 0 }
    }
    pub fn current(&self) -> &str {
        &self.entries[self.pos]
    }
    /// Visits `path`. Visiting the folder already shown changes nothing.
    pub fn push(&mut self, path: &str) {
        if path.eq_ignore_ascii_case(self.current()) {
            self.entries[self.pos] = path.to_string();
            return;
        }
        self.entries.truncate(self.pos + 1);
        self.entries.push(path.to_string());
        if self.entries.len() > HISTORY_CAP {
            self.entries.remove(0);
        }
        self.pos = self.entries.len() - 1;
    }
    /// Takes back the last `push` (the folder turned out not to open).
    pub fn undo_push(&mut self) {
        if self.pos > 0 {
            self.entries.truncate(self.pos);
            self.pos -= 1;
        }
    }
    pub fn can_back(&self) -> bool {
        self.pos > 0
    }
    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.entries.len()
    }
    pub fn back(&mut self) -> Option<&str> {
        if self.can_back() {
            self.pos -= 1;
            Some(&self.entries[self.pos])
        } else {
            None
        }
    }
    pub fn forward(&mut self) -> Option<&str> {
        if self.can_forward() {
            self.pos += 1;
            Some(&self.entries[self.pos])
        } else {
            None
        }
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// One breadcrumb: what it shows and where a click goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub path: String,
}

/// Splits a path into (crumb label of the root, root path, remainder).
/// `None` when `p` has no recognisable root.
fn split_root(p: &str) -> Option<(String, String, &str)> {
    let (verbatim, rest) = match p.strip_prefix("\\\\?\\") {
        Some(r) => ("\\\\?\\", r),
        None => ("", p),
    };
    let unc = if verbatim.is_empty() { rest.strip_prefix("\\\\") } else { rest.strip_prefix("UNC\\") };
    if let Some(unc) = unc {
        // server\share[\...]
        let mut it = unc.splitn(3, '\\');
        let server = it.next().filter(|s| !s.is_empty())?;
        let share = it.next().filter(|s| !s.is_empty())?;
        let tail = it.next().unwrap_or("");
        let label = format!("\\\\{server}\\{share}");
        let root = if verbatim.is_empty() { format!("\\\\{server}\\{share}\\") } else { format!("\\\\?\\UNC\\{server}\\{share}\\") };
        return Some((label, root, tail));
    }
    let b = rest.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/') {
        let letter = (b[0] as char).to_ascii_uppercase();
        let label = format!("{letter}:");
        let root = format!("{verbatim}{letter}:\\");
        let tail = if b.len() > 3 { &rest[3..] } else { "" };
        return Some((label, root, tail));
    }
    None
}

/// Breadcrumbs for `path`. Empty for something that is not a path.
pub fn crumbs(path: &str) -> Vec<Crumb> {
    let Some((label, root, tail)) = split_root(path) else { return Vec::new() };
    let mut out = vec![Crumb { label, path: root.clone() }];
    let mut cur = root;
    for part in tail.split(['\\', '/']).filter(|s| !s.is_empty()) {
        if !cur.ends_with('\\') {
            cur.push('\\');
        }
        cur.push_str(part);
        out.push(Crumb { label: part.to_string(), path: cur.clone() });
    }
    out
}

/// The folder above `path`; `None` at a drive or share root.
pub fn parent(path: &str) -> Option<String> {
    let c = crumbs(path);
    if c.len() < 2 { None } else { Some(c[c.len() - 2].path.clone()) }
}

/// Joins a folder and an entry name.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') { format!("{dir}{name}") } else { format!("{dir}\\{name}") }
}

/// Cleans text typed into the path bar: trims, drops surrounding quotes,
/// forward slashes to backslashes, `C:` to `C:\`, no trailing separator
/// except on a root. `None` when it is not an absolute path.
pub fn normalize(input: &str) -> Option<String> {
    let t = input.trim().trim_matches('"').trim();
    if t.is_empty() {
        return None;
    }
    let t = t.replace('/', "\\");
    crumbs(&t).last().map(|c| c.path.clone())
}

/// Splits the path-bar text at its last separator: (folder to list, the
/// fragment being typed). `C:\Us` gives (`C:\`, `Us`); `C:\Users\` gives
/// (`C:\Users\`, ``).
#[cfg(test)]
pub fn split_fragment(text: &str) -> (&str, &str) {
    match text.rfind(['\\', '/']) {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => ("", text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_back_forward_and_truncate() {
        let mut h = History::new("C:\\");
        assert!(!h.can_back() && !h.can_forward());
        h.push("C:\\Users");
        h.push("C:\\Users\\me");
        assert_eq!(h.current(), "C:\\Users\\me");
        assert_eq!(h.back(), Some("C:\\Users"));
        assert_eq!(h.back(), Some("C:\\"));
        assert_eq!(h.back(), None);
        assert!(h.can_forward());
        assert_eq!(h.forward(), Some("C:\\Users"));
        // a new visit drops the forward part
        h.push("D:\\");
        assert!(!h.can_forward());
        assert_eq!(h.len(), 3);
        assert_eq!(h.back(), Some("C:\\Users"));
        // same folder again (any case) adds nothing
        let n = h.len();
        h.push("c:\\users");
        assert_eq!(h.len(), n);
        assert_eq!(h.current(), "c:\\users");
    }

    #[test]
    fn history_is_capped() {
        let mut h = History::new("C:\\0");
        for i in 1..500 {
            h.push(&format!("C:\\{i}"));
        }
        assert_eq!(h.len(), HISTORY_CAP);
        assert_eq!(h.current(), "C:\\499");
        let mut steps = 0;
        while h.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, HISTORY_CAP - 1);
    }

    fn labels(p: &str) -> Vec<String> {
        crumbs(p).into_iter().map(|c| c.label).collect()
    }

    #[test]
    fn crumbs_drive_paths() {
        let c = crumbs("C:\\Users\\me\\Docs");
        assert_eq!(c.len(), 4);
        assert_eq!((c[0].label.as_str(), c[0].path.as_str()), ("C:", "C:\\"));
        assert_eq!((c[1].label.as_str(), c[1].path.as_str()), ("Users", "C:\\Users"));
        assert_eq!(c[3].path, "C:\\Users\\me\\Docs");
        assert_eq!(labels("c:\\"), ["C:"]);
        assert_eq!(labels("C:"), ["C:"]);
        assert_eq!(labels("C:\\a\\"), ["C:", "a"]);
        assert_eq!(labels("C:/a//b"), ["C:", "a", "b"]);
        assert!(crumbs("relative\\path").is_empty());
        assert!(crumbs("").is_empty());
        assert!(crumbs("CC:\\x").is_empty());
    }

    #[test]
    fn crumbs_unc_and_long_paths() {
        let c = crumbs("\\\\srv\\share\\dir\\f");
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].label.as_str(), c[0].path.as_str()), ("\\\\srv\\share", "\\\\srv\\share\\"));
        assert_eq!(c[2].path, "\\\\srv\\share\\dir\\f");
        assert!(crumbs("\\\\srv").is_empty());
        let l = crumbs("\\\\?\\C:\\very\\long");
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].path, "\\\\?\\C:\\");
        assert_eq!(l[2].path, "\\\\?\\C:\\very\\long");
        let lu = crumbs("\\\\?\\UNC\\srv\\share\\x");
        assert_eq!(lu.len(), 2);
        assert_eq!(lu[0].label, "\\\\srv\\share");
        assert_eq!(lu[0].path, "\\\\?\\UNC\\srv\\share\\");
        assert_eq!(lu[1].path, "\\\\?\\UNC\\srv\\share\\x");
        // a 400 char deep path still splits
        let deep = format!("C:\\{}", vec!["abcdefghij"; 40].join("\\"));
        assert_eq!(crumbs(&deep).len(), 41);
    }

    #[test]
    fn parent_stops_at_roots() {
        assert_eq!(parent("C:\\Users\\me").as_deref(), Some("C:\\Users"));
        assert_eq!(parent("C:\\Users").as_deref(), Some("C:\\"));
        assert_eq!(parent("C:\\"), None);
        assert_eq!(parent("\\\\srv\\share\\a").as_deref(), Some("\\\\srv\\share\\"));
        assert_eq!(parent("\\\\srv\\share\\"), None);
        assert_eq!(parent("\\\\?\\C:\\a").as_deref(), Some("\\\\?\\C:\\"));
        assert_eq!(parent("nonsense"), None);
    }

    #[test]
    fn normalize_user_text() {
        assert_eq!(normalize("  \"C:\\Users\\me\\\"  ").as_deref(), Some("C:\\Users\\me"));
        assert_eq!(normalize("c:").as_deref(), Some("C:\\"));
        assert_eq!(normalize("D:/a/b/").as_deref(), Some("D:\\a\\b"));
        assert_eq!(normalize("C:\\\\a").as_deref(), Some("C:\\a"));
        assert_eq!(normalize("\\\\srv\\share").as_deref(), Some("\\\\srv\\share\\"));
        assert_eq!(normalize("not a path"), None);
        assert_eq!(normalize("   "), None);
    }

    #[test]
    fn join_and_fragment() {
        assert_eq!(join("C:\\", "x"), "C:\\x");
        assert_eq!(join("C:\\a", "x"), "C:\\a\\x");
        assert_eq!(split_fragment("C:\\Us"), ("C:\\", "Us"));
        assert_eq!(split_fragment("C:\\Users\\"), ("C:\\Users\\", ""));
        assert_eq!(split_fragment("abc"), ("", "abc"));
    }
}
