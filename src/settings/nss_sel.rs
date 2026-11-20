//! The Nilesoft import's selection screen (mock `ctxgap-nss`): what the file gave, as rows with
//! check boxes (a submenu is tri-state), the skipped statements with their reason and line, filter
//! chips, "Placed in group", Cancel and "Import N selected". Nothing is added to the menu until
//! Import; the report card (`menu_ed`) follows. The model ([`Sel`]) has no drawing in it, so
//! selection, tri-state, placement and undo are unit-tested.

use super::lay::{Look, Pg, button, button_in, fit};
use tsubame_config::menu::{self, MenuEntry};
use tsubame_config::nss::{self, Place};
use tsubame_ui::{Button, Cursor, Event, Id, Key, Mods, Rect, Sense, TextInput, f, text_input};

const ID_ALL: Id = Id::new("nss-all");
const ID_NONE: Id = Id::new("nss-none");
const ID_CHIP: Id = Id::new("nss-chip");
const ID_LIST: Id = Id::new("nss-list");
const ID_DD: Id = Id::new("nss-dd");
const ID_DD_POP: Id = Id::new("nss-dd-pop");
const ID_DD_LIST: Id = Id::new("nss-dd-list");
const ID_NAME: Id = Id::new("nss-name");
const ID_CANCEL: Id = Id::new("nss-cancel");
const ID_IMPORT: Id = Id::new("nss-import");

const ROW_H: f32 = 30.0;
const MAX_ROWS: usize = 12;

/// The chips above the list.
pub const FILTERS: [&str; 3] = ["All", "Importable", "Skipped"];

/// One row of the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// an item or submenu of the import (by id)
    Entry { id: String, depth: usize },
    /// a skipped statement (index into the import's `skipped`)
    Skipped { idx: usize, depth: usize },
}

/// What "Placed in group" is set to (the new group's name is a field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Where {
    Group(String),
    New,
    End,
}

/// What the screen asks for after a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    Cancel,
    Import,
}

/// The state of the open screen.
pub struct Sel {
    pub file: String,
    /// the full path shown under the title
    pub path: String,
    imp: nss::Import,
    rows: Vec<Row>,
    /// ids of the checked items (a submenu is not stored: its children say)
    on: Vec<String>,
    /// index into [`FILTERS`]
    pub filter: usize,
    /// the row the keyboard is on (an index into the shown rows)
    pub cursor: usize,
    scroll: f32,
    pub at: Where,
    pub name: TextInput,
    /// the highlighted option while the dropdown is open
    dd_cursor: usize,
    /// where the pointer was last frame: the highlight follows it only when it moves
    dd_mouse: (f32, f32),
    /// move the keyboard focus to the name field on the next frame (New group… was chosen)
    focus_name: bool,
}

impl Sel {
    /// The screen for `imp`: everything importable starts ticked; the place is the default one
    /// (a new group "Nilesoft" the first time, then the last used).
    pub fn new(file: &str, path: &str, imp: nss::Import, last: &str, users: &[menu::UserGroup]) -> Sel {
        let on: Vec<String> = leaves(&imp).into_iter().map(|e| e.id.clone()).collect();
        let rows = build_rows(&imp);
        let (at, name) = match nss::default_place(last, users) {
            Place::Group(id) => (Where::Group(id), "Nilesoft".to_string()),
            Place::NewGroup(n) => (Where::New, n),
            Place::End => (Where::End, "Nilesoft".to_string()),
        };
        Sel { file: file.to_string(), path: path.to_string(), imp, rows, on, filter: 0, cursor: 0, scroll: 0.0, at, name: TextInput::new(&name), dd_cursor: 0, dd_mouse: (0.0, 0.0), focus_name: false }
    }

    /// The rows the chip lets through.
    pub fn shown(&self) -> Vec<&Row> {
        self.rows
            .iter()
            .filter(|r| !matches!((self.filter, r), (1, Row::Skipped { .. }) | (2, Row::Entry { .. })))
            .collect()
    }

    fn entry(&self, id: &str) -> Option<&MenuEntry> {
        self.imp.entries.iter().find(|e| e.id == id)
    }

    /// Items under a submenu (itself when it is an item), at any depth.
    fn leaf_ids(&self, id: &str) -> Vec<String> {
        match self.entry(id) {
            Some(e) if e.is_submenu() && self.imp.entries.iter().any(|c| c.parent == id && !c.is_separator()) => {
                let kids: Vec<String> = self.imp.entries.iter().filter(|c| c.parent == id && !c.is_separator()).map(|c| c.id.clone()).collect();
                kids.iter().flat_map(|k| self.leaf_ids(k)).collect()
            }
            Some(_) => vec![id.to_string()],
            None => Vec::new(),
        }
    }

    /// The box of an entry row: all, none, or some of its items ticked.
    pub fn state(&self, id: &str) -> Tri {
        let l = self.leaf_ids(id);
        let n = l.iter().filter(|i| self.on.contains(i)).count();
        if n == 0 {
            Tri::Off
        } else if n == l.len() {
            Tri::On
        } else {
            Tri::Mixed
        }
    }

    /// Ticking a submenu ticks its items (all on unless all already are: then all off); an item flips.
    pub fn toggle(&mut self, id: &str) {
        let l = self.leaf_ids(id);
        let all = self.state(id) == Tri::On;
        for i in l {
            self.on.retain(|x| *x != i);
            if !all {
                self.on.push(i);
            }
        }
    }

