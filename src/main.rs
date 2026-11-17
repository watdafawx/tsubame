#![windows_subsystem = "windows"]
//! S1 + S4 spike: a details-view list on Direct2D, fed by shell workers.
//!
//! `tsubame.exe [--dir <path>] [--store] [--bench [--hang]] [--stress menu|thumbs] [--workers N]`
//!   --dir <path>  list a real folder (on a worker thread); rows get real
//!                 shell icons; right-click opens the real shell context menu
//!                 (Shift: extended verbs), Ctrl+click multi-selects, T shows
//!                 a thumbnail of the selected item, H hangs a worker for 20 s,
//!                 W opens a menu owned by the UI window (wrong-owner test)
//!   --store       1M fake rows from a compact array store (else procedural)
//!   --windowed    with --bench: keep the 1120x720 DIP window instead of maximizing
//!   --bench       measure first paint + scroll paint times (icons streaming
//!                 with --dir), write `s1-bench.txt` (env TSUBAME_BENCH_OUT
//!                 renames it), exit. Also: env TSUBAME_BENCH=1
//!   --hang        with --bench: hang every shell worker for 20 s when the
//!                 scroll starts (hung-worker replacement test)
//!   --stress m    S4 stress run (menu: 200 context menus; thumbs: 500
//!                 thumbnails), writes `s4-stress-<m>.txt`, exits
//!   --workers N   shell STA workers (default 2)
//! Env: TSUBAME_ICON_PER_PATH=1 asks every real file's icon by path (worst
//! case), TSUBAME_ICON_FACTORY=1 uses IShellItemImageFactory for per-path
//! icons, TSUBAME_HANG_MS sets the hung-worker threshold (default 3000).

mod bench;
mod data;
mod lru;
mod shellui;
mod stress;
mod view;

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;
use tsubame_d2d::Gfx;
use tsubame_win::shell::{Pool, Request};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, VK_F10, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

/// Shell pool replies are ready (posted by a worker, at most one in flight).
const WM_APP_SHELL: u32 = WM_APP + 1;
/// The folder listing finished.
const WM_APP_DIR: u32 = WM_APP + 2;
/// Hung-worker watchdog: armed only while the pool has icon/thumbnail work.
const WATCH_TIMER: usize = 2;
const WATCH_MS: u32 = 1000;
const HANG_MS: u32 = 20_000;

struct App {
    view: view::View,
    gfx: Gfx,
    bench: Option<bench::Bench>,
    first_done: bool,
    pool: Pool,
    shell: shellui::ShellUi,
    dir_rx: Option<Receiver<std::result::Result<data::DirStore, String>>>,
    stress: Option<stress::Stress>,
    hang_bench: bool,
    watch_armed: bool,
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bench_mode = args.iter().any(|a| a == "--bench") || std::env::var_os("TSUBAME_BENCH").is_some_and(|v| v != "0");
    let use_store = args.iter().any(|a| a == "--store");
    let dir = arg_value(&args, "--dir");
    let stress_mode = arg_value(&args, "--stress").map(|m| if m == "thumbs" { stress::Mode::Thumbs } else { stress::Mode::Menu });
    let workers = arg_value(&args, "--workers").and_then(|v| v.parse().ok()).unwrap_or(2usize);

    let (source, store_ms, store_bytes) = if let Some(d) = &dir {
        (data::Source::Loading(d.clone()), None, 0)
    } else if use_store {
        let t = Instant::now();
        let s = data::Store::build();
        let bytes = s.bytes();
        (data::Source::Store(s), Some(t.elapsed().as_secs_f64() * 1000.0), bytes)
    } else {
        (data::Source::Procedural, None, 0)
    };
    let label = source.label();

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let class = w!("TsubameS1");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let scale = GetDpiForSystem() as f32 / 96.0;
        let (w, h) = ((1120.0 * scale) as i32, (720.0 * scale) as i32);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Tsubame S4 spike"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, w, h, None, None, Some(hinst), None).unwrap();

        let mut gfx = Gfx::new(hwnd).unwrap();
        let dpi = GetDpiForWindow(hwnd) as f32;
        gfx.set_dpi(dpi);
        let bench = bench_mode.then(|| bench::Bench::new(label, store_ms, store_bytes));

        // shell workers: all COM and disk work happens there
        let mut pool = Pool::new(workers, hwnd.0 as isize, WM_APP_SHELL);
        if let Some(ms) = std::env::var("TSUBAME_HANG_MS").ok().and_then(|v| v.parse().ok()) {
            pool.hang_after = std::time::Duration::from_millis(ms);
        }
        let mut shell = shellui::ShellUi::new(icon_px(dpi));
        shell.collect_other = stress_mode == Some(stress::Mode::Thumbs);

