//! Whole-frame tests of Group by on the recording canvas (`docs/grouping.md`): what a click,
//! a key or a command does when the rows on screen are not the entries' positions.
//!
//! The rig folder is `Docs\`, `alpha.txt`, `beta.txt`. Grouped by Type it shows
//! `[Folder] Docs [TXT File] alpha beta`: 5 flat rows for 3 entries (positions 0, 1, 2).

use super::*;
use crate::commands::{CMD_GROUP_COLLAPSE_ALL, CMD_GROUP_EXPAND_ALL, CMD_GROUP_NAME, CMD_GROUP_NONE, CMD_GROUP_TOGGLE, CMD_GROUP_TYPE, CMD_VIEW_GROUP_BY};
use tsubame_core::grouping::GroupBy;

impl Rig {
    fn grouped(tag: &str, by: GroupBy) -> Rig {
        let mut r = Rig::new(tag);
        r.v.set_group_by(by);
        r.frame();
        r
    }
    /// Click the middle of flat row `row` of the list.
    fn click_row(&mut self, row: usize) {
        let (l, rh) = self.list_of(0);
        let y = l.y + (row as f32 + 0.5) * rh - self.v.tab().list.scroll as f32;
        self.click(l.x + 120.0, y);
    }
    fn names_of_selection(&self) -> Vec<String> {
        let t = self.v.tab();
        (0..t.shown()).filter(|&i| t.store.at(i).is_some_and(|id| t.sel.contains(id))).filter_map(|i| t.entry_at(i).map(|(p, _)| p.rsplit('\\').next().unwrap_or("").to_string())).collect()
    }
}

#[test]
fn the_list_has_headers_between_the_entries() {
    let r = Rig::grouped("grp-rows", GroupBy::Type);
    let t = r.v.tab();
    assert!(t.group.on());
    let g = t.group.rows().unwrap();
    assert_eq!(g.row_count(), 5);
    assert_eq!(t.list_rows(), 5);
    assert_eq!(t.shown(), 3, "everything else counts entries");
    assert_eq!(t.group.label(0), "Folder");
    assert_eq!(t.group.label(1), "TXT File");
    assert!(r.c.has_text("Folder") && r.c.has_text("TXT File"), "{:?}", r.c.texts());
}

#[test]
fn a_click_on_a_row_selects_that_entry_not_the_entry_at_that_row_number() {
    let mut r = Rig::grouped("grp-click", GroupBy::Type);
    // flat row 3 is `alpha.txt` (entry 1); row 3 as an entry number would be out of range, row 2 `beta`
    r.click_row(3);
    assert_eq!(r.names_of_selection(), ["alpha.txt"]);
    assert_eq!(r.v.tab().list.cursor, 1, "the cursor is an entry, not a row");
    r.click_row(4);
    assert_eq!(r.names_of_selection(), ["beta.txt"]);
    // the header row selects nothing
    r.click_row(2);
    assert_eq!(r.names_of_selection(), ["beta.txt"], "a click on a header only toggles it");
}

#[test]
fn hit_testing_finds_entries_and_not_headers() {
    let r = Rig::grouped("grp-hit", GroupBy::Type);
    let t = r.v.tab();
    let rh = t.list.row_h;
    let at = |row: f32| t.item_at_point(false, 10.0, (row + 0.5) * rh);
    assert_eq!(at(0.0), None, "header Folder");
    assert_eq!(at(1.0), Some(0), "Docs");
    assert_eq!(at(2.0), None, "header TXT File");
    assert_eq!(at(3.0), Some(1));
    assert_eq!(at(4.0), Some(2));
    assert_eq!(at(5.0), None, "below the last row");
    // and the way back: where is an entry drawn
    assert_eq!(t.row_top_of(0), f64::from(rh));
    assert_eq!(t.row_top_of(1), 3.0 * f64::from(rh));
    assert_eq!(t.list_row_of(2), 4);
    assert_eq!(t.item_of_row(0), None);
}

#[test]
fn enter_opens_the_entry_under_the_cursor_and_arrows_skip_headers() {
    let mut r = Rig::grouped("grp-enter", GroupBy::Type);
    let docs = r.dir.join("Docs").to_str().unwrap().to_string();
    r.key(Key::Home, Mods::NONE);
    assert_eq!(r.v.tab().list.cursor, 0, "Home lands on the first entry, not on a header");
    r.key(Key::Down, Mods::NONE);
    assert_eq!(r.v.tab().list.cursor, 1, "Down steps over the TXT File header");
    r.key(Key::Down, Mods::NONE);
    assert_eq!(r.v.tab().list.cursor, 2);
    r.key(Key::Down, Mods::NONE);
    assert_eq!(r.v.tab().list.cursor, 2, "the end");
    r.key(Key::Up, Mods::NONE);
    r.key(Key::Up, Mods::NONE);
    assert_eq!(r.v.tab().list.cursor, 0);
    r.key(Key::Enter, Mods::NONE);
    r.settle();
    assert_eq!(r.v.tab().path, docs, "Enter opened the folder");
}

