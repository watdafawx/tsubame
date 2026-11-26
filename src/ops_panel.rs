//! The Operations panel (B11-E2, `mocks/progress-panel.html`,
//! `docs/progress-panel-mock.md`): one card per file operation, docked
//! bottom-right over the file list, and the status-bar pill when it is folded.
//!
//! - **Model first**: `Panel` holds the cards; `Ops` (src/ops.rs) feeds it from
//!   the queue's events and the extract adapter. Every rule (open delay,
//!   auto-dismiss, hover hold, queue order) is a plain function of `now`, so the
//!   tests drive it without a window.
//! - **Cost**: nothing here owns a timer. Progress arrives on main's
//!   `OPS_TIMER` (only while a job runs) and repaints at most every
//!   `PAINT_EVERY` (`Ops::want_paint`), only when a card's numbers changed. The
//!   only other wake-up is ONE one-shot timer for the earliest finished-card
//!   dismiss (`next_dismiss`, armed by main after a paint). The countdown line
//!   is drawn from `now` at paint time: it does not animate on its own.
//! - Speed is UI side: an EMA of the job's `bytes_done` deltas, sampled at most
//!   4 times a second, and a fixed 40-float ring for the 10 s sparkline. Paused
//!   time never counts (`Speed::pause` drops the baseline).

use crate::data;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use tsubame_ui::{Button, Canvas, Id, Rect, Sense, Theme, Ui, f};
use tsubame_win::fileops::{Cancellation, Operation, Totals};

/// The panel opens this long after an operation starts (quick ones never flash).
pub const OPEN_DELAY: Duration = Duration::from_millis(400);
/// A finished or cancelled card stays this long (hover holds it).
pub const DISMISS: Duration = Duration::from_secs(5);
/// Progress repaints at most this often (10 Hz).
pub const PAINT_EVERY: Duration = Duration::from_millis(100);
/// One speed sample per this much time (4 samples a second).
pub const SAMPLE: Duration = Duration::from_millis(250);
/// Sparkline length: 10 s of samples.
pub const RING: usize = 40;
/// EMA weight of a new speed sample.
const ALPHA: f64 = 0.3;

pub const PANEL_W: f32 = 344.0;

const ID_PANEL: Id = Id::new("ops-panel");
const ID_CLEAR: Id = Id::new("ops-clear");
const ID_COLLAPSE: Id = Id::new("ops-collapse");
const ID_PILL: Id = Id::new("ops-pill");
const ID_CARD: Id = Id::new("ops-card");

// Segoe Fluent Icons
const G_COPY: &str = "\u{E8C8}";
const G_MOVE: &str = "\u{E8DE}";
const G_DELETE: &str = "\u{E74D}";
const G_ZIP: &str = "\u{E7B8}";
const G_OTHER: &str = "\u{E895}";
const G_PAUSE: &str = "\u{E769}";
const G_PLAY: &str = "\u{E768}";
const G_DOWN: &str = "\u{E70D}";
const G_UP: &str = "\u{E70E}";
const G_X: &str = "\u{E711}";
const G_CHECK: &str = "\u{E73E}";
const G_ALERT: &str = "\u{E7BA}";
const G_RETRY: &str = "\u{E72C}";
const G_FILE: &str = "\u{E7C3}";

// ---- speed ---------------------------------------------------------------------

/// A fixed ring of the last `RING` speed samples (oldest first when read).
#[derive(Clone, Debug)]
pub struct Ring {
    buf: [f32; RING],
    len: usize,
    head: usize,
}

impl Default for Ring {
    fn default() -> Self {
        Ring { buf: [0.0; RING], len: 0, head: 0 }
    }
}

impl Ring {
    pub fn push(&mut self, v: f32) {
        self.buf[self.head] = v;
        self.head = (self.head + 1) % RING;
        self.len = (self.len + 1).min(RING);
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Sample `i`, 0 = oldest kept.
    pub fn get(&self, i: usize) -> f32 {
        let start = (self.head + RING - self.len) % RING;
        self.buf[(start + i) % RING]
    }
    pub fn max(&self) -> f32 {
        if self.is_empty() {
            return 0.0;
        }
        (0..self.len).map(|i| self.get(i)).fold(0.0, f32::max)
    }
}

/// Speed of one operation: EMA of the done-counter's deltas plus the sparkline.
#[derive(Clone, Debug, Default)]
pub struct Speed {
    /// units (bytes or items) per second
    pub ema: f64,
    last: Option<(Instant, u64)>,
    pub ring: Ring,
}

impl Speed {
    /// The done counter is `done` at `now`. At most one sample per `SAMPLE`.
    pub fn sample(&mut self, now: Instant, done: u64) {
        let Some((t, d)) = self.last else {
            self.last = Some((now, done));
            return;
        };
        let dt = now.saturating_duration_since(t);
        if dt < SAMPLE {
            return;
        }
        let rate = done.saturating_sub(d) as f64 / dt.as_secs_f64();
        self.ema = if self.ema <= 0.0 { rate } else { self.ema + ALPHA * (rate - self.ema) };
        self.ring.push(rate as f32);
        self.last = Some((now, done));
    }
    /// Paused (or waiting for an answer): the next sample starts a new baseline,
    /// so the pause never counts as slow.
    pub fn pause(&mut self) {
        self.last = None;
    }
    /// Time for `left` units at the current speed (None: no speed yet).
    pub fn eta(&self, left: u64) -> Option<Duration> {
        (self.ema > 0.0).then(|| Duration::from_secs_f64(left as f64 / self.ema))
    }
}

/// "a moment", "0:42", "12:05", "1:02:03".
pub fn eta_text(d: Option<Duration>, out: &mut String) {
    out.clear();
    let Some(d) = d else {
        out.push('\u{2026}');
        return;
    };
    let s = d.as_secs_f64().ceil() as u64;
    if s < 2 {
        out.push_str("a moment");
        return;
    }
    let (h, m, x) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        let _ = write!(out, "{h}:{m:02}:{x:02}");
    } else {
        let _ = write!(out, "{m}:{x:02}");
    }
}

// ---- cards -----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// queued behind another job (FIFO, one worker)
    Waiting,
    Running,
    Paused,
    /// the conflict dialog waits for the user
    Asking,
    Done,
    Cancelled,
    /// finished with failures: never auto-dismissed
    Failed,
}

impl State {
    pub fn active(self) -> bool {
        matches!(self, State::Waiting | State::Running | State::Paused | State::Asking)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Copy,
    Move,
    Delete,
    Extract,
    Other,
}

impl Kind {
    pub fn of_verb(verb: &str) -> Kind {
        match verb {
            "Copying" => Kind::Copy,
            "Moving" => Kind::Move,
            "Deleting" => Kind::Delete,
            "Extracting" => Kind::Extract,
            _ => Kind::Other,
        }
    }
    fn glyph(self) -> &'static str {
        match self {
            Kind::Copy => G_COPY,
            Kind::Move => G_MOVE,
            Kind::Delete => G_DELETE,
            Kind::Extract => G_ZIP,
            Kind::Other => G_OTHER,
        }
    }
}

/// Which producer a card belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// a `fileops` queue job (its ticket id = its undo group)
    Queue(u64),
    /// an archive extraction (`archive_ui`), outside the queue
    Extract(u64),
}

impl Key {
    /// A UIA / widget id part.
    pub fn code(self) -> u64 {
        match self {
            Key::Queue(i) => i << 1,
            Key::Extract(i) => (i << 1) | 1,
        }
    }
}

/// How a card stops its work.
#[derive(Clone)]
pub enum Stop {
    Queue(Cancellation),
    Extract(Arc<AtomicBool>),
    None,
}

/// One operation in the panel.
#[derive(Clone)]
pub struct Card {
    pub key: Key,
    pub kind: Kind,
    pub verb: &'static str,
    pub done_verb: &'static str,
    /// operations submitted (what the result counts)
    pub count: usize,
    /// source and destination folder names (full paths for the tooltip)
    pub from: String,
    pub to: String,
    pub from_path: String,
    pub to_path: String,
    pub state: State,
    pub started: Option<Instant>,
    /// the card has been on screen (a quick job that never was leaves silently)
    pub shown: bool,
    /// the job's own totals (last progress)
    pub totals: Totals,
    /// current operation index, its done bytes and its estimate
    pub index: usize,
    pub item_done: u64,
    pub item_total: u64,
    /// current item name and the next two (expanded view)
    pub files: [String; 3],
    /// source names of the operations (for "next" rows); empty for undo / extract
    pub names: Arc<[String]>,
    pub speed: Speed,
    pub expanded: bool,
    pub details: bool,
    pub errors: Vec<(PathBuf, String)>,
    /// the operations that failed (Retry submits them again)
    pub failed: Vec<Operation>,
    /// files done when it was cancelled
    pub done_at_cancel: u64,
    pub dismiss_at: Option<Instant>,
    /// remaining countdown while the pointer holds it
    pub held: Option<Duration>,
    /// undo / redo itself (no Undo link)
    pub history: bool,
    pub permanent: bool,
    pub stop: Stop,
    /// numbers changed since the last paint
    pub dirty: bool,
}

/// The folder name of `p` for "from A to B".
pub fn folder_name(p: &str) -> String {
    let t = p.trim_end_matches('\\');
    match Path::new(t).file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => t.to_string(),
    }
}

