//! Session capture and restore, and the debounced config/session saves.
//!
//! The schema and the file I/O live in `tsubame-config`; this module is the
//! thin app side: it turns the running `View` into a [`Session`] and back, and
//! owns the two save gates.
//!
//! Restore is lazy on purpose (first paint budget): the active tab is the one
//! started at window creation, every other saved tab is added as a *pending*
//! tab that starts its enumerator the first time it is shown. Sort, scroll row
//! and selected names are attached to each tab and applied when its listing
//! completes.

use crate::tab::{Restore, Tab};
use crate::view::View;
use std::time::Instant;
use tsubame_config::session::{self, Session, SessionPane, SessionTab};
use tsubame_config::{ConfigFile, Debounce, LoadStatus, Locations, SortColumn, ViewMode};

pub use tsubame_config::session::nearest_existing;

/// Sorting, mapped to the core enum and back.
pub fn sort_column(c: tsubame_core::Column) -> SortColumn {
    match c {
        tsubame_core::Column::Name => SortColumn::Name,
        tsubame_core::Column::Size => SortColumn::Size,
        tsubame_core::Column::Modified => SortColumn::Modified,
        tsubame_core::Column::Type => SortColumn::Type,
    }
}

pub fn core_column(c: SortColumn) -> tsubame_core::Column {
    match c {
        SortColumn::Name => tsubame_core::Column::Name,
        SortColumn::Size => tsubame_core::Column::Size,
        SortColumn::Modified => tsubame_core::Column::Modified,
        SortColumn::Type => tsubame_core::Column::Type,
    }
}

/// Selected entry names of a tab (not paths), capped so one save stays small.
fn selected_names(tab: &Tab) -> Vec<String> {
    let mut names: Vec<String> = tab.selected_paths(usize::MAX).into_iter().filter_map(|p| p.rsplit('\\').next().map(str::to_string)).collect();
    session::trim_selected(&mut names);
    names
}

/// The state of one tab, as saved.
pub fn tab_state(tab: &Tab, view_mode: ViewMode) -> SessionTab {
    // a tab restored from the session but never shown has not listed yet: its
    // scroll row and selection are still in `restore`, not in the list/selection
    let restore = tab.restore.as_ref().filter(|_| tab.pending);
    let row = restore.map(|r| r.scroll_first_row).unwrap_or_else(|| (tab.list.scroll / tab.list.row_h.max(1.0) as f64).round() as usize);
    let first = tab.shown().checked_sub(1).map(|last| row.min(last));
    let selected = match restore {
        Some(r) => r.selected.clone(),
        None => selected_names(tab),
    };
    SessionTab {
        path: tab.path.clone(),
        view_mode,
        sort_column: sort_column(tab.sort.column),
        sort_descending: tab.sort.descending,
        scroll_first_row: row,
        scroll_first_name: first.and_then(|i| tab.entry_at(i)).and_then(|(p, _)| p.rsplit('\\').next().map(str::to_string)),
        selected,
        expanded: tab.tree_paths(),
    }
}

/// What the app saves on exit.
pub fn capture(view: &View, window: tsubame_config::WindowState) -> Session {
    let mode = view.mode.to_config();
    let n = if view.dual() { 2 } else { 1 };
    let panes: Vec<SessionPane> = (0..n)
        .filter_map(|slot| view.pane_tabs(slot))
        .map(|tabs| {
            // the Settings tab is not a folder: it is not restored
            let active_tab = tabs.items[..tabs.active.min(tabs.items.len())].iter().filter(|t| !t.settings).count();
            let tabs_v: Vec<SessionTab> = tabs.items.iter().filter(|t| !t.settings).map(|t| tab_state(t, mode)).collect();
            SessionPane { active_tab: active_tab.min(tabs_v.len().saturating_sub(1)), tabs: tabs_v }
        })
        .collect();
    let last_folder = view.tabs.current().map(|t| t.path.clone()).unwrap_or_default();
    Session { version: session::SESSION_VERSION, last_folder, active_pane: view.active.min(n - 1), window, panes, recents: view.side.recents.clone() }
}

