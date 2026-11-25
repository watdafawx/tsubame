//! Settings > Extensions > File age colours (P4.F6, `mocks/mock-f6-age.html`).
//!
//! Every control writes `prefs.file_age` and the next frame shows it. The
//! thresholds are typed: a set that is not valid (`tsubame_config::thresholds_error`)
//! is not stored, the previous valid values stay in force and the row says why.

use super::lay::{Look, Pg, button, fit};
use super::{C_OTHER, Cx};
use tsubame_config::{AgePalette, AgeStyle, AgeToday, FileAge, thresholds_error};
use tsubame_ui::{Id, Rect, Sense, TextInput, text_input};

const ID_W: Id = Id::new("age-week");
const ID_M: Id = Id::new("age-month");
const ID_Y: Id = Id::new("age-year");
const ID_RESET: Id = Id::new("age-reset");
const ID_PAL: Id = Id::new("age-pal");

#[derive(Default)]
pub struct State {
    inputs: [TextInput; 3],
    inited: bool,
    /// the message under the thresholds (`None`: the typed set is valid)
    pub err: Option<String>,
}

const PALETTES: [(AgePalette, &str); 3] = [(AgePalette::Heat, "Heat"), (AgePalette::ColourBlind, "Colour-blind safe"), (AgePalette::Mono, "Monochrome")];

