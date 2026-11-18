//! U4 glue between the view and the new parts: the key map, the command palette, the
//! Settings tab, and the settings that change how the view looks or lists.
//!
//! The palette and the Settings pages are built when they open and dropped when they close
//! (`View::palette`, `View::settings`), so a closed one costs nothing. A page edits
//! `View::prefs` (the config as the user has it) and says what it touched in a `C_*` bit mask;
//! `apply_prefs` makes each bit live and raises `config_dirty` so main writes the file.

use super::*;
use crate::keymap::{self, KeyMap};
use crate::palette::{self, Action, Mode, Palette};
use crate::settings::{C_APPEARANCE, C_HIDE, C_KEYS, C_MENU, C_UPDATES, C_ZEBRA, Cx, Page, Settings};
use std::time::{Duration, Instant};
use tsubame_ui::themes;

/// Commands that make sense with the Settings tab in front (the rest are about a folder).
pub(super) fn settings_tab_allows(cmd: u32) -> bool {
    matches!(
        cmd,
        CMD_NEW_TAB | CMD_CLOSE_TAB | CMD_NEXT_TAB | CMD_PREV_TAB | CMD_THEME | CMD_DENSITY | CMD_ZEBRA | CMD_SPLIT | CMD_SPLIT_ORIENTATION | CMD_SWITCH_PANE | CMD_TAB_TO_OTHER
    ) || (CMD_PALETTE..=CMD_RESET_KEYS).contains(&cmd)
}

impl View {
    // ---- theme and preferences -------------------------------------------------------

    /// The kit theme for the chosen colour theme, accent, density and font size.
    pub(super) fn themed(&self) -> Theme {
        let pal = themes::find(&self.theme_id).unwrap_or_else(|| themes::tsubame(self.dark));
        let mut base = Theme::from_palette(pal);
        if let Some(a) = self.prefs.appearance.accent_rgb() {
            base = base.with_accent(a);
        }
        Theme::with_density(&base, self.density).with_font_scale(self.prefs.appearance.font_scale)
    }

    /// The fields the shortcut commands changed (theme, density, zebra) go back into `prefs`.
    pub fn pull_appearance(&mut self) {
        let a = &mut self.prefs.appearance;
        a.theme = self.theme_id.clone();
        a.density = crate::session::config_density(self.density);
        a.zebra = self.zebra;
    }

    /// Startup: the loaded config becomes the view's settings. `apply_hide` / `theme_from_cfg`
    /// are false in a measurement run (it shows every file and follows the system theme).
    pub fn set_prefs(&mut self, cfg: &tsubame_config::Config, apply_hide: bool, theme_from_cfg: bool) {
        self.prefs = cfg.clone();
        if theme_from_cfg && let Some(p) = themes::find(&cfg.appearance.theme) {
            self.theme_id = p.id.to_string();
            self.dark = p.dark;
        }
        self.pull_appearance();
        let (km, _unknown) = KeyMap::from_overrides(&cfg.keys.overrides);
        self.keymap = km;
        keymap::set_current(&self.keymap);
        if apply_hide {
            self.apply_hide();
        }
    }

    fn apply_hide(&mut self) {
        let g = &self.prefs.general;
        let mask = crate::tab::hide_mask(g.show_hidden, g.show_system);
        crate::tab::HIDE.store(mask, std::sync::atomic::Ordering::Relaxed);
        for t in &mut self.tabs.items {
            t.set_hide(mask);
        }
        if let Some(o) = self.other.as_deref_mut() {
            for t in &mut o.tabs.items {
                t.set_hide(mask);
            }
        }
    }

    /// Makes what a page changed live. `bits` are the `C_*` flags.
    pub fn apply_prefs(&mut self, ui: &mut Ui, bits: u32) {
        if bits & (C_APPEARANCE | C_ZEBRA) != 0 {
            let a = &self.prefs.appearance;
            let pal = themes::find(&a.theme).unwrap_or_else(|| themes::tsubame(true));
            self.theme_id = pal.id.to_string();
            self.dark = pal.dark;
            self.density = crate::ui_density(a.density);
            self.zebra = a.zebra;
            self.apply_theme(ui);
        }
        if bits & C_HIDE != 0 {
            self.apply_hide();
        }
        if bits & C_KEYS != 0 {
            self.prefs.keys.overrides = self.keymap.to_overrides(&self.prefs.keys.overrides);
            keymap::set_current(&self.keymap);
            self.keymap.bind_all(ui);
        }
        if bits & C_UPDATES != 0 {
            crate::updates::set_auto_install(self.prefs.updates.auto_install);
        }
        if bits & C_MENU != 0 {
            crate::shellops::set_menu_entries(&self.prefs.context_menu.entries);
        }
        self.config_dirty = true;
        ui.request_repaint();
    }

    /// Startup: hands the saved context menu to the shell-menu code.
    pub fn publish_menu(&self) {
        crate::shellops::set_menu_entries(&self.prefs.context_menu.entries);
    }

