//! The own-drawn context menu (`docs/ctxmenu.md`): a stack of popups, one per
//! open level, drawn after every other widget.
//!
//! The app hands rows in through [`MenuSource`] each frame (so rows that
//! arrive late from the shell just show up) and keeps a [`MenuState`] while
//! the menu is open. Rows are identified by a key, not an index: focus and
//! the open submenu stay on the same row when rows are inserted.
//!
//! Hooks for later cards (see the doc): accelerators and type-to-filter get
//! the `Char` events the menu swallows now (CM4), submenus open on hover at
//! once (`MenuStyle::hover_delay`, CM4 safe triangle), the wheel is
//! swallowed (CM4), a submenu with no rows yet shows "Loading…" (CM6 fills
//! it lazily).

use crate::canvas::{Canvas, Font, f};
use crate::geom::{Pt, Rect};
use crate::input::{Button, Event, Key};
use crate::theme::Theme;
use crate::ui::{Id, Sense, Ui};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowKind {
    #[default]
    Item,
    Separator,
    Header,
    /// "Some items could not load" + Retry (picking it sets `retry`)
    Status,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum RowIcon {
    #[default]
    None,
    /// a bitmap from [`MenuSource::icon`]
    Image(u64),
    /// a glyph of the icon font
    Glyph(char),
}

/// One row as the menu draws it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuRow {
    /// stable across frames (focus and submenus follow it)
    pub key: u64,
    pub kind: RowKind,
    pub label: String,
    /// byte index of the accelerator char in `label`
    pub accel: Option<usize>,
    /// right-aligned key text ("Ctrl+C")
    pub shortcut: String,
    pub icon: RowIcon,
    pub disabled: bool,
    /// the default verb: semibold
    pub default: bool,
    /// destructive (Delete): danger colour
    pub danger: bool,
    /// a user-added entry: accent dot in the left gutter
    pub custom: bool,
    pub checked: bool,
    pub radio: bool,
    /// opens a submenu (its rows come from `MenuSource::rows` with this key
    /// appended to the path)
    pub submenu: bool,
}

impl MenuRow {
    pub fn item(key: u64, label: &str) -> MenuRow {
        MenuRow { key, label: label.into(), ..Default::default() }
    }
    pub fn separator() -> MenuRow {
        MenuRow { kind: RowKind::Separator, ..Default::default() }
    }
    pub fn status(key: u64, label: &str) -> MenuRow {
        MenuRow { key, kind: RowKind::Status, label: label.into(), ..Default::default() }
    }
    /// Keyboard focus and hover can land on it.
    pub fn focusable(&self) -> bool {
        match self.kind {
            RowKind::Item => !self.disabled,
            RowKind::Status => true,
            RowKind::Separator | RowKind::Header => false,
        }
    }
}

/// Where the rows come from.
pub trait MenuSource {
    /// The rows of the menu at `path` (row keys from the top level down;
    /// empty = the top level).
    fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>);
    /// Pixels of an image icon: (w, h, premultiplied BGRA).
    fn icon(&self, key: u64) -> Option<(u32, u32, &[u32])>;
}

/// Per-frame inputs that are not rows.
#[derive(Clone, Copy, Debug)]
pub struct MenuStyle {
    /// where the menu may go (the window's client area)
    pub bounds: Rect,
    /// the loading line's phase (0..1) while shell rows are still coming;
    /// None: no line
    pub loading: Option<f32>,
}

/// One open level.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Level {
    /// key of the row in the level above that opened it (0 for the top)
    pub key: u64,
    pub focus: Option<u64>,
    /// the focus came from the keyboard: ring, and it is kept visible
    pub kb: bool,
    pub scroll: f32,
    /// last frame's rect
    pub rect: Rect,
    /// the open child's row key
    pub open: Option<u64>,
    /// the parent row's rect (submenus are placed against it)
    pub parent_row: Rect,
    reveal: bool,
    focus_first: bool,
}

/// The app keeps this while the menu is open.
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    /// top-left corner the menu opens at (it flips left / up from here)
    pub anchor: Pt,
    /// opened from the keyboard (Shift+F10, Menu key): accelerators are
    /// underlined and the first row has the focus
    pub keyboard: bool,
    pub levels: Vec<Level>,
    opened: bool,
    pointer: Option<Pt>,
}

impl MenuState {
    pub fn new(anchor: Pt, keyboard: bool) -> MenuState {
        MenuState { anchor, keyboard, levels: Vec::new(), opened: false, pointer: None }
    }
}

/// What happened this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuResponse {
    /// a row was picked: the keys from the top level to it (the menu closes)
    pub picked: Option<Vec<u64>>,
    /// the menu closed (picked, Esc, a click outside, focus lost)
    pub closed: bool,
    /// Retry on the failure row
    pub retry: bool,
}

// ---- metrics -------------------------------------------------------------

/// Sizes from the theme (density and font scale applied).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// font scale over the density's base size (1.25 at 125 %)
    pub sc: f32,
    pub row: f32,
    pub sep: f32,
    /// menu padding inside the 1 px border
    pub pad: f32,
    pub font: Font,
    pub small: Font,
    pub icon: f32,
    pub gap: f32,
    pub strip: f32,
    pub min_w: f32,
    pub max_w: f32,
    pub radius: f32,
    pub row_radius: f32,
    /// distance kept from the bounds' edges
    pub keep: f32,
}

