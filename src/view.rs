//! Details view on the `tsubame-ui` kit: a filter box, a header, the
//! virtual list with its scrollbar, the thumbnail panel and a status bar.
//! All coordinates are DIPs; the render target applies the DPI scale.
//!
//! The kit owns no rows: `Rows` (a `ListSource`) reads them from `Source`
//! through the filter index, and only the rows the list asks for are fetched
//! and drawn.

use crate::data::{self, RowBuf, Source};
use crate::shellui::ShellUi;
use std::collections::BTreeSet;
use tsubame_d2d::Gfx;
use tsubame_ui::{Canvas, Event, Font, Id, ListSource, ListState, Mods, Output, Pt, Rect, RowState, SelectBy, TextInput, Theme, Ui, f, list, text_input};

pub const ROW_H: f32 = 22.0;
pub const TOOLBAR_H: f32 = 36.0;
pub const HEADER_H: f32 = 26.0;
pub const STATUS_H: f32 = 22.0;

const COL_SIZE_W: f32 = 96.0;
const COL_MOD_W: f32 = 150.0;
const COL_TYPE_W: f32 = 150.0;
const PAD: f32 = 10.0;
const NAME_X: f32 = 34.0;
const FILTER_W: f32 = 320.0;

const ICON_DIR: u32 = 0xe6b84d;
const ICON_FILE: u32 = 0x6f7a91;
const PANEL_BORDER: u32 = 0x3a4252;
pub const ICON_DIP: f32 = 16.0;
pub const THUMB_DIP: f32 = 256.0;

const FONT_R: Font = f(12.0).r();
const FONT_HDR_R: Font = f(11.0).b().r();

pub const LIST_ID: Id = Id::new("details");
pub const FILTER_ID: Id = Id::new("filter");

/// App commands (shortcut table values).
pub const CMD_FIND: u32 = 1;

/// Explorer-style selection: one range from the anchor, XOR Ctrl-toggled
/// rows. Never one entry per selected row, so Shift+End over 1M rows is two
/// numbers. (Spike model; tsubame-core's selection replaces it.)
#[derive(Default)]
pub struct Selection {
    anchor: usize,
    range: Option<(usize, usize)>,
    toggled: BTreeSet<usize>,
}

impl Selection {
    pub fn contains(&self, i: usize) -> bool {
        self.range.is_some_and(|(a, b)| i >= a && i <= b) ^ self.toggled.contains(&i)
    }
    pub fn clear(&mut self) {
        self.range = None;
        self.toggled.clear();
    }
    pub fn single(&mut self, i: usize) {
        self.range = Some((i, i));
        self.toggled.clear();
        self.anchor = i;
    }
    fn toggle(&mut self, i: usize) {
        if !self.toggled.remove(&i) {
            self.toggled.insert(i);
        }
        self.anchor = i;
    }
    pub fn apply(&mut self, i: usize, mods: Mods, by: SelectBy) {
        match by {
            SelectBy::Click | SelectBy::Key if mods.shift => {
                self.range = Some((self.anchor.min(i), self.anchor.max(i)));
                self.toggled.clear();
            }
            SelectBy::Click if mods.ctrl => self.toggle(i),
            SelectBy::Key if mods.ctrl => {} // Ctrl+arrows move the cursor only
            SelectBy::Space if mods.ctrl => self.toggle(i),
            SelectBy::Space | SelectBy::RightClick if self.contains(i) => {}
            _ => self.single(i),
        }
    }
    /// Approximate count for the status bar.
    pub fn count(&self) -> usize {
        let r = self.range.map(|(a, b)| b - a + 1).unwrap_or(0);
        let inside = self.toggled.iter().filter(|&&i| self.range.is_some_and(|(a, b)| i >= a && i <= b)).count();
        r - inside + (self.toggled.len() - inside)
    }
    /// The first `n` selected rows, in order.
    pub fn first_n(&self, n: usize, len: usize) -> Vec<usize> {
        let mut c: Vec<usize> = self.toggled.iter().copied().collect();
        if let Some((a, b)) = self.range {
            c.extend((a..=b).take(n + self.toggled.len()));
        }
        c.sort_unstable();
        c.dedup();
        c.into_iter().filter(|&i| i < len && self.contains(i)).take(n).collect()
    }
}

