# Tsubame bench log

One section per benchmark run on the benchmark machine, newest last (the Correction
at the end is a footnote, not a run). Budgets and the
leak gate are `docs/PLAN.md` section 2; the harness itself is described in
`docs/bench.md`. Every number here comes from the release app on the benchmark machine: real
window, real enumerator, real shell icon workers. Warm filesystem cache unless
the section says otherwise.

The harness builds the release app on the benchmark machine, generates the
fixtures under `<fixtures>\tsubame-b1`, runs the benches and appends a section
here (see `docs/bench.md` for the procedure).

Trimmed to the latest result per metric on 2026-11-24. The latest full harness
run is `cb13120` below and holds every budget row and the leak gate; passes after
it carry the newer per-metric numbers (idle wake-ups, 1M scroll, exe size, packs).
Older runs (2026-11-18 to 2026-11-19, commits `7eac782`, `02a2731`, `159b639`,
`c908f4a`, `18b1bc8`, `4897312`, `5b1cafb`), with their raw reports, are in git
history: `git show 9498a14:docs/bench-log.md`. The memory pass (MEM1/MEM1b)
numbers are in `docs/memory.md`; the context-menu measurements in
`docs/ctxmenu-validation.md`.

## INT1 - 2026-11-18, `4897312`

Numbers for the integration pass, cited by `docs/integration.md`; the full runs
below supersede them.

Scroll paint p95, 4K details view, 6 runs each on the benchmark machine, same fixtures:

| Build | p95 median | Budget |
|---|---|---|
| `63c27b7` (baseline) | 4.41 ms | < 5 ms |
| `7e6c65b` (U3 + W1 merge, before INT1) | 4.70 ms | < 5 ms |
| `4897312` (INT1 final) | 4.63 ms | < 5 ms |

INT1 is not slower than the merge it starts from; the +0.2 ms against
`63c27b7` came in with the merge. Idle: 0 ms CPU in the first window, 47 ms in
the second (the table's figure). Private bytes 66.2 MB idle (budget 90 MB
windowed), 84.2 MB after the 4K scroll run (budget 110 MB). Leak gate PASS
(+3.7% worst run). Exe 1664 KB. The two cold-cache rows (n=1, disk-bound
after a working-set purge) miss their budgets; INT1 did not touch listing.

## U4 - 2026-11-18, `da7b850`, full run

Numbers for the palette and Settings pass, cited by `docs/u4-settings.md`. The
benchmark machine was loaded (8 Java server processes, 25 containers, CPU 47% busy), so the
latency rows are noisy.

| Metric | Budget | Measured | |
|---|---|---|---|
| Idle CPU, second 10 s window | 0 wake-ups | 47 ms (first window 16 ms) | as INT1 (31-47 ms) |
| Private bytes, one tab, small folder, 4K | < 110 MB | 64.5 MB | PASS |
| Private bytes after the 5 s scroll run (100k) | < 110 MB | 83.8 MB median (89.5 max) | PASS |
| Main exe | < 6 MB | 1714 KB (stale binary; a real U4 build is 2.60 MB, see Correction) | PASS |
| Leak gate (10,000 navs + 1,000 tabs, palette + settings churn in every tab cycle) | within 5% | worst run +4.12% (runs: +2.85, +3.54, +4.12) | PASS |
| Scroll paint p95, 4K | < 5.0 ms | 6.67 ms median of 10 (8.82 max) | noisy machine, see below |
| Cold start to first frame | < 100 ms | 115 ms median | noisy machine, see below |

Scroll p95 under load: the same run of the INT1 baseline `e230e30` gave a 4.73 ms median when the machine was
quieter (21:34) and a 7.5 ms median when run again from the same binary at 21:55 (5 Java server processes, 22% busy).
U4 builds gave 6.0 to 6.7 ms in between (`815a440`, `e1de2ee`, `da7b850`). Nothing U4 added runs per
row or per frame in the list (palette and Settings are `None` while closed), so the spread is machine load, not a
measured regression; rerun on a quiet machine before relying on the figure.