#[test]
fn collapsing_hides_a_groups_entries_from_navigation_and_back() {
    let mut r = Rig::grouped("grp-collapse", GroupBy::Type);
    r.key(Key::Home, Mods::NONE);
    // Down, Down: on beta; Left goes to the header of its group, Left again collapses it
    r.key(Key::Down, Mods::NONE);
    r.key(Key::Left, Mods::NONE);
    assert_eq!(r.v.tab().list.header_focus(), Some(1), "the keyboard is on the TXT File header");
    r.key(Key::Left, Mods::NONE);
    {
        let t = r.v.tab();
        let g = t.group.rows().unwrap();
        assert!(g.is_collapsed(1));
        assert_eq!(g.row_count(), 3, "two headers and Docs");
        assert_eq!(g.shown_len(1), 0);
    }
    // the entries are still there, and Ctrl+A takes them along
    assert_eq!(r.v.tab().shown(), 3);
    r.key(Key::Char('A'), Mods::CTRL);
    assert_eq!(r.names_of_selection().len(), 3, "Ctrl+A selects collapsed entries too");
    // Invert only flips what is on screen
    r.key(Key::Char('A'), Mods::CTRL_SHIFT);
    assert_eq!(r.names_of_selection(), ["alpha.txt", "beta.txt"], "Docs was flipped off, the hidden two kept");
    r.key(Key::Right, Mods::NONE);
    assert!(!r.v.tab().group.rows().unwrap().is_collapsed(1), "Right expands it again");
    assert_eq!(r.v.tab().group.rows().unwrap().row_count(), 5);
}

#[test]
fn a_header_click_toggles_and_ctrl_click_takes_every_group() {
    let mut r = Rig::grouped("grp-header", GroupBy::Type);
    r.click_row(2);
    assert!(r.v.tab().group.rows().unwrap().is_collapsed(1));
    assert!(!r.v.tab().group.rows().unwrap().is_collapsed(0));
    r.click_row(2);
    assert!(!r.v.tab().group.rows().unwrap().is_collapsed(1));
    // Ctrl+click on a header: every group follows
    let (l, rh) = r.list_of(0);
    let pos = Pt::new(l.x + 120.0, l.y + 0.5 * rh);
    r.ui.push(Event::MouseMove { pos });
    r.frame();
    r.ui.push(Event::MouseDown { pos, button: Button::Left, mods: Mods::CTRL, double: false });
    r.frame();
    r.ui.push(Event::MouseUp { pos, button: Button::Left, mods: Mods::CTRL });
    r.frame();
    let g = r.v.tab().group.rows().unwrap();
    assert!(g.is_collapsed(0) && g.is_collapsed(1));
    assert_eq!(g.row_count(), 2);
}

#[test]
fn the_commands_change_the_mode_collapse_and_expand() {
    let mut r = Rig::new("grp-cmds");
    assert_eq!(r.v.tab().group.by, GroupBy::None);
    assert!(!r.v.tab().group.on());
    r.v.run_command(&mut r.ui, CMD_VIEW_GROUP_BY);
    r.frame();
    assert_eq!(r.v.tab().group.by, GroupBy::Name, "the cycle starts at Name");
    assert_eq!(r.v.tab().group.label(0), "A");
    r.v.run_command(&mut r.ui, CMD_GROUP_TYPE);
    r.frame();
    assert_eq!(r.v.tab().group.by, GroupBy::Type);
    r.v.run_command(&mut r.ui, CMD_GROUP_COLLAPSE_ALL);
    r.frame();
    assert_eq!(r.v.tab().group.rows().unwrap().row_count(), 2);
    r.v.run_command(&mut r.ui, CMD_GROUP_EXPAND_ALL);
    r.frame();
    assert_eq!(r.v.tab().group.rows().unwrap().row_count(), 5);
    // the cursor's group
    r.v.tab_mut().list.cursor = 1;
    r.v.run_command(&mut r.ui, CMD_GROUP_TOGGLE);
    r.frame();
    assert!(r.v.tab().group.rows().unwrap().is_collapsed(1));
    // a new mode starts from expanded groups
    r.v.run_command(&mut r.ui, CMD_GROUP_NAME);
    r.frame();
    assert_eq!(r.v.tab().group.rows().unwrap().row_count(), r.v.tab().group.rows().unwrap().group_count() + 3);
    r.v.run_command(&mut r.ui, CMD_GROUP_NONE);
    r.frame();
    assert!(!r.v.tab().group.on());
    assert_eq!(r.v.tab().list_rows(), 3, "plain rows again");
}

#[test]
fn tree_and_miller_ignore_grouping_and_it_comes_back() {
    let mut r = Rig::grouped("grp-modes", GroupBy::Type);
    assert!(r.v.tab().group.on());
    r.key(Key::Char('4'), Mods::CTRL_SHIFT);
    tree_wait(&mut r, "tree rows on", |r| r.v.tab().tree_on);
    assert!(!r.v.tab().group.on(), "Tree shows plain rows");
    r.key(Key::Char('1'), Mods::CTRL_SHIFT);
    tree_wait(&mut r, "tree rows off", |r| !r.v.tab().tree_on);
    assert!(r.v.tab().group.on());
    assert_eq!(r.v.tab().group.by, GroupBy::Type, "the mode was kept");
}

#[test]
fn the_grid_groups_too_and_a_click_selects_the_right_cell() {
    let mut r = Rig::grouped("grp-grid", GroupBy::Type);
    r.key(Key::Char('2'), Mods::CTRL_SHIFT);
    r.frame();
    let t = r.v.tab();
    assert!(t.group.on());
    assert!(t.grid.geo_active(), "the grid laid its cells out in groups");
    // alpha.txt (entry 1) is drawn in the second group; click its cell
    let (ox, oy) = t.cell_origin_of(1);
    let (cw, ch) = (t.grid.cell_w, t.grid.cell_h);
    let (x, y) = (r.v.list_r.x + ox + cw / 2.0, r.v.list_r.y + oy + ch / 2.0);
    r.click(x, y);
    assert_eq!(r.names_of_selection(), ["alpha.txt"]);
    assert_eq!(r.v.tab().grid.cursor, 1);
}
