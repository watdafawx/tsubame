//! Colours and metrics. A plain struct for now; the generated theme from
//! `mocks/tokens.json` (task M1) replaces `Theme::dark()` later.

use crate::canvas::{Font, f};
use crate::geom::Color;

#[derive(Clone, Debug)]
pub struct Theme {
    // surfaces
    pub bg: Color,
    pub panel: Color,
    pub header: Color,
    pub sep: Color,
    // text
    pub text: Color,
    pub dim: Color,
    // list
    pub row_sel: Color,
    /// selected rows while the list does not have the focus
    pub row_sel_unfocused: Color,
    pub row_hot: Color,
    /// outline of the keyboard cursor row
    pub row_cursor: Color,
    // scrollbar
    pub sb_track: Color,
    pub sb_thumb: Color,
    pub sb_thumb_hot: Color,
    pub sb_thumb_drag: Color,
    // controls
    pub accent: Color,
    pub input_bg: Color,
    pub input_border: Color,
    pub input_focus: Color,
    pub caret: Color,
    pub text_sel: Color,
    pub placeholder: Color,
    pub button: Color,
    pub button_hot: Color,
    pub button_down: Color,
    pub popup_bg: Color,
    pub popup_border: Color,
    pub tab: Color,
    pub tab_hot: Color,
    pub tab_active: Color,
    pub splitter: Color,
    pub splitter_hot: Color,

    // metrics (DIP)
    pub row_h: f32,
    pub sb_w: f32,
    pub sb_min_thumb: f32,
    pub pad: f32,
    pub radius: f32,
    pub input_h: f32,
    pub tab_h: f32,
    pub tab_min_w: f32,
    pub tab_max_w: f32,
    pub splitter_w: f32,
    /// rows per wheel notch
    pub wheel_lines: f32,

    // fonts
    pub font: Font,
    pub font_small: Font,
    pub font_bold: Font,
    pub font_icon: Font,
}

impl Theme {
    /// The S1 spike colours (dark).
    pub fn dark() -> Theme {
        Theme {
            bg: 0x1b1f27,
            panel: 0x161a21,
            header: 0x222731,
            sep: 0x2d3340,
            text: 0xdde1e8,
            dim: 0x8f97a8,
            row_sel: 0x2d4f86,
            row_sel_unfocused: 0x343c4c,
            row_hot: 0x232935,
            row_cursor: 0x5b7fc0,
            sb_track: 0x20252e,
            sb_thumb: 0x4b5468,
            sb_thumb_hot: 0x5a6479,
            sb_thumb_drag: 0x6b7690,
            accent: 0x4f8cff,
            input_bg: 0x14181f,
            input_border: 0x3a4252,
            input_focus: 0x4f8cff,
            caret: 0xdde1e8,
            text_sel: 0x2d4f86,
            placeholder: 0x6b7385,
            button: 0x2a303c,
            button_hot: 0x343b49,
            button_down: 0x232833,
            popup_bg: 0x1f242d,
            popup_border: 0x3a4252,
            tab: 0x1b1f27,
            tab_hot: 0x232833,
            tab_active: 0x2a303c,
            splitter: 0x2d3340,
            splitter_hot: 0x4f8cff,
            row_h: 22.0,
            sb_w: 14.0,
            sb_min_thumb: 24.0,
            pad: 10.0,
            radius: 4.0,
            input_h: 24.0,
            tab_h: 30.0,
            tab_min_w: 72.0,
            tab_max_w: 220.0,
            splitter_w: 5.0,
            wheel_lines: 3.0,
            font: f(12.0),
            font_small: f(11.0),
            font_bold: f(11.0).b(),
            font_icon: f(12.0).icon(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark()
    }
}
