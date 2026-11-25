//! Tree view model (P4.F6, built-in extension `tsubame.ext.tree`,
//! `docs/f6-mock.md`, `mocks/mock-f6-tree.html`).
//!
//! One model serves two surfaces: *Tree rows* (the Details list made
//! hierarchical: files and folders as expandable rows, the root folder is the
//! tab's own listing) and the *Tree pane* (folders only, rooted at a volume).
//!
//! The model is an expanded set plus a flattened list of visible rows:
//!
//! * every expanded folder is one [`Dir`]: its path string, its own
//!   `EntryStore` (arrays, one name buffer; no per-entry allocation) and the
//!   map from a folder entry to the `Dir` that expands it;
//! * the visible rows are `Vec<TRow>`, 12 bytes each (`slot`, `pos` in that
//!   dir's sorted view, depth, kind). There is no per-row heap object.
//!
//! Folders list lazily on first expand through the normal enumerator (the
//! worker streams batches, every request has its own cancel token, collapsing
//! drops the `Dir` and with it the enumerator, which cancels). Nothing is read
//! ahead. A refresh enumerates into a second store and swaps at the end, so
//! rows do not jump. The UI thread never touches the disk: reparse targets and
//! the loop check run on short-lived workers that wake the UI when done.
//!
//! Caps (never silent: `take_status` says what stopped):
//! * recursive expand: `expand_cap` folders (500), links are never followed;
//! * `child_cap` children per expanded folder (5,000), then a "Showing first
//!   5,000 of N" row;
//! * `MAX_ROWS` live rows (50,000);
//! * `MAX_WATCHED` live watchers (32), nearest to the selection first.
//!
//! Idle cost is zero: no timers and no spinner. The only waiting thread is a
//! one-shot 80 ms sleeper that wakes the UI so the "Loading…" row can appear
//! for a listing that is still running.

use crate::nav;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};
use tsubame_core::{EntryId, EntryInput, EntryStore, FOLDER, REPARSE, Sort};
use tsubame_win::enumerate::{Batch, CancelToken, Done, Enumerator, Message, Wake};
use tsubame_win::watch::{self, DirWatcher, Gone, Options as WatchOptions};

pub const EXT_TREE: &str = "tsubame.ext.tree";

/// The pane never gets narrower than this (DIPs).
pub const PANE_MIN: f32 = 160.0;

/// Built-in extension registering `view.tree` and `pane.tree`.
pub struct TreeExtension;

impl tsubame_ext_api::Extension<crate::ext::App> for TreeExtension {
    fn info(&self) -> tsubame_ext_api::ExtensionInfo {
        tsubame_ext_api::ExtensionInfo::builtin(EXT_TREE, "Tree view", false)
    }

    fn contribute(&self, c: &mut tsubame_ext_api::Contributions<crate::ext::App>) {
        for spec in crate::commands::TREE_COMMANDS {
            c.command(spec.clone(), crate::view::View::run_builtin);
        }
    }
}

/// The tree extension is enabled.
pub fn enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_TREE).unwrap_or(false))
}

pub const DEFAULT_EXPAND_CAP: usize = 500;
pub const DEFAULT_CHILD_CAP: usize = 5_000;
/// Live rows at most (a folder opening past this is refused with a message).
pub const MAX_ROWS: usize = 50_000;
/// Expanded folders that get a live watcher.
pub const MAX_WATCHED: usize = 32;
/// The "Loading…" row appears after this long.
pub const LOADING_AFTER: Duration = Duration::from_millis(80);
/// Indent per level (DIPs), full steps up to this depth.
pub const INDENT: f32 = 16.0;
pub const FULL_DEPTH: u16 = 12;
/// Indent per level past `FULL_DEPTH`.
pub const CLAMPED_INDENT: f32 = 6.0;
/// Pending paths a session restore may re-expand.
pub const RESTORE_CAP: usize = 64;
/// Paths the session saves per tab.
pub const SESSION_CAP: usize = 256;
/// Type-ahead buffer lifetime.
const TYPE_AHEAD: Duration = Duration::from_millis(1000);
/// Watch batches up to this many names apply on the UI thread.
const WATCH_INLINE: usize = 30;
/// A listing that nothing watches is re-read when the selection enters it after this long.
const STALE_AFTER: Duration = Duration::from_secs(3);
/// Reparse points resolved per folder.
const LINK_CAP: usize = 256;

/// Indent in DIPs of a row at `depth` (16 per level, 6 per level past 12).
pub fn indent(depth: u16) -> f32 {
    if depth <= FULL_DEPTH { depth as f32 * INDENT } else { FULL_DEPTH as f32 * INDENT + (depth - FULL_DEPTH) as f32 * CLAMPED_INDENT }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// a file or folder of a listing (`pos` is its view position)
    Entry,
    Loading,
    Empty,
    Denied,
    /// a followed link that loops back to an ancestor
    Loop,
    /// the child cap cut the listing
    Capped,
}

/// One visible row: 12 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TRow {
    pub slot: u32,
    pub pos: u32,
    pub depth: u16,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// the first listing runs
    Loading,
    Done,
    /// the folder could not be listed (message for the row)
    Denied(String),
    /// the target is an ancestor: the path it loops back to
    Loop(String),
}

/// A row the cursor or selection can refer to across a re-flatten.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub slot: u32,
    /// entry id, or `u32::MAX` for a pseudo row
    pub id: u32,
    pub kind: Kind,
}

/// The root folder's own store when it lives outside the model (Tree rows:
/// the tab's listing). `None`: the model's slot 0 owns it (Tree pane).
pub type Root<'a> = Option<&'a EntryStore>;

/// Result of an expand request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expanded {
    Opened,
    Already,
    NotFolder,
    /// a link: only an explicit expand follows it
    Link,
    Refused,
}

struct Load {
    store: EntryStore,
    fids: Vec<(u128, u32)>,
    total: usize,
    capped: bool,
    /// keeps the worker alive: dropping it cancels the listing
    _en: Enumerator,
    rx: Receiver<Message>,
}

struct Dir {
    path: String,
    /// depth of this folder's children
    depth: u16,
    parent: u32,
    parent_id: EntryId,
    store: EntryStore,
    state: State,
    /// when the first listing started (the loading row waits `LOADING_AFTER`)
    started: Instant,
    published: Option<Instant>,
    total: usize,
    capped: bool,
    fids: Vec<(u128, u32)>,
    links: HashMap<String, String>,
    links_rx: Option<Receiver<Vec<(String, String)>>>,
    kids: HashMap<EntryId, u32>,
    load: Option<Box<Load>>,
    /// the loop check of a followed link runs; the listing starts when it passes
    check: Option<Receiver<Option<String>>>,
    /// opened by recursive expand: its folders open when it lists
    auto: bool,
    watch: Option<(DirWatcher, Receiver<watch::Message>)>,
    /// the loading row has been drawn at least once (re-flatten when it is due)
    loading_due: bool,
}

pub struct TreeModel {
    pub folders_only: bool,
    root: String,
    /// slot 0 is the root folder; freed slots are `None`
    dirs: Vec<Option<Dir>>,
    free: Vec<u32>,
    rows: Vec<TRow>,
    dirty: bool,
    rows_capped: bool,
    sel: HashSet<(u32, u32)>,
    cursor: Option<Key>,
    anchor: Option<Key>,
    sort: Sort,
    hide: u16,
    wake: Wake,
    pub expand_cap: usize,
    pub child_cap: usize,
    status: Option<String>,
    /// folders opened by the running recursive expand, and its budget
    auto_opened: usize,
    auto_budget: usize,
    auto_cut: bool,
    auto_active: bool,
    /// paths to expand as their parents list (session restore, refresh, reveal)
    restore: Vec<String>,
    /// paths to select as they appear
    reselect: Vec<String>,
    /// the folder the cursor was in at the last pump (a move into a stale one refreshes it)
    cursor_slot: u32,
    /// the pane follows this folder: select it once its row exists
    pub follow: Option<String>,
    /// the folder the pane last revealed
    followed: Option<String>,
    typed: String,
    typed_at: Option<Instant>,
    visible: bool,
    root_links: HashMap<String, String>,
    root_links_rx: Option<Receiver<Vec<(String, String)>>>,
    root_links_done: bool,
    /// the watched set needs recomputing (selection or expansion moved)
    watch_dirty: bool,
    /// select this entry once its row exists (after a rename)
    pick: Option<(String, Instant)>,
}

fn utf8(w: &[u16]) -> String {
    String::from_utf16_lossy(w)
}

fn lower_u16(c: u16) -> u16 {
    char::from_u32(u32::from(c)).map_or(c, |ch| lower(ch) as u32 as u16)
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Case-insensitive path equality (Windows).
fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\').eq_ignore_ascii_case(b.trim_end_matches('\\')) || a.trim_end_matches('\\').to_lowercase() == b.trim_end_matches('\\').to_lowercase()
}

fn new_dir(path: String, depth: u16, parent: u32, parent_id: EntryId, hide: u16, sort: Sort) -> Dir {
    let mut store = EntryStore::new();
    store.set_hide(hide);
    store.sort(sort);
    Dir {
        path,
        depth,
        parent,
        parent_id,
        store,
        state: State::Loading,
        started: Instant::now(),
        published: None,
        total: 0,
        capped: false,
        fids: Vec::new(),
        links: HashMap::new(),
        links_rx: None,
        kids: HashMap::new(),
        load: None,
        check: None,
        auto: false,
        watch: None,
        loading_due: false,
    }
}

