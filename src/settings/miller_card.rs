//! Settings > Extensions > Miller columns: the default column width (`[miller] col_width`).
//!
//! The slider writes `prefs.miller.col_width` and the view applies it on the
//! next frame to every column that has no width of its own (a column the
//! user dragged keeps it). The same range as the view: 140 to 480 DIPs, in
//! steps of 10.

use super::lay::{Look, Pg, button};
use super::{C_OTHER, Cx};
use tsubame_config::MillerConfig;
use tsubame_ui::{Id, Rect, slider};

const ID_COL_W: Id = Id::new("miller-col-width");
const ID_COL_RESET: Id = Id::new("miller-col-reset");
/// The slider's step in DIPs.
const STEP: u32 = 10;

/// The slider position (0..1) of a width.
pub fn fraction_of(w: u32) -> f32 {
    let (lo, hi) = (MillerConfig::MIN_COL_WIDTH, MillerConfig::MAX_COL_WIDTH);
    (w.clamp(lo, hi) - lo) as f32 / (hi - lo) as f32
}

/// The width (a multiple of the step, in range) of a slider position.
pub fn width_of(f: f32) -> u32 {
    let (lo, hi) = (MillerConfig::MIN_COL_WIDTH, MillerConfig::MAX_COL_WIDTH);
    let raw = lo as f32 + f.clamp(0.0, 1.0) * (hi - lo) as f32;
    let snapped = lo + ((raw - lo as f32) / STEP as f32).round() as u32 * STEP;
    snapped.clamp(lo, hi)
}

pub fn paint(pg: &mut Pg, cx: &mut Cx) {
    let t = pg.t;
    let on = crate::miller::enabled();
    pg.section("Miller columns");
    let mut c = pg.card(1, 52.0);
    let row = pg.next_row(&mut c);
    pg.row_text(row, "Column width", "Default width of a column (140 to 480). A column you drag keeps its own width; double-click its edge to reset it.", t.layout_slider_width + 190.0, on);
    let w = cx.prefs.miller.sanitized().col_width;
    let sr = Rect::new(row.r() - 16.0 - 150.0 - t.layout_slider_width, row.y + row.h / 2.0 - 8.0, t.layout_slider_width, 16.0);
    if let Some(v) = slider(pg.ui, pg.g, ID_COL_W, sr, fraction_of(w), STEP as f32 / (MillerConfig::MAX_COL_WIDTH - MillerConfig::MIN_COL_WIDTH) as f32)
        && on
    {
        let nw = width_of(v);
        if nw != w {
            cx.prefs.miller.col_width = nw;
            cx.changed |= C_OTHER;
        }
    }
    pg.g.text(&format!("{w} px"), Rect::new(sr.r() + 10.0, row.y, 52.0, row.h), t.font_small().r(), t.text_dim);
    let default = MillerConfig::default().col_width;
    if button(pg.ui, pg.g, ID_COL_RESET, row.r() - 16.0, row.y + (row.h - t.height_button) / 2.0, "Default", Look::Normal, on && w != default, true).0 {
        cx.prefs.miller.col_width = default;
        cx.changed |= C_OTHER;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slider_maps_to_the_view_range_in_steps() {
        assert_eq!(width_of(0.0), MillerConfig::MIN_COL_WIDTH);
        assert_eq!(width_of(1.0), MillerConfig::MAX_COL_WIDTH);
        assert_eq!(width_of(-3.0), MillerConfig::MIN_COL_WIDTH);
        assert_eq!(width_of(9.0), MillerConfig::MAX_COL_WIDTH);
        for k in 0..=100 {
            let w = width_of(k as f32 / 100.0);
            assert!((MillerConfig::MIN_COL_WIDTH..=MillerConfig::MAX_COL_WIDTH).contains(&w));
            assert_eq!((w - MillerConfig::MIN_COL_WIDTH) % STEP, 0, "{w}");
        }
    }

    #[test]
    fn a_width_round_trips_through_the_slider() {
        for w in (MillerConfig::MIN_COL_WIDTH..=MillerConfig::MAX_COL_WIDTH).step_by(STEP as usize) {
            assert_eq!(width_of(fraction_of(w)), w);
        }
        assert_eq!(width_of(fraction_of(220)), 220);
    }
}
