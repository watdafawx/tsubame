//! P4.F6 Miller columns, the view's side (`tsubame.ext.miller`, `crate::miller`
//! is the model, `docs/f6-mock.md` the design). One 24 DIP header and one
//! list per column, an info column after a selected file, a single-column
//! drill-down under 560 DIPs.
//!
//! Zero idle cost: nothing here runs on a timer. A frame draws only the
//! columns that intersect the pane and only the rows each list shows; the
//! loading skeleton is a static drawing shown when a worker batch or the
//! one-shot 80 ms wake arrives.
#![allow(clippy::too_many_arguments)]

use super::*;
use crate::miller::{self, Column, Excerpt, MillerModel, State};
use tsubame_ui::{ListResponse, ListState};

const ID_MCOL: Id = Id::new("miller-col");
const ID_MHEAD: Id = Id::new("miller-head");
const ID_MGRIP: Id = Id::new("miller-grip");
const ID_MACT: Id = Id::new("miller-act");
const ID_MCUE: Id = Id::new("miller-cue");
const ID_MBAR: Id = Id::new("miller-bar");
/// Column header height (DIPs).
const HEAD_H: f32 = 24.0;
/// Resize grip width.
const GRIP: f32 = 5.0;
/// The drill-down's parent header.
const DRILL_H: f32 = 28.0;
/// The info strip under the list in the drill-down.
const STRIP_H: f32 = 52.0;
/// Horizontal scrollbar height.
const BAR_H: f32 = 6.0;

/// What a click in a painted column asked for, handled after the model is back in its tab.
#[derive(Default)]
struct Asks {
    /// an activated file (Enter, double click)
    open: Option<String>,
    /// a folder opened in a new tab (middle click)
    new_tab: Option<String>,
    /// a context menu: (row of the active column, point)
    menu: Option<(usize, Option<Pt>)>,
    bg_menu: Option<Pt>,
    /// the preview-off note's button
    settings: bool,
    /// a press landed on a column
    pressed: bool,
}

impl View {
    /// Miller columns are showing (the mode is on and the extension is enabled).
    pub(super) fn miller_rows_on(&self) -> bool {
        self.mode == ViewMode::Miller && miller::enabled()
    }

    /// The mode is Miller and the active tab has no model yet: make it.
    pub(super) fn ensure_miller_now(&mut self) {
        if !self.miller_rows_on() || self.tab().miller.is_some() {
            return;
        }
        let w = self.prefs.miller.sanitized().col_width as f32;
        let t = self.tab_mut();
        t.miller_on = true;
        t.ensure_miller(w);
    }

    /// Miller columns show in the live tab (the model exists).
    pub(super) fn miller_showing(&self) -> bool {
        self.miller_rows_on() && self.tab().miller.is_some()
    }

    /// The extension switch changed: the mode and the models follow.
    pub(super) fn sync_miller_ext(&mut self) {
        if miller::enabled() {
            return;
        }
        if self.mode == ViewMode::Miller {
            self.set_mode(ViewMode::Details);
        }
        for t in &mut self.tabs.items {
            t.leave_miller();
        }
    }

