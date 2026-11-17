//! `NtQueryDirectoryFile` with `FileIdExtdDirectoryInformation` (class 60).
//!
//! Ported from the S2 bench (`crates/tsubame-bench/src/enum_nt.rs` +
//! `ntinfo.rs` + `dirhandle.rs`), which measured this as the fastest method and
//! the only one that returns the 128-bit file id and the reparse tag in the same
//! syscall. Differences from the bench:
//! - entries stream into batches instead of one big store,
//! - the buffer is a 64 KB `Vec<u64>` (8-byte aligned, as the kernel assumes),
//! - `.` and `..` are dropped by the sink,
//! - `STATUS_NO_MORE_FILES` at the very first call is an empty directory,
//! - `STATUS_INVALID_INFO_CLASS` and friends become [`Failure::Unsupported`] so
//!   the caller can retry with `FindFirstFileExW`.

use core::ffi::c_void;
use core::ptr;
use core::slice;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::*;
use windows::core::PCWSTR;

use super::{
    EntryData, Failure, Lister, NT_BUFFER_BYTES, Sink, Win32Error, extended_wide,
};

const STATUS_SUCCESS: i32 = 0;
const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005u32 as i32;
const STATUS_NO_MORE_FILES: i32 = 0x8000_0006u32 as i32;

/// `FILE_INFORMATION_CLASS` value for `FILE_ID_EXTD_DIR_INFORMATION`.
const FILE_ID_EXTD_DIRECTORY_INFORMATION: u32 = 60;
/// Size of the fixed header; the name follows it, UTF-16, not NUL terminated.
const HEADER_LEN: usize = 88;

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_SHARE_ALL: u32 = 0x0007;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// `ERROR_DIRECTORY`: the path exists but is not a directory.
const ERROR_DIRECTORY: u32 = 267;

#[repr(C)]
#[derive(Default)]
struct IoStatusBlock {
    status: isize,
    information: usize,
}

type NtQueryDirectoryFileFn = unsafe extern "system" fn(
    file_handle: HANDLE,
    event: HANDLE,
    apc_routine: *const c_void,
    apc_context: *mut c_void,
    io_status_block: *mut IoStatusBlock,
    file_information: *mut c_void,
    length: u32,
    file_information_class: u32,
    return_single_entry: u8,
    file_name: *const c_void,
    restart_scan: u8,
) -> i32;

fn nt_query_directory_file() -> Option<NtQueryDirectoryFileFn> {
    static MISSING: AtomicBool = AtomicBool::new(false);
    if MISSING.load(Ordering::Relaxed) {
        return None;
    }
    let name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    unsafe {
        let module =
            windows::Win32::System::LibraryLoader::GetModuleHandleW(PCWSTR(name.as_ptr())).ok()?;
        let proc = windows::Win32::System::LibraryLoader::GetProcAddress(
            module,
            windows::core::PCSTR(c"NtQueryDirectoryFile".as_ptr().cast::<u8>()),
        );
        match proc {
            Some(f) => Some(core::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                NtQueryDirectoryFileFn,
            >(f)),
            None => {
                MISSING.store(true, Ordering::Relaxed);
                None
            }
        }
    }
}

/// An owned directory handle: closed on every path, error and cancel included.
pub(crate) struct DirHandle(HANDLE);

impl Drop for DirHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Open `dir` for listing and reject non-directories with a typed error.
/// `FILE_FLAG_BACKUP_SEMANTICS` is what lets a directory be opened at all;
/// `FILE_FLAG_OPEN_REPARSE_POINT` is deliberately not set, so a junction is
/// followed to the real directory.
pub(crate) fn open_dir(dir: &std::path::Path) -> Result<DirHandle, Win32Error> {
    let wide = extended_wide(dir)?;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_MODE(FILE_SHARE_ALL),
            None,
            FILE_CREATION_DISPOSITION(OPEN_EXISTING),
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS),
            None,
        )
    }
    .map_err(|e| Win32Error::from_win_error(&e))?;
    let handle = DirHandle(handle);

    // CreateFileW happily opens a *file*; only the attributes tell them apart.
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let ok = unsafe { GetFileInformationByHandle(handle.0, &mut info) };
    ok.map_err(|e| Win32Error::from_win_error(&e))?;
    if info.dwFileAttributes & super::ATTR_DIRECTORY == 0 {
        return Err(Win32Error::win32(ERROR_DIRECTORY));
    }
    Ok(handle)
}

