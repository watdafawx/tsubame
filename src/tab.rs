//! One tab: its folder, history, entry store, selection, sort, filter and
//! list scroll, plus the running enumeration that feeds the store.
//!
//! Closing a tab drops it: the store is freed and the enumerator cancelled
//! (its worker sees the flag, closes the directory handle and exits).
//! Nothing here touches the disk; the enumerator worker does, and the UI
//! thread only moves finished batches into the store (`pump`).
//!
//! A visible tab (the active tab of each pane, `set_visible`) watches its
//! folder (`tsubame_win::watch`): changes arrive as coalesced batches and go
//! into the store incrementally (`EntryStore` IDs stay, so selection, cursor
//! and scroll stay on the same entries). Small batches are applied on the UI
//! thread, big ones are resolved on a short-lived thread first. Batches that
//! arrive while the listing runs wait for it. Only an overflow (records
//! lost) or a folder that came back relists. A hidden tab drops its watcher
//! and, when shown again, relists if the folder changed meanwhile. An idle
//! watcher costs no wake-ups.

use crate::nav::{self, History};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};
use tsubame_core::{EntryInput, EntryStore, FOLDER, FilterMode, HIDDEN, READONLY, REPARSE, SYSTEM, Selection, Sort};
use tsubame_ui::{BandMode, GridState, ListState};
use std::collections::VecDeque;
use std::sync::mpsc::{Sender, channel};
use tsubame_win::enumerate::{Batch, CancelToken, Done, Enumerator, Message, Wake};
use tsubame_win::watch::{self, DirWatcher, Gone, Options as WatchOptions, Unreliable};

/// Watch batches up to this many names are applied on the UI thread (each
/// name is one stat); bigger ones are resolved on a worker first.
pub const APPLY_INLINE: usize = 30;
/// Names to select after a file operation wait this long for the watcher.
const WANT_FOR: Duration = Duration::from_secs(3);

/// What a visible tab's watcher said that the view has to act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchNote {
    /// records were lost (or the folder came back): relist keeping the place
    Relist,
    /// the folder was deleted or renamed: go up
    Gone,
}

/// The directory watcher of a visible tab and what is waiting to be applied.
#[derive(Default)]
struct Watching {
    w: Option<(DirWatcher, Receiver<watch::Message>)>,
    /// batches waiting: the listing runs, or an earlier batch is resolving
    queued: VecDeque<watch::Batch>,
    /// a big batch resolving on a worker
    resolving: Option<Receiver<(watch::Batch, watch::Resolved)>>,
    /// notifications here may miss changes (network, removable): ops reload
    unreliable: bool,
    /// the folder's last-write time when the watcher stopped (hidden tab)
    mtime_at_stop: Option<std::time::SystemTime>,
    /// the tab was hidden with no watcher: relist when shown again
    stale: bool,
    note: Option<WatchNote>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    Loading,
    Done,
    /// the folder could not be listed: message for the status bar / empty state
    Failed(String),
}

/// Session restore for one tab: applied when its listing completes, because
/// the scroll row and the selected names need the entries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Restore {
    /// index of the first visible row
    pub scroll_first_row: usize,
    /// entry names to select
    pub selected: Vec<String>,
}

/// Times of the last navigation, all measured from the navigation event.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub t_nav: Instant,
    /// first batch moved into the store
    pub first_batch: Option<Duration>,
    /// Done message handled (store complete and sorted)
    pub done: Option<Duration>,
    /// a frame that showed rows was presented
    pub first_frame: Option<Duration>,
    /// the frame after Done was presented (complete + sorted on screen)
    pub final_frame: Option<Duration>,
    /// worker time inside the enumerator (not UI)
    pub worker: Option<Duration>,
    /// UI time converting and inserting batches (including incremental sort).
    pub store_cpu: Duration,
    /// UI time of the one full sort when the listing completed (`end_stream`)
    pub final_sort: Duration,
    /// UI time starting the folder watcher
    pub watch_start: Duration,
    /// frames presented until the final frame (included)
    pub frames: u32,
}

impl Timing {
    fn new() -> Timing {
        Timing {
            t_nav: Instant::now(),
            first_batch: None,
            done: None,
            first_frame: None,
            final_frame: None,
            worker: None,
            store_cpu: Duration::ZERO,
            final_sort: Duration::ZERO,
            watch_start: Duration::ZERO,
            frames: 0,
        }
    }
}

