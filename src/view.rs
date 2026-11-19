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
use crate::commands::*;
use crate::data::{self, RowBuf};
use crate::nav::{self, Crumb};
use crate::panes;
use crate::shellui::ShellUi;
use crate::statusbar::{self, StatusInfo};
use crate::sidebar::{Act, DragCtx, Sidebar};
use crate::tab::{Load, Tab};
use crate::tabs::Tabs;
use crate::shellui::GridPic;
use tsubame_config::DEFAULT_SIDEBAR_WIDTH;
use tsubame_core::{EntryId, EntryStore, Selection};
use tsubame_ui::{
    BandCells, Button, Canvas, Cursor, Density, Event, Font, GridSource, GridState, HeaderCol, Id, Key, ListSource, Mods, Output, Pt, Rect, RowState, SelectBy, Sense, Split, TabStrip, TextInput, Theme, Ui, column_header, f, grid, list, splitter, tab_strip, text_input,
};
use tsubame_win::enumerate::Wake;

#[path = "view_edit.rs"]
mod edit;
#[path = "view_u4.rs"]
mod u4;
use crate::batchui::BatchEdit;
use crate::editing::RenameEdit;
use crate::ops::Ops;
use edit::{Drag, PressInfo};

pub const ICON_DIP: f32 = 16.0;

/// How a folder is shown. One global setting (all tabs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Details,
    GridSmall,
    GridLarge,
}

impl ViewMode {
    pub fn is_grid(self) -> bool {
        self != ViewMode::Details
    }
    /// (cell width, cell height, picture box, type icon) in DIPs. The cell is
    /// the picture box, a 6 DIP gap, two label lines and 8 DIP of padding.
    pub fn dims(self) -> (f32, f32, f32, f32) {
        match self {
            ViewMode::GridLarge => (152.0, 182.0, 128.0, 64.0),
            _ => (96.0, 118.0, 64.0, 48.0),
        }
    }
    pub fn next(self) -> ViewMode {
        match self {
            ViewMode::Details => ViewMode::GridSmall,
            ViewMode::GridSmall => ViewMode::GridLarge,
            ViewMode::GridLarge => ViewMode::Details,
        }
    }
    pub fn parse(s: &str) -> Option<ViewMode> {
        match s.to_ascii_lowercase().as_str() {
            "details" | "list" => Some(ViewMode::Details),
            "grid-small" | "small" | "grid" => Some(ViewMode::GridSmall),
            "grid-large" | "large" => Some(ViewMode::GridLarge),
            _ => None,
        }
    }
    /// The config file's name for this mode.
    pub fn to_config(self) -> tsubame_config::ViewMode {
        match self {
            ViewMode::Details => tsubame_config::ViewMode::Details,
            ViewMode::GridSmall => tsubame_config::ViewMode::GridSmall,
            ViewMode::GridLarge => tsubame_config::ViewMode::GridLarge,
        }
    }
    pub fn from_config(m: tsubame_config::ViewMode) -> ViewMode {
        match m {
            tsubame_config::ViewMode::Details => ViewMode::Details,
            tsubame_config::ViewMode::GridSmall => ViewMode::GridSmall,
            tsubame_config::ViewMode::GridLarge => ViewMode::GridLarge,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            ViewMode::Details => "details",
            ViewMode::GridSmall => "grid-small",
            ViewMode::GridLarge => "grid-large",
        }
    }
}

/// Height of one label line in the grid (DIPs).
const LABEL_LINE: f32 = 16.0;
/// Slots of the direct-mapped label split cache (16 bytes each).
const LABEL_SLOTS: usize = 1024;

const ID_TABS: Id = Id::new("tabs");
const ID_VIEW: Id = Id::new("view-mode");
pub const ID_LIST: Id = Id::new("details");
pub const ID_FILTER: Id = Id::new("filter");
const ID_PATH: Id = Id::new("pathbar");
const ID_PATH_EDIT: Id = Id::new("pathedit");
const ID_HEADER: Id = Id::new("header");
const ID_BACK: Id = Id::new("nav-back");
const ID_FWD: Id = Id::new("nav-forward");
const ID_UP: Id = Id::new("nav-up");
const ID_WIN: Id = Id::new("winctl");
const ID_SPLIT: Id = Id::new("pane-split");
const ID_LAYOUT: Id = Id::new("pane-layout");
const ID_SEARCH: Id = Id::new("pane-search");
/// Height of a pane's own tab strip (two panes).
const PANE_STRIP_H: f32 = 30.0;

/// Widget id of pane `slot` (0 = left / top, 1 = right / bottom): the second
/// pane gets its own copy of every id.
pub fn pid(slot: usize, base: Id) -> Id {
    if slot == 0 { base } else { base.with(0x70_616e_65) }
}

/// Where a drop lands: into a folder, or pinned at a place of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropDest {
    Folder(String),
    Pin(usize),
}

/// The per-pane state that is swapped in and out of the view: the pane that
/// is not active keeps it here.
pub struct PaneState {
    tabs: Tabs<Tab>,
    strip: TabStrip,
    filter: TextInput,
    path_text: TextInput,
    editing: bool,
    edit_frames: u32,
    path_error: Option<String>,
    edit_nav: Option<(u64, String, String)>,
    crumbs: CrumbCache,
    focus_list: bool,
    focus_path: bool,
    list_top: f32,
    list_r: Rect,
    last_range: (usize, usize),
}

impl PaneState {
    fn new(tab: Tab) -> PaneState {
        PaneState {
            tabs: Tabs::new(tab),
            strip: TabStrip::default(),
            filter: TextInput::default(),
            path_text: TextInput::default(),
            editing: false,
            edit_frames: 0,
            path_error: None,
            edit_nav: None,
            crumbs: CrumbCache::default(),
            focus_list: false,
            focus_path: false,
            list_top: 0.0,
            list_r: Rect::new(0.0, 0.0, 0.0, 0.0),
            last_range: (0, 0),
        }
    }
}

