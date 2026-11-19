//! Settings > Context menu: the editable right-click menu. Hide items with the eye, drag the
//! grip to reorder (drop on a submenu to move inside), double-click a name to rename, add custom
//! commands (name + program + arguments + optional extensions), import a Nilesoft `shell.nss`.
//!
//! The list is `[[context_menu.entries]]` of the config. The native Windows shell menu stays;
//! Tsubame only appends the enabled custom entries to it (`shellops`).

use super::lay::{Look, Pg, button, fit, wrap};
use super::{C_MENU, Cx, nss_pick};
use crate::keymap::KeyMap;
use tsubame_config::menu::{self, EntryKind, MenuEntry, ShowFor, Source};
use tsubame_config::nss;
use tsubame_ui::{Button, Cursor, Id, Rect, Sense, TextInput, f, text_input};

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
const ID_GEDIT: Id = Id::new("menu-group-rename");
const ID_IGRIP: Id = Id::new("menu-item-grip");
const ID_HDR: Id = Id::new("menu-item-hdr");

/// The filter chips (mock 6b). "3rd party" = a packaged (Win11) item whose
/// package family is not Microsoft's (`menu::is_third_party`): the classic menu
/// exposes no owning program, so nothing else can be told apart.
const CHIPS: [&str; 5] = ["All", "Windows", "3rd party", "Tsubame", "Hidden"];

/// The kind switch (mock 6b): which right-click the list is shown for. The
/// config has `show_for` flags per entry (file, folder, background); "Several
/// items" and "Drive" have no data behind them, so they are not offered.
const KINDS: [&str; 3] = ["File", "Folder", "Empty area"];

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
    err: Option<String>,
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
    e.show_for = ShowFor { files: true, folders: true, background: false };
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
pub fn item_shown(p: &menu::ItemPref, q: &str, chip: usize) -> bool {
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
        _ => true,
    };
    chip_ok && kind_ok && (q.is_empty() || (!e.is_separator() && e.name.to_lowercase().contains(q)))
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
                menu::Group::Custom => entries.filter(|e| e.source.runs_program()).count(),
                _ => 0,
            }
        }
        menu::GroupRef::User(_) => 0,
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

/// What each chip counts (the small number on the chip).
pub fn chip_counts(list: &[MenuEntry], items: &[menu::ItemPref], kind: Option<usize>) -> [usize; 5] {
    let mut n = [0usize; 5];
    for (c, slot) in n.iter_mut().enumerate() {
        *slot = list.iter().filter(|e| !e.is_separator() && entry_shown(e, "", c, kind)).count() + items.iter().filter(|p| item_shown(p, "", c)).count();
    }
    n
}

