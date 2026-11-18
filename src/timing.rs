//! `--timing` mode: navigates a list of folders (`--timing-dirs "A;B"`,
//! default the start folder) `--reps N` times each and prints, per
//! navigation, the times measured from the navigation event:
//!
//! - first_batch: first batch of entries moved into the store
//! - first_frame: first presented frame that shows rows
//! - done:        Done message handled (store complete and sorted)
//! - final_frame: first presented frame after Done (complete + sorted on screen)
//! - worker:      time inside the enumerator worker (not UI)
//!
//! - store_cpu:   UI time moving batches into the store
//! - final_sort:  UI time of the one full sort at Done (streaming store)
//! - watch_start: UI time starting the folder watcher
//! - frames:      frames presented up to the final one
//!
//! The very first navigation also includes window creation and the first GPU
//! frame (cold start); it is labelled `startup`.
//!
//! A `<purge>` entry in `--timing-dirs` is no folder: it runs
//! `tsubame-bench.exe purge --mode working` (next to the exe; the app must
//! run elevated; `TSUBAME_TIMING_PURGE` picks another mode) before the next
//! navigation, so `tiny;<purge>;tiny;big`
//! measures a cold folder in a warm app: the second `tiny` re-faults the
//! app's own pages (paint path, enumerator, store) that the purge dropped,
//! and `big` is then the only cold thing. Its result is printed as `purge:`.
//!
//! An `<idle>` entry waits 3 s before the next navigation: after a purge
//! made from outside and a fresh start, `tiny;<idle>;tiny;big` opens a cold
//! folder in an app whose startup is over (the cold probe uses this).
//!
//! An `<ops>` entry is no folder either: on the folder listed last it times
//! the UI work of a sort switch per column (Size, Modified, Type, Name, Name
//! descending, Name) and of filter-as-you-type (the first characters of a
//! name from the middle of the list typed one at a time, then erased one at
//! a time). Printed as `ops:` lines; paint is not included (the scroll
//! paint gate bounds one frame).

use crate::tab::Tab;
use crate::view::View;
use std::fmt::Write;
use std::time::{Duration, Instant};

/// `--timing-dirs` entry that purges the system caches instead of navigating.
const PURGE: &str = "<purge>";
/// Purge mode: `TSUBAME_TIMING_PURGE` (`working` by default, the only mode
/// that makes NTFS metadata cold on Windows 11; it also empties every
/// working set, this app's too, and starts a paging storm on the whole box).
fn purge_mode() -> String {
    std::env::var("TSUBAME_TIMING_PURGE").unwrap_or_else(|_| "working".into())
}
/// Wait after a purge: a working-set purge needs its paging storm to pass.
fn purge_settle(mode: &str) -> Duration {
    Duration::from_secs(if mode == "working" { 15 } else { 2 })
}
/// `--timing-dirs` entry that times sort switches and filter keystrokes.
const OPS: &str = "<ops>";
/// `--timing-dirs` entry that idles before the next navigation, so the
/// startup tail (GPU paint path, first paints) is over when it starts.
const IDLE: &str = "<idle>";
const IDLE_FOR: Duration = Duration::from_secs(3);

/// Runs the bench tool's purge next to the exe and waits for it (bench only).
fn purge() -> String {
    use std::os::windows::process::CommandExt;
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else {
        return "failed: no exe dir".into();
    };
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new(dir.join("tsubame-bench.exe")).args(["purge", "--mode", &purge_mode()]).creation_flags(CREATE_NO_WINDOW).output() {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            let first = text.lines().next().unwrap_or("").trim().to_string();
            if o.status.success() && first.contains(" ok") { first } else { format!("failed ({}): {first}", o.status) }
        }
        Err(e) => format!("failed: {e}"),
    }
}

pub struct Run {
    dirs: Vec<String>,
    reps: usize,
    n: usize,
    out: String,
    pub first_paint_ms: f64,
    private_first: usize,
}

