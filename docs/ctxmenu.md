# Own-drawn context menu (CM2–CM9)

`src/ctxui.rs` owns one generation, target, worker handle, model and widget state.
`src/shellops.rs` opens it and dispatches picks after painting. The existing
file-operation queue, clipboard worker and inline rename handle app verbs;
other shell ids invoke on the STA that retains the original IContextMenu.
`windows.more` opens the native popup. Sidebar pin menus remain their existing
app-only popup (they did not use the shell menu).
Popups have no app-name header/caption rows, including loading and failure frames.
App-owned actions and group separators remain; custom entries use an accent dot.
Sidebar drive right-clicks use the shell-menu path. Drive/share roots cannot be
cut, renamed or deleted through app rows; dispatch also rejects those actions.
Keyboard anchors follow the active pane and grid cell.

Own rows require no COM or disk access. Folder classification comes from the
enumerated entry store. The first frame draws those rows and a thin loading
line. The native top level appends below them; stable row keys retain focus and
submenu state. Generation mismatches drop. Closing drops the handle; replacing
drops it before submitting another request. Retry creates a new generation and
discards old shell command ids and submenu tokens. The pool replaces hung
workers at four seconds, with its existing eight-thread abandon cap. At three
seconds the loading state shows failure/Retry; late results may still complete.

## Lazy submenus (CM6)

The worker walks only the top level up front. Each submenu it finds gets a
token (`MenuItem::sub`, flag `LAZY`). When the widget shows a submenu, or its
150 ms hover delay starts, `ctxui` asks `CtxHandle::expand(sub)` once per
generation (`ctxmodel::SubLoads`). The same STA worker sends `WM_INITMENUPOPUP`
to that submenu, walks one level (nested submenus get tokens of their own) and
answers `Part::Submenu { sub, items }`, stamped with the generation, so stale
answers drop in `MenuModel::apply`. While it fills a submenu the job counts as a
context-menu job again (`waiting(false)`), so the 4 s watchdog applies to a
hung handler; the UI thread never waits.

An empty lazy submenu shows "Loading…". After 3 s without an answer, or when the
worker was lost, it shows the failure row with Retry (a new generation). While
the type-to-filter text is set, every lazy submenu is asked for, so its rows are
searched as they arrive. `TSUBAME_CTX_EAGER=1` restores the CM2 eager walk.

## Packaged Windows 11 commands (CM7)

Windows 11 apps register context-menu commands (`IExplorerCommand`) in their
package manifest (`windows.fileExplorerContextMenus`), not in the classic
registry, so the classic `IContextMenu` does not list all of them ("Open in
Terminal", "Open with Code", PowerToys). `shell::packaged` finds them:
`HKCR\PackagedCom\Package` names the packages, each package's
`AppxManifest.xml` (from the AppModel repository's `PackageRootFolder`, else
`%ProgramW6432%\WindowsApps\<full name>`) maps item types (`*`, `.ext`,
`Directory`, `Directory\Background`, `Drive`, `AllFilesystemObjects`) to
CLSIDs. The scan is cached for 60 s (a cold scan took 167 ms on the test PC,
warm 5–9 ms).

The same STA worker that holds the classic menu runs it, after the classic
top level is out, so it never delays those rows: it creates each applicable
handler (`CoCreateInstance`, in-process or local server), drops hidden ones
(`GetState`), and reads title, tooltip, icon and flags. Items merge in one
menu after the classic rows and before the custom entries; there is no
"Show more options" row. Command ids use `0xA000..=0xBFFF`, submenu tokens
`0x1_0000..=0x1_FFFF` (lazy, `Part::Submenu` like CM6), so they never collide
with classic ids. Invoke runs `IExplorerCommand::Invoke` on the worker with
the host window in front. A command the classic menu already shows (same
CLSID verb, or the same label, which covers the classic proxies Windows adds
for some packaged commands) is left out. The package family and the CLSID
key the user's edits (`windows.pkg:<family>/{clsid}`, CM8; CM7's
`windows.verb:{clsid}` still applies). The loading line stays until the packaged step
is done; a handler that hangs there trips the same 4 s watchdog.

