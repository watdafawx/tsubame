//! The command palette (Ctrl+Shift+P, also Ctrl+K): one input, four modes picked by a
//! first character (`>` commands, `/` go to path, `#` filter this folder, `@` open tabs).
//! Fuzzy over command names, the user's current shortcut on the right, recently used
//! commands first on an empty query. A popup: no scrim, Esc or an outside click closes it,
//! arrows + Enter, the mouse works. It exists only while open (`View.palette` is `None`
//! otherwise) and wakes nothing while idle.
//!
//! Split in two: pure row building (`command_rows`, `path_rows`, `tab_rows`, `parse_mode`,
//! all unit-tested) and the painting/input in `Palette::paint`, which only returns an
//! [`Action`] for `View` to run.

use crate::fuzzy;
use tsubame_ui::{Canvas, Event, Font, Id, Key, Rect, Sense, TextInput, Theme, Ui, f, keycap, keycaps_right};

pub const ID_PALETTE: Id = Id::new("palette");
const ID_INPUT: Id = Id::new("palette.input");
const ID_ROW: Id = Id::new("palette.row");
const ID_CHIP: Id = Id::new("palette.chip");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Commands,
    Path,
    Filter,
    Tabs,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Commands, Mode::Path, Mode::Filter, Mode::Tabs];
    pub fn glyph(self) -> char {
        match self {
            Mode::Commands => '>',
            Mode::Path => '/',
            Mode::Filter => '#',
            Mode::Tabs => '@',
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Commands => "Commands",
            Mode::Path => "Go to path",
            Mode::Filter => "Filter folder",
            Mode::Tabs => "Open tabs",
        }
    }
}

/// `Some(mode)` when `c` is a mode prefix.
pub fn mode_for_prefix(c: char) -> Option<Mode> {
    Mode::ALL.into_iter().find(|m| m.glyph() == c)
}

/// "> rn" style text (prefix first) to a mode and the rest. No prefix: commands.
#[cfg(test)]
pub fn parse_mode(text: &str) -> (Mode, &str) {
    let t = text.trim_start();
    match t.chars().next().and_then(mode_for_prefix) {
        Some(m) => (m, t[1..].trim_start()),
        None => (Mode::Commands, t),
    }
}

// ---- data the view hands over -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct TabRow {
    pub pane: usize,
    pub index: usize,
    pub title: String,
    pub path: String,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchRow {
    pub name: String,
    pub kind: String,
    pub size: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    /// command ids, most recently used first
    pub recents: Vec<u32>,
    pub tabs: Vec<TabRow>,
    /// (path, "yesterday")
    pub recent_folders: Vec<(String, String)>,
    pub folder_name: String,
    /// the first few entries of the folder that match the live filter
    pub matches: Vec<MatchRow>,
    pub match_count: usize,
    pub total: usize,
}

// ---- rows -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Section { title: String, count: Option<usize> },
    Cmd { id: u32, title: &'static str, cat: &'static str, keys: String, hl: Vec<usize> },
    Folder { path: String, hint: String, hl: Vec<usize>, recent: bool },
    Tab { t: TabRow, num: usize, hl: Vec<usize> },
    /// the "Filter <folder> by “q”" line
    FilterHeader { folder: String, query: String, shown: usize, total: usize },
    Match { m: MatchRow, hl: Vec<usize> },
}

impl Row {
    pub fn selectable(&self) -> bool {
        !matches!(self, Row::Section { .. })
    }
}

/// Commands matching `query`. Empty query: the recent ones, then every other command.
pub fn command_rows(query: &str, recents: &[u32], keys: &dyn Fn(u32) -> String) -> Vec<Row> {
    let all: Vec<&'static crate::commands::Command> = crate::commands::COMMANDS.iter().filter(|c| c.id != crate::commands::CMD_PALETTE).collect();
    let mk = |c: &'static crate::commands::Command, hl: Vec<usize>| Row::Cmd { id: c.id, title: c.title, cat: crate::keymap::category(c.name), keys: keys(c.id), hl };
    let mut out = Vec::new();
    if query.trim().is_empty() {
        let recent: Vec<&&crate::commands::Command> = recents.iter().filter_map(|id| all.iter().find(|c| c.id == *id)).take(5).collect();
        if !recent.is_empty() {
            out.push(Row::Section { title: "Recent".into(), count: None });
            for c in &recent {
                out.push(mk(c, Vec::new()));
            }
        }
        let rest: Vec<&&crate::commands::Command> = all.iter().filter(|c| !recent.iter().any(|r| r.id == c.id)).collect();
        out.push(Row::Section { title: "All commands".into(), count: Some(rest.len()) });
        for c in rest {
            out.push(mk(c, Vec::new()));
        }
        return out;
    }
    let ranked = fuzzy::rank(query, &all, |c| c.title);
    out.push(Row::Section { title: "Best matches".into(), count: Some(ranked.len()) });
    for (i, m) in ranked {
        out.push(mk(all[i], m.positions));
    }
    out
}

/// "C:\Users\Alex\D" -> ("C:\Users\Alex\", "D").
pub fn split_dir(text: &str) -> (&str, &str) {
    match text.rfind(['\\', '/']) {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => ("", text),
    }
}

/// "C:\Users\Alex\" -> "C:\Users\"; "C:\" stays.
pub fn path_up(text: &str) -> String {
    let t = text.trim_end_matches(['\\', '/']);
    match t.rfind(['\\', '/']) {
        Some(i) if i >= 2 || !t[..i].is_empty() => t[..=i].to_string(),
        Some(i) => t[..=i].to_string(),
        None if t.len() == 2 && t.ends_with(':') => format!("{t}\\"),
        None => String::new(),
    }
}

