# F6 mocks: Miller columns, tree view, tags, file-age colouring (P4.F6) and F4 on ext-api

Mocks only. No Rust, no app change. Each file is one self-contained HTML page (no network, works from `file://` or an inline preview). They reuse the conventions of `mocks/mock-preview.html`: tokens copied verbatim from `mocks/tokens.css`, the same window chrome, flat fills and 1 px strokes, no shadows, no gradients, no animation timers.

Common controls on every page: **Theme** (dark / light, the two themes that exist in `tokens.json`), **Density** (Compact 22 default, Normal 26, Comfortable 30, switchable live), **Zebra** stripes, and State pills. URL params for deterministic captures: `?theme=dark|light&density=22|26|30&zebra=0|1&state=<id>&only=1` (plus `&w=` window width on Miller and Tree, `&surface=rows|pane` on Tree, `&mode=` and `&pal=` on Age, `&shapes=0|1` on Tags). Sidebar rows stay fixed at 24 px at every density. Each page ends with accessibility notes, a budget note and an **Open questions** box.

| File | Task |
|---|---|
| `mocks/mock-f6-miller.html` | Miller columns view |
| `mocks/mock-f6-tree.html` | Tree view (rows and pane) |
| `mocks/mock-f6-tags.html` | Tags and colours |
| `mocks/mock-f6-age.html` | File-age colouring |
| `mocks/mock-f4-batch-rename-ext.html` | F4: batch rename on ext-api |

## What each mock shows

**Miller (`tsubame.ext.miller`).** Live: click rows or use the arrows, Enter, Backspace, Home/End and type-ahead; drag a column edge to resize (140 to 480 px, default 220, double-click resets). Selected path rows in earlier columns get a quiet fill plus a 2 px accent bar; the active column gets the full selection and the focus ring. The last column is an info column (metadata, preview excerpt when `tsubame.ext.preview` is on). States: default, deep path with overflow (a left cue says how many columns are hidden), file with preview, file with the preview extension off, loading folder (skeleton, Esc), empty folder, access denied with Retry, junction target. Window widths 1280 / 760 / 480: below 560 px the view becomes single-column drill-down with a parent header.

**Tree (`tsubame.ext.tree`).** Two surfaces from one model: Tree rows (files and folders as expandable rows in the list) and Tree pane (folders-only pane beside Details; a drawer below 560 px). Live: chevrons, rows, arrows, `*`, Shift+Right, +/-, Enter. 16 px indent per level, clamped to 6 px past depth 12 with a depth badge; 20 px chevron hit area. Rows for lazy loading, empty folder, access denied, junction loop (not expanded), child-cap overflow, symlink and junction badges with target text.

**Tags (`tsubame.ext.tags`, column `tsubame.column.tags`).** Eight tag colours (new `--tg-*` tokens per theme) each with a shape, so colour is never the only cue. Details (shape cluster after the name plus a Tags column of chips), Grid (shapes under the name), sidebar Tags section with counts and click-to-filter, filter result list across folders with a path-bar chip, own-drawn picker popover in the context-menu style (check, mixed state for multi-select, number keys, find or create), new-tag colour chooser, empty filter result, indexing (progress line, pending marks), read-only store banner. A computed contrast table (both themes, six surfaces) is on the page. A storage comparison table (sidecar vs NTFS stream vs central database) with the recommendation is on the page and summarised below.

**Age (`tsubame.ext.file_age`).** Five buckets (today, week, month, year, older) on the Modified column: styles date text (default), date chip, row accent bar; palettes Heat, Colour-blind safe (adds pips), Monochrome (bold today plus pips). Status-bar toggle and legend popup with thresholds and counts; a settings card with a master toggle, style, palette, editable thresholds (with an invalid-threshold error state), pips, folders, date used. Edge cases: future date, unknown date, exact threshold, just before midnight. Mock clock fixed at 2026-11-25 14:30 UTC. Computed contrast and colour-blind separation tables are on the page.

**F4 on ext-api.** One page: a before/after table (nothing visible changes), the Settings > Extensions toggle for `tsubame.ext.batch_rename`, and the same window with the extension on (bar and live preview) and off (F2 renames the focused item inline, status bar explains).

## Tag storage recommendation

Central database `tags.db` next to `config.toml`, keyed by (volume serial, file id) with the last known path as a hint. The enumerator already reads the file id, so keys cost nothing.

| | Sidecar per folder | NTFS stream | Central database |
|---|---|---|---|
| Survives rename/move | only if Tsubame rewrites it | yes on NTFS | yes inside a volume (file id) |
| Writes into user folders | yes (clutter, read-only fails) | yes (file level, AV and sync tools react) | no |
| FAT/exFAT, network, zip, cloud | if writable | lost | path fallback, weaker |
| Listing cost | 1 read per folder | 1 open per file (too slow) | one batched lookup per page |
| Cross-folder filter | scan everything | scan everything | one index lookup |

Rejected: `System.Keywords` through the property store (rewrites files, fails for most types). Possible later as a one-way export or an opt-in ADS mirror.

## Open questions and recommendations

