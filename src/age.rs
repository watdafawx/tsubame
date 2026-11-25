//! File-age colouring (P4.F6, built-in extension `tsubame.ext.file_age`,
//! `docs/f6-mock.md`, `mocks/mock-f6-age.html`).
//!
//! Tints the Modified date (text, chip or a row accent bar) by how long ago the
//! file changed: today, week, month, year, older. The date is the entry
//! record's own modified time, so there is no extra read, no cache and no
//! thread; no timer either (a window left idle across midnight keeps its tints
//! until the next repaint). The clock is read once per frame; each visible row
//! costs one subtract and up to four compares. Nothing here runs when the
//! feature is off: `frame` returns `None` and the row draws as before.
//!
//! The legend counts every row of the folder, but only while the legend is open
//! (capped at 1,000,000 rows).

use crate::view::View;
use tsubame_config::{AgePalette, AgeStyle, AgeToday, FileAge};
use tsubame_core::{EntryStore, FOLDER};
use tsubame_ext_api::{Contributions, Extension, ExtensionInfo};
use tsubame_ui::{Button, Canvas, Cursor, Id, Rect, Sense, Theme, Ui, f};

pub const EXT_FILE_AGE: &str = "tsubame.ext.file_age";

const DAY: u32 = 86_400;
/// Rows the legend counts at most.
pub const COUNT_CAP: usize = 1_000_000;
/// Width before the date when pips are shown (four bars and a gap).
const PIP_PAD: f32 = 17.0;
/// Width reserved at the right of the status bar for the key.
pub const KEY_W: f32 = 150.0;
const LEGEND_W: f32 = 316.0;
const ID_KEY: Id = Id::new("age-key");
const ID_LEGEND: Id = Id::new("age-legend");

pub struct FileAgeExtension;

impl Extension<crate::ext::App> for FileAgeExtension {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(EXT_FILE_AGE, "File age colours", false)
    }
    fn contribute(&self, c: &mut Contributions<crate::ext::App>) {
        for spec in crate::commands::AGE_COMMANDS {
            c.command(spec.clone(), View::run_age);
        }
    }
}

/// The extension is on (its status key and command exist). The colours also
/// need `[file_age] enabled`.
pub fn is_enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_FILE_AGE).unwrap_or(false))
}

/// Text colours per palette and theme (dark, light), bucket 0 = today ... 4 = older.
/// Each is at least 4.5:1 on the row, zebra, hover, selected and inactive-selected
/// surfaces (computed in the mock).
const PAL: [[[u32; 5]; 2]; 3] = [
    // heat
    [[0xff8a5c, 0xe6c84a, 0x5ccf7a, 0x7fb0ff, 0xa3adbf], [0xa8380c, 0x6f5400, 0x17692f, 0x1b52bf, 0x525d72]],
    // colour-blind safe
    [[0xffe066, 0xd6ecff, 0x7fb0ff, 0xc9a56a, 0xa3adbf], [0x6b5000, 0x0b5a8a, 0x1b52bf, 0x8c4a1a, 0x525d72]],
    // monochrome
    [[0xf5f2eb, 0xdde1e8, 0xc7cedb, 0xb3bccb, 0xa3adbf], [0x0b0f18, 0x2a3243, 0x3d475b, 0x4b556b, 0x525d72]],
];

pub fn palette_colours(p: AgePalette, dark: bool) -> [u32; 5] {
    let pi = match p {
        AgePalette::Heat => 0,
        AgePalette::ColourBlind => 1,
        AgePalette::Mono => 2,
    };
    PAL[pi][usize::from(!dark)]
}

/// Seconds since the local midnight before `now` (unix, UTC), measured in real
/// elapsed time, so a day with a DST change is 23 or 25 hours long and still
/// right. `local` maps a unix time to local wall-clock seconds.
pub fn secs_since_local_midnight(now: u32, local: impl Fn(u32) -> u32) -> u32 {
    let wall = local(now);
    let mid_wall = wall - wall % DAY;
    // the UTC instant of that wall-clock midnight: the offset at the guess decides
    let guess = (i64::from(mid_wall) - (i64::from(wall) - i64::from(now))).max(0) as u32;
    let off = i64::from(local(guess)) - i64::from(guess);
    let mid = (i64::from(mid_wall) - off).clamp(0, i64::from(now));
    // never longer than a day and an hour (a DST gap at midnight)
    (i64::from(now) - mid).clamp(0, i64::from(DAY + 3600)) as u32
}

