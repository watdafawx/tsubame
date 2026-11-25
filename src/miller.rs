//! Miller columns model (P4.F6, built-in extension `tsubame.ext.miller`,
//! `docs/f6-mock.md`, `mocks/mock-f6-miller.html`).
//!
//! One column per folder level. The model is a chain: `cols[0]` is the
//! outermost live folder, `cols[active]` is the column the keyboard is in,
//! and at most one column follows it: the children of the folder selected in
//! the active column (a *path row* in every column left of it), or none when
//! a file is selected (the info column is drawn from [`Info`]).
//!
//! Memory and work, by design:
//!
//! * a column is one `EntryStore` (arrays and one name buffer, no
//!   per-entry allocation; the sorted view is its `Vec<u32>` index), one
//!   `ListState`, a path and a few flags. At most [`MAX_COLS`] (12) columns
//!   are live; going deeper drops the leftmost (the view counts them in the
//!   "hidden columns" cue and `ascend` re-lists them on demand);
//! * a folder is listed only when it becomes a column (selected, or on the
//!   path), through the normal enumerator, one cancel token per request.
//!   Moving the selection on drops the unfinished listing (which cancels it).
//!   Nothing is read ahead, nothing scans a disk;
//! * a column lists at most [`COL_CAP`] entries (a header note says so);
//! * junctions and symlinks are never followed by a selection: the column
//!   for one is a card until an explicit action (Right, Enter, double click,
//!   the Follow button), then an ancestor loop check runs on a worker;
//! * watchers: one per live listed column (so at most 12), dropped with it;
//! * no timers. The only waiting thread is the one-shot 80 ms sleeper that
//!   wakes the UI so the loading skeleton can appear for a listing still
//!   running, and the lazy excerpt worker, blocked in `recv` when idle.

use crate::nav;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};
use tsubame_core::{EntryId, EntryInput, EntryStore, FOLDER, FilterMode, HIDDEN, READONLY, REPARSE, SYSTEM, Selection, Sort};
use tsubame_ui::{ListState, Rect};
use tsubame_win::enumerate::{Batch, CancelToken, Done, Enumerator, Message, Wake};
use tsubame_win::watch::{self, DirWatcher, Gone, Options as WatchOptions};

pub const EXT_MILLER: &str = "tsubame.ext.miller";

/// Live columns at most.
pub const MAX_COLS: usize = 12;
pub const MIN_W: f32 = 140.0;
pub const MAX_W: f32 = 480.0;
#[cfg(test)]
pub const DEFAULT_W: f32 = 220.0;
/// The info column's minimum width.
pub const INFO_MIN_W: f32 = 260.0;
/// Under this pane width the view is a single-column drill-down.
pub const DRILL_BELOW: f32 = 560.0;
/// Entries one column lists at most.
pub const COL_CAP: usize = 100_000;
/// The skeleton appears after this long.
pub const LOADING_AFTER: Duration = Duration::from_millis(80);
/// Bytes read for the info column's text excerpt, and its line limits.
pub const EXCERPT_BYTES: usize = 8 * 1024;
pub const EXCERPT_LINES: usize = 14;
pub const EXCERPT_COLS: usize = 120;
/// Watch batches up to this many names apply in place; more relist.
const WATCH_INLINE: usize = 30;
const TYPE_AHEAD: Duration = Duration::from_millis(1000);

/// Built-in extension registering `view.miller`.
pub struct MillerExtension;

impl tsubame_ext_api::Extension<crate::ext::App> for MillerExtension {
    fn info(&self) -> tsubame_ext_api::ExtensionInfo {
        tsubame_ext_api::ExtensionInfo::builtin(EXT_MILLER, "Miller columns view", false)
    }

    fn contribute(&self, c: &mut tsubame_ext_api::Contributions<crate::ext::App>) {
        for spec in crate::commands::MILLER_COMMANDS {
            c.command(spec.clone(), crate::view::View::run_builtin);
        }
    }
}

/// The Miller extension is enabled.
pub fn enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_MILLER).unwrap_or(false))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// the first listing (or a loop check) runs
    Loading,
    Done,
    /// the folder could not be listed (message for the card)
    Denied(String),
    /// Esc stopped the first listing
    Cancelled,
    /// the target is an ancestor: the path it loops back to
    Loop(String),
    /// a junction or symlink that nobody followed yet
    Link,
}

/// What the info column knows about the selected file's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Excerpt {
    /// `tsubame.ext.preview` is off: metadata only
    Off,
    Loading,
    Text(String),
    /// binary, or not text
    NoText,
    Failed,
}

/// The selected file (metadata from the listing, the excerpt from a worker).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub flags: u16,
    pub excerpt: Excerpt,
}

struct Load {
    store: EntryStore,
    total: usize,
    capped: bool,
    /// keeps the worker alive: dropping it cancels the listing
    _en: Enumerator,
    rx: Receiver<Message>,
}

pub struct Column {
    pub path: String,
    pub store: EntryStore,
    pub state: State,
    /// the selected entry: a path row (columns left of the active one) or the
    /// selection of the active column
    pub sel: Option<EntryId>,
    /// the rows of a multi-selection (Ctrl/Shift click, Ctrl+A, Shift+arrows) in
    /// the active column. Fewer than two rows: `sel` alone is the selection.
    pub multi: Selection,
    pub list: ListState,
    /// own width in DIPs; 0 = the default
    pub width: f32,
    /// entries the folder has / the listing kept
    pub total: usize,
    pub capped: bool,
    /// reached through a junction or symlink
    pub is_link: bool,
    /// where that link points (resolved on a worker)
    pub target: Option<String>,
    /// scroll the selected row into view at the next paint (the list has no height yet)
    pub reveal_row: bool,
    started: Instant,
    published: Option<Instant>,
    /// the skeleton may be drawn (80 ms passed since the listing started)
    pub loading_due: bool,
    load: Option<Box<Load>>,
    check: Option<Receiver<Option<String>>>,
    target_rx: Option<Receiver<Option<String>>>,
    watch: Option<(DirWatcher, Receiver<watch::Message>)>,
    /// select this name once the listing arrives (the next folder of the path)
    want_sel: Option<String>,
    /// select the first entry once the listing arrives (Right / Enter into it)
    want_first: bool,
}

impl Column {
    fn new(path: String, state: State, hide: u16, sort: Sort, width: f32) -> Column {
        let mut store = EntryStore::new();
        store.set_hide(hide);
        store.sort(sort);
        Column {
            path,
            store,
            state,
            sel: None,
            multi: Selection::new(),
            list: ListState::new(22.0),
            width,
            total: 0,
            capped: false,
            is_link: false,
            target: None,
            reveal_row: false,
            started: Instant::now(),
            published: None,
            loading_due: false,
            load: None,
            check: None,
            target_rx: None,
            watch: None,
            want_sel: None,
            want_first: false,
        }
    }

    /// Folder name for the header (the drive for a root).
    pub fn title(&self) -> &str {
        let t = self.path.trim_end_matches('\\');
        if t.is_empty() {
            return &self.path;
        }
        match t.rfind('\\') {
            Some(i) if i + 1 < t.len() && !t.ends_with(':') => &t[i + 1..],
            _ => t,
        }
    }

    /// Entries the column shows (after the filter).
    pub fn shown(&self) -> usize {
        self.store.view().len()
    }

    pub fn is_loading(&self) -> bool {
        self.load.is_some() || self.check.is_some()
    }

    /// Entries the running first listing has delivered so far.
    pub fn delivered(&self) -> usize {
        self.load.as_ref().map_or(0, |l| l.total)
    }

    /// Several rows are selected (the column has no child column and no info then).
    pub fn is_multi(&self) -> bool {
        self.multi.count() > 1
    }

    /// Row `id` is selected (the full selection of the active column).
    pub fn row_selected(&self, id: EntryId) -> bool {
        if self.is_multi() { self.multi.contains(id) } else { self.sel == Some(id) }
    }

    /// Rows selected in this column.
    pub fn sel_count(&self) -> usize {
        if self.is_multi() { self.multi.count() } else { usize::from(self.sel.is_some()) }
    }

    /// Position in the view of the selected entry.
    pub fn sel_pos(&self) -> Option<usize> {
        let id = self.sel?;
        self.store.view().iter().position(|v| *v == id)
    }

    pub fn entry_name(&self, pos: usize) -> Option<String> {
        let id = self.store.at(pos)?;
        self.store.name(id).map(utf8)
    }

    pub fn is_folder_at(&self, pos: usize) -> bool {
        self.store.at(pos).and_then(|id| self.store.record(id)).is_some_and(|r| r.flags & FOLDER != 0)
    }

    pub fn is_link_at(&self, pos: usize) -> bool {
        self.store.at(pos).and_then(|id| self.store.record(id)).is_some_and(|r| r.flags & FOLDER != 0 && r.flags & REPARSE != 0)
    }
}

fn utf8(w: &[u16]) -> String {
    String::from_utf16_lossy(w)
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Case-insensitive path equality (Windows), ignoring a trailing separator.
pub fn same_path(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_end_matches('\\'), b.trim_end_matches('\\'));
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

/// `C:\a\b` becomes `["C:\", "C:\a", "C:\a\b"]`.
pub fn chain(path: &str) -> Vec<String> {
    let mut v = vec![path.to_string()];
    let mut cur = path.to_string();
    for _ in 0..256 {
        match nav::parent(&cur) {
            Some(p) if !same_path(&p, &cur) => {
                v.push(p.clone());
                cur = p;
            }
            _ => break,
        }
    }
    v.reverse();
    v
}

fn base_name(path: &str) -> String {
    path.trim_end_matches('\\').rsplit('\\').next().unwrap_or("").to_string()
}

/// What Enter or a double click did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activated {
    Nothing,
    /// moved into a folder (the new active column)
    Entered,
    /// a file: the caller opens it
    Open(String),
}