/// Reads the three typed day counts; the error text when one is not a number or the set is invalid.
pub fn check(texts: [&str; 3]) -> Result<(u32, u32, u32), String> {
    let mut v = [0u32; 3];
    for (i, t) in texts.iter().enumerate() {
        v[i] = t.trim().parse::<u32>().map_err(|_| "Enter whole numbers of days.".to_string())?;
    }
    match thresholds_error(v[0], v[1], v[2]) {
        None => Ok((v[0], v[1], v[2])),
        Some(e) => Err(e),
    }
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    let t = pg.t;
    if !st.inited {
        let f = &cx.prefs.file_age;
        for (i, d) in [f.week_days, f.month_days, f.year_days].into_iter().enumerate() {
            st.inputs[i].set_text(&d.to_string());
        }
        st.inited = true;
    }
    let ext_on = crate::age::is_enabled();
    pg.section("File age colours");
    let mut c = pg.card(8, 52.0);
    let fa = &mut cx.prefs.file_age;
    if let Some(v) = pg.toggle_row(&mut c, "Colour files by age", "Tints the Modified date (or the row) by how recently the file changed. Off by default.", fa.enabled, ext_on) {
        fa.enabled = v;
        cx.changed |= C_OTHER;
    }
    let style = match fa.style {
        AgeStyle::Text => 0,
        AgeStyle::Chip => 1,
        AgeStyle::Accent => 2,
    };
    if let Some(i) = pg.seg_row(&mut c, "Style", "Date text costs no extra drawing. Chip tints a rect behind the date. Row accent draws a bar.", &["Date text", "Date chip", "Row accent"], style) {
        fa.style = [AgeStyle::Text, AgeStyle::Chip, AgeStyle::Accent][i];
        cx.changed |= C_OTHER;
    }
    // palette cards
    let row = pg.next_row(&mut c);
    // cards shrink in a narrow window so they never cover the label
    let cw = ((row.w - 32.0 - 150.0 - 16.0) / 3.0).clamp(84.0, 124.0);
    let step = cw + 8.0;
    pg.row_text(row, "Palette", "Every palette keeps the date at 4.5:1 or better. The last two also show pips.", 3.0 * step + 16.0, true);
    let dark = t.row < 0x80_8080;
    for (i, (p, name)) in PALETTES.iter().enumerate() {
        let r = Rect::new(row.r() - 16.0 - (3 - i) as f32 * step + 8.0, row.y + 6.0, cw, row.h - 12.0);
        let it = pg.ui.interact(ID_PAL.with(i as u64), r, Sense::click());
        let sel = fa.palette == *p;
        pg.g.round(r, t.radius_md, if sel { t.selected } else if it.hovered { t.hover } else { t.field });
        pg.g.round_stroke(r, t.radius_md, if sel { t.focus } else { t.border }, 1.0);
        pg.g.text(name, Rect::new(r.x + 8.0, r.y + 2.0, r.w - 12.0, 16.0), t.font_small().b(), t.text);
        for (k, col) in crate::age::palette_colours(*p, dark).iter().enumerate() {
            let sw = ((cw - 16.0) / 5.0 - 3.0).clamp(10.0, 18.0);
            pg.g.round(Rect::new(r.x + 8.0 + k as f32 * (sw + 3.0), r.y + 22.0, sw, 10.0), 2.0, *col);
        }
        if it.clicked == Some(tsubame_ui::Button::Left) && !sel {
            fa.palette = *p;
            cx.changed |= C_OTHER;
        }
    }
    if let Some(v) = pg.toggle_row(&mut c, "Show pips", "Four small bars before the date (4 = today, 0 = older). Always on for the accessible palettes.", fa.shows_pips(), fa.palette == AgePalette::Heat) {
        fa.pips = v;
        cx.changed |= C_OTHER;
    }
    let today = usize::from(fa.today == AgeToday::Last24h);
    if let Some(i) = pg.seg_row(&mut c, "Today means", "Since local midnight, or the last 24 hours.", &["Since midnight", "Last 24 h"], today) {
        fa.today = if i == 0 { AgeToday::Midnight } else { AgeToday::Last24h };
        cx.changed |= C_OTHER;
    }
    // thresholds
    let row = pg.next_row(&mut c);
    let reserve = 3.0 * 118.0 + 10.0;
    let w = (row.w - 32.0 - reserve).max(80.0);
    pg.g.text("Thresholds", Rect::new(row.x + 16.0, row.y + 9.0, w, 18.0), t.font_md(), t.text);
    let (uw, um, uy) = fa.thresholds();
    match &st.err {
        Some(e) => {
            let e = fit(pg.g, &format!("{e} Using {uw} / {um} / {uy}."), t.font_small(), w);
            pg.g.text(&e, Rect::new(row.x + 16.0, row.y + 28.0, w, 16.0), t.font_small(), t.danger);
        }
        None => {
            let d = fit(pg.g, "Days since the file changed. Each limit must be longer than the one before.", t.font_small(), w);
            pg.g.text(&d, Rect::new(row.x + 16.0, row.y + 28.0, w, 16.0), t.font_small(), t.text_dim);
        }
    }
    let mut changed = false;
    for (i, (id, label)) in [(ID_W, "Week \u{2264}"), (ID_M, "Month \u{2264}"), (ID_Y, "Year \u{2264}")].into_iter().enumerate() {
        let x0 = row.r() - 16.0 - (3 - i) as f32 * 118.0;
        pg.g.text(label, Rect::new(x0, row.y, 56.0, row.h), t.font_small(), t.text_dim);
        let fr = Rect::new(x0 + 56.0, row.y + (row.h - 26.0) / 2.0, 50.0, 26.0);
        let resp = text_input(pg.ui, pg.g, id, fr, &mut st.inputs[i], "");
        if pg.ui.has_focus(id) && pg.ui.window_focused() {
            pg.ui.mark_text_focus();
        }
        changed |= resp.changed;
    }
    if changed {
        match check([&st.inputs[0].text, &st.inputs[1].text, &st.inputs[2].text]) {
            Ok((w, m, y)) => {
                fa.week_days = w;
                fa.month_days = m;
                fa.year_days = y;
                st.err = None;
                cx.changed |= C_OTHER;
            }
            // the previous valid values stay; the row says why
            Err(e) => st.err = Some(e),
        }
    }
    if let Some(v) = pg.toggle_row(&mut c, "Colour folders too", "A folder date only changes when its direct children change, so it can mislead. Off by default.", fa.folders, true) {
        fa.folders = v;
        cx.changed |= C_OTHER;
    }
    let row = pg.next_row(&mut c);
    pg.row_text(row, "Date used", "Modified, read from the folder listing (no extra reads). Created and Accessed are not offered yet.", 110.0, true);
    if button(pg.ui, pg.g, ID_RESET, row.r() - 16.0, row.y + (row.h - t.height_button) / 2.0, "Reset", Look::Normal, true, true).0 {
        *fa = FileAge { enabled: fa.enabled, ..FileAge::default() };
        st.inited = false;
        st.err = None;
        cx.changed |= C_OTHER;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_thresholds_are_checked() {
        assert_eq!(check(["7", "30", "365"]), Ok((7, 30, 365)));
        assert_eq!(check([" 3 ", "20", "100"]), Ok((3, 20, 100)));
        assert!(check(["7", "6", "365"]).unwrap_err().contains("longer than Week"));
        assert!(check(["7", "30", "abc"]).unwrap_err().contains("whole numbers"));
        assert!(check(["", "30", "365"]).is_err());
        assert!(check(["0", "30", "365"]).is_err());
        assert!(check(["-1", "30", "365"]).is_err());
    }
}
