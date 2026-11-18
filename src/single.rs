//! Single instance: the first process owns a named mutex and a hidden window;
//! a second launch forwards its paths to it over `WM_COPYDATA` and exits
//! without creating a window.
//!
//! Why this shape: a named mutex alone cannot carry a path, and a named pipe
//! would need a reader thread. A hidden window on the UI thread costs one
//! `RegisterClassExW` + `CreateWindowExW` at startup (well under a millisecond)
//! and its messages are dispatched by the loop that is already running.
//!
//! Foreground: the second process is the one Windows would activate (the user
//! just double-clicked it), so it calls `AllowSetForegroundWindow(ASFW_ANY)`
//! before sending; the first process may then call `SetForegroundWindow`.
//!
//! Failure is never fatal: no window found, a hung receiver (`SendMessageTimeoutW`
//! gives up after 500 ms), a different build's payload (version mismatch) or no
//! mutex permission all end in a normal standalone start.

use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use crate::cli::Target;

/// Hidden window class of the owning instance.
const CLASS: PCWSTR = w!("TsubameInstance.v1");
/// `Local\` = this session, so a second user's Tsubame is a different instance.
const MUTEX_NAME: PCWSTR = w!("Local\\Tsubame.SingleInstance.v1");
/// WM_COPYDATA payload tag, checked by the receiver.
const DW_DATA: usize = 0x5453_4231; // "TSB1"
const MAGIC: [u8; 4] = *b"TSB1";
const PAYLOAD_VERSION: u32 = 1;
const FLAG_NEW_TAB: u32 = 1;
/// The first window to be brought to the front receives forwarded paths.
const MAX_PATHS: u32 = 64;
const MAX_STRING: usize = 32 * 1024;
const MAX_PAYLOAD: usize = 1024 * 1024;
/// How long a second launch waits for the owner's window to exist.
const FIND_TIMEOUT: Duration = Duration::from_millis(500);
/// A hung owner must not hold the second process longer than this.
const SEND_TIMEOUT_MS: u32 = 500;

static MAIN: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// Forwarded launches, drained by the app on [`WM_APP_OPEN`](crate::WM_APP_OPEN).
static PENDING: Mutex<Vec<Launch>> = Mutex::new(Vec::new());

/// What a second launch asked the running instance to open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// `--new-tab`: the paths open as new tabs even when there is one
    pub new_tab: bool,
    pub paths: Vec<Target>,
}

#[derive(Debug)]
pub enum Claim {
    /// This process owns the instance (mutex + hidden window).
    Owner(Instance),
    /// Another instance took the launch; exit now.
    Forwarded,
    /// No forwarding (new window, a dev run, or forwarding failed): start a
    /// normal window without owning the instance.
    Standalone,
}

/// Owns the mutex and the hidden window; dropping it releases both.
#[derive(Debug)]
pub struct Instance {
    mutex: HANDLE,
    hwnd: HWND,
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = CloseHandle(self.mutex);
        }
    }
}

/// Tries to become the instance, or forwards `launch` to the running one.
///
/// `new_window` skips forwarding entirely (the caller wants a second window).
pub fn claim(hinst: HINSTANCE, launch: &Launch, new_window: bool) -> Claim {
    unsafe {
        let mutex = match CreateMutexW(None, true, MUTEX_NAME) {
            Ok(h) => h,
            Err(_) => return Claim::Standalone,
        };
        let already = GetLastError() == ERROR_ALREADY_EXISTS;
        if already {
            let _ = CloseHandle(mutex);
            if !new_window && forward_to_owner(launch) {
                return Claim::Forwarded;
            }
            return Claim::Standalone;
        }
        match create_hidden(hinst) {
            Some(hwnd) => Claim::Owner(Instance { mutex, hwnd }),
            None => {
                // no hidden window: do not block the next launch behind our mutex
                let _ = CloseHandle(mutex);
                Claim::Standalone
            }
        }
    }
}

