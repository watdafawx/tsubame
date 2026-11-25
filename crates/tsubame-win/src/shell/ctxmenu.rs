//! The shell's context menu as data (CM1), for the own-drawn menu.
//!
//! Runs on a shell STA worker, like the native menu in `menu.rs`, with the
//! same hidden host window and `IContextMenu2/3` message forwarding:
//! 1. bind the items (or the folder background) and `QueryContextMenu` into
//!    an HMENU that is never shown;
//! 2. walk the top level into [`RawItem`]s and send it ([`Part::Native`],
//!    submenus marked lazy) at once;
//! 3. lazy (CM6, the default): when the UI asks ([`CtxHandle::expand`] with
//!    the token a submenu got in step 2), send `WM_INITMENUPOPUP` to that one
//!    submenu through `HandleMenuMsg2` (how "Send to", "Open with" and most
//!    extensions fill theirs), walk its level and send it as
//!    [`Part::Submenu`]; eager (`CtxOpts::submenus`): walk every submenu now
//!    and send the full tree again;
//! 4. keep the menu alive and wait, pumping messages, for the UI's pick
//!    ([`CtxHandle::invoke`]) or close; then `InvokeCommand` by id, without
//!    any popup, and release everything.
//!
//! Icons: `hbmpItem` bitmaps belong to the handler that inserted them (it
//! deletes them, `DestroyMenu` does not); they are only read here.
//! `HBMMENU_CALLBACK` icons and owner-drawn items are drawn by the handler
//! through `WM_DRAWITEM` into a DIB of ours, which is deleted right after.
//! Every GDI object made here is freed here.

use super::ctxmodel::{IconCache, MenuBatch, Part, RawItem, build_native};
use super::packaged;
use super::menu::{self, CMIC_MASK_SHIFT_DOWN, CMIC_MASK_UNICODE, ID_FIRST, ID_LAST, Pidls};
use crate::bitmap::Bitmap;
use crate::wide;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, Sleep};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODA_DRAWENTIRE, ODS_FLAGS, ODT_MENU};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCSTR, PCWSTR, PSTR, PWSTR};

const CMIC_MASK_CONTROL_DOWN: u32 = 0x4000_0000;
const CMIC_MASK_FLAG_NO_UI: u32 = 0x0000_0400;

/// What to build.
#[derive(Clone, Debug)]
pub struct CtxOpts {
    /// `paths[0]` is a folder; its background menu
    pub background: bool,
    /// Shift held: `CMF_EXTENDEDVERBS`
    pub extended: bool,
    /// add `CMF_CANRENAME` (the app can rename inline, like Explorer)
    pub can_rename: bool,
    /// walk every submenu up front (eager, step 3); off = lazy: the top level,
    /// submenus when [`CtxHandle::expand`] asks
    pub submenus: bool,
    /// icon edge in px (16 at 100 %)
    pub icon_px: u32,
    /// release the menu if the UI neither picks nor closes it in this time
    pub linger: Duration,
    /// test only: `Sleep(ms)` before binding, without pumping (a hung handler)
    pub debug_hang_ms: u32,
    /// debug only: `Sleep(ms)` before binding, then build as usual (the
    /// loading frame; past the hang limit, the failure frame)
    pub debug_delay_ms: u32,
    /// debug only: `Sleep(ms)` before filling a lazy submenu, without
    /// pumping (past the 4 s hang limit: a hung submenu handler)
    pub debug_sub_delay_ms: u32,
    /// merge the Win11 packaged (`IExplorerCommand`) commands after the
    /// classic top level (CM7)
    pub packaged: bool,
    /// debug only: `Sleep(ms)` before the packaged step (the loading line
    /// stays after the classic rows are in)
    pub debug_pkg_delay_ms: u32,
}

impl Default for CtxOpts {
    fn default() -> Self {
        CtxOpts { background: false, extended: false, can_rename: true, submenus: true, icon_px: 16, linger: Duration::from_secs(600), debug_hang_ms: 0, debug_delay_ms: 0, debug_sub_delay_ms: 0, packaged: true, debug_pkg_delay_ms: 0 }
    }
}

