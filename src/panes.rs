//! Two panes: the layout math (ratio to pixels and back, minimum sizes, the
//! narrow-pane rules) and the names of the sides. Pure functions so the split
//! rules are unit tested; `view.rs` owns the panes' state and draws them.

use tsubame_config::Split as Orient;
use tsubame_ui::Rect;

/// Narrowest a pane may get (DIPs).
pub const MIN_PANE: f32 = 240.0;
/// Below this pane width the Type column goes and the filter box becomes a
/// search button.
pub const NARROW: f32 = 640.0;
/// A split ratio is kept inside this range (the config says 0.2 to 0.8).
pub const MIN_RATIO: f32 = 0.2;
pub const MAX_RATIO: f32 = 0.8;
/// Width of the splitter bar (the kit's `splitter_w`).
pub const BAR: f32 = 5.0;

pub fn clamp_ratio(r: f32) -> f32 {
    if r.is_finite() { r.clamp(MIN_RATIO, MAX_RATIO) } else { 0.5 }
}

/// Where the bar sits (distance from the area's start) for a ratio. Both panes
/// keep `MIN_PANE`; an area too small for two panes splits in half.
pub fn pos_for(ratio: f32, len: f32) -> f32 {
    let room = (len - BAR).max(0.0);
    let want = room * clamp_ratio(ratio);
    if room < 2.0 * MIN_PANE { room / 2.0 } else { want.clamp(MIN_PANE, room - MIN_PANE) }
}

/// The ratio a bar at `pos` stands for.
pub fn ratio_for(pos: f32, len: f32) -> f32 {
    let room = (len - BAR).max(1.0);
    clamp_ratio(pos / room)
}

/// The two pane rectangles and the bar position for `area`.
pub fn rects(area: Rect, orient: Orient, ratio: f32) -> (Rect, Rect, f32) {
    match orient {
        Orient::Vertical => {
            let pos = pos_for(ratio, area.w);
            let (a, rest) = area.split_x(pos);
            (a, rest.pad4(BAR, 0.0, 0.0, 0.0), pos)
        }
        Orient::Horizontal => {
            let pos = pos_for(ratio, area.h);
            let (a, rest) = area.split_y(pos);
            (a, rest.pad4(0.0, BAR, 0.0, 0.0), pos)
        }
    }
}

/// Length of the area along the split.
pub fn len_of(area: Rect, orient: Orient) -> f32 {
    match orient {
        Orient::Vertical => area.w,
        Orient::Horizontal => area.h,
    }
}

/// The Type column is hidden in panes narrower than `NARROW`.
pub fn hide_type_column(pane_w: f32) -> bool {
    pane_w < NARROW
}

/// The filter box becomes a search button in panes narrower than `NARROW`.
pub fn compact_toolbar(pane_w: f32) -> bool {
    pane_w < NARROW
}

/// The other slot (0 = left / top, 1 = right / bottom).
pub fn other(slot: usize) -> usize {
    1 - slot.min(1)
}

/// "right", "left", "top" or "bottom": where the other pane is, for the
/// "F5 Copy to right" hint.
pub fn side_name(orient: Orient, from_slot: usize) -> &'static str {
    match (orient, from_slot) {
        (Orient::Vertical, 0) => "right",
        (Orient::Vertical, _) => "left",
        (Orient::Horizontal, 0) => "bottom",
        (Orient::Horizontal, _) => "top",
    }
}

/// Opposite orientation (Ctrl+Shift+\).
pub fn flip(o: Orient) -> Orient {
    match o {
        Orient::Vertical => Orient::Horizontal,
        Orient::Horizontal => Orient::Vertical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_split_of_a_wide_area() {
        let area = Rect::new(0.0, 0.0, 1005.0, 600.0);
        let (a, b, pos) = rects(area, Orient::Vertical, 0.5);
        assert_eq!(pos, 500.0);
        assert_eq!(a.w, 500.0);
        assert_eq!(b.x, 505.0);
        assert_eq!(b.w, 500.0);
        assert_eq!(a.h, 600.0);
        assert_eq!(a.w + BAR + b.w, area.w);
    }

    #[test]
    fn stacked_split_cuts_the_height() {
        let area = Rect::new(10.0, 20.0, 800.0, 605.0);
        let (a, b, pos) = rects(area, Orient::Horizontal, 0.5);
        assert_eq!(pos, 300.0);
        assert_eq!(a.y, 20.0);
        assert_eq!(a.h, 300.0);
        assert_eq!(b.y, 20.0 + 300.0 + BAR);
        assert_eq!(b.h, 300.0);
        assert_eq!(a.w, 800.0);
    }

    #[test]
    fn panes_keep_their_minimum() {
        let len = 1000.0;
        // ratio is clamped to 0.2..0.8 first, then both panes keep 240
        assert!((pos_for(0.0, len) - 240.0).abs() < 0.001 || pos_for(0.0, len) >= MIN_PANE);
        assert!(pos_for(0.0, len) >= MIN_PANE);
        assert!(len - BAR - pos_for(1.0, len) >= MIN_PANE);
        assert!((pos_for(0.5, len) - 497.5).abs() < 0.001);
        // too narrow for two panes: half and half
        assert_eq!(pos_for(0.3, 400.0), 197.5);
        // a window collapsed to nothing never gives negative sizes
        assert_eq!(pos_for(0.5, 0.0), 0.0);
        assert!(pos_for(0.5, 3.0) >= 0.0);
    }

    #[test]
    fn ratio_round_trips_inside_the_limits() {
        for r in [0.25, 0.4, 0.5, 0.6, 0.75] {
            let pos = pos_for(r, 1400.0);
            assert!((ratio_for(pos, 1400.0) - r).abs() < 0.002, "{r}");
        }
        assert_eq!(clamp_ratio(0.01), MIN_RATIO);
        assert_eq!(clamp_ratio(0.99), MAX_RATIO);
        assert_eq!(clamp_ratio(f32::NAN), 0.5);
    }

    #[test]
    fn narrow_panes_drop_the_type_column_and_the_filter_box() {
        assert!(hide_type_column(639.0));
        assert!(!hide_type_column(640.0));
        assert!(compact_toolbar(500.0));
        assert!(!compact_toolbar(900.0));
    }

    #[test]
    fn side_names_and_flip() {
        assert_eq!(side_name(Orient::Vertical, 0), "right");
        assert_eq!(side_name(Orient::Vertical, 1), "left");
        assert_eq!(side_name(Orient::Horizontal, 0), "bottom");
        assert_eq!(side_name(Orient::Horizontal, 1), "top");
        assert_eq!(flip(Orient::Vertical), Orient::Horizontal);
        assert_eq!(other(0), 1);
        assert_eq!(other(1), 0);
        assert_eq!(len_of(Rect::new(0.0, 0.0, 10.0, 20.0), Orient::Horizontal), 20.0);
    }
}
