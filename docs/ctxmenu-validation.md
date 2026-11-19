# Context menu verification (CM2–CM9)

Implementation inherited the CM1 model/STA worker and the WIP menu widget,
composition, right-click wiring and loading state. Completion repaired the
unfinished stress harness, repaint/handle lifetime issues, retry generations,
keyboard focus and pane/cell anchors, shortcut rounding, root safety and drive
routing. Popup captions were removed following user feedback; mock HTML,
decisions and reference PNGs match that change.

## Validation method

Release app on the remote test desktop, through the shipping/slot harness.
Screenshots use keyboard input with focused-window checks before every shortcut.
No local GUI tests. Mock PNGs rendered by an isolated headless browser on the test
host; its temporary profile is removed after verification.

`--stress ctxmenu` opens, paints and closes 200 real app menus, alternating one
and three files. Timing starts immediately before submitting the UI request and
ends after `Canvas::end`; it includes own-row composition, worker queueing (for
completion), UI wake and frame submission, but not physical input delivery or
display scanout. Cold means first menu in a fresh process, not purged OS caches.
Warm figures exclude that first menu. Native comparison uses `--stress menu` on
the same binary and fixtures. Growth compares samples at menu 10 and 200,
excluding the one-time handler DLL load; absolute growth is reported honestly
alongside comparison with native. Navigation/tab gate stays a separate scenario.

## Screenshots checked

All popup captures have no app-name caption. App actions, separators and shell
icons remain. Checked against revised mock look/kinds/loading/flip/submenu states:

- `shots/cm2-file-light.png`, `cm2-file-dark.png`: default semibold Open,
  right-aligned complete shortcuts, separators, disabled Paste, red Delete,
  mono app glyphs and colour shell icons.
- `shots/cm2-folder-light.png`, `cm2-folder-dark.png`: folder-specific new-tab
  action; no caption before Open.
- `shots/cm2-multi-light.png`, `cm2-multi-dark.png`: retained multi-selection
  and menu targeting, with the same flag/shortcut styling.
- `shots/cm2-background-light.png`, `cm2-background-dark.png`: empty-folder
  keyboard invocation, disabled Paste, New folder/text file, native New submenu.
- `shots/cm2-submenu-dark.png`: populated Send to submenu, keyboard focus ring.
- `shots/cm2-grid-dark.png`, `cm2-dual-dark.png`: menu follows grid cell and
  active pane; Open in other pane appears when split.
- `shots/cm2-flip-dark.png`: submenu flips left and is clamped above lower edge.
- `shots/cm2-tall-dark.png`: clipped viewport with both chevron strips and thumb;
  rows retain their height rather than shrinking.
- `shots/cm2-loading-dark.png`, `cm2-failure-dark.png`, `cm2-retry-dark.png`:
  immediate own rows and thin line; 5-second injected worker delay produces
  failure/Retry; keyboard Retry removes failure and starts a fresh loading line.

Differences from illustrative mock: real installed handlers determine labels,
icons and submenu contents; current U4 command table determines own actions.
Own actions form a prefix, as required for stable asynchronous loading, rather
than the illustrative interleaving in the full-menu mock. Custom user entries
remain after shell entries. No quick row. Safe-triangle delays, text filtering,
access-key invocation, wheel scrolling and insertion misclick guard are CM4;
packaged discovery, inline menu editing and folding common actions are later
cards. The revised mock deliberately has no caption row.

## Tests and limits

Workspace check passed. Workspace tests include layout/flip/scroll math,
keyboard/submenu/hover routing, stationary-pointer focus, right-click noninvoke,
row identity, generation dropping/loading clock, pending-verb failure cleanup,
root classification and icon-cache cap/eviction. The real clipboard integration
test is environment-sensitive and failed on repeated full-suite runs; earlier
runs passed it. Remaining suite is rerun with only `clipboard_round_trip` skipped.
Mock JavaScript passed `node --check`.

Not GUI-verified: every theme set/font-scale combination (pure density/scale tests
cover metrics), custom-entry dot with a user-configured command, checked/radio
rows from a real shell extension, owner-draw/callback bitmaps, real hung third-party
handlers, each destructive/file-operation route, Properties before failure,
physical right-button/Apps-key input and drive-row interaction (remote tests use
keyboard input). Drive routing and root guards are implemented; sidebar bookmark
management remains its existing app-only popup. UI Automation is CM10. ETW
wake-up counts are not measured; idle CPU and timer lifetime are checked instead.
One full navigation/tab gate run is performed per measured binary; stability
across three repeated runs is not verified. Cold filesystem-cache navigation is
outside these menu cards.

