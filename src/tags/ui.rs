//! The UI-thread half of P4.F6: the row cache (file -> tags, LRU of 4,096),
//! batched lookups for the visible page, the picker popup, the tag filter
//! results, the read-only banner and the status text. Nothing here touches
//! the disk; the worker (`worker.rs`) owns `tags.db`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tsubame_ui::{Button, Canvas, Cursor, Event, Id, Key, ListSource, ListState, Pt, Rect, RowState, Sense, TextInput, Theme, Ui, f, text_input};
use tsubame_win::enumerate::Wake;
use tsubame_win::uia;

use super::marks::{self, Mark};
use super::model::{self, Check, Edit, Shape, TagDef};
use super::worker::{Hit, Req, Resp, Row, Snapshot, Worker};
use super::store::Mode;

pub const ID_PICKER: Id = Id::new("tags.picker");
const ID_PK_FIELD: Id = Id::new("tags.picker.field");
const ID_PK_NAME: Id = Id::new("tags.picker.name");
const ID_PK_ROW: Id = Id::new("tags.picker.row");
const ID_PK_SW: Id = Id::new("tags.picker.swatch");
pub const ID_RESULTS: Id = Id::new("tags.results");
const ID_BANNER: Id = Id::new("tags.banner");
pub const ID_CHIP_X: Id = Id::new("tags.chip.x");

/// File-id -> tags rows the UI keeps (P4.F6 budget).
pub const CACHE_ROWS: usize = 4096;
/// Most files one picker edits.
pub const MAX_PICK: usize = 50_000;
/// How long a status message stays.
const MESSAGE_FOR: Duration = Duration::from_secs(6);

/// Hash of a UTF-16 name (the tab's file-id table checks entries with it).
pub fn name_hash16(units: &[u16]) -> u32 {
    units.iter().fold(0x811c_9dc5u32, |h, &u| (h ^ u as u32).wrapping_mul(0x0100_0193))
}

fn name_hash(name: &str) -> u32 {
    name.encode_utf16().fold(0x811c_9dc5u32, |h, u| (h ^ u as u32).wrapping_mul(0x0100_0193))
}

