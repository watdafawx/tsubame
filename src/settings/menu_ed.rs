//! Settings > Context menu: the editable right-click menu. Hide items with the eye, drag the
//! grip to reorder (drop on a submenu to move inside), double-click a name to rename, add custom
//! commands (name + program + arguments + optional extensions), import a Nilesoft `shell.nss`.
//!
//! The list is `[[context_menu.entries]]` of the config. The native Windows shell menu stays;
//! Tsubame only appends the enabled custom entries to it (`shellops`).

use super::lay::{Look, Pg, button, fit, wrap};
use super::{C_MENU, Cx, nss_pick, nss_sel};
use crate::keymap::KeyMap;
use crate::{ctxui, rowmenu};
use std::time::{Duration, Instant};
use tsubame_config::menu::{self, EntryKind, MenuEntry, ShowFor, Source};
use tsubame_config::nss;
use tsubame_ui::widgets::menu::{EDIT_DELETE, EDIT_DOWN, EDIT_ENTRY, EDIT_GROUP, EDIT_GROUP_BASE, EDIT_GROUP_NATIVE, EDIT_GROUP_NEW, EDIT_HIDE, EDIT_PIN, EDIT_RENAME, EDIT_UP, edit_only};
use tsubame_ui::{Button, Cursor, Event, Id, Key, MenuRow, MenuSource, MenuState, MenuStyle, Mods, Pt, Rect, Sense, TextInput, f, text_input, toggle};

const ID_GRIP: Id = Id::new("menu-grip");
const ID_ROW: Id = Id::new("menu-row");
const ID_EYE: Id = Id::new("menu-eye");
const ID_DEL: Id = Id::new("menu-del");
const ID_EDIT: Id = Id::new("menu-rename");
const ID_TB: Id = Id::new("menu-tb");
const ID_FORM: Id = Id::new("menu-form");
const ID_SEARCH: Id = Id::new("menu-search");
const ID_CHIP: Id = Id::new("menu-chip");
const ID_GROUP: Id = Id::new("menu-group");
const ID_ITEM: Id = Id::new("menu-item");
const ID_ITEM_EYE: Id = Id::new("menu-item-eye");
const ID_ITEM_EDIT: Id = Id::new("menu-item-rename");
const ID_KIND: Id = Id::new("menu-kind");
const ID_GROW: Id = Id::new("menu-group-row");
const ID_GDEL: Id = Id::new("menu-group-del");
const ID_GNAME: Id = Id::new("menu-group-name");
const ID_GEDIT: Id = Id::new("menu-group-rename");
const ID_IGRIP: Id = Id::new("menu-item-grip");
const ID_HDR: Id = Id::new("menu-item-hdr");
const ID_DOTS: Id = Id::new("menu-dots");
const ID_PIN: Id = Id::new("menu-pin");
const ID_POP: Id = Id::new("menu-rowpop");

/// The filter chips (mock 6b). "3rd party" = a packaged (Win11) item whose
/// package family is not Microsoft's (`menu::is_third_party`): the classic menu
/// exposes no owning program, so nothing else can be told apart.
const CHIPS: [&str; 5] = ["All", "Windows", "3rd party", "Tsubame", "Hidden"];

/// The kind switch (mock 6b): which right-click the list is shown for. The
/// config has `show_for` flags per entry (file, folder, background, multiple,
/// drives). "Several items" lists the entries that show for 2 or more selected
/// items, "Drive" the ones for a drive root; both also list only the learned
/// items a menu of that kind has shown.
const KINDS: [&str; 5] = ["File", "Folder", "Empty area", "Several items", "Drive"];

#[derive(Default)]
pub struct State {
    sel: Option<String>,
    /// renaming: (entry id, field)
    edit: Option<(String, TextInput)>,
    edit_fresh: bool,
    /// index of the entry being dragged
    drag: Option<usize>,
    add: Option<Box<AddForm>>,
    /// the open file dialog (a worker thread)
    picker: Option<nss_pick::Picker>,
    /// a shell.nss found in a usual place (looked up once per visit)
    detected: Option<Option<nss::Detected>>,
    /// the selection screen of an import that is being reviewed (mock `ctxgap-nss`)
    review: Option<Box<nss_sel::Sel>>,
    /// the last import's report (mock 6b state 3)
    report: Option<nss::Report>,
    msg: Option<(String, bool)>,
    confirm_reset: bool,
    search: TextInput,
    /// index into [`CHIPS`]
    chip: usize,
    /// index into [`KINDS`]; none = every kind
    kind: Option<usize>,
    /// Ctrl+F: focus the search field on the next paint
    want_search: bool,
    /// the group being dragged (index into the drawn full order)
    gdrag: Option<usize>,
    /// renaming a user group: (id, field)
    gedit: Option<(String, TextInput)>,
    /// the learned item being dragged (index into `context_menu.items`)
    idrag: Option<usize>,
    /// renaming a learned item: (key, field)
    iedit: Option<(String, TextInput)>,
    /// the keys tagged New when the page was opened (shown this visit; the
    /// flags themselves are cleared at once)
    new_shown: Option<Vec<String>>,
    /// `focus_item`: select this key and search for it
    focus: Option<String>,
    /// the open row "..." popup
    pop: Option<RowPop>,
    /// a key on a focused "..." button: run this action on that row this frame
    act: Option<(RowTarget, u64)>,
    /// "Refresh from last menu"
    refresh: Refresh,
    /// "Edit entry…" from the menu: open this entry's form once the list is known
    edit_entry: Option<String>,
    /// the last Delete entry (the Undo line)
    deleted: Option<Deleted>,
    /// a tooltip drawn last: (pointer, text)
    tip: Option<(Pt, String)>,
}

impl State {
    /// Ctrl+F on the page: the search field takes the focus.
    pub fn focus_search(&mut self) {
        self.want_search = true;
    }
}

#[derive(Default)]
struct AddForm {
    name: TextInput,
    cmd: TextInput,
    args: TextInput,
    ext: TextInput,
    /// "Show for": File, Folder, Empty area, Several items, Drive
    show: ShowFor,
    err: Option<String>,
    /// Edit entry: the id of the entry the form changes (None: a new one)
    edit: Option<String>,
}

impl AddForm {
    /// A new entry's form: it shows for files, folders and a selection of several.
    fn blank() -> AddForm {
        AddForm { show: ShowFor { multiple: true, drives: false, ..ShowFor::default() }, ..Default::default() }
    }
}

impl State {
    pub fn reset_transient(&mut self) {
        *self = State::default();
    }

    /// `Edit in Settings…` / Manage from the menu: the list shows this row.
    pub fn focus_item(&mut self, key: &str) {
        *self = State::default();
        self.focus = Some(key.to_string());
    }

    /// "Edit entry…" from the menu: the row is selected and its form is open.
    pub fn focus_edit(&mut self, key: &str) {
        self.focus_item(key);
        self.edit_entry = Some(key.to_string());
    }
}

/// "txt, .md *.log" -> ["txt", "md", "log"].
pub fn parse_exts(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in s.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        let p = part.trim().trim_start_matches('*').trim_start_matches('.').to_ascii_lowercase();
        if !p.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// A new custom entry, or why the form is not ready.
pub fn new_entry(list: &[MenuEntry], name: &str, program: &str, args: &str, exts: &str) -> Result<MenuEntry, String> {
    let (name, program) = (name.trim(), program.trim().trim_matches('"'));
    if name.is_empty() {
        return Err("Give the command a name.".into());
    }
    if program.is_empty() {
        return Err("Say which program to run.".into());
    }
    crate::custom_cmd::split_args(args).map_err(|e| format!("The arguments are not valid: {e}"))?;
    let mut e = MenuEntry::custom(&menu::next_id(list, "custom"), name, program, args.trim());
    e.extensions = parse_exts(exts);
    e.show_for = ShowFor { multiple: true, drives: false, ..ShowFor::default() };
    Ok(e)
}

fn depth(list: &[MenuEntry], e: &MenuEntry) -> usize {
    let mut d = 0;
    let mut p = e.parent.as_str();
    while !p.is_empty() && d < 3 {
        match list.iter().find(|x| x.id == p) {
            Some(x) => {
                d += 1;
                p = x.parent.as_str();
            }
            None => break,
        }
    }
    d
}

fn shortcut_of(e: &MenuEntry, km: &KeyMap) -> String {
    if let Some(c) = crate::commands::by_name(&e.command) {
        let l = km.label(c.id);
        if !l.is_empty() {
            return l;
        }
    }
    match e.id.as_str() {
        "builtin.open" => "Enter",
        "builtin.cut" => "Ctrl+X",
        "builtin.copy" => "Ctrl+C",
        "builtin.paste" => "Ctrl+V",
        "builtin.copy-path" => "Ctrl+Shift+C",
        "builtin.delete" => "Del",
        "builtin.properties" => "Alt+Enter",
        "windows.more" => "Shift+F10",
        _ => "",
    }
    .to_string()
}

/// Where a drop at insertion point `to` puts the entry: the parent it gets.
pub fn drop_parent(list: &[MenuEntry], from: usize, to: usize, onto_submenu: Option<usize>) -> String {
    if list[from].is_submenu() {
        return String::new();
    }
    if let Some(s) = onto_submenu {
        return list[s].id.clone();
    }
    // between two rows: the parent of the row that follows (a row of a submenu stays in it)
    match list.get(to) {
        Some(next) if next.id != list[from].id => next.parent.clone(),
        Some(_) => list[from].parent.clone(),
        None => String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn label_field(pg: &mut Pg, id: Id, label: &str, input: &mut TextInput, x: f32, y: f32, w: f32, ph: &str) {
    let t = pg.t;
    pg.g.text(label, Rect::new(x, y, 90.0, t.height_field), t.font_small(), t.text_dim);
    let r = Rect::new(x + 96.0, y, w - 96.0, t.height_field);
    text_input(pg.ui, pg.g, id, r, input, ph);
    if pg.ui.has_focus(id) && pg.ui.window_focused() {
        pg.ui.mark_text_focus();
    }
}

/// Does a learned item pass the search `q` (lower-case) and the chip?
#[cfg(test)]
pub fn item_shown(p: &menu::ItemPref, q: &str, chip: usize) -> bool {
    item_shown_for(p, q, chip, None)
}

/// [`item_shown`] for the kind switch's pick (`None`: every kind). Several
/// items and Drive list only the learned items a menu of that kind showed.
pub fn item_shown_for(p: &menu::ItemPref, q: &str, chip: usize, kind: Option<usize>) -> bool {
    if kind.and_then(kind_target).is_some_and(|k| !p.listed_for(k)) {
        return false;
    }
    let ours = p.key.starts_with("app:");
    let chip_ok = match chip {
        1 => !ours && !menu::is_third_party(p),
        2 => menu::is_third_party(p),
        3 => ours,
        4 => p.hidden,
        _ => true,
    };
    chip_ok && (q.is_empty() || [&p.label, &p.name, &p.from].iter().any(|s| s.to_lowercase().contains(q)))
}

/// Does a list entry pass the search `q` (lower-case) and the chip?
pub fn entry_shown(e: &MenuEntry, q: &str, chip: usize, kind: Option<usize>) -> bool {
    let chip_ok = match chip {
        1 => e.source == Source::Windows,
        2 => false,
        3 => e.source != Source::Windows,
        4 => e.hidden,
        _ => true,
    };
    let kind_ok = match kind {
        Some(0) => e.show_for.files,
        Some(1) => e.show_for.folders,
        Some(2) => e.show_for.background,
        Some(3) => e.show_for.multiple,
        Some(4) => e.show_for.drives,
        _ => true,
    };
    chip_ok && kind_ok && (q.is_empty() || (!e.is_separator() && (e.name.to_lowercase().contains(q) || e.source.label().to_lowercase().contains(q))))
}

/// Which built-in group a learned item belongs to by origin.
fn origin_group(p: &menu::ItemPref) -> menu::Group {
    match p.from.as_str() {
        "Windows" => menu::Group::Windows,
        "Tsubame" => menu::Group::Tsubame,
        _ => menu::Group::Apps,
    }
}

/// The group a learned item is drawn in: its user group when that still
/// exists, else the one it belongs to by origin.
pub fn item_group(p: &menu::ItemPref, users: &[menu::UserGroup]) -> menu::GroupRef {
    if !p.group.is_empty() && users.iter().any(|u| u.id == p.group) {
        menu::GroupRef::User(p.group.clone())
    } else {
        menu::GroupRef::Builtin(origin_group(p))
    }
}

/// Items in group `g` (the count on its header row).
pub fn group_count(list: &[MenuEntry], items: &[menu::ItemPref], users: &[menu::UserGroup], g: &menu::GroupRef) -> usize {
    let n = match g {
        menu::GroupRef::Builtin(b) => {
            let entries = list.iter().filter(|e| e.parent.is_empty() && !e.is_separator() && !(e.source == Source::Windows && e.id == "windows.open-with"));
            match b {
                menu::Group::Tsubame => entries.filter(|e| !e.source.runs_program()).count(),
                menu::Group::Custom => entries.filter(|e| e.source.runs_program() && e.group.is_empty()).count(),
                _ => 0,
            }
        }
        menu::GroupRef::User(id) => list.iter().filter(|e| e.parent.is_empty() && !e.is_separator() && e.group == *id).count(),
    };
    n + items.iter().filter(|p| item_group(p, users) == *g).count()
}

/// Where dropping a learned item on a row of group `target` puts it: the user
/// group's id, or empty (back to its origin) when `target` is the group it
/// came from. `None` = not a place for it (another built-in group).
pub fn drop_group(p: &menu::ItemPref, target: &menu::GroupRef) -> Option<String> {
    match target {
        menu::GroupRef::User(id) => Some(id.clone()),
        menu::GroupRef::Builtin(b) => (*b == origin_group(p)).then(String::new),
    }
}

/// What a key does on a focused group header (the keyboard twins of the
/// double-click, the x and the grip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKey {
    Rename,
    Delete,
    /// one place up (-1) or down (+1)
    Move(isize),
}

/// The action for `key` on a group header. Rename and Delete only exist for
/// the user's own groups (`user`); every group can move.
pub fn group_key(key: Key, mods: Mods, user: bool) -> Option<GroupKey> {
    match key {
        Key::F(2) | Key::Enter if user && mods.none() => Some(GroupKey::Rename),
        Key::Delete if user && mods.none() => Some(GroupKey::Delete),
        Key::Up if mods.alt && !mods.ctrl && !mods.shift => Some(GroupKey::Move(-1)),
        Key::Down if mods.alt && !mods.ctrl && !mods.shift => Some(GroupKey::Move(1)),
        _ => None,
    }
}

/// `order` with group `k` moved one place by `dir`; `None` at either end.
pub fn move_group(order: &[menu::GroupRef], k: usize, dir: isize) -> Option<Vec<menu::GroupRef>> {
    let to = k.checked_add_signed(dir).filter(|&t| k < order.len() && t < order.len())?;
    let mut o = order.to_vec();
    o.swap(k, to);
    Some(o)
}

/// What each chip counts (the small number on the chip).
pub fn chip_counts(list: &[MenuEntry], items: &[menu::ItemPref], kind: Option<usize>) -> [usize; 5] {
    let mut n = [0usize; 5];
    for (c, slot) in n.iter_mut().enumerate() {
        *slot = list.iter().filter(|e| !e.is_separator() && entry_shown(e, "", c, kind)).count() + items.iter().filter(|p| item_shown_for(p, "", c, kind)).count();
    }
    n
}

/// Reset to defaults: the default list and group order, every learned item
/// shown under its own name again (the items stay known).
pub fn reset_menu(m: &mut tsubame_config::ContextMenu) {
    m.entries.clear();
    m.groups.clear();
    m.user_groups.clear();
    m.nss_group.clear();
    for p in &mut m.items {
        p.hidden = false;
        p.name.clear();
        p.group.clear();
        p.pin = false;
    }
}

/// Anything Reset to defaults would change.
pub fn menu_changed(m: &tsubame_config::ContextMenu) -> bool {
    !m.entries.is_empty() || !m.groups.is_empty() || !m.user_groups.is_empty() || m.items.iter().any(|p| p.hidden || p.pin || !p.name.is_empty() || !p.group.is_empty())
}

fn chip(pg: &mut Pg, id: Id, x: f32, y: f32, label: &str, count: Option<usize>, on: bool) -> (bool, Rect) {
    let t = pg.t;
    let cs = count.map(|c| c.to_string()).unwrap_or_default();
    let lw = pg.g.measure(label, t.font_small());
    let cw = if cs.is_empty() { 0.0 } else { pg.g.measure(&cs, t.font_small()) + 8.0 };
    let w = lw + cw + 20.0;
    let r = Rect::new(x, y, w, t.height_field - 4.0);
    let it = pg.ui.interact(id, r, Sense::click().cursor(Cursor::Hand));
    let fill = if on { t.selected } else if it.hovered { t.hover } else { t.field };
    pg.g.round(r, r.h / 2.0, fill);
    pg.g.round_stroke(r, r.h / 2.0, if on { t.accent } else { t.popup_border }, 1.0);
    pg.g.text(label, Rect::new(r.x + 10.0, r.y, lw + 2.0, r.h), t.font_small(), if on { t.accent } else { t.text });
    if !cs.is_empty() {
        pg.g.text(&cs, Rect::new(r.x + 10.0 + lw + 8.0, r.y, cw, r.h), t.font_small(), t.text_dim);
    }
    (it.clicked == Some(Button::Left), r)
}

fn tag(pg: &mut Pg, right: f32, y: f32, h: f32, label: &str, accent: bool, solid: bool) -> Rect {
    let t = pg.t;
    let tw = pg.g.measure(label, t.font_small()) + 12.0;
    let tr = Rect::new(right - tw, y, tw, h);
    if solid {
        pg.g.round(tr, t.radius_sm, t.accent);
        pg.g.text(label, tr, t.font_small().c(), t.on_accent);
    } else {
        pg.g.round_stroke(tr, t.radius_sm, t.popup_border, 1.0);
        pg.g.text(label, tr, t.font_small().c(), if accent { t.accent } else { t.text_dim });
    }
    tr
}

// ---- the row "..." menu, the pin cell and "Refresh from last menu" (mocks `ctxgap-*`)

/// The last Delete entry: the list before and after, and the entry's name.
struct Deleted {
    before: Vec<MenuEntry>,
    after: Vec<MenuEntry>,
    name: String,
}

/// The add form filled with `e`, saving into it.
fn edit_form(e: &MenuEntry) -> AddForm {
    AddForm { name: TextInput::new(&e.name), cmd: TextInput::new(&e.command), args: TextInput::new(&e.args), ext: TextInput::new(&e.extensions.join(", ")), show: e.show_for, err: None, edit: Some(e.id.clone()) }
}

/// "Name in menu" of user group `id`. Returns whether it changed.
pub fn set_group_name_shown(users: &mut [menu::UserGroup], id: &str, on: bool) -> bool {
    match users.iter_mut().find(|u| u.id == id) {
        Some(u) if u.show_name != on => {
            u.show_name = on;
            true
        }
        _ => false,
    }
}

/// The five "Show for" boxes of the add form (mock `ctxgap-kinds` C), in the
/// kind switch's order.
const SHOW_FOR: [&str; 5] = ["File", "Folder", "Empty area", "Several items", "Drive"];

fn show_for_flag(s: &mut ShowFor, i: usize) -> &mut bool {
    match i {
        0 => &mut s.files,
        1 => &mut s.folders,
        2 => &mut s.background,
        3 => &mut s.multiple,
        _ => &mut s.drives,
    }
}

fn show_for_row(pg: &mut Pg, show: &mut ShowFor, x: f32, y: f32) {
    let t = pg.t;
    pg.g.text("Show for", Rect::new(x, y, 90.0, 22.0), t.font_small(), t.text_dim);
    let mut cx = x + 96.0;
    for (i, l) in SHOW_FOR.iter().enumerate() {
        let on = *show_for_flag(show, i);
        let b = Rect::new(cx, y + 3.0, 16.0, 16.0);
        if tsubame_ui::checkbox(pg.ui, pg.g, ID_FORM.with(20 + i as u64), b, on) {
            *show_for_flag(show, i) = !on;
        }
        let lw = pg.g.measure(l, t.font_small());
        pg.g.text(l, Rect::new(b.r() + 6.0, y, lw + 2.0, 22.0), t.font_small(), t.text);
        cx = b.r() + 6.0 + lw + 18.0;
    }
}

/// What a row "..." menu is for.
#[derive(Clone, Debug, PartialEq)]
enum RowTarget {
    /// a list entry (its id)
    Entry(String),
    /// a learned item (its key)
    Item(String),
}

impl RowTarget {
    fn key(&self) -> &str {
        match self {
            RowTarget::Entry(k) | RowTarget::Item(k) => k,
        }
    }
}

/// An open row popup: its target, the kit's menu state and the button it came from.
struct RowPop {
    target: RowTarget,
    st: MenuState,
    btn: Rect,
    btn_id: Id,
}

/// The kit's edit menu opens against one row: this is the row.
struct RowSrc {
    name: String,
    facts: rowmenu::Facts,
}

const ROW_KEY: u64 = 1;

impl MenuSource for RowSrc {
    fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
        if path.is_empty() {
            out.push(MenuRow::item(ROW_KEY, &self.name));
        }
    }
    fn icon(&self, _key: u64) -> Option<(u32, u32, &[u32])> {
        None
    }
    fn edit_rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
        match path.split_last() {
            Some((&EDIT_GROUP, _)) => out.extend(rowmenu::group_rows(&self.facts)),
            Some(_) => out.extend(rowmenu::rows(&self.facts)),
            None => {}
        }
    }
}

