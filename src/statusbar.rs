//! The status bar of a pane: item count, selection count and size, and on the
//! right the free space of the drive, or (in the active pane of two) the key
//! hints for copying and moving to the other pane, or (during a batch rename)
//! "Enter Apply / Esc Cancel".

use crate::data;
use crate::overlay;
use std::fmt::Write;
use tsubame_ui::{Canvas, Rect, Theme};

/// What a status bar shows.
#[derive(Default)]
pub struct StatusInfo<'a> {
    /// folder still loading: items delivered so far
    pub loading: Option<usize>,
    pub failed: bool,
    pub shown: usize,
    pub total: usize,
    pub selected: usize,
    pub selected_bytes: u64,
    /// free and total bytes of the drive
    pub free: Option<(u64, u64)>,
    /// "right" / "left" / "top" / "bottom": where the other pane is. Set for
    /// the active pane in two-pane mode.
    pub other_side: Option<&'a str>,
    /// a batch rename is open
    pub batch: bool,
    /// something in the message slot ("Undone: rename"), shown instead of the hints
    pub message: Option<&'a str>,
}

/// 12480 -> "12,480"
pub fn count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// "12 items", "1 item", "3 of 12 items", "Reading folder... 400 items".
pub fn items_text(i: &StatusInfo, s: &mut String) {
    s.clear();
    if let Some(d) = i.loading {
        let _ = write!(s, "Reading folder... {} items", count(d));
    } else if i.failed {
        s.push_str("Can't open this folder");
    } else if i.shown != i.total {
        let _ = write!(s, "{} of {} items", count(i.shown), count(i.total));
    } else {
        let _ = write!(s, "{} {}", count(i.shown), if i.shown == 1 { "item" } else { "items" });
    }
}

/// "3 selected  1.2 MB"
pub fn selection_text(i: &StatusInfo, s: &mut String) {
    s.clear();
    let mut sz = String::new();
    data::fmt_size(i.selected_bytes, &mut sz);
    let _ = write!(s, "{} selected  {}", count(i.selected), sz);
}

/// "412 GB free of 931 GB"
pub fn free_text(free: u64, total: u64, s: &mut String) {
    s.clear();
    let (mut a, mut b) = (String::new(), String::new());
    data::fmt_volume(free, &mut a);
    data::fmt_volume(total, &mut b);
    let _ = write!(s, "{a} free of {b}");
}

/// The keycap hints for the active pane: (key, label) pairs.
pub fn pane_hints(side: &str) -> [(&'static str, String); 2] {
    [("F5", format!("Copy to {side}")), ("F6", format!("Move to {side}"))]
}

/// Draws the bar in `r`. `buf` is scratch text reused between frames.
pub fn paint(g: &mut dyn Canvas, t: &Theme, r: Rect, i: &StatusInfo, buf: &mut String) {
    g.fill(r, t.panel);
    g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
    let f = t.font_small();
    items_text(i, buf);
    let left = Rect::new(r.x + 12.0, r.y + 1.0, 360.0f32.min(r.w * 0.5), r.h - 1.0);
    g.text(buf, left, f, if i.failed { t.danger } else { t.text_dim });
    let mut x = r.x + 12.0 + g.measure(buf, f).min(left.w) + 24.0;
    if i.selected > 0 {
        selection_text(i, buf);
        let w = g.measure(buf, f) + 4.0;
        g.text(buf, Rect::new(x, r.y + 1.0, w, r.h - 1.0), f, t.text);
        x += w + 24.0;
    }
    let cy = r.y + 1.0 + (r.h - 1.0) / 2.0;
    let mut right = r.r() - 12.0;
    if i.batch {
        for (key, label) in [("Ctrl+Z", "Undo"), ("Esc", "Cancel"), ("Enter", "Apply")] {
            let lw = g.measure(label, f) + 2.0;
            g.text(label, Rect::new(right - lw, r.y + 1.0, lw, r.h - 1.0), f, t.text_dim);
            right -= lw + 4.0;
            right -= overlay::keycap_right(g, t, right, cy, key) + 14.0;
        }
        return;
    }
    if let Some(m) = i.message {
        let w = g.measure(m, f) + 4.0;
        g.text(m, Rect::new(right - w, r.y + 1.0, w, r.h - 1.0), f, t.text);
        return;
    }
    if let Some(side) = i.other_side {
        for (key, label) in pane_hints(side).iter().rev() {
            let lw = g.measure(label, f) + 2.0;
            if right - lw - 40.0 < x {
                break;
            }
            g.text(label, Rect::new(right - lw, r.y + 1.0, lw, r.h - 1.0), f, t.text_dim);
            right -= lw + 4.0;
            right -= overlay::keycap_right(g, t, right, cy, key) + 14.0;
        }
        return;
    }
    if let Some((free, total)) = i.free {
        free_text(free, total, buf);
        let w = g.measure(buf, f) + 4.0;
        if right - w > x {
            g.text(buf, Rect::new(right - w, r.y + 1.0, w, r.h - 1.0), f.r(), t.text_dim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(12_480), "12,480");
        assert_eq!(count(1_000_000), "1,000,000");
    }

    #[test]
    fn item_texts() {
        let mut s = String::new();
        let mut i = StatusInfo { shown: 1, total: 1, ..Default::default() };
        items_text(&i, &mut s);
        assert_eq!(s, "1 item");
        i.shown = 12480;
        i.total = 12480;
        items_text(&i, &mut s);
        assert_eq!(s, "12,480 items");
        i.shown = 3;
        items_text(&i, &mut s);
        assert_eq!(s, "3 of 12,480 items");
        i.loading = Some(400);
        items_text(&i, &mut s);
        assert_eq!(s, "Reading folder... 400 items");
        i.loading = None;
        i.failed = true;
        items_text(&i, &mut s);
        assert_eq!(s, "Can't open this folder");
    }

    #[test]
    fn selection_and_free_texts() {
        let mut s = String::new();
        let i = StatusInfo { selected: 3, selected_bytes: 1536, ..Default::default() };
        selection_text(&i, &mut s);
        assert!(s.starts_with("3 selected  "), "{s}");
        free_text(412 << 30, 931 << 30, &mut s);
        assert!(s.contains("free of"), "{s}");
        assert_eq!(pane_hints("right")[0], ("F5", "Copy to right".to_string()));
        assert_eq!(pane_hints("top")[1], ("F6", "Move to top".to_string()));
    }
}