fn hash64(bytes: impl IntoIterator<Item = u8>, seed: u64) -> u64 {
    bytes.into_iter().fold(seed, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Cache key of a row: its folder, name and (when known) file id.
fn row_key(dir_hash: u64, name: &str, fid: u128) -> u64 {
    let h = hash64(name.bytes(), dir_hash);
    hash64(fid.to_le_bytes(), h) | 1
}

pub fn dir_hash(dir: &str) -> u64 {
    hash64(dir.bytes().map(|b| b.to_ascii_lowercase()), 0xcbf2_9ce4_8422_2325)
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') { format!("{dir}{name}") } else { format!("{dir}\\{name}") }
}

/// What the view should do after the picker or the results list.
#[derive(Debug, PartialEq)]
pub enum Act {
    None,
    /// open Settings › Tags
    Manage,
    /// open a folder
    Go(String),
    /// show a file in its folder
    Reveal { folder: String, name: String },
}

pub struct TagsUi {
    w: Option<Worker>,
    /// `tags.db` (empty: tags are off, as in tests and the bench)
    path: PathBuf,
    on: bool,
    failed: bool,
    wake: Option<Wake>,
    pub snap: Option<Snapshot>,
    cache: crate::lru::Lru<u64, u64>,
    pending: HashSet<u64>,
    want: HashMap<String, Vec<Row>>,
    pub progress: Option<(usize, usize)>,
    pub message: Option<(String, Instant)>,
    pub picker: Option<Picker>,
    pub results: Option<Results>,
    seq: u64,
    pub shapes: bool,
    /// the banner's Details are open
    pub details: bool,
    last_scroll: f64,
}

impl Default for TagsUi {
    fn default() -> Self {
        TagsUi::new()
    }
}

impl TagsUi {
    pub fn new() -> TagsUi {
        TagsUi {
            w: None,
            path: PathBuf::new(),
            on: false,
            failed: false,
            wake: None,
            snap: None,
            cache: crate::lru::Lru::new(CACHE_ROWS, CACHE_ROWS * 32),
            pending: HashSet::new(),
            want: HashMap::new(),
            progress: None,
            message: None,
            picker: None,
            results: None,
            seq: 0,
            shapes: true,
            details: false,
            last_scroll: 0.0,
        }
    }

    /// The extension state and where the store lives. Switching off drops
    /// the worker (it saves and exits): zero cost while disabled.
    pub fn configure(&mut self, path: PathBuf, enabled: bool, shapes: bool, wake: Option<Wake>) {
        self.shapes = shapes;
        let on = enabled && !path.as_os_str().is_empty();
        if self.on && (!on || path != self.path) {
            self.stop();
        }
        self.on = on;
        self.path = path;
        self.wake = wake;
        super::set_want_fids(on && self.has_items());
    }

    /// Tags are switched on (the store may not be open yet).
    pub fn on(&self) -> bool {
        self.on
    }

    fn stop(&mut self) {
        self.w = None;
        self.snap = None;
        self.cache = crate::lru::Lru::new(CACHE_ROWS, CACHE_ROWS * 32);
        self.pending.clear();
        self.want.clear();
        self.picker = None;
        self.results = None;
        self.progress = None;
        self.failed = false;
        super::set_want_fids(false);
    }

    /// Opens the store on first need (the worker loads it in the background).
    pub fn ensure(&mut self) {
        if self.on && self.w.is_none() && !self.failed {
            self.w = Worker::start(self.path.clone(), self.wake.clone());
            self.failed = self.w.is_none();
        }
    }

    pub fn has_items(&self) -> bool {
        self.snap.as_ref().is_some_and(|s| s.items > 0)
    }

    pub fn defs(&self) -> &[TagDef] {
        self.snap.as_ref().map_or(&[], |s| &s.defs)
    }

    pub fn writable(&self) -> bool {
        self.snap.as_ref().is_none_or(|s| s.mode == Mode::Rw)
    }

    pub fn read_only(&self) -> bool {
        self.on && !self.writable()
    }

    pub fn send(&mut self, r: Req) {
        self.ensure();
        if let Some(w) = &self.w {
            let _ = w.tx.send(r);
        }
    }

    pub fn say(&mut self, s: impl Into<String>) {
        self.message = Some((s.into(), Instant::now()));
    }

    /// Drains worker replies. True: something to repaint.
    pub fn pump(&mut self) -> bool {
        let Some(w) = &self.w else { return false };
        let mut got = Vec::new();
        while let Ok(r) = w.rx.try_recv() {
            got.push(r);
        }
        let any = !got.is_empty();
        for r in got {
            match r {
                Resp::State(s) => {
                    self.snap = Some(*s);
                    super::set_want_fids(self.has_items());
                }
                Resp::Masks(m) => {
                    for (ck, mask) in m {
                        self.pending.remove(&ck);
                        self.cache.insert(ck, mask, 32);
                        if let Some(p) = &mut self.picker {
                            p.got(ck, mask);
                        }
                    }
                }
                Resp::Changed => {
                    self.cache = crate::lru::Lru::new(CACHE_ROWS, CACHE_ROWS * 32);
                    self.pending.clear();
                    if let Some(r) = &self.results {
                        let (slot, pane) = (r.slot, r.pane);
                        self.query(slot, pane);
                    }
                }
                Resp::Results { seq, slot, hits, capped } => {
                    if let Some(r) = &mut self.results
                        && r.seq == seq
                        && r.slot == slot
                    {
                        r.hits = hits;
                        r.capped = capped;
                        r.loading = false;
                    }
                }
                Resp::Progress(p) => self.progress = p,
                Resp::Message(m) => self.say(m),
            }
        }
        if self.message.as_ref().is_some_and(|(_, at)| at.elapsed() > MESSAGE_FOR) {
            self.message = None;
        }
        any
    }

    /// The status message, until it times out.
    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().filter(|(_, at)| at.elapsed() <= MESSAGE_FOR).map(|(m, _)| m.as_str())
    }

    /// The list scrolled: the reconcile pauses.
    pub fn note_scroll(&mut self, scroll: f64) {
        if scroll != self.last_scroll {
            self.last_scroll = scroll;
            if let Some(w) = &self.w {
                w.scrolled();
            }
        }
    }

    /// A frame's helper for the rows of `dir` (`None`: nothing to draw, no
    /// lookups: tags off or the store holds no item).
    pub fn rows<'a>(&'a mut self, dir: &'a str, fids: &'a [(u128, u32)]) -> Option<RowTags<'a>> {
        if !self.on || !self.has_items() {
            return None;
        }
        let dh = dir_hash(dir);
        Some(RowTags { ui: self, dir, dh, fids, buf: String::new() })
    }

    /// The cached tags of a file (paths outside a listing: the picker).
    pub fn cached(&mut self, path: &str, fid: u128) -> Option<u64> {
        let (dir, name) = split(path);
        let ck = row_key(dir_hash(dir), name, fid);
        self.cache.get(&ck).copied()
    }

    /// Sends the lookups this frame collected (one batch per folder).
    pub fn flush(&mut self) {
        if self.want.is_empty() {
            return;
        }
        let want = std::mem::take(&mut self.want);
        for (dir, rows) in want {
            self.send(Req::Lookup { dir, rows });
        }
    }

    /// Runs a tag filter for pane `pane`.
    pub fn filter(&mut self, slot: u8, pane: usize) {
        if self.results.as_ref().is_some_and(|r| r.slot == slot) {
            self.results = None;
            return;
        }
        self.results = Some(Results { slot, pane, seq: 0, hits: Vec::new(), capped: false, loading: true, list: ListState::new(24.0) });
        self.query(slot, pane);
    }

    fn query(&mut self, slot: u8, pane: usize) {
        self.seq += 1;
        let seq = self.seq;
        if let Some(r) = &mut self.results {
            r.seq = seq;
            r.pane = pane;
            r.loading = r.hits.is_empty();
        }
        self.send(Req::Query { seq, slot });
    }

    pub fn filter_slot(&self) -> Option<u8> {
        self.results.as_ref().map(|r| r.slot)
    }

    /// Opens the picker for `files` at `at`.
    /// `new`: start in the New tag form (the sidebar's "+").
    pub fn open_picker(&mut self, ui: &mut Ui, files: Vec<(String, u128)>, at: Pt, new: bool) {
        if !self.on || (files.is_empty() && !new) {
            return;
        }
        self.ensure();
        let files: Vec<(String, u128)> = files.into_iter().take(MAX_PICK).collect();
        let title = if files.is_empty() {
            "New tag".to_string()
        } else if files.len() == 1 { format!("Tags for {}", split(&files[0].0).1) } else { format!("{} items selected", sep(files.len())) };
        let mut p = Picker { title, at, focus: 0, edit: Edit::default(), field: TextInput::default(), new: None, fresh: true, masks: vec![None; files.len()], by_ck: HashMap::new(), files: Vec::new(), a11y: false, a11y_inv: None };
        // what is cached answers at once; the rest is one lookup per folder
        let mut ask: HashMap<String, Vec<Row>> = HashMap::new();
        let none = !self.has_items() && self.snap.is_some();
        for (i, (path, fid)) in files.iter().enumerate() {
            let (dir, name) = split(path);
            let ck = row_key(dir_hash(dir), name, *fid);
            p.by_ck.insert(ck, i);
            match self.cache.get(&ck).copied() {
                Some(m) => p.masks[i] = Some(m),
                None if none => p.masks[i] = Some(0),
                None => ask.entry(dir.to_string()).or_default().push(Row { ck, path: path.clone(), fid: *fid }),
            }
        }
        p.files = files;
        for (dir, rows) in ask {
            self.send(Req::Lookup { dir, rows });
        }
        if new {
            let defs = self.defs().to_vec();
            p.start_new(&defs);
        }
        self.picker = Some(p);
        ui.open_popup(ID_PICKER);
        ui.request_repaint();
    }

    pub fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    /// A screen reader invoked a picker row (`uia::Req` key): the next paint acts
    /// on it as a click would. False: no picker is open.
    pub fn a11y_invoke(&mut self, key: u64) -> bool {
        match self.picker.as_mut() {
            Some(p) => {
                p.a11y_inv = Some(key);
                true
            }
            None => false,
        }
    }

    /// Paints the picker (when open) and runs what it asks.
    pub fn paint_picker(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bounds: Rect) -> Act {
        let Some(mut p) = self.picker.take() else { return Act::None };
        let defs = self.defs().to_vec();
        let ro = !self.writable();
        let out = p.paint(ui, g, bounds, &defs, self.shapes, ro);
        match out {
            PickOut::Open => {
                self.picker = Some(p);
                Act::None
            }
            PickOut::Cancel => {
                p.close_a11y();
                ui.capture_keys = false;
                Act::None
            }
            PickOut::Apply => {
                p.close_a11y();
                ui.capture_keys = false;
                if !p.edit.is_empty() && !ro {
                    let files = std::mem::take(&mut p.files);
                    let n = files.len();
                    self.send(Req::Apply { files, edit: p.edit });
                    self.say(format!("Tags updated on {} item{}", sep(n), if n == 1 { "" } else { "s" }));
                }
                Act::None
            }
            PickOut::Create { name, colour, shape } => {
                p.close_a11y();
                ui.capture_keys = false;
                let files = std::mem::take(&mut p.files);
                if !p.edit.is_empty() {
                    self.send(Req::Apply { files: files.clone(), edit: p.edit });
                }
                self.send(Req::Create { name, colour, shape, files });
                Act::None
            }
            PickOut::Manage => {
                p.close_a11y();
                ui.capture_keys = false;
                Act::Manage
            }
        }
    }

    /// The read-only banner (26 DIPs) above the list, when the store is
    /// locked or unwritable. Returns the height used.
    pub fn paint_banner(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) -> f32 {
        let Some(s) = &self.snap else { return 0.0 };
        // a second window of the same user (another process holds the lock) is normal: it reads
        // the tags and says so in the status bar, no banner
        if !self.on || (s.mode == Mode::Rw && s.note.is_none()) || (s.mode == Mode::Locked && s.note.is_none()) {
            return 0.0;
        }
        let h = 26.0;
        let b = Rect::new(r.x, r.y, r.w, h);
        g.fill(b, t.popup);
        g.fill(Rect::new(b.x, b.b() - 1.0, b.w, 1.0), t.popup_border);
        let font = t.font_small();
        let mut x = b.x + t.space_5;
        let warn = s.mode != Mode::Rw;
        g.text("\u{E7BA}", Rect::new(x, b.y, 14.0, h), f(12.0).icon().c(), if warn { t.danger } else { t.accent });
        x += 22.0;
        let text = match &s.mode {
            Mode::Locked => "Tags are read-only: tags.db is locked by another process. Changes will not be saved.".to_string(),
            Mode::Unwritable(_) => "Tags are read-only: tags.db cannot be written. Changes will not be saved.".to_string(),
            Mode::Rw => s.note.clone().unwrap_or_default(),
        };
        let detail = match &s.mode {
            Mode::Unwritable(e) => Some(e.clone()),
            _ => s.note.clone().filter(|_| warn),
        };
        let links_w = if warn { 110.0 } else { 70.0 };
        let shown = if self.details { detail.clone().map(|d| format!("{text}  {d}")).unwrap_or(text.clone()) } else { text.clone() };
        let tw = (b.r() - x - links_w - t.space_5).max(0.0);
        g.text(&shown, Rect::new(x, b.y, tw, h), font, t.text);
        let mw = g.measure(&shown, font).min(tw);
        x += mw + 8.0;
        let link = |g: &mut dyn Canvas, ui: &mut Ui, x: &mut f32, label: &str, n: u64| -> bool {
            let w = g.measure(label, font) + 2.0;
            let lr = Rect::new(*x, b.y + 4.0, w, h - 8.0);
            let it = ui.interact(ID_BANNER.with(n), lr, Sense::click().focusable().cursor(Cursor::Hand));
            g.text(label, Rect::new(*x, b.y, w, h), font, t.accent);
            g.fill(Rect::new(*x, b.y + h / 2.0 + 6.0, w - 2.0, 1.0), t.accent);
            if it.focused {
                g.round_stroke(lr.pad(-2.0), t.radius_sm, t.focus, 1.0);
            }
            *x += w + 8.0;
            it.clicked == Some(Button::Left) || (it.focused && ui.key_input(ID_BANNER.with(n)).iter().any(|(_, e)| matches!(e, Event::KeyDown { key: Key::Enter | Key::Space, .. })))
        };
        if warn && link(g, ui, &mut x, "Retry", 1) {
            self.send(Req::Retry);
            ui.request_repaint();
        }
        if detail.is_some() && link(g, ui, &mut x, if self.details { "Hide" } else { "Details" }, 2) {
            self.details = !self.details;
            ui.request_repaint();
        }
        if !warn && link(g, ui, &mut x, "Dismiss", 3) {
            if let Some(s) = &mut self.snap {
                s.note = None;
            }
            ui.request_repaint();
        }
        h
    }

    /// The 2 DIP progress line of the reconcile, at the top of `r`.
    pub fn paint_progress(&self, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        if let Some((done, total)) = self.progress
            && total > 0
        {
            let w = r.w * (done as f32 / total as f32).clamp(0.02, 1.0);
            g.fill(Rect::new(r.x, r.y, w, t.height_progress_line), t.accent);
        }
    }

    /// What Settings > Tags shows (a copy of the last snapshot).
    pub fn page_cx(&self, config_path: &std::path::Path) -> crate::settings::tags_page::TagsCx {
        let s = self.snap.as_ref();
        crate::settings::tags_page::TagsCx {
            on: self.on,
            writable: self.writable(),
            defs: s.map(|s| s.defs.clone()).unwrap_or_default(),
            counts: s.map(|s| s.counts.to_vec()).unwrap_or_default(),
            items: s.map_or(0, |s| s.items),
            usage: s.map_or(0.0, |s| s.usage),
            note: s.and_then(|s| s.note.clone()),
            json: config_path.with_file_name("tags-export.json"),
            reqs: Vec::new(),
        }
    }

    /// Status-bar text for tags, when it has something to say.
    pub fn status(&self) -> Option<(String, bool)> {
        if !self.on {
            return None;
        }
        if let Some(m) = self.message() {
            return Some((m.to_string(), false));
        }
        if self.read_only() {
            return Some((if self.snap.as_ref().is_some_and(|s| s.mode == Mode::Locked) { "Tags read-only: open in another window" } else { "Tags read-only" }.into(), true));
        }
        if let Some((done, total)) = self.progress {
            return Some((format!("Tags: reconciling {} / {} · background, pauses while you scroll", sep(done), sep(total)), false));
        }
        if let Some(s) = &self.snap
            && s.usage >= super::store::WARN_AT
        {
            return Some((format!("Tag store {:.0}% full", s.usage * 100.0), true));
        }
        None
    }
}