## Tall menus: the icon row (CM9)

When the top level does not fit the window's work area (bounds minus the
8 DIP keep-out), Cut, Copy, Paste, Rename, Delete and Share fold into an
icon-only row above the scroll area, like Windows 11 (`docs/ctxmenu-mock.md`
question 3). It needs at least two foldable rows. Once folded, the menu stays
folded while it is open (rows arriving later do not unfold it). If the rest is
still too tall, the list scrolls under the row with the chevron strips.

- Buttons are left-aligned, `row × 1.5` DIPs wide (narrower if the menu is
  narrow), keep the row states (disabled, danger) and show a tooltip with the
  label and shortcut on hover or keyboard focus.
- Keys: Up from the first list row (or Down from the last) enters the row;
  Left/Right move along it (wrapping); Down leaves to the first list row, Up to
  the last; Home goes to the first button; Enter/Space run it. Alt+letter
  accelerators include the folded rows.
- While the type-to-filter text is set, the icon row is hidden and folded rows
  match like any other row.

## Editing the menu (CM8)

There is no edit mode: right-clicking a row inside the open menu opens that
row's **edit menu** next to it (mock state 6a), and the row gets a dashed
accent outline. A right click is never an invocation. Keyboard: the Menu key
or Shift+F10 on the focused row (also an icon-row button) opens it; it is not
offered while the type-to-filter text is set.

