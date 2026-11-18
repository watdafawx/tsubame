//! The Win32 side: the real [`Host`] (process start, Explorer, files, clock) and the real
//! registry. Everything here is thin; the decisions are in [`crate::run`].
//!
//! Costs on the success path: `GetCommandLineW`, `current_exe`, one `exists`, `CreateProcessW`,
//! `WaitForInputIdle`. No file or registry access, no log write, no `shell32` (it is loaded on
//! demand, only after a registration was removed).

use crate::cmdline;
use crate::registry::Registry;
use crate::run::{self, DEPTH_ENV, Host, SpawnError, Started};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE};
use windows::Win32::System::Environment::GetCommandLineW;
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows::Win32::System::Threading::{CreateProcessW, GetExitCodeProcess, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW, WaitForInputIdle};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
use windows::core::{PCWSTR, PWSTR};

/// How long the app gets to show it is alive (message loop idle) before the shim calls it hung.
pub const GRACE_MS: u32 = 4000;
const STILL_ACTIVE: u32 = 259;
const WAIT_TIMEOUT: u32 = 258;
const LOG_CAP_BYTES: u64 = 32 * 1024;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The shim's whole job, for `main`. Returns the process exit code.
pub fn run_from_process() -> i32 {
    // SAFETY: GetCommandLineW returns a NUL-terminated string owned by the process.
    let raw = unsafe {
        let p = GetCommandLineW();
        String::from_utf16_lossy(p.as_wide())
    };
    let req = cmdline::request_from_command_line(&raw);
    let mut host = SysHost::new();
    run::handle(&mut host, req).exit_code()
}

pub struct SysHost {
    shim: PathBuf,
    data_dir: PathBuf,
    reg: RawRegistry,
}

impl Default for SysHost {
    fn default() -> Self {
        Self::new()
    }
}

impl SysHost {
    pub fn new() -> SysHost {
        let shim = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(crate::registry::SHIM_EXE));
        let data_dir = data_dir_for(shim.parent());
        SysHost { shim, data_dir, reg: RawRegistry }
    }
    fn dir(&self) -> PathBuf {
        self.shim.parent().map(Path::to_path_buf).unwrap_or_default()
    }
}

/// Where the log and the failure counter live: the same folder the app uses
/// (`tsubame-config` locate.rs), without depending on it.
fn data_dir_for(exe_dir: Option<&Path>) -> PathBuf {
    if let Some(d) = std::env::var_os("TSUBAME_DATA_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = exe_dir {
        if d.join("tsubame.portable").exists() || d.join("portable.txt").exists() {
            return d.join("data");
        }
    }
    match std::env::var_os("APPDATA") {
        Some(a) => PathBuf::from(a).join("Tsubame"),
        None => PathBuf::from("."),
    }
}