/// Folders of `dir` whose name matches `frag`; `dir_names` are the sub folder names.
pub fn path_rows(typed: &str, dir_names: &[String], recent: &[(String, String)]) -> Vec<Row> {
    let (dir, frag) = split_dir(typed);
    let mut out = Vec::new();
    if !dir.is_empty() {
        let items: Vec<&String> = dir_names.iter().collect();
        let ranked: Vec<(usize, fuzzy::Match)> = if frag.is_empty() { (0..items.len()).map(|i| (i, fuzzy::Match { score: 0, positions: vec![] })).collect() } else { fuzzy::rank(frag, &items, |s| s.as_str()) };
        out.push(Row::Section { title: format!("Folders in {dir}"), count: Some(ranked.len()) });
        let dir_chars = dir.chars().count();
        for (i, m) in ranked.into_iter().take(60) {
            out.push(Row::Folder { path: format!("{dir}{}", items[i]), hint: String::new(), hl: m.positions.iter().map(|p| p + dir_chars).collect(), recent: false });
        }
    }
    let rec: Vec<&(String, String)> = recent.iter().filter(|(p, _)| typed.is_empty() || fuzzy::fuzzy(typed, p).is_some()).take(6).collect();
    if !rec.is_empty() {
        out.push(Row::Section { title: "Recent folders".into(), count: None });
        for (p, ago) in rec {
            out.push(Row::Folder { path: p.clone(), hint: ago.clone(), hl: Vec::new(), recent: true });
        }
    }
    out
}

/// Open tabs matching `query` (title or path), numbered like Alt+N.
pub fn tab_rows(query: &str, tabs: &[TabRow]) -> Vec<Row> {
    let mut out = vec![Row::Section { title: "Open tabs".into(), count: Some(tabs.len()) }];
    let panes: std::collections::BTreeSet<usize> = tabs.iter().map(|t| t.pane).collect();
    if let Row::Section { title, .. } = &mut out[0] {
        *title = format!("Open tabs  {}, across {} pane{}", tabs.len(), panes.len().max(1), if panes.len() > 1 { "s" } else { "" });
    }
    if query.trim().is_empty() {
        for (i, t) in tabs.iter().enumerate() {
            out.push(Row::Tab { t: t.clone(), num: i + 1, hl: Vec::new() });
        }
    } else {
        let hay: Vec<String> = tabs.iter().map(|t| format!("{} {}", t.title, t.path)).collect();
        for (i, m) in fuzzy::rank(query, &hay, |s| s.as_str()) {
            let hl = m.positions.into_iter().filter(|p| *p < tabs[i].title.chars().count()).collect();
            out.push(Row::Tab { t: tabs[i].clone(), num: i + 1, hl });
        }
    }
    out
}

pub fn filter_rows(query: &str, d: &Data) -> Vec<Row> {
    let mut out = vec![Row::FilterHeader { folder: d.folder_name.clone(), query: query.to_string(), shown: d.match_count, total: d.total }];
    if !d.matches.is_empty() {
        out.push(Row::Section { title: "Matches".into(), count: None });
        for m in &d.matches {
            let hl = fuzzy::fuzzy(query, &m.name).map(|x| x.positions).unwrap_or_default();
            out.push(Row::Match { m: m.clone(), hl });
        }
    }
    out
}

// ---- state ------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    Close,
    /// run a command; `keep`: leave the palette open (Ctrl+Enter)
    Run { id: u32, keep: bool },
    /// the "Show shortcut" key: open Settings > Keys for the command
    ShowShortcut(u32),
    Go { path: String, new_tab: bool },
    /// apply this text as the live filter of the folder
    FilterLive(String),
    /// keep the filter and close (Enter); `open`: open the selected match (Shift+Enter)
    FilterKeep { open: Option<String> },
    /// Esc in filter mode: drop the filter
    FilterCancel,
    SwitchTab { pane: usize, index: usize },
    MoveTab { pane: usize, index: usize },
    CloseTab { pane: usize, index: usize },
}

pub struct Palette {
    pub mode: Mode,
    pub input: TextInput,
    pub data: Data,
    rows: Vec<Row>,
    sel: usize,
    scroll: f32,
    /// the rows must be rebuilt (text/mode/data changed)
    dirty: bool,
    last_text: String,
    last_mode: Mode,
    dir_cache: Option<(String, Vec<String>)>,
    last_mouse: tsubame_ui::Pt,
    pub keys: Box<dyn Fn(u32) -> String>,
    /// the opened-first frame: take the focus
    fresh: bool,
}

impl Palette {
    pub fn open(mode: Mode, data: Data, keys: Box<dyn Fn(u32) -> String>) -> Palette {
        Palette { mode, input: TextInput::default(), data, rows: Vec::new(), sel: 0, scroll: 0.0, dirty: true, last_text: String::new(), last_mode: mode, dir_cache: None, last_mouse: tsubame_ui::Pt::new(-1.0, -1.0), keys, fresh: true }
    }

    pub fn set_data(&mut self, d: Data) {
        self.data = d;
        self.dirty = true;
    }

    fn rebuild(&mut self) {
        let q = self.input.text.clone();
        self.rows = match self.mode {
            Mode::Commands => command_rows(&q, &self.data.recents, &*self.keys),
            Mode::Path => {
                let (dir, _) = split_dir(&q);
                let dir = dir.to_string();
                if !dir.is_empty() && self.dir_cache.as_ref().is_none_or(|(d, _)| *d != dir) {
                    self.dir_cache = Some((dir.clone(), list_folders(&dir)));
                }
                let names: &[String] = self.dir_cache.as_ref().filter(|(d, _)| *d == dir).map(|(_, n)| n.as_slice()).unwrap_or(&[]);
                path_rows(&q, names, &self.data.recent_folders)
            }
            Mode::Filter => filter_rows(&q, &self.data),
            Mode::Tabs => tab_rows(&q, &self.data.tabs),
        };
        self.sel = self.rows.iter().position(Row::selectable).unwrap_or(0);
        self.scroll = 0.0;
        self.dirty = false;
    }

