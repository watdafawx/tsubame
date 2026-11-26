//! The group commands (`view.group-*`) and the picks of the "Group by" and "Sort by"
//! menus, on the view side. The model is `crate::view_group`; `docs/grouping.md`.

use super::*;
use tsubame_core::grouping::GroupBy;

impl View {
    /// `view.group-*` commands. False for any other command.
    pub(super) fn run_group(&mut self, cmd: u32) -> bool {
        let by = match cmd {
            CMD_VIEW_GROUP_BY => self.tab().group.by.next(),
            CMD_GROUP_NONE => GroupBy::None,
            CMD_GROUP_NAME => GroupBy::Name,
            CMD_GROUP_TYPE => GroupBy::Type,
            CMD_GROUP_DATE => GroupBy::Date,
            CMD_GROUP_SIZE => GroupBy::Size,
            CMD_GROUP_TOGGLE | CMD_GROUP_COLLAPSE_ALL | CMD_GROUP_EXPAND_ALL => {
                self.group_collapse(cmd);
                return true;
            }
            _ => return false,
        };
        self.set_group_by(by);
        true
    }

    /// Groups the active tab by `by` (a no-op when it already is).
    pub(crate) fn set_group_by(&mut self, by: GroupBy) {
        let grid = self.mode.is_grid();
        self.tab_mut().set_group_by(by, grid);
    }

    /// Collapse / expand: the group of the cursor, or every group.
    fn group_collapse(&mut self, cmd: u32) {
        let grid = self.mode.is_grid();
        let t = self.tab_mut();
        t.sync_group();
        if !t.group.on() {
            return;
        }
        let cur = if grid { t.grid.cursor } else { t.list.cursor };
        let g = t.group.group_of(cur).unwrap_or(0);
        let changed = match cmd {
            CMD_GROUP_TOGGLE => {
                let to = t.group.grouping().groups().get(g).is_some_and(|x| !x.collapsed);
                t.group.grouping_mut().set_collapsed(g, to, false)
            }
            CMD_GROUP_COLLAPSE_ALL => t.group.grouping_mut().set_collapsed(0, true, true),
            _ => t.group.grouping_mut().set_collapsed(0, false, true),
        };
        if changed {
            // a collapsed group takes the cursor with it: it rests on the header
            if t.group.rows().is_some_and(|r| r.item_row(cur).is_none()) {
                t.list.set_header_focus(Some(g));
                t.grid.set_header_focus(Some(g));
            }
        }
    }

    /// What the empty-space menu shows of the active tab (`None` for the Settings tab).
    pub(crate) fn view_menu(&self) -> Option<crate::viewmenu::ViewMenu> {
        let t = self.tab();
        if t.settings {
            return None;
        }
        let columns = self
            .colset
            .order()
            .iter()
            .filter_map(|&c| c.sort_column().map(|sc| (sc, self.colset_label(c, ""))))
            .collect();
        Some(crate::viewmenu::ViewMenu { can_group: !(t.tree_on || t.miller_on), group: t.group.by, sort: t.sort.column, descending: t.sort.descending, columns })
    }

    /// A pick of the "Sort by" / "Group by" menus.
    pub(crate) fn apply_view_pick(&mut self, pick: crate::viewmenu::Pick) {
        use crate::viewmenu::Pick;
        match pick {
            Pick::Group(by) => self.set_group_by(by),
            Pick::Sort(col) => self.sort_to(col),
            Pick::Direction { descending } => self.sort_direction(descending),
        }
    }

    /// Sorts the active tab by `col`; a column already sorted by keeps its direction.
    pub(crate) fn sort_to(&mut self, col: tsubame_core::Column) {
        let t = self.tab_mut();
        if t.sort.column != col {
            t.sort_by(col);
            // the folder remembers it like a header click does
            self.remember_folder_view();
        }
    }

    /// Sets the direction of the current sort.
    pub(crate) fn sort_direction(&mut self, descending: bool) {
        let t = self.tab_mut();
        if t.sort.descending != descending {
            let col = t.sort.column;
            t.sort_by(col);
            self.remember_folder_view();
        }
    }
}
