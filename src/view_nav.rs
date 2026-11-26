//! B10-B Explorer parity helpers on [`View`]: reopen closed tab, new window,
//! reveal current folder in sidebar, Back/Forward history dropdown (right-click
//! and long-press) and breadcrumb sibling chevrons.
//!
//! Kept in its own submodule to minimize edits in `src/view.rs`.

use std::path::Path;

use super::*;
use crate::nav_menu::{HoldState, NavMenu, NavMenuOut};
use crate::uia_rows::CrumbUia;

/// Pure command-line args for spawning a new Tsubame window (`--new-window`).
pub fn build_new_window_args(folder: &str, portable: bool) -> Vec<String> {
    let mut args = vec!["--new-window".to_string()];
    if portable {
        args.push("--portable".to_string());
    }
    let dir = if crate::archive_ui::is_archive_path(folder) {
        let host = tsubame_core::archive::host_archive_path(folder);
        crate::archive_ui::host_folder_of(host).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        folder.to_string()
    };
    if !dir.is_empty() {
        args.push(dir);
    }
    args
}

/// Spawns a new Tsubame window on a short background thread with
/// `CREATE_NO_WINDOW` so the UI thread never blocks and no console flashes.
pub fn spawn_new_window(exe: &Path, folder: String, portable: bool) {
    let exe = exe.to_path_buf();
    let args = build_new_window_args(&folder, portable);
    let _ = std::thread::Builder::new().name("tsubame-new-window".into()).spawn(move || {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new(&exe).args(&args).creation_flags(CREATE_NO_WINDOW).spawn();
    });
}

impl View {
    /// Records tab `i` onto the window's bounded reopen stack before it closes.
    pub(super) fn record_closed_tab(&mut self, i: usize) {
        let mode = self.mode.to_config();
        if let Some(snap) = self.tabs.items.get(i).and_then(|t| t.snapshot_closed(mode, i)) {
            self.closed_tabs.push(snap);
        }
    }

    /// `Ctrl+Shift+T` (`tab.reopen`): restores the most recently closed tab
    /// (path, view mode, sort, scroll row) into the active pane.
    pub fn reopen_closed_tab(&mut self) {
        let Some(c) = self.closed_tabs.pop() else { return };
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut t = Tab::new(id, &c.path, self.density.row_h());
        t.sort = c.sort;
        t.start(&c.path, self.wake.clone());
        if c.scroll_first_row > 0 {
            t.restore = Some(crate::tab::Restore { scroll_first_row: c.scroll_first_row, selected: Vec::new() });
        }
        self.tabs.insert(c.index, t);
        self.set_mode(ViewMode::from_config(c.view_mode));
        self.switched();
    }

    /// `Ctrl+N` (`window.new`): queues spawning a new window on the current folder.
    pub fn open_new_window(&mut self, folder: Option<&str>) {
        let p = folder.map(str::to_string).unwrap_or_else(|| self.folder_path());
        self.open_windows.push(p);
    }

    /// `Ctrl+Shift+E` (`sidebar.reveal-current`): opens the sidebar if collapsed,
    /// unfolds the matching section and scrolls the current folder into view.
    pub fn reveal_in_sidebar(&mut self, ui: &mut Ui) {
        let p = self.folder_path();
        let layout_changed = self.side.reveal(&p);
        if layout_changed {
            self.side_act(ui, Some(Act::Layout));
        } else if self.side.dirty {
            self.side.dirty = false;
            self.config_dirty = true;
        }
        ui.request_repaint();
    }

    /// Jumps the active tab's history directly to `index` (from the history dropdown).
    pub fn go_history(&mut self, index: usize) {
        if self.tab().settings {
            return;
        }
        let wake = self.wake.clone();
        let t = self.tab_mut();
        if let Some(p) = t.history.go_to(index).map(str::to_string) {
            t.start(&p, wake);
            self.after_nav();
        }
    }

    /// Opens the Back or Forward history dropdown anchored at `anchor` for the live pane.
    pub fn open_history_menu(&mut self, ui: &mut Ui, forward: bool, anchor: Pt) {
        let tab = self.tab();
        if tab.settings {
            return;
        }
        let raw: Vec<(usize, String)> = if forward {
            tab.history.forward_entries(crate::nav_menu::HISTORY_CAP)
        } else {
            tab.history.back_entries(crate::nav_menu::HISTORY_CAP)
        }
        .into_iter()
        .map(|(i, p)| (i, p.to_string()))
        .collect();
        if let Some(m) = NavMenu::history(self.live, forward, anchor, raw) {
            self.ctx = None;
            ui.close_popup(crate::ctxui::ID_CTX.with(0));
            self.nav_menu = Some(Box::new(m));
            ui.request_repaint();
        }
    }