Miller
1. Column header (24 px, name and count) on every column or none? **Recommend: show it** (names columns that scroll off-screen; carries the active accent rule).
2. Sort and filter: pane-wide like Details, or per column? **Recommend: pane-wide.**
3. Window below 560 px: single-column drill-down, or keep scrolling? **Recommend: drill-down below 560 px, scroll above.**
4. Keys: Enter enters a folder or opens a file; Right on a file does nothing. View-switch chords `Ctrl+Alt+1..4` (Details, Grid, Miller, Tree) are proposals, no chord is confirmed free. **Built:** the mock's `Ctrl+Alt+3` is stale. `Ctrl+Shift+3` is `view.grid-large` and `Ctrl+Shift+4` is `view.tree`, so Miller is `Ctrl+Shift+5` (`view.miller`). **Recommend: as stated, settle chords in the keybinding pass** (`docs/PLAN.md` section 9).

Tree
1. Folder tree pane: separate opt-in pane (`Alt+T`) or a sidebar section? **Recommend: separate pane**; the sidebar stays Quick access and Drives.
2. Tree rows: files as well as folders? **Recommend: yes** (the pane stays folders-only).
3. Follow junctions and symlinks on expand? **Recommend: yes on explicit action with a loop guard, never in recursive expand.**
4. Caps: recursive expand 500 folders, 5,000 children per expanded folder. **Recommend: keep both, configurable, never silent.**

Tags
1. Storage: central database or NTFS streams? **Recommend: central `tags.db`**; never sidecars.
2. Default tag set: 8 named colours (Urgent, Waiting, To review, Done, Work, Personal, Invoice, Archive) or 7 unnamed labels? **Recommend: 8, renamable, deletable.**
3. Shapes on by default? **Recommend: on** (the palette is not colour-blind safe on its own); one setting for plain dots.
4. Picker chord `Alt+T`? **Recommend: `Alt+T` for the picker, palette command `tags.pick`, number keys only inside the picker.**

Age
1. On or off by default? **Recommend: off**, one click in the status bar.
2. Default style and palette? **Recommend: date text with Heat**; Colour-blind safe is offered first in the settings card.
3. Thresholds and "today"? **Recommend: since midnight, 7 / 30 / 365 days, all editable**, plus a last-24-hours option.
4. Tint folders by newest content? **Recommend: no in v1** (off by default); if wanted later, reuse the folder-sizes cache and caps.

F4 on ext-api
1. With the extension off, what does F2 do on a multi-selection? **Recommend: rename the focused item inline and say so in the status bar.**
2. Switchable or `required`? **Recommend: switchable, default on.**
3. Pattern engine in core or in the extension? **Recommend: stays in core (pure, reusable by packs and wasm).**

## Budget implications

- **Zero idle cost for all five.** No timers, no wake-ups. Loading rows and the tag-indexing line are static glyphs and progress fills drawn only when a worker message arrives. Age buckets read the clock once per frame, so a window left idle across midnight keeps stale tints until the next repaint (accepted trade-off).
- **Memory caps.** Miller: at most 12 live columns. As built, each column has its own `EntryStore` (not an index into one shared store), because every column lists a different folder; each is capped at 100,000 entries. Tree: 5,000 children per expanded folder, 50,000 live rows, collapse frees children at once, at most 32 watched expanded folders. Tags: 4,096-row LRU of file-id to tags, database soft cap 100,000 tagged items / 16 MB. Age: nothing cached per file.
- **Background work: only tags, plus lazy enumeration for Miller and Tree.** Tags: batched lookups for visible rows, re-keying after watcher rename or move events, a one-time lazy reconcile per volume. Age: none (modified time is already in the 24-byte entry record). No full-disk scans anywhere.
- **Paint.** Visible rows only; the details p95 < 5 ms budget at 4K is unchanged. Age text colour is a different colour on the same draw call; chip adds one alpha-filled rounded rect per visible row.
- **Switchable.** All four F6 features and batch rename are built-in extensions (`tsubame.ext.miller`, `tsubame.ext.tree`, `tsubame.ext.tags`, `tsubame.ext.file_age`, `tsubame.ext.batch_rename`); off removes their buttons, commands, columns and settings cleanly.

## Accessibility summary

- All text and glyph colours were computed against the token surfaces: tag colours >= 3:1 on row, zebra, hover, selected, panel and popup in both themes (dark min 4.3, light min 3.3); age date text >= 4.5:1 for all three palettes on all five row states.
- The Heat age palette and the tag palette are not colour-blind safe on their own. Mitigations built into the designs: tag shapes and names, age pips and bold-today, and the Colour-blind safe palette (neighbouring buckets are separated by CIE delta E of 30 or more under deuteranopia and protanopia).
- Everything is keyboard-complete; the focus ring is the existing 1 px `--c-focus` (8.5:1 dark, 7.0:1 light on the row fill).
- UI Automation follows the CM10 rules in `docs/ctxmenu.md`: built only while a client listens. Tree and TreeItem with ExpandCollapse for the tree, List and ListItem for Miller columns, tags and age bucket appended to the row's Name or Modified value. Built: see `docs/uia-views.md`.

## Verification

No browser was opened. Each page was run headless in jsdom (Node): scripts execute, every state pill, theme, density, zebra, width and surface control is clicked, keys are dispatched, and every state is loaded through its URL param; the run fails on any script error or console error, any external URL, any shadow, blur or gradient in the CSS, any personal path, any contrast-table FAIL, or a missing open-questions box. Visual layout is not verified by this check; it needs a look on a real display (rig, 4K at 200%) before implementation.