/// `12500` -> `12,500`.
pub fn sep(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// (folder, name) of a path.
pub fn split(path: &str) -> (&str, &str) {
    let p = path.trim_end_matches('\\');
    match p.rfind('\\') {
        Some(i) => {
            let dir = if i <= 2 && p.as_bytes().get(1) == Some(&b':') { &path[..=i] } else { &path[..i] };
            (dir, &p[i + 1..])
        }
        None => ("", p),
    }
}

/// One frame of row tags for a folder.
pub struct RowTags<'a> {
    pub ui: &'a mut TagsUi,
    dir: &'a str,
    dh: u64,
    fids: &'a [(u128, u32)],
    pub buf: String,
}

impl RowTags<'_> {
    /// The tags of entry `id` named `name` (`None`: not read yet; asked).
    pub fn mask(&mut self, id: u32, name: &str) -> Option<u64> {
        let fid = self.fids.get(id as usize).filter(|(_, h)| *h == name_hash(name)).map_or(0, |x| x.0);
        let ck = row_key(self.dh, name, fid);
        if let Some(m) = self.ui.cache.get(&ck) {
            return Some(*m);
        }
        if self.ui.pending.insert(ck) {
            self.ui.want.entry(self.dir.to_string()).or_default().push(Row { ck, path: join(self.dir, name), fid });
        }
        None
    }

    pub fn shapes(&self) -> bool {
        self.ui.shapes
    }

    /// The reconcile runs: unread rows show the dashed mark.
    pub fn reading(&self) -> bool {
        self.ui.progress.is_some()
    }

    pub fn defs(&self) -> &[TagDef] {
        self.ui.defs()
    }
}