/// The shape of the window from the config (one pane or two, how they sit,
/// the split) and, with two panes, the other pane's saved tabs. Call after
/// `apply`: the pane the window started with keeps the slot it was saved in.
pub fn apply_layout(view: &mut View, cfg: &tsubame_config::Config, s: Option<&Session>) {
    view.ratio = crate::panes::clamp_ratio(cfg.panes.ratio);
    view.orient = cfg.panes.split;
    if !cfg.panes.dual {
        return;
    }
    let slot = s.map(|s| s.active_pane.min(1)).unwrap_or(0);
    view.live = slot;
    view.active = slot;
    view.set_layout(true, cfg.panes.split);
    if let Some(s) = s
        && s.panes.len() == 2
    {
        let other = &s.panes[1 - slot];
        let tabs: Vec<Tab> = other
            .tabs
            .iter()
            .map(|st| {
                let mut t = view.pending_tab(&tab_folder(&st.path));
                attach(&mut t, st);
                t
            })
            .collect();
        view.restore_other_pane(tabs, other.active_tab);
    }
    // starting up is not a change the user made
    view.config_dirty = false;
}

/// Restores `s` into a view that was created with the session's active folder.
///
/// `view` must hold exactly one tab, already listing the active folder. The
/// other tabs are added as pending (no enumerator) in their saved order and
/// the active tab ends up at `s.active_tab()`.
pub fn apply(view: &mut View, s: &Session) {
    view.side.recents = s.recents.iter().take(crate::sidebar::RECENT_CAP).cloned().collect();
    let tabs = s.tabs();
    if tabs.is_empty() {
        return;
    }
    let active = s.active_tab();
    let mut active_tab = view.tabs.close(0).expect("the view starts with one tab");
    let mut items = Vec::with_capacity(tabs.len());
    for (i, st) in tabs.iter().enumerate() {
        let mut t = if i == active {
            std::mem::replace(&mut active_tab, view.pending_tab(&st.path))
        } else {
            view.pending_tab(&st.path)
        };
        attach(&mut t, st);
        items.push(t);
    }
    view.tabs.items = items;
    view.tabs.active = active;
    view.after_restore();
}

/// Sort/scroll/selection of a saved tab, applied when the listing completes.
fn attach(t: &mut Tab, st: &SessionTab) {
    let column = core_column(st.sort_column);
    t.sort = tsubame_core::Sort { column, descending: st.sort_descending, folders_first: true };
    t.store.sort(t.sort);
    t.restore = Some(Restore { scroll_first_row: st.scroll_first_row, selected: st.selected.clone() });
    t.tree_seed.0 = st.expanded.iter().take(crate::tree::RESTORE_CAP).cloned().collect();
}

/// The folder a tab's listing should use: the saved path, or the nearest
/// existing parent when it is gone.
pub fn tab_folder(path: &str) -> String {
    nearest_existing(path).unwrap_or_else(|| path.to_string())
}

/// Copies what the user can change in the view into the config (theme,
/// density, zebra, view mode, sidebar). Called when `View::config_dirty` is set.
pub fn sync_config(view: &View, cfg: &mut tsubame_config::Config) {
    cfg.appearance.theme = view.theme_id.clone();
    cfg.appearance.accent = view.prefs.appearance.accent.clone();
    cfg.appearance.font_scale = view.prefs.appearance.font_scale;
    cfg.general = view.prefs.general.clone();
    cfg.updates = view.prefs.updates.clone();
    cfg.keys = view.prefs.keys.clone();
    cfg.context_menu = view.prefs.context_menu.clone();
    cfg.advanced = view.prefs.advanced.clone();
    cfg.file_age = view.prefs.file_age.clone();
    cfg.appearance.density = config_density(view.density);
    cfg.appearance.zebra = view.zebra;
    cfg.appearance.view_mode = view.mode.to_config();
    cfg.panes.dual = view.dual();
    cfg.panes.split = view.orient;
    cfg.panes.ratio = view.ratio;
    cfg.panes.sidebar_width = view.sidebar_width;
    cfg.panes.sidebar_collapsed = view.side.rail;
    cfg.preview = view.prefs.preview.clone().normalized();
    cfg.tags = view.prefs.tags.clone();
    cfg.sidebar.pinned = view.side.pinned.clone();
    cfg.sidebar.closed = view.side.closed_names();
}

