//! `FindFirstFileExW` fallback: `FindExInfoBasic` (no 8.3 names) plus
//! `FIND_FIRST_EX_LARGE_FETCH` (the kernel fetches ahead in bigger chunks).
//!
//! Used when a filesystem rejects `FileIdExtdDirectoryInformation` (some network
//! shares) and by `Method::Find` in tests. It has no file id and no allocation
//! size, so those `RawEntry` fields stay 0; `dwReserved0` is the reparse tag.

use core::ffi::c_void;
use core::ptr;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::*;
use windows::core::PCWSTR;

use super::{Lister, Sink, Win32Error, search_pattern_wide};

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_NO_MORE_FILES: u32 = 18;

/// An owned find handle.
struct FindHandle(HANDLE);

impl Drop for FindHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = FindClose(self.0);
        }
    }
}

/// List `dir` into `lister`.
pub(crate) fn list<S: Sink>(
    lister: &mut Lister<'_, S>,
    dir: &std::path::Path,
) -> Result<(), Win32Error> {
    // Classify the path first: `FindFirstFileExW` reports a missing directory and
    // an empty one with nearly the same error, and cannot tell a file from a
    // directory. The NT opener gives typed errors and holds the handle open for
    // the duration of the listing.
    let _guard = super::nt::open_dir(dir)?;

    let pattern = search_pattern_wide(dir)?;
    let mut data = WIN32_FIND_DATAW::default();
    let found = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            ptr::addr_of_mut!(data).cast::<c_void>(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let handle = match found {
        Ok(handle) => FindHandle(handle),
        Err(e) => {
            let code = win32_code(&e);
            if code == ERROR_NO_MORE_FILES || code == ERROR_FILE_NOT_FOUND {
                return Ok(()); // empty directory: Win32 reports "no files"
            }
            return Err(Win32Error::win32(code));
        }
    };

    loop {
        if lister.cancelled() {
            return Ok(());
        }
        if !push_find_entry(lister, &data) {
            return Ok(());
        }
        if let Err(e) = unsafe { FindNextFileW(handle.0, &mut data) } {
            let code = win32_code(&e);
            if code == ERROR_NO_MORE_FILES {
                return Ok(());
            }
            return Err(Win32Error::win32(code));
        }
    }
}

#[inline]
fn win32_code(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

#[inline]
fn push_find_entry<S: Sink>(lister: &mut Lister<'_, S>, data: &WIN32_FIND_DATAW) -> bool {
    // cFileName is NUL padded to 260 code units.
    let len = data
        .cFileName
        .iter()
        .position(|&u| u == 0)
        .unwrap_or(data.cFileName.len());
    let name = &data.cFileName[..len];
    let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
    let mtime =
        ((data.ftLastWriteTime.dwHighDateTime as u64) << 32) | data.ftLastWriteTime.dwLowDateTime as u64;
    let tag = if data.dwFileAttributes & super::ATTR_REPARSE_POINT != 0 {
        data.dwReserved0
    } else {
        0
    };
    lister.push(
        name,
        super::EntryData {
            attrs: data.dwFileAttributes,
            size,
            // FindFirstFileExW reports no allocation size and no file id.
            alloc_size: 0,
            mtime,
            file_id: 0,
            reparse_tag: tag,
        },
    )
}
