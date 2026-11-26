//! The parts of the view that change files: inline rename, batch rename,
//! drag and drop between the sidebar and the panes, copy / move to the other
//! pane, undo and redo. A child module of `view`, so it reaches the view's
//! fields; the rules live in `rename`, `dnd` and `ops`, the drawing in
//! `editing` and `batchui`.

use super::*;
use crate::batchui::{self, BatchEdit, BatchOut};
use crate::dnd::{self, DragPayload, DropAction};
use crate::editing::{self, RenameEdit, RenameOut};
use crate::overlay;
use crate::rename::{self, BatchItem};
use crate::tab::Restore;
use std::sync::OnceLock;
use std::time::Instant;

const ID_RENAME: Id = Id::new("rename-edit");
const ID_BATCH: Id = Id::new("batch-pattern");
/// Pointer travel (DIPs) before a press on a row becomes a drag.
const DRAG_START: f32 = 5.0;
/// A new item that has not shown in its folder's list by now is not renamed.
const RENAME_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// A new file or folder waiting to show in the list, to be renamed.
pub struct RenameWait {
    pub folder: String,
    pub name: String,
    pub since: Instant,
}

/// Milliseconds since the first call (a monotonic clock for click timing).
pub fn now_ms() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Files being dragged inside the app.
pub struct Drag {
    pub payload: DragPayload,
    /// the single dragged folder (it can be pinned)
    pub folder: Option<String>,
    /// text of the chip's hint line, set by whatever the pointer is over
    pub hint: String,
    /// pointer of the last frame it was drawn at
    pub last: Pt,
}

/// The press that started on a row of the active pane.
/// Where a drop in a pane is highlighted: a list row, or a rect (Miller's rows and columns).
#[derive(Clone, Copy)]
enum DropRow {
    Row(usize),
    Rect(Rect),
}

/// A drag rests this long on a collapsed Tree folder before it opens.
const HOVER_OPEN: std::time::Duration = std::time::Duration::from_millis(650);

pub struct PressInfo {
    pub pos: Pt,
    /// a slow second click on the only selected row: renames on release
    pub slow: bool,
    /// a plain press on a multi-selection: collapses to this row on release
    /// unless it turned into a drag
    pub collapse: Option<usize>,
}

impl View {
    // ---- pointer: slow click rename, drag start ------------------------------

    /// Once per frame, with the pointer state of the frame before. `held`: a
    /// widget is held now. Returns true when the pointer was released.
    pub(super) fn track_pointer(&mut self, ui: &mut Ui, held: bool) -> bool {
        let list_id = pid(self.active, ID_LIST);
        let pressed_now = held && !self.ptr_was_active;
        let released = !held && self.ptr_was_active;
        if !held {
            self.ignore_press = false;
        }
        if pressed_now && !self.ignore_press {
            self.ops.message = None;
            self.press = None;
            let keep = super::KEEP_MULTI.with(|k| k.take());
            if ui.active() == Some(list_id) && self.rename.is_none() && !self.tab().list.has_band() && !self.tab().grid.has_band() {
                let pos = ui.mouse();
                let ms = now_ms();
                self.press = match self.row_at(pos) {
                    Some(row) => {
                        let prev = self.last_click.filter(|(r, _)| *r == row).map(|(_, t)| t);
                        // slow-click rename: details rows only (a grid label is small, archives are read-only)
                        let plain = !self.cur_mods.ctrl && !self.cur_mods.shift && !self.mode.is_grid() && !self.tab().is_archive();
                        let slow = plain && rename::is_slow_click(prev, self.sole_hist[0] == Some(row), ms);
                        self.last_click = Some((row, ms));
                        Some(PressInfo { pos, slow, collapse: keep.filter(|k| *k == row) })
                    }
                    None => {
                        self.last_click = None;
                        None
                    }
                };
            }
        }
        if held
            && self.drag.is_none()
            && ui.active() == Some(list_id)
            && let Some(p) = &self.press
        {
            let m = ui.mouse();
            if (m.x - p.pos.x).hypot(m.y - p.pos.y) > DRAG_START {
                self.start_drag();
            }
        }
        if released
            && let Some(p) = self.press.take()
            && self.drag.is_none()
        {
            if let Some(row) = p.collapse {
                let t = self.tab_mut();
                if t.miller_on && let Some(m) = t.miller.as_mut() {
                    let a = m.active;
                    m.select_pos(a, row);
                } else if t.tree_on && let Some(m) = t.tree.as_mut() {
                    m.select_single(row, Some(&t.store));
                } else {
                    t.sel.single(&t.store, row);
                }
                ui.request_repaint();
            }
            if p.slow {
                self.begin_rename(ui);
            }
        }
        released
    }

