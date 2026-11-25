//! B10-A Explorer basics: zoom (Ctrl+wheel, Ctrl+Plus / Minus), type-ahead, invert selection,
//! the column header's menu and fit, hidden extensions. Child of `view::tests`: it uses the `Rig`.

use super::*;
use crate::view::basics::*;
use tsubame_config::ViewConfig;
use tsubame_core::{EntryInput, EntryStore, Sort};

fn store(names: &[&str]) -> EntryStore {
    let mut s = EntryStore::new();
    s.sort(Sort { column: tsubame_core::Column::Name, descending: false, folders_first: false });
    for (i, n) in names.iter().enumerate() {
        let n: Vec<u16> = n.encode_utf16().collect();
        s.push_batch(0, &[EntryInput { name: &n, size: i as u64, modified: i as i64, flags: 0 }]).unwrap();
    }
    s
}

fn ctrl() -> Mods {
    Mods { ctrl: true, shift: false, alt: false }
}

/// A wheel over the list, one frame later.
fn wheel(r: &mut Rig, dy: f32, mods: Mods) {
    r.ui.push(Event::Wheel { pos: Pt::new(600.0, 300.0), dy, dx: 0.0, mods });
    r.frame();
}

// ---- pure ------------------------------------------------------------------------------

#[test]
fn grid_cells_keep_the_old_presets_and_grow_with_the_size() {
    assert_eq!(grid_dims(96.0), (96.0, 118.0, 64.0, 48.0), "the old small cell");
    assert_eq!(grid_dims(152.0), (152.0, 182.0, 128.0, 64.0), "the old large cell");
    let mut last = (0.0, 0.0, 0.0, 0.0);
    let mut s = ViewConfig::GRID_MIN;
    while s <= ViewConfig::GRID_MAX {
        let d = grid_dims(s as f32);
        assert_eq!(d.0, s as f32);
        assert!(d.2 >= last.2 && d.1 > last.1, "bigger cell, bigger picture: {s} {d:?} after {last:?}");
        assert!(d.3 <= d.2 && d.3 >= 16.0, "the type icon fits its box: {d:?}");
        assert!(d.2 + 8.0 <= d.0, "the box sits inside the cell width: {d:?}");
        last = d;
        s += ViewConfig::GRID_STEP;
    }
    assert_eq!(grid_mode_for(96), ViewMode::GridSmall);
    assert_eq!(grid_mode_for(152), ViewMode::GridLarge);
}

#[test]
fn zoom_steps_cross_between_details_and_the_grids() {
    let (min, max) = (ViewConfig::GRID_MIN, ViewConfig::GRID_MAX);
    assert_eq!(zoom_step(ViewMode::Details, 96, 1), Some((ViewMode::GridSmall, min)), "up from Details: the smallest grid");
    assert_eq!(zoom_step(ViewMode::Details, 96, -1), None, "Details is the end");
    assert_eq!(zoom_step(ViewMode::GridSmall, min, -1), Some((ViewMode::Details, min)), "below the smallest grid: Details");
    assert_eq!(zoom_step(ViewMode::GridSmall, 56, -1), Some((ViewMode::GridSmall, 48)));
    assert_eq!(zoom_step(ViewMode::GridSmall, 120, 1), Some((ViewMode::GridLarge, 128)));
    assert_eq!(zoom_step(ViewMode::GridLarge, max, 1), None);
    assert_eq!(zoom_step(ViewMode::GridLarge, max - 4, 1), Some((ViewMode::GridLarge, max)), "never past the largest");
    assert_eq!(zoom_step(ViewMode::Tree, 96, 1), None);
    assert_eq!(zoom_step(ViewMode::Miller, 96, -1), None);
}

#[test]
fn wheel_deltas_add_up_to_whole_steps() {
    let mut b = Basics::default();
    let t0 = Instant::now();
    assert_eq!(b.wheel_steps(1.0, t0), 2, "a mouse notch is two steps");
    assert_eq!(b.wheel_steps(-1.0, t0), -2);
    // a touchpad: tenths wait until they make a step
    let mut got = 0;
    for _ in 0..4 {
        got += b.wheel_steps(0.1, t0);
    }
    assert_eq!(got, 0, "0.4 notch is not a step yet");
    assert_eq!(b.wheel_steps(0.1, t0), 1, "0.5 is");
    assert_eq!(b.wheel_steps(0.3, t0), 0);
    // turning around drops what was waiting
    assert_eq!(b.wheel_steps(-0.3, t0), 0);
    assert_eq!(b.wheel_steps(-0.2, t0), -1, "only the new direction counts: 0.5 down");
    // a pause drops it too
    assert_eq!(b.wheel_steps(0.4, t0), 0);
    assert_eq!(b.wheel_steps(0.4, t0 + WHEEL_IDLE + Duration::from_millis(1)), 0, "0.4 after a pause, not 0.8");
}