impl TreeModel {
    /// A model rooted at `root`. `owns_root`: slot 0 lists the root itself
    /// (the pane); otherwise the root's store is passed to every call (Tree rows).
    pub fn new(root: &str, folders_only: bool, owns_root: bool, wake: Wake, sort: Sort, hide: u16) -> TreeModel {
        let mut m = TreeModel {
            folders_only,
            root: root.to_string(),
            dirs: Vec::new(),
            free: Vec::new(),
            rows: Vec::new(),
            dirty: true,
            rows_capped: false,
            sel: HashSet::new(),
            cursor: None,
            anchor: None,
            sort,
            hide,
            wake,
            expand_cap: DEFAULT_EXPAND_CAP,
            child_cap: DEFAULT_CHILD_CAP,
            status: None,
            auto_opened: 0,
            auto_budget: 0,
            auto_cut: false,
            auto_active: false,
            restore: Vec::new(),
            reselect: Vec::new(),
            cursor_slot: 0,
            follow: None,
            followed: None,
            typed: String::new(),
            typed_at: None,
            visible: true,
            root_links: HashMap::new(),
            root_links_rx: None,
            root_links_done: false,
            watch_dirty: true,
            pick: None,
        };
        let mut d = new_dir(root.to_string(), 0, 0, u32::MAX, hide, sort);
        if owns_root {
            m.dirs.push(Some(d));
            m.start_load(0);
        } else {
            d.state = State::Done;
            m.dirs.push(Some(d));
        }
        m
    }

    pub fn root_path(&self) -> &str {
        &self.root
    }

