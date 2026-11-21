//! Global search UI overlay (P4.F1): Ctrl+Shift+F opens a flat File Pilot-style
//! search popup backed by `tsubame_win::search::GlobalSearch` (`SearchMode::Auto`:
//! Everything IPC when available, else bounded parallel walk).
//!
//! Zero disk I/O on the UI thread, zero idle wake-ups (created lazily on open,
//! dropped on close; stale generations cancel immediately on every keystroke).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use tsubame_ext_api::{Contributions, Extension, ExtensionInfo, Hit, Query, SearchProvider, text};
use tsubame_ui::{
    Button, Canvas, Cursor, Event, Id, Key, Rect, Sense, TextInput, Theme, Ui, f, keycaps_right,
};
use tsubame_win::enumerate::Wake;
use tsubame_win::search::{
    GlobalSearch, SearchBackend, SearchGeneration, SearchHit, SearchMessage, SearchMode,
    SearchQuery, SearchTask,
};

/// Built-in extension id for global search.
pub const EXT_GLOBAL_SEARCH: &str = "tsubame.ext.global_search";

pub const ID_SEARCH_OVERLAY: Id = Id::new("global-search");
const ID_INPUT: Id = Id::new("global-search.input");
const ID_ROW: Id = Id::new("global-search.row");
const ID_SCOPE: Id = Id::new("global-search.scope");

/// Cap on hits kept in the overlay list so huge wildcard searches stay instant.
const MAX_OVERLAY_HITS: usize = 2_000;
/// Maximum UI-thread pump duration per wake.
const PUMP_BUDGET: Duration = Duration::from_millis(4);

/// Built-in extension registering `search.global` (Ctrl+Shift+F) and the
/// `tsubame_ext_api::SearchProvider` adapter.
pub struct GlobalSearchExtension;

impl Extension<crate::ext::App> for GlobalSearchExtension {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(EXT_GLOBAL_SEARCH, "Global search", false)
    }

    fn contribute(&self, c: &mut Contributions<crate::ext::App>) {
        for spec in crate::commands::SEARCH_COMMANDS {
            c.command(spec.clone(), crate::view::View::run_builtin);
        }
        c.search(text("search.global"), text("Global search"), Box::new(GlobalSearchProvider));
    }
}

/// `tsubame_ext_api::SearchProvider` adapter over `tsubame_win::search::GlobalSearch`.
struct GlobalSearchProvider;

impl SearchProvider for GlobalSearchProvider {
    fn search(&self, q: &Query, sink: &mut dyn FnMut(&[Hit]) -> bool) -> Result<(), String> {
        let engine = GlobalSearch::auto();
        let gen_token = SearchGeneration::new();
        let generation = gen_token.next_generation();
        let root = if q.root.is_empty() { None } else { Some(PathBuf::from(q.root)) };
        let sq = SearchQuery {
            text: q.text.to_string(),
            root,
            max_results: q.limit.max(1),
            ..SearchQuery::default()
        };
        let (task, rx) = engine.start(sq, generation, gen_token, None);
        let mut out_err = None;
        while let Ok(msg) = rx.recv() {
            match msg {
                SearchMessage::Batch { hits, .. } => {
                    let mapped: Vec<Hit> = hits
                        .into_iter()
                        .map(|hit| Hit {
                            path: hit.path.to_string_lossy().into_owned(),
                            dir: hit.is_dir,
                            size: hit.size,
                            mtime: hit.modified.max(0) as u64,
                        })
                        .collect();
                    if !sink(&mapped) {
                        task.cancel();
                        break;
                    }
                }
                SearchMessage::Done(d) => {
                    if let Some(e) = d.error
                        && !d.cancelled
                    {
                        out_err = Some(e.to_string());
                    }
                    break;
                }
            }
        }
        task.join();
        match out_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// True when `tsubame.ext.global_search` is enabled in the registry.
pub fn is_enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_GLOBAL_SEARCH).unwrap_or(false))
}

