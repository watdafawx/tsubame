//! Own-drawn dropdown menus for navigation: Back/Forward history (up to 15
//! entries: folder name + dim path) and Breadcrumb sibling subfolders (async
//! background enumeration, folders only, cap 200, current folder in bold).
//!
//! Uses `tsubame_ui::widgets::menu::menu` so scrolling, type-to-filter,
//! keyboard navigation, themes, DPI scaling and UI Automation (`uia::publish`)
//! match the rest of the app with zero idle timers or UI-thread disk access.

use std::cmp::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use tsubame_ui::widgets::menu::{A11yReq, A11ySnap, menu as draw_menu};
use tsubame_ui::{Canvas, Id, MenuRow, MenuSource, MenuState, MenuStyle, Pt, Rect, RowIcon, RowKind, Ui};
use tsubame_win::enumerate::{CancelToken, Enumerator, Message, Wake};
use tsubame_win::uia;

use crate::nav;
use crate::tab::{HIDE, flags_of};

pub const ID_NAV_MENU: Id = Id::new("navmenu");
/// Maximum entries shown in Back / Forward history dropdowns.
pub const HISTORY_CAP: usize = 15;
/// Maximum subfolders listed in a breadcrumb chevron dropdown.
pub const CRUMB_CAP: usize = 200;
/// Long-press duration on Back / Forward buttons before opening history (ms).
pub const HOLD_MS: u64 = 400;
/// Max characters of the dim path shown beside a history folder name.
const DIM_PATH_MAX: usize = 36;
const DISABLED_INFO_KEY: u64 = 0x7fff_0001;

/// Action returned by [`NavMenu::paint`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavMenuOut {
    None,
    Closed,
    /// Navigate active tab's history directly to this index.
    GoHistory(usize),
    /// Navigate active tab to this subfolder path.
    GoFolder(String),
}

/// Long-press tracker for Back / Forward toolbar buttons (zero idle cost:
/// active only while the mouse button is physically held on Back or Forward).
#[derive(Clone, Debug)]
pub struct HoldState {
    pub slot: usize,
    pub forward: bool,
    pub started: Instant,
    /// True once the hold threshold opened the history menu (suppresses release click).
    pub fired: bool,
}

impl HoldState {
    pub fn new(slot: usize, forward: bool) -> Self {
        Self { slot, forward, started: Instant::now(), fired: false }
    }

    pub fn ready(&self) -> bool {
        !self.fired && self.started.elapsed().as_millis() as u64 >= HOLD_MS
    }

    /// Remaining ms until the hold triggers, or `None` if already fired.
    pub fn next_tick_ms(&self) -> Option<u32> {
        self.tick_ms_at(self.started.elapsed().as_millis() as u64)
    }

    /// `next_tick_ms` for `el` ms since the press. A hold nobody resolved (released
    /// outside the button, tab switched, button disabled) must not keep a 1 ms timer
    /// and a repaint loop alive: past twice the threshold it asks for nothing.
    fn tick_ms_at(&self, el: u64) -> Option<u32> {
        if self.fired || el >= HOLD_MS * 2 {
            return None;
        }
        Some(HOLD_MS.saturating_sub(el).max(1) as u32)
    }
}

#[derive(Clone, Debug)]
struct HistRow {
    index: usize,
    name: String,
    dim_path: String,
}

enum Kind {
    History {
        forward: bool,
        entries: Vec<HistRow>,
    },
    Crumb {
        crumb_idx: usize,
        label: String,
        dir_path: String,
        active_child: Option<String>,
        folders: Vec<String>,
        loading: bool,
        truncated: bool,
        error: Option<String>,
        _token: CancelToken,
        enum_gen: u64,
        enumerator: Option<Enumerator>,
        rx: Option<Receiver<Message>>,
        archive_rx: Option<Receiver<crate::archive_ui::ArchiveListReply>>,
    },
}

pub struct NavMenu {
    pub pane: usize,
    pub state: MenuState,
    pub open: bool,
    kind: Kind,
    clock: Instant,
    a11y_prev: Option<A11ySnap>,
    a11y_published: bool,
}