/// List `dir` into `lister`. `Err(Failure::Unsupported)` means the filesystem
/// rejected the information class and nothing was delivered yet.
pub(crate) fn list<S: Sink>(lister: &mut Lister<'_, S>, dir: &std::path::Path) -> Result<(), Failure> {
    let query = nt_query_directory_file().ok_or(Failure::Unsupported)?;
    let handle = open_dir(dir).map_err(Failure::Error)?;

    // Vec<u64>: FILE_ID_EXTD_DIR_INFORMATION holds LARGE_INTEGERs and the kernel
    // writes them with 8-byte alignment in mind.
    let mut buffer = vec![0u64; NT_BUFFER_BYTES.div_ceil(8)];
    let buf_len = (buffer.len() * 8) as u32;
    let buf_ptr = buffer.as_mut_ptr().cast::<c_void>();

    let mut restart = 1u8;
    loop {
        if lister.cancelled() {
            return Ok(());
        }
        let mut iosb = IoStatusBlock::default();
        let status = unsafe {
            query(
                handle.0,
                HANDLE(ptr::null_mut()),
                ptr::null(),
                ptr::null_mut(),
                &mut iosb,
                buf_ptr,
                buf_len,
                FILE_ID_EXTD_DIRECTORY_INFORMATION,
                0,
                ptr::null(),
                restart,
            )
        };
        restart = 0;

        if status == STATUS_NO_MORE_FILES {
            // Normal end, and also what an empty directory returns.
            return Ok(());
        }
        if status != STATUS_SUCCESS && status != STATUS_BUFFER_OVERFLOW {
            let error = Win32Error::from_nt(status);
            return Err(if error.is_unsupported() {
                Failure::Unsupported
            } else {
                Failure::Error(error)
            });
        }
        // STATUS_BUFFER_OVERFLOW is success with a partially filled buffer.
        let bytes = iosb.information.min(buf_len as usize);
        if bytes == 0 {
            return Ok(());
        }
        let buf: &[u8] = unsafe { slice::from_raw_parts(buf_ptr.cast::<u8>(), bytes) };
        if !parse_and_push(buf, lister).map_err(Failure::Error)? {
            return Ok(()); // cancelled or the consumer is gone
        }
    }
}

/// Walk one query buffer. Returns `false` when the caller must stop.
fn parse_and_push<S: Sink>(buf: &[u8], lister: &mut Lister<'_, S>) -> Result<bool, Win32Error> {
    debug_assert!((buf.as_ptr() as usize).is_multiple_of(8), "query buffer must be 8-byte aligned");
    let mut off = 0usize;
    loop {
        if off + HEADER_LEN > buf.len() {
            return Err(Win32Error::malformed());
        }
        let next = u32_at(buf, off) as usize;
        let name_len = u32_at(buf, off + 60) as usize;
        if name_len & 1 != 0 {
            return Err(Win32Error::malformed());
        }
        let name_start = off + HEADER_LEN;
        let name_end = name_start + name_len;
        if name_end > buf.len() {
            return Err(Win32Error::malformed());
        }
        let name: &[u16] =
            unsafe { slice::from_raw_parts(buf.as_ptr().add(name_start).cast::<u16>(), name_len / 2) };
        let mut file_id = [0u8; 16];
        file_id.copy_from_slice(&buf[off + 72..off + 88]);
        let ok = lister.push(
            name,
            EntryData {
                attrs: u32_at(buf, off + 56),       // FileAttributes
                size: u64_at(buf, off + 40),        // EndOfFile
                alloc_size: u64_at(buf, off + 48),  // AllocationSize
                mtime: u64_at(buf, off + 24),       // LastWriteTime
                file_id: u128::from_le_bytes(file_id),
                reparse_tag: u32_at(buf, off + 68), // ReparsePointTag
            },
        );
        if !ok {
            return Ok(false);
        }
        if next == 0 {
            return Ok(true);
        }
        if next < HEADER_LEN {
            return Err(Win32Error::malformed());
        }
        off += next;
    }
}

#[inline]
fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

#[inline]
fn u64_at(buf: &[u8], off: usize) -> u64 {
    u64::from_ne_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}
