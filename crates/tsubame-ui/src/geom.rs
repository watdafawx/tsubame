//! Geometry and layout helpers. Everything is in DIPs (logical px, `f32`);
//! the backend applies the DPI scale.

/// `0xRRGGBB`. Alpha is a separate `f32` argument where a call takes one.
pub type Color = u32;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    pub const fn new(x: f32, y: f32) -> Pt {
        Pt { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Where `Rect::anchor` places a box inside another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

/// A column/row size for `Rect::columns` / `Rect::rows_split`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Len {
    /// fixed DIPs
    Px(f32),
    /// a share of what is left after the fixed sizes (weights add up)
    Fill(f32),
}

impl Rect {
    pub const ZERO: Rect = Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };

    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
    /// Right edge.
    pub fn r(&self) -> f32 {
        self.x + self.w
    }
    /// Bottom edge.
    pub fn b(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> Pt {
        Pt::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    /// Half-open: the right and bottom edges are outside, so adjacent rects
    /// never both contain a point.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.r() && py >= self.y && py < self.b()
    }
    pub fn contains_pt(&self, p: Pt) -> bool {
        self.contains(p.x, p.y)
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.r() && o.x < self.r() && self.y < o.b() && o.y < self.b()
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        Rect::new(x, y, (self.r().min(o.r()) - x).max(0.0), (self.b().min(o.b()) - y).max(0.0))
    }
    /// Shrinks by `dx` on the left and right and `dy` on the top and bottom.
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    /// Shrinks by `p` on every side.
    pub fn pad(&self, p: f32) -> Rect {
        self.inset(p, p)
    }
    /// Shrinks by (left, top, right, bottom).
    pub fn pad4(&self, l: f32, t: f32, r: f32, b: f32) -> Rect {
        Rect::new(self.x + l, self.y + t, (self.w - l - r).max(0.0), (self.h - t - b).max(0.0))
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Cuts `h` off the top and returns it; `self` keeps the rest.
    pub fn cut_top(&mut self, h: f32) -> Rect {
        let h = h.clamp(0.0, self.h);
        let out = Rect::new(self.x, self.y, self.w, h);
        self.y += h;
        self.h -= h;
        out
    }
    pub fn cut_bottom(&mut self, h: f32) -> Rect {
        let h = h.clamp(0.0, self.h);
        self.h -= h;
        Rect::new(self.x, self.y + self.h, self.w, h)
    }
    pub fn cut_left(&mut self, w: f32) -> Rect {
        let w = w.clamp(0.0, self.w);
        let out = Rect::new(self.x, self.y, w, self.h);
        self.x += w;
        self.w -= w;
        out
    }
    pub fn cut_right(&mut self, w: f32) -> Rect {
        let w = w.clamp(0.0, self.w);
        self.w -= w;
        Rect::new(self.x + self.w, self.y, w, self.h)
    }

    /// Splits left/right at `at` DIPs from the left edge (clamped).
    pub fn split_x(&self, at: f32) -> (Rect, Rect) {
        let mut r = *self;
        let a = r.cut_left(at);
        (a, r)
    }
    /// Splits top/bottom at `at` DIPs from the top edge (clamped).
    pub fn split_y(&self, at: f32) -> (Rect, Rect) {
        let mut r = *self;
        let a = r.cut_top(at);
        (a, r)
    }

    /// A `w` x `h` box placed inside `self` at `a` (may stick out if larger).
    pub fn anchor(&self, a: Anchor, w: f32, h: f32) -> Rect {
        let x = match a {
            Anchor::TopLeft | Anchor::Left | Anchor::BottomLeft => self.x,
            Anchor::Top | Anchor::Center | Anchor::Bottom => self.x + (self.w - w) / 2.0,
            Anchor::TopRight | Anchor::Right | Anchor::BottomRight => self.r() - w,
        };
        let y = match a {
            Anchor::TopLeft | Anchor::Top | Anchor::TopRight => self.y,
            Anchor::Left | Anchor::Center | Anchor::Right => self.y + (self.h - h) / 2.0,
            Anchor::BottomLeft | Anchor::Bottom | Anchor::BottomRight => self.b() - h,
        };
        Rect::new(x, y, w, h)
    }

    /// Row `i` of fixed height `h` from the top (no clamping).
    pub fn row(&self, i: usize, h: f32) -> Rect {
        Rect::new(self.x, self.y + i as f32 * h, self.w, h)
    }

    /// Lays `lens` out left to right, `gap` between them. Fixed sizes first,
    /// `Fill` shares what is left. Writes into `out` (cleared first) so a frame
    /// can reuse one buffer.
    pub fn columns_into(&self, lens: &[Len], gap: f32, out: &mut Vec<Rect>) {
        let sizes = distribute(self.w, lens, gap);
        out.clear();
        let mut x = self.x;
        for s in sizes {
            out.push(Rect::new(x, self.y, s, self.h));
            x += s + gap;
        }
    }
    pub fn columns(&self, lens: &[Len], gap: f32) -> Vec<Rect> {
        let mut v = Vec::with_capacity(lens.len());
        self.columns_into(lens, gap, &mut v);
        v
    }
    /// Same as `columns`, top to bottom.
    pub fn rows_split(&self, lens: &[Len], gap: f32) -> Vec<Rect> {
        let sizes = distribute(self.h, lens, gap);
        let mut y = self.y;
        sizes
            .into_iter()
            .map(|s| {
                let r = Rect::new(self.x, y, self.w, s);
                y += s + gap;
                r
            })
            .collect()
    }
}

fn distribute(total: f32, lens: &[Len], gap: f32) -> impl Iterator<Item = f32> + '_ {
    let fixed: f32 = lens.iter().map(|l| if let Len::Px(p) = l { *p } else { 0.0 }).sum();
    let weights: f32 = lens.iter().map(|l| if let Len::Fill(w) = l { *w } else { 0.0 }).sum();
    let gaps = gap * lens.len().saturating_sub(1) as f32;
    let free = (total - fixed - gaps).max(0.0);
    lens.iter().map(move |l| match l {
        Len::Px(p) => *p,
        Len::Fill(w) if weights > 0.0 => free * w / weights,
        Len::Fill(_) => 0.0,
    })
}