impl Card {
    pub fn new(key: Key, verb: &'static str, done_verb: &'static str, count: usize) -> Card {
        Card {
            key,
            kind: Kind::of_verb(verb),
            verb,
            done_verb,
            count,
            from: String::new(),
            to: String::new(),
            from_path: String::new(),
            to_path: String::new(),
            state: State::Waiting,
            started: None,
            shown: false,
            totals: Totals::default(),
            index: 0,
            item_done: 0,
            item_total: 0,
            files: Default::default(),
            names: Arc::from(Vec::new()),
            speed: Speed::default(),
            expanded: false,
            details: false,
            errors: Vec::new(),
            failed: Vec::new(),
            done_at_cancel: 0,
            dismiss_at: None,
            held: None,
            history: false,
            permanent: false,
            stop: Stop::None,
            dirty: true,
        }
    }
    pub fn with_folders(mut self, from: &str, to: &str) -> Card {
        self.from_path = from.to_string();
        self.to_path = to.to_string();
        self.from = folder_name(from);
        self.to = folder_name(to);
        self
    }
    /// Deleting counts items, not bytes.
    pub fn counts_items(&self) -> bool {
        self.kind == Kind::Delete || (self.totals.bytes_total == 0 && self.totals.files_total > 0)
    }
    /// 0..=1 of the job.
    pub fn fraction(&self) -> f32 {
        let t = self.totals;
        let f = if self.counts_items() {
            t.files_done as f64 / t.files_total.max(1) as f64
        } else if t.bytes_total > 0 {
            t.bytes_done as f64 / t.bytes_total as f64
        } else {
            t.files_done as f64 / t.files_total.max(1) as f64
        };
        f.clamp(0.0, 1.0) as f32
    }
    pub fn percent(&self) -> u32 {
        (self.fraction() * 100.0).floor() as u32
    }
    fn done_units(&self) -> u64 {
        if self.counts_items() { self.totals.files_done } else { self.totals.bytes_done }
    }
    fn left_units(&self) -> u64 {
        let t = self.totals;
        if self.counts_items() { t.files_total.saturating_sub(t.files_done) } else { t.bytes_total.saturating_sub(t.bytes_done) }
    }
    /// Can the Undo link run (it is shown only then).
    pub fn can_undo(&self) -> bool {
        !self.history && !self.permanent && matches!(self.key, Key::Queue(_)) && matches!(self.state, State::Done | State::Cancelled)
    }
    /// Sets the current item and the two after it, from the operation list.
    fn set_current(&mut self, index: usize, current: &Path) {
        if index == self.index && !self.files[0].is_empty() {
            return;
        }
        self.index = index;
        self.files[0] = current.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for k in 1..3 {
            self.files[k] = self.names.get(index + k).cloned().unwrap_or_default();
        }
    }
}

// ---- the panel ---------------------------------------------------------------------

/// What a click (or a screen reader's Invoke) asks `Ops` to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Pause(Key),
    Resume(Key),
    Cancel(Key),
    Undo(Key),
    Retry(Key),
    /// drop the card (dismiss x, Skip on an error card)
    Dismiss(Key),
    Expand(Key),
    Details(Key),
    ClearFinished,
    Collapse,
    Open,
}

#[derive(Default)]
pub struct Panel {
    /// queue order (submission order)
    pub cards: Vec<Card>,
    /// on screen
    pub open: bool,
    /// the user folded it into the pill: it stays folded until the cards are gone
    pub collapsed: bool,
    pub scroll: f32,
    /// a card under the pointer (its countdown holds)
    hover: Option<Key>,
    /// cards were added, removed or changed state
    pub layout_dirty: bool,
}

impl Panel {
    pub fn card(&self, key: Key) -> Option<&Card> {
        self.cards.iter().find(|c| c.key == key)
    }
    pub fn card_mut(&mut self, key: Key) -> Option<&mut Card> {
        self.cards.iter_mut().find(|c| c.key == key)
    }
    pub fn add(&mut self, card: Card) {
        self.cards.push(card);
        self.layout_dirty = true;
    }
    pub fn remove(&mut self, key: Key) {
        self.cards.retain(|c| c.key != key);
        self.layout_dirty = true;
        if self.hover == Some(key) {
            self.hover = None;
        }
    }
    /// The pill shows: there are cards and the panel is folded.
    pub fn pill_visible(&self) -> bool {
        !self.cards.is_empty() && self.collapsed && self.cards.iter().any(|c| c.shown)
    }

    /// `--bench` with `TSUBAME_BENCH_OPS=1`: one copy card that runs through
    /// the whole scroll run (2 GB at 400 MB/s, wrapping), so the scroll budget
    /// is measured with the panel open and its numbers changing every frame.
    pub fn bench_tick(&mut self, now: Instant) {
        const KEY: Key = Key::Queue(1 << 32); // far above real ids; a11y keys multiply it
        const TOTAL: u64 = 2_000_000_000;
        if self.card_mut(KEY).is_none() {
            self.add(Card::new(KEY, "Copying", "Copied", 1406).with_folders("G:\\rig-scratch\\src", "G:\\rig-scratch\\dst"));
            self.started(KEY, now.checked_sub(OPEN_DELAY).unwrap_or(now));
            self.tick(now);
        }
        let Some(start) = self.card_mut(KEY).and_then(|c| c.started) else { return };
        let done = (now.saturating_duration_since(start).as_millis() as u64 * 400_000) % TOTAL;
        let file = 1_400_000;
        let job = Totals { files_done: done / file, files_total: 1406, bytes_done: done, bytes_total: TOTAL };
        self.progress(KEY, now, job, (done / file) as usize, Path::new("G:\\rig-scratch\\src\\big\\big3.bin"), done % 256_000_000, 256_000_000);
    }

    pub fn started(&mut self, key: Key, now: Instant) {
        if let Some(c) = self.card_mut(key)
            && c.state == State::Waiting
        {
            c.state = State::Running;
            c.started = Some(now);
            c.speed.sample(now, 0);
            c.dirty = true;
            self.layout_dirty = true;
        }
    }

    /// A progress event: the job's totals, the current item.
    #[allow(clippy::too_many_arguments)]
    pub fn progress(&mut self, key: Key, now: Instant, job: Totals, index: usize, current: &Path, item_done: u64, item_total: u64) {
        let Some(c) = self.card_mut(key) else { return };
        if c.state == State::Waiting {
            c.state = State::Running;
            c.started.get_or_insert(now);
        }
        c.totals = job;
        c.item_done = item_done;
        c.item_total = item_total;
        c.set_current(index, current);
        if c.state == State::Running {
            let d = c.done_units();
            c.speed.sample(now, d);
        }
        c.dirty = true;
    }

    pub fn set_paused(&mut self, key: Key, paused: bool) {
        let Some(c) = self.card_mut(key) else { return };
        match (c.state, paused) {
            (State::Running, true) => {
                c.state = State::Paused;
                c.speed.pause();
            }
            (State::Paused, false) => c.state = State::Running,
            _ => return,
        }
        c.dirty = true;
        self.layout_dirty = true;
    }

    /// The conflict dialog opened (true) or was answered (false) for `key`.
    pub fn set_asking(&mut self, key: Key, asking: bool) {
        let Some(c) = self.card_mut(key) else { return };
        match (c.state, asking) {
            (State::Running | State::Waiting, true) => {
                c.state = State::Asking;
                c.speed.pause();
            }
            (State::Asking, false) => c.state = State::Running,
            _ => return,
        }
        c.dirty = true;
        self.layout_dirty = true;
    }

    pub fn error(&mut self, key: Key, path: PathBuf, message: String, op: Option<Operation>) {
        let Some(c) = self.card_mut(key) else { return };
        c.errors.push((path, message));
        c.failed.extend(op);
    }

    /// The job ended. A card that never showed leaves quietly unless it failed;
    /// a cancelled job that never started leaves too.
    pub fn finished(&mut self, key: Key, now: Instant, cancelled: bool, job: Totals) {
        let Some(i) = self.cards.iter().position(|c| c.key == key) else { return };
        let c = &mut self.cards[i];
        let was = c.state;
        if job != Totals::default() {
            c.totals = job;
        }
        c.state = if !c.errors.is_empty() {
            State::Failed
        } else if cancelled {
            State::Cancelled
        } else {
            State::Done
        };
        c.dirty = true;
        self.layout_dirty = true;
        let never_ran = was == State::Waiting && cancelled;
        if never_ran || (!c.shown && c.state != State::Failed) {
            self.cards.remove(i);
            return;
        }
        if matches!(c.state, State::Done | State::Cancelled) {
            if self.hover == Some(key) {
                c.held = Some(DISMISS);
            } else {
                c.dismiss_at = Some(now + DISMISS);
            }
        }
        if c.state == State::Failed && !self.collapsed {
            // a failure is never silent: the panel shows it now
            self.open = true;
            self.cards[i].shown = true;
        }
    }

    /// The user cancelled `key` (the job's Finished follows): a waiting card goes
    /// at once, a running one shows "Cancelled after N of M".
    pub fn cancelled(&mut self, key: Key, now: Instant) {
        let Some(i) = self.cards.iter().position(|c| c.key == key) else { return };
        let c = &mut self.cards[i];
        match c.state {
            State::Waiting => {
                self.cards.remove(i);
            }
            s if s.active() => {
                c.done_at_cancel = c.totals.files_done;
                c.state = State::Cancelled;
                c.dismiss_at = if self.hover == Some(key) { None } else { Some(now + DISMISS) };
                c.held = if self.hover == Some(key) { Some(DISMISS) } else { None };
                c.dirty = true;
            }
            _ => {}
        }
        self.layout_dirty = true;
    }