    /// After "Reset all settings": every setting back to the default, live.
    pub fn reset_prefs(&mut self, ui: &mut Ui, cfg: &tsubame_config::Config) {
        self.prefs = cfg.clone();
        self.keymap = KeyMap::defaults();
        self.apply_prefs(ui, C_APPEARANCE | C_ZEBRA | C_HIDE | C_KEYS | C_UPDATES | C_MENU);
    }

    /// The folder new tabs and the palette start from (not the Settings tab's empty path).
    pub(super) fn folder_path(&self) -> String {
        let t = self.tab();
        if !t.settings {
            return t.path.clone();
        }
        for slot in 0..2 {
            if let Some(ts) = self.pane_tabs(slot)
                && let Some(t) = ts.items.iter().find(|t| !t.settings)
            {
                return t.path.clone();
            }
        }
        tsubame_win::places::home_folder()
    }

    // ---- commands ----------------------------------------------------------------------

    /// The U4 commands. True when `cmd` was one of them.
    pub(super) fn run_u4(&mut self, ui: &mut Ui, cmd: u32) -> bool {
        match cmd {
            CMD_PALETTE => self.toggle_palette(ui),
            CMD_SETTINGS => self.open_settings(None),
            CMD_SETTINGS_KEYS | CMD_SETTINGS_APPEARANCE | CMD_SETTINGS_MENU | CMD_SETTINGS_UPDATES | CMD_SETTINGS_DEFAULT_FM => self.open_settings(Page::from_command(cmd)),
            CMD_HIDDEN => {
                self.prefs.general.show_hidden = !self.prefs.general.show_hidden;
                self.apply_prefs(ui, C_HIDE);
            }
            CMD_SYSTEM => {
                self.prefs.general.show_system = !self.prefs.general.show_system;
                self.apply_prefs(ui, C_HIDE);
            }
            CMD_CHECK_UPDATES => {
                crate::updates::check_now();
                self.open_settings(Some(Page::Updates));
            }
            CMD_OPEN_CONFIG => self.open_paths.push(self.env.data_dir.display().to_string()),
            CMD_RESET_KEYS => {
                self.keymap.reset_all();
                self.apply_prefs(ui, C_KEYS);
            }
            _ => return false,
        }
        true
    }

    // ---- the Settings tab ----------------------------------------------------------------

    /// Shows the one Settings tab (made on first use, in the active pane) and, with `page`,
    /// opens that page.
    pub fn open_settings(&mut self, page: Option<Page>) {
        let mut found = None;
        for slot in 0..2 {
            if let Some(ts) = self.pane_tabs(slot)
                && let Some(i) = ts.items.iter().position(|t| t.settings)
            {
                found = Some((slot, i));
                break;
            }
        }
        match found {
            Some((slot, i)) => {
                self.set_active_pane(slot);
                self.switch_tab(i);
            }
            None => {
                let id = self.next_tab_id;
                self.next_tab_id += 1;
                let t = Tab::new_settings(id, self.density.row_h());
                self.tabs.add(t);
                self.switched();
            }
        }
        let st = self.settings.get_or_insert_with(|| Box::new(Settings::new(Page::General)));
        if let Some(p) = page {
            st.set_page(p);
        }
    }

