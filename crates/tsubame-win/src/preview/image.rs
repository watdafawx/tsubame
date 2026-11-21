//! Image previews: WIC decode scaled to [`PREVIEW_MAX_EDGE`] and a bounded LRU
//! of decoded bitmaps.
//!
//! Decoding uses [`crate::bitmap::load_file`] for files it accepts (`.ico` and
//! images up to its 1 MiB read cap). Larger photos and screenshots — the usual
//! case for a preview pane — go through the same WIC pipeline here with the
//! preview's own 256 MiB cap, because the icon loader deliberately refuses big
//! files.
//!
//! The LRU follows `docs/PLAN.md` §3 rule 5: a hard 32 MiB / 32-item cap, least
//! recently used evicted first, and [`PreviewImageCache::clear`] when the pane
//! or tab closes so the leak gate sees the memory return.

use super::{path_key, PreviewError};
use crate::bitmap::{self, Bitmap};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Longest edge a preview bitmap is scaled to (`docs/preview-mock.md` #8).
pub const PREVIEW_MAX_EDGE: u32 = 2048;

/// Biggest file the preview decoder will read (256 MiB).
pub const PREVIEW_MAX_FILE_BYTES: u64 = 256 << 20;

/// Hard byte cap of the decoded-bitmap LRU (32 MiB).
pub const DEFAULT_CACHE_BYTES: usize = 32 << 20;

/// Hard item cap of the decoded-bitmap LRU.
pub const DEFAULT_CACHE_ITEMS: usize = 32;

/// Header bytes [`image_size`] needs (enough for a JPEG SOF scan in practice).
const SNIFF_BYTES: usize = 64 << 10;

/// One decoded image: the drawable bitmap plus what the metadata strip shows.
#[derive(Clone, PartialEq)]
pub struct LoadedImage {
    /// Premultiplied BGRA, fitted within [`PREVIEW_MAX_EDGE`] on the long edge.
    pub bmp: Bitmap,
    /// The file's own width (the bitmap is scaled down to fit).
    pub orig_w: u32,
    /// The file's own height.
    pub orig_h: u32,
    /// Upper-case extension (`PNG`, `JPG`, ...) for the format badge.
    pub format: String,
}

impl std::fmt::Debug for LoadedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LoadedImage({}x{} -> {}x{}, {})", self.orig_w, self.orig_h, self.bmp.w, self.bmp.h, self.format)
    }
}

impl LoadedImage {
    /// Bytes this entry costs in the LRU.
    pub fn cost(&self) -> usize {
        self.bmp.bytes()
    }
}

/// Cache key: the path plus the file's stamp, so an edited file misses.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PreviewImageKey {
    /// Lower-cased full path (Windows paths are case-insensitive).
    pub path: PathBuf,
    /// Milliseconds since the UNIX epoch, 0 when unavailable.
    pub mtime_ms: i64,
    pub len: u64,
}

impl PreviewImageKey {
    pub fn for_path(path: &Path) -> Option<PreviewImageKey> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        Some(PreviewImageKey { path: path_key(path), mtime_ms, len: meta.len() })
    }
}

struct CacheEntry {
    image: LoadedImage,
    used: u64,
    cost: usize,
}

/// Hard-capped LRU of decoded preview bitmaps.
///
/// Byte and item caps both apply; whichever is hit first evicts the least
/// recently used entry.
pub struct PreviewImageCache {
    max_bytes: usize,
    max_items: usize,
    bytes: usize,
    tick: u64,
    entries: HashMap<PreviewImageKey, CacheEntry>,
}

impl Default for PreviewImageCache {
    fn default() -> Self {
        PreviewImageCache::new(DEFAULT_CACHE_BYTES, DEFAULT_CACHE_ITEMS)
    }
}

impl PreviewImageCache {
    /// A cache with the given caps (`max_items` at least 1).
    pub fn new(max_bytes: usize, max_items: usize) -> PreviewImageCache {
        PreviewImageCache { max_bytes, max_items: max_items.max(1), bytes: 0, tick: 0, entries: HashMap::new() }
    }

