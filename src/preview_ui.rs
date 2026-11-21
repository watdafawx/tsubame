//! Preview pane and Quick Look UI (P4.F2):
//! - Built-in switchable extension `tsubame.ext.preview`
//! - Right-docked resizable preview pane (`Alt+P`, 5 px splitter, double-click resets to 320 px)
//! - Centered Quick Look modal (`Space` / `F3`) sharing the exact same renderer & worker
//! - Lazy `PreviewWorker` (spawned only when pane or Quick Look opens; cache cleared & thread
//!   dropped when both close; 0 idle wake-ups — no timers)
//! - All 5 modes: Image, Text/Code (6-token syntax tinting + Wrap), Media info, Hex dump
//!   (auto 8 B/row < 420 px + live 8 B / 16 B toggle), and Multi-select / Unsupported / Empty / Error.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use tsubame_config::{DEFAULT_PREVIEW_WIDTH, PreviewConfig};
use tsubame_ext_api::{Contributions, Extension, ExtensionInfo};
use tsubame_ui::{
    Button, Canvas, Cursor, Event, Id, Key, Rect, ScrollMetrics, Sense, Theme, Ui, f, keycaps_right,
    vscrollbar,
};
use tsubame_win::enumerate::Wake;
use tsubame_win::preview::hex::{
    self, HEX_NARROW_BYTES_PER_ROW, HEX_WIDE_BYTES_PER_ROW, bytes_per_row,
};
use tsubame_win::preview::image::{DEFAULT_CACHE_BYTES, DEFAULT_CACHE_ITEMS};
use tsubame_win::preview::{MediaKind, PreviewKind, PreviewReply, PreviewWorker, TokenKind};

/// Built-in extension id for the Preview pane and Quick Look.
pub const EXT_PREVIEW: &str = "tsubame.ext.preview";

/// Splitter hit strip width in DIPs (`docs/preview-mock.md`).
pub const SPLITTER_W: f32 = 5.0;
/// Minimum main-list width kept when the docked preview pane is open.
pub const MIN_MAIN_W: f32 = 240.0;
/// Loading skeleton delay (no timer: only shown if a frame paints after this).
const LOADING_DELAY: Duration = Duration::from_millis(80);

pub const ID_PREVIEW_SPLIT: Id = Id::new("preview-split");
pub const ID_QL_OVERLAY: Id = Id::new("quick-look");
const ID_PV_CLOSE: Id = Id::new("preview.close");
const ID_PV_OPEN: Id = Id::new("preview.open");
const ID_PV_HEX: Id = Id::new("preview.view-hex");
const ID_PV_FIT: Id = Id::new("preview.img-fit");
const ID_PV_ACTUAL: Id = Id::new("preview.img-100");
const ID_PV_WRAP: Id = Id::new("preview.text-wrap");
const ID_PV_HEX8: Id = Id::new("preview.hex-8");
const ID_PV_HEX16: Id = Id::new("preview.hex-16");
const ID_PV_SCROLL: Id = Id::new("preview.scroll");
const ID_PV_VBAR: Id = Id::new("preview.vbar");
const ID_QL_PREV: Id = Id::new("quick-look.prev");
const ID_QL_NEXT: Id = Id::new("quick-look.next");
const ID_QL_OPEN: Id = Id::new("quick-look.open");
const ID_QL_CLOSE: Id = Id::new("quick-look.close");

/// Built-in extension registering `preview.toggle_pane` (`Alt+P`) and
/// `preview.quick_look` (`Space`, `F3`).
pub struct PreviewExtension;

impl Extension<crate::ext::App> for PreviewExtension {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo::builtin(EXT_PREVIEW, "Preview pane & Quick Look", false)
    }

    fn contribute(&self, c: &mut Contributions<crate::ext::App>) {
        for spec in crate::commands::PREVIEW_COMMANDS {
            c.command(spec.clone(), crate::view::View::run_builtin);
        }
    }
}

/// True when `tsubame.ext.preview` is enabled in the registry.
pub fn is_enabled() -> bool {
    crate::ext::read(|r| r.is_enabled(EXT_PREVIEW).unwrap_or(false))
}

/// What the active pane currently points the preview at (built from `EntryStore`, zero disk I/O).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PreviewTarget {
    #[default]
    Empty,
    Multi {
        count: usize,
        total_bytes: u64,
        /// Up to 5 sample items: `(name, size, is_dir)`.
        samples: Vec<(String, u64, bool)>,
    },
    Single {
        path: PathBuf,
        name: String,
        /// 0-based index in the active tab's view and total visible items.
        index: usize,
        total: usize,
        is_dir: bool,
        size: u64,
        /// Virtual archive entry (cannot read directly from host filesystem).
        in_archive: bool,
    },
}

/// Action emitted by the docked pane or Quick Look for `View` to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewAction {
    None,
    ClosePane,
    CloseQuickLook,
    OpenFile(String),
    Step(isize),
}

/// Live preview state owned by `View`.
pub struct PreviewState {
    pub pane_open: bool,
    pub quick_look: bool,
    ql_fresh: bool,
    /// Current docked pane width in DIPs (`220..=600`, default `320`).
    pub width: f32,
    /// Lazy worker + reply channel. `None` while both pane and Quick Look are closed.
    worker: Option<(PreviewWorker, Receiver<PreviewReply>)>,
    active_gen: u64,
    target: PreviewTarget,
    /// Deduplication key of the last submitted worker request:
    /// `(path, force_hex, direct_hex_binaries, syntax_highlight)`.
    requested_key: Option<(PathBuf, bool, bool, bool)>,
    /// User clicked `View as Hex` for the current file (resets on path change).
    pub force_hex: bool,
    loading_since: Option<Instant>,
    loaded_path: Option<PathBuf>,
    loaded_kind: Option<PreviewKind>,
    image_key_seq: u64,
    // Per-mode UI toggles & scroll offsets
    pub image_actual: bool,
    pub text_wrap: bool,
    pub hex_override_cols: Option<usize>,
    scroll: f64,
    content_h: f64,
    /// Last `hex_auto_width` seen so flipping the setting clears manual override.
    last_hex_auto: bool,
    /// Last `width` while dragging splitter (to show live px chip).
    dragging_split: bool,
}

impl Default for PreviewState {
    fn default() -> Self {
        PreviewState {
            pane_open: false,
            quick_look: false,
            ql_fresh: false,
            width: DEFAULT_PREVIEW_WIDTH as f32,
            worker: None,
            active_gen: 0,
            target: PreviewTarget::Empty,
            requested_key: None,
            force_hex: false,
            loading_since: None,
            loaded_path: None,
            loaded_kind: None,
            image_key_seq: 0,
            image_actual: false,
            text_wrap: false,
            hex_override_cols: None,
            scroll: 0.0,
            content_h: 0.0,
            last_hex_auto: true,
            dragging_split: false,
        }
    }
}

impl PreviewState {
    /// True when the docked preview pane should be drawn.
    pub fn pane_visible(&self) -> bool {
        self.pane_open && is_enabled()
    }

    /// True when Quick Look is open and the extension is enabled.
    pub fn quick_look_open(&self) -> bool {
        self.quick_look && is_enabled()
    }

    /// True when either surface is active.
    pub fn active(&self) -> bool {
        is_enabled() && (self.pane_open || self.quick_look)
    }

    #[cfg(test)]
    pub fn worker_running(&self) -> bool {
        self.worker.is_some()
    }

    #[cfg(test)]
    pub fn loaded_kind(&self) -> Option<&PreviewKind> {
        self.loaded_kind.as_ref()
    }

    #[cfg(test)]
    pub fn worker_syntax_highlight(&self) -> Option<bool> {
        self.worker.as_ref().map(|(w, _)| w.syntax_highlight())
    }

    /// Maximum docked width allowed for a window body `avail_w` DIPs wide.
    pub fn clamp_width(raw: f32, avail_w: f32) -> f32 {
        let min_w = PreviewConfig::MIN_WIDTH as f32;
        let max_by_window = (avail_w * 0.5).min(avail_w - MIN_MAIN_W - SPLITTER_W);
        let max_w = (PreviewConfig::MAX_WIDTH as f32).min(max_by_window).max(min_w);
        raw.clamp(min_w, max_w).round()
    }

    /// Shuts down the worker, clears cached bitmaps, and drops decoded preview data.
    pub fn release_worker(&mut self) {
        if let Some((worker, _rx)) = self.worker.take() {
            worker.clear_cache();
            worker.shutdown();
        }
        self.requested_key = None;
        self.loading_since = None;
        self.loaded_path = None;
        self.loaded_kind = None;
        self.force_hex = false;
        self.scroll = 0.0;
        self.content_h = 0.0;
    }

    /// Called when `tsubame.ext.preview` is switched off: hides both surfaces and
    /// releases all memory / threads immediately.
    pub fn disable(&mut self, ui: &mut Ui) {
        if self.quick_look {
            self.quick_look = false;
            self.ql_fresh = false;
            ui.capture_keys = false;
            ui.close_popup(ID_QL_OVERLAY);
        }
        self.pane_open = false;
        self.release_worker();
    }

    /// Opens or closes the docked pane. Returns the new `pane_open` value.
    pub fn set_pane_open(&mut self, open: bool) -> bool {
        let open = open && is_enabled();
        self.pane_open = open;
        if !self.active() {
            self.release_worker();
        }
        self.pane_open
    }

