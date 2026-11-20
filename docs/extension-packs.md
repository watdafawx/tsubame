# Extension packs

A pack is one TOML file that adds commands, context-menu entries, key bindings,
themes and file-type rules to Tsubame. A pack holds data, not code: it can start
a program the way a custom context-menu entry does, recolour file names and add
a theme, and nothing else. This is the first of the three extension tiers in
`docs/PLAN.md` section 4; it registers through the same registry as the built-in
features (`docs/extensions.md`).

Code: the parser is `crates/tsubame-config/src/packs.rs` (no Win32), the loader
and the runtime are `src/packs.rs`. Two examples to copy are in
`examples/packs/`.

## Install, reload, remove

1. Put the file in the `packs` folder inside the settings folder (the one with
   `config.toml`): `%APPDATA%\Tsubame\packs\` for an installed copy,
   `<exe folder>\data\packs\` for a portable one. Settings > Advanced >
   Extension packs shows the path and has an Open folder button.
2. Restart Tsubame, or press Reload packs on that page.
3. Remove the file (and reload) to uninstall it.

The file name is the pack's name and must be lower-case letters, digits and `-`
(`open-in-terminal.toml`). Everything the pack adds is named `pack.<name>.<id>`:
the pack itself is the extension `pack.open-in-terminal`, its command is
`pack.open-in-terminal.terminal`.

Tsubame reads the folder at start-up and on Reload packs. It does not watch it,
so an idle Tsubame does no pack work at all, and with no `packs` folder the cost
is one failed folder open.

## Turn a pack off

Add its id to `[extensions] disabled` in `config.toml`:

```toml
[extensions]
disabled = ["pack.open-in-terminal"]
```

A disabled pack stays loaded but contributes nothing: its commands leave the
palette and the key map (the keys are free again), its menu entries, themes and
file rules stop applying. Taking the id out of the list switches it back on.

## Trust

A pack can start programs, so install packs the way you install programs: only
from people you trust. What it can not do is run code inside Tsubame, read your
files, change your settings or build a shell command line: arguments are passed
to the program as a list (never through `cmd` or any shell), so a file name with
spaces, quotes, `&` or `%` stays one argument.

## A broken pack is skipped, never a crash

The first mistake in a file rejects that pack only. The error names the file and
the key, and Settings > Advanced > Extension packs lists it:

```text
open-in-terminal.toml: command[0].keys[1]: cannot read "Ctrl+Nope" as a key (write it like Ctrl+Alt+T)
broken.toml: line 7: is not valid TOML: invalid escape sequence
other.toml: menu[0].command: this pack has no command "terminl"
```

Unknown keys are mistakes too, so a typo is never silent. Limits: 128 KB per
file, 32 packs, 64 commands, 32 menu entries, 8 themes, 64 file rules per pack,
512 characters per text, 2048 per `args`; no control characters in any text.

A key chord another command already uses is the one soft case: the pack loads,
that chord is left out and the page shows a warning.

## The file

```toml
[pack]                      # optional
title = "My pack"           # shown in Settings (default: the file name)
version = "1.0"
description = "What it does."
author = "You"