/// Times sort switches and filter keystrokes on the loaded tab (UI work).
fn ops(out: &mut String, t: &mut Tab) {
    use tsubame_core::Column::*;
    let time = |f: &mut dyn FnMut()| {
        let t0 = Instant::now();
        f();
        t0.elapsed().as_secs_f64() * 1000.0
    };
    let mut line = format!("ops: entries {:>7}  sort", t.store.len());
    for (label, col) in [("size", Size), ("modified", Modified), ("type", Type), ("name", Name), ("name_desc", Name), ("name_asc", Name)] {
        let _ = write!(line, "  {label} {:.2}", time(&mut || t.sort_by(col)));
    }
    let _ = writeln!(out, "{line}");
    let mid = t.store.at(t.shown() / 2);
    let word: Vec<char> = mid.and_then(|id| t.store.name(id)).map(|n| String::from_utf16_lossy(n).chars().take(5).collect()).unwrap_or_default();
    let mut line = format!("ops: filter {:?}", word.iter().collect::<String>());
    let mut typed = String::new();
    for c in &word {
        typed.push(*c);
        let ms = time(&mut || t.set_filter(&typed));
        let _ = write!(line, "  +{} {:.2} ({})", typed.chars().count(), ms, t.shown());
    }
    while typed.pop().is_some() {
        let ms = time(&mut || t.set_filter(&typed));
        let _ = write!(line, "  -{} {:.2} ({})", typed.chars().count(), ms, t.shown());
    }
    let _ = writeln!(out, "{line}");
}

fn ms(d: Option<Duration>) -> String {
    d.map(|d| format!("{:8.2}", d.as_secs_f64() * 1000.0)).unwrap_or_else(|| "       -".into())
}

impl Run {
    pub fn new(dirs: Vec<String>, reps: usize) -> Run {
        Run { dirs, reps: reps.max(1), n: 0, out: String::new(), first_paint_ms: 0.0, private_first: 0 }
    }

    pub fn first_paint(&mut self, ms_since_start: f64, private: usize) {
        self.first_paint_ms = ms_since_start;
        self.private_first = private;
    }

    fn line(&mut self, label: &str, t: &Tab) {
        let tm = t.timing;
        let _ = writeln!(
            self.out,
            "{label:<9} entries {:>7} ({:.1} MB)  first_batch {}  first_frame {}  done {}  final_frame {}  worker {}  store_cpu {}  final_sort {}  watch_start {}  frames {:>3}   {}",
            t.store.len(),
            t.memory_bytes() as f64 / 1048576.0,
            ms(tm.first_batch),
            ms(tm.first_frame),
            ms(tm.done),
            ms(tm.final_frame),
            ms(tm.worker),
            ms(Some(tm.store_cpu)),
            ms(Some(tm.final_sort)),
            ms(Some(tm.watch_start)),
            tm.frames,
            t.path
        );
    }

    /// The final frame of the current navigation was presented. Records it
    /// and starts the next navigation; true when the run is over.
    pub fn next(&mut self, v: &mut View) -> bool {
        let i = self.n;
        let rep = i % self.reps;
        let label = if i == 0 {
            "startup".to_string()
        } else if rep == 0 {
            "first".to_string()
        } else {
            format!("repeat {rep}")
        };
        self.line(&label, v.tab());
        self.n += 1;
        if self.n >= self.dirs.len() * self.reps {
            return true;
        }
        let mut next = self.dirs[self.n / self.reps].clone();
        while next == PURGE || next == OPS || next == IDLE {
            if next == IDLE {
                std::thread::sleep(IDLE_FOR);
            } else if next == OPS {
                for _ in 0..self.reps {
                    ops(&mut self.out, v.tab_mut());
                }
            } else {
                let note = purge();
                std::thread::sleep(purge_settle(&purge_mode()));
                let _ = writeln!(self.out, "purge: {note}");
            }
            self.n += self.reps;
            if self.n >= self.dirs.len() * self.reps {
                return true;
            }
            next = self.dirs[self.n / self.reps].clone();
        }
        v.go(&next);
        false
    }

    pub fn report(&self, window_px: (i32, i32), dpi: f32, path: &str, private_end: usize) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "tsubame U2 timing ({})", if cfg!(debug_assertions) { "debug" } else { "release" });
        let _ = writeln!(s, "window_px: {}x{} at {:.0} dpi, paint path {path}", window_px.0, window_px.1, dpi);
        let _ = writeln!(s, "first_paint_ms (process start to first completed paint): {:.1}", self.first_paint_ms);
        let _ = writeln!(s, "private_bytes_first_paint: {:.1} MB", self.private_first as f64 / 1048576.0);
        let _ = writeln!(s, "all times in ms from the navigation event");
        s.push_str(&self.out);
        let _ = writeln!(s, "private_bytes_end: {:.1} MB", private_end as f64 / 1048576.0);
        s
    }
}