    /// Opens the breadcrumb sibling subfolder menu for crumb `idx` of the live pane.
    pub fn open_crumb_menu(&mut self, ui: &mut Ui, idx: usize, anchor: Pt) {
        let Some(c) = self.crumbs.crumbs.get(idx) else { return };
        let label = c.label.clone();
        let dir_path = c.path.clone();
        let active_child = self.crumbs.crumbs.get(idx + 1).map(|next| next.label.clone());
        let m = NavMenu::crumb(self.live, idx, label, dir_path, active_child, anchor, self.wake.clone());
        self.ctx = None;
        ui.close_popup(crate::ctxui::ID_CTX.with(0));
        self.nav_menu = Some(Box::new(m));
        ui.request_repaint();
    }

    /// Toolbar Back / Forward button with left-click navigate, right-click history
    /// dropdown, and long-press history dropdown.
    pub(super) fn paint_nav_btn(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, enabled: bool, forward: bool) {
        let t = ui.theme;
        let id = pid(self.live, if forward { ID_FWD } else { ID_BACK });
        let glyph = if forward { "\u{E72A}" } else { "\u{E72B}" };
        let it = ui.interact(id, r, Sense::click());
        if enabled && it.hovered {
            g.round(r, t.radius_md, if it.active { t.selected_inactive } else { t.hover });
        }
        g.text(glyph, r, f(t.icon_toolbar).icon().c(), if enabled { t.text } else { t.text_disabled });
        self.tip_cmd(ui, id, r, if forward { "Forward (right click: history)" } else { "Back (right click: history)" }, if forward { CMD_FORWARD } else { CMD_BACK });
        if !enabled {
            return;
        }
        let anchor = Pt::new(r.x, r.b() + 2.0);
        if let Some(p) = it.pressed
            && p.button == Button::Left
        {
            self.nav_hold = Some(HoldState::new(self.live, forward));
        }
        let mut fired_hold = false;
        if let Some(h) = self.nav_hold.as_mut()
            && h.slot == self.live
            && h.forward == forward
        {
            if it.active {
                if h.ready() {
                    h.fired = true;
                    fired_hold = true;
                }
            } else if it.clicked == Some(Button::Left) {
                let was_fired = h.fired;
                self.nav_hold = None;
                if !was_fired {
                    if forward { self.forward() } else { self.back() }
                }
            } else {
                self.nav_hold = None;
            }
        } else if it.clicked == Some(Button::Left) {
            if forward { self.forward() } else { self.back() }
        }
        if fired_hold || it.clicked == Some(Button::Right) {
            self.open_history_menu(ui, forward, anchor);
        }
    }

    /// Renders visible breadcrumb segments and a sibling-folder chevron after each
    /// segment (including the last). Publishes UIA chevron items when a client listens.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_crumbs_with_chevrons(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bar: Rect, t: &Theme, first: usize, n: usize, mut x: f32) {
        let mut go = None;
        let mut new_tab = None;
        let mut open_chev: Option<(usize, Pt)> = None;
        let mut close_chev = false;
        let mut right_click: Option<Pt> = None;
        let open_idx = self.nav_menu.as_ref().and_then(|m| m.open_crumb()).filter(|(slot, _)| *slot == self.live).map(|(_, i)| i);
        let a11y = crate::uia_rows::on();
        let mut uia_items: Vec<CrumbUia> = Vec::new();
        self.crumb_anchors[self.live].clear();

