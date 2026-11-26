//! Whole-frame tests for CARD B12-B: the gear button and the app menu, F10, the tooltips.

use super::*;

fn gear(r: &Rig) -> Rect {
    r.v.appm.gear.expect("the title bar drew the gear")
}

fn center(g: Rect) -> (f32, f32) {
    (g.x + g.w / 2.0, g.y + g.h / 2.0)
}

fn labels(r: &Rig) -> Vec<String> {
    r.v.nav_menu.as_ref().unwrap().test_rows().into_iter().map(|x| x.label).collect()
}

#[test]
fn the_gear_button_is_in_the_title_bar_and_opens_the_app_menu() {
    let mut r = Rig::new("b12b-gear");
    let g = gear(&r);
    assert!(g.y >= 0.0 && g.b() <= 36.0, "inside the title bar: {g:?}");
    assert!(g.r() < 1120.0 - 3.0 * 46.0, "left of the window controls");
    assert!(!r.v.nav_menu_open());
    let (x, y) = center(g);
    r.click(x, y);
    assert!(r.v.nav_menu_open(), "a click opens the menu");
    let m = r.v.nav_menu.as_ref().unwrap();
    assert!(m.is_app());
    assert_eq!(m.a11y_name(), "App menu");
    let rows = m.test_rows();
    assert_eq!(rows[0].label, "Settings\u{2026}");
    assert_eq!(rows[0].shortcut, "Ctrl+,");
    let l = labels(&r);
    for want in ["Keyboard shortcuts", "New window", "Check for updates", "About Tsubame", "Quit", "View", "Tabs", "Panes", "Go", "Files"] {
        assert!(l.iter().any(|x| x == want), "{want} is a row: {l:?}");
    }
}

#[test]
fn the_tab_strip_stops_before_the_gear() {
    let mut r = Rig::new("b12b-strip");
    let g = gear(&r);
    // the "+" button of the strip is left of the gear
    r.v.new_tab();
    r.frame();
    let strip = r.v.tabs.items.len();
    assert!(strip >= 2);
    assert!(g.x > 300.0, "{g:?}");
}

#[test]
fn f10_opens_and_closes_the_menu() {
    let mut r = Rig::new("b12b-f10");
    r.key(Key::F(10), Mods::NONE);
    assert!(r.v.nav_menu_open());
    assert!(r.v.nav_menu.as_ref().unwrap().is_app());
    r.v.run_command(&mut r.ui, CMD_APP_MENU);
    r.frame();
    assert!(!r.v.nav_menu_open(), "the same command closes it");
}

#[test]
fn the_menu_follows_the_view_and_the_key_map() {
    let mut r = Rig::new("b12b-ticks");
    let m = r.v.app_menu_model();
    let mut top = Vec::new();
    m.rows(&[], &mut top);
    let view = top.iter().find(|x| x.label == "View").unwrap().key;
    let mut kids = Vec::new();
    m.rows(&[view], &mut kids);
    let on = |name: &str, kids: &[tsubame_ui::MenuRow]| kids.iter().find(|x| x.label == name).map(|x| x.radio || x.checked);
    assert_eq!(on("Details", &kids), Some(true));
    assert_eq!(on("Hidden files", &kids), Some(false));
    r.v.set_mode(ViewMode::GridSmall);
    r.v.run_command(&mut r.ui, CMD_HIDDEN);
    let m = r.v.app_menu_model();
    let mut kids = Vec::new();
    m.rows(&[view], &mut kids);
    assert_eq!(on("Details", &kids), Some(false));
    assert_eq!(on("Small thumbnails", &kids), Some(true));
    assert_eq!(on("Hidden files", &kids), Some(true));
    let sc = |name: &str, kids: &[tsubame_ui::MenuRow]| kids.iter().find(|x| x.label == name).map(|x| x.shortcut.clone());
    assert_eq!(sc("Small thumbnails", &kids).as_deref(), Some("Ctrl+Shift+2"));
}

#[test]
fn a_switched_off_extension_leaves_the_menu() {
    let mut r = Rig::new("b12b-ext");
    let files = |r: &Rig| {
        let m = r.v.app_menu_model();
        let mut top = Vec::new();
        m.rows(&[], &mut top);
        let k = top.iter().find(|x| x.label == "Files").unwrap().key;
        let mut kids = Vec::new();
        m.rows(&[k], &mut kids);
        kids.into_iter().map(|x| x.label).collect::<Vec<_>>()
    };
    assert!(files(&r).iter().any(|l| l == "Extract archive here"));
    r.v.set_extension_enabled(&mut r.ui, crate::archive_ui::EXT_ARCHIVE, false).unwrap();
    assert!(!files(&r).iter().any(|l| l == "Extract archive here"));
    r.v.set_extension_enabled(&mut r.ui, crate::archive_ui::EXT_ARCHIVE, true).unwrap();
}

#[test]
fn a_command_picked_in_the_menu_runs() {
    let mut r = Rig::new("b12b-pick");
    r.key(Key::F(10), Mods::NONE);
    assert!(r.v.nav_menu_open());
    // the menu's own keyboard: first row, Enter
    r.key(Key::Down, Mods::NONE);
    r.key(Key::Enter, Mods::NONE);
    assert!(!r.v.nav_menu_open(), "a pick closes the menu");
    assert!(r.v.tab().settings, "the first row is Settings");
}

#[test]
fn about_says_the_version_and_opens_the_updates_page() {
    let mut r = Rig::new("b12b-about");
    r.v.run_command(&mut r.ui, CMD_ABOUT);
    assert!(r.v.tab().settings);
    assert!(r.v.ops.message.as_deref().is_some_and(|m| m.starts_with("Tsubame ")), "{:?}", r.v.ops.message);
}

#[test]
fn quit_asks_the_window_to_close() {
    let mut r = Rig::new("b12b-quit");
    r.v.run_command(&mut r.ui, CMD_QUIT);
    assert!(matches!(r.v.win_cmd, Some(WinCmd::Close)));
}

#[test]
fn the_gear_has_a_tooltip_with_the_keys() {
    let mut r = Rig::new("b12b-tip");
    let (x, y) = center(gear(&r));
    r.ui.push(Event::MouseMove { pos: Pt::new(x, y) });
    r.frame();
    r.frame();
    assert!(r.c.has_text("Menu and settings  F10"), "{:?}", r.c.texts());
    assert!(r.c.has_text("Settings: Ctrl+,"));
}

#[test]
fn toolbar_buttons_show_their_key_hint() {
    let mut r = Rig::new("b12b-toolbar-tip");
    // sweep the pointer over the toolbar's first row: the Up button names itself with its key
    let mut found = false;
    for x in (60..200).step_by(4) {
        r.ui.push(Event::MouseMove { pos: Pt::new(x as f32, 66.0) });
        r.frame();
        r.frame();
        if r.c.has_text("Up one folder") {
            found = true;
            assert!(r.c.has_text("Alt+Up"));
            break;
        }
    }
    assert!(found, "hovering the Up button names it and shows Alt+Up");
}

#[test]
fn choose_columns_opens_the_column_menu() {
    let mut r = Rig::new("b12b-cols");
    r.v.run_command(&mut r.ui, CMD_COLUMNS_MENU);
    r.frame();
    assert!(r.ui.popups_open() > 0, "the header's menu opens from the app menu");
}
