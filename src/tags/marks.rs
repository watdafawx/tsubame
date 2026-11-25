//! Tag marks as the mock draws them: eight shapes on a 10x10 grid (colour is
//! never the only cue), a plain-dot mode, the dashed "reading…" mark, mark
//! clusters after a name and chips for the Tags column.
//!
//! The canvas has no paths, so each mark is rasterized once on the CPU
//! (4x4 supersampled, premultiplied BGRA) at the device size and handed to
//! `Canvas::image` under a stable key; the backend keeps the device copy.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use tsubame_ui::{Canvas, Rect, Theme, f};

use super::model::{Shape, TagDef};

/// Mark size after a name (DIPs), gap between marks, gap before a cluster.
pub const MARK: f32 = 10.0;
pub const GAP: f32 = 2.0;
pub const LEAD: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    Shape(Shape),
    /// "show shapes" off: a plain dot
    Dot,
    /// tags not read yet (dashed ring)
    Pending,
}

impl Mark {
    fn code(self) -> u8 {
        match self {
            Mark::Shape(s) => s as u8,
            Mark::Dot => 20,
            Mark::Pending => 21,
        }
    }
}

/// (mark, colour, size px) -> (key, BGRA pixels)
type Rasters = HashMap<(u8, u32, u16), (String, Vec<u32>)>;

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    static CACHE: RefCell<Rasters> = RefCell::new(HashMap::new());
}

/// Device pixels per DIP (the view sets it from the window DPI).
pub fn set_scale(s: f32) {
    SCALE.with(|c| c.set(if s.is_finite() && s > 0.25 { s } else { 1.0 }));
}

type Poly = &'static [(f32, f32)];
const TRI: Poly = &[(5.0, 1.0), (9.5, 9.0), (0.5, 9.0)];
const DIA: Poly = &[(5.0, 0.5), (9.5, 5.0), (5.0, 9.5), (0.5, 5.0)];
const HEX: Poly = &[(5.0, 0.6), (8.8, 2.8), (8.8, 7.2), (5.0, 9.4), (1.2, 7.2), (1.2, 2.8)];
const STAR: Poly = &[(5.0, 0.5), (6.3, 3.6), (9.6, 3.9), (7.1, 6.0), (7.9, 9.3), (5.0, 7.5), (2.1, 9.3), (2.9, 6.0), (0.4, 3.9), (3.7, 3.6)];
const PLUS: Poly = &[(3.4, 0.8), (6.6, 0.8), (6.6, 3.4), (9.2, 3.4), (9.2, 6.6), (6.6, 6.6), (6.6, 9.2), (3.4, 9.2), (3.4, 6.6), (0.8, 6.6), (0.8, 3.4), (3.4, 3.4)];

fn in_poly(p: Poly, x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut j = p.len() - 1;
    for i in 0..p.len() {
        let (xi, yi) = p[i];
        let (xj, yj) = p[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Whether a point of the 10x10 view box is ink.
fn ink(m: Mark, x: f32, y: f32) -> bool {
    let d = ((x - 5.0).powi(2) + (y - 5.0).powi(2)).sqrt();
    match m {
        Mark::Shape(Shape::Tri) => in_poly(TRI, x, y),
        Mark::Shape(Shape::Dia) => in_poly(DIA, x, y),
        Mark::Shape(Shape::Sq) => {
            // x 1..9, y 1..9, corner radius 1
            let cx = x.clamp(2.0, 8.0);
            let cy = y.clamp(2.0, 8.0);
            (1.0..=9.0).contains(&x) && (1.0..=9.0).contains(&y) && (x - cx).powi(2) + (y - cy).powi(2) <= 1.0
        }
        Mark::Shape(Shape::Cir) => d <= 4.2,
        Mark::Shape(Shape::Hex) => in_poly(HEX, x, y),
        Mark::Shape(Shape::Star) => in_poly(STAR, x, y),
        Mark::Shape(Shape::Plus) => in_poly(PLUS, x, y),
        Mark::Shape(Shape::Ring) => (2.9..=4.5).contains(&d),
        Mark::Dot => d <= 3.6,
        Mark::Pending => {
            // r 4.2 stroke 1, dashes 2 on 1.6 along the circumference
            if !(3.7..=4.7).contains(&d) {
                return false;
            }
            let a = (y - 5.0).atan2(x - 5.0) + std::f32::consts::PI;
            (a * 4.2) % 3.6 < 2.0
        }
    }
}

/// Premultiplied BGRA pixels of `m` in `rgb`, `n` x `n`.
pub fn raster(m: Mark, rgb: u32, n: usize) -> Vec<u32> {
    const SS: usize = 4;
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let k = 10.0 / n as f32;
    let mut out = vec![0u32; n * n];
    for py in 0..n {
        for px in 0..n {
            let mut hit = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) * k;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) * k;
                    hit += ink(m, x, y) as u32;
                }
            }
            let a = hit * 255 / (SS * SS) as u32;
            if a > 0 {
                out[py * n + px] = (a << 24) | ((r * a / 255) << 16) | ((g * a / 255) << 8) | (b * a / 255);
            }
        }
    }
    out
}

