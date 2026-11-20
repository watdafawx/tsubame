//! Batch rename: a 40 px bar docked under the pane's toolbar (pattern field,
//! token chips, Apply / Cancel) and a preview popup under it with the first
//! rows, collisions flagged. All or nothing: Apply is off while any row
//! collides. The engine is `rename::plan_batch`; this file draws it.

use crate::overlay;
use crate::rename::{self, BatchItem, BatchPlan, RowProblem};
use tsubame_ui::{Button, Canvas, Id, Rect, Sense, TextInput, Theme, Ui, f, text_input};

pub const BAR_H: f32 = 40.0;
const PREVIEW_ROWS: usize = 8;
const PREVIEW_W: f32 = 640.0;
const ROW_H: f32 = 26.0;
/// Pattern new batches start with: keep the name.
pub const DEFAULT_PATTERN: &str = "{name}";
const TOKENS: [&str; 3] = ["{name}", "{n:3}", "{date}"];

pub struct BatchEdit {
    /// full paths, in view order
    pub paths: Vec<String>,
    pub items: Vec<BatchItem>,
    pub parent: String,
    /// every name in the folder (collision checks)
    pub folder_names: Vec<String>,
    pub pattern: TextInput,
    pub plan: BatchPlan,
    pub frames: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchOut {
    None,
    Apply,
    Cancel,
}

impl BatchEdit {
    pub fn new(paths: Vec<String>, items: Vec<BatchItem>, parent: String, folder_names: Vec<String>) -> BatchEdit {
        let mut pattern = TextInput::default();
        pattern.set_text(DEFAULT_PATTERN);
        // select the token: typing replaces it, an arrow key keeps it
        pattern.anchor = 0;
        let mut b = BatchEdit { paths, items, parent, folder_names, pattern, plan: BatchPlan::default(), frames: 0 };
        b.replan();
        b
    }

    pub fn replan(&mut self) {
        self.plan = rename::plan_batch(&self.pattern.text, &self.items, &self.parent, &self.folder_names);
    }

    /// (old path, new path) of every row that changes.
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.plan
            .rows
            .iter()
            .zip(&self.paths)
            .filter(|(r, _)| !r.unchanged())
            .map(|(r, p)| (p.clone(), crate::nav::join(&self.parent, &r.new)))
            .collect()
    }

    pub fn header(&self) -> String {
        let c = self.plan.conflicts();
        let n = self.plan.changes();
        if c > 0 { format!("{n} renames \u{b7} {c} names collide") } else { format!("{n} renames") }
    }
}

#[allow(clippy::too_many_arguments)]
fn button(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, label: &str, primary: bool, enabled: bool) -> bool {
    let it = ui.interact(id, r, Sense::click());
    let (bg, fg) = match (primary, enabled) {
        (true, true) => (t.accent, t.on_accent),
        (true, false) => (t.selected_inactive, t.text_disabled),
        (false, _) => (if it.hovered { t.hover } else { t.panel }, t.text),
    };
    g.round(r, t.radius_md, bg);
    if !primary {
        g.round_stroke(r, t.radius_md, t.border, 1.0);
    }
    g.text(label, r, t.font().c(), fg);
    enabled && it.clicked == Some(Button::Left)
}

/// The bar in `bar` (BAR_H tall). Enter applies, Esc cancels.
pub fn paint_bar(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, bar: Rect, b: &mut BatchEdit) -> BatchOut {
    b.frames += 1;
    g.fill(bar, t.panel);
    g.fill(Rect::new(bar.x, bar.b() - 1.0, bar.w, 1.0), t.border);
    let cy = bar.y + bar.h / 2.0;
    let font = t.font();
    let mut x = bar.x + 12.0;
    let label = format!("Rename {} items", b.items.len());
    let lw = g.measure(&label, font.b()) + 2.0;
    g.text(&label, Rect::new(x, bar.y, lw, bar.h), font.b(), t.text);
    x += lw + 12.0;

    // right side first: Cancel, Apply
    let mut right = bar.r() - 12.0;
    let cancel = Rect::new(right - 76.0, cy - 14.0, 76.0, 28.0);
    right = cancel.x - 8.0;
    let apply_r = Rect::new(right - 76.0, cy - 14.0, 76.0, 28.0);
    right = apply_r.x - 12.0;
    let can = b.plan.can_apply();

    // token chips
    let mut chips_w = 0.0;
    for tok in TOKENS {
        chips_w += g.measure(tok, t.font_mono()) + 18.0 + 4.0;
    }
    let field_w = (right - chips_w - 8.0 - x).clamp(120.0, 420.0);
    let field = Rect::new(x, cy - 14.0, field_w, 28.0);
    g.fill(field, t.popup);
    let resp = text_input(ui, g, id, field, &mut b.pattern, "Pattern");
    if resp.changed {
        b.replan();
        ui.request_repaint();
    }
    let mut cx = field.r() + 8.0;
    for (i, tok) in TOKENS.iter().enumerate() {
        let w = g.measure(tok, t.font_mono()) + 18.0;
        if cx + w > right {
            break;
        }
        let r = Rect::new(cx, cy - 11.0, w, 22.0);
        let it = ui.interact(id.with(0x100 + i as u64), r, Sense::click());
        g.round(r, t.radius_sm, if it.hovered { t.hover } else { t.row_alt });
        g.round_stroke(r, t.radius_sm, t.border, 1.0);
        g.text(tok, r, t.font_mono().c(), t.text_dim);
        if it.clicked == Some(Button::Left) {
            b.pattern.insert(tok);
            b.replan();
            ui.set_focus(Some(id));
        }
        cx += w + 4.0;
    }

    let mut out = BatchOut::None;
    if button(ui, g, t, id.with(0x200), apply_r, "Apply", true, can) {
        out = BatchOut::Apply;
    }
    if button(ui, g, t, id.with(0x201), cancel, "Cancel", false, true) {
        out = BatchOut::Cancel;
    }
    if resp.submitted && can {
        out = BatchOut::Apply;
    }
    if resp.cancelled {
        out = BatchOut::Cancel;
    }
    out
}