    /// Opens Quick Look overlay (if extension and setting allow).
    pub fn open_quick_look(&mut self) {
        if !is_enabled() {
            return;
        }
        if !self.quick_look {
            self.quick_look = true;
            self.ql_fresh = true;
            self.scroll = 0.0;
        }
    }

    /// Closes Quick Look overlay and frees the worker if the docked pane is closed.
    pub fn close_quick_look(&mut self, ui: &mut Ui) {
        if self.quick_look {
            self.quick_look = false;
            self.ql_fresh = false;
            ui.capture_keys = false;
            ui.close_popup(ID_QL_OVERLAY);
        }
        if !self.active() {
            self.release_worker();
        }
    }

    /// Applies live changes from `prefs.preview` (Settings toggles or config load).
    pub fn apply_prefs(&mut self, ui: &mut Ui, cfg: &PreviewConfig) {
        let enabled = is_enabled();
        self.width = cfg.width.clamp(PreviewConfig::MIN_WIDTH, PreviewConfig::MAX_WIDTH) as f32;
        self.pane_open = cfg.enabled && enabled;
        if (!cfg.quick_look || !enabled) && self.quick_look {
            self.close_quick_look(ui);
        }
        if cfg.hex_auto_width != self.last_hex_auto {
            self.last_hex_auto = cfg.hex_auto_width;
            self.hex_override_cols = None;
        }
        if let Some((worker, _)) = self.worker.as_ref() {
            worker.set_syntax_highlight(cfg.syntax_highlight);
        }
        if !self.active() {
            self.release_worker();
        } else {
            // Re-sync on the next paint so syntax_highlight / direct_hex_binaries apply live.
            self.requested_key = None;
        }
    }

    /// Forces hex decoding for the currently targeted single file (`View as Hex` button).
    pub fn request_force_hex(&mut self) {
        self.force_hex = true;
        self.requested_key = None;
        self.scroll = 0.0;
    }

    /// Ensures the lazy worker is running and matches `target` + `cfg`.
    /// Never touches the disk on the UI thread.
    pub fn sync_target(&mut self, target: PreviewTarget, cfg: &PreviewConfig, wake: &Wake) {
        if !self.active() {
            if self.worker.is_some() {
                self.release_worker();
            }
            self.target = PreviewTarget::Empty;
            return;
        }

        // Reset per-file toggles/scroll when the path itself changed.
        let path_changed = match (&self.target, &target) {
            (PreviewTarget::Single { path: a, .. }, PreviewTarget::Single { path: b, .. }) => a != b,
            (PreviewTarget::Single { .. }, _) | (_, PreviewTarget::Single { .. }) => true,
            _ => false,
        };
        if path_changed {
            self.force_hex = false;
            self.image_actual = false;
            self.scroll = 0.0;
            self.content_h = 0.0;
        }
        self.target = target;

        let (worker, _) = self.worker.get_or_insert_with(|| {
            let (w, rx) = PreviewWorker::with_options(
                DEFAULT_CACHE_BYTES,
                DEFAULT_CACHE_ITEMS,
                Some(wake.clone()),
            );
            w.set_syntax_highlight(cfg.syntax_highlight);
            (w, rx)
        });
        if worker.syntax_highlight() != cfg.syntax_highlight {
            worker.set_syntax_highlight(cfg.syntax_highlight);
        }

        match &self.target {
            PreviewTarget::Single { path, is_dir: false, in_archive: false, .. } => {
                let key = (path.clone(), self.force_hex, cfg.direct_hex_binaries, cfg.syntax_highlight);
                if self.requested_key.as_ref() != Some(&key) {
                    let generation = worker.begin_generation();
                    self.active_gen = generation;
                    self.requested_key = Some(key);
                    self.loading_since = Some(Instant::now());
                    worker.request(generation, path.clone(), self.force_hex, cfg.direct_hex_binaries);
                }
            }
            PreviewTarget::Single { is_dir: true, .. } => {
                if self.requested_key.is_some() || self.loaded_kind.is_some() {
                    self.active_gen = worker.begin_generation();
                    self.requested_key = None;
                    self.loading_since = None;
                    self.loaded_path = None;
                    self.loaded_kind = None;
                }
            }
            PreviewTarget::Single { in_archive: true, .. } | PreviewTarget::Multi { .. } | PreviewTarget::Empty => {
                if self.requested_key.is_some() || self.loaded_kind.is_some() {
                    self.active_gen = worker.begin_generation();
                    self.requested_key = None;
                    self.loading_since = None;
                    self.loaded_path = None;
                    self.loaded_kind = None;
                }
            }
        }
    }

    /// Drains finished preview replies from the background worker.
    /// Drops any reply whose generation is stale. Returns `true` if UI should repaint.
    pub fn pump(&mut self) -> bool {
        let Some((_, rx)) = self.worker.as_ref() else {
            return false;
        };
        let mut latest: Option<PreviewReply> = None;
        while let Ok(reply) = rx.try_recv() {
            if reply.generation == self.active_gen {
                latest = Some(reply);
            }
        }
        let Some(reply) = latest else {
            return false;
        };
        self.loading_since = None;
        self.loaded_path = Some(reply.path);
        if matches!(reply.kind, PreviewKind::Image { .. }) {
            self.image_key_seq = self.image_key_seq.wrapping_add(1);
        }
        self.loaded_kind = Some(reply.kind);
        self.scroll = 0.0;
        self.content_h = 0.0;
        true
    }

    // ---- Docked right splitter & pane ---------------------------------------

    /// Splits `body` into `(main_area, Some((splitter_rect, pane_rect)))` when the
    /// docked preview pane is open and enabled. Handles splitter drag and double-click reset.
    pub fn split_body(
        &mut self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        body: Rect,
    ) -> (Rect, Option<Rect>, Option<u32>) {
        if !self.pane_visible() || body.w < PreviewConfig::MIN_WIDTH as f32 + MIN_MAIN_W + SPLITTER_W {
            self.dragging_split = false;
            return (body, None, None);
        }

        let before = self.width;
        let clamped = Self::clamp_width(self.width, body.w);
        let bar_x = (body.r() - clamped - SPLITTER_W).round();
        let bar = Rect::new(bar_x, body.y, SPLITTER_W, body.h);
        let it = ui.interact(ID_PREVIEW_SPLIT, bar, Sense::click().cursor(Cursor::SizeWE));
        let mut width_changed = None;

        if let Some(p) = it.pressed {
            if p.double {
                self.width = Self::clamp_width(DEFAULT_PREVIEW_WIDTH as f32, body.w);
                width_changed = Some(self.width.round() as u32);
                ui.drag_f64 = f64::NAN;
                ui.request_repaint();
            } else {
                ui.drag_f64 = (p.pos.x - bar_x) as f64;
            }
        }
        if it.active && !ui.drag_f64.is_nan() {
            let new_bar_x = ui.mouse().x - ui.drag_f64 as f32;
            let raw_w = body.r() - new_bar_x - SPLITTER_W;
            self.width = Self::clamp_width(raw_w, body.w);
            if (self.width - before).abs() >= 0.5 {
                width_changed = Some(self.width.round() as u32);
                ui.request_repaint();
            }
        }
        self.dragging_split = it.active && !ui.drag_f64.is_nan();

        let pane_w = Self::clamp_width(self.width, body.w);
        let main_w = (body.w - pane_w - SPLITTER_W).max(0.0);
        let main_r = Rect::new(body.x, body.y, main_w, body.h);
        let split_r = Rect::new(main_r.r(), body.y, SPLITTER_W, body.h);
        let pane_r = Rect::new(split_r.r(), body.y, pane_w, body.h);

        // Paint the 5 px splitter strip (1 px hairline at rest, 3 px hover/drag)
        let t = ui.theme;
        g.fill(split_r, t.panel);
        if self.dragging_split {
            g.fill(Rect::new(split_r.x + 1.0, split_r.y, 3.0, split_r.h), t.accent);
        } else if it.hovered {
            g.fill(Rect::new(split_r.x + 1.0, split_r.y, 3.0, split_r.h), t.scrollbar_hover);
        } else {
            g.fill(Rect::new(split_r.x + 2.0, split_r.y, 1.0, split_r.h), t.border);
        }

        (main_r, Some(pane_r), width_changed)
    }

    /// Paints the docked right preview pane in `rect`.
    pub fn paint_docked(
        &mut self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        rect: Rect,
        cfg: &PreviewConfig,
    ) -> PreviewAction {
        let t = ui.theme;
        g.fill(rect, t.panel);
        g.clip(rect);

        let mut area = rect;
        let head = area.cut_top(28.0);
        let mut action = self.paint_docked_header(ui, g, head, &t);

        let body_act = self.paint_body(ui, g, area, &t, cfg, false);
        if action == PreviewAction::None {
            action = body_act;
        }

        // Live pixel-width chip while dragging the splitter (matches mock).
        if self.dragging_split {
            let label = format!("{} px", rect.w.round() as u32);
            let font = t.font_mono();
            let cw = g.measure(&label, font) + 16.0;
            let cr = Rect::new(rect.x + 8.0, rect.y + 34.0, cw, 22.0);
            g.round(cr, t.radius_md, t.popup);
            g.round_stroke(cr, t.radius_md, t.popup_border, 1.0);
            g.text(&label, cr, font.c(), t.text);
        }

        g.unclip();
        action
    }

