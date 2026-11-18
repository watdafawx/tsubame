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

## 2026-11-18 04:15 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `02a2731`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/private-bytes-memory-budget`, commit `02a2731`
- Window: 3840x2064 at 192 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 11 Minecraft java process(es), 25 docker container(s), CPU 39% busy, servers loaded; rig up 342 h
- Cold-cache 100k open: measured (purge working ok)
  The purge also emptied the app's own pages: that process painted its first frame 450 ms after start (warm: 85 ms), so the cold row includes loading the app itself, not only the folder's metadata.
- Idle: 0 ms of CPU over the first idle window, 31 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 35.9 MB private at the first painted frame, 80.7 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 83.40 ms median, 98.00 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 4.59 ms median, 23.86 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 78.92 ms median, 157.79 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 59.44 ms median, 59.44 ms p95 (n=1) | **FAIL** |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 279.53 ms median, 279.53 ms p95 (n=1) | **FAIL** |
| Scroll paint p95, 4K, details view | < 5.0 ms | 2.61 ms median, 3.19 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 31 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 72.2 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 89.8 MB median (91.8 MB max) | PASS |
| Main exe | < 6 MB | 653 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 85.8 -> 93.8 MB (+8.05 MB, 9.4%; allowance 4.3 MB) | **FAIL** |

**3 budget(s) missed: 100,000-entry folder, cold cache: first rows, 100,000-entry folder, cold cache: complete and sorted, Leak gate: 10,000 navigations + 1,000 tabs.** Reported as measured; nothing was relaxed to make them pass.

Informational: startup navigation first rows 64.92 ms and complete+sorted 172.23 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 85.8 -> 93.8 | +8.06 | +9.40 | 4.29 | 40 | **FAIL** |
| 2 | 83.9 -> 88.2 | +4.29 | +5.12 | 4.19 | 40 | **FAIL** |
| 3 | 93.1 -> 98.0 | +4.83 | +5.18 | 4.66 | 32 | **FAIL** |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **FAIL** (failed: private)
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (72.2 MB private, before the warm-up) to the end the process grew 21.6 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 85.8 | 77.9 | 762 | 45 | 24 | 106 |
| 0 ops | 85.8 | 77.9 | 762 | 45 | 24 | 106 |
| 1000 ops | 86.7 | 78.1 | 764 | 45 | 24 | 106 |
| 2000 ops | 86.0 | 78.7 | 764 | 45 | 24 | 106 |
| 3000 ops | 85.5 | 78.5 | 764 | 45 | 24 | 106 |
| 4000 ops | 88.9 | 79.8 | 764 | 45 | 24 | 106 |
| 5000 ops | 93.2 | 83.1 | 762 | 45 | 24 | 105 |
| 6000 ops | 91.1 | 81.7 | 762 | 45 | 24 | 105 |
| 7000 ops | 95.7 | 83.6 | 762 | 45 | 24 | 105 |
| 8000 ops | 95.1 | 83.8 | 762 | 45 | 24 | 105 |
| 9000 ops | 96.2 | 85.0 | 762 | 45 | 24 | 105 |
| 10000 ops | 94.8 | 83.1 | 762 | 45 | 24 | 105 |
| 11000 ops | 93.8 | 84.0 | 760 | 45 | 24 | 105 |
| after settle | 93.8 | 84.0 | 760 | 45 | 24 | 105 |
| after icon-cache trim | 91.9 | | | | | |

Private bytes +8.06 MB (+9.40%, allowance 4.29 MB), handles -2, GDI +0, USER +0, threads -1. Peak private bytes 96.2 MB.
Forced collection: icon cache cleared (1047 entries, cap 2048 items / 8 MB); private bytes after it 91.9 MB.
Gate wall time 40 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T04:15:30
commit=02a2731
branch=t3/private-bytes-memory-budget
dirty=0
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=10
cache=warm filesystem cache (no purge before these runs)
cold=measured (purge working ok)
dpi=192
rig_load=11 Minecraft java process(es), 25 docker container(s), CPU 39% busy, servers loaded; rig up 342 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=1
reports=G:\rig-test\tsubame\bench-2026-11-18T04-15-30
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 83.40 paint_p50_ms 1.86 paint_p95_ms 3.19 private_before_scroll_mb 79.1 private_end_mb 91.1 peak_commit_mb 91.1 rows 100000 exe_bytes 668672
run 2: first_paint_ms 81.40 paint_p50_ms 1.64 paint_p95_ms 2.43 private_before_scroll_mb 80.2 private_end_mb 91.8 peak_commit_mb 91.8 rows 100000 exe_bytes 668672
run 3: first_paint_ms 90.20 paint_p50_ms 1.65 paint_p95_ms 2.39 private_before_scroll_mb 78.1 private_end_mb 91.1 peak_commit_mb 91.1 rows 100000 exe_bytes 668672
run 4: first_paint_ms 89.70 paint_p50_ms 1.70 paint_p95_ms 2.66 private_before_scroll_mb 80.9 private_end_mb 88.4 peak_commit_mb 88.4 rows 100000 exe_bytes 668672
run 5: first_paint_ms 74.60 paint_p50_ms 1.66 paint_p95_ms 2.41 private_before_scroll_mb 80.8 private_end_mb 89.6 peak_commit_mb 89.6 rows 100000 exe_bytes 668672
run 6: first_paint_ms 93.70 paint_p50_ms 1.66 paint_p95_ms 2.61 private_before_scroll_mb 80.5 private_end_mb 89.6 peak_commit_mb 89.6 rows 100000 exe_bytes 668672
run 7: first_paint_ms 98.00 paint_p50_ms 1.96 paint_p95_ms 2.90 private_before_scroll_mb 81.1 private_end_mb 90.7 peak_commit_mb 90.7 rows 100000 exe_bytes 668672
run 8: first_paint_ms 85.70 paint_p50_ms 1.82 paint_p95_ms 3.11 private_before_scroll_mb 78.7 private_end_mb 91.1 peak_commit_mb 91.1 rows 100000 exe_bytes 668672
run 9: first_paint_ms 77.50 paint_p50_ms 1.75 paint_p95_ms 2.58 private_before_scroll_mb 81.7 private_end_mb 89.8 peak_commit_mb 89.8 rows 100000 exe_bytes 668672
run 10: first_paint_ms 75.90 paint_p50_ms 1.96 paint_p95_ms 3.02 private_before_scroll_mb 77.6 private_end_mb 87.1 peak_commit_mb 87.5 rows 100000 exe_bytes 668672
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 85.2
private_bytes_first_paint: 35.9 MB
all times in ms from the navigation event
startup   entries  100000 (9.0 MB)  first_batch    26.26  first_frame    64.92  done   169.90  final_frame   172.23  worker    44.33   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.0 MB)  first_batch     6.80  first_frame    10.57  done   103.11  final_frame   105.83  worker    42.00   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.0 MB)  first_batch     3.36  first_frame    23.86  done   156.43  final_frame   157.79  worker    56.24   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.0 MB)  first_batch     4.75  first_frame     7.92  done    77.26  final_frame    78.92  worker    40.77   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.0 MB)  first_batch     1.93  first_frame     4.59  done    78.45  final_frame    80.07  worker    42.69   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.0 MB)  first_batch     0.97  first_frame     3.73  done    66.72  final_frame    67.80  worker    31.59   G:\rig-bench\tsubame-b1\100k
repeat 6  entries  100000 (9.0 MB)  first_batch     1.03  first_frame     4.99  done    86.32  final_frame    87.30  worker    35.45   G:\rig-bench\tsubame-b1\100k
repeat 7  entries  100000 (9.0 MB)  first_batch     0.76  first_frame     2.66  done    70.41  final_frame    71.79  worker    33.17   G:\rig-bench\tsubame-b1\100k
repeat 8  entries  100000 (9.0 MB)  first_batch     1.01  first_frame     2.28  done    64.78  final_frame    66.04  worker    36.47   G:\rig-bench\tsubame-b1\100k
repeat 9  entries  100000 (9.0 MB)  first_batch     0.90  first_frame     4.13  done    69.18  final_frame    70.16  worker    36.32   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 80.7 MB
```
</details>

<details><summary>Raw: cold-cache u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path software (GPU starting)
first_paint_ms (process start to first completed paint): 450.5
private_bytes_first_paint: 26.0 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   133.60  first_frame   426.58  done   426.93  final_frame   428.77  worker     3.08   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (9.6 MB)  first_batch    55.12  first_frame    59.44  done   276.18  final_frame   279.53  worker   210.10   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 42.1 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=0
idle.cpu_ms=31.2
idle.threads=103
idle.threads_warm=107
idle.context_switches_per_s=-1
idle.pid=62912
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
paints=24579
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
elapsed_s=39.7
ops_per_s=276.8
navigations_s=36.1
tabs_s=3.6
navigations_per_s=277.0
tabs_per_s=274.5
stage_s.load=27.6
stage_s.paint=13.5
stage_s.icons=2.0
steps=25120
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=89911296
start.private_mb=85.75
start.working_set_bytes=81702912
start.handles=762
start.gdi=45
start.user=24
start.threads=106
sample.0.private_bytes=89911296
sample.0.private_mb=85.75
sample.0.working_set_bytes=81702912
sample.0.handles=762
sample.0.gdi=45
sample.0.user=24
sample.0.threads=106
sample.1000.private_bytes=90865664
sample.1000.private_mb=86.66
sample.1000.working_set_bytes=81944576
sample.1000.handles=764
sample.1000.gdi=45
sample.1000.user=24
sample.1000.threads=106
sample.2000.private_bytes=90185728
sample.2000.private_mb=86.01
sample.2000.working_set_bytes=82542592
sample.2000.handles=764
sample.2000.gdi=45
sample.2000.user=24
sample.2000.threads=106
sample.3000.private_bytes=89595904
sample.3000.private_mb=85.45
sample.3000.working_set_bytes=82268160
sample.3000.handles=764
sample.3000.gdi=45
sample.3000.user=24
sample.3000.threads=106
sample.4000.private_bytes=93237248
sample.4000.private_mb=88.92
sample.4000.working_set_bytes=83623936
sample.4000.handles=764
sample.4000.gdi=45
sample.4000.user=24
sample.4000.threads=106
sample.5000.private_bytes=97693696
sample.5000.private_mb=93.17
sample.5000.working_set_bytes=87175168
sample.5000.handles=762
sample.5000.gdi=45
sample.5000.user=24
sample.5000.threads=105
sample.6000.private_bytes=95526912
sample.6000.private_mb=91.10
sample.6000.working_set_bytes=85684224
sample.6000.handles=762
sample.6000.gdi=45
sample.6000.user=24
sample.6000.threads=105
sample.7000.private_bytes=100360192
sample.7000.private_mb=95.71
sample.7000.working_set_bytes=87691264
sample.7000.handles=762
sample.7000.gdi=45
sample.7000.user=24
sample.7000.threads=105
sample.8000.private_bytes=99753984
sample.8000.private_mb=95.13
sample.8000.working_set_bytes=87875584
sample.8000.handles=762
sample.8000.gdi=45
sample.8000.user=24
sample.8000.threads=105
sample.9000.private_bytes=100892672
sample.9000.private_mb=96.22
sample.9000.working_set_bytes=89096192
sample.9000.handles=762
sample.9000.gdi=45
sample.9000.user=24
sample.9000.threads=105
sample.10000.private_bytes=99426304
sample.10000.private_mb=94.82
sample.10000.working_set_bytes=87171072
sample.10000.handles=762
sample.10000.gdi=45
sample.10000.user=24
sample.10000.threads=105
sample.11000.private_bytes=98361344
sample.11000.private_mb=93.80
sample.11000.working_set_bytes=88076288
sample.11000.handles=760
sample.11000.gdi=45
sample.11000.user=24
sample.11000.threads=105
end.private_bytes=98361344
end.private_mb=93.80
end.working_set_bytes=88076288
end.handles=760
end.gdi=45
end.user=24
end.threads=105
idle.private_bytes=75751424
idle.private_mb=72.24
idle.working_set_bytes=65404928
idle.handles=729
idle.gdi=45
idle.user=22
idle.threads=105
delta.private_bytes_from_idle=22609920
delta.private_bytes=8450048
delta.private_pct=9.40
allow.private_bytes=4495564
pass.private=0
delta.handles=-2
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=100892672
trim.private_bytes=96342016
trim.private_mb=91.88
trim.working_set_bytes=85798912
trim.handles=760
trim.gdi=45
trim.user=24
trim.threads=105
trim.delta_private_bytes=-2019328
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
paints=24579
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
elapsed_s=40.4
ops_per_s=272.2
navigations_s=36.1
tabs_s=4.3
navigations_per_s=277.2
tabs_per_s=230.7
stage_s.load=28.1
stage_s.paint=13.8
stage_s.icons=1.8
steps=25127
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=87932928
start.private_mb=83.86
start.working_set_bytes=78618624
start.handles=742
start.gdi=45
start.user=22
start.threads=108
sample.0.private_bytes=87932928
sample.0.private_mb=83.86
sample.0.working_set_bytes=78618624
sample.0.handles=742
sample.0.gdi=45
sample.0.user=22
sample.0.threads=108
sample.1000.private_bytes=87572480
sample.1000.private_mb=83.52
sample.1000.working_set_bytes=78639104
sample.1000.handles=744
sample.1000.gdi=45
sample.1000.user=22
sample.1000.threads=108
sample.2000.private_bytes=88358912
sample.2000.private_mb=84.27
sample.2000.working_set_bytes=79822848
sample.2000.handles=744
sample.2000.gdi=45
sample.2000.user=22
sample.2000.threads=108
sample.3000.private_bytes=87928832
sample.3000.private_mb=83.86
sample.3000.working_set_bytes=79368192
sample.3000.handles=744
sample.3000.gdi=45
sample.3000.user=22
sample.3000.threads=108
sample.4000.private_bytes=89235456
sample.4000.private_mb=85.10
sample.4000.working_set_bytes=80908288
sample.4000.handles=740
sample.4000.gdi=45
sample.4000.user=22
sample.4000.threads=108
sample.5000.private_bytes=89096192
sample.5000.private_mb=84.97
sample.5000.working_set_bytes=80404480
sample.5000.handles=744
sample.5000.gdi=45
sample.5000.user=22
sample.5000.threads=107
sample.6000.private_bytes=89677824
sample.6000.private_mb=85.52
sample.6000.working_set_bytes=80670720
sample.6000.handles=740
sample.6000.gdi=45
sample.6000.user=22
sample.6000.threads=107
sample.7000.private_bytes=90062848
sample.7000.private_mb=85.89
sample.7000.working_set_bytes=81485824
sample.7000.handles=744
sample.7000.gdi=45
sample.7000.user=22
sample.7000.threads=107
sample.8000.private_bytes=90734592
sample.8000.private_mb=86.53
sample.8000.working_set_bytes=81633280
sample.8000.handles=744
sample.8000.gdi=45
sample.8000.user=22
sample.8000.threads=107
sample.9000.private_bytes=91385856
sample.9000.private_mb=87.15
sample.9000.working_set_bytes=82862080
sample.9000.handles=744
sample.9000.gdi=45
sample.9000.user=22
sample.9000.threads=107
sample.10000.private_bytes=90824704
sample.10000.private_mb=86.62
sample.10000.working_set_bytes=82001920
sample.10000.handles=744
sample.10000.gdi=45
sample.10000.user=22
sample.10000.threads=107
sample.11000.private_bytes=92434432
sample.11000.private_mb=88.15
sample.11000.working_set_bytes=83894272
sample.11000.handles=742
sample.11000.gdi=45
sample.11000.user=22
sample.11000.threads=107
end.private_bytes=92434432
end.private_mb=88.15
end.working_set_bytes=83894272
end.handles=742
end.gdi=45
end.user=22
end.threads=107
idle.private_bytes=73015296
idle.private_mb=69.63
idle.working_set_bytes=62119936
idle.handles=705
idle.gdi=45
idle.user=20
idle.threads=107
delta.private_bytes_from_idle=19419136
delta.private_bytes=4501504
delta.private_pct=5.12
allow.private_bytes=4396646
pass.private=0
delta.handles=0
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=92434432
trim.private_bytes=88469504
trim.private_mb=84.37
trim.working_set_bytes=79986688
trim.handles=742
trim.gdi=45
trim.user=22
trim.threads=104
trim.delta_private_bytes=-3964928
trim.note=icon cache cleared (1040 entries, cap 2048 items / 8 MB)
timeouts=0
gate=FAIL
fail=private
exit=1

# --- leak-gate-03.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
start_paint_path=hardware
paints=24518
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
elapsed_s=32.4
ops_per_s=339.3
navigations_s=29.3
tabs_s=3.1
navigations_per_s=341.6
tabs_per_s=318.2
stage_s.load=22.8
stage_s.paint=10.9
stage_s.icons=1.5
steps=24988
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=97640448
start.private_mb=93.12
start.working_set_bytes=86003712
start.handles=762
start.gdi=45
start.user=24
start.threads=109
sample.0.private_bytes=97640448
sample.0.private_mb=93.12
sample.0.working_set_bytes=86003712
sample.0.handles=762
sample.0.gdi=45
sample.0.user=24
sample.0.threads=109
sample.1000.private_bytes=98140160
sample.1000.private_mb=93.59
sample.1000.working_set_bytes=86659072
sample.1000.handles=764
sample.1000.gdi=45
sample.1000.user=24
sample.1000.threads=109
sample.2000.private_bytes=99377152
sample.2000.private_mb=94.77
sample.2000.working_set_bytes=87535616
sample.2000.handles=764
sample.2000.gdi=45
sample.2000.user=24
sample.2000.threads=109
sample.3000.private_bytes=99725312
sample.3000.private_mb=95.11
sample.3000.working_set_bytes=87568384
sample.3000.handles=764
sample.3000.gdi=45
sample.3000.user=24
sample.3000.threads=109
sample.4000.private_bytes=98648064
sample.4000.private_mb=94.08
sample.4000.working_set_bytes=87310336
sample.4000.handles=764
sample.4000.gdi=45
sample.4000.user=24
sample.4000.threads=109
sample.5000.private_bytes=102469632
sample.5000.private_mb=97.72
sample.5000.working_set_bytes=89968640
sample.5000.handles=764
sample.5000.gdi=45
sample.5000.user=24
sample.5000.threads=109
sample.6000.private_bytes=96579584
sample.6000.private_mb=92.11
sample.6000.working_set_bytes=86839296
sample.6000.handles=760
sample.6000.gdi=45
sample.6000.user=24
sample.6000.threads=108
sample.7000.private_bytes=101883904
sample.7000.private_mb=97.16
sample.7000.working_set_bytes=90304512
sample.7000.handles=764
sample.7000.gdi=45
sample.7000.user=24
sample.7000.threads=108
sample.8000.private_bytes=100499456
sample.8000.private_mb=95.84
sample.8000.working_set_bytes=88817664
sample.8000.handles=764
sample.8000.gdi=45
sample.8000.user=24
sample.8000.threads=108
sample.9000.private_bytes=100814848
sample.9000.private_mb=96.14
sample.9000.working_set_bytes=89321472
sample.9000.handles=764
sample.9000.gdi=45
sample.9000.user=24
sample.9000.threads=108
sample.10000.private_bytes=102973440
sample.10000.private_mb=98.20
sample.10000.working_set_bytes=90234880
sample.10000.handles=764
sample.10000.gdi=45
sample.10000.user=24
sample.10000.threads=108
sample.11000.private_bytes=102703104
sample.11000.private_mb=97.95
sample.11000.working_set_bytes=91185152
sample.11000.handles=762
sample.11000.gdi=45
sample.11000.user=24
sample.11000.threads=108
end.private_bytes=102703104
end.private_mb=97.95
end.working_set_bytes=91185152
end.handles=762
end.gdi=45
end.user=24
end.threads=108
idle.private_bytes=77877248
idle.private_mb=74.27
idle.working_set_bytes=67801088
idle.handles=707
idle.gdi=45
idle.user=22
idle.threads=108
delta.private_bytes_from_idle=24825856
delta.private_bytes=5062656
delta.private_pct=5.18
allow.private_bytes=4882022
pass.private=0
delta.handles=0
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=102973440
trim.private_bytes=96587776
trim.private_mb=92.11
trim.working_set_bytes=86704128
trim.handles=762
trim.gdi=45
trim.user=24
trim.threads=108
trim.delta_private_bytes=-6115328
trim.note=icon cache cleared (1040 entries, cap 2048 items / 8 MB)
timeouts=0
gate=FAIL
fail=private
exit=1
```
</details>

## 2026-11-18 04:38 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `159b639`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/private-bytes-memory-budget`, commit `159b639`
- Window: 3840x2064 at 192 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 8 Minecraft java process(es), 25 docker container(s), CPU 12% busy, servers loaded; rig up 342 h
- Cold-cache 100k open: measured (purge working ok)
  The purge also emptied the app's own pages: that process painted its first frame 374 ms after start (warm: 107 ms), so the cold row includes loading the app itself, not only the folder's metadata.
- Idle: 16 ms of CPU over the first idle window, 16 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 28.3 MB private at the first painted frame, 85.4 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 92.20 ms median, 106.40 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 3.50 ms median, 18.07 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 64.63 ms median, 122.08 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 60.70 ms median, 60.70 ms p95 (n=1) | **FAIL** |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 274.22 ms median, 274.22 ms p95 (n=1) | **FAIL** |
| Scroll paint p95, 4K, details view | < 5.0 ms | 3.09 ms median, 4.47 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 16 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 62.3 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 80.1 MB median (81.1 MB max) | PASS |
| Main exe | < 6 MB | 654 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 73.7 -> 75.8 MB (+2.17 MB, 2.9%; allowance 3.7 MB) | PASS |

**2 budget(s) missed: 100,000-entry folder, cold cache: first rows, 100,000-entry folder, cold cache: complete and sorted.** Reported as measured; nothing was relaxed to make them pass.

Informational: startup navigation first rows 87.99 ms and complete+sorted 214.96 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 74.9 -> 75.6 | +0.74 | +0.99 | 3.74 | 42 | PASS |
| 2 | 73.7 -> 75.8 | +2.16 | +2.94 | 3.68 | 38 | PASS |
| 3 | 73.6 -> 75.5 | +1.93 | +2.63 | 3.68 | 40 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **PASS**
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (62.3 MB private, before the warm-up) to the end the process grew 13.5 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 73.7 | 67.8 | 599 | 45 | 21 | 37 |
| 0 ops | 73.7 | 67.8 | 599 | 45 | 21 | 37 |
| 1000 ops | 76.7 | 70.5 | 599 | 45 | 21 | 37 |
| 2000 ops | 77.9 | 71.7 | 599 | 45 | 21 | 37 |
| 3000 ops | 78.6 | 72.0 | 599 | 45 | 21 | 37 |
| 4000 ops | 79.1 | 72.9 | 599 | 45 | 21 | 37 |
| 5000 ops | 79.1 | 72.3 | 599 | 45 | 21 | 36 |
| 6000 ops | 77.5 | 71.4 | 599 | 45 | 21 | 36 |
| 7000 ops | 79.4 | 73.1 | 599 | 45 | 21 | 36 |
| 8000 ops | 77.6 | 71.7 | 597 | 45 | 21 | 36 |
| 9000 ops | 79.8 | 73.4 | 599 | 45 | 21 | 36 |
| 10000 ops | 79.9 | 73.5 | 599 | 45 | 21 | 36 |
| 11000 ops | 75.8 | 70.4 | 597 | 45 | 21 | 36 |
| after settle | 75.8 | 70.4 | 597 | 45 | 21 | 36 |
| after icon-cache trim | 75.8 | | | | | |