/// A stable number for a row's id (a moved row keeps its button, so the keyboard focus follows it).
fn seed(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// The action a key runs on a row whose "..." button has the focus (F2, Del, Alt+Up / Down); None:
/// not a row key, or the action is off for that row.
pub fn button_key(key: Key, mods: Mods, f: &rowmenu::Facts) -> Option<rowmenu::Act> {
    use rowmenu::Act;
    let act = match key {
        Key::F(2) if mods.none() => Act::Rename,
        // Settings: Del deletes a custom row (Hide is the eye); Hide has no key here
        Key::Delete if mods.none() => Act::DeleteEntry,
        Key::Up if mods == Mods::ALT => Act::Up,
        Key::Down if mods == Mods::ALT => Act::Down,
        _ => return None,
    };
    (rowmenu::present(f, act) && rowmenu::state(f, act).0).then_some(act)
}

/// The facts of the row `t` stands for, from the data as it is now. None: the row is gone.
fn facts_for(t: &RowTarget, list: &[MenuEntry], m: &tsubame_config::ContextMenu) -> Option<rowmenu::Facts> {
    match t {
        RowTarget::Entry(id) => {
            let e = list.iter().find(|e| e.id == *id)?;
            let shown: Vec<&str> = list.iter().map(|e| e.id.as_str()).collect();
            Some(rowmenu::Facts::entry(e, ctxui::can_move(list, id, true, &shown), ctxui::can_move(list, id, false, &shown), false))
        }
        RowTarget::Item(key) => Some(rowmenu::Facts::learned(menu::item(&m.items, key)?, &m.user_groups, false, false)),
    }
}

/// The learned rows of each group in drawing order: pinned rows first, each part in its native order.
fn order_rows(items: &[menu::ItemPref], shown: &[usize], users: &[menu::UserGroup], order: &[menu::GroupRef]) -> Vec<(menu::GroupRef, Vec<usize>)> {
    order
        .iter()
        .map(|g| {
            let mine: Vec<usize> = shown.iter().copied().filter(|&i| item_group(&items[i], users) == *g).collect();
            (g.clone(), rowmenu::pinned_first(&mine, |i| items[i].pin))
        })
        .collect()
}

/// "Refresh from last menu": what the button and its status line are doing.
#[derive(Clone, Debug, Default, PartialEq)]
enum Refresh {
    #[default]
    Idle,
    /// working since `since`; `wait` is a test aid (`TSUBAME_REFRESH_DELAY_MS`), zero normally
    Running { since: Instant, wait: Duration },
    /// this many rows left the list
    Done(usize),
    UpToDate,
}

/// The running state is drawn only when the work takes longer than this.
const RUNNING_AFTER: Duration = Duration::from_millis(150);

fn refresh_outcome(n: usize) -> Refresh {
    if n == 0 { Refresh::UpToDate } else { Refresh::Done(n) }
}

fn running_visible(since: Instant, now: Instant) -> bool {
    now.saturating_duration_since(since) >= RUNNING_AFTER
}

/// The kind switch's entry as the menu's target kind.
fn kind_target(i: usize) -> Option<menu::Target> {
    match i {
        0 => Some(menu::Target::File),
        1 => Some(menu::Target::Folder),
        2 => Some(menu::Target::Background),
        3 => Some(menu::Target::Multiple),
        4 => Some(menu::Target::Drive),
        _ => None,
    }
}

/// Is the button on? A menu was shown, and for the kind the switch picks (no pick: any).
fn refresh_enabled(last: Option<&ctxui::LastMenu>, kind: Option<usize>) -> bool {
    match (last, kind) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(l), Some(i)) => kind_target(i) == Some(l.kind),
    }
}

/// What the button row shows.
#[derive(Clone, Debug, PartialEq)]
struct RefreshView {
    label: &'static str,
    status: String,
    /// dim second line next to the status ("The list is shorter by 12 rows")
    extra: String,
    /// the status is the green-ish "done" one
    ok: bool,
    /// the check icon in front of the status
    check: bool,
    busy: bool,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn refresh_view(r: &Refresh, last: Option<&ctxui::LastMenu>, now: Instant) -> RefreshView {
    const LABEL: &str = "Refresh from last menu";
    let idle = |status: String| RefreshView { label: LABEL, status, extra: String::new(), ok: false, check: false, busy: false };
    match r {
        Refresh::Running { since, .. } if running_visible(*since, now) => RefreshView { label: "Refreshing\u{2026}", status: "Comparing with the last menu\u{2026}".into(), extra: String::new(), ok: false, check: false, busy: true },
        Refresh::Done(n) => RefreshView { status: format!("{} updated", plural(*n, "item", "items")), extra: format!("The list is shorter by {}", plural(*n, "row", "rows")), ok: true, check: true, ..idle(String::new()) },
        Refresh::UpToDate => RefreshView { check: true, ..idle("Already up to date".into()) },
        _ => idle(last.map(|l| format!("Last menu: {}, {}", l.at, plural(l.items, "item", "items"))).unwrap_or_default()),
    }
}

/// The disabled button's tooltip.
fn refresh_tip(last: Option<&ctxui::LastMenu>) -> &'static str {
    if last.is_none() { "Right-click a file in Tsubame first. The button compares with that menu." } else { "The last menu was for another kind of right-click. Pick that kind (or none) to compare." }
}

/// Runs the compare now.
fn run_refresh(st: &mut State, cx: &mut Cx) {
    let n = ctxui::refresh_learned(&mut cx.prefs.context_menu);
    if n > 0 {
        cx.changed |= C_MENU;
    }
    st.refresh = refresh_outcome(n);
}

/// The second toolbar row's Refresh button, its status and the busy line. `x` is where the
/// button goes, `limit` the right end of the status text.
fn refresh_row(pg: &mut Pg, st: &mut State, cx: &mut Cx, ty: f32, x: f32, limit: f32) {
    let t = pg.t;
    let now = Instant::now();
    let last = ctxui::last_menu();
    let on = refresh_enabled(last.as_ref(), st.kind);
    if let Refresh::Running { since, wait } = st.refresh
        && now.saturating_duration_since(since) >= wait
    {
        run_refresh(st, cx);
    }
    let view = refresh_view(&st.refresh, last.as_ref(), now);
    let (hit, r) = button(pg.ui, pg.g, ID_TB.with(10), x, ty, view.label, Look::Normal, on && !view.busy, false);
    if hit {
        let wait = std::env::var("TSUBAME_REFRESH_DELAY_MS").ok().and_then(|v| v.parse().ok()).map(Duration::from_millis).unwrap_or_default();
        st.refresh = Refresh::Running { since: now, wait };
        if wait.is_zero() {
            run_refresh(st, cx);
        }
    }
    if matches!(st.refresh, Refresh::Running { .. }) {
        pg.ui.request_repaint();
    }
    let view = refresh_view(&st.refresh, last.as_ref(), now);
    let mut sx = r.r() + 12.0;
    let w = (limit - sx).max(0.0);
    if view.check {
        pg.g.text("\u{E73E}", Rect::new(sx, ty, 16.0, t.height_button), f(13.0).icon().c(), if view.ok { t.accent } else { t.text_dim });
        sx += 20.0;
    }
    if view.ok {
        let tw = pg.g.measure(&view.status, t.font_small().b());
        pg.g.text(&view.status, Rect::new(sx, ty, tw + 4.0, t.height_button), t.font_small().b(), t.accent);
        let ex = fit(pg.g, &view.extra, t.font_small(), (w - tw - 28.0).max(0.0));
        pg.g.text(&ex, Rect::new(sx + tw + 12.0, ty, (w - tw - 12.0).max(0.0), t.height_button), t.font_small(), t.text_dim);
    } else {
        let s = fit(pg.g, &view.status, t.font_small(), w);
        pg.g.text(&s, Rect::new(sx, ty, w, t.height_button), t.font_small(), t.text_dim);
    }
    if view.busy {
        // the 2 px line: a bar sliding along the row
        let ly = ty + t.height_button + 3.0;
        pg.g.fill(Rect::new(pg.x, ly, pg.w, 2.0), t.border);
        let ms = if let Refresh::Running { since, .. } = st.refresh { now.saturating_duration_since(since).as_millis() as f32 } else { 0.0 };
        let phase = ms % 1400.0 / 1400.0;
        let bw = pg.w * 0.36;
        let x0 = (pg.x + (pg.w + bw) * phase - bw).max(pg.x);
        let x1 = (pg.x + (pg.w + bw) * phase).min(pg.x + pg.w);
        pg.g.fill(Rect::new(x0, ly, (x1 - x0).max(0.0), 2.0), t.accent);
    }
    if !on && r.contains_pt(pg.ui.mouse()) && pg.ui.mouse_in() {
        st.tip = Some((pg.ui.mouse(), refresh_tip(last.as_ref()).into()));
    }
}

