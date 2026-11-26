//! Settings > Updates: this copy's version, the check, the download, roll back. The state is
//! the updater's (`updates::snapshot`); this page only draws it and asks for things.

use super::lay::{Look, Pg, button, button_in, fit, wrap};
use super::{C_UPDATES, Cx};
use crate::updates::{self, OffReason, Phase};
use tsubame_update::Channel;
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

const CHANNEL_DESC: &str = "Stable: tested releases. Nightly: test builds of the newest code.";
/// Shown under the channel row while Nightly is picked.
pub const NIGHTLY_WARNING: &str = "Nightly builds are test versions made automatically from the newest code. They can have bugs, lose settings or crash. They are signed and checked like every update, but not tested by hand. Back up anything important. To go back, pick Stable: Tsubame then waits for the next stable release that is newer than this build (it never installs an older version).";

fn available_title(info: &crate::updates::Info) -> String {
    match info.channel {
        Channel::Nightly => format!("Nightly {} is available", info.version),
        Channel::Stable => format!("Version {} is available", info.version),
    }
}

/// The small line under the version: how this copy is installed, its build channel and commit.
pub fn build_line(portable: bool, channel: &str, commit: Option<&str>) -> String {
    let how = if portable { "64-bit \u{b7} portable copy" } else { "64-bit \u{b7} installed for this user" };
    match commit {
        Some(c) => format!("{how} \u{b7} {channel} \u{b7} {c}"),
        None => format!("{how} \u{b7} {channel}"),
    }
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
        Phase::Available(i) => (format!("Version {} ({}) is available", i.version, i.channel), false),
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
    #[allow(unused_mut)]
    let mut snap = updates::snapshot();
    #[cfg(debug_assertions)]
    if let Some(p) = fake_phase() {
        snap.phase = p;
    }
    let now = tsubame_update::state::now_unix();
    let last = updates::ago(snap.last_check_unix, now);

    // an update to act on
    let mut tip: Option<(tsubame_ui::Pt, String)> = None;
    match &snap.phase {
        Phase::Available(info) => {
            let sub = if info.in_place_ok { format!("{} \u{b7} the signature is checked before it is installed", mb(info.size)) } else { "This copy cannot update itself. Download the installer from the release page.".to_string() };
            let title = available_title(info);
            let bn = Banner::new(pg.g, &t, pg.w, BANNER_ICON_X, &title, &sub, &["Release notes", "Download and restart"]);
            let r = Rect::new(pg.x, pg.y, pg.w, bn.h);
            pg.g.round(r, t.radius_lg, t.panel);
            pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
            pg.g.text("\u{E896}", Rect::new(r.x + 14.0, r.y + 9.0, 20.0, 18.0), f(16.0).icon().c(), t.accent);
            bn.paint_text(pg.g, &t, r);
            tip = bn.tooltip(pg.ui.mouse(), r, &title, &sub);
            let notes = button_in(pg.ui, pg.g, ID_NOTES, bn.btns[0].at(r), "Release notes", Look::Normal, !info.notes_url.is_empty());
            let go = button_in(pg.ui, pg.g, ID_INSTALL, bn.btns[1].at(r), "Download and restart", Look::Primary, info.in_place_ok);
            if go {
                updates::install();
            }
            if notes {
                cx.open.push(info.notes_url.clone());
            }
            pg.y += bn.h + 12.0;
        }
        Phase::Downloading { done, total } => {
            let r = Rect::new(pg.x, pg.y, pg.w, 56.0);
            pg.g.round(r, t.radius_lg, t.panel);
            pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
            let cw = pg.g.measure("Cancel", t.font().b()) + 28.0;
            let title_w = (pg.w - 16.0 - cw - 28.0).max(60.0);
            let title = fit(pg.g, "Downloading the update\u{2026}", t.font_md().b(), title_w);
            pg.g.text(&title, Rect::new(r.x + 16.0, r.y + 9.0, title_w, 18.0), t.font_md().b(), t.text);
            let label_w = 110.0;
            let bar = Rect::new(r.x + 16.0, r.y + 36.0, (pg.w - 16.0 - 10.0 - label_w - 8.0 - cw - 14.0).max(40.0), 6.0);
            pg.g.round(bar, 3.0, t.scrollbar);
            let frac = if *total > 0 { (*done as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
            pg.g.round(Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 3.0, t.accent);
            pg.g.text(&format!("{} of {}", mb(*done), mb(*total)), Rect::new(bar.r() + 10.0, r.y + 28.0, label_w, 20.0), t.font_small(), t.text_dim);
            if button(pg.ui, pg.g, ID_CANCEL, r.r() - 14.0, r.y + 14.0, "Cancel", Look::Normal, true, true).0 {
                updates::cancel();
            }
            pg.y += 68.0;
        }
        Phase::Staged(v) | Phase::ReadyToRestart(v) => {
            let (title, sub) = staged_texts(&snap.phase, v);
            let bn = Banner::new(pg.g, &t, pg.w, BANNER_TEXT_X, &title, sub, &["Restart now"]);
            let r = Rect::new(pg.x, pg.y, pg.w, bn.h);
            pg.g.round(r, t.radius_lg, t.panel);
            pg.g.round_stroke(r, t.radius_lg, t.accent, 1.0);
            bn.paint_text(pg.g, &t, r);
            tip = bn.tooltip(pg.ui.mouse(), r, &title, sub);
            if button_in(pg.ui, pg.g, ID_RESTART, bn.btns[0].at(r), "Restart now", Look::Primary, true) {
                cx.restart = true;
            }
            pg.y += bn.h + 12.0;
        }
        _ => {}
    }

    pg.section("This copy");
    // Version block. Wide: text left, "Check now" on the right. Narrow: the button
    // goes under the text, and the status line wraps (3 lines at most, then an
    // ellipsis with the full text in a tooltip).
    let (line, ok) = status_line(&snap.phase, &last);
    let lay = VersionLayout::new(pg.g, &t, pg.w, &line, ok);
    let mut c = pg.card_h(lay.h + 52.0, 52.0);
    {
        let r = Rect::new(c.r.x, c.r.y, c.r.w, lay.h);
        let logo = Rect::new(r.x + 16.0, r.y + 20.0, 44.0, 44.0);
        crate::appmark::draw(pg.g, logo);
        let title_font = f(t.font_size_lg + 2.0).b();
        let title = fit(pg.g, &format!("Tsubame {}", snap.running), title_font, lay.text_w);
        pg.g.text(&title, Rect::new(r.x + TEXT_X, r.y + 14.0, lay.text_w, 22.0), title_font, t.text);
        let how = build_line(cx.env.portable, crate::buildinfo::build_channel(), crate::buildinfo::short_commit());
        let how = fit(pg.g, &how, t.font_small(), lay.text_w);
        pg.g.text(&how, Rect::new(r.x + TEXT_X, r.y + 37.0, lay.text_w, 16.0), t.font_small(), t.text_dim);
        let indent = if ok { ICON_W } else { 0.0 };
        if ok {
            pg.g.text("\u{E73E}", Rect::new(r.x + TEXT_X, r.y + 56.0, 16.0, 18.0), f(12.0).icon().c(), t.accent);
        }
        for (i, l) in lay.lines.iter().enumerate() {
            pg.g.text(l, Rect::new(r.x + TEXT_X + indent, r.y + 56.0 + i as f32 * LINE_H, lay.text_w - indent, 18.0), t.font().b(), t.text);
        }
        if lay.cut {
            let area = Rect::new(r.x + TEXT_X, r.y + 56.0, lay.text_w, lay.lines.len() as f32 * LINE_H);
            let m = pg.ui.mouse();
            if area.contains_pt(m) {
                tip = Some((m, line.clone()));
            }
        }
        let busy = matches!(snap.phase, Phase::Checking | Phase::Downloading { .. });
        let allowed = updates::manual_check_allowed(crate::updates::current_env()) && !busy;
        let primary = !matches!(snap.phase, Phase::Available(_));
        let look = if primary { Look::Primary } else { Look::Normal };
        let clicked = if lay.wide {
            button(pg.ui, pg.g, ID_CHECK, r.r() - 16.0, r.y + 28.0, "Check now", look, allowed, true).0
        } else {
            button(pg.ui, pg.g, ID_CHECK, r.x + TEXT_X, r.y + 56.0 + lay.lines.len() as f32 * LINE_H + 6.0, "Check now", look, allowed, false).0
        };
        if clicked {
            updates::check_now();
        }
    }
    c.restart_at(c.r.y + lay.h);
    let nightly = Channel::from_config(&cx.prefs.updates.channel) == Channel::Nightly;
    let downloading = matches!(snap.phase, Phase::Downloading { .. });
    if downloading {
        // no switching under a running download: show the channel, read-only
        let row = pg.next_row(&mut c);
        pg.row_text(row, "Update channel", "Can\u{2019}t change the channel while an update downloads.", 160.0, false);
        let label = if nightly { "Nightly" } else { "Stable" };
        pg.g.text(label, Rect::new(row.r() - 16.0 - 100.0, row.y, 100.0, row.h), t.font_md().r(), t.text_dim);
    } else if let Some(i) = pg.seg_row(&mut c, "Update channel", CHANNEL_DESC, &["Stable", "Nightly"], nightly as usize) {
        let pick = if i == 1 { Channel::Nightly } else { Channel::Stable };
        if pick.as_str() != cx.prefs.updates.channel {
            cx.prefs.updates.channel = pick.as_str().to_string();
            cx.changed |= C_UPDATES;
        }
    }
    if nightly {
        pg.note("\u{E7BA}", NIGHTLY_WARNING, true);
    }

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
    // drawn last so nothing paints over it
    if let Some((at, text)) = tip {
        let lines = wrap(pg.g, &text, t.font_small(), 380.0);
        let rest: Vec<&str> = lines.iter().skip(1).map(String::as_str).collect();
        crate::overlay::tooltip(pg.g, &t, pg.ui.size(), at, lines.first().map_or("", String::as_str), &rest);
    }
}

/// Left edge of the text next to the logo, inside the card.
const TEXT_X: f32 = 74.0;
/// Width of the check mark in front of an "ok" status line.
const ICON_W: f32 = 20.0;
const LINE_H: f32 = 18.0;
/// Status lines shown before the rest is cut with an ellipsis.
const MAX_LINES: usize = 3;
/// Room the text needs beside the button before the button drops under it.
const MIN_TEXT_W: f32 = 320.0;

/// Where the version block puts its text and the "Check now" button for a card `w` wide.
struct VersionLayout {
    /// button on the right of the text (else under it)
    wide: bool,
    /// width of every text line
    text_w: f32,
    /// the status line, wrapped (the last one ends in an ellipsis when `cut`)
    lines: Vec<String>,
    cut: bool,
    /// height of the block (the card adds its rows below)
    h: f32,
}

impl VersionLayout {
    fn new(g: &mut dyn tsubame_ui::Canvas, t: &tsubame_ui::Theme, w: f32, status: &str, ok: bool) -> VersionLayout {
        let bw = g.measure("Check now", t.font().b()) + 28.0;
        let beside = w - TEXT_X - 16.0 - bw - 16.0;
        let wide = beside >= MIN_TEXT_W;
        let text_w = if wide { beside } else { (w - TEXT_X - 16.0).max(120.0) };
        let (lines, cut) = wrap_status(g, status, t.font().b(), text_w - if ok { ICON_W } else { 0.0 });
        let n = lines.len().max(1) as f32;
        let mut h = 56.0 + n * LINE_H + 10.0;
        if !wide {
            h += t.height_button + 6.0;
        }
        VersionLayout { wide, text_w, lines, cut, h }
    }
}

/// `s` wrapped to `w`, at most `MAX_LINES` lines; true when text was cut.
fn wrap_status(g: &mut dyn tsubame_ui::Canvas, s: &str, font: tsubame_ui::Font, w: f32) -> (Vec<String>, bool) {
    let mut lines = wrap(g, s, font, w);
    // one word wider than the line: fit() cuts it
    for l in lines.iter_mut() {
        if g.measure(l, font) > w {
            *l = fit(g, l, font, w);
        }
    }
    if lines.len() <= MAX_LINES {
        return (lines, false);
    }
    lines.truncate(MAX_LINES);
    let last = lines.pop().unwrap_or_default();
    lines.push(fit(g, &format!("{last}\u{2026}"), font, w));
    (lines, true)
}

/// Title and second line of the "downloaded / installed" banner.
fn staged_texts(phase: &Phase, v: &str) -> (String, &'static str) {
    match phase {
        Phase::ReadyToRestart(_) => (format!("Version {v} is installed"), "Restart to start it."),
        _ => (format!("Version {v} is downloaded"), "It installs when you restart Tsubame."),
    }
}

/// Debug builds only: `TSUBAME_FAKE_UPDATE=<kind>[:<channel>[:<version>]]` swaps the phase the page
/// draws (kind: available, downloading, staged, ready) so the banners can be looked at without a
/// real release. Draw only: nothing is checked, downloaded or installed.
#[cfg(debug_assertions)]
fn fake_phase() -> Option<Phase> {
    let v = std::env::var("TSUBAME_FAKE_UPDATE").ok()?;
    let mut it = v.splitn(3, ':');
    let kind = it.next()?;
    let channel = Channel::from_config(it.next().unwrap_or("stable"));
    let version = match it.next() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ if channel == Channel::Nightly => "0.1.1-nightly.20261125.123".to_string(),
        _ => "0.2.0".to_string(),
    };
    Some(match kind {
        "available" => Phase::Available(crate::updates::Info { version, channel, notes_url: "https://github.com/watdafawx/tsubame/releases".into(), in_place_ok: true, size: 14_680_064 }),
        "downloading" => Phase::Downloading { done: 6_291_456, total: 14_680_064 },
        "staged" => Phase::Staged(version),
        "ready" => Phase::ReadyToRestart(version),
        _ => return None,
    })
}

/// Left edge of the text in a banner that has an icon.
const BANNER_ICON_X: f32 = 44.0;
/// Left edge of the text in a banner without one.
const BANNER_TEXT_X: f32 = 16.0;
/// Second-line rows shown before the rest is cut.
const BANNER_SUB_LINES: usize = 3;
/// Room the text needs beside the buttons before they drop under it.
const BANNER_MIN_TEXT: f32 = 200.0;
const BANNER_SUB_H: f32 = 16.0;
const BANNER_GAP: f32 = 8.0;

/// A button's place in a banner, relative to the banner's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Place {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Place {
    fn at(&self, r: Rect) -> Rect {
        Rect::new(r.x + self.x, r.y + self.y, self.w, self.h)
    }
}