    /// Cached image for `key`; bumps its use stamp.
    pub fn get(&mut self, key: &PreviewImageKey) -> Option<LoadedImage> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(key)?;
        entry.used = tick;
        Some(entry.image.clone())
    }

    /// True when `key` is held (does not bump the use stamp).
    pub fn contains(&self, key: &PreviewImageKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Stores `image`, evicting until both caps hold.
    pub fn insert(&mut self, key: PreviewImageKey, image: LoadedImage) {
        let cost = image.cost();
        // A single image over the whole cap is never worth flushing the cache
        // for; show it once and drop it.
        if cost > self.max_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.cost);
        }
        self.tick += 1;
        let used = self.tick;
        self.entries.insert(key, CacheEntry { image, used, cost });
        self.bytes += cost;
        self.evict();
    }

    fn evict(&mut self) {
        while self.entries.len() > self.max_items || self.bytes > self.max_bytes {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(entry.cost);
            }
        }
    }

    /// Entries held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of decoded pixels held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Caps in force, `(bytes, items)`.
    pub fn caps(&self) -> (usize, usize) {
        (self.max_bytes, self.max_items)
    }

    /// Drops everything (pane or tab closed).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Decodes `path` (cache first) into a preview bitmap.
///
/// `Err` means "no image preview", never a panic; the caller turns it into an
/// Unsupported card or a hex fallback.
pub fn load(path: &Path, cache: &mut PreviewImageCache) -> Result<LoadedImage, PreviewError> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PreviewError::NotFound(path.display().to_string()),
        std::io::ErrorKind::PermissionDenied => PreviewError::AccessDenied(path.display().to_string()),
        _ => PreviewError::Io(format!("{}: {e}", path.display())),
    })?;
    if !meta.is_file() {
        return Err(PreviewError::Unsupported("not a file".into()));
    }
    if meta.len() == 0 {
        return Err(PreviewError::Unsupported("empty image".into()));
    }
    if meta.len() > PREVIEW_MAX_FILE_BYTES {
        return Err(PreviewError::TooLarge { size: meta.len() });
    }
    let key = PreviewImageKey::for_path(path).ok_or_else(|| PreviewError::Io(path.display().to_string()))?;
    if let Some(hit) = cache.get(&key) {
        return Ok(hit);
    }
    let image = decode(path, meta.len())?;
    cache.insert(key, image.clone());
    Ok(image)
}

/// Decodes without the cache.
pub fn decode(path: &Path, size: u64) -> Result<LoadedImage, PreviewError> {
    let dims = file_size_sniff(path);
    let bmp = if size <= bitmap::MAX_IMAGE_FILE_BYTES {
        bitmap::load_file(path, PREVIEW_MAX_EDGE)
    } else {
        None
    };
    let bmp = match bmp {
        Some(b) => b,
        None => decode_wic(path)?,
    };
    let (orig_w, orig_h) = dims.unwrap_or((bmp.w, bmp.h));
    let format = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_uppercase();
    Ok(LoadedImage { bmp, orig_w, orig_h, format })
}

/// Original dimensions from the file header; `None` when the format is not
/// sniffed (the decoded size is then reported instead).
pub fn file_size_sniff(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut buf = vec![0u8; SNIFF_BYTES];
    let mut f = std::fs::File::open(path).ok()?;
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    image_size(&buf)
}

