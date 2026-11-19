//! The row "..." menu of the context menu editor (mock `mocks/mock-ctxmenu-gaps.html`, state 1).
//!
//! One fixed list for every row, in Settings > Context menu (the "..." button of a row) and in
//! the menu-side edit menu (Shift+F10 on a row): Hide / Show, Rename, Pin to top / Unpin, Move to
//! group, Move up, Move down, then Edit entry and Delete entry on custom rows (the menu-side copy
//! adds Edit in Settings). Rows that do not apply are shown disabled, never hidden, with the
//! one-line reason as a note at the bottom of the popup. This module is the pure part: which row
//! is enabled and why, the rows to draw, and the pin order.

use tsubame_config::menu::{ItemPref, MenuEntry, Source, UserGroup};
use tsubame_ui::widgets::menu::{EDIT_DELETE, EDIT_ENTRY, EDIT_GROUP, EDIT_GROUP_BASE, EDIT_GROUP_NATIVE, EDIT_GROUP_NEW, EDIT_HIDE, EDIT_PIN, EDIT_RENAME, EDIT_SETTINGS, EDIT_UP, EDIT_DOWN, G_EYE_OFF};
use tsubame_ui::{MenuRow, RowIcon, RowKind};

/// Windows' and apps' rows keep their native order: Move up / down are off, Pin to top is the way.
pub const NOTE_LEARNED: &str = "Windows\u{2019} and apps\u{2019} items keep their native order. Pin to top is the way to move one.";
/// Tsubame's own rows move with Move up / down; Pin and groups are for Windows' and apps' items.
pub const NOTE_OWN: &str = "Tsubame\u{2019}s own rows move with Move up / down. Pin and groups are for Windows\u{2019} and apps\u{2019} items.";
/// A row inside a submenu cannot pin or change group.
pub const NOTE_NESTED: &str = "Rows inside a submenu keep their place. Pin and groups work on top-level rows.";
/// Own rows in a submenu are moved with the grip.
pub const NOTE_NESTED_OWN: &str = "Rows inside a submenu move with the grip.";
pub const NOTE_FIRST: &str = "It is already the first row of its group.";
pub const NOTE_LAST: &str = "It is already the last row of its group.";

/// What kind of row the menu is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowType {
    /// Windows', an installed app's or an `app:` row: `[[context_menu.items]]`
    Learned,
    /// Tsubame's built-in list rows (and the classic-menu row)
    Own,
    /// a custom or imported entry (a submenu too)
    Custom { submenu: bool },
}

/// The rows of the popup, as actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Hide,
    Rename,
    Pin,
    Group,
    Up,
    Down,
    EditEntry,
    DeleteEntry,
}

impl Act {
    /// The edit-menu key the kit knows the action by.
    pub fn key(self) -> u64 {
        match self {
            Act::Hide => EDIT_HIDE,
            Act::Rename => EDIT_RENAME,
            Act::Pin => EDIT_PIN,
            Act::Group => EDIT_GROUP,
            Act::Up => EDIT_UP,
            Act::Down => EDIT_DOWN,
            Act::EditEntry => EDIT_ENTRY,
            Act::DeleteEntry => EDIT_DELETE,
        }
    }
}

/// What the popup needs to know about one row.
#[derive(Clone, Debug)]
pub struct Facts {
    pub ty: RowType,
    /// the name the row shows (quoted in Hide / Show)
    pub name: String,
    pub hidden: bool,
    pub pinned: bool,
    /// the row sits inside a submenu
    pub nested: bool,
    pub can_up: bool,
    pub can_down: bool,
    /// the user's groups (id, name) and the one the row is in (empty: its native place)
    pub groups: Vec<(String, String)>,
    pub group: String,
    /// the menu-side copy: Hide has Del, Edit in Settings at the end
    pub menu_side: bool,
}

impl Facts {
    /// A learned row (`[[context_menu.items]]`).
    pub fn learned(p: &ItemPref, users: &[UserGroup], nested: bool, menu_side: bool) -> Facts {
        Facts {
            ty: RowType::Learned,
            name: if p.name.is_empty() { p.label.clone() } else { p.name.clone() },
            hidden: p.hidden,
            pinned: p.pin,
            nested,
            can_up: false,
            can_down: false,
            groups: users.iter().map(|u| (u.id.clone(), u.name.clone())).collect(),
            group: if users.iter().any(|u| u.id == p.group) { p.group.clone() } else { String::new() },
            menu_side,
        }
    }

