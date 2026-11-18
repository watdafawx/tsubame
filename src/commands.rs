//! The one table of named commands. Every action the app can do from the
//! keyboard (or, later, from the command palette and the key settings) is a
//! row here: a stable number the shortcut table carries, a dotted name for
//! config files and the palette, a title, and the default keys.
//!
//! `bind_all` is the only place keys are bound. `View::run_command` is the
//! only place commands run.

use tsubame_ui::{Key, Mods, Ui};

pub const CMD_NEW_TAB: u32 = 1;
pub const CMD_CLOSE_TAB: u32 = 2;
pub const CMD_NEXT_TAB: u32 = 3;
pub const CMD_PREV_TAB: u32 = 4;
pub const CMD_FOCUS_PATH: u32 = 5;
pub const CMD_FIND: u32 = 6;
pub const CMD_BACK: u32 = 7;
pub const CMD_FORWARD: u32 = 8;
pub const CMD_UP: u32 = 9;
pub const CMD_THEME: u32 = 10;
pub const CMD_DENSITY: u32 = 11;
pub const CMD_ZEBRA: u32 = 12;
pub const CMD_REFRESH: u32 = 13;
pub const CMD_VIEW_DETAILS: u32 = 14;
pub const CMD_VIEW_GRID_SMALL: u32 = 15;
pub const CMD_VIEW_GRID_LARGE: u32 = 16;
/// non-global: only when no widget took the key
pub const CMD_SELECT_ALL: u32 = 20;
pub const CMD_ESCAPE: u32 = 21;
pub const CMD_BACKSPACE_UP: u32 = 22;
// panes, sidebar, rename, file operations
pub const CMD_RENAME: u32 = 30;
pub const CMD_SIDEBAR: u32 = 31;
pub const CMD_SPLIT: u32 = 32;
pub const CMD_SPLIT_ORIENTATION: u32 = 33;
pub const CMD_SWITCH_PANE: u32 = 34;
pub const CMD_COPY_OTHER: u32 = 35;
pub const CMD_MOVE_OTHER: u32 = 36;
pub const CMD_UNDO: u32 = 37;
pub const CMD_REDO: u32 = 38;
pub const CMD_PIN_FOLDER: u32 = 39;
pub const CMD_OPEN_OTHER: u32 = 40;
pub const CMD_OPEN_NEW_TAB: u32 = 41;
pub const CMD_TAB_TO_OTHER: u32 = 42;
pub const CMD_SYNC_OTHER: u32 = 43;
// U4: palette, settings, hidden files, updates
pub const CMD_PALETTE: u32 = 44;
pub const CMD_SETTINGS: u32 = 45;
pub const CMD_HIDDEN: u32 = 46;
pub const CMD_SYSTEM: u32 = 47;
pub const CMD_CHECK_UPDATES: u32 = 48;
pub const CMD_OPEN_CONFIG: u32 = 49;
pub const CMD_SETTINGS_KEYS: u32 = 50;
pub const CMD_SETTINGS_APPEARANCE: u32 = 51;
pub const CMD_SETTINGS_MENU: u32 = 52;
pub const CMD_SETTINGS_UPDATES: u32 = 53;
pub const CMD_SETTINGS_DEFAULT_FM: u32 = 54;
pub const CMD_RESET_KEYS: u32 = 55;

pub struct Command {
    pub id: u32,
    /// dotted name: `group.action`
    pub name: &'static str,
    pub title: &'static str,
    pub keys: &'static [(Key, Mods)],
    /// global keys run even while a text field has the focus
    pub global: bool,
}

const fn c(id: u32, name: &'static str, title: &'static str, keys: &'static [(Key, Mods)], global: bool) -> Command {
    Command { id, name, title, keys, global }
}

const CS: Mods = Mods::CTRL_SHIFT;
const CT: Mods = Mods::CTRL;
const NO: Mods = Mods::NONE;
const AL: Mods = Mods::ALT;

