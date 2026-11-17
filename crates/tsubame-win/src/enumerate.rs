//! Minimal directory lister for the S4 demo: `FindFirstFileExW` with
//! `FindExInfoBasic` (no 8.3 names) and `FIND_FIRST_EX_LARGE_FETCH`. Runs on
//! a worker thread, never on the UI thread. The real enumerator is a later task.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::*;
use windows::core::PCWSTR;

/// One entry, borrowed from the find buffer for the duration of the callback.
pub struct Entry<'a> {
    /// UTF-16 file name, no NUL
    pub name: &'a [u16],
    pub attrs: u32,
    pub size: u64,
    /// last write time, FILETIME (100 ns since 1601)
    pub mtime: u64,
}

/// Calls `f` for every entry of `dir` except `.` and `..`.
pub fn list_dir(dir: &str, mut f: impl FnMut(Entry)) -> windows::core::Result<()> {
    let pattern = crate::wide(&format!("{}\\*", dir.trim_end_matches(['\\', '/'])));
    let mut data = WIN32_FIND_DATAW::default();
    let h: HANDLE = unsafe { FindFirstFileExW(PCWSTR(pattern.as_ptr()), FindExInfoBasic, &mut data as *mut _ as *mut _, FindExSearchNameMatch, None, FIND_FIRST_EX_LARGE_FETCH)? };
    loop {
        let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
        let name = &data.cFileName[..len];
        let dot = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
        if !dot {
            f(Entry {
                name,
                attrs: data.dwFileAttributes,
                size: (data.nFileSizeHigh as u64) << 32 | data.nFileSizeLow as u64,
                mtime: (data.ftLastWriteTime.dwHighDateTime as u64) << 32 | data.ftLastWriteTime.dwLowDateTime as u64,
            });
        }
        if unsafe { FindNextFileW(h, &mut data) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(h);
    }
    Ok(())
}
