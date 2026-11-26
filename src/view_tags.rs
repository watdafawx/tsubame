//! P4.F6 glue between the view and the tags UI (`crate::tags::ui`): the
//! settings and extension switch, the sidebar section, the picker, the
//! filter results in place of a listing, the banner, the path-bar crumbs and
//! the status bar. The tags UI owns its state; this file only routes.

use super::*;
use crate::sidebar::SideTag;
use crate::tags::model::Edit;
use crate::tags::ui::{Act as TagAct, ID_CHIP_X, MAX_PICK, join, name_hash16};
use crate::tags::worker::Req;
use crate::tags::{self, marks};

impl View {
    /// `[tags]` and the extension switch -> the tags UI (and the column set).
    pub(super) fn sync_tags(&mut self) {
        let on = tags::enabled();
        let path = if self.env.config_path.as_os_str().is_empty() { std::path::PathBuf::new() } else { self.env.config_path.with_file_name("tags.db") };
        self.tags.configure(path, on, self.prefs.tags.shapes, Some(self.wake.clone()));
        tags::set_column_shown(on && self.prefs.tags.column);
        marks::set_scale(self.dpi / 96.0);
    }

    /// Once a frame, before the body: opens the store on first use (on its
    /// worker) and gives the sidebar its Tags rows.
    pub(super) fn tags_frame(&mut self) {
        if !self.tags.on() {
            self.side.tags = None;
            return;
        }
        self.tags.ensure();
        marks::set_scale(self.dpi / 96.0);
        let dark = self.dark;
        let shapes = self.tags.shapes;
        let filt = self.tags.filter_slot();
        self.side.tags = Some(match &self.tags.snap {
            None => Vec::new(),
            Some(s) => s
                .defs
                .iter()
                .map(|d| SideTag { slot: d.slot, name: d.name.clone(), rgb: d.rgb(dark), mark: marks::mark_of(d, shapes), count: s.counts[d.slot as usize] as usize, on: filt == Some(d.slot) })
                .collect(),
        });
    }

    /// The files tags act on: the selection, else the cursor row (or the
    /// cursor hit of the filter results). Paths with their file ids.
    pub(super) fn tag_targets(&self) -> Vec<(String, u128)> {
        if let Some(r) = &self.tags.results
            && r.pane == self.live
        {
            return self.tags.result_at(r.list.cursor).map(|h| vec![(h.path.clone(), 0)]).unwrap_or_default();
        }
        let tab = self.tab();
        if tab.synthetic || tab.settings || crate::archive_ui::is_archive_path(&tab.path) {
            return Vec::new();
        }
        let fid = |id: tsubame_core::EntryId, name: &[u16]| tab.tag_fids.get(id as usize).filter(|(_, h)| *h == name_hash16(name)).map_or(0, |x| x.0);
        let mut v = Vec::new();
        for &id in tab.store.view() {
            if tab.sel.contains(id)
                && let Some(name) = tab.store.name(id)
            {
                v.push((join(&tab.path, &String::from_utf16_lossy(name)), fid(id, name)));
                if v.len() >= MAX_PICK {
                    break;
                }
            }
        }
        if v.is_empty() {
            let cur = if self.mode.is_grid() { tab.grid.cursor } else { tab.list.cursor };
            if let Some(id) = tab.store.at(cur)
                && let Some(name) = tab.store.name(id)
            {
                v.push((join(&tab.path, &String::from_utf16_lossy(name)), fid(id, name)));
            }
        }
        v
    }

    /// Alt+T / `tags.pick` / the context menu's "Tags…": opens the picker
    /// under the cursor row (or at `at`).
    pub(crate) fn open_tags_picker(&mut self, ui: &mut Ui, at: Option<Pt>, new: bool) {
        if !self.tags.on() {
            return;
        }
        let files = self.tag_targets();
        if files.is_empty() && !new {
            self.ops.message = Some("Select files to tag".into());
            ui.request_repaint();
            return;
        }
        let at = at.unwrap_or_else(|| {
            let tab = self.tab();
            let (x, y) = if self.mode.is_grid() {
                let (ox, oy) = tab.cell_origin_of(tab.grid.cursor);
                (ox + 12.0, oy + tab.grid.cell_h - (tab.grid.scroll as f32))
            } else {
                (40.0, (tab.row_top_of(tab.list.cursor) - tab.list.scroll) as f32 + tab.list.row_h)
            };
            Pt { x: self.list_r.x + x, y: (self.list_r.y + y).clamp(self.list_r.y, self.list_r.b() - 40.0) }
        });
        self.tags.open_picker(ui, files, at, new);
    }