    fn paint_docked_header(
        &self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        r: Rect,
        t: &Theme,
    ) -> PreviewAction {
        g.fill(r, t.panel);
        g.fill(Rect::new(r.x, r.b() - 1.0, r.w, 1.0), t.border);

        let title = match &self.target {
            PreviewTarget::Single { name, .. } => name.as_str(),
            PreviewTarget::Multi { .. } => "Selection",
            PreviewTarget::Empty => "Preview",
        };
        let close_r = Rect::new(r.r() - 26.0, r.y + 3.0, 22.0, 22.0);
        let open_path = match &self.target {
            PreviewTarget::Single { path, is_dir: false, in_archive: false, .. } => {
                Some(path.to_string_lossy().into_owned())
            }
            _ => None,
        };

        let mut right_edge = close_r.x - 4.0;
        let mut act = PreviewAction::None;

        if let Some(path) = open_path {
            let open_r = Rect::new(right_edge - 22.0, r.y + 3.0, 22.0, 22.0);
            if small_icon_btn(ui, g, t, ID_PV_OPEN, open_r, "\u{E8A7}") {
                act = PreviewAction::OpenFile(path);
            }
            right_edge = open_r.x - 4.0;
        }

        if small_icon_btn(ui, g, t, ID_PV_CLOSE, close_r, "\u{E8BB}") {
            act = PreviewAction::ClosePane;
        }

        let icon = match &self.target {
            PreviewTarget::Single { is_dir: true, .. } => "\u{E8B7}",
            PreviewTarget::Single { .. } => "\u{E8A5}",
            PreviewTarget::Multi { .. } => "\u{E762}",
            PreviewTarget::Empty => "\u{E890}",
        };
        let ic_col = match &self.target {
            PreviewTarget::Single { is_dir: true, .. } => t.folder,
            _ => t.text_dim,
        };
        g.text(icon, Rect::new(r.x + 8.0, r.y, 16.0, r.h - 1.0), f(13.0).icon().c(), ic_col);
        let tx = r.x + 28.0;
        g.text(title, Rect::new(tx, r.y, (right_edge - tx).max(12.0), r.h - 1.0), t.font().b(), t.text);
        act
    }

    // ---- Quick Look overlay (Space / F3) ------------------------------------

    /// Paints the centered Quick Look modal over `win_size` when open.
    pub fn paint_quick_look(
        &mut self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        win_size: (f32, f32),
        cfg: &PreviewConfig,
    ) -> PreviewAction {
        if !self.quick_look {
            return PreviewAction::None;
        }
        if !is_enabled() || !cfg.quick_look {
            self.close_quick_look(ui);
            return PreviewAction::CloseQuickLook;
        }

        let t = ui.theme;
        if self.ql_fresh {
            self.ql_fresh = false;
            ui.open_popup(ID_QL_OVERLAY);
            ui.set_focus(Some(ID_QL_OVERLAY));
        }
        if !ui.is_popup_open(ID_QL_OVERLAY) {
            self.quick_look = false;
            ui.capture_keys = false;
            if !self.active() {
                self.release_worker();
            }
            return PreviewAction::CloseQuickLook;
        }
        ui.capture_keys = true;

        // Keyboard input for Quick Look (Space/Esc/F3 close, arrows step, Enter opens)
        let mut key_act = PreviewAction::None;
        for (qi, ev) in ui.key_input(ID_QL_OVERLAY) {
            match ev {
                Event::KeyDown { key, mods, .. } => {
                    let mut take = true;
                    match key {
                        Key::Space | Key::Escape | Key::F(3) if mods.none() => {
                            key_act = PreviewAction::CloseQuickLook;
                        }
                        Key::Left | Key::Up if mods.none() => {
                            key_act = PreviewAction::Step(-1);
                        }
                        Key::Right | Key::Down if mods.none() => {
                            key_act = PreviewAction::Step(1);
                        }
                        Key::Enter if mods.none() => {
                            if let PreviewTarget::Single { path, is_dir: false, in_archive: false, .. } = &self.target {
                                key_act = PreviewAction::OpenFile(path.to_string_lossy().into_owned());
                            } else {
                                key_act = PreviewAction::CloseQuickLook;
                            }
                        }
                        Key::PageDown if mods.none() => {
                            self.scroll += 240.0;
                            ui.request_repaint();
                        }
                        Key::PageUp if mods.none() => {
                            self.scroll = (self.scroll - 240.0).max(0.0);
                            ui.request_repaint();
                        }
                        _ => take = false,
                    }
                    if take {
                        ui.consume(qi);
                    }
                }
                Event::Char(_) => {
                    // Swallow trailing WM_CHAR (e.g. ' ' from Space) so filter doesn't open.
                    ui.consume(qi);
                }
                _ => {}
            }
        }

        // Scrim below titlebar
        let scrim = Rect::new(0.0, t.height_titlebar, win_size.0, (win_size.1 - t.height_titlebar).max(0.0));
        let (scrim_col, scrim_a) = if t.is_dark() { (0x070A10, 0.66) } else { (0x161B26, 0.36) };
        g.fill_a(scrim, scrim_col, scrim_a);

        // Centered modal (~75% width x 80% height, clamped cleanly)
        let max_w = (scrim.w - 32.0).max(260.0);
        let max_h = (scrim.h - 32.0).max(220.0);
        let mw = (scrim.w * 0.75).clamp(480.0, 960.0).min(max_w).round();
        let mh = (scrim.h * 0.80).clamp(360.0, 680.0).min(max_h).round();
        let mx = (scrim.x + (scrim.w - mw) / 2.0).round();
        let my = (scrim.y + (scrim.h - mh) / 2.0).round();
        let modal = Rect::new(mx, my, mw, mh);

        let mut btn_act = PreviewAction::None;
        if ui.popup_begin_r(g, ID_QL_OVERLAY, modal, t.radius_lg) {
            let mut inner = modal;
            let head = inner.cut_top(36.0);
            let foot = inner.cut_bottom(28.0);

            btn_act = self.paint_ql_header(ui, g, head, &t);
            let body_act = self.paint_body(ui, g, inner, &t, cfg, true);
            if btn_act == PreviewAction::None {
                btn_act = body_act;
            }
            self.paint_ql_footer(g, foot, &t);
            ui.popup_end(g);
        }

        let act = if key_act != PreviewAction::None { key_act } else { btn_act };
        if act == PreviewAction::CloseQuickLook {
            self.close_quick_look(ui);
        }
        act
    }

    fn paint_ql_header(
        &self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        r: Rect,
        t: &Theme,
    ) -> PreviewAction {
        g.fill(r, t.panel);
        g.fill(Rect::new(r.x, r.b() - 1.0, r.w, 1.0), t.border);

        let mut act = PreviewAction::None;
        let by = r.y + 6.0;
        let bh = 24.0;
        let mut rx = r.r() - 10.0;

        // Close button
        let close_label = "Close";
        let close_w = g.measure(close_label, t.font_small()) + 18.0;
        let close_r = Rect::new(rx - close_w, by, close_w, bh);
        if pill_btn(ui, g, t, ID_QL_CLOSE, close_r, close_label, false, true) {
            act = PreviewAction::CloseQuickLook;
        }
        rx = close_r.x - 6.0;

        // Open button (when single real file)
        if let PreviewTarget::Single { path, is_dir: false, in_archive: false, .. } = &self.target {
            let open_label = "Open";
            let open_w = g.measure(open_label, t.font_small().b()) + 20.0;
            let open_r = Rect::new(rx - open_w, by, open_w, bh);
            if pill_btn(ui, g, t, ID_QL_OPEN, open_r, open_label, true, true) {
                act = PreviewAction::OpenFile(path.to_string_lossy().into_owned());
            }
            rx = open_r.x - 8.0;
        }

        // Prev / Next buttons
        let (can_prev, can_next, idx_text) = match &self.target {
            PreviewTarget::Single { index, total, .. } if *total > 0 => (
                *index > 0,
                *index + 1 < *total,
                Some(format!("{} / {}", index + 1, total)),
            ),
            _ => (false, false, None),
        };
        let next_r = Rect::new(rx - 26.0, by, 26.0, bh);
        let prev_r = Rect::new(next_r.x - 26.0, by, 26.0, bh);
        let nav_frame = Rect::new(prev_r.x, by, 52.0, bh);
        g.round(nav_frame, t.radius_md, t.row);
        g.round_stroke(nav_frame, t.radius_md, t.popup_border, 1.0);
        g.fill(Rect::new(next_r.x, by + 1.0, 1.0, bh - 2.0), t.border);
        if nav_step_btn(ui, g, t, ID_QL_PREV, prev_r, "\u{2190}", can_prev) {
            act = PreviewAction::Step(-1);
        }
        if nav_step_btn(ui, g, t, ID_QL_NEXT, next_r, "\u{2192}", can_next) {
            act = PreviewAction::Step(1);
        }
        rx = prev_r.x - 10.0;

        // Left: icon + filename + index pill
        let (icon, ic_col, title) = match &self.target {
            PreviewTarget::Single { name, is_dir: true, .. } => ("\u{E8B7}", t.folder, name.as_str()),
            PreviewTarget::Single { name, .. } => ("\u{E8A5}", t.text_dim, name.as_str()),
            PreviewTarget::Multi { .. } => ("\u{E762}", t.text_dim, "Selection"),
            PreviewTarget::Empty => ("\u{E890}", t.text_dim, "Quick Look"),
        };
        g.text(icon, Rect::new(r.x + 12.0, r.y, 18.0, r.h - 1.0), f(14.0).icon().c(), ic_col);

        let idx_w = idx_text.as_ref().map(|s| g.measure(s, t.font_mono()) + 14.0).unwrap_or(0.0);
        let tx = r.x + 34.0;
        let max_title_w = (rx - tx - idx_w - 8.0).max(40.0);
        let title_w = g.measure(title, t.font_md().b()).min(max_title_w);
        g.text(title, Rect::new(tx, r.y, max_title_w, r.h - 1.0), t.font_md().b(), t.text);

        if let Some(idx) = idx_text {
            let ix = tx + title_w + 8.0;
            if ix + idx_w <= rx {
                let ir = Rect::new(ix, r.y + 9.0, idx_w, 18.0);
                g.round(ir, t.radius_sm, t.field);
                g.round_stroke(ir, t.radius_sm, t.border, 1.0);
                g.text(&idx, ir, t.font_mono().c(), t.text_dim);
            }
        }

        act
    }

