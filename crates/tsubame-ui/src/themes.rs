//! The built-in themes of the Settings > Appearance picker (U4). Each one is
//! seven colours (the mock's `THEMES` table); `Theme::from_palette` derives
//! the other surface colours from them, so a theme is one line here and the
//! kit keeps one `Theme` struct. The two Tsubame themes are the generated
//! `DARK` / `LIGHT` tokens, not derived.

use crate::theme::{DARK, LIGHT, Theme};
use std::sync::{Mutex, RwLock};

/// Colours are `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// config id (`[appearance] theme`)
    pub id: &'static str,
    pub name: &'static str,
    pub dark: bool,
    pub bg: u32,
    /// sidebar, toolbar, title bar
    pub panel: u32,
    /// list rows, fields
    pub surface: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
}

#[allow(clippy::too_many_arguments)]
const fn p(id: &'static str, name: &'static str, dark: bool, bg: u32, panel: u32, surface: u32, text: u32, muted: u32, accent: u32) -> Palette {
    Palette { id, name, dark, bg, panel, surface, text, muted, accent }
}

/// Which group of the picker a theme is listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Tsubame,
    Dark,
    Light,
}

/// Every theme the picker shows, in picker order: Tsubame (2), dark (12), light (5).
pub static THEMES: &[Palette] = &[
    p("tsubame-dark", "Tsubame Dark", true, 0x0b0f16, 0x121722, 0x0e131b, 0xe9e6df, 0xa3adbf, 0x6aa0ff),
    p("tsubame-light", "Tsubame Light", false, 0xdde2ea, 0xeef0f4, 0xffffff, 0x161b26, 0x525d72, 0x1b52bf),
    p("catppuccin-mocha", "Catppuccin Mocha", true, 0x1e1e2e, 0x181825, 0x262637, 0xcdd6f4, 0xa6adc8, 0xcba6f7),
    p("catppuccin-macchiato", "Catppuccin Macchiato", true, 0x24273a, 0x1e2030, 0x2b2e43, 0xcad3f5, 0xa5adcb, 0xc6a0f6),
    p("rose-pine", "Ros\u{e9} Pine", true, 0x191724, 0x16141f, 0x1f1d2e, 0xe0def4, 0x908caa, 0xebbcba),
    p("rose-pine-moon", "Ros\u{e9} Pine Moon", true, 0x232136, 0x1d1b2e, 0x2a273f, 0xe0def4, 0x908caa, 0xea9a97),
    p("tokyo-night", "Tokyo Night", true, 0x1a1b26, 0x16161e, 0x1f2335, 0xc0caf5, 0x9aa5ce, 0x7aa2f7),
    p("dracula", "Dracula", true, 0x282a36, 0x21222c, 0x303241, 0xf8f8f2, 0xa3a7c2, 0xbd93f9),
    p("nord", "Nord", true, 0x2e3440, 0x272c36, 0x353c4a, 0xeceff4, 0xa3acbf, 0x88c0d0),
    p("gruvbox-dark", "Gruvbox Dark", true, 0x282828, 0x1d2021, 0x32302f, 0xebdbb2, 0xa89984, 0xfabd2f),
    p("everforest", "Everforest", true, 0x2d353b, 0x272e33, 0x343f44, 0xd3c6aa, 0x9da9a0, 0xa7c080),
    p("one-dark", "One Dark", true, 0x282c34, 0x21252b, 0x2c313a, 0xabb2bf, 0x8b92a0, 0x61afef),
    p("ristretto", "Ristretto", true, 0x2c2525, 0x211c1c, 0x352e2e, 0xfff1f3, 0xc3b7b8, 0xf9cc6c),
    p("kanagawa", "Kanagawa", true, 0x1f1f28, 0x16161d, 0x2a2a37, 0xdcd7ba, 0xa09c8c, 0x7e9cd8),
    p("catppuccin-latte", "Catppuccin Latte", false, 0xeff1f5, 0xe6e9ef, 0xe6e9ef, 0x4c4f69, 0x6c6f85, 0x8839ef),
    p("rose-pine-dawn", "Ros\u{e9} Pine Dawn", false, 0xfaf4ed, 0xf2e9e1, 0xfffaf3, 0x575279, 0x797593, 0xd7827e),
    p("tokyo-night-day", "Tokyo Night Day", false, 0xe1e2e7, 0xd0d5e3, 0xe9e9ed, 0x3760bf, 0x6172b0, 0x2e7de9),
    p("gruvbox-light", "Gruvbox Light", false, 0xfbf1c7, 0xf2e5bc, 0xf9f5d7, 0x3c3836, 0x7c6f64, 0xaf3a03),
    p("solarized-light", "Solarized Light", false, 0xfdf6e3, 0xeee8d5, 0xf5efdc, 0x475b62, 0x657b83, 0x268bd2),
];