#[test]
fn type_ahead_matches_prefixes_case_insensitively_and_cycles() {
    let s = store(&["Apple", "avocado", "Banana", "apricot", "berry"]);
    let names: Vec<String> = (0..5).map(|i| String::from_utf16_lossy(s.name(s.at(i).unwrap()).unwrap())).collect();
    assert_eq!(names, ["Apple", "apricot", "avocado", "Banana", "berry"], "the view order the test works in");
    assert_eq!(type_ahead_find(&s, "a", 0, false), Some(0), "nothing selected: the first match, even the cursor row");
    assert_eq!(type_ahead_find(&s, "a", 0, true), Some(1), "a letter again moves on to the next one");
    assert_eq!(type_ahead_find(&s, "a", 2, true), Some(0), "and wraps");
    assert_eq!(type_ahead_find(&s, "aa", 0, true), Some(1), "the same letter twice still cycles");
    assert_eq!(type_ahead_find(&s, "AV", 0, true), Some(2), "case does not matter");
    assert_eq!(type_ahead_find(&s, "ap", 0, true), Some(0), "a longer text stays where the cursor is");
    assert_eq!(type_ahead_find(&s, "apr", 0, true), Some(1));
    assert_eq!(type_ahead_find(&s, "ber", 3, true), Some(4));
    assert_eq!(type_ahead_find(&s, "zz", 0, true), None);
    assert_eq!(type_ahead_find(&s, "", 0, true), None);
    assert_eq!(type_ahead_find(&store(&[]), "a", 0, false), None);
}

#[test]
fn the_typing_buffer_restarts_after_a_pause() {
    let mut b = Basics::default();
    let t0 = Instant::now();
    assert_eq!(b.type_push('a', t0), "a");
    assert_eq!(b.type_push('b', t0 + Duration::from_millis(400)), "ab");
    assert_eq!(b.type_push('c', t0 + Duration::from_millis(800)), "abc", "400 ms since the last letter, not 800 since the first");
    assert_eq!(b.type_push('d', t0 + Duration::from_millis(1400)), "d", "600 ms of quiet starts again");
}

#[test]
fn rows_hide_the_extension_of_files_only() {
    let mut r = RowBuf::new();
    for (name, dir, shown) in [("a.txt", false, "a"), (".gitignore", false, ".gitignore"), ("Makefile", false, "Makefile"), ("v1.2.tar.gz", false, "v1.2.tar"), ("dir.d", true, "dir.d")] {
        let n: Vec<u16> = name.encode_utf16().collect();
        let mut s = EntryStore::new();
        s.push_batch(0, &[EntryInput { name: &n, size: 1, modified: 1, flags: if dir { tsubame_core::FOLDER } else { 0 } }]).unwrap();
        r.fill(&s, s.at(0).unwrap());
        data::set_show_extensions(true);
        assert_eq!(r.label(), name);
        data::set_show_extensions(false);
        assert_eq!(r.label(), shown, "{name}");
        assert_eq!(r.name_str(), name, "the real name is untouched");
    }
    data::set_show_extensions(true);
}

// ---- in the window ---------------------------------------------------------------------