/// Column x positions for a rows rect.
#[derive(Clone, Copy)]
struct Cols {
    name_w: f32,
    size_x: f32,
    mod_x: f32,
    type_x: f32,
}

impl Cols {
    fn new(x: f32, w: f32) -> Cols {
        let type_x = x + w - COL_TYPE_W;
        let mod_x = type_x - COL_MOD_W;
        let size_x = mod_x - COL_SIZE_W;
        Cols { name_w: (size_x - x - NAME_X - PAD).max(40.0), size_x, mod_x, type_x }
    }
}

pub struct View {
    pub list: ListState,
    pub filter: TextInput,
    pub source: Source,
    pub sel: Selection,
    /// rows of `source` that match the filter (None: no filter, all rows)
    filtered: Option<Vec<u32>>,
    filtered_for: String,
    pub filter_ms: f64,
    text: String,
    /// rows drawn by the last paint (first, last+1)
    pub last_range: (usize, usize),
    rowbuf: Box<RowBuf>,
    /// the list asked for a context menu: (view row, DIP point); Shift+F10 /
    /// Menu key give (cursor row, None)
    pub menu_request: Option<(usize, Option<Pt>)>,
}

impl View {
    pub fn new(source: Source) -> Self {
        View {
            list: ListState::new(ROW_H),
            filter: TextInput::default(),
            source,
            sel: Selection::default(),
            filtered: None,
            filtered_for: String::new(),
            filter_ms: 0.0,
            text: String::with_capacity(32),
            last_range: (0, 0),
            rowbuf: Box::new(RowBuf::new()),
            menu_request: None,
        }
    }

    /// Rows shown (after the filter).
    pub fn len(&self) -> usize {
        self.filtered.as_ref().map(|f| f.len()).unwrap_or_else(|| self.source.len())
    }
    /// Source row of view row `i`.
    pub fn src_row(&self, i: usize) -> usize {
        self.filtered.as_ref().map(|f| f[i] as usize).unwrap_or(i)
    }
    pub fn path(&self, i: usize) -> Option<String> {
        (i < self.len()).then(|| self.source.path(self.src_row(i))).flatten()
    }
    pub fn cursor(&self) -> usize {
        self.list.cursor
    }
    pub fn set_cursor(&mut self, i: usize) {
        self.list.cursor = i;
        self.sel.single(i);
    }
    pub fn max_scroll(&self) -> f64 {
        self.list.max_scroll(self.len())
    }
    pub fn page_rows(&self) -> usize {
        self.list.page_rows()
    }
    pub fn set_scroll(&mut self, s: f64) -> bool {
        let n = self.len();
        self.list.set_scroll(s, n)
    }
    /// A new data source (folder listed): back to the top, no filter result.
    pub fn set_source(&mut self, s: Source) {
        self.source = s;
        self.filtered_for.clear();
        self.filtered = None;
        self.refilter();
        self.list.scroll = 0.0;
        self.list.cursor = 0;
        self.sel.clear();
    }

    /// Client y (DIP) of the middle of view row `i` (for keyboard menus).
    pub fn row_mid_y(&self, i: usize) -> f32 {
        TOOLBAR_H + HEADER_H + self.list.row_top(i) as f32 + ROW_H / 2.0
    }

    /// Paths for a context menu on `row`: the selection if `row` is in it,
    /// else just `row` (at most 64). Empty for fake rows.
    pub fn menu_paths(&self, row: usize) -> Vec<String> {
        let rows = if self.sel.contains(row) { self.sel.first_n(64, self.len()) } else { vec![row] };
        rows.into_iter().filter_map(|r| self.path(r)).collect()
    }

