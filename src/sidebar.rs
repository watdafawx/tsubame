//! Left panel (per the mock): Pinned bookmarks, Quick access, Recent, This PC
//! (drives with free-space bars) and Network, a footer with the collapse
//! button, and a 48 px icon rail when collapsed. The bar at its right edge
//! resizes it. Drive and folder data comes from a worker
//! (`tsubame_win::places`); pins live in config.toml, recents in session.toml.
//!
//! The model (pins, recents, the resize rule) is plain data with unit tests;
//! `paint` draws it and reports what the user asked for as an [`Act`].

use crate::commands;
use crate::data::fmt_volume;
use crate::dnd::{self, DragPayload, PinSlot};
use crate::overlay::{self, MenuItem};
use tsubame_config::Pinned;
use tsubame_ui::{Button, Canvas, Cursor, Id, Key, Pt, Rect, Sense, TextInput, Theme, Ui, f, text_input};
use tsubame_win::places::{Drive, DriveKind, Place};

const ID: Id = Id::new("sidebar");
const ID_MENU: Id = Id::new("sidebar-menu");

pub const RAIL_W: f32 = 48.0;
pub const MIN_W: f32 = 140.0;
pub const MAX_W: f32 = 360.0;
/// Dragging the edge below this snaps to the rail.
pub const SNAP_W: f32 = 120.0;
pub const RECENT_CAP: usize = 8;
const ROW_H: f32 = 24.0;
const RAIL_ITEM: f32 = 28.0;
const FOOTER_H: f32 = 28.0;

pub const PINNED: usize = 0;
pub const QUICK: usize = 1;
pub const RECENT: usize = 2;
pub const PC: usize = 3;
pub const NETWORK: usize = 4;
/// P4.F6: tags with counts (shown while the tags extension is on)
pub const TAGS: usize = 5;
/// config names of the sections (`sidebar.closed`) and their titles
pub const SECTIONS: [(&str, &str); 6] = [("pinned", "Pinned"), ("quick", "Quick access"), ("recent", "Recent"), ("pc", "This PC"), ("network", "Network"), ("tags", "Tags")];
/// the places shown under Quick access, in this order
const QUICK_NAMES: [(&str, &str); 7] = [("Home", "\u{E80F}"), ("Desktop", "\u{E7F4}"), ("Downloads", "\u{E896}"), ("Documents", "\u{E8A5}"), ("Pictures", "\u{E91B}"), ("Music", "\u{E8D6}"), ("Videos", "\u{E714}")];
const G_STAR: &str = "\u{E734}";
const G_RECENT: &str = "\u{E81C}";
const G_DRIVE: &str = "\u{EDA2}";
const G_NET: &str = "\u{E968}";
const G_UNPIN: &str = "\u{E77A}";
const G_CHEV_DOWN: &str = "\u{E70D}";
const G_CHEV_RIGHT: &str = "\u{E76C}";
const G_COLLAPSE: &str = "\u{E76B}";

/// What the user asked of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Go(String),
    NewTab(String),
    OtherPane(String),
    /// files were dropped on this folder
    DropOn(String),
    /// a tag row was clicked: filter by it (again: clear)
    TagFilter(u8),
    /// the Tags header's "+"
    NewTag,
    /// the width or the rail state changed: the view rebuilds its theme
    Layout,
}

/// The pointer's drag, as the sidebar needs it.
pub struct DragCtx<'a> {
    pub payload: Option<&'a DragPayload>,
    /// the single dragged folder, when one can be pinned
    pub folder: Option<&'a str>,
    /// the button went up this frame
    pub released: bool,
}

impl DragCtx<'_> {
    #[allow(dead_code)]
    pub const NONE: DragCtx<'static> = DragCtx { payload: None, folder: None, released: false };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropTarget {
    Folder(String),
    /// between bookmarks: pins the dragged folder at this index
    Pin(usize),
    /// a tag row: applies the tag to the dragged files
    Tag(u8),
}

/// One row of the Tags section (the view fills these from the tag store).
#[derive(Clone, Debug, PartialEq)]
pub struct SideTag {
    pub slot: u8,
    pub name: String,
    pub rgb: u32,
    pub mark: crate::tags::marks::Mark,
    pub count: usize,
    /// the filter shows this tag
    pub on: bool,
}

#[derive(Default)]
pub struct SideOut {
    pub act: Option<Act>,
    /// where a drag in progress would land
    pub target: Option<DropTarget>,
}

/// The result of dragging the resize edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resize {
    Rail,
    Width(f32),
}