pub struct MillerModel {
    cols: Vec<Column>,
    /// the column the keyboard is in
    pub active: usize,
    /// ancestors left of `cols[0]` that are not live (dropped past the cap)
    pub dropped: usize,
    wake: Wake,
    sort: Sort,
    hide: u16,
    default_w: f32,
    visible: bool,
    watch_dirty: bool,
    /// the folder path the tab was told about (the view compares it with the tab's)
    pub synced: String,
    /// a message for the status bar (cancelled, capped, refresh errors)
    pub status: Option<String>,
    typed: String,
    typed_at: Option<Instant>,
    /// the selected file's info
    info: Option<Info>,
    preview_on: bool,
    excerpter: Option<Excerpter>,
    excerpt_gen: u64,
    /// the filter text applied to `filter_col`'s store
    filter: String,
    filter_col: usize,
    /// horizontal scroll of the columns strip (DIPs) and the wish to bring the active column into view
    pub hscroll: f32,
    pub reveal: bool,
    /// narrow window: the list shown is the active column only (set by the view each frame)
    pub dirty: bool,
    /// the rows area each column was painted in last frame (presses, drops and the rename field hit-test it)
    pub hits: Vec<ColHit>,
    /// a double-click on the grip of this column reset its width: the press is still held, so the drag must not take the width back until it is released
    pub grip_hold: Option<usize>,
    /// the active folder before the last `goto` that built new columns (a typed path that fails goes back to it)
    goto_from: Option<String>,
    /// (folder, name, since): select this entry once it shows up (after a rename)
    pick: Option<(String, String, Instant)>,
}

/// Where one column's rows were painted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColHit {
    pub col: usize,
    /// the rows area (scrollbar excluded)
    pub rows: Rect,
}

/// How long a renamed entry is waited for.
const PICK_FOR: Duration = Duration::from_secs(5);

impl MillerModel {
    /// A model showing `path` as its active column; the ancestors are the path rows.
    pub fn new(path: &str, wake: Wake, sort: Sort, hide: u16, default_w: f32) -> MillerModel {
        let mut m = MillerModel {
            cols: Vec::new(),
            active: 0,
            dropped: 0,
            wake,
            sort,
            hide,
            default_w: default_w.clamp(MIN_W, MAX_W),
            visible: true,
            watch_dirty: true,
            synced: path.to_string(),
            status: None,
            typed: String::new(),
            typed_at: None,
            info: None,
            preview_on: false,
            excerpter: None,
            excerpt_gen: 0,
            filter: String::new(),
            filter_col: 0,
            hscroll: 0.0,
            reveal: true,
            dirty: true,
            hits: Vec::new(),
            grip_hold: None,
            goto_from: None,
            pick: None,
        };
        m.goto(path);
        m
    }

    // ---- reading ---------------------------------------------------------------

    #[cfg(test)]
    pub fn cols(&self) -> &[Column] {
        &self.cols
    }

    pub fn col(&self, i: usize) -> Option<&Column> {
        self.cols.get(i)
    }

    pub fn col_mut(&mut self, i: usize) -> Option<&mut Column> {
        self.cols.get_mut(i)
    }

    pub fn len(&self) -> usize {
        self.cols.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }

    pub fn active_col(&self) -> Option<&Column> {
        self.cols.get(self.active)
    }

    /// The folder of the active column (what the tab's path follows).
    pub fn active_path(&self) -> &str {
        self.cols.get(self.active).map_or("", |c| c.path.as_str())
    }

    pub fn info(&self) -> Option<&Info> {
        self.info.as_ref()
    }

    /// Entries the active column shows (the status bar's count).
    pub fn active_shown(&self) -> usize {
        self.active_col().map_or(0, |c| c.shown())
    }

    /// Full path of row `pos` of the active column, and whether it is a folder.
    pub fn active_entry_at(&self, pos: usize) -> Option<(String, bool)> {
        let c = self.active_col()?;
        let name = c.entry_name(pos)?;
        Some((nav::join(&c.path, &name), c.is_folder_at(pos)))
    }

    /// The selected entry of the active column: path and folder flag.
    pub fn selected(&self) -> Option<(String, bool)> {
        let c = self.active_col()?;
        let pos = c.sel_pos()?;
        self.active_entry_at(pos)
    }

    pub fn selected_path(&self) -> Option<String> {
        self.selected().map(|s| s.0)
    }

    pub fn width(&self, i: usize) -> f32 {
        match self.cols.get(i) {
            Some(c) if c.width >= MIN_W => c.width.min(MAX_W),
            _ => self.default_w,
        }
    }

    pub fn set_default_width(&mut self, w: f32) {
        self.default_w = w.clamp(MIN_W, MAX_W);
    }

    pub fn set_width(&mut self, i: usize, w: f32) {
        if let Some(c) = self.cols.get_mut(i) {
            c.width = w.clamp(MIN_W, MAX_W);
        }
    }

    pub fn reset_width(&mut self, i: usize) {
        if let Some(c) = self.cols.get_mut(i) {
            c.width = 0.0;
        }
    }

    /// The column after the active one exists (a folder is selected) and is not a file.
    pub fn has_child(&self) -> bool {
        self.active + 1 < self.cols.len()
    }

    /// Columns drawn: the chain, plus the info column when a file is selected.
    pub fn shows_info(&self) -> bool {
        self.info.is_some() && !self.has_child()
    }

    /// The first listing of any column runs.
    pub fn loading(&self) -> bool {
        self.cols.iter().any(|c| c.is_loading())
    }

    /// Live watchers (at most `MAX_COLS`).
    #[cfg(test)]
    pub fn watched_count(&self) -> usize {
        self.cols.iter().filter(|c| c.watch.is_some()).count()
    }

    // ---- building the chain -----------------------------------------------------

    /// Shows `path` as the active column. A column already on the path stays
    /// (its listing is kept); a folder that is one of the live columns just
    /// becomes the active one.
    pub fn goto(&mut self, path: &str) {
        self.synced = path.to_string();
        let from = self.cols.get(self.active).filter(|c| c.state == State::Done).map(|c| c.path.clone());
        self.goto_from = None;
        if let Some(j) = self.cols.iter().position(|c| same_path(&c.path, path)) {
            self.active = j;
            self.reveal = true;
            self.dirty = true;
            self.after_active_change();
            return;
        }
        let mut chain = chain(path);
        let mut dropped = 0;
        if chain.len() > MAX_COLS {
            dropped = chain.len() - MAX_COLS;
            chain.drain(..dropped);
        }
        // keep the common prefix of the live columns
        let keep = self.cols.iter().zip(&chain).take_while(|(c, p)| same_path(&c.path, p)).count();
        // the dropped count only makes sense for the same root: a different prefix resets it
        if keep == 0 || dropped != self.dropped {
            self.cols.clear();
        } else {
            self.cols.truncate(keep);
        }
        self.dropped = dropped;
        let keep = self.cols.len();
        if chain.len() > keep {
            // new columns will list: a typed path that fails on one goes back to `from`
            self.goto_from = from;
        }
        for p in chain.iter().skip(keep) {
            let mut c = Column::new(p.clone(), State::Loading, self.hide, self.sort, 0.0);
            c.started = Instant::now();
            self.cols.push(c);
            let i = self.cols.len() - 1;
            self.start_load(i);
        }
        // every column but the last points at the next folder of the path
        let n = self.cols.len();
        for i in 0..n.saturating_sub(1) {
            let next = base_name(&self.cols[i + 1].path);
            self.point_at(i, &next);
        }
        if let Some(last) = self.cols.last_mut() {
            last.sel = None;
            last.want_sel = None;
        }
        self.active = n.saturating_sub(1);
        self.info = None;
        self.reveal = true;
        self.dirty = true;
        self.watch_dirty = true;
        self.after_active_change();
    }

    /// Column `i` selects the entry called `name` (now if listed, else when it arrives).
    fn point_at(&mut self, i: usize, name: &str) {
        let c = &mut self.cols[i];
        let w: Vec<u16> = name.encode_utf16().collect();
        match c.store.find(&w) {
            Some(id) if c.published.is_some() => {
                c.sel = Some(id);
                c.reveal_row = true;
                c.want_sel = None;
                if let Some(p) = c.sel_pos() {
                    c.list.cursor = p;
                }
            }
            _ => {
                c.sel = None;
                c.want_sel = Some(name.to_string());
            }
        }
    }

    /// Re-reads every column (F5): rows, selection and columns stay.
    pub fn relist_all(&mut self) {
        for i in 0..self.cols.len() {
            self.relist(i);
        }
    }

    /// Re-reads column `i` keeping its rows until the new listing swaps in.
    pub fn relist(&mut self, i: usize) {
        let ok = self.cols.get(i).is_some_and(|c| c.load.is_none() && c.published.is_some() && c.check.is_none());
        if ok {
            self.start_load(i);
        }
    }

    // ---- loading ------------------------------------------------------------------

