//! Drop and paste rules (pure): which DROPEFFECT a drop gets, and the file
//! operations a drop or paste turns into. Approved rules: same drive moves,
//! another drive copies, Ctrl copies, Shift moves, Alt (or Ctrl+Shift) makes
//! shortcuts. Dropping items onto the folder they are already in, or a folder
//! into itself, does nothing. See `docs/shell-integration.md`.
//!
//! (U3 adds `src/dnd.rs` with the same rules for in-app drags; when both
//! exist, this file should fold into that one.)

use std::path::{Path, PathBuf};
use tsubame_win::dnd::formats::{self, EFFECT_COPY, EFFECT_LINK, EFFECT_MOVE, EFFECT_NONE, MK_ALT, MK_CONTROL, MK_SHIFT};
use tsubame_win::fileops::Operation;

/// The effect the user asked for with the keys, before `allowed`.
pub fn wanted(sources: &[PathBuf], target: &Path, keys: u32) -> u32 {
    let (ctrl, shift, alt) = (keys & MK_CONTROL != 0, keys & MK_SHIFT != 0, keys & MK_ALT != 0);
    if alt || (ctrl && shift) {
        EFFECT_LINK
    } else if ctrl {
        EFFECT_COPY
    } else if shift || sources.first().is_some_and(|s| formats::same_volume(s, target)) {
        EFFECT_MOVE
    } else {
        EFFECT_COPY
    }
}

/// The effect of dropping `sources` on folder `target` with `keys` (MK_*
/// bits), limited to what the source `allowed`. A forced effect (a modifier
/// held) that the source does not allow gives NONE; the default falls back
/// to copy, then move.
pub fn choose(sources: &[PathBuf], target: &Path, keys: u32, allowed: u32) -> u32 {
    if !droppable(sources, target) {
        return EFFECT_NONE;
    }
    let w = wanted(sources, target, keys);
    let forced = keys & (MK_CONTROL | MK_SHIFT | MK_ALT) != 0;
    if w & allowed != 0 {
        w
    } else if forced {
        EFFECT_NONE
    } else {
        [EFFECT_COPY, EFFECT_MOVE].into_iter().find(|e| e & allowed != 0).unwrap_or(EFFECT_NONE)
    }
}

/// Can `sources` go into `target` at all? Not when every item already sits
/// there (self-drop), when the target is one of them or inside one, or when
/// a source has no name (a drive root).
pub fn droppable(sources: &[PathBuf], target: &Path) -> bool {
    if sources.is_empty() || target.as_os_str().is_empty() {
        return false;
    }
    if sources.iter().any(|s| s.file_name().is_none() || formats::is_within(target, s)) {
        return false;
    }
    !sources.iter().all(|s| s.parent().is_some_and(|p| formats::same_path(p, target)))
}

/// File operations for a drop or a paste of `sources` into folder `target`.
/// Items already in `target` are skipped for a move (nothing to do) and kept
/// for a copy (the queue's conflict answer "keep both" makes "name (2)").
/// LINK and NONE give no operations (shortcuts are made by a shell worker).
pub fn ops(sources: &[PathBuf], target: &Path, effect: u32) -> Vec<Operation> {
    let mut v = Vec::with_capacity(sources.len());
    for s in sources {
        let Some(name) = s.file_name() else { continue };
        if formats::is_within(target, s) {
            continue;
        }
        let dest = target.join(name);
        let here = s.parent().is_some_and(|p| formats::same_path(p, target));
        match effect {
            EFFECT_MOVE if !here => v.push(Operation::Move { source: s.clone(), target: dest }),
            EFFECT_COPY => v.push(Operation::Copy { source: s.clone(), target: dest }),
            _ => {}
        }
    }
    v
}

