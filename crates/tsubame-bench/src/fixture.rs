//! Fixture generator: folders of realistic mixed names, deterministic seed,
//! resumable, parallel.
//!
//! Names are derived from `(seed, index)` alone, so thread count and creation
//! order never change the result and a half-finished folder resumes into the
//! same set. Existing files are counted and skipped, never overwritten.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use core::ffi::c_void;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
    FindNextFileW, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

use crate::util::{extended, search_pattern_wide};

const MARKER: &str = ".tsubame-fixture";

pub struct GenSpec {
    pub root: String,
    pub sizes: Vec<(String, usize)>,
    pub seed: u64,
    pub threads: usize,
    /// 0 = empty files; >0 writes that many bytes of filler.
    pub tiny_bytes: usize,
    pub force: bool,
    pub verify: bool,
}

/// SplitMix64, seeded per index so names do not depend on thread order.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn mix(seed: u64, index: usize) -> u64 {
    seed ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA5A5_5A5A_1234_5678
}

const STEMS: &[&str] = &[
    "report",
    "invoice",
    "IMG",
    "notes",
    "screenshot",
    "main",
    "lib",
    "vendor",
    "backup",
    "project",
    "budget",
    "README",
    "index",
    "config",
    "photo",
    "video",
    "archive",
    "chart",
    "design",
    "mockup",
    "final",
    "draft",
    "receipt",
    "scan",
    "music",
    "podcast",
    "episode",
    "dataset",
    "model",
    "weights",
    "build",
    "release",
    "debug",
    "Meeting-Minutes",
    "annual-summary",
    "客户名单",
    "会议记录",
    "数据",
    "テスト",
    "Übersicht",
    "фотографии",
    "résumé",
    "naïve",
    "café",
    "quarterly",
    "latency-trace",
    "kernel-dump",
];

const EXTS: &[&str] = &[
    ".txt", ".rs", ".png", ".jpg", ".json", ".toml", ".log", ".dll", ".exe", ".pdf", ".zip", ".mp4",
    ".md", ".csv", ".bin", "", "", "",
];

const SEPS: &[&str] = &["-", "_", "", " "];

/// Deterministic name for entry `index`.
pub fn name_for(seed: u64, index: usize) -> String {
    let mut r = Rng::new(mix(seed, index));
    let stem = STEMS[r.below(STEMS.len())];
    let sep = SEPS[r.below(SEPS.len())];
    let mut name = String::with_capacity(48);
    name.push_str(stem);
    if r.below(100) < 35 {
        name.push_str(sep);
        name.push_str(STEMS[r.below(STEMS.len())]);
    }
    name.push_str(sep);
    name.push_str(&format!("{index:06}"));
    if r.below(1000) < 4 {
        // ~0.4% long names, to exercise MAX_PATH handling.
        name.push('-');
        for k in 0..6 {
            name.push_str(STEMS[(index + k) % STEMS.len()]);
            name.push('-');
        }
    }
    name.push_str(EXTS[r.below(EXTS.len())]);
    name
}

fn is_dir_index(index: usize) -> bool {
    index.is_multiple_of(211)
}

fn nested_files(index: usize) -> usize {
    (index / 211) % 4
}

pub fn generate(spec: &GenSpec) -> Result<(), String> {
    fs::create_dir_all(&spec.root)
        .map_err(|e| format!("create_dir_all({}): {e}", spec.root))?;
    for (label, count) in &spec.sizes {
        let dir = format!("{}\\{}", spec.root.trim_end_matches('\\'), label);
        generate_one(spec, &dir, *count)?;
    }
    Ok(())
}

fn marker_line(seed: u64, count: usize, tiny: usize) -> String {
    format!("seed={seed};count={count};tiny={tiny};done=1")
}

