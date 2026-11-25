//! Row decoding for drawing, and the formats of the details columns.
//!
//! Entries live in `tsubame_core::EntryStore` (one UTF-16 name buffer plus
//! 24-byte records). The list asks for the rows it draws; `RowBuf::fill`
//! decodes one into a reused buffer, no allocation per row.
//! `synthetic_store` is the bench's fake folder, built through the same
//! `push_batch` path a real listing uses.

use tsubame_core::{EntryId, EntryInput, EntryStore, FOLDER, HIDDEN, READONLY, SYSTEM};

/// (extension, type column text). Index 0 is the folder kind.
pub const KINDS: [(&str, &str); 12] = [
    ("", "File folder"),
    ("txt", "Text document"),
    ("png", "PNG image"),
    ("rs", "Rust source file"),
    ("pdf", "PDF document"),
    ("zip", "Compressed archive"),
    ("exe", "Application"),
    ("mp4", "MP4 video"),
    ("dll", "Application extension"),
    ("json", "JSON file"),
    ("jpg", "JPEG image"),
    ("md", "Markdown document"),
];

/// `RowBuf::kind` of a file whose extension is not in `KINDS`.
pub const KIND_OTHER: u8 = 255;

/// Win32 attribute bits the icon cache looks at.
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_READONLY: u32 = 0x1;

thread_local! {
    /// `[view] show_extensions` for the UI thread (the views draw names through `RowBuf::label`).
    static SHOW_EXT: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// `[view] show_extensions`: off draws file names without their extension.
pub fn set_show_extensions(on: bool) {
    SHOW_EXT.with(|c| c.set(on));
}

pub fn show_extensions() -> bool {
    SHOW_EXT.with(|c| c.get())
}

/// One row, decoded and ready to draw. Reused for every row of a frame.
pub struct RowBuf {
    /// UTF-8; names reach 255 UTF-16 units (up to 765 bytes)
    pub name: [u8; 768],
    pub name_len: usize,
    pub size: u64,
    /// unix seconds
    pub mtime: u32,
    /// index into `KINDS`, or `KIND_OTHER`
    pub kind: u8,
    /// Win32 attributes rebuilt from the store flags (read-only/system/hidden)
    pub attrs: u32,
    pub hidden: bool,
}

impl RowBuf {
    pub const fn new() -> Self {
        RowBuf { name: [0; 768], name_len: 0, size: 0, mtime: 0, kind: 0, attrs: 0, hidden: false }
    }
    pub fn name_str(&self) -> &str {
        std::str::from_utf8(&self.name[..self.name_len]).unwrap_or("")
    }
    pub fn is_dir(&self) -> bool {
        self.kind == 0
    }
    /// ".txt" as written (not lowercased), or "" when there is none.
    pub fn ext(&self) -> &str {
        let n = self.name_str();
        match n.rfind('.') {
            Some(i) if i > 0 && !self.is_dir() => &n[i..],
            _ => "",
        }
    }
    /// The name as drawn: without the extension of a file when extensions are
    /// hidden (folders and dot-files keep theirs). Everything that is not
    /// drawing (rename, sort, open, tags, colours) keeps using `name_str`.
    pub fn label(&self) -> &str {
        let n = self.name_str();
        if show_extensions() {
            return n;
        }
        &n[..n.len() - self.ext().len()]
    }
    /// Type column text into `out`.
    pub fn type_text(&self, out: &mut String) {
        out.clear();
        if self.kind != KIND_OTHER {
            out.push_str(KINDS[self.kind as usize].1);
            return;
        }
        let e = self.ext();
        if e.is_empty() {
            out.push_str("File");
        } else {
            out.extend(e[1..].chars().map(|c| c.to_ascii_uppercase()));
            out.push_str(" File");
        }
    }

    /// Decodes entry `id` of `store` into this buffer.
    pub fn fill(&mut self, store: &EntryStore, id: EntryId) {
        let (Some(rec), Some(name)) = (store.record(id), store.name(id)) else {
            self.name_len = 0;
            self.kind = KIND_OTHER;
            return;
        };
        // UTF-16 to UTF-8 straight into the fixed buffer
        let mut n = 0;
        for ch in char::decode_utf16(name.iter().copied()) {
            let c = ch.unwrap_or('\u{fffd}');
            if n + c.len_utf8() > self.name.len() {
                break;
            }
            n += c.encode_utf8(&mut self.name[n..]).len();
        }
        self.name_len = n;
        let dir = rec.flags & FOLDER != 0;
        self.size = if dir { 0 } else { rec.size };
        self.mtime = filetime_to_unix(rec.modified);
        self.kind = kind_of(self.name_str(), dir);
        self.attrs = (if rec.flags & READONLY != 0 { ATTR_READONLY } else { 0 }) | (if rec.flags & SYSTEM != 0 { ATTR_SYSTEM } else { 0 }) | (if rec.flags & HIDDEN != 0 { ATTR_HIDDEN } else { 0 });
        self.hidden = rec.flags & HIDDEN != 0;
    }
}

/// FILETIME (100 ns since 1601) to unix seconds.
pub fn filetime_to_unix(ft: i64) -> u32 {
    ((ft.max(0) as u64) / 10_000_000).saturating_sub(11_644_473_600).min(u32::MAX as u64) as u32
}

pub fn kind_of(name: &str, dir: bool) -> u8 {
    if dir {
        return 0;
    }
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => return KIND_OTHER,
    };
    KINDS.iter().skip(1).position(|(e, _)| e.eq_ignore_ascii_case(ext)).map(|p| p as u8 + 1).unwrap_or(KIND_OTHER)
}