    /// A list entry (Tsubame's own, custom or imported).
    pub fn entry(e: &MenuEntry, can_up: bool, can_down: bool, menu_side: bool) -> Facts {
        let custom = e.source.runs_program() || (e.source == Source::Custom);
        Facts {
            ty: if custom { RowType::Custom { submenu: e.is_submenu() } } else { RowType::Own },
            name: e.name.clone(),
            hidden: e.hidden,
            pinned: false,
            nested: !e.parent.is_empty(),
            can_up,
            can_down,
            groups: Vec::new(),
            group: String::new(),
            menu_side,
        }
    }
}

/// Is `act` available, and if not, why (the reason is one line, the note of the popup).
pub fn state(f: &Facts, act: Act) -> (bool, Option<&'static str>) {
    let learned = f.ty == RowType::Learned;
    match act {
        Act::Hide | Act::Rename => (true, None),
        Act::Pin | Act::Group => {
            if !learned {
                (false, Some(NOTE_OWN))
            } else if f.nested {
                (false, Some(NOTE_NESTED))
            } else {
                (true, None)
            }
        }
        Act::Up | Act::Down => {
            let up = act == Act::Up;
            if learned {
                (false, Some(if f.nested { NOTE_NESTED } else { NOTE_LEARNED }))
            } else if f.nested {
                (false, Some(NOTE_NESTED_OWN))
            } else if up && !f.can_up {
                (false, Some(NOTE_FIRST))
            } else if !up && !f.can_down {
                (false, Some(NOTE_LAST))
            } else {
                (true, None)
            }
        }
        Act::EditEntry => match f.ty {
            RowType::Custom { submenu: false } => (true, None),
            _ => (false, None),
        },
        Act::DeleteEntry => (matches!(f.ty, RowType::Custom { .. }), None),
    }
}

/// Which actions are drawn: Edit entry and Delete entry exist only on custom rows (Edit entry
/// not on a submenu); everything else is always there.
pub fn present(f: &Facts, act: Act) -> bool {
    match act {
        Act::EditEntry => matches!(f.ty, RowType::Custom { submenu: false }),
        Act::DeleteEntry => matches!(f.ty, RowType::Custom { .. }),
        _ => true,
    }
}

/// The one-line note at the bottom of the popup: why something is disabled. Pin's reason first
/// (it names the kind of row), then Move up / down's.
pub fn note(f: &Facts) -> Option<&'static str> {
    [Act::Pin, Act::Group, Act::Up, Act::Down].into_iter().find_map(|a| state(f, a).1)
}

/// The label of an action.
pub fn label(f: &Facts, act: Act) -> String {
    match act {
        Act::Hide if f.hidden => format!("Show \u{201c}{}\u{201d}", f.name),
        Act::Hide => format!("Hide \u{201c}{}\u{201d}", f.name),
        Act::Rename => "Rename\u{2026}".into(),
        Act::Pin if f.pinned => "Unpin".into(),
        Act::Pin => "Pin to top".into(),
        Act::Group => "Move to group".into(),
        Act::Up => "Move up".into(),
        Act::Down => "Move down".into(),
        Act::EditEntry => "Edit entry\u{2026}".into(),
        Act::DeleteEntry => "Delete entry".into(),
    }
}

fn glyph(act: Act, f: &Facts) -> char {
    match act {
        Act::Hide if f.hidden => '\u{E7B3}',
        Act::Hide => G_EYE_OFF,
        Act::Rename => '\u{E8AC}',
        Act::Pin if f.pinned => '\u{E77A}',
        Act::Pin => '\u{E718}',
        Act::Group => '\u{E8B7}',
        Act::Up => '\u{E74A}',
        Act::Down => '\u{E74B}',
        Act::EditEntry => '\u{E70F}',
        Act::DeleteEntry => '\u{E74D}',
    }
}