/// A pick sent from the UI to the worker holding the menu.
#[derive(Clone, Debug)]
pub enum CtxCmd {
    /// `InvokeCommand` for command id `id` (from [`super::ctxmodel::MenuItem::id`])
    /// at screen point (x, y); `no_ui` adds `CMIC_MASK_FLAG_NO_UI`
    Invoke { id: u32, x: i32, y: i32, shift: bool, ctrl: bool, no_ui: bool },
    /// fill the lazy submenu with token `sub` (CM6): `WM_INITMENUPOPUP`,
    /// settle, walk one level, answer with [`Part::Submenu`]
    Expand { sub: u32 },
    Close,
}

/// Shared between the UI's [`CtxHandle`] and the worker: a command queue and
/// an auto-reset event the worker waits on while it pumps.
pub struct CtxSession {
    cmds: Mutex<VecDeque<CtxCmd>>,
    event: super::SendHandle,
}

impl CtxSession {
    fn push(&self, c: CtxCmd) {
        self.cmds.lock().unwrap().push_back(c);
        unsafe {
            let _ = SetEvent(self.event.0);
        }
    }
}

impl Drop for CtxSession {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.event.0);
        }
    }
}

/// The UI's side of one menu. Dropping it closes the menu on the worker.
pub struct CtxHandle {
    pub(super) session: Arc<CtxSession>,
}

impl CtxHandle {
    pub fn new() -> CtxHandle {
        let ev = unsafe { CreateEventW(None, false, false, None).expect("CreateEventW") };
        CtxHandle { session: Arc::new(CtxSession { cmds: Mutex::new(VecDeque::new()), event: super::SendHandle(ev) }) }
    }
    pub fn invoke(&self, id: u32, x: i32, y: i32, shift: bool, ctrl: bool) {
        self.session.push(CtxCmd::Invoke { id, x, y, shift, ctrl, no_ui: false });
    }
    pub fn send(&self, c: CtxCmd) {
        self.session.push(c);
    }
    pub fn close(&self) {
        self.session.push(CtxCmd::Close);
    }
    /// Asks for the children of lazy submenu `sub` (answered as
    /// [`Part::Submenu`] of the menu's generation).
    pub fn expand(&self, sub: u32) {
        self.session.push(CtxCmd::Expand { sub });
    }
}

impl Default for CtxHandle {
    fn default() -> Self {
        CtxHandle::new()
    }
}