// ---- the picker -------------------------------------------------------------

pub struct NewTag {
    pub name: TextInput,
    /// swatch 0..8: colour and its mark
    pub swatch: usize,
}

pub struct Picker {
    pub title: String,
    pub at: Pt,
    pub files: Vec<(String, u128)>,
    masks: Vec<Option<u64>>,
    by_ck: HashMap<u64, usize>,
    pub edit: Edit,
    pub field: TextInput,
    /// focused row (tags first, then the action rows)
    pub focus: usize,
    pub new: Option<NewTag>,
    fresh: bool,
    a11y: bool,
    /// a row a screen reader invoked, acted on by the next paint (`TagsUi::a11y_invoke`)
    a11y_inv: Option<u64>,
}

enum PickOut {
    Open,
    Cancel,
    Apply,
    Create { name: String, colour: u8, shape: Shape },
    Manage,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PRow {
    Tag(usize),
    New,
    Manage,
    Clear,
}

impl Picker {
    fn got(&mut self, ck: u64, mask: u64) {
        if let Some(&i) = self.by_ck.get(&ck) {
            self.masks[i] = Some(mask);
        }
    }

    /// Masks read so far (unread files count as untagged until they come).
    fn known(&self) -> impl Iterator<Item = &u64> {
        self.masks.iter().flatten()
    }

    pub fn loading(&self) -> bool {
        self.masks.iter().any(|m| m.is_none())
    }

    fn check(&self, bit: u64) -> Check {
        let orig = model::check_of(self.known(), bit);
        self.edit.shown(bit, orig)
    }

    fn toggle(&mut self, bit: u64) {
        let c = model::toggled(self.check(bit));
        let orig = model::check_of(self.known(), bit);
        // back to the original state: nothing staged for this tag
        if c == orig { self.edit.stage(bit, Check::Mixed) } else { self.edit.stage(bit, c) }
    }

    fn rows(&self, defs: &[TagDef]) -> Vec<PRow> {
        let q = self.field.text.trim().to_lowercase();
        let mut v: Vec<PRow> = defs.iter().enumerate().filter(|(_, d)| q.is_empty() || d.name.to_lowercase().contains(&q)).map(|(i, _)| PRow::Tag(i)).collect();
        v.extend([PRow::New, PRow::Manage, PRow::Clear]);
        v
    }

    fn exact(&self, defs: &[TagDef]) -> bool {
        let q = self.field.text.trim();
        defs.iter().any(|d| d.name.eq_ignore_ascii_case(q))
    }

    fn start_new(&mut self, defs: &[TagDef]) {
        let mut name = TextInput::default();
        name.text = self.field.text.trim().to_string();
        name.caret = name.text.len();
        name.anchor = name.caret;
        let (c, _) = model::suggest_look(defs);
        self.new = Some(NewTag { name, swatch: c as usize });
    }

