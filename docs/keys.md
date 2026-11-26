# Keyboard shortcuts

Every built-in command and its default key. Rebind any of them in
**Settings > Keys** (Ctrl+,): click a row, press the new chord. Changes apply at
once and are saved as `[keys.overrides]` in the config (see [config.md](config.md)).
Up to two chords per command; a chord another command already uses asks before it
moves. Commands with no default key can be given one the same way, or run from the
command palette (Ctrl+Shift+P or Ctrl+K).

The extension commands (preview, tags, tree pane, extract, checksum, folder sizes)
use `Alt+letter`; the view switches use `Ctrl+Shift+1` to `Ctrl+Shift+5`.

This table is generated from the command table in `src/commands.rs`; the test
`keys_doc_matches_the_command_table` fails when it is stale. Regenerate with
`TSUBAME_UPDATE_KEYS_DOC=1 cargo test keys_doc`.

<!-- keys:start (generated: cargo test keys_doc; TSUBAME_UPDATE_KEYS_DOC=1 rewrites it) -->

### File

| Command | Name | Default keys |
|---|---|---|
| Rename (several items: batch rename) | `file.rename` | `F2` |
| Undo the last file operation | `file.undo` | `Ctrl+Z` |
| Redo | `file.redo` | `Ctrl+Y` |
| Open a terminal in this folder | `file.open-terminal` | `Ctrl+Alt+T` |
| Copy file checksums (SHA-256 / SHA-1 / MD5 / CRC32) | `file.checksum` | `Alt+H` |

### Edit

| Command | Name | Default keys |
|---|---|---|
| Select all | `select.all` | `Ctrl+A` |
| Invert selection | `select.invert` | `Ctrl+Shift+A` |
| Clear filter, then selection | `select.escape` | `Esc` |

### Navigate

| Command | Name | Default keys |
|---|---|---|
| Edit the path | `nav.focus-path` | `Ctrl+L`, `F4`, `Alt+D` |
| Filter this folder | `nav.find` | `Ctrl+F`, `Ctrl+E` |
| Back | `nav.back` | `Alt+Left` |
| Forward | `nav.forward` | `Alt+Right` |
| Up one folder | `nav.up` | `Alt+Up` |
| Refresh | `nav.refresh` | `Ctrl+R` |
| Up one folder (Backspace) | `nav.backspace-up` | `Backspace` |
| Global search… | `search.global` | `Ctrl+Shift+F` |

### Tabs

| Command | Name | Default keys |
|---|---|---|
| New tab | `tab.new` | `Ctrl+T` |
| Close tab | `tab.close` | `Ctrl+W`, `Ctrl+F4` |
| Next tab | `tab.next` | `Ctrl+Tab` |
| Previous tab | `tab.prev` | `Ctrl+Shift+Tab` |
| Open the selected folder in a new tab | `tab.open-selected` | `Ctrl+Enter` |
| Reopen closed tab | `tab.reopen` | `Ctrl+Shift+T` |

### Panes

| Command | Name | Default keys |
|---|---|---|
| Split the window in two panes | `pane.split` | `Ctrl+\` |
| Panes side by side or stacked | `pane.orientation` | `Ctrl+Shift+\` |
| Switch to the other pane | `pane.switch` | `Tab` |
| Copy the selection to the other pane (F5 refreshes with one pane) | `pane.copy-to-other` | `F5` |
| Move the selection to the other pane | `pane.move-to-other` | `F6` |
| Open the selected folder in the other pane | `pane.open-in-other` | `Ctrl+Shift+Enter` |
| Toggle tree pane | `pane.tree` | `Shift+Alt+T` |

### View

| Command | Name | Default keys |
|---|---|---|
| Switch dark / light | `view.theme` | `Ctrl+Shift+L` |
| Next row density | `view.density` | `Ctrl+Shift+D` |
| Toggle striped rows | `view.zebra` | `Ctrl+Shift+Z` |
| Details view | `view.details` | `Ctrl+Shift+1` |
| Small thumbnails | `view.grid-small` | `Ctrl+Shift+2` |
| Large thumbnails | `view.grid-large` | `Ctrl+Shift+3` |
| Bigger icons (Ctrl+wheel up) | `view.zoom-in` | `Ctrl+=`, `Ctrl+Shift+=` |
| Smaller icons (Ctrl+wheel down) | `view.zoom-out` | `Ctrl+-` |
| Collapse or expand the sidebar | `sidebar.toggle` | `Ctrl+B` |
| Pin this folder to the sidebar | `sidebar.pin-folder` | `Ctrl+D` |
| Show hidden files | `view.hidden` | `Ctrl+H` |
| Reveal current folder in sidebar | `sidebar.reveal-current` | `Ctrl+Shift+E` |
| Toggle preview pane | `preview.toggle_pane` | `Alt+P` |
| Quick Look | `preview.quick_look` | `Space`, `F3` |
| Tree view | `view.tree` | `Ctrl+Shift+4` |
| Miller columns view | `view.miller` | `Ctrl+Shift+5` |

### App

| Command | Name | Default keys |
|---|---|---|
| Command palette | `palette.open` | `Ctrl+Shift+P`, `Ctrl+K` |
| Open settings | `settings.open` | `Ctrl+,` |
| Settings: keyboard shortcuts | `settings.keys` | `F1` |
| New window | `window.new` | `Ctrl+N` |
| Toggle fullscreen | `window.fullscreen` | `F11` |

### Tools

| Command | Name | Default keys |
|---|---|---|
| Extract archive here | `archive.extract_here` | `Alt+E` |
| Extract selected from archive | `archive.extract_selected` | `Shift+Alt+E` |
| Calculate folder sizes | `folder.calc_sizes` | `Alt+S` |
| Tags… | `tags.pick` | `Alt+T` |

### No default key

| Command | Name |
|---|---|
| Show file extensions | `view.extensions` |
| Move this tab to the other pane | `pane.move-tab` |
| Show this folder in the other pane | `pane.sync-folders` |
| Show protected system files | `view.system` |
| Check for updates | `app.check-updates` |
| Open the settings folder | `app.open-config-folder` |
| Settings: themes and appearance | `settings.appearance` |
| Settings: context menu editor | `settings.context-menu` |
| Settings: updates | `settings.updates` |
| Settings: default file manager | `settings.default-file-manager` |
| Reset all keyboard shortcuts | `settings.reset-keys` |
| Create shortcut | `file.create-shortcut` |
| Paste shortcut | `edit.paste-shortcut` |
| Rename: commit and rename the previous item | `rename.previous` |
| Copy address | `nav.copy-address` |
| Copy address as text with quotes | `nav.copy-address-quoted` |
| Paste the clipboard as an address and go | `nav.paste-and-go` |
| Age colours: toggle | `view.age_colours` |
| Batch rename selected items… | `file.batch_rename` |

<!-- keys:end -->
