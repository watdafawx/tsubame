//! The "Sort by >" and "Group by >" submenus of the empty-space context menu, and the
//! header menu's "Group by" rows: what they show (`ViewMenu`, a snapshot of the active tab),
//! their action ids, and the pick they decode to. `View::apply_view_pick` runs a pick.

use tsubame_core::Column;
use tsubame_core::grouping::GroupBy;
use tsubame_win::shell::ctxmodel::{ItemKind, ItemSource, MenuItem, flag};

/// The submenus' own rows; the picks are `x.group:<mode>`, `x.sort:<column>`, `x.sort-asc|desc`.
pub const ACT_GROUP: &str = "x.group";
pub const ACT_SORT: &str = "x.sort";
const GROUP_PICK: &str = "x.group:";
const SORT_PICK: &str = "x.sort:";
const ASC: &str = "x.sort-asc";
const DESC: &str = "x.sort-desc";

/// What the active tab shows, for the menus.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewMenu {
    /// the tab can group (not Tree, Miller or Settings)
    pub can_group: bool,
    pub group: GroupBy,
    pub sort: Column,
    pub descending: bool,
    /// the sortable columns that are on, in column order, with their header labels
    pub columns: Vec<(Column, String)>,
}

/// A decoded pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Group(GroupBy),
    Sort(Column),
    Direction { descending: bool },
}

fn column_name(c: Column) -> &'static str {
    match c {
        Column::Name => "name",
        Column::Size => "size",
        Column::Modified => "date",
        Column::Type => "type",
    }
}

fn parse_column(s: &str) -> Option<Column> {
    [Column::Name, Column::Size, Column::Modified, Column::Type].into_iter().find(|&c| column_name(c) == s)
}

/// The pick an action id stands for (`None`: some other action, or a submenu row).
pub fn parse(act: &str) -> Option<Pick> {
    if let Some(m) = act.strip_prefix(GROUP_PICK) {
        return GroupBy::parse(m).map(Pick::Group);
    }
    if let Some(c) = act.strip_prefix(SORT_PICK) {
        return parse_column(c).map(Pick::Sort);
    }
    match act {
        ASC => Some(Pick::Direction { descending: false }),
        DESC => Some(Pick::Direction { descending: true }),
        _ => None,
    }
}

fn row(label: &str, act: &str, checked: bool) -> MenuItem {
    let mut it = MenuItem { key: format!("app:{act}"), ..MenuItem::command(ItemSource::Tsubame, label, act) };
    if checked {
        it.flags |= flag::CHECKED | flag::RADIO;
    }
    it
}

fn submenu(label: &str, act: &str, kids: Vec<MenuItem>) -> MenuItem {
    MenuItem { kind: ItemKind::Submenu, children: kids, key: format!("app:{act}"), ..MenuItem::command(ItemSource::Tsubame, label, act) }
}

/// The group modes as rows (radio checks on the current one).
pub fn group_rows(current: GroupBy) -> Vec<MenuItem> {
    GroupBy::ALL.iter().map(|&g| row(g.label(), &format!("{GROUP_PICK}{}", g.name()), g == current)).collect()
}

/// "Group by >".
pub fn group_submenu(v: &ViewMenu) -> MenuItem {
    submenu("Group by", ACT_GROUP, group_rows(v.group))
}

/// "Sort by >": the columns that are on, then the direction.
pub fn sort_submenu(v: &ViewMenu) -> MenuItem {
    let mut kids: Vec<MenuItem> = v.columns.iter().map(|(c, label)| row(label, &format!("{SORT_PICK}{}", column_name(*c)), *c == v.sort)).collect();
    kids.push(MenuItem::separator(ItemSource::Tsubame));
    kids.push(row("Ascending", ASC, !v.descending));
    kids.push(row("Descending", DESC, v.descending));
    submenu("Sort by", ACT_SORT, kids)
}

/// The rows the empty-space menu adds ("Group by" only for a tab that can group).
pub fn background_rows(v: &ViewMenu) -> Vec<MenuItem> {
    let mut rows = vec![sort_submenu(v)];
    if v.can_group {
        rows.push(group_submenu(v));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> ViewMenu {
        ViewMenu { can_group: true, group: GroupBy::Type, sort: Column::Modified, descending: true, columns: vec![(Column::Name, "Name".into()), (Column::Modified, "Date modified".into()), (Column::Size, "Size".into())] }
    }

    fn checked(kids: &[MenuItem]) -> Vec<&str> {
        kids.iter().filter(|k| k.flags & flag::CHECKED != 0).map(|k| k.label.as_str()).collect()
    }

    #[test]
    fn group_by_checks_the_current_mode() {
        let g = group_submenu(&menu());
        assert_eq!(g.kind, ItemKind::Submenu);
        let labels: Vec<&str> = g.children.iter().map(|k| k.label.as_str()).collect();
        assert_eq!(labels, ["(None)", "Name", "Type", "Date modified", "Size"]);
        assert_eq!(checked(&g.children), ["Type"]);
        assert!(g.children.iter().all(|k| k.flags & flag::RADIO != 0 || k.flags == 0));
    }

    #[test]
    fn sort_by_lists_the_visible_columns_then_the_direction() {
        let s = sort_submenu(&menu());
        let labels: Vec<&str> = s.children.iter().map(|k| k.label.as_str()).collect();
        assert_eq!(labels, ["Name", "Date modified", "Size", "", "Ascending", "Descending"]);
        assert!(s.children[3].is_separator());
        assert_eq!(checked(&s.children), ["Date modified", "Descending"]);
    }

    #[test]
    fn a_tab_that_cannot_group_gets_no_group_row() {
        let mut v = menu();
        assert_eq!(background_rows(&v).len(), 2);
        v.can_group = false;
        let rows = background_rows(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, ACT_SORT);
    }

    #[test]
    fn every_row_decodes_and_keeps_a_unique_key() {
        let v = menu();
        let mut keys = Vec::new();
        for sub in background_rows(&v) {
            assert_eq!(parse(&sub.action), None, "the submenu itself picks nothing");
            for k in sub.children.iter().filter(|k| !k.is_separator()) {
                assert!(parse(&k.action).is_some(), "{}", k.action);
                keys.push(k.key.clone());
            }
        }
        let n = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n);
        assert_eq!(parse("x.group:date"), Some(Pick::Group(GroupBy::Date)));
        assert_eq!(parse("x.group:none"), Some(Pick::Group(GroupBy::None)));
        assert_eq!(parse("x.sort:size"), Some(Pick::Sort(Column::Size)));
        assert_eq!(parse("x.sort-desc"), Some(Pick::Direction { descending: true }));
        assert_eq!(parse("x.group:bogus"), None);
        assert_eq!(parse("file.paste"), None);
    }
}
