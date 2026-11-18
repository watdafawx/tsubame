# File operation queue (C3 foundation; incomplete)

`fileops::FileOpQueue::new(backend)` returns queue and event receiver.
`submit(Vec<Operation>)`, `undo()`, `redo()` return cancellable tickets.
Submission performs no disk or COM calls. One STA worker executes FIFO commands.
Poll receiver using `try_recv`; conflict event owns a one-use `ConflictReply`.
`answer(choice, apply_to_all)` resumes worker. Cancellation interrupts conflict waits.
Errors are per item; subsequent items continue. Dropping queue closes submission;
already queued work drains on detached worker.

`OpBackend::execute` receives operation, cancellation token and byte-delta callback.
Return inverse operation only after success. `FsBackend` copies in 256 KiB chunks,
rejects symlinks, uses rename for same-volume moves and copies before removing source
on Windows ERROR_NOT_SAME_DEVICE. Targets must have existing parents.
NewFile uses create_new. NewFolder fails if destination exists.

Events: Started, Progress (current path, cumulative bytes for item), Conflict,
Error, Finished. Progress limited to 30 Hz within each submitted batch; lifecycle,
conflict and error events bypass throttle. Queue and event channels are unbounded.

Journal currently private in-memory forward/inverse pairs, capped at 200 records.
Copy/new-file/new-folder inverse is permanent deletion in this filesystem fallback;
move/rename inverse moves back. Permanent delete has no inverse. Successful new work
clears redo. Undo/redo retain their entry on failure. Each list item is one entry.

## Outstanding before C3 acceptance

- IFileOperation backend, COM message pump, progress sink and recycle restore tokens.
- Replace with reversible backup; currently reports error and skips (prevents data loss).
- Recycle deletion; currently Unsupported (never silently permanently deletes).
- Journal serialization, non-undoable records/reasons, grouped batch undo.
- Queue-wide totals, item counts, wake callback and error-response choices.
- Cancellation on queue drop/shutdown and bounded event delivery.
- Reparse-point-safe cross-volume move source deletion; metadata preservation.
- TOCTOU hardening against concurrent destination/source mutations.
- Long-path and throttle tests; shell integration and replace tests.

No GUI integration. Do not wire this foundation to destructive UI commands yet.

## FOX measurements (2026-11-18)

Debug optimized test profile, CCD1, 10,000 files of 128 bytes in system temp.
Single serial repeat after workspace tests: filesystem copy 5.197 s (1,924 files/s),
robocopy /E /R:0 /W:0 7.899 s (1,266 files/s). Warm cache, fixed order, one sample;
not a cold-cache or shell-backend comparison. Includes subprocess startup for robocopy.
100,000 fake-backend items in one batch: 0.042 microseconds/item, excluding batch
construction. Does not measure a submit call per item or per-item event delivery.

Validation: cargo check --workspace, cargo test --workspace and clippy -p tsubame-win
--all-targets -- -D warnings passed through cargo-ccd1. Added six tests/benchmarks.
Existing ignored child-process helper and updater doctest remain ignored.