impl Metrics {
    pub fn new(t: &Theme) -> Metrics {
        let dens = t.height_list_row;
        // the density's base text size (12 / 13 / 14) gives the font scale
        let base = if dens < 24.0 { 12.0 } else if dens < 28.0 { 13.0 } else { 14.0 };
        // snapped to eighths: the rounded text sizes do not give it back exactly
        let sc = ((t.font_size_sm / base) * 8.0).round().max(4.0) / 8.0;
        let row = dens.max((t.font_size_sm * 1.75).round());
        let sep = if dens < 24.0 { 8.0 } else { 9.0 } * sc;
        Metrics {
            sc,
            row,
            sep: sep.round(),
            pad: 4.0,
            font: f(t.font_size_sm),
            small: f(t.font_size_xs),
            icon: (16.0 * sc).round(),
            gap: (8.0 * sc).round(),
            strip: (14.0 * sc).round(),
            min_w: (232.0 * sc).round(),
            max_w: (360.0 * sc).round(),
            radius: t.radius_lg,
            row_radius: t.radius_md,
            keep: 8.0,
        }
    }
    fn row_h(&self, r: &MenuRow) -> f32 {
        if r.kind == RowKind::Separator { self.sep } else { self.row }
    }
    /// Border + padding on each side.
    fn inset(&self) -> f32 {
        1.0 + self.pad
    }
    fn label_x(&self) -> f32 {
        6.0 + self.icon + self.gap
    }
    fn arrow_w(&self) -> f32 {
        (12.0 * self.sc).round()
    }
    fn retry_w(&self, c: &mut dyn Canvas) -> f32 {
        c.measure(RETRY, self.small) + 14.0 * self.sc
    }
}

const RETRY: &str = "Retry";
const LOADING: &str = "Loading\u{2026}";
const G_CHECK: char = '\u{E73E}';
const G_ARROW: char = '\u{E76C}';
const G_UP: char = '\u{E70E}';
const G_DOWN: char = '\u{E70D}';
const G_WARN: char = '\u{E7BA}';

/// Y offset of each row from the content top; returns the content height.
pub fn layout(rows: &[MenuRow], m: &Metrics, out: &mut Vec<f32>) -> f32 {
    out.clear();
    let mut y = 0.0;
    for r in rows {
        out.push(y);
        y += m.row_h(r);
    }
    y
}

/// Menu width for `rows`: widest label + widest shortcut + the columns,
/// clamped to the mock's 232..360 (scaled).
pub fn width(c: &mut dyn Canvas, rows: &[MenuRow], m: &Metrics) -> f32 {
    let (mut lw, mut sw, mut sub, mut st) = (0.0f32, 0.0f32, false, 0.0f32);
    for r in rows {
        match r.kind {
            RowKind::Separator | RowKind::Header => {}
            RowKind::Item => {
                let font = if r.default { m.font.b() } else { m.font };
                lw = lw.max(c.measure(&r.label, font));
                if !r.shortcut.is_empty() {
                    sw = sw.max(c.measure(&r.shortcut, m.small));
                }
                sub |= r.submenu;
            }
            RowKind::Status => st = st.max(c.measure(&r.label, m.font) + m.gap + m.retry_w(c)),
        }
    }
    let mut w = m.label_x() + lw.max(st) + 8.0;
    if sw > 0.0 {
        w += 2.0 * m.gap + sw;
    }
    if sub {
        w += m.gap + m.arrow_w();
    }
    (w + 2.0 * m.inset()).clamp(m.min_w, m.max_w)
}

/// The top level's rect: at `anchor`, flipped left / up when it would come
/// closer than `keep` to the bounds' edge; clamped (and shorter, so it
/// scrolls) when it fits neither way.
pub fn place(anchor: Pt, w: f32, h: f32, bounds: Rect, keep: f32) -> Rect {
    let (l, r, t, b) = (bounds.x + keep, bounds.r() - keep, bounds.y + keep, bounds.b() - keep);
    let w = w.min((r - l).max(0.0));
    let h = h.min((b - t).max(0.0));
    let x = if anchor.x + w <= r {
        anchor.x.max(l)
    } else if anchor.x - w >= l {
        anchor.x - w
    } else {
        (r - w).max(l)
    };
    let y = if anchor.y + h <= b {
        anchor.y.max(t)
    } else if anchor.y - h >= t {
        anchor.y - h
    } else {
        (b - h).max(t)
    };
    Rect::new(x, y, w, h)
}

/// A submenu's rect next to its parent row: right of it with 2 px overlap
/// into the parent's padding and the first row level with the parent row,
/// flipped to the left side when it does not fit.
pub fn place_sub(row: Rect, w: f32, h: f32, bounds: Rect, keep: f32) -> Rect {
    let (l, r, t, b) = (bounds.x + keep, bounds.r() - keep, bounds.y + keep, bounds.b() - keep);
    let w = w.min((r - l).max(0.0));
    let h = h.min((b - t).max(0.0));
    let mut x = row.r() + 2.0;
    if x + w > r {
        x = row.x - 2.0 - w;
        if x < l {
            x = (r - w).max(l);
        }
    }
    let y = (row.y - 5.0).min(b - h).max(t);
    Rect::new(x, y, w, h)
}

pub fn clamp_scroll(scroll: f32, content: f32, view: f32) -> f32 {
    scroll.clamp(0.0, (content - view).max(0.0))
}

/// The scroll that shows the row at `y..y+h` in a `view` tall window.
pub fn scroll_to_show(scroll: f32, y: f32, h: f32, view: f32) -> f32 {
    if y < scroll {
        y
    } else if y + h > scroll + view {
        y + h - view
    } else {
        scroll
    }
}