        // the folder listing runs on its own thread too
        let dir_rx = dir.map(|d| {
            let (tx, rx) = channel();
            let h = hwnd.0 as isize;
            std::thread::spawn(move || {
                let _ = tx.send(data::DirStore::load(&d));
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_DIR, WPARAM(0), LPARAM(0));
            });
            rx
        });

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                view: view::View::new(source),
                gfx,
                bench,
                first_done: false,
                pool,
                shell,
                dir_rx,
                stress: stress_mode.map(stress::Stress::new),
                hang_bench: args.iter().any(|a| a == "--hang"),
                watch_armed: false,
            })
        });

        // bench measures at full screen (4K on rig); normal runs open 1120x720 DIP
        let maximize = bench_mode && !args.iter().any(|a| a == "--windowed");
        let _ = ShowWindow(hwnd, if maximize { SW_SHOWMAXIMIZED } else { SW_SHOW });

        // Idle = blocked here. Timers only while a bench/stress run or the
        // hung-worker watchdog (pool busy) needs them.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // drop D2D/D3D objects now, while the process is fully alive (blink: a
        // thread-local destructor run inside ExitProcess crashes in D3D teardown)
        let taken = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.take()));
        drop(taken);
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

/// After a paint: submit icon misses, cancel rows scrolled away, follow the
/// selection with the thumbnail panel.
fn after_paint(hwnd: HWND, a: &mut App) {
    a.shell.end_frame(&mut a.pool);
    if a.shell.thumbs.on {
        let sel = a.view.selected;
        let path = a.view.source.path(sel);
        let px = (view::THUMB_DIP * a.gfx.dpi / 96.0) as u32;
        a.shell.want_thumb(&mut a.pool, sel, path, px);
    }
    arm_watch(hwnd, a);
}

unsafe fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    unsafe { BeginPaint(hwnd, &mut ps) };
    let info = with(|a| {
        let t0 = Instant::now();
        a.shell.begin_frame();
        a.view.paint(&mut a.gfx, &mut a.shell);
        let total = t0.elapsed().as_secs_f32() * 1000.0;
        after_paint(hwnd, a);
        if let Some(s) = a.stress.as_mut() {
            s.painted(total);
        }
        (total, a.gfx.end_ms, a.gfx.path())
    });
    let _ = unsafe { EndPaint(hwnd, &ps) };
    let Some((total_ms, end_ms, path)) = info else { return };

    // first completed paint: time from process start
    let first = with(|a| {
        if a.first_done {
            return None;
        }
        a.first_done = true;
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let ms = bench::ms_since_process_start();
        if let Some(b) = a.bench.as_mut() {
            b.first_paint(path, (rc.right, rc.bottom), a.gfx.dpi);
        }
        Some(ms)
    })
    .flatten();
    if let Some(ms) = first {
        let title = format!("Tsubame S4 spike - first paint {ms:.0} ms\0");
        let w: Vec<u16> = title.encode_utf16().collect();
        let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(w.as_ptr())) };
        if with(|a| a.bench.is_some()).unwrap_or(false) {
            unsafe { SetTimer(Some(hwnd), bench::TIMER_ID, bench::TICK_MS, None) };
        }
        if with(|a| a.stress.is_some()).unwrap_or(false) {
            unsafe { SetTimer(Some(hwnd), stress::TIMER_ID, stress::TICK_MS, None) };
        }
    } else {
        with(|a| {
            if let Some(b) = a.bench.as_mut() {
                b.painted(total_ms, end_ms, path);
            }
        });
    }
}

