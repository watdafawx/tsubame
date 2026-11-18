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
