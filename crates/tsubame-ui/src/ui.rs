//! The immediate-mode core: ids, hot/active/focus, the event queue, hit
//! testing, popups, keyboard routing and the repaint rule.
//!
//! One frame:
//! 1. The platform hands every input to `Ui::push`. It returns `true` when the
//!    event can change what is on screen; only then does the platform ask for
//!    a paint (InvalidateRect). Events that cannot matter (a move over empty
//!    space, a key-up) are dropped there, so an idle app stays idle.
//! 2. On paint: `begin_frame`, then the app draws its widgets in back-to-front
//!    order (popups last), then `end_frame`. Widgets read the queued events
//!    while they draw and mark the ones they used.
//! 3. `end_frame` returns `Output`. `repaint` is true only if something asked
//!    for another frame (a widget changed state the frame already drew past,
//!    or an animation); otherwise nothing runs until the next input.
//!
//! Hit testing uses the hit list of the previous frame: a widget registers
//! its rect while it draws (`interact`), and the next frame's events go to the
//! topmost registered rect under the pointer. That one-frame lag is the
//! normal immediate-mode trade; it is what lets a popup drawn last swallow
//! clicks meant for widgets drawn under it.

use crate::canvas::Canvas;
use crate::geom::{Pt, Rect};
use crate::input::{Button, Cursor, Event, Key, Mods};
use crate::theme::Theme;

/// A widget id: a hash of a name and indices. Stable across frames as long as
/// the app derives it the same way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(pub u64);

const FNV_OFF: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl Id {
    pub const fn new(name: &str) -> Id {
        let b = name.as_bytes();
        let mut h = FNV_OFF;
        let mut i = 0;
        while i < b.len() {
            h ^= b[i] as u64;
            h = h.wrapping_mul(FNV_PRIME);
            i += 1;
        }
        Id(h)
    }
    /// A child id (row `n` of a list, tab `n`, a part of a widget).
    pub const fn with(self, n: u64) -> Id {
        let mut h = self.0 ^ 0x9e37_79b9_7f4a_7c15;
        let mut i = 0;
        while i < 8 {
            h ^= (n >> (i * 8)) & 0xff;
            h = h.wrapping_mul(FNV_PRIME);
            i += 1;
        }
        Id(h)
    }
    pub const fn child(self, name: &str) -> Id {
        Id::new(name).with(self.0)
    }
}

/// What a widget's rect responds to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sense {
    /// press/release (becomes `active` while a button is held)
    pub click: bool,
    /// takes the keyboard focus on press, and joins the Tab chain
    pub focus: bool,
    /// receives wheel events over it
    pub wheel: bool,
    pub cursor: Cursor,
    /// Hover granularity: `Some(cells)` means the hover state changes per
    /// cell (list rows, tabs), so a move between rows repaints and a move
    /// inside one row does not. `None`: hover is per widget.
    pub cells: Option<Cells>,
    /// redraw on every move while hovered (rare: tooltips that follow)
    pub track_moves: bool,
}

impl Sense {
    /// Hover only (blocks the widgets under it from being hot).
    pub const fn hover() -> Sense {
        Sense { click: false, focus: false, wheel: false, cursor: Cursor::Arrow, cells: None, track_moves: false }
    }
    pub const fn click() -> Sense {
        Sense { click: true, ..Sense::hover() }
    }
    pub const fn focusable(mut self) -> Sense {
        self.focus = true;
        self
    }
    pub const fn wheel(mut self) -> Sense {
        self.wheel = true;
        self
    }
    pub const fn cursor(mut self, c: Cursor) -> Sense {
        self.cursor = c;
        self
    }
    /// Rows of height `h` starting at `y0`.
    pub const fn cells(mut self, y0: f32, h: f32) -> Sense {
        self.cells = Some(Cells { horizontal: false, origin: y0, size: h });
        self
    }
    /// Columns of width `w` starting at `x0`.
    pub const fn cells_x(mut self, x0: f32, w: f32) -> Sense {
        self.cells = Some(Cells { horizontal: true, origin: x0, size: w });
        self
    }
}

/// Equal cells along one axis (see `Sense::cells`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cells {
    pub horizontal: bool,
    pub origin: f32,
    pub size: f32,
}

