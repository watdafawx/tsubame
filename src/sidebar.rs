//! Left panel (per the mock): Quick access (known folders), Drives with
//! free-space bars, and Bookmarks. The drive and folder data comes from a
//! worker (`tsubame_win::places`); until it arrives the panel shows the
//! sections without entries' targets, and clicks do nothing.

use crate::data::fmt_volume;
use tsubame_ui::{Canvas, Cursor, Id, Rect, Sense, Theme, Ui, f};
use tsubame_win::places::{Drive, Place};

const ID: Id = Id::new("sidebar");

/// (place name, glyph)
const QUICK: [(&str, &str); 5] = [("Home", "\u{E80F}"), ("Desktop", "\u{E7F4}"), ("Downloads", "\u{E896}"), ("Documents", "\u{E8A5}"), ("Pictures", "\u{E91B}")];
const DRIVE_GLYPH: &str = "\u{EDA2}";
const BOOKMARK_GLYPH: &str = "\u{E734}";
const SECTIONS: [&str; 3] = ["Quick access", "Drives", "Bookmarks"];
/// Placeholders until bookmarks are stored (they do not navigate).
const BOOKMARKS: [&str; 4] = ["tsubame", "blink", "Minecraft packs", "rig"];

#[derive(Default)]
pub struct Sidebar {
    pub drives: Vec<Drive>,
    pub places: Vec<Place>,
    pub collapsed: [bool; 3],
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

    /// Which entry is current for `path`: a quick-access place (exact match),
    /// else the drive it is on. (section, index)
    fn current(&self, path: &str) -> Option<(usize, usize)> {
        let norm = |s: &str| s.trim_end_matches('\\').to_ascii_lowercase();
        let p = norm(path);
        for (i, (name, _)) in QUICK.iter().enumerate() {
            if self.place_path(name).is_some_and(|q| norm(q) == p) {
                return Some((0, i));
            }
        }
        let b = path.as_bytes();
        if b.len() >= 2 && b[1] == b':' {
            let l = (b[0] as char).to_ascii_uppercase();
            return self.drives.iter().position(|d| d.letter == l).map(|i| (1, i));
        }
        None
    }

    /// Draws the panel in `r`. Returns the folder a click asked for.
    pub fn paint(&mut self, ui: &mut Ui, c: &mut dyn Canvas, r: Rect, current: &str) -> Option<String> {
        let t = ui.theme;
        c.fill(r, t.panel);
        c.fill(Rect::new(r.r() - 1.0, r.y, 1.0, r.h), t.border);
        // everything below clips to the panel
        let inner = Rect::new(r.x, r.y, r.w - 1.0, r.h);
        c.clip(inner);
        let cur = self.current(current);
        let mut go = None;
        let mut y = r.y + t.space_2;
        for (sec, title) in SECTIONS.iter().enumerate() {
            // section header: chevron + title, click folds it
            let hr = Rect::new(r.x, y, inner.w, t.height_sidebar_header);
            let hid = ID.child("sec").with(sec as u64);
            let it = ui.interact(hid, hr, Sense::click());
            if it.clicked == Some(tsubame_ui::Button::Left) {
                self.collapsed[sec] = !self.collapsed[sec];
                ui.request_repaint();
            }
            let chev = if self.collapsed[sec] { "\u{E76C}" } else { "\u{E70D}" };
            c.text(chev, Rect::new(hr.x + 6.0, hr.y, 14.0, hr.h), f(8.0).icon().c(), t.text_dim);
            c.text(title, Rect::new(hr.x + 24.0, hr.y, hr.w - 30.0, hr.h), t.font(), t.text_dim);
            y += hr.h;
            if self.collapsed[sec] {
                y += t.space_2;
                continue;
            }
            match sec {
                0 => {
                    for (i, (name, glyph)) in QUICK.iter().enumerate() {
                        let ir = Rect::new(r.x + 4.0, y, inner.w - 8.0, t.height_sidebar_item);
                        let path = self.place_path(name).map(str::to_string);
                        if item(ui, c, &t, ID.child("q").with(i as u64), ir, glyph, name, cur == Some((0, i)), path.is_some()) {
                            go = path;
                        }
                        y += ir.h;
                    }
                }
                1 => {
                    for i in 0..self.drives.len() {
                        let d = &self.drives[i];
                        let ir = Rect::new(r.x + 4.0, y, inner.w - 8.0, t.height_sidebar_drive);
                        let id = ID.child("d").with(i as u64);
                        let active = cur == Some((1, i));
                        if drive_item(ui, c, &t, id, ir, d, active) {
                            go = Some(d.root.clone());
                        }
                        y += ir.h;
                    }
                    if self.drives.is_empty() {
                        y += t.height_sidebar_item;
                    }
                }
                _ => {
                    for (i, name) in BOOKMARKS.iter().enumerate() {
                        let ir = Rect::new(r.x + 4.0, y, inner.w - 8.0, t.height_sidebar_item);
                        item(ui, c, &t, ID.child("b").with(i as u64), ir, if i == 3 { "\u{E7F8}" } else { BOOKMARK_GLYPH }, name, false, false);
                        y += ir.h;
                    }
                }
            }
            y += t.space_2;
        }
        c.unclip();
        go
    }
}

