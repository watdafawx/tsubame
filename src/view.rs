//! The window's contents on the `tsubame-ui` kit, per the mock: title bar
//! with tabs and window controls, toolbar (back/forward/up, breadcrumb or
//! editable path, filter box), sidebar, details list with sortable and
//! resizable columns, status bar. All coordinates are DIPs.
//!
//! The view owns the tabs (`Tabs<Tab>`) and the UI state that is not per tab
//! (path edit, filter box text, column widths, density, theme). It never
//! touches the disk: listings run on enumerator workers (`Tab::start`), and
//! what the platform must do (shell menu, open a file, window commands) is
//! left in `menu_request`, `open_paths` and `win_cmd` for main.rs.
//!
//! `paint` takes `&mut dyn Canvas`, so the tests drive whole frames on the
//! kit's `TestCanvas`.

use crate::cols::{Col, Columns};
use crate::data::{self, RowBuf};
use crate::nav::{self, Crumb};
use crate::shellui::ShellUi;
use crate::sidebar::Sidebar;
use crate::tab::{Load, Tab};
use crate::tabs::Tabs;
use tsubame_core::{EntryStore, Selection};
use tsubame_ui::{
    Button, Canvas, Cursor, DARK, Density, Event, HeaderCol, Id, Key, LIGHT, ListSource, Mods, Output, Pt, Rect, RowState, SelectBy, Sense, TabStrip, TextInput, Theme, Ui, column_header, f, list, tab_strip, text_input,
};
use tsubame_win::enumerate::Wake;

pub const ICON_DIP: f32 = 16.0;

const ID_TABS: Id = Id::new("tabs");
pub const ID_LIST: Id = Id::new("details");
pub const ID_FILTER: Id = Id::new("filter");
const ID_PATH: Id = Id::new("pathbar");
const ID_PATH_EDIT: Id = Id::new("pathedit");
const ID_HEADER: Id = Id::new("header");
const ID_BACK: Id = Id::new("nav-back");
const ID_FWD: Id = Id::new("nav-forward");
const ID_UP: Id = Id::new("nav-up");
const ID_WIN: Id = Id::new("winctl");

/// App commands (shortcut table values).
pub const CMD_NEW_TAB: u32 = 1;
pub const CMD_CLOSE_TAB: u32 = 2;
pub const CMD_NEXT_TAB: u32 = 3;
pub const CMD_PREV_TAB: u32 = 4;
pub const CMD_FOCUS_PATH: u32 = 5;
pub const CMD_FIND: u32 = 6;
pub const CMD_BACK: u32 = 7;
pub const CMD_FORWARD: u32 = 8;
pub const CMD_UP: u32 = 9;
pub const CMD_THEME: u32 = 10;
pub const CMD_DENSITY: u32 = 11;
pub const CMD_ZEBRA: u32 = 12;
pub const CMD_REFRESH: u32 = 13;
/// non-global: only when no widget took the key
pub const CMD_SELECT_ALL: u32 = 20;
pub const CMD_ESCAPE: u32 = 21;
pub const CMD_BACKSPACE_UP: u32 = 22;

/// What main.rs must do with the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinCmd {
    Minimize,
    MaxRestore,
    Close,
}

/// Binds the keyboard shortcuts (global ones run even in a text field).
pub fn bind_shortcuts(ui: &mut Ui) {
    let s = &mut ui.shortcuts;
    s.bind(Key::Char('T'), Mods::CTRL, CMD_NEW_TAB, true);
    s.bind(Key::Char('W'), Mods::CTRL, CMD_CLOSE_TAB, true);
    s.bind(Key::F(4), Mods::CTRL, CMD_CLOSE_TAB, true);
    s.bind(Key::Tab, Mods::CTRL, CMD_NEXT_TAB, true);
    s.bind(Key::Tab, Mods::CTRL_SHIFT, CMD_PREV_TAB, true);
    s.bind(Key::Char('L'), Mods::CTRL, CMD_FOCUS_PATH, true);
    s.bind(Key::F(4), Mods::NONE, CMD_FOCUS_PATH, true);
    s.bind(Key::Char('D'), Mods::ALT, CMD_FOCUS_PATH, true);
    s.bind(Key::Char('F'), Mods::CTRL, CMD_FIND, true);
    s.bind(Key::Left, Mods::ALT, CMD_BACK, true);
    s.bind(Key::Right, Mods::ALT, CMD_FORWARD, true);
    s.bind(Key::Up, Mods::ALT, CMD_UP, true);
    s.bind(Key::Char('L'), Mods::CTRL_SHIFT, CMD_THEME, true);
    s.bind(Key::Char('D'), Mods::CTRL_SHIFT, CMD_DENSITY, true);
    s.bind(Key::Char('Z'), Mods::CTRL_SHIFT, CMD_ZEBRA, true);
    s.bind(Key::F(5), Mods::NONE, CMD_REFRESH, true);
    s.bind(Key::Char('R'), Mods::CTRL, CMD_REFRESH, true);
    s.bind(Key::Char('A'), Mods::CTRL, CMD_SELECT_ALL, false);
    s.bind(Key::Escape, Mods::NONE, CMD_ESCAPE, false);
    s.bind(Key::Backspace, Mods::NONE, CMD_BACKSPACE_UP, false);
}

