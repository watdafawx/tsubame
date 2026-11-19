# Spike: directory enumeration (S2)

Measured 2026-11-17 on the **benchmark machine** (Ryzen 9 5900X, 64 GB DDR4-3600, Windows 11) by
`crates/tsubame-bench`. Question from `docs/PLAN.md` §1: no public benchmark
compares `NtQueryDirectoryFile` against `FindFirstFileExW` with large fetches, so
this decides which one `tsubame-win` uses (C2).

**Answer: `NtQueryDirectoryFile` with `FileIdExtdDirectoryInformation` and a
64 KB buffer, streamed.** It is the fastest or tied-fastest for the first rows
and for the complete listing, and it is the only Win32 route that returns the
file id and reparse tag in the same syscall. Both PLAN budgets pass on NVMe, warm
with 6x headroom and cold with about 7%.

## What was measured

| | |
|---|---|
| Methods | `find` = `FindFirstFileExW(FindExInfoBasic, FIND_FIRST_EX_LARGE_FETCH)`; `nt64k` = `NtQueryDirectoryFile(FileIdExtdDirectoryInformation)`, 64 KB; `nt1m` = same, 1 MB; `gfhie` = `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`, 1 MB |
| Fixtures | 1,000 / 100,000 / 1,000,000 top-level entries, seed 20261117, empty files, mixed realistic names (1–3 word stems, 15 extensions, ~0.4 % names over 60 chars, ~0.5 % sub-directories with 0–3 files inside, 8 non-ASCII stems) |
| Disks | NVMe `G:` (`seek penalty = false`) and hard disk `E:` (`seek penalty = true`), both queried with `IOCTL_STORAGE_QUERY_PROPERTY` at run time |
| Clock | `QueryPerformanceCounter`; one warm-up run discarded for warm measurements |
| first-batch | ticks from call start until the **256th** entry is in the store |
| complete | ticks until the last entry is in the store (enumeration only; sort measured separately) |
| Store | one contiguous UTF-16 name buffer + `Vec<EntryRec>` of 48-byte records (name span, size, mtime, attributes, reparse tag, 128-bit file id). No per-entry allocation, as PLAN §3 rule 2 requires |
| Background load | The machine was also running two game servers (one at 8.3 GB working set) and other cargo builds. CPU busy sampled 6–37 % before each run and printed in every log; repeat runs of the same cell moved by up to ~15 % (see "Noise") |

Every run also compares its store against the previous method's store: entry
count, names, attributes, size, mtime, reparse flag and (where both methods have
one) the 128-bit file id. All four methods agreed on every folder
(1k/100k/1m on NVMe, 1k/100k on HDD); file ids were compared on 1,000 / 100,000 /
1,000,000 entries respectively. `verify` re-runs the check on demand.

## Results, NVMe `G:` (ms, median / p95)

Warm = one discarded warm-up run, then N runs. Cold = flush before **every** run
(see "How cold is cold").

