use super::journal::{Entry, Journal};
use super::*;
use std::{
    ffi::OsString,
    fs, io,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::{Mutex, mpsc::Receiver},
    time::{Duration, Instant},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "tsubame-fileops-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Wraps a backend and remembers every Recycle Bin item it created, so tests can empty
/// those entries from the real bin at the end.
struct Recording<B> {
    inner: B,
    items: std::sync::Arc<Mutex<Vec<RecycledItem>>>,
}
impl<B: OpBackend> OpBackend for Recording<B> {
    fn execute(
        &mut self,
        op: &Operation,
        cancel: &Cancellation,
        progress: &mut dyn FnMut(Step),
    ) -> io::Result<Option<Operation>> {
        let r = self.inner.execute(op, cancel, progress);
        if let Ok(Some(Operation::Restore(item))) = &r {
            self.items.lock().unwrap().push(item.clone());
        }
        r
    }
}

/// Queue on the real backend plus a cleanup handle for the bin.
struct Rig {
    q: FileOpQueue,
    rx: Receiver<Event>,
    bin: std::sync::Arc<Mutex<Vec<RecycledItem>>>,
}
impl Rig {
    fn new() -> Self {
        let bin = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (q, rx) = FileOpQueue::new(Recording {
            inner: FsBackend,
            items: bin.clone(),
        })
        .unwrap();
        Self { q, rx, bin }
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        // Items may be in the bin or already restored; purge only what is still there.
        // The shell needs an STA thread.
        let items = std::mem::take(&mut *self.bin.lock().unwrap());
        if items.is_empty() {
            return;
        }
        let _ = std::thread::spawn(move || unsafe {
            use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
            let ok = OleInitialize(None).is_ok();
            for item in &items {
                let _ = recycle::purge(item);
            }
            if ok {
                OleUninitialize();
            }
        })
        .join();
    }
}

#[derive(Default)]
struct Outcome {
    errors: Vec<String>,
    progress: Vec<(Instant, Totals)>,
    conflicts: usize,
    finished: Totals,
    cancelled: bool,
}

/// Drain events until `Finished`. Conflicts are answered with `answer`.
fn finish_with(rx: &Receiver<Event>, answer: Option<(Choice, bool)>) -> Outcome {
    let mut out = Outcome::default();
    loop {
        match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
            Event::Finished {
                totals, cancelled, ..
            } => {
                out.finished = totals;
                out.cancelled = cancelled;
                return out;
            }
            Event::Error { message, .. } => out.errors.push(message),
            Event::Progress { totals, .. } => out.progress.push((Instant::now(), totals)),
            Event::Conflict { reply, .. } => {
                out.conflicts += 1;
                let (c, all) = answer.expect("unexpected conflict");
                reply.answer(c, all);
            }
            Event::Started(_) => {}
        }
    }
}
fn finish(rx: &Receiver<Event>) -> Outcome {
    let o = finish_with(rx, None);
    assert!(o.errors.is_empty(), "errors: {:?}", o.errors);
    o
}

/// The Recycle Bin tests need a bin on the temp volume.
fn bin_or_skip(dir: &Path) -> bool {
    use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
    let ok = unsafe { OleInitialize(None) }.is_ok();
    let has = recycle::has_recycle_bin(dir);
    if ok {
        unsafe { OleUninitialize() };
    }
    if !has {
        eprintln!(
            "SKIP: {} is not on a fixed volume with a Recycle Bin",
            dir.display()
        );
    }
    has
}

#[test]
fn queue_copy_undo_redo() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let a = t.0.join("a");
    let b = t.0.join("b");
    fs::write(&a, b"hello").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Copy {
        source: a,
        target: b.clone(),
    }])
    .unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&b).unwrap(), b"hello");
    // Undo of a copy recycles the created copy.
    r.q.undo().unwrap();
    finish(&r.rx);
    assert!(!b.exists());
    r.q.redo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&b).unwrap(), b"hello");
}

#[test]
fn nested_move_rename_delete_and_self_rejection() {
    let t = Temp::new();
    let a = t.0.join("a");
    fs::create_dir(&a).unwrap();
    fs::write(a.join("file"), b"x").unwrap();
    let mut b = FsBackend;
    let c = Cancellation::default();
    assert!(
        b.execute(
            &Operation::Move {
                source: a.clone(),
                target: a.join("inside")
            },
            &c,
            &mut |_| {}
        )
        .is_err()
    );
    let dest = t.0.join("b");
    b.execute(
        &Operation::Move {
            source: a.clone(),
            target: dest.clone(),
        },
        &c,
        &mut |_| {},
    )
    .unwrap();
    assert!(!a.exists());
    let upper = t.0.join("B");
    b.execute(
        &Operation::Rename {
            source: dest,
            target: upper.clone(),
        },
        &c,
        &mut |_| {},
    )
    .unwrap();
    b.execute(
        &Operation::Delete {
            path: upper.clone(),
            permanent: true,
        },
        &c,
        &mut |_| {},
    )
    .unwrap();
    assert!(!upper.exists());
}

#[test]
fn cancel_copy_removes_partial_file() {
    let t = Temp::new();
    let a = t.0.join("a");
    let b = t.0.join("b");
    let f = fs::File::create(&a).unwrap();
    f.set_len(64 * 1024 * 1024).unwrap();
    let c = Cancellation::default();
    let result = FsBackend.execute(
        &Operation::Copy {
            source: a,
            target: b.clone(),
        },
        &c,
        &mut |_| c.cancel(),
    );
    assert!(result.is_err());
    assert!(!b.exists());
}

