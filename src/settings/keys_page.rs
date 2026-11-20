//! Settings > Keys: every command with its shortcuts. Click a row to record a new shortcut
//! (up to two per command). A chord another command already runs shows a warning first.
//! Changes apply at once (`C_KEYS`) and persist as `[keys.overrides]`.

use super::lay::{Look, Pg, button, fit, wrap};
use super::{C_KEYS, Cx};
use crate::commands;
use crate::keymap::{self, CATEGORIES, Chord, KeyMap, Reject, chord_text, is_modifier_key};
use tsubame_ui::{Button, Cursor, Event, Id, Key, Mods, Rect, Sense, TextInput, f, keycap, text_input};

const ID_SEARCH: Id = Id::new("keys-search");
const ID_ROW: Id = Id::new("keys-row");
const ID_REC: Id = Id::new("keys-recorder");
const ID_BTN: Id = Id::new("keys-btn");
const ID_RESET: Id = Id::new("keys-reset-row");
const ID_ALL: Id = Id::new("keys-reset-all");

const HEAD_H: f32 = 28.0;

impl State {
    /// "Show shortcut" from the palette: the list is searched for this command.
    pub fn focus_command(&mut self, title: &str) {
        self.rec = None;
        self.search.set_text(title);
        self.focus_first = true;
    }
}

#[derive(Default)]
pub struct State {
    search: TextInput,
    rec: Option<Rec>,
    /// "Show shortcut": the first row found takes the keyboard focus (Enter records)
    focus_first: bool,
    /// the "Reset all" button asked once ("Click again to confirm")
    confirm_all: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rec {
    pub cmd: u32,
    pub slot: usize,
    pub pending: Option<Chord>,
    /// the command that owns `pending` now
    pub conflict: Option<u32>,
    pub msg: Option<String>,
}

impl Rec {
    pub fn new(cmd: u32, slot: usize) -> Rec {
        Rec { cmd, slot, pending: None, conflict: None, msg: None }
    }
}

/// What a key did to the recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecOut {
    /// nothing visible happened (a modifier, a refused key shows a message)
    Stay,
    /// the recorder closes, nothing changed
    Cancel,
    /// the key map changed; the recorder closes
    Changed,
}

fn reject_text(r: Reject) -> &'static str {
    match r {
        Reject::NeedsModifier => "A plain key would break typing. Add Ctrl or Alt.",
        Reject::Reserved => "Esc, Enter, Tab and the arrow keys keep their job. Add a modifier.",
        Reject::Windows => "Windows keeps this shortcut.",
    }
}

/// One key press while recording. Esc cancels, Backspace clears the slot, Enter accepts a
/// chord that has a conflict ("assign here, unbind there"), any other chord is bound at once
/// when nothing else runs it, else it waits for a decision.
pub fn on_key(rec: &mut Rec, km: &mut KeyMap, key: Key, mods: Mods) -> RecOut {
    if is_modifier_key(key) {
        return RecOut::Stay;
    }
    if mods.none() {
        match key {
            Key::Escape => return RecOut::Cancel,
            Key::Backspace => {
                km.clear_slot(rec.cmd, rec.slot);
                return RecOut::Changed;
            }
            Key::Enter if rec.pending.is_some() => return accept(rec, km),
            _ => {}
        }
    }
    let c = Chord::new(key, mods);
    if let Err(why) = keymap::bindable(c) {
        rec.msg = Some(reject_text(why).to_string());
        rec.pending = None;
        rec.conflict = None;
        return RecOut::Stay;
    }
    rec.msg = None;
    rec.pending = Some(c);
    rec.conflict = km.conflict(c, rec.cmd).map(|k| k.command);
    if rec.conflict.is_some() {
        return RecOut::Stay;
    }
    accept(rec, km)
}

/// Binds the pending chord (taking it from its owner).
pub fn accept(rec: &mut Rec, km: &mut KeyMap) -> RecOut {
    match rec.pending {
        Some(c) => match km.assign(rec.cmd, rec.slot, c) {
            Ok(_) => RecOut::Changed,
            Err(why) => {
                rec.msg = Some(reject_text(why).to_string());
                RecOut::Stay
            }
        },
        None => RecOut::Stay,
    }
}

impl State {
    pub fn reset_transient(&mut self) {
        self.rec = None;
        self.confirm_all = false;
    }
}