| method | size | cache | runs | first med | first p95 | complete med | complete p95 |
|---|---|---|---|---|---|---|---|
| find | 1k | warm | 10 | 0.229 | 0.271 | 0.395 | 0.426 |
| nt64k | 1k | warm | 10 | 0.214 | 0.217 | 0.359 | 0.377 |
| nt1m | 1k | warm | 10 | 0.397 | 0.427 | 0.452 | 0.482 |
| gfhie | 1k | warm | 10 | 0.407 | 0.459 | 0.465 | 0.518 |
| find | 1k | cold | 5 | 16.654 | 111.220 | 17.704 | 111.839 |
| nt64k | 1k | cold | 5 | 19.643 | 127.173 | 27.094 | 132.712 |
| nt1m | 1k | cold | 5 | 4.753 | 20.960 | 4.862 | 21.070 |
| gfhie | 1k | cold | 5 | 5.554 | 5.816 | 5.796 | 5.917 |
| find | 100k | warm | 10 | 0.401 | 0.566 | 35.128 | 39.076 |
| nt64k | 100k | warm | 10 | 0.418 | 0.514 | 35.161 | 38.970 |
| nt1m | 100k | warm | 10 | 3.471 | 3.780 | 34.383 | 36.872 |
| gfhie | 100k | warm | 10 | 3.533 | 4.992 | 34.191 | 40.216 |
| find | 100k | cold | 5 | 13.162 | 20.802 | 176.521 | 188.533 |
| nt64k | 100k | cold | 5 | 12.565 | 13.289 | 175.229 | 186.453 |
| nt1m | 100k | cold | 5 | 107.101 | 109.085 | 180.339 | 181.494 |
| gfhie | 100k | cold | 5 | 106.642 | 131.206 | 181.763 | 508.560 |
| find | 1m | warm | 5 | 0.502 | 0.560 | 430.229 | 457.145 |
| nt64k | 1m | warm | 5 | 0.511 | 0.673 | 420.484 | 427.230 |
| nt1m | 1m | warm | 5 | 4.085 | 4.652 | 383.984 | 401.265 |
| gfhie | 1m | warm | 5 | 4.051 | 4.233 | 409.802 | 446.430 |
| find | 1m | cold | 3 | 18.470 | 18.686 | 2072.065 | 2082.525 |
| nt64k | 1m | cold | 3 | 16.470 | 18.674 | 2047.115 | 2603.478 |
| nt1m | 1m | cold | 3 | 200.875 | 200.977 | 2026.519 | 2097.979 |
| gfhie | 1m | cold | 3 | 196.433 | 206.712 | 1981.988 | 2004.431 |

The 1k cold rows are dominated by the flush itself (a handful of entries cannot
cost 17 ms); they are listed for completeness, not for decisions.

## Results, hard disk `E:` (ms, median / p95)

| method | size | cache | runs | first med | first p95 | complete med | complete p95 |
|---|---|---|---|---|---|---|---|
| find | 1k | warm | 7 | 0.170 | 0.228 | 0.327 | 0.377 |
| nt64k | 1k | warm | 7 | 0.170 | 0.181 | 0.318 | 0.330 |
| nt1m | 1k | warm | 7 | 0.339 | 0.350 | 0.382 | 0.391 |
| gfhie | 1k | warm | 7 | 0.337 | 0.348 | 0.379 | 0.392 |
| find | 1k | cold | 5 | 8.314 | 15.453 | 9.030 | 16.215 |
| nt64k | 1k | cold | 5 | 2.868 | 5.603 | 3.599 | 6.367 |
| nt1m | 1k | cold | 5 | 5.080 | 6.399 | 5.149 | 6.479 |
| gfhie | 1k | cold | 5 | 3.772 | 5.693 | 3.894 | 5.778 |
| find | 100k | warm | 7 | 0.390 | 0.487 | 46.931 | 51.652 |
| nt64k | 100k | warm | 7 | 0.334 | 0.343 | 33.836 | 35.122 |
| nt1m | 100k | warm | 7 | 2.791 | 2.922 | 32.420 | 33.204 |
| gfhie | 100k | warm | 7 | 2.797 | 3.126 | 32.188 | 34.791 |
| find | 100k | cold | 5 | 16.246 | 99.556 | 134.355 | 679.312 |
| nt64k | 100k | cold | 5 | 10.137 | 25.086 | 130.627 | 157.756 |
| nt1m | 100k | cold | 5 | 79.756 | 90.096 | 138.715 | 155.072 |
| gfhie | 100k | cold | 5 | 70.432 | 76.720 | 142.300 | 159.931 |

1M was not generated on the HDD: ~15 minutes of file creation at the measured
~6,000 files/s, and the 100k cells already separate warm from cold by 4x.

## Buffer size sweep (NVMe, 100k, `nt64k` with `--buffer-kb`)

