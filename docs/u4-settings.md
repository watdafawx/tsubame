# U4: command palette, rebindable keys, Settings tab

What shipped, where it lives, what it costs, and what it does not do (yet).
Mocks: `mocks/mock-palette.html`, `mock-settings.html`; shots `mocks/shots/u4-*.png`.
Implementation shots (benchmark machine, 4K at 200%, scaled): `docs/shots/u4-impl-*.png`.

## Design in one page

| Piece | File | Notes |
|---|---|---|
| Key map | `src/keymap.rs` | defaults from the `COMMANDS` table (`src/commands.rs`), overrides from `[keys.overrides]`, published to a global (`set_current`) so labels everywhere show the current binding |
| Fuzzy matcher | `src/fuzzy.rs` | subsequence match with word-start / run bonuses, returns the matched letters (palette highlights them) |
| Palette | `src/palette.rs` | pure row builders (`command_rows`, `path_rows`, `tab_rows`, filter rows) + the painted popup |
| Settings | `src/settings/` | `mod.rs` (nav, scroll, find), `lay.rs` (rows/cards kit), one file per page |
| View glue | `src/view_u4.rs` | opens/closes both, applies `C_*` change bits live, themed() |
| Custom menu entries | `src/custom_cmd.rs`, `src/shellops.rs` | argv builder, spawn thread, extras appended to the W1 native menu |
| Updater wiring | `src/updates.rs` | `tsubame-update` behind a small state machine, see `docs/updater.md` |
| Default file manager | `crates/tsubame-win/src/defaultfm.rs`, `crates/tsubame-handler` | re-exports the shared registry logic (trait + fake) from the handler crate, real HKCU writer; the shim itself |
| Config schema | `crates/tsubame-config` | `[general] [appearance] [updates] [keys] [context_menu] [advanced]`, `nss.rs`, `menu.rs` |

Lifetime: the palette is a `Box<Palette>` created when it opens and dropped when it closes. The
Settings page state is a `Box<Settings>` created with the Settings tab and dropped when the tab
closes. Closed = zero cost. `View::churn_u4` (open + drop both) runs in `--leak-smoke` and in every
tab cycle of `--leak-gate`, and a unit test runs it 1000 times.

Idle: neither piece owns a timer. The palette and Settings repaint only on input. The caret blink
timer exists only while a text field has focus (`ui.mark_text_focus`) and stops after ~10 s.

### Command palette (Ctrl+Shift+P, also Ctrl+K)

- Opens as a popup layer, no scrim, Esc closes, the opening chord closes it too.
- Empty query: recent commands first (last 8 run from the palette), then every command by category.
- Fuzzy match over command names; the best match is first, matched letters are highlighted.
- Shortcut text on the right is the *current* binding (rebinding updates it).
- Prefix modes (type the character first; Backspace on an empty box leaves the mode):

| Prefix | Mode | Enter | Ctrl+Enter |
|---|---|---|---|
| `>` (default) | Commands | run, close | run, keep open |
| `/` | Go to path (completes folders, Tab = next) | go | open in new tab |
| `#` | Filter this folder (live: the list filters while you type) | keep the filter, close | |
| `@` | Open tabs of both panes (Alt+1..9, Del closes) | switch | |

- Tab on a command row = "Show shortcut": opens Settings > Keys with that command searched and its
  row focused (Enter records a new chord).
- Mouse: click a row = Enter; chips switch mode.

### Key map

Format, in `config.toml`:

```toml
[keys.overrides]
"tab.new"       = ["Ctrl+N", "Ctrl+T"]   # up to 2 chords
"view.refresh"  = []                      # empty list = unbound
"file.rename"   = ["F2"]                  # same as the default is dropped on save
```

- Keys are the stable command ids of the table (`palette.open`, `settings.open`, `view.hidden`, ...).
  The Keys page lists them all with their category.
- Chord text: `Ctrl+`, `Shift+`, `Alt+` in that order, then the key: a letter/digit/punctuation, `F1`..`F24`,
  `Enter`, `Tab`, `Space`, `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`,
  `Up`/`Down`/`Left`/`Right`. `parse_chord` and `chord_text` round-trip (unit test).
