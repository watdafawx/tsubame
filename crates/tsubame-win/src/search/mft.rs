//! Optional NTFS MFT / USN capability probe and elevated-helper architecture notes.
//!
//! ## Design (per `docs/PLAN.md` §8 decision #2)
//!
//! Reading an NTFS Master File Table directly (`\\.\C:` + `FSCTL_ENUM_USN_DATA` /
//! `FSCTL_QUERY_USN_JOURNAL`) requires Administrator privileges
//! (`SeManageVolumePrivilege`). Tsubame's main process runs unprivileged:
//! - By default, global search uses Voidtools Everything IPC (when running) or
//!   the parallel directory walk fallback — neither requires admin rights.
//! - Direct MFT indexing is **off by default** and is designed as an optional
//!   elevated helper process (`tsubame-mft-helper.exe`) launched only when the
//!   user explicitly opts in:
//!   1. Main UI checks [`mft_available`] (or whether the opt-in helper service
//!      is enabled).
//!   2. The helper opens the NTFS volume handle, builds/maintains an in-memory
//!      FRN-indexed table via `FSCTL_ENUM_USN_DATA` + `FSCTL_READ_USN_JOURNAL`,
//!      and serves queries over a local named pipe restricted by DACL to the
//!      current user's SID.
//!   3. When disabled or unavailable, Tsubame cleanly uses Everything IPC or
//!      parallel walk with zero extra crates or elevation prompts.

use core::ffi::c_void;
use core::mem::size_of;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::GetVolumeInformationW;
use windows::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

use crate::wide;

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Returns `true` if the current process token is elevated (Administrator).
pub fn is_process_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let _guard = OwnedHandle(token);

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// Returns `true` if `drive` (e.g. `'C'`) is a mounted, accessible NTFS volume.
pub fn is_ntfs_volume(drive: char) -> bool {
    if !drive.is_ascii_alphabetic() {
        return false;
    }
    let letter = drive.to_ascii_uppercase();
    let root = wide(&format!("{letter}:\\"));
    let mut fs_name = [0u16; 64];

    let ok = unsafe {
        let mut old = THREAD_ERROR_MODE(0);
        let _ = SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old));
        let res = GetVolumeInformationW(
            PCWSTR(root.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut fs_name),
        );
        let _ = SetThreadErrorMode(old, None);
        res.is_ok()
    };
    if !ok {
        return false;
    }

    let len = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    String::from_utf16_lossy(&fs_name[..len]).eq_ignore_ascii_case("NTFS")
}

/// Returns `true` when direct MFT access on `drive` is possible in this process
/// right now (i.e. the current process is elevated and `drive` is an NTFS volume).
///
/// Does not trigger UAC or require admin rights; returns `false` when running
/// as a standard user.
pub fn mft_available(drive: char) -> bool {
    is_process_elevated() && is_ntfs_volume(drive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_handles_invalid_drive_and_system_drive() {
        assert!(!is_ntfs_volume('1'));
        assert!(!mft_available('1'));
        // Calling on C: must never panic regardless of elevation state.
        let _ = is_ntfs_volume('C');
        let _ = mft_available('C');
    }
}
