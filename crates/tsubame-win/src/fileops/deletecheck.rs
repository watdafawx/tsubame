//! Before a Delete goes to the queue: which items can the Recycle Bin take?
//!
//! The shell deletes **permanently** without telling us when:
//! - the path has 260 characters or more (`recycle()` refuses those),
//! - the volume has no bin, or the bin is turned off (`NukeOnDelete`),
//! - the item is bigger than the bin's maximum size.
//!
//! [`check`] finds these up front so the app can ask before a permanent
//! delete instead of silently losing data. It reads the disk (folder sizes),
//! so run it on a worker thread, never the UI thread.

use super::recycle::{MAX_PATH_CHARS, has_recycle_bin};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::PCWSTR;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// fine to recycle
    Recycle,
    /// recycling would delete it for good (or might): ask first. The text says why.
    Permanent(String),
    /// gone already
    Missing,
}

/// Folder sizing stops after this long; an item not measured in time is
/// treated as "might not fit".
pub const SIZE_BUDGET: Duration = Duration::from_secs(8);

/// Verdict per path, in order.
pub fn check(paths: &[PathBuf]) -> Vec<(PathBuf, Verdict)> {
    let deadline = Instant::now() + SIZE_BUDGET;
    paths.iter().map(|p| (p.clone(), verdict(p, deadline))).collect()
}

fn verdict(p: &Path, deadline: Instant) -> Verdict {
    let Ok(meta) = std::fs::symlink_metadata(p) else { return Verdict::Missing };
    if p.as_os_str().len() >= MAX_PATH_CHARS {
        return Verdict::Permanent(format!("the path is {} characters or longer", MAX_PATH_CHARS));
    }
    if !has_recycle_bin(p) {
        return Verdict::Permanent("this drive has no Recycle Bin".into());
    }
    let bin = bin_settings(p);
    if bin.nuke {
        return Verdict::Permanent("the Recycle Bin is turned off for this drive".into());
    }
    let Some(max) = bin.max_bytes else { return Verdict::Recycle };
    let size = if meta.is_dir() && !meta.file_type().is_symlink() { tree_size(p, max, deadline) } else { Some(meta.len()) };
    match size {
        Some(s) if s > max => Verdict::Permanent(format!("it is larger than the Recycle Bin ({} MB)", max >> 20)),
        Some(_) => Verdict::Recycle,
        None => Verdict::Permanent("it is too large to measure quickly and may not fit in the Recycle Bin".into()),
    }
}

/// Bytes under `root`, stopping early once over `stop_above` (returns that
/// partial sum, already too big). `None` if the deadline passed first.
/// Reparse points are not followed.
pub fn tree_size(root: &Path, stop_above: u64, deadline: Instant) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    let mut n = 0u32;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            n += 1;
            if n.is_multiple_of(256) && Instant::now() > deadline {
                return None;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(e.path());
            } else if let Ok(m) = e.metadata() {
                total = total.saturating_add(m.len());
                if total > stop_above {
                    return Some(total);
                }
            }
        }
    }
    Some(total)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BinSettings {
    pub nuke: bool,
    /// maximum bin size of the volume; None if unknown
    pub max_bytes: Option<u64>,
}

/// The per-volume bin settings (`HKCU\...\Explorer\BitBucket\Volume\{guid}`:
/// `NukeOnDelete`, `MaxCapacity` in MB). A volume without the key uses the
/// Windows default, taken here as 5% of the volume (conservative: the real
/// default is about that or larger).
pub fn bin_settings(p: &Path) -> BinSettings {
    let w = crate::wide(&p.to_string_lossy());
    let mut root = [0u16; 261];
    if unsafe { GetVolumePathNameW(PCWSTR(w.as_ptr()), &mut root) }.is_err() {
        return BinSettings::default();
    }
    let mut total = 0u64;
    unsafe {
        let _ = GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), None, Some(&mut total), None);
    }
    let default = (total > 0).then_some(total / 20);
    let mut name = [0u16; 64];
    if unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut name) }.is_err() {
        return BinSettings { nuke: false, max_bytes: default };
    }
    let name = String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(0)]);
    let Some(guid) = name.find('{').and_then(|a| name[a..].find('}').map(|b| &name[a..a + b + 1])) else {
        return BinSettings { nuke: false, max_bytes: default };
    };
    let key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
    let nuke = reg_dword(&key, "NukeOnDelete").is_some_and(|v| v != 0);
    let max = reg_dword(&key, "MaxCapacity").map(|mb| (mb as u64) << 20).or(default);
    BinSettings { nuke, max_bytes: max }
}

fn reg_dword(key: &str, value: &str) -> Option<u32> {
    let (k, v) = (crate::wide(key), crate::wide(value));
    let mut out = 0u32;
    let mut sz = 4u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), PCWSTR(v.as_ptr()), RRF_RT_REG_DWORD, None, Some(&mut out as *mut u32 as *mut _), Some(&mut sz)) };
    (r.is_ok()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts() {
        let dir = std::env::temp_dir().join(format!("tsubame-delcheck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("a.bin"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.join("b.txt"), b"hi").unwrap();
        // sizes
        let far = Instant::now() + Duration::from_secs(10);
        assert_eq!(tree_size(&dir, u64::MAX, far), Some(3002));
        assert!(tree_size(&dir, 100, far).unwrap() > 100);
        let r = check(&[dir.join("b.txt"), dir.join("missing")]);
        assert_eq!(r[1].1, Verdict::Missing);
        // the temp volume normally has a bin; without one the verdict says so
        if has_recycle_bin(&dir) {
            let s = bin_settings(&dir);
            if !s.nuke {
                assert_eq!(r[0].1, Verdict::Recycle);
            }
        } else {
            assert!(matches!(r[0].1, Verdict::Permanent(_)));
        }
        // a long path is never recycled
        let long = dir.join("x".repeat(250)).join("y".repeat(20));
        if std::fs::create_dir_all(crate::fileops::recycle::long_path(&long)).is_ok() {
            let r = check(std::slice::from_ref(&long));
            assert!(matches!(&r[0].1, Verdict::Permanent(m) if m.contains("260")), "{:?}", r[0].1);
        }
        let _ = std::fs::remove_dir_all(crate::fileops::recycle::long_path(&dir));
    }
}
