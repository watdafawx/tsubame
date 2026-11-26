#![windows_subsystem = "windows"]
//! Tsubame: a very fast Windows file browser (U2: the browsable skeleton).
//! Raw Win32 window with its own title bar, Direct2D drawing, and the
//! `tsubame-ui` kit on top. See `docs/u2-views.md` for the code structure.
//!
//! `tsubame.exe [options] [path ...]`, see `docs/config.md` and `src/cli.rs`:
//!   <path>        a folder, or a file (opens its folder and selects it);
//!                 several paths open as tabs. `--new-tab`, `--new-window`,
//!                 `--select <path>`, `--portable`, `--config <file>`,
//!                 `--help`, `--version`.
//!   a second launch forwards its paths to the running instance (`src/single.rs`)
//! Settings and the session live in `%APPDATA%\Tsubame\` (or `.\data\` in
//! portable mode): `config.toml` and `session.toml` (`crates/tsubame-config`).
//! `--theme` and `--density` override the config for one run.
//!
//! Measurement and test modes (hidden, also `--dir <path>` for the folder):
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
//!   --stress m    S4 stress run (menu: 200 context menus; thumbs: 500 thumbnails;
//!                 clip: 200 menu + clipboard copy/read cycles)
//!   --workers N   shell STA workers for icons and menus (default 2)
//!   --thumb-workers N  STA workers for thumbnails (default 2)
//!   --view M      details (default), grid-small or grid-large
//! Dev modes never use the single instance and never write the config/session.
//! Env: TSUBAME_ICON_PER_PATH=1 asks every real file's icon by path (worst
//! case), TSUBAME_ICON_FACTORY=1 uses IShellItemImageFactory for per-path
//! icons, TSUBAME_HANG_MS sets the hung-worker threshold (default 3000),
//! TSUBAME_DATA_DIR overrides the config/session directory.

mod age;
mod archive_ui;
mod batchui;
mod bench;
mod buildinfo;
mod cli;
mod clip;
mod cols;
mod custom_cmd;
mod commands;
mod ctxui;
mod rowmenu;
mod data;
mod dnd;
mod editing;
mod fuzzy;
mod ext;
mod icon_overlay;
mod keymap;
mod leak;
mod lru;
mod memdiag;
mod meta_ui;
mod miller;
mod nav;
mod nav_menu;
mod newmenu;
mod ole_glue;
mod ops;
mod overlay;
mod packs;
mod palette;
mod panes;
mod preview_ui;
mod rename;
mod search_ui;
mod session;
mod settings;
mod shellops;
mod statusbar;
mod shellui;
mod sidebar;
mod single;
mod stress;
mod tab;
mod tabs;
mod tags;
mod tree;
mod timing;
mod uia_rows;
mod updates;
mod view;
mod view_tooltip;
mod wakelog;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tsubame_config::{Density as ConfigDensity, LoadStatus};
use tsubame_d2d::Gfx;
use tsubame_ui::{Button, Cursor, Density, Event, Key, Mods, Pt, Ui};
use tsubame_win::places;
use tsubame_win::shell::{Pool, Request};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Console::ATTACH_PARENT_PROCESS;
use windows::Win32::System::Console::AttachConsole;
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
/// A second launch forwarded paths (see `single`).
const WM_APP_OPEN: u32 = WM_APP + 4;
/// The updater changed state (check result, download progress): repaint Settings > Updates.
const WM_APP_UPDATE: u32 = WM_APP + 5;
/// The main window, for the updater's wake function (0 until it exists).
static UPDATE_HWND: AtomicIsize = AtomicIsize::new(0);
/// Hung-worker watchdog: armed only while the pool has icon/thumbnail work.
const WATCH_TIMER: usize = 2;
/// Caret blink: exists only while a text field has focus.
const CARET_TIMER: usize = 4;
/// One-shot config/session save while a change is inside the debounce window.
const SAVE_TIMER: usize = 6;
/// File-operation events: polled only while an operation runs.
const OPS_TIMER: usize = 8;
/// One-shot: lets the GPU device go once the window has not painted for
/// `gpu_park_after()` (`Gfx::park`: the NVIDIA driver wakes ~100/s while a
/// device exists). Armed only while a GPU target exists.
const PARK_TIMER: usize = 9;
/// One-shot, a moment after the first frame: loads the shell-extension DLLs
/// of the folder's context menus on a worker (`ctxui::prewarm`), so the first
/// right click is as fast as the later ones. Never repeats.
const CTX_WARM_TIMER: usize = 12;
const CTX_WARM_MS: u32 = 400;
const OPS_MS: u32 = 60;
const CARET_MS: u32 = 530;
/// The blink stops (caret solid) after this long without input.
const CARET_IDLE: Duration = Duration::from_secs(10);
/// not exported by the windows crate features we use
const WM_MOUSELEAVE: u32 = 0x02A3;
const APPCOMMAND_BROWSER_BACKWARD: u32 = 1;
const APPCOMMAND_BROWSER_FORWARD: u32 = 2;
const WATCH_MS: u32 = 1000;
const HANG_MS: u32 = 20_000;
/// Resize border of the frameless window, DIPs.
const RESIZE_BORDER: f32 = 6.0;

#[cfg(feature = "memdiag")]
#[global_allocator]
static ALLOC: memdiag::counting::Counting = memdiag::counting::Counting;

type PlacesResult = (Vec<places::Drive>, Vec<places::Place>, Vec<places::WslDistro>);

static WAKE_PENDING: AtomicBool = AtomicBool::new(false);
static PLACES_BUSY: AtomicBool = AtomicBool::new(false);
static PLACES: Mutex<Option<PlacesResult>> = Mutex::new(None);

struct App {
    view: view::View,
    ui: Ui,
    /// config + session files and their debounced writers
    saver: session::Saver,
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
    /// the own-drawn menu's reveal-grace / loading-line / hover timer interval
    ctx_timer: Option<u32>,
    /// the one-shot 600 ms row-tooltip dwell timer is armed for this hover sequence
    row_tip_timer: Option<u64>,
    /// the GPU park timer is armed
    park_timer: bool,
    /// the blink timer exists
    caret_timer: bool,
    /// the blink stopped for lack of input (caret solid until the next key)
    caret_idle: bool,
    last_input: Instant,
    /// start of the last paint (streaming listings repaint at most ~60/s)
    last_paint: Instant,
    /// menu entries, drag and drop, clipboard, delete (`src/shellops.rs`)
    ops: shellops::ShellOps,
    /// WM_CLOSE already posted/handled (prevents duplicate close posts or double flush)
    closing: bool,
    /// `save_flush(..., true)` already ran on `WM_CLOSE`
    saved_on_close: bool,
    /// Pre-fullscreen style and placement while F11 borderless fullscreen is active.
    fullscreen: Option<FullscreenSaved>,
}

