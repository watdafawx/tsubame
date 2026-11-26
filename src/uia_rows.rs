//! UI Automation for the file views, the app's side (`docs/uia-views.md`).
//!
//! `tsubame_win::uia` owns the providers. This module builds what they read:
//! one [`VSurface`] per visible List or Tree (Details and Grid rows, Tree rows,
//! the Tree pane, each visible Miller column) with the rows the widget drew and
//! the total. It runs only while a screen reader listens (`uia::active()`, one
//! atomic load until a client first asks): a frame without a listener allocates
//! nothing here. Rows come from the entries the frame already decoded for
//! painting (`RowBuf`), never from the entry store beyond the visible range.
//!
//! Frame protocol (all on the UI thread): [`begin_frame`], any number of
//! builders while the panes paint, [`end_frame`], which publishes the snapshot
//! (the provider compares it with the last one and raises events).

use std::cell::{Cell, RefCell};
use std::fmt::Write;

use tsubame_core::{EntryStore, Selection};
use tsubame_ui::{GridState, ListState, Rect};
use tsubame_win::uia::{self, Expand, VItem, VSurface, ViewSnap};

use crate::age::{AgeFrame, Tint};
use crate::data::{self, RowBuf};
use crate::miller::Column;
use crate::tags::model::a11y_name;
use crate::tags::ui::{RowTags, TagsUi};
use crate::tree::{Kind, TreeModel};

// ---- surface keys -------------------------------------------------------------

/// The file list of a pane (Details, Grid or Tree rows).
pub const PART_LIST: u32 = 1;
/// The folders-only Tree pane.
pub const PART_PANE: u32 = 2;
/// Path bar breadcrumb chevrons (each invokes/expands its subfolder menu).
pub const PART_CRUMBS: u32 = 3;
/// The path edit's autocomplete dropdown (each row invokes: opens that folder).
pub const PART_SUGGEST: u32 = 4;
/// Miller column `i` is `PART_MILLER + i` (at most 13 live columns).
pub const PART_MILLER: u32 = 16;

/// Stable id of a surface: the pane's slot and the part.
pub fn skey(slot: usize, part: u32) -> u32 {
    slot as u32 * 64 + part
}

pub fn slot_of(key: u32) -> usize {
    (key / 64) as usize
}

pub fn part_of(key: u32) -> u32 {
    key % 64
}

/// A tree row's key: slot, kind and entry id in one number.
pub fn tree_key(slot: u32, kind: Kind, id: u32) -> u64 {
    (u64::from(slot) << 40) | (u64::from(kind_code(kind)) << 32) | u64::from(id)
}

/// The parts of [`tree_key`].
pub fn untree_key(k: u64) -> Option<(u32, Kind, u32)> {
    let kind = match (k >> 32) & 0xff {
        0 => Kind::Entry,
        1 => Kind::Loading,
        2 => Kind::Empty,
        3 => Kind::Denied,
        4 => Kind::Loop,
        5 => Kind::Capped,
        _ => return None,
    };
    Some(((k >> 40) as u32, kind, k as u32))
}

fn kind_code(k: Kind) -> u8 {
    match k {
        Kind::Entry => 0,
        Kind::Loading => 1,
        Kind::Empty => 2,
        Kind::Denied => 3,
        Kind::Loop => 4,
        Kind::Capped => 5,
    }
}

// ---- the frame ----------------------------------------------------------------

#[derive(Default)]
struct Frame {
    surfaces: Vec<VSurface>,
    /// something was published: an empty snapshot is owed when the listener goes
    published: bool,
}

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static FRAME: RefCell<Frame> = RefCell::new(Frame::default());
    /// tests: the last snapshot, instead of a screen reader
    #[cfg(test)]
    static LAST: RefCell<ViewSnap> = RefCell::new(ViewSnap::default());
    #[cfg(test)]
    static FORCE: Cell<bool> = const { Cell::new(false) };
}

/// Starts a frame: asks once whether a client listens.
pub fn begin_frame() {
    #[cfg(test)]
    let on = uia::active() || FORCE.with(Cell::get);
    #[cfg(not(test))]
    let on = uia::active();
    ON.with(|c| c.set(on));
    if on {
        FRAME.with(|f| f.borrow_mut().surfaces.clear());
    }
}

