//! Directory data for the spikes, never as one `String` per row.
//!
//! Sources behind one call, `Source::row(i, &mut RowBuf)`:
//! - `Procedural`: every field is a pure function of the row index. Zero bytes
//!   of storage, zero startup cost. Measures the renderer alone.
//! - `Store`: the architecture rule from PLAN.md section 3 (rule 2): one
//!   contiguous name buffer plus fixed 24-byte records. Measures what a real
//!   entry store costs.
//! - `Dir` (S4, `--dir <path>`): a real folder, listed on a worker thread into
//!   one name buffer + 24-byte records + a sort index.

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

/// `RowBuf::kind` of a real file whose extension is not in `KINDS`.
pub const KIND_OTHER: u8 = 255;

const WORDS: [&str; 16] = [
    "report", "photo", "backup", "draft", "invoice", "holiday", "project", "notes", "design", "archive", "render", "budget", "screen", "export", "sample", "build",
];

/// 2018-01-01 00:00:00 UTC, unix seconds.
const DATE_BASE: u64 = 1_514_764_800;
const DATE_SPAN: u64 = 8 * 365 * 86_400;

/// One row, decoded and ready to draw. Lives on the stack, reused per row.
pub struct RowBuf {
    /// UTF-8; real names reach 255 UTF-16 units (up to 765 bytes)
    pub name: [u8; 768],
    pub name_len: usize,
    pub size: u64,
    pub mtime: u32,
    /// index into `KINDS`, or `KIND_OTHER`
    pub kind: u8,
    /// Win32 file attributes (real folders only, else 0)
    pub attrs: u32,
}

impl RowBuf {
    pub const fn new() -> Self {
        RowBuf { name: [0; 768], name_len: 0, size: 0, mtime: 0, kind: 0, attrs: 0 }
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
    out.attrs = 0;
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
        out.attrs = 0;
    }
}

/// 24-byte record of a real directory entry.
#[repr(C)]
#[derive(Clone, Copy)]
struct DirRec {
    name_off: u32,
    attrs: u32,
    size: u64,
    mtime: u32,
    name_len: u16,
    kind: u8,
    _pad: u8,
}

const _: () = assert!(size_of::<DirRec>() == 24);

const ATTR_DIRECTORY: u32 = 0x10;

/// A real folder: one UTF-8 name buffer, fixed records, a sort index
/// (folders first, then by name, case-insensitive).
pub struct DirStore {
    pub root: String,
    recs: Vec<DirRec>,
    names: Vec<u8>,
    order: Vec<u32>,
    /// enumerate + sort time on the worker
    pub list_ms: f64,
}

/// FILETIME (100 ns since 1601) to unix seconds.
fn filetime_to_unix(ft: u64) -> u32 {
    (ft / 10_000_000).saturating_sub(11_644_473_600).min(u32::MAX as u64) as u32
}

fn kind_of(name: &str, dir: bool) -> u8 {
    if dir {
        return 0;
    }
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => return KIND_OTHER,
    };
    KINDS.iter().skip(1).position(|(e, _)| e.eq_ignore_ascii_case(ext)).map(|p| p as u8 + 1).unwrap_or(KIND_OTHER)
}

impl DirStore {
    /// Lists `root`. Worker threads only: touches the disk.
    pub fn load(root: &str) -> Result<DirStore, String> {
        let t = std::time::Instant::now();
        let mut root_s = root.trim_end_matches(['\\', '/']).to_string();
        if !root_s.contains('\\') {
            root_s.push('\\'); // "C:" -> "C:\"
        }
        let mut recs = Vec::new();
        let mut names: Vec<u8> = Vec::new();
        tsubame_win::enumerate::list_dir(&root_s, |e| {
            let name = String::from_utf16_lossy(e.name);
            let dir = e.attrs & ATTR_DIRECTORY != 0;
            recs.push(DirRec {
                name_off: names.len() as u32,
                attrs: e.attrs,
                size: if dir { 0 } else { e.size },
                mtime: filetime_to_unix(e.mtime),
                name_len: name.len().min(u16::MAX as usize) as u16,
                kind: kind_of(&name, dir),
                _pad: 0,
            });
            names.extend_from_slice(name.as_bytes());
        })
        .map_err(|e| format!("{root}: {}", e.message()))?;
        let mut st = DirStore { root: root_s, recs, names, order: Vec::new(), list_ms: 0.0 };
        let lower: Vec<String> = (0..st.recs.len()).map(|i| st.raw_name(i).to_lowercase()).collect();
        let mut order: Vec<u32> = (0..st.recs.len() as u32).collect();
        order.sort_by(|&a, &b| {
            let (ra, rb) = (&st.recs[a as usize], &st.recs[b as usize]);
            (ra.kind != 0).cmp(&(rb.kind != 0)).then_with(|| lower[a as usize].cmp(&lower[b as usize]))
        });
        st.order = order;
        st.list_ms = t.elapsed().as_secs_f64() * 1000.0;
        Ok(st)
    }

