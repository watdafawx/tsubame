//! Whole-frame tests for CARD B11-G: per-folder view memory, the tab context menu and the
//! column drag-reorder. Child of `view::tests`: it uses the `Rig`.

use super::*;
use tabmenu::TabAction;
use tsubame_core::Column;

// ---- helpers -------------------------------------------------------------------------------

fn fv_file(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("tsubame-b11g-{tag}-{}.tsv", std::process::id()))
}

/// Gives the rig's view a folder-views file and waits for the (background) read.
fn attach(r: &mut Rig, file: &std::path::Path) {
    let _ = std::fs::remove_file(file);
    r.v.attach_folder_views(file.to_path_buf());
    wait_loaded(r);
}

fn wait_loaded(r: &mut Rig) {
    let t0 = Instant::now();
    while !r.v.folder_views.loaded {
        r.v.pump();
        assert!(t0.elapsed() < Duration::from_secs(10), "the folder-views file did not load");
        std::thread::sleep(Duration::from_millis(1));
    }
    r.settle();
}

fn path_of(r: &Rig, name: &str) -> String {
    r.dir.join(name).to_string_lossy().into_owned()
}

/// Where the text `s` was drawn in the band from `y0` to `y1`.
fn text_rect(r: &Rig, s: &str, y0: f32, y1: f32) -> Rect {
    r.c.ops
        .iter()
        .find_map(|o| match o {
            tsubame_ui::testcanvas::Op::Text(t, rc, _, _) if t == s && rc.y >= y0 && rc.y < y1 => Some(*rc),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text {s:?} in y {y0}..{y1}: {:?}", r.c.texts()))
}

/// The header cell of column `col` (the label's rect is the cell inset by 8 DIPs).
fn header_cell(r: &Rig, col: Col) -> Rect {
    let t = r.ui.theme;
    let y0 = t.height_titlebar + t.height_toolbar;
    let rc = text_rect(r, r.v.colset.label(col), y0, y0 + t.height_column_header);
    Rect::new(rc.x - 8.0, rc.y, rc.w + 16.0, rc.h)
}

fn click_header(r: &mut Rig, col: Col) {
    let c = header_cell(r, col);
    r.click(c.x + c.w / 2.0, c.y + c.h / 2.0);
}

/// Press the header cell of `col`, drag to `x` and let go.
fn drag_header(r: &mut Rig, col: Col, x: f32) {
    let c = header_cell(r, col);
    let (px, py) = (c.x + c.w / 2.0, c.y + c.h / 2.0);
    let pos = Pt::new(px, py);
    r.ui.push(Event::MouseMove { pos });
    r.frame();
    r.ui.push(Event::MouseDown { pos, button: Button::Left, mods: Mods::NONE, double: false });
    r.frame();
    for step in 1..=4 {
        let at = Pt::new(px + (x - px) * step as f32 / 4.0, py);
        r.ui.push(Event::MouseMove { pos: at });
        r.frame();
    }
    let end = Pt::new(x, py);
    r.ui.push(Event::MouseUp { pos: end, button: Button::Left, mods: Mods::NONE });
    r.frame();
}

fn names(v: &View) -> Vec<Col> {
    v.colset.order().to_vec()
}

// ---- per-folder view memory ----------------------------------------------------------------

#[test]
fn a_folder_remembers_its_mode_and_sort_and_the_others_show_the_default() {
    let mut r = Rig::new("b11g-nav");
    let file = fv_file("nav");
    attach(&mut r, &file);
    let (root, docs) = (r.dir.to_string_lossy().into_owned(), path_of(&r, "Docs"));
    assert!(r.v.folder_memory_on());
    assert_eq!(r.v.mode, ViewMode::Details);

    r.v.go(&docs);
    r.settle();
    // sort by Size with a real header click, then large thumbnails
    click_header(&mut r, Col::Size);
    assert_eq!(r.v.tab().sort.column, Column::Size);
    r.v.run_command(&mut r.ui, CMD_VIEW_GRID_LARGE);
    assert_eq!(r.v.mode, ViewMode::GridLarge);
    assert_eq!(r.v.folder_views.len(), 1, "only Docs remembers it");

    // another folder: the default, not Docs' view
    r.v.go(&root);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::Details);
    assert_eq!(r.v.tab().sort.column, Column::Name);
    assert_eq!(r.v.folder_views.len(), 1, "visiting a folder stores nothing");

    // back: Docs has its view again, in the same tab
    r.v.go(&docs);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::GridLarge);
    assert_eq!(r.v.tab().sort.column, Column::Size);
    assert!(!r.v.tab().sort.descending);

    // the path's case and a trailing slash find the same folder
    r.v.go(&root);
    r.settle();
    r.v.go(&format!("{}\\", docs.to_uppercase()));
    r.settle();
    assert_eq!(r.v.mode, ViewMode::GridLarge, "keys ignore case and the trailing separator");

    // tabs: each one shows what its folder remembers when it comes to the front
    r.v.go(&root);
    r.settle();
    r.v.open_in_new_tab(&docs);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::GridLarge, "a new tab on Docs");
    r.v.switch_tab(0);
    assert_eq!(r.v.mode, ViewMode::Details, "the root tab");
    r.v.switch_tab(1);
    assert_eq!(r.v.mode, ViewMode::GridLarge);
    let _ = std::fs::remove_file(&file);
}