fn generate_one(spec: &GenSpec, dir: &str, count: usize) -> Result<(), String> {
    // The marker sits next to the folder, not inside it, so the folder contains
    // exactly `count` entries.
    let marker_path = PathBuf::from(format!("{dir}.tsubame-fixture"));
    let want = marker_line(spec.seed, count, spec.tiny_bytes);
    if !spec.force && marker_path.exists()
        && let Ok(existing) = fs::read_to_string(&marker_path)
            && existing.trim() == want {
                println!("gen: {dir} already complete ({want}), skipped");
                if spec.verify {
                    let got = count_entries(dir)?;
                    println!("gen: {dir} verify: {got} entries (expected {count})");
                }
                return Ok(());
            }

    fs::create_dir_all(dir).map_err(|e| format!("create_dir_all({dir}): {e}"))?;
    let dir_ext = extended(dir);
    println!(
        "gen: {dir}: {count} entries, seed {}, {} threads, {} byte files",
        spec.seed,
        spec.threads,
        spec.tiny_bytes
    );

    let started = Instant::now();
    let created = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let first_error = std::sync::Mutex::new(None::<String>);
    let filler = vec![0x5Au8; spec.tiny_bytes];

    std::thread::scope(|scope| {
        for t in 0..spec.threads {
            let created = &created;
            let skipped = &skipped;
            let failed = &failed;
            let first_error = &first_error;
            let dir_ext = &dir_ext;
            let filler = &filler;
            let seed = spec.seed;
            scope.spawn(move || {
                let mut local_created = 0usize;
                let mut local_skipped = 0usize;
                let mut local_failed = 0usize;
                let mut idx = t;
                while idx < count {
                    let name = name_for(seed, idx);
                    let path = format!("{dir_ext}\\{name}");
                    if is_dir_index(idx) {
                        match fs::create_dir(&path) {
                            Ok(()) => {
                                local_created += 1;
                                for k in 0..nested_files(idx) {
                                    let child = format!("{dir_ext}\\{name}\\{}", name_for(seed ^ 0x51ED, idx * 7 + k));
                                    match fs::OpenOptions::new()
                                        .write(true)
                                        .create_new(true)
                                        .open(&child)
                                    {
                                        Ok(_) => local_created += 1,
                                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                            local_skipped += 1
                                        }
                                        Err(e) => {
                                            local_failed += 1;
                                            let mut fe = first_error.lock().unwrap();
                                            if fe.is_none() {
                                                *fe = Some(format!("{child}: {e}"));
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => local_skipped += 1,
                            Err(e) => {
                                local_failed += 1;
                                let mut fe = first_error.lock().unwrap();
                                if fe.is_none() {
                                    *fe = Some(format!("{path}: {e}"));
                                }
                            }
                        }
                    } else {
                        match fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)
                        {
                            Ok(mut f) => {
                                if !filler.is_empty()
                                    && let Err(e) = f.write_all(filler) {
                                        local_failed += 1;
                                        let mut fe = first_error.lock().unwrap();
                                        if fe.is_none() {
                                            *fe = Some(format!("{path}: write: {e}"));
                                        }
                                        idx += spec.threads;
                                        continue;
                                    }
                                local_created += 1;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => local_skipped += 1,
                            Err(e) => {
                                local_failed += 1;
                                let mut fe = first_error.lock().unwrap();
                                if fe.is_none() {
                                    *fe = Some(format!("{path}: {e}"));
                                }
                            }
                        }
                    }
                    idx += spec.threads;
                }
                created.fetch_add(local_created, Ordering::Relaxed);
                skipped.fetch_add(local_skipped, Ordering::Relaxed);
                failed.fetch_add(local_failed, Ordering::Relaxed);
            });
        }
    });

    let elapsed = started.elapsed();
    let c = created.load(Ordering::Relaxed);
    let s = skipped.load(Ordering::Relaxed);
    let f = failed.load(Ordering::Relaxed);
    println!(
        "gen: {dir}: created {c}, skipped {s}, failed {f} in {:.1} s ({:.0} files/s)",
        elapsed.as_secs_f64(),
        c as f64 / elapsed.as_secs_f64().max(0.001)
    );
    if f > 0 {
        let msg = first_error.lock().unwrap().clone().unwrap_or_default();
        return Err(format!("{f} create failures, first: {msg}"));
    }
    fs::write(&marker_path, format!("{want}\n"))
        .map_err(|e| format!("write marker {}: {e}", marker_path.display()))?;

    if spec.verify {
        let got = count_entries(dir)?;
        println!("gen: {dir} verify: {got} top-level entries (expected {count})");
        if got != count {
            return Err(format!("{dir}: expected {count} entries, found {got}"));
        }
    }
    Ok(())
}

/// Count top-level entries with `FindFirstFileExW` (no store, low memory).
pub fn count_entries(dir: &str) -> Result<usize, String> {
    let pattern = search_pattern_wide(dir);
    let mut data = WIN32_FIND_DATAW::default();
    let handle: HANDLE = unsafe {
        match FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut WIN32_FIND_DATAW as *mut c_void,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        ) {
            Ok(h) => h,
            Err(e) => {
                if (e.code().0 as u32 & 0xFFFF) == 2 {
                    return Ok(0);
                }
                return Err(format!("FindFirstFileExW({dir}): {}", e.message()));
            }
        }
    };
    let mut n = 0usize;
    loop {
        // With a `\\?\` path the Win32 layer does not filter "." / "..".
        let len = data
            .cFileName
            .iter()
            .position(|&u| u == 0)
            .unwrap_or(data.cFileName.len());
        let name = &data.cFileName[..len];
        let is_dot = matches!(name, [0x2E] | [0x2E, 0x2E]);
        let is_marker =
            name.len() == MARKER.len() && name.iter().zip(MARKER.bytes()).all(|(&u, b)| u == b as u16);
        if !is_dot && !is_marker {
            n += 1;
        }
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(handle);
    }
    Ok(n)
}

/// Parse `1k,100k,1m` into `[("1k", 1000), ...]`.
pub fn parse_sizes(s: &str) -> Result<Vec<(String, usize)>, String> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let lower = p.to_ascii_lowercase();
        let (num, mult) = if let Some(n) = lower.strip_suffix('k') {
            (n, 1_000usize)
        } else if let Some(n) = lower.strip_suffix('m') {
            (n, 1_000_000usize)
        } else {
            (lower.as_str(), 1usize)
        };
        let base: usize = num
            .trim()
            .parse()
            .map_err(|_| format!("bad size {p:?} (want 1k, 100k, 1m or a plain number)"))?;
        out.push((p.to_string(), base * mult));
    }
    if out.is_empty() {
        return Err("no sizes given".to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_stable() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..5000 {
            let n = name_for(42, i);
            assert!(seen.insert(n.clone()), "duplicate name {n} at {i}");
            assert_eq!(n, name_for(42, i), "unstable name at {i}");
            assert!(n.len() < 255, "name too long: {} chars", n.len());
        }
    }

    #[test]
    fn seeds_change_names() {
        assert_ne!(name_for(1, 7), name_for(2, 7));
    }

    #[test]
    fn parses_sizes() {
        let v = parse_sizes("1k,100k,1m").unwrap();
        assert_eq!(v[0], ("1k".to_string(), 1_000));
        assert_eq!(v[1], ("100k".to_string(), 100_000));
        assert_eq!(v[2], ("1m".to_string(), 1_000_000));
        assert_eq!(parse_sizes("42").unwrap()[0].1, 42);
        assert!(parse_sizes("abc").is_err());
    }

    #[test]
    fn some_entries_are_directories_and_some_are_unicode() {
        let mut dirs = 0;
        let mut unicode = 0;
        for i in 0..2000 {
            if is_dir_index(i) {
                dirs += 1;
            }
            if name_for(7, i).chars().any(|c| !c.is_ascii()) {
                unicode += 1;
            }
        }
        assert!(dirs > 0, "no directories generated");
        assert!(unicode > 0, "no unicode names generated");
    }
}