Later hooks: CM4 input/hover branches and stable keys; CM6 `Part::Submenu` paths;
CM7 composition boundary before custom entries; CM8 U4 override identities;
CM9 chevron viewport before folding common actions. See [ctxmenu.md](ctxmenu.md).

## Measured menu results

Final app code at `fa31fd5`; release executable 2,859,008 B,
SHA-256 `3c4e28e615f68021` (prefix; full digest in `measurements/cm2-meta.txt`).
Shipping guard confirmed the executable was written by this build. The shipping
line marked the worktree dirty because screenshot/report artifacts were pending;
compiled source was committed. Size remains below 6 MB with no >10% increase.

| Metric | Own-drawn | Native | Budget / interpretation |
|---|---:|---:|---|
| Cold first painted frame | 2.315 ms | 508.98 ms to native popup | Own rows are immediate; native waits for shell query |
| Warm first painted frame, median / p95 | 1.29 / 1.71 ms | 33.59 / 39.78 ms to native popup | Includes app frame submission |
| Cold complete shell tree | 615.077 ms | Not a tree-streaming scenario | First menu in fresh process |
| Warm complete shell tree, median / p95 | 162.21 / 200.98 ms | Not a tree-streaming scenario | Eager deep walk, CM6 will make it lazy |
| Load failures / 200 menus | 0 | 0 never shown | PASS |
| Private bytes at menu 10 / 200 | 71.0 / 75.8 MB | 68.3 / 74.0 MB | Own grows less than native |
| Growth, menu 10 to 200 | +4.8 MB, +6.8% | +5.7 MB, +8.3% | Both FAIL absolute 5% warm-menu gate; own/native comparison passes |
| Private bytes after settle, windowed | 74.3 MB | 73.8 MB | <90 MB: PASS |

Menu growth remains consistent with CM1's shell-side growth. This task does not
claim that repeated shell menus meet the absolute 5% gate; it shows that the
own-drawn path grows less than the native comparison. Handles/GDI/USER remain
bounded: own menu 10 to 200 is -8 / 0 / 0, native +17 / +5 / +2. Raw menu reports:
[own](measurements/cm2-own.txt), [native](measurements/cm2-native.txt).

## Final budget harness

Same executable digest as the menu stress, 10 release startup/scroll launches,
one 10,000-navigation/1,000-tab gate. Full results in
[measurements/cm2-budgets.md](measurements/cm2-budgets.md).

