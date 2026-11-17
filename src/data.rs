//! Fake directory data for the spike: 1,000,000 rows, never as `String` objects.
//!
//! Two sources behind one call, `Source::row(i, &mut RowBuf)`:
//! - `Procedural`: every field is a pure function of the row index. Zero bytes
//!   of storage, zero startup cost. Measures the renderer alone.
//! - `Store`: the architecture rule from PLAN.md section 3 (rule 2): one
//!   contiguous name buffer plus fixed 24-byte records. Measures what a real
//!   entry store costs.

pub const N: usize = 1_000_000;

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

const WORDS: [&str; 16] = [
    "report", "photo", "backup", "draft", "invoice", "holiday", "project", "notes", "design", "archive", "render", "budget", "screen", "export", "sample", "build",
];

/// 2018-01-01 00:00:00 UTC, unix seconds.
const DATE_BASE: u64 = 1_514_764_800;
const DATE_SPAN: u64 = 8 * 365 * 86_400;

/// One row, decoded and ready to draw. Lives on the stack, reused per row.
pub struct RowBuf {
    pub name: [u8; 56],
    pub name_len: usize,
    pub size: u64,
    pub mtime: u32,
    pub kind: u8,
}

impl RowBuf {
    pub const fn new() -> Self {
        RowBuf { name: [0; 56], name_len: 0, size: 0, mtime: 0, kind: 0 }
    }
    pub fn name_str(&self) -> &str {
        // names are ASCII by construction
        std::str::from_utf8(&self.name[..self.name_len]).unwrap_or("")
    }
    pub fn is_dir(&self) -> bool {
        self.kind == 0
    }
}

fn mix(mut x: u64) -> u64 {
    // splitmix64
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn push(buf: &mut RowBuf, s: &[u8]) {
    let n = s.len().min(buf.name.len() - buf.name_len);
    buf.name[buf.name_len..buf.name_len + n].copy_from_slice(&s[..n]);
    buf.name_len += n;
}

fn push_num(buf: &mut RowBuf, mut v: usize) {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    push(buf, &tmp[i..]);
}

/// Deterministic row `i`.
pub fn gen_row(i: usize, out: &mut RowBuf) {
    let h = mix(i as u64);
    out.kind = if h % 9 == 0 { 0 } else { 1 + ((h >> 20) % (KINDS.len() as u64 - 1)) as u8 };
    out.name_len = 0;
    push(out, WORDS[((h >> 8) & 15) as usize].as_bytes());
    push(out, b"-");
    push(out, WORDS[((h >> 12) & 15) as usize].as_bytes());
    push(out, b"-");
    push_num(out, i);
    if out.kind != 0 {
        push(out, b".");
        push(out, KINDS[out.kind as usize].0.as_bytes());
        out.size = ((h >> 24) & 0xFFFF) << ((h >> 40) % 14);
    } else {
        out.size = 0;
    }
    out.mtime = (DATE_BASE + (h >> 11) % DATE_SPAN) as u32;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rec {
    name_off: u32,
    mtime: u32,
    size: u64,
    name_len: u8,
    kind: u8,
}

pub struct Store {
    recs: Vec<Rec>,
    names: Vec<u8>,
}

impl Store {
    /// All `N` rows into one name buffer and one record array.
    pub fn build() -> Store {
        let mut recs = Vec::with_capacity(N);
        let mut names: Vec<u8> = Vec::with_capacity(N * 28);
        let mut b = RowBuf::new();
        for i in 0..N {
            gen_row(i, &mut b);
            recs.push(Rec { name_off: names.len() as u32, mtime: b.mtime, size: b.size, name_len: b.name_len as u8, kind: b.kind });
            names.extend_from_slice(&b.name[..b.name_len]);
        }
        Store { recs, names }
    }
    pub fn bytes(&self) -> usize {
        self.recs.len() * size_of::<Rec>() + self.names.len()
    }
    fn row(&self, i: usize, out: &mut RowBuf) {
        let r = &self.recs[i];
        let off = r.name_off as usize;
        out.name_len = r.name_len as usize;
        out.name[..out.name_len].copy_from_slice(&self.names[off..off + out.name_len]);
        out.size = r.size;
        out.mtime = r.mtime;
        out.kind = r.kind;
    }
}

pub enum Source {
    Procedural,
    Store(Store),
}

impl Source {
    pub fn row(&self, i: usize, out: &mut RowBuf) {
        match self {
            Source::Procedural => gen_row(i, out),
            Source::Store(s) => s.row(i, out),
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Source::Procedural => "procedural",
            Source::Store(_) => "store",
        }
    }
}

/// "12.3 KB" style, into a reused buffer.
pub fn fmt_size(size: u64, out: &mut String) {
    use std::fmt::Write;
    out.clear();
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = size as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        let _ = write!(out, "{size} B");
    } else {
        let _ = write!(out, "{v:.1} {}", UNITS[u]);
    }
}

/// "2025-03-14 09:41" from unix seconds (UTC), into a reused buffer.
pub fn fmt_date(secs: u32, out: &mut String) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_deterministic_and_store_matches() {
        let (mut a, mut b) = (RowBuf::new(), RowBuf::new());
        gen_row(123_456, &mut a);
        gen_row(123_456, &mut b);
        assert_eq!(a.name_str(), b.name_str());
        assert!(a.name_str().contains("123456"));
    }

    #[test]
    fn date_format() {
        let mut s = String::new();
        fmt_date(DATE_BASE as u32, &mut s);
        assert_eq!(s, "2018-01-01 00:00");
        fmt_date(1_741_945_260, &mut s);
        assert_eq!(s, "2025-03-14 09:41");
    }

    #[test]
    fn size_format() {
        let mut s = String::new();
        fmt_size(1536, &mut s);
        assert_eq!(s, "1.5 KB");
        fmt_size(7, &mut s);
        assert_eq!(s, "7 B");
    }
}
