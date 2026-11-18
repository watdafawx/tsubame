//! Queue-wide progress totals and the off-thread pre-scan that feeds them.
//!
//! Each submitted batch is handed to a scanner thread, which sizes every operation
//! (recursing into folders for copies and cross-volume moves) and adds the result to the
//! queue totals. The UI thread never touches the disk. The worker may start before the scan
//! reaches an operation; it then adds what it actually did, and the scan result for that
//! operation is ignored (`Settled`), so nothing is counted twice.
//!
//! "Files" counts regular files for copies and one unit for every other operation. Bytes
//! count only file data that is read and written. Counters reset when the queue drains.
use super::{Cancellation, Operation};
use std::{
    collections::HashMap,
    fs, io,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

/// Queue-wide numbers carried by every progress event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Clone, Copy, Debug)]
enum OpScan {
    Pending,
    Scanned { files: u64, bytes: u64 },
    Settled,
}

#[derive(Default)]
struct State {
    totals: Totals,
    jobs: HashMap<u64, Vec<OpScan>>,
}

/// Shared between queue, worker and scanner.
#[derive(Clone, Default)]
pub struct Shared(Arc<Mutex<State>>);

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn snapshot(&self) -> Totals {
        self.lock().totals
    }
    /// Register a job of `ops` operations (all `Pending`).
    pub fn begin_job(&self, id: u64, ops: usize) {
        self.lock().jobs.insert(id, vec![OpScan::Pending; ops]);
    }
    /// Job still running (a scan result for a finished job is dropped).
    pub fn is_open(&self, id: u64) -> bool {
        self.lock().jobs.contains_key(&id)
    }
    /// The scanner sized operation `index`.
    pub fn scanned(&self, id: u64, index: usize, files: u64, bytes: u64) {
        let mut s = self.lock();
        let State { totals, jobs } = &mut *s;
        if let Some(slot) = jobs.get_mut(&id).and_then(|j| j.get_mut(index))
            && matches!(slot, OpScan::Pending)
        {
            *slot = OpScan::Scanned { files, bytes };
            totals.files_total += files;
            totals.bytes_total += bytes;
        }
    }
    /// The backend moved data. Called often; cheap.
    pub fn step(&self, files: u64, bytes: u64) {
        let mut s = self.lock();
        s.totals.files_done += files;
        s.totals.bytes_done += bytes;
    }
    /// Operation `index` ended. `files`/`bytes` are what the backend reported through
    /// `step` (already counted as done). Call for skipped and failed operations too.
    pub fn op_finished(&self, id: u64, index: usize, files: u64, bytes: u64) {
        let mut s = self.lock();
        let State { totals, jobs } = &mut *s;
        let Some(slot) = jobs.get_mut(&id).and_then(|j| j.get_mut(index)) else {
            return;
        };
        match *slot {
            OpScan::Scanned { files: f, bytes: b } => {
                // Done may be short of the estimate (renames, skips): close the gap.
                // Or over it (folder changed while copying): grow the total.
                if files < f {
                    totals.files_done += f - files;
                } else {
                    totals.files_total += files - f;
                }
                if bytes < b {
                    totals.bytes_done += b - bytes;
                } else {
                    totals.bytes_total += bytes - b;
                }
            }
            OpScan::Pending => {
                // No estimate yet: count the work as it happened. At least one unit.
                let units = files.max(1);
                totals.files_total += units;
                totals.files_done += units - files;
                totals.bytes_total += bytes;
            }
            OpScan::Settled => {}
        }
        *slot = OpScan::Settled;
    }
    /// The job is over (finished or cancelled). Estimates of operations that never ran leave
    /// the totals. When no job is left the counters reset to zero.
    pub fn end_job(&self, id: u64) {
        let mut s = self.lock();
        let State { totals, jobs } = &mut *s;
        if let Some(job) = jobs.remove(&id) {
            for slot in job {
                if let OpScan::Scanned { files, bytes } = slot {
                    totals.files_total = totals.files_total.saturating_sub(files);
                    totals.bytes_total = totals.bytes_total.saturating_sub(bytes);
                }
            }
        }
        if jobs.is_empty() {
            *totals = Totals::default();
        }
    }
}

pub struct ScanRequest {
    pub id: u64,
    pub ops: Arc<Vec<Operation>>,
    pub cancel: Cancellation,
}

