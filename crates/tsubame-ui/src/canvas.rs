//! The drawing surface the kit draws through. One trait, 17 calls; Direct2D
//! (`tsubame-d2d`) is the first backend, `TestCanvas` the test one.

use crate::geom::{Color, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Normal,
    Semibold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// the UI face (Segoe UI Variable on Windows)
    Ui,
    /// monospace (Cascadia Mono)
    Mono,
    /// the icon font (Segoe Fluent Icons)
    Icon,
}

/// A text style. `Copy` and hashable, so backends cache formats by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Font {
    /// size in tenths of a DIP (hashable)
    pub size10: u32,
    pub weight: Weight,
    pub face: Face,
    pub align: Align,
}

/// A `size` DIP UI font, left-aligned, normal weight.
pub const fn f(size: f32) -> Font {
    Font { size10: (size * 10.0) as u32, weight: Weight::Normal, face: Face::Ui, align: Align::Left }
}

impl Font {
    pub fn size(&self) -> f32 {
        self.size10 as f32 / 10.0
    }
    /// semibold
    pub const fn b(mut self) -> Self {
        self.weight = Weight::Semibold;
        self
    }
    /// monospace
    pub const fn m(mut self) -> Self {
        self.face = Face::Mono;
        self
    }
    /// the icon face
    pub const fn icon(mut self) -> Self {
        self.face = Face::Icon;
        self
    }
    /// right-aligned
    pub const fn r(mut self) -> Self {
        self.align = Align::Right;
        self
    }
    /// centred
    pub const fn c(mut self) -> Self {
        self.align = Align::Center;
        self
    }
    pub const fn left(mut self) -> Self {
        self.align = Align::Left;
        self
    }
}

/// Everything the kit and the views draw with.
///
/// Rules every backend follows:
/// - Coordinates are DIPs (`f32`); the backend applies the DPI.
/// - Colours are `0xRRGGBB`, alpha separate (`*_a` calls).
/// - `text` is one line, vertically centred in `r`, clipped to `r`, and ends
///   in an ellipsis when it does not fit. Wider boxes than the text never
///   trim, so a scrolled text field draws into a wide box under `clip`.
///   ("Clipped" allows half a DIP of antialiased ink past `r`: the Direct2D
///   backend clips only strings whose ink leaves the box, since a clip per
///   string splits its draw batch.)
/// - `image` pixels are premultiplied BGRA, `w * h` of them. The backend may
///   keep a device copy per `key` until its target goes; the same key must
///   always mean the same pixels.
/// - `clip` / `unclip` nest (a stack).
pub trait Canvas {
    /// Drawable size in DIPs.
    fn size(&self) -> (f32, f32);
    /// Starts a frame cleared to `bg`. False: nothing can be drawn now (no
    /// target); skip the frame.
    fn begin(&mut self, bg: Color) -> bool;
    fn end(&mut self);

    fn fill(&mut self, r: Rect, c: Color) {
        self.fill_a(r, c, 1.0)
    }
    fn fill_a(&mut self, r: Rect, c: Color, a: f32);
    fn round(&mut self, r: Rect, radius: f32, c: Color) {
        self.round_a(r, radius, c, 1.0)
    }
    fn round_a(&mut self, r: Rect, radius: f32, c: Color, a: f32);
    /// Outline of width `w`, drawn inside `r`.
    fn round_stroke(&mut self, r: Rect, radius: f32, c: Color, w: f32);
    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, w: f32);
    fn circle(&mut self, x: f32, y: f32, radius: f32, c: Color);

    /// One line of text, see the trait rules.
    fn text(&mut self, s: &str, r: Rect, font: Font, c: Color);
    /// Advance width of `s` in DIPs (trailing spaces included).
    fn measure(&mut self, s: &str, font: Font) -> f32;
    /// X offset (from the text origin) of the caret before byte `idx` of `s`.
    /// The default measures the prefix; a backend may hit-test the shaped run.
    fn caret_x(&mut self, s: &str, font: Font, idx: usize) -> f32 {
        let idx = floor_char(s, idx);
        if idx == 0 { 0.0 } else { self.measure(&s[..idx], font) }
    }
    /// Byte index of the caret position closest to `x` (from the text origin).
    fn hit_index(&mut self, s: &str, font: Font, x: f32) -> usize {
        if x <= 0.0 || s.is_empty() {
            return 0;
        }
        // binary search over char boundaries on prefix widths
        let bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).chain(std::iter::once(s.len())).collect();
        let (mut lo, mut hi) = (0usize, bounds.len() - 1);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.caret_x(s, font, bounds[mid]) <= x { lo = mid } else { hi = mid - 1 }
        }
        // lo: last boundary at or left of x; pick the nearer of lo / lo+1
        if lo + 1 < bounds.len() {
            let a = self.caret_x(s, font, bounds[lo]);
            let b = self.caret_x(s, font, bounds[lo + 1]);
            if x - a > b - x {
                return bounds[lo + 1];
            }
        }
        bounds[lo]
    }

    /// Byte length of the first line of `s` wrapped to `w` DIPs: `s.len()`
    /// when it fits one line, else the end of the line (a break after a
    /// separator when one lies in the second half, else mid-word). File names
    /// in the grid use it to show two lines. A backend may ask the shaper.
    fn first_line_len(&mut self, s: &str, font: Font, w: f32) -> usize {
        if s.is_empty() || self.measure(s, font) <= w {
            return s.len();
        }
        let bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).skip(1).collect();
        // longest prefix (in chars) that fits
        let (mut lo, mut hi) = (0usize, bounds.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.measure(&s[..bounds[mid - 1]], font) <= w { lo = mid } else { hi = mid - 1 }
        }
        let cut = if lo == 0 { bounds.first().copied().unwrap_or(s.len()) } else { bounds[lo - 1] };
        // prefer a break after a separator in the second half of the line
        let head = &s[..cut];
        match head.rfind([' ', '-', '_', '.']) {
            Some(i) if i + 1 >= cut / 2 && i + 1 < s.len() => i + 1,
            _ => cut,
        }
    }

    /// Premultiplied BGRA pixels scaled into `r`, cached by `key`.
    fn image(&mut self, key: &str, w: u32, h: u32, px: &[u32], r: Rect);
    /// `image` at opacity `a` (0..1); the thumbnail fade-in uses it. The
    /// default ignores `a` for backends that cannot blend.
    fn image_a(&mut self, key: &str, w: u32, h: u32, px: &[u32], r: Rect, a: f32) {
        let _ = a;
        self.image(key, w, h, px, r)
    }
    /// Whether the backend already holds a device copy for `key`, so drawing
    /// it costs no upload. A view that draws many new images in one frame
    /// uses this to spread the uploads over frames. Default: always.
    fn has_image(&self, key: &str) -> bool {
        let _ = key;
        true
    }

    fn clip(&mut self, r: Rect);
    fn unclip(&mut self);
}

/// Largest char boundary at or below `i`.
pub fn floor_char(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
