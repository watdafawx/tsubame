//! B11-G tab context menu (right click on a tab): Close, Close other tabs, Close tabs to the
//! right, Duplicate tab, Move to other pane (two panes), Move to new window, Reopen closed tab.
//!
//! The menu is the own-drawn popup (`overlay::menu`). A tab is named by its id, not its index, so
//! the closes that shift indexes stay correct. Every tab a menu action closes goes on the window's
//! reopen stack (`record_closed_tab`, Ctrl+Shift+T).
//!
//! "Move to new window" starts a new Tsubame process on the tab's folder and closes the tab here
//! only after the process started: the spawn worker reports the tab id back (`tab_windows`) and
//! `pump_tab_windows` closes it. A failed spawn leaves the tab where it is.

use super::*;
use crate::overlay::{self, MenuItem};
use std::sync::{Arc, Mutex};

const ID_TAB_MENU: Id = Id::new("tab-menu");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAction {
    Close,
    CloseOthers,
    CloseRight,
    Duplicate,
    MoveToPane,
    MoveToWindow,
    Reopen,
}

/// One row of the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabRow {
    pub action: TabAction,
    pub label: &'static str,
    pub keys: &'static str,
    pub enabled: bool,
    pub sep: bool,
}

/// What decides which rows are enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuCtx {
    /// the tab's index in its pane
    pub index: usize,
    /// tabs in its pane
    pub count: usize,
    pub dual: bool,
    /// tabs in the whole window (both panes)
    pub window_tabs: usize,
    /// a real folder tab (not Settings or a synthetic listing)
    pub folder: bool,
    pub can_reopen: bool,
}

/// The rows for a tab. "Move to other pane" only exists with two panes.
pub fn rows(c: &MenuCtx) -> Vec<TabRow> {
    let row = |action, label, keys, enabled, sep| TabRow { action, label, keys, enabled, sep };
    let mut v = vec![
        row(TabAction::Close, "Close", "Ctrl+W", true, false),
        row(TabAction::CloseOthers, "Close other tabs", "", c.count > 1, false),
        row(TabAction::CloseRight, "Close tabs to the right", "", c.index + 1 < c.count, false),
        row(TabAction::Duplicate, "Duplicate tab", "", c.folder, true),
    ];
    if c.dual {
        v.push(row(TabAction::MoveToPane, "Move to other pane", "", c.count > 1, false));
    }
    v.push(row(TabAction::MoveToWindow, "Move to new window", "", c.folder && c.window_tabs > 1, false));
    v.push(row(TabAction::Reopen, "Reopen closed tab", "Ctrl+Shift+T", c.can_reopen, true));
    v
}

/// The indexes `action` closes in a pane of `count` tabs with the menu on `index`, last first
/// (closing from the end keeps the ones still to close where they are).
pub fn victims(action: TabAction, index: usize, count: usize) -> Vec<usize> {
    match action {
        TabAction::Close if index < count => vec![index],
        TabAction::CloseOthers => (0..count).rev().filter(|&i| i != index).collect(),
        TabAction::CloseRight => ((index + 1)..count).rev().collect(),
        _ => Vec::new(),
    }
}

/// Starts a Tsubame window on `folder` and tells `done` (and wakes the UI) with `tab` when the
/// process is running. A failure reports nothing. Off the UI thread, no console window.
pub fn spawn_window_for_tab(exe: &std::path::Path, folder: String, portable: bool, tab: u64, done: Arc<Mutex<Vec<u64>>>, wake: tsubame_win::enumerate::Wake) {
    let exe = exe.to_path_buf();
    let args = navv::build_new_window_args(&folder, portable);
    let _ = std::thread::Builder::new().name("tsubame-tab-window".into()).spawn(move || {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        if std::process::Command::new(&exe).args(&args).creation_flags(CREATE_NO_WINDOW).spawn().is_ok() {
            done.lock().unwrap_or_else(|e| e.into_inner()).push(tab);
            wake();
        }
    });
}

/// The open menu: (pane slot, tab id, where it opened).
pub type OpenMenu = (usize, u64, Pt);

impl View {
    /// A right click on tab `i` of the live pane at `at`.
    pub(super) fn open_tab_menu(&mut self, ui: &mut Ui, i: usize, at: Pt) {
        let Some(id) = self.tabs.items.get(i).map(|t| t.id) else { return };
        self.ctx = None;
        ui.close_all_popups();
        self.tab_menu = Some((self.live, id, at));
        ui.open_popup(pid(self.live, ID_TAB_MENU));
        ui.request_repaint();
    }