/// A name for a new item in a folder whose (lowercased) names are `taken`:
/// `base`, then `base (2)`, `base (3)`, ... with `ext` kept at the end.
pub fn free_name(base: &str, ext: &str, taken: &dyn Fn(&str) -> bool) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }
    const ALL: u32 = EFFECT_COPY | EFFECT_MOVE | EFFECT_LINK;

    #[test]
    fn modifier_rules() {
        let src = [p(r"C:\a\x.txt")];
        assert_eq!(choose(&src, &p(r"C:\b"), 0, ALL), EFFECT_MOVE, "same drive moves");
        assert_eq!(choose(&src, &p(r"D:\b"), 0, ALL), EFFECT_COPY, "other drive copies");
        assert_eq!(choose(&src, &p(r"C:\b"), MK_CONTROL, ALL), EFFECT_COPY);
        assert_eq!(choose(&src, &p(r"D:\b"), MK_SHIFT, ALL), EFFECT_MOVE);
        assert_eq!(choose(&src, &p(r"D:\b"), MK_ALT, ALL), EFFECT_LINK);
        assert_eq!(choose(&src, &p(r"C:\b"), MK_CONTROL | MK_SHIFT, ALL), EFFECT_LINK);
        assert_eq!(choose(&[p(r"\\srv\share\x")], &p(r"\\SRV\Share\y"), 0, ALL), EFFECT_MOVE);
        assert_eq!(choose(&[p(r"\\srv\share\x")], &p(r"\\srv\other\y"), 0, ALL), EFFECT_COPY);
    }

    #[test]
    fn allowed_mask() {
        let src = [p(r"C:\a\x.txt")];
        // the default falls back
        assert_eq!(choose(&src, &p(r"C:\b"), 0, EFFECT_COPY), EFFECT_COPY);
        assert_eq!(choose(&src, &p(r"D:\b"), 0, EFFECT_MOVE), EFFECT_MOVE);
        assert_eq!(choose(&src, &p(r"D:\b"), 0, EFFECT_LINK), EFFECT_NONE);
        // a forced effect does not
        assert_eq!(choose(&src, &p(r"C:\b"), MK_SHIFT, EFFECT_COPY), EFFECT_NONE);
        assert_eq!(choose(&src, &p(r"C:\b"), MK_ALT, EFFECT_COPY | EFFECT_MOVE), EFFECT_NONE);
    }

    #[test]
    fn self_drops_do_nothing() {
        let src = [p(r"C:\a\x.txt"), p(r"C:\a\y")];
        assert_eq!(choose(&src, &p(r"C:\a"), 0, ALL), EFFECT_NONE, "same folder");
        assert_eq!(choose(&src, &p(r"c:\A\"), MK_CONTROL, ALL), EFFECT_NONE, "same folder, other case");
        assert_eq!(choose(&src, &p(r"C:\a\y"), 0, ALL), EFFECT_NONE, "into itself");
        assert_eq!(choose(&src, &p(r"C:\a\y\deep"), 0, ALL), EFFECT_NONE, "into its child");
        assert_eq!(choose(&[p("C:\\")], &p(r"D:\x"), 0, ALL), EFFECT_NONE, "drive root");
        assert_eq!(choose(&[], &p(r"D:\x"), 0, ALL), EFFECT_NONE);
        // one item elsewhere is enough to drop
        assert_eq!(choose(&[p(r"C:\a\x"), p(r"C:\b\z")], &p(r"C:\a"), 0, ALL), EFFECT_MOVE);
        assert_eq!(choose(&src, &p(r"C:\ab"), 0, ALL), EFFECT_MOVE, "prefix is not inside");
    }

    #[test]
    fn operations() {
        let src = [p(r"C:\a\x.txt"), p(r"C:\b\y")];
        assert_eq!(
            ops(&src, &p(r"D:\t"), EFFECT_COPY),
            vec![Operation::Copy { source: p(r"C:\a\x.txt"), target: p(r"D:\t\x.txt") }, Operation::Copy { source: p(r"C:\b\y"), target: p(r"D:\t\y") }]
        );
        // a move skips what is already there
        assert_eq!(ops(&src, &p(r"C:\a"), EFFECT_MOVE), vec![Operation::Move { source: p(r"C:\b\y"), target: p(r"C:\a\y") }]);
        // a copy into the same folder is kept (keep-both makes the copy)
        assert_eq!(ops(&src[..1], &p(r"C:\a"), EFFECT_COPY).len(), 1);
        // never into itself
        assert!(ops(&[p(r"C:\b\y")], &p(r"C:\b\y\z"), EFFECT_COPY).is_empty());
        assert!(ops(&src, &p(r"D:\t"), EFFECT_LINK).is_empty());
        assert!(ops(&src, &p(r"D:\t"), EFFECT_NONE).is_empty());
    }

    #[test]
    fn names() {
        let taken = |n: &str| n == "new folder" || n == "new folder (2)";
        assert_eq!(free_name("New folder", "", &taken), "New folder (3)");
        assert_eq!(free_name("New Text Document", ".txt", &|_| false), "New Text Document.txt");
    }
}