fn shortcut(f: &Facts, act: Act) -> &'static str {
    match act {
        // Hide has Del in the menu; in Settings Del is Delete entry
        Act::Hide if f.menu_side => "Del",
        Act::Rename => "F2",
        Act::Up => "Alt+\u{2191}",
        Act::Down => "Alt+\u{2193}",
        Act::DeleteEntry if !f.menu_side => "Del",
        _ => "",
    }
}

/// The popup's rows (the flyout of Move to group is [`group_rows`]).
pub fn rows(f: &Facts) -> Vec<MenuRow> {
    let row = |act: Act| {
        let (on, _) = state(f, act);
        MenuRow {
            icon: RowIcon::Glyph(glyph(act, f)),
            shortcut: shortcut(f, act).to_string(),
            disabled: !on,
            danger: act == Act::DeleteEntry,
            submenu: act == Act::Group,
            ..MenuRow::item(act.key(), &label(f, act))
        }
    };
    let mut out = vec![row(Act::Hide), row(Act::Rename), MenuRow::separator()];
    out.extend([Act::Pin, Act::Group, Act::Up, Act::Down].map(row));
    if present(f, Act::DeleteEntry) {
        out.push(MenuRow::separator());
        for a in [Act::EditEntry, Act::DeleteEntry] {
            if present(f, a) {
                out.push(row(a));
            }
        }
    }
    if f.menu_side {
        out.push(MenuRow::separator());
        out.push(MenuRow { icon: RowIcon::Glyph('\u{E713}'), ..MenuRow::item(EDIT_SETTINGS, "Edit in Settings\u{2026}") });
    }
    if let Some(n) = note(f) {
        out.push(MenuRow { kind: RowKind::Note, ..MenuRow::item(0, n) });
    }
    out
}

/// The flyout of Move to group: the user's groups (the current one marked), New group, Native place.
pub fn group_rows(f: &Facts) -> Vec<MenuRow> {
    let mut out: Vec<MenuRow> = f
        .groups
        .iter()
        .enumerate()
        .map(|(i, (id, name))| MenuRow { radio: *id == f.group, ..MenuRow::item(EDIT_GROUP_BASE + i as u64, name) })
        .collect();
    if !out.is_empty() {
        out.push(MenuRow::separator());
    }
    out.push(MenuRow { icon: RowIcon::Glyph('\u{E710}'), ..MenuRow::item(EDIT_GROUP_NEW, "New group\u{2026}") });
    out.push(MenuRow { icon: RowIcon::Glyph('\u{E7A7}'), radio: f.group.is_empty(), ..MenuRow::item(EDIT_GROUP_NATIVE, "Native place (no group)") });
    out
}

/// What a flyout key means: a user group by id, New group, or the native place (empty id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupPick {
    Group(String),
    New,
    Native,
}

/// The pick behind flyout row `key` of `f`.
pub fn group_pick(f: &Facts, key: u64) -> Option<GroupPick> {
    match key {
        EDIT_GROUP_NEW => Some(GroupPick::New),
        EDIT_GROUP_NATIVE => Some(GroupPick::Native),
        k if k >= EDIT_GROUP_BASE => f.groups.get((k - EDIT_GROUP_BASE) as usize).map(|(id, _)| GroupPick::Group(id.clone())),
        _ => None,
    }
}

