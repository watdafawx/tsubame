//! B11-G per-folder view memory (Explorer parity): each folder keeps its own view mode, icon
//! size and sort, and a folder with no memory shows the global default.
//!
//! - The map lives in memory (`FolderViews`: one hash lookup per navigation, 2,000 folders at
//!   most, the least recently used one goes first) and in `folder-views.tsv` next to the
//!   config. The file is read once at startup on a worker and written lazily: a change marks it
//!   dirty and rides the config's 2 s save gate (`View::flush_folder_views`, called when that
//!   gate fires or on exit), and the bytes go to disk on a worker thread. Nothing runs on a
//!   timer of its own, so an idle window costs nothing.
//! - Tree and Miller views, archive and virtual folders and the Settings tab are never stored,
//!   and while a Tree or Miller view is showing navigation does not switch views.
//! - Commands: `view.reset-folder-views` forgets every folder; `view.apply-to-all-folders` makes
//!   the current view the default and forgets the folders (like Explorer's "Apply to Folders").

use super::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Folders remembered (the least recently used one is dropped past this).
pub const CAP: usize = 2000;
/// The file next to `config.toml`.
pub const FILE_NAME: &str = "folder-views.tsv";
const HEADER: &str = "# tsubame folder views 1";

/// What a folder remembers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderView {
    pub mode: tsubame_config::ViewMode,
    /// grid cell width in DIPs (kept even in Details, so a later grid switch has it)
    pub grid: u32,
    pub column: tsubame_core::Column,
    pub descending: bool,
}

impl FolderView {
    /// The view of `mode`, grid size and sort.
    pub fn of(mode: ViewMode, grid: u32, sort: tsubame_core::Sort) -> FolderView {
        FolderView { mode: mode.to_config(), grid: tsubame_config::ViewConfig::snap_grid(grid as f32), column: sort.column, descending: sort.descending }
    }

    fn line(&self, path: &str) -> String {
        format!("{}\t{}\t{}\t{}\t{}\n", self.mode.as_str(), self.grid, crate::session::sort_column(self.column).as_str(), u8::from(self.descending), path)
    }

    /// `mode \t grid \t sort \t desc` (the fields before the path).
    fn parse(f: &[&str]) -> Option<FolderView> {
        if f.len() < 4 {
            return None;
        }
        let mode = tsubame_config::ViewMode::parse(f[0]);
        // Tree and Miller are never stored: a hand-edited file naming them reads as Details
        let mode = if matches!(mode, tsubame_config::ViewMode::Tree | tsubame_config::ViewMode::Miller) { tsubame_config::ViewMode::Details } else { mode };
        let grid = tsubame_config::ViewConfig::snap_grid(f[1].parse::<f32>().ok()?);
        let column = crate::session::core_column(tsubame_config::SortColumn::parse(f[2]));
        Some(FolderView { mode, grid, column, descending: f[3] == "1" })
    }
}

/// The key of a folder: lower case, one backslash between names, none at the end (a drive root
/// keeps its `c:\`). `None` for what is never remembered: no path, archives and other virtual
/// folders, anything that is not a drive or UNC path.
pub fn key_of(path: &str) -> Option<String> {
    let p = path.trim();
    if p.is_empty() || crate::archive_ui::is_archive_path(p) {
        return None;
    }
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    let mut out = String::with_capacity(p.len());
    let mut last_sep = false;
    for c in p.chars() {
        let c = if c == '/' { '\\' } else { c };
        if c == '\\' {
            // a UNC path keeps its two leading separators
            if last_sep && out.len() > 1 {
                continue;
            }
            last_sep = true;
        } else {
            last_sep = false;
        }
        out.extend(c.to_lowercase());
    }
    let b = out.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let unc = out.starts_with(r"\\") && out.len() > 2;
    if !drive && !unc {
        return None;
    }
    // "c:" is the drive's current folder, not its root: `c:\`
    if out.len() == 2 {
        out.push('\\');
    }
    while out.len() > 3 && out.ends_with('\\') {
        out.pop();
    }
    if out.contains(['\t', '\n', '\r']) || out.contains("::") {
        return None;
    }
    Some(out)
}