    /// The pointer moved onto `key` (or off every card): countdowns hold while held.
    pub fn hover(&mut self, key: Option<Key>, now: Instant) {
        if self.hover == key {
            return;
        }
        if let Some(old) = self.hover
            && let Some(c) = self.card_mut(old)
            && let Some(left) = c.held.take()
        {
            c.dismiss_at = Some(now + left);
        }
        if let Some(new) = key
            && let Some(c) = self.card_mut(new)
            && let Some(at) = c.dismiss_at.take()
        {
            c.held = Some(at.saturating_duration_since(now));
        }
        self.hover = key;
    }

    /// Time passes: open after the delay, drop expired cards, close when empty.
    /// True when something visible changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        let before = self.cards.len();
        self.cards.retain(|c| c.dismiss_at.is_none_or(|t| t > now));
        if self.cards.len() != before {
            changed = true;
            self.layout_dirty = true;
            if self.hover.is_some_and(|h| !self.cards.iter().any(|c| c.key == h)) {
                self.hover = None;
            }
        }
        if !self.open && !self.collapsed && self.cards.iter().any(|c| c.state.active() && c.started.is_some_and(|s| now.saturating_duration_since(s) >= OPEN_DELAY)) {
            self.open = true;
            changed = true;
        }
        if self.open || self.collapsed {
            for c in &mut self.cards {
                c.shown = true;
            }
        }
        if self.cards.is_empty() && (self.open || self.collapsed) {
            self.open = false;
            self.collapsed = false;
            self.scroll = 0.0;
            changed = true;
        }
        changed
    }

    /// When the earliest finished card goes (main's one-shot timer). None: no timer.
    pub fn next_dismiss(&self) -> Option<Instant> {
        self.cards.iter().filter_map(|c| c.dismiss_at).min()
    }

    /// Some card's numbers changed since the last paint.
    pub fn dirty(&self) -> bool {
        self.layout_dirty || self.cards.iter().any(|c| c.dirty)
    }
    fn painted(&mut self) {
        self.layout_dirty = false;
        for c in &mut self.cards {
            c.dirty = false;
        }
    }

    /// Finished cards first (newest on top), then the active ones in queue order.
    pub fn order(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.cards.len()).filter(|&i| !self.cards[i].state.active()).rev().collect();
        v.extend((0..self.cards.len()).filter(|&i| self.cards[i].state.active()));
        v
    }

    pub fn collapse(&mut self) {
        self.open = false;
        self.collapsed = true;
        self.layout_dirty = true;
    }
    pub fn expand_panel(&mut self) {
        self.open = true;
        self.collapsed = false;
        for c in &mut self.cards {
            c.shown = true;
        }
        self.layout_dirty = true;
    }
    pub fn clear_finished(&mut self) {
        self.cards.retain(|c| c.state.active());
        self.layout_dirty = true;
    }

    /// Mean progress of the active cards, 0..=100 (the pill).
    pub fn aggregate_percent(&self) -> u32 {
        let act: Vec<&Card> = self.cards.iter().filter(|c| c.state.active()).collect();
        if act.is_empty() {
            return 0;
        }
        let sum: f32 = act.iter().map(|c| c.fraction()).sum();
        (sum / act.len() as f32 * 100.0).round() as u32
    }

    /// The pill's text: "Copying, 43%", "2 operations, 43%", "Done", "1 failed".
    pub fn pill_text(&self, out: &mut String) -> PillIcon {
        out.clear();
        let act: Vec<&Card> = self.cards.iter().filter(|c| c.state.active()).collect();
        let failed = self.cards.iter().filter(|c| c.state == State::Failed).count();
        if act.len() == 1 {
            let _ = write!(out, "{}, {}%", act[0].verb, self.aggregate_percent());
            PillIcon::Bar
        } else if !act.is_empty() {
            let _ = write!(out, "{} operations, {}%", act.len(), self.aggregate_percent());
            PillIcon::Bar
        } else if failed > 0 {
            let _ = write!(out, "{failed} failed");
            PillIcon::Failed
        } else {
            out.push_str("Done");
            PillIcon::Done
        }
    }

    /// Height of card `c` (mock metrics).
    fn card_h(c: &Card) -> f32 {
        match c.state {
            State::Done | State::Cancelled => 39.0,
            State::Failed => {
                let mut h = 8.0 + 22.0 + 18.0 + 6.0 + 24.0 + 9.0;
                if c.details {
                    h += 6.0 + 4.0 + c.errors.len() as f32 * 36.0;
                }
                h
            }
            _ => {
                let mut h = 8.0 + 22.0 + 18.0 + 15.0 + 16.0 + 9.0;
                if c.expanded && c.state != State::Waiting {
                    h += 6.0 + 5.0 + 22.0 * Self::file_rows(c) as f32;
                }
                h
            }
        }
    }
    fn file_rows(c: &Card) -> usize {
        let named = c.files.iter().filter(|s| !s.is_empty()).count().max(1);
        named + usize::from(more_count(c) > 0)
    }
}

