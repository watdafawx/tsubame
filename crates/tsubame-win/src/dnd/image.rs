//! A picture on the clipboard (no file list) saved as `Image.png`.
//!
//! Sources, best first: the registered `PNG` format (the bytes are the file),
//! `CF_DIBV5`, `CF_DIB` (a device-independent bitmap: header, optional colour
//! table / masks, pixels). A DIB becomes a BMP file in memory (14 byte file
//! header in front) and WIC re-encodes it as PNG. Runs on a shell worker (an
//! STA that pumps), never on the UI thread.

use super::{get_bytes, has_format};
use std::path::{Path, PathBuf};
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Ole::*;
use windows::core::{Interface, PCWSTR};

/// Largest picture taken (a 4K screenshot is ~33 MB as a DIB).
pub const IMAGE_CAP: usize = 256 << 20;

/// What the clipboard holds as a picture.
pub enum Pic {
    /// PNG file bytes
    Png(Vec<u8>),
    /// a packed DIB (`BITMAPINFOHEADER`.. + pixels)
    Dib(Vec<u8>),
}

fn cf_png() -> u16 {
    static C: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        let w = crate::wide("PNG");
        unsafe { RegisterClipboardFormatW(PCWSTR(w.as_ptr())) as u16 }
    })
}

/// The data object carries a picture.
pub fn has_image(obj: &IDataObject) -> bool {
    has_format(obj, cf_png()) || has_format(obj, CF_DIBV5.0) || has_format(obj, CF_DIB.0)
}

/// The picture, best format first.
pub fn read_image(obj: &IDataObject) -> Option<Pic> {
    if let Some(b) = get_bytes(obj, cf_png(), -1, IMAGE_CAP).filter(|b| b.starts_with(&PNG_MAGIC)) {
        return Some(Pic::Png(b));
    }
    for cf in [CF_DIBV5.0, CF_DIB.0] {
        if let Some(b) = get_bytes(obj, cf, -1, IMAGE_CAP).filter(|b| b.len() > 40) {
            return Some(Pic::Dib(b));
        }
    }
    None
}

pub const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Where the pixels start in a packed DIB, from its header: the header, the
/// colour masks of a plain 40 byte header with `BI_BITFIELDS` / `BI_ALPHABITFIELDS`,
/// and the colour table. `None`: not a DIB we know.
pub fn dib_pixel_offset(dib: &[u8]) -> Option<usize> {
    let le32 = |i: usize| dib.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let size = le32(0)? as usize;
    // BITMAPCOREHEADER (12) has 3 byte palette entries and no compression: not worth it
    if !matches!(size, 40 | 52 | 56 | 108 | 124) || dib.len() < size {
        return None;
    }
    let bpp = u16::from_le_bytes([dib[14], dib[15]]) as usize;
    let compression = le32(16)?;
    let used = le32(32)? as usize;
    let masks = if size == 40 {
        match compression {
            3 => 12,  // BI_BITFIELDS
            6 => 16,  // BI_ALPHABITFIELDS
            _ => 0,
        }
    } else {
        0
    };
    let colours = if used > 0 { used } else if bpp > 0 && bpp <= 8 { 1usize << bpp } else { 0 };
    let off = size + masks + colours * 4;
    (off <= dib.len()).then_some(off)
}

/// A BMP file image of a packed DIB.
pub fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    let off = dib_pixel_offset(dib)?;
    let total = 14 + dib.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&((14 + off) as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

/// PNG-encodes a BMP file image with WIC and writes it to `dest`.
fn encode_png_file(bmp: &[u8], dest: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::GENERIC_WRITE;
    use windows::Win32::Graphics::Imaging::*;
    let e = |what: &str, err: windows::core::Error| format!("{what} {:?}", err.code());
    unsafe {
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(|x| e("WIC", x))?;
        let input = f.CreateStream().map_err(|x| e("stream", x))?;
        input.InitializeFromMemory(bmp).map_err(|x| e("memory", x))?;
        let dec = f.CreateDecoderFromStream(&input, std::ptr::null(), WICDecodeMetadataCacheOnDemand).map_err(|x| e("decode", x))?;
        let frame = dec.GetFrame(0).map_err(|x| e("frame", x))?;
        let mut fmt = frame.GetPixelFormat().map_err(|x| e("format", x))?;
        let src_fmt = fmt;
        let enc = f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).map_err(|x| e("encoder", x))?;
        let out = f.CreateStream().map_err(|x| e("file stream", x))?;
        let w = crate::wide(&dest.to_string_lossy());
        out.InitializeFromFilename(PCWSTR(w.as_ptr()), GENERIC_WRITE.0).map_err(|x| e("file", x))?;
        enc.Initialize(&out, WICBitmapEncoderNoCache).map_err(|x| e("init", x))?;
        let mut target = None;
        enc.CreateNewFrame(&mut target, std::ptr::null_mut()).map_err(|x| e("new frame", x))?;
        let target = target.ok_or("no frame")?;
        target.Initialize(None).map_err(|x| e("frame init", x))?;
        target.SetPixelFormat(&mut fmt).map_err(|x| e("pixel format", x))?;
        // the encoder may want another layout (PNG has no 32 bit without alpha)
        let src: IWICBitmapSource = if fmt == src_fmt { frame.cast().map_err(|x| e("source", x))? } else { WICConvertBitmapSource(&fmt, &frame).map_err(|x| e("convert", x))? };
        target.WriteSource(&src, std::ptr::null()).map_err(|x| e("write", x))?;
        target.Commit().map_err(|x| e("frame commit", x))?;
        enc.Commit().map_err(|x| e("commit", x))?;
    }
    Ok(())
}

