//! S4 stress runs (`--stress menu|thumbs`, with `--dir <path>`). Each one
//! drives the shell pool from the UI thread, measures process counters
//! before and after, writes `s4-stress-<mode>.txt` next to the exe and
//! closes the window.
//!
//! - menu: 200 context menus opened and closed by the worker itself
//!   (`auto_close`: `EndMenu` on the first `WM_ENTERIDLE`), alternating one
//!   and three items. Then one menu held open for 2 s while the UI thread
//!   repaints every 10 ms, to show the menu does not block it.
//! - clip: 200 cycles of a context menu with the app's entries and
//!   intercepted verbs (W1), then a clipboard copy/cut of the same files and a
//!   clipboard read, all on the shell workers. Writes `s4-stress-clip.txt`.
//! - thumbs: thumbnails for up to 500 image files of the folder, all queued
//!   at once; cached vs generated counts and times.
//! - ctxmenu: 200 own-drawn context menus (`ctxui`), alternating one and
//!   three items: each one opens, paints until every shell item is in (or the
//!   failure row shows) and is dropped. Open to first paint, to the shell's top
//!   level and to all items (lazy, CM6: the top level and the first lazy
//!   submenu, opened like a hover would; `TSUBAME_CTX_EAGER=1`: the whole tree),
//!   and the same counters as `menu`, to compare growth with the native path.

use crate::shellui::{ShellUi, pcts};
use crate::view::View;
use std::time::{Duration, Instant};
use tsubame_win::procinfo::{Counters, counters};
use tsubame_win::dnd::clipboard::{ClipJob, ClipReply};
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
    /// menu + clipboard cycles (W1)
    Clip,
    /// the own-drawn menu (CM2)
    CtxMenu,
}

enum State {
    Wait(Instant),
    MenuRun { i: usize },
    MenuHold,
    /// own-drawn menu `i`: opens it when none is up, else waits for its items
    CtxRun { i: usize },
    /// cycle `i`: waiting for the menu (`clip: 0`) or for the two clipboard replies
    ClipRun { i: usize, clip: usize },
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
    clip_us: Vec<u64>,
    clip_errors: Vec<String>,
    staging: std::path::PathBuf,
    memdiag: String,
    menus: usize,
    ctx_first_us: Vec<u64>,
    ctx_top_us: Vec<u64>,
    ctx_all_us: Vec<u64>,
    ctx_rows: Vec<u64>,
    /// packaged (IExplorerCommand) worker time per menu and its row count (CM7)
    ctx_pkg_us: Vec<u64>,
    ctx_pkg_rows: Vec<u64>,
    ctx_failed: usize,
    ctx_at: Option<Instant>,
    /// lazy submenus filled (CM6: one per menu, like a hover) and their
    /// request-to-answer times
    ctx_sub_us: Vec<u64>,
    ctx_sub_asked: bool,
}

impl Stress {
    pub fn new(mode: Mode) -> Stress {
        Stress {
            mode,
            ctx_first_us: Vec::new(),
            ctx_top_us: Vec::new(),
            ctx_all_us: Vec::new(),
            ctx_rows: Vec::new(),
            ctx_pkg_us: Vec::new(),
            ctx_pkg_rows: Vec::new(),
            ctx_failed: 0,
            ctx_at: None,
            ctx_sub_us: Vec::new(),
            ctx_sub_asked: false,
            memdiag: String::new(),
            menus: std::env::var("TSUBAME_STRESS_MENUS").ok().and_then(|s| s.parse().ok()).filter(|n| (200..=10000).contains(n)).unwrap_or(MENUS),
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
            clip_us: Vec::new(),
            clip_errors: Vec::new(),
            staging: std::env::temp_dir().join(format!("tsubame-stress-clip-{}", std::process::id())),
        }
    }

    fn snap(&mut self, label: &str) {
        self.c.push((label.to_string(), counters()));
        self.memdiag.push_str(&crate::memdiag::sample(label));
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
        let opts = if self.mode == Mode::Clip { crate::shellops::item_opts() } else { Default::default() };
        pool.submit(Request::Menu { paths, x: self.pt.0, y: self.pt.1, extended: false, auto_close: Some(hold_ms.unwrap_or(0)), owner: None, opts });
        self.asked = Some(Instant::now());
    }

