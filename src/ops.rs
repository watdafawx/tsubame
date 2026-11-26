//! The one owner of file changes the window makes: every copy, move, rename,
//! delete, paste, drop, new folder, undo and redo goes through `Ops`, so there
//! is ONE `tsubame_win::fileops` queue and ONE undo/redo journal.
//!
//! - the queue (and its worker thread) starts on the first operation, so a
//!   window that never copies pays nothing;
//! - a submission is one undo step: Ctrl+Z reverts the newest submission
//!   whatever it was (a paste of 40 files, a batch rename, a delete of a
//!   selection), Ctrl+Y redoes it (`FileOpQueue::undo_group` / `redo_group`);
//! - events are drained on the UI thread by main.rs's `OPS_TIMER`, which
//!   exists only while `busy()`: idle stays at zero wake-ups. The timer
//!   handler (`shellops::on_ops_timer`) asks conflicts with a dialog, outside
//!   the app borrow; everything else is `on_event` here;
//! - `message` is the status bar's file-operation text (progress, result,
//!   errors, clipboard results). One field, one writer path, so it never
//!   shows twice and a newer message always replaces an older one;
//! - after a job, the listings follow by themselves (the tabs' directory
//!   watchers); `take_finished` says which folders changed and what to select,
//!   for tabs without a watcher and for the selection of new items;
//! - `panel` is the Operations panel's model (`ops_panel`): every job gets a
//!   card, fed from the same events. `conflict` is the open conflict dialog
//!   (`conflict_dialog`); while it is up the event timer stops (the worker
//!   waits for the answer) and a second question (an extraction) queues in
//!   `waiting`.

use crate::conflict_dialog::{self, Answer, Dialog, Reply};
use crate::dnd::Step;
use crate::nav;
use crate::ops_panel::{Act, Card, Key, Panel, State, Stop};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;
use tsubame_win::fileops::{Cancellation, ConflictReply, Event, FileMeta, FileOpQueue, FsBackend, Operation, Totals};

/// What a submission is, for the status bar and for what happens after it.
#[derive(Clone, Debug)]
pub struct Job {
    /// "Copying", "Deleting", ... (progress)
    pub verb: &'static str,
    /// "Copied", "Deleted", ... (result)
    pub done: &'static str,
    /// operations submitted (the result counts these, not files inside folders)
    pub count: usize,
    /// folder that receives the items: they are selected there afterwards
    pub folder: Option<String>,
    /// names to select in `folder`
    pub select: Vec<String>,
    /// conflicts answer "keep both" without asking (a copy into the same folder)
    pub keep_both: bool,
    /// staged virtual files to remove when the job is over
    pub staging: Option<PathBuf>,
    /// folders whose listing the job changes
    pub touched: Vec<String>,
    /// an undo / redo: every shown folder may have changed
    pub history: bool,
    errors: usize,
    last_error: String,
    /// the job's cancel flag and pause gate (the panel's buttons)
    cancel: Cancellation,
}

impl Job {
    pub fn new(verb: &'static str, done: &'static str) -> Job {
        Job { verb, done, count: 0, folder: None, select: Vec::new(), keep_both: false, staging: None, touched: Vec::new(), history: false, errors: 0, last_error: String::new(), cancel: Cancellation::default() }
    }
    pub fn into_folder(mut self, folder: impl Into<String>, select: Vec<String>) -> Job {
        self.folder = Some(folder.into());
        self.select = select;
        self
    }
}

/// A job that ended, for the view (re-select, reload unwatched tabs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Finished {
    pub folder: Option<String>,
    pub select: Vec<String>,
    pub touched: Vec<String>,
    /// undo / redo: re-read every shown folder that has no watcher
    pub history: bool,
}

#[derive(Default)]
pub struct Ops {
    queue: Option<(FileOpQueue, Receiver<Event>)>,
    jobs: HashMap<u64, Job>,
    finished: Vec<Finished>,
    /// the status bar's file-operation / clipboard text
    pub message: Option<String>,
    /// a conflict or delete dialog is up (the timer must not open another)
    pub asking: bool,
    /// Alt-drops planned by `submit_steps`: (folder, sources). Shortcuts are made on
    /// a shell worker, so `shellops::after_frame` takes them.
    pub links: Vec<(String, Vec<String>)>,
    /// the Operations panel
    pub panel: Panel,
    /// the open conflict dialog
    pub conflict: Option<Dialog>,
    /// further questions (an extraction while a copy asks)
    waiting: VecDeque<Dialog>,
    /// the last progress repaint (at most one per `ops_panel::PAINT_EVERY`)
    last_paint: Option<Instant>,
}

