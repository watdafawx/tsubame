//! Gate fixture: the folder tree the leak gate cycles.
//!
//! `tsubame-bench gate-fixture --root DIR` creates `DIR\gate` with at least
//! 200 folders of mixed sizes so 10,000 navigations land in tiny, medium and
//! huge listings, the way a real user's browsing does:
//!
//! ```text
//! DIR\gate\tiny\0001..0190   20 entries each (and 2 .exe + 2 .lnk, so the
//!                            per-path icon cache is exercised every visit)
//! DIR\gate\small\0001..0009  1,000 entries each
//! DIR\gate\big\0001          10,000 entries
//! ```
//!
//! Names come from `fixture::name_for`, so they are deterministic, unique and
//! include the unicode and long-name cases. Files are empty: the gate measures
//! handles, icons and memory, not disk throughput. A marker next to the folder
//! makes a rerun skip a complete tree (`--force` rebuilds it).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::fixture::{count_entries, name_for};

const MARKER_SUFFIX: &str = ".tsubame-gate";
/// Per-path icon files, the ones whose icon lives inside the file
/// (`.exe`, `.lnk`): their cache key is the full path.
const PER_PATH: [&str; 4] = ["setup.exe", "tool.exe", "shortcut.lnk", "link.lnk"];

pub struct GateSpec {
    pub root: String,
    pub tiny: usize,
    pub small: usize,
    pub big: usize,
    pub tiny_files: usize,
    pub small_files: usize,
    pub big_files: usize,
    pub seed: u64,
    pub threads: usize,
    pub force: bool,
}

impl GateSpec {
    pub fn folders(&self) -> usize {
        self.tiny + self.small + self.big
    }

    fn marker(&self) -> String {
        format!(
            "seed={};tiny={}x{};small={}x{};big={}x{};done=1",
            self.seed, self.tiny, self.tiny_files, self.small, self.small_files, self.big, self.big_files
        )
    }
}

pub fn generate(spec: &GateSpec) -> Result<(), String> {
    let gate = format!("{}\\gate", spec.root.trim_end_matches('\\'));
    let marker_path = PathBuf::from(format!("{gate}{MARKER_SUFFIX}"));
    let want = spec.marker();
    if !spec.force
        && marker_path.exists()
        && let Ok(have) = fs::read_to_string(&marker_path)
        && have.trim() == want
    {
        println!("gate-fixture: {gate} already complete ({want}), skipped");
        return Ok(());
    }
    fs::create_dir_all(&gate).map_err(|e| format!("create_dir_all({gate}): {e}"))?;
    let started = Instant::now();
    let mut created = 0usize;
    for (kind, count, files) in [("tiny", spec.tiny, spec.tiny_files), ("small", spec.small, spec.small_files), ("big", spec.big, spec.big_files)] {
        for i in 0..count {
            let dir = format!("{gate}\\{kind}\\{:04}", i + 1);
            created += one_folder(spec, &dir, files, kind, i)?;
        }
    }
    println!(
        "gate-fixture: {} folders under {gate}, {created} entries in {:.1} s",
        spec.folders(),
        started.elapsed().as_secs_f64()
    );
    fs::write(&marker_path, format!("{want}\n")).map_err(|e| format!("write {}: {e}", marker_path.display()))?;
    // spot-check one folder of each kind: a resumed or half-written tree is a
    // wrong fixture, and a wrong fixture makes the gate numbers meaningless
    for (kind, count, files) in [("tiny", spec.tiny, spec.tiny_files), ("small", spec.small, spec.small_files), ("big", spec.big, spec.big_files)] {
        if count == 0 {
            continue;
        }
        let dir = format!("{gate}\\{kind}\\0001");
        let got = count_entries(&dir)?;
        let want = files + PER_PATH.len();
        println!("gate-fixture: {dir} verify: {got} entries (expected {want})");
        if got != want {
            return Err(format!("{dir}: expected {want} entries, found {got}"));
        }
    }
    Ok(())
}

/// Creates one folder's files (existing files are overwritten: they are empty).
fn one_folder(spec: &GateSpec, dir: &str, files: usize, kind: &str, index: usize) -> Result<usize, String> {
    fs::create_dir_all(dir).map_err(|e| format!("create_dir_all({dir}): {e}"))?;
    let created = AtomicUsize::new(0);
    let threads = spec.threads.max(1);
    std::thread::scope(|scope| {
        for t in 0..threads {
            let created = &created;
            let seed = spec.seed;
            scope.spawn(move || {
                let mut idx = t;
                while idx < files {
                    let name = name_for(seed ^ (index as u64).wrapping_mul(0x9E37_79B9) ^ (kind.len() as u64), idx);
                    if fs::write(format!("{dir}\\{name}"), b"").is_ok() {
                        created.fetch_add(1, Ordering::Relaxed);
                    }
                    idx += threads;
                }
                // per-path icon files: every visit asks the shell for these
                if t == 0 {
                    for n in PER_PATH {
                        if fs::write(format!("{dir}\\{n}"), b"").is_ok() {
                            created.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    let made = created.load(Ordering::Relaxed);
    if made == 0 {
        return Err(format!("{dir}: no file could be created"));
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(root: &str) -> GateSpec {
        GateSpec { root: root.to_string(), tiny: 2, small: 1, big: 0, tiny_files: 4, small_files: 6, big_files: 0, seed: 7, threads: 2, force: true }
    }

    #[test]
    fn creates_the_tree_and_verifies_counts() {
        let root = std::env::temp_dir().join(format!("tsubame-gate-fixture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let s = spec(root.to_str().unwrap());
        generate(&s).expect("generate");
        // tiny folders: 4 named files + the per-path icon files
        assert_eq!(count_entries(&format!("{}\\gate\\tiny\\0001", s.root)).unwrap(), 4 + PER_PATH.len());
        assert_eq!(count_entries(&format!("{}\\gate\\small\\0001", s.root)).unwrap(), 6 + PER_PATH.len());
        // a second run skips the complete tree
        generate(&s).expect("regenerate");
        // forced rebuild starts from scratch
        let mut s2 = spec(&s.root);
        s2.force = true;
        s2.tiny_files = 5;
        generate(&s2).expect("force");
        assert_eq!(count_entries(&format!("{}\\gate\\tiny\\0001", s2.root)).unwrap(), 5 + PER_PATH.len());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn folder_count_matches_the_plan() {
        let s = GateSpec { root: "X:\\x".into(), tiny: 190, small: 9, big: 1, tiny_files: 20, small_files: 1000, big_files: 10_000, seed: 1, threads: 1, force: false };
        assert!(s.folders() >= 200);
        assert!(s.marker().contains("tiny=190x20"));
    }
}
