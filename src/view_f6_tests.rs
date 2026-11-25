//! Tree rows and Miller columns do what Details does: rename, multi-select,
//! drag and drop; plus the Miller details (typed path that fails, narrow
//! drill-down, resize grip, default width). Child of `view::tests`: it uses
//! the `Rig`.

use super::*;

const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };

/// A click with modifier keys held.
fn click_mods(r: &mut Rig, x: f32, y: f32, mods: Mods) {
    let pos = Pt::new(x, y);
    r.ui.push(Event::MouseMove { pos });
    r.frame();
    r.ui.push(Event::MouseDown { pos, button: Button::Left, mods, double: false });
    r.frame();
    r.ui.push(Event::MouseUp { pos, button: Button::Left, mods });
    r.frame();
}

fn tree_rig(tag: &str) -> Rig {
    let mut r = Rig::new(tag);
    std::fs::write(r.dir.join("Docs").join("inner.txt"), b"x").unwrap();
    r.key(Key::Char('4'), Mods::CTRL_SHIFT);
    assert_eq!(r.v.mode, ViewMode::Tree);
    tree_wait(&mut r, "tree lists", |r| r.v.tab().tree_on && r.v.tab().tree.as_ref().is_some_and(|m| m.len() == 3 && m.loading_count() == 0));
    // the tab's folder watcher arms on a worker: a change made before it would never show
    tree_wait(&mut r, "the folder watcher is armed", |r| r.v.tab().watched());
    std::thread::sleep(Duration::from_millis(400));
    r.v.pump();
    r.frame();
    r
}

fn miller_rig(tag: &str) -> Rig {
    let mut r = Rig::new(tag);
    std::fs::write(r.dir.join("Docs").join("inner.txt"), b"x").unwrap();
    r.key(Key::Char('5'), Mods::CTRL_SHIFT);
    assert_eq!(r.v.mode, ViewMode::Miller);
    tree_wait(&mut r, "columns list", |r| r.c.has_text("inner.txt") && r.v.tab().miller.as_ref().is_some_and(|m| !m.loading()));
    // the folder watchers arm on a worker: a change made at once would not show
    std::thread::sleep(Duration::from_millis(400));
    r.v.pump();
    r.frame();
    r
}

/// Where row `i` of the tree is on screen.
fn trow(r: &Rig, i: usize) -> (f32, f32) {
    (r.v.list_r.x + 150.0, r.v.row_mid_y(i))
}

/// Where row `pos` of Miller column `c` is on screen.
fn mrow(r: &Rig, c: usize, pos: usize) -> (f32, f32) {
    let rc = r.v.tab().miller.as_ref().unwrap().row_rect(c, pos).expect("row on screen");
    (rc.x + 60.0, rc.y + rc.h / 2.0)
}

fn mil(r: &Rig) -> &crate::miller::MillerModel {
    r.v.tab().miller.as_ref().unwrap()
}

fn sel_tree(r: &Rig) -> usize {
    r.v.tab().tree.as_ref().unwrap().sel_count()
}

// ---- rename -------------------------------------------------------------------------

#[test]
fn f2_renames_a_tree_row_inline_and_selects_it_again() {
    let mut r = tree_rig("f6-tree-f2");
    r.key(Key::F(2), Mods::NONE);
    let e = r.v.rename.as_ref().expect("inline editor in Tree rows");
    assert!(e.path.ends_with("Docs"), "{}", e.path);
    r.v.rename.as_mut().unwrap().input.set_text("Zeta");
    r.key(Key::Enter, Mods::NONE);
    assert!(r.v.rename.is_none());
    let to = r.dir.join("Zeta");
    tree_wait(&mut r, "renamed on disk", |_| to.exists());
    tree_wait(&mut r, "shown in the tree", |r| r.c.has_text("Zeta"));
    assert!(!r.dir.join("Docs").exists());
    tree_wait(&mut r, "the renamed row is selected", |r| r.v.tab().selected_paths(8).iter().any(|p| p.ends_with("Zeta")));
}