fn more_count(c: &Card) -> usize {
    let n = c.names.len().max(c.count);
    n.saturating_sub(c.index + 3)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PillIcon {
    Bar,
    Done,
    Failed,
}

// ---- paint -------------------------------------------------------------------------

/// Per-paint scratch: strings reused between cards.
#[derive(Default)]
struct Scratch {
    a: String,
    b: String,
    c: String,
    /// a screen reader listens: button rects are recorded for UIA
    uia: bool,
    rects: Vec<(u64, Rect)>,
}

impl Scratch {
    fn hit(&mut self, key: u64, r: Rect) {
        if self.uia {
            self.rects.push((key, r));
        }
    }
}

/// A 22 px icon button. True when clicked.
fn icon_button(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, glyph: &str, danger: bool) -> bool {
    let it = ui.interact(id, r, Sense::click().focusable());
    let mut clicked = it.clicked == Some(Button::Left);
    for (qi, ev) in ui.key_input(id) {
        if let tsubame_ui::Event::KeyDown { key: tsubame_ui::Key::Enter | tsubame_ui::Key::Space, mods, .. } = ev
            && mods.none()
        {
            ui.consume(qi);
            clicked = true;
        }
    }
    let col = if it.hovered && danger {
        g.round(r, t.radius_sm, t.danger_fill);
        t.on_danger
    } else if it.hovered {
        g.round(r, t.radius_sm, t.hover);
        t.text
    } else {
        t.text_dim
    };
    if it.focused && ui.window_focused() {
        g.round_stroke(r.pad(-1.0), t.radius_sm, t.focus, 1.0);
    }
    g.text(glyph, r, f(11.0).icon().c(), col);
    if clicked {
        ui.request_repaint();
    }
    clicked
}

/// An accent text link. True when clicked.
#[allow(clippy::too_many_arguments)]
fn link(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, x: f32, y: f32, h: f32, label: &str) -> (bool, f32) {
    let font = t.font();
    let w = g.measure(label, font) + 8.0;
    let r = Rect::new(x, y, w, h);
    let it = ui.interact(id, r, Sense::click().focusable());
    let mut clicked = it.clicked == Some(Button::Left);
    for (qi, ev) in ui.key_input(id) {
        if let tsubame_ui::Event::KeyDown { key: tsubame_ui::Key::Enter | tsubame_ui::Key::Space, mods, .. } = ev
            && mods.none()
        {
            ui.consume(qi);
            clicked = true;
        }
    }
    // +2: a rect exactly as wide as the measured text ellipsises it
    g.text(label, Rect::new(x + 4.0, y, w - 6.0, h), font, t.accent);
    if it.hovered {
        g.fill(Rect::new(x + 4.0, y + h / 2.0 + 7.0, w - 8.0, 1.0), t.accent);
    }
    if it.focused && ui.window_focused() {
        g.round_stroke(r, t.radius_sm, t.focus, 1.0);
    }
    if clicked {
        ui.request_repaint();
    }
    (clicked, w)
}

/// A 24 px text button; `primary` is filled with the accent.
#[allow(clippy::too_many_arguments)]
pub fn text_button(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, label: &str, glyph: Option<&str>, primary: bool) -> bool {
    let it = ui.interact(id, r, Sense::click().focusable());
    let mut clicked = it.clicked == Some(Button::Left);
    for (qi, ev) in ui.key_input(id) {
        if let tsubame_ui::Event::KeyDown { key: tsubame_ui::Key::Enter | tsubame_ui::Key::Space, mods, .. } = ev
            && mods.none()
        {
            ui.consume(qi);
            clicked = true;
        }
    }
    let (fill, fg) = if primary { (Some(t.accent), t.on_accent) } else if it.hovered { (Some(t.hover), t.text) } else { (None, t.text) };
    if let Some(c) = fill {
        g.round(r, t.radius_md, c);
    }
    if !primary {
        g.round_stroke(r, t.radius_md, t.popup_border, 1.0);
    }
    if (primary && it.hovered) || (it.focused && ui.window_focused()) {
        g.round_stroke(r.pad(-1.0), t.radius_md + 1.0, t.focus, 1.0);
    }
    let font = if primary { t.font_bold() } else { t.font() };
    let lw = g.measure(label, font);
    let gw = if glyph.is_some() { 20.0 } else { 0.0 };
    let x = r.x + (r.w - lw - gw) / 2.0;
    if let Some(gl) = glyph {
        g.text(gl, Rect::new(x, r.y, 14.0, r.h), f(11.0).icon().c(), fg);
    }
    g.text(label, Rect::new(x + gw, r.y, lw + 2.0, r.h), font, fg);
    if clicked {
        ui.request_repaint();
    }
    clicked
}

/// Width of a text button for `label`.
pub fn text_button_w(g: &mut dyn Canvas, t: &Theme, label: &str, glyph: bool, primary: bool) -> f32 {
    let font = if primary { t.font_bold() } else { t.font() };
    g.measure(label, font) + 20.0 + if glyph { 20.0 } else { 0.0 }
}

/// Draws `parts` (text, emphasised) on one line from `x`, ellipsising the
/// emphasised names when the line is too long.
#[allow(clippy::too_many_arguments)]
pub fn rich_line(g: &mut dyn Canvas, t: &Theme, x: f32, y: f32, w: f32, h: f32, parts: &[(&str, bool)], size: f32) {
    let plain = f(size);
    let widths: Vec<f32> = parts.iter().map(|(s, _)| g.measure(s, plain)).collect();
    let total: f32 = widths.iter().sum();
    let names: f32 = parts.iter().zip(&widths).filter(|((_, e), _)| *e).map(|(_, w)| *w).sum();
    let scale = if total > w && names > 0.0 { ((names - (total - w)) / names).max(0.2) } else { 1.0 };
    let mut cx = x;
    for ((s, emph), pw) in parts.iter().zip(widths) {
        let pw = if *emph { pw * scale } else { pw };
        let room = (x + w - cx).max(0.0);
        let dw = pw.min(room);
        if dw <= 0.0 {
            break;
        }
        g.text(s, Rect::new(cx, y, dw + 1.0, h), plain, if *emph { t.text } else { t.text_dim });
        cx += dw;
    }
}

/// Paints the panel docked bottom-right in `area` (the window body above the
/// status bar). Returns the user's action.
pub fn paint(panel: &mut Panel, ui: &mut Ui, g: &mut dyn Canvas, area: Rect, now: Instant) -> Option<Act> {
    if !panel.open || panel.cards.is_empty() {
        panel.painted();
        return None;
    }
    let t = ui.theme;
    let order = panel.order();
    let content: f32 = order.iter().map(|&i| Panel::card_h(&panel.cards[i]) + 1.0).sum::<f32>() - 1.0;
    let header = 28.0;
    let max_h = (area.h - 16.0).max(header + 40.0);
    let h = (header + content).min(max_h);
    let pr = Rect::new(area.r() - 14.0 - PANEL_W, area.b() - 8.0 - h, PANEL_W, h);
    // swallows clicks and wheel over the panel that no button takes
    let blocker = ui.interact(ID_PANEL, pr, Sense::click().wheel());
    let view_h = h - header;
    let max_scroll = (content - view_h).max(0.0);
    if blocker.wheel.1 != 0.0 {
        panel.scroll = (panel.scroll - blocker.wheel.1 * 40.0).clamp(0.0, max_scroll);
        ui.request_repaint();
    }
    panel.scroll = panel.scroll.clamp(0.0, max_scroll);
    g.round(pr, t.radius_lg, t.popup);
    g.round_stroke(pr, t.radius_lg, t.popup_border, 1.0);
    let mut act = None;
    let mut s = Scratch { uia: crate::uia_rows::on(), ..Default::default() };

    // header
    let hr = Rect::new(pr.x, pr.y, pr.w, header);
    let title_f = t.font_bold();
    let tw = g.measure("Operations", title_f);
    g.text("Operations", Rect::new(hr.x + 10.0, hr.y, tw + 2.0, hr.h), title_f, t.text);
    let active = panel.cards.iter().filter(|c| c.state.active()).count();
    if active > 0 {
        s.a.clear();
        let _ = write!(s.a, "{active}");
        let cf = t.font_small();
        let cw = g.measure(&s.a, cf) + 12.0;
        let chip = Rect::new(hr.x + 10.0 + tw + 6.0, hr.y + 6.0, cw, 16.0);
        g.round_stroke(chip, 8.0, t.border, 1.0);
        g.text(&s.a, chip, cf.c(), t.text_dim);
    }
    let mut rx = hr.r() - 4.0 - 22.0;
    let col_r = Rect::new(rx, hr.y + 3.0, 22.0, 22.0);
    s.hit(A11Y_COLLAPSE, col_r);
    if icon_button(ui, g, &t, ID_COLLAPSE, col_r, G_DOWN, false) {
        act = Some(Act::Collapse);
    }
    let finished = panel.cards.iter().any(|c| !c.state.active());
    if finished {
        let lw = g.measure("Clear finished", t.font()) + 8.0;
        rx -= lw + 4.0;
        s.hit(A11Y_CLEAR, Rect::new(rx, hr.y + 3.0, lw, 22.0));
        if link(ui, g, &t, ID_CLEAR, rx, hr.y + 3.0, 22.0, "Clear finished").0 {
            act = Some(Act::ClearFinished);
        }
    }
    g.fill(Rect::new(pr.x, hr.b() - 1.0, pr.w, 1.0), t.border);

    // cards
    let cards_r = Rect::new(pr.x + 1.0, hr.b(), pr.w - 2.0, view_h - 1.0);
    g.clip(cards_r);
    let mut y = cards_r.y - panel.scroll;
    let mut hover = None;
    let thumb = max_scroll > 0.0;
    let inner_w = PANEL_W - 20.0 - if thumb { 8.0 } else { 0.0 };
    for (n, &i) in order.iter().enumerate() {
        let ch = Panel::card_h(&panel.cards[i]);
        if n > 0 {
            g.fill(Rect::new(pr.x, y, pr.w, 1.0), t.border);
            y += 1.0;
        }
        let cr = Rect::new(pr.x, y, PANEL_W, ch);
        if cr.b() >= cards_r.y && cr.y <= cards_r.b() {
            let c = &panel.cards[i];
            let hit = ui.interact(ID_CARD.with(c.key.code()), Rect::new(cr.x, cr.y.max(cards_r.y), cr.w, (cr.b().min(cards_r.b()) - cr.y.max(cards_r.y)).max(0.0)), Sense::hover());
            if hit.hovered {
                hover = Some(c.key);
            }
            if let Some(a) = paint_card(ui, g, &t, c, Rect::new(pr.x + 10.0, y, inner_w, ch), cr, now, &mut s) {
                act = Some(a);
            }
        }
        y += ch;
    }
    g.unclip();
    if thumb {
        let track = Rect::new(pr.r() - 8.0, cards_r.y + 2.0, 6.0, cards_r.h - 4.0);
        let th = (track.h * view_h / content).max(16.0);
        let ty = track.y + (track.h - th) * (panel.scroll / max_scroll);
        g.round(Rect::new(track.x, ty, 6.0, th), 3.0, t.scrollbar);
    }
    panel.hover(hover, now);
    if s.uia {
        publish_uia(panel, pr, &order, &s.rects);
    }
    panel.painted();
    act
}

#[allow(clippy::too_many_arguments)]
fn paint_card(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, c: &Card, r: Rect, cr: Rect, now: Instant, s: &mut Scratch) -> Option<Act> {
    let id = |n: u64| ID_CARD.with(c.key.code() * 16 + n);
    let mut act = None;
    let x = r.x;
    let mut y = r.y + 8.0;
    let small = t.font_small();
    match c.state {
        State::Done | State::Cancelled => {
            let row = Rect::new(x, y, r.w, 22.0);
            let (glyph, col) = if c.state == State::Done { (G_CHECK, t.ok) } else { (G_X, t.text_dim) };
            g.text(glyph, Rect::new(x, y, 16.0, 22.0), f(12.0).icon().c(), col);
            let mut right = row.r();
            right -= 22.0;
            s.hit(a11y_key(c.key, 1), Rect::new(right, y, 22.0, 22.0));
            if icon_button(ui, g, t, id(1), Rect::new(right, y, 22.0, 22.0), G_X, true) {
                act = Some(Act::Dismiss(c.key));
            }
            if c.can_undo() {
                let uw = g.measure("Undo", t.font()) + 8.0;
                right -= uw + 4.0;
                s.hit(a11y_key(c.key, 2), Rect::new(right, y, uw, 22.0));
                if link(ui, g, t, id(2), right, y, 22.0, "Undo").0 {
                    act = Some(Act::Undo(c.key));
                }
            }
            s.a.clear();
            s.b.clear();
            let msg_w = right - 6.0 - (x + 22.0);
            if c.state == State::Done {
                let _ = write!(s.b, " {} {}", c.count, crate::ops::plural(c.count));
                if c.kind == Kind::Delete || c.to.is_empty() || c.history {
                    rich_line(g, t, x + 22.0, y, msg_w, 22.0, &[(c.done_verb, true), (&s.b, false)], t.font_size_sm);
                } else {
                    s.b.push_str(" to ");
                    rich_line(g, t, x + 22.0, y, msg_w, 22.0, &[(c.done_verb, true), (&s.b, false), (&c.to, true)], t.font_size_sm);
                }
            } else {
                let total = c.totals.files_total.max(c.done_at_cancel);
                let _ = write!(s.b, " after {} of {} {}", c.done_at_cancel, total, if total == 1 { "item" } else { "items" });
                rich_line(g, t, x + 22.0, y, msg_w, 22.0, &[("Cancelled", true), (&s.b, false)], t.font_size_sm);
            }
            // countdown: the remaining fraction at paint time (no animation of its own)
            let left = match (c.held, c.dismiss_at) {
                (Some(l), _) => l,
                (None, Some(at)) => at.saturating_duration_since(now),
                _ => Duration::ZERO,
            };
            let frac = (left.as_secs_f32() / DISMISS.as_secs_f32()).clamp(0.0, 1.0);
            g.fill(Rect::new(cr.x, cr.b() - 2.0, cr.w * frac, 2.0), t.ok);
        }
        State::Failed => {
            g.text(G_ALERT, Rect::new(x, y, 16.0, 22.0), f(12.0).icon().c(), t.danger);
            let right = r.r() - 22.0;
            s.hit(a11y_key(c.key, 1), Rect::new(right, y, 22.0, 22.0));
            if icon_button(ui, g, t, id(1), Rect::new(right, y, 22.0, 22.0), G_X, true) {
                act = Some(Act::Dismiss(c.key));
            }
            let nfail = c.errors.len();
            s.a.clear();
            s.b.clear();
            let _ = write!(s.a, " {} of {} {}, ", c.count.saturating_sub(nfail), c.count, crate::ops::plural(c.count));
            let _ = write!(s.b, "{nfail} failed");
            let mw = right - 6.0 - (x + 22.0);
            let font = t.font();
            let vw = g.measure(c.done_verb, t.font_bold()).min(mw);
            g.text(c.done_verb, Rect::new(x + 22.0, y, vw + 1.0, 22.0), t.font_bold(), t.text);
            let aw = g.measure(&s.a, font).min((mw - vw).max(0.0));
            g.text(&s.a, Rect::new(x + 22.0 + vw, y, aw + 1.0, 22.0), font, t.text);
            g.text(&s.b, Rect::new(x + 22.0 + vw + aw, y, (mw - vw - aw).max(0.0), 22.0), font, t.danger);
            y += 22.0;
            if !c.from.is_empty() {
                if c.to.is_empty() {
                    rich_line(g, t, x + 22.0, y, r.w - 22.0, 18.0, &[("from ", false), (&c.from, true)], t.font_size_sm);
                } else {
                    rich_line(g, t, x + 22.0, y, r.w - 22.0, 18.0, &[("from ", false), (&c.from, true), (" to ", false), (&c.to, true)], t.font_size_sm);
                }
            }
            y += 18.0 + 6.0;
            let label = if c.details { "Hide details" } else { "Show details" };
            s.hit(a11y_key(c.key, 3), Rect::new(x + 18.0, y, 90.0, 24.0));
            if link(ui, g, t, id(3), x + 18.0, y, 24.0, label).0 {
                act = Some(Act::Details(c.key));
            }
            let skip_w = text_button_w(g, t, "Skip", false, false);
            let retry_w = text_button_w(g, t, "Retry", true, true);
            let sk = Rect::new(r.r() - skip_w, y, skip_w, 24.0);
            let rt = Rect::new(sk.x - 6.0 - retry_w, y, retry_w, 24.0);
            s.hit(a11y_key(c.key, 4), rt);
            s.hit(a11y_key(c.key, 5), sk);
            if !c.failed.is_empty() && text_button(ui, g, t, id(4), rt, "Retry", Some(G_RETRY), true) {
                act = Some(Act::Retry(c.key));
            }
            if text_button(ui, g, t, id(5), sk, "Skip", None, false) {
                act = Some(Act::Dismiss(c.key));
            }
            y += 24.0;
            if c.details {
                y += 6.0;
                g.fill(Rect::new(x + 22.0, y, r.w - 22.0, 1.0), t.border);
                y += 4.0;
                for (p, why) in &c.errors {
                    let name = p.file_name().map(|n| n.to_string_lossy()).unwrap_or_else(|| p.to_string_lossy());
                    g.text(&name, Rect::new(x + 22.0, y + 2.0, r.w - 22.0, 16.0), t.font(), t.text);
                    g.text(why, Rect::new(x + 22.0, y + 18.0, r.w - 22.0, 16.0), small, t.danger);
                    y += 36.0;
                }
            }
        }
        _ => {
            let waiting = c.state == State::Waiting;
            let paused = c.state == State::Paused;
            let asking = c.state == State::Asking;
            g.text(c.kind.glyph(), Rect::new(x, y, 16.0, 22.0), f(12.0).icon().c(), t.accent);
            let vf = t.font_md().b();
            let vw = g.measure(c.verb, vf);
            g.text(c.verb, Rect::new(x + 22.0, y, vw + 2.0, 22.0), vf, t.text);
            if !waiting {
                s.a.clear();
                let _ = write!(s.a, "{}%", c.percent());
                g.text(&s.a, Rect::new(x + 22.0 + vw + 6.0, y, 40.0, 22.0), small.left(), t.text_dim);
            }
            let mut bx = r.r() - 22.0;
            s.hit(a11y_key(c.key, 1), Rect::new(bx, y, 22.0, 22.0));
            if icon_button(ui, g, t, id(1), Rect::new(bx, y, 22.0, 22.0), G_X, true) {
                act = Some(Act::Cancel(c.key));
            }
            if !waiting {
                bx -= 28.0;
                s.hit(a11y_key(c.key, 2), Rect::new(bx, y, 22.0, 22.0));
                if icon_button(ui, g, t, id(2), Rect::new(bx, y, 22.0, 22.0), if c.expanded { G_UP } else { G_DOWN }, false) {
                    act = Some(Act::Expand(c.key));
                }
                if !asking && matches!(c.stop, Stop::Queue(_)) {
                    bx -= 28.0;
                    s.hit(a11y_key(c.key, 3), Rect::new(bx, y, 22.0, 22.0));
                    if icon_button(ui, g, t, id(3), Rect::new(bx, y, 22.0, 22.0), if paused { G_PLAY } else { G_PAUSE }, false) {
                        act = Some(if paused { Act::Resume(c.key) } else { Act::Pause(c.key) });
                    }
                }
            }
            y += 22.0;
            // "N items from A to B"
            s.b.clear();
            let n = c.count.max(c.totals.files_total as usize);
            let _ = write!(s.b, "{n} {} from ", crate::ops::plural(n));
            let line = Rect::new(x, y, r.w, 18.0);
            if n == 0 {
                s.b.clear();
                s.b.push_str("From ");
            }
            if c.from.is_empty() && c.to.is_empty() {
                s.b.truncate(s.b.len().saturating_sub(" from ".len()));
                rich_line(g, t, x, y, line.w, 18.0, &[(&s.b, false)], t.font_size_sm);
            } else if c.kind == Kind::Delete {
                rich_line(g, t, x, y, line.w, 18.0, &[(&s.b, false), (&c.from, true), (" to Recycle Bin", false)], t.font_size_sm);
            } else {
                rich_line(g, t, x, y, line.w, 18.0, &[(&s.b, false), (&c.from, true), (" to ", false), (&c.to, true)], t.font_size_sm);
            }
            y += 18.0 + 5.0;
            let bar = Rect::new(x, y, r.w, 4.0);
            g.round(bar, 2.0, t.border);
            if !waiting {
                let w = (bar.w * c.fraction()).round();
                if w > 0.0 {
                    g.round(Rect::new(bar.x, bar.y, w, 4.0), 2.0, if paused { t.text_dim } else { t.accent });
                }
            }
            y += 4.0 + 6.0;
            let line = Rect::new(x, y, r.w, 16.0);
            if waiting {
                g.text("Waiting", line, small.left(), t.text_dim);
                total_text(c, &mut s.a);
                let w = g.measure(&s.a, small) + 2.0;
                g.text(&s.a, Rect::new(line.r() - w, y, w, 16.0), small, t.text_dim);
            } else {
                done_text(c, &mut s.a);
                let dw = g.measure(&s.a, small) + 2.0;
                g.text(&s.a, Rect::new(x, y, dw, 16.0), small.left(), t.text_dim);
                if asking {
                    let af = small.b();
                    let w = g.measure("Waiting for your answer", af) + 2.0;
                    g.text("Waiting for your answer", Rect::new(line.r() - w, y, w, 16.0), af, t.accent);
                } else if paused {
                    let w = g.measure("Paused", small) + 2.0;
                    g.text("Paused", Rect::new(line.r() - w, y, w, 16.0), small, t.text_dim);
                } else {
                    speed_text(c, &mut s.b, &mut s.c);
                    let w = g.measure(&s.b, small) + 2.0;
                    g.text(&s.b, Rect::new(line.r() - w, y, w, 16.0), small, t.text_dim);
                    let sx = x + dw + 8.0;
                    if sx + 72.0 < line.r() - w - 8.0 {
                        spark(g, t, Rect::new(sx, y, 72.0, 16.0), &c.speed.ring, paused);
                    }
                }
            }
            y += 16.0;
            if c.expanded && !waiting {
                y += 6.0;
                g.fill(Rect::new(x, y, r.w, 1.0), t.border);
                y += 5.0;
                for (k, name) in c.files.iter().enumerate() {
                    if name.is_empty() && k > 0 {
                        continue;
                    }
                    let col = if k == 0 { t.text } else { t.text_dim };
                    g.text(G_FILE, Rect::new(x, y, 16.0, 22.0), f(11.0).icon().c(), t.text_dim);
                    let mut nr = r.r();
                    if k == 0 && c.item_total > 0 {
                        s.a.clear();
                        data::fmt_size(c.item_total, &mut s.a);
                        let sw = g.measure(&s.a, small) + 2.0;
                        g.text(&s.a, Rect::new(nr - sw, y, sw, 22.0), small, t.text_dim);
                        nr -= sw + 6.0;
                        let mini = Rect::new(nr - 40.0, y + 9.5, 40.0, 3.0);
                        g.round(mini, 1.5, t.border);
                        let fr = (c.item_done as f32 / c.item_total as f32).clamp(0.0, 1.0);
                        if fr > 0.0 {
                            g.round(Rect::new(mini.x, mini.y, 40.0 * fr, 3.0), 1.5, t.accent);
                        }
                        nr -= 46.0;
                    }
                    g.text(if name.is_empty() { "\u{2026}" } else { name }, Rect::new(x + 22.0, y, (nr - x - 22.0).max(0.0), 22.0), t.font().left(), col);
                    y += 22.0;
                }
                let more = more_count(c);
                if more > 0 {
                    s.a.clear();
                    let _ = write!(s.a, "and {more} more");
                    g.text(&s.a, Rect::new(x, y, r.w, 22.0), small.left(), t.text_dim);
                }
            }
        }
    }
    act
}

/// "1.3 GB of 3.4 GB" or "88 of 212 items".
fn done_text(c: &Card, out: &mut String) {
    out.clear();
    let tt = c.totals;
    if c.counts_items() {
        let _ = write!(out, "{} of {} items", tt.files_done, tt.files_total.max(tt.files_done));
    } else {
        let mut a = String::new();
        let mut b = String::new();
        data::fmt_size(tt.bytes_done, &mut a);
        data::fmt_size(tt.bytes_total.max(tt.bytes_done), &mut b);
        let _ = write!(out, "{a} of {b}");
    }
}

fn total_text(c: &Card, out: &mut String) {
    out.clear();
    let tt = c.totals;
    if tt.bytes_total > 0 && !c.counts_items() {
        data::fmt_size(tt.bytes_total, out);
    } else {
        let n = c.count.max(tt.files_total as usize);
        let _ = write!(out, "{n} {}", crate::ops::plural(n));
    }
}

/// "88 MB/s · 0:24 left" or "46 items/s · 0:04 left".
fn speed_text(c: &Card, out: &mut String, tmp: &mut String) {
    out.clear();
    if c.speed.ema <= 0.0 {
        out.push('\u{2026}');
        return;
    }
    if c.counts_items() {
        let _ = write!(out, "{} items/s", c.speed.ema.round() as u64);
    } else {
        data::fmt_size(c.speed.ema as u64, tmp);
        let _ = write!(out, "{tmp}/s");
    }
    eta_text(c.speed.eta(c.left_units()), tmp);
    let _ = write!(out, " \u{b7} {tmp} left");
}

/// The 10 s speed sparkline: a base line and the samples as a polyline.
fn spark(g: &mut dyn Canvas, t: &Theme, r: Rect, ring: &Ring, paused: bool) {
    g.line(r.x, r.b() - 0.5, r.r(), r.b() - 0.5, t.border, 1.0);
    let n = ring.len();
    if n < 2 {
        return;
    }
    let max = ring.max().max(1.0) * 1.1;
    let col = if paused { t.text_disabled } else { t.accent };
    // the newest sample sits at the right edge; fewer than RING samples start later
    let step = (r.w - 2.0) / (RING - 1) as f32;
    let x0 = r.x + 1.0 + step * (RING - n) as f32;
    let py = |v: f32| r.b() - 2.0 - (v / max) * (r.h - 4.0);
    let mut px = x0;
    let mut prev = py(ring.get(0));
    for i in 1..n {
        let y = py(ring.get(i));
        g.line(px, prev, px + step, y, col, 1.4);
        px += step;
        prev = y;
    }
}

// ---- the pill -------------------------------------------------------------------------

/// The pill's width in the status bar (0: none).
pub fn pill_width(panel: &Panel, g: &mut dyn Canvas, t: &Theme) -> f32 {
    if !panel.pill_visible() {
        return 0.0;
    }
    let mut s = String::new();
    let icon = panel.pill_text(&mut s);
    let lead = if icon == PillIcon::Bar { 36.0 } else { 14.0 };
    6.0 + lead + 6.0 + g.measure(&s, t.font_small()) + 6.0 + 12.0 + 8.0
}

/// Paints the pill right-aligned at `right` in the status bar `r`. True when clicked.
pub fn paint_pill(panel: &Panel, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, r: Rect, right: f32) -> bool {
    let w = pill_width(panel, g, t);
    if w <= 0.0 {
        return false;
    }
    let pr = Rect::new(right - w, r.y + (r.h - 20.0) / 2.0 + 0.5, w, 20.0);
    let it = ui.interact(ID_PILL, pr, Sense::click().focusable());
    let mut clicked = it.clicked == Some(Button::Left);
    for (qi, ev) in ui.key_input(ID_PILL) {
        if let tsubame_ui::Event::KeyDown { key: tsubame_ui::Key::Enter | tsubame_ui::Key::Space, mods, .. } = ev
            && mods.none()
        {
            ui.consume(qi);
            clicked = true;
        }
    }
    g.round(pr, 10.0, if it.hovered { t.hover } else { t.popup });
    g.round_stroke(pr, 10.0, if it.focused && ui.window_focused() { t.focus } else { t.border }, 1.0);
    let mut s = String::new();
    let icon = panel.pill_text(&mut s);
    let mut x = pr.x + 6.0;
    match icon {
        PillIcon::Bar => {
            let mini = Rect::new(x, pr.y + 8.0, 36.0, 4.0);
            g.round(mini, 2.0, t.border);
            let fw = 36.0 * panel.aggregate_percent() as f32 / 100.0;
            if fw > 0.0 {
                g.round(Rect::new(x, mini.y, fw, 4.0), 2.0, t.accent);
            }
            x += 36.0;
        }
        PillIcon::Done => {
            g.text(G_CHECK, Rect::new(x, pr.y, 14.0, 20.0), f(10.0).icon().c(), t.ok);
            x += 14.0;
        }
        PillIcon::Failed => {
            g.text(G_ALERT, Rect::new(x, pr.y, 14.0, 20.0), f(10.0).icon().c(), t.danger);
            x += 14.0;
        }
    }
    x += 6.0;
    let sw = g.measure(&s, t.font_small()) + 2.0;
    g.text(&s, Rect::new(x, pr.y, sw, 20.0), t.font_small().left(), t.text);
    g.text(G_UP, Rect::new(pr.r() - 20.0, pr.y, 12.0, 20.0), f(9.0).icon().c(), t.text_dim);
    if crate::uia_rows::on() {
        crate::uia_rows::buttons(
            crate::uia_rows::PART_OPS,
            "Operations",
            &s,
            pr,
            vec![(A11Y_OPEN, "Show operations".into(), pr)],
        );
    }
    if clicked {
        ui.request_repaint();
    }
    clicked
}

// ---- UI Automation ---------------------------------------------------------------------

const A11Y_OPEN: u64 = 1;
const A11Y_COLLAPSE: u64 = 2;
const A11Y_CLEAR: u64 = 3;

/// The button `n` of card `key` as a UIA item key (1..3 are the header's).
fn a11y_key(key: Key, n: u64) -> u64 {
    16 + key.code() * 16 + n
}

/// Every panel button as an Invokable item with a name (docs/uia-views.md).
fn publish_uia(panel: &Panel, pr: Rect, order: &[usize], rects: &[(u64, Rect)]) {
    let pr_of = |k: u64| rects.iter().find(|(x, _)| *x == k).map(|(_, r)| *r).unwrap_or(pr);
    let mut items: Vec<(u64, String, Rect)> = vec![(A11Y_COLLAPSE, "Collapse operations".into(), pr_of(A11Y_COLLAPSE))];
    if panel.cards.iter().any(|c| !c.state.active()) {
        items.push((A11Y_CLEAR, "Clear finished".into(), pr_of(A11Y_CLEAR)));
    }
    for &i in order {
        let c = &panel.cards[i];
        let what = format!("{} {} {}", c.verb, c.count, crate::ops::plural(c.count));
        match c.state {
            State::Done | State::Cancelled => {
                if c.can_undo() {
                    items.push((a11y_key(c.key, 2), format!("Undo: {what}"), pr_of(a11y_key(c.key, 2))));
                }
                items.push((a11y_key(c.key, 1), format!("Dismiss: {what}"), pr_of(a11y_key(c.key, 1))));
            }
            State::Failed => {
                items.push((a11y_key(c.key, 3), format!("{}: {what}, {} failed", if c.details { "Hide details" } else { "Show details" }, c.errors.len()), pr_of(a11y_key(c.key, 3))));
                if !c.failed.is_empty() {
                    items.push((a11y_key(c.key, 4), format!("Retry: {what}"), pr_of(a11y_key(c.key, 4))));
                }
                items.push((a11y_key(c.key, 5), format!("Skip: {what}"), pr_of(a11y_key(c.key, 5))));
            }
            State::Waiting => items.push((a11y_key(c.key, 1), format!("Cancel: {what}, waiting"), pr)),
            s => {
                if matches!(c.stop, Stop::Queue(_)) && s != State::Asking {
                    items.push((a11y_key(c.key, 3), format!("{}: {what}, {}%", if s == State::Paused { "Resume" } else { "Pause" }, c.percent()), pr_of(a11y_key(c.key, 3))));
                }
                items.push((a11y_key(c.key, 2), format!("{}: {what}", if c.expanded { "Hide files" } else { "Show files" }), pr_of(a11y_key(c.key, 2))));
                items.push((a11y_key(c.key, 1), format!("Cancel: {what}, {}%", c.percent()), pr_of(a11y_key(c.key, 1))));
            }
        }
    }
    let n = panel.cards.iter().filter(|c| c.state.active()).count();
    crate::uia_rows::buttons(crate::uia_rows::PART_OPS, "Operations", &format!("{n} running"), pr, items);
}

/// What a screen reader's Invoke on item `k` means now.
pub fn a11y_act(panel: &Panel, k: u64) -> Option<Act> {
    match k {
        A11Y_OPEN => return Some(Act::Open),
        A11Y_COLLAPSE => return Some(Act::Collapse),
        A11Y_CLEAR => return Some(Act::ClearFinished),
        _ => {}
    }
    let c = panel.cards.iter().find(|c| (16..).contains(&k) && (k - 16) / 16 == c.key.code())?;
    let n = (k - 16) % 16;
    Some(match (c.state, n) {
        (State::Done | State::Cancelled, 1) | (State::Failed, 1 | 5) => Act::Dismiss(c.key),
        (State::Done | State::Cancelled, 2) => Act::Undo(c.key),
        (State::Failed, 3) => Act::Details(c.key),
        (State::Failed, 4) => Act::Retry(c.key),
        (s, 1) if s.active() => Act::Cancel(c.key),
        (s, 2) if s.active() => Act::Expand(c.key),
        (State::Paused, 3) => Act::Resume(c.key),
        (State::Running, 3) => Act::Pause(c.key),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }
    fn tot(done: u64, total: u64) -> Totals {
        Totals { files_done: 0, files_total: 1, bytes_done: done, bytes_total: total }
    }

    #[test]
    fn ring_keeps_the_last_forty_oldest_first() {
        let mut r = Ring::default();
        assert!(r.is_empty());
        for i in 0..50 {
            r.push(i as f32);
        }
        assert_eq!(r.len(), RING);
        assert_eq!(r.get(0), 10.0);
        assert_eq!(r.get(RING - 1), 49.0);
        assert_eq!(r.max(), 49.0);
        let mut s = Ring::default();
        s.push(3.0);
        s.push(1.0);
        assert_eq!((s.len(), s.get(0), s.get(1)), (2, 3.0, 1.0));
    }

    #[test]
    fn speed_is_an_ema_of_deltas_at_four_samples_a_second() {
        let t = t0();
        let mut s = Speed::default();
        s.sample(t, 0);
        s.sample(t + Duration::from_millis(100), 50); // too soon: no sample
        assert_eq!(s.ring.len(), 0);
        s.sample(t + Duration::from_millis(250), 250); // 1000 /s
        assert!((s.ema - 1000.0).abs() < 1e-6);
        s.sample(t + Duration::from_millis(500), 750); // 2000 /s
        assert!((s.ema - 1300.0).abs() < 1e-6, "{}", s.ema);
        assert_eq!(s.ring.len(), 2);
        assert_eq!(s.eta(1300).map(|d| d.as_millis()), Some(1000));
        // a pause of 10 s does not count as slow
        s.pause();
        s.sample(t + Duration::from_secs(10), 750);
        s.sample(t + Duration::from_millis(10_250), 1250); // 2000 /s again
        assert!(s.ema > 1300.0, "{}", s.ema);
    }

    #[test]
    fn eta_texts() {
        let mut s = String::new();
        eta_text(None, &mut s);
        assert_eq!(s, "\u{2026}");
        eta_text(Some(Duration::from_millis(900)), &mut s);
        assert_eq!(s, "a moment");
        eta_text(Some(Duration::from_secs(42)), &mut s);
        assert_eq!(s, "0:42");
        eta_text(Some(Duration::from_secs(3723)), &mut s);
        assert_eq!(s, "1:02:03");
    }

    #[test]
    fn panel_opens_after_the_delay_and_quick_jobs_never_show() {
        let t = t0();
        let mut p = Panel::default();
        p.add(Card::new(Key::Queue(1), "Copying", "Copied", 1));
        p.started(Key::Queue(1), t);
        assert!(!p.tick(t + Duration::from_millis(200)));
        assert!(!p.open);
        // quick: done before 400 ms, the card goes silently
        p.finished(Key::Queue(1), t + Duration::from_millis(300), false, tot(10, 10));
        assert!(p.cards.is_empty());
        p.tick(t + Duration::from_millis(500));
        assert!(!p.open);
        // a slow one opens the panel at 400 ms
        p.add(Card::new(Key::Queue(2), "Copying", "Copied", 3));
        p.started(Key::Queue(2), t);
        assert!(p.tick(t + OPEN_DELAY));
        assert!(p.open && p.cards[0].shown);
    }

    #[test]
    fn finished_cards_dismiss_after_five_seconds_and_hover_holds() {
        let t = t0();
        let mut p = Panel::default();
        p.add(Card::new(Key::Queue(1), "Copying", "Copied", 1));
        p.started(Key::Queue(1), t);
        p.tick(t + OPEN_DELAY);
        let end = t + Duration::from_secs(1);
        p.finished(Key::Queue(1), end, false, tot(10, 10));
        assert_eq!(p.cards[0].state, State::Done);
        assert_eq!(p.next_dismiss(), Some(end + DISMISS));
        // hovering at +2 s holds 3 s
        p.hover(Some(Key::Queue(1)), end + Duration::from_secs(2));
        assert_eq!(p.next_dismiss(), None, "no timer while held");
        p.tick(end + Duration::from_secs(20));
        assert_eq!(p.cards.len(), 1);
        // leaving at +20 s: 3 s left
        p.hover(None, end + Duration::from_secs(20));
        assert_eq!(p.next_dismiss(), Some(end + Duration::from_secs(23)));
        p.tick(end + Duration::from_secs(23));
        assert!(p.cards.is_empty());
        assert!(!p.open, "the panel closes with its last card");
        assert_eq!(p.next_dismiss(), None);
    }

    #[test]
    fn errors_never_auto_dismiss_and_open_the_panel() {
        let t = t0();
        let mut p = Panel::default();
        p.add(Card::new(Key::Queue(1), "Copying", "Copied", 3));
        p.started(Key::Queue(1), t);
        p.error(Key::Queue(1), "C:\\a\\x.txt".into(), "Access denied".into(), Some(Operation::NewFile("x".into())));
        p.finished(Key::Queue(1), t + Duration::from_millis(100), false, tot(10, 10));
        assert_eq!(p.cards[0].state, State::Failed);
        assert!(p.open, "a quick failure still shows");
        assert_eq!(p.next_dismiss(), None);
        p.tick(t + Duration::from_secs(60));
        assert_eq!(p.cards.len(), 1);
        p.clear_finished();
        assert!(p.cards.is_empty());
    }

    #[test]
    fn cancel_of_a_waiting_card_removes_it_and_of_a_running_one_counts() {
        let t = t0();
        let mut p = Panel::default();
        p.add(Card::new(Key::Queue(1), "Copying", "Copied", 14));
        p.add(Card::new(Key::Queue(2), "Moving", "Moved", 3));
        p.started(Key::Queue(1), t);
        p.tick(t + OPEN_DELAY);
        assert_eq!(p.cards[1].state, State::Waiting);
        p.cancelled(Key::Queue(2), t + OPEN_DELAY);
        assert!(p.card(Key::Queue(2)).is_none());
        // its Finished (never started) changes nothing
        p.finished(Key::Queue(2), t + OPEN_DELAY, true, Totals::default());
        p.progress(Key::Queue(1), t + OPEN_DELAY, Totals { files_done: 6, files_total: 14, bytes_done: 60, bytes_total: 140 }, 6, Path::new("C:\\a\\f6"), 0, 0);
        p.cancelled(Key::Queue(1), t + Duration::from_secs(1));
        let c = p.card(Key::Queue(1)).unwrap();
        assert_eq!((c.state, c.done_at_cancel), (State::Cancelled, 6));
        p.finished(Key::Queue(1), t + Duration::from_secs(1), true, Totals { files_done: 7, files_total: 14, bytes_done: 70, bytes_total: 140 });
        assert_eq!(p.card(Key::Queue(1)).unwrap().state, State::Cancelled);
        assert!(p.next_dismiss().is_some());
    }

    #[test]
    fn pause_and_asking_move_between_states() {
        let t = t0();
        let mut p = Panel::default();
        p.add(Card::new(Key::Queue(1), "Copying", "Copied", 1));
        p.started(Key::Queue(1), t);
        p.set_paused(Key::Queue(1), true);
        assert_eq!(p.cards[0].state, State::Paused);
        p.set_asking(Key::Queue(1), true); // paused: no change
        assert_eq!(p.cards[0].state, State::Paused);
        p.set_paused(Key::Queue(1), false);
        p.set_asking(Key::Queue(1), true);
        assert_eq!(p.cards[0].state, State::Asking);
        p.set_asking(Key::Queue(1), false);
        assert_eq!(p.cards[0].state, State::Running);
    }

    #[test]
    fn order_puts_finished_on_top_and_the_pill_aggregates() {
        let t = t0();
        let mut p = Panel::default();
        for i in 1..=3 {
            p.add(Card::new(Key::Queue(i), "Copying", "Copied", 1));
        }
        p.started(Key::Queue(1), t);
        p.tick(t + OPEN_DELAY);
        p.finished(Key::Queue(1), t + OPEN_DELAY, false, tot(10, 10));
        p.started(Key::Queue(2), t + OPEN_DELAY);
        p.progress(Key::Queue(2), t + OPEN_DELAY, tot(50, 100), 0, Path::new("x"), 0, 0);
        assert_eq!(p.order(), vec![0, 1, 2]);
        let mut s = String::new();
        p.collapse();
        assert!(p.pill_visible());
        assert_eq!(p.pill_text(&mut s), PillIcon::Bar);
        assert_eq!(s, "2 operations, 25%");
        p.finished(Key::Queue(2), t + OPEN_DELAY, false, tot(100, 100));
        p.cancelled(Key::Queue(3), t + OPEN_DELAY);
        assert_eq!(p.pill_text(&mut s), PillIcon::Done);
        assert_eq!(s, "Done");
    }

    #[test]
    fn delete_counts_items_and_next_names_follow_the_index() {
        let mut c = Card::new(Key::Queue(1), "Deleting", "Deleted", 5);
        c.names = Arc::from(vec!["a".to_string(), "b".into(), "c".into(), "d".into(), "e".into()]);
        c.totals = Totals { files_done: 2, files_total: 5, bytes_done: 0, bytes_total: 0 };
        assert!(c.counts_items());
        assert_eq!(c.percent(), 40);
        c.set_current(1, Path::new("C:\\x\\b"));
        assert_eq!(c.files, ["b".to_string(), "c".into(), "d".into()]);
        assert_eq!(more_count(&c), 1);
        let mut s = String::new();
        done_text(&c, &mut s);
        assert_eq!(s, "2 of 5 items");
    }

    #[test]
    fn a11y_keys_map_to_the_buttons_of_each_state() {
        let t = t0();
        let mut p = Panel::default();
        let mut c = Card::new(Key::Queue(7), "Copying", "Copied", 2);
        c.stop = Stop::Queue(Cancellation::default());
        p.add(c);
        p.started(Key::Queue(7), t);
        let k = Key::Queue(7);
        assert_eq!(a11y_act(&p, a11y_key(k, 1)), Some(Act::Cancel(k)));
        assert_eq!(a11y_act(&p, a11y_key(k, 3)), Some(Act::Pause(k)));
        p.set_paused(k, true);
        assert_eq!(a11y_act(&p, a11y_key(k, 3)), Some(Act::Resume(k)));
        p.tick(t + OPEN_DELAY);
        p.finished(k, t + OPEN_DELAY, false, tot(1, 1));
        assert_eq!(a11y_act(&p, a11y_key(k, 2)), Some(Act::Undo(k)));
        assert_eq!(a11y_act(&p, A11Y_COLLAPSE), Some(Act::Collapse));
        assert_eq!(a11y_act(&p, a11y_key(Key::Extract(7), 1)), None);
    }

    #[test]
    fn folder_names_for_from_and_to() {
        assert_eq!(folder_name("C:\\Users\\me\\Downloads"), "Downloads");
        assert_eq!(folder_name("D:\\"), "D:");
        let c = Card::new(Key::Queue(1), "Copying", "Copied", 1).with_folders("C:\\a\\Downloads", "X:\\Dev\\Projects\\");
        assert_eq!((c.from.as_str(), c.to.as_str()), ("Downloads", "Projects"));
    }


    fn frame(p: &mut Panel, now: Instant) -> (tsubame_ui::TestCanvas, Option<Act>) {
        let mut ui = Ui::new(tsubame_ui::DARK);
        let mut g = tsubame_ui::TestCanvas::new(1200.0, 900.0);
        ui.begin_frame((1200.0, 900.0));
        let a = paint(p, &mut ui, &mut g, Rect::new(0.0, 60.0, 1200.0, 800.0), now);
        let _ = ui.end_frame();
        (g, a)
    }

    #[test]
    fn paints_every_state_with_the_mock_copy() {
        let t = t0();
        let mut p = Panel::default();
        let mk = |i: u64, verb: &'static str, done: &'static str| {
            let mut c = Card::new(Key::Queue(i), verb, done, 14).with_folders("C:\\a\\Downloads", "D:\\Projects");
            c.stop = Stop::Queue(Cancellation::default());
            c
        };
        for (i, v, d) in [(1, "Copying", "Copied"), (2, "Moving", "Moved"), (3, "Copying", "Copied"), (4, "Deleting", "Deleted"), (5, "Copying", "Copied"), (6, "Copying", "Copied")] {
            p.add(mk(i, v, d));
        }
        for i in 1..=6 {
            p.started(Key::Queue(i), t);
        }
        p.tick(t + OPEN_DELAY);
        p.progress(Key::Queue(1), t + OPEN_DELAY, Totals { files_done: 3, files_total: 14, bytes_done: 430, bytes_total: 1000 }, 3, Path::new("C:\\a\\f.bin"), 5, 10);
        p.set_paused(Key::Queue(2), true);
        p.set_asking(Key::Queue(3), true);
        p.finished(Key::Queue(4), t + OPEN_DELAY, false, tot(1, 1));
        p.cancelled(Key::Queue(5), t + OPEN_DELAY);
        p.error(Key::Queue(6), "C:\\a\\locked.txt".into(), "Access is denied.".into(), Some(Operation::NewFile("x".into())));
        p.finished(Key::Queue(6), t + OPEN_DELAY, false, tot(1, 1));
        p.cards.push({
            let mut c = mk(7, "Copying", "Copied");
            c.shown = true;
            c
        }); // a waiting one behind them
        let (g, a) = frame(&mut p, t + OPEN_DELAY);
        assert!(a.is_none());
        for want in ["Operations", "Copying", "43%", "Paused", "Waiting for your answer", "Waiting", "Deleted", "Cancelled", "Show details", "Retry", "Skip", "Clear finished", "Undo"] {
            assert!(g.texts().iter().any(|s| s.contains(want)), "missing {want:?} in {:?}", g.texts());
        }
        assert!(!p.dirty(), "a paint clears the dirty flags");
        // the countdown line is the ok colour
        assert!(g.ops.iter().any(|o| matches!(o, tsubame_ui::testcanvas::Op::Fill(_, c, _) if *c == tsubame_ui::DARK.ok)));
    }

    #[test]
    fn closed_panel_paints_nothing_and_the_pill_names_the_job() {
        let t = t0();
        let mut p = Panel::default();
        p.add(Card::new(Key::Queue(1), "Copying", "Copied", 2));
        p.started(Key::Queue(1), t);
        let (g, _) = frame(&mut p, t);
        assert!(g.ops.is_empty(), "nothing before the open delay");
        p.tick(t + OPEN_DELAY);
        p.collapse();
        let (g, _) = frame(&mut p, t + OPEN_DELAY);
        assert!(g.ops.is_empty(), "folded: only the pill shows");
        let mut s = String::new();
        assert_eq!(p.pill_text(&mut s), PillIcon::Bar);
        assert_eq!(s, "Copying, 0%");
        let mut ui = Ui::new(tsubame_ui::DARK);
        let mut g = tsubame_ui::TestCanvas::new(800.0, 30.0);
        ui.begin_frame((800.0, 30.0));
        assert!(pill_width(&p, &mut g, &tsubame_ui::DARK) > 0.0);
        paint_pill(&p, &mut ui, &mut g, &tsubame_ui::DARK, Rect::new(0.0, 0.0, 800.0, 30.0), 780.0);
        let _ = ui.end_frame();
        assert!(g.has_text("Copying, 0%"));
    }


    /// Rig showed "Un..." and "Show deta...": a text rect exactly as wide as
    /// the measured label ellipsises it. Links get room to spare.
    #[test]
    fn link_labels_are_never_squeezed() {
        let t = t0();
        let mut p = Panel::default();
        for i in 1..=2 {
            let mut c = Card::new(Key::Queue(i), "Copying", "Copied", 1).with_folders("C:\\a\\src", "C:\\a\\dst");
            c.stop = Stop::Queue(Cancellation::default());
            p.add(c);
            p.started(Key::Queue(i), t);
        }
        p.tick(t + OPEN_DELAY);
        p.finished(Key::Queue(1), t + OPEN_DELAY, false, tot(1, 1));
        p.error(Key::Queue(2), "C:\\a\\src\\small".into(), "in use".into(), Some(Operation::NewFile("x".into())));
        p.finished(Key::Queue(2), t + OPEN_DELAY, false, tot(1, 1));
        let (g, _) = frame(&mut p, t + OPEN_DELAY);
        let mut seen = 0;
        for o in &g.ops {
            if let tsubame_ui::testcanvas::Op::Text(s, r, font, _) = o
                && (s == "Undo" || s == "Show details")
            {
                assert!(r.w > tsubame_ui::TestCanvas::width_of(s, *font) + 1.0, "{s:?} squeezed into {}", r.w);
                seen += 1;
            }
        }
        assert_eq!(seen, 2, "{:?}", g.texts());
    }


    #[test]
    fn bench_card_opens_at_once_and_moves() {
        let t = t0();
        let mut p = Panel::default();
        p.bench_tick(t);
        assert!(p.open && p.cards.len() == 1 && p.cards[0].shown);
        let a = p.cards[0].totals.bytes_done;
        p.bench_tick(t + Duration::from_millis(500));
        assert_eq!(p.cards.len(), 1, "one card for the whole run");
        assert!(p.cards[0].totals.bytes_done > a && p.cards[0].dirty);
        for ms in [1000, 5000, 10_000] {
            p.bench_tick(t + Duration::from_millis(ms));
            let _ = frame(&mut p, t + Duration::from_millis(ms));
        }
    }
}