/// Claims a free `base`+".png" in `folder` (created empty so a racing job can
/// not take it), returns its path.
fn claim(folder: &Path, base: &str) -> Result<PathBuf, String> {
    for _ in 0..50 {
        let name = crate::shellnew::unique_name(base, ".png", &|n| folder.join(n).exists() || std::fs::symlink_metadata(folder.join(n)).is_ok());
        let p = folder.join(&name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("no free name".into())
}

/// Saves the clipboard's picture as `<base>.png` (a free name) in `folder`.
/// Returns the file name.
pub fn save_clipboard_image(obj: &IDataObject, folder: &Path, base: &str) -> Result<String, String> {
    let pic = read_image(obj).ok_or("No picture on the clipboard")?;
    let dest = claim(folder, base)?;
    let res = match pic {
        Pic::Png(b) => std::fs::write(&dest, b).map_err(|e| e.to_string()),
        Pic::Dib(d) => dib_to_bmp(&d).ok_or_else(|| "Unsupported picture format".to_string()).and_then(|bmp| encode_png_file(&bmp, &dest)),
    };
    if let Err(e) = res {
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }
    Ok(dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(size: u32, bpp: u16, compression: u32, used: u32) -> Vec<u8> {
        let mut h = vec![0u8; size as usize];
        h[0..4].copy_from_slice(&size.to_le_bytes());
        h[4..8].copy_from_slice(&2i32.to_le_bytes());
        h[8..12].copy_from_slice(&2i32.to_le_bytes());
        h[12..14].copy_from_slice(&1u16.to_le_bytes());
        h[14..16].copy_from_slice(&bpp.to_le_bytes());
        h[16..20].copy_from_slice(&compression.to_le_bytes());
        h[32..36].copy_from_slice(&used.to_le_bytes());
        h
    }

    fn with_pixels(mut h: Vec<u8>, extra: usize) -> Vec<u8> {
        h.extend(std::iter::repeat_n(0xAB, extra));
        h
    }

    #[test]
    fn pixel_offset_follows_the_header() {
        // 24 bit: the header only
        assert_eq!(dib_pixel_offset(&with_pixels(header(40, 24, 0, 0), 16)), Some(40));
        // 8 bit: 256 colours of 4 bytes after the header
        assert_eq!(dib_pixel_offset(&with_pixels(header(40, 8, 0, 0), 256 * 4 + 4)), Some(40 + 1024));
        // an explicit colour count wins
        assert_eq!(dib_pixel_offset(&with_pixels(header(40, 8, 0, 10), 64)), Some(40 + 40));
        // 32 bit bit fields in a plain header: three masks follow
        assert_eq!(dib_pixel_offset(&with_pixels(header(40, 32, 3, 0), 16)), Some(52));
        assert_eq!(dib_pixel_offset(&with_pixels(header(40, 32, 6, 0), 16)), Some(56));
        // V5 carries its masks inside
        assert_eq!(dib_pixel_offset(&with_pixels(header(124, 32, 3, 0), 16)), Some(124));
        // not a DIB: odd header size, cut short
        assert_eq!(dib_pixel_offset(&with_pixels(header(40, 24, 0, 0), 0)[..30]), None);
        let mut odd = header(40, 24, 0, 0);
        odd[0] = 41;
        assert_eq!(dib_pixel_offset(&odd), None);
        assert_eq!(dib_pixel_offset(&[]), None);
        // a colour table that runs past the data
        assert_eq!(dib_pixel_offset(&header(40, 8, 0, 0)), None);
    }

    #[test]
    fn a_dib_becomes_a_bmp_file() {
        let dib = with_pixels(header(40, 24, 0, 0), 16);
        let bmp = dib_to_bmp(&dib).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize, bmp.len());
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 14 + 40);
        assert_eq!(&bmp[14..], &dib[..]);
        assert!(dib_to_bmp(&[1, 2, 3]).is_none());
    }

    #[test]
    fn wic_turns_a_dib_into_a_png_file() {
        // 2 x 2, 24 bit, rows padded to 8 bytes: red, green / blue, white
        let mut dib = header(40, 24, 0, 0);
        let px: [u8; 16] = [0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0];
        dib.extend_from_slice(&px);
        let bmp = dib_to_bmp(&dib).unwrap();
        let dir = std::env::temp_dir().join(format!("tsubame-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.png");
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let res = encode_png_file(&bmp, &out);
        let bytes = std::fs::read(&out).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        res.unwrap();
        assert!(bytes.starts_with(&PNG_MAGIC) && bytes.len() > 40, "{} bytes", bytes.len());
    }

    #[test]
    fn claiming_a_name_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("tsubame-claim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = claim(&dir, "Image").unwrap();
        let b = claim(&dir, "Image").unwrap();
        let c = claim(&dir, "Image").unwrap();
        let names: Vec<_> = [&a, &b, &c].iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(names, ["Image.png", "Image (2).png", "Image (3).png"]);
    }
}