/// The indexes of `order` with the pinned rows first, each part in its own order (several pins
/// keep their native order among themselves). `pinned(i)` says whether row `i` is pinned.
pub fn pinned_first(order: &[usize], pinned: impl Fn(usize) -> bool) -> Vec<usize> {
    let (mut head, tail): (Vec<usize>, Vec<usize>) = order.iter().copied().partition(|&i| pinned(i));
    head.extend(tail);
    head
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsubame_config::menu::{self, EntryKind};

    fn pref(key: &str, label: &str, pin: bool) -> ItemPref {
        ItemPref { key: key.into(), label: label.into(), from: "Windows".into(), pin, ..Default::default() }
    }

    fn enabled(f: &Facts) -> Vec<(Act, bool)> {
        [Act::Hide, Act::Rename, Act::Pin, Act::Group, Act::Up, Act::Down, Act::EditEntry, Act::DeleteEntry].into_iter().filter(|a| present(f, *a)).map(|a| (a, state(f, a).0)).collect()
    }

    #[test]
    fn a_windows_row_pins_and_groups_but_never_moves() {
        let f = Facts::learned(&pref("windows.verb:share", "Share", false), &[], false, false);
        assert_eq!(enabled(&f), vec![(Act::Hide, true), (Act::Rename, true), (Act::Pin, true), (Act::Group, true), (Act::Up, false), (Act::Down, false)]);
        assert_eq!(state(&f, Act::Up).1, Some(NOTE_LEARNED));
        assert_eq!(note(&f), Some(NOTE_LEARNED));
        assert_eq!(label(&f, Act::Pin), "Pin to top");
        assert_eq!(label(&f, Act::Hide), "Hide \u{201c}Share\u{201d}");
    }

    #[test]
    fn pinned_and_hidden_rows_flip_their_labels() {
        let mut p = pref("windows.label:7-zip", "7-Zip", true);
        p.hidden = true;
        let f = Facts::learned(&p, &[], false, false);
        assert_eq!(label(&f, Act::Pin), "Unpin");
        assert_eq!(label(&f, Act::Hide), "Show \u{201c}7-Zip\u{201d}");
        // hidden and pinned: everything else stays available
        assert!(state(&f, Act::Pin).0 && state(&f, Act::Group).0);
    }

    #[test]
    fn a_tsubame_row_moves_but_does_not_pin_or_group() {
        let e = MenuEntry::builtin("builtin.copy", "Copy", "file.copy");
        let f = Facts::entry(&e, true, true, false);
        assert_eq!(enabled(&f), vec![(Act::Hide, true), (Act::Rename, true), (Act::Pin, false), (Act::Group, false), (Act::Up, true), (Act::Down, true)]);
        assert_eq!(state(&f, Act::Pin).1, Some(NOTE_OWN));
        assert_eq!(note(&f), Some(NOTE_OWN));
        // at the ends the move is off, with its own reason
        let first = Facts::entry(&e, false, true, false);
        assert_eq!(state(&first, Act::Up), (false, Some(NOTE_FIRST)));
        let last = Facts::entry(&e, true, false, false);
        assert_eq!(state(&last, Act::Down), (false, Some(NOTE_LAST)));
    }

    #[test]
    fn custom_rows_add_edit_and_delete_but_a_submenu_has_no_edit_entry() {
        let c = MenuEntry::custom("custom.1", "Open in VS Code", "code.exe", "");
        let f = Facts::entry(&c, true, true, false);
        let on = enabled(&f);
        assert!(on.contains(&(Act::EditEntry, true)) && on.contains(&(Act::DeleteEntry, true)));
        assert!(!state(&f, Act::Pin).0 && !state(&f, Act::Group).0);
        let sub = MenuEntry { id: "custom.2".into(), kind: EntryKind::Submenu, source: Source::Custom, name: "Tools".into(), ..Default::default() };
        let fs = Facts::entry(&sub, true, true, false);
        assert!(!present(&fs, Act::EditEntry) && present(&fs, Act::DeleteEntry));
        // Windows' and Tsubame's own rows never show them (Delete entry exists only on custom rows)
        let own = Facts::entry(&MenuEntry::builtin("builtin.copy", "Copy", "file.copy"), true, true, false);
        assert!(!present(&own, Act::DeleteEntry) && !present(&own, Act::EditEntry));
        let win = Facts::learned(&pref("windows.verb:share", "Share", false), &[], false, false);
        assert!(!present(&win, Act::DeleteEntry));
    }

    #[test]
    fn a_nested_row_cannot_pin_group_or_move() {
        let f = Facts::learned(&pref("windows.verb:x", "X", false), &[], true, true);
        assert!(!state(&f, Act::Pin).0 && !state(&f, Act::Group).0);
        assert_eq!(state(&f, Act::Pin).1, Some(NOTE_NESTED));
        let mut e = MenuEntry::custom("custom.1", "A", "a.exe", "");
        e.parent = "custom.9".into();
        let fe = Facts::entry(&e, true, true, false);
        assert_eq!(state(&fe, Act::Up), (false, Some(NOTE_NESTED_OWN)));
    }

    #[test]
    fn rows_follow_the_mock_in_both_places() {
        let labels = |v: &[MenuRow]| -> Vec<String> { v.iter().map(|r| match r.kind { RowKind::Separator => "--".into(), RowKind::Note => format!("note: {}", r.label), _ => r.label.clone() }).collect() };
        // Settings, a Windows row
        let f = Facts::learned(&pref("windows.verb:share", "Share", false), &[], false, false);
        let r = rows(&f);
        assert_eq!(labels(&r), vec!["Hide \u{201c}Share\u{201d}", "Rename\u{2026}", "--", "Pin to top", "Move to group", "Move up", "Move down", &format!("note: {NOTE_LEARNED}")]);
        assert!(r[4].submenu && r[5].disabled && r[6].disabled && !r[3].disabled);
        assert!(r.iter().all(|x| x.shortcut != "Del"), "no Del in Settings on a non-custom row");
        // the menu-side copy: Hide has Del, Edit in Settings at the end, Delete entry has no shortcut
        let m = rows(&Facts { menu_side: true, ..f.clone() });
        assert_eq!(m[0].shortcut, "Del");
        assert_eq!(labels(&m)[8], "Edit in Settings\u{2026}");
        let c = MenuEntry::custom("custom.1", "Open in VS Code", "code.exe", "");
        let mc = rows(&Facts::entry(&c, true, true, true));
        let del = mc.iter().find(|x| x.key == EDIT_DELETE).unwrap();
        assert!(del.danger && del.shortcut.is_empty(), "Delete entry has no shortcut in the menu");
        assert_eq!(mc.iter().find(|x| x.key == EDIT_HIDE).unwrap().shortcut, "Del");
        let sc = rows(&Facts::entry(&c, true, true, false));
        assert_eq!(sc.iter().find(|x| x.key == EDIT_DELETE).unwrap().shortcut, "Del", "Settings: Del deletes a custom row");
        assert_eq!(sc.iter().find(|x| x.key == EDIT_HIDE).unwrap().shortcut, "");
        // disabled rows are never hidden: all seven standard actions are always drawn
        for x in [&r, &m, &mc, &sc] {
            for k in [EDIT_HIDE, EDIT_RENAME, EDIT_PIN, EDIT_GROUP, EDIT_UP, EDIT_DOWN] {
                assert!(x.iter().any(|row| row.key == k), "{k:x}");
            }
        }
    }

    #[test]
    fn the_group_flyout_marks_the_current_group() {
        let users = vec![UserGroup { id: "g1".into(), name: "Archives and tools".into(), pos: 0, ..Default::default() }, UserGroup { id: "g2".into(), name: "Clipboard".into(), pos: 1, ..Default::default() }];
        let mut p = pref("windows.verb:share", "Share", false);
        p.group = "g2".into();
        let f = Facts::learned(&p, &users, false, false);
        let g = group_rows(&f);
        assert_eq!(g.iter().map(|r| r.radio).collect::<Vec<_>>(), vec![false, true, false, false, false]);
        assert_eq!(group_pick(&f, EDIT_GROUP_BASE + 1), Some(GroupPick::Group("g2".into())));
        assert_eq!(group_pick(&f, EDIT_GROUP_NEW), Some(GroupPick::New));
        assert_eq!(group_pick(&f, EDIT_GROUP_NATIVE), Some(GroupPick::Native));
        assert_eq!(group_pick(&f, EDIT_GROUP_BASE + 9), None);
        // no user groups yet: New group and Native place (checked) only
        let none = group_rows(&Facts::learned(&pref("windows.verb:share", "Share", false), &[], false, false));
        assert_eq!(none.len(), 2);
        assert!(none[1].radio);
        // a deleted group falls back to the native place
        let mut q = pref("windows.verb:share", "Share", false);
        q.group = "gone".into();
        assert!(Facts::learned(&q, &users, false, false).group.is_empty());
    }

    #[test]
    fn pinned_rows_go_first_in_native_order() {
        let items = vec![pref("a", "A", false), pref("b", "B", true), pref("c", "C", false), pref("d", "D", true)];
        let order = pinned_first(&[0, 1, 2, 3], |i| items[i].pin);
        assert_eq!(order, vec![1, 3, 0, 2], "pins first, each part keeps its order");
        assert_eq!(pinned_first(&[2, 0], |i| items[i].pin), vec![2, 0], "no pins: unchanged");
        let _ = menu::default_entries();
    }
}
