//! Real-directory tests for the enumerator: fixtures on disk, the NT path and
//! the Find fallback, cancellation, handle accounting and batch layout.
//!
//! Fixtures live in the system temp dir and are removed when the fixture drops.

use std::collections::HashSet;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;
use std::time::Duration;

use tsubame_win::enumerate::{
    ATTR_DIRECTORY, ATTR_HIDDEN, ATTR_REPARSE_POINT, Batch, CancelToken, Done, ErrorKind, Enumerator,
    Message, Method, Options, Sink,
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
            "tsubame-enum-{}-{}-{tag}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create fixture dir");
        Self { path }
    }

    fn file(&self, name: &str, len: usize) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, vec![b'x'; len]).expect("write fixture file");
        path
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::create_dir_all(&path).expect("create fixture sub-dir");
        path
    }

    /// Create a directory link (symlink, else a junction). Returns false when the
    /// process may not create one, in which case the caller skips its checks.
    fn link(&self, name: &str, target: &Path) -> bool {
        let link = self.path.join(name);
        if std::os::windows::fs::symlink_dir(target, &link).is_ok() {
            return true;
        }
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(target)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status();
        matches!(status, Ok(s) if s.success()) && link.exists()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------- helpers

fn drain(rx: Receiver<Message>, limit: Duration) -> (Vec<Batch>, Done) {
    let mut batches = Vec::new();
    loop {
        match rx
            .recv_timeout(limit)
            .unwrap_or_else(|e| panic!("no Done within {limit:?}: {e}"))
        {
            Message::Batch(b) => batches.push(b),
            Message::Done(d) => return (batches, d),
        }
    }
}

fn enumerate_with(dir: &Path, method: Method) -> (Vec<Batch>, Done, usize) {
    let token = CancelToken::new();
    let generation = token.next_generation();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let (tx, rx) = channel();
    let enumerator = Enumerator::start_with(
        dir,
        generation,
        token,
        tx,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }),
        Options { method },
    );
    let (batches, done) = drain(rx, Duration::from_secs(60));
    enumerator.join();
    (batches, done, wakes.load(Ordering::Relaxed))
}

fn names(batches: &[Batch]) -> Vec<String> {
    batches
        .iter()
        .flat_map(|b| b.iter().map(|(name, _)| String::from_utf16_lossy(name)))
        .collect()
}

fn name_set(batches: &[Batch]) -> HashSet<String> {
    names(batches).into_iter().collect()
}

fn looks_like_dir(attrs: u32) -> bool {
    attrs & ATTR_DIRECTORY != 0
}

