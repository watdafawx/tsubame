//! Layout helpers of the Settings pages: a cursor that walks down the page, cards of
//! fixed-height rows, and the few controls every page shares (buttons, toggle rows,
//! segmented rows, notes). Immediate mode: each call draws and handles its input.

use tsubame_ui::{Canvas, Id, Rect, Sense, Theme, Ui, f, segmented, toggle};

/// How a button looks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    Normal,
    Primary,
    /// red outline (Reset all)
    Danger,
    /// red fill (the second step of a destructive confirm)
    DangerFill,
}

/// Cuts `s` so it fits `w` DIPs, with an ellipsis.
pub fn fit(g: &mut dyn Canvas, s: &str, font: tsubame_ui::Font, w: f32) -> String {
    if g.measure(s, font) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        out.push(ch);
        if g.measure(&out, font) > w - 12.0 {
            out.pop();
            break;
        }
    }
    out.push('\u{2026}');
    out
}

/// A button of `label`'s width (+ 28) with its left edge at `x`, or right edge when
/// `right` is true. Returns (clicked, rect).
#[allow(clippy::too_many_arguments)]
pub fn button(ui: &mut Ui, g: &mut dyn Canvas, id: Id, x: f32, y: f32, label: &str, look: Look, enabled: bool, right: bool) -> (bool, Rect) {
    let t = ui.theme;
    let w = g.measure(label, t.font().b()) + 28.0;
    let r = Rect::new(if right { x - w } else { x }, y, w, t.height_button);
    (button_in(ui, g, id, r, label, look, enabled), r)
}

/// A button in `r`.
pub fn button_in(ui: &mut Ui, g: &mut dyn Canvas, id: Id, r: Rect, label: &str, look: Look, enabled: bool) -> bool {
    let t = ui.theme;
    let it = ui.interact(id, r, if enabled { Sense::click().focusable() } else { Sense::hover() });
    let mut clicked = enabled && it.clicked == Some(tsubame_ui::Button::Left);
    if enabled {
        for (qi, ev) in ui.key_input(id) {
            if let tsubame_ui::Event::KeyDown { key: tsubame_ui::Key::Enter | tsubame_ui::Key::Space, mods, .. } = ev
                && mods.none()
            {
                ui.consume(qi);
                clicked = true;
            }
        }
    }
    let hot = enabled && (it.hovered || ui.is_active(id));
    let (fill, stroke, text) = match look {
        Look::Primary => (if hot { crate::settings::lighten(t.accent, 0.12) } else { t.accent }, None, t.on_accent),
        Look::DangerFill => (if hot { crate::settings::lighten(t.danger_fill, 0.12) } else { t.danger_fill }, None, t.on_danger),
        Look::Danger => (if hot { t.hover } else { t.field }, Some(t.danger), t.danger),
        Look::Normal => (if hot { t.hover } else { t.field }, Some(t.popup_border), t.text),
    };
    g.round(r, t.radius_md, fill);
    if let Some(s) = stroke {
        g.round_stroke(r, t.radius_md, s, 1.0);
    }
    if enabled && ui.has_focus(id) && ui.window_focused() {
        g.round_stroke(r.pad(-1.0), t.radius_md, t.focus, 1.0);
    }
    g.text(label, r, t.font().b().c(), if enabled { text } else { t.text_disabled });
    if clicked {
        ui.request_repaint();
    }
    clicked
}

/// A card of equal rows: the rounded background is drawn first, rows go on top.
pub struct Card {
    pub r: Rect,
    i: usize,
    rh: f32,
    /// top of row 0 (moves when a card has a tall block in the middle)
    y0: f32,
    divider_first: bool,
}

impl Card {
    /// The rows after a tall block start at `y` (with a divider above the first).
    pub fn restart_at(&mut self, y: f32) {
        self.y0 = y;
        self.i = 0;
        self.divider_first = true;
    }
}

pub struct Pg<'a> {
    pub ui: &'a mut Ui,
    pub g: &'a mut dyn Canvas,
    pub t: Theme,
    pub x: f32,
    pub w: f32,
    /// the cursor: top of the next thing
    pub y: f32,
    /// the visible part of the page (rows outside are not drawn)
    pub vis: Rect,
    base: Id,
    n: u64,
}

