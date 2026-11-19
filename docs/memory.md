# Memory: where Tsubame's private bytes go

Task MEM1 / MEM1b (2026-11-18). Budget: private bytes after the 5 s scroll run at 4K
under 110 MB (target 100), and the leak gate (10,000 navigations + 1,000 tabs,
private bytes within 5% of the start, 2 MB floor) passing in every run, with
no speed budget made worse. All numbers are from the release app on the benchmark machine
(Ryzen 9 5900X, RTX 3080, 3840x2064 client area at 192 dpi), measured with
the benchmark harness (`docs/bench.md`); the full runs are in `docs/bench-log.md`.

## Result

| | Before (`7eac782`) | After (production MEM1b) |
|---|---|---|
| Private bytes after the 5 s scroll | 151.3 MB median (154.0 max) | 80.9 MB median, 83.2 MB max (10 launches) |
| Private bytes, idle one tab (gate's idle sample) | 105.8 MB | 63.43 / 63.75 / 63.53 MB |
| Leak gate, 3 runs | FAIL (+11.5 / +3.8 / +5.2 MB) | PASS (+2.61 / +1.87 / +1.88 MB; +3.51 / +2.48 / +2.50%) |
| Cold start to first painted frame | 78.8 ms median, 100.9 ms p95 | 66.3 ms median, 80.9 ms p95 (n=10) |
| Scroll paint p95 | 3.17 ms median, 3.76 ms p95 | 3.10 ms median, 4.03 ms p95 (n=10) |
| 100k warm first rows / sorted frame | 3.79 / 69.3 ms median | 3.87 / 68.28 ms median (9 navigations) |
| Threads in the gate | 50 | 56->55 / 58->57 / 59->58 |

## How it is measured

`cargo build --features memdiag` plus `TSUBAME_MEMDIAG=1` makes the bench and
leak-gate reports end with `memdiag.<label>.*` lines (`src/memdiag.rs`):

- `private_usage_mb`: the process's commit charge (what Task Manager calls
  private bytes / commit size).
- every heap from `GetProcessHeaps`, walked with `HeapWalk`: committed, busy
  (allocated), free-but-committed, and blocks of 64 KB or more.
- `outside_heaps`: a `VirtualQuery` walk of the address space: private
  committed regions that no heap owns, largest first, with their protection
  (`0x4` read-write, `0x404` write-combined).
- `image_writable`: writable image regions of loaded DLLs (not a per-page dirty/shared test)
  by module; attribution evidence, not an exact private-page sum.
- `rust_live`: live bytes of the Rust allocator (a counting `GlobalAlloc`, only
  with the `memdiag` feature).
- `TSUBAME_MEMDIAG=2` also runs `HeapSetInformation(HeapOptimizeResources)`
  after the end sample and samples again (`*_opt`).

The bench report also has a `first_frame:` line (paint start, frame total,
target creation, EndDraw + Present, ms) and the gfx cache sizes.

Rerun one experiment: run the benchmark harness with the scroll stage only, 3 runs,
fixtures kept, the app built with `--features memdiag` and `TSUBAME_MEMDIAG=1` in its
environment.

## Cost per contributor (diagnostic build, end of the 5 s scroll, 4K)

| Contributor | Before | After | What it is |
|---|---|---|---|
| GPU driver DLLs, writable image regions | 33.1 MB | 33.19 MB writable image regions | nvwgf2umx.dll 25.0, nvppex.dll 3.7, nvgpucomp64.dll 3.3. Paid by any process with a D3D11 device on this driver. |
| Driver read-write block | 31.9 MB | 0 | System-memory copy of the window kept by the driver for the HWND render target's GDI-compatible blt-model swap chain. Scales with the window (13.1 MB at 2240x1440). |
| WARP stand-in leftover | 30.2 MB | 0 | Exactly 3840x2064x4: the software HWND render target's frame, never freed after the GPU swap, even with a fresh factory, `IDXGIDevice3::Trim` or the caches cleared. |
| Driver write-combined block | ~20 MB | 14.20-14.69 MB | Driver upload/command memory for the GPU device. |
| Process heap | 33-37 MB committed (busy ~20, free 11-18) | 23.51-23.89 MB committed (19.41-20.16 busy, 3.34-4.45 free) | Busy: Rust ~10 MB (100k-entry store, text layouts, icon pixels) plus DirectWrite, shell and D2D. Free-but-committed: fragmentation. |
| Other (mapped, stacks, small regions) | ~5 MB | 10.1-10.3 MB residual (total minus heap, writable images and write-combined block; categories are approximate) | |
| **Total** | **151 MB** | **81.3-81.8 MB** | |

