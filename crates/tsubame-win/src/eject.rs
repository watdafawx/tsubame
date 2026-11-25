//! Safe ejection of removable and USB drives.
//!
//! Runs off the UI thread (`eject_async`):
//! 1. Locks and dismounts the volume (`FSCTL_LOCK_VOLUME`, `FSCTL_DISMOUNT_VOLUME`)
//!    and requests media ejection (`IOCTL_STORAGE_MEDIA_REMOVAL`,
//!    `IOCTL_STORAGE_EJECT_MEDIA`) for removable media (flash drives, SD cards,
//!    optical drives).
//! 2. Requests PnP device ejection via `CM_Request_Device_EjectW` on the parent
//!    device node (matching `STORAGE_DEVICE_NUMBER` via `SetupDiGetClassDevsW`
//!    on `GUID_DEVINTERFACE_DISK`), which handles USB external drives and
//!    reports clear veto reasons when a handle is still open.

use crate::wide;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::time::Duration;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::*;
use windows::core::{GUID, PCWSTR};

/// `GUID_DEVINTERFACE_DISK` (`{53f56307-b6bf-11d0-94f2-00a0c91efb8b}`).
const GUID_DISK: GUID = GUID::from_u128(0x53f56307_b6bf_11d0_94f2_00a0c91efb8b);

/// Why ejecting a drive failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EjectError {
    /// Not an ASCII drive letter (`A`..=`Z`).
    InvalidDrive,
    /// Open handles or a PnP veto prevented ejection.
    InUse(String),
    /// The drive letter does not exist or has no media.
    NotReady,
    /// Another Win32 / CfgMgr32 failure.
    Failed(String),
}

impl EjectError {
    /// Short status-bar message for the user.
    pub fn message(&self, letter: char) -> String {
        let l = letter.to_ascii_uppercase();
        match self {
            EjectError::InvalidDrive => "Can't eject: invalid drive".into(),
            EjectError::InUse(detail) if detail.is_empty() => format!("Can't eject {l}:: in use"),
            EjectError::InUse(detail) => format!("Can't eject {l}:: in use ({detail})"),
            EjectError::NotReady => format!("Can't eject {l}:: drive not ready"),
            EjectError::Failed(why) => format!("Can't eject {l}:: {why}"),
        }
    }
}

/// Owned Win32 `HANDLE` closed on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Owned `HDEVINFO` freed on drop.
struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn win32_code(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

/// Maps a Win32 error code from volume open / lock / eject to [`EjectError`].
pub fn classify_win32(code: u32) -> EjectError {
    match code {
        5 | 32 | 33 | 170 => EjectError::InUse(String::new()),
        2 | 3 | 15 | 21 | 1167 => EjectError::NotReady,
        _ => EjectError::Failed(format!("error {code}")),
    }
}

/// Maps a `CM_Request_Device_EjectW` veto to [`EjectError::InUse`].
pub fn classify_veto(veto_type: PNP_VETO_TYPE, veto_name: &str) -> EjectError {
    let trimmed = veto_name.trim();
    if !trimmed.is_empty() && !trimmed.contains('\\') {
        return EjectError::InUse(trimmed.to_string());
    }
    let detail = if veto_type == PNP_VetoWindowsApp {
        "open application"
    } else if veto_type == PNP_VetoWindowsService {
        "running service"
    } else if veto_type == PNP_VetoOutstandingOpen {
        "open files"
    } else {
        ""
    };
    EjectError::InUse(detail.to_string())
}

fn open_volume(letter: char, access: u32) -> Result<OwnedHandle, u32> {
    let path = wide(&format!("\\\\.\\{}:", letter.to_ascii_uppercase()));
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map_err(|e| win32_code(&e))?;
    Ok(OwnedHandle(h))
}

fn query_device_number(handle: HANDLE) -> Option<STORAGE_DEVICE_NUMBER> {
    let mut num: STORAGE_DEVICE_NUMBER = unsafe { zeroed() };
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut num as *mut _ as *mut c_void),
            size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&mut returned),
            None,
        )
    };
    ok.is_ok().then_some(num)
}

fn ioctl_empty(handle: HANDLE, code: u32) -> Result<(), u32> {
    let mut returned = 0u32;
    unsafe { DeviceIoControl(handle, code, None, 0, None, 0, Some(&mut returned), None) }
        .map_err(|e| win32_code(&e))
}

fn ioctl_allow_removal(handle: HANDLE) -> Result<(), u32> {
    let mut pmr = PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: false };
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_MEDIA_REMOVAL,
            Some(&mut pmr as *mut _ as *const c_void),
            size_of::<PREVENT_MEDIA_REMOVAL>() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|e| win32_code(&e))
}

