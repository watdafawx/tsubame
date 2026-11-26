//! The View's side of the Operations panel and the conflict dialog (B11-E2):
//! where they paint and what their answers do. The models live in
//! `ops_panel` / `conflict_dialog`, the jobs in `ops`.

use super::View;
use crate::ops_panel::{self, Act};
use tsubame_ui::{Canvas, Rect, Theme, Ui};

impl View {
    /// The panel, docked bottom-right over the window body (above the status bar).
    pub(super) fn paint_ops_panel(&mut self, ui: &mut Ui, g: &mut dyn Canvas, (w, h): (f32, f32)) {
        if !self.ops.panel.open {
            return;
        }
        let t = ui.theme;
        let top = t.height_titlebar + if self.tab().settings { 0.0 } else { t.height_toolbar };
        let area = Rect::new(0.0, top, w, (h - top - t.height_status_bar).max(0.0));
        let now = std::time::Instant::now();
        if let Some(a) = ops_panel::paint(&mut self.ops.panel, ui, g, area, now) {
            self.ops_act(ui, a);
        }
    }

    /// The conflict dialog, on top of everything (modal, no scrim).
    pub(super) fn paint_conflict(&mut self, ui: &mut Ui, g: &mut dyn Canvas, (w, h): (f32, f32)) {
        let Some(d) = self.ops.conflict.as_mut() else { return };
        let t = ui.theme;
        let window = Rect::new(0.0, 0.0, w, h);
        let area = Rect::new(0.0, t.height_titlebar, w, (h - t.height_titlebar - t.height_status_bar).max(0.0));
        if let Some(a) = crate::conflict_dialog::paint(d, ui, g, area, window) {
            self.ops.answer(a);
            ui.capture_keys = false;
            ui.set_focus(Some(super::pid(self.live, super::ID_LIST)));
            ui.request_repaint();
        }
    }

    /// Width the status bar keeps free for the pill (0: no pill).
    pub(super) fn ops_pill_w(&self, g: &mut dyn Canvas, t: &Theme) -> f32 {
        ops_panel::pill_width(&self.ops.panel, g, t)
    }

    /// The pill at `right` in the status bar `r`; a click unfolds the panel.
    pub(super) fn paint_ops_pill(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, r: Rect, right: f32) {
        if ops_panel::paint_pill(&self.ops.panel, ui, g, t, r, right) {
            self.ops_act(ui, Act::Open);
        }
    }

    pub(super) fn ops_act(&mut self, ui: &mut Ui, a: Act) {
        self.ops.act(a);
        self.after_ops();
        ui.request_repaint();
    }

    /// A screen reader's Invoke on the panel (`PART_OPS`) or the dialog (`PART_CONFLICT`).
    pub(super) fn a11y_ops(&mut self, conflict: bool, key: u64) {
        if conflict {
            if let Some(d) = self.ops.conflict.as_mut()
                && let Some(a) = crate::conflict_dialog::a11y_answer(d, key)
            {
                self.ops.answer(a);
            }
        } else if let Some(a) = ops_panel::a11y_act(&self.ops.panel, key) {
            self.ops.act(a);
            self.after_ops();
        }
    }
}