/// What a row's date gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    /// not tinted (a folder while folders are off)
    None,
    /// no timestamp: an em dash, no tint
    Unknown,
    /// bucket 0..=4, and whether the date is in the future (shown as today)
    Bucket(u8, bool),
}

/// The settings resolved for one frame: the thresholds in seconds, the colours,
/// the clock. Copy, no allocation.
#[derive(Clone, Copy, Debug)]
pub struct AgeFrame {
    pub style: AgeStyle,
    pub palette: AgePalette,
    pub pips: bool,
    pub folders: bool,
    now: u32,
    /// ages (seconds) up to which a row is today / this week / this month / this year
    cut: [u32; 4],
    pub colours: [u32; 5],
    chip_w: f32,
}

impl AgeFrame {
    pub fn new(cfg: &FileAge, dark: bool, now: u32, local: impl Fn(u32) -> u32, chip_w: f32) -> AgeFrame {
        let (w, m, y) = cfg.thresholds();
        let today = match cfg.today {
            AgeToday::Midnight => secs_since_local_midnight(now, local),
            AgeToday::Last24h => DAY,
        };
        AgeFrame {
            style: cfg.style,
            palette: cfg.palette,
            pips: cfg.shows_pips(),
            folders: cfg.folders,
            now,
            cut: [today, w.saturating_mul(DAY), m.saturating_mul(DAY), y.saturating_mul(DAY)],
            colours: palette_colours(cfg.palette, dark),
            chip_w,
        }
    }

    /// The bucket of an age in seconds: today, week, month, year, older. A
    /// threshold is inclusive (exactly 7 days is still this week).
    #[inline]
    pub fn bucket_of_age(&self, age: u32) -> u8 {
        let c = &self.cut;
        if age <= c[0] {
            0
        } else if age <= c[1] {
            1
        } else if age <= c[2] {
            2
        } else if age <= c[3] {
            3
        } else {
            4
        }
    }

    /// One subtract and the compares. A date in the future is today.
    #[inline]
    pub fn classify(&self, mtime: u32, dir: bool) -> Tint {
        if dir && !self.folders {
            return Tint::None;
        }
        if mtime == 0 {
            return Tint::Unknown;
        }
        Tint::Bucket(self.bucket_of_age(self.now.saturating_sub(mtime)), mtime > self.now)
    }
}

/// The frame's settings, or `None` when the colours are off (the extension off,
/// or `[file_age] enabled = false`): then nothing is read, measured or drawn.
pub fn frame(cfg: &FileAge, dark: bool, g: &mut dyn Canvas, t: &Theme) -> Option<AgeFrame> {
    if !cfg.enabled || !is_enabled() {
        return None;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs().min(u64::from(u32::MAX)) as u32);
    // digits are tabular, so one measure of a date serves every chip
    let chip_w = if cfg.style == AgeStyle::Chip { g.measure("0000-00-00 00:00", t.font()) } else { 0.0 };
    Some(AgeFrame::new(cfg, dark, now, tsubame_win::tz::local_secs, chip_w))
}

