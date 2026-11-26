# tsubame-ui: the immediate-mode kit (U1)

`crates/tsubame-ui` is portable: it has no `windows` dependency. The platform layer turns window messages into `Event`s and implements `Canvas`. Direct2D/DirectWrite does that in `tsubame-d2d` (`impl Canvas for Gfx`), and `src/main.rs` translates the Win32 messages. Tests run against `TestCanvas`.

## Canvas

Every coordinate is f32 DIPs. Colours are `0xRRGGBB` (`Color = u32`).

| Call | Purpose |
|---|---|
| `size() -> (w, h)` | target size in DIPs |
| `begin(bg) -> bool` / `end()` | frame bracket. `false` means skip the frame (device lost) |
| `fill(r, c)`, `fill_a(r, c, a)` | rectangle, opaque or with alpha |
| `round(r, radius, c)`, `round_a(..)`, `round_stroke(r, radius, c, w)` | rounded rect, filled or outlined |
| `line(x0, y0, x1, y1, c, w)`, `circle(x, y, radius, c)` | strokes and dots |
| `text(s, r, font, c)` | one line, clipped to `r`, vertically centred, aligned by `font.align` |
| `measure(s, font) -> f32` | advance width |
| `caret_x(s, font, byte_idx)`, `hit_index(s, font, x)` | caret placement and hit-testing. Defaults use `measure`; Gfx overrides them with DWrite hit-testing |
| `image(key, w, h, px, r)` | BGRA premultiplied pixels, cached by `key` |
| `clip(r)` / `unclip()` | axis-aligned clip stack |

`Font` is a small `Copy` value: size in tenths, `Weight`, `Face` (Ui/Mono/Icon), `Align`. Build it with `f(12.0).b().r()`. U2 can add calls with default bodies without breaking implementors.

## Ids, hot, active, focus

- `Id` is an FNV-1a hash: `Id::new("details")`, `id.with(row)`, `id.child("sb")`. It is the same value every frame, with no allocation.
- `ui.interact(id, rect, Sense)` registers a hit rect and returns an `Interaction`: hovered, active, pressed, clicked, released, wheel, focused.
- **Hit-testing** uses the previous frame's rects. The last rect registered (topmost) wins, so popups drawn later take the mouse first.
- **Hot** is the topmost hit under the mouse.
- **Active** starts at the press and ends at the release. While a widget is active, the platform holds mouse capture (`Output.capture`).
- **Focus** moves to a focusable widget when it is pressed. Tab and Shift+Tab walk the focus chain from last frame's order. `set_focus` moves it in code.
- `FocusLost` clears active and closes popups.
- `ui.claimed` chords (and a claimed Tab) skip the window-level handling for the next frame, so a focused widget (an autocomplete list on a text field) gets them. A widget that has the focus claims them again each frame it draws.
- `Sense.cells` reports hover per row or per tab. A mouse move inside one row then causes no repaint.

## Events and frames

- `ui.push(Event) -> bool` queues an event and returns whether a frame is needed. Mouse moves that change no hover state and key-ups return `false`, so the platform does not invalidate for them.
- Events: `MouseMove`, `MouseDown{button, mods, double}`, `MouseUp`, `Wheel{dy, dx}` (notches, +1 = up), `MouseLeave`, `KeyDown{key, mods, repeat}`, `KeyUp`, `Char`, `FocusLost`, `FocusGained`.
- IME: the platform calls `ui.set_composition(s)` with the composition string and pushes the result as `Char`s. The text input draws the composition inline and reports `Output.ime_caret`.
- **A frame:** `begin_frame(size)`, then widgets draw and react, then `end_frame() -> Output{repaint, capture, cursor, ime_caret}`. The frame runs inside `WM_PAINT`, and queued input is applied during it.
- `Output.repaint` is set only when an animation is running or a widget called `request_repaint` (buttons and tabs do on click, so the app sees the new state). Nothing else redraws, and there are no timers while idle.
- **Keyboard routing**, in order:
  1. Escape closes the top popup.
  2. Global shortcuts (`ui.shortcuts.bind(key, mods, cmd, true)`) fire.
  3. Tab moves the focus.
  4. The focused widget takes keys through `key_input(id)` + `consume(i)`. A text input swallows all `Char`s and its edit keys.
  5. Leftover keys try the non-global shortcuts, then go to the app through `ui.leftover()`.
  6. Fired commands are in `ui.commands()`.
