# Directory enumerator (C2)

`crates/tsubame-win/src/enumerate/` — the real lister behind every folder view.
It replaces the S4 stopgap `enumerate.rs` (whose `list_dir` is kept, unchanged, as
`enumerate/simple.rs` and re-exported, because `src/data.rs` still uses it).

Design follows `docs/spike-enum.md`: `NtQueryDirectoryFile` with
`FileIdExtdDirectoryInformation` (class 60) and a **64 KB** query buffer, streamed
in batches to a channel; `FindFirstFileExW(FindExInfoBasic,
FIND_FIRST_EX_LARGE_FETCH)` only as the fallback. Nothing here touches COM, and
nothing runs on the UI thread.

## API

```rust
let token = CancelToken::new();            // one per tab/pane
let gen = token.next_generation();         // cancels the older request
let wake = Arc::new(move || unsafe { PostMessageW(hwnd, WM_APP_ENUM, 0, 0) });
let (enumerator, rx) = Enumerator::start_channel(dir, gen, token, wake);

// UI thread, on the wake message:
while let Ok(msg) = rx.try_recv() {
    match msg {
        Message::Batch(b) => store.extend(b),          // drop if b.generation != current
        Message::Done(d) => { /* d.total, d.elapsed, d.error, d.cancelled */ }
    }
}
```

- [`Enumerator::start`] / `start_channel` spawn one worker thread per request.
  `start_with(.., Options { method })` forces `Method::Nt` or `Method::Find`
  (tests only; `Method::Auto` is the production path).
- The consumer is a [`Sink`] (`mpsc::Sender<Message>` implements it): `send` is
  called on the worker thread and returns `false` when the receiver is gone, which
  stops the request. A custom sink can record or throttle.
- `wake: Wake` (`Arc<dyn Fn() + Send + Sync>`) is called after **every** delivered
  message; the app's implementation is a `PostMessageW` and nothing else.
- [`Batch`]: `generation`, `names: Vec<u16>` (one contiguous buffer per batch) and
  `recs: Vec<RawEntry>`. No per-entry allocation: two `Vec`s per batch.
  `Batch::name(i)` / `iter()` index through `name_off`/`name_len`.
- [`RawEntry`] (64 bytes, `#[repr(C)]`): `file_id: u128` (`FILE_ID_128`, 0 for the
  Find fallback), `size` (EndOfFile), `alloc_size` (AllocationSize, 0 for Find),
  `mtime` (FILETIME ticks, no conversion), `attrs` (full 32-bit attribute set),
  `name_off`, `name_len`, `reparse_tag` (`dwReserved0` on the Find path).
  `.` and `..` are dropped by the worker, so a batch never contains them.
- [`Done`]: `generation`, `total` (entries actually delivered; partial when
  cancelled), `elapsed`, `error: Option<Win32Error>`, `cancelled`,
  `find_fallback`. Always the last message of a request.

### Errors

`Win32Error { code, ntstatus }` with `kind() -> ErrorKind`. Mapped:
`AccessDenied` (5 / `STATUS_ACCESS_DENIED`), `NotFound` (2, 3 /
`STATUS_OBJECT_*NOT_FOUND`), `NotADirectory` (267 / `STATUS_NOT_A_DIRECTORY`),
`Unsupported` (the class was rejected and no fallback ran), `Other`. A file path
is rejected right after `CreateFileW` by checking `FILE_ATTRIBUTE_DIRECTORY`,
because `CreateFileW` happily opens a file. The enumerator never panics on disk
errors and never returns a partial listing as success.

## Threading

- One short-lived worker thread per request (`tsubame-enumerate`). It owns the
  directory handle from `CreateFileW` (`FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES
  | SYNCHRONIZE`, share all, `FILE_FLAG_BACKUP_SEMANTICS`, no
  `FILE_FLAG_OPEN_REPARSE_POINT` so junctions are followed). The handle is an RAII
  guard: closed on success, on error, on cancel and on a failed sink.
