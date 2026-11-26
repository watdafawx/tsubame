# Operations panel mock (B11-E)

Mock only, nothing implemented. Open `mocks/progress-panel.html` (self-contained, works from `file://`, about 1100 px wide). Switches at the top: State (running + waiting, four queued, finished, error, conflict, collapsed pill), Theme, Scale (100 % / 200 %, the 200 % window is 640 x 420 logical so it fits), Sim speed (1x / 6x), and the conflict case (incoming newer / existing newer / identical). URL params for shots: `?state=running|queue|done|error|conflict|pill&theme=light|dark&scale=1|2&conf=in|ex|eq&sim=6`.

## Decisions in the mock

- **Panel**: docked bottom-right over the file list, 344 px wide, `popup` fill, 1 px `popup_border`, radius 8, no shadow. Header "Operations" + count chip + "Clear finished" + collapse chevron. Cards scroll inside (6 px thumb) when they do not fit. It opens 400 ms after an operation starts, so quick operations never flash (the status bar text stays as it is today).
- **Card** (running): verb + percent, "N items from **A** to **B**" (folder names, full path in a tooltip), 4 px bar, "bytes done of total", 72 px sparkline of the last 10 s (4 samples/s), "speed . time left" on the right. Pause / Resume, expand (3 files: current with its own mini bar, 2 next, "and N more") and Cancel (x, danger fill on hover). Deleting counts items and speaks "items/s", no bytes.
- **Queue**: the queue is FIFO with one worker, so the mock shows one running card and the rest "Waiting" (empty bar, only Cancel, total size at right). The next starts by itself.
- **Finished**: one line, green check, "Copied 14 items to Projects", Undo link, dismiss x, 2 px `ok` countdown line, gone after 5 s. Hover holds the countdown. Cancelled shows "Cancelled after 6 of 14 items" + Undo, same 5 s. Finished cards sit above the running ones.
- **Error**: warning icon, "Copied 12 of 14 items, 2 failed", Show details (file + reason per failed item), Retry (filled) and Skip. Never auto-dismisses.
- **Pill** (status bar, right): mini bar + "2 operations, 43%" (or "Copying, 43%" for one, "Done" with a check, "1 failed" with a warning). Click opens the panel; the chevron in the panel header collapses it.
- **Conflict dialog** replaces the MessageBox: modal inside the window (no scrim, because no alpha), two cards side by side, Existing (in B) and Incoming (from A), each with a 36 px icon, name, size ("larger" / "smaller") and modified date. The newer one gets the accent border, `selected` fill and a "Newer" chip. Identical files show "Same date" on both and a different title. Buttons: Replace, Keep both, Skip, Cancel operation. Suggested button (filled, Enter): Replace when incoming is newer, Skip otherwise. Checkbox "Do this for all remaining conflicts" turns the labels into "Replace all" / "Keep both, all" / "Skip all". Keys R, K, S, Esc. While it is open the card says "Waiting for your answer". Hint line keeps today's promise: replaced files go to the Recycle Bin.
- **Colour**: one new token, `ok` (dark `#5fd08a`, light `#1e7a3c`), used only for the check and the countdown. Everything else is existing tokens. Motion: bars and numbers change, nothing animates; no easing.
- **Perf rules the real code must keep**: no per-frame timer when nothing runs; progress repaint at most 10 Hz and only the dirty card (bar, numbers, sparkline); one one-shot timer for the earliest finished-card dismiss (re-armed on hover/leave), not a polling timer; panel closed and nothing running = zero wake-ups. The sparkline buffer is a fixed 40-float ring per running op.

## Data per element: what exists, what is missing

