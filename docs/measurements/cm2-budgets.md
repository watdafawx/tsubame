| Metric | Budget | Measured | Result |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 71.00 ms median, 95.70 ms p95 (n=10) | PASS |
| 100,000-entry folder: first rows | < 30 ms | 4.32 ms median, 28.44 ms p95 (n=9) | PASS |
| 100,000-entry folder: complete and sorted | < 200 ms | 51.18 ms median, 99.10 ms p95 (n=9) | PASS |
| Scroll paint p95, 4K, details view | < 5.0 ms | 2.90 ms median, 4.53 ms p95 (n=10) | PASS |
| Idle CPU over the idle window | 0 wake-ups (0 ms CPU) | 94 ms | **FAIL** |
| Private bytes, one tab, small folder, 4K | < 110 MB | 64.2 MB (idle sample before the gate's warm-up) | PASS |
| Private bytes after the 5 s scroll run (100k folder) | < 110 MB | 82.2 MB median (85.1 MB max) | PASS |
| Main exe | < 6 MB | 2792 KB | PASS |
| Leak gate: 10,000 navigations + 1,000 tabs | private bytes within 5% of the start | 81.6 -> 84.5 MB (+2.81 MB, 3.4%; allowance 4.1 MB) | PASS |
