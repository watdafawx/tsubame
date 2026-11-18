//! Splitter between two panes. `pos` is the size of the first pane in DIPs.

use crate::canvas::Canvas;
use crate::geom::Rect;
use crate::input::Cursor;
use crate::ui::{Id, Sense, Ui};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// panes side by side, the bar is vertical
    LeftRight,
    /// panes stacked, the bar is horizontal
    TopBottom,
}

/// Splits `area`, draws the bar, handles its drag. Returns (first, second,
/// changed). `pos` is clamped so both panes keep `min_a` / `min_b`.
#[allow(clippy::too_many_arguments)]
pub fn splitter(ui: &mut Ui, c: &mut dyn Canvas, id: Id, area: Rect, split: Split, pos: &mut f32, min_a: f32, min_b: f32) -> (Rect, Rect, bool) {
    let bw = ui.theme.splitter_w();
    let len = if split == Split::LeftRight { area.w } else { area.h };
    let clamp = |p: f32| p.clamp(min_a.min(len - bw), (len - bw - min_b).max(min_a.min(len - bw))).max(0.0);
    let before = *pos;
    *pos = clamp(*pos);
    let bar = |p: f32| match split {
        Split::LeftRight => Rect::new(area.x + p, area.y, bw, area.h),
        Split::TopBottom => Rect::new(area.x, area.y + p, area.w, bw),
    };
    let cursor = if split == Split::LeftRight { Cursor::SizeWE } else { Cursor::SizeNS };
    let it = ui.interact(id, bar(*pos), Sense::click().cursor(cursor));
    let m = ui.mouse();
    let along = |x: f32, y: f32| if split == Split::LeftRight { x - area.x } else { y - area.y };
    if let Some(p) = it.pressed {
        if p.double {
            // double-click puts the bar back in the middle
            *pos = clamp((len - bw) / 2.0);
        }
        ui.drag_f64 = (along(p.pos.x, p.pos.y) - *pos) as f64;
    }
    if it.active {
        *pos = clamp(along(m.x, m.y) - ui.drag_f64 as f32);
    }
    let b = bar(*pos);
    let t = &ui.theme;
    let col = if it.active || it.hovered { t.accent } else { t.border };
    match split {
        Split::LeftRight => c.fill(Rect::new(b.x + (bw / 2.0).floor(), b.y, 1.0, b.h), col),
        Split::TopBottom => c.fill(Rect::new(b.x, b.y + (bw / 2.0).floor(), b.w, 1.0), col),
    }
    let (a, rest) = match split {
        Split::LeftRight => area.split_x(*pos),
        Split::TopBottom => area.split_y(*pos),
    };
    let second = match split {
        Split::LeftRight => rest.pad4(bw, 0.0, 0.0, 0.0),
        Split::TopBottom => rest.pad4(0.0, bw, 0.0, 0.0),
    };
    (a, second, *pos != before)
}