    /// Re-runs the filter when its text changed. Typing one more char only
    /// searches the previous result.
    fn refilter(&mut self) {
        let needle = self.filter.text.to_ascii_lowercase();
        if needle == self.filtered_for && (self.filtered.is_some() || needle.is_empty()) {
            return;
        }
        let t = std::time::Instant::now();
        if needle.is_empty() {
            self.filtered = None;
        } else {
            let narrow = !self.filtered_for.is_empty() && needle.contains(self.filtered_for.as_str());
            let row = &mut *self.rowbuf;
            let src = &self.source;
            let keep = |i: usize, row: &mut RowBuf| {
                src.row(i, row);
                contains_ci(&row.name[..row.name_len], needle.as_bytes())
            };
            let out: Vec<u32> = match (&self.filtered, narrow) {
                (Some(prev), true) => prev.iter().copied().filter(|&i| keep(i as usize, row)).collect(),
                _ => (0..src.len()).filter(|&i| keep(i, row)).map(|i| i as u32).collect(),
            };
            self.filtered = Some(out);
        }
        self.filtered_for = needle;
        self.filter_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.list.scroll = 0.0;
        self.list.cursor = 0;
        self.sel.clear();
    }

    // ---- drawing ----
    /// One frame. None when there is no render target.
    pub fn paint(&mut self, ui: &mut Ui, g: &mut Gfx, shell: &mut ShellUi) -> Option<Output> {
        let (w, h) = g.size();
        if !g.begin(ui.theme.bg) {
            return None;
        }
        ui.begin_frame((w, h));
        if ui.commands().contains(&CMD_FIND) {
            ui.set_focus(Some(FILTER_ID));
            self.filter.select_all();
        }
        let mut r = Rect::new(0.0, 0.0, w, h);
        let toolbar = r.cut_top(TOOLBAR_H);
        let status = r.cut_bottom(STATUS_H);
        let header = r.cut_top(HEADER_H);
        let list_r = r;
        let t = ui.theme.clone();

        // toolbar: the filter box
        g.fill(toolbar, t.panel);
        let fr = Rect::new(toolbar.x + PAD, toolbar.y + (TOOLBAR_H - t.input_h) / 2.0, FILTER_W.min(toolbar.w - 2.0 * PAD), t.input_h);
        let fresp = text_input(ui, g, FILTER_ID, fr, &mut self.filter, "Filter (Ctrl+F)");
        if fresp.changed {
            self.refilter();
        }
        if fresp.cancelled {
            if self.filter.text.is_empty() {
                ui.set_focus(Some(LIST_ID));
            } else {
                self.filter.set_text("");
                self.refilter();
            }
        }
        if fresp.submitted {
            ui.set_focus(Some(LIST_ID));
        }
        if !self.filter.text.is_empty() {
            self.text.clear();
            use std::fmt::Write;
            let _ = write!(self.text, "{} of {} match  ({:.1} ms)", self.len(), self.source.len(), self.filter_ms);
            g.text(&self.text, Rect::new(fr.r() + PAD, toolbar.y, 320.0, TOOLBAR_H), t.font_small, t.dim);
        }

        // header
        let cols = Cols::new(list_r.x, list_r.w - t.sb_w);
        g.fill(header, t.header);
        g.fill(Rect::new(0.0, header.b() - 1.0, w, 1.0), t.sep);
        let (hy, hh) = (header.y, HEADER_H - 1.0);
        g.text("Name", Rect::new(NAME_X, hy, cols.name_w, hh), t.font_bold, t.dim);
        g.text("Size", Rect::new(cols.size_x, hy, COL_SIZE_W - PAD, hh), FONT_HDR_R, t.dim);
        g.text("Modified", Rect::new(cols.mod_x + PAD, hy, COL_MOD_W - PAD, hh), t.font_bold, t.dim);
        g.text("Type", Rect::new(cols.type_x + PAD, hy, COL_TYPE_W - PAD, hh), t.font_bold, t.dim);

        // the list
        let root = match &self.source {
            Source::Dir(d) => Some(d.root.as_str()),
            _ => None,
        };
        let mut rows = Rows { source: &self.source, filtered: self.filtered.as_deref(), sel: &mut self.sel, shell, row: &mut self.rowbuf, text: &mut self.text, cols, root };
        let lr = list(ui, g, LIST_ID, list_r, &mut self.list, &mut rows);
        self.last_range = lr.range;
        if let Some((row, pos)) = lr.context {
            match row {
                Some(r) => self.menu_request = Some((r, Some(pos))),
                None => self.sel.clear(),
            }
        }
        if lr.context_key {
            self.menu_request = Some((self.list.cursor, None));
        }
        if let Source::Loading(p) = &self.source {
            g.text(&format!("listing {p} on a worker thread..."), Rect::new(NAME_X, list_r.y + 10.0, w - 2.0 * NAME_X, ROW_H), t.font, t.dim);
        }

        if shell.thumbs.on {
            paint_thumb(g, &t, shell, list_r);
        }

        // status bar
        g.fill(status, t.header);
        g.fill(Rect::new(0.0, status.y, w, 1.0), t.sep);
        self.text.clear();
        {
            use std::fmt::Write;
            let _ = write!(
                self.text,
                "Item {} of {}   |   {} selected   |   showing {}-{}   |   {} data, {} render   |   icons {} cached, {} pending   |   {}",
                if self.len() == 0 { 0 } else { self.list.cursor + 1 },
                self.len(),
                self.sel.count(),
                lr.range.0 + 1,
                lr.range.1,
                self.source.label(),
                g.path(),
                shell.icons.len(),
                shell.pending(),
                shell.menu_status
            );
        }
        g.text(&self.text, Rect::new(PAD, status.y + 1.0, w - 2.0 * PAD, STATUS_H - 1.0), t.font_small, t.dim);

        let out = ui.end_frame();
        g.end();
        Some(out)
    }