/// The next focusable row after `from` (before it when `!down`), wrapping;
/// from None the first (last) one.
pub fn next_focus(rows: &[MenuRow], from: Option<usize>, down: bool) -> Option<usize> {
    let n = rows.len();
    if n == 0 {
        return None;
    }
    let start = match from {
        Some(i) => i.min(n - 1),
        None if down => n - 1,
        None => 0,
    };
    (1..=n).map(|k| if down { (start + k) % n } else { (start + n - k % n) % n }).find(|&i| rows[i].focusable())
}

/// The first (or last) focusable row.
pub fn edge_focus(rows: &[MenuRow], first: bool) -> Option<usize> {
    if first { rows.iter().position(MenuRow::focusable) } else { rows.iter().rposition(MenuRow::focusable) }
}

// ---- the widget ----------------------------------------------------------

enum Act {
    Pick(Vec<u64>),
    Retry,
    Open(usize, u64, bool),
}

fn rows_at(src: &dyn MenuSource, st: &MenuState, lv: usize, out: &mut Vec<MenuRow>) {
    let path: Vec<u64> = st.levels[1..=lv].iter().map(|l| l.key).collect();
    out.clear();
    src.rows(&path, out);
    if lv > 0 && out.is_empty() {
        // CM6: a lazy submenu that has not been filled yet
        out.push(MenuRow { disabled: true, ..MenuRow::item(0, LOADING) });
    }
}

fn index_of(rows: &[MenuRow], key: Option<u64>) -> Option<usize> {
    key.and_then(|k| rows.iter().position(|r| r.key == k && r.kind != RowKind::Separator))
}

/// What activating row `i` of level `lv` does.
fn activate(st: &MenuState, rows: &[MenuRow], lv: usize, i: usize, kb: bool) -> Option<Act> {
    let r = rows.get(i)?;
    match r.kind {
        RowKind::Separator | RowKind::Header => None,
        RowKind::Status => Some(Act::Retry),
        RowKind::Item if r.disabled => None,
        RowKind::Item if r.submenu => Some(Act::Open(lv, r.key, kb)),
        RowKind::Item => {
            let mut path: Vec<u64> = st.levels[1..=lv].iter().map(|l| l.key).collect();
            path.push(r.key);
            Some(Act::Pick(path))
        }
    }
}

fn close_above(ui: &mut Ui, id: Id, st: &mut MenuState, lv: usize) {
    if st.levels.len() > lv + 1 {
        ui.close_popup(id.with(lv as u64 + 1));
        st.levels.truncate(lv + 1);
    }
    st.levels[lv].open = None;
}

fn open_child(ui: &mut Ui, id: Id, st: &mut MenuState, lv: usize, key: u64, kb: bool) {
    if st.levels[lv].open == Some(key) && st.levels.len() > lv + 1 {
        if kb {
            st.levels[lv + 1].focus_first = true;
            st.levels[lv + 1].kb = true;
        }
        return;
    }
    close_above(ui, id, st, lv);
    st.levels[lv].open = Some(key);
    st.levels.push(Level { key, kb, focus_first: kb, ..Default::default() });
    ui.open_popup(id.with(lv as u64 + 1));
}