/// Reset to defaults: the default list and group order, every learned item
/// shown under its own name again (the items stay known).
pub fn reset_menu(m: &mut tsubame_config::ContextMenu) {
    m.entries.clear();
    m.groups.clear();
    m.user_groups.clear();
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

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
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
    let hint = if st.kind.is_some() { "Entries that show for this right-click. Items learned from Windows are the same for every kind." } else { "Pick a kind to see which entries show for it." };
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
    let (newg, r) = button(pg.ui, pg.g, ID_TB.with(6), x, ty, "New group", Look::Normal, true, false);
    x = r.r() + 8.0;
    let (imp, _) = button(pg.ui, pg.g, ID_TB.with(4), x, ty, "Import from Nilesoft Shell\u{2026}", Look::Normal, true, false);
    let reset_label = if st.confirm_reset { "Click again to reset" } else { "Reset to defaults" };
    let (reset, _) = button(pg.ui, pg.g, ID_TB.with(5), pg.x + pg.w, ty, reset_label, if st.confirm_reset { Look::DangerFill } else { Look::Danger }, menu_changed(&cx.prefs.context_menu), true);
    pg.y += t.height_button + 12.0;
    if add {
        st.add = if st.add.is_some() { None } else { Some(Box::default()) };
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
                modified |= run_import(st, &mut list, &path);
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
        let fh = 4.0 * 36.0 + 12.0 + 44.0;
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
        pg.g.text("{path} first selected item   {selection} every selected item   {dir} this folder   {name} {ext} {parent}", Rect::new(fx, y - 2.0, r.w - 32.0, 16.0), t.font_small(), t.text_dim);
        y += 20.0;
        let (ok, rr) = button(pg.ui, pg.g, ID_FORM.with(10), fx, y, "Add to menu", Look::Primary, true, false);
        let (cancel, _) = button(pg.ui, pg.g, ID_FORM.with(11), rr.r() + 8.0, y, "Cancel", Look::Normal, true, false);
        if let Some(e) = &form.err {
            pg.g.text(e, Rect::new(rr.r() + 100.0, y, r.w - 240.0, t.height_button), t.font_small(), t.danger);
        }
        let mut done = false;
        if ok {
            let args = if form.args.text.trim().is_empty() { "\"{path}\"".to_string() } else { form.args.text.clone() };
            match new_entry(&list, &form.name.text, &form.cmd.text, &args, &form.ext.text) {
                Ok(e) => {
                    list.push(e);
                    modified = true;
                    done = true;
                    st.msg = Some(("Added. It shows at the end of the menu.".into(), false));
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
    if st.report.is_none() {
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
                    modified |= run_import(st, &mut list, &d.path);
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
        let sub = fit(pg.g, "Added at the end of the menu, tagged \u{201C}From Nilesoft\u{201D}. The original file was not changed.", t.font_small(), hw);
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
            rep.undo(&mut list);
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
    for (k, g) in order.iter().enumerate() {
        let r = Rect::new(gcard.x + 1.0, gcard.y + 1.0 + k as f32 * rh, gcard.w - 2.0, rh);
        if k > 0 {
            pg.g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        }
        if st.gdrag == Some(k) {
            pg.g.fill(r, t.selected);
        }
        let uid = if let menu::GroupRef::User(id) = g { Some(id.clone()) } else { None };
        let row = pg.ui.interact(ID_GROW.with(k as u64), r, Sense::click().cursor(Cursor::Arrow));
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
        tag(pg, rx, r.y + 5.0, r.h - 10.0, if uid.is_some() { "Your group" } else { "Group" }, uid.is_some(), false);
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
    if let Some(id) = gstart {
        if let Some(u) = cx.prefs.context_menu.user_groups.iter().find(|u| u.id == id) {
            st.gedit = Some((id, TextInput::new(&u.name)));
            st.edit_fresh = true;
        }
    }
    if let Some((id, text)) = gcommit.take() {
        // an empty name keeps the old one
        if let Some(u) = cx.prefs.context_menu.user_groups.iter_mut().find(|u| u.id == id) {
            if !text.is_empty() && u.name != text {
                u.name = text;
                cx.changed |= C_MENU;
            }
        }
        st.gedit = None;
    }
    if gcancel {
        st.gedit = None;
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
    let mut shown: Vec<usize> = (0..cx.prefs.context_menu.items.len()).filter(|&i| !filtered || item_shown(&cx.prefs.context_menu.items[i], q, st.chip)).collect();
    // grouped like the menu (stable inside a group: first seen first)
    shown.sort_by_key(|&i| {
        let g = item_group(&cx.prefs.context_menu.items[i], &users);
        order.iter().position(|x| *x == g).unwrap_or(usize::MAX)
    });
    // rows: a header per group (unfiltered: the user's groups always, a built-in one when it has items), then its items
    let mut rows: Vec<(Option<usize>, menu::GroupRef)> = Vec::new();
    for g in &order {
        let mine: Vec<usize> = shown.iter().copied().filter(|&i| item_group(&cx.prefs.context_menu.items[i], &users) == *g).collect();
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
        for (dx, dy) in [(0.0, -4.0), (4.0, -4.0), (0.0, 0.0), (4.0, 0.0), (0.0, 4.0), (4.0, 4.0)] {
            pg.g.circle(gr.x + 7.0 + dx, r.y + r.h / 2.0 + dy, 1.0, if gi.hovered { t.text } else { t.text_disabled });
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
        // right: where it comes from, New
        let mut rx = r.r() - 10.0;
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
}

/// Reads `path` (never writes it), adds what Tsubame can run to `list` and keeps the report
/// for the page. Returns whether `list` changed.
fn run_import(st: &mut State, list: &mut Vec<MenuEntry>, path: &std::path::Path) -> bool {
    match nss::read_file(path) {
        Ok(src) => {
            let imp = nss::import(&src, list, "");
            let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "shell.nss".into());
            let changed = !imp.entries.is_empty();
            st.report = Some(nss::Report::new(&file, &imp));
            st.msg = if imp.imported == 0 { Some(("Nothing in that file can be imported. The report lists why.".into(), true)) } else { None };
            list.extend(imp.entries);
            changed
        }
        Err(e) => {
            st.msg = Some((e, true));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn the_kind_switch_follows_show_for() {
        let mut e = MenuEntry::custom("custom.1", "VS Code", "code.exe", "");
        e.show_for = ShowFor { files: true, folders: false, background: true };
        assert!(entry_shown(&e, "", 0, Some(0)) && !entry_shown(&e, "", 0, Some(1)) && entry_shown(&e, "", 0, Some(2)) && entry_shown(&e, "", 0, None));
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
        let users = vec![menu::UserGroup { id: "g1".into(), name: "T".into(), pos: 0 }];
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
        m.user_groups.push(menu::UserGroup { id: "g1".into(), name: "T".into(), pos: 0 });
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

    #[test]
    fn shortcuts_come_from_the_key_map() {
        let km = KeyMap::defaults();
        let e = MenuEntry::builtin("builtin.rename", "Rename", "file.rename");
        assert_eq!(shortcut_of(&e, &km), "F2");
        let e = MenuEntry::builtin("builtin.copy", "Copy", "file.copy");
        assert_eq!(shortcut_of(&e, &km), "Ctrl+C");
    }
}