impl Cells {
    /// Cell index of `p` (may be negative or past the end).
    pub fn index(&self, p: Pt) -> i64 {
        if self.size <= 0.0 {
            return 0;
        }
        let v = if self.horizontal { p.x } else { p.y };
        ((v - self.origin) / self.size).floor() as i64
    }
}

#[derive(Clone, Copy, Debug)]
struct Hit {
    id: Id,
    rect: Rect,
    sense: Sense,
}

#[derive(Clone, Copy, Debug)]
struct Queued {
    ev: Event,
    /// widget the event goes to, resolved at `begin_frame` from the previous
    /// frame's hits (mouse events only)
    target: Option<Id>,
    used: bool,
}

/// A press on a widget.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Press {
    pub pos: Pt,
    pub button: Button,
    pub mods: Mods,
    pub double: bool,
}

/// What happened to a widget this frame (from `Ui::interact`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Interaction {
    /// the pointer is over it (and nothing else is being dragged)
    pub hovered: bool,
    /// it is `active` (a button went down on it and is still held)
    pub active: bool,
    /// the last press on it this frame
    pub pressed: Option<Press>,
    /// a press and its release both landed on it: which button
    pub clicked: Option<Button>,
    /// where the last click was released
    pub click_pos: Pt,
    pub click_mods: Mods,
    /// a button that was held on it went up this frame
    pub released: bool,
    /// wheel notches over it this frame (summed): (dx, dy)
    pub wheel: (f32, f32),
    pub wheel_mods: Mods,
    /// it has the keyboard focus
    pub focused: bool,
}

/// A key shortcut bound to an app command (the command palette and the
/// keybinding settings use the same table later).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub key: Key,
    pub mods: Mods,
    pub command: u32,
    /// Global shortcuts run before the focused widget sees the key (Ctrl+T in
    /// a text field opens a tab). Others only get keys no widget used.
    pub global: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Shortcuts {
    pub list: Vec<Shortcut>,
}

impl Shortcuts {
    pub fn bind(&mut self, key: Key, mods: Mods, command: u32, global: bool) {
        self.list.retain(|s| !(s.key == key && s.mods == mods));
        self.list.push(Shortcut { key, mods, command, global });
    }
    pub fn lookup(&self, key: Key, mods: Mods, global: bool) -> Option<u32> {
        self.list.iter().find(|s| s.key == key && s.mods == mods && s.global == global).map(|s| s.command)
    }
}

/// Clipboard text, provided by the platform (text input calls it on Ctrl+C/X/V).
pub trait Clipboard {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, s: &str);
}

/// An in-memory clipboard (tests, and the default until the platform sets one).
#[derive(Default)]
pub struct MemClipboard(pub Option<String>);
impl Clipboard for MemClipboard {
    fn get(&mut self) -> Option<String> {
        self.0.clone()
    }
    fn set(&mut self, s: &str) {
        self.0 = Some(s.to_string());
    }
}

struct Popup {
    id: Id,
    /// where it was drawn last frame (for click-outside)
    rect: Rect,
    /// focus to give back when it closes
    return_focus: Option<Id>,
}

/// What a frame leaves for the platform.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// paint another frame now (a widget asked, or an animation runs)
    pub repaint: bool,
    /// keep the mouse captured (a drag is in progress): SetCapture/ReleaseCapture
    pub capture: bool,
    pub cursor: Cursor,
    /// caret rect of the focused text field (position the IME window here)
    pub ime_caret: Option<Rect>,
}

pub struct Ui {
    pub theme: Theme,
    pub shortcuts: Shortcuts,
    pub clipboard: Box<dyn Clipboard>,

    mouse: Pt,
    mouse_in: bool,
    /// buttons currently held (by the platform's account)
    held: [bool; 3],
    window_focused: bool,

    queue: Vec<Queued>,
    hits_prev: Vec<Hit>,
    hits: Vec<Hit>,
    chain_prev: Vec<Id>,
    chain: Vec<Id>,

    hot: Option<Id>,
    /// (id, cell) the pointer was over when hover was last drawn
    hover_key: Option<(Id, i64)>,
    active: Option<Id>,
    active_button: Button,
    focus: Option<Id>,
    /// a value the active widget keeps for its drag (thumb grab offset...)
    pub drag_f64: f64,
    pub drag_pt: Pt,

