//! Settings > Advanced: where the settings live, the diagnostics log, a live memory readout,
//! and "Reset all settings" (two clicks).

use super::lay::{Look, Pg, button};
use super::{C_OTHER, Cx};
use tsubame_ui::{Id, Rect};

const ID_OPEN: Id = Id::new("adv-open");
const ID_RESET: Id = Id::new("adv-reset");
const ID_YES: Id = Id::new("adv-reset-yes");
const ID_NO: Id = Id::new("adv-reset-no");

#[derive(Default)]
pub struct State {
    confirm: bool,
}

pub fn mb(bytes: usize) -> String {
    format!("{:.0} MB", bytes as f64 / 1_048_576.0)
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("Advanced", "Diagnostics and where things are kept. The defaults are fine for nearly everyone.");
    let t = pg.t;

    pg.section("This copy");
    let mut c = pg.card(2, 52.0);
    let row = pg.next_row(&mut c);
    let kind = if cx.env.portable { "Portable copy" } else { "Installed for this user" };
    pg.row_text(row, kind, if cx.env.portable { "Settings and session sit next to the program, so the folder can move with it." } else { "Settings and session are kept in your user profile." }, 170.0, true);
    if button(pg.ui, pg.g, ID_OPEN, row.r() - 16.0, row.y + (row.h - t.height_button) / 2.0, "Open settings folder", Look::Normal, true, true).0 {
        cx.open.push(cx.env.data_dir.display().to_string());
    }
    let row = pg.next_row(&mut c);
    pg.row_text(row, "Settings file", &cx.env.config_path.display().to_string(), 20.0, true);

    pg.section("Diagnostics");
    let mut c = pg.card(2, 52.0);
    if let Some(v) = pg.toggle_row(&mut c, "Write a diagnostics log", "Errors and timings only, never file names. Off by default; takes effect at the next start.", cx.prefs.advanced.diagnostics, true) {
        cx.prefs.advanced.diagnostics = v;
        cx.changed |= C_OTHER;
    }
    let row = pg.next_row(&mut c);
    let pb = cx.private_bytes;
    pg.row_text(row, "Memory (live)", "Private bytes of this process. Budget: 90 MB in a window, 110 MB on a 4K screen.", 120.0, true);
    let bar = Rect::new(row.r() - 16.0 - 100.0, row.y + row.h / 2.0 + 10.0, 100.0, 4.0);
    pg.g.round(bar, 2.0, t.scrollbar);
    let frac = (pb as f32 / (110.0 * 1_048_576.0)).clamp(0.0, 1.0);
    pg.g.round(Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 2.0, if pb > 110 * 1_048_576 { t.danger } else { t.accent });
    pg.g.text(&mb(pb), Rect::new(row.r() - 16.0 - 100.0, row.y + 8.0, 100.0, 20.0), t.font_md().r(), t.text);

    pg.section("Reset");
    if !st.confirm {
        let mut c = pg.card(1, 64.0);
        let row = pg.next_row(&mut c);
        pg.row_text(row, "Reset all settings", "Restores every setting, shortcut, context-menu edit and theme choice. Files and tabs are not touched. The old config.toml is kept as config.toml.bak.", 150.0, true);
        if button(pg.ui, pg.g, ID_RESET, row.r() - 16.0, row.y + (row.h - t.height_button) / 2.0, "Reset all\u{2026}", Look::Danger, true, true).0 {
            st.confirm = true;
        }
    } else {
        pg.note("\u{E7BA}", "Reset every setting? All settings, shortcuts, context-menu edits and the theme go back to the defaults. Files and tabs are untouched; config.toml.bak keeps your old settings.", true);
        let y = pg.y;
        let (yes, yr) = button(pg.ui, pg.g, ID_YES, pg.x + pg.w, y, "Yes, reset everything", Look::DangerFill, true, true);
        let (no, _) = button(pg.ui, pg.g, ID_NO, yr.x - 8.0, y, "Cancel", Look::Normal, true, true);
        pg.y += t.height_button + 12.0;
        if yes {
            st.confirm = false;
            cx.reset_all = true;
        }
        if no {
            st.confirm = false;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn megabytes() {
        assert_eq!(super::mb(96 * 1_048_576), "96 MB");
    }
}
