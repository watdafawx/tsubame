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
#[derive(Clone, PartialEq)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bitmap({}x{})", self.w, self.h)
    }
}

impl Bitmap {
    pub fn bytes(&self) -> usize {
        self.px.len() * 4
    }

    /// Area-averaging resize to exactly `w` x `h` (premultiplied pixels stay
    /// premultiplied). Workers use it to cut big shell bitmaps (256 px
    /// jumbo icons, cached 256 / 1024 px thumbnails) to the size the cell
    /// draws, so the caches hold what is shown and nothing more.
    pub fn scaled(&self, w: u32, h: u32) -> Bitmap {
        let (w, h) = (w.max(1), h.max(1));
        if (w, h) == (self.w, self.h) || self.w == 0 || self.h == 0 {
            return self.clone();
        }
        // horizontal pass into f32 channels, then vertical pass
        let (sw, sh) = (self.w as usize, self.h as usize);
        let (dw, dh) = (w as usize, h as usize);
        let mut tmp = vec![[0f32; 4]; dw * sh];
        let kx = sw as f32 / dw as f32;
        for y in 0..sh {
            let row = &self.px[y * sw..(y + 1) * sw];
            for x in 0..dw {
                tmp[y * dw + x] = box_avg(|i| row[i], x as f32 * kx, (x + 1) as f32 * kx, sw);
            }
        }
        let mut px = vec![0u32; dw * dh];
        let ky = sh as f32 / dh as f32;
        for x in 0..dw {
            for y in 0..dh {
                let a = box_avg_f(|i| tmp[i * dw + x], y as f32 * ky, (y + 1) as f32 * ky, sh);
                let c = |v: f32| (v + 0.5).clamp(0.0, 255.0) as u32;
                px[y * dw + x] = c(a[3]) << 24 | c(a[2]) << 16 | c(a[1]) << 8 | c(a[0]);
            }
        }
        Bitmap { w, h, px }
    }

    /// This bitmap fitted inside `max` x `max` (aspect kept); unchanged when
    /// it already fits.
    pub fn fit_within(self, max: u32) -> Bitmap {
        let m = self.w.max(self.h);
        if m <= max || max == 0 {
            return self;
        }
        let f = max as f32 / m as f32;
        self.scaled(((self.w as f32 * f).round() as u32).max(1), ((self.h as f32 * f).round() as u32).max(1))
    }
}

/// Weighted average of the source pixels covering `[a, b)` (fractional edges
/// weighted by overlap). Channels are 0 = blue ... 3 = alpha.
fn box_avg(get: impl Fn(usize) -> u32, a: f32, b: f32, n: usize) -> [f32; 4] {
    box_avg_f(|i| {
        let p = get(i);
        [(p & 255) as f32, ((p >> 8) & 255) as f32, ((p >> 16) & 255) as f32, (p >> 24) as f32]
    }, a, b, n)
}

fn box_avg_f(get: impl Fn(usize) -> [f32; 4], a: f32, b: f32, n: usize) -> [f32; 4] {
    let (i0, i1) = (a.floor() as usize, (b.ceil() as usize).min(n));
    let mut acc = [0f32; 4];
    let mut wsum = 0f32;
    for i in i0..i1.max(i0 + 1).min(n) {
        let w = (b.min(i as f32 + 1.0) - a.max(i as f32)).max(0.0);
        let v = get(i);
        for k in 0..4 {
            acc[k] += v[k] * w;
        }
        wsum += w;
    }
    if wsum > 0.0 {
        for v in &mut acc {
            *v /= wsum;
        }
    }
    acc
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
    fn scaling_averages_and_keeps_premultiplied() {
        // 4x2 -> 2x1: each output pixel is the mean of a 2x2 block
        let b = Bitmap { w: 4, h: 2, px: vec![0xff00_0000, 0xffff_ffff, 0x0000_0000, 0x8040_4040, 0xff00_0000, 0xffff_ffff, 0x0000_0000, 0x8040_4040] };
        let s = b.scaled(2, 1);
        assert_eq!((s.w, s.h), (2, 1));
        assert_eq!(s.px[0], 0xff80_8080, "black + white = grey, opaque");
        assert_eq!(s.px[1], 0x4020_2020, "transparent + half alpha");
        // same size is a copy, fit_within keeps aspect
        assert_eq!(b.scaled(4, 2).px, b.px);
        let f = Bitmap { w: 256, h: 128, px: vec![0xff11_2233; 256 * 128] }.fit_within(64);
        assert_eq!((f.w, f.h), (64, 32));
        assert!(f.px.iter().all(|&p| p == 0xff11_2233), "flat colour stays flat");
        let small = Bitmap { w: 8, h: 8, px: vec![0; 64] }.fit_within(64);
        assert_eq!((small.w, small.h), (8, 8), "never upscaled");
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
