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
