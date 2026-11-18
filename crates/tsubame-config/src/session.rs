//! The session file: open tabs per pane, each tab's view state, and the window
//! geometry. Restored only when the config says so and the command line named
//! no path.
//!
//! The app restores lazily: only the active tab's folder is listed at startup;
//! every other tab keeps its saved path, sort and scroll and starts its
//! enumerator the first time it is shown. That keeps the first paint under
//! budget no matter how many tabs were open.
//!
//! Paths that no longer exist fall back to the nearest existing parent. Network
//! paths are never checked: touching a dead share on the UI thread would blow
//! the startup budget, so they are opened as saved and the enumerator reports
//! the failure.

use crate::atomic::write_atomic;
use crate::locate::Locations;
use crate::{ViewMode, WindowState};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

/// Schema version of `session.toml`.
pub const SESSION_VERSION: u32 = 1;

/// One saved tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionTab {
    pub path: String,
    pub view_mode: ViewMode,
    pub sort_column: crate::SortColumn,
    pub sort_descending: bool,
    /// Index of the first visible row.
    pub scroll_first_row: usize,
    /// Name of the entry that was first visible. Used when the folder changed
    /// and the index no longer means the same thing; optional.
    pub scroll_first_name: Option<String>,
    /// Selected entry names (not full paths), optional.
    pub selected: Vec<String>,
}

/// One pane's tabs. Today the app has a single pane; the schema already has
/// room for the dual-pane task.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionPane {
    pub active_tab: usize,
    pub tabs: Vec<SessionTab>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    #[serde(default = "crate::default_version")]
    pub version: u32,
    /// The folder the next plain start opens when the start folder is "last".
    pub last_folder: String,
    pub active_pane: usize,
    pub window: WindowState,
    pub panes: Vec<SessionPane>,
}

impl Default for Session {
    fn default() -> Self {
        Session { version: SESSION_VERSION, last_folder: String::new(), active_pane: 0, window: WindowState::default(), panes: Vec::new() }
    }
}

impl Session {
    /// The tabs of the pane the app shows (pane 0 when there is nothing).
    pub fn pane(&self) -> Option<&SessionPane> {
        self.panes.get(self.active_pane).or_else(|| self.panes.first())
    }
    pub fn tabs(&self) -> &[SessionTab] {
        self.pane().map(|p| p.tabs.as_slice()).unwrap_or(&[])
    }
    /// Index of the tab to start from, clamped into the saved tabs.
    pub fn active_tab(&self) -> usize {
        let n = self.tabs().len();
        if n == 0 { 0 } else { self.pane().map(|p| p.active_tab).unwrap_or(0).min(n - 1) }
    }
    /// Paths that no longer exist move up to their nearest existing parent.
    /// Returns the number of paths that changed. Never touches network paths.
    pub fn repair_paths(&mut self) -> usize {
        let mut changed = 0;
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                if let Some(p) = nearest_existing(&tab.path)
                    && p != tab.path
                {
                    tab.path = p;
                    changed += 1;
                }
            }
        }
        changed
    }
    pub fn load(path: &Path) -> crate::Loaded<Session> {
        crate::load_toml(path)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, self.to_toml().as_bytes())
    }
    pub fn to_toml(&self) -> String {
        crate::to_toml_string(self).unwrap_or_default()
    }
    pub fn save_to(&self, loc: &Locations) -> io::Result<()> {
        self.save(&loc.session)
    }
    /// A session restored from this file may only be used if it has tabs.
    pub fn is_usable(&self) -> bool {
        !self.tabs().is_empty()
    }
}

const MAX_SELECTED: usize = 500;

/// Keeps a stored session bounded: tabs and names per tab.
pub fn trim_selected(names: &mut Vec<String>) {
    names.truncate(MAX_SELECTED);
}

