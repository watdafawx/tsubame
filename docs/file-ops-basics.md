# File operation basics (B10-C)

Explorer-parity details of rename, New, shortcuts and picture paste. Commands
are in the table of `docs/keys.md` (generated from `src/commands.rs`, ids 140-159).

## Commands

| Id | Command | Default key | Where |
|---|---|---|---|
| 140 | `file.create-shortcut` | none | palette, context menu of items (not a drive root) |
| 141 | `edit.paste-shortcut` | none | palette, context menu of empty space |
| 142 | `rename.previous` | `Shift+Tab` while renaming | bound only while a rename field is open |

`Tab` while renaming is the existing "switch pane" command, which commits the
name and renames the next row (`CMD_SWITCH_PANE` while `rename` is open).

## Rename

- `Tab` / `Shift+Tab` commit and start the rename of the next / previous row,
  also when the name did not change. The last / first row ends the rename.
  Index math: `rename::neighbour`. Not in Tree rows and Miller columns.
- Changing the extension of a FILE asks "If you change a file name extension, the
  file might become unusable. Are you sure?" (Yes / No). No question for a
  folder, a case-only change, or a file that had no extension (`rename::ext_change_needs_confirm`).
  Dialogs cannot open inside the frame: the view parks the edit in `View.ext_ask`
  and `shellops::after_frame` asks, then calls `View::ext_answer`. No keeps the
  field open with the typed name.

## New >

The empty-space menu has `New >`: Folder, Text Document, then the types Windows
registers under `HKCR\.ext\ShellNew` (`NullFile`, `FileName`, `Data`), at most 20,
known-bad types skipped (`tsubame_win::shellnew`). The scan runs once per
session on a thread started at the first context menu (or pre-warm); until it
ends the submenu shows only Folder and Text Document.

One row is one queue operation (`NewFolder`, `NewFile`, or `Copy` of the type's
template), so it is one undo step. The new item is selected and goes into rename
mode as soon as the listing has it (`View::rename_when_listed`, polled per frame,
gives up after 5 s). The native shell menu ("Show more options") keeps its own New.

## Shortcuts

- `file.create-shortcut` makes `<stem> - Shortcut.lnk` (extension stripped for files, like Explorer) next to each selected item
  (IShellLinkW + IPersistFile on an STA shell worker; the working directory of a
  file target is its folder).
- `edit.paste-shortcut` makes shortcuts in the shown folder to the files on the
  clipboard; the clipboard is left as it is (a cut stays a cut).
- Alt+drag (drop effect link) plans `Step::Link`; `Ops::submit_steps` collects
  them in `Ops.links` and `shellops::after_frame` creates them.

## Picture on the clipboard

When the clipboard has a picture (PNG, `CF_DIBV5` or `CF_DIB`) and no file list,
`Ctrl+V` saves it as a new `Image.png` (`Image (2).png`, ... if taken) with WIC and
selects it. Text on the clipboard: nothing happens, as before.

## Search wording

An empty result with a filter says "No items match your search".