/// Edge x (from the sidebar's left) to the state it means.
pub fn resize_rule(x: f32) -> Resize {
    if x < SNAP_W { Resize::Rail } else { Resize::Width(x.clamp(MIN_W, MAX_W)) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Header(usize),
    Pinned(usize),
    Quick(usize),
    Recent(usize),
    Drive(usize),
    Net(usize),
    Tag(usize),
    Empty(usize),
    Sep,
}

#[derive(Clone, Copy, Debug)]
struct Item {
    kind: Kind,
    y: f32,
    h: f32,
}

pub struct Sidebar {
    /// A drive's shell menu, consumed after painting (client DIPs).
    pub shell_menu: Option<(String, Pt)>,
    pub drives: Vec<Drive>,
    pub places: Vec<Place>,
    pub pinned: Vec<Pinned>,
    /// newest first
    pub recents: Vec<String>,
    /// folded sections, in `SECTIONS` order
    pub closed: [bool; 6],
    /// the Tags section (`None`: tags are off, no section)
    pub tags: Option<Vec<SideTag>>,
    /// the 48 px icon rail
    pub rail: bool,
    /// pins or folds changed: main writes config.toml
    pub dirty: bool,
    /// recents changed: main writes session.toml
    pub recents_dirty: bool,
    scroll: f32,
    /// bookmark being renamed
    rename: Option<(usize, TextInput)>,
    rename_frames: u32,
    /// the pinned row the keyboard works on
    pub kb: Option<usize>,
    menu: Option<(usize, Pt)>,
    reorder: Option<usize>,
    /// pinned row pressed: (index, press point)
    press: Option<(usize, Pt)>,
    resizing: bool,
    /// width the resize edge asks for; the view takes it (and clears it)
    pub want_width: Option<f32>,
    tip: Option<(Rect, String)>,
    /// the Tags header's "+" was clicked this frame
    new_tag: bool,
}

impl Default for Sidebar {
    fn default() -> Self {
        Sidebar {
            shell_menu: None,
            drives: Vec::new(),
            places: Vec::new(),
            pinned: Vec::new(),
            recents: Vec::new(),
            closed: [false; 6],
            tags: None,
            rail: false,
            dirty: false,
            recents_dirty: false,
            scroll: 0.0,
            rename: None,
            rename_frames: 0,
            kb: None,
            menu: None,
            reorder: None,
            press: None,
            resizing: false,
            want_width: None,
            tip: None,
            new_tag: false,
        }
    }
}

fn norm(p: &str) -> String {
    p.trim_end_matches('\\').to_ascii_lowercase()
}

/// The label a bookmark gets by default: the folder name (or the drive).
pub fn default_label(path: &str) -> String {
    let n = dnd::name_of(path);
    if n.is_empty() { path.to_string() } else { n.to_string() }
}

/// Appends a row of height `h` at `*y` and moves `*y` below it.
fn add(v: &mut Vec<Item>, y: &mut f32, kind: Kind, h: f32) {
    v.push(Item { kind, y: *y, h });
    *y += h;
}

impl Sidebar {
    pub fn place_path(&self, name: &str) -> Option<&str> {
        self.places.iter().find(|p| p.name == name).map(|p| p.path.as_str())
    }

    /// Free and total bytes of the volume holding `path`, from the last
    /// drive scan (None for UNC paths or before the scan finished).
    pub fn volume_of(&self, path: &str) -> Option<(u64, u64)> {
        let b = path.as_bytes();
        if b.len() < 2 || b[1] != b':' {
            return None;
        }
        let l = (b[0] as char).to_ascii_uppercase();
        self.drives.iter().find(|d| d.letter == l && d.total > 0).map(|d| (d.free, d.total))
    }

    // ---- model -----------------------------------------------------------

    pub fn is_pinned(&self, path: &str) -> bool {
        self.pinned.iter().any(|p| norm(&p.path) == norm(path))
    }

    /// Pins `path` before index `at` (the end when past it). False when it is
    /// already pinned.
    pub fn pin_at(&mut self, path: &str, at: usize) -> bool {
        if path.is_empty() || self.is_pinned(path) {
            return false;
        }
        let at = at.min(self.pinned.len());
        self.pinned.insert(at, Pinned { name: default_label(path), path: path.to_string() });
        self.dirty = true;
        true
    }

    pub fn unpin(&mut self, i: usize) -> bool {
        if i >= self.pinned.len() {
            return false;
        }
        self.pinned.remove(i);
        self.dirty = true;
        self.kb = self.kb.filter(|&k| k != i).map(|k| if k > i { k - 1 } else { k });
        true
    }

    /// Moves bookmark `from` into the gap `slot` (index in the list before the move).
    pub fn reorder_to(&mut self, from: usize, slot: usize) -> Option<usize> {
        if from >= self.pinned.len() {
            return None;
        }
        let to = dnd::reorder_target(from, slot.min(self.pinned.len()));
        if to == from {
            return None;
        }
        let p = self.pinned.remove(from);
        self.pinned.insert(to, p);
        self.dirty = true;
        if self.kb == Some(from) {
            self.kb = Some(to);
        }
        Some(to)
    }

    /// Alt+Up / Alt+Down: one place up or down.
    pub fn move_by(&mut self, i: usize, delta: i32) -> Option<usize> {
        let to = i as i32 + delta;
        if to < 0 || to as usize >= self.pinned.len() {
            return None;
        }
        self.pinned.swap(i, to as usize);
        self.dirty = true;
        if self.kb == Some(i) {
            self.kb = Some(to as usize);
        }
        Some(to as usize)
    }

    pub fn rename_pinned(&mut self, i: usize, name: &str) -> bool {
        let name = name.trim();
        match self.pinned.get_mut(i) {
            Some(p) if !name.is_empty() && p.name != name => {
                p.name = name.to_string();
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// A folder was opened: it goes to the top of Recent (no duplicates,
    /// `RECENT_CAP` kept). Drive roots count too.
    pub fn push_recent(&mut self, path: &str) -> bool {
        if path.is_empty() || self.recents.first().is_some_and(|p| norm(p) == norm(path)) {
            return false;
        }
        self.recents.retain(|p| norm(p) != norm(path));
        self.recents.insert(0, path.to_string());
        self.recents.truncate(RECENT_CAP);
        self.recents_dirty = true;
        true
    }

    /// A folder that could not be opened must not stay in Recent.
    pub fn drop_recent(&mut self, path: &str) -> bool {
        let n = self.recents.len();
        self.recents.retain(|p| norm(p) != norm(path));
        let gone = self.recents.len() != n;
        self.recents_dirty |= gone;
        gone
    }

    pub fn clear_recents(&mut self) {
        if !self.recents.is_empty() {
            self.recents.clear();
            self.recents_dirty = true;
        }
    }

    pub fn toggle_section(&mut self, sec: usize) {
        if let Some(c) = self.closed.get_mut(sec) {
            *c = !*c;
            self.dirty = true;
        }
    }

    /// Names of the folded sections for config.toml.
    pub fn closed_names(&self) -> Vec<String> {
        SECTIONS.iter().zip(self.closed).filter(|(_, c)| *c).map(|((n, _), _)| n.to_string()).collect()
    }

    pub fn set_closed_names(&mut self, names: &[String]) {
        for (i, (n, _)) in SECTIONS.iter().enumerate() {
            self.closed[i] = names.iter().any(|x| x == n);
        }
    }

    pub fn set_rail(&mut self, rail: bool) {
        if self.rail != rail {
            self.rail = rail;
            self.dirty = true;
        }
    }

    fn quick_places(&self) -> Vec<(&'static str, &'static str, &str)> {
        QUICK_NAMES.iter().filter_map(|(n, g)| self.place_path(n).map(|p| (*n, *g, p))).collect()
    }

    fn pc_drives(&self) -> Vec<usize> {
        (0..self.drives.len()).filter(|&i| self.drives[i].kind != DriveKind::Network).collect()
    }

    fn net_drives(&self) -> Vec<usize> {
        (0..self.drives.len()).filter(|&i| self.drives[i].kind == DriveKind::Network).collect()
    }

    /// Which row is the current folder: a pin with this exact path, else a quick
    /// place, else the drive it is on.
    fn current(&self, path: &str) -> Option<Kind> {
        let p = norm(path);
        if let Some(i) = self.pinned.iter().position(|b| norm(&b.path) == p) {
            return Some(Kind::Pinned(i));
        }
        let quick = self.quick_places();
        if let Some(i) = quick.iter().position(|(_, _, q)| norm(q) == p) {
            return Some(Kind::Quick(i));
        }
        let b = path.as_bytes();
        if b.len() >= 2 && b[1] == b':' {
            let l = (b[0] as char).to_ascii_uppercase();
            return self.drives.iter().position(|d| d.letter == l).map(|i| if self.drives[i].kind == DriveKind::Network { Kind::Net(i) } else { Kind::Drive(i) });
        }
        None
    }

    fn path_of(&self, k: Kind) -> Option<String> {
        match k {
            Kind::Pinned(i) => self.pinned.get(i).map(|p| p.path.clone()),
            Kind::Quick(i) => self.quick_places().get(i).map(|(_, _, p)| p.to_string()),
            Kind::Recent(i) => self.recents.get(i).cloned(),
            Kind::Drive(i) | Kind::Net(i) => self.drives.get(i).map(|d| d.root.clone()),
            _ => None,
        }
    }

    /// The rows top to bottom, with their y inside the content. Rail mode has
    /// no headers; sections are told apart by a separator.
    fn items(&self, rail: bool) -> Vec<Item> {
        let mut v = Vec::new();
        let mut y = 4.0;
        let pc = self.pc_drives();
        let net = self.net_drives();
        let ntags = self.tags.as_ref().map_or(0, |t| t.len());
        let counts = [self.pinned.len(), self.quick_places().len(), self.recents.len(), pc.len(), net.len(), ntags];
        let mut first = true;
        for (sec, &count) in counts.iter().enumerate() {
            if rail && (sec == RECENT || sec == TAGS || count == 0) {
                continue;
            }
            if sec == TAGS && self.tags.is_none() {
                continue;
            }
            if rail {
                if !first {
                    add(&mut v, &mut y, Kind::Sep, 9.0);
                }
            } else {
                add(&mut v, &mut y, Kind::Header(sec), 26.0);
            }
            first = false;
            if !rail && self.closed[sec] {
                y += 4.0;
                continue;
            }
            let h = if rail { RAIL_ITEM } else { ROW_H };
            match sec {
                PINNED => (0..count).for_each(|i| add(&mut v, &mut y, Kind::Pinned(i), h)),
                QUICK => (0..count).for_each(|i| add(&mut v, &mut y, Kind::Quick(i), h)),
                RECENT => (0..count).for_each(|i| add(&mut v, &mut y, Kind::Recent(i), h)),
                PC => pc.iter().for_each(|&i| add(&mut v, &mut y, Kind::Drive(i), if rail { RAIL_ITEM } else { 32.0 })),
                TAGS => (0..count).for_each(|i| add(&mut v, &mut y, Kind::Tag(i), ROW_H)),
                _ => net.iter().for_each(|&i| add(&mut v, &mut y, Kind::Net(i), if rail { RAIL_ITEM } else { 32.0 })),
            }
            if count == 0 && !rail {
                add(&mut v, &mut y, Kind::Empty(sec), ROW_H);
            }
            if !rail {
                y += 4.0;
            }
        }
        v
    }

    fn content_h(items: &[Item]) -> f32 {
        items.last().map(|i| i.y + i.h + 4.0).unwrap_or(0.0)
    }

    // ---- drawing ---------------------------------------------------------

    /// Draws the panel in `r`. `current` is the active folder; `dual` lets the
    /// menu offer "open in other pane".
    #[allow(clippy::too_many_arguments)]
    pub fn paint(&mut self, ui: &mut Ui, c: &mut dyn Canvas, r: Rect, current: &str, drag: &DragCtx) -> SideOut {
        let t = ui.theme;
        let mut out = SideOut::default();
        self.tip = None;
        c.fill(r, t.panel);
        c.fill(Rect::new(r.r() - 1.0, r.y, 1.0, r.h), t.border);
        let inner = Rect::new(r.x, r.y, r.w - 1.0, r.h);
        let body = Rect::new(inner.x, inner.y, inner.w, (inner.h - FOOTER_H).max(0.0));
        let items = self.items(self.rail);
        let content = Self::content_h(&items);
        let max_scroll = (content - body.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        let bg = ui.interact(ID.child("bg"), inner, Sense::click().focusable().wheel());
        if bg.wheel.1 != 0.0 {
            self.scroll = (self.scroll - bg.wheel.1 * 3.0 * ROW_H).clamp(0.0, max_scroll);
            ui.request_repaint();
        }
        let focused = ui.has_focus(ID.child("bg"));
        self.keys(ui, focused, &mut out);

        let cur = self.current(current);
        let mouse = ui.mouse();
        let dragging = drag.payload.is_some();
        // drop target under the pointer while files are dragged in
        let mut target: Option<Kind> = None;
        let mut pin_slot: Option<usize> = None;
        let pinned_rows: Vec<&Item> = items.iter().filter(|i| matches!(i.kind, Kind::Pinned(_))).collect();
        let first_y = pinned_rows.first().map(|i| i.y - self.scroll + body.y);
        if (dragging || self.reorder.is_some()) && body.contains_pt(mouse) {
            let reorder = self.reorder.is_some();
            let n = self.pinned.len();
            if let Some(fy) = first_y.filter(|_| n > 0)
                && mouse.y >= fy - 8.0
                && mouse.y < fy + n as f32 * ROW_H + 8.0
                && !self.rail
            {
                match dnd::pin_slot(mouse.y - fy, ROW_H, n, reorder) {
                    PinSlot::Between(k) => pin_slot = Some(k),
                    PinSlot::OnRow(i) => target = Some(Kind::Pinned(i)),
                }
            } else if dragging && self.pinned.is_empty() && drag.folder.is_some() && !self.rail
                && let Some(h) = items.iter().find(|i| i.kind == Kind::Header(PINNED))
            {
                let hr = Rect::new(inner.x, h.y - self.scroll + body.y, inner.w, h.h + ROW_H);
                if hr.contains_pt(mouse) {
                    pin_slot = Some(0);
                }
            }
            if target.is_none() && pin_slot.is_none() && dragging {
                target = items.iter().find(|i| !matches!(i.kind, Kind::Header(_) | Kind::Sep | Kind::Empty(_) | Kind::Pinned(_)) && Rect::new(inner.x, i.y - self.scroll + body.y, inner.w, i.h).contains_pt(mouse)).map(|i| i.kind);
            }
        }

        c.clip(body);
        let mut act: Option<Act> = None;
        for item in &items {
            let ir = Rect::new(inner.x, item.y - self.scroll + body.y, inner.w, item.h);
            if ir.b() < body.y || ir.y > body.b() {
                continue;
            }
            match item.kind {
                Kind::Header(sec) => {
                    if self.header(ui, c, &t, ir, sec) {
                        ui.request_repaint();
                    }
                }
                Kind::Sep => c.fill(Rect::new(ir.x + 10.0, ir.y + 4.0, ir.w - 20.0, 1.0), t.border),
                Kind::Tag(i) => {
                    let is_target = target == Some(item.kind) && dragging;
                    if let Some(a) = self.tag_row(ui, c, &t, ir, i, is_target) {
                        act = Some(a);
                    }
                }
                Kind::Empty(sec) => {
                    let msg = match sec {
                        PINNED => "Drag a folder here to pin it",
                        RECENT => "No recent folders",
                        NETWORK => "No network locations",
                        TAGS => "No tags",
                        _ => "Nothing here",
                    };
                    c.text(msg, Rect::new(ir.x + 32.0, ir.y, ir.w - 36.0, ir.h), t.font_small(), t.text_disabled);
                }
                kind => {
                    let is_cur = cur == Some(kind);
                    let is_target = target == Some(kind) && dragging;
                    if let Some(a) = self.row(ui, c, &t, ir, kind, is_cur, is_target, focused && self.kb.is_some() && Some(kind) == self.kb.map(Kind::Pinned)) {
                        act = Some(a);
                    }
                }
            }
        }
        // insertion line for a pin or a reorder
        if let Some(k) = pin_slot
            && (drag.folder.is_some() || self.reorder.is_some())
        {
            let y = match (first_y, k) {
                (Some(fy), k) => fy + k as f32 * ROW_H,
                (None, _) => items.iter().find(|i| i.kind == Kind::Header(PINNED)).map(|h| h.y - self.scroll + body.y + h.h + 2.0).unwrap_or(body.y),
            };
            c.fill(Rect::new(inner.x + 10.0, y - 1.0, inner.w - 20.0, 2.0), t.accent);
            c.circle(inner.x + 10.0, y, 3.0, t.accent);
        }
        c.unclip();

        // pinned row dragged: reorder on release
        if let Some(from) = self.reorder
            && !ui.is_active(ID.child("pin").with(from as u64))
        {
            self.reorder = None;
            if let Some(k) = pin_slot {
                self.reorder_to(from, k);
            }
            ui.request_repaint();
        }

        if std::mem::take(&mut self.new_tag) {
            act = Some(Act::NewTag);
        }
        self.footer(ui, c, &t, Rect::new(inner.x, body.b(), inner.w, FOOTER_H), &mut act);
        self.resize_edge(ui, c, &t, r, &mut act);

        // a drop: the button went up over a target
        if dragging && drag.released {
            if let Some(k) = pin_slot
                && let Some(folder) = drag.folder
            {
                if self.pin_at(folder, k) {
                    ui.request_repaint();
                }
            } else if let Some(k) = target
                && let Some(p) = self.path_of(k)
            {
                act = Some(Act::DropOn(p));
            }
        }
        if dragging {
            out.target = match (pin_slot, target) {
                (Some(k), _) if drag.folder.is_some() => Some(DropTarget::Pin(k)),
                (_, Some(Kind::Tag(i))) => self.tags.as_ref().and_then(|v| v.get(i)).map(|tg| DropTarget::Tag(tg.slot)),
                (_, Some(k)) => self.path_of(k).map(DropTarget::Folder),
                _ => None,
            };
        }
        out.act = act;
        out
    }

    /// Section header: chevron, title, "Clear" on Recent. True when it folded.
    fn header(&mut self, ui: &mut Ui, c: &mut dyn Canvas, t: &Theme, r: Rect, sec: usize) -> bool {
        let it = ui.interact(ID.child("sec").with(sec as u64), r, Sense::click());
        let mut changed = false;
        let on_plus = sec == TAGS && ui.mouse().x >= r.r() - 26.0;
        if it.clicked == Some(Button::Left) && !on_plus {
            self.toggle_section(sec);
            changed = true;
        }
        let chev = if self.closed[sec] { G_CHEV_RIGHT } else { G_CHEV_DOWN };
        c.text(chev, Rect::new(r.x + 6.0, r.y, 14.0, r.h), f(8.0).icon().c(), t.text_dim);
        c.text(SECTIONS[sec].1, Rect::new(r.x + 24.0, r.y, r.w - 30.0, r.h), t.font(), t.text_dim);
        if sec == TAGS {
            let b = Rect::new(r.r() - 26.0, r.y + 3.0, 20.0, r.h - 6.0);
            let hit = ui.interact(ID.child("newtag"), b, Sense::click().cursor(Cursor::Hand));
            if hit.hovered {
                c.round(b, t.radius_sm, t.hover);
                self.tip = Some((b, "New tag".into()));
            }
            c.text("\u{E710}", b, f(10.0).icon().c(), t.text_dim);
            if hit.clicked == Some(Button::Left) {
                self.new_tag = true;
                changed = true;
            }
        }
        if sec == RECENT && !self.recents.is_empty() && !self.closed[sec] {
            let w = c.measure("Clear", t.font_small()) + 12.0;
            let b = Rect::new(r.r() - w - 6.0, r.y + 3.0, w, r.h - 6.0);
            let hit = ui.interact(ID.child("clear"), b, Sense::click().cursor(Cursor::Hand));
            if hit.hovered {
                c.round(b, t.radius_sm, t.hover);
            }
            c.text("Clear", b, t.font_small().c(), t.text_dim);
            if hit.clicked == Some(Button::Left) {
                self.clear_recents();
                changed = true;
            }
        }
        changed
    }

    /// One row. Returns what a click asked for.
    #[allow(clippy::too_many_arguments)]
    fn row(&mut self, ui: &mut Ui, c: &mut dyn Canvas, t: &Theme, r: Rect, kind: Kind, active: bool, drop_target: bool, key_focus: bool) -> Option<Act> {
        let id = match kind {
            Kind::Pinned(i) => ID.child("pin").with(i as u64),
            Kind::Quick(i) => ID.child("q").with(i as u64),
            Kind::Recent(i) => ID.child("r").with(i as u64),
            Kind::Drive(i) => ID.child("d").with(i as u64),
            Kind::Net(i) => ID.child("n").with(i as u64),
            _ => return None,
        };
        let path = self.path_of(kind)?;
        let it = ui.interact(id, r, Sense::click().cursor(Cursor::Hand));
        let mouse = ui.mouse();
        let hover = it.hovered && self.reorder.is_none();
        let mut act = None;
        let ghost = matches!(kind, Kind::Pinned(i) if self.reorder == Some(i));
        let rr = if self.rail { Rect::new(r.x + 6.0, r.y, r.w - 12.0, r.h) } else { Rect::new(r.x + 4.0, r.y, r.w - 8.0, r.h) };
        if drop_target {
            c.round(rr, t.radius_md, t.selected);
            c.round_stroke(rr.inset(1.0, 1.0), t.radius_md, t.focus, 1.0);
        } else if active {
            c.round(rr, t.radius_md, t.selected_inactive);
            c.round(Rect::new(rr.x, rr.y + 5.0, 3.0, rr.h - 10.0), 1.5, t.accent);
        } else if hover {
            c.round(rr, t.radius_md, t.hover);
        }
        if key_focus {
            c.round_stroke(rr, t.radius_md, t.focus, 1.0);
        }
        let dim = if ghost { 0.4 } else { 1.0 };
        let (glyph, label, sub): (&str, String, Option<String>) = match kind {
            Kind::Pinned(i) => (G_STAR, self.pinned[i].name.clone(), None),
            Kind::Quick(i) => {
                let q = self.quick_places();
                (q[i].1, q[i].0.to_string(), None)
            }
            Kind::Recent(i) => {
                let p = &self.recents[i];
                let parent = crate::nav::parent(p).map(|pp| default_label(&pp));
                (G_RECENT, default_label(p), parent)
            }
            Kind::Drive(i) | Kind::Net(i) => (if matches!(kind, Kind::Net(_)) { G_NET } else { G_DRIVE }, self.drives[i].display_name(), None),
            _ => ("", String::new(), None),
        };
        let icon_col = if active { t.text } else { t.text_dim };
        if self.rail {
            c.text(glyph, rr, f(t.icon_sidebar - 2.0).icon().c(), icon_col);
            if let Kind::Drive(i) | Kind::Net(i) = kind {
                let d = &self.drives[i];
                if d.total > 0 {
                    let bar = Rect::new(rr.x + 6.0, rr.b() - 4.0, rr.w - 12.0, 2.0);
                    c.round(bar, 1.0, t.border);
                    let u = d.used_fraction();
                    c.round(Rect::new(bar.x, bar.y, (bar.w * u).max(2.0), bar.h), 1.0, if u > 0.9 { t.danger } else { t.accent });
                }
            }
            if hover {
                self.tip = Some((rr, label));
            }
        } else if let Kind::Drive(i) | Kind::Net(i) = kind {
            let d = &self.drives[i];
            let top = Rect::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, r.h - 2.0 - t.layout_drive_bar_height - 4.0);
            c.text(glyph, Rect::new(r.x + 12.0, top.y, t.icon_sidebar, top.h), f(t.icon_sidebar - 2.0).icon().c(), icon_col);
            let mut free = String::new();
            if d.total > 0 {
                fmt_volume(d.free, &mut free);
                free.push_str(" free");
            }
            let fw = if free.is_empty() { 0.0 } else { c.measure(&free, t.font_small()) + 8.0 };
            c.text(&label, Rect::new(r.x + 36.0, top.y, (r.w - 40.0 - fw).max(0.0), top.h), t.font(), t.text);
            if d.total > 0 {
                c.text(&free, Rect::new(r.r() - fw - 8.0, top.y, fw, top.h), t.font_small().r(), t.text_dim);
                let bar = Rect::new(r.x + 36.0, r.b() - t.layout_drive_bar_height - 4.0, r.w - 52.0, t.layout_drive_bar_height);
                c.round(bar, 1.5, t.border);
                let u = d.used_fraction();
                c.round(Rect::new(bar.x, bar.y, (bar.w * u).max(3.0), bar.h), 1.5, if u > 0.9 { t.danger } else { t.accent });
            }
        } else {
            let left = r.x + 12.0;
            c.text(glyph, Rect::new(left, r.y, t.icon_sidebar, r.h), f(t.icon_sidebar - 2.0).icon().c(), icon_col);
            let mut right = r.r() - 8.0;
            let renaming = matches!(&self.rename, Some((i, _)) if kind == Kind::Pinned(*i));
            if let Kind::Pinned(i) = kind
                && hover
                && !renaming
            {
                // grip on the left edge, unpin on the right
                for dy in [-3.0f32, 0.0, 3.0] {
                    c.circle(r.x + 8.0, r.y + r.h / 2.0 + dy, 0.9, t.text_dim);
                }
                let ub = Rect::new(r.r() - 26.0, r.y + 2.0, 20.0, r.h - 4.0);
                let hit = ui.interact(ID.child("unpin").with(i as u64), ub, Sense::click().cursor(Cursor::Hand));
                if hit.hovered {
                    c.round(ub, t.radius_sm, t.hover);
                }
                c.text(G_UNPIN, ub, f(11.0).icon().c(), t.text_dim);
                right = ub.x - 2.0;
                if hit.clicked == Some(Button::Left) {
                    self.unpin(i);
                    ui.request_repaint();
                    return None;
                }
            }
            let name_r = Rect::new(r.x + 36.0, r.y, (right - r.x - 36.0).max(0.0), r.h);
            if renaming {
                if let Some((_, input)) = &mut self.rename {
                    self.rename_frames += 1;
                    let fr = Rect::new(r.x + 32.0, r.y + 1.0, r.w - 40.0, r.h - 2.0);
                    let resp = text_input(ui, c, ID.child("rename"), fr, input, "");
                    let done = resp.submitted;
                    let cancel = resp.cancelled;
                    let lost = !resp.focused && self.rename_frames > 2;
                    if done || cancel || lost {
                        let text = input.text.clone();
                        if let Some((i, _)) = self.rename.take()
                            && !cancel
                        {
                            self.rename_pinned(i, &text);
                        }
                        ui.request_repaint();
                    }
                }
            } else {
                match sub {
                    Some(s) => {
                        let lw = c.measure(&label, t.font()).min(name_r.w * 0.7);
                        c.text(&label, Rect::new(name_r.x, name_r.y, lw, name_r.h), t.font(), t.text);
                        c.text(&s, Rect::new(name_r.x + lw + 6.0, name_r.y, (name_r.w - lw - 6.0).max(0.0), name_r.h), t.font_small(), t.text_dim);
                    }
                    None => c.text(&label, name_r, t.font(), if ghost { t.text_dim } else { t.text }),
                }
            }
        }
        let _ = dim;

        // pinned rows: press-drag reorders, right-click menu, F2 rename by key
        if let Kind::Pinned(i) = kind {
            if let Some(p) = it.pressed
                && p.button == Button::Left
            {
                self.press = Some((i, p.pos));
                self.kb = Some(i);
                ui.set_focus(Some(ID.child("bg")));
            }
            if it.active
                && self.reorder.is_none()
                && let Some((pi, p0)) = self.press
                && pi == i
                && ((mouse.x - p0.x).powi(2) + (mouse.y - p0.y).powi(2)).sqrt() > 5.0
                && !self.rail
            {
                self.reorder = Some(i);
                ui.request_repaint();
            }
            if it.clicked == Some(Button::Right) {
                self.kb = Some(i);
                self.menu = Some((i, it.click_pos));
                ui.open_popup(ID_MENU);
            }
        }
        if it.clicked == Some(Button::Left) && self.reorder.is_none() && !matches!((&self.rename, kind), (Some((i, _)), Kind::Pinned(j)) if *i == j) {
            if !matches!(kind, Kind::Pinned(_)) {
                self.kb = None;
            }
            act = Some(Act::Go(path.clone()));
        }
        if it.clicked == Some(Button::Middle) {
            act = Some(Act::NewTab(path));
        } else if it.clicked == Some(Button::Right) && matches!(kind, Kind::Drive(_)) {
            self.shell_menu = Some((path, it.click_pos));
        }
        act
    }

    /// A Tags row: mark, name, count. A click filters by the tag.
    fn tag_row(&mut self, ui: &mut Ui, c: &mut dyn Canvas, t: &Theme, r: Rect, i: usize, drop_target: bool) -> Option<Act> {
        let tg = self.tags.as_ref()?.get(i)?.clone();
        let it = ui.interact(ID.child("tag").with(tg.slot as u64), r, Sense::click().cursor(Cursor::Hand));
        let rr = Rect::new(r.x + 4.0, r.y, r.w - 8.0, r.h);
        if drop_target {
            c.round(rr, t.radius_md, t.selected);
            c.round_stroke(rr.inset(1.0, 1.0), t.radius_md, t.focus, 1.0);
        } else if tg.on {
            c.round(rr, t.radius_md, t.selected_inactive);
            c.round(Rect::new(rr.x, rr.y + 5.0, 3.0, rr.h - 10.0), 1.5, t.accent);
        } else if it.hovered {
            c.round(rr, t.radius_md, t.hover);
        }
        crate::tags::marks::draw(c, tg.mark, tg.rgb, r.x + 12.0 + 8.0, r.y + r.h / 2.0, 11.0);
        let n = crate::tags::ui::sep(tg.count);
        let cw = c.measure(&n, f(t.font_size_xs)) + 4.0;
        c.text(&n, Rect::new(r.r() - 10.0 - cw, r.y, cw, r.h), f(t.font_size_xs).r(), if tg.on { t.text } else { t.text_dim });
        let font = if tg.on { t.font().b() } else { t.font() };
        c.text(&tg.name, Rect::new(r.x + 36.0, r.y, (r.w - 36.0 - cw - 14.0).max(0.0), r.h), font, t.text);
        (it.clicked == Some(Button::Left)).then_some(Act::TagFilter(tg.slot))
    }

    /// Keys while the panel has the focus: F2, Delete, arrows, Enter.
    fn keys(&mut self, ui: &mut Ui, focused: bool, out: &mut SideOut) {
        if !focused || self.rename.is_some() {
            return;
        }
        for (qi, ev) in ui.key_input(ID.child("bg")) {
            let tsubame_ui::Event::KeyDown { key, mods, .. } = ev else { continue };
            let Some(k) = self.kb.filter(|&k| k < self.pinned.len()) else { continue };
            match (key, mods.ctrl, mods.shift, mods.alt) {
                (Key::F(2), false, false, false) => {
                    ui.consume(qi);
                    self.begin_rename(k);
                }
                (Key::Delete, false, false, false) => {
                    ui.consume(qi);
                    self.unpin(k);
                }
                (Key::Up, false, false, false) => {
                    ui.consume(qi);
                    self.kb = Some(k.saturating_sub(1));
                }
                (Key::Down, false, false, false) => {
                    ui.consume(qi);
                    self.kb = Some((k + 1).min(self.pinned.len() - 1));
                }
                (Key::Enter, false, false, false) => {
                    ui.consume(qi);
                    out.act = Some(Act::Go(self.pinned[k].path.clone()));
                }
                (Key::Enter, true, false, false) => {
                    ui.consume(qi);
                    out.act = Some(Act::NewTab(self.pinned[k].path.clone()));
                }
                (Key::Enter, true, true, false) => {
                    ui.consume(qi);
                    out.act = Some(Act::OtherPane(self.pinned[k].path.clone()));
                }
                _ => {}
            }
        }
    }

    pub fn begin_rename(&mut self, i: usize) {
        if let Some(p) = self.pinned.get(i) {
            let mut input = TextInput::new(&p.name);
            input.select_all();
            self.rename = Some((i, input));
            self.rename_frames = 0;
        }
    }

    /// The bookmark the keyboard works on, when the panel has the focus.
    #[allow(dead_code)]
    pub fn kb_target(&self, ui: &Ui) -> Option<usize> {
        self.kb.filter(|&k| k < self.pinned.len() && ui.has_focus(ID.child("bg")))
    }

    /// Footer: collapse to the rail / expand from it.
    fn footer(&mut self, ui: &mut Ui, c: &mut dyn Canvas, t: &Theme, r: Rect, act: &mut Option<Act>) {
        c.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        let it = ui.interact(ID.child("footer"), r, Sense::click().cursor(Cursor::Hand));
        if it.hovered {
            c.fill(Rect::new(r.x, r.y + 1.0, r.w, r.h - 1.0), t.hover);
        }
        if self.rail {
            c.text(G_CHEV_RIGHT, r, f(11.0).icon().c(), t.text_dim);
            if it.hovered {
                self.tip = Some((r, format!("Expand  {}", commands::first_key_label(commands::CMD_SIDEBAR))));
            }
        } else {
            c.text(G_COLLAPSE, Rect::new(r.x + 12.0, r.y, 16.0, r.h), f(11.0).icon().c(), t.text_dim);
            c.text("Collapse", Rect::new(r.x + 36.0, r.y, 100.0, r.h), t.font(), t.text_dim);
            overlay::keycap_right(c, t, r.r() - 10.0, r.y + r.h / 2.0, &commands::first_key_label(commands::CMD_SIDEBAR));
        }
        if it.clicked == Some(Button::Left) {
            self.set_rail(!self.rail);
            *act = Some(Act::Layout);
            ui.request_repaint();
        }
    }

    /// The 5 px handle on the right edge: drag to resize, double-click for
    /// the default width, below `SNAP_W` it becomes the rail.
    fn resize_edge(&mut self, ui: &mut Ui, c: &mut dyn Canvas, t: &Theme, r: Rect, act: &mut Option<Act>) {
        let edge = Rect::new(r.r() - 3.0, r.y, 5.0, r.h);
        let it = ui.interact(ID.child("resize"), edge, Sense::click().cursor(Cursor::SizeWE));
        if it.pressed.is_some_and(|p| p.double) {
            self.rail = false;
            self.dirty = true;
            *act = Some(Act::Layout);
            self.want_width = Some(tsubame_config::DEFAULT_SIDEBAR_WIDTH);
            return;
        }
        self.resizing = it.active;
        if it.active {
            let x = ui.mouse().x - r.x;
            match resize_rule(x) {
                Resize::Rail => {
                    self.set_rail(true);
                    self.want_width = None;
                }
                Resize::Width(w) => {
                    self.rail = false;
                    self.dirty = true;
                    self.want_width = Some(w);
                }
            }
            *act = Some(Act::Layout);
            // the live width chip
            let label = if self.rail { "Icons".to_string() } else { format!("{} px", self.want_width.unwrap_or(r.w) as i32) };
            let w = c.measure(&label, t.font_small()) + 14.0;
            let chip = Rect::new(r.r() + 6.0, ui.mouse().y.clamp(r.y + 14.0, r.b() - 14.0) - 10.0, w, 20.0);
            c.round(chip, t.radius_md, t.popup);
            c.round_stroke(chip, t.radius_md, t.accent, 1.0);
            c.text(&label, chip, t.font_small().c(), t.text);
        } else if it.hovered {
            c.fill(Rect::new(r.r() - 2.0, r.y, 2.0, r.h), t.accent);
        }
    }

    /// Menu and tooltips: call after everything else is drawn.
    pub fn paint_overlay(&mut self, ui: &mut Ui, c: &mut dyn Canvas, dual: bool) -> Option<Act> {
        let t = ui.theme;
        let win = ui.size();
        if let Some((r, text)) = &self.tip
            && !ui.is_popup_open(ID_MENU)
            && !self.resizing
        {
            overlay::tip_right_of(c, &t, win, *r, text);
        }
        let (i, at) = self.menu?;
        if !ui.is_popup_open(ID_MENU) {
            self.menu = None;
            return None;
        }
        let n = self.pinned.len();
        let k = |id| commands::first_key_label(id);
        let _ = k;
        let items = [
            MenuItem::new("Open", "Enter"),
            MenuItem::new("Open in new tab", "Ctrl+Enter"),
            MenuItem::new("Open in other pane", "Ctrl+Shift+Enter").off(!dual),
            MenuItem::new("Rename", "F2").sep(),
            MenuItem::new("Move up", "Alt+Up").off(i == 0),
            MenuItem::new("Move down", "Alt+Down").off(i + 1 >= n),
            MenuItem::new("Unpin", "Del").sep().danger(),
        ];
        let picked = overlay::menu(ui, c, ID_MENU, at, &items)?;
        self.menu = None;
        let path = self.pinned.get(i)?.path.clone();
        match picked {
            0 => return Some(Act::Go(path)),
            1 => return Some(Act::NewTab(path)),
            2 => return Some(Act::OtherPane(path)),
            3 => self.begin_rename(i),
            4 => {
                self.move_by(i, -1);
            }
            5 => {
                self.move_by(i, 1);
            }
            _ => {
                self.unpin(i);
            }
        }
        ui.request_repaint();
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins(names: &[&str]) -> Sidebar {
        let mut s = Sidebar::default();
        for (i, n) in names.iter().enumerate() {
            s.pin_at(&format!("C:\\{n}"), i);
        }
        s.dirty = false;
        s
    }

    fn labels(s: &Sidebar) -> Vec<&str> {
        s.pinned.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn pin_dedupes_and_orders() {
        let mut s = pins(&["a", "b", "c"]);
        assert_eq!(labels(&s), ["a", "b", "c"]);
        assert!(!s.pin_at("c:\\A\\", 0), "same folder, any case, trailing slash");
        assert!(s.pin_at("C:\\x", 1));
        assert_eq!(labels(&s), ["a", "x", "b", "c"]);
        assert!(s.pin_at("C:\\end", 99));
        assert_eq!(labels(&s).last(), Some(&"end"));
        assert!(s.dirty);
        assert!(!s.pin_at("", 0));
    }

    #[test]
    fn unpin_and_reorder() {
        let mut s = pins(&["a", "b", "c", "d"]);
        assert_eq!(s.reorder_to(0, 3), Some(2));
        assert_eq!(labels(&s), ["b", "c", "a", "d"]);
        assert_eq!(s.reorder_to(2, 2), None, "own gap");
        assert_eq!(s.reorder_to(2, 3), None, "the gap after itself");
        assert_eq!(s.reorder_to(3, 0), Some(0));
        assert_eq!(labels(&s), ["d", "b", "c", "a"]);
        assert_eq!(s.move_by(1, -1), Some(0));
        assert_eq!(labels(&s), ["b", "d", "c", "a"]);
        assert_eq!(s.move_by(0, -1), None);
        assert_eq!(s.move_by(3, 1), None);
        assert!(s.unpin(1));
        assert_eq!(labels(&s), ["b", "c", "a"]);
        assert!(!s.unpin(9));
    }

    #[test]
    fn keyboard_row_follows_moves() {
        let mut s = pins(&["a", "b", "c"]);
        s.kb = Some(2);
        s.move_by(2, -1);
        assert_eq!(s.kb, Some(1));
        s.unpin(0);
        assert_eq!(s.kb, Some(0));
        s.unpin(0);
        assert_eq!(s.kb, None);
    }

    #[test]
    fn rename_keeps_the_path() {
        let mut s = pins(&["a"]);
        assert!(s.rename_pinned(0, "  Work  "));
        assert_eq!(s.pinned[0].name, "Work");
        assert_eq!(s.pinned[0].path, "C:\\a");
        assert!(!s.rename_pinned(0, "   "), "empty names are refused");
        assert!(!s.rename_pinned(0, "Work"), "no change");
        assert!(!s.rename_pinned(5, "x"));
        assert_eq!(default_label("D:\\"), "D:");
        assert_eq!(default_label("C:\\Users\\me\\Documents"), "Documents");
    }

    #[test]
    fn recents_are_newest_first_deduped_and_capped() {
        let mut s = Sidebar::default();
        for i in 0..12 {
            assert!(s.push_recent(&format!("C:\\f{i}")));
        }
        assert_eq!(s.recents.len(), RECENT_CAP);
        assert_eq!(s.recents[0], "C:\\f11");
        assert!(!s.push_recent("c:\\F11\\"), "already first");
        assert!(s.push_recent("C:\\f9"));
        assert_eq!(s.recents[0], "C:\\f9");
        assert_eq!(s.recents.iter().filter(|p| p.eq_ignore_ascii_case("C:\\f9")).count(), 1);
        s.recents_dirty = false;
        s.clear_recents();
        assert!(s.recents.is_empty() && s.recents_dirty);
    }

    #[test]
    fn resize_rule_clamps_and_snaps() {
        assert_eq!(resize_rule(60.0), Resize::Rail);
        assert_eq!(resize_rule(119.0), Resize::Rail);
        assert_eq!(resize_rule(120.0), Resize::Width(140.0));
        assert_eq!(resize_rule(208.0), Resize::Width(208.0));
        assert_eq!(resize_rule(900.0), Resize::Width(360.0));
    }

    #[test]
    fn fold_state_round_trips_names() {
        let mut s = Sidebar::default();
        s.toggle_section(RECENT);
        s.toggle_section(NETWORK);
        assert_eq!(s.closed_names(), ["recent", "network"]);
        let mut t = Sidebar::default();
        t.set_closed_names(&s.closed_names());
        assert_eq!(t.closed, s.closed);
        t.set_closed_names(&[]);
        assert_eq!(t.closed, [false; 6]);
    }

    fn drive(letter: char, kind: DriveKind) -> Drive {
        Drive { letter, root: format!("{letter}:\\"), label: String::new(), kind, total: 100, free: 40 }
    }

    #[test]
    fn current_row_prefers_pin_then_place_then_drive() {
        let mut s = Sidebar {
            drives: vec![drive('C', DriveKind::Fixed), drive('Z', DriveKind::Network)],
            places: vec![Place { name: "Downloads", path: "C:\\Users\\me\\Downloads".into() }],
            ..Default::default()
        };
        s.pin_at("C:\\Users\\me\\Downloads", 0);
        assert_eq!(s.current("C:\\Users\\me\\Downloads"), Some(Kind::Pinned(0)));
        s.unpin(0);
        assert_eq!(s.current("c:\\users\\me\\downloads\\"), Some(Kind::Quick(0)));
        assert_eq!(s.current("C:\\Other"), Some(Kind::Drive(0)));
        assert_eq!(s.current("Z:\\share"), Some(Kind::Net(1)));
        assert_eq!(s.current("\\\\srv\\x"), None);
        assert_eq!(s.volume_of("C:\\x"), Some((40, 100)));
    }

    #[test]
    fn rail_has_no_headers_and_full_has_all_sections() {
        let mut s = Sidebar {
            drives: vec![drive('C', DriveKind::Fixed)],
            places: vec![Place { name: "Home", path: "C:\\Users\\me".into() }],
            ..Default::default()
        };
        s.pin_at("C:\\a", 0);
        s.push_recent("C:\\r");
        let rail = s.items(true);
        assert!(!rail.iter().any(|i| matches!(i.kind, Kind::Header(_))));
        assert!(!rail.iter().any(|i| matches!(i.kind, Kind::Recent(_))));
        assert_eq!(rail.iter().filter(|i| i.kind == Kind::Sep).count(), 2, "between pinned, quick and drives");
        assert!(rail.iter().filter(|i| !matches!(i.kind, Kind::Sep)).all(|i| i.h == RAIL_ITEM));
        let full = s.items(false);
        assert_eq!(full.iter().filter(|i| matches!(i.kind, Kind::Header(_))).count(), 5);
        // rows are a fixed 24 px, drives 32
        assert!(full.iter().filter(|i| matches!(i.kind, Kind::Pinned(_) | Kind::Quick(_) | Kind::Recent(_))).all(|i| i.h == 24.0));
        assert!(full.iter().filter(|i| matches!(i.kind, Kind::Drive(_))).all(|i| i.h == 32.0));
        // a folded section keeps only its header
        s.toggle_section(QUICK);
        assert!(!s.items(false).iter().any(|i| matches!(i.kind, Kind::Quick(_))));
        // rows never overlap
        let items = s.items(false);
        for w in items.windows(2) {
            assert!(w[1].y >= w[0].y + w[0].h - 0.01);
        }
    }
}