    fn paint_ql_footer(&self, g: &mut dyn Canvas, r: Rect, t: &Theme) {
        g.fill(r, t.panel);
        g.fill(Rect::new(r.x, r.y, r.w, 1.0), t.border);

        let cy = r.y + r.h / 2.0;
        let mut right = r.r() - 12.0;
        let font = t.font_small();
        for (keys, label) in [
            ("Esc", "Close"),
            ("Space", "Toggle"),
            ("Enter", "Open"),
            ("\u{2190}\u{2192}", "Step"),
        ] {
            let lw = g.measure(label, font) + 2.0;
            g.text(label, Rect::new(right - lw, r.y + 1.0, lw, r.h - 1.0), font, t.text_dim);
            right -= lw + 4.0;
            right -= keycaps_right(g, t, right, cy, keys) + 12.0;
        }

        let summary = self.footer_summary();
        let avail = (right - (r.x + 12.0)).max(0.0);
        if avail > 20.0 && !summary.is_empty() {
            g.text(&summary, Rect::new(r.x + 12.0, r.y + 1.0, avail, r.h - 1.0), font, t.text_dim);
        }
    }

    fn footer_summary(&self) -> String {
        let mut sz = String::new();
        match (&self.target, &self.loaded_kind) {
            (PreviewTarget::Multi { count, total_bytes, .. }, _) => {
                crate::data::fmt_size(*total_bytes, &mut sz);
                format!("{count} items selected \u{2014} {sz} total")
            }
            (PreviewTarget::Empty, _) => "No item selected".into(),
            (PreviewTarget::Single { is_dir: true, .. }, _) => "Folder".into(),
            (PreviewTarget::Single { in_archive: true, size, .. }, _) => {
                crate::data::fmt_size(*size, &mut sz);
                format!("Archive entry \u{00B7} {sz}")
            }
            (_, Some(PreviewKind::Image { orig_w, orig_h, format, file_size, .. })) => {
                crate::data::fmt_size(*file_size, &mut sz);
                format!("{orig_w} \u{00D7} {orig_h} \u{00B7} {format} \u{00B7} {sz}")
            }
            (_, Some(PreviewKind::Text { lines, language, encoding, total_bytes, .. })) => {
                crate::data::fmt_size(*total_bytes, &mut sz);
                format!("{language} \u{00B7} {encoding} \u{00B7} {} lines \u{00B7} {sz}", lines.len())
            }
            (_, Some(PreviewKind::Media { kind, file_size, .. })) => {
                crate::data::fmt_size(*file_size, &mut sz);
                let label = match kind {
                    MediaKind::Audio => "Audio",
                    MediaKind::Video => "Video",
                };
                format!("{label} \u{00B7} {sz}")
            }
            (_, Some(PreviewKind::Hex { total_bytes, magic, .. })) => {
                crate::data::fmt_size(*total_bytes, &mut sz);
                match magic {
                    Some(m) => format!("Hex ({m}) \u{00B7} {sz}"),
                    None => format!("Hex \u{00B7} {sz}"),
                }
            }
            (PreviewTarget::Single { size, .. }, _) => {
                crate::data::fmt_size(*size, &mut sz);
                sz
            }
        }
    }

    // ---- Shared content renderer (Docked Pane + Quick Look) -----------------

    fn paint_body(
        &mut self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        area: Rect,
        t: &Theme,
        cfg: &PreviewConfig,
        in_quick_look: bool,
    ) -> PreviewAction {
        if area.w <= 0.0 || area.h <= 0.0 {
            return PreviewAction::None;
        }

        match self.target.clone() {
            PreviewTarget::Empty => {
                paint_empty(g, t, area, in_quick_look);
                PreviewAction::None
            }
            PreviewTarget::Multi { count, total_bytes, samples } => {
                paint_multi(g, t, area, count, total_bytes, &samples);
                PreviewAction::None
            }
            PreviewTarget::Single { is_dir: true, .. } => {
                let _ = paint_unsupported(
                    ui,
                    g,
                    t,
                    area,
                    "Folders are shown in the list, not previewed.",
                    false,
                    0,
                    None,
                );
                PreviewAction::None
            }
            PreviewTarget::Single { in_archive: true, size, .. } => {
                let _ = paint_unsupported(
                    ui,
                    g,
                    t,
                    area,
                    "Extract archive entries to preview them.",
                    false,
                    size,
                    None,
                );
                PreviewAction::None
            }
            PreviewTarget::Single { path, .. } => {
                // If still loading and past the 80 ms threshold, show flat skeleton.
                let waiting_for_path = self.loaded_path.as_ref() != Some(&path) || self.loaded_kind.is_none();
                if waiting_for_path {
                    if self.loading_since.is_some_and(|s| s.elapsed() >= LOADING_DELAY) {
                        paint_loading(g, t, area);
                    } else {
                        g.fill(area, t.panel);
                    }
                    return PreviewAction::None;
                }

                let kind = self.loaded_kind.clone();
                let path_str = path.to_string_lossy().into_owned();
                match kind {
                    Some(PreviewKind::Image { bmp, orig_w, orig_h, format, file_size, modified }) => {
                        self.paint_image(ui, g, t, area, &bmp, orig_w, orig_h, &format, file_size, modified)
                    }
                    Some(PreviewKind::Text { lines, language, encoding, total_bytes, truncated }) => {
                        self.paint_text(
                            ui,
                            g,
                            t,
                            area,
                            &lines,
                            &language,
                            &encoding,
                            total_bytes,
                            truncated,
                            cfg.syntax_highlight,
                            &path_str,
                        )
                    }
                    Some(PreviewKind::Media { kind, rows, file_size }) => {
                        self.paint_media(ui, g, t, area, kind, &rows, file_size, &path_str, in_quick_look)
                    }
                    Some(PreviewKind::Hex { bytes, total_bytes, truncated, magic }) => {
                        self.paint_hex(
                            ui,
                            g,
                            t,
                            area,
                            &bytes,
                            total_bytes,
                            truncated,
                            magic.as_deref(),
                            cfg.hex_auto_width,
                            in_quick_look,
                            &path_str,
                        )
                    }
                    Some(PreviewKind::Unsupported { reason, can_hex, file_size }) => {
                        let act = paint_unsupported(
                            ui,
                            g,
                            t,
                            area,
                            &reason,
                            can_hex,
                            file_size,
                            Some(&path_str),
                        );
                        if matches!(act, UnsupportedOut::ForceHex) {
                            self.request_force_hex();
                            ui.request_repaint();
                            PreviewAction::None
                        } else if let UnsupportedOut::Open(p) = act {
                            PreviewAction::OpenFile(p)
                        } else {
                            PreviewAction::None
                        }
                    }
                    Some(PreviewKind::Error(msg)) => {
                        paint_error(g, t, area, &msg);
                        PreviewAction::None
                    }
                    None => PreviewAction::None,
                }
            }
        }
    }