    /// View row (details) or cell (grid) under `p` in the live pane.
    pub(super) fn row_at(&self, p: Pt) -> Option<usize> {
        if !self.list_r.contains_pt(p) {
            return None;
        }
        if self.miller_showing() {
            // the row under the pointer, when it is in the active column
            let m = self.tab().miller.as_ref()?;
            let (c, pos) = m.hit_row(p.x, p.y)?;
            return (c == m.active).then_some(pos);
        }
        let n = self.tab().shown();
        if self.mode.is_grid() {
            return self.tab().grid.cell_at(p.x - self.list_r.x, p.y - self.list_r.y, n);
        }
        let l = &self.tab().list;
        let row = ((p.y - self.list_r.y) as f64 + l.scroll) / l.row_h as f64;
        let row = row as usize;
        (row < self.tab().shown()).then_some(row)
    }

    /// The press moved far enough: main runs the drag as an OLE drag
    /// (`shellops::drag_out`), so it can end anywhere, here or in Explorer.
    fn start_drag(&mut self) {
        let paths = self.tab().selected_paths(crate::shellops::MAX_ITEMS);
        self.press = None;
        if !paths.is_empty() && !self.tab().synthetic && !self.tab().is_archive() {
            self.drag_out = Some(paths);
        }
    }

    /// A drag came over the window (ours or another program's).
    pub fn drag_enter(&mut self, paths: Vec<String>) {
        let folder = (paths.len() == 1).then(|| paths[0].clone()).filter(|p| std::path::Path::new(p).is_dir());
        self.drag = Some(Drag { payload: DragPayload::new(paths), folder, hint: String::new(), last: Pt { x: -1.0, y: -1.0 } });
        self.drop_dest = None;
    }

    /// The drag left the window, was dropped or cancelled.
    pub fn drag_leave(&mut self) {
        self.drag = None;
        self.hover_open = None;
        self.drop_dest = None;
        self.drag_keys = None;
    }

    /// A folder dropped between pinned rows.
    pub fn pin_dropped(&mut self, path: &str, at: usize) {
        if self.side.pin_at(path, at) {
            self.ops.message = Some(format!("Pinned {}", dnd::name_of(path)));
            self.config_dirty = true;
        }
    }

    /// Esc: closes the rename / batch or cancels a marquee band (a drag is OLE's: Esc cancels it there).
    pub(super) fn cancel_edits(&mut self) -> bool {
        if self.batch.take().is_some() {
            self.focus_list = true;
            return true;
        }
        if self.rename.take().is_some() {
            self.focus_list = true;
            return true;
        }
        let t = self.tab_mut();
        if t.list.has_band() || t.grid.has_band() {
            t.list.cancel_band();
            t.grid.cancel_band();
            t.band_base.clear();
            return true;
        }
        false
    }

    /// A folder or tab change ends whatever was being edited.
    pub(super) fn close_edits(&mut self) {
        self.rename = None;
        self.batch = None;
        self.press = None;
        let t = self.tab_mut();
        t.list.cancel_band();
        t.grid.cancel_band();
        t.band_base.clear();
    }

    // ---- inline rename -------------------------------------------------------

    /// F2: renames the cursor row; several selected rows open the batch bar.
    pub fn begin_rename(&mut self, ui: &mut Ui) {
        if self.tab().is_archive() {
            self.ops.message = Some(crate::archive_ui::READ_ONLY_MSG.into());
            ui.request_repaint();
            return;
        }
        // Tree rows and Miller columns rename the focused row inline; the batch bar is Details' (its names come from one listing)
        let other_view = self.tree_rows_on() || self.miller_showing();
        if self.tab().sel_total() > 1 {
            if other_view {
                self.ops.message = Some("Batch rename is in Details. Renaming the focused item.".into());
            } else if crate::batchui::is_enabled() {
                return self.begin_batch(ui);
            } else {
                // extension off: rename the focused item and say so
                self.ops.message = Some(crate::batchui::OFF_MSG.into());
            }
        }
        let cursor = if self.mode.is_grid() { self.tab().grid.cursor } else { self.tab().cursor_row() };
        let t = self.tab_mut();
        let Some((path, dir)) = t.entry_at(cursor) else { return };
        if !other_view && t.sel.count() == 0 {
            t.sel.single(&t.store, cursor);
        }
        self.batch = None;
        self.rename = Some(RenameEdit::new(&path, dir));
        self.rename_slot = self.active;
        ui.set_focus(Some(pid(self.live, ID_RENAME)));
        ui.request_repaint();
    }