- Paths go through `\\?\` (and `\\?\UNC\` for UNC), so long names work; a
  path with 244-unit names beyond `MAX_PATH` is covered by a test.
- The UI thread only calls `start*`, `cancel` and `try_recv`. `join()` waits for
  the worker and is for tests/shutdown: a dead network share can hold the worker
  inside one syscall for seconds. **Dropping the `Enumerator` cancels and
  detaches**; the worker then exits on its own and closes the handle.

## Cancellation

- `CancelToken` is a single `Arc<AtomicU64>` (`next_generation`, `cancel`,
  `is_current`). A request runs while `generation()` equals its own generation:
  starting the next request cancels the previous one.
- The worker checks the token before every `NtQueryDirectoryFile` call, every 256
  entries inside a buffer and immediately before every send. Worst case a cancel
  is seen after the current syscall plus one buffer parse (tens of microseconds of
  work at 64 KB) — well under 1 ms.
- A cancelled request never flushes its pending batch: it clears it and sends only
  `Done { cancelled: true, .. }`. `total` then counts only delivered entries.
  Batches carry their generation, so a consumer that pipelines requests can drop
  everything from an older one without touching the enumerator.

## Batching

| batch | flush rule |
|---|---|
| first | 256 entries, or the end of the listing |
| later | 4,096 entries, or 4 ms after the batch started, whichever first |

The clock is read only every 256 entries, so the policy costs one `Instant::now()`
per ~256 entries. After each flush the next batch is pre-sized from the previous
one (`2x`, capped at 4,096), so a 100k listing makes ~26 batches with no
per-entry allocation.

## Method and fallback

1. `NtQueryDirectoryFile` (class 60, 64 KB `Vec<u64>` buffer for 8-byte alignment,
   `RestartScan` on the first call only). `STATUS_BUFFER_OVERFLOW` is success with
   a partial buffer and is parsed; `STATUS_NO_MORE_FILES` ends the listing and is
   also what an empty directory returns.
2. If the class is rejected (`STATUS_INVALID_INFO_CLASS`, `STATUS_INVALID_PARAMETER`,
   `STATUS_NOT_IMPLEMENTED`, `STATUS_INVALID_DEVICE_REQUEST`) **and nothing was
   delivered yet**, the request restarts with `FindFirstFileExW`. The Find path
   first opens the directory with `CreateFileW`, so a missing path and a file path
   still produce `NotFound` / `NotADirectory` instead of "empty folder". It reports
   `file_id = 0` and `alloc_size = 0`.
3. Any other NT failure is returned as a typed error; no partial listing is
   reported as complete.

## Measured (rig, NVMe `G:`)

`enum_bench` on `G:\rig-bench\tsubame-c2\100k` (100,000 entries, S2 fixture
generator, seed 20261117, empty files), release build, warm file cache, rig also
serving Minecraft and other agent builds (CPU busy 15 % at the time of the run).
Times are measured in the caller from just before `Enumerator::start` to the first
`Batch` and to `Done`, so they include thread spawn — the honest app-side number.

| method | runs | first batch (median) | done (median) | entries | batches |
|---|---|---|---|---|---|
| `Auto` (NT 64 KB) | 7 (after a discarded warm-up) | **0.50 ms** | **33.6 ms** | 100,000 | 26 |
| `Find` (fallback) | 3 | 0.59 ms | 32.2 ms | 100,000 | 26 |

Budget check, `docs/PLAN.md` §2 (100,000-entry folder on NVMe, warm):

| metric | budget | measured | verdict |
|---|---|---|---|
| first rows (first batch) | < 30 ms | 0.50 ms | **PASS** (60x headroom) |
| complete listing | < 200 ms | 33.6 ms | **PASS** (6x headroom) |
| complete **and sorted** | < 200 ms | 33.6 ms + C1's sort (S2: 11.5 ms) | **PASS** (~45 ms) |

For comparison, S2's raw method (one big 48-byte-record store, no batching, no
channel) measured 0.4 ms / 35.2 ms warm for the same folder: the streaming
enumerator adds ~0.1 ms of first-batch latency and is within noise on the total
(33.6 vs 35.2 ms here), while delivering the first rows to the UI at 0.5 ms.

S2's cold numbers still apply for a first visit to a folder (64 KB buffer: 12.6 ms
first / 175 ms complete). Cold was **not** re-measured here: it needs the bench's
system-wide working-set purge (`--cold --purge-mode working`, elevated) and is an
S2 measurement of the same syscall, not a property of the batching code.

Reproduce:

```powershell
# on FOX: build the fixture generator and the example on rig (slot 2 was used)
pwsh -NoProfile -File X:\Dev\tools\push-to-rig.ps1 -Project <worktree> -BuildOnRig -Exe tsubame-bench -CargoArgs --package=tsubame-bench -Profile release
ssh rig "G:\rig-test\tsubame-s2\tsubame-bench.exe gen --root G:\rig-bench\tsubame-c2 --sizes 100k --seed 20261117"
# (cargo can only take one target filter per build; run the example build directly in the slot)
ssh rig 'Set-Location ''G:\rig-build\tsubame-s2''; cargo build -j 12 --release --package=tsubame-win --example enum_bench'
ssh rig 'G:\rig-build\tsubame-s2\target\release\examples\enum_bench.exe G:\rig-bench\tsubame-c2\100k --runs 7'
pwsh -NoProfile -File X:\Dev\tools\push-to-rig.ps1 -Project <worktree> -Stop -Slot 2
# fixture G:\rig-bench\tsubame-c2 deleted after the run
```

## Tests (`cargo test -p tsubame-win`)

`src/enumerate/tests.rs` (no disk): path shaping (`\\?\`, `\\?\UNC\`, drive root,
`dir\*` pattern), dot handling, the 256/4,096/4 ms batch rules, batch name
offsets, `RawEntry` size, error mapping, cancel-token generations.

`tests/enumerate.rs` (temp-dir fixtures, removed on drop):

- 0, 1, 10 and 5,000 files (counts, sizes, first batch exactly 256 for 256+);
- unicode names, a 244-unit long name, sub-directories (hidden bit, reparse bit,
  junction created with a fallback to `mklink /J`, skipped gracefully when the
  process may not create one);
- nonexistent path → `NotFound`, a file path → `NotADirectory` (both methods);
- cancel mid-listing on 5,000 entries with a sink that blocks the worker: no batch
  after the cancel, `Done { cancelled: true }`, `total` equals what was delivered;
- NT and Find agree on names, attributes and sizes (and ids are 0 only for Find);
- every batch: contiguous `name_off`s, no NUL terminators, spans cover `names`;
- generations on every message;
- dropping the enumerator makes the worker finish and report `Done`;
- 500 start/cancel cycles in an **isolated child process** (other tests spawn `cmd`
  and hold transient handles) leave `GetProcessHandleCount` unchanged. The count
  moves +2 over the first ~100 cycles of a fresh process (Windows lazy allocation:
  measured, then flat for the remaining 1,400 cycles), so the test proves the
  count is stable over 100-cycle rounds before measuring the 500.

Skipped on purpose:

- **Access denied** is mapped (`ErrorKind::AccessDenied`, `STATUS_ACCESS_DENIED`)
  but not exercised by a test: it needs ACL manipulation on a fixture, which can
  leave a locked directory behind in the temp folder.
- **Cold** measurement, see above.
- `Method::Nt` on a filesystem that rejects class 60: no such share is available
  on rig, so the fallback is covered via `Method::Find` plus the code path guard
  (fallback only when nothing was delivered).