    /// Draws the picker (top layer, under the context menu) and runs its asks.
    pub(super) fn paint_tags_picker(&mut self, ui: &mut Ui, g: &mut dyn Canvas, size: (f32, f32)) {
        if !self.tags.picker_open() {
            return;
        }
        match self.tags.paint_picker(ui, g, Rect::new(0.0, 0.0, size.0, size.1)) {
            TagAct::None => {}
            TagAct::Manage => self.open_settings(Some(crate::settings::Page::Tags)),
            TagAct::Go(p) => self.go(&p),
            TagAct::Reveal { folder, name } => {
                self.go(&folder);
                self.tab_mut().select_names(&[name]);
            }
        }
        if !self.tags.picker_open() {
            self.focus_list = true;
        }
    }

    /// The filter results shown in place of this pane's listing. True when
    /// they took `main`.
    pub(super) fn paint_tag_results(&mut self, ui: &mut Ui, g: &mut dyn Canvas, main: Rect, t: &Theme) -> bool {
        if !self.tags.results.as_ref().is_some_and(|r| r.pane == self.live) {
            return false;
        }
        self.list_top = main.y + t.height_column_header;
        self.list_r = Rect::new(main.x, self.list_top, main.w, main.h - t.height_column_header);
        match self.tags.paint_results(ui, g, main, t) {
            TagAct::Go(p) => self.go(&p),
            TagAct::Reveal { folder, name } => {
                self.go(&folder);
                self.tab_mut().select_names(&[name]);
            }
            _ => {}
        }
        true
    }

    /// The read-only banner and the reconcile's progress line over the body.
    /// Returns the body left under the banner.
    pub(super) fn paint_tags_banner(&mut self, ui: &mut Ui, g: &mut dyn Canvas, main: Rect, t: &Theme) -> Rect {
        if !self.tags.on() {
            return main;
        }
        let mut m = main;
        let h = self.tags.paint_banner(ui, g, m, t);
        m.cut_top(h);
        m
    }

    pub(super) fn paint_tags_progress(&self, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        if self.tags.on() {
            self.tags.paint_progress(g, r, t);
        }
    }

    /// Path bar while the filter shows: "Tags › Urgent" and the chip
    /// "Tag: Urgent ×". True when it drew the bar.
    pub(super) fn paint_tag_crumbs(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bar: Rect, t: &Theme) -> bool {
        let Some(r) = &self.tags.results else { return false };
        if r.pane != self.live {
            return false;
        }
        let slot = r.slot;
        let Some(d) = self.tags.defs().iter().find(|d| d.slot == slot).cloned() else { return false };
        g.round(bar, t.radius_md, t.field);
        g.round_stroke(bar, t.radius_md, t.border, 1.0);
        g.text("\u{E8EC}", Rect::new(bar.x + 8.0, bar.y, 16.0, bar.h), f(13.0).icon().c(), t.text_dim);
        let mut x = bar.x + 8.0 + 16.0 + 6.0 + CRUMB_PAD;
        let font = t.font_md();
        let w = g.measure("Tags", font);
        g.text("Tags", Rect::new(x, bar.y, w + 2.0, bar.h), font, t.text_dim);
        x += w + CRUMB_PAD;
        g.text("\u{E76C}", Rect::new(x, bar.y, SEP_W, bar.h), f(8.0).icon().c(), t.text_dim);
        x += SEP_W + CRUMB_PAD;
        let bold = t.font_md().b();
        let w = g.measure(&d.name, bold);
        g.text(&d.name, Rect::new(x, bar.y, w + 2.0, bar.h), bold, t.text);
        // the chip, right-aligned
        let label = format!("Tag: {}", d.name);
        let lw = g.measure(&label, t.font_small());
        let cw = lw + 8.0 + 14.0 + 22.0;
        let chip = Rect::new(bar.r() - cw - 4.0, bar.y + 4.0, cw, bar.h - 8.0);
        if chip.x > x + w + 8.0 {
            g.round(chip, chip.h / 2.0, t.selected_inactive);
            g.round_stroke(chip, chip.h / 2.0, d.rgb(t.is_dark()), 1.0);
            marks::draw(g, marks::mark_of(&d, self.tags.shapes), d.rgb(t.is_dark()), chip.x + 12.0, chip.y + chip.h / 2.0, 10.0);
            g.text(&label, Rect::new(chip.x + 20.0, chip.y, lw + 2.0, chip.h), t.font_small(), t.text);
            let xr = Rect::new(chip.r() - 20.0, chip.y + 1.0, 18.0, chip.h - 2.0);
            let hit = ui.interact(ID_CHIP_X, xr, Sense::click().cursor(Cursor::Hand));
            if hit.hovered {
                g.round(xr, xr.h / 2.0, t.hover);
            }
            g.text("\u{E711}", xr, f(8.0).icon().c(), t.text_dim);
            if hit.clicked == Some(Button::Left) {
                self.tags.results = None;
                self.focus_list = true;
                ui.request_repaint();
            }
        }
        true
    }

    /// Navigation in a pane ends its tag filter.
    pub(super) fn tags_after_nav(&mut self) {
        if self.tags.results.as_ref().is_some_and(|r| r.pane == self.live) {
            self.tags.results = None;
        }
    }

