//! The app menu (gear button in the title bar, `F10`): every command the app has, grouped, with
//! its shortcut text. It is the safety net of discoverability: no feature may be reachable only
//! by a hidden shortcut (`docs/discoverability.md`).
//!
//! This file is the model: a static tree of rows (`tree`), the rows a given moment shows
//! (`AppMenu::build`: switched-off extensions drop out, shortcut text follows the user's key
//! map, ticks follow the view), and the rows `NavMenu` hands to the shared menu widget (own-drawn
//! rows, submenus, keyboard and UI Automation come with it). The test at the bottom fails when a
//! command is neither here nor on the allow-list.

use tsubame_ui::{MenuRow, RowIcon};

use crate::commands::*;

/// How a row is ticked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    None,
    /// a check box (on / off)
    Check,
    /// one of a group (a dot on the current one)
    Radio,
}

/// A row of the static tree.
pub enum Node {
    Item { cmd: u32, label: &'static str, glyph: char, mark: Mark },
    Sep,
    Sub { label: &'static str, glyph: char, kids: Vec<Node> },
}

const NO_GLYPH: char = '\0';

fn it(cmd: u32, label: &'static str) -> Node {
    Node::Item { cmd, label, glyph: NO_GLYPH, mark: Mark::None }
}
fn ig(cmd: u32, label: &'static str, glyph: char) -> Node {
    Node::Item { cmd, label, glyph, mark: Mark::None }
}
fn chk(cmd: u32, label: &'static str) -> Node {
    Node::Item { cmd, label, glyph: NO_GLYPH, mark: Mark::Check }
}
fn rad(cmd: u32, label: &'static str) -> Node {
    Node::Item { cmd, label, glyph: NO_GLYPH, mark: Mark::Radio }
}
fn sub(label: &'static str, glyph: char, kids: Vec<Node>) -> Node {
    Node::Sub { label, glyph, kids }
}

/// The whole menu, before the moment's filters.
pub fn tree() -> Vec<Node> {
    vec![
        ig(CMD_SETTINGS, "Settings\u{2026}", '\u{E713}'),
        ig(CMD_SETTINGS_KEYS, "Keyboard shortcuts", '\u{E765}'),
        ig(CMD_PALETTE, "Command palette", '\u{E721}'),
        Node::Sep,
        ig(CMD_NEW_WINDOW, "New window", '\u{E8A7}'),
        Node::Item { cmd: CMD_FULLSCREEN, label: "Full screen", glyph: '\u{E740}', mark: Mark::Check },
        Node::Sep,
        sub(
            "Go",
            '\u{E8B7}',
            vec![
                it(CMD_BACK, "Back"),
                it(CMD_FORWARD, "Forward"),
                it(CMD_UP, "Up one folder"),
                it(CMD_REFRESH, "Refresh"),
                Node::Sep,
                it(CMD_FOCUS_PATH, "Edit the path"),
                it(CMD_COPY_ADDRESS, "Copy address"),
                it(CMD_COPY_ADDRESS_QUOTED, "Copy address as text with quotes"),
                it(CMD_PASTE_AND_GO, "Paste the clipboard as an address and go"),
                Node::Sep,
                it(CMD_FIND, "Filter this folder"),
                it(CMD_GLOBAL_SEARCH, "Search everywhere\u{2026}"),
                Node::Sep,
                it(CMD_PIN_FOLDER, "Pin this folder to the sidebar"),
                it(CMD_REVEAL_SIDEBAR, "Reveal this folder in the sidebar"),
                it(CMD_OPEN_TERMINAL, "Open a terminal here"),
            ],
        ),
        sub(
            "Tabs",
            '\u{E8A1}',
            vec![
                it(CMD_NEW_TAB, "New tab"),
                it(CMD_CLOSE_TAB, "Close tab"),
                it(CMD_NEXT_TAB, "Next tab"),
                it(CMD_PREV_TAB, "Previous tab"),
                it(CMD_REOPEN_TAB, "Reopen closed tab"),
                Node::Sep,
                it(CMD_OPEN_NEW_TAB, "Open the selected folder in a new tab"),
                it(CMD_DUPLICATE_TAB, "Duplicate tab"),
                it(CMD_CLOSE_OTHER_TABS, "Close other tabs"),
                it(CMD_CLOSE_TABS_RIGHT, "Close tabs to the right"),
                it(CMD_TAB_TO_WINDOW, "Move tab to a new window"),
            ],
        ),
        sub(
            "View",
            '\u{E8A9}',
            vec![
                rad(CMD_VIEW_DETAILS, "Details"),
                rad(CMD_VIEW_GRID_SMALL, "Small thumbnails"),
                rad(CMD_VIEW_GRID_LARGE, "Large thumbnails"),
                rad(CMD_VIEW_TREE, "Tree"),
                rad(CMD_VIEW_MILLER, "Miller columns"),
                Node::Sep,
                it(CMD_VIEW_ZOOM_IN, "Bigger icons"),
                it(CMD_VIEW_ZOOM_OUT, "Smaller icons"),
                it(CMD_DENSITY, "Next row density"),
                chk(CMD_ZEBRA, "Striped rows"),
                chk(CMD_AGE_COLOURS, "Age colours"),
                Node::Sep,
                sub(
                    "Group by",
                    NO_GLYPH,
                    vec![
                        rad(CMD_GROUP_NONE, "None"),
                        rad(CMD_GROUP_NAME, "Name"),
                        rad(CMD_GROUP_TYPE, "Type"),
                        rad(CMD_GROUP_DATE, "Date modified"),
                        rad(CMD_GROUP_SIZE, "Size"),
                        Node::Sep,
                        it(CMD_VIEW_GROUP_BY, "Next mode"),
                        it(CMD_GROUP_TOGGLE, "Collapse or expand this group"),
                        it(CMD_GROUP_COLLAPSE_ALL, "Collapse all groups"),
                        it(CMD_GROUP_EXPAND_ALL, "Expand all groups"),
                    ],
                ),
                it(CMD_COLUMNS_MENU, "Choose columns\u{2026}"),
                it(CMD_RESET_COLUMNS, "Reset columns"),
                Node::Sep,
                chk(CMD_HIDDEN, "Hidden files"),
                chk(CMD_SYSTEM, "Protected system files"),
                chk(CMD_VIEW_EXTENSIONS, "File extensions"),
                Node::Sep,
                it(CMD_SIDEBAR, "Collapse or expand the sidebar"),
                it(CMD_PREVIEW_TOGGLE, "Preview pane"),
                it(CMD_PREVIEW_QUICK_LOOK, "Quick Look"),
                it(CMD_OPS_PANEL, "Operations panel"),
                it(CMD_THEME, "Switch dark / light"),
                Node::Sep,
                it(CMD_APPLY_VIEW_ALL, "Use this view for all folders"),
                it(CMD_RESET_FOLDER_VIEWS, "Forget all folder views"),
            ],
        ),
        sub(
            "Panes",
            '\u{E8A0}',
            vec![
                Node::Item { cmd: CMD_SPLIT, label: "Two panes", glyph: NO_GLYPH, mark: Mark::Check },
                it(CMD_SPLIT_ORIENTATION, "Side by side or stacked"),
                it(CMD_SWITCH_PANE, "Switch to the other pane"),
                Node::Sep,
                it(CMD_COPY_OTHER, "Copy the selection to the other pane"),
                it(CMD_MOVE_OTHER, "Move the selection to the other pane"),
                it(CMD_OPEN_OTHER, "Open the selected folder in the other pane"),
                it(CMD_SYNC_OTHER, "Show this folder in the other pane"),
                it(CMD_TAB_TO_OTHER, "Move this tab to the other pane"),
                Node::Sep,
                it(CMD_PANE_TREE, "Folder tree pane"),
            ],
        ),
        sub(
            "Files",
            '\u{E8A5}',
            vec![
                it(CMD_SELECT_ALL, "Select all"),
                it(CMD_SELECT_INVERT, "Invert selection"),
                Node::Sep,
                it(CMD_RENAME, "Rename"),
                it(CMD_BATCH_RENAME, "Batch rename\u{2026}"),
                it(CMD_CREATE_SHORTCUT, "Create shortcut"),
                it(CMD_PASTE_SHORTCUT, "Paste shortcut"),
                it(CMD_TAGS_PICK, "Tags\u{2026}"),
                Node::Sep,
                it(CMD_UNDO, "Undo"),
                it(CMD_REDO, "Redo"),
                Node::Sep,
                it(CMD_ARCHIVE_EXTRACT_HERE, "Extract archive here"),
                it(CMD_ARCHIVE_EXTRACT_SELECTED, "Extract selected from archive"),
                it(CMD_FOLDER_CALC_SIZES, "Calculate folder sizes"),
                it(CMD_FILE_CHECKSUM, "Copy file checksums"),
            ],
        ),
        Node::Sep,
        sub(
            "Settings pages",
            '\u{E713}',
            vec![
                it(CMD_SETTINGS_APPEARANCE, "Themes and appearance"),
                it(CMD_SETTINGS_MENU, "Context menu editor"),
                it(CMD_SETTINGS_DEFAULT_FM, "Default file manager"),
                it(CMD_SETTINGS_UPDATES, "Updates"),
                Node::Sep,
                it(CMD_OPEN_CONFIG, "Open the settings folder"),
                it(CMD_RESET_KEYS, "Reset all keyboard shortcuts"),
            ],
        ),
        ig(CMD_CHECK_UPDATES, "Check for updates", '\u{E895}'),
        ig(CMD_ABOUT, "About Tsubame", '\u{E946}'),
        ig(CMD_QUIT, "Quit", '\u{E7E8}'),
    ]
}

/// Every command id the static tree lists (the test and the audit read it).
#[cfg(test)]
pub fn listed() -> Vec<u32> {
    fn walk(nodes: &[Node], out: &mut Vec<u32>) {
        for n in nodes {
            match n {
                Node::Item { cmd, .. } => out.push(*cmd),
                Node::Sub { kids, .. } => walk(kids, out),
                Node::Sep => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&tree(), &mut out);
    out
}

/// What the view shows right now: the commands whose tick is on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ticks(pub Vec<u32>);

/// A row as the menu shows it at this moment.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Item { key: u64, cmd: u32, label: String, glyph: char, keys: String, mark: Mark, on: bool },
    Sep,
    Sub { key: u64, label: String, glyph: char, kids: Vec<Entry> },
}

/// The menu a click on the gear button opens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppMenu {
    pub entries: Vec<Entry>,
}

impl AppMenu {
    /// The tree with the commands that are on, their shortcut text and ticks. `keys` answers the
    /// shortcut text of a command ("" for none), `known` whether a command is registered (an
    /// extension that is switched off has none: its rows are left out, and so are empty groups).
    pub fn build(ticks: &Ticks, keys: &dyn Fn(u32) -> String, known: &dyn Fn(u32) -> bool) -> AppMenu {
        let mut next = 1u64;
        let entries = resolve(&tree(), ticks, keys, known, &mut next);
        AppMenu { entries }
    }

    /// The rows of the menu at `path` (row keys from the top; empty = the top level).
    pub fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
        let mut level = &self.entries;
        for k in path {
            let Some(Entry::Sub { kids, .. }) = level.iter().find(|e| matches!(e, Entry::Sub { key, .. } if key == k)) else { return };
            level = kids;
        }
        for e in level {
            match e {
                Entry::Sep => out.push(MenuRow::separator()),
                Entry::Item { key, label, glyph, keys, mark, on, .. } => out.push(MenuRow {
                    icon: if *glyph == NO_GLYPH { RowIcon::None } else { RowIcon::Glyph(*glyph) },
                    shortcut: keys.clone(),
                    checked: *on && *mark == Mark::Check,
                    radio: *on && *mark == Mark::Radio,
                    ..MenuRow::item(*key, label)
                }),
                Entry::Sub { key, label, glyph, .. } => out.push(MenuRow {
                    icon: if *glyph == NO_GLYPH { RowIcon::None } else { RowIcon::Glyph(*glyph) },
                    submenu: true,
                    ..MenuRow::item(*key, label)
                }),
            }
        }
    }

    /// The command of the row `key` (None: a submenu row or no such row).
    pub fn cmd_of(&self, key: u64) -> Option<u32> {
        fn find(es: &[Entry], k: u64) -> Option<u32> {
            es.iter().find_map(|e| match e {
                Entry::Item { key, cmd, .. } if *key == k => Some(*cmd),
                Entry::Sub { kids, .. } => find(kids, k),
                _ => None,
            })
        }
        find(&self.entries, key)
    }
}

fn resolve(nodes: &[Node], ticks: &Ticks, keys: &dyn Fn(u32) -> String, known: &dyn Fn(u32) -> bool, next: &mut u64) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::with_capacity(nodes.len());
    for n in nodes {
        match n {
            Node::Item { cmd, label, glyph, mark } => {
                if !known(*cmd) {
                    continue;
                }
                let key = *next;
                *next += 1;
                out.push(Entry::Item { key, cmd: *cmd, label: (*label).to_string(), glyph: *glyph, keys: keys(*cmd), mark: *mark, on: ticks.0.contains(cmd) });
            }
            Node::Sep => {
                // never first, never twice in a row
                if !matches!(out.last(), None | Some(Entry::Sep)) {
                    out.push(Entry::Sep);
                }
            }
            Node::Sub { label, glyph, kids } => {
                let key = *next;
                *next += 1;
                let kids = resolve(kids, ticks, keys, known, next);
                if !kids.is_empty() {
                    out.push(Entry::Sub { key, label: (*label).to_string(), glyph: *glyph, kids });
                }
            }
        }
    }
    while matches!(out.last(), Some(Entry::Sep)) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Commands that are deliberately not in the app menu, each with the reason. A command that is
    /// neither listed in the menu nor here fails `every_command_is_reachable_from_a_menu`: add it
    /// to `tree()` (the usual answer) or, when a menu row would be wrong, here with the reason.
    const NOT_IN_THE_MENU: &[(u32, &str)] = &[
        (CMD_APP_MENU, "this is the menu itself: the gear button in the title bar opens it"),
        (CMD_ESCAPE, "Esc cancels whatever is open; a menu row for 'cancel' would do nothing visible"),
        (CMD_BACKSPACE_UP, "a second key for 'Up one folder', which the Go menu lists"),
        (CMD_RENAME_PREV, "only meaningful inside an open rename box (Shift+Tab), which has its own hint"),
    ];