impl NavMenu {
    /// Opens a Back or Forward history dropdown anchored at `anchor`.
    /// Returns `None` if `entries` is empty.
    pub fn history(pane: usize, forward: bool, anchor: Pt, entries: Vec<(usize, String)>) -> Option<Self> {
        if entries.is_empty() {
            return None;
        }
        let rows: Vec<HistRow> = entries
            .into_iter()
            .take(HISTORY_CAP)
            .map(|(index, path)| {
                let name = folder_label(&path);
                let dim_path = compact_path(&path, DIM_PATH_MAX);
                HistRow { index, name, dim_path }
            })
            .collect();
        Some(Self {
            pane,
            state: MenuState::new(anchor, false),
            open: true,
            kind: Kind::History { forward, entries: rows },
            clock: Instant::now(),
            a11y_prev: None,
            a11y_published: false,
        })
    }

    /// Opens a breadcrumb sibling subfolder menu for `dir_path`, starting
    /// asynchronous enumeration on the background enumerator worker.
    pub fn crumb(pane: usize, crumb_idx: usize, label: String, dir_path: String, active_child: Option<String>, anchor: Pt, wake: Wake) -> Self {
        let token = CancelToken::new();
        let enum_gen = token.next_generation();
        let (enumerator, rx, archive_rx) = if crate::archive_ui::is_archive_path(&dir_path) {
            (None, None, Some(crate::archive_ui::start_list(&dir_path, enum_gen, wake)))
        } else {
            let (e, rx) = Enumerator::start_channel(&dir_path, enum_gen, token.clone(), wake);
            (Some(e), Some(rx), None)
        };
        Self {
            pane,
            state: MenuState::new(anchor, false),
            open: true,
            kind: Kind::Crumb {
                crumb_idx,
                label,
                dir_path,
                active_child,
                folders: Vec::new(),
                loading: true,
                truncated: false,
                error: None,
                _token: token,
                enum_gen,
                enumerator,
                rx,
                archive_rx,
            },
            clock: Instant::now(),
            a11y_prev: None,
            a11y_published: false,
        }
    }

    /// Which breadcrumb segment's menu is currently open, if any: `(pane, crumb_idx)`.
    pub fn open_crumb(&self) -> Option<(usize, usize)> {
        match &self.kind {
            Kind::Crumb { crumb_idx, .. } if self.open => Some((self.pane, *crumb_idx)),
            _ => None,
        }
    }

    /// UIA menu name.
    pub fn a11y_name(&self) -> String {
        match &self.kind {
            Kind::History { forward: false, .. } => "Back history".into(),
            Kind::History { forward: true, .. } => "Forward history".into(),
            Kind::Crumb { label, .. } => format!("Subfolders of {label}"),
        }
    }