    /// The sidebar's tag rows: filter, and "+".
    pub(super) fn tags_side_act(&mut self, ui: &mut Ui, act: &crate::sidebar::Act) {
        match act {
            crate::sidebar::Act::TagFilter(slot) => {
                self.tags.filter(*slot, self.live);
                ui.request_repaint();
            }
            crate::sidebar::Act::NewTag => {
                let at = Pt { x: self.list_r.x + 40.0, y: self.list_r.y + 8.0 };
                self.open_tags_picker(ui, Some(at), true);
            }
            _ => {}
        }
    }

    /// Files dropped on a sidebar tag: they get the tag.
    pub fn tag_dropped(&mut self, paths: &[std::path::PathBuf], slot: u8) {
        if !self.tags.writable() {
            self.ops.message = Some("Tags read-only".into());
            return;
        }
        let files: Vec<(String, u128)> = paths.iter().take(MAX_PICK).map(|p| (p.to_string_lossy().into_owned(), 0)).collect();
        let n = files.len();
        let name = self.tags.defs().iter().find(|d| d.slot == slot).map(|d| d.name.clone()).unwrap_or_default();
        self.tags.send(Req::Apply { files, edit: Edit { on: 1u64 << slot, off: 0 } });
        self.tags.say(format!("Tagged {n} item{} {name}", if n == 1 { "" } else { "s" }));
    }

    /// The drag hint over a sidebar tag.
    pub(super) fn tag_drop_hint(&self, slot: u8, count: usize) -> String {
        let name = self.tags.defs().iter().find(|d| d.slot == slot).map_or("", |d| d.name.as_str());
        format!("Tag {count} item{} {name}", if count == 1 { "" } else { "s" })
    }

    /// The status bar's tag part (drawn over the right side of the bar):
    /// results count, reconcile, read-only, the selected file's tags.
    pub(super) fn paint_tags_status(&mut self, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        if !self.tags.on() {
            return;
        }
        let font = t.font_small();
        let res_here = self.tags.results.as_ref().is_some_and(|x| x.pane == self.live);
        let left = if res_here { self.tags.results_status() } else { None };
        let right = self.tags.status();
        if let Some(l) = left {
            // the folder counts do not apply: the results' own count instead
            let lr = Rect::new(r.x + 1.0, r.y + 1.0, 420.0f32.min(r.w * 0.6), r.h - 1.0);
            g.fill(lr, t.panel);
            g.text(&l, Rect::new(r.x + 12.0, r.y + 1.0, lr.w - 12.0, r.h - 1.0), font, t.text_dim);
        }
        if let Some((msg, warn)) = right {
            let w = g.measure(&msg, font) + 4.0;
            let x = (r.r() - 12.0 - w).max(r.x + r.w * 0.4);
            g.fill(Rect::new(x - 8.0, r.y + 1.0, r.r() - x + 8.0, r.h - 1.0), t.panel);
            if warn {
                g.text("\u{E7BA}", Rect::new(x - 18.0, r.y + 1.0, 14.0, r.h - 1.0), f(10.0).icon().c(), t.danger);
            }
            g.text(&msg, Rect::new(x, r.y + 1.0, w, r.h - 1.0), font, t.text);
            return;
        }
        // one selected file: its tags
        if res_here || self.tab().sel.count() != 1 {
            return;
        }
        let Some((path, fid)) = self.tag_targets().into_iter().next() else { return };
        let Some(mask) = self.tags.cached(&path, fid).filter(|m| *m != 0) else { return };
        let defs = self.tags.defs().to_vec();
        let mut idx = Vec::new();
        crate::tags::model::visible_tags(&defs, mask, 8, &mut idx);
        let names: Vec<&str> = idx.iter().map(|&i| defs[i].name.as_str()).collect();
        let text = names.join(", ");
        let tw = g.measure(&text, font) + 4.0;
        let mut buf = String::new();
        let cw = marks::cluster_w(g, t, &defs, mask, 4, &mut buf);
        let x = (r.r() - 12.0 - tw - cw - 6.0).max(r.x + r.w * 0.5);
        g.fill(Rect::new(x - 8.0, r.y + 1.0, r.r() - x + 8.0, r.h - 1.0), t.panel);
        marks::draw_cluster(g, t, &defs, mask, self.tags.shapes, x, r.y + r.h / 2.0 + 0.5, 4, marks::MARK, marks::GAP, &mut buf);
        g.text(&text, Rect::new(x + cw + 6.0, r.y + 1.0, tw, r.h - 1.0), font, t.text_dim);
    }

    /// Our own renames: path-keyed tags (FAT, network) follow them; id-keyed
    /// ones follow by file id anyway.
    pub(super) fn tags_moved(&mut self, pairs: &[(String, String)]) {
        if self.tags.on() && self.tags.has_items() {
            for (from, to) in pairs {
                self.tags.send(Req::Moved { from: from.clone(), to: to.clone() });
            }
        }
    }
}