    fn paint(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bounds: Rect, defs: &[TagDef], shapes: bool, ro: bool) -> PickOut {
        let t = ui.theme;
        if self.fresh {
            self.fresh = false;
            ui.set_focus(Some(ID_PK_FIELD));
        }
        if !ui.is_popup_open(ID_PICKER) {
            // a click outside: done
            return if self.new.is_some() { PickOut::Cancel } else { PickOut::Apply };
        }
        ui.capture_keys = true;
        if self.new.is_some() {
            return self.paint_new(ui, g, bounds, &t, shapes);
        }
        let rows = self.rows(defs);
        self.focus = self.focus.min(rows.len().saturating_sub(1));
        let ntags = rows.iter().filter(|r| matches!(r, PRow::Tag(_))).count();

        // keys (the field takes printable text unless it is empty)
        let mut out = None;
        let mut act: Option<PRow> = None;
        let empty = self.field.text.is_empty();
        for (qi, ev) in ui.key_input(ID_PK_FIELD) {
            let mut take = true;
            match ev {
                Event::KeyDown { key: Key::Escape, .. } => out = Some(PickOut::Cancel),
                Event::KeyDown { key: Key::Down, mods, .. } if mods.none() => self.focus = (self.focus + 1) % rows.len(),
                Event::KeyDown { key: Key::Up, mods, .. } if mods.none() => self.focus = (self.focus + rows.len() - 1) % rows.len(),
                Event::KeyDown { key: Key::Home, mods, .. } if mods.none() && empty => self.focus = 0,
                Event::KeyDown { key: Key::End, mods, .. } if mods.none() && empty => self.focus = rows.len() - 1,
                Event::KeyDown { key: Key::Char('N'), mods, .. } if mods.ctrl && !mods.alt && !ro => act = Some(PRow::New),
                Event::KeyDown { key: Key::Space, mods, .. } if mods.none() && empty => act = rows.get(self.focus).copied(),
                Event::Char(' ') if empty => {}
                Event::Char(c @ '1'..='9') if empty => {
                    let n = c as usize - '1' as usize;
                    if n < ntags && !ro {
                        act = Some(rows[n]);
                        self.focus = n;
                    }
                }
                Event::KeyDown { key: Key::Enter, mods, .. } if mods.none() => {
                    let q = self.field.text.trim();
                    match rows.get(self.focus) {
                        Some(r @ (PRow::New | PRow::Manage | PRow::Clear)) => act = Some(*r),
                        _ if !q.is_empty() && !self.exact(defs) && !ro => act = Some(PRow::New),
                        _ => out = Some(PickOut::Apply),
                    }
                }
                Event::KeyDown { key: Key::Char(c), mods, .. } if empty && !mods.ctrl && !mods.alt && c.is_ascii_digit() => {}
                _ => take = false,
            }
            if take {
                ui.consume(qi);
            }
        }

        // layout
        let pad = 4.0;
        let ih = t.height_menu_item;
        let w = t.layout_menu_width;
        let h = pad * 2.0 + 20.0 + t.height_field + 6.0 + ntags.max(1) as f32 * ih + 9.0 + 3.0 * ih + 22.0;
        let x = self.at.x.clamp(bounds.x + 4.0, (bounds.r() - w - 4.0).max(bounds.x));
        let y = if self.at.y + h > bounds.b() - 4.0 { (self.at.y - h).max(bounds.y + 4.0) } else { self.at.y };
        let pr = Rect::new(x, y, w, h);
        let mut a11y_rows: Vec<uia::Row> = Vec::new();
        if ui.popup_begin_r(g, ID_PICKER, pr, t.radius_lg) {
            let inner = pr.pad(pad);
            let mut cy = inner.y;
            // header
            let title = if self.loading() { format!("{} \u{b7} reading\u{2026}", self.title) } else { self.title.clone() };
            g.text(&title, Rect::new(inner.x + 8.0, cy, inner.w - 16.0, 20.0), t.font_small().b(), t.text_dim);
            cy += 20.0;
            // find or create
            let fr = Rect::new(inner.x + 2.0, cy + 2.0, inner.w - 4.0, t.height_field);
            self.field.pad_left = 20.0;
            let resp = text_input(ui, g, ID_PK_FIELD, fr, &mut self.field, "Find or create tag");
            g.text("\u{E721}", Rect::new(fr.x + 6.0, fr.y, 16.0, fr.h), f(12.0).icon(), t.text_dim);
            if resp.changed {
                self.focus = 0;
            }
            cy = fr.b() + 4.0;
            if ntags == 0 {
                let q = self.field.text.trim();
                let msg = if q.is_empty() { "No tags yet".to_string() } else { format!("Enter: create \u{201c}{q}\u{201d}") };
                g.text(&msg, Rect::new(inner.x + 10.0, cy, inner.w - 20.0, ih), t.font(), t.text_dim);
                cy += ih;
            }
            let mut n = 0usize;
            for (i, row) in rows.iter().enumerate() {
                if matches!(row, PRow::New) {
                    g.fill(Rect::new(inner.x + 4.0, cy + 4.0, inner.w - 8.0, 1.0), t.border);
                    cy += 9.0;
                }
                let rr = Rect::new(inner.x, cy, inner.w, ih);
                let disabled = ro && !matches!(row, PRow::Manage);
                let it = ui.interact(ID_PK_ROW.with(i as u64), rr, Sense::click());
                let focused = i == self.focus;
                if it.hovered || focused {
                    g.round(rr, t.radius_sm, t.selected);
                }
                if focused {
                    g.round_stroke(rr, t.radius_sm, t.focus, 1.0);
                }
                if (it.clicked == Some(Button::Left) || self.a11y_inv == Some(i as u64 + 1)) && !disabled {
                    self.focus = i;
                    act = Some(*row);
                }
                let name_c = if disabled { t.text_disabled } else { t.text };
                let (name, check, sc) = match row {
                    PRow::Tag(k) => {
                        n += 1;
                        let d = &defs[*k];
                        let c = self.check(d.bit());
                        let cb = Rect::new(rr.x + 8.0, rr.y + (ih - 14.0) / 2.0, 14.0, 14.0);
                        match c {
                            Check::On => {
                                g.round(cb, 3.0, t.accent);
                                g.text("\u{E73E}", cb, f(9.0).icon().c(), t.on_accent);
                            }
                            Check::Mixed => {
                                g.round(cb, 3.0, t.field);
                                g.round_stroke(cb, 3.0, t.popup_border, 1.0);
                                g.text("\u{E738}", cb, f(9.0).icon().c(), t.text);
                            }
                            Check::Off => {
                                g.round(cb, 3.0, t.field);
                                g.round_stroke(cb, 3.0, t.popup_border, 1.0);
                            }
                        }
                        marks::draw(g, marks::mark_of(d, shapes), d.rgb(t.is_dark()), cb.r() + 8.0 + 6.0, rr.y + ih / 2.0, 12.0);
                        g.text(&d.name, Rect::new(cb.r() + 28.0, rr.y, rr.w - 28.0 - 70.0, ih), t.font(), name_c);
                        let sc = if n <= 9 { n.to_string() } else { String::new() };
                        (d.name.clone(), Some(c), sc)
                    }
                    PRow::New => {
                        g.text("\u{E710}", Rect::new(rr.x + 8.0, rr.y, 14.0, ih), f(12.0).icon().c(), name_c);
                        g.text("New tag\u{2026}", Rect::new(rr.x + 30.0, rr.y, rr.w - 100.0, ih), t.font(), name_c);
                        ("New tag".into(), None, "Ctrl+N".into())
                    }
                    PRow::Manage => {
                        g.text("\u{E9E9}", Rect::new(rr.x + 8.0, rr.y, 14.0, ih), f(12.0).icon().c(), name_c);
                        g.text("Manage tags\u{2026}", Rect::new(rr.x + 30.0, rr.y, rr.w - 100.0, ih), t.font(), name_c);
                        ("Manage tags".into(), None, String::new())
                    }
                    PRow::Clear => {
                        g.text("Clear all tags", Rect::new(rr.x + 30.0, rr.y, rr.w - 100.0, ih), t.font(), name_c);
                        ("Clear all tags".into(), None, String::new())
                    }
                };
                if !sc.is_empty() {
                    let kw = g.measure(&sc, t.font_mono()) + 10.0;
                    tsubame_ui::keycap(g, &t, rr.r() - 8.0 - kw.max(t.height_keycap), rr.y + ih / 2.0, &sc);
                }
                let spoken = match check {
                    Some(Check::Mixed) => format!("{name}, mixed"),
                    _ => name,
                };
                a11y_rows.push(uia::Row {
                    key: i as u64 + 1,
                    name: spoken,
                    shortcut: sc,
                    rect: [rr.x, rr.y, rr.w, rr.h],
                    enabled: !disabled,
                    toggled: check == Some(Check::On),
                    focused,
                    visible: true,
                    ..Default::default()
                });
                cy += ih;
            }
            self.a11y_inv = None;
            // footer
            let fr = Rect::new(pr.x, pr.b() - 22.0, pr.w, 22.0);
            g.fill(Rect::new(fr.x, fr.y, fr.w, 1.0), t.border);
            let mut fx = fr.x + 8.0;
            for (k, label) in [("Space", "toggle"), ("Enter", "done"), ("Esc", "cancel")] {
                fx = tsubame_ui::keycap(g, &t, fx, fr.y + 11.0, k) + 4.0;
                let lw = g.measure(label, f(10.0));
                g.text(label, Rect::new(fx, fr.y, lw + 2.0, fr.h), f(10.0), t.text_dim);
                fx += lw + 10.0;
            }
            ui.popup_end(g);
        }
        if uia::active() {
            self.a11y = true;
            uia::publish(uia::Snap { levels: vec![uia::Level { key: 0, name: self.title.clone(), rect: [pr.x, pr.y, pr.w, pr.h], rows: a11y_rows, filter: Some(([pr.x, pr.y + 24.0, pr.w, t.height_field], self.field.text.clone())) }] });
        }
        if let Some(row) = act {
            match row {
                PRow::Tag(k) => {
                    if !ro {
                        self.toggle(defs[k].bit());
                    }
                }
                PRow::New => self.start_new(defs),
                PRow::Manage => return PickOut::Manage,
                PRow::Clear => {
                    if !ro {
                        for d in defs {
                            self.edit.stage(d.bit(), Check::Off);
                        }
                        return PickOut::Apply;
                    }
                }
            }
            ui.request_repaint();
        }
        if let Some(o) = out {
            ui.close_popup(ID_PICKER);
            return o;
        }
        PickOut::Open
    }

