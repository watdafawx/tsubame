# INT1: panes and shell glue as one app

U3 (panes, rename, status bar) and W1 (shell menu, OLE drag and drop,
clipboard, delete checks) were built side by side, each with its own queue,
mouse routing and status text. INT1 joins them. This page says who owns what
now; `docs/u3-panes.md` and `docs/shell-integration.md` describe each half.

## One owner per job

| Job | Owner | Notes |
|---|---|---|
| File operations, progress, result text | `View::ops` (`src/ops.rs`) | The only `FileOpQueue` in the app. Copy, move, rename, batch rename, delete, paste, drop, F5/F6 all `submit` here. |
| Undo / redo | `Ops::undo` / `Ops::redo` | Call the journal's `undo_group` / `redo_group`: one Ctrl+Z reverts one whole submission (a 4-file batch rename, a 200-file paste) of any kind. |
| Queue events | `shellops::on_ops_timer` | Driven by main's `OPS_TIMER` (60 ms, only while a job runs). Answers conflicts (keep both by itself for same-folder copies, else the Yes/No/Cancel dialog outside the app borrow), then `View::after_ops`. |
| Status text | `Ops::message` | One slot. Progress, results, clipboard and pin messages all write it; the status bar paints only it (`status_message`). A new press clears it. |
| Mouse in the lists | `View::track_pointer` | Press, slow-click rename, multi-selection press, drag start. W1's own mouse routing is gone. |
| Drags | OLE, always | Inside the app too: `start_drag` sets `drag_out`, main runs `shellops::drag_out` after the frame. Our `IDropTarget` feeds the view (`drag_enter`, `drag_keys`, `drop_dest`). |
| File keys | `shellops::on_key` + `key_act` | Ctrl+C/X/V, Ctrl+Insert, Shift+Insert, Shift+Ctrl+C, Delete, Shift+Delete, Ctrl+Z, Ctrl+Y, Ctrl+Shift+N, for the ACTIVE pane's list (`View::list_focused`). |
| Folder contents after a change | the tab's watcher (`src/tab.rs`) | Relist only when there is no watcher or it lost events. |

## Two panes

- Every per-pane thing is keyed by pane: list focus (`pid(slot, ID_LIST)`),
  tab strips (`View::strips[slot]`), the context-menu request (`menu_pane`
  makes the clicked pane active before the menu opens), the background menu
  (`bg_menu`), rename (`rename_slot`).
- Outside a paint `live == active`, so key handlers use `view.tab()` and get
  the active pane's tab, in single and dual mode alike.
- File keys stay off while an inline rename, the batch bar or the filter has
  the focus (`list_focused`).

## Drag and drop

- Press on a row: nothing yet. A plain press on a row of a multi-selection keeps
  the selection (`PressInfo::collapse`); it collapses to that row on release
  only if no drag started.
- More than 5 DIPs of travel (`DRAG_START`) starts the drag with the whole
  selection. Main releases mouse capture and pushes a MouseUp to the kit
  before `DoDragDrop`, so the list never sees a stuck button.
- While a drag is over the window, `DragOver` sets `drag_keys`, pushes a
  MouseMove and paints synchronously; the paint decides `drop_dest`
  (`DropDest::Folder` or `DropDest::Pin`) and the hint chip. `DragOver` then
  reads `drop_dest`, so the cursor effect and the chip always agree.