/// Search scope toggled in the overlay chip bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchScope {
    CurrentFolder,
    AllDrives,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            SearchScope::CurrentFolder => "Current folder",
            SearchScope::AllDrives => "All drives (Everything)",
        }
    }

    pub fn toggle(self) -> SearchScope {
        match self {
            SearchScope::CurrentFolder => SearchScope::AllDrives,
            SearchScope::AllDrives => SearchScope::CurrentFolder,
        }
    }
}

/// Action returned by `SearchOverlay::paint` for `View` to execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchAction {
    None,
    Close,
    OpenFolder(String),
    RevealInFolder { folder: String, name: String },
    OpenFile(String),
}

/// Live global-search popup state. Created lazily on open; dropped on close
/// (which cancels any running worker thread immediately).
pub struct SearchOverlay {
    pub input: TextInput,
    pub scope: SearchScope,
    pub folder: String,
    pub hits: Vec<SearchHit>,
    pub sel: usize,
    scroll: f32,
    last_query: String,
    fresh: bool,
    searching: bool,
    backend: Option<SearchBackend>,
    elapsed: Option<Duration>,
    error: Option<String>,
    engine: GlobalSearch,
    gen_token: SearchGeneration,
    active_gen: u64,
    task: Option<SearchTask>,
    rx: Option<Receiver<SearchMessage>>,
    wake: Wake,
    size_buf: String,
}

impl Drop for SearchOverlay {
    fn drop(&mut self) {
        self.cancel_active();
    }
}

impl SearchOverlay {
    pub fn open(folder: String, wake: Wake) -> SearchOverlay {
        Self::with_mode(folder, SearchMode::Auto, wake)
    }

    pub fn with_mode(folder: String, mode: SearchMode, wake: Wake) -> SearchOverlay {
        SearchOverlay {
            input: TextInput::default(),
            scope: SearchScope::CurrentFolder,
            folder,
            hits: Vec::new(),
            sel: 0,
            scroll: 0.0,
            last_query: String::new(),
            fresh: true,
            searching: false,
            backend: None,
            elapsed: None,
            error: None,
            engine: GlobalSearch::new(mode),
            gen_token: SearchGeneration::new(),
            active_gen: 0,
            task: None,
            rx: None,
            wake,
            size_buf: String::with_capacity(16),
        }
    }

    #[cfg(test)]
    pub fn is_searching(&self) -> bool {
        self.searching
    }

    #[cfg(test)]
    pub fn active_generation(&self) -> u64 {
        self.active_gen
    }

    pub fn cancel_active(&mut self) {
        self.gen_token.cancel();
        if let Some(t) = self.task.take() {
            t.cancel();
        }
        self.rx = None;
        self.searching = false;
    }

    /// Updates the query text and immediately restarts the background search
    /// with a fresh generation, cancelling any in-flight search.
    #[cfg(test)]
    pub fn set_query(&mut self, query: &str) {
        self.input.set_text(query);
        self.restart_search();
    }

    /// Switches between `CurrentFolder` and `AllDrives` and re-runs the query.
    pub fn set_scope(&mut self, scope: SearchScope) {
        if self.scope != scope {
            self.scope = scope;
            self.restart_search();
        }
    }

    fn restart_search(&mut self) {
        self.last_query = self.input.text.clone();
        self.cancel_active();
        self.hits.clear();
        self.sel = 0;
        self.scroll = 0.0;
        self.backend = None;
        self.elapsed = None;
        self.error = None;

        let trimmed = self.input.text.trim();
        if trimmed.is_empty() {
            return;
        }

        let root = match self.scope {
            SearchScope::CurrentFolder => {
                let host = tsubame_core::archive::host_archive_path(&self.folder);
                let base = if crate::archive_ui::is_archive_path(&self.folder) {
                    crate::archive_ui::host_folder_of(&self.folder).unwrap_or_else(|| PathBuf::from(host))
                } else {
                    PathBuf::from(host)
                };
                if base.as_os_str().is_empty() { None } else { Some(base) }
            }
            SearchScope::AllDrives => None,
        };

        let generation = self.gen_token.next_generation();
        self.active_gen = generation;
        self.searching = true;
        let query = SearchQuery {
            text: trimmed.to_string(),
            root,
            max_results: MAX_OVERLAY_HITS,
            ..SearchQuery::default()
        };
        let (task, rx) = self.engine.start(query, generation, self.gen_token.clone(), Some(self.wake.clone()));
        self.task = Some(task);
        self.rx = Some(rx);
    }

