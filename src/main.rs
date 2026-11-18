#![windows_subsystem = "windows"]
//! Tsubame: a very fast Windows file browser (U2: the browsable skeleton).
//! Raw Win32 window with its own title bar, Direct2D drawing, and the
//! `tsubame-ui` kit on top. See `docs/u2-views.md` for the code structure.
//!
//! `tsubame.exe [path] [--dir <path>] [--theme dark|light] [--density compact|comfortable|spacious]`
//!   path / --dir  folder to open (default: the profile folder)
//! Measurement and test modes (hidden):
//!   --bench       first paint + scroll paint times; writes `s1-bench.txt`
//!                 (env TSUBAME_BENCH_OUT renames it), exits. Without --dir the
//!                 list holds 1M synthetic rows. Also: env TSUBAME_BENCH=1
//!     --windowed  keep the 1120x720 DIP window instead of maximizing
//!     --hang      hang every shell worker for 20 s when the scroll starts
//!   --timing      navigation timings (first rows, complete+sorted), writes
//!                 `u2-timing.txt`, exits. `--timing-dirs "A;B"` the folders
//!                 (the first one is also the start folder), `--reps N` per folder
//!   --leak-smoke  no window: 200 navigations + 50 tab open/close, writes
//!                 `u2-leak.txt` (private bytes before/after)
//!   --leak-gate   the full gate in this window: 10,000 navigations + 1,000
//!                 tab open/close over a fixture tree, counters sampled every
//!                 1,000 ops and after a 5 s settle; writes `leak-gate.txt`
//!                 (env TSUBAME_LEAK_OUT renames it) and exits 0 on pass, 1 on
//!                 fail. `--gate-fixture DIR` the folder tree (default: the
//!                 start folder), `--gate-navs N`, `--gate-tabs N`,
//!                 `--gate-sample N` (ops per counter sample, 1,000),
//!                 `--gate-settle S` (idle seconds before the end sample, 5)
//!   --stress m    S4 stress run (menu: 200 context menus; thumbs: 500 thumbnails)
//!   --workers N   shell STA workers (default 2)
//! Env: TSUBAME_ICON_PER_PATH=1 asks every real file's icon by path (worst
//! case), TSUBAME_ICON_FACTORY=1 uses IShellItemImageFactory for per-path
//! icons, TSUBAME_HANG_MS sets the hung-worker threshold (default 3000).

mod bench;
mod clip;
mod cols;
mod data;
mod leak;
mod lru;
mod memdiag;
mod nav;
mod shellui;
mod sidebar;
mod stress;
mod tab;
mod tabs;
mod timing;
mod view;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tsubame_d2d::Gfx;
use tsubame_ui::{Button, Cursor, Density, Event, Key, Mods, Pt, Ui};
use tsubame_win::places;
use tsubame_win::shell::{Pool, Request};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

/// Shell pool replies are ready (posted by a worker, at most one in flight).
const WM_APP_SHELL: u32 = WM_APP + 1;
/// An enumerator delivered a batch or finished (coalesced: at most one in flight).
const WM_APP_WAKE: u32 = WM_APP + 2;
/// The drives / known folders worker finished.
const WM_APP_PLACES: u32 = WM_APP + 3;
/// Hung-worker watchdog: armed only while the pool has icon/thumbnail work.
const WATCH_TIMER: usize = 2;
/// Caret blink: exists only while a text field has focus.
const CARET_TIMER: usize = 4;
const CARET_MS: u32 = 530;
/// The blink stops (caret solid) after this long without input.
const CARET_IDLE: Duration = Duration::from_secs(10);
/// not exported by the windows crate features we use
const WM_MOUSELEAVE: u32 = 0x02A3;
const WATCH_MS: u32 = 1000;
const HANG_MS: u32 = 20_000;
/// Resize border of the frameless window, DIPs.
const RESIZE_BORDER: f32 = 6.0;

#[cfg(feature = "memdiag")]
#[global_allocator]
static ALLOC: memdiag::counting::Counting = memdiag::counting::Counting;

static WAKE_PENDING: AtomicBool = AtomicBool::new(false);
static PLACES_BUSY: AtomicBool = AtomicBool::new(false);
static PLACES: Mutex<Option<(Vec<places::Drive>, Vec<places::Place>)>> = Mutex::new(None);