/// Dimensions from the first bytes of a PNG, GIF, BMP, JPEG, WebP or ICO file.
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| -> Option<u32> { b.get(i..i + 2).map(|s| u16::from_be_bytes([s[0], s[1]]) as u32) };
    let le16 = |i: usize| -> Option<u32> { b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as u32) };
    let be32 = |i: usize| -> Option<u32> { b.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]])) };
    let le32 = |i: usize| -> Option<u32> { b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])) };
    if !b.is_empty() && b.starts_with(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]) {
        let (w, h) = (be32(16)?, be32(20)?);
        return (w > 0 && h > 0).then_some((w, h));
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        let (w, h) = (le16(6)?, le16(8)?);
        return (w > 0 && h > 0).then_some((w, h));
    }
    if b.starts_with(b"BM") && b.len() >= 26 {
        let w = le32(18)? as i32;
        let h = le32(22)? as i32;
        return (w > 0 && h != 0).then_some((w as u32, h.unsigned_abs()));
    }
    if b.len() >= 8 && b.starts_with(&[0, 0, 1, 0]) {
        // ICO / CUR: the first directory entry's width and height (0 = 256).
        let w = *b.get(6)? as u32;
        let h = *b.get(7)? as u32;
        let dim = |v: u32| if v == 0 { 256 } else { v };
        return Some((dim(w), dim(h)));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        match b.get(12..16)? {
            b"VP8X" => {
                let w = 1 + (*b.get(24)? as u32 | (*b.get(25)? as u32) << 8 | (*b.get(26)? as u32) << 16);
                let h = 1 + (*b.get(27)? as u32 | (*b.get(28)? as u32) << 8 | (*b.get(29)? as u32) << 16);
                return Some((w, h));
            }
            b"VP8 " => {
                if b.get(23..26) != Some(&[0x9d, 0x01, 0x2a]) {
                    return None;
                }
                let w = le16(26)? & 0x3fff;
                let h = le16(28)? & 0x3fff;
                return Some((w, h));
            }
            b"VP8L" => {
                let bits = le32(21)?;
                return Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1));
            }
            _ => return None,
        }
    }
    if b.starts_with(&[0xff, 0xd8]) {
        // Walk the JPEG segments to the first SOFn frame header.
        let mut i = 2usize;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
                i += 2;
                continue;
            }
            if marker == 0xd9 || marker == 0xda {
                return None;
            }
            let len = be16(i + 2)? as usize;
            let is_sof = matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf);
            if is_sof {
                let h = be16(i + 5)?;
                let w = be16(i + 7)?;
                return (w > 0 && h > 0).then_some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

/// WIC decode for files over the icon loader's cap: premultiplied BGRA fitted
/// within [`PREVIEW_MAX_EDGE`].
fn decode_wic(path: &Path) -> Result<Bitmap, PreviewError> {
    use windows::Win32::Foundation::{GENERIC_READ, RPC_E_CHANGED_MODE, S_FALSE, S_OK};
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };
    use windows::core::PCWSTR;

    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let _guard = if hr == S_OK || hr == S_FALSE {
        ComGuard(true)
    } else if hr == RPC_E_CHANGED_MODE {
        ComGuard(false)
    } else {
        return Err(PreviewError::Decode("COM unavailable".into()));
    };

    let wpath = crate::wide(&path.to_string_lossy());
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(|e| PreviewError::Decode(e.message()))?;
        let decoder = factory
            .CreateDecoderFromFilename(PCWSTR(wpath.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .map_err(|e| PreviewError::Decode(e.message()))?;
        let frame = decoder.GetFrame(0).map_err(|e| PreviewError::Decode(e.message()))?;
        let src = WICConvertBitmapSource(&GUID_WICPixelFormat32bppPBGRA, &frame).map_err(|e| PreviewError::Decode(e.message()))?;
        let (mut w, mut h) = (0u32, 0u32);
        src.GetSize(&mut w, &mut h).map_err(|e| PreviewError::Decode(e.message()))?;
        if w == 0 || h == 0 || w > 65_535 || h > 65_535 {
            return Err(PreviewError::Decode("bad dimensions".into()));
        }
        let stride = w.checked_mul(4).ok_or_else(|| PreviewError::Decode("image too wide".into()))?;
        let len = (stride as usize).checked_mul(h as usize).ok_or_else(|| PreviewError::Decode("image too tall".into()))?;
        let mut buf = vec![0u8; len];
        src.CopyPixels(std::ptr::null(), stride, &mut buf).map_err(|e| PreviewError::Decode(e.message()))?;
        let px: Vec<u32> = buf.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
        Ok(Bitmap { w, h, px }.fit_within(PREVIEW_MAX_EDGE))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Minimal bottom-up 24-bit BMP, used by the preview tests.
#[cfg(test)]
pub(crate) fn tiny_bmp(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let row = (w as usize * 3).div_ceil(4) * 4;
    let data = row * h as usize;
    let mut out = Vec::with_capacity(54 + data);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54u32 + data as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(data as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for _ in 0..h {
        let start = out.len();
        for _ in 0..w {
            out.extend_from_slice(&[rgb[2], rgb[1], rgb[0]]);
        }
        while out.len() - start < row {
            out.push(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-preview-img-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A `w` x `h` bitmap of one colour, used for cache arithmetic.
    fn flat(w: u32, h: u32) -> LoadedImage {
        LoadedImage { bmp: Bitmap { w, h, px: vec![0xff11_2233; (w * h) as usize] }, orig_w: w, orig_h: h, format: "BMP".into() }
    }

    fn key(n: u32) -> PreviewImageKey {
        PreviewImageKey { path: PathBuf::from(format!("c:\\img{n}.png")), mtime_ms: 1, len: 1 }
    }

    /// Minimal bottom-up 24-bit BMP.

    #[test]
    fn caps_are_the_documented_ones() {
        let c = PreviewImageCache::default();
        assert_eq!(c.caps(), (32 << 20, 32));
        assert_eq!(DEFAULT_CACHE_BYTES, 32 * 1024 * 1024);
        assert!(c.is_empty());
        assert_eq!(PREVIEW_MAX_EDGE, 2048);
    }

    #[test]
    fn byte_cap_evicts_the_least_recently_used() {
        let mut c = PreviewImageCache::new(4 * 1024, 32);
        for n in 0..4 {
            c.insert(key(n), flat(16, 16)); // 1 KiB each
        }
        assert_eq!((c.len(), c.bytes()), (4, 4096));
        // touch 0 so 1 is now the oldest, then add a fifth
        assert!(c.get(&key(0)).is_some());
        c.insert(key(4), flat(16, 16));
        assert_eq!(c.len(), 4, "byte cap holds");
        assert!(c.bytes() <= 4 * 1024);
        assert!(c.contains(&key(0)), "recently used survives");
        assert!(!c.contains(&key(1)), "least recently used is evicted");
        assert!(c.contains(&key(4)));
    }

    #[test]
    fn the_32_mib_cap_holds_for_full_sized_bitmaps() {
        let mut c = PreviewImageCache::new(DEFAULT_CACHE_BYTES, DEFAULT_CACHE_ITEMS);
        for n in 0..33 {
            c.insert(key(n), flat(512, 512)); // 1 MiB each
        }
        assert_eq!(c.bytes(), DEFAULT_CACHE_BYTES, "never over the byte cap");
        assert_eq!(c.len(), 32);
        assert!(!c.contains(&key(0)));
        assert!(c.contains(&key(32)));
        c.clear();
        assert_eq!((c.len(), c.bytes()), (0, 0));
    }

    #[test]
    fn item_cap_evicts_even_when_the_bytes_fit() {
        let mut c = PreviewImageCache::new(1 << 20, 2);
        for n in 0..3 {
            c.insert(key(n), flat(16, 16));
        }
        assert_eq!(c.len(), 2);
        assert!(!c.contains(&key(0)) && c.contains(&key(1)) && c.contains(&key(2)));
    }

    #[test]
    fn an_image_over_the_whole_cap_is_not_cached() {
        let mut c = PreviewImageCache::new(1024, 32);
        c.insert(key(0), flat(32, 32)); // 4 KiB
        assert!(c.is_empty(), "a single oversized image does not flush the cache");
    }

    #[test]
    fn the_key_includes_the_stamp_so_an_edited_file_misses() {
        let d = temp_dir("stamp");
        let p = d.join("a.bmp");
        std::fs::write(&p, tiny_bmp(2, 2, [0x10, 0x20, 0x30])).unwrap();
        let mut c = PreviewImageCache::default();
        let first = load(&p, &mut c).unwrap();
        assert_eq!((first.orig_w, first.orig_h), (2, 2));
        assert_eq!(c.len(), 1);
        // same stamp: cache hit
        let again = load(&p, &mut c).unwrap();
        assert_eq!(again, first);
        // different stamp: miss, decodes again
        let mut other = PreviewImageKey::for_path(&p).unwrap();
        other.len += 1;
        assert!(!c.contains(&other));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_real_bmp_decodes_and_scales() {
        let d = temp_dir("decode");
        let p = d.join("a.bmp");
        std::fs::write(&p, tiny_bmp(4, 2, [0x80, 0x40, 0x20])).unwrap();
        let mut c = PreviewImageCache::default();
        let img = load(&p, &mut c).unwrap();
        assert_eq!((img.bmp.w, img.bmp.h), (4, 2));
        assert_eq!((img.orig_w, img.orig_h), (4, 2));
        assert_eq!(img.format, "BMP");
        // WIC hands back bottom-up BMPs as premultiplied PBGRA; opaque stays opaque
        assert!(img.bmp.px.iter().all(|&p| p >> 24 == 0xff), "opaque bitmap");
        assert!(load(&d.join("missing.png"), &mut c).is_err());
        let bad = d.join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(load(&bad, &mut c).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn header_sizes_are_sniffed_for_common_formats() {
        // 320x200 PNG IHDR
        let mut png = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
        png.extend_from_slice(&[0; 8]);
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&200u32.to_be_bytes());
        assert_eq!(image_size(&png), Some((320, 200)));

        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&120u16.to_le_bytes());
        gif.extend_from_slice(&45u16.to_le_bytes());
        assert_eq!(image_size(&gif), Some((120, 45)));

        let bmp = tiny_bmp(7, 3, [0; 3]);
        assert_eq!(image_size(&bmp), Some((7, 3)));

        // minimal JPEG: SOI, APP0, SOF0 640x480
        let mut jpg = vec![0xff, 0xd8];
        jpg.extend_from_slice(&[0xff, 0xe0, 0x00, 0x04, 0x00, 0x00]);
        jpg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        jpg.extend_from_slice(&480u16.to_be_bytes());
        jpg.extend_from_slice(&640u16.to_be_bytes());
        jpg.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(image_size(&jpg), Some((640, 480)));

        assert_eq!(image_size(b"nope"), None);
    }
}
