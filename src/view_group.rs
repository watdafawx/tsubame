//! Group by (`docs/grouping.md`): the per-tab group state, the header row, and
//! the flat row <-> item translation the rest of the view relies on.
//!
//! Everything the app keeps (cursor, hover, selection, drag, activation, session)
//! is an *item*: a position in `store.view()`. Only the list and the grid work in
//! flat rows (headers + shown items); this module is the one place that maps
//! between them. The grouping is rebuilt once when the store's revision, the group
//! mode or the sort direction moves, never while painting rows.

use crate::data;
use crate::tab::Tab;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use tsubame_core::grouping::{DateBoundaries, GroupBy, GroupKey, group_key};
use tsubame_core::{EntryStore, FOLDER};
use tsubame_ui::{Canvas, GroupRow, GroupRows, Grouping, HeaderState, Rect, Theme};

/// `[view] group_by`: the mode new tabs start in.
static DEFAULT: AtomicU8 = AtomicU8::new(0);

fn code(by: GroupBy) -> u8 {
    GroupBy::ALL.iter().position(|&b| b == by).unwrap_or(0) as u8
}

pub fn set_default(by: GroupBy) {
    DEFAULT.store(code(by), Ordering::Relaxed);
}

pub fn default_by() -> GroupBy {
    GroupBy::ALL.get(DEFAULT.load(Ordering::Relaxed) as usize).copied().unwrap_or_default()
}

/// What a built grouping was made from.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Built {
    revision: u64,
    by: GroupBy,
    descending: bool,
    on: bool,
}

pub struct GroupState {
    pub by: GroupBy,
    grouping: Grouping<GroupKey>,
    built: Option<Built>,
    /// the groups' header texts, cached per rebuild
    labels: Vec<String>,
    on: bool,
}

impl GroupState {
    pub fn new(by: GroupBy) -> GroupState {
        GroupState { by, grouping: Grouping::default(), built: None, labels: Vec::new(), on: false }
    }

    /// The mode changed (`true`) and the grouping is rebuilt on the next `sync`.
    pub fn set_by(&mut self, by: GroupBy) -> bool {
        if self.by == by {
            return false;
        }
        self.by = by;
        self.built = None;
        true
    }

    /// Grouped rows are in use: a mode is set, the listing is complete, and the view
    /// shows plain rows or cells (Tree and Miller ignore grouping).
    pub fn on(&self) -> bool {
        self.on
    }

    pub fn rows(&self) -> Option<&dyn GroupRows> {
        self.on.then_some(&self.grouping as &dyn GroupRows)
    }

    pub fn grouping(&self) -> &Grouping<GroupKey> {
        &self.grouping
    }

    pub fn grouping_mut(&mut self) -> &mut Grouping<GroupKey> {
        &mut self.grouping
    }

    pub fn label(&self, g: usize) -> &str {
        self.labels.get(g).map_or("", String::as_str)
    }

    /// Collapses or expands group `g` (`all`: every group). True when something changed.
    pub fn toggle(&mut self, g: usize, all: bool) -> bool {
        if !all {
            return self.grouping.toggle(g, false);
        }
        // Ctrl+click: every group follows the clicked one's new state
        let to = !self.grouping.groups().get(g).is_some_and(|x| x.collapsed);
        self.grouping.set_collapsed(g, to, true)
    }

    /// Brings the grouping up to date with `store`. Cheap when nothing moved.
    /// `allowed`: the view shows plain rows or cells (not Tree or Miller).
    pub fn sync(&mut self, store: &EntryStore, descending: bool, allowed: bool) {
        let on = allowed && self.by != GroupBy::None && !store.is_streaming();
        let built = Built { revision: store.revision(), by: self.by, descending, on };
        if self.built == Some(built) {
            return;
        }
        self.built = Some(built);
        self.on = on;
        if !on {
            if !self.labels.is_empty() {
                self.grouping.rebuild(0, std::iter::empty(), false);
                self.labels.clear();
            }
            return;
        }
        let by = self.by;
        let dates = if by == GroupBy::Date { DateBoundaries::from_local(now_unix(), offset_at) } else { DateBoundaries::utc(0) };
        let view = store.view();
        let items = view.iter().enumerate().filter_map(|(j, &id)| {
            let rec = store.record(id)?;
            let name = store.name(id)?;
            let key = group_key(by, name, rec.flags & FOLDER != 0, rec.size, i64::from(data::filetime_to_unix(rec.modified)), &dates)?;
            Some((j, key))
        });
        self.grouping.rebuild(view.len(), items, descending);
        self.labels.clear();
        self.labels.extend(self.grouping.groups().iter().map(|g| g.key.label()));
    }