/// Accent swatches of the picker (the first entry, "theme default", is not a colour).
pub const ACCENTS: &[u32] = &[0x6aa0ff, 0xf97316, 0xe0492c, 0xcba6f7, 0xf38ba8, 0x2fb3a2, 0xa7c080, 0xfabd2f];

impl Palette {
    pub fn group(&self) -> Group {
        if self.id.starts_with("tsubame") {
            Group::Tsubame
        } else if self.dark {
            Group::Dark
        } else {
            Group::Light
        }
    }
}

/// The theme with config id `id` (`dark` / `light` are the old names of the two
/// Tsubame themes). Case and `_`/space are forgiven.
pub fn find(id: &str) -> Option<&'static Palette> {
    let key: String = id.trim().chars().map(|c| if c == ' ' || c == '_' { '-' } else { c.to_ascii_lowercase() }).collect();
    let key = match key.as_str() {
        "dark" => "tsubame-dark",
        "light" => "tsubame-light",
        other => other,
    };
    THEMES.iter().find(|t| t.id == key).or_else(|| EXTRA.read().ok().and_then(|e| e.iter().copied().find(|t| t.id == key)))
}

// ---- themes from extension packs ---------------------------------------------------------

/// The themes extension packs add (`docs/extension-packs.md`), listed after the built-in ones.
static EXTRA: RwLock<Vec<&'static Palette>> = RwLock::new(Vec::new());
/// Every pack palette ever made. A palette is leaked once (`Palette` holds `&'static str`), and a
/// reload that makes the same one again reuses it, so the leak is bounded by the distinct themes seen.
static MADE: Mutex<Vec<&'static Palette>> = Mutex::new(Vec::new());

/// A theme from a pack: the colours are already filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackPalette {
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub bg: u32,
    pub panel: u32,
    pub surface: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
}

/// Replaces the pack themes. Ids that clash with a built-in theme or repeat are skipped.
pub fn set_extra(list: &[PackPalette]) {
    let mut out: Vec<&'static Palette> = Vec::with_capacity(list.len());
    if let Ok(mut made) = MADE.lock() {
        for pp in list {
            if THEMES.iter().any(|t| t.id == pp.id) || out.iter().any(|t| t.id == pp.id) {
                continue;
            }
            let same = |m: &&&'static Palette| {
                m.id == pp.id && m.name == pp.name && m.dark == pp.dark && (m.bg, m.panel, m.surface, m.text, m.muted, m.accent) == (pp.bg, pp.panel, pp.surface, pp.text, pp.muted, pp.accent)
            };
            let pal = match made.iter().find(same) {
                Some(p) => *p,
                None => {
                    let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
                    let p: &'static Palette = Box::leak(Box::new(Palette { id: leak(&pp.id), name: leak(&pp.name), dark: pp.dark, bg: pp.bg, panel: pp.panel, surface: pp.surface, text: pp.text, muted: pp.muted, accent: pp.accent }));
                    made.push(p);
                    p
                }
            };
            out.push(pal);
        }
    }
    if let Ok(mut e) = EXTRA.write() {
        *e = out;
    }
}

/// Every theme the picker shows: the built-in ones, then the packs'.
pub fn all() -> Vec<&'static Palette> {
    let mut v: Vec<&'static Palette> = THEMES.iter().collect();
    if let Ok(e) = EXTRA.read() {
        v.extend(e.iter().copied());
    }
    v
}

