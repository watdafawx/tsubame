//! Small settings controls (U4): toggle switch, checkbox, segmented control,
//! slider, key cap. Each is one call that draws, handles the pointer and the
//! keyboard, and returns what changed; the app owns the value.

use crate::canvas::{Canvas, f};
use crate::geom::Rect;
use crate::input::{Button, Cursor, Event, Key};
use crate::ui::{Id, Sense, Ui};
use crate::widgets::button::button_behaviour;

fn focus_ring(ui: &Ui, c: &mut dyn Canvas, id: Id, r: Rect, radius: f32) {
    if ui.has_focus(id) && ui.window_focused() {
        c.round_stroke(r.pad(-1.0), radius, ui.theme.focus, 1.0);
    }
}

/// The on/off switch (34 x 18 in the mock). Returns true when it was flipped
/// (the caller flips its value). `rect` is the whole control.
pub fn toggle(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, on: bool) -> bool {
    let clicked = button_behaviour(ui, id, rect);
    let t = ui.theme;
    let hot = ui.hot() == Some(id);
    let track = if on { t.accent } else if hot { t.scrollbar_hover } else { t.scrollbar };
    c.round(rect, rect.h / 2.0, track);
    let knob = rect.h - 6.0;
    let kx = if on { rect.r() - 3.0 - knob } else { rect.x + 3.0 };
    c.circle(kx + knob / 2.0, rect.y + rect.h / 2.0, knob / 2.0, if on { t.on_accent } else { t.text });
    focus_ring(ui, c, id, rect.pad(-1.0), rect.h / 2.0);
    clicked
}

/// A 16 px check box. Returns true when flipped.
pub fn checkbox(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, on: bool) -> bool {
    let clicked = button_behaviour(ui, id, rect);
    let t = ui.theme;
    if on {
        c.round(rect, t.radius_sm, t.accent);
        c.text("\u{E73E}", rect, f(rect.h - 6.0).icon().c(), t.on_accent);
    } else {
        c.round(rect, t.radius_sm, t.field);
        c.round_stroke(rect, t.radius_sm, if ui.hot() == Some(id) { t.scrollbar_hover } else { t.popup_border }, 1.0);
    }
    focus_ring(ui, c, id, rect.pad(-1.0), t.radius_sm);
    clicked
}

/// A row of connected buttons, one selected (Compact / Normal / Comfortable).
/// Each part is `width_of(label)` + padding wide, laid from `rect.x`; the
/// total width is returned with the click. `Left`/`Right` move the choice
/// while the control has the focus.
pub fn segmented(ui: &mut Ui, c: &mut dyn Canvas, id: Id, origin: (f32, f32), height: f32, labels: &[&str], cur: usize) -> (f32, Option<usize>) {
    let t = ui.theme;
    let font = t.font();
    let widths: Vec<f32> = labels.iter().map(|l| c.measure(l, font) + 24.0).collect();
    let total: f32 = widths.iter().sum();
    let whole = Rect::new(origin.0, origin.1, total, height);
    c.round(whole, t.radius_md, t.field);
    let mut pick = None;
    let mut x = whole.x;
    for (i, (l, w)) in labels.iter().zip(&widths).enumerate() {
        let r = Rect::new(x, whole.y, *w, height);
        let it = ui.interact(id.with(i as u64), r, Sense::click().focusable().cursor(Cursor::Hand));
        if i == cur {
            c.round(r.pad(1.0), t.radius_sm + 1.0, t.selected);
        } else if it.hovered {
            c.round(r.pad(1.0), t.radius_sm + 1.0, t.hover);
        }
        c.text(l, r, font.c(), if i == cur { t.text } else { t.text_dim });
        if it.clicked == Some(Button::Left) {
            pick = Some(i);
        }
        let me = id.with(i as u64);
        for (qi, ev) in ui.key_input(me) {
            if let Event::KeyDown { key, mods, .. } = ev
                && mods.none()
            {
                match key {
                    Key::Left if i > 0 => {
                        pick = Some(i - 1);
                        ui.consume(qi);
                    }
                    Key::Right if i + 1 < labels.len() => {
                        pick = Some(i + 1);
                        ui.consume(qi);
                    }
                    Key::Enter | Key::Space => {
                        pick = Some(i);
                        ui.consume(qi);
                    }
                    _ => {}
                }
            }
        }
        if ui.has_focus(me) && ui.window_focused() {
            c.round_stroke(r.pad(1.0), t.radius_sm + 1.0, t.focus, 1.0);
        }
        x += w;
    }
    c.round_stroke(whole, t.radius_md, t.border, 1.0);
    if pick.is_some() {
        ui.request_repaint();
    }
    (total, pick.filter(|&p| p != cur))
}