pub static COMMANDS: &[Command] = &[
    c(CMD_NEW_TAB, "tab.new", "New tab", &[(Key::Char('T'), CT)], true),
    c(CMD_CLOSE_TAB, "tab.close", "Close tab", &[(Key::Char('W'), CT), (Key::F(4), CT)], true),
    c(CMD_NEXT_TAB, "tab.next", "Next tab", &[(Key::Tab, CT)], true),
    c(CMD_PREV_TAB, "tab.prev", "Previous tab", &[(Key::Tab, CS)], true),
    c(CMD_FOCUS_PATH, "nav.focus-path", "Edit the path", &[(Key::Char('L'), CT), (Key::F(4), NO), (Key::Char('D'), AL)], true),
    c(CMD_FIND, "nav.find", "Filter this folder", &[(Key::Char('F'), CT)], true),
    c(CMD_BACK, "nav.back", "Back", &[(Key::Left, AL)], true),
    c(CMD_FORWARD, "nav.forward", "Forward", &[(Key::Right, AL)], true),
    c(CMD_UP, "nav.up", "Up one folder", &[(Key::Up, AL)], true),
    c(CMD_THEME, "view.theme", "Switch dark / light", &[(Key::Char('L'), CS)], true),
    c(CMD_DENSITY, "view.density", "Next row density", &[(Key::Char('D'), CS)], true),
    c(CMD_ZEBRA, "view.zebra", "Toggle striped rows", &[(Key::Char('Z'), CS)], true),
    c(CMD_REFRESH, "nav.refresh", "Refresh", &[(Key::Char('R'), CT)], true),
    c(CMD_VIEW_DETAILS, "view.details", "Details view", &[(Key::Char('1'), CS)], true),
    c(CMD_VIEW_GRID_SMALL, "view.grid-small", "Small thumbnails", &[(Key::Char('2'), CS)], true),
    c(CMD_VIEW_GRID_LARGE, "view.grid-large", "Large thumbnails", &[(Key::Char('3'), CS)], true),
    c(CMD_SELECT_ALL, "select.all", "Select all", &[(Key::Char('A'), CT)], false),
    c(CMD_ESCAPE, "select.escape", "Clear filter, then selection", &[(Key::Escape, NO)], false),
    c(CMD_BACKSPACE_UP, "nav.backspace-up", "Up one folder (Backspace)", &[(Key::Backspace, NO)], false),
    c(CMD_RENAME, "file.rename", "Rename (several items: batch rename)", &[(Key::F(2), NO)], false),
    c(CMD_SIDEBAR, "sidebar.toggle", "Collapse or expand the sidebar", &[(Key::Char('B'), CT)], true),
    c(CMD_SPLIT, "pane.split", "Split the window in two panes", &[(Key::Char('\\'), CT)], true),
    c(CMD_SPLIT_ORIENTATION, "pane.orientation", "Panes side by side or stacked", &[(Key::Char('\\'), CS)], true),
    c(CMD_SWITCH_PANE, "pane.switch", "Switch to the other pane", &[(Key::Tab, NO)], false),
    // F5 / F6 refresh and do nothing when there is one pane
    c(CMD_COPY_OTHER, "pane.copy-to-other", "Copy the selection to the other pane (F5 refreshes with one pane)", &[(Key::F(5), NO)], false),
    c(CMD_MOVE_OTHER, "pane.move-to-other", "Move the selection to the other pane", &[(Key::F(6), NO)], false),
    c(CMD_UNDO, "file.undo", "Undo the last file operation", &[(Key::Char('Z'), CT)], false),
    c(CMD_REDO, "file.redo", "Redo", &[(Key::Char('Y'), CT)], false),
    c(CMD_PIN_FOLDER, "sidebar.pin-folder", "Pin this folder to the sidebar", &[(Key::Char('D'), CT)], false),
    c(CMD_OPEN_OTHER, "pane.open-in-other", "Open the selected folder in the other pane", &[(Key::Enter, CS)], false),
    c(CMD_OPEN_NEW_TAB, "tab.open-selected", "Open the selected folder in a new tab", &[(Key::Enter, CT)], false),
    c(CMD_TAB_TO_OTHER, "pane.move-tab", "Move this tab to the other pane", &[], false),
    c(CMD_SYNC_OTHER, "pane.sync-folders", "Show this folder in the other pane", &[], false),
    c(CMD_PALETTE, "palette.open", "Command palette", &[(Key::Char('P'), CS), (Key::Char('K'), CT)], true),
    c(CMD_SETTINGS, "settings.open", "Open settings", &[(Key::Char(','), CT)], true),
    c(CMD_HIDDEN, "view.hidden", "Show hidden files", &[(Key::Char('H'), CT)], true),
    c(CMD_SYSTEM, "view.system", "Show protected system files", &[], true),
    c(CMD_CHECK_UPDATES, "app.check-updates", "Check for updates", &[], true),
    c(CMD_OPEN_CONFIG, "app.open-config-folder", "Open the settings folder", &[], true),
    c(CMD_SETTINGS_KEYS, "settings.keys", "Settings: keyboard shortcuts", &[], true),
    c(CMD_SETTINGS_APPEARANCE, "settings.appearance", "Settings: themes and appearance", &[], true),
    c(CMD_SETTINGS_MENU, "settings.context-menu", "Settings: context menu editor", &[], true),
    c(CMD_SETTINGS_UPDATES, "settings.updates", "Settings: updates", &[], true),
    c(CMD_SETTINGS_DEFAULT_FM, "settings.default-file-manager", "Settings: default file manager", &[], true),
    c(CMD_RESET_KEYS, "settings.reset-keys", "Reset all keyboard shortcuts", &[], true),
];