#[test]
fn f2_in_a_tree_asks_before_it_clobbers_a_name_in_the_same_folder() {
    let mut r = tree_rig("f6-tree-dup");
    r.key(Key::F(2), Mods::NONE);
    r.v.rename.as_mut().unwrap().input.set_text("alpha.txt");
    r.key(Key::Enter, Mods::NONE);
    // the name exists next to it: no rename happens, the editor stays
    assert!(r.dir.join("Docs").exists());
    assert!(r.dir.join("alpha.txt").exists());
}

#[test]
fn a_slow_second_click_renames_a_tree_row() {
    let mut r = tree_rig("f6-tree-slow");
    let (x, y) = trow(&r, 2);
    r.click(x, y);
    assert!(r.v.rename.is_none());
    std::thread::sleep(Duration::from_millis(crate::rename::SLOW_CLICK_MIN_MS + 80));
    r.click(x, y);
    assert!(r.v.rename.as_ref().is_some_and(|e| e.path.ends_with("beta.txt")), "{:?}", r.v.rename.as_ref().map(|e| e.path.clone()));
}

#[test]
fn f2_renames_in_miller_columns_and_the_new_name_is_selected() {
    let mut r = miller_rig("f6-miller-f2");
    r.key(Key::F(2), Mods::NONE);
    let e = r.v.rename.as_ref().expect("inline editor in Miller columns");
    assert!(e.path.ends_with("Docs"), "{}", e.path);
    r.v.rename.as_mut().unwrap().input.set_text("Zeta");
    r.key(Key::Enter, Mods::NONE);
    let to = r.dir.join("Zeta");
    tree_wait(&mut r, "renamed on disk", |_| to.exists());
    tree_wait(&mut r, "shown in the tree", |r| r.c.has_text("Zeta"));
    tree_wait(&mut r, "the renamed entry is selected", |r| r.v.tab().miller.as_ref().and_then(|m| m.selected_path()).is_some_and(|p| p.ends_with("Zeta")));
}

#[test]
fn a_slow_second_click_renames_a_miller_row() {
    let mut r = miller_rig("f6-miller-slow");
    let (x, y) = mrow(&r, mil(&r).active, 2);
    r.click(x, y);
    assert!(r.v.rename.is_none());
    std::thread::sleep(Duration::from_millis(crate::rename::SLOW_CLICK_MIN_MS + 80));
    r.click(x, y);
    assert!(r.v.rename.as_ref().is_some_and(|e| e.path.ends_with("beta.txt")));
    r.key(Key::Escape, Mods::NONE);
    assert!(r.v.rename.is_none());
}

#[test]
fn f2_on_several_rows_of_a_tree_renames_the_focused_one_and_says_so() {
    let mut r = tree_rig("f6-tree-batch");
    r.key(Key::Char('A'), Mods::CTRL);
    assert!(sel_tree(&r) > 1);
    r.key(Key::F(2), Mods::NONE);
    assert!(r.v.rename.is_some() && r.v.batch.is_none());
    assert!(r.v.ops.message.as_deref().is_some_and(|m| m.contains("Batch rename is in Details")), "{:?}", r.v.ops.message);
}

// ---- multi-select ----------------------------------------------------------------------

