# Shell integration (W1): context menu, drag and drop, clipboard, delete

What Tsubame does with the Explorer shell: the native context menu with our own
entries on top, OLE drag out and drag in, the clipboard in the formats Explorer
uses, and a Recycle-Bin check before every delete. Everything that moves files
goes through the file-op queue, so it shows progress and can be undone.

## Where the code is

| File | What |
|---|---|
| `crates/tsubame-win/src/shell/menu.rs` | `MenuOpts`: app entries on top of the shell menu (`extras`), verbs the app runs itself (`intercept`), folder background menu (`background`, via `IShellFolder::CreateViewObject`). |
| `crates/tsubame-win/src/dnd/formats.rs` | Pure: `CF_HDROP`, `Preferred DropEffect`, `FileGroupDescriptorW` bytes, path validation, same-volume test. Unit tested. |
| `crates/tsubame-win/src/dnd/dataobject.rs` | Our `IDataObject` (+ `IEnumFORMATETC`), used for drag out and the clipboard. |
| `crates/tsubame-win/src/dnd/mod.rs` | `IDropSource`, `IDropTarget` (calls a `DropEvents` trait), `read_info` (what a data object holds, validated), OLE init. |
| `crates/tsubame-win/src/dnd/clipboard.rs` | Clipboard jobs (set files / text, read with virtual-file staging, create shortcuts). Run on shell workers. |
| `crates/tsubame-win/src/fileops/deletecheck.rs` | Which items the Recycle Bin can take. |
| `src/ole_glue.rs` | Pure drop rules: which DROPEFFECT, which file operations. Unit tested. |
| `src/shellops.rs` | App glue: keys, menus, clipboard flow, drag start, drop target, delete flow, file-op queue driving. |
| `src/view.rs`, `src/main.rs`, `src/shellui.rs` | Hook lines only (`ViewHooks`, message routing). |

## Threading

The UI thread makes no blocking shell COM call.

- **Context menus** run on the shell STA workers (`Request::Menu` with
  `MenuOpts`). The reply says which app entry was picked (`extra`) or which
  intercepted verb (`intercepted`); intercepted verbs are never invoked by the
  shell, the app runs them.
- **Clipboard** jobs (`Request::Clip(ClipJob)`) run on the same workers.
  `OleSetClipboard` calls `EmptyClipboard`, which sends a message to the
  previous owner, and reading a cut from another app is a cross-process call;
  either hangs when the other app hangs. Our data object is flushed at once
  (`OleFlushClipboard`), so the data outlives the worker and the app. The app
  submits one clip job at a time (`clip_busy` + a wait queue).
  While a job waits for a clipboard someone else holds open, it **pumps
  messages** (never a plain `Sleep`): the previous owner can be the other
  worker's OLE clipboard window, and the two workers would otherwise wait for
  each other until the retries run out. The stress run found this
  (CLIPBRD_E_CANT_OPEN on every read, 204 ms per job); with the pump it is 0
  errors and p95 4.4 ms.
- **Delete checks** (`deletecheck::check`) read the disk (folder sizes, up to
  8 s), so they run on a plain thread. Results go to a static and
  `PostMessage(WM_APP + 9)` brings them back to the UI thread, which asks and
  queues the deletes.
- **Drag and drop** is OLE on the UI thread, as it must be: `OleInitialize`,
  `RegisterDragDrop` on the main window, `DoDragDrop` called outside the app
  borrow (`crate::with`), because it runs a modal loop that dispatches our own
  window messages.
- **The file-op queue** has its own worker. The UI polls it with a 100 ms
  timer (`OPS_TIMER`) only while a job is in flight; idle means no timer.
- **Opening a terminal** (`wt.exe -d`, else `cmd.exe`) is started on a thread.

## Drag out

A press on a selected row without modifiers is held back. Moving past
`SM_CXDRAG`/`SM_CYDRAG` starts `DoDragDrop` with our data object
(`CF_HDROP`), allowing COPY | MOVE | LINK. If the button comes up first, the
held press is sent to the UI kit before the release, so clicks behave as
before. After the drop the active tab is refreshed.