/// Mock format: "812 B", "34 KB", "1.5 MB" under 10 MB, "88 MB", "1.25 GB".
pub fn fmt_size(size: u64, out: &mut String) {
    use std::fmt::Write;
    out.clear();
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if size < KB {
        let _ = write!(out, "{size} B");
    } else if size < MB {
        let _ = write!(out, "{} KB", (size as f64 / KB as f64).round() as u64);
    } else if size < GB {
        let v = size as f64 / MB as f64;
        if size < 10 * MB {
            let s = format!("{v:.1}");
            let _ = write!(out, "{} MB", s.strip_suffix(".0").unwrap_or(&s));
        } else {
            let _ = write!(out, "{} MB", v.round() as u64);
        }
    } else {
        let _ = write!(out, "{:.2} GB", size as f64 / GB as f64);
    }
}

/// "2025-03-14 09:41" in local time (DST of that date) from unix seconds UTC,
/// into a reused buffer. Sorting keeps using the UTC value.
pub fn fmt_date(secs: u32, out: &mut String) {
    fmt_civil(tsubame_win::tz::local_secs(secs), out);
}

/// "2025-03-14 09:41" of wall-clock seconds (no zone applied).
pub fn fmt_civil(secs: u32, out: &mut String) {
    use std::fmt::Write;
    out.clear();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let _ = write!(out, "{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60);
}

/// "118 GB" / "1.1 TB" for volume sizes.
pub fn fmt_volume(bytes: u64, out: &mut String) {
    use std::fmt::Write;
    out.clear();
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let v = bytes as f64 / GB;
    if v >= 1024.0 {
        let _ = write!(out, "{:.1} TB", v / 1024.0);
    } else {
        let _ = write!(out, "{:.0} GB", v);
    }
}

// ---- synthetic folder for `--bench` ------------------------------------

const WORDS: [&str; 16] = [
    "report", "photo", "backup", "draft", "invoice", "holiday", "project", "notes", "design", "archive", "render", "budget", "screen", "export", "sample", "build",
];

/// 2018-01-01 00:00:00 UTC, unix seconds.
const DATE_BASE: u64 = 1_514_764_800;
const DATE_SPAN: u64 = 8 * 365 * 86_400;

fn mix(mut x: u64) -> u64 {
    // splitmix64
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// `n` fake entries (a ninth are folders) pushed in batches of 4096 through
/// `push_batch`, then sorted by name, folders first. Deterministic.
pub fn synthetic_store(n: usize) -> EntryStore {
    let mut store = EntryStore::with_capacity(n, n * 28);
    store.sort(tsubame_core::Sort { column: tsubame_core::Column::Name, descending: false, folders_first: true });
    let gen_id = store.generation();
    let mut names: Vec<u16> = Vec::new();
    let mut spans: Vec<(usize, usize, u64, i64, u16)> = Vec::new();
    let mut i = 0;
    while i < n {
        names.clear();
        spans.clear();
        let end = (i + 4096).min(n);
        for k in i..end {
            let h = mix(k as u64);
            let dir = h.is_multiple_of(9);
            let kind = 1 + ((h >> 20) % (KINDS.len() as u64 - 1)) as usize;
            let s = if dir {
                format!("{}-{}-{k}", WORDS[((h >> 8) & 15) as usize], WORDS[((h >> 12) & 15) as usize])
            } else {
                format!("{}-{}-{k}.{}", WORDS[((h >> 8) & 15) as usize], WORDS[((h >> 12) & 15) as usize], KINDS[kind].0)
            };
            let start = names.len();
            names.extend(s.encode_utf16());
            let size = if dir { 0 } else { ((h >> 24) & 0xFFFF) << ((h >> 40) % 14) };
            let unix = DATE_BASE + (h >> 11) % DATE_SPAN;
            let ft = ((unix + 11_644_473_600) * 10_000_000) as i64;
            spans.push((start, names.len(), size, ft, if dir { FOLDER } else { 0 }));
        }
        let batch: Vec<EntryInput> = spans.iter().map(|&(a, b, size, modified, flags)| EntryInput { name: &names[a..b], size, modified, flags }).collect();
        store.push_batch(gen_id, &batch).expect("synthetic batch");
        i = end;
    }
    store
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_format() {
        let mut s = String::new();
        fmt_civil(DATE_BASE as u32, &mut s);
        assert_eq!(s, "2018-01-01 00:00");
        fmt_civil(1_741_945_260, &mut s);
        assert_eq!(s, "2025-03-14 09:41");
    }

    #[test]
    fn size_format_matches_the_mock() {
        let f = |v: u64| {
            let mut s = String::new();
            fmt_size(v, &mut s);
            s
        };
        assert_eq!(f(7), "7 B");
        assert_eq!(f(1023), "1023 B");
        assert_eq!(f(1536), "2 KB");
        assert_eq!(f(34 * 1024), "34 KB");
        assert_eq!(f(1_572_864), "1.5 MB");
        assert_eq!(f(2 * 1024 * 1024), "2 MB");
        assert_eq!(f(88 * 1024 * 1024 + 300_000), "88 MB");
        assert_eq!(f(1_342_177_280), "1.25 GB");
        let mut s = String::new();
        fmt_volume(118 * 1024 * 1024 * 1024, &mut s);
        assert_eq!(s, "118 GB");
        fmt_volume(1_200_000_000_000, &mut s);
        assert_eq!(s, "1.1 TB");
    }

    #[test]
    fn kinds_and_type_text() {
        assert_eq!(kind_of("a.TXT", false), 1);
        assert_eq!(kind_of("folder.txt", true), 0);
        assert_eq!(kind_of("notepad.exe", false), 6);
        assert_eq!(kind_of("x.weird", false), KIND_OTHER);
        assert_eq!(kind_of(".hidden", false), KIND_OTHER);
        let mut b = RowBuf::new();
        b.name[..7].copy_from_slice(b"a.weird");
        b.name_len = 7;
        b.kind = KIND_OTHER;
        let mut s = String::new();
        b.type_text(&mut s);
        assert_eq!(s, "WEIRD File");
        assert_eq!(b.ext(), ".weird");
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), 0);
    }

    #[test]
    fn fill_decodes_a_store_entry() {
        let mut s = EntryStore::new();
        let n1: Vec<u16> = "Ünïcode 日本.txt".encode_utf16().collect();
        let n2: Vec<u16> = "Sub".encode_utf16().collect();
        let ft = ((1_741_945_260u64 + 11_644_473_600) * 10_000_000) as i64;
        s.push_batch(0, &[EntryInput { name: &n1, size: 5, modified: ft, flags: READONLY }, EntryInput { name: &n2, size: 99, modified: ft, flags: FOLDER | HIDDEN }]).unwrap();
        let mut r = RowBuf::new();
        r.fill(&s, 0);
        assert_eq!(r.name_str(), "Ünïcode 日本.txt");
        assert_eq!((r.size, r.kind, r.mtime, r.attrs), (5, 1, 1_741_945_260, ATTR_READONLY));
        r.fill(&s, 1);
        assert!(r.is_dir() && r.hidden);
        assert_eq!(r.size, 0, "folders show no size");
    }

    #[test]
    fn synthetic_store_is_sorted_and_deterministic() {
        let s = synthetic_store(20_000);
        assert_eq!(s.len(), 20_000);
        assert_eq!(s.view().len(), 20_000);
        let (mut a, mut b) = (RowBuf::new(), RowBuf::new());
        // folders first
        a.fill(&s, s.at(0).unwrap());
        b.fill(&s, s.at(19_999).unwrap());
        assert!(a.is_dir() && !b.is_dir());
        let s2 = synthetic_store(20_000);
        b.fill(&s2, s2.at(0).unwrap());
        assert_eq!(a.name_str(), b.name_str());
    }
}
