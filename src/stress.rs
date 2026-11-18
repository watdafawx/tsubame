//! S4 stress runs (`--stress menu|thumbs`, with `--dir <path>`). Each one
//! drives the shell pool from the UI thread, measures process counters
//! before and after, writes `s4-stress-<mode>.txt` next to the exe and
//! closes the window.
//!
//! - menu: 200 context menus opened and closed by the worker itself
//!   (`auto_close`: `EndMenu` on the first `WM_ENTERIDLE`), alternating one
//!   and three items. Then one menu held open for 2 s while the UI thread
//!   repaints every 10 ms, to show the menu does not block it.
//! - thumbs: thumbnails for up to 500 image files of the folder, all queued
//!   at once; cached vs generated counts and times.

use crate::shellui::{ShellUi, pcts};
use crate::tab::Tab;
use std::time::{Duration, Instant};
use tsubame_win::procinfo::{Counters, counters};
use tsubame_win::shell::{Payload, Pool, Request};

pub const TIMER_ID: usize = 3;
pub const TICK_MS: u32 = 10;

const MENUS: usize = 200;
const THUMBS: usize = 500;
const THUMB_PX: u32 = 512;
const WARMUP: Duration = Duration::from_secs(3);
const SETTLE: Duration = Duration::from_secs(3);
const IMAGE_EXT: [&str; 9] = ["jpg", "jpeg", "png", "bmp", "gif", "tif", "tiff", "webp", "heic"];

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Menu,
    Thumbs,
}

enum State {
    Wait(Instant),
    MenuRun { i: usize },
    MenuHold,
    Thumbs { left: usize },
    Settle(Instant),
    Done,
}

pub struct Stress {
    pub mode: Mode,
    state: State,
    pt: (i32, i32),
    paths: Vec<String>,
    c: Vec<(String, Counters)>,
    popup_us: Vec<u64>,
    query_us: Vec<u64>,
    bind_us: Vec<u64>,
    menu_total_us: Vec<u64>,
    asked: Option<Instant>,
    menu_errors: Vec<String>,
    not_shown: usize,
    hold_paints: Vec<f32>,
    hold: Option<(Instant, f64)>,
    // thumbs
    t_cached: Vec<u64>,
    t_generated: Vec<u64>,
    t_probe_miss: Vec<u64>,
    t_failed: usize,
    t_errors: Vec<String>,
    t_bytes: usize,
    t0: Instant,
    log: Vec<String>,
}

impl Stress {
    pub fn new(mode: Mode) -> Stress {
        Stress {
            mode,
            state: State::Wait(Instant::now() + WARMUP),
            pt: (200, 200),
            paths: Vec::new(),
            c: Vec::new(),
            popup_us: Vec::new(),
            query_us: Vec::new(),
            bind_us: Vec::new(),
            menu_total_us: Vec::new(),
            asked: None,
            menu_errors: Vec::new(),
            not_shown: 0,
            hold_paints: Vec::new(),
            hold: None,
            t_cached: Vec::new(),
            t_generated: Vec::new(),
            t_probe_miss: Vec::new(),
            t_failed: 0,
            t_errors: Vec::new(),
            t_bytes: 0,
            t0: Instant::now(),
            log: Vec::new(),
        }
    }

    fn snap(&mut self, label: &str) {
        self.c.push((label.to_string(), counters()));
    }

    /// The UI thread painted (ms): recorded while a menu is held open.
    pub fn painted(&mut self, ms: f32) {
        if matches!(self.state, State::MenuHold) {
            self.hold_paints.push(ms);
        }
    }

    fn submit_menu(&mut self, pool: &mut Pool, i: usize, hold_ms: Option<u32>) {
        let n = if i.is_multiple_of(2) { 1 } else { 3.min(self.paths.len()) };
        let paths = self.paths[..n].to_vec();
        pool.submit(Request::Menu { paths, x: self.pt.0, y: self.pt.1, extended: false, auto_close: Some(hold_ms.unwrap_or(0)), owner: None, opts: Default::default() });
        self.asked = Some(Instant::now());
    }

