# Icons, thumbnails and the grid view (W2)

How file icons and image thumbnails get from the shell to the screen without
ever blocking the UI thread, and how the grid view draws them.

## Pipeline

```
paint (UI thread)                    shell pool (STA workers)             UI thread
─────────────────                    ────────────────────────             ─────────
cells/rows ask for pictures  ──►  icon lane  : 2 workers (icons, menus)
  visible = priority 0              thumb lane : 2 workers (thumbnails only,
  prefetch = rows away                           lazy, BELOW_NORMAL)
end_frame: cancel what nobody         │
  asked for, queue nearest first,     ▼
  submit up to the in-flight caps   reply + coalesced PostMessage ──► WM_APP_SHELL
                                                                      apply() → LRU
                                                                      refill() → repaint
```

* `tsubame-win::shell::Pool` has two **lanes**. The icon lane runs icons, shell
  menus and the hang test. The thumbnail lane runs `Request::Thumb` only, so a
  slow decoder (a huge RAW, a network share) can never stall icons or a menu.
  Each lane has its own queue, semaphore, hang watchdog (thumbnail threshold is
  3x the icon one) and STA workers. `Pool::new` keeps one lane; the app uses
  `Pool::with_lanes(icon_workers, thumb_workers, ...)`.
* Replies carry the request id. A reply whose id is no longer pending is
  dropped (`stats.stale`), so cancelled work cannot repaint an old folder.
* The UI side is `src/shellui.rs` (`ShellUi`). It keeps a **want list** that is
  rebuilt every frame: each cell or row that needs a picture registers a want
  with a priority (0 = on screen, N = N rows away, prefetch). `end_frame`
  1. cancels pending requests that no frame stamped this time (they left the
     viewport + margin),
  2. sorts this frame's misses by priority, then draw order,
  3. submits while the lane has room (`ICON_INFLIGHT` 16, `THUMB_INFLIGHT` 8).
  What does not fit waits in the queue; every reply calls `refill`, so the
  queue drains without repainting.

## What is requested

| What | When | Key |
|---|---|---|
| Visible cells/rows | always | see below |
| Prefetch | one page above and below, only as many cells as the thumbnail cache can hold | same |
| Anything else | never | |

Cache keys:

* `e:.ext@px` – icon shared by extension (the common case),
* `folder@px` – the plain folder icon,
* `p:<path>@px` – per-file icon: `.exe .lnk .ico .cur .ani .url .scr .cpl .msc .appref-ms`, and
  folders with READONLY/SYSTEM (custom icon via desktop.ini),
* `t:<path>@px` – thumbnail of one file (extensions in `THUMB_EXT`: images,
  RAW, video, pdf...). Other types show their large type icon.

Thumbnails ask `IShellItemImageFactory` / `IThumbnailCache` for a bitmap sized
to the cell: `min(box px rounded up to 16, size so the visible cells fit 85 %
of the cache)`. The result is fitted inside that square (aspect kept).

## Caches (all bounded in bytes)

All numbers live at the top of `src/shellui.rs`.

| Cache | Cap | Items | Notes |
|---|---|---|---|
| Icons (`ShellUi::icons`) | 6 MB (`ICON_CAP_BYTES`) | 2048 | by extension: a few hundred KB in practice |
| Thumbnails (`ShellUi::thumbs`) | 12 MB (`THUMB_CAP_BYTES`) | 4096 | pixels until the first draw, then only the device copy; cleared on every navigation |
| Device bitmaps (`Gfx`) | 16 MB (`BITMAP_CAP_BYTES`) | | the only copy of a drawn thumbnail; frame-stamped LRU evicting to 90 % |

`Lru` evicts least-recently-used entries until both the item and the byte cap
hold. Thumbnails are charged `w*h*4`; a failed or empty one costs a few bytes
so it is not asked for again.

**One copy.** A thumbnail arrives as CPU pixels (`Thumb::px`). The first frame
that draws it uploads it (`Canvas::image_a`) and then drops the pixels, so the
device cache is the only copy. If the device cache later evicts it (or the
render target is recreated) `paint_pic` sees `!has_image && px.is_empty()`,
removes the entry (`Lru::remove`) and the next frame asks the shell again
(`IThumbnailCache` makes that cheap). `BITMAP_CAP_BYTES` must stay above
`THUMB_CAP_BYTES` plus the icons in use, or visible cells would thrash.

## Cancel rules

1. **Left the viewport + margin**: not re-stamped by a frame → cancelled by the
   next `end_frame` (the worker skips it if it has not started; a running one
   finishes and its reply is dropped as stale).
2. **Navigation / tab switch / refresh**: `Tab::nav_key()` changes;
   `ShellUi::sync_generation` cancels everything, clears the want list and
   empties the thumbnail cache. So 10,000 navigations leave nothing behind.
3. **Tab closed**: same as 2 for the next tab shown.
4. **Hung worker**: the watchdog replaces it; `forget` drops the lost ids so
   the cells ask again.

