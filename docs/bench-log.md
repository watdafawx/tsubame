# Tsubame bench log

One section per `tools/bench-rig.ps1` run on rig, newest last. Budgets and the
leak gate are `docs/PLAN.md` section 2; the harness itself is described in
`docs/bench.md`. Every number here comes from the release app on rig: real
window, real enumerator, real shell icon workers. Warm filesystem cache unless
the section says otherwise.

Rerun (from a worktree; needs the rig skill's ssh):

```
pwsh X:\Dev\tools\bench-rig.ps1 -Project <worktree>
```

The script builds on rig with `push-to-rig.ps1`, generates the fixtures under
`G:\rig-bench\tsubame-b1`, runs the benches and appends a section here.

## 2026-11-18 03:19 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `7eac782`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/bench-leak-gate-harness`, commit `7eac782` (dirty worktree)
- Window: 3840x2064 at 192 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 5 Minecraft java process(es), 25 docker container(s), CPU 18% busy, servers loaded; rig up 341 h
- Cold-cache 100k open: measured (purge working ok)
  The purge also emptied the app's own pages: that process painted its first frame 505 ms after start (warm: 74 ms), so the cold row includes loading the app itself, not only the folder's metadata.
- Idle: 16 ms of CPU over the first idle window, 0 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 94.5 MB private at the first painted frame, 177.3 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 78.80 ms median, 100.90 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 3.79 ms median, 21.21 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 69.28 ms median, 176.34 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 66.07 ms median, 66.07 ms p95 (n=1) | **FAIL** |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 316.96 ms median, 316.96 ms p95 (n=1) | **FAIL** |
| Scroll paint p95, 4K, details view | < 5.0 ms | 3.17 ms median, 3.76 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 0 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 105.8 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 151.3 MB median (154.0 MB max) | **FAIL** |
| Main exe | < 6 MB | 624 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 111.5 -> 123.1 MB (+11.52 MB, 10.3%; allowance 5.6 MB) | **FAIL** |

**4 budget(s) missed: 100,000-entry folder, cold cache: first rows, 100,000-entry folder, cold cache: complete and sorted, Private bytes after the 5 s scroll run (100k folder), Leak gate: 10,000 navigations + 1,000 tabs.** Reported as measured; nothing was relaxed to make them pass.

Informational: startup navigation first rows 57.08 ms and complete+sorted 203.99 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 111.5 -> 123.1 | +11.53 | +10.34 | 5.58 | 39 | **FAIL** |
| 2 | 109.9 -> 113.8 | +3.82 | +3.47 | 5.50 | 38 | PASS |
| 3 | 115.2 -> 120.4 | +5.16 | +4.48 | 5.76 | 38 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **FAIL** (failed: private)
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (105.8 MB private, before the warm-up) to the end the process grew 17.3 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 111.5 | 73.1 | 591 | 45 | 21 | 50 |
| 0 ops | 111.5 | 73.1 | 591 | 45 | 21 | 50 |
| 1000 ops | 113.3 | 74.5 | 591 | 45 | 21 | 50 |
| 2000 ops | 114.6 | 74.4 | 591 | 45 | 21 | 50 |
| 3000 ops | 115.3 | 74.2 | 591 | 45 | 21 | 50 |
| 4000 ops | 117.2 | 76.9 | 591 | 45 | 21 | 50 |
| 5000 ops | 120.0 | 77.4 | 589 | 45 | 21 | 49 |
| 6000 ops | 116.4 | 75.4 | 589 | 45 | 21 | 49 |
| 7000 ops | 118.3 | 76.4 | 589 | 45 | 21 | 49 |
| 8000 ops | 121.0 | 78.6 | 589 | 45 | 21 | 49 |
| 9000 ops | 117.2 | 76.8 | 589 | 45 | 21 | 49 |
| 10000 ops | 119.9 | 78.7 | 589 | 45 | 21 | 49 |
| 11000 ops | 123.1 | 81.2 | 589 | 45 | 21 | 49 |
| after settle | 123.1 | 81.2 | 589 | 45 | 21 | 49 |
| after icon-cache trim | 116.6 | | | | | |

Private bytes +11.53 MB (+10.34%, allowance 5.58 MB), handles -2, GDI +0, USER +0, threads -1. Peak private bytes 123.1 MB.
Forced collection: icon cache cleared (1047 entries, cap 2048 items / 8 MB); private bytes after it 116.6 MB.
Gate wall time 39 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T03:19:19
commit=7eac782
branch=t3/bench-leak-gate-harness
dirty=1
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=10
cache=warm filesystem cache (no purge before these runs)
cold=measured (purge working ok)
dpi=192
rig_load=5 Minecraft java process(es), 25 docker container(s), CPU 18% busy, servers loaded; rig up 341 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=1
reports=G:\rig-test\tsubame\bench-2026-11-18T03-19-19
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 72.10 paint_p50_ms 2.22 paint_p95_ms 3.17 private_before_scroll_mb 177.6 private_end_mb 148.7 peak_commit_mb 180.8 rows 100000 exe_bytes 638976
run 2: first_paint_ms 81.20 paint_p50_ms 2.20 paint_p95_ms 3.17 private_before_scroll_mb 176.4 private_end_mb 151.1 peak_commit_mb 178.6 rows 100000 exe_bytes 638976
run 3: first_paint_ms 81.50 paint_p50_ms 2.21 paint_p95_ms 3.36 private_before_scroll_mb 177.4 private_end_mb 152.3 peak_commit_mb 180.4 rows 100000 exe_bytes 638976
run 4: first_paint_ms 100.90 paint_p50_ms 2.26 paint_p95_ms 3.76 private_before_scroll_mb 178.4 private_end_mb 152.8 peak_commit_mb 181.1 rows 100000 exe_bytes 638976
run 5: first_paint_ms 78.80 paint_p50_ms 2.20 paint_p95_ms 3.23 private_before_scroll_mb 177.7 private_end_mb 154.0 peak_commit_mb 181.4 rows 100000 exe_bytes 638976
run 6: first_paint_ms 80.60 paint_p50_ms 2.18 paint_p95_ms 3.06 private_before_scroll_mb 178.3 private_end_mb 152.2 peak_commit_mb 181.3 rows 100000 exe_bytes 638976
run 7: first_paint_ms 78.70 paint_p50_ms 2.21 paint_p95_ms 3.15 private_before_scroll_mb 176.0 private_end_mb 148.6 peak_commit_mb 179.8 rows 100000 exe_bytes 638976
run 8: first_paint_ms 78.30 paint_p50_ms 2.18 paint_p95_ms 3.10 private_before_scroll_mb 177.1 private_end_mb 151.3 peak_commit_mb 180.0 rows 100000 exe_bytes 638976
run 9: first_paint_ms 86.10 paint_p50_ms 2.17 paint_p95_ms 2.99 private_before_scroll_mb 179.4 private_end_mb 152.6 peak_commit_mb 181.7 rows 100000 exe_bytes 638976
run 10: first_paint_ms 71.30 paint_p50_ms 2.17 paint_p95_ms 3.19 private_before_scroll_mb 177.1 private_end_mb 147.5 peak_commit_mb 181.1 rows 100000 exe_bytes 638976
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 73.6
private_bytes_first_paint: 94.5 MB
all times in ms from the navigation event
startup   entries  100000 (9.0 MB)  first_batch    14.67  first_frame    57.08  done   194.51  final_frame   203.99  worker    41.51   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.0 MB)  first_batch     8.95  first_frame    21.21  done   173.96  final_frame   176.34  worker    44.87   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.0 MB)  first_batch     0.92  first_frame     5.02  done    85.29  final_frame    86.77  worker    44.80   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.0 MB)  first_batch     0.85  first_frame     4.12  done    70.81  final_frame    72.29  worker    37.66   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.0 MB)  first_batch     1.08  first_frame     3.79  done    67.68  final_frame    69.28  worker    34.95   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.0 MB)  first_batch     0.87  first_frame     2.29  done    67.20  final_frame    68.64  worker    36.25   G:\rig-bench\tsubame-b1\100k
repeat 6  entries  100000 (9.0 MB)  first_batch     2.40  first_frame     5.26  done    66.55  final_frame    67.89  worker    34.63   G:\rig-bench\tsubame-b1\100k
repeat 7  entries  100000 (9.0 MB)  first_batch     1.27  first_frame     2.77  done    65.62  final_frame    66.89  worker    35.35   G:\rig-bench\tsubame-b1\100k
repeat 8  entries  100000 (9.0 MB)  first_batch     0.91  first_frame     3.60  done    65.09  final_frame    66.34  worker    35.15   G:\rig-bench\tsubame-b1\100k
repeat 9  entries  100000 (9.0 MB)  first_batch     0.89  first_frame     2.51  done    68.36  final_frame    69.57  worker    37.43   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 177.3 MB
```
</details>

<details><summary>Raw: cold-cache u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path software (GPU starting)
first_paint_ms (process start to first completed paint): 504.6
private_bytes_first_paint: 86.4 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   192.88  first_frame   432.44  done   432.53  final_frame   442.22  worker     4.75   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (9.6 MB)  first_batch    56.27  first_frame    66.07  done   306.52  final_frame   316.96  worker   214.28   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 124.2 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=15.6
idle.cpu_ms=0
idle.threads=53
idle.threads_warm=53
idle.context_switches_per_s=-1
idle.pid=62864
```
</details>

