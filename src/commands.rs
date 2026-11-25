//! The built-in commands. Every action the app can do from the keyboard, the
//! palette and the key settings is a row of `BUILTIN`: a stable number the
//! shortcut table carries, a dotted name for config files and the palette, a
//! title, the default keys and flags. The core extension (`crate::ext`)
//! registers the table; lookups go through the extension registry, so a
//! disabled or pack-provided command is seen the same way.
//!
//! `KeyMap::bind_all` is the only place keys are bound. `View::run_command` is
//! the only place commands run (it dispatches through the registry).

use tsubame_ext_api::{CmdFlags, CommandSpec};
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
// P4 built-in extensions (F3 archive, F5 folder sizes / checksum, F1 global search, F2 preview)
pub const CMD_ARCHIVE_EXTRACT_HERE: u32 = 56;
pub const CMD_ARCHIVE_EXTRACT_SELECTED: u32 = 57;
pub const CMD_FOLDER_CALC_SIZES: u32 = 58;
pub const CMD_FILE_CHECKSUM: u32 = 59;
pub const CMD_GLOBAL_SEARCH: u32 = 60;
pub const CMD_PREVIEW_TOGGLE: u32 = 61;
pub const CMD_PREVIEW_QUICK_LOOK: u32 = 62;
/// F6 file-age colouring (`tsubame.ext.file_age`)
pub const CMD_AGE_COLOURS: u32 = 70;
// F4 batch rename (id kept clear of the other P4 batches in flight)
pub const CMD_BATCH_RENAME: u32 = 72;

/// What the rest of the app reads about a command: id, dotted name, title,
/// default chords (text) and flags. Lives in the extension registry.
pub type Command = CommandSpec;

const G: CmdFlags = CmdFlags::GLOBAL;
const F: CmdFlags = CmdFlags::NEEDS_FOLDER;
const GF: CmdFlags = CmdFlags::GLOBAL.with(CmdFlags::NEEDS_FOLDER);
const NO: CmdFlags = CmdFlags::NONE;

use tsubame_ext_api::builtin_command as cmd;