#[test]
fn keep_both_apply_all_and_skip() {
    let t = Temp::new();
    let a = t.0.join("a");
    let b = t.0.join("b.txt");
    fs::write(&a, b"new").unwrap();
    fs::write(&b, b"old").unwrap();
    let r = Rig::new();
    let op = Operation::Copy {
        source: a,
        target: b.clone(),
    };
    r.q.submit(vec![op.clone(), op.clone()]).unwrap();
    let o = finish_with(&r.rx, Some((Choice::KeepBoth, true)));
    assert!(o.errors.is_empty());
    assert_eq!(o.conflicts, 1, "apply-to-all asks once");
    assert!(t.0.join("b (2).txt").exists());
    assert!(t.0.join("b (3).txt").exists());
    r.q.submit(vec![op]).unwrap();
    let o = finish_with(&r.rx, Some((Choice::Skip, false)));
    assert_eq!(o.conflicts, 1);
    assert_eq!(fs::read(b).unwrap(), b"old");
}

// ---------------------------------------------------------------- recycle

#[test]
fn recycle_delete_and_undo_restores_file() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let f = t.0.join("ünï cødé 100%.txt");
    fs::write(&f, b"precious").unwrap();
    let dir = t.0.join("folder");
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub").join("x.bin"), [7u8; 1000]).unwrap();
    let r = Rig::new();
    r.q.submit(vec![
        Operation::Delete {
            path: f.clone(),
            permanent: false,
        },
        Operation::Delete {
            path: dir.clone(),
            permanent: false,
        },
    ])
    .unwrap();
    finish(&r.rx);
    assert!(!f.exists() && !dir.exists());
    // Journal has two undoable entries whose inverses are Restore tokens.
    {
        let j = r.q.journal();
        let j = j.lock().unwrap();
        assert_eq!(j.undo_entries().len(), 2);
        for e in j.undo_entries() {
            assert!(matches!(e.inverse, Some(Operation::Restore(_))));
        }
    }
    // One undo per entry: newest first.
    r.q.undo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(dir.join("sub").join("x.bin")).unwrap().len(), 1000);
    assert!(!f.exists());
    r.q.undo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&f).unwrap(), b"precious");
    // Redo recycles again with a fresh token; undo of that works too.
    r.q.redo().unwrap();
    finish(&r.rx);
    assert!(!f.exists());
    r.q.undo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&f).unwrap(), b"precious");
}

#[test]
fn restore_refuses_to_overwrite() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let f = t.0.join("a.txt");
    fs::write(&f, b"old").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Delete {
        path: f.clone(),
        permanent: false,
    }])
    .unwrap();
    finish(&r.rx);
    fs::write(&f, b"new occupant").unwrap();
    r.q.undo().unwrap();
    let o = finish_with(&r.rx, None);
    assert_eq!(o.errors.len(), 1, "{:?}", o.errors);
    assert_eq!(fs::read(&f).unwrap(), b"new occupant");
    // The failed undo keeps its entry: free the path and undo again.
    fs::remove_file(&f).unwrap();
    r.q.undo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&f).unwrap(), b"old");
}

#[test]
fn permanent_delete_is_not_undoable_and_says_why() {
    let t = Temp::new();
    let f = t.0.join("gone.txt");
    fs::write(&f, b"x").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Delete {
        path: f.clone(),
        permanent: true,
    }])
    .unwrap();
    finish(&r.rx);
    assert!(!f.exists());
    let j = r.q.journal();
    let entry = j.lock().unwrap().undo_entries()[0].clone();
    assert!(!entry.is_undoable());
    assert!(entry.reason.as_deref().unwrap().contains("permanent"));
    let text = j.lock().unwrap().save_to_string();
    assert!(text.contains("permanent delete cannot be undone"));
    // Undo has nothing to run.
    r.q.undo().unwrap();
    finish(&r.rx);
    assert!(!f.exists());
}

#[test]
fn new_folder_undo() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let d = t.0.join("new folder");
    let r = Rig::new();
    r.q.submit(vec![Operation::NewFolder(d.clone())]).unwrap();
    finish(&r.rx);
    assert!(d.is_dir());
    r.q.undo().unwrap();
    finish(&r.rx);
    assert!(!d.exists());
}

// ---------------------------------------------------------------- replace

#[test]
fn replace_recycles_old_target_and_undo_restores_it() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let a = t.0.join("a.txt");
    let b = t.0.join("b.txt");
    fs::write(&a, b"NEW").unwrap();
    fs::write(&b, b"OLD").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Copy {
        source: a.clone(),
        target: b.clone(),
    }])
    .unwrap();
    let o = finish_with(&r.rx, Some((Choice::Replace, false)));
    assert!(o.errors.is_empty(), "{:?}", o.errors);
    assert_eq!(fs::read(&b).unwrap(), b"NEW");
    assert_eq!(fs::read(&a).unwrap(), b"NEW");
    r.q.undo().unwrap();
    finish(&r.rx);
    assert_eq!(
        fs::read(&b).unwrap(),
        b"OLD",
        "undo brings the old target back"
    );
    assert_eq!(fs::read(&a).unwrap(), b"NEW");
    // Redo replaces again (the restored old target is recycled once more).
    r.q.redo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&b).unwrap(), b"NEW");
    r.q.undo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&b).unwrap(), b"OLD");
}

