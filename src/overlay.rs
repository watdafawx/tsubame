//! Things drawn on top of everything: tooltips, the context menu list, the
//! chip that follows the pointer while files are dragged, and the key caps
//! ("Enter", "Esc") the mock shows in fields and the status bar.
//! The caller draws them last, so nothing paints over them.

use tsubame_ui::{Button, Canvas, Cursor, Id, Pt, Rect, Sense, Theme, Ui, f};

const PAD: f32 = 8.0;

/// A popup of text lines: a bold first line, then dim ones. `at` is the
/// pointer; the box sits below-right of it and stays inside the window.
pub fn tooltip(c: &mut dyn Canvas, t: &Theme, win: (f32, f32), at: Pt, title: &str, lines: &[&str]) {
    let tf = t.font_small().b();
    let lf = t.font_small();
    let mut w = c.measure(title, tf);
    for l in lines {
        w = w.max(c.measure(l, lf));
    }
    let max_text_w = (win.0 - 8.0 - 2.0 * PAD).clamp(60.0, 360.0);
    let w = (w.ceil() + 2.0).min(max_text_w) + 2.0 * PAD;
    let h = 8.0 + 16.0 + lines.len() as f32 * 15.0;
    let x = (at.x + 14.0).min(win.0 - w - 4.0).max(4.0);
    let y = (at.y + 18.0).min(win.1 - h - 4.0).max(4.0);
    let r = Rect::new(x, y, w, h);
    c.round(r, t.radius_md, t.popup);
    c.round_stroke(r, t.radius_md, t.popup_border, 1.0);
    c.text(title, Rect::new(r.x + PAD, r.y + 4.0, w - 2.0 * PAD, 16.0), tf, t.text);
    for (i, l) in lines.iter().enumerate() {
        c.text(l, Rect::new(r.x + PAD, r.y + 20.0 + i as f32 * 15.0, w - 2.0 * PAD, 15.0), lf, t.text_dim);
    }
}

/// A one-line label beside an icon rail item.
pub fn tip_right_of(c: &mut dyn Canvas, t: &Theme, win: (f32, f32), anchor: Rect, text: &str) {
    let font = t.font();
    let w = c.measure(text, font) + 2.0 * PAD;
    let r = Rect::new(anchor.r() + 6.0, anchor.y + (anchor.h - 24.0) / 2.0, w, 24.0);
    let r = Rect::new(r.x.min(win.0 - w - 2.0), r.y.clamp(2.0, (win.1 - 26.0).max(2.0)), w, 24.0);
    c.round(r, t.radius_md, t.popup);
    c.round_stroke(r, t.radius_md, t.popup_border, 1.0);
    c.text(text, Rect::new(r.x + PAD, r.y, w - PAD, r.h), font, t.text);
}

/// The key cap: "Enter", "F5", "Ctrl+B". Returns its width.
pub fn keycap(c: &mut dyn Canvas, t: &Theme, x: f32, cy: f32, label: &str) -> f32 {
    let font = t.font_mono();
    let w = c.measure(label, font) + 10.0;
    let r = Rect::new(x, cy - 8.0, w, 16.0);
    c.round_stroke(r, t.radius_sm, t.border, 1.0);
    c.text(label, r, font.c(), t.text_dim);
    w
}

/// Like `keycap` but right-aligned to `right`.
pub fn keycap_right(c: &mut dyn Canvas, t: &Theme, right: f32, cy: f32, label: &str) -> f32 {
    let w = c.measure(label, t.font_mono()) + 10.0;
    keycap(c, t, right - w, cy, label)
}

/// What follows the pointer during a drag: folder or file icon, the first
/// name, and a count badge when several items are dragged.
pub fn drag_chip(c: &mut dyn Canvas, t: &Theme, win: (f32, f32), at: Pt, name: &str, count: usize, folder: bool) {
    let font = t.font();
    let badge = if count > 1 { format!("{count}") } else { String::new() };
    let nw = c.measure(name, font).min(220.0);
    let bw = if badge.is_empty() { 0.0 } else { c.measure(&badge, t.font_small().b()) + 12.0 };
    let w = 8.0 + 18.0 + 6.0 + nw + if bw > 0.0 { 8.0 + bw } else { 0.0 } + 8.0;
    let r = Rect::new((at.x + 12.0).min(win.0 - w - 2.0), (at.y + 12.0).min(win.1 - 28.0), w, 26.0);
    c.round_a(r, t.radius_md, t.popup, 0.96);
    c.round_stroke(r, t.radius_md, t.accent, 1.0);
    let (glyph, col) = if folder { ("\u{E8B7}", t.folder) } else { ("\u{E7C3}", t.text_dim) };
    c.text(glyph, Rect::new(r.x + 8.0, r.y, 18.0, r.h), f(14.0).icon().c(), col);
    c.text(name, Rect::new(r.x + 32.0, r.y, nw, r.h), font, t.text);
    if bw > 0.0 {
        let b = Rect::new(r.r() - 8.0 - bw, r.y + 4.0, bw, 18.0);
        c.round(b, 9.0, t.accent);
        c.text(&badge, b, t.font_small().b().c(), t.on_accent);
    }
}