/// Every record's offset/length pair must tile the batch's name buffer exactly.
fn assert_batch_layout(batches: &[Batch], generation: u64) {
    for batch in batches {
        assert_eq!(batch.generation, generation);
        assert!(!batch.is_empty());
        assert!(!batch.names.is_empty());
        let mut running = 0usize;
        for rec in &batch.recs {
            assert_eq!(
                rec.name_off as usize, running,
                "name offsets must be contiguous within a batch"
            );
            assert!(rec.name_len > 0);
            running += rec.name_len as usize;
        }
        assert_eq!(
            running,
            batch.names.len(),
            "record spans must cover the whole name buffer"
        );
        assert!(
            !batch.names.contains(&0),
            "names must not carry NUL terminators"
        );
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn empty_folder_is_zero_entries() {
    let fixture = Fixture::new("empty");
    let (batches, done, wakes) = enumerate_with(&fixture.path, Method::Auto);
    assert!(batches.is_empty());
    assert_eq!(done.total, 0);
    assert!(done.error.is_none(), "{:?}", done.error);
    assert!(!done.cancelled);
    assert!(!done.find_fallback, "NT path should have handled a local dir");
    assert_eq!(wakes, 1, "one wake for the Done message");
}

#[test]
fn file_counts_and_sizes() {
    for count in [0usize, 1, 10, 5_000] {
        let fixture = Fixture::new(&format!("count{count}"));
        for i in 0..count {
            fixture.file(&format!("file-{i:05}.txt"), i % 7);
        }
        let (batches, done, _) = enumerate_with(&fixture.path, Method::Auto);
        assert_eq!(done.total, count, "{count} files listed");
        assert!(done.error.is_none(), "{:?}", done.error);
        let set = name_set(&batches);
        assert_eq!(set.len(), count);
        for i in 0..count {
            assert!(set.contains(&format!("file-{i:05}.txt")));
        }
        // Sizes come through EndOfFile, per file.
        for batch in &batches {
            for (name, rec) in batch.iter() {
                let name = String::from_utf16_lossy(name);
                let index: usize = name[5..10].parse().unwrap();
                assert_eq!(rec.size, (index % 7) as u64, "{name} size");
                assert!(!rec.is_dir(), "{name} is a file");
            }
        }
        if count >= 256 {
            assert_eq!(batches[0].len(), 256, "first batch is capped at 256");
            assert!(batches.len() >= 2, "5,000 entries need several batches");
            assert_batch_layout(&batches, done.generation);
        }
    }
}

#[test]
fn unicode_long_names_and_subdirs() {
    let fixture = Fixture::new("unicode");
    let unicode = [
        "日本語-test-テスト.txt",
        "😀 emoji 🐦.log",
        "héllo wörld.txt",
        "Ω≈ç√∫.bin",
    ];
    for name in unicode {
        fixture.file(name, 12);
    }
    // 240 plus ".txt" is 244 units: over the classic MAX_PATH when prefixed by
    // the temp path, so this only works through `\\?\`.
    let long = format!("{}.txt", "l".repeat(240));
    fixture.file(&long, 3);
    fixture.dir("sub-dir");
    fixture.dir("另一個文件夾");

    let (batches, done, _) = enumerate_with(&fixture.path, Method::Auto);
    assert_eq!(done.total, unicode.len() + 3);
    let set = name_set(&batches);
    for name in unicode {
        assert!(set.contains(name), "missing {name:?}");
    }
    assert!(set.contains(&long), "missing the 244-unit name");
    assert!(set.contains("sub-dir"));
    assert!(set.contains("另一個文件夾"));

    for batch in &batches {
        for (name, rec) in batch.iter() {
            let name = String::from_utf16_lossy(name);
            let should_be_dir = name == "sub-dir" || name == "另一個文件夾";
            assert_eq!(looks_like_dir(rec.attrs), should_be_dir, "{name}");
            assert!(!rec.is_reparse_point(), "{name}");
        }
    }
}

#[test]
fn hidden_and_reparse_points() {
    let fixture = Fixture::new("attrs");
    let hidden = fixture.file("hidden.txt", 5);
    assert!(set_hidden(&hidden), "could not set the hidden attribute");

    let sub = fixture.dir("target-dir");
    let linked = fixture.link("junction", &sub);

    let (batches, done, _) = enumerate_with(&fixture.path, Method::Auto);
    assert!(done.error.is_none(), "{:?}", done.error);
    let mut saw_hidden = false;
    let mut saw_link = false;
    for batch in &batches {
        for (name, rec) in batch.iter() {
            let name = String::from_utf16_lossy(name);
            match name.as_str() {
                "hidden.txt" => {
                    saw_hidden = true;
                    assert_ne!(rec.attrs & ATTR_HIDDEN, 0, "hidden bit survives");
                }
                "junction" => {
                    saw_link = true;
                    assert_ne!(rec.attrs & ATTR_REPARSE_POINT, 0, "reparse bit");
                    assert!(rec.is_reparse_point());
                }
                "target-dir" => assert!(looks_like_dir(rec.attrs)),
                _ => {}
            }
        }
    }
    assert!(saw_hidden, "hidden file missing");
    if linked {
        assert!(saw_link, "junction missing");
    } else {
        eprintln!("skipped junction checks: no symlink/junction privilege");
    }
}

/// `attrib +H <path>`: setting FILE_ATTRIBUTE_HIDDEN without the `windows` crate
/// in the test target.
fn set_hidden(path: &Path) -> bool {
    matches!(
        std::process::Command::new("cmd")
            .args(["/C", "attrib", "+H"])
            .arg(path)
            .creation_flags(0x0800_0000)
            .status(),
        Ok(status) if status.success()
    )
}

#[test]
fn nonexistent_path_is_not_found() {
    let fixture = Fixture::new("missing");
    let missing = fixture.path.join("does-not-exist");
    for method in [Method::Auto, Method::Find] {
        let (batches, done, _) = enumerate_with(&missing, method);
        assert!(batches.is_empty());
        let error = done.error.expect("typed error for a missing directory");
        assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
        assert_eq!(done.total, 0);
    }
}

#[test]
fn file_path_is_not_a_directory() {
    let fixture = Fixture::new("notadir");
    let file = fixture.file("plain.txt", 4);
    for method in [Method::Auto, Method::Find] {
        let (batches, done, _) = enumerate_with(&file, method);
        assert!(batches.is_empty());
        let error = done.error.expect("typed error for a file");
        assert_eq!(error.kind(), ErrorKind::NotADirectory, "{error}");
    }
}

#[test]
fn cancel_delivers_no_stale_batches() {
    let fixture = Fixture::new("cancel");
    for i in 0..5_000 {
        fixture.file(&format!("f{i:05}.bin"), 1);
    }

    // A gated sink: it forwards the first batch and then blocks on `release`
    // until the test thread has issued the cancel, eliminating timing races on
    // contended CI runners. `cancel_seen` is checked *before* forwarding so the
    // pre-cancel batch itself cannot race with the test setting `cancel_seen`.
    struct SlowSink {
        tx: Sender<Message>,
        release: Option<Receiver<()>>,
        cancel_seen: Arc<AtomicBool>,
        batches_after_cancel: Arc<AtomicUsize>,
    }

    impl Sink for SlowSink {
        fn send(&mut self, msg: Message) -> bool {
            let is_batch = matches!(msg, Message::Batch(_));
            if is_batch && self.cancel_seen.load(Ordering::SeqCst) {
                self.batches_after_cancel.fetch_add(1, Ordering::SeqCst);
            }
            let delivered = self.tx.send(msg).is_ok();
            if is_batch && let Some(gate) = self.release.take() {
                let _ = gate.recv();
            }
            delivered
        }
    }

    let token = CancelToken::new();
    let generation = token.next_generation();
    let (tx, rx) = channel();
    let (gate_tx, gate_rx) = channel();
    let cancel_seen = Arc::new(AtomicBool::new(false));
    let batches_after_cancel = Arc::new(AtomicUsize::new(0));
    let enumerator = Enumerator::start_with(
        &fixture.path,
        generation,
        token.clone(),
        SlowSink {
            tx,
            release: Some(gate_rx),
            cancel_seen: Arc::clone(&cancel_seen),
            batches_after_cancel: Arc::clone(&batches_after_cancel),
        },
        Arc::new(|| {}),
        Options::default(),
    );

    let first = match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Message::Batch(b)) => b,
        Ok(other) => panic!("expected a batch first, got {other:?}"),
        Err(e) => panic!("no first batch: {e}"),
    };
    assert_eq!(first.len(), 256);
    // The worker is now parked inside SlowSink::send() on the first batch and
    // cannot enumerate or send another batch until we drop `gate_tx`.
    cancel_seen.store(true, Ordering::SeqCst);
    token.cancel();
    drop(gate_tx);

    let mut entries = first.len();
    let mut cancelled_batches = 0usize;
    let done = loop {
        match rx.recv_timeout(Duration::from_secs(60)).expect("Done after cancel") {
            Message::Batch(b) => {
                cancelled_batches += 1;
                entries += b.len();
            }
            Message::Done(d) => break d,
        }
    };
    enumerator.join();

    assert!(done.cancelled, "cancelled request must say so");
    assert!(done.error.is_none(), "{:?}", done.error);
    assert_eq!(done.total, entries, "total counts what was delivered");
    assert!(
        batches_after_cancel.load(Ordering::SeqCst) == 0,
        "no batch may be sent after the cancel was visible"
    );
    assert_eq!(cancelled_batches, 0, "only Done may follow the cancel");
    assert!(
        entries < 5_000,
        "cancel must cut the listing short, delivered {entries}"
    );
}