/// The row "..." button: Tab-focusable, Enter / Space / Shift+F10 open the popup, F2 / Del /
/// Alt+Up / Down on it run the action at once. `facts` is only asked when a key needs it.
fn dots_row(pg: &mut Pg, st: &mut State, r: Rect, target: &RowTarget, facts: &dyn Fn() -> rowmenu::Facts) {
    let t = pg.t;
    let id = ID_DOTS.with(seed(target.key()));
    let it = pg.ui.interact(id, r, Sense::click().focusable().cursor(Cursor::Hand));
    let open_here = st.pop.as_ref().is_some_and(|p| p.btn_id == id);
    let focused = pg.ui.has_focus(id) && pg.ui.window_focused();
    if open_here {
        pg.g.round(r, t.radius_sm, t.selected);
    } else if it.hovered {
        pg.g.round(r, t.radius_sm, t.hover);
    }
    if focused {
        pg.g.round_stroke(r.pad(-1.0), t.radius_sm, t.focus, 1.0);
        // Alt+Up / Alt+Down move the row here, not "Up one folder"
        pg.ui.claimed.extend([(Key::Up, Mods::ALT), (Key::Down, Mods::ALT)]);
    }
    pg.g.text("\u{E712}", r, f(14.0).icon().c(), if it.hovered || open_here || focused { t.text } else { t.text_dim });
    let mut open = (it.clicked == Some(Button::Left)).then_some(false);
    for (qi, ev) in pg.ui.key_input(id) {
        if let Event::KeyDown { key, mods, .. } = ev {
            if (matches!(key, Key::Enter | Key::Space) && mods.none()) || (key == Key::F(10) && mods == Mods::SHIFT) {
                pg.ui.consume(qi);
                open = Some(true);
            } else if let Some(a) = button_key(key, mods, &facts()) {
                pg.ui.consume(qi);
                pg.ui.request_repaint();
                st.act = Some((target.clone(), a.key()));
            }
        }
    }
    if let Some(kb) = open {
        if open_here {
            // the button toggles its popup
            st.pop = None;
            pg.ui.close_all_popups();
        } else {
            let mut ms = MenuState::new(Pt { x: r.x, y: r.y }, kb);
            ms.del_deletes = true;
            ms.external_rename = true;
            st.pop = Some(RowPop { target: target.clone(), st: ms, btn: r, btn_id: id });
        }
        pg.ui.request_repaint();
    }
}

/// The pin cell of a learned row: empty, dim on hover, accent when pinned. True: toggled.
fn pin_cell(pg: &mut Pg, st: &mut State, r: Rect, key: &str, pinned: bool, hidden: bool, group: &str) -> bool {
    let t = pg.t;
    let id = ID_PIN.with(seed(key));
    let it = pg.ui.interact(id, r, Sense::click().focusable().cursor(Cursor::Hand));
    let focused = pg.ui.has_focus(id) && pg.ui.window_focused();
    let mut hit = it.clicked == Some(Button::Left);
    for (qi, ev) in pg.ui.key_input(id) {
        if let Event::KeyDown { key: Key::Enter | Key::Space, mods, .. } = ev
            && mods.none()
        {
            pg.ui.consume(qi);
            hit = true;
        }
    }
    if it.hovered {
        pg.g.round(r, t.radius_sm, t.hover);
    }
    if focused {
        pg.g.round_stroke(r.pad(-1.0), t.radius_sm, t.focus, 1.0);
    }
    let col = match (pinned, hidden) {
        (true, true) => Some(t.text_disabled),
        (true, false) => Some(t.accent),
        (false, _) if it.hovered || focused => Some(t.text_dim),
        _ => None,
    };
    if let Some(c) = col {
        pg.g.text("\u{E718}", r, f(13.0).icon().c(), c);
    }
    if it.hovered {
        st.tip = Some((pg.ui.mouse(), if pinned { "Unpin".into() } else { format!("Pin to top of \u{201c}{group}\u{201d}") }));
    }
    if hit {
        pg.ui.request_repaint();
    }
    hit
}