pub fn plural(n: usize) -> &'static str {
    if n == 1 { "item" } else { "items" }
}

fn name_of(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// Names of the items `ops` (journal forwards) put into `folder`, in order:
/// the real targets, after "keep both" gave some a new name.
fn landed<'a>(ops: impl Iterator<Item = &'a Operation>, folder: &str) -> Vec<String> {
    fn target(op: &Operation) -> Option<&PathBuf> {
        match op {
            Operation::Copy { target, .. } | Operation::Move { target, .. } | Operation::Rename { target, .. } => Some(target),
            Operation::NewFolder(p) | Operation::NewFile(p) => Some(p),
            Operation::Sequence(v) => v.last().and_then(target),
            _ => None,
        }
    }
    ops.filter_map(target)
        .filter(|t| t.to_str().and_then(nav::parent).is_some_and(|p| p.eq_ignore_ascii_case(folder)))
        .map(|t| name_of(t))
        .collect()
}

/// The item an operation is about (its source), for "next" rows and errors.
fn subject(op: &Operation) -> Option<&Path> {
    match op {
        Operation::Copy { source, .. } | Operation::Move { source, .. } | Operation::Rename { source, .. } => Some(source),
        Operation::Delete { path, .. } | Operation::NewFolder(path) | Operation::NewFile(path) => Some(path),
        Operation::Restore(r) => Some(&r.original),
        Operation::Sequence(v) => v.iter().rev().find_map(subject),
    }
}

/// Folder the items come from (the first operation's parent).
fn source_folder(ops: &[Operation]) -> Option<String> {
    ops.first().and_then(subject).and_then(|p| p.to_str()).and_then(nav::parent)
}

/// Parent folders of everything `op` touches.
fn touched(op: &Operation, out: &mut Vec<String>) {
    let mut add = |p: &PathBuf| {
        if let Some(parent) = p.to_str().and_then(nav::parent)
            && !out.iter().any(|s| s.eq_ignore_ascii_case(&parent))
        {
            out.push(parent);
        }
    };
    match op {
        Operation::Copy { source, target } | Operation::Move { source, target } | Operation::Rename { source, target } => {
            add(source);
            add(target);
        }
        Operation::Delete { path, .. } | Operation::NewFolder(path) | Operation::NewFile(path) => add(path),
        Operation::Restore(item) => add(&item.original),
        Operation::Sequence(v) => v.iter().for_each(|o| touched(o, out)),
    }
}

impl Ops {
    /// Jobs are in flight: main keeps the event timer.
    pub fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Main's event timer is needed: jobs run, no dialog waits for the user and
    /// the running job is not paused (its worker sleeps on the gate, sends nothing).
    pub fn wants_timer(&self) -> bool {
        self.busy() && self.conflict.is_none() && !self.panel.cards.iter().any(|c| c.state == State::Paused)
    }

    fn queue(&mut self) -> Option<&FileOpQueue> {
        if self.queue.is_none() {
            match FileOpQueue::new(FsBackend) {
                Ok(q) => self.queue = Some(q),
                Err(e) => {
                    self.message = Some(format!("File operations unavailable: {e}"));
                    return None;
                }
            }
        }
        self.queue.as_ref().map(|(q, _)| q)
    }

    /// Queues `ops` as one undo step. False when nothing was queued.
    pub fn submit(&mut self, ops: Vec<Operation>, mut job: Job) -> bool {
        if ops.is_empty() {
            self.message = Some("Nothing to do".into());
            return false;
        }
        job.count = ops.len();
        for o in &ops {
            touched(o, &mut job.touched);
        }
        let from = source_folder(&ops);
        let names: Arc<[String]> = ops.iter().map(|o| subject(o).map(name_of).unwrap_or_default()).collect();
        let permanent = ops.iter().all(|o| matches!(o, Operation::Delete { permanent: true, .. }));
        let Some(q) = self.queue() else { return false };
        match q.submit(ops) {
            Ok(t) => {
                self.message = Some(format!("{} {} {}...", job.verb, job.count, plural(job.count)));
                let mut card = Card::new(Key::Queue(t.id), job.verb, job.done, job.count).with_folders(from.as_deref().unwrap_or(""), job.folder.as_deref().unwrap_or(""));
                card.names = names;
                card.permanent = permanent;
                card.stop = Stop::Queue(t.cancellation.clone());
                self.panel.add(card);
                job.cancel = t.cancellation;
                self.jobs.insert(t.id, job);
                true
            }
            Err(e) => {
                self.message = Some(format!("{} failed: {e}", job.verb));
                false
            }
        }
    }