    pub fn set_limits(&mut self, expand_cap: usize, child_cap: usize) {
        self.expand_cap = expand_cap.max(1);
        self.child_cap = child_cap.max(1);
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row(&self, i: usize) -> Option<TRow> {
        self.rows.get(i).copied()
    }

    #[allow(dead_code)]
    pub fn rows_capped(&self) -> bool {
        self.rows_capped
    }

    pub fn take_status(&mut self) -> Option<String> {
        self.status.take()
    }

    fn dir(&self, slot: u32) -> Option<&Dir> {
        self.dirs.get(slot as usize).and_then(|d| d.as_ref())
    }

    fn dir_mut(&mut self, slot: u32) -> Option<&mut Dir> {
        self.dirs.get_mut(slot as usize).and_then(|d| d.as_mut())
    }

    fn store<'a>(&'a self, slot: u32, root: Root<'a>) -> Option<&'a EntryStore> {
        if slot == 0 && let Some(r) = root {
            return Some(r);
        }
        self.dir(slot).map(|d| &d.store)
    }

    /// Folders currently expanded below the root (live `Dir`s).
    #[cfg(test)]
    pub fn expanded_count(&self) -> usize {
        self.dirs.iter().skip(1).filter(|d| d.is_some()).count()
    }

    #[cfg(test)]
    pub fn loading_count(&self) -> usize {
        self.dirs.iter().flatten().filter(|d| d.load.is_some() || d.check.is_some()).count()
    }

    #[cfg(test)]
    pub fn watched_count(&self) -> usize {
        self.dirs.iter().flatten().filter(|d| d.watch.is_some()).count()
    }

    // ---- sort, hide, visibility ---------------------------------------------

    /// The tab's sort or hide rule changed: every listing follows it.
    pub fn set_rules(&mut self, sort: Sort, hide: u16) {
        if sort == self.sort && hide == self.hide {
            return;
        }
        self.sort = sort;
        self.hide = hide;
        for d in self.dirs.iter_mut().skip(1).flatten() {
            d.store.set_hide(hide);
            d.store.sort(sort);
        }
        if let Some(d) = self.dirs.first_mut().and_then(|d| d.as_mut()) {
            d.store.set_hide(hide);
            d.store.sort(sort);
        }
        self.dirty = true;
    }

    /// The tab is shown (watchers run) or hidden (they stop).
    pub fn set_visible(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        if !visible {
            for d in self.dirs.iter_mut().flatten() {
                d.watch = None;
            }
        } else {
            self.watch_dirty = true;
        }
    }

    /// Marks the root listing changed (the tab's store was mutated).
    pub fn root_changed(&mut self) {
        self.dirty = true;
    }

    // ---- loading --------------------------------------------------------------

    fn start_load(&mut self, slot: u32) {
        let wake = self.wake.clone();
        let (hide, sort, cap) = (self.hide, self.sort, self.child_cap);
        let Some(d) = self.dir_mut(slot) else { return };
        let token = CancelToken::new();
        let generation = token.next_generation();
        let (en, rx) = Enumerator::start_channel(&d.path, generation, token, wake.clone());
        let mut store = EntryStore::new();
        store.set_hide(hide);
        store.sort(sort);
        store.begin_stream();
        let _ = cap;
        d.load = Some(Box::new(Load { store, fids: Vec::new(), total: 0, capped: false, _en: en, rx }));
        if d.published.is_none() {
            d.started = Instant::now();
            // the loading row needs a repaint at 80 ms; nothing else would wake the UI
            let w = wake;
            let _ = std::thread::Builder::new().name("tsubame-tree-delay".into()).stack_size(64 * 1024).spawn(move || {
                std::thread::sleep(LOADING_AFTER + Duration::from_millis(5));
                w();
            });
        }
    }

    /// Opens child folder `id` of `slot` as a new `Dir` (lazy listing).
    fn open_child(&mut self, slot: u32, id: EntryId, name: &str, reparse: bool, auto: bool, root: Root) -> bool {
        let Some(parent) = self.dir(slot) else { return false };
        let depth = parent.depth + 1;
        let path = nav::join(&parent.path, name);
        let ancestors = self.chain_paths(slot);
        let mut d = new_dir(path.clone(), depth, slot, id, self.hide, self.sort);
        d.auto = auto;
        let new_slot = match self.free.pop() {
            Some(s) => {
                self.dirs[s as usize] = Some(d);
                s
            }
            None => {
                self.dirs.push(Some(d));
                (self.dirs.len() - 1) as u32
            }
        };
        let _ = root;
        if let Some(p) = self.dir_mut(slot) {
            p.kids.insert(id, new_slot);
        }
        if reparse {
            // follow only after the loop check passes (a worker: no disk on the UI thread)
            let (tx, rx) = channel();
            let wake = self.wake.clone();
            let p2 = path;
            let spawned = std::thread::Builder::new().name("tsubame-tree-loop".into()).spawn(move || {
                let me = tsubame_win::fileid::file_id(&p2);
                let hit = me.and_then(|m| ancestors.into_iter().find(|a| tsubame_win::fileid::file_id(a) == Some(m)));
                let _ = tx.send(hit);
                wake();
            });
            if spawned.is_ok() {
                if let Some(d) = self.dir_mut(new_slot) {
                    d.check = Some(rx);
                }
            } else {
                self.start_load(new_slot);
            }
        } else {
            self.start_load(new_slot);
        }
        self.dirty = true;
        self.watch_dirty = true;
        true
    }

    /// Paths of `slot` and its ancestors up to the root.
    fn chain_paths(&self, slot: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = slot;
        for _ in 0..4096 {
            let Some(d) = self.dir(cur) else { break };
            out.push(d.path.clone());
            if cur == 0 {
                break;
            }
            cur = d.parent;
        }
        out
    }

    fn ingest(d: &mut Dir, b: &Batch, folders_only: bool, cap: usize, tag_ids: bool) {
        let Some(load) = d.load.as_mut() else { return };
        let mut inputs: Vec<EntryInput> = Vec::with_capacity(b.len());
        let mut fids: Vec<(u128, u32)> = Vec::new();
        for (name, e) in b.iter() {
            if folders_only && !e.is_dir() {
                continue;
            }
            load.total += 1;
            if load.store.len() + inputs.len() >= cap {
                load.capped = true;
                continue;
            }
            inputs.push(EntryInput { name, size: e.size, modified: e.mtime as i64, flags: crate::tab::flags_of(e.attrs, e.is_dir(), e.is_reparse_point()) });
            if tag_ids {
                fids.push((e.file_id, crate::tags::ui::name_hash16(name)));
            }
        }
        let first_id = load.store.id_bound();
        if load.store.push_batch(load.store.generation(), &inputs).is_ok() && tag_ids {
            load.fids.resize(first_id, (0, 0));
            load.fids.extend(fids);
        }
    }

    // ---- pump -----------------------------------------------------------------

    /// Moves what the workers delivered into the model. UI thread, no disk.
    /// `root`/`root_done`: the root listing (Tree rows). True when rows may have changed.
    pub fn pump(&mut self, root: Root, root_done: bool) -> bool {
        let before = self.dirty;
        let n = self.dirs.len();
        for slot in 0..n as u32 {
            if self.dirs[slot as usize].is_some() {
                self.pump_dir(slot);
            }
        }
        if root_done {
            self.pump_root(root);
        }
        self.after_pump(root, root_done);
        self.try_pick(root);
        self.dirty && !before || self.dirty
    }

    fn pump_root(&mut self, root: Root) {
        if !self.root_links_done && let Some(r) = root {
            self.root_links_done = true;
            let names = reparse_names(r);
            if !names.is_empty() {
                self.root_links_rx = Some(spawn_links(self.root.clone(), names, self.wake.clone()));
            }
        }
        if let Some(rx) = &self.root_links_rx {
            match rx.try_recv() {
                Ok(v) => {
                    self.root_links.extend(v);
                    self.root_links_rx = None;
                    self.dirty = true;
                }
                Err(TryRecvError::Disconnected) => self.root_links_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    fn pump_dir(&mut self, slot: u32) {
        // loop check
        let mut checked: Option<Option<String>> = None;
        if let Some(d) = self.dir_mut(slot)
            && let Some(rx) = &d.check
        {
            match rx.try_recv() {
                Ok(v) => checked = Some(v),
                Err(TryRecvError::Disconnected) => checked = Some(None),
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(hit) = checked {
            if let Some(d) = self.dir_mut(slot) {
                d.check = None;
                if let Some(target) = hit {
                    d.state = State::Loop(target);
                } else {
                    // the check passed: list it
                }
            }
            let loops = self.dir(slot).is_some_and(|d| matches!(d.state, State::Loop(_)));
            if !loops {
                self.start_load(slot);
            }
            self.dirty = true;
        }
        // listing messages, within a small time budget
        let t0 = Instant::now();
        let want_ids = crate::tags::want_fids();
        let (folders_only, cap) = (self.folders_only, self.child_cap);
        let mut finished: Option<Done> = None;
        loop {
            if t0.elapsed() >= Duration::from_millis(4) {
                (self.wake)();
                break;
            }
            let Some(d) = self.dir_mut(slot) else { break };
            let Some(load) = d.load.as_ref() else { break };
            match load.rx.try_recv() {
                Ok(Message::Batch(b)) => Self::ingest(d, &b, folders_only, cap, want_ids),
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
            self.finish_load(slot, done);
        }
        // reparse targets
        if let Some(d) = self.dir_mut(slot)
            && let Some(rx) = &d.links_rx
        {
            match rx.try_recv() {
                Ok(v) => {
                    d.links.extend(v);
                    d.links_rx = None;
                    self.dirty = true;
                }
                Err(TryRecvError::Disconnected) => d.links_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        self.pump_watch(slot);
    }

    fn finish_load(&mut self, slot: u32, done: Done) {
        let wake = self.wake.clone();
        let Some(d) = self.dir_mut(slot) else { return };
        let Some(load) = d.load.take() else { return };
        let load = *load;
        if done.cancelled {
            return;
        }
        if let Some(e) = done.error {
            if d.published.is_none() {
                d.state = State::Denied(e.message());
            } else {
                self.status = Some(format!("Could not refresh {}: {}", d.path, e.message()));
            }
            self.dirty = true;
            return;
        }
        let Load { mut store, fids, total, capped, .. } = load;
        store.end_stream();
        let first = d.published.is_none();
        let old = std::mem::replace(&mut d.store, store);
        d.fids = fids;
        d.total = total;
        d.capped = capped;
        d.state = State::Done;
        d.published = Some(Instant::now());
        let path = d.path.clone();
        let names = reparse_names(&d.store);
        if !names.is_empty() {
            d.links_rx = Some(spawn_links(path, names, wake));
        }
        if !first {
            self.remap(slot, &old);
        }
        self.dirty = true;
        self.watch_dirty = true;
        self.on_listed(slot);
    }

    /// After a refresh swapped the store: expanded children, the selection and
    /// the cursor follow their entries by name; gone entries drop out.
    fn remap(&mut self, slot: u32, old: &EntryStore) {
        let Some(new_store) = self.dirs[slot as usize].as_ref().map(|d| &d.store) else { return };
        let map_id = |id: EntryId| -> Option<EntryId> { old.name(id).and_then(|n| new_store.find(n)) };
        let kids: Vec<(EntryId, u32)> = self.dir(slot).map(|d| d.kids.iter().map(|(a, b)| (*a, *b)).collect()).unwrap_or_default();
        let mut moved: Vec<(EntryId, u32)> = Vec::new();
        let mut gone: Vec<u32> = Vec::new();
        for (id, child) in kids {
            match map_id(id) {
                Some(n) => moved.push((n, child)),
                None => gone.push(child),
            }
        }
        let sel: Vec<u32> = self.sel.iter().filter(|(s, _)| *s == slot).map(|(_, id)| *id).collect();
        let mut keep: Vec<(u32, u32)> = Vec::new();
        for id in sel {
            self.sel.remove(&(slot, id));
            if let Some(n) = map_id(id) {
                keep.push((slot, n));
            }
        }
        let remap_key = |k: Option<Key>| -> Option<Key> {
            let k = k?;
            if k.slot != slot || k.id == u32::MAX {
                return Some(k);
            }
            map_id(k.id).map(|id| Key { id, ..k })
        };
        let (cur, anc) = (remap_key(self.cursor), remap_key(self.anchor));
        for k in keep {
            self.sel.insert(k);
        }
        self.cursor = cur;
        self.anchor = anc;
        if let Some(d) = self.dir_mut(slot) {
            d.kids = moved.into_iter().collect();
        }
        for c in gone {
            self.collapse_slot(c);
        }
    }

    /// A folder finished listing: recursive expand, restore and follow continue.
    fn on_listed(&mut self, slot: u32) {
        // recursive expand continues into this folder's folders
        let auto = self.dir(slot).is_some_and(|d| d.auto);
        if auto {
            self.expand_folders_of(slot);
        }
        self.apply_restore(slot);
        if self.auto_active && self.dirs.iter().flatten().all(|d| !(d.auto && (d.load.is_some() || d.check.is_some()))) {
            self.auto_active = false;
            let n = self.auto_opened;
            self.status = Some(if self.auto_cut {
                format!("Expanded {n} folders (stopped at the cap of {}; links not followed). Raise tree.expand_cap to go further.", self.auto_budget)
            } else {
                format!("Expanded {n} folders (links not followed)")
            });
        }
    }

    /// Expands every plain folder of `slot` (recursive expand) while the budget lasts.
    fn expand_folders_of(&mut self, slot: u32) {
        let folders: Vec<(EntryId, String, bool)> = {
            let Some(d) = self.dir(slot) else { return };
            d.store
                .view()
                .iter()
                .filter_map(|&id| {
                    let r = d.store.record(id)?;
                    (r.flags & FOLDER != 0).then(|| (id, utf8(d.store.name(id).unwrap_or(&[])), r.flags & REPARSE != 0))
                })
                .collect()
        };
        for (id, name, link) in folders {
            if link || self.dir(slot).is_some_and(|d| d.kids.contains_key(&id)) {
                continue;
            }
            if self.auto_opened >= self.auto_budget || self.rows.len() >= MAX_ROWS {
                self.auto_cut = true;
                break;
            }
            if self.open_child(slot, id, &name, false, true, None) {
                self.auto_opened += 1;
            }
        }
    }

    /// Expands the restore paths whose parent is `slot`, selects the pending names.
    fn apply_restore(&mut self, slot: u32) {
        if self.restore.is_empty() && self.reselect.is_empty() && self.follow.is_none() {
            return;
        }
        let Some(d) = self.dir(slot) else { return };
        let dpath = d.path.clone();
        let mut opens: Vec<(EntryId, String, bool)> = Vec::new();
        let mut picks: Vec<(EntryId, String)> = Vec::new();
        let mut keep: Vec<String> = Vec::new();
        for p in std::mem::take(&mut self.restore) {
            let Some(parent) = nav::parent(&p) else { continue };
            if !same_path(&parent, &dpath) {
                keep.push(p);
                continue;
            }
            let name = p.trim_end_matches('\\').rsplit('\\').next().unwrap_or("").to_string();
            let w: Vec<u16> = name.encode_utf16().collect();
            if let Some(d) = self.dir(slot)
                && let Some(id) = d.store.find(&w)
                && d.store.record(id).is_some_and(|r| r.flags & FOLDER != 0)
                && !d.kids.contains_key(&id)
            {
                let link = d.store.record(id).is_some_and(|r| r.flags & REPARSE != 0);
                if !link {
                    opens.push((id, name, link));
                }
            }
        }
        self.restore = keep;
        for (id, name, _) in opens {
            self.open_child(slot, id, &name, false, false, None);
        }
        for p in std::mem::take(&mut self.reselect) {
            let Some(parent) = nav::parent(&p) else { continue };
            if !same_path(&parent, &dpath) {
                self.reselect.push(p);
                continue;
            }
            let name = p.trim_end_matches('\\').rsplit('\\').next().unwrap_or("").to_string();
            let w: Vec<u16> = name.encode_utf16().collect();
            if let Some(d) = self.dir(slot)
                && let Some(id) = d.store.find(&w)
            {
                picks.push((id, name));
            }
        }
        for (id, _) in picks {
            self.sel.insert((slot, id));
            self.cursor = Some(Key { slot, id, kind: Kind::Entry });
        }
    }

    fn after_pump(&mut self, root: Root, root_done: bool) {
        // root-level restore (Tree rows: the tab's listing is the root)
        if root_done && let Some(r) = root {
            if !self.restore.is_empty() || !self.reselect.is_empty() {
                self.apply_root_restore(r);
            }
            if self.auto_active && self.dirs[0].as_ref().is_some_and(|d| d.auto) {
                // the root's own folders opened by a recursive expand started on a root row
            }
        }
        // the loading row becomes due after 80 ms
        let now = Instant::now();
        for d in self.dirs.iter_mut().flatten() {
            if (!d.loading_due && d.published.is_none() && d.load.is_some() || d.check.is_some() && !d.loading_due) && now.duration_since(d.started) >= LOADING_AFTER {
                d.loading_due = true;
                self.dirty = true;
            }
        }
        let cs = self.cursor.map_or(0, |k| k.slot);
        if cs != self.cursor_slot {
            self.cursor_slot = cs;
            self.refresh_cursor_dir();
        }
        if self.watch_dirty && self.visible {
            self.watch_dirty = false;
            self.rebalance_watchers();
        }
    }

    fn apply_root_restore(&mut self, root: &EntryStore) {
        let rpath = self.root.clone();
        let mut keep = Vec::new();
        let mut opens: Vec<(EntryId, String)> = Vec::new();
        for p in std::mem::take(&mut self.restore) {
            let Some(parent) = nav::parent(&p) else { continue };
            if !same_path(&parent, &rpath) {
                keep.push(p);
                continue;
            }
            let name = p.trim_end_matches('\\').rsplit('\\').next().unwrap_or("").to_string();
            let w: Vec<u16> = name.encode_utf16().collect();
            if let Some(id) = root.find(&w)
                && root.record(id).is_some_and(|r| r.flags & FOLDER != 0 && r.flags & REPARSE == 0)
                && !self.dirs[0].as_ref().is_some_and(|d| d.kids.contains_key(&id))
            {
                opens.push((id, name));
            }
        }
        self.restore = keep;
        for (id, name) in opens {
            self.open_child(0, id, &name, false, false, Some(root));
        }
        for p in std::mem::take(&mut self.reselect) {
            let Some(parent) = nav::parent(&p) else { continue };
            if !same_path(&parent, &rpath) {
                self.reselect.push(p);
                continue;
            }
            let name = p.trim_end_matches('\\').rsplit('\\').next().unwrap_or("").to_string();
            let w: Vec<u16> = name.encode_utf16().collect();
            if let Some(id) = root.find(&w) {
                self.sel.insert((0, id));
                self.cursor = Some(Key { slot: 0, id, kind: Kind::Entry });
            }
        }
        self.dirty = true;
    }

    // ---- watchers -------------------------------------------------------------

    /// Distance between two folder paths: components that differ.
    fn distance(a: &str, b: &str) -> usize {
        let ca: Vec<String> = a.trim_end_matches('\\').split('\\').map(|s| s.to_lowercase()).collect();
        let cb: Vec<String> = b.trim_end_matches('\\').split('\\').map(|s| s.to_lowercase()).collect();
        let common = ca.iter().zip(&cb).take_while(|(x, y)| x == y).count();
        (ca.len() - common) + (cb.len() - common)
    }

    /// At most `MAX_WATCHED` expanded folders keep a watcher: the ones nearest the cursor.
    fn rebalance_watchers(&mut self) {
        let focus = self.cursor.and_then(|k| self.dir(k.slot).map(|d| d.path.clone())).unwrap_or_else(|| self.root.clone());
        let mut order: Vec<(usize, u32)> = self
            .dirs
            .iter()
            .enumerate()
            .filter_map(|(i, d)| d.as_ref().filter(|d| d.published.is_some()).map(|d| (Self::distance(&d.path, &focus), i as u32)))
            .collect();
        order.sort();
        let keep: HashSet<u32> = order.iter().take(MAX_WATCHED).map(|(_, s)| *s).collect();
        let wake = self.wake.clone();
        for (i, d) in self.dirs.iter_mut().enumerate() {
            let Some(d) = d else { continue };
            // the root of Tree rows is the tab's own folder: the tab watches it
            let own = i == 0 && d.published.is_none();
            if own {
                continue;
            }
            if keep.contains(&(i as u32)) && !d.path.starts_with("\\\\") {
                if d.watch.is_none() && d.published.is_some() {
                    d.watch = DirWatcher::spawn(&d.path, WatchOptions::default(), wake.clone()).ok();
                }
            } else {
                d.watch = None;
            }
        }
    }

    fn pump_watch(&mut self, slot: u32) {
        let mut relist = false;
        let mut gone = false;
        let mut batches: Vec<watch::Batch> = Vec::new();
        let Some(d) = self.dir_mut(slot) else { return };
        if let Some((_, rx)) = &d.watch {
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
                d.watch = None;
            }
        }
        if d.load.is_some() && d.published.is_some() {
            // a refresh runs: it will see these changes
            batches.clear();
        }
        let dpath = PathBuf::from(&d.path);
        let mut changed = false;
        for b in batches {
            if b.len() > WATCH_INLINE {
                relist = true;
                break;
            }
            let d = self.dir_mut(slot).expect("dir");
            changed |= watch::apply_batch(&mut d.store, None, &dpath, &b).map(|s| s.changed()).unwrap_or(false);
        }
        if changed {
            self.prune_dead(slot);
            self.dirty = true;
        }
        if gone && slot != 0 {
            self.collapse_slot(slot);
            self.dirty = true;
        } else if relist {
            self.relist(slot);
        }
    }

    /// Entries a watcher removed take their expansion and selection with them.
    fn prune_dead(&mut self, slot: u32) {
        let Some(d) = self.dir(slot) else { return };
        let dead: Vec<(EntryId, u32)> = d.kids.iter().filter(|(id, _)| !d.store.is_live(**id)).map(|(a, b)| (*a, *b)).collect();
        for (id, child) in dead {
            if let Some(d) = self.dir_mut(slot) {
                d.kids.remove(&id);
            }
            self.collapse_slot(child);
        }
        let store_dead: Vec<(u32, u32)> = self.sel.iter().filter(|(s, id)| *s == slot && !self.dir(*s).is_some_and(|d| d.store.is_live(*id))).copied().collect();
        for k in store_dead {
            self.sel.remove(&k);
        }
        if self.cursor.is_some_and(|k| k.slot == slot && k.id != u32::MAX && !self.dir(slot).is_some_and(|d| d.store.is_live(k.id))) {
            self.cursor = None;
        }
    }

    /// Re-reads a folder keeping rows, expansions and selection (no "Loading…" flash).
    pub fn relist(&mut self, slot: u32) {
        if slot == 0 && self.dir(0).is_some_and(|d| d.published.is_none()) {
            return;
        }
        if self.dir(slot).is_some_and(|d| d.load.is_none() && d.published.is_some() && d.check.is_none()) {
            self.start_load(slot);
        }
    }

    /// Refreshes the listing that holds the cursor when nothing watches it and it is stale.
    pub fn refresh_cursor_dir(&mut self) {
        let Some(k) = self.cursor else { return };
        if k.slot == 0 && self.dirs[0].as_ref().is_some_and(|d| d.watch.is_none()) {
            // the root's own refresh is the tab's
            return;
        }
        let stale = self.dir(k.slot).is_some_and(|d| d.watch.is_none() && d.load.is_none() && d.published.is_some_and(|t| t.elapsed() >= STALE_AFTER));
        if stale {
            self.relist(k.slot);
        }
    }

    // ---- flatten ---------------------------------------------------------------

    pub fn needs_flatten(&self) -> bool {
        self.dirty
    }

    /// Rebuilds the visible rows from the expanded set. Keeps the cursor on its entry.
    pub fn flatten(&mut self, root: Root) {
        self.dirty = false;
        self.rows.clear();
        self.rows_capped = false;
        let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
        let head_done = |m: &mut TreeModel, slot: u32, root: Root| {
            let Some(d) = m.dir(slot) else { return };
            let depth = d.depth;
            let kind = match (&d.state, slot == 0 && root.is_some()) {
                (State::Loading, false) if d.loading_due || d.started.elapsed() >= LOADING_AFTER => Some(Kind::Loading),
                (State::Denied(_), _) => Some(Kind::Denied),
                (State::Loop(_), _) => Some(Kind::Loop),
                (State::Done, false) if d.store.view().is_empty() && !m.folders_only => Some(Kind::Empty),
                _ => None,
            };
            if let Some(kind) = kind {
                m.rows.push(TRow { slot, pos: 0, depth, kind });
            }
        };
        head_done(self, 0, root);
        while let Some((slot, pos)) = stack.pop() {
            let Some(store) = self.store(slot, root) else { continue };
            let view_len = store.view().len();
            if pos >= view_len {
                // end of this dir: the cap row
                let (capped, total, depth) = self.dir(slot).map(|d| (d.capped && !(slot == 0 && root.is_some()), d.total, d.depth)).unwrap_or((false, 0, 0));
                let _ = total;
                if capped {
                    self.rows.push(TRow { slot, pos: 0, depth, kind: Kind::Capped });
                }
                continue;
            }
            if self.rows.len() >= MAX_ROWS {
                self.rows_capped = true;
                break;
            }
            let id = store.view()[pos];
            let folder = store.record(id).is_some_and(|r| r.flags & FOLDER != 0);
            let depth = self.dir(slot).map(|d| d.depth).unwrap_or(0);
            self.rows.push(TRow { slot, pos: pos as u32, depth, kind: Kind::Entry });
            stack.push((slot, pos + 1));
            if folder && let Some(&child) = self.dir(slot).and_then(|d| d.kids.get(&id)) && self.dir(child).is_some() {
                head_done(self, child, root);
                stack.push((child, 0));
            }
        }
        // the cursor follows its entry
        if let Some(k) = self.cursor
            && self.row_of(k, root).is_none()
        {
            // the entry is hidden by a collapse: the nearest visible ancestor takes it
            self.cursor = self.ancestor_key(k, root);
        }
    }

    fn ancestor_key(&self, k: Key, root: Root) -> Option<Key> {
        let mut slot = k.slot;
        for _ in 0..4096 {
            let d = self.dir(slot)?;
            if slot == 0 {
                return None;
            }
            let (p, pid) = (d.parent, d.parent_id);
            let key = Key { slot: p, id: pid, kind: Kind::Entry };
            if self.row_of(key, root).is_some() {
                return Some(key);
            }
            slot = p;
        }
        None
    }

    /// Row index of a key.
    pub fn row_of(&self, k: Key, root: Root) -> Option<usize> {
        self.rows.iter().position(|r| self.key_of(*r, root) == Some(k))
    }

    pub fn key_of(&self, r: TRow, root: Root) -> Option<Key> {
        match r.kind {
            Kind::Entry => {
                let id = self.store(r.slot, root)?.at(r.pos as usize)?;
                Some(Key { slot: r.slot, id, kind: Kind::Entry })
            }
            k => Some(Key { slot: r.slot, id: u32::MAX, kind: k }),
        }
    }

    pub fn key_at(&self, i: usize, root: Root) -> Option<Key> {
        self.rows.get(i).and_then(|r| self.key_of(*r, root))
    }

    /// The row the cursor is on (index), if it is visible.
    pub fn cursor_row(&self, root: Root) -> Option<usize> {
        self.cursor.and_then(|k| self.row_of(k, root))
    }

    pub fn set_cursor_row(&mut self, i: usize, root: Root) {
        self.cursor = self.key_at(i, root);
        self.watch_dirty = true;
    }

    // ---- row facts ---------------------------------------------------------------

    pub fn entry_id(&self, i: usize, root: Root) -> Option<(u32, EntryId)> {
        let r = self.rows.get(i)?;
        (r.kind == Kind::Entry).then(|| self.store(r.slot, root)?.at(r.pos as usize).map(|id| (r.slot, id))).flatten()
    }

    pub fn name_at<'a>(&'a self, i: usize, root: Root<'a>) -> Option<&'a [u16]> {
        let (slot, id) = self.entry_id(i, root)?;
        self.store(slot, root)?.name(id)
    }

    pub fn is_folder(&self, i: usize, root: Root) -> bool {
        self.entry_id(i, root).and_then(|(s, id)| self.store(s, root)?.record(id)).is_some_and(|r| r.flags & FOLDER != 0)
    }

    pub fn is_link(&self, i: usize, root: Root) -> bool {
        self.entry_id(i, root).and_then(|(s, id)| self.store(s, root)?.record(id)).is_some_and(|r| r.flags & REPARSE != 0)
    }

    /// Folder path of the dir a row belongs to.
    pub fn dir_path(&self, slot: u32) -> Option<&str> {
        if slot == 0 {
            return Some(&self.root);
        }
        self.dir(slot).map(|d| d.path.as_str())
    }

    pub fn dir_fids(&self, slot: u32) -> &[(u128, u32)] {
        self.dir(slot).map(|d| d.fids.as_slice()).unwrap_or(&[])
    }

    pub fn state_of(&self, slot: u32) -> Option<&State> {
        self.dir(slot).map(|d| &d.state)
    }

    pub fn dir_total(&self, slot: u32) -> usize {
        self.dir(slot).map(|d| d.total).unwrap_or(0)
    }

    pub fn dir_shown(&self, slot: u32) -> usize {
        self.dir(slot).map(|d| d.store.len()).unwrap_or(0)
    }

    /// Link target text of an entry row (`None`: not a link, or not resolved yet).
    pub fn link_target(&self, i: usize, root: Root) -> Option<&str> {
        let r = self.rows.get(i)?;
        let (slot, id) = self.entry_id(i, root)?;
        let name = utf8(self.store(slot, root)?.name(id)?);
        let map = if r.slot == 0 && root.is_some() { &self.root_links } else { &self.dir(r.slot)?.links };
        map.get(&name).map(String::as_str)
    }

    /// Full path of an entry row and whether it is a folder.
    pub fn path_of(&self, i: usize, root: Root) -> Option<(String, bool)> {
        let (slot, id) = self.entry_id(i, root)?;
        let store = self.store(slot, root)?;
        let name = utf8(store.name(id)?);
        let dir = store.record(id)?.flags & FOLDER != 0;
        Some((nav::join(self.dir_path(slot)?, &name), dir))
    }

    /// The folder a row stands for: a folder entry, or the dir a pseudo row belongs to.
    pub fn folder_path_of(&self, i: usize, root: Root) -> Option<String> {
        let r = self.rows.get(i)?;
        match r.kind {
            Kind::Entry => self.path_of(i, root).filter(|(_, d)| *d).map(|(p, _)| p),
            _ => self.dir_path(r.slot).map(str::to_string),
        }
    }

    pub fn is_expanded(&self, i: usize, root: Root) -> bool {
        self.entry_id(i, root).is_some_and(|(s, id)| self.dir(s).is_some_and(|d| d.kids.contains_key(&id)))
    }

    /// The expanded folder of a row is still reading (spinner glyph in the chevron).
    #[allow(dead_code)]
    pub fn is_loading(&self, i: usize, root: Root) -> bool {
        let Some((s, id)) = self.entry_id(i, root) else { return false };
        self.dir(s).and_then(|d| d.kids.get(&id)).and_then(|c| self.dir(*c)).is_some_and(|c| c.published.is_none() && matches!(c.state, State::Loading))
    }

    /// Row of the parent folder (the nearest row above with a smaller depth).
    pub fn parent_row(&self, i: usize) -> Option<usize> {
        let depth = self.rows.get(i)?.depth;
        if depth == 0 {
            return None;
        }
        (0..i).rev().find(|&j| self.rows[j].depth < depth && self.rows[j].kind == Kind::Entry)
    }

    /// First child row of an expanded folder row.
    pub fn first_child_row(&self, i: usize) -> Option<usize> {
        let r = self.rows.get(i)?;
        let n = self.rows.get(i + 1)?;
        (n.depth > r.depth).then_some(i + 1)
    }

    // ---- expand and collapse -------------------------------------------------------

    /// Expands the folder on row `i`. `follow`: an explicit action may follow a link.
    pub fn expand(&mut self, i: usize, follow: bool, root: Root) -> Expanded {
        let Some((slot, id)) = self.entry_id(i, root) else { return Expanded::NotFolder };
        let Some(store) = self.store(slot, root) else { return Expanded::NotFolder };
        let Some(rec) = store.record(id) else { return Expanded::NotFolder };
        if rec.flags & FOLDER == 0 {
            return Expanded::NotFolder;
        }
        let link = rec.flags & REPARSE != 0;
        let name = utf8(store.name(id).unwrap_or(&[]));
        if self.dir(slot).is_some_and(|d| d.kids.contains_key(&id)) {
            return Expanded::Already;
        }
        if link && !follow {
            return Expanded::Link;
        }
        if self.rows.len() >= MAX_ROWS {
            self.status = Some(format!("Not expanded: the tree is at its {MAX_ROWS} row limit. Collapse a folder first."));
            return Expanded::Refused;
        }
        self.open_child(slot, id, &name, link, false, root);
        Expanded::Opened
    }

    pub fn collapse(&mut self, i: usize, root: Root) -> bool {
        let Some((slot, id)) = self.entry_id(i, root) else { return false };
        let Some(child) = self.dir(slot).and_then(|d| d.kids.get(&id).copied()) else { return false };
        // the cursor moves onto the folder it was inside
        if self.cursor.is_some_and(|k| self.in_subtree(k.slot, child)) {
            self.cursor = Some(Key { slot, id, kind: Kind::Entry });
        }
        if let Some(d) = self.dir_mut(slot) {
            d.kids.remove(&id);
        }
        self.collapse_slot(child);
        true
    }

    fn in_subtree(&self, slot: u32, root_slot: u32) -> bool {
        let mut cur = slot;
        for _ in 0..4096 {
            if cur == root_slot {
                return true;
            }
            if cur == 0 {
                return false;
            }
            match self.dir(cur) {
                Some(d) => cur = d.parent,
                None => return false,
            }
        }
        false
    }

    /// Frees `slot` and every folder below it (cancels their listings, drops watchers and selection).
    fn collapse_slot(&mut self, slot: u32) {
        if slot == 0 {
            return;
        }
        let mut stack = vec![slot];
        while let Some(s) = stack.pop() {
            let Some(d) = self.dirs.get_mut(s as usize).and_then(|d| d.take()) else { continue };
            stack.extend(d.kids.values().copied());
            self.sel.retain(|(sl, _)| *sl != s);
            if self.cursor.is_some_and(|k| k.slot == s) {
                self.cursor = None;
            }
            if self.anchor.is_some_and(|k| k.slot == s) {
                self.anchor = None;
            }
            self.free.push(s);
            // dropping `d` cancels its enumerator and its watcher
        }
        // trim trailing free slots
        while self.dirs.len() > 1 && self.dirs.last().is_some_and(|d| d.is_none()) {
            let last = (self.dirs.len() - 1) as u32;
            self.dirs.pop();
            self.free.retain(|f| *f != last);
        }
        self.dirty = true;
        self.watch_dirty = true;
    }

    /// Recursive expand from row `i` (`*`, `Shift+Right`): plain folders only, at most `expand_cap`.
    pub fn expand_all(&mut self, i: usize, root: Root) {
        let Some((slot, id)) = self.entry_id(i, root) else { return };
        if !self.is_folder(i, root) || self.is_link(i, root) {
            self.status = Some("Links are not followed by a recursive expand. Open it with Right or Enter first.".into());
            return;
        }
        self.auto_opened = 0;
        self.auto_budget = self.expand_cap;
        self.auto_cut = false;
        self.auto_active = true;
        let name = utf8(self.store(slot, root).and_then(|s| s.name(id)).unwrap_or(&[]));
        let child = match self.dir(slot).and_then(|d| d.kids.get(&id).copied()) {
            Some(c) => c,
            None => {
                self.open_child(slot, id, &name, false, true, root);
                self.auto_opened += 1;
                match self.dir(slot).and_then(|d| d.kids.get(&id).copied()) {
                    Some(c) => c,
                    None => return,
                }
            }
        };
        // an already listed folder expands its folders now; a listing one when it finishes
        let listed = self.dir(child).is_some_and(|d| d.published.is_some());
        if let Some(d) = self.dir_mut(child) {
            d.auto = true;
        }
        if listed {
            self.expand_folders_of(child);
            // folders already open below it continue too
            let kids: Vec<u32> = self.dirs.iter().enumerate().filter(|(_, d)| d.as_ref().is_some_and(|d| d.parent == child)).map(|(s, _)| s as u32).collect();
            for k in kids {
                if let Some(d) = self.dir_mut(k) {
                    d.auto = true;
                }
                if self.dir(k).is_some_and(|d| d.published.is_some()) {
                    self.expand_folders_of(k);
                }
            }
        }
        // nothing left to load: say so now
        if self.dirs.iter().flatten().all(|d| !(d.auto && (d.load.is_some() || d.check.is_some()))) {
            self.on_listed(child);
        }
    }

    /// Opens every folder of `path`'s chain below the root (the pane follows the tab).
    pub fn reveal(&mut self, path: &str) {
        self.follow = Some(path.to_string());
        self.followed = Some(path.to_string());
        let mut chain: Vec<String> = Vec::new();
        let mut cur = path.to_string();
        for _ in 0..256 {
            if same_path(&cur, &self.root) {
                break;
            }
            chain.push(cur.clone());
            match nav::parent(&cur) {
                Some(p) => cur = p,
                None => break,
            }
        }
        // the target itself is only selected, its ancestors open
        if !chain.is_empty() {
            chain.remove(0);
        }
        chain.reverse();
        self.restore = chain;
        self.reselect = vec![path.to_string()];
        self.sel.clear();
        let slots: Vec<u32> = self.dirs.iter().enumerate().filter(|(_, d)| d.as_ref().is_some_and(|d| d.published.is_some())).map(|(s, _)| s as u32).collect();
        for s in slots {
            self.apply_restore(s);
        }
        self.dirty = true;
    }

    pub fn followed(&self) -> Option<&str> {
        self.followed.as_deref()
    }

    /// The pane's own click moved the tab here: nothing to reveal.
    pub fn mark_following(&mut self, path: &str) {
        self.followed = Some(path.to_string());
        self.follow = None;
    }

    /// The store of `slot` (the external root for slot 0 when given).
    pub fn store_of<'a>(&'a self, slot: u32, root: Root<'a>) -> Option<&'a EntryStore> {
        self.store(slot, root)
    }

    /// Adds row `i` to the selection (marquee).
    pub fn add_selection(&mut self, i: usize, root: Root) {
        if let Some(k) = self.entry_id(i, root) {
            self.sel.insert(k);
        }
    }

    // ---- selection -------------------------------------------------------------------

    pub fn is_selected(&self, i: usize, root: Root) -> bool {
        self.entry_id(i, root).is_some_and(|k| self.sel.contains(&k))
    }

    pub fn sel_count(&self) -> usize {
        self.sel.len()
    }

    pub fn clear_selection(&mut self) {
        self.sel.clear();
    }

    /// Plain click or cursor move: selects only this row.
    pub fn select_single(&mut self, i: usize, root: Root) {
        self.sel.clear();
        if let Some(k) = self.entry_id(i, root) {
            self.sel.insert(k);
        }
        self.cursor = self.key_at(i, root);
        self.anchor = self.cursor;
        self.watch_dirty = true;
    }

    pub fn toggle(&mut self, i: usize, root: Root) {
        if let Some(k) = self.entry_id(i, root)
            && !self.sel.remove(&k)
        {
            self.sel.insert(k);
        }
        self.cursor = self.key_at(i, root);
        self.anchor = self.cursor;
    }

    /// Shift: the rows between the anchor and `i`.
    pub fn extend(&mut self, i: usize, root: Root) {
        let a = self.anchor.and_then(|k| self.row_of(k, root)).unwrap_or(i);
        let (lo, hi) = (a.min(i), a.max(i));
        self.sel.clear();
        for j in lo..=hi {
            if let Some(k) = self.entry_id(j, root) {
                self.sel.insert(k);
            }
        }
        self.cursor = self.key_at(i, root);
        self.watch_dirty = true;
    }

    #[allow(dead_code)]
    pub fn select_all(&mut self, root: Root) {
        self.sel.clear();
        for j in 0..self.rows.len() {
            if let Some(k) = self.entry_id(j, root) {
                self.sel.insert(k);
            }
        }
    }

    /// Row of the entry `path` (an entry row of the dir it lives in), if it is visible.
    pub fn row_of_path(&self, path: &str, root: Root) -> Option<usize> {
        let parent = nav::parent(path)?;
        let name: Vec<u16> = path.trim_end_matches('\\').rsplit('\\').next().unwrap_or("").encode_utf16().collect();
        (0..self.rows.len()).find(|&i| {
            let r = self.rows[i];
            r.kind == Kind::Entry && self.dir_path(r.slot).is_some_and(|d| same_path(d, &parent)) && self.name_at(i, root).is_some_and(|n| n.len() == name.len() && n.iter().zip(&name).all(|(a, b)| a == b || lower_u16(*a) == lower_u16(*b)))
        })
    }

    /// The store of the dir that holds row `i` (its siblings), for name checks.
    pub fn siblings_of<'a>(&'a self, i: usize, root: Root<'a>) -> Option<&'a EntryStore> {
        let r = self.rows.get(i)?;
        self.store(r.slot, root)
    }

    /// The folder a drop on row `i` lands in: a folder row itself, a file row's own folder.
    pub fn drop_folder_of(&self, i: usize, root: Root) -> Option<String> {
        let r = self.rows.get(i)?;
        match r.kind {
            Kind::Entry => match self.path_of(i, root)? {
                (p, true) => Some(p),
                _ => self.dir_path(r.slot).map(str::to_string),
            },
            _ => self.dir_path(r.slot).map(str::to_string),
        }
    }

    /// Select the entry `path` once its row exists (a rename just made it).
    pub fn want_pick(&mut self, path: &str) {
        self.pick = Some((path.to_string(), Instant::now()));
        self.dirty = true;
    }

    fn try_pick(&mut self, root: Root) {
        let Some((path, since)) = self.pick.clone() else { return };
        if since.elapsed() > Duration::from_secs(5) {
            self.pick = None;
            return;
        }
        if self.needs_flatten() {
            self.flatten(root);
        }
        if let Some(i) = self.row_of_path(&path, root) {
            self.pick = None;
            self.select_single(i, root);
            self.dirty = true;
        }
    }

    /// Paths of up to `n` selected rows in row order.
    pub fn selected_paths(&self, n: usize, root: Root) -> Vec<String> {
        (0..self.rows.len()).filter(|&i| self.is_selected(i, root)).take(n).filter_map(|i| self.path_of(i, root).map(|p| p.0)).collect()
    }

    pub fn selected_total_size(&self, root: Root) -> u64 {
        (0..self.rows.len()).filter(|&i| self.is_selected(i, root)).filter_map(|i| self.entry_id(i, root).and_then(|(s, id)| self.store(s, root)?.record(id)).filter(|r| r.flags & FOLDER == 0).map(|r| r.size)).sum()
    }

    // ---- type-ahead -----------------------------------------------------------------------

    /// Jumps to the next entry row whose name starts with what was typed in the last second.
    pub fn type_ahead(&mut self, c: char, from: usize, root: Root) -> Option<usize> {
        let now = Instant::now();
        if self.typed_at.is_none_or(|t| now.duration_since(t) > TYPE_AHEAD) {
            self.typed.clear();
        }
        self.typed_at = Some(now);
        self.typed.extend(c.to_lowercase());
        let typed: Vec<char> = self.typed.chars().collect();
        let cycling = typed.len() > 1 && typed.iter().all(|x| *x == typed[0]);
        let want: Vec<char> = if cycling { vec![typed[0]] } else { typed };
        let n = self.rows.len();
        // the same letter again moves on; a longer prefix may stay on the current row
        let start = if (self.typed.chars().count() == 1 || cycling) && self.cursor.is_some() { from + 1 } else { from };
        for k in 0..n {
            let j = (start + k) % n.max(1);
            if let Some(name) = self.name_at(j, root) {
                let s = utf8(name);
                let mut it = s.chars().map(lower);
                if want.iter().all(|w| it.next() == Some(*w)) {
                    return Some(j);
                }
            }
        }
        None
    }

    // ---- persistence --------------------------------------------------------------------------

    /// Paths of the expanded folders (parents before children), capped.
    #[allow(dead_code)]
    pub fn expanded_paths(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in &self.rows {
            let _ = r;
        }
        // depth-first order = order of the rows
        let mut seen: HashSet<u32> = HashSet::new();
        for (i, r) in self.rows.iter().enumerate() {
            if r.kind != Kind::Entry {
                continue;
            }
            let _ = i;
            if let Some(d) = self.dir(r.slot)
                && let Some(id) = self.store(r.slot, None).and_then(|s| s.at(r.pos as usize))
                && let Some(&c) = d.kids.get(&id)
                && seen.insert(c)
                && let Some(cd) = self.dir(c)
            {
                out.push(cd.path.clone());
            }
        }
        out.truncate(SESSION_CAP);
        out
    }

    /// `expanded_paths` for Tree rows (the root store comes from the tab).
    pub fn expanded_paths_in(&self, root: Root) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for i in 0..self.rows.len() {
            if let Some((s, id)) = self.entry_id(i, root)
                && let Some(&c) = self.dir(s).and_then(|d| d.kids.get(&id))
                && let Some(cd) = self.dir(c)
            {
                out.push(cd.path.clone());
            }
        }
        out.truncate(SESSION_CAP);
        out
    }

    /// Queues folders to re-expand and entries to re-select as their listings arrive
    /// (session restore, refresh of the root).
    pub fn seed(&mut self, expand: Vec<String>, select: Vec<String>) {
        let mut ex: Vec<String> = expand.into_iter().filter(|p| nav::parent(p).is_some()).collect();
        ex.truncate(RESTORE_CAP);
        self.restore = ex;
        self.reselect = select;
        self.dirty = true;
    }

    #[allow(dead_code)]
    pub fn has_pending_restore(&self) -> bool {
        !self.restore.is_empty() || !self.reselect.is_empty()
    }
}

/// Names of the reparse points of a store (capped).
fn reparse_names(store: &EntryStore) -> Vec<String> {
    store
        .view()
        .iter()
        .filter(|&&id| store.record(id).is_some_and(|r| r.flags & REPARSE != 0))
        .take(LINK_CAP)
        .filter_map(|&id| store.name(id).map(utf8))
        .collect()
}

/// Reads the targets of `names` in `dir` on a worker.
fn spawn_links(dir: String, names: Vec<String>, wake: Wake) -> Receiver<Vec<(String, String)>> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new().name("tsubame-tree-links".into()).spawn(move || {
        let out: Vec<(String, String)> = names
            .into_iter()
            .filter_map(|n| {
                let p = nav::join(&dir, &n);
                std::fs::read_link(&p).ok().map(|t| (n, tsubame_win::fileid::strip_verbatim(&t.to_string_lossy())))
            })
            .collect();
        let _ = tx.send(out);
        wake();
    });
    rx
}

// ---- the tab's side of Tree rows ------------------------------------------------

impl crate::tab::Tab {
    /// Builds the Tree rows model for this tab's folder when it has none.
    pub fn ensure_tree(&mut self, cfg: &tsubame_config::TreeConfig) {
        if self.settings || self.synthetic || self.is_archive() {
            self.tree = None;
            return;
        }
        if self.tree.is_some() {
            return;
        }
        let Some(wake) = self.wake_handle() else { return };
        let mut m = TreeModel::new(&self.path, false, false, wake, self.sort, self.store.hide());
        m.set_limits(cfg.expand_cap as usize, cfg.child_cap as usize);
        let (ex, sel) = std::mem::take(&mut self.tree_seed);
        m.seed(ex, sel);
        self.tree = Some(Box::new(m));
        self.tree_dirty = true;
    }

    /// Moves what the folder workers delivered into the model and re-flattens
    /// when needed. UI thread, no disk. True when rows changed.
    pub fn pump_tree(&mut self) -> bool {
        let done = self.load == crate::tab::Load::Done;
        let Some(m) = self.tree.as_mut() else { return false };
        m.set_rules(self.sort, self.store.hide());
        let mut changed = m.pump(Some(&self.store), done);
        if std::mem::take(&mut self.tree_dirty) {
            m.root_changed();
        }
        if m.needs_flatten() {
            m.flatten(Some(&self.store));
            changed = true;
        }
        changed
    }

    /// Leaving Tree rows: the cursor goes to the top-level entry it was inside.
    pub fn cursor_to_root_row(&mut self) {
        let Some(m) = &self.tree else { return };
        let mut i = self.list.cursor;
        while let Some(p) = m.parent_row(i) {
            i = p;
        }
        if let Some((0, id)) = m.entry_id(i, Some(&self.store))
            && let Some(pos) = self.store.view().iter().position(|v| *v == id)
        {
            self.list.cursor = pos;
        }
    }

    /// Leaves Tree rows: the expanded folders wait in the seed for the way back.
    pub fn leave_tree(&mut self) {
        self.tree_on = false;
        if let Some(m) = self.tree.take() {
            self.tree_seed = (m.expanded_paths_in(Some(&self.store)), m.selected_paths(64, Some(&self.store)));
        }
    }

    /// Expanded folders for the session file.
    pub fn tree_paths(&self) -> Vec<String> {
        match &self.tree {
            Some(m) => m.expanded_paths_in(Some(&self.store)),
            None => self.tree_seed.0.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tsubame_core::Column;

    fn sort() -> Sort {
        Sort { column: Column::Name, descending: false, folders_first: true }
    }

    fn wake() -> Wake {
        Arc::new(|| {})
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-tree-{name}-{}", std::process::id()));
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
        std::fs::write(d.join("a").join("f.txt"), b"x").unwrap();
        std::fs::write(d.join("b").join("b.txt"), b"x").unwrap();
        std::fs::write(d.join("z.txt"), b"x").unwrap();
        d
    }

    /// A model whose root is listed by the model itself (pane-style, all entries).
    fn model(root: &std::path::Path, folders_only: bool) -> TreeModel {
        let mut m = TreeModel::new(root.to_str().unwrap(), folders_only, true, wake(), sort(), 0);
        settle(&mut m);
        m
    }

    fn settle(m: &mut TreeModel) {
        let t0 = Instant::now();
        loop {
            m.pump(None, false);
            m.flatten(None);
            if m.loading_count() == 0 {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "listing did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        m.pump(None, false);
        m.flatten(None);
    }

    fn names(m: &TreeModel) -> Vec<String> {
        (0..m.len())
            .map(|i| match m.row(i).unwrap().kind {
                Kind::Entry => format!("{}{}", "  ".repeat(m.row(i).unwrap().depth as usize), utf8(m.name_at(i, None).unwrap())),
                k => format!("{}<{k:?}>", "  ".repeat(m.row(i).unwrap().depth as usize)),
            })
            .collect()
    }

    fn row_named(m: &TreeModel, n: &str) -> usize {
        (0..m.len()).find(|&i| m.name_at(i, None).is_some_and(|x| utf8(x) == n)).unwrap_or_else(|| panic!("no row {n}: {:?}", names(m)))
    }

    #[test]
    fn indent_is_16_then_6_per_level_past_12() {
        assert_eq!(indent(0), 0.0);
        assert_eq!(indent(3), 48.0);
        assert_eq!(indent(12), 192.0);
        assert_eq!(indent(13), 198.0);
        assert_eq!(indent(20), 192.0 + 8.0 * 6.0);
    }

    #[test]
    fn a_row_is_small() {
        assert!(std::mem::size_of::<TRow>() <= 12);
    }

    #[test]
    fn expand_and_collapse_flatten_depth_first_and_sorted() {
        let d = fixture("flatten");
        let mut m = model(&d, false);
        assert_eq!(names(&m), ["a", "b", "empty", "z.txt"]);
        let a = row_named(&m, "a");
        assert_eq!(m.expand(a, false, None), Expanded::Opened);
        settle(&mut m);
        assert_eq!(names(&m), ["a", "  a1", "  a2", "  f.txt", "b", "empty", "z.txt"]);
        let a1 = row_named(&m, "a1");
        m.expand(a1, false, None);
        settle(&mut m);
        assert_eq!(names(&m)[..4], ["a", "  a1", "    deep.txt", "  a2"]);
        // collapse frees the children at once
        assert!(m.collapse(row_named(&m, "a"), None));
        m.flatten(None);
        assert_eq!(names(&m), ["a", "b", "empty", "z.txt"]);
        assert_eq!(m.expanded_count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_folder_gets_an_empty_row() {
        let d = fixture("empty");
        let mut m = model(&d, false);
        m.expand(row_named(&m, "empty"), false, None);
        settle(&mut m);
        assert_eq!(names(&m), ["a", "b", "empty", "  <Empty>", "z.txt"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn folders_only_drops_files() {
        let d = fixture("folders");
        let mut m = model(&d, true);
        assert_eq!(names(&m), ["a", "b", "empty"]);
        m.expand(row_named(&m, "a"), false, None);
        settle(&mut m);
        assert_eq!(names(&m), ["a", "  a1", "  a2", "b", "empty"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_child_cap_cuts_and_counts() {
        let d = temp("childcap");
        let big = d.join("big");
        std::fs::create_dir_all(&big).unwrap();
        for i in 0..40 {
            std::fs::write(big.join(format!("f{i:02}.txt")), b"x").unwrap();
        }
        let mut m = TreeModel::new(d.to_str().unwrap(), false, true, wake(), sort(), 0);
        m.set_limits(500, 25);
        settle(&mut m);
        m.expand(row_named(&m, "big"), false, None);
        settle(&mut m);
        let slot = m.row(1).unwrap().slot;
        assert_eq!(m.dir_shown(slot), 25);
        assert_eq!(m.dir_total(slot), 40);
        let last = m.row(m.len() - 1).unwrap();
        assert_eq!(last.kind, Kind::Capped);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn recursive_expand_stops_at_the_cap_and_says_so() {
        let d = temp("expandcap");
        for i in 0..6 {
            std::fs::create_dir_all(d.join("top").join(format!("s{i}")).join("t")).unwrap();
        }
        let mut m = TreeModel::new(d.to_str().unwrap(), true, true, wake(), sort(), 0);
        m.set_limits(4, 5000);
        settle(&mut m);
        m.expand_all(row_named(&m, "top"), None);
        settle(&mut m);
        // the cap counts the folder expand_all started on: 4 folders opened in all
        assert_eq!(m.expanded_count(), 4);
        let s = m.take_status().expect("status says it stopped");
        assert!(s.contains("Expanded 4 folders") && s.contains("cap"), "{s}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn recursive_expand_below_the_cap_says_how_many() {
        let d = fixture("expandall");
        let mut m = model(&d, true);
        m.expand_all(row_named(&m, "a"), None);
        settle(&mut m);
        assert_eq!(names(&m), ["a", "  a1", "  a2", "b", "empty"]);
        assert_eq!(m.take_status().as_deref(), Some("Expanded 3 folders (links not followed)"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_junction_is_not_followed_by_a_plain_expand_or_recursion() {
        let d = temp("junction");
        std::fs::create_dir_all(d.join("real").join("x")).unwrap();
        let link = d.join("link");
        let ok = { use std::os::windows::process::CommandExt; std::process::Command::new("cmd").args(["/C", "mklink", "/J", link.to_str().unwrap(), d.join("real").to_str().unwrap()]).creation_flags(0x0800_0000).output() }.map(|o| o.status.success()).unwrap_or(false);
        if !ok {
            eprintln!("mklink /J unavailable: skipped");
            return;
        }
        let mut m = model(&d, false);
        let l = row_named(&m, "link");
        assert!(m.is_link(l, None));
        assert_eq!(m.expand(l, false, None), Expanded::Link);
        m.expand_all(row_named(&m, "real"), None);
        settle(&mut m);
        assert!(m.expanded_count() >= 1);
        assert!(!m.is_expanded(row_named(&m, "link"), None), "recursion never follows links");
        // an explicit follow works
        assert_eq!(m.expand(row_named(&m, "link"), true, None), Expanded::Opened);
        settle(&mut m);
        assert!(m.is_expanded(row_named(&m, "link"), None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_junction_back_to_an_ancestor_does_not_expand() {
        let d = temp("loop");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let link = d.join("sub").join("up");
        let ok = { use std::os::windows::process::CommandExt; std::process::Command::new("cmd").args(["/C", "mklink", "/J", link.to_str().unwrap(), d.to_str().unwrap()]).creation_flags(0x0800_0000).output() }.map(|o| o.status.success()).unwrap_or(false);
        if !ok {
            eprintln!("mklink /J unavailable: skipped");
            return;
        }
        let mut m = model(&d, false);
        m.expand(row_named(&m, "sub"), false, None);
        settle(&mut m);
        let up = row_named(&m, "up");
        assert_eq!(m.expand(up, true, None), Expanded::Opened);
        settle(&mut m);
        let i = row_named(&m, "up");
        let kind = m.row(i + 1).unwrap().kind;
        assert_eq!(kind, Kind::Loop, "{:?}", names(&m));
        let slot = m.row(i + 1).unwrap().slot;
        match m.state_of(slot) {
            Some(State::Loop(p)) => assert!(same_path(p, d.to_str().unwrap()), "{p}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn access_denied_is_a_row_not_a_failure() {
        let d = temp("denied");
        let mut m = model(&d, false);
        // a folder that is gone behaves like a denied one: the error is the row's text
        std::fs::create_dir_all(d.join("vanish")).unwrap();
        m.relist(0);
        settle(&mut m);
        let v = row_named(&m, "vanish");
        std::fs::remove_dir_all(d.join("vanish")).unwrap();
        m.expand(v, false, None);
        settle(&mut m);
        assert!(matches!(m.row(v + 1).unwrap().kind, Kind::Denied), "{:?}", names(&m));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn selection_survives_expand_collapse_and_refresh() {
        let d = fixture("selection");
        let mut m = model(&d, false);
        m.expand(row_named(&m, "a"), false, None);
        settle(&mut m);
        let f = row_named(&m, "f.txt");
        m.select_single(f, None);
        // expanding another folder above shifts the row but not the selection
        m.expand(row_named(&m, "a1"), false, None);
        settle(&mut m);
        let f2 = row_named(&m, "f.txt");
        assert!(f2 > f && m.is_selected(f2, None));
        assert_eq!(m.cursor_row(None), Some(f2));
        // a refresh of the folder keeps it
        let slot = m.row(f2).unwrap().slot;
        m.relist(slot);
        settle(&mut m);
        let f3 = row_named(&m, "f.txt");
        assert!(m.is_selected(f3, None), "selection follows the entry by name");
        assert_eq!(m.cursor_row(None), Some(f3));
        assert!(m.is_expanded(row_named(&m, "a1"), None), "expansions survive a refresh");
        // collapsing the parent moves the cursor onto it and drops the selection inside
        m.collapse(row_named(&m, "a"), None);
        m.flatten(None);
        assert_eq!(m.cursor_row(None), Some(row_named(&m, "a")));
        assert_eq!(m.sel_count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn type_ahead_jumps_by_prefix_and_cycles_one_letter() {
        let d = fixture("typeahead");
        std::fs::write(d.join("alpha.txt"), b"x").unwrap();
        let mut m = model(&d, false);
        m.relist(0);
        settle(&mut m);
        let first = m.type_ahead('a', 0, None).unwrap();
        assert_eq!(utf8(m.name_at(first, None).unwrap()), "a");
        m.select_single(first, None);
        m.typed_at = Some(Instant::now() - Duration::from_secs(2));
        let next = m.type_ahead('a', first, None).unwrap();
        assert_eq!(utf8(m.name_at(next, None).unwrap()), "alpha.txt");
        // a longer prefix narrows
        let nxt = m.type_ahead('l', next, None).unwrap();
        assert_eq!(nxt, next);
        m.typed_at = Some(Instant::now() - Duration::from_secs(2));
        assert!(m.type_ahead('q', 0, None).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn seed_expands_the_saved_folders_and_selects_names() {
        let d = fixture("seed");
        let mut m = TreeModel::new(d.to_str().unwrap(), false, true, wake(), sort(), 0);
        let a = d.join("a");
        let a1 = a.join("a1");
        m.seed(vec![a.to_str().unwrap().to_string(), a1.to_str().unwrap().to_string()], vec![a.join("f.txt").to_str().unwrap().to_string()]);
        settle(&mut m);
        assert_eq!(names(&m)[..4], ["a", "  a1", "    deep.txt", "  a2"]);
        assert!(m.is_selected(row_named(&m, "f.txt"), None));
        let saved = m.expanded_paths_in(None);
        assert_eq!(saved.len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn collapse_cancels_a_running_listing_and_frees_the_slot() {
        let d = fixture("cancel");
        let mut m = model(&d, false);
        m.expand(row_named(&m, "a"), false, None);
        assert!(m.loading_count() >= 1);
        m.collapse(row_named(&m, "a"), None);
        assert_eq!(m.expanded_count(), 0);
        assert_eq!(m.loading_count(), 0);
        settle(&mut m);
        assert_eq!(names(&m), ["a", "b", "empty", "z.txt"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn at_most_32_folders_keep_a_watcher() {
        let d = temp("watchcap");
        for i in 0..40 {
            std::fs::create_dir_all(d.join(format!("d{i:02}"))).unwrap();
        }
        let mut m = TreeModel::new(d.to_str().unwrap(), true, true, wake(), sort(), 0);
        settle(&mut m);
        for i in 0..40 {
            let r = row_named(&m, &format!("d{i:02}"));
            m.expand(r, false, None);
            settle(&mut m);
        }
        m.watch_dirty = true;
        m.pump(None, false);
        assert!(m.watched_count() <= MAX_WATCHED, "{}", m.watched_count());
        assert!(m.watched_count() >= 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_watched_folder_picks_up_a_new_file_without_a_relist() {
        let d = fixture("watch");
        let mut m = model(&d, false);
        m.expand(row_named(&m, "b"), false, None);
        settle(&mut m);
        m.set_cursor_row(row_named(&m, "b.txt"), None);
        m.watch_dirty = true;
        m.pump(None, false);
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(d.join("b").join("new.txt"), b"x").unwrap();
        let t0 = Instant::now();
        loop {
            m.pump(None, false);
            m.flatten(None);
            if names(&m).iter().any(|n| n.trim() == "new.txt") {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "watcher did not deliver: {:?}", names(&m));
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rows_are_capped_at_50000() {
        // the cap is a constant of the model; the flatten loop stops there
        assert_eq!(MAX_ROWS, 50_000);
    }

    #[test]
    fn parent_and_first_child_rows() {
        let d = fixture("nav");
        let mut m = model(&d, false);
        m.expand(row_named(&m, "a"), false, None);
        settle(&mut m);
        let a = row_named(&m, "a");
        let a1 = row_named(&m, "a1");
        assert_eq!(m.parent_row(a1), Some(a));
        assert_eq!(m.first_child_row(a), Some(a + 1));
        assert_eq!(m.parent_row(a), None);
        assert_eq!(m.first_child_row(row_named(&m, "b")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn row_of_path_finds_an_entry_row_by_full_path_ignoring_case() {
        let d = fixture("rowofpath");
        let mut m = model(&d, false);
        let a = row_named(&m, "a");
        m.expand(a, false, None);
        settle(&mut m);
        let f = d.join("a").join("F.TXT");
        assert_eq!(m.row_of_path(f.to_str().unwrap(), None), Some(row_named(&m, "f.txt")));
        assert_eq!(m.row_of_path(d.join("z.txt").to_str().unwrap(), None), Some(row_named(&m, "z.txt")));
        assert_eq!(m.row_of_path(d.join("a").join("nope").to_str().unwrap(), None), None);
        // a folder collapsed hides its entries: no row
        let deep = d.join("a").join("a1").join("deep.txt");
        assert_eq!(m.row_of_path(deep.to_str().unwrap(), None), None);
        assert!(m.siblings_of(row_named(&m, "f.txt"), None).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_drop_on_a_folder_row_goes_into_it_and_on_a_file_row_into_its_folder() {
        let d = fixture("dropfolder");
        let mut m = model(&d, false);
        let a = row_named(&m, "a");
        m.expand(a, false, None);
        settle(&mut m);
        let same = |p: Option<String>, want: &std::path::Path| p.is_some_and(|p| same_path(&p, want.to_str().unwrap()));
        assert!(same(m.drop_folder_of(row_named(&m, "a1"), None), &d.join("a").join("a1")), "folder row: itself");
        assert!(same(m.drop_folder_of(row_named(&m, "f.txt"), None), &d.join("a")), "file in a: a");
        assert!(same(m.drop_folder_of(row_named(&m, "z.txt"), None), &d), "file in the root: the root");
        assert_eq!(m.drop_folder_of(999, None), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn want_pick_selects_the_row_once_it_shows() {
        let d = fixture("pick");
        let mut m = model(&d, false);
        let a = row_named(&m, "a");
        m.expand(a, false, None);
        settle(&mut m);
        let f = d.join("a").join("f.txt");
        m.want_pick(f.to_str().unwrap());
        m.pump(None, false);
        assert!(m.is_selected(row_named(&m, "f.txt"), None));
        assert_eq!(m.sel_count(), 1);
        assert!(m.pick.is_none(), "one shot");
        // a path that has no row yet is kept while it waits
        m.want_pick(d.join("a").join("later.txt").to_str().unwrap());
        m.pump(None, false);
        assert!(m.pick.is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ctrl_shift_and_select_all_build_a_set_that_reports_its_paths() {
        let d = fixture("multi");
        let mut m = model(&d, false);
        m.select_single(0, None);
        m.toggle(2, None);
        assert_eq!(m.sel_count(), 2);
        let p = m.selected_paths(64, None);
        assert!(p[0].ends_with("a") && p[1].ends_with("empty"), "{p:?}");
        m.select_single(0, None);
        m.extend(3, None);
        assert_eq!(m.sel_count(), 4);
        m.clear_selection();
        assert_eq!(m.sel_count(), 0);
        m.select_all(None);
        assert_eq!(m.sel_count(), 4);
        let _ = std::fs::remove_dir_all(&d);
    }
}