pub struct Tab {
    pub id: u64,
    /// folder shown (the history's current entry)
    pub path: String,
    pub history: History,
    pub store: EntryStore,
    pub sel: Selection,
    pub sort: Sort,
    /// text in the filter box (applied to the store)
    pub filter: String,
    pub list: ListState,
    /// scroll and cursor of the grid view (the details view keeps `list`)
    pub grid: GridState,
    /// selection before a Ctrl/Shift rubber-band started
    pub band_base: Vec<tsubame_core::EntryId>,
    /// how the active rubber-band combines with `band_base`
    pub band_mode: BandMode,
    pub load: Load,
    /// entries the enumerator delivered so far
    pub delivered: usize,
    pub timing: Timing,
    /// restored from the session: no listing runs until the tab is first shown
    pub pending: bool,
    /// restored scroll row and selection, applied when the listing completes
    pub restore: Option<Restore>,
    title_s: String,
    /// the bench's fake rows: no real files behind them
    pub synthetic: bool,
    /// the Settings tab: no folder, no listing (always `synthetic` too, so nothing
    /// lists, watches or touches files for it)
    pub settings: bool,
    token: CancelToken,
    enum_gen: u64,
    enumerator: Option<Enumerator>,
    rx: Option<Receiver<Message>>,
    archive_rx: Option<Receiver<crate::archive_ui::ArchiveListReply>>,
    /// the tab is shown in its pane (it watches its folder)
    visible: bool,
    watching: Watching,
    /// a watcher batch mutated the store since the last `take_watch_changed`
    watch_changed: bool,
    wake: Option<Wake>,
    /// names to select once they show up (after a copy / move / rename)
    want: Option<(Vec<String>, Instant)>,
    /// P4.F6: (file id, name hash) per `EntryId`, kept only while tags want
    /// them (`tags::want_fids`); empty otherwise
    pub tag_fids: Vec<(u128, u32)>,
    /// P4.F6 Tree rows: the expanded folders of this tab (`tsubame.ext.tree`)
    pub tree: Option<Box<crate::tree::TreeModel>>,
    /// the view shows this tab in Tree rows mode (rows come from `tree`)
    pub tree_on: bool,
    /// the root listing changed since the tree last flattened
    pub tree_dirty: bool,
    /// expanded folders (and the selected path) to restore when the model is built
    pub tree_seed: (Vec<String>, Vec<String>),
    /// P4.F6 Tree pane: folders only, its own root
    pub tree_pane: Option<Box<crate::tree::TreeModel>>,
    /// scroll and cursor of the Tree pane's list
    pub pane_list: ListState,
    /// P4.F6 Miller columns: the model of this tab (`tsubame.ext.miller`)
    pub miller: Option<Box<crate::miller::MillerModel>>,
    /// the view shows this tab in Miller columns mode
    pub miller_on: bool,
}

/// What `pump` found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pumped {
    /// the store or the load state changed: repaint
    pub changed: bool,
    /// this call handled the Done message
    pub done: bool,
    /// stopped at the time budget with messages still waiting: call again
    pub more: bool,
}

/// One `pump` call works at most this long, so a fast worker cannot keep the
/// UI thread from painting while a big folder streams in.
pub const PUMP_BUDGET: Duration = Duration::from_millis(6);

pub(crate) fn flags_of(attrs: u32, is_dir: bool, reparse: bool) -> u16 {
    let mut f = 0;
    if is_dir {
        f |= FOLDER;
    }
    if attrs & 0x2 != 0 {
        f |= HIDDEN;
    }
    if attrs & 0x4 != 0 {
        f |= SYSTEM;
    }
    if attrs & 0x1 != 0 {
        f |= READONLY;
    }
    if reparse {
        f |= REPARSE;
    }
    f
}

/// Which attributes hide an entry in tabs made from now on (`Settings > General`; 0 = show all).
pub static HIDE: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// The hide mask for the two "Show hidden / system files" settings.
pub fn hide_mask(show_hidden: bool, show_system: bool) -> u16 {
    (if show_hidden { 0 } else { tsubame_core::HIDDEN }) | (if show_system { 0 } else { tsubame_core::SYSTEM })
}

impl Tab {
    /// Applies a new hide rule to this tab's store (rebuilds the shown index).
    pub fn set_hide(&mut self, mask: u16) {
        if self.store.hide() == (mask & (tsubame_core::HIDDEN | tsubame_core::SYSTEM)) {
            return;
        }
        self.store.set_hide(mask);
        self.tree_dirty = true;
        self.sel.clear();
        self.reset_view();
    }

    pub fn new(id: u64, path: &str, row_h: f32) -> Tab {
        Tab {
            id,
            path: path.to_string(),
            history: History::new(path),
            store: {
                let mut s = EntryStore::new();
                s.set_hide(HIDE.load(std::sync::atomic::Ordering::Relaxed));
                s
            },
            sel: Selection::new(),
            sort: Sort { column: tsubame_core::Column::Name, descending: false, folders_first: true },
            filter: String::new(),
            list: ListState::new(row_h),
            grid: GridState::new(96.0, 118.0),
            band_base: Vec::new(),
            band_mode: BandMode::Replace,
            load: Load::Done,
            delivered: 0,
            timing: Timing::new(),
            pending: false,
            restore: None,
            title_s: Self::make_title(path),
            synthetic: false,
            settings: false,
            token: CancelToken::new(),
            enum_gen: 0,
            enumerator: None,
            rx: None,
            archive_rx: None,
            visible: false,
            watching: Watching::default(),
            watch_changed: false,
            wake: None,
            want: None,
            tag_fids: Vec::new(),
            tree: None,
            tree_on: false,
            tree_dirty: true,
            tree_seed: (Vec::new(), Vec::new()),
            tree_pane: None,
            pane_list: ListState::new(row_h),
            miller: None,
            miller_on: false,
        }
    }

    /// The single "Settings" tab.
    pub fn new_settings(id: u64, row_h: f32) -> Tab {
        let mut t = Tab::new(id, "", row_h);
        t.settings = true;
        t.synthetic = true;
        t.title_s = "Settings".to_string();
        t
    }

    /// Short title for the tab strip: the folder's own name (cached).
    pub fn title(&self) -> &str {
        &self.title_s
    }

    fn make_title(path: &str) -> String {
        nav::crumbs(path).pop().map(|c| c.label).unwrap_or_else(|| path.to_string())
    }

    /// True when this tab is browsing a read-only virtual archive (`host.zip::...`).
    pub fn is_archive(&self) -> bool {
        crate::archive_ui::is_archive_path(&self.path)
    }

    /// Consumes the flag set when the directory watcher mutated this tab's store.
    pub fn take_watch_changed(&mut self) -> bool {
        std::mem::take(&mut self.watch_changed)
    }