    /// A drop, F5 / F6: the steps of `dnd::plan`. Shortcuts are made by a shell
    /// worker, not the queue: `Link` steps wait in `links` for `shellops`.
    pub fn submit_steps(&mut self, steps: &[Step], folder: &str) -> bool {
        let mut select = Vec::new();
        for s in steps {
            if let Step::Link { source, target } = s
                && let Some(dest) = nav::parent(target)
            {
                match self.links.iter_mut().find(|(f, _)| f.eq_ignore_ascii_case(&dest)) {
                    Some((_, v)) => v.push(source.clone()),
                    None => self.links.push((dest, vec![source.clone()])),
                }
            }
        }
        let ops: Vec<Operation> = steps
            .iter()
            .filter_map(|s| {
                let (op, target) = match s {
                    Step::Copy { source, target } => (Operation::Copy { source: source.into(), target: target.into() }, target),
                    Step::Move { source, target } => (Operation::Move { source: source.into(), target: target.into() }, target),
                    Step::Link { .. } => return None,
                };
                select.push(crate::dnd::name_of(target).to_string());
                Some(op)
            })
            .collect();
        if ops.is_empty() {
            if !self.links.is_empty() {
                return true;
            }
            self.message = Some("Nothing to do".into());
            return false;
        }
        let job = if matches!(steps.first(), Some(Step::Copy { .. })) { Job::new("Copying", "Copied") } else { Job::new("Moving", "Moved") };
        self.submit(ops, job.into_folder(folder, select))
    }

    /// Renames `from` to `to`. With `replace` an existing `to` goes to the
    /// Recycle Bin first (one step: undo brings it back).
    pub fn rename(&mut self, from: &str, to: &str, replace: bool) -> bool {
        let rn = Operation::Rename { source: from.into(), target: to.into() };
        let op = if replace { Operation::Sequence(vec![Operation::Delete { path: to.into(), permanent: false }, rn]) } else { rn };
        let folder = nav::parent(to).unwrap_or_default();
        let job = Job::new("Renaming", "Renamed").into_folder(folder, vec![crate::dnd::name_of(to).to_string()]);
        self.submit(vec![op], job)
    }

    /// A batch rename: every pair (the caller checked conflicts), one undo step.
    pub fn rename_batch(&mut self, pairs: &[(String, String)]) -> bool {
        let ops = pairs.iter().map(|(a, b)| Operation::Rename { source: a.into(), target: b.into() }).collect();
        let folder = pairs.first().and_then(|(_, b)| nav::parent(b)).unwrap_or_default();
        let names = pairs.iter().map(|(_, b)| crate::dnd::name_of(b).to_string()).collect();
        self.submit(ops, Job::new("Renaming", "Renamed").into_folder(folder, names))
    }

    /// Nothing recorded to undo (or redo) and nothing running that could add some.
    fn nothing(&self, redo: bool) -> bool {
        if !self.jobs.is_empty() {
            return false;
        }
        let Some((q, _)) = &self.queue else { return true };
        let j = q.journal();
        let j = j.lock().unwrap_or_else(|e| e.into_inner());
        if redo { j.redo_group().is_empty() } else { j.undo_group().is_empty() }
    }

    /// Ctrl+Z: the newest submission of any kind, as a whole.
    pub fn undo(&mut self) -> bool {
        self.history(false)
    }

    /// Ctrl+Y: the submission undone last.
    pub fn redo(&mut self) -> bool {
        self.history(true)
    }

    fn history(&mut self, redo: bool) -> bool {
        if self.nothing(redo) {
            self.message = Some(if redo { "Nothing to redo" } else { "Nothing to undo" }.into());
            return false;
        }
        let Some(q) = self.queue() else { return false };
        let r = if redo { q.redo_group() } else { q.undo_group() };
        let (verb, done) = if redo { ("Redoing", "Redone") } else { ("Undoing", "Undone") };
        match r {
            Ok(t) => {
                self.track_history(t, verb, done);
                self.message = Some(format!("{verb}..."));
                true
            }
            Err(e) => {
                self.message = Some(format!("{verb} failed: {e}"));
                false
            }
        }
    }

    /// The next queue event, if any (UI thread, never blocks).
    pub fn next_event(&mut self) -> Option<Event> {
        self.queue.as_ref().and_then(|(_, rx)| rx.try_recv().ok())
    }

    /// A conflict of job `id` is answered "keep both" without a dialog.
    pub fn keep_both(&self, id: u64) -> bool {
        self.jobs.get(&id).is_some_and(|j| j.keep_both)
    }