/// Smoke bug: the right-click background menu's "Sort by >" changed the sort but the folder
/// forgot it; header clicks were remembered. Every route shares `remember_folder_view`.
#[test]
fn sort_by_from_the_background_menu_is_remembered_per_folder() {
    let mut r = Rig::new("b11fix-sortmenu");
    let file = fv_file("sortmenu");
    attach(&mut r, &file);
    let (root, docs) = (r.dir.to_string_lossy().into_owned(), path_of(&r, "Docs"));
    r.v.go(&docs);
    r.settle();
    r.v.apply_view_pick(crate::viewmenu::Pick::Sort(Column::Size));
    r.v.apply_view_pick(crate::viewmenu::Pick::Direction { descending: true });
    assert_eq!(r.v.folder_views.len(), 1, "Docs remembers the menu's sort");
    r.v.go(&root);
    r.settle();
    assert_eq!(r.v.tab().sort.column, Column::Name, "another folder keeps the default");
    r.v.go(&docs);
    r.settle();
    assert_eq!(r.v.tab().sort.column, Column::Size);
    assert!(r.v.tab().sort.descending);
    let _ = std::fs::remove_file(&file);
}

#[test]
fn memory_stays_off_without_a_file_or_with_the_setting_off() {
    // no file attached (tests, --dev): the old behaviour, one global view
    let mut r = Rig::new("b11g-off");
    let docs = path_of(&r, "Docs");
    assert!(!r.v.folder_memory_on());
    r.v.go(&docs);
    r.settle();
    r.v.run_command(&mut r.ui, CMD_VIEW_GRID_SMALL);
    r.v.go(&r.dir.to_string_lossy());
    r.settle();
    assert_eq!(r.v.mode, ViewMode::GridSmall, "the view stays");
    assert!(r.v.folder_views.is_empty());

    // attached but switched off in Settings: nothing stored, nothing applied
    let file = fv_file("off");
    r.v.set_mode(ViewMode::Details);
    attach(&mut r, &file);
    r.v.prefs.general.remember_folder_views = false;
    r.v.go(&docs);
    r.settle();
    r.v.run_command(&mut r.ui, CMD_VIEW_GRID_LARGE);
    assert!(r.v.folder_views.is_empty(), "off: nothing remembered");
    r.v.go(&r.dir.to_string_lossy());
    r.settle();
    assert_eq!(r.v.mode, ViewMode::GridLarge, "off: the view stays");
    // back on: Docs has nothing stored, so it shows the default
    r.v.prefs.general.remember_folder_views = true;
    r.v.go(&docs);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::Details);
    let _ = std::fs::remove_file(&file);
}

#[test]
fn settings_archives_and_miller_are_never_remembered() {
    let mut r = Rig::new("b11g-excl");
    let file = fv_file("excl");
    attach(&mut r, &file);
    let docs = path_of(&r, "Docs");

    // an archive path has no key
    let real = r.v.tab().path.clone();
    r.v.tab_mut().path = r"C:\Temp\x.zip::\inner".into();
    assert!(r.v.memory_key().is_none());
    r.v.tab_mut().path = real;
    assert!(r.v.memory_key().is_some());

    // the Settings tab
    r.v.open_settings(None);
    assert!(r.v.tab().settings);
    assert!(r.v.memory_key().is_none());
    r.v.set_mode_preset(ViewMode::GridSmall);
    assert!(r.v.folder_views.is_empty(), "Settings stores no view");
    r.v.switch_tab(0);
    r.v.set_mode(ViewMode::Details);

    // Miller keeps itself over navigation and is not stored
    r.key(Key::Char('5'), Mods::CTRL_SHIFT);
    assert_eq!(r.v.mode, ViewMode::Miller);
    assert!(!r.v.view_is_remembered());
    r.v.go(&docs);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::Miller, "navigating in Miller does not switch views");
    assert!(r.v.folder_views.is_empty());
    let _ = std::fs::remove_file(&file);
}

