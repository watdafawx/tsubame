//! Hex previews: the first [`HEX_CAP`] bytes, a magic-header label, and the
//! pure row formatter the UI draws.
//!
//! Rows are 16 bytes wide in Quick Look and in docked panes at least
//! [`NARROW_PANE_MAX_PX`] wide, and 8 bytes in narrower panes
//! (`docs/preview-mock.md` #7 and open question 1). The formatter is pure so it
//! is unit-tested without a window.

use super::PreviewError;
use std::io::Read;
use std::path::Path;

/// Bytes read into a hex preview (`docs/preview-mock.md`: 16 KB).
pub const HEX_CAP: usize = 16_384;

/// Classic row width.
pub const HEX_WIDE_BYTES_PER_ROW: usize = 16;

/// Row width used in a narrow docked pane so ASCII stays visible.
pub const HEX_NARROW_BYTES_PER_ROW: usize = 8;

/// Docked panes narrower than this switch to 8 bytes/row when the setting is
/// on.
pub const NARROW_PANE_MAX_PX: u32 = 420;

/// The loaded hex dump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewHex {
    /// The first [`HEX_CAP`] bytes.
    pub bytes: Vec<u8>,
    /// The file's real size.
    pub total_bytes: u64,
    /// The file is larger than [`HEX_CAP`]: show the truncation banner.
    pub truncated: bool,
    /// A recognised header (`PE/MZ`, `PNG`, `ZIP`, ...).
    pub magic: Option<String>,
}

/// One formatted hex row: `00000000 | 4D 5A 90 00 ... | MZ..`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexRow {
    /// Eight upper-case hex digits of the byte offset.
    pub offset: String,
    /// Upper-case bytes separated by single spaces, padded so every row's
    /// ASCII column starts at the same place.
    pub hex: String,
    /// Printable ASCII, `.` for everything else (dimming `00` is the UI's job).
    pub ascii: String,
    /// Bytes on this row (less than the row width on the last one).
    pub len: usize,
}

/// Bytes per row for a pane `width_px` wide.
///
/// `auto_width` is the `[preview] hex_auto_width` setting: off always means 16.
pub fn bytes_per_row(width_px: u32, auto_width: bool) -> usize {
    if auto_width && width_px < NARROW_PANE_MAX_PX {
        HEX_NARROW_BYTES_PER_ROW
    } else {
        HEX_WIDE_BYTES_PER_ROW
    }
}

/// Formats one row at `offset`. `per_row` is clamped to 1..=64.
pub fn format_row(offset: u64, bytes: &[u8], per_row: usize) -> HexRow {
    let per_row = per_row.clamp(1, 64);
    let bytes = &bytes[..bytes.len().min(per_row)];
    let mut hex = String::with_capacity(per_row * 3);
    let mut ascii = String::with_capacity(bytes.len());
    for &b in bytes {
        if !hex.is_empty() {
            hex.push(' ');
        }
        hex.push_str(&format!("{b:02X}"));
        ascii.push(if (0x20..=0x7e).contains(&b) { b as char } else { '.' });
    }
    // Pad short rows so the ASCII column lines up.
    let width = per_row * 3 - 1;
    while hex.len() < width {
        hex.push(' ');
    }
    HexRow { offset: format!("{offset:08X}"), hex, ascii, len: bytes.len() }
}

/// Every row of `bytes` at `per_row` bytes each.
pub fn rows(bytes: &[u8], per_row: usize) -> Vec<HexRow> {
    let per_row = per_row.clamp(1, 64);
    bytes
        .chunks(per_row)
        .enumerate()
        .map(|(n, chunk)| format_row((n * per_row) as u64, chunk, per_row))
        .collect()
}

/// Label for a recognised file header (the mock's badge above the dump).
pub fn magic(bytes: &[u8]) -> Option<&'static str> {
    const SQLITE: &[u8] = b"SQLite format 3\0";
    if bytes.starts_with(b"MZ") {
        Some("PE/MZ")
    } else if bytes.starts_with(b"\x7fELF") {
        Some("ELF")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("PNG")
    } else if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") || bytes.starts_with(b"PK\x07\x08") {
        Some("ZIP")
    } else if bytes.starts_with(b"7z\xbc\xaf\x27\x1c") {
        Some("7z")
    } else if bytes.starts_with(b"%PDF") {
        Some("PDF")
    } else if bytes.starts_with(SQLITE) {
        Some("SQLite")
    } else if bytes.starts_with(b"\0asm") {
        Some("WASM")
    } else {
        None
    }
}

