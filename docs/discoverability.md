# Discoverability: nothing is reachable only by a hidden shortcut

Rule (B12-B): every feature has a visible control or a menu entry that shows its
shortcut. A shortcut is the fast way, never the only way.

The safety net is the **app menu**: the gear button at the right of the title bar
(left of the pane layout switch), or `F10`. It lists every command of the command
table in `src/commands.rs`, grouped (Settings, Keyboard shortcuts, Command palette,
New window, Full screen, **Go**, **Tabs**, **View**, **Panes**, **Files**,
**Settings pages**, Check for updates, About, Quit), each row with the shortcut the
user has bound. Rows of a switched-off extension leave the menu. The menu is the
shared own-drawn menu (submenus, keyboard, type-to-find, themes, UI Automation:
the menu is published as "App menu").

The test `appmenu::tests::every_command_is_reachable_from_a_menu` fails when a
command is in the command table but neither in `appmenu::tree()` nor on the
`NOT_IN_THE_MENU` allow-list (which carries a reason for every row). A new command
therefore cannot ship shortcut-only.

Toolbar buttons show a tooltip with the name and the key (`Back`, `Forward`,
`Up one folder`, view mode, filter, preview pane, the gear: "Menu and settings
F10", second line "Settings: Ctrl+,").

## Commands

"Visible" = a control or menu the user can find without knowing a key. `App menu`
= gear button / F10. Other columns name the extra places.

| Command | Default key | Visible control (besides the app menu) |
|---|---|---|
| `settings.open` | Ctrl+, | App menu (first row), command palette |
| `settings.keys` | F1 | App menu, Settings sidebar |
| `settings.appearance` / `.context-menu` / `.updates` / `.default-file-manager` | none | App menu > Settings pages, Settings sidebar |
| `settings.reset-keys` | none | App menu > Settings pages, Settings > Keys |
| `app.menu` | F10 | Gear button in the title bar |
| `app.about` | none | App menu (opens Settings > Updates with the version) |
| `app.quit` | Ctrl+Q | App menu, window close button |
| `app.check-updates` | none | App menu, Settings > Updates |
| `app.open-config-folder` | none | App menu > Settings pages |
| `palette.open` | Ctrl+Shift+P, Ctrl+K | App menu |
| `window.new` | Ctrl+N | App menu, folder right click "Open in new window", tab menu "Move to new window" |
| `window.fullscreen` | F11 | App menu |
| `tab.new` | Ctrl+T | "+" button of the tab strip, app menu |
| `tab.close` | Ctrl+W, Ctrl+F4 | Tab close button, tab menu |
| `tab.next` / `tab.prev` | Ctrl+Tab, Ctrl+Shift+Tab | Click a tab; app menu > Tabs |
| `tab.reopen` | Ctrl+Shift+T | Tab menu (right click a tab), app menu > Tabs |
| `tab.open-selected` | Ctrl+Enter | Folder right click "Open in new tab" |
| `tab.duplicate`, `tab.close-others`, `tab.close-right`, `tab.move-to-window` | none | Tab menu (right click a tab), app menu > Tabs |
| `nav.back` / `nav.forward` | Alt+Left / Alt+Right | Toolbar buttons (tooltip with key; right click: history) |
| `nav.up` | Alt+Up | Toolbar button (tooltip with key) |
| `nav.backspace-up` | Backspace | Same as Up; allow-listed |
| `nav.refresh` | Ctrl+R | Empty-space right click "Refresh", app menu > Go |
| `nav.focus-path` | Ctrl+L, F4, Alt+D | Click the path bar; path bar right click "Edit address" |
| `nav.copy-address`, `.copy-address-quoted`, `.paste-and-go` | none | Path bar right click, app menu > Go |
| `nav.find` | Ctrl+F, Ctrl+E | Filter box (shows the key), search button in a narrow pane |
| `search.global` | Ctrl+Shift+F | App menu > Go, command palette |
| `file.open-terminal` | Ctrl+Alt+T | Folder / empty-space right click "Open terminal here", app menu > Go |
| `sidebar.toggle` | Ctrl+B | Sidebar footer ("Collapse" / chevron) |
| `sidebar.pin-folder` | Ctrl+D | Drag a folder to the sidebar; app menu > Go |
| `sidebar.reveal-current` | Ctrl+Shift+E | App menu > Go |
| `view.details`, `.grid-small`, `.grid-large`, `.tree`, `.miller` | Ctrl+Shift+1 to 5 | View mode button in the toolbar (cycles), app menu > View (radio rows) |
| `view.zoom-in` / `view.zoom-out` | Ctrl+= / Ctrl+- | Ctrl+wheel, app menu > View |
| `view.density`, `view.zebra`, `view.theme` | Ctrl+Shift+D / Z / L | Settings > Appearance, app menu > View |
| `view.hidden`, `view.system`, `view.extensions` | Ctrl+H, none, none | Settings > General, app menu > View (check rows) |
| `view.age_colours` | none | Status bar "Age" key (when on), Settings > Extensions, app menu > View |
| `view.group-*` (none, name, type, date, size) | none | Empty-space right click "Group by", column header right click, app menu > View > Group by |
| `view.group-by` (next), `view.group-toggle`, `.group-collapse-all`, `.group-expand-all` | none | Group header click; app menu > View > Group by |
| sort (not a command) | none | Click a column header; empty-space right click "Sort by" |
| `view.reset-columns` | none | Column header right click, app menu > View |
| column choice (not a command) | none | Column header right click; also `view.columns-menu` in the app menu |
| `view.columns-menu` | none | App menu > View "Choose columns" |
| `view.reset-folder-views`, `view.apply-to-all-folders` | none | App menu > View (folder view memory itself is automatic) |
| `view.operations` | Ctrl+Shift+O | Status bar pill "N running", app menu > View |
| `preview.toggle_pane` | Alt+P | Toolbar preview button (tooltip with key), app menu > View |
| `preview.quick_look` | Space, F3 | App menu > View |
| `pane.split` | Ctrl+\ | Layout switch in the title bar (one / side by side / stacked), app menu > Panes |
| `pane.orientation` | Ctrl+Shift+\ | Layout switch, app menu > Panes |
| `pane.switch` | Tab | Click the other pane, app menu > Panes |
| `pane.copy-to-other`, `pane.move-to-other` | F5, F6 | Status bar hint with two panes, app menu > Panes |
| `pane.open-in-other`, `pane.sync-folders`, `pane.move-tab` | Ctrl+Shift+Enter, none, none | Folder right click "Open in other pane", tab menu "Move to other pane", app menu > Panes |
| `pane.tree`, `view.tree` | Shift+Alt+T, Ctrl+Shift+4 | App menu > Panes / View |
| `view.miller` | Ctrl+Shift+5 | View mode button, app menu > View |
| `select.all`, `select.invert` | Ctrl+A, Ctrl+Shift+A | App menu > Files |
| `select.escape` | Esc | Allow-listed: cancels whatever is open |
| `file.rename` | F2 | Right click "Rename", app menu > Files |
| `file.batch_rename` | none | Right click on several items "Rename N items", app menu > Files |
| `rename.previous` | (Shift+Tab while renaming) | Allow-listed: only inside an open rename box |
| `file.undo` / `file.redo` | Ctrl+Z / Ctrl+Y | App menu > Files; status bar message after an operation |
| `file.create-shortcut`, `edit.paste-shortcut` | none | Right click menus, app menu > Files |
| `file.checksum`, `folder.calc_sizes` | Alt+H, Alt+S | App menu > Files |
| `archive.extract_here`, `archive.extract_selected` | Alt+E, Shift+Alt+E | Archive right click, app menu > Files |
| `tags.pick` | Alt+T | Right click "Tags...", app menu > Files; tag manager: sidebar Tags header "+", Settings > Tags |

Cut, Copy, Paste, Delete, Open, Open with, Properties and New (folder, file types) are
the shell's right click menu (item and empty-space), with the shortcut text.

## Features that are not commands

| Feature | Visible control |
|---|---|
| Sort by column | Click a column header (arrow shows direction); empty-space right click "Sort by" |
| Column menu (choose columns, git column, size to fit) | Right click the column header; app menu > View > Choose columns |
| Path autocomplete | Click the path bar and type; the dropdown shows under it |
| Folder size, git column | Column menu, Settings > Extensions |
| Layout: single pane, side by side, stacked | Three-button switch in the title bar |
| Operations panel | Status bar pill while a file operation runs; app menu > View |
| Conflict dialog | Appears on its own during a copy or move |
| Tab drag to reorder, drop on a pane | Drag a tab |
| Ctrl+wheel zoom | App menu > View "Bigger icons" / "Smaller icons" |
| Folder view memory | Automatic per folder; app menu > View > "Use this view for all folders", "Forget all folder views" |
| Sidebar drives, network, tags, pins | Sidebar; right click for menus |

## Known gaps

- The sidebar footer has no Settings entry (the footer is the collapse button; the
  title bar gear covers it).
- The app menu rows run the command as is: a file command with nothing selected
  does nothing (it does not grey out per selection).
- The gear is not in the UI Automation tree (the title bar buttons are not either);
  screen reader users open the same menu with `F10`, and the menu itself is published.