/// The Tsubame theme of the same brightness (what "Light" / "Dark" colour mode picks).
pub fn tsubame(dark: bool) -> &'static Palette {
    &THEMES[if dark { 0 } else { 1 }]
}

/// `a * t + b * (1 - t)` per channel.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let x = ((a >> s) & 0xff) as f32 * t + ((b >> s) & 0xff) as f32 * (1.0 - t);
        (x.round().clamp(0.0, 255.0) as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Relative luminance (0..1, sRGB-weighted); `contrast` is built on it.
pub fn luminance(c: u32) -> f32 {
    let lin = |v: u32| {
        let s = v as f32 / 255.0;
        if s <= 0.039_28 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin((c >> 16) & 0xff) + 0.7152 * lin((c >> 8) & 0xff) + 0.0722 * lin(c & 0xff)
}

/// WCAG contrast ratio of two colours (1..21).
pub fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Black or white, whichever reads better on `bg`.
pub fn on_color(bg: u32) -> u32 {
    if contrast(bg, 0x0b0f16) >= contrast(bg, 0xffffff) { 0x0b0f16 } else { 0xffffff }
}

impl Theme {
    /// The theme of a palette. The Tsubame pair is the generated token set;
    /// the others take its metrics and the dark or light helper colours
    /// (`danger`, `folder`, `highlight`) and derive the surfaces.
    pub fn from_palette(pal: &Palette) -> Theme {
        if pal.id == "tsubame-dark" {
            return DARK;
        }
        if pal.id == "tsubame-light" {
            return LIGHT;
        }
        let mut t = if pal.dark { DARK } else { LIGHT };
        t.name = pal.name;
        t.bg = pal.bg;
        t.panel = pal.panel;
        t.row = pal.surface;
        t.row_alt = mix(pal.surface, pal.text, 0.965);
        t.popup = mix(pal.panel, pal.text, 0.95);
        t.field = mix(pal.bg, pal.surface, 0.5);
        t.hover = mix(pal.panel, pal.text, 0.93);
        t.text = pal.text;
        t.text_dim = pal.muted;
        t.text_disabled = mix(pal.muted, pal.bg, 0.55);
        t.border = mix(pal.panel, pal.text, 0.9);
        t.popup_border = mix(pal.panel, pal.text, 0.82);
        t.scrollbar = mix(pal.panel, pal.text, 0.78);
        t.scrollbar_hover = mix(pal.panel, pal.text, 0.58);
        t.with_accent(pal.accent)
    }

    /// This theme with accent colour `rgb`: the accent, the colour on it, the
    /// focus ring and the two selection fills follow it.
    pub fn with_accent(mut self, rgb: u32) -> Theme {
        self.accent = rgb;
        self.on_accent = on_color(rgb);
        self.focus = if self.is_dark() { mix(rgb, 0xffffff, 0.8) } else { rgb };
        self.selected = mix(rgb, self.row, 0.22);
        self.selected_inactive = mix(rgb, self.row, 0.12);
        self
    }

    /// Text sizes times `scale` (the Settings font-size slider), on top of the density.
    pub fn with_font_scale(mut self, scale: f32) -> Theme {
        let s = scale.clamp(0.8, 1.6);
        if (s - 1.0).abs() > f32::EPSILON {
            self.font_size_xs = (self.font_size_xs * s).round();
            self.font_size_sm = (self.font_size_sm * s).round();
            self.font_size_md = (self.font_size_md * s).round();
            self.font_size_lg = (self.font_size_lg * s).round();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_the_mock_s_themes() {
        assert_eq!(THEMES.len(), 19);
        assert_eq!(THEMES.iter().filter(|t| t.group() == Group::Tsubame).count(), 2);
        assert_eq!(THEMES.iter().filter(|t| t.group() == Group::Dark).count(), 12);
        assert_eq!(THEMES.iter().filter(|t| t.group() == Group::Light).count(), 5);
        for (i, a) in THEMES.iter().enumerate() {
            assert!(THEMES[i + 1..].iter().all(|b| b.id != a.id), "{} twice", a.id);
        }
    }

    #[test]
    fn ids_resolve_with_the_old_names() {
        assert_eq!(find("dark").unwrap().id, "tsubame-dark");
        assert_eq!(find("Light").unwrap().id, "tsubame-light");
        assert_eq!(find("Rose Pine").unwrap().id, "rose-pine");
        assert_eq!(find("TOKYO_NIGHT").unwrap().id, "tokyo-night");
        assert!(find("nope").is_none());
    }

    #[test]
    fn brightness_flag_matches_the_colours() {
        for p in THEMES {
            let t = Theme::from_palette(p);
            assert_eq!(t.is_dark(), p.dark, "{}", p.id);
            assert_eq!(luminance(p.bg) < 0.5, p.dark, "{}", p.id);
        }
    }

    #[test]
    fn text_is_readable_on_every_surface() {
        for p in THEMES {
            let t = Theme::from_palette(p);
            for (what, bg) in [("bg", t.bg), ("panel", t.panel), ("row", t.row), ("popup", t.popup), ("selected", t.selected)] {
                let c = contrast(t.text, bg);
                let min = 3.5; // Tokyo Night Day itself is 4.0 on its panel
                assert!(c >= min, "{}: text on {what} is {c:.1}", p.id);
            }
            assert!(contrast(t.text_dim, t.row) >= 3.0, "{}: muted text", p.id);
            assert!(contrast(t.on_accent, t.accent) >= 4.0, "{}: text on accent", p.id);
        }
    }

    #[test]
    fn the_tsubame_pair_is_the_token_set() {
        assert_eq!(Theme::from_palette(tsubame(true)), DARK);
        assert_eq!(Theme::from_palette(tsubame(false)), LIGHT);
    }

    #[test]
    fn accent_follows_and_font_scale_never_compounds() {
        let t = Theme::from_palette(find("nord").unwrap()).with_accent(0xe0492c);
        assert_eq!(t.accent, 0xe0492c);
        assert_ne!(t.selected, Theme::from_palette(find("nord").unwrap()).selected);
        let base = DARK;
        let a = base.with_font_scale(1.25);
        assert_eq!(a.font_size_sm, 15.0);
        assert_eq!(base.with_font_scale(1.0), base);
        assert_eq!(base.with_font_scale(9.0).font_size_sm, (12.0f32 * 1.6).round());
    }

    #[test]
    fn mix_ends() {
        assert_eq!(mix(0x102030, 0xffffff, 1.0), 0x102030);
        assert_eq!(mix(0x102030, 0xffffff, 0.0), 0xffffff);
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
    }
}

#[cfg(test)]
mod pack_tests {
    use super::*;

    fn pp(id: &str, bg: u32) -> PackPalette {
        PackPalette { id: id.into(), name: "Mine".into(), dark: true, bg, panel: 2, surface: 3, text: 4, muted: 5, accent: 6 }
    }

    #[test]
    fn pack_themes_are_found_listed_and_replaced() {
        let id = "pack.t-find.mine";
        assert!(find(id).is_none());
        set_extra(&[pp(id, 1), pp("tsubame-dark", 9), pp(id, 7)]);
        let p = find(id).expect("found");
        assert_eq!((p.name, p.bg, p.dark), ("Mine", 1, true), "a repeated id and a built-in id are skipped");
        assert_eq!(all().len(), THEMES.len() + 1);
        assert_eq!(find("tsubame-dark").unwrap().bg, 0x0b0f16);
        assert_eq!(Theme::from_palette(p).bg, 1);
        // the same theme again is the same leaked palette; a changed one is a new one
        let again = {
            set_extra(&[pp(id, 1)]);
            find(id).unwrap() as *const Palette
        };
        assert_eq!(again, p as *const Palette);
        set_extra(&[pp(id, 8)]);
        assert_eq!(find(id).unwrap().bg, 8);
        set_extra(&[]);
        assert!(find(id).is_none());
        assert_eq!(all().len(), THEMES.len());
    }
}