    fn tab_menu_ctx(&self, slot: usize, id: u64) -> Option<MenuCtx> {
        let tabs = self.pane_tabs(slot)?;
        let index = tabs.items.iter().position(|t| t.id == id)?;
        let t = &tabs.items[index];
        let window_tabs = self.pane_tabs(0).map_or(0, |t| t.items.len()) + if self.other.is_some() { self.pane_tabs(1).map_or(0, |t| t.items.len()) } else { 0 };
        Some(MenuCtx { index, count: tabs.items.len(), dual: self.other.is_some(), window_tabs, folder: !t.settings && !t.synthetic && !t.path.is_empty(), can_reopen: !self.closed_tabs.is_empty() })
    }

    /// Draws the menu (last in the frame, like every popup) and runs the row that was picked.
    pub(super) fn paint_tab_menu(&mut self, ui: &mut Ui, g: &mut dyn Canvas) {
        let Some((slot, id, at)) = self.tab_menu else { return };
        let pop = pid(slot, ID_TAB_MENU);
        let Some(ctx) = self.tab_menu_ctx(slot, id).filter(|_| ui.is_popup_open(pop)) else {
            self.tab_menu = None;
            ui.close_popup(pop);
            return;
        };
        let rows = rows(&ctx);
        let items: Vec<MenuItem> = rows
            .iter()
            .map(|r| {
                let m = MenuItem::new(r.label, r.keys).off(!r.enabled);
                if r.sep { m.sep() } else { m }
            })
            .collect();
        let Some(picked) = overlay::menu(ui, g, pop, at, &items) else { return };
        self.tab_menu = None;
        if let Some(r) = rows.get(picked) {
            self.run_tab_action(ui, slot, id, r.action);
        }
        ui.request_repaint();
    }

    /// Runs `action` on the tab `id` of pane `slot`.
    pub fn run_tab_action(&mut self, ui: &mut Ui, slot: usize, id: u64, action: TabAction) {
        if action == TabAction::Reopen {
            return self.reopen_closed_tab();
        }
        if slot == 1 && self.other.is_none() {
            return;
        }
        self.with_pane(slot, |v| v.run_tab_action_here(ui, id, action));
    }

    /// `action` on the tab `id` of the live pane.
    fn run_tab_action_here(&mut self, ui: &mut Ui, id: u64, action: TabAction) {
        let Some(i) = self.tabs.items.iter().position(|t| t.id == id) else { return };
        match action {
            TabAction::Close => self.close_tab(i),
            TabAction::CloseOthers | TabAction::CloseRight => {
                let ids: Vec<u64> = victims(action, i, self.tabs.items.len()).into_iter().filter_map(|j| self.tabs.items.get(j).map(|t| t.id)).collect();
                self.close_tabs_by_id(&ids);
                if let Some(k) = self.tabs.items.iter().position(|t| t.id == id) {
                    self.switch_tab(k);
                }
            }
            TabAction::Duplicate => self.duplicate_tab(i),
            TabAction::MoveToPane => {
                if self.other.is_some() && self.tabs.items.len() > 1 {
                    self.tabs.activate(i);
                    self.run_command(ui, CMD_TAB_TO_OTHER);
                }
            }
            TabAction::MoveToWindow => self.move_tab_to_window(i),
            TabAction::Reopen => {}
        }
    }

    /// Closes the tabs `ids` of the live pane (never the last one of it).
    fn close_tabs_by_id(&mut self, ids: &[u64]) {
        for id in ids {
            if self.tabs.items.len() <= 1 {
                break;
            }
            if let Some(j) = self.tabs.items.iter().position(|t| t.id == *id) {
                self.close_tab(j);
            }
        }
    }

    /// A second tab on the same folder with the same sort, next to the first and in front.
    fn duplicate_tab(&mut self, i: usize) {
        let Some(src) = self.tabs.items.get(i) else { return };
        if src.settings || src.synthetic || src.path.is_empty() {
            return;
        }
        let (path, sort) = (src.path.clone(), src.sort);
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut t = Tab::new(id, &path, self.density.row_h());
        t.sort = sort;
        t.start(&path, self.wake.clone());
        self.tabs.insert(i + 1, t);
        self.switched();
    }

    /// Opens the tab's folder in a new window; the tab closes here once that window started.
    fn move_tab_to_window(&mut self, i: usize) {
        let Some(t) = self.tabs.items.get(i) else { return };
        if t.settings || t.synthetic || t.path.is_empty() {
            return;
        }
        spawn_window_for_tab(&self.env.exe, t.path.clone(), self.env.portable, t.id, self.tab_windows.clone(), self.wake.clone());
    }

