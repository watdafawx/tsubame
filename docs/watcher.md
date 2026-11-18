# Directory watcher (C4)

`crates/tsubame-win/src/watch/`: watches one folder per tab with
`ReadDirectoryChangesW`, folds the raw events into a minimal diff, and applies
the diff to the tab's `EntryStore` without a relist. Headless; the tab hookup is
a later task (steps below).

## Files

| File | What |
|---|---|
| `watch/mod.rs` | `DirWatcher`, `Options`, `Message`, `Unreliable`, `Gone` |
| `watch/hub.rs` | the shared I/O completion port, its one thread, all `unsafe` |
| `watch/coalesce.rs` | `Coalescer`: raw events in, `Batch` out. Pure logic, unit-tested |
| `watch/apply.rs` | `resolve` (disk), `apply_resolved` (store), `apply_batch` (both) |
| `tests/watch.rs`, `tests/watch_idle.rs` | real-directory tests; thread count and idle CPU |
| `examples/watch_bench.rs` | the numbers below |

## Thread model: one IOCP, one thread, for every watcher

All directory handles are opened `FILE_FLAG_OVERLAPPED` and attached to a single
completion port (created on the first `DirWatcher::start`). One thread
(`tsubame-watch`, lives for the process) blocks in `GetQueuedCompletionStatus`.
Why not a thread per watcher: a tab would cost a thread and 1 MB of stack for a
read that is pending 99.99% of the time; 50 tabs must not mean 50 threads. Why
not a window/APC per watcher: APCs need an alertable thread each, and posting
one message per tab to the UI thread to do the parsing would put the parsing on
the UI thread. Measured: 50 watchers add 1 thread in total (the hub), and the
second watcher onward add none.

- **Idle = zero wake-ups.** The wait timeout is the nearest open coalescing
  deadline, or `INFINITE` when no window is open. There is no polling timer and
  no timer thread; the timeout exists only while events are pending.
- **Memory the kernel writes** (the `OVERLAPPED` and the record buffer) lives in
  an `Arc<Watch>`. While a read is pending the kernel owns one strong count; the
  hub takes it back when the packet arrives. A closed handle always delivers one
  last aborted packet, so the memory outlives the I/O however the watcher dies.
- **Re-arm before parse.** A finished read is copied out and the next read is
  issued first; parsing happens after. The kernel buffers events only while no
  read is pending, so this keeps the unarmed window to a memcpy.
- **Drop** (`DirWatcher::drop`): cancels the read and closes the directory
  handle before returning (measured < 1 ms; 100 start/drop rounds leave the
  handle count flat). The folder opens with `FILE_SHARE_DELETE`, so the watcher
  never blocks deleting or renaming the folder it shows.

## Coalescing

Each touched name keeps `start` (there when the window opened), `now`, `dirty`
and `origin` (the name the file held at the start, after renames). The diff is
read off that state:

| start | now | result |
|---|---|---|
| no | no | nothing: created + deleted in one window cancel |
| no | yes | `Created` (`Renamed` if it arrived by rename) |
| yes | no | `Removed` (a file that only moved is covered by its `Renamed`) |
| yes | yes | `Modified` if anything touched it; delete + create of one name lands here |

Also: repeated modifies merge; `RenamedOld`+`RenamedNew` pair into one
`Renamed`, chains collapse (`a>b`, `b>c` is `a>c`), a rename back is nothing,
created-then-renamed is `Created(final name)`, a lone `RenamedOld` (moved out)
is `Removed`, a lone `RenamedNew` (moved in) is `Created`. A rename swap through
a temp name is two renames. Names are compared exactly (a case-only rename is a
rename); the store lookup is case-insensitive.

**Window.** A debounce: it closes `window` (default 60 ms) after the last event
but never later than `max_delay` (default 250 ms) after the first. So one event
arrives about one window later (measured 61-70 ms) and a long burst comes out as
one batch per 250 ms instead of one huge late one. Batches that reduce to nothing
are not sent.

The diff is advisory. `apply` stats every changed name and trusts the disk, so a
stale hint costs a stat, never a wrong row.

## Messages