    // ---- 1. Image mode ------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_image(
        &mut self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        t: &Theme,
        mut area: Rect,
        bmp: &tsubame_win::bitmap::Bitmap,
        orig_w: u32,
        orig_h: u32,
        format: &str,
        file_size: u64,
        modified: i64,
    ) -> PreviewAction {
        // Sub-bar: dimensions, format, Fit / 100% toggle
        let sub = area.cut_top(26.0);
        g.fill(sub, t.row);
        g.fill(Rect::new(sub.x, sub.b() - 1.0, sub.w, 1.0), t.border);

        let seg_r = Rect::new(sub.r() - 8.0 - 78.0, sub.y + 4.0, 78.0, 18.0);
        if let Some(actual) = two_seg(ui, g, t, ID_PV_FIT, ID_PV_ACTUAL, seg_r, "Fit", "100%", self.image_actual) {
            self.image_actual = actual;
            self.scroll = 0.0;
            ui.request_repaint();
        }

        let mut sx = sub.x + 8.0;
        let dim_text = format!("{orig_w} \u{00D7} {orig_h}");
        sx += draw_badge(g, t, sx, sub.y + 4.0, &dim_text, false) + 6.0;
        if !format.is_empty() && sx + 40.0 < seg_r.x {
            draw_badge(g, t, sx, sub.y + 4.0, format, false);
        }

        // Metadata strip pinned at bottom
        let meta_rows = if modified > 0 { 4 } else { 3 };
        let meta_h = (meta_rows as f32 * 20.0 + 14.0).min(area.h * 0.45);
        let meta_r = area.cut_bottom(meta_h);
        let stage = area;

        // Checkerboard stage
        let (ck_a, ck_b) = if t.is_dark() { (0x101520, 0x161D2B) } else { (0xF5F7FA, 0xE6EBF2) };
        g.fill(stage, ck_a);
        g.clip(stage);

        let bw = bmp.w.max(1) as f32;
        let bh = bmp.h.max(1) as f32;
        let dst = if self.image_actual {
            let iw = orig_w.max(bmp.w).max(1) as f32;
            let ih = orig_h.max(bmp.h).max(1) as f32;
            let max_y = (ih - stage.h).max(0.0) as f64;
            let wi = ui.interact(ID_PV_SCROLL, stage, Sense::hover().wheel());
            if wi.wheel.1 != 0.0 && max_y > 0.0 {
                self.scroll = (self.scroll - wi.wheel.1 as f64 * 48.0).clamp(0.0, max_y);
                ui.request_repaint();
            }
            self.scroll = self.scroll.clamp(0.0, max_y);
            let x = if iw < stage.w { stage.x + (stage.w - iw) / 2.0 } else { stage.x };
            let y = if ih < stage.h { stage.y + (stage.h - ih) / 2.0 } else { stage.y - self.scroll as f32 };
            Rect::new(x.round(), y.round(), iw, ih)
        } else {
            let avail_w = (stage.w - 24.0).max(16.0);
            let avail_h = (stage.h - 24.0).max(16.0);
            let scale = (avail_w / bw).min(avail_h / bh).min(1.0);
            let dw = (bw * scale).max(1.0).round();
            let dh = (bh * scale).max(1.0).round();
            let x = (stage.x + (stage.w - dw) / 2.0).round();
            let y = (stage.y + (stage.h - dh) / 2.0).round();
            Rect::new(x, y, dw, dh)
        };

        // Subtle checkerboard tiles behind the image rect (bounded count to keep paint tiny)
        let tile = 16.0;
        let cols = ((dst.w / tile).ceil() as usize).min(48);
        let rows = ((dst.h / tile).ceil() as usize).min(48);
        let tw = dst.w / cols.max(1) as f32;
        let th = dst.h / rows.max(1) as f32;
        for ry in 0..rows {
            for cx in 0..cols {
                if rx_parity(cx, ry) {
                    g.fill(Rect::new(dst.x + cx as f32 * tw, dst.y + ry as f32 * th, tw, th), ck_b);
                }
            }
        }

        let key = format!("pv-img-{}", self.image_key_seq);
        g.image(&key, bmp.w, bmp.h, &bmp.px, dst);
        g.round_stroke(dst, 0.0, t.border, 1.0);
        g.unclip();

        // Metadata strip
        g.fill(meta_r, t.panel);
        g.fill(Rect::new(meta_r.x, meta_r.y, meta_r.w, 1.0), t.border);
        let mut sz = String::new();
        crate::data::fmt_size(file_size, &mut sz);
        let mut my = meta_r.y + 7.0;
        let mut draw_meta = |k: &str, v: &str| {
            let rr = Rect::new(meta_r.x + 12.0, my, (meta_r.w - 24.0).max(40.0), 18.0);
            g.text(k, Rect::new(rr.x, rr.y, 90.0, rr.h), t.font_small(), t.text_dim);
            g.text(v, Rect::new(rr.x + 92.0, rr.y, (rr.w - 92.0).max(20.0), rr.h), t.font_small().r(), t.text);
            my += 20.0;
        };
        draw_meta("Dimensions", &dim_text);
        draw_meta("Format", if format.is_empty() { "Image" } else { format });
        draw_meta("File size", &sz);
        if modified > 0 {
            let mut date_s = String::new();
            let secs = (modified / 1000).clamp(0, u32::MAX as i64) as u32;
            crate::data::fmt_date(secs, &mut date_s);
            draw_meta("Modified", &date_s);
        }

        PreviewAction::None
    }

    // ---- 2. Text / Code mode ------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_text(
        &mut self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        t: &Theme,
        mut area: Rect,
        lines: &[tsubame_win::preview::TextLine],
        language: &str,
        encoding: &str,
        total_bytes: u64,
        truncated: bool,
        syntax_highlight: bool,
        path_str: &str,
    ) -> PreviewAction {
        let mut act = PreviewAction::None;

        // Sub-bar: language badge, encoding, line count, file size, Wrap toggle
        let sub = area.cut_top(26.0);
        g.fill(sub, t.row);
        g.fill(Rect::new(sub.x, sub.b() - 1.0, sub.w, 1.0), t.border);

        let wrap_w = 42.0;
        let wrap_r = Rect::new(sub.r() - 8.0 - wrap_w, sub.y + 4.0, wrap_w, 18.0);
        if toggle_chip(ui, g, t, ID_PV_WRAP, wrap_r, "Wrap", self.text_wrap) {
            self.text_wrap = !self.text_wrap;
            self.scroll = 0.0;
            ui.request_repaint();
        }

        let mut sx = sub.x + 8.0;
        sx += draw_badge(g, t, sx, sub.y + 4.0, language, true) + 6.0;
        let mut sz = String::new();
        crate::data::fmt_size(total_bytes, &mut sz);
        let info = format!("{encoding} \u{00B7} {} lines \u{00B7} {sz}", lines.len());
        let avail = (wrap_r.x - sx - 6.0).max(0.0);
        if avail > 20.0 {
            g.text(&info, Rect::new(sx, sub.y, avail, sub.h - 1.0), t.font_small(), t.text_dim);
        }

        // Truncation banner
        if truncated {
            let banner = area.cut_top(26.0);
            let msg = format!("Showing first 64 KB of {sz}");
            if paint_trunc_banner(ui, g, t, banner, &msg) {
                act = PreviewAction::OpenFile(path_str.to_string());
            }
        }

        // Code body with line-number gutter + vertical scrollbar
        g.fill(area, t.field);
        let bar_w = t.layout_scrollbar_width;
        let view_r = Rect::new(area.x, area.y, (area.w - bar_w).max(0.0), area.h);
        let bar_r = Rect::new(area.r() - bar_w, area.y, bar_w, area.h);

        let mono = t.font_mono();
        let line_h = (t.font_size_xs + 6.0).max(17.0);
        let gutter_w = 38.0;
        let code_x = view_r.x + gutter_w + 8.0;
        let code_w = (view_r.w - gutter_w - 14.0).max(20.0);
        let char_w = g.measure("M", mono).max(6.0);
        let wrap_cols = ((code_w / char_w).floor() as usize).max(12);

        // Compute visual rows (exact 1:1 when unwrap, chunked when wrap)
        let mut visual_rows: Vec<(usize, usize, usize)> = Vec::new();
        if self.text_wrap {
            for (li, line) in lines.iter().enumerate() {
                let n = line.chars();
                if n == 0 {
                    visual_rows.push((li, 0, 0));
                } else {
                    let mut start = 0;
                    while start < n {
                        let end = (start + wrap_cols).min(n);
                        visual_rows.push((li, start, end));
                        start = end;
                    }
                }
            }
        }

        let total_rows = if self.text_wrap { visual_rows.len() } else { lines.len() };
        self.content_h = total_rows as f64 * line_h as f64 + 8.0;
        let max_scroll = (self.content_h - view_r.h as f64).max(0.0);

        let wi = ui.interact(ID_PV_SCROLL, view_r, Sense::hover().wheel());
        if wi.wheel.1 != 0.0 {
            self.scroll = (self.scroll - wi.wheel.1 as f64 * line_h as f64 * 3.0).clamp(0.0, max_scroll);
            ui.request_repaint();
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        g.clip(view_r);
        // Gutter background & divider
        g.fill(Rect::new(view_r.x, view_r.y, gutter_w, view_r.h), t.row_alt);
        g.fill(Rect::new(view_r.x + gutter_w, view_r.y, 1.0, view_r.h), t.border);

        let first = (self.scroll / line_h as f64).floor().max(0.0) as usize;
        let vis_count = (view_r.h / line_h).ceil() as usize + 2;
        let last = (first + vis_count).min(total_rows);
        let dark = t.is_dark();

        for row_idx in first..last {
            let y = view_r.y + 4.0 + row_idx as f32 * line_h - self.scroll as f32;
            let (li, c_start, c_end) = if self.text_wrap {
                visual_rows[row_idx]
            } else {
                (row_idx, 0, lines[row_idx].chars())
            };
            if c_start == 0 {
                let ln = format!("{}", li + 1);
                g.text(&ln, Rect::new(view_r.x + 2.0, y, gutter_w - 8.0, line_h), mono.r(), t.text_disabled);
            }
            let line = &lines[li];
            if line.text.is_empty() || c_start >= c_end {
                continue;
            }
            if !syntax_highlight || line.spans.is_empty() {
                let slice = char_slice(&line.text, c_start, c_end);
                g.text(slice, Rect::new(code_x, y, code_w + 400.0, line_h), mono, t.text);
            } else {
                // Draw each intersecting token span
                let mut cx = code_x;
                let mut pos = 0usize;
                for span in &line.spans {
                    let s_end = pos + span.len as usize;
                    let a = pos.max(c_start);
                    let b = s_end.min(c_end);
                    if a < b {
                        let seg = char_slice(&line.text, a, b);
                        let seg_w = g.measure(seg, mono);
                        let col = token_color(span.kind, t, dark);
                        g.text(seg, Rect::new(cx, y, seg_w + 4.0, line_h), mono, col);
                        cx += seg_w;
                        if cx > view_r.r() + 4.0 {
                            break;
                        }
                    }
                    pos = s_end;
                    if pos >= c_end {
                        break;
                    }
                }
            }
        }
        g.unclip();

        if self.content_h > view_r.h as f64 {
            let m = ScrollMetrics { content: self.content_h, view: view_r.h as f64, page: (view_r.h as f64 * 0.85).max(line_h as f64) };
            if vscrollbar(ui, g, ID_PV_VBAR, bar_r, m, &mut self.scroll) {
                ui.request_repaint();
            }
        } else {
            g.fill(bar_r, t.field);
        }

        act
    }

    // ---- 3. Media info mode -------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_media(
        &mut self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        t: &Theme,
        mut area: Rect,
        kind: MediaKind,
        rows: &[(String, String)],
        file_size: u64,
        path_str: &str,
        in_quick_look: bool,
    ) -> PreviewAction {
        let mut act = PreviewAction::None;

        // Sub-bar
        let sub = area.cut_top(26.0);
        g.fill(sub, t.row);
        g.fill(Rect::new(sub.x, sub.b() - 1.0, sub.w, 1.0), t.border);
        let badge = match kind {
            MediaKind::Video => "VIDEO",
            MediaKind::Audio => "AUDIO",
        };
        let mut sx = sub.x + 8.0;
        sx += draw_badge(g, t, sx, sub.y + 4.0, badge, true) + 6.0;
        let mut sz = String::new();
        crate::data::fmt_size(file_size, &mut sz);
        g.text(&sz, Rect::new(sx, sub.y, (sub.r() - sx - 64.0).max(20.0), sub.h - 1.0), t.font_small(), t.text_dim);

        let open_r = Rect::new(sub.r() - 8.0 - 52.0, sub.y + 4.0, 52.0, 18.0);
        if pill_btn(ui, g, t, ID_PV_OPEN.with(1), open_r, "Open", true, true) {
            act = PreviewAction::OpenFile(path_str.to_string());
        }

        // Split side-by-side in wide Quick Look, stacked in docked pane
        let side_by_side = in_quick_look && area.w >= 620.0;
        let (hero_r, table_r) = if side_by_side {
            let right_w = 300.0;
            let left = Rect::new(area.x, area.y, area.w - right_w, area.h);
            let right = Rect::new(left.r(), area.y, right_w, area.h);
            (left, right)
        } else {
            let hero_h = ((area.w - 24.0) * 9.0 / 16.0 + 30.0).clamp(110.0, 190.0).min(area.h * 0.48);
            let top = area.cut_top(hero_h);
            (top, area)
        };

        // Hero card (icon / badge / duration)
        g.fill(hero_r, t.field);
        if side_by_side {
            g.fill(Rect::new(hero_r.r() - 1.0, hero_r.y, 1.0, hero_r.h), t.border);
        } else {
            g.fill(Rect::new(hero_r.x, hero_r.b() - 1.0, hero_r.w, 1.0), t.border);
        }
        let poster = hero_r.pad(12.0);
        g.round(poster, t.radius_md, t.bg);
        g.round_stroke(poster, t.radius_md, t.border, 1.0);
        let glyph = match kind {
            MediaKind::Video => "\u{E714}",
            MediaKind::Audio => "\u{E8D6}",
        };
        g.text(glyph, poster, f(36.0).icon().c(), t.accent);
        let container = rows.iter().find(|(k, _)| k == "Container").map(|(_, v)| v.as_str()).unwrap_or(badge);
        draw_badge(g, t, poster.x + 8.0, poster.y + 8.0, container, false);
        if let Some((_, dur)) = rows.iter().find(|(k, _)| k == "Duration") {
            let dw = g.measure(dur, t.font_mono()) + 12.0;
            draw_badge(g, t, poster.r() - dw - 8.0, poster.b() - 25.0, dur, false);
        }

        // Structured key/value metadata table
        g.fill(table_r, t.panel);
        let bar_w = t.layout_scrollbar_width;
        let view_r = Rect::new(table_r.x, table_r.y, (table_r.w - bar_w).max(0.0), table_r.h);
        let bar_r = Rect::new(table_r.r() - bar_w, table_r.y, bar_w, table_r.h);
        let row_h = 24.0;
        let head_h = 24.0;
        self.content_h = head_h as f64 + rows.len() as f64 * row_h as f64 + 8.0;
        let max_scroll = (self.content_h - view_r.h as f64).max(0.0);

        let wi = ui.interact(ID_PV_SCROLL, view_r, Sense::hover().wheel());
        if wi.wheel.1 != 0.0 {
            self.scroll = (self.scroll - wi.wheel.1 as f64 * row_h as f64 * 3.0).clamp(0.0, max_scroll);
            ui.request_repaint();
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        g.clip(view_r);
        let mut y = view_r.y - self.scroll as f32;
        let sec_r = Rect::new(view_r.x, y, view_r.w, head_h);
        g.fill(sec_r, t.panel);
        g.fill(Rect::new(sec_r.x, sec_r.b() - 1.0, sec_r.w, 1.0), t.border);
        g.text("MEDIA INFO", Rect::new(sec_r.x + 12.0, sec_r.y, sec_r.w - 24.0, sec_r.h), t.font_small().b(), t.text_dim);
        y += head_h;

        for (i, (k, v)) in rows.iter().enumerate() {
            let rr = Rect::new(view_r.x, y, view_r.w, row_h);
            if rr.b() >= view_r.y && rr.y <= view_r.b() {
                g.fill(rr, if i % 2 == 0 { t.row } else { t.row_alt });
                g.fill(Rect::new(rr.x, rr.b() - 1.0, rr.w, 1.0), t.border);
                let kw = (rr.w * 0.42).clamp(76.0, 120.0);
                g.text(k, Rect::new(rr.x + 12.0, rr.y, kw, rr.h), t.font_small(), t.text_dim);
                g.text(v, Rect::new(rr.x + 12.0 + kw, rr.y, (rr.w - kw - 24.0).max(20.0), rr.h), t.font_mono().r(), t.text);
            }
            y += row_h;
        }
        g.unclip();

        if self.content_h > view_r.h as f64 {
            let m = ScrollMetrics { content: self.content_h, view: view_r.h as f64, page: (view_r.h as f64 * 0.85).max(row_h as f64) };
            if vscrollbar(ui, g, ID_PV_VBAR, bar_r, m, &mut self.scroll) {
                ui.request_repaint();
            }
        } else {
            g.fill(bar_r, t.panel);
        }

        act
    }

    // ---- 4. Hex dump mode ---------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_hex(
        &mut self,
        ui: &mut Ui,
        g: &mut dyn Canvas,
        t: &Theme,
        mut area: Rect,
        bytes: &[u8],
        total_bytes: u64,
        truncated: bool,
        magic: Option<&str>,
        hex_auto_width: bool,
        in_quick_look: bool,
        path_str: &str,
    ) -> PreviewAction {
        let mut act = PreviewAction::None;

        // Effective bytes per row: manual toggle > Quick Look (16) > auto based on pane width
        let auto_cols = if in_quick_look {
            HEX_WIDE_BYTES_PER_ROW
        } else {
            bytes_per_row(area.w.round().max(0.0) as u32, hex_auto_width)
        };
        let per_row = self.hex_override_cols.unwrap_or(auto_cols);

        // Sub-bar: magic badge, size, 8 B / 16 B segmented toggle
        let sub = area.cut_top(26.0);
        g.fill(sub, t.row);
        g.fill(Rect::new(sub.x, sub.b() - 1.0, sub.w, 1.0), t.border);

        let seg_r = Rect::new(sub.r() - 8.0 - 76.0, sub.y + 4.0, 76.0, 18.0);
        if let Some(wide) = two_seg(
            ui,
            g,
            t,
            ID_PV_HEX8,
            ID_PV_HEX16,
            seg_r,
            "8 B",
            "16 B",
            per_row == HEX_WIDE_BYTES_PER_ROW,
        ) {
            self.hex_override_cols = Some(if wide { HEX_WIDE_BYTES_PER_ROW } else { HEX_NARROW_BYTES_PER_ROW });
            self.scroll = 0.0;
            ui.request_repaint();
        }

        let mut sx = sub.x + 8.0;
        if let Some(m) = magic {
            sx += draw_badge(g, t, sx, sub.y + 4.0, m, true) + 6.0;
        } else {
            sx += draw_badge(g, t, sx, sub.y + 4.0, "HEX", false) + 6.0;
        }
        let mut sz = String::new();
        crate::data::fmt_size(total_bytes, &mut sz);
        let avail = (seg_r.x - sx - 6.0).max(0.0);
        if avail > 20.0 {
            g.text(&sz, Rect::new(sx, sub.y, avail, sub.h - 1.0), t.font_small(), t.text_dim);
        }

        if truncated {
            let banner = area.cut_top(26.0);
            let msg = format!("Showing first 16 KB of {sz}");
            if paint_trunc_banner(ui, g, t, banner, &msg) {
                act = PreviewAction::OpenFile(path_str.to_string());
            }
        }

        // Column header
        let head = area.cut_top(22.0);
        let mono = t.font_mono();
        let off_w = g.measure("00000000", mono) + 12.0;
        let hex_sample = if per_row == 8 {
            "00 01 02 03 04 05 06 07"
        } else {
            "00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F"
        };
        let hex_w = g.measure(hex_sample, mono) + 14.0;

        g.fill(head, t.panel);
        g.fill(Rect::new(head.x, head.b() - 1.0, head.w, 1.0), t.border);
        g.clip(head);
        g.text("OFFSET", Rect::new(head.x + 6.0, head.y, off_w - 6.0, head.h), mono, t.text_dim);
        g.fill(Rect::new(head.x + off_w, head.y, 1.0, head.h), t.border);
        g.text(hex_sample, Rect::new(head.x + off_w + 7.0, head.y, hex_w - 7.0, head.h), mono, t.text_dim);
        g.fill(Rect::new(head.x + off_w + hex_w, head.y, 1.0, head.h), t.border);
        g.text("ASCII", Rect::new(head.x + off_w + hex_w + 7.0, head.y, 80.0, head.h), mono, t.text_dim);
        g.unclip();

        // Virtualized hex rows
        g.fill(area, t.field);
        let bar_w = t.layout_scrollbar_width;
        let view_r = Rect::new(area.x, area.y, (area.w - bar_w).max(0.0), area.h);
        let bar_r = Rect::new(area.r() - bar_w, area.y, bar_w, area.h);

        let row_h = (t.font_size_xs + 6.0).max(17.0);
        let total_rows = bytes.len().div_ceil(per_row);
        self.content_h = total_rows as f64 * row_h as f64 + 6.0;
        let max_scroll = (self.content_h - view_r.h as f64).max(0.0);

        let wi = ui.interact(ID_PV_SCROLL, view_r, Sense::hover().wheel());
        if wi.wheel.1 != 0.0 {
            self.scroll = (self.scroll - wi.wheel.1 as f64 * row_h as f64 * 3.0).clamp(0.0, max_scroll);
            ui.request_repaint();
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        g.clip(view_r);
        let first = (self.scroll / row_h as f64).floor().max(0.0) as usize;
        let vis_count = (view_r.h / row_h).ceil() as usize + 2;
        let last = (first + vis_count).min(total_rows);

        for row_idx in first..last {
            let y = view_r.y + row_idx as f32 * row_h - self.scroll as f32;
            let rr = Rect::new(view_r.x, y, view_r.w, row_h);
            g.fill(rr, if row_idx % 2 == 0 { t.row } else { t.row_alt });

            let start = row_idx * per_row;
            let end = (start + per_row).min(bytes.len());
            let row = hex::format_row(start as u64, &bytes[start..end], per_row);

            g.text(&row.offset, Rect::new(rr.x + 6.0, y, off_w - 6.0, row_h), mono, t.accent);
            g.fill(Rect::new(rr.x + off_w, y, 1.0, row_h), t.border);
            g.text(&row.hex, Rect::new(rr.x + off_w + 7.0, y, hex_w + 8.0, row_h), mono, t.text);
            g.fill(Rect::new(rr.x + off_w + hex_w, y, 1.0, row_h), t.border);
            let ascii_x = rr.x + off_w + hex_w + 7.0;
            g.text(&row.ascii, Rect::new(ascii_x, y, (rr.r() - ascii_x).max(16.0), row_h), mono, t.text_dim);
        }
        g.unclip();

        if self.content_h > view_r.h as f64 {
            let m = ScrollMetrics { content: self.content_h, view: view_r.h as f64, page: (view_r.h as f64 * 0.85).max(row_h as f64) };
            if vscrollbar(ui, g, ID_PV_VBAR, bar_r, m, &mut self.scroll) {
                ui.request_repaint();
            }
        } else {
            g.fill(bar_r, t.field);
        }

        act
    }
}