- **Popups:** `open_popup(id)`, then `popup_begin(c, id, rect)` … `popup_end`. A click outside all popups closes them and is eaten. Escape closes the top one. On close, focus goes back to whatever had it before.

## Widgets

| Widget | Notes |
|---|---|
| `list(ui, c, id, rect, &mut ListState, &mut impl ListSource)` | Fixed row height, f64 scroll, only visible rows are drawn. Wheel moves 3 rows; arrows, PgUp/PgDn, Home/End move the cursor. Selection goes to the app via `ListSource::select(i, mods, SelectBy)`. Returns range, activated (Enter or double-click), context (right-click), and Shift+F10 / menu key. |
| `vscrollbar` | Thumb drag (keeps the grab offset) and track click (pages). Used inside `list`. |
| `text_input` | Caret, selection, mouse and double-click word select, Home/End, Ctrl+arrows, Backspace/Delete (with Ctrl: whole word), Ctrl+A/C/X/V through `ui.clipboard`, IME slot. Scrolls horizontally to keep the caret visible. Measures text only while focused. |
| `button`, `tab_strip` (add, close, drag-reorder, overflow scroll), `splitter` | |

**Writing a widget:**
1. Compute the rect.
2. Call `ui.interact(id, rect, sense)`.
3. If focused, read `ui.key_input(id)` and `consume` what you used.
4. Draw with `c`, using `ui.theme` for colours and metrics.
5. Return a small response struct.
6. Keep state in a caller-owned struct (`ListState`, `TextInput`, `TabStrip`). The kit stores no widget state beyond ids.

## Tests

`cargo test -p tsubame-ui` runs 17 tests:
- layout
- id stability
- topmost hit
- hot/active transitions
- focus and Tab chain
- idle frames asking for nothing
- only visible rows drawn (off-screen rows never reach the canvas)
- list maths and scrollbar at 1M rows
- selection hooks
- text editing keys
- typing, mouse and composition in the widget
- caret scrolling
- popup dismissal
- keyboard routing order
- tabs
- splitter

The app has 2 more: the selection model and the case-insensitive filter.

## The spike app on the kit

`src/view.rs` draws the toolbar with the filter box, the header, the details list and scrollbar (`list`), the thumbnail panel and the status bar. `src/main.rs` keeps the Win32 loop and the S4 shell workers, and translates messages into `Event`s. Shortcuts: Ctrl+F focuses the filter; letters T/H/W reach the app through `leftover()`.

## Measured on the benchmark machine (4K, 3840x2019 client at 192 dpi, release, 1M procedural rows, `--bench`)

| Metric | Budget | Kit (U1), 3 runs | Same day, pre-kit `ccd7dfd`, 2 runs | |
|---|---|---|---|---|
| Scroll paint p95 | < 5 ms | 3.56 / 3.78 / 3.75 ms (p50 2.5-2.6) | 5.78 / 3.93 ms | **pass** |
| First paint | < 100 ms | 73.6 / 66.6 / 55.1 ms; `--dir System32` 66 ms | 61.7 / 60.2 ms | **pass** |
| Idle CPU | 0 | 0 ms CPU over 5 s idle (`--dir`, window open) | | **pass** |
| Private bytes end of scroll | < 110 MB | 113.6 / 115.3 / 115.9 MB | 117.1 / 114.6 MB | **fail** (also fails without the kit) |

- **Memory.** The kit adds no memory. Pre-kit S4 measured 115-117 MB on the same day, while S1 recorded 111 MB earlier.
- **Where the bytes are.** The GPU device swap peaks at ~165 MB. The steady number is DirectWrite heap after a 5 s scroll that makes new layouts every frame.
- **Layout cache size.** `TSUBAME_LAYOUT_CAP=256` gives 106.5 MB but p95 4.74 ms, because 256 is smaller than one 4K frame of strings. Cap 0 gives 130.6 MB, so the cache is not the cause.
- **Fix.** Drawing row text as glyph runs instead of `IDWriteTextLayout` is the next step for this budget (U2 or later).
- `--dir C:\Windows\System32` (5013 entries), driven by mouse and keys and checked in screenshots:
  - filter "sys": 118 of 5013 match (0.0 ms)
  - Tab to the list, Down, Shift+Down ×2: 3 selected
  - scrollbar track click: pages
  - thumb drag to the bottom: shows rows 91-118
  - right-click: selects the row and opens the real shell menu from the STA worker