## What changed

1. **Flip-model swap chain** (`crates/tsubame-d2d/src/gfx.rs`, `Flip`). The
   render target is now a D2D device context drawing into a
   `DXGI_SWAP_EFFECT_FLIP_DISCARD` swap chain (2 buffers, no GDI
   compatibility) on our own D3D11 device, not an `ID2D1HwndRenderTarget`.
   That removes the driver's system-memory copy (31.9 MB) and the WARP
   leftover (30.2 MB), and the write-combined block shrank (~20 to ~14 MB).
   The GPU target reuses the device the warm-up worker made instead of
   dropping it. `TSUBAME_FLIP=0` brings back the HWND target (A/B), and it is
   also the fallback if the swap chain cannot be made.
2. **The software stand-in keeps the first frame fast.** On the flip path the
   first frame first took ~10 ms longer than the HWND target's (frame 45-50 ms
   vs 35-41 ms). Two fixes: the WARP device is made without
   `PREVENT_INTERNAL_THREADING_OPTIMIZATIONS` (WARP rasterises on its own
   threads: ~16 ms of drawing instead of ~24), and it is made on a worker from
   the start of `main` (`prewarm_software`), so target creation in the first
   frame dropped from ~20 ms to ~6 ms.
3. **No driver threads on the GPU device.** Our own hardware device without
   `D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS` started ~70
   NVIDIA threads (106 in the gate instead of 50) and woke the idle process.
   D2D passes the flag for its own devices; so do we now.
4. **Segment heap** (`res/tsubame.manifest`, `heapType SegmentHeap`). The
   leak gate's growth was not a leak: Rust's live bytes were flat (5.38 to
   5.40 MB over 11,000 operations) while the NT process heap's
   free-but-committed bytes grew from 7.6 to 19.2 MB (fragmentation from
   navigation churn). `HeapOptimizeResources` gave back only part of it (6.5-6.9
   MB, with 24 MB still free-but-committed). The segment heap decommits free
   pages: free-but-committed at the scroll endpoint is 3.34-4.45 MB; during navigation it can transiently reach ~8 MB.
   First paint and scroll were the same with and without it (A/B, 2x5 runs
   each).

5. **Finish the WIP diagnostics.** Removed temporary `SMOKE_NAVS` / chunked
   instrumentation from the headless smoke mode: it could silently run fewer
   navigations than its report claimed. Full GUI gate and normal app paths
   are unchanged. No allocator replacement or cache-cap reduction was needed.

## Tried and rejected (numbers)

| Experiment | Private after scroll | Why not |
|---|---|---|
| No software stand-in (first frame waits for the GPU) | 111.5 MB | first paint 252 ms |
| Software rendering only (`TSUBAME_GPURENDER=0`, HWND target) | 101.5 MB | scroll paint p95 19 ms |
| `ID2D1DCRenderTarget` stand-in | 122-124 MB | first paint 117-188 ms: D2D's DC target uses its CPU rasteriser, not WARP; large fills are slow (no text at all still cost 73-94 ms) |
| HWND stand-in + flip-model GPU target | 110-111 MB | the 30.2 MB leftover comes from the HWND stand-in itself |
| `IDXGIDevice3::Trim` + `ClearState` on the stand-in before the swap | 150.7 MB | no change |
| Clear text layouts and formats at the swap | 151.8 MB | no change |
| `HeapOptimizeResources` after the scroll | -0.67 MB | fragmentation is not given back |
| D2D `ENABLE_MULTITHREADED_OPTIMIZATIONS`, single-threaded device, grayscale stand-in text | - | first frame not faster (or slower) |

## What remains

- **GPU driver DLL writable regions, ~33 MB.** VirtualQuery attributes these
  to NVIDIA images; it does not prove every page is private. Only the software path avoids them, and it is too slow
  to scroll at 4K.