    /// The group an item belongs to, collapsed or not (items of a group ascend).
    pub fn group_of(&self, item: usize) -> Option<usize> {
        self.grouping.groups().iter().position(|g| g.items.binary_search(&item).is_ok())
    }

    /// Expands the group of `item` when it is hidden in a collapsed one. True when it did.
    pub fn reveal(&mut self, item: usize) -> bool {
        if !self.on || self.grouping.row(item).is_some() {
            return false;
        }
        match self.group_of(item) {
            Some(g) => self.grouping.set_collapsed(g, false, false),
            None => false,
        }
    }

    /// The item range of the flat rows `(a, b)` (end exclusive) a list shows:
    /// the first and last item among them, as `(first, last + 1)` in item numbers.
    pub fn item_range(&self, rows: (usize, usize)) -> (usize, usize) {
        let Some(g) = self.rows() else { return rows };
        let (mut lo, mut hi) = (usize::MAX, 0);
        let end = rows.1.min(g.row_count());
        for r in rows.0.min(end)..end {
            if let GroupRow::Item(i) = g.row_at(r) {
                lo = lo.min(i);
                hi = hi.max(i + 1);
            }
        }
        if lo == usize::MAX { (0, 0) } else { (lo, hi) }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Local offset (seconds) at a UTC instant, DST of that date included.
fn offset_at(utc: i64) -> i64 {
    let u = utc.clamp(0, i64::from(u32::MAX)) as u32;
    i64::from(tsubame_win::tz::local_secs(u)) - i64::from(u)
}

/// A group header: chevron, bold name, count, and a thin rule to the right edge.
/// `buf` is scratch for the count (no allocation per draw).
pub fn draw_header(c: &mut dyn Canvas, t: &Theme, label: &str, count: usize, r: Rect, st: HeaderState, buf: &mut String) {
    let chevron = if st.collapsed { "\u{E76C}" } else { "\u{E70D}" };
    c.text(chevron, Rect::new(r.x + 8.0, r.y, 16.0, r.h), tsubame_ui::f(10.0).icon().c(), t.text_dim);
    let font = t.font_bold();
    let max_w = (r.w - 80.0).max(40.0);
    let lw = c.measure(label, font).min(max_w);
    let x = r.x + 30.0;
    c.text(label, Rect::new(x, r.y, lw + 1.0, r.h), font, t.text);
    buf.clear();
    let _ = write!(buf, "{count}");
    let cw = c.measure(buf, t.font());
    let cx = x + lw + 8.0;
    c.text(buf, Rect::new(cx, r.y, cw + 1.0, r.h), t.font(), t.text_dim);
    let rx = cx + cw + 10.0;
    let right = r.x + r.w - 8.0;
    if right > rx {
        c.fill(Rect::new(rx, r.y + (r.h / 2.0).floor(), right - rx, 1.0), t.border);
    }
}

/// Items of the flat rows `a..=b`, headers skipped (marquee in the list).
pub fn items_of_rows(g: &dyn GroupRows, a: usize, b: usize) -> impl Iterator<Item = usize> + '_ {
    let end = b.saturating_add(1).min(g.row_count());
    (a.min(end)..end).filter_map(move |r| match g.row_at(r) {
        GroupRow::Item(i) => Some(i),
        GroupRow::Header(_) => None,
    })
}

/// Shift+click / Shift+arrow range in reading order: from the anchor to `item`
/// over what the screen shows (so a range never selects hidden items, and follows
/// the groups, not the sort order underneath them).
pub fn extend_visual(store: &EntryStore, sel: &mut tsubame_core::Selection, g: &dyn GroupRows, item: usize) {
    let Some(id) = store.at(item) else { return };
    let Some(to) = g.item_row(item) else { return };
    let anchor = sel.anchor.and_then(|a| store.view().iter().position(|&v| v == a));
    let from = anchor.and_then(|p| g.item_row(p)).unwrap_or(to);
    let (a, b) = (from.min(to), from.max(to));
    sel.replace_with(store, items_of_rows(g, a, b).filter_map(|i| store.at(i)));
    if from == to || sel.anchor.is_none() {
        sel.anchor = Some(id);
    }
    sel.focus = Some(id);
}

/// Invert on what is shown: collapsed groups keep their selection.
pub fn invert_visible(store: &EntryStore, sel: &mut tsubame_core::Selection, g: &dyn GroupRows) {
    let flip: Vec<_> = items_of_rows(g, 0, g.row_count().saturating_sub(1)).filter_map(|i| store.at(i)).collect();
    let keep: Vec<_> = store.view().iter().copied().filter(|id| sel.contains(*id) && !flip.contains(id)).collect();
    let on: Vec<_> = flip.iter().copied().filter(|id| !sel.contains(*id)).collect();
    sel.replace_with(store, keep.into_iter().chain(on));
}

impl Tab {
    /// Rebuilds the grouping if the listing, the mode or the sort moved.
    pub fn sync_group(&mut self) {
        let allowed = !(self.tree_on || self.miller_on || self.settings);
        self.group.sync(&self.store, self.sort.descending, allowed);
    }