#[derive(Clone, Copy)]
struct FullscreenSaved {
    style: i32,
    placement: WINDOWPLACEMENT,
    maximized: bool,
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

/// `[appearance] density` -> the kit's row height preset.
fn ui_density(d: ConfigDensity) -> Density {
    match d {
        ConfigDensity::Compact => Density::Compact,
        ConfigDensity::Normal => Density::Comfortable,
        ConfigDensity::Comfortable => Density::Spacious,
    }
}

/// `--help` / `--version`: a GUI-subsystem process has no console handles of
/// its own, so attach to the console that started it and write there. A
/// redirected handle (`tsubame --help > out.txt`) is already valid and is used
/// as is: attaching would point stdout at the console instead.
fn print_and_exit(text: &str) -> ! {
    use std::io::Write;
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let have_stdout = stdout.is_ok_and(|h| !h.is_invalid());
    if !have_stdout {
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
    std::process::exit(0);
}

/// The work areas of every monitor, for the saved window geometry.
fn work_areas() -> Vec<tsubame_config::WorkArea> {
    unsafe extern "system" fn collect(hmon: HMONITOR, _hdc: HDC, _rc: *mut RECT, data: LPARAM) -> BOOL {
        let v = unsafe { &mut *(data.0 as *mut Vec<tsubame_config::WorkArea>) };
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
            let r = mi.rcWork;
            v.push(tsubame_config::WorkArea { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top });
        }
        TRUE
    }
    let mut v: Vec<tsubame_config::WorkArea> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// The current window geometry, as the config and session store it.
fn window_state(hwnd: HWND) -> tsubame_config::WindowState {
    if let Some(saved) = with(|a| a.fullscreen).flatten() {
        let r = saved.placement.rcNormalPosition;
        return tsubame_config::WindowState { x: r.left, y: r.top, w: (r.right - r.left).max(1), h: (r.bottom - r.top).max(1), maximized: saved.maximized };
    }
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    let maximized = unsafe { IsZoomed(hwnd).as_bool() };
    if unsafe { GetWindowPlacement(hwnd, &mut wp) }.is_ok() {
        // rcNormalPosition is the restored (non-maximized) geometry
        let r = wp.rcNormalPosition;
        return tsubame_config::WindowState { x: r.left, y: r.top, w: (r.right - r.left).max(1), h: (r.bottom - r.top).max(1), maximized };
    }
    let mut r = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut r) };
    tsubame_config::WindowState { x: r.left, y: r.top, w: (r.right - r.left).max(1), h: (r.bottom - r.top).max(1), maximized }
}