    fn start_load(&mut self, i: usize) {
        let wake = self.wake.clone();
        let (hide, sort) = (self.hide, self.sort);
        let Some(c) = self.cols.get_mut(i) else { return };
        let token = CancelToken::new();
        let generation = token.next_generation();
        let (en, rx) = Enumerator::start_channel(&c.path, generation, token, wake.clone());
        let mut store = EntryStore::new();
        store.set_hide(hide);
        store.sort(sort);
        store.begin_stream();
        c.load = Some(Box::new(Load { store, total: 0, capped: false, _en: en, rx }));
        if c.published.is_none() {
            c.state = State::Loading;
            c.started = Instant::now();
            c.loading_due = false;
            // the skeleton needs a repaint at 80 ms; nothing else would wake the UI
            let _ = std::thread::Builder::new().name("tsubame-miller-delay".into()).stack_size(64 * 1024).spawn(move || {
                std::thread::sleep(LOADING_AFTER + Duration::from_millis(5));
                wake();
            });
        }
    }

    fn ingest(c: &mut Column, b: &Batch) {
        let Some(load) = c.load.as_mut() else { return };
        let mut inputs: Vec<EntryInput> = Vec::with_capacity(b.len());
        for (name, e) in b.iter() {
            load.total += 1;
            if load.store.len() + inputs.len() >= COL_CAP {
                load.capped = true;
                continue;
            }
            inputs.push(EntryInput { name, size: e.size, modified: e.mtime as i64, flags: crate::tab::flags_of(e.attrs, e.is_dir(), e.is_reparse_point(), e.reparse_tag) });
        }
        let _ = load.store.push_batch(load.store.generation(), &inputs);
    }

    /// Moves what the workers delivered into the model. UI thread, no disk.
    /// True when something to draw changed.
    pub fn pump(&mut self, preview_on: bool) -> bool {
        let mut changed = std::mem::take(&mut self.dirty);
        if preview_on != self.preview_on {
            self.preview_on = preview_on;
            self.refresh_info();
            changed = true;
        }
        for i in 0..self.cols.len() {
            changed |= self.pump_col(i);
        }
        changed |= self.pump_excerpt();
        changed |= self.try_pick();
        // the skeleton becomes due after 80 ms
        let now = Instant::now();
        for c in &mut self.cols {
            if !c.loading_due && c.published.is_none() && c.is_loading() && now.duration_since(c.started) >= LOADING_AFTER {
                c.loading_due = true;
                changed = true;
            }
        }
        if self.watch_dirty && self.visible {
            self.watch_dirty = false;
            self.rebalance_watchers();
        }
        changed
    }