    /// Drains streamed search batches for the current generation.
    /// Returns `true` when the UI should repaint.
    pub fn pump(&mut self) -> bool {
        let Some(rx) = self.rx.as_ref() else {
            return false;
        };
        let t0 = Instant::now();
        let mut changed = false;
        let mut done = false;

        loop {
            if t0.elapsed() >= PUMP_BUDGET {
                break;
            }
            match rx.try_recv() {
                Ok(msg) => {
                    if msg.generation() != self.active_gen {
                        continue;
                    }
                    match msg {
                        SearchMessage::Batch { hits, .. } => {
                            let room = MAX_OVERLAY_HITS.saturating_sub(self.hits.len());
                            if room > 0 {
                                self.hits.extend(hits.into_iter().take(room));
                                changed = true;
                            }
                        }
                        SearchMessage::Done(d) => {
                            self.searching = false;
                            self.backend = d.backend;
                            self.elapsed = Some(d.elapsed);
                            if !d.cancelled {
                                self.error = d.error.map(|e| e.to_string());
                            }
                            done = true;
                            changed = true;
                            break;
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.searching = false;
                    done = true;
                    changed = true;
                    break;
                }
            }
        }
        if done {
            self.rx = None;
            self.task = None;
        }
        changed
    }

    fn step(&mut self, down: bool, by: usize) {
        if self.hits.is_empty() {
            self.sel = 0;
            return;
        }
        let max = self.hits.len() - 1;
        self.sel = if down {
            (self.sel + by).min(max)
        } else {
            self.sel.saturating_sub(by)
        };
    }

    fn activate_sel(&self, open_file_directly: bool) -> SearchAction {
        let Some(hit) = self.hits.get(self.sel) else {
            return SearchAction::None;
        };
        let full = hit.path.to_string_lossy().into_owned();
        if hit.is_dir {
            return SearchAction::OpenFolder(full);
        }
        if open_file_directly {
            return SearchAction::OpenFile(full);
        }
        match hit.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => SearchAction::RevealInFolder {
                folder: parent.to_string_lossy().into_owned(),
                name: hit.name.clone(),
            },
            _ => SearchAction::OpenFile(full),
        }
    }

    /// Paints the overlay as a popup and returns what `View` should do.
    pub fn paint(&mut self, ui: &mut Ui, g: &mut dyn Canvas, area: Rect) -> SearchAction {
        let t = ui.theme;
        if self.fresh {
            self.fresh = false;
            ui.open_popup(ID_SEARCH_OVERLAY);
            ui.set_focus(Some(ID_INPUT));
        }
        if !ui.is_popup_open(ID_SEARCH_OVERLAY) {
            ui.capture_keys = false;
            return SearchAction::Close;
        }
        ui.capture_keys = true;

        let mut esc = false;
        let mut toggle = false;
        let mut toggle_scope = false;
        let mut activate: Option<bool> = None; // true = open file directly (Ctrl+Enter)

        for (qi, ev) in ui.key_input(ID_INPUT) {
            if let Event::KeyDown { key, mods, .. } = ev {
                let mut take = true;
                match key {
                    Key::Down if mods.none() => self.step(true, 1),
                    Key::Up if mods.none() => self.step(false, 1),
                    Key::PageDown => self.step(true, 10),
                    Key::PageUp => self.step(false, 10),
                    Key::Home if mods.ctrl => self.sel = 0,
                    Key::End if mods.ctrl && !self.hits.is_empty() => self.sel = self.hits.len() - 1,
                    Key::Tab if mods.none() => toggle_scope = true,
                    Key::Enter => activate = Some(mods.ctrl),
                    Key::Escape if mods.none() => esc = true,
                    Key::Char('F') if mods.ctrl && mods.shift && !mods.alt => toggle = true,
                    _ => take = false,
                }
                if take {
                    ui.consume(qi);
                }
            }
        }

        let panel = panel_rect(area, &t, self.hits.len());
        let input_r = Rect::new(panel.x, panel.y, panel.w, t.height_palette_input);
        let scope_r = Rect::new(panel.x, input_r.b(), panel.w, t.height_palette_mode);
        let footer_r = Rect::new(panel.x, panel.b() - t.height_palette_footer, panel.w, t.height_palette_footer);
        let list_r = Rect::new(panel.x, scope_r.b() + 1.0, panel.w, (footer_r.y - scope_r.b() - 1.0).max(0.0));

        if ui.popup_begin(g, ID_SEARCH_OVERLAY, panel) {
            self.paint_input(ui, g, input_r, &t);
            if self.searching {
                let frac = 1.0 - (-(self.hits.len() as f32) / 500.0).exp();
                let lw = (panel.w * frac).max(panel.w * 0.12);
                g.fill(Rect::new(panel.x, input_r.b() - t.height_progress_line, lw, t.height_progress_line), t.accent);
            }
            let clicked_scope = self.paint_scope_bar(ui, g, scope_r, &t);
            g.fill(Rect::new(panel.x, scope_r.b(), panel.w, 1.0), t.border);

            if toggle_scope {
                self.set_scope(self.scope.toggle());
                ui.request_repaint();
            } else if let Some(sc) = clicked_scope {
                self.set_scope(sc);
                ui.set_focus(Some(ID_INPUT));
                ui.request_repaint();
            } else if self.input.text != self.last_query {
                self.restart_search();
                ui.request_repaint();
            }

            if let Some(open_direct) = self.paint_rows(ui, g, list_r, &t) {
                activate = Some(open_direct);
            }
            self.paint_footer(g, footer_r, &t);
            ui.popup_end(g);
        }

        if esc || toggle {
            ui.capture_keys = false;
            ui.close_popup(ID_SEARCH_OVERLAY);
            return SearchAction::Close;
        }
        if let Some(open_direct) = activate {
            let act = self.activate_sel(open_direct);
            if act != SearchAction::None {
                ui.capture_keys = false;
                ui.close_popup(ID_SEARCH_OVERLAY);
                return act;
            }
        }
        SearchAction::None
    }

    fn paint_input(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        let it = ui.interact(ID_INPUT, r, Sense::click().focusable().cursor(Cursor::IBeam));
        g.text("\u{E721}", Rect::new(r.x + 14.0, r.y, 22.0, r.h), f(15.0).icon().c(), t.accent);

        let font = f(t.font_size_md);
        let tx = r.x + 42.0;
        let esc_w = 38.0;
        let inner = Rect::new(tx, r.y, (r.w - (tx - r.x) - esc_w - 18.0).max(40.0), r.h);

        let mut clip = std::mem::replace(&mut ui.clipboard, Box::new(tsubame_ui::MemClipboard::default()));
        for (qi, ev) in ui.key_input(ID_INPUT) {
            match ev {
                Event::Char(ch) if !ch.is_control() => {
                    ui.consume(qi);
                    let mut b = [0u8; 4];
                    self.input.insert(ch.encode_utf8(&mut b));
                }
                Event::Char(_) => ui.consume(qi),
                Event::KeyDown { key, mods, .. } if !matches!(key, Key::Enter | Key::Escape | Key::Tab) => {
                    let (used, _) = self.input.key(key, mods, &mut *clip);
                    if used {
                        ui.consume(qi);
                    }
                }
                _ => {}
            }
        }
        ui.clipboard = clip;

        if let Some(p) = it.pressed {
            let i = g.hit_index(&self.input.text, font, p.pos.x - tx);
            self.input.caret = i;
            self.input.anchor = i;
        }

        g.clip(inner);
        let text = self.input.text.clone();
        if text.is_empty() {
            g.text("Search files and folders (supports * and ?)", inner, font, t.text_disabled);
        } else {
            if self.input.has_selection() {
                let (a, b) = self.input.sel();
                let (xa, xb) = (g.caret_x(&text, font, a), g.caret_x(&text, font, b));
                g.fill(Rect::new(tx + xa, r.y + 10.0, xb - xa, r.h - 20.0), t.selected);
            }
            g.text(&text, Rect::new(tx, r.y, inner.w + 400.0, r.h), font, t.text);
        }
        if ui.has_focus(ID_INPUT) && ui.window_focused() {
            let cx = tx + g.caret_x(&text, font, self.input.caret);
            ui.mark_text_focus();
            if ui.caret_on {
                g.fill(Rect::new(cx.round(), r.y + 12.0, 1.0, r.h - 24.0), t.text);
            }
            ui.set_ime_caret(Rect::new(cx, r.y + 10.0, 1.0, r.h - 20.0));
        }
        g.unclip();

        let er = Rect::new(r.r() - esc_w - 14.0, r.y + (r.h - 20.0) / 2.0, esc_w, 20.0);
        g.round(er, t.radius_sm, t.field);
        g.round_stroke(er, t.radius_sm, t.popup_border, 1.0);
        g.text("Esc", er, t.font_mono().c(), t.text_dim);
    }

    fn paint_scope_bar(&self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) -> Option<SearchScope> {
        let mut x = r.x + 8.0;
        let mut picked = None;
        let font = t.font_small();
        for (i, sc) in [SearchScope::CurrentFolder, SearchScope::AllDrives].into_iter().enumerate() {
            let label = sc.label();
            let w = g.measure(label, font) + 18.0;
            let cr = Rect::new(x, r.y + 3.0, w, r.h - 6.0);
            let it = ui.interact(ID_SCOPE.with(i as u64), cr, Sense::click());
            let on = sc == self.scope;
            if on {
                g.round(cr, t.radius_sm, t.selected);
            } else if it.hovered {
                g.round(cr, t.radius_sm, t.hover);
            }
            g.text(label, cr, if on { font.b().c() } else { font.c() }, if on { t.text } else { t.text_dim });
            if it.clicked == Some(Button::Left) {
                picked = Some(sc);
            }
            x += w + 4.0;
        }

        // Right-aligned live status text
        let (status, color) = if let Some(err) = &self.error {
            (err.clone(), t.danger)
        } else if self.searching {
            (format!("Searching\u{2026} {} results", crate::statusbar::count(self.hits.len())), t.accent)
        } else if self.input.text.trim().is_empty() {
            (format!("In {}", crate::dnd::name_of(&self.folder)), t.text_dim)
        } else {
            let engine = match self.backend {
                Some(SearchBackend::Everything) => "Everything",
                Some(SearchBackend::Walk) => "Walk",
                None => "Search",
            };
            let ms = self.elapsed.map(|d| d.as_millis()).unwrap_or(0);
            (
                format!(
                    "{} {} \u{00B7} {engine} ({ms} ms)",
                    crate::statusbar::count(self.hits.len()),
                    if self.hits.len() == 1 { "result" } else { "results" }
                ),
                t.text_dim,
            )
        };
        let sw = g.measure(&status, font).min((r.r() - x - 12.0).max(0.0));
        if sw > 10.0 {
            g.text(&status, Rect::new(r.r() - sw - 10.0, r.y, sw + 2.0, r.h), font.r(), color);
        }
        picked
    }

    /// Draws only the visible rows of `self.hits`. Returns `Some(open_direct)` on double-click.
    fn paint_rows(&mut self, ui: &mut Ui, g: &mut dyn Canvas, r: Rect, t: &Theme) -> Option<bool> {
        let row_h = t.height_palette_item;
        let total_h = self.hits.len() as f32 * row_h;
        let max_scroll = (total_h - r.h).max(0.0);

        let wi = ui.interact(ID_ROW, r, Sense::hover().wheel());
        if wi.wheel.1 != 0.0 {
            self.scroll = (self.scroll - wi.wheel.1 * row_h * t.wheel_lines()).clamp(0.0, max_scroll);
        }

        if !self.hits.is_empty() {
            self.sel = self.sel.min(self.hits.len() - 1);
            let sy = self.sel as f32 * row_h;
            if sy < self.scroll {
                self.scroll = sy;
            } else if sy + row_h > self.scroll + r.h {
                self.scroll = sy + row_h - r.h;
            }
            self.scroll = self.scroll.clamp(0.0, max_scroll);
        } else {
            self.scroll = 0.0;
        }

        g.clip(r);
        if self.hits.is_empty() {
            let msg = if self.input.text.trim().is_empty() {
                "Type a filename or wildcard pattern to search"
            } else if self.searching {
                "Searching\u{2026}"
            } else {
                "No matching files or folders"
            };
            g.text(msg, r, t.font().c(), t.text_dim);
            g.unclip();
            return None;
        }

        let first = (self.scroll / row_h).floor().max(0.0) as usize;
        let visible = (r.h / row_h).ceil() as usize + 2;
        let last = (first + visible).min(self.hits.len());
        let mut activated = None;

        for i in first..last {
            let y = r.y + i as f32 * row_h - self.scroll;
            let rr = Rect::new(r.x, y, r.w, row_h);
            let it = ui.interact(ID_ROW.with(i as u64 + 1), rr, Sense::click());
            let selected = i == self.sel;
            if selected {
                g.fill(rr, t.selected);
            } else if it.hovered {
                g.fill(rr, t.hover);
            }
            if let Some(p) = it.pressed
                && p.button == Button::Left
            {
                self.sel = i;
                if p.double {
                    activated = Some(p.mods.ctrl);
                }
                ui.request_repaint();
            }

            let hit = &self.hits[i];
            let (glyph, gcol) = if hit.is_dir { ("\u{E8B7}", t.folder) } else { ("\u{E8A5}", t.text_dim) };
            g.text(glyph, Rect::new(r.x + 12.0, rr.y, 18.0, row_h), f(14.0).icon().c(), gcol);

            let size_w = if hit.is_dir {
                0.0
            } else {
                crate::data::fmt_size(hit.size, &mut self.size_buf);
                let w = 72.0;
                g.text(&self.size_buf, Rect::new(r.r() - w - 12.0, rr.y, w, row_h), t.font_small().r(), t.text_dim);
                w + 16.0
            };

            let tx = r.x + 36.0;
            let tw = (r.r() - tx - size_w - 12.0).max(20.0);
            let top = Rect::new(tx, rr.y + 2.0, tw, row_h * 0.52);
            let sub = Rect::new(tx, rr.y + row_h * 0.50, tw, row_h * 0.46);
            g.text(&hit.name, top, if selected { t.font().b() } else { t.font() }, t.text);
            let parent = hit.path.parent().map(|p| p.to_string_lossy()).unwrap_or_default();
            g.text(&parent, sub, t.font_small(), t.text_dim);
        }

        // Minimal scrollbar indicator when scrolling
        if total_h > r.h {
            let track_h = r.h - 4.0;
            let thumb_h = ((r.h / total_h) * track_h).max(t.layout_scrollbar_thumb_min);
            let frac = if max_scroll > 0.0 { self.scroll / max_scroll } else { 0.0 };
            let ty = r.y + 2.0 + frac * (track_h - thumb_h);
            g.round(Rect::new(r.r() - 6.0, ty, 4.0, thumb_h), 2.0, t.scrollbar);
        }

        g.unclip();
        activated
    }

    fn paint_footer(&self, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);
        let cy = r.y + r.h / 2.0;
        let mut right = r.r() - 12.0;
        let font = t.font_small();
        for (keys, label) in [
            ("Esc", "Close"),
            ("Tab", "Scope"),
            ("Ctrl+Enter", "Open"),
            ("Enter", "Go to item"),
        ] {
            let lw = g.measure(label, font) + 2.0;
            g.text(label, Rect::new(right - lw, r.y + 1.0, lw, r.h - 1.0), font, t.text_dim);
            right -= lw + 4.0;
            right -= keycaps_right(g, t, right, cy, keys) + 14.0;
        }
    }
}