/// Draws the Modified cell. `af == None` is exactly the plain cell. `cell` is the
/// text box, `row` the whole row (for the accent bar).
#[allow(clippy::too_many_arguments)]
pub fn draw_date(af: Option<&AgeFrame>, g: &mut dyn Canvas, t: &Theme, mtime: u32, dir: bool, text: &str, mut cell: Rect, row: Rect) {
    let Some(af) = af else {
        g.text(text, cell, t.font(), t.text_dim);
        return;
    };
    let tint = af.classify(mtime, dir);
    let origin = cell.x;
    if af.pips {
        cell.x += PIP_PAD;
        cell.w = (cell.w - PIP_PAD).max(0.0);
    }
    let Tint::Bucket(b, future) = tint else {
        g.text(if tint == Tint::Unknown { "\u{2014}" } else { text }, cell, t.font(), t.text_dim);
        return;
    };
    let col = af.colours[usize::from(b)];
    let cy = cell.y + cell.h / 2.0;
    if af.pips {
        let n = 4 - usize::from(b);
        for i in 0..4usize {
            let h = [4.0, 6.0, 8.0, 10.0][i];
            g.fill_a(Rect::new(origin + i as f32 * 3.0, cy + 5.0 - h, 2.0, h), col, if i < n { 1.0 } else { 0.28 });
        }
    }
    match af.style {
        AgeStyle::Text => {
            let font = if af.palette == AgePalette::Mono && b == 0 { t.font().b() } else { t.font() };
            g.text(text, cell, font, col);
        }
        AgeStyle::Chip => {
            g.round_a(Rect::new(cell.x - 6.0, cy - 9.0, af.chip_w + 12.0, 18.0), t.radius_sm, col, 0.18);
            g.text(text, cell, t.font(), t.text);
        }
        AgeStyle::Accent => {
            g.round(Rect::new(row.x, row.y + 2.0, 3.0, (row.h - 4.0).max(0.0)), 1.0, col);
            g.text(text, cell, t.font(), t.text_dim);
        }
    }
    if future {
        let w = g.measure(text, t.font());
        g.text("\u{E7BA}", Rect::new(cell.x + w + 4.0, cell.y, 14.0, cell.h), f(11.0).icon().c(), t.danger);
    }
}

/// Counts the rows of a folder per bucket (rows without a timestamp and folders
/// that are not tinted are left out). Stops at [`COUNT_CAP`] rows.
pub fn count(store: &EntryStore, af: &AgeFrame) -> ([u32; 5], bool) {
    let mut c = [0u32; 5];
    let view = store.view();
    let capped = view.len() > COUNT_CAP;
    for &id in &view[..view.len().min(COUNT_CAP)] {
        let Some(rec) = store.record(id) else { continue };
        let dir = rec.flags & FOLDER != 0;
        if let Tint::Bucket(b, _) = af.classify(crate::data::filetime_to_unix(rec.modified), dir) {
            c[usize::from(b)] += 1;
        }
    }
    (c, capped)
}

/// What the legend asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    /// open Settings > Extensions
    Settings,
    /// switch the colours on or off
    Toggle,
}

/// The status-bar key and the legend popup. Per view; nothing is stored per file.
#[derive(Default)]
pub struct AgeUi {
    /// counts shown in the open legend
    counts: [u32; 5],
    capped: bool,
}

const BUCKET_NAMES: [&str; 5] = ["Today", "This week", "This month", "This year", "Older"];

/// The name of bucket `b` (0 today .. 4 older), for the legend and screen readers.
pub fn bucket_name(b: usize) -> &'static str {
    BUCKET_NAMES[b.min(4)]
}

fn threshold_text(cfg: &FileAge, b: usize, out: &mut String) {
    use std::fmt::Write;
    out.clear();
    let (w, m, y) = cfg.thresholds();
    let _ = match b {
        0 if cfg.today == AgeToday::Last24h => write!(out, "last 24 h"),
        0 => write!(out, "since midnight"),
        1 => write!(out, "\u{2264} {w} days"),
        2 => write!(out, "\u{2264} {m} days"),
        3 if y == 365 => write!(out, "\u{2264} 1 year"),
        3 => write!(out, "\u{2264} {y} days"),
        _ => write!(out, "older"),
    };
}