impl<'a> Pg<'a> {
    pub fn new(ui: &'a mut Ui, g: &'a mut dyn Canvas, x: f32, y: f32, w: f32, vis: Rect, base: Id) -> Pg<'a> {
        let t = ui.theme;
        Pg { ui, g, t, x, w, y, vis, base, n: 0 }
    }

    /// A fresh widget id for this frame's layout (stable while the page's rows do not change).
    pub fn id(&mut self) -> Id {
        self.n += 1;
        self.base.with(self.n)
    }

    pub fn visible(&self, r: Rect) -> bool {
        r.b() > self.vis.y - 2.0 && r.y < self.vis.b() + 2.0
    }

    /// Page title and its one-line subtitle.
    pub fn heading(&mut self, title: &str, sub: &str) {
        let t = self.t;
        let r = Rect::new(self.x, self.y, self.w, 26.0);
        self.g.text(title, r, f(t.font_size_lg + 3.0).b(), t.text);
        self.y += 28.0;
        if !sub.is_empty() {
            let s = fit(self.g, sub, t.font(), self.w);
            self.g.text(&s, Rect::new(self.x, self.y, self.w, 18.0), t.font(), t.text_dim);
            self.y += 22.0;
        }
        self.y += 6.0;
    }

    /// Small dim label above a card.
    pub fn section(&mut self, label: &str) {
        let t = self.t;
        self.y += 8.0;
        self.g.text(label, Rect::new(self.x + 2.0, self.y, self.w, 18.0), t.font_small().b(), t.text_dim);
        self.y += 22.0;
    }

    /// A card of `rows` rows `rh` high (the background is drawn now).
    pub fn card(&mut self, rows: usize, rh: f32) -> Card {
        self.card_h(rows as f32 * rh, rh)
    }

    /// A card of height `h` made of rows `rh` high (the last row may be shorter / taller by the caller).
    pub fn card_h(&mut self, h: f32, rh: f32) -> Card {
        let t = self.t;
        let r = Rect::new(self.x, self.y, self.w, h);
        self.g.round(r, t.radius_lg, t.panel);
        self.g.round_stroke(r, t.radius_lg, t.border, 1.0);
        self.y += h + 12.0;
        Card { r, i: 0, rh, y0: r.y, divider_first: false }
    }

    /// The next row of the card, with a divider above it (except the first).
    pub fn next_row(&mut self, c: &mut Card) -> Rect {
        let r = Rect::new(c.r.x, c.y0 + c.i as f32 * c.rh, c.r.w, c.rh);
        if c.i > 0 || c.divider_first {
            self.g.fill(Rect::new(r.x + 1.0, r.y, r.w - 2.0, 1.0), self.t.border);
        }
        c.i += 1;
        r
    }

    /// Title and description of a settings row (left side); `reserve` DIPs on the right stay free.
    pub fn row_text(&mut self, row: Rect, title: &str, desc: &str, reserve: f32, enabled: bool) {
        let t = self.t;
        let w = (row.w - 32.0 - reserve).max(80.0);
        let tc = if enabled { t.text } else { t.text_disabled };
        if desc.is_empty() {
            let s = fit(self.g, title, t.font_md(), w);
            self.g.text(&s, Rect::new(row.x + 16.0, row.y, w, row.h), t.font_md(), tc);
        } else {
            let s = fit(self.g, title, t.font_md(), w);
            self.g.text(&s, Rect::new(row.x + 16.0, row.y + 9.0, w, 18.0), t.font_md(), tc);
            let d = fit(self.g, desc, t.font_small(), w);
            self.g.text(&d, Rect::new(row.x + 16.0, row.y + 28.0, w, 16.0), t.font_small(), if enabled { t.text_dim } else { t.text_disabled });
        }
    }

    /// A row with a toggle on the right. Returns the new value when flipped.
    pub fn toggle_row(&mut self, c: &mut Card, title: &str, desc: &str, on: bool, enabled: bool) -> Option<bool> {
        let row = self.next_row(c);
        self.row_text(row, title, desc, 70.0, enabled);
        let id = self.id();
        let t = self.t;
        let r = Rect::new(row.r() - 16.0 - t.layout_toggle_width, row.y + (row.h - t.layout_toggle_height) / 2.0, t.layout_toggle_width, t.layout_toggle_height);
        if !enabled {
            // greyed track, no input
            self.g.round(r, r.h / 2.0, t.scrollbar);
            let knob = r.h - 6.0;
            self.g.circle(r.x + 3.0 + knob / 2.0, r.y + r.h / 2.0, knob / 2.0, t.text_disabled);
            return None;
        }
        toggle(self.ui, self.g, id, r, on).then_some(!on)
    }

    /// A row with a segmented control on the right. Returns the picked index.
    pub fn seg_row(&mut self, c: &mut Card, title: &str, desc: &str, labels: &[&str], cur: usize) -> Option<usize> {
        let row = self.next_row(c);
        let t = self.t;
        let font = t.font();
        let total: f32 = labels.iter().map(|l| self.g.measure(l, font) + 24.0).sum();
        self.row_text(row, title, desc, total + 16.0, true);
        let id = self.id();
        segmented(self.ui, self.g, id, (row.r() - 16.0 - total, row.y + (row.h - 28.0) / 2.0), 28.0, labels, cur).1
    }

    /// A rounded note box with a leading glyph. Returns its rect.
    pub fn note(&mut self, glyph: &str, text: &str, warn: bool) -> Rect {
        let t = self.t;
        let tw = self.w - 52.0;
        let lines = wrap(self.g, text, t.font_small(), tw);
        let h = 20.0 + lines.len() as f32 * 16.0;
        let r = Rect::new(self.x, self.y, self.w, h);
        let edge = if warn { t.danger } else { t.border };
        self.g.round(r, t.radius_md, t.panel);
        self.g.round_stroke(r, t.radius_md, edge, 1.0);
        self.g.text(glyph, Rect::new(r.x + 12.0, r.y + 8.0, 20.0, 18.0), f(14.0).icon().c(), if warn { t.danger } else { t.text_dim });
        for (i, l) in lines.iter().enumerate() {
            self.g.text(l, Rect::new(r.x + 40.0, r.y + 10.0 + i as f32 * 16.0, tw, 16.0), t.font_small(), t.text_dim);
        }
        self.y += h + 12.0;
        r
    }
}

/// Greedy word wrap to `w` DIPs.
pub fn wrap(g: &mut dyn Canvas, s: &str, font: tsubame_ui::Font, w: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let try_ = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if !cur.is_empty() && g.measure(&try_, font) > w {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = try_;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}
