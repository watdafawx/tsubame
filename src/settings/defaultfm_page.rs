//! Settings > Default file manager (opt-in, R1 Tier 1): one per-user registry verb that makes
//! folder opens go to Tsubame through the fail-safe shim, and one button that removes it.
//!
//! The registry is only read when the page opens or "Refresh" is pressed, and only written by
//! the two buttons (never by a test: `WinRegistry` refuses writes under `cfg(test)`). With no
//! shim next to the exe the switch stays off and says why.

use super::lay::{Look, Pg, button, fit, wrap};
use super::Cx;
use tsubame_ui::{Id, Pt, Rect};
use tsubame_win::defaultfm::{self, Availability, Status, WinRegistry};

const ID_REFRESH: Id = Id::new("dfm-refresh");
const ID_RESTORE: Id = Id::new("dfm-restore");

#[derive(Default)]
pub struct State {
    status: Option<Status>,
    avail: Option<Availability>,
    /// the last action's result
    note: Option<(String, bool)>,
}

impl State {
    fn refresh(&mut self, install_dir: &std::path::Path) {
        let a = defaultfm::availability(install_dir);
        let shim = match &a {
            Availability::Ready { shim } | Availability::Missing { shim, .. } => shim.clone(),
        };
        self.status = Some(defaultfm::status(&WinRegistry, &shim));
        self.avail = Some(a);
    }
}

/// "What the page calls the current owner".
pub fn owner_text(s: &Status) -> String {
    match s {
        Status::Explorer => "File Explorer".to_string(),
        Status::Tsubame => "Tsubame".to_string(),
        Status::TsubameStale { .. } => "Tsubame (its helper is missing: restore Explorer)".to_string(),
        Status::Other(name) => format!("another app ({name})"),
    }
}