    /// Keys and chars no widget used: the S4 letter commands.
    pub fn leftover_chars(ui: &Ui) -> Vec<char> {
        ui.leftover().iter().filter_map(|e| if let Event::Char(c) = e { Some(c.to_ascii_lowercase()) } else { None }).collect()
    }
}

/// ASCII-case-insensitive substring search (`needle` already lowercase).
fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| w.iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

/// The list's view of the data for one frame.
struct Rows<'a> {
    source: &'a Source,
    filtered: Option<&'a [u32]>,
    sel: &'a mut Selection,
    shell: &'a mut ShellUi,
    row: &'a mut RowBuf,
    text: &'a mut String,
    cols: Cols,
    root: Option<&'a str>,
}

impl ListSource for Rows<'_> {
    fn len(&self) -> usize {
        self.filtered.map(|f| f.len()).unwrap_or_else(|| self.source.len())
    }
    fn is_selected(&self, i: usize) -> bool {
        self.sel.contains(i)
    }
    fn select(&mut self, i: usize, mods: Mods, by: SelectBy) {
        self.sel.apply(i, mods, by);
    }
    fn select_none(&mut self) {
        self.sel.clear();
    }
    fn draw_row(&mut self, g: &mut dyn Canvas, t: &Theme, i: usize, r: Rect, _st: RowState) {
        let si = self.filtered.map(|f| f[i] as usize).unwrap_or(i);
        let row = &mut *self.row;
        self.source.row(si, row);
        let (x, y) = (r.x, r.y);
        let c = self.cols;
        match self.shell.row_icon(row, self.root) {
            Some(img) => {
                let ir = Rect::new(x + 11.0, y + (ROW_H - ICON_DIP) / 2.0, ICON_DIP, ICON_DIP);
                g.image(&img.key, img.w, img.h, &img.px, ir);
            }
            None => {
                // placeholder until the shell icon arrives
                let (ic, icol) = if row.is_dir() { (Rect::new(x + 12.0, y + 6.0, 14.0, 10.0), ICON_DIR) } else { (Rect::new(x + 14.0, y + 4.0, 10.0, 14.0), ICON_FILE) };
                g.round_a(ic, 2.0, icol, 0.35);
            }
        }
        g.text(row.name_str(), Rect::new(x + NAME_X, y, c.name_w, ROW_H), t.font, t.text);
        if !row.is_dir() {
            data::fmt_size(row.size, self.text);
            g.text(self.text, Rect::new(c.size_x, y, COL_SIZE_W - PAD, ROW_H), FONT_R, t.dim);
        }
        data::fmt_date(row.mtime, self.text);
        g.text(self.text, Rect::new(c.mod_x + PAD, y, COL_MOD_W - PAD, ROW_H), t.font, t.dim);
        row.type_text(self.text);
        g.text(self.text, Rect::new(c.type_x + PAD, y, COL_TYPE_W - PAD, ROW_H), t.font, t.dim);
    }
}

