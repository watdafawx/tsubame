//! Grouped layout of the grid: per group a full-width header row, then the
//! group's cells wrapped to the width. `grid` hands over to `grid_grouped`
//! when its source has groups.
//!
//! Geometry is cached in `GridState::geo` (one top per group, rebuilt when the
//! grouping, the column count or the cell sizes change), so a frame costs a
//! binary search plus the cells it draws, and nothing is allocated.

use crate::canvas::Canvas;
use crate::geom::{Pt, Rect};
use crate::input::{Button, Event, Key, Mods};
use crate::ui::{Id, Sense, Ui};
use crate::widgets::grid::{BAND_DEADZONE, Band, BandCells, GridResponse, GridSource, GridState};
use crate::widgets::grouping::{GroupRows, Row};
use crate::widgets::list::{BandMode, HeaderState, RowState, SelectBy};
use crate::widgets::scrollbar::{ScrollMetrics, vscrollbar};

/// Cells a rubber band covers in one group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupBand {
    pub group: usize,
    /// rows and columns inside the group's own cell block
    pub cells: BandCells,
}

/// What is under a point of a grouped grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupHit {
    Header(usize),
    /// `(group, place in the group)`
    Cell(usize, usize),
    None,
}

/// Cached vertical layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupGeo {
    key: (u64, usize, usize, u32, u32),
    /// y of every group's header in content coordinates
    tops: Vec<f64>,
    total: f64,
    /// the last grid call was grouped
    pub(crate) active: bool,
}

impl GroupGeo {
    pub fn total(&self) -> f64 {
        self.total
    }
    pub fn top(&self, g: usize) -> f64 {
        self.tops.get(g).copied().unwrap_or(self.total)
    }
    pub fn bottom(&self, g: usize) -> f64 {
        self.tops.get(g + 1).copied().unwrap_or(self.total)
    }
    /// Group whose block contains content y.
    pub fn group_at(&self, y: f64) -> Option<usize> {
        if y < 0.0 || y >= self.total {
            return None;
        }
        self.tops.partition_point(|&t| t <= y).checked_sub(1)
    }
    fn sync(&mut self, g: &dyn GroupRows, cols: usize, ch: f32, hh: f32) {
        let key = (g.revision(), g.group_count(), cols, ch.to_bits(), hh.to_bits());
        self.active = true;
        if self.key == key && self.tops.len() == g.group_count() {
            return;
        }
        self.tops.clear();
        let mut y = 0.0;
        for i in 0..g.group_count() {
            self.tops.push(y);
            y += hh as f64 + g.shown_len(i).div_ceil(cols) as f64 * ch as f64;
        }
        self.total = y;
        self.key = key;
    }
}

/// Next group after `g` (or before, `forward` false) that shows items.
fn group_with_items(gr: &dyn GroupRows, g: usize, forward: bool) -> Option<usize> {
    if forward {
        (g + 1..gr.group_count()).find(|&i| gr.shown_len(i) > 0)
    } else {
        (0..g).rev().find(|&i| gr.shown_len(i) > 0)
    }
}

/// The item a cursor key moves to in a grouped grid (`None`: stay). Headers
/// are skipped, collapsed groups have no cells, Left/Right walk the visual
/// order across groups.
pub fn group_nav(gr: &dyn GroupRows, key: Key, cur: usize, cols: usize, page_rows: usize) -> Option<usize> {
    let cols = cols.max(1);
    let item_at = |row: usize| match gr.row_at(row) {
        Row::Item(i) => Some(i),
        Row::Header(_) => None,
    };
    let step_down = |g: usize, j: usize| -> Option<(usize, usize)> {
        let shown = gr.shown_len(g);
        if j + cols < shown {
            Some((g, j + cols))
        } else if j / cols < (shown - 1) / cols {
            Some((g, shown - 1)) // the row below is the short last row: its last cell
        } else {
            let n = group_with_items(gr, g, true)?;
            Some((n, (j % cols).min(gr.shown_len(n) - 1)))
        }
    };
    let step_up = |g: usize, j: usize| -> Option<(usize, usize)> {
        if j >= cols {
            Some((g, j - cols))
        } else {
            let p = group_with_items(gr, g, false)?;
            let last = gr.shown_len(p) - 1;
            Some((p, ((last / cols) * cols + j % cols).min(last)))
        }
    };
    let row = gr.item_row(cur)?;
    let (g, j) = gr.item_slot(cur)?;
    let target = match key {
        Key::Left => gr.nav_row(row.checked_sub(1)?, false).and_then(item_at),
        Key::Right => gr.nav_row(row + 1, true).and_then(item_at),
        Key::Up | Key::PageUp => {
            let mut at = (g, j);
            for _ in 0..if key == Key::Up { 1 } else { page_rows } {
                match step_up(at.0, at.1) {
                    Some(n) => at = n,
                    None => break,
                }
            }
            Some(gr.group_item(at.0, at.1))
        }
        Key::Down | Key::PageDown => {
            let mut at = (g, j);
            for _ in 0..if key == Key::Down { 1 } else { page_rows } {
                match step_down(at.0, at.1) {
                    Some(n) => at = n,
                    None => break,
                }
            }
            Some(gr.group_item(at.0, at.1))
        }
        Key::Home => gr.nav_row(0, true).and_then(item_at),
        Key::End => gr.nav_row(gr.row_count().checked_sub(1)?, false).and_then(item_at),
        _ => None,
    }?;
    (target != cur).then_some(target)
}