#[test]
fn tree_rows_take_ctrl_and_shift_clicks_and_ctrl_a_and_escape() {
    let mut r = tree_rig("f6-tree-multi");
    let n = r.v.tab().tree.as_ref().unwrap().len();
    assert_eq!(n, 3);
    let (x, y) = trow(&r, 0);
    r.click(x, y);
    assert_eq!(sel_tree(&r), 1);
    let (x, y) = trow(&r, 2);
    click_mods(&mut r, x, y, Mods::CTRL);
    assert_eq!(sel_tree(&r), 2, "ctrl adds");
    let (x, y) = trow(&r, 1);
    click_mods(&mut r, x, y, Mods::CTRL);
    assert_eq!(sel_tree(&r), 3);
    click_mods(&mut r, x, y, Mods::CTRL);
    assert_eq!(sel_tree(&r), 2, "ctrl on a selected row removes it");
    let (x0, y0) = trow(&r, 0);
    r.click(x0, y0);
    assert_eq!(sel_tree(&r), 1, "a plain click collapses");
    let (x2, y2) = trow(&r, 2);
    click_mods(&mut r, x2, y2, SHIFT);
    assert_eq!(sel_tree(&r), 3, "shift takes the range");
    r.key(Key::Escape, Mods::NONE);
    assert_eq!(sel_tree(&r), 0);
    r.key(Key::Char('A'), Mods::CTRL);
    assert_eq!(sel_tree(&r), 3);
    assert_eq!(r.v.tab().selected_paths(64).len(), 3);
    assert_eq!(r.v.menu_paths(1).len(), 3, "the menu works on the set");
    assert!(r.c.has_text("3 items selected") || r.c.texts().iter().any(|t| t.contains("3 selected") || t.contains("3 items")), "{:?}", r.c.texts());
}

#[test]
fn a_menu_on_a_row_outside_the_tree_selection_is_that_row_only() {
    let mut r = tree_rig("f6-tree-menu");
    let (x, y) = trow(&r, 0);
    r.click(x, y);
    let (x, y) = trow(&r, 1);
    click_mods(&mut r, x, y, Mods::CTRL);
    assert_eq!(r.v.menu_paths(0).len(), 2);
    assert_eq!(r.v.menu_paths(2).len(), 1, "row 2 is not in the set");
    assert!(r.v.menu_paths(2)[0].ends_with("beta.txt"));
}

#[test]
fn shift_arrows_extend_the_tree_selection() {
    let mut r = tree_rig("f6-tree-shift-arrows");
    let (x, y) = trow(&r, 0);
    r.click(x, y);
    r.key(Key::Down, SHIFT);
    r.key(Key::Down, SHIFT);
    assert_eq!(sel_tree(&r), 3);
}

#[test]
fn miller_rows_take_ctrl_and_shift_clicks_and_ctrl_a_and_escape() {
    let mut r = miller_rig("f6-miller-multi");
    let a = mil(&r).active;
    let (x, y) = mrow(&r, a, 1);
    r.click(x, y);
    assert_eq!(mil(&r).sel_count(), 1);
    let (x, y) = mrow(&r, a, 2);
    click_mods(&mut r, x, y, Mods::CTRL);
    assert_eq!(mil(&r).sel_count(), 2, "ctrl adds");
    assert_eq!(r.v.menu_paths(1).len(), 2, "the menu works on the set");
    assert_eq!(r.v.tab().selected_paths(64).len(), 2);
    let (x, y) = mrow(&r, a, 0);
    click_mods(&mut r, x, y, SHIFT);
    assert_eq!(mil(&r).sel_count(), 3, "shift takes the range from the anchor");
    r.key(Key::Escape, Mods::NONE);
    assert_eq!(mil(&r).sel_count(), 1, "Esc collapses to the focused row");
    r.key(Key::Char('A'), Mods::CTRL);
    assert_eq!(mil(&r).sel_count(), 3);
    assert_eq!(mil(&r).len(), mil(&r).active + 1, "no child column beside a set");
    assert!(r.v.tab().sel_total() == 3);
}

#[test]
fn shift_arrows_extend_the_miller_selection() {
    let mut r = miller_rig("f6-miller-shift-arrows");
    let a = mil(&r).active;
    let (x, y) = mrow(&r, a, 0);
    r.click(x, y);
    r.key(Key::Down, SHIFT);
    assert_eq!(mil(&r).sel_count(), 2);
    r.key(Key::Down, SHIFT);
    assert_eq!(mil(&r).sel_count(), 3);
    r.key(Key::Up, SHIFT);
    assert_eq!(mil(&r).sel_count(), 2);
}

