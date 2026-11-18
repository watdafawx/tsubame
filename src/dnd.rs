//! Drag and drop rules, for every drop: from our own rows and from other
//! programs alike (OLE, `src/shellops.rs` → `src/ole_glue.rs` call these).
//! Pure functions, no UI and no disk, so the rules are unit tested.
//!
//! Rules (the mock, "Plain Windows rules"): the same drive moves, another drive
//! copies, Ctrl copies, Shift moves, Alt or Ctrl+Shift makes a shortcut.
//! Dropping a folder between two pinned rows pins it there instead
//! (`pin_slot`). A drop is refused when the target is one of the items or
//! inside one, when an item has no name (a drive root), or when every item
//! already sits in the target.

use crate::nav;
use tsubame_ui::Mods;

/// What is being dragged: full paths, all in one folder (a selection).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DragPayload {
    pub paths: Vec<String>,
}

impl DragPayload {
    pub fn new(paths: Vec<String>) -> DragPayload {
        DragPayload { paths }
    }
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
    /// The name of the first dragged item (the drag image shows it).
    pub fn first_name(&self) -> &str {
        self.paths.first().map(|p| name_of(p)).unwrap_or("")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAction {
    Move,
    Copy,
    /// Create a shortcut (Alt). Needs the shell; the app shows it but cannot do it yet.
    Link,
}

impl DropAction {
    pub fn verb(self) -> &'static str {
        match self {
            DropAction::Move => "Move to",
            DropAction::Copy => "Copy to",
            DropAction::Link => "Shortcut in",
        }
    }
}

/// The last path component.
pub fn name_of(path: &str) -> &str {
    path.trim_end_matches('\\').rsplit('\\').next().unwrap_or(path)
}

/// Identity of the volume a path is on: the drive letter, or `\\server\share`.
/// `None` for something that is not an absolute path.
pub fn volume_key(path: &str) -> Option<String> {
    let c = nav::crumbs(path);
    let root = c.first()?;
    let r = root.path.strip_prefix("\\\\?\\UNC\\").map(|s| format!("\\\\{s}")).or_else(|| root.path.strip_prefix("\\\\?\\").map(str::to_string)).unwrap_or_else(|| root.path.clone());
    Some(r.trim_end_matches('\\').to_ascii_lowercase())
}

pub fn same_volume(a: &str, b: &str) -> bool {
    match (volume_key(a), volume_key(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Alt (or Ctrl+Shift, like Explorer) makes a shortcut, Shift moves, Ctrl
/// copies; with no key the same drive moves and another drive copies.
pub fn resolve(source: &str, dest_folder: &str, mods: Mods) -> DropAction {
    if mods.alt || (mods.ctrl && mods.shift) {
        DropAction::Link
    } else if mods.shift {
        DropAction::Move
    } else if mods.ctrl {
        DropAction::Copy
    } else if same_volume(source, dest_folder) {
        DropAction::Move
    } else {
        DropAction::Copy
    }
}

/// Why a drop on `dest_folder` is refused (`None` = it is allowed).
pub fn refusal(payload: &DragPayload, dest_folder: &str) -> Option<&'static str> {
    if payload.is_empty() {
        return Some("Nothing to drop");
    }
    let dest = dest_folder.trim_end_matches('\\').to_ascii_lowercase();
    if dest.is_empty() {
        return Some("Nothing to drop on");
    }
    let mut all_here = true;
    for p in &payload.paths {
        let src = p.trim_end_matches('\\').to_ascii_lowercase();
        let Some(parent) = nav::parent(p) else { return Some("Can't move a drive") };
        if src == dest || dest.starts_with(&format!("{src}\\")) {
            return Some("Can't drop a folder into itself");
        }
        all_here &= parent.trim_end_matches('\\').eq_ignore_ascii_case(&dest);
    }
    all_here.then_some("Already in this folder")
}

/// One step of a drop, ready for the file-op queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Copy { source: String, target: String },
    Move { source: String, target: String },
    Link { source: String, target: String },
}

/// The steps that drop `payload` into `dest_folder` with `action`. A refused
/// drop gives no steps.
pub fn plan(payload: &DragPayload, dest_folder: &str, action: DropAction) -> Vec<Step> {
    if refusal(payload, dest_folder).is_some() {
        return Vec::new();
    }
    payload
        .paths
        .iter()
        .map(|p| {
            let target = nav::join(dest_folder, name_of(p));
            let source = p.clone();
            match action {
                DropAction::Move => Step::Move { source, target },
                DropAction::Copy => Step::Copy { source, target },
                DropAction::Link => Step::Link { source, target: format!("{target}.lnk") },
            }
        })
        .collect()
}

/// Where a drag over the pinned list lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinSlot {
    /// between rows: insert at this index (0 = before the first)
    Between(usize),
    /// on a row: drop into that folder
    OnRow(usize),
}

/// `y` is the pointer's distance from the top of the first pinned row, `row_h`
/// the row height and `n` the number of rows. The top and bottom quarter of a
/// row mean "between"; the middle half means "on the row". A bookmark being
/// reordered (`reorder`) has no "on the row": the nearest gap wins.
pub fn pin_slot(y: f32, row_h: f32, n: usize, reorder: bool) -> PinSlot {
    if n == 0 || y < 0.0 {
        return PinSlot::Between(0);
    }
    let i = (y / row_h).floor() as usize;
    if i >= n {
        return PinSlot::Between(n);
    }
    let f = y / row_h - i as f32;
    if reorder {
        return PinSlot::Between(if f < 0.5 { i } else { i + 1 });
    }
    if f < 0.25 {
        PinSlot::Between(i)
    } else if f > 0.75 {
        PinSlot::Between(i + 1)
    } else {
        PinSlot::OnRow(i)
    }
}

/// Moving a bookmark from `from` to the gap `slot` (a [`PinSlot::Between`]
/// index in the list before the move): the index it has afterwards.
pub fn reorder_target(from: usize, slot: usize) -> usize {
    if slot > from { slot - 1 } else { slot }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(p: &[&str]) -> DragPayload {
        DragPayload::new(p.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn same_drive_moves_other_drive_copies() {
        assert_eq!(resolve("C:\\a\\f.txt", "C:\\b", Mods::NONE), DropAction::Move);
        assert_eq!(resolve("c:\\a\\f.txt", "C:\\b", Mods::NONE), DropAction::Move);
        assert_eq!(resolve("C:\\a\\f.txt", "X:\\b", Mods::NONE), DropAction::Copy);
        assert_eq!(resolve("C:\\a\\f.txt", "\\\\srv\\share\\b", Mods::NONE), DropAction::Copy);
        assert_eq!(resolve("\\\\srv\\share\\a", "\\\\SRV\\Share\\b", Mods::NONE), DropAction::Move);
    }

    #[test]
    fn modifiers_override_the_default() {
        let ctrl = Mods { ctrl: true, ..Mods::NONE };
        let shift = Mods { shift: true, ..Mods::NONE };
        let alt = Mods { alt: true, ..Mods::NONE };
        assert_eq!(resolve("C:\\a\\f", "C:\\b", ctrl), DropAction::Copy);
        assert_eq!(resolve("C:\\a\\f", "X:\\b", shift), DropAction::Move);
        assert_eq!(resolve("C:\\a\\f", "C:\\b", alt), DropAction::Link);
        // Alt wins over Ctrl and Shift (Windows: Ctrl+Shift is a shortcut too)
        assert_eq!(resolve("C:\\a\\f", "C:\\b", Mods { ctrl: true, shift: true, alt: true }), DropAction::Link);
        assert_eq!(resolve("C:\\a\\f", "X:\\b", Mods { ctrl: true, shift: true, alt: false }), DropAction::Link);
    }

    #[test]
    fn volume_keys() {
        assert_eq!(volume_key("C:\\x").as_deref(), Some("c:"));
        assert_eq!(volume_key("\\\\?\\C:\\x").as_deref(), Some("c:"));
        assert_eq!(volume_key("\\\\?\\UNC\\srv\\share\\x").as_deref(), Some("\\\\srv\\share"));
        assert_eq!(volume_key("relative"), None);
        assert!(!same_volume("relative", "relative"));
    }

    #[test]
    fn refuses_into_itself_and_into_the_same_folder() {
        let p = payload(&["C:\\a\\Docs"]);
        assert!(refusal(&p, "C:\\a\\Docs").is_some());
        assert!(refusal(&p, "C:\\a\\Docs\\Sub").is_some());
        assert!(refusal(&p, "C:\\a").is_some(), "its own folder");
        assert!(refusal(&p, "c:\\A\\").is_some(), "case and trailing slash");
        assert!(refusal(&p, "C:\\a\\Docs2").is_none(), "a sibling with the same prefix is fine");
        assert!(refusal(&p, "C:\\b").is_none());
        assert!(refusal(&DragPayload::default(), "C:\\b").is_some());
        // some items elsewhere: the drop goes on (the ones already there are skipped)
        assert!(refusal(&payload(&["C:\\a\\x", "C:\\b\\y"]), "C:\\a").is_none());
        assert!(refusal(&payload(&["C:\\"]), "D:\\b").is_some(), "a drive root");
    }

    #[test]
    fn plan_builds_targets() {
        let p = payload(&["C:\\a\\x.txt", "C:\\a\\Docs"]);
        assert_eq!(
            plan(&p, "C:\\b", DropAction::Move),
            vec![Step::Move { source: "C:\\a\\x.txt".into(), target: "C:\\b\\x.txt".into() }, Step::Move { source: "C:\\a\\Docs".into(), target: "C:\\b\\Docs".into() }]
        );
        assert_eq!(plan(&p, "X:\\", DropAction::Copy)[0], Step::Copy { source: "C:\\a\\x.txt".into(), target: "X:\\x.txt".into() });
        assert_eq!(plan(&p, "C:\\b", DropAction::Link)[0], Step::Link { source: "C:\\a\\x.txt".into(), target: "C:\\b\\x.txt.lnk".into() });
        assert!(plan(&p, "C:\\a", DropAction::Move).is_empty(), "refused drop");
    }

    #[test]
    fn pin_slots_between_and_on() {
        let h = 24.0;
        assert_eq!(pin_slot(-3.0, h, 4, false), PinSlot::Between(0));
        assert_eq!(pin_slot(2.0, h, 4, false), PinSlot::Between(0));
        assert_eq!(pin_slot(12.0, h, 4, false), PinSlot::OnRow(0));
        assert_eq!(pin_slot(22.0, h, 4, false), PinSlot::Between(1));
        assert_eq!(pin_slot(h + 12.0, h, 4, false), PinSlot::OnRow(1));
        assert_eq!(pin_slot(4.0 * h - 1.0, h, 4, false), PinSlot::Between(4));
        assert_eq!(pin_slot(400.0, h, 4, false), PinSlot::Between(4));
        assert_eq!(pin_slot(5.0, h, 0, false), PinSlot::Between(0));
        // reordering never drops "on" a row
        assert_eq!(pin_slot(12.0, h, 4, true), PinSlot::Between(1));
        assert_eq!(pin_slot(11.0, h, 4, true), PinSlot::Between(0));
    }

    #[test]
    fn reorder_target_accounts_for_the_removed_row() {
        assert_eq!(reorder_target(1, 0), 0);
        assert_eq!(reorder_target(1, 1), 1, "dropping in its own gap changes nothing");
        assert_eq!(reorder_target(1, 2), 1, "the gap after itself is its own place too");
        assert_eq!(reorder_target(1, 4), 3);
        assert_eq!(reorder_target(0, 3), 2);
    }

    #[test]
    fn names() {
        assert_eq!(name_of("C:\\a\\b.txt"), "b.txt");
        assert_eq!(name_of("C:\\a\\Docs\\"), "Docs");
        assert_eq!(payload(&["C:\\x\\y.zip"]).first_name(), "y.zip");
        assert_eq!(DropAction::Move.verb(), "Move to");
    }
}