/// Breadcrumbs of the shown path with their text widths (recomputed only
/// when the path or the text size changes).
#[derive(Default)]
struct CrumbCache {
    path: String,
    size: u32,
    crumbs: Vec<Crumb>,
    widths: Vec<f32>,
}

pub struct View {
    pub tabs: Tabs<Tab>,
    next_tab_id: u64,
    strip: TabStrip,
    pub filter: TextInput,
    path_text: TextInput,
    /// the path bar is a text field
    pub editing: bool,
    edit_frames: u32,
    /// path-bar error shown under the field
    path_error: Option<String>,
    /// a typed path is being opened: (tab id, folder before, typed path)
    edit_nav: Option<(u64, String, String)>,
    pub cols: Columns,
    pub density: Density,
    pub dark: bool,
    pub zebra: bool,
    pub side: Sidebar,
    wake: Wake,
    rowbuf: Box<RowBuf>,
    text: String,
    crumbs: CrumbCache,
    focus_list: bool,
    pub maximized: bool,
    /// y of the list's top edge (keyboard menus)
    list_top: f32,
    /// rows drawn by the last paint (first, last+1)
    pub last_range: (usize, usize),
    /// the volume of the shown folder changed: main re-reads the drives
    pub want_drives: bool,
    last_drive: char,

    /// the list asked for a context menu: (view row, DIP point); Shift+F10 /
    /// the Menu key give (cursor row, None)
    pub menu_request: Option<(usize, Option<Pt>)>,
    /// files to open with their default handler (main runs them on a worker)
    pub open_paths: Vec<String>,
    pub win_cmd: Option<WinCmd>,
    /// last tab closed: the window should close
    pub quit: bool,
}

impl View {
    pub fn new(start: &str, wake: Wake, dark: bool, density: Density) -> View {
        let mut tab = Tab::new(1, start, density.row_h());
        tab.start(start, wake.clone());
        View {
            tabs: Tabs::new(tab),
            next_tab_id: 2,
            strip: TabStrip::default(),
            filter: TextInput::default(),
            path_text: TextInput::default(),
            editing: false,
            edit_frames: 0,
            path_error: None,
            edit_nav: None,
            cols: Columns::default(),
            density,
            dark,
            zebra: false,
            side: Sidebar::default(),
            wake,
            rowbuf: Box::new(RowBuf::new()),
            text: String::with_capacity(64),
            crumbs: CrumbCache::default(),
            focus_list: true,
            maximized: false,
            list_top: 0.0,
            last_range: (0, 0),
            want_drives: true,
            last_drive: start.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('C'),
            menu_request: None,
            open_paths: Vec::new(),
            win_cmd: None,
            quit: false,
        }
    }

    pub fn base_theme(&self) -> Theme {
        Theme::with_density(if self.dark { &DARK } else { &LIGHT }, self.density)
    }

    /// Pushes theme and density into the kit and every tab's list.
    pub fn apply_theme(&mut self, ui: &mut Ui) {
        ui.theme = self.base_theme();
        let rh = ui.theme.height_list_row;
        for t in &mut self.tabs.items {
            if t.list.row_h != rh {
                t.list.scroll = t.list.scroll / t.list.row_h as f64 * rh as f64;
                t.list.row_h = rh;
            }
        }
        ui.request_repaint();
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs.items[self.tabs.active]
    }
    pub fn tab_mut(&mut self) -> &mut Tab {
        let i = self.tabs.active;
        &mut self.tabs.items[i]
    }

    // ---- navigation ------------------------------------------------------

    /// Opens `path` in the current tab and records it in the history.
    pub fn go(&mut self, path: &str) {
        let wake = self.wake.clone();
        let t = self.tab_mut();
        t.history.push(path);
        t.start(path, wake);
        self.after_nav();
    }

    fn after_nav(&mut self) {
        self.filter.set_text("");
        self.editing = false;
        self.path_error = None;
        self.focus_list = true;
        let d = self.tab().path.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('C');
        if d != self.last_drive {
            self.last_drive = d;
            self.want_drives = true;
        }
    }

    pub fn back(&mut self) {
        let wake = self.wake.clone();
        let t = self.tab_mut();
        if let Some(p) = t.history.back().map(str::to_string) {
            t.start(&p, wake);
            self.after_nav();
        }
    }
    pub fn forward(&mut self) {
        let wake = self.wake.clone();
        let t = self.tab_mut();
        if let Some(p) = t.history.forward().map(str::to_string) {
            t.start(&p, wake);
            self.after_nav();
        }
    }
    pub fn up(&mut self) {
        if let Some(p) = nav::parent(&self.tab().path) {
            self.go(&p);
        }
    }
    pub fn refresh(&mut self) {
        let wake = self.wake.clone();
        let t = self.tab_mut();
        let p = t.path.clone();
        t.start(&p, wake);
        self.after_nav();
        self.want_drives = true;
    }

