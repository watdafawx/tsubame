use std::time::Instant;
use tsubame_core::*;
fn main() {
    for n in [100_000, 1_000_000] {
        streamed(n);
        skewed(n);
    }
    for n in [100_000, 1_000_000] {
        let t = Instant::now();
        let mut s = EntryStore::with_capacity(n + 256, (n + 256) * 11);
        // Mixed stems, numeric runs, extensions, shuffled arrival; fixture construction included in build timing.
        let names: Vec<Vec<u16>> = (0..1024)
            .map(|i| {
                format!("{:03x}{}.{:02}", i % 251, i, i % 17)
                    .encode_utf16()
                    .collect()
            })
            .collect();
        for chunk in (0..n).step_by(n) {
            let batch: Vec<_> = (chunk..(chunk + n).min(n))
                .map(|i| EntryInput {
                    name: &names[(i * 7919) % 1024],
                    size: (i * 7919 % n) as u64,
                    modified: (i * 3571 % n) as i64,
                    flags: if i % 100 == 0 { FOLDER } else { 0 },
                })
                .collect();
            // One bulk build avoids measuring repeated merge as construction.
            s_push(&mut s, &batch);
        }
        println!("{n} build {:?} memory {}", t.elapsed(), s.memory_bytes());
        for column in [Column::Name, Column::Size, Column::Modified, Column::Type] {
            let t = Instant::now();
            s.sort(Sort {
                column,
                descending: true,
                folders_first: false,
            });
            println!("{n} {column:?} {:?}", t.elapsed());
        }
        let t = Instant::now();
        s.filter("00.", FilterMode::Substring);
        println!("{n} filter {:?}", t.elapsed());
        let t = Instant::now();
        s.filter("00.0", FilterMode::Substring);
        println!("{n} refine {:?}", t.elapsed());
        let t = Instant::now();
        s.filter("*00.?", FilterMode::Glob);
        println!("{n} glob {:?}", t.elapsed());
        s.filter("", FilterMode::Substring);
        s.sort(Sort::default());
        let b = vec![
            EntryInput {
                name: &names[0],
                size: 0,
                modified: 0,
                flags: 0
            };
            256
        ];
        let t = Instant::now();
        s.push_batch(0, &b).unwrap();
        println!("{n} merge256 {:?}", t.elapsed());
    }
}
/// A listing as the app gets it: unique mixed names in 256 + 4,096-entry
/// batches. Old path: merge every batch into the sorted view. New path:
/// streaming (sorted top only) and one sort at the end.
fn streamed(n: usize) {
    let names: Vec<Vec<u16>> = (0..n)
        .map(|i| {
            let j = (i * 7919) % n;
            format!("{}file {}-{:x}.{}", ["IMG_", "", "Report ", "a", "Z"][j % 5], j % 1000, j, ["txt", "jpg", "rs", "md"][j % 4])
                .encode_utf16()
                .collect()
        })
        .collect();
    let inputs: Vec<_> = names.iter().enumerate().map(|(i, n)| EntryInput { name: n, size: i as u64, modified: i as i64, flags: 0 }).collect();
    let mut batches = vec![&inputs[..256.min(n)]];
    let mut at = 256.min(n);
    while at < n {
        let k = 4096.min(n - at);
        batches.push(&inputs[at..at + k]);
        at += k;
    }
    for stream in [false, true] {
        let mut s = EntryStore::new();
        let t = Instant::now();
        if stream {
            s.begin_stream();
        }
        let mut first = None;
        for b in &batches {
            s.push_batch(0, b).unwrap();
            first.get_or_insert(t.elapsed());
        }
        let pushed = t.elapsed();
        s.end_stream();
        println!(
            "{n} {} first batch {:?} all batches {:?} total sorted {:?}",
            if stream { "streaming+end sort" } else { "merge per batch" },
            first.unwrap(),
            pushed,
            t.elapsed()
        );
    }
}
/// Camera folder: every name shares one prefix key ("img_" + a digit run),
/// the worst case for the sample sort's buckets.
fn skewed(n: usize) {
    let names: Vec<Vec<u16>> = (0..n).map(|i| format!("IMG_{:07}.JPG", (i * 7919) % n).encode_utf16().collect()).collect();
    let inputs: Vec<_> = names.iter().enumerate().map(|(i, n)| EntryInput { name: n, size: i as u64, modified: i as i64, flags: 0 }).collect();
    let mut s = EntryStore::new();
    s.begin_stream();
    s.push_batch(0, &inputs).unwrap();
    let t = Instant::now();
    s.end_stream();
    print!("{n} camera names: end sort {:?}", t.elapsed());
    for column in [Column::Name, Column::Type, Column::Size] {
        let t = Instant::now();
        s.sort(Sort { column, descending: false, folders_first: true });
        print!("  {column:?} {:?}", t.elapsed());
    }
    println!();
}
fn s_push(s: &mut EntryStore, b: &[EntryInput<'_>]) {
    s.push_batch(0, b).unwrap();
}