    /// Timer tick and after every drained batch of replies. `pt` is a screen
    /// point inside the window. Returns (done, repaint).
    pub fn step(&mut self, tab: &Tab, shell: &mut ShellUi, pool: &mut Pool, pt: (i32, i32)) -> (bool, bool) {
        self.pt = pt;
        match self.state {
            State::Wait(t) => {
                if Instant::now() < t || tab.shown() == 0 || tab.is_loading() {
                    return (false, false);
                }
                if tab.synthetic {
                    self.log.push("needs a real folder (--dir <path>)".into());
                    self.state = State::Settle(Instant::now());
                    return (false, false);
                }
                match self.mode {
                    Mode::Menu => {
                        self.paths = (0..tab.shown()).filter_map(|i| tab.entry_at(i)).filter(|e| !e.1).take(3).map(|e| e.0).collect();
                        if self.paths.is_empty() {
                            self.log.push("no files in the folder".into());
                            self.state = State::Settle(Instant::now());
                            return (false, false);
                        }
                        self.snap("start (before the first menu)");
                        self.t0 = Instant::now();
                        self.submit_menu(pool, 0, None);
                        self.state = State::MenuRun { i: 0 };
                    }
                    Mode::Thumbs => {
                        self.paths = (0..tab.shown())
                            .filter_map(|i| tab.entry_at(i))
                            .filter(|(p, dir)| !dir && p.rsplit_once('.').is_some_and(|(_, e)| IMAGE_EXT.iter().any(|x| x.eq_ignore_ascii_case(e))))
                            .take(THUMBS)
                            .map(|e| e.0)
                            .collect();
                        self.snap("start (before the first thumbnail)");
                        self.t0 = Instant::now();
                        let px = THUMB_PX;
                        for p in &self.paths {
                            pool.submit(Request::Thumb { path: p.clone(), size: px, cache_only: false });
                        }
                        self.state = State::Thumbs { left: self.paths.len() };
                    }
                }
                (false, false)
            }
            State::MenuRun { i } => {
                let Some((m, _)) = shell.last_menu.take() else { return (false, false) };
                self.bind_us.push(m.bind_us);
                self.query_us.push(m.query_us);
                if let Some(t) = self.asked.take() {
                    self.menu_total_us.push(t.elapsed().as_micros() as u64);
                }
                match m.popup_us {
                    Some(u) => self.popup_us.push(u),
                    None => self.not_shown += 1,
                }
                if let Some(e) = m.error
                    && self.menu_errors.len() < 5
                {
                    self.menu_errors.push(format!("menu {i}: {e}"));
                }
                let i = i + 1;
                if i == 1 {
                    self.snap("after menu 1 (shell extensions loaded)");
                } else if i == 10 {
                    self.snap("after menu 10");
                } else if i == 100 {
                    self.snap("after menu 100");
                }
                if i < MENUS {
                    self.submit_menu(pool, i, None);
                    self.state = State::MenuRun { i };
                } else {
                    self.snap("after menu 200");
                    self.log.push(format!("{MENUS} menus in {:.1} s", self.t0.elapsed().as_secs_f64()));
                    self.submit_menu(pool, 0, Some(2000));
                    self.hold = Some((Instant::now(), 0.0));
                    self.state = State::MenuHold;
                }
                (false, true)
            }
            State::MenuHold => {
                if let Some((m, _)) = shell.last_menu.take() {
                    let held = self.hold.map(|(t, _)| t.elapsed().as_secs_f64()).unwrap_or(0.0);
                    self.log.push(format!("held menu: shown {} for {:.2} s; UI thread painted {} frames meanwhile: {}", m.shown, held, self.hold_paints.len(), stats_ms(&self.hold_paints)));
                    self.state = State::Settle(Instant::now() + SETTLE);
                    return (false, false);
                }
                // keep the UI thread painting while the worker shows the menu
                (false, true)
            }
            State::Thumbs { left } => {
                let mut left = left;
                for r in shell.other_replies.drain(..) {
                    left = left.saturating_sub(1);
                    if let Payload::Thumb { bmp, from_cache, probe_us, error } = r.payload {
                        match bmp {
                            Some(b) => {
                                self.t_bytes += b.bytes();
                                if from_cache {
                                    self.t_cached.push(r.work_us);
                                } else {
                                    self.t_generated.push(r.work_us);
                                    self.t_probe_miss.push(probe_us);
                                }
                            }
                            None => {
                                self.t_failed += 1;
                                if let Some(e) = error
                                    && self.t_errors.len() < 5
                                {
                                    self.t_errors.push(e);
                                }
                            }
                        }
                    }
                }
                if left == 0 {
                    self.log.push(format!("{} thumbnails in {:.1} s", self.paths.len(), self.t0.elapsed().as_secs_f64()));
                    self.snap("after all thumbnails (bitmaps dropped)");
                    self.state = State::Settle(Instant::now() + SETTLE);
                } else {
                    self.state = State::Thumbs { left };
                }
                (false, false)
            }
            State::Settle(t) => {
                if Instant::now() >= t {
                    self.snap("end (after settle)");
                    self.state = State::Done;
                    self.write();
                    return (true, false);
                }
                (false, false)
            }
            State::Done => (true, false),
        }
    }

