//! Built-in measurement mode (`--bench` or env `TSUBAME_BENCH=1`).
//!
//! 1. first paint: process creation time (GetProcessTimes) to the end of the
//!    first completed WM_PAINT, both on the precise system FILETIME clock.
//! 2. waits until the GPU render target has replaced the software first frame
//!    (or 3 s), then scrolls programmatically for 5 s on a ~60 Hz bench-only
//!    timer, timing every paint (std `Instant` = QueryPerformanceCounter).
//! 3. writes the report to stdout (parent console, if any) and `s1-bench.txt`
//!    next to the exe, then the window closes.
//!
//! Scroll script (logical DIP/s of content):
//!   A 0-2 s   steady fling, 2,000 DIP/s  (~90 rows/s, ~1.5 new rows per frame)
//!   B 2-4 s   fast fling, 20,000 DIP/s   (~900 rows/s, ~15 new rows per frame)
//!   C 4-5 s   one full page per frame    (every string on screen is new)

use crate::view::{ROW_H, View};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

pub const TIMER_ID: usize = 1;
pub const TICK_MS: u32 = 10;
const GPU_WAIT: Duration = Duration::from_secs(3);
const PHASE_END: [f64; 3] = [2.0, 4.0, 5.0];
const START_ROW: usize = 500_000;

#[derive(PartialEq)]
enum State {
    WaitGpu(Instant),
    Running(Instant),
    Done,
}

struct Sample {
    phase: usize,
    total_ms: f32,
    end_ms: f32,
}

pub struct Bench {
    state: State,
    samples: Vec<Sample>,
    pub first_paint_ms: f64,
    first_path: &'static str,
    private_first: usize,
    private_before_scroll: usize,
    gpu_swap_ms: Option<f64>,
    final_path: &'static str,
    page_scroll_acc: f64,
    store_build_ms: Option<f64>,
    store_bytes: usize,
    source: &'static str,
    window_px: (i32, i32),
    dpi: f32,
    start_row: usize,
    /// set on the tick that starts the scroll run (main submits hang jobs then)
    pub just_started: bool,
    /// extra report lines (shell stats, set by main before the report)
    pub extra: String,
    pub rows: usize,
}

fn filetime_u64(ft: FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

/// Milliseconds from process creation to now.
pub fn ms_since_process_start() -> f64 {
    unsafe {
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        let now = filetime_u64(GetSystemTimePreciseAsFileTime());
        now.saturating_sub(filetime_u64(c)) as f64 / 10_000.0
    }
}

/// (private bytes, working set, peak pagefile/commit)
pub fn memory() -> (usize, usize, usize) {
    unsafe {
        let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb);
        (c.PrivateUsage, c.WorkingSetSize, c.PeakPagefileUsage)
    }
}

fn pct(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[i]
}

fn stats(label: &str, mut v: Vec<f32>) -> String {
    v.sort_by(|a, b| a.total_cmp(b));
    format!(
        "{label}: n={} p50={:.2} p95={:.2} p99={:.2} max={:.2} ms",
        v.len(),
        pct(&v, 0.50),
        pct(&v, 0.95),
        pct(&v, 0.99),
        v.last().copied().unwrap_or(0.0)
    )
}

impl Bench {
    pub fn new(source: &'static str, store_build_ms: Option<f64>, store_bytes: usize) -> Bench {
        Bench {
            state: State::Done, // until the first paint arms it
            samples: Vec::with_capacity(2048),
            first_paint_ms: 0.0,
            first_path: "",
            private_first: 0,
            private_before_scroll: 0,
            gpu_swap_ms: None,
            final_path: "",
            page_scroll_acc: 0.0,
            store_build_ms,
            store_bytes,
            source,
            window_px: (0, 0),
            dpi: 96.0,
            start_row: START_ROW,
            just_started: false,
            extra: String::new(),
            rows: 0,
        }
    }

    /// Called once, after the first paint completed.
    pub fn first_paint(&mut self, path: &'static str, window_px: (i32, i32), dpi: f32) {
        self.first_paint_ms = ms_since_process_start();
        self.first_path = path;
        self.private_first = memory().0;
        self.window_px = window_px;
        self.dpi = dpi;
        self.state = State::WaitGpu(Instant::now() + GPU_WAIT);
    }

    /// Called after every completed paint.
    pub fn painted(&mut self, total_ms: f32, end_ms: f32, path: &'static str) {
        match self.state {
            State::WaitGpu(_) if path == "hardware" => {
                self.gpu_swap_ms = Some(ms_since_process_start());
                // the swap paint itself is not a scroll frame; the next tick starts the run
                self.final_path = path;
                self.state = State::WaitGpu(Instant::now());
            }
            State::Running(t0) => {
                let t = t0.elapsed().as_secs_f64();
                let phase = PHASE_END.iter().position(|&e| t < e).unwrap_or(2);
                self.samples.push(Sample { phase, total_ms, end_ms });
                self.final_path = path;
            }
            _ => {}
        }
    }