## PERF2 - 2026-11-18, open-a-folder probe, base `63c27b7` vs main `29a1d3b` vs perf2 (`4c37edb`)

The 1M-scale numbers; later full runs report 100k only. Method: the open-a-folder probe (see `docs/bench.md`),
6 runs per point (warm: repeats), release. Cold = working purge from an elevated session, disk settle, launch
`tiny;<idle>;1k;tiny;<dir>`. Results (median / p95 ms):

| case | build | first rows | complete+sorted | worker | overhead |
|---|---|---|---|---|---|
| cold 100k | base | 22.9 / 40.7 | 180 / 306 | 148 | 32 |
| cold 100k | main | 13.9 / 30.4 | 182 / 211 | 137 | 44 |
| cold 100k | perf2 | 14.2 / 23.5 | 163 / 179 | 148 | 14 |
| cold 1M | base | 23.8 / 82 | 3147 / 3255 | 1452 | 1691 |
| cold 1M | main | 22.8 / 146 | 3490 / 3682 | 1490 | 2014 |
| cold 1M | perf2 | 44.6 / 970 | 1624 / 3405 | 1565 | 57 |
| warm 100k | base | 5.1 / 5.8 | 70.8 / 74.7 | - | 36 |
| warm 100k | main | 5.9 / 6.8 | 85.9 / 94.5 | - | 51 |
| warm 100k | perf2 | 6.7 / 11.0 | 43.2 / 49.3 | - | 10 |
| warm 1M | base | 6.6 | 2298 / 3179 | - | - |
| warm 1M | main | 6.9 | 2619 / 2681 | - | - |
| warm 1M | perf2 | 7.0 / 7.7 | 430 / 514 | - | 51 |

perf2 cold 100k: store_cpu 9.1, final_sort 6.5. 1M p95 outliers are disk.

Ops (`<ops>`, perf2, UI work only, median ms; filter p95 max in brackets):

| op | 100k | 1M |
|---|---|---|
| sort size / modified / type | 3.5 / 2.5 / 5.6 | 28 / 30 / 84 |
| sort name (from type order) | 16.8 | 166 |
| sort name desc / asc | 6.7 / 5.3 | 69 / 66 |
| filter type +1..+5 | 3.7 / 1.9 / 0.2 / 0.1 / 0.1 | 37 / 19 / 1.8 / 0.5 / 0.5 |
| filter erase -4..-0 | 3.9 / 4.2 / 4.3 / 3.6 / 0.4 [4.9] | 39 / 42 / 43 / 36 / 4.9 |

Open at 1M: name sort 166 ms and a filter keystroke ~40 ms (> 50 / 16 ms).