/// Finds the disk `DEVINST` whose `STORAGE_DEVICE_NUMBER` matches `target` and
/// asks CfgMgr32 to eject its parent device node (e.g. the USB mass-storage device).
fn pnp_eject_disk(target: STORAGE_DEVICE_NUMBER) -> Option<Result<(), EjectError>> {
    let hdev = unsafe {
        SetupDiGetClassDevsW(Some(&GUID_DISK), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
    }
    .ok()?;
    let hdev = DevInfo(hdev);
    let mut dev_inst: Option<u32> = None;
    let mut index = 0u32;
    loop {
        let mut if_data = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInterfaces(hdev.0, None, &GUID_DISK, index, &mut if_data) }.is_err() {
            break;
        }
        index += 1;
        let mut needed = 0u32;
        let _ = unsafe { SetupDiGetDeviceInterfaceDetailW(hdev.0, &if_data, None, 0, Some(&mut needed), None) };
        if needed < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 {
            continue;
        }
        let mut buf = vec![0u8; needed as usize];
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        let mut dev_info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                hdev.0,
                &if_data,
                Some(detail),
                needed,
                None,
                Some(&mut dev_info),
            )
        }
        .is_err()
        {
            continue;
        }
        let path_ptr = unsafe { &(*detail).DevicePath as *const u16 };
        let dh = unsafe {
            CreateFileW(
                PCWSTR(path_ptr),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        if let Ok(h) = dh {
            let h = OwnedHandle(h);
            if let Some(n) = query_device_number(h.0)
                && n.DeviceType == target.DeviceType
                && n.DeviceNumber == target.DeviceNumber
            {
                dev_inst = Some(dev_info.DevInst);
                break;
            }
        }
    }
    let disk = dev_inst?;
    let mut parent = 0u32;
    let node = if unsafe { CM_Get_Parent(&mut parent, disk, 0) } == CR_SUCCESS {
        parent
    } else {
        disk
    };
    let mut veto_type = PNP_VETO_TYPE(0);
    let mut veto_buf = [0u16; MAX_PATH as usize];
    let cr = unsafe { CM_Request_Device_EjectW(node, Some(&mut veto_type), Some(&mut veto_buf), 0) };
    if cr == CR_SUCCESS {
        return Some(Ok(()));
    }
    if cr == CR_REMOVE_VETOED {
        let len = veto_buf.iter().position(|&c| c == 0).unwrap_or(veto_buf.len());
        let name = String::from_utf16_lossy(&veto_buf[..len]);
        return Some(Err(classify_veto(veto_type, &name)));
    }
    Some(Err(EjectError::Failed(format!("device eject returned {cr:?}"))))
}

/// Ejects the removable / USB drive at `letter` (`'E'`, `'f'`, ...).
/// Blocking: always run on a background thread.
pub fn eject_drive(letter: char) -> Result<(), EjectError> {
    if !letter.is_ascii_alphabetic() {
        return Err(EjectError::InvalidDrive);
    }
    let letter = letter.to_ascii_uppercase();
    // Open with read+write first (needed for lock/dismount); fall back to read-only or metadata-only.
    let handle = match open_volume(letter, GENERIC_READ.0 | GENERIC_WRITE.0)
        .or_else(|_| open_volume(letter, GENERIC_READ.0))
        .or_else(|_| open_volume(letter, 0))
    {
        Ok(h) => h,
        Err(code) => return Err(classify_win32(code)),
    };
    let dev_num = query_device_number(handle.0);

    // Retry locking briefly in case a recent folder close is finishing.
    let mut locked = false;
    let mut lock_err = 0u32;
    for attempt in 0..3 {
        match ioctl_empty(handle.0, FSCTL_LOCK_VOLUME) {
            Ok(()) => {
                locked = true;
                break;
            }
            Err(code) => {
                lock_err = code;
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(120));
                }
            }
        }
    }

    let mut ioctl_ejected = false;
    if locked {
        let _ = ioctl_empty(handle.0, FSCTL_DISMOUNT_VOLUME);
        let _ = ioctl_allow_removal(handle.0);
        if ioctl_empty(handle.0, IOCTL_STORAGE_EJECT_MEDIA).is_ok() {
            ioctl_ejected = true;
        }
    }
    // Close our own handle BEFORE asking PnP to eject so we don't veto ourselves!
    drop(handle);

    if let Some(num) = dev_num
        && let Some(pnp) = pnp_eject_disk(num)
    {
        match pnp {
            Ok(()) => return Ok(()),
            Err(_) if ioctl_ejected => return Ok(()),
            Err(e) => return Err(e),
        }
    }

    if ioctl_ejected {
        return Ok(());
    }
    Err(classify_win32(if lock_err != 0 { lock_err } else { 5 }))
}

/// Runs [`eject_drive`] on a short-lived background thread and delivers the result to `done`.
pub fn eject_async(letter: char, done: impl FnOnce(char, Result<(), EjectError>) + Send + 'static) {
    let l = letter.to_ascii_uppercase();
    let _ = std::thread::Builder::new().name("tsubame-eject".into()).spawn(move || {
        let res = eject_drive(l);
        done(l, res);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_drive_letters() {
        assert_eq!(eject_drive('1'), Err(EjectError::InvalidDrive));
        assert_eq!(eject_drive(' '), Err(EjectError::InvalidDrive));
        assert_eq!(EjectError::InvalidDrive.message('1'), "Can't eject: invalid drive");
    }

    #[test]
    fn classifies_errors_and_formats_clear_messages() {
        assert_eq!(classify_win32(5), EjectError::InUse(String::new()));
        assert_eq!(classify_win32(32), EjectError::InUse(String::new()));
        assert_eq!(classify_win32(33), EjectError::InUse(String::new()));
        assert_eq!(classify_win32(2), EjectError::NotReady);
        assert_eq!(classify_win32(21), EjectError::NotReady);
        assert_eq!(EjectError::InUse(String::new()).message('e'), "Can't eject E:: in use");
        assert_eq!(EjectError::InUse("open files".into()).message('e'), "Can't eject E:: in use (open files)");
        assert_eq!(EjectError::NotReady.message('f'), "Can't eject F:: drive not ready");
        assert_eq!(classify_veto(PNP_VetoOutstandingOpen, ""), EjectError::InUse("open files".into()));
        assert_eq!(classify_veto(PNP_VetoWindowsApp, "Notepad"), EjectError::InUse("Notepad".into()));
    }
}
