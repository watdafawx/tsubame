//! File operations the window starts: copy / move from drops and F5 / F6,
//! renames (single, replace and batch), undo and redo. A thin layer over the
//! `tsubame_win::fileops` queue:
//!
//! - the queue (and its worker thread) starts on the first operation, so a
//!   window that never copies pays nothing;
//! - events are polled from the UI thread (`poll`), by a timer main.rs runs
//!   only while `busy()`: idle stays at zero wake-ups;
//! - conflicts are answered here ("keep both"; nothing is overwritten unasked);
//! - `Replace` is explicit: the old target goes to the Recycle Bin and the
//!   source takes its name, as one undoable step;
//! - a batch is one group for undo: Ctrl+Z reverts every rename of it.

use crate::dnd::{DropAction, Step};
use crate::nav;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use tsubame_win::fileops::{Choice, Event, FileOpQueue, FsBackend, Operation};

#[derive(Default)]
pub struct Ops {
    queue: Option<(FileOpQueue, Receiver<Event>)>,
    /// submissions that have not finished
    pending: u32,
    /// operations per submission, newest last (undo reverts a whole group)
    undo_groups: Vec<usize>,
    redo_groups: Vec<usize>,
    /// folders whose listing is stale (an operation touched them)
    stale: Vec<String>,
    /// the last result, for the status bar ("Moved 3 items", "Couldn't ...")
    pub message: Option<String>,
}

/// What `poll` found.
#[derive(Default)]
pub struct Polled {
    /// something finished: the stale folders to re-read
    pub refresh: Vec<String>,
    /// the message changed
    pub message: bool,
}

impl Ops {
    pub fn busy(&self) -> bool {
        self.pending > 0
    }

    fn queue(&mut self) -> Option<&FileOpQueue> {
        if self.queue.is_none() {
            match FileOpQueue::new(FsBackend) {
                Ok(q) => self.queue = Some(q),
                Err(e) => {
                    self.message = Some(format!("Can't start file operations: {e}"));
                    return None;
                }
            }
        }
        self.queue.as_ref().map(|(q, _)| q)
    }

    /// A folder whose listing changed by other means (undo, redo).
    pub fn mark_dir(&mut self, dir: &str) {
        if !self.stale.iter().any(|s| s.eq_ignore_ascii_case(dir)) {
            self.stale.push(dir.to_string());
        }
    }

    fn mark_stale(&mut self, op: &Operation) {
        let mut add = |p: &PathBuf| {
            if let Some(parent) = p.to_str().and_then(nav::parent)
                && !self.stale.iter().any(|s| s.eq_ignore_ascii_case(&parent))
            {
                self.stale.push(parent);
            }
        };
        match op {
            Operation::Copy { source, target } | Operation::Move { source, target } | Operation::Rename { source, target } => {
                add(source);
                add(target);
            }
            Operation::Delete { path, .. } => add(path),
            Operation::NewFolder(p) | Operation::NewFile(p) => add(p),
            Operation::Restore(_) => {}
            Operation::Sequence(v) => v.iter().for_each(|o| self.mark_stale(o)),
        }
    }

    /// Queues `ops` as one group. False when the queue can't start.
    pub fn submit(&mut self, ops: Vec<Operation>) -> bool {
        if ops.is_empty() {
            return true;
        }
        for o in &ops {
            self.mark_stale(o);
        }
        let n = ops.len();
        let Some(q) = self.queue() else { return false };
        match q.submit(ops) {
            Ok(_) => {
                self.pending += 1;
                self.undo_groups.push(n);
                self.redo_groups.clear();
                true
            }
            Err(e) => {
                self.message = Some(format!("Can't start the operation: {e}"));
                false
            }
        }
    }

