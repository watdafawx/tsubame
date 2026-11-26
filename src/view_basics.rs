//! B10-A Explorer basics, the view's side: Ctrl+wheel / Ctrl+Plus / Ctrl+Minus zoom (a
//! continuous grid size that crosses over into Details), type-ahead jump, invert selection,
//! the column header's context menu and "fit to content", and "show file extensions".
//!
//! Nothing here runs on a timer: the wheel and typing buffers are plain fields that a later
//! event looks at (`Instant` compared on use), so an idle window stays idle.

use super::*;
use crate::cols::{MIN_W, NUM_COLS};
use crate::overlay::{self, MenuItem};
use std::time::{Duration, Instant};
use tsubame_config::ViewConfig;

/// Wheel notches (a mouse notch is 1.0) that make one zoom step of `ViewConfig::GRID_STEP` DIPs.
/// A notch is two steps; a touchpad's small deltas add up until they reach this.
pub const WHEEL_PER_STEP: f32 = 0.5;
/// A wheel gesture that pauses this long starts again from zero.
pub const WHEEL_IDLE: Duration = Duration::from_millis(500);
/// Letters typed within this long of each other make one search.
pub const TYPE_AHEAD: Duration = Duration::from_millis(500);
/// Widest a column grows when fitted (DIPs).
pub const FIT_CAP: f32 = 600.0;
/// Rows looked at when fitting (the loaded rows from the top; more would only cost time).
const FIT_ROWS: usize = 2000;
/// Cells at or below this width are the "small" grid, above it the "large" one (the toolbar icon).
const SMALL_MAX: u32 = 120;
/// Presets of the `view.grid-small` / `view.grid-large` commands (the old fixed sizes).
pub const PRESET_SMALL: u32 = 96;
pub const PRESET_LARGE: u32 = 152;

const ID_COLMENU: Id = Id::new("col-menu");

/// (cell width, cell height, picture box, type icon) in DIPs of a grid whose cell is `cell` wide.
/// 96 and 152 give the old small (96, 118, 64, 48) and large (152, 182, 128, 64) cells; the
/// picture box takes what the sides leave (`cell - pad`), the cell is the box, a 6 DIP gap, two
/// label lines and 8 DIP of padding.
pub fn grid_dims(cell: f32) -> (f32, f32, f32, f32) {
    let pad = if cell <= 96.0 { 16.0 + (cell - 48.0).max(0.0) / 3.0 } else { 32.0 - 8.0 * ((cell - 96.0) / 56.0).clamp(0.0, 1.0) };
    let boxd = (cell - pad).round().max(24.0);
    let icon = (((48.0 + (boxd - 64.0) * 0.25) / 8.0).round() * 8.0).clamp(16.0, boxd);
    (cell, boxd + 54.0, boxd, icon)
}

/// The view mode a grid of this cell width is called.
pub fn grid_mode_for(size: u32) -> ViewMode {
    if size <= SMALL_MAX { ViewMode::GridSmall } else { ViewMode::GridLarge }
}

/// One zoom step (`dir` > 0 bigger, < 0 smaller) from `mode` at grid size `size`: the new mode
/// and size, or `None` when there is nowhere to go (Details at the small end, the largest grid,
/// Tree and Miller). Below the smallest grid the view becomes Details; one step up from Details
/// is the smallest grid.
pub fn zoom_step(mode: ViewMode, size: u32, dir: i32) -> Option<(ViewMode, u32)> {
    match mode {
        ViewMode::Details => (dir > 0).then_some((grid_mode_for(ViewConfig::GRID_MIN), ViewConfig::GRID_MIN)),
        ViewMode::GridSmall | ViewMode::GridLarge => {
            if dir > 0 {
                let s = (size + ViewConfig::GRID_STEP).min(ViewConfig::GRID_MAX);
                (s != size).then_some((grid_mode_for(s), s))
            } else if size <= ViewConfig::GRID_MIN {
                Some((ViewMode::Details, size))
            } else {
                let s = size - ViewConfig::GRID_STEP;
                Some((grid_mode_for(s), s))
            }
        }
        ViewMode::Tree | ViewMode::Miller => None,
    }
}