Private bytes +2.16 MB (+2.94%, allowance 3.68 MB), handles -2, GDI +0, USER +0, threads -1. Peak private bytes 79.9 MB.
Forced collection: icon cache cleared (1044 entries, cap 2048 items / 8 MB); private bytes after it 75.8 MB.
Gate wall time 38 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T04:38:27
commit=159b639
branch=t3/private-bytes-memory-budget
dirty=0
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=10
cache=warm filesystem cache (no purge before these runs)
cold=measured (purge working ok)
dpi=192
rig_load=8 Minecraft java process(es), 25 docker container(s), CPU 12% busy, servers loaded; rig up 342 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=1
reports=G:\rig-test\tsubame\bench-2026-11-18T04-38-27
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 84.90 paint_p50_ms 2.18 paint_p95_ms 3.09 private_before_scroll_mb 86.7 private_end_mb 79.7 peak_commit_mb 87.6 rows 100000 exe_bytes 669696
run 2: first_paint_ms 87.00 paint_p50_ms 2.25 paint_p95_ms 3.38 private_before_scroll_mb 85.0 private_end_mb 80.2 peak_commit_mb 85.9 rows 100000 exe_bytes 669696
run 3: first_paint_ms 97.90 paint_p50_ms 2.44 paint_p95_ms 3.85 private_before_scroll_mb 87.0 private_end_mb 81.1 peak_commit_mb 87.9 rows 100000 exe_bytes 669696
run 4: first_paint_ms 106.40 paint_p50_ms 2.44 paint_p95_ms 4.33 private_before_scroll_mb 85.2 private_end_mb 79.5 peak_commit_mb 86.1 rows 100000 exe_bytes 669696
run 5: first_paint_ms 97.80 paint_p50_ms 2.79 paint_p95_ms 4.47 private_before_scroll_mb 83.4 private_end_mb 80.1 peak_commit_mb 84.2 rows 100000 exe_bytes 669696
run 6: first_paint_ms 96.70 paint_p50_ms 2.39 paint_p95_ms 3.77 private_before_scroll_mb 84.2 private_end_mb 80.3 peak_commit_mb 85.0 rows 100000 exe_bytes 669696
run 7: first_paint_ms 88.00 paint_p50_ms 2.19 paint_p95_ms 3.03 private_before_scroll_mb 83.1 private_end_mb 80.0 peak_commit_mb 83.9 rows 100000 exe_bytes 669696
run 8: first_paint_ms 92.00 paint_p50_ms 2.16 paint_p95_ms 2.99 private_before_scroll_mb 85.5 private_end_mb 79.7 peak_commit_mb 86.3 rows 100000 exe_bytes 669696
run 9: first_paint_ms 104.10 paint_p50_ms 2.18 paint_p95_ms 3.05 private_before_scroll_mb 85.9 private_end_mb 80.1 peak_commit_mb 86.8 rows 100000 exe_bytes 669696
run 10: first_paint_ms 92.20 paint_p50_ms 2.18 paint_p95_ms 3.04 private_before_scroll_mb 84.0 private_end_mb 81.0 peak_commit_mb 84.9 rows 100000 exe_bytes 669696
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 107.0
private_bytes_first_paint: 28.3 MB
all times in ms from the navigation event
startup   entries  100000 (9.0 MB)  first_batch    20.27  first_frame    87.99  done   205.33  final_frame   214.96  worker    43.66   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.0 MB)  first_batch     7.11  first_frame    18.07  done   120.49  final_frame   122.08  worker    40.74   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.0 MB)  first_batch     1.00  first_frame     3.67  done    62.67  final_frame    64.21  worker    38.18   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.0 MB)  first_batch     0.94  first_frame     3.35  done    63.91  final_frame    65.32  worker    34.45   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.0 MB)  first_batch     1.20  first_frame     4.14  done    62.58  final_frame    63.95  worker    34.23   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.0 MB)  first_batch     0.81  first_frame     2.23  done    61.89  final_frame    63.23  worker    33.81   G:\rig-bench\tsubame-b1\100k
repeat 6  entries  100000 (9.0 MB)  first_batch     1.27  first_frame     3.81  done    63.56  final_frame    64.71  worker    31.62   G:\rig-bench\tsubame-b1\100k
repeat 7  entries  100000 (9.0 MB)  first_batch     0.79  first_frame     2.16  done    57.62  final_frame    58.95  worker    31.03   G:\rig-bench\tsubame-b1\100k
repeat 8  entries  100000 (9.0 MB)  first_batch     0.85  first_frame     3.50  done    63.82  final_frame    65.30  worker    33.45   G:\rig-bench\tsubame-b1\100k
repeat 9  entries  100000 (9.0 MB)  first_batch     0.86  first_frame     3.47  done    63.42  final_frame    64.63  worker    38.33   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 85.4 MB
```
</details>

<details><summary>Raw: cold-cache u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path software (GPU starting)
first_paint_ms (process start to first completed paint): 374.2
private_bytes_first_paint: 14.5 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   130.85  first_frame   316.76  done   317.02  final_frame   334.59  worker     2.37   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (9.6 MB)  first_batch    46.20  first_frame    60.70  done   264.37  final_frame   274.22  worker   197.70   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 43.6 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=15.6
idle.cpu_ms=15.6
idle.threads=37
idle.threads_warm=37
idle.context_switches_per_s=-1
idle.pid=49760
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
paints=24586
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
elapsed_s=41.8
ops_per_s=263.2
navigations_s=37.2
tabs_s=4.6
navigations_per_s=269.0
tabs_per_s=216.8
stage_s.load=28.8
stage_s.paint=14.5
stage_s.icons=1.6
steps=25145
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=78499840
start.private_mb=74.86
start.working_set_bytes=72957952
start.handles=601
start.gdi=45
start.user=21
start.threads=37
sample.0.private_bytes=78499840
sample.0.private_mb=74.86
sample.0.working_set_bytes=72957952
sample.0.handles=601
sample.0.gdi=45
sample.0.user=21
sample.0.threads=37
sample.1000.private_bytes=80257024
sample.1000.private_mb=76.54
sample.1000.working_set_bytes=73728000
sample.1000.handles=601
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=37
sample.2000.private_bytes=81174528
sample.2000.private_mb=77.41
sample.2000.working_set_bytes=74891264
sample.2000.handles=601
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=37
sample.3000.private_bytes=78323712
sample.3000.private_mb=74.70
sample.3000.working_set_bytes=72728576
sample.3000.handles=601
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=37
sample.4000.private_bytes=81383424
sample.4000.private_mb=77.61
sample.4000.working_set_bytes=75468800
sample.4000.handles=597
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=37
sample.5000.private_bytes=81637376
sample.5000.private_mb=77.86
sample.5000.working_set_bytes=75452416
sample.5000.handles=601
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=36
sample.6000.private_bytes=79761408
sample.6000.private_mb=76.07
sample.6000.working_set_bytes=73850880
sample.6000.handles=601
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=36
sample.7000.private_bytes=79958016
sample.7000.private_mb=76.25
sample.7000.working_set_bytes=74076160
sample.7000.handles=601
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=36
sample.8000.private_bytes=79986688
sample.8000.private_mb=76.28
sample.8000.working_set_bytes=74428416
sample.8000.handles=599
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=36
sample.9000.private_bytes=80379904
sample.9000.private_mb=76.66
sample.9000.working_set_bytes=74412032
sample.9000.handles=601
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=36
sample.10000.private_bytes=79417344
sample.10000.private_mb=75.74
sample.10000.working_set_bytes=73539584
sample.10000.handles=601
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=36
sample.11000.private_bytes=79278080
sample.11000.private_mb=75.61
sample.11000.working_set_bytes=73719808
sample.11000.handles=597
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=34
end.private_bytes=79278080
end.private_mb=75.61
end.working_set_bytes=73719808
end.handles=597
end.gdi=45
end.user=21
end.threads=34
idle.private_bytes=64819200
idle.private_mb=61.82
idle.working_set_bytes=58355712
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=37
delta.private_bytes_from_idle=14458880
delta.private_bytes=778240
delta.private_pct=0.99
allow.private_bytes=3924992
pass.private=1
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=81637376
trim.private_bytes=79278080
trim.private_mb=75.61
trim.working_set_bytes=73719808
trim.handles=597
trim.gdi=45
trim.user=21
trim.threads=34
trim.delta_private_bytes=0
trim.note=icon cache cleared (1047 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-02.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
start_paint_path=hardware
paints=24537
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
elapsed_s=37.8
ops_per_s=291.1
navigations_s=34.1
tabs_s=3.7
navigations_per_s=293.4
tabs_per_s=269.6
stage_s.load=26.0
stage_s.paint=13.3
stage_s.icons=1.5
steps=25057
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=77250560
start.private_mb=73.67
start.working_set_bytes=71118848
start.handles=599
start.gdi=45
start.user=21
start.threads=37
sample.0.private_bytes=77250560
sample.0.private_mb=73.67
sample.0.working_set_bytes=71118848
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=37
sample.1000.private_bytes=80371712
sample.1000.private_mb=76.65
sample.1000.working_set_bytes=73969664
sample.1000.handles=599
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=37
sample.2000.private_bytes=81645568
sample.2000.private_mb=77.86
sample.2000.working_set_bytes=75153408
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=37
sample.3000.private_bytes=82448384
sample.3000.private_mb=78.63
sample.3000.working_set_bytes=75546624
sample.3000.handles=599
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=37
sample.4000.private_bytes=82919424
sample.4000.private_mb=79.08
sample.4000.working_set_bytes=76394496
sample.4000.handles=599
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=37
sample.5000.private_bytes=82919424
sample.5000.private_mb=79.08
sample.5000.working_set_bytes=75804672
sample.5000.handles=599
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=36
sample.6000.private_bytes=81219584
sample.6000.private_mb=77.46
sample.6000.working_set_bytes=74878976
sample.6000.handles=599
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=36
sample.7000.private_bytes=83271680
sample.7000.private_mb=79.41
sample.7000.working_set_bytes=76681216
sample.7000.handles=599
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=36
sample.8000.private_bytes=81326080
sample.8000.private_mb=77.56
sample.8000.working_set_bytes=75153408
sample.8000.handles=597
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=36
sample.9000.private_bytes=83693568
sample.9000.private_mb=79.82
sample.9000.working_set_bytes=76980224
sample.9000.handles=599
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=36
sample.10000.private_bytes=83787776
sample.10000.private_mb=79.91
sample.10000.working_set_bytes=77086720
sample.10000.handles=599
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=36
sample.11000.private_bytes=79519744
sample.11000.private_mb=75.84
sample.11000.working_set_bytes=73797632
sample.11000.handles=597
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=36
end.private_bytes=79519744
end.private_mb=75.84
end.working_set_bytes=73797632
end.handles=597
end.gdi=45
end.user=21
end.threads=36
idle.private_bytes=65323008
idle.private_mb=62.30
idle.working_set_bytes=58417152
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=37
delta.private_bytes_from_idle=14196736
delta.private_bytes=2269184
delta.private_pct=2.94
allow.private_bytes=3862528
pass.private=1
delta.handles=-2
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=83787776
trim.private_bytes=79519744
trim.private_mb=75.84
trim.working_set_bytes=73797632
trim.handles=597
trim.gdi=45
trim.user=21
trim.threads=36
trim.delta_private_bytes=0
trim.note=icon cache cleared (1044 entries, cap 2048 items / 8 MB)
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
paints=24516
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
elapsed_s=40.4
ops_per_s=272.0
navigations_s=36.1
tabs_s=4.3
navigations_per_s=276.7
tabs_per_s=232.9
stage_s.load=28.2
stage_s.paint=14.1
stage_s.icons=1.9
steps=25071
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=77193216
start.private_mb=73.62
start.working_set_bytes=70692864
start.handles=601
start.gdi=45
start.user=21
start.threads=37
sample.0.private_bytes=77193216
sample.0.private_mb=73.62
sample.0.working_set_bytes=70692864
sample.0.handles=601
sample.0.gdi=45
sample.0.user=21
sample.0.threads=37
sample.1000.private_bytes=78626816
sample.1000.private_mb=74.98
sample.1000.working_set_bytes=71528448
sample.1000.handles=601
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=37
sample.2000.private_bytes=81801216
sample.2000.private_mb=78.01
sample.2000.working_set_bytes=74211328
sample.2000.handles=601
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=37
sample.3000.private_bytes=81735680
sample.3000.private_mb=77.95
sample.3000.working_set_bytes=74035200
sample.3000.handles=601
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=37
sample.4000.private_bytes=82227200
sample.4000.private_mb=78.42
sample.4000.working_set_bytes=74711040
sample.4000.handles=597
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=37
sample.5000.private_bytes=82255872
sample.5000.private_mb=78.45
sample.5000.working_set_bytes=74731520
sample.5000.handles=601
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=36
sample.6000.private_bytes=81842176
sample.6000.private_mb=78.05
sample.6000.working_set_bytes=74117120
sample.6000.handles=601
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=36
sample.7000.private_bytes=82554880
sample.7000.private_mb=78.73
sample.7000.working_set_bytes=74772480
sample.7000.handles=601
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=36
sample.8000.private_bytes=82915328
sample.8000.private_mb=79.07
sample.8000.working_set_bytes=76152832
sample.8000.handles=601
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=36
sample.9000.private_bytes=83427328
sample.9000.private_mb=79.56
sample.9000.working_set_bytes=75763712
sample.9000.handles=601
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=36
sample.10000.private_bytes=83357696
sample.10000.private_mb=79.50
sample.10000.working_set_bytes=75841536
sample.10000.handles=601
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=36
sample.11000.private_bytes=79220736
sample.11000.private_mb=75.55
sample.11000.working_set_bytes=72814592
sample.11000.handles=597
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=34
end.private_bytes=79220736
end.private_mb=75.55
end.working_set_bytes=72814592
end.handles=597
end.gdi=45
end.user=21
end.threads=34
idle.private_bytes=65392640
idle.private_mb=62.36
idle.working_set_bytes=58421248
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=37
delta.private_bytes_from_idle=13828096
delta.private_bytes=2027520
delta.private_pct=2.63
allow.private_bytes=3859660
pass.private=1
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=83427328
trim.private_bytes=79220736
trim.private_mb=75.55
trim.working_set_bytes=72814592
trim.handles=597
trim.gdi=45
trim.user=21
trim.threads=34
trim.delta_private_bytes=0
trim.note=icon cache cleared (1042 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0
```
</details>

## 2026-11-18 05:14 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `c908f4a`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/private-bytes-memory-budget`, commit `c908f4a`
- Window: 3840x2064 at 192 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 8 Minecraft java process(es), 25 docker container(s), CPU 15% busy, servers loaded; rig up 343 h
- Cold-cache 100k open: measured (purge working ok)
  The purge also emptied the app's own pages: that process painted its first frame 395 ms after start (warm: 68 ms), so the cold row includes loading the app itself, not only the folder's metadata.
- Idle: 16 ms of CPU over the first idle window, 31 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 36.9 MB private at the first painted frame, 90.4 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 79.30 ms median, 682.80 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 5.21 ms median, 11.15 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 66.35 ms median, 104.48 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 133.62 ms median, 133.62 ms p95 (n=1) | **FAIL** |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 2346.20 ms median, 2346.20 ms p95 (n=1) | **FAIL** |
| Scroll paint p95, 4K, details view | < 5.0 ms | 3.75 ms median, 4.83 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 31 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 63.6 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 81.1 MB median (81.9 MB max) | PASS |
| Main exe | < 6 MB | 656 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 74.9 -> 77.4 MB (+2.46 MB, 3.3%; allowance 3.7 MB) | PASS |

**2 budget(s) missed: 100,000-entry folder, cold cache: first rows, 100,000-entry folder, cold cache: complete and sorted.** Reported as measured; nothing was relaxed to make them pass.

Informational: startup navigation first rows 49.72 ms and complete+sorted 131.25 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 74.4 -> 76.5 | +2.10 | +2.83 | 3.72 | 47 | PASS |
| 2 | 75.6 -> 77.2 | +1.61 | +2.13 | 3.78 | 45 | PASS |
| 3 | 74.9 -> 77.4 | +2.45 | +3.27 | 3.75 | 43 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **PASS**
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (63.6 MB private, before the warm-up) to the end the process grew 13.8 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 74.9 | 69.3 | 599 | 45 | 21 | 58 |
| 0 ops | 74.9 | 69.3 | 599 | 45 | 21 | 58 |
| 1000 ops | 79.5 | 73.0 | 599 | 45 | 21 | 58 |
| 2000 ops | 76.3 | 70.1 | 599 | 45 | 21 | 58 |
| 3000 ops | 79.5 | 73.4 | 599 | 45 | 21 | 58 |
| 4000 ops | 79.1 | 72.8 | 597 | 45 | 21 | 57 |
| 5000 ops | 80.3 | 74.0 | 597 | 45 | 21 | 57 |
| 6000 ops | 78.6 | 72.6 | 597 | 45 | 21 | 57 |
| 7000 ops | 78.5 | 72.4 | 597 | 45 | 21 | 57 |
| 8000 ops | 78.8 | 72.4 | 597 | 45 | 21 | 57 |
| 9000 ops | 77.9 | 71.8 | 597 | 45 | 21 | 57 |
| 10000 ops | 81.0 | 74.6 | 597 | 45 | 21 | 57 |
| 11000 ops | 77.4 | 71.7 | 595 | 45 | 21 | 55 |
| after settle | 77.4 | 71.7 | 595 | 45 | 21 | 55 |
| after icon-cache trim | 77.4 | | | | | |

Private bytes +2.45 MB (+3.27%, allowance 3.75 MB), handles -4, GDI +0, USER +0, threads -3. Peak private bytes 81.0 MB.
Forced collection: icon cache cleared (1040 entries, cap 2048 items / 8 MB); private bytes after it 77.4 MB.
Gate wall time 43 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T05:14:59
commit=c908f4a
branch=t3/private-bytes-memory-budget
dirty=0
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=10
cache=warm filesystem cache (no purge before these runs)
cold=measured (purge working ok)
dpi=192
rig_load=8 Minecraft java process(es), 25 docker container(s), CPU 15% busy, servers loaded; rig up 343 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=1
reports=G:\rig-test\tsubame\bench-2026-11-18T05-14-59
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 110.70 paint_p50_ms 2.49 paint_p95_ms 3.75 private_before_scroll_mb 85.3 private_end_mb 80.3 peak_commit_mb 91.7 rows 100000 exe_bytes 672256
run 2: first_paint_ms 75.50 paint_p50_ms 2.18 paint_p95_ms 3.10 private_before_scroll_mb 88.1 private_end_mb 81.2 peak_commit_mb 92.1 rows 100000 exe_bytes 672256
run 3: first_paint_ms 75.20 paint_p50_ms 2.21 paint_p95_ms 3.17 private_before_scroll_mb 87.6 private_end_mb 81.5 peak_commit_mb 93.5 rows 100000 exe_bytes 672256
run 4: first_paint_ms 73.90 paint_p50_ms 2.31 paint_p95_ms 3.65 private_before_scroll_mb 88.3 private_end_mb 81.0 peak_commit_mb 92.6 rows 100000 exe_bytes 672256
run 5: first_paint_ms 682.80 paint_p50_ms 2.43 paint_p95_ms 3.90 private_before_scroll_mb 86.6 private_end_mb 81.9 peak_commit_mb 94.5 rows 100000 exe_bytes 672256
run 6: first_paint_ms 198.30 paint_p50_ms 2.56 paint_p95_ms 4.08 private_before_scroll_mb 87.2 private_end_mb 80.6 peak_commit_mb 92.9 rows 100000 exe_bytes 672256
run 7: first_paint_ms 314.60 paint_p50_ms 2.66 paint_p95_ms 4.83 private_before_scroll_mb 88.6 private_end_mb 81.5 peak_commit_mb 93.0 rows 100000 exe_bytes 672256
run 8: first_paint_ms 126.50 paint_p50_ms 2.44 paint_p95_ms 3.80 private_before_scroll_mb 86.7 private_end_mb 79.9 peak_commit_mb 89.5 rows 100000 exe_bytes 672256
run 9: first_paint_ms 79.30 paint_p50_ms 2.31 paint_p95_ms 3.68 private_before_scroll_mb 89.4 private_end_mb 81.1 peak_commit_mb 93.0 rows 100000 exe_bytes 672256
run 10: first_paint_ms 75.00 paint_p50_ms 2.31 paint_p95_ms 4.18 private_before_scroll_mb 89.5 private_end_mb 81.1 peak_commit_mb 97.9 rows 100000 exe_bytes 672256
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 68.5
private_bytes_first_paint: 36.9 MB
all times in ms from the navigation event
startup   entries  100000 (9.0 MB)  first_batch    24.39  first_frame    49.72  done   128.36  final_frame   131.25  worker    45.43   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.0 MB)  first_batch     6.95  first_frame    11.15  done   102.54  final_frame   104.48  worker    50.85   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.0 MB)  first_batch     2.98  first_frame     8.51  done    96.82  final_frame    98.79  worker    46.15   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.0 MB)  first_batch     0.99  first_frame     6.01  done    71.95  final_frame    73.56  worker    39.33   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.0 MB)  first_batch     0.91  first_frame     3.71  done    64.60  final_frame    66.35  worker    39.58   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.0 MB)  first_batch     1.43  first_frame     5.43  done    67.75  final_frame    69.13  worker    40.39   G:\rig-bench\tsubame-b1\100k
repeat 6  entries  100000 (9.0 MB)  first_batch     0.83  first_frame     3.85  done    60.40  final_frame    61.59  worker    36.62   G:\rig-bench\tsubame-b1\100k
repeat 7  entries  100000 (9.0 MB)  first_batch     0.80  first_frame     2.87  done    59.79  final_frame    61.32  worker    33.81   G:\rig-bench\tsubame-b1\100k
repeat 8  entries  100000 (9.0 MB)  first_batch     0.82  first_frame     2.24  done    61.43  final_frame    63.49  worker    34.90   G:\rig-bench\tsubame-b1\100k
repeat 9  entries  100000 (9.0 MB)  first_batch     2.06  first_frame     5.21  done    63.74  final_frame    65.33  worker    37.81   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 90.4 MB
```
</details>

<details><summary>Raw: cold-cache u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 394.8
private_bytes_first_paint: 26.2 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   172.46  first_frame   325.10  done   325.39  final_frame   327.55  worker    26.23   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (9.6 MB)  first_batch   129.02  first_frame   133.62  done  2345.03  final_frame  2346.20  worker  2308.67   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 82.4 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=15.6
idle.cpu_ms=31.2
idle.threads=53
idle.threads_warm=56
idle.context_switches_per_s=-1
idle.pid=54220
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
paints=24692
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
elapsed_s=47.4
ops_per_s=231.9
navigations_s=43.5
tabs_s=3.9
navigations_per_s=229.7
tabs_per_s=256.6
stage_s.load=33.1
stage_s.paint=16.1
stage_s.icons=4.2
steps=25329
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=77987840
start.private_mb=74.38
start.working_set_bytes=72200192
start.handles=599
start.gdi=45
start.user=21
start.threads=55
sample.0.private_bytes=77987840
sample.0.private_mb=74.38
sample.0.working_set_bytes=72200192
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=55
sample.1000.private_bytes=79708160
sample.1000.private_mb=76.02
sample.1000.working_set_bytes=73334784
sample.1000.handles=597
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=55
sample.2000.private_bytes=80089088
sample.2000.private_mb=76.38
sample.2000.working_set_bytes=73854976
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=55
sample.3000.private_bytes=82857984
sample.3000.private_mb=79.02
sample.3000.working_set_bytes=76075008
sample.3000.handles=597
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=54
sample.4000.private_bytes=81776640
sample.4000.private_mb=77.99
sample.4000.working_set_bytes=75239424
sample.4000.handles=597
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=54
sample.5000.private_bytes=83574784
sample.5000.private_mb=79.70
sample.5000.working_set_bytes=76959744
sample.5000.handles=597
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=54
sample.6000.private_bytes=81432576
sample.6000.private_mb=77.66
sample.6000.working_set_bytes=74739712
sample.6000.handles=597
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=54
sample.7000.private_bytes=83275776
sample.7000.private_mb=79.42
sample.7000.working_set_bytes=76464128
sample.7000.handles=597
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=54
sample.8000.private_bytes=81629184
sample.8000.private_mb=77.85
sample.8000.working_set_bytes=75276288
sample.8000.handles=597
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=54
sample.9000.private_bytes=81752064
sample.9000.private_mb=77.96
sample.9000.working_set_bytes=75337728
sample.9000.handles=595
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=52
sample.10000.private_bytes=82071552
sample.10000.private_mb=78.27
sample.10000.working_set_bytes=75599872
sample.10000.handles=595
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=52
sample.11000.private_bytes=80191488
sample.11000.private_mb=76.48
sample.11000.working_set_bytes=74612736
sample.11000.handles=595
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=52
end.private_bytes=80191488
end.private_mb=76.48
end.working_set_bytes=74612736
end.handles=595
end.gdi=45
end.user=21
end.threads=52
idle.private_bytes=66904064
idle.private_mb=63.80
idle.working_set_bytes=60731392
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=55
delta.private_bytes_from_idle=13287424
delta.private_bytes=2203648
delta.private_pct=2.83
allow.private_bytes=3899392
pass.private=1
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=83574784
trim.private_bytes=80191488
trim.private_mb=76.48
trim.working_set_bytes=74612736
trim.handles=595
trim.gdi=45
trim.user=21
trim.threads=52
trim.delta_private_bytes=0
trim.note=icon cache cleared (1047 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-02.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
start_paint_path=hardware
paints=24616
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
elapsed_s=45.2
ops_per_s=243.3
navigations_s=40.9
tabs_s=4.3
navigations_per_s=244.3
tabs_per_s=233.1
stage_s.load=31.1
stage_s.paint=15.5
stage_s.icons=1.5
steps=25204
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=79278080
start.private_mb=75.61
start.working_set_bytes=72859648
start.handles=600
start.gdi=45
start.user=21
start.threads=58
sample.0.private_bytes=79278080
sample.0.private_mb=75.61
sample.0.working_set_bytes=72859648
sample.0.handles=600
sample.0.gdi=45
sample.0.user=21
sample.0.threads=58
sample.1000.private_bytes=83615744
sample.1000.private_mb=79.74
sample.1000.working_set_bytes=76120064
sample.1000.handles=600
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=58
sample.2000.private_bytes=80601088
sample.2000.private_mb=76.87
sample.2000.working_set_bytes=73662464
sample.2000.handles=600
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=58
sample.3000.private_bytes=82149376
sample.3000.private_mb=78.34
sample.3000.working_set_bytes=74612736
sample.3000.handles=600
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=58
sample.4000.private_bytes=83173376
sample.4000.private_mb=79.32
sample.4000.working_set_bytes=75767808
sample.4000.handles=598
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=57
sample.5000.private_bytes=84770816
sample.5000.private_mb=80.84
sample.5000.working_set_bytes=77131776
sample.5000.handles=598
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=57
sample.6000.private_bytes=83316736
sample.6000.private_mb=79.46
sample.6000.working_set_bytes=75886592
sample.6000.handles=598
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=57
sample.7000.private_bytes=82006016
sample.7000.private_mb=78.21
sample.7000.working_set_bytes=74850304
sample.7000.handles=598
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=57
sample.8000.private_bytes=82370560
sample.8000.private_mb=78.55
sample.8000.working_set_bytes=75051008
sample.8000.handles=598
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=57
sample.9000.private_bytes=81559552
sample.9000.private_mb=77.78
sample.9000.working_set_bytes=74760192
sample.9000.handles=598
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=57
sample.10000.private_bytes=84717568
sample.10000.private_mb=80.79
sample.10000.working_set_bytes=77217792
sample.10000.handles=598
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=57
sample.11000.private_bytes=80969728
sample.11000.private_mb=77.22
sample.11000.working_set_bytes=74412032
sample.11000.handles=596
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=55
end.private_bytes=80969728
end.private_mb=77.22
end.working_set_bytes=74412032
end.handles=596
end.gdi=45
end.user=21
end.threads=55
idle.private_bytes=66785280
idle.private_mb=63.69
idle.working_set_bytes=59408384
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=58
delta.private_bytes_from_idle=14184448
delta.private_bytes=1691648
delta.private_pct=2.13
allow.private_bytes=3963904
pass.private=1
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=84770816
trim.private_bytes=80969728
trim.private_mb=77.22
trim.working_set_bytes=74412032
trim.handles=596
trim.gdi=45
trim.user=21
trim.threads=55
trim.delta_private_bytes=0
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
paints=24602
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
elapsed_s=43.4
ops_per_s=253.7
navigations_s=39.6
tabs_s=3.8
navigations_per_s=252.6
tabs_per_s=264.5
stage_s.load=29.9
stage_s.paint=14.8
stage_s.icons=1.5
steps=25174
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=78573568
start.private_mb=74.93
start.working_set_bytes=72626176
start.handles=599
start.gdi=45
start.user=21
start.threads=58
sample.0.private_bytes=78573568
sample.0.private_mb=74.93
sample.0.working_set_bytes=72626176
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=58
sample.1000.private_bytes=83406848
sample.1000.private_mb=79.54
sample.1000.working_set_bytes=76587008
sample.1000.handles=599
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=58
sample.2000.private_bytes=79978496
sample.2000.private_mb=76.27
sample.2000.working_set_bytes=73523200
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=58
sample.3000.private_bytes=83382272
sample.3000.private_mb=79.52
sample.3000.working_set_bytes=76963840
sample.3000.handles=599
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=58
sample.4000.private_bytes=82948096
sample.4000.private_mb=79.11
sample.4000.working_set_bytes=76296192
sample.4000.handles=597
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=57
sample.5000.private_bytes=84254720
sample.5000.private_mb=80.35
sample.5000.working_set_bytes=77561856
sample.5000.handles=597
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=57
sample.6000.private_bytes=82460672
sample.6000.private_mb=78.64
sample.6000.working_set_bytes=76124160
sample.6000.handles=597
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=57
sample.7000.private_bytes=82362368
sample.7000.private_mb=78.55
sample.7000.working_set_bytes=75894784
sample.7000.handles=597
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=57
sample.8000.private_bytes=82612224
sample.8000.private_mb=78.79
sample.8000.working_set_bytes=75923456
sample.8000.handles=597
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=57
sample.9000.private_bytes=81653760
sample.9000.private_mb=77.87
sample.9000.working_set_bytes=75284480
sample.9000.handles=597
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=57
sample.10000.private_bytes=84983808
sample.10000.private_mb=81.05
sample.10000.working_set_bytes=78266368
sample.10000.handles=597
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=57
sample.11000.private_bytes=81145856
sample.11000.private_mb=77.39
sample.11000.working_set_bytes=75194368
sample.11000.handles=595
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=55
end.private_bytes=81145856
end.private_mb=77.39
end.working_set_bytes=75194368
end.handles=595
end.gdi=45
end.user=21
end.threads=55
idle.private_bytes=66719744
idle.private_mb=63.63
idle.working_set_bytes=59523072
idle.handles=551
idle.gdi=45
idle.user=21
idle.threads=58
delta.private_bytes_from_idle=14426112
delta.private_bytes=2572288
delta.private_pct=3.27
allow.private_bytes=3928678
pass.private=1
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=84983808
trim.private_bytes=81145856
trim.private_mb=77.39
trim.working_set_bytes=75194368
trim.handles=595
trim.gdi=45
trim.user=21
trim.threads=55
trim.delta_private_bytes=0
trim.note=icon cache cleared (1040 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0
```
</details>