    /// The rect of the rename field for view row `pos`, when the row is in view.
    fn rename_rect(&self, pos: usize, spans: &crate::cols::Spans) -> Option<Rect> {
        let r = self.list_r;
        if self.mode.is_grid() {
            let g = &self.tab().grid;
            let (ox, oy) = g.cell_origin(pos);
            let f = Rect::new(r.x + ox + 4.0, r.y + oy + g.cell_h - 44.0, g.cell_w - 8.0, 26.0);
            return (f.y >= r.y && f.b() <= r.b()).then_some(f);
        }
        let l = &self.tab().list;
        let y = r.y + l.row_top(pos) as f32;
        if y < r.y - 0.5 || y + l.row_h > r.b() + 0.5 {
            return None;
        }
        Some(Rect::new(r.x + spans[0].0 + 28.0, y + 1.0, (spans[0].1 - 28.0 - 4.0).max(80.0), l.row_h - 2.0))
    }

    /// Draws the rename field of the live pane, if it is the one renaming.
    pub(super) fn paint_rename(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, spans: &crate::cols::Spans) {
        if self.rename.is_none() || self.rename_slot != self.live || self.live != self.active {
            return;
        }
        if self.miller_showing() {
            return;
        }
        if self.tree_rows_on() && self.tab().tree.is_some() {
            return self.paint_rename_tree(ui, g, t, spans);
        }
        let tab = &self.tabs.items[self.tabs.active];
        let (path, parent) = {
            let e = self.rename.as_ref().unwrap();
            (e.path.clone(), e.parent.clone())
        };
        // the entry may be gone (renamed, deleted): close the field
        let name = crate::dnd::name_of(&path);
        let w16: Vec<u16> = name.encode_utf16().collect();
        let pos = tab.store.find(&w16).and_then(|id| tab.store.view().iter().position(|v| *v == id));
        if !tab.path.eq_ignore_ascii_case(&parent) || pos.is_none() {
            self.rename = None;
            self.focus_list = true;
            return;
        }
        let pos = pos.unwrap_or(0);
        let Some(rect) = self.rename_rect(pos, spans) else { return };
        let tab = &self.tabs.items[self.tabs.active];
        let exists = |n: &str| {
            let w: Vec<u16> = n.encode_utf16().collect();
            tab.store.find(&w).is_some()
        };
        let edit = self.rename.as_mut().unwrap();
        let out = editing::paint(ui, g, t, pid(self.live, ID_RENAME), rect, edit, &exists);
        if out != RenameOut::None {
            self.finish_rename(ui, out);
        }
    }

    /// The rename field on a Tree row: over the name, indented like the row.
    fn paint_rename_tree(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, spans: &crate::cols::Spans) {
        let path = self.rename.as_ref().map(|e| e.path.clone()).unwrap_or_default();
        let tab = &self.tabs.items[self.tabs.active];
        let root = Some(&tab.store);
        let Some(m) = tab.tree.as_ref() else { return };
        // the row may be gone (renamed, deleted, its folder collapsed): close the field
        let Some(row) = m.row_of_path(&path, root) else {
            self.rename = None;
            self.focus_list = true;
            return;
        };
        let Some(tr) = m.row(row) else { return };
        let l = &tab.list;
        let r = self.list_r;
        let y = r.y + l.row_top(row) as f32;
        if y < r.y - 0.5 || y + l.row_h > r.b() + 0.5 {
            return;
        }
        let name_x = r.x + spans[0].0 + 4.0 + crate::tree::indent(tr.depth) + super::treev::CHEV_W + ICON_DIP + 6.0;
        let x = name_x - 6.0;
        let w = (r.x + spans[0].0 + spans[0].1 - x - 4.0).max(80.0);
        let rect = Rect::new(x, y + 1.0, w, l.row_h - 2.0);
        let sib = m.siblings_of(row, root);
        let exists = |n: &str| {
            let w: Vec<u16> = n.encode_utf16().collect();
            sib.is_some_and(|s| s.find(&w).is_some())
        };
        let edit = self.rename.as_mut().unwrap();
        let out = editing::paint(ui, g, t, pid(self.live, ID_RENAME), rect, edit, &exists);
        if out != RenameOut::None {
            self.finish_rename(ui, out);
        }
    }

