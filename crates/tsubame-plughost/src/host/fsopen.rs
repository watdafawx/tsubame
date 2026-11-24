//! Handle-based file access for the `host` imports (security review PH-04,
//! PH-05, PH-06, and the resolved half of PH-01).
//!
//! A check on a path string followed by a second open of the same string can
//! be raced, and a name that does not exist yet (a dangling link) can not be
//! resolved at all. Here the check is done on the handle that the I/O then
//! uses, and nothing is opened twice by path:
//!
//! 1. The grant's folder is opened (links in the folder's own path are
//!    followed: that is the folder the user approved) and its final path is
//!    read from the handle (`GetFinalPathNameByHandleW`).
//! 2. Every component below it is opened **relative to the handle of its
//!    parent** (`NtCreateFile` with `RootDirectory`) with
//!    `FILE_OPEN_REPARSE_POINT`, so no link is followed by the kernel. A
//!    name-surrogate reparse point (symbolic link, junction, mount point) is
//!    refused. Other reparse points (cloud files, deduplicated files) are not
//!    links and are opened again, relative to the same parent, normally.
//! 3. A file that does not exist is created relative to its parent's handle
//!    with `FILE_CREATE` and `FILE_OPEN_REPARSE_POINT`: a name that appears in
//!    between makes the create fail, it is never followed.
//! 4. The final path of the opened handle must lie under the final path of the
//!    folder. Under a folder or input grant, a file with more than one hard
//!    link is refused (read, write and stat): its other names may be outside.
//! 5. The caller does all I/O on the returned handle.
//!
//! A path that is exactly the folder of a grant, or exactly an input path, is
//! opened as given, links followed: the user handed over that very path.
//!
//! Network: a root that may not be remote (`*` always, a folder grant without
//! `network = true`) is refused before anything is opened when it is on a
//! network drive, and any handle whose final path is UNC is refused.

use crate::caps::{self, Root};
use std::fs::File;

/// What the caller will do with the handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Stat,
    Read,
    List,
    Write { append: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Outside the grant, through a link, a hard link, or on the network.
    Denied,
    NotFound,
    Io,
}

/// One directory entry, as `read-dir` reports it.
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// 100 ns steps since 1601-01-01 UTC.
    pub mtime: u64,
}

/// Opens `norm` (a [`caps::normalize`]d path) for `want` under one of `roots`
/// (what [`caps::PathPolicy::check`] returned for it). `network` is the
/// plugin's network capability.
pub fn open_checked(norm: &str, roots: &[Root], network: bool, want: Want) -> Result<File, OpenError> {
    let k = caps::key(norm);
    // The path itself first (it may follow links), the folder grants next, `*` last.
    let mut order: Vec<&Root> = roots.iter().collect();
    order.sort_by_key(|r| match r {
        Root::Dir(d) | Root::Input(d) if caps::key(d) == k => 0,
        Root::Any => 2,
        _ => 1,
    });
    let mut err = OpenError::Denied;
    for root in order {
        match imp::open_under(norm, &k, root, network, want) {
            Ok(f) => return Ok(f),
            Err(e) => err = e,
        }
    }
    Err(err)
}

/// Lists a directory opened with [`Want::List`], through its handle.
pub fn read_dir(dir: &File, skip: usize, max: usize) -> Result<Vec<Entry>, OpenError> {
    imp::read_dir(dir, skip, max)
}