impl AgeUi {
    /// The key at the right of the status bar `r` of pane `slot`, and its legend.
    /// Click: switches the colours on if they are off, and opens or closes the legend.
    #[allow(clippy::too_many_arguments)]
    pub fn status_key(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, r: Rect, slot: u64, cfg: &FileAge, dark: bool, store: &EntryStore) -> Act {
        let kr = Rect::new(r.r() - 12.0 - KEY_W, r.y + 3.0, KEY_W, (r.h - 5.0).max(0.0));
        let id = ID_KEY.with(slot);
        let lid = ID_LEGEND.with(slot);
        let it = ui.interact(id, kr, Sense::click().cursor(Cursor::Hand));
        let open = ui.is_popup_open(lid);
        if it.hovered || open {
            g.round(kr, t.radius_sm, t.hover);
        }
        let on = cfg.enabled;
        let cols = palette_colours(cfg.palette, dark);
        for (i, c) in cols.iter().enumerate() {
            let sq = Rect::new(kr.x + 6.0 + i as f32 * 10.0, kr.y + (kr.h - 8.0) / 2.0, 8.0, 8.0);
            if on {
                g.round(sq, 2.0, *c);
            } else {
                g.round_a(sq, 2.0, t.text_disabled, 0.5);
            }
        }
        let label = if on { "Age colours: on" } else { "Age colours: off" };
        g.text(label, Rect::new(kr.x + 6.0 + 50.0 + 4.0, kr.y, KEY_W - 62.0, kr.h), t.font_small(), if on { t.text_dim } else { t.text_disabled });
        let mut act = Act::None;
        if it.clicked == Some(Button::Left) {
            if open {
                ui.close_popup(lid);
            } else {
                ui.open_popup(lid);
                if !on {
                    act = Act::Toggle; // a click on the off key turns the colours on and shows the legend
                }
            }
        }
        if ui.is_popup_open(lid) {
            let a2 = self.legend(ui, g, t, kr, lid, cfg, dark, store);
            if a2 != Act::None {
                act = a2;
            }
        }
        act
    }

    #[allow(clippy::too_many_arguments)]
    fn legend(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, key: Rect, id: Id, cfg: &FileAge, dark: bool, store: &EntryStore) -> Act {
        // counts only while the legend is open
        let probe = FileAge { enabled: true, ..cfg.clone() };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs().min(u64::from(u32::MAX)) as u32);
        let af = AgeFrame::new(&probe, dark, now, tsubame_win::tz::local_secs, 0.0);
        (self.counts, self.capped) = count(store, &af);
        let h = 4.0 + 20.0 + 5.0 * 26.0 + 9.0 + 2.0 * 24.0 + 4.0;
        let (win_w, _) = ui.size();
        let x = (key.r() - LEGEND_W).clamp(4.0, (win_w - LEGEND_W - 4.0).max(4.0));
        let rect = Rect::new(x, (key.y - h - 4.0).max(2.0), LEGEND_W, h);
        let mut act = Act::None;
        if !ui.popup_begin(g, id, rect) {
            return act;
        }
        let cols = palette_colours(cfg.palette, dark);
        let mut y = rect.y + 4.0;
        g.text("File age: Modified date", Rect::new(rect.x + 10.0, y, rect.w - 20.0, 20.0), t.font_small().b(), t.text_dim);
        y += 20.0;
        let mut s = String::new();
        for b in 0..5usize {
            let row = Rect::new(rect.x + 4.0, y, rect.w - 8.0, 26.0);
            let sw = Rect::new(row.x + 6.0, y + 5.0, 38.0, 16.0);
            g.round_a(sw, 3.0, cols[b], 0.18);
            if cfg.shows_pips() {
                let n = 4 - b;
                for i in 0..4usize {
                    let ph = [4.0, 6.0, 8.0, 10.0][i];
                    g.fill_a(Rect::new(sw.x + 13.0 + i as f32 * 3.0, sw.y + 13.0 - ph, 2.0, ph), cols[b], if i < n { 1.0 } else { 0.28 });
                }
            } else {
                g.text("Aa", sw, t.font_small().b().c(), cols[b]);
            }
            g.text(BUCKET_NAMES[b], Rect::new(sw.r() + 8.0, y, 90.0, 26.0), t.font(), t.text);
            threshold_text(cfg, b, &mut s);
            g.text(&s, Rect::new(sw.r() + 100.0, y, 100.0, 26.0), t.font_small(), t.text_dim);
            let n = self.counts[b];
            let ct = if self.capped && n as usize >= COUNT_CAP { "1M+".to_string() } else { n.to_string() };
            g.text(&ct, Rect::new(row.r() - 52.0, y, 46.0, 26.0), t.font_small().r(), t.text_dim);
            y += 26.0;
        }
        g.fill(Rect::new(rect.x + 8.0, y + 4.0, rect.w - 16.0, 1.0), t.border);
        y += 9.0;
        for (i, (label, a)) in [("Thresholds and palette\u{2026}", Act::Settings), (if cfg.enabled { "Turn off age colours" } else { "Turn on age colours" }, Act::Toggle)].into_iter().enumerate() {
            let r = Rect::new(rect.x + 4.0, y, rect.w - 8.0, 24.0);
            let it = ui.interact(id.child("item").with(i as u64), r, Sense::click().cursor(Cursor::Hand));
            if it.hovered {
                g.round(r, t.radius_sm, t.hover);
            }
            g.text(label, Rect::new(r.x + 10.0, r.y, r.w - 20.0, r.h), t.font(), t.text);
            if it.clicked == Some(Button::Left) {
                act = a;
            }
            y += 24.0;
        }
        ui.popup_end(g);
        if act != Act::None {
            ui.close_popup(id);
        }
        act
    }
}

