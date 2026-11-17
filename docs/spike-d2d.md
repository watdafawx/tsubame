# Spike S1: Direct2D list, 1,000,000 rows

Task S1 of `PLAN.md` section 7. Question: does blink's renderer (`gfx.rs`, lifted into `crates/tsubame-d2d`) hit the section 2 budgets when it draws a details list of 1M rows at 4K?

**Verdict: speed budgets pass with margin (first paint, scroll paint, exe size). The private-bytes budget (< 40 MB) fails with the GPU render target and cannot pass with this renderer as built.** See "Memory" for numbers and options.

## What was built

- `crates/tsubame-d2d/src/gfx.rs`: copy of `blink\src\gfx.rs` (software first frame, GPU swap on a warm-up worker, device-loss fallback, layout cache, bitmaps).
- `src/main.rs`: Win32 window, per-monitor v2 DPI, `GetMessageW` loop with no timers (the bench-only 10 ms ticker exists only under `--bench`). `WM_PAINT` + `InvalidateRect`.
- `src/view.rs`: details view (name, size, modified, type), header, status bar, scrollbar (thumb drag, track click), selected-row highlight. Wheel (3 rows per notch), Up/Down/PageUp/PageDown/Home/End, click to select. Only rows that intersect the viewport are fetched and drawn.
- `src/data.rs`: fake rows. `Source::Procedural` (pure function of the row index, zero storage) or `Source::Store` (`--store`): one name buffer + 24-byte records (`name_off u32, mtime u32, size u64, name_len u8, kind u8`). No `String` per row either way.
- `src/bench.rs`: `--bench` (or env `TSUBAME_BENCH=1`). `--windowed` keeps the 1120x720 DIP window instead of maximizing.

## Method

- Rig (Ryzen 9 5900X, RTX 3080, one 4K display at 200%). Release profile (`lto`, `codegen-units=1`, `strip`, `panic=abort`), built on rig with `push-to-rig.ps1 -BuildOnRig -Profile release`. Window maximized: 3840x2019 px at 192 dpi.
- First paint: `GetProcessTimes` creation time to the end of the first completed `WM_PAINT`, both on the precise system FILETIME clock. This is the software stand-in frame, as in blink (the GPU target arrives later, see below).
- Scroll paint: the bench waits for the GPU target, then scrolls for 5 s on a ~60 Hz timer, timing every `WM_PAINT` (begin to `EndDraw` inclusive; std `Instant`, which is QueryPerformanceCounter on Windows). Script: A 2 s steady fling (2,000 DIP/s, ~1.5 new rows/frame), B 2 s fast fling (20,000 DIP/s, ~15 new rows/frame), C 1 s one full page per frame (every string on screen is new: worst case). About 320 frames per run.
- Private bytes: `GetProcessMemoryInfo` `PrivateUsage`. Exe size: file size of the stripped release exe.
- Raw report per run: `s1-bench.txt` next to the exe.

## Results against the budgets (4K, 3 runs, default settings)

| Metric | Budget | Measured | |
|---|---|---|---|
| Cold start to first painted frame | < 100 ms | 64.9 / 68.2 / 72.2 ms (4K); 60-71 ms windowed | **pass** |
| Scroll paint p95, 4K, details view | < 5 ms | 3.66 / 3.75 / 3.75 ms (p50 2.6, p99 4.1-4.2, max 4.9) | **pass** |
| Private bytes, one tab, small folder | < 40 MB | 73 MB at first paint (4K software frame); 160 MB peak while the GPU target swaps in; 111 MB steady after scrolling (4K). Windowed 2214x1369: 36 MB first paint, ~85 MB steady | **fail** (steady state) |
| Main exe | < 6 MB | 0.23 MB (243,712 bytes) | **pass** |
| Idle CPU | 0 wake-ups | 0.00 s CPU over 5 s idle (checked with the window open, `Get-Process`) | pass |

Paint by phase (4K, three runs): A steady fling p95 2.6-2.8 ms, B fast fling p95 3.4-3.6 ms, C page-per-frame p95 4.1 ms (max 4.9). `EndDraw` alone (rasterise + present on the GPU): p50 0.78, p95 ~1.0 ms.