    fn report(&self) -> String {
        let mut o = String::new();
        let mut line = |s: String| {
            o.push_str(&s);
            o.push('\n');
        };
        line(format!("tsubame S4 stress: {}", if self.mode == Mode::Menu { "context menu" } else { "thumbnails" }));
        line(format!("build: {}", if cfg!(debug_assertions) { "debug" } else { "release" }));
        line(format!("items: {}", self.paths.len()));
        for l in &self.log {
            line(l.clone());
        }
        for (label, c) in &self.c {
            line(format!("counters {label}: {}", c.line()));
        }
        if let (Some((_, a)), Some((_, z))) = (self.c.first(), self.c.last()) {
            let warm = self.c.get(1).map(|(_, c)| *c).unwrap_or(*a);
            let pct = |x: usize, base: usize| (x as f64 - base as f64) / base.max(1) as f64 * 100.0;
            line(format!(
                "private bytes end vs start: {:+.1}% ({:+.1} MB); end vs after first item: {:+.1}% ({:+.1} MB)",
                pct(z.private_bytes, a.private_bytes),
                (z.private_bytes as f64 - a.private_bytes as f64) / 1048576.0,
                pct(z.private_bytes, warm.private_bytes),
                (z.private_bytes as f64 - warm.private_bytes as f64) / 1048576.0
            ));
            line(format!(
                "handles end-start {:+}, gdi end-start {:+}, user end-start {:+}; vs after first item: handles {:+}, gdi {:+}, user {:+}",
                z.handles as i64 - a.handles as i64,
                z.gdi as i64 - a.gdi as i64,
                z.user as i64 - a.user as i64,
                z.handles as i64 - warm.handles as i64,
                z.gdi as i64 - warm.gdi as i64,
                z.user as i64 - warm.user as i64
            ));
            // A leak is growth: private bytes may fall (the GPU swap transient
            // is trimmed during the run) without failing the gate.
            let grow = pct(z.private_bytes, a.private_bytes);
            let gate_total = grow <= 5.0;
            line(format!("leak gate A (private bytes end vs start, growth <= 5%): {grow:+.1}% {}", if gate_total { "PASS" } else { "FAIL" }));
            // per cycle: after the first ten items (handlers loaded, caches
            // warm) to the last item; a per-item leak shows up here
            let at = |label: &str| self.c.iter().find(|(l, _)| l.starts_with(label)).map(|(_, c)| *c);
            let mut gate = gate_total;
            if let (Some(b), Some(e)) = (at("after menu 10"), at("after menu 200")) {
                let (pp, ph) = (pct(e.private_bytes, b.private_bytes), pct(e.handles as usize, b.handles as usize));
                let ok = pp <= 5.0 && ph <= 5.0;
                gate &= ok;
                line(format!(
                    "leak gate B (menu 10 -> 200, 190 cycles, growth <= 5%): private {pp:+.1}%, handles {ph:+.1}% ({:+}), gdi {:+}, user {:+} {}",
                    e.handles as i64 - b.handles as i64,
                    e.gdi as i64 - b.gdi as i64,
                    e.user as i64 - b.user as i64,
                    if ok { "PASS" } else { "FAIL" }
                ));
            }
            line(format!("leak gate: {}", if gate { "PASS" } else { "FAIL" }));
        }
        match self.mode {
            Mode::Menu => {
                line(format!("menu on screen (job start to first WM_ENTERIDLE): {}", pcts(&self.popup_us)));
                line(format!("menu bind (parse + bind + GetUIObjectOf): {}", pcts(&self.bind_us)));
                line(format!("menu QueryContextMenu: {}", pcts(&self.query_us)));
                line(format!("menu round trip (UI submit to reply applied): {}", pcts(&self.menu_total_us)));
                if let Some(first) = self.popup_us.first() {
                    line(format!("first menu on screen (cold): {:.1} ms", *first as f64 / 1000.0));
                }
                line(format!("menus never shown: {}", self.not_shown));
                for e in &self.menu_errors {
                    line(format!("error: {e}"));
                }
            }
            Mode::Thumbs => {
                line(format!("thumbnail from cache (SIIGBF_INCACHEONLY hit): {}", pcts(&self.t_cached)));
                line(format!("thumbnail generated (probe miss + GetImage): {}", pcts(&self.t_generated)));
                line(format!("thumbnail cache probe that missed: {}", pcts(&self.t_probe_miss)));
                line(format!("thumbnail failed: {}", self.t_failed));
                line(format!("thumbnail pixels received: {:.1} MB (dropped right away)", self.t_bytes as f64 / 1048576.0));
                for e in &self.t_errors {
                    line(format!("error: {e}"));
                }
            }
        }
        o
    }

    fn write(&self) {
        let text = self.report();
        let name = format!("s4-stress-{}.txt", if self.mode == Mode::Menu { "menu" } else { "thumbs" });
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            let _ = std::fs::write(dir.join(name), &text);
        }
    }
}

fn stats_ms(v: &[f32]) -> String {
    let us: Vec<u64> = v.iter().map(|ms| (ms * 1000.0) as u64).collect();
    pcts(&us)
}
