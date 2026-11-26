//! Tab strip (title bar tabs, per the mock): select, close (x box or middle
//! click), add (+), drag to reorder, and overflow (tabs shrink to
//! `layout_tab_min_width`, then the strip scrolls with the wheel and keeps
//! the active tab in view). The app owns the tabs; the strip reports what the
//! user asked for.
//!
//! `rect` is the title-bar band the strip may use. Tabs sit on its bottom
//! edge (`height_tab` high) and the "+" button follows the last tab, so the
//! empty rest of the band has no widget and stays free to drag the window.
//! Tabs share one width, so no label is measured, and only tabs inside the
//! strip are drawn.

use crate::canvas::Canvas;
use crate::geom::{Pt, Rect};
use crate::input::Button;
use crate::ui::{Id, Sense, Ui};
use crate::widgets::button::button_behaviour;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabStrip {
    /// overflow scroll, DIPs
    pub scroll: f32,
    /// the active tab the strip last scrolled into view
    last_active: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TabResponse {
    pub select: Option<usize>,
    pub close: Option<usize>,
    pub add: bool,
    /// drag-reorder: move tab `.0` to index `.1` (the drag goes on with the
    /// tab at its new index)
    pub moved: Option<(usize, usize)>,
    /// right click released on tab `.0` at this point: the app opens its tab menu
    pub context: Option<(usize, Pt)>,
}

/// "+" button size and the gap before it.
const ADD_SIZE: f32 = 28.0;
const ADD_GAP: f32 = 4.0;
/// gap between two tabs
const TAB_GAP: f32 = 2.0;
const CLOSE_W: f32 = 18.0;

/// Tab width (including the gap) for `n` tabs in `avail` DIPs.
pub fn tab_width(avail: f32, n: usize, min: f32, max: f32) -> f32 {
    if n == 0 {
        return max;
    }
    (avail / n as f32).clamp(min, max)
}

pub fn tab_strip(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, st: &mut TabStrip, labels: &[&str], active: usize) -> TabResponse {
    let mut resp = TabResponse::default();
    let n = labels.len();
    let (min_w, max_w) = (ui.theme.layout_tab_min_width, ui.theme.layout_tab_width);
    let avail = (rect.w - ADD_SIZE - ADD_GAP).max(0.0);
    let w = tab_width(avail, n, min_w, max_w);
    let total = w * n as f32;
    let tab_h = ui.theme.height_tab;
    let area = Rect::new(rect.x, rect.b() - tab_h, total.min(avail), tab_h);
    let max_scroll = (total - area.w).max(0.0);
    // keep the active tab in view when it changes
    if st.last_active != Some(active) && n > 0 {
        let (l, r) = (active as f32 * w, (active + 1) as f32 * w);
        if l < st.scroll {
            st.scroll = l;
        } else if r > st.scroll + area.w {
            st.scroll = r - area.w;
        }
        st.last_active = Some(active);
    }
    st.scroll = st.scroll.clamp(0.0, max_scroll);
    let x0 = area.x - st.scroll;
    let tab_at = |x: f32| -> Option<usize> {
        let i = ((x - x0) / w).floor();
        (i >= 0.0 && (i as usize) < n).then_some(i as usize)
    };
    let close_box = |i: usize| Rect::new(x0 + (i + 1) as f32 * w - TAB_GAP - 4.0 - CLOSE_W, area.y + (area.h - CLOSE_W) / 2.0, CLOSE_W, CLOSE_W);

    let it = ui.interact(id, area, Sense::click().wheel().cells_x(x0, w));
    if it.wheel.1 != 0.0 || it.wheel.0 != 0.0 {
        st.scroll = (st.scroll - (it.wheel.1 + it.wheel.0) * w * 0.5).clamp(0.0, max_scroll);
    }
    if let Some(p) = it.pressed
        && let Some(i) = tab_at(p.pos.x)
    {
        match p.button {
            Button::Left if !close_box(i).contains_pt(p.pos) => {
                resp.select = Some(i);
                ui.drag_f64 = i as f64;
            }
            Button::Left => ui.drag_f64 = f64::NAN, // press on the close box
            _ => ui.drag_f64 = f64::NAN,
        }
    }
    if it.clicked == Some(Button::Middle)
        && let Some(i) = tab_at(it.click_pos.x)
    {
        resp.close = Some(i);
    }
    if it.clicked == Some(Button::Left)
        && let Some(i) = tab_at(it.click_pos.x)
        && close_box(i).contains_pt(it.click_pos)
    {
        resp.close = Some(i);
    }
    if it.clicked == Some(Button::Right)
        && let Some(i) = tab_at(it.click_pos.x)
    {
        resp.context = Some((i, it.click_pos));
    }
    // drag to reorder: past the middle of a neighbour swaps with it
    if it.active && !ui.drag_f64.is_nan() && n > 1 {
        let from = ui.drag_f64 as usize;
        let to = (((ui.mouse().x - x0) / w).floor().max(0.0) as usize).min(n - 1);
        if to != from && from < n {
            resp.moved = Some((from, to));
            ui.drag_f64 = to as f64;
        }
    }
    if resp != TabResponse::default() {
        ui.request_repaint();
    }

    // draw visible tabs only
    let t = ui.theme;
    c.clip(area);
    let hot_tab = if it.hovered { tab_at(ui.mouse().x) } else { None };
    let first = (st.scroll / w).floor() as usize;
    let last = (((st.scroll + area.w) / w).ceil() as usize).min(n);
    for (i, label) in labels.iter().enumerate().take(last).skip(first) {
        let r = Rect::new(x0 + i as f32 * w, area.y, w - TAB_GAP, area.h);
        let bg = if i == active {
            t.panel
        } else if hot_tab == Some(i) {
            t.hover
        } else {
            t.bg
        };
        if bg != t.bg {
            // rounded top corners only: draw a taller rounded rect, clipped
            c.clip(r);
            c.round(Rect::new(r.x, r.y, r.w, r.h + t.radius_lg), t.radius_lg, bg);
            c.unclip();
        }
        let show_close = i == active || hot_tab == Some(i);
        let text_r = Rect::new(r.x + 8.0, r.y, (r.w - 8.0 - if show_close { CLOSE_W + 4.0 + 6.0 } else { 8.0 }).max(0.0), r.h);
        c.text(label, text_r, t.font(), if i == active || hot_tab == Some(i) { t.text } else { t.text_dim });
        if show_close {
            let cb = close_box(i);
            if hot_tab == Some(i) && cb.contains_pt(ui.mouse()) {
                c.round(cb, t.radius_sm, if i == active { t.selected_inactive } else { t.hover });
            }
            c.text("\u{E711}", cb, crate::f(t.icon_window_control).icon().c(), if hot_tab == Some(i) && cb.contains_pt(ui.mouse()) { t.text } else { t.text_dim });
        }
    }
    c.unclip();

    // add button: right after the last tab (pinned at the end when tabs overflow)
    let add_r = Rect::new(area.x + area.w + ADD_GAP, rect.y + (rect.h - ADD_SIZE) / 2.0, ADD_SIZE, ADD_SIZE);
    if button_behaviour(ui, id.child("add"), add_r) {
        resp.add = true;
    }
    if ui.hot() == Some(id.child("add")) {
        c.round(add_r, t.radius_md, t.hover);
    }
    c.text("\u{E710}", add_r, crate::f(t.icon_toolbar - 2.0).icon().c(), if ui.hot() == Some(id.child("add")) { t.text } else { t.text_dim });
    resp
}
