//! Settings > Appearance: colour mode, theme cards, accent, row density with live previews,
//! zebra rows and the font size.

use super::lay::Pg;
use super::{C_APPEARANCE, C_ZEBRA, Cx};
use tsubame_config::Density as CfgDensity;
use tsubame_ui::themes::{self, ACCENTS, Group, Palette};
use tsubame_ui::{Button, Canvas, Cursor, Id, Rect, Sense, Theme, f, slider};

const ID_CARD: Id = Id::new("theme-card");
const ID_ACCENT: Id = Id::new("accent");
const ID_DENS: Id = Id::new("density-card");
const ID_FONT: Id = Id::new("font-slider");

#[derive(Default)]
pub struct State {
    /// "Match Windows" was picked: the theme was set once from the system mode (it does not follow changes while running)
    pub match_windows: bool,
}

/// Font size shown by the slider (px) for a scale.
pub fn px_of(scale: f32) -> f32 {
    (12.0 * scale).round().clamp(10.0, 18.0)
}
pub fn scale_of(px: f32) -> f32 {
    px / 12.0
}

pub fn accent_hex(rgb: u32) -> String {
    format!("#{:06x}", rgb & 0xffffff)
}

/// One theme card: a miniature window in the theme's own colours.
fn card(pg: &mut Pg, p: &Palette, r: Rect, cur: bool, hot: bool) {
    let t = pg.t;
    let g = &mut *pg.g;
    g.round(r, t.radius_md, p.surface);
    let side = Rect::new(r.x, r.y, 34.0, r.h);
    g.fill(side, p.panel);
    g.fill(Rect::new(r.x + 34.0, r.y, r.w - 34.0, 12.0), p.panel);
    g.fill(Rect::new(r.x + 4.0, r.y + 8.0, 18.0, 3.0), p.accent);
    for (y, w) in [(19.0, 22.0), (28.0, 16.0), (37.0, 19.0)] {
        g.fill(Rect::new(r.x + 4.0, r.y + y, w, 3.0), p.muted);
    }
    g.fill(Rect::new(r.x + 42.0, r.y + 19.0, 54.0, 3.0), p.text);
    g.fill(Rect::new(r.x + 34.0, r.y + 26.0, r.w - 34.0, 10.0), themes::mix(p.accent, p.surface, 0.34));
    g.fill(Rect::new(r.x + 42.0, r.y + 29.5, 68.0, 3.0), p.text);
    g.fill(Rect::new(r.x + 42.0, r.y + 46.0, 44.0, 3.0), p.text);
    g.fill(Rect::new(r.x + 42.0, r.y + 56.0, 60.0, 3.0), p.muted);
    g.circle(r.r() - 8.0, r.b() - 8.0, 3.5, p.accent);
    if cur {
        g.round_stroke(r.pad(-1.0), t.radius_md, t.accent, 2.0);
    } else {
        g.round_stroke(r, t.radius_md, if hot { t.scrollbar_hover } else { t.popup_border }, 1.0);
    }
}

