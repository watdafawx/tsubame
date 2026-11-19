# CM4+CM6 budget run (release, 4K at 200%), commit `83c307e`

Raw harness output, trimmed to the summary and leak-gate table.
- Mode: full (10 cold-start runs, 10 scroll runs)
- Build: release (lto, codegen-units=1, strip, panic=abort) on test host, branch `t3/cm4-cm6-menu-input-lazy-submenus`, commit `83c307e` (worktree dirty only from report files)
- Exe: 2887680 B (2820 KB), SHA-256 `3dc29a397d945a7ee9d24117cff94edcdd55635dc5c00c23907ba53884ca576a`
- Window: 3840x2076 at 168 dpi, paint path `hardware`
- Fixtures: `<bench root>\100k` (100,000 entries), `<bench root>\gate` (204 folders, mixed sizes)
- Cache: warm filesystem cache (no purge before these runs); test host had background services running, CPU 5% busy
- Cold-cache 100k open: not measured (purge working needs elevation on test host: the ssh session and gsudo -n were refused)
- Idle: 0 ms of CPU over the first idle window, 78 ms over the second (the table uses the second); thread context switches: not measured (Get-Counter has no \Thread instances for this process)
- Memory context: 29.7 MB private at the first painted frame, 98.7 MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 61.40 ms median, 68.50 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 4.59 ms median, 11.91 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 43.34 ms median, 66.17 ms p95 (n=9) | PASS |
| 100,000-entry folder, cold cache | < 30 / < 200 ms | not measured (not measured (purge working needs elevation on test host: the ssh session and gsudo -n were refused)) | n/a |
| Scroll paint p95, 4K, details view | < 5.0 ms | 2.44 ms median, 2.76 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 78 ms | **FAIL** |
| Private bytes, one tab, small folder, 4K | < 110 MB | 63.0 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 81.6 MB median (83.9 MB max) | PASS |
| Main exe | < 6 MB | 2820 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 82.2 -> 83.9 MB (+1.74 MB, 2.1%; allowance 4.1 MB) | PASS |

**1 budget(s) missed: Idle CPU over the idle window.** Reported as measured; nothing was relaxed to make them pass.
Not measured: 100,000-entry folder, cold cache (not measured (purge working needs elevation on test host: the ssh session and gsudo -n were refused)).

Informational: startup navigation first rows 37.54 ms and complete+sorted 63.46 ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).

### Leak gate evidence

10000 navigations over 204 folders and 1000 tab open/close (with a scroll) in one process, 12 samples every 1000 operations, 5 s settle. Gate: **PASS**
The start sample is the steady state: 408 warm-up navigations and 200 warm-up tabs run first, then the window shows the fixture's first folder for 5 s. From the idle one-tab sample (63.0 MB private, before the warm-up) to the end the process grew 21.0 MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.

Worst run (single run):

| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |
|---|---|---|---|---|---|---|
| start | 82.2 | 75.4 | 625 | 45 | 21 | 61 |
| 0 ops | 82.2 | 75.4 | 625 | 45 | 21 | 61 |
| 1000 ops | 83.1 | 76.0 | 624 | 45 | 21 | 60 |
| 2000 ops | 83.6 | 76.4 | 626 | 45 | 22 | 60 |
| 3000 ops | 83.9 | 76.8 | 624 | 45 | 22 | 58 |
| 4000 ops | 86.2 | 78.6 | 620 | 45 | 22 | 58 |
| 5000 ops | 86.8 | 79.2 | 624 | 45 | 22 | 58 |
| 6000 ops | 85.0 | 77.7 | 618 | 45 | 22 | 57 |
| 7000 ops | 84.4 | 77.5 | 620 | 45 | 22 | 35 |
| 8000 ops | 84.7 | 77.6 | 622 | 45 | 23 | 35 |
| 9000 ops | 84.4 | 77.7 | 622 | 45 | 23 | 35 |
| 10000 ops | 84.7 | 77.9 | 622 | 45 | 23 | 35 |
| 11000 ops | 83.9 | 77.6 | 622 | 45 | 23 | 35 |
| after settle | 83.9 | 77.6 | 622 | 45 | 23 | 35 |
| after icon-cache trim | 83.9 | | | | | |