/// What the worker read: the default sort and the entries, oldest first.
struct Parsed {
    default_sort: Option<(tsubame_core::Column, bool)>,
    entries: Vec<(String, FolderView)>,
}

/// Reads the file text (a bad line is skipped, never an error).
fn parse_text(text: &str) -> Parsed {
    let mut out = Parsed { default_sort: None, entries: Vec::new() };
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f[0] == "default" {
            if f.len() >= 3 {
                out.default_sort = Some((crate::session::core_column(tsubame_config::SortColumn::parse(f[1])), f[2] == "1"));
            }
            continue;
        }
        let Some(v) = FolderView::parse(&f) else { continue };
        let Some(path) = f.get(4).and_then(|p| key_of(p)) else { continue };
        out.entries.push((path, v));
    }
    out
}

/// The writer's ordering: the newest snapshot wins, however the threads are scheduled.
/// Kept per file (one entry in the app; tests write to several files).
static WRITTEN: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());

fn write_file(path: &std::path::Path, seq: u64, bytes: &[u8]) {
    let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    let at = written.iter().position(|(p, _)| p == path);
    if at.is_some_and(|i| seq < written[i].1) {
        return;
    }
    if tsubame_config::write_atomic(path, bytes).is_ok() {
        match at {
            Some(i) => written[i].1 = seq,
            None => written.push((path.to_path_buf(), seq)),
        }
    }
}

type LoadSlot = Arc<Mutex<Option<Parsed>>>;

/// The remembered views of every folder, and the global default.
pub struct FolderViews {
    map: HashMap<String, (FolderView, u64)>,
    tick: u64,
    /// shown for a folder with no memory; "Use this view for all folders" sets it
    pub default: FolderView,
    /// the app attached a file (tests and `--dev` runs have none and remember nothing)
    attached: bool,
    /// the file has been read (nothing applies before: the default shows)
    pub(super) loaded: bool,
    dirty: bool,
    file: Option<PathBuf>,
    load: Option<LoadSlot>,
    seq: u64,
}

impl Default for FolderViews {
    fn default() -> Self {
        FolderViews {
            map: HashMap::new(),
            tick: 0,
            default: FolderView::of(ViewMode::Details, tsubame_config::DEFAULT_GRID_SIZE, tsubame_core::Sort { column: tsubame_core::Column::Name, descending: false, folders_first: true }),
            attached: false,
            loaded: false,
            dirty: false,
            file: None,
            load: None,
            seq: 0,
        }
    }
}

impl FolderViews {
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    #[cfg(test)]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The remembered view of `key`, now the most recently used.
    pub fn get(&mut self, key: &str) -> Option<FolderView> {
        self.tick += 1;
        let t = self.tick;
        let e = self.map.get_mut(key)?;
        e.1 = t;
        Some(e.0)
    }

    /// Remembers `v` for `key`. True when that changed what is stored (and the file is now due).
    /// A folder whose view equals the default and has no entry stays unlisted.
    pub fn set(&mut self, key: &str, v: FolderView) -> bool {
        self.tick += 1;
        let t = self.tick;
        if let Some(e) = self.map.get_mut(key) {
            e.1 = t;
            if e.0 == v {
                return false;
            }
            e.0 = v;
            self.dirty = true;
            return true;
        }
        if v == self.default {
            return false;
        }
        if self.map.len() >= CAP {
            self.evict();
        }
        self.map.insert(key.to_string(), (v, t));
        self.dirty = true;
        true
    }

