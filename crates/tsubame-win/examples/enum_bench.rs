//! Headless timing for the real enumerator: time to first batch and to Done.
//!
//! ```text
//! enum_bench <dir> [--runs N] [--method auto|nt|find]
//! ```
//!
//! `first batch` is measured in the caller from just before `Enumerator::start`
//! until the first `Batch` is received (the app can paint then); `done` until the
//! `Done` message. Both include thread spawn, so they are honest end-to-end
//! numbers for the UI. For comparison, the S2 bench measured the raw method at
//! 0.4 ms / 35 ms warm and 12.6 ms / 175 ms cold for 100,000 entries on NVMe
//! (`docs/spike-enum.md`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tsubame_win::enumerate::{CancelToken, Enumerator, Message, Method, Options};

fn main() {
    let mut dir: Option<PathBuf> = None;
    let mut runs = 5usize;
    let mut method = Method::Auto;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => {
                runs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage("--runs needs a number"));
            }
            "--method" => {
                method = match args.next().as_deref() {
                    Some("auto") => Method::Auto,
                    Some("nt") => Method::Nt,
                    Some("find") => Method::Find,
                    _ => usage("--method auto|nt|find"),
                };
            }
            other if dir.is_none() => dir = Some(PathBuf::from(other)),
            other => usage(&format!("unexpected argument {other:?}")),
        }
    }
    let dir = dir.unwrap_or_else(|| usage("missing <dir>"));

    println!(
        "enum_bench: {} runs, method {:?}, dir {}",
        runs,
        method,
        dir.display()
    );
    let mut firsts = Vec::with_capacity(runs);
    let mut dones = Vec::with_capacity(runs);
    for run in 1..=runs {
        let token = CancelToken::new();
        let generation = token.next_generation();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let wake = Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let (tx, rx) = channel();
        let started = Instant::now();
        let enumerator = Enumerator::start_with(
            &dir,
            generation,
            token,
            tx,
            wake,
            Options { method },
        );
        let mut first_ms = None;
        let mut entries = 0usize;
        let mut batches = 0usize;
        let done = loop {
            match rx.recv_timeout(Duration::from_secs(120)) {
                Ok(Message::Batch(batch)) => {
                    if first_ms.is_none() {
                        first_ms = Some(started.elapsed());
                    }
                    entries += batch.len();
                    batches += 1;
                }
                Ok(Message::Done(done)) => break done,
                Err(e) => {
                    eprintln!("enum_bench: no message for 120 s: {e}");
                    std::process::exit(1);
                }
            }
        };
        let total = started.elapsed();
        enumerator.join();
        let first_ms = first_ms.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0);
        firsts.push(first_ms);
        dones.push(total.as_secs_f64() * 1000.0);
        println!(
            "run {run}: first batch {first_ms:8.3} ms, done {:8.3} ms, {entries} entries in {batches} batches, wakes {}, worker {:.3} ms, find-fallback {}, error {}",
            total.as_secs_f64() * 1000.0,
            wakes.load(Ordering::Relaxed),
            done.elapsed.as_secs_f64() * 1000.0,
            if done.find_fallback { "yes" } else { "no" },
            match done.error {
                Some(e) => e.to_string(),
                None if done.cancelled => "cancelled".to_string(),
                None => "none".to_string(),
            }
        );
    }
    println!(
        "median: first batch {:.3} ms, done {:.3} ms (of {runs} runs)",
        median(&firsts),
        median(&dones)
    );
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    match sorted.len() {
        0 => 0.0,
        n if n % 2 == 1 => sorted[n / 2],
        n => (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0,
    }
}

fn usage(message: &str) -> ! {
    eprintln!("enum_bench: {message}");
    eprintln!("usage: enum_bench <dir> [--runs N] [--method auto|nt|find]");
    std::process::exit(2);
}