    /// The rename field on a Miller row of the column that lists the entry.
    pub(super) fn paint_rename_miller(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme) {
        if self.rename.is_none() || self.rename_slot != self.live || self.live != self.active {
            return;
        }
        let (path, parent) = {
            let e = self.rename.as_ref().unwrap();
            (e.path.clone(), e.parent.clone())
        };
        let tab = &self.tabs.items[self.tabs.active];
        let Some(m) = tab.miller.as_ref() else { return };
        let name = crate::dnd::name_of(&path);
        let w16: Vec<u16> = name.encode_utf16().collect();
        let found = (0..m.len()).find(|&i| m.col(i).is_some_and(|c| crate::miller::same_path(&c.path, &parent))).and_then(|ci| {
            let c = m.col(ci)?;
            let pos = c.store.find(&w16).and_then(|id| c.store.view().iter().position(|v| *v == id))?;
            Some((ci, pos))
        });
        let Some((ci, pos)) = found else {
            self.rename = None;
            self.focus_list = true;
            return;
        };
        let Some(row) = m.row_rect(ci, pos) else { return };
        let x = row.x + 10.0 + ICON_DIP + 8.0 - 6.0;
        let rect = Rect::new(x, row.y + 1.0, (row.r() - x - 6.0).max(80.0), row.h - 2.0);
        let store = m.col(ci).map(|c| &c.store);
        let exists = |n: &str| {
            let w: Vec<u16> = n.encode_utf16().collect();
            store.is_some_and(|s| s.find(&w).is_some())
        };
        let edit = self.rename.as_mut().unwrap();
        let out = editing::paint(ui, g, t, pid(self.live, ID_RENAME), rect, edit, &exists);
        if out != RenameOut::None {
            self.finish_rename(ui, out);
        }
    }

    pub(super) fn finish_rename(&mut self, ui: &mut Ui, out: RenameOut) {
        self.finish_rename_checked(ui, out, false);
    }

    /// `confirmed`: the user already said yes to a changed file name extension.
    fn finish_rename_checked(&mut self, ui: &mut Ui, out: RenameOut, confirmed: bool) {
        let Some(e) = self.rename.take() else { return };
        self.focus_list = true;
        let step = matches!(out, RenameOut::CommitNext | RenameOut::CommitPrev) && !self.tree_rows_on() && !self.miller_showing();
        if out == RenameOut::Cancel || !(e.unchanged() || e.valid() || matches!(out, RenameOut::Commit { replace: true })) {
            ui.request_repaint();
            return;
        }
        // Tab / Shift+Tab move on even when the name stayed the same
        if e.unchanged() {
            if step {
                self.rename_neighbour(ui, &e.path, out == RenameOut::CommitNext);
            }
            ui.request_repaint();
            return;
        }
        // Explorer asks before a file's extension changes; `shellops::after_frame` shows the
        // question (a dialog can not open inside the frame) and calls `ext_answer`
        if !confirmed && rename::ext_change_needs_confirm(&e.original, e.name(), e.is_dir) {
            self.ext_ask = Some((e, out));
            ui.request_repaint();
            return;
        }
        let to = crate::nav::join(&e.parent, e.name());
        let replace = matches!(out, RenameOut::Commit { replace: true });
        if self.ops.rename(&e.path, &to, replace) {
            self.ops.message = Some(format!("Renaming to \"{}\"...", e.name()));
            self.tags_moved(&[(e.path.clone(), to.clone())]);
            // Tree rows and Miller columns select the renamed entry once their listing shows it
            let t = self.tab_mut();
            if t.miller_on && let Some(m) = t.miller.as_mut() {
                m.want_pick(&e.parent, e.name());
            } else if t.tree_on && let Some(m) = t.tree.as_mut() {
                m.want_pick(&to);
            }
        }
        if step {
            self.rename_neighbour(ui, &e.path, out == RenameOut::CommitNext);
        }
        ui.request_repaint();
    }

    /// The answer to the extension question (`shellops::after_frame` asked it).
    /// Yes: the rename goes on. No: the field comes back with the typed name.
    pub fn ext_answer(&mut self, ui: &mut Ui, yes: bool) {
        let Some((mut e, out)) = self.ext_ask.take() else { return };
        if yes {
            self.rename = Some(e);
            self.finish_rename_checked(ui, out, true);
        } else {
            // the field takes the focus again before its "lost the focus" rule can end it
            e.frames = 0;
            self.rename = Some(e);
            self.focus_list = false;
            ui.set_focus(Some(pid(self.live, ID_RENAME)));
        }
        ui.request_repaint();
    }

    /// Tab / Shift+Tab: the row after / before the one that was just renamed.
    fn rename_neighbour(&mut self, ui: &mut Ui, prev: &str, forward: bool) {
        let grid = self.mode.is_grid();
        let t = self.tab_mut();
        let name = crate::dnd::name_of(prev);
        let w: Vec<u16> = name.encode_utf16().collect();
        let Some(pos) = t.store.find(&w).and_then(|id| t.store.view().iter().position(|v| *v == id)) else { return };
        let Some(next) = rename::neighbour(pos, t.shown(), forward) else { return };
        t.sel.single(&t.store, next);
        if grid {
            t.grid.cursor = next;
        } else {
            t.list.cursor = next;
            let len = t.shown();
            t.list.ensure_visible(next, len);
        }
        self.begin_rename(ui);
    }

