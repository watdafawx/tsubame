# Config, session and the command line (D1)

Code: `crates/tsubame-config` (schema, files, atomic writes, debounce),
`src/cli.rs` (parsing), `src/session.rs` (capture/restore + the app's save
gates), `src/single.rs` (one instance per session). Tests:
`cargo test -p tsubame-config` (44) and the app's `cargo test` (CLI table and
session round trips included).

## Where the files are

| Mode | Config | Session |
|---|---|---|
| Normal | `%APPDATA%\Tsubame\config.toml` | `%APPDATA%\Tsubame\session.toml` |
| Portable | `<exe folder>\data\config.toml` | `<exe folder>\data\session.toml` |
| `--config <file>` | that file | still `session.toml` in the data folder |
| `TSUBAME_DATA_DIR` (env, tests and support) | `<dir>\config.toml` | `<dir>\session.toml` |

Portable mode is on when either marker file exists next to the exe:
`tsubame.portable` (empty; the app's own marker) or `portable.txt` (the
updater's marker, `docs/updater.md`), or with `--portable`.

Tags (P4.F6) live in `tags.db` next to `config.toml` (so `--config` and
portable mode move them too), with `tags.db.bak` (last clean state),
`tags.db.lock` (only one process writes; a second one shows tags read-only)
and, after damage, `tags.db.bad` kept aside. Export and import in
Settings > Tags use `tags-export.json` in the same folder. Files themselves are
never touched: no sidecar files, no NTFS streams.

Per-folder views (B11-G) live in `folder-views.tsv` next to `session.toml`
(the data folder; portable mode moves it too). One line per folder, tab
separated: `mode`, grid size, sort column, descending (`0`/`1`), then the
folder (lower case, no trailing backslash); a `default` line holds the sort of
the global default. At most 2,000 folders, the least recently used one is
dropped first; lines are in use order, oldest first. It is read once at startup
on a worker thread and written lazily on a worker thread, riding the config's
2 s save gate and the window close, only when a view changed. Tree and Miller
views, archives, virtual folders and the Settings tab are never stored. The
file is safe to delete (every folder then shows the default) or to hand edit;
a bad line is skipped. Commands: `view.reset-folder-views` forgets every
folder, `view.apply-to-all-folders` makes the current view the default and
forgets the folders. The global default (`appearance.view_mode`,
`view.grid_size`) stays what the config says, not whatever folder was open last.

Nothing is written until something changed. The first start with no file
writes nothing; the file is created on the first change (theme, density,
zebra, view mode, sidebar) or when the window closes with a geometry worth
saving.

### Writes

- Atomic: temp file in the same directory, `flush` + `sync_all`, then
  `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)` through `std::fs::rename`.
  A crash leaves the old file or the new one, never half of each.
- Debounced: at most one write per 2 s (`Debounce`, the app arms a one-shot
  timer for the remaining time only while a change is pending). The window
  close/exit flushes whatever is still inside the window.
- No write when the rendered contents equal the last write.
- A file that is not TOML at all (syntax error, not UTF-8) is moved to
  `<name>.bad` and the defaults are used; the app never fails to start. The
  reason names the line and column (`line 2, column 17: ...`).
- A file that is valid TOML but has a wrong-shaped value (a number where a
  string belongs, `pinned = "x"`, one bad `[[sidebar.pinned]]` entry) is **not
  moved**. Only the failing key (or array entry) is dropped and uses its
  default; every other setting is kept. The original text is copied to
  `<name>.bad` first, and the reason is printed to stderr as
  ``sidebar.pinned[1] (line 9): invalid type: integer `5`, expected a string``.
  The next save writes a clean file.
- A Windows path in a double-quoted string (`path = "D:\work"`) is invalid
  TOML (`\w` is not an escape). The loader fixes the backslashes, keeps the
  copy as `.bad` and reports it. Write paths in single quotes (`'D:\work'`) or
  double the backslashes (`"D:\\work"`) to avoid the warning. (Careful:
  `"D:\temp"` is valid TOML and means `D:<tab>emp`.)
- Unknown keys are kept: the file is parsed into the typed schema and the
  raw table is remembered; saving merges keys this build does not know back
  in, under the keys it does. A newer Tsubame's settings survive an older
  build, and the typed values always win.
- A file with `version` newer than this build is read best effort and **never
  written back** (the app runs without persisting settings until a newer build
  is used again).
- `version = 1` is the current schema. Migrations are registered in
  `MIGRATIONS` (`fn(&mut toml::Table)` per version step) and run on load
  before the typed parse; the file is stamped with the current version.

## `config.toml`

Every key is optional; a missing key keeps its default. Sections missing
entirely are the same as all-defaults. Values are case-insensitive and an
unknown enum value falls back to its default instead of making the file
corrupt.

```toml
version = 1

[general]
start_folder = "last"        # "last", or a path like "D:\\Downloads"
restore_session = true       # restore tabs at startup when no path was given
show_hidden = false
show_system = false
confirm_delete = true
delete_to_recycle = true
row_tooltips = true          # hover tooltip on Details / Grid rows after ~600 ms
show_sidebar_onedrive = true # show OneDrive in Quick access when installed
show_sidebar_wsl = true      # show Linux (WSL) section when WSL is installed
show_sidebar_recycle = true  # show Recycle Bin in Quick access
remember_folder_views = true # each folder keeps its own view mode, icon size and sort (folder-views.tsv in the data folder); false: one view for every folder

[appearance]
theme = "tsubame-dark"       # "tsubame-dark" | "tsubame-light" ("dark"/"light" too)
density = "compact"          # "compact" (22 px) | "normal" (26 px) | "comfortable" (30 px)
zebra = true
font_scale = 1.0             # list and menu text size, 10-18 px at 12 px = 1.0 (Settings > Appearance)
accent = "#e0492c"           # optional; absent = the theme's accent
view_mode = "details"        # "details" | "grid-small" | "grid-large"

[view]
grid_size = 96               # grid cell width in DIPs, 48-256 in steps of 8 (Ctrl+wheel, Ctrl+Plus / Ctrl+Minus)
type_ahead_filters = false   # true: typing in the list starts the filter (old behaviour); false: typing jumps to the first name that starts with it
show_extensions = true       # false: file names are drawn without their extension (rename still edits the full name; sorting uses the real name)
group_by = "none"            # none | name | type | date | size: how a new tab groups its files (Group by in the right-click menu changes it per tab; the choice is kept in the session). See docs/grouping.md

[columns]
order = ["core.size", "core.modified"]  # Details column order after Name (drag a header cell); absent or empty = default order. Name is always first and not listed. "Reset columns" in the header menu clears it.

[panes]
dual = false
split = "vertical"           # "vertical" = side by side | "horizontal" = stacked
ratio = 0.5                  # size of the first pane, 0.15-0.85
sidebar_width = 208.0
sidebar_collapsed = false    # true = 48 px icon rail

[preview]
enabled = false              # docked right preview pane (Alt+P)
width = 320                  # pane width in DIPs, 220-600 (double-click splitter resets to 320)
quick_look = true            # Space / F3 Quick Look overlay
syntax_highlight = true      # 6-token syntax tinting in text/code preview
hex_auto_width = true        # 8 bytes/row in docked panes < 420 px, 16 bytes/row otherwise
direct_hex_binaries = true   # show hex directly for .exe/.dll/.bin/.dat

[sidebar]
closed = ["network"]         # folded sections: pinned | quick | recent | pc | network | tags | linux

[[sidebar.pinned]]           # bookmarks, in order; drag between rows to pin
name = "Work"
path = 'D:\work'            # single quotes: backslashes are literal

[updates]
auto_check = true            # background check at most once per 24 h
auto_install = false         # download in the background; it still installs only at restart
channel = "stable"           # "stable" (default) or "nightly" (test builds, see TESTERS.md)

[keys]
profile = "default"          # read, not used (no profiles yet)

[keys.overrides]             # command id -> up to 2 chords; [] = unbound
"view.refresh" = ["F5", "Ctrl+R"]

[[context_menu.entries]]     # see docs/u4-settings.md; empty list = default menu
id = "custom.1"
name = "Open in VS Code"
command = "C:\\Program Files\\Microsoft VS Code\\Code.exe"
args = "\"{path}\""
extensions = []
hidden = false
source = "custom"

[context_menu]
groups = ["tsubame", "windows", "apps", "custom"]   # menu order; missing groups follow

[[context_menu.items]]       # Windows', apps' and Tsubame's rows outside the list (CM8)
key = "windows.verb:windows.modernshare"
label = "Share"               # first label seen (Settings shows it)
from = "Windows"              # Windows | Tsubame | a package family
hidden = true
name = ""                     # the user's label; empty = the item's own
new = false                   # appeared since Settings > Context menu was last opened

[advanced]
diagnostics = false          # persists; the log writer is not wired yet

[extensions]                 # docs/extensions.md
disabled = ["tsubame.column.type"]   # extension ids switched off; unknown ids are kept

[window]
x = 100
y = 100
w = 1120
h = 720
maximized = false
```

| Key | Default | Used by |
|---|---|---|
| `general.start_folder` | `"last"` | startup folder when no path and no session |
| `general.restore_session` | `true` | restore tabs when the command line names no path |
| `general.show_hidden` / `show_system` | `false` | hide rule of every tab; Settings > General, Ctrl+H (live) |
| `general.confirm_delete` / `delete_to_recycle` | `true` | Delete key: ask first / Recycle Bin vs permanent (Shift+Delete does the other one) |
| `general.row_tooltips` | `true` | Details and Grid row hover tooltip (~600 ms dwell: Name, Type, Size, Date modified; folders omit Size); Settings > General (live) |
| `general.show_sidebar_onedrive` / `show_sidebar_wsl` / `show_sidebar_recycle` | `true` | show or hide OneDrive, the Linux (WSL) section, and Recycle Bin in the sidebar; Settings > General (live) |
| `general.remember_folder_views` | `true` | each folder keeps its own view mode, icon size and sort; a folder with no memory shows the default view (Settings > General, live) |
| `columns.order` | `[]` | Details column order after Name, as ids (`core.modified`, `core.type`, `core.size`, `core.git`, `core.tags`, or an extension column's id). Drag a header cell to reorder; "Reset columns" (header menu, `view.reset-columns`) clears it, resets the widths and hides the Tags column again (`tags.column` default). Unknown ids are ignored; columns not listed follow in the default order |
| `appearance.theme` | `"tsubame-dark"` | `View::dark`, Ctrl+Shift+L; Settings > Appearance picks any of the 19 themes (live) |
| `appearance.density` | `"compact"` | row height, Ctrl+Shift+D, Settings > Appearance (live) |
| `appearance.zebra` | `true` | alternating rows, Ctrl+Shift+Z, Settings > Appearance (live) |
| `appearance.font_scale` | `1.0` | text size slider on Settings > Appearance (live) |
| `appearance.accent` | absent | accent swatches on Settings > Appearance (live); absent = the theme's own |
| `appearance.view_mode` | `"details"` | `View::view_mode` (grid arrives with W2) |
| `panes.dual` / `split` | `false` / `"vertical"` | two panes, side by side or stacked (Ctrl+\, Ctrl+Shift+\) |
| `panes.ratio` | `0.5` | first pane's share, clamped 0.15–0.85; the splitter drags it, double-click resets |
| `panes.sidebar_width` | `208.0` | sidebar width, clamped 120–480; dragging under 120 collapses to the rail |
| `panes.sidebar_collapsed` | `false` | `true` = the 48 px icon rail (Ctrl+B) |
| `preview.enabled` | `false` | docked right preview pane (Alt+P, Settings > General, live) |
| `preview.width` | `320` | docked preview pane width in DIPs, clamped 220–600; double-click splitter resets to 320 |
| `preview.quick_look` | `true` | `Space` / `F3` centered Quick Look overlay (Settings > General, live) |
| `preview.syntax_highlight` | `true` | 6-token syntax tinting in text/code preview (Settings > General, live) |
| `preview.hex_auto_width` | `true` | auto-switch hex dump to 8 bytes/row in docked panes narrower than 420 px (Settings > General, live) |
| `preview.direct_hex_binaries` | `true` | show hex directly for `.exe`/`.dll`/`.bin`/`.dat` instead of an Unsupported card (Settings > General, live) |
| `file_age.enabled` | `false` | file-age colouring (built-in extension `tsubame.ext.file_age`); status-bar key and `view.age_colours` toggle it (live). Off = Details looks as before |
| `file_age.style` | `"text"` | `text` (tints the Modified date), `chip` (tinted rect behind the date), `accent` (3 px bar on the row's left edge). Unknown value = `text` |
| `file_age.palette` | `"heat"` | `heat`, `colour_blind` (blue/yellow safe) or `mono` (grey steps, bold for the newest). Unknown value = `heat` |
| `file_age.pips` | `false` | four small bars before the date (4 = today, 0 = older). Always on for `colour_blind` and `mono` |
| `miller.col_width` | `220` | Miller columns: default column width in px (140 to 480). Columns also drag-resize per session (double-click on the edge resets). Settings > Extensions > Miller columns has a slider (steps of 10, applied live) and a Default button |
| `tree.expand_cap` | `500` | Tree rows: recursive expand (`Shift+Right`, `*`) stops after this many folders (1 to 5,000); the status bar says so. Never follows junctions or symlinks |
| `tree.child_cap` | `5000` | Tree rows: children listed per expanded folder, then a "Showing first 5,000 of N" row (100 to 50,000) |
| `tree.pane` | `false` | folders-only Tree pane left of the list (`Shift+Alt+T`, command `pane.tree`; a 248 px drawer below 560 px) |
| `tree.pane_width` | `260` | Tree pane width in px (160 to 480) |
| `file_age.today` | `"midnight"` | `midnight` = since local midnight (DST-safe), `last24h` = the last 24 hours |
| `file_age.week_days` / `month_days` / `year_days` | `7` / `30` / `365` | bucket limits in days, inclusive (a file exactly on a limit is in the newer bucket). Must grow (`week < month < year`), 1–36500; an invalid set is ignored as a whole and the defaults apply (Settings > Extensions keeps the last valid values and says why) |
| `file_age.folders` | `false` | tint folder dates too. A folder's date changes only with its direct children, so it can mislead |
| `extensions.disabled` | empty | ids of built-in extensions switched off, e.g. `["tsubame.ext.file_age"]` removes the age key and command |
| `sidebar.pinned` | empty | `[[sidebar.pinned]]` `name` + `path` (both strings; write the path in single quotes, `path = 'D:\work'`), shown in the Pinned section. Extra keys in an entry are ignored |
| `sidebar.closed` | empty | sidebar sections folded by the user |
| `updates.auto_check` | `true` | background check at start (<= 1 per 24 h). `false` = no network from the automatic path |
| `updates.auto_install` | `false` | download without a click; installing is still only at restart |
| `updates.channel` | `"stable"` | `"stable"` or `"nightly"`; anything else means stable. Nightly also follows stable releases and never downgrades when switched back (`docs/updater.md`, "Channels"). Settings > Updates > Update channel; applied at once |
| `keys.overrides` | empty | rebound shortcuts, applied live; Settings > Keys, format in `docs/u4-settings.md` |
| `keys.profile` | `"default"` | read, not wired |
| `context_menu.entries` | empty | what Tsubame adds to the native context menu (`docs/u4-settings.md`) |
| `context_menu.items` | empty | hide / rename of the menu's other rows, keyed by `key`, never by label: `windows.verb:<verb>` (classic item with a canonical verb), `windows.label:<label>` (classic item without one), `windows.pkg:<package family>/{clsid}` (packaged Win11 item), `app:<action>` (Tsubame rows such as Copy name). Learned from real menus (at most 400; the oldest unedited ones go first); Settings > Context menu and the menu's own edit menu change them. A wrong-shaped item is dropped alone. Old `source = "windows"` entries in `entries` (and CM7's `windows.verb:{clsid}` keys) are still honoured, read only |
| `context_menu.groups` | empty | group order, top to bottom: `tsubame`, `windows`, `apps`, `custom`; duplicates are dropped, missing ones follow in that order, an unknown value is dropped alone |
| `tags.shapes` | `true` | each tag colour also has its own shape (circle, square, triangle ...); `false` draws plain dots |
| `tags.column` | `false` | the Details Tags column (168 px, two chips then +N); tags after the name and in Grid show either way. "Reset columns" switches it back to this default (off); a second window that cannot take the tags lock reads them read-only and says so in the status bar instead of a banner |
| `advanced.diagnostics` | `false` | persists; no log writer behind it yet |
| `window.*` | `0,0,1120,720,false` | geometry on a plain start |

Density naming: the UI kit's `Density` has three presets (22/26/30 px), the
config calls them `compact`/`normal`/`comfortable`. `spacious` is accepted as
an alias for `comfortable`; `--density` keeps the kit spelling.

Window geometry is made monitor-safe on load: the size is clamped to at least
640×400 and a window that is no longer visible on any monitor's work area
(monitor disconnected, resolution changed) is centred on the primary monitor.
The `[window]` section is a fallback; a restored session's geometry wins.

Theme on a first-ever start (no `config.toml`): the Windows "apps" theme.
After that the file decides, so the theme no longer flips when the system
theme changes unless the user changes it in Tsubame.

## `session.toml`

```toml
version = 1
last_folder = "C:\\Users\\me\\Documents"
active_pane = 0

[window]
x = 0
y = 0
w = 1920
h = 1040
maximized = true

[[panes]]
active_tab = 1

[[panes.tabs]]
path = "C:\\Users\\me\\Documents"
view_mode = "details"
sort_column = "name"        # name | size | modified | type
sort_descending = false
scroll_first_row = 0        # index of the first visible row
scroll_first_name = "a.txt" # optional, informational
selected = ["a.txt"]        # optional, entry names, at most 500
expanded = ["C:\Users\me\Documents\src"]  # optional, Tree rows: expanded folders (full paths, at most 256)

[[panes.tabs]]
path = "D:\\Downloads"
view_mode = "details"
sort_column = "size"
sort_descending = true
scroll_first_row = 40
```

With two panes there are two `[[panes]]` tables (first pane, second pane);
`active_pane` is the one with focus and `last_folder` is its active tab.
`recents` (top level, newest first, at most 8 folders) feeds the sidebar's
Recent section and is written with the rest of the session on exit.

Only the active tab's folder is listed at startup. Every other tab is restored
lazily: its path and view state are kept, and its enumerator starts the first
time the tab is shown. That keeps the first paint at one folder no matter how
many tabs were open.

Restore rules:

- Only when `restore_session = true`, the file exists and parses, and no path
  was given on the command line.
- A folder that no longer exists falls back to its nearest existing parent
  (checked for local drive paths only; a dead network share is opened as saved
  and the enumerator reports the error, so startup never blocks on it).
- Sort, scroll row and selection are applied when the tab's listing completes;
  selection is matched case-insensitively by entry name, and unknown names are
  dropped.
- The session is written on window close/exit, never while the user browses.
  Nothing is written when the state did not change.

## Command line

```
tsubame [options] [path ...]
```

| Argument | What |
|---|---|
| `<path>` | a folder opens; a file opens its folder and selects it. Several paths open as tabs (the first in the current tab unless `--new-tab`). A path that no longer exists opens its nearest existing parent. |
| `--new-tab` | the paths become new tabs; the first tab shows the usual start folder |
| `--new-window` | do not forward to the running instance; start a separate window |
| `--select <path>` | open the folder of `<path>` and select the entry |
| `--portable` | use `<exe folder>\data\` for config and session |
| `--config <file>` | read and write this config file |
| `--thumb-workers <n>` | threads that load thumbnails; default 2, `0` shares the icon threads, values above 16 are clamped to 16. A missing or non-numeric value is reported on stderr and ignored |
| `--help`, `-h` | print usage and exit |
| `--version`, `-V` | print the version and exit |
| `--dir <path>` | old spelling of a folder to open (kept for the benches) |

`--help`/`--version` attach to the parent console (`AttachConsole`), print, and
exit — the exe is a GUI-subsystem app and has no console of its own.

An unknown option (`--nonsense`, `-x`) is never a path: it is ignored with a one-line warning on stderr (`tsubame: unknown option --nonsense; ignored`) and the launch carries on, so no tab is opened for it. Options that take a value (`--thumb-workers 20`, `--view grid-small`, `--theme`, `--workers`, ...) consume it, so the value is not mistaken for a folder.

Path parsing: surrounding quotes are dropped, `/` becomes `\`, `.`/`..` are
resolved, relative paths use the working directory, `C:` is `C:\`, trailing
separators are dropped (except on drive/share roots), and UNC (`\\server\share`)
and `\\?\` (including `\\?\UNC\`) paths keep their form. The parsing is a
table of 30+ cases in `src/cli.rs`.

Measurement flags keep working and are never forwarded to a running instance:
`--bench`, `--timing`, `--leak-gate`, `--leak-smoke`, `--stress`, `--hang`,
plus their options (`--workers`, `--timing-dirs`, `--reps`, `--gate-*`).
A dev run uses the default appearance, the folder from the command line, and
writes no config or session, so measurements are not polluted by a user's
settings.

## One instance per session

`Local\Tsubame.SingleInstance.v1` (per logon session) plus a hidden window of
class `TsubameInstance.v1`.

1. The first start owns the mutex and creates the hidden window (before the
   main window, so launches during startup are queued).
2. A second start opens the mutex, gets `ERROR_ALREADY_EXISTS`, finds the
   hidden window (up to 500 ms), calls `AllowSetForegroundWindow(ASFW_ANY)`
   and sends its paths with `SendMessageTimeoutW(WM_COPYDATA, ...)`. The
   payload is versioned (`TSB1`, version 1, magic + flags + UTF-8 paths), so a
   different build's payload is ignored instead of misread. The second process
   then exits without ever creating a window (a few milliseconds).
3. The first instance restores/activates its window, brings it to the
   foreground, and opens each forwarded path: the tab already showing that
   folder is focused, anything else becomes a new tab (and a forwarded file is
   selected once listed).
4. If there is no hidden window, the receiver is hung (`SendMessageTimeoutW`
   gives up after 500 ms), the mutex is unavailable, or `--new-window` was
   given, the launch starts normally without owning the instance.

## Measured (D1, 2026-11-18, release on the benchmark machine unless stated)

| Metric | Value |
|---|---|
| `config.toml` load | 0.075 ms per load (typical 604-byte file, 200 loads, test profile; release is faster). Total startup cost is a config load + a session load, both well under 0.2 ms |
| Main exe, release, before / after D1 | 664 KB (680,000 B) / 1,083,904 B = 1.03 MB (+~404 KB, +59%; `toml` + `serde` + the new code). Budget < 6 MB |
| Cold start to first painted frame (release, 3 runs, 100k fixture, 4K) | 65.1 / 69.9 / 59.5 ms (median 65.1, budget < 100). These runs have no session file; a session load adds ~0.1 ms and only the active tab is listed |
| Scroll paint p95 (same runs) | 2.86 / 2.94 / 2.90 ms (budget < 5) |
| Idle CPU, 10 s | 0 ms (the save timer exists only while a change waits out its 2 s window) |
| Leak gate, small counts (`-Quick`: 300 navigations, 30 tabs) | PASS: private +1.21%, handles/GDI/USER/threads unchanged |
| Second launch forwarding (release) | kernel process lifetime `ExitTime - StartTime` = **17.25 ms**, no window, no second process; wrapper (PowerShell `Start-Process`) 92 ms |
| Session restore after a forwarded launch | saved session had 2 tabs (`C:\Windows`, `C:\Windows\System32`), `active_tab = 1`, restored and closed with both tabs intact |
| Three paths on the command line | 3 tabs saved in order; `--select C:\Windows\win.ini` saved `selected = ["win.ini"]` |
| Portable mode | marker next to the exe wrote `<exe dir>\data\config.toml` and `session.toml` |
| `--help` / `--version` | printed via the parent console and to a redirect (`> file`), 1,168-byte help |

Exe size impact is the TOML parser plus its serde glue; it is a one-time
~400 KB against the 6 MB budget, and the crate is only linked into the app
(not the wasm host or the updater).