#[test]
fn a_menu_on_a_miller_row_outside_the_set_is_that_row_only() {
    let mut r = miller_rig("f6-miller-menu");
    let a = mil(&r).active;
    let (x, y) = mrow(&r, a, 1);
    r.click(x, y);
    let (x, y) = mrow(&r, a, 2);
    click_mods(&mut r, x, y, Mods::CTRL);
    assert_eq!(r.v.menu_paths(2).len(), 2);
    assert_eq!(r.v.menu_paths(0).len(), 1, "Docs is not in the set");
}

// ---- drag and drop ------------------------------------------------------------------------

#[test]
fn a_drag_over_tree_rows_lands_in_the_folder_row_or_the_folder_of_a_file_row() {
    let mut r = tree_rig("f6-tree-drop");
    // open Docs so a file row sits inside it
    let (x, y) = trow(&r, 0);
    r.click(x, y);
    r.key(Key::Right, Mods::NONE);
    tree_wait(&mut r, "inner.txt shows", |r| r.c.has_text("inner.txt"));
    let docs = r.dir.join("Docs").to_str().unwrap().to_string();
    let alpha = r.dir.join("alpha.txt").to_str().unwrap().to_string();
    r.v.drag_enter(vec![alpha]);
    // the folder row: into it
    let (x, y) = trow(&r, 0);
    r.move_to(x, y);
    assert_eq!(r.v.drop_dest, Some(DropDest::Folder(docs.clone())));
    assert!(r.c.has_text("Move to Docs"), "{:?}", r.c.texts());
    // a file row inside Docs: into Docs
    let (x, y) = trow(&r, 1);
    r.move_to(x, y);
    assert_eq!(r.v.drop_dest, Some(DropDest::Folder(docs.clone())));
    // a file row of the root: the root, where alpha already is
    let (x, y) = trow(&r, 3);
    r.move_to(x, y);
    assert_eq!(r.v.drop_dest, None);
    assert!(r.c.has_text("Already in this folder"), "{:?}", r.c.texts());
    r.v.drag_leave();
    r.frame();
    assert_eq!(r.v.drop_dest, None);
}

#[test]
fn a_drag_resting_on_a_closed_tree_folder_opens_it_after_a_delay() {
    let mut r = tree_rig("f6-tree-hover");
    let alpha = r.dir.join("alpha.txt").to_str().unwrap().to_string();
    r.v.drag_enter(vec![alpha]);
    let (x, y) = trow(&r, 0);
    r.move_to(x, y);
    r.frame();
    assert!(!r.c.has_text("inner.txt"), "not at once");
    std::thread::sleep(Duration::from_millis(700));
    tree_wait(&mut r, "the folder opened under the drag", |r| r.c.has_text("inner.txt"));
    assert_eq!(r.v.tab().tree.as_ref().unwrap().expanded_count(), 1);
    // moving away and back restarts the clock: leaving cancels it
    r.v.drag_leave();
    assert!(r.v.hover_open.is_none());
}

#[test]
fn a_drag_that_moves_on_before_the_delay_opens_nothing() {
    let mut r = tree_rig("f6-tree-hover-cancel");
    let alpha = r.dir.join("alpha.txt").to_str().unwrap().to_string();
    r.v.drag_enter(vec![alpha]);
    let (x, y) = trow(&r, 0);
    r.move_to(x, y);
    assert!(r.v.hover_open.is_some(), "the clock runs on the folder");
    let (x, y) = trow(&r, 2);
    r.move_to(x, y);
    assert!(r.v.hover_open.is_none(), "a file row has no clock");
    std::thread::sleep(Duration::from_millis(700));
    r.frame();
    assert!(!r.c.has_text("inner.txt"));
}