/// Builders run only when this is true (a client listens).
#[inline]
pub fn on() -> bool {
    ON.with(Cell::get)
}

fn push(s: VSurface) {
    FRAME.with(|f| f.borrow_mut().surfaces.push(s));
}

/// Ends the frame: publishes the surfaces (the provider diffs and raises events),
/// or one empty snapshot when the listener has gone.
pub fn end_frame() {
    if on() {
        let mut surfaces = FRAME.with(|f| std::mem::take(&mut f.borrow_mut().surfaces));
        surfaces.sort_by_key(|s| s.key);
        FRAME.with(|f| f.borrow_mut().published = true);
        let snap = ViewSnap { surfaces };
        #[cfg(test)]
        LAST.with(|l| *l.borrow_mut() = snap.clone());
        uia::publish_view(snap);
    } else if FRAME.with(|f| std::mem::take(&mut f.borrow_mut().published)) {
        uia::publish_view(ViewSnap::default());
    }
}

#[cfg(test)]
pub fn force(on: bool) {
    FORCE.with(|f| f.set(on));
}

#[cfg(test)]
pub fn last() -> ViewSnap {
    LAST.with(|l| l.borrow().clone())
}

// ---- geometry and text --------------------------------------------------------

/// What every builder needs about the widget it follows.
pub struct Geo<'a> {
    pub key: u32,
    /// the folder or column name
    pub title: &'a str,
    /// the rows area (the scrollbar column excluded)
    pub area: Rect,
    /// the list has the keyboard and the window is active
    pub focused: bool,
}

fn px(r: Rect) -> [f32; 4] {
    [r.x, r.y, r.w, r.h]
}

/// `r` cut to `area` (rows at the edge of the viewport are partly clipped).
fn clipped(r: Rect, area: Rect) -> [f32; 4] {
    let (x0, y0) = (r.x.max(area.x), r.y.max(area.y));
    let (x1, y1) = (r.r().min(area.r()), r.b().min(area.b()));
    [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)]
}

/// The row rectangle of list row `i`.
fn list_row(area: Rect, ls: &ListState, i: usize) -> [f32; 4] {
    let y = area.y + (i as f64 * ls.row_h as f64 - ls.scroll) as f32;
    clipped(Rect::new(area.x, y, area.w, ls.row_h), area)
}

/// The last folder name of a path (the drive for a root).
pub fn leaf(path: &str) -> &str {
    let t = path.trim_end_matches('\\');
    match t.rfind('\\') {
        Some(i) if i + 1 < t.len() => &t[i + 1..],
        _ => if t.is_empty() { path } else { t },
    }
}

fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn counts(total: usize, selected: usize) -> String {
    let mut s = format!("{} item{}", group(total), if total == 1 { "" } else { "s" });
    if selected > 0 {
        let _ = write!(s, ", {} selected", group(selected));
    }
    s
}

/// The value text of a row: type, size, modified date and the age bucket.
/// Returns the type text too (UIA ItemType).
pub fn row_texts(row: &RowBuf, folder_size: Option<&str>, age: Option<&AgeFrame>) -> (String, String) {
    let mut kind = String::new();
    row.type_text(&mut kind);
    let mut v = kind.clone();
    let mut t = String::new();
    if !row.is_dir() {
        data::fmt_size(row.size, &mut t);
        let _ = write!(v, ", {t}");
    } else if let Some(s) = folder_size {
        let _ = write!(v, ", {s}");
    }
    if row.mtime != 0 {
        data::fmt_date(row.mtime, &mut t);
        let _ = write!(v, ", modified {t}");
        if let Some(af) = age
            && let Tint::Bucket(b, _) = af.classify(row.mtime, row.is_dir())
        {
            let _ = write!(v, ", age: {}", crate::age::bucket_name(b as usize));
        }
    }
    (kind, v)
}

