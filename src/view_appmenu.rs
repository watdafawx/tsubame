//! The view side of the app menu (`crate::appmenu`): the gear button in the title bar, opening
//! the menu (button, `F10`, palette), `About` and `Quit`, and the tooltips of the toolbar buttons
//! (name + key hint). `docs/discoverability.md`.

use super::*;
use crate::appmenu::{AppMenu, Ticks};
use crate::nav_menu::NavMenu;
use tsubame_core::grouping::GroupBy;

/// Width of the gear button (DIPs); the layout switch is 3 x `LAYOUT_BTN_W`.
pub(super) const GEAR_W: f32 = 28.0;
const ID_GEAR: Id = Id::new("app-menu-btn");

/// The app menu's state on the view: where the gear is (the anchor of F10) and the tooltip the
/// toolbar buttons asked for this frame.
#[derive(Default)]
pub struct AppUi {
    pub(super) gear: Option<Rect>,
    tip: Option<(Rect, String, String)>,
}

impl View {
    /// The commands whose tick is on, for the menu.
    fn app_ticks(&self) -> Ticks {
        let mut on = Vec::new();
        on.push(match self.mode {
            ViewMode::Details => CMD_VIEW_DETAILS,
            ViewMode::GridSmall => CMD_VIEW_GRID_SMALL,
            ViewMode::GridLarge => CMD_VIEW_GRID_LARGE,
            ViewMode::Tree => CMD_VIEW_TREE,
            ViewMode::Miller => CMD_VIEW_MILLER,
        });
        let group = [CMD_GROUP_NONE, CMD_GROUP_NAME, CMD_GROUP_TYPE, CMD_GROUP_DATE, CMD_GROUP_SIZE];
        if let Some(i) = GroupBy::ALL.iter().position(|g| *g == self.tab().group.by) {
            on.push(group[i]);
        }
        let flags = [
            (self.prefs.general.show_hidden, CMD_HIDDEN),
            (self.prefs.general.show_system, CMD_SYSTEM),
            (self.prefs.view.show_extensions, CMD_VIEW_EXTENSIONS),
            (self.zebra, CMD_ZEBRA),
            (self.prefs.file_age.enabled, CMD_AGE_COLOURS),
            (self.fullscreen, CMD_FULLSCREEN),
            (self.dual(), CMD_SPLIT),
        ];
        on.extend(flags.iter().filter(|(v, _)| *v).map(|(_, c)| *c));
        Ticks(on)
    }

    /// The menu as it reads now.
    pub(crate) fn app_menu_model(&self) -> AppMenu {
        AppMenu::build(&self.app_ticks(), &|c| self.keymap.label(c), &|c| crate::commands::by_id(c).is_some())
    }

    /// Opens the app menu below the gear button (or the top right corner when it is not on screen).
    pub(crate) fn open_app_menu(&mut self, ui: &mut Ui) {
        let (w, _) = ui.size();
        let at = match self.appm.gear {
            Some(g) => Pt::new((g.r() - 280.0).max(2.0), g.b() + 2.0),
            None => Pt::new((w - 290.0).max(2.0), 34.0),
        };
        self.ctx = None;
        ui.close_popup(crate::ctxui::ID_CTX.with(0));
        self.nav_menu = Some(Box::new(NavMenu::app(self.live, at, self.app_menu_model())));
        ui.request_repaint();
    }

    pub(super) fn app_menu_is_open(&self) -> bool {
        self.nav_menu.as_ref().is_some_and(|m| m.open && m.is_app())
    }

    /// The gear button at `r` (title bar): opens or closes the menu. Own-drawn icon, flat.
    pub(super) fn paint_gear(&mut self, ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, r: Rect) {
        self.appm.gear = Some(r);
        let it = ui.interact(ID_GEAR, r, Sense::click());
        let open = self.app_menu_is_open();
        if open {
            g.round(r.pad(1.0), t.radius_sm, t.selected_inactive);
        } else if it.hovered {
            g.round(r.pad(1.0), t.radius_sm, if it.active { t.selected_inactive } else { t.hover });
        }
        g.text("\u{E713}", r, f(t.icon_toolbar).icon().c(), if open { t.accent } else { t.text_dim });
        if it.hovered && ui.active().is_none() && !open {
            let k = self.keymap.label(CMD_SETTINGS);
            let line = if k.is_empty() { String::new() } else { format!("Settings: {k}") };
            self.appm.tip = Some((r, format!("Menu and settings  {}", self.keymap.label(CMD_APP_MENU)).trim_end().to_string(), line));
        }
        if it.clicked == Some(Button::Left) {
            if open {
                self.nav_menu = None;
                ui.close_popup(crate::nav_menu::ID_NAV_MENU.with(0));
            } else {
                self.open_app_menu(ui);
            }
        }
    }

    /// Asks for a tooltip under `r` this frame: `cmd`'s name and key text.
    pub(super) fn tip_cmd(&mut self, ui: &Ui, id: Id, r: Rect, title: &str, cmd: u32) {
        if ui.hot() == Some(id) && ui.active().is_none() {
            self.appm.tip = Some((r, title.to_string(), self.keymap.label(cmd)));
        }
    }

    /// Draws the tooltip a button asked for (last in the frame, only while no popup is open).
    pub(super) fn paint_btn_tip(&mut self, ui: &Ui, g: &mut dyn Canvas, t: &Theme, size: (f32, f32)) {
        let Some((r, title, line)) = self.appm.tip.take() else { return };
        if ui.popups_open() > 0 {
            return;
        }
        let lines: Vec<&str> = if line.is_empty() { Vec::new() } else { vec![line.as_str()] };
        crate::overlay::tooltip(g, t, size, Pt { x: r.x, y: r.b() + 6.0 }, &title, &lines);
    }

    /// `app.menu`, `app.about`, `app.quit`. False for any other command.
    pub(super) fn run_appmenu(&mut self, ui: &mut Ui, cmd: u32) -> bool {
        match cmd {
            CMD_APP_MENU => {
                if self.app_menu_is_open() {
                    self.nav_menu = None;
                    ui.close_popup(crate::nav_menu::ID_NAV_MENU.with(0));
                } else {
                    self.open_app_menu(ui);
                }
            }
            CMD_ABOUT => {
                let commit = crate::buildinfo::short_commit().map(|c| format!(" ({c})")).unwrap_or_default();
                self.ops.message = Some(format!("Tsubame {}{} \u{b7} {}", self.env.version, commit, crate::buildinfo::build_channel()));
                self.open_settings(Some(crate::settings::Page::Updates));
            }
            CMD_QUIT => self.win_cmd = Some(WinCmd::Close),
            CMD_COLUMNS_MENU => {
                let r = self.list_r;
                self.open_col_menu(ui, Pt::new(r.x + 40.0, r.y));
            }
            _ => return false,
        }
        ui.request_repaint();
        true
    }
}