    /// Timer tick. Returns (run over, repaint needed).
    pub fn tick(&mut self, v: &mut View) -> (bool, bool) {
        match self.state {
            State::WaitGpu(deadline) => {
                // --dir: also wait for the worker to finish listing
                if Instant::now() >= deadline && v.len() > 0 {
                    self.private_before_scroll = memory().0;
                    self.rows = v.len();
                    // small real folders start at the top and wrap around
                    self.start_row = if v.len() > 2 * START_ROW { START_ROW } else { 0 };
                    v.set_cursor(self.start_row + 3);
                    v.set_scroll(self.start_row as f64 * ROW_H as f64);
                    self.state = State::Running(Instant::now());
                    self.just_started = true;
                    return (false, true);
                }
                (false, false)
            }
            State::Running(t0) => {
                let t = t0.elapsed().as_secs_f64();
                let start = self.start_row as f64 * ROW_H as f64;
                if t >= PHASE_END[2] {
                    self.state = State::Done;
                    return (true, false);
                }
                let s = if t < PHASE_END[0] {
                    start + 2_000.0 * t
                } else if t < PHASE_END[1] {
                    start + 2_000.0 * PHASE_END[0] + 20_000.0 * (t - PHASE_END[0])
                } else {
                    self.page_scroll_acc += (v.page_rows() as f32 * ROW_H) as f64;
                    start + 2_000.0 * PHASE_END[0] + 20_000.0 * (PHASE_END[1] - PHASE_END[0]) + self.page_scroll_acc
                };
                let max = v.max_scroll();
                v.set_scroll(if max > 0.0 { s % max } else { 0.0 });
                (false, true)
            }
            State::Done => (false, false),
        }
    }

    pub fn report(&self) -> String {
        let exe = std::env::current_exe().ok();
        let exe_bytes = exe.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()).unwrap_or(0);
        let (private_end, ws_end, peak_commit) = memory();
        let all: Vec<f32> = self.samples.iter().map(|s| s.total_ms).collect();
        let mut out = String::new();
        let mut line = |s: String| {
            out.push_str(&s);
            out.push('\n');
        };
        line("tsubame S1/S4 bench".into());
        line(format!("rows: {}", self.rows));
        line(format!("build: {}", if cfg!(debug_assertions) { "debug" } else { "release" }));
        line(format!("data_source: {}", self.source));
        if let Some(ms) = self.store_build_ms {
            line(format!("store_build_ms: {ms:.1}"));
            line(format!("store_bytes: {} ({:.1} MB)", self.store_bytes, self.store_bytes as f64 / 1048576.0));
        }
        line(format!("window_px: {}x{} at {:.0} dpi", self.window_px.0, self.window_px.1, self.dpi));
        line(format!("first_paint_path: {}", self.first_path));
        line(format!("first_paint_ms: {:.1}", self.first_paint_ms));
        if let Some(b) = self.store_build_ms {
            line(format!("first_paint_minus_store_build_ms: {:.1}", self.first_paint_ms - b));
        }
        match self.gpu_swap_ms {
            Some(ms) => line(format!("gpu_target_ready_ms_since_start: {ms:.1}")),
            None => line("gpu_target_ready_ms_since_start: never (stayed software)".into()),
        }
        line(format!("scroll_path: {}", self.final_path));
        line(stats("paint_all", all));
        for (i, name) in ["A steady fling 2000 DIP/s", "B fast fling 20000 DIP/s", "C page per frame"].iter().enumerate() {
            line(stats(&format!("paint_phase_{name}"), self.samples.iter().filter(|s| s.phase == i).map(|s| s.total_ms).collect()));
        }
        line(stats("enddraw_all", self.samples.iter().map(|s| s.end_ms).collect()));
        line(format!("private_bytes_first_paint: {} ({:.1} MB)", self.private_first, self.private_first as f64 / 1048576.0));
        line(format!("private_bytes_before_scroll (after GPU swap, idle): {} ({:.1} MB)", self.private_before_scroll, self.private_before_scroll as f64 / 1048576.0));
        line(format!("private_bytes_end: {} ({:.1} MB)", private_end, private_end as f64 / 1048576.0));
        line(format!("working_set_end: {} ({:.1} MB)", ws_end, ws_end as f64 / 1048576.0));
        line(format!("peak_commit: {} ({:.1} MB)", peak_commit, peak_commit as f64 / 1048576.0));
        line(format!("exe_size_bytes: {exe_bytes} ({:.2} MB)", exe_bytes as f64 / 1048576.0));
        out.push_str(&self.extra);
        out
    }

    /// stdout (the parent's console, if any) and `s1-bench.txt` next to the exe.
    pub fn write_report(&self) {
        use std::io::Write;
        let text = self.report();
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            let name = std::env::var("TSUBAME_BENCH_OUT").unwrap_or_else(|_| "s1-bench.txt".into());
            let _ = std::fs::write(dir.join(name), &text);
        }
        // a windows-subsystem exe has no stdout: borrow the parent's console
        unsafe {
            use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
        if let Ok(mut con) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            let _ = con.write_all(text.as_bytes());
        }
    }
}