    /// "New tag": name, the 4x2 colour and mark swatches, a preview line,
    /// "Create and apply" (Enter) and Cancel (Esc).
    fn paint_new(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bounds: Rect, t: &Theme, shapes: bool) -> PickOut {
        let Some(nt) = &mut self.new else { return PickOut::Open };
        if ui.focus() != Some(ID_PK_NAME) {
            ui.set_focus(Some(ID_PK_NAME));
        }
        let mut create = false;
        let mut cancel = false;
        for (qi, ev) in ui.key_input(ID_PK_NAME) {
            let mut take = true;
            match ev {
                Event::KeyDown { key: Key::Escape, .. } => cancel = true,
                Event::KeyDown { key: Key::Enter, .. } => create = true,
                Event::KeyDown { key: Key::Tab, mods, .. } => nt.swatch = if mods.shift { (nt.swatch + 7) % 8 } else { (nt.swatch + 1) % 8 },
                Event::KeyDown { key: Key::Up | Key::Down, mods, .. } if mods.none() => nt.swatch = (nt.swatch + 4) % 8,
                Event::KeyDown { key: Key::Left, mods, .. } if mods.alt => nt.swatch = (nt.swatch + 7) % 8,
                Event::KeyDown { key: Key::Right, mods, .. } if mods.alt => nt.swatch = (nt.swatch + 1) % 8,
                Event::KeyDown { key: Key::Left, mods, .. } if mods.none() && nt.name.text.is_empty() => nt.swatch = (nt.swatch + 7) % 8,
                Event::KeyDown { key: Key::Right, mods, .. } if mods.none() && nt.name.caret >= nt.name.text.len() => nt.swatch = (nt.swatch + 1) % 8,
                _ => take = false,
            }
            if take {
                ui.consume(qi);
            }
        }
        let pad = 4.0;
        let ih = t.height_menu_item;
        // grows (up to 420) so the "Create ... and apply" row fits its name
        let create_w = g.measure(&format!("Create \u{201c}{}\u{201d} and apply", nt.name.text.trim()), t.font()) + 30.0 + 100.0 + 2.0 * pad;
        let w = t.layout_menu_width.max(create_w).min(420.0);
        let h = pad * 2.0 + 20.0 + t.height_field + 6.0 + 20.0 + 8.0 + 30.0 * 2.0 + 4.0 + 8.0 + 20.0 + 9.0 + ih * 2.0;
        let x = self.at.x.clamp(bounds.x + 4.0, (bounds.r() - w - 4.0).max(bounds.x));
        let y = if self.at.y + h > bounds.b() - 4.0 { (self.at.y - h).max(bounds.y + 4.0) } else { self.at.y };
        let pr = Rect::new(x, y, w, h);
        let mut a11y_rows = Vec::new();
        let (colour, shape) = (nt.swatch as u8, Shape::ALL[nt.swatch]);
        if ui.popup_begin_r(g, ID_PICKER, pr, t.radius_lg) {
            let inner = pr.pad(pad);
            let mut cy = inner.y;
            g.text("New tag", Rect::new(inner.x + 8.0, cy, inner.w - 16.0, 20.0), t.font_small().b(), t.text_dim);
            cy += 20.0;
            let fr = Rect::new(inner.x + 2.0, cy + 2.0, inner.w - 4.0, t.height_field);
            text_input(ui, g, ID_PK_NAME, fr, &mut nt.name, "Tag name");
            cy = fr.b() + 6.0;
            g.text("Colour and mark", Rect::new(inner.x + 8.0, cy, inner.w - 16.0, 20.0), t.font_small().b(), t.text_dim);
            cy += 24.0;
            let sw = (inner.w - 4.0 - 3.0 * 4.0) / 4.0;
            for i in 0..8 {
                let (col, row) = (i % 4, i / 4);
                let sr = Rect::new(inner.x + 2.0 + col as f32 * (sw + 4.0), cy + row as f32 * 34.0, sw, 30.0);
                let it = ui.interact(ID_PK_SW.with(i as u64), sr, Sense::click());
                g.round(sr, t.radius_md, if it.hovered { t.hover } else { t.field });
                g.round_stroke(sr, t.radius_md, if i == nt.swatch { t.focus } else { t.border }, 1.0);
                if i == nt.swatch {
                    g.round_stroke(sr.pad(-2.0), t.radius_md + 2.0, t.focus, 1.0);
                }
                let m = if shapes { Mark::Shape(Shape::ALL[i]) } else { Mark::Dot };
                marks::draw(g, m, model::colour_rgb(i as u8, t.is_dark()), sr.x + sr.w / 2.0, sr.y + 15.0, 14.0);
                if it.clicked == Some(Button::Left) || self.a11y_inv == Some(100 + i as u64) {
                    nt.swatch = i;
                    ui.set_focus(Some(ID_PK_NAME));
                }
                a11y_rows.push(uia::Row { key: 100 + i as u64, name: format!("{} {}", model::COLOUR_NAMES[i], Shape::ALL[i].name()), rect: [sr.x, sr.y, sr.w, sr.h], enabled: true, toggled: i == nt.swatch, focused: false, visible: true, ..Default::default() });
            }
            cy += 68.0;
            // preview: "Blue · hexagon"
            let pv = format!("{} \u{b7} {}", model::COLOUR_NAMES[colour as usize], if shapes { shape.name() } else { "dot" });
            marks::draw(g, if shapes { Mark::Shape(shape) } else { Mark::Dot }, model::colour_rgb(colour, t.is_dark()), inner.x + 13.0, cy + 10.0, 10.0);
            g.text(&pv, Rect::new(inner.x + 24.0, cy, inner.w - 120.0, 20.0), t.font_small(), t.text_dim);
            g.text("Tab / arrows", Rect::new(inner.r() - 100.0, cy, 92.0, 20.0), t.font_small().r(), t.text_dim);
            cy += 20.0;
            g.fill(Rect::new(inner.x + 4.0, cy + 4.0, inner.w - 8.0, 1.0), t.border);
            cy += 9.0;
            let name = nt.name.text.trim().to_string();
            let ok = model::clean_name(&name).is_some();
            for (i, (label, key)) in [(format!("Create \u{201c}{name}\u{201d} and apply"), "Enter"), ("Cancel".to_string(), "Esc")].into_iter().enumerate() {
                let rr = Rect::new(inner.x, cy, inner.w, ih);
                let it = ui.interact(ID_PK_ROW.with(50 + i as u64), rr, Sense::click());
                let focused = i == 0;
                if it.hovered || focused {
                    g.round(rr, t.radius_sm, t.selected);
                }
                if focused {
                    g.round_stroke(rr, t.radius_sm, t.focus, 1.0);
                }
                if i == 0 {
                    g.text("\u{E73E}", Rect::new(rr.x + 8.0, rr.y, 14.0, ih), f(12.0).icon().c(), if ok { t.text } else { t.text_disabled });
                }
                g.text(&label, Rect::new(rr.x + 30.0, rr.y, rr.w - 100.0, ih), t.font(), if i == 0 && !ok { t.text_disabled } else { t.text });
                let kw = g.measure(key, t.font_mono()) + 10.0;
                tsubame_ui::keycap(g, t, rr.r() - 8.0 - kw.max(t.height_keycap), rr.y + ih / 2.0, key);
                if it.clicked == Some(Button::Left) || self.a11y_inv == Some(200 + i as u64) {
                    if i == 0 { create = true } else { cancel = true }
                }
                a11y_rows.push(uia::Row { key: 200 + i as u64, name: label, shortcut: key.into(), rect: [rr.x, rr.y, rr.w, rr.h], enabled: i == 1 || ok, focused, visible: true, ..Default::default() });
                cy += ih;
            }
            self.a11y_inv = None;
            ui.popup_end(g);
        }
        if uia::active() {
            self.a11y = true;
            uia::publish(uia::Snap { levels: vec![uia::Level { key: 0, name: "New tag".into(), rect: [pr.x, pr.y, pr.w, pr.h], rows: a11y_rows, filter: Some(([pr.x, pr.y + 24.0, pr.w, t.height_field], nt.name.text.clone())) }] });
        }
        if cancel {
            // opened straight into the form (sidebar +): nothing to go back to
            if self.files.is_empty() {
                return PickOut::Cancel;
            }
            self.new = None;
            ui.set_focus(Some(ID_PK_FIELD));
            ui.request_repaint();
            return PickOut::Open;
        }
        if create {
            let name = nt.name.text.trim().to_string();
            if model::clean_name(&name).is_some() {
                ui.close_popup(ID_PICKER);
                return PickOut::Create { name, colour, shape };
            }
        }
        PickOut::Open
    }