// ---- 5. Empty / Multi-select / Unsupported / Error / Loading ----------------

fn paint_empty(g: &mut dyn Canvas, t: &Theme, area: Rect, in_quick_look: bool) {
    g.fill(area, t.panel);
    let cy = area.y + area.h * 0.44;
    g.text("\u{E890}", Rect::new(area.x, cy - 42.0, area.w, 40.0), f(36.0).icon().c(), t.text_disabled);
    g.text("No item selected", Rect::new(area.x + 12.0, cy + 4.0, (area.w - 24.0).max(20.0), 22.0), t.font_md().b().c(), t.text);
    let hint = if in_quick_look { "Press Space or Esc to close" } else { "Select a file to preview (Alt+P to hide)" };
    g.text(hint, Rect::new(area.x + 16.0, cy + 28.0, (area.w - 32.0).max(20.0), 18.0), t.font_small().c(), t.text_dim);
}

fn paint_multi(
    g: &mut dyn Canvas,
    t: &Theme,
    area: Rect,
    count: usize,
    total_bytes: u64,
    samples: &[(String, u64, bool)],
) {
    g.fill(area, t.panel);
    let mut sz = String::new();
    crate::data::fmt_size(total_bytes, &mut sz);
    let title = format!("{count} items selected \u{2014} {sz} total");
    let cy = area.y + (area.h * 0.30).max(48.0);
    g.text("\u{E762}", Rect::new(area.x, cy - 38.0, area.w, 36.0), f(34.0).icon().c(), t.text_disabled);
    g.text(&title, Rect::new(area.x + 12.0, cy + 2.0, (area.w - 24.0).max(20.0), 22.0), t.font_md().b().c(), t.text);
    g.text("Select a single file to preview", Rect::new(area.x + 12.0, cy + 24.0, (area.w - 24.0).max(20.0), 18.0), t.font_small().c(), t.text_dim);

    if !samples.is_empty() && area.h > 180.0 {
        let card_w = (area.w - 32.0).clamp(160.0, 264.0);
        let row_h = 24.0;
        let card_h = samples.len() as f32 * row_h;
        let card_r = Rect::new(area.x + (area.w - card_w) / 2.0, cy + 54.0, card_w, card_h);
        if card_r.b() <= area.b() - 12.0 {
            g.round(card_r, t.radius_md, t.row);
            g.round_stroke(card_r, t.radius_md, t.border, 1.0);
            for (i, (name, size, is_dir)) in samples.iter().enumerate() {
                let rr = Rect::new(card_r.x, card_r.y + i as f32 * row_h, card_r.w, row_h);
                if i > 0 {
                    g.fill(Rect::new(rr.x + 1.0, rr.y, rr.w - 2.0, 1.0), t.border);
                }
                let ic = if *is_dir { "\u{E8B7}" } else { "\u{E8A5}" };
                let ic_c = if *is_dir { t.folder } else { t.text_dim };
                g.text(ic, Rect::new(rr.x + 8.0, rr.y, 14.0, row_h), f(12.0).icon().c(), ic_c);
                let sw = if *is_dir {
                    0.0
                } else {
                    crate::data::fmt_size(*size, &mut sz);
                    g.text(&sz, Rect::new(rr.r() - 62.0, rr.y, 54.0, row_h), t.font_small().r(), t.text_dim);
                    64.0
                };
                g.text(name, Rect::new(rr.x + 26.0, rr.y, (rr.w - 34.0 - sw).max(20.0), row_h), t.font_small(), t.text);
            }
        }
    }
}