fn main() {
    wakelog::init();
    tsubame_win::lagtrace::init();
    let args: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let cli = cli::Cli::parse(&args, &cwd);
    if cli.help {
        print_and_exit(cli::HELP);
    }
    if cli.version {
        print_and_exit(&format!("{}\n", buildinfo::version_line()));
    }
    let flag = |n: &str| args.iter().any(|a| a == n);
    let bench_mode = flag("--bench") || std::env::var_os("TSUBAME_BENCH").is_some_and(|v| v != "0");
    let timing_mode = flag("--timing");
    let gate_mode = flag("--leak-gate");
    let stress_mode = arg_value(&args, "--stress").map(|m| match m.as_str() {
        "thumbs" => stress::Mode::Thumbs,
        "clip" => stress::Mode::Clip,
        "ctxmenu" => stress::Mode::CtxMenu,
        _ => stress::Mode::Menu,
    });
    let workers = arg_value(&args, "--workers").and_then(|v| v.parse().ok()).unwrap_or(shellui::ICON_WORKERS);
    let timing_dirs: Vec<String> = arg_value(&args, "--timing-dirs").map(|s| s.split(';').filter(|p| !p.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    // a measurement run: no single instance, no config writes, default appearance
    let dev = cli.is_dev_run();
    let targets = cli::targets(&cli);
    let explicit_dir = targets.first().map(|t| t.folder.clone()).or_else(|| arg_value(&args, "--dir"));
    let synthetic = bench_mode && explicit_dir.is_none();

    // --- settings and session -------------------------------------------------
    let loc = tsubame_config::locate_now(cli.portable, cli.config.clone());
    let loaded = tsubame_config::ConfigFile::load(&loc.config);
    let cfg = loaded.value.config.clone();
    let cfg_status = loaded.status.clone();
    for w in &cli.warnings {
        eprintln!("tsubame: {w}");
    }
    if let Some(m) = cfg_status.message() {
        eprintln!("tsubame: {m}");
    }
    let session_loaded = tsubame_config::Session::load(&loc.session);
    let mut saved_session = session_loaded.value.clone();
    // restore only with no path on the command line, and never during a bench
    let restore_session = !dev && targets.is_empty() && cfg.general.restore_session && session_loaded.status == LoadStatus::Loaded && saved_session.is_usable();
    if restore_session {
        // the active tab is the only folder listed at startup; the rest are lazy
        let active = saved_session.active_tab();
        if let Some(tab) = saved_session.panes.get_mut(saved_session.active_pane).and_then(|p| p.tabs.get_mut(active)) {
            // a folder that disappeared falls back to its nearest existing parent
            tab.path = session::tab_folder(&tab.path);
        }
    }
    // a path replaced the first tab (no --new-tab)
    let inline_first = !cli.new_tab && !targets.is_empty();
    let start = if restore_session {
        saved_session.tabs().get(saved_session.active_tab()).map(|t| t.path.clone()).unwrap_or_default()
    } else if inline_first {
        targets[0].folder.clone()
    } else if dev {
        timing_dirs.first().cloned().or_else(|| explicit_dir.clone()).unwrap_or_else(places::home_folder)
    } else {
        session::start_folder(&cfg, Some(&saved_session), &[])
    };
    let start = if start.is_empty() { places::home_folder() } else { start };

    let dark = match arg_value(&args, "--theme").as_deref() {
        Some("light") => false,
        Some("dark") => true,
        // the first ever start follows the system theme; afterwards the file decides
        _ if dev || cfg_status == LoadStatus::Missing => system_dark(),
        _ => cfg.appearance.is_dark(),
    };
    let density = arg_value(&args, "--density").and_then(|d| parse_density(&d)).unwrap_or(if dev { Density::Compact } else { ui_density(cfg.appearance.density) });

    if flag("--leak-smoke") {
        let text = leak::run(&start);
        bench::write_text_report("u2-leak.txt", &text);
        return;
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();

        // --- updater ----------------------------------------------------------
        // before the single-instance claim: a copy the updater just started waits for the
        // old process, and a crashed update rolls back here. No network and no disk in a
        // dev run.
        if let Some(pid) = cli.after_update {
            updates::wait_for_exit(pid, Duration::from_secs(10));
        }
        let upd_env = updates::Env { dev_run: dev, portable: cli.portable || loc.portable, bench: bench_mode };
        let wake = || {
            let h = UPDATE_HWND.load(Ordering::Acquire);
            if h != 0 {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
            }
        };
        let channel = tsubame_update::Channel::from_config(&cfg.updates.channel);
        if updates::init(upd_env, cfg.updates.auto_install, channel, wake) == updates::StartAction::Exit {
            return;
        }

        // --- single instance --------------------------------------------------
        // Claimed before the window exists, so a forwarded launch that arrives
        // while this process is still setting up waits in the queue.
        let mut _instance = None;
        if !dev {
            let launch = single::Launch { new_tab: cli.new_tab, paths: targets.clone() };
            // an update restart never forwards to another running window: it must become ready itself
            match single::claim(hinst, &launch, cli.new_window || cli.after_update.is_some()) {
                single::Claim::Forwarded => return,
                single::Claim::Owner(inst) => _instance = Some(inst),
                single::Claim::Standalone => {}
            }
        }

        // the first frame's software stand-in device, made while the window and
        // the folder are set up
        tsubame_d2d::prewarm_software();

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
        single::set_main(hwnd);
        UPDATE_HWND.store(hwnd.0 as isize, Ordering::Release);
        // OLE STA + drop target, before anything else uses COM on this thread
        let drop_target = shellops::init(hwnd);

        // the saved geometry: the session first (when restoring), else the config
        let saved_geom = if restore_session && session_loaded.status == LoadStatus::Loaded {
            Some(saved_session.window)
        } else if !dev && cfg_status == LoadStatus::Loaded {
            Some(cfg.window)
        } else {
            None
        };
        let geom = saved_geom.map(|g| g.sanitized(&work_areas()));
        if let Some(g) = geom {
            // a window saved on a monitor that is gone comes back on the primary
            let _ = SetWindowPos(hwnd, None, g.x, g.y, g.w, g.h, SWP_NOZORDER | SWP_NOACTIVATE);
        }

        let mut gfx = Gfx::new(hwnd).unwrap();
        let dpi = GetDpiForWindow(hwnd) as f32;
        gfx.set_dpi(dpi);

        // shell workers: all COM and disk work happens there
        let thumb_workers = cli.thumb_workers.unwrap_or(shellui::THUMB_WORKERS);
        let mut pool = Pool::with_lanes(workers, thumb_workers, hwnd.0 as isize, WM_APP_SHELL);
        if let Some(ms) = std::env::var("TSUBAME_HANG_MS").ok().and_then(|v| v.parse().ok()) {
            pool.hang_after = Duration::from_millis(ms);
        }
        let mut shell = shellui::ShellUi::new(icon_px(dpi));
        shell.collect_other = matches!(stress_mode, Some(stress::Mode::Thumbs | stress::Mode::Clip));

        // enumerator workers wake the UI thread with one coalesced message
        let hw = hwnd.0 as isize;
        let wake: tsubame_win::enumerate::Wake = Arc::new(move || {
            if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
                let _ = PostMessageW(Some(HWND(hw as *mut _)), WM_APP_WAKE, WPARAM(0), LPARAM(0));
            }
        });

        let mut view = view::View::new(&start, wake, dark, density);
        // where this copy lives (Settings shows it) and the settings as the user has them
        view.env = settings::Env {
            config_path: loc.config.clone(),
            data_dir: loc.data_dir.clone(),
            install_dir: std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default(),
            exe: std::env::current_exe().unwrap_or_default(),
            portable: cli.portable || loc.portable,
            system_dark: system_dark(),
            version: buildinfo::VERSION,
        };
        let theme_from_cfg = !dev && cfg_status != LoadStatus::Missing && arg_value(&args, "--theme").is_none();
        // a measurement run uses the defaults and shows every file
        view.set_prefs(&if dev { tsubame_config::Config::default() } else { cfg.clone() }, !dev, theme_from_cfg);
        view.publish_menu();
        // config appearance and panes
        view.zebra = flag("--zebra") || (!dev && cfg.appearance.zebra);
        view.pull_appearance();
        // set the field directly: no tab has been listed yet, and starting up must not dirty the config
        view.mode = if dev { view::ViewMode::Details } else { view::ViewMode::from_config(cfg.appearance.view_mode) };
        view.sidebar_width = if dev { tsubame_config::DEFAULT_SIDEBAR_WIDTH } else { cfg.panes.sidebar_width.clamp(120.0, 480.0) };
        if !dev && cfg.panes.sidebar_collapsed {
            view.side.rail = true;
        }
        if !dev {
            view.side.pinned = cfg.sidebar.pinned.clone();
            view.side.set_closed_names(&cfg.sidebar.closed);
        }
        if restore_session {
            // the started tab is the active one; the rest stay lazy
            session::apply(&mut view, &saved_session);
        }
        if !dev {
            session::apply_layout(&mut view, &cfg, restore_session.then_some(&saved_session));
        }
        // command-line paths: the first one is already shown unless --new-tab
        if inline_first
            && let Some(sel) = &targets[0].select
        {
            view.tab_mut().select_names(std::slice::from_ref(sel));
        }
        let extra: &[cli::Target] = if cli.new_tab { &targets[..] } else { &targets[targets.len().min(1)..] };
        for t in extra {
            let mut tab = view.pending_tab(&t.folder);
            tab.restore = t.select.clone().map(|sel| tab::Restore { scroll_first_row: 0, selected: vec![sel] });
            view.tabs.items.push(tab);
        }
        if !extra.is_empty() || inline_first {
            view.after_restore();
        }
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
        view.keymap.bind_all(&mut ui);
        ui.clipboard = Box::new(clip::WinClipboard(hwnd.0 as isize));
        ui.set_focus(Some(view.list_id()));
        view.dpi = dpi;
        if let Some(m) = arg_value(&args, "--view").and_then(|v| view::ViewMode::parse(&v)) {
            view.set_mode(m);
        }
        if let Some(b) = bench_state.as_mut() {
            b.grid = view.mode.is_grid();
        }

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
        // a measurement run never writes the user's config or session
        let saver_status = if dev { LoadStatus::Newer { version: u32::MAX } } else { cfg_status.clone() };
        let saver_session_status = if dev { LoadStatus::Newer { version: u32::MAX } } else { session_loaded.status.clone() };
        let saver = session::Saver::new(loc.clone(), loaded.value, saved_session.clone(), &saver_status, &saver_session_status);

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                view,
                ui,
                saver,
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
                ctx_timer: None,
                row_tip_timer: None,
                park_timer: false,
                caret_timer: false,
                caret_idle: false,
                last_input: Instant::now(),
                last_paint: Instant::now(),
                ops: Default::default(),
                closing: false,
                saved_on_close: false,
                fullscreen: None,
            })
        });
        with(|a| shellops::attach(a, hwnd, drop_target));

        // bench and timing runs measure at full screen (4K on the benchmark machine); a restored
        // maximized window comes back maximized
        let maximize = ((bench_mode || timing_mode || gate_mode) && !flag("--windowed")) || (geom.is_some_and(|g| g.maximized) && !flag("--windowed"));
        let _ = ShowWindow(hwnd, if maximize { SW_SHOWMAXIMIZED } else { SW_SHOW });

        // Idle = blocked here. Timers only while a bench/stress run, the
        // hung-worker watchdog (pool busy) or a focused text field's caret needs them,
        // plus one park timer per stretch of GPU painting (gpu_park_after).
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd.is_invalid() {
                // a thread message (no window procedure sees it)
                wakelog::note(msg.message | 0x8000_0000, msg.wParam.0);
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        wakelog::write();
        let trace = tsubame_win::lagtrace::on();
        let t_teardown = trace.then(Instant::now);
        if trace {
            tsubame_win::lagtrace::record("close.loop_exit", None, format_args!("ok=true"), false);
        }
        // drop D2D/D3D objects now, while the process is fully alive (a
        // thread-local destructor run inside ExitProcess crashes in D3D teardown)
        let taken = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.take()));
        if let Some(app) = taken {
            if trace {
                let App { view, ui, saver, gfx, bench, timing, gate, pool, shell, stress, ops, .. } = app;
                let t = Instant::now();
                drop(view);
                tsubame_win::lagtrace::record("close.drop.view", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                let t = Instant::now();
                drop(pool);
                tsubame_win::lagtrace::record("close.drop.pool", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                let t = Instant::now();
                drop((shell, ui, saver, bench, timing, gate, stress, ops));
                tsubame_win::lagtrace::record("close.drop.state", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                let t = Instant::now();
                drop(gfx);
                tsubame_win::lagtrace::record("close.drop.gfx", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
            } else {
                drop(app);
            }
        }
        if trace {
            let t = Instant::now();
            drop(_instance);
            tsubame_win::lagtrace::record("close.drop.instance", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
            if let Some(t0) = t_teardown {
                tsubame_win::lagtrace::record("close.exit", Some(t0.elapsed().as_secs_f64() * 1000.0), format_args!(""), true);
            }
            tsubame_win::lagtrace::flush();
        } else {
            drop(_instance);
        }
        // "Download and restart" / "Roll back": the window is gone and the session saved
        match AFTER_EXIT.load(Ordering::Acquire) {
            1 => {
                updates::restart();
            }
            2 => {
                updates::rollback_and_relaunch();
            }
            _ => {}
        }
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

/// The own-drawn menu ticks only while holding for the reveal grace window
/// (single one-shot), while shell items or a submenu are still loading (100 ms,
/// ~10 FPS), or while a submenu hover delay is pending. An idle menu and no
/// menu: no timer (0 wake-ups).
pub(crate) fn arm_ctx(hwnd: HWND, a: &mut App) {
    let want = a.view.ctx.as_ref().and_then(|c| c.next_tick_ms()).or_else(|| a.view.nav_menu_tick_ms());
    match want {
        Some(ms) if a.ctx_timer != Some(ms) => {
            unsafe { SetTimer(Some(hwnd), ctxui::TIMER_ID, ms, None) };
            a.ctx_timer = Some(ms);
        }
        None if a.ctx_timer.is_some() => {
            let _ = unsafe { KillTimer(Some(hwnd), ctxui::TIMER_ID) };
            a.ctx_timer = None;
        }
        _ => {}
    }
}

/// How long the window must go without a paint before the GPU device is let
/// go. `TSUBAME_GPU_PARK=<seconds>` changes it, `0` keeps the device. Off in
/// bench, stress and leak-gate runs, which measure the GPU path.
fn gpu_park_after(a: &App) -> Option<Duration> {
    static S: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let s = *S.get_or_init(|| std::env::var("TSUBAME_GPU_PARK").ok().and_then(|v| v.parse().ok()).unwrap_or(10));
    (s > 0 && a.bench.is_none() && a.stress.is_none() && a.gate.is_none()).then(|| Duration::from_secs(s))
}

/// Arms the park timer once a GPU target is painting. It is not re-armed
/// per paint: when it fires early (a paint since), it waits out the rest.
fn arm_park(hwnd: HWND, a: &mut App) {
    if a.park_timer || !a.gfx.on_gpu() {
        return;
    }
    if let Some(d) = gpu_park_after(a) {
        unsafe { SetTimer(Some(hwnd), PARK_TIMER, d.as_millis() as u32, None) };
        a.park_timer = true;
    }
}

/// The park timer fired: park the GPU when nothing painted for the whole
/// period, otherwise wait the rest of it. No repaint: DWM keeps showing the
/// last presented frame after the swap chain is gone (a software repaint of a
/// 4K window cost ~330 ms of CPU on WARP's threads); a paint the system asks
/// for later is drawn on a software target.
fn on_park_timer(hwnd: HWND) {
    with(|a| {
        let _ = unsafe { KillTimer(Some(hwnd), PARK_TIMER) };
        a.park_timer = false;
        let Some(d) = gpu_park_after(a) else { return };
        let since = a.last_paint.elapsed();
        if since < d {
            let left = (d - since).as_millis().max(1) as u32;
            unsafe { SetTimer(Some(hwnd), PARK_TIMER, left, None) };
            a.park_timer = true;
            return;
        }
        a.gfx.park();
    });
}

/// The file-operation poll timer exists only while an operation is running.
fn arm_ops(hwnd: HWND, a: &mut App) {
    let busy = a.view.ops_busy();
    if busy && !a.view.ops_timer {
        unsafe { SetTimer(Some(hwnd), OPS_TIMER, OPS_MS, None) };
        a.view.ops_timer = true;
    } else if !busy && a.view.ops_timer {
        let _ = unsafe { KillTimer(Some(hwnd), OPS_TIMER) };
        a.view.ops_timer = false;
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

/// One-shot dwell timer for row hover tooltips: armed ONLY while waiting out
/// the 600 ms hover delay, disarmed immediately on fire or leave (0 wake-ups when idle).
fn arm_row_tip(hwnd: HWND, a: &mut App) {
    match a.view.row_tip.next_timer(Instant::now()) {
        Some((seq, ms)) if a.row_tip_timer != Some(seq) => {
            unsafe { SetTimer(Some(hwnd), view_tooltip::TIMER_ID, ms, None) };
            a.row_tip_timer = Some(seq);
        }
        None if a.row_tip_timer.is_some() => {
            let _ = unsafe { KillTimer(Some(hwnd), view_tooltip::TIMER_ID) };
            a.row_tip_timer = None;
        }
        _ => {}
    }
}

/// Cancels any pending or visible row tooltip and kills its one-shot timer.
fn cancel_row_tip(hwnd: HWND) {
    let repaint = with(|a| {
        if a.row_tip_timer.is_some() {
            let _ = unsafe { KillTimer(Some(hwnd), view_tooltip::TIMER_ID) };
            a.row_tip_timer = None;
        }
        a.view.row_tip.cancel()
    })
    .unwrap_or(false);
    if repaint {
        invalidate(hwnd);
    }
}

/// Reads drives, known folders (including OneDrive) and WSL distros on a worker
/// (they can block on a dead network share or an empty card reader); WM_APP_PLACES brings the result.
fn spawn_places(hwnd: HWND) {
    if PLACES_BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let h = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("tsubame-places".into()).spawn(move || {
        let r = (places::list_drives(), places::known_folders(), places::wsl_distros());
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
    arm_ops(hwnd, a);
    arm_ctx(hwnd, a);
    arm_row_tip(hwnd, a);
    arm_park(hwnd, a);
    if a.view.want_drives && !PLACES_BUSY.load(Ordering::Acquire) {
        a.view.want_drives = false;
        spawn_places(hwnd);
    }
}

/// What to do after the process exits: 1 start the updated version, 2 roll back.
static AFTER_EXIT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Requests the Settings pages made that main must carry out: restart into the updated
/// version, roll back, reset every setting.
fn settings_requests(hwnd: HWND) {
    let (restart, rollback, reset) = with(|a| (std::mem::take(&mut a.view.want_restart), std::mem::take(&mut a.view.want_rollback), std::mem::take(&mut a.view.want_reset_all))).unwrap_or_default();
    if reset {
        with(|a| {
            let old = a.saver.config.config.clone();
            // the old file stays as config.toml.bak
            let _ = std::fs::copy(&a.saver.loc.config, a.saver.loc.config.with_file_name("config.toml.bak"));
            let def = tsubame_config::Config { window: old.window, panes: old.panes, sidebar: old.sidebar, ..Default::default() };
            let App { view, ui, .. } = a;
            view.reset_prefs(ui, &def);
            view.ops.message = Some("All settings are back to the defaults. The old file is config.toml.bak.".to_string());
        });
        invalidate(hwnd);
    }
    if restart || rollback {
        AFTER_EXIT.store(if restart { 1 } else { 2 }, Ordering::Release);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// A setting the user changed: copy it into the config and wake the debounced
/// writer (at most one write per 2 s).
fn settings_changed(hwnd: HWND) {
    let changed = with(|a| {
        let App { view, saver, .. } = a;
        if !std::mem::take(&mut view.config_dirty) {
            return false;
        }
        session::sync_config(view, &mut saver.config.config);
        saver.config_changed();
        true
    })
    .unwrap_or(false);
    if changed {
        with(|a| arm_save(hwnd, a));
    }
}

/// One-shot timer for the moment a save is allowed; no timer while clean.
fn arm_save(hwnd: HWND, a: &mut App) {
    match (a.saver.pending(), a.saver.wait_ms(Instant::now())) {
        (true, Some(ms)) => {
            unsafe { SetTimer(Some(hwnd), SAVE_TIMER, ms.max(1), None) };
        }
        _ => {
            let _ = unsafe { KillTimer(Some(hwnd), SAVE_TIMER) };
        }
    }
}

/// Writes the session and the window geometry in the config. `force` is used on
/// exit: a change inside the debounce window is still written. Writes nothing
/// when the content is unchanged.
fn save_flush(hwnd: HWND, force: bool) {
    let geom = window_state(hwnd);
    save_flush_geom(hwnd, geom, force);
}

fn save_flush_geom(hwnd: HWND, geom: tsubame_config::WindowState, force: bool) {
    with(|a| {
        a.saver.session = session::capture(&a.view, geom);
        a.saver.session_changed();
        a.saver.config.config.window = geom;
        a.saver.config_changed();
        a.saver.flush(Instant::now(), force);
        arm_save(hwnd, a);
    });
}

/// Paths forwarded by a second launch: focus the tab already showing the
/// folder, or open a new tab in this instance.
fn open_launch(hwnd: HWND) {
    let launches = single::take_pending();
    if launches.is_empty() {
        return;
    }
    with(|a| {
        for l in &launches {
            session::open_targets(&mut a.view, &l.paths, l.new_tab);
        }
    });
    invalidate(hwnd);
}

unsafe fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let mut animating = false;
    unsafe { BeginPaint(hwnd, &mut ps) };
    let info = with(|a| {
        let t0 = Instant::now();
        a.last_paint = t0;
        // If the GPU device was parked while idle, clear the parked state via
        // `release()` before `Gfx::begin()` so the first frame paints on the
        // software stand-in (~13 ms) without racing a concurrent hardware
        // D3D11CreateDevice (~170 ms UI-thread stall). `Gfx::end()` starts the
        // background GPU warm-up right after presenting this frame.
        if a.gfx.parked() {
            a.gfx.release();
        }
        a.shell.begin_frame();
        a.view.cur_mods = mods();
        let out = a.view.paint(&mut a.ui, &mut a.gfx, &mut a.shell);
        let total = t0.elapsed().as_secs_f32() * 1000.0;
        after_paint(hwnd, a);
        animating = a.shell.needs_frame();
        if let Some(g) = a.gate.as_mut() {
            g.painted(a.gfx.path());
        }
        if let Some(s) = a.stress.as_mut() {
            s.painted(total);
        }
        // timing of the navigation: a frame with rows / the final frame was presented
        let final_frame = out.is_some() && !a.view.tabs.is_empty() && a.view.tab_mut().frame_presented();
        (total, a.gfx.end_ms, a.gfx.path(), out, final_frame)
    });
    let _ = unsafe { EndPaint(hwnd, &ps) };
    // a thumbnail is fading in (or uploads were postponed): one more frame, which ends by itself
    if animating {
        invalidate(hwnd);
    }
    let Some((total_ms, end_ms, path, out, final_frame)) = info else { return };
    if out.is_some() && wakelog::on() {
        wakelog::paint(path == "hardware", total_ms, with(|a| a.gfx.create_ms).unwrap_or(0.0));
    }
    if let Some(out) = out {
        after_frame(hwnd, out);
    }
    // a theme/density/zebra change: write the config, debounced
    settings_changed(hwnd);

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
        // Pre-warm Win11 packaged context-menu manifest scan on a low-priority
        // background thread 250 ms after first paint, keeping the <100 ms cold
        // start and initial folder load untouched while eliminating the ~167 ms
        // manifest scan on the first right-click.
        if with(|a| a.bench.is_none() && a.timing.is_none() && a.gate.is_none()).unwrap_or(false) {
            tsubame_win::shell::packaged::prewarm_after(Duration::from_millis(250));
        }
        // the update (if any) took: end its trial; then the once-a-day background check
        // (a worker; a no-op in a dev run, a portable copy, or with the setting off)
        updates::on_ready();
        let auto = with(|a| a.saver.config.config.updates.auto_check).unwrap_or(false);
        updates::auto_check(auto);
        // a measurement run times the cold paths itself; `TSUBAME_CTX_WARM=0` turns it off (A/B)
        if with(|a| a.bench.is_none() && a.stress.is_none() && a.gate.is_none() && a.timing.is_none()).unwrap_or(false) && ctxui::warm_enabled() {
            unsafe { SetTimer(Some(hwnd), CTX_WARM_TIMER, CTX_WARM_MS, None) };
        }
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
                let g = &a.gfx;
                let cost = bench::FrameCost {
                    present_ms: g.present_ms,
                    layouts: g.layouts_made,
                    layout_ms: g.layout_ms,
                    text_calls: g.text_calls,
                    text_clipped: g.text_clipped,
                    text_ms: g.text_ms,
                    image_ms: g.image_ms,
                    bitmaps: g.bitmaps_made,
                };
                b.painted(total_ms, end_ms, cost, path);
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
            extra.push_str(&format!(
                "thumb cache: {} entries, {:.2} MB (cap {} MB), evictions {}; device bitmaps {:.2} MB; view {} (thumbnail request {} px; at end {} in flight, {} queued)\n",
                a.shell.thumbs.len(),
                a.shell.thumbs.bytes() as f64 / 1048576.0,
                shellui::THUMB_CAP_BYTES >> 20,
                a.shell.thumbs.evictions,
                a.gfx.bitmap_bytes() as f64 / 1048576.0,
                a.view.mode.name(),
                a.shell.thumb_px(),
                a.shell.load().0,
                a.shell.load().1
            ));
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

/// Context menu for view row `row`, at client px (x, y); `keyboard`: opened
/// by Shift+F10 / the Menu key.
fn open_menu(hwnd: HWND, row: usize, x: i32, y: i32, keyboard: bool) {
    let extended = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 && !keyboard;
    let (sx, sy) = screen_pt(hwnd, x, y);
    let paint_now = with(|a| {
        if a.view.tab().shown() == 0 {
            let s = a.gfx.dpi / 96.0;
            shellops::open_background_menu(a, a.view.tab().path.clone(), sx, sy, extended, Pt::new(x as f32 / s, y as f32 / s), keyboard);
            arm_ctx(hwnd, a);
            return !a.view.ctx.as_ref().is_some_and(|c| c.open && !c.is_revealed());
        }
        let paths = a.view.menu_paths(row);
        let t = a.view.tab();
        let folders = t.store.view().iter().enumerate().filter(|(_, id)| t.sel.contains(**id) || t.store.at(row) == Some(**id)).filter_map(|(i, _)| t.entry_at(i)).filter(|(p, dir)| *dir && paths.contains(p)).map(|(p, _)| p).collect();
        if paths.is_empty() {
            return false;
        }
        let s = a.gfx.dpi / 96.0;
        shellops::open_item_menu(a, paths, folders, sx, sy, extended, Pt::new(x as f32 / s, y as f32 / s), keyboard);
        arm_ctx(hwnd, a);
        !a.view.ctx.as_ref().is_some_and(|c| c.open && !c.is_revealed())
    })
    .unwrap_or(false);
    if paint_now {
        invalidate(hwnd);
    }
}

/// Keyboard-invoked menu, at the cursor row.
fn own_menu_open() -> bool {
    with(|a| a.view.ctx.as_ref().is_some_and(|c| c.open)).unwrap_or(false)
}

fn key_menu(hwnd: HWND) {
    ctxui::note_input();
    let p = with(|a| {
        let s = a.gfx.dpi / 96.0;
        let grid = a.view.mode.is_grid();
        let t = a.view.tab_mut();
        let row = if grid { t.grid.cursor } else { t.list.cursor };
        if t.shown() > 0 && !t.store.at(row).is_some_and(|id| t.sel.contains(id)) {
            t.sel.single(&t.store, row);
        }
        let anchor = a.view.menu_anchor(row);
        (row, (anchor.x * s) as i32, (anchor.y * s) as i32)
    });
    if let Some((row, x, y)) = p {
        open_menu(hwnd, row, x, y, true);
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
            Some(p) => (row, (p.x * s) as i32, (p.y * s) as i32, false),
            None => {
                let anchor = a.view.menu_anchor(row);
                (row, (anchor.x * s) as i32, (anchor.y * s) as i32, true)
            }
        });
        sync_caret(hwnd, a, out.text_focus);
        let open = std::mem::take(&mut a.view.open_paths);
        let new_wins = std::mem::take(&mut a.view.open_windows);
        let exe = a.view.env.exe.clone();
        let portable = a.view.env.portable;
        (cap, ime, menu, open, new_wins, exe, portable, a.view.win_cmd.take(), a.view.quit)
    });
    let Some((cap, ime, menu, open, new_wins, exe, portable, win_cmd, quit)) = r else { return };
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
    if let Some((row, x, y, keyboard)) = menu {
        open_menu(hwnd, row, x, y, keyboard);
    }
    shellops::after_frame(hwnd);
    shellops::ctx_after_frame(hwnd);
    // a drag started on our rows: OLE's modal loop, outside the app borrow
    shellops::drag_out(hwnd);
    for p in open {
        places::open_async(p);
    }
    for dir in new_wins {
        view::spawn_new_window(&exe, dir, portable);
    }
    match win_cmd {
        Some(view::WinCmd::Minimize) => unsafe {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        },
        Some(view::WinCmd::MaxRestore) => {
            if with(|a| a.fullscreen.is_some()).unwrap_or(false) {
                toggle_fullscreen(hwnd);
            } else {
                unsafe {
                    let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
                }
            }
        }
        Some(view::WinCmd::Close) => unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        Some(view::WinCmd::ToggleFullscreen) => toggle_fullscreen(hwnd),
        None => {}
    }
    if quit {
        let should_post = with(|a| !std::mem::replace(&mut a.closing, true)).unwrap_or(true);
        if should_post {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        return;
    }
    settings_requests(hwnd);
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
        // the numpad + and - are the zoom keys with Ctrl (Ctrl+= / Ctrl+-)
        VK_ADD => Key::Char('='),
        VK_SUBTRACT => Key::Char('-'),
        v if (VK_F1.0..=VK_F24.0).contains(&v.0) => Key::F((v.0 - VK_F1.0 + 1) as u8),
        v if (0x30..=0x39).contains(&v.0) || (0x41..=0x5a).contains(&v.0) => Key::Char(v.0 as u8 as char),
        v if v.0 == 0xDC => Key::Char('\\'), // VK_OEM_5: the backslash key (Ctrl+\ splits the window)
        // the other punctuation keys, so they can be bound (Ctrl+, opens Settings)
        v if crate::keymap::oem_char(v.0).is_some() => Key::Char(crate::keymap::oem_char(v.0).unwrap_or(' ')),
        v => Key::Other(v.0 as u32),
    }
}

/// Decodes XBUTTON1 (Back -> Some(false)) / XBUTTON2 (Forward -> Some(true)) from WM_XBUTTON* wParam.
fn xbutton_nav(wp: WPARAM) -> Option<bool> {
    match ((wp.0 >> 16) & 0xffff) as u16 {
        XBUTTON1 => Some(false),
        XBUTTON2 => Some(true),
        _ => None,
    }
}

/// Decodes APPCOMMAND_BROWSER_BACKWARD (Some(false)) / APPCOMMAND_BROWSER_FORWARD (Some(true)) from WM_APPCOMMAND lParam.
fn appcommand_nav(lp: LPARAM) -> Option<bool> {
    // GET_APPCOMMAND_LPARAM(lParam) = HIWORD(lParam) & ~FAPPCOMMAND_MASK
    let cmd = (((lp.0 >> 16) & 0xffff) as u32) & !FAPPCOMMAND_MASK;
    match cmd {
        APPCOMMAND_BROWSER_BACKWARD => Some(false),
        APPCOMMAND_BROWSER_FORWARD => Some(true),
        _ => None,
    }
}

/// Routes a Back/Forward navigation button to the active pane.
fn mouse_nav(hwnd: HWND, forward: bool) -> bool {
    with(|a| {
        note_input(a);
        let changed = a.view.nav_button(&mut a.ui, forward);
        arm_ctx(hwnd, a);
        changed
    })
    .unwrap_or(false)
}

fn button_event(hwnd: HWND, msg: u32, lp: LPARAM) {
    let pos = dip_pt(lp);
    let mods = mods();
    let down = |button, double| Event::MouseDown { pos, button, mods, double };
    let ev = match msg {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            // Cancel a context menu still hidden inside its reveal grace window.
            with(|a| {
                if a.view.ctx.as_ref().is_some_and(|c| c.open && !c.is_revealed()) {
                    a.view.ctx = None;
                    arm_ctx(hwnd, a);
                }
            });
            down(Button::Left, msg == WM_LBUTTONDBLCLK)
        }
        WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => {
            // the own-drawn menu measures right-click -> first paint from here
            ctxui::note_input();
            // Let a new right-click outside the old menu reach the view in
            // this same gesture, instead of popup dismissal eating it.
            with(|a| {
                if a.view.ctx.as_ref().is_some_and(|c| c.open && !c.state.levels.iter().any(|l| l.rect.contains_pt(pos))) {
                    a.view.ctx = None;
                    a.ui.close_popup(ctxui::ID_CTX.with(0));
                    arm_ctx(hwnd, a);
                }
            });
            down(Button::Right, false)
        }
        WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => {
            with(|a| {
                if a.view.ctx.as_ref().is_some_and(|c| c.open && !c.is_revealed()) {
                    a.view.ctx = None;
                    arm_ctx(hwnd, a);
                }
            });
            down(Button::Middle, false)
        }
        WM_LBUTTONUP => Event::MouseUp { pos, button: Button::Left, mods },
        WM_RBUTTONUP => Event::MouseUp { pos, button: Button::Right, mods },
        _ => Event::MouseUp { pos, button: Button::Middle, mods },
    };
    push(hwnd, ev);
}

/// Borderless fullscreen toggle (F11): covers the whole monitor (including
/// taskbar) and restores the previous style, placement and maximized state.
/// Esc never calls this.
fn toggle_fullscreen(hwnd: HWND) {
    let leaving = with(|a| a.fullscreen.take()).flatten();
    if let Some(saved) = leaving {
        with(|a| a.view.fullscreen = false);
        unsafe {
            SetWindowLongW(hwnd, GWL_STYLE, saved.style);
            let _ = SetWindowPlacement(hwnd, &saved.placement);
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
            if saved.maximized && !IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_MAXIMIZE);
            }
        }
        invalidate(hwnd);
        return;
    }
    let mut placement = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    let (style, maximized, mon_rect) = unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE);
        let _ = GetWindowPlacement(hwnd, &mut placement);
        let maximized = IsZoomed(hwnd).as_bool();
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let ok = GetMonitorInfoW(hmon, &mut mi).as_bool();
        (style, maximized, ok.then_some(mi.rcMonitor))
    };
    let Some(r) = mon_rect else { return };
    with(|a| {
        a.fullscreen = Some(FullscreenSaved { style, placement, maximized });
        a.view.fullscreen = true;
    });
    unsafe {
        let strip = (WS_OVERLAPPEDWINDOW | WS_MAXIMIZE).0 as i32;
        SetWindowLongW(hwnd, GWL_STYLE, style & !strip);
        let _ = SetWindowPos(hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
    }
    invalidate(hwnd);
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
        if a.fullscreen.is_some() {
            return (1.0, false, true);
        }
        let s = a.gfx.dpi / 96.0;
        let pt = Pt::new(p.x as f32 / s, p.y as f32 / s);
        let caption = pt.y < a.ui.theme.height_titlebar && !a.ui.over_widget(pt);
        (s, caption, false)
    });
    let Some((s, caption, fullscreen)) = r else { return LRESULT(HTCLIENT as isize) };
    if fullscreen {
        return LRESULT(HTCLIENT as isize);
    }
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
    wakelog::note(msg, wp.0);
    // Note: a parked GPU is unparked lazily in `on_paint` (software stand-in
    // first, hardware warm-up in `Gfx::end()` after presenting), so input such
    // as right-click never races a synchronous D3D11 hardware init (~170 ms)
    // against the first software frame.
    unsafe {
        match msg {
            WM_PAINT => {
                on_paint(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            // no system title bar or frame: the client area is the whole window
            WM_NCCALCSIZE if wp.0 != 0 => {
                let fs = with(|a| a.fullscreen.is_some()).unwrap_or(false);
                if !fs && IsZoomed(hwnd).as_bool() {
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
                    a.view.dpi = dpi;
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
                        let (rest, need) = shellops::on_reply(a, r);
                        repaint |= need;
                        if let Some(r) = rest {
                            repaint |= a.shell.apply(r);
                        }
                    }
                    // replies free lane slots: the queued misses go out now
                    a.shell.refill(&mut a.pool);
                    let st = {
                        let App { view, shell, pool, stress, .. } = &mut *a;
                        stress.as_mut().map(|s| s.step(view, shell, pool, pt))
                    };
                    arm_watch(hwnd, a);
                    arm_ctx(hwnd, a);
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
            WM_APP_OPEN => {
                open_launch(hwnd);
                LRESULT(0)
            }
            WM_APP_PLACES => {
                let got = PLACES.lock().ok().and_then(|mut g| g.take());
                if let Some((drives, places, wsl)) = got {
                    with(|a| {
                        a.view.side.drives = drives;
                        a.view.side.places = places;
                        a.view.side.wsl = wsl;
                    });
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                cancel_row_tip(hwnd);
                let dy = ((wp.0 >> 16) as u16 as i16) as f32 / 120.0;
                // wheel positions are screen px
                let mut p = POINT { x: lo(lp), y: hi(lp) };
                let _ = ScreenToClient(hwnd, &mut p);
                let s = dpi_scale();
                // MK_CONTROL / MK_SHIFT ride in wParam: the key state at the time of the wheel message
                let mut m = mods();
                m.ctrl |= wp.0 & 0x0008 != 0;
                m.shift |= wp.0 & 0x0004 != 0;
                push(hwnd, Event::Wheel { pos: Pt::new(p.x as f32 / s, p.y as f32 / s), dy, dx: 0.0, mods: m });
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONDBLCLK | WM_MBUTTONDOWN | WM_MBUTTONDBLCLK | WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                if matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONDBLCLK | WM_MBUTTONDOWN | WM_MBUTTONDBLCLK) {
                    cancel_row_tip(hwnd);
                }
                button_event(hwnd, msg, lp);
                LRESULT(0)
            }
            // Mouse Back (XBUTTON1) / Forward (XBUTTON2). Return TRUE (1) on both
            // down and up so DefWindowProc does not double-fire WM_APPCOMMAND.
            WM_XBUTTONDOWN | WM_XBUTTONDBLCLK => {
                if let Some(forward) = xbutton_nav(wp) {
                    if mouse_nav(hwnd, forward) {
                        invalidate(hwnd);
                    }
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            WM_XBUTTONUP => {
                if xbutton_nav(wp).is_some() {
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            WM_APPCOMMAND => {
                if let Some(forward) = appcommand_nav(lp) {
                    if mouse_nav(hwnd, forward) {
                        invalidate(hwnd);
                    }
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
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
                cancel_row_tip(hwnd);
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
                // keyboard (menu key): at the cursor row. While the own menu
                // is open the key went to it as a KeyDown (CM8: the focused
                // row's edit menu), so it does not reopen the menu.
                if !own_menu_open() {
                    key_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_SYSKEYDOWN | WM_SYSKEYUP if wp.0 as u16 == VK_F10.0 => {
                // F10 must not reach DefWindowProc: it enters menu mode on the
                // UI thread, and while any menu is active the worker's
                // SetForegroundWindow fails, so Escape never reaches its menu.
                if msg == WM_SYSKEYDOWN && GetKeyState(VK_SHIFT.0 as i32) < 0 {
                    if own_menu_open() {
                        // CM8: Shift+F10 in the own menu opens the focused row's edit menu
                        push(hwnd, Event::KeyDown { key: Key::F(10), mods: mods(), repeat: lp.0 & (1 << 30) != 0 });
                    } else {
                        key_menu(hwnd);
                    }
                }
                LRESULT(0)
            }
            // Alt+key shortcuts (Alt+Left/Right/Up, Alt+D) arrive as SYSKEY messages
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                cancel_row_tip(hwnd);
                let repeat = lp.0 & (1 << 30) != 0;
                let key = map_key(wp.0 as u16);
                // Escape while a context menu is still inside its reveal grace window cancels it.
                if key == Key::Escape
                    && with(|a| {
                        if a.view.ctx.as_ref().is_some_and(|c| c.open && !c.is_revealed()) {
                            a.view.ctx = None;
                            arm_ctx(hwnd, a);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false)
                {
                    return LRESULT(0);
                }
                // bare Alt (menu activation) is ignored; every other Alt combo goes to the kit
                if matches!(VIRTUAL_KEY(wp.0 as u16), VK_MENU | VK_LMENU | VK_RMENU) {
                    invalidate(hwnd);
                    return LRESULT(0);
                }
                // dedicated keyboard Browser Back / Forward keys
                if matches!(VIRTUAL_KEY(wp.0 as u16), VK_BROWSER_BACK | VK_BROWSER_FORWARD) {
                    if !repeat && mouse_nav(hwnd, VIRTUAL_KEY(wp.0 as u16) == VK_BROWSER_FORWARD) {
                        invalidate(hwnd);
                    }
                    return LRESULT(0);
                }
                // file shortcuts of the list (copy, paste, delete, undo, Alt+Enter Properties)
                if !repeat && shellops::on_key(hwnd, wp.0 as u16, mods()) {
                    return LRESULT(0);
                }
                push(hwnd, Event::KeyDown { key, mods: mods(), repeat });
                LRESULT(0)
            }
            WM_KEYUP | WM_SYSKEYUP => {
                // the own menu underlines accelerators while Alt is held:
                // releasing it repaints
                if matches!(VIRTUAL_KEY(wp.0 as u16), VK_MENU | VK_LMENU | VK_RMENU) {
                    invalidate(hwnd);
                }
                push(hwnd, Event::KeyUp { key: map_key(wp.0 as u16), mods: mods() });
                LRESULT(0)
            }
            // Alt+letter / Alt+Enter (Alt+D, Alt+Enter, own menu accelerators) arrive
            // as WM_SYSCHAR after WM_SYSKEYDOWN; DefWindowProc would only beep.
            // Keep Alt+Space for the window's system menu.
            WM_SYSCHAR if wp.0 != ' ' as usize => LRESULT(0),
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
            WM_TIMER if wp.0 == CTX_WARM_TIMER => {
                let _ = KillTimer(Some(hwnd), CTX_WARM_TIMER);
                with(|a| {
                    let icon_px = ((16.0 * a.view.dpi / 96.0 * (a.ui.theme.font_size_sm / 12.0).max(1.0)).round() as u32).clamp(16, 64);
                    ctxui::prewarm(&mut a.pool, &a.view.tab().path, icon_px);
                });
                LRESULT(0)
            }
            WM_TIMER if wp.0 == ctxui::TIMER_ID => {
                // One-shot grace/hover deadline or 10 FPS loading tick;
                // `after_paint` re-arms with the next interval or leaves it off.
                let _ = KillTimer(Some(hwnd), ctxui::TIMER_ID);
                with(|a| a.ctx_timer = None);
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == view_tooltip::TIMER_ID => {
                let _ = KillTimer(Some(hwnd), view_tooltip::TIMER_ID);
                let show = with(|a| {
                    a.row_tip_timer = None;
                    a.view.row_tip.on_timer()
                })
                .unwrap_or(false);
                if show {
                    invalidate(hwnd);
                }
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
            WM_APP_UPDATE => {
                // "Download and restart" applied the update: quit through the normal close path
                if updates::take_restart_request() {
                    AFTER_EXIT.store(1, Ordering::Release);
                    let first = with(|a| !std::mem::replace(&mut a.closing, true)).unwrap_or(true);
                    if first {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    return LRESULT(0);
                }
                // only Settings > Updates shows update state: repaint when it is visible
                invalidate(hwnd);
                LRESULT(0)
            }
            shellops::WM_APP_SHELLOPS => {
                shellops::on_app(hwnd);
                LRESULT(0)
            }
            // CM10: a screen reader invoked or collapsed a menu row
            tsubame_win::uia::WM_APP_UIA => {
                with(|a| {
                    if let Some(c) = a.view.ctx.as_mut() {
                        c.run_a11y_requests();
                    } else if a.view.nav_menu_open() {
                        a.view.run_nav_menu_a11y();
                    } else if a.view.tags.picker_open() {
                        // the tags picker's rows (a popup, not a context menu)
                        for r in tsubame_win::uia::take_requests() {
                            if !r.collapse && r.lv == 0 {
                                a.view.tags.a11y_invoke(r.key);
                            }
                        }
                    }
                    // the file views' rows (rows, Tree, Miller): docs/uia-views.md
                    a.view.run_view_requests();
                });
                invalidate(hwnd);
                LRESULT(0)
            }
            // CM10: a screen reader asks for the window's UI Automation provider
            // (the own-drawn menu's rows); the repaint publishes an open menu
            WM_GETOBJECT => match tsubame_win::uia::on_get_object(hwnd, wp, lp) {
                Some(r) => {
                    invalidate(hwnd);
                    r
                }
                None => DefWindowProcW(hwnd, msg, wp, lp),
            },
            WM_TIMER if wp.0 == OPS_TIMER => {
                // file-op events: progress, conflicts (a dialog), results
                shellops::on_ops_timer(hwnd);
                with(|a| arm_ops(hwnd, a));
                LRESULT(0)
            }
            WM_TIMER if wp.0 == PARK_TIMER => {
                on_park_timer(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == SAVE_TIMER => {
                // the debounce window of a config/session change is over
                save_flush(hwnd, false);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == WATCH_TIMER => {
                let lost = with(|a| {
                    let lost = a.pool.check_hung();
                    if let Some(c) = a.view.ctx.as_mut()
                        && lost.contains(&c.ticket)
                    {
                        // the shell part hung: the failure row with Retry
                        c.lost();
                    }
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
            WM_CLOSE => {
                let trace = tsubame_win::lagtrace::on();
                let t_all = trace.then(Instant::now);
                // Read geometry before hiding, then hide immediately so disk
                // flushes (`sync_all`) and teardown never leave a frozen window
                // on screen.
                let geom = window_state(hwnd);
                let _ = ShowWindow(hwnd, SW_HIDE);
                if let Some(t) = t_all {
                    tsubame_win::lagtrace::record("close.wm_close.hide", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                }
                let t_save = trace.then(Instant::now);
                save_flush_geom(hwnd, geom, true);
                with(|a| a.saved_on_close = true);
                if let Some(t) = t_save {
                    tsubame_win::lagtrace::record("close.wm_close.save_flush", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                }
                let t_dest = trace.then(Instant::now);
                let _ = DestroyWindow(hwnd);
                if let Some(t) = t_dest {
                    tsubame_win::lagtrace::record("close.wm_close.destroy_window", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                }
                if let Some(t) = t_all {
                    tsubame_win::lagtrace::record("close.wm_close.total", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let trace = tsubame_win::lagtrace::on();
                let t_all = trace.then(Instant::now);
                let t = trace.then(Instant::now);
                tsubame_win::uia::shutdown(hwnd);
                if let Some(t) = t {
                    tsubame_win::lagtrace::record("close.wm_destroy.uia", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                }
                // Also reached directly when bench/timing/stress calls DestroyWindow.
                let already_saved = with(|a| a.saved_on_close).unwrap_or(false);
                if !already_saved {
                    let t = trace.then(Instant::now);
                    save_flush(hwnd, true);
                    if let Some(t) = t {
                        tsubame_win::lagtrace::record("close.wm_destroy.save_flush", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                    }
                }
                let t = trace.then(Instant::now);
                shellops::destroy(hwnd);
                if let Some(t) = t {
                    tsubame_win::lagtrace::record("close.wm_destroy.shellops", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), false);
                }
                PostQuitMessage(0);
                if let Some(t) = t_all {
                    tsubame_win::lagtrace::record("close.wm_destroy.total", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!(""), true);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xbutton_and_appcommand_decode_back_and_forward() {
        assert_eq!(xbutton_nav(WPARAM((XBUTTON1 as usize) << 16)), Some(false));
        assert_eq!(xbutton_nav(WPARAM((XBUTTON2 as usize) << 16)), Some(true));
        assert_eq!(xbutton_nav(WPARAM(0)), None);

        // WM_APPCOMMAND puts command in low 12 bits of HIWORD, device in FAPPCOMMAND_MASK bits
        let mk_appcmd = |cmd: u32, dev: u32| LPARAM(((cmd | dev) as isize) << 16);
        assert_eq!(appcommand_nav(mk_appcmd(APPCOMMAND_BROWSER_BACKWARD, 0x8000)), Some(false));
        assert_eq!(appcommand_nav(mk_appcmd(APPCOMMAND_BROWSER_FORWARD, 0x8000)), Some(true));
        assert_eq!(appcommand_nav(mk_appcmd(3, 0x8000)), None); // APPCOMMAND_BROWSER_REFRESH
    }
}