    fn step(&mut self, down: bool, by: usize) {
        for _ in 0..by.max(1) {
            let mut i = self.sel;
            loop {
                let n = if down { i + 1 } else { i.wrapping_sub(1) };
                if n >= self.rows.len() {
                    break;
                }
                i = n;
                if self.rows[i].selectable() {
                    self.sel = i;
                    break;
                }
            }
        }
    }

    fn row_h(&self, r: &Row, t: &Theme) -> f32 {
        match r {
            Row::Section { .. } => t.height_palette_section,
            _ => t.height_palette_item,
        }
    }

    fn content_h(&self, t: &Theme) -> f32 {
        self.rows.iter().map(|r| self.row_h(r, t)).sum()
    }

    fn y_of(&self, i: usize, t: &Theme) -> f32 {
        self.rows[..i.min(self.rows.len())].iter().map(|r| self.row_h(r, t)).sum()
    }

    fn switch_mode(&mut self, m: Mode) {
        self.mode = m;
        self.input.set_text("");
        self.dirty = true;
        self.dir_cache = None;
    }

    /// Mode prefix typed into an empty field, Backspace on an empty field: handled before the
    /// text field sees the key.
    fn prefix_keys(&mut self, ui: &mut Ui) -> Option<Action> {
        for (qi, ev) in ui.key_input(ID_INPUT) {
            match ev {
                Event::Char(c) if self.input.text.is_empty() && mode_for_prefix(c).is_some() && !c.is_control() => {
                    ui.consume(qi);
                    let m = mode_for_prefix(c).unwrap();
                    if m != self.mode {
                        self.switch_mode(m);
                    }
                    ui.request_repaint();
                    return Some(if m == Mode::Filter { Action::FilterLive(String::new()) } else { Action::None });
                }
                Event::KeyDown { key: Key::Backspace, mods, .. } if mods.none() => {
                    if self.input.text.is_empty() && !self.input.has_selection() {
                        ui.consume(qi);
                        if self.mode != Mode::Commands {
                            let was_filter = self.mode == Mode::Filter;
                            self.switch_mode(Mode::Commands);
                            ui.request_repaint();
                            return Some(if was_filter { Action::FilterCancel } else { Action::None });
                        }
                    } else if self.mode == Mode::Path && self.input.text.ends_with(['\\', '/']) && self.input.caret == self.input.text.len() && !self.input.has_selection() {
                        ui.consume(qi);
                        let up = path_up(&self.input.text);
                        self.input.set_text(&up);
                        self.dirty = true;
                        ui.request_repaint();
                        return Some(Action::None);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Painted as a popup. Returns what the view should do.
    pub fn paint(&mut self, ui: &mut Ui, g: &mut dyn Canvas, area: Rect) -> Action {
        let t = ui.theme;
        if self.fresh {
            self.fresh = false;
            ui.open_popup(ID_PALETTE);
            ui.set_focus(Some(ID_INPUT));
        }
        if !ui.is_popup_open(ID_PALETTE) {
            ui.capture_keys = false;
            return Action::Close;
        }
        ui.capture_keys = true;
        let mut action = Action::None;
        // keys that act on the list, before the input sees them
        let mut run: Option<(bool, bool)> = None; // (keep open, new tab)
        let mut tab_key = false;
        let mut delete_key = false;
        let mut esc = false;
        // the chord that opened the palette also shuts it (the keys are captured here)
        let mut toggle = false;
        let mut alt_num: Option<usize> = None;
        if let Some(a) = self.prefix_keys(ui) {
            action = a;
        }
        for (qi, ev) in ui.key_input(ID_INPUT) {
            if let Event::KeyDown { key, mods, .. } = ev {
                let mut take = true;
                match key {
                    Key::Down if mods.none() => self.step(true, 1),
                    Key::Up if mods.none() => self.step(false, 1),
                    Key::PageDown => self.step(true, 8),
                    Key::PageUp => self.step(false, 8),
                    Key::Enter => run = Some((mods.ctrl, mods.shift)),
                    Key::Tab if mods.none() => tab_key = true,
                    Key::Delete if mods.none() && self.mode == Mode::Tabs => delete_key = true,
                    Key::Escape if mods.none() => esc = true,
                    Key::Char('P') if mods.ctrl && mods.shift && !mods.alt => toggle = true,
                    Key::Char('K') if mods.ctrl && !mods.shift && !mods.alt => toggle = true,
                    Key::Char(c) if mods.alt && !mods.ctrl && c.is_ascii_digit() && self.mode == Mode::Tabs => alt_num = c.to_digit(10).map(|d| d as usize),
                    _ => take = false,
                }
                if take {
                    ui.consume(qi);
                }
            }
        }
        // the text field
        let before = self.input.text.clone();
        let before_mode = self.mode;
        let panel = panel_rect(area, &t, self.content_h(&t));
        let input_r = Rect::new(panel.x, panel.y, panel.w, t.height_palette_input);
        let mode_r = Rect::new(panel.x, input_r.b(), panel.w, t.height_palette_mode);
        let footer_r = Rect::new(panel.x, panel.b() - t.height_palette_footer, panel.w, t.height_palette_footer);
        let list_r = Rect::new(panel.x, mode_r.b() + 1.0, panel.w, (footer_r.y - mode_r.b() - 1.0).max(0.0));
        if ui.popup_begin(g, ID_PALETTE, panel) {
            self.paint_input(ui, g, input_r, &t);
            let chip = self.paint_modes(ui, g, mode_r, &t);
            g.fill(Rect::new(panel.x, mode_r.b(), panel.w, 1.0), t.border);
            if self.input.text != before {
                self.dirty = true;
            }
            if self.dirty || self.mode != self.last_mode || self.input.text != self.last_text {
                self.rebuild();
                self.last_text = self.input.text.clone();
                self.last_mode = self.mode;
                if self.mode == Mode::Filter && (self.input.text != before || before_mode != Mode::Filter) {
                    action = Action::FilterLive(self.input.text.clone());
                }
            }
            if let Some(m) = chip {
                self.switch_mode(m);
                ui.set_focus(Some(ID_INPUT));
                ui.request_repaint();
                if m == Mode::Filter {
                    action = Action::FilterLive(String::new());
                }
            }
            let clicked = self.paint_rows(ui, g, list_r, &t);
            self.paint_footer(g, footer_r, &t);
            ui.popup_end(g);
            if let Some((i, new_tab_or_keep)) = clicked {
                self.sel = i;
                run = Some((new_tab_or_keep, false));
            }
        }
        // what the keys mean in this mode
        if esc || toggle {
            ui.capture_keys = false;
            ui.close_popup(ID_PALETTE);
            return if esc && self.mode == Mode::Filter { Action::FilterCancel } else { Action::Close };
        }
        if let Some(n) = alt_num
            && let Some(Row::Tab { t, .. }) = self.rows.iter().filter(|r| matches!(r, Row::Tab { .. })).nth(n.saturating_sub(1))
        {
            action = Action::SwitchTab { pane: t.pane, index: t.index };
        }
        if delete_key
            && let Some(Row::Tab { t, .. }) = self.rows.get(self.sel)
        {
            action = Action::CloseTab { pane: t.pane, index: t.index };
            // the row disappears: rebuild with the data the view hands back
            self.dirty = true;
        }
        if tab_key {
            match self.rows.get(self.sel) {
                Some(Row::Folder { path, .. }) if self.mode == Mode::Path => {
                    let p = format!("{}\\", path.trim_end_matches('\\'));
                    self.input.set_text(&p);
                    self.input.caret = self.input.text.len();
                    self.input.anchor = self.input.caret;
                    self.dirty = true;
                }
                Some(Row::Cmd { id, .. }) => action = Action::ShowShortcut(*id),
                _ => {}
            }
        }
        if let Some((ctrl, shift)) = run {
            action = self.activate(ctrl, shift);
        }
        if !matches!(action, Action::None) || self.dirty {
            ui.request_repaint();
        }
        action
    }

    fn activate(&mut self, ctrl: bool, shift: bool) -> Action {
        let row = self.rows.get(self.sel).cloned();
        match (self.mode, row) {
            (Mode::Commands, Some(Row::Cmd { id, .. })) => Action::Run { id, keep: ctrl },
            (Mode::Path, Some(Row::Folder { path, .. })) => Action::Go { path, new_tab: ctrl },
            (Mode::Path, _) if !self.input.text.trim().is_empty() => Action::Go { path: self.input.text.trim().to_string(), new_tab: ctrl },
            (Mode::Filter, Some(Row::Match { m, .. })) if shift => Action::FilterKeep { open: Some(m.name) },
            (Mode::Filter, _) => Action::FilterKeep { open: None },
            (Mode::Tabs, Some(Row::Tab { t, .. })) if ctrl => Action::MoveTab { pane: t.pane, index: t.index },
            (Mode::Tabs, Some(Row::Tab { t, .. })) => Action::SwitchTab { pane: t.pane, index: t.index },
            _ => Action::None,
        }
    }

    // ---- painting --------------------------------------------------------------------

    fn paint_input(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        let it = ui.interact(ID_INPUT, r, Sense::click().focusable().cursor(tsubame_ui::Cursor::IBeam));
        let _ = it;
        // icon + mode glyph
        let icon = match self.mode {
            Mode::Commands => "\u{E7C9}",
            Mode::Path => "\u{E8B7}",
            Mode::Filter => "\u{E721}",
            Mode::Tabs => "\u{E710}",
        };
        let ic = Rect::new(r.x + 14.0, r.y, 22.0, r.h);
        g.text(icon, ic, f(15.0).icon().c(), t.text_dim);
        let gl = Rect::new(r.x + 44.0, r.y, 14.0, r.h);
        let glyph = self.mode.glyph().to_string();
        g.text(&glyph, gl, f(t.font_size_md).m().c(), t.accent);
        // the text, own caret (no field chrome in the palette)
        let font = f(t.font_size_md);
        let tx = r.x + 64.0;
        let esc_w = 38.0;
        let inner = Rect::new(tx, r.y, r.w - (tx - r.x) - esc_w - 18.0, r.h);
        let mut clip = std::mem::replace(&mut ui.clipboard, Box::new(tsubame_ui::MemClipboard::default()));
        for (qi, ev) in ui.key_input(ID_INPUT) {
            match ev {
                Event::Char(ch) if !ch.is_control() => {
                    ui.consume(qi);
                    let mut b = [0u8; 4];
                    self.input.insert(ch.encode_utf8(&mut b));
                }
                Event::Char(_) => ui.consume(qi),
                Event::KeyDown { key, mods, .. } if !matches!(key, Key::Enter | Key::Escape | Key::Tab) => {
                    let (used, _) = self.input.key(key, mods, &mut *clip);
                    if used {
                        ui.consume(qi);
                    }
                }
                _ => {}
            }
        }
        ui.clipboard = clip;
        // mouse: click in the text places the caret
        if let Some(p) = it.pressed {
            let i = g.hit_index(&self.input.text, font, p.pos.x - tx);
            self.input.caret = i;
            self.input.anchor = i;
        }
        g.clip(inner);
        let text = self.input.text.clone();
        if self.input.has_selection() {
            let (a, b) = self.input.sel();
            let (xa, xb) = (g.caret_x(&text, font, a), g.caret_x(&text, font, b));
            g.fill(Rect::new(tx + xa, r.y + 10.0, xb - xa, r.h - 20.0), t.selected);
        }
        g.text(&text, Rect::new(tx, r.y, inner.w + 400.0, r.h), font, t.text);
        if ui.has_focus(ID_INPUT) && ui.window_focused() {
            let cx = tx + g.caret_x(&text, font, self.input.caret);
            ui.mark_text_focus();
            if ui.caret_on {
                g.fill(Rect::new(cx.round(), r.y + 12.0, 1.0, r.h - 24.0), t.text);
            }
            ui.set_ime_caret(Rect::new(cx, r.y + 10.0, 1.0, r.h - 20.0));
        }
        g.unclip();
        // Esc key cap
        let er = Rect::new(r.r() - esc_w - 14.0, r.y + (r.h - 20.0) / 2.0, esc_w, 20.0);
        g.round(er, t.radius_sm, t.field);
        g.round_stroke(er, t.radius_sm, t.popup_border, 1.0);
        g.text("Esc", er, t.font_mono().c(), t.text_dim);
    }

    /// Mode chips; returns the one that was clicked.
    fn paint_modes(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) -> Option<Mode> {
        let mut x = r.x + 8.0;
        let mut out = None;
        let font = t.font_small();
        for (i, m) in Mode::ALL.into_iter().enumerate() {
            let label = m.label();
            let gl = m.glyph().to_string();
            let w = g.measure(label, font) + g.measure(&gl, t.font_mono()) + 24.0;
            let cr = Rect::new(x, r.y + 3.0, w, r.h - 6.0);
            let it = ui.interact(ID_CHIP.with(i as u64), cr, Sense::click());
            let on = m == self.mode;
            if on {
                g.round(cr, t.radius_sm, t.selected);
            } else if it.hovered {
                g.round(cr, t.radius_sm, t.hover);
            }
            g.text(&gl, Rect::new(cr.x + 6.0, cr.y, 12.0, cr.h), t.font_mono().c(), if on { t.accent } else { t.text_dim });
            g.text(label, Rect::new(cr.x + 20.0, cr.y, w, cr.h), if on { font.b() } else { font }, if on { t.text } else { t.text_dim });
            if it.clicked.is_some() {
                out = Some(m);
            }
            x += w + 4.0;
        }
        let hint = "Type a prefix, or Backspace on an empty field to switch";
        let hw = g.measure(hint, font);
        if x + hw + 16.0 < r.r() {
            g.text(hint, Rect::new(r.r() - hw - 10.0, r.y, hw + 4.0, r.h), font, t.text_dim);
        }
        out
    }

    /// Returns (row index, ctrl held) of a row clicked.
    fn paint_rows(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) -> Option<(usize, bool)> {
        let content = self.content_h(t);
        let max = (content - r.h).max(0.0);
        // wheel over the list
        let wi = ui.interact(ID_ROW, r, Sense::hover().wheel());
        if wi.wheel.1 != 0.0 {
            self.scroll = (self.scroll - wi.wheel.1 * t.height_palette_item * t.wheel_lines()).clamp(0.0, max);
        }
        // keep the selection visible
        let sy = self.y_of(self.sel, t);
        let sh = self.rows.get(self.sel).map(|row| self.row_h(row, t)).unwrap_or(0.0);
        if sy < self.scroll {
            self.scroll = sy.max(0.0);
            // show the section header above the first row
            if self.sel > 0 && matches!(self.rows[self.sel - 1], Row::Section { .. }) {
                self.scroll = self.y_of(self.sel - 1, t);
            }
        } else if sy + sh > self.scroll + r.h {
            self.scroll = sy + sh - r.h;
        }
        self.scroll = self.scroll.clamp(0.0, max);
        g.clip(r);
        let mut y = r.y - self.scroll;
        let mut clicked = None;
        let right = r.r() - 12.0;
        for i in 0..self.rows.len() {
            let h = self.row_h(&self.rows[i], t);
            let rr = Rect::new(r.x, y, r.w, h);
            y += h;
            if rr.b() < r.y || rr.y > r.b() {
                continue;
            }
            match &self.rows[i] {
                Row::Section { title, count } => {
                    g.text(title, Rect::new(r.x + 12.0, rr.y, r.w, h), t.font_small().b(), t.text_dim);
                    if let Some(n) = count {
                        let w = g.measure(title, t.font_small().b());
                        g.text(&n.to_string(), Rect::new(r.x + 12.0 + w + 8.0, rr.y, 60.0, h), t.font_small(), t.text_disabled);
                    }
                }
                row => {
                    let rr_hit = Rect::new(rr.x, rr.y.max(r.y), rr.w - 10.0, rr.h.min(rr.b() - r.y).min(r.b() - rr.y));
                    let it = ui.interact(ID_ROW.with(i as u64 + 1), rr_hit, Sense::click());
                    if it.hovered && ui.mouse() != self.last_mouse {
                        self.sel = i;
                    }
                    if it.clicked.is_some() {
                        clicked = Some((i, it.click_mods.ctrl));
                    }
                    let selected = i == self.sel;
                    if selected {
                        g.fill(rr, t.selected);
                        g.fill(Rect::new(rr.x, rr.y, 2.0, rr.h), t.accent);
                    }
                    let row = row.clone();
                    paint_row(g, t, &row, rr, right);
                }
            }
        }
        g.unclip();
        self.last_mouse = ui.mouse();
        // thin scroll thumb
        if content > r.h {
            let th = (r.h * r.h / content).clamp(24.0, r.h);
            let ty = r.y + if max > 0.0 { self.scroll / max } else { 0.0 } * (r.h - th);
            g.round(Rect::new(r.r() - 8.0, ty, 4.0, th), 2.0, t.scrollbar);
        }
        clicked
    }

    fn paint_footer(&self, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        let hints: &[(&[&str], &str)] = match self.mode {
            Mode::Commands => &[(&["\u{2191}", "\u{2193}"], "Move"), (&["Enter"], "Run"), (&["Ctrl", "Enter"], "Run, keep open"), (&["Tab"], "Show shortcut")],
            Mode::Path => &[(&["\u{2191}", "\u{2193}"], "Move"), (&["Tab"], "Complete"), (&["Enter"], "Go"), (&["Ctrl", "Enter"], "Open in new tab"), (&["Backspace"], "Up one level")],
            Mode::Filter => &[(&["Enter"], "Keep filter in folder"), (&["\u{2191}", "\u{2193}"], "Select a match"), (&["Shift", "Enter"], "Open selected"), (&["Esc"], "Cancel, clear filter")],
            Mode::Tabs => &[(&["\u{2191}", "\u{2193}"], "Move"), (&["Enter"], "Switch to tab"), (&["Ctrl", "Enter"], "Move tab here"), (&["Delete"], "Close tab")],
        };
        let font = t.font_small();
        let cy = r.y + r.h / 2.0 + 0.5;
        let mut x = r.x + 12.0;
        for (caps, label) in hints {
            for (k, cap) in caps.iter().enumerate() {
                if k > 0 {
                    x += 3.0;
                }
                x = keycap(g, t, x, cy, cap);
            }
            x += 6.0;
            let w = g.measure(label, font);
            if x + w > r.r() - 8.0 {
                break;
            }
            g.text(label, Rect::new(x, r.y, w + 4.0, r.h), font, t.text_dim);
            x += w + 18.0;
        }
    }
}

fn panel_rect(area: Rect, t: &Theme, content_h: f32) -> Rect {
    let w = t.layout_palette_width.min(area.w - 24.0).max(320.0);
    let fixed = t.height_palette_input + t.height_palette_mode + 1.0 + t.height_palette_footer;
    let max_h = (area.h - t.layout_palette_top - 24.0).max(fixed + 60.0);
    let want = (fixed + content_h).max(fixed + 120.0);
    let h = want.min(max_h).min(fixed + 11.0 * t.height_palette_item + 40.0);
    Rect::new(area.x + (area.w - w) / 2.0, area.y + t.layout_palette_top, w, h)
}

fn byte_of(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// `s` with the matched chars on a highlight; `x` is the text origin.
fn hl_text(g: &mut dyn Canvas, t: &Theme, s: &str, r: Rect, font: Font, color: u32, hl: &[usize]) {
    if !hl.is_empty() {
        for &p in hl {
            let (a, b) = (byte_of(s, p), byte_of(s, p + 1));
            if a >= b {
                continue;
            }
            let (xa, xb) = (g.caret_x(s, font, a), g.caret_x(s, font, b));
            g.fill_a(Rect::new(r.x + xa, r.y + 6.0, xb - xa, r.h - 12.0), t.highlight, 0.55);
        }
    }
    g.text(s, r, font, color);
}

fn paint_row(g: &mut dyn Canvas, t: &Theme, row: &Row, rr: Rect, right: f32) {
    let font = f(t.font_size_md);
    let small = t.font_small();
    let text_x = rr.x + 36.0;
    match row {
        Row::Cmd { title, cat, keys, hl, .. } => {
            hl_text(g, t, title, Rect::new(text_x, rr.y, 360.0, rr.h), font, t.text, hl);
            let cap_left = if keys.is_empty() {
                let w = g.measure("unbound", small) + 12.0;
                let ur = Rect::new(right - w, rr.y + (rr.h - 18.0) / 2.0, w, 18.0);
                g.round(ur, t.radius_sm, t.field);
                g.text("unbound", ur, small.c(), t.text_disabled);
                ur.x
            } else {
                keycaps_right(g, t, right, rr.y + rr.h / 2.0, keys)
            };
            // the category column sits at a fixed x so the rows line up
            let cx = (rr.x + 400.0).min(cap_left - 70.0);
            g.text(cat, Rect::new(cx, rr.y, 80.0, rr.h), small, t.text_dim);
        }
        Row::Folder { path, hint, hl, recent } => {
            g.text(if *recent { "\u{E823}" } else { "\u{E8B7}" }, Rect::new(rr.x + 12.0, rr.y, 20.0, rr.h), f(14.0).icon().c(), if *recent { t.text_dim } else { t.folder });
            hl_text(g, t, path, Rect::new(text_x, rr.y, rr.w - 140.0, rr.h), font, t.text, hl);
            if !hint.is_empty() {
                g.text(hint, Rect::new(right - 120.0, rr.y, 120.0, rr.h), small.r(), t.text_dim);
            }
        }
        Row::Tab { t: tab, num, hl } => {
            g.text("\u{E8B7}", Rect::new(rr.x + 12.0, rr.y, 20.0, rr.h), f(14.0).icon().c(), t.folder);
            let tw = g.measure(&tab.title, font);
            hl_text(g, t, &tab.title, Rect::new(text_x, rr.y, tw + 4.0, rr.h), font, t.text, hl);
            g.text(&tab.path, Rect::new(text_x + tw + 12.0, rr.y, 280.0, rr.h), small, t.text_dim);
            let cy = rr.y + rr.h / 2.0;
            let mut rx = right;
            if *num <= 9 {
                let l = keycaps_right(g, t, rx, cy, &format!("Alt+{num}"));
                rx = l - 8.0;
            }
            let (label, active) = if tab.current { ("Current".to_string(), true) } else { (format!("Pane {}", tab.pane + 1), false) };
            let w = g.measure(&label, small) + 14.0;
            let br = Rect::new(rx - w, cy - 9.0, w, 18.0);
            g.round_stroke(br, t.radius_sm, if active { t.accent } else { t.popup_border }, 1.0);
            g.text(&label, br, small.c(), if active { t.accent } else { t.text_dim });
        }
        Row::FilterHeader { folder, query, shown, total } => {
            g.text("#", Rect::new(rr.x + 12.0, rr.y, 20.0, rr.h), f(15.0).m().c(), t.text_dim);
            let pre = "Filter ";
            let pw = g.measure(pre, font);
            g.text(pre, Rect::new(text_x, rr.y, pw + 2.0, rr.h), font, t.text);
            let fw = g.measure(folder, font.b());
            g.text(folder, Rect::new(text_x + pw, rr.y, fw + 2.0, rr.h), font.b(), t.text);
            let rest = format!(" by \u{201C}{query}\u{201D}");
            g.text(&rest, Rect::new(text_x + pw + fw, rr.y, 300.0, rr.h), font, t.text);
            let right_text = format!("{shown} of {total} items match");
            g.text(&right_text, Rect::new(right - 220.0, rr.y, 220.0, rr.h), small.r(), t.text_dim);
        }
        Row::Match { m, hl } => {
            g.text(if m.is_dir { "\u{E8B7}" } else { "\u{E8A5}" }, Rect::new(rr.x + 12.0, rr.y, 20.0, rr.h), f(14.0).icon().c(), if m.is_dir { t.folder } else { t.text_dim });
            hl_text(g, t, &m.name, Rect::new(text_x, rr.y, 360.0, rr.h), font, t.text, hl);
            g.text(&m.size, Rect::new(right - 70.0, rr.y, 70.0, rr.h), small.r(), t.text_dim);
            g.text(&m.kind, Rect::new(right - 260.0, rr.y, 170.0, rr.h), small.r(), t.text_dim);
        }
        Row::Section { .. } => {}
    }
}

/// Sub folder names of `dir` (a quick sync read; the user typed this folder, so it is
/// normally a local, small listing). Capped so a huge folder cannot stall a key press.
fn list_folders(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten().take(4000) {
            if e.file_type().is_ok_and(|t| t.is_dir())
                && let Some(n) = e.file_name().to_str()
            {
                out.push(n.to_string());
            }
        }
    }
    out.sort_by_key(|n| n.to_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::*;

    fn keys(id: u32) -> String {
        first_key_label(id)
    }

    #[test]
    fn prefixes_pick_the_mode() {
        assert_eq!(parse_mode("rn"), (Mode::Commands, "rn"));
        assert_eq!(parse_mode("> rn"), (Mode::Commands, "rn"));
        assert_eq!(parse_mode("/C:\\Users"), (Mode::Path, "C:\\Users"));
        assert_eq!(parse_mode("# inv"), (Mode::Filter, "inv"));
        assert_eq!(parse_mode("@down"), (Mode::Tabs, "down"));
        assert_eq!(parse_mode(""), (Mode::Commands, ""));
        for m in Mode::ALL {
            assert_eq!(mode_for_prefix(m.glyph()), Some(m));
        }
        assert_eq!(mode_for_prefix('x'), None);
    }

    #[test]
    fn empty_query_lists_recents_first_then_everything_else() {
        let rows = command_rows("", &[CMD_SETTINGS, CMD_NEW_TAB], &keys);
        assert!(matches!(&rows[0], Row::Section { title, .. } if title == "Recent"));
        assert!(matches!(&rows[1], Row::Cmd { id, .. } if *id == CMD_SETTINGS));
        assert!(matches!(&rows[2], Row::Cmd { id, .. } if *id == CMD_NEW_TAB));
        let all = rows.iter().position(|r| matches!(r, Row::Section { title, .. } if title == "All commands")).unwrap();
        assert_eq!(all, 3);
        let n = rows.iter().filter(|r| matches!(r, Row::Cmd { .. })).count();
        // every command except the palette's own, once
        assert_eq!(n, COMMANDS.len() - 1);
        let mut ids: Vec<u32> = rows.iter().filter_map(|r| if let Row::Cmd { id, .. } = r { Some(*id) } else { None }).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "no command twice");
    }

    #[test]
    fn unknown_or_palette_recents_are_dropped() {
        let rows = command_rows("", &[9999, CMD_PALETTE], &keys);
        assert!(matches!(&rows[0], Row::Section { title, .. } if title == "All commands"), "no Recent section: {:?}", rows[0]);
    }

    #[test]
    fn typing_ranks_by_fuzzy_score_and_marks_the_letters() {
        let rows = command_rows("ren", &[], &keys);
        assert!(matches!(&rows[0], Row::Section { title, .. } if title == "Best matches"));
        match &rows[1] {
            Row::Cmd { id, hl, .. } => {
                assert_eq!(*id, CMD_RENAME);
                assert_eq!(hl, &vec![0, 1, 2]);
            }
            r => panic!("{r:?}"),
        }
        assert!(command_rows("zzzzqq", &[], &keys).iter().all(|r| !matches!(r, Row::Cmd { .. })));
    }

    #[test]
    fn rows_show_the_current_binding_and_unbound_is_empty() {
        let rows = command_rows("rename", &[], &keys);
        let Row::Cmd { keys: k, .. } = &rows[1] else { panic!() };
        assert_eq!(k, "F2");
        // a command without a default key shows no cap text
        let rows = command_rows("pin", &[], &|id| if id == CMD_PIN_FOLDER { String::new() } else { keys(id) });
        let Row::Cmd { keys: k, .. } = &rows[1] else { panic!() };
        assert!(k.is_empty());
    }

    #[test]
    fn path_helpers() {
        assert_eq!(split_dir("C:\\Users\\Alex\\D"), ("C:\\Users\\Alex\\", "D"));
        assert_eq!(split_dir("C:\\Users\\"), ("C:\\Users\\", ""));
        assert_eq!(split_dir("abc"), ("", "abc"));
        assert_eq!(path_up("C:\\Users\\Alex\\"), "C:\\Users\\");
        assert_eq!(path_up("C:\\Users\\"), "C:\\");
        assert_eq!(path_up("C:\\"), "C:\\");
        assert_eq!(path_up("C:"), "C:\\");
        assert_eq!(path_up("abc"), "");
    }

    #[test]
    fn path_rows_filter_and_offset_the_highlight() {
        let names: Vec<String> = ["Desktop", "Documents", "Downloads", "Dropbox", "Music"].iter().map(|s| s.to_string()).collect();
        let rows = path_rows("C:\\Users\\Alex\\D", &names, &[]);
        assert!(matches!(&rows[0], Row::Section { title, count } if title == "Folders in C:\\Users\\Alex\\" && *count == Some(4)));
        let Row::Folder { path, hl, .. } = &rows[1] else { panic!() };
        assert!(path.starts_with("C:\\Users\\Alex\\D"));
        assert_eq!(hl, &vec!["C:\\Users\\Alex\\".chars().count()]);
        // no fragment: every folder
        let rows = path_rows("C:\\Users\\Alex\\", &names, &[]);
        assert!(matches!(&rows[0], Row::Section { count, .. } if *count == Some(5)));
    }

    #[test]
    fn path_rows_add_recent_folders() {
        let rec = vec![("X:\\Dev\\tsubame\\mocks".to_string(), "yesterday".to_string())];
        let rows = path_rows("", &[], &rec);
        assert!(matches!(&rows[0], Row::Section { title, .. } if title == "Recent folders"));
        assert!(matches!(&rows[1], Row::Folder { recent: true, hint, .. } if hint == "yesterday"));
        // typed text narrows the recents
        assert!(path_rows("zzz", &[], &rec).is_empty());
        assert!(!path_rows("tsubame", &[], &rec).is_empty());
    }

    fn tabs() -> Vec<TabRow> {
        vec![
            TabRow { pane: 0, index: 0, title: "Downloads".into(), path: "C:\\Users\\Alex\\Downloads".into(), current: true },
            TabRow { pane: 0, index: 1, title: "tsubame".into(), path: "X:\\Dev\\tsubame".into(), current: false },
            TabRow { pane: 1, index: 0, title: "blink".into(), path: "X:\\Dev\\blink".into(), current: false },
        ]
    }

    #[test]
    fn tab_rows_number_like_alt_n_and_filter_by_title() {
        let rows = tab_rows("", &tabs());
        assert!(matches!(&rows[0], Row::Section { title, .. } if title.contains("3") && title.contains("2 panes")), "{:?}", rows[0]);
        assert!(matches!(&rows[3], Row::Tab { num: 3, .. }));
        let rows = tab_rows("tsu", &tabs());
        let found: Vec<&str> = rows.iter().filter_map(|r| if let Row::Tab { t, .. } = r { Some(t.title.as_str()) } else { None }).collect();
        assert_eq!(found, vec!["tsubame"]);
        // numbers stay the tab's own position even when filtered
        assert!(matches!(&rows[1], Row::Tab { num: 2, .. }));
    }

    #[test]
    fn filter_rows_show_the_counts_and_matches() {
        let d = Data { folder_name: "Downloads".into(), total: 33, match_count: 7, matches: vec![MatchRow { name: "Invoices".into(), kind: "File folder".into(), size: "".into(), is_dir: true }], ..Default::default() };
        let rows = filter_rows("inv", &d);
        assert!(matches!(&rows[0], Row::FilterHeader { shown: 7, total: 33, .. }));
        assert!(matches!(&rows[1], Row::Section { title, .. } if title == "Matches"));
        let Row::Match { hl, .. } = &rows[2] else { panic!() };
        assert_eq!(hl, &vec![0, 1, 2]);
    }

    #[test]
    fn selection_skips_section_headers() {
        let mut p = Palette::open(Mode::Commands, Data::default(), Box::new(|_| String::new()));
        p.rebuild();
        assert!(p.rows[p.sel].selectable());
        let first = p.sel;
        p.step(true, 1);
        assert!(p.sel > first && p.rows[p.sel].selectable());
        p.step(false, 1);
        assert_eq!(p.sel, first);
        p.step(false, 1);
        assert_eq!(p.sel, first, "stays at the top");
        for _ in 0..500 {
            p.step(true, 1);
        }
        assert_eq!(p.sel, p.rows.len() - 1, "stops at the last row");
    }

    #[test]
    fn enter_means_what_the_mode_says() {
        let mut p = Palette::open(Mode::Commands, Data::default(), Box::new(|_| String::new()));
        p.input.set_text("rename");
        p.rebuild();
        assert_eq!(p.activate(false, false), Action::Run { id: CMD_RENAME, keep: false });
        assert_eq!(p.activate(true, false), Action::Run { id: CMD_RENAME, keep: true });
        p.mode = Mode::Path;
        p.input.set_text("C:\\nope-not-a-folder-xyz\\");
        p.rebuild();
        assert_eq!(p.activate(false, false), Action::Go { path: "C:\\nope-not-a-folder-xyz\\".into(), new_tab: false });
        assert_eq!(p.activate(true, false), Action::Go { path: "C:\\nope-not-a-folder-xyz\\".into(), new_tab: true });
        p.mode = Mode::Tabs;
        p.data.tabs = tabs();
        p.input.set_text("");
        p.rebuild();
        assert_eq!(p.activate(false, false), Action::SwitchTab { pane: 0, index: 0 });
        assert_eq!(p.activate(true, false), Action::MoveTab { pane: 0, index: 0 });
        p.mode = Mode::Filter;
        p.rebuild();
        assert_eq!(p.activate(false, false), Action::FilterKeep { open: None });
    }

    #[test]
    fn building_the_rows_is_cheap() {
        let t = std::time::Instant::now();
        for q in ["", "r", "ren", "toggle", "sdl", "zzz"] {
            let _ = command_rows(q, &[], &keys);
        }
        assert!(t.elapsed() < std::time::Duration::from_millis(100), "{:?}", t.elapsed());
    }
}