[[command]]   ...           # zero or more of each
[[menu]]      ...
[[theme]]     ...
[[file_type]] ...
```

At least one `[[command]]`, `[[menu]]`, `[[theme]]` or `[[file_type]]` is
needed. Ids (`id = "..."`) are lower-case letters, digits, `-` and `_`, unique
per kind within the file.

Write Windows paths in single quotes (`program = 'C:\Tools\tool.exe'`); in
double quotes a backslash starts an escape.

### Commands and key bindings

```toml
[[command]]
id = "terminal"                  # required
title = "Open terminal here"     # required: the palette and Settings > Keys
program = "wt.exe"               # required: a name on PATH or a full path
args = "-d {dir}"                # optional
keys = ["Ctrl+Alt+T"]            # optional: up to 4 default chords
needs_folder = true              # optional, default true: does nothing on the Settings tab
global = false                   # optional, default false: the keys also work while typing in a text field
```

A command appears in the command palette as `title` and in Settings > Keys, where
its key can be changed or reset like any other. Chords are written like
`Ctrl+Shift+K`, `Alt+F9`, `Ctrl+Alt+T`. A plain letter or digit alone is refused
(it would break typing).

`args` is split like a command line first (spaces separate, `"..."` groups),
then the tokens are replaced inside each argument:

| Token | Is |
|---|---|
| `{path}` | the first selected item (the folder in front when nothing is selected) |
| `{selection}` | every selected item. As a whole argument it becomes one argument per item |
| `{dir}` | the folder in front |
| `{name}` / `{ext}` / `{parent}` | name, extension (no dot) and folder of the first item |

A command whose `args` has no `{selection}` runs once per selected item (that
item is `{path}`); more than 10 items is refused with a message instead of
starting that many programs. Any other `{word}` is an error (`unknown token
{pth}`), so a typo is caught when the pack loads.

### Context-menu entries

```toml
[[menu]]
id = "terminal"
label = "Open terminal here"     # required
command = "terminal"             # a command of this pack: its program and args
# program = "code.exe"           # or, instead of `command`: a program and args of its own
# args = "{path}"
show = ["folders", "background"] # files, folders, background, multiple, drives
types = ["rs", "toml"]           # only for files with these extensions (empty = any)
```

Without `show` the entry shows for files, folders and a selection of several
items. The entry is Tsubame's existing custom-entry model (`MenuEntry`) with the
source tag `Pack`; it is added after Tsubame's own rows, runs exactly like a
custom entry, and is never written to `config.toml`: it comes from the pack each
time. To hide it, disable the pack.

### Themes

```toml
[[theme]]
id = "lite"
name = "Solarized Lite"          # shown in Settings > Appearance
dark = false                     # required: dark or light
bg = "#fbf3dc"                   # window background
panel = "#eee8d5"                # sidebar, toolbar, title bar
surface = "#fdf6e3"              # list rows, fields
text = "#073642"
muted = "#586e75"                # secondary text
accent = "#cb4b16"               # selection, focus, toggles
```

Every colour is optional (`#rrggbb` or `#rgb`); one left out comes from the
Tsubame theme of the same brightness. The theme shows up in the Appearance
picker next to the built-in ones, and its config id is `pack.<name>.<id>`
(`[appearance] theme = "pack.solarized-lite.lite"`). A saved theme whose pack is
gone or switched off falls back to the Tsubame theme.

### File-type rules

```toml
[[file_type]]
id = "code"
extensions = ["rs", "py"]        # required: no dot, any case, no wildcards
color = "#268bd2"                # the file's name in the list and grid
icon = "code"                    # glyph name, U+E943, badge:RS, or .ico/.png path
open_with = '"C:\Tools\code.exe" --goto {path}'   # Enter and double click start this
```

At least one of `color`, `icon`, `open_with`. Rules apply to files, not folders.
When two rules name the same extension the first one wins (packs load in file
name order). `open_with` is a command line: the program (quoted when it has
spaces), then arguments with the same tokens as a command. It replaces the
system's default program for Enter and double click; "Open" in the context menu
and Open with still use the shell.

`icon` replaces the file's icon in both details and grid views (content
thumbnails still draw over it in grid view once loaded) and accepts four forms:

| Form | Example | What is drawn |
|---|---|---|
| Image file | `"icons/rust.ico"`, `'C:\Icons\rust.png'` | `.ico` or `.png` file (relative to the `packs` folder without `..`, or an absolute path; decoded once at pack load, capped at 64 px / 1 MB total cache). A missing or unreadable file adds a warning and falls back to the shell icon. |
| Built-in glyph name | `"code"`, `"doc"`, `"image"`, `"archive"` | Segoe Fluent Icons glyph tinted with the rule's `color` (or accent). Built-in names: `code`, `doc` (`document`, `text`), `image` (`picture`, `photo`), `archive` (`zip`), `audio` (`music`), `video` (`film`), `terminal` (`shell`), `config` (`gear`, `settings`), `data` (`table`), `book` (`markdown`), `lock` (`key`), `web` (`globe`), `link`, `star`, `tag`, `font`, `app` (`program`), `pdf`, `bug`, `tool` (`wrench`), `file`, `folder`. |
| Icon-font codepoint | `"U+E943"` | Any Segoe Fluent Icons Private Use Area codepoint (`U+E000`–`U+F8FF`). |
| Badge | `"badge:RS"`, `"rust"`, `"zig"` | A crisp rounded badge tinted with `color` (or accent): `badge:<1-3 ASCII chars>`, or any other plain name (1–3 chars as-is, longer names take the first 2 letters, uppercased). |

## What a pack costs

Nothing when there is no `packs` folder. With packs: reading and parsing the
files at start-up (too small to see in the cold-start measurement: both example
packs together measured within the run-to-run spread, `docs/bench-log.md`), and
one registry entry per item. File colours and icons are looked up from a table
built once per registry change (image icons are decoded once at load into a
bounded LRU cache), so drawing a row costs one short comparison per rule
extension and allocates nothing.