fn panel_rect(area: Rect, t: &Theme, hit_count: usize) -> Rect {
    let w = (t.layout_palette_width + 80.0).min(area.w - 32.0).max(320.0);
    let chrome = t.height_palette_input + t.height_palette_mode + t.height_palette_footer + 2.0;
    let rows_h = if hit_count == 0 {
        56.0
    } else {
        (hit_count.min(11) as f32 * t.height_palette_item).max(56.0)
    };
    let h = (chrome + rows_h).min(area.h - t.layout_palette_top - 24.0).max(chrome + 40.0);
    let x = area.x + (area.w - w) / 2.0;
    let y = area.y + t.layout_palette_top.min((area.h - h).max(8.0));
    Rect::new(x.round(), y.round(), w.round(), h.round())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("tsubame-search-ui-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("alpha.rs"), b"fn a() {}").unwrap();
        std::fs::write(d.join("sub").join("beta.rs"), b"fn b() {}").unwrap();
        std::fs::write(d.join("notes.txt"), b"hello").unwrap();
        d
    }

    #[test]
    fn global_search_streams_and_cancels_stale_generations() {
        let dir = temp_dir("stream");
        let wake: Wake = Arc::new(|| {});
        let mut ov = SearchOverlay::with_mode(dir.to_string_lossy().into_owned(), SearchMode::WalkOnly, wake);

        ov.set_query("*.txt");
        let g1 = ov.active_generation();
        // Rapidly supersede with a new query before waiting -> g1 is cancelled.
        ov.set_query("*.rs");
        let g2 = ov.active_generation();
        assert!(g2 > g1);

        let t0 = Instant::now();
        while ov.is_searching() {
            ov.pump();
            assert!(t0.elapsed() < Duration::from_secs(5), "search timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        ov.pump();

        assert_eq!(ov.hits.len(), 2, "only .rs hits from g2: {:?}", ov.hits);
        assert!(ov.hits.iter().all(|h| h.name.ends_with(".rs")));
        assert_eq!(ov.backend, Some(SearchBackend::Walk));

        // Clearing query cancels and empties hits.
        ov.set_query("");
        assert!(!ov.is_searching());
        assert!(ov.hits.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_toggle_and_activation_actions() {
        let dir = temp_dir("act");
        let wake: Wake = Arc::new(|| {});
        let mut ov = SearchOverlay::with_mode(dir.to_string_lossy().into_owned(), SearchMode::WalkOnly, wake);
        assert_eq!(ov.scope, SearchScope::CurrentFolder);
        assert_eq!(ov.scope.toggle(), SearchScope::AllDrives);

        ov.set_query("beta.rs");
        let t0 = Instant::now();
        while ov.is_searching() {
            ov.pump();
            assert!(t0.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(ov.hits.len(), 1);
        assert_eq!(
            ov.activate_sel(false),
            SearchAction::RevealInFolder {
                folder: dir.join("sub").to_string_lossy().into_owned(),
                name: "beta.rs".into(),
            }
        );
        assert_eq!(
            ov.activate_sel(true),
            SearchAction::OpenFile(dir.join("sub").join("beta.rs").to_string_lossy().into_owned())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