/// One entry: icon, label; true when clicked (and `enabled`).
#[allow(clippy::too_many_arguments)]
fn item(ui: &mut Ui, c: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, glyph: &str, label: &str, active: bool, enabled: bool) -> bool {
    let it = ui.interact(id, r, Sense::click().cursor(if enabled { Cursor::Hand } else { Cursor::Arrow }));
    if active {
        c.round(r, t.radius_md, t.selected_inactive);
        c.round(Rect::new(r.x, r.y + 5.0, 3.0, r.h - 10.0), 1.5, t.accent);
    } else if it.hovered {
        c.round(r, t.radius_md, t.hover);
    }
    c.text(glyph, Rect::new(r.x + 8.0, r.y, t.icon_sidebar, r.h), f(t.icon_sidebar - 2.0).icon().c(), if active { t.text } else { t.text_dim });
    c.text(label, Rect::new(r.x + 32.0, r.y, r.w - 36.0, r.h), t.font(), t.text);
    enabled && it.clicked == Some(tsubame_ui::Button::Left)
}

fn drive_item(ui: &mut Ui, c: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, d: &Drive, active: bool) -> bool {
    let it = ui.interact(id, r, Sense::click().cursor(Cursor::Hand));
    if active {
        c.round(r, t.radius_md, t.selected_inactive);
        c.round(Rect::new(r.x, r.y + 5.0, 3.0, r.h - 10.0), 1.5, t.accent);
    } else if it.hovered {
        c.round(r, t.radius_md, t.hover);
    }
    let top = Rect::new(r.x, r.y + 2.0, r.w, r.h - 2.0 - t.layout_drive_bar_height - 4.0);
    c.text(DRIVE_GLYPH, Rect::new(r.x + 8.0, top.y, t.icon_sidebar, top.h), f(t.icon_sidebar - 2.0).icon().c(), if active { t.text } else { t.text_dim });
    let mut free = String::new();
    if d.total > 0 {
        fmt_volume(d.free, &mut free);
        free.push_str(" free");
    }
    let fw = if free.is_empty() { 0.0 } else { c.measure(&free, t.font_small()) + 8.0 };
    c.text(&d.display_name(), Rect::new(r.x + 32.0, top.y, (r.w - 36.0 - fw).max(0.0), top.h), t.font(), t.text);
    if !free.is_empty() {
        c.text(&free, Rect::new(r.r() - fw - 4.0, top.y, fw, top.h), t.font_small().r(), t.text_dim);
        // usage bar under the name
        let bar = Rect::new(r.x + 32.0, r.b() - t.layout_drive_bar_height - 4.0, r.w - 44.0, t.layout_drive_bar_height);
        c.round(bar, 1.5, t.border);
        let used = d.used_fraction();
        c.round(Rect::new(bar.x, bar.y, (bar.w * used).max(3.0), bar.h), 1.5, if used > 0.9 { t.danger } else { t.accent });
    }
    it.clicked == Some(tsubame_ui::Button::Left)
}
