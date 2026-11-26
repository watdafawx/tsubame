//! Virtual list: fixed row height, any number of rows. The kit never owns
//! the entries: a `ListSource` says how many rows there are, whether one is
//! selected, and draws one row. Only rows that intersect the viewport are
//! asked to draw.
//!
//! Selection belongs to the app: clicks and cursor keys call
//! `ListSource::select` with the modifiers (Ctrl/Shift rules live in the
//! selection model), before the rows are drawn, so a frame shows its own
//! result.

use crate::canvas::Canvas;
use crate::geom::{Pt, Rect};
use crate::input::{Button, Event, Key, Mods};
use crate::theme::Theme;
use crate::ui::{Id, Sense, Ui};
use crate::widgets::grid::BAND_DEADZONE;
use crate::widgets::grouping::{GroupRows, Row};
use crate::widgets::scrollbar::{ScrollMetrics, vscrollbar};

/// How a marquee / rubber-band selection combines with the pre-drag selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BandMode {
    /// Plain drag: replaces the selection.
    #[default]
    Replace,
    /// Shift drag: adds covered items to the pre-drag selection.
    Add,
    /// Ctrl drag: toggles covered items against the pre-drag selection.
    Toggle,
}

impl BandMode {
    pub fn from_mods(mods: Mods) -> BandMode {
        if mods.ctrl && !mods.shift {
            BandMode::Toggle
        } else if mods.shift {
            BandMode::Add
        } else {
            BandMode::Replace
        }
    }
}

/// How a row is drawn this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowState {
    pub selected: bool,
    /// the keyboard cursor (focus row)
    pub cursor: bool,
    pub hot: bool,
    /// the list has the keyboard focus
    pub focused: bool,
}

/// Why `ListSource::select` is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectBy {
    /// left press on the row
    Click,
    /// right press on the row (Explorer selects it first if it was not)
    RightClick,
    /// the cursor moved by a key (arrows, PageUp/Down, Home/End)
    Key,
    /// Space on the cursor row (Ctrl+Space toggles in Explorer)
    Space,
}

pub trait ListSource {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn is_selected(&self, _i: usize) -> bool {
        false
    }
    /// Draws row `i` in `r` (background already drawn by the list).
    fn draw_row(&mut self, c: &mut dyn Canvas, theme: &Theme, i: usize, r: Rect, st: RowState);
    /// The cursor moved to `i` (or a click landed on it): update the selection.
    fn select(&mut self, _i: usize, _mods: Mods, _by: SelectBy) {}
    /// A press on empty space below the last row (non-left press, e.g. right click).
    fn select_none(&mut self) {}
    /// A left press on empty space started a marquee band.
    fn band_begin(&mut self, _mode: BandMode) {}
    /// Every frame the band is active past the dead zone: inclusive row range
    /// covered (`None`: covers no rows).
    fn band_update(&mut self, _rows: Option<(usize, usize)>) {}
    fn band_end(&mut self) {}
    /// Grouped rows, if the list is grouped. Then `len()` counts items, every
    /// index the source is handed is an item (source) index, and only the
    /// band range (`band_update`) and `ListResponse::range` are flat rows.
    fn groups(&self) -> Option<&dyn GroupRows> {
        None
    }
    /// A click (or key) on the header of group `g`: toggle it (`all`: every group).
    fn toggle_group(&mut self, _g: usize, _all: bool) {}
    /// Draws the header row of group `g` in `r` (background already drawn).
    fn draw_header(&mut self, _c: &mut dyn Canvas, _theme: &Theme, _g: usize, _r: Rect, _st: HeaderState) {}
}

/// How a group header is drawn this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeaderState {
    pub collapsed: bool,
    pub hot: bool,
    /// the header has the keyboard focus (and the list has it too)
    pub cursor: bool,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ListBand {
    /// start in content coordinates (x from the rows' left edge, y from the top)
    sx: f32,
    sy: f64,
    started: bool,
}