    /// Drains background subfolder enumeration batches. UI thread, non-blocking.
    pub fn pump(&mut self) -> bool {
        let Kind::Crumb { active_child, folders, loading, truncated, error, enum_gen, enumerator, rx, archive_rx, .. } = &mut self.kind else {
            return false;
        };
        let mut changed = false;
        if let Some(arx) = archive_rx.as_ref() {
            match arx.try_recv() {
                Ok(msg) => {
                    *archive_rx = None;
                    if msg.generation == *enum_gen {
                        *loading = false;
                        match msg.result {
                            Ok(entries) => {
                                let mut dirs: Vec<String> = entries.into_iter().filter(|e| e.is_dir).map(|e| e.name).collect();
                                dirs.sort_by(|a, b| nat_cmp_ci(a, b));
                                if dirs.len() > CRUMB_CAP {
                                    *truncated = true;
                                    dirs.truncate(CRUMB_CAP);
                                }
                                *folders = dirs;
                            }
                            Err(e) => *error = Some(e),
                        }
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    *archive_rx = None;
                    *loading = false;
                    changed = true;
                }
            }
            return changed;
        }
        let hide = HIDE.load(std::sync::atomic::Ordering::Relaxed);
        let mut stop = false;
        if let Some(erx) = rx.as_ref() {
            loop {
                match erx.try_recv() {
                    Ok(msg) => {
                        if msg.generation() != *enum_gen {
                            continue;
                        }
                        match msg {
                            Message::Batch(b) => {
                                for (name_u16, raw) in b.iter() {
                                    if !raw.is_dir() {
                                        continue;
                                    }
                                    let name = String::from_utf16_lossy(name_u16);
                                    let is_active = active_child.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&name));
                                    let fl = flags_of(raw.attrs, true, raw.is_reparse_point(), raw.reparse_tag);
                                    if !is_active && (fl & hide) != 0 {
                                        continue;
                                    }
                                    if folders.len() < CRUMB_CAP {
                                        folders.push(name);
                                        changed = true;
                                    } else {
                                        *truncated = true;
                                        stop = true;
                                        break;
                                    }
                                }
                                if stop {
                                    break;
                                }
                            }
                            Message::Done(d) => {
                                *loading = false;
                                if let Some(e) = d.error
                                    && !d.cancelled
                                {
                                    *error = Some(e.message());
                                }
                                stop = true;
                                changed = true;
                                break;
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        *loading = false;
                        stop = true;
                        changed = true;
                        break;
                    }
                }
            }
        }
        if stop {
            *loading = false;
            *rx = None;
            *enumerator = None;
            folders.sort_by(|a, b| nat_cmp_ci(a, b));
            changed = true;
        }
        changed
    }

    /// Paints the dropdown on top of everything and returns any pick/close action.
    pub fn paint(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bounds: Rect) -> NavMenuOut {
        if !self.open {
            return NavMenuOut::Closed;
        }
        let a11y = uia::active();
        let now_ms = self.clock.elapsed().as_millis() as u64;
        let style = MenuStyle { bounds, loading: None, alt: false, now_ms, a11y };
        let src = Source { kind: &self.kind };
        let r = draw_menu(ui, g, ID_NAV_MENU, &mut self.state, &src, &style);
        if a11y {
            self.publish_a11y(r.closed);
        } else if self.a11y_published {
            self.close_a11y();
        }
        if let Some(path) = r.picked
            && let Some(&key) = path.last()
        {
            self.open = false;
            self.close_a11y();
            if let Some(out) = self.pick_for(key) {
                return out;
            }
            return NavMenuOut::Closed;
        }
        if r.closed {
            self.open = false;
            self.close_a11y();
            return NavMenuOut::Closed;
        }
        NavMenuOut::None
    }

    fn pick_for(&self, key: u64) -> Option<NavMenuOut> {
        let i = key.checked_sub(1)? as usize;
        match &self.kind {
            Kind::History { entries, .. } => entries.get(i).map(|e| NavMenuOut::GoHistory(e.index)),
            Kind::Crumb { dir_path, folders, .. } => folders.get(i).map(|name| NavMenuOut::GoFolder(nav::join(dir_path, name))),
        }
    }

    /// Runs queued UIA Invoke/Collapse requests on the next frame.
    pub fn run_a11y_requests(&mut self) {
        for r in uia::take_requests() {
            self.state.request(r.lv, r.key, if r.collapse { A11yReq::Collapse } else { A11yReq::Invoke });
        }
    }

    fn publish_a11y(&mut self, closed: bool) {
        let Some(snap) = self.state.a11y.take() else { return };
        if closed || snap.levels.len() != self.state.levels.len() || self.a11y_prev.as_ref() == Some(&snap) {
            return;
        }
        let mut s = crate::ctxui::a11y_snap(&snap);
        if let Some(lv0) = s.levels.first_mut() {
            lv0.name = self.a11y_name();
        }
        uia::publish(s);
        self.a11y_prev = Some(snap);
        self.a11y_published = true;
    }

    fn close_a11y(&mut self) {
        if std::mem::take(&mut self.a11y_published) {
            uia::publish(uia::Snap::default());
        }
        self.a11y_prev = None;
    }

    #[cfg(test)]
    pub fn folders(&self) -> Option<&[String]> {
        match &self.kind {
            Kind::Crumb { folders, .. } => Some(folders),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn is_loading(&self) -> bool {
        matches!(&self.kind, Kind::Crumb { loading: true, .. })
    }

    #[cfg(test)]
    pub fn is_truncated(&self) -> bool {
        matches!(&self.kind, Kind::Crumb { truncated: true, .. })
    }

    #[cfg(test)]
    pub fn test_rows(&self) -> Vec<MenuRow> {
        let src = Source { kind: &self.kind };
        let mut out = Vec::new();
        src.rows(&[], &mut out);
        out
    }
}

impl Drop for NavMenu {
    fn drop(&mut self) {
        self.close_a11y();
    }
}

struct Source<'a> {
    kind: &'a Kind,
}

impl MenuSource for Source<'_> {
    fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
        if !path.is_empty() {
            return;
        }
        match self.kind {
            Kind::History { entries, .. } => {
                for (i, e) in entries.iter().enumerate() {
                    out.push(MenuRow {
                        icon: RowIcon::Glyph('\u{E8B7}'),
                        shortcut: e.dim_path.clone(),
                        ..MenuRow::item(i as u64 + 1, &e.name)
                    });
                }
            }
            Kind::Crumb { active_child, folders, loading, truncated, error, .. } => {
                if *loading && folders.is_empty() {
                    out.push(MenuRow { disabled: true, ..MenuRow::item(DISABLED_INFO_KEY, "Loading\u{2026}") });
                    return;
                }
                if let Some(err) = error
                    && folders.is_empty()
                {
                    out.push(MenuRow { disabled: true, ..MenuRow::item(DISABLED_INFO_KEY, err) });
                    return;
                }
                if folders.is_empty() {
                    out.push(MenuRow { disabled: true, ..MenuRow::item(DISABLED_INFO_KEY, "No subfolders") });
                    return;
                }
                for (i, name) in folders.iter().enumerate() {
                    let is_cur = active_child.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(name));
                    out.push(MenuRow {
                        icon: RowIcon::Glyph('\u{E8B7}'),
                        default: is_cur,
                        ..MenuRow::item(i as u64 + 1, name)
                    });
                }
                if *truncated {
                    out.push(MenuRow::separator());
                    out.push(MenuRow {
                        kind: RowKind::Label,
                        ..MenuRow::item(0, &format!("Showing first {CRUMB_CAP} folders"))
                    });
                }
            }
        }
    }

    fn icon(&self, _key: u64) -> Option<(u32, u32, &[u32])> {
        None
    }
}