/// The first entry (view order) whose name starts with `typed`, case-insensitive, like Explorer:
/// one letter (or the same letter again and again) looks from the entry after the cursor so
/// repeats cycle through the entries of that letter; a longer text looks from the cursor itself.
/// `have_sel`: something is selected (otherwise the first match from the cursor wins at once).
pub fn type_ahead_find(store: &EntryStore, typed: &str, cursor: usize, have_sel: bool) -> Option<usize> {
    type_ahead_seq(store, typed, cursor, have_sel, store.view().len(), Some)
}

/// `type_ahead_find` over grouped rows: only entries on screen (collapsed groups are skipped),
/// in the order the groups show them. `cursor` is an entry.
pub fn type_ahead_grouped(store: &EntryStore, g: &dyn tsubame_ui::GroupRows, typed: &str, cursor: usize, have_sel: bool) -> Option<usize> {
    let from = g.item_row(cursor).unwrap_or(0);
    type_ahead_seq(store, typed, from, have_sel, g.row_count(), |r| match g.row_at(r) {
        tsubame_ui::GroupRow::Item(i) => Some(i),
        tsubame_ui::GroupRow::Header(_) => None,
    })
}

/// The search of `type_ahead_find` over `len` places in some order: `cursor` is a place and
/// `entry` maps a place to the entry (position in the view) it shows, if any.
fn type_ahead_seq(store: &EntryStore, typed: &str, cursor: usize, have_sel: bool, len: usize, entry: impl Fn(usize) -> Option<usize>) -> Option<usize> {
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    let typed: Vec<char> = typed.chars().map(lower).collect();
    let (&first, _) = typed.split_first()?;
    if len == 0 {
        return None;
    }
    let cycling = typed.len() > 1 && typed.iter().all(|c| *c == first);
    let want: &[char] = if cycling { &typed[..1] } else { &typed };
    let start = if (typed.len() == 1 || cycling) && have_sel { cursor + 1 } else { cursor };
    (0..len).map(|k| (start + k) % len).filter_map(&entry).find(|&j| {
        let Some(name) = store.at(j).and_then(|id| store.name(id)) else { return false };
        let mut it = char::decode_utf16(name.iter().copied()).map(|c| lower(c.unwrap_or('\u{fffd}')));
        want.iter().all(|w| it.next() == Some(*w))
    })
}

/// What the view keeps for these features. Plain data, nothing timed.
#[derive(Default)]
pub struct Basics {
    /// wheel notches not yet worth a step, and when the last one came
    wheel_acc: f32,
    wheel_at: Option<Instant>,
    /// the letters typed so far and when the last one came
    typed: String,
    typed_at: Option<Instant>,
    /// a header edge was double-clicked: the held button must not drag the edge back
    fit_hold: bool,
    /// the column menu is open: (pane slot, where it opened)
    col_menu: Option<(usize, Pt)>,
    /// "Size all columns to fit" was picked: the pane in this slot does it while drawing
    fit_pending: Option<usize>,
}

impl Basics {
    /// Adds `dy` wheel notches (+ = away from the user) and returns the whole zoom steps they
    /// make (+ bigger). Small deltas (a touchpad) wait here; a pause or a turn around starts over.
    pub fn wheel_steps(&mut self, dy: f32, now: Instant) -> i32 {
        let stale = self.wheel_at.is_none_or(|t| now.duration_since(t) > WHEEL_IDLE);
        if stale || (self.wheel_acc != 0.0 && self.wheel_acc.signum() != dy.signum()) {
            self.wheel_acc = 0.0;
        }
        self.wheel_at = Some(now);
        self.wheel_acc += dy;
        let steps = (self.wheel_acc / WHEEL_PER_STEP).trunc();
        self.wheel_acc -= steps * WHEEL_PER_STEP;
        steps as i32
    }

    /// Adds a typed letter; the text so far (a pause of `TYPE_AHEAD` starts a new one).
    pub fn type_push(&mut self, c: char, now: Instant) -> &str {
        if self.typed_at.is_none_or(|t| now.duration_since(t) > TYPE_AHEAD) {
            self.typed.clear();
        }
        self.typed_at = Some(now);
        self.typed.push(c);
        &self.typed
    }
}

