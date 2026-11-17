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
//! The very first navigation also includes window creation and the first GPU
//! frame (cold start); it is labelled `startup`.

use crate::tab::Tab;
use crate::view::View;
use std::fmt::Write;
use std::time::Duration;

pub struct Run {
    dirs: Vec<String>,
    reps: usize,
    n: usize,
    out: String,
    pub first_paint_ms: f64,
    private_first: usize,
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
            "{label:<9} entries {:>7} ({:.1} MB)  first_batch {}  first_frame {}  done {}  final_frame {}  worker {}   {}",
            t.store.len(),
            t.memory_bytes() as f64 / 1048576.0,
            ms(tm.first_batch),
            ms(tm.first_frame),
            ms(tm.done),
            ms(tm.final_frame),
            ms(tm.worker),
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
        let next = self.dirs[self.n / self.reps].clone();
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