    popups: Vec<Popup>,
    /// `popup_begin` nesting (clip depth to undo)
    popup_depth: u32,

    composition: String,
    commands: Vec<u32>,
    leftover: Vec<Event>,

    repaint: bool,
    cursor: Cursor,
    ime_caret: Option<Rect>,
    size: (f32, f32),
    frame: u64,
    in_frame: bool,
}

fn button_ix(b: Button) -> usize {
    match b {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
    }
}

impl Default for Ui {
    fn default() -> Self {
        Ui::new(Theme::dark())
    }
}

impl Ui {
    pub fn new(theme: Theme) -> Ui {
        Ui {
            theme,
            shortcuts: Shortcuts::default(),
            clipboard: Box::new(MemClipboard::default()),
            mouse: Pt::new(-1.0, -1.0),
            mouse_in: false,
            held: [false; 3],
            window_focused: true,
            queue: Vec::with_capacity(16),
            hits_prev: Vec::with_capacity(64),
            hits: Vec::with_capacity(64),
            chain_prev: Vec::new(),
            chain: Vec::new(),
            hot: None,
            hover_key: None,
            active: None,
            active_button: Button::Left,
            focus: None,
            drag_f64: 0.0,
            drag_pt: Pt::default(),
            popups: Vec::new(),
            popup_depth: 0,
            composition: String::new(),
            commands: Vec::new(),
            leftover: Vec::new(),
            repaint: false,
            cursor: Cursor::Arrow,
            ime_caret: None,
            size: (0.0, 0.0),
            frame: 0,
            in_frame: false,
        }
    }

    // ---------------------------------------------------------------- input

    /// Feeds one platform event. Returns `true` when it may change the screen
    /// (the platform then asks for a paint); `false` means it was absorbed
    /// (pointer position or cursor updated only) and no frame is needed.
    pub fn push(&mut self, ev: Event) -> bool {
        match ev {
            Event::MouseMove { pos } => {
                self.mouse = pos;
                self.mouse_in = true;
                if self.active.is_some() {
                    return true;
                }
                let key = self.hover_key_at(pos);
                let (cursor, track) = self.top_hit(pos).map(|h| (h.sense.cursor, h.sense.track_moves)).unwrap_or_default();
                self.cursor = cursor;
                if track {
                    return true;
                }
                key != self.hover_key
            }
            Event::MouseLeave => {
                self.mouse_in = false;
                self.mouse = Pt::new(-1.0, -1.0);
                self.active.is_none() && self.hover_key.take().is_some()
            }
            Event::MouseDown { pos, button, .. } => {
                self.mouse = pos;
                self.held[button_ix(button)] = true;
                let wanted = self.top_hit(pos).is_some() || !self.popups.is_empty() || self.active.is_some();
                if wanted {
                    self.enqueue(ev);
                }
                wanted
            }
            Event::MouseUp { pos, button, .. } => {
                self.mouse = pos;
                self.held[button_ix(button)] = false;
                let wanted = self.active.is_some() || self.top_hit(pos).is_some_and(|h| h.sense.click);
                if wanted {
                    self.enqueue(ev);
                }
                wanted
            }
            Event::Wheel { pos, .. } => {
                let wanted = self.wheel_target(pos).is_some();
                if wanted {
                    self.enqueue(ev);
                }
                wanted
            }
            Event::KeyDown { .. } | Event::Char(_) => {
                self.enqueue(ev);
                true
            }
            Event::KeyUp { .. } => false,
            Event::FocusLost => {
                self.window_focused = false;
                self.held = [false; 3];
                self.active = None;
                self.composition.clear();
                self.close_all_popups();
                true
            }
            Event::FocusGained => {
                self.window_focused = true;
                true
            }
        }
    }

    /// Sets the IME composition (uncommitted) string; empty ends it. The
    /// focused text field draws it at the caret. Returns `true` (repaint).
    pub fn set_composition(&mut self, s: &str) -> bool {
        if self.composition == s {
            return false;
        }
        self.composition.clear();
        self.composition.push_str(s);
        true
    }
    pub fn composition(&self) -> &str {
        &self.composition
    }