#[test]
fn apply_to_all_makes_this_view_the_default_and_reset_forgets_every_folder() {
    let mut r = Rig::new("b11g-all");
    let file = fv_file("all");
    attach(&mut r, &file);
    let (root, docs) = (r.dir.to_string_lossy().into_owned(), path_of(&r, "Docs"));
    r.v.go(&docs);
    r.settle();
    r.v.run_command(&mut r.ui, CMD_VIEW_GRID_LARGE);
    assert_eq!(r.v.folder_views.len(), 1);

    // "use this view for all folders": the default is Docs' view and the entries go
    r.v.run_command(&mut r.ui, CMD_APPLY_VIEW_ALL);
    assert!(r.v.folder_views.is_empty());
    assert_eq!(r.v.folder_views.default.mode, tsubame_config::ViewMode::GridLarge);
    r.v.go(&root);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::GridLarge, "a folder with no memory shows the new default");

    // one folder goes its own way, then "reset folder views" forgets it
    r.v.run_command(&mut r.ui, CMD_VIEW_DETAILS);
    assert_eq!(r.v.folder_views.len(), 1);
    r.v.go(&docs);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::GridLarge);
    r.v.go(&root);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::Details, "the root remembers Details");
    r.v.run_command(&mut r.ui, CMD_RESET_FOLDER_VIEWS);
    assert!(r.v.folder_views.is_empty());
    r.v.go(&docs);
    r.settle();
    r.v.go(&root);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::GridLarge, "after the reset every folder shows the default");

    // the config keeps the default, not whatever folder is on screen
    r.v.run_command(&mut r.ui, CMD_VIEW_DETAILS);
    let mut cfg = tsubame_config::Config::default();
    crate::session::sync_config(&r.v, &mut cfg);
    assert_eq!(cfg.appearance.view_mode, tsubame_config::ViewMode::GridLarge);
    let _ = std::fs::remove_file(&file);
}

#[test]
fn the_file_is_written_on_flush_and_read_back_at_startup() {
    let file = fv_file("disk");
    let docs;
    {
        let mut r = Rig::new("b11g-disk1");
        attach(&mut r, &file);
        docs = path_of(&r, "Docs");
        r.v.go(&docs);
        r.settle();
        r.v.run_command(&mut r.ui, CMD_VIEW_GRID_SMALL);
        assert!(r.v.folder_views.is_dirty());
        assert!(r.v.config_dirty, "the config's save gate times the file");
        r.v.flush_folder_views(true);
        assert!(!r.v.folder_views.is_dirty());
        // nothing changed: nothing written again
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        r.v.flush_folder_views(true);
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
    }
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("grid-small"), "{text}");
    assert!(text.to_lowercase().contains(&docs.to_lowercase()), "{text}");

    // a new window: the file is read on a worker and applied to the folder on screen
    let mut r = Rig::new("b11g-disk2");
    r.v.go(&docs);
    r.settle();
    assert_eq!(r.v.mode, ViewMode::Details, "not applied before the file arrives");
    r.v.attach_folder_views(file.clone());
    wait_loaded(&mut r);
    assert_eq!(r.v.mode, ViewMode::GridSmall, "Docs shows its remembered view once the file is in");
    let _ = std::fs::remove_file(&file);
}

// ---- tab context menu ----------------------------------------------------------------------

fn tab_ids(r: &mut Rig) -> Vec<u64> {
    r.v.tabs.items.iter().map(|t| t.id).collect()
}

/// Three tabs: root, Docs, root again (the last one active).
fn three_tabs(tag: &str) -> Rig {
    let mut r = Rig::new(tag);
    let docs = path_of(&r, "Docs");
    r.v.open_in_new_tab(&docs);
    r.settle();
    let root = r.dir.to_string_lossy().into_owned();
    r.v.open_in_new_tab(&root);
    r.settle();
    assert_eq!(r.v.tabs.len(), 3);
    r
}