    /// Drops the least recently used entry. O(n), only when a new folder arrives at the cap.
    fn evict(&mut self) {
        if let Some(k) = self.map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| k.clone()) {
            self.map.remove(&k);
        }
    }

    /// Forgets every folder.
    pub fn clear(&mut self) {
        if !self.map.is_empty() {
            self.dirty = true;
        }
        self.map.clear();
    }

    /// The file's text: entries oldest first, so the order is the LRU order next time.
    pub fn to_text(&self) -> String {
        let mut rows: Vec<(&String, &(FolderView, u64))> = self.map.iter().collect();
        rows.sort_by_key(|(_, (_, t))| *t);
        let mut s = String::with_capacity(64 + rows.len() * 80);
        s.push_str(HEADER);
        s.push('\n');
        s.push_str(&format!("default\t{}\t{}\n", crate::session::sort_column(self.default.column).as_str(), u8::from(self.default.descending)));
        for (k, (v, _)) in rows {
            s.push_str(&v.line(k));
        }
        s
    }

    /// Merges what the worker read: what changed in memory while it ran wins.
    fn merge(&mut self, p: Parsed) {
        if let Some((c, d)) = p.default_sort {
            self.default.column = c;
            self.default.descending = d;
        }
        let n = p.entries.len() as u64;
        for (_, t) in self.map.values_mut() {
            *t += n;
        }
        self.tick += n;
        for (i, (k, v)) in p.entries.into_iter().enumerate() {
            if self.map.len() >= CAP && !self.map.contains_key(&k) {
                break;
            }
            self.map.entry(k).or_insert((v, i as u64 + 1));
        }
        self.loaded = true;
    }

    /// Parses `text` into the map (used by the loader and the tests).
    #[cfg(test)]
    pub fn load_text(&mut self, text: &str) {
        self.merge(parse_text(text));
    }
}

impl View {
    /// Startup: the folder-views file of this copy. Reads it on a worker; until it arrives every
    /// folder shows the default. `wake` repaints when it is in.
    pub fn attach_folder_views(&mut self, file: PathBuf) {
        let fv = &mut self.folder_views;
        fv.attached = true;
        fv.default = FolderView::of(self.mode, self.prefs.view.grid_size, tsubame_core::Sort { column: tsubame_core::Column::Name, descending: false, folders_first: true });
        fv.file = Some(file.clone());
        let slot: LoadSlot = Arc::new(Mutex::new(None));
        fv.load = Some(slot.clone());
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new().name("tsubame-folder-views".into()).spawn(move || {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let parsed = parse_text(&text);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(parsed);
            wake();
        });
        if spawned.is_err() {
            self.folder_views.loaded = true;
            self.folder_views.load = None;
        }
    }

    /// Memory on: the setting is on and the app gave it a file.
    pub fn folder_memory_on(&self) -> bool {
        self.prefs.general.remember_folder_views && self.folder_views.attached
    }

    /// A tab whose folder is remembered: a real folder, not Settings, an archive or a synthetic
    /// listing.
    pub(super) fn memory_key(&self) -> Option<String> {
        let t = self.tab();
        if t.settings || t.synthetic || t.pending {
            return None;
        }
        key_of(&t.path)
    }

    /// Tree and Miller views are not stored, and navigating in them keeps them.
    pub(super) fn view_is_remembered(&self) -> bool {
        !matches!(self.mode, ViewMode::Tree | ViewMode::Miller) && !self.tab().tree_on && !self.tab().miller_on
    }

    /// The view the active tab's folder should show: its memory, else the default.
    pub fn folder_view_target(&mut self) -> Option<FolderView> {
        if !self.folder_memory_on() || !self.folder_views.loaded || !self.view_is_remembered() {
            return None;
        }
        let key = self.memory_key()?;
        Some(self.folder_views.get(&key).unwrap_or(self.folder_views.default))
    }

