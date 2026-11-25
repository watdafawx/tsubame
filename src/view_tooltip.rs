//! Delayed hover tooltip for file rows (Details and Grid).
//!
//! Shows after [`DELAY_MS`] (~600 ms) of hovering the same entry:
//! - Files: Name (bold), Type, Size, Date modified
//! - Folders: Name (bold), Type, Date modified (never computes folder size)
//!
//! Idle / allocation rules:
//! - Uses a single one-shot Win32 timer (`TIMER_ID`) ONLY while waiting out the
//!   600 ms hover delay. Once the tooltip appears (or when idle), no timer runs
//!   (0 wake-ups).
//! - All formatted strings live in pre-allocated `String` buffers reused across
//!   hovers; normal hover/paint frames allocate nothing.

use crate::data::{self, RowBuf};
use crate::overlay;
use std::time::{Duration, Instant};
use tsubame_core::{EntryId, EntryStore};
use tsubame_ui::{Canvas, Pt, Theme};

/// Win32 timer id owned by `main.rs` for the one-shot hover delay.
pub const TIMER_ID: usize = 13;
/// Hover dwell before the tooltip appears (ms).
pub const DELAY_MS: u32 = 600;

/// Identity of the hovered row across frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoverKey {
    pub slot: usize,
    pub nav_key: u64,
    pub entry_id: EntryId,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    Waiting { seq: u64, key: HoverKey, at: Pt, since: Instant },
    Showing { key: HoverKey, at: Pt },
}

pub struct RowTooltip {
    phase: Phase,
    next_seq: u64,
    /// Hover recorded during the current frame (cleared at `begin_frame`).
    frame_hover: Option<(HoverKey, Pt)>,
    /// Reusable text buffers formatted once when a hovered entry is shown.
    populated: Option<HoverKey>,
    is_dir: bool,
    title: String,
    line_type: String,
    line_size: String,
    line_date: String,
    tmp: String,
}

impl Default for RowTooltip {
    fn default() -> Self {
        Self::new()
    }
}

