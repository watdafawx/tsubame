//! The real shell context menu, shown from a worker (STA) thread.
//!
//! The menu's owner is a hidden `WS_POPUP` window created on, and pumped by,
//! the same worker thread. `TrackPopupMenuEx` runs the modal menu loop on
//! that thread, so the UI thread keeps painting. Owner-drawn and lazily
//! filled submenus ("Send to", "Open with", "New") need `WM_INITMENUPOPUP`,
//! `WM_DRAWITEM`, `WM_MEASUREITEM` and `WM_MENUCHAR` forwarded to
//! `IContextMenu3::HandleMenuMsg2` (or `IContextMenu2::HandleMenuMsg`).
//!
//! Not a message-only window (`HWND_MESSAGE`): those cannot be foreground,
//! and a menu whose owner is not foreground does not close when the user
//! clicks elsewhere.

use crate::wide;
use std::cell::{Cell, RefCell};
use std::sync::Once;
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCSTR, PCWSTR, PSTR, w};

#[derive(Debug, Default, Clone)]
pub struct MenuResult {
    /// the popup was shown (WM_ENTERIDLE / WM_INITMENUPOPUP reached the owner)
    pub shown: bool,
    /// top-level items
    pub items: u32,
    /// the chosen command's verb (or "#<id>" if the handler has none)
    pub chosen: Option<String>,
    /// InvokeCommand result
    pub invoked: Option<String>,
    pub error: Option<String>,
    /// parse + bind + GetUIObjectOf
    pub bind_us: u64,
    /// QueryContextMenu (loads the shell extensions)
    pub query_us: u64,
    /// job start until the menu first went idle on screen
    pub popup_us: Option<u64>,
    /// the owner used was a window of another thread
    pub foreign_owner: bool,
    /// SetForegroundWindow(owner) succeeded; when false, Escape and
    /// click-away do not reach the menu (e.g. the UI thread was in menu mode)
    pub foreground: bool,
    /// the chosen verb is one of `MenuOpts::intercept`: it was NOT invoked,
    /// the app runs it (e.g. "delete" through the file-op queue)
    pub intercepted: Option<String>,
    /// index into `MenuOpts::extras` of the chosen app entry
    pub extra: Option<usize>,
}

/// What the app adds to a shell menu.
#[derive(Debug, Default, Clone)]
pub struct MenuOpts {
    /// `paths[0]` is a folder and the menu is its background menu (right
    /// click on empty space): `IShellFolder::CreateViewObject(IContextMenu)`
    pub background: bool,
    /// app entries put on top of the shell's, then a separator; `""` adds a
    /// separator
    pub extras: Vec<String>,
    /// verbs the app runs itself instead of the shell (case-insensitive)
    pub intercept: Vec<String>,
}

pub(super) const ID_FIRST: u32 = 1;
pub(super) const ID_LAST: u32 = 0x7eff;
/// app entries: ID_EXTRA + index
const ID_EXTRA: u32 = 0x7f00;
pub(super) const CMIC_MASK_UNICODE: u32 = 0x4000;
pub(super) const CMIC_MASK_SHIFT_DOWN: u32 = 0x1000_0000;
const AUTO_CLOSE_TIMER: usize = 0x5453;

thread_local! {
    static CM2: RefCell<Option<IContextMenu2>> = const { RefCell::new(None) };
    static CM3: RefCell<Option<IContextMenu3>> = const { RefCell::new(None) };
    /// Some(ms) while a stress menu is up: end it `ms` after it is shown
    static AUTO_CLOSE: Cell<Option<u32>> = const { Cell::new(None) };
    static START: Cell<Option<Instant>> = const { Cell::new(None) };
    static SHOWN_US: Cell<Option<u64>> = const { Cell::new(None) };
}

static REGISTER: Once = Once::new();

/// Routes the host window's menu messages to `cm` (`None` stops it). The
/// own-drawn path (`ctxmenu`) uses it while it walks and invokes a menu.
pub(super) fn set_handlers(cm: Option<&IContextMenu>) {
    CM3.with(|c| *c.borrow_mut() = cm.and_then(|m| m.cast::<IContextMenu3>().ok()));
    CM2.with(|c| *c.borrow_mut() = cm.and_then(|m| m.cast::<IContextMenu2>().ok()));
}

/// The worker's hidden menu owner. Created once per worker thread.
pub fn host_window() -> Option<HWND> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        REGISTER.call_once(|| {
            let wc = WNDCLASSEXW { cbSize: size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(host_proc), hInstance: hinst, lpszClassName: w!("TsubameShellHost"), ..Default::default() };
            RegisterClassExW(&wc);
        });
        CreateWindowExW(WS_EX_TOOLWINDOW, w!("TsubameShellHost"), w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None).ok()
    }
}

fn note_shown() {
    if SHOWN_US.get().is_none()
        && let Some(t) = START.get()
    {
        SHOWN_US.set(Some(t.elapsed().as_micros() as u64));
    }
}

extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR => {
                if msg == WM_INITMENUPOPUP {
                    note_shown();
                }
                // clone out (AddRef) so no RefCell borrow is held while the
                // handler runs: it may re-enter this window procedure
                if let Some(cm3) = CM3.with(|c| c.borrow().clone()) {
                    let mut lr = LRESULT(0);
                    if cm3.HandleMenuMsg2(msg, wp, lp, Some(&mut lr)).is_ok() {
                        return if msg == WM_DRAWITEM || msg == WM_MEASUREITEM { LRESULT(1) } else { lr };
                    }
                } else if msg != WM_MENUCHAR
                    && let Some(cm2) = CM2.with(|c| c.borrow().clone())
                    && cm2.HandleMenuMsg(msg, wp, lp).is_ok()
                {
                    return if msg == WM_DRAWITEM || msg == WM_MEASUREITEM { LRESULT(1) } else { LRESULT(0) };
                }
            }
            WM_ENTERIDLE if wp.0 as u32 == MSGF_MENU => {
                note_shown();
                if let Some(ms) = AUTO_CLOSE.take() {
                    if ms == 0 {
                        let _ = EndMenu();
                    } else {
                        SetTimer(Some(hwnd), AUTO_CLOSE_TIMER, ms, None);
                    }
                }
            }
            WM_TIMER if wp.0 == AUTO_CLOSE_TIMER => {
                let _ = KillTimer(Some(hwnd), AUTO_CLOSE_TIMER);
                let _ = EndMenu();
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

pub(super) struct Pidls(pub(super) Vec<*mut ITEMIDLIST>);
impl Drop for Pidls {
    fn drop(&mut self) {
        for &p in &self.0 {
            unsafe { CoTaskMemFree(Some(p as *const _)) };
        }
    }
}

pub(super) fn parent_of(p: &str) -> &str {
    let p = p.trim_end_matches('\\');
    p.rfind('\\').map(|i| &p[..i]).unwrap_or("")
}

/// `ui` is the app window that gets the foreground back afterwards (the
/// hidden host keeps it otherwise, and the UI stops receiving keys).
#[allow(clippy::too_many_arguments)]
pub fn show(host: Option<HWND>, paths: &[String], x: i32, y: i32, extended: bool, auto_close: Option<u32>, owner: Option<isize>, ui: HWND, opts: &MenuOpts) -> MenuResult {
    let mut r = MenuResult::default();
    let Some(host) = host else {
        r.error = Some("no host window".into());
        return r;
    };
    if paths.is_empty() {
        r.error = Some("no paths".into());
        return r;
    }
    if !opts.background && paths.iter().any(|p| !parent_of(p).eq_ignore_ascii_case(parent_of(&paths[0]))) {
        r.error = Some("items must share one folder".into());
        return r;
    }
    let owner = owner.map(|h| HWND(h as *mut _)).unwrap_or(host);
    unsafe {
        r.foreign_owner = GetWindowThreadProcessId(owner, None) != windows::Win32::System::Threading::GetCurrentThreadId();
    }
    START.set(Some(Instant::now()));
    SHOWN_US.set(None);
    let t = Instant::now();

    // absolute PIDLs, then the parent folder + child ids
    let mut pidls = Pidls(Vec::with_capacity(paths.len()));
    for p in paths {
        let w = wide(p);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        if let Err(e) = unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) } {
            r.error = Some(format!("SHParseDisplayName {:?} for {p}", e.code()));
            return r;
        }
        pidls.0.push(pidl);
    }
    let cm = if opts.background { background_menu(pidls.0[0], host) } else { item_menu(&pidls, host) };
    let cm = match cm {
        Ok(c) => c,
        Err(e) => {
            r.error = Some(e);
            return r;
        }
    };
    r.bind_us = t.elapsed().as_micros() as u64;

    let Ok(hmenu) = (unsafe { CreatePopupMenu() }) else {
        r.error = Some("CreatePopupMenu".into());
        return r;
    };
    let t = Instant::now();
    let flags = CMF_NORMAL | CMF_EXPLORE | if extended { CMF_EXTENDEDVERBS } else { 0 };
    let hr = unsafe { cm.QueryContextMenu(hmenu, 0, ID_FIRST, ID_LAST, flags) };
    r.query_us = t.elapsed().as_micros() as u64;
    if hr.is_err() {
        r.error = Some(format!("QueryContextMenu {hr:?}"));
        unsafe {
            let _ = DestroyMenu(hmenu);
        }
        return r;
    }
    r.items = unsafe { GetMenuItemCount(Some(hmenu)) }.max(0) as u32;
    if !opts.extras.is_empty() {
        // app entries on top, then a separator before the shell's items
        unsafe {
            if r.items > 0 {
                let _ = InsertMenuW(hmenu, 0, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null());
            }
            for (i, label) in opts.extras.iter().enumerate().rev() {
                if label.is_empty() {
                    let _ = InsertMenuW(hmenu, 0, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null());
                } else {
                    let w = wide(label);
                    let _ = InsertMenuW(hmenu, 0, MF_BYPOSITION | MF_STRING, (ID_EXTRA + i as u32) as usize, PCWSTR(w.as_ptr()));
                }
            }
        }
    }

    CM3.with(|c| *c.borrow_mut() = cm.cast::<IContextMenu3>().ok());
    CM2.with(|c| *c.borrow_mut() = cm.cast::<IContextMenu2>().ok());
    AUTO_CLOSE.set(auto_close);

    let cmd = unsafe {
        // the owner must be foreground or a click elsewhere does not close
        // the menu (KB 135788); the app is the foreground process, so a
        // window of any of its threads may take it. It fails while any menu
        // is active, including the UI thread's F10/Alt menu mode.
        r.foreground = SetForegroundWindow(owner).as_bool();
        SetLastError(WIN32_ERROR(0));
        let cmd = TrackPopupMenuEx(hmenu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, x, y, owner, None).0 as u32;
        if cmd == 0 {
            let e = GetLastError();
            if e.0 != 0 {
                r.error = Some(format!("TrackPopupMenuEx: GetLastError {} ({})", e.0, windows::core::Error::from(e.to_hresult()).message()));
            }
        }
        // KB 135788: lets the menu loop end cleanly
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        cmd
    };
    AUTO_CLOSE.set(None);
    unsafe {
        let _ = KillTimer(Some(host), AUTO_CLOSE_TIMER);
    }
    r.popup_us = SHOWN_US.get();
    r.shown = r.popup_us.is_some();

    if cmd >= ID_EXTRA && ((cmd - ID_EXTRA) as usize) < opts.extras.len() {
        r.extra = Some((cmd - ID_EXTRA) as usize);
        r.chosen = Some(format!("extra:{}", opts.extras[(cmd - ID_EXTRA) as usize]));
    } else if cmd >= ID_FIRST {
        let off = (cmd - ID_FIRST) as usize;
        let mut buf = [0u16; 128];
        let verb = unsafe { cm.GetCommandString(off, GCS_VERBW, None, PSTR(buf.as_mut_ptr() as *mut u8), buf.len() as u32) }
            .ok()
            .map(|_| String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("#{off}"));
        let skip = opts.intercept.iter().any(|v| v.eq_ignore_ascii_case(&verb));
        r.chosen = Some(verb.clone());
        if skip {
            r.intercepted = Some(verb.to_ascii_lowercase());
        }
        let ici = CMINVOKECOMMANDINFOEX {
            cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE | if extended { CMIC_MASK_SHIFT_DOWN } else { 0 },
            hwnd: host,
            lpVerb: PCSTR(off as *const u8),
            lpVerbW: PCWSTR(off as *const u16),
            nShow: SW_SHOWNORMAL.0,
            ptInvoke: POINT { x, y },
            ..Default::default()
        };
        if !skip {
            let res = unsafe { cm.InvokeCommand(&ici as *const _ as *const CMINVOKECOMMANDINFO) };
            r.invoked = Some(match res {
                Ok(()) => "ok".into(),
                Err(e) => format!("{:?}", e.code()),
            });
        }
    }

    CM3.with(|c| *c.borrow_mut() = None);
    CM2.with(|c| *c.borrow_mut() = None);
    unsafe {
        // hand the foreground back unless the command moved it (a dialog,
        // another app): keys would otherwise go to the hidden host window
        if GetForegroundWindow() == host && !ui.is_invalid() {
            let _ = SetForegroundWindow(ui);
        }
        let _ = DestroyMenu(hmenu);
    }
    START.set(None);
    r
}