pub struct MenuItem<'a> {
    pub label: &'a str,
    /// key hint shown at the right ("" for none)
    pub keys: &'a str,
    pub enabled: bool,
    /// draw a separator line above
    pub sep_before: bool,
    pub danger: bool,
    /// a checkbox row: `Some(on)` draws a check mark slot before the label
    pub checked: Option<bool>,
}

impl<'a> MenuItem<'a> {
    pub fn new(label: &'a str, keys: &'a str) -> MenuItem<'a> {
        MenuItem { label, keys, enabled: true, sep_before: false, danger: false, checked: None }
    }
    pub fn check(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }
    pub fn off(mut self, off: bool) -> Self {
        self.enabled = !off;
        self
    }
    pub fn sep(mut self) -> Self {
        self.sep_before = true;
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}

pub const MENU_W: f32 = 248.0;
const ITEM_H: f32 = 26.0;
const SEP_H: f32 = 7.0;

pub fn menu_height(items: &[MenuItem]) -> f32 {
    8.0 + items.iter().map(|i| ITEM_H + if i.sep_before { SEP_H } else { 0.0 }).sum::<f32>()
}

/// Where a menu opened at `at` goes inside a window of size `win`.
pub fn menu_rect(at: Pt, win: (f32, f32), h: f32) -> Rect {
    let x = at.x.min(win.0 - MENU_W - 2.0).max(2.0);
    let y = if at.y + h > win.1 - 2.0 { (at.y - h).max(2.0) } else { at.y };
    Rect::new(x, y, MENU_W, h)
}

/// The menu popup `id` (open it with `ui.open_popup`). Returns the index of
/// the item clicked; the popup is closed then.
pub fn menu(ui: &mut Ui, c: &mut dyn Canvas, id: Id, at: Pt, items: &[MenuItem]) -> Option<usize> {
    if !ui.is_popup_open(id) {
        return None;
    }
    let win = ui.size();
    let t = ui.theme;
    let rect = menu_rect(at, win, menu_height(items));
    if !ui.popup_begin(c, id, rect) {
        return None;
    }
    let mut y = rect.y + 4.0;
    let mut picked = None;
    for (i, item) in items.iter().enumerate() {
        if item.sep_before {
            c.fill(Rect::new(rect.x + 8.0, y + 3.0, rect.w - 16.0, 1.0), t.border);
            y += SEP_H;
        }
        let r = Rect::new(rect.x + 4.0, y, rect.w - 8.0, ITEM_H);
        let it = ui.interact(id.child("item").with(i as u64), r, Sense::click().cursor(if item.enabled { Cursor::Hand } else { Cursor::Arrow }));
        if item.enabled && it.hovered {
            c.round(r, t.radius_sm, t.hover);
        }
        let col = if !item.enabled {
            t.text_disabled
        } else if item.danger {
            t.danger
        } else {
            t.text
        };
        let lead = if item.checked.is_some() { 26.0 } else { 0.0 };
        if item.checked == Some(true) {
            c.text("\u{E73E}", Rect::new(r.x + 8.0, r.y, 16.0, r.h), f(12.0).icon().c(), if item.enabled { t.accent } else { t.text_disabled });
        }
        c.text(item.label, Rect::new(r.x + 10.0 + lead, r.y, r.w - 20.0 - lead, r.h), t.font(), col);
        if !item.keys.is_empty() {
            keycap_right(c, &t, r.r() - 6.0, r.y + r.h / 2.0, item.keys);
        }
        if item.enabled && it.clicked == Some(Button::Left) {
            picked = Some(i);
        }
        y += ITEM_H;
    }
    ui.popup_end(c);
    if picked.is_some() {
        ui.close_popup(id);
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_stays_inside_the_window() {
        let items = [MenuItem::new("a", "F2"), MenuItem::new("b", "").sep(), MenuItem::new("c", "Del").danger()];
        let h = menu_height(&items);
        assert_eq!(h, 8.0 + 3.0 * 26.0 + 7.0);
        let r = menu_rect(Pt::new(900.0, 500.0), (1000.0, 600.0), h);
        assert!(r.r() <= 1000.0 && r.x >= 2.0);
        assert!(r.b() <= 600.0, "flips above the pointer near the bottom edge");
        let r2 = menu_rect(Pt::new(10.0, 10.0), (1000.0, 600.0), h);
        assert_eq!((r2.x, r2.y), (10.0, 10.0));
        assert!(!items[0].danger && items[2].danger && items[2].enabled);
        assert!(!MenuItem::new("x", "").off(true).enabled);
    }
}
