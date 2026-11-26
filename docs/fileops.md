# File operation queue (C3)

Headless logic, no UI. Code: `crates/tsubame-win/src/fileops/`.

| File | What |
|---|---|
| `mod.rs` | `Operation`, `Event`, `Choice`, `Step`, `OpBackend`, `Cancellation` |
| `queue.rs` | `FileOpQueue`, the STA worker, conflicts, throttle, journal updates |
| `fsbackend.rs` | `FsBackend`: std::fs copy/move, Recycle Bin through `recycle` |
| `recycle.rs` | `IFileOperation` recycle, restore, purge |
| `totals.rs` | queue-wide totals and the off-thread pre-scan |
| `journal.rs` | undo/redo journal and its text format |

## API

```rust
let (queue, events) = FileOpQueue::new(FsBackend)?;   // or with_journal(backend, Arc<Mutex<Journal>>)
let ticket = queue.submit(vec![Operation::Copy { source, target }])?;
queue.undo()?; queue.redo()?;           // each returns a cancellable Ticket
queue.journal();                        // Arc<Mutex<Journal>>: save_to_string / load_from_str
queue.totals();                         // queue-wide Totals, zero when idle
```

- Submission performs no disk or COM call. One STA worker thread (`OleInitialize`) runs jobs FIFO and pumps messages while idle (`MsgWaitForMultipleObjectsEx` on a wake event) and while it waits for a conflict answer.
- Poll `events` with `try_recv`. Events: `Started`, `Progress`, `Conflict`, `Error` (per item; later items continue), `Finished`.
- A `Conflict` owns a one-use `ConflictReply`: `answer(choice, apply_to_all)`. Choices: `Replace`, `Skip`, `KeepBoth`, `Cancel` (stops the whole job). It also carries `source` and both sides' `FileMeta` (size, modified, folder) for the dialog. Cancelling interrupts the wait.
- `Ticket.cancellation` is the job's cancel flag and pause gate. `pause()` holds the worker before the next operation and, inside a copy, between chunks; it sleeps on a condition variable (no polling, no events, so the throttle never sees paused time). `resume()` and `cancel()` wake it. A job cancelled while it waits behind another sends only `Finished { cancelled: true }`.
- `undo_group_of(id)` reverts submission `id` only if it is still the newest undo group when the worker reaches it (the panel's Undo link); otherwise nothing runs.
- Dropping the queue closes submission; queued work drains on the detached worker.
- `OpBackend::execute(op, cancel, progress: FnMut(Step))` returns the inverse operation only after success. `Step { bytes, files }`.

## What is undoable

| Operation | Undo | Notes |
|---|---|---|
| Copy | recycle the created copy | falls back to permanent delete of our own copy only if the volume has no bin |
| Move, Rename | move back | same-volume rename, or copy then delete the source across volumes |
| NewFile, NewFolder | recycle it | same fallback as copy |
| Delete (recycle) | restore from the Recycle Bin | see below |
| Replace (any of the above onto an existing target) | undoes the operation, then restores the old target | see below |
| Delete permanent | **not undoable** | the journal entry stays as history with the reason `permanent delete cannot be undone` |

Undo takes the newest undoable entry and skips non-undoable ones. A failed undo or redo leaves its entry where it was and reports an `Error`. New work clears redo. The journal holds 200 entries; when full it drops non-undoable history first, then the oldest entry. Each submitted item is one entry (grouped undo of a whole batch is not implemented).

## Recycle Bin: how delete and restore work

Delete: `IFileOperation::DeleteItem` with `FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOFX_NOCOPYHOOKS` and no owner window, so no dialog, progress UI or focus change. A progress sink catches `PostDeleteItem`; the "newly created" item is the entry inside the bin and its `SIGDN_FILESYSPATH` (`C:\$Recycle.Bin\<sid>\$R….ext`) is the restore token. `Operation::Restore(RecycledItem { original, recycled })` is the inverse.

Restore: enumerate the aggregated bin (`SHGetKnownFolderItem(FOLDERID_RecycleBinFolder)` → `IEnumShellItems`), find the entry whose file system path equals the token, and `IFileOperation::MoveItem` it to the original folder under the original name (missing parent folders are recreated). The shell keeps the `$I` metadata consistent. Restore never overwrites: if the original path is occupied it fails with `AlreadyExists` and the entry stays undoable. Matching by original path plus deletion time was not needed: the token from the sink is exact and survives a saved journal as long as the bin entry exists. If the user emptied the bin, restore reports `NotFound`.

Redo of a recycle delete deletes again and replaces the entry's inverse with the new token (tokens change every time an item is recycled).

Safety limits found by testing (`recycle.rs`):
- A silent recycle of a path of 260 characters or more makes the shell **permanently delete** the item, even in a long-path-aware process. `recycle()` refuses those up front (`Unsupported`, file untouched). Paths up to 259 characters recycle and restore fine. Callers may offer an explicit permanent delete.
- Volumes without a bin (not `DRIVE_FIXED`, or `SHQueryRecycleBinW` fails) are refused. If the shell reports success but no bin entry, `recycle()` returns an error saying the item was deleted permanently.
- The shell rejects `\\?\` paths in `SHCreateItemFromParsingName` (error 87), so shell calls use plain absolute paths. std::fs uses verbatim paths itself, so copy and move work beyond 260 characters.
- Not detected: an item larger than the bin's size limit is deleted for good by the shell. The post-check reports it, but the item is already gone. Check size before offering recycle if that matters.

## Replace

On a `Replace` answer the queue runs `Sequence[Delete{target, recycle}, op]`. If `op` fails, the recycled target is restored (rolled back). If it succeeds, the inverse is `Sequence[inverse of op, Restore(old target)]`, so one undo puts everything back. Redo repeats the sequence. Replace is refused (`Error`, nothing touched) when the source and target are the same or one contains the other.

## Progress and totals

Every `Progress` event carries `totals: Totals { files_done, files_total, bytes_done, bytes_total }` for the whole queue (all submitted batches, not just the current one), `job` (the same numbers for this job alone), `index` and `item_bytes` (the current operation and its scanned size) and `bytes_done` for the current item. `Finished` carries the last queue snapshot and the job's own totals.

- On submit a scanner thread (never the UI thread) sizes each operation: copies and cross-volume moves recurse into folders (files and bytes); everything else counts as one file and no bytes. If the worker reaches an item before the scanner, the worker adds what it actually did and the late scan result is ignored. Skipped or cancelled items leave the totals. Counters reset when the queue drains.
- Progress is throttled queue-wide to one event per 34 ms (at most 30 in any second). Lifecycle, conflict and error events are not throttled.

## Operations panel and conflict dialog (B11-E2)

The app shows the queue in an own-drawn panel (`src/ops_panel.rs`, mock `mocks/progress-panel.html`, decisions in `docs/progress-panel-mock.md`) and asks conflicts in an own-drawn dialog (`src/conflict_dialog.rs`). `src/ops.rs` feeds both from the events above; archive extraction gets a card through a thin adapter (`archive_ui::ExtractNote`).

- One card per job, FIFO: waiting (Cancel only), running (Pause, file list, Cancel), paused, asking ("Waiting for your answer"), done (Undo link for 5 s, then Ctrl+Z as before), cancelled ("Cancelled after N of M"), failed (never auto-dismissed; Show details, Retry the failed items, Skip). Finished cards sit above the active ones.
- The panel opens 400 ms after a job starts, so quick jobs never flash; a failure always shows. Folding it (chevron or Ctrl+Shift+O, `view.operations`) leaves a pill in the status bar ("Copying, 43%", "2 operations, 43%", "Done", "1 failed").
- Speed is UI side: an EMA (weight 0.3) of the job's done counter, at most 4 samples a second, and a 40-sample ring for the 10 s sparkline. Paused time and the time a question waits never count.
- Dialog: Replace (R), Keep both (K), Skip (S), Cancel operation (Esc, stops the job), Enter = focused or suggested button, A toggles "Do this for all remaining conflicts", Tab moves the focus. Suggested: Replace when the incoming file is newer, else Skip (also when both look identical: same size and second). Modal over the window, no scrim. For an extraction, Cancel sets its cancel flag and skips.
- Cost: no timer of its own. Main's 60 ms `OPS_TIMER` exists only while a job runs and is not paused and no dialog waits. Progress repaints at most 10 times a second and only when a card's numbers changed (the frame is still painted whole: the renderer has no partial redraw). One one-shot `DISMISS_TIMER` fires for the earliest finished card; the countdown line is drawn from the clock at paint time and does not animate on its own. With the panel closed and nothing running there are no wake-ups.
- UI Automation: the panel (or pill) is one surface (`PART_OPS`), the dialog another (`PART_CONFLICT`); every button is an Invokable item with a name (`docs/uia-views.md`).

## Journal text format

`Journal::save_to_string()` / `Journal::load_from_str(&str) -> (Journal, LoadReport { loaded, skipped })`. Plain text, no serde, one record per line:

```
tsubame-journal<TAB>1
U<TAB>copy<TAB>C:\a<TAB>C:\b<TAB>delete<TAB>C:\b<TAB>0<TAB>
U<TAB>delete<TAB>C:\x<TAB>1<TAB>-<TAB>permanent delete cannot be undone
```

`U` undo stack (oldest first), `R` redo stack. Fields: forward operation, inverse (`-` if none), reason. Operations: `copy|move|rename src dst`, `delete path 0|1`, `newfolder path`, `newfile path`, `restore original recycled`, `seq n op…`. In every path and text field `%`, control characters below U+0020 and U+007F become `%XX`; C1 controls and lone UTF-16 surrogates become `%uXXXX`; Unicode and spaces are written as is. Unknown or corrupt lines, a bad escape, trailing junk or an unknown or future header version are skipped and counted; loading never panics (tested with truncation at every character and byte mutations).

## Tests

`cargo test -p tsubame-win --lib fileops` (about 30 s, most of it the progress and 10k-copy tests). The tests use temp directories on the system drive, send real items to the Recycle Bin, and purge what they created. They skip with a message if the temp volume has no bin. `crates/tsubame-win/build.rs` embeds a `longPathAware` manifest into this crate's test and example executables, matching the app manifest.

Coverage: recycle delete and undo/redo of file and folder, restore refuses to overwrite, permanent delete not undoable, Replace (copy and move-over-folder, undo, redo, self refusal, rollback on failure), long paths (copy, move, tree copy; recycle at 255 characters works, at 300 is refused untouched), throttle (unit and through the queue), queue-wide totals, journal round trip with unicode, spaces, `%`, tabs, control characters, a lone surrogate, corrupt and future-version journals, journal survives a new queue.

## Not done

- Grouped undo: a batch of N items is N journal entries and N undos.
- Wake callback, error-response choices (retry/skip/abort per error), cancellation on queue drop.
- Reparse points: `FsBackend` refuses to copy symlinks and junctions. Cross-volume move does not check for them before deleting the source.
- Metadata (timestamps, ACLs, alternate streams) is not preserved by the `std::fs` copy.
- No TOCTOU hardening against another process changing source or target between the check and the operation.
- Recycling is one `IFileOperation` per item (each call creates the COM object and performs on its own); batching many deletes into one `IFileOperation` is not done and its cost for large selections is not measured.
- No GUI integration. Do not wire this to destructive UI commands before grouped undo and the error-response choices exist.

## Measurements (dev machine)

Measured 2026-11-18 on the dev machine (a game was running in the background; disk noise is real), test profile, 10,000 files of 128 bytes in `%TEMP%` (system drive), warm cache, `FsBackend` copy of the whole tree against `robocopy /E /R:0 /W:0` (includes process start), serial, nothing else building:

| Run | fs copy | robocopy |
|---|---|---|
| 1 (all benchmarks, 1 thread) | 5.41 s (1,848 files/s) | 8.72 s (1,147 files/s) |
| 2 | 10.41 s (960 files/s) | 8.33 s (1,200 files/s) |
| 3 | 5.78 s (1,730 files/s) | 7.96 s (1,257 files/s) |
| 4 | 4.82 s (2,074 files/s) | 7.28 s (1,373 files/s) |
| 5 | 5.99 s (1,669 files/s) | 9.88 s (1,012 files/s) |

Median: fs copy 5.78 s, robocopy 8.33 s. Run 2 is an outlier where the copy was slower than robocopy; the spread is large, so treat the gap as roughly 1.4x in favour of the filesystem backend, not a precise figure. Earlier child result (5.2 s vs 7.9 s) is in line. Queue overhead: 100,000 fake-backend items in one batch, submit call 0.4 ms, 0.32 microseconds per item end to end (includes the scanner and totals bookkeeping).