fn windows_dir() -> Option<String> {
    let mut buf = [0u16; 260];
    // SAFETY: plain out-buffer call.
    let n = unsafe { GetWindowsDirectoryW(Some(&mut buf)) } as usize;
    (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

/// Starts `app` with `command_line`; the handles are returned to the caller.
fn create_process(app: &str, command_line: &str) -> Result<PROCESS_INFORMATION, SpawnError> {
    let app_w = wide(app);
    let mut cmd = wide(command_line);
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: all buffers outlive the call; `cmd` is writable as CreateProcessW requires.
    let r = unsafe { CreateProcessW(PCWSTR(app_w.as_ptr()), Some(PWSTR(cmd.as_mut_ptr())), None, None, false, PROCESS_CREATION_FLAGS(0), None, PCWSTR::null(), &si, &mut pi) };
    match r {
        Ok(()) => Ok(pi),
        Err(e) => {
            let code = e.code().0 as u32;
            let not_found = code == (0x8007_0000 | ERROR_FILE_NOT_FOUND.0) || code == (0x8007_0000 | ERROR_PATH_NOT_FOUND.0);
            Err(SpawnError { not_found, message: format!("{} ({:#010x})", e.message().trim(), code) })
        }
    }
}

fn close(pi: &PROCESS_INFORMATION) {
    // SAFETY: both handles came from a successful CreateProcessW.
    unsafe {
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
}

fn exit_code(h: HANDLE) -> Option<u32> {
    let mut code = 0u32;
    // SAFETY: valid process handle, plain out parameter.
    unsafe { GetExitCodeProcess(h, &mut code) }.ok().map(|_| code)
}

impl Host for SysHost {
    fn depth(&self) -> u32 {
        std::env::var(DEPTH_ENV).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0)
    }
    fn shim_path(&self) -> PathBuf {
        self.shim.clone()
    }
    fn app_path(&self) -> PathBuf {
        self.dir().join("tsubame.exe")
    }
    fn exists(&self, p: &Path) -> bool {
        p.is_file()
    }
    fn sleep_ms(&mut self, ms: u32) {
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    }
    fn now_secs(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }

    fn start_app(&mut self, app: &Path, command_line: &str) -> Result<Started, SpawnError> {
        // The app gets a clean environment on purpose: it must not look like a fallback loop
        // when it later opens a folder through the verb itself.
        let pi = create_process(&app.display().to_string(), command_line)?;
        // The shell started us with foreground rights; pass them on so the window can come up
        // in front instead of blinking in the taskbar.
        // SAFETY: plain call with a process id.
        unsafe {
            let _ = AllowSetForegroundWindow(pi.dwProcessId);
        }
        // SAFETY: valid process handle. Returns when the app's message loop is idle, the
        // process ends, or the grace window is over.
        let idle = unsafe { WaitForInputIdle(pi.hProcess, GRACE_MS) };
        let out = match exit_code(pi.hProcess) {
            Some(c) if c != STILL_ACTIVE => {
                if c == 0 {
                    Started::ForwardedAndExited
                } else {
                    Started::Crashed(c)
                }
            }
            // idle == WAIT_FAILED: no message queue (nothing to wait for), treated as running
            _ if idle == WAIT_TIMEOUT => Started::Hung,
            _ => Started::Running,
        };
        close(&pi);
        Ok(out)
    }

    fn start_explorer(&mut self, command_line: &str) -> Result<(), String> {
        // Marker for the loop guard: only Explorer's fallback carries it.
        // SAFETY: the shim is single-threaded here.
        unsafe { std::env::set_var(DEPTH_ENV, (self.depth() + 1).to_string()) };
        let exe = self.explorer_path();
        let pi = create_process(&exe, command_line).map_err(|e| e.message)?;
        close(&pi);
        Ok(())
    }

    fn explorer_path(&self) -> String {
        // absolute, never through PATH or the current directory
        match windows_dir() {
            Some(w) => format!(r"{}\explorer.exe", w.trim_end_matches('\\')),
            None => r"C:\Windows\explorer.exe".to_string(),
        }
    }

    fn registry(&mut self) -> &mut dyn Registry {
        &mut self.reg
    }

    fn load_failures(&self) -> Vec<u64> {
        std::fs::read_to_string(self.data_dir.join("handler-fails.txt")).map(|s| s.split_whitespace().filter_map(|t| t.parse().ok()).collect()).unwrap_or_default()
    }
    fn save_failures(&mut self, times: &[u64]) {
        let p = self.data_dir.join("handler-fails.txt");
        if times.is_empty() {
            let _ = std::fs::remove_file(&p);
            return;
        }
        let _ = std::fs::create_dir_all(&self.data_dir);
        let text: Vec<String> = times.iter().map(u64::to_string).collect();
        let _ = std::fs::write(p, text.join("\n"));
    }

    fn log(&mut self, line: &str) {
        use std::io::Write;
        let p = self.data_dir.join("handler.log");
        if std::fs::metadata(&p).map(|m| m.len() > LOG_CAP_BYTES).unwrap_or(false) {
            let _ = std::fs::remove_file(&p);
        }
        let _ = std::fs::create_dir_all(&self.data_dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let _ = writeln!(f, "{} {}", self.now_secs(), line);
        }
    }
}

// ---- the real registry ------------------------------------------------------------------

mod win {
    use super::wide;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::*;
    use windows::core::{PCWSTR, PWSTR};

    fn full(key: &str) -> Vec<u16> {
        wide(&format!(r"Software\Classes\{key}"))
    }

    fn ok(e: WIN32_ERROR) -> Result<(), String> {
        if e == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry error {}", e.0)) }
    }

    fn open(key: &str, sam: REG_SAM_FLAGS) -> Option<HKEY> {
        let k = full(key);
        let mut h = HKEY::default();
        let e = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), None, sam, &mut h) };
        (e == ERROR_SUCCESS).then_some(h)
    }

    pub fn get(key: &str, name: &str) -> Option<String> {
        let h = open(key, KEY_READ)?;
        let n = wide(name);
        let mut ty = REG_VALUE_TYPE(0);
        let mut len = 0u32;
        let mut out = None;
        unsafe {
            if RegQueryValueExW(h, PCWSTR(n.as_ptr()), None, Some(&mut ty), None, Some(&mut len)) == ERROR_SUCCESS && (ty == REG_SZ || ty == REG_EXPAND_SZ) {
                let mut buf = vec![0u8; len as usize + 2];
                let mut len2 = buf.len() as u32;
                if RegQueryValueExW(h, PCWSTR(n.as_ptr()), None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len2)) == ERROR_SUCCESS {
                    let used = (len2 as usize).min(buf.len());
                    let w: Vec<u16> = buf[..used].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    out = Some(String::from_utf16_lossy(&w).trim_end_matches('\0').to_string());
                }
            }
            let _ = RegCloseKey(h);
        }
        out
    }

    pub fn set(key: &str, name: &str, value: &str) -> Result<(), String> {
        let k = full(key);
        let mut h = HKEY::default();
        unsafe {
            ok(RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut h, None))?;
            let n = wide(name);
            let v: Vec<u8> = wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
            let r = ok(RegSetValueExW(h, PCWSTR(n.as_ptr()), None, REG_SZ, Some(&v)));
            let _ = RegCloseKey(h);
            r
        }
    }

    pub fn delete_value(key: &str, name: &str) -> Result<(), String> {
        let Some(h) = open(key, KEY_WRITE) else { return Ok(()) };
        let n = wide(name);
        let e = unsafe { RegDeleteValueW(h, PCWSTR(n.as_ptr())) };
        unsafe {
            let _ = RegCloseKey(h);
        }
        // "not found" is the state we wanted
        if e == ERROR_SUCCESS || e == ERROR_FILE_NOT_FOUND { Ok(()) } else { ok(e) }
    }

    pub fn delete_tree(key: &str) -> Result<(), String> {
        let k = full(key);
        let e = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr())) };
        if e == ERROR_SUCCESS || e == ERROR_FILE_NOT_FOUND { Ok(()) } else { ok(e) }
    }

    pub fn subkeys(key: &str) -> Vec<String> {
        let Some(h) = open(key, KEY_READ) else { return Vec::new() };
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            let e = unsafe { RegEnumKeyExW(h, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
            if e != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]));
            i += 1;
        }
        unsafe {
            let _ = RegCloseKey(h);
        }
        out
    }

    pub fn key_exists(key: &str) -> bool {
        match open(key, KEY_READ) {
            Some(h) => {
                unsafe {
                    let _ = RegCloseKey(h);
                }
                true
            }
            None => false,
        }
    }

    /// `SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST | SHCNF_FLUSH, null, null)`, with
    /// `shell32` loaded on demand so the shim does not pay for it on every folder open.
    pub fn assoc_changed() {
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        use windows::core::{s, w};
        type Notify = unsafe extern "system" fn(i32, u32, *const core::ffi::c_void, *const core::ffi::c_void);
        const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
        const SHCNF_IDLIST_FLUSH: u32 = 0x1000;
        unsafe {
            let Ok(m) = LoadLibraryW(w!("shell32.dll")) else { return };
            if let Some(f) = GetProcAddress(m, s!("SHChangeNotify")) {
                let f: Notify = std::mem::transmute(f);
                f(SHCNE_ASSOCCHANGED, SHCNF_IDLIST_FLUSH, std::ptr::null(), std::ptr::null());
            }
        }
    }
}