impl GridState {
    /// Brings the grouped geometry up to date for `g` (the grid does it every
    /// frame; the app calls it before using the methods below outside a frame).
    pub fn sync_groups(&mut self, g: &dyn GroupRows) {
        let cols = self.cols.max(1);
        self.geo.sync(g, cols, self.cell_h, self.header_h);
    }
    /// What is under `(x, y)` DIPs from the top-left of the cells area.
    pub fn group_hit(&self, g: &dyn GroupRows, x: f32, y: f32) -> GroupHit {
        if x < 0.0 || y < 0.0 {
            return GroupHit::None;
        }
        let cy = self.scroll + y as f64;
        let Some(gi) = self.geo.group_at(cy) else { return GroupHit::None };
        let rel = cy - self.geo.top(gi);
        if rel < self.header_h as f64 {
            return GroupHit::Header(gi);
        }
        let col = (x / self.cell_w.max(1.0)) as usize;
        let row = ((rel - self.header_h as f64) / self.cell_h.max(1.0) as f64) as usize;
        let j = row * self.cols.max(1) + col;
        if col < self.cols && j < g.shown_len(gi) { GroupHit::Cell(gi, j) } else { GroupHit::None }
    }
    /// Top-left of a shown item relative to the cells area (may be off screen).
    pub fn group_item_origin(&self, g: &dyn GroupRows, item: usize) -> Option<(f32, f32)> {
        let (gi, j) = g.item_slot(item)?;
        let cols = self.cols.max(1);
        let y = self.geo.top(gi) + self.header_h as f64 + (j / cols) as f64 * self.cell_h as f64 - self.scroll;
        Some(((j % cols) as f32 * self.cell_w, y as f32))
    }
    /// Top-left of group `gi`'s header relative to the cells area.
    pub fn group_header_y(&self, gi: usize) -> f32 {
        (self.geo.top(gi) - self.scroll) as f32
    }
    /// Scrolls the least amount that shows content `y0..y1` (the top wins when it is taller).
    pub fn ensure_span(&mut self, y0: f64, y1: f64) -> bool {
        let vh = self.view_h as f64;
        let max = (self.geo.total - vh).max(0.0);
        let s = if y0 < self.scroll {
            y0
        } else if y1 > self.scroll + vh {
            y1 - vh
        } else {
            return false;
        };
        let s = s.clamp(0.0, max);
        let changed = s != self.scroll;
        self.scroll = s;
        changed
    }
    /// Scrolls a shown item's cell into view (its header too when it is on the first row).
    pub fn reveal_group_item(&mut self, g: &dyn GroupRows, item: usize, with_header: bool) -> bool {
        let Some((gi, j)) = g.item_slot(item) else { return false };
        let cols = self.cols.max(1);
        let top = self.geo.top(gi);
        let y = top + self.header_h as f64 + (j / cols) as f64 * self.cell_h as f64;
        let y0 = if with_header && j < cols { top } else { y };
        self.ensure_span(y0, y + self.cell_h as f64)
    }
    /// Every group the view shows any of (its header included): calls
    /// `f(group, first place, last place + 1)`, the places being empty for a collapsed group.
    pub fn visible_groups(&self, g: &dyn GroupRows, mut f: impl FnMut(usize, usize, usize)) {
        let (y0, y1) = (self.scroll.max(0.0), self.scroll + self.view_h as f64);
        let cols = self.cols.max(1);
        let Some(mut gi) = self.geo.group_at(y0) else { return };
        while gi < g.group_count() && self.geo.top(gi) < y1 {
            let shown = g.shown_len(gi);
            let base = self.geo.top(gi) + self.header_h as f64;
            let (mut a, mut b) = (0, 0);
            if shown > 0 {
                let r0 = ((y0 - base) / self.cell_h as f64).floor().max(0.0) as usize;
                let r1 = (((y1 - base) / self.cell_h as f64).ceil().max(0.0) as usize).min(shown.div_ceil(cols));
                (a, b) = ((r0 * cols).min(shown), (r1 * cols).min(shown));
            }
            f(gi, a, b.max(a));
            gi += 1;
        }
    }
    /// Items on screen: calls `f(group, first place, last place + 1)` for each visible
    /// stretch of whole rows, widened by `margin_rows` each way (prefetch).
    pub fn group_visible(&self, g: &dyn GroupRows, margin_rows: usize, mut f: impl FnMut(usize, usize, usize)) {
        let margin = margin_rows as f64 * self.cell_h as f64;
        let (y0, y1) = ((self.scroll - margin).max(0.0), self.scroll + self.view_h as f64 + margin);
        let cols = self.cols.max(1);
        let Some(mut gi) = self.geo.group_at(y0) else { return };
        while gi < g.group_count() && self.geo.top(gi) < y1 {
            let shown = g.shown_len(gi);
            let base = self.geo.top(gi) + self.header_h as f64;
            if shown > 0 && base < y1 && self.geo.bottom(gi) > y0 {
                let r0 = ((y0 - base) / self.cell_h as f64).floor().max(0.0) as usize;
                let r1 = (((y1 - base) / self.cell_h as f64).ceil().max(0.0) as usize).min(shown.div_ceil(cols));
                let (a, b) = ((r0 * cols).min(shown), (r1 * cols).min(shown));
                if a < b {
                    f(gi, a, b);
                }
            }
            gi += 1;
        }
    }
}

