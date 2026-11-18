# U4: command palette, rebindable keys, Settings tab

What shipped, where it lives, what it costs, and what it does not do (yet).
Mocks: `mocks/mock-palette.html`, `mock-settings.html`; shots `mocks/shots/u4-*.png`.
Implementation shots (rig, 4K at 200%, scaled): `docs/shots/u4-impl-*.png`.

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
| Default file manager | `crates/tsubame-win/src/defaultfm.rs` | registry trait + fake + real HKCU writer |
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
| Context menu | list of entries with visibility toggles, drag to reorder, add custom entry (name, command line, optional extension filter), Nilesoft import, reset |
| Keys | see above |
| Updates | version, channel, last checked, state line (see below), Check now, Download and restart, Roll back, "Check automatically" and "Install updates automatically" (off) |
| Default file manager | opt-in R1 state + buttons (see below) |
| Advanced | open config folder, reset all settings (asks first; the old file is kept as `config.toml.bak`), diagnostics toggle, memory readout, portable indicator |

Density naming: config `compact/normal/comfortable` = kit `Compact/Comfortable/Spacious` = 22/26/30 px.

### Context menu editor

The W1 native shell menu is untouched (`crates/tsubame-win/src/shell/menu.rs`). What the editor changes
is what Tsubame *adds* to it, through the `extras` list in `src/shellops.rs`:

- the built-in lines of ours (Rename, Copy path, Paste, New folder, ...) that are hidden are left out;
- enabled custom / imported entries that apply to the click (file with matching extension, folder,
  background) are appended after a separator, in the saved order, submenus flattened to their children.

Not possible while the native menu stays: hiding or renaming Windows' own items (Open with, Share, ...).
They show as "Windows" rows in the editor, locked.

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

Reads `shell.nss` (never changes it). Supported:

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

Only the design in `docs/research-default-explorer-defender.md`: a HKCU
`Software\Classes\Directory\shell\...` verb that starts the fail-safe shim, plus a Remove button and the
current state. No shell-hijack keys, no HKLM. The registry code is behind a trait (`Registry`): the fake
is used by tests, and the real `WinRegistry` refuses to write under `cfg(test)` or with
`TSUBAME_NO_REGISTRY_WRITE=1`. **The shim (`tsubame-handler.exe`) does not exist yet**, so the button is
disabled with the tooltip "shim not built yet".

## Budgets

See the numbers in the U4 report (`docs/HANDOFF.md` is not touched by this task). Short form: palette
and settings state are built lazily and freed on close; the leak gate cycles both with every tab cycle.

## Not done / known gaps

- Settings pages for Panes and Search, key profiles (`keys.profile` is read, not used), custom theme
  packs, hover preview of a theme, Win+E takeover and the extra Explorer verb.
- "Match Windows" colour mode reads the system theme once at startup/selection; it does not follow a
  live change.
- Hiding or renaming Windows' own context menu items (see above).
- `advanced.diagnostics` persists, the log writer is not wired.
- `updates.channel` is read; the updater is pinned to stable.
- The memory readout on Advanced refreshes on repaint (about once a second while the page is visible),
  not on a timer.