    fn close_a11y(&mut self) {
        if std::mem::take(&mut self.a11y) {
            uia::publish(uia::Snap::default());
        }
    }
}

// ---- filter results ------------------------------------------------------

pub struct Results {
    pub slot: u8,
    /// the pane that shows them
    pub pane: usize,
    seq: u64,
    pub hits: Vec<Hit>,
    pub capped: bool,
    pub loading: bool,
    pub list: ListState,
}

struct ResultRows<'a> {
    hits: &'a [Hit],
    defs: &'a [TagDef],
    shapes: bool,
    loc_w: f32,
    date_w: f32,
    size_w: f32,
    text: String,
    buf: String,
}

impl ListSource for ResultRows<'_> {
    fn len(&self) -> usize {
        self.hits.len()
    }
    fn draw_row(&mut self, g: &mut dyn Canvas, t: &Theme, i: usize, r: Rect, _st: RowState) {
        let Some(h) = self.hits.get(i) else { return };
        let (dir, name) = split(&h.path);
        let ir = Rect::new(r.x + 8.0, r.y + (r.h - 16.0) / 2.0, 16.0, 16.0);
        if !crate::packs::draw_icon(g, t, name, h.dir, false, ir) {
            let (ic, col) = if h.dir { (Rect::new(ir.x, ir.y + 3.0, 16.0, 11.0), t.folder) } else { (Rect::new(ir.x + 2.0, ir.y, 12.0, 16.0), t.text_dim) };
            g.round_a(ic, 2.0, col, 0.35);
        }
        let name_w = (r.w - self.loc_w - self.date_w - self.size_w - 40.0).max(40.0);
        let cw = marks::cluster_w(g, t, self.defs, h.mask, 3, &mut self.buf);
        let font = t.font();
        let nw = g.measure(name, font).min((name_w - cw).max(20.0));
        g.text(name, Rect::new(r.x + 32.0, r.y, nw + 1.0, r.h), font, t.text);
        marks::draw_cluster(g, t, self.defs, h.mask, self.shapes, r.x + 32.0 + nw + marks::LEAD, r.y + r.h / 2.0, 3, marks::MARK, marks::GAP, &mut self.buf);
        let mut x = r.x + 32.0 + name_w + 8.0;
        g.text(dir, Rect::new(x + 8.0, r.y, self.loc_w - 12.0, r.h), font, t.text_dim);
        x += self.loc_w;
        if h.modified > 0 {
            crate::data::fmt_date(h.modified, &mut self.text);
            g.text(&self.text, Rect::new(x + 8.0, r.y, self.date_w - 12.0, r.h), font, t.text_dim);
        }
        x += self.date_w;
        if !h.dir {
            crate::data::fmt_size(h.size, &mut self.text);
            g.text(&self.text, Rect::new(x, r.y, self.size_w - 8.0, r.h), font.r(), t.text_dim);
        }
    }
}

