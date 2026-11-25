//! Places the sidebar and the status bar need: drive letters with free
//! space, known folders (including OneDrive when present), WSL Linux
//! distributions, and "open this with the default handler".
//!
//! Every call here can block (an empty card reader, a dead network share, a
//! slow shell), so the app runs them on a worker thread, never on the UI
//! thread. All results are plain data.

use crate::wide;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, MAX_PATH};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{GUID, PCWSTR, PWSTR};

/// Shell parsing name for the Recycle Bin (opened via `ShellExecuteW` because
/// the filesystem enumerator lists real filesystem paths only).
pub const RECYCLE_BIN_SHELL: &str = "shell:RecycleBinFolder";
/// Maximum WSL distributions shown in the sidebar Linux section.
pub const WSL_CAP: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    pub letter: char,
    /// `C:\`
    pub root: String,
    /// volume label, empty when it has none or is not ready
    pub label: String,
    pub kind: DriveKind,
    /// 0 when the volume is not ready
    pub total: u64,
    pub free: u64,
}

impl Drive {
    /// "Windows (C:)" / "Local Disk (C:)"
    pub fn display_name(&self) -> String {
        let l = if self.label.is_empty() {
            match self.kind {
                DriveKind::Removable => "Removable Disk",
                DriveKind::Network => "Network Drive",
                DriveKind::Optical => "CD/DVD Drive",
                _ => "Local Disk",
            }
        } else {
            self.label.as_str()
        };
        format!("{l} ({}:)", self.letter)
    }
    /// Fraction used, 0..=1 (0 for a volume without a size).
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (1.0 - self.free as f64 / self.total as f64).clamp(0.0, 1.0) as f32 }
    }
    /// True when the drive supports Eject in the sidebar context menu.
    pub fn is_ejectable(&self) -> bool {
        matches!(self.kind, DriveKind::Removable | DriveKind::Optical)
    }
}

/// Checks if a local fixed drive sits on a USB bus (external USB HDD/SSD) using
/// metadata-only access (`dwDesiredAccess = 0`, no admin rights needed).
fn is_usb_storage_drive(letter: char) -> bool {
    let path = wide(&format!("\\\\.\\{letter}:"));
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    };
    let Ok(h) = h else { return false };
    let mut query: STORAGE_PROPERTY_QUERY = unsafe { zeroed() };
    query.PropertyId = StorageDeviceProperty;
    query.QueryType = PropertyStandardQuery;
    let mut desc: STORAGE_DEVICE_DESCRIPTOR = unsafe { zeroed() };
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const c_void),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut desc as *mut _ as *mut c_void),
            size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32,
            Some(&mut returned),
            None,
        )
    };
    unsafe {
        let _ = CloseHandle(h);
    }
    // BusTypeUsb = 7 (STORAGE_BUS_TYPE)
    ok.is_ok() && desc.BusType.0 == 7
}

/// Free and total bytes of the volume holding `path`; `None` when it cannot be read.
pub fn volume_space(path: &str) -> Option<(u64, u64)> {
    let w = wide(path);
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    unsafe {
        let mut old = THREAD_ERROR_MODE(0);
        let _ = SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old));
        let r = GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), Some(&mut total), Some(&mut total_free));
        let _ = SetThreadErrorMode(old, None);
        r.ok()?;
    }
    Some((free, total))
}

/// Mounted drive letters, in letter order.
pub fn list_drives() -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let w = wide(&root);
        let mut kind = match unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) } {
            3 => DriveKind::Fixed,
            2 => DriveKind::Removable,
            4 => DriveKind::Network,
            5 => DriveKind::Optical,
            _ => DriveKind::Other,
        };
        if kind == DriveKind::Fixed && is_usb_storage_drive(letter) {
            kind = DriveKind::Removable;
        }
        let (free, total) = volume_space(&root).unwrap_or((0, 0));
        let mut label = String::new();
        if total > 0 {
            let mut buf = [0u16; MAX_PATH as usize + 1];
            unsafe {
                let mut old = THREAD_ERROR_MODE(0);
                let _ = SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old));
                if GetVolumeInformationW(PCWSTR(w.as_ptr()), Some(&mut buf), None, None, None, None).is_ok() {
                    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    label = String::from_utf16_lossy(&buf[..n]);
                }
                let _ = SetThreadErrorMode(old, None);
            }
        }
        out.push(Drive { letter, root, label, kind, total, free });
    }
    out
}

