//! The own-drawn context menu (`docs/ctxmenu.md`): a stack of popups, one per
//! open level, drawn after every other widget.
//!
//! The app hands rows in through [`MenuSource`] each frame (so rows that
//! arrive late from the shell just show up) and keeps a [`MenuState`] while
//! the menu is open. Rows are identified by a key, not an index: focus and
//! the open submenu stay on the same row when rows are inserted.
//!
//! Input (CM4): typed text filters the menu (an inline field at the top,
//! submenu children searched too), Alt + letter runs accelerators, the wheel
//! and PageUp/PageDown scroll tall menus, submenus open after [`OPEN_MS`] of
//! hover and close [`CLOSE_MS`] after the pointer left them, unless it is
//! heading for the open submenu (the safe triangle). Timing is pure: the app
//! passes the clock in [`MenuStyle::now_ms`] and runs a tick only while
//! [`MenuState::deadline`] is set. A submenu whose rows are not in yet shows
//! "Loading…" (CM6 fills it lazily, [`MenuSource::sub_state`]).

use crate::canvas::{Canvas, Font, f};
use crate::geom::{Pt, Rect};
use crate::input::{Button, Event, Key, Mods};
use crate::theme::Theme;
use crate::ui::{Id, Sense, Ui};

/// Hover time before a submenu opens (the mock's state 5c).
pub const OPEN_MS: u64 = 150;
/// Time an open submenu stays after the pointer moved to another row.
pub const CLOSE_MS: u64 = 250;
/// How deep the filter searches submenus (the shell model's depth cap).
const FILTER_DEPTH: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowKind {
    #[default]
    Item,
    Separator,
    /// "Some items could not load" + Retry (picking it sets `retry`)
    Status,
    /// a user group's name above its block ("Name in menu"): a dim 11 px
    /// text, not focusable, not in the UI Automation tree
    Label,
    /// CM8 edit menu: the edited item's name, `shortcut` holds its source tag
    /// ("Windows"); not focusable
    Header,
    /// CM8 edit menu: a dim note, wrapped over as many lines as it needs
    Note,
    /// CM8: the strip after Hide ("Hid "Share"" | Undo | Manage); the widget
    /// adds it itself
    Strip,
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
    /// a common action (Cut, Copy, Paste, Rename, Delete, Share) that folds
    /// into the icon-only row at the top when the menu is too tall (CM9)
    pub quick: bool,
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
            RowKind::Status | RowKind::Strip => true,
            RowKind::Separator | RowKind::Header | RowKind::Note | RowKind::Label => false,
        }
    }
    /// The accelerator letter, lower-case.
    pub fn accel_char(&self) -> Option<char> {
        let a = self.accel?;
        let c = self.label.get(a..)?.chars().next()?;
        c.to_lowercase().next()
    }
}

/// What an empty submenu shows (CM6: its rows are fetched when it opens).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SubLoad {
    /// the rows are in; no rows means an empty submenu
    #[default]
    Ready,
    /// "Loading…"
    Loading,
    /// the rows did not come: a failure row with Retry
    Failed,
}

/// Where the rows come from.
pub trait MenuSource {
    /// The rows of the menu at `path` (row keys from the top level down;
    /// empty = the top level).
    fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>);
    /// Pixels of an image icon: (w, h, premultiplied BGRA).
    fn icon(&self, key: u64) -> Option<(u32, u32, &[u32])>;
    /// The state of the submenu at `path` while it has no rows.
    fn sub_state(&self, _path: &[u64]) -> SubLoad {
        SubLoad::Ready
    }
    /// CM8: the edit menu of the row at `path` (right click on it, or the
    /// Menu key / Shift+F10 while it has the focus). Rows with the `EDIT_*`
    /// keys get their keys (Del, F2, Alt+Up / Down) and behaviour; others are
    /// reported in [`MenuResponse::edit`]. No rows: the row is not editable.
    fn edit_rows(&self, _path: &[u64], _out: &mut Vec<MenuRow>) {}
}

/// Edit-menu row keys the widget knows (CM8). `EDIT_HIDE` shows the Undo
/// strip, `EDIT_RENAME` turns the row into a text field, `EDIT_UP` /
/// `EDIT_DOWN` keep the edit menu open (the row moves under it),
/// `EDIT_SETTINGS` closes the menu. Every one is reported in
/// [`MenuResponse::edit`] except `EDIT_RENAME` (see [`MenuResponse::renamed`]).
pub const EDIT_HIDE: u64 = 0xed17_0001;
pub const EDIT_RENAME: u64 = 0xed17_0002;
pub const EDIT_UP: u64 = 0xed17_0003;
pub const EDIT_DOWN: u64 = 0xed17_0004;
pub const EDIT_SETTINGS: u64 = 0xed17_0005;
/// Pin to top / Unpin: like Move up, the row moves under the open edit menu.
pub const EDIT_PIN: u64 = 0xed17_0006;
/// "Move to group": a submenu row of the edit menu; its rows come from
/// `edit_rows(path + [EDIT_GROUP])` (flyout to the right).
pub const EDIT_GROUP: u64 = 0xed17_0007;
/// Edit entry... (custom and imported rows).
pub const EDIT_ENTRY: u64 = 0xed17_0008;
/// Delete entry (custom and imported rows): the Undo strip says "Deleted".
pub const EDIT_DELETE: u64 = 0xed17_0009;
/// In the group flyout: New group... and Native place (no group).
pub const EDIT_GROUP_NEW: u64 = 0xed17_0010;
pub const EDIT_GROUP_NATIVE: u64 = 0xed17_0011;
/// In the group flyout: group `n` is `EDIT_GROUP_BASE + n`.
pub const EDIT_GROUP_BASE: u64 = 0xed17_0100;
/// The Undo strip's row key.
pub const KEY_STRIP: u64 = 0xed17_00ff;
/// The edit menu's popup id is `id.with(EDIT_POPUP)` (above every level).
const EDIT_POPUP: u64 = 0xed17;
const RENAME_ID: u64 = 0xed18;
const UNDO: &str = "Undo";
const MANAGE: &str = "Manage";
pub const G_EYE_OFF: char = '\u{ED1A}';

/// The open edit menu (CM8).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditMenu {
    /// the level of the edited row and its path (row keys from the top)
    pub lv: usize,
    pub path: Vec<u64>,
    pub focus: Option<u64>,
    pub kb: bool,
    /// the edited row's rect (the popup is placed against it, like a submenu)
    pub row: Rect,
    pub rect: Rect,
    /// the "Move to group" flyout, when open
    pub sub: Option<EditSub>,
    /// last frame's rect of the [`EDIT_GROUP`] row (the flyout is placed against it)
    pub sub_row: Rect,
}

/// The open flyout of the edit menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditSub {
    pub focus: Option<u64>,
    pub kb: bool,
    pub rect: Rect,
}

/// The inline rename of a row (CM8).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rename {
    pub lv: usize,
    pub path: Vec<u64>,
    pub input: crate::widgets::text_input::TextInput,
    /// the name shown when the rename started
    pub orig: String,
    fresh: bool,
}

/// Per-frame inputs that are not rows.
#[derive(Clone, Copy, Debug)]
pub struct MenuStyle {
    /// Alt is held: accelerators are underlined
    pub alt: bool,
    /// where the menu may go (the window's client area)
    pub bounds: Rect,
    /// the loading line's phase (0..1) while shell rows are still coming;
    /// None: no line
    pub loading: Option<f32>,
    /// a monotonic clock in ms (submenu hover timing)
    pub now_ms: u64,
    /// a screen reader listens (CM10): fill [`MenuState::a11y`] this frame.
    /// False: no snapshot work at all
    pub a11y: bool,
}

/// Submenu hover timing of one level, a state machine over a clock.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubTimer {
    /// a submenu row is hovered: (its key, when it opens)
    pub open_at: Option<(u64, u64)>,
    /// the open child closes then (the pointer went to another row)
    pub close_at: Option<u64>,
}

/// One frame's pointer facts for [`SubTimer::step`].
#[derive(Clone, Copy, Debug, Default)]
pub struct HoverIn {
    pub now: u64,
    /// the pointer moved since the last frame
    pub moved: bool,
    /// last frame's pointer (the safe triangle's apex)
    pub prev: Option<Pt>,
    pub pointer: Option<Pt>,
    /// the focusable row under the pointer: (key, opens a submenu)
    pub hovered: Option<(u64, bool)>,
    /// the row whose submenu is open
    pub open: Option<u64>,
    /// the open submenu's rect
    pub child: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverAct {
    /// open this row's submenu (closing another one)
    Open(u64),
    /// close the open submenu
    Close,
}

/// Inside the triangle from `apex` to the near corners of `sub` (with a few
/// DIPs of slack above and below): the pointer is heading for the submenu.
pub fn in_safe_triangle(apex: Pt, p: Pt, sub: Rect) -> bool {
    const SLACK: f32 = 4.0;
    if sub.w <= 0.0 || sub.h <= 0.0 {
        return false;
    }
    let nx = if sub.x >= apex.x { sub.x } else { sub.r() };
    let (b, c) = (Pt::new(nx, sub.y - SLACK), Pt::new(nx, sub.b() + SLACK));
    let cross = |o: Pt, a: Pt, q: Pt| (a.x - o.x) * (q.y - o.y) - (a.y - o.y) * (q.x - o.x);
    let (d1, d2, d3) = (cross(apex, b, p), cross(b, c, p), cross(c, apex, p));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

impl SubTimer {
    /// Feeds one frame. Returns what to do now, and whether the pointer is
    /// heading for the open submenu (hover must not move the focus then).
    pub fn step(&mut self, i: &HoverIn) -> (Option<HoverAct>, bool) {
        let in_child = matches!((i.pointer, i.child), (Some(p), Some(c)) if c.contains_pt(p));
        if in_child {
            *self = SubTimer::default();
            return (None, false);
        }
        let over_open = i.open.is_some() && i.hovered.map(|h| h.0) == i.open;
        let aiming = !over_open && i.moved && matches!((i.open, i.prev, i.pointer, i.child), (Some(_), Some(a), Some(p), Some(c)) if in_safe_triangle(a, p, c));
        if aiming {
            // keep the submenu while the pointer travels; a pause on the
            // way lets the row under it win after CLOSE_MS
            self.open_at = None;
            self.close_at = Some(i.now + CLOSE_MS);
            return (None, true);
        }
        match i.hovered {
            _ if over_open => *self = SubTimer::default(),
            Some((k, true)) => {
                if self.open_at.map(|o| o.0) != Some(k) {
                    self.open_at = Some((k, i.now + OPEN_MS));
                }
                if i.open.is_some() && self.close_at.is_none() {
                    self.close_at = Some(i.now + CLOSE_MS);
                }
            }
            Some((_, false)) => {
                self.open_at = None;
                if i.open.is_some() && self.close_at.is_none() {
                    self.close_at = Some(i.now + CLOSE_MS);
                }
            }
            // outside every row (another level, outside the menu, or a
            // stationary pointer under keyboard focus): nothing changes
            None => {
                if i.moved || i.pointer.is_none() {
                    *self = SubTimer::default();
                }
            }
        }
        if let Some((k, at)) = self.open_at
            && i.now >= at
        {
            *self = SubTimer::default();
            return (Some(HoverAct::Open(k)), false);
        }
        if let Some(at) = self.close_at
            && i.now >= at
            && i.open.is_some()
        {
            self.close_at = None;
            return (Some(HoverAct::Close), false);
        }
        (None, false)
    }

    /// When the next frame is needed (the app's tick).
    pub fn deadline(&self) -> Option<u64> {
        match (self.open_at.map(|o| o.1), self.close_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
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
    pub timer: SubTimer,
    reveal: bool,
    focus_first: bool,
    strip: f32,
    /// the top level folded its common actions into the icon row (CM9);
    /// once folded it stays folded while the menu is open
    folded: bool,
    /// height above the scroll viewport (filter field or icon row)
    hdr: f32,
}

/// One row as a screen reader sees it (CM10): plain data the app copies to
/// the UI Automation thread. Separators, headers and notes are not listed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct A11yRow {
    pub key: u64,
    /// the label without its accelerator marker; in a filtered list the
    /// parents' labels come first ("Send to > Desktop")
    pub name: String,
    /// the accelerator letter (Alt + it runs the row)
    pub accel: Option<char>,
    pub shortcut: String,
    /// client-area DIPs; may lie outside the viewport (see `visible`)
    pub rect: Rect,
    pub enabled: bool,
    pub checked: bool,
    pub radio: bool,
    pub submenu: bool,
    /// its submenu is open
    pub expanded: bool,
    /// the menu's keyboard focus: the deepest level's focused row
    pub focused: bool,
    /// at least partly inside the scroll viewport
    pub visible: bool,
    /// a button of the icon row
    pub quick: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct A11yLevel {
    /// key of the row that opened it (0 for the top)
    pub key: u64,
    pub rect: Rect,
    pub rows: Vec<A11yRow>,
    /// the type-to-filter field (top level only): rect and text
    pub filter: Option<(Rect, String)>,
}

/// What the open menu looks like to a screen reader, one entry per open level.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct A11ySnap {
    pub levels: Vec<A11yLevel>,
}

/// What a screen reader asked of a row (CM10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum A11yReq {
    /// the same path as Enter on the focused row
    Invoke,
    /// close the submenu this row opened (and the ones above it)
    Collapse,
}

/// The app keeps this while the menu is open.
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    /// CM10: filled by [`menu`] each frame while [`MenuStyle::a11y`] is set
    pub a11y: Option<A11ySnap>,
    /// CM10: requests from the screen reader, run by the next frame
    a11y_req: Vec<(usize, u64, A11yReq)>,
    /// top-left corner the menu opens at (it flips left / up from here)
    pub anchor: Pt,
    /// opened from the keyboard (Shift+F10, Menu key): accelerators are
    /// underlined and the first row has the focus
    pub keyboard: bool,
    pub levels: Vec<Level>,
    /// the type-to-filter text (empty: no filter)
    pub filter: String,
    opened: bool,
    pointer: Option<Pt>,
    /// width when the filter started (the menu does not shrink while typing)
    filter_w: f32,
    /// the filter text changed: focus the first match
    filter_reset: bool,
    /// CM8: the open edit menu
    pub edit: Option<EditMenu>,
    /// CM8: the row being renamed in place
    pub rename: Option<Rename>,
    /// CM8: the name of the item just hidden (the Undo strip shows)
    pub strip: Option<String>,
    /// the strip's keyboard part: false Undo, true Manage
    strip_manage: bool,
    /// the strip says "Deleted" (a custom entry) instead of "Hid"
    pub strip_deleted: bool,
    /// Del in the edit menu deletes the entry instead of hiding the row (Settings)
    pub del_deletes: bool,
    /// Rename in the edit menu is reported in [`MenuResponse::edit`] (the app has
    /// its own field) instead of turning a menu row into one (Settings)
    pub external_rename: bool,
    /// the edit menu closed on a press in the menu: that click must not
    /// run the row under it
    eat_click: bool,
}

impl MenuState {
    pub fn new(anchor: Pt, keyboard: bool) -> MenuState {
        MenuState { anchor, keyboard, ..Default::default() }
    }

    /// An edit menu or an inline rename is open (CM8).
    pub fn editing(&self) -> bool {
        self.edit.is_some() || self.rename.is_some()
    }

    /// When the menu needs its next frame without input (a submenu hover
    /// delay). None: no timer.
    pub fn deadline(&self) -> Option<u64> {
        self.levels.iter().filter_map(|l| l.timer.deadline()).min()
    }

    /// Submenu paths whose rows are wanted now: the open ones and the one the
    /// pointer is about to open (CM6 fetches them).
    pub fn wanted(&self) -> Vec<Vec<u64>> {
        let path = |lv: usize| -> Vec<u64> { self.levels.get(1..=lv).map(|l| l.iter().map(|l| l.key).collect()).unwrap_or_default() };
        let mut v: Vec<Vec<u64>> = (1..self.levels.len()).map(path).collect();
        for (lv, l) in self.levels.iter().enumerate() {
            if let Some((k, _)) = l.timer.open_at {
                let mut p = path(lv);
                p.push(k);
                v.push(p);
            }
        }
        v
    }

    pub fn filtering(&self) -> bool {
        !self.filter.is_empty()
    }

    /// CM10: a screen reader invokes (or collapses) row `key` of level `lv`;
    /// the next frame runs it like the keyboard would.
    pub fn request(&mut self, lv: usize, key: u64, what: A11yReq) {
        self.a11y_req.push((lv, key, what));
    }

