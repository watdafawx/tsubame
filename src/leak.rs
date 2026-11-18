//! Leak measurement. Two modes:
//!
//! - `--leak-smoke`: no window. Navigates 200 folders in one tab, then opens
//!   and closes 50 tabs, and prints private bytes, handles and GDI/USER
//!   objects before and after. A short warm-up round runs first so one-time
//!   allocations (thread pools, allocator arenas) are not counted as leaks.
//! - `--leak-gate`: the full gate from `docs/PLAN.md` section 2, in the real
//!   GUI process: real window at 4K, real enumerator, real shell icon
//!   workers, real tabs. Navigates `--gate-navs` folders (default 10,000,
//!   cycling a fixture tree, with back/forward/up), then opens and closes
//!   `--gate-tabs` tabs (default 1,000, each opening a folder, scrolling,
//!   then closing), painting every step so every visible row's icon is asked
//!   for like a real user's. First it shows one tab on a small folder until
//!   the GPU target is up and settled (the memory budget's scenario), then it
//!   warms the bounded caches (two fixture passes, `tabs/5` tab cycles) so the
//!   measured round starts from the steady state. Counters are sampled
//!   before, every `--gate-sample` operations (default 1,000) and after a
//!   `--gate-settle` second idle settle (default 5), then the icon cache (the
//!   app's one explicit cache) is cleared and sampled once more. Writes
//!   `leak-gate.txt` next to the exe (env `TSUBAME_LEAK_OUT` renames it) and
//!   leaves the exit code 0 (pass) or 1 (fail) to `main`.
//!
//! The gate is driven by `main.rs`: a 100 ms timer plus a kick after every
//! paint call `Gate::step`, which never blocks the message loop.

use crate::shellui::ShellUi;
use crate::view::View;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tsubame_ui::Density;
use tsubame_win::procinfo::{Counters, counters};
use tsubame_win::shell::Pool;

const NAVS: usize = 200;
const TABS: usize = 50;

// ---- the gate's thresholds (also written into the report) ------------------

/// Private bytes after the settle may exceed the start by this percent...
pub const PRIVATE_PCT: f64 = 5.0;
/// ...ignoring growth below this absolute floor (allocator arenas, one-time
/// growth of small runs).
pub const PRIVATE_FLOOR: usize = 2 << 20;
/// Kernel handles.
pub const ALLOW_HANDLES: i64 = 32;
/// GDI objects.
pub const ALLOW_GDI: i64 = 16;
/// USER objects.
pub const ALLOW_USER: i64 = 16;
/// Threads (shell workers, enumerator threads that have not exited yet).
pub const ALLOW_THREADS: i64 = 8;

/// The gate timer (`main.rs`): a watchdog, not the driver. Every paint and
/// every enumerator/shell message advances the gate first; this keeps a lost
/// wake from stalling a 10,000-operation run.
pub const TIMER_ID: usize = 5;
pub const TICK_MS: u32 = 100;

/// Set when the gate ran and failed check. `main` exits with 1 then.
pub static GATE_FAILED: AtomicBool = AtomicBool::new(false);

/// What the gate wants from `main` after `step`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Waiting for a worker, a shell reply or the clock: nothing to do.
    Wait,
    /// Invalidate the window (the next paint continues the gate).
    Repaint,
    /// Report written: close the window.
    Done,
}

fn mb(b: usize) -> f64 {
    b as f64 / 1048576.0
}

// ---- headless smoke --------------------------------------------------------

fn settle(v: &mut View) {
    let t0 = Instant::now();
    while v.tab().is_loading() && t0.elapsed() < Duration::from_secs(10) {
        v.pump();
        std::thread::sleep(Duration::from_millis(1));
    }
    v.pump();
}

/// Folders to visit: children of the start folder and of its siblings,
/// cycled until `NAVS` navigations are done.
fn pick_folders(v: &mut View, start: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut roots = vec![start.to_string()];
    if let Some(p) = crate::nav::parent(start) {
        roots.push(p);
    }
    for r in roots {
        v.go(&r);
        settle(v);
        let t = v.tab();
        for i in 0..t.shown() {
            if let Some((p, true)) = t.entry_at(i) {
                out.push(p);
                if out.len() >= 64 {
                    return out;
                }
            }
        }
    }
    if out.is_empty() {
        out.push(start.to_string());
    }
    out
}