#[test]
fn ctrl_plus_and_minus_walk_the_sizes_and_cross_to_details() {
    let mut r = Rig::new("zoom-keys");
    assert_eq!(r.v.mode, ViewMode::Details);
    r.key(Key::Char('='), ctrl());
    assert_eq!(r.v.mode, ViewMode::GridSmall, "Ctrl+= from Details: the smallest grid");
    assert_eq!(r.v.prefs.view.grid_size, 48);
    assert_eq!(r.v.tab().grid.cell_w, 48.0);
    r.key(Key::Char('='), Mods { ctrl: true, shift: true, alt: false });
    assert_eq!(r.v.prefs.view.grid_size, 64, "Ctrl+Shift+= (Ctrl+Plus) is the same command: two steps");
    assert_eq!((r.v.tab().grid.cell_w, r.v.tab().grid.cell_h), (64.0, grid_dims(64.0).1));
    r.key(Key::Char('-'), ctrl());
    assert_eq!(r.v.prefs.view.grid_size, 48);
    r.key(Key::Char('-'), ctrl());
    assert_eq!(r.v.mode, ViewMode::Details, "below the smallest grid: Details");
    r.key(Key::Char('-'), ctrl());
    assert_eq!(r.v.mode, ViewMode::Details, "and no further");
    // the chords still set the presets
    r.key(Key::Char('2'), Mods::CTRL_SHIFT);
    assert_eq!((r.v.mode, r.v.prefs.view.grid_size), (ViewMode::GridSmall, 96));
    r.key(Key::Char('3'), Mods::CTRL_SHIFT);
    assert_eq!((r.v.mode, r.v.prefs.view.grid_size), (ViewMode::GridLarge, 152));
    assert_eq!((r.v.tab().grid.cell_w, r.v.tab().grid.cell_h), (152.0, 182.0));
    assert!(r.v.config_dirty);
    let mut cfg = tsubame_config::Config::default();
    crate::session::sync_config(&r.v, &mut cfg);
    assert_eq!(cfg.view.grid_size, 152, "the size is saved in [view]");
    assert_eq!(cfg.appearance.view_mode, tsubame_config::ViewMode::GridLarge);
}

#[test]
fn ctrl_wheel_zooms_in_both_layouts_and_a_plain_wheel_does_not() {
    let mut r = Rig::new("zoom-wheel");
    wheel(&mut r, 1.0, Mods::NONE);
    assert_eq!(r.v.mode, ViewMode::Details, "a plain wheel never zooms");
    wheel(&mut r, 1.0, ctrl());
    assert_eq!((r.v.mode, r.v.prefs.view.grid_size), (ViewMode::GridSmall, 48), "wheel up from Details: the smallest grid");
    assert_eq!(r.v.tab().grid.cell_w, 48.0);
    wheel(&mut r, 1.0, ctrl());
    assert_eq!(r.v.prefs.view.grid_size, 64, "a notch is two steps of 8");
    wheel(&mut r, 4.0, ctrl());
    assert_eq!(r.v.prefs.view.grid_size, 128);
    assert_eq!(r.v.mode, ViewMode::GridLarge, "past 120 the toolbar says large");
    assert_eq!(r.v.tab().grid.cell_w, 128.0);
    assert_eq!(r.v.tab().grid.cell_h, grid_dims(128.0).1, "the cells follow the size, no preset");
    wheel(&mut r, -20.0, ctrl());
    assert_eq!(r.v.mode, ViewMode::Details, "wheel down past the smallest grid: Details");
    // a touchpad: small deltas wait for a step
    let size = |r: &Rig| r.v.prefs.view.grid_size;
    wheel(&mut r, 0.4, ctrl());
    assert_eq!(r.v.mode, ViewMode::Details, "0.4 notch is not a step");
    wheel(&mut r, 0.1, ctrl());
    assert_eq!((r.v.mode, size(&r)), (ViewMode::GridSmall, 48));
    // the size stays inside its range
    wheel(&mut r, 500.0, ctrl());
    assert_eq!(size(&r), ViewConfig::GRID_MAX);
    // and it works over a grid too (the pointer is over the cells)
    wheel(&mut r, -1.0, ctrl());
    assert_eq!(size(&r), ViewConfig::GRID_MAX - 16);
    assert_eq!(r.c.clip_depth(), 0);
}

#[test]
fn zoom_works_in_the_second_pane_too() {
    let mut r = Rig::new("zoom-dual");
    r.v.set_layout(true, tsubame_config::Split::Vertical);
    r.settle();
    // the right pane's list
    r.ui.push(Event::Wheel { pos: Pt::new(900.0, 300.0), dy: 1.0, dx: 0.0, mods: ctrl() });
    r.frame();
    assert_eq!((r.v.mode, r.v.prefs.view.grid_size), (ViewMode::GridSmall, 48));
    r.frame();
    for slot in 0..2 {
        let w = r.v.with_pane(slot, |v| v.tab().grid.cell_w);
        assert_eq!(w, 48.0, "pane {slot} follows");
    }
}

