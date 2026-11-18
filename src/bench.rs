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

use crate::tab::{Load, Tab};
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
    cost: FrameCost,
}

/// Where one frame's time went, beyond the total and EndDraw+Present (main
/// fills it from the renderer): Present alone, and the text layouts the frame
/// had to make (layout cache misses) with the time spent making them.
#[derive(Clone, Copy, Default)]
pub struct FrameCost {
    pub present_ms: f32,
    pub layouts: u32,
    pub layout_ms: f32,
    pub text_calls: u32,
    pub text_clipped: u32,
    pub text_ms: f32,
    pub image_ms: f32,
    pub bitmaps: u32,
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
    /// scroll the grid instead of the details list (main sets it from the view mode)
    pub grid: bool,
    /// extra report lines (shell stats, set by main before the report)
    pub extra: String,
    pub rows: usize,
    /// TSUBAME_MEMDIAG blocks (first paint, before the scroll)
    memdiag: String,
    /// first frame breakdown (paint start, target creation, EndDraw), set by main
    pub first_frame: String,
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
        let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
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

fn counts(label: &str, mut n: Vec<u32>) -> String {
    n.sort_unstable();
    let at = |p: f64| if n.is_empty() { 0 } else { n[((p * n.len() as f64).ceil() as usize).clamp(1, n.len()) - 1] };
    format!("{label}: n={} p50={} p95={} max={}", n.len(), at(0.5), at(0.95), n.last().copied().unwrap_or(0))
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
            grid: false,
            extra: String::new(),
            rows: 0,
            memdiag: String::new(),
            first_frame: String::new(),
        }
    }

    /// Called once, after the first paint completed.
    pub fn first_paint(&mut self, path: &'static str, window_px: (i32, i32), dpi: f32) {
        self.first_paint_ms = ms_since_process_start();
        self.first_path = path;
        self.private_first = memory().0;
        self.memdiag.push_str(&crate::memdiag::sample("first_paint"));
        self.window_px = window_px;
        self.dpi = dpi;
        self.state = State::WaitGpu(Instant::now() + GPU_WAIT);
    }

    /// Called after every completed paint.
    pub fn painted(&mut self, total_ms: f32, end_ms: f32, cost: FrameCost, path: &'static str) {
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
                self.samples.push(Sample { phase, total_ms, end_ms, cost });
                self.final_path = path;
            }
            _ => {}
        }
    }

    /// Timer tick. Returns (run over, repaint needed).
    pub fn tick(&mut self, v: &mut Tab) -> (bool, bool) {
        let row_h = v.list.row_h;
        let len = v.shown();
        match self.state {
            State::WaitGpu(deadline) => {
                // a real folder: also wait for the worker to finish listing
                if Instant::now() >= deadline && len > 0 && v.load == Load::Done {
                    self.private_before_scroll = memory().0;
                    self.memdiag.push_str(&crate::memdiag::sample("before_scroll"));
                    self.rows = len;
                    // small real folders start at the top and wrap around
                    self.start_row = if len > 2 * START_ROW { START_ROW } else { 0 };
                    v.list.cursor = self.start_row + 3;
                    v.sel.single(&v.store, self.start_row + 3);
                    if self.grid {
                        v.grid.cursor = self.start_row + 3;
                        v.grid.set_scroll(self.start_row as f64 * row_h as f64, len);
                    } else {
                        v.list.set_scroll(self.start_row as f64 * row_h as f64, len);
                    }
                    self.state = State::Running(Instant::now());
                    self.just_started = true;
                    return (false, true);
                }
                (false, false)
            }
            State::Running(t0) => {
                let t = t0.elapsed().as_secs_f64();
                let start = self.start_row as f64 * row_h as f64;
                if t >= PHASE_END[2] {
                    self.state = State::Done;
                    return (true, false);
                }
                let s = if t < PHASE_END[0] {
                    start + 2_000.0 * t
                } else if t < PHASE_END[1] {
                    start + 2_000.0 * PHASE_END[0] + 20_000.0 * (t - PHASE_END[0])
                } else {
                    let page = if self.grid { v.grid.page_rows() as f32 * v.grid.cell_h } else { v.list.page_rows() as f32 * row_h };
                    self.page_scroll_acc += page as f64;
                    start + 2_000.0 * PHASE_END[0] + 20_000.0 * (PHASE_END[1] - PHASE_END[0]) + self.page_scroll_acc
                };
                if self.grid {
                    let max = v.grid.max_scroll(len);
                    v.grid.set_scroll(if max > 0.0 { s % max } else { 0.0 }, len);
                } else {
                    let max = v.list.max_scroll(len);
                    v.list.set_scroll(if max > 0.0 { s % max } else { 0.0 }, len);
                }
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
        if !self.first_frame.is_empty() {
            line(self.first_frame.clone());
        }
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
        // where a frame's time goes (the scroll budget's regression guard,
        // docs/bench.md "PERF3"): building the frame on the CPU (everything
        // before EndDraw), Present alone, and the text layouts made per frame
        line(stats("cpu_build_all", self.samples.iter().map(|s| s.total_ms - s.end_ms).collect()));
        line(stats("present_all", self.samples.iter().map(|s| s.cost.present_ms).collect()));
        for (i, name) in ["A", "B", "C"].iter().enumerate() {
            let ph = || self.samples.iter().filter(move |s| s.phase == i);
            line(stats(&format!("cpu_build_phase_{name}"), ph().map(|s| s.total_ms - s.end_ms).collect()));
            line(stats(&format!("enddraw_phase_{name}"), ph().map(|s| s.end_ms).collect()));
            line(stats(&format!("layout_ms_phase_{name}"), ph().map(|s| s.cost.layout_ms).collect()));
            line(stats(&format!("text_ms_phase_{name}"), ph().map(|s| s.cost.text_ms).collect()));
            line(stats(&format!("image_ms_phase_{name}"), ph().map(|s| s.cost.image_ms).collect()));
            line(counts(&format!("layouts_made_phase_{name}"), ph().map(|s| s.cost.layouts).collect()));
            line(counts(&format!("text_calls_phase_{name}"), ph().map(|s| s.cost.text_calls).collect()));
            line(counts(&format!("text_clipped_phase_{name}"), ph().map(|s| s.cost.text_clipped).collect()));
            line(counts(&format!("bitmaps_made_phase_{name}"), ph().map(|s| s.cost.bitmaps).collect()));
        }
        line(format!("private_bytes_first_paint: {} ({:.1} MB)", self.private_first, self.private_first as f64 / 1048576.0));
        line(format!("private_bytes_before_scroll (after GPU swap, idle): {} ({:.1} MB)", self.private_before_scroll, self.private_before_scroll as f64 / 1048576.0));
        line(format!("private_bytes_end: {} ({:.1} MB)", private_end, private_end as f64 / 1048576.0));
        line(format!("working_set_end: {} ({:.1} MB)", ws_end, ws_end as f64 / 1048576.0));
        line(format!("peak_commit: {} ({:.1} MB)", peak_commit, peak_commit as f64 / 1048576.0));
        line(format!("exe_size_bytes: {exe_bytes} ({:.2} MB)", exe_bytes as f64 / 1048576.0));
        out.push_str(&self.extra);
        out.push_str(&self.memdiag);
        out.push_str(&crate::memdiag::sample_end());
        out
    }

    /// stdout (the parent's console, if any) and `s1-bench.txt` next to the exe.
    pub fn write_report(&self) {
        let name = std::env::var("TSUBAME_BENCH_OUT").unwrap_or_else(|_| "s1-bench.txt".into());
        write_text_report(&name, &self.report());
    }
}

/// Writes `text` to `name` next to the exe and to the parent's console (a
/// windows-subsystem exe has no stdout of its own).
pub fn write_text_report(name: &str, text: &str) {
    use std::io::Write;
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let _ = std::fs::write(dir.join(name), text);
    }
    unsafe {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    if let Ok(mut con) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        let _ = con.write_all(text.as_bytes());
    }
}