- Drop targets, first match wins: a pinned-row gap (pin) or a sidebar folder,
  a tab of either pane (that tab's folder), a folder row of either pane, the
  folder of the pane under the pointer.
- Rules (one copy in `dnd`, `ole_glue` delegates): Ctrl copies, Shift moves,
  Alt or Ctrl+Shift makes shortcuts; plain = move on the same volume, copy
  across. Refused: a drive root, a folder into itself, a drop where every item
  is already in the target. A drop where only some items are already there goes
  on and skips those.

## After a job

`View::after_ops` runs for every finished job, for both panes and all tabs:

- A tab showing the job's folder that has a watcher gets `want_select(names)`:
  the names are selected as the watcher brings them in (the wish expires after
  3 s). Names are what really landed: a "keep both" copy selects
  `file1 (2).txt`, read back from the journal (`ops::landed`); an Alt-drop
  selects the `.lnk` names `make_links` returned.
- A tab without a watcher that the job touched is relisted.
- After an undo or redo, every tab without a watcher is relisted (the journal
  does not say which folders a group touched without walking it).

## Watchers on tabs

- Only the visible tab of each pane watches (`Tab::set_visible`). A hidden tab
  drops its watcher and keeps the folder's modified time; shown again, it
  relists when that time changed (always for UNC paths, whose times lie).
- Batches that arrive while a listing runs wait and apply after it.
- Up to `APPLY_INLINE` (30) changes apply on the UI thread; bigger batches are
  resolved on a thread and applied in arrival order.
- `keep_place` keeps the cursor row and the top row by entry id, so the
  selection and scroll position stay put while rows come and go above them.
- Overflow → relist. Folder deleted or renamed → the pane goes up a level.
  `NotSupported` / unavailable → the watcher stops. Network, removable and CD
  volumes are flagged unreliable: they still apply events, and our own jobs
  relist them.
- Idle cost: the watcher thread blocks in `ReadDirectoryChangesW`; the UI has
  no timer for it (it wakes through the view's wake handle only when a batch
  arrives).

## Rename

- F2: inline rename; several rows selected: the batch bar.
- Context menu: "Rename\tF2" on top (an app entry), and the shell's own
  "rename" verb is intercepted; both start the inline rename on the next frame
  (`rename_request`).
- Rename and batch rename go through `Ops::rename` / `rename_batch`, so Ctrl+Z
  reverts them like any other job (a batch in one step).

## Tests

| Area | Tests |
|---|---|
| Journal groups | `fileops/tests.rs`: undo/redo of a group as one step; `ops.rs`: `one_journal_for_every_kind_of_operation`, `keep_both_names_are_what_gets_selected` |
| Key routing | `shellops.rs`: `file_shortcuts`; `view.rs`: `clicks_and_keys_go_to_the_pane_they_are_in` |
| Per-pane hooks | `view.rs`: `context_menus_belong_to_the_pane_clicked`, `drop_targets_follow_the_pointer_across_panes_and_tabs`, `a_press_on_a_multi_selection_drags_all_of_it`, `finished_jobs_select_new_items_in_every_pane_showing_the_folder` |
| Watcher batches | `tab.rs`: `a_visible_tab_follows_changes_without_relisting`, `big_bursts_resolve_off_the_ui_thread_and_land_in_order`, `synthetic_and_hidden_tabs_do_not_watch` |
| Drop rules | `dnd.rs`, `ole_glue.rs` (Ctrl+Shift = link, partial same-folder drops, drive roots) |
| Shortcuts | `crates/tsubame-win/tests/dnd.rs`: `shortcuts` (names returned) |

By hand on rig (dual pane, fixtures under `G:\rig-test\int1-fixtures`, deleted
afterwards): copy/paste across panes, cut/paste, Delete then Ctrl+Z, menu
Rename then Ctrl+Z, batch rename of 4 then one Ctrl+Z, drag of a 4-item
selection to the other pane, drag onto a sidebar folder (conflict → keep
both), folder dropped on the pinned gap, drag in from Explorer and out to
Explorer, Alt-drag → shortcut (the new `.lnk` selected), a PowerShell
create/delete in both folders shown without F5 with the selection kept. Light
theme in dual pane; dark theme in single pane (paste, Delete then Ctrl+Z, F2
rename then Ctrl+Z). Screenshots: `docs/shots/int1-*.png`. Numbers: the INT1
section in `docs/bench-log.md`.