enum UnsupportedOut {
    None,
    Open(String),
    ForceHex,
}

#[allow(clippy::too_many_arguments)]
fn paint_unsupported(
    ui: &mut Ui,
    g: &mut dyn Canvas,
    t: &Theme,
    area: Rect,
    reason: &str,
    can_hex: bool,
    file_size: u64,
    open_path: Option<&str>,
) -> UnsupportedOut {
    g.fill(area, t.panel);
    let cy = area.y + area.h * 0.42;
    g.text("\u{E8A5}", Rect::new(area.x, cy - 42.0, area.w, 40.0), f(36.0).icon().c(), t.text_disabled);
    g.text("No preview available", Rect::new(area.x + 12.0, cy + 4.0, (area.w - 24.0).max(20.0), 22.0), t.font_md().b().c(), t.text);

    let sub = if file_size > 0 {
        let mut sz = String::new();
        crate::data::fmt_size(file_size, &mut sz);
        format!("{reason} ({sz})")
    } else {
        reason.to_string()
    };
    g.text(&sub, Rect::new(area.x + 16.0, cy + 28.0, (area.w - 32.0).max(20.0), 18.0), t.font_small().c(), t.text_dim);

    let mut out = UnsupportedOut::None;
    let by = cy + 58.0;
    let bh = 26.0;
    let open_w = if open_path.is_some() { g.measure("Open", t.font_small().b()) + 24.0 } else { 0.0 };
    let hex_w = if can_hex { g.measure("View as Hex", t.font_small()) + 22.0 } else { 0.0 };
    let gap = if open_w > 0.0 && hex_w > 0.0 { 8.0 } else { 0.0 };
    let total_w = open_w + gap + hex_w;
    let mut bx = (area.x + (area.w - total_w) / 2.0).round();

    if let Some(p) = open_path {
        let r = Rect::new(bx, by, open_w, bh);
        if pill_btn(ui, g, t, ID_PV_OPEN.with(2), r, "Open", true, true) {
            out = UnsupportedOut::Open(p.to_string());
        }
        bx += open_w + gap;
    }
    if can_hex {
        let r = Rect::new(bx, by, hex_w, bh);
        if pill_btn(ui, g, t, ID_PV_HEX, r, "View as Hex", false, true) {
            out = UnsupportedOut::ForceHex;
        }
    }
    out
}

fn paint_error(g: &mut dyn Canvas, t: &Theme, area: Rect, msg: &str) {
    g.fill(area, t.panel);
    let cy = area.y + area.h * 0.44;
    g.text("\u{E783}", Rect::new(area.x, cy - 42.0, area.w, 40.0), f(36.0).icon().c(), t.danger);
    g.text("Can't preview file", Rect::new(area.x + 12.0, cy + 4.0, (area.w - 24.0).max(20.0), 22.0), t.font_md().b().c(), t.text);
    g.text(msg, Rect::new(area.x + 16.0, cy + 28.0, (area.w - 32.0).max(20.0), 18.0), t.font_small().c(), t.text_dim);
}