#[test]
fn tab_actions_close_others_and_to_the_right_and_feed_the_reopen_stack() {
    let mut r = three_tabs("b11g-close");
    let ids = tab_ids(&mut r);
    r.v.run_tab_action(&mut r.ui, 0, ids[0], TabAction::CloseRight);
    assert_eq!(tab_ids(&mut r), vec![ids[0]]);
    assert_eq!(r.v.tab().id, ids[0], "the tab of the menu is the one in front");
    assert_eq!(r.v.closed_tabs_len(), 2, "both went on the reopen stack");
    // the last tab never goes
    r.v.run_tab_action(&mut r.ui, 0, ids[0], TabAction::CloseOthers);
    r.v.run_tab_action(&mut r.ui, 0, ids[0], TabAction::CloseRight);
    assert_eq!(r.v.tabs.len(), 1);
    assert!(!r.v.quit);
    // Ctrl+Shift+T brings them back, newest first
    r.v.reopen_closed_tab();
    r.v.reopen_closed_tab();
    assert_eq!(r.v.tabs.len(), 3);
    assert_eq!(r.v.closed_tabs_len(), 0);

    // close others from the middle tab
    let ids = tab_ids(&mut r);
    r.v.run_tab_action(&mut r.ui, 0, ids[1], TabAction::CloseOthers);
    assert_eq!(tab_ids(&mut r), vec![ids[1]]);
    assert_eq!(r.v.tab().id, ids[1]);
    assert_eq!(r.v.closed_tabs_len(), 2);
    // Close: the last tab asks the window to close, like Ctrl+W
    r.v.run_tab_action(&mut r.ui, 0, ids[1], TabAction::Close);
    assert!(r.v.quit);
    assert_eq!(r.v.tabs.len(), 1, "the tab list is never empty");
}

#[test]
fn duplicate_tab_copies_folder_and_sort_next_to_the_original() {
    let mut r = three_tabs("b11g-dup");
    let ids = tab_ids(&mut r);
    r.v.switch_tab(1);
    r.v.tab_mut().sort_by(Column::Size);
    let (path, sort) = (r.v.tab().path.clone(), r.v.tab().sort);
    r.v.run_tab_action(&mut r.ui, 0, ids[1], TabAction::Duplicate);
    r.settle();
    assert_eq!(r.v.tabs.len(), 4);
    assert_eq!(r.v.tabs.active, 2, "the copy comes right after it, in front");
    assert_eq!(r.v.tab().path, path);
    assert_eq!(r.v.tab().sort, sort);
    assert_ne!(r.v.tab().id, ids[1]);
    assert_eq!(r.v.tabs.items[1].id, ids[1], "the original stays");
    // the palette command acts on the tab in front
    r.v.run_command(&mut r.ui, CMD_DUPLICATE_TAB);
    assert_eq!(r.v.tabs.len(), 5);
    // Settings has nothing to duplicate
    r.v.open_settings(None);
    let n = r.v.tabs.len();
    r.v.run_command(&mut r.ui, CMD_DUPLICATE_TAB);
    assert_eq!(r.v.tabs.len(), n);
}

#[test]
fn the_palette_commands_close_other_tabs_and_tabs_to_the_right() {
    let mut r = three_tabs("b11g-cmd");
    r.v.switch_tab(1);
    r.v.run_command(&mut r.ui, CMD_CLOSE_TABS_RIGHT);
    assert_eq!(r.v.tabs.len(), 2);
    assert_eq!(r.v.tabs.active, 1);
    r.v.run_command(&mut r.ui, CMD_CLOSE_TABS_RIGHT);
    assert_eq!(r.v.tabs.len(), 2, "nothing on its right");
    r.v.run_command(&mut r.ui, CMD_CLOSE_OTHER_TABS);
    assert_eq!(r.v.tabs.len(), 1);
    assert_eq!(r.v.closed_tabs_len(), 2);
}

