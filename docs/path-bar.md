# Path bar: autocomplete, menu, terminal (B11-H)

## Autocomplete in the path edit

Ctrl+L / F4 / Alt+D (or a click on the bar) turns the bar into a text field. Typing opens a
dropdown of up to **8** rows under it (own-drawn, never takes the keyboard focus from the field).

| Typed text | Rows |
|---|---|
| `C:\Users\me\Do` | subfolders of `C:\Users\me` that start with `Do` (any case, same case first, natural order), folders only, hidden/system per the "show hidden" setting |
| `C` or `C:` | the drive root(s) of that letter, with the volume label |
| empty field | the drives, then the sidebar's Recent folders (a thin rule between the two) |
| `%USERPROFILE%\Doc`, `~\Doc` | expanded for listing and navigation; the field keeps what was typed |
| `\server\share\Pro` | listed below the share; `\server\` is never listed (no server enumeration) |
| a path inside an archive (`a.zip::x`) | nothing |

Keys (the field keeps the focus; **Esc** closes the list first, the edit second):

| Key | Does |
|---|---|
| Down / Up | move the highlight (wraps); the field's text does not change |
| Tab, or Right with the caret at the end | complete the highlighted row (the first row when none) and add `\`; the next level is listed at once |
| Shift+Tab | highlight up |
| Enter | open the highlighted row; without a highlight, open the typed text |
| Esc | close the list; Esc again closes the edit |
| click on a row | open that folder |

Cost model (`src/path_suggest.rs`):

- the parent folder is listed on the shared enumerator worker (`tsubame_win::enumerate`), never on
  the UI thread. A reply of a listing the user typed past is dropped (cancel token generation).
- a keystroke that keeps the **same parent** only re-filters the cached names in memory: no disk,
  no thread, no timer (5k folders: worst 0.05 ms).
- a keystroke that **changes the parent** cancels the running listing and arms **one** 80 ms
  one-shot `SetTimer` (id 14, `main.rs`); it is killed when it fires or the field closes. Idle: no
  timer, no wake-up.
- Tab / Right / a completion lists the new parent at once (no debounce). Opening the field already
  lists the current folder's parent, so the first keystroke has names.
- the cache lives only while the field is open.

UI Automation: the rows are a List `"Address suggestions"` (`PART_SUGGEST`, `docs/uia-views.md`),
each a ListItem with the folder path as its value and Invoke = open it.

## Right click on the path bar

Edit address (`nav.focus-path`), Copy address (`nav.copy-address`), Copy address as text with quotes
(`nav.copy-address-quoted`), Paste and go (`nav.paste-and-go`). Paste and go takes the first line of
the clipboard text (quotes and `%VAR%` allowed): a folder that opens, opens; one that does not
exist leaves you where you were with "Can't open ...", and text that is no path says so. No
default keys for these commands (bind them in Settings > Keys).

## Other keys

- **Ctrl+E**: the search box, same as Ctrl+F (`nav.find`). F3 stays Quick Look.
- **Ctrl+Alt+T** (`file.open-terminal`): Windows Terminal (`wt.exe -d`, fallback `cmd.exe`) in the
  shown folder; in an archive, the folder the archive sits in.