impl Drop for CtxHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// Worker side. `emit` sends a batch to the UI now (false: the pool gave
/// this worker up, nobody listens); `waiting(true)` is called when
/// the menu is built and only waits for the user (the pool stops treating
/// the job as possibly hung), `waiting(false)` while it fills a lazy
/// submenu (a handler can hang there: the 4 s watchdog applies again).
/// Returns the last batch (`Closed`).
#[allow(clippy::too_many_arguments)]
pub(super) fn run(host: Option<HWND>, ui: HWND, paths: &[String], generation: u64, opts: &CtxOpts, session: &CtxSession, emit: &dyn Fn(MenuBatch) -> bool, waiting: &dyn Fn(bool)) -> MenuBatch {
    let batch = |part: Part, timing: [u64; 3]| MenuBatch { generation, part, timing };
    if opts.debug_hang_ms > 0 {
        unsafe { Sleep(opts.debug_hang_ms) };
        return batch(Part::Failed("debug hang".into()), [0; 3]);
    }
    if opts.debug_delay_ms > 0 {
        unsafe { Sleep(opts.debug_delay_ms) };
    }
    let built = match build(host, paths, opts) {
        Ok(b) => b,
        Err(e) => return batch(Part::Failed(e), [0; 3]),
    };
    let Built { cm, hmenu, host, bind_us, query_us } = built;
    menu::set_handlers(Some(&cm));
    let mut icons = IconCache::new(super::ctxmodel::ICON_CACHE_CAP);
    let t = Instant::now();
    let mut ctx = Walk { cm: &cm, host, icon_px: opts.icon_px.clamp(8, 64), drawn: 0, subs: Vec::new() };
    let top = ctx.level(hmenu, false, 0);
    let walk_us = t.elapsed().as_micros() as u64;
    let timing = [bind_us, query_us, walk_us];
    // false: the pool gave this worker up while it built; release at once
    let mut alive = emit(batch(Part::Native(build_native(&top, &mut icons)), timing));
    if !icons.is_empty() {
        emit(batch(Part::Icons(icons.entries()), timing));
    }
    if alive && opts.submenus && top.iter().any(|r| r.submenu.is_some()) {
        let t = Instant::now();
        let full = ctx.level(hmenu, true, 0);
        let sub_us = t.elapsed().as_micros() as u64;
        let before = icons.len();
        let items = build_native(&full, &mut icons);
        alive = emit(batch(Part::Native(items), [bind_us, query_us, walk_us + sub_us]));
        if icons.len() != before {
            emit(batch(Part::Icons(icons.entries()), timing));
        }
    }
    // CM7: the packaged (Win11) commands, after the classic rows are out so
    // they never delay them; the loading line stays until they are in
    let mut pkg = None;
    if alive && opts.packaged {
        if opts.debug_pkg_delay_ms > 0 {
            unsafe { Sleep(opts.debug_pkg_delay_ms) };
        }
        let t = Instant::now();
        if let Some(mut s) = packaged::Session::new(paths, opts.icon_px.clamp(8, 64)) {
            let classes: Vec<(packaged::ItemClass, String)> = if opts.background { vec![(packaged::ItemClass::Background, String::new())] } else { paths.iter().map(|p| packaged::class_of(p)).collect() };
            // a CLSID the classic menu already lists as a verb is not made again
            let skip: Vec<String> = top.iter().filter_map(|r| r.verb.as_deref()).filter(|v| v.starts_with('{')).map(packaged::norm_clsid).collect();
            let raw = s.top(&classes, &skip);
            let us = t.elapsed().as_micros() as u64;
            if !raw.is_empty() {
                let before = icons.len();
                let items = build_native(&raw, &mut icons);
                alive = emit(batch(Part::Packaged(items), [bind_us, query_us, us]));
                if alive && icons.len() != before {
                    emit(batch(Part::Icons(icons.entries()), timing));
                }
            } else {
                alive = emit(batch(Part::Packaged(Vec::new()), [bind_us, query_us, us]));
            }
            pkg = Some(s);
        }
    }
    if alive {
        // every row is in: the UI ends its loading line
        emit(batch(Part::Complete, timing));
    }
    waiting(true);

    // wait for the pick, pumping (handlers post to this STA; Properties and
    // other dialogs run their own windows on it after InvokeCommand)
    let deadline = Instant::now() + if alive { opts.linger } else { Duration::ZERO };
    let mut last = batch(Part::Closed, timing);
    let mut expanded: Vec<u32> = Vec::new();
    'wait: loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let ms = left.as_millis().min(INFINITE as u128 - 1) as u32;
        let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&[session.event.0]), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        if r == WAIT_TIMEOUT {
            break;
        }
        pump();
        // the event is auto-reset: take every queued command now
        loop {
            let next = session.cmds.lock().unwrap().pop_front();
            let Some(c) = next else { break };
            match c {
                CtxCmd::Close => break 'wait,
                CtxCmd::Invoke { id, x, y, shift, ctrl, no_ui } => {
                    let (verb, res) = match &pkg {
                        Some(p) if packaged::is_id(id) => foreground_for(host, ui, || p.invoke(id)),
                        _ => invoke(&cm, host, ui, id, x, y, shift, ctrl, no_ui),
                    };
                    emit(batch(Part::Invoked { id, verb, result: res }, timing));
                    break 'wait;
                }
                CtxCmd::Expand { sub } => {
                    if expanded.contains(&sub) {
                        continue;
                    }
                    expanded.push(sub);
                    waiting(false);
                    if opts.debug_sub_delay_ms > 0 {
                        unsafe { Sleep(opts.debug_sub_delay_ms) };
                    }
                    let t = Instant::now();
                    let raw = if packaged::is_sub(sub) { pkg.as_mut().and_then(|p| p.expand(sub)) } else { ctx.expand(sub) };
                    let items = raw.map(|raw| {
                        // one level: nested submenus stay lazy, so no deeper icons
                        let any = raw.iter().any(|r| r.icon.is_some());
                        (build_native(&raw, &mut icons), any)
                    });
                    let us = t.elapsed().as_micros() as u64;
                    let (items, new_icons) = items.unwrap_or_default();
                    // an unknown token still answers (empty), so the UI stops loading
                    if !emit(batch(Part::Submenu { sub, items }, [bind_us, query_us, us])) {
                        break 'wait;
                    }
                    if new_icons {
                        emit(batch(Part::Icons(icons.entries()), timing));
                    }
                    waiting(true);
                }
            }
        }
    }
    drop(pkg);
    menu::set_handlers(None);
    unsafe {
        let _ = DestroyMenu(hmenu);
    }
    drop(cm);
    last.part = Part::Closed;
    last
}