fn paint_thumb(g: &mut Gfx, t: &Theme, shell: &mut ShellUi, list_r: Rect) {
    let (pw, ph) = (THUMB_DIP + 24.0, THUMB_DIP + 64.0);
    let pr = Rect::new(list_r.r() - t.sb_w - pw - 12.0, list_r.b() - ph - 12.0, pw, ph);
    g.round(pr, 6.0, t.panel);
    g.round_stroke(pr, 6.0, PANEL_BORDER, 1.0);
    let title = shell.thumbs.path.as_deref().and_then(|p| p.rsplit('\\').next()).unwrap_or("(no file)").to_string();
    g.text(&title, Rect::new(pr.x + 12.0, pr.y + 4.0, pw - 24.0, 22.0), t.font_bold, t.text);
    let status = shell.thumbs.status.clone();
    g.text(&status, Rect::new(pr.x + 12.0, pr.b() - 28.0, pw - 24.0, 22.0), f(10.5), t.dim);
    let area = Rect::new(pr.x + 12.0, pr.y + 30.0, THUMB_DIP, THUMB_DIP);
    if let Some(img) = shell.thumb_image() {
        // fit, keep aspect
        let s = (area.w / img.w as f32).min(area.h / img.h as f32);
        let (dw, dh) = (img.w as f32 * s, img.h as f32 * s);
        let r = Rect::new(area.x + (area.w - dw) / 2.0, area.y + (area.h - dh) / 2.0, dw, dh);
        g.image(&img.key, img.w, img.h, &img.px, r);
    } else {
        g.round_a(area, 4.0, PANEL_BORDER, 0.3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_model() {
        let mut s = Selection::default();
        s.apply(5, Mods::NONE, SelectBy::Click);
        s.apply(9, Mods::SHIFT, SelectBy::Click);
        assert_eq!(s.count(), 5);
        s.apply(7, Mods::CTRL, SelectBy::Click);
        assert!(!s.contains(7) && s.contains(8));
        s.apply(20, Mods::CTRL, SelectBy::Click);
        assert_eq!(s.first_n(10, 100), vec![5, 6, 8, 9, 20]);
        assert_eq!(s.count(), 5);
        // Shift+End over a million rows is two numbers
        s.apply(0, Mods::NONE, SelectBy::Key);
        s.apply(999_999, Mods::SHIFT, SelectBy::Key);
        assert_eq!(s.count(), 1_000_000);
        assert_eq!(s.first_n(3, 1_000_000), vec![0, 1, 2]);
        // a right click inside the selection keeps it
        s.apply(42, Mods::NONE, SelectBy::RightClick);
        assert_eq!(s.count(), 1_000_000);
        s.apply(3, Mods::CTRL, SelectBy::Key);
        assert_eq!(s.count(), 1_000_000, "Ctrl+arrow moves the cursor only");
    }

    #[test]
    fn filter_case_insensitive() {
        assert!(contains_ci(b"Report-Photo-12.TXT", b"photo"));
        assert!(contains_ci(b"abc", b""));
        assert!(!contains_ci(b"ab", b"abc"));
    }
}