    fn enqueue(&mut self, ev: Event) {
        // a stalled paint must not grow the queue without bound
        if self.queue.len() >= 512 {
            self.queue.remove(0);
        }
        self.queue.push(Queued { ev, target: None, used: false });
    }

    fn top_hit(&self, p: Pt) -> Option<&Hit> {
        self.hits_prev.iter().rev().find(|h| h.rect.contains_pt(p))
    }
    fn wheel_target(&self, p: Pt) -> Option<Id> {
        self.hits_prev.iter().rev().filter(|h| h.rect.contains_pt(p)).find(|h| h.sense.wheel).map(|h| h.id)
    }
    fn hover_key_at(&self, p: Pt) -> Option<(Id, i64)> {
        let h = self.top_hit(p)?;
        let cell = h.sense.cells.map(|c| c.index(p)).unwrap_or(0);
        Some((h.id, cell))
    }

    // ---------------------------------------------------------------- frame

    /// Starts a frame of `size` DIPs: resolves event targets, dismisses
    /// popups on outside clicks and Escape, runs global shortcuts and Tab.
    pub fn begin_frame(&mut self, size: (f32, f32)) {
        debug_assert!(!self.in_frame, "begin_frame twice");
        self.in_frame = true;
        self.frame += 1;
        self.size = size;
        self.repaint = false;
        self.ime_caret = None;
        self.commands.clear();
        self.leftover.clear();
        self.hover_key = if self.mouse_in { self.hover_key_at(self.mouse) } else { None };
        self.hot = match self.active {
            Some(a) => self.hover_key.filter(|k| k.0 == a).map(|k| k.0),
            None => self.hover_key.map(|k| k.0),
        };

        for i in 0..self.queue.len() {
            let q = self.queue[i];
            match q.ev {
                Event::MouseDown { pos, .. } => {
                    if !self.popups.is_empty() {
                        // close every popup above the one under the click; a
                        // click outside all of them closes all and is eaten
                        let inside = self.popups.iter().rposition(|p| p.rect.contains_pt(pos));
                        match inside {
                            Some(k) if k + 1 < self.popups.len() => self.close_popup(self.popups[k + 1].id),
                            Some(_) => {}
                            None => {
                                self.close_all_popups();
                                self.queue[i].used = true;
                                self.repaint = true;
                                continue;
                            }
                        }
                    }
                    self.queue[i].target = self.top_hit(pos).map(|h| h.id);
                }
                Event::MouseUp { pos, .. } => {
                    self.queue[i].target = self.active.or_else(|| self.top_hit(pos).map(|h| h.id));
                }
                Event::Wheel { pos, .. } => self.queue[i].target = self.wheel_target(pos),
                Event::KeyDown { key, mods, .. } => {
                    if key == Key::Escape && mods.none() && !self.popups.is_empty() {
                        let top = self.popups.last().unwrap().id;
                        self.close_popup(top);
                        self.queue[i].used = true;
                    } else if let Some(cmd) = self.shortcuts.lookup(key, mods, true) {
                        self.commands.push(cmd);
                        self.queue[i].used = true;
                    } else if key == Key::Tab && !mods.ctrl && !mods.alt {
                        self.focus_step(!mods.shift);
                        self.queue[i].used = true;
                    }
                }
                _ => {}
            }
        }
        // `hits_prev` stays last frame's list for the whole frame; `hits`
        // collects this frame's
        self.hits.clear();
        self.chain.clear();
    }