struct App {
    view: view::View,
    ui: Ui,
    /// we hold the mouse capture for a kit drag
    captured: bool,
    /// WM_MOUSELEAVE is armed (TrackMouseEvent)
    tracking: bool,
    /// first half of a UTF-16 surrogate pair from WM_CHAR
    hi_surrogate: Option<u16>,
    cursor_kind: Cursor,
    gfx: Gfx,
    bench: Option<bench::Bench>,
    timing: Option<timing::Run>,
    /// `--leak-gate`: the full in-window leak gate
    gate: Option<leak::Gate>,
    first_done: bool,
    pool: Pool,
    shell: shellui::ShellUi,
    stress: Option<stress::Stress>,
    hang_bench: bool,
    watch_armed: bool,
    /// the blink timer exists
    caret_timer: bool,
    /// the blink stopped for lack of input (caret solid until the next key)
    caret_idle: bool,
    last_input: Instant,
    /// start of the last paint (streaming listings repaint at most ~60/s)
    last_paint: Instant,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state. Never held across a Win32 call that can
/// re-enter the window procedure.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

/// The system "apps" theme: dark unless it says light.
fn system_dark() -> bool {
    let mut v = 1u32;
    let mut sz = size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut sz),
        )
    };
    r == ERROR_SUCCESS && v == 0
}

fn parse_density(s: &str) -> Option<Density> {
    match s.to_ascii_lowercase().as_str() {
        "compact" => Some(Density::Compact),
        "comfortable" => Some(Density::Comfortable),
        "spacious" => Some(Density::Spacious),
        _ => None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |n: &str| args.iter().any(|a| a == n);
    let bench_mode = flag("--bench") || std::env::var_os("TSUBAME_BENCH").is_some_and(|v| v != "0");
    let timing_mode = flag("--timing");
    let gate_mode = flag("--leak-gate");
    let stress_mode = arg_value(&args, "--stress").map(|m| if m == "thumbs" { stress::Mode::Thumbs } else { stress::Mode::Menu });
    let workers = arg_value(&args, "--workers").and_then(|v| v.parse().ok()).unwrap_or(2usize);
    let timing_dirs: Vec<String> = arg_value(&args, "--timing-dirs").map(|s| s.split(';').filter(|p| !p.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    // first bare argument that is not a flag value is the folder to open
    const VALUE_FLAGS: [&str; 12] = ["--dir", "--theme", "--density", "--stress", "--workers", "--timing-dirs", "--reps", "--gate-fixture", "--gate-navs", "--gate-tabs", "--gate-sample", "--gate-settle"];
    let positional = args.iter().enumerate().skip(1).find(|(i, a)| !a.starts_with("--") && !VALUE_FLAGS.contains(&args[i - 1].as_str())).map(|(_, a)| a.clone());
    let explicit_dir = arg_value(&args, "--dir").or(positional);
    let synthetic = bench_mode && explicit_dir.is_none();
    let start = timing_dirs.first().cloned().or(explicit_dir.clone()).unwrap_or_else(places::home_folder);
    let dark = match arg_value(&args, "--theme").as_deref() {
        Some("light") => false,
        Some("dark") => true,
        _ => system_dark(),
    };
    let density = arg_value(&args, "--density").and_then(|d| parse_density(&d)).unwrap_or(Density::Compact);

    if flag("--leak-smoke") {
        let text = leak::run(&start);
        bench::write_text_report("u2-leak.txt", &text);
        return;
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let class = w!("TsubameMain");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            style: CS_DBLCLKS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let scale = GetDpiForSystem() as f32 / 96.0;
        let (w, h) = ((1120.0 * scale) as i32, (720.0 * scale) as i32);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Tsubame"), WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, w, h, None, None, Some(hinst), None).unwrap();

        // no WS_CAPTION: the kit draws the title bar (WM_NCCALCSIZE gives it the whole window)
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        let mut gfx = Gfx::new(hwnd).unwrap();
        let dpi = GetDpiForWindow(hwnd) as f32;
        gfx.set_dpi(dpi);

        // shell workers: all COM and disk work happens there
        let mut pool = Pool::new(workers, hwnd.0 as isize, WM_APP_SHELL);
        if let Some(ms) = std::env::var("TSUBAME_HANG_MS").ok().and_then(|v| v.parse().ok()) {
            pool.hang_after = Duration::from_millis(ms);
        }
        let mut shell = shellui::ShellUi::new(icon_px(dpi));
        shell.collect_other = stress_mode == Some(stress::Mode::Thumbs);

        // enumerator workers wake the UI thread with one coalesced message
        let hw = hwnd.0 as isize;
        let wake: tsubame_win::enumerate::Wake = Arc::new(move || {
            if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
                let _ = PostMessageW(Some(HWND(hw as *mut _)), WM_APP_WAKE, WPARAM(0), LPARAM(0));
            }
        });

        let mut view = view::View::new(&start, wake, dark, density);
        let mut bench_state = None;
        if synthetic {
            let t = Instant::now();
            let store = data::synthetic_store(1_000_000);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let bytes = store.memory_bytes();
            view.tab_mut().adopt_store("Synthetic\\1,000,000 rows", store);
            bench_state = Some(bench::Bench::new("synthetic 1M-entry EntryStore", Some(ms), bytes));
        } else if bench_mode {
            bench_state = Some(bench::Bench::new("real folder (enumerator)", None, 0));
        }

        let mut ui = Ui::new(view.base_theme());
        view::bind_shortcuts(&mut ui);
        ui.clipboard = Box::new(clip::WinClipboard(hwnd.0 as isize));
        ui.set_focus(Some(view::ID_LIST));
        view.zebra = flag("--zebra");

        let timing = timing_mode.then(|| timing::Run::new(if timing_dirs.is_empty() { vec![start.clone()] } else { timing_dirs.clone() }, arg_value(&args, "--reps").and_then(|v| v.parse().ok()).unwrap_or(3)));
        let gate = gate_mode.then(|| {
            leak::Gate::new(
                arg_value(&args, "--gate-fixture").as_deref().unwrap_or(&start),
                arg_value(&args, "--gate-navs").and_then(|v| v.parse().ok()).unwrap_or(10_000),
                arg_value(&args, "--gate-tabs").and_then(|v| v.parse().ok()).unwrap_or(1_000),
                arg_value(&args, "--gate-sample").and_then(|v| v.parse().ok()).unwrap_or(1_000),
                std::time::Duration::from_secs(arg_value(&args, "--gate-settle").and_then(|v| v.parse().ok()).unwrap_or(5)),
            )
        });

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                view,
                ui,
                captured: false,
                tracking: false,
                hi_surrogate: None,
                cursor_kind: Cursor::Arrow,
                gfx,
                bench: bench_state,
                timing,
                gate,
                first_done: false,
                pool,
                shell,
                stress: stress_mode.map(stress::Stress::new),
                hang_bench: flag("--hang"),
                watch_armed: false,
                caret_timer: false,
                caret_idle: false,
                last_input: Instant::now(),
                last_paint: Instant::now(),
            })
        });

        // bench and timing runs measure at full screen (4K on rig)
        let maximize = (bench_mode || timing_mode || gate_mode) && !flag("--windowed");
        let _ = ShowWindow(hwnd, if maximize { SW_SHOWMAXIMIZED } else { SW_SHOW });

        // Idle = blocked here. Timers only while a bench/stress run, the
        // hung-worker watchdog (pool busy) or a focused text field's caret needs them.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // drop D2D/D3D objects now, while the process is fully alive (blink: a
        // thread-local destructor run inside ExitProcess crashes in D3D teardown)
        let taken = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.take()));
        drop(taken);
        // the leak gate's verdict is the process exit code: 0 pass, 1 fail
        if leak::GATE_FAILED.load(Ordering::Acquire) {
            std::process::exit(1);
        }
    }
}

