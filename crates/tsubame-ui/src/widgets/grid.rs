//! Virtual grid: equal cells, wrapped to the width, scrolled vertically. The
//! thumbnail view. Like `list`, the kit never owns the entries: a
//! `GridSource` says how many cells there are, whether one is selected, and
//! draws one. Only cells that intersect the viewport are asked to draw.
//!
//! Selection belongs to the app (same `SelectBy` hooks as the list). The grid
//! adds 2D keyboard navigation (arrows, Home/End, PageUp/PageDown),
//! rubber-band selection with edge autoscroll, and per-cell hover.

use crate::canvas::Canvas;
use crate::geom::{Pt, Rect};
use crate::input::{Button, Event, Key, Mods};
use crate::theme::Theme;
use crate::ui::{Id, Sense, Ui};
use crate::widgets::grid_grouped::{GroupBand, GroupGeo, grid_grouped};
use crate::widgets::grouping::GroupRows;
use crate::widgets::list::{BandMode, HeaderState, RowState, SelectBy};
use crate::widgets::scrollbar::{ScrollMetrics, vscrollbar};

/// Pointer travel (DIPs) before a press on empty space becomes a marquee band.
pub const BAND_DEADZONE: f32 = 4.0;

/// Cells a rubber band covers: inclusive row and column ranges. A cell is
/// covered when the band touches its tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandCells {
    pub rows: (usize, usize),
    pub cols: (usize, usize),
}

impl BandCells {
    /// Indices of the covered cells that exist (`cols` per row, `len` cells).
    pub fn indices(&self, cols: usize, len: usize) -> impl Iterator<Item = usize> + use<> {
        let (rows, cs) = (self.rows, self.cols);
        (rows.0..=rows.1).flat_map(move |r| (cs.0..=cs.1).map(move |c| r * cols + c)).filter(move |&i| i < len)
    }
}

