# Benchmarks and the leak gate on rig

`tools/bench-rig.ps1` builds the app on rig, generates the fixtures, runs the
app benchmarks and the full leak gate in real windows, and appends one section
to `docs/bench-log.md`. Every number in that log comes from the app's own
reports (`src/bench.rs`, `src/timing.rs`, `src/leak.rs`), never from a proxy.

```
pwsh X:\Dev\tools\bench-rig.ps1 -Project <worktree>     # full run, appends to the log
pwsh X:\Dev\tools\bench-rig.ps1 -Quick                  # 2 min smoke of the harness, no log entry
pwsh X:\Dev\tools\bench-rig.ps1 -LeakGateOnly -NoLog -GateRuns 1   # just the gate
```

The GUI only ever runs on rig (see the `rig` skill): the script ships with
`X:\Dev\tools\push-to-rig.ps1 -BuildOnRig`, launches through a scheduled task
in rig's interactive session, reads the reports back over `\\Rig\g`, and
finally hands them to `tsubame-bench app`, which owns the parsing, the budget
thresholds and the markdown. That split is deliberate: the rig plumbing must be
PowerShell (push-to-rig, ssh, `schtasks`), while the part that can be wrong in
interesting ways (parsing, pass/fail, the log format) is Rust with unit tests
in `crates/tsubame-bench/src/appbench.rs`.

## What a run does

| Step | App mode | What it measures |
|---|---|---|
| 10 launches | `--bench --dir <100k>` | cold start to first painted frame, 5 s scroll paint p50/p95, private bytes before/after the scroll, exe size |
| 1 launch | `--timing --timing-dirs <100k> --reps 10` | open a 100,000-entry folder: first rows and complete+sorted, per navigation |
| 1 launch | plain, `--dir <100k>` | idle CPU over 10 s, twice (the second window is reported), thread count |
| 1 purge + 1 launch | `gsudo -n tsubame-bench purge --mode working` then `--timing --timing-dirs "<tiny>;<100k>" --reps 1` | cold-cache open of the 100k folder (only if the purge is permitted) |
| 3 launches | `--leak-gate --gate-fixture <gate> --gate-navs 10000 --gate-tabs 1000` | the leak gate: 10,000 navigations + 1,000 tab open/close in one process, counters every 1,000 ops |

