//! Opening a directory handle for the NT-style enumeration APIs.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE,
};
use windows::core::PCWSTR;

use crate::util::extended_wide;
use crate::winerr::win32_code;

pub const FILE_LIST_DIRECTORY: u32 = 0x0001;
pub const FILE_READ_ATTRIBUTES: u32 = 0x0080;
pub const SYNCHRONIZE: u32 = 0x0010_0000;
pub const FILE_SHARE_ALL: u32 = 0x7;
pub const OPEN_EXISTING: u32 = 3;
pub const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// Open `dir` with list access. `FILE_FLAG_BACKUP_SEMANTICS` is what lets a
/// directory be opened at all; `FILE_FLAG_OPEN_REPARSE_POINT` is deliberately
/// not set, so a junction is followed to the real directory.
pub fn open_dir(dir: &str) -> Result<HANDLE, String> {
    let wide = extended_wide(dir);
    unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_MODE(FILE_SHARE_ALL),
            None,
            FILE_CREATION_DISPOSITION(OPEN_EXISTING),
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS),
            None,
        )
        .map_err(|e| format!("CreateFileW({dir}): {} (win32 {})", e.message(), win32_code(&e)))
    }
}
