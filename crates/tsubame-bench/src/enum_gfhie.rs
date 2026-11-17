//! Method D: `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`.
//!
//! Same `FILE_ID_EXTD_DIR_INFO` layout as the NT method, but the enumeration
//! position lives in the file object, so a fresh handle per run starts at the
//! beginning (no restart flag needed). Returns file id, reparse tag, size,
//! mtime and attributes.

use core::ffi::c_void;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    FILE_INFO_BY_HANDLE_CLASS, FileIdExtdDirectoryInfo, GetFileInformationByHandleEx,
};

use crate::clock::Clock;
use crate::dirhandle::open_dir;
use crate::ntinfo::{Layout, parse_buffer};
use crate::store::EntryStore;
use crate::winerr::win32_code;

pub const ERROR_NO_MORE_FILES: u32 = 18;
pub const ERROR_MORE_DATA: u32 = 234;

pub fn enumerate(
    dir: &str,
    buf_bytes: usize,
    store: &mut EntryStore,
    clock: &Clock,
    start: i64,
) -> Result<(), String> {
    let handle = open_dir(dir)?;

    let words = buf_bytes.div_ceil(8);
    let mut buffer = vec![0u64; words];
    let buf_len = (words * 8) as u32;
    let buf_ptr = buffer.as_mut_ptr() as *mut c_void;

    let result = (|| -> Result<(), String> {
        loop {
            let res = unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    FILE_INFO_BY_HANDLE_CLASS(FileIdExtdDirectoryInfo.0),
                    buf_ptr,
                    buf_len,
                )
            };
            match res {
                Ok(()) => {}
                Err(e) => {
                    let code = win32_code(&e);
                    if code == ERROR_NO_MORE_FILES {
                        return Ok(());
                    }
                    if code != ERROR_MORE_DATA {
                        return Err(format!(
                            "GetFileInformationByHandleEx failed in {dir}: {} (win32 {code})",
                            e.message()
                        ));
                    }
                    // Partial buffer: the entries that fit are still valid.
                }
            }
            let buf: &[u8] = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, buf_len as usize) };
            let parsed = parse_buffer(buf, Layout::Extd, store)?;
            store.note_progress(clock, start);
            if parsed == 0 {
                return Ok(());
            }
        }
    })();

    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}