fn theme_grid(pg: &mut Pg, cx: &mut Cx, group: Group, label: &str, st: &mut State) {
    let t = pg.t;
    pg.g.text(label, Rect::new(pg.x + 16.0, pg.y, 200.0, 16.0), t.font_small().b(), t.text_dim);
    pg.y += 20.0;
    let (cw, ch) = (t.layout_theme_card_width, t.layout_theme_card_height);
    let pitch = cw + 8.0;
    let per_row = (((pg.w - 32.0 + 8.0) / pitch).floor() as usize).max(1);
    let cur_id = themes::find(&cx.prefs.appearance.theme).map(|p| p.id).unwrap_or("tsubame-dark");
    let list: Vec<&Palette> = themes::all().into_iter().filter(|p| p.group() == group).collect();
    for (n, p) in list.iter().enumerate() {
        let (col, row) = (n % per_row, n / per_row);
        let r = Rect::new(pg.x + 16.0 + col as f32 * pitch, pg.y + row as f32 * (ch + 28.0), cw, ch);
        let id = ID_CARD.with(n as u64 + (group as u64) * 100);
        let it = pg.ui.interact(id, r, Sense::click().focusable().cursor(Cursor::Hand));
        let cur = p.id == cur_id;
        card(pg, p, r, cur, it.hovered);
        if pg.ui.has_focus(id) && pg.ui.window_focused() {
            pg.g.round_stroke(r.pad(-3.0), t.radius_md, t.focus, 1.0);
        }
        pg.g.text(p.name, Rect::new(r.x, r.b() + 4.0, cw - 18.0, 18.0), t.font().b(), t.text);
        if cur {
            pg.g.text("\u{E73E}", Rect::new(r.r() - 16.0, r.b() + 4.0, 16.0, 18.0), f(12.0).icon().c(), t.accent);
        }
        if it.clicked == Some(Button::Left) {
            cx.prefs.appearance.theme = p.id.to_string();
            st.match_windows = false;
            cx.changed |= C_APPEARANCE;
        }
    }
    let rows = list.len().div_ceil(per_row);
    pg.y += rows as f32 * (ch + 28.0) + 2.0;
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("Appearance", "Changes show immediately in every open window. Click a theme to apply it.");
    let t = pg.t;
    let cur = themes::find(&cx.prefs.appearance.theme).unwrap_or_else(|| themes::tsubame(true));

    pg.section("Theme");
    // the card is as tall as the three groups need: header row + 3 groups
    let per_row = (((pg.w - 32.0 + 8.0) / (t.layout_theme_card_width + 8.0)).floor() as usize).max(1);
    let all = themes::all();
    let rows_of = |g: Group| all.iter().filter(|p| p.group() == g).count().div_ceil(per_row);
    let group_h = |g: Group| 20.0 + rows_of(g) as f32 * (t.layout_theme_card_height + 28.0) + 2.0;
    let h = 56.0 + 8.0 + group_h(Group::Tsubame) + group_h(Group::Dark) + group_h(Group::Light) + 8.0;
    let mut c = pg.card_h(h, 56.0);
    let start_y = c.r.y;
    // colour mode
    let mode = if st.match_windows { 0 } else if cur.dark { 2 } else { 1 };
    if let Some(i) = pg.seg_row(&mut c, "Colour mode", "Follow Windows once, or stay light or dark. Picking a theme below sets this to match.", &["Match Windows", "Light", "Dark"], mode) {
        let want_dark = match i {
            0 => cx.env.system_dark,
            1 => false,
            _ => true,
        };
        st.match_windows = i == 0;
        if cur.dark != want_dark {
            cx.prefs.appearance.theme = themes::tsubame(want_dark).id.to_string();
            cx.changed |= C_APPEARANCE;
        }
    }
    pg.y = start_y + 64.0;
    theme_grid(pg, cx, Group::Tsubame, "Tsubame", st);
    theme_grid(pg, cx, Group::Dark, "Dark", st);
    theme_grid(pg, cx, Group::Light, "Light", st);
    pg.y = start_y + h + 12.0;

    // accent
    pg.section("Accent colour");
    let mut c = pg.card(1, 56.0);
    let row = pg.next_row(&mut c);
    pg.row_text(row, "Accent", "Selection bar, focus ring, links and toggles. \"Theme default\" keeps the theme's own.", 9.0 * 30.0 + 16.0, true);
    let cur_acc = cx.prefs.appearance.accent_rgb();
    let mut x = row.r() - 16.0 - 9.0 * 30.0 + 4.0;
    let cy = row.y + row.h / 2.0;
    for n in 0..9usize {
        let col = if n == 0 { cur.accent } else { ACCENTS[n - 1] };
        let r = Rect::new(x, cy - 11.0, 22.0, 22.0);
        let id = ID_ACCENT.with(n as u64);
        let it = pg.ui.interact(id, r, Sense::click().focusable().cursor(Cursor::Hand));
        let on = if n == 0 { cur_acc.is_none() } else { cur_acc == Some(col) };
        pg.g.circle(x + 11.0, cy, 11.0, col);
        if n == 0 {
            // "theme default": the theme's accent with a hollow centre
            pg.g.circle(x + 11.0, cy, 7.0, t.panel);
            pg.g.circle(x + 11.0, cy, 4.0, col);
        }
        if on {
            pg.g.round_stroke(Rect::new(x - 3.0, cy - 14.0, 28.0, 28.0), 14.0, t.text, 2.0);
        }
        if pg.ui.has_focus(id) && pg.ui.window_focused() {
            pg.g.round_stroke(Rect::new(x - 5.0, cy - 16.0, 32.0, 32.0), 16.0, t.focus, 1.0);
        }
        if it.clicked == Some(Button::Left) {
            cx.prefs.appearance.accent = if n == 0 { None } else { Some(accent_hex(col)) };
            cx.changed |= C_APPEARANCE;
        }
        x += 30.0;
    }

    // density
    pg.section("Lists");
    let panel_h = 20.0 + 5.0 * 30.0 + 18.0;
    let h = 52.0 + panel_h + 8.0 + 52.0 + 52.0;
    let mut c = pg.card_h(h, 52.0);
    let start = c.r.y;
    let dens = [("Compact", 22.0, CfgDensity::Compact, " \u{b7} default"), ("Normal", 26.0, CfgDensity::Normal, ""), ("Comfortable", 30.0, CfgDensity::Comfortable, "")];
    let cur_d = cx.prefs.appearance.density;
    if let Some(i) = pg.seg_row(&mut c, "Row density", "Switches on the fly, in every folder. Cycle with Ctrl+Shift+D.", &["Compact", "Normal", "Comfortable"], dens.iter().position(|d| d.2 == cur_d).unwrap_or(0)) {
        cx.prefs.appearance.density = dens[i].2;
        cx.changed |= C_APPEARANCE;
    }
    // previews
    let gap = 14.0;
    let pw = (pg.w - 32.0 - 2.0 * gap) / 3.0;
    let py = start + 52.0 + 4.0;
    for (i, (name, h_row, d, tag)) in dens.iter().enumerate() {
        let x = pg.x + 16.0 + i as f32 * (pw + gap);
        let head = format!("{name}  {h_row} px rows{tag}");
        pg.g.text(&head, Rect::new(x, py, pw, 16.0), t.font_small(), if *d == cur_d { t.accent } else { t.text_dim });
        let list_r = Rect::new(x, py + 20.0, pw, 5.0 * h_row);
        let id = ID_DENS.with(i as u64);
        let it = pg.ui.interact(id, list_r, Sense::click().focusable().cursor(Cursor::Hand));
        preview(pg, list_r, *h_row, cx.prefs.appearance.zebra, &t);
        if *d == cur_d {
            pg.g.round_stroke(list_r.pad(-0.5), t.radius_md, t.accent, 2.0);
        } else if it.hovered {
            pg.g.round_stroke(list_r.pad(-0.5), t.radius_md, t.scrollbar_hover, 1.0);
        }
        if it.clicked == Some(Button::Left) && *d != cur_d {
            cx.prefs.appearance.density = *d;
            cx.changed |= C_APPEARANCE;
        }
    }
    // the rows below the previews start after them
    c.restart_at(start + 52.0 + panel_h + 8.0);
    if let Some(v) = pg.toggle_row(&mut c, "Zebra rows", "Shade every second row slightly. Ctrl+Shift+Z.", cx.prefs.appearance.zebra, true) {
        cx.prefs.appearance.zebra = v;
        cx.changed |= C_ZEBRA;
    }
    // font size
    let row = pg.next_row(&mut c);
    pg.row_text(row, "Font size", "List and menu text. Windows text scaling is applied on top.", t.layout_slider_width + 120.0, true);
    let px = px_of(cx.prefs.appearance.font_scale);
    let sr = Rect::new(row.r() - 16.0 - 60.0 - t.layout_slider_width, row.y + row.h / 2.0 - 8.0, t.layout_slider_width, 16.0);
    pg.g.text("Aa", Rect::new(sr.x - 26.0, row.y, 20.0, row.h), t.font_small(), t.text_dim);
    if let Some(v) = slider(pg.ui, pg.g, ID_FONT, sr, (px - 10.0) / 8.0, 0.125) {
        let new_px = (10.0 + v * 8.0).round();
        let s = scale_of(new_px);
        if (s - cx.prefs.appearance.font_scale).abs() > 1e-3 {
            cx.prefs.appearance.font_scale = s;
            cx.changed |= C_APPEARANCE;
        }
    }
    pg.g.text(&format!("{px:.0} px"), Rect::new(row.r() - 16.0 - 50.0, row.y, 50.0, row.h), t.font_small().r(), t.text_dim);
    pg.y = start + h + 12.0;
}

