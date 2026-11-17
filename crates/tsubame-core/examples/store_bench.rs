use std::time::Instant;
use tsubame_core::*;
fn main() {
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
fn s_push(s: &mut EntryStore, b: &[EntryInput<'_>]) {
    s.push_batch(0, b).unwrap();
}