#[test]
fn move_to_new_window_closes_the_tab_only_after_the_process_started() {
    let mut r = three_tabs("b11g-win");
    let ids = tab_ids(&mut r);
    // a spawn that fails (no such program) leaves the tab where it is
    r.v.env.exe = r.dir.join("no-such-tsubame.exe");
    r.v.run_tab_action(&mut r.ui, 0, ids[1], TabAction::MoveToWindow);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!r.v.pump_tab_windows(), "nothing was reported");
    assert_eq!(r.v.tabs.len(), 3);

    // the worker reports a started window: that tab closes (by id, wherever it is now)
    r.v.tab_windows.lock().unwrap().push(ids[1]);
    r.v.switch_tab(0);
    assert!(r.v.pump_tab_windows());
    assert_eq!(tab_ids(&mut r), vec![ids[0], ids[2]]);
    assert_eq!(r.v.closed_tabs_len(), 1, "it can be reopened here too");
    // a tab that is already gone, or two reports at once, are harmless
    r.v.tab_windows.lock().unwrap().extend([ids[1], ids[2]]);
    assert!(r.v.pump_tab_windows());
    assert_eq!(tab_ids(&mut r), vec![ids[0]]);
    // the last tab of the window stays: it would end this window
    r.v.tab_windows.lock().unwrap().push(ids[0]);
    assert!(r.v.pump_tab_windows());
    assert_eq!(r.v.tabs.len(), 1);
    assert!(!r.v.quit);
    assert!(!r.v.pump_tab_windows(), "idle: nothing queued, nothing to do");
}

#[test]
fn move_to_other_pane_needs_two_panes_and_moves_the_tab() {
    let mut r = three_tabs("b11g-pane");
    let ids = tab_ids(&mut r);
    // one pane: no such row
    let rows = tabmenu::rows(&tabmenu::MenuCtx { index: 1, count: 3, dual: r.v.other.is_some(), window_tabs: 3, folder: true, can_reopen: false });
    assert!(!rows.iter().any(|x| x.action == TabAction::MoveToPane));
    r.v.run_tab_action(&mut r.ui, 0, ids[1], TabAction::MoveToPane);
    assert_eq!(r.v.tabs.len(), 3, "no second pane: nothing moved");

    r.v.set_layout(true, tsubame_config::Split::Vertical);
    r.settle();
    assert!(r.v.other.is_some());
    let before = (r.v.pane_tabs(0).unwrap().items.len(), r.v.pane_tabs(1).unwrap().items.len());
    r.v.run_tab_action(&mut r.ui, 0, ids[1], TabAction::MoveToPane);
    r.settle();
    let after = (r.v.pane_tabs(0).unwrap().items.len(), r.v.pane_tabs(1).unwrap().items.len());
    assert_eq!(after, (before.0 - 1, before.1 + 1));
    assert!(r.v.pane_tabs(1).unwrap().items.iter().any(|t| t.id == ids[1]));
}

#[test]
fn right_click_on_a_tab_opens_the_menu_and_a_row_runs() {
    let mut r = Rig::new("b11g-menu");
    let docs = path_of(&r, "Docs");
    r.v.open_in_new_tab(&docs);
    r.settle();
    let (strip, _) = r.v.strips[0];
    // the second tab is titled after its folder
    let tab = text_rect(&r, "Docs", strip.y, strip.b());
    r.right_click(tab.x + 4.0, tab.y + tab.h / 2.0);
    for row in ["Close", "Close other tabs", "Close tabs to the right", "Duplicate tab", "Move to new window", "Reopen closed tab"] {
        assert!(r.c.has_text(row), "{row}: {:?}", r.c.texts());
    }
    assert!(!r.c.has_text("Move to other pane"), "one pane");
    assert!(r.v.tab_menu.is_some());
    let row = text_rect(&r, "Duplicate tab", 0.0, 720.0);
    r.click(row.x + 20.0, row.y + row.h / 2.0);
    assert!(r.v.tab_menu.is_none(), "the menu closed");
    assert_eq!(r.v.tabs.len(), 3, "Duplicate tab ran on the tab that was clicked");
    assert_eq!(r.v.tab().path, docs);
    assert!(!r.c.has_text("Close other tabs"));

    // a menu on the first tab, "Close other tabs"
    let (strip, _) = r.v.strips[0];
    let first = r.v.tabs.items[0].title().to_string();
    let first_r = r.c.ops.iter().find_map(|o| match o {
        tsubame_ui::testcanvas::Op::Text(t, rc, _, _) if rc.y >= strip.y && rc.y < strip.b() && first.starts_with(t.trim_end_matches('…')) => Some(*rc),
        _ => None,
    });
    let first_r = first_r.expect("the first tab's title");
    r.right_click(first_r.x + 4.0, first_r.y + first_r.h / 2.0);
    let row = text_rect(&r, "Close other tabs", 0.0, 720.0);
    r.click(row.x + 20.0, row.y + row.h / 2.0);
    assert_eq!(r.v.tabs.len(), 1);
    assert_eq!(r.v.closed_tabs_len(), 2);
    assert_eq!(r.c.clip_depth(), 0);
}

