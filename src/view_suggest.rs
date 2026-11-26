//! B11-H the view's side of the path-bar extras: the autocomplete dropdown under the path
//! edit (model and tests in `path_suggest.rs`), the path bar's right-click menu, "Open a terminal
//! here", "Copy address" (plain and quoted) and "Paste and go".
//!
//! The dropdown is own-drawn on top of the frame and never takes the keyboard focus: the path
//! field keeps it, so typing goes on while the list is open. Its keys are read from the field's
//! key queue before the field sees them (`suggest_keys`) and eaten when they are the list's.
//! Rows are not focusable, so a click on one leaves the field's focus alone and opens the folder.
//! Nothing here runs on a timer except the one-shot 80 ms debounce of `PathSuggest`.

use super::*;
use crate::path_suggest::{self as ps, ItemKind, KeyOut};
use crate::uia_rows::{self, SuggestUia};
use std::time::Instant;

const ID_SUGGEST: Id = Id::new("path-suggest");
/// Padding above the first and below the last row (DIPs).
const PAD_Y: f32 = 4.0;
/// Narrowest the dropdown is drawn, so a short path bar still shows names.
const MIN_W: f32 = 280.0;

#[cfg(test)]
thread_local! {
    /// tests: the folders a terminal was asked for, instead of starting one
    pub(crate) static TERMINALS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn launch_terminal(dir: String) {
    #[cfg(test)]
    TERMINALS.with(|t| t.borrow_mut().push(dir));
    #[cfg(not(test))]
    crate::shellops::open_terminal(dir);
}

/// What the dropdown's keys asked for this frame.
#[derive(Default)]
pub(super) struct SuggestAct {
    /// replace the field's text (Tab, Right)
    complete: Option<String>,
    /// open this folder (Enter on a highlighted row)
    go: Option<String>,
}

impl View {
    /// The field is being painted: starts a session for it (places snapshot, first listing)
    /// when it has none, and remembers where the bar is.
    pub(super) fn suggest_sync(&mut self, bar: Rect) {
        self.psug.bar = bar;
        if self.psug.active && self.psug.slot == self.live {
            return;
        }
        let drives = self.side.drives.iter().map(|d| (d.root.clone(), d.label.clone())).collect();
        let recents = self.side.recents.clone();
        let text = self.path_text.text.clone();
        self.psug.begin(self.live, &text, drives, recents, &ps::system_env);
    }

    /// Reads the dropdown's keys from the field's queue (before the field sees them).
    pub(super) fn suggest_keys(&mut self, ui: &mut Ui) -> SuggestAct {
        let mut act = SuggestAct::default();
        if !self.psug.visible() {
            return act;
        }
        // Tab belongs to the list while it shows (the window would move the focus)
        ui.claimed.push((Key::Tab, Mods::NONE));
        ui.claimed.push((Key::Tab, Mods::SHIFT));
        let t = &self.path_text;
        let at_end = t.caret == t.text.len() && t.anchor == t.caret;
        for (i, ev) in ui.key_input(pid(self.live, ID_PATH_EDIT)) {
            let Event::KeyDown { key, mods, .. } = ev else { continue };
            match self.psug.key(key, mods, at_end) {
                KeyOut::Pass => {}
                KeyOut::Eat => ui.consume(i),
                KeyOut::Complete(s) => {
                    ui.consume(i);
                    act.complete = Some(s);
                    break;
                }
                KeyOut::Go(p) => {
                    ui.consume(i);
                    act.go = Some(p);
                    break;
                }
            }
        }
        act
    }

    /// After the field handled the frame: applies a completion or an open, and feeds the
    /// typed text to the dropdown.
    pub(super) fn suggest_after(&mut self, ui: &mut Ui, resp: tsubame_ui::TextResponse, act: SuggestAct) -> tsubame_ui::TextResponse {
        let now = Instant::now();
        if let Some(s) = act.complete {
            self.path_text.set_text(&s);
            self.path_error = None;
            // the new parent is listed at once: the next level shows without a debounce
            self.psug.typed(&s, now, true, &ps::system_env);
            ui.request_repaint();
        } else if let Some(p) = act.go {
            self.path_text.set_text(&p);
            self.submit_path();
            ui.request_repaint();
        } else if resp.changed {
            let text = self.path_text.text.clone();
            self.psug.typed(&text, now, false, &ps::system_env);
        }
        resp
    }

    /// The dropdown, on top of everything (called after the path error box).
    pub(super) fn paint_path_suggest(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme) {
        if !self.editing || self.psug.slot != self.live || !self.psug.visible() {
            return;
        }
        let bar = self.psug.bar;
        let row_h = t.height_list_row;
        let n = self.psug.items.len();
        let area = Rect::new(bar.x, bar.b() + 2.0, bar.w.max(MIN_W), row_h * n as f32 + 2.0 * PAD_Y);
        g.round(area, t.radius_md, t.popup);
        g.round_stroke(area, t.radius_md, t.popup_border, 1.0);
        // padding clicks stay in the dropdown (they must not reach what is under it)
        ui.interact(pid(self.live, ID_SUGGEST), area, Sense::click());
        let mut open: Option<String> = None;
        let a11y = uia_rows::on();
        let mut uia: Vec<SuggestUia> = Vec::new();
        g.clip(area);
        for (i, item) in self.psug.items.iter().enumerate() {
            let r = Rect::new(area.x + 4.0, area.y + PAD_Y + row_h * i as f32, area.w - 8.0, row_h);
            let hit = ui.interact(pid(self.live, ID_SUGGEST).child("row").with(i as u64), r, Sense::click().cursor(Cursor::Hand));
            let selected = self.psug.sel == Some(i);
            if selected {
                g.round(r, t.radius_sm, t.selected);
            } else if hit.hovered {
                g.round(r, t.radius_sm, t.hover);
            }
            // the first recent folder starts its own section
            if item.kind == ItemKind::Recent && i > 0 && self.psug.items[i - 1].kind != ItemKind::Recent {
                g.fill(Rect::new(area.x + 8.0, r.y, area.w - 16.0, 1.0), t.border);
            }
            let (glyph, tint) = match item.kind {
                ItemKind::Folder => ("\u{E8B7}", t.folder),
                ItemKind::Drive => ("\u{EDA2}", t.text_dim),
                ItemKind::Recent => ("\u{E81C}", t.text_dim),
            };
            g.text(glyph, Rect::new(r.x + 6.0, r.y, 16.0, r.h), f(13.0).icon().c(), tint);
            let mut right = r.r() - 8.0;
            if !item.detail.is_empty() {
                let w = g.measure(&item.detail, t.font()).min(r.w * 0.5);
                right -= w;
                g.text(&item.detail, Rect::new(right, r.y, w, r.h), t.font(), t.text_dim);
                right -= 10.0;
            }
            g.text(&item.label, Rect::new(r.x + 28.0, r.y, (right - r.x - 28.0).max(0.0), r.h), t.font(), t.text);
            if hit.clicked == Some(Button::Left) {
                open = Some(item.nav.clone());
            }
            if a11y {
                uia.push(SuggestUia { label: item.label.clone(), path: item.nav.clone(), rect: r, selected });
            }
        }
        g.unclip();
        if a11y {
            uia_rows::suggestions(self.live, area, &uia);
        }
        if let Some(p) = open {
            self.path_text.set_text(&p);
            self.submit_path();
            ui.request_repaint();
        }
    }

    /// A screen reader invoked row `i`: opens that folder.
    pub(crate) fn suggest_invoke(&mut self, i: usize) {
        if let Some(p) = self.psug.items.get(i).map(|it| it.nav.clone()) {
            self.path_text.set_text(&p);
            self.submit_path();
        }
    }

    /// Remaining debounce as `(sequence, ms)` for the window's one-shot timer; `None`
    /// when nothing waits (no timer then).
    pub fn suggest_timer(&self, now: Instant) -> Option<(u64, u32)> {
        if self.editing { self.psug.next_timer(now) } else { None }
    }

    /// The debounce timer fired: lists the parent. True when a repaint is due.
    pub fn on_suggest_timer(&mut self) -> bool {
        self.psug.on_timer(Instant::now())
    }

    // ---- the path bar's menu and commands ----------------------------------------------

    /// Right click on the path bar: Edit address, Copy address, Copy address as text with
    /// quotes, Paste and go.
    pub(super) fn open_path_menu(&mut self, ui: &mut Ui, anchor: Pt) {
        let keys = |c: u32| self.keymap.label(c);
        let rows = vec![
            (CMD_FOCUS_PATH, "Edit address".to_string(), '\u{E70F}', keys(CMD_FOCUS_PATH)),
            (CMD_COPY_ADDRESS, "Copy address".to_string(), '\u{E8C8}', keys(CMD_COPY_ADDRESS)),
            (CMD_COPY_ADDRESS_QUOTED, "Copy address as text with quotes".to_string(), '\u{E8C8}', keys(CMD_COPY_ADDRESS_QUOTED)),
            (CMD_PASTE_AND_GO, "Paste and go".to_string(), '\u{E77F}', keys(CMD_PASTE_AND_GO)),
        ];
        let m = crate::nav_menu::NavMenu::actions(self.live, anchor, rows);
        self.ctx = None;
        ui.close_popup(crate::ctxui::ID_CTX.with(0));
        self.nav_menu = Some(Box::new(m));
        ui.request_repaint();
    }

    /// The B11-H commands. True when `cmd` was one of them.
    pub(super) fn run_suggest(&mut self, ui: &mut Ui, cmd: u32) -> bool {
        match cmd {
            CMD_OPEN_TERMINAL => {
                let dir = self.terminal_dir();
                launch_terminal(dir);
            }
            CMD_COPY_ADDRESS => {
                let p = self.tab().path.clone();
                ui.clipboard.set(&p);
                self.ops.message = Some("Address copied".into());
            }
            CMD_COPY_ADDRESS_QUOTED => {
                let p = format!("\"{}\"", self.tab().path);
                ui.clipboard.set(&p);
                self.ops.message = Some("Address copied with quotes".into());
            }
            CMD_PASTE_AND_GO => self.paste_and_go(ui),
            _ => return false,
        }
        ui.request_repaint();
        true
    }

    /// The folder a terminal opens in: the shown folder, or the folder an open archive sits in.
    fn terminal_dir(&self) -> String {
        let p = self.tab().path.clone();
        if crate::archive_ui::is_archive_path(&p)
            && let Some(host) = crate::archive_ui::host_folder_of(&p)
        {
            return host.to_string_lossy().into_owned();
        }
        p
    }

    /// The clipboard's text as an address: opens it, or says why it cannot be. A path that does
    /// not exist comes back through the same failure flow as a typed one (old folder, message).
    fn paste_and_go(&mut self, ui: &mut Ui) {
        let raw = ui.clipboard.get().unwrap_or_default();
        let typed = raw.lines().next().unwrap_or("").trim().trim_matches('"').trim().to_string();
        if typed.is_empty() {
            self.ops.message = Some("Nothing to paste: the clipboard has no text.".into());
            return;
        }
        match nav::normalize(&ps::expand_env(&typed, &ps::system_env)) {
            Some(p) => {
                let prev = self.tab().path.clone();
                let id = self.tab().id;
                self.go(&p);
                self.edit_nav = Some((id, prev, typed));
            }
            None => self.ops.message = Some(format!("Can't open \"{typed}\": not a full folder path.")),
        }
    }
}