/// The preview popup under the bar: the first rows, collisions flagged.
pub fn paint_preview(g: &mut dyn Canvas, t: &Theme, win: (f32, f32), bar: Rect, b: &BatchEdit) {
    let n = b.plan.rows.len();
    let shown = n.min(PREVIEW_ROWS);
    let more = n - shown;
    let w = PREVIEW_W.min(bar.w - 16.0).max(240.0);
    let foot = if b.plan.suggests_counter(&b.pattern.text) { 30.0 } else { 0.0 };
    let h = 32.0 + shown as f32 * ROW_H + if more > 0 { 22.0 } else { 0.0 } + foot + 6.0;
    let r = Rect::new((bar.x + 12.0).min(win.0 - w - 4.0).max(4.0), bar.b() + 4.0, w, h);
    g.round(r, t.radius_md, t.popup);
    g.round_stroke(r, t.radius_md, t.popup_border, 1.0);
    let font = t.font();
    let small = t.font_small();
    let head = b.header();
    g.text(&head, Rect::new(r.x + 12.0, r.y, r.w - 24.0, 32.0), small.b(), if b.plan.conflicts() > 0 { t.danger } else { t.text_dim });
    let mut y = r.y + 32.0;
    let half = (r.w - 24.0 - 24.0) / 2.0;
    for row in b.plan.rows.iter().take(shown) {
        let bad = row.problem.is_some();
        let rr = Rect::new(r.x + 6.0, y, r.w - 12.0, ROW_H);
        if bad {
            g.round_a(rr, t.radius_sm, t.danger, 0.12);
        }
        g.text(&row.old, Rect::new(r.x + 12.0, y, half, ROW_H), font, t.text_dim);
        g.text("\u{E72A}", Rect::new(r.x + 12.0 + half, y, 24.0, ROW_H), f(11.0).icon().c(), t.text_dim);
        let col = if bad { t.danger } else if row.unchanged() { t.text_dim } else { t.text };
        let nw = if bad { half - 70.0 } else { half };
        g.text(&row.new, Rect::new(r.x + 12.0 + half + 24.0, y, nw, ROW_H), font, col);
        if let Some(p) = &row.problem {
            let tag = match p {
                RowProblem::Duplicate => "duplicate",
                RowProblem::Taken => "exists",
                RowProblem::Invalid(_) => "invalid",
            };
            let tw = g.measure(tag, small) + 2.0;
            g.text(tag, Rect::new(rr.r() - tw - 6.0, y, tw, ROW_H), small, t.danger);
        }
        y += ROW_H;
    }
    if more > 0 {
        g.text(&format!("+ {more} more"), Rect::new(r.x + 12.0, y, r.w - 24.0, 22.0), small, t.text_dim);
        y += 22.0;
    }
    if foot > 0.0 {
        let cy = y + 15.0;
        let k = overlay::keycap(g, t, r.x + 12.0, cy, "{n:3}");
        g.text("Add a counter to give each name its own number.", Rect::new(r.x + 12.0 + k + 8.0, y, r.w - 40.0 - k, 30.0), small, t.text_dim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[&str]) -> (Vec<String>, Vec<BatchItem>) {
        let paths = names.iter().map(|n| format!("C:\\d\\{n}")).collect();
        let it = names.iter().map(|n| BatchItem { name: n.to_string(), is_dir: false, mtime: 0 }).collect();
        (paths, it)
    }

    #[test]
    fn default_pattern_changes_nothing_and_cannot_apply() {
        let (p, i) = items(&["a.txt", "b.txt"]);
        let b = BatchEdit::new(p, i, "C:\\d".into(), vec!["a.txt".into(), "b.txt".into()]);
        assert!(!b.plan.can_apply());
        assert!(b.pairs().is_empty());
    }

    #[test]
    fn counter_pattern_gives_pairs_and_header() {
        let (p, i) = items(&["a.txt", "b.txt"]);
        let mut b = BatchEdit::new(p, i, "C:\\d".into(), vec!["a.txt".into(), "b.txt".into()]);
        b.pattern.set_text("pic{n:3}");
        b.replan();
        assert!(b.plan.can_apply());
        assert_eq!(b.pairs(), vec![("C:\\d\\a.txt".to_string(), "C:\\d\\pic001.txt".to_string()), ("C:\\d\\b.txt".to_string(), "C:\\d\\pic002.txt".to_string())]);
        assert_eq!(b.header(), "2 renames");
    }

    #[test]
    fn duplicates_block_apply_and_say_so() {
        let (p, i) = items(&["a.txt", "b.txt"]);
        let mut b = BatchEdit::new(p, i, "C:\\d".into(), vec!["a.txt".into(), "b.txt".into()]);
        b.pattern.set_text("same");
        b.replan();
        assert!(!b.plan.can_apply());
        assert!(b.header().contains("collide"), "{}", b.header());
        assert!(b.plan.suggests_counter("same"));
    }
}