// ---- column order ----------------------------------------------------------------------------

#[test]
fn dragging_a_header_cell_reorders_the_columns_and_the_order_is_saved() {
    let mut r = Rig::new("b11g-cols");
    let default = names(&r.v);
    assert_eq!(default[0], Col::Name);
    assert!(default.len() >= 4, "{default:?}");
    assert!(r.v.prefs.columns.order.is_empty());
    let (a, b) = (default[1], *default.last().unwrap());
    assert_ne!(a, b);

    // drag the last column onto the left edge of the second: it goes second
    let target = header_cell(&r, a).x + 3.0;
    drag_header(&mut r, b, target);
    let now = names(&r.v);
    assert_eq!(now[0], Col::Name);
    assert_eq!(now[1], b, "{now:?}");
    assert_eq!(now.len(), default.len());
    assert_eq!(r.v.tab().sort.column, Column::Name, "a drag is not a sort click");
    assert!(r.v.config_dirty);
    assert_eq!(r.v.prefs.columns.order, crate::cols::order_ids(&now));
    // the header draws in the new order
    assert!(header_cell(&r, b).x < header_cell(&r, a).x);
    // and the config file gets it
    let mut cfg = tsubame_config::Config::default();
    crate::session::sync_config(&r.v, &mut cfg);
    assert_eq!(cfg.columns.order, r.v.prefs.columns.order);

    // a new window reads it back
    let mut r2 = Rig::new("b11g-cols2");
    r2.v.prefs.columns = cfg.columns.clone();
    r2.v.sync_basics();
    assert_eq!(names(&r2.v), now);

    // Name never moves: a drag from it, or onto its left edge, changes nothing
    let name = header_cell(&r, Col::Name);
    drag_header(&mut r, Col::Name, name.x + name.w + 200.0);
    assert_eq!(names(&r.v), now);
    let before = r.v.prefs.columns.order.clone();
    drag_header(&mut r, b, 2.0);
    assert_eq!(names(&r.v)[0], Col::Name, "nothing goes before Name");
    let _ = before;
    // a plain click still sorts
    click_header(&mut r, Col::Size);
    assert_eq!(r.v.tab().sort.column, Column::Size);
}

#[test]
fn reset_columns_restores_the_order_and_the_widths() {
    let mut r = Rig::new("b11g-reset");
    let default = names(&r.v);
    let shown = default.clone();
    r.v.reorder_columns(&shown, shown.len() - 1, 1);
    assert_ne!(names(&r.v), default);
    assert!(!r.v.prefs.columns.order.is_empty());
    r.v.config_dirty = false;
    // the header's right-click menu has the row
    let t = r.ui.theme;
    let at = Pt::new(700.0, t.height_titlebar + t.height_toolbar + t.height_column_header / 2.0);
    r.right_click(at.x, at.y);
    assert!(r.c.has_text("Reset columns"), "{:?}", r.c.texts());
    let row = text_rect(&r, "Reset columns", 0.0, 720.0);
    r.click(row.x + 20.0, row.y + row.h / 2.0);
    assert_eq!(names(&r.v), default);
    assert!(r.v.prefs.columns.order.is_empty());
    assert!(r.v.config_dirty);
    assert!(!r.c.has_text("Reset columns"), "the menu closed");

    // the command does the same
    r.v.reorder_columns(&shown, shown.len() - 1, 1);
    r.v.run_command(&mut r.ui, CMD_RESET_COLUMNS);
    assert_eq!(names(&r.v), default);
}

#[test]
fn a_saved_order_keeps_columns_that_are_off_or_unknown_in_place() {
    let mut r = Rig::new("b11g-ext");
    let default = names(&r.v);
    // an id nobody knows (an extension that is gone) and a column that is switched off
    r.v.prefs.columns.order = vec!["tsubame.column.gone".into(), "core.size".into(), "core.type".into()];
    r.v.sync_basics();
    let now = names(&r.v);
    assert_eq!(now[0], Col::Name);
    assert_eq!(now.len(), default.len(), "no column is lost or doubled: {now:?}");
    let mut a = now.clone();
    let mut b = default.clone();
    a.sort_by_key(|c| c.slot());
    b.sort_by_key(|c| c.slot());
    assert_eq!(a, b);
    // the unknown id stays in the config until the user reorders again
    assert_eq!(r.v.prefs.columns.order[0], "tsubame.column.gone");
}
