//! Settings, drawn inside a tab (U4): a navigation list on the left, one page on the right.
//! Pages: General, Appearance, Context menu, Keys, Updates, Default file manager, Extensions, Advanced.
//!
//! The state here is built the first time the tab opens and dropped when it closes. Every
//! change is applied the moment it is made: a page edits `Cx::prefs` (the config) or
//! `Cx::keymap` and ORs a bit into `Cx::changed`; the view applies it live and the debounced
//! config writer saves it.

pub mod lay;

mod advanced;
mod age_card;
mod miller_card;
mod appearance;
mod defaultfm_page;
mod extensions_page;
mod general;
mod keys_page;
mod menu_ed;
mod nss_pick;
mod nss_sel;
pub mod tags_page;
mod updates_page;

use lay::Pg;
use std::path::PathBuf;
use tsubame_config::Config;
use tsubame_ui::{Canvas, Id, Rect, ScrollMetrics, Sense, TextInput, Ui, f, text_input, vscrollbar};

use crate::keymap::KeyMap;

pub const C_APPEARANCE: u32 = 1;
pub const C_ZEBRA: u32 = 2;
pub const C_HIDE: u32 = 4;
pub const C_KEYS: u32 = 8;
pub const C_UPDATES: u32 = 16;
pub const C_MENU: u32 = 32;
pub const C_OTHER: u32 = 64;
pub const C_PREVIEW: u32 = 128;

const ID_NAV: Id = Id::new("settings-nav");
const ID_FIND: Id = Id::new("settings-find");
const ID_SCROLL: Id = Id::new("settings-scroll");
const ID_BAR: Id = Id::new("settings-bar");
const ID_PAGE: Id = Id::new("settings-page");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    General,
    Appearance,
    ContextMenu,
    Keys,
    Updates,
    DefaultFm,
    Extensions,
    Advanced,
    /// P4.F6
    Tags,
}

impl Page {
    pub const ALL: [Page; 9] = [Page::General, Page::Appearance, Page::ContextMenu, Page::Keys, Page::Tags, Page::Updates, Page::DefaultFm, Page::Extensions, Page::Advanced];

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Appearance => "Appearance",
            Page::ContextMenu => "Context menu",
            Page::Keys => "Keys",
            Page::Updates => "Updates",
            Page::DefaultFm => "Default file manager",
            Page::Extensions => "Extensions",
            Page::Advanced => "Advanced",
            Page::Tags => "Tags",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Page::General => "\u{E713}",
            Page::Appearance => "\u{E790}",
            Page::ContextMenu => "\u{E8FD}",
            Page::Keys => "\u{E765}",
            Page::Updates => "\u{E896}",
            Page::DefaultFm => "\u{E8B7}",
            Page::Extensions => "\u{EA86}",
            Page::Advanced => "\u{E90F}",
            Page::Tags => "\u{E8EC}",
        }
    }

    /// Words the "Find a setting" box looks in (besides the title).
    fn keywords(self) -> &'static str {
        match self {
            Page::General => "start folder session restore hidden system files extensions type ahead jump filter delete recycle confirm preview quick look syntax hex",
            Page::Appearance => "theme dark light colour color accent density zebra font size rows",
            Page::ContextMenu => "right click menu custom command nilesoft import submenu",
            Page::Keys => "keyboard shortcuts keybindings hotkeys rebind chord",
            Page::Updates => "update version check download install rollback signature",
            Page::DefaultFm => "explorer default folder registry win+e shim",
            Page::Extensions => "extension plugin built-in switch on off batch rename preview archive search checksum folder sizes file age colours heat modified date tint palette thresholds pips",
            Page::Advanced => "config folder reset diagnostics log memory portable",
            Page::Tags => "tag tags colour color label mark shape export import",
        }
    }

    pub fn from_command(cmd: u32) -> Option<Page> {
        use crate::commands::*;
        Some(match cmd {
            CMD_SETTINGS_KEYS => Page::Keys,
            CMD_SETTINGS_APPEARANCE => Page::Appearance,
            CMD_SETTINGS_MENU => Page::ContextMenu,
            CMD_SETTINGS_UPDATES => Page::Updates,
            CMD_SETTINGS_DEFAULT_FM => Page::DefaultFm,
            _ => return None,
        })
    }
}

/// Where this copy lives (the view fills it in).
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub install_dir: PathBuf,
    pub exe: PathBuf,
    pub portable: bool,
    /// Windows is set to dark
    pub system_dark: bool,
    pub version: &'static str,
}

