//! The key map (U4): which chords run which command. Defaults come from the
//! enabled commands of the extension registry (`CmdList`); the user's changes live in `[keys.overrides]` of the
//! config (`"tab.new" = ["Ctrl+N", "Ctrl+T"]`, an empty list = unbound), are
//! loaded at startup and applied live from Settings > Keys.
//!
//! Pure data and logic: no window, no files. `chord_text` / `parse_chord`
//! round-trip, `KeyMap::conflict` finds the command a chord already runs.

use crate::commands::{self, Command};
use std::collections::BTreeMap;
use std::cell::RefCell;
use std::sync::Arc;
use tsubame_ui::{Key, Mods, Ui};

/// At most this many chords per command (the Keys page shows two slots).
pub const MAX_CHORDS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    pub mods: Mods,
}

impl Chord {
    pub const fn new(key: Key, mods: Mods) -> Chord {
        Chord { key, mods }
    }
}

/// Virtual-key codes of the punctuation keys the window maps to `Key::Char`.
pub fn oem_char(vk: u16) -> Option<char> {
    Some(match vk {
        0xBA => ';',
        0xBB => '=',
        0xBC => ',',
        0xBD => '-',
        0xBE => '.',
        0xBF => '/',
        0xC0 => '`',
        0xDB => '[',
        0xDD => ']',
        0xDE => '\'',
        _ => return None,
    })
}

/// Modifier keys on their own are not chords (the recorder waits for the next key).
pub fn is_modifier_key(key: Key) -> bool {
    matches!(key, Key::Other(0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5 | 0x14))
}

/// "Ctrl+Shift+T". Modifiers in the fixed order Ctrl, Shift, Alt.
pub fn chord_text(c: Chord) -> String {
    commands::key_label(c.key, c.mods)
}

/// Parses "Ctrl+Shift+T", "ctrl+,", "Alt+Left", "F5", "Ctrl++" (any order, any case).
pub fn parse_chord(s: &str) -> Option<Chord> {
    let parts = tsubame_ui::split_chord(s.trim());
    let (last, mods_part) = parts.split_last()?;
    let mut mods = Mods::NONE;
    for m in mods_part {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" => mods.alt = true,
            _ => return None,
        }
    }
    Some(Chord { key: parse_key(last)?, mods })
}

fn parse_key(s: &str) -> Option<Key> {
    let l = s.to_ascii_lowercase();
    Some(match l.as_str() {
        "enter" | "return" => Key::Enter,
        "esc" | "escape" => Key::Escape,
        "tab" => Key::Tab,
        "backspace" | "back" => Key::Backspace,
        "del" | "delete" => Key::Delete,
        "ins" | "insert" => Key::Insert,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" | "pgdown" => Key::PageDown,
        "space" => Key::Space,
        _ => {
            if let Some(n) = l.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                return (1..=24).contains(&n).then_some(Key::F(n));
            }
            let mut it = s.chars();
            let ch = it.next()?;
            if it.next().is_some() {
                return None;
            }
            let ch = ch.to_ascii_uppercase();
            if ch.is_ascii_alphanumeric() || matches!(ch, ',' | '.' | '/' | ';' | '\'' | '[' | ']' | '-' | '=' | '`' | '\\' | '+') {
                Key::Char(ch)
            } else {
                return None;
            }
        }
    })
}

/// Why a chord cannot be bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// a plain letter, digit, punctuation or Space would hijack typing
    NeedsModifier,
    /// Esc, Enter, Tab and the arrows alone belong to the lists and fields
    Reserved,
    /// Alt+F4, Ctrl+Alt+Del and friends belong to Windows
    Windows,
}

/// Can `c` be bound to a command? Plain keys that type text, and the keys
/// every widget needs, are refused so a rebind can never break typing.
pub fn bindable(c: Chord) -> Result<(), Reject> {
    let typing = matches!(c.key, Key::Char(_) | Key::Space);
    if typing && !c.mods.ctrl && !c.mods.alt {
        return Err(Reject::NeedsModifier);
    }
    if c.key == Key::F(4) && c.mods.alt && !c.mods.ctrl {
        return Err(Reject::Windows);
    }
    if matches!(c.key, Key::Escape | Key::Enter | Key::Tab | Key::Left | Key::Right | Key::Up | Key::Down) && c.mods.none() {
        return Err(Reject::Reserved);
    }
    Ok(())
}