    /// A folder was entered, a tab came to the front or the file arrived: show what the folder
    /// remembers (or the default). A view that already matches costs two compares.
    pub(super) fn apply_folder_view(&mut self) {
        let Some(v) = self.folder_view_target() else { return };
        self.folder_applying = true;
        // showing a remembered view is not a setting the user changed
        let was_dirty = self.config_dirty;
        let mode = ViewMode::from_config(v.mode);
        if mode != self.mode {
            self.set_mode(mode);
        }
        if self.prefs.view.grid_size != v.grid {
            self.prefs.view.grid_size = v.grid;
        }
        let t = self.tab_mut();
        if t.sort.column != v.column || t.sort.descending != v.descending {
            t.sort = tsubame_core::Sort { column: v.column, descending: v.descending, folders_first: t.sort.folders_first };
            t.store.sort(t.sort);
            t.tree_dirty = true;
        }
        self.config_dirty = was_dirty;
        self.folder_applying = false;
    }

    /// The user changed the view mode, icon size or sort here: this folder (and no other)
    /// remembers it.
    pub(super) fn remember_folder_view(&mut self) {
        if self.folder_applying || !self.folder_memory_on() || !self.view_is_remembered() {
            return;
        }
        let Some(key) = self.memory_key() else { return };
        let v = FolderView::of(self.mode, self.prefs.view.grid_size, self.tab().sort);
        if self.folder_views.set(&key, v) {
            // the config's save gate also times the file
            self.config_dirty = true;
        }
    }

    /// Takes the file when the worker has read it, then applies it to the folder on screen.
    pub(super) fn pump_folder_views(&mut self) -> bool {
        let Some(slot) = self.folder_views.load.as_ref() else { return false };
        let Some(p) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else { return false };
        self.folder_views.load = None;
        self.folder_views.merge(p);
        self.apply_folder_view();
        true
    }

    /// Writes the file when it changed: on a worker, or here when `force` (exit).
    pub fn flush_folder_views(&mut self, force: bool) {
        let fv = &mut self.folder_views;
        if !fv.dirty || !fv.loaded {
            return;
        }
        let Some(file) = fv.file.clone() else { return };
        fv.dirty = false;
        fv.seq += 1;
        let (seq, bytes) = (fv.seq, fv.to_text().into_bytes());
        if force {
            write_file(&file, seq, &bytes);
        } else {
            let _ = std::thread::Builder::new().name("tsubame-folder-views-save".into()).spawn(move || write_file(&file, seq, &bytes));
        }
    }

    /// The global default view as the config file keeps it (so a restart starts from it, not from
    /// whatever the last folder showed).
    pub fn default_view_for_config(&self) -> (tsubame_config::ViewMode, u32) {
        if self.folder_memory_on() { (self.folder_views.default.mode, self.folder_views.default.grid) } else { (self.mode.to_config(), self.prefs.view.grid_size) }
    }