    fn raw_name(&self, rec: usize) -> &str {
        let r = &self.recs[rec];
        let off = r.name_off as usize;
        std::str::from_utf8(&self.names[off..off + r.name_len as usize]).unwrap_or("")
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn name(&self, i: usize) -> &str {
        self.raw_name(self.order[i] as usize)
    }

    /// Full path of row `i`.
    pub fn path(&self, i: usize) -> String {
        let mut p = String::with_capacity(self.root.len() + 64);
        p.push_str(&self.root);
        if !p.ends_with('\\') {
            p.push('\\');
        }
        p.push_str(self.name(i));
        p
    }

    pub fn is_dir(&self, i: usize) -> bool {
        self.recs[self.order[i] as usize].kind == 0
    }

    fn row(&self, i: usize, out: &mut RowBuf) {
        let ri = self.order[i] as usize;
        let r = self.recs[ri];
        let n = self.raw_name(ri).as_bytes();
        out.name_len = n.len().min(out.name.len());
        out.name[..out.name_len].copy_from_slice(&n[..out.name_len]);
        out.size = r.size;
        out.mtime = r.mtime;
        out.kind = r.kind;
        out.attrs = r.attrs;
    }

    pub fn bytes(&self) -> usize {
        self.recs.len() * size_of::<DirRec>() + self.names.len() + self.order.len() * 4
    }
}

pub enum Source {
    Procedural,
    Store(Store),
    Dir(DirStore),
    /// `--dir` while the worker lists it (zero rows)
    Loading(String),
}

impl Source {
    pub fn row(&self, i: usize, out: &mut RowBuf) {
        match self {
            Source::Procedural | Source::Loading(_) => gen_row(i, out),
            Source::Store(s) => s.row(i, out),
            Source::Dir(d) => d.row(i, out),
        }
    }
    pub fn len(&self) -> usize {
        match self {
            Source::Dir(d) => d.len(),
            Source::Loading(_) => 0,
            _ => N,
        }
    }
    /// Full path of row `i` (real folders only).
    pub fn path(&self, i: usize) -> Option<String> {
        match self {
            Source::Dir(d) if i < d.len() => Some(d.path(i)),
            _ => None,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Source::Procedural => "procedural",
            Source::Store(_) => "store",
            Source::Dir(_) => "dir",
            Source::Loading(_) => "loading",
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

    #[test]
    fn real_kinds_and_type_text() {
        assert_eq!(kind_of("a.TXT", false), 1);
        assert_eq!(kind_of("folder.txt", true), 0);
        assert_eq!(kind_of("notepad.exe", false), 6);
        assert_eq!(kind_of("x.weird", false), KIND_OTHER);
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
    fn dir_store_lists_a_real_folder() {
        let tmp = std::env::temp_dir().join(format!("tsubame-s4-test-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("Sub")).unwrap();
        std::fs::write(tmp.join("b.txt"), b"hi").unwrap();
        std::fs::write(tmp.join("A.md"), b"hello").unwrap();
        let d = DirStore::load(tmp.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(d.len(), 3);
        assert_eq!(d.name(0), "Sub", "folders first");
        assert_eq!(d.name(1), "A.md", "then by name, case-insensitive");
        assert!(d.path(2).ends_with("\\b.txt"));
        let mut r = RowBuf::new();
        Source::Dir(d).row(2, &mut r);
        assert_eq!(r.size, 2);
    }
}
