# Tsubame (燕): plan

A very fast, feature-rich file browser. Windows first, portable core. File Pilot's look and speed, the features of Files and OneCommander, and no leaks.

Written 2026-11-17 by the planning session. No code exists yet. This file is the handoff: a master session reads it, writes one brief per task card, and delegates.

## 1. Tech stack (decided)

**Rust + raw Win32 (`windows` crate) + Direct2D/DirectWrite, with our own immediate-mode UI kit. Same stack as `X:\Dev\blink`.**

| Option | Verdict | Why |
|---|---|---|
| Rust + Win32 + Direct2D (blink's stack) | **Chosen** | Measured on rig in `X:\Dev\blink\docs\gpu-render.md`: first paint 79–85 ms, paint 3–5 ms at 4K, 2.3 MB exe. `blink\src\gfx.rs` (742 lines) already has rect, text, image, clip and the software-first-frame / GPU-swap / device-loss handling. |
| GPUI (hopper's stack) | No | Cross-platform for free, but hopper needed a memory project (646 MB working set on a 1,188-row list before 0.59.0) and has a 2,000 ms cold-start budget. Wrong weight class. No native shell context menus. |
| egui (spotifast's stack) | No | Fine speed, weaker text, repaint-loop traps, still a GL context at start. |
| C + custom engine (what File Pilot does) | No | Same result as option 1 with no crates, no cargo tooling, and worse output from the executing models. |
| Tauri / WinUI / WPF | No | This is why Files and OneCommander are slow. |

File Pilot itself is C with a custom immediate-mode UI and moved its renderer to Direct3D in v0.7.1, so the chosen shape matches the app whose speed we want.

**Multi-platform** is kept possible, not built now:
- All logic lives in `tsubame-core`, which has no `windows` dependency.
- The UI kit draws through one small `Canvas` trait (about 15 calls, the same set as blink's `Gfx`). Direct2D is the first backend. A second backend (Skia or vello) plus a `tsubame-platform-*` crate is the Linux/macOS port.

**Plugins:** three tiers, section 4.

### Things the spikes must confirm (not yet measured)
These are targets, not facts. Phase 0 measures them and the plan changes if they fail.
- Directory enumeration: no source benchmarks `NtQueryDirectoryFile` (`FileIdExtdDirectoryInformation`, large buffer) against `FindFirstFileExW` (`FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`). Both are candidates.
- Wasmtime: host size, cold spawn time and per-call cost for our call pattern are unmeasured.
- MFT index crates (`usn-journal-rs`, `ntfs-reader`, `ntfs-core`): one README number only (full MFT scan 3.8 s cached). All need admin rights.

## 2. Budgets (gates, checked by `tsubame-bench` on rig)

| Metric | Target |
|---|---|
| Cold start to first painted frame | < 100 ms |
| Open a 100,000-entry folder (NVMe): first rows / complete and sorted | < 30 ms / < 200 ms |
| Scroll paint, 4K, details view | p95 < 5 ms |
| Idle CPU | 0 wake-ups (no timers; event-driven only) |
| Private bytes, one tab, small folder | < 40 MB |
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
    tsubame-d2d/              `Canvas` backend. Lifted from blink\src\gfx.rs.
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
6. **Dependencies stay tiny** (`windows`, `serde`, `toml`, little else in the main exe). X: is a hard disk and target dirs are the main disk hog.

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

Same as blink (`blink\docs\mock-*.html`, `design-tokens.md`).
1. `mocks/tokens.json` is the single source: colours, spacing, radii, font sizes, row heights. A script emits CSS variables for mocks and a Rust `theme.rs` for the app.
2. Before each UI task a Sonnet child builds a static HTML mock in `mocks/`. The user approves or edits it.
3. The implementing child gets the mock path and matches it. It is verified on rig with screenshots (`push-to-rig.ps1`, `rig-windows` Screenshot with `region=`). Never launch the app on FOX.

## 6. Who does what

| Model | `target` | Gets |
|---|---|---|
| DeepSeek V4.1 Flash, up to 3 | `{"providerInstanceId":"opencode","model":"opencode-go/deepseek-v4.1-flash"}` | Scaffolding, fixture generators, bench runs and reports, config/TOML code, mechanical ports from blink, docs, changelogs. |
| GPT, 1 at a time (small limit, spend carefully) | `{"providerInstanceId":"codex","model":"gpt-6.1-sol","options":{"reasoningEffort":"medium"}}` | Dense, well-specified logic with tests: entry store, sort/filter, op queue + undo, watcher coalescing. Second-opinion review of `unsafe` and COM code. |
| Sonnet 5.5, 2–3 | `{"providerInstanceId":"claudeAgent","model":"claude-sonnet-5-5"}` | HTML mocks, widgets, views, anything checked by screenshot on rig. |
| Opus 5.5, 1, never xhigh | `{"providerInstanceId":"claudeAgent","model":"claude-opus-5-5"}` | UI kit core, shell COM (context menu, OLE drag-drop), plugin API + WIT + capability model, hard bugs, anything a cheaper model failed. |
| Master session | — | Writes briefs, reviews every report, merges, removes worktrees, writes a handoff at the end of each phase and starts fresh. |

Every brief must contain:
- Read first: `X:\Dev\tsubame\docs\PLAN.md` sections 2–3, plus the files the card names.
- Build only with `pwsh -NoProfile -File X:\Dev\tools\cargo-ccd1.ps1 <cargo args>`. GUI runs only on rig via `X:\Dev\tools\push-to-rig.ps1 -Project <worktree> -BuildOnRig -Launch`, then `-Stop -Slot N`.
- Builds and tests in the foreground. Never end a turn while a build, test or agent runs.
- Work in the given worktree, commit on its branch, do not push.
- Report: what changed, test output, measured numbers against the budgets in section 2, anything skipped.
- Max 4 sessions building at once across all children.

## 7. Phases and task cards

Times are estimates for agent wall-clock, unverified.

### P0. Spikes (about 1 day, all four in parallel). Gate: budgets reachable, or change the plan.

| ID | Model | Task | Done when |
|---|---|---|---|
| S0 | DeepSeek | Create workspace, crates as empty stubs, release profile copied from blink (`lto`, `codegen-units=1`, `strip`, `panic=abort`), `.gitignore`, `git init`. | `cargo check` passes. |
| S0b | DeepSeek | Build `res\tsubame.ico` from the two SVGs in `res\`: 16 and 24 px from `tsubame-small.svg`, 32, 48, 64 and 256 px from `tsubame.svg`. Embed it with `embed-resource` as blink does. Brand colours (navy `#16233f`, cream `#f3ecdf`, red `#e0492c`) go into `mocks/tokens.json`. Reference sheet: `mocks\logo-final.html`. | Exe shows the icon in Explorer and the taskbar on rig. |
| S1 | Sonnet | Copy `blink\src\gfx.rs` into `tsubame-d2d`. Window that draws a details list of 1,000,000 fake rows with scrolling. | Numbers on rig: first paint, paint p95, private bytes, exe size. |
| S2 | DeepSeek | `tsubame-bench`: fixture generator (folders of 1k / 100k / 1M files) and an enumeration benchmark of three methods: `FindFirstFileExW` large fetch, `NtQueryDirectoryFile` with 64 KB and 1 MB buffers, `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`. Cold and warm cache, NVMe and hard disk. | Table of results in `docs/spike-enum.md`, with a recommended method. |
| S3 | GPT | Minimal wasmtime component host exe + one guest that returns a column value for a batch of 10,000 paths. | `docs/spike-wasm.md`: host exe size, cold spawn ms, batch call ms, idle memory. |
| S4 | Opus | Shell on worker STA threads: system icon for a path, `IShellItemImageFactory` thumbnail, `IContextMenu` popup at a point, all from a non-UI thread into the S1 window. | Works on rig; notes on threading rules in `docs/shell-threading.md`. |

### P1. Skeleton you can browse with (about 2–3 days)

| ID | Model | Deps | Task |
|---|---|---|---|
| M1 | Sonnet | — | Mocks: main window (tabs, path bar, sidebar, details list, status bar) in File Pilot's density. `tokens.json` + generator script. **User approves before U-tasks start.** |
| C1 | GPT | S2 | `tsubame-core` entry store (section 3 rule 2), sort by name (natural order) / size / date / type, filter-as-you-type, selection model. Unit tests + 1M-entry bench. |
| C2 | DeepSeek | S2 | `tsubame-win` enumerator using the S2 winner, streaming batches with generation cancel. |
| U1 | Opus | S1 | `tsubame-ui` core: `Canvas` trait, immediate-mode ids/focus/hot/active, layout, virtual list, scrollbar, text input, popup layer, keyboard routing. |
| U2 | Sonnet | U1, M1, C1, C2 | Details view, path bar with breadcrumbs and edit mode, back/forward/up, tabs. |
| B1 | DeepSeek | U2 | Leak gate + startup and folder-open benches in `tsubame-bench`, runnable on rig, results appended to `docs/bench-log.md`. |

### P2. Daily driver (about 1–2 weeks)

| ID | Model | Task |
|---|---|---|
| C3 | GPT | File-op queue: copy/move/delete/rename via `IFileOperation`, progress, conflicts, undo/redo journal. |
| C4 | GPT | Directory watcher with event coalescing; applies diffs to the entry store without a relist. |
| W1 | Opus | Shell context menu, OLE drag-drop in and out, clipboard cut/copy/paste (shell formats). |
| W2 | Sonnet | Icon and thumbnail pipeline: visible-only requests, bounded LRU, cancel on scroll. Grid/thumbnail view. |
| U3 | Sonnet | Sidebar (drives, bookmarks, recents), dual pane and splits, inline rename, status bar. Mock first. |
| U4 | Sonnet | Command palette, rebindable keys, settings screen. Mock first. |
| D1 | DeepSeek | Config load/save (TOML, atomic write), session restore, CLI arguments (`tsubame <path>`, `--new-tab`). |

### P3. Extension registry (about 3 days)

| ID | Model | Task |
|---|---|---|
| X1 | Opus | `tsubame-ext-api`: traits for every extension point in section 4, registry, enable/disable. Move P2's commands and columns onto it. |
| X2 | DeepSeek | Tier 1 loader: TOML packs for commands, menu entries, keybinds, themes, file-type rules. Two example packs. Docs. |

### P4. Features as built-in extensions (parallel, each mock-first where it has UI)

| ID | Model | Extension |
|---|---|---|
| F1 | GPT | Global search: MFT/USN index (admin) with a plain parallel walk fallback. Separately, an Everything IPC search provider. |
| F2 | Sonnet | Preview pane: image, text/code, media info, hex; Quick Look on Space. |
| F3 | DeepSeek | Archive VFS (zip, 7z, tar) browse and extract. |
| F4 | Sonnet | Batch rename with live preview. |
| F5 | DeepSeek | Folder sizes column (background, cached), git status column, checksum command. |
| F6 | Sonnet | Miller columns view, tree view, tags and colours, file-age colouring (OneCommander). |

### P5. Wasm plugins (about 1 week)

| ID | Model | Task |
|---|---|---|
| P1 | Opus | WIT interface from `tsubame-ext-api`, capability manifest, `tsubame-plughost` lifecycle (lazy start, kill on hang), IPC. |
| P2 | GPT | Guest SDK crate + two sample plugins (a column, a previewer) + plugin author docs. |
| P3 | GPT | Security review of P1 (as a fresh review task). |

### P6. Ship

Installer and portable zip, set-as-default-explorer, self-update (reuse blink's `update/`), shell helper process if the leak gate needs it, then decide on the second `Canvas` backend for Linux/macOS.

## 8. Open decisions (defaults apply if nobody answers)

1. **Name.** Decided 2026-11-17: Tsubame (燕, swallow: fast, and a sign of a safe house). Folder `X:\Dev\tsubame`. Logo concepts: `mocks\logos.html`.
2. **MFT search needs admin.** Default: optional elevated helper, off until enabled; the Everything provider and the walk fallback need no admin.
3. **Linux/macOS.** Default: not before P6.

## Sources

- File Pilot roadmap and release notes: https://filepilot.tech/roadmap
- File Pilot internals (C, custom renderer, immediate-mode UI): https://filepilot.handmade.network/ , https://voyager.handmade.network
- Enumeration: https://ntdoc.m417z.com/file_id_extd_dir_information , https://cppstories.com/2024/cpp-query-file-attribs-faster
- MFT/USN crates: https://docs.rs/usn-journal-rs , https://docs.rs/crate/ntfs-reader/latest , https://docs.rs/crate/ntfs-core/0.9.0
- Plugins: https://github.com/extism/extism/issues/666 , https://news.lavx.hu/article/the-rust-plugin-puzzle-navigating-trade-offs-in-extensible-system-design
- GPUI on Windows: https://zed.dev/blog/windows-progress-report
- Local measurements: `X:\Dev\blink\docs\gpu-render.md`, `X:\Dev\hopper\docs\changelog\cards\0.59.0-memory.md`