/// The menu, drawn on top of everything (call it after all other widgets).
/// While it is open it takes every key (the app's shortcuts and the list
/// do not see them) and keeps the keyboard focus on itself.
pub fn menu(ui: &mut Ui, c: &mut dyn Canvas, id: Id, st: &mut MenuState, src: &dyn MenuSource, style: &MenuStyle) -> MenuResponse {
    let mut resp = MenuResponse::default();
    let lid = |i: usize| id.with(i as u64);
    if !st.opened {
        st.opened = true;
        st.levels = vec![Level { kb: st.keyboard, focus_first: st.keyboard, ..Default::default() }];
        // a menu that replaced an open one starts clean (no stale submenus)
        ui.close_popup(lid(0));
        ui.open_popup(lid(0));
    }
    if !ui.is_popup_open(lid(0)) {
        resp.closed = true;
        return resp;
    }
    // a click on a lower level closed the ones above it
    while st.levels.len() > 1 && !ui.is_popup_open(lid(st.levels.len() - 1)) {
        st.levels.pop();
        st.levels.last_mut().unwrap().open = None;
    }
    ui.capture_keys = true;
    ui.set_focus(Some(lid(0)));
    let t = ui.theme;
    let pointer = ui.mouse_in().then(|| ui.mouse());
    let moved = st.pointer != pointer;
    st.pointer = pointer;
    let m = Metrics::new(&t);
    let mut rows: Vec<MenuRow> = Vec::new();

    // keys go to the deepest level
    for (qi, ev) in ui.key_input(lid(0)) {
        ui.consume(qi);
        // CM4: Char events feed accelerators and type-to-filter
        let Event::KeyDown { key, .. } = ev else { continue };
        let lv = st.levels.len() - 1;
        rows_at(src, st, lv, &mut rows);
        let cur = index_of(&rows, st.levels[lv].focus);
        let mut act = None;
        let mut to = None;
        match key {
            Key::Down => to = next_focus(&rows, cur, true),
            Key::Up => to = next_focus(&rows, cur, false),
            Key::Home | Key::PageUp => to = edge_focus(&rows, true),
            Key::End | Key::PageDown => to = edge_focus(&rows, false),
            Key::Right => {
                if let Some(i) = cur
                    && rows[i].submenu
                    && !rows[i].disabled
                {
                    act = Some(Act::Open(lv, rows[i].key, true));
                }
            }
            Key::Left | Key::Escape if lv > 0 => {
                close_above(ui, id, st, lv - 1);
                st.levels[lv - 1].kb = true;
                st.levels[lv - 1].reveal = true;
            }
            Key::Escape => {
                ui.close_all_popups();
                resp.closed = true;
                return resp;
            }
            Key::Enter | Key::Space => act = cur.and_then(|i| activate(st, &rows, lv, i, true)),
            _ => {}
        }
        if let Some(i) = to {
            let l = &mut st.levels[lv];
            l.focus = Some(rows[i].key);
            l.kb = true;
            l.reveal = true;
            ui.request_repaint();
        }
        match act {
            Some(Act::Pick(p)) => {
                ui.close_all_popups();
                resp.picked = Some(p);
                resp.closed = true;
                return resp;
            }
            Some(Act::Retry) => resp.retry = true,
            Some(Act::Open(lv, k, kb)) => open_child(ui, id, st, lv, k, kb),
            None => {}
        }
    }

    let mut ys = Vec::new();
    let mut lv = 0;
    while lv < st.levels.len() {
        rows_at(src, st, lv, &mut rows);
        // the parent row went away (rows were replaced): close from here
        if lv > 0 {
            let mut up = Vec::new();
            rows_at(src, st, lv - 1, &mut up);
            if !up.iter().any(|r| r.submenu && r.key == st.levels[lv].key) {
                close_above(ui, id, st, lv - 1);
                break;
            }
        }
        let content = layout(&rows, &m, &mut ys);
        let w = width(c, &rows, &m);
        let full_h = content + 2.0 * m.inset();
        let last = st.levels[lv].rect;
        let frozen = lv == 0 && last.w > 0.0 && ui.mouse_in() && last.contains_pt(ui.mouse());
        let rect = if lv > 0 {
            place_sub(st.levels[lv].parent_row, w, full_h, style.bounds, m.keep)
        } else if frozen {
            // rows arriving while the pointer is in the menu never move the
            // rows under it: keep the top, grow down, scroll past the edge
            let b = style.bounds.b() - m.keep;
            let x = last.x.min((style.bounds.r() - m.keep - w).max(style.bounds.x + m.keep));
            Rect::new(x, last.y, w, full_h.min((b - last.y).max(m.row + 2.0 * m.inset())))
        } else {
            place(st.anchor, w, full_h, style.bounds, m.keep)
        };
        st.levels[lv].rect = rect;
        if focus_first_pending(st, lv) {
            st.levels[lv].focus = edge_focus(&rows, true).map(|i| rows[i].key);
            st.levels[lv].reveal = true;
        }
        if !ui.popup_begin_r(c, lid(lv), rect, m.radius) {
            lv += 1;
            continue;
        }
        let scrolling = full_h > rect.h + 0.5;
        let strip = if scrolling { m.strip } else { 0.0 };
        let vp = Rect::new(rect.x + m.inset(), rect.y + m.inset() + strip, rect.w - 2.0 * m.inset(), (rect.h - 2.0 * m.inset() - 2.0 * strip).max(0.0));
        // keep the keyboard focus visible
        let mut scroll = st.levels[lv].scroll;
        if st.levels[lv].reveal {
            st.levels[lv].reveal = false;
            if let Some(i) = index_of(&rows, st.levels[lv].focus) {
                scroll = scroll_to_show(scroll, ys[i], m.row_h(&rows[i]), vp.h);
            }
        }
        scroll = clamp_scroll(scroll, content, vp.h);
        if scrolling {
            let step = 3.0 * m.row;
            let up = Rect::new(vp.x, rect.y + m.inset(), vp.w, strip);
            let down = Rect::new(vp.x, vp.b(), vp.w, strip);
            for (r, dir, glyph, id2) in [(up, -1.0, G_UP, "up"), (down, 1.0, G_DOWN, "down")] {
                let it = ui.interact(lid(lv).child(id2), r, Sense::click());
                let live = if dir < 0.0 { scroll > 0.0 } else { scroll + vp.h < content - 0.5 };
                if it.hovered && live {
                    c.round(r, t.radius_sm, t.selected_inactive);
                }
                if it.clicked.is_some() && live {
                    scroll = clamp_scroll(scroll + dir * step, content, vp.h);
                    ui.request_repaint();
                }
                let col = if !live { t.text_disabled } else if it.hovered { t.text } else { t.text_dim };
                c.text(&glyph.to_string(), r, f(m.arrow_w()).icon().c(), col);
            }
            // thumb
            let th = (vp.h * vp.h / content).max(12.0).min(vp.h);
            let ty = vp.y + (vp.h - th) * (scroll / (content - vp.h).max(1.0));
            c.round(Rect::new(rect.r() - 1.0 - 4.0, ty, 4.0, th), 2.0, t.scrollbar);
        }
        st.levels[lv].scroll = scroll;
        if lv == 0
            && let Some(ph) = style.loading
        {
            paint_loading(c, &t, rect, m.radius, ph);
        }
        c.clip(vp);
        let mut act = None;
        let kb_ring = st.levels[lv].kb;
        for (i, row) in rows.iter().enumerate() {
            let r = Rect::new(vp.x, vp.y + ys[i] - scroll, vp.w, m.row_h(row));
            if r.b() <= vp.y || r.y >= vp.b() {
                continue;
            }
            if row.kind == RowKind::Separator {
                let y = (r.center().y).floor() + 0.5;
                c.line(r.x + 6.0, y, r.r() - 6.0, y, t.border, 1.0);
                continue;
            }
            if row.kind == RowKind::Header {
                c.text(&row.label, Rect::new(r.x + m.label_x(), r.y, r.w - m.label_x(), r.h), m.small, t.text_dim);
                continue;
            }
            let it = ui.interact(lid(lv).with(row.key ^ 0x5eed), r.intersect(&vp), Sense::click());
            let lvl = &st.levels[lv];
            if it.hovered && row.focusable() && (moved || !lvl.kb) && (lvl.focus != Some(row.key) || lvl.kb) {
                let l = &mut st.levels[lv];
                l.focus = Some(row.key);
                l.kb = false;
                ui.request_repaint();
            }
            if it.hovered && row.focusable() && (moved || !st.levels[lv].kb) {
                // CM4: hover delay and safe triangle go here
                if row.submenu {
                    if st.levels[lv].open != Some(row.key) {
                        act = Some(Act::Open(lv, row.key, false));
                    }
                } else if st.levels[lv].open.is_some() {
                    close_above(ui, id, st, lv);
                }
                st.levels[lv + 1..].iter_mut().for_each(|l| l.kb = false);
            }
            if matches!(it.clicked, Some(Button::Left | Button::Right)) {
                act = activate(st, &rows, lv, i, false).or(act);
            }
            let lvl = &st.levels[lv];
            let state = RowPaint {
                focused: lvl.focus == Some(row.key) && row.focusable(),
                ring: kb_ring && lvl.focus == Some(row.key),
                pressed: it.active && row.kind == RowKind::Item && !row.disabled,
                open: lvl.open == Some(row.key),
                underline: st.keyboard,
            };
            if row.submenu && state.open && st.levels.len() > lv + 1 {
                st.levels[lv + 1].parent_row = r;
            }
            paint_row(c, &t, &m, r, row, state, src);
        }
        c.unclip();
        ui.popup_end(c);
        match act {
            Some(Act::Pick(p)) => {
                ui.close_all_popups();
                resp.picked = Some(p);
                resp.closed = true;
                return resp;
            }
            Some(Act::Retry) => resp.retry = true,
            Some(Act::Open(l, k, kb)) => {
                open_child(ui, id, st, l, k, kb);
                // place it against the row this frame
                if let Some(i) = rows.iter().position(|r| r.key == k) {
                    st.levels[l + 1].parent_row = Rect::new(vp.x, vp.y + ys[i] - scroll, vp.w, m.row);
                }
                ui.request_repaint();
            }
            None => {}
        }
        lv += 1;
    }
    resp
}

