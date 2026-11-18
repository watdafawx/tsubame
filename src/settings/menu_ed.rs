//! Settings > Context menu: the editable right-click menu. Hide items with the eye, drag the
//! grip to reorder (drop on a submenu to move inside), double-click a name to rename, add custom
//! commands (name + program + arguments + optional extensions), import a Nilesoft `shell.nss`.
//!
//! The list is `[[context_menu.entries]]` of the config. The native Windows shell menu stays;
//! Tsubame only appends the enabled custom entries to it (`shellops`).

use super::lay::{Look, Pg, button, fit, wrap};
use super::{C_MENU, Cx};
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

#[derive(Default)]
pub struct State {
    sel: Option<String>,
    /// renaming: (entry id, field)
    edit: Option<(String, TextInput)>,
    edit_fresh: bool,
    /// index of the entry being dragged
    drag: Option<usize>,
    add: Option<Box<AddForm>>,
    import: Option<Box<ImportForm>>,
    msg: Option<(String, bool)>,
    confirm_reset: bool,
}

#[derive(Default)]
struct AddForm {
    name: TextInput,
    cmd: TextInput,
    args: TextInput,
    ext: TextInput,
    err: Option<String>,
}

struct ImportForm {
    path: TextInput,
    err: Option<String>,
}

impl State {
    pub fn editing(&self) -> bool {
        self.edit.is_some() || self.add.is_some() || self.import.is_some()
    }
    pub fn reset_transient(&mut self) {
        *self = State::default();
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

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("Context menu", "The menu that opens when you right-click a file or folder. Windows shell extensions stay available under \u{201c}Show more options\u{201d}. Custom commands are added to it.");
    let t = pg.t;
    let mut list = menu::effective(&cx.prefs.context_menu.entries);
    let mut modified = false;

    // toolbar
    let ty = pg.y;
    let mut x = pg.x;
    let (add, r) = button(pg.ui, pg.g, ID_TB.with(1), x, ty, "+  Add custom command", Look::Primary, true, false);
    x = r.r() + 8.0;
    let (sub, r) = button(pg.ui, pg.g, ID_TB.with(2), x, ty, "New submenu", Look::Normal, true, false);
    x = r.r() + 8.0;
    let (sep, r) = button(pg.ui, pg.g, ID_TB.with(3), x, ty, "Add separator", Look::Normal, true, false);
    x = r.r() + 8.0;
    let (imp, _) = button(pg.ui, pg.g, ID_TB.with(4), x, ty, "Import from Nilesoft Shell\u{2026}", Look::Normal, true, false);
    let reset_label = if st.confirm_reset { "Click again to reset" } else { "Reset menu" };
    let (reset, _) = button(pg.ui, pg.g, ID_TB.with(5), pg.x + pg.w, ty, reset_label, if st.confirm_reset { Look::DangerFill } else { Look::Danger }, !cx.prefs.context_menu.entries.is_empty(), true);
    pg.y += t.height_button + 12.0;
    if add {
        st.add = if st.add.is_some() { None } else { Some(Box::default()) };
        st.import = None;
        st.msg = None;
    }
    if imp {
        let default = std::env::var("ProgramFiles").map(|p| format!("{p}\\Nilesoft Shell\\shell.nss")).unwrap_or_default();
        st.import = if st.import.is_some() { None } else { Some(Box::new(ImportForm { path: TextInput::new(if std::path::Path::new(&default).is_file() { &default } else { "" }), err: None })) };
        st.add = None;
        st.msg = None;
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
            cx.prefs.context_menu.entries.clear();
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
    // import form
    if let Some(form) = st.import.as_mut() {
        let r = Rect::new(pg.x, pg.y, pg.w, 100.0);
        pg.g.round(r, t.radius_lg, t.panel);
        pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
        let fw = (r.w - 32.0).min(620.0);
        label_field(pg, ID_FORM.with(20), "shell.nss", &mut form.path, r.x + 16.0, r.y + 14.0, fw, "C:\\Program Files\\Nilesoft Shell\\shell.nss");
        pg.g.text("Simple items, menus and separators are read; scripts and conditions are skipped and listed. The file is only read.", Rect::new(r.x + 16.0, r.y + 50.0, r.w - 32.0, 16.0), t.font_small(), t.text_dim);
        let (go, rr) = button(pg.ui, pg.g, ID_FORM.with(21), r.x + 16.0, r.y + 62.0, "Import", Look::Primary, true, false);
        let (cancel, _) = button(pg.ui, pg.g, ID_FORM.with(22), rr.r() + 8.0, r.y + 62.0, "Cancel", Look::Normal, true, false);
        if let Some(e) = &form.err {
            let shown = fit(pg.g, e, t.font_small(), r.w - 240.0);
            pg.g.text(&shown, Rect::new(rr.r() + 100.0, r.y + 62.0, r.w - 240.0, t.height_button), t.font_small(), t.danger);
        }
        pg.y += r.h + 12.0;
        let mut done = cancel;
        if go {
            match read_nss(form.path.text.trim()) {
                Ok(src) => {
                    let imp = nss::import(&src, &list, "");
                    let n = imp.imported;
                    let skipped = imp.skipped.len();
                    list.extend(imp.entries);
                    modified |= n > 0;
                    let mut msg = format!("Imported {n} {}.", if n == 1 { "entry" } else { "entries" });
                    if skipped > 0 {
                        msg.push_str(&format!(" Skipped {skipped}: {}", imp.skipped.iter().take(2).map(|s| format!("line {} ({})", s.line, s.reason)).collect::<Vec<_>>().join("; ")));
                    }
                    st.msg = Some((msg, false));
                    done = true;
                }
                Err(e) => form.err = Some(e),
            }
        }
        if done {
            st.import = None;
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

    // list and preview
    let show_preview = pg.w >= 720.0;
    let list_w = if show_preview { pg.w - 258.0 } else { pg.w };
    let rh = t.height_menu_edit_row;
    let top = pg.y;
    let card = Rect::new(pg.x, top, list_w, list.len() as f32 * rh + 2.0);
    pg.g.round(card, t.radius_lg, t.panel);
    pg.g.round_stroke(card, t.radius_lg, t.border, 1.0);

    let mut hide_toggle: Option<usize> = None;
    let mut delete: Option<usize> = None;
    let mut start_edit: Option<usize> = None;
    let mut grip_press: Option<usize> = None;
    let mut commit: Option<(String, String)> = None;
    let mut cancel_edit = false;
    for (i, e) in list.iter().enumerate() {
        let r = Rect::new(card.x + 1.0, card.y + 1.0 + i as f32 * rh, card.w - 2.0, rh);
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
        if i > 0 {
            pg.g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        }
        let dim = e.hidden;
        // grip
        let gr = Rect::new(r.x + 6.0 + d * 18.0, r.y, 18.0, r.h);
        let gi = pg.ui.interact(ID_GRIP.with(i as u64), gr, Sense::click().cursor(Cursor::Hand));
        for (dx, dy) in [(0.0, -4.0), (4.0, -4.0), (0.0, 0.0), (4.0, 0.0), (0.0, 4.0), (4.0, 4.0)] {
            pg.g.circle(gr.x + 7.0 + dx, r.y + r.h / 2.0 + dy, 1.0, if gi.hovered { t.text } else { t.text_disabled });
        }
        if gi.pressed.is_some() {
            grip_press = Some(i);
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
            let tag = if e.is_submenu() { "Submenu" } else { e.source.label() };
            let tw = pg.g.measure(tag, t.font_small()) + 12.0;
            let tr = Rect::new(rx - tw, r.y + 5.0, tw, r.h - 10.0);
            pg.g.round_stroke(tr, t.radius_sm, t.popup_border, 1.0);
            pg.g.text(tag, tr, t.font_small().c(), if e.source == Source::Custom { t.accent } else { t.text_dim });
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
                pg.g.text(&name, Rect::new(nx, r.y, (rx - nx).max(40.0), r.h), t.font_md(), if dim { t.text_disabled } else { t.text });
                if e.is_submenu() {
                    let n = list.iter().filter(|c| c.parent == e.id).count();
                    let nw = pg.g.measure(&e.name, t.font_md());
                    pg.g.text(&format!("\u{2014} submenu, {n} item{}", if n == 1 { "" } else { "s" }), Rect::new(nx + nw + 8.0, r.y, 200.0, r.h), t.font_small(), t.text_dim);
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
        if !text.is_empty()
            && let Some(e) = list.iter_mut().find(|e| e.id == id)
            && e.name != text
        {
            e.name = text;
            modified = true;
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

    // preview
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
        let hint = wrap(pg.g, "Hidden items and the contents of submenus are not shown here. Matches what right-click shows.", t.font_small(), pw - 24.0);
        let hr = Rect::new(px, pr.b() + 10.0, pw, 16.0 + hint.len() as f32 * 15.0);
        pg.g.round(hr, t.radius_md, t.panel);
        pg.g.round_stroke(hr, t.radius_md, t.border, 1.0);
        for (i, l) in hint.iter().enumerate() {
            pg.g.text(l, Rect::new(hr.x + 12.0, hr.y + 8.0 + i as f32 * 15.0, pw - 24.0, 15.0), t.font_small(), t.text_dim);
        }
    }
    pg.y = top + card.h + 12.0;
    let note = "Drag the grip to reorder, click the eye to hide, double-click a name to rename, drop on a submenu to move inside it. Built-in items can be hidden and renamed but not deleted.";
    pg.note("\u{E946}", note, false);

    if modified {
        // keep an unchanged default menu out of the file
        cx.prefs.context_menu.entries = if list == menu::default_entries() { Vec::new() } else { list };
        cx.changed |= C_MENU;
        st.confirm_reset = false;
    }
}

fn read_nss(path: &str) -> Result<String, String> {
    if path.is_empty() {
        return Err("Type the path of a shell.nss file.".into());
    }
    let p = std::path::Path::new(path.trim_matches('"'));
    let meta = std::fs::metadata(p).map_err(|_| "That file does not exist.".to_string())?;
    if meta.len() > 512 * 1024 {
        return Err("That file is too big for a menu definition (over 512 KB).".into());
    }
    std::fs::read_to_string(p).map_err(|e| format!("Could not read it: {e}"))
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

    #[test]
    fn shortcuts_come_from_the_key_map() {
        let km = KeyMap::defaults();
        let e = MenuEntry::builtin("builtin.rename", "Rename", "file.rename");
        assert_eq!(shortcut_of(&e, &km), "F2");
        let e = MenuEntry::builtin("builtin.copy", "Copy", "file.copy");
        assert_eq!(shortcut_of(&e, &km), "Ctrl+C");
    }
}