<details><summary>Raw: leak-gate.txt</summary>

```
# --- leak-gate-01.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
start_paint_path=hardware
paints=24559
fixture=G:\rig-bench\tsubame-b1\gate
folders=204
largest_folder_entries=10004
navigations=10000
tabs=1000
ops=11000
warmup_navigations=408
warmup_tabs=200
sample_interval=1000
settle_s=5
elapsed_s=39.1
ops_per_s=281.3
navigations_s=35.4
tabs_s=3.7
navigations_per_s=282.5
tabs_per_s=269.3
stage_s.load=26.8
stage_s.paint=14.0
stage_s.icons=1.9
steps=25096
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=116953088
start.private_mb=111.54
start.working_set_bytes=76599296
start.handles=591
start.gdi=45
start.user=21
start.threads=50
sample.0.private_bytes=116953088
sample.0.private_mb=111.54
sample.0.working_set_bytes=76599296
sample.0.handles=591
sample.0.gdi=45
sample.0.user=21
sample.0.threads=50
sample.1000.private_bytes=118841344
sample.1000.private_mb=113.34
sample.1000.working_set_bytes=78094336
sample.1000.handles=591
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=50
sample.2000.private_bytes=120184832
sample.2000.private_mb=114.62
sample.2000.working_set_bytes=77967360
sample.2000.handles=591
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=50
sample.3000.private_bytes=120930304
sample.3000.private_mb=115.33
sample.3000.working_set_bytes=77840384
sample.3000.handles=591
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=50
sample.4000.private_bytes=122945536
sample.4000.private_mb=117.25
sample.4000.working_set_bytes=80650240
sample.4000.handles=591
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=50
sample.5000.private_bytes=125878272
sample.5000.private_mb=120.05
sample.5000.working_set_bytes=81137664
sample.5000.handles=589
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=49
sample.6000.private_bytes=122023936
sample.6000.private_mb=116.37
sample.6000.working_set_bytes=79089664
sample.6000.handles=589
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=49
sample.7000.private_bytes=124067840
sample.7000.private_mb=118.32
sample.7000.working_set_bytes=80101376
sample.7000.handles=589
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=49
sample.8000.private_bytes=126902272
sample.8000.private_mb=121.02
sample.8000.working_set_bytes=82436096
sample.8000.handles=589
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=49
sample.9000.private_bytes=122884096
sample.9000.private_mb=117.19
sample.9000.working_set_bytes=80572416
sample.9000.handles=589
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=49
sample.10000.private_bytes=125730816
sample.10000.private_mb=119.91
sample.10000.working_set_bytes=82558976
sample.10000.handles=589
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=49
sample.11000.private_bytes=129040384
sample.11000.private_mb=123.06
sample.11000.working_set_bytes=85098496
sample.11000.handles=589
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=49
end.private_bytes=129040384
end.private_mb=123.06
end.working_set_bytes=85098496
end.handles=589
end.gdi=45
end.user=21
end.threads=49
idle.private_bytes=110927872
idle.private_mb=105.79
idle.working_set_bytes=62074880
idle.handles=564
idle.gdi=45
idle.user=21
idle.threads=50
delta.private_bytes_from_idle=18112512
delta.private_bytes=12087296
delta.private_pct=10.34
allow.private_bytes=5847654
pass.private=0
delta.handles=-2
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=129040384
trim.private_bytes=122273792
trim.private_mb=116.61
trim.working_set_bytes=78991360
trim.handles=589
trim.gdi=45
trim.user=21
trim.threads=49
trim.delta_private_bytes=-6766592
trim.note=icon cache cleared (1047 entries, cap 2048 items / 8 MB)
timeouts=0
gate=FAIL
fail=private
exit=1

# --- leak-gate-02.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
start_paint_path=hardware
paints=24538
fixture=G:\rig-bench\tsubame-b1\gate
folders=204
largest_folder_entries=10004
navigations=10000
tabs=1000
ops=11000
warmup_navigations=408
warmup_tabs=200
sample_interval=1000
settle_s=5
elapsed_s=38.5
ops_per_s=285.7
navigations_s=34.7
tabs_s=3.8
navigations_per_s=287.9
tabs_per_s=265.0
stage_s.load=26.4
stage_s.paint=13.8
stage_s.icons=1.5
steps=25067
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=115277824
start.private_mb=109.94
start.working_set_bytes=75534336
start.handles=590
start.gdi=45
start.user=21
start.threads=53
sample.0.private_bytes=115277824
sample.0.private_mb=109.94
sample.0.working_set_bytes=75534336
sample.0.handles=590
sample.0.gdi=45
sample.0.user=21
sample.0.threads=53
sample.1000.private_bytes=115068928
sample.1000.private_mb=109.74
sample.1000.working_set_bytes=75505664
sample.1000.handles=590
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=53
sample.2000.private_bytes=115146752
sample.2000.private_mb=109.81
sample.2000.working_set_bytes=75599872
sample.2000.handles=590
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=53
sample.3000.private_bytes=115372032
sample.3000.private_mb=110.03
sample.3000.working_set_bytes=75563008
sample.3000.handles=590
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=53
sample.4000.private_bytes=114880512
sample.4000.private_mb=109.56
sample.4000.working_set_bytes=75702272
sample.4000.handles=586
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=53
sample.5000.private_bytes=117960704
sample.5000.private_mb=112.50
sample.5000.working_set_bytes=77742080
sample.5000.handles=590
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=52
sample.6000.private_bytes=117190656
sample.6000.private_mb=111.76
sample.6000.working_set_bytes=77131776
sample.6000.handles=590
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=52
sample.7000.private_bytes=118222848
sample.7000.private_mb=112.75
sample.7000.working_set_bytes=77778944
sample.7000.handles=590
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=52
sample.8000.private_bytes=118251520
sample.8000.private_mb=112.77
sample.8000.working_set_bytes=77541376
sample.8000.handles=590
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=52
sample.9000.private_bytes=117755904
sample.9000.private_mb=112.30
sample.9000.working_set_bytes=77860864
sample.9000.handles=590
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=52
sample.10000.private_bytes=118493184
sample.10000.private_mb=113.00
sample.10000.working_set_bytes=77914112
sample.10000.handles=590
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=52
sample.11000.private_bytes=119279616
sample.11000.private_mb=113.75
sample.11000.working_set_bytes=79192064
sample.11000.handles=590
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=52
end.private_bytes=119279616
end.private_mb=113.75
end.working_set_bytes=79192064
end.handles=590
end.gdi=45
end.user=21
end.threads=52
idle.private_bytes=113160192
idle.private_mb=107.92
idle.working_set_bytes=62025728
idle.handles=564
idle.gdi=45
idle.user=21
idle.threads=53
delta.private_bytes_from_idle=6119424
delta.private_bytes=4001792
delta.private_pct=3.47
allow.private_bytes=5763891
pass.private=1
delta.handles=0
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=119279616
trim.private_bytes=114262016
trim.private_mb=108.97
trim.working_set_bytes=74616832
trim.handles=590
trim.gdi=45
trim.user=21
trim.threads=52
trim.delta_private_bytes=-5017600
trim.note=icon cache cleared (1042 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-03.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
start_paint_path=hardware
paints=24552
fixture=G:\rig-bench\tsubame-b1\gate
folders=204
largest_folder_entries=10004
navigations=10000
tabs=1000
ops=11000
warmup_navigations=408
warmup_tabs=200
sample_interval=1000
settle_s=5
elapsed_s=38.2
ops_per_s=288.1
navigations_s=34.5
tabs_s=3.7
navigations_per_s=290.0
tabs_per_s=270.1
stage_s.load=26.1
stage_s.paint=13.7
stage_s.icons=1.6
steps=25078
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=120844288
start.private_mb=115.25
start.working_set_bytes=77824000
start.handles=590
start.gdi=45
start.user=21
start.threads=55
sample.0.private_bytes=120844288
sample.0.private_mb=115.25
sample.0.working_set_bytes=77824000
sample.0.handles=590
sample.0.gdi=45
sample.0.user=21
sample.0.threads=55
sample.1000.private_bytes=122101760
sample.1000.private_mb=116.45
sample.1000.working_set_bytes=78139392
sample.1000.handles=590
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=55
sample.2000.private_bytes=121651200
sample.2000.private_mb=116.02
sample.2000.working_set_bytes=78372864
sample.2000.handles=590
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=55
sample.3000.private_bytes=122040320
sample.3000.private_mb=116.39
sample.3000.working_set_bytes=78376960
sample.3000.handles=590
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=55
sample.4000.private_bytes=119812096
sample.4000.private_mb=114.26
sample.4000.working_set_bytes=78065664
sample.4000.handles=590
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=55
sample.5000.private_bytes=123355136
sample.5000.private_mb=117.64
sample.5000.working_set_bytes=81031168
sample.5000.handles=590
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=54
sample.6000.private_bytes=121286656
sample.6000.private_mb=115.67
sample.6000.working_set_bytes=78925824
sample.6000.handles=590
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=54
sample.7000.private_bytes=125321216
sample.7000.private_mb=119.52
sample.7000.working_set_bytes=80515072
sample.7000.handles=590
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=54
sample.8000.private_bytes=121335808
sample.8000.private_mb=115.71
sample.8000.working_set_bytes=79302656
sample.8000.handles=588
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=54
sample.9000.private_bytes=124329984
sample.9000.private_mb=118.57
sample.9000.working_set_bytes=80400384
sample.9000.handles=590
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=54
sample.10000.private_bytes=125431808
sample.10000.private_mb=119.62
sample.10000.working_set_bytes=81817600
sample.10000.handles=590
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=54
sample.11000.private_bytes=126255104
sample.11000.private_mb=120.41
sample.11000.working_set_bytes=81842176
sample.11000.handles=590
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=54
end.private_bytes=126255104
end.private_mb=120.41
end.working_set_bytes=81842176
end.handles=590
end.gdi=45
end.user=21
end.threads=54
idle.private_bytes=111869952
idle.private_mb=106.69
idle.working_set_bytes=62828544
idle.handles=564
idle.gdi=45
idle.user=21
idle.threads=55
delta.private_bytes_from_idle=14385152
delta.private_bytes=5410816
delta.private_pct=4.48
allow.private_bytes=6042214
pass.private=1
delta.handles=0
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=126255104
trim.private_bytes=119107584
trim.private_mb=113.59
trim.working_set_bytes=76304384
trim.handles=590
trim.gdi=45
trim.user=21
trim.threads=54
trim.delta_private_bytes=-7147520
trim.note=icon cache cleared (1042 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0
```
</details>