fn round(v: &mut View, folders: &[String], navs: usize, tabs: usize) {
    for i in 0..navs {
        v.go(&folders[i % folders.len()]);
        settle(v);
    }
    for _ in 0..tabs {
        v.new_tab();
        settle(v);
        v.close_tab(v.tabs.active);
    }
}

pub fn run(start: &str) -> String {
    let wake: tsubame_win::enumerate::Wake = Arc::new(|| {});
    let mut v = View::new(start, wake, true, Density::Compact);
    settle(&mut v);
    let folders = pick_folders(&mut v, start);
    round(&mut v, &folders, 20, 5); // warm-up
    let before = counters();
    round(&mut v, &folders, NAVS, TABS);
    // let detached enumerator threads finish before measuring
    std::thread::sleep(Duration::from_millis(500));
    let after = counters();
    let mut s = String::new();
    s.push_str(&format!("leak-smoke: {NAVS} navigations over {} folders, {TABS} tab open/close, start {start}\n", folders.len()));
    s.push_str(&format!("before: {}\n", before.line()));
    s.push_str(&format!("after:  {}\n", after.line()));
    s.push_str(&format!(
        "delta: private {:+.2} MB, working set {:+.2} MB, handles {:+}, gdi {:+}, user {:+}\n",
        mb(after.private_bytes) - mb(before.private_bytes),
        mb(after.working_set) - mb(before.working_set),
        after.handles as i64 - before.handles as i64,
        after.gdi as i64 - before.gdi as i64,
        after.user as i64 - before.user as i64
    ));
    s
}

// ---- the full gate ---------------------------------------------------------

/// One counter sample.
#[derive(Clone, Copy, Debug, Default)]
struct Snap {
    c: Counters,
    threads: u32,
}

impl Snap {
    fn take() -> Snap {
        Snap { c: counters(), threads: thread_count() }
    }
}

#[derive(Clone, Debug)]
struct Sample {
    ops: usize,
    snap: Snap,
}

/// Threads of this process. `windows` 0.61 puts `CreateToolhelp32Snapshot`
/// behind the `Win32_System_Diagnostics_ToolHelp` feature, which this binary
/// does not enable; the three calls are declared here instead of widening the
/// app's feature list for one diagnostic counter.
#[repr(C)]
struct ThreadEntry32 {
    dw_size: u32,
    cnt_usage: u32,
    th32_thread_id: u32,
    th32_owner_process_id: u32,
    tp_base_pri: i32,
    tp_delta_pri: i32,
    dw_flags: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
    fn Thread32First(snapshot: *mut c_void, entry: *mut ThreadEntry32) -> i32;
    fn Thread32Next(snapshot: *mut c_void, entry: *mut ThreadEntry32) -> i32;
    fn CloseHandle(object: *mut c_void) -> i32;
}

const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;