    pub fn select_all(&mut self) {
        self.on = leaves(&self.imp).into_iter().map(|e| e.id.clone()).collect();
    }

    pub fn select_none(&mut self) {
        self.on.clear();
    }

    /// The import cut down to what is ticked.
    pub fn kept(&self) -> nss::Import {
        self.imp.keep_ids(&self.on)
    }

    /// How many entries Import would add (a submenu with a ticked child counts as one).
    pub fn selected(&self) -> usize {
        self.kept().imported
    }

    /// How many entries can be imported at all.
    pub fn importable(&self) -> usize {
        self.imp.imported
    }

    pub fn skipped(&self) -> usize {
        self.imp.skipped.len()
    }

    pub fn found(&self) -> usize {
        self.imp.found()
    }

    /// The place chosen, as the config crate takes it.
    pub fn place(&self) -> Place {
        match &self.at {
            Where::Group(id) => Place::Group(id.clone()),
            Where::New => Place::NewGroup(self.name.text.trim().to_string()),
            Where::End => Place::End,
        }
    }

    /// Adds what is ticked to `list` in the chosen place and remembers the place. Returns the
    /// report (it knows the group that was made, for Undo).
    pub fn commit(&self, list: &mut Vec<MenuEntry>, m: &mut tsubame_config::ContextMenu) -> nss::Report {
        let mut kept = self.kept();
        if kept.entries.is_empty() {
            // nothing ticked: no group is made, nothing is remembered
            return nss::Report::new(&self.file, &kept);
        }
        let place = self.place();
        let (created, last) = nss::place_import(&mut kept, &place, &mut m.user_groups, &m.groups);
        m.nss_group = last;
        let mut rep = nss::Report::new(&self.file, &kept);
        rep.group_name = match (&created, &place) {
            (Some(id), _) | (None, Place::Group(id)) => m.user_groups.iter().find(|u| u.id == *id).map(|u| u.name.clone()).unwrap_or_default(),
            _ => String::new(),
        };
        rep.created_group = created;
        list.extend(kept.entries);
        rep
    }

    /// The dropdown's options: the user groups, then "New group…" and "End of the menu".
    fn options(&self, users: &[menu::UserGroup]) -> Vec<(String, Where)> {
        let mut v: Vec<(String, Where)> = users.iter().filter(|u| !u.id.is_empty()).map(|u| (u.name.clone(), Where::Group(u.id.clone()))).collect();
        v.push(("New group\u{2026}".into(), Where::New));
        v.push(("End of the menu (no group)".into(), Where::End));
        v
    }

    fn at_label(&self, users: &[menu::UserGroup]) -> String {
        match &self.at {
            Where::Group(id) => users.iter().find(|u| u.id == *id).map(|u| u.name.clone()).unwrap_or_else(|| "End of the menu".into()),
            Where::New => "New group\u{2026}".into(),
            Where::End => "End of the menu (no group)".into(),
        }
    }

    /// Keeps the cursor in the shown rows.
    fn clamp(&mut self) {
        let n = self.shown().len();
        if n == 0 {
            self.cursor = 0;
        } else if self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    /// "shows for" of an entry row: Files, folders, Empty area, with the extensions.
    pub fn shows_for(&self, id: &str) -> String {
        let Some(e) = self.entry(id) else { return String::new() };
        if e.is_submenu() {
            return "Submenu".into();
        }
        shows_for(e)
    }
}

/// The three states of a row's box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tri {
    Off,
    On,
    Mixed,
}

/// The items that can be ticked (everything but a submenu that has items).
fn leaves(imp: &nss::Import) -> Vec<&MenuEntry> {
    imp.entries.iter().filter(|e| !e.is_separator() && !(e.is_submenu() && imp.entries.iter().any(|c| c.parent == e.id && !c.is_separator()))).collect()
}

/// "Files, folders" / "Empty area" / "Files · .txt .md".
pub fn shows_for(e: &MenuEntry) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if e.show_for.files {
        parts.push("Files");
    }
    if e.show_for.folders {
        parts.push("folders");
    }
    if e.show_for.background {
        parts.push("empty area");
    }
    let mut s = parts.join(", ");
    if let Some(c) = s.get(..1) {
        let up = c.to_uppercase();
        s.replace_range(..1, &up);
    }
    if s.is_empty() {
        s = "Nowhere".into();
    }
    if e.show_for.files && !e.extensions.is_empty() {
        s.push_str(" \u{b7} ");
        s.push_str(&e.extensions.iter().map(|x| format!(".{x}")).collect::<Vec<_>>().join(" "));
    }
    s
}