pub trait GridSource {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn is_selected(&self, _i: usize) -> bool {
        false
    }
    /// Draws cell `i` in `r` (selection / hover / focus already drawn).
    fn draw_cell(&mut self, c: &mut dyn Canvas, theme: &Theme, i: usize, r: Rect, st: RowState);
    /// The cursor moved to `i` (or a click landed on it): update the selection.
    fn select(&mut self, _i: usize, _mods: Mods, _by: SelectBy) {}
    /// A press on empty space that is not a left press (right press).
    fn select_none(&mut self) {}
    /// A left press on empty space started a rubber band.
    fn band_begin(&mut self, _mode: BandMode) {}
    /// Every frame the band is up and moved past the dead zone: the cells it
    /// covers (`None`: it covers none). The selection is base + these.
    fn band_update(&mut self, _cells: Option<BandCells>, _cols: usize) {}
    fn band_end(&mut self) {}
    /// Grouped cells, if the grid is grouped. Then `len()` counts items and every
    /// index the source is handed is an item (source) index.
    fn groups(&self) -> Option<&dyn GroupRows> {
        None
    }
    /// A click (or key) on the header of group `g`: toggle it (`all`: every group).
    fn toggle_group(&mut self, _g: usize, _all: bool) {}
    /// Draws the header row of group `g` in `r` (full width, background drawn).
    fn draw_header(&mut self, _c: &mut dyn Canvas, _theme: &Theme, _g: usize, _r: Rect, _st: HeaderState) {}
    /// Grouped `band_update`: the covered cells of every group (inside its own cell block).
    fn band_update_groups(&mut self, _spans: &[GroupBand], _cols: usize) {}
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Band {
    /// start in content coordinates (x from the cells' left edge, y from the top)
    pub(crate) sx: f32,
    pub(crate) sy: f64,
    pub(crate) started: bool,
}

/// Scroll, cursor and metrics of one grid. Plain data: the app keeps one per view.
#[derive(Clone, Debug, PartialEq)]
pub struct GridState {
    pub scroll: f64,
    /// keyboard cursor cell
    pub cursor: usize,
    /// cell size in DIPs (the app sets it from the view mode)
    pub cell_w: f32,
    pub cell_h: f32,
    /// cells area of the last frame
    pub view_w: f32,
    pub view_h: f32,
    /// columns of the last frame
    pub cols: usize,
    /// scroll the cursor into view on the next frame (set after the app moved it)
    pub reveal: bool,
    /// header row height of a grouped grid (DIPs)
    pub header_h: f32,
    pub(crate) band: Option<Band>,
    /// grouped: the header that has the keyboard focus, and the cursor item when it got it
    pub(crate) header: Option<(usize, usize)>,
    pub(crate) geo: GroupGeo,
    pub(crate) band_scratch: Vec<GroupBand>,
}

impl GridState {
    pub fn new(cell_w: f32, cell_h: f32) -> GridState {
        GridState { scroll: 0.0, cursor: 0, cell_w, cell_h, view_w: 0.0, view_h: 0.0, cols: 1, reveal: false, header_h: 24.0, band: None, header: None, geo: GroupGeo::default(), band_scratch: Vec::new() }
    }
    pub fn cols_for(&self, w: f32) -> usize {
        ((w / self.cell_w.max(1.0)) as usize).max(1)
    }
    pub fn rows_for(&self, len: usize) -> usize {
        len.div_ceil(self.cols.max(1))
    }
    pub fn content_h(&self, len: usize) -> f64 {
        if self.geo.active { self.geo.total() } else { self.rows_for(len) as f64 * self.cell_h as f64 }
    }
    /// The last frame laid the cells out in groups (`cell_origin` / `cell_at` do not apply).
    pub fn geo_active(&self) -> bool {
        self.geo.active
    }
    /// Group whose header has the keyboard focus (grouped grid).
    pub fn header_focus(&self) -> Option<usize> {
        self.header.filter(|&(_, at)| at == self.cursor).map(|(g, _)| g)
    }
    pub fn set_header_focus(&mut self, g: Option<usize>) {
        self.header = g.map(|g| (g, self.cursor));
    }
    pub fn max_scroll(&self, len: usize) -> f64 {
        (self.content_h(len) - self.view_h as f64).max(0.0)
    }
    /// Whole rows in a page (at least 1).
    pub fn page_rows(&self) -> usize {
        ((self.view_h / self.cell_h.max(1.0)) as usize).max(1)
    }
    pub fn set_scroll(&mut self, s: f64, len: usize) -> bool {
        let s = s.clamp(0.0, self.max_scroll(len));
        let changed = s != self.scroll;
        self.scroll = s;
        changed
    }
    /// Is a rubber band being dragged (past the dead zone)?
    pub fn banding(&self) -> bool {
        self.band.is_some_and(|b| b.started)
    }
    /// Is a left press on empty space tracking a potential or active rubber band?
    pub fn has_band(&self) -> bool {
        self.band.is_some()
    }
    /// Cancels any in-progress rubber band.
    pub fn cancel_band(&mut self) {
        self.band = None;
    }
    /// Changes the cell size (zoom) and keeps the top visible row on screen:
    /// the scroll is re-based on the same first cell at the new height.
    pub fn set_cell_size(&mut self, w: f32, h: f32) {
        if w == self.cell_w && h == self.cell_h {
            return;
        }
        let row = (self.scroll / self.cell_h.max(1.0) as f64).floor().max(0.0);
        let ratio = h as f64 / self.cell_h.max(1.0) as f64;
        self.cell_w = w;
        self.cell_h = h;
        // grouped: the same place in the content, to within a header
        self.scroll = if self.geo.active { self.scroll * ratio } else { row * h as f64 };
    }
    /// Index of the first cell on the top visible row.
    pub fn top_index(&self) -> usize {
        (self.scroll / self.cell_h.max(1.0) as f64).floor() as usize * self.cols.max(1)
    }
    /// (first, last+1) cell indices that intersect the viewport.
    pub fn visible(&self, len: usize) -> (usize, usize) {
        self.visible_with(len, 0)
    }
    /// The same widened by `margin_rows` whole rows each way (prefetch).
    pub fn visible_with(&self, len: usize, margin_rows: usize) -> (usize, usize) {
        let ch = self.cell_h.max(1.0) as f64;
        let first_row = ((self.scroll / ch).floor() as usize).saturating_sub(margin_rows);
        let last_row = (((self.scroll + self.view_h as f64) / ch).ceil() as usize + margin_rows).max(first_row);
        let cols = self.cols.max(1);
        let (a, b) = ((first_row * cols).min(len), (last_row * cols).min(len));
        (a, b)
    }
    /// Scrolls the least amount that shows cell `i`'s row whole.
    pub fn ensure_visible(&mut self, i: usize, len: usize) -> bool {
        let ch = self.cell_h as f64;
        let top = (i / self.cols.max(1)) as f64 * ch;
        let vh = self.view_h as f64;
        if top < self.scroll {
            self.set_scroll(top, len)
        } else if top + ch > self.scroll + vh {
            self.set_scroll(top + ch - vh, len)
        } else {
            false
        }
    }
    /// Moves the cursor (clamped) and scrolls it into view.
    pub fn move_cursor(&mut self, i: usize, len: usize) -> bool {
        if len == 0 {
            return false;
        }
        let i = i.min(len - 1);
        let moved = i != self.cursor;
        self.cursor = i;
        self.ensure_visible(i, len) || moved
    }
    /// The cell under `(x, y)` DIPs from the cells area's top-left corner,
    /// `None` on empty space (right of the last column, past the last cell).
    pub fn cell_at(&self, x: f32, y: f32, len: usize) -> Option<usize> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let col = (x / self.cell_w.max(1.0)) as usize;
        let row = ((self.scroll + y as f64) / self.cell_h.max(1.0) as f64).floor() as usize;
        let i = row.checked_mul(self.cols)?.checked_add(col)?;
        (col < self.cols && i < len).then_some(i)
    }
    /// Top-left of cell `i` relative to the cells area (may be off screen).
    pub fn cell_origin(&self, i: usize) -> (f32, f32) {
        let (row, col) = (i / self.cols.max(1), i % self.cols.max(1));
        (col as f32 * self.cell_w, (row as f64 * self.cell_h as f64 - self.scroll) as f32)
    }
}