impl View {
    /// `view.age_colours`: switch the colours on or off.
    pub(crate) fn run_age(&mut self, ui: &mut Ui, _cmd: u32) {
        self.toggle_age(ui);
    }

    pub(crate) fn toggle_age(&mut self, ui: &mut Ui) {
        self.prefs.file_age.enabled = !self.prefs.file_age.enabled;
        self.ops.message = Some(format!("Age colours {}", if self.prefs.file_age.enabled { "on" } else { "off" }));
        self.apply_prefs(ui, crate::settings::C_OTHER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOON: u32 = 1_764_072_600; // 2025-11-25 12:50 UTC (any instant is fine)

    fn cfg() -> FileAge {
        FileAge { enabled: true, ..FileAge::default() }
    }

    fn frame_at(c: &FileAge, now: u32, off: i64) -> AgeFrame {
        AgeFrame::new(c, true, now, move |u| (i64::from(u) + off) as u32, 0.0)
    }

    fn b(af: &AgeFrame, now: u32, ago: u32) -> Tint {
        af.classify(now - ago, false)
    }

    #[test]
    fn today_is_since_local_midnight() {
        // UTC+2, 2026-11-25 14:30 UTC = 16:30 local: midnight was 16 h 30 min ago
        let now = 1_795_617_000 - 1_795_617_000 % DAY + 14 * 3600 + 30 * 60;
        let af = frame_at(&cfg(), now, 7200);
        assert_eq!(b(&af, now, 16 * 3600 + 30 * 60), Tint::Bucket(0, false), "exactly at midnight is today");
        assert_eq!(b(&af, now, 16 * 3600 + 30 * 60 + 1), Tint::Bucket(1, false), "one second before midnight is this week");
        assert_eq!(b(&af, now, 0), Tint::Bucket(0, false));
    }

    #[test]
    fn midnight_rollover_resets_today() {
        let day = 20_000 * DAY; // some UTC midnight
        let just_before = frame_at(&cfg(), day - 1, 0);
        assert_eq!(just_before.classify(day - 100, false), Tint::Bucket(0, false), "23:58 is today at 23:59:59");
        let just_after = frame_at(&cfg(), day + 30, 0);
        assert_eq!(just_after.classify(day - 100, false), Tint::Bucket(1, false), "the same file is this week 30 s after midnight");
        assert_eq!(just_after.classify(day + 10, false), Tint::Bucket(0, false));
    }

    #[test]
    fn last_24h_option() {
        let c = FileAge { today: AgeToday::Last24h, ..cfg() };
        let now = 20_000 * DAY + 3600; // 01:00, a minute after midnight does not matter
        let af = frame_at(&c, now, 0);
        assert_eq!(b(&af, now, DAY), Tint::Bucket(0, false), "exactly 24 h is still today");
        assert_eq!(b(&af, now, DAY + 1), Tint::Bucket(1, false));
        assert_eq!(b(&af, now, 3 * 3600), Tint::Bucket(0, false), "yesterday evening is within 24 h");
    }

    #[test]
    fn dst_days_are_measured_in_real_time() {
        // local = UTC until 02:00 UTC of the day D, then +1 h (spring forward)
        let d = 20_000 * DAY;
        let local = |u: u32| if u < d + 2 * 3600 { u } else { u + 3600 };
        // 05:00 UTC = 06:00 local; local midnight was at 00:00 UTC, 5 h of real time ago
        assert_eq!(secs_since_local_midnight(d + 5 * 3600, local), 5 * 3600);
        // local = UTC+1 until 01:00 UTC on day D (fall back to UTC+0): 05:00 UTC = 05:00 local,
        // midnight local (UTC+1) was 23:00 UTC the day before: 6 h ago
        let local2 = |u: u32| if u < d + 3600 { u + 3600 } else { u };
        assert_eq!(secs_since_local_midnight(d + 5 * 3600, local2), 6 * 3600);
        // a plain day
        assert_eq!(secs_since_local_midnight(d + 5 * 3600, |u| u), 5 * 3600);
        // an offset that puts local midnight on the previous UTC day
        assert_eq!(secs_since_local_midnight(d + 3600, |u| u - 5 * 3600), 20 * 3600, "UTC-5: local midnight was 05:00 UTC the day before");
        // never panics or exceeds a day and an hour
        assert!(secs_since_local_midnight(0, |u| u) <= DAY + 3600);
        assert!(secs_since_local_midnight(10, |u| u.saturating_sub(7 * 3600)) <= DAY + 3600);
    }

    #[test]
    fn thresholds_are_inclusive() {
        let now = 30_000 * DAY + 12 * 3600;
        let af = frame_at(&cfg(), now, 0);
        for (ago, want) in [(7 * DAY, 1), (7 * DAY + 1, 2), (30 * DAY, 2), (30 * DAY + 1, 3), (365 * DAY, 3), (365 * DAY + 1, 4)] {
            assert_eq!(b(&af, now, ago), Tint::Bucket(want, false), "{ago} s ago");
        }
    }

    #[test]
    fn custom_thresholds_and_invalid_fall_back() {
        let now = 30_000 * DAY + 12 * 3600;
        let c = FileAge { week_days: 2, month_days: 10, year_days: 50, ..cfg() };
        let af = frame_at(&c, now, 0);
        assert_eq!(b(&af, now, 2 * DAY), Tint::Bucket(1, false));
        assert_eq!(b(&af, now, 10 * DAY), Tint::Bucket(2, false));
        assert_eq!(b(&af, now, 50 * DAY + 1), Tint::Bucket(4, false));
        let bad = FileAge { week_days: 9, month_days: 5, ..cfg() };
        let af = frame_at(&bad, now, 0);
        assert_eq!(b(&af, now, 7 * DAY), Tint::Bucket(1, false), "invalid thresholds use 7 / 30 / 365");
        assert_eq!(b(&af, now, 8 * DAY), Tint::Bucket(2, false));
    }

    #[test]
    fn future_unknown_and_folders() {
        let af = frame_at(&cfg(), NOON, 0);
        assert_eq!(af.classify(NOON + 5 * DAY, false), Tint::Bucket(0, true), "a future date is today, flagged");
        assert_eq!(af.classify(NOON + 1, false), Tint::Bucket(0, true));
        assert_eq!(af.classify(NOON, false), Tint::Bucket(0, false));
        assert_eq!(af.classify(0, false), Tint::Unknown);
        assert_eq!(af.classify(NOON - 10, true), Tint::None, "folders are not tinted by default");
        let with = frame_at(&FileAge { folders: true, ..cfg() }, NOON, 0);
        assert_eq!(with.classify(NOON - 10, true), Tint::Bucket(0, false));
    }

    #[test]
    fn disabled_frame_is_none_and_palettes_differ() {
        let mut g = tsubame_ui::TestCanvas::new(100.0, 100.0);
        let t = tsubame_ui::DARK;
        assert!(frame(&FileAge::default(), true, &mut g, &t).is_none(), "off by default");
        assert_ne!(palette_colours(AgePalette::Heat, true), palette_colours(AgePalette::Mono, true));
        assert_ne!(palette_colours(AgePalette::Heat, true), palette_colours(AgePalette::Heat, false));
        assert!(AgeFrame::new(&FileAge { palette: AgePalette::ColourBlind, ..cfg() }, true, NOON, |u| u, 0.0).pips);
        assert!(!frame_at(&cfg(), NOON, 0).pips);
    }

    #[test]
    fn extension_switches_off_cleanly() {
        let mut r = crate::ext::builtin();
        assert_eq!(r.is_enabled(EXT_FILE_AGE), Some(true));
        assert!(r.command(crate::commands::CMD_AGE_COLOURS).is_some());
        assert_eq!(r.set_enabled(EXT_FILE_AGE, false), Ok(true));
        assert!(r.command(crate::commands::CMD_AGE_COLOURS).is_none(), "off removes the palette command");
    }

    fn draw(af: Option<&AgeFrame>, mtime: u32, dir: bool) -> Vec<tsubame_ui::testcanvas::Op> {
        let mut g = tsubame_ui::TestCanvas::new(400.0, 40.0);
        let t = tsubame_ui::DARK;
        draw_date(af, &mut g, &t, mtime, dir, "2026-11-25 09:12", Rect::new(100.0, 0.0, 150.0, 22.0), Rect::new(0.0, 0.0, 400.0, 22.0));
        g.ops
    }

    #[test]
    fn off_draws_exactly_the_plain_cell() {
        use tsubame_ui::testcanvas::Op;
        let ops = draw(None, NOON, false);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], Op::Text(s, r, _, c) if s == "2026-11-25 09:12" && r.x == 100.0 && *c == tsubame_ui::DARK.text_dim));
    }

    #[test]
    fn styles_draw_text_chip_accent_and_pips() {
        use tsubame_ui::testcanvas::Op;
        let heat = palette_colours(AgePalette::Heat, true);
        let text = frame_at(&cfg(), NOON, 0);
        let ops = draw(Some(&text), NOON - 60, false);
        assert_eq!(ops.len(), 1, "date text is the same single draw call, a different colour");
        assert!(matches!(&ops[0], Op::Text(_, _, _, c) if *c == heat[0]));
        let chip = frame_at(&FileAge { style: AgeStyle::Chip, ..cfg() }, NOON, 0);
        let ops = draw(Some(&chip), NOON - 60, false);
        assert_eq!(ops.len(), 2, "chip adds one rounded rect");
        assert!(matches!(ops[0], Op::Round(_, _, c, a) if c == heat[0] && (a - 0.18).abs() < 1e-6));
        let bar = frame_at(&FileAge { style: AgeStyle::Accent, ..cfg() }, NOON, 0);
        let ops = draw(Some(&bar), NOON - 60, false);
        assert!(matches!(ops[0], Op::Round(r, _, c, _) if r.x == 0.0 && r.w == 3.0 && c == heat[0]));
        let cb = frame_at(&FileAge { palette: AgePalette::ColourBlind, ..cfg() }, NOON, 0);
        let ops = draw(Some(&cb), NOON - 60, false);
        assert_eq!(ops.iter().filter(|o| matches!(o, Op::Fill(..))).count(), 4, "four pips");
        assert!(matches!(ops.last(), Some(Op::Text(_, r, _, _)) if r.x == 117.0), "the date moves right to make room for the pips");
    }

    #[test]
    fn unknown_dates_and_folders_are_not_tinted() {
        use tsubame_ui::testcanvas::Op;
        let af = frame_at(&cfg(), NOON, 0);
        let ops = draw(Some(&af), 0, false);
        assert!(matches!(&ops[0], Op::Text(s, _, _, c) if s == "\u{2014}" && *c == tsubame_ui::DARK.text_dim));
        let ops = draw(Some(&af), NOON - 60, true);
        assert!(matches!(&ops[0], Op::Text(_, _, _, c) if *c == tsubame_ui::DARK.text_dim));
    }
}
