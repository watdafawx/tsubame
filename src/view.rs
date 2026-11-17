//! Details-view list: state, input, and drawing. All coordinates are DIPs
//! (logical px); the render target applies the DPI scale.
//!
//! Only the rows that intersect the viewport are fetched and drawn.

use crate::data::{self, KINDS, N, RowBuf, Source};
use tsubame_d2d::{Font, Gfx, Rect, f};

pub const ROW_H: f32 = 22.0;
pub const HEADER_H: f32 = 26.0;
pub const STATUS_H: f32 = 22.0;
pub const SB_W: f32 = 14.0;
const SB_MIN_THUMB: f32 = 24.0;

const COL_SIZE_W: f32 = 96.0;
const COL_MOD_W: f32 = 150.0;
const COL_TYPE_W: f32 = 150.0;
const PAD: f32 = 10.0;
const NAME_X: f32 = 34.0;

const BG: u32 = 0x1b1f27;
const BG_HEADER: u32 = 0x222731;
const BG_STATUS: u32 = 0x222731;
const SEP: u32 = 0x2d3340;
const TEXT: u32 = 0xdde1e8;
const DIM: u32 = 0x8f97a8;
const SEL: u32 = 0x2d4f86;
const SB_TRACK: u32 = 0x20252e;
const SB_THUMB: u32 = 0x4b5468;
const SB_THUMB_DRAG: u32 = 0x6b7690;
const ICON_DIR: u32 = 0xe6b84d;
const ICON_FILE: u32 = 0x6f7a91;

const FONT: Font = f(12.0);
const FONT_R: Font = f(12.0).r();
const FONT_HDR: Font = f(11.0).b();
const FONT_HDR_R: Font = f(11.0).b().r();

pub struct View {
    /// content offset in DIPs. f64: 1M rows * 22 DIP = 22M, and f32 only has
    /// whole-pixel precision up to 16.7M.
    pub scroll: f64,
    pub selected: usize,
    pub w: f32,
    pub h: f32,
    pub source: Source,
    drag_grab: Option<f32>,
    text: String,
    /// rows drawn by the last paint (first, last+1)
    pub last_range: (usize, usize),
}

impl View {
    pub fn new(source: Source) -> Self {
        View { scroll: 0.0, selected: 0, w: 1120.0, h: 720.0, source, drag_grab: None, text: String::with_capacity(32), last_range: (0, 0) }
    }

    pub fn viewport_h(&self) -> f32 {
        (self.h - HEADER_H - STATUS_H).max(ROW_H)
    }
    fn content_h(&self) -> f64 {
        N as f64 * ROW_H as f64
    }
    pub fn max_scroll(&self) -> f64 {
        (self.content_h() - self.viewport_h() as f64).max(0.0)
    }
    pub fn page_rows(&self) -> usize {
        ((self.viewport_h() / ROW_H) as usize).saturating_sub(1).max(1)
    }
    pub fn set_scroll(&mut self, s: f64) -> bool {
        let s = s.clamp(0.0, self.max_scroll());
        let changed = s != self.scroll;
        self.scroll = s;
        changed
    }

    // ---- scrollbar geometry ----
    fn track(&self) -> Rect {
        Rect::new(self.w - SB_W, HEADER_H, SB_W, self.viewport_h())
    }
    /// (thumb top, thumb height)
    fn thumb(&self) -> (f32, f32) {
        let t = self.track();
        let ratio = (self.viewport_h() as f64 / self.content_h()).clamp(0.0, 1.0) as f32;
        let th = (t.h * ratio).max(SB_MIN_THUMB).min(t.h);
        let max = self.max_scroll();
        let pos = if max > 0.0 { (self.scroll / max) as f32 } else { 0.0 };
        (t.y + pos * (t.h - th), th)
    }

    // ---- input ----
    pub fn wheel(&mut self, notches: f32) -> bool {
        self.set_scroll(self.scroll - notches as f64 * 3.0 * ROW_H as f64)
    }

