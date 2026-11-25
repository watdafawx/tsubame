//! Whole-frame tests of the UIA view snapshot (`docs/uia-views.md`): what the
//! builders in `uia_rows` publish for Details, Tree and Miller on the kit's
//! recording canvas, and what the requests of a screen reader do.

use super::*;
use crate::uia_rows::{self, PART_LIST, PART_MILLER};
use tsubame_win::uia::{Expand as UExpand, VAct, VReq, VSurface};

fn surface(key: u32) -> Option<VSurface> {
    uia_rows::last().surfaces.into_iter().find(|s| s.key == key)
}

fn item_named<'a>(s: &'a VSurface, name: &str) -> &'a tsubame_win::uia::VItem {
    s.items.iter().find(|i| i.name.starts_with(name)).unwrap_or_else(|| panic!("{name} missing: {:?}", s.items.iter().map(|i| &i.name).collect::<Vec<_>>()))
}

#[test]
fn nothing_is_built_while_no_client_listens() {
    uia_rows::force(false);
    let r = Rig::new("uia-off");
    assert!(uia_rows::last().surfaces.is_empty());
    assert!(!uia_rows::on());
    drop(r);
}

#[test]
fn details_rows_have_names_values_and_a_total() {
    uia_rows::force(true);
    let r = Rig::new("uia-details");
    let s = surface(uia_rows::skey(0, PART_LIST)).expect("the list surface");
    assert!(!s.tree);
    assert_eq!(s.size, 3, "total rows, not only the visible ones");
    assert_eq!(s.items.len(), 3);
    let a = item_named(&s, "alpha.txt");
    assert!(!a.value.is_empty(), "type, size and date are the row's value");
    assert!(a.invokable && a.selectable);
    assert_eq!(a.level, 0, "not a tree row");
    assert!(a.rect[2] > 0.0 && a.rect[3] > 0.0);
    item_named(&s, "Docs");
    uia_rows::force(false);
    drop(r);
}

#[test]
fn select_and_invoke_requests_act_like_the_keyboard() {
    uia_rows::force(true);
    let mut r = Rig::new("uia-req");
    let s = surface(uia_rows::skey(0, PART_LIST)).unwrap();
    let beta = item_named(&s, "beta.txt").key;
    let docs = item_named(&s, "Docs").key;
    r.v.run_view_request(VReq { surf: s.key, key: beta, act: VAct::Select });
    r.frame();
    let s = surface(s.key).unwrap();
    assert!(item_named(&s, "beta.txt").selected);
    assert!(!item_named(&s, "alpha.txt").selected);
    // a row that has gone is dropped without a panic
    r.v.run_view_request(VReq { surf: s.key, key: 9_999_999, act: VAct::Invoke });
    // Invoke on a folder enters it
    let start = r.v.tab().path.clone();
    r.v.run_view_request(VReq { surf: s.key, key: docs, act: VAct::Invoke });
    tree_wait(&mut r, "the folder opens", |r| r.v.tab().path != start);
    assert!(r.v.tab().path.ends_with("Docs"));
    uia_rows::force(false);
}

#[test]
fn tree_rows_carry_level_position_and_expand_state() {
    uia_rows::force(true);
    let mut r = Rig::new("uia-tree");
    r.key(Key::Char('4'), Mods::CTRL_SHIFT);
    tree_wait(&mut r, "tree lists", |_| uia_rows::last().surfaces.iter().any(|s| s.tree && s.items.len() >= 3));
    let s = uia_rows::last().surfaces.into_iter().find(|s| s.tree).unwrap();
    let docs = item_named(&s, "Docs");
    assert!(docs.level >= 1);
    assert_eq!(docs.expand, UExpand::Collapsed);
    let alpha = item_named(&s, "alpha.txt");
    assert_eq!(alpha.expand, UExpand::Leaf);
    assert!(alpha.pos >= 1 && alpha.pos <= s.size);
    // Expand request opens the folder
    let key = docs.key;
    r.v.run_view_request(VReq { surf: s.key, key, act: VAct::Expand });
    tree_wait(&mut r, "docs expanded", |_| {
        uia_rows::last().surfaces.iter().find(|s| s.tree).is_some_and(|s| s.items.iter().any(|i| i.key == key && i.expand == UExpand::Expanded))
    });
    r.v.run_view_request(VReq { surf: s.key, key, act: VAct::Collapse });
    tree_wait(&mut r, "docs collapsed", |_| {
        uia_rows::last().surfaces.iter().find(|s| s.tree).is_some_and(|s| s.items.iter().any(|i| i.key == key && i.expand == UExpand::Collapsed))
    });
    uia_rows::force(false);
}

#[test]
fn miller_columns_are_lists_with_the_selected_row() {
    uia_rows::force(true);
    let mut r = Rig::new("uia-miller");
    std::fs::write(r.dir.join("Docs").join("inner.txt"), b"x").unwrap();
    r.key(Key::Char('5'), Mods::CTRL_SHIFT);
    // the temp folder is deep, so the columns' indices are not 0 and 1: find them by their rows
    let has = |what: &'static str| move |_: &Rig| uia_rows::last().surfaces.iter().any(|s| s.key >= uia_rows::skey(0, PART_MILLER) && s.items.iter().any(|i| i.name.starts_with(what)));
    tree_wait(&mut r, "alpha column", has("alpha.txt"));
    tree_wait(&mut r, "inner column", has("inner.txt"));
    let l = uia_rows::last();
    let cols: Vec<&VSurface> = l.surfaces.iter().filter(|s| s.key >= uia_rows::skey(0, PART_MILLER)).collect();
    let parent = cols.iter().find(|s| s.items.iter().any(|i| i.name.starts_with("alpha.txt"))).unwrap();
    let child = cols.iter().find(|s| s.items.iter().any(|i| i.name.starts_with("inner.txt"))).unwrap();
    assert!(!parent.name.is_empty() && !child.name.is_empty(), "a column is named by its folder");
    assert_eq!(child.key, parent.key + 1, "columns are in path order");
    assert!(item_named(parent, "Docs").selected, "the path runs through the selected row");
    assert!(cols.iter().all(|c| !c.tree));
    uia_rows::force(false);
}
