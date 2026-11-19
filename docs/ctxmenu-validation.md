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