#[test]
fn a_drag_over_miller_columns_lands_in_the_folder_row_or_the_column_folder() {
    let mut r = miller_rig("f6-miller-drop");
    let docs = r.dir.join("Docs").to_str().unwrap().to_string();
    let alpha = r.dir.join("alpha.txt").to_str().unwrap().to_string();
    let a = mil(&r).active;
    r.v.drag_enter(vec![alpha]);
    // the Docs row of the active column
    let (x, y) = mrow(&r, a, 0);
    r.move_to(x, y);
    assert_eq!(r.v.drop_dest, Some(DropDest::Folder(docs.clone())));
    assert!(r.c.has_text("Move to Docs"), "{:?}", r.c.texts());
    // a file row of the Docs column (the child): Docs itself
    let (x, y) = mrow(&r, a + 1, 0);
    r.move_to(x, y);
    assert_eq!(r.v.drop_dest, Some(DropDest::Folder(docs.clone())));
    // the empty space of the column that holds alpha: its folder, which refuses
    let rc = mil(&r).col_rows(a).unwrap();
    r.move_to(rc.x + 60.0, rc.b() - 6.0);
    assert_eq!(r.v.drop_dest, None);
    assert!(r.c.has_text("Already in this folder"), "{:?}", r.c.texts());
}

#[test]
fn a_press_on_a_miller_multi_selection_drags_all_of_it() {
    let mut r = miller_rig("f6-miller-drag");
    let a = mil(&r).active;
    let (x, y) = mrow(&r, a, 1);
    r.click(x, y);
    let (x, y) = mrow(&r, a, 2);
    click_mods(&mut r, x, y, Mods::CTRL);
    assert_eq!(mil(&r).sel_count(), 2);
    // the columns scrolled (the child column went): where the row is now
    let (x, y) = mrow(&r, a, 2);
    // press on a row of the set, pull away: the drag carries both
    let pos = Pt::new(x, y);
    r.ui.push(Event::MouseMove { pos });
    r.frame();
    r.ui.push(Event::MouseDown { pos, button: Button::Left, mods: Mods::NONE, double: false });
    r.frame();
    r.ui.push(Event::MouseMove { pos: Pt::new(x + 60.0, y + 40.0) });
    r.frame();
    let out = r.v.drag_out.take().expect("a drag started");
    assert_eq!(out.len(), 2, "{out:?}");
    r.ui.push(Event::MouseUp { pos: Pt::new(x + 60.0, y + 40.0), button: Button::Left, mods: Mods::NONE });
    r.frame();
}

// ---- Miller details ---------------------------------------------------------------------------

#[test]
fn a_typed_path_that_fails_goes_back_to_the_previous_folder_with_a_message() {
    let mut r = miller_rig("f6-miller-typed");
    let start = r.v.tab().path.clone();
    let bad = r.dir.join("nope").join("deeper");
    r.v.path_text.set_text(bad.to_str().unwrap());
    r.v.submit_path();
    tree_wait(&mut r, "the failure is noticed", |r| r.v.path_error.is_some());
    tree_wait(&mut r, "back and listed", |r| r.v.tab().path == start && r.v.tab().miller.as_ref().is_some_and(|m| !m.loading()));
    assert!(r.v.ops.message.as_deref().is_some_and(|m| m.contains("Can't open") && m.contains("previous folder")), "{:?}", r.v.ops.message);
    assert!(r.v.path_error.as_deref().is_some_and(|m| m.contains("Can't open")));
    assert!(r.c.has_text("beta.txt"), "the old folder is on screen: {:?}", r.c.texts());
    // the dead folder is not left in Recent
    assert!(!r.v.side.recents.iter().any(|p| p.contains("nope") || p.contains("deeper")), "{:?}", r.v.side.recents);
}

#[test]
fn a_typed_path_that_works_moves_on_with_no_message() {
    let mut r = miller_rig("f6-miller-typed-ok");
    let docs = r.dir.join("Docs");
    r.v.path_text.set_text(docs.to_str().unwrap());
    r.v.submit_path();
    tree_wait(&mut r, "in Docs", |r| r.v.tab().path.ends_with("Docs") && r.v.tab().miller.as_ref().is_some_and(|m| !m.loading()));
    r.frame();
    assert!(r.v.path_error.is_none());
    assert!(r.v.ops.message.as_deref().is_none_or(|m| !m.contains("Can't open")));
}

