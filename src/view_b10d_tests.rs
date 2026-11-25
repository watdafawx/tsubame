//! Whole-frame tests for CARD B10-D: sidebar entries (Recycle Bin, OneDrive,
//! WSL Linux section + hide toggles), removable drive eject menu entry,
//! delayed row hover tooltips (Details and Grid), and icon overlays (.lnk,
//! symlinks/junctions, OneDrive placeholders).

use super::*;
use tsubame_core::{CLOUD_ONLINE, CLOUD_PINNED, CLOUD_UNPINNED, EntryInput, EntryStore, FOLDER, SYMLINK};
use tsubame_win::places::{Drive, DriveKind, Place, RECYCLE_BIN_SHELL, WslDistro};

#[test]
fn sidebar_shows_recycle_onedrive_wsl_and_honours_hide_toggles() {
    let mut r = Rig::new("b10d-side");
    r.v.side.places = vec![
        Place { name: "Home", path: r.dir.to_str().unwrap().into() },
        Place { name: "OneDrive", path: r.dir.join("Docs").to_str().unwrap().into() },
    ];
    r.v.side.wsl = vec![WslDistro { name: "Ubuntu".into(), path: "\\\\wsl.localhost\\Ubuntu".into() }];
    r.v.side.drives = vec![
        Drive { letter: 'C', root: "C:\\".into(), label: "System".into(), kind: DriveKind::Fixed, total: 100, free: 50 },
        Drive { letter: 'E', root: "E:\\".into(), label: "USB".into(), kind: DriveKind::Removable, total: 100, free: 80 },
    ];
    r.frame();
    assert!(r.c.has_text("OneDrive"), "{:?}", r.c.texts());
    assert!(r.c.has_text("Recycle Bin"), "{:?}", r.c.texts());
    assert!(r.c.has_text("Linux"), "{:?}", r.c.texts());
    assert!(r.c.has_text("Ubuntu"), "{:?}", r.c.texts());

    // Clicking Recycle Bin queues shell:RecycleBinFolder in open_paths (non-blocking shell open)
    r.v.side_act(&mut r.ui, Some(Act::OpenShell(RECYCLE_BIN_SHELL.into())));
    assert_eq!(r.v.open_paths, vec![RECYCLE_BIN_SHELL]);

    // Hide-per-user toggles take effect live via apply_prefs
    r.v.prefs.general.show_sidebar_onedrive = false;
    r.v.prefs.general.show_sidebar_wsl = false;
    r.v.prefs.general.show_sidebar_recycle = false;
    r.v.apply_prefs(&mut r.ui, crate::settings::C_OTHER);
    r.frame();
    assert!(!r.c.has_text("OneDrive"));
    assert!(!r.c.has_text("Recycle Bin"));
    assert!(!r.c.has_text("Linux"));
    assert!(!r.c.has_text("Ubuntu"));
}

#[test]
fn removable_drive_context_menu_includes_eject_and_fixed_drive_does_not() {
    let entries = tsubame_config::menu::default_entries();
    let rem = crate::ctxui::Target {
        paths: vec!["E:\\".into()],
        folders: vec!["E:\\".into()],
        folder: "E:\\".into(),
        removable_drive: true,
        ..Default::default()
    };
    let fixed = crate::ctxui::Target {
        paths: vec!["C:\\".into()],
        folders: vec!["C:\\".into()],
        folder: "C:\\".into(),
        removable_drive: false,
        ..Default::default()
    };
    let mut pool = tsubame_win::shell::Pool::with_lanes(1, 1, 0, 0x8001);
    let m_rem = crate::ctxui::CtxMenu::open(rem, tsubame_config::ContextMenu { entries: entries.clone(), ..Default::default() }, false, false, 16, &mut pool);
    let m_fix = crate::ctxui::CtxMenu::open(fixed, tsubame_config::ContextMenu { entries, ..Default::default() }, false, false, 16, &mut pool);
    assert!(m_rem.model.tsubame.iter().any(|i| i.label == "Eject" && i.action == "drive.eject"), "removable drive has Eject");
    assert!(!m_fix.model.tsubame.iter().any(|i| i.action == "drive.eject"), "fixed drive has no Eject");
}