#[cfg(windows)]
mod imp {
    use super::{Entry, OpenError, Want};
    use crate::caps::{self, Root};
    use std::fs::File;
    use std::io;
    use std::mem::{offset_of, size_of, zeroed};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::{null, null_mut};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
    };
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_TAG_INFO, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY,
        FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FileAttributeTagInfo,
        FileDispositionInfo, FileFullDirectoryInfo, FileFullDirectoryRestartInfo, GetDriveTypeW, GetFileInformationByHandle, GetFileInformationByHandleEx,
        GetFinalPathNameByHandleW, OPEN_ALWAYS, OPEN_EXISTING, SYNCHRONIZE, SetFileInformationByHandle, VOLUME_NAME_DOS,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    /// Enough to open a folder on the way down and ask what it is.
    const TRAVERSE: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
    /// `IsReparseTagNameSurrogate`: the reparse point names another file (link, junction, mount point).
    const NAME_SURROGATE: u32 = 0x2000_0000;
    const DRIVE_REMOTE: u32 = 4;

    impl Want {
        fn access(self) -> u32 {
            match self {
                Want::Stat => FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                Want::Read => FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                Want::List => FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                // Without FILE_WRITE_DATA every write goes to the end of the file.
                Want::Write { append: true } => FILE_APPEND_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                Want::Write { append: false } => FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES,
            }
        }

        fn options(self) -> u32 {
            match self {
                Want::Stat => 0,
                Want::List => FILE_DIRECTORY_FILE,
                Want::Read | Want::Write { .. } => FILE_NON_DIRECTORY_FILE,
            }
        }
    }

    fn from_io(e: io::Error) -> OpenError {
        if e.kind() == io::ErrorKind::NotFound { OpenError::NotFound } else { OpenError::Io }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// `\\?\C:\...` or `\\?\UNC\server\share\...` for a normalized path: no
    /// further parsing by Win32, no length limit.
    fn verbatim(norm: &str) -> Vec<u16> {
        match norm.strip_prefix("\\\\") {
            Some(unc) => wide(&format!("\\\\?\\UNC\\{unc}")),
            None => wide(&format!("\\\\?\\{norm}")),
        }
    }

    /// Opens a full path. Links on the way, and at the end, are followed.
    fn open_path(norm: &str, access: u32, disposition: u32, flags: u32) -> io::Result<OwnedHandle> {
        let path = verbatim(norm);
        // SAFETY: `path` is NUL-terminated and outlives the call; no security attributes or template.
        let h = unsafe { CreateFileW(path.as_ptr(), access, SHARE, null(), disposition, flags, null_mut()) };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a valid handle that nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(h) })
    }

    /// Opens one name in the folder `parent`, relative to that handle. The name
    /// comes from a normalized path: no separator, stream, `.` or `..`.
    fn open_at(parent: &OwnedHandle, name: &str, access: u32, disposition: u32, options: u32) -> io::Result<OwnedHandle> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let bytes = u16::try_from(name.len() * 2).map_err(|_| io::Error::from(io::ErrorKind::InvalidFilename))?;
        let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: name.as_ptr().cast_mut() };
        let oa = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle(),
            ObjectName: &us,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: null(),
            SecurityQualityOfService: null(),
        };
        // SAFETY: plain data, all-zero is a valid value.
        let mut iosb: IO_STATUS_BLOCK = unsafe { zeroed() };
        let mut h: HANDLE = null_mut();
        // SAFETY: every pointer is valid for the call; `name` and `us` outlive it.
        let status = unsafe {
            NtCreateFile(&mut h, access, &oa, &mut iosb, null(), FILE_ATTRIBUTE_NORMAL, SHARE, disposition, options | FILE_SYNCHRONOUS_IO_NONALERT, null(), 0)
        };
        if status < 0 {
            // SAFETY: a pure conversion.
            return Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32));
        }
        // SAFETY: a valid handle that nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(h) })
    }

    enum Reparse {
        None,
        /// A symbolic link, junction or mount point.
        Link,
        /// Anything else (cloud file, dedup, ...): not a name of another file.
        Other,
    }

    fn reparse(h: &OwnedHandle) -> Result<Reparse, OpenError> {
        let mut info = FILE_ATTRIBUTE_TAG_INFO { FileAttributes: 0, ReparseTag: 0 };
        // SAFETY: the buffer is the size the class needs.
        let ok = unsafe { GetFileInformationByHandleEx(h.as_raw_handle(), FileAttributeTagInfo, (&raw mut info).cast(), size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32) };
        if ok == 0 {
            return Err(OpenError::Io);
        }
        Ok(if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            Reparse::None
        } else if info.ReparseTag & NAME_SURROGATE != 0 {
            Reparse::Link
        } else {
            Reparse::Other
        })
    }

    /// One component below the root: opened without following a link, and refused if it is one.
    fn open_component(parent: &OwnedHandle, name: &str, access: u32, disposition: u32, options: u32) -> Result<OwnedHandle, OpenError> {
        let h = open_at(parent, name, access, disposition, options | FILE_OPEN_REPARSE_POINT).map_err(from_io)?;
        match reparse(&h)? {
            Reparse::None => Ok(h),
            Reparse::Link => Err(OpenError::Denied),
            Reparse::Other => {
                // Let the file system filter (cloud files, dedup) do its work. The name
                // may have been swapped for a link in between: the second handle must
                // be the same file as the first (and the final path check still runs).
                let first = file_id(&h)?;
                drop(h);
                let again = open_at(parent, name, access, FILE_OPEN, options).map_err(from_io)?;
                if file_id(&again)? != first {
                    return Err(OpenError::Denied);
                }
                Ok(again)
            }
        }
    }

    fn info(h: &OwnedHandle) -> Result<BY_HANDLE_FILE_INFORMATION, OpenError> {
        // SAFETY: plain data, all-zero is a valid value.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
        // SAFETY: `info` is the structure the call fills.
        if unsafe { GetFileInformationByHandle(h.as_raw_handle(), &mut info) } == 0 {
            return Err(OpenError::Io);
        }
        Ok(info)
    }

    /// Volume serial number and file index: which file a handle is.
    fn file_id(h: &OwnedHandle) -> Result<(u32, u32, u32), OpenError> {
        let i = info(h)?;
        Ok((i.dwVolumeSerialNumber, i.nFileIndexHigh, i.nFileIndexLow))
    }

    /// The handle's final path as a [`caps::key`]. A path that does not normalize
    /// (a volume with no drive letter, a device, a name Win32 can not spell) is refused.
    fn final_key(h: &OwnedHandle) -> Result<String, OpenError> {
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: `buf` is valid for `buf.len()` units.
            let n = unsafe { GetFinalPathNameByHandleW(h.as_raw_handle(), buf.as_mut_ptr(), buf.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS) } as usize;
            if n == 0 {
                return Err(OpenError::Io);
            }
            if n < buf.len() {
                buf.truncate(n);
                break;
            }
            if n > 1 << 16 {
                return Err(OpenError::Io);
            }
            // `n` is the size needed, NUL included.
            buf.resize(n + 1, 0);
        }
        let s = String::from_utf16(&buf).map_err(|_| OpenError::Denied)?;
        caps::normalize(&s).map(|n| caps::key(&n)).map_err(|_| OpenError::Denied)
    }

    fn links(h: &OwnedHandle) -> Result<u32, OpenError> {
        Ok(info(h)?.nNumberOfLinks)
    }

    /// Removes a file this module just created, through its handle (opened with DELETE).
    fn delete(h: &OwnedHandle) {
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: the buffer is the size the class needs. A failure leaves an empty file.
        unsafe { SetFileInformationByHandle(h.as_raw_handle(), FileDispositionInfo, (&raw const info).cast(), size_of::<FILE_DISPOSITION_INFO>() as u32) };
    }

    fn is_remote_drive(norm: &str) -> bool {
        let b = norm.as_bytes();
        if b.len() < 3 || b[1] != b':' {
            return false;
        }
        let root = wide(&norm[..3]);
        // SAFETY: `root` is NUL-terminated.
        unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
    }

    pub(super) fn open_under(norm: &str, k: &str, root: &Root, network: bool, want: Want) -> Result<File, OpenError> {
        let (root_path, remote_ok, folder) = match root {
            // A drive path (the policy never gives `*` a UNC path); its drive root is the folder.
            Root::Any if !caps::is_unc(norm) && norm.len() >= 3 => (&norm[..3], false, false),
            Root::Any => return Err(OpenError::Denied),
            Root::Dir(d) => (d.as_str(), network, true),
            Root::Input(d) => (d.as_str(), true, true),
        };
        let rk = caps::key(root_path);
        if !caps::is_under(k, &rk) {
            return Err(OpenError::Denied);
        }
        if !remote_ok && (caps::is_unc(root_path) || is_remote_drive(root_path)) {
            return Err(OpenError::Denied);
        }
        let local = |fin: &str| if !remote_ok && caps::is_unc(fin) { Err(OpenError::Denied) } else { Ok(()) };

        if rk == k {
            // Exactly the path the grant names: opened as given.
            let h = match want {
                Want::Stat | Want::List => open_path(norm, want.access(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS),
                Want::Read => open_path(norm, want.access(), OPEN_EXISTING, 0),
                Want::Write { .. } => open_path(norm, want.access(), OPEN_ALWAYS, 0),
            }
            .map_err(from_io)?;
            local(&final_key(&h)?)?;
            return Ok(File::from(h));
        }

        let mut parent = open_path(root_path, TRAVERSE, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS).map_err(from_io)?;
        let root_real = final_key(&parent)?;
        local(&root_real)?;
        // `norm` starts with `root_path` (same key, and keys keep the byte length).
        let rest = norm.get(root_path.len()..).ok_or(OpenError::Denied)?.trim_start_matches('\\');
        let mut names = rest.split('\\');
        let leaf = names.next_back().filter(|n| !n.is_empty()).ok_or(OpenError::Denied)?;
        for name in names {
            parent = open_component(&parent, name, TRAVERSE, FILE_OPEN, FILE_DIRECTORY_FILE)?;
        }
        let (h, created) = match open_component(&parent, leaf, want.access(), FILE_OPEN, want.options()) {
            Ok(h) => (h, false),
            Err(OpenError::NotFound) if matches!(want, Want::Write { .. }) => (open_component(&parent, leaf, want.access() | DELETE, FILE_CREATE, want.options())?, true),
            Err(e) => return Err(e),
        };
        let verdict = (|| {
            let real = final_key(&h)?;
            local(&real)?;
            // The folder chain can still be moved while it is open: the handle says where it ended up.
            if !caps::is_under(&real, &root_real) {
                return Err(OpenError::Denied);
            }
            if folder && want != Want::List && links(&h)? > 1 {
                return Err(OpenError::Denied);
            }
            Ok(())
        })();
        if let Err(e) = verdict {
            if created {
                delete(&h);
            }
            return Err(e);
        }
        Ok(File::from(h))
    }

    pub(super) fn read_dir(dir: &File, skip: usize, max: usize) -> Result<Vec<Entry>, OpenError> {
        // u64 for the alignment the entries need.
        let mut buf = vec![0u64; 8 * 1024];
        let bytes = buf.len() * 8;
        let mut class = FileFullDirectoryRestartInfo;
        let (mut seen, mut out) = (0usize, Vec::new());
        let name_at = offset_of!(FILE_FULL_DIR_INFO, FileName);
        while out.len() < max {
            // SAFETY: `buf` is valid for `bytes` bytes.
            let ok = unsafe { GetFileInformationByHandleEx(dir.as_raw_handle(), class, buf.as_mut_ptr().cast(), bytes as u32) };
            if ok == 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    break;
                }
                return Err(from_io(e));
            }
            class = FileFullDirectoryInfo;
            let base = buf.as_ptr().cast::<u8>();
            let mut off = 0usize;
            loop {
                if off + name_at > bytes {
                    return Err(OpenError::Io);
                }
                // SAFETY: in bounds (checked above); read unaligned to be independent of the layout.
                let e: FILE_FULL_DIR_INFO = unsafe { base.add(off).cast::<FILE_FULL_DIR_INFO>().read_unaligned() };
                let len = e.FileNameLength as usize;
                if off + name_at + len > bytes {
                    return Err(OpenError::Io);
                }
                // SAFETY: in bounds (checked above).
                let raw = unsafe { std::slice::from_raw_parts(base.add(off + name_at), len) };
                let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                let is_dot = units == [b'.' as u16] || units == [b'.' as u16, b'.' as u16];
                if !is_dot {
                    if seen >= skip && out.len() < max {
                        // For a reparse point `EaSize` holds the tag. A link to a folder is not a folder (as in `std`).
                        let link = e.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && e.EaSize & NAME_SURROGATE != 0;
                        out.push(Entry {
                            name: String::from_utf16_lossy(&units),
                            dir: e.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 && !link,
                            size: e.EndOfFile.max(0) as u64,
                            mtime: e.LastWriteTime.max(0) as u64,
                        });
                    }
                    seen += 1;
                }
                if e.NextEntryOffset == 0 {
                    break;
                }
                off += e.NextEntryOffset as usize;
            }
        }
        Ok(out)
    }
}

#[cfg(not(windows))]
mod imp {
    //! The host only runs on Windows. Elsewhere every file import is denied.
    use super::{Entry, OpenError, Want};
    use crate::caps::Root;
    use std::fs::File;

    pub(super) fn open_under(_: &str, _: &str, _: &Root, _: bool, _: Want) -> Result<File, OpenError> {
        Err(OpenError::Denied)
    }

    pub(super) fn read_dir(_: &File, _: usize, _: usize) -> Result<Vec<Entry>, OpenError> {
        Err(OpenError::Io)
    }
}