enum Item {
    Head(&'static str, usize),
    Row(usize),
}

fn rec_height(rec: &Rec) -> f32 {
    let base = 8.0 + 36.0 + 8.0 + 30.0 + 12.0;
    if rec.conflict.is_some() { base + 46.0 + 8.0 } else { base }
}

fn matches(q: &str, km: &KeyMap, i: usize) -> bool {
    if q.is_empty() {
        return true;
    }
    let c = &km.commands()[i];
    c.title.to_lowercase().contains(q) || c.name.contains(q) || km.chords(c.id).iter().any(|ch| chord_text(*ch).to_lowercase().contains(q))
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("Keys", "Every command has a name and up to two shortcuts. Click a row, then press the keys you want. The command palette (Ctrl+Shift+P) shows the same shortcuts.");
    let t = pg.t;
    // toolbar: search, count, reset all
    let tb = Rect::new(pg.x, pg.y, pg.w, t.height_field);
    st.search.pad_left = 22.0;
    let sr = Rect::new(tb.x, tb.y, 380.0_f32.min(pg.w - 200.0), tb.h);
    text_input(pg.ui, pg.g, ID_SEARCH, sr, &mut st.search, "Search commands or shortcuts");
    pg.g.text("\u{E721}", Rect::new(sr.x + 6.0, sr.y, 16.0, sr.h), f(12.0).icon().c(), t.text_dim);
    if pg.ui.has_focus(ID_SEARCH) && pg.ui.window_focused() {
        pg.ui.mark_text_focus();
    }
    let changed_n = cx.keymap.changed();
    let label = if st.confirm_all { "Click again to reset all" } else { "Reset all" };
    let (clicked, br) = button(pg.ui, pg.g, ID_ALL, tb.r(), tb.y, label, if st.confirm_all { Look::DangerFill } else { Look::Danger }, changed_n > 0, true);
    if clicked {
        if st.confirm_all {
            cx.keymap.reset_all();
            st.rec = None;
            st.confirm_all = false;
            cx.changed |= C_KEYS;
        } else {
            st.confirm_all = true;
        }
    } else if st.confirm_all && pg.ui.active().is_none() && pg.ui.hot() != Some(ID_ALL) && pg.ui.mouse_in() && !br.contains_pt(pg.ui.mouse()) {
        // a click elsewhere takes the question back
    }
    if changed_n > 0 {
        let s = format!("{changed_n} changed");
        pg.g.text(&s, Rect::new(br.x - 100.0, tb.y, 92.0, tb.h), t.font_small().r(), t.text_dim);
    }
    pg.y += tb.h + 12.0;

    // the list
    let q = st.search.text.trim().to_lowercase();
    let mut items = Vec::new();
    for cat in CATEGORIES {
        let cmds = cx.keymap.commands();
        let rows: Vec<usize> = (0..cmds.len()).filter(|&i| keymap::category(&cmds[i].name) == *cat && matches(&q, cx.keymap, i)).collect();
        if !rows.is_empty() {
            items.push(Item::Head(cat, rows.len()));
            items.extend(rows.into_iter().map(Item::Row));
        }
    }
    if items.is_empty() {
        pg.g.text("No command matches.", Rect::new(pg.x + 4.0, pg.y, pg.w, 24.0), t.font_md(), t.text_dim);
        pg.y += 30.0;
        return;
    }
    let rec_id = st.rec.as_ref().map(|r| r.cmd);
    let total: f32 = items
        .iter()
        .map(|it| match it {
            Item::Head(..) => HEAD_H,
            Item::Row(i) => t.height_key_row + if rec_id == Some(cx.keymap.commands()[*i].id) { rec_height(st.rec.as_ref().unwrap()) } else { 0.0 },
        })
        .sum();
    let card = pg.card_h(total, t.height_key_row);
    let mut y = card.r.y;
    let col_x = card.r.x + (card.r.w * 0.56).min(card.r.w - 330.0).max(200.0);
    let mut closed = false;
    let mut applied = false;
    for it in &items {
        match it {
            Item::Head(cat, n) => {
                let r = Rect::new(card.r.x, y, card.r.w, HEAD_H);
                if pg.visible(r) {
                    pg.g.fill(Rect::new(r.x + 1.0, r.y, r.w - 2.0, r.h), t.bg);
                    pg.g.text(cat, Rect::new(r.x + 16.0, r.y, 200.0, r.h), t.font_small().b(), t.text_dim);
                    let cw = pg.g.measure(cat, t.font_small().b());
                    pg.g.text(&n.to_string(), Rect::new(r.x + 24.0 + cw, r.y, 40.0, r.h), t.font_small(), t.text_disabled);
                }
                y += HEAD_H;
            }
            Item::Row(i) => {
                let cmd = cx.keymap.commands()[*i].clone();
                let open = rec_id == Some(cmd.id);
                let r = Rect::new(card.r.x, y, card.r.w, t.height_key_row);
                let h = t.height_key_row + if open { rec_height(st.rec.as_ref().unwrap()) } else { 0.0 };
                if pg.visible(Rect::new(r.x, r.y, r.w, h)) {
                    let id = ID_ROW.with(cmd.id as u64);
                    let it = pg.ui.interact(id, r, Sense::click().focusable().cursor(Cursor::Hand));
                    if std::mem::take(&mut st.focus_first) {
                        pg.ui.set_focus(Some(id));
                    }
                    if open {
                        pg.g.fill(Rect::new(r.x + 1.0, r.y, r.w - 2.0, h), t.selected_inactive);
                    } else if it.hovered {
                        pg.g.fill(Rect::new(r.x + 1.0, r.y, r.w - 2.0, r.h), t.hover);
                    }
                    pg.g.fill(Rect::new(r.x + 1.0, r.y, r.w - 2.0, 1.0), t.border);
                    let title = fit(pg.g, &cmd.title, t.font_md(), col_x - r.x - 32.0);
                    pg.g.text(&title, Rect::new(r.x + 16.0, r.y, col_x - r.x - 24.0, r.h), t.font_md(), t.text);
                    // chords
                    let cy = r.y + r.h / 2.0;
                    let list = cx.keymap.chords(cmd.id);
                    let mut x = col_x;
                    if list.is_empty() {
                        let w = pg.g.measure("unbound", t.font_mono()) + 12.0;
                        let br = Rect::new(x, cy - 9.0, w, 18.0);
                        pg.g.round_stroke(br, t.radius_sm, t.popup_border, 1.0);
                        pg.g.text("unbound", br, t.font_mono().c(), t.text_disabled);
                    }
                    for (n, ch) in list.iter().enumerate() {
                        if n > 0 {
                            pg.g.text("or", Rect::new(x + 6.0, r.y, 20.0, r.h), t.font_small(), t.text_dim);
                            x += 32.0;
                        }
                        for p in tsubame_ui::split_chord(&chord_text(*ch)) {
                            x = keycap(pg.g, &t, x, cy, p) + 3.0;
                        }
                    }
                    // reset this command
                    if !cx.keymap.is_default(cmd.id) {
                        let rr = Rect::new(r.r() - 40.0, r.y + 4.0, 24.0, r.h - 8.0);
                        let rid = ID_RESET.with(cmd.id as u64);
                        let ri = pg.ui.interact(rid, rr, Sense::click().cursor(Cursor::Hand));
                        if ri.hovered {
                            pg.g.round(rr, t.radius_sm, t.hover);
                        }
                        pg.g.text("\u{E777}", rr, f(12.0).icon().c(), t.text_dim);
                        if ri.clicked == Some(Button::Left) {
                            cx.keymap.reset(cmd.id);
                            applied = true;
                            if open {
                                closed = true;
                            }
                        }
                    }
                    // Enter / Space on the focused row opens the recorder too
                    let mut activate = it.clicked == Some(Button::Left);
                    if !open && pg.ui.has_focus(id) {
                        for (qi, ev) in pg.ui.key_input(id) {
                            if let Event::KeyDown { key: Key::Enter | Key::Space, mods, .. } = ev
                                && mods.none()
                            {
                                pg.ui.consume(qi);
                                activate = true;
                            }
                        }
                    }
                    if activate && !applied {
                        if open {
                            closed = true;
                        } else {
                            st.rec = Some(Rec::new(cmd.id, 0));
                            pg.ui.set_focus(Some(ID_REC));
                        }
                    }
                    if open && let Some(rec) = st.rec.as_mut() {
                        let panel = Rect::new(r.x, r.b(), r.w, h - r.h);
                        match recorder(pg, cx.keymap, rec, panel) {
                            RecOut::Stay => {}
                            RecOut::Cancel => closed = true,
                            RecOut::Changed => {
                                applied = true;
                                closed = true;
                            }
                        }
                    }
                }
                y += h;
            }
        }
    }
    if closed {
        st.rec = None;
        pg.ui.request_repaint();
    }
    if applied {
        cx.changed |= C_KEYS;
        pg.ui.request_repaint();
    }
    if st.confirm_all && pg.ui.mouse_in() {
        // any press outside the button (the list, the search) withdraws the question
        if pg.ui.active().is_some() && pg.ui.active() != Some(ID_ALL) {
            st.confirm_all = false;
        }
    }
}

/// The recorder panel under an open row. Returns what the keys did.
fn recorder(pg: &mut Pg, km: &mut KeyMap, rec: &mut Rec, p: Rect) -> RecOut {
    let t = pg.t;
    let mut out = RecOut::Stay;
    pg.ui.capture_keys = true;
    // the field takes the keyboard
    let fr = Rect::new(p.x + 16.0, p.y + 8.0, p.w - 32.0, 36.0);
    let _ = pg.ui.interact(ID_REC, fr, Sense::click().focusable());
    if !pg.ui.has_focus(ID_REC) {
        pg.ui.set_focus(Some(ID_REC));
    }
    for (qi, ev) in pg.ui.key_input(ID_REC) {
        match ev {
            Event::KeyDown { key, mods, .. } => {
                pg.ui.consume(qi);
                let r = on_key(rec, km, key, mods);
                if r != RecOut::Stay {
                    out = r;
                }
            }
            Event::Char(_) => pg.ui.consume(qi),
            _ => {}
        }
    }
    pg.g.round(fr, t.radius_md, t.field);
    pg.g.round_stroke(fr, t.radius_md, t.accent, 1.0);
    let cy = fr.y + fr.h / 2.0;
    let mut x = fr.x + 12.0;
    if let Some(c) = rec.pending {
        for part in tsubame_ui::split_chord(&chord_text(c)) {
            x = keycap(pg.g, &t, x, cy, part) + 3.0;
        }
        x += 4.0;
    }
    if pg.ui.caret_on && pg.ui.window_focused() {
        pg.g.fill(Rect::new(x.round(), cy - 8.0, 1.0, 16.0), t.text);
    }
    pg.ui.mark_text_focus();
    let (msg, col) = match &rec.msg {
        Some(m) => (m.clone(), t.danger),
        None => (format!("Recording shortcut {}: press the keys you want", rec.slot + 1), t.text_dim),
    };
    let mw = pg.g.measure(&msg, t.font_small());
    pg.g.text(&msg, Rect::new(fr.r() - 12.0 - mw, fr.y, mw + 2.0, fr.h), t.font_small(), col);
    let mut y = fr.b() + 8.0;
    // conflict
    if let (Some(c), Some(owner)) = (rec.pending, rec.conflict) {
        let o = commands::by_id(owner);
        let name = o.as_ref().map(|o| &*o.title).unwrap_or("another command");
        let cat = o.as_ref().map(|o| keymap::category(&o.name)).unwrap_or("");
        let only = km.chords(owner).len() <= 1;
        let tail = if only { format!("so {name} will have no shortcut.") } else { format!("so {name} loses this shortcut.") };
        let text = format!("{} is already used by {name} ({cat}). If you assign it here it is removed from there, {tail}", chord_text(c));
        let lines = wrap(pg.g, &text, t.font_small(), fr.w - 52.0);
        let h = (20.0 + lines.len() as f32 * 16.0).max(46.0);
        let br = Rect::new(fr.x, y, fr.w, h);
        pg.g.round(br, t.radius_md, t.field);
        pg.g.round_stroke(br, t.radius_md, t.danger, 1.0);
        pg.g.text("\u{E7BA}", Rect::new(br.x + 12.0, br.y + 8.0, 20.0, 18.0), f(14.0).icon().c(), t.danger);
        for (i, l) in lines.iter().enumerate() {
            pg.g.text(l, Rect::new(br.x + 40.0, br.y + 9.0 + i as f32 * 16.0, br.w - 52.0, 16.0), t.font_small(), t.text);
        }
        y += h + 8.0;
    }
    // buttons and key hints
    let mut bx = fr.x;
    if rec.conflict.is_some() {
        let (c1, r1) = button(pg.ui, pg.g, ID_BTN.with(1), bx, y, "Assign here, unbind there", Look::Primary, true, false);
        if c1 {
            out = accept(rec, km);
            if out == RecOut::Stay {
                out = RecOut::Cancel;
            }
        }
        bx = r1.r() + 8.0;
        let (c2, r2) = button(pg.ui, pg.g, ID_BTN.with(2), bx, y, "Pick another", Look::Normal, true, false);
        if c2 {
            rec.pending = None;
            rec.conflict = None;
            pg.ui.set_focus(Some(ID_REC));
        }
        bx = r2.r() + 8.0;
    }
    let (c3, r3) = button(pg.ui, pg.g, ID_BTN.with(3), bx, y, "Cancel", Look::Normal, true, false);
    if c3 {
        out = RecOut::Cancel;
    }
    bx = r3.r() + 8.0;
    // slot chips
    for s in 0..2usize {
        let label = format!("Shortcut {}", s + 1);
        let (c4, r4) = button(pg.ui, pg.g, ID_BTN.with(10 + s as u64), bx, y, &label, if rec.slot == s { Look::Primary } else { Look::Normal }, true, false);
        if c4 && rec.slot != s {
            rec.slot = s;
            rec.pending = None;
            rec.conflict = None;
            pg.ui.set_focus(Some(ID_REC));
        }
        bx = r4.r() + 6.0;
    }
    let hint = "Esc cancel   Backspace clear this shortcut   Enter accept";
    let hw = pg.g.measure(hint, t.font_small());
    if bx + hw + 24.0 < fr.r() {
        pg.g.text(hint, Rect::new(fr.r() - hw - 2.0, y, hw + 4.0, t.height_button), t.font_small(), t.text_dim);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::*;

    const CT: Mods = Mods::CTRL;
    const CS: Mods = Mods::CTRL_SHIFT;

    #[test]
    fn a_free_chord_binds_at_once() {
        let mut km = KeyMap::defaults();
        let mut rec = Rec::new(CMD_NEW_TAB, 1);
        assert_eq!(on_key(&mut rec, &mut km, Key::Char('N'), CT), RecOut::Changed);
        assert_eq!(km.chords(CMD_NEW_TAB).len(), 2);
        assert_eq!(km.label(CMD_NEW_TAB), "Ctrl+T");
        assert_eq!(chord_text(km.chords(CMD_NEW_TAB)[1]), "Ctrl+N");
    }

    #[test]
    fn modifiers_alone_wait_and_plain_letters_are_refused() {
        let mut km = KeyMap::defaults();
        let mut rec = Rec::new(CMD_NEW_TAB, 0);
        assert_eq!(on_key(&mut rec, &mut km, Key::Other(0x11), CT), RecOut::Stay);
        assert!(rec.pending.is_none());
        assert_eq!(on_key(&mut rec, &mut km, Key::Char('X'), Mods::NONE), RecOut::Stay);
        assert!(rec.msg.as_deref().unwrap().contains("Ctrl"));
        assert_eq!(km, KeyMap::defaults());
    }

    #[test]
    fn a_taken_chord_asks_first_and_then_moves() {
        let mut km = KeyMap::defaults();
        // Ctrl+Shift+Z is "striped rows"; give it to the density command
        let mut rec = Rec::new(CMD_DENSITY, 0);
        assert_eq!(on_key(&mut rec, &mut km, Key::Char('Z'), CS), RecOut::Stay);
        assert_eq!(rec.conflict, Some(CMD_ZEBRA));
        assert_eq!(km, KeyMap::defaults(), "nothing changes before the answer");
        assert_eq!(accept(&mut rec, &mut km), RecOut::Changed);
        assert_eq!(km.label(CMD_DENSITY), "Ctrl+Shift+Z");
        assert!(km.chords(CMD_ZEBRA).is_empty(), "taken from the other command");
    }

    #[test]
    fn esc_cancels_and_backspace_clears() {
        let mut km = KeyMap::defaults();
        let mut rec = Rec::new(CMD_CLOSE_TAB, 0);
        assert_eq!(on_key(&mut rec, &mut km, Key::Escape, Mods::NONE), RecOut::Cancel);
        assert_eq!(km, KeyMap::defaults());
        assert_eq!(on_key(&mut rec, &mut km, Key::Backspace, Mods::NONE), RecOut::Changed);
        assert_eq!(km.chords(CMD_CLOSE_TAB).len(), 1, "the second default chord stays");
    }

    #[test]
    fn enter_accepts_a_waiting_conflict() {
        let mut km = KeyMap::defaults();
        let mut rec = Rec::new(CMD_DENSITY, 0);
        on_key(&mut rec, &mut km, Key::Char('Z'), CS);
        assert_eq!(on_key(&mut rec, &mut km, Key::Enter, Mods::NONE), RecOut::Changed);
        assert_eq!(km.label(CMD_DENSITY), "Ctrl+Shift+Z");
    }

    #[test]
    fn rows_are_found_by_name_or_chord() {
        let km = KeyMap::defaults();
        let i = km.commands().iter().position(|c| c.id == CMD_NEW_TAB).unwrap();
        assert!(matches("new tab", &km, i));
        assert!(matches("ctrl+t", &km, i));
        assert!(!matches("zebra", &km, i));
        assert!(matches("", &km, i));
    }
}