    /// Ends the frame. Unused keys go through the non-global shortcuts; what
    /// is still unused is left in `leftover()` for the app.
    pub fn end_frame(&mut self) -> Output {
        debug_assert!(self.in_frame, "end_frame without begin_frame");
        debug_assert_eq!(self.popup_depth, 0, "popup_begin without popup_end");
        self.in_frame = false;
        for q in std::mem::take(&mut self.queue) {
            if q.used {
                continue;
            }
            match q.ev {
                Event::KeyDown { key, mods, .. } => match self.shortcuts.lookup(key, mods, false) {
                    Some(cmd) => self.commands.push(cmd),
                    None => self.leftover.push(q.ev),
                },
                Event::Char(_) => self.leftover.push(q.ev),
                Event::MouseUp { button, .. } if self.active.is_some() && button == self.active_button => self.active = None,
                Event::MouseDown { .. } | Event::MouseUp { .. } => self.leftover.push(q.ev),
                _ => {}
            }
        }
        // a widget that was active or focused but is gone lets go
        if let Some(a) = self.active
            && (!self.hits.iter().any(|h| h.id == a) || !self.held[button_ix(self.active_button)])
        {
            self.active = None;
        }
        if let Some(f) = self.focus
            && !self.chain.contains(&f)
            && !self.hits.iter().any(|h| h.id == f)
        {
            self.focus = None;
        }
        std::mem::swap(&mut self.hits_prev, &mut self.hits);
        self.hits.clear();
        std::mem::swap(&mut self.chain_prev, &mut self.chain);
        self.chain.clear();
        // cursor from the new hits
        self.cursor = match self.active.and_then(|a| self.hits_prev.iter().find(|h| h.id == a)) {
            Some(h) => h.sense.cursor,
            None => self.top_hit(self.mouse).map(|h| h.sense.cursor).unwrap_or_default(),
        };
        if self.active.is_none() {
            self.hover_key = if self.mouse_in { self.hover_key_at(self.mouse) } else { None };
        }
        Output { repaint: self.repaint, capture: self.active.is_some(), cursor: self.cursor, ime_caret: self.ime_caret }
    }

    /// Ask for another frame after this one (state changed that an earlier
    /// widget already drew, or an animation step).
    pub fn request_repaint(&mut self) {
        self.repaint = true;
    }

    /// Commands triggered by shortcuts in the last frame (global ones are
    /// available during the frame already).
    pub fn commands(&self) -> &[u32] {
        &self.commands
    }
    /// Key, char and mouse events of the last frame that no widget used.
    pub fn leftover(&self) -> &[Event] {
        &self.leftover
    }

    // ------------------------------------------------------------- widgets

    /// Registers `rect` for `id` and returns what happened to it this frame.
    /// Call it once per widget per frame, in draw order.
    pub fn interact(&mut self, id: Id, rect: Rect, sense: Sense) -> Interaction {
        self.hits.push(Hit { id, rect, sense });
        if sense.focus {
            self.chain.push(id);
        }
        let mut it = Interaction { hovered: self.hot == Some(id), ..Default::default() };
        for i in 0..self.queue.len() {
            let q = self.queue[i];
            if q.used {
                continue;
            }
            match q.ev {
                Event::MouseDown { pos, button, mods, double } if q.target == Some(id) => {
                    self.queue[i].used = true;
                    if !sense.click {
                        continue; // swallowed (hover-only blocker)
                    }
                    self.active = Some(id);
                    self.active_button = button;
                    if sense.focus && button == Button::Left {
                        self.focus = Some(id);
                    }
                    it.pressed = Some(Press { pos, button, mods, double });
                }
                Event::MouseUp { pos, button, mods } if (q.target == Some(id) || self.active == Some(id)) => {
                    self.queue[i].used = true;
                    if self.active == Some(id) && button == self.active_button {
                        self.active = None;
                        it.released = true;
                        if rect.contains_pt(pos) {
                            it.clicked = Some(button);
                            it.click_pos = pos;
                            it.click_mods = mods;
                        }
                    }
                }
                Event::Wheel { dx, dy, mods, .. } if q.target == Some(id) => {
                    self.queue[i].used = true;
                    if sense.wheel {
                        it.wheel.0 += dx;
                        it.wheel.1 += dy;
                        it.wheel_mods = mods;
                    }
                }
                _ => {}
            }
        }
        it.active = self.active == Some(id);
        if it.active {
            it.hovered = rect.contains_pt(self.mouse);
        }
        it.focused = self.focus == Some(id);
        it
    }

    /// Keyboard input for the focused widget: unused `KeyDown` and `Char`
    /// events with their queue index (pass it to `consume`). Empty unless
    /// `id` has the focus.
    pub fn key_input(&self, id: Id) -> Vec<(usize, Event)> {
        if self.focus != Some(id) {
            return Vec::new();
        }
        self.queue
            .iter()
            .enumerate()
            .filter(|(_, q)| !q.used && matches!(q.ev, Event::KeyDown { .. } | Event::Char(_)))
            .map(|(i, q)| (i, q.ev))
            .collect()
    }
    /// Marks queued event `i` as used (no other widget or shortcut sees it).
    pub fn consume(&mut self, i: usize) {
        if let Some(q) = self.queue.get_mut(i) {
            q.used = true;
        }
    }