fn icon_px(dpi: f32) -> u32 {
    (view::ICON_DIP * dpi / 96.0).round() as u32
}

fn lo(lp: LPARAM) -> i32 {
    (lp.0 & 0xffff) as i16 as i32
}
fn hi(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xffff) as i16 as i32
}

fn invalidate(hwnd: HWND) {
    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
}

/// Arms the hung-worker watchdog while icon/thumbnail work is in flight,
/// disarms it when the pool is idle (an idle app keeps zero timers).
fn arm_watch(hwnd: HWND, a: &mut App) {
    let busy = a.pool.busy();
    if busy && !a.watch_armed {
        unsafe { SetTimer(Some(hwnd), WATCH_TIMER, WATCH_MS, None) };
        a.watch_armed = true;
    } else if !busy && a.watch_armed {
        let _ = unsafe { KillTimer(Some(hwnd), WATCH_TIMER) };
        a.watch_armed = false;
    }
}

/// The caret rule: one blink timer exists only while a text field has
/// focus; it stops after `CARET_IDLE` without input (caret stays solid);
/// with no text field focused there is no timer at all.
fn sync_caret(hwnd: HWND, a: &mut App, text_focus: bool) {
    if text_focus && !a.caret_timer && !a.caret_idle {
        a.last_input = Instant::now();
        a.ui.caret_on = true;
        unsafe { SetTimer(Some(hwnd), CARET_TIMER, CARET_MS, None) };
        a.caret_timer = true;
    } else if !text_focus {
        if a.caret_timer {
            let _ = unsafe { KillTimer(Some(hwnd), CARET_TIMER) };
            a.caret_timer = false;
        }
        a.caret_idle = false;
        a.ui.caret_on = true;
    }
}

/// Keyboard / click input restarts a stopped blink and shows the caret.
fn note_input(a: &mut App) {
    a.last_input = Instant::now();
    a.caret_idle = false;
    a.ui.caret_on = true;
}

/// Reads drives and known folders on a worker (they can block on a dead
/// network share or an empty card reader); WM_APP_PLACES brings the result.
fn spawn_places(hwnd: HWND) {
    if PLACES_BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let h = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("tsubame-places".into()).spawn(move || {
        let r = (places::list_drives(), places::known_folders());
        if let Ok(mut g) = PLACES.lock() {
            *g = Some(r);
        }
        PLACES_BUSY.store(false, Ordering::Release);
        let _ = unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_APP_PLACES, WPARAM(0), LPARAM(0)) };
    });
}