#[test]
fn nt_and_find_agree() {
    let fixture = Fixture::new("agree");
    for i in 0..40 {
        fixture.file(&format!("mixed-{i:02}.dat"), i * 13);
    }
    fixture.dir("child");
    fixture.file("ünïcode.txt", 7);
    let hidden = fixture.file("hidden.bin", 2);
    let _ = set_hidden(&hidden);

    let (nt_batches, nt_done, _) = enumerate_with(&fixture.path, Method::Nt);
    let (find_batches, find_done, _) = enumerate_with(&fixture.path, Method::Find);
    assert!(nt_done.error.is_none(), "NT: {:?}", nt_done.error);
    assert!(find_done.error.is_none(), "Find: {:?}", find_done.error);
    assert!(!nt_done.find_fallback);
    assert!(find_done.find_fallback);

    let mut nt: Vec<(String, u32, u64)> = nt_batches
        .iter()
        .flat_map(|b| b.iter().map(|(n, r)| (String::from_utf16_lossy(n), r.attrs, r.size)))
        .collect();
    let mut find: Vec<(String, u32, u64)> = find_batches
        .iter()
        .flat_map(|b| b.iter().map(|(n, r)| (String::from_utf16_lossy(n), r.attrs, r.size)))
        .collect();
    nt.sort();
    find.sort();
    assert_eq!(nt.len(), 43);
    assert_eq!(nt, find, "names, attributes and sizes must match");

    // Only the NT method has ids; the fallback reports none at all.
    for batch in &nt_batches {
        for (name, rec) in batch.iter() {
            assert_ne!(rec.file_id, 0, "{:?} should have a file id", String::from_utf16_lossy(name));
        }
    }
    for batch in &find_batches {
        for rec in &batch.recs {
            assert_eq!(rec.file_id, 0);
            assert_eq!(rec.alloc_size, 0);
        }
    }
}

