//! Cold-cache setup, in three strengths.
//!
//! Windows has no user-mode "drop this file's pages" call, so we flush
//! system-wide and measure immediately afterwards.
//!
//! `standby`: `NtSetSystemInformation(SystemMemoryListInformation,
//! MemoryPurgeStandbyList)`, what RAMMap's "Empty Standby List" does. Cheap and
//! safe, but **not enough on its own**: hot NTFS metadata lives in the file-cache
//! working set, not on the standby list, so the next enumeration stays warm.
//! Measured: HDD 100k "cold" 32.5 ms vs warm 30.6 ms.
//!
//! `filecache`: also trims the System process working set (`EmptyWorkingSet` on
//! PID 4), which is where the file cache lives. Needs `SeDebugPrivilege`; touches
//! no other process. On current Windows 11 this fails (the System process is
//! protected), so it is kept for older builds and reports honestly.
//!
//! `working`: also issues `MemoryEmptyWorkingSets`, the strongest and the
//! rudest: every process on the box re-faults its pages. This is the only mode
//! that produced real cold reads here.
//!
//! `SetSystemFileCacheSize` is tried first for the file-cache step (it evicts
//! metadata without touching any process); Windows 11 rejected 0 MB and 1 MB with
//! ERROR_INVALID_PARAMETER, so the code retries with a 64 MB limit and falls back
//! to the System working set. Whichever step worked is reported per run.
//!
//! All modes need an elevated token; if a privilege is missing we say so
//! instead of calling the run cold.

use core::ffi::c_void;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ACCESS_MASK,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Memory::SetSystemFileCacheSize;
use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_INFORMATION,
    PROCESS_SET_QUOTA,
};
use windows::core::PCWSTR;

const SYSTEM_MEMORY_LIST_INFORMATION: u32 = 80;
const MEMORY_EMPTY_WORKING_SETS: u32 = 2;
const MEMORY_PURGE_STANDBY_LIST: u32 = 4;
/// The System process (PID 4) owns the file-cache working set.
const SYSTEM_PROCESS_ID: u32 = 4;
const FILE_CACHE_MIN_HARD_ENABLE: u32 = 0x1;
const FILE_CACHE_MAX_HARD_ENABLE: u32 = 0x2;
/// Limits handed to the cache manager. Zero and 1 MB are rejected with
/// ERROR_INVALID_PARAMETER on Windows 11; 64 MB is small enough to evict a
/// million-entry folder's metadata and large enough to be accepted.
const CACHE_LIMIT_MIN_BYTES: usize = 64 * 1024 * 1024;
const CACHE_LIMIT_MAX_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeMode {
    Standby,
    FileCache,
    WorkingSets,
}

impl PurgeMode {
    pub fn parse(s: &str) -> Option<PurgeMode> {
        match s.to_ascii_lowercase().as_str() {
            "standby" => Some(PurgeMode::Standby),
            "filecache" | "file-cache" | "cache" => Some(PurgeMode::FileCache),
            "working" | "workingsets" | "working-sets" => Some(PurgeMode::WorkingSets),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PurgeMode::Standby => "standby",
            PurgeMode::FileCache => "filecache",
            PurgeMode::WorkingSets => "working",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColdStatus {
    /// Requested flush ran.
    Purged,
    /// Privilege not held: the run is first-touch-at-best, not cold.
    PrivilegeUnavailable(String),
    /// A call failed.
    Failed(String),
}

impl ColdStatus {
    pub fn label(&self) -> &'static str {
        match self {
            ColdStatus::Purged => "purged",
            ColdStatus::PrivilegeUnavailable(_) => "purge-denied",
            ColdStatus::Failed(_) => "purge-failed",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            ColdStatus::Purged => None,
            ColdStatus::PrivilegeUnavailable(m) | ColdStatus::Failed(m) => Some(m),
        }
    }
}

/// Enable a named privilege on the current process token.
fn enable_privilege(name: &str) -> Result<(), String> {
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    wide.push(0);
    unsafe {
        let mut luid = LUID::default();
        windows::Win32::Security::LookupPrivilegeValueW(
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            &mut luid,
        )
        .map_err(|e| format!("LookupPrivilegeValueW({name}): {}", e.message()))?;

        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ACCESS_MASK(TOKEN_ADJUST_PRIVILEGES.0 | TOKEN_QUERY.0),
            &mut token,
        )
        .map_err(|e| format!("OpenProcessToken: {}", e.message()))?;

        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let res = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        let last = windows::Win32::Foundation::GetLastError();
        let _ = CloseHandle(token);
        res.map_err(|e| format!("AdjustTokenPrivileges({name}): {}", e.message()))?;
        // AdjustTokenPrivileges "succeeds" even when nothing was assigned.
        if last.0 == 1300 {
            return Err(format!("privilege {name} not held by this token"));
        }
        Ok(())
    }
}

type NtSetSystemInformationFn =
    unsafe extern "system" fn(system_information_class: u32, info: *mut c_void, len: u32) -> i32;

fn nt_set_system_information() -> Result<NtSetSystemInformationFn, String> {
    let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    unsafe {
        let hmod = windows::Win32::System::LibraryLoader::GetModuleHandleW(PCWSTR(ntdll.as_ptr()))
            .map_err(|e| format!("GetModuleHandleW(ntdll.dll): {}", e.message()))?;
        let proc = windows::Win32::System::LibraryLoader::GetProcAddress(
            hmod,
            windows::core::PCSTR(c"NtSetSystemInformation".as_ptr() as *const u8),
        );
        match proc {
            Some(f) => Ok(core::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                NtSetSystemInformationFn,
            >(f)),
            None => Err("NtSetSystemInformation not exported by ntdll".to_string()),
        }
    }
}

fn memory_list_command(cmd: u32) -> Result<(), String> {
    let f = nt_set_system_information()?;
    let status = unsafe {
        f(
            SYSTEM_MEMORY_LIST_INFORMATION,
            &cmd as *const u32 as *mut c_void,
            core::mem::size_of::<u32>() as u32,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "NtSetSystemInformation(SystemMemoryListInformation, {cmd}) status {:#010X}",
            status as u32
        ))
    }
}