/// The folder above `path`, using the app's path conventions: drive roots
/// (`C:\`), UNC shares (`\\server\share\`) and `\\?\` paths.
pub fn parent(path: &str) -> Option<String> {
    let p = path.trim_end_matches('\\');
    // keep a root intact
    if p.is_empty() {
        return None;
    }
    if let Some(rest) = p.strip_prefix("\\\\?\\UNC\\") {
        // \\?\UNC\server\share[\...]
        let parts: Vec<&str> = rest.split('\\').collect();
        if parts.len() <= 2 {
            return None;
        }
        return Some(format!("\\\\?\\UNC\\{}", parts[..parts.len() - 1].join("\\")));
    }
    if let Some(rest) = p.strip_prefix("\\\\?\\") {
        // \\?\C:\a
        let b = rest.as_bytes();
        if b.len() <= 3 {
            return None;
        }
        return Some(format!("\\\\?\\{}", &rest[..rest.rfind('\\')?]));
    }
    if p.len() == 2 && p.as_bytes()[1] == b':' {
        return None;
    }
    if let Some(rest) = p.strip_prefix("\\\\") {
        // \\server\share[\...]
        let parts: Vec<&str> = rest.split('\\').collect();
        if parts.len() <= 2 {
            return None;
        }
        return Some(format!("\\\\{}", parts[..parts.len() - 1].join("\\")));
    }
    let b = p.as_bytes();
    if b.len() <= 3 && b.len() >= 2 && b[1] == b':' {
        return None;
    }
    p.rfind('\\').map(|i| {
        let head = &p[..i];
        if head.len() == 2 && head.as_bytes()[1] == b':' { format!("{head}\\") } else { head.to_string() }
    })
}

/// `path` if it exists, else its nearest existing parent (`C:\` at worst).
/// `None` for something that is not an absolute path or has no existing
/// ancestor. Network paths are returned unchanged and never checked.
pub fn nearest_existing(path: &str) -> Option<String> {
    let p = path.trim();
    if p.is_empty() {
        return None;
    }
    let lower_start = p.to_ascii_lowercase();
    let network = lower_start.starts_with("\\\\") && !lower_start.starts_with("\\\\?\\") || lower_start.starts_with("\\\\?\\unc\\");
    if network {
        return Some(p.to_string());
    }
    // not a path this function can reason about
    if !is_absolute(p) {
        return None;
    }
    let mut cur = p.trim_end_matches('\\').to_string();
    loop {
        if !cur.is_empty() && Path::new(&cur).exists() {
            return Some(cur);
        }
        cur = parent(&cur)?;
    }
}

fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':') || p.starts_with("\\\\")
}

/// What the app does with a saved tab, kept here so the rule is testable
/// without the app: the folder to list and an optional entry to select.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartHere {
    pub folder: String,
    pub select: Option<String>,
}

/// The folder for a saved tab that may be gone, plus the entry to select.
pub fn start_here(tab: &SessionTab) -> Option<StartHere> {
    let folder = nearest_existing(&tab.path)?;
    Some(StartHere { folder, select: tab.selected.first().cloned() })
}