Explorer does an optimized move on the same volume (it moves the files itself
and reports DROPEFFECT_MOVE via `Performed DropEffect` + `Paste Succeeded`).
Tsubame never deletes the source after a drag; it only refreshes.

## Drag in and the DROPEFFECT rules

Target, in this order: a tab in the tab strip (that tab's folder), a folder row
under the cursor, else the current folder. The hovered row is highlighted.

`ole_glue::choose(sources, target, keys, allowed)`:

| Keys held | Effect asked | If the source does not allow it |
|---|---|---|
| none, same volume | MOVE | COPY, then MOVE, then NONE |
| none, other volume | COPY | COPY, then MOVE, then NONE |
| Ctrl | COPY | NONE |
| Shift | MOVE | NONE |
| Alt, or Ctrl+Shift | LINK (shortcut) | NONE |

"Same volume" compares drive letters, or server + share for UNC paths, case
insensitive.

Always NONE (no drop):
- every item is already in the target folder (dropping onto its own folder is a no-op);
- the target is one of the items or inside one (a folder into itself);
- an item is a drive root;
- no valid paths.

`ole_glue::ops` turns the drop into queue operations: COPY → `Copy`, MOVE →
`Move` (items already in the target are skipped), LINK → no queue op; a shell
worker creates `<name> - Shortcut.lnk` files (`ClipJob::Links`).

## Clipboard

| Key / entry | What |
|---|---|
| Ctrl+C, Ctrl+Insert, menu "Copy" | `CF_HDROP` + `Preferred DropEffect` = COPY |
| Ctrl+X, menu "Cut" | same with MOVE |
| Ctrl+V, Shift+Insert, menu "Paste", background "Paste" | read and paste into the current folder (menu Paste on a folder item: into that folder) |
| Shift+Ctrl+C, menu "Copy path" | each path quoted, one per line (`CF_UNICODETEXT`) |
| menu "Copy name" | names, one per line |

Paste reads `CF_HDROP` + `Preferred DropEffect` (MOVE only when MOVE without
COPY). A cut pastes once: the clipboard is emptied after reading it, unless
someone replaced it meanwhile (sequence number check). Paste into the folder
the items are in answers "keep both" by itself (`file1 (2).txt`), like
Explorer. Other conflicts ask (Yes replace / No keep both / Cancel skip, for
the rest of the job).

**Virtual files** (`FileGroupDescriptorW` + `FileContents`, e.g. from a zip
folder or a mail client) are written to a staging folder under `%TEMP%`, then
moved into place by the queue. Paste only; virtual files are not accepted from
a drop yet.

Dimming cut items is **not done** (the brief made it optional).

## Security

- Nothing from the clipboard or a drop is ever executed or opened. Paths only
  become queue operations; text is only read as text.
- Every path from `CF_HDROP` goes through `formats::validate`, which refuses
  device namespaces (`\\.\`, `\\?\GLOBALROOT`, `\\?\Volume{...}`), `..`
  components, relative paths and alternate data streams (`file:stream`).
  Refused paths are counted (`rejected`) and dropped.
- Virtual-file names go through `formats::safe_relative`: no absolute paths,
  drive letters, `..`, ADS or reserved device names, so nothing lands outside
  the staging folder.
- Virtual files are capped at 512 MB in all per paste (`STAGE_CAP`). A
  declared size over the cap is skipped before anything is written; an
  undeclared size is cut off while streaming and the partial file is removed.
- Clipboard and drop reads have byte caps (`get_bytes(.., cap)`).

## Delete and its hazards

Delete recycles; Shift+Delete deletes permanently after one confirmation. The
context menu's Delete verb is intercepted and takes the same path.

Before a recycle, `deletecheck` finds items the shell would delete
**permanently without telling us**:

| Hazard | Why |
|---|---|
| path of 260 characters or more | the Recycle Bin refuses such paths |
| volume has no Recycle Bin | network shares, some removable drives |
| bin turned off (`NukeOnDelete`) | per-volume setting |
| item bigger than the bin's maximum size | folder sizes measured within 8 s; not measured in time counts as "might not fit" |

If any item hits one, a dialog lists them (names cut in the middle past 80
characters) with the reason: Yes deletes them permanently (and recycles the
rest), No keeps them (recycles the rest), Cancel deletes nothing.

## Context menu

Item menu, on top: Copy path, Copy name, Open terminal here, then a separator
and the shell's items. Background menu (right click on empty space): Paste,
New folder, New text file, Copy path, Open terminal here. The shell leaves
Paste out of a background menu that has no folder view behind it, so ours
stands in.

Intercepted verbs: delete, cut, copy, paste, rename. Rename from the menu
shows a status message for now (inline rename belongs to another task).

## Measured on rig

Debug build, `--stress clip`: 200 cycles of (item menu with our entries, auto
closed) + (clipboard set files, copy or cut) + (clipboard read), fixture folder
of 3 files.

| Budget | Result |
|---|---|
| first menu 0.5–1.3 s | 541–841 ms on screen across runs |
| warm menu < 100 ms | on screen p50 43 ms, p95 53 ms; UI submit → reply applied p50 72 ms, p95 87 ms |
| clipboard jobs | 400 jobs, p50 1.3 ms, p95 4.4 ms, 0 errors |
| private bytes < 90 MB windowed | 75.5 MB at the end of 200 cycles |
| exe < 6 MB | 1.39 MB (release) |
| leak growth within 5% | **fails, but not because of W1** (below) |

Leak gate B (menu 10 → menu 200) gave private bytes +8.0% with W1's clipboard
cycles, +8.4% for the plain `--stress menu` on this branch, and **+9.0% for
plain `--stress menu` on `main` (4a0c29a)**, same rig, same fixture. Handles
and GDI/USER objects are flat (±5). So the growth (about 30 KB per menu) is
older than W1 and comes from the menu path itself, not from the clipboard or
our entries. `docs/shell-threading.md` recorded −5.3% for the same gate at S4;
what changed since then (shell extensions on rig, or code merged after S4) is
not found yet.

Scroll paint p95 and idle wake-ups were not re-measured. W1 adds almost no per-frame
work (a few rect copies and the status text in paint) and no timer while idle (the queue timer runs
only while a job is in flight).

## Tested

Unit tests (`cargo test`):
- `src/ole_glue.rs`: modifier rules, allowed-effect fallback, self drops, operations, free names.
- `src/shellops.rs`: copy-as-path quoting, delete lists, long-name cutting.
- `crates/tsubame-win/src/dnd/formats.rs`: HDROP/effect/descriptor bytes both ways, path validation (GLOBALROOT, ADS, device names), safe relative names.
- `crates/tsubame-win/tests/dnd.rs` (headless OLE): our data object read back through `IDataObject`, the real clipboard round trip (copy, cut consumed once, text), virtual-file staging under caps and with hostile names, shortcut creation.

By hand on rig (push-to-rig, fixtures in `G:\rig-test\w1-fixtures`, deleted afterwards):
- item menu and background menu with our entries; Copy path; New folder; background Paste
- Ctrl+C in Tsubame → paste in Explorer; Ctrl+X in Explorer → Ctrl+V in Tsubame (moved, selected), Ctrl+Z restores it
- Shift+Ctrl+C → quoted path text
- drag out to Explorer (moved); drag in from Explorer onto a folder row (moved into it)
- conflict dialog: No = keep both (`file1 (4).txt`), Cancel = skip
- same-folder paste → `file1 (2).txt` without asking
- Delete → Recycle Bin; 263-character path → hazard dialog, Yes deletes permanently

Not tested by hand: drag to a second Tsubame window, drop on a tab, Alt-drop
shortcuts in the GUI (shortcut creation itself is covered by the headless test).

## Not done

- Dimming cut items.
- A drag image (the cursor shows the effect only).
- Virtual files from a drop (paste only).
- Right-button drag with the Move/Copy/Link menu.
- Rename from the context menu.
- Inactive tabs showing the dropped folder are not refreshed until visited.
- After an optimized move by the target, the source never deletes anything
  (correct for Explorer; a target that reports MOVE without moving would leave
  the files in place, which is the safe side).