/// Draws mark `m` in `rgb`, `size` DIPs, centred on (`cx`, `cy`).
pub fn draw(g: &mut dyn Canvas, m: Mark, rgb: u32, cx: f32, cy: f32, size: f32) {
    let scale = SCALE.with(|c| c.get());
    let n = (size * scale).round().clamp(4.0, 96.0) as u16;
    let r = Rect::new(cx - size / 2.0, cy - size / 2.0, size, size);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 512 {
            c.clear();
        }
        let e = c.entry((m.code(), rgb, n)).or_insert_with(|| (format!("tg:{}:{rgb:06x}:{n}", m.code()), raster(m, rgb, n as usize)));
        g.image(&e.0, n as u32, n as u32, &e.1, r);
    });
}

/// The mark a tag shows.
pub fn mark_of(d: &TagDef, shapes: bool) -> Mark {
    if shapes { Mark::Shape(d.shape) } else { Mark::Dot }
}

/// Tags of `mask` in list order, at most `max`, and the count left over.
fn pick<'a>(defs: &'a [TagDef], mask: u64, max: usize) -> (smallvec::SmallVec<'a>, usize) {
    let mut v = smallvec::SmallVec::default();
    let mut more = 0;
    for d in defs.iter().filter(|d| mask & d.bit() != 0) {
        if v.len < max { v.push(d) } else { more += 1 }
    }
    (v, more)
}

/// A fixed array instead of a `Vec` so painting allocates nothing.
mod smallvec {
    use super::TagDef;
    #[derive(Default)]
    pub struct SmallVec<'a> {
        pub items: [Option<&'a TagDef>; 4],
        pub len: usize,
    }
    impl<'a> SmallVec<'a> {
        pub fn push(&mut self, d: &'a TagDef) {
            if self.len < 4 {
                self.items[self.len] = Some(d);
                self.len += 1;
            }
        }
        pub fn iter(&self) -> impl Iterator<Item = &'a TagDef> + '_ {
            self.items[..self.len].iter().flatten().copied()
        }
    }
}

fn more_text(n: usize, buf: &mut String) {
    use std::fmt::Write;
    buf.clear();
    let _ = write!(buf, "+{n}");
}

/// Width of the cluster `draw_cluster` would draw (0: no tags).
pub fn cluster_w(g: &mut dyn Canvas, _t: &Theme, defs: &[TagDef], mask: u64, max: usize, buf: &mut String) -> f32 {
    let (v, more) = pick(defs, mask, max);
    if v.len == 0 {
        return 0.0;
    }
    let mut w = LEAD + v.len as f32 * MARK + (v.len - 1) as f32 * GAP;
    if more > 0 {
        more_text(more, buf);
        w += GAP + 2.0 + g.measure(buf, f(10.0));
    }
    w
}

/// Marks of `mask` from `x`, centred on `cy` (up to `max`, then "+N").
/// Returns the width used.
#[allow(clippy::too_many_arguments)]
pub fn draw_cluster(g: &mut dyn Canvas, t: &Theme, defs: &[TagDef], mask: u64, shapes: bool, x: f32, cy: f32, max: usize, size: f32, gap: f32, buf: &mut String) -> f32 {
    let (v, more) = pick(defs, mask, max);
    let mut cx = x;
    for d in v.iter() {
        draw(g, mark_of(d, shapes), d.rgb(t.is_dark()), cx + size / 2.0, cy, size);
        cx += size + gap;
    }
    if more > 0 {
        more_text(more, buf);
        let w = g.measure(buf, f(10.0));
        g.text(buf, Rect::new(cx, cy - 7.0, w + 2.0, 14.0), f(10.0), t.text_dim);
        cx += w + 2.0;
    }
    (cx - x - if more > 0 { 0.0 } else { gap }).max(0.0)
}

