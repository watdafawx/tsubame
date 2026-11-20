# Preview pane & Quick Look mock (P4.F2)

Mock only — no Rust implementation yet. Open [mocks/mock-preview.html](../mocks/mock-preview.html) directly in any browser (fully self-contained, zero network requests, works from `file://` and inline HTML preview).

URL params for deterministic captures:
`?theme=dark|light&state=image|text|media|audio|hex|multi|unsupported|empty|loading&ql=0|1&only=1`

## Interactive states in the mock

1. **Image (`1`)**: fitted canvas on a subtle checkerboard with `Fit` / `100%` toggle, dimensions badge, zoom percentage, and a pinned metadata strip (Dimensions, Format, File size, Modified, Color space, EXIF/Source).
2. **Text / Code (`2`)**: monospace viewer (`Cascadia Mono`) with line-number gutter, restrained theme-matched syntax tinting, header with language badge, encoding (`UTF-8 · LF`), line count, `Wrap` toggle, and a clean truncation banner (`Showing first 64 KB of 2.4 MB — Open full file`).
3. **Media info (`3` Video / `3b` Audio)**: widescreen 16:9 poster frame (or audio waveform snapshot) with format and duration chips, `Open` button (no embedded video/audio player), and structured metadata table (Duration, Resolution, Codec, Frame rate, Bitrate, Audio codec, Sample rate / Channels, Container, File size).
4. **Hex dump (`4`)**: classic offset | hex bytes | ASCII table (`00000000 | 4D 5A 90 00 ... | MZ..`) with dimmed `00` bytes and non-printable `.` markers, PE/magic header badge, `16 B` / `8 B` row-width toggle, and truncation banner (`Showing first 16 KB of 11.4 MB`).
5. **Multi-select (`5`) / Unsupported (`6`) / Empty (`6b`)**:
   - Multi-select: `"3 items selected — 14.2 MB total"` with selected-item summary and `Space` hint.
   - Unsupported: `"No preview available"` with 40 px icon, type + size subtitle, `Open with default app`, and `View as Hex` fallback button.
   - Empty: `"No item selected"` with `Alt+P` hint.
6. **Loading (`7`)**: shown only if the worker takes `> 80 ms` — thin 2 px accent progress bar plus flat skeleton rects (no animation timer, preserving 0 idle CPU).
7. **Quick Look overlay (`Space`)**: centered modal over the dimmed file list (flat scrim fill, crisp 1 px `--c-popup-border`, no drop shadow), top bar with icon, filename, index (`4 / 17`), prev/next chevrons (`←` / `→`), `Open`, and `Close (Space / Esc)`, plus a single-line metadata & shortcut footer.
8. **Resizable docked splitter**: 5 px hit strip with 1 px hairline (3 px accent + live pixel-width chip while dragging; double-click resets to `320 px`).

## Proposed design defaults (for user approval)

| # | Decision | Proposed default | Alternatives / Notes |
|---|---|---|---|
| 1 | **Default pane width & bounds** | `320 px` default, min `220 px`, max `50%` of window (`600 px` cap); double-click splitter resets to `320 px`; persisted in `config.toml`. | `360 px` wider default. |
| 2 | **Toggle preview pane shortcut** | `Alt+P` (`preview.toggle`), plus a toggle button at the right end of the toolbar. | `Ctrl+Shift+P` is already taken by Command Palette. |
| 3 | **Quick Look shortcut** | `Space` (`preview.quick-look`) when the file list has focus (never triggers inside filter, path bar, or inline rename). `Space` or `Esc` closes; `←`/`→` (and `↑`/`↓`) step through files. | `F3` (classic commander style) as an optional secondary binding. |
| 4 | **Dual-pane behavior** | One shared preview pane docked on the right edge of the window, tracking the **active pane's** focused/selected file (same rule as the shared sidebar). | Per-pane preview split (too cramped on 1080p/1280px). |
| 5 | **Selection debounce & loading threshold** | `50 ms` debounce while stepping rows; generation counter drops stale worker replies (`PLAN.md` §3 rule 1). Loading skeleton appears only after `80 ms` to prevent flicker on fast NVMe reads. | `0 ms` (immediate) or `150 ms` skeleton delay. |
| 6 | **Text / Code preview limit** | Read first `64 KB` max. Sniff UTF-8 / UTF-16 BOM; reject NUL bytes in first `4 KB` (route binary files to Hex/Unsupported). Show truncation banner with full size. | `128 KB` or `256 KB` cap. |
| 7 | **Hex preview limit & columns** | Read first `16 KB` (1,024 rows × 16 B). Classic `16 B/row` in Quick Look and docked widths `≥ 420 px`; auto-switch to `8 B/row` (with manual `16 B / 8 B` toggle) in narrower docked widths so ASCII stays visible without forced horizontal scrolling. | Always `16 B/row` with horizontal scroll. |
| 8 | **Image decode & LRU memory cap** | Worker decodes via WIC scaled to `max_px = 2048` (`Limits::max_px`), premultiplied BGRA. Bounded preview bitmap LRU capped at `32 MB` (or 16 entries, whichever hits first); cleared on pane/tab close to satisfy the 5% leak gate. | `64 MB` cap or `max_px = 4096`. |
| 9 | **Media info extraction** | Lightweight header parser on worker thread (MP4/MKV/WEBM/AVI/MP3/FLAC/WAV/OGG) + shell thumbnail frame via STA worker (`IShellItemImageFactory`). No embedded media player or FFmpeg binary. | Shell property store (`IPropertyStore`) fallback for uncommon containers. |
| 10 | **Extension architecture (`tsubame-ext-api`)** | Register built-ins (`tsubame.preview.image`, `tsubame.preview.text`, `tsubame.preview.media`, `tsubame.preview.hex`) through `Previewer`, so each is switchable and Tier 2 wasm previewers can override any extension. Add optional metadata/truncation/zoom hints to `Doc`/`Node` as needed. | Keep hex as a non-extension core fallback only. |
| 11 | **Syntax tinting tokens** | 6 restrained solid hex colors per theme (`syn_kw`, `syn_ty`, `syn_str`, `syn_num`, `syn_cm`, `syn_fn`) added to `mocks/tokens.json` (all ≥ 4.5:1 on `field`), zero tree-sitter bloat — lightweight keyword/comment/string lexer for common languages. | Plain single-color monochrome text (zero syntax highlighting). |

## Open questions for approval

1. **Hex in narrow docked panes (`< 420 px`)**: auto-switch to `8 B/row` (with `16 B` toggle), or always keep `16 B/row` with horizontal scroll? *(Recommended: auto `8 B/row` below `420 px`, `16 B/row` at `≥ 420 px` and in Quick Look.)*
2. **Binary files with no dedicated previewer** (e.g. `.exe`, `.dll`, `.bin` vs `.iso`, `.zip`): show **Hex** directly for small/known binary formats or show **Unsupported** with the `"View as Hex"` button? *(Recommended: Unsupported + `"View as Hex"` button for archives/ISOs/unknowns; direct Hex only for `.bin`/`.dat`/`.exe`/`.dll` or when user clicks `"View as Hex"`.)*
3. **Syntax tinting**: lightweight built-in keyword/string/comment highlighter for common code extensions (Rust, C/C++, JS/TS, Python, JSON, TOML, Markdown), or plain monochrome text in v1? *(Recommended: lightweight zero-dep highlighter.)*
4. **Secondary Quick Look key**: keep `Space` only, or also bind `F3`? *(Recommended: `Space` only by default; users can rebind in Settings > Keys.)*