    /// Flat rows the list shows: items alone, or headers and shown items when grouped.
    pub fn list_rows(&self) -> usize {
        self.group.rows().map_or(self.store.view().len(), |g| g.row_count())
    }

    /// The flat row of `item` in the list (its group's header when it is hidden).
    pub fn list_row_of(&self, item: usize) -> usize {
        let Some(g) = self.group.rows() else { return item };
        g.item_row(item).or_else(|| self.group.group_of(item).map(|gi| g.header_row(gi))).unwrap_or(0)
    }

    /// The item shown on flat row `row` (`None`: a header or past the end).
    pub fn item_of_row(&self, row: usize) -> Option<usize> {
        match self.group.rows() {
            None => (row < self.store.view().len()).then_some(row),
            Some(g) => (row < g.row_count()).then(|| g.row_at(row)).and_then(|r| match r {
                GroupRow::Item(i) => Some(i),
                GroupRow::Header(_) => None,
            }),
        }
    }

    /// The item at or after flat row `row` (before it when none follows): what the
    /// session and `keep_place` remember of the top of the list.
    pub fn item_at_top_row(&self, row: usize) -> usize {
        let Some(g) = self.group.rows() else { return row };
        let item = |r: usize| match g.row_at(r) {
            GroupRow::Item(i) => Some(i),
            GroupRow::Header(_) => None,
        };
        g.nav_row(row, true).or_else(|| g.nav_row(row, false)).and_then(item).unwrap_or(0)
    }

    /// Scrolls the list so `item` is on its top row (the header too when it opens its group).
    pub fn set_list_top(&mut self, item: usize) {
        self.sync_group();
        self.group.reveal(item);
        let row = match self.group.rows() {
            None => item,
            Some(g) => match g.item_slot(item) {
                Some((gi, 0)) => g.header_row(gi),
                Some(_) => g.item_row(item).unwrap_or(0),
                None => 0,
            },
        };
        self.list.scroll = row as f64 * self.list.row_h as f64;
    }

    /// The cursor goes to `item` (expanding its group) and scrolls into view, in the
    /// list or the grid. Without grouping this is the plain `move_cursor`.
    pub fn move_cursor_to(&mut self, grid: bool, item: usize) {
        self.sync_group();
        let len = self.store.view().len();
        if len == 0 {
            return;
        }
        let item = item.min(len - 1);
        self.group.reveal(item);
        if self.group.rows().is_none() {
            if grid {
                self.grid.move_cursor(item, len);
            } else {
                self.list.move_cursor(item, len);
            }
            return;
        }
        if grid {
            self.grid.set_header_focus(None);
            self.grid.cursor = item;
            // the grid brings it into view when it paints
            self.grid.reveal = true;
        } else {
            self.list.set_header_focus(None);
            self.list.cursor = item;
            let row = self.list_row_of(item);
            let n = self.list_rows();
            let first_of_group = self.group.rows().and_then(|g| g.item_slot(item)).is_some_and(|(_, j)| j == 0);
            self.list.ensure_visible(row, n);
            if first_of_group && row > 0 {
                self.list.ensure_visible(row - 1, n);
            }
        }
    }

    /// Top-left of `item`'s cell in the grid, relative to the cells area.
    pub fn cell_origin_of(&self, item: usize) -> (f32, f32) {
        match self.group.rows() {
            Some(g) if self.grid.geo_active() => self.grid.group_item_origin(g, item).unwrap_or((0.0, -1.0e6)),
            _ => self.grid.cell_origin(item),
        }
    }