    fn pump_col(&mut self, i: usize) -> bool {
        let mut changed = false;
        // a followed link: the loop check answers
        let mut answer: Option<Option<String>> = None;
        if let Some(c) = self.cols.get_mut(i)
            && let Some(rx) = &c.check
        {
            match rx.try_recv() {
                Ok(v) => answer = Some(v),
                Err(TryRecvError::Disconnected) => answer = Some(None),
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(hit) = answer {
            if let Some(c) = self.cols.get_mut(i) {
                c.check = None;
                if let Some(target) = hit {
                    c.state = State::Loop(target);
                }
            }
            if !self.cols.get(i).is_some_and(|c| matches!(c.state, State::Loop(_))) {
                self.start_load(i);
            }
            changed = true;
        }
        // a link's target text
        if let Some(c) = self.cols.get_mut(i)
            && let Some(rx) = &c.target_rx
        {
            match rx.try_recv() {
                Ok(t) => {
                    c.target = t;
                    c.target_rx = None;
                    changed = true;
                }
                Err(TryRecvError::Disconnected) => c.target_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        // listing messages, within a small time budget
        let t0 = Instant::now();
        let mut finished: Option<Done> = None;
        loop {
            if t0.elapsed() >= Duration::from_millis(4) {
                (self.wake)();
                break;
            }
            let Some(c) = self.cols.get_mut(i) else { break };
            let Some(load) = c.load.as_ref() else { break };
            match load.rx.try_recv() {
                Ok(Message::Batch(b)) => {
                    Self::ingest(c, &b);
                    changed = true;
                }
                Ok(Message::Done(done)) => {
                    finished = Some(done);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Done { generation: 0, total: 0, elapsed: Duration::ZERO, error: Some(tsubame_win::enumerate::Win32Error::win32(1)), cancelled: false, find_fallback: false });
                    break;
                }
            }
        }
        if let Some(done) = finished {
            self.finish_load(i, done);
            changed = true;
        }
        changed |= self.pump_watch(i);
        changed
    }

    fn finish_load(&mut self, i: usize, done: Done) {
        let active_now = self.active;
        let Some(c) = self.cols.get_mut(i) else { return };
        let Some(load) = c.load.take() else { return };
        let load = *load;
        if done.cancelled {
            return;
        }
        if let Some(e) = done.error {
            if c.published.is_none() {
                c.state = State::Denied(e.message());
                // the columns after a folder that cannot open are all the same failure: one card
                if i + 1 < self.cols.len() {
                    self.cols.truncate(i + 1);
                    self.active = self.active.min(i);
                    self.info = None;
                }
            } else {
                self.status = Some(format!("Could not refresh {}: {}", c.path, e.message()));
            }
            return;
        }
        let Load { mut store, total, capped, .. } = load;
        store.end_stream();
        let first = c.published.is_none();
        // a multi-selection follows its names across the refresh
        let multi_names: Vec<Vec<u16>> = if !first && c.is_multi() { c.store.view().iter().filter(|id| c.multi.contains(**id)).filter_map(|id| c.store.name(*id).map(<[u16]>::to_vec)).collect() } else { Vec::new() };
        let old = std::mem::replace(&mut c.store, store);
        if !multi_names.is_empty() {
            let ids: Vec<EntryId> = multi_names.iter().filter_map(|n| c.store.find(n)).collect();
            c.multi = Selection::new();
            c.multi.replace_with(&c.store, ids);
        } else {
            c.multi = Selection::new();
        }
        c.total = total;
        c.capped = capped;
        c.state = State::Done;
        c.published = Some(Instant::now());
        if capped {
            self.status = Some(format!("{} lists the first {} of {} entries.", c.title(), c.store.len(), total));
        }
        if first {
            // the selection the path or the user asked for
            if let Some(name) = c.want_sel.take() {
                let w: Vec<u16> = name.encode_utf16().collect();
                if let Some(id) = c.store.find(&w) {
                    c.sel = Some(id);
                    c.reveal_row = true;
                }
            }
            // Right / Enter into the column, or the column the keyboard is in: the first entry is selected
            let first_wanted = std::mem::take(&mut c.want_first);
            if c.sel.is_none() && (first_wanted || i == active_now) {
                c.sel = c.store.at(0);
                c.reveal_row = true;
            }
        } else if let Some(old_id) = c.sel {
            // a refresh: the selection follows its name
            let found = old.name(old_id).and_then(|n| c.store.find(n));
            c.sel = found;
            c.multi.focus = found;
            if c.multi.count() < 2 {
                match found.and_then(|_| c.sel_pos()) {
                    Some(p) => c.multi.single(&c.store, p),
                    None => c.multi.clear(),
                }
            }
            if found.is_none() {
                self.cols.truncate(i + 1);
                self.info = None;
            }
        }
        let was_sel = self.cols.get(i).and_then(|c| c.sel);
        if let Some(c) = self.cols.get_mut(i) {
            if let Some(p) = c.sel_pos() {
                c.list.cursor = p;
            }
            if c.sel.is_some() {
                c.reveal_row = true;
            }
        }
        if i == self.filter_col && !self.filter.is_empty() {
            let (text, mode) = (self.filter.clone(), filter_mode(&self.filter));
            if let Some(c) = self.cols.get_mut(i) {
                c.store.filter(&text, mode);
            }
        }
        // a selection that arrived opens its child column (first listing of a path row, want_first)
        if was_sel.is_some() && first && i == self.active && !self.has_child() && self.info.is_none() {
            let pos = self.cols[i].sel_pos();
            if let Some(p) = pos {
                self.apply_selection(i, p);
            }
        } else if was_sel.is_some() && !first && i == self.active {
            self.refresh_info();
        }
        self.watch_dirty = true;
        self.dirty = true;
    }

    // ---- selection ----------------------------------------------------------------

    /// Selects row `pos` of column `c` and makes `c` the active column. True
    /// when the selection or the active column changed.
    pub fn select_pos(&mut self, c: usize, pos: usize) -> bool {
        let has_child = self.has_child();
        let (active, has_info) = (self.active, self.info.is_some());
        let Some(col) = self.cols.get_mut(c) else { return false };
        let Some(id) = col.store.at(pos) else { return false };
        col.list.cursor = pos;
        let same = col.sel == Some(id) && active == c && !col.is_multi() && (has_child || has_info || !col.is_folder_at(pos));
        if same {
            return false;
        }
        col.sel = Some(id);
        let moved = self.active != c;
        self.apply_selection(c, pos);
        if moved {
            self.after_active_change();
        }
        true
    }

    // ---- multi-selection ------------------------------------------------------------

    /// Makes `c` the active column holding its single selection in `multi`
    /// (the anchor of a Shift range, the first row of a Ctrl toggle).
    fn prepare_multi(&mut self, c: usize) -> bool {
        if c >= self.cols.len() || self.cols[c].state != State::Done {
            return false;
        }
        if self.active != c {
            self.set_active(c);
        }
        let col = &mut self.cols[c];
        if !col.is_multi() {
            match col.sel_pos() {
                Some(p) => col.multi.single(&col.store, p),
                None => col.multi.clear(),
            }
        }
        true
    }

    /// Shift+click / Shift+arrow: the rows from the anchor to `pos`.
    pub fn extend_to(&mut self, c: usize, pos: usize) -> bool {
        if !self.prepare_multi(c) || self.cols[c].store.at(pos).is_none() {
            return false;
        }
        let col = &mut self.cols[c];
        col.multi.extend(&col.store, pos);
        col.list.cursor = pos;
        self.after_multi(c);
        true
    }

    /// Ctrl+click / Ctrl+Space: row `pos` joins or leaves the selection.
    pub fn toggle_at(&mut self, c: usize, pos: usize) -> bool {
        if !self.prepare_multi(c) || self.cols[c].store.at(pos).is_none() {
            return false;
        }
        let col = &mut self.cols[c];
        col.multi.toggle(&col.store, pos);
        col.list.cursor = pos;
        self.after_multi(c);
        true
    }

    /// Ctrl+A: every shown row of the active column.
    pub fn select_all(&mut self) -> bool {
        let c = self.active;
        if !self.prepare_multi(c) {
            return false;
        }
        let col = &mut self.cols[c];
        col.multi.select_all(&col.store);
        if col.multi.focus.is_none() {
            col.multi.focus = col.sel.or_else(|| col.store.at(0));
        }
        if let Some(p) = col.multi.focus.and_then(|f| col.store.view().iter().position(|v| *v == f)) {
            col.list.cursor = p;
        }
        self.after_multi(c);
        true
    }

    /// Esc on a multi-selection: back to the focused row alone. False when nothing was multi.
    pub fn collapse_selection(&mut self) -> bool {
        let a = self.active;
        let Some(col) = self.cols.get(a) else { return false };
        if !col.is_multi() {
            return false;
        }
        let pos = col.sel_pos().unwrap_or(col.list.cursor.min(col.shown().saturating_sub(1)));
        self.select_pos(a, pos);
        true
    }

    /// The multi set changed: the focus row is `sel`, one row is a plain selection, none clears it.
    fn after_multi(&mut self, c: usize) {
        let col = &mut self.cols[c];
        let n = col.multi.count();
        let chosen: Vec<usize> = if n <= 2 { col.store.view().iter().enumerate().filter(|(_, id)| col.multi.contains(**id)).map(|(p, _)| p).collect() } else { Vec::new() };
        if n >= 2 {
            let cursor_id = col.store.at(col.list.cursor).filter(|id| col.multi.contains(*id));
            col.sel = cursor_id.or(col.multi.focus.filter(|f| col.multi.contains(*f))).or_else(|| col.store.view().iter().copied().find(|id| col.multi.contains(*id)));
            col.reveal_row = true;
            self.cols.truncate(c + 1);
            self.info = None;
            self.reveal = true;
            self.dirty = true;
            self.watch_dirty = true;
        } else if n == 1 {
            let pos = chosen[0];
            col.multi.single(&col.store, pos);
            col.list.cursor = pos;
            col.sel = col.store.at(pos);
            col.reveal_row = true;
            self.apply_selection(c, pos);
        } else {
            col.sel = None;
            self.cols.truncate(c + 1);
            self.info = None;
            self.dirty = true;
            self.watch_dirty = true;
        }
    }

    /// Rows selected in the active column.
    pub fn sel_count(&self) -> usize {
        self.active_col().map_or(0, Column::sel_count)
    }

    /// Bytes of the selected files (folders count 0, like Details).
    pub fn sel_bytes(&self) -> u128 {
        let Some(c) = self.active_col() else { return 0 };
        if c.is_multi() {
            return c.multi.total_size();
        }
        c.sel.and_then(|id| c.store.record(id)).filter(|r| r.flags & FOLDER == 0).map_or(0, |r| u128::from(r.size))
    }

    /// Full paths of the selected rows of the active column, in view order.
    pub fn selected_paths(&self) -> Vec<String> {
        let Some(c) = self.active_col() else { return Vec::new() };
        if c.is_multi() {
            return c.store.view().iter().filter(|id| c.multi.contains(**id)).filter_map(|id| c.store.name(*id).map(|n| nav::join(&c.path, &utf8(n)))).collect();
        }
        self.selected_path().into_iter().collect()
    }

    // ---- where the columns were painted ---------------------------------------------------

    /// Column and row under the point (x, y), from the last painted frame.
    pub fn hit_row(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let h = self.hits.iter().find(|h| h.rows.contains_pt(tsubame_ui::Pt { x, y }))?;
        let col = self.cols.get(h.col)?;
        let pos = col.list.row_at_offset(y - h.rows.y);
        (pos < col.shown()).then_some((h.col, pos))
    }

    /// The column whose rows area holds the point (a drop on empty space lands in its folder).
    pub fn hit_col(&self, x: f32, y: f32) -> Option<usize> {
        self.hits.iter().find(|h| h.rows.contains_pt(tsubame_ui::Pt { x, y })).map(|h| h.col)
    }

    /// Screen rect of row `pos` of column `c` (clipped to the rows area).
    pub fn row_rect(&self, c: usize, pos: usize) -> Option<Rect> {
        let h = self.hits.iter().find(|h| h.col == c)?;
        let col = self.cols.get(c)?;
        let top = h.rows.y + col.list.row_top(pos) as f32;
        let r = Rect { x: h.rows.x, y: top, w: h.rows.w, h: col.list.row_h };
        (r.y + r.h > h.rows.y && r.y < h.rows.y + h.rows.h).then_some(r)
    }

    /// The rows area of column `c` as painted.
    pub fn col_rows(&self, c: usize) -> Option<Rect> {
        self.hits.iter().find(|h| h.col == c).map(|h| h.rows)
    }

    // ---- after a rename ----------------------------------------------------------------------

    /// Select the entry `name` of `folder` once it appears (a rename just made it).
    pub fn want_pick(&mut self, folder: &str, name: &str) {
        self.pick = Some((folder.to_string(), name.to_string(), Instant::now()));
        self.dirty = true;
    }

    fn try_pick(&mut self) -> bool {
        let Some((folder, name, since)) = self.pick.clone() else { return false };
        if since.elapsed() > PICK_FOR {
            self.pick = None;
            return false;
        }
        let Some(ci) = self.cols.iter().position(|c| same_path(&c.path, &folder) && c.state == State::Done) else { return false };
        let w: Vec<u16> = name.encode_utf16().collect();
        let Some(id) = self.cols[ci].store.find(&w) else { return false };
        let Some(pos) = self.cols[ci].store.view().iter().position(|v| *v == id) else { return false };
        self.pick = None;
        // not `select_pos`: a rename keeps the entry id, which that call treats as "already
        // selected" and leaves the info column on the old name
        let moved = self.active != ci;
        self.cols[ci].list.cursor = pos;
        self.cols[ci].sel = self.cols[ci].store.at(pos);
        self.apply_selection(ci, pos);
        if moved {
            self.after_active_change();
        }
        self.cols[ci].reveal_row = true;
        true
    }

    // ---- a typed path that fails -------------------------------------------------------------

    /// A `goto` built new columns and the last one cannot be listed: the folder
    /// the user was in and the reason. None while it loads or when it worked.
    pub fn failed_goto(&self) -> Option<(String, String)> {
        let prev = self.goto_from.as_ref()?;
        let last = self.cols.last()?;
        match &last.state {
            State::Denied(msg) if last.published.is_none() => Some((prev.clone(), msg.clone())),
            _ => None,
        }
    }

    /// The failed `goto` was handled (or the user moved on).
    pub fn clear_goto_from(&mut self) {
        self.goto_from = None;
    }

    /// Column `c` has the selection at `pos`: it is the active column, the
    /// columns after it go, a folder gets its child column, a file its info.
    fn apply_selection(&mut self, c: usize, pos: usize) {
        self.active = c;
        self.goto_from = None;
        self.cols.truncate(c + 1);
        self.info = None;
        if let Some(col) = self.cols.get_mut(c) {
            col.multi.single(&col.store, pos);
            col.multi.focus = col.sel;
        }
        let Some(col) = self.cols.get(c) else { return };
        let Some(id) = col.store.at(pos) else { return };
        let (Some(rec), Some(name)) = (col.store.record(id).copied(), col.store.name(id).map(utf8)) else { return };
        let path = nav::join(&col.path, &name);
        if rec.flags & FOLDER != 0 {
            let link = rec.flags & REPARSE != 0;
            let mut child = Column::new(path.clone(), if link { State::Link } else { State::Loading }, self.hide, self.sort, 0.0);
            child.is_link = link;
            self.cols.push(child);
            let ci = self.cols.len() - 1;
            if link {
                self.cols[ci].target_rx = Some(spawn_link_target(path, self.wake.clone()));
            } else {
                self.start_load(ci);
            }
            self.enforce_cap();
        } else {
            self.info = Some(Info { path, name, size: rec.size, mtime: rec.modified, flags: rec.flags, excerpt: Excerpt::Off });
            self.refresh_info();
        }
        self.reveal = true;
        self.dirty = true;
        self.watch_dirty = true;
    }

    /// More than `MAX_COLS` live columns: the leftmost goes (the cue counts it).
    fn enforce_cap(&mut self) {
        while self.cols.len() > MAX_COLS {
            self.cols.remove(0);
            self.dropped += 1;
            self.active = self.active.saturating_sub(1);
            if self.filter_col > 0 {
                self.filter_col -= 1;
            }
        }
    }

    fn after_active_change(&mut self) {
        // the filter follows the active column
        self.apply_filter_to_active();
        self.typed.clear();
    }

    /// Right / Enter on a folder: the selected folder's column becomes the active one.
    pub fn descend(&mut self) -> Activated {
        let a = self.active;
        let Some(sel) = self.cols.get(a).and_then(|c| c.sel_pos()) else { return Activated::Nothing };
        if !self.cols[a].is_folder_at(sel) {
            return match self.selected() {
                Some((p, false)) => Activated::Open(p),
                _ => Activated::Nothing,
            };
        }
        if !self.has_child() {
            return Activated::Nothing;
        }
        let ci = a + 1;
        if self.cols[ci].state == State::Link {
            self.follow(ci);
        }
        self.active = ci;
        let col = &mut self.cols[ci];
        if col.sel.is_none() {
            if col.published.is_some() {
                col.sel = col.store.at(0);
                col.reveal_row = true;
                col.list.cursor = 0;
                let first = col.sel.is_some();
                if first {
                    self.apply_selection(ci, 0);
                }
            } else {
                col.want_first = true;
            }
        } else if let Some(p) = col.sel_pos() {
            // coming back into a column that remembers its selection
            col.list.cursor = p;
            col.reveal_row = true;
            self.apply_selection(ci, p);
        }
        self.reveal = true;
        self.dirty = true;
        self.after_active_change();
        Activated::Entered
    }

    /// Left / Backspace: the parent column becomes the active one. Past the
    /// leftmost live column an ancestor that was dropped comes back.
    pub fn ascend(&mut self) -> bool {
        if self.active > 0 {
            self.active -= 1;
            self.goto_from = None;
            // the column we left stays as the child column of the folder it came from
            let a = self.active;
            if let Some(p) = self.cols[a].sel_pos() {
                self.cols[a].list.cursor = p;
                self.cols[a].reveal_row = true;
                if self.cols.len() <= a + 1 {
                    self.apply_selection(a, p);
                }
            }
            self.info = None;
            self.reveal = true;
            self.dirty = true;
            self.after_active_change();
            return true;
        }
        if self.dropped > 0
            && let Some(first) = self.cols.first()
            && let Some(parent) = nav::parent(&first.path)
        {
            let name = base_name(&first.path);
            let mut c = Column::new(parent, State::Loading, self.hide, self.sort, 0.0);
            c.want_sel = Some(name);
            self.cols.insert(0, c);
            self.dropped -= 1;
            self.start_load(0);
            // the cap: the rightmost column (the farthest preview) goes
            while self.cols.len() > MAX_COLS {
                self.cols.pop();
            }
            self.active = 0;
            self.reveal = true;
            self.dirty = true;
            self.watch_dirty = true;
            self.after_active_change();
            return true;
        }
        false
    }

    /// A click on column `i`: it becomes the active one (its child column stays).
    pub fn set_active(&mut self, i: usize) {
        if i >= self.cols.len() || i == self.active {
            return;
        }
        self.goto_from = None;
        self.active = i;
        if let Some(p) = self.cols[i].sel_pos() {
            self.cols[i].list.cursor = p;
            if self.cols.len() <= i + 1 {
                self.apply_selection(i, p);
            }
        } else {
            self.cols.truncate(i + 1);
        }
        self.info = None;
        if self.cols.len() <= i + 1
            && let Some(p) = self.cols[i].sel_pos()
        {
            self.apply_selection(i, p);
        }
        self.reveal = true;
        self.dirty = true;
        self.after_active_change();
    }

    /// The cue's click when columns were dropped: the leftmost live column's parent comes back.
    pub fn ascend_to_dropped(&mut self) {
        if self.dropped > 0 {
            self.active = 0;
            self.ascend();
        }
    }

    /// Follows the junction or symlink column `ci` (an explicit action only).
    pub fn follow(&mut self, ci: usize) {
        let ancestors: Vec<String> = self.cols.iter().take(ci).map(|c| c.path.clone()).collect();
        let wake = self.wake.clone();
        let Some(c) = self.cols.get_mut(ci) else { return };
        if c.state != State::Link {
            return;
        }
        c.state = State::Loading;
        c.started = Instant::now();
        c.loading_due = false;
        let path = c.path.clone();
        let (tx, rx) = channel();
        let w2 = wake.clone();
        let spawned = std::thread::Builder::new().name("tsubame-miller-loop".into()).spawn(move || {
            let me = tsubame_win::fileid::file_id(&path);
            let hit = me.and_then(|m| ancestors.into_iter().find(|a| tsubame_win::fileid::file_id(a) == Some(m)));
            let _ = tx.send(hit);
            w2();
        });
        if spawned.is_ok() {
            c.check = Some(rx);
            // the skeleton's wake
            let _ = std::thread::Builder::new().name("tsubame-miller-delay".into()).stack_size(64 * 1024).spawn(move || {
                std::thread::sleep(LOADING_AFTER + Duration::from_millis(5));
                wake();
            });
        } else {
            self.start_load(ci);
        }
        self.dirty = true;
    }

    /// Enter or a double click on row `pos` of column `c`.
    pub fn activate(&mut self, c: usize, pos: usize) -> Activated {
        if self.cols.get(c).is_none() {
            return Activated::Nothing;
        }
        self.select_pos(c, pos);
        if self.cols.get(c).is_some_and(|col| col.is_folder_at(pos)) {
            return self.descend();
        }
        match self.selected() {
            Some((p, false)) => Activated::Open(p),
            _ => Activated::Nothing,
        }
    }

    /// Retry for a denied, cancelled or looping column, or Follow for a link card.
    pub fn retry(&mut self, i: usize) {
        let Some(c) = self.cols.get_mut(i) else { return };
        match c.state {
            State::Denied(_) | State::Cancelled => {
                c.published = None;
                self.start_load(i);
            }
            State::Link => self.follow(i),
            _ => {}
        }
        self.dirty = true;
    }

    /// Esc: stops every first listing that is still running. True when one was.
    pub fn cancel_loading(&mut self) -> bool {
        let mut any = false;
        for c in &mut self.cols {
            if c.published.is_none() && (c.load.is_some() || c.check.is_some()) {
                c.load = None;
                c.check = None;
                c.state = State::Cancelled;
                any = true;
            }
        }
        if any {
            self.status = Some("Listing cancelled. Retry reads the folder again.".into());
            self.dirty = true;
        }
        any
    }

    /// Letters in the active column: the next entry whose name starts with what was typed.
    pub fn type_ahead(&mut self, ch: char) -> Option<usize> {
        let now = Instant::now();
        if self.typed_at.is_none_or(|t| now.duration_since(t) > TYPE_AHEAD) {
            self.typed.clear();
        }
        self.typed_at = Some(now);
        self.typed.extend(ch.to_lowercase());
        let typed: Vec<char> = self.typed.chars().collect();
        let cycling = typed.len() > 1 && typed.iter().all(|x| *x == typed[0]);
        let want: Vec<char> = if cycling { vec![typed[0]] } else { typed };
        let col = self.active_col()?;
        let n = col.shown();
        if n == 0 {
            return None;
        }
        let from = col.list.cursor;
        let start = if (self.typed.chars().count() == 1 || cycling) && col.sel.is_some() { from + 1 } else { from };
        for k in 0..n {
            let j = (start + k) % n;
            if let Some(name) = col.entry_name(j) {
                let mut it = name.chars().map(lower);
                if want.iter().all(|w| it.next() == Some(*w)) {
                    return Some(j);
                }
            }
        }
        None
    }

    // ---- sort, hide, filter, visibility ----------------------------------------------

    /// The tab's sort or hide rule changed: every listing follows it.
    pub fn set_rules(&mut self, sort: Sort, hide: u16) {
        if sort == self.sort && hide == self.hide {
            return;
        }
        self.sort = sort;
        self.hide = hide;
        for c in &mut self.cols {
            c.store.set_hide(hide);
            c.store.sort(sort);
            if let Some(p) = c.sel_pos() {
                c.list.cursor = p;
                c.reveal_row = true;
            }
        }
        self.dirty = true;
    }

    /// The filter box's text goes to the active column.
    pub fn set_filter(&mut self, text: &str) {
        if self.filter == text && self.filter_col == self.active {
            return;
        }
        self.filter = text.to_string();
        self.apply_filter_to_active();
        self.dirty = true;
    }

    fn apply_filter_to_active(&mut self) {
        let old = self.filter_col;
        if old != self.active
            && let Some(c) = self.cols.get_mut(old)
        {
            c.store.filter("", FilterMode::Substring);
            if let Some(p) = c.sel_pos() {
                c.list.cursor = p;
            }
        }
        self.filter_col = self.active;
        let (text, mode) = (self.filter.clone(), filter_mode(&self.filter));
        if let Some(c) = self.cols.get_mut(self.active) {
            c.store.filter(&text, mode);
            let sel = c.sel_pos();
            c.list.cursor = sel.unwrap_or(0).min(c.shown().saturating_sub(1));
            if c.sel.is_some() && sel.is_none() {
                // the filter hides the selected row: the selection stays, no row shows it
            }
        }
    }

    /// The tab is shown (watchers run) or hidden (they stop).
    pub fn set_visible(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.watch_dirty = true;
        } else {
            for c in &mut self.cols {
                c.watch = None;
            }
        }
    }

    // ---- watchers -----------------------------------------------------------------

    fn rebalance_watchers(&mut self) {
        let wake = self.wake.clone();
        for c in &mut self.cols {
            if c.published.is_some() && !c.path.starts_with("\\\\") {
                if c.watch.is_none() && !matches!(c.state, State::Denied(_)) {
                    c.watch = DirWatcher::spawn(&c.path, WatchOptions::default(), wake.clone()).ok();
                }
            } else {
                c.watch = None;
            }
        }
    }

    fn pump_watch(&mut self, i: usize) -> bool {
        let mut relist = false;
        let mut gone = false;
        let mut batches: Vec<watch::Batch> = Vec::new();
        let Some(c) = self.cols.get_mut(i) else { return false };
        if let Some((_, rx)) = &c.watch {
            let mut end = false;
            while let Ok(m) = rx.try_recv() {
                match m {
                    watch::Message::Batch(b) => batches.push(b),
                    watch::Message::Overflow => {
                        batches.clear();
                        relist = true;
                    }
                    watch::Message::Unreliable(_) => {}
                    watch::Message::Gone(Gone::Unavailable) | watch::Message::Failed(_) => end = true,
                    watch::Message::Gone(Gone::Deleted | Gone::Renamed) => {
                        end = true;
                        gone = true;
                    }
                }
            }
            if end {
                c.watch = None;
            }
        }
        if c.load.is_some() && c.published.is_some() {
            // a refresh runs: it will see these changes
            batches.clear();
        }
        let dpath = std::path::PathBuf::from(&c.path);
        let mut changed = false;
        for b in batches {
            if b.len() > WATCH_INLINE {
                relist = true;
                break;
            }
            if let Some(c) = self.cols.get_mut(i) {
                changed |= watch::apply_batch(&mut c.store, Some(&mut c.multi), &dpath, &b).map(|s| s.changed()).unwrap_or(false);
            }
        }
        if changed {
            self.prune_dead(i);
        }
        if gone {
            if let Some(c) = self.cols.get_mut(i) {
                c.state = State::Denied("This folder was moved or deleted.".into());
                c.store.clear();
                c.published = None;
                c.sel = None;
            }
            self.cols.truncate(i + 1);
            if self.active > i {
                self.active = i;
            }
            self.info = None;
            changed = true;
        } else if relist {
            self.relist(i);
        }
        changed
    }

    /// A removed entry takes its selection (and the columns after it) with it.
    fn prune_dead(&mut self, i: usize) {
        let Some(c) = self.cols.get_mut(i) else { return };
        if let Some(id) = c.sel
            && !c.store.is_live(id)
        {
            c.sel = None;
            self.cols.truncate(i + 1);
            if self.info.is_some() || self.active == i {
                self.info = None;
            }
        } else if i == self.active && self.info.is_some() {
            self.refresh_info();
        }
        if let Some(c) = self.cols.get_mut(i) {
            c.list.cursor = c.list.cursor.min(c.shown().saturating_sub(1));
        }
        self.dirty = true;
    }

    // ---- the info column --------------------------------------------------------------

    /// Re-reads the selected file's metadata from the listing and asks for its excerpt.
    fn refresh_info(&mut self) {
        let Some(info) = self.info.as_ref() else { return };
        let path = info.path.clone();
        // metadata may have changed (a refresh): read the active column's record again
        if let Some(c) = self.cols.get(self.active)
            && let Some(id) = c.sel
            && let Some(r) = c.store.record(id).copied()
            && let Some(i) = self.info.as_mut()
        {
            i.size = r.size;
            i.mtime = r.modified;
            i.flags = r.flags;
        }
        if !self.preview_on {
            if let Some(i) = self.info.as_mut() {
                i.excerpt = Excerpt::Off;
            }
            return;
        }
        let wake = self.wake.clone();
        let ex = self.excerpter.get_or_insert_with(|| Excerpter::spawn(wake));
        self.excerpt_gen += 1;
        let _ = ex.tx.send((self.excerpt_gen, path));
        if let Some(i) = self.info.as_mut() {
            i.excerpt = Excerpt::Loading;
        }
    }

    fn pump_excerpt(&mut self) -> bool {
        let Some(ex) = self.excerpter.as_ref() else { return false };
        let mut latest: Option<(u64, Excerpt)> = None;
        while let Ok(r) = ex.rx.try_recv() {
            latest = Some(r);
        }
        match latest {
            Some((g, e)) if g == self.excerpt_gen => {
                if let Some(i) = self.info.as_mut() {
                    i.excerpt = e;
                }
                true
            }
            _ => false,
        }
    }

    /// Drops the excerpt worker (the extension is off or the model goes).
    #[allow(dead_code)]
    pub fn release_worker(&mut self) {
        self.excerpter = None;
    }
}

/// `*` and `?` make a glob, anything else a substring (the same rule as Details).
fn filter_mode(text: &str) -> FilterMode {
    if text.contains(['*', '?']) { FilterMode::Glob } else { FilterMode::Substring }
}

/// "Attributes" text of a record's flags.
pub fn attr_text(flags: u16) -> String {
    let mut v: Vec<&str> = Vec::new();
    if flags & READONLY != 0 {
        v.push("Read-only");
    }
    if flags & HIDDEN != 0 {
        v.push("Hidden");
    }
    if flags & SYSTEM != 0 {
        v.push("System");
    }
    if flags & REPARSE != 0 {
        v.push("Link");
    }
    if v.is_empty() { "Normal".to_string() } else { v.join(", ") }
}

/// The lazy worker for text excerpts: latest request wins, idle in `recv`.
struct Excerpter {
    tx: Sender<(u64, String)>,
    rx: Receiver<(u64, Excerpt)>,
}

impl Excerpter {
    fn spawn(wake: Wake) -> Excerpter {
        let (tx, req_rx) = channel::<(u64, String)>();
        let (out_tx, rx) = channel::<(u64, Excerpt)>();
        let _ = std::thread::Builder::new().name("tsubame-miller-excerpt".into()).stack_size(256 * 1024).spawn(move || {
            while let Ok(mut req) = req_rx.recv() {
                while let Ok(newer) = req_rx.try_recv() {
                    req = newer;
                }
                let e = read_excerpt(&req.1);
                if out_tx.send((req.0, e)).is_err() {
                    break;
                }
                wake();
            }
        });
        Excerpter { tx, rx }
    }
}

/// The first lines of a text file (at most 8 KiB read), or why there are none.
pub fn read_excerpt(path: &str) -> Excerpt {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return Excerpt::Failed };
    let mut buf = vec![0u8; EXCERPT_BYTES];
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return Excerpt::Failed,
        }
    }
    excerpt_of(&buf[..n])
}