unsafe fn create_hidden(hinst: HINSTANCE) -> Option<HWND> {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(ipc_proc),
            hInstance: hinst,
            lpszClassName: CLASS,
            ..Default::default()
        };
        // a class name already registered by this process is fine
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!(""), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(hinst), None).ok()?;
        Some(hwnd)
    }
}

/// Waits (briefly) for the owner's hidden window and sends `launch` to it.
fn forward_to_owner(launch: &Launch) -> bool {
    let hwnd = match wait_for_owner() {
        Some(h) => h,
        None => return false,
    };
    let bytes = encode(launch);
    let cds = COPYDATASTRUCT { dwData: DW_DATA, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut core::ffi::c_void };
    let mut result: usize = 0;
    unsafe {
        // we are the foreground process: let the owner take the foreground
        let _ = AllowSetForegroundWindow(u32::MAX); // ASFW_ANY
        let r = SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG | SMTO_BLOCK, SEND_TIMEOUT_MS, Some(&mut result));
        r.0 != 0 && result == 1
    }
}

fn wait_for_owner() -> Option<HWND> {
    let t0 = Instant::now();
    loop {
        if let Ok(hwnd) = unsafe { FindWindowW(CLASS, PCWSTR::null()) } {
            return Some(hwnd);
        }
        if t0.elapsed() >= FIND_TIMEOUT {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The app calls this once its main window exists, then [`take_pending`].
pub fn set_main(hwnd: HWND) {
    MAIN.store(hwnd.0 as isize, std::sync::atomic::Ordering::Release);
}

/// Forwarded launches waiting for the app. Called on the UI thread.
pub fn take_pending() -> Vec<Launch> {
    PENDING.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

unsafe extern "system" fn ipc_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COPYDATA => {
                let cds = *(lp.0 as *const COPYDATASTRUCT);
                if cds.dwData != DW_DATA || cds.lpData.is_null() || cds.cbData == 0 || cds.cbData as usize > MAX_PAYLOAD {
                    return LRESULT(0);
                }
                let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
                let Some(launch) = decode(bytes) else { return LRESULT(0) };
                let main = HWND(MAIN.load(std::sync::atomic::Ordering::Acquire) as *mut core::ffi::c_void);
                if main.0.is_null() {
                    return LRESULT(0);
                }
                if let Ok(mut q) = PENDING.lock() {
                    // a burst from a script must not grow without bound
                    if q.len() >= 32 {
                        q.remove(0);
                    }
                    q.push(launch);
                }
                let _ = ShowWindow(main, SW_RESTORE);
                let _ = SetForegroundWindow(main);
                let _ = PostMessageW(Some(main), crate::WM_APP_OPEN, WPARAM(0), LPARAM(0));
                LRESULT(1)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// The `WM_COPYDATA` payload. Pure and versioned: the receiver ignores
/// anything it does not understand. Layout (little endian):
/// `magic[4] version u32 flags u32 count u32`, then per path
/// `folder_len u32 folder_utf8 [1 select_len u32 select_utf8]` where the
/// leading 1 is present on every path.
pub fn encode(l: &Launch) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + l.paths.iter().map(|p| p.folder.len() + 16).sum::<usize>());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&PAYLOAD_VERSION.to_le_bytes());
    out.extend_from_slice(&(if l.new_tab { FLAG_NEW_TAB } else { 0 }).to_le_bytes());
    let count = l.paths.len().min(MAX_PATHS as usize) as u32;
    out.extend_from_slice(&count.to_le_bytes());
    for p in l.paths.iter().take(count as usize) {
        push_str(&mut out, &p.folder);
        match &p.select {
            Some(s) => {
                out.push(1);
                push_str(&mut out, s);
            }
            None => {
                out.push(0);
                out.extend_from_slice(&0u32.to_le_bytes());
            }
        }
    }
    out
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let mut n = b.len().min(MAX_STRING);
    // never cut a UTF-8 sequence in half: the receiver would refuse the payload
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    out.extend_from_slice(&(n as u32).to_le_bytes());
    out.extend_from_slice(&b[..n]);
}

/// `None` for anything that is not a payload this build wrote.
pub fn decode(b: &[u8]) -> Option<Launch> {
    if b.len() < 16 || b.len() > MAX_PAYLOAD || b[..4] != MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(b[4..8].try_into().ok()?);
    if version != PAYLOAD_VERSION {
        return None;
    }
    let flags = u32::from_le_bytes(b[8..12].try_into().ok()?);
    let count = u32::from_le_bytes(b[12..16].try_into().ok()?);
    if count > MAX_PATHS {
        return None;
    }
    let mut at = 16usize;
    let mut paths = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let folder = read_str(b, &mut at)?;
        let has_select = *b.get(at)?;
        at += 1;
        let select = match has_select {
            1 => Some(read_str(b, &mut at)?),
            _ => {
                at += 4; // the empty length, so old/new payloads stay aligned
                None
            }
        };
        paths.push(Target { folder, select });
    }
    if at != b.len() {
        return None;
    }
    Some(Launch { new_tab: flags & FLAG_NEW_TAB != 0, paths })
}

fn read_str(b: &[u8], at: &mut usize) -> Option<String> {
    let len = u32::from_le_bytes(b.get(*at..*at + 4)?.try_into().ok()?) as usize;
    *at += 4;
    if len > MAX_STRING || *at + len > b.len() {
        return None;
    }
    let s = std::str::from_utf8(&b[*at..*at + len]).ok()?.to_string();
    *at += len;
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(folder: &str, select: Option<&str>) -> Target {
        Target { folder: folder.to_string(), select: select.map(str::to_string) }
    }

    #[test]
    fn payload_round_trip() {
        let l = Launch { new_tab: true, paths: vec![target("C:\\Users\\me", None), target("D:\\work\\a file.txt", Some("a file.txt")), target("\\\\srv\\share", None)] };
        let bytes = encode(&l);
        assert_eq!(decode(&bytes), Some(l));
    }

    #[test]
    fn an_empty_launch_round_trips() {
        let l = Launch::default();
        let bytes = encode(&l);
        assert_eq!(bytes.len(), 16);
        assert_eq!(decode(&bytes), Some(l));
    }

    #[test]
    fn unicode_paths_round_trip() {
        let l = Launch { new_tab: false, paths: vec![target("C:\\Ünïcode\\ファイル", Some("ファイル.txt"))] };
        assert_eq!(decode(&encode(&l)), Some(l));
    }

    #[test]
    fn junk_is_refused() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"TSB1"), None);
        let mut b = encode(&Launch::default());
        b[4] = 9; // version
        assert_eq!(decode(&b), None);
        // truncated
        let b = encode(&Launch { new_tab: false, paths: vec![target("C:\\a", None)] });
        for cut in 16..b.len() {
            assert_eq!(decode(&b[..cut]), None, "a truncated payload at {cut} must be refused");
        }
        // trailing junk
        let mut b = encode(&Launch::default());
        b.push(0);
        assert_eq!(decode(&b), None);
        // a huge count
        let mut b = encode(&Launch::default());
        b[12..16].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(decode(&b), None);
    }

    #[test]
    fn paths_are_capped_and_long_strings_are_not_sent() {
        let many: Vec<Target> = (0..200).map(|i| target(&format!("C:\\{i}"), None)).collect();
        let bytes = encode(&Launch { new_tab: false, paths: many });
        let back = decode(&bytes).unwrap();
        assert_eq!(back.paths.len(), MAX_PATHS as usize);
        let long = "x".repeat(MAX_STRING * 2);
        let back = decode(&encode(&Launch { new_tab: false, paths: vec![target(&long, None)] })).unwrap();
        assert_eq!(back.paths[0].folder.len(), MAX_STRING, "clamped, still valid UTF-8");
    }
}