    /// Top of `item`'s row in the list, relative to the rows area.
    pub fn row_top_of(&self, item: usize) -> f64 {
        self.list.row_top(self.list_row_of(item))
    }

    /// The item under `(x, y)` DIPs from the top-left of the list or grid area.
    pub fn item_at_point(&self, grid: bool, x: f32, y: f32) -> Option<usize> {
        let len = self.store.view().len();
        if grid {
            return match self.group.rows() {
                Some(g) if self.grid.geo_active() => match self.grid.group_hit(g, x, y) {
                    tsubame_ui::GroupHit::Cell(gi, j) => Some(g.group_item(gi, j)),
                    _ => None,
                },
                _ => self.grid.cell_at(x, y, len),
            };
        }
        if y < 0.0 {
            return None;
        }
        let row = ((f64::from(y) + self.list.scroll) / f64::from(self.list.row_h)) as usize;
        self.item_of_row(row)
    }

    /// Switches the group mode of this tab.
    pub fn set_group_by(&mut self, by: GroupBy, grid: bool) -> bool {
        if !self.group.set_by(by) {
            return false;
        }
        self.list.set_header_focus(None);
        self.grid.set_header_focus(None);
        self.sync_group();
        let cur = if grid { self.grid.cursor } else { self.list.cursor };
        self.move_cursor_to(grid, cur);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsubame_core::EntryInput;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// (name, size, is folder) in the order given; the store sorts them by name.
    fn store(items: &[(&str, u64, bool)]) -> EntryStore {
        let mut s = EntryStore::new();
        let names: Vec<Vec<u16>> = items.iter().map(|i| u(i.0)).collect();
        let batch: Vec<EntryInput> = items
            .iter()
            .zip(&names)
            .map(|(i, n)| EntryInput { name: n, size: i.1, modified: 0, flags: if i.2 { FOLDER } else { 0 } })
            .collect();
        s.push_batch(s.generation(), &batch).unwrap();
        s
    }

    fn mixed() -> EntryStore {
        store(&[("alpha.txt", 10, false), ("Beta.rs", 20_000, false), ("apple.rs", 5, false), ("zeta", 0, true), ("9lives.txt", 2_000_000, false), ("bin", 0, true)])
    }

    #[test]
    fn rebuilds_once_per_store_change() {
        let mut s = mixed();
        let mut g = GroupState::new(GroupBy::Name);
        g.sync(&s, false, true);
        assert!(g.on());
        let rev = g.rows().unwrap().revision();
        g.sync(&s, false, true);
        assert_eq!(g.rows().unwrap().revision(), rev, "nothing moved: no rebuild");
        s.sort(tsubame_core::Sort { column: tsubame_core::Column::Size, descending: false, folders_first: true });
        g.sync(&s, false, true);
        assert_ne!(g.rows().unwrap().revision(), rev, "a sort rebuilds");
    }

    #[test]
    fn name_groups_and_labels() {
        let s = mixed();
        let mut g = GroupState::new(GroupBy::Name);
        g.sync(&s, false, true);
        let labels: Vec<&str> = (0..g.rows().unwrap().group_count()).map(|i| g.label(i)).collect();
        assert_eq!(labels, ["#", "A", "B", "Z"]);
        // folders first in the sorted view: bin, zeta, then files by name
        let rows = g.rows().unwrap();
        assert_eq!(rows.group_len(1), 2, "alpha and apple");
    }

    #[test]
    fn descending_reverses_groups_only() {
        let s = mixed();
        let mut a = GroupState::new(GroupBy::Name);
        a.sync(&s, false, true);
        let mut d = GroupState::new(GroupBy::Name);
        d.sync(&s, true, true);
        let up: Vec<&str> = (0..4).map(|i| a.label(i)).collect();
        let down: Vec<&str> = (0..4).map(|i| d.label(i)).collect();
        assert_eq!(down, up.iter().rev().copied().collect::<Vec<_>>());
    }

    #[test]
    fn tree_miller_and_streaming_are_ungrouped() {
        let mut s = mixed();
        let mut g = GroupState::new(GroupBy::Type);
        g.sync(&s, false, false);
        assert!(!g.on() && g.rows().is_none(), "Tree and Miller ignore grouping");
        g.sync(&s, false, true);
        assert!(g.on());
        s.begin_stream();
        g.sync(&s, false, true);
        assert!(!g.on(), "no groups while the listing streams");
        s.end_stream();
        g.sync(&s, false, true);
        assert!(g.on());
        let mut none = GroupState::new(GroupBy::None);
        none.sync(&s, false, true);
        assert!(!none.on());
    }

    #[test]
    fn collapse_survives_a_rebuild_and_reveal_expands() {
        let mut s = mixed();
        let mut g = GroupState::new(GroupBy::Type);
        g.sync(&s, false, true);
        assert_eq!(g.label(0), "Folder");
        assert!(g.toggle(0, false));
        assert!(g.rows().unwrap().is_collapsed(0));
        // a listing change rebuilds; the collapsed group stays collapsed
        let n = u("new.txt");
        s.push_batch(s.generation(), &[EntryInput { name: &n, size: 1, modified: 0, flags: 0 }]).unwrap();
        g.sync(&s, false, true);
        assert!(g.rows().unwrap().is_collapsed(0));
        // a hidden item is revealed by expanding its group
        let hidden = g.rows().unwrap().group_item(0, 0);
        assert!(g.rows().unwrap().item_row(hidden).is_none());
        assert!(g.reveal(hidden));
        assert!(g.rows().unwrap().item_row(hidden).is_some());
        assert!(!g.reveal(hidden), "already shown");
    }

    #[test]
    fn ctrl_toggle_follows_the_clicked_group() {
        let s = mixed();
        let mut g = GroupState::new(GroupBy::Name);
        g.sync(&s, false, true);
        assert!(g.toggle(1, true));
        let r = g.rows().unwrap();
        assert!((0..r.group_count()).all(|i| r.is_collapsed(i)), "all collapse with the clicked one");
        assert!(g.toggle(2, true));
        let r = g.rows().unwrap();
        assert!((0..r.group_count()).all(|i| !r.is_collapsed(i)), "then all expand");
    }

    #[test]
    fn item_range_of_flat_rows() {
        let s = mixed();
        let mut g = GroupState::new(GroupBy::Name);
        g.sync(&s, false, true);
        let r = g.rows().unwrap();
        // row 0 is a header, row 1 the first item
        let first = match r.row_at(1) {
            GroupRow::Item(i) => i,
            GroupRow::Header(_) => unreachable!(),
        };
        assert_eq!(g.item_range((0, 2)), (first, first + 1));
        assert_eq!(g.item_range((0, 0)), (0, 0));
        let ungrouped = GroupState::new(GroupBy::None);
        assert_eq!(ungrouped.item_range((3, 9)), (3, 9));
    }

    #[test]
    fn visual_range_follows_groups_not_sort() {
        // by type: Folder (bin, zeta), then rs (apple, Beta), then txt (9lives, alpha)
        let s = mixed();
        let mut g = GroupState::new(GroupBy::Type);
        g.sync(&s, false, true);
        let rows = g.rows().unwrap();
        let mut sel = tsubame_core::Selection::new();
        let first_rs = rows.group_item(1, 0);
        let last_rs = rows.group_item(1, 1);
        sel.single(&s, first_rs);
        extend_visual(&s, &mut sel, rows, last_rs);
        assert_eq!(sel.count(), 2);
        // across the groups: from the first folder to the first txt
        sel.single(&s, rows.group_item(0, 0));
        extend_visual(&s, &mut sel, rows, rows.group_item(2, 0));
        assert_eq!(sel.count(), 2 + 2 + 1, "both folders, both rs files, one txt");
        // hidden items are left alone
        let mut g2 = GroupState::new(GroupBy::Type);
        g2.sync(&s, false, true);
        g2.toggle(1, false);
        let rows = g2.rows().unwrap();
        sel.single(&s, rows.group_item(0, 0));
        extend_visual(&s, &mut sel, rows, rows.group_item(2, 0));
        assert_eq!(sel.count(), 2 + 1, "the collapsed group is skipped");
    }

    #[test]
    fn invert_leaves_collapsed_items_alone() {
        let s = mixed();
        let mut g = GroupState::new(GroupBy::Type);
        g.sync(&s, false, true);
        let hidden = g.rows().unwrap().group_item(1, 0);
        let mut sel = tsubame_core::Selection::new();
        sel.single(&s, hidden);
        g.toggle(1, false);
        let rows = g.rows().unwrap();
        invert_visible(&s, &mut sel, rows);
        assert!(sel.contains(s.at(hidden).unwrap()), "still selected");
        // every shown item is now selected too
        assert_eq!(sel.count(), 1 + 2 + 2);
    }
}