    fn all_known(_: u32) -> bool {
        true
    }

    #[test]
    fn every_command_is_reachable_from_a_menu() {
        let listed = listed();
        let mut missing = Vec::new();
        for c in all_builtin() {
            let allowed = NOT_IN_THE_MENU.iter().any(|(id, _)| *id == c.id);
            if !listed.contains(&c.id) && !allowed {
                missing.push(format!("{} ({})", c.title, c.name));
            }
        }
        assert!(missing.is_empty(), "no menu entry and not on the allow-list (add to appmenu::tree or NOT_IN_THE_MENU with a reason): {missing:?}");
    }

    #[test]
    fn the_allow_list_has_reasons_and_no_stale_rows() {
        let listed = listed();
        for (id, why) in NOT_IN_THE_MENU {
            assert!(why.len() > 20, "{id}: say why");
            assert!(all_builtin().any(|c| c.id == *id), "{id} is not a command any more");
            assert!(!listed.contains(id), "{id} is in the menu: drop it from the allow-list");
        }
    }

    #[test]
    fn the_menu_lists_only_real_commands_and_each_once() {
        let listed = listed();
        for (i, id) in listed.iter().enumerate() {
            assert!(all_builtin().any(|c| c.id == *id), "{id} is not a command");
            assert!(!listed[i + 1..].contains(id), "{id} is listed twice");
        }
    }

