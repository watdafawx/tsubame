//! Settings > Updates: this copy's version, the check, the download, roll back. The state is
//! the updater's (`updates::snapshot`); this page only draws it and asks for things.

use super::lay::{Look, Pg, button, button_in, fit};
use super::{C_UPDATES, Cx};
use crate::updates::{self, OffReason, Phase};
use tsubame_ui::{Id, Rect, f};

const ID_CHECK: Id = Id::new("upd-check");
const ID_INSTALL: Id = Id::new("upd-install");
const ID_NOTES: Id = Id::new("upd-notes");
const ID_CANCEL: Id = Id::new("upd-cancel");
const ID_RESTART: Id = Id::new("upd-restart");
const ID_ROLL: Id = Id::new("upd-roll");

#[derive(Default)]
pub struct State {
    /// "Roll back" asked once
    confirm_roll: bool,
}

fn mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1_048_576.0)
}

/// One line for the phase (what the user reads under the version).
pub fn status_line(phase: &Phase, last: &str) -> (String, bool) {
    match phase {
        Phase::Off(OffReason::DevRun) => ("Updates are off in this run (development build).".into(), false),
        Phase::Off(OffReason::Portable) => ("This is a portable copy: it does not update itself. Download a newer one yourself.".into(), false),
        Phase::Off(OffReason::NoUpdater) => ("Updates are not available in this build.".into(), false),
        Phase::Idle => (format!("Not checked yet this session \u{b7} last checked {last}"), false),
        Phase::Checking => ("Checking for updates\u{2026}".into(), false),
        Phase::UpToDate => (format!("You\u{2019}re up to date \u{b7} last checked {last}"), true),
        Phase::Available(i) => (format!("Version {} is available", i.version), false),
        Phase::Downloading { .. } => ("Downloading the update\u{2026}".into(), false),
        Phase::Staged(v) => (format!("Version {v} is downloaded. It installs when you restart Tsubame."), true),
        Phase::ReadyToRestart(v) => (format!("Version {v} is installed. Restart to start it."), true),
        Phase::Unreachable => (format!("Can\u{2019}t reach the update server right now. Tsubame will try again later \u{b7} last checked {last}"), false),
        Phase::Failed(m) => (m.clone(), false),
    }
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("Updates", "Tsubame checks for a newer version and tells you. It never installs anything without you clicking, unless you turn automatic updates on.");
    let t = pg.t;
    let snap = updates::snapshot();
    let now = tsubame_update::state::now_unix();
    let last = updates::ago(snap.last_check_unix, now);

    // an update to act on
    match &snap.phase {
        Phase::Available(info) => {
            let r = Rect::new(pg.x, pg.y, pg.w, 56.0);
            pg.g.round(r, t.radius_lg, t.panel);
            pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
            pg.g.text("\u{E896}", Rect::new(r.x + 14.0, r.y, 20.0, r.h), f(16.0).icon().c(), t.accent);
            pg.g.text(&format!("Version {} is available", info.version), Rect::new(r.x + 44.0, r.y + 9.0, 400.0, 18.0), t.font_md().b(), t.text);
            let sub = if info.in_place_ok { format!("{} \u{b7} the signature is checked before it is installed", mb(info.size)) } else { "This copy cannot update itself. Download the installer from the release page.".to_string() };
            let shown = fit(pg.g, &sub, t.font_small(), pg.w - 360.0);
            pg.g.text(&shown, Rect::new(r.x + 44.0, r.y + 29.0, pg.w - 360.0, 16.0), t.font_small(), t.text_dim);
            let (go, _) = button(pg.ui, pg.g, ID_INSTALL, r.r() - 14.0, r.y + 14.0, "Download and restart", Look::Primary, info.in_place_ok, true);
            let nx = r.r() - 14.0 - pg.g.measure("Download and restart", t.font().b()) - 28.0 - 8.0;
            let (notes, _) = button(pg.ui, pg.g, ID_NOTES, nx, r.y + 14.0, "Release notes", Look::Normal, !info.notes_url.is_empty(), true);
            if go {
                updates::install();
            }
            if notes {
                cx.open.push(info.notes_url.clone());
            }
            pg.y += 68.0;
        }
        Phase::Downloading { done, total } => {
            let r = Rect::new(pg.x, pg.y, pg.w, 56.0);
            pg.g.round(r, t.radius_lg, t.panel);
            pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
            pg.g.text("Downloading the update\u{2026}", Rect::new(r.x + 16.0, r.y + 9.0, 300.0, 18.0), t.font_md().b(), t.text);
            let bar = Rect::new(r.x + 16.0, r.y + 36.0, pg.w - 220.0, 6.0);
            pg.g.round(bar, 3.0, t.scrollbar);
            let frac = if *total > 0 { (*done as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
            pg.g.round(Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 3.0, t.accent);
            pg.g.text(&format!("{} of {}", mb(*done), mb(*total)), Rect::new(bar.r() + 10.0, r.y + 28.0, 110.0, 20.0), t.font_small(), t.text_dim);
            if button(pg.ui, pg.g, ID_CANCEL, r.r() - 14.0, r.y + 14.0, "Cancel", Look::Normal, true, true).0 {
                updates::cancel();
            }
            pg.y += 68.0;
        }
        Phase::Staged(_) | Phase::ReadyToRestart(_) => {
            let r = Rect::new(pg.x, pg.y, pg.w, 48.0);
            pg.g.round(r, t.radius_lg, t.panel);
            pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
            let (line, _) = status_line(&snap.phase, &last);
            let shown = fit(pg.g, &line, t.font_md(), pg.w - 200.0);
            pg.g.text(&shown, Rect::new(r.x + 16.0, r.y, pg.w - 200.0, r.h), t.font_md(), t.text);
            if button(pg.ui, pg.g, ID_RESTART, r.r() - 14.0, r.y + 10.0, "Restart now", Look::Primary, true, true).0 {
                cx.restart = true;
            }
            pg.y += 60.0;
        }
        _ => {}
    }

    pg.section("This copy");
    let mut c = pg.card_h(84.0 + 52.0, 52.0);
    // version block
    {
        let r = Rect::new(c.r.x, c.r.y, c.r.w, 84.0);
        let logo = Rect::new(r.x + 16.0, r.y + 20.0, 44.0, 44.0);
        pg.g.round(logo, t.radius_lg, tsubame_ui::theme::BRAND_NAVY);
        pg.g.text("\u{2197}", logo, f(22.0).b().c(), tsubame_ui::theme::BRAND_CREAM);
        pg.g.text(&format!("Tsubame {}", snap.running), Rect::new(r.x + 74.0, r.y + 14.0, 400.0, 22.0), f(t.font_size_lg + 2.0).b(), t.text);
        let how = if cx.env.portable { "64-bit \u{b7} portable copy" } else { "64-bit \u{b7} installed for this user" };
        pg.g.text(how, Rect::new(r.x + 74.0, r.y + 37.0, 400.0, 16.0), t.font_small(), t.text_dim);
        let (line, ok) = status_line(&snap.phase, &last);
        let line = fit(pg.g, &line, t.font(), r.w - 74.0 - 170.0);
        if ok {
            pg.g.text("\u{E73E}", Rect::new(r.x + 74.0, r.y + 56.0, 16.0, 18.0), f(12.0).icon().c(), t.accent);
        }
        pg.g.text(&line, Rect::new(r.x + 74.0 + if ok { 20.0 } else { 0.0 }, r.y + 56.0, r.w - 250.0, 18.0), t.font().b(), t.text);
        let busy = matches!(snap.phase, Phase::Checking | Phase::Downloading { .. });
        let allowed = updates::manual_check_allowed(crate::updates::current_env()) && !busy;
        let primary = !matches!(snap.phase, Phase::Available(_));
        if button(pg.ui, pg.g, ID_CHECK, r.r() - 16.0, r.y + 28.0, "Check now", if primary { Look::Primary } else { Look::Normal }, allowed, true).0 {
            updates::check_now();
        }
    }
    c.restart_at(c.r.y + 84.0);
    let row = pg.next_row(&mut c);
    pg.row_text(row, "Update channel", "Stable is the only channel today: a release when it is ready, signed and checked.", 120.0, true);
    pg.g.text("Stable", Rect::new(row.r() - 16.0 - 100.0, row.y, 100.0, row.h), t.font_md().r(), t.text_dim);

    pg.section("How updates happen");
    let mut c = pg.card(3, 52.0);
    if let Some(v) = pg.toggle_row(&mut c, "Check for updates when Tsubame starts", "At most once a day. One small request to the release server, no personal data.", cx.prefs.updates.auto_check, true) {
        cx.prefs.updates.auto_check = v;
        cx.changed |= C_UPDATES;
    }
    if let Some(v) = pg.toggle_row(&mut c, "Install updates automatically", "Download in the background and install when you next restart. Off: you click \u{201c}Download and restart\u{201d}.", cx.prefs.updates.auto_install, true) {
        cx.prefs.updates.auto_install = v;
        cx.changed |= C_UPDATES;
    }
    let prev = snap.previous.clone();
    let title = match &prev {
        Some(v) => format!("Roll back to {v}"),
        None => "Roll back".to_string(),
    };
    let label = if st.confirm_roll { "Click again to roll back" } else { "Roll back\u{2026}" };
    let row = pg.next_row(&mut c);
    pg.row_text(row, &title, "The previous version is kept for a while after an update. If Tsubame crashes right after updating it offers this itself.", 190.0, prev.is_some());
    let bw = pg.g.measure("Click again to roll back", t.font().b()) + 28.0;
    let br = Rect::new(row.r() - 16.0 - bw, row.y + (row.h - t.height_button) / 2.0, bw, t.height_button);
    if button_in(pg.ui, pg.g, ID_ROLL, br, label, if st.confirm_roll { Look::DangerFill } else { Look::Normal }, prev.is_some()) {
        if st.confirm_roll {
            st.confirm_roll = false;
            cx.rollback = true;
        } else {
            st.confirm_roll = true;
        }
    }

    pg.section("Integrity");
    let mut c = pg.card(1, 84.0);
    let r = pg.next_row(&mut c);
    pg.g.text("\u{E83D}", Rect::new(r.x + 16.0, r.y + 10.0, 20.0, 18.0), f(14.0).icon().c(), t.accent);
    pg.g.text("Signed updates", Rect::new(r.x + 42.0, r.y + 10.0, 200.0, 18.0), t.font_md().b(), t.text);
    pg.g.text("Every update is signed with a key built into this app and checked before it is used.", Rect::new(r.x + 150.0, r.y + 11.0, r.w - 160.0, 18.0), t.font_small(), t.text_dim);
    pg.g.text("Updates come from", Rect::new(r.x + 42.0, r.y + 36.0, 140.0, 18.0), t.font_small(), t.text_dim);
    pg.g.text(&format!("github.com/{} (https only)", tsubame_update::DEFAULT_REPO), Rect::new(r.x + 190.0, r.y + 36.0, r.w - 200.0, 18.0), t.font_small(), t.text);
    pg.g.text("Network use", Rect::new(r.x + 42.0, r.y + 56.0, 140.0, 18.0), t.font_small(), t.text_dim);
    pg.g.text("Only when checking is on or you press Check now. Nothing is sent about you.", Rect::new(r.x + 190.0, r.y + 56.0, r.w - 200.0, 18.0), t.font_small(), t.text);
    if !matches!(snap.phase, Phase::Available(_)) {
        st.confirm_roll &= pg.ui.active().is_none() || pg.ui.is_active(ID_ROLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updates::Info;

    #[test]
    fn the_status_line_says_it_in_plain_words() {
        assert!(status_line(&Phase::Off(OffReason::DevRun), "x").0.contains("development"));
        assert!(status_line(&Phase::Off(OffReason::Portable), "x").0.contains("portable"));
        let (u, ok) = status_line(&Phase::Unreachable, "2 h ago");
        assert!(u.contains("Can\u{2019}t reach") && u.contains("2 h ago") && !ok);
        assert!(status_line(&Phase::UpToDate, "today").1);
        let a = Phase::Available(Info { version: "0.2.0".into(), notes_url: String::new(), in_place_ok: true, size: 1 });
        assert!(status_line(&a, "x").0.contains("0.2.0"));
        assert_eq!(mb(1_048_576), "1.0 MB");
    }
}
