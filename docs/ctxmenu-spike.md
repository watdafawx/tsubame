# Context menu spike (CM1)

Question: can Tsubame read the shell's context menu (`HMENU`) and draw it itself, the way `docs/ctxmenu-mock.md` decides? Answer: **yes**. The whole classic menu comes out as data on a worker thread, any item (submenus included) can be invoked by id without a popup, and the model adds no leak over today's native popup. Two gaps need extra work: Win11 packaged (`IExplorerCommand`) items that are not in the classic menu, and view items (View, Sort, Paste) that Explorer adds from its own view.

All numbers below were measured on a Windows 11 25H2 (build 26200) test PC (Ryzen 9 5900X), debug profile (optimized + debuginfo), with the rig harness `ctxmenu_dump`. Fixtures were `%TEMP%\tsubame-cm1\{a.txt, b.txt, p.png, folder\}`. Installed extensions on that PC: Nilesoft Shell, PeaZip, ShareX, Tailscale, Defender, NVIDIA, VS Code, Windows Terminal, Notepad, Photos, Paint. No 7-Zip, no Git.

Nothing is wired into right-click yet.

## Code

| Where | What |
|---|---|
| `crates/tsubame-win/src/shell/ctxmodel.rs` | Pure model, no Win32. `MenuItem`, `RawItem -> build_native` (separator cleanup, lazy flags, depth 4 / 512 items cap), `parse_label` (accelerator, tab shortcut), `route` (app verbs vs shell id), `own_items` / `apply_overrides` / `compose` on the U4 `tsubame_config::MenuEntry` list (no fork), `MenuBatch {generation, part, timing}`, `MenuModel::apply` (drops other generations), bounded LRU `IconCache` (256, deduped by content hash). 11 unit tests. |
| `crates/tsubame-win/src/shell/ctxmenu.rs` | Worker side. Builds the menu (`QueryContextMenu`), walks it with `GetMenuItemInfoW`, sends `WM_INITMENUPOPUP` to submenus through `HandleMenuMsg2` and waits for async fill (`settle`, quiet 40 ms, cap 300 ms), reads verb and help text (`GetCommandString`), converts icons on the worker (handler bitmaps only read, owner-draw and `HBMMENU_CALLBACK` drawn into our own DIB on black and white to recover alpha, DIB and DC freed). Keeps the `IContextMenu` alive (linger, default 600 s) so `CtxHandle::invoke(id)` runs `InvokeCommand` with `CMINVOKECOMMANDINFOEX` later. |
| `crates/tsubame-win/src/shell/mod.rs` | `Request::ctx(paths, generation, opts, &handle)` on the existing STA pool, `Payload::Ctx(MenuBatch)`. Abandon: a build counts as hung after `ctx_hang_after` (4 s), the worker is replaced, the pool's existing `max_replaced` (8) caps leaked threads. Once the menu is built and waiting for a command it is never treated as hung. |
| `crates/tsubame-win/tests/pool.rs` | `hung_own_drawn_menu_build_is_abandoned`, `own_drawn_menu_without_paths_fails_with_its_generation`. |
| `crates/tsubame-win/examples/ctxmenu_dump.rs` | Rig-only harness (touches the real shell). Prints the tree and timings; `--bg --ext --top --repeat N`, `--handlers` (per-handler create/init/query times with DLL), `--packaged` (manifest scan + `IExplorerCommand` probe), `--invoke VERB|#id|label:Text [--no-ui] [--hold S]`, `--cycles N [--native]` leak loop. |

## 1. Building and walking the HMENU

Works for every case tried. Per item we get id, text with `&` accelerator and `\t` shortcut, type and state flags, submenu, bitmap, verb (`GCS_VERBW`) and help text (`GCS_HELPTEXTW`).

| Target | Items (deep) | With icon | With verb | Owner-draw | Icon callback |
|---|---|---|---|---|---|
| a.txt | 43 | 15 | 29 | 0 | 0 |
| b.txt (after Send to fix) | 54 | 21 | 29 | 0 | 0 |
| folder | 39 | | | 0 | 0 |
| folder, Shift (`CMF_EXTENDEDVERBS`) | + opennewprocess, powershell, wsl | | | | |
| background | 9 | | | 0 | 0 |
| drive C:\ | 14 (BitLocker, format) | | | | |
| 2 files + folder | 43 | | | | |
| p.png | 62 (Photos/Paint verbs, wallpaper, rotate, print, Cast) | | | | |