/// Binds every default key (tests and the bench; the app binds its key map,
/// `KeyMap::bind_all`, which starts from the same defaults).
#[allow(dead_code)]
pub fn bind_all(ui: &mut Ui) {
    for cmd in COMMANDS {
        for (key, mods) in cmd.keys {
            ui.shortcuts.bind(*key, *mods, cmd.id, cmd.global);
        }
    }
}

pub fn by_id(id: u32) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

pub fn by_name(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// "Ctrl+Shift+\" for tooltips and menus.
pub fn key_label(key: Key, mods: Mods) -> String {
    let mut s = String::new();
    if mods.ctrl {
        s.push_str("Ctrl+");
    }
    if mods.shift {
        s.push_str("Shift+");
    }
    if mods.alt {
        s.push_str("Alt+");
    }
    match key {
        Key::Char(c) => s.push(c),
        Key::F(n) => s.push_str(&format!("F{n}")),
        Key::Enter => s.push_str("Enter"),
        Key::Escape => s.push_str("Esc"),
        Key::Tab => s.push_str("Tab"),
        Key::Backspace => s.push_str("Backspace"),
        Key::Delete => s.push_str("Del"),
        Key::Up => s.push_str("Up"),
        Key::Down => s.push_str("Down"),
        Key::Left => s.push_str("Left"),
        Key::Right => s.push_str("Right"),
        Key::Space => s.push_str("Space"),
        other => s.push_str(&format!("{other:?}")),
    }
    s
}

/// The first key of a command as text ("" when it has none): the user's
/// binding once the key map is loaded (`keymap::set_current`), else the default.
pub fn first_key_label(id: u32) -> String {
    if let Some(l) = crate::keymap::current_label(id) {
        return l;
    }
    by_id(id).and_then(|c| c.keys.first()).map(|(k, m)| key_label(*k, *m)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names_are_unique() {
        for (i, a) in COMMANDS.iter().enumerate() {
            for b in &COMMANDS[i + 1..] {
                assert_ne!(a.id, b.id, "{} / {}", a.name, b.name);
                assert_ne!(a.name, b.name);
            }
            assert!(a.name.contains('.'), "{} is group.action", a.name);
            assert!(!a.title.is_empty());
        }
    }

    #[test]
    fn no_key_is_bound_twice() {
        let mut seen: Vec<(Key, Mods, u32)> = Vec::new();
        for cmd in COMMANDS {
            for (k, m) in cmd.keys {
                if let Some((_, _, other)) = seen.iter().find(|(k2, m2, _)| k2 == k && m2 == m) {
                    panic!("{:?}+{k:?} is bound to {} and {}", m, other, cmd.id);
                }
                seen.push((*k, *m, cmd.id));
            }
        }
    }

    #[test]
    fn lookup_and_labels() {
        assert_eq!(by_name("pane.split").map(|c| c.id), Some(CMD_SPLIT));
        assert_eq!(by_id(CMD_RENAME).map(|c| c.name), Some("file.rename"));
        assert_eq!(key_label(Key::Char('\\'), Mods::CTRL_SHIFT), "Ctrl+Shift+\\");
        assert_eq!(first_key_label(CMD_SIDEBAR), "Ctrl+B");
        assert_eq!(first_key_label(CMD_RENAME), "F2");
        assert_eq!(first_key_label(CMD_TAB_TO_OTHER), "");
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn the_brief_s_keys_are_there() {
        let has = |id: u32, k: Key, m: Mods| by_id(id).unwrap().keys.contains(&(k, m));
        assert!(has(CMD_RENAME, Key::F(2), Mods::NONE));
        assert!(has(CMD_SIDEBAR, Key::Char('B'), Mods::CTRL));
        assert!(has(CMD_SWITCH_PANE, Key::Tab, Mods::NONE));
        assert!(has(CMD_SPLIT, Key::Char('\\'), Mods::CTRL));
        assert!(has(CMD_COPY_OTHER, Key::F(5), Mods::NONE));
    }
}