    pub(super) fn run_folder_views(&mut self, ui: &mut Ui, cmd: u32) -> bool {
        match cmd {
            CMD_RESET_FOLDER_VIEWS => {
                self.folder_views.clear();
                self.config_dirty = true;
                self.ops.message = Some("All folders forget their view".into());
            }
            CMD_APPLY_VIEW_ALL => {
                if !self.folder_memory_on() || !self.view_is_remembered() {
                    self.ops.message = Some("Switch to Details or a thumbnail view to use it for all folders".into());
                } else {
                    let v = FolderView::of(self.mode, self.prefs.view.grid_size, self.tab().sort);
                    self.folder_views.default = v;
                    self.folder_views.clear();
                    // the default is part of the file
                    self.folder_views.dirty = true;
                    self.config_dirty = true;
                    self.ops.message = Some("This view is now used for all folders".into());
                }
            }
            _ => return false,
        }
        ui.request_repaint();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fv(mode: tsubame_config::ViewMode, grid: u32, column: tsubame_core::Column, descending: bool) -> FolderView {
        FolderView { mode, grid, column, descending }
    }
    fn grid_view() -> FolderView {
        fv(tsubame_config::ViewMode::GridLarge, 152, tsubame_core::Column::Modified, true)
    }

    #[test]
    fn keys_ignore_case_slashes_and_the_trailing_separator() {
        assert_eq!(key_of(r"C:\Users\Me\Docs").as_deref(), Some(r"c:\users\me\docs"));
        assert_eq!(key_of(r"c:\USERS\me\docs\").as_deref(), Some(r"c:\users\me\docs"));
        assert_eq!(key_of("C:/Users//Me/Docs/").as_deref(), Some(r"c:\users\me\docs"));
        assert_eq!(key_of(r"\\?\C:\Users").as_deref(), Some(r"c:\users"));
        assert_eq!(key_of(r"C:\").as_deref(), Some(r"c:\"), "a drive root keeps its separator");
        assert_eq!(key_of("C:").as_deref(), Some(r"c:\"));
        assert_eq!(key_of(r"\\NAS\Share\Pics\").as_deref(), Some(r"\\nas\share\pics"), "UNC keeps both leading separators");
        assert_eq!(key_of(r"C:\Ünï\Ä").as_deref(), Some(r"c:\ünï\ä"));
    }

    #[test]
    fn archives_virtual_and_empty_paths_have_no_key() {
        assert_eq!(key_of(""), None);
        assert_eq!(key_of("   "), None);
        assert_eq!(key_of("This PC"), None);
        assert_eq!(key_of("shell:RecycleBinFolder"), None);
        assert_eq!(key_of("relative\\path"), None);
        assert_eq!(key_of("C:\\a\tb"), None, "a tab would break the file");
        assert_eq!(key_of(r"C:\Temp\x.zip::\inner"), None, "inside an archive");
    }

    #[test]
    fn set_and_get_remember_per_folder_and_leave_the_default_unlisted() {
        let mut f = FolderViews::default();
        assert!(f.get(r"c:\a").is_none());
        // the default view of a folder with no entry is not worth an entry
        assert!(!f.set(r"c:\a", f.default));
        assert!(f.is_empty() && !f.is_dirty());
        assert!(f.set(r"c:\a", grid_view()));
        assert!(f.is_dirty());
        assert_eq!(f.get(r"c:\a"), Some(grid_view()));
        assert!(f.get(r"c:\b").is_none(), "another folder keeps the default");
        // the same value again changes nothing
        f.dirty = false;
        assert!(!f.set(r"c:\a", grid_view()));
        assert!(!f.is_dirty());
        // going back to the default is a change of an existing entry
        let d = f.default;
        assert!(f.set(r"c:\a", d));
        assert_eq!(f.get(r"c:\a"), Some(d));
    }

    #[test]
    fn the_least_recently_used_folder_goes_first_at_the_cap() {
        let mut f = FolderViews::default();
        for i in 0..CAP {
            assert!(f.set(&format!(r"c:\f{i}"), grid_view()));
        }
        assert_eq!(f.len(), CAP);
        // use the oldest one: it is now the newest
        assert!(f.get(r"c:\f0").is_some());
        assert!(f.set(r"c:\new", grid_view()));
        assert_eq!(f.len(), CAP, "never past the cap");
        assert!(f.get(r"c:\f0").is_some(), "a used folder stays");
        assert!(f.get(r"c:\f1").is_none(), "the least recently used folder is dropped");
        assert!(f.get(r"c:\new").is_some());
        // a change to an existing entry does not evict
        let mut other = grid_view();
        other.grid = 200;
        assert!(f.set(r"c:\f5", other));
        assert_eq!(f.len(), CAP);
    }

    #[test]
    fn the_file_round_trips_and_keeps_the_use_order() {
        let mut f = FolderViews::default();
        f.default.column = tsubame_core::Column::Size;
        f.default.descending = true;
        assert!(f.set(r"c:\one", grid_view()));
        assert!(f.set(r"c:\two", fv(tsubame_config::ViewMode::Details, 96, tsubame_core::Column::Type, false)));
        assert!(f.set(r"c:\three", fv(tsubame_config::ViewMode::GridSmall, 64, tsubame_core::Column::Size, true)));
        f.get(r"c:\one"); // one is the newest
        let text = f.to_text();
        assert!(text.starts_with(HEADER));
        let mut g = FolderViews::default();
        g.load_text(&text);
        assert!(g.loaded);
        assert_eq!(g.default.column, tsubame_core::Column::Size);
        assert!(g.default.descending);
        assert_eq!(g.len(), 3);
        for k in [r"c:\one", r"c:\two", r"c:\three"] {
            assert_eq!(g.get(k), f.get(k), "{k}");
        }
        // the use order survived: fill to the cap and the oldest (two) goes first
        let mut h = FolderViews::default();
        h.load_text(&text);
        for i in 0..CAP - 3 {
            h.set(&format!(r"c:\x{i}"), grid_view());
        }
        h.set(r"c:\extra", grid_view());
        assert!(h.get(r"c:\two").is_none() && h.get(r"c:\one").is_some() && h.get(r"c:\three").is_some());
    }

    #[test]
    fn a_damaged_file_loses_only_the_bad_lines() {
        let text = "# tsubame folder views 1\nnot a line\n\ngrid-large\t152\tmodified\t1\tC:\\Good\\Dir\\\ngrid-large\tx\tmodified\t1\tc:\\badgrid\ntree\t96\tname\t0\tc:\\tree\nmiller\t96\tname\t0\tc:\\miller\ndetails\t96\tname\t0\n";
        let mut f = FolderViews::default();
        f.load_text(text);
        assert_eq!(f.get(r"c:\good\dir"), Some(grid_view()));
        assert!(f.get(r"c:\badgrid").is_none());
        // Tree and Miller are never kept: they read as Details
        assert_eq!(f.get(r"c:\tree").map(|v| v.mode), Some(tsubame_config::ViewMode::Details));
        assert_eq!(f.get(r"c:\miller").map(|v| v.mode), Some(tsubame_config::ViewMode::Details));
        assert_eq!(f.len(), 3);
        let mut empty = FolderViews::default();
        empty.load_text("");
        assert!(empty.loaded && empty.is_empty());
    }

    #[test]
    fn merging_the_file_keeps_what_changed_while_it_loaded() {
        let mut f = FolderViews::default();
        f.set(r"c:\live", grid_view());
        let mut disk = FolderViews::default();
        disk.set(r"c:\live", fv(tsubame_config::ViewMode::Details, 96, tsubame_core::Column::Size, false));
        disk.set(r"c:\old", grid_view());
        f.load_text(&disk.to_text());
        assert_eq!(f.get(r"c:\live"), Some(grid_view()), "the newer in-memory view wins");
        assert_eq!(f.get(r"c:\old"), Some(grid_view()));
    }

    #[test]
    fn a_grid_size_off_the_step_is_snapped_and_clear_forgets_everything() {
        let mut f = FolderViews::default();
        f.load_text("grid-small\t101\tname\t0\tc:\\snap\n");
        assert_eq!(f.get(r"c:\snap").map(|v| v.grid), Some(tsubame_config::ViewConfig::snap_grid(101.0)));
        f.dirty = false;
        f.clear();
        assert!(f.is_empty() && f.is_dirty());
        f.dirty = false;
        f.clear();
        assert!(!f.is_dirty(), "clearing nothing writes nothing");
    }

    #[test]
    fn the_file_is_written_and_read_back() {
        let d = std::env::temp_dir().join(format!("tsubame-fv-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let file = d.join(FILE_NAME);
        let mut f = FolderViews::default();
        f.set(r"c:\disk", grid_view());
        write_file(&file, 1_000_000, f.to_text().as_bytes());
        // an older snapshot never replaces a newer one
        write_file(&file, 999_999, b"stale");
        let text = std::fs::read_to_string(&file).unwrap();
        let mut g = FolderViews::default();
        g.load_text(&text);
        assert_eq!(g.get(r"c:\disk"), Some(grid_view()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