/// The row's spoken name: the file name, then its tags.
fn named(row: &RowBuf, id: u32, tags: &mut Option<&mut RowTags>) -> String {
    let name = row.name_str();
    if let Some(tg) = tags.as_deref_mut()
        && let Some(m) = tg.mask(id, name).filter(|m| *m != 0)
    {
        return a11y_name(name, tg.defs(), m);
    }
    name.to_string()
}

// ---- Details and Grid ---------------------------------------------------------

/// Details rows: the rows `range` of the list.
#[allow(clippy::too_many_arguments)]
pub fn details(g: &Geo, ls: &ListState, range: (usize, usize), store: &EntryStore, sel: &Selection, row: &mut RowBuf, mut tags: Option<&mut RowTags>, age: Option<&AgeFrame>) {
    let total = store.view().len();
    let mut items = Vec::with_capacity(range.1.saturating_sub(range.0));
    for i in range.0..range.1.min(total) {
        let Some(id) = store.at(i) else { continue };
        row.fill(store, id);
        let (kind, value) = row_texts(row, None, age);
        items.push(VItem {
            key: u64::from(id),
            name: named(row, id, &mut tags),
            value,
            kind,
            rect: list_row(g.area, ls, i),
            selected: sel.contains(id),
            focused: g.focused && i == ls.cursor,
            pos: i as u32 + 1,
            selectable: true,
            invokable: true,
            ..Default::default()
        });
    }
    push(VSurface { key: g.key, tree: false, name: g.title.to_string(), desc: counts(total, sel.count()), rect: px(g.area), focused: g.focused, multi: true, size: total as u32, items });
}

/// Grid tiles: the cells `range` of the grid.
#[allow(clippy::too_many_arguments)]
pub fn grid(g: &Geo, gs: &GridState, range: (usize, usize), store: &EntryStore, sel: &Selection, row: &mut RowBuf, mut tags: Option<&mut RowTags>) {
    let total = store.view().len();
    let mut items = Vec::with_capacity(range.1.saturating_sub(range.0));
    for i in range.0..range.1.min(total) {
        let Some(id) = store.at(i) else { continue };
        row.fill(store, id);
        let (kind, value) = row_texts(row, None, None);
        let (ox, oy) = gs.cell_origin(i);
        items.push(VItem {
            key: u64::from(id),
            name: named(row, id, &mut tags),
            value,
            kind,
            rect: clipped(Rect::new(g.area.x + ox, g.area.y + oy, gs.cell_w, gs.cell_h), g.area),
            selected: sel.contains(id),
            focused: g.focused && i == gs.cursor,
            pos: i as u32 + 1,
            selectable: true,
            invokable: true,
            ..Default::default()
        });
    }
    push(VSurface { key: g.key, tree: false, name: g.title.to_string(), desc: counts(total, sel.count()), rect: px(g.area), focused: g.focused, multi: true, size: total as u32, items });
}

// ---- Tree rows and the Tree pane ----------------------------------------------