/// The grouped grid in `rect`; the body of `grid` for a source with groups.
pub(crate) fn grid_grouped(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, st: &mut GridState, src: &mut dyn GridSource) -> GridResponse {
    let mut resp = GridResponse::default();
    let len = src.len();
    let mut area = rect;
    let track = area.cut_right(ui.theme.layout_scrollbar_width);
    let scroll0 = st.scroll;
    let (cw, ch, hh) = (st.cell_w, st.cell_h, st.header_h);
    let width_w = area.w.max(0.0);

    // reflow: keep the first item on the top row on screen when the column count changes
    let cols = st.cols_for(area.w);
    let cols_before = st.cols.max(1);
    if cols != st.cols {
        // anchor: the header, or the first item of the top row, of the top group
        let anchor = {
            let g = src.groups().expect("grouped");
            st.geo.sync(g, cols_before, ch, hh);
            let cy = st.scroll.max(0.0);
            st.geo.group_at(cy).map(|gi| {
                let rel = cy - st.geo.top(gi);
                (gi, if rel < hh as f64 { Err(rel) } else { Ok((((rel - hh as f64) / ch as f64) as usize) * cols_before) })
            })
        };
        st.cols = cols;
        let g = src.groups().expect("grouped");
        st.geo.sync(g, cols, ch, hh);
        if let Some((gi, at)) = anchor {
            st.scroll = st.geo.top(gi)
                + match at {
                    Err(rel) => rel,
                    Ok(j) => hh as f64 + (j / cols) as f64 * ch as f64,
                };
        }
    }
    st.view_w = width_w;
    st.view_h = area.h.max(0.0);
    {
        let g = src.groups().expect("grouped");
        st.geo.sync(g, cols, ch, hh);
    }
    st.scroll = st.scroll.clamp(0.0, st.max_scroll(len));
    if len > 0 && st.cursor >= len {
        st.cursor = len - 1;
    }
    if st.reveal {
        st.reveal = false;
        if let Some(g) = src.groups() {
            st.reveal_group_item(g, st.cursor, false);
        }
    }
    resp.cols = cols;

    let origin_y = area.y - st.scroll as f32;
    let mut sense = Sense::click().focusable().wheel().cells_2d(area.x, cw, origin_y, ch);
    // headers and cells do not share one raster: repaint on every move over the grid
    sense.track_moves = true;
    let it = ui.interact(id, rect, sense);

    if it.wheel.1 != 0.0 {
        if it.wheel_mods.ctrl {
            resp.zoom = it.wheel.1;
        } else {
            let step = (ui.theme.wheel_lines() * ui.theme.height_list_row).max(ch * 0.6) as f64;
            st.set_scroll(st.scroll - it.wheel.1 as f64 * step, len);
        }
    }

    let hit_at = |st: &GridState, src: &dyn GridSource, p: Pt| -> GroupHit {
        match src.groups() {
            Some(g) => st.group_hit(g, p.x - area.x, p.y - area.y),
            None => GroupHit::None,
        }
    };
    let item_of = |src: &dyn GridSource, gi: usize, j: usize| src.groups().map_or(usize::MAX, |g| g.group_item(gi, j));

    // mouse
    if let Some(p) = it.pressed
        && area.contains_pt(p.pos)
    {
        if p.button != Button::Right {
            resp.pressed = true;
        }
        match (hit_at(st, src, p.pos), p.button) {
            (GroupHit::Header(gi), Button::Left) => {
                st.band = None;
                st.header = Some((gi, st.cursor));
                if !p.double {
                    toggle(st, src, &mut resp, gi, p.mods.ctrl, cols);
                }
            }
            (GroupHit::Header(_), Button::Right) => st.band = None,
            (GroupHit::Header(_), Button::Middle) => {}
            (GroupHit::Cell(gi, j), Button::Left) => {
                let i = item_of(src, gi, j);
                st.band = None;
                st.header = None;
                let moved = i != st.cursor;
                st.cursor = i;
                if let Some(g) = src.groups() {
                    st.reveal_group_item(g, i, false);
                }
                resp.cursor_moved |= moved;
                src.select(i, p.mods, SelectBy::Click);
                if p.double {
                    resp.activated = Some(i);
                }
            }
            (GroupHit::Cell(gi, j), Button::Right) => {
                let i = item_of(src, gi, j);
                st.band = None;
                st.header = None;
                st.cursor = i;
                src.select(i, p.mods, SelectBy::RightClick);
            }
            (GroupHit::Cell(..), Button::Middle) => {}
            (GroupHit::None, Button::Left) => {
                if p.double && p.mods.none() {
                    st.band = None;
                    resp.empty_double_click = true;
                } else {
                    st.band = Some(Band { sx: p.pos.x - area.x, sy: (p.pos.y - area.y) as f64 + st.scroll, started: false });
                    src.band_begin(BandMode::from_mods(p.mods));
                }
            }
            (GroupHit::None, Button::Right) => {
                st.band = None;
                src.select_none();
            }
            (GroupHit::None, Button::Middle) => {}
        }
    }
    if it.clicked == Some(Button::Right) && area.contains_pt(it.click_pos) {
        let cell = match hit_at(st, src, it.click_pos) {
            GroupHit::Cell(gi, j) => Some(item_of(src, gi, j)),
            _ => None,
        };
        resp.context = Some((cell, it.click_pos));
    }
    if it.clicked == Some(Button::Middle) && area.contains_pt(it.click_pos)
        && let GroupHit::Cell(gi, j) = hit_at(st, src, it.click_pos)
    {
        resp.middle_clicked = Some(item_of(src, gi, j));
    }

    // keys
    for (qi, ev) in ui.key_input(id) {
        let Event::KeyDown { key, mods, .. } = ev else { continue };
        if key == Key::Escape && mods.none() && st.band.is_some() {
            ui.consume(qi);
            if st.band.take().is_some_and(|b| b.started) {
                src.band_end();
            }
            ui.clear_active();
            ui.request_repaint();
            continue;
        }
        // keys on a focused header
        if let Some(h) = st.header_focus() {
            let collapsed = src.groups().is_some_and(|g| g.is_collapsed(h));
            match key {
                Key::Left | Key::Right if mods.none() => {
                    ui.consume(qi);
                    if collapsed == (key == Key::Right) {
                        toggle(st, src, &mut resp, h, false, cols);
                    }
                    continue;
                }
                Key::Enter | Key::Space if mods.none() => {
                    ui.consume(qi);
                    toggle(st, src, &mut resp, h, false, cols);
                    continue;
                }
                Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End => {
                    ui.consume(qi);
                    let found = src.groups().and_then(|g| {
                        let hr = g.header_row(h);
                        let row = match key {
                            Key::Up | Key::PageUp => g.nav_row(hr.checked_sub(1)?, false),
                            Key::Down | Key::PageDown => g.nav_row(hr + 1, true),
                            Key::Home => g.nav_row(0, true),
                            _ => g.nav_row(g.row_count().checked_sub(1)?, false),
                        }?;
                        match g.row_at(row) {
                            Row::Item(i) => Some(i),
                            Row::Header(_) => None,
                        }
                    });
                    if let Some(i) = found {
                        st.header = None;
                        st.cursor = i;
                        if let Some(g) = src.groups() {
                            st.reveal_group_item(g, i, matches!(key, Key::Up | Key::PageUp | Key::Home));
                        }
                        resp.cursor_moved = true;
                        src.select(i, mods, SelectBy::Key);
                    }
                    continue;
                }
                _ => {}
            }
        }
        let page = st.page_rows();
        let moved = src.groups().and_then(|g| group_nav(g, key, st.cursor, cols, page));
        let is_nav = matches!(key, Key::Left | Key::Right | Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End);
        if is_nav {
            ui.consume(qi);
            if len > 0 {
                // a cursor on a hidden item restarts from the ends
                let hidden = src.groups().is_some_and(|g| g.item_row(st.cursor).is_none());
                let t = if hidden {
                    src.groups().and_then(|g| match key {
                        Key::Home | Key::Down | Key::PageDown | Key::Right => g.nav_row(0, true),
                        _ => g.nav_row(g.row_count().saturating_sub(1), false),
                    })
                    .and_then(|r| src.groups().map(|g| g.row_at(r)))
                    .and_then(|r| match r {
                        Row::Item(i) => Some(i),
                        Row::Header(_) => None,
                    })
                } else {
                    moved
                };
                if let Some(t) = t {
                    st.header = None;
                    st.cursor = t;
                    if let Some(g) = src.groups() {
                        st.reveal_group_item(g, t, matches!(key, Key::Up | Key::PageUp | Key::Home | Key::Left));
                    }
                    resp.cursor_moved = true;
                    src.select(t, mods, SelectBy::Key);
                }
            }
            continue;
        }
        match key {
            Key::Enter if mods.none() && len > 0 => {
                ui.consume(qi);
                resp.activated = Some(st.cursor);
            }
            Key::Space if len > 0 && !mods.alt => {
                ui.consume(qi);
                src.select(st.cursor, mods, SelectBy::Space);
            }
            Key::F(10) if mods == Mods::SHIFT => {
                ui.consume(qi);
                resp.context_key = true;
            }
            _ => {}
        }
    }

    // rubber band: edge autoscroll, then the covered cells of every group
    let mut band_rect: Option<Rect> = None;
    if let Some(mut b) = st.band {
        if !it.active || !ui.is_active(id) {
            if b.started {
                src.band_end();
            }
            st.band = None;
            ui.request_repaint();
        } else {
            let m = ui.mouse();
            let past = if m.y < area.y { m.y - area.y } else if m.y > area.b() { m.y - area.b() } else { 0.0 };
            if past != 0.0 && b.started {
                let speed = (5.0 + past.abs() * 0.4).min(30.0) as f64;
                if st.set_scroll(st.scroll + speed * past.signum() as f64, len) {
                    ui.request_repaint();
                }
            }
            let max_y = st.geo.total().max(st.scroll + st.view_h as f64).max(1.0);
            let mx = (m.x - area.x).clamp(0.0, st.view_w);
            let my = ((m.y - area.y) as f64 + st.scroll).clamp(0.0, max_y);
            if !b.started && (mx - b.sx).hypot((my - b.sy) as f32) >= BAND_DEADZONE {
                b.started = true;
                st.band = Some(b);
            }
            if b.started {
                let (x0, x1) = (b.sx.min(mx), b.sx.max(mx));
                let (y0, y1) = (b.sy.min(my), b.sy.max(my));
                st.band_scratch.clear();
                if let Some(g) = src.groups()
                    && x0 < cols as f32 * cw
                    && x1 > 0.0
                {
                    let c0 = (x0 / cw).floor().max(0.0) as usize;
                    let c1 = ((((x1 - 1e-4).max(x0)) / cw).floor().max(0.0) as usize).min(cols - 1);
                    if let Some(first) = st.geo.group_at(y0.max(0.0)) {
                        let mut gi = first;
                        while gi < g.group_count() && st.geo.top(gi) < y1 {
                            let shown_rows = g.shown_len(gi).div_ceil(cols);
                            let base = st.geo.top(gi) + hh as f64;
                            let end = base + shown_rows as f64 * ch as f64;
                            if shown_rows > 0 && y1 > base && y0 < end {
                                let r0 = ((y0.max(base) - base) / ch as f64).floor().max(0.0) as usize;
                                let r1 = ((((y1.min(end) - base) - 1e-6).max(0.0)) / ch as f64).floor() as usize;
                                st.band_scratch.push(GroupBand { group: gi, cells: BandCells { rows: (r0, r1.min(shown_rows - 1)), cols: (c0, c1) } });
                            }
                            gi += 1;
                        }
                    }
                }
                src.band_update_groups(&st.band_scratch, cols);
                band_rect = Some(Rect::new(area.x + x0, area.y + (y0 - st.scroll) as f32, x1 - x0, (y1 - y0) as f32));
            }
        }
    }

    // scrollbar
    let m = ScrollMetrics { content: st.geo.total(), view: st.view_h as f64, page: st.page_rows() as f64 * ch as f64 };
    vscrollbar(ui, c, id.child("sb"), track, m, &mut st.scroll);
    resp.scrolled = st.scroll != scroll0;

    // headers and cells that intersect the viewport
    let focused = it.focused && ui.window_focused();
    let hot = if it.hovered && ui.active().is_none_or(|a| a == id) && area.contains_pt(ui.mouse()) && !st.banding() { hit_at(st, src, ui.mouse()) } else { GroupHit::None };
    resp.hovered = match hot {
        GroupHit::Cell(gi, j) => Some(item_of(src, gi, j)),
        _ => None,
    };
    let header_cur = st.header_focus();
    let theme = &ui.theme;
    c.clip(area);
    let (view0, view1) = (st.scroll, st.scroll + st.view_h as f64);
    let n_groups = src.groups().map_or(0, |g| g.group_count());
    let mut gi = st.geo.group_at(view0).unwrap_or(n_groups);
    while gi < n_groups && st.geo.top(gi) < view1 {
        let top = st.geo.top(gi);
        let hr = Rect::new(area.x, area.y + (top - st.scroll) as f32, area.w, hh);
        let (collapsed, shown) = src.groups().map_or((false, 0), |g| (g.is_collapsed(gi), g.shown_len(gi)));
        if hr.b() > area.y && hr.y < area.b() {
            let is_hot = hot == GroupHit::Header(gi);
            if is_hot {
                c.fill(hr, theme.hover);
            }
            let cursor = header_cur == Some(gi);
            if cursor && focused {
                c.round_stroke(hr, 2.0, theme.focus, 1.0);
            }
            src.draw_header(c, theme, gi, hr, HeaderState { collapsed, hot: is_hot, cursor, focused });
        }
        let base = top + hh as f64;
        let rows_n = shown.div_ceil(cols);
        if rows_n > 0 && base < view1 {
            let r0 = ((view0 - base) / ch as f64).floor().max(0.0) as usize;
            let r1 = (((view1 - base) / ch as f64).ceil().max(0.0) as usize).min(rows_n);
            for row in r0..r1 {
                let y = area.y + (base + row as f64 * ch as f64 - st.scroll) as f32;
                for col in 0..cols {
                    let j = row * cols + col;
                    if j >= shown {
                        break;
                    }
                    let i = item_of(src, gi, j);
                    let r = Rect::new(area.x + col as f32 * cw, y, cw, ch);
                    let tile = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0);
                    let rs = RowState { selected: src.is_selected(i), cursor: i == st.cursor && header_cur.is_none(), hot: hot == GroupHit::Cell(gi, j), focused };
                    if rs.selected {
                        c.round(tile, theme.radius_md, if focused { theme.selected } else { theme.selected_inactive });
                    } else if rs.hot {
                        c.round(tile, theme.radius_md, theme.hover);
                    }
                    if rs.cursor && focused {
                        c.round_stroke(tile, theme.radius_md, theme.focus, 1.0);
                    }
                    src.draw_cell(c, theme, i, r, rs);
                }
            }
        }
        gi += 1;
    }
    if let Some(br) = band_rect {
        c.fill_a(br, theme.accent, 0.18);
        c.round_stroke(br, 0.0, theme.accent, 1.0);
    }
    c.unclip();
    resp
}

fn toggle(st: &mut GridState, src: &mut dyn GridSource, resp: &mut GridResponse, gi: usize, all: bool, cols: usize) {
    src.toggle_group(gi, all);
    resp.header_toggled = true;
    if let Some(g) = src.groups() {
        st.geo.sync(g, cols, st.cell_h, st.header_h);
        st.scroll = st.scroll.clamp(0.0, st.max_scroll(g.row_count()));
    }
}
