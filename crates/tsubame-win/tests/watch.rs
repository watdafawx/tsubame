//! Real-directory tests for the directory watcher: single events, coalescing,
//! a 10,000-file burst, folder deleted/renamed, overflow, handle release and an
//! end-to-end check that applying the batches keeps the entry store identical to
//! the disk. Fixtures live in the system temp dir and are removed on drop.
//!
//! Every wait in here is for an event, never for a length of time. The watcher
//! reports from its own thread, and a loaded CI runner routinely leaves that
//! thread unscheduled for longer than any sleep a test could pick; a fixed wait
//! then reads as "the event never came" and the test fails for no reason. That
//! is exactly what used to happen on `main` (a 350 ms quiet gap expiring before
//! the batch the test had just caused arrived).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use tsubame_core::{EntryStore, FOLDER, Selection, Sort};
use tsubame_win::enumerate::ErrorKind;
use tsubame_win::watch::{
    Batch, ChangeKind, DirWatcher, Gone, Message, Options, Unreliable, apply_batch,
};

// ---------------------------------------------------------------- fixtures

struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tsubame-watch-{}-{}-{tag}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create fixture dir");
        Self { path }
    }
    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Watch {
    watcher: DirWatcher,
    rx: Receiver<Message>,
    wakes: Arc<AtomicUsize>,
    dir: PathBuf,
    barriers: AtomicUsize,
}