## Latest full run - 2026-11-19 00:29, bench machine (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `cb13120`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on the bench machine, branch `t3/fix-details-scroll-paint-p95`, commit `cb13120`
- Window: 3840x2076 at 168 dpi, paint path `hardware`
- Fixtures: `<fixtures>\tsubame-b1\100k` (100,000 entries), `<fixtures>\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); machine load: 5 Java server process(es), 25 docker container(s), CPU 12% busy, servers loaded; machine up 1 h
- Cold-cache 100k open: measured (3 runs: purge working from outside, app warmed up on small folders, only the cold 100k navigation timed)
  3 runs, one process each: purge, then the app warms up on small folders (and idles 3 s) before it opens the still cold 100k folder; the row times only that last navigation.
- Idle: 0 ms of CPU over the first idle window, 31 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 28.1 MB private at the first painted frame, 99.8 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 66.60 ms median, 72.80 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 5.52 ms median, 29.32 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 53.62 ms median, 93.74 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 13.73 ms median, 14.73 ms p95 (n=3) | PASS |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 168.98 ms median, 221.55 ms p95 (n=3) | PASS |
| Scroll paint p95, 4K, details view | < 5.0 ms | 2.65 ms median, 2.81 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 31 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 63.3 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 82.4 MB median (83.1 MB max) | PASS |
| Main exe | < 6 MB | 2681 KB (later builds: X2 3,501,568 B, still PASS) | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 80.6 -> 82.9 MB (+2.27 MB, 2.8%; allowance 4.0 MB) | PASS |

All measured budgets and the leak gate pass.

The cold "complete and sorted" p95 (221.55 ms) is over the 200 ms budget while the median (168.98 ms) is inside it.

Informational: startup navigation first rows 49.40 ms and complete+sorted 81.74 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 83.1 -> 84.7 | +1.55 | +1.87 | 4.16 | 37 | PASS |
| 2 | 80.6 -> 82.9 | +2.27 | +2.82 | 4.03 | 41 | PASS |
| 3 | 82.5 -> 84.2 | +1.73 | +2.10 | 4.12 | 37 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **PASS**
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (63.3 MB private, before the warm-up) to the end the process grew 19.6 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 80.6 | 75.0 | 626 | 45 | 21 | 59 |
| 0 ops | 80.6 | 75.0 | 626 | 45 | 21 | 59 |
| 1000 ops | 84.8 | 78.1 | 626 | 45 | 21 | 59 |
| 2000 ops | 81.8 | 76.1 | 626 | 45 | 21 | 59 |
| 3000 ops | 85.3 | 78.6 | 626 | 45 | 21 | 59 |
| 4000 ops | 85.2 | 78.7 | 622 | 45 | 21 | 59 |
| 5000 ops | 85.7 | 78.9 | 625 | 45 | 21 | 58 |
| 6000 ops | 83.6 | 77.1 | 625 | 45 | 21 | 58 |
| 7000 ops | 86.4 | 79.6 | 625 | 45 | 21 | 58 |
| 8000 ops | 85.5 | 79.5 | 623 | 45 | 21 | 58 |
| 9000 ops | 83.5 | 76.9 | 625 | 45 | 21 | 58 |
| 10000 ops | 83.9 | 77.1 | 625 | 45 | 21 | 58 |
| 11000 ops | 82.9 | 77.1 | 623 | 45 | 21 | 56 |
| after settle | 82.9 | 77.1 | 623 | 45 | 21 | 56 |
| after icon-cache trim | 82.9 | | | | | |

Private bytes +2.27 MB (+2.82%, allowance 4.03 MB), handles -3, GDI +0, USER +0, threads -3. Peak private bytes 86.4 MB.
Forced collection: icon cache cleared (1121 entries, cap 2048 items / 6 MB); private bytes after it 82.9 MB.
Gate wall time 41 s.

## Idle wake-ups - 2026-11-19, benchmark machine, branch `t3/idle-wakeup-counter-fix` on `12a04d3`

Release builds via push-to-rig. Old: `12a04d3` (`tsubame.exe 2,887,680 B`). New: this branch
(`tsubame.exe 2,900,480 B`). Launch as the harness does (`--dir <fixture>\100k`, maximized 4K),
no input. Wake-ups = context switches of every thread of the process between two snapshots
(`tools/idle-wakes.ps1`, NtQuerySystemInformation; nothing runs inside the process). CPU = process CPU time
(15.6 ms ticks).

What woke the idle process (old build, 3 windows of 10 s after a 5 s settle): the NVIDIA driver thread
(start `nvwgf2umx.dll+0xece040`) 970, 969, 970; the Tsubame main thread 16, 8, 0 (startup tail); other 2-5;
total 988, 982, 972. The driver thread exists for as long as any hardware D3D11 device lives in the process. Not
changed by: dropping `PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`, `SINGLETHREADED`, `FLIP_SEQUENTIAL`, `Present(1)`,
`SetMaximumFrameLatency(1)`, `IDXGIDevice3::Trim` after each frame, or an HWND render target instead of the flip
chain (each ~485 wake-ups per 5 s). `TSUBAME_GPURENDER=0` (WARP only): 0 wake-ups after settling. Tsubame's own
threads (shell pool, watcher, file-op queue, updater) never woke.

Fix: after 10 s without a paint, the GPU target, device and D2D factory are dropped (`Gfx::park`, one-shot
`PARK_TIMER`; off in bench/stress/leak-gate runs; `TSUBAME_GPU_PARK=<s>`, `0` = never). The driver unloads and
its thread ends. Nothing is repainted: DWM keeps the last presented frame on screen (checked by screenshot). Input
(key, button, wheel, real mouse move, resize) or a paint the system asks for starts the GPU device on its worker;
frames until then use the software stand-in, as at open. A WARP repaint at park time was tried first and dropped:
~330 ms of CPU at 4K.

Interleaved A/B on the new exe (`TSUBAME_GPU_PARK=0` = old behaviour), wake-ups / CPU ms per 10 s window:

| Run | 3-13 s | 13-23 s | 23-33 s | 33-43 s | Private |
|---|---|---|---|---|---|
| new, round 1 | 39 / 0 | 1 / 0 | 1 / 0 | 0 / 0 | 17.5 MB |
| park off, round 1 | 972 / 31.2 | 980 / 31.2 | 971 / 31.2 | 972 / 31.2 | 66.0 MB |
| new, round 2 | 32 / 31.2 | 1 / 0 | 1 / 0 | 2 / 0 | 17.6 MB |
| park off, round 2 | 966 / 0 | 970 / 15.6 | 971 / 15.6 | 972 / 15.6 | 65.9 MB |
| new, round 3 | 59 / 31.2 | 1 / 0 | 2 / 0 | 0 / 0 | 17.7 MB |
| park off, round 3 | 1192 / 0 | 980 / 15.6 | 1367 / 62.5 | 997 / 15.6 | 67.4 MB |

The 13-23 s window is the one the harness reports. The remaining 1-2 wake-ups are `ntdll` thread-pool workers
(left from the startup WARP stand-in) timing out and exiting, plus one `crypt32` thread; a longer run read 1, 1,
16, 0, 0, 0 (windows from 13 s on; one window lost to an unpark from desktop activity on the shared machine).
Private bytes while parked drop from ~66 MB to ~18 MB (the leak gate and the memory rows run with parking off,
so their numbers are unchanged). The X1 comparison below re-measures this on a later build: 1-2 wake-ups, 0 ms.

Cost, moved to the first input after an idle stretch (`TSUBAME_WAKE_LOG`, a posted key after the park):

| | First frame after the key | GPU frame after the key | CPU for the wake |
|---|---|---|---|
| software stand-in (shipped) | ~13 ms (12.3 ms frame) | ~180 ms | 172, 188 ms |
| skip frames until the GPU is ready (tried) | 162 ms | 162 ms | 156, 172 ms |

Pinning `nvwgf2umx.dll` loaded did not make the device come back faster (220 ms).

Diagnostics: `tools/idle-wakes.ps1` (per-thread wake-ups, CPU, cycles, thread names, start module; run it in
the app's session) and `TSUBAME_WAKE_LOG=<file>` (every message the main window gets and every paint, with times,
written when the window closes; one relaxed load per message when unset).

## CM11 (2026-11-19): 200-menu growth owner

Private-bytes growth menu 10 -> 200 (+5.5..+8.4 %, no plateau) is one leaked
thread per menu in `WorkfoldersShell.dll` (Microsoft Work Folders context-menu
handler): 200 threads at menu 200, alive 240 s later. Handler disabled for the
user: +0.7 / +1.0 / +0.7 % and a flat thread count; 1000 menus: +36.4 %
(+2.7 MB per 100 menus) with it, +1.2 % without. Same growth on the native
`--stress menu` path and with packaged off or eager on. No change in our code;
details and the helper-process estimate in `docs/ctxmenu-validation.md`.

## X1 (2026-11-20): extension registry vs main, bench machine (Ryzen 9 5900X, RTX 3080, 4K at 200%)

Release builds via push-to-rig. Main `a03d0e1` (`tsubame.exe 3,322,880 B`; differs from
the X1 base `18dc64c` only in docs). X1 `3521a0c` (`tsubame.exe 3,375,616 B`). Runs
interleaved (order alternating per round), `TSUBAME_GPU_PARK=0`, a scratch `TSUBAME_DATA_DIR`, warm cache.

| Metric | Budget | main | X1 | |
|---|---|---|---|---|
| Cold start to first painted frame, `--bench --dir <14 entries>` | < 100 ms | 53.6 ms median, min 51.5 (n=16) | 55.9 ms median, min 51.1 (n=16) | PASS |
| Private bytes at first paint (same runs) | | 12.87 MB median (n=8) | 12.94 MB median (n=8) | |
| Private bytes after the GPU swap, idle (same runs) | < 110 MB | 77.0 MB median (n=8) | 76.3 MB median (n=8) | PASS |
| Scroll paint, 1M synthetic rows, `--bench` | < 5.0 ms p95 | p50 1.79-1.89, p95 3.21-3.38 ms (n=6) | p50 1.77-1.87, p95 3.28-3.55 ms (n=6) | PASS |
| Idle (13-23 s and 23-33 s windows, parking on) | 0 wake-ups, 0 ms | 1, 2 wakes / 0 ms (both rounds) | 1, 2 wakes / 0 ms (both rounds) | PASS |
| Private bytes idle and parked | | 14.5, 14.5 MB | 14.4, 14.7 MB | |
| Main exe | < 6 MB | 3,322,880 B | 3,375,616 B (+52,736 B, +1.6 %) | PASS |

The idle wake-ups are the same OS threads in both builds (one `crypt32` thread, then two `ntdll` pool workers
exiting); no Tsubame thread woke. The registry work at start-up, timed in an optimized test build: registry
75 µs, default key map 74 µs, column set 3 µs, about 0.15 ms in all. The 2.3 ms gap in the median first paint is
larger than that and the fastest runs are equal, so it is run-to-run spread on the shared machine, not the
registry. Frames read the column set and command list from per-generation copies (no registry access, no
allocation; the header lost three per-frame `Vec`s).

## X2: extension packs (tier-1 loader)

Release build, `TSUBAME_GPU_PARK=0`, `--bench --dir <200 empty files>`, n=10 per row, own data dir each.

| Case | First painted frame (median, min, max) | Exe |
|---|---|---|
| No `packs` folder | 56.8 ms, 51.6 - 62.1 | 3,501,568 B (main: 3.38 MB) |
| Both example packs (`open-in-terminal`, `solarized-lite`) | 53.2 ms, 51.6 - 77.2 | same |

Pack parse and register cost is inside the run-to-run spread (the medians differ by less than the spread), well under
the 100 ms budget. Idle wake-ups: nothing is scheduled by packs (no watcher, no timer); the folder is read at
start-up and on Reload packs only. Another benchmark loop was running during part of these runs, so the
numbers carry some extra noise. Screenshots: `docs/shots/x2-palette.png`, `x2-keybind.png`, `x2-menu.png`,
`x2-theme.png`.

## Correction - stale-exe sizes in the U4 and earlier rows (2026-11-19)

Two `Main exe` figures in earlier versions of this log measured the wrong binary (the U4
row above is one of them). The harness mirrored the source
with `robocopy`, which preserves each file's LastWriteTime; cargo's fingerprints are mtime-based, so
in a warm build directory a synced commit whose files are older than the build directory's artifacts made cargo skip
both the rebuild and the relink, and the previous exe was copied into the run dir. Evidence from the
investigation: five consecutive runs shipped the same 2,660,352 B exe, and the exe behind the
`1742 KB` row was the PERF2-line `11fe5f3` binary (1,783,808 B), not a `5b1cafb` build.

The harness now sets LastWriteTime = now on exactly the files a run copied (unchanged files
keep theirs, so warm incremental builds stay warm), puts those mtimes back after a successful build
(so the next sync copies nothing), and refuses to ship an exe that the build did not write; it
prints the shipped exe's size, SHA-256 prefix and commit. New sections carry
`- Exe: <bytes> B (<KB>), SHA-256 ...`, and `tsubame-bench app` prints a WARNING when the exe size
moves more than 10% against the previous row (the 6 MB budget still decides PASS/FAIL).

Real release sizes (rebuilding identical source gives the same size, but the PE carries a link
timestamp, so two builds of the same commit do not have the same sha256 - which is why the
staleness guard keys on mtime, not on the hash):

| Build | Main exe |
|---|---|
| pre-U4 (`11fe5f3`, the PERF2 line) | 1,783,808 B (1742 KB) |
| `d6990cd` U4: updater wired into `main.rs` | +459 KB over pre-U4 |
| `231d929` Merge U4 | 2.60 MB (2662 KB) |
| `701c3ab` main | 2,750,976 B (2687 KB) |
| `b25e4cf` main + this fix | 2,779,648 B (2715 KB) |

So the U4 row's "the updater crate and U4 add 50 KB" has the sign and the scale wrong: making the
`tsubame-update` stack reachable from the app (WinHTTP, WinTrust, bcrypt, ed25519, `serde_json`,
miniz_oxide) costs about **+890 KB**, and that is the whole jump from ~1.75 MB to 2.60 MB. The U4
section's 1714 KB is in the same class as the 1742 KB row (a pre-U4 binary), as is the figure in
`docs/u4-settings.md`. The 6 MB budget is met with ~3.2 MB to spare, so the PASS verdicts above
stand; only the sizes were wrong. Later builds (X1, X2) read 3.3-3.5 MB for the same reason:
new features, same budget.

## B11-F2 Group by scroll - 2026-11-26, benchmark machine, branch `feat/group-by-sort-menu`

Release build via push-to-rig (`tsubame.exe` 5,578,752 B, 5.32 MB), 4K at 200% (3840x2076), real
enumerator, `--bench --dir <100k> --group <mode>` (list, flat rows with headers, same 5 s fling plan as the
plain bench). Fixture: 100,006 entries, 15 extensions, 4 sizes, 10 date spreads (`type` 16 groups, `date` 8).

| Metric | Budget | Group by Type | Group by Date |
|---|---|---|---|
| Scroll paint p50 / p95 / p99 (all phases) | p95 < 5 ms | 2.18 / **3.78** / 4.42 ms | 1.90 / **3.44** / 4.09 ms |
| Steady fling 2000 DIP/s p95 | | 2.24 ms | 2.56 ms |
| Fast fling 20000 DIP/s p95 | | 2.97 ms | 3.03 ms |
| Page per frame p95 | | 4.42 ms | 3.67 ms |
| First painted frame | < 100 ms | 78.3 ms | 73.1 ms |
| Private bytes after scroll | < 110 MB | 91.4 MB | 93.1 MB |
| Bitmaps made while scrolling (type) | 0 per frame | 0 | |

Idle after use (scroll 30 notches grouped by Type, then `tools/idle-wakes.ps1`, settle 14 s, 3 x 10 s):
process CPU 0 ms in all three windows; the app's own main thread woke 12 times in window 1 (the GPU park
at 10 s), 0 in window 2, 0 in window 3 (9 wake-ups there belong to OS and shell threads: `ntdll`, `shcore`,
`combase`; two of them ended). No timer is added by grouping.

Screenshots: `docs/shots/group-type.png`, `group-date.png`, `group-size-collapsed.png`, `group-menu.png`.
