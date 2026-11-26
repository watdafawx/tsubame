//! Column header of a details list: labels, a sort arrow on the sorted
//! column, hover, click-to-sort and drag-to-resize handles. The app owns the
//! column widths (`spans`) and the sort state; the header reports clicks and
//! drags.

use crate::canvas::Canvas;
use crate::geom::Rect;
use crate::input::{Button, Cursor};
use crate::ui::{Id, Sense, Ui};

/// A column label.
#[derive(Clone, Copy, Debug)]
pub struct HeaderCol<'a> {
    pub label: &'a str,
    /// label and arrow right-aligned (numbers)
    pub right: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeaderResponse {
    /// a column was clicked: sort by it
    pub sort: Option<usize>,
    /// the left edge of column `.0` is being dragged to x `.1` (relative to
    /// the header's left edge)
    pub resize: Option<(usize, f32)>,
    /// right click released on column `.0` at this point
    pub context: Option<(usize, crate::geom::Pt)>,
    /// double click on the right edge of column `.0`: fit it to its content
    pub auto_fit: Option<usize>,
    /// a header cell was dragged and dropped: move column `.0` before column `.1`
    /// (`.1 == cols.len()` is after the last one). Never column 0, never to slot 0.
    pub reorder: Option<(usize, usize)>,
}

/// Distance the pointer moves along the header before a pressed cell becomes a drag (DIPs).
pub const DRAG_START: f32 = 6.0;

/// The state of a column drag across frames (the app owns it; `enabled` switches reordering on).
/// A header without one (`column_header`) sorts and resizes only.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeaderDrag {
    pub enabled: bool,
    /// the cell pressed and the x of the press
    press: Option<(usize, f32)>,
    /// the pressed cell has moved far enough: a drag, not a click
    dragging: bool,
    /// where it would land now (the gap before this column)
    drop: Option<usize>,
}

impl HeaderDrag {
    pub fn new() -> HeaderDrag {
        HeaderDrag { enabled: true, ..HeaderDrag::default() }
    }
    /// A column is being dragged.
    pub fn is_dragging(&self) -> bool {
        self.dragging
    }
    fn reset(&mut self) {
        self.press = None;
        self.dragging = false;
        self.drop = None;
    }
}

/// The gap (0..=n) whose left edge is nearest to `x`, among the gaps that can take a column:
/// not before the first column (Name keeps its place).
pub fn drop_gap(spans: &[(f32, f32)], n: usize, x: f32) -> usize {
    let mut best = 1.min(n);
    let mut best_d = f32::MAX;
    for k in 1..=n {
        let edge = if k < n { spans[k].0 } else { spans[n - 1].0 + spans[n - 1].1 };
        let d = (edge - x).abs();
        if d < best_d {
            best_d = d;
            best = k;
        }
    }
    best
}

/// `Some((from, gap))` when moving column `from` into `gap` changes the order.
pub fn reorder_of(from: usize, gap: usize) -> Option<(usize, usize)> {
    (from >= 1 && gap >= 1 && gap != from && gap != from + 1).then_some((from, gap))
}

/// Width of the grab zone on a column's left edge.
const GRAB: f32 = 6.0;
const ARROW_W: f32 = 8.0;
const ARROW_H: f32 = 5.0;

/// Draws a small filled triangle `w` x `h` in `r` (pointing up or down) as
/// stacked 1 DIP rows: no path support needed in `Canvas`.
fn arrow(c: &mut dyn Canvas, x: f32, y: f32, descending: bool, color: u32) {
    let rows = ARROW_H as usize;
    for k in 0..rows {
        let frac = (k as f32 + 1.0) / rows as f32;
        let w = (ARROW_W * frac).round().max(1.0);
        // ascending: narrow at the top; descending: narrow at the bottom
        let row = if descending { rows - 1 - k } else { k };
        c.fill(Rect::new(x + (ARROW_W - w) / 2.0, y + row as f32, w, 1.0), color);
    }
}

/// The header in `rect`. `spans` are the column x/width pairs relative to
/// `rect.x` (see `cols::Columns::spans` in the app); `sorted` is the sorted
/// column and whether it is descending.
pub fn column_header(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, cols: &[HeaderCol], spans: &[(f32, f32)], sorted: Option<(usize, bool)>) -> HeaderResponse {
    column_header_ex(ui, c, id, rect, cols, spans, sorted, &mut HeaderDrag::default())
}