    #[test]
    fn rows_carry_shortcuts_ticks_and_submenus() {
        let keys = |c: u32| if c == CMD_SETTINGS { "Ctrl+,".to_string() } else { String::new() };
        let ticks = Ticks(vec![CMD_VIEW_GRID_SMALL, CMD_HIDDEN]);
        let m = AppMenu::build(&ticks, &keys, &all_known);
        let mut top = Vec::new();
        m.rows(&[], &mut top);
        assert_eq!(top[0].label, "Settings\u{2026}");
        assert_eq!(top[0].shortcut, "Ctrl+,");
        let view = top.iter().find(|r| r.label == "View").expect("View submenu");
        assert!(view.submenu);
        let mut kids = Vec::new();
        m.rows(&[view.key], &mut kids);
        let small = kids.iter().find(|r| r.label == "Small thumbnails").unwrap();
        assert!(small.radio && !small.checked);
        assert!(!kids.iter().find(|r| r.label == "Details").unwrap().radio);
        assert!(kids.iter().find(|r| r.label == "Hidden files").unwrap().checked);
        assert_eq!(m.cmd_of(small.key), Some(CMD_VIEW_GRID_SMALL));
        assert_eq!(m.cmd_of(view.key), None, "a submenu row runs nothing");
    }

    #[test]
    fn row_keys_are_unique_and_stable() {
        let a = AppMenu::build(&Ticks::default(), &|_| String::new(), &all_known);
        let b = AppMenu::build(&Ticks(vec![CMD_ZEBRA]), &|_| String::new(), &all_known);
        let keys = |m: &AppMenu| {
            fn walk(es: &[Entry], out: &mut Vec<u64>) {
                for e in es {
                    match e {
                        Entry::Item { key, .. } => out.push(*key),
                        Entry::Sub { key, kids, .. } => {
                            out.push(*key);
                            walk(kids, out);
                        }
                        Entry::Sep => {}
                    }
                }
            }
            let mut v = Vec::new();
            walk(&m.entries, &mut v);
            v
        };
        let (ka, kb) = (keys(&a), keys(&b));
        assert_eq!(ka, kb, "ticks do not move keys");
        let mut s = ka.clone();
        s.sort();
        s.dedup();
        assert_eq!(s.len(), ka.len());
    }

