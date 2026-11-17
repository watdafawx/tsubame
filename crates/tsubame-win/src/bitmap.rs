//! HICON / HBITMAP to premultiplied BGRA (`0xAARRGGBB` per `u32`, the layout
//! `D2D1_ALPHA_MODE_PREMULTIPLIED` + `DXGI_FORMAT_B8G8R8A8_UNORM` expects).
//!
//! Icons come in three shapes:
//! - 32-bit with an alpha channel: straight (not premultiplied) alpha, so it
//!   is premultiplied here.
//! - 32-bit or less **without** alpha (all alpha bytes 0, old icons): the AND
//!   mask says which pixels are transparent; alpha = 255 elsewhere.
//! - monochrome (no colour bitmap): the mask bitmap is twice as tall, AND on
//!   top, XOR below.
//!
//! Every GDI object made here is deleted here: `GetIconInfo` hands back two
//! new bitmaps the caller owns (a classic GDI leak).

use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, HICON, ICONINFO};

/// Premultiplied BGRA pixels, top-down rows.
#[derive(Clone)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

impl Bitmap {
    pub fn bytes(&self) -> usize {
        self.px.len() * 4
    }
}

/// Reads any HBITMAP as 32-bit top-down BGRA (alpha as stored). Does not
/// take ownership of `hbm`.
pub fn read_hbitmap(hbm: HBITMAP) -> Option<(u32, u32, Vec<u32>)> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(hbm.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth.max(0) as u32, bm.bmHeight.unsigned_abs());
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            return None;
        }
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u32; (w * h) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(dc, hbm, 0, h, Some(px.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        (lines == h as i32).then_some((w, h, px))
    }
}

/// HICON to premultiplied BGRA. Does not destroy `icon`.
pub fn from_hicon(icon: HICON) -> Option<Bitmap> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(icon, &mut ii).ok()?;
        let color = (!ii.hbmColor.is_invalid()).then(|| read_hbitmap(ii.hbmColor)).flatten();
        let mask = (!ii.hbmMask.is_invalid()).then(|| read_hbitmap(ii.hbmMask)).flatten();
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask.into());
        }
        match (color, mask) {
            (Some((w, h, mut px)), mask) => {
                if has_alpha(&px) {
                    premultiply(&mut px);
                } else if let Some((mw, mh, m)) = mask.filter(|(mw, mh, _)| *mw == w && *mh >= h) {
                    let _ = (mw, mh);
                    apply_and_mask(&mut px, &m[..(w * h) as usize]);
                } else {
                    opaque(&mut px);
                }
                Some(Bitmap { w, h, px })
            }
            // monochrome: mask is w x 2h, AND rows then XOR rows
            (None, Some((w, h2, m))) if h2 >= 2 => {
                let h = h2 / 2;
                let n = (w * h) as usize;
                let (and, xor) = m.split_at(n);
                let mut px = xor[..n].to_vec();
                apply_and_mask(&mut px, and);
                Some(Bitmap { w, h, px })
            }
            _ => None,
        }
    }
}

/// An HBITMAP from `IShellItemImageFactory::GetImage` to premultiplied BGRA.
/// Does not delete `hbm`.
pub fn from_shell_hbitmap(hbm: HBITMAP) -> Option<Bitmap> {
    let (w, h, mut px) = read_hbitmap(hbm)?;
    normalize_shell(&mut px);
    Some(Bitmap { w, h, px })
}

pub fn has_alpha(px: &[u32]) -> bool {
    px.iter().any(|p| p >> 24 != 0)
}

/// Straight alpha to premultiplied, in place.
pub fn premultiply(px: &mut [u32]) {
    for p in px {
        let a = *p >> 24;
        if a == 255 {
            continue;
        }
        if a == 0 {
            *p = 0;
            continue;
        }
        let m = |c: u32| (c * a + 127) / 255;
        let (r, g, b) = ((*p >> 16) & 255, (*p >> 8) & 255, *p & 255);
        *p = a << 24 | m(r) << 16 | m(g) << 8 | m(b);
    }
}

/// Alpha from an AND mask (read as 32-bit: non-zero = transparent).
pub fn apply_and_mask(px: &mut [u32], mask: &[u32]) {
    for (p, m) in px.iter_mut().zip(mask) {
        *p = if m & 0x00ff_ffff != 0 { 0 } else { 0xff00_0000 | (*p & 0x00ff_ffff) };
    }
}

pub fn opaque(px: &mut [u32]) {
    for p in px {
        *p |= 0xff00_0000;
    }
}

/// Shell image factory bitmaps: no alpha at all (JPEG thumbnails) means
/// opaque; a colour channel above alpha means straight alpha; otherwise the
/// pixels already are premultiplied.
pub fn normalize_shell(px: &mut [u32]) {
    if !has_alpha(px) {
        opaque(px);
        return;
    }
    let straight = px.iter().any(|&p| {
        let a = p >> 24;
        ((p >> 16) & 255) > a || ((p >> 8) & 255) > a || (p & 255) > a
    });
    if straight {
        premultiply(px);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_halves_at_half_alpha() {
        let mut px = [0x80ff_8040u32, 0x00ff_ffff, 0xff12_3456];
        premultiply(&mut px);
        assert_eq!(px[0], 0x8080_4020);
        assert_eq!(px[1], 0, "alpha 0 clears colour");
        assert_eq!(px[2], 0xff12_3456, "opaque unchanged");
    }

    #[test]
    fn and_mask_gives_alpha() {
        let mut px = [0x0011_2233u32, 0x0044_5566];
        apply_and_mask(&mut px, &[0x0000_0000, 0x00ff_ffff]);
        assert_eq!(px, [0xff11_2233, 0]);
    }

    #[test]
    fn shell_bitmaps() {
        let mut jpeg = [0x0011_2233u32, 0x0044_5566];
        normalize_shell(&mut jpeg);
        assert_eq!(jpeg, [0xff11_2233, 0xff44_5566], "no alpha = opaque");
        let mut pre = [0x8040_2010u32, 0xff00_0000];
        normalize_shell(&mut pre);
        assert_eq!(pre, [0x8040_2010, 0xff00_0000], "premultiplied left alone");
        let mut straight = [0x80ff_ffffu32];
        normalize_shell(&mut straight);
        assert_eq!(straight, [0x8080_8080]);
    }
}
