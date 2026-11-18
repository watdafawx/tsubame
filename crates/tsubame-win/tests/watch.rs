//! Real-directory tests for the directory watcher: single events, coalescing,
//! a 10,000-file burst, folder deleted/renamed, overflow, handle release and an
//! end-to-end check that applying the batches keeps the entry store identical to
//! the disk. Fixtures live in the system temp dir and are removed on drop.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
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
    Watch { watcher, rx, wakes }
}

impl Watch {
    /// Every message until `quiet` passes without one (or `max` in total).
    fn drain(&self, quiet: Duration, max: Duration) -> Vec<(Instant, Message)> {
        let end = Instant::now() + max;
        let mut out = Vec::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return out;
            }
            match self.rx.recv_timeout(quiet.min(left)) {
                Ok(m) => out.push((Instant::now(), m)),
                Err(RecvTimeoutError::Timeout) => return out,
                Err(RecvTimeoutError::Disconnected) => return out,
            }
        }
    }

    /// Descriptions of every change received until quiet, plus other messages' names.
    fn changes(&self) -> Vec<String> {
        describe(&self.drain(Duration::from_millis(350), Duration::from_secs(10)))
    }
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

const QUIET: Duration = Duration::from_millis(350);

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
    let wt = watch(&fx.path, Options::default());

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
    let wt = watch(&fx.path, Options::default());

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
    let opts = Options { window: Duration::from_millis(60), ..Options::default() };
    let wt = watch(&fx.path, opts);
    let mut lat = Vec::new();
    for i in 0..5 {
        std::thread::sleep(Duration::from_millis(150));
        let t0 = Instant::now();
        fs::write(fx.file(&format!("f{i}.txt")), b"x").unwrap();
        let (at, msg) = wt.drain(Duration::from_secs(2), Duration::from_secs(2)).remove(0);
        assert!(matches!(msg, Message::Batch(_)));
        lat.push(at - t0);
    }
    println!("single-event latency: {lat:?}");
    for l in &lat {
        assert!(*l >= Duration::from_millis(50), "{l:?}");
        assert!(*l < Duration::from_millis(250), "{l:?}");
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
    let t_end = Instant::now();
    let msgs = wt.drain(QUIET, Duration::from_secs(20));
    let bs = batches(&msgs);
    let total: usize = bs.iter().map(|b| b.len()).sum();
    let raw: u32 = bs.iter().map(|b| b.raw_events).sum();
    let overflow = msgs.iter().any(|(_, m)| matches!(m, Message::Overflow));
    let last = msgs.last().map(|(t, _)| *t).unwrap_or(t_end);
    println!(
        "burst: 10000 files written in {created_in:?}; {} batches, {total} changes from {raw} raw events, last batch {:?} after the last write",
        bs.len(),
        last.saturating_duration_since(t_end),
    );
    assert!(!overflow, "default buffer overflowed");
    // every file is created exactly once; a few get a trailing "modified" when
    // their size/time record lands in the next window
    let created: std::collections::HashSet<_> = bs
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
    // the tail is delivered about one window after the last write
    assert!(last.saturating_duration_since(t_end) < Duration::from_millis(500));
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
    let msgs = wt.drain(Duration::from_secs(3), Duration::from_secs(5));
    let text = describe(&msgs);
    println!("deleted: {text:?}");
    assert!(
        msgs.iter().any(|(_, m)| matches!(m, Message::Gone(Gone::Deleted))),
        "{text:?}"
    );
    assert!(text.contains(&"-a.txt".to_string()), "pending changes are flushed first: {text:?}");
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
    let msgs = wt.drain(Duration::from_secs(1), Duration::from_secs(3));
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
    std::thread::sleep(Duration::from_millis(200));
    let before = tsubame_win::procinfo::counters().handles;
    for round in 0..100 {
        let dir = fx.file(&format!("d{round}"));
        fs::create_dir(&dir).unwrap();
        let wt = watch(&dir, Options::default());
        fs::write(dir.join("x.txt"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let t0 = Instant::now();
        drop(wt);
        assert!(t0.elapsed() < Duration::from_millis(100), "drop took {:?}", t0.elapsed());
        // deletable right away, and really gone (no delete-pending leftover)
        fs::remove_file(dir.join("x.txt")).unwrap();
        fs::remove_dir(&dir).unwrap();
        assert!(!dir.exists());
    }
    std::thread::sleep(Duration::from_millis(200)); // the hub frees the aborted reads
    let after = tsubame_win::procinfo::counters().handles;
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
    let opts = Options { buffer_bytes: 64, max_buffer_bytes: 64, ..Options::default() };
    let wt = watch(&fx.path, opts);
    let long = "n".repeat(200);
    fs::write(fx.file(&long), b"x").unwrap();
    let msgs = wt.drain(QUIET, Duration::from_secs(5));
    assert!(msgs.iter().any(|(_, m)| matches!(m, Message::Overflow)), "{:?}", describe(&msgs));
    // still alive, and short names fit
    fs::write(fx.file("a"), b"x").unwrap();
    assert_eq!(wt.changes(), ["+a"]);
}

#[test]
fn overflow_grows_the_buffer() {
    let fx = Fixture::new("grow");
    let opts = Options { buffer_bytes: 64, max_buffer_bytes: 4096, ..Options::default() };
    let wt = watch(&fx.path, opts);
    let long = "g".repeat(100); // ~212 bytes per record
    fs::write(fx.file(&format!("{long}1")), b"x").unwrap();
    let first = wt.drain(QUIET, Duration::from_secs(5));
    assert!(first.iter().any(|(_, m)| matches!(m, Message::Overflow)));
    fs::write(fx.file(&format!("{long}2")), b"x").unwrap();
    let second = wt.drain(QUIET, Duration::from_secs(5));
    let text = describe(&second);
    assert!(text.iter().any(|s| s.starts_with('+')), "grown buffer delivers: {text:?}");
}

#[test]
fn unreliable_paths_are_reported() {
    // a path on a network share is flagged before anything else
    let wake: tsubame_win::enumerate::Wake = Arc::new(|| {});
    // `\\localhost\c$` needs admin rights; accept either outcome of opening it,
    // but when it opens the first message must be the warning
    if let Ok((_w, rx)) = DirWatcher::start(r"\\localhost\c$\Windows", Options::default(), wake) {
        match rx.recv_timeout(Duration::from_secs(1)) {
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
    for (_, m) in wt.drain(QUIET, Duration::from_secs(10)) {
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

    let wt = watch(&fx.path, Options::default());
    fs::rename(fx.file("b.txt"), fx.file("z.txt")).unwrap();
    fs::write(fx.file("z.txt"), b"much longer content").unwrap();
    for (_, m) in wt.drain(QUIET, Duration::from_secs(5)) {
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
    let msgs = wt.drain(QUIET, Duration::from_secs(5));
    fs::remove_file(fx.file("ghost.txt")).unwrap();
    let mut store = EntryStore::new();
    for (_, m) in msgs {
        if let Message::Batch(b) = m {
            let s = apply_batch(&mut store, None, &fx.path, &b).unwrap();
            assert_eq!(s.created, 0);
        }
    }
    assert!(store.is_empty());
}