- Rejected as a binding (`keymap::bindable`): a plain letter, digit, punctuation or Space (also with only Shift:
  it would eat typing), Esc / Enter / Tab / arrows without a modifier (the lists and fields need them), and
  `Alt+F4` (Windows owns it). `Backspace`, `Delete`, `F1`..`F24`, `Home`, `End`, `PageUp`, `PageDown` alone are fine.
- Unknown command ids and unparsable chords in the file are skipped (kept in the file untouched), a chord
  claimed twice in the file goes to the first command in table order.
- Keys page: search, click a row (or Enter on it) to record. The recorder shows the pressed chord live,
  Enter accepts, Esc cancels, Backspace clears the slot. If another command already owns the chord the
  recorder shows who, and accepting assigns it here and unbinds it there ("assign here, unbind there").
  Reset per row (small arrow) and "Reset all keys" (asks twice). Everything applies live
  (`KeyMap::bind_all` re-registers the shortcuts) and is written by the debounced saver.

### Settings tab (Ctrl+,)

A normal tab (title "Settings") in the active pane. Single instance: opening it again, from either
pane, focuses the existing one. It is not saved in the session. Every control applies the moment it is
changed: a page edits `View::prefs` (the master copy of the config) and ORs a change bit
(`C_APPEARANCE`, `C_ZEBRA`, `C_HIDE`, `C_KEYS`, `C_UPDATES`, `C_MENU`, `C_OTHER`) into `Cx::changed`;
`View::apply_prefs` makes each bit live and raises `config_dirty`, which wakes the debounced atomic
config writer. A "Find a setting" box in the nav filters the page list by title and keywords.

| Page | Contents |
|---|---|
| General | start folder, restore session, show hidden, show system, confirm delete, delete to recycle bin |
| Appearance | colour mode (Match Windows / Light / Dark), 19 themes with previews, accent colour (9 swatches + theme default), row density with live previews (Compact 22 / Normal 26 / Comfortable 30 px), zebra, font size slider |
| Context menu | search, filter chips, group order, the U4 list (eye, rename, drag to reorder, add custom entry, Nilesoft import), the items learned from real menus (eye, rename, New), reset to defaults |
| Keys | see above |
| Updates | version, channel, last checked, state line (see below), Check now, Download and restart, Roll back, "Check automatically" and "Install updates automatically" (off) |
| Default file manager | opt-in state, switch, Repair, Remove (see below, `docs/default-file-manager.md`) |
| Advanced | open config folder, reset all settings (asks first; the old file is kept as `config.toml.bak`), diagnostics toggle, memory readout, portable indicator |

Density naming: config `compact/normal/comfortable` = kit `Compact/Comfortable/Spacious` = 22/26/30 px.

### Context menu editor

Settings > Context menu edits the own-drawn menu (`docs/ctxmenu.md`; mock state 6b). Top to bottom:

- the approved note that Windows' items are only hidden in Tsubame, never removed from the system;
- search (name, original label, source) and the chips All / Windows / Tsubame / Hidden;
- Add custom entry, New submenu, Add separator, Import from Nilesoft Shell…, Reset to defaults (asks
  once; drops every hide, rename, reorder and the group order, keeps the learned items);