    /// A new item was queued in `folder`: rename it as soon as the listing shows it.
    pub fn rename_when_listed(&mut self, folder: &str, name: &str) {
        self.rename_wait = Some(RenameWait { folder: folder.to_string(), name: name.to_string(), since: std::time::Instant::now() });
    }

    /// Every frame (cheap when nothing waits): starts the rename of a new item
    /// once it is in the list; gives up after a few seconds, when the user
    /// went elsewhere or started another rename.
    pub(super) fn poll_rename_wait(&mut self, ui: &mut Ui) {
        let Some(w) = self.rename_wait.as_ref() else { return };
        let gone = w.since.elapsed() > RENAME_WAIT || self.rename.is_some() || self.batch.is_some() || self.tree_rows_on() || self.miller_showing();
        if gone || !self.tab().path.trim_end_matches('\\').eq_ignore_ascii_case(w.folder.trim_end_matches('\\')) {
            self.rename_wait = None;
            return;
        }
        let wide: Vec<u16> = w.name.encode_utf16().collect();
        let grid = self.mode.is_grid();
        let t = self.tab_mut();
        let Some(pos) = t.store.find(&wide).and_then(|id| t.store.view().iter().position(|v| *v == id)) else { return };
        t.sel.single(&t.store, pos);
        if grid {
            t.grid.cursor = pos;
        } else {
            t.list.cursor = pos;
            let len = t.shown();
            t.list.ensure_visible(pos, len);
        }
        self.rename_wait = None;
        self.begin_rename(ui);
    }

    // ---- batch rename --------------------------------------------------------

    pub fn begin_batch(&mut self, ui: &mut Ui) {
        if self.tab().is_archive() {
            self.ops.message = Some(crate::archive_ui::READ_ONLY_MSG.into());
            ui.request_repaint();
            return;
        }
        let t = self.tab();
        let mut paths = Vec::new();
        let mut items = Vec::new();
        for pos in 0..t.shown() {
            let Some(id) = t.store.at(pos) else { continue };
            if !t.sel.contains(id) {
                continue;
            }
            let Some(rec) = t.store.record(id) else { continue };
            let Some((path, dir)) = t.entry_at(pos) else { continue };
            items.push(BatchItem { name: crate::dnd::name_of(&path).to_string(), is_dir: dir, mtime: rec.modified.clamp(0, u32::MAX as i64) as u32 });
            paths.push(path);
            if paths.len() >= 5_000 {
                break;
            }
        }
        if paths.len() < 2 {
            return;
        }
        let mut names = Vec::with_capacity(t.store.len());
        for id in 0..t.store.id_bound() as u32 {
            if let Some(n) = t.store.name(id) {
                names.push(String::from_utf16_lossy(n));
            }
        }
        let parent = t.path.clone();
        self.rename = None;
        self.batch = Some(BatchEdit::new(paths, items, parent, names));
        self.batch_slot = self.active;
        ui.set_focus(Some(pid(self.live, ID_BATCH)));
        ui.request_repaint();
    }

    /// The batch bar's height in the live pane (0 when it is not shown here).
    pub(super) fn batch_h(&self) -> f32 {
        if self.batch.is_some() && self.batch_slot == self.live { batchui::BAR_H } else { 0.0 }
    }

    /// Draws the bar in `bar` and, after everything else, the preview.
    pub(super) fn paint_batch_bar(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, bar: Rect) {
        let id = pid(self.live, ID_BATCH);
        let Some(b) = self.batch.as_mut() else { return };
        match batchui::paint_bar(ui, g, t, id, bar, b) {
            BatchOut::Apply => self.apply_batch(ui),
            BatchOut::Cancel => {
                self.batch = None;
                self.focus_list = true;
            }
            BatchOut::None => {}
        }
        if let Some(b) = self.batch.as_ref() {
            self.batch_bar = bar;
            let _ = b;
        }
    }

    pub(super) fn paint_batch_preview(&mut self, g: &mut dyn Canvas, t: &Theme, win: (f32, f32)) {
        if let Some(b) = self.batch.as_ref() {
            batchui::paint_preview(g, t, win, self.batch_bar, b);
        }
    }

    fn apply_batch(&mut self, ui: &mut Ui) {
        let Some(b) = self.batch.take() else { return };
        self.focus_list = true;
        let pairs = b.pairs();
        if !b.plan.can_apply() || pairs.is_empty() {
            return;
        }
        let n = pairs.len();
        if self.ops.rename_batch(&pairs) {
            self.ops.message = Some(format!("Renaming {n} items..."));
            self.tags_moved(&pairs);
        }
        ui.request_repaint();
    }

    // ---- file operations -----------------------------------------------------

