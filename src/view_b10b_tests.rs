//! Whole-frame unit tests for CARD B10-B (Explorer parity: reopen closed tab,
//! new window, F11 fullscreen, Back/Forward history dropdown, breadcrumb
//! sibling chevrons + UIA, Ctrl+Shift+E reveal in sidebar, F1 keyboard help).

use super::*;
use crate::uia_rows::{self, PART_CRUMBS};
use tsubame_win::uia::{Expand as UExpand, VAct, VReq};

#[test]
fn reopen_closed_tab_restores_path_mode_sort_scroll_and_keeps_last_tab_close() {
    let mut r = Rig::new("b10b-reopen");
    let sub = r.dir.join("Docs");
    let sub_s = sub.to_string_lossy().into_owned();
    let root_s = r.dir.to_string_lossy().into_owned();

    r.v.open_in_new_tab(&sub_s);
    r.settle();
    r.v.set_mode(ViewMode::GridSmall);
    r.v.tab_mut().sort_by(tsubame_core::Column::Size);
    r.v.tab_mut().list.scroll = 44.0;
    let saved_sort = r.v.tab().sort;
    assert_eq!(r.v.tabs.len(), 2);

    // Close the second tab (Ctrl+W)
    r.key(Key::Char('W'), Mods::CTRL);
    assert_eq!(r.v.tabs.len(), 1);
    assert_eq!(r.v.tab().path, root_s);
    assert_eq!(r.v.closed_tabs_len(), 1);

    // Switch mode away to prove reopen restores the closed tab's view mode
    r.v.set_mode(ViewMode::Details);
    assert_eq!(r.v.mode, ViewMode::Details);

    // Ctrl+Shift+T reopens it with path, view mode, sort and scroll restore
    r.key(Key::Char('T'), Mods::CTRL_SHIFT);
    assert_eq!(r.v.tabs.len(), 2);
    assert_eq!(r.v.tab().path, sub_s);
    assert_eq!(r.v.mode, ViewMode::GridSmall);
    assert_eq!(r.v.tab().sort, saved_sort);
    assert_eq!(r.v.tab().restore.as_ref().map(|x| x.scroll_first_row), Some(2));
    assert_eq!(r.v.closed_tabs_len(), 0);

    // Closing the last remaining tab still sets quit without emptying tabs (B9-LAG)
    r.v.close_tab(1);
    assert_eq!(r.v.tabs.len(), 1);
    assert!(!r.v.quit);
    r.v.close_tab(0);
    assert!(r.v.quit);
    assert_eq!(r.v.tabs.len(), 1, "last tab stays intact so paint/session capture never panic");
}

#[test]
fn ctrl_n_queues_new_window_and_args_include_new_window_flag() {
    let mut r = Rig::new("b10b-newwin");
    let root_s = r.dir.to_string_lossy().into_owned();
    r.key(Key::Char('N'), Mods::CTRL);
    assert_eq!(r.v.open_windows, vec![root_s.clone()]);

    let args = navv::build_new_window_args(&root_s, false);
    assert_eq!(args, vec!["--new-window", &root_s]);
    let portable = navv::build_new_window_args(&root_s, true);
    assert_eq!(portable, vec!["--new-window", "--portable", &root_s]);
    let from_archive = navv::build_new_window_args(r"C:\work\pack.zip::sub", false);
    assert_eq!(from_archive, vec!["--new-window", r"C:\work"]);
}

#[test]
fn f11_requests_fullscreen_toggle_and_esc_does_not_exit() {
    let mut r = Rig::new("b10b-fullscreen");
    r.key(Key::F(11), Mods::NONE);
    assert_eq!(r.v.win_cmd, Some(WinCmd::ToggleFullscreen));
    r.v.win_cmd = None;
    r.v.fullscreen = true;

    // Esc must NOT exit fullscreen
    r.key(Key::Escape, Mods::NONE);
    assert_eq!(r.v.win_cmd, None);
    assert!(r.v.fullscreen);

    // F11 again toggles back
    r.key(Key::F(11), Mods::NONE);
    assert_eq!(r.v.win_cmd, Some(WinCmd::ToggleFullscreen));
}