    /// Every event except `Conflict` (the caller answers those).
    pub fn on_event(&mut self, ev: Event) {
        let now = Instant::now();
        match ev {
            Event::Conflict { .. } => {}
            Event::Started(id) => self.panel.started(Key::Queue(id), now),
            Event::Progress { id, totals, job, index, current, bytes_done, item_bytes } => {
                self.panel.progress(Key::Queue(id), now, job, index, &current, bytes_done, item_bytes);
                let verb = self.jobs.get(&id).map(|j| j.verb).unwrap_or("Working");
                let pct = totals.bytes_done.saturating_mul(100).checked_div(totals.bytes_total).unwrap_or(0);
                self.message = Some(format!("{verb}: {} of {} files, {pct}%", totals.files_done, totals.files_total.max(totals.files_done)));
            }
            Event::Error { id, message, item } => {
                let what = match &item {
                    Operation::Copy { source, .. } | Operation::Move { source, .. } | Operation::Rename { source, .. } => name_of(source),
                    Operation::Delete { path, .. } | Operation::NewFolder(path) | Operation::NewFile(path) => name_of(path),
                    Operation::Restore(r) => name_of(&r.original),
                    Operation::Sequence(_) => String::new(),
                };
                if let Some(j) = self.jobs.get_mut(&id) {
                    j.errors += 1;
                    j.last_error = format!("{what}: {message}");
                }
                let path = subject(&item).map(Path::to_path_buf).unwrap_or_default();
                self.panel.error(Key::Queue(id), path, message.clone(), Some(item));
                self.message = Some(format!("Couldn't finish: {what}: {message}"));
            }
            Event::Finished { id, cancelled, job: totals, .. } => {
                self.panel.finished(Key::Queue(id), now, cancelled, totals);
                let Some(job) = self.jobs.remove(&id) else { return };
                if let Some(dir) = job.staging.clone() {
                    // staged virtual files: whatever was not moved out goes
                    let _ = std::thread::Builder::new().name("tsubame-unstage".into()).spawn(move || {
                        let _ = std::fs::remove_dir_all(dir);
                    });
                }
                self.message = Some(result_text(&job, cancelled));
                let mut job = job;
                if !job.select.is_empty()
                    && let Some(folder) = &job.folder
                    && let Some((q, _)) = &self.queue
                {
                    // "keep both" may have renamed targets: select what really landed
                    let j = q.journal();
                    let j = j.lock().unwrap_or_else(|e| e.into_inner());
                    let names = landed(j.undo_entries().iter().filter(|e| e.group == id).map(|e| &e.forward), folder);
                    if !names.is_empty() {
                        job.select = names;
                    }
                }
                self.finished.push(Finished { folder: job.folder, select: job.select, touched: job.touched, history: job.history });
            }
        }
    }

    /// An undo / redo ticket: its job and its card (no Undo link on it).
    fn track_history(&mut self, t: tsubame_win::fileops::Ticket, verb: &'static str, done: &'static str) {
        let mut job = Job::new(verb, done);
        job.history = true;
        let mut card = Card::new(Key::Queue(t.id), verb, done, 0);
        card.history = true;
        card.stop = Stop::Queue(t.cancellation.clone());
        self.panel.add(card);
        job.cancel = t.cancellation;
        self.jobs.insert(t.id, job);
    }

    // ---- the conflict dialog --------------------------------------------------

    /// A queue job asks about `target`: the dialog opens (or waits behind one).
    pub fn ask(&mut self, id: u64, source: PathBuf, target: PathBuf, existing: FileMeta, incoming: FileMeta, reply: ConflictReply) {
        let key = Key::Queue(id);
        let (verb, count, from, to) = match self.panel.card(key) {
            Some(c) => (c.verb, c.count, c.from.clone(), c.to.clone()),
            None => ("Copying", 1, String::new(), String::new()),
        };
        let to = if to.is_empty() { parent_name(&target) } else { to };
        self.open_dialog(Dialog::new(key, Reply::Queue(reply), source, target, existing, incoming, verb, count, from, to));
    }

    /// The extract thread asks about `target` (the incoming file is in the archive).
    pub fn ask_extract(&mut self, target: PathBuf, existing: FileMeta, reply: std::sync::mpsc::Sender<tsubame_core::archive::ConflictAnswer>) {
        let card = self.panel.cards.iter().find(|c| matches!(c.key, Key::Extract(_)) && c.state.active());
        let key = card.map(|c| c.key).unwrap_or(Key::Extract(0));
        let cancel = card.and_then(|c| if let Stop::Extract(f) = &c.stop { Some(f.clone()) } else { None });
        let from = card.map(|c| c.from.clone()).unwrap_or_default();
        let to = parent_name(&target);
        let source = PathBuf::from(target.file_name().unwrap_or_default());
        self.open_dialog(Dialog::new(key, Reply::Extract(reply, cancel), source, target, existing, FileMeta::default(), "Extracting", 0, from, to));
    }