#[test]
fn replace_move_over_folder_and_undo() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let src = t.0.join("src.txt");
    let dst = t.0.join("dst");
    fs::write(&src, b"file").unwrap();
    fs::create_dir(&dst).unwrap();
    fs::write(dst.join("inner"), b"inner").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Move {
        source: src.clone(),
        target: dst.clone(),
    }])
    .unwrap();
    finish_with(&r.rx, Some((Choice::Replace, false)));
    assert_eq!(fs::read(&dst).unwrap(), b"file");
    assert!(!src.exists());
    r.q.undo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&src).unwrap(), b"file");
    assert_eq!(fs::read(dst.join("inner")).unwrap(), b"inner");
}

#[test]
fn replace_onto_itself_is_refused() {
    let t = Temp::new();
    let a = t.0.join("a.txt");
    fs::write(&a, b"data").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Copy {
        source: a.clone(),
        target: a.clone(),
    }])
    .unwrap();
    let o = finish_with(&r.rx, Some((Choice::Replace, false)));
    assert_eq!(o.errors.len(), 1);
    assert_eq!(fs::read(&a).unwrap(), b"data", "source must survive");
}

#[test]
fn failed_replace_rolls_the_target_back() {
    struct FailCopy;
    impl OpBackend for FailCopy {
        fn execute(
            &mut self,
            op: &Operation,
            c: &Cancellation,
            p: &mut dyn FnMut(Step),
        ) -> io::Result<Option<Operation>> {
            if matches!(op, Operation::Copy { .. }) {
                return Err(io::Error::other("disk on fire"));
            }
            FsBackend.execute(op, c, p)
        }
    }
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let a = t.0.join("a.txt");
    let b = t.0.join("b.txt");
    fs::write(&a, b"NEW").unwrap();
    fs::write(&b, b"OLD").unwrap();
    let (q, rx) = FileOpQueue::new(FailCopy).unwrap();
    q.submit(vec![Operation::Copy {
        source: a,
        target: b.clone(),
    }])
    .unwrap();
    let o = finish_with(&rx, Some((Choice::Replace, false)));
    assert_eq!(o.errors, vec!["disk on fire".to_string()]);
    assert_eq!(fs::read(&b).unwrap(), b"OLD", "old target must come back");
}

// ---------------------------------------------------------------- long paths

fn deep(root: &Path) -> PathBuf {
    let mut p = root.to_path_buf();
    for i in 0..8 {
        p.push(format!("{i}{}", "d".repeat(40)));
    }
    p
}

#[test]
fn long_path_copy_and_move() {
    let t = Temp::new();
    let src_dir = deep(&t.0.join("s"));
    let dst_dir = deep(&t.0.join("d"));
    assert!(src_dir.as_os_str().len() > 300);
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();
    let file = src_dir.join("payload.bin");
    fs::write(&file, vec![5u8; 300_000]).unwrap();
    let copy = dst_dir.join("copy.bin");
    let moved = dst_dir.join("moved.bin");
    let tree = src_dir.join("tree");
    fs::create_dir_all(tree.join("inner")).unwrap();
    fs::write(tree.join("inner").join("f"), b"deep").unwrap();
    let tree_copy = dst_dir.join("tree-copy");
    let r = Rig::new();
    r.q.submit(vec![
        Operation::Copy {
            source: file.clone(),
            target: copy.clone(),
        },
        Operation::Copy {
            source: tree.clone(),
            target: tree_copy.clone(),
        },
        Operation::Move {
            source: file.clone(),
            target: moved.clone(),
        },
    ])
    .unwrap();
    let o = finish(&r.rx);
    assert_eq!(fs::read(&copy).unwrap().len(), 300_000);
    assert_eq!(
        fs::read(tree_copy.join("inner").join("f")).unwrap(),
        b"deep"
    );
    assert_eq!(fs::read(&moved).unwrap().len(), 300_000);
    assert!(!file.exists());
    assert_eq!(o.finished.files_total, 3, "copy + file in tree + move");
    // Undo the move: back where it was.
    r.q.undo().unwrap();
    finish(&r.rx);
    assert!(file.exists() && !moved.exists());
}

/// A file path of exactly `len` characters under `root`.
fn file_path_of_len(root: &Path, len: usize) -> PathBuf {
    let mut p = root.to_path_buf();
    while p.as_os_str().len() + 1 + 40 + 1 + 20 < len {
        p.push("d".repeat(40));
    }
    let name = len - p.as_os_str().len() - 1;
    p.push("f".repeat(name));
    assert_eq!(p.as_os_str().len(), len);
    p
}

#[test]
fn recycle_near_the_path_limit_works() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let file = file_path_of_len(&t.0, 255);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"near").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Delete {
        path: file.clone(),
        permanent: false,
    }])
    .unwrap();
    finish(&r.rx);
    assert!(!file.exists());
    r.q.undo().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(&file).unwrap(), b"near");
}

#[test]
fn recycle_of_a_path_over_the_limit_is_refused_not_destroyed() {
    // The shell deletes such items permanently when asked to recycle them silently.
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let file = file_path_of_len(&t.0, 300);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"long").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Delete {
        path: file.clone(),
        permanent: false,
    }])
    .unwrap();
    let o = finish_with(&r.rx, None);
    assert_eq!(o.errors.len(), 1, "{:?}", o.errors);
    assert!(o.errors[0].contains("Recycle Bin"), "{}", o.errors[0]);
    assert_eq!(fs::read(&file).unwrap(), b"long", "file must be untouched");
    assert!(r.q.journal().lock().unwrap().undo_entries().is_empty());
    // An explicit permanent delete still works for such a path.
    r.q.submit(vec![Operation::Delete {
        path: file.clone(),
        permanent: true,
    }])
    .unwrap();
    finish(&r.rx);
    assert!(!file.exists());
}

// ---------------------------------------------------------------- progress