fn watch(dir: &Path, opts: Options) -> Watch {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let (watcher, rx) = DirWatcher::start(
        dir,
        opts,
        Arc::new(move || {
            w.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .expect("start watcher");
    Watch {
        watcher,
        rx,
        wakes,
        dir: dir.to_path_buf(),
        barriers: AtomicUsize::new(0),
    }
}

// --------------------------------------------------------- synchronization

/// A deadline, not a wait: only a wedged or hung watcher reaches it.
const PATIENCE: Duration = Duration::from_secs(60);

/// Prefix of the barrier files below. Every barrier name contains it, so the
/// records a barrier itself causes can be told apart from the test's changes.
const BARRIER: &str = "tb-barrier";

/// Window used by the tests that assert what a set of filesystem calls reduced
/// to. One call can report several records, and a loaded runner can let the
/// watcher read them in two completions; a wide window keeps them in one
/// window, so the assertion tests the merge rules instead of the scheduler. The
/// rules themselves are unit-tested at fixed timestamps in
/// `src/watch/coalesce.rs`.
fn relaxed() -> Options {
    Options {
        window: Duration::from_secs(1),
        max_delay: Duration::from_secs(1),
        ..Options::default()
    }
}

/// Collects messages until `done` accepts one (that message is included), then
/// returns everything it saw. Panics only when `PATIENCE` runs out.
fn wait_message(
    rx: &Receiver<Message>,
    mut done: impl FnMut(&Message) -> bool,
) -> Vec<(Instant, Message)> {
    let end = Instant::now() + PATIENCE;
    let mut out: Vec<(Instant, Message)> = Vec::new();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(m) => {
                let hit = done(&m);
                out.push((Instant::now(), m));
                if hit {
                    return out;
                }
            }
            Err(RecvTimeoutError::Timeout) => panic!(
                "no expected message within {PATIENCE:?}; saw {:?}",
                describe(&out)
            ),
            Err(RecvTimeoutError::Disconnected) => panic!(
                "the watcher ended before the expected message; saw {:?}",
                describe(&out)
            ),
        }
    }
}

impl Watch {
    /// A fresh barrier name, unique per call so a late record of an earlier
    /// barrier can never be taken for a new one.
    fn next_barrier(&self) -> String {
        format!(
            "{BARRIER}-{:04}",
            self.barriers.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Creates `name` in the watched folder and waits until the watcher has
    /// reported it; returns every message up to and including that one.
    ///
    /// The kernel reports one directory handle's records in order, so a change
    /// to a file created *after* the test's own calls proves those calls have
    /// been through the coalescer too: this is the only sound way to know that
    /// "nothing else happened". The file is left in place, for callers that
    /// compare the store against the disk.
    fn barrier(&self, name: &str) -> Vec<(Instant, Message)> {
        fs::write(self.dir.join(name), b"").expect("write barrier file");
        let want = format!("+{name}");
        wait_message(&self.rx, |m| match m {
            Message::Batch(b) => reports(b, &want),
            _ => false,
        })
    }

    /// Waits until the watcher has reported everything caused before now, and
    /// returns those messages. Barrier records included.
    fn settle(&self) -> Vec<(Instant, Message)> {
        let name = self.next_barrier();
        let msgs = self.barrier(&name);
        let _ = fs::remove_file(self.dir.join(&name));
        msgs
    }

    /// Every change caused since the last call, and nothing still in flight.
    /// The barrier's own records are dropped.
    fn changes(&self) -> Vec<String> {
        let mut out = describe(&self.settle());
        out.retain(|d| !d.contains(BARRIER));
        out
    }

    /// Waits for the coalesced batch that reports `change` (`+name`, `-name`,
    /// `from>name`).
    fn batch_for(&self, change: &str) -> Batch {
        let msgs = wait_message(&self.rx, |m| match m {
            Message::Batch(b) => reports(b, change),
            _ => false,
        });
        msgs.into_iter()
            .find_map(|(_, m)| match m {
                Message::Batch(b) if reports(&b, change) => Some(b),
                _ => None,
            })
            .expect("the waited-for batch")
    }
}

/// Whether a batch reports `change` (`+name`, `-name`, `from>name`).
fn reports(b: &Batch, change: &str) -> bool {
    b.iter().any(|c| c.describe() == change)
}

fn describe(msgs: &[(Instant, Message)]) -> Vec<String> {
    let mut out = Vec::new();
    for (_, m) in msgs {
        match m {
            Message::Batch(b) => out.extend(b.describe()),
            other => out.push(format!("{other:?}")),
        }
    }
    out
}

fn batches(msgs: &[(Instant, Message)]) -> Vec<&Batch> {
    msgs.iter()
        .filter_map(|(_, m)| if let Message::Batch(b) = m { Some(b) } else { None })
        .collect()
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

// --------------------------------------------------------- process helpers

/// The process handle count, sampled until it stops moving: the hub frees
/// aborted reads on its own thread, so the count settles a little after the
/// drops return.
fn handles_settled() -> u32 {
    let end = Instant::now() + PATIENCE;
    let mut last = tsubame_win::procinfo::counters().handles;
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let now = tsubame_win::procinfo::counters().handles;
        if now == last {
            return now;
        }
        assert!(
            Instant::now() < end,
            "the handle count never settled: {last} -> {now}"
        );
        last = now;
    }
}

/// The handle count, waited for until it is at most `most`.
fn handles_at_most(most: u32) -> u32 {
    let end = Instant::now() + PATIENCE;
    loop {
        let now = tsubame_win::procinfo::counters().handles;
        if now <= most || Instant::now() >= end {
            return now;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Deletes `dir` once whatever held it has let go: with `FILE_SHARE_DELETE` the
/// removal succeeds early but the folder lingers (delete pending) until the last
/// handle closes.
fn remove_when_released(dir: &Path) {
    let end = Instant::now() + PATIENCE;
    loop {
        let _ = fs::remove_dir(dir);
        if !dir.exists() {
            return;
        }
        assert!(Instant::now() < end, "{} is still held open", dir.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ------------------------------------------------------------------- tests

#[test]
fn start_errors() {
    let fx = Fixture::new("errors");
    let wake: tsubame_win::enumerate::Wake = Arc::new(|| {});
    let missing = DirWatcher::start(fx.file("nope"), Options::default(), wake.clone());
    assert_eq!(missing.err().map(|e| e.kind()), Some(ErrorKind::NotFound));
    fs::write(fx.file("f.txt"), b"x").unwrap();
    let file = DirWatcher::start(fx.file("f.txt"), Options::default(), wake);
    assert_eq!(file.err().map(|e| e.code), Some(267)); // ERROR_DIRECTORY
}

#[test]
fn single_file_lifecycle() {
    let fx = Fixture::new("single");
    let wt = watch(&fx.path, relaxed());

    fs::write(fx.file("a.txt"), b"hello").unwrap();
    assert_eq!(wt.changes(), ["+a.txt"]);

    fs::write(fx.file("a.txt"), b"hello world").unwrap();
    assert_eq!(wt.changes(), ["~a.txt"]);

    fs::rename(fx.file("a.txt"), fx.file("b.txt")).unwrap();
    assert_eq!(wt.changes(), ["a.txt>b.txt"]);

    fs::remove_file(fx.file("b.txt")).unwrap();
    assert_eq!(wt.changes(), ["-b.txt"]);

    fs::create_dir(fx.file("sub")).unwrap();
    assert_eq!(wt.changes(), ["+sub"]);
    // activity inside a subfolder touches the folder's own entry (its time), nothing deeper
    fs::write(fx.file("sub").join("deep.txt"), b"x").unwrap();
    let c = wt.changes();
    assert!(c.iter().all(|s| s == "~sub"), "{c:?}");
    fs::remove_dir_all(fx.file("sub")).unwrap();
    assert_eq!(wt.changes(), ["-sub"]);

    assert!(wt.wakes.load(Ordering::Relaxed) >= 6, "wake runs once per message");
}

#[test]
fn coalescing_on_disk() {
    let fx = Fixture::new("coalesce");
    fs::write(fx.file("keep.txt"), b"k").unwrap();
    fs::write(fx.file("old.txt"), b"o").unwrap();
    let wt = watch(&fx.path, relaxed());

    // created and deleted inside one window: nothing at all
    fs::write(fx.file("tmp.txt"), b"x").unwrap();
    fs::remove_file(fx.file("tmp.txt")).unwrap();
    assert!(wt.changes().is_empty());

    // repeated modifies are one change
    for i in 0..20 {
        fs::write(fx.file("keep.txt"), vec![b'k'; i + 1]).unwrap();
    }
    assert_eq!(wt.changes(), ["~keep.txt"]);

    // delete + create of one name is a modify
    fs::remove_file(fx.file("old.txt")).unwrap();
    fs::write(fx.file("old.txt"), b"new").unwrap();
    assert_eq!(wt.changes(), ["~old.txt"]);

    // rename chain a -> b -> c
    fs::write(fx.file("a.txt"), b"a").unwrap();
    wt.changes();
    fs::rename(fx.file("a.txt"), fx.file("b.txt")).unwrap();
    fs::rename(fx.file("b.txt"), fx.file("c.txt")).unwrap();
    assert_eq!(wt.changes(), ["a.txt>c.txt"]);

    // rename over an existing file
    fs::write(fx.file("src.txt"), b"s").unwrap();
    fs::write(fx.file("dst.txt"), b"d").unwrap();
    wt.changes();
    fs::rename(fx.file("src.txt"), fx.file("dst.txt")).unwrap();
    let c = wt.changes();
    assert!(c.contains(&"src.txt>dst.txt".to_string()) || c == ["-src.txt", "~dst.txt"], "{c:?}");
}

#[test]
fn single_event_latency_is_about_the_window() {
    let fx = Fixture::new("latency");
    let window = Duration::from_millis(60);
    let wt = watch(&fx.path, Options { window, ..Options::default() });
    let mut measured = Vec::new();
    for i in 0..5 {
        std::thread::sleep(Duration::from_millis(150)); // let the last window go
        fs::write(fx.file(&format!("f{i}.txt")), b"x").unwrap();
        let b = wt.batch_for(&format!("+f{i}.txt"));
        // Cut time measured inside the watcher: how long the window stayed
        // open, with none of the runner's scheduling in it. A lone event must
        // wait for the window (no early flush) and must not wait for a second
        // event. Promptness itself is a benchmark, not a test: PLAN section 2
        // checks the latency budgets on a dedicated machine.
        measured.push(b.emitted - b.first_event);
    }
    println!("single-event coalescing latency: {measured:?}");
    for delay in &measured {
        assert!(*delay >= window, "a lone event was flushed early: {delay:?}");
        assert!(*delay < Duration::from_secs(10), "the window never closed: {delay:?}");
    }
}

#[test]
fn burst_of_10k_files_is_a_few_batches() {
    let fx = Fixture::new("burst");
    let wt = watch(&fx.path, Options::default());
    let t0 = Instant::now();
    for i in 0..10_000 {
        fs::write(fx.file(&format!("file_{i:05}.txt")), b"payload").unwrap();
    }
    let created_in = t0.elapsed();
    // Wait for the last file's create, not for a quiet gap: a stall between two
    // batches must not be read as "nothing left". The tail arrives on the window
    // timer with no further event to wake it, which is what this proves.
    let msgs = wait_message(&wt.rx, |m| match m {
        Message::Batch(b) => reports(b, "+file_09999.txt"),
        _ => false,
    });
    let bs = batches(&msgs);
    let total: usize = bs.iter().map(|b| b.len()).sum();
    let raw: u32 = bs.iter().map(|b| b.raw_events).sum();
    let overflow = msgs.iter().any(|(_, m)| matches!(m, Message::Overflow));
    println!(
        "burst: 10000 files written in {created_in:?}; {} batches, {total} changes from {raw} raw events",
        bs.len(),
    );
    assert!(!overflow, "the default 64 KB buffer overflowed");
    // every file is created exactly once; a few get a trailing "modified" when
    // their size/time record lands in the next window
    let created: HashSet<Vec<u16>> = bs
        .iter()
        .flat_map(|b| b.iter())
        .filter(|c| c.kind == ChangeKind::Created)
        .map(|c| c.name.to_vec())
        .collect();
    assert_eq!(created.len(), 10_000);
    assert!(bs.iter().flat_map(|b| b.iter()).all(|c| matches!(c.kind, ChangeKind::Created | ChangeKind::Modified)));
    assert!(total < 10_500, "{total} changes");
    // one batch per max_delay (250 ms) at most, plus the tail
    let ceiling = (created_in.as_millis() / 250) as usize + 3;
    assert!(bs.len() <= ceiling, "{} batches for {created_in:?}", bs.len());
}

#[test]
fn watched_folder_deleted() {
    let fx = Fixture::new("deleted");
    let dir = fx.file("watched");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("a.txt"), b"a").unwrap();
    let wt = watch(&dir, Options::default());
    assert!(wt.watcher.still_present());
    fs::remove_file(dir.join("a.txt")).unwrap();
    fs::remove_dir(&dir).unwrap();
    let msgs = wait_message(&wt.rx, |m| matches!(m, Message::Gone(_)));
    let text = describe(&msgs);
    println!("deleted: {text:?}");
    assert!(
        msgs.iter().any(|(_, m)| matches!(m, Message::Gone(Gone::Deleted))),
        "{text:?}"
    );
    // pending changes are flushed before Gone when the event was read first; the directory
    // handle can also report the deletion before the "-a.txt" record is read (a race, harmless:
    // the folder is gone and the caller navigates away on Gone), so only Gone is required.
    assert!(!wt.watcher.still_present());
    // terminal: nothing after Gone
    assert!(matches!(msgs.last().unwrap().1, Message::Gone(_)));
}

#[test]
fn watched_folder_renamed() {
    let fx = Fixture::new("renamed");
    let dir = fx.file("watched");
    fs::create_dir(&dir).unwrap();
    let wt = watch(&dir, Options::default());
    let moved = fx.file("moved");
    fs::rename(&dir, &moved).unwrap();
    // the rename itself raises no event; the focus-time check sees it
    assert!(!wt.watcher.still_present());
    // and so does the next event batch
    fs::write(moved.join("late.txt"), b"x").unwrap();
    let msgs = wait_message(&wt.rx, |m| matches!(m, Message::Gone(_)));
    assert!(
        msgs.iter().any(|(_, m)| matches!(m, Message::Gone(Gone::Renamed))),
        "{:?}",
        describe(&msgs)
    );
}

#[test]
fn drop_releases_the_handle_promptly() {
    let fx = Fixture::new("drop");
    let round_trip = |round: usize| {
        let dir = fx.file(&format!("w{round}"));
        fs::create_dir(&dir).unwrap();
        drop(watch(&dir, Options::default()));
        fs::remove_dir(&dir).unwrap();
    };
    for round in 0..5 {
        round_trip(round); // warm up: the hub thread and port exist from here on
    }
    let before = handles_settled();
    for round in 0..100 {
        let dir = fx.file(&format!("d{round}"));
        fs::create_dir(&dir).unwrap();
        let wt = watch(&dir, Options::default());
        fs::write(dir.join("x.txt"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let t0 = Instant::now();
        drop(wt);
        let took = t0.elapsed();
        // Drop closes the handle synchronously; it never waits for the pending
        // read or for a coalescing timer. A loaded runner can deschedule the
        // thread while it takes the watcher's lock, so the budget is loose on
        // purpose: it only has to catch a drop that actually waits.
        assert!(took < Duration::from_secs(2), "drop took {took:?}");
        // deletable right away, and really gone (no delete-pending leftover)
        fs::remove_file(dir.join("x.txt")).unwrap();
        remove_when_released(&dir);
    }
    let after = handles_at_most(before + 2);
    println!("handles {before} -> {after} over 100 watch/drop rounds");
    assert!(after <= before + 2, "handles {before} -> {after}");
}

#[test]
fn folder_can_be_deleted_while_watched() {
    // FILE_SHARE_DELETE: the watcher never blocks removing the folder it shows
    let fx = Fixture::new("sharedelete");
    let dir = fx.file("w");
    fs::create_dir(&dir).unwrap();
    let wt = watch(&dir, Options::default());
    fs::remove_dir(&dir).expect("delete a watched folder");
    drop(wt);
    assert!(!dir.exists());
}

#[test]
fn overflow_is_reported_and_watching_continues() {
    let fx = Fixture::new("overflow");
    // a record for this name is ~412 bytes: it cannot fit a 64-byte buffer
    let opts = Options { buffer_bytes: 64, max_buffer_bytes: 64, ..relaxed() };
    let wt = watch(&fx.path, opts);
    let long = "n".repeat(200);
    fs::write(fx.file(&long), b"x").unwrap();
    // Deterministic: the kernel cannot fit that record, so it reports the loss
    // instead of the change. No barrier here: an overflow discards whatever is
    // queued with it, so a barrier record could be dropped too.
    let msgs = wait_message(&wt.rx, |m| matches!(m, Message::Overflow));
    assert!(msgs.iter().any(|(_, m)| matches!(m, Message::Overflow)), "{:?}", describe(&msgs));
    // still alive, and short names fit (the tiny buffer may report further
    // overflows on the way: at 64 bytes it cannot always hold two records)
    fs::write(fx.file("a"), b"x").unwrap();
    assert_eq!(wt.batch_for("+a").describe(), ["+a"]);
}

#[test]
fn overflow_grows_the_buffer() {
    let fx = Fixture::new("grow");
    let opts = Options { buffer_bytes: 64, max_buffer_bytes: 16 * 1024, ..relaxed() };
    let wt = watch(&fx.path, opts);
    // ~212 bytes per record for these names: too large for the 64-byte start
    // buffer. Each overflow grows the buffer 4x, so a later file is reported
    // instead of lost.
    let long = "g".repeat(100);
    let mut overflows = 0;
    let mut delivered = false;
    for i in 0..8 {
        let name = format!("{long}{i}");
        fs::write(fx.file(&name), b"x").unwrap();
        let msgs = wait_message(&wt.rx, |_| true); // the next message, whatever it is
        overflows += msgs.iter().filter(|(_, m)| matches!(m, Message::Overflow)).count();
        if describe(&msgs).contains(&format!("+{name}")) {
            delivered = true;
            break;
        }
    }
    assert!(overflows >= 1, "a record larger than the buffer must overflow");
    assert!(delivered, "the buffer never grew enough to report a change");
    // and the grown buffer keeps reporting
    fs::write(fx.file("a"), b"x").unwrap();
    assert_eq!(wt.batch_for("+a").describe(), ["+a"]);
}

#[test]
fn unreliable_paths_are_reported() {
    // a path on a network share is flagged before anything else
    let wake: tsubame_win::enumerate::Wake = Arc::new(|| {});
    // `\\localhost\c$` needs admin rights; accept either outcome of opening it,
    // but when it opens the first message must be the warning
    if let Ok((_w, rx)) = DirWatcher::start(r"\\localhost\c$\Windows", Options::default(), wake) {
        match rx.recv_timeout(PATIENCE) {
            Ok(Message::Unreliable(Unreliable::Network)) => {}
            other => panic!("expected the network warning, got {other:?}"),
        }
    }
}

// ------------------------------------------------- store consistency (e2e)

/// name -> (size, is_dir) of what is really on disk.
fn disk(dir: &Path) -> BTreeMap<String, (u64, bool)> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            (e.file_name().to_string_lossy().into_owned(), (if m.is_dir() { 0 } else { m.len() }, m.is_dir()))
        })
        .collect()
}

fn store_listing(s: &EntryStore) -> BTreeMap<String, (u64, bool)> {
    s.view()
        .iter()
        .map(|&id| {
            let r = s.record(id).unwrap();
            (String::from_utf16(s.name(id).unwrap()).unwrap(), (r.size, r.flags & FOLDER != 0))
        })
        .collect()
}

fn lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *state >> 33
}

#[test]
fn applied_batches_keep_the_store_equal_to_the_disk() {
    let fx = Fixture::new("e2e");
    // start with a few files, store filled by hand (as the enumerator would)
    for i in 0..30 {
        fs::write(fx.file(&format!("seed{i:02}.txt")), vec![b's'; i]).unwrap();
    }
    let mut store = EntryStore::new();
    store.sort(Sort { folders_first: true, ..Sort::default() });
    let mut sel = Selection::new();
    let seed = disk(&fx.path);
    let names: Vec<Vec<u16>> = seed.keys().map(|n| w(n)).collect();
    let inputs: Vec<_> = names
        .iter()
        .zip(seed.values())
        .map(|(n, (size, dir))| tsubame_core::EntryInput {
            name: n,
            size: *size,
            modified: 0,
            flags: if *dir { FOLDER } else { 0 },
        })
        .collect();
    store.push_batch(store.generation(), &inputs).unwrap();
    sel.select_all(&store);
    let wt = watch(&fx.path, Options::default());

    let mut rng = 0x1234_5678_9abc_def0u64;
    let pool: Vec<String> = (0..40).map(|i| format!("n{i:02}.dat")).chain((0..6).map(|i| format!("dir{i}"))).collect();
    let mut applied = 0;
    for step in 0..400 {
        let a = &pool[(lcg(&mut rng) % pool.len() as u64) as usize];
        let b = &pool[(lcg(&mut rng) % pool.len() as u64) as usize];
        let (pa, pb) = (fx.file(a), fx.file(b));
        let is_dir_name = a.starts_with("dir");
        match lcg(&mut rng) % 6 {
            0 | 1 if !is_dir_name => {
                let _ = fs::write(&pa, vec![b'z'; (lcg(&mut rng) % 5000) as usize]);
            }
            2 if !is_dir_name => {
                let _ = fs::remove_file(&pa);
            }
            3 if is_dir_name => {
                let _ = fs::create_dir(&pa);
            }
            4 if is_dir_name => {
                let _ = fs::remove_dir(&pa);
            }
            5 if !is_dir_name && !b.starts_with("dir") => {
                let _ = fs::rename(&pa, &pb);
            }
            _ => {}
        }
        if step % 25 == 0 {
            std::thread::sleep(Duration::from_millis(90)); // let some windows close mid-run
            while let Ok(m) = wt.rx.try_recv() {
                if let Message::Batch(b) = m {
                    apply_batch(&mut store, Some(&mut sel), &fx.path, &b).unwrap();
                    applied += 1;
                }
            }
        }
    }
    // Barrier: the file is left on disk, so the store and the disk both have it
    // once its own change has been applied, and every change made above is
    // ahead of it in the same record stream.
    let barrier = wt.next_barrier();
    for (_, m) in wt.barrier(&barrier) {
        match m {
            Message::Batch(b) => {
                apply_batch(&mut store, Some(&mut sel), &fx.path, &b).unwrap();
                applied += 1;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    println!("e2e: {applied} batches applied");
    assert_eq!(store_listing(&store), disk(&fx.path));
    // the selection (everything at the start) only holds live entries and its
    // total tracks sizes
    assert!(sel.count() <= store.len());
    let live_selected: u128 = store
        .view()
        .iter()
        .filter(|&&id| sel.contains(id))
        .map(|&id| store.record(id).unwrap().size as u128)
        .sum();
    assert_eq!(sel.total_size(), live_selected);
    assert_eq!(sel.count(), store.view().iter().filter(|&&id| sel.contains(id)).count());
}

#[test]
fn rename_keeps_id_and_selection() {
    let fx = Fixture::new("rename-id");
    for n in ["a.txt", "b.txt", "c.txt"] {
        fs::write(fx.file(n), n.as_bytes()).unwrap();
    }
    let mut store = EntryStore::new();
    let names = [w("a.txt"), w("b.txt"), w("c.txt")];
    let inputs: Vec<_> = names
        .iter()
        .map(|n| tsubame_core::EntryInput { name: n, size: 5, modified: 0, flags: 0 })
        .collect();
    store.push_batch(0, &inputs).unwrap();
    let mut sel = Selection::new();
    sel.single(&store, 1); // b.txt
    let id = store.at(1).unwrap();
    assert_eq!(store.name(id).unwrap(), &w("b.txt")[..]);

    let wt = watch(&fx.path, relaxed());
    fs::rename(fx.file("b.txt"), fx.file("z.txt")).unwrap();
    fs::write(fx.file("z.txt"), b"much longer content").unwrap();
    // A barrier proves the rename and the write are both applied. Its name
    // ("tb-barrier-…") sorts before z.txt, so "re-sorted into place" still holds.
    let barrier = wt.next_barrier();
    for (_, m) in wt.barrier(&barrier) {
        if let Message::Batch(b) = m {
            apply_batch(&mut store, Some(&mut sel), &fx.path, &b).unwrap();
        }
    }
    assert_eq!(store.name(id).unwrap(), &w("z.txt")[..], "same ID, new name");
    assert!(sel.contains(id), "still selected");
    assert_eq!(sel.focus, Some(id));
    assert_eq!(sel.total_size(), 19, "selected bytes follow the new size");
    assert_eq!(store.view().last().copied(), Some(id), "re-sorted into place");
}

#[test]
fn apply_handles_files_gone_before_the_stat() {
    // Created, but already deleted when the batch is applied: no row, no error.
    let fx = Fixture::new("gone");
    let wt = watch(&fx.path, Options::default());
    fs::write(fx.file("ghost.txt"), b"x").unwrap();
    let batch = wt.batch_for("+ghost.txt");
    fs::remove_file(fx.file("ghost.txt")).unwrap();
    let mut store = EntryStore::new();
    let s = apply_batch(&mut store, None, &fx.path, &batch).unwrap();
    assert_eq!(s.created, 0);
    assert!(store.is_empty());
}

// ------------------------------------------------------- spawn (no blocking)

fn wait_armed(w: &DirWatcher) {
    let t0 = Instant::now();
    while !w.armed() {
        assert!(t0.elapsed() < PATIENCE, "never armed");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn spawn_errors_arrive_as_messages() {
    let fx = Fixture::new("spawn-errors");
    let wake: tsubame_win::enumerate::Wake = Arc::new(|| {});
    let (w, rx) = DirWatcher::spawn(fx.file("nope"), Options::default(), wake.clone()).expect("spawn");
    match rx.recv_timeout(PATIENCE) {
        Ok(Message::Failed(e)) => assert_eq!(e.kind(), ErrorKind::NotFound),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(!w.armed());
    fs::write(fx.file("f.txt"), b"x").unwrap();
    let (w, rx) = DirWatcher::spawn(fx.file("f.txt"), Options::default(), wake).expect("spawn");
    match rx.recv_timeout(PATIENCE) {
        Ok(Message::Failed(e)) => assert_eq!(e.code, 267), // ERROR_DIRECTORY
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(!w.armed());
}

#[test]
fn spawn_reports_changes_once_armed() {
    let fx = Fixture::new("spawn");
    let wakes = Arc::new(AtomicUsize::new(0));
    let k = wakes.clone();
    let (watcher, rx) = DirWatcher::spawn(
        &fx.path,
        relaxed(),
        Arc::new(move || {
            k.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .expect("spawn");
    wait_armed(&watcher);
    let wt = Watch {
        watcher,
        rx,
        wakes,
        dir: fx.path.clone(),
        barriers: AtomicUsize::new(0),
    };
    fs::write(fx.file("a.txt"), b"hello").unwrap();
    assert_eq!(wt.changes(), ["+a.txt"]);
    fs::remove_file(fx.file("a.txt")).unwrap();
    assert_eq!(wt.changes(), ["-a.txt"]);
}

#[test]
fn spawn_then_drop_at_once_releases_everything() {
    let fx = Fixture::new("spawn-drop");
    let wake: tsubame_win::enumerate::Wake = Arc::new(|| {});
    for _ in 0..5 {
        let (w, _rx) = DirWatcher::spawn(&fx.path, Options::default(), wake.clone()).unwrap();
        wait_armed(&w);
        drop(w);
    }
    let before = handles_settled();
    for round in 0..200 {
        let dir = fx.file(&format!("d{round}"));
        fs::create_dir(&dir).unwrap();
        // dropped before (or while) the opener thread runs
        drop(DirWatcher::spawn(&dir, Options::default(), wake.clone()).unwrap());
        if round % 2 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    // the opener thread and the hub free their handles asynchronously
    let settled = handles_at_most(before + 4);
    for round in 0..200 {
        // no handle left open: the folders delete for real
        let dir = fx.file(&format!("d{round}"));
        remove_when_released(&dir);
    }
    let after = handles_at_most(before + 4);
    println!("handles {before} -> {settled} -> {after} over 200 spawn/drop rounds");
    assert!(after <= before + 4, "handles {before} -> {after}");
}
