# Tsubame (燕): plan

A very fast, feature-rich file browser. Windows first, portable core. File Pilot's look and speed, the features of Files and OneCommander, and no leaks.

Written 2026-11-17 at the start of the project. Sections 1-5 record the decisions that bind the work; sections 6-7 track the roadmap.

## 1. Tech stack (decided)

**Rust + raw Win32 (`windows` crate) + Direct2D/DirectWrite, with our own immediate-mode UI kit. Same stack as blink, an earlier project by the same author.**

| Option | Verdict | Why |
|---|---|---|
| Rust + Win32 + Direct2D (blink's stack) | **Chosen** | Measured on a 4K test machine in blink's `docs/gpu-render.md`: first paint 79–85 ms, paint 3–5 ms at 4K, 2.3 MB exe. blink's `src/gfx.rs` (742 lines) already has rect, text, image, clip and the software-first-frame / GPU-swap / device-loss handling. |
| GPUI (hopper's stack) | No | Cross-platform for free, but hopper needed a memory project (646 MB working set on a 1,188-row list before 0.59.0) and has a 2,000 ms cold-start budget. Wrong weight class. No native shell context menus. |
| egui (spotifast's stack) | No | Fine speed, weaker text, repaint-loop traps, still a GL context at start. |
| C + custom engine (what File Pilot does) | No | Same result as option 1 with no crates and no cargo tooling. |
| Tauri / WinUI / WPF | No | This is why Files and OneCommander are slow. |

File Pilot itself is C with a custom immediate-mode UI and moved its renderer to Direct3D in v0.7.1, so the chosen shape matches the app whose speed we want.

**Multi-platform** is kept possible, not built now:
- All logic lives in `tsubame-core`, which has no `windows` dependency.
- The UI kit draws through one small `Canvas` trait (about 15 calls, the same set as blink's `Gfx`). Direct2D is the first backend. A second backend (Skia or vello) plus a `tsubame-platform-*` crate is the Linux/macOS port.

**Plugins:** three tiers, section 4.

### P0 outcome (2026-11-17, user-approved)
Spikes measured in `docs/spike-*.md` and `docs/shell-threading.md`. Decisions that now bind every task:
- **Enumeration:** `NtQueryDirectoryFile` class `FileIdExtdDirectoryInformation`, 64 KB buffer, streamed. `FindFirstFileExW` only as fallback (no file id). Never 1 MB or 16 KB buffers. Cold sorted 100k is 187 of 200 ms: stream and sort incrementally.
- **Entry record:** 24 bytes plus one name buffer (1M rows = 45 MB), not S2's 48-byte bench record.
- **Memory budget:** relaxed as in the section 2 table. Keep the layout cache at 1024 entries / 4 frames. Scroll offsets are `f64` (or row index + sub-row offset).
- **Wasm host:** ship `tsubame-plughost` without Cranelift (0.86 MB), load only `.cwasm` files our own install step compiled (never from a plugin package), plain `list<string>` API, batches capped at about 2,000 paths.
- **Shell:** workers on STA threads exactly as `docs/shell-threading.md` says (F10 handled in the app, foreground handed back after a worker menu, menu HWNDs owned by the worker).
- **Icon:** both cuts stay (small cut for 16 and 24 px).

### Things the spikes had to confirm (measured in P0; kept for history)
These are targets, not facts. Phase 0 measures them and the plan changes if they fail.
- Directory enumeration: no source benchmarks `NtQueryDirectoryFile` (`FileIdExtdDirectoryInformation`, large buffer) against `FindFirstFileExW` (`FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`). Both are candidates.
- Wasmtime: host size, cold spawn time and per-call cost for our call pattern are unmeasured.
- MFT index crates (`usn-journal-rs`, `ntfs-reader`, `ntfs-core`): one README number only (full MFT scan 3.8 s cached). All need admin rights.

## 2. Budgets (gates, checked by `tsubame-bench` on a dedicated benchmark machine)

| Metric | Target |
|---|---|
| Cold start to first painted frame | < 100 ms |
| Open a 100,000-entry folder (NVMe): first rows / complete and sorted | < 30 ms / < 200 ms |
| Scroll paint, 4K, details view | p95 < 5 ms |
| Idle CPU | 0 wake-ups (no timers; event-driven only) |
| Private bytes, one tab, small folder (GPU render target) | < 90 MB windowed, < 110 MB at 4K (changed 2026-11-17 from 40 MB: the GPU target alone costs 45-50 MB, see `docs/spike-d2d.md`). The leak gate below is the real guard. |
| Main exe (no wasm host) | < 6 MB |
| **Leak gate:** navigate 10,000 folders, open and close 1,000 tabs | private bytes return to within 5% of the start |

The leak gate runs in every phase from P1 on. It is the reason this project exists.

## 3. Architecture

```
tsubame/                   workspace
  crates/
    tsubame-core/             portable. Entry store, sort/filter, selection, navigation history,
                           file-op queue + undo journal, config, command registry, extension registry.
    tsubame-win/              Windows platform. Enumerate, watch (ReadDirectoryChangesW), IFileOperation,
                           recycle bin, shell icons/thumbnails/context menu, OLE drag-drop, clipboard.
    tsubame-ui/               Immediate-mode kit over `Canvas`: layout, virtual list/grid, text input,
                           menus, tabs, splitters, scrollbars, popups, focus and keyboard routing.
    tsubame-d2d/              `Canvas` backend. Lifted from blink's `src/gfx.rs`.
    tsubame-ext-api/          Extension traits + WIT file. Used by built-ins and by the wasm host.
    tsubame-plughost/         Separate exe. Wasmtime. Started only when a wasm plugin is enabled.
    tsubame-bench/            Fixture generator, benchmarks, leak gate.
  src/                     The app binary: views and wiring.
  mocks/                   HTML mocks + tokens.json.
  docs/
```

Rules every task follows:
1. **The UI thread never touches the disk or COM shell objects.** Workers send batches over a channel and post a wake message. Every request carries a generation number; stale results are dropped.
2. **Entries are stored as arrays, not objects.** One contiguous name buffer, fixed 32–40 byte records, a `Vec<u32>` sort index. No per-entry allocation. One million entries must fit in about 60 MB.
3. **Only visible rows are measured and drawn.** Icons and thumbnails are requested for visible rows only, cached in a bounded LRU, and cancelled when scrolled away.
4. **Shell COM runs on dedicated STA worker threads.** Third-party shell extensions crash and leak. If the leak gate shows them leaking, move them into a helper process (P6 task).
5. **Every cache has a hard cap and an eviction test.** No unbounded maps.
6. **Dependencies stay tiny** (`windows`, `serde`, `toml`, little else in the main exe).

## 4. Plugins

"Everything above the basics is a plugin" is the goal, with one honest limit: a wasm call per row per frame would break the paint budget. So there are three tiers on one registry.

| Tier | What | Cost | Used for |
|---|---|---|---|
| 0. Built-in extensions | Rust, compiled in, registered through the same `tsubame-ext-api` traits third parties get. Each can be switched off. | Zero | Every feature in P4. Proves the API is enough. |
| 1. Declarative packs | A TOML file. No code. | Zero | Custom commands (run a tool with `{path}` `{selection}`), context-menu entries, keybinds, themes, icon packs, file-type colours and rules. |
| 2. Wasm components | WIT interface, run in `tsubame-plughost.exe`, never in the main process. | Paid only when used | Third-party columns, previewers, virtual file systems, search providers. |

Extension points (the same list for all tiers): `command`, `context-menu item`, `column` (batched: N paths in, N values out), `previewer`, `vfs provider`, `search provider`, `side panel`, `file-type rule`, `theme`.

Wasm rules: batch calls only; a capability manifest (which paths, network yes/no, run processes yes/no) shown to the user at install; plugin UI is a declared tree of rows/text/images/buttons that Tsubame draws, never direct drawing; a crashed or slow host is killed and the UI carries on.

**Core, not a plugin:** listing, navigation, tabs, panes, selection, sort/filter, file operations with undo, drag-drop, clipboard, shell context menu, icons, rename, command palette, keybinds, settings, theming.

## 5. Mock-first workflow

Mocks come before UI code.
1. `mocks/tokens.json` is the single source: colours, spacing, radii, font sizes, row heights. A script emits CSS variables for mocks and a Rust `theme.rs` for the app.
2. Before each UI task a static HTML mock is built in `mocks/`. It is approved or edited before the implementation starts.
3. The implementation matches the mock and is verified with screenshots on a real display (4K at 200%).


## 6. Roadmap

Work is split into phases. Each phase has a gate that is checked before the next one starts. Status below is as of 2026-11-24.

| Phase | Scope | Gate | Status |
|---|---|---|---|
| P0 | Spikes: renderer, enumeration, wasm host, shell on worker threads | Budgets reachable, or the plan changes | Done |
| P1 | A skeleton you can browse with | Browse, sort, filter, tabs; leak gate runs | Done |
| P2 | Daily driver: file operations, watcher, shell integration, icons, panes, palette, config | Daily use without data-loss hazards | Done |
| P3 | Extension registry | Built-in features registered through `tsubame-ext-api` | Done (`docs/extensions.md`, `docs/extension-packs.md`) |
| P4 | Features as built-in extensions | Each feature switchable | Partial: F1, F2, F3, F4, F5 done as switchable built-ins; F6 tags, file-age colouring, Tree view and Miller columns done |
| P5 | Wasm plugins | Host lifecycle, capability manifest, SDK | Partial: P1 and P2 done (`docs/wasm-plugins.md`, `docs/plugin-authoring.md`); P3 security review open |
| P6 | Ship: installer, updater, default file manager | Signed release a user can install and update | Mostly done; two items open |

Work outside the original plan that has landed: the memory work (MEM1: flip-model swap chain, WARP prewarm, segment heap), the performance passes (PERF1 to PERF3), the integration pass that unified file operations, panes and drag-drop (INT1), the opt-in default-file-manager shim (R2) and the own-drawn context menu (CM1 to CM5, see `docs/ctxmenu.md`).

## 7. Phases and tasks

### P0. Spikes. Gate: budgets reachable, or change the plan.

| ID | Task | Deliverable | Status |
|---|---|---|---|
| S0 | Workspace, crates as empty stubs, release profile (`lto`, `codegen-units=1`, `strip`, `panic=abort`), `.gitignore` | `cargo check` passes | Done |
| S0b | App icon from the two SVGs in `res/` (16 and 24 px from `tsubame-small.svg`; 32, 48, 64 and 256 px from `tsubame.svg`), embedded with `embed-resource`. Brand colours (navy `#16233f`, cream `#f3ecdf`, red `#e0492c`) in `mocks/tokens.json`. Reference sheet: `mocks/logo-final.html` | The exe shows the icon in Explorer and the taskbar | Done |
| S1 | Direct2D spike: a details list of 1,000,000 fake rows with scrolling | First paint, paint p95, private bytes, exe size in `docs/spike-d2d.md` | Done |
| S2 | `tsubame-bench`: fixture generator (folders of 1k / 100k / 1M files) and an enumeration benchmark of three methods: `FindFirstFileExW` large fetch, `NtQueryDirectoryFile` with 64 KB and 1 MB buffers, `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`; cold and warm cache, NVMe and hard disk | Results table and a recommended method in `docs/spike-enum.md` | Done |
| S3 | Minimal wasmtime component host exe plus one guest that returns a column value for a batch of 10,000 paths | Host size, cold spawn, batch call, idle memory in `docs/spike-wasm.md` | Done |
| S4 | Shell on worker STA threads: system icon for a path, `IShellItemImageFactory` thumbnail, `IContextMenu` popup at a point, all from a non-UI thread | Threading rules in `docs/shell-threading.md` | Done |

### P1. Skeleton you can browse with

| ID | Deps | Task | Status |
|---|---|---|---|
| M1 | none | Mocks: main window (tabs, path bar, sidebar, details list, status bar) in File Pilot's density; `tokens.json` plus generator script | Done |
| C1 | S2 | `tsubame-core` entry store (section 3 rule 2); sort by name (natural order), size, date, type; filter-as-you-type; selection model | Done |
| C2 | S2 | `tsubame-win` enumerator using the S2 winner, streaming batches with generation cancel | Done |
| U1 | S1 | `tsubame-ui` core: `Canvas` trait, immediate-mode ids/focus/hot/active, layout, virtual list, scrollbar, text input, popup layer, keyboard routing | Done |
| U2 | U1, M1, C1, C2 | Details view, path bar with breadcrumbs and edit mode, back/forward/up, tabs | Done |
| B1 | U2 | Leak gate plus startup and folder-open benches in `tsubame-bench`, results appended to `docs/bench-log.md` | Done |

### P2. Daily driver

| ID | Task | Status |
|---|---|---|
| C3 | File-op queue: copy/move/delete/rename via `IFileOperation`, progress, conflicts, undo/redo journal | Done |
| C4 | Directory watcher with event coalescing; applies diffs to the entry store without a relist | Done |
| W1 | Shell context menu, OLE drag-drop in and out, clipboard cut/copy/paste (shell formats) | Done |
| W2 | Icon and thumbnail pipeline: visible-only requests, bounded LRU, cancel on scroll; grid/thumbnail view | Done |
| U3 | Sidebar (drives, bookmarks, recents), dual pane and splits, inline rename, status bar | Done |
| U4 | Command palette, rebindable keys, settings screen | Done |
| D1 | Config load/save (TOML, atomic write), session restore, CLI arguments (`tsubame <path>`, `--new-tab`) | Done |

### P3. Extension registry

| ID | Task | Status |
|---|---|---|
| X1 | `tsubame-ext-api`: traits for every extension point in section 4, registry, enable/disable; move P2's commands and columns onto it | Done (`docs/extensions.md`) |
| X2 | Tier 1 loader: TOML packs for commands, menu entries, keybinds, themes, file-type rules; two example packs; docs | Done (`docs/extension-packs.md`, `examples/packs/`) |

### P4. Features as built-in extensions

Each one gets a mock first where it has UI.

| ID | Extension | Status |
|---|---|---|
| F1 | Global search: MFT/USN index (admin) with a plain parallel walk fallback; separately, an Everything IPC search provider | Done (built-in extension `tsubame.ext.global_search`, `src/search_ui.rs`) |
| F2 | Preview pane: image, text/code, media info, hex; Quick Look on Space | Done (built-in extension `tsubame.ext.preview`, `src/preview_ui.rs`, `docs/preview-mock.md`) |
| F3 | Archive VFS (zip, 7z, tar) browse and extract | Done (built-in extension `tsubame.vfs.archive`, `src/archive_ui.rs`). Follow-up accepted 2026-11-24: status-bar message for blocked actions, overwrite prompt (skip, replace, rename) |
| F4 | Batch rename with live preview | Done (built-in extension `tsubame.ext.batch_rename`, `src/batchui.rs`; switchable, default on, not required; pattern engine `src/rename.rs` stays core; Settings > Extensions page) |
| F5 | Folder sizes column (background, cached), git status column, checksum command | Done (built-in extensions `tsubame.ext.folder_sizes`, `tsubame.ext.checksum`, `tsubame.column.git`; `crates/tsubame-win/src/meta/`) |
| F6 | Miller columns view, tree view, tags and colours, file-age colouring | Partial: tags and colours done (built-in extensions `tsubame.ext.tags`, `tsubame.column.tags`; `src/tags/`, `src/view_tags.rs`, `docs/f6-mock.md`); file-age colouring done (built-in extension `tsubame.ext.file_age`, `src/age.rs`; date text tint in Details, Grid bar skipped (not trivial), Tree-ready via `AgeFrame`). Tree view done (built-in extension `tsubame.ext.tree`, `src/tree.rs`, `src/view_tree.rs`: Tree rows mode `Ctrl+Shift+4` and folders-only pane `Shift+Alt+T`). Miller columns done (built-in extension `tsubame.ext.miller`, `src/miller.rs`, `src/view_miller.rs`: `Ctrl+Shift+5`, one lazily listed column per folder level, info column with preview excerpt, drill-down below 560 px; gaps in the findings log) |

### P5. Wasm plugins

| ID | Task | Status |
|---|---|---|
| P1 | WIT interface from `tsubame-ext-api`, capability manifest, `tsubame-plughost` lifecycle (lazy start, kill on hang), IPC | Done (`docs/wasm-plugins.md`) |
| P2 | Guest SDK crate, two sample plugins (a column, a previewer), plugin author docs | Done (`plugins/tsubame-plugin-sdk`, `plugins/sample-file-kind`, `plugins/sample-csv-summary`, `docs/plugin-authoring.md`; e2e in `crates/tsubame-plughost/tests/samples.rs`) |
| P3 | Security review of P1; set a `tsubame-plughost.exe` size budget | Review and budget done (`docs/security/plughost-review.md`); fix cards A to L open |

### P6. Ship

| Item | Status |
|---|---|
| Installer and portable zip, signed update manifest, release pipeline (`docs/release.md`) | Done |
| Self-update (`tsubame-update`, `docs/updater.md`) | Done |
| Nightly test channel: opt-in `[updates] channel = "nightly"`, rolling `nightly` pre-release from `.github/workflows/nightly.yml` after green `ci` on main, signed `tsubame-update-nightly.json`, `TESTERS.md` (`docs/release.md` "Nightly channel", `docs/updater.md` "Channels") | Built and tested locally; workflow not run on GitHub yet (owner's one-time steps in `docs/release.md`) |
| Set as default file manager, opt-in and fail-safe (`docs/default-file-manager.md`) | Done |
| Shell helper process, only if the leak gate shows shell extensions leaking | Not done |
| Decide on a second `Canvas` backend for Linux/macOS | Not done |
| Release gate: scripted rig QA checklist (preview, Quick Look, search, archive, columns, mouse, context menu, idle CPU) run before every tag | Documented (`docs/qa-checklist.md`, `docs/release.md`); automation not done |
| Release gate: short manual real-mouse list in `docs/release.md` for input that synthetic tests cannot cover (Back/Forward buttons) | Documented (`docs/qa-checklist.md`, `docs/release.md`); automation not done |
| Release gate: Defender and SmartScreen smoke test on a clean VM for each unsigned release | Documented (`docs/qa-checklist.md`, `docs/release.md`); automation not done |

## 8. Open decisions (defaults apply if nobody answers)

1. **Name.** Decided 2026-11-17: Tsubame (燕, swallow: fast, and a sign of a safe house). Logo concepts: `mocks/logos.html`.
2. **MFT search needs admin.** Default: optional elevated helper, off until enabled; the Everything provider and the walk fallback need no admin.
3. **Linux/macOS.** Default: not before P6.
4. **Plughost in the release.** Decided 2026-11-24: `v0.2.0` ships without `tsubame-plughost.exe` (release pipeline unchanged). Revisit when the guest SDK and sample plugins (P5.P2) land.

## 9. Findings and improvements log

Kept current as work lands: every measurement surprise, QA finding or improvement idea goes here, newest first. Entries marked *proposed* need a user decision before they become tasks above.

| Date | Finding | Status |
|---|---|---|
| 2026-11-25 | UIA for the file views (`docs/uia-views.md`): Details and Grid rows are `List`/`ListItem` (name = file name plus `, tags: …`, value = type, size, modified and the age bucket, selection state, Invoke), Tree rows and the folders pane are `Tree`/`TreeItem` (level, position and size in set, ExpandCollapse, Invoke, Select), each Miller column is a `List` named by its folder with the selected row marked, and the tags picker's rows (and the New tag form's swatches and buttons) can be invoked. One new snapshot (`ViewSnap`) in `tsubame-win/src/uia/view.rs`, built by `src/uia_rows.rs` hooks after the list/grid/tree/Miller widgets only while `UiaClientsAreListening`; visible rows plus the surface's total only, no entry store reads beyond the drawn rows; requests run on the UI thread via `WM_APP_UIA` (`src/view_uia.rs`). Tests: 7 provider tests in `tsubame-win`, 5 collector tests and 5 whole-frame tests (names, tags, total vs visible, expand state, Miller columns, select and invoke requests, nothing built with no listener); full workspace passes except the old flaky `tab::tests::big_bursts_resolve_off_the_ui_thread_and_land_in_order` (passes alone). Rig (4K, 200%, debug-optimized, PowerShell `System.Windows.Automation`): Tree `'G:'` with `TreeItem`s (Invoke, Value, ExpandCollapse, SelectionItem), `Expand` opened a folder and its children appeared in the list, `Select` moved focus to the row; Details `List` with `ListItem`s and `Invoke` entering the folder; Miller `List` with the first row selected. Scroll paint p95 3.28 ms on 100,000 rows with no client (limit 5 ms; the hooks cost one bool per frame then); release `tsubame.exe` 5,261,824 B. Not measured: scroll p95 or idle CPU with a client attached (the bench has no UIA client), Tree and Miller scroll (the bench scrolls Details only), Narrator/NVDA listening (only the UIAutomationClient was used). Gaps: Tree rows are a flat list (nesting only through `Level`), Grid is a flat List without a 2-D Grid pattern, no column header items, no editor or drag providers, scrolled-off Miller columns have no provider. | Done (gaps listed) |
| 2026-11-25 | P4.F6 Miller columns landed as `tsubame.ext.miller` (`src/miller.rs` model, `src/view_miller.rs` paint, `Ctrl+Shift+5`). Each column owns its own `EntryStore` and its own enumerator, watcher and sort/filter copy (the mock assumed one shared store; sort and filter stay pane-wide by pushing the rules to every column). The tab keeps no listing of its own while Miller shows (`Tab::drop_listing`), `tab.path` and history follow the active column, leaving Miller lists the folder again. At most 12 live columns, a column lists at most 100,000 entries, 80 ms to the skeleton, Esc cancels, loop guard on links runs on a worker. `Ctrl+Shift+3` is `view.grid-large` and `Alt+T` is the tags picker, so the mock's `Ctrl+Alt+3` became `Ctrl+Shift+5`. Tests: 26 Miller tests in the app crate (model, keys, caps, states, links, watcher, widths, view wiring) plus 1 config test; workspace passes except the old flaky `tab::tests::big_bursts_resolve_off_the_ui_thread_and_land_in_order` (5 s timeout under a full parallel run, passes alone). Rig (4K, 200%, debug-optimized build, slot 3): open, drill, 6 columns plus info with a left cue "1 hidden", drag resize, narrow drill-down (1000 px window), dark and light, empty, loading skeleton on 31,318-entry WinSxS, access denied with Retry, junction Follow, loop card, preview excerpt (text and hex), Settings > Extensions off/on all behave. Idle CPU delta 0.016 s over 20 s with Miller open, working set 42.8 MB. `shipping tsubame.exe` 9,315,328 B is the debug-optimized rig build, so the 6 MB release budget was not measured. Not measured: scroll p95 (`tsubame-bench` scrolls only the Details list and grid, so it cannot drive Miller), cold start. Found on the rig: a mode switch by key painted one frame before the model existed and an idle window never ran another pump (fixed: the model is made at paint time); a typed path that does not exist made a row of identical denied cards (fixed: columns after the first failure drop). Gaps: no drag from Miller columns, rename blocked (message), single selection only (no multi-select, band or marquee), a typed path that fails stays on its denied card instead of reverting, drill-down shows an info strip instead of the full info column, no UIA List/ListItem yet (provided 2026-11-25, see the UIA views row above), `miller.col_width` has no settings control, link and denied card text is cut with an ellipsis instead of wrapping, double-click on the grip did not reset the width in the rig run (synthetic double-click, not confirmed in a real mouse run). | Done (gaps listed) |
| 2026-11-25 | Nightly channel built. Version rule `X.Y.(Z+1)-nightly.N` (N = workflow run number), so a nightly sorts above the stable it follows and below the next one; a nightly copy also checks stable and is offered the next stable; switching back to stable never downgrades and drops a staged nightly. The signed manifest `channel` must match the source it came from, so a nightly manifest served as stable (or the reverse) is refused. `tools/release-e2e.ps1` 40/40 including the nightly path; `tools/lint-workflows.py` clean on all three workflows; rig: Settings > Updates channel switch reads and writes `config.toml`, warning shows only for nightly. Not verified: the workflow on GitHub (gh release/tag commands, compare API notes, asset cleanup) until the owner's first manual run. A fresh install from the nightly installer still defaults to the stable channel until the tester switches it. A nightly check costs 2 API requests instead of 1. | Built; first GitHub run pending |
| 2026-11-25 | P4.F6 tags and colours built as `tsubame.ext.tags` + `tsubame.column.tags`: central `tags.db` keyed by (volume serial, file id) with a path hint, owned by one worker thread (lazy open, coalesced saves, one reconcile per volume that pauses while the view scrolls), view reads through a 4,096-row LRU, soft cap 100k items / 16 MB with a warning at 90%. Rig (4K, 200%, debug build, 3,000-file folder all tagged): scroll paint p95 3.58 ms vs 2.76 ms with an empty store, idle CPU 0.0 ms over 30 s, `tags.db` 242 KB for 3,003 items (about 80 B each). Choices made while building: a click outside the picker applies it (Esc cancels); the context menu gets one "Tags…" row that opens the picker instead of a submenu; the Tags column is opt-in (`[tags] column`); export/import use a fixed `tags-export.json` next to `config.toml` (no file dialog yet); a rename moves path-keyed tags at submit time, not when the shell confirms. *Proposed:* `Alt+T` is also the approved chord for the future tree pane, so one of them needs another chord in the keybinding pass; the picker's UIA rows can be read but not invoked (fixed 2026-11-25: they are invokable now). | Done (tree-pane chord open) |
| 2026-11-25 | P4.F6 file-age colouring landed as `tsubame.ext.file_age`. Bench on rig (debug-optimized profile, 1M synthetic rows, 4K, 3 interleaved runs each): scroll paint p95 off 3.69 ms median, date-text on 3.56, chip on 3.62: no measurable change. Idle CPU delta 0 over 30 s (on and off). Grid thumbnail bar skipped (not trivial, needs a second tint path in the thumbnail painter). Open: Miller, tree and tags still open under F6. | Done |
| 2026-11-25 | Plughost cards C and E: install record (`src/install_record.rs`, written once, verified before each start; capabilities come from the record), protected folders in `Load` (protocol version 2; package and settings read-only, cache sealed, checked on the handle's final path), one cache folder per id and version, capped reads; `$input` scoped by entry-point kind (column and previewer flat, search and command selection recursive, command folder flat). Host exe 1,070,592 B. The app still loads no plugins, so the record and protected folders are tested in the crate only. | Fixed: PH-02, 03, 10, 12, 18 (compile bound still open: run `tsubame-plugc` in a job) |
| 2026-11-25 | P5.P2 guest SDK and samples: sample `.cwasm` files are 327 KB (file kind column) and 426 KB (CSV previewer); `.wasm` 88 KB and 122 KB. The SDK exports every WIT interface and answers the unused ones with errors, so a plugin of one kind still works against the host. vfs, search and command have no SDK trait yet. The e2e test skips when `wasm32-wasip2` is missing unless `TSUBAME_REQUIRE_WASM_SAMPLES` is set; decide whether CI sets it. | Done |
| 2026-11-25 | Tags and colours landed (`tsubame.ext.tags`, central `tags.db`, picker, sidebar filter, Tags settings page). Scroll p95 3.58 ms with 3,000 tagged rows, idle 0, about 80 B per tagged item. Open: `Alt+T` is both the tags picker and the approved Tree pane chord (settle in the keybinding pass); file-list rows have no UIA names yet, so tags are not announced to screen readers (fixed 2026-11-25: file rows are named with their tags, see the UIA views row above); export/import uses a fixed `tags-export.json` (no file dialog). | Done |
| 2026-11-25 | P4.F6 Tree view landed as `tsubame.ext.tree`: Tree rows (Details becomes hierarchical, lazy expand through the normal enumerator, loading row after 80 ms, empty and access-denied rows with Retry, junction badge plus target, follow only on explicit action with an ancestor loop guard) and a separate folders-only pane. Caps: recursive expand 500 folders, 5,000 children per folder, 32 live watchers; the status bar reports every cap. Model is 12 B per visible row plus one path per expanded folder. `Alt+T` conflict resolved: pane is `Shift+Alt+T` (`pane.tree`), view is `Ctrl+Shift+4` (`view.tree`). Rig (4K, debug-optimized): idle CPU delta 0 s over 20 s with the tree open, 39 MB working set. The repo bench uses a synthetic store, so it cannot build Tree rows: Details scroll p95 3.45 to 3.57 ms over 3 runs (the same widget paints Tree rows, visible rows only). Gaps: rename is blocked in Tree rows (use Details); UIA Tree/TreeItem not yet provided (provided 2026-11-25, see the UIA views row above); no bench path for Tree rows on a real folder. | Done; Miller next |
| 2026-11-25 | File-age colouring (`tsubame.ext.file_age`) and batch rename as a built-in extension (`tsubame.ext.batch_rename`) landed, with a Settings > Extensions page listing every optional built-in. Age: scroll p95 3.56 to 3.69 ms at 1M rows (debug-optimized, budget < 5 ms), idle 0. Gaps: Grid thumbnail bar skipped, folder chip missing on one future-dated row (not investigated), Tree tint waits for the Tree view. | Done |
| 2026-11-25 | P4.F4/F6 mocks approved with all recommendations (`docs/f6-mock.md`, `mocks/mock-f6-*.html`, `mock-f4-batch-rename-ext.html`): Miller (header per column, pane-wide sort, drill-down below 560 px), Tree (separate `Alt+T` pane, files and folders, junction follow on explicit action, caps 500/5,000), Tags (central `tags.db`, 8 named tags, shapes on, `Alt+T` picker), Age (off by default, date text + Heat palette, since midnight 7/30/365 days, no folder tint), F4 (switchable built-in, default on, engine stays in core). Mocks only have dark and light: the other basic themes still need adding to the mock tokens. | Approved; Age, Tags, F4 and Tree landed; Miller next |
| 2026-11-25 | Plugin host security review (`docs/security/plughost-review.md`): 0 critical, 4 high, 6 medium, 11 low. The Wasm layer holds; the weak spots are around it (UNC and `*` grants reach the network, read-write grants can cover the compiled-module cache, no install record pins approved capabilities, dangling-symlink and hard-link escapes, host runs with the full user token). 13 `#[ignore]` PoC tests. Wasm plugins stay unwired in the app until cards A to F land. | Fixed: PH-01, 04, 05, 06, 07 (tier 1), 09, 13b, 16, 17 (cards A, B, D, F, merged). Open: C and E wait for owner decisions; G, H, I, J, K, L later. Host exe 1.05 MB after the fixes. |
| 2026-11-25 | Guest SDK, two sample plugins (file kind column, CSV previewer) and `docs/plugin-authoring.md` landed (P5.P2). | Done |
| 2026-11-25 | Real-mouse check on the user's machine: Back/Forward buttons and double-click-up work; marks the manual real-mouse gate as passed for the 0.2 build. | Done |
| 2026-11-25 | Middle-click on the last open tab freezes the UI for a noticeable time before the window closes (also on 0.1.0). Cause: closing the last tab emptied the tab list and the next paint panicked (`panic=abort`), so Windows Error Reporting froze the window 3 to 4.5 s and the session was not saved. Fixed in `2fa51c7`: close in 4.9 to 21 ms, exit under 70 ms, session saved, no WER event. A last-tab-close regression test was added. | Fixed |
| 2026-11-25 | First right-click context menu after launch is slightly laggy on the user's machine (many third-party shell extensions); warm opens are fine. Cause: cold first menu held the popup invisible for the 60 ms grace while shell extensions loaded (about 550 to 600 ms off the UI thread), and an early click could start a second packaged-handler scan. Fixed in `2fa51c7`: cold first paint 65.7 ms to 2.7 ms on rig; warm opens unchanged (p95 43.6 ms). FOX's third-party extensions still load off-thread; `TSUBAME_TRACE_LAG=<file>` traces them if it still feels slow. | Fixed, verify on FOX |
| 2026-11-25 | Release `tsubame.exe` built as `-p tsubame -p tsubame-handler` is 4.63 MB vs 3.50 MB for `-p tsubame -p tsubame-plughost`; both under budget. Find which feature unification adds about 1.1 MB. | Open |
| 2026-11-25 | Archive delete/rename/cut/paste/new/drop-in now show 'Archives are read-only. Extract first.'; extract onto existing files asks Replace / Keep both / Skip (`3b01718`). Rename, cut, paste, drop-in and right-click menus not rig-checked individually. | Fixed |
| 2026-11-24 | Release `tsubame.exe` is 3.34 MiB after the Batch 8 archive crates (budget < 6 MB, 0.1.0 was 3.20 MB). | Within budget |
| 2026-11-25 | `tsubame-plughost.exe` is 1.00 MiB (1,051,136 B) from `cargo build --release -p tsubame-plughost`. The 9 MB figure came from a Cranelift build (`cargo test --release` or `--features compiler`) overwriting the same output path. Budget: host 1.5 MiB hard fail in `release.ps1`; `tsubame-plugc.exe` 11 MiB (9.05 MiB today). Do not use a bare '< 12 MB'. | Resolved; guard is card K |
| 2026-11-24 | Rig QA of Batch 8 (preview pane, Quick Look, global search, archive VFS, git and checksum columns, mouse UX, context menu): 24 pass, 1 fail, 4 partial. No crash or hang, idle CPU delta 0.000 s. | Done |
| 2026-11-24 | Zip entry times display +2 h (DOS local time treated as UTC). | Fixed (`67eb12a`). Zip extended timestamp 0x5455 not read (slows listing); optional follow-up |
| 2026-11-24 | List keyboard focus is lost after inline-rename Esc and after the command palette closes (`Ctrl+Shift+F`, `Alt+Enter` stop working until a row is clicked). | Not reproduced on rig; defensive refocus added when the palette closes (`15ac858`) |
| 2026-11-24 | First breadcrumb click out of an archive was swallowed once. | Not reproduced; watch for it |
| 2026-11-24 | `archive.extract_here`, `archive.extract_selected`, `file.checksum`, `folder.calc_sizes` have no default key binding. | Open: no obviously free chord; needs a keybinding pass |
| 2026-11-24 | Delete and rename inside an archive are blocked silently; extract overwrites existing files without asking. | Fixed, see next row |
| 2026-11-24 | Mouse Back/Forward buttons passed only 1 of 7 synthetic-input tries. Source handles them; `Alt+Left/Right` work. | Needs one check on a real mouse |
| 2026-11-24 | Preview pane default shows 564 px wide at 200% scale (config default is 320 DIP, so about 640 px). | Check the DIP to px conversion |
| 2026-11-25 | P4.F4 shipped: batch rename is the built-in extension `tsubame.ext.batch_rename` (switchable, default on, not required). Added `file.batch_rename` (id 72, no default key; F2 stays on `file.rename`, which delegates), a "Rename N items" context-menu row, and a new Settings > Extensions page (one switch per optional built-in; live via `set_extension_enabled`). Off: command and menu row gone, open bar closed, F2 on several items renames the focused item inline and the status bar says "Batch rename is switched off (Settings > Extensions). Renaming one item." Engine untouched. Rig (slot 1, debug): on, off and the settings page checked by screenshot; idle CPU 0.016 s over 15 s (one timer tick) | Done |

## Sources

- File Pilot roadmap and release notes: https://filepilot.tech/roadmap
- File Pilot internals (C, custom renderer, immediate-mode UI): https://filepilot.handmade.network/ , https://voyager.handmade.network
- Enumeration: https://ntdoc.m417z.com/file_id_extd_dir_information , https://cppstories.com/2024/cpp-query-file-attribs-faster
- MFT/USN crates: https://docs.rs/usn-journal-rs , https://docs.rs/crate/ntfs-reader/latest , https://docs.rs/crate/ntfs-core/0.9.0
- Plugins: https://github.com/extism/extism/issues/666 , https://news.lavx.hu/article/the-rust-plugin-puzzle-navigating-trade-offs-in-extensible-system-design
- GPUI on Windows: https://zed.dev/blog/windows-progress-report