/// What main.rs must do with the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinCmd {
    Minimize,
    MaxRestore,
    Close,
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
    /// details or grid (one setting for every tab)
    pub mode: ViewMode,
    /// width of the sidebar in DIPs, from the config
    pub sidebar_width: f32,
    /// a setting the user changed: main writes the config (debounced)
    pub config_dirty: bool,
    /// window DPI (main sets it): thumbnails are made for this
    pub dpi: f32,
    /// direct-mapped cache of where a grid label breaks: (hash, first line bytes)
    labels: Vec<(u64, u32)>,
    pub side: Sidebar,
    wake: Wake,
    rowbuf: Box<RowBuf>,
    text: String,
    crumbs: CrumbCache,
    focus_list: bool,
    /// a failed typed path re-opens the field: focus it on the next frame
    focus_path: bool,
    pub maximized: bool,
    /// y of the list's top edge (keyboard menus)
    list_top: f32,
    /// the list (or grid) area of the last paint
    list_r: Rect,
    /// rows drawn by the last paint (first, last+1)
    pub last_range: (usize, usize),
    /// the volume of the shown folder changed: main re-reads the drives
    pub want_drives: bool,
    /// the last `pump` stopped at its time budget: pump again soon
    pub pump_more: bool,
    last_drive: char,

    /// the list asked for a context menu: (view row, DIP point); Shift+F10 /
    /// the Menu key give (cursor row, None)
    pub menu_request: Option<(usize, Option<Pt>)>,
    /// a right click on empty list space (DIP point) of the active pane:
    /// main opens the folder's shell menu (`shellops::after_frame`)
    pub bg_menu: Option<Pt>,
    /// pane whose list asked for a context menu this frame (made active
    /// before main opens the menu)
    menu_pane: Option<usize>,
    /// per pane slot: the tab strip's rect and scroll (drops on tabs)
    strips: [(Rect, f32); 2],
    /// a drag started on our rows: main runs it as an OLE drag
    pub drag_out: Option<Vec<String>>,
    /// where a drop at the pointer lands (set while a drag is painted)
    pub drop_dest: Option<DropDest>,
    /// modifier keys OLE reports during a drag (they rule the drop)
    pub drag_keys: Option<Mods>,
    /// the shell menu's Rename: inline rename on the next frame
    pub rename_request: bool,
    /// files to open with their default handler (main runs them on a worker)
    pub open_paths: Vec<String>,
    pub win_cmd: Option<WinCmd>,
    /// last tab closed: the window should close
    pub quit: bool,
    /// the other pane while two are shown (its state is swapped in to draw it)
    other: Option<Box<PaneState>>,
    /// slot (0 = left / top, 1 = right / bottom) the live fields belong to
    pub live: usize,
    /// slot of the pane that has the focus
    pub active: usize,
    /// how the two panes sit: Vertical = side by side, Horizontal = stacked
    pub orient: tsubame_config::Split,
    /// share of the area the first pane takes
    pub ratio: f32,
    /// a widget was held last frame (a press that starts on another pane makes it active)
    ptr_was_active: bool,
    /// where the two panes were drawn last frame (a press there activates the pane)
    pane_rects: [Rect; 2],
    /// file operations (copy, move, rename, undo)
    pub ops: Ops,
    rename: Option<RenameEdit>,
    rename_slot: usize,
    batch: Option<BatchEdit>,
    batch_slot: usize,
    batch_bar: Rect,
    drag: Option<Drag>,
    press: Option<PressInfo>,
    /// (row, ms) of the last press on a row
    last_click: Option<(usize, u64)>,
    /// the only selected row at the end of the last two frames
    sole_hist: [Option<usize>; 2],
    /// modifier keys held now (main sets it each frame)
    pub cur_mods: Mods,
    /// main runs the file-operation poll timer
    pub ops_timer: bool,
    /// where the sidebar says a drag would land
    side_target: Option<crate::sidebar::DropTarget>,
    /// the settings as the user has them (U4): Settings edits this, `config_dirty` saves it
    pub prefs: tsubame_config::Config,
    /// id of the colour theme (`themes::find`)
    pub theme_id: String,
    /// the key map (defaults + `[keys.overrides]`)
    pub keymap: crate::keymap::KeyMap,
    /// the command palette, built when it opens and dropped when it closes
    palette: Option<Box<crate::palette::Palette>>,
    /// the own-drawn context menu (open, or waiting for its shell invoke);
    /// main opens it and runs its picks (`shellops::ctx_after_frame`)
    pub ctx: Option<Box<crate::ctxui::CtxMenu>>,
    /// command ids run from the palette, most recent first
    recents: Vec<u32>,
    /// the Settings tab's pages, built when the tab opens and dropped when it closes
    settings: Option<Box<crate::settings::Settings>>,
    /// where this copy lives (main fills it in)
    pub env: crate::settings::Env,
    /// main must restart into the updated version / roll back / reset the config
    pub want_restart: bool,
    pub want_rollback: bool,
    pub want_reset_all: bool,
    /// private bytes of the process, sampled at most once a second while Settings is shown
    pub private_bytes: usize,
    private_at: Option<std::time::Instant>,
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
            mode: ViewMode::Details,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            config_dirty: false,
            dpi: 96.0,
            labels: vec![(0, 0); LABEL_SLOTS],
            side: Sidebar::default(),
            wake,
            rowbuf: Box::new(RowBuf::new()),
            text: String::with_capacity(64),
            crumbs: CrumbCache::default(),
            focus_list: true,
            focus_path: false,
            maximized: false,
            list_top: 0.0,
            list_r: Rect::new(0.0, 0.0, 0.0, 0.0),
            last_range: (0, 0),
            want_drives: true,
            pump_more: false,
            last_drive: start.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('C'),
            menu_request: None,
            bg_menu: None,
            menu_pane: None,
            strips: [(Rect::new(0.0, 0.0, 0.0, 0.0), 0.0); 2],
            drag_out: None,
            drop_dest: None,
            drag_keys: None,
            rename_request: false,
            open_paths: Vec::new(),
            win_cmd: None,
            quit: false,
            other: None,
            live: 0,
            active: 0,
            orient: tsubame_config::Split::Vertical,
            ratio: 0.5,
            ptr_was_active: false,
            pane_rects: [Rect::new(0.0, 0.0, 0.0, 0.0); 2],
            ops: Ops::default(),
            rename: None,
            rename_slot: 0,
            batch: None,
            batch_slot: 0,
            batch_bar: Rect::new(0.0, 0.0, 0.0, 0.0),
            drag: None,
            press: None,
            last_click: None,
            sole_hist: [None, None],
            cur_mods: Mods::default(),
            ops_timer: false,
            side_target: None,
            prefs: tsubame_config::Config::default(),
            theme_id: tsubame_ui::themes::tsubame(dark).id.to_string(),
            keymap: crate::keymap::KeyMap::defaults(),
            palette: None,
            ctx: None,
            recents: Vec::new(),
            settings: None,
            env: crate::settings::Env::default(),
            want_restart: false,
            want_rollback: false,
            want_reset_all: false,
            private_bytes: 0,
            private_at: None,
        }
    }

    pub fn base_theme(&self) -> Theme {
        let mut t = self.themed();
        t.layout_sidebar_width = if self.side.rail { crate::sidebar::RAIL_W } else { self.sidebar_width };
        t
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

    /// Generation key of the shared icon / thumbnail work: changes when either
    /// pane shows another folder (both panes share the shell caches).
    fn shell_key(&self) -> u64 {
        let a = self.tab().nav_key();
        match self.other.as_deref() {
            Some(o) => a ^ o.tabs.items[o.tabs.active].nav_key().rotate_left(23),
            None => a,
        }
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs.items[self.tabs.active]
    }
    pub fn tab_mut(&mut self) -> &mut Tab {
        let i = self.tabs.active;
        &mut self.tabs.items[i]
    }

    // ---- two panes ---------------------------------------------------------

    /// Two panes are shown.
    pub fn dual(&self) -> bool {
        self.other.is_some()
    }

    /// Widget id of the active pane's list (the window focuses it at start).
    pub fn list_id(&self) -> Id {
        pid(self.live, ID_LIST)
    }

    /// Swaps the live per-pane fields with the other pane's: afterwards
    /// `self.tabs`, `self.filter`, ... belong to the other pane.
    fn swap_live(&mut self) {
        let Some(o) = self.other.as_deref_mut() else { return };
        std::mem::swap(&mut self.tabs, &mut o.tabs);
        std::mem::swap(&mut self.strip, &mut o.strip);
        std::mem::swap(&mut self.filter, &mut o.filter);
        std::mem::swap(&mut self.path_text, &mut o.path_text);
        std::mem::swap(&mut self.editing, &mut o.editing);
        std::mem::swap(&mut self.edit_frames, &mut o.edit_frames);
        std::mem::swap(&mut self.path_error, &mut o.path_error);
        std::mem::swap(&mut self.edit_nav, &mut o.edit_nav);
        std::mem::swap(&mut self.crumbs, &mut o.crumbs);
        std::mem::swap(&mut self.focus_list, &mut o.focus_list);
        std::mem::swap(&mut self.focus_path, &mut o.focus_path);
        std::mem::swap(&mut self.list_top, &mut o.list_top);
        std::mem::swap(&mut self.list_r, &mut o.list_r);
        std::mem::swap(&mut self.last_range, &mut o.last_range);
        self.live = 1 - self.live;
    }

    /// Runs `f` with the pane in `slot` live (no-op swap when it already is).
    pub fn with_pane<R>(&mut self, slot: usize, f: impl FnOnce(&mut View) -> R) -> R {
        let swap = self.other.is_some() && slot != self.live;
        if swap {
            self.swap_live();
        }
        let r = f(self);
        if swap {
            self.swap_live();
        }
        r
    }

    /// Gives the focus to pane `slot` (a click in it, Tab).
    pub fn set_active_pane(&mut self, slot: usize) {
        if self.other.is_none() || slot == self.active || slot > 1 {
            return;
        }
        if self.live != slot {
            self.swap_live();
        }
        self.active = slot;
        self.focus_list = true;
    }

    /// The tabs of pane `slot`, whichever is live.
    pub fn pane_tabs(&self, slot: usize) -> Option<&Tabs<Tab>> {
        if slot == self.live {
            Some(&self.tabs)
        } else {
            self.other.as_deref().map(|o| &o.tabs)
        }
    }

    /// Startup: puts the saved tabs in the other pane (it starts as a copy
    /// of the first folder). Only the shown tab lists now; the rest stay lazy.
    pub fn restore_other_pane(&mut self, tabs: Vec<Tab>, active: usize) {
        let wake = self.wake.clone();
        let Some(o) = self.other.as_deref_mut() else { return };
        if tabs.is_empty() {
            return;
        }
        o.tabs.items = tabs;
        o.tabs.active = active.min(o.tabs.items.len() - 1);
        o.strip = TabStrip::default();
        let a = o.tabs.active;
        o.tabs.items[a].resume(wake);
    }

    /// One pane or two, and how they sit (the title-bar switch and the
    /// "pane.split" commands). Two panes start with the same folder in both;
    /// back to one, the other pane's tabs join the remaining pane.
    pub fn set_layout(&mut self, dual: bool, orient: tsubame_config::Split) {
        let changed = self.orient != orient || dual != self.dual();
        self.orient = orient;
        if dual && self.other.is_none() {
            let path = self.folder_path();
            let id = self.next_tab_id;
            self.next_tab_id += 1;
            let mut t = Tab::new(id, &path, self.density.row_h());
            t.start(&path, self.wake.clone());
            let mut o = PaneState::new(t);
            o.focus_list = false;
            self.other = Some(Box::new(o));
            // the live pane keeps its slot; the new one takes the other
            self.active = self.live;
        } else if !dual && let Some(o) = self.other.take() {
            let mut o = *o;
            self.tabs.items.append(&mut o.tabs.items);
            self.live = 0;
            self.active = 0;
            self.focus_list = true;
        }
        if changed {
            self.config_dirty = true;
        }
    }

    /// Ctrl+\
    pub fn toggle_split(&mut self) {
        let o = self.orient;
        self.set_layout(!self.dual(), o);
    }

    /// Ctrl+Shift+\ : side by side <-> stacked (opens the second pane if needed).
    pub fn flip_split(&mut self) {
        let o = crate::panes::flip(self.orient);
        if self.dual() {
            self.set_layout(true, o);
        } else {
            self.set_layout(true, o);
        }
    }

    // ---- navigation ------------------------------------------------------

    /// Opens `path` in the current tab and records it in the history.
    pub fn go(&mut self, path: &str) {
        if self.tab().settings {
            return self.open_in_new_tab(path);
        }
        let wake = self.wake.clone();
        let t = self.tab_mut();
        t.history.push(path);
        t.start(path, wake);
        self.after_nav();
    }

    fn after_nav(&mut self) {
        self.close_edits();
        self.ops.message = None;
        let p = self.tab().path.clone();
        self.side.push_recent(&p);
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
        if self.tab().settings {
            return;
        }
        let wake = self.wake.clone();
        let t = self.tab_mut();
        if let Some(p) = t.history.back().map(str::to_string) {
            t.start(&p, wake);
            self.after_nav();
        }
    }
    pub fn forward(&mut self) {
        if self.tab().settings {
            return;
        }
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
        if self.tab().settings {
            return;
        }
        let wake = self.wake.clone();
        let t = self.tab_mut();
        let p = t.path.clone();
        t.start(&p, wake);
        self.after_nav();
        self.want_drives = true;
    }

    pub fn new_tab(&mut self) {
        let path = self.folder_path();
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
        let was_settings = self.tabs.items.get(i).is_some_and(|t| t.settings);
        drop(self.tabs.close(i));
        if was_settings {
            self.settings = None;
        }
        if self.tabs.is_empty() {
            self.quit = true;
        } else {
            self.switched();
        }
    }

    pub fn switch_tab(&mut self, i: usize) {
        self.tabs.activate(i);
        let wake = self.wake.clone();
        if self.tab().pending {
            // a tab restored from the session: its listing starts the first time it is shown
            self.tab_mut().resume(wake);
        }
        self.switched();
    }

    /// A tab restored from the session. Its listing starts on first show
    /// (`switch_tab`), which keeps the startup cost to one folder.
    pub fn pending_tab(&mut self, path: &str) -> Tab {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut t = Tab::new(id, path, self.density.row_h());
        t.pending = true;
        t
    }

    /// After `session::apply` put the restored tabs in place.
    pub fn after_restore(&mut self) {
        self.filter.set_text("");
        self.editing = false;
        self.path_error = None;
        self.focus_list = true;
        self.strip = TabStrip::default();
        self.tabs.active = self.tabs.active.min(self.tabs.items.len().saturating_sub(1));
    }

    fn switched(&mut self) {
        self.close_edits();
        let f = self.tab().filter.clone();
        self.filter.set_text(&f);
        self.editing = false;
        self.path_error = None;
        self.focus_list = true;
    }

    /// Enter / double click on view row `pos`: a folder opens in this tab, a
    /// file goes to the shell on a worker.
    pub fn activate(&mut self, pos: usize) {
        self.press = None;
        if let Some((path, dir)) = self.tab().entry_at(pos) {
            if dir { self.go(&path) } else { self.open_paths.push(path) }
        }
    }

    /// Moves everything the enumerators delivered into the stores. UI thread,
    /// no disk. True when something visible changed.
    pub fn pump(&mut self) -> bool {
        self.pump_more = false;
        let mut changed = self.pump_pane();
        if self.other.is_some() {
            self.swap_live();
            changed |= self.pump_pane();
            self.swap_live();
        }
        changed
    }

    fn pump_pane(&mut self) -> bool {
        let mut changed = false;
        for t in &mut self.tabs.items {
            let p = t.pump();
            changed |= p.changed;
            self.pump_more |= p.more;
        }
        changed |= self.watch_pane();
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
                        self.focus_path = true;
                    }
                }
                Some(t) if t.is_loading() => self.edit_nav = Some((id, prev, typed)),
                _ => {}
            }
        }
        changed
    }

    /// The live pane's tabs: the shown one watches its folder, the others
    /// do not; a watcher's relist / folder-gone note is acted on here.
    fn watch_pane(&mut self) -> bool {
        let active = self.tabs.active;
        let mut changed = false;
        for i in 0..self.tabs.items.len() {
            self.tabs.items[i].set_visible(i == active);
            match self.tabs.items[i].take_note() {
                Some(crate::tab::WatchNote::Relist) => {
                    self.reload_tab(i, &[]);
                    changed = true;
                }
                Some(crate::tab::WatchNote::Gone) => {
                    let wake = self.wake.clone();
                    let t = &mut self.tabs.items[i];
                    if let Some(up) = nav::parent(&t.path) {
                        t.history.push(&up);
                        t.start(&up, wake);
                        changed = true;
                    }
                }
                None => {}
            }
        }
        changed
    }

    /// Both panes' watchers follow the shown tabs (after a tab switch, a new
    /// tab, a closed one). Cheap: a flag compare per tab.
    fn watch_tabs(&mut self) {
        self.watch_pane();
        if self.other.is_some() {
            self.swap_live();
            self.watch_pane();
            self.swap_live();
        }
    }

    fn begin_edit(&mut self, ui: &mut Ui) {
        let p = self.tab().path.clone();
        self.path_text.set_text(&p);
        self.path_text.select_all();
        self.editing = true;
        self.edit_frames = 0;
        self.path_error = None;
        ui.set_focus(Some(pid(self.live, ID_PATH_EDIT)));
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
        self.theme_id = tsubame_ui::themes::tsubame(dark).id.to_string();
        self.pull_appearance();
        self.config_dirty = true;
        self.apply_theme(ui);
    }

    /// What the sidebar asked for.
    fn side_act(&mut self, ui: &mut Ui, act: Option<Act>) {
        match act {
            Some(Act::Go(p)) => self.go(&p),
            Some(Act::NewTab(p)) => self.open_in_new_tab(&p),
            Some(Act::OtherPane(p)) => self.open_in_other_pane(&p),
            Some(Act::DropOn(p)) => self.drop_selection_on(&p),
            Some(Act::Layout) => {
                if let Some(w) = self.side.want_width.take() {
                    self.sidebar_width = w;
                }
                self.apply_theme(ui);
            }
            None => {}
        }
        if self.side.dirty {
            self.side.dirty = false;
            self.config_dirty = true;
        }
    }

    /// Opens `path` in a new tab of the active pane.
    pub fn open_in_new_tab(&mut self, path: &str) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut t = Tab::new(id, path, self.density.row_h());
        t.start(path, self.wake.clone());
        self.tabs.add(t);
        self.switched();
    }

    /// With two panes: shows `path` in the other one. With one: a new tab.
    pub fn open_in_other_pane(&mut self, path: &str) {
        if self.other.is_none() {
            return self.open_in_new_tab(path);
        }
        let slot = panes::other(self.active);
        self.with_pane(slot, |v| v.go(path));
    }

    pub(crate) fn run_command(&mut self, ui: &mut Ui, cmd: u32) {
        if self.tab().settings && !u4::settings_tab_allows(cmd) {
            return;
        }
        if self.run_u4(ui, cmd) {
            ui.request_repaint();
            return;
        }
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
                ui.set_focus(Some(pid(self.live, ID_FILTER)));
                self.filter.select_all();
            }
            CMD_BACK => self.back(),
            CMD_FORWARD => self.forward(),
            CMD_UP | CMD_BACKSPACE_UP => self.up(),
            CMD_THEME => self.set_dark(ui, !self.dark),
            CMD_DENSITY => {
                self.density = self.density.next();
                self.pull_appearance();
                self.config_dirty = true;
                self.apply_theme(ui);
            }
            CMD_ZEBRA => {
                self.zebra = !self.zebra;
                self.pull_appearance();
                self.config_dirty = true;
            }
            CMD_REFRESH => self.refresh(),
            CMD_VIEW_DETAILS => self.set_mode(ViewMode::Details),
            CMD_VIEW_GRID_SMALL => self.set_mode(ViewMode::GridSmall),
            CMD_VIEW_GRID_LARGE => self.set_mode(ViewMode::GridLarge),
            CMD_SELECT_ALL => {
                let t = self.tab_mut();
                t.sel.select_all(&t.store);
            }
            CMD_ESCAPE => {
                if self.cancel_edits() {
                    // a drag, a rename or the batch bar ended
                } else if !self.tab().filter.is_empty() {
                    self.filter.set_text("");
                    self.tab_mut().set_filter("");
                } else {
                    self.tab_mut().sel.clear();
                }
            }
            CMD_RENAME => self.begin_rename(ui),
            CMD_SIDEBAR => {
                self.side.set_rail(!self.side.rail);
                self.side_act(ui, Some(Act::Layout));
            }
            CMD_SPLIT => self.toggle_split(),
            CMD_SPLIT_ORIENTATION => self.flip_split(),
            CMD_SWITCH_PANE => {
                if let Some(e) = self.rename.as_ref() {
                    if e.valid() || e.unchanged() {
                        self.finish_rename(ui, crate::editing::RenameOut::CommitNext);
                    }
                } else if self.other.is_some() && !ui.has_focus(pid(self.live, ID_FILTER)) && !ui.has_focus(pid(self.live, ID_PATH_EDIT)) {
                    self.set_active_pane(panes::other(self.active));
                }
            }
            CMD_COPY_OTHER => {
                if self.other.is_some() {
                    self.copy_or_move_to_other(false);
                } else {
                    self.refresh();
                }
            }
            CMD_MOVE_OTHER => {
                if self.other.is_some() {
                    self.copy_or_move_to_other(true);
                }
            }
            CMD_UNDO => self.undo_op(),
            CMD_REDO => self.redo_op(),
            CMD_PIN_FOLDER => {
                let p = self.selected_folder().unwrap_or_else(|| self.folder_path());
                let n = self.side.pinned.len();
                if self.side.pin_at(&p, n) {
                    self.ops.message = Some(format!("Pinned {}", crate::dnd::name_of(&p)));
                    self.config_dirty = true;
                }
            }
            CMD_OPEN_OTHER => {
                if let Some(p) = self.selected_folder() {
                    self.open_in_other_pane(&p);
                }
            }
            CMD_OPEN_NEW_TAB => {
                if let Some(p) = self.selected_folder() {
                    self.open_in_new_tab(&p);
                }
            }
            CMD_TAB_TO_OTHER => self.move_tab_to_other(),
            CMD_SYNC_OTHER => {
                if self.other.is_some() {
                    let p = self.folder_path();
                    let slot = panes::other(self.active);
                    self.with_pane(slot, |v| v.go(&p));
                }
            }
            _ => {}
        }
        ui.request_repaint();
    }

    /// The folder under the cursor, when the cursor row is a folder.
    fn selected_folder(&self) -> Option<String> {
        let t = self.tab();
        let c = if self.mode.is_grid() { t.grid.cursor } else { t.list.cursor };
        t.entry_at(c).filter(|(_, dir)| *dir).map(|(p, _)| p)
    }

    /// Moves the active tab to the other pane (two panes only).
    fn move_tab_to_other(&mut self) {
        if self.other.is_none() || self.tabs.items.len() < 2 {
            return;
        }
        let i = self.tabs.active;
        let Some(tab) = self.tabs.close(i) else { return };
        self.switched();
        let slot = panes::other(self.active);
        self.with_pane(slot, |v| {
            v.tabs.add(tab);
            v.switched();
        });
        self.set_active_pane(slot);
    }

    /// Switches between details and the grids. The cursor row follows: it is
    /// the same entry in both, and both scroll it into view.
    pub fn set_mode(&mut self, mode: ViewMode) {
        if mode == self.mode {
            return;
        }
        let was_grid = self.mode.is_grid();
        self.mode = mode;
        self.config_dirty = true;
        for t in &mut self.tabs.items {
            let len = t.store.view().len();
            if mode.is_grid() {
                if !was_grid {
                    t.grid.cursor = t.list.cursor;
                }
                let (cw, ch, _, _) = mode.dims();
                t.grid.cell_w = cw;
                t.grid.cell_h = ch;
                t.grid.reveal = true;
            } else if was_grid {
                t.list.cursor = t.grid.cursor.min(len.saturating_sub(1));
                let c = t.list.cursor;
                if t.list.view_h > 0.0 {
                    t.list.ensure_visible(c, len);
                } else {
                    // the list was never drawn (started in a grid): no height
                    // to fit yet, so put the cursor row at the top
                    let top = t.list.row_top(c);
                    t.list.set_scroll(top, len);
                }
            }
        }
        self.focus_list = true;
    }

    /// Client position (DIP) of the middle of view row/cell `i` (keyboard menus).
    pub fn row_mid_y(&self, i: usize) -> f32 {
        if self.mode.is_grid() {
            let g = &self.tab().grid;
            return self.list_top + g.cell_origin(i).1 + g.cell_h / 2.0;
        }
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
        self.sync_tab_binding(ui);
        // the palette and the key recorder set this again while they are painted
        ui.capture_keys = false;
        // a press that starts in the other pane makes it the active one
        let held = ui.active().is_some();
        if held && !self.ptr_was_active && self.other.is_some() {
            let m = ui.mouse();
            if let Some(slot) = self.pane_rects.iter().position(|r| r.contains_pt(m)) {
                self.set_active_pane(slot);
            }
        }
        self.track_pointer(ui, held);
        self.ptr_was_active = held;
        if std::mem::take(&mut self.rename_request) && !self.tab().synthetic {
            self.begin_rename(ui);
        }
        if std::mem::take(&mut self.focus_path) {
            ui.set_focus(Some(pid(self.live, ID_PATH_EDIT)));
        } else if std::mem::take(&mut self.focus_list) {
            ui.set_focus(Some(pid(self.live, ID_LIST)));
        }
        let t = ui.theme;
        if self.other.is_some() {
            self.paint_dual(ui, g, shell, (w, h), &t);
        } else if self.tab().settings {
            let mut r = tsubame_ui::Rect::new(0.0, 0.0, w, h);
            let titlebar = r.cut_top(t.height_titlebar);
            self.paint_settings(ui, g, r);
            self.paint_titlebar(ui, g, titlebar, &t, true);
        } else {
            let mut r = tsubame_ui::Rect::new(0.0, 0.0, w, h);
            let titlebar = r.cut_top(t.height_titlebar);
            let toolbar = r.cut_top(t.height_toolbar);
            let status = r.cut_bottom(t.height_status_bar);
            let sidebar = r.cut_left(t.layout_sidebar_width);
            let batch_bar = r.cut_top(self.batch_h());
            let main = r;

            // body first: the later widgets (toolbar, title bar) win hit tests
            self.paint_side(ui, g, sidebar);
            self.paint_main(ui, g, shell, main, &t);
            if batch_bar.h > 0.0 {
                self.paint_batch_bar(ui, g, &t, batch_bar);
            }
            self.paint_status(g, status, &t, false);
            self.paint_toolbar(ui, g, toolbar, &t, false, false);
            self.paint_titlebar(ui, g, titlebar, &t, true);
            self.paint_path_error(g, toolbar, &t);
        }
        // a context menu asked for in the other pane: that pane is the active
        // one before main opens the menu (its rows, its folder)
        if let Some(slot) = self.menu_pane.take() {
            self.set_active_pane(slot);
        }
        let oa = self.side.paint_overlay(ui, g, self.other.is_some());
        self.side_act(ui, oa);
        self.paint_batch_preview(g, &t, (w, h));
        self.paint_palette(ui, g, (w, h));
        // the context menu is on top of everything
        if let Some(c) = self.ctx.as_mut() {
            c.paint(ui, g, Rect::new(0.0, 0.0, w, h));
        } else {
            // dropped by the app (picked, replaced, stress run): its popups go too
            ui.close_popup(crate::ctxui::ID_CTX.with(0));
        }
        let target = self.side_target.take();
        self.finish_drag(ui, g, &t, target.as_ref());
        self.sole_hist = [self.sole_hist[1], self.sole_row()];
        self.watch_tabs();

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
        let typed: Vec<char> = if self.tab().settings || self.palette.is_some() { Vec::new() } else { ui.leftover().iter().filter_map(|e| if let Event::Char(c) = e { Some(*c) } else { None }).collect() };
        for c in typed {
            if c.is_control() || (c == ' ' && self.filter.text.is_empty()) {
                continue;
            }
            if !ui.has_focus(pid(self.live, ID_FILTER)) {
                ui.set_focus(Some(pid(self.live, ID_FILTER)));
                self.filter.set_text("");
            }
            let mut b = [0u8; 4];
            self.filter.insert(c.encode_utf8(&mut b));
            let text = self.filter.text.clone();
            self.tab_mut().set_filter(&text);
            again = true;
        }
        // the pointer went up in this frame: one more frame sees the release
        // (drop, slow-click rename) even when nothing else asks for it
        if (self.drag.is_some() || self.press.is_some()) && self.ptr_was_active && ui.active().is_none() {
            again = true;
        }
        if again {
            out.repaint = true;
        }
        g.end();
        if let Some(c) = self.ctx.as_mut() { c.presented(); }
        Some(out)
    }

    /// The sidebar, with the drag in progress (if any) so it can take drops.
    fn paint_side(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect) {
        // drops are OLE drops (`shellops`): the sidebar only says where it lands
        let ctx = DragCtx { payload: self.drag.as_ref().map(|d| &d.payload), folder: self.drag.as_ref().and_then(|d| d.folder.as_deref()), released: false };
        let so = self.side.paint(ui, g, r, &self.tabs.items[self.tabs.active].path, &ctx);
        self.side_target = so.target;
        self.side_act(ui, so.act);
    }

    /// The cursor row when it is the only selected one (slow-click rename).
    fn sole_row(&self) -> Option<usize> {
        let t = self.tab();
        (t.sel.count() == 1).then_some(t.list.cursor)
    }

    /// Two panes: the title bar on top, the sidebar down the left, the panes
    /// (each with its own tab strip, toolbar and status bar) split by a bar.
    fn paint_dual(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, size: (f32, f32), t: &Theme) {
        let mut r = Rect::new(0.0, 0.0, size.0, size.1);
        let titlebar = r.cut_top(t.height_titlebar);
        let sidebar = r.cut_left(t.layout_sidebar_width);
        let area = r;
        let len = panes::len_of(area, self.orient);
        let mut pos = panes::pos_for(self.ratio, len);
        let sp = match self.orient {
            tsubame_config::Split::Vertical => Split::LeftRight,
            tsubame_config::Split::Horizontal => Split::TopBottom,
        };
        let (a, b, moved) = splitter(ui, g, ID_SPLIT, area, sp, &mut pos, panes::MIN_PANE, panes::MIN_PANE);
        if moved {
            self.ratio = panes::ratio_for(pos, len);
            self.config_dirty = true;
        }
        self.pane_rects = [a, b];
        self.paint_side(ui, g, sidebar);
        for slot in 0..2 {
            let rect = self.pane_rects[slot];
            let active = slot == self.active;
            self.with_pane(slot, |v| v.paint_pane(ui, g, shell, rect, t, active));
        }
        self.paint_titlebar(ui, g, titlebar, t, false);
    }

    /// One pane of two: tab strip, toolbar (accent line when active), list,
    /// status bar.
    fn paint_pane(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, rect: Rect, t: &Theme, active: bool) {
        let mut r = rect;
        let strip_r = r.cut_top(PANE_STRIP_H);
        if self.tab().settings {
            g.fill(strip_r, t.bg);
            self.paint_settings(ui, g, r);
            self.paint_tabs(ui, g, strip_r);
            return;
        }
        let toolbar = r.cut_top(t.height_toolbar);
        let status = r.cut_bottom(t.height_status_bar);
        let batch_bar = r.cut_top(self.batch_h());
        g.fill(strip_r, t.bg);
        self.paint_main(ui, g, shell, r, t);
        if batch_bar.h > 0.0 {
            self.paint_batch_bar(ui, g, t, batch_bar);
        }
        self.paint_status(g, status, t, active);
        self.paint_toolbar(ui, g, toolbar, t, panes::compact_toolbar(rect.w), active);
        self.paint_tabs(ui, g, strip_r);
        self.paint_path_error(g, toolbar, t);
    }

    /// The tab strip of the live pane in `r`.
    fn paint_tabs(&mut self, ui: &mut Ui, g: &mut dyn Canvas, strip_r: Rect) {
        let titles: Vec<&str> = self.tabs.items.iter().map(|t| t.title()).collect();
        let resp = tab_strip(ui, g, pid(self.live, ID_TABS), strip_r, &mut self.strip, &titles, self.tabs.active);
        // drops on a tab look the strip up per pane
        self.strips[self.live] = (strip_r, self.strip.scroll);
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

    /// Tab binds to "switch pane" (even in a text field: the command decides)
    /// while two panes or a rename are open; otherwise Tab keeps moving the
    /// keyboard focus along the widgets.
    fn sync_tab_binding(&mut self, ui: &mut Ui) {
        let want = self.other.is_some() || self.rename.is_some();
        let bound = ui.shortcuts.lookup(Key::Tab, Mods::NONE, true).is_some();
        if want != bound {
            ui.shortcuts.bind(Key::Tab, Mods::NONE, CMD_SWITCH_PANE, want);
        }
    }

    fn paint_titlebar(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme, with_tabs: bool) {
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
        // pane layout switch: one pane / side by side / stacked
        let sw = Rect::new(ctl.x - 8.0 - 3.0 * LAYOUT_BTN_W, r.y + (r.h - 24.0) / 2.0, 3.0 * LAYOUT_BTN_W, 24.0);
        if let Some((dual, orient)) = layout_switch(ui, g, t, sw, self.dual(), self.orient) {
            self.set_layout(dual, orient);
            ui.request_repaint();
        }
        // tabs (with two panes every pane has its own strip)
        if with_tabs {
            let strip_r = Rect::new(r.x + 40.0, r.y, (sw.x - r.x - 40.0 - 8.0).max(0.0), r.h);
            self.paint_tabs(ui, g, strip_r);
        }
    }

    /// `compact`: a narrow pane swaps the filter box for a search button.
    /// `underline`: the active pane of two gets an accent line under its toolbar.
    fn paint_toolbar(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme, compact: bool, underline: bool) {
        g.fill(r, t.panel);
        let by = r.y + (r.h - t.height_button) / 2.0;
        let bs = t.height_button;
        let (can_back, can_fwd, can_up) = {
            let tab = self.tab();
            (tab.history.can_back(), tab.history.can_forward(), nav::parent(&tab.path).is_some())
        };
        let mut x = r.x + t.space_4;
        if icon_button(ui, g, t, pid(self.live, ID_BACK), Rect::new(x, by, bs, bs), "\u{E72B}", can_back) {
            self.back();
        }
        x += bs + 2.0;
        if icon_button(ui, g, t, pid(self.live, ID_FWD), Rect::new(x, by, bs, bs), "\u{E72A}", can_fwd) {
            self.forward();
        }
        x += bs + 2.0;
        if icon_button(ui, g, t, pid(self.live, ID_UP), Rect::new(x, by, bs, bs), "\u{E74A}", can_up) {
            self.up();
        }
        x += bs + t.space_4;

        // filter box on the right, path bar takes the rest
        let filter_id = pid(self.live, ID_FILTER);
        let icon_only = compact && self.filter.text.is_empty() && !ui.has_focus(filter_id);
        let fw = if icon_only { bs } else if compact { 140.0 } else { t.layout_filter_width.min((r.r() - x) / 2.0).max(60.0) };
        let fr = Rect::new(r.r() - t.space_4 - fw, r.y + (r.h - t.height_path_bar) / 2.0, fw, t.height_path_bar);
        // view mode toggle between the path bar and the filter box
        let vb = Rect::new(fr.x - t.space_4 - bs, by, bs, bs);
        let glyph = match self.mode {
            ViewMode::Details => "\u{E8FD}",
            ViewMode::GridSmall => "\u{F0E2}",
            ViewMode::GridLarge => "\u{E8A9}",
        };
        if icon_button(ui, g, t, pid(self.live, ID_VIEW), vb, glyph, true) {
            let m = self.mode.next();
            self.set_mode(m);
        }
        let bar = Rect::new(x, fr.y, (vb.x - t.space_4 - x).max(40.0), t.height_path_bar);
        self.paint_path_bar(ui, g, bar, t);
        if icon_only {
            let sb = Rect::new(fr.x, by, bs, bs);
            if icon_button(ui, g, t, pid(self.live, ID_SEARCH), sb, "\u{E721}", true) {
                ui.set_focus(Some(filter_id));
                ui.request_repaint();
            }
        } else {
            self.paint_filter(ui, g, fr, t, compact);
        }
        if underline {
            g.fill(Rect::new(r.x, r.b() - 2.0, r.w, 2.0), t.accent);
        }

        // loading line under the toolbar: grows with the entries delivered
        let tab = self.tab();
        if tab.is_loading() {
            let frac = 1.0 - (-(tab.delivered as f32) / 20_000.0).exp();
            let lw = (r.w * frac).max(r.w * 0.02);
            g.fill(Rect::new(r.x, r.b() - t.height_progress_line, lw, t.height_progress_line), t.accent);
        }
    }

    fn paint_filter(&mut self, ui: &mut Ui, g: &mut dyn Canvas, fr: Rect, t: &Theme, compact: bool) {
        self.filter.pad_left = 20.0;
        let resp = text_input(ui, g, pid(self.live, ID_FILTER), fr, &mut self.filter, "Filter");
        g.text("\u{E721}", Rect::new(fr.x + 8.0, fr.y, 14.0, fr.h), f(12.0).icon().c(), t.text_dim);
        if self.filter.text.is_empty() && !resp.focused && !compact {
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
                ui.set_focus(Some(pid(self.live, ID_LIST)));
            } else {
                self.filter.set_text("");
                self.tab_mut().set_filter("");
            }
            ui.request_repaint();
        }
        if resp.submitted {
            ui.set_focus(Some(pid(self.live, ID_LIST)));
            ui.request_repaint();
        }
    }

    fn paint_path_bar(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bar: Rect, t: &Theme) {
        if self.editing {
            self.edit_frames += 1;
            self.path_text.pad_left = 22.0;
            let resp = text_input(ui, g, pid(self.live, ID_PATH_EDIT), bar, &mut self.path_text, "Type a path");
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
                ui.set_focus(Some(pid(self.live, ID_LIST)));
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
        let it = ui.interact(pid(self.live, ID_PATH), bar, Sense::click().cursor(Cursor::IBeam));
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
            let hit = ui.interact(pid(self.live, ID_PATH).child("more"), r, Sense::click().cursor(Cursor::Hand));
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
            let hit = ui.interact(pid(self.live, ID_PATH).child("crumb").with(i as u64), r, Sense::click().cursor(Cursor::Hand));
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
        // a new folder / another tab: pending shell work and thumbnails go
        shell.sync_generation(self.shell_key());
        if self.mode.is_grid() {
            self.paint_grid_main(ui, g, shell, main, t);
            return;
        }
        let mut area = main;
        let header_r = area.cut_top(t.height_column_header);
        let list_r = area;
        self.list_top = list_r.y;
        self.list_r = list_r;
        let total = main.w - t.layout_scrollbar_width;
        // a narrow pane of two drops the Type column
        let hide_type = self.other.is_some() && panes::hide_type_column(main.w);
        let spans = self.cols.spans_pane(total, hide_type);

        // header: sort and resize
        let all = Col::ALL.map(|c| tsubame_ui::HeaderCol { label: c.label(), right: c.right_aligned() });
        let shown: Vec<usize> = (0..all.len()).filter(|&i| !(hide_type && Col::ALL[i] == Col::Type)).collect();
        let hc: Vec<HeaderCol> = shown.iter().map(|&i| all[i]).collect();
        let hspans: Vec<(f32, f32)> = shown.iter().map(|&i| spans[i]).collect();
        let tab = &mut self.tabs.items[self.tabs.active];
        let sorted = shown.iter().position(|&i| Col::ALL[i].sort_column() == tab.sort.column).map(|i| (i, tab.sort.descending));
        let hr = column_header(ui, g, pid(self.live, ID_HEADER), header_r, &hc, &hspans, sorted);
        if let Some(i) = hr.sort {
            tab.sort_by(Col::ALL[shown[i]].sort_column());
        }
        if !hide_type
            && let Some((i, x)) = hr.resize
            && let Some(&col) = Col::ALL.get(i)
        {
            self.cols.drag_left_edge(col, x, total);
        }
        let spans = self.cols.spans_pane(total, hide_type);

        // the list
        g.fill(list_r, t.row);
        let tab = &mut self.tabs.items[self.tabs.active];
        tab.list.row_h = t.height_list_row;
        let (lr, activated) = {
            let Tab { store, sel, list: lstate, path, synthetic, .. } = &mut *tab;
            let root = if *synthetic { None } else { Some(path.as_str()) };
            let mut rows = Rows { store, sel, shell, row: &mut self.rowbuf, text: &mut self.text, spans, zebra: self.zebra, root };
            let lr = list(ui, g, pid(self.live, ID_LIST), list_r, lstate, &mut rows);
            // one page of icons beyond the viewport, nearest first
            rows.prefetch(lr.range, lstate.page_rows());
            (lr, lr.activated)
        };
        self.last_range = lr.range;
        match lr.context {
            Some((Some(row), pt)) => self.menu_request = Some((row, Some(pt))),
            Some((None, pt)) => self.bg_menu = Some(pt),
            None => {}
        }
        if lr.context_key {
            self.menu_request = Some((self.tabs.items[self.tabs.active].list.cursor, None));
        }
        if lr.context.is_some() || lr.context_key {
            self.menu_pane = Some(self.live);
        }
        self.paint_empty(g, list_r, t);
        self.paint_rename(ui, g, t, &spans);
        if let Some(pos) = activated {
            self.activate(pos);
            ui.request_repaint();
        }
    }

    /// Empty and failed folders.
    fn paint_empty(&self, g: &mut dyn Canvas, list_r: Rect, t: &Theme) {
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
    }

    /// The grid views: a sort strip (there are no columns) over the cells.
    fn paint_grid_main(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, main: Rect, t: &Theme) {
        let mut area = main;
        let strip_r = area.cut_top(t.height_column_header);
        let grid_r = area;
        self.list_top = grid_r.y;
        self.list_r = grid_r;
        self.sort_strip(ui, g, strip_r, t);

        let (cw, ch, box_dip, icon_dip) = self.mode.dims();
        let dpi = self.dpi;
        g.fill(grid_r, t.row);
        let tab = &mut self.tabs.items[self.tabs.active];
        tab.grid.cell_w = cw;
        tab.grid.cell_h = ch;
        // cells one screen shows (thumbnail size is chosen so they all fit the cache)
        let cols = tab.grid.cols_for(grid_r.w - t.layout_scrollbar_width);
        let rows_vis = (grid_r.h / ch).ceil() as usize + 1;
        shell.set_grid(GridPic { box_dip, icon_dip, dpi }, cols * rows_vis);
        let label_font = t.font().c();
        let (gr, activated) = {
            let Tab { store, sel, grid: gstate, band_base, path, synthetic, .. } = &mut *tab;
            let root = if *synthetic { None } else { Some(path.as_str()) };
            let mut tiles = Tiles { store, sel, shell, row: &mut self.rowbuf, root, base: band_base, labels: &mut self.labels, box_dip, font: label_font };
            let gr = grid(ui, g, pid(self.live, ID_LIST), grid_r, gstate, &mut tiles);
            tiles.prefetch(gstate);
            (gr, gr.activated)
        };
        self.last_range = gr.range;
        match gr.context {
            Some((Some(cell), pt)) => self.menu_request = Some((cell, Some(pt))),
            Some((None, pt)) => self.bg_menu = Some(pt),
            None => {}
        }
        if gr.context_key {
            self.menu_request = Some((self.tabs.items[self.tabs.active].grid.cursor, None));
        }
        if gr.context.is_some() || gr.context_key {
            self.menu_pane = Some(self.live);
        }
        self.paint_empty(g, grid_r, t);
        if let Some(pos) = activated {
            self.activate(pos);
            ui.request_repaint();
        }
    }

    /// "Sort by  Name  Date modified  Type  Size": chips that sort like the
    /// details header's columns (a second click flips the direction).
    fn sort_strip(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        g.fill(r, t.panel);
        g.fill(Rect::new(r.x, r.b() - 1.0, r.w, 1.0), t.border);
        let font = t.font_small();
        let mut x = r.x + 12.0;
        g.text("Sort by", Rect::new(x, r.y, 56.0, r.h - 1.0), font, t.text_dim);
        x += 60.0;
        let (sorted_col, desc) = {
            let tab = self.tab();
            (tab.sort.column, tab.sort.descending)
        };
        let mut clicked = None;
        for (i, col) in Col::ALL.iter().enumerate() {
            let is_sorted = col.sort_column() == sorted_col;
            let w = g.measure(col.label(), font) + 16.0 + if is_sorted { 14.0 } else { 0.0 };
            let cell = Rect::new(x, r.y + 3.0, w, r.h - 7.0);
            let it = ui.interact(pid(self.live, ID_HEADER).with(i as u64), cell, Sense::click());
            if it.hovered {
                g.round(cell, t.radius_sm, t.hover);
            }
            let color = if is_sorted || it.hovered { t.text } else { t.text_dim };
            g.text(col.label(), Rect::new(cell.x + 8.0, cell.y, w - 8.0, cell.h), font, color);
            if is_sorted {
                let glyph = if desc { "\u{E70D}" } else { "\u{E70E}" };
                g.text(glyph, Rect::new(cell.r() - 18.0, cell.y, 12.0, cell.h), f(9.0).icon().c(), t.text);
            }
            if it.clicked == Some(Button::Left) {
                clicked = Some(col.sort_column());
            }
            x += w + 2.0;
        }
        if let Some(c) = clicked {
            self.tab_mut().sort_by(c);
            ui.request_repaint();
        }
    }

    /// The status bar of the live pane. `hints`: this is the active pane of
    /// two, so the bar shows the copy / move keys for the other pane.
    fn paint_status(&mut self, g: &mut dyn Canvas, r: Rect, t: &Theme, hints: bool) {
        let tab = &self.tabs.items[self.tabs.active];
        let loading = matches!(tab.load, Load::Loading).then_some(tab.delivered);
        let side = panes::side_name(self.orient, self.live);
        let mut s = std::mem::take(&mut self.text);
        let info = StatusInfo {
            loading,
            failed: matches!(tab.load, Load::Failed(_)),
            shown: tab.shown(),
            total: tab.store.len(),
            selected: tab.sel.count(),
            selected_bytes: tab.sel.total_size().min(u64::MAX as u128) as u64,
            free: self.side.volume_of(&tab.path),
            other_side: hints.then_some(side),
            batch: self.batch.is_some() && self.batch_slot == self.live,
            message: if hints || self.other.is_none() { self.status_message() } else { None },
        };
        statusbar::paint(g, t, r, &info, &mut s);
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

const LAYOUT_BTN_W: f32 = 28.0;

/// The title bar's pane layout switch: one pane, side by side, stacked.
/// Returns the layout the user clicked.
fn layout_switch(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, r: Rect, dual: bool, orient: tsubame_config::Split) -> Option<(bool, tsubame_config::Split)> {
    use tsubame_config::Split as O;
    g.round_stroke(r, t.radius_md, t.border, 1.0);
    let mut out = None;
    let mut tip = None;
    let labels = ["Single pane", "Side by side", "Stacked"];
    for i in 0..3 {
        let b = Rect::new(r.x + i as f32 * LAYOUT_BTN_W, r.y, LAYOUT_BTN_W, r.h);
        let on = match i {
            0 => !dual,
            1 => dual && orient == O::Vertical,
            _ => dual && orient == O::Horizontal,
        };
        let it = ui.interact(ID_LAYOUT.with(i as u64), b, Sense::click());
        if on {
            g.round(b.pad(1.0), t.radius_sm, t.selected_inactive);
        } else if it.hovered {
            g.round(b.pad(1.0), t.radius_sm, t.hover);
        }
        let col = if on { t.accent } else { t.text_dim };
        // tiny icon: a frame with 0, 1 vertical or 1 horizontal divider
        let ic = Rect::new(b.x + (b.w - 14.0) / 2.0, b.y + (b.h - 11.0) / 2.0, 14.0, 11.0);
        g.round_stroke(ic, 2.0, col, 1.2);
        match i {
            1 => g.fill(Rect::new(ic.x + 7.0, ic.y + 1.0, 1.0, ic.h - 2.0), col),
            2 => g.fill(Rect::new(ic.x + 1.0, ic.y + 5.0, ic.w - 2.0, 1.0), col),
            _ => {}
        }
        if it.hovered && ui.active().is_none() {
            tip = Some((i, b));
        }
        if it.clicked == Some(Button::Left) && !on {
            out = Some(match i {
                0 => (false, orient),
                1 => (true, O::Vertical),
                _ => (true, O::Horizontal),
            });
        }
    }
    if let Some((i, b)) = tip {
        crate::overlay::tooltip(g, t, ui.size(), Pt { x: b.x, y: b.b() + 6.0 }, labels[i], &[]);
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

thread_local! {
    /// A plain press on a row of a multi-selection leaves the selection alone
    /// (it may be the start of a drag); the row waits here for the release.
    pub(crate) static KEEP_MULTI: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Click / key / space selection rules, shared by the list and the grid.
fn select_entry(store: &EntryStore, sel: &mut Selection, i: usize, mods: Mods, by: SelectBy) {
    let Some(id) = store.at(i) else { return };
    if by == SelectBy::Click && !mods.ctrl && !mods.shift && sel.count() > 1 && sel.contains(id) {
        KEEP_MULTI.with(|k| k.set(Some(i)));
        return;
    }
    match by {
        SelectBy::Click | SelectBy::Key if mods.shift => sel.extend(store, i),
        SelectBy::Click if mods.ctrl => sel.toggle(store, i),
        SelectBy::Key if mods.ctrl => {} // Ctrl+arrows move the cursor only
        SelectBy::Space if mods.ctrl => sel.toggle(store, i),
        SelectBy::Space | SelectBy::RightClick if sel.contains(id) => {}
        _ => sel.single(store, i),
    }
}

impl Rows<'_> {
    /// Asks for the icons of one page of rows above and below `range`, the
    /// nearest first (priority = rows away). Only registers wants; the
    /// shell's scheduler decides when they run.
    fn prefetch(&mut self, range: (usize, usize), page: usize) {
        let len = self.store.view().len();
        let (a, b) = range;
        for j in (a.saturating_sub(page)..a).chain(b..(b + page).min(len)) {
            let Some(id) = self.store.at(j) else { continue };
            self.row.fill(self.store, id);
            let dist = if j < a { a - j } else { j - b + 1 };
            self.shell.want_row(self.row, self.root, dist as f32);
        }
    }
}

impl ListSource for Rows<'_> {
    fn len(&self) -> usize {
        self.store.view().len()
    }
    fn is_selected(&self, i: usize) -> bool {
        self.store.at(i).is_some_and(|id| self.sel.contains(id))
    }
    fn select(&mut self, i: usize, mods: Mods, by: SelectBy) {
        select_entry(self.store, self.sel, i, mods, by);
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

/// Where the first label line of `name` ends for a box `w` DIPs wide. Backends
/// shape text to answer, so the answer is cached (direct mapped, fixed size).
fn label_split(cache: &mut [(u64, u32)], g: &mut dyn Canvas, name: &str, font: Font, w: f32) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h = (h ^ ((w.to_bits() as u64) << 20) ^ font.size10 as u64).wrapping_mul(0x0100_0000_01b3) | 1;
    let slot = (h >> 11) as usize % cache.len();
    if cache[slot].0 == h {
        let n = cache[slot].1 as usize;
        if n <= name.len() && name.is_char_boundary(n) {
            return n;
        }
    }
    let n = g.first_line_len(name, font, w).min(name.len());
    cache[slot] = (h, n as u32);
    n
}

/// The grid's view of one tab for a frame.
struct Tiles<'a> {
    store: &'a EntryStore,
    sel: &'a mut Selection,
    shell: &'a mut ShellUi,
    row: &'a mut RowBuf,
    /// folder of the cells (None for the bench's synthetic rows)
    root: Option<&'a str>,
    /// selection before a rubber band started (a Ctrl band adds to it)
    base: &'a mut Vec<EntryId>,
    labels: &'a mut [(u64, u32)],
    box_dip: f32,
    font: Font,
}

impl Tiles<'_> {
    /// Asks for the pictures of up to a page of cells on each side of the
    /// viewport (fewer when the thumbnail cache could not hold them), nearest
    /// first. Only registers wants; the shell's scheduler decides.
    fn prefetch(&mut self, st: &GridState) {
        let len = self.store.view().len();
        let (a, b) = st.visible(len);
        let cols = st.cols.max(1);
        let spare = self.shell.thumb_budget_cells().saturating_sub(b - a);
        let margin = st.page_rows().min(spare / 2 / cols);
        if margin == 0 {
            return;
        }
        let (pa, pb) = st.visible_with(len, margin);
        let (first_row, last_row) = (a / cols, b.div_ceil(cols));
        for j in (pa..a).chain(b..pb) {
            let Some(id) = self.store.at(j) else { continue };
            self.row.fill(self.store, id);
            let r = j / cols;
            let dist = if j < a { first_row - r } else { r + 1 - last_row.min(r) };
            self.shell.want_cell(self.row, self.root, dist.max(1) as f32);
        }
    }
}

impl GridSource for Tiles<'_> {
    fn len(&self) -> usize {
        self.store.view().len()
    }
    fn is_selected(&self, i: usize) -> bool {
        self.store.at(i).is_some_and(|id| self.sel.contains(id))
    }
    fn select(&mut self, i: usize, mods: Mods, by: SelectBy) {
        select_entry(self.store, self.sel, i, mods, by);
    }
    fn select_none(&mut self) {
        self.sel.clear();
    }
    fn band_begin(&mut self, additive: bool) {
        self.base.clear();
        if additive {
            let sel = &*self.sel;
            self.base.extend(self.store.view().iter().copied().filter(|&id| sel.contains(id)));
        } else {
            self.sel.clear();
        }
    }
    fn band_update(&mut self, cells: Option<BandCells>, cols: usize) {
        let store = self.store;
        let len = store.view().len();
        let covered = cells.into_iter().flat_map(move |b| b.indices(cols, len)).filter_map(move |i| store.at(i));
        let ids = self.base.iter().copied().chain(covered);
        self.sel.replace_with(store, ids);
    }
    fn band_end(&mut self) {
        self.base.clear();
        self.base.shrink_to(256);
    }
    fn draw_cell(&mut self, c: &mut dyn Canvas, t: &Theme, i: usize, r: Rect, _st: RowState) {
        let Some(id) = self.store.at(i) else { return };
        let row = &mut *self.row;
        row.fill(self.store, id);
        let boxr = Rect::new(r.x + (r.w - self.box_dip) / 2.0, r.y + 8.0, self.box_dip, self.box_dip);
        self.shell.paint_pic(c, t, row, self.root, boxr);
        // name: at most two lines, centred, the second one ends in an ellipsis
        let name = row.name_str();
        let lw = r.w - 8.0;
        let n = label_split(self.labels, c, name, self.font, lw);
        let color = if row.hidden { t.text_dim } else { t.text };
        let y = boxr.b() + 6.0;
        let (l1, l2) = name.split_at(n);
        c.text(l1, Rect::new(r.x + 4.0, y, lw, LABEL_LINE), self.font, color);
        if !l2.is_empty() {
            c.text(l2, Rect::new(r.x + 4.0, y + LABEL_LINE, lw, LABEL_LINE), self.font, color);
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

    // ---- whole-frame tests on the kit's recording canvas -----------------

    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tsubame_ui::{LIGHT, TestCanvas};

    struct Rig {
        v: View,
        ui: Ui,
        c: TestCanvas,
        shell: ShellUi,
        dir: std::path::PathBuf,
    }

    impl Rig {
        fn new(tag: &str) -> Rig {
            let dir = std::env::temp_dir().join(format!("tsubame-u2-view-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("Docs")).unwrap();
            std::fs::write(dir.join("alpha.txt"), b"a").unwrap();
            std::fs::write(dir.join("beta.txt"), b"bb").unwrap();
            let wake: Wake = Arc::new(|| {});
            let v = View::new(dir.to_str().unwrap(), wake, true, Density::Compact);
            let mut ui = Ui::new(v.base_theme());
            v.keymap.bind_all(&mut ui);
            let mut r = Rig { v, ui, c: TestCanvas::new(1120.0, 720.0), shell: ShellUi::new(16), dir };
            r.settle();
            r
        }
        fn settle(&mut self) {
            let t0 = Instant::now();
            let loading = |v: &mut View| (0..2).any(|s| v.with_pane(s, |v| v.tabs.items.iter().any(|t| t.is_loading())));
            while loading(&mut self.v) {
                self.v.pump();
                assert!(t0.elapsed() < Duration::from_secs(10), "listing did not finish");
                std::thread::sleep(Duration::from_millis(1));
            }
            self.v.pump();
            self.frame();
        }
        /// two frames: hit-testing uses the previous frame's widgets
        fn frame(&mut self) {
            for _ in 0..2 {
                self.c.clear();
                self.shell.begin_frame();
                assert!(self.v.paint(&mut self.ui, &mut self.c, &mut self.shell).is_some());
            }
        }
        fn key(&mut self, key: Key, mods: Mods) {
            self.ui.push(Event::KeyDown { key, mods, repeat: false });
            self.frame();
        }
        fn click(&mut self, x: f32, y: f32) {
            let pos = Pt::new(x, y);
            self.ui.push(Event::MouseMove { pos });
            self.frame();
            self.ui.push(Event::MouseDown { pos, button: Button::Left, mods: Mods::NONE, double: false });
            self.frame();
            self.ui.push(Event::MouseUp { pos, button: Button::Left, mods: Mods::NONE });
            self.frame();
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn frame_shows_rows_crumbs_and_status() {
        let r = Rig::new("frame");
        for name in ["Docs", "alpha.txt", "beta.txt"] {
            assert!(r.c.has_text(name), "{name} missing: {:?}", r.c.texts());
        }
        assert!(r.c.has_text("3 items"), "{:?}", r.c.texts());
        assert!(r.c.has_text("Name") && r.c.has_text("Date modified") && r.c.has_text("Type") && r.c.has_text("Size"));
        assert!(r.c.texts().iter().any(|t| t.starts_with("tsubame-u2-view-frame")), "crumb of the folder: {:?}", r.c.texts());
        assert_eq!(r.c.clip_depth(), 0);
    }

    #[test]
    fn typing_in_the_list_filters_and_escape_clears() {
        let mut r = Rig::new("typing");
        r.ui.push(Event::Char('b'));
        r.frame();
        assert_eq!(r.v.tab().filter, "b");
        assert_eq!(r.v.tab().shown(), 1, "only beta.txt has a b");
        assert!(r.c.has_text("1 of 3 items"), "{:?}", r.c.texts());
        r.key(Key::Escape, Mods::NONE);
        assert_eq!(r.v.tab().filter, "");
        assert_eq!(r.v.tab().shown(), 3);
    }

    #[test]
    fn tabs_open_close_and_keep_their_own_state() {
        let mut r = Rig::new("tabs");
        assert_eq!(r.v.tabs.len(), 1);
        r.key(Key::Char('T'), Mods::CTRL);
        r.settle();
        assert_eq!(r.v.tabs.len(), 2);
        assert_eq!(r.v.tabs.active, 1);
        // sort in tab 2 does not touch tab 1
        r.v.tab_mut().sort_by(tsubame_core::Column::Size);
        r.key(Key::Tab, Mods::CTRL);
        assert_eq!(r.v.tabs.active, 0);
        assert_eq!(r.v.tab().sort.column, tsubame_core::Column::Name);
        r.key(Key::Char('W'), Mods::CTRL);
        assert_eq!(r.v.tabs.len(), 1);
        assert!(!r.v.quit);
        r.key(Key::Char('W'), Mods::CTRL);
        assert!(r.v.quit, "closing the last tab ends the window");
    }

    #[test]
    fn history_buttons_and_keys() {
        let mut r = Rig::new("hist");
        let root = r.v.tab().path.clone();
        let docs = format!("{root}\\Docs");
        r.v.go(&docs);
        r.settle();
        assert_eq!(r.v.tab().path, docs);
        assert!(r.c.has_text("This folder is empty"));
        r.key(Key::Left, Mods::ALT);
        r.settle();
        assert_eq!(r.v.tab().path, root);
        r.key(Key::Right, Mods::ALT);
        r.settle();
        assert_eq!(r.v.tab().path, docs);
        r.key(Key::Up, Mods::ALT);
        r.settle();
        assert_eq!(r.v.tab().path, root);
        // Enter on a folder row opens it
        r.v.activate(0);
        r.settle();
        assert_eq!(r.v.tab().path, docs);
    }

    #[test]
    fn path_edit_navigates_or_shows_an_error() {
        let mut r = Rig::new("pathedit");
        r.key(Key::Char('L'), Mods::CTRL);
        assert!(r.v.editing);
        // a bad path: the bar stays in edit mode with an error
        r.v.path_text.set_text("C:\\definitely\\not\\here");
        r.key(Key::Enter, Mods::NONE);
        r.settle();
        r.settle();
        assert!(r.v.editing, "error keeps the field open");
        assert!(r.c.texts().iter().any(|t| t.starts_with("Can't open")), "{:?}", r.c.texts());
        assert_eq!(r.v.tab().path, r.dir.to_str().unwrap(), "back on the old folder");
        // Escape cancels
        r.key(Key::Escape, Mods::NONE);
        assert!(!r.v.editing);
    }

    #[test]
    fn theme_and_density_switch_at_runtime() {
        let mut r = Rig::new("theme");
        assert!(r.v.dark);
        r.key(Key::Char('L'), Mods::CTRL_SHIFT);
        assert!(!r.v.dark);
        assert_eq!(r.ui.theme.bg, LIGHT.bg);
        assert_eq!(r.ui.theme.height_list_row, 22.0);
        r.key(Key::Char('D'), Mods::CTRL_SHIFT);
        assert_eq!(r.v.density, Density::Comfortable);
        assert_eq!(r.ui.theme.height_list_row, 26.0);
        assert_eq!(r.v.tab().list.row_h, 26.0);
        r.key(Key::Char('D'), Mods::CTRL_SHIFT);
        r.key(Key::Char('D'), Mods::CTRL_SHIFT);
        assert_eq!(r.v.density, Density::Compact);
    }

    #[test]
    fn selection_counts_show_in_the_status_bar() {
        let mut r = Rig::new("sel");
        r.key(Key::Char('A'), Mods::CTRL);
        assert_eq!(r.v.tab().sel.count(), 3);
        assert!(r.c.texts().iter().any(|t| t.starts_with("3 selected")), "{:?}", r.c.texts());
        r.key(Key::Escape, Mods::NONE);
        assert_eq!(r.v.tab().sel.count(), 0);
    }

    #[test]
    fn view_mode_shortcuts_switch_and_keep_the_cursor() {
        let mut r = Rig::new("mode");
        assert_eq!(r.v.mode, ViewMode::Details);
        r.key(Key::Down, Mods::NONE);
        assert_eq!(r.v.tab().list.cursor, 1);
        r.key(Key::Char('2'), Mods::CTRL_SHIFT);
        assert_eq!(r.v.mode, ViewMode::GridSmall);
        assert_eq!(r.v.tab().grid.cursor, 1, "same entry in the grid");
        assert_eq!((r.v.tab().grid.cell_w, r.v.tab().grid.cell_h), (96.0, 118.0));
        for name in ["Docs", "alpha.txt", "beta.txt"] {
            assert!(r.c.has_text(name), "{name} missing in grid: {:?}", r.c.texts());
        }
        assert!(r.c.has_text("Sort by"), "{:?}", r.c.texts());
        r.key(Key::Right, Mods::NONE);
        assert_eq!(r.v.tab().grid.cursor, 2);
        r.key(Key::Char('3'), Mods::CTRL_SHIFT);
        assert_eq!(r.v.mode, ViewMode::GridLarge);
        assert_eq!((r.v.tab().grid.cell_w, r.v.tab().grid.cell_h), (152.0, 182.0));
        r.key(Key::Char('1'), Mods::CTRL_SHIFT);
        assert_eq!(r.v.mode, ViewMode::Details);
        assert_eq!(r.v.tab().list.cursor, 2, "back in details on the same entry");
        assert_eq!(r.c.clip_depth(), 0);
    }

    #[test]
    fn grid_selects_with_clicks_and_rubber_band() {
        let mut r = Rig::new("band");
        r.v.set_mode(ViewMode::GridSmall);
        r.frame();
        let t = r.ui.theme;
        let top = t.height_titlebar + t.height_toolbar + t.height_column_header;
        let left = t.layout_sidebar_width;
        // first cell: click selects one
        r.click(left + 40.0, top + 40.0);
        assert_eq!(r.v.tab().sel.count(), 1);
        // a rubber band from empty space (right of the cells) back across all three cells
        let pos = Pt::new(left + 600.0, top + 100.0);
        r.ui.push(Event::MouseMove { pos });
        r.frame();
        r.ui.push(Event::MouseDown { pos, button: Button::Left, mods: Mods::NONE, double: false });
        r.frame();
        for p in [Pt::new(left + 300.0, top + 60.0), Pt::new(left + 10.0, top + 20.0)] {
            r.ui.push(Event::MouseMove { pos: p });
            r.frame();
        }
        assert_eq!(r.v.tab().sel.count(), 3, "the band covers all three cells");
        r.ui.push(Event::MouseUp { pos: Pt::new(left + 10.0, top + 20.0), button: Button::Left, mods: Mods::NONE });
        r.frame();
        assert_eq!(r.v.tab().sel.count(), 3, "the selection stays after release");
        assert!(r.v.tab().band_base.is_empty(), "base released");
    }

    #[test]
    fn grid_sort_chips_sort_like_the_header() {
        let mut r = Rig::new("chips");
        r.v.set_mode(ViewMode::GridSmall);
        r.frame();
        let t = r.ui.theme;
        let y = t.height_titlebar + t.height_toolbar + t.height_column_header / 2.0;
        // "Sort by" is 60 DIPs wide, "Name" is the first chip, the sidebar comes first
        r.click(t.layout_sidebar_width + 12.0 + 60.0 + 14.0, y);
        assert!(r.v.tab().sort.descending, "a second click on the sorted column flips it");
        assert_eq!(r.v.tab().sort.column, tsubame_core::Column::Name);
    }

    #[test]
    fn labels_wrap_to_two_lines_and_the_split_is_cached() {
        let mut c = TestCanvas::new(400.0, 300.0);
        let mut cache = vec![(0u64, 0u32); 8];
        let font = f(12.0);
        let name = "a-very-long-file-name-without-spaces-that-needs-two-lines.png";
        let n = label_split(&mut cache, &mut c, name, font, 80.0);
        assert!(n > 0 && n < name.len(), "{n}");
        assert!(name.is_char_boundary(n));
        assert_eq!(label_split(&mut cache, &mut c, name, font, 80.0), n, "cache hit gives the same split");
        assert_eq!(label_split(&mut cache, &mut c, "short.txt", font, 80.0), "short.txt".len(), "fits one line");
    }

    #[test]
    fn view_mode_names_round_trip() {
        for m in [ViewMode::Details, ViewMode::GridSmall, ViewMode::GridLarge] {
            assert_eq!(ViewMode::parse(m.name()), Some(m));
        }
        assert_eq!(ViewMode::parse("nope"), None);
        assert_eq!(ViewMode::Details.next().next().next(), ViewMode::Details);
    }

    // ---- INT1: two panes act as one app ---------------------------------------

    impl Rig {
        fn dual(tag: &str) -> Rig {
            let mut r = Rig::new(tag);
            r.v.set_layout(true, tsubame_config::Split::Vertical);
            r.settle();
            r
        }
        fn move_to(&mut self, x: f32, y: f32) {
            self.ui.push(Event::MouseMove { pos: Pt::new(x, y) });
            self.frame();
        }
        fn right_click(&mut self, x: f32, y: f32) {
            let pos = Pt::new(x, y);
            self.move_to(x, y);
            self.ui.push(Event::MouseDown { pos, button: Button::Right, mods: Mods::NONE, double: false });
            self.ui.push(Event::MouseUp { pos, button: Button::Right, mods: Mods::NONE });
            self.frame();
        }
        /// (list rect, row height) of pane `slot`
        fn list_of(&mut self, slot: usize) -> (Rect, f32) {
            self.v.with_pane(slot, |v| (v.list_r, v.tab().list.row_h))
        }
    }

    #[test]
    fn clicks_and_keys_go_to_the_pane_they_are_in() {
        let mut r = Rig::dual("keys");
        assert_eq!(r.v.active, 0);
        let (l1, rh) = r.list_of(1);
        // a click on pane 1's second row: pane 1 is active and its list has the keys
        r.click(l1.x + 40.0, l1.y + rh * 1.5);
        assert_eq!(r.v.active, 1);
        assert_eq!(r.v.live, 1, "live == active outside a paint");
        assert!(r.v.list_focused(&r.ui), "the active pane's list has the focus");
        assert!(r.ui.has_focus(pid(1, ID_LIST)) && !r.ui.has_focus(pid(0, ID_LIST)));
        assert_eq!(r.v.tab().sel.count(), 1);
        // F2 renames in pane 1, not pane 0
        r.key(Key::F(2), Mods::NONE);
        assert!(r.v.rename.is_some());
        assert_eq!(r.v.rename_slot, 1);
        assert!(!r.v.list_focused(&r.ui), "file keys stay off while renaming");
        r.key(Key::Escape, Mods::NONE);
        assert!(r.v.rename.is_none());
        // back to pane 0
        let (l0, rh) = r.list_of(0);
        r.click(l0.x + 40.0, l0.y + rh * 0.5);
        assert_eq!(r.v.active, 0);
        assert!(r.ui.has_focus(pid(0, ID_LIST)));
    }

    #[test]
    fn context_menus_belong_to_the_pane_clicked() {
        let mut r = Rig::dual("menus");
        let (l1, rh) = r.list_of(1);
        // empty space of pane 1: the folder menu, pane 1 made active first
        r.right_click(l1.x + 40.0, l1.b() - 10.0);
        assert_eq!(r.v.active, 1);
        assert!(r.v.bg_menu.take().is_some());
        assert!(r.v.menu_request.is_none());
        // a row of pane 0: the item menu for that row, pane 0 active again
        let (l0, _) = r.list_of(0);
        r.right_click(l0.x + 40.0, l0.y + rh * 1.5);
        assert_eq!(r.v.active, 0);
        assert_eq!(r.v.menu_request.take().map(|m| m.0), Some(1));
        // the shell menu's Rename: inline rename of that row on the next frame
        r.v.rename_request = true;
        r.frame();
        assert!(r.v.rename.is_some() && r.v.rename_slot == 0);
    }

    #[test]
    fn drop_targets_follow_the_pointer_across_panes_and_tabs() {
        let mut r = Rig::dual("drops");
        let docs = r.dir.join("Docs").to_str().unwrap().to_string();
        let alpha = r.dir.join("alpha.txt").to_str().unwrap().to_string();
        let wake = r.v.wake.clone();
        r.v.with_pane(1, |v| {
            v.tab_mut().history.push(&docs);
            v.tab_mut().start(&docs, wake);
        });
        r.settle();
        r.v.drag_enter(vec![alpha.clone()]);
        // over pane 1 (showing Docs): into Docs
        let (l1, _) = r.list_of(1);
        r.move_to(l1.x + 40.0, l1.b() - 10.0);
        assert_eq!(r.v.drop_dest, Some(DropDest::Folder(docs.clone())));
        assert!(r.c.has_text("Move to Docs"), "{:?}", r.c.texts());
        // over pane 0's empty space: its own folder is refused
        let (l0, rh) = r.list_of(0);
        r.move_to(l0.x + 40.0, l0.b() - 10.0);
        assert_eq!(r.v.drop_dest, None);
        assert!(r.c.has_text("Already in this folder"));
        // over the Docs row (first: folders first) of pane 0
        r.move_to(l0.x + 40.0, l0.y + rh * 0.5);
        assert_eq!(r.v.drop_dest, Some(DropDest::Folder(docs.clone())));
        // over pane 1's tab
        let (strip, _) = r.v.strips[1];
        r.move_to(strip.x + 30.0, strip.b() - 8.0);
        assert_eq!(r.v.drop_dest, Some(DropDest::Folder(docs.clone())));
        // Ctrl+Shift (the OLE keys): a shortcut
        r.v.drag_keys = Some(Mods { ctrl: true, shift: true, alt: false });
        r.move_to(l1.x + 40.0, l1.b() - 12.0);
        assert!(r.c.has_text("Shortcut in Docs"), "{:?}", r.c.texts());
        r.v.drag_leave();
        r.frame();
        assert_eq!(r.v.drop_dest, None);
        assert!(r.v.drag.is_none());
    }

    #[test]
    fn a_press_on_a_multi_selection_drags_all_of_it() {
        let mut r = Rig::new("multi");
        let (l, rh) = r.list_of(0);
        {
            let t = r.v.tab_mut();
            t.sel.select_all(&t.store);
        }
        r.frame();
        let n = r.v.tab().sel.count();
        assert_eq!(n, 3);
        let p = Pt::new(l.x + 40.0, l.y + rh * 1.5);
        r.move_to(p.x, p.y);
        r.ui.push(Event::MouseDown { pos: p, button: Button::Left, mods: Mods::NONE, double: false });
        r.frame();
        assert_eq!(r.v.tab().sel.count(), 3, "the press keeps the selection");
        // under the threshold: nothing yet
        r.move_to(p.x + 2.0, p.y);
        assert!(r.v.drag_out.is_none());
        r.move_to(p.x + 12.0, p.y + 4.0);
        assert_eq!(r.v.drag_out.as_ref().map(|v| v.len()), Some(3), "the whole selection is dragged");
        r.ui.push(Event::MouseUp { pos: p, button: Button::Left, mods: Mods::NONE });
        r.frame();
        assert_eq!(r.v.tab().sel.count(), 3, "a drag does not collapse the selection");
        // a plain click (no drag) collapses it to the row
        r.v.drag_out = None;
        r.click(p.x, p.y);
        assert_eq!(r.v.tab().sel.count(), 1);
    }

    #[test]
    fn finished_jobs_select_new_items_in_every_pane_showing_the_folder() {
        let mut r = Rig::dual("after");
        let dir = r.dir.to_str().unwrap().to_string();
        std::fs::write(r.dir.join("new.txt"), b"n").unwrap();
        r.v.ops.changed(&dir, vec!["new.txt".into()]);
        r.v.after_ops();
        // watched panes get it from their watcher, unwatched ones relist
        let t0 = Instant::now();
        loop {
            r.v.pump();
            r.frame();
            let ok = (0..2).all(|s| r.v.with_pane(s, |v| v.tab().sel.count() == 1 && v.tab().selected_paths(1)[0].ends_with("new.txt")));
            if ok {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "new.txt not selected in both panes");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn sidebar_click_navigates() {
        let mut r = Rig::new("side");
        let target = r.dir.join("Docs").to_str().unwrap().to_string();
        r.v.side.places = vec![tsubame_win::places::Place { name: "Desktop", path: target.clone() }];
        r.frame();
        // the only quick-access row: below the title bar and toolbar, the Pinned section
        // (header, empty hint, gap) and the Quick access header
        let t = r.ui.theme;
        let y = t.height_titlebar + t.height_toolbar + 4.0 + 26.0 + 24.0 + 4.0 + 26.0 + 12.0;
        r.click(60.0, y);
        r.settle();
        assert_eq!(r.v.tab().path, target);
    }

    // ---- U4: palette, Settings tab, live preferences ---------------------

    use crate::settings::{C_APPEARANCE, C_HIDE, C_KEYS, Page};

    fn settings_tabs(v: &mut View) -> usize {
        (0..2).filter_map(|s| v.pane_tabs(s).map(|t| t.items.iter().filter(|t| t.settings).count())).sum()
    }

    #[test]
    fn palette_opens_with_both_chords_and_closes_with_escape() {
        let mut r = Rig::new("u4-pal");
        assert!(!r.v.palette_open());
        r.key(Key::Char('P'), Mods::CTRL_SHIFT);
        assert!(r.v.palette_open());
        r.key(Key::Escape, Mods::NONE);
        assert!(!r.v.palette_open());
        r.key(Key::Char('K'), Mods::CTRL);
        assert!(r.v.palette_open());
        // the same chord toggles it shut again
        r.key(Key::Char('P'), Mods::CTRL_SHIFT);
        assert!(!r.v.palette_open());
        assert!(!r.ui.capture_keys);
    }

    #[test]
    fn a_prefix_typed_over_a_selection_still_switches_the_mode() {
        let mut r = Rig::new("u4-prefix");
        r.key(Key::Char('K'), Mods::CTRL);
        for c in "rn".chars() {
            r.ui.push(Event::Char(c));
            r.frame();
        }
        assert_eq!(r.v.palette.as_ref().unwrap().mode, crate::palette::Mode::Commands);
        r.key(Key::Char('A'), Mods::CTRL);
        r.ui.push(Event::Char('/'));
        r.frame();
        let p = r.v.palette.as_ref().unwrap();
        assert_eq!(p.mode, crate::palette::Mode::Path);
        assert!(p.input.text.is_empty());
    }

    #[test]
    fn running_a_palette_command_puts_it_first_in_the_recents() {
        let mut r = Rig::new("u4-recent");
        let zebra = r.v.zebra;
        r.v.run_palette_action(&mut r.ui, crate::palette::Action::Run { id: CMD_ZEBRA, keep: false });
        r.v.run_palette_action(&mut r.ui, crate::palette::Action::Run { id: CMD_SIDEBAR, keep: false });
        r.v.run_palette_action(&mut r.ui, crate::palette::Action::Run { id: CMD_ZEBRA, keep: false });
        assert_eq!(r.v.recents, vec![CMD_ZEBRA, CMD_SIDEBAR]);
        // the command really ran (twice: zebra is back where it was)
        assert_eq!(r.v.zebra, zebra);
    }

    #[test]
    fn settings_is_one_tab_across_both_panes_and_is_not_saved() {
        let mut r = Rig::new("u4-single");
        r.key(Key::Char(','), Mods::CTRL);
        r.key(Key::Char(','), Mods::CTRL);
        assert_eq!(settings_tabs(&mut r.v), 1);
        assert!(r.v.settings_open());
        assert!(r.v.tab().settings);
        // a second pane does not get a second copy
        r.v.set_layout(true, tsubame_config::Split::Vertical);
        r.v.set_active_pane(1);
        r.v.open_settings(None);
        assert_eq!(settings_tabs(&mut r.v), 1);
        // session capture skips it
        let s = crate::session::capture(&r.v, tsubame_config::WindowState::default());
        assert!(s.panes.iter().all(|p| !p.tabs.is_empty() && p.tabs.iter().all(|t| !t.path.is_empty())));
        // closing it frees the page state
        let slot = (0..2).find(|&s| r.v.pane_tabs(s).is_some_and(|t| t.items.iter().any(|t| t.settings))).unwrap();
        r.v.set_active_pane(slot);
        let i = r.v.tabs.items.iter().position(|t| t.settings).unwrap();
        r.v.close_tab(i);
        assert!(!r.v.settings_open());
        assert_eq!(settings_tabs(&mut r.v), 0);
    }

    #[test]
    fn every_settings_page_paints_in_both_themes_and_three_densities() {
        use tsubame_config::Density as D;
        let mut r = Rig::new("u4-pages");
        r.v.open_settings(None);
        for theme in ["tsubame-dark", "tsubame-light"] {
            for d in [D::Compact, D::Normal, D::Comfortable] {
                r.v.prefs.appearance.theme = theme.to_string();
                r.v.prefs.appearance.density = d;
                r.v.apply_prefs(&mut r.ui, C_APPEARANCE);
                for p in Page::ALL {
                    r.v.open_settings(Some(p));
                    r.frame();
                    assert!(r.c.ops.len() > 20, "{theme} {d:?} {p:?} drew almost nothing");
                }
            }
        }
        r.v.prefs.appearance.density = D::Compact;
        r.v.apply_prefs(&mut r.ui, C_APPEARANCE);
    }

    #[test]
    fn appearance_changes_apply_live() {
        let mut r = Rig::new("u4-live");
        let rows = r.ui.theme.height_list_row;
        r.v.prefs.appearance.density = tsubame_config::Density::Comfortable;
        r.v.apply_prefs(&mut r.ui, C_APPEARANCE);
        assert!(r.ui.theme.height_list_row > rows);
        assert!(r.v.config_dirty);
        r.v.prefs.appearance.theme = "tsubame-light".into();
        r.v.apply_prefs(&mut r.ui, C_APPEARANCE);
        assert!(!r.v.dark);
        assert_eq!(r.v.theme_id, "tsubame-light");
        r.v.prefs.appearance.accent = Some("#ff8800".into());
        r.v.prefs.appearance.font_scale = 1.25;
        r.v.apply_prefs(&mut r.ui, C_APPEARANCE);
        assert_eq!(r.ui.theme.accent & 0xffffff, 0xff8800);
        r.frame();
    }

    #[test]
    fn show_hidden_toggle_changes_the_listing_live() {
        use std::os::windows::process::CommandExt;
        let mut r = Rig::new("u4-hide");
        let hidden = r.dir.join("secret.txt");
        std::fs::write(&hidden, b"x").unwrap();
        let ok = std::process::Command::new("attrib").arg("+h").arg(&hidden).creation_flags(0x0800_0000).status().map(|s| s.success()).unwrap_or(false);
        assert!(ok, "attrib failed");
        r.v.go(r.dir.to_str().unwrap());
        r.settle();
        // the default rule hides hidden and system files
        r.v.prefs.general.show_hidden = false;
        r.v.prefs.general.show_system = false;
        r.v.apply_prefs(&mut r.ui, C_HIDE);
        r.settle();
        let before = r.v.tab().shown();
        assert_eq!(before, 3, "alpha, beta, Docs");
        r.v.prefs.general.show_hidden = true;
        r.v.apply_prefs(&mut r.ui, C_HIDE);
        r.settle();
        assert_eq!(r.v.tab().shown(), before + 1);
        // the dev-run default (everything shown) is back for the tests that follow
        r.v.prefs.general.show_system = true;
        r.v.apply_prefs(&mut r.ui, C_HIDE);
    }

    #[test]
    fn rebinding_a_key_applies_live_and_survives_a_round_trip() {
        use crate::keymap::{Chord, KeyMap};
        let mut r = Rig::new("u4-rebind");
        let chord = Chord::new(Key::Char('J'), Mods::CTRL);
        assert_eq!(r.v.keymap.assign(CMD_ZEBRA, 0, chord), Ok(None));
        r.v.apply_prefs(&mut r.ui, C_KEYS);
        // saved as an override ...
        assert!(!r.v.prefs.keys.overrides.is_empty());
        let (back, unknown) = KeyMap::from_overrides(&r.v.prefs.keys.overrides);
        assert!(unknown.is_empty());
        assert_eq!(back.label(CMD_ZEBRA), "Ctrl+J");
        // ... and the new chord runs the command
        let zebra = r.v.zebra;
        r.key(Key::Char('J'), Mods::CTRL);
        assert_ne!(r.v.zebra, zebra);
        // reset all brings the defaults back, live
        r.v.run_command(&mut r.ui, CMD_RESET_KEYS);
        assert!(r.v.prefs.keys.overrides.is_empty());
        assert_eq!(r.v.keymap.changed(), 0);
        let zebra = r.v.zebra;
        r.key(Key::Char('J'), Mods::CTRL);
        assert_eq!(r.v.zebra, zebra);
    }

    #[test]
    fn palette_and_settings_open_close_leaves_nothing_behind() {
        let mut r = Rig::new("u4-churn");
        for _ in 0..200 {
            r.key(Key::Char('P'), Mods::CTRL_SHIFT);
            r.key(Key::Escape, Mods::NONE);
            r.v.open_settings(None);
            r.frame();
            let i = r.v.tabs.items.iter().position(|t| t.settings).unwrap();
            r.v.close_tab(i);
        }
        assert!(!r.v.palette_open());
        assert!(!r.v.settings_open());
        assert_eq!(settings_tabs(&mut r.v), 0);
        assert_eq!(r.v.tabs.items.len(), 1);
        for _ in 0..1000 {
            r.v.churn_u4();
        }
        assert!(!r.v.palette_open());
        assert!(!r.v.settings_open());
        assert_eq!(settings_tabs(&mut r.v), 0);
        assert_eq!(r.v.tabs.items.len(), 1);
    }
}