/// Scroll and cursor of one list. Plain data: the app keeps one per view.
#[derive(Clone, Debug, PartialEq)]
pub struct ListState {
    /// content offset in DIPs (`f64`, exact for any row count we can hold)
    pub scroll: f64,
    /// keyboard cursor row
    pub cursor: usize,
    pub row_h: f32,
    /// rows area height of the last frame (page size for keys)
    pub view_h: f32,
    band: Option<ListBand>,
    /// grouped list: the header that has the keyboard focus, and the cursor
    /// item when it got it (a moved cursor drops the focus)
    header: Option<(usize, usize)>,
}

impl ListState {
    pub fn new(row_h: f32) -> ListState {
        ListState { scroll: 0.0, cursor: 0, row_h, view_h: 0.0, band: None, header: None }
    }
    /// Group whose header has the keyboard focus.
    pub fn header_focus(&self) -> Option<usize> {
        self.header.filter(|&(_, at)| at == self.cursor).map(|(g, _)| g)
    }
    pub fn set_header_focus(&mut self, g: Option<usize>) {
        self.header = g.map(|g| (g, self.cursor));
    }
    /// Moves the cursor to `item`, shown on flat row `row` of `n`, and scrolls it into view.
    pub fn cursor_to(&mut self, item: usize, row: usize, n: usize) -> bool {
        let moved = item != self.cursor;
        self.cursor = item;
        self.ensure_visible(row, n) || moved
    }
    /// Is a marquee band being dragged (past the dead zone)?
    pub fn banding(&self) -> bool {
        self.band.is_some_and(|b| b.started)
    }
    /// Is a left press on empty space tracking a potential or active marquee band?
    pub fn has_band(&self) -> bool {
        self.band.is_some()
    }
    /// Cancels any in-progress marquee band.
    pub fn cancel_band(&mut self) {
        self.band = None;
    }
    pub fn content_h(&self, len: usize) -> f64 {
        len as f64 * self.row_h as f64
    }
    pub fn max_scroll(&self, len: usize) -> f64 {
        (self.content_h(len) - self.view_h as f64).max(0.0)
    }
    /// Rows a PageUp/PageDown moves (a full page minus one).
    pub fn page_rows(&self) -> usize {
        ((self.view_h / self.row_h) as usize).saturating_sub(1).max(1)
    }
    pub fn set_scroll(&mut self, s: f64, len: usize) -> bool {
        let s = s.clamp(0.0, self.max_scroll(len));
        let changed = s != self.scroll;
        self.scroll = s;
        changed
    }
    /// Row at `dy` DIPs below the top of the rows area (unclamped).
    pub fn row_at_offset(&self, dy: f32) -> usize {
        ((self.scroll + dy as f64) / self.row_h as f64).floor().max(0.0) as usize
    }
    /// Top of row `i` relative to the rows area (may be off screen).
    pub fn row_top(&self, i: usize) -> f64 {
        i as f64 * self.row_h as f64 - self.scroll
    }
    /// (first, last+1) rows that intersect the viewport.
    pub fn visible(&self, len: usize) -> (usize, usize) {
        let rh = self.row_h as f64;
        let first = (self.scroll / rh).floor() as usize;
        let last = (((self.scroll + self.view_h as f64) / rh).ceil() as usize).min(len);
        (first.min(last), last)
    }
    /// Scrolls the least amount that shows row `i` whole.
    pub fn ensure_visible(&mut self, i: usize, len: usize) -> bool {
        let top = i as f64 * self.row_h as f64;
        let bottom = top + self.row_h as f64;
        let vh = self.view_h as f64;
        if top < self.scroll {
            self.set_scroll(top, len)
        } else if bottom > self.scroll + vh {
            self.set_scroll(bottom - vh, len)
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
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ListResponse {
    /// rows drawn: (first, last+1)
    pub range: (usize, usize),
    /// double click or Enter on this row
    pub activated: Option<usize>,
    /// plain left double-click on empty space below the last row
    pub empty_double_click: bool,
    /// middle click released on this row
    pub middle_clicked: Option<usize>,
    /// a left or middle press landed inside the rows area this frame
    pub pressed: bool,
    /// right click released (row under it, if any), at this point
    pub context: Option<(Option<usize>, Pt)>,
    /// the Menu key / Shift+F10 on the cursor row (the app opens the menu)
    pub context_key: bool,
    pub scrolled: bool,
    pub cursor_moved: bool,
    /// Ctrl + wheel notches over the list this frame (+1 = away from the user);
    /// the list does not scroll for them, the app zooms
    pub zoom: f32,
    /// row currently under the pointer (not banding, within the list)
    pub hovered: Option<usize>,
    /// a group header was toggled (click or key): the app rebuilds what it caches
    pub header_toggled: bool,
}

/// Flat row and item of the nearest selectable row from `row` (in `forward` direction first).
fn nav_target(g: &dyn GroupRows, row: usize, forward: bool) -> Option<(usize, usize)> {
    let r = g.nav_row(row, forward).or_else(|| g.nav_row(row, !forward))?;
    match g.row_at(r) {
        Row::Item(s) => Some((r, s)),
        Row::Header(_) => None,
    }
}

/// Arrow / page / Home / End on a grouped list: moves over item rows, headers skipped.
fn grouped_move(st: &mut ListState, src: &mut dyn ListSource, key: Key, mods: Mods, resp: &mut ListResponse) {
    let page = st.page_rows();
    let (n, found) = {
        let Some(g) = src.groups() else { return };
        let n = g.row_count();
        let cur = match st.header_focus() {
            Some(h) => g.header_row(h),
            None => g.item_row(st.cursor).or_else(|| g.item_slot(st.cursor).map(|(h, _)| g.header_row(h))).unwrap_or(0),
        };
        let (target, forward) = match key {
            Key::Up => (cur.saturating_sub(1), false),
            Key::Down => (cur + 1, true),
            Key::PageUp => (cur.saturating_sub(page), false),
            Key::PageDown => (cur + page, true),
            Key::Home => (0, true),
            _ => (n.saturating_sub(1), false),
        };
        let found = nav_target(g, target, forward).map(|(row, item)| {
            let header_above = row > 0 && matches!(g.row_at(row - 1), Row::Header(_));
            (row, item, header_above)
        });
        (n, found)
    };
    let Some((row, item, header_above)) = found else { return };
    st.header = None;
    resp.cursor_moved |= st.cursor_to(item, row, n);
    // the header above the first item of a group comes into view with it
    if header_above && matches!(key, Key::Up | Key::PageUp | Key::Home) {
        st.ensure_visible(row - 1, n);
    }
    src.select(item, mods, SelectBy::Key);
}

/// Toggles group `g`, returns the new flat row count.
fn toggle(src: &mut dyn ListSource, resp: &mut ListResponse, g: usize, all: bool) -> usize {
    src.toggle_group(g, all);
    resp.header_toggled = true;
    src.groups().map_or(0, |g| g.row_count())
}

/// The list in `rect`, with its scrollbar on the right.
pub fn list(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, st: &mut ListState, src: &mut dyn ListSource) -> ListResponse {
    let mut resp = ListResponse::default();
    let len = src.len();
    // flat rows: items alone, or headers and shown items when grouped
    let grouped = src.groups().is_some();
    let mut n = src.groups().map_or(len, |g| g.row_count());
    let mut rows = rect;
    let track = rows.cut_right(ui.theme.layout_scrollbar_width);
    st.view_h = rows.h.max(0.0);
    let rh = st.row_h;
    let scroll0 = st.scroll;
    st.scroll = st.scroll.clamp(0.0, st.max_scroll(n));
    if len > 0 && st.cursor >= len {
        st.cursor = len - 1;
    }

    // whole rect: wheel over the scrollbar scrolls the list too
    let sense = Sense::click().focusable().wheel().cells(rows.y - (st.scroll % rh as f64) as f32, rh);
    let it = ui.interact(id, rect, sense);

    // wheel: whole line steps, no smoothing
    if it.wheel.1 != 0.0 {
        if it.wheel_mods.ctrl {
            resp.zoom = it.wheel.1;
        } else {
            let step = it.wheel.1 as f64 * ui.theme.wheel_lines() as f64 * rh as f64;
            st.set_scroll(st.scroll - step, n);
        }
    }

    // mouse on the rows
    if let Some(p) = it.pressed
        && rows.contains_pt(p.pos)
    {
        if p.button != Button::Right {
            resp.pressed = true;
        }
        let row = st.row_at_offset(p.pos.y - rows.y);
        let at = if row < n { Some(src.groups().map_or(Row::Item(row), |g| g.row_at(row))) } else { None };
        match at {
            Some(Row::Header(g)) => {
                if p.button == Button::Left {
                    st.band = None;
                    st.set_header_focus(Some(g));
                    // the second press of a double click would undo the first
                    if !p.double {
                        n = toggle(src, &mut resp, g, p.mods.ctrl);
                    }
                } else if p.button == Button::Right {
                    st.band = None;
                }
            }
            Some(Row::Item(item)) => match p.button {
                Button::Left => {
                    st.band = None;
                    st.header = None;
                    resp.cursor_moved |= st.cursor_to(item, row, n);
                    src.select(item, p.mods, SelectBy::Click);
                    if p.double {
                        resp.activated = Some(item);
                    }
                }
                Button::Right => {
                    st.band = None;
                    st.header = None;
                    st.cursor = item;
                    src.select(item, p.mods, SelectBy::RightClick);
                }
                Button::Middle => {}
            },
            None => match p.button {
                Button::Left => {
                    if p.double && p.mods.none() {
                        st.band = None;
                        resp.empty_double_click = true;
                    } else {
                        st.band = Some(ListBand { sx: p.pos.x - rows.x, sy: (p.pos.y - rows.y) as f64 + st.scroll, started: false });
                        src.band_begin(BandMode::from_mods(p.mods));
                    }
                }
                Button::Right => {
                    st.band = None;
                    src.select_none();
                }
                Button::Middle => {}
            },
        }
    }
    if it.clicked == Some(Button::Right) && rows.contains_pt(it.click_pos) {
        let row = st.row_at_offset(it.click_pos.y - rows.y);
        let item = if row < n {
            match src.groups().map_or(Row::Item(row), |g| g.row_at(row)) {
                Row::Item(i) => Some(i),
                Row::Header(_) => None,
            }
        } else {
            None
        };
        resp.context = Some((item, it.click_pos));
    }
    if it.clicked == Some(Button::Middle) && rows.contains_pt(it.click_pos) {
        let row = st.row_at_offset(it.click_pos.y - rows.y);
        if row < n
            && let Row::Item(i) = src.groups().map_or(Row::Item(row), |g| g.row_at(row))
        {
            resp.middle_clicked = Some(i);
        }
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
        if grouped {
            match key {
                Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End => {
                    ui.consume(qi);
                    if len > 0 {
                        grouped_move(st, src, key, mods, &mut resp);
                    }
                    continue;
                }
                Key::Left | Key::Right if mods.none() && len > 0 => {
                    if let Some(h) = st.header_focus() {
                        ui.consume(qi);
                        let collapsed = src.groups().is_some_and(|g| g.is_collapsed(h));
                        if collapsed == (key == Key::Right) {
                            n = toggle(src, &mut resp, h, false);
                        }
                    } else if key == Key::Left
                        && let Some((slot_g, _)) = src.groups().and_then(|g| g.item_slot(st.cursor))
                    {
                        // keyboard way onto the header of the group of the cursor
                        ui.consume(qi);
                        st.set_header_focus(Some(slot_g));
                        let row = src.groups().map_or(0, |g| g.header_row(slot_g));
                        st.ensure_visible(row, n);
                        resp.cursor_moved = true;
                    }
                    continue;
                }
                Key::Enter | Key::Space if mods.none() && st.header_focus().is_some() => {
                    ui.consume(qi);
                    if let Some(h) = st.header_focus() {
                        n = toggle(src, &mut resp, h, false);
                    }
                    continue;
                }
                _ => {}
            }
        }
        let page = st.page_rows();
        let cur = st.cursor;
        let target = match key {
            Key::Up => Some(cur.saturating_sub(1)),
            Key::Down => Some(cur + 1),
            Key::PageUp => Some(cur.saturating_sub(page)),
            Key::PageDown => Some(cur + page),
            Key::Home => Some(0),
            Key::End => Some(len.saturating_sub(1)),
            _ => None,
        };
        if let Some(t) = target {
            ui.consume(qi);
            if len > 0 {
                resp.cursor_moved |= st.move_cursor(t, len);
                src.select(st.cursor, mods, SelectBy::Key);
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

    // marquee rubber band: edge autoscroll, then covered rows
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
            let past = if m.y < rows.y { m.y - rows.y } else if m.y > rows.b() { m.y - rows.b() } else { 0.0 };
            if past != 0.0 && b.started {
                let speed = (5.0 + past.abs() * 0.4).min(30.0) as f64;
                if st.set_scroll(st.scroll + speed * past.signum() as f64, n) {
                    ui.request_repaint();
                }
            }
            let max_y = st.content_h(n).max(st.scroll + st.view_h as f64).max(1.0);
            let mx = (m.x - rows.x).clamp(0.0, rows.w);
            let my = ((m.y - rows.y) as f64 + st.scroll).clamp(0.0, max_y);
            if !b.started && (mx - b.sx).hypot((my - b.sy) as f32) >= BAND_DEADZONE {
                b.started = true;
                st.band = Some(b);
            }
            if b.started {
                let (x0, x1) = (b.sx.min(mx), b.sx.max(mx));
                let (y0, y1) = (b.sy.min(my), b.sy.max(my));
                let content_h = st.content_h(n);
                let covered = if n > 0 && y0 < content_h && y1 > 0.0 {
                    let r0 = (y0 / rh as f64).floor().max(0.0) as usize;
                    let r1 = (((y1 - 1e-6).max(y0)) / rh as f64).floor().max(0.0) as usize;
                    (r0 < n).then(|| (r0, r1.min(n - 1)))
                } else {
                    None
                };
                src.band_update(covered);
                band_rect = Some(Rect::new(rows.x + x0, rows.y + (y0 - st.scroll) as f32, x1 - x0, (y1 - y0) as f32));
            }
        }
    }

    // scrollbar (registered after the list: on top of it for clicks)
    st.scroll = st.scroll.clamp(0.0, st.max_scroll(n));
    let m = ScrollMetrics { content: st.content_h(n), view: st.view_h as f64, page: (st.page_rows() as f64) * rh as f64 };
    vscrollbar(ui, c, id.child("sb"), track, m, &mut st.scroll);
    resp.scrolled = st.scroll != scroll0;

    // rows: only those that intersect the viewport
    let (first, last) = st.visible(n);
    resp.range = (first, last);
    let off = (st.scroll - first as f64 * rh as f64) as f32;
    let hot_row = (it.hovered && ui.active().is_none_or(|a| a == id) && rows.contains_pt(ui.mouse()) && !st.banding()).then(|| st.row_at_offset(ui.mouse().y - rows.y));
    let focused = it.focused && ui.window_focused();
    let theme = &ui.theme;
    let header_cur = st.header_focus();
    resp.hovered = hot_row.filter(|&r| r < n).and_then(|r| match src.groups().map_or(Row::Item(r), |g| g.row_at(r)) {
        Row::Item(i) => Some(i),
        Row::Header(_) => None,
    });
    c.clip(rows);
    for i in first..last {
        let y = rows.y + (i - first) as f32 * rh - off;
        let r = Rect::new(rows.x, y, rows.w, rh);
        let hot = hot_row == Some(i);
        let item = match src.groups().map_or(Row::Item(i), |g| g.row_at(i)) {
            Row::Item(item) => item,
            Row::Header(g) => {
                let collapsed = src.groups().is_some_and(|gr| gr.is_collapsed(g));
                let cursor = header_cur == Some(g);
                if hot {
                    c.fill(r, theme.hover);
                }
                if cursor && focused {
                    c.round_stroke(r, 2.0, theme.focus, 1.0);
                }
                src.draw_header(c, theme, g, r, HeaderState { collapsed, hot, cursor, focused });
                continue;
            }
        };
        let rs = RowState { selected: src.is_selected(item), cursor: item == st.cursor && header_cur.is_none(), hot, focused };
        if rs.selected {
            c.fill(r, if focused { theme.selected } else { theme.selected_inactive });
        } else if rs.hot {
            c.fill(r, theme.hover);
        }
        if rs.cursor && focused {
            c.round_stroke(r, 2.0, theme.focus, 1.0);
        }
        src.draw_row(c, theme, item, r, rs);
    }
    if let Some(br) = band_rect {
        c.fill_a(br, theme.accent, 0.18);
        c.round_stroke(br, 0.0, theme.accent, 1.0);
    }
    c.unclip();
    resp
}
