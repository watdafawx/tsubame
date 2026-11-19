# Own-drawn context menu (CM2, CM3, CM5)

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
line. Native top-level and full-tree batches append below them; stable row keys
retain focus and submenu state. Generation mismatches drop. Closing drops the
handle; replacing drops it before submitting another request. Retry creates a
new generation and discards old shell command ids. The pool replaces hung
workers at four seconds, with its existing eight-thread abandon cap. At three
seconds the loading state shows failure/Retry; late results may still complete.

The widget uses popup-layer hit testing, theme tokens, density/font metrics,
checked/radio/default/disabled/danger rows, shortcut text and custom accent dots.
Placement flips within client bounds with an eight-DIP keep-out. Tall menus use
chevron buttons. Mouse hover opens eagerly; keyboard supports arrows, Home,
End, Enter, Escape. A stationary pointer cannot steal keyboard focus. Alt or
keyboard opening reveals underlines. No shadows. Menu CPU icons cap at 256;
content hashes reuse device uploads in the renderer's existing bounded cache.
Only visible rows draw. A 33 ms animation timer exists only during loading;
closed and settled menus have no animation timer or repaint loop.

## Switches and measurement

- `TSUBAME_NATIVE_MENU=1`: native popup fallback.
- `TSUBAME_CTX_DELAY_MS=N`: worker delay before binding; use >4000 for watchdog
  failure. Debug/test use only; delay also affects Retry.
- `--stress ctxmenu --dir DIR`: 200 real own-menu open/paint/close cycles,
  alternating one and three files. Writes `s4-stress-ctxmenu.txt` beside exe:
  cold/warm first paint and complete-tree latency, failures, process counters.
- `--stress menu --dir DIR`: existing native comparison.
- `TSUBAME_STRESS_MENUS=N`: cycle count, clamped to 200..10000.

## Later-card seams

CM4: swallowed Char input for filtering/Alt accelerators; immediate hover branch
for 150/250 ms delays and safe triangle; wheel input, insertion misclick guard.
CM6: `Part::Submenu` and row paths allow lazy population (currently eager).
CM7: packaged discovery merges after classic shell rows, before custom entries.
CM8: U4 entry identities/overrides and stable row keys support editing.
CM9: fold common actions into icon row before existing chevron scrolling.
CM10: UI Automation provider and high-contrast verification remain separate.

The stress scenario is separate from navigation/tab leak-gate: shell extensions
have known per-menu growth; mixing 1000 menus into that gate would obscure the
navigation baseline. Compare 200-menu growth against the native scenario.