/// Text lines of `bytes` (limits above), or `NoText` for binary data.
pub fn excerpt_of(bytes: &[u8]) -> Excerpt {
    if bytes.is_empty() {
        return Excerpt::Text(String::new());
    }
    if bytes.contains(&0) {
        return Excerpt::NoText;
    }
    let s = String::from_utf8_lossy(bytes);
    let bad = s.chars().filter(|c| *c == '\u{FFFD}').count();
    // a cut multi-byte tail is one replacement char; more means this is not text
    if bad > 2 && bad * 50 > s.chars().count() {
        return Excerpt::NoText;
    }
    let mut out = String::new();
    for (k, line) in s.lines().take(EXCERPT_LINES).enumerate() {
        if k > 0 {
            out.push('\n');
        }
        let line = line.replace('\t', "    ");
        if line.chars().count() > EXCERPT_COLS {
            out.extend(line.chars().take(EXCERPT_COLS));
        } else {
            out.push_str(&line);
        }
    }
    Excerpt::Text(out)
}

/// Reads where link `path` points, on a worker.
fn spawn_link_target(path: String, wake: Wake) -> Receiver<Option<String>> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new().name("tsubame-miller-link".into()).stack_size(64 * 1024).spawn(move || {
        let t = std::fs::read_link(&path).ok().map(|t| tsubame_win::fileid::strip_verbatim(&t.to_string_lossy()));
        let _ = tx.send(t);
        wake();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tsubame_core::Column as SortCol;

    fn sort() -> Sort {
        Sort { column: SortCol::Name, descending: false, folders_first: true }
    }

    fn wake() -> Wake {
        Arc::new(|| {})
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-miller-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// root/{a/{a1/{deep.txt}, a2, f.txt}, b/{b.txt}, empty/, z.txt}
    fn fixture(name: &str) -> PathBuf {
        let d = temp(name);
        std::fs::create_dir_all(d.join("a").join("a1")).unwrap();
        std::fs::create_dir_all(d.join("a").join("a2")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        std::fs::create_dir_all(d.join("empty")).unwrap();
        std::fs::write(d.join("a").join("a1").join("deep.txt"), b"x").unwrap();
        std::fs::write(d.join("a").join("f.txt"), b"hello\nworld\n").unwrap();
        std::fs::write(d.join("b").join("b.txt"), b"x").unwrap();
        std::fs::write(d.join("z.txt"), b"x").unwrap();
        d
    }

    fn settle(m: &mut MillerModel) {
        let t0 = Instant::now();
        loop {
            m.pump(false);
            if !m.loading() {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "listing did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        m.pump(false);
    }

    fn model(root: &std::path::Path) -> MillerModel {
        let mut m = MillerModel::new(root.to_str().unwrap(), wake(), sort(), 0, DEFAULT_W);
        settle(&mut m);
        m
    }

    fn names(m: &MillerModel, c: usize) -> Vec<String> {
        let col = m.col(c).unwrap();
        (0..col.shown()).map(|i| col.entry_name(i).unwrap()).collect()
    }

    fn pos_of(m: &MillerModel, c: usize, n: &str) -> usize {
        names(m, c).iter().position(|x| x == n).unwrap_or_else(|| panic!("no {n} in {:?}", names(m, c)))
    }

    #[test]
    fn chain_walks_up_to_the_drive() {
        assert_eq!(chain("C:\\a\\b"), ["C:\\", "C:\\a", "C:\\a\\b"]);
        assert_eq!(chain("C:\\"), ["C:\\"]);
    }

    #[test]
    fn titles_are_folder_names_and_drives() {
        let c = Column::new("C:\\a\\b".into(), State::Done, 0, sort(), 0.0);
        assert_eq!(c.title(), "b");
        let r = Column::new("C:\\".into(), State::Done, 0, sort(), 0.0);
        assert_eq!(r.title(), "C:");
    }

    #[test]
    fn the_model_opens_on_the_path_with_ancestors_as_path_rows() {
        let d = fixture("open");
        let sub = d.join("a");
        let m = model(&sub);
        let n = m.active + 1;
        assert!(n >= 2, "ancestors are columns");
        assert!(m.len() >= n, "the first entry may add a child column after the active one");
        assert_eq!(m.active_path(), sub.to_str().unwrap());
        assert_eq!(names(&m, n - 1), ["a1", "a2", "f.txt"]);
        // the column before it has the folder selected as a path row
        let parent = m.col(n - 2).unwrap();
        let p = parent.sel_pos().expect("path row");
        assert_eq!(parent.entry_name(p).unwrap(), "a");
        let last = m.col(n - 1).unwrap();
        assert_eq!(last.sel_pos(), Some(0), "the first entry of the active column is selected");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn selecting_a_folder_adds_its_children_as_the_next_column() {
        let d = fixture("child");
        let mut m = model(&d);
        let a = m.active;
        let p = pos_of(&m, a, "a");
        // the active column opens with its first entry (the folder "a") already selected
        assert!(!m.select_pos(a, p), "already selected");
        assert_eq!(m.len(), a + 2);
        settle(&mut m);
        assert_eq!(names(&m, a + 1), ["a1", "a2", "f.txt"]);
        assert_eq!(m.active, a, "selecting does not move the keyboard");
        assert!(m.has_child());
        // another folder replaces the child
        assert!(m.select_pos(a, pos_of(&m, a, "b")));
        settle(&mut m);
        assert_eq!(names(&m, a + 1), ["b.txt"]);
        assert_eq!(m.len(), a + 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn descend_makes_the_child_active_and_selects_its_first_entry() {
        let d = fixture("descend");
        let mut m = model(&d);
        let a = m.active;
        m.select_pos(a, pos_of(&m, a, "a"));
        assert_eq!(m.descend(), Activated::Entered);
        assert_eq!(m.active, a + 1);
        settle(&mut m);
        let col = m.col(a + 1).unwrap();
        assert_eq!(col.entry_name(col.sel_pos().expect("first selected")).unwrap(), "a1");
        // a1 is a folder: its child column follows
        assert_eq!(m.len(), a + 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn enter_on_a_file_opens_it_and_right_on_a_file_does_nothing() {
        let d = fixture("file");
        let mut m = model(&d);
        let a = m.active;
        m.select_pos(a, pos_of(&m, a, "z.txt"));
        assert!(m.shows_info());
        assert_eq!(m.len(), a + 1, "a file has no child column");
        assert_eq!(m.descend(), Activated::Open(d.join("z.txt").to_string_lossy().into_owned()));
        assert_eq!(m.active, a, "Right does not move into a file");
        assert!(matches!(m.activate(a, pos_of(&m, a, "z.txt")), Activated::Open(_)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ascend_keeps_the_column_we_left_as_the_child() {
        let d = fixture("ascend");
        let mut m = model(&d);
        let a = m.active;
        m.select_pos(a, pos_of(&m, a, "a"));
        m.descend();
        settle(&mut m);
        assert_eq!(m.active, a + 1);
        assert!(m.ascend());
        assert_eq!(m.active, a);
        assert!(m.has_child(), "a is still selected: its column stays");
        assert_eq!(m.active_path(), d.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_live_column_cap_drops_the_leftmost_and_ascend_brings_it_back() {
        let d = temp("deep");
        let mut p = d.clone();
        for i in 0..14 {
            p = p.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&p).unwrap();
        let mut m = model(&p);
        assert_eq!(m.len(), MAX_COLS);
        assert!(m.dropped > 0, "ancestors beyond the cap are counted, not held");
        let dropped = m.dropped;
        // walking left past the leftmost live column re-lists one ancestor
        while m.active > 0 {
            assert!(m.ascend());
        }
        assert!(m.ascend(), "a dropped ancestor returns");
        assert_eq!(m.dropped, dropped - 1);
        assert_eq!(m.len(), MAX_COLS, "still at most 12 live");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn descending_past_twelve_columns_drops_from_the_left() {
        let d = temp("deepdown");
        let mut p = d.clone();
        for i in 0..11 {
            p = p.join(format!("n{i}"));
        }
        std::fs::create_dir_all(p.join("last").join("more").join("evenmore")).unwrap();
        let mut m = model(&p);
        for _ in 0..4 {
            let a = m.active;
            m.select_pos(a, 0);
            settle(&mut m);
            m.descend();
            settle(&mut m);
            assert!(m.len() <= MAX_COLS, "{} columns", m.len());
        }
        assert!(m.dropped > 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_folder_lists_empty_and_a_missing_one_is_denied() {
        let d = fixture("states");
        let mut m = model(&d);
        let a = m.active;
        m.select_pos(a, pos_of(&m, a, "empty"));
        settle(&mut m);
        let c = m.col(a + 1).unwrap();
        assert_eq!(c.state, State::Done);
        assert_eq!(c.shown(), 0);
        // a folder that cannot be listed: a card with the message, and Retry lists again
        let mut m = model(&d.join("nope"));
        let last = m.len() - 1;
        let c = m.col(last).unwrap();
        assert!(matches!(c.state, State::Denied(_)), "{:?}", c.state);
        assert!(m.col(last - 1).is_some_and(|p| p.state == State::Done), "the ancestors list fine");
        m.retry(last);
        assert!(m.col(last).unwrap().is_loading());
        settle(&mut m);
        assert!(matches!(m.col(last).unwrap().state, State::Denied(_)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_deep_missing_path_ends_in_one_denied_card() {
        let d = fixture("deepmiss");
        let m = model(&d.join("nope").join("deeper").join("deepest"));
        let first_bad = (0..m.len()).find(|&i| matches!(m.col(i).unwrap().state, State::Denied(_))).expect("one column is denied");
        assert_eq!(m.len(), first_bad + 1, "the columns after the first failure are dropped");
        let last = m.len() - 1;
        assert!(matches!(m.col(last).unwrap().state, State::Denied(_)));
        assert_eq!(m.active, last, "the keyboard is on the card, so Retry works");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn esc_cancels_a_running_first_listing() {
        let d = fixture("cancel");
        let mut m = MillerModel::new(d.to_str().unwrap(), wake(), sort(), 0, DEFAULT_W);
        assert!(m.loading());
        assert!(m.cancel_loading());
        assert!(m.cols().iter().all(|c| c.state == State::Cancelled));
        assert!(!m.loading());
        m.retry(m.len() - 1);
        settle(&mut m);
        assert_eq!(m.col(m.len() - 1).unwrap().state, State::Done);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn type_ahead_moves_to_the_next_matching_name_and_cycles() {
        let d = fixture("type");
        let mut m = model(&d);
        let a = m.active;
        let j = m.type_ahead('b').expect("b");
        assert_eq!(m.col(a).unwrap().entry_name(j).unwrap(), "b");
        m.select_pos(a, j);
        // letters typed together are one prefix: "bz" matches nothing
        assert!(m.type_ahead('z').is_none());
        let mut m2 = model(&d);
        let a2 = m2.active;
        let j = m2.type_ahead('z').expect("z");
        assert_eq!(m2.col(a2).unwrap().entry_name(j).unwrap(), "z.txt");
        assert!(m2.type_ahead('q').is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn widths_clamp_to_140_480_and_reset_to_the_default() {
        let d = fixture("width");
        let mut m = model(&d);
        assert_eq!(m.width(0), 220.0);
        m.set_width(0, 20.0);
        assert_eq!(m.width(0), 140.0);
        m.set_width(0, 9999.0);
        assert_eq!(m.width(0), 480.0);
        m.set_width(0, 300.0);
        assert_eq!(m.width(0), 300.0);
        m.reset_width(0);
        assert_eq!(m.width(0), 220.0);
        m.set_default_width(333.0);
        assert_eq!(m.width(0), 333.0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn goto_an_ancestor_keeps_the_columns_and_goto_elsewhere_rebuilds_them() {
        let d = fixture("goto");
        let mut m = model(&d.join("a"));
        let n = m.len();
        let before = m.col(0).unwrap().path.clone();
        m.goto(d.to_str().unwrap());
        assert_eq!(m.len(), n, "the live columns stay");
        assert_eq!(m.active_path(), d.to_str().unwrap());
        m.goto(d.join("b").to_str().unwrap());
        settle(&mut m);
        assert_eq!(m.col(0).unwrap().path, before, "the common prefix is kept");
        assert_eq!(m.active_path(), d.join("b").to_str().unwrap());
        assert_eq!(names(&m, m.active), ["b.txt"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sort_and_filter_apply_across_the_view() {
        let d = fixture("rules");
        let mut m = model(&d);
        let a = m.active;
        m.set_rules(Sort { column: SortCol::Name, descending: true, folders_first: true }, 0);
        assert_eq!(names(&m, a), ["empty", "b", "a", "z.txt"]);
        m.set_filter("z");
        assert_eq!(names(&m, a), ["z.txt"]);
        m.set_filter("");
        assert_eq!(m.col(a).unwrap().shown(), 4);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_link_column_waits_for_an_explicit_follow() {
        // a directory symlink needs privileges on Windows: build one only when it works
        let d = fixture("link");
        let target = d.join("b");
        let link = d.join("lnk");
        if std::os::windows::fs::symlink_dir(&target, &link).is_err() {
            let _ = std::fs::remove_dir_all(&d);
            return;
        }
        let mut m = model(&d);
        let a = m.active;
        m.select_pos(a, pos_of(&m, a, "lnk"));
        assert_eq!(m.col(a + 1).unwrap().state, State::Link, "selecting never follows");
        assert!(!m.col(a + 1).unwrap().is_loading());
        m.descend();
        settle(&mut m);
        assert_eq!(m.col(a + 1).unwrap().state, State::Done);
        assert_eq!(names(&m, a + 1), ["b.txt"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_link_to_an_ancestor_is_not_followed() {
        let d = fixture("loop");
        let link = d.join("a").join("up");
        if std::os::windows::fs::symlink_dir(&d, &link).is_err() {
            let _ = std::fs::remove_dir_all(&d);
            return;
        }
        let mut m = model(&d.join("a"));
        let a = m.active;
        m.select_pos(a, pos_of(&m, a, "up"));
        m.descend();
        settle(&mut m);
        assert!(matches!(m.col(a + 1).unwrap().state, State::Loop(_)), "{:?}", m.col(a + 1).unwrap().state);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_info_excerpt_is_text_lines_or_no_text() {
        assert_eq!(excerpt_of(b"hello\nworld\n"), Excerpt::Text("hello\nworld".into()));
        assert_eq!(excerpt_of(b"a\0b"), Excerpt::NoText);
        let many: String = (0..40).map(|i| format!("line {i}\n")).collect();
        match excerpt_of(many.as_bytes()) {
            Excerpt::Text(t) => assert_eq!(t.lines().count(), EXCERPT_LINES),
            e => panic!("{e:?}"),
        }
        let long = "x".repeat(500);
        match excerpt_of(long.as_bytes()) {
            Excerpt::Text(t) => assert_eq!(t.chars().count(), EXCERPT_COLS),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn a_selected_file_gets_info_with_an_excerpt_only_when_preview_is_on() {
        let d = fixture("info");
        let mut m = model(&d.join("a"));
        let a = m.active;
        m.select_pos(a, pos_of(&m, a, "f.txt"));
        assert_eq!(m.info().unwrap().excerpt, Excerpt::Off);
        assert_eq!(m.info().unwrap().name, "f.txt");
        assert_eq!(m.info().unwrap().size, 12);
        // preview on: the excerpt arrives from the worker
        let t0 = Instant::now();
        m.pump(true);
        while !matches!(m.info().unwrap().excerpt, Excerpt::Text(_)) {
            m.pump(true);
            assert!(t0.elapsed() < Duration::from_secs(10), "excerpt: {:?}", m.info().unwrap().excerpt);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(m.info().unwrap().excerpt, Excerpt::Text("hello\nworld".into()));
        // off again: metadata only
        m.pump(false);
        assert_eq!(m.info().unwrap().excerpt, Excerpt::Off);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_column_is_small_and_attr_text_names_the_flags() {
        assert!(std::mem::size_of::<tsubame_core::EntryRecord>() == 24);
        assert_eq!(attr_text(0), "Normal");
        assert_eq!(attr_text(HIDDEN | READONLY), "Read-only, Hidden");
    }

    #[test]
    fn a_new_file_shows_up_through_the_watcher() {
        let d = fixture("watch");
        let mut m = model(&d);
        let a = m.active;
        let t0 = Instant::now();
        // the watcher arms on a worker
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(d.join("new.txt"), b"y").unwrap();
        loop {
            m.pump(false);
            if names(&m, a).iter().any(|n| n == "new.txt") {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "no watcher batch: {:?}", names(&m, a));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(m.watched_count() >= 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn shift_and_ctrl_build_a_multi_selection_that_cuts_the_child_columns() {
        let d = fixture("multi");
        let mut m = model(&d.join("a"));
        let a = m.active;
        assert_eq!(names(&m, a), ["a1", "a2", "f.txt"]);
        assert!(m.extend_to(a, 2), "shift to the last row");
        assert_eq!(m.sel_count(), 3);
        assert_eq!(m.len(), a + 1, "no child column beside a set");
        let p: Vec<String> = m.selected_paths();
        assert_eq!(p.len(), 3);
        assert!(p[0].ends_with("a1") && p[2].ends_with("f.txt"), "view order: {p:?}");
        // ctrl toggles a row out and in
        assert!(m.toggle_at(a, 1));
        assert_eq!(m.sel_count(), 2);
        assert!(!m.selected_paths().iter().any(|x| x.ends_with("a2")));
        assert!(m.toggle_at(a, 1));
        assert_eq!(m.sel_count(), 3);
        // down to one row: a plain selection again (folder: its column returns)
        assert!(m.toggle_at(a, 2));
        assert!(m.toggle_at(a, 1));
        assert_eq!(m.sel_count(), 1);
        assert!(!m.active_col().unwrap().is_multi() || m.active_col().unwrap().sel_count() == 1);
        assert!(m.selected_paths()[0].ends_with("a1"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn select_all_and_escape_collapse_to_the_focused_row() {
        let d = fixture("selall");
        let mut m = model(&d.join("a"));
        let a = m.active;
        assert!(m.select_all());
        assert_eq!(m.sel_count(), 3);
        assert_eq!(m.sel_bytes(), 12, "only f.txt has bytes; folders count 0");
        assert!(m.collapse_selection());
        assert_eq!(m.sel_count(), 1);
        assert!(!m.collapse_selection(), "nothing multi left");
        assert_eq!(m.active, a);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_plain_select_replaces_a_multi_selection() {
        let d = fixture("plainsel");
        let mut m = model(&d.join("a"));
        let a = m.active;
        m.extend_to(a, 2);
        assert_eq!(m.sel_count(), 3);
        assert!(m.select_pos(a, 1), "a click on a row of the set changes the selection");
        assert_eq!(m.sel_count(), 1);
        assert!(m.selected_paths()[0].ends_with("a2"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_multi_selection_survives_a_new_file_by_name() {
        let d = fixture("multiwatch");
        let mut m = model(&d.join("a"));
        let a = m.active;
        m.extend_to(a, 2);
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(d.join("a").join("a0.txt"), b"y").unwrap();
        let t0 = Instant::now();
        loop {
            m.pump(false);
            if names(&m, a).iter().any(|n| n == "a0.txt") {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "no watcher batch: {:?}", names(&m, a));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(m.sel_count(), 3, "the new file is not selected, the three stay");
        let p = m.selected_paths();
        assert!(p.iter().all(|x| !x.ends_with("a0.txt")), "{p:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hit_row_maps_a_point_in_the_painted_rows_to_column_and_row() {
        let d = fixture("hit");
        let mut m = model(&d.join("a"));
        let a = m.active;
        let rh = m.col(a).unwrap().list.row_h;
        assert!(rh > 0.0);
        m.hits.clear();
        m.hits.push(ColHit { col: a, rows: Rect::new(100.0, 50.0, 200.0, 300.0) });
        assert_eq!(m.hit_row(150.0, 50.0 + rh * 1.5), Some((a, 1)));
        assert_eq!(m.hit_row(150.0, 50.0 + 1.0), Some((a, 0)));
        assert_eq!(m.hit_row(150.0, 50.0 + rh * 10.0), None, "below the last row is no row");
        assert_eq!(m.hit_col(150.0, 50.0 + rh * 10.0), Some(a), "but it is the column");
        assert_eq!(m.hit_col(10.0, 60.0), None);
        let r = m.row_rect(a, 2).unwrap();
        assert_eq!((r.x, r.w), (100.0, 200.0));
        assert!((r.y - (50.0 + rh * 2.0)).abs() < 0.01);
        assert_eq!(m.col_rows(a), Some(Rect::new(100.0, 50.0, 200.0, 300.0)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn want_pick_selects_the_named_entry_on_the_next_pump() {
        let d = fixture("pick");
        let mut m = model(&d.join("a"));
        let a = m.active;
        m.want_pick(&m.col(a).unwrap().path.clone(), "f.txt");
        m.pump(false);
        assert!(m.selected_path().unwrap().ends_with("f.txt"));
        assert!(m.pick.is_none(), "one shot");
        // a name that never shows up is dropped after PICK_FOR, not kept forever
        m.want_pick(&m.col(a).unwrap().path.clone(), "ghost.txt");
        m.pump(false);
        assert!(m.pick.is_some(), "waits for the listing");
        assert!(m.selected_path().unwrap().ends_with("f.txt"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn want_pick_refreshes_the_info_column_of_an_already_selected_entry() {
        // a rename keeps the entry id: the pick must still rebuild the info for the new name
        let d = fixture("pickinfo");
        let mut m = model(&d.join("a"));
        let a = m.active;
        let folder = m.col(a).unwrap().path.clone();
        m.want_pick(&folder, "f.txt");
        m.pump(false);
        assert!(m.info.as_ref().unwrap().path.ends_with("f.txt"));
        m.info = None; // stands for the stale info of the old name
        m.want_pick(&folder, "f.txt");
        m.pump(false);
        assert!(m.pick.is_none());
        assert!(m.info.as_ref().is_some_and(|i| i.path.ends_with("f.txt")), "info rebuilt");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_goto_to_a_missing_folder_reports_the_folder_to_return_to() {
        let d = fixture("failgoto");
        let mut m = model(&d.join("a"));
        let from = m.active_path().to_string();
        assert!(m.failed_goto().is_none(), "nothing failed yet");
        let bad = d.join("nope").join("deeper");
        m.goto(bad.to_str().unwrap());
        settle(&mut m);
        let (prev, why) = m.failed_goto().expect("the last column is denied");
        assert!(same_path(&prev, &from), "{prev} vs {from}");
        assert!(!why.is_empty());
        m.clear_goto_from();
        assert!(m.failed_goto().is_none());
        // a path that works is no failure
        m.goto(d.join("b").to_str().unwrap());
        settle(&mut m);
        assert!(m.failed_goto().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
