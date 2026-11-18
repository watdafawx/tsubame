# Handoff

Updated 2026-11-18 by the P0+P1 master session. Start a fresh master from this file. Read `docs/MASTER.md`, then this, then `docs/PLAN.md` section 7 (P2).

## State
- P0 and P1 are merged on `main`. `cargo check --workspace` and `cargo test --workspace` pass.
- Repo is public: https://github.com/watdafawx/tsubame (MIT). `main` is pushed up to the key-pin commit; the B1 merge and this handoff are local until pushed.
- Release signing: public key pinned in `crates/tsubame-update/src/keys.rs`; private key `%USERPROFILE%\.tsubame\update-signing.key` (user backed it up); repo secret `UPDATE_SIGNING_KEY` set. `ci.yml` and `release.yml` have never run on GitHub; no real release, no Authenticode signing yet.
- Disk: main `target` 3.4 GB. No git worktrees left. About 14 empty (0 MB) folders under `C:\Users\MIcha\.t3\worktrees\tsubame\` remain because T3 holds them; delete by hand. Rig: `G:\rig-build\tsubame-base` leftover, raw bench reports in `G:\rig-test\tsubame\bench-2026-11-18T03-19-19`, Inno Setup 6 installed per-user. Rig is otherwise clean.
- `X:\Dev\ESL` untouched.

## What exists
Working file-manager skeleton (tabs, breadcrumbs/path edit, back/forward/up, filter, sort, selection, sidebar of drives/known folders, density presets, dark/light, status bar, loading line, native shell menu) on our own immediate-mode kit over Direct2D. Crates: `tsubame-core` (entry store), `tsubame-win` (enumerator, shell workers), `tsubame-ui` (kit), `tsubame-d2d`, `tsubame-plughost` (wasm host), `tsubame-update`, `tsubame-bench`. Docs: `docs/{spike-d2d,spike-enum,spike-wasm,shell-threading,core-store,enumerator,ui-kit,u2-views,updater,release,bench}.md`, research in `docs/research-*.md`, bench history in `docs/bench-log.md`.

## Numbers vs budgets (rig, release, 4K, B1 run 2026-11-18 03:19; MC servers + docker running, CPU 18%)
| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first paint | < 100 ms | 78.8 med / 100.9 p95 (n=10) | pass, p95 on the line |
| 100k first rows | < 30 ms | 3.79 med / 21.2 p95 | pass |
| 100k complete + sorted | < 200 ms | 69.3 med / 176.3 p95 | pass |
| 100k first rows, cold cache | < 30 ms | 66.1 | **fail** (includes app cold start: first paint 505 ms) |
| 100k sorted, cold cache | < 200 ms | 317.0 | **fail** (same caveat) |
| Scroll paint p95, 4K | < 5 ms | 3.17 median of runs (p95 3.76) | pass |
| Idle CPU, 10 s | 0 wake-ups | 0 ms (first window 16 ms) | pass; wake-up count not measured (no ETW) |
| Private bytes, 1 tab, small folder, 4K | < 110 MB | 105.8 | pass, thin |
| Private bytes after 5 s scroll | < 110 MB | 151.3 med / 154.0 max | **fail** |
| Main exe | < 6 MB | 624 KB | pass |
| Leak gate, 10,000 navs + 1,000 tabs | within 5% | +10.3% / +3.5% / +4.5% over 3 runs | **fail on worst run** |

Leak gate detail (worst run): private 111.5 -> 123.1 MB; handles -2, GDI 0, USER 0, threads -1. Bisect: 10k navs in one tiny folder -4.5%; 3,000 tabs on one folder -2.1%; 1,000 tabs over 204 folders +3.8%. Conclusion by B1: no per-tab and no per-navigation leak; growth is the bounded icon cache (2048 entries / 8 MB; trim frees 6.5 MB) plus D2D/driver heap noise of about +-5 MB, same size as the threshold. Nothing relaxed. Needs the memory card (below).

Earlier P0/P1 numbers worth keeping: C1 name sort 100k 9.6 ms / 1M 139 ms, 1M store 50.0 MB, filter 100k 2.6 ms; C2 warm 100k first batch 0.50 ms / done 33.6 ms; U2 startup navigation first rows 53.6 ms counted from tab creation (navigation after startup meets 30 ms); shell: icon warm 0.74 ms, thumbnail 23 ms generated / 3 ms cached, first context menu 0.5-1.3 s cold; wasm host 0.86 MB without Cranelift, spawn 6 ms precompiled.

## Open problems (in priority order)
1. **Memory** (private bytes after scroll 151 MB, 181 MB after repeated 100k navs; budget 110). Contributors: GPU render target and driver heaps, DirectWrite heap during scroll, warm-up WARP target, icon cache. Ideas: glyph-run text instead of layouts, smaller layout cache, release the software target after the GPU swap, cap icon cache lower. New card (Opus or Sonnet). The leak-gate fail is the same problem; after the fix rerun `tools/bench-rig.ps1` and compare with the first `docs/bench-log.md` entry.
2. **Cold-cache rows** need an app-side mid-run purge so the number excludes app cold start (B1 skipped it). The two cold fails are unconfirmed; S2/C2 measured 13 ms first rows / 187 ms sorted cold in isolation (pass).
3. Filter highlight rectangles missing (U2).
4. Cold start p95 100.9 ms, on the line.
5. Wake-up count via ETW not measured.
6. Updater not wired into the app (Settings > Updates, startup check; see `docs/updater.md`). Workflows never run on GitHub.

## Decisions the user already made (do not re-ask)
- Memory budget relaxed to < 90 MB windowed / < 110 MB at 4K (PLAN section 2); keep the leak gate.
- Enumeration: NT query `FileIdExtdDirectoryInformation`, 64 KB buffer, Find fallback. Wasm host without Cranelift, `.cwasm` only, `list<string>` API, batches <= ~2,000.
- Accent blue; ship all basic themes (copy sets from hopper and spotifast); density switchable on the fly; zebra rows with toggle; no shadows for now; loading line OK.
- Own-drawn, user-editable context menu with Nilesoft Shell config import. Entries come from the shell `HMENU` filled by `IContextMenu::QueryContextMenu` (legacy items; Win11 `IExplorerCommand` items may be missing; needs a spike); we draw it and invoke by id.
- Caret blinks only while a text field is focused, stops after about 10 s and stays solid (idle stays 0 wake-ups). Not yet verified in the app.
- Default file manager: opt-in only, `HKCU\Software\Classes\Directory\shell` verb to a signed fail-safe shim `tsubame-handler.exe` that falls back to `explorer.exe` and removes its own key on failure. Never IFEO on explorer.exe; skip `Folder\shell` in v1; Win+E separate experimental opt-in; no MSIX; sign everything. Top VM test: does Defender tolerate `Directory\shell` writes from a signed app. See `docs/research-default-explorer-defender.md`.
- Public repo allowed; git author e-mail stays as is (no history rewrite); updates come from GitHub Releases.
- On a model usage limit: relaunch the same brief on another provider's similar-tier model, `task_cancel` the limited task, log it here (saved in master memory). Substitutions so far: S3 GPT -> Sonnet, U2 Sonnet -> Codex gpt-6.1-sol.

## Rules learned (for the next master)
- Always pass `-Exe tsubame` to `push-to-rig.ps1`; GUI only on rig, never on FOX. Compiling cargo only via `X:\Dev\tools\cargo-ccd1.ps1`.
- Put "WORK ONLY IN YOUR WORKTREE" in every brief (C2 once edited main).
- Merging: root `Cargo.toml` and `Cargo.lock` conflict between parallel children; keep both `Cargo.toml` sides, `git checkout --ours Cargo.lock`, rerun `cargo check`.
- `git worktree remove` fails with Permission denied (T3 holds the folder); then `git worktree prune` and `git branch -d` is enough.
- Scroll offsets `f64`; layout cache 1024 entries / 4 frames for a scrolling list.
- Shell: handle F10 in the app; hand foreground back after a worker menu closes; menu windows owned by the worker thread.
- `gh` calls to api.github.com sometimes time out from FOX while git push works; retry a few times.
- No Python in the shell; use PowerShell `.Replace()` or the Edit tool.
- Rig `G:` is shared with Minecraft; push-to-rig caps and evicts.

## Next batch (P2), after the user says go
From PLAN section 7 P2: C3 file-op queue (GPT), C4 directory watcher (GPT), W1 shell menu/drag-drop/clipboard (Opus), W2 icon/thumbnail pipeline + grid view (Sonnet), U3 sidebar/dual pane/inline rename (Sonnet, mock first), U4 command palette/keys/settings (Sonnet, mock first), D1 config/session restore/CLI args (DeepSeek). Add these queued cards: memory (above, first or in parallel with C3/C4), updater wiring, T1 themes, editable context menu + Nilesoft import (needs spike), default-explorer opt-in, filter highlight fix, cold-cache bench hook. Two tasks that edit the same crate must not run together (W1 and W2 both touch `tsubame-win`/shell). Check GPT quota before launching C3/C4; substitute per the limit rule.