    /// Draws the Settings pages in `area` and applies what they changed.
    pub(super) fn paint_settings(&mut self, ui: &mut Ui, g: &mut dyn Canvas, area: Rect) {
        if self.private_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            self.private_bytes = crate::bench::memory().0;
            self.private_at = Some(Instant::now());
        }
        let mut st = self.settings.take().unwrap_or_else(|| Box::new(Settings::new(Page::General)));
        let mut cx = Cx {
            prefs: &mut self.prefs,
            keymap: &mut self.keymap,
            env: &self.env,
            changed: 0,
            open: Vec::new(),
            reset_all: false,
            restart: false,
            rollback: false,
            message: None,
            private_bytes: self.private_bytes,
        };
        st.paint(ui, g, area, &mut cx);
        let Cx { changed, open, reset_all, restart, rollback, message, .. } = cx;
        self.settings = Some(st);
        if changed != 0 {
            self.apply_prefs(ui, changed);
        }
        self.open_paths.extend(open);
        if let Some(m) = message {
            self.ops.message = Some(m);
        }
        self.want_reset_all |= reset_all;
        self.want_restart |= restart;
        self.want_rollback |= rollback;
    }

    // ---- the command palette ----------------------------------------------------------------

    fn toggle_palette(&mut self, ui: &mut Ui) {
        if self.palette.take().is_some() {
            ui.capture_keys = false;
            return;
        }
        let data = self.palette_data();
        self.palette = Some(Box::new(Palette::open(Mode::Commands, data, Box::new(crate::commands::first_key_label))));
    }

    /// What the palette's modes list: tabs of both panes, recent folders, and the first rows
    /// of the folder that match the live filter.
    fn palette_data(&self) -> palette::Data {
        let mut tabs = Vec::new();
        for slot in 0..2 {
            if let Some(ts) = self.pane_tabs(slot) {
                for (i, t) in ts.items.iter().enumerate() {
                    tabs.push(palette::TabRow { pane: slot, index: i, title: t.title().to_string(), path: t.path.clone(), current: slot == self.active && i == ts.active });
                }
            }
        }
        let t = self.tab();
        let here = t.path.as_str();
        let recent_folders = self.side.recents.iter().filter(|p| p.as_str() != here).take(8).map(|p| (p.clone(), String::new())).collect();
        let mut matches = Vec::new();
        let (mut match_count, mut total) = (0, 0);
        if !t.settings {
            match_count = t.shown();
            total = t.store.len();
            for &id in t.store.view().iter().take(8) {
                let name = t.store.name(id).map(String::from_utf16_lossy).unwrap_or_default();
                let Some(rec) = t.store.record(id) else { continue };
                let is_dir = rec.flags & tsubame_core::FOLDER != 0;
                let kind = if is_dir {
                    "Folder".to_string()
                } else {
                    match name.rsplit_once('.') {
                        Some((_, e)) if !e.is_empty() && e.len() <= 5 => format!("{} file", e.to_ascii_uppercase()),
                        _ => "File".to_string(),
                    }
                };
                let mut size = String::new();
                if !is_dir {
                    data::fmt_size(rec.size, &mut size);
                }
                matches.push(palette::MatchRow { name, kind, size, is_dir });
            }
        }
        palette::Data { recents: self.recents.clone(), tabs, recent_folders, folder_name: crate::dnd::name_of(here).to_string(), matches, match_count, total }
    }

    /// Draws the palette on top of the frame and runs what it asked for.
    pub(super) fn paint_palette(&mut self, ui: &mut Ui, g: &mut dyn Canvas, size: (f32, f32)) {
        let Some(mut p) = self.palette.take() else { return };
        if matches!(p.mode, Mode::Filter | Mode::Tabs) {
            let d = self.palette_data();
            if d != p.data {
                p.set_data(d);
            }
        }
        let act = p.paint(ui, g, Rect::new(0.0, 0.0, size.0, size.1));
        if self.run_palette_action(ui, act) {
            self.palette = Some(p);
        } else {
            ui.capture_keys = false;
        }
    }

    /// True when the palette stays open.
    pub(super) fn run_palette_action(&mut self, ui: &mut Ui, act: Action) -> bool {
        match act {
            Action::None => true,
            Action::Close => false,
            Action::Run { id, keep } => {
                self.recents.retain(|x| *x != id);
                self.recents.insert(0, id);
                self.recents.truncate(8);
                self.run_command(ui, id);
                keep
            }
            Action::ShowShortcut(id) => {
                self.open_settings(Some(Page::Keys));
                if let (Some(st), Some(c)) = (self.settings.as_deref_mut(), crate::commands::by_id(id)) {
                    st.keys.focus_command(c.title);
                }
                false
            }
            Action::Go { path, new_tab } => {
                if new_tab {
                    self.open_in_new_tab(&path);
                } else {
                    self.go(&path);
                }
                false
            }
            Action::FilterLive(text) => {
                if !self.tab().settings {
                    self.filter.set_text(&text);
                    self.tab_mut().set_filter(&text);
                }
                ui.request_repaint();
                true
            }
            Action::FilterKeep { open } => {
                if let Some(name) = open {
                    let path = nav::join(&self.tab().path, &name);
                    let dir = self.tab().store.view().iter().find_map(|&id| {
                        let n = self.tab().store.name(id).map(String::from_utf16_lossy)?;
                        (n == name).then(|| self.tab().store.record(id).is_some_and(|r| r.flags & tsubame_core::FOLDER != 0))
                    });
                    match dir {
                        Some(true) => self.go(&path),
                        Some(false) => self.open_paths.push(path),
                        None => {}
                    }
                }
                false
            }
            Action::FilterCancel => {
                self.filter.set_text("");
                if !self.tab().settings {
                    self.tab_mut().set_filter("");
                }
                false
            }
            Action::SwitchTab { pane, index } => {
                self.set_active_pane(pane);
                self.switch_tab(index);
                false
            }
            Action::MoveTab { pane, index } => {
                self.set_active_pane(pane);
                self.tabs.activate(index);
                self.move_tab_to_other();
                true
            }
            Action::CloseTab { pane, index } => {
                self.set_active_pane(pane);
                self.close_tab(index);
                true
            }
        }
    }

    /// Open the palette for the leak gate and the tests: opens it, paints nothing.
    #[allow(dead_code)]
    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    #[allow(dead_code)]
    pub fn settings_open(&self) -> bool {
        self.settings.is_some()
    }
}