/// Display name for a folder path (its last segment, or the drive label).
pub fn folder_label(path: &str) -> String {
    nav::crumbs(path).pop().map(|c| c.label).unwrap_or_else(|| path.to_string())
}

/// Shortens long paths from the middle so both folder name and dim path fit
/// comfortably in the history dropdown.
pub fn compact_path(path: &str, max_chars: usize) -> String {
    let n = path.chars().count();
    if n <= max_chars || max_chars < 8 {
        return path.to_string();
    }
    let keep_head = 6.min(max_chars / 3);
    let keep_tail = max_chars.saturating_sub(keep_head + 1);
    let mut out: String = path.chars().take(keep_head).collect();
    out.push('\u{2026}');
    out.extend(path.chars().skip(n - keep_tail));
    out
}

/// Natural case-insensitive comparison (`dir2` before `dir10`, ASCII digits).
pub fn nat_cmp_ci(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.as_bytes(), b.as_bytes());
    while !ai.is_empty() && !bi.is_empty() {
        if ai[0].is_ascii_digit() && bi[0].is_ascii_digit() {
            let an = ai.iter().take_while(|c| c.is_ascii_digit()).count();
            let bn = bi.iter().take_while(|c| c.is_ascii_digit()).count();
            let as_ = &ai[..an];
            let bs_ = &bi[..bn];
            let az = as_.iter().take_while(|&&c| c == b'0').count();
            let bz = bs_.iter().take_while(|&&c| c == b'0').count();
            let (ad, bd) = (&as_[az..], &bs_[bz..]);
            let ord = ad.len().cmp(&bd.len()).then_with(|| ad.cmp(bd)).then_with(|| an.cmp(&bn));
            if ord != Ordering::Equal {
                return ord;
            }
            ai = &ai[an..];
            bi = &bi[bn..];
        } else {
            let ac = ai[0].to_ascii_lowercase();
            let bc = bi[0].to_ascii_lowercase();
            if ac != bc {
                return ac.cmp(&bc);
            }
            ai = &ai[1..];
            bi = &bi[1..];
        }
    }
    ai.len().cmp(&bi.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    #[test]
    fn unresolved_hold_stops_asking_for_a_timer() {
        let h = HoldState::new(0, false);
        assert_eq!(h.tick_ms_at(0), Some(HOLD_MS as u32));
        assert_eq!(h.tick_ms_at(HOLD_MS - 1), Some(1));
        // due but never resolved by a paint: still bounded
        assert_eq!(h.tick_ms_at(HOLD_MS), Some(1));
        assert_eq!(h.tick_ms_at(HOLD_MS * 2), None, "no endless 1 ms timer");
        let mut fired = h.clone();
        fired.fired = true;
        assert_eq!(fired.tick_ms_at(0), None);
    }

    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn history_menu_caps_at_15_and_formats_name_and_dim_path() {
        let entries: Vec<(usize, String)> = (0..20).map(|i| (i, format!(r"C:\Users\me\Folder{i}"))).collect();
        let m = NavMenu::history(0, false, Pt::new(10.0, 40.0), entries).unwrap();
        assert_eq!(m.a11y_name(), "Back history");
        let rows = m.test_rows();
        assert_eq!(rows.len(), HISTORY_CAP);
        assert_eq!(rows[0].label, "Folder0");
        assert_eq!(rows[0].shortcut, r"C:\Users\me\Folder0");
        assert_eq!(m.pick_for(rows[2].key), Some(NavMenuOut::GoHistory(2)));
        assert!(NavMenu::history(0, true, Pt::new(0.0, 0.0), vec![]).is_none());
    }

    #[test]
    fn crumb_menu_lists_folders_only_bolds_active_and_caps_at_200() {
        let dir = std::env::temp_dir().join(format!("tsubame-crumb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("beta")).unwrap();
        std::fs::create_dir_all(dir.join("alpha")).unwrap();
        std::fs::create_dir_all(dir.join("dir10")).unwrap();
        std::fs::create_dir_all(dir.join("dir2")).unwrap();
        std::fs::write(dir.join("not_a_folder.txt"), b"x").unwrap();

        let wake: Wake = Arc::new(|| {});
        let mut m = NavMenu::crumb(0, 1, "root".into(), dir.to_string_lossy().into_owned(), Some("beta".into()), Pt::new(10.0, 40.0), wake);
        assert_eq!(m.a11y_name(), "Subfolders of root");
        let t0 = Instant::now();
        while m.is_loading() && t0.elapsed() < Duration::from_secs(5) {
            m.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!m.is_loading());
        assert!(!m.is_truncated());
        let rows = m.test_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "dir2", "dir10"], "folders only, natural order");
        assert!(!rows[0].default && rows[1].default, "active child is bold");
        assert_eq!(m.pick_for(rows[1].key), Some(NavMenuOut::GoFolder(nav::join(&dir.to_string_lossy(), "beta"))));

        // Cap at 200 subfolders
        let big = std::env::temp_dir().join(format!("tsubame-crumb-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&big);
        std::fs::create_dir_all(&big).unwrap();
        for i in 0..210 {
            std::fs::create_dir(big.join(format!("sub{i:03}"))).unwrap();
        }
        let mut m2 = NavMenu::crumb(0, 0, "big".into(), big.to_string_lossy().into_owned(), None, Pt::new(10.0, 40.0), Arc::new(|| {}));
        let t1 = Instant::now();
        while m2.is_loading() && t1.elapsed() < Duration::from_secs(10) {
            m2.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!m2.is_loading());
        assert!(m2.is_truncated());
        assert_eq!(m2.folders().unwrap().len(), CRUMB_CAP);

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&big);
    }

    #[test]
    fn compact_path_keeps_short_and_truncates_long() {
        assert_eq!(compact_path(r"C:\Users\me", 36), r"C:\Users\me");
        let long = r"C:\Users\someone\Documents\Projects\SubFolder\VeryDeepDirectory";
        let c = compact_path(long, 30);
        assert_eq!(c.chars().count(), 30);
        assert!(c.starts_with(r"C:\Use") && c.contains('\u{2026}') && c.ends_with("Directory"));
    }
}