/// The context menu of items: parent folder + child ids.
pub(super) fn item_menu(pidls: &Pidls, host: HWND) -> Result<IContextMenu, String> {
    let folder: IShellFolder = unsafe { SHBindToParent(pidls.0[0], None) }.map_err(|e| format!("SHBindToParent {:?}", e.code()))?;
    let children: Vec<*const ITEMIDLIST> = pidls.0.iter().map(|&p| unsafe { ILFindLastID(p) as *const _ }).collect();
    unsafe { folder.GetUIObjectOf(host, &children, None) }.map_err(|e| format!("GetUIObjectOf {:?}", e.code()))
}

/// The background menu of folder `pidl` ("New", "Paste", "Properties", ...).
pub(super) fn background_menu(pidl: *mut ITEMIDLIST, host: HWND) -> Result<IContextMenu, String> {
    unsafe {
        let desktop = SHGetDesktopFolder().map_err(|e| format!("SHGetDesktopFolder {:?}", e.code()))?;
        let folder: IShellFolder = if (*pidl).mkid.cb == 0 { desktop } else { desktop.BindToObject(pidl, None).map_err(|e| format!("BindToObject {:?}", e.code()))? };
        folder.CreateViewObject(host).map_err(|e| format!("CreateViewObject {:?}", e.code()))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parent() {
        assert_eq!(super::parent_of(r"C:\a\b.txt"), r"C:\a");
        assert_eq!(super::parent_of(r"C:\a\sub\"), r"C:\a");
    }
}
