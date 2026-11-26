//! What a screen reader may ask of the file views (`docs/uia-views.md`): select a
//! row, open it, expand or collapse a tree row. The providers queue the request
//! (`tsubame_win::uia::take_view_requests`); the window's `WM_APP_UIA` handler
//! calls [`View::run_view_requests`] on the UI thread. Each request does what the
//! keyboard or the mouse does for that row, nothing more: no disk access, and a
//! row that has since gone is dropped.

use super::*;
use crate::tree::TreeModel;
use crate::uia_rows::{self, PART_CRUMBS, PART_LIST, PART_MILLER, PART_PANE, PART_SUGGEST};
use tsubame_win::uia::{VAct, VReq};

impl View {
    /// Runs the requests screen readers queued since the last call.
    pub fn run_view_requests(&mut self) {
        for r in tsubame_win::uia::take_view_requests() {
            let slot = uia_rows::slot_of(r.surf);
            if slot > 1 || (slot == 1 && self.other.is_none()) {
                continue;
            }
            self.with_pane(slot, |v| v.run_view_request(r));
        }
    }

    pub(crate) fn run_view_request(&mut self, r: VReq) {
        match uia_rows::part_of(r.surf) {
            PART_LIST => self.a11y_list(r),
            PART_PANE => self.a11y_pane(r),
            PART_CRUMBS => match r.act {
                VAct::Invoke | VAct::Expand => self.a11y_crumb(r.key as usize, true),
                VAct::Collapse => self.a11y_crumb(r.key as usize, false),
                _ => {}
            },
            PART_SUGGEST => {
                if r.act == VAct::Invoke {
                    self.suggest_invoke(r.key as usize);
                }
            }
            p if p >= PART_MILLER => self.a11y_miller((p - PART_MILLER) as usize, r),
            _ => {}
        }
    }

    /// Details, Grid and Tree rows.
    fn a11y_list(&mut self, r: VReq) {
        if self.tree_rows_on() && self.tab().tree.is_some() {
            return self.a11y_tree_row(r);
        }
        let tab = &mut self.tabs.items[self.tabs.active];
        // the row's entry id is its key; its position is found in the view (the
        // list may have changed since the snapshot: a missing row is dropped)
        let id = r.key as u32;
        let Some(pos) = tab.store.view().iter().position(|v| *v == id) else { return };
        let had = tab.sel.contains(id);
        match r.act {
            VAct::Select => tab.sel.single(&tab.store, pos),
            VAct::AddToSelection if !had => tab.sel.toggle(&tab.store, pos),
            VAct::RemoveFromSelection if had => tab.sel.toggle(&tab.store, pos),
            VAct::Invoke => {
                tab.list.cursor = pos;
                tab.grid.cursor = pos;
                return self.activate(pos);
            }
            _ => return,
        }
        tab.list.cursor = pos;
        tab.grid.cursor = pos;
    }

    /// A Tree-rows row: its key holds the tree model's slot, kind and entry id.
    fn a11y_tree_row(&mut self, r: VReq) {
        let tab = &mut self.tabs.items[self.tabs.active];
        let Some(m) = tab.tree.as_mut() else { return };
        let Some(i) = row_of(m, r.key, Some(&tab.store)) else { return };
        let root = Some(&tab.store);
        let open = m.is_expanded(i, root);
        let selected = m.is_selected(i, root);
        match r.act {
            VAct::Select => m.select_single(i, root),
            VAct::AddToSelection if !selected => m.toggle(i, root),
            VAct::RemoveFromSelection if selected => m.toggle(i, root),
            VAct::Expand if !open => return self.tree_activate_row(i),
            VAct::Collapse if open => return self.tree_activate_row(i),
            VAct::Invoke => return self.tree_activate_row(i),
            _ => return,
        }
        tab.list.cursor = i;
    }

    /// Enter on a Tree row (folder toggle, retry, or open a file).
    fn tree_activate_row(&mut self, i: usize) {
        self.tabs.items[self.tabs.active].list.cursor = i;
        self.activate(i);
    }

    /// The folders-only Tree pane: pick a folder, or expand and collapse it.
    fn a11y_pane(&mut self, r: VReq) {
        let tab = &mut self.tabs.items[self.tabs.active];
        let Some(m) = tab.tree_pane.as_mut() else { return };
        let Some(i) = row_of(m, r.key, None) else { return };
        let open = m.is_expanded(i, None);
        let mut go = None;
        match r.act {
            VAct::Expand if !open => {
                m.expand(i, true, None);
            }
            VAct::Collapse if open => {
                m.collapse(i, None);
            }
            VAct::Select | VAct::Invoke => {
                if m.row(i).is_some_and(|row| row.kind == crate::tree::Kind::Entry) {
                    go = m.folder_path_of(i, None);
                } else if r.act == VAct::Invoke {
                    // a Denied row's Retry
                    if let Some(row) = m.row(i) {
                        m.relist(row.slot);
                    }
                }
            }
            _ => return,
        }
        m.flatten(None);
        if let Some(c) = m.cursor_row(None) {
            tab.pane_list.cursor = c;
        }
        if let Some(p) = go {
            m.mark_following(&p);
            if !p.eq_ignore_ascii_case(&self.tab().path) {
                self.go(&p);
            }
        }
    }

    /// Miller column `c`: select the row (it becomes the path), or open it.
    fn a11y_miller(&mut self, c: usize, r: VReq) {
        let a = self.tabs.active;
        let Some(mut m) = self.tabs.items[a].miller.take() else { return };
        let id = r.key as u32;
        let pos = m.col(c).and_then(|col| col.store.view().iter().position(|v| *v == id));
        if let Some(pos) = pos {
            match r.act {
                VAct::Select => {
                    m.select_pos(c, pos);
                    self.focus_list = true;
                }
                VAct::Invoke => {
                    m.select_pos(c, pos);
                    if let crate::miller::Activated::Open(p) = m.activate(c, pos) {
                        self.miller_open(p);
                    }
                }
                _ => {}
            }
        }
        if self.tabs.items[a].miller.is_none() {
            self.tabs.items[a].miller = Some(m);
        }
    }

    /// A file activated in a Miller column: archives browse, the rest opens (as the paint path does).
    fn miller_open(&mut self, p: String) {
        if crate::archive_ui::can_browse(&p) {
            let v = crate::archive_ui::archive_root_path(&p);
            self.go(&v);
        } else if crate::archive_ui::is_archive_path(&p) {
            self.ops.message = Some("Read-only archive \u{2014} use 'Extract selected from archive' to extract files".into());
        } else {
            self.open_file(p);
        }
    }
}

/// The row index of a tree key (`uia_rows::tree_key`) in `m`.
fn row_of(m: &TreeModel, key: u64, root: Option<&EntryStore>) -> Option<usize> {
    let (slot, kind, id) = uia_rows::untree_key(key)?;
    m.row_of(crate::tree::Key { slot, id, kind }, root)
}