    /// Timer tick and after every drained batch of replies. `pt` is a screen
    /// point inside the window. Returns (done, repaint).
    pub fn step(&mut self, view: &mut View, shell: &mut ShellUi, pool: &mut Pool, pt: (i32, i32)) -> (bool, bool) {
        let tab = view.tab();
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
                    Mode::Menu | Mode::Clip | Mode::CtxMenu => {
                        self.paths = (0..tab.shown()).filter_map(|i| tab.entry_at(i)).filter(|e| !e.1).take(3).map(|e| e.0).collect();
                        if self.paths.is_empty() {
                            self.log.push("no files in the folder".into());
                            self.state = State::Settle(Instant::now());
                            return (false, false);
                        }
                        self.snap("start (before the first menu)");
                        self.t0 = Instant::now();
                        if self.mode == Mode::CtxMenu {
                            self.state = State::CtxRun { i: 0 };
                        } else {
                            self.submit_menu(pool, 0, None);
                            self.state = if self.mode == Mode::Clip { State::ClipRun { i: 0, clip: 0 } } else { State::MenuRun { i: 0 } };
                        }
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
            State::CtxRun { i } => {
                if let Some(c) = view.ctx.as_mut() {
                    let in_time = self.ctx_at.is_some_and(|t| t.elapsed() < Duration::from_secs(6));
                    if c.first_paint.is_none() || (c.all_items.is_none() && c.model.error.is_none() && in_time) {
                        return (false, false);
                    }
                    // the lazy path: fill one submenu, as a hover would
                    if c.all_items.is_some() && in_time {
                        if !self.ctx_sub_asked {
                            self.ctx_sub_asked = true;
                            if c.expand_first() {
                                return (false, true);
                            }
                        } else if c.sub_pending() {
                            return (false, true);
                        }
                    }
                    if self.ctx_sub_asked
                        && let Some(t) = c.sub_latency
                    {
                        self.ctx_sub_us.push(t.as_micros() as u64);
                    }
                    self.ctx_sub_asked = false;
                    if let Some(t) = c.first_paint { self.ctx_first_us.push(t.as_micros() as u64); }
                    if let Some(t) = c.top_items { self.ctx_top_us.push(t.as_micros() as u64); }
                    if let Some(t) = c.all_items { self.ctx_all_us.push(t.as_micros() as u64); } else { self.ctx_failed += 1; }
                    self.ctx_rows.push(c.model.native.len() as u64);
                    if let Some(t) = c.model.packaged_us { self.ctx_pkg_us.push(t); }
                    self.ctx_pkg_rows.push(c.model.packaged.iter().filter(|i| !i.is_separator()).count() as u64);
                    view.ctx = None;
                    let done = i + 1;
                    if [1, 10, 100, self.menus].contains(&done) { self.snap(&format!("after menu {done}")); }
                    self.state = if done == self.menus { State::Settle(Instant::now() + SETTLE) } else { State::CtxRun { i: done } };
                } else {
                    let n = if i.is_multiple_of(2) { 1 } else { 3.min(self.paths.len()) };
                    let target = crate::ctxui::Target { paths: self.paths[..n].to_vec(), folder: view.tab().path.clone(), anchor: tsubame_ui::Pt::new(100.0, 100.0), screen: pt, ..Default::default() };
                    crate::ctxui::note_input();
                    view.ctx = Some(Box::new(crate::ctxui::CtxMenu::open(target, tsubame_config::menu::default_entries(), view.dual(), false, 32, pool)));
                    self.ctx_at = Some(Instant::now());
                }
                (false, true)
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
                if (i == 200 || i == 500) && i < self.menus {
                    self.snap(&format!("after menu {i}"));
                }
                if i < self.menus {
                    self.submit_menu(pool, i, None);
                    self.state = State::MenuRun { i };
                } else {
                    self.snap(&format!("after menu {}", self.menus));
                    self.log.push(format!("{} menus in {:.1} s", self.menus, self.t0.elapsed().as_secs_f64()));
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
            State::ClipRun { i, clip: 0 } => {
                let Some((m, _)) = shell.last_menu.take() else { return (false, false) };
                self.bind_us.push(m.bind_us);
                self.query_us.push(m.query_us);
                match m.popup_us {
                    Some(u) => self.popup_us.push(u),
                    None => self.not_shown += 1,
                }
                if let Some(e) = m.error
                    && self.menu_errors.len() < 5
                {
                    self.menu_errors.push(format!("menu {i}: {e}"));
                }
                // the same files to the clipboard (cut on odd cycles), then read back
                let n = if i.is_multiple_of(2) { 1 } else { 3.min(self.paths.len()) };
                let paths = self.paths[..n].iter().map(std::path::PathBuf::from).collect();
                // the read goes out when the set is done (two workers at once
                // would race for the clipboard)
                pool.submit(Request::Clip(ClipJob::SetFiles { paths, cut: !i.is_multiple_of(2) }));
                self.state = State::ClipRun { i, clip: 1 };
                (false, true)
            }
            State::ClipRun { i, clip } => {
                let mut got = clip - 1;
                for r in shell.other_replies.drain(..) {
                    if let Payload::Clip(c) = r.payload {
                        got += 1;
                        self.clip_us.push(r.work_us);
                        if matches!(c, ClipReply::Set(Ok(_))) {
                            pool.submit(Request::Clip(ClipJob::Read { staging: self.staging.clone(), cap: 1 << 20, consume_cut: false }));
                        } else if matches!(c, ClipReply::Set(Err(_))) {
                            // no read follows a failed set
                            got += 1;
                        }
                        let err = match c {
                            ClipReply::Set(Err(e)) | ClipReply::Read(Err(e)) => Some(e),
                            ClipReply::Read(Ok(rd)) if rd.info.paths.is_empty() => Some("read back no files".into()),
                            _ => None,
                        };
                        if let Some(e) = err
                            && self.clip_errors.len() < 5
                        {
                            self.clip_errors.push(format!("cycle {i}: {e}"));
                        }
                    }
                }
                if got < 2 {
                    self.state = State::ClipRun { i, clip: got + 1 };
                    return (false, false);
                }
                if let Some(t) = self.asked.take() {
                    self.menu_total_us.push(t.elapsed().as_micros() as u64);
                }
                let i = i + 1;
                if i == 1 {
                    self.snap("after menu 1 (shell extensions loaded)");
                } else if i == 10 {
                    self.snap("after menu 10");
                } else if i == 100 {
                    self.snap("after menu 100");
                }
                if (i == 200 || i == 500) && i < self.menus {
                    self.snap(&format!("after menu {i}"));
                }
                if i < self.menus {
                    self.submit_menu(pool, i, None);
                    self.state = State::ClipRun { i, clip: 0 };
                } else {
                    self.snap(&format!("after menu {}", self.menus));
                    self.log.push(format!("{} menu + clipboard cycles in {:.1} s", self.menus, self.t0.elapsed().as_secs_f64()));
                    let _ = std::fs::remove_dir_all(&self.staging);
                    self.state = State::Settle(Instant::now() + SETTLE);
                }
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
        line(format!("tsubame S4 stress: {}", self.mode.label()));
        line(format!("build: {}", if cfg!(debug_assertions) { "debug" } else { "release" }));
        line(format!("items: {}", self.paths.len()));
        line(self.memdiag.clone());
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
            if let (Some(b), Some(e)) = (at("after menu 10"), at(&format!("after menu {}", self.menus))) {
                let (pp, ph) = (pct(e.private_bytes, b.private_bytes), pct(e.handles as usize, b.handles as usize));
                let ok = pp <= 5.0 && ph <= 5.0;
                gate &= ok;
                line(format!(
                    "leak gate B (menu 10 -> {end}, {cycles} cycles, growth <= 5%): private {pp:+.1}%, handles {ph:+.1}% ({:+}), gdi {:+}, user {:+} {}",
                    e.handles as i64 - b.handles as i64,
                    e.gdi as i64 - b.gdi as i64,
                    e.user as i64 - b.user as i64,
                    if ok { "PASS" } else { "FAIL" }, end = self.menus, cycles = self.menus - 10
                ));
            }
            line(format!("leak gate: {}", if gate { "PASS" } else { "FAIL" }));
        }
        match self.mode {
            Mode::CtxMenu => {
                line(format!("own first paint: {}", pcts(&self.ctx_first_us)));
                line(format!("own top level: {}", pcts(&self.ctx_top_us)));
                line(format!("own all items: {}", pcts(&self.ctx_all_us)));
                line(format!("own warm first paint: {}", pcts(self.ctx_first_us.get(1..).unwrap_or_default())));
                line(format!("own warm top level: {}", pcts(self.ctx_top_us.get(1..).unwrap_or_default())));
                line(format!("own warm all items: {}", pcts(self.ctx_all_us.get(1..).unwrap_or_default())));
                line(format!("own packaged commands (worker us, {} menus, rows max {}): {}; warm: {}", self.ctx_pkg_us.len(), self.ctx_pkg_rows.iter().max().unwrap_or(&0), pcts(&self.ctx_pkg_us), pcts(self.ctx_pkg_us.get(1..).unwrap_or_default())));
                line(format!("own lazy submenu (request to answer, {} filled): {}", self.ctx_sub_us.len(), pcts(&self.ctx_sub_us)));
                line(format!("own cold first/top/all us: {:?}/{:?}/{:?}; failed: {}", self.ctx_first_us.first(), self.ctx_top_us.first(), self.ctx_all_us.first(), self.ctx_failed));
            }
            Mode::Menu | Mode::Clip => {
                if self.mode == Mode::Clip {
                    line(format!("clipboard jobs (worker time, set + read): {}", pcts(&self.clip_us)));
                    for e in &self.clip_errors {
                        line(format!("clipboard error: {e}"));
                    }
                }
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
        let name = format!("s4-stress-{}.txt", self.mode.name());
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            let _ = std::fs::write(dir.join(name), &text);
        }
    }
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Menu => "menu",
            Mode::Thumbs => "thumbs",
            Mode::Clip => "clip",
            Mode::CtxMenu => "ctxmenu",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Mode::Menu => "context menu",
            Mode::Thumbs => "thumbnails",
            Mode::Clip => "context menu + clipboard (W1)",
            Mode::CtxMenu => "own-drawn context menu",
        }
    }
}

fn stats_ms(v: &[f32]) -> String {
    let us: Vec<u64> = v.iter().map(|ms| (ms * 1000.0) as u64).collect();
    pcts(&us)
}