#[test]
fn throttle_unit_allows_at_most_30_per_second() {
    let mut t = Throttle::new(PROGRESS_INTERVAL);
    let start = Instant::now();
    // Offer an event every 100 microseconds for 2 seconds.
    let mut allowed = Vec::new();
    for i in 0..20_000u32 {
        let now = start + Duration::from_micros(100 * u64::from(i));
        if t.allow(now) {
            allowed.push(now);
        }
    }
    assert!(allowed.len() <= 60, "{} in 2 s", allowed.len());
    // Sliding one-second window never holds more than 30.
    for (i, &a) in allowed.iter().enumerate() {
        let n = allowed[i..]
            .iter()
            .take_while(|&&b| b - a < Duration::from_secs(1))
            .count();
        assert!(n <= 30, "{n} events inside one second");
    }
}

#[test]
fn progress_events_from_a_chatty_backend_are_throttled() {
    struct Chatty;
    impl OpBackend for Chatty {
        fn execute(
            &mut self,
            _: &Operation,
            _: &Cancellation,
            progress: &mut dyn FnMut(Step),
        ) -> io::Result<Option<Operation>> {
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(900) {
                progress(Step { bytes: 1, files: 0 });
            }
            Ok(None)
        }
    }
    let (q, rx) = FileOpQueue::new(Chatty).unwrap();
    // Two jobs back to back: the limit holds across the job boundary too.
    q.submit(vec![Operation::NewFile(PathBuf::from("x"))])
        .unwrap();
    q.submit(vec![Operation::NewFile(PathBuf::from("y"))])
        .unwrap();
    let a = finish_with(&rx, None);
    let b = finish_with(&rx, None);
    let mut times: Vec<Instant> = a.progress.iter().chain(&b.progress).map(|p| p.0).collect();
    times.sort();
    assert!(
        times.len() >= 20,
        "expected steady progress, got {}",
        times.len()
    );
    // The receiver stamps arrival, which can bunch events; count what was sent instead
    // by bounding the total: 1.8 s of work allows at most 1.8 * 30 + 1.
    assert!(times.len() <= 56, "{} events in ~1.8 s", times.len());
}

#[test]
fn totals_cover_the_whole_queue() {
    // A backend that pretends to copy slowly, so the scanner is done before the work ends.
    struct Slow;
    impl OpBackend for Slow {
        fn execute(
            &mut self,
            op: &Operation,
            _: &Cancellation,
            progress: &mut dyn FnMut(Step),
        ) -> io::Result<Option<Operation>> {
            if let Operation::Copy { source, .. } = op
                && fs::metadata(source)?.is_file()
            {
                let len = fs::metadata(source)?.len();
                std::thread::sleep(Duration::from_millis(60));
                progress(Step {
                    bytes: len,
                    files: 1,
                });
            }
            Ok(None)
        }
    }
    let t = Temp::new();
    let mut ops = Vec::new();
    let mut bytes = 0;
    for i in 0..4 {
        let p = t.0.join(format!("f{i}"));
        fs::write(&p, vec![0u8; 1000 * (i + 1)]).unwrap();
        bytes += 1000 * (i as u64 + 1);
        ops.push(Operation::Copy {
            source: p,
            target: t.0.join(format!("c{i}")),
        });
    }
    let dir = t.0.join("tree");
    fs::create_dir_all(dir.join("a")).unwrap();
    fs::write(dir.join("a").join("x"), [1u8; 500]).unwrap();
    fs::write(dir.join("y"), [1u8; 700]).unwrap();
    // The scan sees the folder's two files; the slow backend does not copy it (no steps),
    // so the estimate is closed by the queue when the item ends.
    ops.push(Operation::Copy {
        source: dir,
        target: t.0.join("tree2"),
    });
    let (q, rx) = FileOpQueue::new(Slow).unwrap();
    q.submit(ops).unwrap();
    let o = finish_with(&rx, None);
    // Queue totals: 4 files + 2 files inside the folder, all bytes counted.
    assert_eq!(o.finished.files_total, 6, "{:?}", o.finished);
    assert_eq!(o.finished.files_done, 6);
    assert_eq!(o.finished.bytes_total, bytes + 1200);
    assert_eq!(o.finished.bytes_done, bytes + 1200);
    let seen_full_total = o.progress.iter().any(|(_, t)| t.files_total == 6);
    assert!(
        seen_full_total,
        "progress events must carry the pre-scanned queue total: {:?}",
        o.progress.iter().map(|p| p.1).collect::<Vec<_>>()
    );
    // Idle again: counters are back to zero.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(q.totals(), Totals::default());
}

// ---------------------------------------------------------------- journal

fn weird_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [
        r"C:\plain\file.txt",
        r"C:\with space\and  double  space .txt",
        r"C:\100%\%41\%u0041\%%.txt",
        "C:\\tab\there\\a\tb.txt",
        r"C:\ünï\cødé\日本語\😀 emoji.txt",
        r"\\server\share\x y\z",
        r"C:\trailing dot.\x.",
        "C:\\control\u{1}\u{7f}\u{85}.txt",
        "C:\\cr\rlf\n.txt",
        "",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    // A lone surrogate is a legal Windows file name but not valid Unicode.
    v.push(PathBuf::from(OsString::from_wide(&[
        b'C' as u16,
        b':' as u16,
        b'\\' as u16,
        0xD800,
        b'x' as u16,
    ])));
    v
}

