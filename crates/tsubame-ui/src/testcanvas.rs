//! A recording `Canvas` for unit tests. Text is measured as
//! `chars * size * 0.5`, so layouts are deterministic.

use crate::canvas::{Canvas, Font};
use crate::geom::{Color, Rect};

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Begin(Color),
    End,
    Fill(Rect, Color, f32),
    Round(Rect, f32, Color, f32),
    RoundStroke(Rect, f32, Color, f32),
    Line(f32, f32, f32, f32, Color, f32),
    Circle(f32, f32, f32, Color),
    Text(String, Rect, Font, Color),
    Image(String, Rect),
    Clip(Rect),
    Unclip,
}

pub struct TestCanvas {
    pub w: f32,
    pub h: f32,
    pub ops: Vec<Op>,
    /// `false`: record nothing (counts only)
    pub record: bool,
    pub measures: usize,
    clip_depth: i32,
}

impl TestCanvas {
    pub fn new(w: f32, h: f32) -> TestCanvas {
        TestCanvas { w, h, ops: Vec::new(), record: true, measures: 0, clip_depth: 0 }
    }
    /// Width the test canvas gives `s`.
    pub fn width_of(s: &str, font: Font) -> f32 {
        s.chars().count() as f32 * font.size() * 0.5
    }
    pub fn clear(&mut self) {
        self.ops.clear();
        self.measures = 0;
    }
    /// Every text drawn, in order.
    pub fn texts(&self) -> Vec<&str> {
        self.ops.iter().filter_map(|o| if let Op::Text(s, ..) = o { Some(s.as_str()) } else { None }).collect()
    }
    pub fn has_text(&self, s: &str) -> bool {
        self.texts().contains(&s)
    }
    pub fn clip_depth(&self) -> i32 {
        self.clip_depth
    }
    fn push(&mut self, op: Op) {
        if self.record {
            self.ops.push(op);
        }
    }
}

impl Canvas for TestCanvas {
    fn size(&self) -> (f32, f32) {
        (self.w, self.h)
    }
    fn begin(&mut self, bg: Color) -> bool {
        self.push(Op::Begin(bg));
        true
    }
    fn end(&mut self) {
        assert_eq!(self.clip_depth, 0, "clip/unclip unbalanced at end of frame");
        self.push(Op::End);
    }
    fn fill_a(&mut self, r: Rect, c: Color, a: f32) {
        self.push(Op::Fill(r, c, a));
    }
    fn round_a(&mut self, r: Rect, radius: f32, c: Color, a: f32) {
        self.push(Op::Round(r, radius, c, a));
    }
    fn round_stroke(&mut self, r: Rect, radius: f32, c: Color, w: f32) {
        self.push(Op::RoundStroke(r, radius, c, w));
    }
    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, w: f32) {
        self.push(Op::Line(x0, y0, x1, y1, c, w));
    }
    fn circle(&mut self, x: f32, y: f32, radius: f32, c: Color) {
        self.push(Op::Circle(x, y, radius, c));
    }
    fn text(&mut self, s: &str, r: Rect, font: Font, c: Color) {
        self.push(Op::Text(s.to_string(), r, font, c));
    }
    fn measure(&mut self, s: &str, font: Font) -> f32 {
        self.measures += 1;
        Self::width_of(s, font)
    }
    fn image(&mut self, key: &str, _w: u32, _h: u32, _px: &[u32], r: Rect) {
        self.push(Op::Image(key.to_string(), r));
    }
    fn clip(&mut self, r: Rect) {
        self.clip_depth += 1;
        self.push(Op::Clip(r));
    }
    fn unclip(&mut self) {
        self.clip_depth -= 1;
        assert!(self.clip_depth >= 0, "unclip without clip");
        self.push(Op::Unclip);
    }
}