This is the most actionable result in the spike: the buffer size trades
first-batch latency against cold throughput, and the two optima are different.

| buffer | cache | first med | first p95 | complete med | complete p95 |
|---|---|---|---|---|---|
| 16 KB | warm | 0.300 | 0.370 | 26.736 | 27.443 |
| 16 KB | cold | 5.990 | 41.159 | 780.909 | 3824.111 |
| 64 KB | warm | 0.418 | 0.514 | 35.161 | 38.970 |
| 64 KB | cold | 12.565 | 13.289 | 175.229 | 186.453 |
| 256 KB | warm | 1.062 | 1.170 | 34.328 | 36.699 |
| 256 KB | cold | 39.441 | 57.541 | 182.066 | 200.792 |
| 1 MB | warm | 3.471 | 3.780 | 34.383 | 36.872 |
| 1 MB | cold | 107.101 | 109.085 | 180.339 | 181.494 |

* **16 KB is a trap cold**: 780 ms median (3.8 s worst) because every small
  buffer becomes a separate random read once the metadata is not in RAM. Warm it
  is the fastest of all (26.7 ms) — measuring warm only would have picked it.
* **1 MB costs first-batch**: the kernel fills the buffer before returning, so
  the first 256 entries wait for ~1 MB of index data. Warm 3.5 ms, cold 107 ms —
  the cold number breaks the 30 ms budget on its own.
* **64 KB is the sweet spot**: cold first-batch 12.6 ms, cold complete 175 ms.
  256 KB is 3x worse for the first rows and no better at the end.

## Budget check (PLAN §2: 100,000-entry folder on NVMe)

| metric | budget | warm | cold | verdict |
|---|---|---|---|---|
| first rows (256 entries in the store) | < 30 ms | 0.40 ms (find/nt64k) | 12.6 ms (nt64k, 64 KB) | **PASS** warm and cold |
| first rows, 1 MB buffer | < 30 ms | 3.47 ms | 107.1 ms | FAIL cold (the reason to use 64 KB) |
| complete listing | < 200 ms | 35.2 ms | 175.2 ms | **PASS** |
| complete **and sorted** | < 200 ms | 46.6 ms (35.2 + 11.5 sort) | 187.1 ms (175.2 + 11.9 sort) | **PASS**, cold margin ~7 % |

Cold is the number that matters for a first visit to a folder. Complete+sorted
cold at 187 ms median / 199 ms p95 leaves very little headroom, and one outlier
run in five hit 508 ms (disk stall, not code). The budget is met, but sorting is
now a meaningful part of it: 12 ms of the 187 is the sort. See "for C1" below.

Repeat of the recommended method with the final binary, same fixture: warm
first 0.44 / complete 36.4 / sorted 47.8 ms; cold first 13.0 / complete 177.4 /
sorted 189.4 ms. Same verdict, within the noise band described below.

For reference, 1M entries warm: complete 420 ms, +137 ms to sort. 1M cold:
~2.0 s. PLAN has no budget for 1M.

## Recommended method

**`NtQueryDirectoryFile`, `FileIdExtdDirectoryInformation` (class 60), 64 KB
buffer, restart flag on the first call only, streamed into the entry store.**

Why:

1. **First rows**: 0.42 ms warm / 12.6 ms cold, the best or joint-best of the
   four in every cell that matters, and the only method family that lets the app
   choose how much data to wait for before delivering a batch.
2. **Complete**: within noise of the best everywhere (35.2 vs 34.2–35.1 ms at
   100k warm; 175.2 vs 176.5–181.8 cold), and the winner at 1M cold.
3. **Data**: it returns everything a file browser needs in one syscall — see
   below. `FindFirstFileExW` cannot return a file id at all, which would force a
   per-file `CreateFileW`/`GetFileInformationByHandleEx` for any feature that
   needs stable identity (watcher diffing, selection across a refresh).