pub fn thread_count() -> u32 {
    let pid = std::process::id();
    unsafe {
        let h = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if h.is_null() || h as isize == -1 {
            return 0;
        }
        let mut e = ThreadEntry32 { dw_size: size_of::<ThreadEntry32>() as u32, cnt_usage: 0, th32_thread_id: 0, th32_owner_process_id: 0, tp_base_pri: 0, tp_delta_pri: 0, dw_flags: 0 };
        let mut n = 0;
        if Thread32First(h, &mut e) != 0 {
            loop {
                if e.th32_owner_process_id == pid {
                    n += 1;
                }
                e.dw_size = size_of::<ThreadEntry32>() as u32;
                if Thread32Next(h, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(h);
        n
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Collect the fixture folders (once).
    Prepare,
    /// The first listing of the window: wait until it is done.
    Startup,
    /// One tab on the fixture's first folder, GPU target up, settled: this is
    /// the "one tab, small folder, 4K" budget sample.
    IdleSettle { until: Instant },
    /// Warm-up round, not measured.
    WarmNav,
    WarmTab,
    /// The settle after the warm-up, then the start sample (the measured
    /// round's baseline).
    PreSettle(Instant),
    /// The measured round.
    RunNav,
    RunTab,
    /// Five idle seconds, then the end sample.
    PostSettle(Instant),
    /// Counters right after the icon cache is cleared.
    Trim,
    /// Report written.
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Load,
    Paint,
    Icons,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Nav,
    Tab,
}

/// The full gate (`--leak-gate`). Driven from `main.rs`.
pub struct Gate {
    /// fixture root; its folder tree is cycled
    root: String,
    navs: usize,
    tabs: usize,
    warm_navs: usize,
    warm_tabs: usize,
    sample_every: usize,
    settle: Duration,
    folders: Vec<String>,
    /// most entries seen in one fixture folder, for the report
    largest: usize,
    phase: Phase,
    /// op index inside the current round
    op: usize,
    /// navigation index inside the current round (nav ops only)
    nav_i: usize,
    kind: Kind,
    stage: Stage,
    /// wall clock when the current stage started (timeout guard)
    stage_at: Instant,
    /// `paints` when the current paint wait started
    paint_mark: u64,
    paints: u64,
    timeline: Vec<Sample>,
    /// counters of the one-tab, small-folder idle window (the memory budget's
    /// scenario), before the warm-up
    idle: Option<Snap>,
    start: Option<Snap>,
    /// folder the start sample was taken in (the end sample returns to it)
    start_folder: String,
    end: Option<Snap>,
    trim: Option<Snap>,
    trim_note: String,
    /// TSUBAME_MEMDIAG blocks (start, end, after the trim)
    memdiag: String,
    notes: Vec<String>,
    run_started: Option<Instant>,
    /// start of the current measured phase (navs, then tabs)
    phase_started: Instant,
    nav_s: f64,
    tab_s: f64,
    /// where an op's wall time went: waiting for the listing, the frame, the icons
    t_load: f64,
    t_paint: f64,
    t_icons: f64,
    steps: u64,
    /// the GPU target must be up before the start sample (the WARP first
    /// frame costs ~60 MB this process then gives back)
    gpu_deadline: Instant,
    gpu_ok: bool,
    elapsed_s: f64,
    window_px: (i32, i32),
    dpi: f32,
    paint_path: &'static str,
    /// paint path when the start sample was taken
    start_path: &'static str,
    pass: bool,
    timeouts: usize,
}

/// Stage timeouts: a stage that cannot finish (a lost wake, a worker that
/// never answers) advances the gate with a note instead of stalling the run.
const OP_TIMEOUT: Duration = Duration::from_secs(5);

impl Gate {
    pub fn new(root: &str, navs: usize, tabs: usize, sample_every: usize, settle: Duration) -> Gate {
        let root = root.trim_end_matches('\\').to_string();
        Gate {
            root,
            navs,
            tabs,
            // a full pass over the fixture warms the icon and layout caches, so
            // the measured round starts from the steady state (both numbers are
            // in the report; a bounded cache filling is not a leak)
            warm_navs: 5,
            warm_tabs: (tabs / 5).clamp(2, 200).min(tabs.max(2)),
            sample_every: sample_every.max(1),
            settle,
            folders: Vec::new(),
            largest: 0,
            phase: Phase::Prepare,
            op: 0,
            nav_i: 0,
            kind: Kind::Nav,
            stage: Stage::Load,
            stage_at: Instant::now(),
            paint_mark: 0,
            paints: 0,
            timeline: Vec::new(),
            idle: None,
            start: None,
            start_folder: String::new(),
            end: None,
            trim: None,
            trim_note: String::new(),
            memdiag: String::new(),
            notes: Vec::new(),
            run_started: None,
            phase_started: Instant::now(),
            nav_s: 0.0,
            tab_s: 0.0,
            t_load: 0.0,
            t_paint: 0.0,
            t_icons: 0.0,
            steps: 0,
            gpu_deadline: Instant::now(),
            gpu_ok: false,
            elapsed_s: 0.0,
            window_px: (0, 0),
            dpi: 96.0,
            paint_path: "",
            start_path: "",
            pass: false,
            timeouts: 0,
        }
    }

    /// Window size, DPI and paint path, for the report.
    pub fn env(&mut self, window_px: (i32, i32), dpi: f32, paint_path: &'static str) {
        self.window_px = window_px;
        self.dpi = dpi;
        self.paint_path = paint_path;
    }
    /// A completed paint (rows drawn, icons requested for the visible ones).
    pub fn painted(&mut self, paint_path: &'static str) {
        self.paints += 1;
        self.paint_path = paint_path;
    }

    fn note(&mut self, s: String) {
        if self.notes.len() < 16 {
            self.notes.push(s);
        }
    }

    fn sample(&mut self, ops: usize) -> Snap {
        let s = Snap::take();
        self.timeline.push(Sample { ops, snap: s });
        self.memdiag.push_str(&crate::memdiag::line(&format!("{ops} ops")));
        s
    }

    /// The fixture's folder tree, sorted for a deterministic cycle.
    fn discover(&mut self) {
        let mut out = Vec::new();
        let mut stack = vec![(self.root.clone(), 0usize)];
        let mut largest = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            match std::fs::read_dir(&dir) {
                Ok(rd) => {
                    let mut n = 0usize;
                    for e in rd.flatten() {
                        n += 1;
                        // follow only real directories, and not through reparse points
                        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        if is_dir && depth < 3 && out.len() + stack.len() < 1000 {
                            stack.push((format!("{dir}\\{}", e.file_name().to_string_lossy()), depth + 1));
                        }
                    }
                    largest = largest.max(n);
                }
                Err(e) => self.note(format!("cannot list {dir}: {e}")),
            }
            out.push(dir);
        }
        out.sort();
        if out.len() < 200 {
            self.note(format!("fixture has only {} folders; the plan asks for 200 or more", out.len()));
        }
        // two passes over every folder: the op pattern visits ~60% of them per
        // pass (back/forward/up do not change folder), so one pass would leave
        // the per-path icon cache partly cold at the start sample
        self.warm_navs = out.len().saturating_mul(2).clamp(5, 2000).min(self.navs.max(5));
        self.largest = largest;
        self.folders = out;
    }

    fn tabs_idle(v: &View) -> bool {
        !v.tabs.items.iter().any(|t| t.is_loading())
    }

    /// All submitted shell jobs have answered (icons applied, cancelled or failed).
    fn icons_idle(pool: &Pool) -> bool {
        if pool.busy() {
            return false;
        }
        let s = pool.stats();
        s.replies + s.skipped_cancelled >= s.submitted
    }

    /// Op target: `ops_per_folder` mixes plain navigations with back, forward
    /// and up so history handling is part of the gate.
    fn nav_index(&self, i: usize) -> usize {
        (i.wrapping_mul(37).wrapping_add(11)) % self.folders.len().max(1)
    }

    fn issue(&mut self, v: &mut View) {
        self.stage = Stage::Load;
        self.stage_at = Instant::now();
        self.paint_mark = self.paints;
        match self.kind {
            Kind::Nav => {
                let i = self.nav_i;
                self.nav_i += 1;
                match i % 10 {
                    6 => v.back(),
                    7 => v.forward(),
                    8 => v.up(),
                    _ => {
                        let p = self.folders[self.nav_index(i)].clone();
                        v.go(&p);
                    }
                }
            }
            Kind::Tab => {
                // a new tab opens the current folder, then navigates: the
                // dropped listing must cancel cleanly (that is a leak path)
                v.new_tab();
                let p = self.folders[self.nav_index(self.op.wrapping_mul(53).wrapping_add(7))].clone();
                v.go(&p);
            }
        }
    }

    /// After a tab's listing: scroll it so more icons are asked for, like a
    /// user looking down the folder.
    fn scroll_tab(v: &mut View) {
        let t = v.tab_mut();
        let len = t.shown();
        let rh = t.list.row_h as f64;
        let target = ((len / 2) as f64 * rh).min(t.list.max_scroll(len).max(0.0));
        t.list.set_scroll(target, len);
    }

    fn close_tab(v: &mut View) {
        let i = v.tabs.active;
        if v.tabs.items.len() > 1 {
            v.close_tab(i);
        }
    }

    /// Operations completed in the measured round (navs first, then tabs).
    fn done_ops(&self) -> usize {
        match self.phase {
            Phase::RunTab => self.navs + self.op,
            _ => self.op,
        }
    }

    /// The current op finished: count it, sample if due, start the next one.
    fn op_done(&mut self, v: &mut View) {
        if self.kind == Kind::Tab {
            Self::close_tab(v);
        }
        self.op += 1;
        let done = self.done_ops();
        let measured = matches!(self.phase, Phase::RunNav | Phase::RunTab);
        // the settle sample covers the last operation; do not report it twice
        if measured && done.is_multiple_of(self.sample_every) && done < self.navs + self.tabs {
            self.sample(done);
        }
        // next op
        match self.phase {
            Phase::WarmNav if self.op < self.warm_navs => {
                self.kind = Kind::Nav;
                self.issue(v);
            }
            Phase::WarmNav => {
                self.phase = Phase::WarmTab;
                self.op = 0;
                self.kind = Kind::Tab;
                self.issue(v);
            }
            Phase::WarmTab if self.op < self.warm_tabs => {
                self.kind = Kind::Tab;
                self.issue(v);
            }
            Phase::WarmTab => {
                // paint the folder the end sample will also use, so the two
                // samples see the same layout-cache and icon-cache state
                if let Some(p) = self.folders.first().cloned() {
                    v.go(&p);
                }
                let now = Instant::now();
                self.gpu_deadline = now + Duration::from_secs(15);
                self.gpu_ok = self.paint_path == "hardware";
                self.phase = Phase::PreSettle(now + self.settle);
            }
            Phase::RunNav if self.op < self.navs => {
                self.kind = Kind::Nav;
                self.issue(v);
            }
            Phase::RunNav => {
                self.nav_s = self.phase_started.elapsed().as_secs_f64();
                self.phase_started = Instant::now();
                self.phase = Phase::RunTab;
                self.op = 0;
                self.kind = Kind::Tab;
                self.issue(v);
            }
            Phase::RunTab if self.op < self.tabs => {
                self.kind = Kind::Tab;
                self.issue(v);
            }
            Phase::RunTab => {
                self.tab_s = self.phase_started.elapsed().as_secs_f64();
                // back in the folder the start sample was taken in: the
                // comparison must be like for like
                if !self.start_folder.is_empty() {
                    let p = self.start_folder.clone();
                    v.go(&p);
                }
                self.phase = Phase::PostSettle(Instant::now() + self.settle);
                self.elapsed_s = self.run_started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
            }
            _ => {}
        }
    }

    fn timed_out(&mut self, stage: &str) -> bool {
        if self.stage_at.elapsed() < OP_TIMEOUT {
            return false;
        }
        self.timeouts += 1;
        self.note(format!("stage {stage} timed out after {} ms (op {}, {} total)", self.stage_at.elapsed().as_millis(), self.op, self.timeouts));
        true
    }

    /// One gate step. Called from the timer and after every paint; must not
    /// block and must not dispatch messages.
    pub fn step(&mut self, v: &mut View, shell: &mut ShellUi, pool: &mut Pool) -> Step {
        self.steps += 1;
        // consume batches even if a wake was coalesced away
        v.pump();
        match self.phase {
            Phase::Prepare => {
                self.discover();
                if self.folders.is_empty() {
                    self.folders.push(self.root.clone());
                }
                self.phase = Phase::Startup;
                Step::Wait
            }
            Phase::Startup => {
                if Self::tabs_idle(v) || self.timed_out("startup") {
                    // the budget's "one tab, small folder" window: the fixture
                    // root, once the GPU target has replaced the first frame
                    if let Some(p) = self.folders.first().cloned() {
                        v.go(&p);
                    }
                    let now = Instant::now();
                    self.gpu_deadline = now + Duration::from_secs(15);
                    self.gpu_ok = self.paint_path == "hardware";
                    self.phase = Phase::IdleSettle { until: now + self.settle };
                }
                Step::Wait
            }
            Phase::IdleSettle { until } => {
                if !self.gpu_ok {
                    if self.paint_path == "hardware" || Instant::now() >= self.gpu_deadline {
                        if self.paint_path != "hardware" {
                            self.note(format!("GPU target never appeared: paint path stays '{}'", self.paint_path));
                        }
                        self.gpu_ok = true;
                        self.phase = Phase::IdleSettle { until: Instant::now() + self.settle };
                    }
                    return Step::Wait;
                }
                if Instant::now() < until {
                    return Step::Wait;
                }
                self.idle = Some(Snap::take());
                self.phase = Phase::WarmNav;
                self.op = 0;
                self.nav_i = 0;
                self.kind = Kind::Nav;
                self.issue(v);
                Step::Wait
            }
            Phase::WarmNav | Phase::WarmTab | Phase::RunNav | Phase::RunTab => self.poll(v, pool),
            Phase::PreSettle(until) => {
                // the software first frame is replaced by the GPU target a few
                // hundred ms in and gives back ~60 MB: sampling before that
                // would compare a transient against the steady state
                if !self.gpu_ok {
                    if self.paint_path == "hardware" || Instant::now() >= self.gpu_deadline {
                        if self.paint_path != "hardware" {
                            self.note(format!("GPU target never appeared: paint path stays '{}'", self.paint_path));
                        }
                        self.gpu_ok = true;
                        // the settle counts from the GPU target, like the end sample
                        self.phase = Phase::PreSettle(Instant::now() + self.settle);
                    }
                    return Step::Wait;
                }
                if Instant::now() < until {
                    return Step::Wait;
                }
                self.start = Some(self.sample(0));
                self.memdiag.push_str(&crate::memdiag::sample("gate_start"));
                self.start_folder = v.tab().path.clone();
                self.start_path = self.paint_path;
                self.phase = Phase::RunNav;
                self.op = 0;
                self.nav_i = 0;
                self.kind = Kind::Nav;
                self.run_started = Some(Instant::now());
                self.phase_started = Instant::now();
                self.issue(v);
                Step::Wait
            }
            Phase::PostSettle(until) => {
                if Instant::now() < until {
                    return Step::Wait;
                }
                let end = self.sample(self.navs + self.tabs);
                self.end = Some(end);
                self.memdiag.push_str(&crate::memdiag::sample("gate_end"));
                self.phase = Phase::Trim;
                Step::Wait
            }
            Phase::Trim => {
                let before = shell.icons.len();
                shell.icons = crate::lru::Lru::new(crate::shellui::ICON_CAP_ITEMS, crate::shellui::ICON_CAP_BYTES);
                self.trim_note = format!("icon cache cleared ({before} entries, cap {} items / {} MB)", crate::shellui::ICON_CAP_ITEMS, crate::shellui::ICON_CAP_BYTES / 1048576);
                self.trim = Some(Snap::take());
                self.memdiag.push_str(&crate::memdiag::sample_opt("gate_trim"));
                let text = self.report_text();
                let name = std::env::var("TSUBAME_LEAK_OUT").unwrap_or_else(|_| "leak-gate.txt".into());
                crate::bench::write_text_report(&name, &text);
                self.phase = Phase::Done;
                if !self.pass {
                    GATE_FAILED.store(true, Ordering::Release);
                }
                Step::Done
            }
            Phase::Done => Step::Wait,
        }
    }

    fn poll(&mut self, v: &mut View, pool: &Pool) -> Step {
        let waited = self.stage_at.elapsed().as_secs_f64();
        match self.stage {
            Stage::Load => {
                if Self::tabs_idle(v) {
                    if self.kind == Kind::Tab {
                        Self::scroll_tab(v);
                    }
                    self.t_load += waited;
                    // require a paint *after* the listing settled: the wait
                    // below counts paints from here on
                    self.paint_mark = self.paints;
                    self.stage = Stage::Paint;
                    self.stage_at = Instant::now();
                    return Step::Repaint;
                }
                if self.timed_out("load") {
                    self.t_load += waited;
                    self.paint_mark = self.paints;
                    self.stage = Stage::Paint;
                    self.stage_at = Instant::now();
                    return Step::Repaint;
                }
                // keep painting while the listing streams: the app's own
                // repaints are throttled to 60/s, a user's window still shows
                // every batch, and painting here lets the wake messages through
                Step::Repaint
            }
            Stage::Paint => {
                if self.paints > self.paint_mark || self.timed_out("paint") {
                    self.t_paint += waited;
                    self.stage = Stage::Icons;
                    self.stage_at = Instant::now();
                    // the icons may already be settled (cached from an earlier
                    // visit): check now instead of waiting for the next tick
                    if Self::icons_idle(pool) {
                        self.t_icons += self.stage_at.elapsed().as_secs_f64();
                        self.op_done(v);
                    }
                    return Step::Wait;
                }
                // the invalidate of the previous step is still queued: let it paint
                Step::Repaint
            }
            Stage::Icons => {
                if Self::icons_idle(pool) || self.timed_out("icons") {
                    self.t_icons += waited;
                    self.op_done(v);
                }
                Step::Wait
            }
        }
    }

    // ---- report ----------------------------------------------------------

    fn snapshot_fields(out: &mut String, prefix: &str, s: Snap) {
        use std::fmt::Write;
        let _ = writeln!(out, "{prefix}.private_bytes={}", s.c.private_bytes);
        let _ = writeln!(out, "{prefix}.private_mb={:.2}", mb(s.c.private_bytes));
        let _ = writeln!(out, "{prefix}.working_set_bytes={}", s.c.working_set);
        let _ = writeln!(out, "{prefix}.handles={}", s.c.handles);
        let _ = writeln!(out, "{prefix}.gdi={}", s.c.gdi);
        let _ = writeln!(out, "{prefix}.user={}", s.c.user);
        let _ = writeln!(out, "{prefix}.threads={}", s.threads);
    }

    fn report_text(&mut self) -> String {
        use std::fmt::Write;
        let start = self.start.unwrap_or_default();
        let end = self.end.unwrap_or_default();
        let ops = self.navs + self.tabs;
        let delta_private = end.c.private_bytes as i64 - start.c.private_bytes as i64;
        let pct = if start.c.private_bytes > 0 { delta_private as f64 * 100.0 / start.c.private_bytes as f64 } else { 0.0 };
        let allow_private = ((start.c.private_bytes as f64 * PRIVATE_PCT / 100.0) as usize).max(PRIVATE_FLOOR);
        let delta_handles = end.c.handles as i64 - start.c.handles as i64;
        let delta_gdi = end.c.gdi as i64 - start.c.gdi as i64;
        let delta_user = end.c.user as i64 - start.c.user as i64;
        let delta_threads = end.threads as i64 - start.threads as i64;
        let pass_private = delta_private <= allow_private as i64;
        let pass_handles = delta_handles <= ALLOW_HANDLES;
        let pass_gdi = delta_gdi <= ALLOW_GDI;
        let pass_user = delta_user <= ALLOW_USER;
        let pass_threads = delta_threads <= ALLOW_THREADS;
        let fail = [(!pass_private).then_some("private"), (!pass_handles).then_some("handles"), (!pass_gdi).then_some("gdi"), (!pass_user).then_some("user"), (!pass_threads).then_some("threads")];
        let failed: Vec<&str> = fail.iter().flatten().copied().collect();
        self.pass = failed.is_empty();
        let peak = self.timeline.iter().map(|s| s.snap.c.private_bytes).max().unwrap_or(0);

        let mut s = String::new();
        let _ = writeln!(s, "tool=tsubame-leak-gate");
        let _ = writeln!(s, "version=1");
        let _ = writeln!(s, "mode=full-gui");
        let _ = writeln!(s, "build={}", if cfg!(debug_assertions) { "debug" } else { "release" });
        let _ = writeln!(s, "window_px={}x{}", self.window_px.0, self.window_px.1);
        let _ = writeln!(s, "window_dpi={:.0}", self.dpi);
        let _ = writeln!(s, "paint_path={}", self.paint_path);
        let _ = writeln!(s, "start_paint_path={}", self.start_path);
        let _ = writeln!(s, "paints={}", self.paints);
        let _ = writeln!(s, "fixture={}", self.root);
        let _ = writeln!(s, "folders={}", self.folders.len());
        let _ = writeln!(s, "largest_folder_entries={}", self.largest);
        let _ = writeln!(s, "navigations={}", self.navs);
        let _ = writeln!(s, "tabs={}", self.tabs);
        let _ = writeln!(s, "ops={ops}");
        let _ = writeln!(s, "warmup_navigations={}", self.warm_navs);
        let _ = writeln!(s, "warmup_tabs={}", self.warm_tabs);
        let _ = writeln!(s, "sample_interval={}", self.sample_every);
        let _ = writeln!(s, "settle_s={}", self.settle.as_secs_f64());
        let _ = writeln!(s, "elapsed_s={:.1}", self.elapsed_s);
        if self.elapsed_s > 0.0 {
            let _ = writeln!(s, "ops_per_s={:.1}", ops as f64 / self.elapsed_s);
        }
        let _ = writeln!(s, "navigations_s={:.1}", self.nav_s);
        let _ = writeln!(s, "tabs_s={:.1}", self.tab_s);
        if self.nav_s > 0.0 {
            let _ = writeln!(s, "navigations_per_s={:.1}", self.navs as f64 / self.nav_s);
        }
        if self.tab_s > 0.0 {
            let _ = writeln!(s, "tabs_per_s={:.1}", self.tabs as f64 / self.tab_s);
        }
        let _ = writeln!(s, "stage_s.load={:.1}", self.t_load);
        let _ = writeln!(s, "stage_s.paint={:.1}", self.t_paint);
        let _ = writeln!(s, "stage_s.icons={:.1}", self.t_icons);
        let _ = writeln!(s, "steps={}", self.steps);
        let _ = writeln!(s, "icon_px={}", crate::view::ICON_DIP);
        let _ = writeln!(s, "private_pct_allow={PRIVATE_PCT}");
        let _ = writeln!(s, "private_floor_bytes={PRIVATE_FLOOR}");
        let _ = writeln!(s, "handles_allow={ALLOW_HANDLES}");
        let _ = writeln!(s, "gdi_allow={ALLOW_GDI}");
        let _ = writeln!(s, "user_allow={ALLOW_USER}");
        let _ = writeln!(s, "threads_allow={ALLOW_THREADS}");
        Self::snapshot_fields(&mut s, "start", start);
        for sample in &self.timeline {
            Self::snapshot_fields(&mut s, &format!("sample.{}", sample.ops), sample.snap);
        }
        Self::snapshot_fields(&mut s, "end", end);
        if let Some(b) = self.idle {
            Self::snapshot_fields(&mut s, "idle", b);
            let _ = writeln!(s, "delta.private_bytes_from_idle={}", end.c.private_bytes as i64 - b.c.private_bytes as i64);
        }
        let _ = writeln!(s, "delta.private_bytes={delta_private}");
        let _ = writeln!(s, "delta.private_pct={pct:.2}");
        let _ = writeln!(s, "allow.private_bytes={allow_private}");
        let _ = writeln!(s, "pass.private={}", pass_private as u8);
        let _ = writeln!(s, "delta.handles={delta_handles}");
        let _ = writeln!(s, "pass.handles={}", pass_handles as u8);
        let _ = writeln!(s, "delta.gdi={delta_gdi}");
        let _ = writeln!(s, "pass.gdi={}", pass_gdi as u8);
        let _ = writeln!(s, "delta.user={delta_user}");
        let _ = writeln!(s, "pass.user={}", pass_user as u8);
        let _ = writeln!(s, "delta.threads={delta_threads}");
        let _ = writeln!(s, "pass.threads={}", pass_threads as u8);
        let _ = writeln!(s, "peak.private_bytes={peak}");
        if let Some(t) = self.trim {
            Self::snapshot_fields(&mut s, "trim", t);
            let _ = writeln!(s, "trim.delta_private_bytes={}", t.c.private_bytes as i64 - end.c.private_bytes as i64);
            let _ = writeln!(s, "trim.note={}", self.trim_note);
        }
        let _ = writeln!(s, "timeouts={}", self.timeouts);
        for n in &self.notes {
            let _ = writeln!(s, "note={n}");
        }
        s.push_str(&self.memdiag);
        let _ = writeln!(s, "gate={}", if self.pass { "PASS" } else { "FAIL" });
        let _ = writeln!(s, "fail={}", if failed.is_empty() { "-".to_string() } else { failed.join(",") });
        let _ = writeln!(s, "exit={}", (!self.pass) as u8);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_count_is_plausible() {
        let n = thread_count();
        assert!(n >= 1, "the calling thread alone must be counted, got {n}");
        assert!(n < 4096, "implausible thread count {n}");
    }

    #[test]
    fn the_private_allowance_is_five_percent_with_a_two_mb_floor() {
        let snap = |bytes: usize| Snap { c: Counters { private_bytes: bytes, ..Default::default() }, threads: 4 };
        let mut g = Gate::new("C:\\definitely\\not\\here", 4, 2, 1000, Duration::from_secs(1));
        g.start = Some(snap(100 << 20));
        g.end = Some(snap(104 << 20)); // 4%: inside the 5 MiB allowance
        let text = g.report_text();
        assert!(text.contains("allow.private_bytes=5242880"), "{text}");
        assert!(text.contains("pass.private=1"), "{text}");
        assert!(text.contains("gate=PASS"), "{text}");
        assert!(text.contains("exit=0"), "{text}");
        // 5.5% fails and names the counter
        g.end = Some(snap((105.5 * 1048576.0) as usize));
        let text = g.report_text();
        assert!(text.contains("pass.private=0"), "{text}");
        assert!(text.contains("gate=FAIL"), "{text}");
        assert!(text.contains("fail=private"), "{text}");
        assert!(text.contains("exit=1"), "{text}");
        // a small baseline uses the 2 MiB floor instead of the percentage
        g.start = Some(snap(2 << 20));
        g.end = Some(snap(3 << 20));
        let text = g.report_text();
        assert!(text.contains("allow.private_bytes=2097152"), "{text}");
        assert!(text.contains("gate=PASS"), "{text}");
    }

    #[test]
    fn gate_reports_its_thresholds_and_fails_closed() {
        let mut g = Gate::new("C:\\definitely\\not\\here", 4, 2, 1000, Duration::from_secs(1));
        // no fixture on the test machine: discovery notes it and the run still
        // produces a report (a gate that cannot run is not a pass)
        g.discover();
        assert!(!g.folders.is_empty(), "the root itself is always visited");
        g.start = Some(Snap::default());
        g.end = Some(Snap::default());
        let text = g.report_text();
        assert!(text.contains("tool=tsubame-leak-gate"), "{text}");
        assert!(text.contains("handles_allow="), "{text}");
        // equal start and end: nothing grew, so this one passes
        assert!(text.contains("gate=PASS"), "{text}");
        assert!(text.contains("exit=0"), "{text}");
        assert!(g.pass);
    }
}