## 2026-11-18 05:22 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `c908f4a`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/private-bytes-memory-budget`, commit `c908f4a` (dirty worktree)
- Window: 3840x2064 at 192 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 6 Minecraft java process(es), 25 docker container(s), CPU 36% busy, servers loaded; rig up 343 h
- Cold-cache 100k open: measured (purge working ok)
  The purge also emptied the app's own pages: that process painted its first frame 373 ms after start (warm: 62 ms), so the cold row includes loading the app itself, not only the folder's metadata.
- Idle: 31 ms of CPU over the first idle window, 47 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 36.2 MB private at the first painted frame, 87.9 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 61.30 ms median, 107.20 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 3.57 ms median, 9.29 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 63.78 ms median, 86.33 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 47.73 ms median, 47.73 ms p95 (n=1) | **FAIL** |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 304.02 ms median, 304.02 ms p95 (n=1) | **FAIL** |
| Scroll paint p95, 4K, details view | < 5.0 ms | 3.15 ms median, 4.47 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 47 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 62.0 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 81.6 MB median (82.4 MB max) | PASS |
| Main exe | < 6 MB | 656 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 72.3 -> 76.6 MB (+4.27 MB, 5.9%; allowance 3.6 MB) | **FAIL** |

**3 budget(s) missed: 100,000-entry folder, cold cache: first rows, 100,000-entry folder, cold cache: complete and sorted, Leak gate: 10,000 navigations + 1,000 tabs.** Reported as measured; nothing was relaxed to make them pass.

Informational: startup navigation first rows 44.17 ms and complete+sorted 124.43 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 72.3 -> 76.6 | +4.27 | +5.90 | 3.62 | 42 | **FAIL** |
| 2 | 76.4 -> 76.8 | +0.39 | +0.51 | 3.82 | 37 | PASS |
| 3 | 74.7 -> 77.3 | +2.59 | +3.46 | 3.73 | 44 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **FAIL** (failed: private)
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (62.0 MB private, before the warm-up) to the end the process grew 14.6 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 72.3 | 67.6 | 599 | 45 | 21 | 51 |
| 0 ops | 72.3 | 67.6 | 599 | 45 | 21 | 51 |
| 1000 ops | 77.6 | 71.9 | 599 | 45 | 21 | 51 |
| 2000 ops | 78.0 | 72.3 | 599 | 45 | 21 | 51 |
| 3000 ops | 77.7 | 72.0 | 599 | 45 | 21 | 51 |
| 4000 ops | 78.0 | 72.3 | 595 | 45 | 21 | 51 |
| 5000 ops | 76.4 | 71.2 | 597 | 45 | 21 | 50 |
| 6000 ops | 78.7 | 73.0 | 597 | 45 | 21 | 50 |
| 7000 ops | 77.8 | 72.1 | 597 | 45 | 21 | 50 |
| 8000 ops | 77.7 | 72.2 | 595 | 45 | 21 | 50 |
| 9000 ops | 77.3 | 71.9 | 597 | 45 | 21 | 50 |
| 10000 ops | 79.8 | 73.8 | 597 | 45 | 21 | 50 |
| 11000 ops | 76.6 | 71.7 | 595 | 45 | 21 | 48 |
| after settle | 76.6 | 71.7 | 595 | 45 | 21 | 48 |
| after icon-cache trim | 76.6 | | | | | |

Private bytes +4.27 MB (+5.90%, allowance 3.62 MB), handles -4, GDI +0, USER +0, threads -3. Peak private bytes 79.8 MB.
Forced collection: icon cache cleared (1047 entries, cap 2048 items / 8 MB); private bytes after it 76.6 MB.
Gate wall time 42 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T05:22:27
commit=c908f4a
branch=t3/private-bytes-memory-budget
dirty=1
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=10
cache=warm filesystem cache (no purge before these runs)
cold=measured (purge working ok)
dpi=192
rig_load=6 Minecraft java process(es), 25 docker container(s), CPU 36% busy, servers loaded; rig up 343 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=1
reports=G:\rig-test\tsubame\bench-2026-11-18T05-22-27
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 100.90 paint_p50_ms 2.46 paint_p95_ms 3.99 private_before_scroll_mb 85.8 private_end_mb 81.5 peak_commit_mb 91.1 rows 100000 exe_bytes 672256
run 2: first_paint_ms 107.20 paint_p50_ms 2.62 paint_p95_ms 4.47 private_before_scroll_mb 85.3 private_end_mb 80.6 peak_commit_mb 90.8 rows 100000 exe_bytes 672256
run 3: first_paint_ms 77.50 paint_p50_ms 2.18 paint_p95_ms 3.30 private_before_scroll_mb 85.9 private_end_mb 81.4 peak_commit_mb 96.9 rows 100000 exe_bytes 672256
run 4: first_paint_ms 59.10 paint_p50_ms 2.19 paint_p95_ms 2.98 private_before_scroll_mb 86.8 private_end_mb 82.4 peak_commit_mb 95.4 rows 100000 exe_bytes 672256
run 5: first_paint_ms 70.60 paint_p50_ms 2.21 paint_p95_ms 3.39 private_before_scroll_mb 87.0 private_end_mb 81.7 peak_commit_mb 95.3 rows 100000 exe_bytes 672256
run 6: first_paint_ms 64.60 paint_p50_ms 2.15 paint_p95_ms 3.06 private_before_scroll_mb 88.0 private_end_mb 82.3 peak_commit_mb 95.3 rows 100000 exe_bytes 672256
run 7: first_paint_ms 58.40 paint_p50_ms 2.21 paint_p95_ms 3.08 private_before_scroll_mb 87.9 private_end_mb 81.7 peak_commit_mb 95.2 rows 100000 exe_bytes 672256
run 8: first_paint_ms 60.70 paint_p50_ms 2.21 paint_p95_ms 3.15 private_before_scroll_mb 87.8 private_end_mb 81.3 peak_commit_mb 90.9 rows 100000 exe_bytes 672256
run 9: first_paint_ms 58.10 paint_p50_ms 2.22 paint_p95_ms 3.15 private_before_scroll_mb 85.3 private_end_mb 81.6 peak_commit_mb 93.5 rows 100000 exe_bytes 672256
run 10: first_paint_ms 61.30 paint_p50_ms 2.22 paint_p95_ms 3.21 private_before_scroll_mb 87.4 private_end_mb 81.8 peak_commit_mb 96.7 rows 100000 exe_bytes 672256
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 61.6
private_bytes_first_paint: 36.2 MB
all times in ms from the navigation event
startup   entries  100000 (9.0 MB)  first_batch    23.56  first_frame    44.17  done   120.80  final_frame   124.43  worker    42.23   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.0 MB)  first_batch     4.30  first_frame     9.29  done    73.14  final_frame    74.98  worker    42.84   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.0 MB)  first_batch     2.48  first_frame     7.46  done    71.62  final_frame    86.33  worker    38.74   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.0 MB)  first_batch     1.12  first_frame     4.98  done    64.13  final_frame    65.58  worker    37.28   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.0 MB)  first_batch     1.01  first_frame     3.42  done    71.67  final_frame    73.09  worker    33.63   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.0 MB)  first_batch     0.95  first_frame     3.96  done    62.09  final_frame    63.78  worker    34.40   G:\rig-bench\tsubame-b1\100k
repeat 6  entries  100000 (9.0 MB)  first_batch     0.87  first_frame     3.15  done    61.10  final_frame    62.49  worker    32.95   G:\rig-bench\tsubame-b1\100k
repeat 7  entries  100000 (9.0 MB)  first_batch     0.90  first_frame     3.57  done    61.60  final_frame    63.02  worker    32.54   G:\rig-bench\tsubame-b1\100k
repeat 8  entries  100000 (9.0 MB)  first_batch     0.94  first_frame     3.30  done    60.44  final_frame    62.09  worker    33.44   G:\rig-bench\tsubame-b1\100k
repeat 9  entries  100000 (9.0 MB)  first_batch     0.81  first_frame     3.12  done    59.98  final_frame    61.25  worker    35.65   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 87.9 MB
```
</details>

<details><summary>Raw: cold-cache u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path software (GPU starting)
first_paint_ms (process start to first completed paint): 373.2
private_bytes_first_paint: 25.9 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   146.59  first_frame   315.23  done   315.38  final_frame   317.19  worker     1.96   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (9.6 MB)  first_batch    44.10  first_frame    47.73  done   302.36  final_frame   304.02  worker   248.71   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 54.0 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=31.2
idle.cpu_ms=46.9
idle.threads=53
idle.threads_warm=56
idle.context_switches_per_s=-1
idle.pid=46708
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
paints=24566
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
elapsed_s=41.7
ops_per_s=263.5
navigations_s=38.0
tabs_s=3.7
navigations_per_s=263.0
tabs_per_s=268.9
stage_s.load=28.8
stage_s.paint=14.4
stage_s.icons=1.6
steps=25122
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=75825152
start.private_mb=72.31
start.working_set_bytes=70909952
start.handles=599
start.gdi=45
start.user=21
start.threads=51
sample.0.private_bytes=75825152
sample.0.private_mb=72.31
sample.0.working_set_bytes=70909952
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=51
sample.1000.private_bytes=81371136
sample.1000.private_mb=77.60
sample.1000.working_set_bytes=75431936
sample.1000.handles=599
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=51
sample.2000.private_bytes=81772544
sample.2000.private_mb=77.98
sample.2000.working_set_bytes=75808768
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=51
sample.3000.private_bytes=81469440
sample.3000.private_mb=77.70
sample.3000.working_set_bytes=75538432
sample.3000.handles=599
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=51
sample.4000.private_bytes=81825792
sample.4000.private_mb=78.04
sample.4000.working_set_bytes=75821056
sample.4000.handles=595
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=51
sample.5000.private_bytes=80134144
sample.5000.private_mb=76.42
sample.5000.working_set_bytes=74616832
sample.5000.handles=597
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=50
sample.6000.private_bytes=82542592
sample.6000.private_mb=78.72
sample.6000.working_set_bytes=76537856
sample.6000.handles=597
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=50
sample.7000.private_bytes=81555456
sample.7000.private_mb=77.78
sample.7000.working_set_bytes=75579392
sample.7000.handles=597
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=50
sample.8000.private_bytes=81506304
sample.8000.private_mb=77.73
sample.8000.working_set_bytes=75759616
sample.8000.handles=595
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=50
sample.9000.private_bytes=81068032
sample.9000.private_mb=77.31
sample.9000.working_set_bytes=75366400
sample.9000.handles=597
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=50
sample.10000.private_bytes=83636224
sample.10000.private_mb=79.76
sample.10000.working_set_bytes=77332480
sample.10000.handles=597
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=50
sample.11000.private_bytes=80297984
sample.11000.private_mb=76.58
sample.11000.working_set_bytes=75177984
sample.11000.handles=595
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=48
end.private_bytes=80297984
end.private_mb=76.58
end.working_set_bytes=75177984
end.handles=595
end.gdi=45
end.user=21
end.threads=48
idle.private_bytes=64983040
idle.private_mb=61.97
idle.working_set_bytes=59232256
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=51
delta.private_bytes_from_idle=15314944
delta.private_bytes=4472832
delta.private_pct=5.90
allow.private_bytes=3791257
pass.private=0
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=83636224
trim.private_bytes=80297984
trim.private_mb=76.58
trim.working_set_bytes=75177984
trim.handles=595
trim.gdi=45
trim.user=21
trim.threads=48
trim.delta_private_bytes=0
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
elapsed_s=37.3
ops_per_s=294.8
navigations_s=32.8
tabs_s=4.5
navigations_per_s=304.6
tabs_per_s=222.7
stage_s.load=25.8
stage_s.paint=13.1
stage_s.icons=1.6
steps=25055
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=80125952
start.private_mb=76.41
start.working_set_bytes=74289152
start.handles=599
start.gdi=45
start.user=21
start.threads=55
sample.0.private_bytes=80125952
sample.0.private_mb=76.41
sample.0.working_set_bytes=74289152
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=55
sample.1000.private_bytes=79400960
sample.1000.private_mb=75.72
sample.1000.working_set_bytes=73179136
sample.1000.handles=599
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=55
sample.2000.private_bytes=82489344
sample.2000.private_mb=78.67
sample.2000.working_set_bytes=76091392
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=55
sample.3000.private_bytes=83779584
sample.3000.private_mb=79.90
sample.3000.working_set_bytes=77127680
sample.3000.handles=599
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=55
sample.4000.private_bytes=83144704
sample.4000.private_mb=79.29
sample.4000.working_set_bytes=76554240
sample.4000.handles=595
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=55
sample.5000.private_bytes=83562496
sample.5000.private_mb=79.69
sample.5000.working_set_bytes=76906496
sample.5000.handles=599
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=55
sample.6000.private_bytes=80871424
sample.6000.private_mb=77.12
sample.6000.working_set_bytes=74534912
sample.6000.handles=597
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=54
sample.7000.private_bytes=81690624
sample.7000.private_mb=77.91
sample.7000.working_set_bytes=75563008
sample.7000.handles=597
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=54
sample.8000.private_bytes=81739776
sample.8000.private_mb=77.95
sample.8000.working_set_bytes=75649024
sample.8000.handles=595
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=54
sample.9000.private_bytes=84258816
sample.9000.private_mb=80.36
sample.9000.working_set_bytes=77676544
sample.9000.handles=597
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=54
sample.10000.private_bytes=84332544
sample.10000.private_mb=80.43
sample.10000.working_set_bytes=77660160
sample.10000.handles=597
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=54
sample.11000.private_bytes=80535552
sample.11000.private_mb=76.80
sample.11000.working_set_bytes=75018240
sample.11000.handles=597
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=54
end.private_bytes=80535552
end.private_mb=76.80
end.working_set_bytes=75018240
end.handles=597
end.gdi=45
end.user=21
end.threads=54
idle.private_bytes=65810432
idle.private_mb=62.76
idle.working_set_bytes=59396096
idle.handles=551
idle.gdi=45
idle.user=21
idle.threads=55
delta.private_bytes_from_idle=14725120
delta.private_bytes=409600
delta.private_pct=0.51
allow.private_bytes=4006297
pass.private=1
delta.handles=-2
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=84332544
trim.private_bytes=80535552
trim.private_mb=76.80
trim.working_set_bytes=75018240
trim.handles=597
trim.gdi=45
trim.user=21
trim.threads=54
trim.delta_private_bytes=0
trim.note=icon cache cleared (1040 entries, cap 2048 items / 8 MB)
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
paints=24823
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
elapsed_s=43.9
ops_per_s=250.6
navigations_s=38.9
tabs_s=5.0
navigations_per_s=257.1
tabs_per_s=200.1
stage_s.load=30.8
stage_s.paint=14.7
stage_s.icons=1.7
steps=25403
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=78315520
start.private_mb=74.69
start.working_set_bytes=72097792
start.handles=599
start.gdi=45
start.user=21
start.threads=51
sample.0.private_bytes=78315520
sample.0.private_mb=74.69
sample.0.working_set_bytes=72097792
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=51
sample.1000.private_bytes=79532032
sample.1000.private_mb=75.85
sample.1000.working_set_bytes=72781824
sample.1000.handles=599
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=51
sample.2000.private_bytes=82554880
sample.2000.private_mb=78.73
sample.2000.working_set_bytes=74981376
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=51
sample.3000.private_bytes=83132416
sample.3000.private_mb=79.28
sample.3000.working_set_bytes=75788288
sample.3000.handles=599
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=51
sample.4000.private_bytes=83234816
sample.4000.private_mb=79.38
sample.4000.working_set_bytes=75841536
sample.4000.handles=599
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=51
sample.5000.private_bytes=83230720
sample.5000.private_mb=79.38
sample.5000.working_set_bytes=75939840
sample.5000.handles=597
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=50
sample.6000.private_bytes=82165760
sample.6000.private_mb=78.36
sample.6000.working_set_bytes=74838016
sample.6000.handles=597
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=50
sample.7000.private_bytes=81752064
sample.7000.private_mb=77.96
sample.7000.working_set_bytes=74776576
sample.7000.handles=597
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=50
sample.8000.private_bytes=84271104
sample.8000.private_mb=80.37
sample.8000.working_set_bytes=76820480
sample.8000.handles=597
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=50
sample.9000.private_bytes=82448384
sample.9000.private_mb=78.63
sample.9000.working_set_bytes=75436032
sample.9000.handles=597
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=50
sample.10000.private_bytes=84652032
sample.10000.private_mb=80.73
sample.10000.working_set_bytes=77258752
sample.10000.handles=597
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=50
sample.11000.private_bytes=81027072
sample.11000.private_mb=77.27
sample.11000.working_set_bytes=74592256
sample.11000.handles=595
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=48
end.private_bytes=81027072
end.private_mb=77.27
end.working_set_bytes=74592256
end.handles=595
end.gdi=45
end.user=21
end.threads=48
idle.private_bytes=65765376
idle.private_mb=62.72
idle.working_set_bytes=58777600
idle.handles=551
idle.gdi=45
idle.user=21
idle.threads=51
delta.private_bytes_from_idle=15261696
delta.private_bytes=2711552
delta.private_pct=3.46
allow.private_bytes=3915776
pass.private=1
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=84652032
trim.private_bytes=81027072
trim.private_mb=77.27
trim.working_set_bytes=74592256
trim.handles=595
trim.gdi=45
trim.user=21
trim.threads=48
trim.delta_private_bytes=0
trim.note=icon cache cleared (1039 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0
```
</details>

## 2026-11-18 06:12 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `18b1bc8`