    /// Re-reads `i` of the live pane keeping scroll and selection (`names`
    /// replaces the selection when not empty).
    pub(super) fn reload_tab(&mut self, i: usize, names: &[String]) {
        let wake = self.wake.clone();
        let t = &mut self.tabs.items[i];
        let row = (t.list.scroll / t.list.row_h as f64) as usize;
        let selected = if names.is_empty() { t.selected_paths(512).iter().map(|p| crate::dnd::name_of(p).to_string()).collect() } else { names.to_vec() };
        let path = t.path.clone();
        t.start(&path, wake);
        t.restore = Some(Restore { scroll_first_row: row, selected });
    }

    /// After file-operation events (`shellops::on_ops_timer`): the jobs that
    /// ended. Watched tabs follow by themselves (their watcher sees the
    /// change); they only get the names to select once the new items show
    /// up. Unwatched tabs showing a changed folder are re-read keeping
    /// scroll and selection. Every tab of both panes, not just the visible one.
    pub fn after_ops(&mut self) {
        let done = self.ops.take_finished();
        if done.is_empty() {
            return;
        }
        let panes = if self.other.is_some() { 2 } else { 1 };
        for slot in 0..panes {
            self.with_pane(slot, |v| {
                for i in 0..v.tabs.items.len() {
                    let path = v.tabs.items[i].path.clone();
                    if v.tabs.items[i].synthetic {
                        continue;
                    }
                    let same = |d: &String| d.trim_end_matches('\\').eq_ignore_ascii_case(path.trim_end_matches('\\'));
                    let mut reload = false;
                    let mut names: Vec<String> = Vec::new();
                    for f in &done {
                        if f.folder.as_ref().is_some_and(same) {
                            names.extend(f.select.iter().cloned());
                        }
                        reload |= f.history || f.touched.iter().any(same);
                    }
                    if !reload && names.is_empty() {
                        continue;
                    }
                    let t = &mut v.tabs.items[i];
                    if t.watched() {
                        if !names.is_empty() {
                            t.want_select(names);
                        }
                    } else if reload {
                        v.reload_tab(i, &names);
                    }
                }
            });
        }
        self.want_drives = true;
    }

    /// File operations are running: main keeps a timer for their events (not
    /// while the conflict dialog waits or the running job is paused).
    pub fn ops_busy(&self) -> bool {
        self.ops.wants_timer()
    }

    pub(super) fn undo_op(&mut self) {
        self.ops.undo();
    }

    pub(super) fn redo_op(&mut self) {
        self.ops.redo();
    }

    /// The active pane's list has the keyboard focus (file shortcuts apply).
    pub fn list_focused(&self, ui: &Ui) -> bool {
        !self.editing && self.rename.is_none() && self.batch.is_none() && self.ctx.is_none() && self.palette.is_none() && self.search.is_none() && !self.preview.quick_look_open() && ui.has_focus(pid(self.active, ID_LIST))
    }

    /// F5 / F6 with two panes: the selection goes to the other pane's folder.
    pub(super) fn copy_or_move_to_other(&mut self, mv: bool) {
        let Some(o) = self.other.as_deref() else { return };
        let dest = o.tabs.items[o.tabs.active].path.clone();
        if self.tab().is_archive() || crate::archive_ui::is_archive_path(&dest) {
            self.ops.message = Some(crate::archive_ui::READ_ONLY_MSG.into());
            return;
        }
        let paths = self.tab().selected_paths(10_000);
        if paths.is_empty() {
            self.ops.message = Some("Select something first".into());
            return;
        }
        let payload = DragPayload::new(paths);
        if let Some(why) = dnd::refusal(&payload, &dest) {
            self.ops.message = Some(why.into());
            return;
        }
        let action = if mv { DropAction::Move } else { DropAction::Copy };
        let steps = dnd::plan(&payload, &dest, action);
        self.ops.submit_steps(&steps, &dest);
    }

    /// Files dropped on `folder` (a sidebar row, a folder row, a pane).
    pub fn drop_selection_on(&mut self, folder: &str) {
        let Some(d) = self.drag.take() else { return };
        if crate::archive_ui::is_archive_path(folder) {
            self.ops.message = Some(crate::archive_ui::READ_ONLY_MSG.into());
            return;
        }
        if let Some(why) = dnd::refusal(&d.payload, folder) {
            self.ops.message = Some(why.into());
            return;
        }
        let action = crate::ops::action_for(&d.payload.paths, folder, self.cur_mods);
        let steps = dnd::plan(&d.payload, folder, action);
        self.ops.submit_steps(&steps, folder);
    }

    // ---- drag: targets inside the panes, chip -----------------------------------

