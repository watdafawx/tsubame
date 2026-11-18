# U3: sidebar, dual pane, rename

What shipped, where it lives, what is left out. Config keys: `docs/config.md`.

## Files

| File | Job |
|---|---|
| `src/commands.rs` | ONE `commands` table: id, name, label, shortcuts. Palette, menus and key dispatch all read it. |
| `src/sidebar.rs` | Pinned, Quick access, Recent, This PC (free-space bars), Network. 24 px rows, drives 32. 48 px rail. Drag rows to pin. |
| `src/panes.rs` | Split math (ratio clamp, 640 px narrow rule, rail snap). Pure, unit tested. |
| `src/dnd.rs` | Drag action resolution (same drive moves, other drive copies, Ctrl copy, Shift move, Alt or Ctrl+Shift shortcut) and refusals. The one copy of the rules; `ole_glue` delegates here. |
| `src/rename.rs` | Name validation, slow-click rule, batch pattern engine (`{name}`, `{n:3}`, `{date}`). Pure. |
| `src/editing.rs` | Inline rename widget state and paint. |
| `src/batchui.rs` | Batch rename bar and preview popup. |
| `src/ops.rs` | The app's only `fileops` queue (INT1): every file operation, group undo/redo, the one status message, what to select after a job. See `docs/integration.md`. |
| `src/statusbar.rs` | Selection count/size, free space, folder item count. |
| `src/overlay.rs` | Keycaps, drag ghost, small popups. |
| `src/view.rs`, `src/view_edit.rs` | Live pane fields + one stashed `other` pane; paint, input, command run. |
| `src/session.rs` | Two-pane capture/apply (`apply_layout`). |

## Panes

- `View` holds the active pane's state flat. The other pane sits in `other: Option<Box<PaneState>>`; `swap_live()` exchanges them, so single-pane code paths are unchanged.
- Dual layout: each pane has its own toolbar and tab strip; the active pane gets a 2 px accent underline on its toolbar.
- Splitter: drag, double-click resets to half. Ratio persisted (`panes.ratio`, 0.15–0.85).
- Under 640 px pane width: the filter box becomes a search button and the Type column hides.

## Keys

| Key | Command |
|---|---|
| F2 / slow second click | inline rename |
| Shift+Enter (in rename, name exists) | replace, with undo |
| Ctrl+B | sidebar open / rail |
| Tab | switch pane |
| Ctrl+\ | split on/off |
| Ctrl+Shift+\ | side by side / stacked |
| F5 / F6 (two panes) | copy / move selection to the other pane |
| Ctrl+Z / Ctrl+Y | undo / redo the last job of any kind, as one step |
| Ctrl+C / Ctrl+X / Ctrl+V, Delete, Shift+Ctrl+C | clipboard and delete in the active pane (`docs/shell-integration.md`) |
| Ctrl+Shift+Enter | open the selected folder in the other pane |

Deviation from the mock: F6 moves to the other pane; Tab is the focus switch.

## Since INT1

- Drags are OLE drags (W1), inside the app too; Alt or Ctrl+Shift makes
  shortcuts. A press on a multi-selection keeps it and drags all of it.
- Context menus, drop targets, list focus and rename are per pane.
- Each pane's visible tab follows its folder through a watcher: no F5 after
  outside changes, selection and scroll kept.
- Rename, batch rename and replace share the queue and journal with
  copy/move/delete: one Ctrl+Z each.

Details: `docs/integration.md`.

## Not done

- Sync-scroll chip and dragging a tab across panes.
- Recents are saved on exit (and with the debounced config flush), not on every navigation.