/// All rows in source order: entries and skipped statements merged by line; a skipped statement
/// inside a submenu's lines is indented like its children.
fn build_rows(imp: &nss::Import) -> Vec<Row> {
    let depth_of = |e: &MenuEntry| {
        let mut d = 0;
        let mut cur = e;
        while let Some(p) = imp.entries.iter().find(|p| p.id == cur.parent) {
            d += 1;
            cur = p;
        }
        d
    };
    // the line span of each submenu: its own line to its last child's
    let span = |id: &str| -> (usize, usize) {
        let from = imp.line_of(id).unwrap_or(0);
        let to = imp.entries.iter().filter(|c| c.parent == id).filter_map(|c| imp.line_of(&c.id)).max().unwrap_or(from);
        (from, to)
    };
    let mut rows: Vec<(usize, usize, Row)> = Vec::new();
    for (seq, e) in imp.entries.iter().filter(|e| !e.is_separator()).enumerate() {
        rows.push((imp.line_of(&e.id).unwrap_or(usize::MAX), seq, Row::Entry { id: e.id.clone(), depth: depth_of(e) }));
    }
    for (i, s) in imp.skipped.iter().enumerate() {
        let depth = imp.entries.iter().filter(|e| e.is_submenu()).filter(|e| {
            let (a, b) = span(&e.id);
            s.line > a && s.line < b
        }).map(|e| depth_of(e) + 1).max().unwrap_or(0);
        rows.push((s.line, 1_000_000 + i, Row::Skipped { idx: i, depth }));
    }
    rows.sort_by_key(|(l, s, _)| (*l, *s));
    rows.into_iter().map(|(_, _, r)| r).collect()
}

/// What to call a skipped statement: its `title='...'` when it has one, else its text.
pub fn skipped_name(text: &str) -> String {
    let low = text.to_ascii_lowercase();
    if let Some(i) = low.find("title") {
        let rest = text[i + 5..].trim_start();
        if let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start();
            if let Some(q) = rest.chars().next().filter(|c| matches!(*c, '\'' | '"'))
                && let Some(end) = rest[1..].find(q)
            {
                return rest[1..1 + end].replace('&', "");
            }
        }
    }
    text.to_string()
}

// ---- drawing

fn chip(pg: &mut Pg, id: Id, x: f32, y: f32, label: &str, count: usize, on: bool) -> (bool, Rect) {
    let t = pg.t;
    let cs = count.to_string();
    let lw = pg.g.measure(label, t.font_small());
    let cw = pg.g.measure(&cs, t.font_small()) + 8.0;
    let r = Rect::new(x, y, lw + cw + 20.0, t.height_field - 4.0);
    let it = pg.ui.interact(id, r, Sense::click().focusable().cursor(Cursor::Hand));
    let mut hit = it.clicked == Some(Button::Left);
    for (qi, ev) in pg.ui.key_input(id) {
        if let Event::KeyDown { key: Key::Enter | Key::Space, mods, .. } = ev
            && mods.none()
        {
            pg.ui.consume(qi);
            hit = true;
        }
    }
    let fill = if on { t.selected } else if it.hovered { t.hover } else { t.field };
    pg.g.round(r, r.h / 2.0, fill);
    pg.g.round_stroke(r, r.h / 2.0, if on { t.accent } else { t.popup_border }, 1.0);
    if pg.ui.has_focus(id) && pg.ui.window_focused() {
        pg.g.round_stroke(r.pad(-1.0), r.h / 2.0, t.focus, 1.0);
    }
    pg.g.text(label, Rect::new(r.x + 10.0, r.y, lw + 2.0, r.h), t.font_small(), if on { t.accent } else { t.text });
    pg.g.text(&cs, Rect::new(r.x + 10.0 + lw + 8.0, r.y, cw, r.h), t.font_small(), t.text_dim);
    if hit {
        pg.ui.request_repaint();
    }
    (hit, r)
}

fn check_box(pg: &mut Pg, r: Rect, st: Option<Tri>) {
    let t = pg.t;
    match st {
        Some(Tri::On) => {
            pg.g.round(r, t.radius_sm, t.accent);
            pg.g.text("\u{E73E}", r, f(r.h - 6.0).icon().c(), t.on_accent);
        }
        Some(Tri::Mixed) => {
            pg.g.round(r, t.radius_sm, t.accent);
            pg.g.fill(Rect::new(r.x + 4.0, r.y + r.h / 2.0 - 1.0, r.w - 8.0, 2.0), t.on_accent);
        }
        Some(Tri::Off) => {
            pg.g.round(r, t.radius_sm, t.field);
            pg.g.round_stroke(r, t.radius_sm, t.popup_border, 1.0);
        }
        // skipped: cannot be ticked
        None => pg.g.round_stroke(r, t.radius_sm, t.border, 1.0),
    }
}

/// Draws the screen at `pg.y` and moves the cursor below it. `users` are the groups of the menu.
/// True while the placement dropdown's popup is open (the page must not close it).
pub fn dropdown_open(ui: &tsubame_ui::Ui) -> bool {
    ui.is_popup_open(ID_DD_POP)
}