    pub fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        let t = self.track();
        if t.contains(x, y) {
            let (ty, th) = self.thumb();
            if y >= ty && y < ty + th {
                self.drag_grab = Some(y - ty);
                return true;
            }
            // click on the track: one page towards the click
            let page = (self.page_rows() as f32 * ROW_H) as f64;
            return self.set_scroll(if y < ty { self.scroll - page } else { self.scroll + page });
        }
        if y >= HEADER_H && y < HEADER_H + self.viewport_h() && x < self.w - SB_W {
            let row = ((self.scroll + (y - HEADER_H) as f64) / ROW_H as f64) as usize;
            if row < N && row != self.selected {
                self.selected = row;
                return true;
            }
        }
        false
    }

    pub fn dragging(&self) -> bool {
        self.drag_grab.is_some()
    }

    pub fn mouse_move(&mut self, y: f32) -> bool {
        let Some(grab) = self.drag_grab else { return false };
        let t = self.track();
        let (_, th) = self.thumb();
        let span = (t.h - th).max(1.0);
        let frac = ((y - grab - t.y) / span).clamp(0.0, 1.0) as f64;
        self.set_scroll(frac * self.max_scroll())
    }

    pub fn mouse_up(&mut self) -> bool {
        self.drag_grab.take().is_some()
    }

    fn select(&mut self, row: usize) -> bool {
        let row = row.min(N - 1);
        let moved = row != self.selected;
        self.selected = row;
        // keep it on screen
        let top = row as f64 * ROW_H as f64;
        let vh = self.viewport_h() as f64;
        let scrolled = if top < self.scroll {
            self.set_scroll(top)
        } else if top + ROW_H as f64 > self.scroll + vh {
            self.set_scroll(top + ROW_H as f64 - vh)
        } else {
            false
        };
        moved || scrolled
    }

    /// `vk` is a Win32 virtual-key code.
    pub fn key(&mut self, vk: u16) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;
        let page = self.page_rows();
        let sel = self.selected;
        match VIRTUAL_KEY(vk) {
            VK_UP => self.select(sel.saturating_sub(1)),
            VK_DOWN => self.select(sel + 1),
            VK_PRIOR => self.select(sel.saturating_sub(page)),
            VK_NEXT => self.select(sel + page),
            VK_HOME => self.select(0),
            VK_END => self.select(N - 1),
            _ => false,
        }
    }

    // ---- drawing ----
    /// One frame. Returns false when there is no render target.
    pub fn paint(&mut self, g: &mut Gfx) -> bool {
        let (w, h) = g.size();
        self.w = w;
        self.h = h;
        // size changed under us (window shrank): keep scroll valid
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        if !g.begin(BG) {
            return false;
        }

        let vh = self.viewport_h();
        let list_w = w - SB_W;
        let type_x = list_w - COL_TYPE_W;
        let mod_x = type_x - COL_MOD_W;
        let size_x = mod_x - COL_SIZE_W;
        let name_w = (size_x - NAME_X - PAD).max(40.0);

        // header
        g.fill(Rect::new(0.0, 0.0, w, HEADER_H), BG_HEADER);
        g.fill(Rect::new(0.0, HEADER_H - 1.0, w, 1.0), SEP);
        let hdr_h = HEADER_H - 1.0;
        g.text("Name", Rect::new(NAME_X, 0.0, name_w, hdr_h), FONT_HDR, DIM);
        g.text("Size", Rect::new(size_x, 0.0, COL_SIZE_W - PAD, hdr_h), FONT_HDR_R, DIM);
        g.text("Modified", Rect::new(mod_x + PAD, 0.0, COL_MOD_W - PAD, hdr_h), FONT_HDR, DIM);
        g.text("Type", Rect::new(type_x + PAD, 0.0, COL_TYPE_W - PAD, hdr_h), FONT_HDR, DIM);

        // rows: only those that intersect the viewport
        let first = (self.scroll / ROW_H as f64) as usize;
        let off = (self.scroll - first as f64 * ROW_H as f64) as f32;
        let count = ((vh + off) / ROW_H).ceil() as usize + 0;
        let last = (first + count).min(N);
        self.last_range = (first, last);

        g.clip(Rect::new(0.0, HEADER_H, w, vh));
        let mut row = RowBuf::new();
        for i in first..last {
            let y = HEADER_H + (i - first) as f32 * ROW_H - off;
            let r = Rect::new(0.0, y, list_w, ROW_H);
            if i == self.selected {
                g.fill(r, SEL);
            }
            self.source.row(i, &mut row);
            // icon stand-in: folder = amber tab, file = grey page
            let (ic, icol) = if row.is_dir() { (Rect::new(12.0, y + 6.0, 14.0, 10.0), ICON_DIR) } else { (Rect::new(14.0, y + 4.0, 10.0, 14.0), ICON_FILE) };
            g.round(ic, 2.0, icol);
            g.text(row.name_str(), Rect::new(NAME_X, y, name_w, ROW_H), FONT, TEXT);
            if !row.is_dir() {
                data::fmt_size(row.size, &mut self.text);
                g.text(&self.text, Rect::new(size_x, y, COL_SIZE_W - PAD, ROW_H), FONT_R, DIM);
            }
            data::fmt_date(row.mtime, &mut self.text);
            g.text(&self.text, Rect::new(mod_x + PAD, y, COL_MOD_W - PAD, ROW_H), FONT, DIM);
            g.text(KINDS[row.kind as usize].1, Rect::new(type_x + PAD, y, COL_TYPE_W - PAD, ROW_H), FONT, DIM);
        }
        g.unclip();

        // scrollbar
        let t = self.track();
        g.fill(t, SB_TRACK);
        let (ty, th) = self.thumb();
        g.round(Rect::new(t.x + 3.0, ty, t.w - 6.0, th), 4.0, if self.dragging() { SB_THUMB_DRAG } else { SB_THUMB });

        // status bar
        let sy = HEADER_H + vh;
        g.fill(Rect::new(0.0, sy, w, STATUS_H), BG_STATUS);
        g.fill(Rect::new(0.0, sy, w, 1.0), SEP);
        self.text.clear();
        {
            use std::fmt::Write;
            let _ = write!(
                self.text,
                "Item {} of {}   |   showing {}-{}   |   {} data, {} render",
                self.selected + 1,
                N,
                first + 1,
                last,
                self.source.label(),
                g.path()
            );
        }
        g.text(&self.text, Rect::new(PAD, sy + 1.0, w - 2.0 * PAD, STATUS_H - 1.0), f(11.0), DIM);

        g.end();
        true
    }
}