/// The Tree: rows `range` (Tree rows with columns, or the folders-only pane).
#[allow(clippy::too_many_arguments)]
pub fn tree(g: &Geo, ls: &ListState, range: (usize, usize), m: &TreeModel, root: Option<&EntryStore>, root_fids: &[(u128, u32)], tags: &mut TagsUi, row: &mut RowBuf, age: Option<&AgeFrame>, columns: bool) {
    use crate::tree::State;
    let total = m.len();
    let mut items = Vec::with_capacity(range.1.saturating_sub(range.0));
    for i in range.0..range.1.min(total) {
        let (Some(tr), Some(k)) = (m.row(i), m.key_at(i, root)) else { continue };
        let mut it = VItem { key: tree_key(k.slot, k.kind, k.id), rect: list_row(g.area, ls, i), focused: g.focused && i == ls.cursor, level: tr.depth + 1, pos: i as u32 + 1, ..Default::default() };
        match tr.kind {
            Kind::Entry => {
                let Some((slot, id)) = m.entry_id(i, root) else { continue };
                let Some(store) = m.store_of(slot, root) else { continue };
                row.fill(store, id);
                let (kind, mut value) = if columns { row_texts(row, None, age) } else { (String::new(), String::new()) };
                if let Some(target) = m.link_target(i, root) {
                    let _ = write!(value, "{}link to {target}", if value.is_empty() { "" } else { ", " });
                } else if m.is_link(i, root) && !m.is_expanded(i, root) {
                    value.push_str(if value.is_empty() { "link" } else { ", link" });
                }
                let mut rt = None;
                let fids = if slot == 0 && root.is_some() { root_fids } else { m.dir_fids(slot) };
                if let Some(dir) = m.dir_path(slot) {
                    rt = tags.rows(dir, fids);
                }
                it.name = named(row, id, &mut rt.as_mut());
                it.value = value;
                it.kind = kind;
                it.selected = m.is_selected(i, root);
                it.selectable = true;
                it.invokable = true;
                it.expand = if m.is_folder(i, root) {
                    if m.is_expanded(i, root) { Expand::Expanded } else { Expand::Collapsed }
                } else {
                    Expand::Leaf
                };
            }
            Kind::Loading => it.name = "Loading".into(),
            Kind::Empty => it.name = "Empty folder".into(),
            Kind::Denied => {
                let msg = match m.state_of(tr.slot) {
                    Some(State::Denied(s)) if !s.is_empty() => s.clone(),
                    _ => "Access denied".into(),
                };
                it.name = format!("{msg}. Retry");
                it.invokable = true;
            }
            Kind::Loop => {
                let target = match m.state_of(tr.slot) {
                    Some(State::Loop(p)) => p.clone(),
                    _ => String::new(),
                };
                it.name = format!("Not expanded: loops back to {target}");
            }
            Kind::Capped => it.name = format!("Showing first {} of {}", m.dir_shown(tr.slot), m.dir_total(tr.slot)),
        }
        items.push(it);
    }
    let desc = if columns { counts(total, m.sel_count()) } else { format!("{} folders and rows", group(total)) };
    push(VSurface { key: g.key, tree: true, name: g.title.to_string(), desc, rect: px(g.area), focused: g.focused, multi: columns, size: total as u32, items });
}

// ---- Miller columns -----------------------------------------------------------

/// One visible Miller column: rows `range`. Rows on the path (the selection of
/// a column left of the active one) count as selected.
pub fn miller(g: &Geo, ls: &ListState, range: (usize, usize), col: &Column, active: bool, row: &mut RowBuf) {
    let total = col.shown();
    let mut items = Vec::with_capacity(range.1.saturating_sub(range.0));
    for i in range.0..range.1.min(total) {
        let Some(id) = col.store.at(i) else { continue };
        row.fill(&col.store, id);
        let (kind, mut value) = row_texts(row, None, None);
        if col.is_link_at(i) {
            value.push_str(", link");
        }
        items.push(VItem {
            key: u64::from(id),
            name: row.name_str().to_string(),
            value,
            kind,
            rect: list_row(g.area, ls, i),
            selected: col.sel == Some(id),
            focused: active && g.focused && i == ls.cursor,
            pos: i as u32 + 1,
            selectable: true,
            invokable: true,
            ..Default::default()
        });
    }
    let desc = format!("{}, {}", col.path, counts(total, 0));
    push(VSurface { key: g.key, tree: false, name: g.title.to_string(), desc, rect: px(g.area), focused: active && g.focused, multi: false, size: total as u32, items });
}

// ---- Path bar breadcrumb chevrons --------------------------------------------

/// One breadcrumb chevron published to UIA while a screen reader listens.
pub struct CrumbUia {
    pub index: usize,
    pub label: String,
    pub path: String,
    pub rect: Rect,
    pub expanded: bool,
}

/// Publishes the visible breadcrumb chevrons as a `PART_CRUMBS` surface.
pub fn crumbs(slot: usize, bar: Rect, chevrons: &[CrumbUia]) {
    let items: Vec<VItem> = chevrons
        .iter()
        .enumerate()
        .map(|(pos, c)| VItem {
            key: c.index as u64,
            name: format!("Subfolders of {}", c.label),
            value: c.path.clone(),
            kind: "Breadcrumb chevron".into(),
            rect: px(c.rect),
            pos: pos as u32 + 1,
            expand: if c.expanded { Expand::Expanded } else { Expand::Collapsed },
            selectable: false,
            invokable: true,
            ..Default::default()
        })
        .collect();
    let size = items.len() as u32;
    push(VSurface {
        key: skey(slot, PART_CRUMBS),
        tree: false,
        name: "Path bar".into(),
        desc: format!("{size} folder segment{}", if size == 1 { "" } else { "s" }),
        rect: px(bar),
        focused: false,
        multi: false,
        size,
        items,
    });
}