impl RowTooltip {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            next_seq: 1,
            frame_hover: None,
            populated: None,
            is_dir: false,
            title: String::with_capacity(64),
            line_type: String::with_capacity(48),
            line_size: String::with_capacity(32),
            line_date: String::with_capacity(40),
            tmp: String::with_capacity(48),
        }
    }

    /// Starts a new frame's hover tracking.
    pub fn begin_frame(&mut self) {
        self.frame_hover = None;
    }

    /// Records the row/cell currently under the pointer (Details or Grid).
    /// Only formats tooltip text when the tooltip is actually showing for `key`,
    /// keeping normal mouse-move frames allocation-free and formatting-free.
    pub fn note_hover(&mut self, key: HoverKey, at: Pt, store: &EntryStore, row: &mut RowBuf) {
        self.frame_hover = Some((key, at));
        if matches!(self.phase, Phase::Showing { key: k, .. } if k == key) && self.populated != Some(key) {
            self.populate(key, store, row);
        }
    }

    /// Populates the reused buffers for `key` from `store` (zero heap alloc once warm).
    pub fn populate(&mut self, key: HoverKey, store: &EntryStore, row: &mut RowBuf) {
        row.fill(store, key.entry_id);
        self.title.clear();
        self.title.push_str(row.name_str());
        self.is_dir = row.is_dir();

        row.type_text(&mut self.tmp);
        self.line_type.clear();
        self.line_type.push_str("Type: ");
        self.line_type.push_str(&self.tmp);

        self.line_size.clear();
        if !self.is_dir {
            data::fmt_size(row.size, &mut self.tmp);
            self.line_size.push_str("Size: ");
            self.line_size.push_str(&self.tmp);
        }

        data::fmt_date(row.mtime, &mut self.tmp);
        self.line_date.clear();
        self.line_date.push_str("Date modified: ");
        self.line_date.push_str(&self.tmp);

        self.populated = Some(key);
    }

    /// Advances the hover state machine at the end of a frame and draws the
    /// tooltip via [`overlay::tooltip`] if active.
    pub fn finish_and_paint(
        &mut self,
        g: &mut dyn Canvas,
        t: &Theme,
        win: (f32, f32),
        allowed: bool,
        now: Instant,
    ) {
        if !allowed {
            self.phase = Phase::Idle;
            return;
        }
        let Some((key, at)) = self.frame_hover else {
            self.phase = Phase::Idle;
            return;
        };
        let delay = Duration::from_millis(DELAY_MS as u64);
        match self.phase {
            Phase::Showing { key: k, at: anchor } if k == key => {
                if self.populated == Some(key) && !self.title.is_empty() {
                    self.draw(g, t, win, anchor);
                }
            }
            Phase::Waiting { key: k, at: anchor, since, .. } if k == key => {
                if now.saturating_duration_since(since) >= delay {
                    self.phase = Phase::Showing { key, at: anchor };
                    if self.populated == Some(key) && !self.title.is_empty() {
                        self.draw(g, t, win, anchor);
                    }
                }
            }
            _ => {
                let seq = self.next_seq;
                self.next_seq = self.next_seq.wrapping_add(1).max(1);
                self.phase = Phase::Waiting { seq, key, at, since: now };
            }
        }
    }

    fn draw(&self, g: &mut dyn Canvas, t: &Theme, win: (f32, f32), at: Pt) {
        if self.is_dir {
            let lines = [self.line_type.as_str(), self.line_date.as_str()];
            overlay::tooltip(g, t, win, at, &self.title, &lines);
        } else {
            let lines = [self.line_type.as_str(), self.line_size.as_str(), self.line_date.as_str()];
            overlay::tooltip(g, t, win, at, &self.title, &lines);
        }
    }

    /// `(hover_seq, remaining_ms)` for the one-shot Win32 timer, or `None` when
    /// idle or already showing (0 timers when idle or showing).
    pub fn next_timer(&self, now: Instant) -> Option<(u64, u32)> {
        match self.phase {
            Phase::Waiting { seq, since, .. } => {
                let delay = Duration::from_millis(DELAY_MS as u64);
                let rem = delay.saturating_sub(now.saturating_duration_since(since));
                Some((seq, (rem.as_millis() as u32).max(1)))
            }
            Phase::Idle | Phase::Showing { .. } => None,
        }
    }

    /// Called when the one-shot Win32 timer fires. Returns `true` when the
    /// window should repaint once to reveal the tooltip.
    pub fn on_timer(&mut self) -> bool {
        match self.phase {
            Phase::Waiting { key, at, .. } => {
                self.phase = Phase::Showing { key, at };
                true
            }
            _ => false,
        }
    }

    /// Cancels any pending or visible tooltip (on click, scroll, key, mouse leave).
    /// Returns `true` only when a visible tooltip disappeared (needs one repaint).
    pub fn cancel(&mut self) -> bool {
        let was_showing = matches!(self.phase, Phase::Showing { .. });
        self.phase = Phase::Idle;
        was_showing
    }

    #[cfg(test)]
    pub fn is_showing(&self) -> bool {
        matches!(self.phase, Phase::Showing { .. })
    }

    #[cfg(test)]
    pub fn is_waiting(&self) -> bool {
        matches!(self.phase, Phase::Waiting { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsubame_core::{EntryInput, FOLDER};
    use tsubame_ui::{DARK, TestCanvas};

    fn make_store() -> EntryStore {
        let mut s = EntryStore::new();
        let f_name: Vec<u16> = "report.pdf".encode_utf16().collect();
        let d_name: Vec<u16> = "Projects".encode_utf16().collect();
        let ft = ((1_741_945_260u64 + 11_644_473_600) * 10_000_000) as i64;
        s.push_batch(
            0,
            &[
                EntryInput { name: &f_name, size: 1_572_864, modified: ft, flags: 0 },
                EntryInput { name: &d_name, size: 999_999, modified: ft, flags: FOLDER },
            ],
        )
        .unwrap();
        s
    }

    #[test]
    fn waits_600ms_then_shows_file_and_folder_fields_with_zero_idle_timers() {
        let store = make_store();
        let mut row = RowBuf::new();
        let mut tip = RowTooltip::new();
        let mut c = TestCanvas::new(1000.0, 700.0);
        let t0 = Instant::now();
        let k_file = HoverKey { slot: 0, nav_key: 1, entry_id: 0 };

        // Frame 1: pointer enters file row -> Waiting, one-shot timer requested, nothing drawn
        tip.begin_frame();
        tip.note_hover(k_file, Pt::new(120.0, 100.0), &store, &mut row);
        tip.finish_and_paint(&mut c, &DARK, (1000.0, 700.0), true, t0);
        assert!(tip.is_waiting());
        assert!(!tip.is_showing());
        assert_eq!(tip.next_timer(t0).map(|(_, ms)| ms), Some(DELAY_MS));
        assert!(c.texts().is_empty());

        // Timer fires at 600 ms -> promotes to Showing, no more timer
        assert!(tip.on_timer());
        assert!(tip.is_showing());
        assert_eq!(tip.next_timer(t0 + Duration::from_millis(600)), None, "no timer once showing");

        // Next paint populates and draws Name, Type, Size, Date modified
        tip.begin_frame();
        tip.note_hover(k_file, Pt::new(120.0, 100.0), &store, &mut row);
        tip.finish_and_paint(&mut c, &DARK, (1000.0, 700.0), true, t0 + Duration::from_millis(600));
        let texts = c.texts();
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert_eq!(texts[0], "report.pdf");
        assert_eq!(texts[1], "Type: PDF document");
        assert_eq!(texts[2], "Size: 1.5 MB");
        assert!(texts[3].starts_with("Date modified: "), "{:?}", texts[3]);

        // Folder: shows Name, Type, Date modified ONLY (no Size line!)
        let k_dir = HoverKey { slot: 0, nav_key: 1, entry_id: 1 };
        c.clear();
        tip.begin_frame();
        tip.note_hover(k_dir, Pt::new(120.0, 130.0), &store, &mut row);
        tip.finish_and_paint(&mut c, &DARK, (1000.0, 700.0), true, t0 + Duration::from_millis(700));
        assert!(tip.is_waiting(), "moving to another row restarts dwell");
        assert!(tip.on_timer());
        tip.begin_frame();
        tip.note_hover(k_dir, Pt::new(120.0, 130.0), &store, &mut row);
        tip.finish_and_paint(&mut c, &DARK, (1000.0, 700.0), true, t0 + Duration::from_millis(1300));
        let texts = c.texts();
        assert_eq!(texts.len(), 3, "folder tooltip has no Size line: {texts:?}");
        assert_eq!(texts[0], "Projects");
        assert_eq!(texts[1], "Type: File folder");
        assert!(texts[2].starts_with("Date modified: "));

        // Cancel returns true when hiding a visible tooltip, false when already idle
        assert!(tip.cancel());
        assert!(!tip.cancel());
        assert_eq!(tip.next_timer(t0 + Duration::from_millis(1400)), None);
    }

    #[test]
    fn disabled_toggle_suppresses_tooltip_and_timer() {
        let store = make_store();
        let mut row = RowBuf::new();
        let mut tip = RowTooltip::new();
        let mut c = TestCanvas::new(1000.0, 700.0);
        let t0 = Instant::now();
        let k = HoverKey { slot: 0, nav_key: 1, entry_id: 0 };
        tip.begin_frame();
        tip.note_hover(k, Pt::new(100.0, 100.0), &store, &mut row);
        tip.finish_and_paint(&mut c, &DARK, (1000.0, 700.0), false, t0);
        assert!(!tip.is_waiting() && !tip.is_showing());
        assert_eq!(tip.next_timer(t0), None);
        assert!(c.texts().is_empty());
    }
}