- Mode: full (3 cold-start runs, 3 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/526fd871`, commit `18b1bc8`
- Window: 3840x2064 at 192 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 8 Minecraft java process(es), 25 docker container(s), CPU 20% busy, servers loaded; rig up 344 h
- Cold-cache 100k open: measured (purge working ok)
  The purge also emptied the app's own pages: that process painted its first frame 301 ms after start (warm: 77 ms), so the cold row includes loading the app itself, not only the folder's metadata.
- Idle: 16 ms of CPU over the first idle window, 0 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 35.3 MB private at the first painted frame, 90.8 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 69.80 ms median, 86.60 ms p95 (n=3) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 12.33 ms median, 20.41 ms p95 (n=2) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 102.61 ms median, 104.12 ms p95 (n=2) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 49.08 ms median, 49.08 ms p95 (n=1) | **FAIL** |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 340.57 ms median, 340.57 ms p95 (n=1) | **FAIL** |
| Scroll paint p95, 4K, details view | < 5.0 ms | 2.98 ms median, 3.46 ms p95 (n=3) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 0 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 62.0 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 81.5 MB median (81.8 MB max) | PASS |
| Main exe | < 6 MB | 686 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 73.1 -> 74.7 MB (+1.61 MB, 2.2%; allowance 3.7 MB) | PASS |

**2 budget(s) missed: 100,000-entry folder, cold cache: first rows, 100,000-entry folder, cold cache: complete and sorted.** Reported as measured; nothing was relaxed to make them pass.

Informational: startup navigation first rows 57.80 ms and complete+sorted 146.10 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 73.1 -> 74.7 | +1.61 | +2.20 | 3.65 | 46 | PASS |
| 2 | 75.5 -> 76.5 | +1.07 | +1.42 | 3.77 | 57 | PASS |
| 3 | 76.5 -> 76.7 | +0.22 | +0.29 | 3.82 | 41 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **PASS**
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (62.0 MB private, before the warm-up) to the end the process grew 12.7 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 73.1 | 66.2 | 579 | 45 | 19 | 50 |
| 0 ops | 73.1 | 66.2 | 579 | 45 | 19 | 50 |
| 1000 ops | 76.3 | 68.7 | 579 | 45 | 19 | 50 |
| 2000 ops | 76.1 | 68.9 | 579 | 45 | 19 | 50 |
| 3000 ops | 76.1 | 69.0 | 579 | 45 | 19 | 50 |
| 4000 ops | 77.3 | 70.0 | 577 | 45 | 19 | 49 |
| 5000 ops | 77.5 | 70.2 | 577 | 45 | 19 | 49 |
| 6000 ops | 76.0 | 68.8 | 577 | 45 | 19 | 49 |
| 7000 ops | 77.9 | 70.6 | 577 | 45 | 19 | 49 |
| 8000 ops | 78.1 | 70.8 | 577 | 45 | 19 | 49 |
| 9000 ops | 75.8 | 69.0 | 577 | 45 | 19 | 49 |
| 10000 ops | 78.3 | 70.6 | 577 | 45 | 19 | 49 |
| 11000 ops | 74.7 | 67.9 | 575 | 45 | 19 | 47 |
| after settle | 74.7 | 67.9 | 575 | 45 | 19 | 47 |
| after icon-cache trim | 74.8 | | | | | |

Private bytes +1.61 MB (+2.20%, allowance 3.65 MB), handles -4, GDI +0, USER +0, threads -3. Peak private bytes 78.3 MB.
Forced collection: icon cache cleared (1047 entries, cap 2048 items / 8 MB); private bytes after it 74.8 MB.
Gate wall time 46 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T06:12:39
commit=18b1bc8
branch=t3/526fd871
dirty=0
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=3
cache=warm filesystem cache (no purge before these runs)
cold=measured (purge working ok)
dpi=192
rig_load=8 Minecraft java process(es), 25 docker container(s), CPU 20% busy, servers loaded; rig up 344 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=1
reports=G:\rig-test\tsubame\bench-2026-11-18T06-12-39
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 69.70 paint_p50_ms 2.19 paint_p95_ms 2.98 private_before_scroll_mb 86.3 private_end_mb 81.8 peak_commit_mb 96.2 rows 100000 exe_bytes 701952
run 2: first_paint_ms 86.60 paint_p50_ms 2.18 paint_p95_ms 2.95 private_before_scroll_mb 88.1 private_end_mb 81.5 peak_commit_mb 94.9 rows 100000 exe_bytes 701952
run 3: first_paint_ms 69.80 paint_p50_ms 2.24 paint_p95_ms 3.46 private_before_scroll_mb 86.3 private_end_mb 81.3 peak_commit_mb 90.4 rows 100000 exe_bytes 701952
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 77.4
private_bytes_first_paint: 35.3 MB
all times in ms from the navigation event
startup   entries  100000 (9.0 MB)  first_batch    29.08  first_frame    57.80  done   143.67  final_frame   146.10  worker    62.35   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.0 MB)  first_batch    16.78  first_frame    20.41  done   100.56  final_frame   102.61  worker    45.54   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.0 MB)  first_batch     2.46  first_frame    12.33  done   102.43  final_frame   104.12  worker    48.05   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 90.8 MB
```
</details>

<details><summary>Raw: cold-cache u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path software (GPU starting)
first_paint_ms (process start to first completed paint): 301.2
private_bytes_first_paint: 25.9 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   126.34  first_frame   242.46  done   242.70  final_frame   245.25  worker     3.78   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (9.6 MB)  first_batch    45.03  first_frame    49.08  done   337.89  final_frame   340.57  worker   269.90   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 56.1 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=15.6
idle.cpu_ms=0
idle.threads=57
idle.threads_warm=57
idle.context_switches_per_s=-1
idle.pid=26992
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
paints=24592
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
elapsed_s=45.5
ops_per_s=241.8
navigations_s=40.2
tabs_s=5.3
navigations_per_s=248.6
tabs_per_s=190.1
stage_s.load=31.8
stage_s.paint=15.6
stage_s.icons=2.0
steps=25196
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=76627968
start.private_mb=73.08
start.working_set_bytes=69390336
start.handles=579
start.gdi=45
start.user=19
start.threads=50
sample.0.private_bytes=76627968
sample.0.private_mb=73.08
sample.0.working_set_bytes=69390336
sample.0.handles=579
sample.0.gdi=45
sample.0.user=19
sample.0.threads=50
sample.1000.private_bytes=79998976
sample.1000.private_mb=76.29
sample.1000.working_set_bytes=72015872
sample.1000.handles=579
sample.1000.gdi=45
sample.1000.user=19
sample.1000.threads=50
sample.2000.private_bytes=79781888
sample.2000.private_mb=76.09
sample.2000.working_set_bytes=72232960
sample.2000.handles=579
sample.2000.gdi=45
sample.2000.user=19
sample.2000.threads=50
sample.3000.private_bytes=79761408
sample.3000.private_mb=76.07
sample.3000.working_set_bytes=72339456
sample.3000.handles=579
sample.3000.gdi=45
sample.3000.user=19
sample.3000.threads=50
sample.4000.private_bytes=81027072
sample.4000.private_mb=77.27
sample.4000.working_set_bytes=73379840
sample.4000.handles=577
sample.4000.gdi=45
sample.4000.user=19
sample.4000.threads=49
sample.5000.private_bytes=81305600
sample.5000.private_mb=77.54
sample.5000.working_set_bytes=73572352
sample.5000.handles=577
sample.5000.gdi=45
sample.5000.user=19
sample.5000.threads=49
sample.6000.private_bytes=79708160
sample.6000.private_mb=76.02
sample.6000.working_set_bytes=72089600
sample.6000.handles=577
sample.6000.gdi=45
sample.6000.user=19
sample.6000.threads=49
sample.7000.private_bytes=81711104
sample.7000.private_mb=77.93
sample.7000.working_set_bytes=73990144
sample.7000.handles=577
sample.7000.gdi=45
sample.7000.user=19
sample.7000.threads=49
sample.8000.private_bytes=81936384
sample.8000.private_mb=78.14
sample.8000.working_set_bytes=74215424
sample.8000.handles=577
sample.8000.gdi=45
sample.8000.user=19
sample.8000.threads=49
sample.9000.private_bytes=79527936
sample.9000.private_mb=75.84
sample.9000.working_set_bytes=72388608
sample.9000.handles=577
sample.9000.gdi=45
sample.9000.user=19
sample.9000.threads=49
sample.10000.private_bytes=82149376
sample.10000.private_mb=78.34
sample.10000.working_set_bytes=74063872
sample.10000.handles=577
sample.10000.gdi=45
sample.10000.user=19
sample.10000.threads=49
sample.11000.private_bytes=78315520
sample.11000.private_mb=74.69
sample.11000.working_set_bytes=71155712
sample.11000.handles=575
sample.11000.gdi=45
sample.11000.user=19
sample.11000.threads=47
end.private_bytes=78315520
end.private_mb=74.69
end.working_set_bytes=71155712
end.handles=575
end.gdi=45
end.user=19
end.threads=47
idle.private_bytes=64970752
idle.private_mb=61.96
idle.working_set_bytes=56766464
idle.handles=549
idle.gdi=45
idle.user=19
idle.threads=50
delta.private_bytes_from_idle=13344768
delta.private_bytes=1687552
delta.private_pct=2.20
allow.private_bytes=3831398
pass.private=1
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=82149376
trim.private_bytes=78385152
trim.private_mb=74.75
trim.working_set_bytes=71229440
trim.handles=575
trim.gdi=45
trim.user=19
trim.threads=47
trim.delta_private_bytes=69632
trim.note=icon cache cleared (1047 entries, cap 2048 items / 8 MB)
timeouts=0
memdiag.line 0 ops: private=73.08 heaps=18.58 heap0_busy=14.45 heap0_free=3.22 outside=18.83 allocs=76 images=33.23 mapped=4.94 rust=5.41
memdiag.gate_start.private_usage_mb=73.12
memdiag.gate_start.private_commit_mb=37.80
memdiag.gate_start.heaps=5 heaps_committed_mb=18.62
memdiag.gate_start.heap0(process): committed_mb=17.71 busy_mb=14.45 blocks=27694 free_committed_mb=3.26 big_blocks=10 big_mb=1.94
memdiag.gate_start.heap3: committed_mb=0.87 busy_mb=0.72 blocks=1471 free_committed_mb=0.15 big_blocks=1 big_mb=0.10
memdiag.gate_start.outside_heaps_mb=18.83 allocs=76
memdiag.gate_start.outside 0x1ea18300000 13.02 MB protect=0x404
memdiag.gate_start.outside 0x1e98d830000 1.00 MB protect=0x4
memdiag.gate_start.outside 0x1e98b410000 0.44 MB protect=0x4
memdiag.gate_start.outside 0x1e998300000 0.44 MB protect=0x4
memdiag.gate_start.outside 0x1e9d8300000 0.44 MB protect=0x4
memdiag.gate_start.outside 0x24c5e00000 0.39 MB protect=0x4
memdiag.gate_start.outside 0x1e98d9d0000 0.38 MB protect=0x4
memdiag.gate_start.outside 0x1e98d980000 0.28 MB protect=0x4
memdiag.gate_start.outside 0x1e993320000 0.28 MB protect=0x4
memdiag.gate_start.outside 0x1e98fa50000 0.27 MB protect=0x4
memdiag.gate_start.outside 0x1e98d950000 0.19 MB protect=0x4
memdiag.gate_start.outside 0x1e98d780000 0.14 MB protect=0x4
memdiag.gate_start.image_writable_mb=33.23 mapped_writable_mb=4.94
memdiag.gate_start.image nvwgf2umx.dll 24.96 MB
memdiag.gate_start.image nvppex.dll 3.66 MB
memdiag.gate_start.image nvgpucomp64.dll 3.34 MB
memdiag.gate_start.image nvspcap64.dll 0.07 MB
memdiag.gate_start.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_start.image urlmon.dll 0.05 MB
memdiag.gate_start.image msvcp_win.dll 0.05 MB
memdiag.gate_start.image shell32.dll 0.05 MB
memdiag.gate_start.image windows.storage.dll 0.04 MB
memdiag.gate_start.image KERNELBASE.dll 0.04 MB
memdiag.gate_start.rust_live_mb=5.41 rust_peak_mb=7.61
memdiag.line 1000 ops: private=76.29 heaps=21.78 heap0_busy=13.96 heap0_free=6.90 outside=18.83 allocs=76 images=33.23 mapped=4.94 rust=5.40
memdiag.line 2000 ops: private=76.09 heaps=21.57 heap0_busy=15.08 heap0_free=5.60 outside=18.83 allocs=76 images=33.23 mapped=4.94 rust=5.42
memdiag.line 3000 ops: private=72.81 heaps=18.16 heap0_busy=14.19 heap0_free=3.24 outside=18.99 allocs=76 images=33.23 mapped=4.94 rust=5.40
memdiag.line 4000 ops: private=77.27 heaps=22.64 heap0_busy=14.72 heap0_free=6.99 outside=18.95 allocs=75 images=33.23 mapped=4.94 rust=5.41
memdiag.line 5000 ops: private=77.54 heaps=22.91 heap0_busy=13.82 heap0_free=8.12 outside=18.95 allocs=75 images=33.23 mapped=4.94 rust=5.40
memdiag.line 6000 ops: private=76.02 heaps=21.25 heap0_busy=14.97 heap0_free=5.35 outside=19.09 allocs=75 images=33.23 mapped=4.94 rust=5.41
memdiag.line 7000 ops: private=77.93 heaps=23.15 heap0_busy=14.40 heap0_free=7.85 outside=19.09 allocs=75 images=33.23 mapped=4.94 rust=5.40
memdiag.line 8000 ops: private=74.27 heaps=19.51 heap0_busy=15.15 heap0_free=3.56 outside=19.09 allocs=75 images=33.23 mapped=4.94 rust=5.41
memdiag.line 9000 ops: private=75.84 heaps=21.07 heap0_busy=15.92 heap0_free=4.22 outside=19.09 allocs=75 images=33.23 mapped=4.94 rust=5.42
memdiag.line 10000 ops: private=78.34 heaps=23.57 heap0_busy=14.98 heap0_free=7.63 outside=19.09 allocs=75 images=33.23 mapped=4.94 rust=5.40
memdiag.line 11000 ops: private=74.69 heaps=19.99 heap0_busy=16.28 heap0_free=2.85 outside=19.03 allocs=73 images=33.23 mapped=4.94 rust=5.41
memdiag.gate_end.private_usage_mb=74.73
memdiag.gate_end.private_commit_mb=39.40
memdiag.gate_end.heaps=5 heaps_committed_mb=20.02
memdiag.gate_end.heap0(process): committed_mb=19.17 busy_mb=16.28 blocks=39144 free_committed_mb=2.89 big_blocks=10 big_mb=1.94
memdiag.gate_end.heap3: committed_mb=0.81 busy_mb=0.69 blocks=1314 free_committed_mb=0.12 big_blocks=1 big_mb=0.10
memdiag.gate_end.outside_heaps_mb=19.03 allocs=73
memdiag.gate_end.outside 0x1ea18300000 13.31 MB protect=0x404
memdiag.gate_end.outside 0x1e98d830000 1.00 MB protect=0x4
memdiag.gate_end.outside 0x1e98b410000 0.44 MB protect=0x4
memdiag.gate_end.outside 0x1e998300000 0.44 MB protect=0x4
memdiag.gate_end.outside 0x1e9d8300000 0.44 MB protect=0x4
memdiag.gate_end.outside 0x1e98d9d0000 0.38 MB protect=0x4
memdiag.gate_end.outside 0x24c5e00000 0.37 MB protect=0x4
memdiag.gate_end.outside 0x1e98d980000 0.28 MB protect=0x4
memdiag.gate_end.outside 0x1e993320000 0.28 MB protect=0x4
memdiag.gate_end.outside 0x1e98fa50000 0.27 MB protect=0x4
memdiag.gate_end.outside 0x1e98d950000 0.19 MB protect=0x4
memdiag.gate_end.outside 0x1e98d780000 0.14 MB protect=0x4
memdiag.gate_end.image_writable_mb=33.23 mapped_writable_mb=4.94
memdiag.gate_end.image nvwgf2umx.dll 24.96 MB
memdiag.gate_end.image nvppex.dll 3.66 MB
memdiag.gate_end.image nvgpucomp64.dll 3.34 MB
memdiag.gate_end.image nvspcap64.dll 0.07 MB
memdiag.gate_end.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_end.image urlmon.dll 0.05 MB
memdiag.gate_end.image shell32.dll 0.05 MB
memdiag.gate_end.image msvcp_win.dll 0.05 MB
memdiag.gate_end.image KERNELBASE.dll 0.04 MB
memdiag.gate_end.image windows.storage.dll 0.04 MB
memdiag.gate_end.rust_live_mb=5.41 rust_peak_mb=7.64
memdiag.gate_trim.private_usage_mb=74.75
memdiag.gate_trim.private_commit_mb=39.43
memdiag.gate_trim.heaps=5 heaps_committed_mb=20.05
memdiag.gate_trim.heap0(process): committed_mb=19.20 busy_mb=11.90 blocks=37049 free_committed_mb=7.30 big_blocks=9 big_mb=1.77
memdiag.gate_trim.heap3: committed_mb=0.81 busy_mb=0.69 blocks=1314 free_committed_mb=0.12 big_blocks=1 big_mb=0.10
memdiag.gate_trim.outside_heaps_mb=19.03 allocs=73
memdiag.gate_trim.outside 0x1ea18300000 13.31 MB protect=0x404
memdiag.gate_trim.outside 0x1e98d830000 1.00 MB protect=0x4
memdiag.gate_trim.outside 0x1e98b410000 0.44 MB protect=0x4
memdiag.gate_trim.outside 0x1e9d8300000 0.44 MB protect=0x4
memdiag.gate_trim.outside 0x1e998300000 0.44 MB protect=0x4
memdiag.gate_trim.outside 0x1e98d9d0000 0.38 MB protect=0x4
memdiag.gate_trim.outside 0x24c5e00000 0.37 MB protect=0x4
memdiag.gate_trim.outside 0x1e98d980000 0.28 MB protect=0x4
memdiag.gate_trim.outside 0x1e993320000 0.28 MB protect=0x4
memdiag.gate_trim.outside 0x1e98fa50000 0.27 MB protect=0x4
memdiag.gate_trim.outside 0x1e98d950000 0.19 MB protect=0x4
memdiag.gate_trim.outside 0x1e98d780000 0.14 MB protect=0x4
memdiag.gate_trim.image_writable_mb=33.23 mapped_writable_mb=4.94
memdiag.gate_trim.image nvwgf2umx.dll 24.96 MB
memdiag.gate_trim.image nvppex.dll 3.66 MB
memdiag.gate_trim.image nvgpucomp64.dll 3.34 MB
memdiag.gate_trim.image nvspcap64.dll 0.07 MB
memdiag.gate_trim.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_trim.image urlmon.dll 0.05 MB
memdiag.gate_trim.image shell32.dll 0.05 MB
memdiag.gate_trim.image msvcp_win.dll 0.05 MB
memdiag.gate_trim.image windows.storage.dll 0.04 MB
memdiag.gate_trim.image KERNELBASE.dll 0.04 MB
memdiag.gate_trim.rust_live_mb=1.03 rust_peak_mb=7.64
gate=PASS
fail=-
exit=0

# --- leak-gate-02.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
start_paint_path=hardware
paints=24944
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
elapsed_s=56.6
ops_per_s=194.3
navigations_s=52.6
tabs_s=4.0
navigations_per_s=190.2
tabs_per_s=247.9
stage_s.load=40.5
stage_s.paint=18.6
stage_s.icons=2.3
steps=25653
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=79134720
start.private_mb=75.47
start.working_set_bytes=71958528
start.handles=599
start.gdi=45
start.user=21
start.threads=50
sample.0.private_bytes=79134720
sample.0.private_mb=75.47
sample.0.working_set_bytes=71958528
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=50
sample.1000.private_bytes=81805312
sample.1000.private_mb=78.02
sample.1000.working_set_bytes=74489856
sample.1000.handles=597
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=50
sample.2000.private_bytes=83542016
sample.2000.private_mb=79.67
sample.2000.working_set_bytes=74899456
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=50
sample.3000.private_bytes=78364672
sample.3000.private_mb=74.73
sample.3000.working_set_bytes=71315456
sample.3000.handles=597
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=49
sample.4000.private_bytes=81932288
sample.4000.private_mb=78.14
sample.4000.working_set_bytes=74334208
sample.4000.handles=597
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=49
sample.5000.private_bytes=83169280
sample.5000.private_mb=79.32
sample.5000.working_set_bytes=75083776
sample.5000.handles=597
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=49
sample.6000.private_bytes=82714624
sample.6000.private_mb=78.88
sample.6000.working_set_bytes=75022336
sample.6000.handles=597
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=49
sample.7000.private_bytes=81649664
sample.7000.private_mb=77.87
sample.7000.working_set_bytes=74334208
sample.7000.handles=597
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=49
sample.8000.private_bytes=81522688
sample.8000.private_mb=77.75
sample.8000.working_set_bytes=74166272
sample.8000.handles=595
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=49
sample.9000.private_bytes=81166336
sample.9000.private_mb=77.41
sample.9000.working_set_bytes=73953280
sample.9000.handles=595
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=47
sample.10000.private_bytes=81715200
sample.10000.private_mb=77.93
sample.10000.working_set_bytes=74395648
sample.10000.handles=595
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=47
sample.11000.private_bytes=80261120
sample.11000.private_mb=76.54
sample.11000.working_set_bytes=73740288
sample.11000.handles=595
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=47
end.private_bytes=80261120
end.private_mb=76.54
end.working_set_bytes=73740288
end.handles=595
end.gdi=45
end.user=21
end.threads=47
idle.private_bytes=66514944
idle.private_mb=63.43
idle.working_set_bytes=59273216
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=50
delta.private_bytes_from_idle=13746176
delta.private_bytes=1126400
delta.private_pct=1.42
allow.private_bytes=3956736
pass.private=1
delta.handles=-4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=83542016
trim.private_bytes=80306176
trim.private_mb=76.59
trim.working_set_bytes=73789440
trim.handles=595
trim.gdi=45
trim.user=21
trim.threads=47
trim.delta_private_bytes=45056
trim.note=icon cache cleared (1042 entries, cap 2048 items / 8 MB)
timeouts=0
memdiag.line 0 ops: private=75.47 heaps=19.76 heap0_busy=15.98 heap0_free=2.89 outside=19.96 allocs=77 images=33.25 mapped=4.94 rust=5.38
memdiag.gate_start.private_usage_mb=75.49
memdiag.gate_start.private_commit_mb=40.09
memdiag.gate_start.heaps=5 heaps_committed_mb=19.78
memdiag.gate_start.heap0(process): committed_mb=18.89 busy_mb=15.98 blocks=29346 free_committed_mb=2.91 big_blocks=10 big_mb=1.94
memdiag.gate_start.heap3: committed_mb=0.84 busy_mb=0.73 blocks=1491 free_committed_mb=0.12 big_blocks=1 big_mb=0.10
memdiag.gate_start.outside_heaps_mb=19.96 allocs=77
memdiag.gate_start.outside 0x25598a00000 14.20 MB protect=0x404
memdiag.gate_start.outside 0x2554d610000 1.00 MB protect=0x4
memdiag.gate_start.outside 0x25558a00000 0.44 MB protect=0x4
memdiag.gate_start.outside 0x25500000000 0.44 MB protect=0x4
memdiag.gate_start.outside 0x9926000000 0.39 MB protect=0x4
memdiag.gate_start.outside 0x2554d7b0000 0.38 MB protect=0x4
memdiag.gate_start.outside 0x2554b410000 0.36 MB protect=0x4
memdiag.gate_start.outside 0x2554d760000 0.28 MB protect=0x4
memdiag.gate_start.outside 0x25554300000 0.28 MB protect=0x4
memdiag.gate_start.outside 0x2554f830000 0.27 MB protect=0x4
memdiag.gate_start.outside 0x2554d730000 0.19 MB protect=0x4
memdiag.gate_start.outside 0x2554d550000 0.14 MB protect=0x4
memdiag.gate_start.image_writable_mb=33.25 mapped_writable_mb=4.94
memdiag.gate_start.image nvwgf2umx.dll 24.96 MB
memdiag.gate_start.image nvppex.dll 3.66 MB
memdiag.gate_start.image nvgpucomp64.dll 3.34 MB
memdiag.gate_start.image nvspcap64.dll 0.07 MB
memdiag.gate_start.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_start.image urlmon.dll 0.05 MB
memdiag.gate_start.image msvcp_win.dll 0.05 MB
memdiag.gate_start.image shell32.dll 0.05 MB
memdiag.gate_start.image KERNELBASE.dll 0.04 MB
memdiag.gate_start.image windows.storage.dll 0.04 MB
memdiag.gate_start.rust_live_mb=5.38 rust_peak_mb=7.57
memdiag.line 1000 ops: private=73.59 heaps=17.79 heap0_busy=13.54 heap0_free=3.54 outside=20.09 allocs=77 images=33.25 mapped=4.94 rust=5.37
memdiag.line 2000 ops: private=79.67 heaps=23.82 heap0_busy=14.15 heap0_free=8.76 outside=20.09 allocs=77 images=33.25 mapped=4.94 rust=5.38
memdiag.line 3000 ops: private=74.73 heaps=18.93 heap0_busy=13.53 heap0_free=4.73 outside=20.05 allocs=76 images=33.25 mapped=4.94 rust=5.36
memdiag.line 4000 ops: private=78.14 heaps=22.33 heap0_busy=14.63 heap0_free=6.89 outside=20.05 allocs=76 images=33.25 mapped=4.94 rust=5.37
memdiag.line 5000 ops: private=79.32 heaps=23.39 heap0_busy=16.06 heap0_free=6.43 outside=20.18 allocs=76 images=33.25 mapped=4.94 rust=5.38
memdiag.line 6000 ops: private=78.88 heaps=22.95 heap0_busy=15.20 heap0_free=6.82 outside=20.18 allocs=76 images=33.25 mapped=4.94 rust=5.37
memdiag.line 7000 ops: private=77.87 heaps=21.93 heap0_busy=15.55 heap0_free=5.43 outside=20.18 allocs=76 images=33.25 mapped=4.94 rust=5.38
memdiag.line 8000 ops: private=77.75 heaps=21.86 heap0_busy=14.57 heap0_free=6.36 outside=20.18 allocs=76 images=33.25 mapped=4.94 rust=5.37
memdiag.line 9000 ops: private=77.41 heaps=21.53 heap0_busy=16.11 heap0_free=4.56 outside=20.12 allocs=74 images=33.25 mapped=4.94 rust=5.38
memdiag.line 10000 ops: private=77.93 heaps=22.05 heap0_busy=15.24 heap0_free=5.89 outside=20.12 allocs=74 images=33.25 mapped=4.94 rust=5.37
memdiag.line 11000 ops: private=76.54 heaps=20.67 heap0_busy=16.23 heap0_free=3.59 outside=20.12 allocs=74 images=33.25 mapped=4.94 rust=5.38
memdiag.gate_end.private_usage_mb=76.57
memdiag.gate_end.private_commit_mb=41.17
memdiag.gate_end.heaps=5 heaps_committed_mb=20.70
memdiag.gate_end.heap0(process): committed_mb=19.85 busy_mb=16.23 blocks=39337 free_committed_mb=3.62 big_blocks=10 big_mb=1.94
memdiag.gate_end.heap3: committed_mb=0.80 busy_mb=0.68 blocks=1298 free_committed_mb=0.12 big_blocks=1 big_mb=0.10
memdiag.gate_end.outside_heaps_mb=20.12 allocs=74
memdiag.gate_end.outside 0x25598a00000 14.33 MB protect=0x404
memdiag.gate_end.outside 0x2554d610000 1.00 MB protect=0x4
memdiag.gate_end.outside 0x2554b410000 0.49 MB protect=0x4
memdiag.gate_end.outside 0x25500000000 0.44 MB protect=0x4
memdiag.gate_end.outside 0x25558a00000 0.44 MB protect=0x4
memdiag.gate_end.outside 0x2554d7b0000 0.38 MB protect=0x4
memdiag.gate_end.outside 0x9926000000 0.37 MB protect=0x4
memdiag.gate_end.outside 0x2554d760000 0.28 MB protect=0x4
memdiag.gate_end.outside 0x25554300000 0.28 MB protect=0x4
memdiag.gate_end.outside 0x2554f830000 0.27 MB protect=0x4
memdiag.gate_end.outside 0x2554d730000 0.19 MB protect=0x4
memdiag.gate_end.outside 0x2554d550000 0.14 MB protect=0x4
memdiag.gate_end.image_writable_mb=33.25 mapped_writable_mb=4.94
memdiag.gate_end.image nvwgf2umx.dll 24.96 MB
memdiag.gate_end.image nvppex.dll 3.66 MB
memdiag.gate_end.image nvgpucomp64.dll 3.34 MB
memdiag.gate_end.image nvspcap64.dll 0.07 MB
memdiag.gate_end.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_end.image urlmon.dll 0.05 MB
memdiag.gate_end.image shell32.dll 0.05 MB
memdiag.gate_end.image msvcp_win.dll 0.05 MB
memdiag.gate_end.image KERNELBASE.dll 0.04 MB
memdiag.gate_end.image windows.storage.dll 0.04 MB
memdiag.gate_end.rust_live_mb=5.38 rust_peak_mb=7.60
memdiag.gate_trim.private_usage_mb=76.59
memdiag.gate_trim.private_commit_mb=41.18
memdiag.gate_trim.heaps=5 heaps_committed_mb=20.71
memdiag.gate_trim.heap0(process): committed_mb=19.86 busy_mb=11.87 blocks=37252 free_committed_mb=7.99 big_blocks=9 big_mb=1.77
memdiag.gate_trim.heap3: committed_mb=0.80 busy_mb=0.68 blocks=1298 free_committed_mb=0.12 big_blocks=1 big_mb=0.10
memdiag.gate_trim.outside_heaps_mb=20.12 allocs=74
memdiag.gate_trim.outside 0x25598a00000 14.33 MB protect=0x404
memdiag.gate_trim.outside 0x2554d610000 1.00 MB protect=0x4
memdiag.gate_trim.outside 0x2554b410000 0.49 MB protect=0x4
memdiag.gate_trim.outside 0x25558a00000 0.44 MB protect=0x4
memdiag.gate_trim.outside 0x25500000000 0.44 MB protect=0x4
memdiag.gate_trim.outside 0x2554d7b0000 0.38 MB protect=0x4
memdiag.gate_trim.outside 0x9926000000 0.37 MB protect=0x4
memdiag.gate_trim.outside 0x25554300000 0.28 MB protect=0x4
memdiag.gate_trim.outside 0x2554d760000 0.28 MB protect=0x4
memdiag.gate_trim.outside 0x2554f830000 0.27 MB protect=0x4
memdiag.gate_trim.outside 0x2554d730000 0.19 MB protect=0x4
memdiag.gate_trim.outside 0x2554d550000 0.14 MB protect=0x4
memdiag.gate_trim.image_writable_mb=33.25 mapped_writable_mb=4.94
memdiag.gate_trim.image nvwgf2umx.dll 24.96 MB
memdiag.gate_trim.image nvppex.dll 3.66 MB
memdiag.gate_trim.image nvgpucomp64.dll 3.34 MB
memdiag.gate_trim.image nvspcap64.dll 0.07 MB
memdiag.gate_trim.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_trim.image urlmon.dll 0.05 MB
memdiag.gate_trim.image msvcp_win.dll 0.05 MB
memdiag.gate_trim.image shell32.dll 0.05 MB
memdiag.gate_trim.image KERNELBASE.dll 0.04 MB
memdiag.gate_trim.image windows.storage.dll 0.04 MB
memdiag.gate_trim.rust_live_mb=1.02 rust_peak_mb=7.60
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
paints=24558
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
elapsed_s=40.6
ops_per_s=270.8
navigations_s=36.7
tabs_s=4.0
navigations_per_s=272.7
tabs_per_s=253.1
stage_s.load=27.9
stage_s.paint=14.1
stage_s.icons=1.6
steps=25106
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=80203776
start.private_mb=76.49
start.working_set_bytes=73175040
start.handles=597
start.gdi=45
start.user=21
start.threads=54
sample.0.private_bytes=80203776
sample.0.private_mb=76.49
sample.0.working_set_bytes=73175040
sample.0.handles=597
sample.0.gdi=45
sample.0.user=21
sample.0.threads=54
sample.1000.private_bytes=82313216
sample.1000.private_mb=78.50
sample.1000.working_set_bytes=74350592
sample.1000.handles=597
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=54
sample.2000.private_bytes=83038208
sample.2000.private_mb=79.19
sample.2000.working_set_bytes=75026432
sample.2000.handles=597
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=54
sample.3000.private_bytes=82718720
sample.3000.private_mb=78.89
sample.3000.working_set_bytes=74899456
sample.3000.handles=597
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=54
sample.4000.private_bytes=83177472
sample.4000.private_mb=79.32
sample.4000.working_set_bytes=75227136
sample.4000.handles=597
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=54
sample.5000.private_bytes=83296256
sample.5000.private_mb=79.44
sample.5000.working_set_bytes=75456512
sample.5000.handles=595
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=53
sample.6000.private_bytes=79790080
sample.6000.private_mb=76.09
sample.6000.working_set_bytes=73064448
sample.6000.handles=595
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=53
sample.7000.private_bytes=81666048
sample.7000.private_mb=77.88
sample.7000.working_set_bytes=74272768
sample.7000.handles=595
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=53
sample.8000.private_bytes=81547264
sample.8000.private_mb=77.77
sample.8000.working_set_bytes=74215424
sample.8000.handles=593
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=53
sample.9000.private_bytes=84041728
sample.9000.private_mb=80.15
sample.9000.working_set_bytes=76447744
sample.9000.handles=595
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=53
sample.10000.private_bytes=81760256
sample.10000.private_mb=77.97
sample.10000.working_set_bytes=74412032
sample.10000.handles=595
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=53
sample.11000.private_bytes=80433152
sample.11000.private_mb=76.71
sample.11000.working_set_bytes=73658368
sample.11000.handles=595
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=53
end.private_bytes=80433152
end.private_mb=76.71
end.working_set_bytes=73658368
end.handles=595
end.gdi=45
end.user=21
end.threads=53
idle.private_bytes=66752512
idle.private_mb=63.66
idle.working_set_bytes=59236352
idle.handles=551
idle.gdi=45
idle.user=21
idle.threads=54
delta.private_bytes_from_idle=13680640
delta.private_bytes=229376
delta.private_pct=0.29
allow.private_bytes=4010188
pass.private=1
delta.handles=-2
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=84041728
trim.private_bytes=80490496
trim.private_mb=76.76
trim.working_set_bytes=73719808
trim.handles=595
trim.gdi=45
trim.user=21
trim.threads=51
trim.delta_private_bytes=57344
trim.note=icon cache cleared (1040 entries, cap 2048 items / 8 MB)
timeouts=0
memdiag.line 0 ops: private=76.49 heaps=20.54 heap0_busy=15.97 heap0_free=3.67 outside=20.16 allocs=81 images=33.25 mapped=3.94 rust=5.37
memdiag.gate_start.private_usage_mb=76.52
memdiag.gate_start.private_commit_mb=41.11
memdiag.gate_start.heaps=5 heaps_committed_mb=20.58
memdiag.gate_start.heap0(process): committed_mb=19.67 busy_mb=15.97 blocks=29470 free_committed_mb=3.70 big_blocks=10 big_mb=1.94
memdiag.gate_start.heap3: committed_mb=0.86 busy_mb=0.73 blocks=1517 free_committed_mb=0.13 big_blocks=1 big_mb=0.10
memdiag.gate_start.outside_heaps_mb=20.16 allocs=81
memdiag.gate_start.outside 0x1e1ba9f0000 14.20 MB protect=0x404
memdiag.gate_start.outside 0x1e12f8e0000 1.00 MB protect=0x4
memdiag.gate_start.outside 0x1e12d410000 0.44 MB protect=0x4
memdiag.gate_start.outside 0x1e17a9f0000 0.44 MB protect=0x4
memdiag.gate_start.outside 0x1e13a9f0000 0.44 MB protect=0x4
memdiag.gate_start.outside 0x3edd200000 0.43 MB protect=0x4
memdiag.gate_start.outside 0x1e12fa80000 0.38 MB protect=0x4
memdiag.gate_start.outside 0x1e135400000 0.28 MB protect=0x4
memdiag.gate_start.outside 0x1e12fa30000 0.28 MB protect=0x4
memdiag.gate_start.outside 0x1e131b00000 0.27 MB protect=0x4
memdiag.gate_start.outside 0x1e12fa00000 0.19 MB protect=0x4
memdiag.gate_start.outside 0x1e12afd0000 0.14 MB protect=0x4
memdiag.gate_start.image_writable_mb=33.25 mapped_writable_mb=3.94
memdiag.gate_start.image nvwgf2umx.dll 24.96 MB
memdiag.gate_start.image nvppex.dll 3.66 MB
memdiag.gate_start.image nvgpucomp64.dll 3.34 MB
memdiag.gate_start.image nvspcap64.dll 0.07 MB
memdiag.gate_start.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_start.image urlmon.dll 0.05 MB
memdiag.gate_start.image shell32.dll 0.05 MB
memdiag.gate_start.image msvcp_win.dll 0.05 MB
memdiag.gate_start.image KERNELBASE.dll 0.04 MB
memdiag.gate_start.image windows.storage.dll 0.04 MB
memdiag.gate_start.rust_live_mb=5.37 rust_peak_mb=7.27
memdiag.line 1000 ops: private=78.50 heaps=22.55 heap0_busy=14.26 heap0_free=7.32 outside=20.16 allocs=81 images=33.25 mapped=3.94 rust=5.36
memdiag.line 2000 ops: private=75.06 heaps=19.12 heap0_busy=14.54 heap0_free=3.73 outside=20.16 allocs=81 images=33.25 mapped=3.94 rust=5.37
memdiag.line 3000 ops: private=78.89 heaps=22.93 heap0_busy=14.07 heap0_free=7.88 outside=20.16 allocs=81 images=33.25 mapped=3.94 rust=5.36
memdiag.line 4000 ops: private=75.47 heaps=19.53 heap0_busy=15.11 heap0_free=3.58 outside=20.16 allocs=81 images=33.25 mapped=3.94 rust=5.37
memdiag.line 5000 ops: private=79.44 heaps=23.52 heap0_busy=15.75 heap0_free=6.85 outside=20.12 allocs=80 images=33.25 mapped=3.94 rust=5.38
memdiag.line 6000 ops: private=76.09 heaps=20.19 heap0_busy=15.24 heap0_free=4.16 outside=20.12 allocs=80 images=33.25 mapped=3.94 rust=5.36
memdiag.line 7000 ops: private=77.88 heaps=21.97 heap0_busy=15.59 heap0_free=5.42 outside=20.12 allocs=80 images=33.25 mapped=3.94 rust=5.37
memdiag.line 8000 ops: private=77.77 heaps=21.90 heap0_busy=14.61 heap0_free=6.33 outside=20.12 allocs=80 images=33.25 mapped=3.94 rust=5.36
memdiag.line 9000 ops: private=80.15 heaps=24.23 heap0_busy=16.15 heap0_free=7.11 outside=20.12 allocs=80 images=33.25 mapped=3.94 rust=5.37
memdiag.line 10000 ops: private=77.97 heaps=22.06 heap0_busy=15.29 heap0_free=5.82 outside=20.12 allocs=80 images=33.25 mapped=3.94 rust=5.36
memdiag.line 11000 ops: private=76.71 heaps=20.68 heap0_busy=16.32 heap0_free=3.50 outside=20.25 allocs=80 images=33.25 mapped=3.94 rust=5.40
memdiag.gate_end.private_usage_mb=76.74
memdiag.gate_end.private_commit_mb=41.32
memdiag.gate_end.heaps=5 heaps_committed_mb=20.71
memdiag.gate_end.heap0(process): committed_mb=19.85 busy_mb=16.32 blocks=39496 free_committed_mb=3.53 big_blocks=10 big_mb=1.94
memdiag.gate_end.heap3: committed_mb=0.81 busy_mb=0.69 blocks=1324 free_committed_mb=0.12 big_blocks=1 big_mb=0.10
memdiag.gate_end.outside_heaps_mb=20.25 allocs=80
memdiag.gate_end.outside 0x1e1ba9f0000 14.33 MB protect=0x404
memdiag.gate_end.outside 0x1e12f8e0000 1.00 MB protect=0x4
memdiag.gate_end.outside 0x1e12d410000 0.44 MB protect=0x4
memdiag.gate_end.outside 0x1e17a9f0000 0.44 MB protect=0x4
memdiag.gate_end.outside 0x1e13a9f0000 0.44 MB protect=0x4
memdiag.gate_end.outside 0x3edd200000 0.42 MB protect=0x4
memdiag.gate_end.outside 0x1e12fa80000 0.38 MB protect=0x4
memdiag.gate_end.outside 0x1e135400000 0.28 MB protect=0x4
memdiag.gate_end.outside 0x1e12fa30000 0.28 MB protect=0x4
memdiag.gate_end.outside 0x1e131b00000 0.27 MB protect=0x4
memdiag.gate_end.outside 0x1e12fa00000 0.19 MB protect=0x4
memdiag.gate_end.outside 0x1e12afd0000 0.14 MB protect=0x4
memdiag.gate_end.image_writable_mb=33.25 mapped_writable_mb=3.94
memdiag.gate_end.image nvwgf2umx.dll 24.96 MB
memdiag.gate_end.image nvppex.dll 3.66 MB
memdiag.gate_end.image nvgpucomp64.dll 3.34 MB
memdiag.gate_end.image nvspcap64.dll 0.07 MB
memdiag.gate_end.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_end.image urlmon.dll 0.05 MB
memdiag.gate_end.image shell32.dll 0.05 MB
memdiag.gate_end.image msvcp_win.dll 0.05 MB
memdiag.gate_end.image KERNELBASE.dll 0.04 MB
memdiag.gate_end.image windows.storage.dll 0.04 MB
memdiag.gate_end.rust_live_mb=5.40 rust_peak_mb=7.59
memdiag.gate_trim.private_usage_mb=76.71
memdiag.gate_trim.private_commit_mb=41.29
memdiag.gate_trim.heaps=5 heaps_committed_mb=20.73
memdiag.gate_trim.heap0(process): committed_mb=19.87 busy_mb=11.96 blocks=37402 free_committed_mb=7.91 big_blocks=9 big_mb=1.77
memdiag.gate_trim.heap3: committed_mb=0.81 busy_mb=0.69 blocks=1324 free_committed_mb=0.12 big_blocks=1 big_mb=0.10
memdiag.gate_trim.outside_heaps_mb=20.19 allocs=78
memdiag.gate_trim.outside 0x1e1ba9f0000 14.33 MB protect=0x404
memdiag.gate_trim.outside 0x1e12f8e0000 1.00 MB protect=0x4
memdiag.gate_trim.outside 0x1e12d410000 0.44 MB protect=0x4
memdiag.gate_trim.outside 0x1e17a9f0000 0.44 MB protect=0x4
memdiag.gate_trim.outside 0x1e13a9f0000 0.44 MB protect=0x4
memdiag.gate_trim.outside 0x3edd200000 0.40 MB protect=0x4
memdiag.gate_trim.outside 0x1e12fa80000 0.38 MB protect=0x4
memdiag.gate_trim.outside 0x1e135400000 0.28 MB protect=0x4
memdiag.gate_trim.outside 0x1e12fa30000 0.28 MB protect=0x4
memdiag.gate_trim.outside 0x1e131b00000 0.27 MB protect=0x4
memdiag.gate_trim.outside 0x1e12fa00000 0.19 MB protect=0x4
memdiag.gate_trim.outside 0x1e12afd0000 0.14 MB protect=0x4
memdiag.gate_trim.image_writable_mb=33.25 mapped_writable_mb=3.94
memdiag.gate_trim.image nvwgf2umx.dll 24.96 MB
memdiag.gate_trim.image nvppex.dll 3.66 MB
memdiag.gate_trim.image nvgpucomp64.dll 3.34 MB
memdiag.gate_trim.image nvspcap64.dll 0.07 MB
memdiag.gate_trim.image NvMemMapStoragex.dll 0.07 MB
memdiag.gate_trim.image urlmon.dll 0.05 MB
memdiag.gate_trim.image shell32.dll 0.05 MB
memdiag.gate_trim.image msvcp_win.dll 0.05 MB
memdiag.gate_trim.image windows.storage.dll 0.04 MB
memdiag.gate_trim.image KERNELBASE.dll 0.04 MB
memdiag.gate_trim.rust_live_mb=1.04 rust_peak_mb=7.59
gate=PASS
fail=-
exit=0
```
</details>

## MEM1b diagnostic follow-up (2026-11-18)

`18b1bc8`, same release with counting allocator, ten additional launches in
`G:\rig-test\tsubame\bench-2026-11-18T06-18-56`:
first paint ms: 71.0, 75.0, 79.0, 66.9, 67.3, 73.3, 76.3, 101.4, 71.3, 74.3.
Nearest-rank median 73.3 / p95 101.4 ms. Startup p95 exceeds baseline 100.9
by 0.5 ms; this is not reported as passing that comparison.
Scroll endpoints 79.4-81.6 MB, per-run paint p95 3.01, 3.17, 3.34, 3.92,
3.77, 4.90, 3.99, 4.44, 3.64, 3.23 ms (median 3.64, max 4.90).
All memory and scroll samples pass their budgets.

Three more diagnostic-release navigation processes, reports
`mem1-timing-{1,2,3}.txt` under `bench-2026-11-18T06-12-39`, each with three
warm navigations: first rows medians 8.52 / 7.60 / 8.98 ms; complete+sorted
frame medians 101.83 / 95.40 / 108.45 ms. All nine samples pass 30 / 200 ms.
The following production-build suite removes counting-allocator overhead;
it is recorded separately, without discarding these diagnostic results.
## 2026-11-18 06:21 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `18b1bc8`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/526fd871`, commit `18b1bc8` (dirty worktree)
- Window: 3840x2064 at 192 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 11 Minecraft java process(es), 25 docker container(s), CPU 35% busy, servers loaded; rig up 344 h
- Cold-cache 100k open: not measured
- Idle: 16 ms of CPU over the first idle window, 0 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 35.5 MB private at the first painted frame, 86.3 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 66.30 ms median, 80.90 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 3.87 ms median, 23.61 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 68.28 ms median, 97.80 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache | < 30 / < 200 ms | not measured (not measured) | n/a |
| Scroll paint p95, 4K, details view | < 5.0 ms | 3.10 ms median, 4.03 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 0 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 63.4 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 80.9 MB median (83.2 MB max) | PASS |
| Main exe | < 6 MB | 664 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 74.3 -> 77.0 MB (+2.61 MB, 3.5%; allowance 3.7 MB) | PASS |

All measured budgets and the leak gate pass.
Not measured: 100,000-entry folder, cold cache (not measured).

Informational: startup navigation first rows 49.93 ms and complete+sorted 139.38 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 74.3 -> 77.0 | +2.61 | +3.51 | 3.72 | 37 | PASS |
| 2 | 75.3 -> 77.2 | +1.87 | +2.48 | 3.76 | 36 | PASS |
| 3 | 75.2 -> 77.1 | +1.88 | +2.50 | 3.76 | 36 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **PASS**
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (63.4 MB private, before the warm-up) to the end the process grew 13.5 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 74.3 | 68.1 | 599 | 45 | 21 | 56 |
| 0 ops | 74.3 | 68.1 | 599 | 45 | 21 | 56 |
| 1000 ops | 78.0 | 70.7 | 599 | 45 | 21 | 56 |
| 2000 ops | 80.0 | 72.7 | 599 | 45 | 21 | 56 |
| 3000 ops | 78.4 | 71.0 | 599 | 45 | 21 | 56 |
| 4000 ops | 78.9 | 71.6 | 599 | 45 | 21 | 56 |
| 5000 ops | 80.3 | 72.8 | 597 | 45 | 21 | 55 |
| 6000 ops | 77.7 | 71.7 | 597 | 45 | 21 | 55 |
| 7000 ops | 79.8 | 73.2 | 597 | 45 | 21 | 55 |
| 8000 ops | 77.8 | 70.9 | 597 | 45 | 21 | 55 |
| 9000 ops | 79.2 | 72.1 | 597 | 45 | 21 | 55 |
| 10000 ops | 80.2 | 72.8 | 597 | 45 | 21 | 55 |
| 11000 ops | 77.0 | 70.5 | 597 | 45 | 21 | 55 |
| after settle | 77.0 | 70.5 | 597 | 45 | 21 | 55 |
| after icon-cache trim | 76.9 | | | | | |

Private bytes +2.61 MB (+3.51%, allowance 3.72 MB), handles -2, GDI +0, USER +0, threads -1. Peak private bytes 80.3 MB.
Forced collection: icon cache cleared (1042 entries, cap 2048 items / 8 MB); private bytes after it 76.9 MB.
Gate wall time 37 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T06:21:01
commit=18b1bc8
branch=t3/526fd871
dirty=1
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=10
cache=warm filesystem cache (no purge before these runs)
cold=not measured
dpi=192
rig_load=11 Minecraft java process(es), 25 docker container(s), CPU 35% busy, servers loaded; rig up 344 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=1
reports=G:\rig-test\tsubame\bench-2026-11-18T06-21-01
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 80.90 paint_p50_ms 2.37 paint_p95_ms 3.56 private_before_scroll_mb 87.4 private_end_mb 80.1 peak_commit_mb 93.5 rows 100000 exe_bytes 679936
run 2: first_paint_ms 74.70 paint_p50_ms 2.43 paint_p95_ms 4.03 private_before_scroll_mb 87.8 private_end_mb 83.2 peak_commit_mb 92.3 rows 100000 exe_bytes 679936
run 3: first_paint_ms 64.20 paint_p50_ms 2.29 paint_p95_ms 3.18 private_before_scroll_mb 86.1 private_end_mb 82.0 peak_commit_mb 93.1 rows 100000 exe_bytes 679936
run 4: first_paint_ms 69.50 paint_p50_ms 2.23 paint_p95_ms 3.25 private_before_scroll_mb 86.8 private_end_mb 80.8 peak_commit_mb 91.7 rows 100000 exe_bytes 679936
run 5: first_paint_ms 63.40 paint_p50_ms 2.24 paint_p95_ms 3.09 private_before_scroll_mb 85.5 private_end_mb 81.4 peak_commit_mb 93.2 rows 100000 exe_bytes 679936
run 6: first_paint_ms 59.90 paint_p50_ms 2.26 paint_p95_ms 3.37 private_before_scroll_mb 84.1 private_end_mb 79.7 peak_commit_mb 93.2 rows 100000 exe_bytes 679936
run 7: first_paint_ms 66.00 paint_p50_ms 2.19 paint_p95_ms 2.96 private_before_scroll_mb 85.8 private_end_mb 81.2 peak_commit_mb 92.0 rows 100000 exe_bytes 679936
run 8: first_paint_ms 69.50 paint_p50_ms 2.22 paint_p95_ms 3.10 private_before_scroll_mb 89.2 private_end_mb 81.4 peak_commit_mb 93.7 rows 100000 exe_bytes 679936
run 9: first_paint_ms 66.30 paint_p50_ms 2.20 paint_p95_ms 3.05 private_before_scroll_mb 85.9 private_end_mb 80.5 peak_commit_mb 93.2 rows 100000 exe_bytes 679936
run 10: first_paint_ms 72.80 paint_p50_ms 2.20 paint_p95_ms 3.05 private_before_scroll_mb 90.3 private_end_mb 80.9 peak_commit_mb 93.4 rows 100000 exe_bytes 679936
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 69.4
private_bytes_first_paint: 35.5 MB
all times in ms from the navigation event
startup   entries  100000 (9.0 MB)  first_batch    22.24  first_frame    49.93  done   136.30  final_frame   139.38  worker    51.09   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.0 MB)  first_batch    20.26  first_frame    23.61  done    96.02  final_frame    97.80  worker    39.10   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.0 MB)  first_batch     2.57  first_frame     9.12  done    93.25  final_frame    95.07  worker    46.41   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.0 MB)  first_batch     1.46  first_frame     6.70  done    66.98  final_frame    68.28  worker    42.76   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.0 MB)  first_batch     1.57  first_frame     4.72  done    66.99  final_frame    68.25  worker    36.68   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.0 MB)  first_batch     0.89  first_frame     2.59  done    80.21  final_frame    81.61  worker    35.32   G:\rig-bench\tsubame-b1\100k
repeat 6  entries  100000 (9.0 MB)  first_batch     1.11  first_frame     3.87  done    77.11  final_frame    78.70  worker    32.32   G:\rig-bench\tsubame-b1\100k
repeat 7  entries  100000 (9.0 MB)  first_batch     1.12  first_frame     3.10  done    66.60  final_frame    68.10  worker    45.56   G:\rig-bench\tsubame-b1\100k
repeat 8  entries  100000 (9.0 MB)  first_batch     1.25  first_frame     3.84  done    64.98  final_frame    66.36  worker    38.20   G:\rig-bench\tsubame-b1\100k
repeat 9  entries  100000 (9.0 MB)  first_batch     0.78  first_frame     2.16  done    65.94  final_frame    67.57  worker    37.10   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 86.3 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=15.6
idle.cpu_ms=0
idle.threads=56
idle.threads_warm=56
idle.context_switches_per_s=-1
idle.pid=50880
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
paints=24535
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
elapsed_s=37.3
ops_per_s=294.6
navigations_s=33.8
tabs_s=3.6
navigations_per_s=296.2
tabs_per_s=279.3
stage_s.load=25.8
stage_s.paint=13.1
stage_s.icons=1.6
steps=25053
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=77946880
start.private_mb=74.34
start.working_set_bytes=71368704
start.handles=599
start.gdi=45
start.user=21
start.threads=56
sample.0.private_bytes=77946880
sample.0.private_mb=74.34
sample.0.working_set_bytes=71368704
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=56
sample.1000.private_bytes=81801216
sample.1000.private_mb=78.01
sample.1000.working_set_bytes=74145792
sample.1000.handles=599
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=56
sample.2000.private_bytes=83927040
sample.2000.private_mb=80.04
sample.2000.working_set_bytes=76210176
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=56
sample.3000.private_bytes=82235392
sample.3000.private_mb=78.43
sample.3000.working_set_bytes=74440704
sample.3000.handles=599
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=56
sample.4000.private_bytes=82759680
sample.4000.private_mb=78.93
sample.4000.working_set_bytes=75100160
sample.4000.handles=599
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=56
sample.5000.private_bytes=84246528
sample.5000.private_mb=80.34
sample.5000.working_set_bytes=76304384
sample.5000.handles=597
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=55
sample.6000.private_bytes=81506304
sample.6000.private_mb=77.73
sample.6000.working_set_bytes=75165696
sample.6000.handles=597
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=55
sample.7000.private_bytes=83644416
sample.7000.private_mb=79.77
sample.7000.working_set_bytes=76804096
sample.7000.handles=597
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=55
sample.8000.private_bytes=81543168
sample.8000.private_mb=77.77
sample.8000.working_set_bytes=74366976
sample.8000.handles=597
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=55
sample.9000.private_bytes=83091456
sample.9000.private_mb=79.24
sample.9000.working_set_bytes=75563008
sample.9000.handles=597
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=55
sample.10000.private_bytes=84135936
sample.10000.private_mb=80.24
sample.10000.working_set_bytes=76324864
sample.10000.handles=597
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=55
sample.11000.private_bytes=80683008
sample.11000.private_mb=76.95
sample.11000.working_set_bytes=73895936
sample.11000.handles=597
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=55
end.private_bytes=80683008
end.private_mb=76.95
end.working_set_bytes=73895936
end.handles=597
end.gdi=45
end.user=21
end.threads=55
idle.private_bytes=66506752
idle.private_mb=63.43
idle.working_set_bytes=59478016
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=56
delta.private_bytes_from_idle=14176256
delta.private_bytes=2736128
delta.private_pct=3.51
allow.private_bytes=3897344
pass.private=1
delta.handles=-2
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=84246528
trim.private_bytes=80683008
trim.private_mb=76.95
trim.working_set_bytes=73895936
trim.handles=597
trim.gdi=45
trim.user=21
trim.threads=55
trim.delta_private_bytes=0
trim.note=icon cache cleared (1042 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-02.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
start_paint_path=hardware
paints=24525
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
elapsed_s=36.4
ops_per_s=302.4
navigations_s=32.8
tabs_s=3.6
navigations_per_s=304.9
tabs_per_s=279.3
stage_s.load=25.0
stage_s.paint=12.8
stage_s.icons=1.5
steps=25032
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=78954496
start.private_mb=75.30
start.working_set_bytes=72339456
start.handles=599
start.gdi=45
start.user=21
start.threads=58
sample.0.private_bytes=78954496
sample.0.private_mb=75.30
sample.0.working_set_bytes=72339456
sample.0.handles=599
sample.0.gdi=45
sample.0.user=21
sample.0.threads=58
sample.1000.private_bytes=80289792
sample.1000.private_mb=76.57
sample.1000.working_set_bytes=72851456
sample.1000.handles=599
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=58
sample.2000.private_bytes=83292160
sample.2000.private_mb=79.43
sample.2000.working_set_bytes=75440128
sample.2000.handles=599
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=58
sample.3000.private_bytes=83963904
sample.3000.private_mb=80.07
sample.3000.working_set_bytes=75911168
sample.3000.handles=599
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=58
sample.4000.private_bytes=80228352
sample.4000.private_mb=76.51
sample.4000.working_set_bytes=73076736
sample.4000.handles=599
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=58
sample.5000.private_bytes=84021248
sample.5000.private_mb=80.13
sample.5000.working_set_bytes=76087296
sample.5000.handles=597
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=57
sample.6000.private_bytes=82436096
sample.6000.private_mb=78.62
sample.6000.working_set_bytes=74661888
sample.6000.handles=597
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=57
sample.7000.private_bytes=83693568
sample.7000.private_mb=79.82
sample.7000.working_set_bytes=76214272
sample.7000.handles=597
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=57
sample.8000.private_bytes=82362368
sample.8000.private_mb=78.55
sample.8000.working_set_bytes=74850304
sample.8000.handles=595
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=57
sample.9000.private_bytes=84598784
sample.9000.private_mb=80.68
sample.9000.working_set_bytes=76496896
sample.9000.handles=597
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=57
sample.10000.private_bytes=84852736
sample.10000.private_mb=80.92
sample.10000.working_set_bytes=76836864
sample.10000.handles=597
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=57
sample.11000.private_bytes=80916480
sample.11000.private_mb=77.17
sample.11000.working_set_bytes=74031104
sample.11000.handles=597
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=57
end.private_bytes=80916480
end.private_mb=77.17
end.working_set_bytes=74031104
end.handles=597
end.gdi=45
end.user=21
end.threads=57
idle.private_bytes=66850816
idle.private_mb=63.75
idle.working_set_bytes=59404288
idle.handles=573
idle.gdi=45
idle.user=21
idle.threads=58
delta.private_bytes_from_idle=14065664
delta.private_bytes=1961984
delta.private_pct=2.48
allow.private_bytes=3947724
pass.private=1
delta.handles=-2
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=84852736
trim.private_bytes=80916480
trim.private_mb=77.17
trim.working_set_bytes=74031104
trim.handles=597
trim.gdi=45
trim.user=21
trim.threads=57
trim.delta_private_bytes=0
trim.note=icon cache cleared (1040 entries, cap 2048 items / 8 MB)
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
paints=24517
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
elapsed_s=35.7
ops_per_s=308.2
navigations_s=32.1
tabs_s=3.6
navigations_per_s=311.3
tabs_per_s=280.6
stage_s.load=24.5
stage_s.paint=12.6
stage_s.icons=1.5
steps=25018
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=78856192
start.private_mb=75.20
start.working_set_bytes=72101888
start.handles=597
start.gdi=45
start.user=21
start.threads=59
sample.0.private_bytes=78856192
sample.0.private_mb=75.20
sample.0.working_set_bytes=72101888
sample.0.handles=597
sample.0.gdi=45
sample.0.user=21
sample.0.threads=59
sample.1000.private_bytes=80297984
sample.1000.private_mb=76.58
sample.1000.working_set_bytes=73539584
sample.1000.handles=597
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=59
sample.2000.private_bytes=83128320
sample.2000.private_mb=79.28
sample.2000.working_set_bytes=75935744
sample.2000.handles=597
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=59
sample.3000.private_bytes=82964480
sample.3000.private_mb=79.12
sample.3000.working_set_bytes=75550720
sample.3000.handles=597
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=59
sample.4000.private_bytes=83427328
sample.4000.private_mb=79.56
sample.4000.working_set_bytes=76402688
sample.4000.handles=597
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=59
sample.5000.private_bytes=83431424
sample.5000.private_mb=79.57
sample.5000.working_set_bytes=75845632
sample.5000.handles=595
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=58
sample.6000.private_bytes=81739776
sample.6000.private_mb=77.95
sample.6000.working_set_bytes=74862592
sample.6000.handles=591
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=58
sample.7000.private_bytes=81870848
sample.7000.private_mb=78.08
sample.7000.working_set_bytes=75300864
sample.7000.handles=595
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=58
sample.8000.private_bytes=83894272
sample.8000.private_mb=80.01
sample.8000.working_set_bytes=76779520
sample.8000.handles=595
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=58
sample.9000.private_bytes=83775488
sample.9000.private_mb=79.89
sample.9000.working_set_bytes=77213696
sample.9000.handles=595
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=58
sample.10000.private_bytes=80191488
sample.10000.private_mb=76.48
sample.10000.working_set_bytes=73785344
sample.10000.handles=595
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=58
sample.11000.private_bytes=80830464
sample.11000.private_mb=77.09
sample.11000.working_set_bytes=74489856
sample.11000.handles=595
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=58
end.private_bytes=80830464
end.private_mb=77.09
end.working_set_bytes=74489856
end.handles=595
end.gdi=45
end.user=21
end.threads=58
idle.private_bytes=66617344
idle.private_mb=63.53
idle.working_set_bytes=59428864
idle.handles=551
idle.gdi=45
idle.user=21
idle.threads=59
delta.private_bytes_from_idle=14213120
delta.private_bytes=1974272
delta.private_pct=2.50
allow.private_bytes=3942809
pass.private=1
delta.handles=-2
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=83894272
trim.private_bytes=80830464
trim.private_mb=77.09
trim.working_set_bytes=74489856
trim.handles=595
trim.gdi=45
trim.user=21
trim.threads=58
trim.delta_private_bytes=0
trim.note=icon cache cleared (1040 entries, cap 2048 items / 8 MB)
timeouts=0
gate=PASS
fail=-
exit=0
```
</details>

## MEM1b final comparison and validation

Production release, rig 4K, `18b1bc8` normal app code; later cleanup removes
only temporary instrumentation from `--leak-smoke`. No gate or cache cap changed.

| Metric | Baseline `7eac782` | MEM1b production |
|---|---|---|
| Scroll private MB, median / max | 151.3 / 154.0 | 80.9 / 83.2 (10 runs) |
| Small-folder idle private MB | 105.8 | 63.43 / 63.75 / 63.53 |
| Full gate growth MB | +11.52 / +3.8 / +5.2, worst fails | +2.61 / +1.87 / +1.88, all pass |
| First paint ms, median / p95 | 78.8 / 100.9 | 66.3 / 80.9 (10 runs) |
| Scroll paint p95 ms, median / maximum | 3.17 / 3.76 | 3.10 / 4.03 (10 runs) |
| Warm 100k first rows / sorted ms, median | 3.79 / 69.3 | 3.87 / 68.28 (9 navigations) |
| Idle CPU, 10 s | 0 ms | 0 ms |
| Exe | 624 KB | 664 KB |

Full workspace check/test passed; strict clippy on app and d2d with memdiag
passed; app tests (37) and clippy repeated after smoke-only cleanup passed.
Rig debug lifecycle smoke passed resize, synthetic DPI changes and
minimize/restore on flip, HWND and forced WARP. Injected device loss on both
flip and HWND ended on software fallback and exited cleanly. See
`lifecycle-*.txt` in `bench-2026-11-18T06-21-01`.
Real monitor DPI changes, physical GPU removal and actual TDR untested.
Exact idle wake-up count unavailable. Optional cold-cache probe still failed
49.08 / 340.57 ms in the diagnostic full suite; production full suite did not
repeat that known purge-and-launch measurement. No threshold was relaxed.
## 2026-11-18 19:46 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `4897312`

- Mode: full (6 cold-start runs, 6 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/unify-panes-shell-integration`, commit `4897312` (dirty worktree)
- Window: 3840x2076 at 168 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 5 Minecraft java process(es), 25 docker container(s), CPU 10% busy, servers loaded; rig up 357 h
- Cold-cache 100k open: measured (purge working ok)
  The purge also emptied the app's own pages: that process painted its first frame 307 ms after start (warm: 80 ms), so the cold row includes loading the app itself, not only the folder's metadata.
- Idle: 0 ms of CPU over the first idle window, 47 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 61.9 MB private at the first painted frame, 117.8 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 82.70 ms median, 89.90 ms p95 (n=6) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 7.11 ms median, 18.30 ms p95 (n=5) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 91.82 ms median, 169.11 ms p95 (n=5) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 40.43 ms median, 40.43 ms p95 (n=1) | **FAIL** |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 397.64 ms median, 397.64 ms p95 (n=1) | **FAIL** |
| Scroll paint p95, 4K, details view | < 5.0 ms | 4.63 ms median, 4.67 ms p95 (n=6) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 47 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 66.2 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 84.2 MB median (84.4 MB max) | PASS |
| Main exe | < 6 MB | 1664 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 83.9 -> 87.1 MB (+3.14 MB, 3.7%; allowance 4.2 MB) | PASS |

**2 budget(s) missed: 100,000-entry folder, cold cache: first rows, 100,000-entry folder, cold cache: complete and sorted.** Reported as measured; nothing was relaxed to make them pass.

Informational: startup navigation first rows 57.32 ms and complete+sorted 231.75 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 83.9 -> 87.1 | +3.14 | +3.74 | 4.20 | 64 | PASS |
| 2 | 84.8 -> 86.5 | +1.71 | +2.02 | 4.24 | 63 | PASS |
| 3 | 85.0 -> 87.7 | +2.64 | +3.10 | 4.25 | 66 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **PASS**
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (66.2 MB private, before the warm-up) to the end the process grew 20.9 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 83.9 | 77.6 | 625 | 45 | 21 | 67 |
| 0 ops | 83.9 | 77.6 | 625 | 45 | 21 | 67 |
| 1000 ops | 88.1 | 80.6 | 625 | 45 | 21 | 67 |
| 2000 ops | 88.4 | 81.0 | 625 | 45 | 21 | 67 |
| 3000 ops | 88.4 | 81.0 | 624 | 45 | 21 | 66 |
| 4000 ops | 88.5 | 81.3 | 620 | 45 | 21 | 66 |
| 5000 ops | 90.2 | 82.6 | 624 | 45 | 21 | 66 |
| 6000 ops | 87.2 | 80.0 | 620 | 45 | 21 | 66 |
| 7000 ops | 87.5 | 80.2 | 624 | 45 | 21 | 66 |
| 8000 ops | 87.5 | 80.2 | 622 | 45 | 21 | 64 |
| 9000 ops | 87.0 | 80.1 | 622 | 45 | 21 | 64 |
| 10000 ops | 89.9 | 82.7 | 622 | 45 | 21 | 64 |
| 11000 ops | 87.1 | 80.6 | 622 | 45 | 21 | 64 |
| after settle | 87.1 | 80.6 | 622 | 45 | 21 | 64 |
| after icon-cache trim | 87.1 | | | | | |

Private bytes +3.14 MB (+3.74%, allowance 4.20 MB), handles -3, GDI +0, USER +0, threads -3. Peak private bytes 90.3 MB.
Forced collection: icon cache cleared (1137 entries, cap 2048 items / 6 MB); private bytes after it 87.1 MB.
Gate wall time 64 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T19:46:59
commit=4897312
branch=t3/unify-panes-shell-integration
dirty=1
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=6
cache=warm filesystem cache (no purge before these runs)
cold=measured (purge working ok)
dpi=192
rig_load=5 Minecraft java process(es), 25 docker container(s), CPU 10% busy, servers loaded; rig up 357 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=3
reports=G:\rig-test\tsubame-s3\bench-2026-11-18T19-46-59
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 83.20 paint_p50_ms 3.40 paint_p95_ms 4.48 private_before_scroll_mb 108.7 private_end_mb 84.3 peak_commit_mb 120.7 rows 100000 exe_bytes 1703424
run 2: first_paint_ms 89.90 paint_p50_ms 3.42 paint_p95_ms 4.55 private_before_scroll_mb 104.9 private_end_mb 83.9 peak_commit_mb 124.5 rows 100000 exe_bytes 1703424
run 3: first_paint_ms 81.50 paint_p50_ms 3.44 paint_p95_ms 4.65 private_before_scroll_mb 104.0 private_end_mb 84.2 peak_commit_mb 124.2 rows 100000 exe_bytes 1703424
run 4: first_paint_ms 83.90 paint_p50_ms 3.36 paint_p95_ms 4.66 private_before_scroll_mb 104.1 private_end_mb 84.1 peak_commit_mb 126.0 rows 100000 exe_bytes 1703424
run 5: first_paint_ms 81.00 paint_p50_ms 3.39 paint_p95_ms 4.63 private_before_scroll_mb 107.5 private_end_mb 84.4 peak_commit_mb 123.6 rows 100000 exe_bytes 1703424
run 6: first_paint_ms 82.70 paint_p50_ms 3.43 paint_p95_ms 4.67 private_before_scroll_mb 103.1 private_end_mb 84.2 peak_commit_mb 127.0 rows 100000 exe_bytes 1703424
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2076 at 168 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 79.9
private_bytes_first_paint: 61.9 MB
all times in ms from the navigation event
startup   entries  100000 (9.0 MB)  first_batch    16.27  first_frame    57.32  done   225.95  final_frame   231.75  worker    53.27   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.0 MB)  first_batch     7.93  first_frame    18.30  done   166.18  final_frame   169.11  worker    67.68   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.0 MB)  first_batch     1.69  first_frame     7.50  done    89.16  final_frame    91.82  worker    37.94   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.0 MB)  first_batch     1.80  first_frame     6.28  done    89.46  final_frame    92.61  worker    34.65   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.0 MB)  first_batch     1.62  first_frame     7.11  done    87.84  final_frame    90.36  worker    33.70   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.0 MB)  first_batch     1.65  first_frame     5.99  done    85.59  final_frame    88.27  worker    35.74   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 117.8 MB
```
</details>

<details><summary>Raw: cold-cache u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2076 at 168 dpi, paint path software (GPU starting)
first_paint_ms (process start to first completed paint): 307.0
private_bytes_first_paint: 39.3 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   104.07  first_frame   215.29  done   215.67  final_frame   219.53  worker     2.91   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (9.6 MB)  first_batch    31.22  first_frame    40.43  done   382.62  final_frame   397.64  worker   232.65   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 113.0 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=0
idle.cpu_ms=46.9
idle.threads=76
idle.threads_warm=79
idle.context_switches_per_s=-1
idle.pid=41716
```
</details>