/// Pre-warm (see [`super::Request::CtxWarm`]): builds the background menu of
/// `folder` and the item menu of `folder` itself the way a click would
/// (bind, `QueryContextMenu`, top-level walk, packaged scan), then releases
/// them. The handler DLLs stay loaded in the process, so the first real menu
/// pays warm costs. Nothing is sent to the UI.
pub(super) fn warm(host: Option<HWND>, folder: &str, icon_px: u32) {
    let paths = [folder.to_string()];
    for background in [true, false] {
        let opts = CtxOpts { background, submenus: false, icon_px, ..Default::default() };
        let Ok(Built { cm, hmenu, host, .. }) = build(host, &paths, &opts) else { continue };
        menu::set_handlers(Some(&cm));
        let px = icon_px.clamp(8, 64);
        let mut ctx = Walk { cm: &cm, host, icon_px: px, drawn: 0, subs: Vec::new() };
        let top = ctx.level(hmenu, false, 0);
        if let Some(mut s) = packaged::Session::new(&paths, px) {
            let classes: Vec<(packaged::ItemClass, String)> = if background { vec![(packaged::ItemClass::Background, String::new())] } else { paths.iter().map(|p| packaged::class_of(p)).collect() };
            let skip: Vec<String> = top.iter().filter_map(|r| r.verb.as_deref()).filter(|v| v.starts_with('{')).map(packaged::norm_clsid).collect();
            let _ = s.top(&classes, &skip);
        }
        pump();
        menu::set_handlers(None);
        unsafe {
            let _ = DestroyMenu(hmenu);
        }
        drop(cm);
    }
}

fn pump() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

const SETTLE_QUIET_MS: u64 = 40;
const SETTLE_MAX_MS: u64 = 300;

/// Pumps until `sub`'s item count is unchanged for `SETTLE_QUIET_MS`
/// (capped at `SETTLE_MAX_MS`). Returns the final count.
fn settle(sub: HMENU) -> i32 {
    let start = Instant::now();
    let mut n = unsafe { GetMenuItemCount(Some(sub)) };
    let mut still = Instant::now();
    loop {
        pump();
        let now = unsafe { GetMenuItemCount(Some(sub)) };
        if now != n {
            n = now;
            still = Instant::now();
        }
        if still.elapsed().as_millis() as u64 >= SETTLE_QUIET_MS || start.elapsed().as_millis() as u64 >= SETTLE_MAX_MS {
            return n;
        }
        unsafe { MsgWaitForMultipleObjectsEx(None, 5, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
    }
}

struct Built {
    cm: IContextMenu,
    hmenu: HMENU,
    host: HWND,
    bind_us: u64,
    query_us: u64,
}

/// Flags Explorer passes for a normal right-click on items.
pub fn query_flags(opts: &CtxOpts) -> u32 {
    let mut f = CMF_NORMAL | CMF_EXPLORE;
    if !opts.background {
        f |= CMF_ITEMMENU;
        if opts.can_rename {
            f |= CMF_CANRENAME;
        }
    }
    if opts.extended {
        f |= CMF_EXTENDEDVERBS;
    }
    f
}

fn build(host: Option<HWND>, paths: &[String], opts: &CtxOpts) -> Result<Built, String> {
    let host = host.ok_or("no host window")?;
    if paths.is_empty() {
        return Err("no paths".into());
    }
    if !opts.background && paths.iter().any(|p| !menu::parent_of(p).eq_ignore_ascii_case(menu::parent_of(&paths[0]))) {
        return Err("items must share one folder".into());
    }
    let t = Instant::now();
    let pidls = parse(paths)?;
    let cm = if opts.background { menu::background_menu(pidls.0[0], host) } else { menu::item_menu(&pidls, host) }?;
    let bind_us = t.elapsed().as_micros() as u64;
    let hmenu = unsafe { CreatePopupMenu() }.map_err(|e| format!("CreatePopupMenu {:?}", e.code()))?;
    let t = Instant::now();
    let hr = unsafe { cm.QueryContextMenu(hmenu, 0, ID_FIRST, ID_LAST, query_flags(opts)) };
    let query_us = t.elapsed().as_micros() as u64;
    if hr.is_err() {
        unsafe {
            let _ = DestroyMenu(hmenu);
        }
        return Err(format!("QueryContextMenu {hr:?}"));
    }
    Ok(Built { cm, hmenu, host, bind_us, query_us })
}

pub(super) fn parse(paths: &[String]) -> Result<Pidls, String> {
    let mut pidls = Pidls(Vec::with_capacity(paths.len()));
    for p in paths {
        let w = wide(p);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) }.map_err(|e| format!("SHParseDisplayName {:?} for {p}", e.code()))?;
        pidls.0.push(pidl);
    }
    Ok(pidls)
}