impl TagsUi {
    /// The filter results in place of the folder listing: Name, Location,
    /// Modified, Size across folders.
    pub fn paint_results(&mut self, ui: &mut Ui, g: &mut dyn Canvas, area: Rect, t: &Theme) -> Act {
        let defs = self.defs().to_vec();
        let shapes = self.shapes;
        let Some(res) = &mut self.results else { return Act::None };
        let mut a = area;
        let hr = a.cut_top(t.height_column_header);
        let total = hr.w - t.layout_scrollbar_width;
        let (loc_w, date_w, size_w) = (190.0, 150.0, 90.0);
        let name_w = (total - loc_w - date_w - size_w).max(60.0);
        let cols = [tsubame_ui::HeaderCol { label: "Name", right: false }, tsubame_ui::HeaderCol { label: "Location", right: false }, tsubame_ui::HeaderCol { label: "Modified", right: false }, tsubame_ui::HeaderCol { label: "Size", right: true }];
        let spans = [(0.0, name_w), (name_w, loc_w), (name_w + loc_w, date_w), (name_w + loc_w + date_w, size_w)];
        tsubame_ui::column_header(ui, g, ID_RESULTS.with(1), hr, &cols, &spans, None);
        g.fill(a, t.row);
        res.list.row_h = t.height_list_row;
        let mut rows = ResultRows { hits: &res.hits, defs: &defs, shapes, loc_w, date_w, size_w, text: String::new(), buf: String::new() };
        let lr = tsubame_ui::list(ui, g, ID_RESULTS, a, &mut res.list, &mut rows);
        if res.hits.is_empty() && !res.loading {
            let name = defs.iter().find(|d| d.slot == res.slot).map_or("this tag", |d| d.name.as_str());
            let body = Rect::new(a.x, a.y, a.w - t.layout_scrollbar_width, a.h);
            let cy = body.y + body.h / 2.0;
            g.text("\u{E8EC}", Rect::new(body.x, cy - 60.0, body.w, 40.0), f(32.0).icon().c(), t.text_disabled);
            g.text(&format!("No items tagged {name}"), Rect::new(body.x, cy - 14.0, body.w, 22.0), t.font_md().b().c(), t.text);
            g.text("Drag files onto the tag in the sidebar, or select files and press Alt+T.", Rect::new(body.x + 20.0, cy + 10.0, body.w - 40.0, 20.0), t.font().c(), t.text_dim);
        }
        let act = lr.activated.and_then(|i| res.hits.get(i)).map(|h| {
            if h.dir {
                Act::Go(h.path.clone())
            } else {
                let (d, n) = split(&h.path);
                Act::Reveal { folder: d.to_string(), name: n.to_string() }
            }
        });
        let scroll = res.list.scroll;
        self.note_scroll(scroll);
        if act.is_some() {
            self.results = None;
        }
        act.unwrap_or(Act::None)
    }

    /// Status-bar count of the results ("12 items" / "Tag results (all folders)").
    pub fn results_status(&self) -> Option<String> {
        let r = self.results.as_ref()?;
        Some(if r.loading {
            "Tag results: reading…".into()
        } else if r.capped {
            format!("{}+ items · Tag results (all folders)", sep(r.hits.len()))
        } else {
            format!("{} item{} · Tag results (all folders)", sep(r.hits.len()), if r.hits.len() == 1 { "" } else { "s" })
        })
    }

    /// The selected hit's path (context menu, drag) when results show.
    pub fn result_at(&self, i: usize) -> Option<&Hit> {
        self.results.as_ref()?.hits.get(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(sep(12500), "12,500");
        assert_eq!(sep(999), "999");
        assert_eq!(split("C:\\a\\b.txt"), ("C:\\a", "b.txt"));
        assert_eq!(split("C:\\b.txt"), ("C:\\", "b.txt"));
        assert_eq!(join("C:\\", "x"), "C:\\x");
        assert_eq!(join("C:\\a", "x"), "C:\\a\\x");
        assert_eq!(name_hash16(&"abc".encode_utf16().collect::<Vec<_>>()), name_hash("abc"));
        assert_ne!(row_key(dir_hash("C:\\a"), "x", 0), row_key(dir_hash("C:\\a"), "x", 5));
        assert_eq!(dir_hash("C:\\A"), dir_hash("c:\\a"));
    }

    /// Smoke bug: "Move to new window" opened a process that showed a banner "tags.db is locked
    /// by another process". That is normal for a second window: no banner, a status text; a
    /// real problem (unwritable folder, damaged store) keeps its banner.
    #[test]
    fn a_second_windows_locked_store_is_quiet_but_real_problems_still_show() {
        let snap = |mode: Mode, note: Option<&str>| Snapshot { defs: model::defaults(), counts: [0; model::MAX_TAGS], mode, note: note.map(String::from), items: 0, usage: 0.0 };
        let paint = |tags: &mut TagsUi| {
            let mut ui = Ui::new(tsubame_ui::DARK);
            let mut g = tsubame_ui::TestCanvas::new(800.0, 200.0);
            ui.begin_frame((800.0, 200.0));
            let h = tags.paint_banner(&mut ui, &mut g, Rect::new(0.0, 0.0, 800.0, 100.0), &tsubame_ui::DARK);
            let _ = ui.end_frame();
            h
        };
        let mut tags = TagsUi::new();
        tags.on = true;
        tags.snap = Some(snap(Mode::Locked, None));
        assert_eq!(paint(&mut tags), 0.0, "no banner for a locked store");
        assert!(tags.read_only() && !tags.writable(), "still read-only");
        assert_eq!(tags.status().map(|s| s.0).as_deref(), Some("Tags read-only: open in another window"));
        tags.snap = Some(snap(Mode::Unwritable("access denied".into()), None));
        assert!(paint(&mut tags) > 0.0, "an unwritable folder is a problem the user must see");
        tags.snap = Some(snap(Mode::Locked, Some("tags.db was damaged")));
        assert!(paint(&mut tags) > 0.0, "a damage note keeps the banner");
    }
    #[test]
    fn picker_toggles_like_the_mock_for_a_mixed_selection() {
        let defs = model::defaults();
        let mut p = Picker { title: String::new(), at: Pt::default(), files: vec![], masks: vec![Some(1), Some(0), Some(1 | 16)], by_ck: HashMap::new(), edit: Edit::default(), field: TextInput::default(), focus: 0, new: None, fresh: false, a11y: false, a11y_inv: None };
        assert_eq!(p.check(1), Check::Mixed);
        assert_eq!(p.check(16), Check::Mixed);
        assert_eq!(p.check(2), Check::Off);
        p.toggle(1);
        assert_eq!(p.check(1), Check::On, "mixed -> on for all");
        p.toggle(1);
        assert_eq!(p.check(1), Check::Off);
        p.toggle(1);
        assert_eq!(p.check(1), Check::On);
        assert_eq!(p.edit.apply(0), 1);
        // find-or-create filters the rows; actions stay
        p.field.text = "re".into();
        let rows = p.rows(&defs);
        assert_eq!(rows.iter().filter(|r| matches!(r, PRow::Tag(_))).count(), 1, "To review");
        assert!(rows.ends_with(&[PRow::New, PRow::Manage, PRow::Clear]));
        p.field.text = "urgent".into();
        assert!(p.exact(&defs));
        p.field.text = "Contract".into();
        p.start_new(&defs);
        assert_eq!(p.new.as_ref().unwrap().name.text, "Contract");
    }
}