#[test]
fn handle_count_returns_to_start() {
    // Run the measurement in a child process: other tests in this binary spawn
    // `cmd` children and touch thousands of files, and every transient handle
    // they hold would pollute a count taken in the same process.
    let exe = std::env::current_exe().expect("test exe path");
    let output = std::process::Command::new(exe)
        .args(["--exact", "handle_count_worker", "--ignored", "--nocapture"])
        .output()
        .expect("run the isolated handle test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated handle test failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("handles stable"), "unexpected output:\n{stdout}");
}

/// The real handle check; only run by [`handle_count_returns_to_start`] in a
/// fresh process, so the count cannot see another test's handles.
#[test]
#[ignore = "run by handle_count_returns_to_start in a child process"]
fn handle_count_worker() {
    let fixture = Fixture::new("handles");
    for i in 0..8 {
        fixture.file(&format!("h{i}.txt"), 2);
    }
    // One enumeration first, so any one-off process state is already in place.
    let (_, done, _) = enumerate_with(&fixture.path, Method::Auto);
    assert_eq!(done.total, 8);

    let cycle = |i: usize| {
        let token = CancelToken::new();
        let generation = token.next_generation();
        let (tx, rx) = channel();
        let enumerator = Enumerator::start(&fixture.path, generation, token, tx, Arc::new(|| {}));
        if i.is_multiple_of(2) {
            // Run to completion: the directory handle is opened and closed.
            let (_, done) = drain(rx, Duration::from_secs(60));
            assert_eq!(done.total, 8);
        }
        enumerator.join();
    };

    // Windows (and the first few `GetProcessHandleCount`/thread spawns) allocates
    // a couple of process handles lazily; prove the count has settled over 100-
    // cycle rounds before measuring, so a real leak cannot hide behind that.
    let mut baseline = tsubame_win::procinfo::counters().handles;
    let mut settled = false;
    for round in 0..6 {
        for i in 0..100 {
            cycle(i);
        }
        let now = tsubame_win::procinfo::counters().handles;
        println!("round {round}: {now} handles (was {baseline})");
        if now == baseline {
            settled = true;
            break;
        }
        baseline = now;
    }
    assert!(settled, "handle count never settled; suspect a leak");

    let before = baseline;
    for i in 0..500 {
        cycle(i);
        if i % 100 == 99 {
            let now = tsubame_win::procinfo::counters().handles;
            assert_eq!(now, before, "handle count moved at cycle {}", i + 1);
        }
    }
    let after = tsubame_win::procinfo::counters().handles;
    assert_eq!(
        after, before,
        "500 start/join cycles must not leak handles ({before} -> {after})"
    );
    println!("handles stable at {before} after 500 cycles");
}

#[test]
fn dropped_enumerator_cancels_and_closes() {
    let fixture = Fixture::new("drop");
    for i in 0..20 {
        fixture.file(&format!("d{i:02}.txt"), 1);
    }
    let token = CancelToken::new();
    let generation = token.next_generation();
    let (tx, rx) = channel();
    let enumerator = Enumerator::start(&fixture.path, generation, token, tx, Arc::new(|| {}));
    drop(enumerator); // no join: the worker must finish on its own

    // Whatever it managed to send, a Done must arrive: the worker exited, so the
    // directory handle is closed.
    let mut saw_done = None;
    while saw_done.is_none() {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Message::Batch(_)) => {}
            Ok(Message::Done(d)) => saw_done = Some(d),
            Err(e) => panic!("worker did not report Done after the drop: {e}"),
        }
    }
    let done = saw_done.unwrap();
    assert_eq!(done.generation, generation);
}

#[test]
fn batch_generation_matches_request() {
    let fixture = Fixture::new("generation");
    for i in 0..300 {
        fixture.file(&format!("g{i:03}.txt"), 0);
    }
    let token = CancelToken::new();
    let generation = token.next_generation();
    let (tx, rx) = channel();
    let enumerator = Enumerator::start(&fixture.path, generation, token, tx, Arc::new(|| {}));
    let (batches, done) = drain(rx, Duration::from_secs(60));
    enumerator.join();
    assert_eq!(done.generation, generation);
    for batch in &batches {
        assert_eq!(batch.generation, generation);
    }
    assert_eq!(done.total, 300);
    assert_batch_layout(&batches, generation);
}