Fixtures come from `tsubame-bench gen --root G:\rig-bench\tsubame-b1 --sizes 100k`
(100,000 empty files, mixed names, seed 20261117) and
`tsubame-bench gate-fixture --root G:\rig-bench\tsubame-b1`, which creates
`gate\tiny\0001..0190` (20 entries each), `gate\small\0001..0009` (1,000 each)
and `gate\big\0001` (10,000) — 204 folders in total, plus `setup.exe`,
`tool.exe`, `shortcut.lnk` and `link.lnk` in every folder so the per-path icon
cache is exercised. They stay under 10 GB and are deleted at the end unless
`-KeepFixtures`. The raw reports of a run stay in
`G:\rig-test\<slot>\bench-<stamp>\` (a few hundred KB) and the log names that
directory.

Options: `-Runs` (cold-start launches, default 10), `-GateNavs`, `-GateTabs`,
`-GateRuns` (default 3), `-ColdMode none|standby|working`, `-Slot N`,
`-SkipBuild`, `-SkipGen`, `-SkipLeakGate`, `-LeakGateOnly`, `-KeepFixtures`,
`-Quick`, `-NoLog`, `-LogPath`, `-GateFixture` (override the tree, for bisecting).

The script never leaves a process behind: it stops the app and deletes its
scheduled task even when a stage throws, and it takes the slot through
push-to-rig's own bookkeeping, so `-Stop -Slot N` from a shell does the same.

## The leak gate

`tsubame --leak-gate --gate-fixture DIR [--gate-navs 10000] [--gate-tabs 1000]
[--gate-sample 1000] [--gate-settle 5]` runs in the real window (maximized, 4K
on rig) with the real enumerator, the real shell icon workers and real tabs:

1. Discover every folder under the fixture (depth 3).
2. Show the fixture's first folder in one tab, wait for the GPU target and
   settle: that sample is the memory budget's "one tab, small folder, 4K"
   scenario.
3. Warm up: two passes over the fixture as navigations (the op pattern sends
   every 10th op to back/forward/up, so ~60% of a pass changes folder) and
   `tabs/5` tab open/close cycles. This fills the bounded caches — icon LRU
   (2048 items / 8 MB) and the DirectWrite layout cache (1024 layouts, 4
   frames) — which otherwise fill *during* the measured round and look like
   growth.
4. Measure: 10,000 navigations (cycling the fixture, every 10th op is
   back/forward/up) followed by 1,000 tab open/close cycles (each new tab opens
   a folder, scrolls to its middle, waits for the icons, then closes). Every
   op paints, so every visible row's icon is asked for like a real user's.
5. Sample `GetProcessMemoryInfo` `PrivateUsage`, working set,
   `GetProcessHandleCount`, `GetGuiResources` (GDI and USER) and the process's
   thread count before the round, every `--gate-sample` operations, after a
   `--gate-settle` second idle settle, and once more after clearing the icon
   cache (the app's one explicit cache; there is no other trim step).
6. Write `leak-gate.txt` next to the exe (env `TSUBAME_LEAK_OUT` renames it)
   as `key=value` lines, and exit 0 (pass) or 1 (fail).

Thresholds (also written into the report): private bytes after the settle may
grow by at most 5% of the start sample, ignoring growth below 2 MB; handles
+32, GDI +16, USER +16, threads +8. The baseline is the post-warm-up start, so
a bounded cache filling up is not scored as a leak; the report also carries the
idle sample, the peak, the per-1,000-op timeline, and the delta from the idle
sample, so a reviewer can see the cold-to-warm picture too.

The harness runs the gate three times (fresh processes) and the *worst* run
decides: one run may not exceed the allowance. The spread is printed because it
is as large as the threshold (see below).

## Budgets judged

| Metric | Threshold | Where it comes from |
|---|---|---|
| Cold start to first painted frame | < 100 ms median of 10 | `--bench` |
| 100k folder: first rows | < 30 ms median of 9 | `--timing` |
| 100k folder: complete and sorted | < 200 ms median of 9 | `--timing` |
| 100k folder, cold cache | < 30 / < 200 ms, or "not measured" | `--timing` after a purge |
| Scroll paint p95, 4K | < 5 ms median of 10 runs' p95 | `--bench` |
| Idle CPU, 10 s | < 50 ms of CPU (the budget is 0 wake-ups; process CPU time is quantised to ~15.6 ms on rig, so one tick reads as 15.6 ms) | plain launch |
| Private bytes, one tab, small folder, 4K | < 110 MB | gate's idle sample |
| Private bytes after the 5 s scroll run | < 110 MB | `--bench` |
| Main exe | < 6 MB | `--bench` |
| Leak gate | private bytes within 5% (2 MB floor), handles/GDI/USER/threads within the constants above | `--leak-gate` |

## Known state, 2026-11-18 (task B1)

Measured on `7eac782` (U2 merged), first real entry: `docs/bench-log.md`.

- Speed is inside budget: cold start 78.8 ms median (100.9 p95), 100k first
  rows 3.8 ms, complete+sorted 69.3 ms, scroll paint p95 3.2 ms, idle CPU 0,
  exe 624 KB.
- **Memory is not.** Private bytes after the 5 s scroll run are 151 MB against
  110; the one-tab small-folder sample is 105.8 MB (inside, but only just).
- **The leak gate does not pass yet, and it is noisy at its own threshold.**
  Three runs of the same binary: +10.34%, +3.47%, +4.48% (allowance ~5.6 MB),
  so the worst run fails. Bisect runs from B1: 1,000 tabs alone +3.8% (pass);
  3,000 tabs in a *single* folder -2.1% (no tab-lifecycle leak); 10,000
  navigations in a single folder -4.5% (no per-navigation leak); 3,000 tabs
  over the 200-folder tree +4.8% (icon cache filling); clearing the icon cache
  at the end frees 4–6 MB. Growth is therefore dominated by the bounded icon
  and layout caches and by D2D/driver heap state (±5 MB run to run), not by a
  counter that grows per operation. Next steps for whoever owns memory: trim
  the per-path icon cache harder (or cap it by bytes at the 32 px size),
  and decide whether the gate should compare a median of runs instead of the
  worst one.
- Cold-cache open of the 100k folder fails both budgets (66 ms first rows,
  317 ms sorted). The purge empties the app's own pages too, so that run
  includes the app's cold start (first paint 505 ms vs 74 ms warm); a cold
  measurement that isolates the folder would need the app to purge mid-run.
- Idle wake-ups were not counted with ETW; `Get-Counter \Thread(*)` has no
  instances for this process on rig, so only CPU time is reported.