/// The built-in commands, registered by the core extension (`crate::ext`) in
/// this order. `GLOBAL` keys run even while a text field has the focus;
/// `NEEDS_FOLDER` commands do nothing with the Settings tab in front.
pub static BUILTIN: &[CommandSpec] = &[
    cmd!(CMD_NEW_TAB, "tab.new", "New tab", ["Ctrl+T"], G),
    cmd!(CMD_CLOSE_TAB, "tab.close", "Close tab", ["Ctrl+W", "Ctrl+F4"], G),
    cmd!(CMD_NEXT_TAB, "tab.next", "Next tab", ["Ctrl+Tab"], G),
    cmd!(CMD_PREV_TAB, "tab.prev", "Previous tab", ["Ctrl+Shift+Tab"], G),
    cmd!(CMD_FOCUS_PATH, "nav.focus-path", "Edit the path", ["Ctrl+L", "F4", "Alt+D"], GF),
    cmd!(CMD_FIND, "nav.find", "Filter this folder", ["Ctrl+F"], G),
    cmd!(CMD_BACK, "nav.back", "Back", ["Alt+Left"], GF),
    cmd!(CMD_FORWARD, "nav.forward", "Forward", ["Alt+Right"], GF),
    cmd!(CMD_UP, "nav.up", "Up one folder", ["Alt+Up"], GF),
    cmd!(CMD_THEME, "view.theme", "Switch dark / light", ["Ctrl+Shift+L"], G),
    cmd!(CMD_DENSITY, "view.density", "Next row density", ["Ctrl+Shift+D"], G),
    cmd!(CMD_ZEBRA, "view.zebra", "Toggle striped rows", ["Ctrl+Shift+Z"], G),
    cmd!(CMD_REFRESH, "nav.refresh", "Refresh", ["Ctrl+R"], GF),
    cmd!(CMD_VIEW_DETAILS, "view.details", "Details view", ["Ctrl+Shift+1"], GF),
    cmd!(CMD_VIEW_GRID_SMALL, "view.grid-small", "Small thumbnails", ["Ctrl+Shift+2"], GF),
    cmd!(CMD_VIEW_GRID_LARGE, "view.grid-large", "Large thumbnails", ["Ctrl+Shift+3"], GF),
    cmd!(CMD_SELECT_ALL, "select.all", "Select all", ["Ctrl+A"], F),
    cmd!(CMD_ESCAPE, "select.escape", "Clear filter, then selection", ["Esc"], F),
    cmd!(CMD_BACKSPACE_UP, "nav.backspace-up", "Up one folder (Backspace)", ["Backspace"], F),
    cmd!(CMD_RENAME, "file.rename", "Rename (several items: batch rename)", ["F2"], F),
    cmd!(CMD_SIDEBAR, "sidebar.toggle", "Collapse or expand the sidebar", ["Ctrl+B"], GF),
    cmd!(CMD_SPLIT, "pane.split", "Split the window in two panes", ["Ctrl+\\"], G),
    cmd!(CMD_SPLIT_ORIENTATION, "pane.orientation", "Panes side by side or stacked", ["Ctrl+Shift+\\"], G),
    cmd!(CMD_SWITCH_PANE, "pane.switch", "Switch to the other pane", ["Tab"], NO),
    // F5 / F6 refresh and do nothing when there is one pane
    cmd!(CMD_COPY_OTHER, "pane.copy-to-other", "Copy the selection to the other pane (F5 refreshes with one pane)", ["F5"], F),
    cmd!(CMD_MOVE_OTHER, "pane.move-to-other", "Move the selection to the other pane", ["F6"], F),
    cmd!(CMD_UNDO, "file.undo", "Undo the last file operation", ["Ctrl+Z"], F),
    cmd!(CMD_REDO, "file.redo", "Redo", ["Ctrl+Y"], F),
    cmd!(CMD_PIN_FOLDER, "sidebar.pin-folder", "Pin this folder to the sidebar", ["Ctrl+D"], F),
    cmd!(CMD_OPEN_OTHER, "pane.open-in-other", "Open the selected folder in the other pane", ["Ctrl+Shift+Enter"], F),
    cmd!(CMD_OPEN_NEW_TAB, "tab.open-selected", "Open the selected folder in a new tab", ["Ctrl+Enter"], F),
    cmd!(CMD_TAB_TO_OTHER, "pane.move-tab", "Move this tab to the other pane", [], NO),
    cmd!(CMD_SYNC_OTHER, "pane.sync-folders", "Show this folder in the other pane", [], F),
    cmd!(CMD_PALETTE, "palette.open", "Command palette", ["Ctrl+Shift+P", "Ctrl+K"], G),
    cmd!(CMD_SETTINGS, "settings.open", "Open settings", ["Ctrl+,"], G),
    cmd!(CMD_HIDDEN, "view.hidden", "Show hidden files", ["Ctrl+H"], G),
    cmd!(CMD_SYSTEM, "view.system", "Show protected system files", [], G),
    cmd!(CMD_CHECK_UPDATES, "app.check-updates", "Check for updates", [], G),
    cmd!(CMD_OPEN_CONFIG, "app.open-config-folder", "Open the settings folder", [], G),
    cmd!(CMD_SETTINGS_KEYS, "settings.keys", "Settings: keyboard shortcuts", [], G),
    cmd!(CMD_SETTINGS_APPEARANCE, "settings.appearance", "Settings: themes and appearance", [], G),
    cmd!(CMD_SETTINGS_MENU, "settings.context-menu", "Settings: context menu editor", [], G),
    cmd!(CMD_SETTINGS_UPDATES, "settings.updates", "Settings: updates", [], G),
    cmd!(CMD_SETTINGS_DEFAULT_FM, "settings.default-file-manager", "Settings: default file manager", [], G),
    cmd!(CMD_RESET_KEYS, "settings.reset-keys", "Reset all keyboard shortcuts", [], G),
];

/// Archive extension (`tsubame.vfs.archive`) commands.
pub static ARCHIVE_COMMANDS: &[CommandSpec] = &[
    cmd!(CMD_ARCHIVE_EXTRACT_HERE, "archive.extract_here", "Extract archive here", [], F),
    cmd!(CMD_ARCHIVE_EXTRACT_SELECTED, "archive.extract_selected", "Extract selected from archive", [], F),
];

/// Folder-sizes extension (`tsubame.ext.folder_sizes`) commands.
pub static FOLDER_SIZE_COMMANDS: &[CommandSpec] = &[
    cmd!(CMD_FOLDER_CALC_SIZES, "folder.calc_sizes", "Calculate folder sizes", [], F),
];