fn sample_journal() -> Journal {
    let mut j = Journal::default();
    for (i, p) in weird_paths().into_iter().enumerate() {
        let q = p.with_extension("copy");
        j.record(Entry {
            forward: Operation::Copy {
                source: p.clone(),
                target: q.clone(),
            },
            inverse: Some(Operation::Delete {
                path: q.clone(),
                permanent: i % 2 == 0,
            }),
            reason: None,
            group: 0,
        });
        j.record(Entry {
            forward: Operation::Delete {
                path: p.clone(),
                permanent: false,
            },
            inverse: Some(Operation::Restore(RecycledItem {
                original: p.clone(),
                recycled: PathBuf::from(r"C:\$Recycle.Bin\S-1-5-21-1\$R1ABC.txt"),
            })),
            reason: None,
            group: 0,
        });
        j.record(Entry {
            forward: Operation::Sequence(vec![
                Operation::Delete {
                    path: q.clone(),
                    permanent: false,
                },
                Operation::Move {
                    source: p.clone(),
                    target: q,
                },
            ]),
            inverse: None,
            reason: Some("weird\treason with 100% and\nnewline é".into()),
            group: 0,
        });
    }
    j.record(Entry {
        forward: Operation::NewFolder(PathBuf::from(r"C:\n")),
        inverse: Some(Operation::Rename {
            source: PathBuf::from("a"),
            target: PathBuf::from("b"),
        }),
        reason: None,
        group: 0,
    });
    j.record(Entry {
        forward: Operation::NewFile(PathBuf::from(r"C:\f")),
        inverse: None,
        reason: Some("x".into()),
        group: 0,
    });
    j
}

#[test]
fn journal_round_trip_with_weird_paths() {
    let mut j = sample_journal();
    // Put a few entries on the redo stack through the real transitions.
    let (i, _) = j.next_undo().unwrap();
    j.mark_undone(i);
    let (i, _) = j.next_undo().unwrap();
    j.mark_undone(i);
    assert_eq!(j.redo_entries().len(), 2);
    let text = j.save_to_string();
    assert!(text.starts_with("tsubame-journal\t1\n"));
    // One record per line: every non-header line is a record.
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        1 + j.undo_entries().len() + j.redo_entries().len(),
        "escaping must keep records on one line"
    );
    for l in &lines[1..] {
        assert!(l.starts_with("U\t") || l.starts_with("R\t"), "{l:?}");
    }
    let (back, report) = Journal::load_from_str(&text);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.loaded, lines.len() - 1);
    assert_eq!(back.undo_entries(), j.undo_entries());
    assert_eq!(back.redo_entries(), j.redo_entries());
    // And once more: saving what was loaded gives identical text.
    assert_eq!(back.save_to_string(), text);
}

#[test]
fn journal_corrupt_input_is_skipped_and_counted() {
    let good = "U\tcopy\tC:\\a\tC:\\b\tdelete\tC:\\b\t0\t";
    let text = [
        "tsubame-journal\t1",
        good,
        "garbage with no tabs",
        "X\tcopy\ta\tb\t-\t",                // unknown stack tag
        "U\tteleport\ta\tb\t-\t",            // unknown operation
        "U\tcopy\ta",                        // truncated
        "U\tcopy\ta\tb\t-",                  // missing reason field
        "U\tcopy\tC:\\%zz\tb\t-\t",          // bad escape
        "U\tcopy\tC:\\%\tb\t-\t",            // dangling percent
        "U\tcopy\tC:\\%u12\tb\t-\t",         // short unicode escape
        "U\tcopy\ta\tb\t-\t\textra",         // trailing junk
        "U\tdelete\ta\t2\t-\t",              // bad flag
        "U\tseq\t99999999\tcopy\ta\tb\t-\t", // absurd sequence count
        "U\tseq\t2\tcopy\ta\tb\t-\t",        // sequence shorter than its count
        "U\tcopy\ta\tb\t-\t", // good: no inverse needs a reason, but empty is accepted
        "",
        "   ",
        "R\tdelete\tC:\\z\t1\t-\tpermanent delete cannot be undone",
    ]
    .join("\n");
    let (j, report) = Journal::load_from_str(&text);
    assert_eq!(report.loaded, 3, "{report:?}");
    assert_eq!(report.skipped, 12, "{report:?}");
    assert_eq!(j.undo_entries().len(), 2);
    assert_eq!(j.redo_entries().len(), 1);
}

#[test]
fn journal_bad_or_future_header_skips_everything() {
    for text in [
        "",
        "not a journal\nU\tcopy\ta\tb\t-\t\n",
        "tsubame-journal\t2\nU\tcopy\ta\tb\t-\t\n",
        "tsubame-journal\n",
        "tsubame-journal\t1\tx\nU\tcopy\ta\tb\t-\t\n",
    ] {
        let (j, report) = Journal::load_from_str(text);
        assert!(j.undo_entries().is_empty(), "{text:?}");
        assert_eq!(report.loaded, 0);
        if !text.is_empty() {
            assert!(report.skipped >= 1, "{text:?}: {report:?}");
        }
    }
}

#[test]
fn journal_never_panics_on_truncation_or_mutation() {
    let text = sample_journal().save_to_string();
    // Cut at every char boundary.
    for (i, _) in text.char_indices() {
        let _ = Journal::load_from_str(&text[..i]);
    }
    // Flip each byte position (ASCII-safe replacement) in a window of the text.
    let mut bytes = text.into_bytes();
    for i in (0..bytes.len().min(4000)).step_by(7) {
        let old = bytes[i];
        for repl in *b"%\tu-0" {
            bytes[i] = repl;
            if let Ok(s) = std::str::from_utf8(&bytes) {
                let _ = Journal::load_from_str(s);
            }
        }
        bytes[i] = old;
    }
}