    /// CM10: requests are waiting for the next frame.
    pub fn has_requests(&self) -> bool {
        !self.a11y_req.is_empty()
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
    /// CM8: an edit-menu row was run on the row at this path: (path, the
    /// edit row's key, `EDIT_*` or the app's own)
    pub edit: Option<(Vec<u64>, u64)>,
    /// CM8: an inline rename was kept (Enter): (path, the new name, trimmed;
    /// empty = back to the original name)
    pub renamed: Option<(Vec<u64>, String)>,
    /// CM8: Undo on the strip after Hide
    pub undo: bool,
    /// CM8: Manage on the strip (the menu closes; the app opens Settings)
    pub manage: bool,
}

// ---- metrics -------------------------------------------------------------

/// Sizes from the theme (density and font scale applied).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// font scale over the density's base size (1.25 at 125 %)
    pub sc: f32,
    pub row: f32,
    pub sep: f32,
    /// a [`RowKind::Label`] row (20 px at 100 %)
    pub label: f32,
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
            label: (20.0 * sc).round(),
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
        match r.kind {
            RowKind::Separator => self.sep,
            RowKind::Label => self.label,
            _ => self.row,
        }
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
    /// The filter field's height (the mock: a row + 4) and the gap below it.
    fn field_h(&self) -> f32 {
        self.row + 4.0
    }
    fn header(&self) -> f32 {
        self.field_h() + self.pad
    }
    /// The icon row's height (CM9): a little taller than a row, like Win11.
    pub fn quick_h(&self) -> f32 {
        (self.row + 8.0 * self.sc).round()
    }
    /// One icon button's width.
    pub fn quick_w(&self) -> f32 {
        (self.row * 1.5).round()
    }
}

const RETRY: &str = "Retry";
const LOADING: &str = "Loading\u{2026}";
const EMPTY: &str = "(Empty)";
const SUB_FAILED: &str = "Could not load";
const NO_MATCH: &str = "No matching items";
const PARENT_SEP: &str = " \u{203A} ";
const G_CHECK: char = '\u{E73E}';
const G_ARROW: char = '\u{E76C}';
const G_UP: char = '\u{E70E}';
const G_DOWN: char = '\u{E70D}';
const G_WARN: char = '\u{E7BA}';
const G_SEARCH: char = '\u{E721}';
/// Keys of rows the widget makes itself.
const KEY_LOADING: u64 = 0x10ad_0001;
const KEY_SUB_FAILED: u64 = 0x10ad_0002;
const KEY_NO_MATCH: u64 = 0x10ad_0003;

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
            RowKind::Separator => {}
            RowKind::Item => {
                let font = if r.default { m.font.b() } else { m.font };
                lw = lw.max(c.measure(&r.label, font));
                if !r.shortcut.is_empty() {
                    sw = sw.max(c.measure(&r.shortcut, m.small));
                }
                sub |= r.submenu;
            }
            RowKind::Label => lw = lw.max(c.measure(&r.label, m.small)),
            RowKind::Status => st = st.max(c.measure(&r.label, m.font) + m.gap + m.retry_w(c)),
            RowKind::Strip => st = st.max(strip_w(c, &r.label, m)),
            RowKind::Header => lw = lw.max(c.measure(&r.label, m.font.b()) + if r.shortcut.is_empty() { 0.0 } else { c.measure(&r.shortcut, m.small) + 3.0 * m.gap }),
            // notes wrap to the width the other rows give
            RowKind::Note => {}
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

/// The Undo strip's label + its two text buttons (CM8).
fn strip_w(c: &mut dyn Canvas, label: &str, m: &Metrics) -> f32 {
    c.measure(label, m.font) + 2.0 * m.gap + c.measure(UNDO, m.font) + c.measure(MANAGE, m.font) + 4.0 * m.gap
}

/// Greedy word wrap of `s` into lines no wider than `w` (CM8 notes).
pub fn wrap_text(c: &mut dyn Canvas, s: &str, font: Font, w: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let cand = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if line.is_empty() || c.measure(&cand, font) <= w {
            line = cand;
        } else {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
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

/// The scroll after `dy` wheel notches (+1 = up), `step` DIPs per notch.
pub fn wheel_scroll(scroll: f32, dy: f32, step: f32, content: f32, view: f32) -> f32 {
    clamp_scroll(scroll - dy * step, content, view)
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

/// PageDown / PageUp: `page` rows on (no wrap), then the nearest focusable
/// row not past that point; the last (first) one when the page runs out.
pub fn page_focus(rows: &[MenuRow], from: Option<usize>, page: usize, down: bool) -> Option<usize> {
    let n = rows.len();
    if n == 0 {
        return None;
    }
    let page = page.max(1);
    let Some(c) = from else { return edge_focus(rows, down) };
    let ok = |i: &usize| rows[*i].focusable();
    if down {
        let t = c.saturating_add(page).min(n - 1);
        (c + 1..=t).rev().find(ok).or_else(|| (t + 1..n).find(ok)).or(Some(c))
    } else {
        let t = c.saturating_sub(page);
        (t..c).find(ok).or_else(|| (0..t).rev().find(ok)).or(Some(c))
    }
}

/// Alt + `ch`: the row with that accelerator. One match: (it, run it);
/// several: (the next one after `from`, cycling, only focus it).
pub fn accel_target(rows: &[MenuRow], from: Option<usize>, ch: char) -> Option<(usize, bool)> {
    let ch = ch.to_lowercase().next()?;
    let hits: Vec<usize> = rows.iter().enumerate().filter(|(_, r)| r.kind == RowKind::Item && r.focusable() && r.accel_char() == Some(ch)).map(|(i, _)| i).collect();
    match hits.len() {
        0 => None,
        1 => Some((hits[0], true)),
        _ => Some((hits.iter().copied().find(|&i| from.is_none_or(|c| i > c)).unwrap_or(hits[0]), false)),
    }
}

/// Case-insensitive substring: the byte range of the first match.
pub fn find_ci(label: &str, q: &str) -> Option<(usize, usize)> {
    let low = |c: char| c.to_lowercase().next().unwrap_or(c);
    let qc: Vec<char> = q.chars().map(low).collect();
    let lc: Vec<(usize, char)> = label.char_indices().map(|(i, c)| (i, low(c))).collect();
    if qc.is_empty() || qc.len() > lc.len() {
        return None;
    }
    (0..=lc.len() - qc.len()).find(|&s| (0..qc.len()).all(|k| lc[s + k].1 == qc[k])).map(|s| (lc[s].0, lc.get(s + qc.len()).map_or(label.len(), |x| x.0)))
}

/// A row of the filtered list: where it lives and how it is marked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// row keys from the top level to it
    pub path: Vec<u64>,
    /// its parents' labels ("Send to"), empty at the top level
    pub prefix: String,
    /// the matched bytes of the label
    pub hl: Option<(usize, usize)>,
}

fn path_key(path: &[u64]) -> u64 {
    if let [k] = path {
        return *k;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for k in path {
        h = (h ^ k).wrapping_mul(0x100_0000_01b3);
    }
    h | 1
}

/// The rows matching `q`, the top level and every submenu that has rows
/// (depth first, as the menu reads), with their paths. Separators, failure
/// rows and empty submenus drop out.
pub fn filter_rows(src: &dyn MenuSource, q: &str, rows: &mut Vec<MenuRow>, found: &mut Vec<Found>) {
    fn walk(src: &dyn MenuSource, q: &str, path: &mut Vec<u64>, prefix: &str, rows: &mut Vec<MenuRow>, found: &mut Vec<Found>) {
        let mut level = Vec::new();
        src.rows(path, &mut level);
        for r in level {
            if r.kind != RowKind::Item {
                continue;
            }
            path.push(r.key);
            if let Some(hl) = find_ci(&r.label, q) {
                rows.push(MenuRow { key: path_key(path), ..r.clone() });
                found.push(Found { path: path.clone(), prefix: prefix.to_string(), hl: Some(hl) });
            }
            if r.submenu && path.len() < FILTER_DEPTH {
                let pre = if prefix.is_empty() { r.label.clone() } else { format!("{prefix}{PARENT_SEP}{}", r.label) };
                walk(src, q, path, &pre, rows, found);
            }
            path.pop();
        }
    }
    rows.clear();
    found.clear();
    walk(src, q, &mut Vec::new(), "", rows, found);
}

// ---- tall menus: the icon row (CM9) -----------------------------------------

/// The top level does not fit in `avail` DIPs (the bounds minus the
/// keep-out) as a plain list, and at least two rows can fold: fold them into
/// the icon row (the Windows 11 trick, `docs/ctxmenu-mock.md` question 3).
pub fn should_fold(rows: &[MenuRow], m: &Metrics, avail: f32) -> bool {
    let n = rows.iter().filter(|r| r.quick && r.kind == RowKind::Item).count();
    let mut ys = Vec::new();
    n >= 2 && layout(rows, m, &mut ys) + 2.0 * m.inset() > avail + 0.5
}

/// Splits the top level into the icon row (the foldable rows, in menu order)
/// and the list without them (no leading, trailing or doubled separators).
pub fn fold_quick(rows: &[MenuRow]) -> (Vec<MenuRow>, Vec<MenuRow>) {
    let mut quick = Vec::new();
    let mut rest: Vec<MenuRow> = Vec::with_capacity(rows.len());
    for r in rows {
        if r.quick && r.kind == RowKind::Item {
            quick.push(r.clone());
        } else if r.kind != RowKind::Separator || rest.last().is_some_and(|l| l.kind != RowKind::Separator) {
            rest.push(r.clone());
        }
    }
    while rest.last().is_some_and(|l| l.kind == RowKind::Separator) {
        rest.pop();
    }
    (quick, rest)
}

/// The icon buttons inside `area`, left to right (Win11: left-aligned,
/// a fixed width each; narrower when they would not fit).
pub fn quick_rects(area: Rect, n: usize, m: &Metrics) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let w = m.quick_w().min(area.w / n as f32);
    (0..n).map(|i| Rect::new(area.x + i as f32 * w, area.y, w, area.h)).collect()
}

/// The icon button under `p`.
pub fn quick_hit(rects: &[Rect], p: Pt) -> Option<usize> {
    rects.iter().position(|r| r.contains_pt(p))
}

/// Keys that move between the icon row and the list (the row is a
/// horizontal strip above the list): Left / Right within the row (wrapping),
/// Down from it to the first list row, Up from it to the last one (the menu
/// wraps), Up from the first list row and Down from the last one into the
/// row, Home to its first button. Returns the new focus, None when the key
/// is the list's business (or there is no row).
pub fn quick_nav(quick: &[MenuRow], rows: &[MenuRow], focus: Option<u64>, key: Key) -> Option<u64> {
    if quick.iter().all(|q| !q.focusable()) {
        return None;
    }
    let qi = index_of(quick, focus);
    let li = index_of(rows, focus);
    let key_of = |v: &[MenuRow], i: Option<usize>| i.map(|i| v[i].key);
    match (key, qi) {
        (Key::Left, Some(i)) => key_of(quick, next_focus(quick, Some(i), false)),
        (Key::Right, Some(i)) => key_of(quick, next_focus(quick, Some(i), true)),
        (Key::Down | Key::PageDown, Some(_)) => key_of(rows, edge_focus(rows, true)).or(focus),
        (Key::Up, Some(_)) => key_of(rows, edge_focus(rows, false)).or(focus),
        (Key::PageUp, Some(_)) => focus,
        (Key::Home, _) => key_of(quick, edge_focus(quick, true)),
        (Key::Up, None) if li.is_some() && li == edge_focus(rows, true) => key_of(quick, edge_focus(quick, true)),
        (Key::Down, None) if li.is_some() && li == edge_focus(rows, false) => key_of(quick, edge_focus(quick, true)),
        _ => None,
    }
}

// ---- the widget ----------------------------------------------------------

enum Act {
    Pick(Vec<u64>),
    Retry,
    Open(usize, u64, bool),
    /// a submenu row of the filtered list: leave the filter, open it
    OpenPath(Vec<u64>),
    /// CM8: open the edit menu of row (level, key), from the keyboard
    Edit(usize, u64, bool),
    /// CM8: the strip's Undo (false) or Manage (true)
    Strip(bool),
}

fn path_to(st: &MenuState, lv: usize) -> Vec<u64> {
    st.levels[1..=lv].iter().map(|l| l.key).collect()
}

fn rows_at(src: &dyn MenuSource, st: &MenuState, lv: usize, out: &mut Vec<MenuRow>) {
    let path = path_to(st, lv);
    out.clear();
    src.rows(&path, out);
    if lv > 0 && out.is_empty() {
        out.push(match src.sub_state(&path) {
            SubLoad::Loading => MenuRow { disabled: true, ..MenuRow::item(KEY_LOADING, LOADING) },
            SubLoad::Failed => MenuRow::status(KEY_SUB_FAILED, SUB_FAILED),
            SubLoad::Ready => MenuRow { disabled: true, ..MenuRow::item(KEY_LOADING, EMPTY) },
        });
    }
}

/// The rows level `lv` shows: the filtered list at the top while filtering
/// (`found` parallel to it), else the level's own rows.
fn level_rows(src: &dyn MenuSource, st: &MenuState, lv: usize, rows: &mut Vec<MenuRow>, found: &mut Vec<Found>) {
    if lv == 0 && st.filtering() {
        filter_rows(src, &st.filter, rows, found);
        if rows.is_empty() {
            rows.push(MenuRow { disabled: true, icon: RowIcon::Glyph(G_SEARCH), ..MenuRow::item(KEY_NO_MATCH, NO_MATCH) });
            found.push(Found::default());
        }
        return;
    }
    found.clear();
    rows_at(src, st, lv, rows);
    if lv == 0
        && let Some(name) = &st.strip
    {
        rows.push(MenuRow::separator());
        rows.push(MenuRow { kind: RowKind::Strip, icon: RowIcon::Glyph(G_EYE_OFF), ..MenuRow::item(KEY_STRIP, &format!("{} \u{201c}{name}\u{201d}", if st.strip_deleted { "Deleted" } else { "Hid" })) });
    }
}

fn index_of(rows: &[MenuRow], key: Option<u64>) -> Option<usize> {
    key.and_then(|k| rows.iter().position(|r| r.key == k && r.kind != RowKind::Separator))
}

/// What activating row `i` of level `lv` does.
fn activate(st: &MenuState, rows: &[MenuRow], found: &[Found], lv: usize, i: usize, kb: bool) -> Option<Act> {
    let r = rows.get(i)?;
    let filtered = lv == 0 && st.filtering();
    match r.kind {
        RowKind::Separator | RowKind::Header | RowKind::Note | RowKind::Label => None,
        RowKind::Status => Some(Act::Retry),
        RowKind::Strip => Some(Act::Strip(st.strip_manage)),
        RowKind::Item if r.disabled => None,
        RowKind::Item if filtered => {
            let p = found.get(i)?.path.clone();
            if p.is_empty() {
                None
            } else if r.submenu {
                Some(Act::OpenPath(p))
            } else {
                Some(Act::Pick(p))
            }
        }
        RowKind::Item if r.submenu => Some(Act::Open(lv, r.key, kb)),
        RowKind::Item => {
            let mut path = path_to(st, lv);
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
    st.levels[lv].timer = SubTimer::default();
}

fn open_child(ui: &mut Ui, id: Id, st: &mut MenuState, lv: usize, key: u64, kb: bool) {
    st.levels[lv].timer = SubTimer::default();
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

/// Leaves the filter and opens the submenus down to `path` (its last key is
/// a submenu row), keyboard style: its first row gets the focus.
fn open_path(ui: &mut Ui, id: Id, st: &mut MenuState, path: &[u64]) {
    st.filter.clear();
    st.levels[0].scroll = 0.0;
    close_above(ui, id, st, 0);
    for (d, &k) in path.iter().enumerate() {
        let l = &mut st.levels[d];
        l.focus = Some(k);
        l.kb = true;
        l.reveal = true;
        l.focus_first = false;
        open_child(ui, id, st, d, k, true);
    }
}

// ---- editing (CM8) ---------------------------------------------------------

fn edit_id(id: Id) -> Id {
    id.with(EDIT_POPUP)
}

/// Opens the edit menu of row `key` at level `lv`. False: the row has no
/// edit menu (the app gave no rows).
#[allow(clippy::too_many_arguments)]
fn open_edit(ui: &mut Ui, id: Id, st: &mut MenuState, src: &dyn MenuSource, lv: usize, key: u64, kb: bool, row: Option<Rect>) -> bool {
    let mut path = path_to(st, lv);
    path.push(key);
    let mut er = Vec::new();
    src.edit_rows(&path, &mut er);
    if er.is_empty() {
        return false;
    }
    let row = row.or_else(|| st.edit.as_ref().filter(|e| e.lv == lv && e.path == path).map(|e| e.row)).unwrap_or_default();
    close_above(ui, id, st, lv);
    let eid = edit_id(id);
    if ui.is_popup_open(eid) {
        ui.close_popup(eid);
    }
    st.rename = None;
    let l = &mut st.levels[lv];
    l.focus = Some(key);
    l.kb = kb;
    st.edit = Some(EditMenu { lv, path, focus: if kb { edge_focus(&er, true).map(|i| er[i].key) } else { None }, kb, row, rect: Rect::ZERO, sub: None, sub_row: Rect::ZERO });
    ui.open_popup(eid);
    ui.request_repaint();
    true
}

fn sub_id(id: Id) -> Id {
    edit_id(id).with(1)
}

fn close_edit(ui: &mut Ui, id: Id, st: &mut MenuState) {
    if st.edit.take().is_some() {
        ui.close_popup(sub_id(id));
        ui.close_popup(edit_id(id));
    }
}

/// The flyout of "Move to group" closes; the edit menu stays.
fn close_sub(ui: &mut Ui, id: Id, st: &mut MenuState) {
    if let Some(e) = st.edit.as_mut()
        && e.sub.take().is_some()
    {
        ui.close_popup(sub_id(id));
        ui.request_repaint();
    }
}

/// The flyout rows of the edit menu `e`.
fn sub_rows_of(src: &dyn MenuSource, e: &EditMenu, out: &mut Vec<MenuRow>) {
    let mut p = e.path.clone();
    p.push(EDIT_GROUP);
    out.clear();
    src.edit_rows(&p, out);
}

/// Opens the "Move to group" flyout of the open edit menu (keyboard: its first row has the focus).
fn open_sub(ui: &mut Ui, id: Id, st: &mut MenuState, src: &dyn MenuSource, kb: bool) {
    let Some(e) = st.edit.as_mut() else { return };
    if e.sub.is_some() {
        return;
    }
    let mut sr = Vec::new();
    sub_rows_of(src, e, &mut sr);
    if sr.is_empty() {
        return;
    }
    let focus = if kb { edge_focus(&sr, true).map(|i| sr[i].key) } else { None };
    e.sub = Some(EditSub { focus, kb, rect: Rect::ZERO });
    ui.open_popup(sub_id(id));
    ui.request_repaint();
}

/// Runs edit row `k` of the open edit menu. True: the menu closed.
fn run_edit(ui: &mut Ui, id: Id, st: &mut MenuState, src: &dyn MenuSource, k: u64, resp: &mut MenuResponse) -> bool {
    let Some(e) = st.edit.clone() else { return false };
    let mut er = Vec::new();
    src.edit_rows(&e.path, &mut er);
    if !er.iter().any(|r| r.key == k && r.focusable()) {
        return false;
    }
    let mut rows = Vec::new();
    rows_at(src, st, e.lv, &mut rows);
    let key = e.path.last().copied().unwrap_or(0);
    let label = rows.iter().find(|r| r.key == key).map(|r| r.label.clone()).unwrap_or_default();
    ui.request_repaint();
    match k {
        EDIT_RENAME if st.external_rename => {
            close_edit(ui, id, st);
            resp.edit = Some((e.path, k));
        }
        EDIT_RENAME => {
            close_edit(ui, id, st);
            let mut input = crate::widgets::text_input::TextInput::new(&label);
            input.select_all();
            st.rename = Some(Rename { lv: e.lv, path: e.path, input, orig: label, fresh: true });
        }
        // the row moves under the open edit menu
        EDIT_UP | EDIT_DOWN | EDIT_PIN => resp.edit = Some((e.path, k)),
        EDIT_GROUP => open_sub(ui, id, st, src, e.kb),
        EDIT_SETTINGS => {
            st.edit = None;
            ui.close_all_popups();
            resp.edit = Some((e.path, k));
            resp.closed = true;
            return true;
        }
        EDIT_HIDE | EDIT_DELETE => {
            // the keyboard moves on to a neighbour of the row that goes
            if let Some(i) = rows.iter().position(|r| r.key == key) {
                let next = (i + 1..rows.len()).chain((0..i).rev()).find(|&j| rows[j].kind == RowKind::Item && rows[j].focusable());
                st.levels[e.lv].focus = next.map(|j| rows[j].key);
            }
            close_edit(ui, id, st);
            st.strip = Some(label);
            st.strip_deleted = k == EDIT_DELETE;
            st.strip_manage = false;
            resp.edit = Some((e.path, k));
        }
        _ => {
            close_edit(ui, id, st);
            resp.edit = Some((e.path, k));
        }
    }
    false
}

/// Runs row `k` of the "Move to group" flyout (the edit menu closes).
fn run_edit_sub(ui: &mut Ui, id: Id, st: &mut MenuState, src: &dyn MenuSource, k: u64, resp: &mut MenuResponse) -> bool {
    let Some(e) = st.edit.clone() else { return false };
    let mut sr = Vec::new();
    sub_rows_of(src, &e, &mut sr);
    if !sr.iter().any(|r| r.key == k && r.focusable()) {
        return false;
    }
    close_edit(ui, id, st);
    resp.edit = Some((e.path, k));
    ui.request_repaint();
    false
}

/// A key while the edit menu is open (it takes every key). True: the menu
/// closed.
fn edit_key(ui: &mut Ui, id: Id, st: &mut MenuState, src: &dyn MenuSource, key: Key, mods: Mods, resp: &mut MenuResponse) -> bool {
    let Some(e) = st.edit.clone() else { return false };
    // the flyout has the keys while it is open
    if let Some(sub) = &e.sub {
        let mut sr = Vec::new();
        sub_rows_of(src, &e, &mut sr);
        let cur = index_of(&sr, sub.focus);
        let to = match key {
            Key::Down => next_focus(&sr, cur, true),
            Key::Up => next_focus(&sr, cur, false),
            Key::Home => edge_focus(&sr, true),
            Key::End => edge_focus(&sr, false),
            _ => None,
        };
        match key {
            Key::Enter | Key::Space => {
                if let Some(i) = cur {
                    return run_edit_sub(ui, id, st, src, sr[i].key, resp);
                }
            }
            Key::Escape | Key::Left => close_sub(ui, id, st),
            _ => {}
        }
        if let Some(i) = to
            && let Some(s) = st.edit.as_mut().and_then(|ed| ed.sub.as_mut())
        {
            s.focus = Some(sr[i].key);
            s.kb = true;
            ui.request_repaint();
        }
        return false;
    }
    let mut er = Vec::new();
    src.edit_rows(&e.path, &mut er);
    let cur = index_of(&er, e.focus);
    let mut to = None;
    let mut run = None;
    match key {
        Key::Up if mods.alt => run = Some(EDIT_UP),
        Key::Down if mods.alt => run = Some(EDIT_DOWN),
        Key::Down => to = next_focus(&er, cur, true),
        Key::Up => to = next_focus(&er, cur, false),
        Key::Home => to = edge_focus(&er, true),
        Key::End => to = edge_focus(&er, false),
        Key::Enter | Key::Space => run = cur.map(|i| er[i].key),
        Key::Right if e.focus == Some(EDIT_GROUP) => run = Some(EDIT_GROUP),
        Key::Delete if mods.none() => run = Some(if st.del_deletes { EDIT_DELETE } else { EDIT_HIDE }),
        Key::F(2) if mods.none() => run = Some(EDIT_RENAME),
        Key::Escape | Key::Left => {
            close_edit(ui, id, st);
            st.levels[e.lv].kb = true;
            ui.request_repaint();
        }
        _ => {}
    }
    if let Some(i) = to
        && let Some(ed) = st.edit.as_mut()
    {
        ed.focus = Some(er[i].key);
        ed.kb = true;
        ui.request_repaint();
    }
    match run {
        Some(k) => {
            // the flyout opened by a key is a keyboard flyout
            if k == EDIT_GROUP
                && let Some(ed) = st.edit.as_mut()
            {
                ed.kb = true;
            }
            run_edit(ui, id, st, src, k, resp)
        }
        None => false,
    }
}

/// Undo (false) or Manage (true) on the strip. True: the menu closed.
fn strip_act(ui: &mut Ui, st: &mut MenuState, manage: bool, resp: &mut MenuResponse) -> bool {
    st.strip = None;
    st.strip_manage = false;
    ui.request_repaint();
    if manage {
        ui.close_all_popups();
        resp.manage = true;
        resp.closed = true;
        return true;
    }
    resp.undo = true;
    false
}

/// The strip's Undo and Manage text buttons inside row `r`.
fn strip_parts(c: &mut dyn Canvas, r: Rect, m: &Metrics) -> (Rect, Rect) {
    let mw = c.measure(MANAGE, m.font).ceil() + m.gap;
    let uw = c.measure(UNDO, m.font).ceil() + m.gap;
    let manage = Rect::new(r.r() - 4.0 - mw, r.y + 2.0, mw, r.h - 4.0);
    let undo = Rect::new(manage.x - 2.0 * m.gap - uw, r.y + 2.0, uw, r.h - 4.0);
    (undo, manage)
}

fn set_filter(ui: &mut Ui, id: Id, st: &mut MenuState, f: impl FnOnce(&mut String)) {
    if !st.filtering() {
        // the filter lives in the top popup: submenus close
        close_above(ui, id, st, 0);
        st.filter_w = st.levels[0].rect.w;
    }
    f(&mut st.filter);
    st.filter_reset = true;
    st.levels[0].scroll = 0.0;
    st.levels[0].kb = true;
    ui.request_repaint();
}

/// Rows a page of level `lv` holds (from its last rect).
fn page_rows(st: &MenuState, lv: usize, m: &Metrics) -> usize {
    let h = st.levels[lv].rect.h - 2.0 * m.inset() - st.levels[lv].hdr - 2.0 * st.levels[lv].strip;
    if h <= 0.0 { usize::MAX } else { ((h / m.row).floor() as usize).max(1) }
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
        let l = st.levels.last_mut().unwrap();
        l.open = None;
        l.timer = SubTimer::default();
    }
    // CM8: a press elsewhere in the menu closed the edit menu: that click
    // only dismisses it
    if st.edit.is_some() && !ui.is_popup_open(edit_id(id)) {
        st.edit = None;
        ui.close_popup(sub_id(id));
        st.eat_click = true;
    }
    if st.edit.as_ref().is_some_and(|e| e.lv >= st.levels.len()) {
        close_edit(ui, id, st);
    }
    if st.rename.as_ref().is_some_and(|r| r.lv >= st.levels.len()) {
        st.rename = None;
    }
    ui.capture_keys = true;
    // an inline rename has the keys (its text field), else the menu
    ui.set_focus(Some(if st.rename.is_some() { id.with(RENAME_ID) } else { lid(0) }));
    let t = ui.theme;
    let pointer = ui.mouse_in().then(|| ui.mouse());
    let prev = st.pointer;
    let moved = prev != pointer;
    st.pointer = pointer;
    let m = Metrics::new(&t);
    let now = style.now_ms;
    let mut rows: Vec<MenuRow> = Vec::new();
    let mut found: Vec<Found> = Vec::new();

    // CM10: the screen reader's request runs as the keyboard would (one per
    // frame): the row's level becomes the deepest, the row gets the focus
    // and Enter is typed for it
    let mut evs = ui.key_input(lid(0));
    if !st.a11y_req.is_empty() {
        let (lv, key, what) = st.a11y_req.remove(0);
        if lv < st.levels.len() && !st.editing() {
            close_above(ui, id, st, lv);
            match what {
                A11yReq::Invoke => {
                    let l = &mut st.levels[lv];
                    l.focus = Some(key);
                    l.kb = true;
                    l.reveal = true;
                    evs.insert(0, (usize::MAX, Event::KeyDown { key: Key::Enter, mods: Mods::NONE, repeat: false }));
                }
                A11yReq::Collapse => {}
            }
        }
        ui.request_repaint();
    }
    st.a11y = style.a11y.then(A11ySnap::default);
    // keys go to the deepest level
    for (qi, ev) in evs {
        ui.consume(qi);
        // CM8: the edit menu takes every key while it is open
        if st.edit.is_some() {
            if let Event::KeyDown { key, mods, .. } = ev
                && edit_key(ui, id, st, src, key, mods, &mut resp)
            {
                return resp;
            }
            continue;
        }
        let lv = st.levels.len() - 1;
        let filtering = st.filtering();
        let key = match ev {
            // typed text filters (Space only once a filter is going)
            Event::Char(ch) if !ch.is_control() && (ch != ' ' || filtering) => {
                set_filter(ui, id, st, |s| s.push(ch));
                continue;
            }
            Event::KeyDown { key, mods, .. } => (key, mods),
            _ => continue,
        };
        level_rows(src, st, lv, &mut rows, &mut found);
        // CM9: the folded top level (its common actions are the icon row)
        let mut quick: Vec<MenuRow> = Vec::new();
        if lv == 0 && !filtering && st.levels[0].folded {
            (quick, rows) = fold_quick(&rows);
        }
        let cur = index_of(&rows, st.levels[lv].focus);
        let qcur = index_of(&quick, st.levels[lv].focus);
        let edit_target = cur.filter(|&i| rows[i].kind == RowKind::Item).map(|i| rows[i].key).or(qcur.map(|i| quick[i].key));
        let mut act = None;
        let mut to = None;
        let mut to_key = None;
        if !key.1.alt
            && let Some(k) = quick_nav(&quick, &rows, st.levels[lv].focus, key.0)
        {
            to_key = Some(k);
        } else {
        match key {
            (Key::Char(ch), mods) if mods.alt && !mods.ctrl => {
                // the icon row's accelerators work too (Alt+T cuts)
                let all: Vec<MenuRow> = quick.iter().chain(rows.iter()).cloned().collect();
                let from = qcur.or(cur.map(|i| i + quick.len()));
                if let Some((i, run)) = accel_target(&all, from, ch) {
                    to_key = Some(all[i].key);
                    if run {
                        act = activate(st, &all, &found, lv, i, true);
                    }
                }
            }
            (Key::Enter | Key::Space, _) if qcur.is_some() => act = qcur.and_then(|i| activate(st, &quick, &[], lv, i, true)),
            (Key::Backspace, _) if filtering => set_filter(ui, id, st, |s| {
                s.pop();
            }),
            (Key::Escape, _) if filtering => {
                // first Esc: the whole menu again, focus kept if the row is
                // a top-level one
                st.filter.clear();
                st.levels[0].scroll = 0.0;
                st.levels[0].reveal = true;
                ui.request_repaint();
            }
            // CM8: the edit menu of the focused row (Shift+F10, the Menu key)
            (Key::F(10), m) if m == Mods::SHIFT && !filtering => act = edit_target.map(|k| Act::Edit(lv, k, true)),
            (Key::Other(0x5D), _) if !filtering => act = edit_target.map(|k| Act::Edit(lv, k, true)),
            // the strip's two buttons
            (Key::Left | Key::Right, _) if cur.is_some_and(|i| rows[i].kind == RowKind::Strip) => {
                st.strip_manage = !st.strip_manage;
                ui.request_repaint();
            }
            (Key::Down, _) => to = next_focus(&rows, cur, true),
            (Key::Up, _) => to = next_focus(&rows, cur, false),
            (Key::Home, _) => to = edge_focus(&rows, true),
            (Key::End, _) => to = edge_focus(&rows, false),
            (Key::PageDown, _) => to = page_focus(&rows, cur, page_rows(st, lv, &m), true),
            (Key::PageUp, _) => to = page_focus(&rows, cur, page_rows(st, lv, &m), false),
            (Key::Right, _) => {
                if let Some(i) = cur
                    && rows[i].submenu
                    && !rows[i].disabled
                {
                    act = activate(st, &rows, &found, lv, i, true);
                }
            }
            (Key::Left | Key::Escape, _) if lv > 0 => {
                close_above(ui, id, st, lv - 1);
                st.levels[lv - 1].kb = true;
                st.levels[lv - 1].reveal = true;
                ui.request_repaint();
            }
            (Key::Escape, _) => {
                ui.close_all_popups();
                resp.closed = true;
                return resp;
            }
            (Key::Enter, _) => act = cur.and_then(|i| activate(st, &rows, &found, lv, i, true)),
            (Key::Space, _) if !filtering => act = cur.and_then(|i| activate(st, &rows, &found, lv, i, true)),
            _ => {}
        }
        }
        if let Some(i) = to {
            to_key = Some(rows[i].key);
        }
        if let Some(k) = to_key {
            let l = &mut st.levels[lv];
            l.focus = Some(k);
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
            Some(Act::OpenPath(p)) => open_path(ui, id, st, &p),
            Some(Act::Edit(l, k, kb)) => {
                open_edit(ui, id, st, src, l, k, kb, None);
            }
            Some(Act::Strip(manage)) if strip_act(ui, st, manage, &mut resp) => return resp,
            Some(Act::Strip(_)) => {}
            None => {}
        }
    }

    let mut ys = Vec::new();
    let mut lv = 0;
    while lv < st.levels.len() {
        level_rows(src, st, lv, &mut rows, &mut found);
        // the parent row went away (rows were replaced): close from here
        if lv > 0 {
            let mut up = Vec::new();
            rows_at(src, st, lv - 1, &mut up);
            if !up.iter().any(|r| r.submenu && r.key == st.levels[lv].key) {
                close_above(ui, id, st, lv - 1);
                break;
            }
        }
        let filtering = lv == 0 && st.filtering();
        // CM8: the renamed row went away (rows were replaced)
        if st.rename.as_ref().is_some_and(|rn| rn.lv == lv && !rows.iter().any(|r| rn.path.last() == Some(&r.key))) {
            st.rename = None;
        }
        if filtering {
            // typing focuses the first match; rows arriving later never
            // move the focus
            let keep = !std::mem::take(&mut st.filter_reset) && index_of(&rows, st.levels[0].focus).is_some();
            if !keep {
                st.levels[0].focus = edge_focus(&rows, true).map(|i| rows[i].key);
                st.levels[0].reveal = true;
            }
        }
        // CM9: a top level taller than the bounds folds its common actions
        // into the icon row first; only if it still does not fit, it scrolls
        let mut quick: Vec<MenuRow> = Vec::new();
        let mut w = if filtering { filtered_width(c, &rows, &found, &m).max(st.filter_w).min(m.max_w) } else { width(c, &rows, &m) };
        if lv == 0 && !filtering {
            if !st.levels[0].folded && should_fold(&rows, &m, style.bounds.h - 2.0 * m.keep) {
                st.levels[0].folded = true;
            }
            if st.levels[0].folded {
                (quick, rows) = fold_quick(&rows);
                // the width the plain list had, wide enough for the buttons
                w = w.max(quick.len() as f32 * m.quick_w() + 2.0 * m.inset()).min(m.max_w);
            }
        }
        let content = layout(&rows, &m, &mut ys);
        let hdr = if filtering {
            m.header()
        } else if !quick.is_empty() {
            m.quick_h() + m.pad
        } else {
            0.0
        };
        st.levels[lv].hdr = hdr;
        let full_h = content + hdr + 2.0 * m.inset();
        let last = st.levels[lv].rect;
        let frozen = lv == 0 && last.w > 0.0 && ((ui.mouse_in() && last.contains_pt(ui.mouse())) || st.filtering());
        let rect = if lv > 0 {
            place_sub(st.levels[lv].parent_row, w, full_h, style.bounds, m.keep)
        } else if frozen {
            // rows arriving while the pointer is in the menu (or the filter
            // shrinking it) never move the rows under it: keep the top, grow
            // down, scroll past the edge
            let b = style.bounds.b() - m.keep;
            let x = last.x.min((style.bounds.r() - m.keep - w).max(style.bounds.x + m.keep));
            Rect::new(x, last.y, w, full_h.min((b - last.y).max(m.row + hdr + 2.0 * m.inset())))
        } else {
            place(st.anchor, w, full_h, style.bounds, m.keep)
        };
        st.levels[lv].rect = rect;
        // keyboard-opened submenus focus their first row once it is there
        // (a lazy one shows "Loading…" first)
        if st.levels[lv].focus_first
            && let Some(i) = edge_focus(&rows, true)
        {
            st.levels[lv].focus_first = false;
            st.levels[lv].focus = Some(rows[i].key);
            st.levels[lv].reveal = true;
        }
        // the keyboard's row went away (a submenu that failed to load): the
        // keyboard lands on the first row left
        if st.levels[lv].kb && st.levels[lv].focus.is_some() && index_of(&rows, st.levels[lv].focus).is_none() && index_of(&quick, st.levels[lv].focus).is_none() {
            st.levels[lv].focus = edge_focus(&rows, true).map(|i| rows[i].key);
        }
        if !ui.popup_begin_r(c, lid(lv), rect, m.radius) {
            lv += 1;
            continue;
        }
        let scrolling = full_h > rect.h + 0.5;
        let strip = if scrolling { m.strip } else { 0.0 };
        let top = rect.y + m.inset() + hdr;
        let vp = Rect::new(rect.x + m.inset(), top + strip, rect.w - 2.0 * m.inset(), (rect.h - 2.0 * m.inset() - hdr - 2.0 * strip).max(0.0));
        if filtering {
            let n = found.iter().filter(|f| !f.path.is_empty()).count();
            paint_field(c, &t, &m, Rect::new(rect.x + m.inset(), rect.y + m.inset(), rect.w - 2.0 * m.inset(), m.field_h()), &st.filter, n);
        }
        let qarea = Rect::new(rect.x + m.inset(), rect.y + m.inset(), rect.w - 2.0 * m.inset(), m.quick_h());
        let qrects = quick_rects(qarea, quick.len(), &m);
        if !quick.is_empty() {
            // the line between the icon row and the list
            let y = (qarea.b() + m.pad / 2.0).floor() + 0.5;
            c.line(qarea.x + 6.0, y, qarea.r() - 6.0, y, t.border, 1.0);
        }
        // the wheel over the menu scrolls it (and never reaches the list)
        let wheel = ui.interact(lid(lv).child("wheel"), rect, Sense::hover().wheel()).wheel.1;
        let mut scroll = st.levels[lv].scroll;
        if frozen {
            // Adding chevrons must not shift an existing row under the pointer.
            scroll += strip - st.levels[lv].strip;
        }
        st.levels[lv].strip = strip;
        // keep the keyboard focus visible
        if st.levels[lv].reveal {
            st.levels[lv].reveal = false;
            if let Some(i) = index_of(&rows, st.levels[lv].focus) {
                scroll = scroll_to_show(scroll, ys[i], m.row_h(&rows[i]), vp.h);
            }
        }
        if wheel != 0.0 && scrolling {
            scroll = wheel_scroll(scroll, wheel, 3.0 * m.row, content, vp.h);
            ui.request_repaint();
        }
        scroll = clamp_scroll(scroll, content, vp.h);
        if scrolling {
            let step = 3.0 * m.row;
            let up = Rect::new(vp.x, top, vp.w, strip);
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
        // CM10: the screen reader's copy of this level (only when one listens)
        if st.a11y.is_some() {
            let focus = if lv + 1 == st.levels.len() && st.edit.is_none() { st.levels[lv].focus } else { None };
            let open = st.levels[lv].open;
            let row_of = |row: &MenuRow, r: Rect, visible: bool, quick: bool, prefix: &str| A11yRow {
                key: row.key,
                name: if prefix.is_empty() { row.label.clone() } else { format!("{prefix}{PARENT_SEP}{}", row.label) },
                accel: row.accel_char(),
                shortcut: row.shortcut.clone(),
                rect: r,
                enabled: !row.disabled,
                checked: row.checked,
                radio: row.radio,
                submenu: row.submenu,
                expanded: row.submenu && open == Some(row.key),
                focused: focus == Some(row.key) && row.focusable(),
                visible,
                quick,
            };
            let mut out = A11yLevel {
                key: st.levels[lv].key,
                rect,
                rows: Vec::with_capacity(rows.len() + quick.len()),
                filter: filtering.then(|| (Rect::new(rect.x + m.inset(), rect.y + m.inset(), rect.w - 2.0 * m.inset(), m.field_h()), st.filter.clone())),
            };
            for (row, r) in quick.iter().zip(qrects.iter()) {
                out.rows.push(row_of(row, *r, true, true, ""));
            }
            for (i, row) in rows.iter().enumerate() {
                if matches!(row.kind, RowKind::Separator | RowKind::Header | RowKind::Note | RowKind::Label) {
                    continue;
                }
                let r = Rect::new(vp.x, vp.y + ys[i] - scroll, vp.w, m.row_h(row));
                let prefix = found.get(i).map_or("", |f| f.prefix.as_str());
                out.rows.push(row_of(row, r, r.b() > vp.y && r.y < vp.b(), false, prefix));
            }
            if let Some(snap) = st.a11y.as_mut() {
                snap.levels.push(out);
            }
        }
        if lv == 0
            && let Some(ph) = style.loading
        {
            paint_loading(c, &t, rect, m.radius, ph);
        }

        // hover: the row under the pointer (not under a deeper level)
        let over_deeper = pointer.is_some_and(|p| st.levels[lv + 1..].iter().any(|l| l.rect.contains_pt(p)) || st.edit.as_ref().is_some_and(|e| e.rect.contains_pt(p) || e.sub.as_ref().is_some_and(|s| s.rect.contains_pt(p))));
        let under = pointer.filter(|p| !over_deeper && vp.contains_pt(*p)).and_then(|p| (0..rows.len()).find(|&i| rows[i].kind != RowKind::Separator && p.y >= vp.y + ys[i] - scroll && p.y < vp.y + ys[i] - scroll + m.row_h(&rows[i])));
        // a stationary pointer cannot take the keyboard's focus
        let fresh = moved || !st.levels[lv].kb;
        let hovered = under.filter(|&i| fresh && rows[i].focusable()).map(|i| (rows[i].key, rows[i].submenu && !rows[i].disabled && !filtering));
        // the icon row (CM9): its buttons are rows that never open submenus
        let qunder = pointer.filter(|_| !over_deeper).and_then(|p| quick_hit(&qrects, p));
        let hovered = hovered.or_else(|| qunder.filter(|&i| fresh && quick[i].focusable()).map(|i| (quick[i].key, false)));
        // CM8: while a row is edited the pointer moves no focus and opens
        // no submenu
        let hovered = if st.editing() { None } else { hovered };
        let child = st.levels.get(lv + 1).map(|l| l.rect);
        let open = st.levels[lv].open;
        let (hact, aiming) = st.levels[lv].timer.step(&HoverIn { now, moved, prev, pointer, hovered, open, child });
        let mut act = None;
        if let Some((k, _)) = hovered
            && !aiming
        {
            let l = &mut st.levels[lv];
            if l.focus != Some(k) || l.kb {
                l.focus = Some(k);
                l.kb = false;
                ui.request_repaint();
            }
            st.levels[lv + 1..].iter_mut().for_each(|l| l.kb = false);
        }
        match hact {
            Some(HoverAct::Open(k)) => act = Some(Act::Open(lv, k, false)),
            Some(HoverAct::Close) => {
                close_above(ui, id, st, lv);
                ui.request_repaint();
            }
            None => {}
        }

        let kb_ring = st.levels[lv].kb;
        let mut tip: Option<(Rect, String)> = None;
        // CM8: the row a right click asked to edit, and its rect
        let mut edit_at: Option<Rect> = None;
        let mut rename_done: Option<bool> = None;
        let edited = st.edit.as_ref().filter(|e| e.lv == lv).and_then(|e| e.path.last().copied());
        for (i, (row, r)) in quick.iter().zip(qrects.iter()).enumerate() {
            let it = ui.interact(lid(lv).with(row.key ^ 0x9a1c), *r, Sense::click());
            if it.pressed.is_some() && st.rename.is_some() {
                rename_done = Some(true);
                st.eat_click = true;
            }
            if it.clicked == Some(Button::Left) {
                if !std::mem::take(&mut st.eat_click) {
                    act = activate(st, &quick, &[], lv, i, false).or(act);
                }
            } else if it.clicked == Some(Button::Right) && !st.filtering() {
                act = Some(Act::Edit(lv, row.key, false));
                edit_at = Some(*r);
            }
            if edited == Some(row.key)
                && let Some(e) = st.edit.as_mut()
            {
                e.row = *r;
            }
            let lvl = &st.levels[lv];
            let focused = lvl.focus == Some(row.key) && row.focusable();
            let state = RowPaint { focused, ring: kb_ring && focused, pressed: it.active && !row.disabled, ..Default::default() };
            paint_quick(c, &t, &m, *r, row, state, src);
            if edited == Some(row.key) {
                dashed_rect(c, r.inset(1.0, 1.0), t.accent);
            }
            // icon-only: the label (and its keys) as a tooltip
            if focused || it.hovered {
                let text = if row.shortcut.is_empty() { row.label.clone() } else { format!("{}  {}", row.label, row.shortcut) };
                tip = Some((*r, text));
            }
        }
        c.clip(vp);
        let underline = (st.keyboard && !st.filtering()) || style.alt;
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
            if row.kind == RowKind::Label {
                c.text(&row.label, Rect::new(r.x + m.label_x(), r.y, (r.w - m.label_x() - 8.0).max(0.0), r.h), m.small, t.text_dim);
                continue;
            }
            // CM8: the row being renamed is a text field
            if st.rename.as_ref().is_some_and(|rn| rn.lv == lv && rn.path.last() == Some(&row.key)) {
                let rid = id.with(RENAME_ID);
                let rn = st.rename.as_mut().unwrap();
                if std::mem::take(&mut rn.fresh) {
                    ui.set_focus(Some(rid));
                }
                c.round(r, m.row_radius, t.selected);
                let fr = Rect::new(r.x + m.label_x() - 7.0, r.y + 1.0, (r.w - m.label_x() + 3.0).max(40.0), r.h - 2.0);
                let tr = crate::widgets::text_input::text_input(ui, c, rid, fr, &mut rn.input, &rn.orig);
                ui.mark_text_focus();
                if tr.submitted {
                    rename_done = Some(true);
                } else if tr.cancelled {
                    rename_done = Some(false);
                }
                // keys the field does not use stay in the menu
                for (qi, _) in ui.key_input(rid) {
                    ui.consume(qi);
                }
                continue;
            }
            let it = ui.interact(lid(lv).with(row.key ^ 0x5eed), r.intersect(&vp), Sense::click());
            // a press elsewhere keeps the rename (Explorer's rule) and is
            // eaten
            if it.pressed.is_some() && st.rename.is_some() {
                rename_done = Some(true);
                st.eat_click = true;
            }
            // a click opens a submenu at once; right-click opens the CM8
            // edit menu, never invokes
            if it.clicked == Some(Button::Left) {
                if std::mem::take(&mut st.eat_click) {
                    // it only closed the edit menu / ended a rename
                } else if row.kind == RowKind::Strip {
                    let (_, mg) = strip_parts(c, r, &m);
                    act = Some(Act::Strip(mg.contains_pt(it.click_pos)));
                } else {
                    act = activate(st, &rows, &found, lv, i, false).or(act);
                }
            } else if it.clicked == Some(Button::Right) && !filtering && row.kind == RowKind::Item {
                act = Some(Act::Edit(lv, row.key, false));
                edit_at = Some(r);
            }
            if edited == Some(row.key)
                && let Some(e) = st.edit.as_mut()
            {
                e.row = r;
            }
            let lvl = &st.levels[lv];
            let fd = found.get(i);
            let state = RowPaint {
                focused: lvl.focus == Some(row.key) && row.focusable(),
                ring: kb_ring && lvl.focus == Some(row.key),
                pressed: it.active && row.kind == RowKind::Item && !row.disabled,
                open: lvl.open == Some(row.key),
                underline,
                prefix: fd.map(|f| f.prefix.as_str()).filter(|p| !p.is_empty()),
                hl: fd.and_then(|f| f.hl),
            };
            if row.submenu && state.open && st.levels.len() > lv + 1 {
                st.levels[lv + 1].parent_row = r;
            }
            if row.kind == RowKind::Strip {
                paint_strip(c, &t, &m, r, row, state, st.strip_manage);
            } else {
                paint_row(c, &t, &m, r, row, state, src);
            }
            if edited == Some(row.key) {
                dashed_rect(c, r.inset(0.5, 0.5), t.accent);
            }
        }
        c.unclip();
        ui.popup_end(c);
        if let Some((b, text)) = tip {
            paint_tip(c, &t, &m, b, &text, rect, style.bounds);
        }
        if let Some(keep) = rename_done
            && let Some(rn) = st.rename.take()
        {
            if keep {
                resp.renamed = Some((rn.path.clone(), rn.input.text.trim().to_string()));
            }
            if let Some(l) = st.levels.get_mut(rn.lv) {
                l.focus = rn.path.last().copied();
                l.kb = true;
            }
            ui.set_focus(Some(lid(0)));
            ui.request_repaint();
        }
        if st.eat_click && ui.active().is_none() {
            // the press that closed the edit menu landed on no row
            st.eat_click = false;
        }
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
            Some(Act::OpenPath(p)) => {
                open_path(ui, id, st, &p);
                ui.request_repaint();
                break;
            }
            Some(Act::Edit(l, k, kb)) => {
                if open_edit(ui, id, st, src, l, k, kb, edit_at) {
                    // the edit menu goes above every level: deeper ones closed
                    break;
                }
            }
            Some(Act::Strip(manage)) if strip_act(ui, st, manage, &mut resp) => return resp,
            Some(Act::Strip(_)) => {}
            None => {}
        }
        lv += 1;
    }
    if st.edit.is_some() && paint_edit(ui, c, id, st, src, style, &m, moved, &mut resp) {
        return resp;
    }
    resp
}

/// What one painted edit-menu list reports.
struct EditList {
    /// a row was clicked
    run: Option<u64>,
    /// the pointer moved onto this row
    hover: Option<u64>,
    /// the rect of the [`EDIT_GROUP`] row
    group: Option<Rect>,
}

/// Width, row heights, wrapped notes, note line height and total height of an edit list.
type EditSizes = (f32, Vec<f32>, Vec<Vec<String>>, f32, f32);

fn edit_sizes(c: &mut dyn Canvas, er: &[MenuRow], m: &Metrics) -> EditSizes {
    let w = width(c, er, m);
    let text_w = w - 2.0 * m.inset() - 16.0;
    let line_h = (m.small.size() * 1.4).round();
    let notes: Vec<Vec<String>> = er.iter().map(|r| if r.kind == RowKind::Note { wrap_text(c, &r.label, m.small, text_w) } else { Vec::new() }).collect();
    let hs: Vec<f32> = er.iter().zip(&notes).map(|(r, n)| if r.kind == RowKind::Note { n.len() as f32 * line_h + 10.0 } else { m.row_h(r) }).collect();
    let full_h = hs.iter().sum::<f32>() + 2.0 * m.inset();
    (w, hs, notes, line_h, full_h)
}

/// Paints one edit list (the menu or its flyout) as the popup `pid`. None: the popup is not open.
#[allow(clippy::too_many_arguments)]
fn paint_edit_list(ui: &mut Ui, c: &mut dyn Canvas, pid: Id, rect: Rect, er: &[MenuRow], sizes: &EditSizes, focus: Option<u64>, kb: bool, moved: bool, m: &Metrics, src: &dyn MenuSource) -> Option<EditList> {
    let t = ui.theme;
    let (_, hs, notes, line_h, _) = sizes;
    let text_w = rect.w - 2.0 * m.inset() - 16.0;
    if !ui.popup_begin_r(c, pid, rect, m.radius) {
        return None;
    }
    let mut out = EditList { run: None, hover: None, group: None };
    let (x, rw) = (rect.x + m.inset(), rect.w - 2.0 * m.inset());
    let mut y = rect.y + m.inset();
    for (i, row) in er.iter().enumerate() {
        let r = Rect::new(x, y, rw, hs[i]);
        y += hs[i];
        match row.kind {
            RowKind::Separator => {
                let ly = r.center().y.floor() + 0.5;
                c.line(r.x + 6.0, ly, r.r() - 6.0, ly, t.border, 1.0);
            }
            RowKind::Header => {
                let mut right = r.r() - 8.0;
                if !row.shortcut.is_empty() {
                    let tw = c.measure(&row.shortcut, m.small).ceil() + 12.0 * m.sc;
                    let th = (16.0 * m.sc).round();
                    let tr = Rect::new(right - tw, r.center().y - th / 2.0, tw, th);
                    c.round_stroke(tr, t.radius_sm, t.popup_border, 1.0);
                    c.text(&row.shortcut, tr, m.small.c(), t.text_dim);
                    right = tr.x - m.gap;
                }
                c.text(&row.label, Rect::new(r.x + 8.0, r.y, (right - r.x - 8.0).max(0.0), r.h), m.font.b(), t.text);
            }
            RowKind::Note => {
                for (k, l) in notes[i].iter().enumerate() {
                    c.text(l, Rect::new(r.x + 8.0, r.y + 5.0 + k as f32 * line_h, text_w, *line_h), m.small, t.text_dim);
                }
            }
            _ => {
                let it = ui.interact(pid.with(row.key), r, Sense::click());
                if row.key == EDIT_GROUP {
                    out.group = Some(r);
                }
                if it.hovered && row.focusable() && (moved || !kb) {
                    out.hover = Some(row.key);
                }
                if it.clicked == Some(Button::Left) && row.focusable() {
                    out.run = Some(row.key);
                }
                let focused = focus == Some(row.key) && row.focusable();
                let state = RowPaint { focused, ring: focused && kb, pressed: it.active && !row.disabled, ..Default::default() };
                paint_row(c, &t, m, r, row, state, src);
            }
        }
    }
    ui.popup_end(c);
    Some(out)
}

/// The edit menu (CM8, the mock's state 6a): placed against the edited row
/// like a submenu, with the "Move to group" flyout. True: the menu closed.
#[allow(clippy::too_many_arguments)]
fn paint_edit(ui: &mut Ui, c: &mut dyn Canvas, id: Id, st: &mut MenuState, src: &dyn MenuSource, style: &MenuStyle, m: &Metrics, moved: bool, resp: &mut MenuResponse) -> bool {
    let Some(e) = st.edit.clone() else { return false };
    let mut er = Vec::new();
    src.edit_rows(&e.path, &mut er);
    let mut rows = Vec::new();
    rows_at(src, st, e.lv, &mut rows);
    // the edited row went away (rows replaced, a submenu closed)
    if er.is_empty() || !rows.iter().any(|r| e.path.last() == Some(&r.key)) {
        close_edit(ui, id, st);
        return false;
    }
    // a click elsewhere closed the flyout's popup
    if e.sub.is_some()
        && !ui.is_popup_open(sub_id(id))
        && let Some(ed) = st.edit.as_mut()
    {
        ed.sub = None;
    }
    let sizes = edit_sizes(c, &er, m);
    let rect = place_sub(e.row, sizes.0, sizes.4, style.bounds, m.keep);
    if let Some(ed) = st.edit.as_mut() {
        ed.rect = rect;
    }
    let focus = st.edit.as_ref().and_then(|x| x.focus);
    let Some(out) = paint_edit_list(ui, c, edit_id(id), rect, &er, &sizes, focus, e.kb, moved, m, src) else { return false };
    if let Some(r) = out.group
        && let Some(ed) = st.edit.as_mut()
    {
        ed.sub_row = r;
    }
    if let Some(k) = out.hover {
        if let Some(ed) = st.edit.as_mut()
            && ed.focus != Some(k)
        {
            ed.focus = Some(k);
            ed.kb = false;
            ui.request_repaint();
        }
        if k == EDIT_GROUP {
            open_sub(ui, id, st, src, false);
        } else {
            close_sub(ui, id, st);
        }
    }
    // the flyout
    let mut run_sub = None;
    if let Some(ed) = st.edit.clone()
        && let Some(sub) = ed.sub.clone()
    {
        let mut sr = Vec::new();
        sub_rows_of(src, &ed, &mut sr);
        if sr.is_empty() {
            close_sub(ui, id, st);
        } else {
            let ss = edit_sizes(c, &sr, m);
            let srect = place_sub(ed.sub_row, ss.0, ss.4, style.bounds, m.keep);
            if let Some(s) = st.edit.as_mut().and_then(|x| x.sub.as_mut()) {
                s.rect = srect;
            }
            match paint_edit_list(ui, c, sub_id(id), srect, &sr, &ss, sub.focus, sub.kb, moved, m, src) {
                Some(o) => {
                    if let Some(k) = o.hover
                        && let Some(s) = st.edit.as_mut().and_then(|x| x.sub.as_mut())
                        && s.focus != Some(k)
                    {
                        s.focus = Some(k);
                        s.kb = false;
                        ui.request_repaint();
                    }
                    run_sub = o.run;
                }
                None => {
                    if let Some(x) = st.edit.as_mut() {
                        x.sub = None;
                    }
                }
            }
        }
    }
    if let Some(k) = run_sub {
        return run_edit_sub(ui, id, st, src, k, resp);
    }
    match out.run {
        Some(k) => run_edit(ui, id, st, src, k, resp),
        None => false,
    }
}

/// Settings' row "..." menu: the edit menu of one row on its own (no menu
/// levels). `key` is the row `src.rows(&[])` returns, `rect` the button it
/// opens from. `resp.closed` once it has closed.
#[allow(clippy::too_many_arguments)]
pub fn edit_only(ui: &mut Ui, c: &mut dyn Canvas, id: Id, st: &mut MenuState, src: &dyn MenuSource, style: &MenuStyle, key: u64, rect: Rect) -> MenuResponse {
    let mut resp = MenuResponse::default();
    if !st.opened {
        st.opened = true;
        st.levels = vec![Level { kb: st.keyboard, ..Default::default() }];
        if !open_edit(ui, id, st, src, 0, key, st.keyboard, Some(rect)) {
            resp.closed = true;
            return resp;
        }
    }
    if st.edit.is_some() && !ui.is_popup_open(edit_id(id)) {
        close_edit(ui, id, st);
    }
    if st.edit.is_none() {
        resp.closed = true;
        return resp;
    }
    ui.capture_keys = true;
    ui.set_focus(Some(id.with(0)));
    let pointer = ui.mouse_in().then(|| ui.mouse());
    let moved = st.pointer != pointer;
    st.pointer = pointer;
    for (qi, ev) in ui.key_input(id.with(0)) {
        ui.consume(qi);
        if let Event::KeyDown { key, mods, .. } = ev
            && edit_key(ui, id, st, src, key, mods, &mut resp)
        {
            return resp;
        }
    }
    if st.edit.is_none() {
        resp.closed = true;
        return resp;
    }
    let m = Metrics::new(&ui.theme);
    if paint_edit(ui, c, id, st, src, style, &m, moved, &mut resp) {
        return resp;
    }
    if st.edit.is_none() {
        resp.closed = true;
    }
    resp
}

/// The Undo strip (CM8, after Hide): eye-off glyph, "Hid "Share"", then the
/// accent Undo and the plain Manage text buttons on the right.
fn paint_strip(c: &mut dyn Canvas, t: &Theme, m: &Metrics, r: Rect, row: &MenuRow, s: RowPaint, manage_focus: bool) {
    let mid = r.center().y;
    let ir = Rect::new(r.x + 6.0, mid - m.icon / 2.0, m.icon, m.icon);
    c.text(&G_EYE_OFF.to_string(), ir, f(m.icon * 0.875).icon().c(), t.text_dim);
    let (undo, manage) = strip_parts(c, r, m);
    let lx = r.x + m.label_x();
    c.text(&row.label, Rect::new(lx, r.y, (undo.x - m.gap - lx).max(0.0), r.h), m.font, t.text_dim);
    for (b, label, is_manage) in [(undo, UNDO, false), (manage, MANAGE, true)] {
        if s.focused && manage_focus == is_manage {
            c.round(b, t.radius_sm, t.selected);
            if s.ring {
                c.round_stroke(b, t.radius_sm, t.focus, t.layout_focus_ring_width.max(1.0));
            }
        }
        c.text(label, b, if is_manage { m.font.c() } else { m.font.b().c() }, if is_manage { t.text } else { t.accent });
    }
    let sx = (manage.x - m.gap).round() + 0.5;
    c.line(sx, mid - m.row * 0.25, sx, mid + m.row * 0.25, t.border, 1.0);
}

/// A 1 px dashed outline (the edited row, CM8).
fn dashed_rect(c: &mut dyn Canvas, r: Rect, col: crate::geom::Color) {
    let (dash, gap) = (4.0, 3.0);
    let mut seg = |x0: f32, y0: f32, x1: f32, y1: f32| {
        let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        let (dx, dy) = ((x1 - x0) / len.max(1.0), (y1 - y0) / len.max(1.0));
        let mut d = 0.0;
        while d < len {
            let e = (d + dash).min(len);
            c.line(x0 + dx * d, y0 + dy * d, x0 + dx * e, y0 + dy * e, col, 1.0);
            d += dash + gap;
        }
    };
    seg(r.x, r.y, r.r(), r.y);
    seg(r.r(), r.y, r.r(), r.b());
    seg(r.r(), r.b(), r.x, r.b());
    seg(r.x, r.b(), r.x, r.y);
}

/// Width of the filtered list: parents' labels count too.
fn filtered_width(c: &mut dyn Canvas, rows: &[MenuRow], found: &[Found], m: &Metrics) -> f32 {
    let shown: Vec<MenuRow> = rows
        .iter()
        .zip(found)
        .map(|(r, f)| if f.prefix.is_empty() { r.clone() } else { MenuRow { label: format!("{}{PARENT_SEP}{}", f.prefix, r.label), ..r.clone() } })
        .collect();
    width(c, &shown, m)
}

/// The inline filter field (the mock's state 5d): search glyph, the text
/// with a steady caret (no blink timer), the match count and an Esc cap.
fn paint_field(c: &mut dyn Canvas, t: &Theme, m: &Metrics, r: Rect, text: &str, n: usize) {
    c.round(r, m.row_radius, t.field);
    c.round_stroke(r, m.row_radius, t.focus, 1.0);
    let mid = r.center().y;
    let gs = (14.0 * m.sc).round();
    c.text(&G_SEARCH.to_string(), Rect::new(r.x + 6.0, mid - gs / 2.0, gs, gs), f(gs * 0.9).icon().c(), t.text_dim);
    let mut right = r.r() - 6.0;
    let cap = "Esc";
    let cw = c.measure(cap, m.small).ceil() + 10.0 * m.sc;
    let ch = (16.0 * m.sc).round();
    let cr = Rect::new(right - cw, mid - ch / 2.0, cw, ch);
    c.round_stroke(cr, t.radius_sm, t.popup_border, 1.0);
    c.text(cap, cr, m.small.c(), t.text_dim);
    right = cr.x - m.gap;
    let count = if n == 1 { "1 match".to_string() } else { format!("{n} matches") };
    let nw = c.measure(&count, m.small).ceil() + 1.0;
    c.text(&count, Rect::new(right - nw, r.y, nw, r.h), m.small, t.text_dim);
    right -= nw + m.gap;
    let tx = r.x + 6.0 + gs + 6.0;
    let tr = Rect::new(tx, r.y, (right - tx).max(0.0), r.h);
    c.text(text, tr, m.font, t.text);
    let cx = (tx + c.measure(text, m.font)).min(tr.r()).round() + 0.5;
    let half = (m.font.size() * 0.6).round();
    c.line(cx, mid - half, cx, mid + half, t.text, 1.0);
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
struct RowPaint<'a> {
    focused: bool,
    ring: bool,
    pressed: bool,
    open: bool,
    underline: bool,
    /// filtered list: the parents' labels, dim before the label
    prefix: Option<&'a str>,
    /// filtered list: the matched bytes of the label
    hl: Option<(usize, usize)>,
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
    // CM10 high contrast: a selection filled with the accent (system
    // highlight) takes the colour made for it, not the window text colour
    let on_sel = if s.focused && t.selected == t.accent { t.on_accent } else { t.text };
    let (mut fg, mut ico) = (on_sel, if lit { on_sel } else { t.text_dim });
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
        c.circle(r.x + 4.0, mid, 2.0, if on_sel != t.text { t.on_accent } else { t.accent });
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
    // the accent marks (check, radio dot) must not vanish on an accent-filled selection
    let mark = if s.pressed || on_sel != t.text { t.on_accent } else { t.accent };
    if row.checked {
        c.round(ir, t.radius_sm, t.selected_inactive);
        c.text(&G_CHECK.to_string(), ir, f(m.icon * 0.75).icon().c(), mark);
    } else if row.radio {
        c.circle(ir.center().x, mid, 3.0 * m.sc, mark);
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
        c.text(&G_ARROW.to_string(), ar, f(aw * 0.85).icon().c(), if s.pressed { t.on_accent } else if row.disabled { t.text_disabled } else if on_sel != t.text { on_sel } else { t.text_dim });
        right -= aw + m.gap;
    }
    if !row.shortcut.is_empty() {
        // The backend quantizes layout widths to quarter DIPs. Leave a DIP
        // of slack so rounding cannot ellipsize a precisely measured chord.
        let sw = c.measure(&row.shortcut, m.small).ceil() + 1.0;
        let col = if s.pressed { t.on_accent } else if row.disabled { t.text_disabled } else if on_sel != t.text { on_sel } else { t.text_dim };
        c.text(&row.shortcut, Rect::new(right - sw, r.y, sw, r.h), m.small, col);
        right -= sw + 2.0 * m.gap;
    }
    let font = if row.default { m.font.b() } else { m.font };
    let mut lx = r.x + m.label_x();
    if let Some(pre) = s.prefix {
        let pre = format!("{pre}{PARENT_SEP}");
        let pw = c.measure(&pre, m.font);
        c.text(&pre, Rect::new(lx, r.y, (right - lx).max(0.0), r.h), m.font, if s.pressed { t.on_accent } else { t.text_dim });
        lx += pw;
    }
    let lr = Rect::new(lx, r.y, (right - lx).max(0.0), r.h);
    if let Some((a, b)) = s.hl
        && b <= row.label.len()
        && row.label.is_char_boundary(a)
        && row.label.is_char_boundary(b)
    {
        let (xa, xb) = (c.caret_x(&row.label, font, a), c.caret_x(&row.label, font, b));
        let hr = Rect::new(lx + xa, r.y + 3.0, (xb - xa).min(lr.r() - lx - xa).max(0.0), r.h - 6.0);
        c.round_a(hr, 2.0, t.highlight, 0.55);
    }
    c.text(&row.label, lr, font, fg);
    // only while Alt is held or after a keyboard open (CM4)
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

/// One icon-only button of the folded row (CM9): the row's states, its icon
/// centred (a glyph, the shell bitmap, or the label's first letter when it
/// has neither), danger and disabled colours like the text row.
fn paint_quick(c: &mut dyn Canvas, t: &Theme, m: &Metrics, r: Rect, row: &MenuRow, s: RowPaint, src: &dyn MenuSource) {
    let b = r.inset(2.0, 2.0);
    let rr = m.row_radius;
    if s.pressed {
        c.round(b, rr, t.accent);
    } else if s.focused {
        c.round(b, rr, t.selected);
        if s.ring {
            c.round_stroke(b, rr, t.focus, t.layout_focus_ring_width.max(1.0));
        }
    }
    let mut col = if s.focused { if t.selected == t.accent { t.on_accent } else { t.text } } else { t.text_dim };
    if row.danger {
        col = t.danger;
    }
    if row.disabled {
        col = t.text_disabled;
    }
    if s.pressed {
        col = t.on_accent;
    }
    let side = (m.icon * 1.125).round();
    let ir = Rect::new((b.center().x - side / 2.0).round(), (b.center().y - side / 2.0).round(), side, side);
    let drawn = match row.icon {
        RowIcon::Glyph(g) => {
            c.text(&g.to_string(), ir, f(side).icon().c(), col);
            true
        }
        RowIcon::Image(k) => match src.icon(k) {
            Some((w, h, px)) => {
                c.image(&image_key(k), w, h, px, ir.inset(((side - m.icon) / 2.0).max(0.0), ((side - m.icon) / 2.0).max(0.0)));
                true
            }
            None => false,
        },
        RowIcon::None => false,
    };
    if !drawn && let Some(ch) = row.label.chars().next() {
        c.text(&ch.to_string(), ir, m.font.b().c(), col);
    }
}

/// The icon button's label, a tooltip under button `b` (above it when there
/// is no room), kept inside `menu`'s width and the bounds. No delay, so no
/// timer: it shows while the button is hovered or has the keyboard focus.
fn paint_tip(c: &mut dyn Canvas, t: &Theme, m: &Metrics, b: Rect, text: &str, menu: Rect, bounds: Rect) {
    let w = (c.measure(text, m.small).ceil() + 14.0 * m.sc).min(bounds.w - 2.0 * m.keep);
    let h = (m.row * 0.8).round();
    let x = (b.center().x - w / 2.0).max(menu.x + 2.0).min(menu.r() - 2.0 - w).max(bounds.x + 2.0).min(bounds.r() - 2.0 - w);
    let mut y = b.b() + 4.0;
    if y + h > bounds.b() - 2.0 {
        y = b.y - 4.0 - h;
    }
    let r = Rect::new(x.round(), y.round(), w, h);
    c.round(r, t.radius_sm, t.popup);
    c.round_stroke(r, t.radius_sm, t.popup_border, 1.0);
    c.text(text, r, m.small.c(), t.text);
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
        /// rows of [9, 93] ("Send to › More")
        deep: Vec<MenuRow>,
        sub_state: SubLoad,
    }
    impl MenuSource for Src {
        fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
            match path {
                [] => out.extend(self.top.iter().cloned()),
                [9] => out.extend(self.sub.iter().cloned()),
                [9, 93] => out.extend(self.deep.iter().cloned()),
                _ => {}
            }
        }
        fn icon(&self, _: u64) -> Option<(u32, u32, &[u32])> {
            None
        }
        fn sub_state(&self, _: &[u64]) -> SubLoad {
            self.sub_state
        }
        fn edit_rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
            // Open and Copy are editable, the rest not
            if matches!(path, [1] | [2]) {
                out.extend([
                    MenuRow { kind: RowKind::Header, shortcut: "Windows".into(), ..MenuRow::item(0, "Copy") },
                    MenuRow { shortcut: "Del".into(), ..MenuRow::item(EDIT_HIDE, "Hide") },
                    MenuRow { shortcut: "F2".into(), ..MenuRow::item(EDIT_RENAME, "Rename\u{2026}") },
                    MenuRow { disabled: path == [2], ..MenuRow::item(EDIT_UP, "Move up") },
                    MenuRow::item(EDIT_DOWN, "Move down"),
                    MenuRow::separator(),
                    MenuRow::item(EDIT_SETTINGS, "Edit in Settings\u{2026}"),
                    MenuRow { kind: RowKind::Note, ..MenuRow::item(0, "A Windows item: hiding it only changes Tsubame's menu. Explorer is not touched.") },
                ]);
            }
        }
    }

    fn src() -> Src {
        Src {
            top: vec![MenuRow { default: true, accel: Some(0), ..MenuRow::item(1, "Open") }, MenuRow::separator(), MenuRow { accel: Some(0), ..MenuRow::item(2, "Copy") }, MenuRow { submenu: true, accel: Some(0), ..MenuRow::item(9, "Send to") }, MenuRow { disabled: true, ..MenuRow::item(3, "Paste") }],
            sub: vec![MenuRow::item(91, "Desktop"), MenuRow::item(92, "Mail")],
            deep: Vec::new(),
            sub_state: SubLoad::Ready,
        }
    }

    struct H {
        ui: Ui,
        c: TestCanvas,
        st: MenuState,
        src: Src,
        now: u64,
        alt: bool,
        a11y: bool,
    }
    const ID: Id = Id::new("test.menu");
    impl H {
        fn new(at: Pt, keyboard: bool) -> H {
            H { ui: Ui::default(), c: TestCanvas::new(800.0, 600.0), st: MenuState::new(at, keyboard), src: src(), now: 1000, alt: false, a11y: false }
        }
        fn frame(&mut self) -> MenuResponse {
            self.c.clear();
            assert!(self.c.begin(0));
            self.ui.begin_frame((800.0, 600.0));
            // like the app: the menu sets it again while it is drawn
            self.ui.capture_keys = false;
            let style = MenuStyle { bounds: Rect::new(0.0, 0.0, 800.0, 600.0), loading: None, alt: self.alt, now_ms: self.now, a11y: self.a11y };
            let r = menu(&mut self.ui, &mut self.c, ID, &mut self.st, &self.src, &style);
            self.ui.end_frame();
            self.c.end();
            r
        }
        fn key(&mut self, k: Key) -> MenuResponse {
            self.ui.push(Event::KeyDown { key: k, mods: Mods::NONE, repeat: false });
            self.frame()
        }
        fn alt_key(&mut self, ch: char) -> MenuResponse {
            self.ui.push(Event::KeyDown { key: Key::Char(ch.to_ascii_uppercase()), mods: Mods { alt: true, ..Mods::NONE }, repeat: false });
            self.frame()
        }
        fn typed(&mut self, s: &str) -> MenuResponse {
            for ch in s.chars() {
                self.ui.push(Event::Char(ch));
            }
            self.frame()
        }
        fn hover(&mut self, p: Pt) -> MenuResponse {
            self.ui.push(Event::MouseMove { pos: p });
            self.frame()
        }
        /// Advances the clock and runs a frame (the app's tick).
        fn wait(&mut self, ms: u64) -> MenuResponse {
            self.now += ms;
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
        h.key(Key::Escape);
        assert_eq!(h.st.levels.len(), 1);
        assert!(h.ui.is_popup_open(ID.with(0)));
        h.ui.push(Event::Char('x'));
        h.frame();
        assert!(h.ui.leftover().is_empty(), "typed chars stay in the menu");
        assert_eq!(h.st.filter, "x");
        h.key(Key::Escape);
        assert!(h.st.filter.is_empty() && h.ui.is_popup_open(ID.with(0)), "first Esc clears the filter");
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
        assert_eq!(h.st.levels.len(), 1, "not before 150 ms");
        assert_eq!(h.st.deadline(), Some(h.now + OPEN_MS));
        h.wait(OPEN_MS);
        assert_eq!(h.st.levels.len(), 2, "hover opens the submenu");
        assert_eq!(h.st.deadline(), None);
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
    fn right_click_on_an_entry_never_invokes_it() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        let pos = h.row_rect(0, 0).center();
        h.ui.push(Event::MouseDown { pos, button: Button::Right, mods: Mods::NONE, double: false });
        h.frame();
        h.ui.push(Event::MouseUp { pos, button: Button::Right, mods: Mods::NONE });
        assert!(h.frame().picked.is_none());
    }

    impl H {
        fn rclick(&mut self, p: Pt) -> MenuResponse {
            self.ui.push(Event::MouseMove { pos: p });
            self.frame();
            self.ui.push(Event::MouseDown { pos: p, button: Button::Right, mods: Mods::NONE, double: false });
            self.frame();
            self.ui.push(Event::MouseUp { pos: p, button: Button::Right, mods: Mods::NONE });
            self.frame()
        }
        fn key_m(&mut self, k: Key, mods: Mods) -> MenuResponse {
            self.ui.push(Event::KeyDown { key: k, mods, repeat: false });
            self.frame()
        }
        /// The edit menu's row `key`: its rect (as drawn last frame).
        fn edit_row(&mut self, key: u64) -> Rect {
            let e = self.st.edit.clone().expect("edit menu open");
            let m = m();
            let mut er = Vec::new();
            self.src.edit_rows(&e.path, &mut er);
            let mut y = e.rect.y + m.inset();
            for r in &er {
                let h = if r.kind == RowKind::Note { 0.0 } else { m.row_h(r) };
                if r.key == key && r.kind == RowKind::Item {
                    return Rect::new(e.rect.x + m.inset(), y, e.rect.w - 2.0 * m.inset(), h);
                }
                y += h;
            }
            panic!("no edit row {key:x}");
        }
    }

    #[test]
    fn right_click_opens_the_edit_menu_next_to_the_row_and_hide_shows_the_strip() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        let copy = h.row_rect(0, 2);
        let r = h.rclick(copy.center());
        assert!(r.picked.is_none() && r.edit.is_none() && !r.closed);
        let e = h.st.edit.clone().expect("edit menu");
        assert_eq!(e.path, vec![2]);
        assert_eq!(h.st.levels[0].focus, Some(2));
        assert!(h.ui.is_popup_open(ID.with(EDIT_POPUP)));
        assert!(e.rect.x >= copy.r(), "placed right of the row like a submenu: {:?} {copy:?}", e.rect);
        assert!(h.c.has_text("Hide") && h.c.has_text("Windows") && h.c.has_text("Edit in Settings\u{2026}"));
        // the note wraps inside the popup
        assert!(h.c.has_text("A Windows item: hiding it only"), "note line 1");
        // a right click on a row without edit rows opens nothing
        let send = h.row_rect(0, 3);
        h.rclick(send.center());
        assert!(h.st.edit.is_none(), "Send to has no edit menu");
        h.rclick(copy.center());
        // Hide by click
        let hide = h.edit_row(EDIT_HIDE);
        let r = h.click(hide.center());
        assert_eq!(r.edit, Some((vec![2], EDIT_HIDE)));
        assert!(!r.closed && h.st.edit.is_none() && !h.ui.is_popup_open(ID.with(EDIT_POPUP)));
        assert_eq!(h.st.strip.as_deref(), Some("Copy"));
        h.frame();
        assert!(h.c.has_text("Hid \u{201c}Copy\u{201d}") && h.c.has_text("Undo") && h.c.has_text("Manage"));
        // Undo by keyboard: End lands on the strip, Enter runs Undo
        h.key(Key::End);
        assert_eq!(h.st.levels[0].focus, Some(KEY_STRIP));
        let r = h.key(Key::Enter);
        assert!(r.undo && !r.closed && r.picked.is_none());
        assert!(h.st.strip.is_none());
    }

    #[test]
    fn strip_manage_closes_the_menu() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.frame();
        h.st.strip = Some("Share".into());
        h.frame();
        h.key(Key::End);
        h.key(Key::Right);
        let r = h.key(Key::Enter);
        assert!(r.manage && r.closed && !r.undo);
        // and by mouse
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.st.strip = Some("Share".into());
        h.frame();
        let rows = { let mut v = Vec::new(); level_rows(&h.src, &h.st, 0, &mut v, &mut Vec::new()); v };
        let i = rows.iter().position(|r| r.kind == RowKind::Strip).unwrap();
        let sr = { let m = m(); let mut ys = Vec::new(); layout(&rows, &m, &mut ys); let r = h.st.levels[0].rect; Rect::new(r.x + m.inset(), r.y + m.inset() + ys[i], r.w - 2.0 * m.inset(), m.row) };
        let (undo, _) = strip_parts(&mut h.c, sr, &m());
        let r = h.click(undo.center());
        assert!(r.undo && !r.closed);
    }

    #[test]
    fn keyboard_edit_menu_runs_del_f2_and_alt_arrows_and_esc_closes_only_it() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.frame();
        assert_eq!(h.st.levels[0].focus, Some(1));
        h.key_m(Key::F(10), Mods::SHIFT);
        let e = h.st.edit.clone().expect("Shift+F10 opens the edit menu");
        assert_eq!(e.path, vec![1]);
        assert_eq!(e.focus, Some(EDIT_HIDE), "keyboard: first edit row focused");
        // typed letters do not filter while it is open
        h.typed("x");
        assert!(h.st.filter.is_empty());
        // Esc closes the edit menu, not the menu
        let r = h.key(Key::Escape);
        assert!(!r.closed && h.st.edit.is_none() && h.ui.is_popup_open(ID.with(0)));
        // the Menu key does the same
        h.key(Key::Other(0x5D));
        assert!(h.st.edit.is_some());
        // Alt+Down moves the row; the edit menu stays
        let r = h.key_m(Key::Down, Mods { alt: true, ..Mods::NONE });
        assert_eq!(r.edit, Some((vec![1], EDIT_DOWN)));
        assert!(h.st.edit.is_some());
        // Down / Enter: Rename (row 2 of the edit menu)
        h.key(Key::Down);
        assert_eq!(h.st.edit.as_ref().unwrap().focus, Some(EDIT_RENAME));
        h.key(Key::Enter);
        assert!(h.st.edit.is_none());
        let rn = h.st.rename.clone().expect("inline rename");
        assert_eq!((rn.orig.as_str(), rn.input.selected()), ("Open", "Open"));
        h.frame();
        // the field has the keys: typing replaces the selected name
        h.typed("Launch");
        assert_eq!(h.st.rename.as_ref().unwrap().input.text, "Launch");
        assert!(h.st.filter.is_empty(), "typing in the field does not filter");
        let r = h.key(Key::Enter);
        assert_eq!(r.renamed, Some((vec![1], "Launch".to_string())));
        assert!(h.st.rename.is_none() && !r.closed);
        assert_eq!(h.st.levels[0].focus, Some(1));
        // F2 inside the edit menu, then Esc cancels the rename
        h.key_m(Key::F(10), Mods::SHIFT);
        h.key(Key::F(2));
        assert!(h.st.rename.is_some());
        h.frame();
        let r = h.key(Key::Escape);
        assert!(r.renamed.is_none() && h.st.rename.is_none() && !r.closed && h.ui.is_popup_open(ID.with(0)));
        // Del hides; the keyboard moves on to the next row
        h.key_m(Key::F(10), Mods::SHIFT);
        let r = h.key(Key::Delete);
        assert_eq!(r.edit, Some((vec![1], EDIT_HIDE)));
        assert_eq!(h.st.levels[0].focus, Some(2));
        // a row without edit rows: Shift+F10 does nothing
        h.key(Key::Down);
        assert_eq!(h.st.levels[0].focus, Some(9));
        h.key_m(Key::F(10), Mods::SHIFT);
        assert!(h.st.edit.is_none());
    }