/// `InvokeCommand` by id. Returns the verb (if the handler names one) and the result.
#[allow(clippy::too_many_arguments)]
fn invoke(cm: &IContextMenu, host: HWND, ui: HWND, id: u32, x: i32, y: i32, shift: bool, ctrl: bool, no_ui: bool) -> (Option<String>, Result<(), String>) {
    if !(ID_FIRST..=ID_LAST).contains(&id) {
        return (None, Err(format!("id {id} is not a shell command")));
    }
    let off = (id - ID_FIRST) as usize;
    let verb = command_string(cm, off, GCS_VERBW);
    let mut mask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE;
    if shift {
        mask |= CMIC_MASK_SHIFT_DOWN;
    }
    if ctrl {
        mask |= CMIC_MASK_CONTROL_DOWN;
    }
    if no_ui {
        mask |= CMIC_MASK_FLAG_NO_UI;
    }
    let ici = CMINVOKECOMMANDINFOEX {
        cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: mask,
        hwnd: host,
        lpVerb: PCSTR(off as *const u8),
        lpVerbW: PCWSTR(off as *const u16),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: POINT { x, y },
        ..Default::default()
    };
    let res = foreground_for(host, ui, || unsafe { cm.InvokeCommand(&ici as *const _ as *const CMINVOKECOMMANDINFO) });
    (verb, res.map_err(|e| format!("InvokeCommand {:?}", e.code())))
}

/// Runs an invoke with the host window in front: dialogs the verb opens
/// (Properties, Open with) are owned by the host; it may take the
/// foreground because the app has it. The app gets it back after.
fn foreground_for<R>(host: HWND, ui: HWND, f: impl FnOnce() -> R) -> R {
    unsafe {
        let _ = SetForegroundWindow(host);
        let r = f();
        if GetForegroundWindow() == host && !ui.is_invalid() {
            let _ = SetForegroundWindow(ui);
        }
        r
    }
}

fn command_string(cm: &IContextMenu, off: usize, kind: u32) -> Option<String> {
    let mut buf = [0u16; 260];
    unsafe { cm.GetCommandString(off, kind, None, PSTR(buf.as_mut_ptr() as *mut u8), buf.len() as u32) }.ok()?;
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    // a few handlers write ANSI into the wide buffer: one char of 2 bytes
    (!s.is_empty() && s.chars().all(|c| !c.is_control())).then_some(s)
}

struct Walk<'a> {
    cm: &'a IContextMenu,
    host: HWND,
    icon_px: u32,
    /// owner-draw / callback draws done (for the report)
    drawn: u32,
    /// lazy submenus left unwalked: (submenu, its position in the parent,
    /// depth); token = index + 1
    subs: Vec<(HMENU, u32, usize)>,
}