/// `HKCU\Software\Classes` through the Win32 API. Reads always work; writes are refused with
/// `TSUBAME_NO_REGISTRY_WRITE` set and in this crate's own tests. Only the opt-in Settings
/// button and the shim's fail-safe may call writes on it.
pub struct RawRegistry;

impl RawRegistry {
    pub fn writes_allowed() -> Result<(), String> {
        if cfg!(test) {
            return Err("registry writes are disabled in tests".into());
        }
        if std::env::var_os("TSUBAME_NO_REGISTRY_WRITE").is_some() {
            return Err("registry writes are disabled (TSUBAME_NO_REGISTRY_WRITE)".into());
        }
        Ok(())
    }
}

impl Registry for RawRegistry {
    fn get(&self, key: &str, name: &str) -> Option<String> {
        win::get(key, name)
    }
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        win::set(key, name, value)
    }
    fn delete_value(&mut self, key: &str, name: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        win::delete_value(key, name)
    }
    fn delete_tree(&mut self, key: &str) -> Result<(), String> {
        Self::writes_allowed()?;
        win::delete_tree(key)
    }
    fn subkeys(&self, key: &str) -> Vec<String> {
        win::subkeys(key)
    }
    fn key_exists(&self, key: &str) -> bool {
        win::key_exists(key)
    }
    fn changed(&mut self) {
        if Self::writes_allowed().is_ok() {
            win::assoc_changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_registry_refuses_writes_in_tests() {
        let mut r = RawRegistry;
        assert!(r.set(r"Directory\shell\TsubameTestMustNotExist", "", "x").is_err());
        assert!(r.delete_tree(r"Directory\shell\TsubameTestMustNotExist").is_err());
        assert!(r.delete_value(r"Directory\shell\TsubameTestMustNotExist", "").is_err());
        // reads are fine and see nothing
        assert!(!r.key_exists(r"Directory\shell\TsubameTestMustNotExist"));
        assert_eq!(r.get(r"Directory\shell\TsubameTestMustNotExist", ""), None);
    }

    #[test]
    fn explorer_path_is_absolute_and_named_explorer() {
        let h = SysHost::new();
        let p = h.explorer_path();
        assert!(p.to_ascii_lowercase().ends_with(r"\explorer.exe"), "{p}");
        assert!(Path::new(&p).is_absolute(), "{p}");
    }

    #[test]
    fn data_dir_prefers_the_env_override_then_portable_then_appdata() {
        let d = crate::registry::testutil::Dir::new();
        assert!(data_dir_for(Some(d.path())).ends_with("Tsubame") || std::env::var_os("TSUBAME_DATA_DIR").is_some() || std::env::var_os("APPDATA").is_none());
        std::fs::write(d.path().join("portable.txt"), b"").unwrap();
        if std::env::var_os("TSUBAME_DATA_DIR").is_none() {
            assert_eq!(data_dir_for(Some(d.path())), d.path().join("data"));
        }
    }
}