- Distinct icons per menu: 12 to 17, so the icon cache dedupe matters (Open with, Send to repeat the same bitmaps).
- No owner-draw or callback items showed up on this PC; that code path is written but only covered by construction, not by a real handler (see "Not verified").
- Lazy submenus: Open with, Send to, New, Give access to are empty until `WM_INITMENUPOPUP`. **Send to fills asynchronously**: right after the message it holds 1 item, the other 14 arrive a few ms later. Without `settle` the walk saw 1 of 15 targets. Some Send to targets still have no icon when the count settles (icons also arrive later).
- Verb-less items exist: Defender's "Scan with Microsoft Defender...", PeaZip's submenu items, Send to targets. These can only be keyed by label.
- Packaged commands can appear in the classic menu: "Edit in Notepad" has verb `{CA6CC9F1-...}` (a CLSID), Photos/Paint likewise on .png.

## 2. Invoke by id without a popup

`CMINVOKECOMMANDINFOEX` (`UNICODE | PTINVOKE`, plus `NO_UI`, `SHIFT_DOWN`, `CONTROL_DOWN` as needed) on the kept-alive menu. Times are submit to invoke return, cold process.

| Item | Result | Notes |
|---|---|---|
| properties | Ok, 607 ms | Sheet opens on its own thread in **our** process and closes when the process exits. |
| copy | Ok, 652 ms | Clipboard holds the file while our process lives, **empty after it exits** (data object owned by us, not flushed). Tsubame does copy itself anyway. |
| delete (`--no-ui`) | Ok, 633 ms | File went to the Recycle Bin. Tsubame does delete itself (undo, own progress). |
| rename | **`E_NOTIMPL` (0x80004001)** | Needs an Explorer view. Tsubame must rename itself (it already does). |
| openas | Ok, 1021 ms | Spawns `OpenWith.exe` (separate process). |
| Send to > Compressed (zipped) folder | Ok, 936 ms | Submenu item invoked by id, `b.zip` created. |
| Edit in Notepad (packaged verb in HMENU) | Ok, 945 ms | Notepad opened the file. |

Split: the app handles `delete, cut, copy, paste, rename` (`ctxmodel::APP_VERBS`, same as `INTERCEPT` in `src/shellops.rs`), plus Tsubame's own rows. Everything else goes to the shell by id.

## 3. Win11 gap

The test PC has Nilesoft Shell installed, so Explorer's right-click there shows Nilesoft's menu, not the stock Win11 one; the "native Win11" column is the stock Win11 25H2 layout, **not observed on that PC**.