/// Runs the popup row `k` on `target`. True: the entry list changed.
fn apply(st: &mut State, cx: &mut Cx, list: &mut Vec<MenuEntry>, f: &rowmenu::Facts, target: &RowTarget, k: u64) -> bool {
    match target {
        RowTarget::Entry(id) => {
            let Some(i) = list.iter().position(|e| e.id == *id) else { return false };
            match k {
                EDIT_HIDE => {
                    list[i].hidden = !list[i].hidden;
                    true
                }
                EDIT_RENAME => {
                    st.edit = Some((id.clone(), TextInput::new(&list[i].name)));
                    st.edit_fresh = true;
                    st.sel = Some(id.clone());
                    false
                }
                EDIT_UP | EDIT_DOWN => {
                    let shown: Vec<String> = list.iter().map(|e| e.id.clone()).collect();
                    let shown: Vec<&str> = shown.iter().map(String::as_str).collect();
                    ctxui::move_entry_by(list, id, k == EDIT_UP, &shown)
                }
                EDIT_ENTRY if rowmenu::present(f, rowmenu::Act::EditEntry) => {
                    st.add = Some(Box::new(edit_form(&list[i])));
                    st.msg = None;
                    st.sel = Some(id.clone());
                    false
                }
                EDIT_DELETE if rowmenu::present(f, rowmenu::Act::DeleteEntry) => {
                    let before = list.clone();
                    let name = list[i].name.clone();
                    let gone = menu::delete_entry(list, i);
                    if gone {
                        st.deleted = Some(Deleted { before, after: list.clone(), name });
                    }
                    gone
                }
                _ => false,
            }
        }
        RowTarget::Item(key) => {
            let m = &mut cx.prefs.context_menu;
            let Some(p) = menu::item(&m.items, key).cloned() else { return false };
            match k {
                EDIT_HIDE => {
                    ctxui::set_hidden(m, key, &p.label, !p.hidden);
                    cx.changed |= C_MENU;
                }
                EDIT_RENAME => {
                    st.iedit = Some((key.clone(), TextInput::new(if p.name.is_empty() { &p.label } else { &p.name })));
                    st.edit_fresh = true;
                    st.sel = Some(key.clone());
                }
                EDIT_PIN => {
                    if ctxui::set_pin(m, key, &p.label, !p.pin) {
                        cx.changed |= C_MENU;
                    }
                }
                k if k == EDIT_GROUP_NEW || k == EDIT_GROUP_NATIVE || k >= EDIT_GROUP_BASE => {
                    if let Some(pick) = rowmenu::group_pick(f, k) {
                        let (gid, fresh) = match pick {
                            rowmenu::GroupPick::Group(id) => (id, false),
                            rowmenu::GroupPick::Native => (String::new(), false),
                            rowmenu::GroupPick::New => {
                                let id = menu::add_group(&mut m.user_groups, &m.groups, "");
                                st.gedit = Some((id.clone(), TextInput::new("New group")));
                                st.edit_fresh = true;
                                (id, true)
                            }
                        };
                        if menu::move_item_to_group(&mut m.items, &m.user_groups, key, &gid) || fresh {
                            cx.changed |= C_MENU;
                        }
                    }
                }
                _ => {}
            }
            false
        }
    }
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    st.tip = None;
    pg.heading("Context menu", "");
    let t = pg.t;
    for l in wrap(pg.g, "The menu that opens when you right-click in Tsubame. It lists everything the right-click can contain: Tsubame\u{2019}s own entries, Windows\u{2019} items and the entries added by installed programs. Hide, rename, reorder and group them.", t.font(), pg.w) {
        pg.g.text(&l, Rect::new(pg.x, pg.y, pg.w, 18.0), t.font(), t.text_dim);
        pg.y += 19.0;
    }
    pg.y += 8.0;
    pg.note("\u{E7BA}", "Windows\u{2019} own items cannot be removed from the system. Hiding or renaming an item here only changes Tsubame\u{2019}s menu: File Explorer and other programs still show it, and uninstalling a program removes its items for good. Entries marked Custom are ours and can be deleted.", true);
    let mut list = menu::effective(&cx.prefs.context_menu.entries);
    let mut modified = false;

    // New: what the menus learned since the last visit; the flags clear now
    if st.new_shown.is_none() {
        let keys: Vec<String> = cx.prefs.context_menu.items.iter().filter(|p| p.new).map(|p| p.key.clone()).collect();
        if !keys.is_empty() {
            for p in &mut cx.prefs.context_menu.items {
                p.new = false;
            }
            cx.changed |= C_MENU;
        }
        st.new_shown = Some(keys);
    }
    // from the menu: search for the row and select it
    if let Some(key) = st.focus.take() {
        let label = menu::item(&cx.prefs.context_menu.items, &key).map(|p| if p.name.is_empty() { p.label.clone() } else { p.name.clone() }).or_else(|| list.iter().find(|e| e.id == key).map(|e| e.name.clone()));
        if let Some(l) = label {
            st.search.set_text(&l);
        }
        st.sel = Some(key);
    }
    if let Some(id) = st.edit_entry.take()
        && let Some(e) = list.iter().find(|e| e.id == id && e.source.runs_program() && !e.is_submenu() && !e.is_separator())
    {
        st.add = Some(Box::new(edit_form(e)));
    }

    // kind switch: the list for one kind of right-click (click again = every kind)
    let ty = pg.y;
    let mut cxp = pg.x;
    for (i, l) in KINDS.iter().enumerate() {
        let (hit, r) = chip(pg, ID_KIND.with(i as u64), cxp, ty + 2.0, l, None, st.kind == Some(i));
        if hit {
            st.kind = if st.kind == Some(i) { None } else { Some(i) };
        }
        cxp = r.r() + 6.0;
    }
    let hint = if st.kind.is_some() { "Entries that show for this right-click. Windows' items for File, Folder and Empty area are listed together; Several items and Drive list what their own menus showed." } else { "Pick a kind to see which entries show for it." };
    pg.g.text(hint, Rect::new(cxp + 8.0, ty, (pg.x + pg.w - cxp - 8.0).max(0.0), t.height_field), t.font_small(), t.text_dim);
    pg.y += t.height_field + 6.0;

    // search and chips
    let ty = pg.y;
    st.search.pad_left = 22.0;
    let sr = Rect::new(pg.x, ty, 300.0_f32.min(pg.w - 320.0).max(160.0), t.height_field);
    if st.want_search {
        st.want_search = false;
        pg.ui.set_focus(Some(ID_SEARCH));
        st.search.select_all();
    }
    text_input(pg.ui, pg.g, ID_SEARCH, sr, &mut st.search, "Search items\u{2026}");
    pg.g.text("\u{E721}", Rect::new(sr.x + 6.0, sr.y, 16.0, sr.h), f(12.0).icon().c(), t.text_dim);
    if st.search.text.is_empty() && !pg.ui.has_focus(ID_SEARCH) {
        let kw = pg.g.measure("Ctrl+F", t.font_mono()) + 4.0;
        pg.g.text("Ctrl+F", Rect::new(sr.r() - kw - 8.0, sr.y, kw, sr.h), t.font_mono(), t.text_disabled);
    }
    if pg.ui.has_focus(ID_SEARCH) && pg.ui.window_focused() {
        pg.ui.mark_text_focus();
    }
    let counts = chip_counts(&list, &cx.prefs.context_menu.items, st.kind);
    let mut cxp = sr.r() + 12.0;
    for (i, l) in CHIPS.iter().enumerate() {
        let (hit, r) = chip(pg, ID_CHIP.with(i as u64), cxp, ty + 2.0, l, Some(counts[i]), st.chip == i);
        if hit {
            st.chip = i;
        }
        cxp = r.r() + 6.0;
    }
    pg.y += t.height_field + 10.0;
    let q = st.search.text.trim().to_lowercase();
    let filtered = !q.is_empty() || st.chip != 0 || st.kind.is_some();

    // toolbar
    let ty = pg.y;
    let mut x = pg.x;
    let (add, r) = button(pg.ui, pg.g, ID_TB.with(1), x, ty, "+  Add custom entry", Look::Primary, true, false);
    x = r.r() + 8.0;
    let (sub, r) = button(pg.ui, pg.g, ID_TB.with(2), x, ty, "New submenu", Look::Normal, true, false);
    x = r.r() + 8.0;
    let (sep, r) = button(pg.ui, pg.g, ID_TB.with(3), x, ty, "Add separator", Look::Normal, true, false);
    x = r.r() + 8.0;
    let (newg, _) = button(pg.ui, pg.g, ID_TB.with(6), x, ty, "New group", Look::Normal, true, false);
    // row 2: import, refresh from the last menu, reset
    let ty2 = ty + t.height_button + 8.0;
    let (imp, ir) = button(pg.ui, pg.g, ID_TB.with(4), pg.x, ty2, "Import from Nilesoft Shell\u{2026}", Look::Normal, true, false);
    let reset_label = if st.confirm_reset { "Click again to reset" } else { "Reset to defaults" };
    let reset_w = pg.g.measure(reset_label, t.font().b()) + 28.0;
    refresh_row(pg, st, cx, ty2, ir.r() + 8.0, pg.x + pg.w - reset_w - 12.0);
    let (reset, _) = button(pg.ui, pg.g, ID_TB.with(5), pg.x + pg.w, ty2, reset_label, if st.confirm_reset { Look::DangerFill } else { Look::Danger }, menu_changed(&cx.prefs.context_menu), true);
    pg.y = ty2 + t.height_button + 14.0;
    if (add || sub || sep || newg || imp || reset) && !matches!(st.refresh, Refresh::Running { .. }) {
        st.refresh = Refresh::Idle;
    }
    if add {
        st.add = if st.add.is_some() { None } else { Some(Box::new(AddForm::blank())) };
        st.msg = None;
    }
    if imp && st.picker.is_none() {
        st.picker = Some(nss_pick::Picker::open());
        st.add = None;
        st.msg = None;
    }
    // the file dialog is open on its own thread: wait for it
    if let Some(pk) = &st.picker {
        match pk.poll() {
            nss_pick::Poll::Open => pg.ui.request_repaint(),
            nss_pick::Poll::Cancelled => st.picker = None,
            nss_pick::Poll::Chosen(path) => {
                st.picker = None;
                modified |= run_import(st, &mut list, &path, &cx.prefs.context_menu);
            }
        }
    }
    if newg {
        let m = &mut cx.prefs.context_menu;
        let id = menu::add_group(&mut m.user_groups, &m.groups, "");
        st.gedit = Some((id, TextInput::new("New group")));
        st.edit_fresh = true;
        st.msg = None;
        cx.changed |= C_MENU;
    }
    if sub {
        let id = menu::next_id(&list, "custom");
        list.push(MenuEntry { id, kind: EntryKind::Submenu, source: Source::Custom, name: "New submenu".into(), ..Default::default() });
        modified = true;
    }
    if sep {
        let id = menu::next_id(&list, "sep");
        list.push(MenuEntry::separator(&id));
        modified = true;
    }
    if reset {
        if st.confirm_reset {
            list = menu::default_entries();
            reset_menu(&mut cx.prefs.context_menu);
            cx.changed |= C_MENU;
            st.confirm_reset = false;
            st.msg = Some(("Menu reset to the default.".into(), false));
        } else {
            st.confirm_reset = true;
        }
    }

    // add form
    if let Some(form) = st.add.as_mut() {
        let fh = 4.0 * 36.0 + 12.0 + 44.0 + 30.0;
        let r = Rect::new(pg.x, pg.y, pg.w, fh + 24.0);
        pg.g.round(r, t.radius_lg, t.panel);
        pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
        let fx = r.x + 16.0;
        let fw = (r.w - 32.0).min(620.0);
        let mut y = r.y + 14.0;
        label_field(pg, ID_FORM.with(1), "Name", &mut form.name, fx, y, fw, "Open in VS Code");
        y += 36.0;
        label_field(pg, ID_FORM.with(2), "Program", &mut form.cmd, fx, y, fw, "C:\\Program Files\\Microsoft VS Code\\Code.exe");
        y += 36.0;
        label_field(pg, ID_FORM.with(3), "Arguments", &mut form.args, fx, y, fw, "\"{path}\"");
        y += 36.0;
        label_field(pg, ID_FORM.with(4), "File types", &mut form.ext, fx, y, fw, "optional: txt, md (empty = every file)");
        y += 36.0;
        show_for_row(pg, &mut form.show, fx, y);
        y += 30.0;
        pg.g.text("{path} first selected item   {selection} every selected item   {dir} this folder   {name} {ext} {parent}", Rect::new(fx, y - 2.0, r.w - 32.0, 16.0), t.font_small(), t.text_dim);
        y += 20.0;
        let (ok, rr) = button(pg.ui, pg.g, ID_FORM.with(10), fx, y, if form.edit.is_some() { "Save" } else { "Add to menu" }, Look::Primary, true, false);
        let (cancel, _) = button(pg.ui, pg.g, ID_FORM.with(11), rr.r() + 8.0, y, "Cancel", Look::Normal, true, false);
        if let Some(e) = &form.err {
            pg.g.text(e, Rect::new(rr.r() + 100.0, y, r.w - 240.0, t.height_button), t.font_small(), t.danger);
        }
        let mut done = false;
        if ok {
            let args = if form.args.text.trim().is_empty() { "\"{path}\"".to_string() } else { form.args.text.clone() };
            match new_entry(if form.edit.is_some() { &[] } else { &list }, &form.name.text, &form.cmd.text, &args, &form.ext.text) {
                Ok(e) => {
                    if let Some(eid) = &form.edit {
                        if let Some(old) = list.iter_mut().find(|o| o.id == *eid) {
                            old.name = e.name;
                            old.command = e.command;
                            old.args = e.args;
                            old.extensions = e.extensions;
                            old.show_for = form.show;
                        }
                        st.msg = Some(("Saved.".into(), false));
                    } else {
                        list.push(MenuEntry { show_for: form.show, ..e });
                        st.msg = Some(("Added. It shows at the end of the menu.".into(), false));
                    }
                    modified = true;
                    done = true;
                }
                Err(e) => form.err = Some(e),
            }
        }
        if cancel {
            done = true;
        }
        pg.y += r.h + 12.0;
        if done {
            st.add = None;
        }
    }
    // Nilesoft import, mock 6b: 1 nothing found (the hint), 2 file detected, 3 imported (report)
    if st.review.is_some() {
        let users = cx.prefs.context_menu.user_groups.clone();
        let act = nss_sel::draw(pg, st.review.as_mut().unwrap(), &users);
        match act {
            nss_sel::Act::Cancel => {
                st.review = None;
                st.msg = Some(("Import cancelled. Nothing was added.".into(), false));
            }
            nss_sel::Act::Import => {
                let sel = st.review.take().unwrap();
                let rep = sel.commit(&mut list, &mut cx.prefs.context_menu);
                st.msg = None;
                st.report = Some(rep);
                modified = true;
                cx.changed |= C_MENU;
            }
            nss_sel::Act::None => {}
        }
    }
    if st.report.is_none() && st.review.is_none() {
        if st.detected.is_none() {
            st.detected = Some(nss::detect(&list));
        }
        match st.detected.clone().flatten() {
            None => {
                pg.g.text("Import from Nilesoft Shell opens a file picker for shell.nss. The file is only read, never changed.", Rect::new(pg.x + 2.0, pg.y, pg.w, 16.0), t.font_small(), t.text_dim);
                pg.y += 22.0;
            }
            Some(d) => {
                let file = d.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "shell.nss".into());
                let text = format!("Nilesoft Shell found in {}: {file}, {} {} ({} can be imported, {} {} skipped).", d.place, d.found, if d.found == 1 { "entry" } else { "entries" }, d.importable, d.skipped, if d.skipped == 1 { "is" } else { "are" });
                let r = Rect::new(pg.x, pg.y, pg.w, 44.0);
                pg.g.round(r, t.radius_md, t.panel);
                pg.g.round_stroke(r, t.radius_md, t.accent, 1.0);
                pg.g.text("\u{E946}", Rect::new(r.x + 14.0, r.y, 18.0, r.h), f(14.0).icon().c(), t.accent);
                let (go, br) = button(pg.ui, pg.g, ID_TB.with(7), r.r() - 10.0, r.y + (r.h - t.height_button) / 2.0, "Review and import\u{2026}", Look::Primary, d.importable > 0, true);
                let shown = fit(pg.g, &text, t.font_small(), (br.x - r.x - 56.0).max(40.0));
                pg.g.text(&shown, Rect::new(r.x + 40.0, r.y, br.x - r.x - 48.0, r.h), t.font_small(), t.text);
                pg.y += r.h + 10.0;
                if go {
                    modified |= run_import(st, &mut list, &d.path, &cx.prefs.context_menu);
                }
            }
        }
    }
    if let Some(rep) = st.report.clone() {
        let shown_skips = rep.skipped.len().min(6);
        let extra = rep.skipped.len() - shown_skips;
        let r = Rect::new(pg.x, pg.y, pg.w, 64.0 + shown_skips as f32 * 38.0 + if extra > 0 { 20.0 } else { 0.0 });
        pg.g.round(r, t.radius_lg, t.panel);
        pg.g.round_stroke(r, t.radius_lg, t.border, 1.0);
        pg.g.text("\u{E73E}", Rect::new(r.x + 14.0, r.y + 10.0, 22.0, 22.0), f(18.0).icon().c(), t.accent);
        let (undo, ur) = button(pg.ui, pg.g, ID_TB.with(8), r.r() - 12.0, r.y + 10.0, "Undo import", Look::Normal, true, true);
        let (show, sr2) = button(pg.ui, pg.g, ID_TB.with(9), ur.x - 8.0, r.y + 10.0, "Show imported", Look::Normal, rep.imported > 0, true);
        let head = format!("{}: {} imported, {} skipped", rep.file, rep.imported, rep.skipped.len());
        let hw = (sr2.x - r.x - 56.0).max(40.0);
        let hs = fit(pg.g, &head, t.font().b(), hw);
        pg.g.text(&hs, Rect::new(r.x + 44.0, r.y + 8.0, hw, 20.0), t.font().b(), t.text);
        let place = if rep.group_name.is_empty() { "at the end of the menu".to_string() } else { format!("in the group \u{201C}{}\u{201D}", rep.group_name) };
        let sub = fit(pg.g, &format!("Added {place}, tagged \u{201C}From Nilesoft\u{201D}. The original file was not changed."), t.font_small(), hw);
        pg.g.text(&sub, Rect::new(r.x + 44.0, r.y + 30.0, hw, 16.0), t.font_small(), t.text_dim);
        let mut y = r.y + 58.0;
        for s in rep.skipped.iter().take(shown_skips) {
            pg.g.fill(Rect::new(r.x + 12.0, y - 2.0, r.w - 24.0, 1.0), t.border);
            pg.g.text("\u{E7BA}", Rect::new(r.x + 14.0, y + 2.0, 18.0, 16.0), f(13.0).icon().c(), t.text_dim);
            let w = r.w - 60.0;
            let l1 = fit(pg.g, &format!("Line {}: {}", s.line, s.text), t.font_small(), w);
            pg.g.text(&l1, Rect::new(r.x + 40.0, y, w, 16.0), t.font_small(), t.text);
            let l2 = fit(pg.g, &s.reason, t.font_small(), w);
            pg.g.text(&l2, Rect::new(r.x + 40.0, y + 17.0, w, 16.0), t.font_small(), t.text_dim);
            y += 38.0;
        }
        if extra > 0 {
            pg.g.text(&format!("and {extra} more skipped"), Rect::new(r.x + 40.0, y, r.w - 60.0, 16.0), t.font_small(), t.text_dim);
        }
        pg.y += r.h + 12.0;
        if undo {
            let m = &mut cx.prefs.context_menu;
            rep.undo_all(&mut list, &mut m.user_groups, &mut m.items);
            cx.changed |= C_MENU;
            modified = true;
            st.report = None;
            st.detected = None;
            st.msg = Some(("Import undone. Your menu is as it was.".into(), false));
        } else if show {
            st.chip = 0;
            st.kind = None;
            st.search.set_text("From Nilesoft");
        }
    }
    if let Some((m, bad)) = &st.msg {
        let lines = wrap(pg.g, m, t.font_small(), pg.w);
        for l in lines.iter().take(2) {
            pg.g.text(l, Rect::new(pg.x + 2.0, pg.y, pg.w, 16.0), t.font_small(), if *bad { t.danger } else { t.text_dim });
            pg.y += 16.0;
        }
        pg.y += 6.0;
    }
    // Undo for Delete entry (only while nothing else changed the list)
    if st.deleted.as_ref().is_some_and(|d| d.after != list) {
        st.deleted = None;
    }
    if let Some(d) = &st.deleted {
        let msg = format!("Deleted \u{201c}{}\u{201d}.", d.name);
        let w = pg.g.measure(&msg, t.font_small());
        pg.g.text(&msg, Rect::new(pg.x + 2.0, pg.y, w + 4.0, t.height_button), t.font_small(), t.text_dim);
        let (undo, _) = button(pg.ui, pg.g, ID_TB.with(12), pg.x + w + 16.0, pg.y, "Undo", Look::Normal, true, false);
        pg.y += t.height_button + 6.0;
        if undo && let Some(d) = st.deleted.take() {
            list = d.before;
            modified = true;
        }
    }

    let rh = t.height_menu_edit_row;

    // groups, top to bottom (the grip reorders them; New group adds one)
    pg.section("Groups (menu order, top to bottom)");
    let order = menu::full_order(&cx.prefs.context_menu.groups, &cx.prefs.context_menu.user_groups);
    let gcard = Rect::new(pg.x, pg.y, pg.w, order.len() as f32 * rh + 2.0);
    pg.g.round(gcard, t.radius_lg, t.panel);
    pg.g.round_stroke(gcard, t.radius_lg, t.border, 1.0);
    let mut gpress = None;
    let mut gdelete: Option<String> = None;
    let mut gstart: Option<String> = None;
    let mut gcommit: Option<(String, String)> = None;
    let mut gcancel = false;
    let mut gname: Option<(String, bool)> = None;
    // keyboard: a move (index, direction) and the row that takes the focus afterwards
    let mut gmove: Option<(usize, isize)> = None;
    let mut gfocus: Option<usize> = None;
    for (k, g) in order.iter().enumerate() {
        let r = Rect::new(gcard.x + 1.0, gcard.y + 1.0 + k as f32 * rh, gcard.w - 2.0, rh);
        if k > 0 {
            pg.g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        }
        if st.gdrag == Some(k) {
            pg.g.fill(r, t.selected);
        }
        let uid = if let menu::GroupRef::User(id) = g { Some(id.clone()) } else { None };
        let rid = ID_GROW.with(k as u64);
        let row = pg.ui.interact(rid, r, Sense::click().focusable().cursor(Cursor::Arrow));
        if pg.ui.has_focus(rid) && pg.ui.window_focused() {
            pg.g.round_stroke(r.pad(-2.0), t.radius_sm, t.focus, 1.0);
            // Alt+Up / Alt+Down move the group here, not "Up one folder"
            pg.ui.claimed.extend([(Key::Up, Mods::ALT), (Key::Down, Mods::ALT)]);
        }
        for (qi, ev) in pg.ui.key_input(rid) {
            if let Event::KeyDown { key, mods, .. } = ev
                && let Some(act) = group_key(key, mods, uid.is_some())
            {
                pg.ui.consume(qi);
                pg.ui.request_repaint();
                match act {
                    GroupKey::Rename => gstart = uid.clone(),
                    GroupKey::Delete => {
                        gdelete = uid.clone();
                        // the focus stays at this place (the one before it at the end)
                        gfocus = Some(k.min(order.len().saturating_sub(2)));
                    }
                    GroupKey::Move(d) => gmove = Some((k, d)),
                }
            }
        }
        let gr = Rect::new(r.x + 6.0, r.y, 18.0, r.h);
        let gi = pg.ui.interact(ID_GROUP.with(k as u64), gr, Sense::click().cursor(Cursor::Hand));
        for (dx, dy) in [(0.0, -4.0), (4.0, -4.0), (0.0, 0.0), (4.0, 0.0), (0.0, 4.0), (4.0, 4.0)] {
            pg.g.circle(gr.x + 7.0 + dx, r.y + r.h / 2.0 + dy, 1.0, if gi.hovered { t.text } else { t.text_disabled });
        }
        if gi.pressed.is_some() {
            gpress = Some(k);
        }
        let name: String = match g {
            menu::GroupRef::Builtin(b) => b.label().to_string(),
            menu::GroupRef::User(id) => cx.prefs.context_menu.user_groups.iter().find(|u| u.id == *id).map(|u| u.name.clone()).unwrap_or_default(),
        };
        let mut rx = r.r() - 10.0;
        // user groups: a x to delete (their items go back to where they came from)
        if let Some(id) = &uid {
            let dr = Rect::new(rx - 22.0, r.y + 3.0, 22.0, r.h - 6.0);
            let di = pg.ui.interact(ID_GDEL.with(k as u64), dr, Sense::click().cursor(Cursor::Hand));
            if di.hovered {
                pg.g.round(dr, t.radius_sm, t.hover);
            }
            pg.g.text("\u{E711}", dr, f(11.0).icon().c(), if di.hovered { t.danger } else { t.text_dim });
            if di.clicked == Some(Button::Left) {
                gdelete = Some(id.clone());
            }
            rx -= 30.0;
        }
        let tr = tag(pg, rx, r.y + 5.0, r.h - 10.0, if uid.is_some() { "Your group" } else { "Group" }, uid.is_some(), false);
        // "Name in menu": the group's block starts with its name (variant B of the mock `ctxgap-groupname`)
        if let Some(u) = uid.as_ref().and_then(|id| cx.prefs.context_menu.user_groups.iter().find(|u| u.id == *id)) {
            let (tw, th) = (t.layout_toggle_width, t.layout_toggle_height);
            let tg = Rect::new(tr.x - 14.0 - tw, r.y + (r.h - th) / 2.0, tw, th);
            let lw = pg.g.measure("Name in menu", t.font_small());
            pg.g.text("Name in menu", Rect::new(tg.x - 8.0 - lw, r.y, lw + 2.0, r.h), t.font_small(), t.text_dim);
            if toggle(pg.ui, pg.g, ID_GNAME.with(k as u64), tg, u.show_name) {
                gname = Some((u.id.clone(), !u.show_name));
            }
        }
        let editing = uid.as_ref().is_some_and(|id| st.gedit.as_ref().is_some_and(|(e, _)| e == id));
        if editing {
            let nr = Rect::new(gr.r() + 6.0, r.y + 2.0, 220.0, r.h - 4.0);
            let fresh = st.edit_fresh;
            let (id, input) = st.gedit.as_mut().unwrap();
            if fresh {
                st.edit_fresh = false;
                pg.ui.set_focus(Some(ID_GEDIT));
                input.select_all();
            }
            let resp = text_input(pg.ui, pg.g, ID_GEDIT, nr, input, "Group name");
            pg.ui.mark_text_focus();
            if resp.submitted || (!resp.cancelled && !pg.ui.has_focus(ID_GEDIT) && pg.ui.active().is_none() && !fresh && pg.ui.frame() > 2) {
                gcommit = Some((id.clone(), input.text.trim().to_string()));
            } else if resp.cancelled {
                gcancel = true;
            }
            // Enter / Esc hand the keyboard back to the header (a click elsewhere does not)
            if resp.submitted || resp.cancelled {
                gfocus = Some(k);
            }
        } else {
            let nw = pg.g.measure(&name, t.font_md().b());
            pg.g.text(&name, Rect::new(gr.r() + 6.0, r.y, nw + 4.0, r.h), t.font_md().b(), t.text);
            let n = group_count(&list, &cx.prefs.context_menu.items, &cx.prefs.context_menu.user_groups, g);
            pg.g.text(&format!("{n} item{}", if n == 1 { "" } else { "s" }), Rect::new(gr.r() + nw + 16.0, r.y, 120.0, r.h), t.font_small(), t.text_dim);
            if row.pressed.is_some_and(|p| p.double) {
                gstart = uid.clone();
            }
        }
    }
    if let Some(k) = gpress {
        st.gdrag = Some(k);
    }
    if let Some(from) = st.gdrag {
        let rel = (pg.ui.mouse().y - gcard.y - 1.0) / rh;
        let to = (rel + 0.5).floor().clamp(0.0, order.len() as f32) as usize;
        let ly = gcard.y + 1.0 + to as f32 * rh;
        pg.g.fill(Rect::new(gcard.x + 6.0, ly - 1.0, gcard.w - 12.0, 2.0), t.accent);
        if pg.ui.active().is_none() {
            st.gdrag = None;
            let mut o = order.clone();
            let g = o.remove(from);
            let at = if to > from { to - 1 } else { to };
            o.insert(at.min(o.len()), g);
            if o != order {
                let m = &mut cx.prefs.context_menu;
                menu::set_full_order(&mut m.groups, &mut m.user_groups, &o);
                cx.changed |= C_MENU;
            }
        }
        pg.ui.request_repaint();
    }
    if let Some((k, d)) = gmove
        && let Some(o) = move_group(&order, k, d)
    {
        let m = &mut cx.prefs.context_menu;
        menu::set_full_order(&mut m.groups, &mut m.user_groups, &o);
        cx.changed |= C_MENU;
        gfocus = Some(k.saturating_add_signed(d));
    }
    if let Some(id) = gstart
        && let Some(u) = cx.prefs.context_menu.user_groups.iter().find(|u| u.id == id)
    {
        st.gedit = Some((id, TextInput::new(&u.name)));
        st.edit_fresh = true;
    }
    if let Some((id, text)) = gcommit.take() {
        // an empty name keeps the old one
        if let Some(u) = cx.prefs.context_menu.user_groups.iter_mut().find(|u| u.id == id)
            && !text.is_empty()
            && u.name != text
        {
            u.name = text;
            cx.changed |= C_MENU;
        }
        st.gedit = None;
    }
    if gcancel {
        st.gedit = None;
    }
    if let Some((id, on)) = gname
        && set_group_name_shown(&mut cx.prefs.context_menu.user_groups, &id, on)
    {
        cx.changed |= C_MENU;
    }
    if let Some(id) = gdelete {
        let m = &mut cx.prefs.context_menu;
        if menu::delete_group(&mut m.user_groups, &mut m.items, &id) {
            cx.changed |= C_MENU;
        }
        if st.gedit.as_ref().is_some_and(|(e, _)| *e == id) {
            st.gedit = None;
        }
    }
    if let Some(k) = gfocus {
        pg.ui.set_focus(Some(ID_GROW.with(k as u64)));
    }
    pg.y = gcard.b() + 12.0;

    // Tsubame's and the custom entries (the list; the grip reorders them)
    pg.section("Tsubame and custom entries");
    let shown: Vec<usize> = (0..list.len()).filter(|&i| !filtered || entry_shown(&list[i], &q, st.chip, st.kind)).collect();
    let show_preview = pg.w >= 720.0;
    let list_w = if show_preview { pg.w - 258.0 } else { pg.w };
    let top = pg.y;
    let card = Rect::new(pg.x, top, list_w, shown.len().max(1) as f32 * rh + 2.0);
    pg.g.round(card, t.radius_lg, t.panel);
    pg.g.round_stroke(card, t.radius_lg, t.border, 1.0);
    if shown.is_empty() {
        pg.g.text("Nothing matches.", Rect::new(card.x + 16.0, card.y + 1.0, card.w - 32.0, rh), t.font_small(), t.text_dim);
    }

    let mut hide_toggle: Option<usize> = None;
    let mut delete: Option<usize> = None;
    let mut start_edit: Option<usize> = None;
    let mut grip_press: Option<usize> = None;
    let mut commit: Option<(String, String)> = None;
    let mut cancel_edit = false;
    for (k, &i) in shown.iter().enumerate() {
        let e = &list[i];
        let r = Rect::new(card.x + 1.0, card.y + 1.0 + k as f32 * rh, card.w - 2.0, rh);
        if !pg.visible(r) {
            continue;
        }
        let d = depth(&list, e) as f32;
        let it = pg.ui.interact(ID_ROW.with(i as u64), r, Sense::click().cursor(Cursor::Arrow));
        let is_sel = st.sel.as_deref() == Some(e.id.as_str());
        if is_sel {
            pg.g.fill(r, t.selected);
        } else if it.hovered {
            pg.g.fill(r, t.hover);
        }
        if k > 0 {
            pg.g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        }
        let dim = e.hidden;
        // grip (no reordering while the list is filtered)
        let gr = Rect::new(r.x + 6.0 + d * 18.0, r.y, 18.0, r.h);
        if !filtered {
            let gi = pg.ui.interact(ID_GRIP.with(i as u64), gr, Sense::click().cursor(Cursor::Hand));
            for (dx, dy) in [(0.0, -4.0), (4.0, -4.0), (0.0, 0.0), (4.0, 0.0), (0.0, 4.0), (4.0, 4.0)] {
                pg.g.circle(gr.x + 7.0 + dx, r.y + r.h / 2.0 + dy, 1.0, if gi.hovered { t.text } else { t.text_disabled });
            }
            if gi.pressed.is_some() {
                grip_press = Some(i);
            }
        }
        // right side, from the right edge inwards
        let mut rx = r.r() - 10.0;
        if !e.is_separator() {
            let dr = Rect::new(rx - 24.0, r.y + 3.0, 24.0, r.h - 6.0);
            let target = RowTarget::Entry(e.id.clone());
            dots_row(pg, st, dr, &target, &|| facts_for(&target, &list, &cx.prefs.context_menu).unwrap_or_else(|| rowmenu::Facts::entry(e, false, false, false)));
            rx -= 28.0;
        }
        if e.source != Source::Builtin || e.is_separator() {
            let dr = Rect::new(rx - 22.0, r.y + 3.0, 22.0, r.h - 6.0);
            let di = pg.ui.interact(ID_DEL.with(i as u64), dr, Sense::click().cursor(Cursor::Hand));
            if di.hovered {
                pg.g.round(dr, t.radius_sm, t.hover);
            }
            pg.g.text("\u{E711}", dr, f(11.0).icon().c(), if di.hovered { t.danger } else { t.text_dim });
            if di.clicked == Some(Button::Left) {
                delete = Some(i);
            }
        }
        rx -= 26.0;
        let er = Rect::new(rx - 22.0, r.y + 3.0, 22.0, r.h - 6.0);
        let ei = pg.ui.interact(ID_EYE.with(i as u64), er, Sense::click().cursor(Cursor::Hand));
        if ei.hovered {
            pg.g.round(er, t.radius_sm, t.hover);
        }
        pg.g.text(if e.hidden { "\u{ED1A}" } else { "\u{E7B3}" }, er, f(13.0).icon().c(), if e.hidden { t.text_disabled } else { t.text_dim });
        if ei.clicked == Some(Button::Left) {
            hide_toggle = Some(i);
        }
        rx -= 28.0;
        if !e.is_separator() {
            let label = if e.is_submenu() { "Submenu" } else if e.source == Source::Builtin { "Tsubame" } else { e.source.label() };
            let tr = tag(pg, rx, r.y + 5.0, r.h - 10.0, label, e.source != Source::Windows, false);
            rx = tr.x - 10.0;
            let sc = shortcut_of(e, cx.keymap);
            if !sc.is_empty() {
                let sw = pg.g.measure(&sc, t.font_mono());
                pg.g.text(&sc, Rect::new(rx - sw, r.y, sw + 2.0, r.h), t.font_mono(), if dim { t.text_disabled } else { t.text_dim });
                rx -= sw + 12.0;
            }
        }
        // name
        let nx = gr.r() + 4.0;
        if e.is_separator() {
            pg.g.fill(Rect::new(nx, r.y + r.h / 2.0, (rx - nx - 80.0).max(10.0), 1.0), t.border);
            pg.g.text("Separator", Rect::new(rx - 70.0, r.y, 70.0, r.h), t.font_small().r(), t.text_disabled);
        } else {
            let editing_this = st.edit.as_ref().is_some_and(|(id, _)| *id == e.id);
            if editing_this {
                let nr = Rect::new(nx, r.y + 2.0, (rx - nx).clamp(100.0, 280.0), r.h - 4.0);
                let (_, input) = st.edit.as_mut().unwrap();
                if st.edit_fresh {
                    st.edit_fresh = false;
                    pg.ui.set_focus(Some(ID_EDIT));
                    input.select_all();
                }
                let resp = text_input(pg.ui, pg.g, ID_EDIT, nr, input, "");
                pg.ui.mark_text_focus();
                if resp.submitted {
                    commit = Some((e.id.clone(), input.text.trim().to_string()));
                } else if resp.cancelled || (!pg.ui.has_focus(ID_EDIT) && pg.ui.active().is_none() && !st.edit_fresh && pg.ui.frame() > 2) {
                    // Esc, or the field lost the focus: a rename ends
                    if resp.cancelled {
                        cancel_edit = true;
                    } else {
                        commit = Some((e.id.clone(), input.text.trim().to_string()));
                    }
                }
            } else {
                let name = fit(pg.g, &e.name, t.font_md(), (rx - nx).max(40.0));
                let nw = pg.g.measure(&name, t.font_md());
                pg.g.text(&name, Rect::new(nx, r.y, (rx - nx).max(40.0), r.h), t.font_md(), if dim { t.text_disabled } else { t.text });
                if dim {
                    pg.g.fill(Rect::new(nx, r.y + r.h / 2.0, nw, 1.0), t.text_disabled);
                }
                if e.is_submenu() {
                    let n = list.iter().filter(|c| c.parent == e.id).count();
                    pg.g.text(&format!("\u{2014} submenu, {n} item{}", if n == 1 { "" } else { "s" }), Rect::new(nx + nw + 8.0, r.y, 200.0, r.h), t.font_small(), t.text_dim);
                } else if let Some(d) = menu::default_entries().iter().find(|d| d.id == e.id && d.name != e.name && !d.name.is_empty()) {
                    let was = fit(pg.g, &format!("\u{2014} was \u{201c}{}\u{201d}", d.name), t.font_small(), (rx - nx - nw - 8.0).max(0.0));
                    pg.g.text(&was, Rect::new(nx + nw + 8.0, r.y, (rx - nx - nw - 8.0).max(0.0), r.h), t.font_small(), t.text_dim);
                }
            }
        }
        if it.clicked == Some(Button::Left) {
            st.sel = Some(e.id.clone());
        }
        if it.pressed.is_some_and(|p| p.double) && !e.is_separator() {
            start_edit = Some(i);
        }
    }
    // drag
    if let Some(i) = grip_press {
        st.drag = Some(i);
        st.sel = list.get(i).map(|e| e.id.clone());
    }
    if filtered {
        st.drag = None;
    }
    if let Some(from) = st.drag {
        let my = pg.ui.mouse().y;
        let rel = (my - card.y - 1.0) / rh;
        let to = (rel + 0.5).floor().clamp(0.0, list.len() as f32) as usize;
        // the middle of a submenu row drops inside it
        let row = rel.floor().clamp(0.0, (list.len() - 1) as f32) as usize;
        let frac = rel - rel.floor();
        let onto = (list[row].is_submenu() && row != from && (0.3..0.7).contains(&frac) && !list[from].is_submenu()).then_some(row);
        match onto {
            Some(s) => {
                let rr = Rect::new(card.x + 2.0, card.y + 1.0 + s as f32 * rh, card.w - 4.0, rh);
                pg.g.round_stroke(rr, t.radius_sm, t.accent, 2.0);
            }
            None => {
                let ly = card.y + 1.0 + to as f32 * rh;
                pg.g.fill(Rect::new(card.x + 6.0, ly - 1.0, card.w - 12.0, 2.0), t.accent);
                pg.g.circle(card.x + 6.0, ly, 4.0, t.accent);
            }
        }
        if pg.ui.active().is_none() {
            st.drag = None;
            let np = drop_parent(&list, from, to, onto);
            let moved = if let Some(s) = onto {
                // to the end of that submenu
                let sid = list[s].id.clone();
                let mut end = s + 1;
                while end < list.len() && list[end].parent == sid {
                    end += 1;
                }
                menu::move_entry(&mut list, from, end)
            } else {
                menu::move_entry(&mut list, from, to)
            };
            let idx = moved.or(Some(from)).unwrap_or(from);
            if idx < list.len() && list[idx].parent != np {
                list[idx].parent = np;
                modified = true;
            }
            if moved.is_some() {
                modified = true;
            }
        }
        pg.ui.request_repaint();
    }
    if let Some(i) = start_edit {
        st.edit = Some((list[i].id.clone(), TextInput::new(&list[i].name)));
        st.edit_fresh = true;
    }
    if let Some((id, text)) = commit.take() {
        if let Some(e) = list.iter_mut().find(|e| e.id == id) {
            // an empty name gives a built-in its default name back
            let text = if text.is_empty() { menu::default_entries().into_iter().find(|d| d.id == id).map(|d| d.name).unwrap_or_default() } else { text };
            if !text.is_empty() && e.name != text {
                e.name = text;
                modified = true;
            }
        }
        st.edit = None;
    }
    if cancel_edit {
        st.edit = None;
    }
    if let Some(i) = hide_toggle {
        list[i].hidden = !list[i].hidden;
        modified = true;
    }
    if let Some(i) = delete {
        modified |= menu::delete_entry(&mut list, i);
    }

    // preview (a filtered list can be shorter than it: what follows goes below both)
    let mut bottom = top + card.h;
    if show_preview {
        let px = pg.x + list_w + 12.0;
        let pw = 246.0;
        pg.g.text("Preview", Rect::new(px, top - 18.0, pw, 16.0), t.font_small().b(), t.text_dim);
        let rows: Vec<&MenuEntry> = list.iter().filter(|e| !e.hidden && e.parent.is_empty()).collect();
        let ph = rows.iter().map(|e| if e.is_separator() { 9.0 } else { 24.0 }).sum::<f32>() + 12.0;
        let pr = Rect::new(px, top, pw, ph);
        pg.g.round(pr, t.radius_lg, t.popup);
        pg.g.round_stroke(pr, t.radius_lg, t.popup_border, 1.0);
        let mut y = pr.y + 6.0;
        for e in rows {
            if e.is_separator() {
                pg.g.fill(Rect::new(pr.x + 8.0, y + 4.0, pr.w - 16.0, 1.0), t.border);
                y += 9.0;
                continue;
            }
            let col = if e.id == "builtin.delete" { t.danger } else { t.text };
            let shown = fit(pg.g, &e.name, t.font(), pw - 90.0);
            pg.g.text(&shown, Rect::new(pr.x + 14.0, y, pw - 90.0, 24.0), t.font(), col);
            if e.is_submenu() || e.source == Source::Windows && e.id == "windows.open-with" {
                pg.g.text("\u{E76C}", Rect::new(pr.r() - 24.0, y, 14.0, 24.0), f(9.0).icon().c(), t.text_dim);
            } else {
                let sc = shortcut_of(e, cx.keymap);
                if !sc.is_empty() {
                    let sw = pg.g.measure(&sc, t.font_small());
                    pg.g.text(&sc, Rect::new(pr.r() - 12.0 - sw, y, sw + 2.0, 24.0), t.font_small(), t.text_dim);
                }
            }
            y += 24.0;
        }
        let hint = wrap(pg.g, "Hidden rows (struck through, eye off) leave the preview. Submenu contents and the items learned from Windows are not drawn here.", t.font_small(), pw - 24.0);
        let hr = Rect::new(px, pr.b() + 10.0, pw, 16.0 + hint.len() as f32 * 15.0);
        pg.g.round(hr, t.radius_md, t.panel);
        pg.g.round_stroke(hr, t.radius_md, t.border, 1.0);
        for (i, l) in hint.iter().enumerate() {
            pg.g.text(l, Rect::new(hr.x + 12.0, hr.y + 8.0 + i as f32 * 15.0, pw - 24.0, 15.0), t.font_small(), t.text_dim);
        }
        bottom = bottom.max(hr.b());
    }
    pg.y = bottom + 12.0;

    // items learned from real menus: Windows', the apps' and Tsubame's own rows
    pg.section("Windows, installed apps and other menu items");
    paint_items(pg, st, cx, &q, !q.is_empty() || st.chip != 0);

    let note = "How the list is filled. Items are learned from real right-clicks: right-click a file once and the entries of every installed shell extension show up here. Each item is remembered by its verb and its handler, not by its label, so a hidden item stays hidden when the program updates or is translated. The eye hides an item, double-clicking a name renames it (an empty name restores the original), and the \u{00d7} on a custom row deletes it.";
    pg.note("\u{E946}", note, false);

    // the row "..." popup (drawn last, over everything) and the keys of a focused "..." button
    let mut done: Option<(RowTarget, u64)> = st.act.take();
    if let Some(mut pop) = st.pop.take() {
        match facts_for(&pop.target, &list, &cx.prefs.context_menu) {
            Some(f) => {
                let src = RowSrc { name: f.name.clone(), facts: f };
                let style = MenuStyle { alt: false, bounds: pg.vis, loading: None, now_ms: 0, a11y: false };
                let resp = edit_only(pg.ui, pg.g, ID_POP, &mut pop.st, &src, &style, ROW_KEY, pop.btn);
                if let Some((_, k)) = resp.edit {
                    done = Some((pop.target.clone(), k));
                }
                if resp.closed || resp.edit.is_some() {
                    // the menu is done: the keyboard goes back to the button it came from
                    pg.ui.close_all_popups();
                    if pop.st.keyboard {
                        pg.ui.set_focus(Some(pop.btn_id));
                    }
                } else {
                    st.pop = Some(pop);
                }
            }
            // the row went away under the popup
            None => pg.ui.close_all_popups(),
        }
    }
    if let Some((target, k)) = done
        && let Some(f) = facts_for(&target, &list, &cx.prefs.context_menu)
    {
        modified |= apply(st, cx, &mut list, &f, &target, k);
    }
    if st.pop.is_none() && pg.ui.popups_open() > usize::from(nss_sel::dropdown_open(pg.ui)) {
        pg.ui.close_all_popups();
    }
    if let Some((at, text)) = st.tip.take() {
        crate::overlay::tooltip(pg.g, &t, pg.ui.size(), at, &text, &[]);
    }

    if modified {
        // keep an unchanged default menu out of the file
        cx.prefs.context_menu.entries = if list == menu::default_entries() { Vec::new() } else { list };
        cx.changed |= C_MENU;
        st.confirm_reset = false;
    }
}