/// What a page sees and changes for one frame.
pub struct Cx<'a> {
    pub prefs: &'a mut Config,
    pub keymap: &'a mut KeyMap,
    pub env: &'a Env,
    /// `C_*` bits the pages OR in
    pub changed: u32,
    /// folders / URLs to open with the shell (the view hands them to its opener)
    pub open: Vec<String>,
    /// "Reset all settings" was confirmed
    pub reset_all: bool,
    /// "Restart now" on the Updates page was pressed
    pub restart: bool,
    /// "Roll back" on the Updates page was confirmed
    pub rollback: bool,
    /// "Reload packs" on the Advanced page was pressed
    pub reload_packs: bool,
    /// an extension switch on the Extensions page was flipped: (id, on)
    pub ext_toggle: Option<(String, bool)>,
    /// a message for the status bar
    pub message: Option<String>,
    /// private bytes of the process, for the Advanced page (the view reads it once a second)
    pub private_bytes: usize,
    /// the tag store as the Tags page sees it, and its requests
    pub tags: tags_page::TagsCx,
}

pub struct Settings {
    pub page: Page,
    find: TextInput,
    scroll: f64,
    content_h: f64,
    pub general: general::State,
    pub appear: appearance::State,
    pub keys: keys_page::State,
    pub menu: menu_ed::State,
    pub dfm: defaultfm_page::State,
    pub adv: advanced::State,
    pub ext: extensions_page::State,
    pub upd: updates_page::State,
    pub tags: tags_page::State,
    /// the page was just switched: scroll to the top, rebuild lazily
    fresh: bool,
}

pub fn lighten(c: u32, amt: f32) -> u32 {
    tsubame_ui::themes::mix(0xffffff, c, amt)
}

impl Settings {
    pub fn new(page: Page) -> Settings {
        Settings { page, find: TextInput::default(), scroll: 0.0, content_h: 0.0, general: Default::default(), appear: Default::default(), keys: Default::default(), menu: Default::default(), dfm: Default::default(), adv: Default::default(), ext: Default::default(), upd: Default::default(), tags: Default::default(), fresh: true }
    }

    pub fn set_page(&mut self, page: Page) {
        if page != self.page {
            self.page = page;
            self.scroll = 0.0;
            self.keys.reset_transient();
            self.menu.reset_transient();
        }
    }

    pub fn paint(&mut self, ui: &mut Ui, g: &mut dyn Canvas, area: Rect, cx: &mut Cx) {
        let t = ui.theme;
        g.fill(area, t.bg);
        let mut r = area;
        let nav = r.cut_left(t.layout_settings_nav_width);
        let content = r;
        self.paint_nav(ui, g, nav, cx);
        // a page switched by the nav: no keys are captured by a page that is gone
        let bar_w = t.layout_scrollbar_width;
        let pad = 32.0;
        let inner_w = (content.w - 2.0 * pad - bar_w).clamp(300.0, t.layout_settings_content_width);
        let x = content.x + pad.min((content.w - inner_w - bar_w) / 2.0).max(16.0);
        let it = ui.interact(ID_SCROLL, content, Sense::hover().wheel());
        let max = (self.content_h - content.h as f64).max(0.0);
        if it.wheel.1 != 0.0 {
            self.scroll = (self.scroll - it.wheel.1 as f64 * 3.0 * t.height_list_row as f64 * 1.5).clamp(0.0, max);
            ui.request_repaint();
        }
        if self.fresh {
            self.fresh = false;
        }
        self.scroll = self.scroll.clamp(0.0, max);
        g.clip(content);
        let y0 = content.y + 22.0 - self.scroll as f32;
        let end;
        {
            let mut pg = Pg::new(ui, g, x, y0, inner_w, content, ID_PAGE.with(self.page as u64));
            match self.page {
                Page::General => general::paint(&mut pg, &mut self.general, cx),
                Page::Appearance => appearance::paint(&mut pg, &mut self.appear, cx),
                Page::ContextMenu => menu_ed::paint(&mut pg, &mut self.menu, cx),
                Page::Keys => keys_page::paint(&mut pg, &mut self.keys, cx),
                Page::Updates => updates_page::paint(&mut pg, &mut self.upd, cx),
                Page::DefaultFm => defaultfm_page::paint(&mut pg, &mut self.dfm, cx),
                Page::Extensions => extensions_page::paint(&mut pg, &mut self.ext, cx),
                Page::Advanced => advanced::paint(&mut pg, &mut self.adv, cx),
                Page::Tags => tags_page::paint(&mut pg, &mut self.tags, cx),
            }
            end = pg.y;
        }
        g.unclip();
        self.content_h = (end - y0) as f64 + 44.0;
        let track = Rect::new(content.r() - bar_w, content.y, bar_w, content.h);
        if self.content_h > content.h as f64 {
            let m = ScrollMetrics { content: self.content_h, view: content.h as f64, page: content.h as f64 * 0.9 };
            if vscrollbar(ui, g, ID_BAR, track, m, &mut self.scroll) {
                ui.request_repaint();
            }
        }
    }