/// Checksum extension (`tsubame.ext.checksum`) commands.
pub static CHECKSUM_COMMANDS: &[CommandSpec] = &[
    cmd!(CMD_FILE_CHECKSUM, "file.checksum", "Copy file checksums (SHA-256 / SHA-1 / MD5 / CRC32)", [], F),
];

/// Global-search extension (`tsubame.ext.global_search`) commands.
pub static SEARCH_COMMANDS: &[CommandSpec] = &[
    cmd!(CMD_GLOBAL_SEARCH, "search.global", "Global search\u{2026}", ["Ctrl+Shift+F"], G),
];

/// Preview extension (`tsubame.ext.preview`) commands.
pub static PREVIEW_COMMANDS: &[CommandSpec] = &[
    cmd!(CMD_PREVIEW_TOGGLE, "preview.toggle_pane", "Toggle preview pane", ["Alt+P"], F),
    cmd!(CMD_PREVIEW_QUICK_LOOK, "preview.quick_look", "Quick Look", ["Space", "F3"], F),
];

/// File-age colouring extension (`tsubame.ext.file_age`) commands.
pub static AGE_COMMANDS: &[CommandSpec] = &[
    cmd!(CMD_AGE_COLOURS, "view.age_colours", "Age colours: toggle", [], G),
];

/// Batch-rename extension (`tsubame.ext.batch_rename`) commands. F2 stays on
/// `file.rename`, which hands several items to this one while the extension is on.
pub static BATCH_RENAME_COMMANDS: &[CommandSpec] = &[
    cmd!(CMD_BATCH_RENAME, "file.batch_rename", "Batch rename selected items\u{2026}", [], F),
];

/// Every built-in command across `tsubame.core` and the P4 built-in extensions,
/// in registration order.
#[cfg_attr(not(test), allow(dead_code))]
pub fn all_builtin() -> impl Iterator<Item = &'static CommandSpec> {
    BUILTIN
        .iter()
        .chain(ARCHIVE_COMMANDS)
        .chain(FOLDER_SIZE_COMMANDS)
        .chain(CHECKSUM_COMMANDS)
        .chain(SEARCH_COMMANDS)
        .chain(PREVIEW_COMMANDS)
        .chain(AGE_COMMANDS)
        .chain(BATCH_RENAME_COMMANDS)
}

/// Total number of commands registered by `crate::ext::builtin()`.
#[cfg_attr(not(test), allow(dead_code))]
pub const fn builtin_count() -> usize {
    BUILTIN.len()
        + ARCHIVE_COMMANDS.len()
        + FOLDER_SIZE_COMMANDS.len()
        + CHECKSUM_COMMANDS.len()
        + SEARCH_COMMANDS.len()
        + PREVIEW_COMMANDS.len()
        + AGE_COMMANDS.len()
        + BATCH_RENAME_COMMANDS.len()
}

/// Binds every default key (tests and the bench; the app binds its key map,
/// `KeyMap::bind_all`, which starts from the same defaults).
#[allow(dead_code)]
pub fn bind_all(ui: &mut Ui) {
    crate::keymap::KeyMap::defaults().bind_all(ui);
}

/// The enabled command with this id (a copy; built-in text is borrowed, so no allocation).
pub fn by_id(id: u32) -> Option<Command> {
    crate::ext::read(|r| r.command(id).map(|c| c.spec.clone()))
}