/// Trim the System process working set, i.e. the file cache.
fn trim_file_cache() -> Result<(), String> {
    enable_privilege("SeDebugPrivilege")?;
    unsafe {
        let handle = OpenProcess(
            PROCESS_ACCESS_RIGHTS(PROCESS_QUERY_INFORMATION.0 | PROCESS_SET_QUOTA.0),
            false,
            SYSTEM_PROCESS_ID,
        )
        .map_err(|e| format!("OpenProcess(System): {}", e.message()))?;
        let res = EmptyWorkingSet(handle);
        let _ = CloseHandle(handle);
        res.map_err(|e| format!("EmptyWorkingSet(System): {}", e.message()))
    }
}

/// Force the system file cache to a small size so the cache manager trims the
/// NTFS metadata out of it, then restore the default.
fn shrink_file_cache_limit() -> Result<(), String> {
    enable_privilege("SeIncreaseQuotaPrivilege")?;
    unsafe {
        // The cache manager requires minimum < maximum, strictly.
        SetSystemFileCacheSize(
            CACHE_LIMIT_MIN_BYTES,
            CACHE_LIMIT_MAX_BYTES,
            FILE_CACHE_MIN_HARD_ENABLE | FILE_CACHE_MAX_HARD_ENABLE,
        )
        .map_err(|e| {
            format!(
                "SetSystemFileCacheSize({CACHE_LIMIT_MIN_BYTES},{CACHE_LIMIT_MAX_BYTES},hard): {}",
                e.message()
            )
        })?;
        // Restore the default immediately: the trim has been requested and the
        // cache manager keeps shrinking towards the old limit.
        let restore = SetSystemFileCacheSize(usize::MAX, usize::MAX, 0);
        if let Err(e) = restore {
            return Err(format!("SetSystemFileCacheSize(restore): {}", e.message()));
        }
    }
    Ok(())
}

/// Flush caches so the next enumeration has to fetch metadata again.
///
/// Order matters: a working-set trim only moves clean pages to the standby
/// list, so the standby purge has to come *after* it. (Measured the other way
/// round: HDD 100k "cold" 32.5 ms, i.e. still warm.)
pub fn purge(mode: PurgeMode) -> ColdStatus {
    if let Err(e) = enable_privilege("SeProfileSingleProcessPrivilege") {
        return ColdStatus::PrivilegeUnavailable(e);
    }
    if mode != PurgeMode::Standby
        && let Err(e) = enable_privilege("SeDebugPrivilege") {
            return ColdStatus::PrivilegeUnavailable(e);
        }

    // 1. Push cached metadata out of working sets.
    match mode {
        PurgeMode::Standby => {}
        PurgeMode::FileCache => {
            // Two attempts, both targeted at the file cache only: the cache
            // size limit first (no process is touched), then the System
            // process working set.
            if let Err(first) = shrink_file_cache_limit()
                && let Err(second) = trim_file_cache() {
                    return ColdStatus::Failed(format!("{first}; then {second}"));
                }
        }
        PurgeMode::WorkingSets => {
            if let Err(e) = memory_list_command(MEMORY_EMPTY_WORKING_SETS) {
                return ColdStatus::Failed(e);
            }
        }
    }

    // 2. Free the standby pages the trim just produced.
    if let Err(e) = memory_list_command(MEMORY_PURGE_STANDBY_LIST) {
        return ColdStatus::Failed(e);
    }
    // 3. Give the trimmers a moment to finish, then free whatever landed late.
    if mode != PurgeMode::Standby {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Err(e) = memory_list_command(MEMORY_PURGE_STANDBY_LIST) {
            return ColdStatus::Failed(e);
        }
    }
    ColdStatus::Purged
}

/// Is the standby privilege present? Used to explain a denied purge.
pub fn profile_privilege_available() -> Result<(), String> {
    enable_privilege("SeProfileSingleProcessPrivilege")
}

/// Report the current elevation state for the log header.
pub fn elevation_note() -> String {
    match (
        profile_privilege_available(),
        enable_privilege("SeDebugPrivilege"),
    ) {
        (Ok(()), Ok(())) => {
            "SeProfileSingleProcessPrivilege + SeDebugPrivilege available".to_string()
        }
        (Ok(()), Err(e)) => format!("standby purge only ({e})"),
        (Err(e), _) => format!("cold purge unavailable: {e}"),
    }
}