#[test]
fn row_tooltip_shows_in_details_and_grid_after_dwell_and_respects_toggle() {
    let mut r = Rig::new("b10d-tip");
    let t = r.ui.theme;
    let list_x = t.layout_sidebar_width + 60.0;
    let row0_y = t.height_titlebar + t.height_toolbar + t.height_column_header + 10.0;
    let row1_y = row0_y + t.height_list_row;

    // Hover folder row 0 ("Docs"): enters Waiting, no tooltip drawn yet
    r.ui.push(Event::MouseMove { pos: Pt::new(list_x, row0_y) });
    r.frame();
    assert!(r.v.row_tip.is_waiting());
    assert!(!r.v.row_tip.is_showing());
    assert!(!r.c.texts().iter().any(|s| s.starts_with("Type: ")));

    // One-shot timer fires -> next paint draws folder tooltip (Type + Date modified, NO Size)
    assert!(r.v.row_tip.on_timer());
    r.frame();
    assert!(r.c.has_text("Type: File folder"), "{:?}", r.c.texts());
    assert!(r.c.texts().iter().any(|s| s.starts_with("Date modified: ")));
    assert!(!r.c.texts().iter().any(|s| s.starts_with("Size: ")), "folders must not show Size");

    // Hover file row 1 ("alpha.txt") -> tooltip shows Type, Size and Date modified
    r.ui.push(Event::MouseMove { pos: Pt::new(list_x, row1_y) });
    r.frame();
    assert!(r.v.row_tip.is_waiting());
    assert!(r.v.row_tip.on_timer());
    r.frame();
    assert!(r.c.has_text("Type: Text document"), "{:?}", r.c.texts());
    assert!(r.c.has_text("Size: 1 B"), "{:?}", r.c.texts());
    assert!(r.c.texts().iter().any(|s| s.starts_with("Date modified: ")));

    // Switch to Grid view and verify tooltip works there too
    r.v.set_mode(ViewMode::GridSmall);
    r.frame();
    let cell1_x = t.layout_sidebar_width + 96.0 + 40.0;
    let cell_y = t.height_titlebar + t.height_toolbar + t.height_column_header + 40.0;
    r.ui.push(Event::MouseMove { pos: Pt::new(cell1_x, cell_y) });
    r.frame();
    assert!(r.v.row_tip.is_waiting());
    assert!(r.v.row_tip.on_timer());
    r.frame();
    assert!(r.c.has_text("Type: Text document"), "grid tooltip: {:?}", r.c.texts());
    assert!(r.c.has_text("Size: 1 B"), "grid tooltip: {:?}", r.c.texts());

    // Disable row_tooltips in prefs -> tooltip gone immediately
    r.v.prefs.general.row_tooltips = false;
    r.v.apply_prefs(&mut r.ui, crate::settings::C_OTHER);
    r.frame();
    assert!(!r.v.row_tip.is_showing() && !r.v.row_tip.is_waiting());
    assert!(!r.c.texts().iter().any(|s| s.starts_with("Type: ")));
}

#[test]
fn icon_overlays_draw_in_details_and_grid_for_lnk_symlink_and_onedrive() {
    let mut r = Rig::new("b10d-ov");
    let mut store = EntryStore::new();
    let ft = ((1_741_945_260u64 + 11_644_473_600) * 10_000_000) as i64;
    let n_lnk: Vec<u16> = "App.lnk".encode_utf16().collect();
    let n_sym: Vec<u16> = "SymDir".encode_utf16().collect();
    let n_cloud: Vec<u16> = "online.docx".encode_utf16().collect();
    let n_pin: Vec<u16> = "pinned.docx".encode_utf16().collect();
    let n_sync: Vec<u16> = "synced.docx".encode_utf16().collect();
    store
        .push_batch(
            0,
            &[
                EntryInput { name: &n_lnk, size: 100, modified: ft, flags: 0 },
                EntryInput { name: &n_sym, size: 0, modified: ft, flags: FOLDER | SYMLINK },
                EntryInput { name: &n_cloud, size: 200, modified: ft, flags: CLOUD_ONLINE },
                EntryInput { name: &n_pin, size: 300, modified: ft, flags: CLOUD_PINNED },
                EntryInput { name: &n_sync, size: 400, modified: ft, flags: CLOUD_UNPINNED },
            ],
        )
        .unwrap();
    r.v.tab_mut().adopt_store("C:\\OverlayTest", store);
    r.frame();
    let imgs = r.c.images();
    for key in ["ov:lnk", "ov:sym", "ov:cloud", "ov:pin", "ov:sync"] {
        assert!(imgs.contains(&key), "Details missing {key}: {imgs:?}");
    }

    r.v.set_mode(ViewMode::GridSmall);
    r.frame();
    let imgs = r.c.images();
    for key in ["ov:lnk", "ov:sym", "ov:cloud", "ov:pin", "ov:sync"] {
        assert!(imgs.contains(&key), "Grid missing {key}: {imgs:?}");
    }
}