fn focus_first_pending(st: &mut MenuState, lv: usize) -> bool {
    std::mem::take(&mut st.levels[lv].focus_first)
}

fn paint_loading(c: &mut dyn Canvas, t: &Theme, rect: Rect, radius: f32, phase: f32) {
    // the 2 px track along the top, inside the border and clear of the corners
    let inset = (radius * 0.6).round();
    let track = Rect::new(rect.x + inset, rect.y + 1.0, rect.w - 2.0 * inset, 2.0);
    c.fill(track, t.border);
    let seg = track.w * 0.36;
    let x = track.x - seg + (track.w + seg) * phase.clamp(0.0, 1.0);
    let s = Rect::new(x, track.y, seg, track.h).intersect(&track);
    if s.w > 0.0 {
        c.fill(s, t.accent);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RowPaint {
    focused: bool,
    ring: bool,
    pressed: bool,
    open: bool,
    underline: bool,
}

fn paint_row(c: &mut dyn Canvas, t: &Theme, m: &Metrics, r: Rect, row: &MenuRow, s: RowPaint, src: &dyn MenuSource) {
    let rr = m.row_radius;
    if s.pressed {
        c.round(r, rr, t.accent);
    } else if s.focused {
        c.round(r, rr, t.selected);
        if s.ring {
            c.round_stroke(r, rr, t.focus, t.layout_focus_ring_width.max(1.0));
        }
    } else if s.open {
        c.round(r, rr, t.selected_inactive);
    }
    let lit = s.focused || s.open;
    let (mut fg, mut ico) = (t.text, if lit { t.text } else { t.text_dim });
    if row.danger {
        fg = t.danger;
        ico = t.danger;
    }
    if row.disabled {
        fg = t.text_disabled;
        ico = t.text_disabled;
    }
    if s.pressed {
        fg = t.on_accent;
        ico = t.on_accent;
    }
    let mid = r.center().y;
    if row.custom {
        c.circle(r.x + 4.0, mid, 2.0, t.accent);
    }
    let ir = Rect::new(r.x + 6.0, mid - m.icon / 2.0, m.icon, m.icon);
    if row.kind == RowKind::Status {
        c.text(&G_WARN.to_string(), ir, f(m.icon * 0.875).icon().c(), t.danger);
        let rw = m.retry_w(c);
        let bh = (18.0 * m.sc).round();
        let br = Rect::new(r.r() - 8.0 - rw, mid - bh / 2.0, rw, bh);
        let lx = r.x + m.label_x();
        c.text(&row.label, Rect::new(lx, r.y, (br.x - m.gap - lx).max(0.0), r.h), m.font, t.text_dim);
        c.round_stroke(br, t.radius_sm, if s.focused { t.focus } else { t.popup_border }, 1.0);
        c.text(RETRY, br, m.small.c(), t.text);
        return;
    }
    if row.checked {
        c.round(ir, t.radius_sm, t.selected_inactive);
        c.text(&G_CHECK.to_string(), ir, f(m.icon * 0.75).icon().c(), if s.pressed { t.on_accent } else { t.accent });
    } else if row.radio {
        c.circle(ir.center().x, mid, 3.0 * m.sc, if s.pressed { t.on_accent } else { t.accent });
    } else {
        match row.icon {
            RowIcon::None => {}
            RowIcon::Glyph(g) => c.text(&g.to_string(), ir, f(m.icon).icon().c(), ico),
            RowIcon::Image(k) => {
                if let Some((w, h, px)) = src.icon(k) {
                    c.image(&image_key(k), w, h, px, ir);
                }
            }
        }
    }
    let mut right = r.r() - 8.0;
    if row.submenu {
        let aw = m.arrow_w();
        let ar = Rect::new(right - aw + 3.0, mid - aw / 2.0, aw, aw);
        c.text(&G_ARROW.to_string(), ar, f(aw * 0.85).icon().c(), if s.pressed { t.on_accent } else if row.disabled { t.text_disabled } else { t.text_dim });
        right -= aw + m.gap;
    }
    if !row.shortcut.is_empty() {
        let sw = c.measure(&row.shortcut, m.small);
        let col = if s.pressed { t.on_accent } else if row.disabled { t.text_disabled } else { t.text_dim };
        c.text(&row.shortcut, Rect::new(right - sw, r.y, sw, r.h), m.small, col);
        right -= sw + 2.0 * m.gap;
    }
    let font = if row.default { m.font.b() } else { m.font };
    let lx = r.x + m.label_x();
    let lr = Rect::new(lx, r.y, (right - lx).max(0.0), r.h);
    c.text(&row.label, lr, font, fg);
    // CM4: also while Alt is held
    if s.underline
        && let Some(a) = row.accel
        && a < row.label.len()
        && row.label.is_char_boundary(a)
    {
        let end = a + row.label[a..].chars().next().map_or(1, char::len_utf8);
        let x0 = lx + c.caret_x(&row.label, font, a);
        let x1 = lx + c.caret_x(&row.label, font, end);
        if x1 <= lr.r() {
            let y = (mid + font.size() * 0.5).round() + 0.5;
            c.line(x0, y, x1, y, fg, 1.0);
        }
    }
}

/// The canvas cache key of image icon `k` (the backend keeps one device
/// copy per key; the app's icon cache bounds how many keys exist).
pub fn image_key(k: u64) -> String {
    format!("cm:{k:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Mods;
    use crate::testcanvas::TestCanvas;

    fn m() -> Metrics {
        Metrics::new(&Theme::dark())
    }

    #[test]
    fn metrics_follow_density_and_font_scale() {
        let base = Theme::dark();
        let cases = [(crate::Density::Compact, 1.0, 22.0, 8.0), (crate::Density::Comfortable, 1.0, 26.0, 9.0), (crate::Density::Spacious, 1.0, 30.0, 9.0), (crate::Density::Compact, 1.25, 26.0, 10.0), (crate::Density::Comfortable, 1.25, 28.0, 11.0), (crate::Density::Spacious, 1.25, 32.0, 11.0)];
        for (d, s, row, sep) in cases {
            let t = Theme::with_density(&base, d).with_font_scale(s);
            let m = Metrics::new(&t);
            assert_eq!(m.row, row, "{d:?} {s}");
            assert_eq!(m.sep, sep, "{d:?} {s}");
            assert_eq!(m.icon, if s > 1.0 { 20.0 } else { 16.0 });
        }
    }

    #[test]
    fn layout_stacks_rows_and_separators() {
        let m = m();
        let rows = [MenuRow::item(1, "a"), MenuRow::separator(), MenuRow::item(2, "b")];
        let mut ys = Vec::new();
        assert_eq!(layout(&rows, &m, &mut ys), 22.0 + 8.0 + 22.0);
        assert_eq!(ys, [0.0, 22.0, 30.0]);
    }

    #[test]
    fn width_is_clamped_and_grows_with_shortcuts() {
        let m = m();
        let mut c = TestCanvas::new(800.0, 600.0);
        assert_eq!(width(&mut c, &[MenuRow::item(1, "Cut")], &m), m.min_w);
        let long = "x".repeat(200);
        assert_eq!(width(&mut c, &[MenuRow::item(1, &long)], &m), m.max_w);
        let mid = "y".repeat(30);
        let a = width(&mut c, &[MenuRow::item(1, &mid)], &m);
        let b = width(&mut c, &[MenuRow { shortcut: "Ctrl+Shift+C".into(), ..MenuRow::item(1, &mid) }], &m);
        assert!(b > a, "{a} {b}");
    }

    #[test]
    fn placement_flips_near_edges_and_keeps_8px() {
        let b = Rect::new(0.0, 0.0, 1000.0, 700.0);
        // room: opens at the point
        assert_eq!(place(Pt::new(100.0, 100.0), 250.0, 300.0, b, 8.0), Rect::new(100.0, 100.0, 250.0, 300.0));
        // near the right edge: flips left
        let r = place(Pt::new(900.0, 100.0), 250.0, 300.0, b, 8.0);
        assert_eq!(r.x, 650.0);
        // near the bottom: flips up
        let r = place(Pt::new(100.0, 600.0), 250.0, 300.0, b, 8.0);
        assert_eq!(r.y, 300.0);
        // bottom-right corner: both
        let r = place(Pt::new(990.0, 690.0), 250.0, 300.0, b, 8.0);
        assert_eq!((r.x, r.y), (740.0, 390.0));
        // fits neither way: clamped to the 8 px keep-out
        let r = place(Pt::new(100.0, 350.0), 250.0, 500.0, b, 8.0);
        assert!(r.y >= 8.0 && r.b() <= 692.0, "{r:?}");
        // taller than the bounds: capped, so it scrolls
        let r = place(Pt::new(100.0, 10.0), 250.0, 5000.0, b, 8.0);
        assert_eq!((r.y, r.h), (8.0, 684.0));
        // left of the keep-out: pushed in
        assert_eq!(place(Pt::new(2.0, 2.0), 250.0, 100.0, b, 8.0).x, 8.0);
    }

    #[test]
    fn submenus_open_right_or_flip_left() {
        let b = Rect::new(0.0, 0.0, 1000.0, 700.0);
        let row = Rect::new(105.0, 200.0, 240.0, 22.0);
        let s = place_sub(row, 250.0, 100.0, b, 8.0);
        assert_eq!((s.x, s.y), (347.0, 195.0));
        let row = Rect::new(705.0, 200.0, 240.0, 22.0);
        let s = place_sub(row, 250.0, 100.0, b, 8.0);
        assert_eq!(s.r(), 703.0);
        // near the bottom: moved up to stay inside
        let row = Rect::new(105.0, 680.0, 240.0, 22.0);
        assert_eq!(place_sub(row, 250.0, 100.0, b, 8.0).b(), 692.0);
    }

    #[test]
    fn scroll_math() {
        assert_eq!(clamp_scroll(-5.0, 100.0, 50.0), 0.0);
        assert_eq!(clamp_scroll(80.0, 100.0, 50.0), 50.0);
        assert_eq!(clamp_scroll(10.0, 40.0, 50.0), 0.0);
        assert_eq!(scroll_to_show(0.0, 60.0, 22.0, 50.0), 32.0);
        assert_eq!(scroll_to_show(40.0, 10.0, 22.0, 50.0), 10.0);
        assert_eq!(scroll_to_show(10.0, 20.0, 22.0, 50.0), 10.0);
    }

    #[test]
    fn focus_skips_separators_and_disabled_and_wraps() {
        let rows = [MenuRow::item(1, "a"), MenuRow::separator(), MenuRow { disabled: true, ..MenuRow::item(2, "b") }, MenuRow::item(3, "c"), MenuRow::status(4, "x")];
        assert_eq!(next_focus(&rows, None, true), Some(0));
        assert_eq!(next_focus(&rows, None, false), Some(4));
        assert_eq!(next_focus(&rows, Some(0), true), Some(3));
        assert_eq!(next_focus(&rows, Some(3), false), Some(0));
        assert_eq!(next_focus(&rows, Some(4), true), Some(0), "wraps");
        assert_eq!(next_focus(&rows, Some(0), false), Some(4), "wraps up");
        assert_eq!(edge_focus(&rows, true), Some(0));
        assert_eq!(edge_focus(&rows, false), Some(4));
        assert_eq!(next_focus(&[MenuRow::separator()], None, true), None);
        assert_eq!(next_focus(&[], None, true), None);
    }

    // ---- driving the widget

    struct Src {
        top: Vec<MenuRow>,
        sub: Vec<MenuRow>,
    }
    impl MenuSource for Src {
        fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
            match path {
                [] => out.extend(self.top.iter().cloned()),
                [9] => out.extend(self.sub.iter().cloned()),
                _ => {}
            }
        }
        fn icon(&self, _: u64) -> Option<(u32, u32, &[u32])> {
            None
        }
    }

    fn src() -> Src {
        Src {
            top: vec![MenuRow { default: true, accel: Some(0), ..MenuRow::item(1, "Open") }, MenuRow::separator(), MenuRow::item(2, "Copy"), MenuRow { submenu: true, ..MenuRow::item(9, "Send to") }, MenuRow { disabled: true, ..MenuRow::item(3, "Paste") }],
            sub: vec![MenuRow::item(91, "Desktop"), MenuRow::item(92, "Mail")],
        }
    }

    struct H {
        ui: Ui,
        c: TestCanvas,
        st: MenuState,
        src: Src,
    }
    const ID: Id = Id::new("test.menu");
    impl H {
        fn new(at: Pt, keyboard: bool) -> H {
            H { ui: Ui::default(), c: TestCanvas::new(800.0, 600.0), st: MenuState::new(at, keyboard), src: src() }
        }
        fn frame(&mut self) -> MenuResponse {
            self.c.clear();
            assert!(self.c.begin(0));
            self.ui.begin_frame((800.0, 600.0));
            // like the app: the menu sets it again while it is drawn
            self.ui.capture_keys = false;
            let style = MenuStyle { bounds: Rect::new(0.0, 0.0, 800.0, 600.0), loading: None };
            let r = menu(&mut self.ui, &mut self.c, ID, &mut self.st, &self.src, &style);
            self.ui.end_frame();
            self.c.end();
            r
        }
        fn key(&mut self, k: Key) -> MenuResponse {
            self.ui.push(Event::KeyDown { key: k, mods: Mods::NONE, repeat: false });
            self.frame()
        }
        fn row_rect(&self, lv: usize, i: usize) -> Rect {
            let m = m();
            let mut ys = Vec::new();
            let mut rows = Vec::new();
            rows_at(&self.src, &self.st, lv, &mut rows);
            layout(&rows, &m, &mut ys);
            let r = self.st.levels[lv].rect;
            Rect::new(r.x + m.inset(), r.y + m.inset() + ys[i], r.w - 2.0 * m.inset(), m.row_h(&rows[i]))
        }
        fn click(&mut self, p: Pt) -> MenuResponse {
            self.ui.push(Event::MouseMove { pos: p });
            self.frame();
            self.ui.push(Event::MouseDown { pos: p, button: Button::Left, mods: Mods::NONE, double: false });
            self.frame();
            self.ui.push(Event::MouseUp { pos: p, button: Button::Left, mods: Mods::NONE });
            self.frame()
        }
    }

    #[test]
    fn keyboard_moves_opens_submenus_and_picks() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        assert_eq!(h.frame(), MenuResponse::default());
        assert_eq!(h.st.levels[0].focus, Some(1), "keyboard open focuses the first row");
        assert!(h.c.has_text("Open") && h.c.has_text("Send to"));
        h.key(Key::Down);
        assert_eq!(h.st.levels[0].focus, Some(2), "separator skipped");
        h.key(Key::Down);
        h.key(Key::Right);
        assert_eq!(h.st.levels.len(), 2);
        assert_eq!(h.st.levels[1].focus, Some(91));
        assert!(h.c.has_text("Desktop"));
        let sub = h.st.levels[1].rect;
        let top = h.st.levels[0].rect;
        assert!(sub.x > top.x + top.w - 10.0, "opens to the right: {sub:?} {top:?}");
        h.key(Key::Down);
        h.key(Key::Left);
        assert_eq!(h.st.levels.len(), 1);
        assert_eq!(h.st.levels[0].focus, Some(9), "focus back on the parent");
        h.key(Key::Right);
        h.key(Key::Down);
        let r = h.key(Key::Enter);
        assert_eq!(r.picked, Some(vec![9, 92]));
        assert!(r.closed);
        assert_eq!(h.ui.popups_open(), 0);
    }

    #[test]
    fn stationary_pointer_does_not_steal_keyboard_focus() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        h.ui.push(Event::MouseMove { pos: h.row_rect(0, 0).center() });
        h.frame();
        h.key(Key::Down);
        assert_eq!(h.st.levels[0].focus, Some(2));
        h.frame();
        assert_eq!(h.st.levels[0].focus, Some(2));
        assert!(h.st.levels[0].kb);
    }

    #[test]
    fn esc_closes_one_level_then_the_menu_and_keys_do_not_leak() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.frame();
        h.key(Key::End);
        assert_eq!(h.st.levels[0].focus, Some(9), "End skips the disabled last row");
        h.key(Key::Right);
        assert_eq!(h.st.levels.len(), 2);
        h.ui.push(Event::Char('x'));
        h.frame();
        assert!(h.ui.leftover().is_empty(), "typed chars stay in the menu");
        h.key(Key::Escape);
        assert_eq!(h.st.levels.len(), 1);
        assert!(h.ui.is_popup_open(ID.with(0)));
        let r = h.key(Key::Escape);
        assert!(r.closed && r.picked.is_none());
        assert!(h.ui.leftover().is_empty());
    }

    #[test]
    fn mouse_hover_opens_submenu_and_click_picks_disabled_does_nothing() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        assert_eq!(h.st.levels[0].focus, None, "mouse open: no focus");
        let send = h.row_rect(0, 3);
        h.ui.push(Event::MouseMove { pos: send.center() });
        h.frame();
        h.frame();
        assert_eq!(h.st.levels.len(), 2, "hover opens the submenu");
        let paste = h.row_rect(0, 4);
        let r = h.click(paste.center());
        assert!(r.picked.is_none() && !r.closed, "a disabled row does nothing");
        let copy = h.row_rect(0, 2);
        let r = h.click(copy.center());
        assert_eq!(r.picked, Some(vec![2]));
    }

    #[test]
    fn click_outside_closes() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        let r = h.click(Pt::new(700.0, 500.0));
        assert!(r.closed && r.picked.is_none());
    }

    #[test]
    fn rows_arriving_keep_the_focus_and_the_top() {
        let mut h = H::new(Pt::new(100.0, 500.0), true);
        h.frame();
        let first = h.st.levels[0].rect;
        assert!(first.b() <= 592.0, "flipped up: {first:?}");
        h.key(Key::Down);
        assert_eq!(h.st.levels[0].focus, Some(2));
        // the pointer rests inside; shell rows arrive below
        h.ui.push(Event::MouseMove { pos: Pt::new(first.x + 1.0, first.y + 1.0) });
        h.frame();
        for k in 0..10 {
            h.src.top.push(MenuRow::item(100 + k, "Shell item"));
        }
        h.frame();
        assert_eq!(h.st.levels[0].rect.y, first.y, "top frozen while the pointer is in");
        assert!(h.st.levels[0].rect.b() <= 592.0);
        assert_eq!(h.st.levels[0].focus, Some(2));
        // tall menu: chevron strips
        let tall: Vec<MenuRow> = (0..60).map(|k| MenuRow::item(1000 + k, "Row")).collect();
        h.src.top = tall;
        h.ui.push(Event::MouseLeave);
        h.frame();
        assert!(h.c.has_text("\u{E70D}"), "down strip");
        h.key(Key::End);
        assert!(h.st.levels[0].scroll > 0.0, "End scrolls to the last row");
    }

    #[test]
    fn status_row_retries_and_lazy_submenu_says_loading() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.src.top.push(MenuRow::status(77, "Some items could not load"));
        h.src.sub.clear();
        h.frame();
        assert!(h.c.has_text("Retry"));
        h.key(Key::Up);
        let r = h.key(Key::Enter);
        assert!(r.retry && !r.closed);
        h.key(Key::Up);
        h.key(Key::Right);
        assert!(h.c.has_text(LOADING), "an empty submenu shows Loading…");
    }
}
