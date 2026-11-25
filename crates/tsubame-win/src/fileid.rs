//! File ids for the P4.F6 tag store: the volume a path lives on (64-bit
//! serial, file system), the 128-bit file id of a path, and the current path
//! of a file id. The id is the same one the enumerator reads from
//! `FileIdExtdDirectoryInformation`, so a listing gives it for free.
//!
//! Every call opens handles with `FILE_READ_ATTRIBUTES` only and full sharing:
//! nothing is read, locked or modified.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ExtendedFileIdType, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_128, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo, GetFileInformationByHandleEx, GetFinalPathNameByHandleW, GetVolumeInformationW, GetVolumePathNameW, OPEN_EXISTING, OpenFileById, VOLUME_NAME_DOS,
};
use windows::core::PCWSTR;

use crate::wide;

/// A volume as the tag store keys it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolInfo {
    /// `FILE_ID_INFO::VolumeSerialNumber` of the root (64 bits on ReFS)
    pub serial: u64,
    /// `NTFS`, `ReFS`, `FAT32`, `exFAT` ...
    pub fs: String,
    /// `C:\` or `\\server\share\`
    pub root: String,
}

impl VolInfo {
    /// File ids survive renames and moves within the volume.
    pub fn ids_stable(&self) -> bool {
        self.fs.eq_ignore_ascii_case("NTFS") || self.fs.eq_ignore_ascii_case("ReFS")
    }
}

struct H(HANDLE);
impl Drop for H {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open(path: &str) -> Option<H> {
    let w = wide(path);
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    unsafe { CreateFileW(PCWSTR(w.as_ptr()), FILE_READ_ATTRIBUTES.0, share, None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, None) }.ok().map(H)
}

fn id_info(h: &H) -> Option<FILE_ID_INFO> {
    let mut info = FILE_ID_INFO::default();
    unsafe { GetFileInformationByHandleEx(h.0, FileIdInfo, &mut info as *mut _ as *mut _, std::mem::size_of::<FILE_ID_INFO>() as u32) }.ok()?;
    Some(info)
}

/// The volume `path` is on.
pub fn volume_of(path: &str) -> Option<VolInfo> {
    let w = wide(path);
    let mut buf = [0u16; 1024];
    unsafe { GetVolumePathNameW(PCWSTR(w.as_ptr()), &mut buf) }.ok()?;
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let root = String::from_utf16_lossy(&buf[..n]);
    let rw = wide(&root);
    let mut fsname = [0u16; 64];
    unsafe { GetVolumeInformationW(PCWSTR(rw.as_ptr()), None, None, None, None, Some(&mut fsname)) }.ok()?;
    let n = fsname.iter().position(|&c| c == 0).unwrap_or(fsname.len());
    let fs = String::from_utf16_lossy(&fsname[..n]);
    let serial = open(&root).and_then(|h| id_info(&h)).map(|i| i.VolumeSerialNumber)?;
    Some(VolInfo { serial, fs, root })
}

/// (volume serial, file id) of `path`.
pub fn file_id(path: &str) -> Option<(u64, u128)> {
    let h = open(path)?;
    let i = id_info(&h)?;
    Some((i.VolumeSerialNumber, u128::from_le_bytes(i.FileId.Identifier)))
}

/// The current path of `fid` on the volume at `root` (`None` when the file
/// is gone or the volume does not support open-by-id).
pub fn path_by_id(root: &str, fid: u128) -> Option<String> {
    let hint = open(root)?;
    let desc = FILE_ID_DESCRIPTOR {
        dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: ExtendedFileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { ExtendedFileId: FILE_ID_128 { Identifier: fid.to_le_bytes() } },
    };
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let h = H(unsafe { OpenFileById(hint.0, &desc, FILE_READ_ATTRIBUTES.0, share, None, FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS.0)) }.ok()?);
    let mut buf = vec![0u16; 1024];
    let mut n = unsafe { GetFinalPathNameByHandleW(h.0, &mut buf, VOLUME_NAME_DOS) } as usize;
    if n >= buf.len() {
        buf = vec![0u16; n + 1];
        n = unsafe { GetFinalPathNameByHandleW(h.0, &mut buf, VOLUME_NAME_DOS) } as usize;
    }
    if n == 0 || n >= buf.len() {
        return None;
    }
    Some(strip_verbatim(&String::from_utf16_lossy(&buf[..n])))
}

/// `\\?\C:\x` -> `C:\x`, `\\?\UNC\s\x` -> `\\s\x`.
pub fn strip_verbatim(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = p.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        p.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_a_rename_and_resolve_back() {
        let d = std::env::temp_dir().join(format!("tsubame-fileid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("a.txt");
        std::fs::write(&a, b"x").unwrap();
        let a = a.to_string_lossy().into_owned();
        let vol = volume_of(&a).expect("volume");
        let (serial, fid) = file_id(&a).expect("id");
        assert_eq!(serial, vol.serial);
        let b = d.join("b.txt");
        std::fs::rename(&a, &b).unwrap();
        assert_eq!(file_id(&b.to_string_lossy()).unwrap().1, fid, "a rename keeps the id");
        if vol.ids_stable() {
            let p = path_by_id(&vol.root, fid).expect("open by id");
            // `temp_dir` may hand out the 8.3 short form (RUNNER~1) while the
            // final path is long: compare against the canonical long path.
            let want = strip_verbatim(&std::fs::canonicalize(&b).unwrap().to_string_lossy());
            assert!(p.eq_ignore_ascii_case(&want), "{p} != {want}");
        }
        assert_eq!(strip_verbatim("\\\\?\\UNC\\s\\x"), "\\\\s\\x");
        let _ = std::fs::remove_dir_all(&d);
    }
}