/// The `[[context_menu.items]]` card: grip (moves the item between groups; the
/// native order inside a group stays), eye, name (double-click renames, "—
/// was" shows the original), where it comes from, New.
fn paint_items(pg: &mut Pg, st: &mut State, cx: &mut Cx, q: &str, filtered: bool) {
    let t = pg.t;
    let rh = t.height_menu_edit_row;
    let order = menu::full_order(&cx.prefs.context_menu.groups, &cx.prefs.context_menu.user_groups);
    let users = cx.prefs.context_menu.user_groups.clone();
    let shown: Vec<usize> = (0..cx.prefs.context_menu.items.len()).filter(|&i| item_shown_for(&cx.prefs.context_menu.items[i], q, st.chip, st.kind)).collect();
    // rows: a header per group (unfiltered: the user's groups always, a built-in one when it has items), then its items
    // (grouped like the menu: pinned first, then first seen first)
    let mut rows: Vec<(Option<usize>, menu::GroupRef)> = Vec::new();
    for (g, mine) in order_rows(&cx.prefs.context_menu.items, &shown, &users, &order) {
        let user = matches!(g, menu::GroupRef::User(_));
        if !filtered && (user || !mine.is_empty()) {
            rows.push((None, g.clone()));
        }
        rows.extend(mine.into_iter().map(|i| (Some(i), g.clone())));
    }
    let card = Rect::new(pg.x, pg.y, pg.w, rows.len().max(1) as f32 * rh + 2.0);
    pg.g.round(card, t.radius_lg, t.panel);
    pg.g.round_stroke(card, t.radius_lg, t.border, 1.0);
    if rows.is_empty() {
        let msg = if cx.prefs.context_menu.items.is_empty() { "Nothing learned yet: right-click a file once." } else { "Nothing matches." };
        pg.g.text(msg, Rect::new(card.x + 16.0, card.y + 1.0, card.w - 32.0, rh), t.font_small(), t.text_dim);
    }
    let new_keys = st.new_shown.clone().unwrap_or_default();
    let mut toggle = None;
    let mut pin_toggle: Option<usize> = None;
    let mut start = None;
    let mut grip = None;
    let mut commit: Option<(String, String)> = None;
    let mut cancel = false;
    for (k, (row, grp)) in rows.iter().enumerate() {
        let r = Rect::new(card.x + 1.0, card.y + 1.0 + k as f32 * rh, card.w - 2.0, rh);
        let Some(i) = *row else {
            // group header
            if !pg.visible(r) {
                continue;
            }
            if k > 0 {
                pg.g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
            }
            pg.g.fill(Rect::new(r.x, r.y + 1.0, r.w, r.h - 1.0), t.hover);
            pg.ui.interact(ID_HDR.with(k as u64), r, Sense::hover());
            let name: String = match grp {
                menu::GroupRef::Builtin(b) => b.label().to_string(),
                menu::GroupRef::User(id) => users.iter().find(|u| u.id == *id).map(|u| u.name.clone()).unwrap_or_default(),
            };
            let nw = pg.g.measure(&name, t.font_md().b());
            pg.g.text(&name, Rect::new(r.x + 14.0, r.y, nw + 4.0, r.h), t.font_md().b(), t.text);
            let n = group_count(&[], &cx.prefs.context_menu.items, &users, grp);
            pg.g.text(&format!("{n} item{}", if n == 1 { "" } else { "s" }), Rect::new(r.x + 14.0 + nw + 12.0, r.y, 120.0, r.h), t.font_small(), t.text_dim);
            continue;
        };
        let p = &cx.prefs.context_menu.items[i];
        if !pg.visible(r) {
            continue;
        }
        let it = pg.ui.interact(ID_ITEM.with(i as u64), r, Sense::click().cursor(Cursor::Arrow));
        if st.sel.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&p.key)) {
            pg.g.fill(r, t.selected);
        } else if it.hovered {
            pg.g.fill(r, t.hover);
        }
        if k > 0 {
            pg.g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        }
        let gr = Rect::new(r.x + 6.0, r.y, 18.0, r.h);
        let gi = pg.ui.interact(ID_IGRIP.with(i as u64), gr, Sense::click().cursor(Cursor::Hand));
        // a pinned row keeps its place at the top of the group: its grip is dimmed
        for (dx, dy) in [(0.0, -4.0), (4.0, -4.0), (0.0, 0.0), (4.0, 0.0), (0.0, 4.0), (4.0, 4.0)] {
            pg.g.circle(gr.x + 7.0 + dx, r.y + r.h / 2.0 + dy, 1.0, if p.pin { t.border } else if gi.hovered { t.text } else { t.text_disabled });
        }
        if gi.pressed.is_some() {
            grip = Some(i);
        }
        let er = Rect::new(gr.r() + 4.0, r.y + 3.0, 22.0, r.h - 6.0);
        let ei = pg.ui.interact(ID_ITEM_EYE.with(i as u64), er, Sense::click().cursor(Cursor::Hand));
        if ei.hovered {
            pg.g.round(er, t.radius_sm, t.hover);
        }
        pg.g.text(if p.hidden { "\u{ED1A}" } else { "\u{E7B3}" }, er, f(13.0).icon().c(), if p.hidden { t.text_disabled } else { t.text_dim });
        if ei.clicked == Some(Button::Left) {
            toggle = Some(i);
        }
        // right: "...", the pin cell, where it comes from, New
        let mut rx = r.r() - 10.0;
        let dr = Rect::new(rx - 24.0, r.y + 3.0, 24.0, r.h - 6.0);
        let target = RowTarget::Item(p.key.clone());
        dots_row(pg, st, dr, &target, &|| rowmenu::Facts::learned(p, &users, false, false));
        rx -= 28.0;
        let gname: String = match grp {
            menu::GroupRef::Builtin(b) => b.label().to_string(),
            menu::GroupRef::User(id) => users.iter().find(|u| u.id == *id).map(|u| u.name.clone()).unwrap_or_default(),
        };
        if pin_cell(pg, st, Rect::new(rx - 22.0, r.y + 3.0, 22.0, r.h - 6.0), &p.key, p.pin, p.hidden, &gname) {
            pin_toggle = Some(i);
        }
        rx -= 28.0;
        if new_keys.iter().any(|n| n.eq_ignore_ascii_case(&p.key)) {
            rx = tag(pg, rx, r.y + 5.0, r.h - 10.0, "New", false, true).x - 4.0;
        }
        let from = if p.from.is_empty() { "Windows" } else { p.from.split('_').next().unwrap_or("Windows") };
        rx = tag(pg, rx, r.y + 5.0, r.h - 10.0, from, from == "Tsubame", false).x - 10.0;
        let nx = er.r() + 8.0;
        if st.iedit.as_ref().is_some_and(|(key, _)| *key == p.key) {
            let nr = Rect::new(nx, r.y + 2.0, (rx - nx).clamp(100.0, 280.0), r.h - 4.0);
            let fresh = st.edit_fresh;
            let (_, input) = st.iedit.as_mut().unwrap();
            if fresh {
                st.edit_fresh = false;
                pg.ui.set_focus(Some(ID_ITEM_EDIT));
                input.select_all();
            }
            let resp = text_input(pg.ui, pg.g, ID_ITEM_EDIT, nr, input, &p.label);
            pg.ui.mark_text_focus();
            if resp.submitted || (!resp.cancelled && !pg.ui.has_focus(ID_ITEM_EDIT) && pg.ui.active().is_none() && !fresh && pg.ui.frame() > 2) {
                commit = Some((p.key.clone(), input.text.trim().to_string()));
            } else if resp.cancelled {
                cancel = true;
            }
        } else {
            let name = if p.name.is_empty() { &p.label } else { &p.name };
            let shown_name = fit(pg.g, name, t.font_md(), (rx - nx).max(40.0));
            let nw = pg.g.measure(&shown_name, t.font_md());
            pg.g.text(&shown_name, Rect::new(nx, r.y, (rx - nx).max(40.0), r.h), t.font_md(), if p.hidden { t.text_disabled } else { t.text });
            if p.hidden {
                pg.g.fill(Rect::new(nx, r.y + r.h / 2.0, nw, 1.0), t.text_disabled);
            }
            if !p.name.is_empty() && p.name != p.label {
                let w = (rx - nx - nw - 8.0).max(0.0);
                let was = fit(pg.g, &format!("\u{2014} was \u{201c}{}\u{201d}", p.label), t.font_small(), w);
                pg.g.text(&was, Rect::new(nx + nw + 8.0, r.y, w, r.h), t.font_small(), t.text_dim);
            }
        }
        if it.clicked == Some(Button::Left) {
            st.sel = Some(p.key.clone());
        }
        if it.pressed.is_some_and(|pr| pr.double) {
            start = Some(i);
        }
    }
    // drag an item onto a group (its header or any of its rows)
    if let Some(i) = grip {
        st.idrag = Some(i);
        st.sel = Some(cx.prefs.context_menu.items[i].key.clone());
    }
    if let Some(from) = st.idrag.filter(|&f| f < cx.prefs.context_menu.items.len()) {
        let rel = (pg.ui.mouse().y - card.y - 1.0) / rh;
        let target = (rel >= 0.0 && (rel as usize) < rows.len()).then(|| rows[rel as usize].1.clone());
        let ok = target.as_ref().and_then(|g| drop_group(&cx.prefs.context_menu.items[from], g));
        if let (Some(g), Some(_)) = (&target, &ok) {
            // the whole group lights up
            for (k, (_, rg)) in rows.iter().enumerate() {
                if rg == g {
                    pg.g.round_stroke(Rect::new(card.x + 2.0, card.y + 1.0 + k as f32 * rh, card.w - 4.0, rh), t.radius_sm, t.accent, 1.0);
                }
            }
        }
        if pg.ui.active().is_none() {
            st.idrag = None;
            if let Some(gid) = ok {
                let key = cx.prefs.context_menu.items[from].key.clone();
                let m = &mut cx.prefs.context_menu;
                if menu::move_item_to_group(&mut m.items, &m.user_groups, &key, &gid) {
                    cx.changed |= C_MENU;
                }
            }
        }
        pg.ui.request_repaint();
    } else {
        st.idrag = None;
    }
    if let Some(i) = pin_toggle {
        let (key, label, pin) = {
            let p = &cx.prefs.context_menu.items[i];
            (p.key.clone(), p.label.clone(), p.pin)
        };
        if ctxui::set_pin(&mut cx.prefs.context_menu, &key, &label, !pin) {
            cx.changed |= C_MENU;
        }
    }
    let items = &mut cx.prefs.context_menu.items;
    if let Some(i) = toggle {
        items[i].hidden = !items[i].hidden;
        cx.changed |= C_MENU;
    }
    if let Some(i) = start {
        let p = &items[i];
        st.iedit = Some((p.key.clone(), TextInput::new(if p.name.is_empty() { &p.label } else { &p.name })));
        st.edit_fresh = true;
    }
    if let Some((key, text)) = commit {
        if let Some(p) = menu::item_mut(items, &key) {
            // the item's own label (or nothing) means: no rename
            let name = if text == p.label { String::new() } else { text };
            if p.name != name {
                p.name = name;
                cx.changed |= C_MENU;
            }
        }
        st.iedit = None;
    }
    if cancel {
        st.iedit = None;
    }
    pg.y = card.b() + 12.0;
    // Several items / Drive before their first right-click: say why the list is short (mock `ctxgap-kinds` B)
    if matches!(st.kind, Some(3 | 4)) && st.chip == 0 && q.is_empty() && shown.is_empty() {
        pg.note("\u{E946}", kind_empty_note(st.kind == Some(4)), false);
    }
}