        for i in first..n {
            let last = i + 1 == n;
            let wd = self.crumbs.widths[i] + 2.0 * CRUMB_PAD;
            let r = Rect::new(x, bar.y + 3.0, wd, bar.h - 6.0);
            let hit = ui.interact(pid(self.live, ID_PATH).child("crumb").with(i as u64), r, Sense::click().cursor(Cursor::Hand));
            if hit.hovered {
                g.round(r, t.radius_sm, t.hover);
            }
            let font = if last { t.font_md().b() } else { t.font_md() };
            g.text(&self.crumbs.crumbs[i].label, Rect::new(x + CRUMB_PAD, bar.y, self.crumbs.widths[i] + 2.0, bar.h), font, if last { t.text } else { t.text_dim });
            if hit.clicked == Some(Button::Left) {
                go = Some(self.crumbs.crumbs[i].path.clone());
            } else if hit.clicked == Some(Button::Middle) {
                new_tab = Some(self.crumbs.crumbs[i].path.clone());
            }
            if hit.clicked == Some(Button::Right) {
                right_click = Some(hit.click_pos);
            }
            x += wd;

            // Sibling / subfolder chevron after every segment (including last)
            let chev_r = Rect::new(x, bar.y + 3.0, SEP_W, bar.h - 6.0);
            let is_open = open_idx == Some(i);
            let chev_hit = ui.interact(pid(self.live, ID_PATH).child("chev").with(i as u64), chev_r, Sense::click().cursor(Cursor::Hand));
            if is_open {
                g.round(chev_r, t.radius_sm, t.selected);
            } else if chev_hit.hovered {
                g.round(chev_r, t.radius_sm, t.hover);
            }
            let glyph = if is_open { "\u{E70D}" } else { "\u{E76C}" };
            g.text(glyph, Rect::new(x, bar.y, SEP_W, bar.h), f(8.0).icon().c(), if is_open || chev_hit.hovered { t.text } else { t.text_dim });
            let anchor = Pt::new(chev_r.x, bar.b() + 2.0);
            self.crumb_anchors[self.live].push((i, anchor));
            if chev_hit.clicked == Some(Button::Left) {
                if is_open {
                    close_chev = true;
                } else {
                    open_chev = Some((i, anchor));
                }
            } else if chev_hit.hovered && open_idx.is_some() && !is_open {
                // Explorer behaviour: while a breadcrumb dropdown is open, hovering
                // another chevron switches the dropdown to that segment.
                open_chev = Some((i, anchor));
            }
            if a11y {
                uia_items.push(CrumbUia {
                    index: i,
                    label: self.crumbs.crumbs[i].label.clone(),
                    path: self.crumbs.crumbs[i].path.clone(),
                    rect: chev_r,
                    expanded: is_open,
                });
            }
            x += SEP_W;
        }
        g.unclip();
        if a11y && !uia_items.is_empty() {
            crate::uia_rows::crumbs(self.live, bar, &uia_items);
        }
        if let Some(p) = go {
            self.nav_menu = None;
            self.go(&p);
        } else if let Some(p) = new_tab {
            self.open_in_new_tab(&p);
            ui.request_repaint();
        } else if let Some(p) = right_click {
            self.open_path_menu(ui, Pt::new(p.x, bar.b() + 2.0));
        } else if close_chev {
            self.nav_menu = None;
            ui.close_popup(crate::nav_menu::ID_NAV_MENU.with(0));
            ui.request_repaint();
        } else if let Some((idx, anchor)) = open_chev {
            self.open_crumb_menu(ui, idx, anchor);
        }
    }

    /// Paints the active navigation dropdown (history or breadcrumb subfolders)
    /// on top of the frame.
    pub(super) fn paint_nav_menu(&mut self, ui: &mut Ui, g: &mut dyn Canvas, size: (f32, f32)) {
        let Some(mut m) = self.nav_menu.take() else {
            ui.close_popup(crate::nav_menu::ID_NAV_MENU.with(0));
            return;
        };
        let pane = m.pane;
        let out = m.paint(ui, g, Rect::new(0.0, 0.0, size.0, size.1));
        match out {
            NavMenuOut::None => self.nav_menu = Some(m),
            NavMenuOut::Closed => {
                self.refocus_list_after_overlay(ui);
            }
            NavMenuOut::GoHistory(idx) => {
                self.with_pane(pane, |v| v.go_history(idx));
                ui.request_repaint();
            }
            NavMenuOut::GoFolder(path) => {
                self.with_pane(pane, |v| v.go(&path));
                ui.request_repaint();
            }
            NavMenuOut::Command(cmd) => {
                self.with_pane(pane, |v| v.run_command(ui, cmd));
                ui.request_repaint();
            }
        }
    }

    /// Pumps background enumeration for an open breadcrumb menu.
    pub(super) fn pump_nav_menu(&mut self) -> bool {
        self.nav_menu.as_mut().is_some_and(|m| m.pump())
    }

    /// One-shot timer interval (ms) needed only while holding Back/Forward.
    pub fn nav_menu_tick_ms(&self) -> Option<u32> {
        self.nav_hold.as_ref().and_then(|h| h.next_tick_ms())
    }

    pub fn nav_menu_open(&self) -> bool {
        self.nav_menu.as_ref().is_some_and(|m| m.open)
    }

    pub fn run_nav_menu_a11y(&mut self) {
        if let Some(m) = self.nav_menu.as_mut() {
            m.run_a11y_requests();
        }
    }

    /// UIA invoke/expand on a breadcrumb chevron (`PART_CRUMBS`).
    pub(super) fn a11y_crumb(&mut self, idx: usize, open: bool) {
        if !open {
            if self.nav_menu.as_ref().and_then(|m| m.open_crumb()) == Some((self.live, idx)) {
                self.nav_menu = None;
            }
            return;
        }
        let anchor = self.crumb_anchors[self.live]
            .iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, a)| *a)
            .unwrap_or(Pt::new(120.0, 68.0));
        let Some(c) = self.crumbs.crumbs.get(idx) else { return };
        let label = c.label.clone();
        let dir_path = c.path.clone();
        let active_child = self.crumbs.crumbs.get(idx + 1).map(|next| next.label.clone());
        let m = NavMenu::crumb(self.live, idx, label, dir_path, active_child, anchor, self.wake.clone());
        self.ctx = None;
        self.nav_menu = Some(Box::new(m));
    }

    #[cfg(test)]
    pub fn closed_tabs_len(&self) -> usize {
        self.closed_tabs.len()
    }
}