/// Where a navigation key moves the cursor (`None`: not a navigation key).
pub fn nav_target(key: Key, cur: usize, cols: usize, len: usize, page_rows: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cols = cols.max(1);
    let last = len - 1;
    let last_row = last / cols;
    Some(match key {
        Key::Left => cur.saturating_sub(1),
        Key::Right => (cur + 1).min(last),
        Key::Up => {
            if cur >= cols {
                cur - cols
            } else {
                cur
            }
        }
        Key::Down => {
            if cur + cols <= last {
                cur + cols
            } else if cur / cols < last_row {
                last // the row below is the short last row: its last cell
            } else {
                cur
            }
        }
        Key::PageUp => {
            let step = page_rows * cols;
            if cur >= step { cur - step } else { cur % cols }
        }
        Key::PageDown => {
            let t = cur + page_rows * cols;
            if t <= last {
                t
            } else {
                ((last_row * cols + cur % cols).min(last)).max(cur)
            }
        }
        Key::Home => 0,
        Key::End => last,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridResponse {
    /// cells drawn: (first, last+1)
    pub range: (usize, usize),
    pub cols: usize,
    /// double click or Enter on this cell
    pub activated: Option<usize>,
    /// plain left double-click on empty space
    pub empty_double_click: bool,
    /// middle click released on this cell
    pub middle_clicked: Option<usize>,
    /// a group header was toggled (click or key)
    pub header_toggled: bool,
    /// a left or middle press landed inside the cells area this frame
    pub pressed: bool,
    /// right click released (cell under it, if any), at this point
    pub context: Option<(Option<usize>, Pt)>,
    /// the Menu key / Shift+F10 on the cursor cell
    pub context_key: bool,
    pub scrolled: bool,
    pub cursor_moved: bool,
    /// Ctrl + wheel notches over the grid this frame (+1 = away from the user);
    /// the grid does not scroll for them, the app zooms
    pub zoom: f32,
    /// cell currently under the pointer (not banding, within the grid)
    pub hovered: Option<usize>,
}

/// The grid in `rect`, with its scrollbar on the right.
pub fn grid(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, st: &mut GridState, src: &mut dyn GridSource) -> GridResponse {
    if src.groups().is_some() {
        return grid_grouped(ui, c, id, rect, st, src);
    }
    st.geo.active = false;
    let mut resp = GridResponse::default();
    let len = src.len();
    let mut area = rect;
    let track = area.cut_right(ui.theme.layout_scrollbar_width);
    let scroll0 = st.scroll;

    // reflow: keep the top visible cell on screen when the column count changes
    let cols = st.cols_for(area.w);
    if cols != st.cols {
        let top = st.top_index();
        st.cols = cols;
        st.scroll = (top / cols) as f64 * st.cell_h as f64;
    }
    st.view_w = area.w.max(0.0);
    st.view_h = area.h.max(0.0);
    st.scroll = st.scroll.clamp(0.0, st.max_scroll(len));
    if len > 0 && st.cursor >= len {
        st.cursor = len - 1;
    }
    if st.reveal {
        st.reveal = false;
        st.ensure_visible(st.cursor, len);
    }
    resp.cols = cols;
    let (cw, ch) = (st.cell_w, st.cell_h);

    let origin_y = area.y - st.scroll as f32;
    let sense = Sense::click().focusable().wheel().cells_2d(area.x, cw, origin_y, ch);
    let it = ui.interact(id, rect, sense);

    // wheel: a fixed step per notch, no smoothing
    if it.wheel.1 != 0.0 {
        if it.wheel_mods.ctrl {
            resp.zoom = it.wheel.1;
        } else {
            let step = (ui.theme.wheel_lines() * ui.theme.height_list_row).max(ch * 0.6) as f64;
            st.set_scroll(st.scroll - it.wheel.1 as f64 * step, len);
        }
    }

    // mouse
    if let Some(p) = it.pressed
        && area.contains_pt(p.pos)
    {
        if p.button != Button::Right {
            resp.pressed = true;
        }
        let cell = st.cell_at(p.pos.x - area.x, p.pos.y - area.y, len);
        match (cell, p.button) {
            (Some(i), Button::Left) => {
                st.band = None;
                resp.cursor_moved |= st.move_cursor(i, len);
                src.select(i, p.mods, SelectBy::Click);
                if p.double {
                    resp.activated = Some(i);
                }
            }
            (Some(i), Button::Right) => {
                st.band = None;
                st.cursor = i;
                src.select(i, p.mods, SelectBy::RightClick);
            }
            (None, Button::Left) => {
                if p.double && p.mods.none() {
                    st.band = None;
                    resp.empty_double_click = true;
                } else {
                    st.band = Some(Band { sx: p.pos.x - area.x, sy: (p.pos.y - area.y) as f64 + st.scroll, started: false });
                    src.band_begin(BandMode::from_mods(p.mods));
                }
            }
            (None, Button::Right) => {
                st.band = None;
                src.select_none();
            }
            (_, Button::Middle) => {}
        }
    }
    if it.clicked == Some(Button::Right) && area.contains_pt(it.click_pos) {
        let cell = st.cell_at(it.click_pos.x - area.x, it.click_pos.y - area.y, len);
        resp.context = Some((cell, it.click_pos));
    }
    if it.clicked == Some(Button::Middle) && area.contains_pt(it.click_pos) {
        resp.middle_clicked = st.cell_at(it.click_pos.x - area.x, it.click_pos.y - area.y, len);
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
        if let Some(t) = nav_target(key, st.cursor, cols, len, st.page_rows()) {
            ui.consume(qi);
            resp.cursor_moved |= st.move_cursor(t, len);
            src.select(st.cursor, mods, SelectBy::Key);
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

    // rubber band: edge autoscroll, then the covered cells
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
                // faster the further out, about 5 to 30 DIPs a frame
                let speed = (5.0 + past.abs() * 0.4).min(30.0) as f64;
                if st.set_scroll(st.scroll + speed * past.signum() as f64, len) {
                    ui.request_repaint();
                }
            }
            let max_y = st.content_h(len).max(st.scroll + st.view_h as f64).max(1.0);
            let mx = (m.x - area.x).clamp(0.0, st.view_w);
            let my = ((m.y - area.y) as f64 + st.scroll).clamp(0.0, max_y);
            if !b.started && (mx - b.sx).hypot((my - b.sy) as f32) >= BAND_DEADZONE {
                b.started = true;
                st.band = Some(b);
            }
            if b.started {
                let (x0, x1) = (b.sx.min(mx), b.sx.max(mx));
                let (y0, y1) = (b.sy.min(my), b.sy.max(my));
                let nrows = st.rows_for(len);
                let grid_w = cols as f32 * cw;
                let grid_h = nrows as f64 * ch as f64;
                let cells = if len > 0 && x0 < grid_w && x1 > 0.0 && y0 < grid_h && y1 > 0.0 {
                    let c0 = (x0 / cw).floor().max(0.0) as usize;
                    let c1 = (((x1 - 1e-4).max(x0)) / cw).floor().max(0.0) as usize;
                    let r0 = (y0 / ch as f64).floor().max(0.0) as usize;
                    let r1 = (((y1 - 1e-6).max(y0)) / ch as f64).floor().max(0.0) as usize;
                    (c0 < cols && r0 < nrows).then(|| BandCells { rows: (r0, r1.min(nrows - 1)), cols: (c0, c1.min(cols - 1)) })
                } else {
                    None
                };
                src.band_update(cells, cols);
                band_rect = Some(Rect::new(area.x + x0, area.y + (y0 - st.scroll) as f32, x1 - x0, (y1 - y0) as f32));
            }
        }
    }

    // scrollbar (registered after the grid: on top of it for clicks)
    let m = ScrollMetrics { content: st.content_h(len), view: st.view_h as f64, page: st.page_rows() as f64 * ch as f64 };
    vscrollbar(ui, c, id.child("sb"), track, m, &mut st.scroll);
    resp.scrolled = st.scroll != scroll0;

    // cells: only those that intersect the viewport
    let (first, last) = st.visible(len);
    resp.range = (first, last);
    let hot = (it.hovered && ui.active().is_none_or(|a| a == id) && area.contains_pt(ui.mouse()) && !st.banding())
        .then(|| st.cell_at(ui.mouse().x - area.x, ui.mouse().y - area.y, len))
        .flatten();
    resp.hovered = hot;
    let focused = it.focused && ui.window_focused();
    let theme = &ui.theme;
    c.clip(area);
    for i in first..last {
        let (ox, oy) = st.cell_origin(i);
        let r = Rect::new(area.x + ox, area.y + oy, cw, ch);
        let tile = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0);
        let rs = RowState { selected: src.is_selected(i), cursor: i == st.cursor, hot: hot == Some(i), focused };
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
    if let Some(br) = band_rect {
        c.fill_a(br, theme.accent, 0.18);
        c.round_stroke(br, 0.0, theme.accent, 1.0);
    }
    c.unclip();
    resp
}