/// A horizontal slider on `rect` for a value in `0..=1`. Returns the new
/// value while the pointer drags it (or an arrow key moves it by `step`).
pub fn slider(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, value: f32, step: f32) -> Option<f32> {
    let t = ui.theme;
    let hit = Rect::new(rect.x - 6.0, rect.y, rect.w + 12.0, rect.h);
    let it = ui.interact(id, hit, Sense::click().focusable().cursor(Cursor::Hand));
    let mut out = None;
    let value = value.clamp(0.0, 1.0);
    let at = |x: f32| ((x - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
    if it.pressed.is_some() || it.active {
        let v = at(ui.mouse().x);
        if (v - value).abs() > 1e-4 {
            out = Some(v);
        }
    }
    for (qi, ev) in ui.key_input(id) {
        if let Event::KeyDown { key, mods, .. } = ev
            && mods.none()
        {
            let d = match key {
                Key::Left | Key::Down => -step,
                Key::Right | Key::Up => step,
                _ => continue,
            };
            ui.consume(qi);
            out = Some((value + d).clamp(0.0, 1.0));
        }
    }
    let shown = out.unwrap_or(value);
    let ty = rect.y + rect.h / 2.0;
    c.round(Rect::new(rect.x, ty - 1.5, rect.w, 3.0), 1.5, t.scrollbar);
    c.round(Rect::new(rect.x, ty - 1.5, rect.w * shown, 3.0), 1.5, t.accent);
    let kx = rect.x + rect.w * shown;
    c.circle(kx, ty, if it.active || it.hovered { 7.0 } else { 6.0 }, t.accent);
    c.circle(kx, ty, 3.0, t.on_accent);
    focus_ring(ui, c, id, hit, t.radius_sm);
    if out.is_some() {
        ui.request_repaint();
    }
    out
}

/// A key cap ("Ctrl") drawn with its left edge at `x`, vertically centred on
/// `cy`. Returns the right edge.
pub fn keycap(c: &mut dyn Canvas, t: &crate::theme::Theme, x: f32, cy: f32, label: &str) -> f32 {
    let font = t.font_mono();
    let w = (c.measure(label, font) + 10.0).max(t.height_keycap);
    let r = Rect::new(x, cy - t.height_keycap / 2.0, w, t.height_keycap);
    c.round(r, t.radius_sm, t.field);
    c.round_stroke(r, t.radius_sm, t.popup_border, 1.0);
    c.text(label, r, font.c(), t.text);
    r.r()
}

/// Draws a chord label as a run of key caps ("Ctrl", "Shift", "T"), right
/// edge at `right`. Returns the left edge.
pub fn keycaps_right(c: &mut dyn Canvas, t: &crate::theme::Theme, right: f32, cy: f32, label: &str) -> f32 {
    let font = t.font_mono();
    let parts: Vec<&str> = split_chord(label);
    let widths: Vec<f32> = parts.iter().map(|p| (c.measure(p, font) + 10.0).max(t.height_keycap)).collect();
    let total: f32 = widths.iter().sum::<f32>() + 3.0 * parts.len().saturating_sub(1) as f32;
    let mut x = right - total;
    let left = x;
    for p in &parts {
        x = keycap(c, t, x, cy, p) + 3.0;
    }
    left
}

/// "Ctrl+Shift+T" -> ["Ctrl", "Shift", "T"]; a trailing "+" key stays ("Ctrl++").
pub fn split_chord(label: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let b = label.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'+' && i > start {
            parts.push(&label[start..i]);
            start = i + 1;
        }
    }
    if start < label.len() {
        parts.push(&label[start..]);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testcanvas::TestCanvas;
    use crate::{Pt, Ui};

    const W: f32 = 400.0;
    const H: f32 = 200.0;
    const TG: Id = Id::new("tg");
    const SL: Id = Id::new("sl");
    const SG: Id = Id::new("sg");

    fn frame(ui: &mut Ui, c: &mut TestCanvas, f: impl FnOnce(&mut Ui, &mut TestCanvas)) {
        c.clear();
        c.begin(0);
        ui.begin_frame((W, H));
        f(ui, c);
        ui.end_frame();
        c.end();
    }
    fn click(ui: &mut Ui, x: f32, y: f32) {
        ui.push(crate::Event::MouseMove { pos: Pt::new(x, y) });
        ui.push(crate::Event::MouseDown { pos: Pt::new(x, y), button: Button::Left, mods: crate::Mods::NONE, double: false });
        ui.push(crate::Event::MouseUp { pos: Pt::new(x, y), button: Button::Left, mods: crate::Mods::NONE });
    }

    #[test]
    fn toggle_flips_on_click_and_on_space() {
        let mut ui = Ui::default();
        let mut c = TestCanvas::new(W, H);
        let r = Rect::new(10.0, 10.0, 34.0, 18.0);
        frame(&mut ui, &mut c, |ui, c| {
            toggle(ui, c, TG, r, false);
        });
        click(&mut ui, 20.0, 18.0);
        let mut flipped = false;
        frame(&mut ui, &mut c, |ui, c| flipped = toggle(ui, c, TG, r, false));
        assert!(flipped);
        // focused now: Space flips again
        ui.push(crate::Event::KeyDown { key: Key::Space, mods: crate::Mods::NONE, repeat: false });
        frame(&mut ui, &mut c, |ui, c| flipped = toggle(ui, c, TG, r, true));
        assert!(flipped);
        // on: the knob is on the right
        frame(&mut ui, &mut c, |ui, c| {
            toggle(ui, c, TG, r, true);
        });
        let knob = c.ops.iter().find_map(|o| if let crate::testcanvas::Op::Circle(x, ..) = o { Some(*x) } else { None }).unwrap();
        assert!(knob > r.x + r.w / 2.0);
    }

    #[test]
    fn segmented_picks_the_clicked_part_not_the_current() {
        let mut ui = Ui::default();
        let mut c = TestCanvas::new(W, H);
        let labels = ["Compact", "Normal", "Comfortable"];
        let mut total = 0.0;
        frame(&mut ui, &mut c, |ui, c| total = segmented(ui, c, SG, (0.0, 0.0), 24.0, &labels, 0).0);
        assert!(total > 100.0);
        let w0 = TestCanvas::width_of("Compact", ui.theme.font()) + 24.0;
        click(&mut ui, w0 + 10.0, 12.0);
        let mut pick = None;
        frame(&mut ui, &mut c, |ui, c| pick = segmented(ui, c, SG, (0.0, 0.0), 24.0, &labels, 0).1);
        assert_eq!(pick, Some(1));
        click(&mut ui, w0 + 10.0, 12.0);
        frame(&mut ui, &mut c, |ui, c| pick = segmented(ui, c, SG, (0.0, 0.0), 24.0, &labels, 1).1);
        assert_eq!(pick, None, "clicking the current part changes nothing");
    }

    #[test]
    fn slider_follows_the_pointer_and_arrows() {
        let mut ui = Ui::default();
        let mut c = TestCanvas::new(W, H);
        let r = Rect::new(20.0, 10.0, 200.0, 16.0);
        frame(&mut ui, &mut c, |ui, c| {
            slider(ui, c, SL, r, 0.0, 0.1);
        });
        ui.push(crate::Event::MouseMove { pos: Pt::new(120.0, 18.0) });
        ui.push(crate::Event::MouseDown { pos: Pt::new(120.0, 18.0), button: Button::Left, mods: crate::Mods::NONE, double: false });
        let mut v = None;
        frame(&mut ui, &mut c, |ui, c| v = slider(ui, c, SL, r, 0.0, 0.1));
        assert!((v.unwrap() - 0.5).abs() < 0.01, "{v:?}");
        ui.push(crate::Event::MouseUp { pos: Pt::new(120.0, 18.0), button: Button::Left, mods: crate::Mods::NONE });
        frame(&mut ui, &mut c, |ui, c| {
            slider(ui, c, SL, r, 0.5, 0.1);
        });
        ui.push(crate::Event::KeyDown { key: Key::Right, mods: crate::Mods::NONE, repeat: false });
        frame(&mut ui, &mut c, |ui, c| v = slider(ui, c, SL, r, 0.5, 0.1));
        assert!((v.unwrap() - 0.6).abs() < 1e-4, "{v:?}");
    }

    #[test]
    fn chords_split_into_caps() {
        assert_eq!(split_chord("Ctrl+Shift+T"), vec!["Ctrl", "Shift", "T"]);
        assert_eq!(split_chord("F2"), vec!["F2"]);
        assert_eq!(split_chord("Ctrl++"), vec!["Ctrl", "+"]);
        assert!(split_chord("").is_empty());
    }
}
