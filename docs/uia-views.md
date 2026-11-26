# UI Automation for the file views

Screen readers (Narrator, NVDA, JAWS) read Tsubame's own-drawn views through UI
Automation. CM10 (`docs/ctxmenu.md`) did this for the context menu; this page
covers the file views: file rows, Tree, Miller columns, and the tags picker.

## What is exposed

| Surface | Control type | Rows | Patterns |
|---|---|---|---|
| Details and Grid file list | List | ListItem: name (+ tags), value text, item type | SelectionItem, Invoke, Value |
| Tree rows (Details in Tree mode) | Tree | TreeItem: level, position in set, size of set | SelectionItem, ExpandCollapse, Invoke, Value |
| Folders-only Tree pane | Tree | TreeItem, same as above | SelectionItem, ExpandCollapse, Invoke |
| Miller column `i` | List (one per column) | ListItem: name, value, selected | SelectionItem, Invoke, Value |
| Path bar chevrons (`PART_CRUMBS`) | List (`"Path bar"`) | ListItem: `"Subfolders of <name>"`, path | Invoke, ExpandCollapse, Value |
| Operations panel or its pill (`PART_OPS`, slot 0) | List (`"Operations"`, `"N running"`) | ListItem `"Button"`: `"Pause: Copying 14 items, 43%"`, `"Cancel: ..."`, `"Undo: ..."`, `"Retry: ..."`, `"Show operations"` | Invoke |
| Conflict dialog (`PART_CONFLICT`, slot 0) | List (the dialog title) | ListItem `"Button"`: Cancel operation, Skip, Keep both, Replace (or "... all"), the "all" check box | Invoke |
| Tags picker & navigation menus | menu-style rows (CM10 snapshot) | row name, toggled state (`"Back history"`, `"Forward history"`, `"Subfolders of <name>"`) | Invoke (and Toggle) |

- **Name** of a file row is the file name, plus `, tags: A, B` when it has tags
  (`tags::model::a11y_name`). **Value** is `type, size, modified`, plus the age
  bucket (`age::bucket_name`) when file-age colouring is on.
- **Selection:** the List supports `ISelectionProvider` (multi for Details and
  Grid, single for Miller columns); a selected row reports `IsSelected`.
- **Tree rows are a flat list** of TreeItems carrying `Level` (depth + 1),
  `PositionInSet` and `SizeOfSet`, as Windows' own lists do. UIA clients build
  the nesting from the level; this keeps navigation (`Next/PreviousSibling`)
  cheap and avoids one provider per ancestor.
- **ExpandCollapse** state is `Collapsed` or `Expanded` for folders and `LeafNode`
  for files and message rows (loading, empty, denied).
- **Invoke** does what Enter does on that row: open a file, enter a folder (in
  Details and Miller), or toggle a Tree folder.
- **Focus:** `GetFocus` is the open menu's row if a menu is open, else the view's
  cursor row. A focus event is raised when the cursor moves.

## How it stays cheap

- Providers are built only while a client listens (`uia::active()`, i.e.
  `UiaClientsAreListening`). The paint hooks check `uia_rows::on()` first: with
  no listener the whole path is one boolean per frame, no allocation, no wake-up.
- The hooks run after the list or grid widget has been drawn and reuse what the
  frame already read: `ListResponse.range` (the visible rows), the row buffer the
  painter filled, the scroll offset and the row height. **No entry store reads
  beyond the visible rows.**
- A surface publishes the visible items plus the total (`size`); a screen
  reader asking for row 90,000 of a 1M-entry folder gets the surface's
  `SizeOfSet`, never a provider for rows that are not drawn.
- The snapshot is immutable (`Arc<ViewSnap>`); provider calls from the UIA
  threads read it and never touch the UI thread's state.
- Providers are cached per live row and disconnected (`UiaDisconnectProvider`)
  when a row leaves the snapshot. A call on a row that has gone answers
  `UIA_E_ELEMENTNOTAVAILABLE`.
- Requests (select, invoke, expand/collapse) are queued and the UI thread is
  woken with `WM_APP + 10` (`WM_APP_UIA`); `View::run_view_requests`
  (`src/view_uia.rs`) executes them against the current model and drops those
  whose row has gone.

## Files

- `crates/tsubame-win/src/uia/view.rs`: snapshot types, providers, events, tests.
- `src/uia_rows.rs`: the collector and the per-view builders (Details, Grid,
  Tree, Pane, Miller), keys, text.
- `src/view_uia.rs`: UI-thread executor of the requests.
- `src/view_uia_tests.rs`: whole-frame tests on the recording canvas.
- Hooks: `view.rs` (`begin_frame`, Details, Grid, `end_frame`), `view_tree.rs`
  (Tree rows and pane), `view_miller.rs` (columns), `tags/ui.rs` (picker invoke).

## Keys and ids

Surface keys are `slot * 64 + part` (`PART_LIST = 1`, `PART_PANE = 2`,
`PART_MILLER + i = 16 + i`), so the two panes of a dual-pane window never
collide. A file row's key is its entry id; a Tree row's key packs the tree
model slot, kind and id (`uia_rows::tree_key`). Runtime ids use kinds 4 (surface)
and 5 (row).

## Gaps

- Tree rows are flat TreeItems (no nested parent/child navigation in the UIA tree).
- Grid cells are exposed as a flat List too (no 2-D Grid pattern).
- No text/rename editor provider, no column header items, no drag.
- Miller shows the columns that fit on screen only (scrolled-off columns have no
  provider).
- Selection requests in a Tree pane move the folder cursor but do not extend a
  multi-selection (the pane is single-select).
