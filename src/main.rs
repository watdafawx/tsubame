#![windows_subsystem = "windows"]
//! S1 + S4 spike on the `tsubame-ui` kit (U1): a details-view list on
//! Direct2D, fed by shell workers, with a filter box (Ctrl+F; Tab moves
//! the focus between the filter and the list).
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
mod clip;
mod data;
mod lru;
mod shellui;
mod stress;
mod view;

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;
use tsubame_d2d::Gfx;
use tsubame_ui::{Button, Cursor, Event, Key, Mods, Pt, Theme, Ui};
use tsubame_win::shell::{Pool, Request};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

/// Shell pool replies are ready (posted by a worker, at most one in flight).
const WM_APP_SHELL: u32 = WM_APP + 1;
/// The folder listing finished.
const WM_APP_DIR: u32 = WM_APP + 2;
/// Hung-worker watchdog: armed only while the pool has icon/thumbnail work.
const WATCH_TIMER: usize = 2;
/// not exported by the windows crate features we use
const WM_MOUSELEAVE: u32 = 0x02A3;
const WATCH_MS: u32 = 1000;
const HANG_MS: u32 = 20_000;

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
            style: CS_DBLCLKS,
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

        let mut ui = Ui::new(Theme::dark());
        ui.shortcuts.bind(Key::Char('F'), Mods::CTRL, view::CMD_FIND, true);
        ui.clipboard = Box::new(clip::WinClipboard(hwnd.0 as isize));
        ui.set_focus(Some(view::LIST_ID));

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                view: view::View::new(source),
                ui,
                captured: false,
                tracking: false,
                hi_surrogate: None,
                cursor_kind: Cursor::Arrow,
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
        let sel = a.view.cursor();
        let path = a.view.path(sel);
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
        let out = a.view.paint(&mut a.ui, &mut a.gfx, &mut a.shell);
        let total = t0.elapsed().as_secs_f32() * 1000.0;
        after_paint(hwnd, a);
        if let Some(s) = a.stress.as_mut() {
            s.painted(total);
        }
        (total, a.gfx.end_ms, a.gfx.path(), out)
    });
    let _ = unsafe { EndPaint(hwnd, &ps) };
    let Some((total_ms, end_ms, path, out)) = info else { return };
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

/// Context menu for view row `row`, at client px (x, y).
fn open_menu(hwnd: HWND, row: usize, x: i32, y: i32, wrong_owner: bool) {
    let extended = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let (sx, sy) = screen_pt(hwnd, x, y);
    with(|a| {
        let paths = a.view.menu_paths(row);
        if paths.is_empty() {
            a.shell.menu_status = "fake rows have no file: run with --dir <path>".into();
            return;
        }
        a.shell.open_menu(&mut a.pool, paths, sx, sy, extended, wrong_owner.then_some(hwnd.0 as isize));
    });
    invalidate(hwnd);
}

/// Keyboard-invoked menu, at the cursor row.
fn key_menu(hwnd: HWND, wrong_owner: bool) {
    let p = with(|a| {
        let s = a.gfx.dpi / 96.0;
        let row = a.view.cursor();
        if a.view.len() > 0 && !a.view.sel.contains(row) {
            a.view.sel.single(row);
        }
        (row, (60.0 * s) as i32, (a.view.row_mid_y(row) * s) as i32)
    });
    if let Some((row, x, y)) = p {
        open_menu(hwnd, row, x, y, wrong_owner);
    }
}

/// What the kit asked for after a frame: another frame, mouse capture, the
/// cursor, the IME position; then the S4 menu and the letter commands.
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
        (cap, ime, menu, view::View::leftover_chars(&a.ui))
    });
    let Some((cap, ime, menu, chars)) = r else { return };
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
        open_menu(hwnd, row, x, y, false);
    }
    for c in chars {
        on_letter(hwnd, c);
    }
    if out.repaint {
        invalidate(hwnd);
    }
}

/// Feeds one event to the kit; paints only if it can change the screen.
fn push(hwnd: HWND, ev: Event) {
    if with(|a| a.ui.push(ev)).unwrap_or(false) {
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

/// Letter commands (chars no widget took): T thumbnail panel, H hang a
/// worker in a "shell call", W wrong-owner menu (owner = this UI window).
fn on_letter(hwnd: HWND, c: char) {
    match c {
        't' => {
            with(|a| {
                a.shell.thumbs.on = !a.shell.thumbs.on;
                a.shell.thumbs.row = None;
            });
            invalidate(hwnd);
        }
        'h' => {
            with(|a| {
                a.pool.submit(Request::Hang { ms: HANG_MS });
                a.shell.menu_status = format!("a worker sleeps {} s; watchdog replaces it after {} ms", HANG_MS / 1000, a.pool.hang_after.as_millis());
                arm_watch(hwnd, a);
            });
            invalidate(hwnd);
        }
        'w' => key_menu(hwnd, true),
        _ => {}
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
                                a.view.set_source(data::Source::Dir(d));
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
                if let Some(s) = clip::ime_string(hwnd, lp, true) {
                    if with(|a| a.ui.set_composition(&s)).unwrap_or(false) {
                        invalidate(hwnd);
                    }
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
            WM_KEYDOWN => {
                let repeat = lp.0 & (1 << 30) != 0;
                push(hwnd, Event::KeyDown { key: map_key(wp.0 as u16), mods: mods(), repeat });
                LRESULT(0)
            }
            WM_KEYUP => {
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