#[test]
fn typing_jumps_instead_of_filtering_and_ctrl_f_still_filters() {
    let mut r = Rig::new("typeahead");
    assert!(!r.v.prefs.view.type_ahead_filters, "off by default");
    r.ui.push(Event::Char('b'));
    r.frame();
    assert_eq!(r.v.tab().filter, "", "no filter");
    assert_eq!(r.v.tab().shown(), 3, "every item stays");
    assert_eq!(r.v.tab().cursor_row(), 2, "beta.txt: after Docs and alpha.txt");
    assert_eq!(r.v.tab().sel.count(), 1);
    assert!(!r.ui.has_focus(pid(r.v.live, ID_FILTER)), "the filter box did not take the focus");
    // within 500 ms the next letter extends the search
    r.ui.push(Event::Char('e'));
    r.frame();
    assert_eq!(r.v.tab().cursor_row(), 2);
    // after a pause a letter starts a new one
    std::thread::sleep(TYPE_AHEAD + Duration::from_millis(60));
    r.ui.push(Event::Char('A'));
    r.frame();
    assert_eq!(r.v.tab().cursor_row(), 1, "alpha.txt, case-insensitive");
    // a space is not a letter: it selects / opens Quick Look as before
    r.ui.push(Event::Char(' '));
    r.frame();
    assert_eq!(r.v.tab().cursor_row(), 1);
    assert_eq!(r.v.tab().filter, "");
    // Ctrl+F is the filter box, and typing in it filters
    r.key(Key::Char('F'), ctrl());
    assert!(r.ui.has_focus(pid(r.v.live, ID_FILTER)));
    r.ui.push(Event::Char('b'));
    r.frame();
    assert_eq!(r.v.tab().filter, "b");
    assert_eq!(r.v.tab().shown(), 1);
}

#[test]
fn typing_jumps_in_the_grid_too() {
    let mut r = Rig::new("typeahead-grid");
    r.v.set_mode(ViewMode::GridSmall);
    r.frame();
    r.ui.push(Event::Char('b'));
    r.frame();
    assert_eq!(r.v.tab().grid.cursor, 2);
    assert_eq!(r.v.tab().sel.count(), 1);
    assert_eq!(r.v.tab().filter, "");
}

#[test]
fn the_old_typing_behaviour_comes_back_with_the_setting() {
    let mut r = Rig::new("typeahead-old");
    r.v.prefs.view.type_ahead_filters = true;
    r.ui.push(Event::Char('b'));
    r.frame();
    assert_eq!(r.v.tab().filter, "b");
    assert_eq!(r.v.tab().shown(), 1);
}

#[test]
fn invert_selection_flips_every_row_of_the_listing() {
    let mut r = Rig::new("invert");
    r.key(Key::Down, Mods::NONE);
    assert_eq!(r.v.tab().sel.count(), 1);
    let first = r.v.tab().store.at(0).unwrap();
    assert!(r.v.tab().sel.contains(first) || r.v.tab().sel.count() == 1);
    let picked: Vec<bool> = (0..3).map(|i| r.v.tab().sel.contains(r.v.tab().store.at(i).unwrap())).collect();
    r.key(Key::Char('A'), Mods::CTRL_SHIFT);
    let now: Vec<bool> = (0..3).map(|i| r.v.tab().sel.contains(r.v.tab().store.at(i).unwrap())).collect();
    assert_eq!(now, picked.iter().map(|b| !b).collect::<Vec<_>>());
    assert_eq!(r.v.tab().sel.count(), 2);
    r.key(Key::Char('A'), Mods::CTRL_SHIFT);
    assert_eq!(r.v.tab().sel.count(), 1, "twice is the identity");
    // nothing selected: everything is
    r.v.tab_mut().sel.clear();
    r.key(Key::Char('A'), Mods::CTRL_SHIFT);
    assert_eq!(r.v.tab().sel.count(), 3);
    // plain Ctrl+A is still select all
    r.v.tab_mut().sel.clear();
    r.key(Key::Char('A'), Mods::CTRL);
    assert_eq!(r.v.tab().sel.count(), 3);
    assert!(crate::commands::by_name("select.invert").is_some(), "in the palette and the key settings");
}

#[test]
fn show_extensions_off_draws_names_without_them_and_sorting_keeps_the_real_name() {
    let mut r = Rig::new("ext");
    assert!(r.c.has_text("alpha.txt"));
    r.v.run_command(&mut r.ui, CMD_VIEW_EXTENSIONS);
    r.frame();
    assert!(!r.v.prefs.view.show_extensions);
    assert!(r.c.has_text("alpha") && r.c.has_text("beta"), "{:?}", r.c.texts());
    assert!(!r.c.has_text("alpha.txt"));
    assert!(r.c.has_text("Docs"), "folders unchanged");
    // the same in the grid
    r.v.set_mode(ViewMode::GridSmall);
    r.frame();
    assert!(r.c.has_text("alpha") && !r.c.has_text("alpha.txt"), "{:?}", r.c.texts());
    // the entry's real name is what sorting, rename and open use
    let id = r.v.tab().store.at(1).unwrap();
    assert_eq!(String::from_utf16_lossy(r.v.tab().store.name(id).unwrap()), "alpha.txt");
    let mut cfg = tsubame_config::Config::default();
    crate::session::sync_config(&r.v, &mut cfg);
    assert!(!cfg.view.show_extensions);
    r.v.run_command(&mut r.ui, CMD_VIEW_EXTENSIONS);
    r.frame();
    assert!(r.c.has_text("alpha.txt"));
}