| Element | Exists today | Must be added |
|---|---|---|
| Verb, done verb | `Job.verb` / `Job.done` (src/ops.rs) | none |
| Items count | `Job.count` (operations submitted, not files inside folders) | files count per op from `Totals.files_total` (scan) |
| From A to B | `Job.folder` (destination only) | source folder (parent of first source) in `Job`; archive name for Extract |
| Percent, bytes done / total | `Event::Progress.totals` (`bytes_done`, `bytes_total`, `files_*`) | `Totals` is queue wide. Need per-op totals (id is in the event, scan in `totals.rs` is per op already via `OpScan`) |
| Speed + ETA | none | UI side: EMA of bytes_done deltas per op id, 4 samples/s ring; no queue change |
| Sparkline | none | UI ring buffer, same samples |
| Current files | `Event::Progress.current` + cumulative `bytes_done` of the item | the next 2 names (queue has the operation list), per-file size |
| Waiting state | `Started(id)` only; `jobs` map holds queued ids | a `Queued` / state field in `Ops` (queued until `Started`) |
| Pause / Resume | none | pause flag next to `Cancellation` (e.g. `PauseGate`: worker blocks in `check()` / between chunks); `Throttle` and speed window must ignore paused time |
| Cancel per op | `Ticket.cancellation` exists in the queue | `Ops` drops the ticket today: keep it in `Job` and expose `cancel(id)`; cancel of a waiting op must skip it without running |
| Cancelled result | `Finished { cancelled }` | files done at cancel time (from last Progress) |
| Undo link | journal group id = ticket id (`undo_group`) | `undo(id)` for a given group (today only the newest group); disable when a newer group exists or the op was permanent delete |
| Error list | `Event::Error { id, item, message }` | `Job` keeps only `errors` count + `last_error`: store `Vec<(PathBuf, String)>` |
| Retry / Skip | none | Retry = resubmit the failed `Operation`s as a new job; Skip = drop the entry (items already counted as failed) |
| Conflict dialog | `Event::Conflict { id, target, reply }`, `Choice`, `answer(choice, apply_to_all)`; plain MessageBox in `shellops::ask_conflict_box` | stat both files (size, mtime) on the UI thread or in the worker; the incoming source path (event carries only `target`: add `source`); own-drawn modal; separate Cancel-operation answer (today Cancel means skip) |
| Conflict dialog for extract | `ask_extract_conflict` on the extract thread | same dialog, same API; extract is outside the queue today (`tsubame_core::archive`), so it needs a thin adapter to show as a card |
| Pill | `Ops::message` text, `busy()` | aggregate percent over active ops (sum of per-op fractions) |
| Throttle | queue emits at most 30/s (`PROGRESS_INTERVAL` 34 ms); UI drains on `OPS_TIMER` | UI paints at most every 100 ms; the timer already exists only while `busy()` |

## Open questions (recommendation first)

1. **Parallel or FIFO?** The queue runs one op at a time. **Keep FIFO** (simple, no disk thrashing on one HDD); the mock shows the rest as Waiting. Alternative: run in parallel when sources and targets are on different physical drives.
2. **Pause in the queue**: **add it** (needed for the Pause button). Alternative: drop Pause, keep Cancel only.
3. **Cancel button in the conflict dialog**: **cancel the whole operation** (what users expect), with Skip as the explicit "skip this one". Today Cancel in the MessageBox means skip.
4. **"All remaining conflicts" count**: the queue does not pre-scan targets, so the box has no number. **No count.** Alternative: a pre-scan that lists conflicts up front (slower start on big copies).
5. **Panel auto-open delay**: **400 ms** after start. Alternative: always open at once, or never open and use the pill only.
6. **Failure mode**: **continue and list failures** (as the queue does now), Retry afterwards. Alternative: stop on the first error and ask.
7. **Undo link life**: **while the card is shown (5 s)**, then Ctrl+Z still works as today. Alternative: keep the card until dismissed.
8. **Extract in the panel**: **yes, via an adapter** (same card, no pause at first). Alternative: leave extract with its own progress UI.
9. **New `ok` green token**: **add to tokens.json** (dark `#5fd08a`, light `#1e7a3c`). Alternative: reuse `accent` for the check.
10. **Modal conflict dialog without scrim**: **keep** (no alpha in the style). Alternative: tint the whole window with a solid-colour checker pattern (rejected, noisy).