pub fn config_density(d: tsubame_ui::Density) -> tsubame_config::Density {
    match d {
        tsubame_ui::Density::Compact => tsubame_config::Density::Compact,
        tsubame_ui::Density::Comfortable => tsubame_config::Density::Normal,
        tsubame_ui::Density::Spacious => tsubame_config::Density::Comfortable,
    }
}

/// Opens paths forwarded by a second launch (or given on this command line):
/// a folder that a tab already shows is focused, anything else becomes a new
/// tab. A file's entry is selected once its folder is listed.
pub fn open_targets(view: &mut View, targets: &[crate::cli::Target], _new_tab: bool) {
    for t in targets {
        let existing = view.tabs.items.iter().position(|tab| !tab.pending && tab.path.eq_ignore_ascii_case(&t.folder));
        let index = match existing {
            Some(i) => i,
            None => {
                let mut tab = view.pending_tab(&t.folder);
                if let Some(sel) = &t.select {
                    tab.restore = Some(Restore { scroll_first_row: 0, selected: vec![sel.clone()] });
                }
                view.tabs.items.push(tab);
                view.tabs.items.len() - 1
            }
        };
        view.switch_tab(index);
        if let Some(sel) = &t.select {
            view.tab_mut().select_names(std::slice::from_ref(sel));
        }
    }
}

/// The first folder to show at startup.
pub fn start_folder(cfg: &tsubame_config::Config, session: Option<&Session>, targets: &[crate::cli::Target]) -> String {
    if let Some(t) = targets.first() {
        return t.folder.clone();
    }
    match cfg.start_folder() {
        tsubame_config::StartFolder::Path(p) => tab_folder(&p),
        tsubame_config::StartFolder::Last => session.map(|s| s.last_folder.clone()).filter(|p| !p.is_empty()).map(|p| tab_folder(&p)).unwrap_or_else(tsubame_win::places::home_folder),
    }
}

/// The config and session files, their save gates, and the bytes last written
/// (so an unchanged state writes nothing).
pub struct Saver {
    pub loc: Locations,
    pub config: ConfigFile,
    pub session: Session,
    /// the config file belongs to a newer build: read it, never write it
    config_read_only: bool,
    /// same for the session file (an independently versioned file)
    session_read_only: bool,
    config_gate: Debounce,
    session_gate: Debounce,
    last_config: Option<Vec<u8>>,
    last_session: Option<Vec<u8>>,
    last_error: Option<String>,
}

impl Saver {
    pub fn new(loc: Locations, config: ConfigFile, session: Session, config_status: &LoadStatus, session_status: &LoadStatus) -> Saver {
        let last_config = Some(config.to_bytes());
        let last_session = Some(session.to_toml().into_bytes());
        Saver {
            loc,
            config,
            session,
            config_read_only: config_status.read_only(),
            session_read_only: session_status.read_only(),
            config_gate: Debounce::new(tsubame_config::SAVE_INTERVAL),
            session_gate: Debounce::new(tsubame_config::SAVE_INTERVAL),
            last_config,
            last_session,
            last_error: None,
        }
    }

    pub fn config_changed(&mut self) {
        if !self.config_read_only {
            self.config_gate.changed();
        }
    }

    pub fn session_changed(&mut self) {
        if !self.session_read_only {
            self.session_gate.changed();
        }
    }

    pub fn pending(&self) -> bool {
        (!self.config_read_only && self.config_gate.is_dirty()) || (!self.session_read_only && self.session_gate.is_dirty())
    }