/// Walk `source`: (regular files, bytes). Symlinks count as one file of no size.
fn walk(source: &Path) -> (u64, u64) {
    let mut files = 0;
    let mut bytes = 0;
    let mut stack = vec![source.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let meta = match fs::symlink_metadata(&dir) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            files += 1;
            if !meta.file_type().is_symlink() {
                bytes += meta.len();
            }
            continue;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let Ok(t) = entry.file_type() else { continue };
            if t.is_dir() && !t.is_symlink() {
                stack.push(entry.path());
            } else {
                files += 1;
                if t.is_file() {
                    bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
    }
    (files, bytes)
}

fn same_volume(a: &Path, b: &Path) -> bool {
    fn drive(p: &Path) -> Option<char> {
        let s = std::path::absolute(p).ok()?;
        let s = s.to_string_lossy().into_owned();
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
        let mut c = s.chars();
        let letter = c.next()?;
        (c.next() == Some(':')).then(|| letter.to_ascii_lowercase())
    }
    match (drive(a), drive(b.parent().unwrap_or(b))) {
        (Some(x), Some(y)) => x == y,
        _ => false, // UNC or unknown: assume data moves
    }
}

/// Estimate (files, bytes) for one operation without changing anything.
pub fn estimate(op: &Operation) -> (u64, u64) {
    match op {
        Operation::Copy { source, .. } => walk(source),
        Operation::Move { source, target } if !same_volume(source, target) => walk(source),
        Operation::Sequence(ops) => ops
            .iter()
            .map(estimate)
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1)),
        _ => (1, 0),
    }
}

/// Scanner thread body: size every operation of every submitted batch, in order.
pub fn scan_loop(rx: std::sync::mpsc::Receiver<ScanRequest>, shared: Shared) -> io::Result<()> {
    for req in rx {
        for (i, op) in req.ops.iter().enumerate() {
            if req.cancel.is_cancelled() || !shared.is_open(req.id) {
                break;
            }
            let (f, b) = estimate(op);
            shared.scanned(req.id, i, f, b);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_then_work_then_reset() {
        let s = Shared::default();
        s.begin_job(1, 2);
        s.scanned(1, 0, 3, 300);
        s.scanned(1, 1, 1, 0);
        assert_eq!(s.snapshot().files_total, 4);
        s.step(3, 300);
        s.op_finished(1, 0, 3, 300);
        s.op_finished(1, 1, 0, 0); // rename: no steps, estimate closes the gap
        let t = s.snapshot();
        assert_eq!((t.files_done, t.files_total), (4, 4));
        assert_eq!((t.bytes_done, t.bytes_total), (300, 300));
        s.end_job(1);
        assert_eq!(s.snapshot(), Totals::default());
    }

    #[test]
    fn work_before_scan_is_not_counted_twice() {
        let s = Shared::default();
        s.begin_job(1, 1);
        s.step(2, 50);
        s.op_finished(1, 0, 2, 50); // worker was faster than the scanner
        s.scanned(1, 0, 2, 50); // late scan result is ignored
        let t = s.snapshot();
        assert_eq!((t.files_done, t.files_total, t.bytes_total), (2, 2, 50));
    }

    #[test]
    fn queue_wide_across_jobs_and_cancel() {
        let s = Shared::default();
        s.begin_job(1, 1);
        s.begin_job(2, 1);
        s.scanned(1, 0, 10, 100);
        s.scanned(2, 0, 5, 50);
        assert_eq!(s.snapshot().files_total, 15);
        // job 2 is cancelled before it runs: its estimate leaves the total
        s.end_job(2);
        assert_eq!(s.snapshot().files_total, 10);
        s.step(10, 100);
        s.op_finished(1, 0, 10, 100);
        s.end_job(1);
        assert_eq!(s.snapshot(), Totals::default());
    }

    #[test]
    fn estimate_walks_folders() {
        let dir = std::env::temp_dir().join(format!("tsubame-est-{}", std::process::id()));
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join("x"), [0u8; 10]).unwrap();
        fs::write(dir.join("a/y"), [0u8; 20]).unwrap();
        fs::write(dir.join("a/b/z"), [0u8; 30]).unwrap();
        let (f, b) = estimate(&Operation::Copy {
            source: dir.clone(),
            target: dir.join("never"),
        });
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!((f, b), (3, 60));
    }
}