#[test]
fn journal_cap_evicts_history_before_undoable_entries() {
    let mut j = Journal::with_capacity(3);
    let undoable = |n: &str| Entry {
        forward: Operation::NewFile(PathBuf::from(n)),
        inverse: Some(Operation::Delete {
            path: PathBuf::from(n),
            permanent: false,
        }),
        reason: None,
        group: 0,
    };
    let permanent = Entry {
        forward: Operation::Delete {
            path: PathBuf::from("p"),
            permanent: true,
        },
        inverse: None,
        reason: Some("permanent delete cannot be undone".into()),
        group: 0,
    };
    j.record(undoable("a"));
    j.record(permanent);
    j.record(undoable("b"));
    j.record(undoable("c"));
    let names: Vec<_> = j.undo_entries().iter().map(|e| e.is_undoable()).collect();
    assert_eq!(
        names,
        vec![true, true, true],
        "the non-undoable record went first"
    );
}

#[test]
fn saved_journal_survives_a_new_queue() {
    // Save after real work, load into a fresh queue, undo there.
    let t = Temp::new();
    let a = t.0.join("a.txt");
    let b = t.0.join("b.txt");
    fs::write(&a, b"1").unwrap();
    let r = Rig::new();
    r.q.submit(vec![Operation::Move {
        source: a.clone(),
        target: b.clone(),
    }])
    .unwrap();
    finish(&r.rx);
    let text = r.q.journal().lock().unwrap().save_to_string();
    drop(r);
    let (journal, report) = Journal::load_from_str(&text);
    assert_eq!(report.loaded, 1);
    let (q, rx) =
        FileOpQueue::with_journal(FsBackend, std::sync::Arc::new(Mutex::new(journal))).unwrap();
    q.undo().unwrap();
    finish(&rx);
    assert!(a.exists() && !b.exists());
}