    fn matching(&self) -> Vec<Page> {
        let q = self.find.text.trim().to_lowercase();
        Page::ALL.into_iter().filter(|p| q.is_empty() || p.title().to_lowercase().contains(&q) || p.keywords().contains(&q)).collect()
    }

    fn paint_nav(&mut self, ui: &mut Ui, g: &mut dyn Canvas, nav: Rect, cx: &Cx) {
        let t = ui.theme;
        g.fill(nav, t.panel);
        g.fill(Rect::new(nav.r() - 1.0, nav.y, 1.0, nav.h), t.border);
        // find box
        let fr = Rect::new(nav.x + 8.0, nav.y + 8.0, nav.w - 17.0, t.height_field);
        self.find.pad_left = 22.0;
        let resp = text_input(ui, g, ID_FIND, fr, &mut self.find, "Find a setting");
        g.text("\u{E721}", Rect::new(fr.x + 6.0, fr.y, 16.0, fr.h), f(12.0).icon().c(), t.text_dim);
        let list = self.matching();
        if resp.submitted
            && let Some(p) = list.first()
        {
            self.set_page(*p);
        }
        if ui.has_focus(ID_FIND) && ui.window_focused() {
            ui.mark_text_focus();
        }
        let mut y = fr.b() + 10.0;
        for p in list {
            let r = Rect::new(nav.x + 8.0, y, nav.w - 17.0, t.height_settings_nav_item);
            let it = ui.interact(ID_NAV.with(p as u64), r, Sense::click().focusable());
            let on = p == self.page;
            if on {
                g.round(r, t.radius_md, t.selected);
                g.round(Rect::new(r.x, r.y + 5.0, 3.0, r.h - 10.0), 1.5, t.accent);
            } else if it.hovered {
                g.round(r, t.radius_md, t.hover);
            }
            if ui.has_focus(ID_NAV.with(p as u64)) && ui.window_focused() {
                g.round_stroke(r.pad(1.0), t.radius_md, t.focus, 1.0);
            }
            g.text(p.glyph(), Rect::new(r.x + 10.0, r.y, 18.0, r.h), f(14.0).icon().c(), if on { t.text } else { t.text_dim });
            g.text(p.title(), Rect::new(r.x + 36.0, r.y, r.w - 40.0, r.h), t.font_md(), t.text);
            let mut go = it.clicked.is_some();
            for (qi, ev) in ui.key_input(ID_NAV.with(p as u64)) {
                if let tsubame_ui::Event::KeyDown { key: tsubame_ui::Key::Enter | tsubame_ui::Key::Space, mods, .. } = ev
                    && mods.none()
                {
                    ui.consume(qi);
                    go = true;
                }
            }
            if go {
                self.set_page(p);
                ui.request_repaint();
            }
            y += t.height_settings_nav_item;
        }
        let foot = format!("Tsubame {} \u{b7} config.toml", cx.env.version);
        g.text(&foot, Rect::new(nav.x + 16.0, nav.b() - 28.0, nav.w - 24.0, 20.0), t.font_small(), t.text_dim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_matches_titles_and_keywords() {
        let mut s = Settings::new(Page::General);
        s.find.set_text("shortcuts");
        assert_eq!(s.matching(), vec![Page::Keys]);
        s.find.set_text("update");
        assert_eq!(s.matching(), vec![Page::Updates]);
        s.find.set_text("");
        assert_eq!(s.matching().len(), 9);
        s.find.set_text("zzz");
        assert!(s.matching().is_empty());
    }

    #[test]
    fn commands_map_to_pages() {
        use crate::commands::*;
        assert_eq!(Page::from_command(CMD_SETTINGS_KEYS), Some(Page::Keys));
        assert_eq!(Page::from_command(CMD_SETTINGS_DEFAULT_FM), Some(Page::DefaultFm));
        assert_eq!(Page::from_command(CMD_NEW_TAB), None);
    }
}