/// `column_header` with drag-to-reorder: press a cell (not the first), move it sideways past
/// `DRAG_START` and an accent line shows where it would land; letting go reports `reorder`
/// (and no sort click).
#[allow(clippy::too_many_arguments)]
pub fn column_header_ex(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, cols: &[HeaderCol], spans: &[(f32, f32)], sorted: Option<(usize, bool)>, drag: &mut HeaderDrag) -> HeaderResponse {
    let mut resp = HeaderResponse::default();
    let t = ui.theme;
    c.fill(rect, t.panel);
    c.fill(Rect::new(rect.x, rect.b() - 1.0, rect.w, 1.0), t.border);
    let body = Rect::new(rect.x, rect.y, rect.w, rect.h - 1.0);

    // cells first, resize handles after them (on top for clicks)
    let mut clicked: Option<usize> = None;
    let mut hot: Option<usize> = None;
    for (i, &(x, w)) in spans.iter().enumerate().take(cols.len()) {
        let cell = Rect::new(rect.x + x, body.y, w, body.h);
        let it = ui.interact(id.with(i as u64), cell, Sense::click());
        if drag.enabled && i >= 1 {
            if let Some(p) = it.pressed.filter(|p| p.button == Button::Left && !p.double) {
                drag.press = Some((i, p.pos.x));
                drag.dragging = false;
                drag.drop = None;
            }
            if it.active
                && let Some((_, x0)) = drag.press.filter(|&(f, _)| f == i)
            {
                let mx = ui.mouse().x;
                if !drag.dragging && (mx - x0).abs() >= DRAG_START {
                    drag.dragging = true;
                }
                if drag.dragging {
                    drag.drop = Some(drop_gap(spans, cols.len().min(spans.len()), mx - rect.x));
                    ui.request_repaint();
                }
            }
            if it.released && drag.press.is_some_and(|(f, _)| f == i) {
                if drag.dragging {
                    resp.reorder = drag.drop.and_then(|gap| reorder_of(i, gap));
                    ui.request_repaint();
                }
                // the sort click that would follow a drag is not a click
                let was_drag = drag.dragging;
                drag.reset();
                if was_drag {
                    continue;
                }
            }
        }
        if it.clicked == Some(Button::Left) {
            clicked = Some(i);
        }
        if it.clicked == Some(Button::Right) {
            resp.context = Some((i, it.click_pos));
        }
        if it.hovered && ui.active().is_none_or(|a| a == id.with(i as u64)) {
            hot = Some(i);
        }
    }
    for (i, span) in spans.iter().enumerate().take(cols.len()).skip(1) {
        let edge = rect.x + span.0;
        let grab = Rect::new(edge - GRAB / 2.0, body.y, GRAB, body.h);
        let hid = id.child("grip").with(i as u64);
        let it = ui.interact(hid, grab, Sense::click().cursor(Cursor::SizeWE));
        if it.pressed.is_some_and(|p| p.double && p.button == Button::Left) {
            // the grip is the left edge of column i: the right edge of i - 1
            resp.auto_fit = Some(i - 1);
        } else if it.active {
            resp.resize = Some((i, ui.mouse().x - rect.x));
            ui.request_repaint();
        }
    }
    // the right edge of the last column: double click fits it (no drag)
    if let Some(&(x, w)) = spans.iter().take(cols.len()).next_back()
        && cols.len() >= 2
    {
        let grab = Rect::new(rect.x + x + w - GRAB, body.y, GRAB, body.h);
        let it = ui.interact(id.child("grip").with(cols.len() as u64), grab, Sense::click().cursor(Cursor::SizeWE));
        if it.pressed.is_some_and(|p| p.double && p.button == Button::Left) {
            resp.auto_fit = Some(cols.len() - 1);
        }
    }
    resp.sort = clicked;
    if drag.dragging {
        hot = drag.press.map(|p| p.0);
    }

    for (i, col) in cols.iter().enumerate().take(spans.len()) {
        let (x, w) = spans[i];
        let cell = Rect::new(rect.x + x, body.y, w, body.h);
        let is_sorted = sorted.is_some_and(|s| s.0 == i);
        if hot == Some(i) {
            c.fill(cell, t.hover);
        }
        let color = if is_sorted || hot == Some(i) { t.text } else { t.text_dim };
        let font = if col.right { t.font_small().r() } else { t.font_small() };
        let text_r = Rect::new(cell.x + 8.0, cell.y, (cell.w - 16.0).max(0.0), cell.h);
        c.text(col.label, text_r, font, color);
        if let Some((_, desc)) = sorted.filter(|s| s.0 == i) {
            // arrow right after the label (left aligned) or before it (right aligned)
            let label_w = c.measure(col.label, font).min(text_r.w);
            let ax = if col.right { text_r.r() - label_w - 4.0 - ARROW_W } else { (text_r.x + label_w + 4.0).min(cell.r() - ARROW_W - 4.0) };
            arrow(c, ax.max(cell.x + 2.0), cell.y + (cell.h - ARROW_H) / 2.0, desc, t.text);
        }
        // separator on the right edge (not on the last column)
        if i + 1 < spans.len().min(cols.len()) {
            c.fill(Rect::new(cell.r() - 1.0, cell.y + 6.0, 1.0, cell.h - 12.0), t.border);
        }
    }
    // where the dragged column would land
    if drag.dragging
        && let (Some((from, _)), Some(gap)) = (drag.press, drag.drop)
        && reorder_of(from, gap).is_some()
        && let n = cols.len().min(spans.len())
        && n > 0
    {
        let edge = if gap < n { spans[gap].0 } else { spans[n - 1].0 + spans[n - 1].1 };
        c.fill(Rect::new(rect.x + edge - 1.0, rect.y + 2.0, 2.0, rect.h - 4.0), t.accent);
    }
    resp
}