/// Layout of an update banner: a title, a second line and buttons. Wide: the buttons sit on the
/// right and the text gets all the room left of them. Narrow (or a long title): the buttons go
/// under the text, in a column when even one row does not fit. The text is cut with an ellipsis
/// only when it still does not fit, and then [`Banner::tooltip`] has the full text.
struct Banner {
    left: f32,
    /// buttons beside the text (read by the tests)
    #[cfg_attr(not(test), allow(dead_code))]
    wide: bool,
    text_w: f32,
    title: String,
    sub: Vec<String>,
    cut: bool,
    /// bottom of the text block
    text_b: f32,
    h: f32,
    btns: Vec<Place>,
}

impl Banner {
    fn new(g: &mut dyn tsubame_ui::Canvas, t: &tsubame_ui::Theme, w: f32, left: f32, title: &str, sub: &str, labels: &[&str]) -> Banner {
        let bh = t.height_button;
        let ws: Vec<f32> = labels.iter().map(|l| g.measure(l, t.font().b()) + 28.0).collect();
        let total = ws.iter().sum::<f32>() + BANNER_GAP * ws.len().saturating_sub(1) as f32;
        let beside = w - left - 14.0 - total - 16.0;
        let want = g.measure(title, t.font_md().b()).clamp(BANNER_MIN_TEXT, 380.0);
        let wide = beside >= want;
        let text_w = if wide { beside } else { (w - left - 14.0).max(80.0) };
        let shown = fit(g, title, t.font_md().b(), text_w);
        let mut cut = shown != title;
        let mut lines = wrap(g, sub, t.font_small(), text_w);
        for l in lines.iter_mut() {
            if g.measure(l, t.font_small()) > text_w {
                *l = fit(g, l, t.font_small(), text_w);
                cut = true;
            }
        }
        if lines.len() > BANNER_SUB_LINES {
            lines.truncate(BANNER_SUB_LINES);
            let last = lines.pop().unwrap_or_default();
            lines.push(fit(g, &format!("{last}\u{2026}"), t.font_small(), text_w));
            cut = true;
        }
        let text_b = 29.0 + lines.len().max(1) as f32 * BANNER_SUB_H;
        let mut btns = Vec::new();
        let h;
        if wide {
            h = (text_b + 11.0).max(56.0);
            let mut x = w - 14.0 - total;
            for bw in &ws {
                btns.push(Place { x, y: (h - bh) / 2.0, w: *bw, h: bh });
                x += bw + BANNER_GAP;
            }
        } else {
            let one_row_at = if total <= w - left - 14.0 { Some(left) } else if total <= w - 28.0 { Some(14.0) } else { None };
            let top = text_b + 8.0;
            let mut y = top;
            let mut x = one_row_at.unwrap_or(14.0);
            for bw in &ws {
                let bw = bw.min((w - 28.0).max(40.0));
                btns.push(Place { x, y, w: bw, h: bh });
                if one_row_at.is_some() {
                    x += bw + BANNER_GAP;
                } else {
                    y += bh + 6.0;
                }
            }
            let bottom = btns.iter().map(|p| p.y + p.h).fold(top, f32::max);
            h = bottom + 12.0;
        }
        Banner { left, wide, text_w, title: shown, sub: lines, cut, text_b, h, btns }
    }