/// After a paint: submit icon misses, cancel rows scrolled away, start the
/// drive scan when the folder moved to another volume.
fn after_paint(hwnd: HWND, a: &mut App) {
    a.shell.end_frame(&mut a.pool);
    arm_watch(hwnd, a);
    if a.view.want_drives && !PLACES_BUSY.load(Ordering::Acquire) {
        a.view.want_drives = false;
        spawn_places(hwnd);
    }
}

unsafe fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    unsafe { BeginPaint(hwnd, &mut ps) };
    let info = with(|a| {
        let t0 = Instant::now();
        a.last_paint = t0;
        a.shell.begin_frame();
        let out = a.view.paint(&mut a.ui, &mut a.gfx, &mut a.shell);
        let total = t0.elapsed().as_secs_f32() * 1000.0;
        after_paint(hwnd, a);
        if let Some(g) = a.gate.as_mut() {
            g.painted(a.gfx.path());
        }
        if let Some(s) = a.stress.as_mut() {
            s.painted(total);
        }
        // timing of the navigation: a frame with rows / the final frame was presented
        let final_frame = out.is_some() && a.view.tab_mut().frame_presented();
        (total, a.gfx.end_ms, a.gfx.path(), out, final_frame)
    });
    let _ = unsafe { EndPaint(hwnd, &ps) };
    let Some((total_ms, end_ms, path, out, final_frame)) = info else { return };
    if let Some(out) = out {
        after_frame(hwnd, out);
    }

    // first completed paint: time from process start
    let first = with(|a| {
        if a.first_done {
            return None;
        }
        a.first_done = true;
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let ms = bench::ms_since_process_start();
        let create_ms = a.gfx.create_ms;
        if let Some(b) = a.bench.as_mut() {
            b.first_paint(path, (rc.right, rc.bottom), a.gfx.dpi);
            b.first_frame = format!("first_frame: paint_start_ms={:.1} total_ms={total_ms:.1} create_ms={create_ms:.1} enddraw_ms={end_ms:.1}", ms - total_ms as f64);
        }
        if let Some(t) = a.timing.as_mut() {
            t.first_paint(ms, bench::memory().0);
        }
        if let Some(g) = a.gate.as_mut() {
            g.env((rc.right, rc.bottom), a.gfx.dpi, a.gfx.path());
        }
        Some(ms)
    })
    .flatten();
    if first.is_some() {
        if with(|a| a.bench.is_some()).unwrap_or(false) {
            unsafe { SetTimer(Some(hwnd), bench::TIMER_ID, bench::TICK_MS, None) };
        }
        if with(|a| a.stress.is_some()).unwrap_or(false) {
            unsafe { SetTimer(Some(hwnd), stress::TIMER_ID, stress::TICK_MS, None) };
        }
        // the gate is driven by paints and shell/enumerator messages; the
        // timer only keeps a lost wake from stalling it
        if with(|a| a.gate.is_some()).unwrap_or(false) {
            unsafe { SetTimer(Some(hwnd), leak::TIMER_ID, leak::TICK_MS, None) };
        }
    } else {
        with(|a| {
            if let Some(b) = a.bench.as_mut() {
                b.painted(total_ms, end_ms, path);
            }
        });
    }
    if final_frame {
        timing_step(hwnd);
    }
    // the leak gate advances on every completed paint
    if with(|a| a.gate.is_some()).unwrap_or(false) {
        on_gate(hwnd);
    }
}

/// The final frame of a navigation was presented: the timing run records it
/// and starts the next one, or writes the report and closes the window.
fn timing_step(hwnd: HWND) {
    let done = with(|a| {
        let App { view, timing, .. } = a;
        let t = timing.as_mut()?;
        if !t.next(view) {
            return Some(false);
        }
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let text = t.report((rc.right, rc.bottom), a.gfx.dpi, a.gfx.path(), bench::memory().0);
        bench::write_text_report("u2-timing.txt", &text);
        Some(true)
    })
    .flatten();
    match done {
        Some(true) => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Some(false) => invalidate(hwnd),
        None => {}
    }
}