    pub fn mouse(&self) -> Pt {
        self.mouse
    }
    pub fn mouse_in(&self) -> bool {
        self.mouse_in
    }
    pub fn hot(&self) -> Option<Id> {
        self.hot
    }
    pub fn active(&self) -> Option<Id> {
        self.active
    }
    pub fn is_active(&self, id: Id) -> bool {
        self.active == Some(id)
    }
    pub fn focus(&self) -> Option<Id> {
        self.focus
    }
    pub fn has_focus(&self, id: Id) -> bool {
        self.focus == Some(id)
    }
    /// The window has the keyboard (carets draw, focus rings show).
    pub fn window_focused(&self) -> bool {
        self.window_focused
    }
    pub fn set_focus(&mut self, id: Option<Id>) {
        if self.focus != id {
            self.focus = id;
            self.composition.clear();
            self.repaint = true;
        }
    }
    /// Frame size in DIPs.
    pub fn size(&self) -> (f32, f32) {
        self.size
    }
    pub fn frame(&self) -> u64 {
        self.frame
    }
    /// Set by the focused text field: where the IME window goes.
    pub fn set_ime_caret(&mut self, r: Rect) {
        self.ime_caret = Some(r);
    }

    /// Moves the focus along last frame's Tab chain.
    fn focus_step(&mut self, forward: bool) {
        let chain = &self.chain_prev;
        if chain.is_empty() {
            return;
        }
        let n = chain.len();
        let next = match self.focus.and_then(|f| chain.iter().position(|&c| c == f)) {
            Some(i) if forward => chain[(i + 1) % n],
            Some(i) => chain[(i + n - 1) % n],
            None if forward => chain[0],
            None => chain[n - 1],
        };
        self.focus = Some(next);
        self.composition.clear();
    }

    // -------------------------------------------------------------- popups

    /// Opens popup `id` on top of the stack (no-op if open). The focus it
    /// takes from is given back when it closes.
    pub fn open_popup(&mut self, id: Id) {
        if self.is_popup_open(id) {
            return;
        }
        self.popups.push(Popup { id, rect: Rect::ZERO, return_focus: self.focus });
        self.repaint = true;
    }
    /// Closes `id` and every popup above it; the focus returns to where it
    /// was when `id` opened.
    pub fn close_popup(&mut self, id: Id) {
        if let Some(k) = self.popups.iter().position(|p| p.id == id) {
            let ret = self.popups[k].return_focus;
            self.popups.truncate(k);
            self.focus = ret;
            self.repaint = true;
        }
    }
    pub fn close_all_popups(&mut self) {
        if let Some(first) = self.popups.first().map(|p| p.id) {
            self.close_popup(first);
        }
    }
    pub fn is_popup_open(&self, id: Id) -> bool {
        self.popups.iter().any(|p| p.id == id)
    }
    pub fn popups_open(&self) -> usize {
        self.popups.len()
    }

    /// Draws popup `id` at `rect` if it is open: panel, border, a hit that
    /// blocks everything under it, and a clip. Returns false (draw nothing)
    /// when closed. Every `true` needs a `popup_end`. Call popups after all
    /// other widgets, lower popups first.
    pub fn popup_begin(&mut self, c: &mut dyn Canvas, id: Id, rect: Rect) -> bool {
        let Some(p) = self.popups.iter_mut().find(|p| p.id == id) else { return false };
        p.rect = rect;
        // swallows clicks and wheel over the panel that no child takes
        self.interact(id, rect, Sense::click().wheel());
        let t = &self.theme;
        c.round(rect, t.radius_md, t.popup);
        c.round_stroke(rect, t.radius_md, t.popup_border, 1.0);
        c.clip(rect);
        self.popup_depth += 1;
        true
    }
    pub fn popup_end(&mut self, c: &mut dyn Canvas) {
        debug_assert!(self.popup_depth > 0);
        self.popup_depth = self.popup_depth.saturating_sub(1);
        c.unclip();
    }
}