## Drawing and the fade

A grid cell draws the file-type icon at once (placeholder shape until even that
arrives), then the thumbnail on top at opacity `age / 100 ms`. The age starts
at the first frame that draws it. While a fade runs `needs_frame()` is true and
`main.rs` invalidates once more after the paint; when the fade (or the upload
backlog) ends, so does the repaint loop. **No timer exists**, so an idle window
has 0 wake-ups.

New device bitmaps cost an upload: at most `UPLOADS_PER_FRAME` (6) new ones are
drawn per frame (`Canvas::has_image`); the rest are shown next frame (counted in
`stats.uploads_deferred`). This keeps a fast scroll through 5,000 images from
making one long frame.

## Grid view

* Modes: **Ctrl+Shift+1** details, **Ctrl+Shift+2** grid-small, **Ctrl+Shift+3**
  grid-large, or the toolbar button (cycles). One global setting (`View::mode`);
  `--view details|grid-small|grid-large` sets the start mode. The cursor entry
  is kept when switching.
* Cell size (DIPs): small 96 x 118 (64 box), large 152 x 182 (128 box). Name
  below, at most two lines, the second ends in an ellipsis (`Canvas::first_line_len`
  asks DirectWrite; the split is cached in a fixed 1024-slot table).
* `tsubame-ui::grid` is the widget: virtualised (only cells that touch the
  viewport are drawn), left-aligned cells, columns = floor(width / cell width),
  resize reflows keeping the top visible row.
* Keys: arrows, Home/End, PageUp/PageDown, Enter, Space, Shift/Ctrl multi-select,
  Menu / Shift+F10. Typing starts the filter box (type-ahead = filter-as-you-type,
  as in details).
* Mouse: click, Ctrl/Shift click, double click, right click menu, wheel (step
  `max(3 rows, 0.6 cell)`), kit scrollbar.
* **Rubber band**: press on empty space, drag past a 3 DIP dead zone. The band
  selects the cells it touches; Ctrl keeps the selection from before. Dragging
  past the top/bottom edge autoscrolls 5 to 30 DIPs per frame.
* Sort: there are no columns, so a strip ("Sort by Name | Date modified | Type |
  Size") shows the same sort; a second click flips the direction. Filtering is
  the normal filter box.
* Style: flat rects (M1), selection / hover / focus from theme tokens, no shadows.

## Tuning

| Knob | Where | Effect |
|---|---|---|
| `--workers N` | CLI | icon-lane workers (default 2) |
| `--thumb-workers N` | CLI | thumbnail-lane workers (default 2; 0 = thumbnails share the icon lane) |
| `ICON_INFLIGHT`, `THUMB_INFLIGHT` | shellui.rs | requests in flight per lane (16 / 8) |
| `ICON_CAP_BYTES`, `THUMB_CAP_BYTES` | shellui.rs | cache caps |
| `UPLOADS_PER_FRAME` | shellui.rs | device uploads per frame (6) |
| `FADE_MS` | shellui.rs | fade length (100) |
| `PREFETCH_FILL` | shellui.rs | share of the thumbnail cache the visible + prefetch cells may use (0.85) |
| `BITMAP_CAP_BYTES` | tsubame-d2d gfx.rs | device bitmap cache |
| `TSUBAME_ICON_PER_PATH=1` | env | per-path icons for every file (worst case) |
| `TSUBAME_ICON_FACTORY=1` | env | IShellItemImageFactory for per-path icons |

To measure: `tools/bench-rig.ps1 -BenchArgs '--view grid-small'` scrolls the grid
instead of the list; the bench report has a `thumb cache:` line (entries, MB,
evictions, device bitmaps, requested px, in flight / queued at the end).
`--stress thumbs` hammers the thumbnail lane directly.

## Memory

Private bytes after a scroll must stay under 110 MB at 4K (details: ~69 MB).
Measured with `TSUBAME_MEMDIAG=2`: the heaps stay around 18 MB; the growth in the
grid is the **device bitmaps**, which the driver backs with roughly 1.5x their
size in private memory (19.8 MB of bitmaps showed up as a 29 MB region). So the
byte caps are the memory knob:

| Caps (thumb / device) | grid-small at end | grid-large at end |
|---|---|---|
| 24 / 32 MB, CPU copy kept | 154 MB | 151 MB |
| 24 / 28 MB, CPU copy dropped | 115 MB | |
| 16 / 20 MB | 110 MB | 116 MB |
| **12 / 16 MB (now)** | 109 MB | 106 to 108 MB |

The thumbnail request size shrinks with the cap (`thumb_px`: the visible cells
must fit 85 % of it), so a 4K screen of large cells asks for 192 px instead of
256 px. To trade quality for memory or back, change `THUMB_CAP_BYTES` and keep
`BITMAP_CAP_BYTES` about 4 MB above it. One thumbnail worker instead of two
saves about 5 MB but fills slower.