/// A chord's current owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub command: u32,
}

/// The enabled commands in registry order, with their default chords parsed
/// once. Every `KeyMap` shares one; it is rebuilt when the registry changes.
#[derive(Debug, PartialEq)]
pub struct CmdList {
    cmds: Vec<Command>,
    defaults: Vec<Vec<Chord>>,
}

impl CmdList {
    pub fn from_registry(r: &crate::ext::Registry) -> CmdList {
        let cmds: Vec<Command> = r.commands.iter().map(|c| c.spec.clone()).collect();
        let defaults = cmds.iter().map(|c| c.keys.iter().filter_map(|t| parse_chord(t)).collect()).collect();
        CmdList { cmds, defaults }
    }

    pub fn commands(&self) -> &[Command] {
        &self.cmds
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.cmds.iter().position(|c| c.id == id)
    }

    fn index_named(&self, name: &str) -> Option<usize> {
        self.cmds.iter().position(|c| c.name == name)
    }
}

/// The current registry's command list (cached by registry generation).
pub fn command_list() -> Arc<CmdList> {
    thread_local! {
        static CACHE: RefCell<Option<(u64, Arc<CmdList>)>> = const { RefCell::new(None) };
    }
    let g = crate::ext::generation();
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((seen, l)) = c.as_ref()
            && *seen == g
        {
            return l.clone();
        }
        let l = Arc::new(crate::ext::read(CmdList::from_registry));
        *c = Some((g, l.clone()));
        l
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyMap {
    /// the commands this map is for
    list: Arc<CmdList>,
    /// one entry per `list` command, same order
    chords: Vec<Vec<Chord>>,
}

impl Default for KeyMap {
    fn default() -> Self {
        KeyMap::defaults()
    }
}

impl KeyMap {
    pub fn defaults() -> KeyMap {
        KeyMap::defaults_for(command_list())
    }

    /// The default chords of `list` (tests use their own registry).
    pub fn defaults_for(list: Arc<CmdList>) -> KeyMap {
        let chords = list.defaults.clone();
        KeyMap { list, chords }
    }

    /// The commands, in the order the Keys page and the palette list them.
    pub fn commands(&self) -> &[Command] {
        &self.list.cmds
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.list.index_of(id)
    }

    /// The defaults with `overrides` applied. Unknown command names and
    /// unparsable or unbindable chords are skipped, and a chord already taken
    /// by an earlier override is skipped too (the file never makes two
    /// commands share a key); `warnings` says what was dropped.
    pub fn from_overrides(overrides: &BTreeMap<String, Vec<String>>) -> (KeyMap, Vec<String>) {
        KeyMap::from_overrides_for(command_list(), overrides)
    }

    pub fn from_overrides_for(list: Arc<CmdList>, overrides: &BTreeMap<String, Vec<String>>) -> (KeyMap, Vec<String>) {
        let mut map = KeyMap::defaults_for(list);
        let mut warnings = Vec::new();
        // overridden commands start empty; the others are set aside, so an
        // override wins over a default chord and never conflicts with one
        let n = map.list.cmds.len();
        let mut kept: Vec<Option<Vec<Chord>>> = Vec::with_capacity(n);
        for i in 0..n {
            let overridden = overrides.contains_key(&*map.list.cmds[i].name);
            let old = std::mem::take(&mut map.chords[i]);
            kept.push((!overridden).then_some(old));
        }
        for name in overrides.keys() {
            // a switched-off extension's command is known, just not bound now
            if map.list.index_named(name).is_none() && !crate::ext::read(|r| r.knows_command(name)) {
                warnings.push(format!("keys.overrides: unknown command \"{name}\""));
            }
        }
        for (name, list) in overrides {
            let Some(i) = map.list.index_named(name) else { continue };
            let cmd_id = map.list.cmds[i].id;
            for text in list.iter().take(MAX_CHORDS) {
                let Some(ch) = parse_chord(text) else {
                    warnings.push(format!("keys.overrides.{name}: cannot read \"{text}\""));
                    continue;
                };
                if let Err(why) = bindable(ch) {
                    warnings.push(format!("keys.overrides.{name}: \"{text}\" cannot be bound ({why:?})"));
                    continue;
                }
                if let Some(c) = map.conflict(ch, cmd_id) {
                    let other = map.index_of(c.command).map(|k| map.list.cmds[k].name.clone()).unwrap_or("?".into());
                    warnings.push(format!("keys.overrides.{name}: \"{text}\" is already used by {other}"));
                    continue;
                }
                if !map.chords[i].contains(&ch) {
                    map.chords[i].push(ch);
                }
            }
        }
        // the other commands keep the defaults no override took
        for (i, old) in kept.into_iter().enumerate() {
            if let Some(old) = old {
                let id = map.list.cmds[i].id;
                map.chords[i] = old.into_iter().filter(|c| map.conflict(*c, id).is_none()).collect();
            }
        }
        (map, warnings)
    }

    pub fn chords(&self, id: u32) -> &[Chord] {
        self.index_of(id).map(|i| self.chords[i].as_slice()).unwrap_or(&[])
    }

    /// The first chord as text ("" when unbound): what menus and the palette show.
    pub fn label(&self, id: u32) -> String {
        self.chords(id).first().map(|c| chord_text(*c)).unwrap_or_default()
    }

    pub fn is_default(&self, id: u32) -> bool {
        match self.index_of(id) {
            Some(i) => self.chords[i] == self.list.defaults[i],
            None => true,
        }
    }

    /// Commands whose chords differ from the defaults.
    pub fn changed(&self) -> usize {
        (0..self.chords.len()).filter(|&i| self.chords[i] != self.list.defaults[i]).count()
    }

    /// The command that runs `c` now, other than `except`.
    pub fn conflict(&self, c: Chord, except: u32) -> Option<Conflict> {
        self.list.cmds.iter().enumerate().find(|(i, cmd)| cmd.id != except && self.chords[*i].contains(&c)).map(|(_, cmd)| Conflict { command: cmd.id })
    }

    /// Puts `c` in slot `slot` (0 or 1; a slot past the end appends) of `id`.
    /// A chord another command runs is taken from it ("assign here, unbind
    /// there"); callers show the conflict first. Returns the command that lost it.
    pub fn assign(&mut self, id: u32, slot: usize, c: Chord) -> Result<Option<u32>, Reject> {
        bindable(c)?;
        let Some(i) = self.index_of(id) else { return Ok(None) };
        let lost = self.conflict(c, id).map(|k| k.command);
        if let Some(l) = lost.and_then(|l| self.index_of(l)) {
            self.chords[l].retain(|x| *x != c);
        }
        let list = &mut self.chords[i];
        list.retain(|x| *x != c);
        let slot = slot.min(list.len()).min(MAX_CHORDS - 1);
        if slot < list.len() {
            list[slot] = c;
        } else {
            list.push(c);
        }
        Ok(lost)
    }

    /// Removes the chord in `slot` of `id`.
    pub fn clear_slot(&mut self, id: u32, slot: usize) {
        if let Some(i) = self.index_of(id)
            && slot < self.chords[i].len()
        {
            self.chords[i].remove(slot);
        }
    }

    pub fn reset(&mut self, id: u32) {
        let Some(i) = self.index_of(id) else { return };
        let defaults = self.list.defaults[i].clone();
        // a default another command now holds stays with that one
        let free: Vec<Chord> = defaults.into_iter().filter(|c| self.conflict(*c, id).is_none()).collect();
        self.chords[i] = free;
    }

    pub fn reset_all(&mut self) {
        self.chords = self.list.defaults.clone();
    }

    /// `[keys.overrides]` for the config: only commands that differ from the
    /// defaults. Entries of `keep` for names this build does not know stay
    /// (a newer version's file is not trimmed by an older one).
    pub fn to_overrides(&self, keep: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = keep.iter().filter(|(k, _)| self.list.index_named(k).is_none()).map(|(k, v)| (k.clone(), v.clone())).collect();
        for (i, cmd) in self.list.cmds.iter().enumerate() {
            if self.chords[i] != self.list.defaults[i] {
                out.insert(cmd.name.to_string(), self.chords[i].iter().map(|c| chord_text(*c)).collect());
            }
        }
        out
    }

    /// Fills the kit's shortcut table (replaces whatever was bound).
    pub fn bind_all(&self, ui: &mut Ui) {
        ui.shortcuts.list.clear();
        for (i, cmd) in self.list.cmds.iter().enumerate() {
            for c in &self.chords[i] {
                ui.shortcuts.bind(c.key, c.mods, cmd.id, cmd.flags.has(tsubame_ext_api::CmdFlags::GLOBAL));
            }
        }
    }

    /// The command a chord runs (global or not).
    #[cfg(test)]
    pub fn lookup(&self, c: Chord) -> Option<u32> {
        self.list.cmds.iter().enumerate().find(|(i, _)| self.chords[*i].contains(&c)).map(|(_, cmd)| cmd.id)
    }
}

static CURRENT: std::sync::RwLock<Option<KeyMap>> = std::sync::RwLock::new(None);

/// Publishes the live key map so menus, tooltips and the palette read the
/// current bindings (`commands::first_key_label`). Called at startup and after every change.
pub fn set_current(m: &KeyMap) {
    if let Ok(mut g) = CURRENT.write() {
        *g = Some(m.clone());
    }
}

/// The first chord of `id` in the published map; `None` before one is published.
pub fn current_label(id: u32) -> Option<String> {
    CURRENT.read().ok()?.as_ref().map(|m| m.label(id))
}

/// The category a command is listed under (Keys page groups, palette column).
pub fn category(name: &str) -> &'static str {
    match name.split('.').next().unwrap_or("") {
        "tab" => "Tabs",
        "nav" | "search" => "Navigate",
        "file" => "File",
        "select" => "Edit",
        "view" | "sidebar" => "View",
        "pane" => "Panes",
        "settings" | "app" | "palette" => "App",
        _ => "Tools",
    }
}