/// The note under an empty Several items / Drive list.
fn kind_empty_note(drive: bool) -> &'static str {
    if drive {
        "No drive items learned yet. Windows\u{2019} and apps\u{2019} drive entries (Format, Check disk, BitLocker, Defender) appear here after the first right-click on a drive in This PC or the sidebar. Until then only Tsubame\u{2019}s own rows are listed."
    } else {
        "No items learned yet for a selection. Windows\u{2019} and apps\u{2019} entries for several items appear here after the first right-click on two or more items. Until then only Tsubame\u{2019}s own rows are listed."
    }
}

/// Reads `path` (never writes it) and opens the selection screen for what Tsubame can run. A file
/// with nothing to import goes straight to the report (it lists why). Returns whether `list` changed.
fn run_import(st: &mut State, list: &mut [MenuEntry], path: &std::path::Path, m: &tsubame_config::ContextMenu) -> bool {
    match nss::read_file(path) {
        Ok(src) => {
            let imp = nss::import(&src, list, "");
            let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "shell.nss".into());
            if imp.imported == 0 {
                st.report = Some(nss::Report::new(&file, &imp));
                st.msg = Some(("Nothing in that file can be imported. The report lists why.".into(), true));
            } else {
                st.review = Some(Box::new(nss_sel::Sel::new(&file, &path.display().to_string(), imp, &m.nss_group, &m.user_groups)));
                st.report = None;
                st.add = None;
                st.msg = None;
            }
        }
        Err(e) => st.msg = Some((e, true)),
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx_parts() -> (tsubame_config::Config, KeyMap, super::super::Env) {
        (tsubame_config::Config::default(), KeyMap::defaults(), super::super::Env::default())
    }

    fn ipref(key: &str, label: &str, pin: bool) -> menu::ItemPref {
        menu::ItemPref { key: key.into(), label: label.into(), from: "Windows".into(), pin, ..Default::default() }
    }

    fn last(kind: menu::Target) -> ctxui::LastMenu {
        ctxui::LastMenu { kind, at: "14:32".into(), items: 31 }
    }

    fn cx_of<'a>(prefs: &'a mut tsubame_config::Config, km: &'a mut KeyMap, env: &'a super::super::Env) -> Cx<'a> {
        Cx { prefs, keymap: km, env, changed: 0, open: vec![], reset_all: false, restart: false, rollback: false, reload_packs: false, message: None, private_bytes: 0, tags: Default::default() }
    }

    #[test]
    fn keys_on_a_focused_dots_button_map_to_actions() {
        use rowmenu::Act;
        let custom = MenuEntry::custom("custom.1", "Open in VS Code", "code.exe", "");
        let f = rowmenu::Facts::entry(&custom, true, true, false);
        assert_eq!(button_key(Key::F(2), Mods::default(), &f), Some(Act::Rename));
        assert_eq!(button_key(Key::Delete, Mods::default(), &f), Some(Act::DeleteEntry));
        assert_eq!(button_key(Key::Up, Mods::ALT, &f), Some(Act::Up));
        assert_eq!(button_key(Key::Down, Mods::ALT, &f), Some(Act::Down));
        assert_eq!(button_key(Key::Down, Mods::default(), &f), None, "a plain arrow is not a row key");
        // the first row cannot move up; Delete is only for custom rows
        let first = rowmenu::Facts::entry(&custom, false, true, false);
        assert_eq!(button_key(Key::Up, Mods::ALT, &first), None);
        let own = rowmenu::Facts::entry(&MenuEntry::builtin("builtin.copy", "Copy", "file.copy"), true, true, false);
        assert_eq!(button_key(Key::Delete, Mods::default(), &own), None);
        // a Windows row never moves, whatever the key
        let win = rowmenu::Facts::learned(&ipref("windows.verb:share", "Share", false), &[], false, false);
        assert_eq!(button_key(Key::Up, Mods::ALT, &win), None);
        assert_eq!(button_key(Key::F(2), Mods::default(), &win), Some(Act::Rename));
    }

    #[test]
    fn refresh_states_follow_the_mock() {
        let t0 = Instant::now();
        // idle: the last menu's time and count
        let idle = refresh_view(&Refresh::Idle, Some(&last(menu::Target::File)), t0);
        assert_eq!((idle.label, idle.status.as_str(), idle.busy, idle.ok), ("Refresh from last menu", "Last menu: 14:32, 31 items", false, false));
        // running is drawn only after 150 ms
        let run = Refresh::Running { since: t0, wait: Duration::from_millis(900) };
        assert_eq!(refresh_view(&run, Some(&last(menu::Target::File)), t0 + Duration::from_millis(100)), idle, "under 150 ms: still idle");
        let busy = refresh_view(&run, None, t0 + Duration::from_millis(200));
        assert!(busy.busy && busy.label == "Refreshing\u{2026}" && busy.status == "Comparing with the last menu\u{2026}");
        // done and nothing to do
        assert_eq!(refresh_outcome(12), Refresh::Done(12));
        assert_eq!(refresh_outcome(0), Refresh::UpToDate);
        let done = refresh_view(&Refresh::Done(12), None, t0);
        assert!(done.ok && done.check && done.status == "12 items updated" && done.extra == "The list is shorter by 12 rows");
        assert_eq!(refresh_view(&Refresh::Done(1), None, t0).status, "1 item updated");
        let up = refresh_view(&Refresh::UpToDate, None, t0);
        assert!(!up.ok && up.check && up.status == "Already up to date");
    }

    #[test]
    fn refresh_needs_a_menu_of_the_selected_kind() {
        assert!(!refresh_enabled(None, None), "no menu yet: disabled");
        assert!(refresh_tip(None).starts_with("Right-click a file in Tsubame first"));
        let l = last(menu::Target::Folder);
        assert!(refresh_enabled(Some(&l), None), "every kind: any menu");
        assert!(refresh_enabled(Some(&l), Some(1)), "Folder switch, folder menu");
        assert!(!refresh_enabled(Some(&l), Some(0)), "File switch, folder menu");
        assert!(!refresh_enabled(Some(&l), Some(2)));
    }

    #[test]
    fn refresh_runs_the_compare_and_reports_the_count() {
        // no menu was shown in a unit test run: nothing to compare with, nothing changes
        let (mut prefs, mut km, env) = cx_parts();
        prefs.context_menu.items = vec![ipref("windows.verb:share", "Share", false)];
        let mut cx = cx_of(&mut prefs, &mut km, &env);
        let mut st = State::default();
        run_refresh(&mut st, &mut cx);
        assert_eq!(st.refresh, Refresh::UpToDate);
        assert_eq!(cx.changed, 0);
        assert_eq!(cx.prefs.context_menu.items.len(), 1);
    }

    #[test]
    fn pinned_rows_lead_their_group_in_native_order() {
        let items = vec![ipref("a", "A", false), ipref("b", "B", true), ipref("c", "C", false), ipref("d", "D", true)];
        let order = menu::full_order(&[], &[]);
        let all: Vec<usize> = (0..items.len()).collect();
        let win = |rows: Vec<(menu::GroupRef, Vec<usize>)>| rows.into_iter().find(|(g, _)| matches!(g, menu::GroupRef::Builtin(menu::Group::Windows))).unwrap().1;
        // every item here comes from Windows: one group, pins first, each part in its order
        assert_eq!(win(order_rows(&items, &all, &[], &order)), vec![1, 3, 0, 2]);
        // a filter keeps the order of what is left
        assert_eq!(win(order_rows(&items, &[2, 3], &[], &order)), vec![3, 2]);
    }

    #[test]
    fn the_popup_actions_edit_the_right_data() {
        let (mut prefs, mut km, env) = cx_parts();
        let mut cx = cx_of(&mut prefs, &mut km, &env);
        let mut st = State::default();
        let mut list = menu::default_entries();
        list.push(MenuEntry::custom("custom.1", "Open in VS Code", "code.exe", "\"{path}\""));
        let facts = |t: &RowTarget, list: &[MenuEntry], cx: &Cx| facts_for(t, list, &cx.prefs.context_menu).unwrap();
        // Hide / Show on a list entry
        let t = RowTarget::Entry("custom.1".into());
        let f = facts(&t, &list, &cx);
        assert!(apply(&mut st, &mut cx, &mut list, &f, &t, EDIT_HIDE) && list.last().unwrap().hidden);
        let f = facts(&t, &list, &cx);
        assert!(apply(&mut st, &mut cx, &mut list, &f, &t, EDIT_HIDE) && !list.last().unwrap().hidden);
        // Move up passes the custom row before it; the first custom row cannot go up
        list.push(MenuEntry::custom("custom.2", "Second", "b.exe", ""));
        let t2 = RowTarget::Entry("custom.2".into());
        let f = facts(&t2, &list, &cx);
        assert!(apply(&mut st, &mut cx, &mut list, &f, &t2, EDIT_UP));
        let pos = |list: &[MenuEntry], id: &str| list.iter().position(|e| e.id == id).unwrap();
        assert!(pos(&list, "custom.2") < pos(&list, "custom.1"));
        let f = facts(&t2, &list, &cx);
        assert!(!apply(&mut st, &mut cx, &mut list, &f, &t2, EDIT_UP) || pos(&list, "custom.2") < pos(&list, "custom.1"));
        // Rename starts the inline field
        let f = facts(&t, &list, &cx);
        assert!(!apply(&mut st, &mut cx, &mut list, &f, &t, EDIT_RENAME));
        assert_eq!(st.edit.as_ref().map(|(id, _)| id.as_str()), Some("custom.1"));
        // Edit entry opens the form filled in, Delete entry removes the row and keeps the Undo
        assert!(!apply(&mut st, &mut cx, &mut list, &f, &t, EDIT_ENTRY));
        let form = st.add.as_ref().unwrap();
        assert_eq!((form.edit.as_deref(), form.name.text.as_str(), form.cmd.text.as_str()), (Some("custom.1"), "Open in VS Code", "code.exe"));
        let before = list.len();
        assert!(apply(&mut st, &mut cx, &mut list, &f, &t, EDIT_DELETE));
        assert_eq!(list.len(), before - 1);
        let d = st.deleted.as_ref().unwrap();
        assert_eq!((d.before.len(), d.after == list), (before, true));
        // a built-in has no Delete entry
        let own = RowTarget::Entry("builtin.copy".into());
        let f = facts(&own, &list, &cx);
        let n = list.len();
        assert!(!apply(&mut st, &mut cx, &mut list, &f, &own, EDIT_DELETE) && list.len() == n);
    }

    #[test]
    fn the_popup_pins_hides_and_groups_a_learned_row() {
        let (mut prefs, mut km, env) = cx_parts();
        prefs.context_menu.items = vec![ipref("windows.verb:share", "Share", false)];
        let mut cx = cx_of(&mut prefs, &mut km, &env);
        let mut st = State::default();
        let mut list = menu::default_entries();
        let t = RowTarget::Item("windows.verb:share".into());
        let mut run = |cx: &mut Cx, st: &mut State, k: u64| {
            let f = facts_for(&t, &[], &cx.prefs.context_menu).unwrap();
            apply(st, cx, &mut list, &f, &t, k)
        };
        // Pin / Unpin write ItemPref.pin
        assert!(!run(&mut cx, &mut st, EDIT_PIN));
        assert!(cx.prefs.context_menu.items[0].pin && cx.changed & C_MENU != 0);
        run(&mut cx, &mut st, EDIT_PIN);
        assert!(!cx.prefs.context_menu.items[0].pin);
        // Hide
        run(&mut cx, &mut st, EDIT_HIDE);
        assert!(cx.prefs.context_menu.items[0].hidden);
        // Move to group > New group makes a group, moves the row in and starts the name field
        run(&mut cx, &mut st, EDIT_GROUP_NEW);
        assert_eq!(cx.prefs.context_menu.user_groups.len(), 1);
        let gid = cx.prefs.context_menu.user_groups[0].id.clone();
        assert_eq!(cx.prefs.context_menu.items[0].group, gid);
        assert_eq!(st.gedit.as_ref().map(|(id, _)| id.clone()), Some(gid.clone()));
        // Native place sends the row home, the flyout's first row brings it back
        run(&mut cx, &mut st, EDIT_GROUP_NATIVE);
        assert!(cx.prefs.context_menu.items[0].group.is_empty());
        run(&mut cx, &mut st, EDIT_GROUP_BASE);
        assert_eq!(cx.prefs.context_menu.items[0].group, gid);
        // Rename starts the item's field
        run(&mut cx, &mut st, EDIT_RENAME);
        assert_eq!(st.iedit.as_ref().map(|(k, _)| k.as_str()), Some("windows.verb:share"));
    }

    #[test]
    fn dots_buttons_keep_their_id_when_a_row_moves() {
        assert_eq!(seed("custom.1"), seed("custom.1"));
        assert_ne!(seed("custom.1"), seed("custom.2"));
        assert_ne!(ID_DOTS.with(seed("a")), ID_DOTS.with(seed("b")));
    }

    #[test]
    fn extensions_are_cleaned() {
        assert_eq!(parse_exts("txt, .MD  *.log;txt"), vec!["txt", "md", "log"]);
        assert!(parse_exts("  ").is_empty());
    }

    #[test]
    fn a_new_entry_needs_a_name_and_a_program() {
        let list = menu::default_entries();
        assert!(new_entry(&list, "", "code.exe", "{path}", "").is_err());
        assert!(new_entry(&list, "VS Code", "  ", "{path}", "").is_err());
        assert!(new_entry(&list, "VS Code", "code.exe", "\"unclosed", "").is_err());
        let e = new_entry(&list, " Open in VS Code ", "\"C:\\x\\Code.exe\"", "\"{path}\"", "ts, .rs").unwrap();
        assert_eq!(e.name, "Open in VS Code");
        assert_eq!(e.command, "C:\\x\\Code.exe");
        assert_eq!(e.extensions, vec!["ts", "rs"]);
        assert!(e.source.runs_program());
        assert!(list.iter().all(|x| x.id != e.id));
    }

    #[test]
    fn dropping_inside_a_submenu_sets_the_parent() {
        let mut l = menu::default_entries();
        l.push(MenuEntry { id: "custom.s".into(), kind: EntryKind::Submenu, source: Source::Custom, name: "Tools".into(), ..Default::default() });
        let s = l.len() - 1;
        assert_eq!(drop_parent(&l, 0, 0, Some(s)), "custom.s");
        assert_eq!(drop_parent(&l, 0, l.len(), None), "");
        // a submenu never goes inside another
        assert_eq!(drop_parent(&l, s, 0, Some(s)), "");
    }

    fn pref(key: &str, label: &str, from: &str) -> menu::ItemPref {
        menu::ItemPref { key: key.into(), label: label.into(), from: from.into(), ..Default::default() }
    }

    #[test]
    fn search_and_chips_filter_both_lists() {
        let share = pref("windows.verb:share", "Share", "Windows");
        let mut hidden = pref("app:x.copy-name", "Copy name", "Tsubame");
        hidden.hidden = true;
        let term = menu::ItemPref { name: "Terminal".into(), ..pref("windows.pkg:Microsoft.WindowsTerminal_8wekyb3d8bbwe/{x}", "Open in Terminal", "Microsoft.WindowsTerminal_8wekyb3d8bbwe") };
        assert!(item_shown(&share, "", 0) && item_shown(&share, "sha", 1) && !item_shown(&share, "", 3));
        assert!(item_shown(&hidden, "", 3) && item_shown(&hidden, "", 4) && !item_shown(&share, "", 4));
        assert!(item_shown(&term, "terminal", 0) && item_shown(&term, "windowsterminal", 1), "name, label and source are searched");
        assert!(!item_shown(&term, "", 2), "a Microsoft package is not 3rd party");
        let seven = pref("windows.pkg:Igor.7zip_abc/{x}", "7-Zip", "Igor.7zip_abc");
        assert!(item_shown(&seven, "", 2) && !item_shown(&seven, "", 1) && !item_shown(&seven, "", 3));
        let copy = MenuEntry::builtin("builtin.copy", "Copy", "file.copy");
        let more = default_more();
        assert!(entry_shown(&copy, "co", 3, None) && !entry_shown(&copy, "co", 1, None) && !entry_shown(&copy, "", 4, None));
        assert!(entry_shown(&more, "", 1, None) && !entry_shown(&more, "", 2, None));
        assert!(!entry_shown(&MenuEntry::separator("sep.1"), "co", 0, None), "a search hides separators");
        let mut nss = MenuEntry::custom("nss.1", "Ver Calc", "calc.exe", "");
        nss.source = Source::Nilesoft;
        assert!(entry_shown(&nss, "from nilesoft", 0, None), "the source tag is searched (Show imported)");
    }

    #[test]
    fn the_kind_switch_follows_show_for() {
        let mut e = MenuEntry::custom("custom.1", "VS Code", "code.exe", "");
        e.show_for = ShowFor::classic(true, false, true);
        assert!(entry_shown(&e, "", 0, Some(0)) && !entry_shown(&e, "", 0, Some(1)) && entry_shown(&e, "", 0, Some(2)) && entry_shown(&e, "", 0, None));
    }

    #[test]
    fn several_items_and_drive_lists_follow_their_flags_and_learned_kinds() {
        assert_eq!(KINDS.len(), 5);
        assert_eq!((kind_target(3), kind_target(4), kind_target(5)), (Some(menu::Target::Multiple), Some(menu::Target::Drive), None));
        let mut e = MenuEntry::custom("custom.1", "Compare", "cmp.exe", "{selection}");
        e.show_for = ShowFor { files: true, folders: false, background: false, multiple: true, drives: false };
        assert!(entry_shown(&e, "", 0, Some(3)) && !entry_shown(&e, "", 0, Some(4)) && entry_shown(&e, "", 0, Some(0)));
        e.show_for.drives = true;
        e.show_for.multiple = false;
        assert!(!entry_shown(&e, "", 0, Some(3)) && entry_shown(&e, "", 0, Some(4)));
        // learned items: every kind of old lists them; Several / Drive only after such a menu showed them
        let mut p = pref("windows.verb:share", "Share", "Windows");
        assert!(item_shown_for(&p, "", 0, Some(0)) && item_shown_for(&p, "", 0, Some(2)) && item_shown_for(&p, "", 0, None));
        assert!(!item_shown_for(&p, "", 0, Some(3)) && !item_shown_for(&p, "", 0, Some(4)), "not learned for them yet (the empty-state note)");
        p.kinds = menu::Target::Drive.bit();
        assert!(item_shown_for(&p, "", 0, Some(4)) && !item_shown_for(&p, "", 0, Some(3)) && !item_shown_for(&p, "sha", 2, Some(4)));
        let counts = chip_counts(&menu::default_entries(), &[p], Some(4));
        let real = menu::default_entries().iter().filter(|e| !e.is_separator() && e.show_for.drives).count();
        assert_eq!(counts[0], real + 1, "the learned drive item counts under All");
        assert!(kind_empty_note(true).contains("drive") && kind_empty_note(false).contains("several"));
    }

    #[test]
    fn the_add_form_keeps_its_show_for_boxes() {
        let b = AddForm::blank();
        assert_eq!(b.show, ShowFor { files: true, folders: true, background: false, multiple: true, drives: false });
        let mut s = b.show;
        for (i, want) in [(0, true), (1, true), (2, false), (3, true), (4, false)] {
            assert_eq!(*show_for_flag(&mut s, i), want, "{}", SHOW_FOR[i]);
        }
        *show_for_flag(&mut s, 4) = true;
        assert!(s.drives);
        let mut e = MenuEntry::custom("custom.1", "X", "x.exe", "");
        e.show_for = s;
        assert_eq!(edit_form(&e).show, s, "Edit entry opens with the entry's own boxes");
        assert!(new_entry(&menu::default_entries(), "N", "n.exe", "{path}", "").unwrap().show_for.multiple);
    }

    #[test]
    fn name_in_menu_switches_per_group() {
        let mut users = vec![menu::UserGroup { id: "g1".into(), name: "A".into(), ..Default::default() }, menu::UserGroup { id: "g2".into(), name: "B".into(), ..Default::default() }];
        assert!(set_group_name_shown(&mut users, "g2", true));
        assert!(!set_group_name_shown(&mut users, "g2", true), "no change");
        assert!(!set_group_name_shown(&mut users, "nope", true));
        assert_eq!((users[0].show_name, users[1].show_name), (false, true), "per group, off by default");
        assert!(set_group_name_shown(&mut users, "g2", false));
    }

    #[test]
    fn chip_counts_add_entries_and_items() {
        let list = menu::default_entries();
        let items = vec![pref("windows.verb:share", "Share", "Windows"), pref("windows.pkg:A.B_1/{x}", "A", "A.B_1"), pref("app:x.copy-name", "Copy name", "Tsubame")];
        let c = chip_counts(&list, &items, None);
        let real = list.iter().filter(|e| !e.is_separator()).count();
        assert_eq!(c[0], real + 3);
        assert_eq!(c[2], 1);
        assert_eq!(c[4], list.iter().filter(|e| e.hidden && !e.is_separator()).count());
    }

    #[test]
    fn dropping_an_item_on_a_group() {
        let mut share = pref("windows.verb:share", "Share", "Windows");
        assert_eq!(drop_group(&share, &menu::GroupRef::User("g1".into())), Some("g1".to_string()));
        assert_eq!(drop_group(&share, &menu::GroupRef::Builtin(menu::Group::Windows)), Some(String::new()), "home group = back");
        assert_eq!(drop_group(&share, &menu::GroupRef::Builtin(menu::Group::Apps)), None);
        let users = vec![menu::UserGroup { id: "g1".into(), name: "T".into(), pos: 0, ..Default::default() }];
        share.group = "g1".into();
        assert_eq!(item_group(&share, &users), menu::GroupRef::User("g1".into()));
        share.group = "gone".into();
        assert_eq!(item_group(&share, &users), menu::GroupRef::Builtin(menu::Group::Windows), "a deleted group falls back");
    }

    fn default_more() -> MenuEntry {
        menu::default_entries().into_iter().find(|e| e.id == "windows.more").unwrap()
    }

    #[test]
    fn reset_keeps_learned_items_but_drops_every_edit() {
        let mut m = tsubame_config::ContextMenu::default();
        assert!(!menu_changed(&m));
        m.items.push(menu::ItemPref { hidden: true, name: "X".into(), new: true, ..pref("windows.verb:share", "Share", "Windows") });
        m.groups = vec![menu::Group::Custom];
        m.entries = menu::default_entries();
        assert!(menu_changed(&m));
        m.user_groups.push(menu::UserGroup { id: "g1".into(), name: "T".into(), pos: 0, ..Default::default() });
        m.items[0].group = "g1".into();
        assert!(menu_changed(&m));
        reset_menu(&mut m);
        assert!(!menu_changed(&m) && m.items[0].group.is_empty());
        assert_eq!(m.items.len(), 1);
        assert!(!m.items[0].hidden && m.items[0].name.is_empty());
    }

    #[test]
    fn group_counts_follow_the_groups() {
        let mut list = menu::default_entries();
        list.push(MenuEntry::custom("custom.1", "VS Code", "code.exe", ""));
        let items = vec![pref("windows.verb:share", "Share", "Windows"), pref("windows.pkg:A_1/{x}", "A", "A_1"), pref("app:x.copy-name", "Copy name", "Tsubame")];
        let b = |g| menu::GroupRef::Builtin(g);
        assert_eq!(group_count(&list, &items, &[], &b(menu::Group::Custom)), 1);
        assert_eq!(group_count(&list, &items, &[], &b(menu::Group::Windows)), 1);
        assert_eq!(group_count(&list, &items, &[], &b(menu::Group::Apps)), 1);
        // built-ins + the classic-menu row + Copy name (Open with is filled by Windows)
        assert_eq!(group_count(&list, &items, &[], &b(menu::Group::Tsubame)), list.iter().filter(|e| !e.is_separator() && e.id != "windows.open-with" && !e.source.runs_program()).count() + 1);
    }

    fn gorder() -> Vec<menu::GroupRef> {
        let mut m = tsubame_config::ContextMenu::default();
        m.user_groups.push(menu::UserGroup { id: "g1".into(), name: "Mine".into(), pos: 2, ..Default::default() });
        menu::full_order(&m.groups, &m.user_groups)
    }

    #[test]
    fn group_keys_rename_delete_and_move() {
        let alt = Mods { alt: true, ..Mods::NONE };
        // F2 and Enter rename, Delete deletes: only the user's own groups
        assert_eq!(group_key(Key::F(2), Mods::NONE, true), Some(GroupKey::Rename));
        assert_eq!(group_key(Key::Enter, Mods::NONE, true), Some(GroupKey::Rename));
        assert_eq!(group_key(Key::Delete, Mods::NONE, true), Some(GroupKey::Delete));
        assert_eq!(group_key(Key::F(2), Mods::NONE, false), None);
        assert_eq!(group_key(Key::Enter, Mods::NONE, false), None);
        assert_eq!(group_key(Key::Delete, Mods::NONE, false), None);
        // Alt+Up / Alt+Down move any group; plain or other-modifier arrows do not
        assert_eq!(group_key(Key::Up, alt, false), Some(GroupKey::Move(-1)));
        assert_eq!(group_key(Key::Down, alt, true), Some(GroupKey::Move(1)));
        assert_eq!(group_key(Key::Up, Mods::NONE, true), None);
        assert_eq!(group_key(Key::Down, Mods::CTRL, true), None);
        assert_eq!(group_key(Key::Delete, Mods::SHIFT, true), None);
        assert_eq!(group_key(Key::Enter, alt, true), None);
    }

    #[test]
    fn a_group_moves_one_place_and_stays_put_at_the_ends() {
        let o = gorder();
        let k = o.iter().position(|g| *g == menu::GroupRef::User("g1".into())).unwrap();
        let up = move_group(&o, k, -1).unwrap();
        assert_eq!(up[k - 1], menu::GroupRef::User("g1".into()));
        assert_eq!(up[k], o[k - 1]);
        let down = move_group(&o, k, 1).unwrap();
        assert_eq!(down[k + 1], menu::GroupRef::User("g1".into()));
        assert_eq!(move_group(&o, 0, -1), None);
        assert_eq!(move_group(&o, o.len() - 1, 1), None);
        assert_eq!(move_group(&o, o.len(), -1), None);
        // stored like a drag: the built-in order and the user group's place round-trip
        let mut m = tsubame_config::ContextMenu::default();
        m.user_groups.push(menu::UserGroup { id: "g1".into(), name: "Mine".into(), pos: 2, ..Default::default() });
        menu::set_full_order(&mut m.groups, &mut m.user_groups, &up);
        assert_eq!(menu::full_order(&m.groups, &m.user_groups), up);
        let top = move_group(&up, 0, 1).unwrap();
        menu::set_full_order(&mut m.groups, &mut m.user_groups, &top);
        assert_eq!(menu::full_order(&m.groups, &m.user_groups), top);
    }

    #[test]
    fn deleting_a_group_by_key_frees_its_items() {
        let mut users = vec![menu::UserGroup { id: "g1".into(), name: "Mine".into(), pos: 0, ..Default::default() }];
        let mut items = vec![menu::ItemPref { group: "g1".into(), ..pref("windows.verb:share", "Share", "Windows") }];
        assert_eq!(group_key(Key::Delete, Mods::NONE, true), Some(GroupKey::Delete));
        assert!(menu::delete_group(&mut users, &mut items, "g1"));
        assert!(users.is_empty() && items[0].group.is_empty());
    }

    #[test]
    fn shortcuts_come_from_the_key_map() {
        let km = KeyMap::defaults();
        let e = MenuEntry::builtin("builtin.rename", "Rename", "file.rename");
        assert_eq!(shortcut_of(&e, &km), "F2");
        let e = MenuEntry::builtin("builtin.copy", "Copy", "file.copy");
        assert_eq!(shortcut_of(&e, &km), "Ctrl+C");
    }
}
