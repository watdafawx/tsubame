#![windows_subsystem = "windows"]
//! S1 spike: a details-view list of 1,000,000 fake rows on Direct2D.
//!
//! `tsubame.exe [--store] [--bench]`
//!   --store  rows come from a compact array store (name buffer + 24-byte
//!            records) instead of being computed per frame
//!   --windowed  with --bench: keep the 1120x720 DIP window instead of maximizing
//!   --bench  measure first paint + scroll paint times, write `s1-bench.txt`, exit
//!            (also: env TSUBAME_BENCH=1)

mod bench;
mod data;
mod view;

use std::cell::RefCell;
use std::time::Instant;
use tsubame_d2d::Gfx;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

struct App {
    view: view::View,
    gfx: Gfx,
    bench: Option<bench::Bench>,
    first_done: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state. Never held across a Win32 call that can
/// re-enter the window procedure.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bench_mode = args.iter().any(|a| a == "--bench") || std::env::var_os("TSUBAME_BENCH").is_some_and(|v| v != "0");
    let use_store = args.iter().any(|a| a == "--store");

    let (source, store_ms, store_bytes) = if use_store {
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
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Tsubame S1 spike"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, w, h, None, None, Some(hinst), None).unwrap();

        let mut gfx = Gfx::new(hwnd).unwrap();
        gfx.set_dpi(GetDpiForWindow(hwnd) as f32);
        let bench = bench_mode.then(|| bench::Bench::new(label, store_ms, store_bytes));
        APP.with(|a| *a.borrow_mut() = Some(App { view: view::View::new(source), gfx, bench, first_done: false }));

        // bench measures at full screen (4K on rig); normal runs open 1120x720 DIP
        let maximize = bench_mode && !args.iter().any(|a| a == "--windowed");
        let _ = ShowWindow(hwnd, if maximize { SW_SHOWMAXIMIZED } else { SW_SHOW });

        // Idle = blocked here. No timers except the bench-only frame ticker.
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

fn lo(lp: LPARAM) -> i32 {
    (lp.0 & 0xffff) as i16 as i32
}
fn hi(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xffff) as i16 as i32
}

unsafe fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    unsafe { BeginPaint(hwnd, &mut ps) };
    let info = with(|a| {
        let t0 = Instant::now();
        a.view.paint(&mut a.gfx);
        let total = t0.elapsed().as_secs_f32() * 1000.0;
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
        let title = format!("Tsubame S1 spike - first paint {ms:.0} ms\0");
        let w: Vec<u16> = title.encode_utf16().collect();
        let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(w.as_ptr())) };
        if with(|a| a.bench.is_some()).unwrap_or(false) {
            unsafe { SetTimer(Some(hwnd), bench::TIMER_ID, bench::TICK_MS, None) };
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
    let r = with(|a| a.bench.as_mut().map(|b| b.tick(&mut a.view))).flatten();
    let Some((done, repaint)) = r else { return };
    if done {
        let _ = unsafe { KillTimer(Some(hwnd), bench::TIMER_ID) };
        with(|a| {
            if let Some(b) = a.bench.as_ref() {
                b.write_report();
            }
        });
        let _ = unsafe { DestroyWindow(hwnd) };
    } else if repaint {
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
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
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wp.0 & 0xffff) as f32;
                with(|a| a.gfx.set_dpi(dpi));
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let notches = ((wp.0 >> 16) as u16 as i16) as f32 / 120.0;
                if with(|a| a.view.wheel(notches)).unwrap_or(false) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let changed = with(|a| {
                    let s = a.gfx.dpi / 96.0;
                    let ch = a.view.mouse_down(lo(lp) as f32 / s, hi(lp) as f32 / s);
                    (ch, a.view.dragging())
                });
                if let Some((ch, drag)) = changed {
                    if drag {
                        SetCapture(hwnd);
                    }
                    if ch {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
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
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP | WM_CAPTURECHANGED => {
                if with(|a| a.view.mouse_up()).unwrap_or(false) {
                    if msg == WM_LBUTTONUP {
                        let _ = ReleaseCapture();
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if with(|a| a.view.key(wp.0 as u16)).unwrap_or(false) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_TIMER if wp.0 == bench::TIMER_ID => {
                on_bench_tick(hwnd);
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