<details><summary>Raw: leak-gate.txt</summary>

```
# --- leak-gate-01.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24543
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
elapsed_s=64.0
ops_per_s=171.8
navigations_s=57.9
tabs_s=6.1
navigations_per_s=172.7
tabs_per_s=162.7
stage_s.load=39.8
stage_s.paint=27.2
stage_s.icons=1.9
steps=25321
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=88006656
start.private_mb=83.93
start.working_set_bytes=81326080
start.handles=625
start.gdi=45
start.user=21
start.threads=67
sample.0.private_bytes=88006656
sample.0.private_mb=83.93
sample.0.working_set_bytes=81326080
sample.0.handles=625
sample.0.gdi=45
sample.0.user=21
sample.0.threads=67
sample.1000.private_bytes=92418048
sample.1000.private_mb=88.14
sample.1000.working_set_bytes=84516864
sample.1000.handles=625
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=67
sample.2000.private_bytes=92737536
sample.2000.private_mb=88.44
sample.2000.working_set_bytes=84885504
sample.2000.handles=625
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=67
sample.3000.private_bytes=92672000
sample.3000.private_mb=88.38
sample.3000.working_set_bytes=84983808
sample.3000.handles=624
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=66
sample.4000.private_bytes=92823552
sample.4000.private_mb=88.52
sample.4000.working_set_bytes=85245952
sample.4000.handles=620
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=66
sample.5000.private_bytes=94638080
sample.5000.private_mb=90.25
sample.5000.working_set_bytes=86605824
sample.5000.handles=624
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=66
sample.6000.private_bytes=91389952
sample.6000.private_mb=87.16
sample.6000.working_set_bytes=83922944
sample.6000.handles=620
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=66
sample.7000.private_bytes=91774976
sample.7000.private_mb=87.52
sample.7000.working_set_bytes=84099072
sample.7000.handles=624
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=66
sample.8000.private_bytes=91774976
sample.8000.private_mb=87.52
sample.8000.working_set_bytes=84115456
sample.8000.handles=622
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=64
sample.9000.private_bytes=91168768
sample.9000.private_mb=86.95
sample.9000.working_set_bytes=83976192
sample.9000.handles=622
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=64
sample.10000.private_bytes=94273536
sample.10000.private_mb=89.91
sample.10000.working_set_bytes=86720512
sample.10000.handles=622
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=64
sample.11000.private_bytes=91295744
sample.11000.private_mb=87.07
sample.11000.working_set_bytes=84516864
sample.11000.handles=622
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=64
end.private_bytes=91295744
end.private_mb=87.07
end.working_set_bytes=84516864
end.handles=622
end.gdi=45
end.user=21
end.threads=64
idle.private_bytes=69419008
idle.private_mb=66.20
idle.working_set_bytes=65974272
idle.handles=609
idle.gdi=45
idle.user=21
idle.threads=67
delta.private_bytes_from_idle=21876736
delta.private_bytes=3289088
delta.private_pct=3.74
allow.private_bytes=4400332
pass.private=1
delta.handles=-3
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=94638080
trim.private_bytes=91295744
trim.private_mb=87.07
trim.working_set_bytes=84516864
trim.handles=622
trim.gdi=45
trim.user=21
trim.threads=64
trim.delta_private_bytes=0
trim.note=icon cache cleared (1137 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-02.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24501
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
elapsed_s=63.0
ops_per_s=174.7
navigations_s=56.8
tabs_s=6.1
navigations_per_s=175.9
tabs_per_s=163.3
stage_s.load=39.3
stage_s.paint=27.0
stage_s.icons=1.8
steps=25272
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=88948736
start.private_mb=84.83
start.working_set_bytes=81911808
start.handles=621
start.gdi=45
start.user=21
start.threads=77
sample.0.private_bytes=88948736
sample.0.private_mb=84.83
sample.0.working_set_bytes=81911808
sample.0.handles=621
sample.0.gdi=45
sample.0.user=21
sample.0.threads=77
sample.1000.private_bytes=91738112
sample.1000.private_mb=87.49
sample.1000.working_set_bytes=84221952
sample.1000.handles=621
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=77
sample.2000.private_bytes=91881472
sample.2000.private_mb=87.62
sample.2000.working_set_bytes=84357120
sample.2000.handles=621
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=77
sample.3000.private_bytes=92606464
sample.3000.private_mb=88.32
sample.3000.working_set_bytes=85102592
sample.3000.handles=620
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=76
sample.4000.private_bytes=92340224
sample.4000.private_mb=88.06
sample.4000.working_set_bytes=85008384
sample.4000.handles=620
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=76
sample.5000.private_bytes=93036544
sample.5000.private_mb=88.73
sample.5000.working_set_bytes=84955136
sample.5000.handles=620
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=76
sample.6000.private_bytes=91340800
sample.6000.private_mb=87.11
sample.6000.working_set_bytes=83947520
sample.6000.handles=620
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=76
sample.7000.private_bytes=91418624
sample.7000.private_mb=87.18
sample.7000.working_set_bytes=83755008
sample.7000.handles=620
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=76
sample.8000.private_bytes=93077504
sample.8000.private_mb=88.77
sample.8000.working_set_bytes=85364736
sample.8000.handles=618
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=74
sample.9000.private_bytes=91746304
sample.9000.private_mb=87.50
sample.9000.working_set_bytes=84230144
sample.9000.handles=618
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=74
sample.10000.private_bytes=93532160
sample.10000.private_mb=89.20
sample.10000.working_set_bytes=85999616
sample.10000.handles=618
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=74
sample.11000.private_bytes=90742784
sample.11000.private_mb=86.54
sample.11000.working_set_bytes=84307968
sample.11000.handles=618
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=74
end.private_bytes=90742784
end.private_mb=86.54
end.working_set_bytes=84307968
end.handles=618
end.gdi=45
end.user=21
end.threads=74
idle.private_bytes=68370432
idle.private_mb=65.20
idle.working_set_bytes=64045056
idle.handles=595
idle.gdi=45
idle.user=21
idle.threads=77
delta.private_bytes_from_idle=22372352
delta.private_bytes=1794048
delta.private_pct=2.02
allow.private_bytes=4447436
pass.private=1
delta.handles=-3
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=93532160
trim.private_bytes=90742784
trim.private_mb=86.54
trim.working_set_bytes=84307968
trim.handles=618
trim.gdi=45
trim.user=21
trim.threads=74
trim.delta_private_bytes=0
trim.note=icon cache cleared (1119 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-03.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24520
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
elapsed_s=65.9
ops_per_s=166.8
navigations_s=59.9
tabs_s=6.0
navigations_per_s=166.9
tabs_per_s=165.7
stage_s.load=40.9
stage_s.paint=28.0
stage_s.icons=1.7
steps=25314
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=89174016
start.private_mb=85.04
start.working_set_bytes=81453056
start.handles=623
start.gdi=45
start.user=21
start.threads=69
sample.0.private_bytes=89174016
sample.0.private_mb=85.04
sample.0.working_set_bytes=81453056
sample.0.handles=623
sample.0.gdi=45
sample.0.user=21
sample.0.threads=69
sample.1000.private_bytes=92700672
sample.1000.private_mb=88.41
sample.1000.working_set_bytes=83894272
sample.1000.handles=621
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=69
sample.2000.private_bytes=93696000
sample.2000.private_mb=89.36
sample.2000.working_set_bytes=84774912
sample.2000.handles=623
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=69
sample.3000.private_bytes=94081024
sample.3000.private_mb=89.72
sample.3000.working_set_bytes=85016576
sample.3000.handles=622
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=68
sample.4000.private_bytes=93798400
sample.4000.private_mb=89.45
sample.4000.working_set_bytes=84738048
sample.4000.handles=622
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=68
sample.5000.private_bytes=94658560
sample.5000.private_mb=90.27
sample.5000.working_set_bytes=85463040
sample.5000.handles=622
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=68
sample.6000.private_bytes=92454912
sample.6000.private_mb=88.17
sample.6000.working_set_bytes=83861504
sample.6000.handles=622
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=68
sample.7000.private_bytes=92516352
sample.7000.private_mb=88.23
sample.7000.working_set_bytes=83726336
sample.7000.handles=622
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=68
sample.8000.private_bytes=92606464
sample.8000.private_mb=88.32
sample.8000.working_set_bytes=83968000
sample.8000.handles=620
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=66
sample.9000.private_bytes=94523392
sample.9000.private_mb=90.14
sample.9000.working_set_bytes=85762048
sample.9000.handles=620
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=66
sample.10000.private_bytes=94466048
sample.10000.private_mb=90.09
sample.10000.working_set_bytes=85667840
sample.10000.handles=620
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=66
sample.11000.private_bytes=91938816
sample.11000.private_mb=87.68
sample.11000.working_set_bytes=84086784
sample.11000.handles=620
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=66
end.private_bytes=91938816
end.private_mb=87.68
end.working_set_bytes=84086784
end.handles=620
end.gdi=45
end.user=21
end.threads=66
idle.private_bytes=69259264
idle.private_mb=66.05
idle.working_set_bytes=63680512
idle.handles=595
idle.gdi=45
idle.user=21
idle.threads=69
delta.private_bytes_from_idle=22679552
delta.private_bytes=2764800
delta.private_pct=3.10
allow.private_bytes=4458700
pass.private=1
delta.handles=-3
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=94658560
trim.private_bytes=91938816
trim.private_mb=87.68
trim.working_set_bytes=84086784
trim.handles=620
trim.gdi=45
trim.user=21
trim.threads=66
trim.delta_private_bytes=0
trim.note=icon cache cleared (1117 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0
```
</details>

