//! One tab: its folder, history, entry store, selection, sort, filter and
//! list scroll, plus the running enumeration that feeds the store.
//!
//! Closing a tab drops it: the store is freed and the enumerator cancelled
//! (its worker sees the flag, closes the directory handle and exits).
//! Nothing here touches the disk; the enumerator worker does, and the UI
//! thread only moves finished batches into the store (`pump`).

use crate::nav::{self, History};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};
use tsubame_core::{EntryInput, EntryStore, FOLDER, FilterMode, HIDDEN, READONLY, REPARSE, SYSTEM, Selection, Sort};
use tsubame_ui::ListState;
use tsubame_win::enumerate::{Batch, CancelToken, Done, Enumerator, Message, Wake};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    Loading,
    Done,
    /// the folder could not be listed: message for the status bar / empty state
    Failed(String),
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
}

impl Timing {
    fn new() -> Timing {
        Timing { t_nav: Instant::now(), first_batch: None, done: None, first_frame: None, final_frame: None, worker: None }
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
    pub load: Load,
    /// entries the enumerator delivered so far
    pub delivered: usize,
    pub timing: Timing,
    token: CancelToken,
    enum_gen: u64,
    enumerator: Option<Enumerator>,
    rx: Option<Receiver<Message>>,
}

/// What `pump` found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pumped {
    /// the store or the load state changed: repaint
    pub changed: bool,
    /// this call handled the Done message
    pub done: bool,
}

fn flags_of(attrs: u32, is_dir: bool, reparse: bool) -> u16 {
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

impl Tab {
    pub fn new(id: u64, path: &str, row_h: f32) -> Tab {
        Tab {
            id,
            path: path.to_string(),
            history: History::new(path),
            store: EntryStore::new(),
            sel: Selection::new(),
            sort: Sort { column: tsubame_core::Column::Name, descending: false, folders_first: true },
            filter: String::new(),
            list: ListState::new(row_h),
            load: Load::Done,
            delivered: 0,
            timing: Timing::new(),
            token: CancelToken::new(),
            enum_gen: 0,
            enumerator: None,
            rx: None,
        }
    }

    /// Short title for the tab strip: the folder's own name.
    pub fn title(&self) -> String {
        nav::crumbs(&self.path).pop().map(|c| c.label).unwrap_or_else(|| self.path.clone())
    }

    pub fn is_loading(&self) -> bool {
        self.load == Load::Loading
    }

    /// Lists `path` (the history is the caller's business): drops the old
    /// listing, cancels its worker, starts a new one. Stale batches of the
    /// old one can never reach the store: the channel is replaced and every
    /// message carries its generation.
    pub fn start(&mut self, path: &str, wake: Wake) {
        self.enumerator = None; // cancels and detaches
        self.rx = None;
        self.path = path.to_string();
        self.store.clear();
        self.store.sort(self.sort);
        self.sel.clear();
        self.filter.clear();
        self.list.scroll = 0.0;
        self.list.cursor = 0;
        self.delivered = 0;
        self.load = Load::Loading;
        self.timing = Timing::new();
        self.enum_gen = self.token.next_generation();
        let (e, rx) = Enumerator::start_channel(path, self.enum_gen, self.token.clone(), wake);
        self.enumerator = Some(e);
        self.rx = Some(rx);
    }

    /// Moves everything the worker has delivered into the store. UI thread.
    pub fn pump(&mut self) -> Pumped {
        let mut out = Pumped::default();
        loop {
            let Some(rx) = &self.rx else { break };
            match rx.try_recv() {
                Ok(msg) => {
                    if msg.generation() != self.enum_gen {
                        continue; // stale: from a listing this tab no longer shows
                    }
                    match msg {
                        Message::Batch(b) => {
                            self.push(&b);
                            out.changed = true;
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
        out
    }

    fn push(&mut self, b: &Batch) {
        let inputs: Vec<EntryInput> =
            b.iter().map(|(name, e)| EntryInput { name, size: e.size, modified: e.mtime as i64, flags: flags_of(e.attrs, e.is_dir(), e.is_reparse_point()) }).collect();
        // generation is the store's own (bumped by `start`): only this tab's
        // current listing ever gets here, so it always matches
        let _ = self.store.push_batch(self.store.generation(), &inputs);
        self.delivered += b.len();
        if self.timing.first_batch.is_none() {
            self.timing.first_batch = Some(self.timing.t_nav.elapsed());
        }
    }

    fn finish(&mut self, d: Done) {
        self.timing.done = Some(self.timing.t_nav.elapsed());
        self.timing.worker = Some(d.elapsed);
        self.load = match d.error {
            Some(e) if !d.cancelled => Load::Failed(e.message()),
            _ => Load::Done,
        };
        self.rx = None;
        self.enumerator = None;
    }

    /// Sort by `col`: a second click on the same column flips the direction.
    pub fn sort_by(&mut self, col: tsubame_core::Column) {
        self.sort = if self.sort.column == col { Sort { descending: !self.sort.descending, ..self.sort } } else { Sort { column: col, descending: false, folders_first: true } };
        self.store.sort(self.sort);
        self.list.scroll = 0.0;
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
        self.list.scroll = 0.0;
        self.list.cursor = 0;
        self.sel.clear();
    }

    /// Entries shown (after the filter).
    pub fn shown(&self) -> usize {
        self.store.view().len()
    }

    /// Full path of the entry at view position `pos`, and whether it is a folder.
    pub fn entry_at(&self, pos: usize) -> Option<(String, bool)> {
        let id = self.store.at(pos)?;
        let name = String::from_utf16_lossy(self.store.name(id)?);
        let dir = self.store.record(id)?.flags & FOLDER != 0;
        Some((nav::join(&self.path, &name), dir))
    }

    /// Paths of up to `n` selected entries in view order (for the shell menu).
    pub fn selected_paths(&self, n: usize) -> Vec<String> {
        self.store.view().iter().enumerate().filter(|(_, id)| self.sel.contains(**id)).take(n).filter_map(|(pos, _)| self.entry_at(pos).map(|e| e.0)).collect()
    }

    /// Heap bytes this tab holds in its store (diagnostics).
    pub fn memory_bytes(&self) -> usize {
        self.store.memory_bytes()
    }
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
        assert_eq!(t.shown(), 2);
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
}