First paint timeline: software target paints the first frame at ~65-72 ms; the GPU device is ready ~285-295 ms after process start and the next paint swaps to it (this is blink's design, unchanged).

## Memory (the one failure)

| Config | first paint | before scroll (after swap) | end of 5 s scroll |
|---|---|---|---|
| 4K, GPU, default | 73 MB | 160 MB | 111 MB |
| 4K, software only (`TSUBAME_GPURENDER=0`) | 73 MB | 73 MB | 161 MB (layout cache 4096, see below) |
| windowed 2214x1369, GPU | 36 MB | 105 MB | 85-90 MB |
| windowed, software only | 37 MB | 37 MB | 113 MB (cache 4096) |
| 4K, GPU, `--store` (1M real records) | 123 MB | 210 MB | 161 MB |

- The store itself costs **45.2 MB** for 1M rows (24 B records + names, 47.4 MB reserved) and builds in **~47 ms**. That fits the "1M entries in about 60 MB" rule in section 3.
- A hardware render target costs about 45-50 MB of private commit over the software one (driver heaps), consistent with blink's own numbers (`gpu-render.md`: 83 MB GPU vs 60 MB software at 2214x1427). Render-target size matters too: 4K software first frame is 73 MB, windowed 36 MB.
- Software (WARP) rendering reaches ~37 MB windowed but paints at p95 7.4 ms windowed / 18.5 ms at 4K, which fails the paint budget.
- So with `ID2D1HwndRenderTarget` the two budgets conflict. Not solved here. Options for the master session: (a) relax the memory budget to a number measured with the GPU target (~90 MB windowed, ~110 MB at 4K) and keep the leak gate (return to within 5% of the start) as the real guard; (b) spike a DXGI flip-model swap chain + `ID2D1DeviceContext` with one shared D3D11 device (smaller driver footprint is plausible but unmeasured); (c) accept software below some window size. I recommend (a) now, (b) as a later spike.

## Changes from blink's `gfx.rs`

- Removed blink-only widgets: `spark`, `series`, `dash_h`, `grid`, `grid_ready`.
- `crate::osutil::log` and `crate::timing::*` replaced by `tsubame_d2d::log` (`OutputDebugString`). The `stall:` timing lines are gone.
- Env vars renamed: `BLINK_GPURENDER` to `TSUBAME_GPURENDER`, `BLINK_TEST_DEVICELOST` to `TSUBAME_TEST_DEVICELOST` (debug builds).
- Workspace edition is 2024: renamed a local called `gen` (reserved keyword). Everything else compiled unchanged against `windows = "0.61"`; `windows-numerics = "0.2"` is still needed for `Vector2` (it is not re-exported under `windows::Foundation::Numerics` in 0.61).
- **Layout cache re-tuned.** Blink kept up to 4096 shaped `IDWriteTextLayout`s for 30 frames. A scrolling list draws mostly new strings, so that cache only held dead layouts: at 4K it added ~60-70 MB private bytes (173 MB end vs 111 MB) with no paint-time gain (cap 0, no cache at all, gave the same p95; steady frames were ~1 ms slower p50). Now cap 1024 and 4 frames. Both are overridable for experiments: `TSUBAME_LAYOUT_CAP`, `TSUBAME_LAYOUT_AGE` (cap 0 = no cache).

## Surprises and notes

- **f32 scroll offset is not enough.** 1M rows x 22 DIP = 22M, f32 is only whole-pixel exact to 16.7M. `View::scroll` is `f64`. `tsubame-ui` must do the same (or keep first-row index + sub-row offset).
- Build time on rig, release: 19 s cold (windows crate + lto), 0.6-4 s incremental.
- A default-sized `DrawTextLayout` per cell is cheap enough: ~45 rows x 4 columns at 4K paints in 2-3 ms total.
- Benchmarks on rig shared slot 1 with another session's exes (`plughost-*`), so runs used `-Exe tsubame -Slot 2`. Always pass `-Exe` when several crates build in one repo.
- Run-to-run noise: p95 varies about +-0.3 ms; first paint about +-6 ms; first paint also depends on what else rig is doing (one `--store` run showed 134 ms while rig was busy, 110 ms another time).
- Interactive check on rig (screenshots, 4K): initial list, `End` (last row 1,000,000 sits exactly at the bottom, status bar "showing 999972-1000000"), wheel up, `PageUp`, scrollbar thumb drag to ~44% (rows ~444,870). All correct.

## Rendering API surface used (seeds the `Canvas` trait)

Used by this spike: `size()` (DIP), `begin(bg)`, `end()`, `fill(rect, color)`, `round(rect, radius, color)`, `text(str, rect, font, color)`, `clip(rect)`, `unclip()`. Backend lifecycle (not part of the trait): `new(hwnd)`, `set_dpi`, `resize`, `path()`, `release()`.

Recommended `Canvas` set (about 15, all present in `Gfx`): `size`, `begin`, `end`, `fill`, `fill_a`, `round`, `round_a`, `round_stroke`, `line`, `circle`, `text` (single line, vertically centred, ellipsis, clipped; font = size, semibold, mono, icon face, alignment), `measure`, `image` (premultiplied BGRA, cached by key), `clip`, `unclip`. Colours are `0xRRGGBB` + alpha; rects are DIP `f32` with the DPI applied by the target.