    pub fn new_tab(&mut self) {
        let path = self.tab().path.clone();
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut t = Tab::new(id, &path, self.density.row_h());
        t.start(&path, self.wake.clone());
        self.tabs.add(t);
        self.filter.set_text("");
        self.editing = false;
        self.focus_list = true;
    }

    /// Closes tab `i`: its store is freed and its enumeration cancelled
    /// (the tab drops). The last tab closing ends the window.
    pub fn close_tab(&mut self, i: usize) {
        drop(self.tabs.close(i));
        if self.tabs.is_empty() {
            self.quit = true;
        } else {
            self.switched();
        }
    }

    pub fn switch_tab(&mut self, i: usize) {
        self.tabs.activate(i);
        self.switched();
    }

    fn switched(&mut self) {
        let f = self.tab().filter.clone();
        self.filter.set_text(&f);
        self.editing = false;
        self.path_error = None;
        self.focus_list = true;
    }

    /// Enter / double click on view row `pos`: a folder opens in this tab, a
    /// file goes to the shell on a worker.
    pub fn activate(&mut self, pos: usize) {
        if let Some((path, dir)) = self.tab().entry_at(pos) {
            if dir { self.go(&path) } else { self.open_paths.push(path) }
        }
    }

    /// Moves everything the enumerators delivered into the stores. UI thread,
    /// no disk. True when something visible changed.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        for t in &mut self.tabs.items {
            changed |= t.pump().changed;
        }
        // a typed path that failed to open: back to the old folder, error shown
        if let Some((id, prev, typed)) = self.edit_nav.take() {
            let wake = self.wake.clone();
            match self.tabs.items.iter_mut().find(|t| t.id == id) {
                Some(t) if matches!(t.load, Load::Failed(_)) => {
                    let msg = match &t.load {
                        Load::Failed(m) => m.clone(),
                        _ => String::new(),
                    };
                    t.history.undo_push();
                    t.start(&prev, wake);
                    let is_active = self.tabs.items[self.tabs.active].id == id;
                    if is_active {
                        self.editing = true;
                        self.edit_frames = 0;
                        self.path_text.set_text(&typed);
                        self.path_text.select_all();
                        self.path_error = Some(format!("Can't open \"{typed}\": {msg}"));
                        self.focus_list = false;
                    }
                }
                Some(t) if t.is_loading() => self.edit_nav = Some((id, prev, typed)),
                _ => {}
            }
        }
        changed
    }

    fn begin_edit(&mut self, ui: &mut Ui) {
        let p = self.tab().path.clone();
        self.path_text.set_text(&p);
        self.path_text.select_all();
        self.editing = true;
        self.edit_frames = 0;
        self.path_error = None;
        ui.set_focus(Some(ID_PATH_EDIT));
        ui.request_repaint();
    }

    fn submit_path(&mut self) {
        let typed = self.path_text.text.trim().to_string();
        match nav::normalize(&typed) {
            Some(p) => {
                let prev = self.tab().path.clone();
                let id = self.tab().id;
                self.go(&p);
                self.edit_nav = Some((id, prev, typed));
            }
            None => self.path_error = Some("Type a full path, like C:\\Users\\you\\Documents.".into()),
        }
    }

    fn set_dark(&mut self, ui: &mut Ui, dark: bool) {
        self.dark = dark;
        self.apply_theme(ui);
    }

    fn run_command(&mut self, ui: &mut Ui, cmd: u32) {
        match cmd {
            CMD_NEW_TAB => self.new_tab(),
            CMD_CLOSE_TAB => self.close_tab(self.tabs.active),
            CMD_NEXT_TAB => {
                self.tabs.next();
                self.switched();
            }
            CMD_PREV_TAB => {
                self.tabs.prev();
                self.switched();
            }
            CMD_FOCUS_PATH => self.begin_edit(ui),
            CMD_FIND => {
                ui.set_focus(Some(ID_FILTER));
                self.filter.select_all();
            }
            CMD_BACK => self.back(),
            CMD_FORWARD => self.forward(),
            CMD_UP | CMD_BACKSPACE_UP => self.up(),
            CMD_THEME => self.set_dark(ui, !self.dark),
            CMD_DENSITY => {
                self.density = self.density.next();
                self.apply_theme(ui);
            }
            CMD_ZEBRA => self.zebra = !self.zebra,
            CMD_REFRESH => self.refresh(),
            CMD_SELECT_ALL => {
                let t = self.tab_mut();
                t.sel.select_all(&t.store);
            }
            CMD_ESCAPE => {
                if !self.tab().filter.is_empty() {
                    self.filter.set_text("");
                    self.tab_mut().set_filter("");
                } else {
                    self.tab_mut().sel.clear();
                }
            }
            _ => {}
        }
        ui.request_repaint();
    }

    /// Client y (DIP) of the middle of view row `i` (keyboard menus).
    pub fn row_mid_y(&self, i: usize) -> f32 {
        let l = &self.tab().list;
        self.list_top + l.row_top(i) as f32 + l.row_h / 2.0
    }

    /// Paths for a context menu on `row`: the selection if `row` is in it,
    /// else just `row` (at most 64).
    pub fn menu_paths(&self, row: usize) -> Vec<String> {
        let t = self.tab();
        if t.store.at(row).is_some_and(|id| t.sel.contains(id)) {
            t.selected_paths(64)
        } else {
            t.entry_at(row).map(|e| vec![e.0]).unwrap_or_default()
        }
    }

    // ---- drawing ---------------------------------------------------------

    /// One frame. None when there is no render target.
    pub fn paint(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi) -> Option<Output> {
        let (w, h) = g.size();
        if !g.begin(ui.theme.bg) {
            return None;
        }
        ui.begin_frame((w, h));
        if !ui.commands().is_empty() {
            for cmd in ui.commands().to_vec() {
                self.run_command(ui, cmd);
            }
        }
        if self.tabs.is_empty() {
            self.quit = true;
            let out = ui.end_frame();
            g.end();
            return Some(out);
        }
        if std::mem::take(&mut self.focus_list) {
            ui.set_focus(Some(ID_LIST));
        }
        let t = ui.theme;
        let mut r = tsubame_ui::Rect::new(0.0, 0.0, w, h);
        let titlebar = r.cut_top(t.height_titlebar);
        let toolbar = r.cut_top(t.height_toolbar);
        let status = r.cut_bottom(t.height_status_bar);
        let sidebar = r.cut_left(t.layout_sidebar_width);
        let main = r;

        // body first: the later widgets (toolbar, title bar) win hit tests
        if let Some(p) = self.side.paint(ui, g, sidebar, &self.tabs.items[self.tabs.active].path) {
            self.go(&p);
        }
        self.paint_main(ui, g, shell, main, &t);
        self.paint_status(g, status, &t);
        self.paint_toolbar(ui, g, toolbar, &t);
        self.paint_titlebar(ui, g, titlebar, &t);
        self.paint_path_error(g, toolbar, &t);

        let n_global = ui.commands().len();
        let mut out = ui.end_frame();
        // commands of keys no widget used (Ctrl+A, Esc, Backspace)
        let late: Vec<u32> = ui.commands()[n_global..].to_vec();
        let mut again = false;
        for cmd in late {
            self.run_command(ui, cmd);
            again = true;
        }
        // typing in the list starts the filter
        let typed: Vec<char> = ui.leftover().iter().filter_map(|e| if let Event::Char(c) = e { Some(*c) } else { None }).collect();
        for c in typed {
            if c.is_control() || (c == ' ' && self.filter.text.is_empty()) {
                continue;
            }
            if !ui.has_focus(ID_FILTER) {
                ui.set_focus(Some(ID_FILTER));
                self.filter.set_text("");
            }
            let mut b = [0u8; 4];
            self.filter.insert(c.encode_utf8(&mut b));
            let text = self.filter.text.clone();
            self.tab_mut().set_filter(&text);
            again = true;
        }
        if again {
            out.repaint = true;
        }
        g.end();
        Some(out)
    }

    fn paint_titlebar(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        g.fill(r, t.bg);
        // app mark
        let mark = Rect::new(r.x + 12.0, r.y + (r.h - 18.0) / 2.0, 18.0, 18.0);
        g.round(mark, t.radius_md, tsubame_ui::theme::BRAND_NAVY);
        g.round_stroke(mark, t.radius_md, t.border, 1.0);
        g.text("\u{2197}", mark, f(12.0).b().c(), tsubame_ui::theme::BRAND_CREAM);
        // window controls
        let cw = t.layout_window_control_width;
        let ctl = Rect::new(r.r() - 3.0 * cw, r.y, 3.0 * cw, r.h);
        let glyphs = ["\u{E921}", if self.maximized { "\u{E923}" } else { "\u{E922}" }, "\u{E8BB}"];
        let cmds = [WinCmd::Minimize, WinCmd::MaxRestore, WinCmd::Close];
        for i in 0..3 {
            let b = Rect::new(ctl.x + i as f32 * cw, ctl.y, cw, ctl.h);
            let it = ui.interact(ID_WIN.with(i as u64), b, Sense::click());
            let close = i == 2;
            if it.hovered {
                g.fill(b, if close { t.danger_fill } else { t.hover });
            }
            g.text(glyphs[i], b, f(t.icon_window_control).icon().c(), if close && it.hovered { t.on_danger } else { t.text });
            if it.clicked == Some(Button::Left) {
                self.win_cmd = Some(cmds[i]);
            }
        }
        // tabs
        let strip_r = Rect::new(r.x + 40.0, r.y, (ctl.x - r.x - 40.0 - 8.0).max(0.0), r.h);
        let titles: Vec<&str> = self.tabs.items.iter().map(|t| t.title()).collect();
        let resp = tab_strip(ui, g, ID_TABS, strip_r, &mut self.strip, &titles, self.tabs.active);
        if let Some(i) = resp.select {
            self.switch_tab(i);
        }
        if let Some((from, to)) = resp.moved {
            self.tabs.move_tab(from, to);
        }
        if let Some(i) = resp.close {
            self.close_tab(i);
        }
        if resp.add {
            self.new_tab();
        }
    }

    fn paint_toolbar(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        g.fill(r, t.panel);
        let by = r.y + (r.h - t.height_button) / 2.0;
        let bs = t.height_button;
        let (can_back, can_fwd, can_up) = {
            let tab = self.tab();
            (tab.history.can_back(), tab.history.can_forward(), nav::parent(&tab.path).is_some())
        };
        let mut x = r.x + t.space_4;
        if icon_button(ui, g, t, ID_BACK, Rect::new(x, by, bs, bs), "\u{E72B}", can_back) {
            self.back();
        }
        x += bs + 2.0;
        if icon_button(ui, g, t, ID_FWD, Rect::new(x, by, bs, bs), "\u{E72A}", can_fwd) {
            self.forward();
        }
        x += bs + 2.0;
        if icon_button(ui, g, t, ID_UP, Rect::new(x, by, bs, bs), "\u{E74A}", can_up) {
            self.up();
        }
        x += bs + t.space_4;

        // filter box on the right, path bar takes the rest
        let fw = t.layout_filter_width.min((r.w - x) / 2.0).max(60.0);
        let fr = Rect::new(r.r() - t.space_4 - fw, r.y + (r.h - t.height_path_bar) / 2.0, fw, t.height_path_bar);
        let bar = Rect::new(x, fr.y, (fr.x - t.space_4 - x).max(40.0), t.height_path_bar);
        self.paint_path_bar(ui, g, bar, t);
        self.paint_filter(ui, g, fr, t);

        // loading line under the toolbar: grows with the entries delivered
        let tab = self.tab();
        if tab.is_loading() {
            let frac = 1.0 - (-(tab.delivered as f32) / 20_000.0).exp();
            let lw = (r.w * frac).max(r.w * 0.02);
            g.fill(Rect::new(r.x, r.b() - t.height_progress_line, lw, t.height_progress_line), t.accent);
        }
    }

    fn paint_filter(&mut self, ui: &mut Ui, g: &mut dyn Canvas, fr: Rect, t: &Theme) {
        self.filter.pad_left = 20.0;
        let resp = text_input(ui, g, ID_FILTER, fr, &mut self.filter, "Filter");
        g.text("\u{E721}", Rect::new(fr.x + 8.0, fr.y, 14.0, fr.h), f(12.0).icon().c(), t.text_dim);
        if self.filter.text.is_empty() && !resp.focused {
            let badge = Rect::new(fr.r() - 54.0, fr.y + 4.0, 48.0, fr.h - 8.0);
            g.round_stroke(badge, t.radius_sm, t.border, 1.0);
            g.text("Ctrl+F", badge, t.font_mono().c(), t.text_dim);
        }
        if resp.changed {
            let text = self.filter.text.clone();
            self.tab_mut().set_filter(&text);
        }
        if resp.cancelled {
            if self.filter.text.is_empty() {
                ui.set_focus(Some(ID_LIST));
            } else {
                self.filter.set_text("");
                self.tab_mut().set_filter("");
            }
            ui.request_repaint();
        }
        if resp.submitted {
            ui.set_focus(Some(ID_LIST));
            ui.request_repaint();
        }
    }

    fn paint_path_bar(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bar: Rect, t: &Theme) {
        if self.editing {
            self.edit_frames += 1;
            self.path_text.pad_left = 22.0;
            let resp = text_input(ui, g, ID_PATH_EDIT, bar, &mut self.path_text, "Type a path");
            g.text("\u{E8B7}", Rect::new(bar.x + 8.0, bar.y, 16.0, bar.h), f(13.0).icon().c(), t.folder);
            if self.path_error.is_some() {
                g.round_stroke(bar, t.radius_md, t.danger, 1.0);
            }
            if resp.changed {
                self.path_error = None;
            }
            if resp.submitted {
                self.submit_path();
                ui.request_repaint();
            } else if resp.cancelled {
                self.editing = false;
                self.path_error = None;
                ui.set_focus(Some(ID_LIST));
                ui.request_repaint();
            } else if !resp.focused && self.edit_frames > 2 {
                // clicked elsewhere
                self.editing = false;
                self.path_error = None;
                ui.request_repaint();
            }
            return;
        }
        // breadcrumb mode
        g.round(bar, t.radius_md, t.field);
        g.round_stroke(bar, t.radius_md, t.border, 1.0);
        let it = ui.interact(ID_PATH, bar, Sense::click().cursor(Cursor::IBeam));
        if it.clicked == Some(Button::Left) {
            self.begin_edit(ui);
            return;
        }
        // cache crumbs and their widths
        let size = t.font_size_md as u32;
        {
            let path = &self.tabs.items[self.tabs.active].path;
            if self.crumbs.path != *path || self.crumbs.size != size {
                self.crumbs.path.clone_from(path);
                self.crumbs.size = size;
                self.crumbs.crumbs = nav::crumbs(path);
                let n = self.crumbs.crumbs.len();
                self.crumbs.widths = self
                    .crumbs
                    .crumbs
                    .iter()
                    .enumerate()
                    .map(|(i, c)| g.measure(&c.label, if i + 1 == n { t.font_md().b() } else { t.font_md() }))
                    .collect();
            }
        }
        g.text("\u{EDA2}", Rect::new(bar.x + 8.0, bar.y, 16.0, bar.h), f(13.0).icon().c(), t.text_dim);
        let first_x = bar.x + 8.0 + 16.0 + 6.0;
        let avail = bar.r() - 8.0 - first_x;
        let n = self.crumbs.crumbs.len();
        let first = first_visible_crumb(&self.crumbs.widths, avail);
        g.clip(bar);
        let mut x = first_x;
        if first > 0 && n > 0 {
            let r = Rect::new(x, bar.y + 3.0, ELLIPSIS_W, bar.h - 6.0);
            let hit = ui.interact(ID_PATH.child("more"), r, Sense::click().cursor(Cursor::Hand));
            if hit.hovered {
                g.round(r, t.radius_sm, t.hover);
            }
            g.text("\u{2026}", r, t.font_md().c(), t.text_dim);
            if hit.clicked == Some(Button::Left) {
                let p = self.crumbs.crumbs[first - 1].path.clone();
                g.unclip();
                self.go(&p);
                return;
            }
            x += ELLIPSIS_W;
            g.text("\u{E76C}", Rect::new(x, bar.y, SEP_W, bar.h), f(8.0).icon().c(), t.text_dim);
            x += SEP_W;
        }
        let mut go = None;
        for i in first..n {
            let last = i + 1 == n;
            let wd = self.crumbs.widths[i] + 2.0 * CRUMB_PAD;
            let r = Rect::new(x, bar.y + 3.0, wd, bar.h - 6.0);
            let hit = ui.interact(ID_PATH.child("crumb").with(i as u64), r, Sense::click().cursor(Cursor::Hand));
            if hit.hovered {
                g.round(r, t.radius_sm, t.hover);
            }
            let font = if last { t.font_md().b() } else { t.font_md() };
            g.text(&self.crumbs.crumbs[i].label, Rect::new(x + CRUMB_PAD, bar.y, self.crumbs.widths[i] + 2.0, bar.h), font, if last { t.text } else { t.text_dim });
            if hit.clicked == Some(Button::Left) {
                go = Some(self.crumbs.crumbs[i].path.clone());
            }
            x += wd;
            if !last {
                g.text("\u{E76C}", Rect::new(x, bar.y, SEP_W, bar.h), f(8.0).icon().c(), t.text_dim);
                x += SEP_W;
            }
        }
        g.unclip();
        if let Some(p) = go {
            self.go(&p);
        }
    }

    /// The path-bar error box, under the bar (drawn last: on top).
    fn paint_path_error(&mut self, g: &mut dyn Canvas, toolbar: Rect, t: &Theme) {
        let Some(msg) = &self.path_error else { return };
        if !self.editing {
            return;
        }
        let r = Rect::new(toolbar.x + 8.0 + 3.0 * (t.height_button + 2.0) + t.space_4, toolbar.b() + 2.0, (toolbar.w - 400.0).clamp(240.0, 720.0), 28.0);
        g.round(r, t.radius_md, t.popup);
        g.round_stroke(r, t.radius_md, t.danger, 1.0);
        g.text("\u{E783}", Rect::new(r.x + 8.0, r.y, 16.0, r.h), f(13.0).icon().c(), t.danger);
        g.text(msg, Rect::new(r.x + 30.0, r.y, r.w - 38.0, r.h), t.font(), t.text);
    }

    fn paint_main(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, main: Rect, t: &Theme) {
        let mut area = main;
        let header_r = area.cut_top(t.height_column_header);
        let list_r = area;
        self.list_top = list_r.y;
        let total = main.w - t.layout_scrollbar_width;
        let spans = self.cols.spans(total);

        // header: sort and resize
        let hc = Col::ALL.map(|c| tsubame_ui::HeaderCol { label: c.label(), right: c.right_aligned() });
        let hc: [HeaderCol; 4] = hc;
        let tab = &mut self.tabs.items[self.tabs.active];
        let sorted = Col::ALL.iter().position(|c| c.sort_column() == tab.sort.column).map(|i| (i, tab.sort.descending));
        let hr = column_header(ui, g, ID_HEADER, header_r, &hc, &spans, sorted);
        if let Some(i) = hr.sort {
            tab.sort_by(Col::ALL[i].sort_column());
        }
        if let Some((i, x)) = hr.resize
            && let Some(&col) = Col::ALL.get(i)
        {
            self.cols.drag_left_edge(col, x, total);
        }
        let spans = self.cols.spans(total);

        // the list
        g.fill(list_r, t.row);
        let tab = &mut self.tabs.items[self.tabs.active];
        tab.list.row_h = t.height_list_row;
        let (lr, activated) = {
            let Tab { store, sel, list: lstate, path, synthetic, .. } = &mut *tab;
            let root = if *synthetic { None } else { Some(path.as_str()) };
            let mut rows = Rows { store, sel, shell, row: &mut self.rowbuf, text: &mut self.text, spans, zebra: self.zebra, root };
            let lr = list(ui, g, ID_LIST, list_r, lstate, &mut rows);
            (lr, lr.activated)
        };
        self.last_range = lr.range;
        if let Some((row, pt)) = lr.context
            && let Some(row) = row
        {
            self.menu_request = Some((row, Some(pt)));
        }
        if lr.context_key {
            self.menu_request = Some((self.tabs.items[self.tabs.active].list.cursor, None));
        }
        // empty and failed folders
        let tab = &self.tabs.items[self.tabs.active];
        if tab.shown() == 0 {
            let body = Rect::new(list_r.x, list_r.y, list_r.w - t.layout_scrollbar_width, list_r.h);
            match &tab.load {
                Load::Done if tab.store.is_empty() => empty_state(g, t, body, "\u{E8B7}", "This folder is empty", None),
                Load::Done => empty_state(g, t, body, "\u{E721}", "No items match the filter", Some("Press Esc to clear it.")),
                Load::Failed(m) => empty_state(g, t, body, "\u{E783}", "Can't open this folder", Some(m)),
                Load::Loading => {}
            }
        }
        if let Some(pos) = activated {
            self.activate(pos);
            ui.request_repaint();
        }
    }

    fn paint_status(&mut self, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        g.fill(r, t.panel);
        g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        let tab = &self.tabs.items[self.tabs.active];
        let f = t.font_small();
        let mut s = std::mem::take(&mut self.text);
        s.clear();
        {
            use std::fmt::Write;
            let shown = tab.shown();
            match &tab.load {
                Load::Loading => {
                    let _ = write!(s, "Reading folder... {} items", count(tab.delivered));
                }
                Load::Failed(_) => s.push_str("Can't open this folder"),
                Load::Done if shown != tab.store.len() => {
                    let _ = write!(s, "{} of {} items", count(shown), count(tab.store.len()));
                }
                Load::Done => {
                    let _ = write!(s, "{} {}", count(shown), if shown == 1 { "item" } else { "items" });
                }
            }
        }
        let left = Rect::new(r.x + 12.0, r.y + 1.0, 360.0, r.h - 1.0);
        g.text(&s, left, f, if matches!(tab.load, Load::Failed(_)) { t.danger } else { t.text_dim });
        let mut x = r.x + 12.0 + g.measure(&s, f) + 24.0;
        if tab.sel.count() > 0 {
            s.clear();
            let mut sz = String::new();
            data::fmt_size(tab.sel.total_size().min(u64::MAX as u128) as u64, &mut sz);
            use std::fmt::Write;
            let _ = write!(s, "{} selected  {}", count(tab.sel.count()), sz);
            g.text(&s, Rect::new(x, r.y + 1.0, 300.0, r.h - 1.0), f, t.text);
            x += 300.0;
        }
        let _ = x;
        if let Some((free, total)) = self.side.volume_of(&tab.path) {
            s.clear();
            let (mut a, mut b) = (String::new(), String::new());
            data::fmt_volume(free, &mut a);
            data::fmt_volume(total, &mut b);
            use std::fmt::Write;
            let _ = write!(s, "{a} free of {b}");
            g.text(&s, Rect::new(r.r() - 12.0 - 260.0, r.y + 1.0, 260.0, r.h - 1.0), f.r(), t.text_dim);
        }
        self.text = s;
    }
}