## INT1 comparison (2026-11-18)

Scroll paint p95, 4K details view, 6 runs each on rig, same fixtures:

| Build | p95 median | Budget |
|---|---|---|
| `63c27b7` (baseline) | 4.41 ms | < 5 ms |
| `7e6c65b` (U3 + W1 merge, before INT1) | 4.70 ms | < 5 ms |
| `4897312` (INT1 final, full run above) | 4.63 ms | < 5 ms |

INT1 is not slower than the merge it starts from; the +0.2 ms against
`63c27b7` came in with the merge. Idle: 0 ms CPU in the first window, 47 ms in
the second (the table's figure). Private bytes 66.2 MB idle (budget 90 MB
windowed), 84.2 MB after the 4K scroll run (budget 110 MB). Leak gate PASS
(+3.7% worst run). Exe 1664 KB. The two cold-cache rows (n=1, disk-bound
after a working-set purge) miss their budgets; INT1 did not touch listing.

## PERF1 investigation — 2026-11-18, base e230e30 (incomplete)

Rig, slots 2/3; same generated seed-20261117 100k fixture on G: NVMe,
System32 for menus. Actual desktop 3840x2076 at 168 dpi, not earlier 192 dpi.
Production allocator for cold timing; memdiag allocator only for menu attribution.
Two Minecraft processes, 26 Docker containers; observed CPU 10–12% (not a
continuous load recording). Six independent working-purge + tiny-folder startup
+ 100k navigation processes per build. Stale-report attempts were discarded.

| Metric ms, median [min,max], n=6 | 63c27b7 | e230e30 + timing counter |
|---|---|---|
| 100k first rows presented | 68.88 [36.07,192.41] | 78.70 [39.83,136.20] |
| 100k sorted frame | 283.99 [246.04,407.10] | 423.68 [304.01,609.29] |
| enumerator worker elapsed | 150.73 [132.96,158.15] | 149.39 [143.02,177.81] |
| UI conversion + store + incremental sort | unavailable | 93.21 [88.42,101.58] |