fn on(s: &Status) -> bool {
    matches!(s, Status::Tsubame | Status::TsubameStale { .. })
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("Default file manager", "Optional. Off by default. Nothing here changes Windows until you switch it on, and one click puts everything back.");
    let t = pg.t;
    if st.status.is_none() {
        st.refresh(&cx.env.install_dir);
    }
    let status = st.status.clone().unwrap_or(Status::Explorer);
    let avail = st.avail.clone().unwrap_or(Availability::Missing { shim: Default::default(), what: "unknown" });

    // state card
    let r = Rect::new(pg.x, pg.y, pg.w, 92.0);
    pg.g.round(r, t.radius_lg, t.panel);
    pg.g.round_stroke(r, t.radius_lg, t.border, 1.0);
    let led = if on(&status) { 0x57ab5a } else { t.text_disabled };
    pg.g.circle(r.x + 22.0, r.y + 24.0, 5.0, led);
    let head = format!("Tsubame handles folder opens: {}", if on(&status) { "On" } else { "Off" });
    pg.g.text(&head, Rect::new(r.x + 38.0, r.y + 14.0, r.w - 160.0, 20.0), t.font_md().b(), t.text);
    if button(pg.ui, pg.g, ID_REFRESH, r.r() - 16.0, r.y + 10.0, "Refresh", Look::Normal, true, true).0 {
        st.refresh(&cx.env.install_dir);
        st.note = None;
    }
    pg.g.text("Windows currently opens folders with", Rect::new(r.x + 38.0, r.y + 44.0, 260.0, 16.0), t.font_small(), t.text_dim);
    pg.g.text(&owner_text(&status), Rect::new(r.x + 300.0, r.y + 44.0, r.w - 316.0, 16.0), t.font_small(), t.text);
    pg.g.text("Checked in the registry", Rect::new(r.x + 38.0, r.y + 64.0, 260.0, 16.0), t.font_small(), t.text_dim);
    pg.g.text("HKCU\\Software\\Classes\\Directory\\shell", Rect::new(r.x + 300.0, r.y + 64.0, r.w - 316.0, 16.0), t.font_mono(), t.text);
    pg.y += 92.0 + 12.0;

    pg.section("Use Tsubame instead of File Explorer for folders");
    let lines = wrap(pg.g, "Windows has no supported setting for this. Tsubame makes a per-user registry change so that folders open in Tsubame. It only affects your Windows account, needs no administrator rights, and \u{201c}Restore Windows Explorer\u{201d} undoes it.", t.font_small(), pg.w - 32.0);
    let mut c = pg.card_h(16.0 + lines.len() as f32 * 16.0 + 4.0 + 52.0 + 52.0, 52.0);
    for (i, l) in lines.iter().enumerate() {
        pg.g.text(l, Rect::new(c.r.x + 16.0, c.r.y + 12.0 + i as f32 * 16.0, c.r.w - 32.0, 16.0), t.font_small(), t.text_dim);
    }
    c.restart_at(c.r.y + 16.0 + lines.len() as f32 * 16.0 + 4.0);
    let ready = matches!(avail, Availability::Ready { .. });
    let desc = if ready { "Double-clicking a folder, the Run dialog and \u{201c}open folder\u{201d} in other apps will open Tsubame.".to_string() } else { "Not available yet: the fail-safe helper (tsubame-handler.exe) is not built yet.".to_string() };
    let row_y = c.r.y + 16.0 + lines.len() as f32 * 16.0 + 4.0;
    let flipped = pg.toggle_row(&mut c, "Open folders in Tsubame", &desc, on(&status), ready);
    if !ready {
        // tooltip over the disabled switch
        let tr = Rect::new(c.r.r() - 16.0 - 34.0, row_y + 17.0, 34.0, 18.0);
        if tr.contains_pt(pg.ui.mouse()) {
            crate::overlay::tooltip(pg.g, &t, pg.ui.size(), Pt { x: tr.x - 60.0, y: tr.b() + 6.0 }, "shim not built yet", &[]);
        }
    }
    if let Some(want) = flipped {
        let res = if want {
            match &avail {
                Availability::Ready { shim } => defaultfm::enable(&mut WinRegistry, shim, &cx.env.exe),
                Availability::Missing { what, .. } => Err((*what).to_string()),
            }
        } else {
            defaultfm::disable(&mut WinRegistry)
        };
        st.note = Some(match res {
            Ok(()) => (if want { "Folders now open in Tsubame." } else { "Windows Explorer restored." }.to_string(), false),
            Err(e) => (format!("Could not change it: {e}"), true),
        });
        st.refresh(&cx.env.install_dir);
    }
    let row = pg.next_row(&mut c);
    pg.row_text(row, "Restore Windows Explorer", "Removes every change this page made, then checks the registry. Safe to press even if nothing was changed.", 250.0, true);
    if button(pg.ui, pg.g, ID_RESTORE, row.r() - 16.0, row.y + (row.h - t.height_button) / 2.0, "Restore Windows Explorer", Look::Primary, true, true).0 {
        st.note = Some(match defaultfm::disable(&mut WinRegistry) {
            Ok(()) => ("Windows Explorer restored.".to_string(), false),
            Err(e) => (format!("Could not restore it: {e}"), true),
        });
        st.refresh(&cx.env.install_dir);
    }
    if let Some((msg, bad)) = &st.note {
        let m = fit(pg.g, msg, t.font(), pg.w - 24.0);
        pg.g.text(&m, Rect::new(pg.x + 4.0, pg.y, pg.w, 20.0), t.font(), if *bad { t.danger } else { t.text_dim });
        pg.y += 26.0;
    }
    pg.note("\u{E946}", "What still opens File Explorer: drives, This PC, the Recycle Bin, Control Panel, the taskbar Explorer icon and every app\u{2019}s Open/Save dialog. Windows does not allow replacing those.", false);
    pg.note("\u{E83D}", "Fail-safe: if Tsubame cannot start (deleted, quarantined, broken), a tiny helper opens File Explorer instead and switches this off by itself. If you are ever stuck, press Win+R and type explorer.exe. Uninstalling restores Explorer first. If another app changes this later, Tsubame says so here and never re-applies it.", false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_names() {
        assert_eq!(owner_text(&Status::Explorer), "File Explorer");
        assert_eq!(owner_text(&Status::Tsubame), "Tsubame");
        assert!(owner_text(&Status::Other("Foo".into())).contains("Foo"));
        assert!(on(&Status::TsubameStale { command: String::new() }));
        assert!(!on(&Status::Explorer));
    }
}
