//! Colours and metrics. `Theme` itself, `DARK` and `LIGHT` are generated from
//! `mocks/tokens.json` by `tools/gen-tokens.ps1` (`theme_generated.rs`); this
//! file adds the hand-written helpers (fonts, density, a few widget metrics).

use crate::canvas::{Font, f};

pub use crate::theme_generated::{BRAND_CREAM, BRAND_NAVY, BRAND_RED, DARK, FONT_MONO, FONT_UI, LIGHT, Theme};

/// Row height and text size preset. Switched at runtime; layouts reflow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Density {
    /// 22 px rows (the mock)
    #[default]
    Compact,
    /// 26 px rows
    Comfortable,
    /// 30 px rows
    Spacious,
}

impl Density {
    pub const ALL: [Density; 3] = [Density::Compact, Density::Comfortable, Density::Spacious];

    pub fn row_h(self) -> f32 {
        match self {
            Density::Compact => 22.0,
            Density::Comfortable => 26.0,
            Density::Spacious => 30.0,
        }
    }
    /// Text size factor on the base (compact) sizes.
    pub fn font_scale(self) -> f32 {
        match self {
            Density::Compact => 1.0,
            Density::Comfortable => 1.08,
            Density::Spacious => 1.17,
        }
    }
    pub fn next(self) -> Density {
        match self {
            Density::Compact => Density::Comfortable,
            Density::Comfortable => Density::Spacious,
            Density::Spacious => Density::Compact,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Density::Compact => "compact",
            Density::Comfortable => "comfortable",
            Density::Spacious => "spacious",
        }
    }
}

impl Theme {
    /// The dark theme (the default).
    pub fn dark() -> Theme {
        DARK
    }
    pub fn light() -> Theme {
        LIGHT
    }
    pub fn is_dark(&self) -> bool {
        self.name == "dark"
    }
    /// This theme with the density preset applied: list row height and the
    /// four text sizes. Always starts from the token values (`base`), so
    /// calling it twice never compounds.
    pub fn with_density(base: &Theme, d: Density) -> Theme {
        let mut t = *base;
        t.height_list_row = d.row_h();
        let s = d.font_scale();
        t.font_size_xs = (base.font_size_xs * s).round();
        t.font_size_sm = (base.font_size_sm * s).round();
        t.font_size_md = (base.font_size_md * s).round();
        t.font_size_lg = (base.font_size_lg * s).round();
        t
    }
    /// Normal UI text (list rows, tabs).
    pub fn font(&self) -> Font {
        f(self.font_size_sm)
    }
    pub fn font_md(&self) -> Font {
        f(self.font_size_md)
    }
    pub fn font_small(&self) -> Font {
        f(self.font_size_xs)
    }
    pub fn font_bold(&self) -> Font {
        f(self.font_size_sm).b()
    }
    pub fn font_mono(&self) -> Font {
        f(self.font_size_xs).m()
    }
    pub fn font_icon(&self) -> Font {
        f(self.font_size_sm).icon()
    }
    /// Rows per wheel notch.
    pub fn wheel_lines(&self) -> f32 {
        3.0
    }
    pub fn splitter_w(&self) -> f32 {
        5.0
    }
}

impl Default for Theme {
    fn default() -> Self {
        DARK
    }
}