    /// The command of the extension, Backspace in the columns and Esc on a running listing.
    pub(super) fn run_miller(&mut self, cmd: u32) -> bool {
        match cmd {
            CMD_VIEW_MILLER => self.set_mode(ViewMode::Miller),
            CMD_BACKSPACE_UP if self.miller_showing() && !self.tab().settings => {
                let moved = self.tab_mut().miller.as_mut().is_some_and(|m| m.ascend());
                if !moved {
                    return false;
                }
            }
            CMD_ESCAPE if self.miller_showing() => {
                if !self.tab_mut().miller.as_mut().is_some_and(|m| m.cancel_loading()) {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }

    /// Pumps the models of the pane's tabs. UI thread, no disk.
    pub(super) fn pump_millers(&mut self) -> bool {
        let rows = self.miller_rows_on();
        let cfg = self.prefs.miller.sanitized();
        let preview = crate::preview_ui::is_enabled();
        let active = self.tabs.active;
        let mut changed = false;
        let mut moved = false;
        for (i, t) in self.tabs.items.iter_mut().enumerate() {
            t.miller_on = rows;
            if rows {
                if i == active {
                    t.ensure_miller(cfg.col_width as f32);
                    if let Some(m) = t.miller.as_mut() {
                        m.set_default_width(cfg.col_width as f32);
                    }
                }
                changed |= t.pump_miller(preview);
                if i == active && t.follow_miller() {
                    moved = true;
                    changed = true;
                }
                if let Some(msg) = t.miller.as_mut().and_then(|m| m.status.take()) {
                    self.ops.message = Some(msg);
                }
            } else if t.miller.is_some() {
                t.leave_miller();
            }
        }
        if moved {
            // the active column is another folder: the same follow-ups as a navigation
            self.tags_after_nav();
            self.meta.cancel_folder_sizes();
            self.meta.reset_git();
            self.path_error = None;
            let p = self.tab().path.clone();
            self.side.push_recent(&p);
        }
        changed
    }

    /// Left / Right and type-ahead in the columns. True when handled (repaint).
    pub(super) fn miller_keys(&mut self, ui: &mut Ui) -> bool {
        let blocked = self.tab().settings
            || self.palette.is_some()
            || self.search.is_some()
            || self.tags.picker_open()
            || self.preview.quick_look_open()
            || self.editing
            || self.rename.is_some()
            || ui.has_focus(pid(self.live, ID_FILTER));
        if blocked || !self.miller_showing() {
            return false;
        }
        let events: Vec<Event> = ui.leftover().to_vec();
        let mut did = false;
        for ev in events {
            let Some(m) = self.tabs.items[self.tabs.active].miller.as_mut() else { break };
            match ev {
                Event::KeyDown { key: Key::Right, mods, .. } if mods.none() => {
                    did |= m.descend() == miller::Activated::Entered;
                }
                Event::KeyDown { key: Key::Left, mods, .. } if mods.none() => {
                    did |= m.ascend();
                }
                Event::Char(c) if !c.is_control() && c != ' ' => {
                    if let Some(j) = m.type_ahead(c) {
                        let a = m.active;
                        m.select_pos(a, j);
                        if let Some(col) = m.col_mut(a) {
                            col.reveal_row = true;
                        }
                        did = true;
                    }
                }
                _ => {}
            }
        }
        did
    }

    /// Paints the columns (or the drill-down) in `main`.
    pub(super) fn paint_miller_main(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, main: Rect, t: &Theme) {
        let mut area = main;
        let strip_r = area.cut_top(t.height_column_header);
        let body = area;
        self.list_top = body.y;
        self.list_r = body;
        self.sort_strip(ui, g, strip_r, t);
        g.fill(body, t.row);
        let a = self.tabs.active;
        let Some(mut m) = self.tabs.items[a].miller.take() else { return };
        let mut asks = Asks::default();
        if body.w < miller::DRILL_BELOW {
            self.paint_drill(ui, g, shell, &mut m, body, t, &mut asks);
        } else {
            self.paint_columns(ui, g, shell, &mut m, body, t, &mut asks);
        }
        // the model goes back before anything touches the tab again
        if self.tabs.items[a].miller.is_none() {
            self.tabs.items[a].miller = Some(m);
        }
        if asks.pressed {
            if self.other.is_some() && self.live != self.active {
                self.focus_pane = Some(self.live);
            }
            // the pressed column is the active one now: it owns the keyboard
            self.focus_list = true;
        }
        if let Some(r) = asks.menu {
            self.menu_request = Some(r);
            self.menu_pane = Some(self.live);
        }
        if let Some(p) = asks.bg_menu {
            self.bg_menu = Some(p);
            self.menu_pane = Some(self.live);
        }
        if let Some(p) = asks.new_tab {
            self.open_in_new_tab(&p);
        }
        if let Some(p) = asks.open {
            ui.request_repaint();
            if crate::archive_ui::can_browse(&p) {
                let v = crate::archive_ui::archive_root_path(&p);
                self.go(&v);
            } else if crate::archive_ui::is_archive_path(&p) {
                self.ops.message = Some("Read-only archive \u{2014} use 'Extract selected from archive' to extract files".into());
            } else {
                self.open_file(p);
            }
        }
        if asks.settings {
            self.open_settings(Some(crate::settings::Page::Extensions));
        }
        if self.tabs.items[a].miller.as_ref().is_some_and(|m| m.loading() || m.dirty) {
            ui.request_repaint();
        }
    }

    /// Wide layout: the columns side by side, scrolling horizontally.
    fn paint_columns(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, m: &mut MillerModel, body: Rect, t: &Theme, asks: &mut Asks) {
        let n = m.len();
        let mut pos = [0f32; miller::MAX_COLS + 2];
        let mut wid = [0f32; miller::MAX_COLS + 2];
        let mut total = 0.0;
        for i in 0..n.min(miller::MAX_COLS + 1) {
            pos[i] = total;
            wid[i] = m.width(i);
            total += wid[i];
        }
        let info = m.shows_info();
        let info_w = if info { (body.w - total).max(miller::INFO_MIN_W) } else { 0.0 };
        let info_x = total;
        total += info_w;
        let max_h = (total - body.w).max(0.0);
        if m.reveal {
            m.reveal = false;
            let active_left = pos.get(m.active).copied().unwrap_or(0.0);
            m.hscroll = max_h.min(active_left);
        }
        m.hscroll = m.hscroll.clamp(0.0, max_h);
        let hs = m.hscroll;
        let mut sw = body;
        if max_h > 0.0 {
            sw.h -= BAR_H;
        }
        let x0 = body.x - hs;
        g.clip(body);
        let active = m.active;
        let mut head_wheel = 0.0;
        for i in 0..n.min(miller::MAX_COLS + 1) {
            let cx = x0 + pos[i];
            let w = wid[i];
            if cx + w < body.x || cx > body.r() || i >= m.len() {
                continue;
            }
            let r = Rect::new(cx, sw.y, w, sw.h);
            let wheel = self.paint_column(ui, g, shell, m, i, r, i == active, t, asks);
            head_wheel += wheel;
            // the resize grip, on top of the column's right edge
            let gr = Rect::new(r.r() - GRIP, sw.y, GRIP, sw.h);
            let it = ui.interact(pid(self.live, ID_MGRIP).with(i as u64), gr, Sense::click().cursor(Cursor::SizeWE));
            if it.pressed.is_some_and(|p| p.double) {
                m.reset_width(i);
                ui.request_repaint();
            } else if it.active {
                m.set_width(i, ui.mouse().x - cx + GRIP / 2.0);
                ui.request_repaint();
            }
            if it.hovered || it.active {
                g.fill(Rect::new(r.r() - 2.0, sw.y, 2.0, sw.h), t.accent);
            } else {
                g.fill(Rect::new(r.r() - 1.0, sw.y, 1.0, sw.h), t.border);
            }
        }
        if info {
            let r = Rect::new(x0 + info_x, sw.y, info_w, sw.h);
            if r.r() > body.x && r.x < body.r() {
                self.paint_info(ui, g, shell, m, r, t, asks);
            }
        }
        g.unclip();
        if head_wheel != 0.0 {
            // wheel over a header: scroll the strip of columns
            m.hscroll = (m.hscroll - head_wheel * 120.0).clamp(0.0, max_h);
            ui.request_repaint();
        }
        // the horizontal bar
        if max_h > 0.0 {
            let bar = Rect::new(body.x, body.b() - BAR_H, body.w, BAR_H);
            let thumb_w = (body.w * body.w / total).clamp(24.0, body.w);
            let tx = bar.x + (bar.w - thumb_w) * (hs / max_h);
            let it = ui.interact(pid(self.live, ID_MBAR), bar, Sense::click());
            if it.active {
                let frac = ((ui.mouse().x - bar.x - thumb_w / 2.0) / (bar.w - thumb_w).max(1.0)).clamp(0.0, 1.0);
                m.hscroll = frac * max_h;
                ui.request_repaint();
            }
            g.fill(bar, t.panel);
            g.round(Rect::new(tx, bar.y + 1.0, thumb_w, BAR_H - 2.0), 2.0, if it.hovered || it.active { t.text_dim } else { t.border });
        }
        // the overflow cue: how many columns are out of sight on the left
        let hidden = m.dropped + (0..n.min(miller::MAX_COLS + 1)).filter(|i| pos[*i] + wid[*i] <= hs + 0.5).count();
        if hidden > 0 {
            let label = format!("\u{2039} {hidden} hidden");
            let font = t.font_small();
            let w = g.measure(&label, font) + 18.0;
            let cue = Rect::new(body.x + 6.0, body.y + HEAD_H + 6.0, w, 22.0);
            let it = ui.interact(pid(self.live, ID_MCUE), cue, Sense::click());
            g.round(cue, 11.0, if it.hovered { t.selected_inactive } else { t.popup });
            g.round_stroke(cue, 11.0, t.border, 1.0);
            g.text(&label, Rect::new(cue.x + 9.0, cue.y, w, cue.h), font, t.text);
            if it.clicked == Some(Button::Left) {
                if m.hscroll > 0.5 {
                    m.hscroll = 0.0;
                } else {
                    m.ascend_to_dropped();
                }
                ui.request_repaint();
            }
        }
    }

    /// One column: header, then the list or the state card. Returns the wheel
    /// notches seen over its header.
    fn paint_column(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, m: &mut MillerModel, i: usize, r: Rect, active: bool, t: &Theme, asks: &mut Asks) -> f32 {
        let mut area = Rect::new(r.x, r.y, r.w - 1.0, r.h);
        let head = area.cut_top(HEAD_H);
        let mut wheel = 0.0;
        // header
        g.fill(head, t.panel);
        let hit = ui.interact(pid(self.live, ID_MHEAD).with(i as u64), head, Sense::click().wheel());
        wheel += hit.wheel.1 + hit.wheel.0;
        if hit.clicked == Some(Button::Left) && !active {
            m.set_active(i);
            asks.pressed = true;
        }
        let (title, count) = match m.col(i) {
            Some(c) => (c.title().to_string(), count_text(c)),
            None => return wheel,
        };
        let font = t.font_small().b();
        let cw = g.measure(&count, t.font_small()).min(head.w / 2.0);
        g.text(&title, Rect::new(head.x + 8.0, head.y, (head.w - cw - 22.0).max(0.0), head.h), font, if active { t.text } else { t.text_dim });
        g.text(&count, Rect::new(head.r() - cw - 8.0, head.y, cw + 1.0, head.h), t.font_small(), t.text_dim);
        if active {
            g.fill(Rect::new(head.x, head.b() - 1.0, head.w, 1.0), t.accent);
        } else {
            g.fill(Rect::new(head.x, head.b() - 1.0, head.w, 1.0), t.border);
        }
        self.paint_column_body(ui, g, shell, m, i, area, active, t, asks);
        wheel
    }

    /// The part of a column under its header (shared by the drill-down).
    fn paint_column_body(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, m: &mut MillerModel, i: usize, area: Rect, active: bool, t: &Theme, asks: &mut Asks) {
        let Some(col) = m.col(i) else { return };
        let state = col.state.clone();
        let empty = col.shown() == 0;
        let filtered = active && !self.tabs.items[self.tabs.active].filter.is_empty();
        let due = col.loading_due;
        let id_act = pid(self.live, ID_MACT).with(i as u64);
        let small = t.font_small();
        let mid = Rect::new(area.x + 10.0, area.y + 16.0, area.w - 20.0, 20.0);
        match state {
            State::Loading => {
                if due {
                    skeleton(g, t, area);
                }
            }
            State::Denied(msg) => {
                card(g, t, area, "\u{E783}", "Can't open this folder", &msg, true);
                if text_button(ui, g, t, id_act, Rect::new(area.x + 10.0, area.y + 104.0, 64.0, 24.0), "Retry") {
                    m.retry(i);
                    asks.pressed = true;
                }
            }
            State::Cancelled => {
                card(g, t, area, "\u{E711}", "Listing cancelled", "Retry reads the folder again.", false);
                if text_button(ui, g, t, id_act, Rect::new(area.x + 10.0, area.y + 104.0, 64.0, 24.0), "Retry") {
                    m.retry(i);
                    asks.pressed = true;
                }
            }
            State::Loop(target) => {
                card(g, t, area, "\u{E71B}", "Not followed", &format!("This link loops back to {target}."), false);
            }
            State::Link => {
                let target = m.col(i).and_then(|c| c.target.clone());
                let body = match &target {
                    Some(p) => format!("Link to {p}. It opens only when you follow it."),
                    None => "A link. It opens only when you follow it.".to_string(),
                };
                card(g, t, area, "\u{E71B}", "Junction or link", &body, false);
                if text_button(ui, g, t, id_act, Rect::new(area.x + 10.0, area.y + 120.0, 72.0, 24.0), "Follow") {
                    m.follow(i);
                    m.set_active(i);
                    asks.pressed = true;
                }
            }
            State::Done if empty => {
                if filtered {
                    g.text("No items match the filter", mid, small, t.text_dim);
                } else {
                    g.text("This folder is empty", mid, small, t.text_dim);
                }
            }
            State::Done => {
                self.paint_column_list(ui, g, shell, m, i, area, active, t, asks);
            }
        }
    }

    /// The rows of one column through the list widget.
    fn paint_column_list(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, m: &mut MillerModel, i: usize, area: Rect, active: bool, t: &Theme, asks: &mut Asks) {
        let (mut ls, path) = match m.col_mut(i) {
            Some(c) => {
                let mut ls: ListState = c.list.clone();
                ls.row_h = t.height_list_row;
                ls.view_h = area.h;
                if std::mem::take(&mut c.reveal_row)
                    && let Some(p) = c.sel_pos()
                {
                    ls.cursor = p;
                    ls.ensure_visible(p, c.shown());
                }
                (ls, c.path.clone())
            }
            None => return,
        };
        let id = if active { pid(self.live, ID_LIST) } else { pid(self.live, ID_MCOL).with(i as u64) };
        let mouse = ui.mouse();
        let lr: ListResponse = {
            let mut rows = ColRows { m: &mut *m, c: i, active, shell, row: &mut self.rowbuf, mouse };
            list(ui, g, id, area, &mut ls, &mut rows)
        };
        if let Some(c) = m.col_mut(i)
            && c.path == path
        {
            c.list.scroll = ls.scroll;
            c.list.cursor = ls.cursor;
            c.list.view_h = ls.view_h;
            c.list.row_h = ls.row_h;
        }
        if lr.pressed {
            asks.pressed = true;
        }
        if let Some(pos) = lr.activated {
            match m.activate(i, pos) {
                miller::Activated::Open(p) => asks.open = Some(p),
                _ => ui.request_repaint(),
            }
        }
        if let Some(pos) = lr.middle_clicked
            && m.col(i).is_some_and(|c| c.is_folder_at(pos))
            && let Some(name) = m.col(i).and_then(|c| c.entry_name(pos))
        {
            asks.new_tab = Some(nav::join(&path, &name));
        }
        match lr.context {
            Some((Some(row), pt)) => asks.menu = Some((row, Some(pt))),
            Some((None, pt)) => asks.bg_menu = Some(pt),
            None => {}
        }
        if lr.context_key {
            asks.menu = Some((ls.cursor, None));
        }
        if lr.cursor_moved || lr.scrolled {
            ui.request_repaint();
        }
    }

    /// The info column: metadata, then the text excerpt, or why there is none.
    fn paint_info(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, m: &mut MillerModel, r: Rect, t: &Theme, asks: &mut Asks) {
        let Some(info) = m.info().cloned() else { return };
        let area = Rect::new(r.x, r.y, r.w, r.h);
        let mut body = area;
        let head = body.cut_top(HEAD_H);
        g.fill(head, t.panel);
        g.fill(Rect::new(head.x, head.b() - 1.0, head.w, 1.0), t.border);
        g.text("Info", Rect::new(head.x + 8.0, head.y, head.w - 16.0, head.h), t.font_small().b(), t.text_dim);
        let mut y = body.y + 14.0;
        let x = body.x + 14.0;
        let w = (body.w - 28.0).max(0.0);
        // the icon, the name
        let mut rb = RowBuf::new();
        let n = info.name.len().min(rb.name.len());
        rb.name[..n].copy_from_slice(&info.name.as_bytes()[..n]);
        rb.name_len = n;
        rb.kind = data::kind_of(&info.name, false);
        rb.hidden = info.flags & tsubame_core::HIDDEN != 0;
        let dir = nav::parent(&info.path);
        let ir = Rect::new(x, y, 32.0, 32.0);
        let drawn = crate::packs::draw_icon(g, t, &info.name, false, rb.hidden, ir);
        if !drawn {
            match shell.row_icon(&rb, dir.as_deref()) {
                Some(img) => g.image(&img.key, img.w, img.h, &img.px, ir),
                None => g.round_a(Rect::new(ir.x + 6.0, ir.y, 20.0, 28.0), 2.0, t.text_dim, 0.35),
            }
        }
        g.text(&info.name, Rect::new(x + 42.0, y, (w - 42.0).max(0.0), 32.0), t.font_md().b(), t.text);
        y += 44.0;
        let label_font = t.font_small();
        let font = t.font();
        let mut s = String::new();
        let line = |g: &mut dyn Canvas, label: &str, value: &str, y: &mut f32| {
            g.text(label, Rect::new(x, *y, 78.0, 20.0), label_font, t.text_dim);
            g.text(value, Rect::new(x + 82.0, *y, (w - 82.0).max(0.0), 20.0), font, t.text);
            *y += 22.0;
        };
        rb.type_text(&mut s);
        let kind = s.clone();
        line(g, "Type", &kind, &mut y);
        data::fmt_size(info.size, &mut s);
        let size = format!("{s} ({} bytes)", group(info.size));
        line(g, "Size", &size, &mut y);
        data::fmt_date(data::filetime_to_unix(info.mtime), &mut s);
        line(g, "Modified", &s.clone(), &mut y);
        line(g, "Attributes", &miller::attr_text(info.flags), &mut y);
        line(g, "Location", dir.as_deref().unwrap_or(""), &mut y);
        y += 8.0;
        g.fill(Rect::new(x, y, w, 1.0), t.border);
        y += 10.0;
        let rest = Rect::new(x, y, w, (body.b() - y - 8.0).max(0.0));
        match &info.excerpt {
            Excerpt::Off => {
                g.text("Preview is off", Rect::new(x, y, w, 20.0), t.font().b(), t.text);
                g.text("Turn on the preview extension to see the first lines of a file here.", Rect::new(x, y + 22.0, w, 20.0), label_font, t.text_dim);
                if text_button(ui, g, t, pid(self.live, ID_MACT).child("ext"), Rect::new(x, y + 50.0, 150.0, 24.0), "Open Extensions") {
                    asks.settings = true;
                }
            }
            Excerpt::Loading => g.text("Reading\u{2026}", Rect::new(x, y, w, 20.0), label_font, t.text_dim),
            Excerpt::NoText => g.text("No text preview for this file", Rect::new(x, y, w, 20.0), label_font, t.text_dim),
            Excerpt::Failed => g.text("Can't read this file", Rect::new(x, y, w, 20.0), label_font, t.danger),
            Excerpt::Text(text) => {
                if text.is_empty() {
                    g.text("The file is empty", Rect::new(x, y, w, 20.0), label_font, t.text_dim);
                } else {
                    let mono = t.font_mono();
                    g.clip(rest);
                    for (k, l) in text.lines().enumerate() {
                        let ly = rest.y + k as f32 * 17.0;
                        if ly > rest.b() {
                            break;
                        }
                        g.text(l, Rect::new(rest.x, ly, rest.w, 17.0), mono, t.text);
                    }
                    g.unclip();
                }
            }
        }
    }

    /// Under 560 DIPs: one column at a time, a parent header on top.
    fn paint_drill(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, m: &mut MillerModel, body: Rect, t: &Theme, asks: &mut Asks) {
        let mut area = body;
        let head = area.cut_top(DRILL_H);
        g.fill(head, t.panel);
        g.fill(Rect::new(head.x, head.b() - 1.0, head.w, 1.0), t.border);
        let can_up = m.active > 0 || m.dropped > 0;
        let parent_title = if m.active > 0 { m.col(m.active - 1).map(|c| c.title().to_string()).unwrap_or_default() } else { String::new() };
        let back = Rect::new(head.x + 4.0, head.y + 2.0, 84.0_f32.min(head.w / 2.0), head.h - 4.0);
        if can_up {
            let it = ui.interact(pid(self.live, ID_MHEAD).child("back"), back, Sense::click());
            if it.hovered {
                g.round(back, t.radius_sm, t.hover);
            }
            let label = if parent_title.is_empty() { "\u{2039} Up".to_string() } else { format!("\u{2039} {parent_title}") };
            g.text(&label, Rect::new(back.x + 6.0, back.y, back.w - 8.0, back.h), t.font_small(), t.accent);
            if it.clicked == Some(Button::Left) {
                m.ascend();
                asks.pressed = true;
                ui.request_repaint();
            }
        }
        let (title, count) = match m.active_col() {
            Some(c) => (c.title().to_string(), count_text(c)),
            None => return,
        };
        let tx = if can_up { back.r() + 4.0 } else { head.x + 10.0 };
        let cw = g.measure(&count, t.font_small());
        g.text(&title, Rect::new(tx, head.y, (head.r() - tx - cw - 14.0).max(0.0), head.h), t.font_small().b(), t.text);
        g.text(&count, Rect::new(head.r() - cw - 10.0, head.y, cw + 1.0, head.h), t.font_small(), t.text_dim);
        let strip = m.shows_info();
        if strip {
            let s = area.cut_bottom(STRIP_H);
            if let Some(info) = m.info() {
                g.fill(s, t.panel);
                g.fill(Rect::new(s.x, s.y, s.w, 1.0), t.border);
                let mut a = String::new();
                data::fmt_size(info.size, &mut a);
                let mut d = String::new();
                data::fmt_date(data::filetime_to_unix(info.mtime), &mut d);
                g.text(&info.name, Rect::new(s.x + 10.0, s.y + 6.0, s.w - 20.0, 20.0), t.font().b(), t.text);
                g.text(&format!("{a}  \u{00B7}  {d}"), Rect::new(s.x + 10.0, s.y + 28.0, s.w - 20.0, 18.0), t.font_small(), t.text_dim);
            }
        }
        let active = m.active;
        self.paint_column_body(ui, g, shell, m, active, area, true, t, asks);
    }
}

/// "12", "12 of 340" while filtered is not tracked: the count of rows shown.
fn count_text(c: &Column) -> String {
    if c.is_loading() && c.state == State::Loading {
        let d = c.delivered();
        return if d > 0 { format!("{d}\u{2026}") } else { "\u{2026}".to_string() };
    }
    match c.state {
        State::Done => {
            if c.capped {
                format!("{}+", c.shown())
            } else {
                c.shown().to_string()
            }
        }
        _ => String::new(),
    }
}

/// 1234567 as "1,234,567".
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Grey bars where rows will be (a listing that takes more than 80 ms).
fn skeleton(g: &mut dyn Canvas, t: &Theme, area: Rect) {
    let rows = ((area.h / t.height_list_row) as usize).clamp(1, 8);
    for k in 0..rows {
        let y = area.y + k as f32 * t.height_list_row + 6.0;
        let w = (area.w - 54.0) * (0.45 + 0.1 * ((k * 7) % 5) as f32);
        g.round_a(Rect::new(area.x + 10.0, y + 1.0, 14.0, 14.0), 3.0, t.text_dim, 0.14);
        g.round_a(Rect::new(area.x + 32.0, y + 3.0, w.max(24.0), 10.0), 3.0, t.text_dim, 0.14);
    }
}

/// A state card: glyph, title, message (wrapped to one line).
fn card(g: &mut dyn Canvas, t: &Theme, area: Rect, glyph: &str, title: &str, msg: &str, danger: bool) {
    g.text(glyph, Rect::new(area.x + 10.0, area.y + 12.0, 20.0, 20.0), f(16.0).icon().c(), if danger { t.danger } else { t.text_dim });
    g.text(title, Rect::new(area.x + 36.0, area.y + 12.0, (area.w - 46.0).max(0.0), 20.0), t.font().b(), t.text);
    g.text(msg, Rect::new(area.x + 10.0, area.y + 40.0, (area.w - 20.0).max(0.0), 56.0), t.font_small(), if danger { t.danger } else { t.text_dim });
}

/// A small outlined text button. True when clicked.
fn text_button(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, label: &str) -> bool {
    let it = ui.interact(id, r, Sense::click());
    if it.hovered || it.active {
        g.round(r, t.radius_sm, t.hover);
    }
    g.round_stroke(r, t.radius_sm, t.accent, 1.0);
    g.text(label, Rect::new(r.x + 8.0, r.y, r.w - 12.0, r.h), t.font_small(), t.accent);
    it.clicked == Some(Button::Left)
}

/// One column's rows as a list source: icon, name, a chevron for folders.
struct ColRows<'a> {
    m: &'a mut MillerModel,
    c: usize,
    active: bool,
    shell: &'a mut ShellUi,
    row: &'a mut RowBuf,
    mouse: Pt,
}

impl ListSource for ColRows<'_> {
    fn len(&self) -> usize {
        self.m.col(self.c).map_or(0, |c| c.shown())
    }

    fn is_selected(&self, i: usize) -> bool {
        // the active column shows the full selection; the others a quiet path row (draw_row)
        self.active && self.m.col(self.c).is_some_and(|c| c.sel.is_some() && c.store.at(i) == c.sel)
    }

    fn select(&mut self, i: usize, _mods: Mods, by: SelectBy) {
        if by == SelectBy::Space {
            return;
        }
        self.m.select_pos(self.c, i);
    }

    fn draw_row(&mut self, g: &mut dyn Canvas, t: &Theme, i: usize, r: Rect, st: RowState) {
        let Some(col) = self.m.col(self.c) else { return };
        let Some(id) = col.store.at(i) else { return };
        let path_row = !self.active && col.sel == Some(id);
        if path_row {
            g.fill(r, t.selected_inactive);
            g.fill(Rect::new(r.x, r.y, 2.0, r.h), t.accent);
        }
        let row = &mut *self.row;
        row.fill(&col.store, id);
        let is_dir = row.is_dir();
        let link = col.is_link_at(i);
        let _ = st;
        // chevron at the right edge: a folder goes deeper
        let cw = 16.0;
        if is_dir {
            let c = if link { t.text_disabled } else { t.text_dim };
            g.text("\u{E76C}", Rect::new(r.r() - cw - 12.0, r.y, cw, r.h), f(8.0).icon().c(), c);
        }
        let ir = Rect::new(r.x + 10.0, r.y + (r.h - ICON_DIP) / 2.0, ICON_DIP, ICON_DIP);
        if !crate::packs::draw_icon(g, t, row.name_str(), is_dir, row.hidden, ir) {
            match self.shell.row_icon(row, Some(col.path.as_str())) {
                Some(img) => g.image(&img.key, img.w, img.h, &img.px, ir),
                None => {
                    let (ic, colr) = if is_dir { (Rect::new(ir.x, ir.y + 3.0, 16.0, 11.0), t.folder) } else { (Rect::new(ir.x + 2.0, ir.y, 12.0, 16.0), t.text_dim) };
                    g.round_a(ic, 2.0, colr, 0.35);
                }
            }
        }
        if link {
            g.text("\u{E71B}", Rect::new(ir.x + ir.w - 8.0, ir.y + ir.h - 9.0, 10.0, 10.0), f(8.0).icon().c(), t.accent);
        }
        let name_x = ir.r() + 8.0;
        let name_w = (r.r() - name_x - if is_dir { cw + 14.0 } else { 8.0 }).max(0.0);
        let color = if row.hidden { t.text_dim } else { crate::packs::name_color(row.name_str(), is_dir).unwrap_or(t.text) };
        let _ = self.mouse;
        g.text(row.name_str(), Rect::new(name_x, r.y, name_w, r.h), t.font(), color);
    }
}
