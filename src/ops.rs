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
//!   for tabs without a watcher and for the selection of new items.

use crate::dnd::Step;
use crate::nav;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use tsubame_win::fileops::{Event, FileOpQueue, FsBackend, Operation};

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
}

impl Job {
    pub fn new(verb: &'static str, done: &'static str) -> Job {
        Job { verb, done, count: 0, folder: None, select: Vec::new(), keep_both: false, staging: None, touched: Vec::new(), history: false, errors: 0, last_error: String::new() }
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
        let Some(q) = self.queue() else { return false };
        match q.submit(ops) {
            Ok(t) => {
                self.message = Some(format!("{} {} {}...", job.verb, job.count, plural(job.count)));
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
    /// worker, not the queue, so `Link` steps are skipped here.
    pub fn submit_steps(&mut self, steps: &[Step], folder: &str) -> bool {
        let mut select = Vec::new();
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
                let mut job = Job::new(verb, done);
                job.history = true;
                self.jobs.insert(t.id, job);
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
        match ev {
            Event::Started(_) | Event::Conflict { .. } => {}
            Event::Progress { id, totals, .. } => {
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
                self.message = Some(format!("Couldn't finish: {what}: {message}"));
            }
            Event::Finished { id, cancelled, .. } => {
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
        assert!(!o.submit_steps(&[Step::Link { source: "C:\\a".into(), target: "C:\\b.lnk".into() }], "C:\\"));
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
}