// ---- Path edit autocomplete ---------------------------------------------------

/// One dropdown row: (label, folder it opens, row rect, highlighted).
pub struct SuggestUia {
    pub label: String,
    pub path: String,
    pub rect: Rect,
    pub selected: bool,
}

/// Publishes the autocomplete rows as a `PART_SUGGEST` list (row key = its index).
pub fn suggestions(slot: usize, area: Rect, rows: &[SuggestUia]) {
    let items: Vec<VItem> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| VItem {
            key: i as u64,
            name: r.label.clone(),
            value: r.path.clone(),
            kind: "Folder suggestion".into(),
            rect: px(r.rect),
            selected: r.selected,
            pos: i as u32 + 1,
            selectable: false,
            invokable: true,
            ..Default::default()
        })
        .collect();
    let size = items.len() as u32;
    push(VSurface {
        key: skey(slot, PART_SUGGEST),
        tree: false,
        name: "Address suggestions".into(),
        desc: format!("{size} suggestion{}", if size == 1 { "" } else { "s" }),
        rect: px(area),
        focused: false,
        multi: false,
        size,
        items,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_keys_hold_pane_and_part() {
        let k = skey(1, PART_MILLER + 3);
        assert_eq!(slot_of(k), 1);
        assert_eq!(part_of(k), PART_MILLER + 3);
        assert_ne!(skey(0, PART_LIST), skey(1, PART_LIST));
        assert_ne!(skey(0, PART_LIST), skey(0, PART_PANE));
    }

    #[test]
    fn tree_keys_round_trip() {
        for kind in [Kind::Entry, Kind::Loading, Kind::Empty, Kind::Denied, Kind::Loop, Kind::Capped] {
            for (slot, id) in [(0, 0), (7, 123_456), (49_999, u32::MAX)] {
                assert_eq!(untree_key(tree_key(slot, kind, id)), Some((slot, kind, id)));
            }
        }
        assert_eq!(untree_key(9 << 32), None);
        assert_ne!(tree_key(1, Kind::Entry, 5), tree_key(1, Kind::Loading, 5));
    }

    #[test]
    fn counts_group_thousands_and_name_the_selection() {
        assert_eq!(counts(1, 0), "1 item");
        assert_eq!(counts(0, 0), "0 items");
        assert_eq!(counts(1_204, 3), "1,204 items, 3 selected");
        assert_eq!(counts(2_000_000, 0), "2,000,000 items");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000), "1,000");
    }

    #[test]
    fn leaf_names_the_folder() {
        assert_eq!(leaf("C:\\Users\\me\\Docs"), "Docs");
        assert_eq!(leaf("C:\\Users\\me\\Docs\\"), "Docs");
        assert_eq!(leaf("C:\\"), "C:");
        assert_eq!(leaf(""), "");
    }

    #[test]
    fn rows_are_placed_by_scroll_and_cut_to_the_viewport() {
        let area = Rect::new(10.0, 100.0, 300.0, 66.0);
        let mut ls = ListState::new(22.0);
        ls.scroll = 30.0;
        ls.view_h = 66.0;
        // row 1 starts at y = 100 + 22 - 30 = 92: cut at the top edge
        let r = list_row(area, &ls, 1);
        assert_eq!((r[0], r[1], r[2]), (10.0, 100.0, 300.0));
        assert!((r[3] - 14.0).abs() < 0.01, "{r:?}");
        // row 3 starts at 136 and ends at 158: whole
        assert_eq!(list_row(area, &ls, 3), [10.0, 136.0, 300.0, 22.0]);
        // row 4 starts at 158, cut at the bottom edge (166)
        let r = list_row(area, &ls, 4);
        assert!((r[3] - 8.0).abs() < 0.01, "{r:?}");
    }
}