/// A known folder: display name and its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub name: &'static str,
    pub path: String,
}

/// One installed WSL Linux distribution for the sidebar's "Linux" section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WslDistro {
    pub name: String,
    /// UNC root, e.g. `\\wsl.localhost\Ubuntu`.
    pub path: String,
}

fn known(id: &GUID) -> Option<String> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn reg_sz(hkey: HKEY, subkey: Option<&str>, value: &str) -> Option<String> {
    let sub_w = subkey.map(wide);
    let val_w = wide(value);
    let sub_ptr = sub_w.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
    let mut bytes = 0u32;
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let r = unsafe { RegGetValueW(hkey, sub_ptr, PCWSTR(val_w.as_ptr()), flags, None, None, Some(&mut bytes)) };
    if r != ERROR_SUCCESS || bytes < 2 {
        return None;
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    let r = unsafe {
        RegGetValueW(
            hkey,
            sub_ptr,
            PCWSTR(val_w.as_ptr()),
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut bytes),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..len]);
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Pure resolver for OneDrive candidates (tested without touching env/registry/disk).
pub fn resolve_onedrive_with(
    candidates: impl IntoIterator<Item = Option<String>>,
    exists: impl Fn(&str) -> bool,
) -> Option<String> {
    for cand in candidates.into_iter().flatten() {
        let trimmed = cand.trim();
        if !trimmed.is_empty() && exists(trimmed) {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Resolves the user's OneDrive root folder from `%OneDrive%` (and commercial/consumer
/// variants) or `HKCU\Software\Microsoft\OneDrive\UserFolder`. Returns `None` if not
/// configured or the folder does not exist on disk.
pub fn onedrive_folder() -> Option<String> {
    let env_main = std::env::var("OneDrive").ok();
    let env_comm = std::env::var("OneDriveCommercial").ok();
    let env_cons = std::env::var("OneDriveConsumer").ok();
    let reg = reg_sz(HKEY_CURRENT_USER, Some("Software\\Microsoft\\OneDrive"), "UserFolder");
    resolve_onedrive_with([env_main, env_comm, env_cons, reg], |p| Path::new(p).is_dir())
}

/// Pure builder for the WSL distro list: puts `default_name` first (when present),
/// sorts the rest alphabetically, deduplicates case-insensitively, and caps at [`WSL_CAP`].
pub fn build_wsl_distros(raw: Vec<String>, default_name: Option<&str>) -> Vec<WslDistro> {
    let mut names: Vec<String> = Vec::new();
    for n in raw {
        let t = n.trim();
        if t.is_empty() || names.iter().any(|x| x.eq_ignore_ascii_case(t)) {
            continue;
        }
        names.push(t.to_string());
    }
    names.sort_by(|a, b| {
        let ad = default_name.is_some_and(|d| a.eq_ignore_ascii_case(d));
        let bd = default_name.is_some_and(|d| b.eq_ignore_ascii_case(d));
        bd.cmp(&ad).then_with(|| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()))
    });
    names.truncate(WSL_CAP);
    names
        .into_iter()
        .map(|name| {
            let path = format!("\\\\wsl.localhost\\{name}");
            WslDistro { name, path }
        })
        .collect()
}

/// Lists installed WSL distributions by reading `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss`.
/// Never spawns `wsl.exe` or starts a WSL VM; returns empty if WSL has no installed distros.
pub fn wsl_distros() -> Vec<WslDistro> {
    let lxss_w = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Lxss");
    let mut root = HKEY::default();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(lxss_w.as_ptr()), Some(0), KEY_READ, &mut root) };
    if r != ERROR_SUCCESS {
        return Vec::new();
    }
    let default_guid = reg_sz(root, None, "DefaultDistribution");
    let mut default_name: Option<String> = None;
    let mut raw: Vec<String> = Vec::new();
    let mut idx = 0u32;
    let mut name_buf = [0u16; 256];
    loop {
        let mut len = name_buf.len() as u32;
        let er = unsafe {
            RegEnumKeyExW(
                root,
                idx,
                Some(PWSTR(name_buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if er != ERROR_SUCCESS {
            break;
        }
        idx += 1;
        let sub = String::from_utf16_lossy(&name_buf[..len as usize]);
        if let Some(distro) = reg_sz(root, Some(&sub), "DistributionName") {
            if default_guid.as_deref().is_some_and(|g| g.eq_ignore_ascii_case(&sub)) {
                default_name = Some(distro.clone());
            }
            raw.push(distro);
        }
    }
    unsafe {
        let _ = RegCloseKey(root);
    }
    build_wsl_distros(raw, default_name.as_deref())
}

/// Desktop, Downloads, Documents, Pictures, Music, Videos, the profile
/// folder, and OneDrive (when present), skipping any the machine does not have.
pub fn known_folders() -> Vec<Place> {
    let list: [(&'static str, &GUID); 7] = [
        ("Desktop", &FOLDERID_Desktop),
        ("Downloads", &FOLDERID_Downloads),
        ("Documents", &FOLDERID_Documents),
        ("Pictures", &FOLDERID_Pictures),
        ("Music", &FOLDERID_Music),
        ("Videos", &FOLDERID_Videos),
        ("Home", &FOLDERID_Profile),
    ];
    let mut out: Vec<Place> = list.iter().filter_map(|(name, id)| known(id).map(|path| Place { name, path })).collect();
    if let Some(od) = onedrive_folder() {
        out.push(Place { name: "OneDrive", path: od });
    }
    out
}

/// The folder a new window or tab starts in: Home, else `C:\`.
pub fn home_folder() -> String {
    known(&FOLDERID_Profile).unwrap_or_else(|| "C:\\".into())
}

/// Opens `path` with its default handler (the shell "open" verb) on a
/// short-lived thread: ShellExecute can block on a slow handler or network
/// path, and some handlers want an STA thread. Also handles shell parsing
/// names like `shell:RecycleBinFolder`.
pub fn open_async(path: String) {
    let _ = std::thread::Builder::new().name("tsubame-open".into()).spawn(move || unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let file = wide(&path);
        let dir_end = path.rfind('\\').map(|i| &path[..=i]).unwrap_or("");
        let dir_w = (!dir_end.is_empty()).then(|| wide(dir_end));
        let dir_ptr = dir_w.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
        let verb = wide("open");
        let _ = ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR::null(), dir_ptr, SW_SHOWNORMAL);
        if com {
            CoUninitialize();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_and_known_folders_exist() {
        let d = list_drives();
        assert!(!d.is_empty());
        let c = d.iter().find(|d| d.letter == 'C').expect("C: drive");
        assert_eq!(c.root, "C:\\");
        assert!(c.total > 0 && c.free <= c.total);
        assert!(c.display_name().ends_with("(C:)"));
        assert!((0.0..=1.0).contains(&c.used_fraction()));
        let k = known_folders();
        assert!(k.iter().any(|p| p.name == "Home" && std::path::Path::new(&p.path).is_dir()));
        assert!(volume_space("C:\\Windows").is_some());
    }

    #[test]
    fn used_fraction_edges() {
        let mut d = Drive { letter: 'X', root: "X:\\".into(), label: String::new(), kind: DriveKind::Fixed, total: 0, free: 0 };
        assert_eq!(d.used_fraction(), 0.0);
        assert!(!d.is_ejectable());
        d.total = 100;
        d.free = 25;
        assert!((d.used_fraction() - 0.75).abs() < 1e-6);
        assert_eq!(d.display_name(), "Local Disk (X:)");
        d.label = "Data".into();
        assert_eq!(d.display_name(), "Data (X:)");
        d.kind = DriveKind::Removable;
        assert!(d.is_ejectable());
    }

    #[test]
    fn onedrive_resolver_picks_first_existing_candidate() {
        let hit = resolve_onedrive_with(
            [Some("C:\\missing".into()), None, Some("C:\\Users\\me\\OneDrive".into()), Some("C:\\reg".into())],
            |p| p == "C:\\Users\\me\\OneDrive",
        );
        assert_eq!(hit.as_deref(), Some("C:\\Users\\me\\OneDrive"));
        assert_eq!(resolve_onedrive_with([None, Some("   ".into())], |_| true), None);
    }

    #[test]
    fn wsl_distros_sort_default_first_dedupe_and_cap_at_10() {
        let raw: Vec<String> = (1..=14).map(|i| format!("Distro-{i:02}")).chain(["ubuntu".into(), "UBUNTU".into(), "  ".into()]).collect();
        let out = build_wsl_distros(raw, Some("Distro-09"));
        assert_eq!(out.len(), WSL_CAP);
        assert_eq!(out[0].name, "Distro-09");
        assert_eq!(out[0].path, "\\\\wsl.localhost\\Distro-09");
        assert_eq!(out[1].name, "Distro-01");
        assert!(build_wsl_distros(Vec::new(), None).is_empty());
    }
}