impl Walk<'_> {
    /// Items of `hmenu`. `deep`: send `WM_INITMENUPOPUP` to submenus and walk
    /// them; else submenus get a token for [`Walk::expand`].
    fn level(&mut self, hmenu: HMENU, deep: bool, depth: usize) -> Vec<RawItem> {
        let n = unsafe { GetMenuItemCount(Some(hmenu)) }.clamp(0, super::ctxmodel::MAX_ITEMS as i32) as u32;
        let mut out = Vec::with_capacity(n as usize);
        for pos in 0..n {
            let Some((mut r, sub, bmp, data)) = self.item(hmenu, pos) else { continue };
            if let Some(sub) = sub {
                let room = depth + 1 < super::ctxmodel::MAX_DEPTH;
                r.submenu = Some(if deep && room {
                    self.init_popup(sub, pos);
                    self.level(sub, true, depth + 1)
                } else {
                    if room && self.subs.len() < super::ctxmodel::MAX_ITEMS {
                        self.subs.push((sub, pos, depth + 1));
                        r.sub = self.subs.len() as u32;
                    }
                    Vec::new()
                });
            }
            if r.icon_callback || r.ownerdraw {
                r.icon = self.draw_item(r.id, data, hmenu);
            } else if let Some(b) = bmp {
                r.icon = read_bitmap(b, self.icon_px).map(Arc::new);
            }
            out.push(r);
        }
        out
    }

    /// Fills lazy submenu `sub` the way the menu loop would before showing
    /// it, and walks its one level (deeper submenus get tokens of their own).
    /// None: not a token of this menu.
    fn expand(&mut self, sub: u32) -> Option<Vec<RawItem>> {
        let &(h, pos, depth) = self.subs.get((sub as usize).checked_sub(1)?)?;
        self.init_popup(h, pos);
        Some(self.level(h, false, depth))
    }

    fn item(&self, hmenu: HMENU, pos: u32) -> Option<(RawItem, Option<HMENU>, Option<HBITMAP>, usize)> {
        let mut mii = MENUITEMINFOW { cbSize: size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_STRING | MIIM_BITMAP | MIIM_DATA, ..Default::default() };
        unsafe { GetMenuItemInfoW(hmenu, pos, true, &mut mii) }.ok()?;
        let mut text = String::new();
        if mii.cch > 0 && mii.fType.0 & (MFT_BITMAP.0 | MFT_SEPARATOR.0) == 0 {
            let mut buf = vec![0u16; mii.cch as usize + 1];
            let mut m2 = MENUITEMINFOW { cbSize: size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_STRING, dwTypeData: PWSTR(buf.as_mut_ptr()), cch: buf.len() as u32, ..Default::default() };
            if unsafe { GetMenuItemInfoW(hmenu, pos, true, &mut m2) }.is_ok() {
                text = String::from_utf16_lossy(&buf[..m2.cch as usize]);
            }
        }
        let t = mii.fType.0;
        let s = mii.fState.0;
        let sub = (!mii.hSubMenu.is_invalid()).then_some(mii.hSubMenu);
        let mut r = RawItem {
            id: mii.wID,
            text,
            separator: t & MFT_SEPARATOR.0 != 0,
            ownerdraw: t & MFT_OWNERDRAW.0 != 0,
            radio: t & MFT_RADIOCHECK.0 != 0,
            menubreak: t & (MFT_MENUBREAK.0 | MFT_MENUBARBREAK.0) != 0,
            disabled: s & MFS_DISABLED.0 != 0,
            checked: s & MFS_CHECKED.0 != 0,
            default: s & MFS_DEFAULT.0 != 0,
            ..Default::default()
        };
        let hb = mii.hbmpItem.0 as isize;
        r.icon_callback = hb == HBMMENU_CALLBACK.0 as isize;
        // 1..=11 are HBMMENU_* system glyphs (close, minimize, ...): none in context menus
        let bmp = (hb > 11).then_some(mii.hbmpItem);
        if sub.is_none() && (ID_FIRST..=ID_LAST).contains(&r.id) && !r.separator {
            let off = (r.id - ID_FIRST) as usize;
            r.verb = command_string(self.cm, off, GCS_VERBW);
            r.help = command_string(self.cm, off, GCS_HELPTEXTW);
        }
        Some((r, sub, bmp, mii.dwItemData))
    }

    fn init_popup(&self, sub: HMENU, pos: u32) {
        // the same message the menu loop sends before showing a submenu
        let wp = WPARAM(sub.0 as usize);
        let lp = LPARAM(pos as isize);
        if let Ok(cm3) = self.cm.cast::<IContextMenu3>() {
            let mut lr = LRESULT(0);
            let _ = unsafe { cm3.HandleMenuMsg2(WM_INITMENUPOPUP, wp, lp, Some(&mut lr)) };
        } else if let Ok(cm2) = self.cm.cast::<IContextMenu2>() {
            let _ = unsafe { cm2.HandleMenuMsg(WM_INITMENUPOPUP, wp, lp) };
        }
        // Some handlers fill the popup later from posted messages or a
        // helper thread ("Send to" adds its first target at once and the
        // rest a few ms later). Pump until the count holds still for
        // SETTLE_QUIET_MS, at most SETTLE_MAX_MS.
        settle(sub);
        let _ = self.host;
    }

    /// Asks the handler to measure and draw item `id` into a DIB of ours,
    /// twice (on black and on white) so alpha can be recovered from GDI
    /// drawing, and keeps a square of the icon column. Frees the DIB and DC.
    fn draw_item(&mut self, id: u32, data: usize, hmenu: HMENU) -> Option<Arc<Bitmap>> {
        let cm3 = self.cm.cast::<IContextMenu3>().ok();
        let cm2 = self.cm.cast::<IContextMenu2>().ok();
        if cm3.is_none() && cm2.is_none() {
            return None;
        }
        let send = |msg: u32, lp: LPARAM| unsafe {
            if let Some(c) = &cm3 {
                let mut lr = LRESULT(0);
                c.HandleMenuMsg2(msg, WPARAM(0), lp, Some(&mut lr)).is_ok()
            } else {
                cm2.as_ref().unwrap().HandleMenuMsg(msg, WPARAM(0), lp).is_ok()
            }
        };
        let mut mis = MEASUREITEMSTRUCT { CtlType: ODT_MENU, CtlID: 0, itemID: id, itemWidth: 0, itemHeight: 0, itemData: data };
        if !send(WM_MEASUREITEM, LPARAM(&mut mis as *mut _ as isize)) {
            return None;
        }
        let (w, h) = (mis.itemWidth.clamp(1, 512), mis.itemHeight.clamp(1, 128));
        let side = h.min(w);
        let on = |bg: u32| -> Option<Vec<u32>> {
            unsafe {
                let dc = CreateCompatibleDC(None);
                if dc.is_invalid() {
                    return None;
                }
                let bi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() }, ..Default::default() };
                let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
                let Ok(dib) = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                    let _ = DeleteDC(dc);
                    return None;
                };
                let old = SelectObject(dc, dib.into());
                let px = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
                px.fill(bg);
                let mut dis = DRAWITEMSTRUCT { CtlType: ODT_MENU, CtlID: 0, itemID: id, itemAction: ODA_DRAWENTIRE, itemState: ODS_FLAGS(0), hwndItem: HWND(hmenu.0), hDC: dc, rcItem: RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 }, itemData: data };
                let ok = send(WM_DRAWITEM, LPARAM(&mut dis as *mut _ as isize));
                let _ = GdiFlush();
                let v = ok.then(|| px.to_vec());
                SelectObject(dc, old);
                let _ = DeleteObject(dib.into());
                let _ = DeleteDC(dc);
                v
            }
        };
        let black = on(0)?;
        let white = on(0x00ff_ffff)?;
        self.drawn += 1;
        // alpha = 255 - (white - black); colour = black / alpha (premultiplied = black)
        let mut out = Vec::with_capacity((side * side) as usize);
        for y in 0..side {
            for x in 0..side {
                let i = (y * w + x) as usize;
                let (b, wh) = (black[i], white[i]);
                let a = 255 - ((wh & 255).saturating_sub(b & 255)).min(255);
                out.push(a << 24 | (b & 0x00ff_ffff));
            }
        }
        if out.iter().all(|p| p >> 24 == 0) {
            return None;
        }
        Some(Arc::new(Bitmap { w: side, h: side, px: out }.fit_within(self.icon_px)))
    }
}