/// Reads the first [`HEX_CAP`] bytes of `path`.
pub fn load(path: &Path) -> Result<PreviewHex, PreviewError> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PreviewError::NotFound(path.display().to_string()),
        std::io::ErrorKind::PermissionDenied => PreviewError::AccessDenied(path.display().to_string()),
        _ => PreviewError::Io(format!("{}: {e}", path.display())),
    })?;
    let file = std::fs::File::open(path).map_err(|e| PreviewError::Io(format!("{}: {e}", path.display())))?;
    let mut bytes = Vec::with_capacity(HEX_CAP);
    file.take(HEX_CAP as u64).read_to_end(&mut bytes).map_err(|e| PreviewError::Io(format!("{}: {e}", path.display())))?;
    let total_bytes = meta.len();
    Ok(PreviewHex {
        magic: magic(&bytes).map(str::to_string),
        truncated: total_bytes > HEX_CAP as u64,
        bytes,
        total_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-preview-hex-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn row_width_follows_the_pane_width() {
        assert_eq!(bytes_per_row(320, true), 8, "narrow docked pane");
        assert_eq!(bytes_per_row(419, true), 8);
        assert_eq!(bytes_per_row(420, true), 16, "420 px and wider");
        assert_eq!(bytes_per_row(600, true), 16);
        assert_eq!(bytes_per_row(320, false), 16, "auto width off keeps 16");
        assert_eq!(NARROW_PANE_MAX_PX, 420);
    }

    #[test]
    fn rows_format_like_the_mock() {
        let data: Vec<u8> = (0x4d..0x4d + 16).collect();
        let row = format_row(0, &data, HEX_WIDE_BYTES_PER_ROW);
        assert_eq!(row.offset, "00000000");
        assert_eq!(row.hex, "4D 4E 4F 50 51 52 53 54 55 56 57 58 59 5A 5B 5C");
        assert_eq!(row.ascii, "MNOPQRSTUVWXYZ[\\");
        assert_eq!(row.len, 16);

        let eight = format_row(0x10, &data[..8], HEX_NARROW_BYTES_PER_ROW);
        assert_eq!(eight.offset, "00000010");
        assert_eq!(eight.ascii, "MNOPQRST");
        assert_eq!(eight.hex.len(), 8 * 3 - 1);
    }

    #[test]
    fn short_rows_are_padded_and_non_printable_bytes_become_dots() {
        let row = format_row(0, &[0x00, 0x41, 0x1f, 0x7f], 8);
        assert_eq!(row.len, 4);
        assert_eq!(row.hex.len(), 8 * 3 - 1, "ASCII column stays aligned");
        assert_eq!(row.ascii, ".A..");
        assert!(row.hex.starts_with("00 41 1F 7F"));
        let empty = format_row(0, &[], 16);
        assert_eq!(empty.len, 0);
        assert_eq!(empty.ascii, "");
    }

    #[test]
    fn all_rows_tile_the_bytes() {
        let data: Vec<u8> = (0..40).collect();
        let out = rows(&data, 16);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2].len, 8);
        assert_eq!(out[1].offset, "00000010");
        assert_eq!(out.iter().map(|r| r.len).sum::<usize>(), 40);
    }

    #[test]
    fn magic_headers_are_recognised() {
        let cases: &[(&[u8], &str)] = &[
            (b"MZ\x90\x00", "PE/MZ"),
            (b"\x7fELF\x02", "ELF"),
            (b"\x89PNG\r\n\x1a\n", "PNG"),
            (b"PK\x03\x04", "ZIP"),
            (b"PK\x05\x06", "ZIP"),
            (b"7z\xbc\xaf\x27\x1c", "7z"),
            (b"%PDF-1.7", "PDF"),
            (b"SQLite format 3\0", "SQLite"),
            (b"\0asm\x01\0\0\0", "WASM"),
        ];
        for (bytes, want) in cases {
            assert_eq!(magic(bytes), Some(*want), "{want}");
        }
        assert_eq!(magic(b"hello"), None);
        assert_eq!(magic(&[]), None);
    }

    #[test]
    fn the_16_kib_cap_truncates_and_reports_the_real_size() {
        let d = temp_dir("cap");
        let p = d.join("big.bin");
        let body = vec![0x5au8; HEX_CAP * 3];
        std::fs::write(&p, &body).unwrap();
        let h = load(&p).unwrap();
        assert_eq!(h.bytes.len(), HEX_CAP);
        assert_eq!(h.total_bytes, body.len() as u64);
        assert!(h.truncated);
        assert_eq!(h.magic, None);

        let small = d.join("small.bin");
        std::fs::write(&small, b"MZ\x90\x00").unwrap();
        let h = load(&small).unwrap();
        assert_eq!(h.total_bytes, 4);
        assert!(!h.truncated);
        assert_eq!(h.magic.as_deref(), Some("PE/MZ"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