#[test]
fn history_dropdown_opens_and_navigates_directly() {
    let mut r = Rig::new("b10b-hist");
    let d1 = r.dir.join("Docs");
    let d2 = d1.join("Nested");
    std::fs::create_dir_all(&d2).unwrap();
    let p0 = r.dir.to_string_lossy().into_owned();
    let p1 = d1.to_string_lossy().into_owned();
    let p2 = d2.to_string_lossy().into_owned();

    r.v.go(&p1);
    r.v.go(&p2);
    r.settle();
    assert_eq!(r.v.tab().path, p2);

    // Open Back history menu
    r.v.open_history_menu(&mut r.ui, false, Pt::new(10.0, 68.0));
    r.frame();
    assert!(r.v.nav_menu_open());

    // Jump directly back to p0 (index 0), skipping p1
    r.v.go_history(0);
    r.settle();
    assert_eq!(r.v.tab().path, p0);
    assert!(r.v.tab().history.can_forward());
    assert_eq!(r.v.tab().history.forward_entries(15).len(), 2);
}

#[test]
fn breadcrumb_chevrons_have_uia_names_and_open_subfolder_menu() {
    uia_rows::force(true);
    let mut r = Rig::new("b10b-crumbs");
    let a = r.dir.join("Alpha");
    let b = r.dir.join("Beta");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    r.v.go(&a.to_string_lossy());
    r.settle();
    r.frame();

    let surf = uia_rows::last().surfaces.into_iter().find(|s| s.key == uia_rows::skey(0, PART_CRUMBS)).expect("Path bar UIA surface");
    assert_eq!(surf.name, "Path bar");
    assert!(!surf.items.is_empty());
    // Last crumb is "Alpha", second-to-last is the temp root dir
    let last_item = surf.items.last().unwrap();
    assert_eq!(last_item.name, "Subfolders of Alpha");
    assert_eq!(last_item.expand, UExpand::Collapsed);
    assert!(last_item.invokable);

    // Invoke the parent crumb's chevron via UIA -> opens menu listing Alpha (bold), Beta, Docs
    let parent_idx = surf.items[surf.items.len() - 2].key;
    r.v.run_view_request(VReq { surf: surf.key, key: parent_idx, act: VAct::Invoke });
    assert!(r.v.nav_menu_open());
    tree_wait(&mut r, "crumb subfolders enumerate", |r| {
        r.v.nav_menu.as_ref().is_some_and(|m| !m.is_loading())
    });
    let rows = r.v.nav_menu.as_ref().unwrap().test_rows();
    let names: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(names, vec!["Alpha", "Beta", "Docs"]);
    assert!(rows[0].default && !rows[1].default, "current subfolder is bold");

    // Expanded state reflected in UIA
    r.frame();
    let surf2 = uia_rows::last().surfaces.into_iter().find(|s| s.key == uia_rows::skey(0, PART_CRUMBS)).unwrap();
    let parent_item = surf2.items.iter().find(|i| i.key == parent_idx).unwrap();
    assert_eq!(parent_item.expand, UExpand::Expanded);

    // Collapse via UIA closes the menu
    r.v.run_view_request(VReq { surf: surf2.key, key: parent_idx, act: VAct::Collapse });
    assert!(!r.v.nav_menu_open());
    uia_rows::force(false);
}

#[test]
fn ctrl_shift_e_reveals_in_sidebar_and_f1_opens_keys_settings() {
    let mut r = Rig::new("b10b-side-f1");
    let root_s = r.dir.to_string_lossy().into_owned();
    r.v.side.pin_at(&root_s, 0);
    r.v.side.closed[crate::sidebar::PINNED] = true;
    r.v.side.set_rail(true);
    r.frame();
    assert!(r.v.side.rail);

    // Ctrl+Shift+E expands collapsed sidebar and unfolds Pinned section
    r.key(Key::Char('E'), Mods::CTRL_SHIFT);
    assert!(!r.v.side.rail);
    assert!(!r.v.side.closed[crate::sidebar::PINNED]);
    assert_eq!(r.v.side.kb, Some(0));

    // F1 opens Settings > Keys page
    r.key(Key::F(1), Mods::NONE);
    assert!(r.v.tab().settings);
    assert_eq!(r.v.settings.as_ref().map(|s| s.page), Some(crate::settings::Page::Keys));
}