/// The Tags column: up to `max` chips (border and mark in the tag colour,
/// name in the text colour), then "+N", clipped to `r`.
#[allow(clippy::too_many_arguments)]
pub fn draw_chips(g: &mut dyn Canvas, t: &Theme, defs: &[TagDef], mask: u64, shapes: bool, r: Rect, max: usize, buf: &mut String) {
    let (v, more) = pick(defs, mask, max);
    if v.len == 0 {
        return;
    }
    let font = f(10.0);
    let cy = r.y + r.h / 2.0;
    let mut x = r.x;
    let right = r.r();
    // the "+N" keeps its room; the last chip shrinks (ellipsis) first
    let more_w = if more > 0 {
        more_text(more, buf);
        g.measure(buf, font) + 4.0
    } else {
        0.0
    };
    for (i, d) in v.iter().enumerate() {
        let room = right - x - if i + 1 == v.len { more_w } else { 0.0 };
        if room < 24.0 {
            break;
        }
        let nw = g.measure(&d.name, font);
        let w = (4.0 + MARK + 4.0 + nw + 2.0 + 5.0).min(room);
        let chip = Rect::new(x, cy - 8.0, w, 16.0);
        let c = d.rgb(t.is_dark());
        g.round_stroke(chip, 8.0, c, 1.0);
        draw(g, mark_of(d, shapes), c, chip.x + 4.0 + MARK / 2.0, cy, MARK);
        g.text(&d.name, Rect::new(chip.x + 4.0 + MARK + 4.0, chip.y, (w - 4.0 - MARK - 4.0 - 3.0).max(0.0), chip.h), font, t.text);
        x += w + 4.0;
    }
    if more > 0 {
        more_text(more, buf);
        g.text(buf, Rect::new(x, r.y, (right - x).max(0.0), r.h), font, t.text_dim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(px: &[u32], n: usize, x: usize, y: usize) -> u32 {
        px[y * n + x] >> 24
    }

    #[test]
    fn shapes_rasterize_distinctly_and_premultiplied() {
        let n = 20;
        let all: Vec<Vec<u32>> = Shape::ALL.iter().map(|&s| raster(Mark::Shape(s), 0x6aa0ff, n)).collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two shapes look the same");
            }
            for &p in a {
                let al = p >> 24;
                for sh in [0, 8, 16] {
                    assert!((p >> sh) & 0xff <= al, "not premultiplied");
                }
            }
        }
        // ring: hollow centre, filled band; circle: filled centre
        let ring = &all[Shape::Ring as usize];
        assert_eq!(alpha(ring, n, 10, 10), 0);
        assert!(alpha(ring, n, 10, 2) > 200);
        let cir = &all[Shape::Cir as usize];
        assert_eq!(alpha(cir, n, 10, 10), 255);
        // triangle points up: top corners empty, bottom row wide
        let tri = &all[Shape::Tri as usize];
        assert_eq!(alpha(tri, n, 1, 3), 0);
        assert!(alpha(tri, n, 10, 16) == 255 && alpha(tri, n, 3, 16) > 0);
        // pending is a dashed ring: some band pixels are empty
        let p = raster(Mark::Pending, 0x888888, 40);
        let band: Vec<u32> = (0..40).map(|x| alpha(&p, 40, x, 3)).collect();
        assert!(band.contains(&0) && band.iter().any(|&a| a > 0));
    }

    #[test]
    fn pick_caps_and_counts_the_rest() {
        let defs = super::super::model::defaults();
        let (v, more) = pick(&defs, 0b1111_1111, 3);
        assert_eq!((v.len, more), (3, 5));
        assert_eq!(v.iter().map(|d| d.slot).collect::<Vec<_>>(), [0, 1, 2]);
        let (v, more) = pick(&defs, 0, 3);
        assert_eq!((v.len, more), (0, 0));
    }
}