impl View {
    // ---- zoom ----------------------------------------------------------------------------

    /// Applies the config's `[view]` section to what draws (names without extensions).
    pub(super) fn sync_basics(&mut self) {
        data::set_show_extensions(self.prefs.view.show_extensions);
        crate::view_group::set_default(tsubame_core::grouping::GroupBy::parse(&self.prefs.view.group_by).unwrap_or_default());
        self.refresh_colset();
    }

    /// The columns of the registry in the user's order (`[columns] order`).
    pub(super) fn refresh_colset(&mut self) {
        let mut set = ColSet::current();
        set.reorder_by(&self.prefs.columns.order);
        self.colset = set;
    }

    /// A header cell was dropped: column `from` of the columns showing goes before column `gap`
    /// (`shown.len()` is the end). The order is saved in `[columns] order`.
    pub(super) fn reorder_columns(&mut self, shown: &[Col], from: usize, gap: usize) {
        let (Some(&col), true) = (shown.get(from), gap <= shown.len()) else { return };
        let before = shown.get(gap).copied();
        let next = crate::cols::move_column(self.colset.order(), col, before);
        let ids = crate::cols::order_ids(&next);
        if ids == crate::cols::order_ids(self.colset.order()) {
            return;
        }
        self.prefs.columns.order = ids;
        self.refresh_colset();
        self.config_dirty = true;
    }

    /// "Reset columns": the default order and widths.
    pub fn reset_columns(&mut self) {
        self.prefs.columns.order.clear();
        self.cols = crate::cols::Columns::default();
        self.refresh_colset();
        self.config_dirty = true;
    }

    /// A view mode picked by name (`view.grid-small`, the toolbar button): the grid modes bring
    /// their old fixed cell sizes.
    pub fn set_mode_preset(&mut self, mode: ViewMode) {
        let size = match mode {
            ViewMode::GridSmall => Some(PRESET_SMALL),
            ViewMode::GridLarge => Some(PRESET_LARGE),
            _ => None,
        };
        if let Some(s) = size
            && self.prefs.view.grid_size != s
        {
            self.prefs.view.grid_size = s;
            self.config_dirty = true;
        }
        self.set_mode(mode);
        self.remember_folder_view();
    }

    /// Ctrl+wheel over a list or grid: `dy` notches this frame.
    pub(super) fn zoom_wheel(&mut self, ui: &mut Ui, dy: f32) {
        if dy == 0.0 {
            return;
        }
        let steps = self.basics.wheel_steps(dy, Instant::now());
        if steps != 0 {
            self.zoom_by(steps);
            ui.request_repaint();
        }
    }

    /// `steps` zoom steps (+ bigger). Crossing between Details and the grids ends the run, so a
    /// notch from Details lands on the smallest grid and one from the smallest grid on Details.
    pub fn zoom_by(&mut self, steps: i32) {
        let dir = steps.signum();
        for _ in 0..steps.unsigned_abs() {
            let Some((mode, size)) = zoom_step(self.mode, self.prefs.view.grid_size, dir) else { break };
            if size != self.prefs.view.grid_size {
                self.prefs.view.grid_size = size;
                self.config_dirty = true;
            }
            let crossing = mode != ViewMode::GridSmall && mode != ViewMode::GridLarge || !self.mode.is_grid();
            if mode != self.mode {
                self.set_mode(mode);
            }
            if crossing {
                break;
            }
        }
        self.remember_folder_view();
    }

    // ---- type-ahead ----------------------------------------------------------------------

    /// Letters typed in the list (nothing else took them): jump to the first name that starts
    /// with what was typed in the last 500 ms. Space is left alone (it selects / opens Quick
    /// Look). True when it did something (repaint).
    pub(super) fn type_ahead_input(&mut self, typed: &[char]) -> bool {
        let mut did = false;
        for &c in typed {
            if c.is_control() || c == ' ' {
                continue;
            }
            let text = self.basics.type_push(c, Instant::now()).to_string();
            let t = self.tab();
            let found = match t.group.rows() {
                Some(g) => type_ahead_grouped(&t.store, g, &text, t.cursor_row(), t.sel.count() > 0),
                None => type_ahead_find(&t.store, &text, t.cursor_row(), t.sel.count() > 0),
            };
            if let Some(j) = found {
                self.jump_to(j);
            }
            did = true;
        }
        did
    }