unsafe fn on_bench_tick(hwnd: HWND) {
    let r = with(|a| {
        let b = a.bench.as_mut()?;
        let r = b.tick(&mut a.view);
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
        stress.as_mut().map(|s| s.step(view, shell, pool, pt))
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

/// Context menu for the row at client px (x, y), at that screen point.
fn menu_at(hwnd: HWND, x: i32, y: i32, wrong_owner: bool) {
    let extended = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let (sx, sy) = screen_pt(hwnd, x, y);
    with(|a| {
        let s = a.gfx.dpi / 96.0;
        let Some(row) = a.view.row_at(x as f32 / s, y as f32 / s) else { return };
        if !a.view.marked.contains(&row) {
            a.view.marked.clear();
            a.view.selected = row;
        }
        let paths = a.view.menu_paths(row);
        if paths.is_empty() {
            a.shell.menu_status = "fake rows have no file: run with --dir <path>".into();
            return;
        }
        a.shell.open_menu(&mut a.pool, paths, sx, sy, extended, wrong_owner.then_some(hwnd.0 as isize));
    });
    invalidate(hwnd);
}

/// Keyboard-invoked menu, at the selected row.
fn key_menu(hwnd: HWND, wrong_owner: bool) {
    let p = with(|a| {
        let s = a.gfx.dpi / 96.0;
        ((60.0 * s) as i32, (a.view.row_mid_y(a.view.selected) * s) as i32)
    });
    if let Some((x, y)) = p {
        menu_at(hwnd, x, y, wrong_owner);
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                on_paint(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                let (w, h) = (lo(lp).max(1) as u32, hi(lp).max(1) as u32);
                with(|a| a.gfx.resize(w, h));
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
                        stress.as_mut().map(|s| s.step(view, shell, pool, pt))
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
            WM_APP_DIR => {
                with(|a| {
                    if let Some(rx) = a.dir_rx.take() {
                        match rx.try_recv() {
                            Ok(Ok(d)) => {
                                a.shell.menu_status = format!("listed {} entries in {:.1} ms on a worker ({:.2} MB store)", d.len(), d.list_ms, d.bytes() as f64 / 1048576.0);
                                a.view.source = data::Source::Dir(d);
                                a.view.selected = 0;
                                a.view.scroll = 0.0;
                            }
                            Ok(Err(e)) => a.shell.menu_status = format!("listing failed: {e}"),
                            Err(_) => a.dir_rx = Some(rx),
                        }
                    }
                });
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let notches = ((wp.0 >> 16) as u16 as i16) as f32 / 120.0;
                if with(|a| a.view.wheel(notches)).unwrap_or(false) {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let ctrl = wp.0 & 0x0008 != 0; // MK_CONTROL
                let changed = with(|a| {
                    let s = a.gfx.dpi / 96.0;
                    let (x, y) = (lo(lp) as f32 / s, hi(lp) as f32 / s);
                    let ch = if ctrl { a.view.toggle_mark(x, y) } else { a.view.mouse_down(x, y) };
                    (ch, a.view.dragging())
                });
                if let Some((ch, drag)) = changed {
                    if drag {
                        SetCapture(hwnd);
                    }
                    if ch {
                        invalidate(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                menu_at(hwnd, lo(lp), hi(lp), false);
                LRESULT(0)
            }
            WM_CONTEXTMENU if lp.0 == -1 => {
                // keyboard (menu key): at the selected row
                key_menu(hwnd, false);
                LRESULT(0)
            }
            WM_SYSKEYDOWN | WM_SYSKEYUP if wp.0 as u16 == VK_F10.0 => {
                // F10 must not reach DefWindowProc: it enters menu mode on the
                // UI thread, and while any menu is active the worker's
                // SetForegroundWindow fails, so Escape never reaches its menu.
                if msg == WM_SYSKEYDOWN && GetKeyState(VK_SHIFT.0 as i32) < 0 {
                    key_menu(hwnd, false);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if with(|a| {
                    let s = a.gfx.dpi / 96.0;
                    a.view.mouse_move(hi(lp) as f32 / s)
                })
                .unwrap_or(false)
                {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP | WM_CAPTURECHANGED => {
                if with(|a| a.view.mouse_up()).unwrap_or(false) {
                    if msg == WM_LBUTTONUP {
                        let _ = ReleaseCapture();
                    }
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if with(|a| a.view.key(wp.0 as u16)).unwrap_or(false) {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            // letter commands as characters: injected input (VK_PACKET) and
            // other layouts arrive here, not as a 'T' virtual key
            WM_CHAR => {
                match char::from_u32(wp.0 as u32).map(|c| c.to_ascii_lowercase()) {
                    Some('t') => {
                        // thumbnail panel
                        with(|a| {
                            a.shell.thumbs.on = !a.shell.thumbs.on;
                            a.shell.thumbs.row = None;
                        });
                        invalidate(hwnd);
                    }
                    Some('h') => {
                        // hang one worker in a "shell call"
                        with(|a| {
                            a.pool.submit(Request::Hang { ms: HANG_MS });
                            a.shell.menu_status = format!("a worker sleeps {} s; watchdog replaces it after {} ms", HANG_MS / 1000, a.pool.hang_after.as_millis());
                            arm_watch(hwnd, a);
                        });
                        invalidate(hwnd);
                    }
                    Some('w') => {
                        // wrong-owner menu (owner = this UI window, on another thread)
                        key_menu(hwnd, true);
                    }
                    _ => {}
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
