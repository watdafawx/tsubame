//! Numbers for `docs/watcher.md`: single-event latency and applying a 10,000-change
//! batch to a 100,000-entry store.
//!
//! `pwsh -NoProfile -File X:\Dev\tools\cargo-ccd1.ps1 run -p tsubame-win --release --example watch_bench`

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tsubame_core::{EntryInput, EntryStore, Selection, Sort};
use tsubame_win::watch::{Batch, Coalescer, DirWatcher, Message, Options, RawKind, apply_batch, apply_resolved, resolve};

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn batch_of(kind: RawKind, names: &[Vec<u16>]) -> Batch {
    let t = Instant::now();
    let mut c = Coalescer::new(Duration::from_millis(60), Duration::from_millis(250));
    for n in names {
        c.push(kind, n, t);
    }
    c.take_batch(t).expect("batch")
}

/// 100,000 entries: `base` synthetic ones plus the given real names.
fn big_store(real: &[Vec<u16>]) -> EntryStore {
    let synthetic: Vec<Vec<u16>> = (0..100_000 - real.len()).map(|i| w(&format!("synthetic_{i:06}.dat"))).collect();
    let mut s = EntryStore::new();
    s.sort(Sort { folders_first: true, ..Sort::default() });
    let all: Vec<_> = synthetic
        .iter()
        .chain(real)
        .enumerate()
        .map(|(i, n)| EntryInput { name: n, size: i as u64, modified: i as i64, flags: 0 })
        .collect();
    s.push_batch(s.generation(), &all).unwrap();
    s
}

fn main() {
    let root = std::env::temp_dir().join(format!("tsubame-watch-bench-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();

    // ---- single event latency
    {
        let dir = root.join("latency");
        fs::create_dir_all(&dir).unwrap();
        let (_w, rx) = DirWatcher::start(&dir, Options::default(), Arc::new(|| {})).unwrap();
        let mut lat = Vec::new();
        for i in 0..40 {
            std::thread::sleep(Duration::from_millis(120));
            let t0 = Instant::now();
            fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
            if let Ok(Message::Batch(_)) = rx.recv_timeout(Duration::from_secs(2)) {
                lat.push(ms(t0.elapsed()));
            }
        }
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "single-event latency, window 60 ms (n={}): min {:.1} / median {:.1} / p95 {:.1} / max {:.1} ms",
            lat.len(),
            lat[0],
            lat[lat.len() / 2],
            lat[lat.len() * 95 / 100],
            lat[lat.len() - 1]
        );
    }

    // ---- apply 10k changes to a 100k store
    let dir = root.join("apply");
    fs::create_dir_all(&dir).unwrap();
    let names: Vec<Vec<u16>> = (0..10_000).map(|i| w(&format!("real_{i:05}.txt"))).collect();
    for i in 0..10_000 {
        fs::write(dir.join(format!("real_{i:05}.txt")), vec![b'x'; i % 300]).unwrap();
    }
    for pass in 0..3 {
        // resolve (disk) in both strategies, then apply (store) alone
        let b = batch_of(RawKind::Added, &names);
        let t = Instant::now();
        let by_stat = resolve(&dir, &b, usize::MAX);
        let stat_ms = ms(t.elapsed());
        let t = Instant::now();
        let by_list = resolve(&dir, &b, 100_000);
        let list_ms = ms(t.elapsed());
        assert!(!by_stat.listed && by_list.listed);
        println!("[{pass}] resolve 10k names   : per-name stat {stat_ms:.1} ms, one listing {list_ms:.1} ms");

        let mut store = big_store(&[]);
        let mut sel = Selection::new();
        sel.select_all(&store);
        let s = apply_resolved(&mut store, Some(&mut sel), &b, &by_list).unwrap();
        println!("[{pass}] apply insert 10k into 100k : {:.1} ms; store now {}", ms(s.store), store.len());

        let mut store = big_store(&names);
        let mut sel = Selection::new();
        sel.select_all(&store);
        let b = batch_of(RawKind::Modified, &names);
        let r = resolve(&dir, &b, store.len());
        let s = apply_resolved(&mut store, Some(&mut sel), &b, &r).unwrap();
        println!("[{pass}] apply modify 10k in 100k   : {:.1} ms; {} updated (resolve {:.1} ms, listed={})", ms(s.store), s.modified, ms(r.elapsed), r.listed);

        let b = batch_of(RawKind::Removed, &names);
        let r = resolve(&dir, &b, store.len());
        let s = apply_resolved(&mut store, Some(&mut sel), &b, &r).unwrap();
        println!("[{pass}] apply remove 10k from 100k : {:.1} ms; store now {}", ms(s.store), store.len());

        // one change into a 100k store (the common case), whole path
        let mut store = big_store(&[]);
        let one = batch_of(RawKind::Added, &names[..1]);
        let t = Instant::now();
        apply_batch(&mut store, None, &dir, &one).unwrap();
        println!("[{pass}] insert 1 into 100k (resolve + apply): {:.3} ms", ms(t.elapsed()));
    }

    let _ = fs::remove_dir_all(&root);
}