    fn open_dialog(&mut self, d: Dialog) {
        self.panel.set_asking(d.key, true);
        if self.conflict.is_none() {
            self.message = Some(format!("\"{}\" already exists", conflict_dialog::target_name(&d.target)));
            self.conflict = Some(d);
        } else {
            self.waiting.push_back(d);
        }
    }

    /// The user answered the open dialog; the next question (if any) opens.
    pub fn answer(&mut self, a: Answer) {
        let Some(mut d) = self.conflict.take() else { return };
        d.answer(a);
        let now = Instant::now();
        self.panel.set_asking(d.key, false);
        if a == Answer::Cancel {
            self.panel.cancelled(d.key, now);
            // the job's other questions go with it
            let (gone, keep): (VecDeque<Dialog>, VecDeque<Dialog>) = std::mem::take(&mut self.waiting).into_iter().partition(|w| w.key == d.key);
            for mut w in gone {
                w.answer(Answer::Cancel);
            }
            self.waiting = keep;
        }
        self.message = None;
        if let Some(n) = self.waiting.pop_front() {
            self.message = Some(format!("\"{}\" already exists", conflict_dialog::target_name(&n.target)));
            self.conflict = Some(n);
        }
    }

    // ---- the panel's buttons --------------------------------------------------

    /// Does what a panel button (or its UIA Invoke) asks.
    pub fn act(&mut self, a: Act) {
        let now = Instant::now();
        match a {
            Act::Pause(k) | Act::Resume(k) => {
                let pause = matches!(a, Act::Pause(_));
                if let Some(c) = self.panel.card(k)
                    && let Stop::Queue(g) = &c.stop
                {
                    if pause { g.pause() } else { g.resume() }
                    self.panel.set_paused(k, pause);
                }
            }
            Act::Cancel(k) => {
                match self.panel.card(k).map(|c| c.stop.clone()) {
                    Some(Stop::Queue(g)) => g.cancel(),
                    Some(Stop::Extract(f)) => f.store(true, std::sync::atomic::Ordering::Release),
                    _ => {}
                }
                if self.conflict.as_ref().is_some_and(|d| d.key == k) {
                    self.answer(Answer::Cancel);
                } else {
                    self.panel.cancelled(k, now);
                }
            }
            Act::Undo(Key::Queue(id)) => {
                if self.undo_job(id) {
                    self.panel.remove(Key::Queue(id));
                }
            }
            Act::Undo(_) => {}
            Act::Retry(k) => {
                let Some(c) = self.panel.card(k) else { return };
                let (ops, verb, done) = (c.failed.clone(), c.verb, c.done_verb);
                let folder = c.to_path.clone();
                self.panel.remove(k);
                let job = if folder.is_empty() { Job::new(verb, done) } else { Job::new(verb, done).into_folder(folder, Vec::new()) };
                self.submit(ops, job);
            }
            Act::Dismiss(k) => self.panel.remove(k),
            Act::Expand(k) => {
                if let Some(c) = self.panel.card_mut(k) {
                    c.expanded = !c.expanded;
                    self.panel.layout_dirty = true;
                }
            }
            Act::Details(k) => {
                if let Some(c) = self.panel.card_mut(k) {
                    c.details = !c.details;
                    self.panel.layout_dirty = true;
                }
            }
            Act::ClearFinished => self.panel.clear_finished(),
            Act::Collapse => self.panel.collapse(),
            Act::Open => self.panel.expand_panel(),
        }
    }

    /// The Undo link of a finished card: reverts that submission, but only while
    /// it is still the newest one and nothing runs (Ctrl+Z order stays intact).
    pub fn undo_job(&mut self, id: u64) -> bool {
        let newest = self.queue.as_ref().and_then(|(q, _)| {
            let j = q.journal();
            let j = j.lock().unwrap_or_else(|e| e.into_inner());
            j.newest_undo_group()
        });
        if newest != Some(id) || !self.jobs.is_empty() {
            self.message = Some("Only the newest operation can be undone (Ctrl+Z)".into());
            return false;
        }
        let Some(q) = self.queue() else { return false };
        match q.undo_group_of(id) {
            Ok(t) => {
                self.track_history(t, "Undoing", "Undone");
                self.message = Some("Undoing...".into());
                true
            }
            Err(e) => {
                self.message = Some(format!("Undoing failed: {e}"));
                false
            }
        }
    }

