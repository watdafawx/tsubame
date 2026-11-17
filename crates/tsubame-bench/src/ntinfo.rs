//! Parsers for the NT directory-information layouts used by
//! `NtQueryDirectoryFile` and `GetFileInformationByHandleEx`.
//!
//! `FILE_ID_EXTD_DIR_INFORMATION` / `FILE_ID_EXTD_DIR_INFO` share one layout:
//!
//! ```text
//!  off  size  field
//!    0     4  NextEntryOffset
//!    4     4  FileIndex
//!    8     8  CreationTime
//!   16     8  LastAccessTime
//!   24     8  LastWriteTime
//!   32     8  ChangeTime
//!   40     8  EndOfFile
//!   48     8  AllocationSize
//!   56     4  FileAttributes
//!   60     4  FileNameLength        (bytes)
//!   64     4  EaSize
//!   68     4  ReparsePointTag
//!   72    16  FileId (FILE_ID_128)
//!   88     -  FileName (UTF-16, no terminator)
//! ```
//!
//! `FILE_FULL_DIR_INFORMATION` (class 2) is the same header without the reparse
//! tag and file id: name starts at 68. Both need 8-byte alignment, which is why
//! callers allocate the query buffer as `Vec<u64>`.

use crate::store::EntryStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// FILE_ID_EXTD_DIR_INFORMATION, 88-byte header, has file id.
    Extd,
    /// FILE_FULL_DIR_INFORMATION, 68-byte header, no file id.
    Full,
}

impl Layout {
    pub const fn header_len(self) -> usize {
        match self {
            Layout::Extd => 88,
            Layout::Full => 68,
        }
    }
}

/// FILE_INFORMATION_CLASS values.
pub const FILE_FULL_DIRECTORY_INFORMATION: u32 = 2;
pub const FILE_ID_EXTD_DIRECTORY_INFORMATION: u32 = 60;

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

/// Walk one query buffer, appending every entry to `store`.
///
/// Returns the number of records parsed. `buf` must be 8-byte aligned (callers
/// use a `Vec<u64>` buffer), so the UTF-16 names are 2-byte aligned.
pub fn parse_buffer(buf: &[u8], layout: Layout, store: &mut EntryStore) -> Result<usize, String> {
    debug_assert_eq!(buf.as_ptr() as usize % 8, 0, "query buffer must be 8-byte aligned");
    let hdr = layout.header_len();
    let mut off = 0usize;
    let mut parsed = 0usize;
    loop {
        if off + hdr > buf.len() {
            return Err(format!(
                "truncated header at offset {off} (buffer {} bytes, header {hdr})",
                buf.len()
            ));
        }
        let next = u32_at(buf, off) as usize;
        let name_len = u32_at(buf, off + 60) as usize;
        if !name_len.is_multiple_of(2) {
            return Err(format!("odd FileNameLength {name_len} at offset {off}"));
        }
        let name_start = off + hdr;
        let name_end = name_start + name_len;
        if name_end > buf.len() {
            return Err(format!(
                "name overruns buffer at offset {off}: {name_start}..{name_end} > {}",
                buf.len()
            ));
        }
        let name: &[u16] = unsafe {
            core::slice::from_raw_parts(buf.as_ptr().add(name_start) as *const u16, name_len / 2)
        };
        let attrs = u32_at(buf, off + 56);
        let size = u64_at(buf, off + 40);
        let mtime = u64_at(buf, off + 24) as i64;
        let (reparse_tag, file_id) = match layout {
            Layout::Extd => {
                let tag = u32_at(buf, off + 68);
                let mut id = [0u8; 16];
                id.copy_from_slice(&buf[off + 72..off + 88]);
                (tag, id)
            }
            Layout::Full => (0, [0u8; 16]),
        };
        store.push(name, attrs, size, mtime, reparse_tag, file_id);
        parsed += 1;
        if next == 0 {
            break;
        }
        if next < hdr {
            return Err(format!("bogus NextEntryOffset {next} at offset {off}"));
        }
        off += next;
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_extd(name: &str, attrs: u32, size: u64, mtime: u64, tag: u32, id: [u8; 16]) -> Vec<u64> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let total = 88 + name.len() * 2;
        let words = total.div_ceil(8);
        let mut buf = vec![0u64; words];
        let bytes: &mut [u8] =
            unsafe { core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, words * 8) };
        bytes[0..4].copy_from_slice(&0u32.to_ne_bytes()); // NextEntryOffset
        bytes[24..32].copy_from_slice(&mtime.to_ne_bytes());
        bytes[40..48].copy_from_slice(&size.to_ne_bytes());
        bytes[56..60].copy_from_slice(&attrs.to_ne_bytes());
        bytes[60..64].copy_from_slice(&((name.len() * 2) as u32).to_ne_bytes());
        bytes[68..72].copy_from_slice(&tag.to_ne_bytes());
        bytes[72..88].copy_from_slice(&id);
        for (i, u) in name.iter().enumerate() {
            bytes[88 + i * 2..90 + i * 2].copy_from_slice(&u.to_ne_bytes());
        }
        buf
    }

    #[test]
    fn parses_extd_layout() {
        let mut id = [0u8; 16];
        id[0] = 0xAB;
        let buf = build_extd("hello.txt", 0x20, 1234, 999, 0, id);
        let bytes: &[u8] =
            unsafe { core::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 8) };
        let mut store = EntryStore::new();
        let n = parse_buffer(bytes, Layout::Extd, &mut store).unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.len(), 1);
        assert_eq!(store.name(0), &"hello.txt".encode_utf16().collect::<Vec<_>>()[..]);
        assert_eq!(store.recs[0].size, 1234);
        assert_eq!(store.recs[0].mtime, 999);
        assert_eq!(store.recs[0].attrs, 0x20);
        assert_eq!(store.recs[0].file_id, id);
    }

    #[test]
    fn parses_full_layout() {
        // Same header without id: name at 68.
        let name: Vec<u16> = "a.rs".encode_utf16().collect();
        let total = 68 + name.len() * 2;
        let words = total.div_ceil(8);
        let mut buf = vec![0u64; words];
        let bytes: &mut [u8] =
            unsafe { core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, words * 8) };
        bytes[56..60].copy_from_slice(&0x20u32.to_ne_bytes());
        bytes[60..64].copy_from_slice(&((name.len() * 2) as u32).to_ne_bytes());
        for (i, u) in name.iter().enumerate() {
            bytes[68 + i * 2..70 + i * 2].copy_from_slice(&u.to_ne_bytes());
        }
        let mut store = EntryStore::new();
        let n = parse_buffer(bytes, Layout::Full, &mut store).unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.name(0), &name[..]);
        assert_eq!(store.recs[0].file_id, [0u8; 16]);
    }
}