    /// Cursor and a single selection on view row `j`, scrolled into view (either layout).
    fn jump_to(&mut self, j: usize) {
        let grid = self.mode.is_grid();
        let t = self.tab_mut();
        t.move_cursor_to(grid, j);
        select_entry(&t.store, &mut t.sel, j, Mods::NONE, SelectBy::Key);
    }

    // ---- commands ------------------------------------------------------------------------

    /// The B10-A commands. True when `cmd` was one of them.
    pub(super) fn run_basics(&mut self, ui: &mut Ui, cmd: u32) -> bool {
        match cmd {
            // one notch of the wheel = two steps
            CMD_VIEW_ZOOM_IN => self.zoom_by(2),
            CMD_VIEW_ZOOM_OUT => self.zoom_by(-2),
            CMD_RESET_COLUMNS => self.reset_columns(),
            CMD_SELECT_INVERT => {
                let t = self.tab_mut();
                if !(t.miller_on || t.tree_on) {
                    match t.group.rows() {
                        // collapsed groups are not on screen: they keep what they have
                        Some(g) => crate::view_group::invert_visible(&t.store, &mut t.sel, g),
                        None => t.sel.invert(&t.store),
                    }
                }
            }
            CMD_VIEW_EXTENSIONS => {
                self.prefs.view.show_extensions = !self.prefs.view.show_extensions;
                self.sync_basics();
                self.config_dirty = true;
            }
            _ => return false,
        }
        ui.request_repaint();
        true
    }

    // ---- column header: menu and fit -----------------------------------------------------

    /// A right click on the header at `at` (the live pane): opens the column menu.
    pub(super) fn open_col_menu(&mut self, ui: &mut Ui, at: Pt) {
        self.basics.col_menu = Some((self.live, at));
        ui.open_popup(pid(self.live, ID_COLMENU));
    }

    /// The header was double-clicked at a column edge: fit that column, and hold off the resize
    /// drag the still-pressed button would otherwise start.
    pub(super) fn fit_edge(&mut self, g: &mut dyn Canvas, t: &Theme, col: Col) {
        self.basics.fit_hold = true;
        self.fit_columns(g, t, &[col]);
    }

    /// While a fit double-click is held, resize drags are ignored (cleared when the button is up).
    pub(super) fn fit_held(&mut self, ui: &Ui) -> bool {
        if self.basics.fit_hold && ui.active().is_none() {
            self.basics.fit_hold = false;
        }
        self.basics.fit_hold
    }

    /// "Size all columns to fit" picked in this pane's menu: done by the pane while it draws.
    pub(super) fn take_fit_all(&mut self) -> bool {
        if self.basics.fit_pending == Some(self.live) {
            self.basics.fit_pending = None;
            return true;
        }
        false
    }

    /// Sets the width of each of `cols` to its widest text in the loaded rows (and its header),
    /// at least `MIN_W`, at most `FIT_CAP`. Name takes what is left, so it is never fitted.
    pub(super) fn fit_columns(&mut self, g: &mut dyn Canvas, t: &Theme, cols: &[Col]) {
        let mut text = std::mem::take(&mut self.text);
        let font = t.font();
        let head = t.font_small();
        let mut want = [0.0f32; NUM_COLS];
        for &c in cols {
            if c != Col::Name {
                // header label, a sort arrow and the side padding
                want[c.slot()] = g.measure(self.colset.label(c), head) + 16.0 + 14.0;
            }
        }
        {
            let tab = &self.tabs.items[self.tabs.active];
            let row = &mut *self.rowbuf;
            let n = tab.store.view().len().min(FIT_ROWS);
            // known file types share a type text: measured once each
            let mut kind_w = [-1.0f32; crate::data::KINDS.len()];
            let mut date_done = false;
            let wants = |c: Col| cols.contains(&c);
            for i in 0..n {
                let Some(id) = tab.store.at(i) else { break };
                row.fill(&tab.store, id);
                if wants(Col::Modified) && !date_done {
                    // digits are tabular: any date is as wide as any other
                    data::fmt_date(row.mtime, &mut text);
                    want[Col::Modified.slot()] = want[Col::Modified.slot()].max(g.measure(&text, font) + 14.0);
                    date_done = true;
                }
                if wants(Col::Type) {
                    let k = row.kind as usize;
                    let w = if k < kind_w.len() && kind_w[k] >= 0.0 {
                        kind_w[k]
                    } else {
                        row.type_text(&mut text);
                        let w = g.measure(&text, font) + 14.0;
                        if k < kind_w.len() {
                            kind_w[k] = w;
                        }
                        w
                    };
                    want[Col::Type.slot()] = want[Col::Type.slot()].max(w);
                }
                if wants(Col::Size) && !row.is_dir() {
                    data::fmt_size(row.size, &mut text);
                    want[Col::Size.slot()] = want[Col::Size.slot()].max(g.measure(&text, font) + 14.0);
                }
            }
        }
        self.text = text;
        for &c in cols {
            if c != Col::Name {
                self.cols.w[c.slot() - 1] = want[c.slot()].clamp(MIN_W, FIT_CAP);
            }
        }
    }