/// One `--leak-gate` step. Never blocks and never dispatches messages: the
/// gate moves on paints, enumerator wakes, shell replies and its timer.
fn on_gate(hwnd: HWND) {
    let r = with(|a| {
        let App { gate, view, shell, pool, .. } = a;
        gate.as_mut().map(|g| g.step(view, shell, pool))
    })
    .flatten();
    match r {
        Some(leak::Step::Repaint) => invalidate(hwnd),
        Some(leak::Step::Done) => unsafe {
            let _ = KillTimer(Some(hwnd), leak::TIMER_ID);
            // WM_CLOSE, not DestroyWindow: this can run inside a paint
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        _ => {}
    }
}

unsafe fn on_bench_tick(hwnd: HWND) {
    let r = with(|a| {
        let b = a.bench.as_mut()?;
        let r = b.tick(a.view.tab_mut());
        if b.just_started {
            b.just_started = false;
            if a.hang_bench {
                for _ in 0..a.pool.workers() {
                    a.pool.submit(Request::Hang { ms: HANG_MS });
                }
                a.shell.menu_status = format!("hung {} workers for {} s", a.pool.workers(), HANG_MS / 1000);
            }
        }
        Some(r)
    })
    .flatten();
    let Some((done, repaint)) = r else { return };
    if done {
        let _ = unsafe { KillTimer(Some(hwnd), bench::TIMER_ID) };
        with(|a| {
            let ps = a.pool.stats();
            let mut extra = a.shell.stats.report();
            extra.push_str(&format!(
                "pool: workers {} submitted {} replies {} skipped(cancel flag) {} workers_replaced {} leaked_threads_still_hung {}\n",
                a.pool.workers(),
                ps.submitted,
                ps.replies,
                ps.skipped_cancelled,
                ps.workers_replaced,
                a.pool.leaked_alive()
            ));
            extra.push_str(&format!("icon cache: {} entries, {:.2} MB, evictions {}\n", a.shell.icons.len(), a.shell.icons.bytes() as f64 / 1048576.0, a.shell.icons.evictions));
            let (layouts, formats, bitmaps) = a.gfx.cache_counts();
            extra.push_str(&format!("gfx caches: {layouts} layouts, {formats} formats, {bitmaps} bitmaps\n"));
            if a.hang_bench {
                extra.push_str(&format!("hang test: {} workers hung {} ms each at scroll start; threshold {} ms\n", a.pool.workers(), HANG_MS, a.pool.hang_after.as_millis()));
            }
            if let Some(b) = a.bench.as_mut() {
                b.extra = extra;
                b.write_report();
            }
        });
        let _ = unsafe { DestroyWindow(hwnd) };
    } else if repaint {
        invalidate(hwnd);
    }
}

fn screen_pt(hwnd: HWND, x_px: i32, y_px: i32) -> (i32, i32) {
    let mut p = POINT { x: x_px, y: y_px };
    let _ = unsafe { ClientToScreen(hwnd, &mut p) };
    (p.x, p.y)
}

fn window_center(hwnd: HWND) -> (i32, i32) {
    let mut rc = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    screen_pt(hwnd, rc.right / 3, rc.bottom / 3)
}

unsafe fn on_stress_tick(hwnd: HWND) {
    let pt = window_center(hwnd);
    let r = with(|a| {
        let App { view, shell, pool, stress, .. } = a;
        stress.as_mut().map(|s| s.step(view.tab(), shell, pool, pt))
    })
    .flatten();
    match r {
        Some((true, _)) => {
            let _ = unsafe { KillTimer(Some(hwnd), stress::TIMER_ID) };
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Some((false, true)) => invalidate(hwnd),
        _ => {}
    }
}

/// Context menu for view row `row`, at client px (x, y).
fn open_menu(hwnd: HWND, row: usize, x: i32, y: i32) {
    let extended = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let (sx, sy) = screen_pt(hwnd, x, y);
    with(|a| {
        let paths = a.view.menu_paths(row);
        if paths.is_empty() {
            return;
        }
        a.shell.open_menu(&mut a.pool, paths, sx, sy, extended, None);
    });
    invalidate(hwnd);
}

/// Keyboard-invoked menu, at the cursor row.
fn key_menu(hwnd: HWND) {
    let p = with(|a| {
        let s = a.gfx.dpi / 96.0;
        let t = a.view.tab_mut();
        let row = t.list.cursor;
        if t.shown() > 0 && !t.store.at(row).is_some_and(|id| t.sel.contains(id)) {
            t.sel.single(&t.store, row);
        }
        (row, (60.0 * s) as i32, (a.view.row_mid_y(row) * s) as i32)
    });
    if let Some((row, x, y)) = p {
        open_menu(hwnd, row, x, y);
    }
}

/// What the kit and the view asked for after a frame: mouse capture, the IME
/// position, the shell menu, files to open, window commands, another frame,
/// and the caret timer.
fn after_frame(hwnd: HWND, out: tsubame_ui::Output) {
    let r = with(|a| {
        a.cursor_kind = out.cursor;
        let cap = (out.capture != a.captured).then(|| {
            a.captured = out.capture;
            out.capture
        });
        let s = a.gfx.dpi / 96.0;
        let ime = out.ime_caret.map(|r| ((r.x * s) as i32, (r.y * s) as i32, (r.h * s) as i32));
        let menu = a.view.menu_request.take().map(|(row, pt)| match pt {
            Some(p) => (row, (p.x * s) as i32, (p.y * s) as i32),
            None => (row, (60.0 * s) as i32, (a.view.row_mid_y(row) * s) as i32),
        });
        sync_caret(hwnd, a, out.text_focus);
        let open = std::mem::take(&mut a.view.open_paths);
        (cap, ime, menu, open, a.view.win_cmd.take(), a.view.quit)
    });
    let Some((cap, ime, menu, open, win_cmd, quit)) = r else { return };
    match cap {
        Some(true) => unsafe {
            SetCapture(hwnd);
        },
        Some(false) => unsafe {
            let _ = ReleaseCapture();
        },
        None => {}
    }
    if let Some((x, y, h)) = ime {
        clip::place_ime(hwnd, x, y, h);
    }
    if let Some((row, x, y)) = menu {
        open_menu(hwnd, row, x, y);
    }
    for p in open {
        places::open_async(p);
    }
    match win_cmd {
        Some(view::WinCmd::Minimize) => unsafe {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        },
        Some(view::WinCmd::MaxRestore) => unsafe {
            let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
        },
        Some(view::WinCmd::Close) => unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        None => {}
    }
    if quit {
        let _ = unsafe { DestroyWindow(hwnd) };
        return;
    }
    if out.repaint {
        invalidate(hwnd);
    }
}

/// Feeds one event to the kit; paints only if it can change the screen.
fn push(hwnd: HWND, ev: Event) {
    let typed = matches!(ev, Event::KeyDown { .. } | Event::Char(_) | Event::MouseDown { .. });
    if with(|a| {
        if typed {
            note_input(a);
        }
        a.ui.push(ev)
    })
    .unwrap_or(false)
    {
        invalidate(hwnd);
    }
}

fn mods() -> Mods {
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
    Mods { ctrl: down(VK_CONTROL), shift: down(VK_SHIFT), alt: down(VK_MENU) }
}

fn dpi_scale() -> f32 {
    with(|a| a.gfx.dpi / 96.0).unwrap_or(1.0)
}

/// Client px in `lp` to DIPs.
fn dip_pt(lp: LPARAM) -> Pt {
    let s = dpi_scale();
    Pt::new(lo(lp) as f32 / s, hi(lp) as f32 / s)
}

fn map_key(vk: u16) -> Key {
    match VIRTUAL_KEY(vk) {
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_TAB => Key::Tab,
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_INSERT => Key::Insert,
        VK_SPACE => Key::Space,
        v if (VK_F1.0..=VK_F24.0).contains(&v.0) => Key::F((v.0 - VK_F1.0 + 1) as u8),
        v if (0x30..=0x39).contains(&v.0) || (0x41..=0x5a).contains(&v.0) => Key::Char(v.0 as u8 as char),
        v => Key::Other(v.0 as u32),
    }
}

fn button_event(hwnd: HWND, msg: u32, lp: LPARAM) {
    let pos = dip_pt(lp);
    let mods = mods();
    let down = |button, double| Event::MouseDown { pos, button, mods, double };
    let ev = match msg {
        WM_LBUTTONDOWN => down(Button::Left, false),
        WM_LBUTTONDBLCLK => down(Button::Left, true),
        WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => down(Button::Right, false),
        WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => down(Button::Middle, false),
        WM_LBUTTONUP => Event::MouseUp { pos, button: Button::Left, mods },
        WM_RBUTTONUP => Event::MouseUp { pos, button: Button::Right, mods },
        _ => Event::MouseUp { pos, button: Button::Middle, mods },
    };
    push(hwnd, ev);
}

/// Frameless window: where a point (screen px) counts as caption or border.
fn hit_test(hwnd: HWND, lp: LPARAM) -> LRESULT {
    let mut p = POINT { x: lo(lp), y: hi(lp) };
    let mut rc = RECT::default();
    unsafe {
        let _ = ScreenToClient(hwnd, &mut p);
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let zoomed = unsafe { IsZoomed(hwnd).as_bool() };
    let r = with(|a| {
        let s = a.gfx.dpi / 96.0;
        let pt = Pt::new(p.x as f32 / s, p.y as f32 / s);
        let caption = pt.y < a.ui.theme.height_titlebar && !a.ui.over_widget(pt);
        (s, caption)
    });
    let Some((s, caption)) = r else { return LRESULT(HTCLIENT as isize) };
    let b = (RESIZE_BORDER * s) as i32;
    if !zoomed {
        let (l, t, rr, bb) = (p.x < b, p.y < b, p.x >= rc.right - b, p.y >= rc.bottom - b);
        let code = match (l, t, rr, bb) {
            (true, true, _, _) => HTTOPLEFT,
            (_, true, true, _) => HTTOPRIGHT,
            (true, _, _, true) => HTBOTTOMLEFT,
            (_, _, true, true) => HTBOTTOMRIGHT,
            (true, ..) => HTLEFT,
            (_, _, true, _) => HTRIGHT,
            (_, true, ..) => HTTOP,
            (_, _, _, true) => HTBOTTOM,
            _ => 0,
        };
        if code != 0 {
            return LRESULT(code as isize);
        }
    }
    LRESULT(if caption { HTCAPTION } else { HTCLIENT } as isize)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                on_paint(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            // no system title bar or frame: the client area is the whole window
            WM_NCCALCSIZE if wp.0 != 0 => {
                if IsZoomed(hwnd).as_bool() {
                    // maximized: the window overhangs the monitor by the frame; pull it in
                    let p = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
                    let dpi = GetDpiForWindow(hwnd);
                    let pad = GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                    let (fx, fy) = (GetSystemMetricsForDpi(SM_CXFRAME, dpi) + pad, GetSystemMetricsForDpi(SM_CYFRAME, dpi) + pad);
                    p.rgrc[0].left += fx;
                    p.rgrc[0].right -= fx;
                    p.rgrc[0].top += fy;
                    p.rgrc[0].bottom -= fy;
                }
                LRESULT(0)
            }
            WM_NCHITTEST => hit_test(hwnd, lp),
            WM_GETMINMAXINFO => {
                let s = dpi_scale();
                let m = &mut *(lp.0 as *mut MINMAXINFO);
                m.ptMinTrackSize = POINT { x: (640.0 * s) as i32, y: (400.0 * s) as i32 };
                LRESULT(0)
            }
            WM_SIZE => {
                let (w, h) = (lo(lp).max(1) as u32, hi(lp).max(1) as u32);
                let zoomed = wp.0 as u32 == SIZE_MAXIMIZED;
                with(|a| {
                    a.gfx.resize(w, h);
                    a.view.maximized = zoomed;
                });
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wp.0 & 0xffff) as f32;
                with(|a| {
                    a.gfx.set_dpi(dpi);
                    a.shell.set_icon_px(icon_px(dpi));
                });
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_APP_SHELL => {
                let pt = window_center(hwnd);
                let r = with(|a| {
                    let mut repaint = false;
                    for r in a.pool.drain() {
                        repaint |= a.shell.apply(r);
                    }
                    let st = {
                        let App { view, shell, pool, stress, .. } = &mut *a;
                        stress.as_mut().map(|s| s.step(view.tab(), shell, pool, pt))
                    };
                    arm_watch(hwnd, a);
                    (repaint, st)
                });
                if let Some((repaint, st)) = r {
                    if let Some((true, _)) = st {
                        let _ = KillTimer(Some(hwnd), stress::TIMER_ID);
                        let _ = DestroyWindow(hwnd);
                        return LRESULT(0);
                    }
                    if repaint {
                        invalidate(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_APP_WAKE => {
                // clear first: a worker that posts after this point gets a new message
                WAKE_PENDING.store(false, Ordering::Release);
                // WM_PAINT is lowest priority: a worker that keeps posting wakes would
                // starve it until the listing is done. Paint right away (at most
                // ~60 frames/s) so the first rows and the streaming show at once.
                let r = with(|a| (a.view.pump(), a.view.pump_more, (a.view.tab().timing.first_frame.is_none() && a.view.tab().shown() > 0) || a.last_paint.elapsed() >= Duration::from_millis(16)));
                if let Some((_, true, _)) = r {
                    // budget used up with batches still queued: come back after the paint
                    if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
                        let _ = PostMessageW(Some(hwnd), WM_APP_WAKE, WPARAM(0), LPARAM(0));
                    }
                }
                if let Some((true, _, stale)) = r {
                    invalidate(hwnd);
                    if stale {
                        let _ = UpdateWindow(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_APP_PLACES => {
                let got = PLACES.lock().ok().and_then(|mut g| g.take());
                if let Some((drives, places)) = got {
                    with(|a| {
                        a.view.side.drives = drives;
                        a.view.side.places = places;
                    });
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let dy = ((wp.0 >> 16) as u16 as i16) as f32 / 120.0;
                // wheel positions are screen px
                let mut p = POINT { x: lo(lp), y: hi(lp) };
                let _ = ScreenToClient(hwnd, &mut p);
                let s = dpi_scale();
                push(hwnd, Event::Wheel { pos: Pt::new(p.x as f32 / s, p.y as f32 / s), dy, dx: 0.0, mods: mods() });
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONDBLCLK | WM_MBUTTONDOWN | WM_MBUTTONDBLCLK | WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                button_event(hwnd, msg, lp);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if with(|a| !std::mem::replace(&mut a.tracking, true)).unwrap_or(false) {
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                }
                push(hwnd, Event::MouseMove { pos: dip_pt(lp) });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with(|a| a.tracking = false);
                push(hwnd, Event::MouseLeave);
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                // another window took a capture we still wanted: end the drag
                if with(|a| std::mem::replace(&mut a.captured, false)).unwrap_or(false) {
                    let pos = with(|a| a.ui.mouse()).unwrap_or_default();
                    push(hwnd, Event::MouseUp { pos, button: Button::Left, mods: Mods::NONE });
                }
                LRESULT(0)
            }
            WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
                let id = match with(|a| a.cursor_kind).unwrap_or(Cursor::Arrow) {
                    Cursor::Arrow => IDC_ARROW,
                    Cursor::IBeam => IDC_IBEAM,
                    Cursor::Hand => IDC_HAND,
                    Cursor::SizeWE => IDC_SIZEWE,
                    Cursor::SizeNS => IDC_SIZENS,
                };
                if let Ok(c) = LoadCursorW(None, id) {
                    SetCursor(Some(c));
                }
                LRESULT(1)
            }
            WM_SETFOCUS => {
                push(hwnd, Event::FocusGained);
                LRESULT(0)
            }
            WM_KILLFOCUS => {
                push(hwnd, Event::FocusLost);
                LRESULT(0)
            }
            // the text field draws the composition inline: no IME window
            WM_IME_STARTCOMPOSITION => LRESULT(0),
            WM_IME_COMPOSITION => {
                if let Some(s) = clip::ime_string(hwnd, lp, false) {
                    with(|a| a.ui.set_composition(""));
                    for c in s.chars() {
                        push(hwnd, Event::Char(c));
                    }
                    invalidate(hwnd);
                }
                if let Some(s) = clip::ime_string(hwnd, lp, true)
                    && with(|a| a.ui.set_composition(&s)).unwrap_or(false) {
                        invalidate(hwnd);
                    }
                LRESULT(0)
            }
            WM_IME_ENDCOMPOSITION => {
                if with(|a| a.ui.set_composition("")).unwrap_or(false) {
                    invalidate(hwnd);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_CONTEXTMENU if lp.0 == -1 => {
                // keyboard (menu key): at the cursor row
                key_menu(hwnd);
                LRESULT(0)
            }
            WM_SYSKEYDOWN | WM_SYSKEYUP if wp.0 as u16 == VK_F10.0 => {
                // F10 must not reach DefWindowProc: it enters menu mode on the
                // UI thread, and while any menu is active the worker's
                // SetForegroundWindow fails, so Escape never reaches its menu.
                if msg == WM_SYSKEYDOWN && GetKeyState(VK_SHIFT.0 as i32) < 0 {
                    key_menu(hwnd);
                }
                LRESULT(0)
            }
            // Alt+key shortcuts (Alt+Left/Right/Up, Alt+D) arrive as SYSKEY messages
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let repeat = lp.0 & (1 << 30) != 0;
                let key = map_key(wp.0 as u16);
                // bare Alt (menu activation) is ignored; every other Alt combo goes to the kit
                if matches!(VIRTUAL_KEY(wp.0 as u16), VK_MENU | VK_LMENU | VK_RMENU) {
                    return LRESULT(0);
                }
                push(hwnd, Event::KeyDown { key, mods: mods(), repeat });
                LRESULT(0)
            }
            WM_KEYUP | WM_SYSKEYUP => {
                push(hwnd, Event::KeyUp { key: map_key(wp.0 as u16), mods: mods() });
                LRESULT(0)
            }
            // text as characters (injected VK_PACKET input and every layout
            // arrive here); a surrogate pair comes as two messages
            WM_CHAR => {
                let u = wp.0 as u16;
                let ch = if (0xd800..0xdc00).contains(&u) {
                    with(|a| a.hi_surrogate = Some(u));
                    None
                } else if (0xdc00..0xe000).contains(&u) {
                    with(|a| a.hi_surrogate.take()).flatten().and_then(|h| char::decode_utf16([h, u]).next()?.ok())
                } else {
                    char::from_u32(u as u32)
                };
                // control chars (backspace, tab, enter, ctrl+letters) come as KeyDown
                if let Some(c) = ch.filter(|c| !c.is_control()) {
                    push(hwnd, Event::Char(c));
                }
                LRESULT(0)
            }
            WM_TIMER if wp.0 == bench::TIMER_ID => {
                on_bench_tick(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == stress::TIMER_ID => {
                on_stress_tick(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == leak::TIMER_ID => {
                on_gate(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == CARET_TIMER => {
                with(|a| {
                    if a.last_input.elapsed() >= CARET_IDLE {
                        // no input for a while: stop the blink, caret stays solid
                        let _ = KillTimer(Some(hwnd), CARET_TIMER);
                        a.caret_timer = false;
                        a.caret_idle = true;
                        a.ui.caret_on = true;
                    } else {
                        a.ui.caret_on = !a.ui.caret_on;
                    }
                });
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == WATCH_TIMER => {
                let lost = with(|a| {
                    let lost = a.pool.check_hung();
                    if !lost.is_empty() {
                        a.shell.forget(&lost);
                        a.shell.menu_status = format!("watchdog: replaced {} hung worker(s), {} total", lost.len(), a.pool.stats().workers_replaced);
                    }
                    arm_watch(hwnd, a);
                    !lost.is_empty()
                });
                if lost == Some(true) {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
