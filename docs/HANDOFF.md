# Handoff

Updated 2026-11-17 by the P0 master session.

## State
- P0 finished. User approved all four decisions (2026-11-17) and said go; they are now written into PLAN.md (P0 outcome block, relaxed memory budget). **P1 started in this same master session**: M1, C1, C2, U1 running in parallel. U2 waits for U1 + M1 (user approves the mock) + C1 + C2; B1 waits for U2.
- `cargo check --workspace` passes on `main`. Nothing was pushed.
- Disk: main `target` 740 MB. No worktrees left. Six empty folders remain under `C:\Users\MIcha\.t3\worktrees\tsubame\` (git removed the worktrees but T3 holds the folders open; delete them by hand, they are 0 MB).
- Rig is clean: no tsubame processes, fixtures deleted. `G:\rig-test\tsubame` still has the S3 wasm host exes and `.cwasm` (small).

## Tasks
| ID | State | Result |
|---|---|---|
| S0 | merged | Workspace, 7 crate stubs, release profile from blink, `windows = "0.61"`. |
| S0b | merged | `res/tsubame.ico` (16, 24 small cut; 32, 48, 64, 256 master), manifest (PerMonitorV2, longPathAware), version info, `tools/make-icon.ps1`, brand colours in `mocks/tokens.json`. Not seen in a live Explorer/taskbar yet (verified with `SHGetFileInfoW` on FOX and rig). |
| S1 | merged | Direct2D 1M-row list. `docs/spike-d2d.md`. |
| S2 | merged | Enumeration bench. `docs/spike-enum.md`. |
| S3 | merged | Wasmtime host. `docs/spike-wasm.md`. Ran on Sonnet 5.5 because GPT hit its usage limit. |
| C1 | merged | Core entry store (24-byte record, 50.0 MB at 1M, name sort 100k 9.6 ms / 1M 139 ms, filter 100k 2.6 ms, ID-based selection). See docs/core-store.md. Type sort at 1M is 470 ms (no gate). Selection is by entry ID with bitset + epoch. |
| M1 | merged | Main-window mocks (dark + light, 16 shots in `mocks/shots`), `mocks/tokens.json`, `tools/gen-tokens.ps1` -> `mocks/tokens.css` + `mocks/generated/theme.rs` (not wired into a crate yet). **Waiting for user approval** of the mock decisions (accent blue, own-drawn context menu, no shadows, solid caret, loading line). |
| C2 | merged | Streaming enumerator in `tsubame-win` (`Enumerator::start`, NT 64 KB, Find fallback, cancel token, 64-byte `RawEntry` batches). 100k NVMe warm: first batch 0.50 ms, done 33.6 ms. 24 tests pass. See docs/enumerator.md. Cold not re-measured (S2: 13 / 175 ms). |
| S4 | merged | Shell on STA workers. `docs/shell-threading.md`. Opus 5.5. |

## Measured numbers against PLAN.md section 2
| Budget | Target | Measured | |
|---|---|---|---|
| Cold start to first frame | < 100 ms | 65-72 ms (4K, software first frame, GPU swaps in at about 290 ms) | pass |
| Open 100k folder, first rows | < 30 ms | 0.4 ms warm, 13 ms cold | pass |
| Open 100k folder, complete and sorted | < 200 ms | 47 ms warm, 187 ms cold (p95 199) | pass, thin cold margin |
| Scroll paint p95, 4K | < 5 ms | 3.7 ms; 2.9-3.1 ms with icons streaming; 2.75 ms with both shell workers hung | pass |
| Idle CPU | 0 wake-ups | 0.00 s over 5 s | pass |
| Private bytes, one tab, small folder | < 40 MB | 73 MB first paint at 4K, 111 MB steady at 4K, about 85 MB windowed (GPU target). 36 MB windowed with software only, but paint p95 is 7-18 ms | **fail** |
| Main exe | < 6 MB | 0.23 MB (S1 spike) | pass |
| Leak gate | within 5% | 200 context menus: private bytes -20% (no growth), handles +4, GDI +0. 500 thumbnails: -29%, GDI 45 -> 45. Wasm host: flat over 3,000 calls | pass (shell and wasm only; the full 10,000 folders / 1,000 tabs gate comes with B1) |

## Other numbers worth keeping
- Store: 1M rows = 45 MB with 24-byte records, builds in 47 ms. S2's bench record is 48 B = 86 MB at 1M, over the 60 MB rule, so C1 uses the small record.
- Enumeration: 1M files warm 420 ms, cold about 2 s. 1 MB buffer fails first rows cold (107 ms); 16 KB buffer takes 781 ms.
- Shell: icon warm 0.74 ms by path, first icon 13.7 ms; thumbnail 23 ms generated / 3 ms cached; first context menu 0.5-1.3 s (shell extensions load), warm 34 ms.
- Wasm host: 0.86 MB without Cranelift (9.3 MB with), spawn 6 ms precompiled (63 ms compiled), 10,000-path round trip 3.5 ms, kill and respawn 6.4 ms, 1.8 MB private.

## Decisions the user must make before P1
1. **Memory budget (fails).** The GPU render target costs about 45-50 MB over software; software rendering misses the paint budget. Options: (a) change the budget to 90 MB windowed / 110 MB at 4K and rely on the leak gate (recommended); (b) spike a DXGI flip-model swap chain with a shared D3D11 device, which may shrink the footprint; (c) software below a window size.
2. **Wasm plan changes (S3).** Ship `tsubame-plughost` without Cranelift (0.86 MB) and load only `.cwasm` files our own install step compiled; plain `list<string>` API; cap batches at about 2,000 paths.
3. **Enumeration.** Use `NtQueryDirectoryFile` class `FileIdExtdDirectoryInformation` with a 64 KB buffer; `FindFirstFileExW` only as fallback (no file id). Cold sorted margin is thin (187 of 200 ms), so C2 and C1 must stream and sort incrementally.
4. Drop the small-cut icon and ship one SVG? Asked earlier, not answered. The small cut is in use.

## Rules learned in P0 (for the next master)
- On a model usage limit, relaunch the same brief on another provider's similar-tier model without asking, and `task_cancel` the limited task so it does not auto-resume (user rule, saved in memory). Log the swap here.
- Always pass `-Exe tsubame` (or the crate's exe) to `push-to-rig.ps1`: several crates share one repo and a slot.
- Scroll offsets must be `f64` or first-row index plus sub-row offset: 1M rows x 22 DIP exceeds f32 whole-pixel precision.
- The layout cache of blink is useless for a scrolling list; S1 re-tuned it to 1024 entries / 4 frames.
- Shell: handle F10 in the app (Shift+F10 puts the UI thread in menu mode); hand the foreground back to the app window after a worker menu closes; menu windows must be owned by the worker thread.
- Merging: `Cargo.lock` and root `Cargo.toml` conflict between parallel children; resolve by keeping both `Cargo.toml` sides and re-running `cargo check` to regenerate the lock.
- Worktree folders under `~\.t3\worktrees\tsubame\` stay locked after `git worktree remove`; git side is clean.

## Next batch (P1), after the user says go
Start now, in parallel (check PLAN.md sections 6 and 7 for models):
- M1 (Sonnet): mocks for the main window and `tokens.json` generator. User approves before U-tasks start.
- C1 (GPT, or a similar-tier substitute if limited): `tsubame-core` entry store, 24-byte record, natural sort, filter, selection. Uses S2's numbers.
- C2 (DeepSeek): `tsubame-win` enumerator using the S2 winner. Start from `crates/tsubame-win/src/enumerate.rs` (S4's simple version).
- U1 (Opus): `tsubame-ui` core. Uses S1's `gfx.rs` surface (docs/spike-d2d.md, last section) and S4's `src/shellui.rs`, `src/lru.rs`.
C1 and C2 edit different crates. U1 edits `tsubame-ui` and may touch `tsubame-d2d`.
U2 waits for U1, M1 (approved), C1, C2. B1 waits for U2.

## Update (P1 in progress, same master session)
- U1 merged: `tsubame-ui` kit + `Canvas` trait, `impl Canvas for Gfx`, app list/filter/scrollbar on the kit. Paint p95 3.6-3.8 ms, first paint 55-74 ms, idle 0. **Private bytes at 4K end of scroll 113-116 MB, over the 110 MB budget** (old app is the same, 115-117 MB; DirectWrite heap). Fix idea: draw row text as glyph runs; layout cache 256 gives 106 MB but p95 4.7 ms. Open: accept ~116 MB or schedule the glyph-run task. `docs/ui-kit.md`.
- User answers on the M1 mock (2026-11-17): accent blue OK; **ship all basic themes** (copy the theme sets from hopper and spotifast); **density switchable on the fly**; **own-drawn context menu, entries user-editable, with import from Nilesoft Shell config**; zebra on with a toggle; no shadows OK for now; loading line OK if it looks decent; solid caret: user asked for an explanation (blink-while-focused-then-stop proposed, pending answer).
- Context menu source: entries come from the Windows shell itself (`IContextMenu::QueryContextMenu` fills an `HMENU`; the S4 code already does this). Own-drawn menu = read that `HMENU` (text, id, flags, icon bitmap, submenus) and draw it ourselves, `InvokeCommand` with the chosen id. Editable layer on top (hide, reorder, rename, own commands). Needs a W1-time spike; legacy-menu items only, Win11 `IExplorerCommand` items may be missing.
- Remaining P1: U2 (details view, path bar, tabs; needs the kit, the mock, C1, C2: all merged), B1 after U2.

## Update 2: user requests (2026-11-17)
- Caret: **blink while a text field is focused, stop after about 10 s and stay solid** (timer only then; idle stays 0 wake-ups). Tell U2/U-kit tasks.
- **Installer and auto-update must be wired early**, not left to the end of P6: new card **D2 (installer + updater pipeline)** after U2 merges, reusing blink's `X:\Dev\blink\src\update\` (logic.rs, mod.rs). Updates come from GitHub Releases of a **public repo**. No remote exists yet; `gh` is logged in as `watdafawx`. User allowed creating and pushing a public repo. Do it when D2 starts, after scrubbing the docs (local paths such as `C:\Users\MIcha\...` in this file, rig address in tool docs) and after the first release is ready. Confirm the repo name with the user at that point (outward-facing).
- **Opt-in "set as default file manager" feature**: R1 (DeepSeek, research only) is writing `docs/research-default-explorer-defender.md` and `docs/research-installer-updater.md` (Defender / SmartScreen / ASR reactions, mechanisms, signing, installer choice). Implementation card waits for R1. Must be reversible in one click and must never leave folders unopenable (fallback to explorer.exe).
- Themes: all basic themes (copy sets from hopper and spotifast), density switch on the fly, editable own-drawn context menu with Nilesoft Shell import (later cards).

## Update 3
- R1 merged: `docs/research-default-explorer-defender.md`, `docs/research-installer-updater.md`. Recommendation: opt-in only, `HKCU\Software\Classes\Directory\shell` verb pointing to a signed fail-safe shim (`tsubame-handler.exe`) that falls back to `explorer.exe` and removes its own key on failure; never IFEO on explorer.exe (Defender `Behavior:Win32/ifeoDebugger.DB`); skip `Folder\shell` in v1 (UACBypass signature); Win+E separate experimental opt-in; per-user Inno + portable zip, no MSIX; sign everything (Artifact Signing, individuals US/CA only, else OV); updater verifies Authenticode against a pinned key. Confidence: medium-high; top VM test: does Defender tolerate `Directory\shell` writes from a signed app (no source proves it).
- U2 started (Sonnet): details view, path bar, tabs, sidebar, runtime density, theme from generated tokens, caret blink rule, leak-smoke mode.
- Next cards to write: D2 installer + updater (reuse blink `src/update`), T1 themes (hopper/spotifast sets), W-menu (own-drawn editable context menu + Nilesoft import), glyph-run text (memory), default-explorer opt-in (after R1 review), B1 leak gate.

## Update 4: D2 merged (2026-11-18)
- D2 merged: `crates/tsubame-update` (51 tests), `installer/tsubame.iss`, `tools/release*.ps1`, `.github/workflows/{ci,release}.yml`, `docs/release.md`, `docs/updater.md`. Local e2e (update, tamper, bad signature, rollback) and a silent install/uninstall on rig passed. Not run: the workflows on GitHub, real signing, Authenticode, a real update from GitHub. Inno Setup 6 is now installed per-user on rig.
- Production builds refuse all updates until a release public key is pinned in `crates/tsubame-update/src/keys.rs` (`RELEASE_PUBLIC_KEYS_HEX`). Run `tools/release-keygen.ps1`, back up the private key, pin the public key, `gh secret set UPDATE_SIGNING_KEY`.
- **Before the first public push**: git history carries the user's real e-mail as author (`watdafawx <...@hotmail.com>`), tracked files carry `C:\Users\MIcha\...` paths (HANDOFF.md, a sample path in `crates/tsubame-plughost/src/proto.rs`). Needs the user's decision (see the report). No remote has been added and nothing was pushed.
- Next: updater wiring card (Settings > Updates, startup check) after U2 merges.

## Update 5 (2026-11-18)
- U2 on Claude Sonnet hit the Claude 5-hour limit (resets 01:10 Cairo). Partial work is committed on branch `t3/u2-browsable-file-manager` (head `be1aba2`, kept until the continuation merges). Substituted a **Codex gpt-6.1-sol (medium)** continuation child per the user's limit rule; it merges that branch first, then finishes and verifies on rig. Old task cancelled.
- Still waiting on the user: public-repo push decision (scrub history e-mail and local paths first).

## Update 6: U2 merged, B1 started (2026-11-18)
- U2 merged (Codex gpt-6.1-sol continuation of the Claude child). Working file manager skeleton: tabs, breadcrumbs/path edit, back/forward/up, filter, sort, selection, sidebar (drives, known folders), density presets, dark/light, status bar, loading line, native shell menu. `docs/u2-views.md`. Tests green (workspace), screenshots checked on rig (dark, light, tabs, selection).
- U2 numbers (rig 4K release): 100k first rows 2.1-18.3 ms (pass), sorted 62-168 ms (pass), scroll p95 2.8 ms (pass), first paint 72 ms (pass), idle CPU 0 (pass), leak smoke +0.25 MB, exe 0.60 MB. **Fails: private bytes after scroll 149.6 MiB (budget 110; 96.8 MiB idle before scroll passes; 180.9 MiB after repeated 100k navigations); startup navigation first rows 53.6 ms when counted from tab creation (includes window creation; navigation after startup meets 30 ms); filter highlight rectangles missing.** Not measured: cold cache, full 10,000/1,000 leak gate (B1), wake-ups via ETW.
- B1 (DeepSeek) running: full leak gate in the real GUI app + bench harness + `docs/bench-log.md`.
- Open card needed: **memory** (private bytes). Contributors per U2/U1: GPU render target and driver heaps, DirectWrite heap during scroll, warm-up WARP target. Ideas: glyph-run text, smaller layout cache, release the software target after the GPU swap, cap icon cache. Run after B1 gives a baseline.