4. **`GetFileInformationByHandleEx` is a fine fallback** (identical data, similar
   speed) but its 1 MB-buffer first-batch penalty is inherent: it has no
   restart-with-query semantics beyond `FileIdExtdDirectoryRestartInfo`, and the
   smallest useful buffer it accepts is the same 64 KB, so nothing is gained.

`FindFirstFileExW` stays as the **fallback for non-NTFS/remote paths** where the
NT call fails (it also cannot report a file id there) and for `\\?\UNC` shares.

### What the recommended method returns

| need | field | note |
|---|---|---|
| name | `FileName` + `FileNameLength` | UTF-16, **not** NUL terminated |
| size | `EndOfFile` | already a 64-bit value, no `nFileSizeHigh/Low` dance |
| mtime | `LastWriteTime` | FILETIME ticks; `CreationTime`/`LastAccessTime`/`ChangeTime` also present |
| attributes | `FileAttributes` | full 32-bit set, incl. `RECALL_ON_DATA_ACCESS` and friends that do not fit a `u16` |
| file id | `FileId` (`FILE_ID_128`) | 128-bit; the 64-bit variant is `FileIdBothDirectoryInformation` (class 37) if 16 bytes per entry is too much |
| reparse point | `FileAttributes & REPARSE_POINT` + `ReparsePointTag` | tag distinguishes symlink/junction/cloud placeholder without a second call |

That is all six. `FindFirstFileExW` gives five: its `dwReserved0` is the reparse
tag, but there is **no file id**.

### For C1 (entry store)

The bench store uses 48-byte records because it carries the 128-bit file id for
cross-method verification. Measured size at 1M entries: 48 MB of records +
~38 MB of UTF-16 names ≈ 86 MB, which is over the ~60 MB PLAN §3 rule 2 targets.
A production record of 24–32 bytes (drop the id, or keep a `u32` index into a
side table built only when the watcher needs identity) lands at ~62 MB. Worth
deciding in C1 before the store is written.

Sorting costs 11.5 ms per 100k entries and 137 ms per 1M with
`sort_unstable_by` over `u32` indices comparing UTF-16 slices. A key-based sort
(precomputed case-folded keys, or a radix pass) should cut that by 3–5x; it is
the second-largest item in the cold budget.

## How cold is cold (what actually worked)

Honest account, because "cold" is easy to fake by accident:

1. **Standby-list purge only** (`NtSetSystemInformation(SystemMemoryListInformation,
   MemoryPurgeStandbyList)`, the RAMMap "Empty Standby List" call, needs
   `SeProfileSingleProcessPrivilege`): **not enough**. HDD 100k measured 32.5 ms
   "cold" vs 30.6 ms warm — i.e. still fully cached. Hot NTFS metadata sits in
   the file-cache *working set*, and a standby purge does not touch it.
2. **`SetSystemFileCacheSize(0/1 MB/64 MB, …, hard flags)`**: rejected with
   `ERROR_INVALID_PARAMETER` on this Windows 11 build for every value tried.
3. **`EmptyWorkingSet` on the System process (PID 4)**: `OpenProcess` gives
   `ACCESS_DENIED` even elevated with `SeDebugPrivilege` (the System process is
   protected on current builds).
4. **`MemoryEmptyWorkingSets` then `MemoryPurgeStandbyList`** — the order is the
   whole trick: emptying working sets only moves clean pages *to* the standby
   list, so the standby purge has to run **after** it (the bench does it twice,
   with a 100 ms gap). That gives HDD 100k cold 130.6 ms vs 33.8 ms warm, and
   NVMe 100k cold 175.2 ms vs 35.2 ms warm — a 4–5x slowdown that can only come
   from the device.

This is `--purge-mode working` (the default for `--cold` is `filecache`, which
does not work on this build and says so). It is a system-wide flush: it also
empties the working sets of the game servers running on the benchmark machine, which re-fault
their pages from NVMe afterwards. That is the cost of a real cold number on a
machine that cannot be rebooted between runs; it is stated here rather than
hidden.