const ELLIPSIS_W: f32 = 22.0;
const SEP_W: f32 = 14.0;
const CRUMB_PAD: f32 = 6.0;

/// Index of the first breadcrumb to show so the last ones fit in `avail`
/// DIPs (the last crumb is always shown). Each crumb takes its text width
/// plus padding and a separator; a hidden head costs one "…" button.
pub fn first_visible_crumb(widths: &[f32], avail: f32) -> usize {
    let n = widths.len();
    if n == 0 {
        return 0;
    }
    let cost = |i: usize| widths[i] + 2.0 * CRUMB_PAD + if i + 1 < n { SEP_W } else { 0.0 };
    let total: f32 = (0..n).map(cost).sum();
    if total <= avail {
        return 0;
    }
    let room = avail - (ELLIPSIS_W + SEP_W);
    let mut used = 0.0;
    let mut first = n - 1;
    for i in (0..n).rev() {
        if i < n - 1 && used + cost(i) > room {
            break;
        }
        used += cost(i);
        first = i;
    }
    first
}

/// 12480 -> "12,480"
fn count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn icon_button(ui: &mut Ui, c: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, glyph: &str, enabled: bool) -> bool {
    let it = ui.interact(id, r, Sense::click());
    if enabled && it.hovered {
        c.round(r, t.radius_md, if it.active { t.selected_inactive } else { t.hover });
    }
    c.text(glyph, r, f(t.icon_toolbar).icon().c(), if enabled { t.text } else { t.text_disabled });
    enabled && it.clicked == Some(Button::Left)
}