- the kind switch (File / Folder / Empty area: the entries whose `show_for` covers it) above the search;
  chips All / Windows / 3rd party / Tsubame / Hidden with counts ("3rd party" = a packaged item whose
  package family is not Microsoft's); Ctrl+F focuses the search;
- Groups: Tsubame, Windows, Installed apps, Custom and the user's own (New group; double-click renames, x
  deletes and frees its items). Every header is a Tab stop: F2 / Enter rename, Delete deletes (own groups),
  Alt+Up / Alt+Down move it (any group; the kit's `Ui::claimed` keeps Alt+Up from also running "Up one
  folder"). With a grip to reorder (`context_menu.groups`, `context_menu.user_groups`);
  the grip on a learned item drags it into a group (`items.group`);
- the U4 list (Tsubame's built-ins and the custom / imported entries): grip (off while filtered), eye,
  double-click rename ("— was …" for a renamed built-in), source tag, × on custom rows;
- the items learned from real right-clicks (`[[context_menu.items]]`): eye, double-click rename
  ("— was …"), where it comes from, New for items that appeared since the last visit (the flags clear when
  the page opens). No grip: Windows' items keep their native order;
- the preview of the U4 list and the "How the list is filled" note.

Gap states (mocks approved 2026-11-20, `docs/ctxmenu-mock.md` "Gap states"):

- Row "...": the last control of every row except separators, a Tab stop (Enter / Space / Shift+F10 open).
  The popup is the kit's edit menu (`edit_only`), the same list as the menu-side Shift+F10 edit menu:
  Hide/Show, Rename, Pin to top/Unpin, Move to group > (flyout: native place, groups, New group), Move up,
  Move down; custom rows also Edit entry and Delete entry. Inapplicable rows are disabled with a one-line
  note, never hidden (`src/rowmenu.rs`, pure model, unit tested). F2 / Delete / Alt+Up / Alt+Down work on the
  focused "...". Delete entry shows an Undo line. On the menu side Del stays Hide and Delete entry has no key.
- Refresh from last menu: second toolbar row between Import and Reset, acts on `ctxui::refresh_learned` for the
  selected kind. States: idle ("Last menu: HH:MM, N items"), running (drawn only after 150 ms),
  "N items updated", "Already up to date", disabled with a tooltip until a menu was shown.
  `TSUBAME_REFRESH_DELAY_MS` (debug) delays the compare so the running state can be checked.
- Pin cell (learned rows, between the source tag and "..."): invisible until hover or focus, accent when pinned,
  Enter / Space toggles, writes `items.pin`. Pinned rows lead their group in native order with the grip
  dimmed. Tsubame's own rows have no pin cell. The real menu draws no pin marker.

The native popup (`TSUBAME_NATIVE_MENU=1`) still gets only what Tsubame adds: hidden built-ins are left
out and enabled custom / imported entries are appended (`src/shellops.rs`).

Custom entry run (`custom_cmd`): the command line is split into arguments first, then the tokens
(`{path}` `{selection}` `{dir}` `{name}` `{ext}` `{parent}`, also `%1` / `%*`) are replaced inside each
argument, so a file name never adds or splits an argument. The argv goes to `std::process::Command`:
no shell, no hand-built command line. Spawning is on its own thread; a failure shows in the status bar.

Saved as:

```toml
[[context_menu.entries]]
id = "custom.1"
name = "Open in VS Code"
command = "C:\\Program Files\\Microsoft VS Code\\Code.exe"
args = "\"{path}\""
extensions = []              # lower-case, no dot; empty = any
show_for = { files = true, folders = true, background = false }
hidden = false
source = "custom"            # builtin | windows | custom | nilesoft
parent = ""                  # id of the submenu it sits in
```

An empty `entries` list = the default menu. The list written by the editor stores the whole effective
list (built-ins included) so hidden state and order survive newer builds (ids are stable).

### Nilesoft Shell import (`crates/tsubame-config/src/nss.rs`)

Reads `shell.nss` (never changes it). The button opens the Windows file dialog (`IFileOpenDialog`, filter
`*.nss`, own thread, owned by the Tsubame window; `src/settings/nss_pick.rs`). On opening the page the usual
places are looked at (`%ProgramFiles%`, `%APPDATA%`, `%LOCALAPPDATA%` + `Nilesoft Shellshell.nss`): a found
file shows "Nilesoft Shell found in ...: N entries (M can be imported, K are skipped)" with Review and import
(`nss::detect`). After an import a report (`nss::Report`) lists what was added and each skipped statement
with line and reason, with Undo import and Show imported. Supported:

- `item(...)`, `menu(...) { ... }` (nested), `separator` / `sep`, `sep=before|after|both` on an item
- properties `title`, `cmd`, `args` (`cmd-line`), `type` (`file`, `dir`/`directory`, `back`, `*`), `find` (extension filter)
- variables `@sel.path`, `@sel.dir`, `@sel.file`, `@sel.paths`, `@sys.dir`, `@sys.bin`, `@user.home`, `@user.desktop`
- `shell { }`, `static { }`, `dynamic { }` blocks are looked into

Skipped, each listed with its line and reason in the import report: `admin` items, dynamic expressions
(`if(...)`, `reg.*`, `$variables`, `@(...)`), `modify(...)`, `theme { }`, `settings { }`, `import`.

### Updates

`tsubame-update` is wired through `src/updates.rs`:

- Background check at most once per 24 h, started after the window is up, on its own thread; never blocks startup.
- No network at all when `updates.auto_check = false` (automatic path), in a dev/measurement run
  (`--bench`, `--leak-gate`, ...), when portable, or in bench mode. The manual "Check now" is the user's call.
- Unreachable / empty feed / rate limited: the friendly "Can't reach the update server right now" state; retried later.
- Verified download is staged; it is applied only at restart (never while running). "Install updates
  automatically" (default off) only changes whether the download starts without a click.
- "Download and restart" saves the session, closes the window, then the process swaps the staged
  version in (`AFTER_EXIT` in `main.rs`); "Roll back" does the same with the previous version.
- Keys pinned in the binary; see `docs/updater.md`.

### Default file manager (opt-in, R1 Tier 1)

Built (R2); full description, exact registry contents and failure modes in `docs/default-file-manager.md`.
A HKCU `Software\Classes\Directory\shell\Tsubame` verb starts the fail-safe shim `tsubame-handler.exe`
(`crates/tsubame-handler`); the page shows the real state (Not set / Active / Needs repair / Not set
(another app)), a switch, a Repair button (only when the helper moved or is gone) and a Remove button.
No shell-hijack keys, no HKLM, no `Folder\shell`, no Win+E takeover. The registry code lives in
`tsubame-handler::registry` behind a trait (`Registry`) and is shared by the page and the shim; the fake is
used by tests, and the real writer refuses to write under `cfg(test)` or with
`TSUBAME_NO_REGISTRY_WRITE=1`. The registry is read when the page opens and on Refresh, written only by
the buttons, never re-applied by the app. With no `tsubame-handler.exe` next to `tsubame.exe` the switch is
disabled with the tooltip "tsubame-handler.exe was not found next to tsubame.exe".

## Budgets

Benchmark machine, release, full harness run (`docs/bench-log.md`, "U4" section): idle 0 wake-ups (46 ms CPU / 10 s,
as before), private bytes 64.5 MB idle / 83.8 MB after the 4K scroll run (budgets 90 / 110), leak gate with
palette + Settings churn in every tab cycle PASS (worst +4.12%, limit 5%), exe 1714 KB (limit 6 MB).
The exe figure came from a stale build directory (a pre-updater binary): a real U4 build is ~2.6 MB, still
well inside the limit - see the correction at the end of `docs/bench-log.md`.
Scroll paint p95 read 6.7 ms on a loaded machine; the INT1 baseline binary reads 4.7 to 7.5 ms on the same machine
at different times, so the figure needs a quiet machine. Palette and Settings are `None` while closed, so
nothing of U4 runs per frame in a list.

## Not done / known gaps

- Settings pages for Panes and Search, key profiles (`keys.profile` is read, not used), custom theme
  packs, hover preview of a theme, Win+E takeover and the extra Explorer verb.
- "Match Windows" colour mode reads the system theme once at startup/selection; it does not follow a
  live change.
- Context menu editor: no "Several items" / "Drive" kinds (no data behind them) and "Edit entry" from the menu's
  edit menu opens the form at the top of the page (no scroll-to hook).
- Nilesoft import has the selection screen (`ctxgap-nss`): tick entries (a submenu box is tri-state), "shows for",
  skipped rows with reason and line, Select all / none, All / Importable / Skipped chips, "Placed in group"
  (your groups, New group... with a name field, End of the menu; first time a new group "Nilesoft", then the last
  used, remembered in `context_menu.nss_group`). Keys: Tab, Up / Down / Home / End / PgUp / PgDn and Space in the
  list, Enter imports, Esc cancels. Undo import also removes a group the import made. Not drawn: the per-entry
  icons of the mock. The placement is stored (`MenuEntry.group`) and shown in the group's count in Settings; the
  real menu does not draw imported entries inside the group yet (group composition there is still learned items only).
- The real menu composes user groups (a plain separator-delimited block, no header row, at the group's
  `pos`) and honours `items.pin` (first in its group).
- `advanced.diagnostics` persists, the log writer is not wired.
- `updates.channel` is read; the updater is pinned to stable.
- The memory readout on Advanced refreshes on repaint (about once a second while the page is visible),
  not on a timer.
- The Updates "can’t reach the server" state is covered by unit tests (`status_line`, `phase_from_error`) but
  has no screenshot: the benchmark machine does reach GitHub and a 404 (no release yet) reads as "up to date".
- `docs/shots/u4-impl-*.png` were driven by keyboard only (mouse injection does not register on the benchmark machine).
