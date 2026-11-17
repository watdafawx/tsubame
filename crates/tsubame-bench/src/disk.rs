//! Best-effort disk-kind detection for the report: asks the volume whether the
//! device incurs a seek penalty (`STORAGE_DEVICE_SEEK_PENALTY_DESCRIPTOR`).
//! Needs no admin: the volume is opened with zero desired access.

use core::ffi::c_void;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::PCWSTR;

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x0002_D1400;
const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
const PROPERTY_STANDARD_QUERY: u32 = 0;

#[repr(C)]
struct StoragePropertyQuery {
    property_id: u32,
    query_type: u32,
    additional: [u8; 4],
}

#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub volume: String,
    pub seek_penalty: Option<bool>,
    pub detail: String,
}

impl DiskInfo {
    /// "hdd" / "ssd" / "unknown" from the seek-penalty flag.
    pub fn kind(&self) -> &'static str {
        match self.seek_penalty {
            Some(true) => "hdd",
            Some(false) => "ssd",
            None => "unknown",
        }
    }
}

/// `C:\dir` -> `C:`
fn volume_root(dir: &str) -> Option<String> {
    let b = dir.as_bytes();
    if b.len() >= 2 && b[1] == b':' {
        Some(dir[..2].to_string())
    } else {
        None
    }
}

pub fn detect(dir: &str) -> DiskInfo {
    let Some(vol) = volume_root(dir) else {
        return DiskInfo {
            volume: "?".to_string(),
            seek_penalty: None,
            detail: "path has no drive letter".to_string(),
        };
    };
    let device = format!("\\\\.\\{vol}");
    let mut wide: Vec<u16> = device.encode_utf16().collect();
    wide.push(0);

    let handle: HANDLE = unsafe {
        match CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_MODE(0x7),
            None,
            FILE_CREATION_DISPOSITION(3),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                return DiskInfo {
                    volume: vol,
                    seek_penalty: None,
                    detail: format!("open {device} failed: {}", e.message()),
                };
            }
        }
    };

    let query = StoragePropertyQuery {
        property_id: STORAGE_DEVICE_SEEK_PENALTY_PROPERTY,
        query_type: PROPERTY_STANDARD_QUERY,
        additional: [0; 4],
    };
    // STORAGE_DEVICE_SEEK_PENALTY_DESCRIPTOR: Version(4), Size(4), IncursSeekPenalty(1)
    let mut out = [0u8; 12];
    let mut returned = 0u32;
    let res = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const StoragePropertyQuery as *const c_void),
            core::mem::size_of::<StoragePropertyQuery>() as u32,
            Some(out.as_mut_ptr() as *mut c_void),
            out.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    match res {
        Ok(()) if returned >= 9 => {
            let penalty = out[8] != 0;
            DiskInfo {
                volume: vol,
                seek_penalty: Some(penalty),
                detail: format!(
                    "IOCTL_STORAGE_QUERY_PROPERTY seek penalty = {penalty} ({} bytes)",
                    returned
                ),
            }
        }
        Ok(()) => DiskInfo {
            volume: vol,
            seek_penalty: None,
            detail: format!("seek-penalty descriptor too short ({returned} bytes)"),
        },
        Err(e) => DiskInfo {
            volume: vol,
            seek_penalty: None,
            detail: format!("DeviceIoControl failed: {}", e.message()),
        },
    }
}