fn empty_state(g: &mut dyn Canvas, t: &Theme, body: Rect, glyph: &str, title: &str, hint: Option<&str>) {
    let cy = body.y + body.h * 0.42;
    g.text(glyph, Rect::new(body.x + body.w / 2.0 - 30.0, cy - t.icon_empty_state, 60.0, t.icon_empty_state + 8.0), f(t.icon_empty_state).icon().c(), t.folder);
    g.text(title, Rect::new(body.x, cy + 14.0, body.w, 22.0), t.font_md().c(), t.text);
    if let Some(h) = hint {
        g.text(h, Rect::new(body.x + 24.0, cy + 36.0, body.w - 48.0, 20.0), t.font().c(), t.text_dim);
    }
}

/// The list's view of one tab for a frame.
struct Rows<'a> {
    store: &'a EntryStore,
    sel: &'a mut Selection,
    shell: &'a mut ShellUi,
    row: &'a mut RowBuf,
    text: &'a mut String,
    spans: crate::cols::Spans,
    zebra: bool,
    /// folder of the rows (None for the bench's synthetic rows: no real files)
    root: Option<&'a str>,
}

impl ListSource for Rows<'_> {
    fn len(&self) -> usize {
        self.store.view().len()
    }
    fn is_selected(&self, i: usize) -> bool {
        self.store.at(i).is_some_and(|id| self.sel.contains(id))
    }
    fn select(&mut self, i: usize, mods: Mods, by: SelectBy) {
        let Some(id) = self.store.at(i) else { return };
        match by {
            SelectBy::Click | SelectBy::Key if mods.shift => self.sel.extend(self.store, i),
            SelectBy::Click if mods.ctrl => self.sel.toggle(self.store, i),
            SelectBy::Key if mods.ctrl => {} // Ctrl+arrows move the cursor only
            SelectBy::Space if mods.ctrl => self.sel.toggle(self.store, i),
            SelectBy::Space | SelectBy::RightClick if self.sel.contains(id) => {}
            _ => self.sel.single(self.store, i),
        }
    }
    fn select_none(&mut self) {
        self.sel.clear();
    }
    fn draw_row(&mut self, g: &mut dyn Canvas, t: &Theme, i: usize, r: Rect, st: RowState) {
        let Some(id) = self.store.at(i) else { return };
        if self.zebra && i % 2 == 1 && !st.selected && !st.hot {
            g.fill(r, t.row_alt);
        }
        let row = &mut *self.row;
        row.fill(self.store, id);
        let sp = &self.spans;
        let x = r.x;
        // icon: the shell's, else a placeholder until it arrives
        let ir = Rect::new(x + 8.0, r.y + (r.h - ICON_DIP) / 2.0, ICON_DIP, ICON_DIP);
        match self.shell.row_icon(row, self.root) {
            Some(img) => g.image(&img.key, img.w, img.h, &img.px, ir),
            None => {
                let (ic, col) = if row.is_dir() { (Rect::new(ir.x, ir.y + 3.0, 16.0, 11.0), t.folder) } else { (Rect::new(ir.x + 2.0, ir.y, 12.0, 16.0), t.text_dim) };
                g.round_a(ic, 2.0, col, 0.35);
            }
        }
        let name_c = if row.hidden { t.text_dim } else { t.text };
        g.text(row.name_str(), Rect::new(x + sp[0].0 + 32.0, r.y, (sp[0].1 - 32.0 - 8.0).max(0.0), r.h), t.font(), name_c);
        data::fmt_date(row.mtime, self.text);
        g.text(self.text, Rect::new(x + sp[1].0 + 8.0, r.y, (sp[1].1 - 12.0).max(0.0), r.h), t.font(), t.text_dim);
        row.type_text(self.text);
        g.text(self.text, Rect::new(x + sp[2].0 + 8.0, r.y, (sp[2].1 - 12.0).max(0.0), r.h), t.font(), t.text_dim);
        if !row.is_dir() {
            data::fmt_size(row.size, self.text);
            g.text(self.text, Rect::new(x + sp[3].0, r.y, (sp[3].1 - 8.0).max(0.0), r.h), t.font().r(), t.text_dim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumbs_truncate_from_the_left() {
        // everything fits
        assert_eq!(first_visible_crumb(&[20.0, 40.0, 60.0], 400.0), 0);
        // too narrow: the head collapses, the last crumb stays
        let w = [80.0, 80.0, 80.0, 80.0];
        let first = first_visible_crumb(&w, 200.0);
        assert!(first >= 1, "{first}");
        assert!(first <= 3);
        // even a tiny bar keeps the last crumb
        assert_eq!(first_visible_crumb(&w, 10.0), 3);
        assert_eq!(first_visible_crumb(&[], 100.0), 0);
    }

    #[test]
    fn counts_get_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(12_480), "12,480");
        assert_eq!(count(1_000_000), "1,000,000");
    }
}