#[cfg(test)]
mod benchmarks {
    use super::*;
    struct Fake;
    impl OpBackend for Fake {
        fn execute(
            &mut self,
            _: &Operation,
            _: &Cancellation,
            _: &mut dyn FnMut(Step),
        ) -> io::Result<Option<Operation>> {
            Ok(None)
        }
    }
    #[test]
    fn fake_queue_overhead() {
        let (q, rx) = FileOpQueue::new(Fake).unwrap();
        let ops = (0..100_000)
            .map(|_| Operation::NewFile(PathBuf::new()))
            .collect();
        let start = Instant::now();
        q.submit(ops).unwrap();
        let submit = start.elapsed();
        while !matches!(
            rx.recv_timeout(Duration::from_secs(60)).unwrap(),
            Event::Finished { .. }
        ) {}
        println!(
            "fake queue: submit {:.1} ms, {:.3} us/item total",
            submit.as_secs_f64() * 1e3,
            start.elapsed().as_secs_f64() * 1e6 / 100_000.0
        );
    }
    #[test]
    fn small_file_throughput() {
        let root = std::env::temp_dir().join(format!("tsubame-10k-{}", std::process::id()));
        assert!(!root.exists());
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let target = root.join("copy");
        fs::create_dir(&source).unwrap();
        for i in 0..10_000 {
            fs::write(source.join(format!("{i}.txt")), [42; 128]).unwrap();
        }
        let start = Instant::now();
        FsBackend
            .execute(
                &Operation::Copy {
                    source: source.clone(),
                    target: target.clone(),
                },
                &Cancellation::default(),
                &mut |_| {},
            )
            .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(fs::read_dir(target).unwrap().count(), 10_000);
        println!(
            "fs copy: {:.3}s, {:.0} files/s",
            elapsed.as_secs_f64(),
            10_000.0 / elapsed.as_secs_f64()
        );
        let start = Instant::now();
        use std::os::windows::process::CommandExt;
        let status = std::process::Command::new("robocopy")
            .arg(&source)
            .arg(root.join("robocopy"))
            .args(["/E", "/NFL", "/NDL", "/NJH", "/NJS", "/NP", "/R:0", "/W:0"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        let elapsed = start.elapsed();
        assert!(status.code().unwrap() < 8);
        println!(
            "robocopy: {:.3}s, {:.0} files/s",
            elapsed.as_secs_f64(),
            10_000.0 / elapsed.as_secs_f64()
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// The app's Ctrl+Z / Ctrl+Y: one submission (a paste of several items, a batch
/// rename, a delete of a selection) undoes and redoes as one step, newest first,
/// whatever kind of operation it was.
#[test]
fn group_undo_redo_of_mixed_operations() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let d = &t.0;
    for n in ["a.txt", "b.txt", "x.txt", "gone.txt"] {
        fs::write(d.join(n), n.as_bytes()).unwrap();
    }
    fs::create_dir(d.join("dst")).unwrap();
    let r = Rig::new();
    // 1: a paste of two items + a new folder in one submission
    r.q.submit(vec![
        Operation::Copy { source: d.join("a.txt"), target: d.join("dst").join("a.txt") },
        Operation::Move { source: d.join("b.txt"), target: d.join("dst").join("b.txt") },
        Operation::NewFolder(d.join("dst").join("new")),
    ])
    .unwrap();
    finish(&r.rx);
    // 2: a batch rename
    r.q.submit(vec![
        Operation::Rename { source: d.join("x.txt"), target: d.join("x1.txt") },
        Operation::Rename { source: d.join("dst").join("a.txt"), target: d.join("dst").join("a1.txt") },
    ])
    .unwrap();
    finish(&r.rx);
    // 3: a delete to the Recycle Bin
    r.q.submit(vec![Operation::Delete { path: d.join("gone.txt"), permanent: false }]).unwrap();
    finish(&r.rx);
    assert!(!d.join("gone.txt").exists() && d.join("x1.txt").exists() && d.join("dst").join("new").is_dir());

    // undo 3: the delete comes back
    r.q.undo_group().unwrap();
    finish(&r.rx);
    assert_eq!(fs::read(d.join("gone.txt")).unwrap(), b"gone.txt");
    assert!(d.join("x1.txt").exists(), "only the newest group");
    // undo 2: both renames revert in one step
    r.q.undo_group().unwrap();
    finish(&r.rx);
    assert!(d.join("x.txt").exists() && !d.join("x1.txt").exists());
    assert!(d.join("dst").join("a.txt").exists() && !d.join("dst").join("a1.txt").exists());
    // undo 1: copy, move and new folder revert together
    r.q.undo_group().unwrap();
    finish(&r.rx);
    assert!(!d.join("dst").join("a.txt").exists() && !d.join("dst").join("new").exists());
    assert!(d.join("b.txt").exists() && !d.join("dst").join("b.txt").exists());
    assert!(d.join("a.txt").exists(), "a copy's source stays");
    {
        let j = r.q.journal();
        let j = j.lock().unwrap();
        assert!(j.undo_entries().is_empty());
        assert_eq!(j.redo_entries().len(), 6);
    }
    // redo 1 then 2: original order inside each group (the rename of dst\a.txt
    // needs the copy that group 1 redoes first)
    r.q.redo_group().unwrap();
    finish(&r.rx);
    assert!(d.join("dst").join("a.txt").exists() && d.join("dst").join("b.txt").exists() && d.join("dst").join("new").is_dir());
    assert!(d.join("x.txt").exists(), "group 2 not redone yet");
    r.q.redo_group().unwrap();
    finish(&r.rx);
    assert!(d.join("x1.txt").exists() && d.join("dst").join("a1.txt").exists());
    // and the redone groups undo as groups again
    r.q.undo_group().unwrap();
    finish(&r.rx);
    assert!(d.join("x.txt").exists() && d.join("dst").join("a.txt").exists());
    {
        let j = r.q.journal();
        let j = j.lock().unwrap();
        assert_eq!(j.undo_entries().len(), 3);
        assert_eq!(j.redo_entries().len(), 3, "group 2 waits for redo, group 3 too");
    }
    // nothing to undo or redo is a finished job with no errors
    let empty = Rig::new();
    empty.q.undo_group().unwrap();
    finish(&empty.rx);
    empty.q.redo_group().unwrap();
    finish(&empty.rx);
}

#[test]
fn group_lookup_skips_history_and_keeps_groups_apart() {
    let e = |n: &str, g: u64, undoable: bool| Entry {
        forward: Operation::NewFile(PathBuf::from(n)),
        inverse: undoable.then(|| Operation::Delete { path: PathBuf::from(n), permanent: false }),
        reason: (!undoable).then(|| "x".to_string()),
        group: g,
    };
    let mut j = Journal::default();
    j.record(e("a", 1, true));
    j.record(e("b", 2, true));
    j.record(e("c", 2, false));
    j.record(e("d", 2, true));
    j.record(e("p", 3, false));
    // group 3 has nothing undoable: group 2 is next, newest first, without "c"
    let g: Vec<usize> = j.undo_group().into_iter().map(|(i, _)| i).collect();
    assert_eq!(g, vec![3, 1]);
    for (i, _) in j.undo_group() {
        j.mark_undone(i);
    }
    assert_eq!(j.redo_entries().len(), 2);
    let r: Vec<usize> = j.redo_group().into_iter().map(|(i, _)| i).collect();
    assert_eq!(r, vec![1, 0], "oldest first: b, then d");
    // a loaded journal: every entry its own group
    let (loaded, _) = Journal::load_from_str(&j.save_to_string());
    assert_eq!(loaded.undo_group().len(), 1);
    assert_eq!(loaded.redo_group().len(), 1);
}

// ---------------------------------------------------------------- B11-E2: pause, cancel, panel data

/// A backend whose every operation is 20 chunks of 1 byte, checking the gate between
/// chunks like the file system backend does. `ran` counts executed operations.
struct Chunky {
    ran: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    chunk: Duration,
}
impl OpBackend for Chunky {
    fn execute(
        &mut self,
        _: &Operation,
        cancel: &Cancellation,
        progress: &mut dyn FnMut(Step),
    ) -> io::Result<Option<Operation>> {
        self.ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        for _ in 0..20 {
            cancel.check()?;
            std::thread::sleep(self.chunk);
            progress(Step { bytes: 1, files: 0 });
        }
        progress(Step { bytes: 0, files: 1 });
        Ok(None)
    }
}

fn chunky(chunk_ms: u64) -> (FileOpQueue, Receiver<Event>, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (q, rx) = FileOpQueue::new(Chunky { ran: ran.clone(), chunk: Duration::from_millis(chunk_ms) }).unwrap();
    (q, rx, ran)
}

#[test]
fn pause_holds_the_worker_and_resume_continues() {
    let (q, rx, ran) = chunky(5);
    let t = q.submit(vec![Operation::NewFile("a".into()), Operation::NewFile("b".into())]).unwrap();
    // wait for the first progress, then pause
    loop {
        if let Event::Progress { .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            break;
        }
    }
    t.cancellation.pause();
    assert!(t.cancellation.is_paused());
    // drain what was in flight; afterwards the worker sends nothing while paused
    std::thread::sleep(Duration::from_millis(60));
    while rx.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(200));
    assert!(rx.try_recv().is_err(), "a paused job must not progress");
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
    t.cancellation.resume();
    let o = finish_with(&rx, None);
    assert!(!o.cancelled);
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 2, "both operations ran after resume");
}

/// Rig: Cancel on a running copy showed "1 failed" with Retry. A cancel that
/// interrupts an item ends the job as cancelled and reports no error.
#[test]
fn cancel_mid_item_is_not_an_error() {
    let (q, rx, ran) = chunky(5);
    let t = q.submit(vec![Operation::NewFile("a".into()), Operation::NewFile("b".into())]).unwrap();
    loop {
        if let Event::Progress { .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            break;
        }
    }
    t.cancellation.cancel();
    let o = finish_with(&rx, None);
    assert!(o.cancelled);
    assert!(o.errors.is_empty(), "errors: {:?}", o.errors);
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1, "the second item never starts");
}

#[test]
fn cancel_wakes_a_paused_job() {
    let (q, rx, _) = chunky(5);
    let t = q.submit(vec![Operation::NewFile("a".into())]).unwrap();
    t.cancellation.pause();
    std::thread::sleep(Duration::from_millis(50));
    t.cancellation.cancel();
    let o = finish_with(&rx, None);
    assert!(o.cancelled);
}

#[test]
fn cancelling_a_waiting_job_skips_it_without_running() {
    let (q, rx, ran) = chunky(10);
    let first = q.submit(vec![Operation::NewFile("a".into())]).unwrap();
    let second = q.submit(vec![Operation::NewFile("b".into()), Operation::NewFile("c".into())]).unwrap();
    second.cancellation.cancel();
    let mut started = Vec::new();
    let mut finished = Vec::new();
    while finished.len() < 2 {
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Started(id) => started.push(id),
            Event::Finished { id, cancelled, job, .. } => finished.push((id, cancelled, job)),
            _ => {}
        }
    }
    assert_eq!(started, vec![first.id], "the cancelled job never starts");
    assert_eq!(finished[0].0, first.id);
    assert!(!finished[0].1);
    assert_eq!(finished[0].2.bytes_done, 20, "per-job totals of the job that ran");
    assert_eq!(finished[1], (second.id, true, Totals::default()));
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn progress_carries_the_jobs_own_totals_and_index() {
    let (q, rx, _) = chunky(2);
    let t = q.submit(vec![Operation::NewFile("a".into()), Operation::NewFile("b".into())]).unwrap();
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Progress { id, index, job, .. } => {
                assert_eq!(id, t.id);
                seen.push((index, job.bytes_done));
            }
            Event::Finished { job, .. } => {
                assert_eq!(job.bytes_done, 40);
                break;
            }
            _ => {}
        }
    }
    assert!(seen.iter().any(|s| s.0 == 1), "index moves to the second operation: {seen:?}");
    assert!(seen.windows(2).all(|w| w[0].1 <= w[1].1), "job bytes only grow");
}