| Metric | Measured | Budget / result |
|---|---:|---|
| Startup to first frame, median / p95 | 71.00 / 95.70 ms | <100 ms median: PASS |
| Scroll paint p95, median of 10 runs | 2.90 ms | <5 ms: PASS (run p95 spread's p95 4.53 ms) |
| Idle CPU, harness 10-second window | 93.8 ms | FAIL; earlier build/run read 0 ms |
| Idle CPU, separate 3 steady windows | 0 / 15.6 / 15.6 ms per 10 s | Under harness's 50 ms tolerance; strict zero wake-ups not proven |
| Small folder, one tab, 4K private | 64.2 MB | <110 MB: PASS |
| Private after 100k-folder scroll, median / max | 82.2 / 85.1 MB | <110 MB: PASS |
| Navigation/tab leak gate | 81.64 -> 84.45 MB, +3.44% | <=5%: PASS; handles/GDI/USER/threads also pass, no timeouts |
| Main executable | 2,859,008 B | <6 MB: PASS |

Idle is not claimed as a clean gate pass. The normal harness observed one
93.8 ms sample; a separate diagnostics-mode run, which disables background
updates/config writes, observed 0/15.6/15.6 ms. CPU accounting is quantized to
15.6 ms on this host. No context menu was open in these windows, so no menu
animation timer existed. ETW wake-up counts remain unverified. See
[idle harness](measurements/cm2-idle-harness.txt) and
[steady samples](measurements/cm2-idle-steady.txt).

Final tests: `check --workspace` passed during implementation. Latest
`test --workspace` attempts failed only `tsubame-win --test dnd
clipboard_round_trip` (known real-clipboard flake). Full remaining workspace run
with `-- --skip clipboard_round_trip`: 637 passed, no failures; the excluded
clipboard test passed in earlier full runs. No dependencies added.

## Change ownership

| Files | Inherited versus completed |
|---|---|
| `crates/tsubame-ui/src/widgets/menu.rs` | Inherited popup/layout/paint/input widget; completed stationary-pointer focus, scroll-strip stability, chord-width rounding, right-click noninvoke and caption removal. |
| `src/ctxui.rs` | Inherited composition/model/worker wiring; completed metadata-only classification, root disabling, Retry session reset, failed/pending-handle cleanup, submitted-frame timing and caption removal. |
| `src/shellops.rs` | Inherited own/native routing; completed replacement lifetime, event-driven repaint, background/drive entry points and root dispatch guards. |
| `src/main.rs` | Inherited keys/loading timer/watchdog; completed empty-folder/grid keyboard targeting, pane anchors, Alt redraw and one-gesture right-click replacement. |
| `src/view.rs` | Inherited topmost menu paint; completed frame-submission timing hook, pane/cell anchor and pane regression assertions. |
| `src/sidebar.rs` | Added drive right-click request handoff; existing bookmark-management popup retained. |
| `src/stress.rs` | Inherited unfinished CM2 fields; completed real own-menu 200-cycle scenario, startup selection, cold/warm latency and growth reports. |
| `crates/tsubame-win/src/shell/{ctxmodel,ctxmenu,mod}.rs` | Inherited CM1 plus WIP own rows, loading/Complete parts, delay switch and queue priority; reused unchanged by replacement. |
| `crates/tsubame-ui/src/{lib,ui}.rs`, `widgets/mod.rs` | Inherited exports, rounded popup entry point and key capture support. |
| `docs/ctxmenu.md`, `docs/ctxmenu-mock.md`, mock HTML/PNGs | Added architecture/env/card hooks, applied caption feedback and refreshed reference images. |
| This report, `measurements/cm2-*`, `shots/cm2-*` | Added reproducible measurements, evidence, limitations and screenshots. |

Native fallback was also opened interactively with `TSUBAME_NATIVE_MENU=1`;
`shots/cm2-native-fallback.png` shows the Windows popup with existing app extras.
The escape-hatch pick is covered by routing tests and uses that same native path.

Cleanup: stopped the owned test app through its slot; removed task fixtures,
headless-browser scratch/profile and local scratch files. No unrelated app was
closed. Standard harness raw report directories remain as measurement evidence.

## CM4 + CM6: input polish and lazy submenus

Same method as above: release build of commit `83c307e` (exe 2,887,680 B,
SHA-256 prefix `3dc29a397d945a7e`, +1.0% vs CM2's 2,859,008 B) on the remote
test desktop, keyboard-only screenshots, focused-window check before each key.

### Screenshots (`docs/shots/`)

| File | Checks |
|---|---|
| `cm4-mouse-dark.png` | Mouse open: no accelerator underlines; Copy and Copy name use different glyphs |
| `cm4-filter-dark.png` | Filter field (glyph, caret, "N matches", Esc cap), match highlight, dim "Send to › " prefix, separators collapsed |
| `cm4-keyboard-dark.png` | Keyboard open: underlines and focus ring |
| `cm4-submenu-kb-dark.png` | Alt+N opens Send to with its first row focused; Left returns to the parent |
| `cm4-alt-held-light.png` | Alt held after a mouse open shows underlines; keyup hides them |
| `cm4-filter-loading-light.png`, `cm4-filter-arrived-light.png` | Filtering while shell rows load; focus stays on the first match as 36 matches arrive |
| `cm6-sub-loading-light.png`, `cm6-sub-filled-light.png` | Lazy submenu shows "Loading…" (`TSUBAME_CTX_SUB_DELAY_MS`), then its rows with the first focused |

First Esc with a filter clears it and keeps the menu; the next Esc closes.

Deviations from the mock: shell handler icons vary between runs (some handlers
return no bitmap); the filtered menu widens to fit prefixed labels; the
"Loading…" row has no icon.

### Menu timing (`--stress ctxmenu`, 200 menus, same binary and fixtures)

Raw reports: `docs/measurements/cm6-lazy.txt`, `docs/measurements/cm6-eager.txt`.

| Metric | CM2 (before) | CM6 lazy (default) | Eager (`TSUBAME_CTX_EAGER=1`) |
|---|---|---|---|
| Warm first paint p50 / p95 | 1.29 ms p50 | 1.46 / 1.83 ms | 1.27 / 1.67 ms |
| Warm right-click to top-level items p50 / p95 | (same as all) | 35.81 / 41.69 ms | 35.72 / 41.36 ms |
| Warm right-click to all items p50 / p95 | 162.21 / 200.98 ms | 35.81 / 41.69 ms (submenus on demand) | 184.91 / 246.53 ms |
| Cold first paint / top level / all | 615 ms all | 2.571 / 557.1 / 557.1 ms | 2.405 / 573.0 / 798.9 ms |
| Lazy submenu answer | n/a | p50 55 / p95 64 / max 112 ms (200 of 200) | n/a |
| Private, menu 10 → 200 | +4.8 MB (native +5.7) | 73.1 → 77.5 MB (+4.4 MB) | 72.4 → 77.5 MB (+5.1 MB) |
| Failures | 0 | 0 | 0 |

The lazy first-paint p50 is 0.17 ms above CM2; the eager run on the same binary
measured 1.27 ms, so the difference is within run-to-run noise.

### Budget harness

Full output: `docs/measurements/cm4-cm6-budgets.md`. Cold start 61.40 ms median
(PASS), scroll p95 2.76 ms (PASS), private 63.0 MB small folder at 4K and
83.9 MB max after the 100k scroll (PASS), leak gate +2.1% (PASS), exe 2820 KB
(PASS). Idle CPU: 0 ms over the first window, 78 ms over the second (FAIL, the
same pattern as CM2's 93.8 ms; no menu was open, so no menu timer existed, and
the menu tick timer is armed only while `CtxMenu::ticking()` is true).
Cold-cache 100k open was not measured (needs elevation).

### Tests

`test --workspace`: 660 passed, 0 failed. New unit tests cover submenu
open/close timing and the safe triangle, accelerator cycling, filter matching
and prefixes, page/Home/End focus, wheel scrolling, focus fallback when a row
vanishes, `SubLoads` state, lazy "Loading…"/failure rows and distinct Copy /
Copy name glyphs.

### Not verified on the test desktop

Hover open/close timing and the safe triangle, wheel scrolling, the Retry row
for a slow lazy submenu and the "No matching items" row are unit-tested only.

## CM7 + CM9: packaged commands and the icon row

Release build of commit `6f21146` (exe 2,974,208 B, SHA-256 prefix
`3e488d995d203179`) on the remote test desktop (one 4K monitor at 200 %),
`TSUBAME_GPU_PARK=0`, keyboard only, focused window checked before keys.
Fixture: a folder with one subfolder and five `.txt` files.

### Menu timing (`--stress ctxmenu`, 200 menus, same binary, runs interleaved)

The test PC was also running game servers; the first pair of runs was noisy
(CPU load up to 97 %), so the table uses the second, interleaved pair.

| Metric | Packaged on (default) | `TSUBAME_CTX_PACKAGED=0` | CM6 reference |
|---|---|---|---|
| Warm first paint p50 / p95 | 1.52 / 1.97 ms | 1.51 / 2.00 ms | 1.46 / 1.83 ms |
| Warm right-click to top-level items p50 / p95 | 38.38 / 44.78 ms | 37.96 / 44.38 ms | 35.81 / 41.69 ms |
| Warm right-click to all items p50 / p95 | 48.69 / 56.80 ms | 37.96 / 44.38 ms | 35.81 / 41.69 ms |
| Packaged step (worker) p50 / p95 | 10.81 / 13.15 ms | n/a | n/a |
| Cold first paint / top level / all | 8.5 / 477.2 / 497.3 ms | 2.8 / 459.0 / 459.0 ms | 2.6 / 557.1 / 557.1 ms |
| Failures | 0 | 0 | 0 |

The top level is unchanged within noise (+0.4 ms p50 against the same binary
with packaged commands off); the packaged step runs after it and adds about
11 ms to "all items" (the loading line ends later). The first quiet run
measured 36.02 / 40.61 ms top level with packaged commands on. The stress
targets are the `.txt` files, whose packaged handlers on that PC are all
hidden by `GetState` or already in the classic menu, so the stress menus had
no packaged rows; the time is creating and asking those handlers.

### Checked on the test desktop

- Folder: "Open in Terminal" (with its icon) arrives as the last group after
  the classic rows; picking it with the keyboard opened a Terminal tab in that
  folder (`IExplorerCommand::Invoke`). On a later open the classic menu listed
  its own "Open in Terminal" proxy, and the packaged copy was left out (no
  duplicate). `.txt` file: "Edit in Notepad" shows once (classic proxy).
- Window 450 DIPs tall, light and dark: the menu folds Cut, Copy, Paste
  (disabled), Rename, Delete (danger colour) and Share (files) into the icon
  row above the list; the list scrolls with chevron strips under it.
- Up from the first row enters the icon row, Right moves along it; the
  focused button shows a focus ring and a tooltip ("Copy  Ctrl+C").
- Typing "ren" hides the icon row and filters the folded Rename as a normal
  row ("1 match"); Esc clears, Esc closes.

No screenshots were committed: every capture of the test desktop's menus
shows third-party shell extensions installed there.

### Tests

New unit tests: manifest parsing (every extension block, any namespace
prefix), handler applicability per item class and skip list, id and token
ranges, merge order and dedupe (classic, packaged, custom; lazy packaged
submenu; stale generation), fold decision, fold split and separator cleanup,
icon-button layout and hit testing, keyboard moves between the row and the
list, and a widget test (fold, keyboard, Alt accelerator, click, tooltip,
chevron scroll after the fold). Two `#[ignore]` probes print the packaged
handlers and the real worker batches for a path (`TSUBAME_PKG_PROBE`).

### Not verified on the test desktop

- 100 % display scale (the test desktop runs 200 %); the layout is in DIPs
  and unit-tested.
- Mouse clicks and hover tooltips on the icon row (mouse input does not reach
  the remote desktop); covered by the widget test.
- Packaged submenus (`ECF_HASSUBCOMMANDS`): no installed handler on the test
  PC has one; covered by the model test only.
- A hung packaged handler: it would trip the 4 s watchdog like a hung classic
  handler: the shell rows go and the failure row with Retry shows (own rows stay).

## CM8: editing the menu

Release build of commit `41fcbf9` (exe 3,070,976 B, SHA-256 prefix
`d286386d193194f7`) against a release build of `10aa230` (CM7 + CM9, exe
2,974,208 B) on the remote test desktop (one 4K monitor at 200 %),
`TSUBAME_GPU_PARK=0`. Fixture: a folder with one subfolder and five `.txt`
files. The CM8 stress menus carry a fixed set of edits (one hidden and one
renamed native item, 40 remembered items), so the override pass runs on
every menu; the CM7 build ignores them.

### Menu timing (`--stress ctxmenu`, 200 menus, runs interleaved CM8, CM7, CM8, CM7)

| Metric | CM8 run 1 | CM8 run 2 | CM7 run 1 | CM7 run 2 |
|---|---|---|---|---|
| Warm first paint p50 / p95 | 1.51 / 1.87 ms | 1.48 / 1.97 ms | 1.47 / 1.89 ms | 1.46 / 1.81 ms |
| Warm top level p50 / p95 | 36.25 / 40.31 ms | 36.19 / 42.46 ms | 35.81 / 40.88 ms | 36.38 / 40.65 ms |
| Warm all items p50 / p95 | 45.90 / 50.72 ms | 45.96 / 53.25 ms | 45.37 / 51.16 ms | 45.39 / 51.23 ms |
| Cold first paint / top level / all | 2.8 / 810.5 / 887.2 ms | 2.7 / 565.8 / 580.9 ms | 2.4 / 567.4 / 583.3 ms | 2.5 / 572.9 / 589.8 ms |
| Failures | 0 | 0 | 0 | 0 |

The warm top level is within noise of CM7 on the same machine and day, and
under the CM7 figures above (38.38 / 44.78 ms). The first CM8 cold figure is
the first start of a freshly copied binary; the second CM8 run matches CM7.
Both builds measured about 570 ms cold that day, above the 477 ms recorded
for CM7, so the difference is the machine, not CM8.

### Growth over 200 menus

| | CM8 run 1 | CM8 run 2 | CM7 run 1 | CM7 run 2 |
|---|---|---|---|---|
| Private bytes, menu 10 -> 200 | +6.7 % | +7.3 % | +7.4 % | +6.9 % |
| Handles, menu 10 -> 200 | +5 | +2 | +6 | +20 |
| GDI / USER, menu 10 -> 200 | +0 / +4 | +0 / +0 | +0 / +2 | -1 / +3 |
| Private bytes end vs start | +21.9 % | +22.6 % | +22.2 % | +21.7 % |

CM8 grows no more than CM7. Both builds fail the harness's 5 % private-bytes
gates on this fixture (the CM7 build too, so it predates CM8); handles, GDI
and USER objects stay flat.

### Checked on the test desktop

Debug build, keyboard only, focused window checked before keys, a scratch
folder of 20 `.txt` files and one subfolder, config restored afterwards.

- Shift+F10 on a row of the open menu opens its edit menu beside the row:
  header with the source tag (Tsubame / Windows), Hide (Del), Rename (F2),
  Move up / down (Alt+arrows), Edit in Settings. A Windows item shows the
  note and greyed moves; an extra like "Copy name" greys the moves too.
- Del hides the row; the menu stays open and the strip
  "Hid “Copy as path” | Undo | Manage" shows at the bottom. End, Enter
  (Undo) brings the row back.
- F2 on "Add to Favorites" turns the row into a field; Enter saves
  "Favourite it", the row shows it at once, and the config has
  `name = "Favourite it"` on key `windows.verb:pintohomefile`.
- Alt+Up on "Copy as path" moves it one place up in the list (config order
  changed). Its neighbour Paste is folded into the icon row on that menu,
  so the visible list did not change.
- Edit in Settings closes the menu and opens Settings > Context menu with
  the search set to the row's name and the row selected.
- The first menu ever learned 23 items, none tagged New.
- Settings: groups with counts, chips, search across both lists, the
  learned-items card with source tags.

Found and fixed during the check: every Windows item of the first menu was
tagged New (the shell batch came after the Tsubame rows were learned); an
edit made the escape-hatch row read "Show more options"; with a search, the
learned-items card overlapped the preview.

### Tests

New unit tests: row keys (verb, label, packaged family + CLSID, `app:`
extras, a translated label keeps the key), overrides (hide, rename, no
reorder, submenus, unknown keys change nothing, CM7 CLSID keys still apply,
the escape hatch keeps its label), group order, learning (first menu tags
nothing, later ones tag new items once), config round trip and a bad item or
group dropped alone, the edit menu (mouse and keyboard hit testing, rename
field, Esc layering, strip Undo / Manage), move up / down limits, and the
editor's search, chips, counts and Reset.

### Not verified on the test desktop

- Mouse paths: right-click on a menu row, the eye toggles, double-click
  rename, the grip drags and the chips in Settings (mouse input does not
  reach the remote desktop); covered by widget and editor tests.
- The Menu key (same code path as Shift+F10, unit-tested).
- Shift+F10 while typing to filter: the edit menu is off while a filter is
  active, by design (filtered rows are flattened paths).
- Packaged rows (`windows.pkg:` keys): no packaged row showed on the test
  fixture; covered by the model test.
## CM8-skip A: editor kind switch, 3rd party, groups, Ctrl+F

Checked on the test desktop (slot build, keyboard only): Settings > Context
menu shows the kind switch, the five chips with counts (All 11, Windows 2,
3rd party 0, Tsubame 9, Hidden 0 on the fixture), the Ctrl+F badge in the
search field and the New group button. Not seen on screen: Ctrl+F itself, New
group, renaming and dragging (another window took the foreground and mouse
input does not register); those paths are covered by unit tests (group order
round trip, drop rules, chip rules, config partial load).

Config: `[[context_menu.user_groups]]` and `items.group` are new; an old file
loads unchanged and a bad group or item is dropped alone. Exe 5.42 MB. The menu
code that builds the right-click (and its timings) was not touched.

## CM8-skip B: user groups, Pin to top, refresh logic (menu side)

Release build on the remote test desktop (one 4K monitor at 200 %),
`TSUBAME_GPU_PARK=0`, same fixture as CM8 (a subfolder and five `.txt` files).
This build against the CM8 exe (`41fcbf9`), `--stress ctxmenu`, 200 menus,
runs interleaved new, CM8, new, CM8. The stress menus carry no group and no
pin, so this measures the fast path every menu pays (one `any` over the learned
items); the grouped path is a retain / partition per level and is covered by
unit tests only.

| Metric | new run 1 | new run 2 | CM8 run 1 | CM8 run 2 |
|---|---|---|---|---|
| Warm first paint p50 / p95 | 1.45 / 1.81 ms | 1.44 / 1.82 ms | 1.41 / 1.82 ms | 1.46 / 1.82 ms |
| Warm top level p50 / p95 | 31.83 / 35.66 ms | 31.71 / 35.25 ms | 31.51 / 35.89 ms | 31.90 / 35.09 ms |
| Warm all items p50 / p95 | 36.83 / 40.82 ms | 36.60 / 39.57 ms | 36.29 / 40.64 ms | 36.65 / 40.95 ms |
| Growth menu 10 -> 200 (private) | +6.7 % | +6.2 % | +7.3 % | +5.3 % |
| Failures | 0 | 0 | 0 | 0 |

Top level is within noise of CM8 (the machine ran faster than on the earlier
day, so the absolute figures are below the 36.2 / 40.3-42.5 ms recorded
then). Growth shows the same pre-existing failure of the 5 % private-bytes gate
as CM7 and CM8; handles +0.4..0.8 %, GDI +0, USER +1. Exe 3,136,512 B (< 6 MB).

### Checked on the test desktop

Release build, keyboard only (Shift+F10, arrows, Enter), foreground window
checked, a scratch folder with a throwaway data dir (`TSUBAME_DATA_DIR`), all
removed afterwards. Config: user group `g1` "Tools" at pos 1, learned item
`windows.verb:pintohomefile` in it, `app:x.terminal` pinned.

- The menu drew "Open terminal here" first in the Tsubame group, and "Add to
  Favorites" alone in its own separator-delimited block right after the
  Tsubame group (no header row).
- Shift+F10 on "Copy name": the edit menu shows the new "Pin to top" row under
  the disabled Move up / down. Enter on it pinned the row: "Copy name" moved to
  the top, the edit menu stayed open on it and now reads "Unpin"; the config
  gained `pin = true` for `app:x.copy-name`.

Only unit-tested: refresh from last menu (no button), Pin on packaged items,
pin inside a user group, deleting a group with pinned items.

## CM10: UI Automation provider and High Contrast

Release build `39d4983` on the remote test desktop, `TSUBAME_GPU_PARK=0`, a
scratch folder with a subfolder and five `.txt` files, throwaway data dir, all
removed afterwards. Exe 3,183,616 B (CM8-skip B exe 3,136,512 B; < 6 MB).

`--stress ctxmenu`, 200 menus, no screen reader attached (the provider costs
nothing here): warm first paint 1.46 / 1.91 ms (p50 / p95), warm top level
31.34 / 34.83 ms, warm all items 36.06 / 39.92 ms, growth menu 10 -> 200
private +7.0 % (handles +0.6 %, GDI +0, USER +1), failures 0. Previous build:
31.5-31.9 / 35.1-35.9 ms and +5.3..+7.3 %: no regression; the 5 % growth gate
still fails as before.

A UIA client (PowerShell, `System.Windows.Automation`) read the open menu
(Shift+F10, keyboard only, foreground checked): Menu "Context menu" with 25
MenuItem rows (names, `Alt+letter` access keys, shortcut text, IsEnabled,
rects in screen pixels, `expand=Collapsed` on the submenu rows, `invoke` on
every row), `Paste` disabled, `FocusedElement` followed Down. `Expand()` on
"PeaZip" added a child Menu "PeaZip" with 12 rows and the first one focused;
`Collapse()` closed it. `Invoke()` on "Copy name" ran the row: the clipboard
changed and the menu closed. Not exercised live: Toggle (no checked row in this
menu), the events (no event listener attached), scrolled-out rows.

High Contrast (High Contrast Black enabled through `SystemParametersInfo`,
restored right after): the menu drew white on black with white borders,
the selected row cyan with dark text, disabled rows grey, accelerators
underlined, shell icons unchanged; the rest of the window kept the app theme.
Unit tests check text-on-popup and text-on-selection contrast >= 7:1 for the
four Windows High Contrast colour sets.

Only unit-tested: snapshot building and request handling in the widget,
the Navigate tree and runtime ids of the provider, the widget-to-UIA mapping.

## CM8-skip C: Nilesoft import states and file picker

Unit-tested (`nss`): found / importable / skipped counts of a detected file, BOM
stripped, the file's bytes unchanged after detect, choices and keep-only
selection (submenus carry their children, stray separators dropped), report
undo removes exactly the added ids.

Not seen in the real app: the file dialog, the file-detected note and the
report card were built and compile, but the GUI check did not happen (no free
build slot for the whole session). The selection screen is not in the mock, so
only its logic exists.

## CM11: owner of the 200-menu private-bytes growth (2026-11-19)

Result: **the growth is one immortal thread per menu inside Microsoft's
Work Folders context-menu handler** (`C:\Windows\System32\WorkfoldersShell.dll`,
"Work Folders Context Menu Handler", CLSID `{E61BF828-5E63-4287-BEF1-60B1A4FDE0E3}`,
registered at `HKLM\SOFTWARE\Classes\*\shellex\ContextMenuHandlers\WorkFolders`,
apartment-threaded). Not our rows, caches, packaged scan or UIA.

### How it was found

Release build `efa7201`, rig, `--stress ctxmenu --dir <3 files>`, `TSUBAME_GPU_PARK=0`,
`TSUBAME_MEMDIAG=1`, runs interleaved, n = 3 each (gate B, menu 10 -> 200):

| Variant | Growth per run | Outside-heap allocs 10 -> 200 |
|---|---|---|
| default (lazy, packaged) | +6.5, +6.7, +7.1 % | 120 -> 308, 295, 311 |
| `TSUBAME_CTX_PACKAGED=0` | +7.3, +6.5, +6.4 % | 119 -> 308, 294, 311 |
| `TSUBAME_CTX_EAGER=1` | +7.2, +4.4, +6.6 % | 119 -> 299, 292, 298 |
| native `--stress menu` (OS-only path) | +8.0, +8.4, +7.7 % | 104 -> 295, 296, 293 |

Every variant, including the OS-only native menu, grows the same way: the
cause is below our row/packaged/lazy code. Rust live heap 0.62 -> 0.69 MB, the
process heap +0.4 MB. The growth is private memory **outside every heap**:
+190 allocations for 190 menus. The new `TSUBAME_MEMDIAG=1` tracker lists them
(`memdiag.<label>.since10`): 191 new allocations, each 1024 KB reserved /
16 KB committed, layout reserved-guard-committed = thread stacks, none freed.
It also counts live threads by start address (`memdiag.<label>.thread`):

| After menu | 10 | 100 | 200 |
|---|---|---|---|
| threads in process | 73 | 163 | 263 |
| of them starting at `WorkfoldersShell.dll+0xfa30` | 10 | 100 | 200 |

With `TSUBAME_STRESS_SETTLE_S=240` (stretches the settle) all 200 threads were
still alive 240 s after the last menu: they never exit.

Control: a per-user override on rig (`HKCU\Software\Classes\*\shellex\ContextMenuHandlers\WorkFolders`
default = null CLSID, removed after each run; registry verified clean at the end),
interleaved, n = 3:

| | handler on | handler off |
|---|---|---|
| Growth menu 10 -> 200 | +5.5, +7.0, +7.3 % FAIL | +0.7, +1.0, +0.7 % PASS |
| Threads menu 10 / 200 | 73 / 263 | 61 / 61, 63 / 63, 63 / 63 |

1000 menus (`TSUBAME_STRESS_MENUS=1000`, n = 1 each): handler on, private
73.2 / 75.2 / 99.9 MB at menu 10 / 100 / 1000 = +36.4 %, slope
**+2.7 MB per 100 menus** (about 27 KB and one thread per menu), no plateau;
handler off: 72.7 / 71.8 / 73.6 MB = +1.2 %, slope about +0.1 MB per 100 menus,
plateaued. Handles, GDI and USER stay flat in both.

So with the Work Folders handler out of the picture the 5 % gate passes with
room; with it in, the gate fails by the thread stacks alone.

### Fix status

None in our code: nothing of ours holds these threads (they start in the
extension, handles are flat, the native path leaks the same). The gate, its
code and thresholds are unchanged. Disabling the handler system-wide or for the
user (the control above) is not something the app should do to a user's shell.

### Helper-process estimate (not built)

A helper process that hosts the shell step (bind, `QueryContextMenu`, walk,
`InvokeCommand`) would be recycled every N menus, ending all leaked threads
with it. Estimate from the numbers above, not measured on a prototype:

- Leak bounded to N x 27 KB (N = 100: 2.7 MB, in the helper, not the app).
- Memory: shell-extension load for the first menu costs +9.7 MB private /
  +29 MB working set (start -> menu 1); the app sheds that and the helper
  carries it, plus an exe baseline of a few MB.
- Latency: a cold first menu is 540-590 ms against 33 ms warm, so the helper
  has to be pre-warmed (a standby helper started while idle) or each recycle
  costs a visible first-menu hit. A named-pipe round trip for a menu's rows
  (tens of rows, icons up to 256 x ~1 KB) should add well under 1 ms.
- Cost: a second exe or mode, an IPC protocol for rows, icons and invoke, a
  hung-helper watchdog (the pool watchdog already exists), and invoke
  dialogs owned by a window of the app.

Decision for the user: accept the failing gate as an OS defect (document and
keep), or build the helper. The master will ask.

Diagnostics added: `TSUBAME_MEMDIAG=1` now also reports allocations since menu 10
and threads by start module (code runs only under the env var);
`TSUBAME_STRESS_SETTLE_S=N` lengthens the stress settle (default 3 s).