/// The enabled command with this dotted name.
pub fn by_name(name: &str) -> Option<Command> {
    crate::ext::read(|r| r.command_named(name).map(|c| c.spec.clone()))
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
    by_id(id).and_then(|c| c.keys.first().and_then(|t| crate::keymap::parse_chord(t))).map(|c| key_label(c.key, c.mods)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::parse_chord;

    const CS: Mods = Mods::CTRL_SHIFT;
    const CT: Mods = Mods::CTRL;
    const NM: Mods = Mods::NONE;
    const AL: Mods = Mods::ALT;

    /// The table before the registry (X1): every id's keys and global flag, so
    /// the move is proven to change no binding.
    type Before = (u32, &'static [(Key, Mods)], bool);
    const BEFORE: &[Before] = &[
        (CMD_NEW_TAB, &[(Key::Char('T'), CT)], true),
        (CMD_CLOSE_TAB, &[(Key::Char('W'), CT), (Key::F(4), CT)], true),
        (CMD_NEXT_TAB, &[(Key::Tab, CT)], true),
        (CMD_PREV_TAB, &[(Key::Tab, CS)], true),
        (CMD_FOCUS_PATH, &[(Key::Char('L'), CT), (Key::F(4), NM), (Key::Char('D'), AL)], true),
        (CMD_FIND, &[(Key::Char('F'), CT)], true),
        (CMD_BACK, &[(Key::Left, AL)], true),
        (CMD_FORWARD, &[(Key::Right, AL)], true),
        (CMD_UP, &[(Key::Up, AL)], true),
        (CMD_THEME, &[(Key::Char('L'), CS)], true),
        (CMD_DENSITY, &[(Key::Char('D'), CS)], true),
        (CMD_ZEBRA, &[(Key::Char('Z'), CS)], true),
        (CMD_REFRESH, &[(Key::Char('R'), CT)], true),
        (CMD_VIEW_DETAILS, &[(Key::Char('1'), CS)], true),
        (CMD_VIEW_GRID_SMALL, &[(Key::Char('2'), CS)], true),
        (CMD_VIEW_GRID_LARGE, &[(Key::Char('3'), CS)], true),
        (CMD_SELECT_ALL, &[(Key::Char('A'), CT)], false),
        (CMD_ESCAPE, &[(Key::Escape, NM)], false),
        (CMD_BACKSPACE_UP, &[(Key::Backspace, NM)], false),
        (CMD_RENAME, &[(Key::F(2), NM)], false),
        (CMD_SIDEBAR, &[(Key::Char('B'), CT)], true),
        (CMD_SPLIT, &[(Key::Char('\\'), CT)], true),
        (CMD_SPLIT_ORIENTATION, &[(Key::Char('\\'), CS)], true),
        (CMD_SWITCH_PANE, &[(Key::Tab, NM)], false),
        (CMD_COPY_OTHER, &[(Key::F(5), NM)], false),
        (CMD_MOVE_OTHER, &[(Key::F(6), NM)], false),
        (CMD_UNDO, &[(Key::Char('Z'), CT)], false),
        (CMD_REDO, &[(Key::Char('Y'), CT)], false),
        (CMD_PIN_FOLDER, &[(Key::Char('D'), CT)], false),
        (CMD_OPEN_OTHER, &[(Key::Enter, CS)], false),
        (CMD_OPEN_NEW_TAB, &[(Key::Enter, CT)], false),
        (CMD_TAB_TO_OTHER, &[], false),
        (CMD_SYNC_OTHER, &[], false),
        (CMD_PALETTE, &[(Key::Char('P'), CS), (Key::Char('K'), CT)], true),
        (CMD_SETTINGS, &[(Key::Char(','), CT)], true),
        (CMD_HIDDEN, &[(Key::Char('H'), CT)], true),
        (CMD_SYSTEM, &[], true),
        (CMD_CHECK_UPDATES, &[], true),
        (CMD_OPEN_CONFIG, &[], true),
        (CMD_SETTINGS_KEYS, &[], true),
        (CMD_SETTINGS_APPEARANCE, &[], true),
        (CMD_SETTINGS_MENU, &[], true),
        (CMD_SETTINGS_UPDATES, &[], true),
        (CMD_SETTINGS_DEFAULT_FM, &[], true),
        (CMD_RESET_KEYS, &[], true),
    ];

    const FEATURE_BEFORE: &[Before] = &[
        (CMD_ARCHIVE_EXTRACT_HERE, &[], false),
        (CMD_ARCHIVE_EXTRACT_SELECTED, &[], false),
        (CMD_FOLDER_CALC_SIZES, &[], false),
        (CMD_FILE_CHECKSUM, &[], false),
        (CMD_GLOBAL_SEARCH, &[(Key::Char('F'), CS)], true),
        (CMD_PREVIEW_TOGGLE, &[(Key::Char('P'), AL)], false),
        (CMD_PREVIEW_QUICK_LOOK, &[(Key::Space, NM), (Key::F(3), NM)], false),
        (CMD_AGE_COLOURS, &[], true),
        (CMD_BATCH_RENAME, &[], false),
    ];

    /// What ran with the Settings tab in front before the registry.
    fn settings_tab_allowed_before(cmd: u32) -> bool {
        matches!(
            cmd,
            CMD_FIND | CMD_NEW_TAB | CMD_CLOSE_TAB | CMD_NEXT_TAB | CMD_PREV_TAB | CMD_THEME | CMD_DENSITY | CMD_ZEBRA | CMD_SPLIT | CMD_SPLIT_ORIENTATION | CMD_SWITCH_PANE | CMD_TAB_TO_OTHER | CMD_GLOBAL_SEARCH | CMD_AGE_COLOURS
        ) || (CMD_PALETTE..=CMD_RESET_KEYS).contains(&cmd)
    }

    #[test]
    fn the_table_keeps_every_id_key_and_flag_and_order() {
        let all: Vec<&CommandSpec> = all_builtin().collect();
        let expected: Vec<Before> = BEFORE.iter().chain(FEATURE_BEFORE).copied().collect();
        assert_eq!(all.len(), expected.len());
        for (spec, (id, keys, global)) in all.iter().zip(&expected) {
            assert_eq!(spec.id, *id, "same order");
            let parsed: Vec<(Key, Mods)> = spec.keys.iter().map(|t| parse_chord(t).map(|c| (c.key, c.mods)).unwrap_or_else(|| panic!("{t}"))).collect();
            assert_eq!(parsed, *keys, "{}", spec.name);
            assert_eq!(spec.flags.has(CmdFlags::GLOBAL), *global, "{}", spec.name);
            assert_eq!(!spec.flags.has(CmdFlags::NEEDS_FOLDER), settings_tab_allowed_before(spec.id), "{}", spec.name);
            // the text is the canonical label, so menus show it unchanged
            for t in spec.keys.iter() {
                let c = parse_chord(t).unwrap();
                assert_eq!(key_label(c.key, c.mods), *t);
            }
        }
    }

    #[test]
    fn ids_and_names_are_unique() {
        let all: Vec<&CommandSpec> = all_builtin().collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
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
        for cmd in all_builtin() {
            for t in cmd.keys.iter() {
                let c = parse_chord(t).unwrap();
                if let Some((_, _, other)) = seen.iter().find(|(k2, m2, _)| *k2 == c.key && *m2 == c.mods) {
                    panic!("{t} is bound to {} and {}", other, cmd.id);
                }
                seen.push((c.key, c.mods, cmd.id));
            }
        }
    }

    #[test]
    fn lookup_and_labels() {
        assert_eq!(by_name("pane.split").map(|c| c.id), Some(CMD_SPLIT));
        assert_eq!(by_id(CMD_RENAME).map(|c| c.name), Some("file.rename".into()));
        assert_eq!(key_label(Key::Char('\\'), Mods::CTRL_SHIFT), "Ctrl+Shift+\\");
        assert_eq!(first_key_label(CMD_SIDEBAR), "Ctrl+B");
        assert_eq!(first_key_label(CMD_RENAME), "F2");
        assert_eq!(first_key_label(CMD_TAB_TO_OTHER), "");
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn the_brief_s_keys_are_there() {
        let has = |id: u32, t: &str| by_id(id).unwrap().keys.iter().any(|k| k == t);
        assert!(has(CMD_RENAME, "F2"));
        assert!(has(CMD_SIDEBAR, "Ctrl+B"));
        assert!(has(CMD_SWITCH_PANE, "Tab"));
        assert!(has(CMD_SPLIT, "Ctrl+\\"));
        assert!(has(CMD_COPY_OTHER, "F5"));
        assert!(has(CMD_GLOBAL_SEARCH, "Ctrl+Shift+F"));
        assert!(has(CMD_PREVIEW_TOGGLE, "Alt+P"));
        assert!(has(CMD_PREVIEW_QUICK_LOOK, "Space"));
        assert!(has(CMD_PREVIEW_QUICK_LOOK, "F3"));
        assert_eq!(by_name("archive.extract_here").map(|c| c.id), Some(CMD_ARCHIVE_EXTRACT_HERE));
        assert_eq!(by_name("archive.extract_selected").map(|c| c.id), Some(CMD_ARCHIVE_EXTRACT_SELECTED));
        assert_eq!(by_name("folder.calc_sizes").map(|c| c.id), Some(CMD_FOLDER_CALC_SIZES));
        assert_eq!(by_name("file.checksum").map(|c| c.id), Some(CMD_FILE_CHECKSUM));
        assert_eq!(by_name("search.global").map(|c| c.id), Some(CMD_GLOBAL_SEARCH));
        assert_eq!(by_name("preview.toggle_pane").map(|c| c.id), Some(CMD_PREVIEW_TOGGLE));
        assert_eq!(by_name("preview.quick_look").map(|c| c.id), Some(CMD_PREVIEW_QUICK_LOOK));
        assert_eq!(by_name("view.age_colours").map(|c| c.id), Some(CMD_AGE_COLOURS));
    }
}