    /// The column menu, drawn last in the frame (a popup): a check row per column and
    /// "Size all columns to fit". Name is always on.
    pub(super) fn paint_col_menu(&mut self, ui: &mut Ui, g: &mut dyn Canvas) {
        let Some((slot, at)) = self.basics.col_menu else { return };
        let id = pid(slot, ID_COLMENU);
        if !ui.is_popup_open(id) {
            self.basics.col_menu = None;
            return;
        }
        // (column, extension id, label, on, can be switched)
        let mut rows: Vec<(Col, &'static str, String, bool, bool)> = Vec::with_capacity(crate::cols::BUILTIN.len());
        for b in crate::cols::BUILTIN {
            let (on, can) = match b.col {
                Col::Name => (true, false),
                Col::Tags => (crate::tags::column_shown(), crate::tags::enabled()),
                _ => (crate::ext::read(|r| r.is_enabled(b.ext).unwrap_or(false)), true),
            };
            rows.push((b.col, b.ext, self.colset_label(b.col, b.spec.label.as_ref()), on, can));
        }
        let mut items: Vec<MenuItem> = rows.iter().map(|(_, _, label, on, can)| MenuItem::new(label, "").check(*on).off(!*can)).collect();
        items.push(MenuItem::new("Size all columns to fit", "").sep());
        items.push(MenuItem::new("Reset columns", ""));
        // "Group by": one check row per mode, the current one ticked
        let can_group = !(self.tab().tree_on || self.tab().miller_on);
        let by = self.tab().group.by;
        if can_group {
            const LABELS: [&str; 5] = ["Group by: none", "Group by: name", "Group by: type", "Group by: date modified", "Group by: size"];
            for (k, m) in tsubame_core::grouping::GroupBy::ALL.iter().enumerate() {
                let item = MenuItem::new(LABELS[k], "").check(*m == by);
                items.push(if k == 0 { item.sep() } else { item });
            }
        }
        let Some(picked) = overlay::menu(ui, g, id, at, &items) else { return };
        self.basics.col_menu = None;
        if let Some(k) = picked.checked_sub(rows.len() + 2).filter(|_| can_group)
            && let Some(&m) = tsubame_core::grouping::GroupBy::ALL.get(k)
        {
            self.set_group_by(m);
            ui.request_repaint();
            return;
        }
        match rows.get(picked) {
            Some(&(Col::Tags, _, _, on, _)) => {
                self.prefs.tags.column = !on;
                self.sync_tags();
                self.config_dirty = true;
            }
            Some(&(_, ext, _, on, _)) => {
                let _ = self.set_extension_enabled(ui, ext, !on);
            }
            None if picked == rows.len() => self.basics.fit_pending = Some(slot),
            None if picked == rows.len() + 1 => self.reset_columns(),
            None => {}
        }
        ui.request_repaint();
    }

    /// The label the details header shows for `col` (the registry's, else the built-in's).
    pub(super) fn colset_label(&self, col: Col, fallback: &str) -> String {
        let l = self.colset.label(col);
        if l.is_empty() { fallback.to_string() } else { l.to_string() }
    }
}