    /// Time passed (the event timer, the dismiss timer): the panel opens after
    /// its delay and drops expired cards. True when it should repaint.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.panel.tick(now)
    }

    /// A repaint is due: layout changes at once, progress at most every
    /// `PAINT_EVERY` (10 Hz). Records the paint it allows.
    pub fn want_paint(&mut self, now: Instant) -> bool {
        let due = self.last_paint.is_none_or(|t| now.saturating_duration_since(t) >= crate::ops_panel::PAINT_EVERY);
        let want = self.panel.layout_dirty || (due && self.panel.dirty());
        if want {
            self.last_paint = Some(now);
        }
        want
    }

    // ---- the extract adapter -----------------------------------------------------

    /// News from `archive_ui`'s extraction (it runs outside the queue).
    pub fn on_extract(&mut self, note: crate::archive_ui::ExtractNote) {
        use crate::archive_ui::ExtractNote as N;
        let now = Instant::now();
        match note {
            N::Started { id, archive, dest, cancel } => {
                let mut c = Card::new(Key::Extract(id), "Extracting", "Extracted", 0).with_folders(&archive, &dest);
                c.stop = Stop::Extract(cancel);
                c.permanent = true;
                self.panel.add(c);
                self.panel.started(Key::Extract(id), now);
            }
            N::Progress { id, done, total } => {
                let t = Totals { files_done: 0, files_total: 0, bytes_done: done, bytes_total: total };
                self.panel.progress(Key::Extract(id), now, t, 0, Path::new(""), 0, 0);
            }
            N::Done { id, cancelled, error, files } => {
                let key = Key::Extract(id);
                if let Some(c) = self.panel.card_mut(key) {
                    c.count = files;
                    c.totals.files_done = files as u64;
                    c.totals.files_total = c.totals.files_total.max(files as u64);
                }
                if let Some(e) = error
                    && !cancelled
                {
                    let p = self.panel.card(key).map(|c| PathBuf::from(&c.from_path)).unwrap_or_default();
                    self.panel.error(key, p, e, None);
                }
                if cancelled {
                    self.panel.cancelled(key, now);
                }
                // a question still open for it is moot now
                if self.conflict.as_ref().is_some_and(|d| d.key == key) {
                    self.answer(Answer::Skip);
                }
                self.panel.finished(key, now, cancelled, Totals::default());
            }
        }
        self.panel.tick(now);
    }

    /// Something outside the queue changed `folder` (shortcuts made by a shell
    /// worker): the view treats it like a finished job.
    pub fn changed(&mut self, folder: &str, select: Vec<String>) {
        self.finished.push(Finished { folder: Some(folder.to_string()), select, touched: vec![folder.to_string()], history: false });
    }

    /// Jobs that ended since the last call.
    pub fn take_finished(&mut self) -> Vec<Finished> {
        std::mem::take(&mut self.finished)
    }
}

/// The name of the folder `p` is in.
fn parent_name(p: &Path) -> String {
    p.parent().map(|d| crate::ops_panel::folder_name(&d.to_string_lossy())).unwrap_or_default()
}

/// The status bar's line for a finished job.
fn result_text(job: &Job, cancelled: bool) -> String {
    if cancelled {
        format!("{} cancelled", job.verb)
    } else if job.errors > 0 {
        let ok = job.count.saturating_sub(job.errors);
        format!("{} {ok} of {}, {} failed: {}", job.done, job.count, job.errors, job.last_error)
    } else if job.history {
        job.done.to_string()
    } else {
        format!("{} {} {}", job.done, job.count, plural(job.count))
    }
}

