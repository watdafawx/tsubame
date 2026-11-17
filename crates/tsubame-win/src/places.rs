//! Places the sidebar and the status bar need: drive letters with free
//! space, known folders, and "open this with the default handler".
//!
//! Every call here can block (an empty card reader, a dead network share, a
//! slow shell), so the app runs them on a worker thread, never on the UI
//! thread. All results are plain data.

use crate::wide;
use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{GUID, PCWSTR};

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
        let kind = match unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) } {
            3 => DriveKind::Fixed,
            2 => DriveKind::Removable,
            4 => DriveKind::Network,
            5 => DriveKind::Optical,
            _ => DriveKind::Other,
        };
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

fn known(id: &GUID) -> Option<String> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Desktop, Downloads, Documents, Pictures, Music, Videos and the profile
/// folder, skipping any the machine does not have.
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
    list.iter().filter_map(|(name, id)| known(id).map(|path| Place { name, path })).collect()
}

/// The folder a new window or tab starts in: Home, else `C:\`.
pub fn home_folder() -> String {
    known(&FOLDERID_Profile).unwrap_or_else(|| "C:\\".into())
}

/// Opens `path` with its default handler (the shell "open" verb) on a
/// short-lived thread: ShellExecute can block on a slow handler or network
/// path, and some handlers want an STA thread.
pub fn open_async(path: String) {
    let _ = std::thread::Builder::new().name("tsubame-open".into()).spawn(move || unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let file = wide(&path);
        let dir_end = path.rfind('\\').map(|i| &path[..=i]).unwrap_or("");
        let dir = wide(dir_end);
        let verb = wide("open");
        let _ = ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR(dir.as_ptr()), SW_SHOWNORMAL);
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
        d.total = 100;
        d.free = 25;
        assert!((d.used_fraction() - 0.75).abs() < 1e-6);
        assert_eq!(d.display_name(), "Local Disk (X:)");
        d.label = "Data".into();
        assert_eq!(d.display_name(), "Data (X:)");
    }
}