    /// Closes the tabs whose new window started (called from `pump`). The last tab of the window
    /// stays: it would end this window too.
    pub(super) fn pump_tab_windows(&mut self) -> bool {
        let done: Vec<u64> = {
            let mut q = self.tab_windows.lock().unwrap_or_else(|e| e.into_inner());
            if q.is_empty() {
                return false;
            }
            std::mem::take(&mut *q)
        };
        for id in done {
            for slot in 0..2 {
                if slot == 1 && self.other.is_none() {
                    break;
                }
                let Some(pos) = self.with_pane(slot, |v| v.tabs.items.iter().position(|t| t.id == id)) else { continue };
                let total = self.pane_tabs(0).map_or(0, |t| t.items.len()) + if self.other.is_some() { self.pane_tabs(1).map_or(0, |t| t.items.len()) } else { 0 };
                if total > 1 {
                    self.with_pane(slot, |v| v.close_tab(pos));
                }
                break;
            }
        }
        true
    }

    /// The commands of the menu, for the palette and keys.
    pub(super) fn run_tab_menu_cmd(&mut self, ui: &mut Ui, cmd: u32) -> bool {
        let action = match cmd {
            CMD_CLOSE_OTHER_TABS => TabAction::CloseOthers,
            CMD_CLOSE_TABS_RIGHT => TabAction::CloseRight,
            CMD_DUPLICATE_TAB => TabAction::Duplicate,
            CMD_TAB_TO_WINDOW => TabAction::MoveToWindow,
            _ => return false,
        };
        let live = self.live;
        let id = self.tab().id;
        let enabled = self.tab_menu_ctx(live, id).is_some_and(|c| rows(&c).iter().any(|r| r.action == action && r.enabled));
        if enabled {
            self.run_tab_action(ui, live, id, action);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> MenuCtx {
        MenuCtx { index: 1, count: 3, dual: false, window_tabs: 3, folder: true, can_reopen: false }
    }
    fn enabled(c: &MenuCtx, a: TabAction) -> Option<bool> {
        rows(c).iter().find(|r| r.action == a).map(|r| r.enabled)
    }

    #[test]
    fn rows_follow_the_tab_the_pane_and_the_window() {
        let c = ctx();
        assert_eq!(enabled(&c, TabAction::Close), Some(true));
        assert_eq!(enabled(&c, TabAction::CloseOthers), Some(true));
        assert_eq!(enabled(&c, TabAction::CloseRight), Some(true), "tab 1 of 3 has one on its right");
        assert_eq!(enabled(&c, TabAction::Duplicate), Some(true));
        assert_eq!(enabled(&c, TabAction::MoveToPane), None, "no such row with one pane");
        assert_eq!(enabled(&c, TabAction::MoveToWindow), Some(true));
        assert_eq!(enabled(&c, TabAction::Reopen), Some(false), "nothing closed yet");
        // the last tab: nothing on its right
        assert_eq!(enabled(&MenuCtx { index: 2, ..c }, TabAction::CloseRight), Some(false));
        // the only tab: nothing to close besides itself, nowhere to move to
        let one = MenuCtx { index: 0, count: 1, window_tabs: 1, ..c };
        assert_eq!(enabled(&one, TabAction::CloseOthers), Some(false));
        assert_eq!(enabled(&one, TabAction::MoveToWindow), Some(false), "it would end this window");
        // two panes: the pane row shows, and needs a second tab in this pane
        let dual = MenuCtx { dual: true, ..c };
        assert_eq!(enabled(&dual, TabAction::MoveToPane), Some(true));
        assert_eq!(enabled(&MenuCtx { count: 1, index: 0, window_tabs: 2, ..dual }, TabAction::MoveToPane), Some(false));
        assert_eq!(enabled(&MenuCtx { window_tabs: 2, count: 1, index: 0, ..dual }, TabAction::MoveToWindow), Some(true), "the other pane still has a tab");
        // Settings: no duplicate, no new window
        let settings = MenuCtx { folder: false, ..c };
        assert_eq!(enabled(&settings, TabAction::Duplicate), Some(false));
        assert_eq!(enabled(&settings, TabAction::MoveToWindow), Some(false));
        assert_eq!(enabled(&MenuCtx { can_reopen: true, ..c }, TabAction::Reopen), Some(true));
    }

    #[test]
    fn victims_are_listed_last_first_and_never_include_the_menu_tab() {
        assert_eq!(victims(TabAction::Close, 2, 5), vec![2]);
        assert_eq!(victims(TabAction::Close, 9, 5), Vec::<usize>::new());
        assert_eq!(victims(TabAction::CloseOthers, 2, 5), vec![4, 3, 1, 0]);
        assert_eq!(victims(TabAction::CloseRight, 2, 5), vec![4, 3]);
        assert_eq!(victims(TabAction::CloseRight, 4, 5), Vec::<usize>::new());
        assert_eq!(victims(TabAction::CloseOthers, 0, 1), Vec::<usize>::new());
        assert_eq!(victims(TabAction::Duplicate, 0, 3), Vec::<usize>::new());
    }
}
