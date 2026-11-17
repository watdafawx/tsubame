//! Method A: `FindFirstFileExW` with `FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`.
//!
//! One Win32 call per entry after the first. No file id (the API has none), no
//! reparse tag except `dwReserved0`, and a fixed internal fetch buffer that the
//! caller cannot size.

use core::ffi::c_void;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
    FindNextFileW, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

use crate::clock::Clock;
use crate::store::{ATTR_REPARSE_POINT, EntryStore};
use crate::util::search_pattern_wide;
use crate::winerr::win32_code;

pub const ERROR_FILE_NOT_FOUND: u32 = 2;
pub const ERROR_NO_MORE_FILES: u32 = 18;

pub fn enumerate(dir: &str, store: &mut EntryStore, clock: &Clock, start: i64) -> Result<(), String> {
    let pattern = search_pattern_wide(dir);
    let mut data = WIN32_FIND_DATAW::default();
    let handle: HANDLE = unsafe {
        match FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut WIN32_FIND_DATAW as *mut c_void,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        ) {
            Ok(h) => h,
            Err(e) => {
                let code = win32_code(&e);
                if code == ERROR_FILE_NOT_FOUND || code == ERROR_NO_MORE_FILES {
                    // Empty folder: Win32 reports "no files" instead of a handle.
                    return Ok(());
                }
                return Err(format!(
                    "FindFirstFileExW failed for {dir}: {} (win32 {code})",
                    e.message()
                ));
            }
        }
    };

    let result = (|| -> Result<(), String> {
        loop {
            push_find_entry(store, &data);
            store.note_progress(clock, start);
            if let Err(e) = unsafe { FindNextFileW(handle, &mut data) } {
                let code = win32_code(&e);
                if code == ERROR_NO_MORE_FILES {
                    return Ok(());
                }
                return Err(format!(
                    "FindNextFileW failed in {dir}: {} (win32 {code})",
                    e.message()
                ));
            }
        }
    })();

    unsafe {
        let _ = FindClose(handle);
    }
    result
}

#[inline]
fn push_find_entry(store: &mut EntryStore, data: &WIN32_FIND_DATAW) {
    // cFileName is NUL padded to 260 units.
    let len = data
        .cFileName
        .iter()
        .position(|&u| u == 0)
        .unwrap_or(data.cFileName.len());
    let name = &data.cFileName[..len];
    let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
    let mtime = (((data.ftLastWriteTime.dwHighDateTime as u64) << 32)
        | data.ftLastWriteTime.dwLowDateTime as u64) as i64;
    let tag = if data.dwFileAttributes & ATTR_REPARSE_POINT != 0 {
        data.dwReserved0
    } else {
        0
    };
    store.push(name, data.dwFileAttributes, size, mtime, tag, [0u8; 16]);
}