    /// The folder a drop at `m` lands on inside the panes: a folder row, else
    /// the pane's own folder.
    fn pane_drop_folder(&mut self, m: Pt) -> Option<(usize, String, Option<DropRow>)> {
        let slots: &[usize] = if self.other.is_some() { &[0, 1] } else { &[0] };
        for &slot in slots {
            let hit = if self.other.is_some() { self.pane_rects[slot].contains_pt(m) } else { self.list_r.contains_pt(m) };
            if !hit {
                continue;
            }
            return self.with_pane(slot, |v| {
                // Miller: a folder row of any column, else the folder of the column under the pointer
                if v.miller_showing() && let Some(mm) = v.tab().miller.as_ref() {
                    let hit = mm.hit_row(m.x, m.y);
                    if let Some(c) = hit.map(|h| h.0).or_else(|| mm.hit_col(m.x, m.y)) {
                        let col = mm.col(c)?;
                        if let Some((_, pos)) = hit
                            && col.is_folder_at(pos)
                            && let Some(name) = col.entry_name(pos)
                            && let Some(r) = mm.row_rect(c, pos)
                        {
                            return Some((slot, crate::nav::join(&col.path, &name), Some(DropRow::Rect(r))));
                        }
                        return mm.col_rows(c).map(|r| (slot, col.path.clone(), Some(DropRow::Rect(r))));
                    }
                    return Some((slot, v.tab().path.clone(), None));
                }
                let row = v.row_at(m);
                // Tree rows: a folder row is the target, a file row drops into its own folder
                if v.tree_rows_on() && let Some(mm) = v.tab().tree.as_ref() {
                    let root = Some(&v.tab().store);
                    if let Some(r) = row
                        && let Some(folder) = mm.drop_folder_of(r, root)
                    {
                        let hl = if mm.is_folder(r, root) { Some(r) } else { mm.parent_row(r) };
                        return Some((slot, folder, hl.map(DropRow::Row)));
                    }
                    return Some((slot, v.tab().path.clone(), None));
                }
                let folder = row.and_then(|r| v.tab().entry_at(r)).filter(|(_, dir)| *dir).map(|(p, _)| p);
                match folder {
                    Some(p) => Some((slot, p, row.map(DropRow::Row))),
                    None => Some((slot, v.tab().path.clone(), None)),
                }
            });
        }
        None
    }

    /// The tab under `m` in a pane's strip: (slot, folder of the tab; None
    /// for a tab that is not a real folder).
    fn tab_drop_folder(&mut self, m: Pt, th: f32, min_w: f32, max_w: f32) -> Option<(usize, Option<String>)> {
        let panes = if self.other.is_some() { 2 } else { 1 };
        for slot in 0..panes {
            let (strip, scroll) = self.strips[slot];
            let hit = Rect::new(strip.x, strip.b() - th, strip.w, th);
            if strip.w <= 0.0 || !hit.contains_pt(m) {
                continue;
            }
            return Some(self.with_pane(slot, |v| {
                // the same layout as tsubame-ui's tab strip (28 + 4 DIPs for "+")
                let n = v.tabs.items.len().max(1);
                let w = ((hit.w - 32.0).max(0.0) / n as f32).clamp(min_w, max_w);
                let i = ((m.x - (hit.x - scroll)) / w).floor();
                let tab = (i >= 0.0).then(|| v.tabs.items.get(i as usize)).flatten();
                let folder = match tab {
                    Some(t) if !t.synthetic => Some(t.path.clone()),
                    Some(_) => None,
                    None => (!v.tab().synthetic).then(|| v.tab().path.clone()),
                };
                (slot, folder)
            }));
        }
        None
    }