#[test]
fn fitting_sizes_columns_to_their_text_inside_the_limits() {
    let mut r = Rig::new("fit");
    let t = r.ui.theme;
    let before = r.v.cols;
    r.v.fit_columns(&mut r.c, &t, &[Col::Modified, Col::Type, Col::Size]);
    let want = |s: &str, r: &mut Rig| r.c.measure(s, t.font());
    let type_w = want("Text document", &mut r) + 14.0;
    assert!(r.v.cols.w[1] >= type_w - 0.01, "Type fits its widest text: {} vs {type_w}", r.v.cols.w[1]);
    assert_ne!(r.v.cols, before, "the widths changed");
    for w in &r.v.cols.w[..3] {
        assert!(*w >= crate::cols::MIN_W && *w <= FIT_CAP, "{w}");
    }
    assert_eq!(r.v.cols.w[3], before.w[3], "Git was not asked");
}

#[test]
fn the_header_menu_switches_columns_and_fits_them() {
    let mut r = Rig::new("colmenu");
    let t = r.ui.theme;
    let y = t.height_titlebar + t.height_toolbar + t.height_column_header / 2.0;
    let at = Pt::new(700.0, y);
    r.ui.push(Event::MouseMove { pos: at });
    r.frame();
    r.ui.push(Event::MouseDown { pos: at, button: Button::Right, mods: Mods::NONE, double: false });
    r.frame();
    r.ui.push(Event::MouseUp { pos: at, button: Button::Right, mods: Mods::NONE });
    r.frame();
    assert!(r.c.has_text("Size all columns to fit"), "the menu is open: {:?}", r.c.texts());
    assert!(r.c.has_text("Tags") && r.c.has_text("Git"), "a row per column");
    // rows: Name, Date modified, Type, Size, Git, Tags, then the fit row (one separator before it)
    let n = crate::cols::BUILTIN.len();
    let h = crate::overlay::menu_height(&(0..=n).map(|i| if i == n { crate::overlay::MenuItem::new("x", "").sep() } else { crate::overlay::MenuItem::new("x", "") }).collect::<Vec<_>>());
    let rect = crate::overlay::menu_rect(at, (1120.0, 720.0), h);
    let row_y = |i: usize| rect.y + 4.0 + i as f32 * 26.0 + 13.0;
    // Type is the third built-in column: unchecking it removes it from the header
    assert!(r.v.colset.order().contains(&Col::Type));
    r.click(rect.x + 30.0, row_y(2));
    assert!(!r.v.colset.order().contains(&Col::Type), "Type is off, like the Settings switch");
    assert_eq!(r.v.prefs.extensions.disabled, ["tsubame.column.type"]);
    assert!(!r.c.has_text("Size all columns to fit"), "the menu closed");
    // the menu again: Type back on
    r.ui.push(Event::MouseMove { pos: at });
    r.frame();
    r.ui.push(Event::MouseDown { pos: at, button: Button::Right, mods: Mods::NONE, double: false });
    r.frame();
    r.ui.push(Event::MouseUp { pos: at, button: Button::Right, mods: Mods::NONE });
    r.frame();
    r.click(rect.x + 30.0, row_y(2));
    assert!(r.v.colset.order().contains(&Col::Type));
    // "Size all columns to fit"
    let before = r.v.cols;
    r.ui.push(Event::MouseMove { pos: at });
    r.frame();
    r.ui.push(Event::MouseDown { pos: at, button: Button::Right, mods: Mods::NONE, double: false });
    r.frame();
    r.ui.push(Event::MouseUp { pos: at, button: Button::Right, mods: Mods::NONE });
    r.frame();
    r.click(rect.x + 30.0, row_y(n));
    assert_ne!(r.v.cols, before, "the columns were fitted");
    assert_eq!(r.c.clip_depth(), 0);
}