/// Display order of the categories on the Keys page.
pub const CATEGORIES: &[&str] = &["File", "Edit", "Navigate", "Tabs", "Panes", "View", "App", "Tools"];

#[cfg(test)]
mod tests {
    use super::*;
    use commands::*;

    fn ov(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect()
    }

    #[test]
    fn chords_parse_and_format_round_trip() {
        for text in ["Ctrl+T", "Ctrl+Shift+P", "Alt+Left", "F5", "Ctrl+Shift+\\", "Ctrl+,", "Ctrl+Alt+Del", "Shift+F10", "Ctrl+Enter", "Backspace", "Ctrl+Shift+Alt+F12", "Ctrl+Space", "Home", "PageDown", "Ctrl++"] {
            let c = parse_chord(text).unwrap_or_else(|| panic!("parse {text}"));
            let back = chord_text(c);
            assert_eq!(parse_chord(&back), Some(c), "{text} -> {back}");
        }
        assert_eq!(chord_text(parse_chord("ctrl+shift+t").unwrap()), "Ctrl+Shift+T");
        assert_eq!(chord_text(parse_chord("alt+ctrl+k").unwrap()), "Ctrl+Alt+K");
        assert_eq!(parse_chord("Control+Del").unwrap(), Chord::new(Key::Delete, Mods::CTRL));
    }