    #[test]
    fn edit_in_settings_closes_and_a_click_that_closes_the_edit_menu_runs_nothing() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        let open = h.row_rect(0, 0);
        h.rclick(open.center());
        assert!(h.st.edit.is_some());
        // a left click on another row only dismisses the edit menu
        let copy = h.row_rect(0, 2);
        let r = h.click(copy.center());
        assert!(r.picked.is_none() && !r.closed, "{r:?}");
        assert!(h.st.edit.is_none());
        // the next click runs it
        let r = h.click(copy.center());
        assert_eq!(r.picked, Some(vec![2]));
        // Edit in Settings closes the whole menu
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        let open = h.row_rect(0, 0);
        h.rclick(open.center());
        let s = h.edit_row(EDIT_SETTINGS);
        let r = h.click(s.center());
        assert_eq!(r.edit, Some((vec![1], EDIT_SETTINGS)));
        assert!(r.closed && h.ui.popups_open() == 0);
    }

    #[test]
    fn a_disabled_edit_row_does_nothing_and_rename_by_click_then_empty_restores() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        let copy = h.row_rect(0, 2);
        h.rclick(copy.center());
        let up = h.edit_row(EDIT_UP);
        let r = h.click(up.center());
        assert!(r.edit.is_none() && h.st.edit.is_some(), "Move up is disabled for Copy");
        let rn = h.edit_row(EDIT_RENAME);
        h.click(rn.center());
        assert!(h.st.rename.is_some());
        h.frame();
        h.key(Key::Backspace);
        let r = h.key(Key::Enter);
        assert_eq!(r.renamed, Some((vec![2], String::new())), "empty: back to the original name");
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
        h.src.sub_state = SubLoad::Loading;
        h.frame();
        assert!(h.c.has_text("Retry"));
        h.key(Key::Up);
        let r = h.key(Key::Enter);
        assert!(r.retry && !r.closed);
        h.key(Key::Up);
        h.key(Key::Right);
        assert!(h.c.has_text(LOADING), "an empty submenu shows Loading…");
        assert_eq!(h.st.levels[1].focus, None, "nothing to focus yet");
        // the children arrive: the keyboard-opened submenu focuses the first
        h.src.sub = vec![MenuRow::item(91, "Desktop")];
        h.src.sub_state = SubLoad::Ready;
        h.frame();
        assert_eq!(h.st.levels[1].focus, Some(91));
        // failed: a Retry row; empty: "(Empty)"
        h.src.sub.clear();
        h.src.sub_state = SubLoad::Failed;
        h.frame();
        assert!(h.c.has_text(SUB_FAILED));
        let r = h.key(Key::Enter);
        assert!(r.retry, "Retry on the submenu failure row");
        h.src.sub_state = SubLoad::Ready;
        h.frame();
        assert!(h.c.has_text(EMPTY));
    }

    // ---- CM4: pure parts

    fn rows4() -> Vec<MenuRow> {
        vec![
            MenuRow { accel: Some(0), ..MenuRow::item(1, "Open") },
            MenuRow { accel: Some(5), ..MenuRow::item(2, "Open with") },
            MenuRow::separator(),
            MenuRow { accel: Some(0), disabled: true, ..MenuRow::item(3, "Cut") },
            MenuRow { accel: Some(0), ..MenuRow::item(4, "Copy") },
            MenuRow::item(5, "No accel"),
            MenuRow { accel: Some(0), ..MenuRow::item(6, "Open in new tab") },
        ]
    }

    #[test]
    fn find_ci_is_case_insensitive_and_returns_byte_ranges() {
        assert_eq!(find_ci("Open with", "WITH"), Some((5, 9)));
        assert_eq!(find_ci("Copy", "co"), Some((0, 2)));
        assert_eq!(find_ci("Copy", "y"), Some((3, 4)));
        assert_eq!(find_ci("Copy", "copy!"), None);
        assert_eq!(find_ci("Copy", ""), None);
        assert_eq!(find_ci("Résumé", "SUM"), Some((3, 6)));
        assert_eq!(find_ci("Größe", "ö"), Some((2, 4)));
    }

    #[test]
    fn accel_runs_a_single_match_and_cycles_shared_letters() {
        let rows = rows4();
        assert_eq!(accel_target(&rows, None, 'C'), Some((4, true)), "disabled Cut is skipped: Copy runs");
        assert_eq!(accel_target(&rows, None, 'w'), Some((1, true)));
        // three rows start with O ('o' of "Open with" is index 0 but its accel is 'w')
        assert_eq!(accel_target(&rows, None, 'o'), Some((0, false)));
        assert_eq!(accel_target(&rows, Some(0), 'o'), Some((6, false)));
        assert_eq!(accel_target(&rows, Some(6), 'o'), Some((0, false)), "wraps");
        assert_eq!(accel_target(&rows, None, 'n'), None, "rows without an accelerator are ignored");
    }

    #[test]
    fn page_focus_moves_by_a_page_and_stops_at_the_ends() {
        let rows = rows4();
        assert_eq!(page_focus(&rows, None, 3, true), Some(0));
        assert_eq!(page_focus(&rows, Some(0), 3, true), Some(1), "rows 2 and 3 are not focusable");
        assert_eq!(page_focus(&rows, Some(1), 3, true), Some(4));
        assert_eq!(page_focus(&rows, Some(4), 10, true), Some(6), "no wrap");
        assert_eq!(page_focus(&rows, Some(6), 10, true), Some(6));
        assert_eq!(page_focus(&rows, Some(6), 3, false), Some(4));
        assert_eq!(page_focus(&rows, Some(4), 2, false), Some(1));
        assert_eq!(page_focus(&rows, Some(1), 10, false), Some(0));
    }

    #[test]
    fn wheel_scroll_steps_and_clamps() {
        assert_eq!(wheel_scroll(0.0, -1.0, 66.0, 1000.0, 300.0), 66.0, "a notch down");
        assert_eq!(wheel_scroll(66.0, 2.0, 66.0, 1000.0, 300.0), 0.0, "clamped at the top");
        assert_eq!(wheel_scroll(650.0, -1.0, 66.0, 1000.0, 300.0), 700.0, "clamped at the end");
        assert_eq!(wheel_scroll(0.0, -1.0, 66.0, 200.0, 300.0), 0.0, "short menus do not scroll");
    }

    #[test]
    fn safe_triangle_covers_the_way_to_the_submenu() {
        let sub = Rect::new(300.0, 100.0, 200.0, 300.0);
        let apex = Pt::new(250.0, 200.0);
        assert!(in_safe_triangle(apex, Pt::new(280.0, 150.0), sub), "heading up-right");
        assert!(in_safe_triangle(apex, Pt::new(299.0, 390.0), sub), "heading down-right");
        assert!(!in_safe_triangle(apex, Pt::new(250.0, 230.0), sub), "straight down");
        assert!(!in_safe_triangle(apex, Pt::new(240.0, 200.0), sub), "moving away");
        // a submenu flipped to the left
        let left = Rect::new(0.0, 100.0, 200.0, 300.0);
        assert!(in_safe_triangle(apex, Pt::new(220.0, 180.0), left));
        assert!(!in_safe_triangle(apex, Pt::new(260.0, 200.0), left));
    }

    fn hin(now: u64, hovered: Option<(u64, bool)>, open: Option<u64>) -> HoverIn {
        HoverIn { now, moved: true, prev: None, pointer: Some(Pt::new(10.0, 10.0)), hovered, open, child: None }
    }

    #[test]
    fn sub_timer_opens_after_150_and_closes_after_250() {
        let mut t = SubTimer::default();
        assert_eq!(t.step(&hin(0, Some((9, true)), None)), (None, false));
        assert_eq!(t.deadline(), Some(OPEN_MS));
        let mut still = hin(149, Some((9, true)), None);
        still.moved = false;
        assert_eq!(t.step(&still).0, None);
        still.now = 150;
        assert_eq!(t.step(&still).0, Some(HoverAct::Open(9)));
        assert_eq!(t.deadline(), None, "no timer once it is open");
        // onto a plain row: closes 250 ms later
        assert_eq!(t.step(&hin(200, Some((2, false)), Some(9))).0, None);
        assert_eq!(t.deadline(), Some(450));
        assert_eq!(t.step(&hin(449, Some((2, false)), Some(9))).0, None);
        assert_eq!(t.step(&hin(450, Some((2, false)), Some(9))).0, Some(HoverAct::Close));
        // back over the open row: nothing pending
        let mut t = SubTimer::default();
        t.step(&hin(0, Some((2, false)), Some(9)));
        t.step(&hin(10, Some((9, true)), Some(9)));
        assert_eq!(t.deadline(), None);
        // another submenu row: it replaces the open one at 150 ms
        let mut t = SubTimer::default();
        t.step(&hin(0, Some((8, true)), Some(9)));
        assert_eq!(t.step(&hin(150, Some((8, true)), Some(9))).0, Some(HoverAct::Open(8)));
        // pointer inside the open child: both timers stop
        let mut t = SubTimer::default();
        t.step(&hin(0, Some((2, false)), Some(9)));
        let mut inside = hin(100, None, Some(9));
        inside.child = Some(Rect::new(0.0, 0.0, 50.0, 50.0));
        t.step(&inside);
        assert_eq!(t.deadline(), None);
    }

    #[test]
    fn sub_timer_keeps_the_submenu_while_aiming_at_it() {
        let mut t = SubTimer::default();
        let i = HoverIn { now: 0, moved: true, prev: Some(Pt::new(250.0, 200.0)), pointer: Some(Pt::new(270.0, 190.0)), hovered: Some((2, false)), open: Some(9), child: Some(Rect::new(300.0, 100.0, 200.0, 300.0)) };
        assert_eq!(t.step(&i), (None, true), "aiming: focus stays");
        assert_eq!(t.deadline(), Some(CLOSE_MS));
        // a pause on the way: the row under the pointer wins after 250 ms
        let mut rest = i;
        rest.moved = false;
        rest.now = CLOSE_MS;
        assert_eq!(t.step(&rest), (Some(HoverAct::Close), false));
    }

    #[test]
    fn filter_rows_search_submenus_with_a_parent_prefix() {
        let mut s = src();
        s.sub.push(MenuRow { submenu: true, ..MenuRow::item(93, "More") });
        s.deep = vec![MenuRow::item(931, "Copy as path")];
        let (mut rows, mut found) = (Vec::new(), Vec::new());
        filter_rows(&s, "co", &mut rows, &mut found);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Copy", "Copy as path"]);
        assert_eq!(found[0].path, [2]);
        assert_eq!(found[0].prefix, "");
        assert_eq!(found[1].path, [9, 93, 931]);
        assert_eq!(found[1].prefix, "Send to \u{203A} More");
        assert_eq!(found[1].hl, Some((0, 2)));
        assert_eq!(rows[0].key, 2, "top-level rows keep their key");
        assert_ne!(rows[1].key, 931);
        filter_rows(&s, "-", &mut rows, &mut found);
        assert!(rows.is_empty(), "separators never match");
    }

    // ---- CM4: the widget

    fn underlines(h: &H) -> usize {
        let t = Theme::dark();
        h.c.ops.iter().filter(|o| matches!(o, crate::testcanvas::Op::Line(.., col, _) if *col != t.border)).count()
    }

    #[test]
    fn underlines_only_with_alt_or_a_keyboard_open() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        assert_eq!(underlines(&h), 0, "mouse open: no underlines");
        h.key(Key::Down);
        assert_eq!(underlines(&h), 0, "arrow keys after a mouse open do not show them");
        h.alt = true;
        h.frame();
        assert_eq!(underlines(&h), 3, "Alt held");
        h.alt = false;
        h.frame();
        assert_eq!(underlines(&h), 0, "Alt released");
        let mut k = H::new(Pt::new(100.0, 100.0), true);
        k.frame();
        assert_eq!(underlines(&k), 3, "keyboard open");
        k.typed("o");
        assert!(!k.c.ops.iter().any(|o| matches!(o, crate::testcanvas::Op::Line(x0, _, x1, _, _, _) if (x1 - x0) > 1.0)), "no underlines while filtering");
    }

    #[test]
    fn alt_letter_runs_or_opens_and_unknown_letters_do_nothing() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        assert_eq!(h.alt_key('c').picked, Some(vec![2]));
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        let r = h.alt_key('s');
        assert!(r.picked.is_none());
        assert_eq!(h.st.levels.len(), 2, "Alt+S opens Send to");
        h.frame();
        assert_eq!(h.st.levels[1].focus, Some(91), "and focuses its first row");
        let r = h.alt_key('z');
        assert!(r.picked.is_none() && !r.closed);
        assert!(h.st.filter.is_empty(), "Alt+letter never types into the filter");
    }

    #[test]
    fn typing_filters_highlights_and_enter_runs_the_first_match() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        h.typed("se");
        assert_eq!(h.st.filter, "se");
        assert!(h.c.has_text("Send to") && !h.c.has_text("Open") && !h.c.has_text("Copy"));
        assert!(h.c.has_text("1 match") && h.c.has_text("Esc"));
        let hl = Theme::dark().highlight;
        assert!(h.c.ops.iter().any(|o| matches!(o, crate::testcanvas::Op::Round(_, _, col, a) if *col == hl && *a < 1.0)), "match highlighted");
        // "o": Open, Copy, Send to, Desktop (Send to › ) — separators gone
        h.key(Key::Backspace);
        h.key(Key::Backspace);
        h.typed("o");
        assert!(h.c.has_text("Open") && h.c.has_text("Copy") && h.c.has_text("Desktop"));
        assert!(h.c.has_text("Send to \u{203A} "), "dim parent prefix");
        assert_eq!(h.st.levels[0].focus, Some(1), "first match focused");
        h.key(Key::Down);
        h.key(Key::Down);
        h.key(Key::Down);
        let r = h.key(Key::Enter);
        assert_eq!(r.picked, Some(vec![9, 91]), "a submenu child runs from the filter");
    }

    #[test]
    fn space_runs_only_without_a_filter_and_no_match_says_so() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.frame();
        h.typed("op");
        h.typed(" ");
        assert_eq!(h.st.filter, "op ");
        h.typed("zz");
        assert!(h.c.has_text(NO_MATCH));
        assert!(h.key(Key::Enter).picked.is_none());
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.frame();
        h.typed(" ");
        assert!(h.st.filter.is_empty(), "a leading space does not start a filter");
    }

    #[test]
    fn filter_keeps_the_focus_while_rows_arrive() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        h.typed("o");
        h.key(Key::Down);
        let f = h.st.levels[0].focus;
        h.src.top.insert(0, MenuRow::item(500, "Shell: Open in Terminal"));
        h.frame();
        assert_eq!(h.st.levels[0].focus, f, "no focus jump as rows arrive");
        h.typed("p");
        assert_eq!(h.st.levels[0].focus, Some(500), "typing focuses the first match again");
    }

    #[test]
    fn filter_enter_on_a_submenu_opens_it_and_left_returns() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        h.typed("send");
        h.key(Key::Enter);
        assert!(h.st.filter.is_empty());
        assert_eq!(h.st.levels.len(), 2);
        h.frame();
        assert_eq!(h.st.levels[1].focus, Some(91));
        h.key(Key::Left);
        assert_eq!(h.st.levels.len(), 1);
        assert_eq!(h.st.levels[0].focus, Some(9));
    }

    #[test]
    fn click_on_a_submenu_row_opens_it_at_once() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        let send = h.row_rect(0, 3).center();
        h.ui.push(Event::MouseMove { pos: send });
        h.frame();
        h.ui.push(Event::MouseDown { pos: send, button: Button::Left, mods: Mods::NONE, double: false });
        h.frame();
        h.ui.push(Event::MouseUp { pos: send, button: Button::Left, mods: Mods::NONE });
        h.frame();
        assert_eq!(h.st.levels.len(), 2, "no 150 ms wait on a click");
    }

    #[test]
    fn hover_close_waits_250_ms_and_idle_has_no_deadline() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        assert_eq!(h.st.deadline(), None, "an idle menu runs no timer");
        h.hover(h.row_rect(0, 3).center());
        h.wait(OPEN_MS);
        assert_eq!(h.st.levels.len(), 2);
        // straight up to Copy: away from the submenu
        h.hover(h.row_rect(0, 2).center());
        assert_eq!(h.st.levels.len(), 2, "still open");
        h.wait(CLOSE_MS - 1);
        assert_eq!(h.st.levels.len(), 2);
        h.wait(1);
        assert_eq!(h.st.levels.len(), 1, "closed after 250 ms");
        assert_eq!(h.st.deadline(), None);
        assert_eq!(h.st.levels[0].focus, Some(2));
    }

    #[test]
    fn wanted_lists_open_and_pending_submenus() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        assert!(h.st.wanted().is_empty());
        h.hover(h.row_rect(0, 3).center());
        assert_eq!(h.st.wanted(), [vec![9]], "asked while the hover delay runs");
        h.wait(OPEN_MS);
        assert_eq!(h.st.wanted(), [vec![9]]);
    }

    #[test]
    fn wheel_scrolls_a_tall_menu_and_page_keys_move() {
        let mut h = H::new(Pt::new(100.0, 10.0), true);
        h.src.top = (0..60).map(|k| MenuRow::item(1000 + k, "Row")).collect();
        h.frame();
        h.frame();
        let r = h.st.levels[0].rect;
        let p = r.center();
        h.hover(p);
        h.ui.push(Event::Wheel { pos: p, dx: 0.0, dy: -1.0, mods: Mods::NONE });
        h.frame();
        assert_eq!(h.st.levels[0].scroll, 3.0 * m().row, "one notch: three rows");
        h.ui.push(Event::MouseLeave);
        h.frame();
        h.key(Key::Home);
        assert_eq!(h.st.levels[0].focus, Some(1000));
        assert_eq!(h.st.levels[0].scroll, 0.0);
        h.key(Key::PageDown);
        let f = h.st.levels[0].focus.unwrap();
        assert!(f > 1005, "PageDown moves a page: {f}");
        h.key(Key::PageUp);
        assert_eq!(h.st.levels[0].focus, Some(1000));
        h.key(Key::End);
        assert_eq!(h.st.levels[0].focus, Some(1059));
    }

    #[test]
    fn esc_closes_one_level_at_a_time_from_a_deep_submenu() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.src.sub.push(MenuRow { submenu: true, ..MenuRow::item(93, "More") });
        h.src.deep = vec![MenuRow::item(931, "Deep")];
        h.frame();
        h.key(Key::End);
        h.key(Key::Right);
        h.key(Key::End);
        h.key(Key::Right);
        assert_eq!(h.st.levels.len(), 3);
        h.frame();
        assert_eq!(h.st.levels[2].focus, Some(931));
        h.key(Key::Escape);
        assert_eq!(h.st.levels.len(), 2);
        assert_eq!(h.st.levels[1].focus, Some(93));
        h.key(Key::Escape);
        assert_eq!(h.st.levels.len(), 1);
        assert!(h.key(Key::Escape).closed);
    }

    // ---- CM9: the icon row

    fn q(key: u64, label: &str) -> MenuRow {
        MenuRow { quick: true, ..MenuRow::item(key, label) }
    }

    fn tall_src(n: u64) -> Vec<MenuRow> {
        let mut v = vec![MenuRow::item(1, "Open"), MenuRow::separator(), q(51, "Cut"), q(52, "Copy"), q(53, "Paste"), q(54, "Rename"), MenuRow { accel: Some(0), ..q(55, "Delete") }, MenuRow::separator()];
        v.extend((0..n).map(|k| MenuRow::item(1000 + k, "Row")));
        v
    }

    #[test]
    fn fold_only_when_too_tall_and_two_rows_can_fold() {
        let m = m();
        let rows = tall_src(5);
        let mut ys = Vec::new();
        let h = layout(&rows, &m, &mut ys) + 2.0 * m.inset();
        assert!(!should_fold(&rows, &m, h), "fits exactly");
        assert!(should_fold(&rows, &m, h - 1.0));
        let one = [MenuRow::item(1, "a"), q(2, "Cut"), MenuRow::item(3, "b")];
        assert!(!should_fold(&one, &m, 1.0), "one foldable row is not worth a row");
        let off = [MenuRow::item(1, "a"), MenuRow { kind: RowKind::Separator, quick: true, ..MenuRow::separator() }, MenuRow::item(3, "b")];
        assert!(!should_fold(&off, &m, 1.0), "only items fold");
    }

    #[test]
    fn folding_keeps_order_and_cleans_separators() {
        let (quick, rest) = fold_quick(&tall_src(2));
        assert_eq!(quick.iter().map(|r| r.key).collect::<Vec<_>>(), [51, 52, 53, 54, 55]);
        let kinds: Vec<_> = rest.iter().map(|r| (r.key, r.kind == RowKind::Separator)).collect();
        assert_eq!(kinds.len(), 4, "{kinds:?}");
        assert_eq!(rest[0].key, 1);
        assert_eq!(rest[1].kind, RowKind::Separator, "one separator, not two");
        assert_eq!((rest[2].key, rest[3].key), (1000, 1001));
        // a foldable row at the very end leaves no trailing separator
        let (_, rest) = fold_quick(&[MenuRow::item(1, "a"), MenuRow::separator(), q(2, "Cut"), q(3, "Copy")]);
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn icon_buttons_are_left_aligned_and_hit_tested() {
        let m = m();
        let area = Rect::new(10.0, 20.0, 300.0, m.quick_h());
        let r = quick_rects(area, 5, &m);
        assert_eq!(r.len(), 5);
        assert_eq!(r[0].x, 10.0);
        assert_eq!(r[1].x, 10.0 + m.quick_w());
        assert!(r.iter().all(|b| b.w == m.quick_w() && b.h == m.quick_h()));
        assert_eq!(quick_hit(&r, Pt::new(11.0, 21.0)), Some(0));
        assert_eq!(quick_hit(&r, r[3].center()), Some(3));
        assert_eq!(quick_hit(&r, Pt::new(r[4].r() + 1.0, 21.0)), None, "right of the buttons");
        assert_eq!(quick_hit(&r, Pt::new(11.0, area.b() + 1.0)), None, "below the row");
        // narrow menu: the buttons shrink to fit
        let r = quick_rects(Rect::new(0.0, 0.0, 100.0, 30.0), 5, &m);
        assert!(r[4].r() <= 100.0 + 0.01);
        assert!(quick_rects(area, 0, &m).is_empty());
    }

    #[test]
    fn arrows_move_between_the_icon_row_and_the_list() {
        let (quick, rows) = fold_quick(&tall_src(3));
        let nav = |f: u64, k: Key| quick_nav(&quick, &rows, Some(f), k);
        assert_eq!(nav(51, Key::Right), Some(52));
        assert_eq!(nav(55, Key::Right), Some(51), "wraps");
        assert_eq!(nav(51, Key::Left), Some(55), "wraps left");
        assert_eq!(nav(53, Key::Down), Some(1), "down into the list");
        assert_eq!(nav(53, Key::Up), Some(1002), "up wraps to the last list row");
        assert_eq!(nav(1, Key::Up), Some(51), "up from the first list row");
        assert_eq!(nav(1002, Key::Down), Some(51), "down from the last list row");
        assert_eq!(nav(1000, Key::Down), None, "the list's business");
        assert_eq!(nav(1000, Key::Home), Some(51));
        assert_eq!(quick_nav(&[], &rows, Some(1), Key::Up), None, "no row");
        let dis: Vec<MenuRow> = quick.iter().map(|r| MenuRow { disabled: true, ..r.clone() }).collect();
        assert_eq!(quick_nav(&dis, &rows, Some(1), Key::Up), None, "all disabled");
    }

    fn quick_button(h: &H, i: usize) -> Rect {
        let m = m();
        let r = h.st.levels[0].rect;
        let area = Rect::new(r.x + m.inset(), r.y + m.inset(), r.w - 2.0 * m.inset(), m.quick_h());
        quick_rects(area, 5, &m)[i]
    }

    #[test]
    fn tall_menu_folds_and_the_icon_row_works_by_keyboard_and_mouse() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.src.top = tall_src(60);
        h.frame();
        assert!(h.st.levels[0].folded);
        assert!(h.st.levels[0].hdr >= m().quick_h());
        assert_eq!(h.st.levels[0].focus, Some(1), "keyboard open focuses the first list row");
        h.key(Key::Up);
        assert_eq!(h.st.levels[0].focus, Some(51));
        h.key(Key::Right);
        assert_eq!(h.st.levels[0].focus, Some(52));
        assert_eq!(h.key(Key::Enter).picked, Some(vec![52]));

        // accelerators reach the folded rows
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.src.top = tall_src(60);
        h.frame();
        assert_eq!(h.alt_key('d').picked, Some(vec![55]));

        // a click on an icon button picks it; hovering shows its label
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.src.top = tall_src(60);
        h.frame();
        let b = quick_button(&h, 1);
        h.hover(b.center());
        assert!(h.c.has_text("Copy"), "tooltip");
        assert_eq!(h.click(quick_button(&h, 3).center()).picked, Some(vec![54]));

        // still too tall: the list scrolls under the row with chevron strips
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.src.top = tall_src(60);
        h.frame();
        assert!(h.c.has_text("\u{E70D}"), "down strip");
        h.key(Key::End);
        assert!(h.st.levels[0].scroll > 0.0);
        assert_eq!(h.st.levels[0].focus, Some(1059));
        h.key(Key::Down);
        assert_eq!(h.st.levels[0].focus, Some(51), "down from the last row enters the icon row");
    }

    #[test]
    fn short_menu_does_not_fold() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.src.top = tall_src(3);
        h.frame();
        assert!(!h.st.levels[0].folded);
        assert!(h.c.has_text("Cut") && h.c.has_text("Rename"));
    }

    #[test]
    fn screen_reader_snapshot_exists_only_when_asked() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.frame();
        assert!(h.st.a11y.is_none(), "nobody listens: no snapshot work");
        h.a11y = true;
        h.src.top[2].checked = true;
        h.frame();
        let snap = h.st.a11y.clone().unwrap();
        assert_eq!(snap.levels.len(), 1);
        let names: Vec<&str> = snap.levels[0].rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Open", "Copy", "Send to", "Paste"], "separator not listed");
        let rows = &snap.levels[0].rows;
        assert!(rows[0].focused && rows[0].visible && rows[0].enabled);
        assert!(rows[1].checked && !rows[0].checked);
        assert!(rows[2].submenu && !rows[2].expanded);
        assert!(!rows[3].enabled);
        assert_eq!(rows[0].accel, Some('o'));
        assert!(snap.levels[0].rect.contains_pt(rows[0].rect.center()));
        h.a11y = false;
        h.frame();
        assert!(h.st.a11y.is_none(), "listener gone: snapshot dropped");
    }

    #[test]
    fn snapshot_follows_submenus_and_focus() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.a11y = true;
        h.frame();
        h.key(Key::Down);
        h.key(Key::Down);
        h.key(Key::Right);
        let snap = h.st.a11y.clone().unwrap();
        assert_eq!(snap.levels.len(), 2);
        assert_eq!(snap.levels[1].key, 9);
        assert!(snap.levels[0].rows[2].expanded);
        assert!(!snap.levels[0].rows[2].focused, "the focus is in the deepest level");
        assert!(snap.levels[1].rows[0].focused);
        assert_eq!(snap.levels[1].rows[0].name, "Desktop");
    }

    #[test]
    fn scrolled_out_rows_are_offscreen_and_the_filter_is_a_field() {
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.a11y = true;
        h.src.top = tall_src(60);
        h.frame();
        h.key(Key::End);
        let snap = h.st.a11y.clone().unwrap();
        let rows = &snap.levels[0].rows;
        assert!(rows.iter().any(|r| r.quick), "icon row buttons are listed");
        assert!(rows.iter().any(|r| !r.visible), "scrolled out rows are off-screen");
        assert!(rows.iter().any(|r| r.visible && !r.quick));
        let mut h = H::new(Pt::new(100.0, 100.0), true);
        h.a11y = true;
        h.frame();
        h.typed("des");
        let snap = h.st.a11y.clone().unwrap();
        let f = snap.levels[0].filter.clone().unwrap();
        assert_eq!(f.1, "des");
        assert!(snap.levels[0].rows.iter().any(|r| r.name.contains("Desktop") && r.name.starts_with("Send to")), "parents lead the name");
    }

    #[test]
    fn screen_reader_invoke_runs_the_row_like_enter() {
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.a11y = true;
        h.frame();
        h.st.request(0, 2, A11yReq::Invoke);
        assert!(h.st.has_requests());
        let r = h.frame();
        assert_eq!(r.picked, Some(vec![2]));
        assert!(r.closed);

        // a submenu row opens its submenu; Collapse closes it again
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.a11y = true;
        h.frame();
        h.st.request(0, 9, A11yReq::Invoke);
        h.frame();
        assert_eq!(h.st.levels.len(), 2);
        h.st.request(0, 9, A11yReq::Collapse);
        h.frame();
        assert_eq!(h.st.levels.len(), 1);

        // a row of a deeper level closes what is above it first
        let mut h = H::new(Pt::new(100.0, 100.0), false);
        h.frame();
        h.st.request(0, 9, A11yReq::Invoke);
        h.frame();
        h.st.request(1, 92, A11yReq::Invoke);
        let r = h.frame();
        assert_eq!(r.picked, Some(vec![9, 92]));
    }

    // ---- Settings' row menu: `edit_only` and the "Move to group" flyout

    struct Row1 {
        /// the row has Delete entry
        custom: bool,
    }
    impl MenuSource for Row1 {
        fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
            if path.is_empty() {
                out.push(MenuRow::item(1, "Share"));
            }
        }
        fn icon(&self, _: u64) -> Option<(u32, u32, &[u32])> {
            None
        }
        fn edit_rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
            match path {
                [1] => {
                    out.extend([
                        MenuRow::item(EDIT_HIDE, "Hide"),
                        MenuRow::item(EDIT_RENAME, "Rename\u{2026}"),
                        MenuRow { disabled: true, ..MenuRow::item(EDIT_UP, "Move up") },
                        MenuRow { submenu: true, ..MenuRow::item(EDIT_GROUP, "Move to group") },
                    ]);
                    if self.custom {
                        out.push(MenuRow { danger: true, ..MenuRow::item(EDIT_DELETE, "Delete entry") });
                    }
                    out.push(MenuRow { kind: RowKind::Note, ..MenuRow::item(0, "why") });
                }
                [1, EDIT_GROUP] => out.extend([MenuRow { radio: true, ..MenuRow::item(EDIT_GROUP_BASE, "Tools") }, MenuRow::item(EDIT_GROUP_NEW, "New group\u{2026}")]),
                _ => {}
            }
        }
    }

    struct E {
        ui: Ui,
        c: TestCanvas,
        st: MenuState,
        src: Row1,
    }
    impl E {
        fn new(custom: bool, keyboard: bool) -> E {
            let mut st = MenuState::new(Pt::new(300.0, 100.0), keyboard);
            st.del_deletes = true;
            st.external_rename = true;
            E { ui: Ui::default(), c: TestCanvas::new(800.0, 600.0), st, src: Row1 { custom } }
        }
        fn frame(&mut self) -> MenuResponse {
            self.c.clear();
            assert!(self.c.begin(0));
            self.ui.begin_frame((800.0, 600.0));
            let style = MenuStyle { bounds: Rect::new(0.0, 0.0, 800.0, 600.0), loading: None, alt: false, now_ms: 1000, a11y: false };
            let r = edit_only(&mut self.ui, &mut self.c, ID, &mut self.st, &self.src, &style, 1, Rect::new(760.0, 90.0, 24.0, 22.0));
            self.ui.end_frame();
            self.c.end();
            r
        }
        fn key(&mut self, k: Key) -> MenuResponse {
            self.ui.push(Event::KeyDown { key: k, mods: Mods::NONE, repeat: false });
            self.frame()
        }
    }

    #[test]
    fn edit_only_opens_with_the_first_row_focused_and_skips_disabled_rows() {
        let mut e = E::new(false, true);
        assert_eq!(e.frame(), MenuResponse::default());
        assert_eq!(e.st.edit.as_ref().and_then(|x| x.focus), Some(EDIT_HIDE));
        e.key(Key::Down);
        e.key(Key::Down);
        // Move up is disabled: the focus goes from Rename to Move to group
        assert_eq!(e.st.edit.as_ref().and_then(|x| x.focus), Some(EDIT_GROUP));
        e.key(Key::Up);
        e.key(Key::Up);
        assert_eq!(e.st.edit.as_ref().and_then(|x| x.focus), Some(EDIT_HIDE));
        // Enter runs the focused row
        let r = e.key(Key::Enter);
        assert_eq!(r.edit, Some((vec![1], EDIT_HIDE)));
    }

    #[test]
    fn edit_only_escape_closes_and_rename_is_reported_not_inlined() {
        let mut e = E::new(false, true);
        e.frame();
        let r = e.key(Key::F(2));
        assert_eq!(r.edit, Some((vec![1], EDIT_RENAME)), "the app has its own field");
        assert!(e.st.rename.is_none());
        let mut e = E::new(false, true);
        e.frame();
        assert!(e.key(Key::Escape).closed);
    }

    #[test]
    fn edit_only_del_deletes_a_custom_row_and_does_nothing_on_the_others() {
        let mut e = E::new(true, true);
        e.frame();
        assert_eq!(e.key(Key::Delete).edit, Some((vec![1], EDIT_DELETE)));
        // no Delete entry row: Del is not a hide either
        let mut e = E::new(false, true);
        e.frame();
        let r = e.key(Key::Delete);
        assert_eq!(r.edit, None);
        assert!(e.st.edit.is_some(), "the menu stays open");
    }

    #[test]
    fn the_move_to_group_flyout_opens_with_right_and_picks_with_enter() {
        let mut e = E::new(false, true);
        e.frame();
        e.key(Key::Down);
        e.key(Key::Down);
        assert_eq!(e.st.edit.as_ref().and_then(|x| x.focus), Some(EDIT_GROUP));
        e.key(Key::Right);
        assert!(e.st.edit.as_ref().is_some_and(|x| x.sub.is_some()), "the flyout is open");
        // Left closes only the flyout
        e.key(Key::Left);
        assert!(e.st.edit.as_ref().is_some_and(|x| x.sub.is_none()));
        e.key(Key::Right);
        e.key(Key::Down);
        let r = e.key(Key::Enter);
        assert_eq!(r.edit, Some((vec![1], EDIT_GROUP_NEW)));
        assert!(e.st.edit.is_none(), "a pick closes the menu");
        // Esc in the flyout closes it, the menu stays
        let mut e = E::new(false, true);
        e.frame();
        e.key(Key::Down);
        e.key(Key::Down);
        e.key(Key::Right);
        e.key(Key::Escape);
        assert!(e.st.edit.as_ref().is_some_and(|x| x.sub.is_none()));
    }

    #[test]
    fn a_disabled_row_cannot_be_run() {
        let mut e = E::new(false, true);
        e.frame();
        // jump the focus onto the disabled row by hand: Enter still does nothing
        e.st.edit.as_mut().unwrap().focus = Some(EDIT_UP);
        let r = e.key(Key::Enter);
        assert_eq!(r.edit, None);
    }
}