    fn paint_text(&self, g: &mut dyn tsubame_ui::Canvas, t: &tsubame_ui::Theme, r: Rect) {
        g.text(&self.title, Rect::new(r.x + self.left, r.y + 9.0, self.text_w, 18.0), t.font_md().b(), t.text);
        for (i, l) in self.sub.iter().enumerate() {
            g.text(l, Rect::new(r.x + self.left, r.y + 29.0 + i as f32 * BANNER_SUB_H, self.text_w, BANNER_SUB_H), t.font_small(), t.text_dim);
        }
    }

    /// The full text under the mouse while some of it is cut.
    fn tooltip(&self, mouse: tsubame_ui::Pt, r: Rect, title: &str, sub: &str) -> Option<(tsubame_ui::Pt, String)> {
        let area = Rect::new(r.x + self.left, r.y + 9.0, self.text_w, self.text_b - 9.0);
        (self.cut && area.contains_pt(mouse)).then(|| (mouse, format!("{title}. {sub}")))
    }

    /// Every box the banner draws, relative to its corner: icon slot (when `left` leaves room for
    /// one), title, second line, buttons. For the no-overlap test.
    #[cfg(test)]
    fn boxes(&self) -> Vec<(&'static str, Rect)> {
        let mut v = vec![("title", Rect::new(self.left, 9.0, self.text_w, 18.0)), ("sub", Rect::new(self.left, 29.0, self.text_w, self.sub.len().max(1) as f32 * BANNER_SUB_H))];
        if self.left > BANNER_TEXT_X {
            v.push(("icon", Rect::new(14.0, 9.0, 20.0, 18.0)));
        }
        for p in &self.btns {
            v.push(("button", Rect::new(p.x, p.y, p.w, p.h)));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updates::Info;
    use tsubame_ui::Canvas;

    #[test]
    fn the_status_line_says_it_in_plain_words() {
        assert!(status_line(&Phase::Off(OffReason::DevRun), "x").0.contains("development"));
        assert!(status_line(&Phase::Off(OffReason::Portable), "x").0.contains("portable"));
        let (u, ok) = status_line(&Phase::Unreachable, "2 h ago");
        assert!(u.contains("Can\u{2019}t reach") && u.contains("2 h ago") && !ok);
        assert!(status_line(&Phase::UpToDate, "today").1);
        let a = Phase::Available(Info { version: "0.2.0".into(), channel: Channel::Stable, notes_url: String::new(), in_place_ok: true, size: 1 });
        assert!(status_line(&a, "x").0.contains("0.2.0"));
        assert_eq!(mb(1_048_576), "1.0 MB");
    }

    #[test]
    fn the_version_block_wraps_under_a_width_and_keeps_the_button_off_the_text() {
        let long = "Can\u{2019}t reach the update server right now. Tsubame will try again later \u{b7} last checked 3 days ago";
        for t in [tsubame_ui::Theme::dark(), tsubame_ui::Theme::light()] {
            let mut g = tsubame_ui::TestCanvas::new(1000.0, 800.0);
            // wide: the button sits right of the text, a short line stays on one line
            let wide = VersionLayout::new(&mut g, &t, 860.0, "You\u{2019}re up to date \u{b7} last checked today", true);
            assert!(wide.wide && wide.lines.len() == 1 && !wide.cut);
            // narrow: the button drops under the text and the block grows by its height
            let narrow = VersionLayout::new(&mut g, &t, 480.0, long, false);
            assert!(!narrow.wide);
            assert!(narrow.h >= 56.0 + narrow.lines.len() as f32 * LINE_H + t.height_button);
            // nothing is wider than its box, at any width
            for w in [300.0, 420.0, 480.0, 520.0, 700.0, 860.0] {
                let lay = VersionLayout::new(&mut g, &t, w, long, false);
                assert!(lay.lines.len() <= MAX_LINES && !lay.lines.is_empty());
                for l in &lay.lines {
                    assert!(g.measure(l, t.font().b()) <= lay.text_w + 0.5, "{w}: {l:?}");
                }
                if lay.wide {
                    // button left edge stays right of the text
                    let bw = g.measure("Check now", t.font().b()) + 28.0;
                    assert!(w - 16.0 - bw >= TEXT_X + lay.text_w, "{w}");
                }
            }
            // a very narrow card cuts and says so
            let tiny = VersionLayout::new(&mut g, &t, 200.0, long, false);
            assert!(tiny.cut && tiny.lines.last().unwrap().ends_with('\u{2026}'));
        }
    }

    const WIDTHS: [f32; 11] = [260.0, 320.0, 400.0, 480.0, 560.0, 640.0, 760.0, 860.0, 1000.0, 1200.0, 1600.0];

    fn assert_banner_sound(g: &mut tsubame_ui::TestCanvas, t: &tsubame_ui::Theme, b: &Banner, w: f32, what: &str) {
        let boxes = b.boxes();
        let all = Rect::new(0.0, 0.0, w, b.h);
        for (i, (na, ra)) in boxes.iter().enumerate() {
            assert!(ra.x >= -0.01 && ra.y >= -0.01 && ra.r() <= all.r() + 0.01 && ra.b() <= all.b() + 0.01, "{what} @{w}: {na} {ra:?} leaves the banner {all:?}");
            for (nb, rb) in &boxes[i + 1..] {
                assert!(!ra.intersects(rb), "{what} @{w}: {na} {ra:?} overlaps {nb} {rb:?}");
            }
        }
        // no drawn text is wider than the box it is drawn in
        assert!(g.measure(&b.title, t.font_md().b()) <= b.text_w + 0.5, "{what} @{w}: title {:?}", b.title);
        for l in &b.sub {
            assert!(g.measure(l, t.font_small()) <= b.text_w + 0.5, "{what} @{w}: {l:?}");
        }
        assert!(b.sub.len() <= BANNER_SUB_LINES);
        // wide: the text ends before the first button
        if b.wide {
            assert!(b.btns.iter().all(|p| p.x >= b.left + b.text_w), "{what} @{w}");
        }
    }

    #[test]
    fn banners_never_overlap_and_give_the_title_the_room_it_needs() {
        let sub = "14.0 MB \u{b7} the signature is checked before it is installed";
        let sub_long = "This copy cannot update itself. Download the installer from the release page. It was installed for all users, so the installer has to run again.";
        let titles = [
            "Version 0.2.0 is available".to_string(),
            "Nightly 0.1.1-nightly.20261125.123 is available".to_string(),
            "Nightly 0.1.1-nightly.20261125.123456789-a-very-long-channel-name-for-a-test is available".to_string(),
        ];
        for t in [tsubame_ui::Theme::dark(), tsubame_ui::Theme::light()] {
            let mut g = tsubame_ui::TestCanvas::new(1700.0, 800.0);
            for title in &titles {
                for s in [sub, sub_long] {
                    for w in WIDTHS {
                        let b = Banner::new(&mut g, &t, w, BANNER_ICON_X, title, s, &["Release notes", "Download and restart"]);
                        assert_banner_sound(&mut g, &t, &b, w, title);
                        assert_eq!(b.btns.len(), 2);
                        // title is whole wherever the page has room for it on one line
                        if w >= 760.0 && title.len() < 60 {
                            assert!(!b.cut || s == sub_long, "{title} @{w} cut for no reason");
                            assert_eq!(&b.title, title, "{title} @{w}");
                        }
                    }
                }
            }
            // the case from the rig: ~800 DIP of page, nightly version, both buttons
            let b = Banner::new(&mut g, &t, 800.0, BANNER_ICON_X, &titles[1], sub, &["Release notes", "Download and restart"]);
            assert!(b.wide && !b.cut && b.title == titles[1]);
            // below the threshold the buttons drop under the text, left to right, same row
            let b = Banner::new(&mut g, &t, 480.0, BANNER_ICON_X, &titles[1], sub, &["Release notes", "Download and restart"]);
            assert!(!b.wide && b.btns[0].y >= b.text_b && b.btns[1].y == b.btns[0].y && b.btns[1].x > b.btns[0].x + b.btns[0].w);
            // a very narrow page stacks them
            let b = Banner::new(&mut g, &t, 260.0, BANNER_ICON_X, &titles[1], sub, &["Release notes", "Download and restart"]);
            assert!(!b.wide && b.btns[1].y > b.btns[0].y + b.btns[0].h);
            // a title that cannot fit even full width is cut and the tooltip has the whole of it
            let b = Banner::new(&mut g, &t, 400.0, BANNER_ICON_X, &titles[2], sub, &["Release notes", "Download and restart"]);
            assert!(b.cut && b.title.ends_with('\u{2026}'));
            let r = Rect::new(10.0, 10.0, 400.0, b.h);
            let tip = b.tooltip(tsubame_ui::Pt::new(60.0, 22.0), r, &titles[2], sub).expect("cut text has a tooltip");
            assert!(tip.1.contains("a-very-long-channel-name"));
            assert!(b.tooltip(tsubame_ui::Pt::new(5.0, 5.0), r, &titles[2], sub).is_none(), "outside the text: no tooltip");
        }
    }

    #[test]
    fn the_downloaded_banner_shows_the_whole_version() {
        let (title, sub) = staged_texts(&Phase::Staged("0.1.1-nightly.20261125.123".into()), "0.1.1-nightly.20261125.123");
        assert_eq!(title, "Version 0.1.1-nightly.20261125.123 is downloaded");
        assert_eq!(sub, "It installs when you restart Tsubame.");
        let (title2, _) = staged_texts(&Phase::ReadyToRestart("0.2.0".into()), "0.2.0");
        assert_eq!(title2, "Version 0.2.0 is installed");
        for t in [tsubame_ui::Theme::dark(), tsubame_ui::Theme::light()] {
            let mut g = tsubame_ui::TestCanvas::new(1700.0, 800.0);
            for w in WIDTHS {
                let b = Banner::new(&mut g, &t, w, BANNER_TEXT_X, &title, sub, &["Restart now"]);
                assert_banner_sound(&mut g, &t, &b, w, "staged");
                if w >= 480.0 {
                    assert_eq!(b.title, title, "@{w}: the version is not cut");
                    assert!(!b.cut);
                }
            }
        }
    }

    #[test]
    fn every_restart_label_leads_to_a_restart() {
        // "Download and restart" (Available) must restart after apply; "Restart now"
        // (Staged / ReadyToRestart) sets cx.restart. A label with "restart" in it is a promise.
        assert!(crate::updates::restarts_after(true));
        let src = include_str!("updates_page.rs");
        assert!(src.contains("updates::install();"));
        assert!(src.contains("cx.restart = true;"));
        assert!(!src.contains("\"Restart to apply\""));
    }

    #[test]
    fn the_offer_names_its_channel_and_the_copy_names_its_build() {
        let n = Info { version: "0.1.1-nightly.42".into(), channel: Channel::Nightly, notes_url: String::new(), in_place_ok: true, size: 1 };
        assert_eq!(available_title(&n), "Nightly 0.1.1-nightly.42 is available");
        assert!(status_line(&Phase::Available(n), "x").0.contains("(nightly)"));
        let s = Info { version: "0.2.0".into(), channel: Channel::Stable, notes_url: String::new(), in_place_ok: true, size: 1 };
        assert_eq!(available_title(&s), "Version 0.2.0 is available");
        assert_eq!(build_line(false, "nightly", Some("abc1234")), "64-bit \u{b7} installed for this user \u{b7} nightly \u{b7} abc1234");
        assert_eq!(build_line(true, "local build", None), "64-bit \u{b7} portable copy \u{b7} local build");
        assert!(NIGHTLY_WARNING.contains("older version"), "the warning says switching back never downgrades");
    }
}