    /// Milliseconds until the app should arm its save timer; 0 = write now.
    pub fn wait_ms(&self, now: Instant) -> Option<u32> {
        let c = if self.config_read_only { None } else { self.config_gate.wait_ms(now) };
        let s = if self.session_read_only { None } else { self.session_gate.wait_ms(now) };
        match (c, s) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(u32::MAX).min(b.unwrap_or(u32::MAX))),
        }
    }

    /// Writes whatever is due. `force` writes even inside the debounce window
    /// (exit, window close). Returns whether anything was written.
    pub fn flush(&mut self, now: Instant, force: bool) -> bool {
        let mut wrote = false;
        if !self.config_read_only && self.config_gate.is_dirty() && (force || self.config_gate.due(now)) {
            let bytes = self.config.to_bytes();
            if self.last_config.as_deref() != Some(bytes.as_slice()) {
                match tsubame_config::write_atomic(&self.loc.config, &bytes) {
                    Ok(()) => {
                        wrote = true;
                        self.last_config = Some(bytes);
                        self.last_error = None;
                    }
                    Err(e) => self.last_error = Some(format!("config: {e}")),
                }
            }
            self.config_gate.saved(now);
        }
        if !self.session_read_only && self.session_gate.is_dirty() && (force || self.session_gate.due(now)) {
            let bytes = self.session.to_toml().into_bytes();
            if self.last_session.as_deref() != Some(bytes.as_slice()) {
                match tsubame_config::write_atomic(&self.loc.session, &bytes) {
                    Ok(()) => {
                        wrote = true;
                        self.last_session = Some(bytes);
                        self.last_error = None;
                    }
                    Err(e) => self.last_error = Some(format!("session: {e}")),
                }
            }
            self.session_gate.saved(now);
        }
        wrote
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tsubame_ui::Density;
    use tsubame_win::enumerate::Wake;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("one")).unwrap();
        std::fs::create_dir_all(d.join("two")).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(d.join("one").join(name), b"x").unwrap();
        }
        std::fs::write(d.join("one").join("file with space.txt"), b"x").unwrap();
        std::fs::write(d.join("two").join("z.txt"), b"x").unwrap();
        d
    }

    fn settle(v: &mut View) {
        let t0 = Instant::now();
        while v.tabs.items.iter().any(|t| t.is_loading()) {
            v.pump();
            assert!(t0.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
        v.pump();
    }

    fn view(start: &str) -> View {
        let wake: Wake = Arc::new(|| {});
        View::new(start, wake, true, Density::Compact)
    }

    #[test]
    fn capture_and_restore_round_trip() {
        let d = temp_dir("round");
        let one = d.join("one").to_str().unwrap().to_string();
        let two = d.join("two").to_str().unwrap().to_string();
        let mut v = view(&one);
        settle(&mut v);
        // a second tab with its own sort, scroll and selection
        let idx = {
            let tab = v.pending_tab(&two);
            v.tabs.items.push(tab);
            v.tabs.items.len() - 1
        };
        v.switch_tab(idx);
        settle(&mut v);
        v.tab_mut().sort_by(tsubame_core::Column::Size);
        v.tab_mut().sort_by(tsubame_core::Column::Size);
        {
            let t = v.tab_mut();
            t.sel.single(&t.store, 0);
        }
        let selected = selected_names(v.tab());
        assert_eq!(selected.len(), 1);

        let s = capture(&v, tsubame_config::WindowState { x: 5, y: 6, w: 900, h: 700, maximized: true });
        assert_eq!(s.tabs().len(), 2);
        assert_eq!(s.tabs()[0].path, one);
        assert_eq!(s.tabs()[1].path, two);
        assert!(s.tabs()[1].sort_descending);
        assert_eq!(s.tabs()[1].sort_column, SortColumn::Size);
        assert_eq!(s.tabs()[1].selected, selected);
        assert!(s.window.maximized);

        // restore into a fresh view created with the active folder
        let mut v2 = view(&two);
        apply(&mut v2, &s);
        assert_eq!(v2.tabs.items.len(), 2);
        assert_eq!(v2.tabs.active, 1, "the active tab is restored");
        assert_eq!(v2.tab().path, two);
        assert!(v2.tabs.items[0].pending, "the other tab is lazy");
        assert!(!v2.tab().pending);
        settle(&mut v2);
        assert_eq!(v2.tab().sort.column, tsubame_core::Column::Size);
        assert!(v2.tab().sort.descending);
        assert_eq!(v2.tab().sel.count(), 1, "selection is restored by name");
        // switching to the lazy tab starts it
        v2.switch_tab(0);
        assert!(!v2.tab().pending);
        settle(&mut v2);
        assert_eq!(v2.tab().shown(), 4);
        assert_eq!(v2.tab().path, one);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_panes_round_trip() {
        let d = temp_dir("dual");
        let one = d.join("one").to_str().unwrap().to_string();
        let two = d.join("two").to_str().unwrap().to_string();
        let mut v = view(&one);
        settle(&mut v);
        v.set_layout(true, tsubame_config::Split::Horizontal);
        v.open_in_other_pane(&two);
        v.ratio = 0.35;
        settle(&mut v);
        let s = capture(&v, tsubame_config::WindowState::default());
        assert_eq!(s.panes.len(), 2);
        assert_eq!(s.active_pane, 0);
        assert_eq!(s.panes[0].tabs[0].path, one);
        assert_eq!(s.panes[1].tabs[0].path, two);
        let mut cfg = tsubame_config::Config::default();
        sync_config(&v, &mut cfg);
        assert!(cfg.panes.dual);
        assert_eq!(cfg.panes.split, tsubame_config::Split::Horizontal);
        assert!((cfg.panes.ratio - 0.35).abs() < 1e-6);

        // restore: the window starts on the active pane's folder
        let mut v2 = view(&one);
        apply(&mut v2, &s);
        apply_layout(&mut v2, &cfg, Some(&s));
        settle(&mut v2);
        assert!(v2.dual());
        assert_eq!(v2.orient, tsubame_config::Split::Horizontal);
        assert!((v2.ratio - 0.35).abs() < 1e-6);
        assert_eq!(v2.pane_tabs(0).unwrap().items[0].path, one);
        assert_eq!(v2.pane_tabs(1).unwrap().items[0].path, two);
        assert!(!v2.config_dirty, "starting up does not dirty the config");

        // the second pane was the active one: it keeps its slot
        v.set_active_pane(1);
        let s = capture(&v, tsubame_config::WindowState::default());
        assert_eq!(s.active_pane, 1);
        assert_eq!(s.last_folder, two);
        let mut v3 = view(&two);
        apply(&mut v3, &s);
        apply_layout(&mut v3, &cfg, Some(&s));
        settle(&mut v3);
        assert_eq!(v3.active, 1);
        assert_eq!(v3.pane_tabs(0).unwrap().items[0].path, one);
        assert_eq!(v3.pane_tabs(1).unwrap().items[0].path, two);
        assert_eq!(v3.tab().path, two);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restore_skips_missing_folders_and_keeps_the_tabs() {
        let d = temp_dir("missing");
        let one = d.join("one").to_str().unwrap().to_string();
        let gone = d.join("gone").join("deeper").to_str().unwrap().to_string();
        let mut s = Session { last_folder: gone.clone(), panes: vec![SessionPane { active_tab: 1, tabs: vec![SessionTab { path: one.clone(), ..Default::default() }, SessionTab { path: gone.clone(), ..Default::default() }] }], ..Session::default() };
        assert_eq!(s.repair_paths(), 1);
        assert_eq!(s.tabs()[1].path, d.to_str().unwrap(), "the nearest existing parent");
        // the view opens the repaired folder instead of failing
        let start = session::start_here(&s.tabs()[1]).unwrap().folder;
        let mut v = view(&start);
        apply(&mut v, &s);
        v.switch_tab(1);
        settle(&mut v);
        assert_eq!(v.tab().path, d.to_str().unwrap());
        assert!(v.tab().load == crate::tab::Load::Done);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_tab_whose_folder_vanished_between_save_and_restore_falls_back() {
        let d = temp_dir("vanish");
        let folder = d.join("temp").to_str().unwrap().to_string();
        std::fs::create_dir_all(&folder).unwrap();
        let mut s = Session { panes: vec![SessionPane { active_tab: 0, tabs: vec![SessionTab { path: folder.clone(), ..Default::default() }] }], ..Session::default() };
        // the folder is gone by the time the app starts
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(s.repair_paths(), 1);
        let start = tab_folder(&s.tabs()[0].path);
        assert_eq!(start, d.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_pending_tab_keeps_its_restore_state_in_the_session() {
        let d = temp_dir("pending-state");
        let one = d.join("one").to_str().unwrap().to_string();
        let two = d.join("two").to_str().unwrap().to_string();
        let mut v = view(&one);
        let mut tab = v.pending_tab(&two);
        tab.sort = tsubame_core::Sort { column: tsubame_core::Column::Modified, descending: true, folders_first: true };
        tab.restore = Some(Restore { scroll_first_row: 7, selected: vec!["z.txt".into()] });
        v.tabs.items.push(tab);
        let s = capture(&v, tsubame_config::WindowState::default());
        assert_eq!(s.tabs().len(), 2);
        let saved = &s.tabs()[1];
        assert_eq!(saved.path, two);
        assert_eq!(saved.sort_column, SortColumn::Modified);
        assert!(saved.sort_descending);
        assert_eq!(saved.scroll_first_row, 7);
        assert_eq!(saved.selected, ["z.txt"], "a never-shown tab still saves its selection");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn saver_writes_nothing_when_nothing_changed() {
        let d = temp_dir("saver");
        let loc = Locations::in_dir(&d);
        let cfg = ConfigFile::new(tsubame_config::Config::default());
        let mut saver = Saver::new(loc.clone(), cfg, Session::default(), &LoadStatus::Missing, &LoadStatus::Missing);
        let t0 = Instant::now();
        assert!(!saver.flush(t0, true));
        assert!(!loc.config.exists(), "the unchanged defaults were not written");
        assert!(!loc.session.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn saver_debounces_and_flushes_on_exit() {
        let d = temp_dir("debounce");
        let loc = Locations::in_dir(&d);
        let cfg = ConfigFile::new(tsubame_config::Config::default());
        let mut saver = Saver::new(loc.clone(), cfg, Session::default(), &LoadStatus::Missing, &LoadStatus::Missing);
        let t0 = Instant::now();
        saver.config.config.appearance.density = tsubame_config::Density::Comfortable;
        saver.config_changed();
        assert!(saver.pending(), "the first change writes at once");
        assert!(saver.flush(t0, false));
        assert!(loc.config.exists());
        let first = std::fs::read(&loc.config).unwrap();
        // a second change inside the window is not written until it is due
        saver.config.config.appearance.density = tsubame_config::Density::Normal;
        saver.config_changed();
        assert!(!saver.flush(t0 + Duration::from_millis(100), false));
        assert_eq!(std::fs::read(&loc.config).unwrap(), first);
        assert_eq!(saver.wait_ms(t0 + Duration::from_millis(100)), Some(1900));
        assert!(saver.flush(t0 + Duration::from_secs(2), false));
        assert_ne!(std::fs::read(&loc.config).unwrap(), first);
        // the last change inside the window is written on exit regardless
        saver.config.config.appearance.density = tsubame_config::Density::Compact;
        saver.config_changed();
        assert!(!saver.flush(t0 + Duration::from_millis(2100), false));
        assert!(saver.flush(t0 + Duration::from_millis(2200), true), "forced on exit");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn saver_never_writes_a_newer_schema() {
        let d = temp_dir("readonly");
        let loc = Locations::in_dir(&d);
        let cfg = ConfigFile::new(tsubame_config::Config::default());
        let mut saver = Saver::new(loc.clone(), cfg, Session::default(), &LoadStatus::Newer { version: 9 }, &LoadStatus::Loaded);
        saver.config.config.appearance.zebra = false;
        saver.config_changed();
        assert!(!saver.pending(), "a newer config schema never writes");
        assert!(!saver.flush(Instant::now(), true));
        assert!(!loc.config.exists(), "a newer config is never overwritten");
        // the session is a different file and is still written
        saver.session.last_folder = "C:\\x".into();
        saver.session_changed();
        assert!(saver.pending());
        assert!(saver.flush(Instant::now(), true));
        assert!(loc.session.exists());
        assert!(!loc.config.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn session_file_round_trips_through_the_saver() {
        let d = temp_dir("file");
        let loc = Locations::in_dir(&d);
        let one = d.join("one").to_str().unwrap().to_string();
        let mut v = view(&one);
        settle(&mut v);
        let s = capture(&v, tsubame_config::WindowState::default());
        let mut saver = Saver::new(loc.clone(), ConfigFile::new(tsubame_config::Config::default()), Session::default(), &LoadStatus::Missing, &LoadStatus::Missing);
        saver.session = s.clone();
        saver.session_changed();
        assert!(saver.flush(Instant::now(), true));
        let l = Session::load(&loc.session);
        assert_eq!(l.status, LoadStatus::Loaded);
        assert_eq!(l.value.tabs()[0].path, one);
        let _ = std::fs::remove_dir_all(&d);
    }
}