- Rows: a header (the row's label and where it comes from: Windows, Tsubame,
  Custom, From Nilesoft or the app's package name), Hide "…" (Del), Rename…
  (F2), Move up (Alt+↑), Move down (Alt+↓), Edit in Settings…, and for
  Windows' and the apps' items the note "A Windows item: hiding it only
  changes Tsubame's menu. Explorer is not touched."
- Keys in the edit menu: Up/Down/Home/End move, Enter/Space run, Del hides,
  F2 renames, Alt+↑/↓ move the row, Esc or Left closes only the edit menu.
  Moves keep the edit menu open on the moved row.
- Hide: the row leaves the menu at once and a strip at the bottom offers
  Undo and Manage (Left/Right pick, Enter runs; Manage opens Settings on that
  item). The strip goes with the menu. Hidden rows are never listed in the
  menu, only in Settings.
- Rename: the row turns into a text field in place. Enter keeps, Esc
  cancels, a click elsewhere keeps, an empty name restores the original. Shell
  items keep their command; only the label is ours.
- Move up / down work on Tsubame's own rows and the user's entries (the U4
  list order). Windows' and the apps' items keep their native order inside
  their group; their Move rows are disabled. Whole groups are reordered in
  Settings.
- A click that closes the edit menu runs nothing.

Identity. Every row has a stable key, not its label: `windows.verb:<verb>`
for a classic item with a canonical verb, `windows.label:<label>` when the
handler gives none, `windows.pkg:<package family>/{clsid}` for a packaged item,
`app:<action>` for Tsubame rows outside the list (Copy name, Open terminal
here, …) and the U4 ids (`builtin.copy`, `custom.3`) for the list. Row keys of
keyed rows skip the label, so a renamed row keeps focus. Limitation: a classic
`IContextMenu` exposes no handler CLSID per item, so classic items are keyed
by verb and, without a verb, by label (a translated label without a verb is a
new item).

Edits land in the menu's copy of `[context_menu]` and the app writes them
with the usual debounced atomic save (`docs/config.md`): built-ins and
custom entries in `entries`, everything else in `[[context_menu.items]]`
(`hidden`, `name`). They apply in `compose_all` (the model compose step), so
the worker and the top-level timing are untouched: no reorder, only a
retain and a relabel per level. Items the menu shows are remembered in
`items` so Settings can list them (new ones are tagged New; the first menu
after an upgrade tags nothing), and the config is written only when one is new.
Nothing is removed from Windows.
## Widget and input (CM4)

The widget uses popup-layer hit testing, theme tokens, density/font metrics,
checked/radio/default/disabled/danger rows, shortcut text and custom accent dots.
Placement flips within client bounds with an eight-DIP keep-out. Tall menus use
chevron buttons and scroll with the wheel (three rows per notch). No shadows.
Menu CPU icons cap at 256; content hashes reuse device uploads in the renderer's
existing bounded cache. Only visible rows draw.

- Keys: arrows, Home/End, PageUp/PageDown (a page of rows, no wrap), Enter,
  Space (only without a filter), Right opens a submenu and focuses its first
  row (once its rows are in), Left and Esc close one level at a time.
- Typing filters: printable keys go to an inline field at the top (search
  glyph, text, caret, "N matches", Esc cap). Matching rows stay, the match is
  highlighted, separators and groups collapse, submenu rows show their parents
  as a dim prefix ("Send to › "). Backspace edits; the first Esc clears the
  filter, the next closes. The first match is focused when the text changes;
  rows arriving later never move the focus. Enter on a submenu match opens the
  submenu chain.
- Accelerators: underlined only while Alt is held or after a keyboard open
  (not while filtering). Alt+letter runs the row with that accelerator, cycles
  focus when several rows share it, and ignores rows without one. Alt keyup
  repaints; `WM_SYSCHAR` is swallowed while the menu is open (no beep).
- Hover: a submenu opens after 150 ms and closes 250 ms after the pointer moved
  to another row. While the pointer travels inside the triangle from its last
  position to the open submenu's near edge, the submenu stays and the focus
  does not move. A click on a submenu row opens it at once. A stationary pointer
  cannot steal keyboard focus.
- Timing is pure: the app passes a clock (`MenuStyle::now_ms`); the 33 ms tick
  timer runs only while the menu is loading, a lazy submenu is younger than
  3 s, or a hover delay is pending (`CtxMenu::ticking`). An idle open menu and
  no menu: no timer.

## Switches and measurement

- `TSUBAME_NATIVE_MENU=1`: native popup fallback.
- `TSUBAME_CTX_DELAY_MS=N`: worker delay before binding; use >4000 for watchdog
  failure. Debug/test use only; delay also affects Retry.
- `TSUBAME_CTX_SUB_DELAY_MS=N`: worker delay before each lazy submenu (shows
  "Loading…"; >4000 trips the watchdog).
- `TSUBAME_CTX_EAGER=1`: walk every submenu up front (CM2 behaviour, A/B runs).
- `TSUBAME_CTX_PACKAGED=0`: leave the packaged commands out (A/B runs).
- `TSUBAME_CTX_PKG_DELAY_MS=N`: worker delay before the packaged step (debug).
- `--stress ctxmenu --dir DIR`: 200 real own-menu open/paint/close cycles,
  alternating one and three files; lazy runs also fill one submenu per menu.
  Writes `s4-stress-ctxmenu.txt` beside exe: cold/warm first paint, top level,
  all items, lazy submenu latency, packaged worker time and row count, failures, process counters.
- `--stress menu --dir DIR`: existing native comparison.
- `TSUBAME_STRESS_MENUS=N`: cycle count, clamped to 200..10000.

### Settings editor: what is still not built

Built since CM8: the kind switch (File / Folder / Empty area, from each entry's
`show_for`), the "3rd party" chip (a packaged item whose package family is not
Microsoft's; the classic menu names no owner, so classic items count as
Windows), counts on the chips, New group, user groups
(`[[context_menu.user_groups]]`: `id`, `name`, `pos`), dragging a learned item
into a group (`ItemPref.group`) and Ctrl+F. Not built, because the mock does not
show it: the row "..." menu (its contents), the kind switch's "Several items" and
"Drive" (no data behind them), "Refresh from last menu", Pin to top, the file
picker and file-detected state of the Nilesoft import. The real right-click menu
does not compose user groups yet: items moved into one keep their native place
there (the one hook needed is in the group composition of `ctxmodel`).

## Later-card seams

CM10: UI Automation provider and high-contrast verification remain separate.

The stress scenario is separate from navigation/tab leak-gate: shell extensions
have known per-menu growth; mixing 1000 menus into that gate would obscure the
navigation baseline. Compare 200-menu growth against the native scenario.
