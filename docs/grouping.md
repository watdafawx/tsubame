# Group by (B11-F)

Details and the thumbnail views can group the files of a folder under headers.
The mode belongs to the tab (it is saved in the session); new tabs start in
`[view] group_by` (`docs/config.md`, default `none`). Tree and Miller views and
the Settings tab ignore grouping, and so does a listing that is still streaming
in (the groups appear when it is complete).

## Modes

| Mode | Groups | Order |
|---|---|---|
| None | no headers | the sort order |
| Name | first letter `A`-`Z` (case folded), `#` for digits, punctuation and non-Latin names | `#` first, then A-Z |
| Type | the extension as the Type column writes it (`PNG File`), `Folder`, `File` (no extension) | `Folder`, then by extension, then `File` |
| Date modified | Today, Yesterday, Earlier this week, Last week, Earlier this month, Last month, Earlier this year, A long time ago | newest first |
| Size | Empty (0), Tiny (< 16 KB), Small (< 1 MB), Medium (< 128 MB), Large (< 1 GB), Huge | smallest first |

Descending sort reverses the order of the groups. Inside a group the files keep
the tab's sort (Name, Date, Type or Size, ascending or descending). Folders
sort inside their group like any other entry (Type puts them in `Folder`).

Date groups use local calendar days: `DateBoundaries` takes the UTC instants of
the local midnights of today, yesterday, the start of the week (Monday), last
week, this month, last month and this year. They are built from the local time
zone's offset at each of those instants (`tsubame_win::tz`), so a 23 or 25 hour
day around a DST change is one day and a file is never put in the wrong day by
an hour.

## Using it

- Right click on empty space: **Sort by >** (Name, the other visible columns,
  Ascending, Descending, the current ones ticked) and **Group by >** (None, Name,
  Type, Date modified, Size).
- Right click on the column header: **Group by** rows below the column list.
- Palette (`Ctrl+Shift+P`, no default keys): `view.group-by` (next mode),
  `view.group-none`, `view.group-name`, `view.group-type`, `view.group-date`,
  `view.group-size`, `view.group-toggle` (collapse or expand the cursor's
  group), `view.group-collapse-all`, `view.group-expand-all`.
- A header shows the name, the count, a chevron and a 1 px rule. Click it (or
  Enter / Space with the keyboard on it) to collapse or expand the group;
  Ctrl+click makes every group follow the clicked one.
- Up / Down / PageUp / PageDown / Home / End move over the files and skip the
  headers. Left on a file puts the keyboard on its group's header; on a header,
  Left collapses and Right expands the group, Enter and Space toggle it.
- A collapsed group's files are hidden: the marquee, Invert, type-ahead and a
  Shift+click range only see what is on screen. Ctrl+A selects every file,
  collapsed groups included.

## How it is built

- `tsubame-core::grouping`: `GroupBy`, the pure key functions (`name_key`,
  `type_key`, size and date buckets) and `DateBoundaries`. Everything takes
  `now` and the zone offset as arguments, so the tests pin DST days and month and
  year edges.
- `tsubame-ui::widgets::grouping`: `Grouping<K>` (cached `Header(group)` /
  `Item(source_index)` rows, constant time row <-> item, collapse by key kept
  across rebuilds) and the `GroupRows` trait the list and grid draw from.
  `list` and `grid` paint headers, hit test, navigate and band-select through it.
- `src/view_group.rs`: the per-tab `GroupState` (mode, grouping, labels). It is
  rebuilt once per store revision, mode change or sort direction change and
  never while painting rows.
- `src/view_groupcmd.rs`, `src/viewmenu.rs`: the commands and the menu rows.

Everything the app keeps (cursor, hover, selection, drag, activation, session)
is an **item**: a position in `store.view()`. Only the list and the grid work in
flat rows (headers plus the shown items); `Tab::{list_row_of, item_of_row,
item_at_top_row, set_list_top, move_cursor_to, cell_origin_of, row_top_of,
item_at_point}` are the only places that translate. A saved scroll position is
the entry at the top of the list, not a row number, so it survives a change of
group mode.

## Cost

Rebuilding is one pass over the listing (key per entry, a `BTreeMap` of the
groups): 100,000 entries group in a few milliseconds and only when the
listing, mode or sort direction changes. Scrolling reads the cached rows and
allocates nothing per row (`GroupState` keeps the header texts). With grouping
off the list and grid take the same code path as before.

The real 4K scroll numbers per `docs/bench.md` (`--bench --group type|date`) are
in `docs/bench-log.md`.

## UIA

Group headers are exposed as UIA groups (`docs/uia-views.md`).
