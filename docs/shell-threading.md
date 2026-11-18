# Shell COM off the UI thread (spike S4)

S4 shows that shell COM can run entirely off the UI thread and feed the S1 Direct2D window. Icons, thumbnails and the real Explorer context menu all come from a pool of STA worker threads. The UI thread makes no shell COM call.

Code:
- `crates/tsubame-win/src/shell/`: `mod.rs` (pool), `icon.rs`, `thumb.rs`, `menu.rs`
- `crates/tsubame-win/src/{bitmap,enumerate,procinfo}.rs`
- In the app: `src/shellui.rs` (requests, icon LRU), `src/lru.rs`, `src/stress.rs`
- Tests: `crates/tsubame-win/tests/pool.rs`, plus unit tests in `bitmap.rs`, `lru.rs` and `data.rs`.

## Result against PLAN §2

All numbers were measured on rig: 4K at 200%, release build, 2 workers, `--dir C:\Windows\System32` (5,013 entries).

| Gate | Budget | Measured | Result |
|---|---|---|---|
| Scroll paint p95, icons streaming | < 5 ms | **2.88 ms** (p99 4.56, max 5.23). Fast fling phase p95 3.20 ms. | **pass** |
| Same, worst case (every file's icon asked by path) | < 5 ms | **3.08 ms** (p99 3.39, max 4.82). 3,139 icons arrived during the run. | **pass** |
| Same, both workers hung for 20 s at scroll start | < 5 ms | **2.75 ms** (max 3.20). Both workers were replaced and icons resumed. | **pass** |
| Paint while a context menu is held open for 2 s | < 5 ms | 132 frames, p95 **0.81 ms** | **pass** |
| Leak gate, 200 context menus (1 and 3 items, alternating) | private bytes within 5% of start | End vs start **−20.1%**. Menu 10 → 200: private bytes −5.3%, handles **+4 (+0.4%)**, GDI +0, USER +0. | **pass** |
| Leak gate, 500 thumbnails (844 MB of pixels streamed through) | private bytes within 5% of start | −29.2%. GDI objects 45 → 45. | **pass** |

The leak gate counts only growth: private bytes are allowed to fall. The "start" sample is taken 3 s after launch and still includes the GPU-swap transient. So two checks are reported:
- **Gate A:** end of the run against start.
- **Gate B:** menu 10 against menu 200, i.e. after the handlers have loaded. A per-cycle leak would show up in gate B.

Handles rise by about 390 once, at the first menu. That is shell extensions loading their DLLs and threads, and the count then stays flat (939 / 943 / 943 at menus 10, 100 and 200). No leaking call was found, so the helper-process fallback was not needed. It stays the plan B: if a third-party extension is ever caught leaking, run `QueryContextMenu` and `InvokeCommand` in a disposable helper process that owns the menu.

## Measured numbers

All from rig unless marked FOX.

**Icons** (SHGetFileInfo + system image list, 32 px at 200%):

| Case | Worker time | Submit → applied on UI |
|---|---|---|
| First icon in a fresh process (folder) | 13.7 ms | 13.8 ms |
| FOX, direct, cold: Folder16 / txt16 / txt48 / txt256 | 25.6 / 8.8 / 7.4 / 12.5 ms | n/a |
| By extension, warm | p50 1.64, p95 24.6 ms (n=37, includes cold first hits) | p50 10.3, p95 40.0 ms |
| Per path, warm (exe, dll, lnk, ico, custom folders) | p50 0.74, p95 2.15 ms (n=3,139) | p50 6.6, p95 19.0 ms |
| Per path via IShellItemImageFactory (`TSUBAME_ICON_FACTORY=1`) | p50 1.12, p95 2.44 ms | p50 9.6, p95 22.9 ms |

The image-list path is about 1.5× faster than the factory, so it is the default. The factory stays as an option because it can return sizes above 256 px.

**Thumbnails** (IShellItemImageFactory, 512 px asked, BIGGERSIZEOK; 500 images of 800×600 JPEG/PNG):

| Case | Time |
|---|---|
| `SIIGBF_INCACHEONLY` probe that misses | p50 0.12 ms |
| Generated (probe miss, then GetImage) | p50 23.4, p95 31.0, max 106 ms. 500 thumbnails in 5.9 s on 2 workers. |
| From the system thumbnail cache (second run) | p50 3.08, p95 3.76 ms. 500 thumbnails in 0.8 s. |

Interactive use: the panel showed a 768×576 thumbnail generated in 95.6 ms (cold) and 58.4 ms.

**Context menu:**

| Case | QueryContextMenu | On screen (job start → first WM_ENTERIDLE) |
|---|---|---|
| First menu in the process (cold handlers) | 490-1290 ms | 512-1309 ms |
| Warm, 200-cycle run | p50 27.7, p95 33.7 ms | p50 33.5, p95 39.5 ms |
| Warm, interactive (1 file, 23-25 items) | 150-167 ms | 156-176 ms |
| Folder, Shift (extended verbs, 28 items) | 371-396 ms | 393-423 ms |

- Bind (SHParseDisplayName + SHBindToParent + GetUIObjectOf) takes p50 1.5 ms.
- A full round trip (UI submit → reply applied, auto-closed menu) takes p50 50 ms.
- Interactive menus cost more than the stress loop: with the mouse over them, handlers such as PeaZip, ShareX, Tailscale and Defender add their own items.
- Cold cost is dominated by loading shell extension DLLs. Warming one hidden `QueryContextMenu` at startup on a worker would hide it.

**Listing** (`--dir`, FindFirstFileExW with FIND_FIRST_EX_LARGE_FETCH, on a worker): 5,013 entries in 3.4-4.1 ms (0.21 MB store); 500 entries in 0.7 ms.

## Threading rules

1. **The UI thread makes no shell COM calls.** It submits `Request`s and drains `Reply`s. Everything that touches IShellFolder, IContextMenu, IShellItemImageFactory, SHGetFileInfo or image lists runs on a worker.
2. **Workers are STA and call `OleInitialize`**, not plain `CoInitializeEx(COINIT_APARTMENTTHREADED)`. Cut and Copy put data objects on the OLE clipboard, and drag/drop helpers need OLE. Shell extensions assume an STA, and many are apartment-threaded only.
3. **Workers pump messages all the time.** The idle wait is `MsgWaitForMultipleObjectsEx(job semaphore, QS_ALLINPUT, MWMO_INPUTAVAILABLE)`. Messages are dispatched whenever there is no job. Shell extensions post to their STA, cross-apartment calls are marshalled through the message queue, and menus need a pump. A worker that only blocks on a channel deadlocks the first extension that does a cross-apartment call.
4. **COM objects never leave the worker that made them.** Only plain data crosses threads: `Bitmap { w, h, px: Vec<u32> }` (premultiplied BGRA), `MenuResult`, strings. HICON and HBITMAP are converted and destroyed on the worker. GDI handles are per process, but the image lists the worker reads from are per thread.
5. **Replies:** an `mpsc` channel plus one `PostMessageW(hwnd, WM_APP+1)`. An `AtomicBool` keeps at most one wake message in flight, and `drain()` clears it before reading. A burst of 500 replies costs one wake and one repaint.
6. **Requests carry a ticket** `{id, Arc<AtomicBool> cancel}`.
   - The UI stamps each visible icon key with the frame number. After the frame it cancels pending icons that are no longer visible. Workers skip cancelled jobs before starting them.
   - Replies whose id is no longer pending are dropped as stale.
   - In the fast-fling bench: 740 requested, 344 cancelled by the UI, 243 skipped by workers, 101 late replies dropped.
7. **Priorities:** Menu and Thumb jobs go to the front of the shared queue and Icon jobs to the back. A right-click never waits behind a screen of icons.
8. **Hangs:**
   - Each worker publishes `busy_since` and the kind of job it is running.
   - A 1 s UI timer, armed only while the pool is busy (an idle app has no timers), calls `check_hung()`.
   - A worker busy longer than `TSUBAME_HANG_MS` (default 3 s) on an icon or thumbnail is abandoned. A fresh worker takes its slot, and the lost job ids are forgotten.
   - The old thread is leaked: it cannot be killed safely, and it exits on its own when the call returns. At most 8 replacements are made.
   - A worker showing a menu is never treated as hung, because the user may keep a menu open for minutes.
9. **Image-list warm-up is serialized per process.** See Pitfalls.

## Apartments and pumping in practice

- Icons: `SHGetFileInfoW(SHGFI_SYSICONINDEX [| SHGFI_USEFILEATTRIBUTES])` gives the index. `SHGetImageList` gives the smallest list (16/32/48/256) that is at least the wanted size, cached in a thread_local per worker. Then `IImageList::GetIcon(ILD_TRANSPARENT)`, and HICON → BGRA.
  - By-extension icons use `USEFILEATTRIBUTES` with a fake `x.ext` name, so there is no disk access.
  - Per-path icons are asked for exe, lnk, ico, cur, ani, url, scr, cpl, msc and appref-ms files, and for folders marked READONLY or SYSTEM (desktop.ini custom icons).
- HICON → pixels: `GetIconInfo`, then `GetDIBits` of the colour bitmap as 32 bpp top-down.
  - If any alpha is non-zero, the pixels are straight alpha and are premultiplied.
  - Otherwise the AND mask gives alpha.
  - Monochrome icons are handled separately.
  - Both bitmaps from `GetIconInfo` are deleted, as is the icon.
  - Thumbnails from `GetImage` that have no alpha are made opaque. Ones with straight alpha are premultiplied.
- Thumbnails: `SHCreateItemFromParsingName` → `IShellItemImageFactory::GetImage(BIGGERSIZEOK | INCACHEONLY)`. On a miss, `GetImage(BIGGERSIZEOK)` generates one. Each reply records whether it came from the cache and how long the probe took.

## HWND and HMENU ownership (context menu)

- **The owner window lives on the worker.** Each worker creates one hidden `WS_POPUP` window with `WS_EX_TOOLWINDOW`. It is not `HWND_MESSAGE`, because a message-only window can't be foreground and the menu then doesn't close on click-away.
- **`IContextMenu2`/`3` forwarding:** the host window's procedure forwards `WM_INITMENUPOPUP`, `WM_DRAWITEM`, `WM_MEASUREITEM` and `WM_MENUCHAR` to `HandleMenuMsg2` (or `HandleMenuMsg`). Without it, "Send to" and "Open with" submenus and owner-drawn items stay empty.
- **The HMENU is created, filled, tracked and destroyed on the same worker:** `CreatePopupMenu` → `QueryContextMenu(1..0x7fff, CMF_NORMAL | CMF_EXPLORE [| CMF_EXTENDEDVERBS])` → `TrackPopupMenuEx(TPM_RETURNCMD | TPM_RIGHTBUTTON)` → `InvokeCommand` → `DestroyMenu`.
- **Before tracking**, the worker calls `SetForegroundWindow(host)`. After tracking it posts `WM_NULL` (KB 135788), which is what makes Escape and click-away work.
- **After the menu**, if the host still owns the foreground, the worker gives it back to the UI window. If it didn't, the hidden host keeps the keyboard and the app stops reacting to keys.
- **InvokeCommand** uses `CMINVOKECOMMANDINFOEX` with `CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE`, plus `CMIC_MASK_SHIFT_DOWN` when Shift was held. The verb comes from `GetCommandString(GCS_VERBW)` and is recorded.
- **Verified on rig:**
  - The menu appeared at the cursor.
  - "Send to" opened.
  - Properties ran: "chose 'properties', InvokeCommand ok", and the Properties dialog opened.
  - Escape and click-away both closed the menu and were logged as "dismissed".
  - Shift (keyboard Shift+F10) added "Open in new process", "Open PowerShell window here" and "Open Linux shell here" for a folder.
  - A keyboard menu opened at the selected row.

**Wrong-owner test** (key `W`: the owner passed is the UI window, which belongs to another thread). `QueryContextMenu` succeeds (26 items), but `TrackPopupMenuEx` fails with `GetLastError 87` (ERROR_INVALID_PARAMETER) and the menu is never shown. Nothing hangs, and the UI thread keeps painting. So the menu owner must be a window of the thread that tracks the menu. The app can't hand its own top-level window to the worker; the worker needs its own window.

## Pitfalls found

- **A cold image-list race.** In a fresh process, two threads calling `SHGetFileInfoW(SYSICONINDEX)` at the same time make one of them return 0. This happened on every run on FOX. The fix is a process-wide mutex around the first `SHGetFileInfo` + `SHGetImageList` on each thread (`icon::warm_up`), run when a worker starts and again whenever a thread's lists are empty.
- **F10 breaks the menu.** `DefWindowProc` turns F10 (including Shift+F10) into menu mode on the UI thread. `SetForegroundWindow` fails while any menu is active, so the worker's popup came up without the foreground and Escape never reached it. Fix: the app handles `WM_SYSKEYDOWN`/`WM_SYSKEYUP` for F10 itself and never passes them on. `MenuResult.foreground` records whether `SetForegroundWindow` succeeded, and the status bar says so when it failed.
- **The host window keeps the foreground after the menu** unless the worker hands it back (see above).
- **Letter shortcuts belong in `WM_CHAR`, not in `WM_KEYDOWN` with `'T'`.** Injected input (`VK_PACKET`) and other keyboard layouts don't produce that virtual key.
- **The menu owner must be on the tracking thread** (wrong-owner test).
- **Cold menu cost is large and lands on the first right-click:** 0.5-1.3 s on rig, almost all of it inside `QueryContextMenu` loading extension DLLs. It is fine on a worker (the UI kept painting), but the user still waits. Pre-warm it.
- **Thumbnail results depend on the system cache.** The first run generated 498 of 500, the second found all 500 in the cache. Report both, never one number.
- **`SHGetFileInfo` icon indices are per process, but the image lists are fetched per thread.** Keep the `IImageList` in a thread_local and never share it.
- **Abandoned hung threads stay alive until their call returns.** They are counted (`leaked_alive`) and capped. A shell extension that never returns would keep one thread forever; if that happens a lot, use the helper process.

## How to reproduce

```
pwsh X:\Dev\tools\push-to-rig.ps1 -Project <worktree> -BuildOnRig -Exe tsubame -Profile release -Launch "-AppArgLine:--dir C:\Windows\System32"
```

- **Keys:**
  - Right-click: menu at the cursor. Shift+right-click or Shift+F10: extended verbs. Menu key: menu at the selected row.
  - Ctrl+click: mark several rows.
  - T: thumbnail panel. H: hang one worker for 20 s. W: wrong-owner menu.
- **Bench:** `--bench --dir <path>` writes `s1-bench.txt` next to the exe, or the name in `TSUBAME_BENCH_OUT`.
  - Add `--hang` to hang every worker at scroll start.
  - Env `TSUBAME_ICON_PER_PATH=1` asks every file's icon by path. `TSUBAME_ICON_FACTORY=1` uses the factory for per-path icons.
- **Stress:**
  - `--stress menu --dir C:\Windows\System32` writes `s4-stress-menu.txt`.
  - `--stress thumbs --dir <folder of images>` writes `s4-stress-thumbs.txt`.

## PERF1 recheck (2026-11-18)

The S4 negative gate was masked by delayed rendering-memory release. On rig,
S4 `e2fa069` repeated 200 menus: menu 10/100/200 private MB =
89.4/82.3/84.9 (10->200 -5.1%, yet 100->200 +2.6 MB). Extending that
same source's stress count to 1,000 gives 88.3/82.4/107.3 MB at 10/100/1000.
Thus growth already exists at the historical endpoint; a binary search for a
new per-menu leak between S4 and main has no passing long-run endpoint.

Main `e230e30`, diagnostic release, System32, 200 menus: 72.39->77.98 MB
at 10->200 (+7.7%); process heap commit 13.20->13.66 MB, busy
9.77->10.02 MB; outside-heap commit 19.23->24.00 MB, allocation count
127->318; Rust live 0.63->0.64 MB, mapped writable 34.85 MB unchanged.
This contradicts process-heap fragmentation or Rust cache growth as the
primary contributor. Attribution to a particular shell extension or native
allocation stack is **not established** by VirtualQuery alone.

Main 1,000 menus: 71.8/74.6/98.6 MB at 10/100/1000; outside-heap
18.89->42.39 MB, allocations 116->1101, Rust live 0.62->0.69 MB.
No plateau. Do not relax either gate or claim shell heap fragmentation.
`TSUBAME_FLIP=0` restores the misleading negative 200-menu gate (-3.8%)
while its menu 100->200 still grows 82.7->85.5 MB. MEM1 removed a
masking transient; these results do not prove MEM1 introduced allocations.

Stress now honors `TSUBAME_MEMDIAG=1` at counter snapshots and
`TSUBAME_STRESS_MENUS=1000` (200..10000, default 200), retaining checkpoint
100 and adding 200/500 for longer runs. Diagnostic snapshots perturb timing;
use production allocator for speed comparisons. Gate B still starts at 10.
Exact allocation owner, native-stack tracing and clip-mode recheck remain open.