/// The action for a drop with the keys held, for the first dragged path.
pub fn action_for(paths: &[String], dest: &str, mods: tsubame_ui::Mods) -> crate::dnd::DropAction {
    paths.first().map(|p| crate::dnd::resolve(p, dest, mods)).unwrap_or(crate::dnd::DropAction::Copy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tsubame_win::fileops::Choice;

    #[test]
    fn link_only_or_empty_submissions_start_nothing() {
        let mut o = Ops::default();
        // an Alt-drop: no queue operation; the shortcuts wait for the shell worker, grouped by folder
        let link = |s: &str, t: &str| Step::Link { source: s.into(), target: t.into() };
        assert!(o.submit_steps(&[link("C:\\a", "C:\\b\\a.lnk"), link("C:\\c.txt", "C:\\b\\c.txt.lnk"), link("C:\\d", "C:\\e\\d.lnk")], "C:\\b"));
        assert_eq!(o.links, vec![("C:\\b".to_string(), vec!["C:\\a".to_string(), "C:\\c.txt".to_string()]), ("C:\\e".to_string(), vec!["C:\\d".to_string()])]);
        assert!(o.queue.is_none(), "no queue thread for nothing");
        assert!(!o.busy());
        assert!(!o.submit(vec![], Job::new("Copying", "Copied")));
        assert!(o.queue.is_none());
    }

    #[test]
    fn touched_folders_are_parents_of_sources_and_targets() {
        let mut v = Vec::new();
        touched(&Operation::Move { source: "C:\\a\\x.txt".into(), target: "D:\\b\\x.txt".into() }, &mut v);
        touched(&Operation::Rename { source: "C:\\a\\y".into(), target: "C:\\A\\z".into() }, &mut v);
        touched(&Operation::Sequence(vec![Operation::Delete { path: "E:\\q\\r".into(), permanent: false }]), &mut v);
        assert_eq!(v, vec!["C:\\a".to_string(), "D:\\b".to_string(), "E:\\q".to_string()]);
    }

    #[test]
    fn keep_both_names_are_what_gets_selected() {
        let ops = [
            Operation::Copy { source: "C:\\a\\x.txt".into(), target: "C:\\b\\x (2).txt".into() },
            Operation::Sequence(vec![
                Operation::Delete { path: "C:\\b\\y.txt".into(), permanent: false },
                Operation::Move { source: "C:\\a\\y.txt".into(), target: "C:\\b\\y.txt".into() },
            ]),
            Operation::Copy { source: "C:\\a\\z.txt".into(), target: "C:\\other\\z.txt".into() },
        ];
        assert_eq!(landed(ops.iter(), "C:\\B"), ["x (2).txt", "y.txt"]);
    }

    #[test]
    fn undo_with_nothing_says_so_and_starts_no_queue() {
        let mut o = Ops::default();
        assert!(!o.undo());
        assert_eq!(o.message.as_deref(), Some("Nothing to undo"));
        assert!(!o.redo());
        assert_eq!(o.message.as_deref(), Some("Nothing to redo"));
        assert!(o.queue.is_none());
    }

    #[test]
    fn results_count_items_and_errors() {
        let mut j = Job::new("Copying", "Copied");
        j.count = 3;
        assert_eq!(result_text(&j, false), "Copied 3 items");
        assert_eq!(result_text(&j, true), "Copying cancelled");
        j.errors = 1;
        j.last_error = "x.txt: denied".into();
        assert_eq!(result_text(&j, false), "Copied 2 of 3, 1 failed: x.txt: denied");
        let mut h = Job::new("Undoing", "Undone");
        h.history = true;
        assert_eq!(result_text(&h, false), "Undone");
    }

    /// Drives `o` like main's timer until it is idle.
    fn drain(o: &mut Ops) -> Vec<Finished> {
        let t0 = Instant::now();
        let mut out = Vec::new();
        while o.busy() {
            assert!(t0.elapsed() < Duration::from_secs(30), "queue stuck");
            match o.next_event() {
                Some(Event::Conflict { reply, .. }) => reply.answer(Choice::KeepBoth, true),
                Some(ev) => o.on_event(ev),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
            out.extend(o.take_finished());
        }
        out
    }

    /// The unified journal end to end on the real file system: a paste (the
    /// W1 path), a rename and a batch rename (U3), then Ctrl+Z three times
    /// undoes them newest first, one submission per press, and Ctrl+Y redoes.
    #[test]
    fn one_journal_for_every_kind_of_operation() {
        let d = std::env::temp_dir().join(format!("tsubame-int1-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("dst")).unwrap();
        for n in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(d.join("src").join(n), n).unwrap();
        }
        let s = |p: &str| d.join(p).to_string_lossy().into_owned();
        let mut o = Ops::default();
        // 1. paste of two files (a shellops-style submission)
        let ops = crate::ole_glue::ops(&[d.join("src\\a.txt"), d.join("src\\b.txt")], &d.join("dst"), tsubame_win::dnd::formats::EFFECT_COPY);
        assert!(o.submit(ops, Job::new("Copying", "Copied").into_folder(s("dst"), vec!["a.txt".into(), "b.txt".into()])));
        let f = drain(&mut o);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].select, vec!["a.txt".to_string(), "b.txt".to_string()]);
        assert!(f[0].touched.iter().any(|t| t.eq_ignore_ascii_case(&s("dst"))));
        assert_eq!(o.message.as_deref(), Some("Copied 2 items"));
        // 2. inline rename
        assert!(o.rename(&s("src\\c.txt"), &s("src\\c2.txt"), false));
        drain(&mut o);
        // 3. batch rename in dst
        assert!(o.rename_batch(&[(s("dst\\a.txt"), s("dst\\a1.txt")), (s("dst\\b.txt"), s("dst\\b1.txt"))]));
        drain(&mut o);
        assert!(d.join("dst\\a1.txt").exists() && d.join("dst\\b1.txt").exists() && d.join("src\\c2.txt").exists());

        // Ctrl+Z: the batch (both names), then the rename, then the paste
        assert!(o.undo());
        let f = drain(&mut o);
        assert!(f[0].history);
        assert!(d.join("dst\\a.txt").exists() && d.join("dst\\b.txt").exists() && !d.join("dst\\a1.txt").exists());
        assert!(d.join("src\\c2.txt").exists(), "one press, one submission");
        assert!(o.undo());
        drain(&mut o);
        assert!(d.join("src\\c.txt").exists() && !d.join("src\\c2.txt").exists());
        assert!(d.join("dst\\a.txt").exists());
        assert!(o.undo());
        drain(&mut o);
        // undo of a copy recycles the copies (bin on the temp volume) or fails visibly
        let pasted_gone = !d.join("dst\\a.txt").exists() && !d.join("dst\\b.txt").exists();
        if pasted_gone {
            assert!(!o.undo(), "nothing left");
            assert_eq!(o.message.as_deref(), Some("Nothing to undo"));
            // Ctrl+Y redoes the paste as a whole
            assert!(o.redo());
            drain(&mut o);
            assert!(d.join("dst\\a.txt").exists() && d.join("dst\\b.txt").exists());
            // the recycled copies are purged by redo's re-copy only if... leave the bin entries: names are unique to this test
        }
        let _ = std::fs::remove_dir_all(&d);
    }


    #[test]
    fn the_event_timer_stops_while_paused_or_asking_and_paint_is_throttled() {
        let mut o = Ops::default();
        let mut c = Card::new(Key::Queue(1), "Copying", "Copied", 1);
        c.stop = Stop::Queue(Cancellation::default());
        o.panel.add(c);
        o.jobs.insert(1, Job::new("Copying", "Copied"));
        assert!(o.wants_timer());
        let t = Instant::now();
        o.panel.started(Key::Queue(1), t);
        o.act(Act::Pause(Key::Queue(1)));
        assert!(!o.wants_timer(), "a paused worker sends nothing");
        o.act(Act::Resume(Key::Queue(1)));
        assert!(o.wants_timer());
        // layout changes paint at once; progress at most every PAINT_EVERY
        assert!(o.want_paint(t));
        o.panel.layout_dirty = false;
        o.panel.cards[0].dirty = true;
        assert!(!o.want_paint(t + std::time::Duration::from_millis(50)));
        assert!(o.want_paint(t + crate::ops_panel::PAINT_EVERY));
        o.panel.cards[0].dirty = false;
        assert!(!o.want_paint(t + std::time::Duration::from_secs(5)), "nothing changed: no paint");
    }

    /// The dialog end to end on the real file system: a copy onto an existing
    /// file asks, the timer stops while it waits, Cancel operation stops the job
    /// (the second file is not copied).
    #[test]
    fn cancel_in_the_conflict_dialog_stops_the_whole_job() {
        let d = std::env::temp_dir().join(format!("tsubame-b11e2-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("dst")).unwrap();
        for n in ["a.txt", "b.txt"] {
            std::fs::write(d.join("src").join(n), n).unwrap();
        }
        std::fs::write(d.join("dst\\a.txt"), "old").unwrap();
        let mut o = Ops::default();
        let ops = vec![
            Operation::Copy { source: d.join("src\\a.txt"), target: d.join("dst\\a.txt") },
            Operation::Copy { source: d.join("src\\b.txt"), target: d.join("dst\\b.txt") },
        ];
        assert!(o.submit(ops, Job::new("Copying", "Copied").into_folder(d.join("dst").to_string_lossy(), Vec::new())));
        let t0 = Instant::now();
        let mut asked = false;
        while o.busy() {
            assert!(t0.elapsed() < Duration::from_secs(30), "queue stuck");
            match o.next_event() {
                Some(Event::Conflict { id, source, target, existing, incoming, reply }) => {
                    assert!(existing.known && incoming.known);
                    o.ask(id, source, target, existing, incoming, reply);
                    assert!(o.conflict.is_some() && !o.wants_timer());
                    assert_eq!(o.panel.cards[0].state, State::Asking);
                    asked = true;
                    o.answer(Answer::Cancel);
                    assert!(o.conflict.is_none());
                }
                Some(ev) => o.on_event(ev),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert!(asked);
        assert_eq!(std::fs::read_to_string(d.join("dst\\a.txt")).unwrap(), "old");
        assert!(!d.join("dst\\b.txt").exists(), "Cancel operation stops the rest");
        assert_eq!(o.message.as_deref(), Some("Copying cancelled"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
