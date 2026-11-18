//! Write throttling for the config and session files.
//!
//! The app marks the state dirty on a change; the gate says when the write may
//! happen (`last write + interval`) and never asks for one when nothing
//! changed. The clock is passed in, so the tests are deterministic.
//!
//! `at most one write per 2 s, flush on exit`: the app arms a one-shot timer
//! for `due_at()` while the gate is dirty and calls `saved` after writing; on
//! exit it writes regardless of the interval.

use std::time::{Duration, Instant};

/// The minimum time between two writes of the same file.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
pub struct Debounce {
    interval: Duration,
    last: Option<Instant>,
    dirty: bool,
}

impl Debounce {
    pub fn new(interval: Duration) -> Debounce {
        Debounce { interval, last: None, dirty: false }
    }
    /// Something the file holds changed.
    pub fn changed(&mut self) {
        self.dirty = true;
    }
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    pub fn last_write(&self) -> Option<Instant> {
        self.last
    }
    /// The earliest moment the dirty state may be written. `None` when nothing
    /// is dirty or when the write is due immediately (the gate never wrote).
    pub fn due_at(&self) -> Option<Instant> {
        if !self.dirty {
            return None;
        }
        self.last.map(|t| t + self.interval)
    }
    /// Milliseconds until the app should arm its one-shot timer (0 = now).
    pub fn wait_ms(&self, now: Instant) -> Option<u32> {
        if !self.dirty {
            return None;
        }
        Some(match self.last {
            None => 0,
            Some(t) => (t + self.interval).saturating_duration_since(now).as_millis().min(u32::MAX as u128) as u32,
        })
    }
    pub fn due(&self, now: Instant) -> bool {
        self.dirty && self.last.is_none_or(|t| now >= t + self.interval)
    }
    /// A write of the current content finished at `now`.
    pub fn saved(&mut self, now: Instant) {
        self.dirty = false;
        self.last = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_dirty_never_writes() {
        let d = Debounce::new(SAVE_INTERVAL);
        assert!(!d.is_dirty());
        assert!(d.due_at().is_none());
        assert!(!d.due(Instant::now()));
        let mut dirty = Debounce::new(SAVE_INTERVAL);
        dirty.changed();
        assert!(dirty.due_at().is_none(), "never written: due at once, no moment to wait for");
        assert_eq!(dirty.wait_ms(Instant::now()), Some(0));
        assert!(dirty.due(Instant::now()));
    }

    #[test]
    fn first_change_is_due_at_once_then_throttled_for_two_seconds() {
        let mut d = Debounce::new(SAVE_INTERVAL);
        let t0 = Instant::now();
        d.changed();
        assert!(d.due(t0), "the first write needs no wait");
        d.saved(t0);
        assert!(!d.is_dirty());
        // a change right after the write waits out the interval
        d.changed();
        assert!(!d.due(t0));
        assert!(!d.due(t0 + Duration::from_millis(1999)));
        assert!(d.due(t0 + SAVE_INTERVAL));
        assert_eq!(d.due_at(), Some(t0 + SAVE_INTERVAL));
        assert_eq!(d.wait_ms(t0 + Duration::from_millis(500)), Some(1500));
        assert_eq!(d.wait_ms(t0 + SAVE_INTERVAL), Some(0));
    }

    #[test]
    fn many_changes_within_the_interval_produce_one_write() {
        let mut d = Debounce::new(SAVE_INTERVAL);
        let t0 = Instant::now();
        let mut writes = 0;
        for ms in [0u64, 100, 700, 1900] {
            let now = t0 + Duration::from_millis(ms);
            d.changed();
            if d.due(now) {
                writes += 1;
                d.saved(now);
            }
        }
        assert_eq!(writes, 1, "one write in the first 2 s");
        // 2 seconds after the write, the pending change goes out
        let now = t0 + SAVE_INTERVAL;
        assert!(d.due(now));
        writes += 1;
        d.saved(now);
        assert_eq!(writes, 2);
        assert!(!d.due(now + Duration::from_secs(10)), "nothing changed since");
    }

    #[test]
    fn a_save_clears_the_dirty_state_and_restarts_the_clock() {
        let mut d = Debounce::new(Duration::from_millis(100));
        let t0 = Instant::now();
        d.changed();
        assert!(d.due(t0));
        d.saved(t0);
        d.changed();
        assert!(!d.due(t0 + Duration::from_millis(50)));
        assert!(d.due(t0 + Duration::from_millis(100)));
        assert!(d.due_at().is_some());
    }
}