/// An item bitmap as premultiplied BGRA fitted into `px`. 32-bit bitmaps
/// with alpha are taken as PARGB (what menus expect), fixed up if a channel
/// exceeds alpha (straight alpha); bitmaps without alpha are made opaque.
fn read_bitmap(hbmp: HBITMAP, px: u32) -> Option<Bitmap> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(hbmp.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth as u32, bm.bmHeight.unsigned_abs());
        if w > 256 || h > 256 {
            return None;
        }
        let dc = CreateCompatibleDC(None);
        let mut bi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() }, ..Default::default() };
        let mut v = vec![0u32; (w * h) as usize];
        let n = GetDIBits(dc, hbmp, 0, h, Some(v.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        if n == 0 {
            return None;
        }
        let has_alpha = bm.bmBitsPixel == 32 && v.iter().any(|p| p >> 24 != 0);
        if !has_alpha {
            for p in &mut v {
                *p |= 0xff00_0000;
            }
        } else if v.iter().any(|&p| {
            let a = p >> 24;
            (p & 255) > a || ((p >> 8) & 255) > a || ((p >> 16) & 255) > a
        }) {
            for p in &mut v {
                let a = *p >> 24;
                let m = |c: u32| (c * a + 127) / 255;
                *p = a << 24 | m((*p >> 16) & 255) << 16 | m((*p >> 8) & 255) << 8 | m(*p & 255);
            }
        }
        Some(Bitmap { w, h, px: v }.fit_within(px))
    }
}
