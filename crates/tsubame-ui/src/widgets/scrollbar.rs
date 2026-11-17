//! Vertical scrollbar: thumb drag, track click (one page towards the click).
//! Scroll positions are `f64` DIPs: 1M rows x 22 DIP is past `f32`'s
//! whole-pixel range (16.7M).

use crate::canvas::Canvas;
use crate::geom::Rect;
use crate::ui::{Id, Sense, Ui};

/// What the bar scrolls over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMetrics {
    /// total content length
    pub content: f64,
    /// visible length
    pub view: f64,
    /// track click step
    pub page: f64,
}

impl ScrollMetrics {
    pub fn max_scroll(&self) -> f64 {
        (self.content - self.view).max(0.0)
    }
}

/// Thumb (top, height) on a track at `ty` of height `th`.
pub fn thumb_geom(ty: f32, th: f32, m: &ScrollMetrics, scroll: f64, min_thumb: f32) -> (f32, f32) {
    let ratio = if m.content > 0.0 { (m.view / m.content).clamp(0.0, 1.0) } else { 1.0 };
    let h = ((th as f64 * ratio) as f32).max(min_thumb).min(th);
    let max = m.max_scroll();
    let frac = if max > 0.0 { (scroll / max).clamp(0.0, 1.0) } else { 0.0 };
    (ty + (frac * (th - h) as f64) as f32, h)
}

/// Scroll for a thumb whose top is at `top` (inverse of `thumb_geom`).
pub fn scroll_for_thumb(ty: f32, th: f32, thumb_h: f32, m: &ScrollMetrics, top: f32) -> f64 {
    let span = (th - thumb_h).max(1.0) as f64;
    let frac = ((top - ty) as f64 / span).clamp(0.0, 1.0);
    frac * m.max_scroll()
}

/// Draws the bar in `track` and handles its input. Returns true when
/// `scroll` changed.
pub fn vscrollbar(ui: &mut Ui, c: &mut dyn Canvas, id: Id, track: Rect, m: ScrollMetrics, scroll: &mut f64) -> bool {
    let before = *scroll;
    let min_thumb = ui.theme.sb_min_thumb;
    let it = ui.interact(id, track, Sense::click());
    let (ty, th) = thumb_geom(track.y, track.h, &m, *scroll, min_thumb);
    if let Some(p) = it.pressed {
        if p.pos.y >= ty && p.pos.y < ty + th {
            // grab offset inside the thumb
            ui.drag_f64 = (p.pos.y - ty) as f64;
        } else {
            ui.drag_f64 = f64::NAN; // track press: no drag
            *scroll = if p.pos.y < ty { *scroll - m.page } else { *scroll + m.page };
        }
    }
    if it.active && !ui.drag_f64.is_nan() {
        let top = ui.mouse().y - ui.drag_f64 as f32;
        *scroll = scroll_for_thumb(track.y, track.h, th, &m, top);
    }
    *scroll = scroll.clamp(0.0, m.max_scroll());

    let t = &ui.theme;
    c.fill(track, t.sb_track);
    if m.content > m.view {
        let (ty, th) = thumb_geom(track.y, track.h, &m, *scroll, min_thumb);
        let dragging = it.active && !ui.drag_f64.is_nan();
        let over_thumb = it.hovered && ui.mouse().y >= ty && ui.mouse().y < ty + th;
        let col = if dragging {
            t.sb_thumb_drag
        } else if over_thumb {
            t.sb_thumb_hot
        } else {
            t.sb_thumb
        };
        c.round(Rect::new(track.x + 3.0, ty, (track.w - 6.0).max(2.0), th), t.radius, col);
    }
    *scroll != before
}