    /// Replaces the listing with a ready-made store (the `--bench` synthetic
    /// folder): no enumeration runs, the tab is complete at once.
    pub fn adopt_store(&mut self, label: &str, store: EntryStore) {
        self.enumerator = None;
        self.rx = None;
        self.archive_rx = None;
        self.watching = Watching::default();
        self.watch_changed = false;
        self.pending = false;
        self.restore = None;
        self.path = label.to_string();
        self.title_s = Self::make_title(label);
        self.delivered = store.len();
        self.store = store;
        self.tag_fids = Vec::new();
        self.sel = Selection::new();
        self.load = Load::Done;
        self.synthetic = true;
    }

    pub fn is_loading(&self) -> bool {
        self.load == Load::Loading
    }

    /// The wake-up the listing workers use (set by `start`).
    pub fn wake_handle(&self) -> Option<Wake> {
        self.wake.clone()
    }

    /// Lists `path` (the history is the caller's business): drops the old
    /// listing, cancels its worker, starts a new one. Stale batches of the
    /// old one can never reach the store: the channel is replaced and every
    /// message carries its generation.
    pub fn start(&mut self, path: &str, wake: Wake) {
        // Miller columns list their own folders: this tab only follows the path
        if self.miller.is_some() {
            if crate::archive_ui::is_archive_path(path) {
                self.miller = None;
            } else {
                return self.start_miller(path, wake);
            }
        }
        // the same folder again (refresh): its expanded folders and selection come back
        if let Some(m) = self.tree.take()
            && path.eq_ignore_ascii_case(&self.path)
        {
            self.tree_seed = (m.expanded_paths_in(Some(&self.store)), m.selected_paths(64, Some(&self.store)));
        }
        self.enumerator = None; // cancels and detaches
        self.rx = None;
        self.archive_rx = None;
        // a new listing: the old folder's watcher and its pending batches go
        self.watching = Watching::default();
        self.watch_changed = false;
        self.want = None;
        self.wake = Some(wake.clone());
        self.pending = false;
        // a navigation is not the restored listing any more
        self.restore = None;
        self.path = path.to_string();
        self.title_s = Self::make_title(path);
        self.synthetic = false;
        self.store.clear();
        self.tag_fids = Vec::new();
        self.tree = None;
        self.tree_dirty = true;
        self.store.sort(self.sort);
        // batches append; only the first screens stay sorted until Done
        // sorts everything once (no O(n) merge per batch)
        self.store.begin_stream();
        self.sel.clear();
        self.filter.clear();
        self.reset_view();
        self.delivered = 0;
        self.load = Load::Loading;
        self.timing = Timing::new();
        self.enum_gen = self.token.next_generation();
        if crate::archive_ui::is_archive_path(path) {
            self.archive_rx = Some(crate::archive_ui::start_list(path, self.enum_gen, wake));
            return;
        }
        let (e, rx) = Enumerator::start_channel(path, self.enum_gen, self.token.clone(), wake);
        self.enumerator = Some(e);
        self.rx = Some(rx);
        // a local folder is watched from the start, so changes made while it
        // lists are not lost (they wait in `queued`); a network folder once
        // it listed (opening it can block)
        if self.visible && !path.starts_with("\\\\") {
            self.watch_start();
        }
    }