    /// While a drag is over the window (after the sidebar and the panes are
    /// drawn): works out where a drop would land (`drop_dest`, read by the
    /// OLE drop target) and draws the chip, the hint and the highlight.
    pub(super) fn finish_drag(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, side_target: Option<&crate::sidebar::DropTarget>) {
        let Some(d) = self.drag.as_ref() else {
            self.drop_dest = None;
            return;
        };
        self.drop_read_only = false;
        let m = ui.mouse();
        let mods = self.drag_keys.unwrap_or(self.cur_mods);
        let paths = d.payload.paths.clone();
        let count = paths.len();
        let first = d.payload.first_name().to_string();
        let is_folder = d.folder.is_some();
        // where would it land?
        let mut hint = String::new();
        let mut dest: Option<String> = None;
        let mut pin: Option<usize> = None;
        let mut tag: Option<u8> = None;
        let mut over_pane: Option<(usize, Option<DropRow>)> = None;
        let mut over_tab = false;
        match side_target {
            Some(crate::sidebar::DropTarget::Pin(k)) => {
                hint = "Pin to sidebar".into();
                pin = Some(*k);
            }
            Some(crate::sidebar::DropTarget::Folder(p)) => dest = Some(p.clone()),
            Some(crate::sidebar::DropTarget::Tag(s)) => {
                hint = self.tag_drop_hint(*s, count);
                tag = Some(*s);
            }
            None => {
                if let Some((_, folder)) = self.tab_drop_folder(m, t.height_tab, t.layout_tab_min_width, t.layout_tab_width) {
                    over_tab = true;
                    dest = folder;
                } else if let Some((slot, folder, row)) = self.pane_drop_folder(m) {
                    over_pane = Some((slot, row));
                    dest = Some(folder);
                }
            }
        }
        if let Some(dir) = &dest {
            if crate::archive_ui::is_archive_path(dir) {
                dest = None;
                hint = "Archives are read-only".into();
                self.drop_read_only = true;
            } else {
                let payload = DragPayload::new(paths.clone());
                hint = match dnd::refusal(&payload, dir) {
                    Some(why) => {
                        dest = None;
                        why.to_string()
                    }
                    None => format!("{} {}", crate::ops::action_for(&paths, dir, mods).verb(), dnd::name_of(dir)),
                };
            }
        }
        self.drop_dest = match (pin, &dest) {
            _ if tag.is_some() => tag.map(DropDest::Tag),
            (Some(k), _) => Some(DropDest::Pin(k)),
            (None, Some(dir)) => Some(DropDest::Folder(dir.clone())),
            _ => None,
        };
        let _ = over_tab;
        // highlight the pane or the folder row under the pointer
        if let (Some((slot, row)), Some(_)) = (over_pane, &dest) {
            let slot_rect = if self.other.is_some() { self.pane_rects[slot] } else { self.list_r };
            let r = match row {
                Some(DropRow::Rect(r)) => r,
                Some(DropRow::Row(r)) => self.with_pane(slot, |v| {
                    if v.mode.is_grid() {
                        let gs = &v.tab().grid;
                        let (ox, oy) = gs.cell_origin(r);
                        Rect::new(v.list_r.x + ox, v.list_r.y + oy, gs.cell_w, gs.cell_h)
                    } else {
                        let l = &v.tab().list;
                        Rect::new(v.list_r.x, v.list_r.y + l.row_top(r) as f32, v.list_r.w - t.layout_scrollbar_width, l.row_h)
                    }
                }),
                None => slot_rect,
            };
            g.round_stroke(r.pad(-1.0), t.radius_sm, t.accent, 2.0);
            self.tree_hover_expand(ui, slot, m);
        } else {
            self.hover_open = None;
        }
        if let Some(d) = self.drag.as_mut() {
            d.hint.clone_from(&hint);
        }
        overlay::drag_chip(g, t, ui.size(), m, &first, count, is_folder);
        if !hint.is_empty() {
            overlay::tooltip(g, t, ui.size(), Pt { x: m.x, y: m.y + 18.0 }, &hint, &[]);
        }
        // the chip follows the pointer: one more frame per move, none when it rests
        if let Some(d) = self.drag.as_mut()
            && (d.last.x != m.x || d.last.y != m.y)
        {
            d.last = m;
            ui.request_repaint();
        }
    }

    /// A drag rests on a collapsed Tree folder: it opens after `HOVER_OPEN`, so the drop can go deeper.
    fn tree_hover_expand(&mut self, ui: &mut Ui, slot: usize, m: Pt) {
        let wake = self.wake.clone();
        let mut hover = self.hover_open.take();
        let opened = self.with_pane(slot, |v| {
            if !v.tree_rows_on() {
                hover = None;
                return false;
            }
            let Some(i) = v.row_at(m) else {
                hover = None;
                return false;
            };
            let t = v.tab_mut();
            let root = Some(&t.store);
            let Some(mm) = t.tree.as_mut() else {
                hover = None;
                return false;
            };
            let Some((path, true)) = mm.path_of(i, root) else {
                hover = None;
                return false;
            };
            if mm.is_expanded(i, root) {
                hover = None;
                return false;
            }
            match &hover {
                Some((p, since)) if crate::miller::same_path(p, &path) => {
                    if since.elapsed() >= HOVER_OPEN {
                        mm.expand(i, false, root);
                        hover = None;
                        return true;
                    }
                }
                _ => {
                    hover = Some((path, Instant::now()));
                    // nothing else wakes an idle frame while the pointer rests
                    let _ = std::thread::Builder::new().name("tsubame-hover-open".into()).stack_size(64 * 1024).spawn(move || {
                        std::thread::sleep(HOVER_OPEN + std::time::Duration::from_millis(30));
                        wake();
                    });
                }
            }
            false
        });
        self.hover_open = hover;
        if opened {
            ui.request_repaint();
        }
    }

    // ---- the status bar's message ----------------------------------------------

    pub(super) fn status_message(&self) -> Option<&str> {
        self.archive_ops
            .status
            .as_deref()
            .or(self.meta.status.as_deref())
            .or(self.ops.message.as_deref())
    }
}
