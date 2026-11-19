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

[appearance]
theme = "tsubame-dark"       # "tsubame-dark" | "tsubame-light" ("dark"/"light" too)
density = "compact"          # "compact" (22 px) | "normal" (26 px) | "comfortable" (30 px)
zebra = true
font_scale = 1.0             # list and menu text size, 10-18 px at 12 px = 1.0 (Settings > Appearance)
accent = "#e0492c"           # optional; absent = the theme's accent
view_mode = "details"        # "details" | "grid-small" | "grid-large"

[panes]
dual = false
split = "vertical"           # "vertical" = side by side | "horizontal" = stacked
ratio = 0.5                  # size of the first pane, 0.15-0.85
sidebar_width = 208.0
sidebar_collapsed = false    # true = 48 px icon rail

[sidebar]
closed = ["network"]         # folded sections: pinned | quick | recent | pc | network

[[sidebar.pinned]]           # bookmarks, in order; drag between rows to pin
name = "Work"
path = 'D:\work'            # single quotes: backslashes are literal

[updates]
auto_check = true            # background check at most once per 24 h
auto_install = false         # download in the background; it still installs only at restart
channel = "stable"           # read; the updater is pinned to stable

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

[advanced]
diagnostics = false          # persists; the log writer is not wired yet

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
| `sidebar.pinned` | empty | `[[sidebar.pinned]]` `name` + `path` (both strings; write the path in single quotes, `path = 'D:\work'`), shown in the Pinned section. Extra keys in an entry are ignored |
| `sidebar.closed` | empty | sidebar sections folded by the user |
| `updates.auto_check` | `true` | background check at start (<= 1 per 24 h). `false` = no network from the automatic path |
| `updates.auto_install` | `false` | download without a click; installing is still only at restart |
| `updates.channel` | `"stable"` | read, not wired (updater pinned to stable) |
| `keys.overrides` | empty | rebound shortcuts, applied live; Settings > Keys, format in `docs/u4-settings.md` |
| `keys.profile` | `"default"` | read, not wired |
| `context_menu.entries` | empty | what Tsubame adds to the native context menu (`docs/u4-settings.md`) |
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