#[test]
fn the_narrow_drill_down_shows_the_whole_info_column_when_there_is_height_for_it() {
    let mut r = miller_rig("f6-drill-info");
    // a file: alpha.txt
    let a = mil(&r).active;
    let (x, y) = mrow(&r, a, 1);
    r.click(x, y);
    tree_wait(&mut r, "the info of a file", |r| r.v.tab().miller.as_ref().is_some_and(|m| m.shows_info()));
    r.c = tsubame_ui::TestCanvas::new(500.0, 900.0);
    r.frame();
    tree_wait(&mut r, "full info", |r| r.c.has_text("Attributes") && r.c.has_text("Location"));
    // a short window: the strip only
    r.c = tsubame_ui::TestCanvas::new(500.0, 380.0);
    r.frame();
    assert!(r.c.has_text("alpha.txt"), "{:?}", r.c.texts());
    assert!(!r.c.has_text("Attributes"), "strip only: {:?}", r.c.texts());
}

#[test]
fn the_resize_grip_drags_and_a_double_click_on_it_resets_the_width() {
    let mut r = miller_rig("f6-grip");
    let def = r.v.prefs.miller.sanitized().col_width as f32;
    let a = mil(&r).active;
    assert_eq!(mil(&r).width(a), def);
    let gy = r.v.list_r.y + 120.0;
    // the grip is the right edge of the column (its rows area stops at the scrollbar)
    let edge = |r: &Rig| {
        let m = mil(r);
        m.col_rows(a).unwrap().x + m.width(a) - 2.0
    };
    // drag the edge wider
    let pos = Pt::new(edge(&r), gy);
    r.ui.push(Event::MouseMove { pos });
    r.frame();
    r.ui.push(Event::MouseDown { pos, button: Button::Left, mods: Mods::NONE, double: false });
    r.frame();
    r.ui.push(Event::MouseMove { pos: Pt::new(pos.x + 50.0, gy) });
    r.frame();
    r.ui.push(Event::MouseUp { pos: Pt::new(pos.x + 50.0, gy), button: Button::Left, mods: Mods::NONE });
    r.frame();
    let w = mil(&r).width(a);
    assert!(w > def + 30.0, "dragged: {w}");
    // a double-click on the grip (a plain click first, then the double): the default again
    let pos = Pt::new(edge(&r), gy);
    r.ui.push(Event::MouseMove { pos });
    r.frame();
    r.ui.push(Event::MouseDown { pos, button: Button::Left, mods: Mods::NONE, double: false });
    r.frame();
    r.ui.push(Event::MouseUp { pos, button: Button::Left, mods: Mods::NONE });
    r.frame();
    let pos = Pt::new(edge(&r), gy);
    r.ui.push(Event::MouseDown { pos, button: Button::Left, mods: Mods::NONE, double: true });
    r.frame();
    r.ui.push(Event::MouseUp { pos, button: Button::Left, mods: Mods::NONE });
    r.frame();
    assert_eq!(mil(&r).width(a), def, "double-click resets");
}

#[test]
fn the_default_column_width_setting_applies_to_columns_without_a_width_of_their_own() {
    let mut r = miller_rig("f6-colw");
    r.v.prefs.miller.col_width = 300;
    r.v.pump();
    r.frame();
    assert_eq!(mil(&r).width(0), 300.0, "live, no restart");
    // a column the user dragged keeps its own
    r.v.tab_mut().miller.as_mut().unwrap().set_width(0, 200.0);
    r.v.prefs.miller.col_width = 420;
    r.v.pump();
    r.frame();
    assert_eq!(mil(&r).width(0), 200.0);
    assert_eq!(mil(&r).width(1), 420.0);
}

#[test]
fn the_settings_slider_reaches_the_columns_without_a_worker_wake() {
    // the app only runs `pump` on a worker wake: the Settings page path is `apply_prefs`
    let mut r = miller_rig("f6-colw-live");
    r.v.prefs.miller.col_width = 380;
    r.v.apply_prefs(&mut r.ui, crate::settings::C_OTHER);
    assert_eq!(mil(&r).width(0), 380.0, "no pump in between");
    r.frame();
    assert_eq!(mil(&r).width(0), 380.0);
}