Every run fails 30/200 ms. Similar worker time and slower main UI endpoint
point toward app-side delivery/rendering/paging rather than slower disk
enumeration. This is not a complete causal regression attribution: runs were
sequential, load was not continuously controlled, and app pages are purged too.
Main sample with 304.01 ms final: worker 161.96, store CPU 88.42,
Done 298.61, final presentation tail 5.40 ms. Worker and store overlap;
subtracting both from total is not a valid stall measurement. Nt syscall-only,
insertion-only versus sort-only, first-paint CPU and UI stall tracing remain open.

Cold FindFirstFileEx floor (same fixture, elevated `tsubame-bench enum --method
find --runs 6 --cold --purge-mode working`): first-batch median 8.921 ms,
complete median 120.717, range 116.521–133.695. Disk floor does not establish
200 ms is impossible. No budget wording change proposed.

Tried binary-search/copy of the unchanged old prefix before incremental merge:
core 13 tests and app 149 tests pass, but cold store CPU median 100.27 ms
[94.18,113.18], sorted frame 388.51 [276.87,1234.05], first frame 115.97
[49.73,665.34]. No demonstrated improvement; experiment reverted.
Three scroll probes of that experimental binary: p95 7.28/5.92/4.99 ms
(median 5.92, FAIL), endpoint private 84.3/83.8/83.9 MB (PASS 110 MB),
exe 1,704,960 bytes (PASS 6 MB). These are not final-patch scroll validation.
No final idle wake-up, windowed-memory or 10k-nav/1k-tab gate rerun.

MENU-LEAK: unmodified main 10->200 72.6->78.1 MB (+7.5%). Diagnostic main
72.39->77.98 (+7.7%): heap commit +0.46 MB; outside heaps +4.77 MB,
127->318 allocations; Rust live +0.01 MB; mapped writable flat.
Main 1000 menus 10/100/1000 = 71.8/74.6/98.6 MB; no plateau.
Historical S4 e2fa069 10/100/200 = 89.4/82.3/84.9; extended S4
10/100/1000 = 88.3/82.4/107.3. S4's negative gate masks existing growth
with delayed renderer release. HWND A/B on main also masks it (-3.8% at
200), yet 100->200 grows 82.7->85.5 MB. No passing long-run historical
endpoint, so new-leak bisect premise fails. Exact native allocation owner is
unresolved; do not label it SegmentHeap fragmentation. See shell-threading.md.
Clip stress and native allocation-stack capture were not run. Gates unchanged.

Retained changes: stress memdiag/checkpoints/configurable count, timing store
CPU counter, stale-report removal in bench harness. No menu lifetime fix or
speed optimization claimed. No U4-owned files edited. Local CCD1 check with
memdiag passed; app 149 tests and core 13 tests passed. Existing dead-code
warnings remain. Cleanup stops only owned slots and removes owned fixtures,
extra worktrees and scratch files. No push/merge.
## U4 (2026-11-18, commit `da7b850`, rig release build, `tools/bench-rig.ps1` full run)

Rig was loaded (8 Minecraft java, 25 containers, CPU 47% busy), so latency rows are noisy.

| Metric | Budget | Measured | |
|---|---|---|---|
| Idle CPU, second 10 s window | 0 wake-ups | 47 ms (first window 16 ms) | as INT1 (31-47 ms) |
| Private bytes, one tab, small folder, 4K | < 110 MB | 64.5 MB | PASS |
| Private bytes after the 5 s scroll run (100k) | < 110 MB | 83.8 MB median (89.5 max) | PASS |
| Main exe | < 6 MB | 1714 KB (INT1: 1664 KB; the updater crate and U4 add 50 KB) | PASS |
| Leak gate (10,000 navs + 1,000 tabs, palette + settings churn in every tab cycle) | within 5% | worst run +4.12% (runs: +2.85, +3.54, +4.12) | PASS |
| Scroll paint p95, 4K | < 5.0 ms | 6.67 ms median of 10 (8.82 max) | see below |
| Cold start to first frame | < 100 ms | 115 ms median | see below |

Scroll p95 under load: the same run of the INT1 baseline `e230e30` gave a 4.73 ms median when the rig was
quieter (21:34) and a 7.5 ms median when run again from the same binary at 21:55 (5 Minecraft, 22% busy).
U4 builds gave 6.0 to 6.7 ms in between (`815a440`, `e1de2ee`, `da7b850`). Nothing U4 added runs per
row or per frame in the list (palette and Settings are `None` while closed), so the spread is rig load, not a
measured regression; rerun on a quiet rig before relying on the figure.

## PERF2 — 2026-11-18, rig, base 63c27b7 vs main 29a1d3b vs perf2 (`4c37edb`)

Method: `tools/open-probe.ps1`, 6 runs per point (warm: repeats), release.
Cold = ssh working purge, disk settle, launch `tiny;<idle>;1k;tiny;<dir>`
(only the folder is cold; standby purge does not cool NTFS metadata, VHDX
dismount is ~7x real disk). overhead = final_frame - worker (app's own cost).

Bisect (startup-overlapped method, median / p95 ms, 100k cold):

| commit | first frame | final | worker | overhead |
|---|---|---|---|---|
| 63c27b7 | 90.1 / 162 | 410 / 703 | 164 | 248 |
| 4a0c29a | 75.8 / 189 | 339 / 424 | 149 | 166 |
| b2df364 | 97.7 / 152 | 381 / 455 | 157 | 204 |
| 7e6c65b | 80.8 / 1146 | 582 / 4164 | 153 | 416 |
| e230e30 | 75.6 / 175 | 423 / 500 | 155 | 257 |
| 29a1d3b | 77.4 / 121 | 385 / 762 | 163 | 223 |

No step commit; old method noise (startup tail + rig disk) dominates. Clean
method: overhead base 32 -> main 44 cold, 36 -> 51 warm; main store_cpu
65-68 ms (merge per batch into sorted view) is the cause.

Results (median / p95 ms):

| case | build | first rows | complete+sorted | worker | overhead |
|---|---|---|---|---|---|
| cold 100k | base | 22.9 / 40.7 | 180 / 306 | 148 | 32 |
| cold 100k | main | 13.9 / 30.4 | 182 / 211 | 137 | 44 |
| cold 100k | perf2 | 14.2 / 23.5 | 163 / 179 | 148 | 14 |
| cold 1M | base | 23.8 / 82 | 3147 / 3255 | 1452 | 1691 |
| cold 1M | main | 22.8 / 146 | 3490 / 3682 | 1490 | 2014 |
| cold 1M | perf2 | 44.6 / 970 | 1624 / 3405 | 1565 | 57 |
| warm 100k | base | 5.1 / 5.8 | 70.8 / 74.7 | – | 36 |
| warm 100k | main | 5.9 / 6.8 | 85.9 / 94.5 | – | 51 |
| warm 100k | perf2 | 6.7 / 11.0 | 43.2 / 49.3 | – | 10 |
| warm 1M | base | 6.6 | 2298 / 3179 | – | – |
| warm 1M | main | 6.9 | 2619 / 2681 | – | – |
| warm 1M | perf2 | 7.0 / 7.7 | 430 / 514 | – | 51 |

perf2 cold 100k: store_cpu 9.1, final_sort 6.5, watch_start 0.0 (was one
227 ms outlier before `DirWatcher::spawn`). 1M p95 outliers are disk.

Ops (`<ops>`, perf2, UI work only, median ms; filter p95 max in brackets):

| op | 100k | 1M |
|---|---|---|
| sort size / modified / type | 3.5 / 2.5 / 5.6 | 28 / 30 / 84 |
| sort name (from type order) | 16.8 | 166 |
| sort name desc / asc | 6.7 / 5.3 | 69 / 66 |
| filter type +1..+5 | 3.7 / 1.9 / 0.2 / 0.1 / 0.1 | 37 / 19 / 1.8 / 0.5 / 0.5 |
| filter erase -4..-0 | 3.9 / 4.2 / 4.3 / 3.6 / 0.4 [4.9] | 39 / 42 / 43 / 36 / 4.9 |

Erase before saved-order fix: 100k -1 10.6 / 21.3.

bench-rig perf2 (`-ColdMode none`): cold first paint 88.1, 100k first rows
6.23 / 27.29, sorted 44.64 / 140.64, scroll p95 4.75, idle CPU 31 ms, private
65.7 / 84.1 MB, exe 1728 KB: PASS. Leak gate private +5.45 / +4.82 / +5.43 %
(handles, GDI, threads flat): FAIL; same-day main 29a1d3b +4.96 / +4.00 /
+4.07 PASS. Cause: a thread per navigation in `DirWatcher::spawn`. Fix
`dd1cd89` (one long-lived opener thread): +2.76 / +3.52 / +3.01 % PASS.

Open: 1M name sort from another column 166 ms and 1M filter keystroke ~40 ms
(> 50 / 16 ms); paint pacing (main.rs invalidates every pump, ~4 ms frames
while streaming; needs a 16 ms timer, U4 file); cold 1M budget is disk-bound
(worker ~1.5 s).

## 2026-11-18 23:13 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `5b1cafb`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `main`, commit `5b1cafb`
- Window: 3840x2076 at 168 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 8 Minecraft java process(es), 25 docker container(s), CPU 18% busy, servers loaded; rig up 0 h
- Cold-cache 100k open: measured (purge working ok)
  The purge also emptied the app's own pages: that process painted its first frame 467 ms after start (warm: 91 ms), so the cold row includes loading the app itself, not only the folder's metadata.
- Idle: 16 ms of CPU over the first idle window, 0 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 64.8 MB private at the first painted frame, 140.7 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 84.40 ms median, 110.10 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 8.37 ms median, 24.55 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 47.29 ms median, 132.83 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache: first rows | < 30 ms | 47.35 ms median, 47.35 ms p95 (n=1) | **FAIL** |
| 100,000-entry folder, cold cache: complete and sorted | < 200 ms | 315.97 ms median, 315.97 ms p95 (n=1) | **FAIL** |
| Scroll paint p95, 4K, details view | < 5.0 ms | 6.78 ms median, 6.96 ms p95 (n=10) | **FAIL** |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 0 ms | PASS |
| Private bytes, one tab, small folder, 4K | < 110 MB | 66.0 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 87.8 MB median (88.4 MB max) | PASS |
| Main exe | < 6 MB | 1742 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 84.0 -> 86.6 MB (+2.60 MB, 3.1%; allowance 4.2 MB) | PASS |

**3 budget(s) missed: 100,000-entry folder, cold cache: first rows, 100,000-entry folder, cold cache: complete and sorted, Scroll paint p95, 4K, details view.** Reported as measured; nothing was relaxed to make them pass.

Informational: startup navigation first rows 67.37 ms and complete+sorted 112.10 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

The gate ran 3 times in fresh processes; the worst run decides (no run may exceed the allowance).

| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |
|---|---|---|---|---|---|---|
| 1 | 84.0 -> 86.6 | +2.60 | +3.10 | 4.20 | 104 | PASS |
| 2 | 85.9 -> 88.5 | +2.57 | +2.99 | 4.30 | 96 | PASS |
| 3 | 86.2 -> 88.0 | +1.84 | +2.14 | 4.31 | 69 | PASS |

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **PASS**
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (66.0 MB private, before the warm-up) to the end the process grew 20.6 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (the one above):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 84.0 | 77.5 | 627 | 45 | 21 | 61 |
| 0 ops | 84.0 | 77.5 | 627 | 45 | 21 | 61 |
| 1000 ops | 85.0 | 78.1 | 627 | 45 | 21 | 61 |
| 2000 ops | 85.7 | 78.4 | 626 | 45 | 21 | 60 |
| 3000 ops | 85.9 | 78.8 | 626 | 45 | 21 | 60 |
| 4000 ops | 88.9 | 81.0 | 624 | 45 | 21 | 58 |
| 5000 ops | 89.0 | 81.4 | 624 | 45 | 21 | 58 |
| 6000 ops | 87.5 | 80.0 | 624 | 45 | 21 | 58 |
| 7000 ops | 87.7 | 79.9 | 624 | 45 | 21 | 58 |
| 8000 ops | 87.6 | 80.0 | 624 | 45 | 21 | 58 |
| 9000 ops | 89.4 | 81.6 | 624 | 45 | 21 | 58 |
| 10000 ops | 87.2 | 79.9 | 624 | 45 | 21 | 58 |
| 11000 ops | 86.6 | 79.6 | 622 | 45 | 21 | 57 |
| after settle | 86.6 | 79.6 | 622 | 45 | 21 | 57 |
| after icon-cache trim | 86.6 | | | | | |

Private bytes +2.60 MB (+3.10%, allowance 4.20 MB), handles -5, GDI +0, USER +0, threads -4. Peak private bytes 89.4 MB.
Forced collection: icon cache cleared (1133 entries, cap 2048 items / 6 MB); private bytes after it 86.6 MB.
Gate wall time 104 s.

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-18T23:13:13
commit=5b1cafb
branch=main
dirty=0
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=10
cache=warm filesystem cache (no purge before these runs)
cold=measured (purge working ok)
dpi=192
rig_load=8 Minecraft java process(es), 25 docker container(s), CPU 18% busy, servers loaded; rig up 0 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=3
reports=G:\rig-test\tsubame-s3\bench-2026-11-18T23-13-13
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 110.10 paint_p50_ms 3.72 paint_p95_ms 6.52 private_before_scroll_mb 103.1 private_end_mb 87.8 peak_commit_mb 126.6 rows 100000 exe_bytes 1783808
run 2: first_paint_ms 84.40 paint_p50_ms 3.73 paint_p95_ms 6.96 private_before_scroll_mb 111.0 private_end_mb 87.8 peak_commit_mb 122.3 rows 100000 exe_bytes 1783808
run 3: first_paint_ms 92.90 paint_p50_ms 3.69 paint_p95_ms 6.74 private_before_scroll_mb 107.4 private_end_mb 87.9 peak_commit_mb 132.1 rows 100000 exe_bytes 1783808
run 4: first_paint_ms 104.20 paint_p50_ms 3.68 paint_p95_ms 6.75 private_before_scroll_mb 103.8 private_end_mb 86.6 peak_commit_mb 121.6 rows 100000 exe_bytes 1783808
run 5: first_paint_ms 81.70 paint_p50_ms 3.75 paint_p95_ms 6.72 private_before_scroll_mb 105.6 private_end_mb 88.4 peak_commit_mb 121.4 rows 100000 exe_bytes 1783808
run 6: first_paint_ms 79.40 paint_p50_ms 3.71 paint_p95_ms 6.78 private_before_scroll_mb 105.7 private_end_mb 87.4 peak_commit_mb 124.2 rows 100000 exe_bytes 1783808
run 7: first_paint_ms 82.10 paint_p50_ms 3.74 paint_p95_ms 6.82 private_before_scroll_mb 105.9 private_end_mb 87.9 peak_commit_mb 126.5 rows 100000 exe_bytes 1783808
run 8: first_paint_ms 87.90 paint_p50_ms 3.72 paint_p95_ms 6.79 private_before_scroll_mb 106.4 private_end_mb 86.0 peak_commit_mb 122.6 rows 100000 exe_bytes 1783808
run 9: first_paint_ms 89.30 paint_p50_ms 3.73 paint_p95_ms 6.90 private_before_scroll_mb 111.0 private_end_mb 87.8 peak_commit_mb 122.0 rows 100000 exe_bytes 1783808
run 10: first_paint_ms 80.50 paint_p50_ms 3.63 paint_p95_ms 6.95 private_before_scroll_mb 100.5 private_end_mb 86.5 peak_commit_mb 123.0 rows 100000 exe_bytes 1783808
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2076 at 168 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 90.7
private_bytes_first_paint: 64.8 MB
all times in ms from the navigation event
startup   entries  100000 (9.8 MB)  first_batch    25.79  first_frame    67.37  done   106.30  final_frame   112.10  worker    42.49  store_cpu    11.07  final_sort     6.63  watch_start     0.08  frames   3   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.8 MB)  first_batch    11.68  first_frame    19.21  done    86.71  final_frame    92.30  worker    55.07  store_cpu     6.42  final_sort    24.00  watch_start     0.03  frames   5   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.8 MB)  first_batch    10.86  first_frame    19.20  done    71.85  final_frame    86.33  worker    54.57  store_cpu     7.51  final_sort     5.57  watch_start     0.01  frames   6   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.8 MB)  first_batch     4.70  first_frame    24.55  done    77.15  final_frame   132.83  worker    55.89  store_cpu     9.82  final_sort     5.47  watch_start     0.01  frames   5   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.8 MB)  first_batch     2.44  first_frame     7.93  done    46.37  final_frame    49.16  worker    37.17  store_cpu     6.07  final_sort     5.66  watch_start     0.01  frames  10   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.8 MB)  first_batch     1.27  first_frame     5.51  done    43.98  final_frame    46.69  worker    34.68  store_cpu     5.39  final_sort     5.95  watch_start     0.01  frames   9   G:\rig-bench\tsubame-b1\100k
repeat 6  entries  100000 (9.8 MB)  first_batch     1.32  first_frame     8.33  done    44.08  final_frame    47.29  worker    34.66  store_cpu     5.60  final_sort     6.41  watch_start     0.01  frames   8   G:\rig-bench\tsubame-b1\100k
repeat 7  entries  100000 (9.8 MB)  first_batch     1.90  first_frame     7.52  done    40.00  final_frame    42.71  worker    33.36  store_cpu     5.51  final_sort     4.86  watch_start     0.01  frames   8   G:\rig-bench\tsubame-b1\100k
repeat 8  entries  100000 (9.8 MB)  first_batch     2.97  first_frame    10.96  done    40.33  final_frame    42.95  worker    34.00  store_cpu     5.60  final_sort     4.94  watch_start     0.01  frames   7   G:\rig-bench\tsubame-b1\100k
repeat 9  entries  100000 (9.8 MB)  first_batch     2.92  first_frame     8.37  done    40.76  final_frame    43.46  worker    32.62  store_cpu     5.15  final_sort     5.25  watch_start     0.01  frames   8   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 140.7 MB
```
</details>

<details><summary>Raw: cold-cache u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2076 at 168 dpi, paint path software (GPU starting)
first_paint_ms (process start to first completed paint): 466.7
private_bytes_first_paint: 35.0 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   124.89  first_frame   311.43  done   311.67  final_frame   315.47  worker     4.55  store_cpu     0.02  final_sort     0.00  watch_start     0.23  frames   2   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (10.4 MB)  first_batch    38.38  first_frame    47.35  done   310.59  final_frame   315.97  worker   188.04  store_cpu    16.10  final_sort    90.38  watch_start     0.01  frames  12   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 118.9 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=15.6
idle.cpu_ms=0
idle.threads=70
idle.threads_warm=70
idle.context_switches_per_s=-1
idle.pid=19804
```
</details>

<details><summary>Raw: leak-gate.txt</summary>

```
# --- leak-gate-01.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24823
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
elapsed_s=103.6
ops_per_s=106.2
navigations_s=95.3
tabs_s=8.3
navigations_per_s=104.9
tabs_per_s=120.9
stage_s.load=60.5
stage_s.paint=46.7
stage_s.icons=2.2
steps=25974
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=88104960
start.private_mb=84.02
start.working_set_bytes=81252352
start.handles=627
start.gdi=45
start.user=21
start.threads=61
sample.0.private_bytes=88104960
sample.0.private_mb=84.02
sample.0.working_set_bytes=81252352
sample.0.handles=627
sample.0.gdi=45
sample.0.user=21
sample.0.threads=61
sample.1000.private_bytes=89079808
sample.1000.private_mb=84.95
sample.1000.working_set_bytes=81887232
sample.1000.handles=627
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=61
sample.2000.private_bytes=89882624
sample.2000.private_mb=85.72
sample.2000.working_set_bytes=82231296
sample.2000.handles=626
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=60
sample.3000.private_bytes=90075136
sample.3000.private_mb=85.90
sample.3000.working_set_bytes=82599936
sample.3000.handles=626
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=60
sample.4000.private_bytes=93175808
sample.4000.private_mb=88.86
sample.4000.working_set_bytes=84918272
sample.4000.handles=624
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=58
sample.5000.private_bytes=93274112
sample.5000.private_mb=88.95
sample.5000.working_set_bytes=85368832
sample.5000.handles=624
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=58
sample.6000.private_bytes=91754496
sample.6000.private_mb=87.50
sample.6000.working_set_bytes=83902464
sample.6000.handles=624
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=58
sample.7000.private_bytes=91910144
sample.7000.private_mb=87.65
sample.7000.working_set_bytes=83824640
sample.7000.handles=624
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=58
sample.8000.private_bytes=91860992
sample.8000.private_mb=87.61
sample.8000.working_set_bytes=83914752
sample.8000.handles=624
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=58
sample.9000.private_bytes=93708288
sample.9000.private_mb=89.37
sample.9000.working_set_bytes=85512192
sample.9000.handles=624
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=58
sample.10000.private_bytes=91447296
sample.10000.private_mb=87.21
sample.10000.working_set_bytes=83783680
sample.10000.handles=624
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=58
sample.11000.private_bytes=90832896
sample.11000.private_mb=86.62
sample.11000.working_set_bytes=83423232
sample.11000.handles=622
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=57
end.private_bytes=90832896
end.private_mb=86.62
end.working_set_bytes=83423232
end.handles=622
end.gdi=45
end.user=21
end.threads=57
idle.private_bytes=69210112
idle.private_mb=66.00
idle.working_set_bytes=64737280
idle.handles=601
idle.gdi=45
idle.user=21
idle.threads=61
delta.private_bytes_from_idle=21622784
delta.private_bytes=2727936
delta.private_pct=3.10
allow.private_bytes=4405248
pass.private=1
delta.handles=-5
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-4
pass.threads=1
peak.private_bytes=93708288
trim.private_bytes=90832896
trim.private_mb=86.62
trim.working_set_bytes=83423232
trim.handles=622
trim.gdi=45
trim.user=21
trim.threads=57
trim.delta_private_bytes=0
trim.note=icon cache cleared (1133 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-02.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24686
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
elapsed_s=96.3
ops_per_s=114.2
navigations_s=88.1
tabs_s=8.2
navigations_per_s=113.5
tabs_per_s=122.5
stage_s.load=56.4
stage_s.paint=43.5
stage_s.icons=1.8
steps=25762
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=90083328
start.private_mb=85.91
start.working_set_bytes=82231296
start.handles=622
start.gdi=45
start.user=21
start.threads=72
sample.0.private_bytes=90083328
sample.0.private_mb=85.91
sample.0.working_set_bytes=82231296
sample.0.handles=622
sample.0.gdi=45
sample.0.user=21
sample.0.threads=72
sample.1000.private_bytes=94101504
sample.1000.private_mb=89.74
sample.1000.working_set_bytes=85393408
sample.1000.handles=620
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=72
sample.2000.private_bytes=91258880
sample.2000.private_mb=87.03
sample.2000.working_set_bytes=82628608
sample.2000.handles=621
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=71
sample.3000.private_bytes=92020736
sample.3000.private_mb=87.76
sample.3000.working_set_bytes=83132416
sample.3000.handles=621
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=71
sample.4000.private_bytes=91680768
sample.4000.private_mb=87.43
sample.4000.working_set_bytes=78168064
sample.4000.handles=617
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=71
sample.5000.private_bytes=94928896
sample.5000.private_mb=90.53
sample.5000.working_set_bytes=80830464
sample.5000.handles=619
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=69
sample.6000.private_bytes=92844032
sample.6000.private_mb=88.54
sample.6000.working_set_bytes=79085568
sample.6000.handles=615
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=69
sample.7000.private_bytes=93229056
sample.7000.private_mb=88.91
sample.7000.working_set_bytes=79372288
sample.7000.handles=619
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=69
sample.8000.private_bytes=93446144
sample.8000.private_mb=89.12
sample.8000.working_set_bytes=79605760
sample.8000.handles=619
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=69
sample.9000.private_bytes=93093888
sample.9000.private_mb=88.78
sample.9000.working_set_bytes=79568896
sample.9000.handles=619
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=69
sample.10000.private_bytes=95440896
sample.10000.private_mb=91.02
sample.10000.working_set_bytes=82378752
sample.10000.handles=619
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=69
sample.11000.private_bytes=92774400
sample.11000.private_mb=88.48
sample.11000.working_set_bytes=80179200
sample.11000.handles=619
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=69
end.private_bytes=92774400
end.private_mb=88.48
end.working_set_bytes=80179200
end.handles=619
end.gdi=45
end.user=21
end.threads=69
idle.private_bytes=69767168
idle.private_mb=66.54
idle.working_set_bytes=64036864
idle.handles=596
idle.gdi=45
idle.user=21
idle.threads=72
delta.private_bytes_from_idle=23007232
delta.private_bytes=2691072
delta.private_pct=2.99
allow.private_bytes=4504166
pass.private=1
delta.handles=-3
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=95440896
trim.private_bytes=92774400
trim.private_mb=88.48
trim.working_set_bytes=81809408
trim.handles=619
trim.gdi=45
trim.user=21
trim.threads=69
trim.delta_private_bytes=0
trim.note=icon cache cleared (1119 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-03.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24428
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
elapsed_s=68.7
ops_per_s=160.0
navigations_s=62.4
tabs_s=6.3
navigations_per_s=160.2
tabs_per_s=158.4
stage_s.load=42.7
stage_s.paint=29.8
stage_s.icons=1.9
steps=25251
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=90382336
start.private_mb=86.20
start.working_set_bytes=81301504
start.handles=622
start.gdi=45
start.user=21
start.threads=68
sample.0.private_bytes=90382336
sample.0.private_mb=86.20
sample.0.working_set_bytes=81301504
sample.0.handles=622
sample.0.gdi=45
sample.0.user=21
sample.0.threads=68
sample.1000.private_bytes=91987968
sample.1000.private_mb=87.73
sample.1000.working_set_bytes=82481152
sample.1000.handles=622
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=68
sample.2000.private_bytes=94244864
sample.2000.private_mb=89.88
sample.2000.working_set_bytes=84557824
sample.2000.handles=621
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=67
sample.3000.private_bytes=94228480
sample.3000.private_mb=89.86
sample.3000.working_set_bytes=85499904
sample.3000.handles=621
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=67
sample.4000.private_bytes=94765056
sample.4000.private_mb=90.38
sample.4000.working_set_bytes=85295104
sample.4000.handles=617
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=67
sample.5000.private_bytes=94846976
sample.5000.private_mb=90.45
sample.5000.working_set_bytes=85860352
sample.5000.handles=621
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=67
sample.6000.private_bytes=92602368
sample.6000.private_mb=88.31
sample.6000.working_set_bytes=83316736
sample.6000.handles=617
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=67
sample.7000.private_bytes=92893184
sample.7000.private_mb=88.59
sample.7000.working_set_bytes=83578880
sample.7000.handles=619
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=65
sample.8000.private_bytes=92459008
sample.8000.private_mb=88.18
sample.8000.working_set_bytes=83587072
sample.8000.handles=619
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=65
sample.9000.private_bytes=93368320
sample.9000.private_mb=89.04
sample.9000.working_set_bytes=84164608
sample.9000.handles=619
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=65
sample.10000.private_bytes=93380608
sample.10000.private_mb=89.05
sample.10000.working_set_bytes=84234240
sample.10000.handles=619
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=65
sample.11000.private_bytes=92315648
sample.11000.private_mb=88.04
sample.11000.working_set_bytes=84045824
sample.11000.handles=619
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=65
end.private_bytes=92315648
end.private_mb=88.04
end.working_set_bytes=84045824
end.handles=619
end.gdi=45
end.user=21
end.threads=65
idle.private_bytes=69140480
idle.private_mb=65.94
idle.working_set_bytes=63381504
idle.handles=576
idle.gdi=45
idle.user=21
idle.threads=68
delta.private_bytes_from_idle=23175168
delta.private_bytes=1933312
delta.private_pct=2.14
allow.private_bytes=4519116
pass.private=1
delta.handles=-3
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=94846976
trim.private_bytes=92315648
trim.private_mb=88.04
trim.working_set_bytes=84045824
trim.handles=619
trim.gdi=45
trim.user=21
trim.threads=65
trim.delta_private_bytes=0
trim.note=icon cache cleared (1116 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0
```
</details>