    /// Moves everything the worker has delivered into the store. UI thread.
    pub fn pump(&mut self) -> Pumped {
        let mut out = Pumped::default();
        if self.archive_rx.is_some() {
            let reply = self.archive_rx.as_ref().map(|rx| rx.try_recv());
            match reply {
                Some(Ok(msg)) => {
                    self.archive_rx = None;
                    if msg.generation == self.enum_gen {
                        self.finish_archive(msg.result);
                        out.changed = true;
                        out.done = true;
                    }
                }
                Some(Err(TryRecvError::Empty)) => {}
                Some(Err(TryRecvError::Disconnected)) => {
                    self.archive_rx = None;
                    self.store.end_stream();
                    if self.load == Load::Loading {
                        self.load = Load::Failed("The archive listing stopped unexpectedly.".into());
                        out.changed = true;
                    }
                }
                None => {}
            }
            return out;
        }
        let t0 = Instant::now();
        loop {
            if t0.elapsed() >= PUMP_BUDGET {
                out.more = true;
                break;
            }
            let Some(rx) = &self.rx else { break };
            match rx.try_recv() {
                Ok(msg) => {
                    if msg.generation() != self.enum_gen {
                        continue; // stale: from a listing this tab no longer shows
                    }
                    match msg {
                        Message::Batch(b) => {
                            let first = self.timing.first_batch.is_none();
                            self.push(&b);
                            out.changed = true;
                            if first {
                                out.more = true;
                                break; // present the first 256 rows before consuming later batches
                            }
                        }
                        Message::Done(d) => {
                            self.finish(d);
                            out.changed = true;
                            out.done = true;
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.store.end_stream();
                    if self.load == Load::Loading {
                        self.load = Load::Failed("The listing stopped unexpectedly.".into());
                        out.changed = true;
                    }
                    self.rx = None;
                    self.enumerator = None;
                    break;
                }
            }
        }
        out.changed |= self.pump_watch();
        self.tree_dirty |= out.changed;
        out
    }

    // ---- watching ------------------------------------------------------------

    /// The view says whether this tab is the one shown in its pane. Shown:
    /// it watches its folder (and relists if it changed while hidden).
    /// Hidden: the watcher goes (a hidden tab costs no handle, no buffer).
    pub fn set_visible(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        if let Some(m) = self.tree.as_mut() {
            m.set_visible(visible);
        }
        if let Some(m) = self.miller.as_mut() {
            // the model watches its own columns; this tab has no listing of its own
            m.set_visible(visible);
            return;
        }
        if self.synthetic || self.pending || self.is_archive() {
            return;
        }
        if !visible {
            if self.watching.w.take().is_some() {
                self.watching.mtime_at_stop = dir_mtime(&self.path);
                self.watching.stale = true;
            }
            self.watching.queued.clear();
            self.watching.resolving = None;
            return;
        }
        if self.load == Load::Loading {
            if !self.path.starts_with("\\\\") {
                self.watch_start();
            }
            return;
        }
        let was_stale = std::mem::take(&mut self.watching.stale);
        let before = self.watching.mtime_at_stop.take();
        self.watch_start();
        // a folder's last-write time moves when entries come, go or are
        // renamed; unknown (network) counts as changed
        if was_stale && (before.is_none() || dir_mtime(&self.path) != before) {
            self.watching.note = Some(WatchNote::Relist);
        }
    }

    fn watch_start(&mut self) {
        if self.watching.w.is_some() || self.synthetic || self.path.is_empty() || self.is_archive() {
            return;
        }
        let Some(wake) = self.wake.clone() else { return };
        let t0 = Instant::now();
        // spawn: opening the folder and arming the read happen off the UI
        // thread (a cold folder can take hundreds of ms there)
        if let Ok(w) = DirWatcher::spawn(&self.path, WatchOptions::default(), wake) {
            self.watching.w = Some(w);
        }
        self.timing.watch_start += t0.elapsed();
    }

    /// A watcher runs and its notifications can be trusted: changes show up
    /// by themselves, no relist needed after a file operation. False until
    /// the watcher's first read is armed.
    pub fn watched(&self) -> bool {
        self.watching.w.as_ref().is_some_and(|(w, _)| w.armed()) && !self.watching.unreliable
    }

    /// What the watcher asks of the view (relist, go up), once.
    pub fn take_note(&mut self) -> Option<WatchNote> {
        self.watching.note.take()
    }

    /// Selects `names` once they are in the listing (they may arrive with the
    /// watcher's next batch), for a few seconds at most.
    pub fn want_select(&mut self, names: Vec<String>) {
        self.want = Some((names, Instant::now()));
        self.try_want();
    }

    fn try_want(&mut self) {
        let Some((names, t0)) = &self.want else { return };
        if t0.elapsed() > WANT_FOR || self.is_loading() {
            if t0.elapsed() > WANT_FOR {
                self.want = None;
            }
            return;
        }
        let all = names.iter().all(|n| {
            let w: Vec<u16> = n.encode_utf16().collect();
            self.store.find(&w).is_some()
        });
        if all {
            let names = self.want.take().map(|w| w.0).unwrap_or_default();
            self.apply_selection(&names);
            if let Some(pos) = self.store.view().iter().position(|id| self.sel.contains(*id)) {
                self.list.cursor = pos;
                self.grid.cursor = pos;
                let len = self.shown();
                self.list.ensure_visible(pos, len);
            }
        }
    }

    /// Drains the watcher: batches into the store, notes for the view.
    fn pump_watch(&mut self) -> bool {
        let mut changed = false;
        // a big batch resolved on its worker
        if let Some(rx) = &self.watching.resolving {
            match rx.try_recv() {
                Ok((b, r)) => {
                    self.watching.resolving = None;
                    changed |= self.keep_place(|t| watch::apply_resolved(&mut t.store, Some(&mut t.sel), &b, &r).map(|s| s.changed()).unwrap_or(false));
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.watching.resolving = None,
            }
        }
        if let Some((_, rx)) = &self.watching.w {
            let mut end = false;
            while let Ok(m) = rx.try_recv() {
                match m {
                    watch::Message::Batch(b) => self.watching.queued.push_back(b),
                    watch::Message::Overflow => {
                        self.watching.queued.clear();
                        self.watching.note = Some(WatchNote::Relist);
                    }
                    watch::Message::Unreliable(Unreliable::NotSupported) => {
                        self.watching.unreliable = true;
                        end = true;
                    }
                    watch::Message::Unreliable(_) => self.watching.unreliable = true,
                    watch::Message::Gone(Gone::Unavailable) | watch::Message::Failed(_) => end = true,
                    watch::Message::Gone(Gone::Deleted | Gone::Renamed) => {
                        end = true;
                        self.watching.note = Some(WatchNote::Gone);
                    }
                }
            }
            if end {
                self.watching.w = None;
                self.watching.queued.clear();
            }
        }
        // in order, one at a time; nothing while the listing runs
        while self.load == Load::Done && self.watching.resolving.is_none() {
            let Some(b) = self.watching.queued.pop_front() else { break };
            if b.len() <= APPLY_INLINE {
                let dir = std::path::PathBuf::from(&self.path);
                changed |= self.keep_place(|t| watch::apply_batch(&mut t.store, Some(&mut t.sel), &dir, &b).map(|s| s.changed()).unwrap_or(false));
            } else {
                self.resolve_on_worker(b);
            }
        }
        if changed {
            self.watch_changed = true;
            if self.want.is_some() {
                self.try_want();
            }
        }
        changed
    }

    fn resolve_on_worker(&mut self, b: watch::Batch) {
        let (tx, rx): (Sender<(watch::Batch, watch::Resolved)>, _) = channel();
        let dir = std::path::PathBuf::from(&self.path);
        let hint = self.store.len();
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new().name("tsubame-watch-resolve".into()).spawn(move || {
            let r = watch::resolve(&dir, &b, hint);
            let _ = tx.send((b, r));
            if let Some(w) = wake {
                w();
            }
        });
        if spawned.is_ok() {
            self.watching.resolving = Some(rx);
        } else {
            self.watching.note = Some(WatchNote::Relist);
        }
    }

    /// Runs a store change keeping the cursor on the same entry and the first
    /// visible entry where it was (positions shift, IDs do not).
    fn keep_place(&mut self, f: impl FnOnce(&mut Tab) -> bool) -> bool {
        let row_h = self.list.row_h.max(1.0) as f64;
        let top_pos = (self.list.scroll / row_h) as usize;
        let top = self.store.at(top_pos);
        let cur = self.store.at(self.list.cursor);
        let gcur = self.store.at(self.grid.cursor);
        if !f(self) {
            return false;
        }
        let pos_of = |t: &Tab, id: Option<tsubame_core::EntryId>| id.and_then(|id| t.store.view().iter().position(|v| *v == id));
        let n = self.shown();
        if let Some(p) = pos_of(self, top) {
            let frac = self.list.scroll - top_pos as f64 * row_h;
            self.list.scroll = p as f64 * row_h + frac;
        }
        let clamp = |p: usize| p.min(n.saturating_sub(1));
        self.list.cursor = pos_of(self, cur).unwrap_or_else(|| clamp(self.list.cursor));
        self.grid.cursor = pos_of(self, gcur).unwrap_or_else(|| clamp(self.grid.cursor));
        true
    }

    fn push(&mut self, b: &Batch) {
        let started = Instant::now();
        let inputs: Vec<EntryInput> =
            b.iter().map(|(name, e)| EntryInput { name, size: e.size, modified: e.mtime as i64, flags: flags_of(e.attrs, e.is_dir(), e.is_reparse_point()) }).collect();
        let first_id = self.store.id_bound();
        // generation is the store's own (bumped by `start`): only this tab's
        // current listing ever gets here, so it always matches
        if let Err(e) = self.store.push_batch(self.store.generation(), &inputs) {
            self.load = Load::Failed(format!("Cannot store directory entries: {e:?}"));
            self.store.end_stream();
            self.enumerator = None;
            self.rx = None;
            return;
        }
        if crate::tags::want_fids() {
            // ids are handed out in batch order; the name hash catches a mismatch
            self.tag_fids.resize(first_id, (0, 0));
            self.tag_fids.extend(b.iter().map(|(name, e)| (e.file_id, crate::tags::ui::name_hash16(name))));
        }
        self.timing.store_cpu += started.elapsed();
        self.delivered += b.len();
        if self.timing.first_batch.is_none() {
            self.timing.first_batch = Some(self.timing.t_nav.elapsed());
        }
    }

    fn finish_archive(&mut self, result: Result<Vec<tsubame_core::archive::ArchiveEntry>, String>) {
        match result {
            Ok(entries) => {
                let mut names: Vec<u16> = Vec::new();
                let mut spans: Vec<(usize, usize, u64, i64, u16)> = Vec::with_capacity(entries.len());
                for e in &entries {
                    let start = names.len();
                    names.extend(e.name.encode_utf16());
                    let flags = if e.is_dir { FOLDER } else { 0 };
                    spans.push((start, names.len(), e.size, e.modified, flags));
                }
                let inputs: Vec<EntryInput> = spans
                    .iter()
                    .map(|&(a, b, size, modified, flags)| EntryInput { name: &names[a..b], size, modified, flags })
                    .collect();
                if let Err(err) = self.store.push_batch(self.store.generation(), &inputs) {
                    self.store.end_stream();
                    self.load = Load::Failed(format!("Cannot store archive entries: {err:?}"));
                    return;
                }
                self.delivered = entries.len();
                self.store.end_stream();
                self.timing.first_batch = Some(self.timing.t_nav.elapsed());
                self.timing.done = Some(self.timing.t_nav.elapsed());
                self.load = Load::Done;
                self.apply_restore();
            }
            Err(msg) => {
                self.store.end_stream();
                self.timing.done = Some(self.timing.t_nav.elapsed());
                self.load = Load::Failed(msg);
            }
        }
    }

    fn finish(&mut self, d: Done) {
        let t0 = Instant::now();
        self.store.end_stream();
        self.timing.final_sort = t0.elapsed();
        self.timing.done = Some(self.timing.t_nav.elapsed());
        self.timing.worker = Some(d.elapsed);
        self.load = match d.error {
            Some(e) if !d.cancelled => Load::Failed(e.message()),
            _ => Load::Done,
        };
        self.rx = None;
        self.enumerator = None;
        self.apply_restore();
        if self.visible && self.load == Load::Done {
            self.watch_start();
        }
    }

    /// Session restore: starts the listing of a tab that was restored at
    /// startup (the folder is repaired if it disappeared).
    pub fn resume(&mut self, wake: Wake) {
        if !self.pending {
            return;
        }
        let p = crate::session::tab_folder(&self.path);
        let restore = self.restore.take();
        self.start(&p, wake);
        self.restore = restore;
    }

    /// Selection restore for a path given on the command line or forwarded by a
    /// second launch: applies at once when the listing is there, else with the
    /// session restore state when the listing completes.
    pub fn select_names(&mut self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        if self.is_loading() || self.shown() == 0 {
            let r = self.restore.get_or_insert_default();
            for n in names {
                if !r.selected.iter().any(|s| s == n) {
                    r.selected.push(n.clone());
                }
            }
            return;
        }
        self.apply_selection(names);
    }

    /// Scroll row and selection saved in the session. Runs once, when the
    /// listing is complete (the entries are what the numbers refer to).
    fn apply_restore(&mut self) {
        let Some(r) = self.restore.take() else { return };
        let n = self.shown();
        if n > 0 && r.scroll_first_row > 0 {
            let row = r.scroll_first_row.min(n - 1);
            self.list.scroll = row as f64 * self.list.row_h as f64;
            self.list.cursor = row;
        }
        self.apply_selection(&r.selected);
    }

    /// Selects the entries whose names are in `names` (case-insensitive).
    fn apply_selection(&mut self, names: &[String]) {
        let n = self.shown();
        if names.is_empty() || n == 0 {
            return;
        }
        let mut hits = Vec::new();
        for pos in 0..n {
            let Some(id) = self.store.at(pos) else { continue };
            let Some(name) = self.store.name(id) else { continue };
            let name = String::from_utf16_lossy(name);
            if names.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                hits.push(pos);
            }
        }
        for (i, pos) in hits.into_iter().enumerate() {
            if i == 0 {
                self.sel.single(&self.store, pos);
            } else {
                self.sel.toggle(&self.store, pos);
            }
        }
    }

    /// Sort by `col`: a second click on the same column flips the direction.
    pub fn sort_by(&mut self, col: tsubame_core::Column) {
        self.sort = if self.sort.column == col { Sort { descending: !self.sort.descending, ..self.sort } } else { Sort { column: col, descending: false, folders_first: true } };
        self.store.sort(self.sort);
        self.tree_dirty = true;
        self.list.scroll = 0.0;
        self.grid.scroll = 0.0;
    }

    /// Changes whenever this tab shows another folder or listing (or another
    /// tab becomes the active one): the shell drops its pending work then.
    pub fn nav_key(&self) -> u64 {
        self.id.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ self.enum_gen
    }

    /// Scroll and cursor back to the top (a new listing, a new filter).
    fn reset_view(&mut self) {
        self.list.scroll = 0.0;
        self.list.cursor = 0;
        self.list.cancel_band();
        self.grid.scroll = 0.0;
        self.grid.cursor = 0;
        self.grid.cancel_band();
        self.band_base.clear();
        self.band_mode = BandMode::Replace;
    }

    /// Applies the filter box text. Typing one more character only searches
    /// the previous result (tsubame-core refines in place).
    pub fn set_filter(&mut self, text: &str) {
        if text == self.filter {
            return;
        }
        let mode = if text.contains(['*', '?']) { FilterMode::Glob } else { FilterMode::Substring };
        self.filter = text.to_string();
        self.store.filter(text, mode);
        self.tree_dirty = true;
        self.reset_view();
        self.sel.clear();
    }

    /// Entries shown (after the filter).
    pub fn shown(&self) -> usize {
        if self.miller_on && let Some(m) = &self.miller {
            return m.active_shown();
        }
        if self.tree_on && let Some(m) = &self.tree {
            return m.len();
        }
        self.store.view().len()
    }

    /// Full path of the entry at view position `pos`, and whether it is a folder.
    pub fn entry_at(&self, pos: usize) -> Option<(String, bool)> {
        if self.miller_on && let Some(m) = &self.miller {
            return m.active_entry_at(pos);
        }
        if self.tree_on && let Some(m) = &self.tree {
            return m.path_of(pos, Some(&self.store));
        }
        let id = self.store.at(pos)?;
        let name = String::from_utf16_lossy(self.store.name(id)?);
        let dir = self.store.record(id)?.flags & FOLDER != 0;
        Some((nav::join(&self.path, &name), dir))
    }

    /// Paths of up to `n` selected entries in view order (for the shell menu).
    pub fn selected_paths(&self, n: usize) -> Vec<String> {
        if self.miller_on && let Some(m) = &self.miller {
            return m.selected_path().into_iter().take(n).collect();
        }
        if self.tree_on && let Some(m) = &self.tree {
            return m.selected_paths(n, Some(&self.store));
        }
        self.store.view().iter().enumerate().filter(|(_, id)| self.sel.contains(**id)).take(n).filter_map(|(pos, _)| self.entry_at(pos).map(|e| e.0)).collect()
    }

    /// A frame of this tab was presented: records the first frame that shows
    /// rows and the first frame after Done (complete + sorted on screen).
    /// Returns true when the final frame of a listing was just recorded.
    pub fn frame_presented(&mut self) -> bool {
        let el = self.timing.t_nav.elapsed();
        if self.timing.final_frame.is_none() {
            self.timing.frames += 1;
        }
        if self.timing.first_frame.is_none() && self.shown() > 0 {
            self.timing.first_frame = Some(el);
        }
        if self.timing.final_frame.is_none() && self.timing.done.is_some() {
            self.timing.final_frame = Some(el);
            if self.timing.first_frame.is_none() {
                self.timing.first_frame = Some(el);
            }
            return true;
        }
        false
    }

    /// Heap bytes this tab holds in its store (diagnostics).
    pub fn memory_bytes(&self) -> usize {
        self.store.memory_bytes()
    }
}

impl Tab {
    /// True when this tab shows Miller columns (the rows come from the model).
    pub fn miller_rows(&self) -> bool {
        self.miller_on && self.miller.is_some()
    }

    /// Builds the Miller model for this tab's folder when it has none. The tab's
    /// own listing and watcher go: the model lists every column itself.
    pub fn ensure_miller(&mut self, default_w: f32) {
        if self.settings || self.synthetic || self.pending || self.is_archive() {
            self.miller = None;
            return;
        }
        if self.miller.is_some() {
            return;
        }
        let Some(wake) = self.wake.clone() else { return };
        let hide = self.store.hide();
        self.drop_listing();
        let mut m = crate::miller::MillerModel::new(&self.path, wake, self.sort, hide, default_w);
        m.set_visible(self.visible);
        self.miller = Some(Box::new(m));
    }

    /// The tab's own listing, watcher and pending work stop (Miller owns the listing).
    fn drop_listing(&mut self) {
        self.enumerator = None;
        self.rx = None;
        self.archive_rx = None;
        self.watching = Watching::default();
        self.watch_changed = false;
        self.want = None;
        self.restore = None;
        self.store.end_stream();
        self.store.clear();
        self.tag_fids = Vec::new();
        self.tree = None;
        self.sel.clear();
        self.load = Load::Done;
        self.delivered = 0;
    }

    /// A navigation while Miller columns show: the model follows the path, no listing runs here.
    fn start_miller(&mut self, path: &str, wake: Wake) {
        self.drop_listing();
        self.wake = Some(wake);
        let same = self.miller.as_ref().is_some_and(|m| crate::miller::same_path(m.active_path(), path));
        self.path = path.to_string();
        self.title_s = Self::make_title(path);
        self.filter.clear();
        self.reset_view();
        self.timing = Timing::new();
        self.enum_gen = self.token.next_generation();
        if let Some(m) = self.miller.as_mut() {
            m.set_filter("");
            if same {
                m.relist_all();
            } else {
                m.goto(path);
            }
        }
    }

    /// The active column moved to another folder (a key, a click): the tab's path
    /// and history follow it. No listing. True when the path changed.
    pub fn follow_miller(&mut self) -> bool {
        let Some(m) = self.miller.as_mut() else { return false };
        let p = m.active_path().to_string();
        if p.is_empty() || crate::miller::same_path(&p, &self.path) {
            return false;
        }
        m.synced = p.clone();
        self.history.push(&p);
        self.path = p.clone();
        self.title_s = Self::make_title(&p);
        self.enum_gen = self.token.next_generation();
        true
    }

    /// Moves what the Miller workers delivered into the model. UI thread, no disk.
    pub fn pump_miller(&mut self, preview_on: bool) -> bool {
        let sort = self.sort;
        let hide = self.store.hide();
        let Some(m) = self.miller.as_mut() else { return false };
        m.set_rules(sort, hide);
        m.set_filter(&self.filter);
        m.pump(preview_on)
    }

    /// Leaves Miller columns: the model goes, the folder lists again.
    pub fn leave_miller(&mut self) {
        self.miller_on = false;
        if self.miller.take().is_some()
            && let Some(w) = self.wake.clone()
        {
            let p = self.path.clone();
            self.start(&p, w);
        }
    }
}

/// A folder's last-write time (moves when entries are added, removed or
/// renamed). `None` for a network path (no cheap, reliable answer).
fn dir_mtime(path: &str) -> Option<std::time::SystemTime> {
    if path.starts_with("\\\\") {
        return None;
    }
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn wake() -> Wake {
        Arc::new(|| {})
    }

    fn wait_done(t: &mut Tab) {
        let t0 = Instant::now();
        while t.is_loading() {
            t.pump();
            assert!(t0.elapsed() < Duration::from_secs(10), "listing did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-u2-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lists_sorts_filters_and_navigates() {
        let d = temp_dir("tab");
        std::fs::create_dir_all(d.join("Sub")).unwrap();
        std::fs::write(d.join("b10.txt"), b"hi").unwrap();
        std::fs::write(d.join("b9.txt"), b"hello").unwrap();
        std::fs::write(d.join("A.md"), b"x").unwrap();
        let p = d.to_str().unwrap().to_string();
        let mut t = Tab::new(1, &p, 22.0);
        t.start(&p, wake());
        assert!(t.is_loading());
        wait_done(&mut t);
        assert_eq!(t.load, Load::Done);
        assert_eq!(t.shown(), 4);
        // folders first, then natural name order, case-insensitive
        let names: Vec<String> = (0..4).map(|i| t.entry_at(i).unwrap().0.rsplit('\\').next().unwrap().to_string()).collect();
        assert_eq!(names, ["Sub", "A.md", "b9.txt", "b10.txt"]);
        assert!(t.entry_at(0).unwrap().1);
        assert!(t.timing.first_batch.is_some() && t.timing.done.is_some());
        // sort by size descending (folders still first)
        t.sort_by(tsubame_core::Column::Size);
        t.sort_by(tsubame_core::Column::Size);
        assert!(t.sort.descending);
        assert!(t.entry_at(1).unwrap().0.ends_with("b9.txt"));
        // filter refines and clears
        t.set_filter("b");
        assert_eq!(t.shown(), 3); // Sub, b9, b10
        t.set_filter("b1");
        assert_eq!(t.shown(), 1);
        t.set_filter("");
        assert_eq!(t.shown(), 4);
        // select two and read their paths back
        t.sel.single(&t.store, 1);
        t.sel.toggle(&t.store, 2);
        assert_eq!(t.sel.count(), 2);
        assert_eq!(t.selected_paths(10).len(), 2);
        // navigating into the folder replaces the listing; the selection resets
        let sub = t.entry_at(0).unwrap().0;
        t.start(&sub, wake());
        wait_done(&mut t);
        assert_eq!(t.shown(), 0);
        assert_eq!(t.sel.count(), 0);
        assert_eq!(t.title(), "Sub");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_folder_fails_with_a_message() {
        let mut t = Tab::new(2, "C:\\definitely\\not\\here", 22.0);
        t.start("C:\\definitely\\not\\here", wake());
        wait_done(&mut t);
        assert!(matches!(&t.load, Load::Failed(m) if !m.is_empty()), "{:?}", t.load);
        assert_eq!(t.shown(), 0);
    }

    #[test]
    fn restarting_drops_the_old_listing() {
        let d = temp_dir("restart");
        for i in 0..50 {
            std::fs::write(d.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let p = d.to_str().unwrap().to_string();
        let mut t = Tab::new(3, &p, 22.0);
        // restart immediately and repeatedly: only the last listing counts
        for _ in 0..5 {
            t.start(&p, wake());
        }
        wait_done(&mut t);
        assert_eq!(t.shown(), 50);
        assert_eq!(t.delivered, 50, "no stale batches reached the store");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_streamed_listing_ends_fully_sorted_and_cancels() {
        // more than one batch and more than the sorted top window
        let d = temp_dir("stream");
        let n = tsubame_core::STREAM_TOP + 1500;
        for i in 0..n {
            std::fs::write(d.join(format!("f{}.txt", (i * 7919) % n)), b"").unwrap();
        }
        std::fs::create_dir_all(d.join("zdir")).unwrap();
        let p = d.to_str().unwrap().to_string();
        let mut t = Tab::new(4, &p, 22.0);
        t.start(&p, wake());
        wait_done(&mut t);
        assert!(!t.store.is_streaming());
        assert_eq!(t.shown(), n + 1);
        let got: Vec<String> = names(&t);
        assert_eq!(got[0], "zdir", "folders first");
        let want: Vec<String> = std::iter::once("zdir".to_string()).chain((0..n).map(|i| format!("f{i}.txt"))).collect();
        assert_eq!(got, want, "natural order after Done");
        // navigating away mid-listing: the old worker stops, nothing of it lands
        t.start(&p, wake());
        let sub = d.join("zdir");
        t.start(sub.to_str().unwrap(), wake());
        wait_done(&mut t);
        assert_eq!(t.shown(), 0);
        assert!(!t.store.is_streaming());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Pumps until `ok` holds (the watcher's window is 60 ms).
    fn pump_until(t: &mut Tab, what: &str, ok: impl Fn(&Tab) -> bool) {
        let t0 = Instant::now();
        while !ok(t) {
            t.pump();
            assert!(t0.elapsed() < Duration::from_secs(5), "{what}: not seen in time");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn names(t: &Tab) -> Vec<String> {
        (0..t.shown()).filter_map(|p| t.entry_at(p)).map(|(p, _)| crate::dnd::name_of(&p).to_string()).collect()
    }

    #[test]
    fn a_visible_tab_follows_changes_without_relisting() {
        let dir = temp_dir("watch");
        for n in ["b.txt", "d.txt", "f.txt"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let mut t = Tab::new(1, dir.to_str().unwrap(), 20.0);
        t.set_visible(true);
        t.start(dir.to_str().unwrap(), wake());
        wait_done(&mut t);
        pump_until(&mut t, "a shown local folder is watched", |t| t.watched());
        assert_eq!(names(&t), ["b.txt", "d.txt", "f.txt"]);
        // the cursor and the selection sit on d.txt
        t.sel.single(&t.store, 1);
        t.list.cursor = 1;
        let listing = t.enum_gen;
        // created before it: the cursor follows d.txt to its new position
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        pump_until(&mut t, "create", |t| t.shown() == 4);
        assert_eq!(names(&t), ["a.txt", "b.txt", "d.txt", "f.txt"]);
        assert_eq!(t.list.cursor, 2, "cursor stays on d.txt");
        assert_eq!(t.selected_paths(4).len(), 1);
        assert!(t.selected_paths(1)[0].ends_with("d.txt"));
        // renamed in place, deleted
        std::fs::rename(dir.join("f.txt"), dir.join("g.txt")).unwrap();
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        pump_until(&mut t, "rename + delete", |t| names(t) == ["a.txt", "d.txt", "g.txt"]);
        assert!(t.selected_paths(1)[0].ends_with("d.txt"));
        assert_eq!(t.enum_gen, listing, "no relist: the changes were applied to the store");
        assert_eq!(t.take_note(), None);
        // names wanted after a copy show up selected
        t.want_select(vec!["h.txt".into()]);
        std::fs::write(dir.join("h.txt"), b"x").unwrap();
        pump_until(&mut t, "wanted name", |t| t.selected_paths(4).len() == 1 && t.selected_paths(1)[0].ends_with("h.txt"));
        // hidden: no watcher; a change meanwhile relists when shown again
        t.set_visible(false);
        assert!(!t.watched());
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("i.txt"), b"x").unwrap();
        t.set_visible(true);
        // the watcher arms off the UI thread
        pump_until(&mut t, "watcher armed", |t| t.watched());
        assert_eq!(t.take_note(), Some(WatchNote::Relist));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn big_bursts_resolve_off_the_ui_thread_and_land_in_order() {
        let dir = temp_dir("burst");
        let mut t = Tab::new(1, dir.to_str().unwrap(), 20.0);
        t.set_visible(true);
        t.start(dir.to_str().unwrap(), wake());
        wait_done(&mut t);
        for i in 0..200 {
            std::fs::write(dir.join(format!("n{i:03}.txt")), b"x").unwrap();
        }
        pump_until(&mut t, "burst", |t| t.shown() == 200);
        for i in 0..50 {
            std::fs::remove_file(dir.join(format!("n{i:03}.txt"))).unwrap();
        }
        pump_until(&mut t, "removals", |t| t.shown() == 150);
        assert_eq!(names(&t)[0], "n050.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn synthetic_and_hidden_tabs_do_not_watch() {
        let dir = temp_dir("nowatch");
        let mut t = Tab::new(1, dir.to_str().unwrap(), 20.0);
        t.start(dir.to_str().unwrap(), wake());
        wait_done(&mut t);
        assert!(!t.watched(), "not shown");
        t.adopt_store("bench", EntryStore::new());
        t.set_visible(true);
        assert!(!t.watched(), "no real folder");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