#[test]
fn conflict_reports_both_sides_and_cancel_stops_the_job() {
    let t = Temp::new();
    let src = t.0.join("in.txt");
    let dst = t.0.join("out");
    fs::create_dir(&dst).unwrap();
    fs::write(&src, b"incoming!").unwrap();
    fs::write(dst.join("in.txt"), b"old").unwrap();
    fs::write(t.0.join("second.txt"), b"2").unwrap();
    let r = Rig::new();
    r.q.submit(vec![
        Operation::Copy { source: src.clone(), target: dst.join("in.txt") },
        Operation::Copy { source: t.0.join("second.txt"), target: dst.join("second.txt") },
    ])
    .unwrap();
    let cancelled = loop {
        match r.rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Conflict { source, target, existing, incoming, reply, .. } => {
                assert_eq!(source, src);
                assert_eq!(target, dst.join("in.txt"));
                assert!(existing.known && incoming.known);
                assert_eq!((existing.size, incoming.size), (3, 9));
                assert!(!existing.dir && existing.modified.is_some());
                reply.answer(Choice::Cancel, false);
            }
            Event::Finished { cancelled, .. } => break cancelled,
            _ => {}
        }
    };
    assert!(cancelled);
    assert_eq!(fs::read(dst.join("in.txt")).unwrap(), b"old");
    assert!(!dst.join("second.txt").exists(), "Cancel operation stops the rest too");
}

#[test]
fn undo_group_of_runs_only_for_the_newest_group() {
    let t = Temp::new();
    if !bin_or_skip(&t.0) {
        return;
    }
    let a = t.0.join("a");
    let b = t.0.join("b");
    fs::write(&a, b"1").unwrap();
    fs::write(&b, b"2").unwrap();
    let r = Rig::new();
    let first = r.q.submit(vec![Operation::Rename { source: a.clone(), target: t.0.join("a2") }]).unwrap();
    finish(&r.rx);
    let second = r.q.submit(vec![Operation::Rename { source: b.clone(), target: t.0.join("b2") }]).unwrap();
    finish(&r.rx);
    // the older group is not the newest any more: nothing happens
    r.q.undo_group_of(first.id).unwrap();
    finish(&r.rx);
    assert!(t.0.join("a2").exists() && t.0.join("b2").exists());
    r.q.undo_group_of(second.id).unwrap();
    finish(&r.rx);
    assert!(b.exists() && !t.0.join("b2").exists());
    assert_eq!(lock_journal(&r.q).newest_undo_group(), Some(first.id));
}

fn lock_journal(q: &FileOpQueue) -> Journal {
    q.journal().lock().unwrap().clone()
}