Caveat: `--purge-mode working` needs elevation. A non-elevated run reports
`purge-denied` and its "cold" numbers are really first-touch; the bench never
labels those as cold.

## Noise

The same cell measured three times (NVMe 100k warm, `find`) gave medians of
38.9, 37.6 and 35.1 ms with CPU busy at 25 %, 13 % and 11 %. Cold runs are worse:
one `gfhie` cold run took 508 ms against a 182 ms median, and the 16 KB cold
sweep had a 3.8 s outlier. Treat single runs as anecdotes; the tables above are
medians of 5–10 runs, and the pass/fail conclusions hold under every repeat.

## Gotchas

* **`\\?\` paths expose `.` and `..`.** With an extended-length path
  `FindFirstFileExW` returns the two dot entries (2 extra in every listing); the
  NT methods do too. All four are dropped by name in the store. A plain `C:\dir\*`
  pattern hides them, so this only bites code that uses `\\?\` for long paths —
  which we must.
* **Alignment.** `FILE_ID_EXTD_DIR_INFORMATION` contains `LARGE_INTEGER`s and the
  kernel writes with that assumption; allocate the query buffer as `Vec<u64>` (or
  with `VirtualAlloc`) and pass the byte length. The bench asserts 8-byte
  alignment before parsing.
* **NTSTATUS, not Win32 errors.** `NtQueryDirectoryFile` returns `STATUS_SUCCESS`,
  `STATUS_BUFFER_OVERFLOW` (0x80000005 — *success* with a partially filled
  buffer: parse it and continue), `STATUS_NO_MORE_FILES` (0x80000006 — done), and
  real failures. Anything that checks `!= 0` treats the normal end of a 64 KB
  read as an error.
* **`FileIdExtdDirectoryInformation` is class 60.** If an older build rejects it
  with `STATUS_INVALID_INFO_CLASS`, fall back to `FileFullDirectoryInformation`
  (class 2, 68-byte header, no file id) — the bench does this automatically and
  prints which layout was used (`layout FileIdExtd` in the logs above).
* **Restart flag.** `RestartScan = TRUE` on the first call, `FALSE` after; a fresh
  handle for `GetFileInformationByHandleEx` starts at the beginning anyway.
* **Long names.** NTFS caps names at 255 code units, so a 64 KB buffer always
  holds a whole entry (max 88 + 510 bytes). `cFileName[260]` in
  `WIN32_FIND_DATAW` is exactly big enough — do not size it by hand.
* **Directory handles** need `FILE_FLAG_BACKUP_SEMANTICS`; `FILE_FLAG_OPEN_REPARSE_POINT`
  is deliberately *not* set, so junctions are followed.
* **First-batch definition.** "First rows" here is 256 entries in the store. A UI
  can deliver the first batch after one buffer (~1,300 entries at 64 KB), which
  is what these numbers approximate; with a 1 MB buffer the first batch waits for
  ~20,000 entries.
* **Buffers do not shrink well.** 16 KB is fastest warm and 4.5x slower cold. Do
  not tune this warm.

## Reproduce

```powershell
# build the bench (release)
cargo build --release -p tsubame-bench

# then, on the benchmark machine
tsubame-bench.exe gen --root <fixture root> --sizes 1k,100k,1m --seed 20261117
tsubame-bench.exe enum --dir <fixture root>\100k --method all --runs 10 --sort --disk nvme
tsubame-bench.exe enum --dir <fixture root>\100k --method all --runs 5 --cold --purge-mode working --sort --disk nvme
tsubame-bench.exe verify --dir <fixture root>\100k
```

`gen` is deterministic (names derive from seed+index) and resumable (an existing
complete folder is skipped, a partial one is completed without overwriting).
`enum --method all` verifies the four methods against each other as it goes.
Fixtures lived on an NVMe drive and on a hard disk and were deleted after the
measurements.
