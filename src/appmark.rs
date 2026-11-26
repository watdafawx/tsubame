//! The app mark (option A, top-down swallow on a navy tile), drawn from the same
//! shapes as `res/tsubame.svg` (big cut, 100-unit grid) and `res/tsubame-small.svg`
//! (small cut, 16-unit integer grid). Used by the title bar and the Updates page
//! so they match the exe icon. The shapes are rasterised here with 4x4
//! supersampling into a premultiplied BGRA bitmap, cached per pixel size, so the
//! mark is crisp at any DPI and costs no allocation after the first draw.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use tsubame_ui::theme::{BRAND_CREAM, BRAND_NAVY, BRAND_RED};
use tsubame_ui::{Canvas, Rect};

/// Right half of the big-cut swallow (100-unit grid, as in `res/tsubame.svg`).
const BIG_HALF: &str = "M50 11C54.5 11 57 15.5 57 22L58 30C71 28 86 35 96 51C83 46 70 47 60 52C61 63 63 77 67 93C58 85 53 78 50 72Z";
/// Small-cut polygon (16-unit grid, as in `res/tsubame-small.svg`).
const SMALL_POLY: [(f32, f32); 12] = [(8.0, 2.0), (9.0, 3.0), (9.0, 4.0), (15.0, 11.0), (10.0, 9.0), (11.0, 14.0), (8.0, 11.0), (5.0, 14.0), (6.0, 9.0), (1.0, 11.0), (7.0, 4.0), (7.0, 3.0)];
/// The small cut is used up to this many device pixels (the ico plan: 16/20/24).
pub const SMALL_MAX_PX: u32 = 24;

type Poly = Vec<(f32, f32)>;

/// Flattens absolute `M/L/C/Z` path data into a polygon.
fn flatten(d: &str) -> Poly {
    let mut nums: Vec<f32> = Vec::new();
    let mut ops: Vec<(char, usize)> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, nums: &mut Vec<f32>| {
        if let Ok(v) = cur.parse::<f32>() {
            nums.push(v);
        }
        cur.clear();
    };
    for ch in d.chars() {
        if ch.is_ascii_alphabetic() {
            flush(&mut cur, &mut nums);
            ops.push((ch, nums.len()));
        } else if ch == ' ' || ch == ',' {
            flush(&mut cur, &mut nums);
        } else {
            cur.push(ch);
        }
    }
    flush(&mut cur, &mut nums);
    let mut out: Poly = Vec::new();
    let mut at = (0.0f32, 0.0f32);
    for (i, &(op, start)) in ops.iter().enumerate() {
        let end = ops.get(i + 1).map_or(nums.len(), |o| o.1);
        let a = &nums[start..end];
        match op {
            'M' | 'L' => {
                at = (a[0], a[1]);
                out.push(at);
            }
            'C' => {
                for s in a.as_chunks::<6>().0 {
                    let (p0, p1, p2, p3) = (at, (s[0], s[1]), (s[2], s[3]), (s[4], s[5]));
                    for k in 1..=12 {
                        let t = k as f32 / 12.0;
                        let u = 1.0 - t;
                        let b = |a0: f32, a1: f32, a2: f32, a3: f32| u * u * u * a0 + 3.0 * u * u * t * a1 + 3.0 * u * t * t * a2 + t * t * t * a3;
                        out.push((b(p0.0, p1.0, p2.0, p3.0), b(p0.1, p1.1, p2.1, p3.1)));
                    }
                    at = p3;
                }
            }
            _ => {}
        }
    }
    out
}

fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut c = false;
    let mut q = poly[poly.len() - 1];
    for &p in poly {
        if (p.1 > y) != (q.1 > y) && x < (q.0 - p.0) * (y - p.1) / (q.1 - p.1) + p.0 {
            c = !c;
        }
        q = p;
    }
    c
}

fn in_tile(units: f32, rx: f32, x: f32, y: f32) -> bool {
    if x < 0.0 || y < 0.0 || x > units || y > units {
        return false;
    }
    let cx = x.clamp(rx, units - rx);
    let cy = y.clamp(rx, units - rx);
    (x - cx).powi(2) + (y - cy).powi(2) <= rx * rx
}

/// Colour (0xRRGGBB) of one sample point, or None outside the tile.
fn shade(big: bool, bird: &[(f32, f32)], x: f32, y: f32) -> Option<u32> {
    if big {
        if !in_tile(100.0, 22.0, x, y) {
            return None;
        }
        // bird group: translate(9 8) scale(.82); right half plus its mirror
        let (gx, gy) = ((x - 9.0) / 0.82, (y - 8.0) / 0.82);
        if (gx - 50.0).powi(2) + (gy - 26.0).powi(2) <= 3.6 * 3.6 {
            return Some(BRAND_RED);
        }
        let hit = inside(bird, gx, gy) || inside(bird, 100.0 - gx, gy);
        Some(if hit { BRAND_CREAM } else { BRAND_NAVY })
    } else {
        if !in_tile(16.0, 3.5, x, y) {
            return None;
        }
        Some(if inside(bird, x, y) { BRAND_CREAM } else { BRAND_NAVY })
    }
}