/// Removes state that would not survive a restart: a scroll row past the end
/// of the folder is clamped to the last row, unknown selections are dropped by
/// the app while applying.
pub fn clamp_scroll(first_row: usize, entries: usize) -> usize {
    if entries == 0 { 0 } else { first_row.min(entries - 1) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-config-session-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn tab(path: &str) -> SessionTab {
        SessionTab { path: path.to_string(), ..SessionTab::default() }
    }

    fn session(paths: &[&str], active: usize) -> Session {
        Session {
            version: SESSION_VERSION,
            last_folder: paths[active].to_string(),
            active_pane: 0,
            window: WindowState { x: 10, y: 20, w: 1000, h: 700, maximized: false },
            panes: vec![SessionPane { active_tab: active, tabs: paths.iter().map(|p| tab(p)).collect() }],
        }
    }

    #[test]
    fn round_trip_through_toml() {
        let mut s = session(&["C:\\Users\\me", "D:\\work"], 1);
        s.panes[0].tabs[0].sort_column = crate::SortColumn::Size;
        s.panes[0].tabs[0].sort_descending = true;
        s.panes[0].tabs[0].scroll_first_row = 42;
        s.panes[0].tabs[0].scroll_first_name = Some("x.txt".into());
        s.panes[0].tabs[0].selected = vec!["a.txt".into(), "b c.txt".into()];
        s.panes[0].tabs[1].view_mode = ViewMode::GridLarge;
        let text = s.to_toml();
        let back: Session = toml::from_str(&text).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.active_tab(), 1);
        assert_eq!(back.last_folder, "D:\\work");
        assert!(back.is_usable());
    }

    #[test]
    fn empty_and_missing_files_give_defaults() {
        let d = temp_dir("default");
        let p = d.join("session.toml");
        let l = Session::load(&p);
        assert_eq!(l.status, crate::LoadStatus::Missing);
        assert_eq!(l.value, Session::default());
        assert!(!l.value.is_usable());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_corrupt_session_is_moved_aside_and_defaults_are_used() {
        let d = temp_dir("corrupt");
        let p = d.join("session.toml");
        fs::write(&p, b"version = = 1\n[[[ not toml").unwrap();
        let l = Session::load(&p);
        assert!(matches!(l.status, crate::LoadStatus::Recovered { .. }), "{:?}", l.status);
        assert_eq!(l.value, Session::default());
        assert!(!p.exists(), "the corrupt file is gone");
        assert!(d.join("session.toml.bad").exists(), "and kept as .bad");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn save_and_load_a_file() {
        let d = temp_dir("save");
        let p = d.join("session.toml");
        let s = session(&[d.to_str().unwrap()], 0);
        s.save(&p).unwrap();
        assert!(p.exists());
        let l = Session::load(&p);
        assert_eq!(l.status, crate::LoadStatus::Loaded);
        assert_eq!(l.value, s);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn parent_walks_drive_roots_and_shares() {
        assert_eq!(parent("C:\\Users\\me").as_deref(), Some("C:\\Users"));
        assert_eq!(parent("C:\\Users").as_deref(), Some("C:\\"));
        assert_eq!(parent("C:\\"), None);
        assert_eq!(parent("C:\\Users\\").as_deref(), Some("C:\\"));
        assert_eq!(parent("\\\\srv\\share\\a").as_deref(), Some("\\\\srv\\share"));
        assert_eq!(parent("\\\\srv\\share").as_deref(), None);
        assert_eq!(parent("\\\\srv\\share\\").as_deref(), None);
        assert_eq!(parent("\\\\?\\C:\\a\\b").as_deref(), Some("\\\\?\\C:\\a"));
        assert_eq!(parent("\\\\?\\UNC\\srv\\share\\a").as_deref(), Some("\\\\?\\UNC\\srv\\share"));
    }

    #[test]
    fn missing_folders_fall_back_to_the_nearest_existing_parent() {
        let d = temp_dir("missing");
        let deep = d.join("gone").join("deeper").join("deepest");
        let p = deep.to_str().unwrap();
        assert_eq!(nearest_existing(p).as_deref(), Some(d.to_str().unwrap()));
        // a file that is gone falls back to its folder
        let f = d.join("gone.txt");
        assert_eq!(nearest_existing(f.to_str().unwrap()).as_deref(), Some(d.to_str().unwrap()));
        // an existing folder is returned as is
        assert_eq!(nearest_existing(d.to_str().unwrap()).as_deref(), Some(d.to_str().unwrap()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn network_paths_are_not_checked_or_rewritten() {
        assert_eq!(nearest_existing("\\\\srv\\share\\gone").as_deref(), Some("\\\\srv\\share\\gone"));
        assert_eq!(nearest_existing("\\\\?\\UNC\\srv\\share\\gone").as_deref(), Some("\\\\?\\UNC\\srv\\share\\gone"));
    }

    #[test]
    fn relative_and_empty_paths_have_no_answer() {
        assert_eq!(nearest_existing(""), None);
        assert_eq!(nearest_existing("relative\\path"), None);
    }

    #[test]
    fn repair_moves_every_missing_tab_to_an_existing_folder() {
        let d = temp_dir("repair");
        let good = d.to_str().unwrap().to_string();
        let gone = d.join("gone").join("sub");
        let mut s = session(&[&good, gone.to_str().unwrap(), "\\\\srv\\share\\x"], 1);
        assert_eq!(s.repair_paths(), 1, "only the local missing path changed");
        assert_eq!(s.tabs()[0].path, good);
        assert_eq!(s.tabs()[1].path, good);
        assert_eq!(s.tabs()[2].path, "\\\\srv\\share\\x");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn start_here_and_scroll_clamp() {
        let d = temp_dir("start");
        let t = tab(d.to_str().unwrap());
        assert_eq!(start_here(&t).unwrap().folder, d.to_str().unwrap());
        assert_eq!(clamp_scroll(9, 0), 0);
        assert_eq!(clamp_scroll(9, 3), 2);
        assert_eq!(clamp_scroll(2, 10), 2);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn selected_names_are_capped() {
        let mut names: Vec<String> = (0..1000).map(|i| i.to_string()).collect();
        trim_selected(&mut names);
        assert_eq!(names.len(), MAX_SELECTED);
    }

    #[test]
    fn a_newer_schema_is_loaded_best_effort_and_reported() {
        let d = temp_dir("newer");
        let p = d.join("session.toml");
        fs::write(&p, b"version = 99\nlast_folder = \"C:\\\\x\"\nfuture_key = 1\n").unwrap();
        let l = Session::load(&p);
        assert_eq!(l.status, crate::LoadStatus::Newer { version: 99 });
        assert_eq!(l.value.last_folder, "C:\\x");
        assert!(p.exists(), "a newer file is never moved aside");
        let _ = fs::remove_dir_all(&d);
    }
}