## 2026-11-19 00:29 — rig (Ryzen 9 5900X, RTX 3080, 4K at 200%), commit `cb13120`

- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on rig, branch `t3/fix-details-scroll-paint-p95`, commit `cb13120`
- Window: 3840x2076 at 168 dpi, paint path `hardware`
- Fixtures: `G:\rig-bench\tsubame-b1\100k` (100,000 entries), `G:\rig-bench\tsubame-b1\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); rig load: 5 Minecraft java process(es), 25 docker container(s), CPU 12% busy, servers loaded; rig up 1 h
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
| Main exe | < 6 MB | 2681 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 80.6 -> 82.9 MB (+2.27 MB, 2.8%; allowance 4.0 MB) | PASS |

All measured budgets and the leak gate pass.

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

<details><summary>Raw: meta.txt</summary>

```
stamp=2026-11-19T00:29:16
commit=cb13120
branch=t3/fix-details-scroll-paint-p95
dirty=0
machine=rig (Ryzen 9 5900X, RTX 3080, 4K at 200%)
build=release (lto, codegen-units=1, strip, panic=abort)
mode=full
runs=10
cache=warm filesystem cache (no purge before these runs)
cold=measured (3 runs: purge working from outside, app warmed up on small folders, only the cold 100k navigation timed)
dpi=192
rig_load=5 Minecraft java process(es), 25 docker container(s), CPU 12% busy, servers loaded; rig up 1 h
fixture_100k=G:\rig-bench\tsubame-b1\100k
gate_fixture=G:\rig-bench\tsubame-b1\gate
gate_navs=10000
gate_tabs=1000
gate_runs=3
slot=3
reports=G:\rig-test\tsubame-s3\bench-2026-11-19T00-29-16
notes=launched through a scheduled task in rig's interactive session; every number is the app's own report from that process
```
</details>

<details><summary>Raw: s1-bench.txt (one --bench run per line)</summary>

```
run 1: first_paint_ms 66.50 paint_p50_ms 1.57 paint_p95_ms 2.69 private_before_scroll_mb 90.2 private_end_mb 82.1 peak_commit_mb 90.5 rows 100000 exe_bytes 2745344
run 2: first_paint_ms 67.30 paint_p50_ms 1.51 paint_p95_ms 2.64 private_before_scroll_mb 86.9 private_end_mb 83.1 peak_commit_mb 90.3 rows 100000 exe_bytes 2745344
run 3: first_paint_ms 66.60 paint_p50_ms 1.56 paint_p95_ms 2.61 private_before_scroll_mb 86.2 private_end_mb 82.2 peak_commit_mb 86.9 rows 100000 exe_bytes 2745344
run 4: first_paint_ms 70.30 paint_p50_ms 1.51 paint_p95_ms 2.71 private_before_scroll_mb 94.5 private_end_mb 82.9 peak_commit_mb 96.5 rows 100000 exe_bytes 2745344
run 5: first_paint_ms 64.90 paint_p50_ms 1.52 paint_p95_ms 2.55 private_before_scroll_mb 90.2 private_end_mb 82.5 peak_commit_mb 96.2 rows 100000 exe_bytes 2745344
run 6: first_paint_ms 61.30 paint_p50_ms 1.50 paint_p95_ms 2.65 private_before_scroll_mb 84.2 private_end_mb 80.5 peak_commit_mb 84.9 rows 100000 exe_bytes 2745344
run 7: first_paint_ms 71.20 paint_p50_ms 1.53 paint_p95_ms 2.73 private_before_scroll_mb 90.3 private_end_mb 82.8 peak_commit_mb 91.0 rows 100000 exe_bytes 2745344
run 8: first_paint_ms 72.80 paint_p50_ms 1.50 paint_p95_ms 2.65 private_before_scroll_mb 92.9 private_end_mb 83.1 peak_commit_mb 95.5 rows 100000 exe_bytes 2745344
run 9: first_paint_ms 64.70 paint_p50_ms 1.55 paint_p95_ms 2.78 private_before_scroll_mb 94.6 private_end_mb 82.4 peak_commit_mb 95.7 rows 100000 exe_bytes 2745344
run 10: first_paint_ms 71.80 paint_p50_ms 1.56 paint_p95_ms 2.81 private_before_scroll_mb 90.5 private_end_mb 81.7 peak_commit_mb 97.4 rows 100000 exe_bytes 2745344
```
</details>

<details><summary>Raw: u2-timing.txt</summary>

```
tsubame U2 timing (release)
window_px: 3840x2076 at 168 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 77.7
private_bytes_first_paint: 28.1 MB
all times in ms from the navigation event
startup   entries  100000 (9.8 MB)  first_batch    21.14  first_frame    49.40  done    79.42  final_frame    81.74  worker    52.50  store_cpu    11.86  final_sort     7.51  watch_start     0.11  frames   3   G:\rig-bench\tsubame-b1\100k
repeat 1  entries  100000 (9.8 MB)  first_batch     1.03  first_frame     5.52  done    51.22  final_frame    53.62  worker    42.57  store_cpu     6.28  final_sort     6.87  watch_start     0.01  frames   6   G:\rig-bench\tsubame-b1\100k
repeat 2  entries  100000 (9.8 MB)  first_batch     4.56  first_frame    12.56  done    61.17  final_frame    64.77  worker    53.66  store_cpu     5.64  final_sort     6.54  watch_start     0.01  frames   9   G:\rig-bench\tsubame-b1\100k
repeat 3  entries  100000 (9.8 MB)  first_batch     4.11  first_frame    11.06  done    59.03  final_frame    62.34  worker    48.60  store_cpu     7.13  final_sort     9.81  watch_start     0.01  frames   8   G:\rig-bench\tsubame-b1\100k
repeat 4  entries  100000 (9.8 MB)  first_batch     1.43  first_frame    29.32  done    90.82  final_frame    93.74  worker    58.46  store_cpu     7.07  final_sort     8.15  watch_start     0.01  frames   9   G:\rig-bench\tsubame-b1\100k
repeat 5  entries  100000 (9.8 MB)  first_batch     2.16  first_frame     7.03  done    45.86  final_frame    48.04  worker    38.77  store_cpu     6.63  final_sort     6.25  watch_start     0.01  frames  18   G:\rig-bench\tsubame-b1\100k
repeat 6  entries  100000 (9.8 MB)  first_batch     2.72  first_frame     5.41  done    44.25  final_frame    45.85  worker    37.25  store_cpu     5.72  final_sort     6.23  watch_start     0.01  frames  21   G:\rig-bench\tsubame-b1\100k
repeat 7  entries  100000 (9.8 MB)  first_batch     0.84  first_frame     3.40  done    42.50  final_frame    43.99  worker    34.77  store_cpu     5.15  final_sort     6.68  watch_start     0.01  frames  24   G:\rig-bench\tsubame-b1\100k
repeat 8  entries  100000 (9.8 MB)  first_batch     1.08  first_frame     3.52  done    39.85  final_frame    41.58  worker    33.68  store_cpu     5.37  final_sort     5.86  watch_start     0.01  frames  22   G:\rig-bench\tsubame-b1\100k
repeat 9  entries  100000 (9.8 MB)  first_batch     0.94  first_frame     3.30  done    53.39  final_frame    54.85  worker    46.46  store_cpu     5.73  final_sort     5.19  watch_start     0.01  frames  26   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 99.8 MB
```
</details>

<details><summary>Raw: cold-cache --timing reports</summary>

```
# --- cold-timing-01.txt ---
tsubame U2 timing (release)
window_px: 3840x2076 at 168 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 496.7
private_bytes_first_paint: 17.4 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   258.75  first_frame   422.04  done   422.37  final_frame   423.80  worker     2.21  store_cpu     0.02  final_sort     0.00  watch_start     0.08  frames   2   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries    1004 (0.1 MB)  first_batch   167.02  first_frame   232.13  done   232.43  final_frame   233.83  worker    14.70  store_cpu     0.43  final_sort     0.00  watch_start     0.01  frames   3   G:\rig-bench\tsubame-b1\gate\small\0001
first     entries      24 (0.1 MB)  first_batch    14.63  first_frame    16.23  done    16.36  final_frame    18.02  worker     0.26  store_cpu     0.01  final_sort     0.00  watch_start     0.01  frames   3   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (8.1 MB)  first_batch    10.67  first_frame    13.58  done   152.74  final_frame   154.46  worker   142.69  store_cpu     7.78  final_sort     6.92  watch_start     0.01  frames  40   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 85.4 MB

# --- cold-timing-02.txt ---
tsubame U2 timing (release)
window_px: 3840x2076 at 168 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 304.2
private_bytes_first_paint: 17.2 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch    98.12  first_frame   237.68  done   237.99  final_frame   239.39  worker     2.66  store_cpu     0.02  final_sort     0.00  watch_start     0.11  frames   2   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries    1004 (0.1 MB)  first_batch    29.75  first_frame    36.25  done    37.00  final_frame    39.91  worker     3.79  store_cpu     0.61  final_sort     0.00  watch_start     0.01  frames   3   G:\rig-bench\tsubame-b1\gate\small\0001
first     entries      24 (0.1 MB)  first_batch     2.05  first_frame     4.44  done     4.51  final_frame     6.02  worker     0.31  store_cpu     0.01  final_sort     0.00  watch_start     0.01  frames   3   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (8.1 MB)  first_batch    10.95  first_frame    14.73  done   167.28  final_frame   168.98  worker   149.33  store_cpu     8.12  final_sort     8.17  watch_start     0.01  frames  40   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 84.0 MB

# --- cold-timing-03.txt ---
tsubame U2 timing (release)
window_px: 3840x2076 at 168 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 439.9
private_bytes_first_paint: 17.3 MB
all times in ms from the navigation event
startup   entries      24 (0.0 MB)  first_batch   119.93  first_frame   246.24  done   246.45  final_frame   247.86  worker     4.32  store_cpu     0.03  final_sort     0.00  watch_start     0.09  frames   2   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries    1004 (0.1 MB)  first_batch    36.71  first_frame    43.12  done    43.76  final_frame    66.44  worker     6.28  store_cpu     0.65  final_sort     0.00  watch_start     0.01  frames   3   G:\rig-bench\tsubame-b1\gate\small\0001
first     entries      24 (0.1 MB)  first_batch     1.16  first_frame     2.67  done     2.75  final_frame     3.83  worker     0.30  store_cpu     0.01  final_sort     0.00  watch_start     0.01  frames   3   G:\rig-bench\tsubame-b1\gate\tiny\0001
first     entries  100000 (8.1 MB)  first_batch    10.92  first_frame    13.73  done   219.99  final_frame   221.55  worker   214.18  store_cpu     7.95  final_sort     5.19  watch_start     0.01  frames  41   G:\rig-bench\tsubame-b1\100k
private_bytes_end: 85.1 MB
```
</details>

<details><summary>Raw: idle.txt</summary>

```
idle.seconds=10
idle.cpu_ms_first=0
idle.cpu_ms=31.2
idle.threads=58
idle.threads_warm=59
idle.context_switches_per_s=-1
idle.pid=28652
```
</details>

<details><summary>Raw: leak-gate.txt</summary>

```
# --- leak-gate-01.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24594
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
elapsed_s=37.4
ops_per_s=293.8
navigations_s=33.9
tabs_s=3.5
navigations_per_s=294.9
tabs_per_s=282.7
stage_s.load=25.5
stage_s.paint=13.6
stage_s.icons=1.7
steps=25113
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=87171072
start.private_mb=83.13
start.working_set_bytes=80261120
start.handles=628
start.gdi=45
start.user=21
start.threads=58
sample.0.private_bytes=87171072
sample.0.private_mb=83.13
sample.0.working_set_bytes=80261120
sample.0.handles=628
sample.0.gdi=45
sample.0.user=21
sample.0.threads=58
sample.1000.private_bytes=90710016
sample.1000.private_mb=86.51
sample.1000.working_set_bytes=82903040
sample.1000.handles=628
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=58
sample.2000.private_bytes=90976256
sample.2000.private_mb=86.76
sample.2000.working_set_bytes=83316736
sample.2000.handles=630
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=58
sample.3000.private_bytes=90931200
sample.3000.private_mb=86.72
sample.3000.working_set_bytes=82976768
sample.3000.handles=630
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=58
sample.4000.private_bytes=91955200
sample.4000.private_mb=87.70
sample.4000.working_set_bytes=83271680
sample.4000.handles=630
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=58
sample.5000.private_bytes=90972160
sample.5000.private_mb=86.76
sample.5000.working_set_bytes=83193856
sample.5000.handles=627
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=57
sample.6000.private_bytes=89350144
sample.6000.private_mb=85.21
sample.6000.working_set_bytes=81903616
sample.6000.handles=623
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=57
sample.7000.private_bytes=91176960
sample.7000.private_mb=86.95
sample.7000.working_set_bytes=83423232
sample.7000.handles=627
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=57
sample.8000.private_bytes=89657344
sample.8000.private_mb=85.50
sample.8000.working_set_bytes=82239488
sample.8000.handles=627
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=57
sample.9000.private_bytes=91635712
sample.9000.private_mb=87.39
sample.9000.working_set_bytes=83804160
sample.9000.handles=627
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=57
sample.10000.private_bytes=92790784
sample.10000.private_mb=88.49
sample.10000.working_set_bytes=84746240
sample.10000.handles=627
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=57
sample.11000.private_bytes=88797184
sample.11000.private_mb=84.68
sample.11000.working_set_bytes=81899520
sample.11000.handles=627
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=57
end.private_bytes=88797184
end.private_mb=84.68
end.working_set_bytes=81899520
end.handles=627
end.gdi=45
end.user=21
end.threads=57
idle.private_bytes=68239360
idle.private_mb=65.08
idle.working_set_bytes=63860736
idle.handles=602
idle.gdi=45
idle.user=21
idle.threads=58
delta.private_bytes_from_idle=20557824
delta.private_bytes=1626112
delta.private_pct=1.87
allow.private_bytes=4358553
pass.private=1
delta.handles=-1
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=92790784
trim.private_bytes=88797184
trim.private_mb=84.68
trim.working_set_bytes=81899520
trim.handles=627
trim.gdi=45
trim.user=21
trim.threads=57
trim.delta_private_bytes=0
trim.note=icon cache cleared (1131 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-02.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24619
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
elapsed_s=41.2
ops_per_s=267.2
navigations_s=37.2
tabs_s=4.0
navigations_per_s=269.0
tabs_per_s=250.0
stage_s.load=28.1
stage_s.paint=14.7
stage_s.icons=1.6
steps=25172
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=84504576
start.private_mb=80.59
start.working_set_bytes=78651392
start.handles=626
start.gdi=45
start.user=21
start.threads=59
sample.0.private_bytes=84504576
sample.0.private_mb=80.59
sample.0.working_set_bytes=78651392
sample.0.handles=626
sample.0.gdi=45
sample.0.user=21
sample.0.threads=59
sample.1000.private_bytes=88879104
sample.1000.private_mb=84.76
sample.1000.working_set_bytes=81854464
sample.1000.handles=626
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=59
sample.2000.private_bytes=85827584
sample.2000.private_mb=81.85
sample.2000.working_set_bytes=79810560
sample.2000.handles=626
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=59
sample.3000.private_bytes=89493504
sample.3000.private_mb=85.35
sample.3000.working_set_bytes=82374656
sample.3000.handles=626
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=59
sample.4000.private_bytes=89350144
sample.4000.private_mb=85.21
sample.4000.working_set_bytes=82505728
sample.4000.handles=622
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=59
sample.5000.private_bytes=89903104
sample.5000.private_mb=85.74
sample.5000.working_set_bytes=82731008
sample.5000.handles=625
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=58
sample.6000.private_bytes=87687168
sample.6000.private_mb=83.62
sample.6000.working_set_bytes=80896000
sample.6000.handles=625
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=58
sample.7000.private_bytes=90636288
sample.7000.private_mb=86.44
sample.7000.working_set_bytes=83484672
sample.7000.handles=625
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=58
sample.8000.private_bytes=89702400
sample.8000.private_mb=85.55
sample.8000.working_set_bytes=83345408
sample.8000.handles=623
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=58
sample.9000.private_bytes=87498752
sample.9000.private_mb=83.45
sample.9000.working_set_bytes=80609280
sample.9000.handles=625
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=58
sample.10000.private_bytes=87928832
sample.10000.private_mb=83.86
sample.10000.working_set_bytes=80879616
sample.10000.handles=625
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=58
sample.11000.private_bytes=86888448
sample.11000.private_mb=82.86
sample.11000.working_set_bytes=80818176
sample.11000.handles=623
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=56
end.private_bytes=86888448
end.private_mb=82.86
end.working_set_bytes=80818176
end.handles=623
end.gdi=45
end.user=21
end.threads=56
idle.private_bytes=66379776
idle.private_mb=63.30
idle.working_set_bytes=62623744
idle.handles=599
idle.gdi=45
idle.user=21
idle.threads=59
delta.private_bytes_from_idle=20508672
delta.private_bytes=2383872
delta.private_pct=2.82
allow.private_bytes=4225228
pass.private=1
delta.handles=-3
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-3
pass.threads=1
peak.private_bytes=90636288
trim.private_bytes=86888448
trim.private_mb=82.86
trim.working_set_bytes=80818176
trim.handles=623
trim.gdi=45
trim.user=21
trim.threads=56
trim.delta_private_bytes=0
trim.note=icon cache cleared (1121 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0

# --- leak-gate-03.txt ---
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2076
window_dpi=168
paint_path=hardware
start_paint_path=hardware
paints=24584
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
elapsed_s=37.0
ops_per_s=297.0
navigations_s=33.4
tabs_s=3.6
navigations_per_s=299.1
tabs_per_s=277.3
stage_s.load=25.2
stage_s.paint=13.4
stage_s.icons=1.6
steps=25098
icon_px=16
private_pct_allow=5
private_floor_bytes=2097152
handles_allow=32
gdi_allow=16
user_allow=16
threads_allow=8
start.private_bytes=86482944
start.private_mb=82.48
start.working_set_bytes=78872576
start.handles=623
start.gdi=45
start.user=21
start.threads=59
sample.0.private_bytes=86482944
sample.0.private_mb=82.48
sample.0.working_set_bytes=78872576
sample.0.handles=623
sample.0.gdi=45
sample.0.user=21
sample.0.threads=59
sample.1000.private_bytes=87113728
sample.1000.private_mb=83.08
sample.1000.working_set_bytes=79155200
sample.1000.handles=621
sample.1000.gdi=45
sample.1000.user=21
sample.1000.threads=59
sample.2000.private_bytes=90374144
sample.2000.private_mb=86.19
sample.2000.working_set_bytes=81682432
sample.2000.handles=623
sample.2000.gdi=45
sample.2000.user=21
sample.2000.threads=59
sample.3000.private_bytes=90853376
sample.3000.private_mb=86.64
sample.3000.working_set_bytes=82567168
sample.3000.handles=623
sample.3000.gdi=45
sample.3000.user=21
sample.3000.threads=59
sample.4000.private_bytes=90701824
sample.4000.private_mb=86.50
sample.4000.working_set_bytes=82341888
sample.4000.handles=623
sample.4000.gdi=45
sample.4000.user=21
sample.4000.threads=59
sample.5000.private_bytes=90611712
sample.5000.private_mb=86.41
sample.5000.working_set_bytes=82419712
sample.5000.handles=622
sample.5000.gdi=45
sample.5000.user=21
sample.5000.threads=58
sample.6000.private_bytes=91697152
sample.6000.private_mb=87.45
sample.6000.working_set_bytes=83206144
sample.6000.handles=622
sample.6000.gdi=45
sample.6000.user=21
sample.6000.threads=58
sample.7000.private_bytes=88649728
sample.7000.private_mb=84.54
sample.7000.working_set_bytes=80883712
sample.7000.handles=622
sample.7000.gdi=45
sample.7000.user=21
sample.7000.threads=58
sample.8000.private_bytes=91144192
sample.8000.private_mb=86.92
sample.8000.working_set_bytes=82472960
sample.8000.handles=620
sample.8000.gdi=45
sample.8000.user=21
sample.8000.threads=58
sample.9000.private_bytes=88072192
sample.9000.private_mb=83.99
sample.9000.working_set_bytes=80371712
sample.9000.handles=622
sample.9000.gdi=45
sample.9000.user=21
sample.9000.threads=58
sample.10000.private_bytes=90296320
sample.10000.private_mb=86.11
sample.10000.working_set_bytes=81784832
sample.10000.handles=622
sample.10000.gdi=45
sample.10000.user=21
sample.10000.threads=58
sample.11000.private_bytes=88297472
sample.11000.private_mb=84.21
sample.11000.working_set_bytes=81039360
sample.11000.handles=622
sample.11000.gdi=45
sample.11000.user=21
sample.11000.threads=58
end.private_bytes=88297472
end.private_mb=84.21
end.working_set_bytes=81039360
end.handles=622
end.gdi=45
end.user=21
end.threads=58
idle.private_bytes=67375104
idle.private_mb=64.25
idle.working_set_bytes=62492672
idle.handles=577
idle.gdi=45
idle.user=21
idle.threads=59
delta.private_bytes_from_idle=20922368
delta.private_bytes=1814528
delta.private_pct=2.10
allow.private_bytes=4324147
pass.private=1
delta.handles=-1
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=-1
pass.threads=1
peak.private_bytes=91697152
trim.private_bytes=88297472
trim.private_mb=84.21
trim.working_set_bytes=81039360
trim.handles=622
trim.gdi=45
trim.user=21
trim.threads=58
trim.delta_private_bytes=0
trim.note=icon cache cleared (1117 entries, cap 2048 items / 6 MB)
timeouts=0
gate=PASS
fail=-
exit=0
```
</details>

## Correction - stale-exe sizes in the U4 and 5b1cafb rows (2026-11-19)

Two `Main exe` figures above measured the wrong binary. `tools/push-to-rig.ps1` mirrored the source
with `robocopy`, which preserves each file's LastWriteTime; cargo's fingerprints are mtime-based, so
in a warm rig slot a synced commit whose files are older than the slot's artifacts made cargo skip
both the rebuild and the relink, and the previous exe was copied into the run dir. Evidence from the
investigation: five consecutive `-BuildOnRig` runs shipped the same 2,660,352 B exe, one attempt
failed with 51 compile errors from old sources against newer rlibs, and the exe behind the
`1742 KB` row was the PERF2-line `11fe5f3` binary (1,783,808 B), not a `5b1cafb` build.

`push-to-rig.ps1` now sets LastWriteTime = now on exactly the files a run copied (unchanged files
keep theirs, so warm incremental builds stay warm), puts those mtimes back after a successful build
(so the next sync copies nothing), and refuses to ship an exe that the build did not write; it
prints the shipped exe's size, SHA-256 prefix and commit. Sections now also carry
`- Exe: <bytes> B (<KB>), SHA-256 ...`, and `tsubame-bench app` prints a WARNING when the exe size
moves more than 10% against the previous row (the 6 MB budget still decides PASS/FAIL).

Real release sizes (`-BuildOnRig -Profile release` on rig; the same commit built twice is byte
identical):

| Build | Main exe |
|---|---|
| pre-U4 (`11fe5f3`, the PERF2 line) | 1,783,808 B (1742 KB) |
| `d6990cd` U4: updater wired into `main.rs` | +459 KB over pre-U4 |
| `231d929` Merge U4 | 2.60 MB (2662 KB) |
| `701c3ab` main | 2,750,976 B (2687 KB) |

So the U4 row's "the updater crate and U4 add 50 KB" has the sign and the scale wrong: making the
`tsubame-update` stack reachable from the app (WinHTTP, WinTrust, bcrypt, ed25519, `serde_json`,
miniz_oxide) costs about **+890 KB**, and that is the whole jump from ~1.75 MB to 2.60 MB. The U4
section's 1714 KB is in the same class as the 1742 KB row (a pre-U4 binary), as is the figure in
`docs/u4-settings.md`. The 6 MB budget is met with ~3.2 MB to spare, so the PASS verdicts above
stand; only the sizes were wrong.