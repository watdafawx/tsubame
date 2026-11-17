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
        if it.clicked == Some(Button::Left) {
            clicked = Some(i);
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
        if it.active {
            resp.resize = Some((i, ui.mouse().x - rect.x));
            ui.request_repaint();
        }
    }
    resp.sort = clicked;

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
    resp
}