    #[test]
    fn a_switched_off_command_leaves_no_row_and_no_empty_group() {
        // everything of the Panes group is gone: the group goes too, no double separators remain
        let gone = [CMD_SPLIT, CMD_SPLIT_ORIENTATION, CMD_SWITCH_PANE, CMD_COPY_OTHER, CMD_MOVE_OTHER, CMD_OPEN_OTHER, CMD_SYNC_OTHER, CMD_TAB_TO_OTHER, CMD_PANE_TREE];
        let m = AppMenu::build(&Ticks::default(), &|_| String::new(), &|c| !gone.contains(&c));
        let mut top = Vec::new();
        m.rows(&[], &mut top);
        assert!(top.iter().all(|r| r.label != "Panes"));
        let mut prev_sep = true;
        for r in &top {
            let sep = r.kind == tsubame_ui::RowKind::Separator;
            assert!(!(sep && prev_sep), "no leading or doubled separator");
            prev_sep = sep;
        }
        assert!(!prev_sep, "no trailing separator");
    }

    #[test]
    fn the_keys_come_from_the_command_table() {
        // the default text of the settings command is the one the title bar tip shows
        assert_eq!(by_id(CMD_SETTINGS).unwrap().keys.first().map(|k| k.to_string()), Some("Ctrl+,".to_string()));
        assert!(by_id(CMD_APP_MENU).is_some() && by_id(CMD_ABOUT).is_some() && by_id(CMD_QUIT).is_some());
    }
}
