//! Untrusted text and plugin output (PH-13 b, PH-16, PH-17).
//!
//! [`sanitize`] is the one text filter. The `clean_*` functions are the
//! validation at the boundary: the host runs them before it encodes an answer
//! (so an honest host never sends what the client would refuse) and the
//! adapters run them again on what they decoded (so a hostile host gets no
//! further). They are idempotent: running them twice changes nothing.

use crate::caps;
use crate::manifest::Kind;
use tsubame_ext_api::{Cell, Hit, Node, VfsEntry};

/// Most nodes in one preview document.
pub const MAX_NODES: usize = 4_096;
/// Most bytes of a `Text` or `Mono` node.
pub const MAX_NODE_TEXT: usize = 8 << 10;
/// Most bytes of a heading, a label, a pair half or a button command.
pub const MAX_LABEL: usize = 256;
/// Most bytes of a text cell (and of a byte-count cell's worth of nothing else).
pub const MAX_CELL_TEXT: usize = 1 << 10;
/// Most bytes of a command message or an error text.
pub const MAX_MESSAGE: usize = 1 << 10;
/// Most bytes of an entry name (a Windows path component is 255 UTF-16 units).
pub const MAX_NAME: usize = 255;

/// Unicode format characters (Cf): bidi controls, zero-width characters,
/// joiners, tag characters. Hand-written so no table crate is needed.
fn is_format(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF
        | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// Characters that never belong in single-line text: control (Cc), format
/// (Cf), line and paragraph separators (Zl, Zp).
fn is_unsafe(c: char) -> bool {
    c.is_control() || is_format(c) || c == '\u{2028}' || c == '\u{2029}'
}

/// True if `c` can make text lie about what it says: control (Cc), format (Cf:
/// bidi overrides, zero-width), line and paragraph separators (Zl, Zp) and
/// private-use characters. A manifest label with one of these is refused (PH-11).
pub fn is_spoofing(c: char) -> bool {
    is_unsafe(c) || matches!(c as u32, 0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD)
}

fn cut(s: &mut String, max: usize) {
    if s.len() > max {
        let mut n = max;
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s.truncate(n);
    }
}

/// Drops control, format and line-separator characters, then cuts to at most
/// `max` bytes on a character boundary.
pub fn sanitize(text: &str, max: usize) -> String {
    let mut out: String = text.chars().filter(|&c| !is_unsafe(c)).collect();
    cut(&mut out, max);
    out
}

/// Like [`sanitize`] for multi-line text: `\n` and `\t` stay.
pub fn sanitize_lines(text: &str, max: usize) -> String {
    let mut out: String = text.chars().filter(|&c| c == '\n' || c == '\t' || !is_unsafe(c)).collect();
    cut(&mut out, max);
    out
}

/// True when `name` is `<plugin id>.<command>` for a command the plugin declared.
pub fn own_command(plugin_id: &str, entrypoints: &[(Kind, String)], name: &str) -> bool {
    let Some(rest) = name.strip_prefix(plugin_id).and_then(|r| r.strip_prefix('.')) else { return false };
    entrypoints.iter().any(|(k, e)| *k == Kind::Command && e == rest)
}

/// Caps and cleans a document. `command_ok` says whether a button may run a
/// command; a button for any other command is dropped. `max_bytes` bounds the
/// text and pixels together; nodes past it are dropped.
pub fn clean_doc(nodes: &mut Vec<Node>, max_bytes: u64, command_ok: &dyn Fn(&str) -> bool) {
    let mut used: u64 = 0;
    let mut kept = 0usize;
    nodes.retain_mut(|n| {
        if kept >= MAX_NODES {
            return false;
        }
        let cost = match n {
            Node::Heading(s) => {
                *s = sanitize(s, MAX_LABEL);
                s.len()
            }
            Node::Text { text, .. } => {
                *text = sanitize_lines(text, MAX_NODE_TEXT);
                text.len()
            }
            Node::Mono(s) => {
                *s = sanitize_lines(s, MAX_NODE_TEXT);
                s.len()
            }
            Node::Pair(a, b) => {
                *a = sanitize(a, MAX_LABEL);
                *b = sanitize(b, MAX_LABEL * 4);
                a.len() + b.len()
            }
            Node::Image { bgra, .. } => bgra.len(),
            Node::Button { label, command } => {
                if command.len() > MAX_LABEL || !command_ok(command) {
                    return false;
                }
                *label = sanitize(label, MAX_LABEL);
                label.len() + command.len()
            }
            Node::Indent | Node::Outdent => 0,
        };
        used = used.saturating_add(cost as u64);
        if used > max_bytes {
            return false;
        }
        kept += 1;
        true
    });
}

/// Caps text cells; the other cell kinds are fixed size.
pub fn clean_cells(cells: &mut [Cell]) {
    for c in cells {
        if let Cell::Text(s) = c {
            *s = sanitize(s, MAX_CELL_TEXT);
        }
    }
}

/// An entry name is one plain path component: no separator, no `.` or `..`,
/// no stream or device name, no control or bidi characters.
pub fn entry_name_ok(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME || name.contains(['\\', '/']) || name.chars().any(is_unsafe) {
        return false;
    }
    // One component after a drive: the path rules of the sandbox decide the rest.
    caps::normalize(&format!("C:\\{name}")).is_ok_and(|p| p.len() == name.len() + 3)
}

/// Drops entries whose name is not [`entry_name_ok`].
pub fn clean_entries(entries: &mut Vec<VfsEntry>) {
    entries.retain(|e| entry_name_ok(&e.name));
}

/// Keeps hits whose path is a normal absolute path under `root` (any path when
/// `root` is empty, but never a UNC path then). A UNC hit needs a UNC root.
/// Kept paths are replaced by their normalized spelling.
pub fn clean_hits(hits: &mut Vec<Hit>, root: &str) {
    let root_key = if root.is_empty() {
        None
    } else {
        match caps::normalize(root) {
            Ok(r) => Some(caps::key(&r)),
            Err(_) => {
                hits.clear();
                return;
            }
        }
    };
    let root_unc = root_key.as_deref().is_some_and(|k| k.starts_with("\\\\"));
    hits.retain_mut(|h| {
        if h.path.chars().any(is_unsafe) {
            return false;
        }
        let Ok(p) = caps::normalize(&h.path) else { return false };
        if p.starts_with("\\\\") && !root_unc {
            return false;
        }
        if root_key.as_deref().is_some_and(|r| !caps::is_under(&caps::key(&p), r)) {
            return false;
        }
        h.path = p;
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_control_bidi_and_separators() {
        let s = sanitize("a\u{1b}[2Jb\u{202e}c\u{200b}d\u{2028}e\u{2029}f\ng", 100);
        assert_eq!(s, "a[2Jbcdefg");
    }

    #[test]
    fn cuts_on_a_char_boundary() {
        assert_eq!(sanitize("aéé", 2), "a");
        assert_eq!(sanitize("aéé", 3), "aé");
        assert_eq!(sanitize("", 0), "");
    }

    #[test]
    fn lines_keep_newline_and_tab_only() {
        assert_eq!(sanitize_lines("a\n\tb\r\u{7}\u{202e}c", 100), "a\n\tbc");
    }

    #[test]
    fn names() {
        for ok in ["a.txt", "x y", "ünï.rs", ".hidden"] {
            assert!(entry_name_ok(ok), "{ok}");
        }
        for bad in ["", "..", ".", "..\\..\\Startup\\x.cmd", "a/b", "a\\b", "a:b", "NUL", "nul.txt", "COM1", "a.", "a ", "a\u{202e}b", "a\u{1}b", "a\nb", &"x".repeat(300)] {
            assert!(!entry_name_ok(bad), "{bad:?}");
        }
    }

    fn hit(p: &str) -> Hit {
        Hit { path: p.into(), dir: false, size: 0, mtime: 0 }
    }

    fn paths(root: &str, input: &[&str]) -> Vec<String> {
        let mut v: Vec<Hit> = input.iter().map(|p| hit(p)).collect();
        clean_hits(&mut v, root);
        v.into_iter().map(|h| h.path).collect()
    }

    #[test]
    fn hits_under_a_root() {
        assert_eq!(paths("C:\\data", &["c:/data/a", "C:\\data", "C:\\other\\a", "C:\\data2\\a", "C:\\data\\..\\x", "x"]), ["C:\\data\\a", "C:\\data"]);
        assert_eq!(paths("", &["D:\\a", "relative"]), ["D:\\a"]);
    }

    #[test]
    fn a_unc_hit_needs_a_unc_root() {
        assert!(paths("", &["\\\\attacker.example\\share\\x"]).is_empty());
        assert!(paths("C:\\data", &["\\\\attacker.example\\share\\x"]).is_empty());
        assert_eq!(paths("\\\\srv\\share", &["\\\\srv\\share\\a", "\\\\evil\\share\\a"]), ["\\\\srv\\share\\a"]);
    }

    #[test]
    fn a_bad_root_drops_everything() {
        assert!(paths("..\\x", &["C:\\a"]).is_empty());
    }

    #[test]
    fn buttons_only_run_own_commands() {
        let eps = vec![(Kind::Command, "open".to_string()), (Kind::Column, "size".to_string())];
        let ok = |c: &str| own_command("acme", &eps, c);
        assert!(ok("acme.open"));
        assert!(!ok("acme.size"), "a column is not a command");
        assert!(!ok("other.open"));
        assert!(!ok("acme.delete"));
        assert!(!ok("acmeX.open"));
        let mut doc = vec![
            Node::Button { label: "Open".into(), command: "acme.open".into() },
            Node::Button { label: "Open".into(), command: "tsubame.delete".into() },
            Node::Text { text: "t\u{202e}".into(), dim: false },
        ];
        clean_doc(&mut doc, 1 << 20, &ok);
        assert_eq!(doc.len(), 2);
        assert!(matches!(&doc[1], Node::Text { text, .. } if text == "t"));
    }

    #[test]
    fn a_document_is_capped() {
        let ok = |_: &str| true;
        let mut doc: Vec<Node> = (0..MAX_NODES + 10).map(|_| Node::Indent).collect();
        clean_doc(&mut doc, 1 << 20, &ok);
        assert_eq!(doc.len(), MAX_NODES);
        let mut doc = vec![Node::Text { text: "x".repeat(MAX_NODE_TEXT * 2), dim: false }, Node::Mono("y".repeat(10)), Node::Heading("z".repeat(10))];
        clean_doc(&mut doc, (MAX_NODE_TEXT + 12) as u64, &ok);
        assert_eq!(doc.len(), 2, "the byte budget stops the document");
        assert!(matches!(&doc[0], Node::Text { text, .. } if text.len() == MAX_NODE_TEXT));
    }

    #[test]
    fn cells_are_capped() {
        let mut c = [Cell::Text("a".repeat(5_000)), Cell::Int(1)];
        clean_cells(&mut c);
        assert!(matches!(&c[0], Cell::Text(s) if s.len() == MAX_CELL_TEXT));
    }

    #[test]
    fn cleaning_twice_changes_nothing() {
        let mut a = vec![hit("C:\\a\\..\\b"), hit("c:/x/y")];
        clean_hits(&mut a, "");
        let once = a.clone();
        clean_hits(&mut a, "");
        assert_eq!(a, once);
    }
}
