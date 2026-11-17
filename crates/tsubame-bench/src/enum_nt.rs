//! Method B/C: `NtQueryDirectoryFile` from ntdll, caller-sized buffer.
//!
//! Asks for `FileIdExtdDirectoryInformation` (class 60, 128-bit file id) and
//! falls back to `FileFullDirectoryInformation` (class 2, no file id) on builds
//! that reject class 60. One syscall fills the whole buffer, so the buffer size
//! is the knob: 64 KB (the default kernel fetch) vs 1 MB.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::core::{PCSTR, PCWSTR};

use crate::clock::Clock;
use crate::dirhandle::open_dir;
use crate::ntinfo::{
    FILE_FULL_DIRECTORY_INFORMATION, FILE_ID_EXTD_DIRECTORY_INFORMATION, Layout, parse_buffer,
};
use crate::store::EntryStore;

const STATUS_SUCCESS: i32 = 0;
const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005u32 as i32;
const STATUS_NO_MORE_FILES: i32 = 0x8000_0006u32 as i32;
const STATUS_INVALID_INFO_CLASS: i32 = 0xC000_0003u32 as i32;
const STATUS_INVALID_PARAMETER: i32 = 0xC000_000Du32 as i32;

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

fn nt_query_directory_file() -> Result<NtQueryDirectoryFileFn, String> {
    static MISSING: AtomicBool = AtomicBool::new(false);
    if MISSING.load(Ordering::Relaxed) {
        return Err("NtQueryDirectoryFile not found in ntdll".to_string());
    }
    let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    unsafe {
        let hmod = windows::Win32::System::LibraryLoader::GetModuleHandleW(PCWSTR(ntdll.as_ptr()))
            .map_err(|e| format!("GetModuleHandleW(ntdll.dll): {}", e.message()))?;
        let proc = windows::Win32::System::LibraryLoader::GetProcAddress(
            hmod,
            PCSTR(c"NtQueryDirectoryFile".as_ptr() as *const u8),
        );
        match proc {
            Some(f) => Ok(core::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                NtQueryDirectoryFileFn,
            >(f)),
            None => {
                MISSING.store(true, Ordering::Relaxed);
                Err("NtQueryDirectoryFile not exported by ntdll".to_string())
            }
        }
    }
}

/// Enumerate `dir` with `NtQueryDirectoryFile` into a `buf_bytes` buffer.
/// Returns the layout actually used.
pub fn enumerate(
    dir: &str,
    buf_bytes: usize,
    store: &mut EntryStore,
    clock: &Clock,
    start: i64,
) -> Result<Layout, String> {
    let query = nt_query_directory_file()?;
    let handle = open_dir(dir)?;

    // Vec<u64> so the buffer is 8-byte aligned: FILE_ID_EXTD_DIR_INFORMATION
    // contains LARGE_INTEGERs and the kernel writes them with that assumption.
    let words = buf_bytes.div_ceil(8);
    let mut buffer = vec![0u64; words];
    let buf_len = (words * 8) as u32;
    let buf_ptr = buffer.as_mut_ptr() as *mut c_void;

    let mut layout = Layout::Extd;
    let mut class = FILE_ID_EXTD_DIRECTORY_INFORMATION;
    let mut restart = 1u8;
    let mut tried_fallback = false;
    let mut iosb = IoStatusBlock::default();

    let result = (|| -> Result<(), String> {
        loop {
            iosb = IoStatusBlock::default();
            let status = unsafe {
                query(
                    handle,
                    HANDLE(core::ptr::null_mut()),
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    &mut iosb,
                    buf_ptr,
                    buf_len,
                    class,
                    0,
                    core::ptr::null(),
                    restart,
                )
            };
            restart = 0;

            if status == STATUS_INVALID_INFO_CLASS || status == STATUS_INVALID_PARAMETER {
                if !tried_fallback {
                    tried_fallback = true;
                    layout = Layout::Full;
                    class = FILE_FULL_DIRECTORY_INFORMATION;
                    restart = 1;
                    continue;
                }
                return Err(format!(
                    "NtQueryDirectoryFile class {class} rejected: status {status:#010X}"
                ));
            }
            if status == STATUS_NO_MORE_FILES {
                return Ok(());
            }
            if status != STATUS_SUCCESS && status != STATUS_BUFFER_OVERFLOW {
                return Err(format!(
                    "NtQueryDirectoryFile failed in {dir}: status {status:#010X}"
                ));
            }
            let bytes = iosb.information.min(buf_len as usize);
            if bytes == 0 {
                return Ok(());
            }
            let buf: &[u8] = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, bytes) };
            parse_buffer(buf, layout, store)?;
            store.note_progress(clock, start);
        }
    })();

    unsafe {
        let _ = CloseHandle(handle);
    }
    result.map(|()| layout)
}