pub fn draw(pg: &mut Pg, sel: &mut Sel, users: &[menu::UserGroup]) -> Act {
    let t = pg.t;
    let mut act = Act::None;
    let shown_n = sel.shown().len();
    sel.clamp();
    let view_rows = shown_n.clamp(1, MAX_ROWS);
    let list_h = view_rows as f32 * ROW_H;
    let (head_h, bar_h, col_h, foot_h) = (56.0, 48.0, 26.0, 60.0);
    let r = Rect::new(pg.x, pg.y, pg.w, head_h + bar_h + col_h + list_h + foot_h);
    pg.g.round(r, t.radius_lg, t.panel);
    pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
    let selected = sel.selected();
    let importable = sel.importable();

    // header
    pg.g.text("\u{E896}", Rect::new(r.x + 14.0, r.y, 22.0, head_h), f(18.0).icon().c(), t.accent);
    let hw = r.w - 60.0;
    let title = format!("{}: {} {} found, {} can be imported, {} {} skipped", sel.file, sel.found(), if sel.found() == 1 { "entry" } else { "entries" }, importable, sel.skipped(), if sel.skipped() == 1 { "is" } else { "are" });
    let ts = fit(pg.g, &title, t.font().b(), hw);
    pg.g.text(&ts, Rect::new(r.x + 44.0, r.y + 10.0, hw, 20.0), t.font().b(), t.text);
    let sub = fit(pg.g, &format!("{}. The file is only read, never changed. Nothing is added until you press Import.", sel.path), t.font_small(), hw);
    pg.g.text(&sub, Rect::new(r.x + 44.0, r.y + 31.0, hw, 16.0), t.font_small(), t.text_dim);

    // bar: select all / none, chips, counter
    let by = r.y + head_h;
    pg.g.fill(Rect::new(r.x, by, r.w, 1.0), t.border);
    let bty = by + (bar_h - t.height_button) / 2.0;
    let (all, ar) = button(pg.ui, pg.g, ID_ALL, r.x + 14.0, bty, "Select all", Look::Normal, true, false);
    let (none, nr) = button(pg.ui, pg.g, ID_NONE, ar.r() + 8.0, bty, "Select none", Look::Normal, true, false);
    let counts = [sel.found(), importable, sel.skipped()];
    let mut cx = nr.r() + 14.0;
    for (i, l) in FILTERS.iter().enumerate() {
        let (hit, cr) = chip(pg, ID_CHIP.with(i as u64), cx, by + (bar_h - (t.height_field - 4.0)) / 2.0, l, counts[i], sel.filter == i);
        if hit {
            sel.filter = i;
            sel.cursor = 0;
            sel.scroll = 0.0;
        }
        cx = cr.r() + 6.0;
    }
    let cnt = format!("{selected} of {importable} selected");
    let cw = pg.g.measure(&cnt, t.font_small().b()) + 4.0;
    pg.g.text(&cnt, Rect::new(r.r() - 14.0 - cw, by, cw, bar_h), t.font_small().b(), t.text);
    if all {
        sel.select_all();
    }
    if none {
        sel.select_none();
    }

    // column header
    let cy = by + bar_h;
    pg.g.fill(Rect::new(r.x, cy, r.w, 1.0), t.border);
    let (why_x, ln_w) = (r.x + r.w * 0.52, 74.0);
    pg.g.text("Entry", Rect::new(r.x + 64.0, cy, 200.0, col_h), t.font_small(), t.text_dim);
    pg.g.text("Shows for / why skipped", Rect::new(why_x, cy, 260.0, col_h), t.font_small(), t.text_dim);
    pg.g.text("Source", Rect::new(r.r() - ln_w - 14.0, cy, ln_w, col_h), t.font_small(), t.text_dim);

    // the list: one focusable area, the keyboard row is the cursor
    let ly = cy + col_h;
    let view = Rect::new(r.x, ly, r.w, list_h);
    pg.g.fill(Rect::new(r.x, ly, r.w, 1.0), t.border);
    let it = pg.ui.interact(ID_LIST, view, Sense::click().focusable().cursor(Cursor::Arrow));
    let focused = pg.ui.has_focus(ID_LIST) && pg.ui.window_focused();
    let content = shown_n as f32 * ROW_H;
    let max_scroll = (content - list_h).max(0.0);
    let (_, wheel) = it.wheel;
    if wheel != 0.0 {
        sel.scroll = (sel.scroll - wheel * ROW_H * 3.0).clamp(0.0, max_scroll);
        pg.ui.request_repaint();
    }
    let mut toggle: Option<String> = None;
    if let Some(p) = it.pressed.filter(|p| p.button == Button::Left) {
        let i = ((p.pos.y - ly + sel.scroll) / ROW_H).floor();
        if i >= 0.0 && (i as usize) < shown_n {
            sel.cursor = i as usize;
            // the box (or a double click) ticks the row
            if (p.pos.x < r.x + 54.0 || p.double)
                && let Row::Entry { id, .. } = sel.shown()[i as usize]
            {
                toggle = Some(id.clone());
            }
        }
    }
    for (qi, ev) in pg.ui.key_input(ID_LIST) {
        let Event::KeyDown { key, mods, .. } = ev else { continue };
        if !mods.none() {
            continue;
        }
        let last = shown_n.saturating_sub(1);
        let page = MAX_ROWS - 1;
        match key {
            Key::Up => sel.cursor = sel.cursor.saturating_sub(1),
            Key::Down => sel.cursor = (sel.cursor + 1).min(last),
            Key::Home => sel.cursor = 0,
            Key::End => sel.cursor = last,
            Key::PageUp => sel.cursor = sel.cursor.saturating_sub(page),
            Key::PageDown => sel.cursor = (sel.cursor + page).min(last),
            Key::Space => {
                if let Some(Row::Entry { id, .. }) = sel.shown().get(sel.cursor) {
                    toggle = Some(id.clone());
                }
            }
            Key::Enter => {
                if selected > 0 {
                    act = Act::Import;
                }
            }
            _ => continue,
        }
        pg.ui.consume(qi);
        pg.ui.request_repaint();
    }
    if let Some(id) = toggle {
        sel.toggle(&id);
        pg.ui.request_repaint();
    }
    // the cursor row stays in view
    let cur_y = sel.cursor as f32 * ROW_H;
    if cur_y < sel.scroll {
        sel.scroll = cur_y;
    } else if cur_y + ROW_H > sel.scroll + list_h {
        sel.scroll = cur_y + ROW_H - list_h;
    }
    sel.scroll = sel.scroll.clamp(0.0, max_scroll);

    pg.g.clip(view);
    let rows: Vec<Row> = sel.shown().into_iter().cloned().collect();
    for (i, row) in rows.iter().enumerate() {
        let rr = Rect::new(r.x, ly + i as f32 * ROW_H - sel.scroll, r.w, ROW_H);
        if rr.b() < ly || rr.y > ly + list_h {
            continue;
        }
        if i > 0 {
            pg.g.fill(Rect::new(rr.x, rr.y, rr.w, 1.0), t.border);
        }
        if focused && i == sel.cursor {
            pg.g.fill(rr, t.hover);
            pg.g.round_stroke(rr.pad(-1.0), t.radius_sm, t.focus, 1.0);
        }
        match row {
            Row::Entry { id, depth } => {
                let Some(e) = sel.entry(id) else { continue };
                let ind = *depth as f32 * 26.0;
                check_box(pg, Rect::new(r.x + 18.0 + ind, rr.y + (ROW_H - 16.0) / 2.0, 16.0, 16.0), Some(sel.state(id)));
                let nx = r.x + 46.0 + ind;
                let sub = if e.is_submenu() { format!(" \u{203a} submenu, {} {}", sel.leaf_ids(id).len(), if sel.leaf_ids(id).len() == 1 { "entry" } else { "entries" }) } else { String::new() };
                let nw = (why_x - nx - 12.0).max(40.0);
                let name = fit(pg.g, &e.name, t.font(), nw);
                pg.g.text(&name, Rect::new(nx, rr.y, nw, ROW_H), t.font(), t.text);
                if !sub.is_empty() {
                    let w1 = pg.g.measure(&name, t.font());
                    pg.g.text(&sub, Rect::new(nx + w1 + 4.0, rr.y, 200.0, ROW_H), t.font_small(), t.text_dim);
                }
                let why = fit(pg.g, &sel.shows_for(id), t.font_small(), r.r() - ln_w - 28.0 - why_x);
                pg.g.text(&why, Rect::new(why_x, rr.y, r.r() - why_x, ROW_H), t.font_small(), t.text);
                if let Some(l) = sel.imp.line_of(id) {
                    pg.g.text(&format!("line {l}"), Rect::new(r.r() - ln_w - 14.0, rr.y, ln_w, ROW_H), t.font_small(), t.text_dim);
                }
            }
            Row::Skipped { idx, depth } => {
                let s = &sel.imp.skipped[*idx];
                let ind = *depth as f32 * 26.0;
                check_box(pg, Rect::new(r.x + 18.0 + ind, rr.y + (ROW_H - 16.0) / 2.0, 16.0, 16.0), None);
                let nx = r.x + 46.0 + ind;
                let nw = (why_x - nx - 12.0).max(40.0);
                let name = fit(pg.g, &skipped_name(&s.text), t.font(), nw);
                pg.g.text(&name, Rect::new(nx, rr.y, nw, ROW_H), t.font(), t.text_disabled);
                pg.g.text("\u{E7BA}", Rect::new(why_x, rr.y, 16.0, ROW_H), f(13.0).icon().c(), t.danger);
                let rw = r.r() - ln_w - 28.0 - why_x - 22.0;
                let why = fit(pg.g, &s.reason, t.font_small(), rw);
                pg.g.text(&why, Rect::new(why_x + 22.0, rr.y, rw, ROW_H), t.font_small(), t.text);
                pg.g.text(&format!("line {}", s.line), Rect::new(r.r() - ln_w - 14.0, rr.y, ln_w, ROW_H), t.font_small(), t.text_dim);
            }
        }
    }
    pg.g.unclip();
    if shown_n == 0 {
        pg.g.text("Nothing in this view.", Rect::new(r.x + 18.0, ly, r.w, ROW_H), t.font_small(), t.text_dim);
    }

    // footer: Placed in group, the dropdown (opens upward), the new group's name, Cancel, Import
    let fy = ly + list_h;
    pg.g.fill(Rect::new(r.x, fy, r.w, 1.0), t.border);
    let rowy = fy + (foot_h - t.height_field) / 2.0;
    pg.g.text("Placed in group", Rect::new(r.x + 14.0, fy, 110.0, foot_h), t.font_small(), t.text_dim);
    let dd = Rect::new(r.x + 130.0, rowy, 250.0, t.height_field);
    let ddi = pg.ui.interact(ID_DD, dd, Sense::click().focusable().cursor(Cursor::Hand));
    let open = pg.ui.is_popup_open(ID_DD_POP);
    pg.g.round(dd, t.radius_md, if ddi.hovered { t.hover } else { t.field });
    pg.g.round_stroke(dd, t.radius_md, t.popup_border, 1.0);
    if pg.ui.has_focus(ID_DD) && pg.ui.window_focused() {
        pg.g.round_stroke(dd.pad(-1.0), t.radius_md, t.focus, 1.0);
    }
    let label = fit(pg.g, &sel.at_label(users), t.font(), dd.w - 40.0);
    pg.g.text(&label, Rect::new(dd.x + 10.0, dd.y, dd.w - 36.0, dd.h), t.font(), t.text);
    pg.g.text("\u{E70D}", Rect::new(dd.r() - 28.0, dd.y, 22.0, dd.h), f(11.0).icon().c(), t.text_dim);
    let mut open_dd = ddi.clicked == Some(Button::Left);
    for (qi, ev) in pg.ui.key_input(ID_DD) {
        if let Event::KeyDown { key, mods, .. } = ev
            && mods.none()
            && matches!(key, Key::Enter | Key::Space | Key::Down | Key::Up)
        {
            pg.ui.consume(qi);
            open_dd = true;
        }
    }
    let opts = sel.options(users);
    // Enter and friends are app shortcuts (Open, ...): the dropdown keeps them while it has the focus
    if pg.ui.has_focus(ID_DD) || pg.ui.has_focus(ID_DD_LIST) || pg.ui.has_focus(ID_LIST) {
        pg.ui.claimed.extend([(Key::Enter, Mods::NONE), (Key::Space, Mods::NONE), (Key::Up, Mods::NONE), (Key::Down, Mods::NONE), (Key::Home, Mods::NONE), (Key::End, Mods::NONE), (Key::PageUp, Mods::NONE), (Key::PageDown, Mods::NONE)]);
    }
    if open_dd && !open {
        sel.dd_cursor = opts.iter().position(|(_, w)| *w == sel.at).unwrap_or(0);
        sel.dd_mouse = (pg.ui.mouse().x, pg.ui.mouse().y);
        pg.ui.open_popup(ID_DD_POP);
        pg.ui.set_focus(Some(ID_DD_LIST));
        pg.ui.request_repaint();
    }
    let mut name_x = dd.r() + 10.0;
    if sel.at == Where::New {
        let nf = Rect::new(name_x, rowy, 190.0, t.height_field);
        if std::mem::take(&mut sel.focus_name) {
            pg.ui.set_focus(Some(ID_NAME));
            sel.name.select_all();
        }
        let resp = text_input(pg.ui, pg.g, ID_NAME, nf, &mut sel.name, "Group name");
        pg.ui.mark_text_focus();
        if resp.submitted && selected > 0 {
            act = Act::Import;
        }
        name_x = nf.r() + 10.0;
    }
    let _ = name_x;
    let imp_label = format!("Import {selected} selected");
    let iw = pg.g.measure(&imp_label, t.font().b()) + 28.0;
    let imp_r = Rect::new(r.r() - 14.0 - iw, rowy + (t.height_field - t.height_button) / 2.0, iw, t.height_button);
    let cancel_w = pg.g.measure("Cancel", t.font().b()) + 28.0;
    let cancel_r = Rect::new(imp_r.x - 8.0 - cancel_w, imp_r.y, cancel_w, t.height_button);
    if button_in(pg.ui, pg.g, ID_CANCEL, cancel_r, "Cancel", Look::Normal, true) {
        act = Act::Cancel;
    }
    if button_in(pg.ui, pg.g, ID_IMPORT, imp_r, &imp_label, Look::Primary, selected > 0) {
        act = Act::Import;
    }
    pg.y = r.b() + 12.0;

    // Esc from anywhere in the screen (a text field or the dropdown take their own first)
    if act == Act::None && !pg.ui.is_popup_open(ID_DD_POP) {
        let mine = [ID_ALL, ID_NONE, ID_LIST, ID_DD, ID_NAME, ID_CANCEL, ID_IMPORT];
        let chips = (0..FILTERS.len()).map(|i| ID_CHIP.with(i as u64));
        if let Some(fid) = pg.ui.focus().filter(|f| mine.contains(f) || chips.clone().any(|c| c == *f)) {
            for (qi, ev) in pg.ui.key_input(fid) {
                if let Event::KeyDown { key: Key::Escape, mods, .. } = ev
                    && mods.none()
                {
                    pg.ui.consume(qi);
                    act = Act::Cancel;
                }
            }
        }
    }

    // the dropdown, above the field: drawn last so it covers the list
    let ph = opts.len() as f32 * 26.0 + 8.0 + if opts.len() > 2 { 9.0 } else { 0.0 };
    let pop = Rect::new(dd.x, dd.y - 4.0 - ph, dd.w, ph);
    if pg.ui.popup_begin(pg.g, ID_DD_POP, pop) {
        let li = pg.ui.interact(ID_DD_LIST, pop, Sense::click().focusable());
        let mut pick: Option<usize> = None;
        for (qi, ev) in pg.ui.key_input(ID_DD_LIST) {
            let Event::KeyDown { key, mods, .. } = ev else { continue };
            if !mods.none() {
                continue;
            }
            match key {
                Key::Up => sel.dd_cursor = sel.dd_cursor.saturating_sub(1),
                Key::Down => sel.dd_cursor = (sel.dd_cursor + 1).min(opts.len() - 1),
                Key::Home => sel.dd_cursor = 0,
                Key::End => sel.dd_cursor = opts.len() - 1,
                Key::Enter | Key::Space => pick = Some(sel.dd_cursor),
                _ => continue,
            }
            pg.ui.consume(qi);
            pg.ui.request_repaint();
        }
        let m = pg.ui.mouse();
        let moved = (m.x, m.y) != sel.dd_mouse;
        sel.dd_mouse = (m.x, m.y);
        let mut y = pop.y + 4.0;
        for (i, (name, w)) in opts.iter().enumerate() {
            // a rule above the last two options (the fixed ones)
            if i + 2 == opts.len() && i > 0 {
                pg.g.fill(Rect::new(pop.x + 6.0, y + 4.0, pop.w - 12.0, 1.0), t.border);
                y += 9.0;
            }
            let rr = Rect::new(pop.x + 4.0, y, pop.w - 8.0, 26.0);
            let hot = li.hovered && rr.contains_pt(pg.ui.mouse());
            if hot && moved {
                sel.dd_cursor = i;
            }
            if i == sel.dd_cursor {
                pg.g.round(rr, t.radius_sm, t.hover);
            }
            if sel.at == *w {
                pg.g.circle(rr.x + 12.0, rr.y + rr.h / 2.0, 3.0, t.accent);
            }
            let lab = fit(pg.g, name, t.font(), rr.w - 34.0);
            pg.g.text(&lab, Rect::new(rr.x + 28.0, rr.y, rr.w - 32.0, rr.h), t.font(), t.text);
            if li.clicked == Some(Button::Left) && rr.contains_pt(li.click_pos) {
                pick = Some(i);
            }
            y += 26.0;
        }
        pg.ui.popup_end(pg.g);
        if let Some(i) = pick.and_then(|i| opts.get(i)) {
            sel.at = i.1.clone();
            sel.focus_name = sel.at == Where::New;
            pg.ui.close_popup(ID_DD_POP);
            pg.ui.request_repaint();
        }
    }
    if act == Act::None && !pg.ui.is_popup_open(ID_DD_POP) && pg.ui.has_focus(ID_DD_LIST) {
        pg.ui.set_focus(Some(ID_DD));
    }
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
shell
{
    menu(title='Tools')
    {
        item(title='VS Code' cmd='code.exe')
        item(title='Git Bash' cmd='git-bash.exe' type='dir')
        item(title='Hash' cmd='hash.exe' find='.zip|.7z')
    }
    item(title='Copy path' cmd='cp.exe')
    item(title='Take ownership' admin cmd='x.exe')
    item(title='Notepad here' type='back' cmd='notepad.exe')
}
"#;

    fn sel(last: &str, users: &[menu::UserGroup]) -> Sel {
        Sel::new("shell.nss", "C:\\x\\shell.nss", nss::import(SRC, &[], ""), last, users)
    }

    fn id_of(s: &Sel, name: &str) -> String {
        s.imp.entries.iter().find(|e| e.name == name).unwrap().id.clone()
    }

    #[test]
    fn everything_importable_starts_ticked_and_skipped_rows_cannot_be_ticked() {
        let s = sel("", &[]);
        assert_eq!((s.found(), s.importable(), s.skipped(), s.selected()), (7, 6, 1, 6));
        let skipped = s.rows.iter().filter(|r| matches!(r, Row::Skipped { .. })).count();
        assert_eq!(skipped, 1);
        assert!(s.on.iter().all(|i| s.entry(i).is_some()), "only entry ids are ticked");
        // in source order, the skipped one sits between Copy path and Notepad
        let names: Vec<String> = s.rows.iter().map(|r| match r {
            Row::Entry { id, .. } => s.entry(id).unwrap().name.clone(),
            Row::Skipped { idx, .. } => format!("skip:{}", s.imp.skipped[*idx].line),
        }).collect();
        assert_eq!(names[..3], ["Tools", "VS Code", "Git Bash"]);
        assert!(names[names.len() - 2].starts_with("skip:") && names[names.len() - 1] == "Notepad here", "{names:?}");
        // children are indented, the skipped top-level row is not
        assert!(matches!(&s.rows[1], Row::Entry { depth: 1, .. }));
    }

    #[test]
    fn a_submenu_box_is_tri_state_and_ticks_its_children() {
        let mut s = sel("", &[]);
        let (tools, hash) = (id_of(&s, "Tools"), id_of(&s, "Hash"));
        assert_eq!(s.state(&tools), Tri::On);
        s.toggle(&hash);
        assert_eq!((s.state(&tools), s.state(&hash)), (Tri::Mixed, Tri::Off));
        assert_eq!(s.selected(), 5, "the submenu still counts: 6 importable, one child off");
        // a mixed submenu ticks all on the first click, all off on the next
        s.toggle(&tools);
        assert_eq!(s.state(&tools), Tri::On);
        s.toggle(&tools);
        assert_eq!(s.state(&tools), Tri::Off);
        assert_eq!(s.selected(), 2, "Copy path and Notepad here");
        // all children off: the submenu is not imported
        let kept = s.kept();
        assert!(kept.entries.iter().all(|e| e.name != "Tools"));
        s.select_all();
        assert_eq!(s.selected(), 6);
        s.select_none();
        assert_eq!((s.selected(), s.kept().entries.len()), (0, 0));
    }

    #[test]
    fn chips_filter_the_rows_but_not_the_selection() {
        let mut s = sel("", &[]);
        assert_eq!(s.shown().len(), 7);
        s.filter = 1;
        assert_eq!(s.shown().len(), 6);
        s.filter = 2;
        assert_eq!(s.shown().len(), 1);
        assert!(matches!(s.shown()[0], Row::Skipped { .. }));
        s.cursor = 9;
        s.clamp();
        assert_eq!(s.cursor, 0);
        assert_eq!(s.selected(), 6, "the chip does not change what is ticked");
    }

    #[test]
    fn a_skipped_row_is_named_by_its_title() {
        assert_eq!(skipped_name("item(title='Take &ownership' admin cmd='x')"), "Take ownership");
        assert_eq!(skipped_name("menu(title=\"Tools\" mode=multiple)"), "Tools");
        assert_eq!(skipped_name("item(title=if(sel.count>1,'a','b') cmd='x')"), "item(title=if(sel.count>1,'a','b') cmd='x')");
        assert_eq!(skipped_name("theme { name = 'modern' }"), "theme { name = 'modern' }");
    }

    #[test]
    fn shows_for_reads_like_the_mock() {
        let s = sel("", &[]);
        assert_eq!(s.shows_for(&id_of(&s, "Tools")), "Submenu");
        assert_eq!(s.shows_for(&id_of(&s, "VS Code")), "Files, folders");
        assert_eq!(s.shows_for(&id_of(&s, "Git Bash")), "Folders");
        assert_eq!(s.shows_for(&id_of(&s, "Hash")), "Files, folders \u{b7} .zip .7z");
        assert_eq!(s.shows_for(&id_of(&s, "Notepad here")), "Empty area");
    }

    #[test]
    fn the_place_starts_as_a_new_nilesoft_group_then_the_last_used_one() {
        let users = vec![menu::UserGroup { id: "g1".into(), name: "Tools".into(), pos: 4, ..Default::default() }];
        let s = sel("", &users);
        assert_eq!((&s.at, s.name.text.as_str()), (&Where::New, "Nilesoft"));
        assert_eq!(s.place(), Place::NewGroup("Nilesoft".into()));
        assert_eq!(sel("g1", &users).at, Where::Group("g1".into()));
        assert_eq!(sel(nss::PLACE_END, &users).at, Where::End);
        assert_eq!(sel("g9", &users).at, Where::New, "a group that is gone");
        let s = sel("g1", &users);
        let labels: Vec<String> = s.options(&users).into_iter().map(|(l, _)| l).collect();
        assert_eq!(labels, ["Tools", "New group\u{2026}", "End of the menu (no group)"]);
        assert_eq!(s.at_label(&users), "Tools");
    }

    #[test]
    fn commit_places_remembers_and_undo_removes_the_group_it_made() {
        let mut m = tsubame_config::ContextMenu::default();
        let mut list = vec![MenuEntry::custom("custom.1", "Mine", "m.exe", "")];
        let mut s = sel(&m.nss_group, &m.user_groups);
        let hash = id_of(&s, "Hash");
        s.toggle(&hash);
        let rep = s.commit(&mut list, &mut m);
        assert_eq!(m.user_groups.len(), 1);
        assert_eq!(m.user_groups[0].name, "Nilesoft");
        assert_eq!(m.nss_group, m.user_groups[0].id, "the last used group is remembered");
        assert_eq!((rep.imported, rep.group_name.as_str(), rep.created_group.clone()), (5, "Nilesoft", Some(m.nss_group.clone())));
        assert_eq!(list.len(), 1 + 5, "Mine, Tools + 2 children, Copy path, Notepad here");
        assert!(list.iter().filter(|e| e.source == menu::Source::Nilesoft && e.parent.is_empty()).all(|e| e.group == m.nss_group));
        assert!(list.iter().all(|e| e.name != "Hash"), "the unticked child is not added");
        let mut items: Vec<menu::ItemPref> = Vec::new();
        rep.undo_all(&mut list, &mut m.user_groups, &mut items);
        assert_eq!(list.len(), 1);
        assert!(m.user_groups.is_empty(), "Undo removes the group the import made");
        // the second import goes to the last used group (made again here) and keeps it on undo
        let g = menu::add_group(&mut m.user_groups, &m.groups, "Mine");
        m.nss_group = g.clone();
        let s = sel(&m.nss_group, &m.user_groups);
        assert_eq!(s.at, Where::Group(g.clone()));
        let rep = s.commit(&mut list, &mut m);
        assert_eq!((rep.created_group.clone(), rep.group_name.as_str()), (None, "Mine"));
        rep.undo_all(&mut list, &mut m.user_groups, &mut items);
        assert_eq!((list.len(), m.user_groups.len()), (1, 1));
        // the end of the menu: no group, remembered
        let mut s = sel(&m.nss_group, &m.user_groups);
        s.at = Where::End;
        let rep = s.commit(&mut list, &mut m);
        assert_eq!((m.nss_group.as_str(), rep.group_name.as_str()), (nss::PLACE_END, ""));
        assert!(list.iter().all(|e| e.group.is_empty()));
    }

    #[test]
    fn nothing_ticked_imports_nothing() {
        let mut s = sel("", &[]);
        s.select_none();
        let mut m = tsubame_config::ContextMenu::default();
        let mut list = Vec::new();
        // the screen disables Import at 0; commit with nothing still adds nothing
        let rep = s.commit(&mut list, &mut m);
        assert!(list.is_empty() && rep.imported == 0 && m.user_groups.is_empty() && m.nss_group.is_empty());
    }
}