fn paint_loading(g: &mut dyn Canvas, t: &Theme, mut area: Rect) {
    g.fill(area, t.panel);
    let bar = area.cut_top(t.height_progress_line);
    g.fill(bar, t.border);
    g.fill(Rect::new(bar.x, bar.y, bar.w * 0.58, bar.h), t.accent);

    let (sk_a, sk_b) = if t.is_dark() { (0x1A2231, 0x222C3F) } else { (0xE4E9F1, 0xD6DDE8) };
    let pad = area.pad(14.0);
    let box_h = 140.0_f32.min((pad.h - 48.0).max(40.0));
    let sk_box = Rect::new(pad.x, pad.y, pad.w, box_h);
    g.round(sk_box, t.radius_md, sk_a);
    g.round_stroke(sk_box, t.radius_md, t.border, 1.0);
    g.text("Loading preview\u{2026}", sk_box, t.font_small().c(), t.text_dim);

    let mut y = sk_box.b() + 14.0;
    for frac in [0.72_f32, 0.54, 0.84, 0.40] {
        if y + 10.0 > pad.b() {
            break;
        }
        g.round(Rect::new(pad.x, y, (pad.w * frac).max(20.0), 10.0), 2.0, sk_b);
        y += 18.0;
    }
}

// ---- Small flat Direct2D helpers -------------------------------------------

fn rx_parity(x: usize, y: usize) -> bool {
    (x + y) % 2 == 1
}

/// 6-token syntax tinting colours matching `mocks/mock-preview.html` (dark + light).
pub fn token_color(kind: TokenKind, t: &Theme, dark: bool) -> u32 {
    if dark {
        match kind {
            TokenKind::Plain => t.text,
            TokenKind::Keyword => 0x7FB0FF,
            TokenKind::Type => 0x5EC4B6,
            TokenKind::String => 0xD9A76A,
            TokenKind::Number => 0xC49BFF,
            TokenKind::Comment => 0x6E7B91,
        }
    } else {
        match kind {
            TokenKind::Plain => t.text,
            TokenKind::Keyword => 0x1B52BF,
            TokenKind::Type => 0x0F766E,
            TokenKind::String => 0x9A4D12,
            TokenKind::Number => 0x6D28D9,
            TokenKind::Comment => 0x6E788A,
        }
    }
}

/// Slices `s` by character indices `start..end`.
fn char_slice(s: &str, start: usize, end: usize) -> &str {
    let mut b0 = s.len();
    let mut b1 = s.len();
    for (ci, (bi, _)) in s.char_indices().enumerate() {
        if ci == start {
            b0 = bi;
        }
        if ci == end {
            b1 = bi;
            break;
        }
    }
    if b0 > b1 { "" } else { &s[b0..b1] }
}

fn draw_badge(g: &mut dyn Canvas, t: &Theme, x: f32, y: f32, label: &str, accent: bool) -> f32 {
    let font = t.font_mono();
    let w = g.measure(label, font) + 12.0;
    let r = Rect::new(x, y, w, 17.0);
    g.round(r, t.radius_sm, t.panel);
    g.round_stroke(r, t.radius_sm, if accent { t.accent } else { t.border }, 1.0);
    g.text(label, r, font.c(), if accent { t.accent } else { t.text });
    w
}

fn paint_trunc_banner(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, r: Rect, msg: &str) -> bool {
    g.fill(r, t.selected_inactive);
    g.fill(Rect::new(r.x, r.b() - 1.0, r.w, 1.0), t.border);
    let open_w = g.measure("Open full", t.font_small().b()) + 12.0;
    let open_r = Rect::new(r.r() - 8.0 - open_w, r.y + 3.0, open_w, 19.0);
    let it = ui.interact(ID_PV_OPEN.with(3), open_r, Sense::click().cursor(Cursor::Hand));
    if it.hovered {
        g.round(open_r, t.radius_sm, t.hover);
    }
    g.text("Open full", open_r, t.font_small().b().c(), t.accent);
    g.text(msg, Rect::new(r.x + 8.0, r.y, (open_r.x - r.x - 12.0).max(20.0), r.h - 1.0), t.font_small(), t.text);
    it.clicked == Some(Button::Left)
}

fn small_icon_btn(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, glyph: &str) -> bool {
    let it = ui.interact(id, r, Sense::click());
    if it.hovered {
        g.round(r, t.radius_sm, t.hover);
    }
    g.text(glyph, r, f(11.0).icon().c(), if it.hovered { t.text } else { t.text_dim });
    it.clicked == Some(Button::Left)
}

fn toggle_chip(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, label: &str, on: bool) -> bool {
    let it = ui.interact(id, r, Sense::click().cursor(Cursor::Hand));
    let bg = if on { t.selected } else if it.hovered { t.hover } else { t.panel };
    let border = if on { t.focus } else { t.border };
    g.round(r, t.radius_sm, bg);
    g.round_stroke(r, t.radius_sm, border, 1.0);
    g.text(label, r, t.font_small().c(), if on || it.hovered { t.text } else { t.text_dim });
    it.clicked == Some(Button::Left)
}

#[allow(clippy::too_many_arguments)]
fn two_seg(
    ui: &mut Ui,
    g: &mut dyn Canvas,
    t: &Theme,
    id_a: Id,
    id_b: Id,
    r: Rect,
    la: &str,
    lb: &str,
    second_on: bool,
) -> Option<bool> {
    let half = (r.w / 2.0).floor();
    let ra = Rect::new(r.x, r.y, half, r.h);
    let rb = Rect::new(r.x + half, r.y, r.w - half, r.h);
    let ia = ui.interact(id_a, ra, Sense::click().cursor(Cursor::Hand));
    let ib = ui.interact(id_b, rb, Sense::click().cursor(Cursor::Hand));

    g.round(r, t.radius_sm, t.panel);
    if !second_on {
        g.fill(ra.pad(1.0), t.selected);
    } else if ia.hovered {
        g.fill(ra.pad(1.0), t.hover);
    }
    if second_on {
        g.fill(rb.pad(1.0), t.selected);
    } else if ib.hovered {
        g.fill(rb.pad(1.0), t.hover);
    }
    g.fill(Rect::new(rb.x, r.y + 1.0, 1.0, r.h - 2.0), t.border);
    g.round_stroke(r, t.radius_sm, t.border, 1.0);

    let fa = if !second_on { t.font_small().b().c() } else { t.font_small().c() };
    let fb = if second_on { t.font_small().b().c() } else { t.font_small().c() };
    g.text(la, ra, fa, if !second_on || ia.hovered { t.text } else { t.text_dim });
    g.text(lb, rb, fb, if second_on || ib.hovered { t.text } else { t.text_dim });

    if ia.clicked == Some(Button::Left) && second_on {
        Some(false)
    } else if ib.clicked == Some(Button::Left) && !second_on {
        Some(true)
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn pill_btn(
    ui: &mut Ui,
    g: &mut dyn Canvas,
    t: &Theme,
    id: Id,
    r: Rect,
    label: &str,
    primary: bool,
    enabled: bool,
) -> bool {
    let it = ui.interact(id, r, if enabled { Sense::click().cursor(Cursor::Hand) } else { Sense::hover() });
    let (bg, border, fg) = if primary {
        (t.accent, None, t.on_accent)
    } else {
        (if it.hovered && enabled { t.hover } else { t.row }, Some(t.popup_border), if enabled { t.text } else { t.text_disabled })
    };
    g.round(r, t.radius_md, bg);
    if let Some(b) = border {
        g.round_stroke(r, t.radius_md, b, 1.0);
    }
    g.text(label, r, if primary { t.font_small().b().c() } else { t.font_small().c() }, fg);
    enabled && it.clicked == Some(Button::Left)
}

fn nav_step_btn(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, r: Rect, glyph: &str, enabled: bool) -> bool {
    let it = ui.interact(id, r, if enabled { Sense::click().cursor(Cursor::Hand) } else { Sense::hover() });
    if enabled && it.hovered {
        g.fill(r.pad(1.0), t.hover);
    }
    g.text(glyph, r, t.font_md().c(), if enabled { t.text } else { t.text_disabled });
    enabled && it.clicked == Some(Button::Left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_clamps_to_bounds_and_half_window() {
        assert_eq!(PreviewState::clamp_width(100.0, 1120.0), 220.0);
        assert_eq!(PreviewState::clamp_width(320.0, 1120.0), 320.0);
        assert_eq!(PreviewState::clamp_width(900.0, 1120.0), 560.0);
        assert_eq!(PreviewState::clamp_width(900.0, 1600.0), 600.0);
    }

    #[test]
    fn char_slice_handles_utf8_boundaries() {
        assert_eq!(char_slice("héllo", 0, 2), "hé");
        assert_eq!(char_slice("héllo", 2, 5), "llo");
        assert_eq!(char_slice("héllo", 5, 5), "");
    }

    #[test]
    fn syntax_tokens_differ_across_dark_and_light() {
        let dark = Theme::dark();
        let light = Theme::light();
        assert_ne!(token_color(TokenKind::Keyword, &dark, true), token_color(TokenKind::Keyword, &light, false));
        assert_eq!(token_color(TokenKind::Plain, &dark, true), dark.text);
        assert_eq!(token_color(TokenKind::Plain, &light, false), light.text);
    }
}