/// Premultiplied BGRA pixels (`0xAARRGGBB`) of the mark at `px` x `px`.
pub fn rasterize(px: u32, big: bool) -> Vec<u32> {
    let bird = if big { flatten(BIG_HALF) } else { SMALL_POLY.to_vec() };
    let units = if big { 100.0 } else { 16.0 };
    const SS: u32 = 4;
    let n = px as usize;
    let mut out = Vec::with_capacity(n * n);
    let step = units / (px * SS) as f32;
    for y in 0..px {
        for x in 0..px {
            let (mut a, mut r, mut g, mut b) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = ((x * SS + sx) as f32 + 0.5) * step;
                    let fy = ((y * SS + sy) as f32 + 0.5) * step;
                    if let Some(c) = shade(big, &bird, fx, fy) {
                        a += 255;
                        r += (c >> 16) & 0xff;
                        g += (c >> 8) & 0xff;
                        b += c & 0xff;
                    }
                }
            }
            // premultiplied: summing only covered samples folds coverage into the colour
            let s = SS * SS;
            out.push(((a / s) << 24) | ((r / s) << 16) | ((g / s) << 8) | (b / s));
        }
    }
    out
}

struct Entry {
    px: u32,
    big: bool,
    key: String,
    pix: Vec<u32>,
}

thread_local! {
    static CACHE: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

/// Display DPI the mark is rasterised for; the title bar refreshes it each frame
/// (the canvas does not expose it, and the Updates page has no view handle).
static DPI: AtomicU32 = AtomicU32::new(96);

pub fn set_dpi(dpi: f32) {
    DPI.store(dpi.max(48.0) as u32, Ordering::Relaxed);
}

/// Draws the mark into `r` at the current display DPI (96 = 100%). Picks the
/// small cut for 24 device px and below, else the big cut.
pub fn draw(g: &mut dyn Canvas, r: Rect) {
    let dpi = DPI.load(Ordering::Relaxed) as f32;
    let px = ((r.w.min(r.h) * dpi / 96.0).round() as u32).clamp(8, 512);
    let big = px > SMALL_MAX_PX;
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let i = match c.iter().position(|e| e.px == px && e.big == big) {
            Some(i) => i,
            None => {
                c.push(Entry { px, big, key: format!("appmark:{px}:{}", u8::from(big)), pix: rasterize(px, big) });
                c.len() - 1
            }
        };
        let e = &c[i];
        let side = px as f32 * 96.0 / dpi;
        let dst = Rect::new(r.x + (r.w - side) / 2.0, r.y + (r.h - side) / 2.0, side, side);
        g.image(&e.key, px, px, &e.pix, dst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_transparent_center_opaque() {
        for big in [false, true] {
            let n = 32usize;
            let px = rasterize(n as u32, big);
            assert_eq!(px.len(), n * n);
            assert_eq!(px[0] >> 24, 0, "corner is transparent (big={big})");
            assert_eq!(px[n / 2 * n + n / 2] >> 24, 255, "centre opaque (big={big})");
        }
    }

    #[test]
    fn bird_is_symmetric_and_cream() {
        let n = 64usize;
        let px = rasterize(n as u32, true);
        for row in px.chunks(n) {
            let mirrored: Vec<u32> = row.iter().rev().copied().collect();
            assert_eq!(row, &mirrored[..], "rows mirror");
        }
        assert_eq!(px[40 * n + 32] & 0xff_ffff, BRAND_CREAM);
    }

    #[test]
    fn red_throat_only_in_big_cut() {
        let has_red = |px: &[u32]| px.iter().any(|p| (p & 0xff_ffff) == BRAND_RED);
        assert!(has_red(&rasterize(96, true)));
        assert!(!has_red(&rasterize(16, false)));
    }

    #[test]
    fn small_polygon_matches_svg() {
        let svg = include_str!("../res/tsubame-small.svg");
        let pts = svg.split("points=\"").nth(1).unwrap().split('"').next().unwrap();
        let parsed: Vec<(f32, f32)> = pts
            .split_whitespace()
            .map(|p| {
                let (a, b) = p.split_once(',').unwrap();
                (a.parse().unwrap(), b.parse().unwrap())
            })
            .collect();
        assert_eq!(parsed, SMALL_POLY.to_vec());
    }

    #[test]
    fn big_half_matches_svg() {
        let svg = include_str!("../res/tsubame.svg");
        assert!(svg.contains(&BIG_HALF[..BIG_HALF.len() - 1]), "svg carries the half path");
    }

    #[test]
    fn ico_has_plan_sizes_and_rc_uses_id_1() {
        let ico = include_bytes!("../res/tsubame.ico");
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        let sizes: Vec<u32> = (0..count).map(|i| match ico[6 + 16 * i] { 0 => 256, w => w as u32 }).collect();
        for want in [16, 20, 24, 32, 48, 256] {
            assert!(sizes.contains(&want), "ico has {want}px frame, got {sizes:?}");
        }
        let rc = include_str!("../res/tsubame.rc");
        assert!(rc.lines().any(|l| l.trim_start().starts_with("1 ICON")), "rc maps icon resource id 1");
    }
}