- **Driver write-combined memory, ~14 MB.** Per device; the flip chain is
  already the smallest swap chain setup.
- **The process heap, 23.51-23.89 MB committed (19.41-20.16 busy, 3.34-4.45 free).** The 100k-entry store, text layouts
  (1024 cap, 4-frame age) and icons are the Rust part (~10 MB at the end of
  the scroll, 5.4 MB live in the gate). Glyph runs instead of per-row
  `IDWriteTextLayout`, a smaller layout cache, or deduplicated icon pixels
  could each save a few MB; none was needed for the budgets, so none was
  done.
- **Threads.** The WARP device's worker threads stay after the GPU swap (37
  threads with WARP held off, 55-58 with them). They are parked; idle CPU
  stays at one 15.6 ms tick or less per 10 s window.

## Validation and limits (MEM1b)

The merge of `t3/private-bytes-memory-budget` was conflict-free. Local
`check --workspace`, `test --workspace`, and
`clippy -p tsubame -p tsubame-d2d --all-targets --features memdiag -- -D warnings`
passed. Existing ignored helper/doc tests stayed ignored.

Diagnostic release (`--features memdiag`, `TSUBAME_MEMDIAG=1`): full suite
run directory `bench-2026-11-18T06-12-39` has three scroll runs and
three full gates. Another ten launches are in
`bench-2026-11-18T06-18-56`: first paint 73.3 ms median /
101.4 ms p95 (one sample 0.5 ms above baseline); all ten scroll endpoints
79.4-81.6 MB, paint p95 3.64 ms median / 4.90 ms maximum. Do not hide this
startup outlier behind the first three passing samples.

Three extra release navigation processes (three warm navigations each),
`mem1-timing-{1,2,3}.txt` in the full-suite report directory, measured
first-row medians 8.52 / 7.60 / 8.98 ms and sorted-frame medians
101.83 / 95.40 / 108.45 ms. All nine samples met 30 / 200 ms.

The optional purge-and-launch cold-cache probe still fails: 49.08 ms first
rows / 340.57 ms sorted, with process first paint 301.2 ms. Baseline was
66.1 / 317.0 ms with first paint 505 ms. This measures cold app and metadata
together; isolated mid-process metadata purge remains unimplemented. These
failures are retained in the log, not counted as passing warm navigation.
Idle CPU time was 0 ms in the measured 10 s window; exact wake-up count
was unavailable (no Thread counter instances), so zero wake-ups is unproven.
Production release (no `memdiag` feature) is measured separately in
`bench-2026-11-18T06-21-01`: 10 launches, nine warm
navigations, one idle CPU measurement, three full gates. Startup 69.5 ms
median / 80.9 ms p95 meets the baseline comparison. The diagnostic build's
counting allocator adds atomic counters to Rust allocations; its timing
should not be substituted for the shipped allocator's timing. No startup
measurement was removed and the production gate thresholds are identical.
The contributor table uses the diagnostic run, because that run also reports
Rust live allocation bytes. Production scroll memory is 79.7-83.2 MB.
Debug lifecycle smoke on the benchmark machine passed on all five paths: flip, HWND
(`TSUBAME_FLIP=0`), forced WARP (`TSUBAME_GPURENDER=0`), and frame-100
`TSUBAME_TEST_DEVICELOST` injection on flip and HWND. Each process restored
from maximized to 1600x1000, handled synthetic 192->144->192 DPI messages,
minimized/restored/maximized, finished its scroll report and exited 0.
Both injected-loss reports ended on `software (GPU fallback)`, while both
normal modes ended on hardware. Reports are archived alongside the production
suite as `lifecycle-*.txt`. This is behavioral smoke, not pixel comparison;
physical monitor DPI changes, real TDR and actual GPU-less hardware were not
available. Forced WARP verifies the software rendering path, not absent-adapter
initialization. Software scroll is slower than the GPU budget, as expected;
software fallback is for correctness and recovery, not the normal speed gate.

The only post-benchmark source change removes the headless smoke mode's
temporary instrumentation; full gate, startup, paint and allocation paths match
the measured binary. App tests (37) and strict clippy passed again after it.
Cleanup: removed the fixtures, temporary scripts and scheduled tasks, and stopped
the test apps. Raw reports retained.