/// Five sample rows at row height `rh` in the live theme.
fn preview(pg: &mut Pg, r: Rect, rh: f32, zebra: bool, t: &Theme) {
    let g: &mut dyn Canvas = &mut *pg.g;
    g.round(r, t.radius_md, t.row);
    let rows = [("Installers", "", true), ("0412-receipt.pdf", "184 KB", false), ("inventory.xlsx", "212 KB", false), ("Invoice-Acme-2026-10.pdf", "311 KB", false), ("rustup-init.exe", "11 MB", false)];
    g.clip(r);
    for (i, (name, size, dir)) in rows.iter().enumerate() {
        let rr = Rect::new(r.x, r.y + i as f32 * rh, r.w, rh);
        if i == 2 {
            g.fill(rr, t.selected);
        } else if zebra && i % 2 == 1 {
            g.fill(rr, t.row_alt);
        }
        let ic = if *dir { t.folder } else { [0xe5534b, 0x57ab5a, 0x6aa0ff][i % 3] };
        g.round(Rect::new(rr.x + 10.0, rr.y + rh / 2.0 - 6.0, 10.0, 12.0), 2.0, ic);
        g.text(name, Rect::new(rr.x + 28.0, rr.y, rr.w - 80.0, rr.h), t.font(), t.text);
        if !size.is_empty() {
            g.text(size, Rect::new(rr.r() - 70.0, rr.y, 62.0, rr.h), t.font().r(), t.text_dim);
        }
    }
    g.unclip();
}