| Item | In classic HMENU? | In stock Win11 menu? | How Tsubame gets it |
|---|---|---|---|
| Cut / Copy / Rename / Delete | yes (verbs) | yes, top icon row | own rows (app verbs) |
| Share | yes (`windows.modernshare`) | yes, icon row | HMENU |
| Open, Open with, Properties | yes | yes | HMENU (Open with submenu lazy) |
| Copy as path | yes (`copyaspath`) | yes | HMENU, or own "Copy path as" |
| Add to Favorites / Pin to Quick access | yes (`pintohomefile`) | yes | HMENU |
| Edit in Notepad, Photos, Paint (packaged) | yes, CLSID verbs | yes | HMENU; dedupe against the packaged scan |
| Open in Terminal (folder, background) | **no** | yes | packaged scan: `IExplorerCommand` from `{9f156763-...}` works from our process (title "Open in &Terminal", state enabled, 74 ms cold, 7 ms warm) |
| Open with Code (VS Code, sparse package) | **no** | yes | packaged scan finds it, but `GetState` returns hidden for our process and the title is garbage. Not reachable that way; offer as a user entry (U4 custom command) |
| Compress to (ZIP/7z/TAR) | **no** | yes (24H2+) | own row, or "Send to > Compressed folder" (works, see 2) |
| Open in new tab / new window (folder) | no (only `opennewprocess` with Shift) | yes | own rows (Tsubame tabs/panes) |
| Background: View, Sort by, Group by, Refresh, Paste, Undo | **no** (they come from Explorer's view) | yes | own rows |
| Background: New > folder/shortcut/files | yes | yes | HMENU (lazy) |
| Defender scan, PeaZip, ShareX, Tailscale | yes (some without verb) | under "Show more options" | HMENU |
| Show more options | n/a | yes | see recommendation |

Packaged discovery: `HKCR\PackagedCom\Package\<full name>` -> package root from the per-user AppModel repository key -> `AppxManifest.xml` `windows.fileExplorerContextMenus`. Sparse packages (VS Code) keep the manifest in `%ProgramW6432%\WindowsApps\<full name>`. 157 entries on the test PC, scan 167 ms cold, 5 to 9 ms warm. One registered CLSID (Office hub `.txt`) failed with class not registered.

**Recommendation for "Show more options": none.** Tsubame shows one menu:

1. Tsubame group (own rows, including View/Sort/Paste/new tab and the app verbs) at t = 0.
2. The full classic `QueryContextMenu` (with `CMF_EXTENDEDVERBS` when Shift is held), in native order.
3. Packaged `IExplorerCommand` items merged in: cached manifest scan, `GetState` first, skip hidden, skip CLSIDs already present as a verb in the HMENU.
4. User entries.

Keep the existing `windows.more` entry as an escape hatch that opens today's native `TrackPopupMenu` path (S4) for anything the own-drawn menu cannot render.

## 4. Timing, deadline, abandon

Submit to first native batch (top level) and to full tree (deep walk), from the harness; "warm" is a second menu in the same process.

| Target | Cold top | Cold full | Warm top | Warm full |
|---|---|---|---|---|
| a.txt | 1408 ms (QueryContextMenu 1351, bind 38) | 1643 ms | 41-58 ms | 82-98 ms |
| b.txt with Send to settle | 500 ms | 773 ms | 41 ms | 252 ms |
| folder | 489 ms | 587 ms | 45 ms (one outlier 267) | |
| background | 421 ms | 622 ms | 20-32 ms | |
| drive | 450 ms | | 36-43 ms | |
| 2 files + folder | 908 ms | | 52 ms | |
| p.png | 1080 ms | 1501 ms | | |

- (a) Tsubame's own rows need no shell call; they are composed from config in the UI thread, so they can paint in the first frame. Not measured in a real window (nothing wired).
- (b) All shell items: 20-60 ms warm for the top level, so after the first menu of a session the rows land before the 120 ms misclick guard in the mock. Cold first menu: 0.4-1.4 s.
- Slow parts, per handler (`--handlers`, a.txt, cold): Nilesoft `shell.dll` 141.5 ms (and `E_NOTIMPL` at create), Defender `shellext.dll` 38 ms, Open With 16 ms, Send To 9 ms, Sharing 7 ms. Sum 231 ms, against 582-1351 ms for the composite cold `QueryContextMenu`: most cold cost is outside `ContextMenuHandlers` (static verbs, association lookup, packaged lookups). Warm, the same handlers sum to 12-20 ms.
- Deep walk cost is dominated by `settle` (40 ms quiet per lazy submenu). Better: walk only the top level up front and fill one submenu on hover (`Part::Submenu {path, items}` already exists).

Deadline design (implemented in the pool, covered by the headless tests):

1. t = 0: own rows, thin loading line.
2. Native batch replaces the line in place; stale generations are dropped by `MenuModel::apply`.
3. 3 s (mock): a "Some items could not load · Retry" row; the menu stays usable.
4. 4 s (`ctx_hang_after`): the worker is abandoned and replaced; at most 8 leaked threads (`max_replaced`), after that menus fail fast with the Retry row instead of leaking more.
5. Retry submits a new generation on a fresh worker.

Fake slow handler: **not built.** It would need a test COM DLL registered under HKCU on the test PC; a debug hook (`CtxOpts::debug_hang_ms`) simulates the hang in the worker instead, and `hung_own_drawn_menu_build_is_abandoned` proves abandon, generation and `leaked_alive` accounting.

## 5. Leaks

`--cycles N`: build + walk + destroy, counters at menu 10 and N.

| Path | Target | Menus | Private growth | Per menu | Handles / GDI / USER |
|---|---|---|---|---|---|
| model (deep walk) | a.txt | 10 -> 200 | +6.55 MB | 35.3 KB | -9 / 0 / -1 |
| native `TrackPopupMenu` | a.txt | 10 -> 200 | +6.31 MB | 34.0 KB | +14 / +5 / +2 |
| native | a.txt | 10 -> 600 | +17.29 MB | 30.0 KB | -13 / 0 / -2 |
| model (top level) | a.txt | 10 -> 600 | +16.98 MB | 29.5 KB | 0 / 0 / 0 |
| model (top level) | background | 10 -> 300 | +7.46 MB | 26.3 KB | 0 / 0 / 0 |
| model (top level) | p.png | 10 -> 300 | +10.68 MB | 37.7 KB | -4 / 0 / 0 |

- The own-drawn model leaks **no more** than the native popup Tsubame ships today; the leak gate (model within 5 % of native) holds.
- Both grow linearly (no plateau by 600 menus), even for the 9-item background menu. Handles, GDI and USER objects stay flat, so icon conversion leaks nothing. The growth is inside shell code; PIDLs, menus and our DIBs are freed. This matches PERF1's "+7.7 % at 200 menus, outside our heap".
- 30 KB per menu is 3 MB per 100 menus: fine for a session, worth a UMDH run per handler later.

## 6. Accessibility and keyboard (notes)

- Own-drawn means no free MSAA/UIA. Needs a UIA provider: Menu / MenuItem control types, Invoke, ExpandCollapse for submenus, Toggle for checked items, `IsEnabled`, `AcceleratorKey` from the `\t` part, `AccessKey` from `&`. The loading line and the Retry row should raise a live-region change.
- Keys like a Win32 menu: Up/Down wrap and skip separators and disabled rows, Right/Enter opens a submenu, Left/Esc closes one level, Home/End, Apps key and Shift+F10 open at the focused item. Mock decision: typing filters, accelerators become Alt + letter. Duplicate access keys cycle (Win32 behaviour).
- Shift opens the extended menu: that is a different `QueryContextMenu` call (`CMF_EXTENDEDVERBS`), so Shift held at open time picks the flag; pressing Shift later would need a rebuild.
- High Contrast: recolour tokens; shell bitmaps stay as they are.

## Risks

1. **Packaged items are uneven from an unpackaged process**: Terminal works, VS Code hides itself. Other vendors may do the same; those users lose items they see in Explorer. Mitigation: user entries, `windows.more` escape hatch.
2. **Identity of native items**: the mock wants verb + handler CLSID. The composite menu does not say which handler added an item, and some items have no verb (Defender, PeaZip, Send to). Overrides fall back to the label, which breaks across languages and product updates.
3. **Async fillers**: Send to proved that some submenus fill late; others (icons too) may fill later than the 300 ms cap. Hover-time fill makes this cheaper but not exact.
4. **Shell-side growth** of ~30 KB per menu, same as today, never freed.
5. **Owner-draw handlers** (some older extensions) render via `WM_DRAWITEM` into our DIB; untested against a real one. Bad ones may draw wrong colours or sizes.
6. **Invoke ownership**: Properties sheets and copied data live in Tsubame's process; quitting Tsubame closes them.

## Cards

| Card | Scope | Estimate |
|---|---|---|
| CM2 wiring | Right-click submits `Request::ctx`, keeps the `CtxHandle`, routes app verbs vs shell ids, setting to fall back to the native popup | 1 day |
| CM3 popup and paint | Popup window, rows/separators/icons/check/radio/disabled, density and font scale, tokens, edge flip and keep-out | 2-3 days |
| CM4 input | Hover, click, submenu 150/250 ms with safe triangle, keyboard map, accelerators, type-to-filter, misclick guard | 2 days |
| CM5 loading states | Own rows at t = 0, loading line, rows in place, Retry row at 3 s, abandon at 4 s | 1 day |
| CM6 lazy submenus | Top level only up front, fill one submenu on hover (`Part::Submenu`), settle per submenu | 0.5-1 day |
| CM7 packaged commands | Cached manifest scan, `GetState`, dedupe with HMENU verbs, `IExplorerCommand::Invoke` with an `IShellItemArray` | 1.5-2 days |
| CM8 edit mode | Right-click an entry: hide/rename/reorder, Settings list of native entries, New tag | 1.5 days |
| CM9 tall menus | Fold the quick row, then chevron scroll | 1 day |
| CM10 accessibility | UIA provider, High Contrast | 2 days |
| CM11 gates | Bench rows (first rows painted, all rows), leak loop on the test PC | 0.5 day |

Total about 13-15 days.

## Not verified

- Paint time of the first rows in a real window (nothing wired).
- Owner-draw and `HBMMENU_CALLBACK` icons against a real handler (none installed).
- The stock Win11 menu on the test PC (Nilesoft replaces it); the Win11 column is from the stock layout.
- 7-Zip, Git and other common handlers (not installed).
- A real slow third-party handler (simulated with the debug hang only).
- `IExplorerCommand::Invoke` for packaged items found by the scan (only probed; Terminal not invoked).
- Release-profile timings.