    /// A drop or F5 / F6: the steps of `dnd::plan` as queue operations.
    /// Shortcuts (`Link`) need the shell and are skipped.
    pub fn submit_steps(&mut self, steps: &[Step]) -> bool {
        let ops: Vec<Operation> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Copy { source, target } => Some(Operation::Copy { source: source.into(), target: target.into() }),
                Step::Move { source, target } => Some(Operation::Move { source: source.into(), target: target.into() }),
                Step::Link { .. } => None,
            })
            .collect();
        if ops.is_empty() {
            self.message = Some("Shortcuts aren't supported yet".into());
            return false;
        }
        let verb = if matches!(steps.first(), Some(Step::Copy { .. })) { "Copying" } else { "Moving" };
        self.message = Some(format!("{verb} {} {}...", ops.len(), if ops.len() == 1 { "item" } else { "items" }));
        self.submit(ops)
    }

    /// Renames `from` to `to`. With `replace` an existing `to` goes to the
    /// Recycle Bin first (undo brings it back).
    pub fn rename(&mut self, from: &str, to: &str, replace: bool) -> bool {
        let rn = Operation::Rename { source: from.into(), target: to.into() };
        let op = if replace { Operation::Sequence(vec![Operation::Delete { path: to.into(), permanent: false }, rn]) } else { rn };
        self.submit(vec![op])
    }

    /// A batch rename: every pair or none (the caller checked conflicts).
    pub fn rename_batch(&mut self, pairs: &[(String, String)]) -> bool {
        let ops = pairs.iter().map(|(a, b)| Operation::Rename { source: a.into(), target: b.into() }).collect();
        self.submit(ops)
    }

    pub fn undo(&mut self) -> bool {
        let Some(n) = self.undo_groups.pop() else {
            self.message = Some("Nothing to undo".into());
            return false;
        };
        let mut ok = false;
        for _ in 0..n {
            ok |= self.queue().is_some_and(|q| q.undo().is_ok());
        }
        if ok {
            self.pending += n as u32;
            self.redo_groups.push(n);
            self.message = Some("Undone".into());
        }
        ok
    }

    pub fn redo(&mut self) -> bool {
        let Some(n) = self.redo_groups.pop() else {
            self.message = Some("Nothing to redo".into());
            return false;
        };
        let mut ok = false;
        for _ in 0..n {
            ok |= self.queue().is_some_and(|q| q.redo().is_ok());
        }
        if ok {
            self.pending += n as u32;
            self.undo_groups.push(n);
            self.message = Some("Redone".into());
        }
        ok
    }

    /// Drains the queue's events (UI thread, never blocks).
    pub fn poll(&mut self) -> Polled {
        let mut out = Polled::default();
        let Some((_, rx)) = &self.queue else { return out };
        let mut finished = 0u32;
        let mut msg: Option<String> = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                Event::Conflict { reply, .. } => reply.answer(Choice::KeepBoth, true),
                Event::Error { message, .. } => msg = Some(format!("Couldn't finish: {message}")),
                Event::Finished { cancelled, .. } => {
                    finished += 1;
                    if msg.is_none() {
                        msg = Some(if cancelled { "Cancelled".into() } else { "Done".into() });
                    }
                }
                Event::Started(_) | Event::Progress { .. } => {}
            }
        }
        if finished > 0 {
            self.pending = self.pending.saturating_sub(finished);
        }
        if let Some(m) = msg {
            self.message = Some(m);
            out.message = true;
        }
        if self.pending == 0 && !self.stale.is_empty() {
            out.refresh = std::mem::take(&mut self.stale);
        }
        out
    }
}

/// The action for a drop with the keys held, for the first dragged path.
pub fn action_for(paths: &[String], dest: &str, mods: tsubame_ui::Mods) -> DropAction {
    paths.first().map(|p| crate::dnd::resolve(p, dest, mods)).unwrap_or(DropAction::Copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_become_operations_and_links_are_skipped() {
        let mut o = Ops::default();
        // no queue is started for an empty or link-only submission
        assert!(!o.submit_steps(&[Step::Link { source: "C:\\a".into(), target: "C:\\b.lnk".into() }]));
        assert!(o.queue.is_none());
        assert!(!o.busy());
        assert!(o.message.as_deref().unwrap().contains("Shortcuts"));
    }

    #[test]
    fn stale_folders_are_parents_of_sources_and_targets() {
        let mut o = Ops::default();
        o.mark_stale(&Operation::Move { source: "C:\\a\\x.txt".into(), target: "D:\\b\\x.txt".into() });
        o.mark_stale(&Operation::Rename { source: "C:\\a\\y".into(), target: "C:\\a\\z".into() });
        assert_eq!(o.stale, vec!["C:\\a".to_string(), "D:\\b".to_string()]);
    }

    #[test]
    fn undo_with_nothing_says_so() {
        let mut o = Ops::default();
        assert!(!o.undo());
        assert_eq!(o.message.as_deref(), Some("Nothing to undo"));
        assert!(!o.redo());
    }
}