`DirWatcher::start(dir, Options, wake) -> (DirWatcher, Receiver<Message>)`; `wake`
(the app's `PostMessageW`) runs on the hub thread after every message.

| Message | Meaning | Caller |
|---|---|---|
| `Batch` | one coalesced window | `resolve` + `apply_resolved` |
| `Overflow` | kernel buffer overflowed, records lost (also a record bigger than the buffer). The watcher goes on, buffer x4 up to `max_buffer_bytes` | relist |
| `Unreliable(Network/Removable/CdRom)` | sent first; notifications may miss changes. Still watching | also relist on focus |
| `Unreliable(NotSupported)` | the filesystem rejects notifications; watching stopped | relist on focus only |
| `Gone(Deleted/Renamed/Unavailable)` | terminal. Pending changes are flushed first | leave the folder or show the state |
| `Failed(Win32Error)` | terminal, unexpected error such as access denied | show it |

A watched folder that is deleted completes the read with an error; the path then
tells deleted (`Gone(Deleted)`) from drive-gone (`Gone(Unavailable)`) from a real
access denial (`Failed`). A **rename of the watched folder raises no event**: the
handle follows the folder. The hub notices at the next batch (final path of the
handle differs from the one at start, `Gone(Renamed)`); for a quiet folder call
`DirWatcher::still_present()` when the window gains focus (one syscall).
Network paths cap the buffer at 64 KB (the redirector rejects more).

## Applying a batch

```rust
let resolved = watch::resolve(&dir, &batch, store.len());       // disk; Send; worker thread for bursts
let stats = watch::apply_resolved(&mut store, Some(&mut sel), &batch, &resolved)?; // store only; UI thread
```

`apply_batch(store, sel, dir, batch)` does both in place (fine for 1-30 names).

- **resolve**: a few names are stat'ed one by one (`GetFileAttributesExW`; `FindFirstFileExW`
  for 8.3-looking names and when the first call is denied or sharing-violates,
  which also gives the directory's own spelling). From 32 names on, when
  `names x 128 > entries in the folder`, the folder is listed once with the
  enumerator instead (a stat is 50-80 us on this machine, a listing about 0.6 us
  per entry). Misses of the listing (differently spelled, or gone) fall back to a stat.
- **apply_resolved**: looks the names up (`EntryStore::find_many`, hashed when many),
  builds removals and upserts, and calls `EntryStore::apply` once. A name that is
  gone by the stat becomes a removal; `Created` of a name already present is an
  update; a rename rewrites the entry in place.
- **IDs and selection stay stable.** A rename keeps its `EntryId`, so selection,
  focus and anchor follow the file. Removed entries leave the `Selection`
  (`Selection::forget`, total and count exact) and a selected entry whose size
  changed updates the selected-bytes total (`Selection::resized`). Tombstoned IDs
  are reused by later inserts; the store keeps `len()` (live) and `id_bound()` (ID
  space) apart.
- Names containing `\` or `/` (recursive watches) are skipped.

Store additions (tsubame-core, see `docs/core-store.md`): `apply`, `find`,
`find_many`, `compact_names`, `len()` counts live entries, `id_bound()`,
`is_live`, `Upsert`, `Selection::forget/resized`.

## Integrating into a tab (later task)

1. `Tab` gets `watcher: Option<DirWatcher>` and `watch_rx: Option<Receiver<Message>>`.
2. In `Tab::start(path, wake)`: drop the old watcher, and after the enumeration
   `Done` (not before: the listing is the baseline) start `DirWatcher::start(path,
   Options::default(), wake_watch)`. For a synthetic or failed tab, no watcher.
   `wake_watch` posts a second `WM_APP` message like the enumerator's.
3. On the watch wake: `while let Ok(m) = rx.try_recv()`. `Batch` with up to ~30
   changes: `apply_batch`; bigger: send `resolve` to the shell worker pool, and on
   its reply call `apply_resolved` (check the tab and its listing generation first;
   drop it if the tab navigated). Then repaint, and keep the cursor/scroll by ID
   (`list.cursor` is a position: re-resolve it from `sel.focus`).
4. `Overflow`, `Unreliable`, `Gone(Unavailable)`: relist (`Tab::start`), and for
   `Unreliable` also relist on window focus. `Gone(Deleted/Renamed)`: go up or show
   a "folder is gone" state. `Failed`: status bar.
5. On window focus: `watcher.still_present()`; false means relist or navigate away.
6. The UI count `"N of M items"` must use `store.len()` (live count), which it already does.
7. Closing or navigating a tab drops the `DirWatcher`; that closes the handle.

Not done (not needed for this task): ignoring our own writes, watching the parent
to catch a rename of a quiet folder without a focus check, a cap on how many
events one window may hold (the 250 ms `max_delay` bounds it by throughput).

## Numbers (FOX, CCD1, game running, -j8; debug tests, release bench)

| Metric | Result |
|---|---|
| Latency, 1 event to batch (window 60 ms, n=40) | min 61.4 / median 64.7 / p95 67.4 / max 70.5 ms |
| 10,000 files written in a loop | 16-17 batches (one per 250 ms of the 3.8-4.0 s write), 20,000 raw events, last batch 69 ms after the last write, no overflow |
| Idle, 20 armed watchers, 10 s | 0 ms CPU, 0 timers |
| Threads for 50 watchers | +1 (the hub); 6 -> 7 |
| Handle count over 100 start/drop rounds | flat (+2 constant) |
| Apply 10k inserts into 100k store (`apply_resolved`) | 3.5-3.7 ms |
| Apply 10k modifies in 100k store | 3.7-4.8 ms |
| Apply 10k removals from 100k store | 3.0-4.0 ms |
| Apply 1 insert into 100k store, resolve + apply | 0.29-0.32 ms |
| `resolve` 10k names: per-name stat / one listing (folder of 10k files) | 496-577 ms / 3.3-4.0 ms |

The listing time scales with the folder: expect 30-60 ms for 100k entries (C2:
warm 100k done in 33.6 ms), still far under 10k stats.