    #[test]
    fn nonsense_does_not_parse() {
        for bad in ["", "Ctrl+", "Hyper+T", "Ctrl+F25", "Ctrl+Ab", "Ctrl+Shift", "F0"] {
            assert!(parse_chord(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn every_default_chord_round_trips() {
        for cmd in BUILTIN {
            for t in cmd.keys.iter() {
                let c = parse_chord(t).unwrap_or_else(|| panic!("{} {t}", cmd.name));
                assert_eq!(parse_chord(&chord_text(c)), Some(c), "{}", cmd.name);
            }
        }
    }

    #[test]
    fn typing_keys_cannot_be_bound() {
        assert_eq!(bindable(Chord::new(Key::Char('A'), Mods::NONE)), Err(Reject::NeedsModifier));
        assert_eq!(bindable(Chord::new(Key::Char('A'), Mods::SHIFT)), Err(Reject::NeedsModifier));
        assert_eq!(bindable(Chord::new(Key::Space, Mods::NONE)), Err(Reject::NeedsModifier));
        assert!(bindable(Chord::new(Key::Char('A'), Mods::CTRL)).is_ok());
        assert!(bindable(Chord::new(Key::F(2), Mods::NONE)).is_ok());
        assert!(bindable(Chord::new(Key::Delete, Mods::NONE)).is_ok());
        assert_eq!(bindable(Chord::new(Key::Escape, Mods::NONE)), Err(Reject::Reserved));
        assert_eq!(bindable(Chord::new(Key::F(4), Mods::ALT)), Err(Reject::Windows));
    }

    #[test]
    fn defaults_match_the_table_and_have_no_conflicts() {
        let m = KeyMap::defaults();
        assert_eq!(m.changed(), 0);
        assert_eq!(m.label(CMD_SIDEBAR), "Ctrl+B");
        assert_eq!(m.label(CMD_TAB_TO_OTHER), "");
        assert_eq!(m.lookup(Chord::new(Key::F(2), Mods::NONE)), Some(CMD_RENAME));
        for cmd in BUILTIN {
            for c in m.chords(cmd.id) {
                assert_eq!(m.conflict(*c, cmd.id), None, "{} {}", cmd.name, chord_text(*c));
            }
        }
    }

    #[test]
    fn overrides_apply_and_to_overrides_round_trips() {
        let o = ov(&[("tab.new", &["Ctrl+N", "Ctrl+Alt+T"]), ("tab.close", &[])]);
        let (m, warn) = KeyMap::from_overrides(&o);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(m.chords(CMD_NEW_TAB), &[Chord::new(Key::Char('N'), Mods::CTRL), Chord::new(Key::Char('T'), Mods { ctrl: true, shift: false, alt: true })]);
        assert!(m.chords(CMD_CLOSE_TAB).is_empty(), "empty list = unbound");
        assert_eq!(m.label(CMD_NEXT_TAB), "Ctrl+Tab", "others keep their defaults");
        assert_eq!(m.changed(), 2);
        assert_eq!(m.to_overrides(&BTreeMap::new()), o);
        // nothing changed: nothing written
        assert!(KeyMap::defaults().to_overrides(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_default_chord_can_move_to_another_command() {
        // Ctrl+T leaves "new tab" for "close tab": no conflict, the old owner is emptied by its own override
        let o = ov(&[("tab.new", &["Ctrl+N"]), ("tab.close", &["Ctrl+T"])]);
        let (m, warn) = KeyMap::from_overrides(&o);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(m.label(CMD_CLOSE_TAB), "Ctrl+T");
        assert_eq!(m.label(CMD_NEW_TAB), "Ctrl+N");
    }

    #[test]
    fn a_chord_taken_from_a_command_that_has_no_override_is_dropped_there() {
        let (m, warn) = KeyMap::from_overrides(&ov(&[("tab.close", &["Ctrl+T"])]));
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(m.label(CMD_CLOSE_TAB), "Ctrl+T");
        assert!(m.chords(CMD_NEW_TAB).is_empty(), "new tab lost its chord to close tab");
        for cmd in BUILTIN {
            for c in m.chords(cmd.id) {
                assert_eq!(m.conflict(*c, cmd.id), None);
            }
        }
    }

    #[test]
    fn bad_overrides_are_skipped_with_a_warning_and_never_break_loading() {
        let o = ov(&[("tab.new", &["Ctrl+Nonsense", "A"]), ("no.such", &["Ctrl+1"]), ("tab.next", &["Ctrl+B"])]);
        let (m, warn) = KeyMap::from_overrides(&o);
        assert_eq!(warn.len(), 3, "{warn:?}");
        assert!(m.chords(CMD_NEW_TAB).is_empty());
        // an override beats another command's default: the sidebar loses Ctrl+B
        assert_eq!(m.label(CMD_NEXT_TAB), "Ctrl+B");
        assert!(m.chords(CMD_SIDEBAR).is_empty());
        // unknown names survive a save
        assert!(m.to_overrides(&o).contains_key("no.such"));
    }

    #[test]
    fn assign_takes_the_chord_from_its_owner_and_reset_gives_defaults_back() {
        let mut m = KeyMap::defaults();
        let c = Chord::new(Key::Char('B'), Mods::CTRL);
        // an unrelated command takes Ctrl+B from the sidebar toggle
        assert_eq!(m.conflict(c, CMD_REFRESH), Some(Conflict { command: CMD_SIDEBAR }));
        let lost = m.assign(CMD_REFRESH, 0, c).unwrap();
        assert_eq!(lost, Some(CMD_SIDEBAR));
        assert!(m.chords(CMD_SIDEBAR).is_empty());
        assert_eq!(m.label(CMD_REFRESH), "Ctrl+B");
        assert_eq!(m.changed(), 2);
        // resetting refresh brings Ctrl+R back, but Ctrl+B stays unowned by it
        m.reset(CMD_REFRESH);
        assert_eq!(m.label(CMD_REFRESH), "Ctrl+R");
        m.reset(CMD_SIDEBAR);
        assert_eq!(m.label(CMD_SIDEBAR), "Ctrl+B");
        assert_eq!(m.changed(), 0);
    }

    #[test]
    fn slots_replace_append_and_clear() {
        let mut m = KeyMap::defaults();
        let ctrl = |ch| Chord::new(Key::Char(ch), Mods::CTRL);
        m.assign(CMD_NEW_TAB, 1, ctrl('N')).unwrap();
        assert_eq!(m.chords(CMD_NEW_TAB), &[ctrl('T'), ctrl('N')]);
        m.assign(CMD_NEW_TAB, 0, ctrl('M')).unwrap();
        assert_eq!(m.chords(CMD_NEW_TAB), &[ctrl('M'), ctrl('N')]);
        m.assign(CMD_NEW_TAB, 5, ctrl('Q')).unwrap();
        assert_eq!(m.chords(CMD_NEW_TAB).len(), 2, "never more than two");
        m.clear_slot(CMD_NEW_TAB, 0);
        assert_eq!(m.chords(CMD_NEW_TAB).len(), 1);
        assert!(m.assign(CMD_NEW_TAB, 0, Chord::new(Key::Char('Z'), Mods::NONE)).is_err());
        m.reset_all();
        assert_eq!(m, KeyMap::defaults());
    }

    #[test]
    fn bind_all_fills_the_kit_table_and_a_rebind_is_live() {
        let mut ui = Ui::default();
        let mut m = KeyMap::defaults();
        m.bind_all(&mut ui);
        assert_eq!(ui.shortcuts.lookup(Key::Char('T'), Mods::CTRL, true), Some(CMD_NEW_TAB));
        assert_eq!(ui.shortcuts.lookup(Key::F(2), Mods::NONE, false), Some(CMD_RENAME));
        m.assign(CMD_NEW_TAB, 0, Chord::new(Key::Char('N'), Mods::CTRL)).unwrap();
        m.bind_all(&mut ui);
        assert_eq!(ui.shortcuts.lookup(Key::Char('T'), Mods::CTRL, true), None, "the old chord is gone");
        assert_eq!(ui.shortcuts.lookup(Key::Char('N'), Mods::CTRL, true), Some(CMD_NEW_TAB));
    }

    #[test]
    fn categories_cover_every_command() {
        for cmd in BUILTIN {
            assert!(CATEGORIES.contains(&category(&cmd.name)), "{}", cmd.name);
        }
    }

    /// A registry with one extension switched off: its commands are not in
    /// the map, their keys are free, and their overrides survive a save.
    #[test]
    fn a_switched_off_extension_s_commands_leave_the_map() {
        use tsubame_ext_api::{CmdFlags, CommandSpec, Contributions, Extension, ExtensionInfo, builtin_command};
        struct Extra;
        static EXTRA: &[CommandSpec] = &[builtin_command!(9000, "extra.tool", "Extra tool", ["Ctrl+Alt+E"], CmdFlags::GLOBAL)];
        impl Extension<crate::ext::App> for Extra {
            fn info(&self) -> ExtensionInfo {
                ExtensionInfo::builtin("test.extra", "Extra", false)
            }
            fn contribute(&self, c: &mut Contributions<crate::ext::App>) {
                c.command(EXTRA[0].clone(), crate::view::View::run_builtin);
            }
        }
        let mut r = crate::ext::builtin();
        r.add(&Extra).unwrap();
        let on = KeyMap::defaults_for(Arc::new(CmdList::from_registry(&r)));
        assert_eq!(on.label(9000), "Ctrl+Alt+E");
        assert_eq!(on.commands().len(), builtin_count() + 1);
        r.set_enabled("test.extra", false).unwrap();
        let list = Arc::new(CmdList::from_registry(&r));
        let o = ov(&[("extra.tool", &["Ctrl+Alt+X"])]);
        let (off, _) = KeyMap::from_overrides_for(list, &o);
        assert_eq!(off.commands().len(), builtin_count());
        assert_eq!(off.label(9000), "");
        assert_eq!(off.lookup(Chord::new(Key::Char('E'), Mods { ctrl: true, shift: false, alt: true })), None);
        assert!(off.to_overrides(&o).contains_key("extra.tool"), "kept for when it is back on");
    }
}
