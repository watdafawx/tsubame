# App icon options (B12-C)

Mock: `mocks/icon-options.html` (open in a browser; 4 options x 16/20/24/32/48/256 px, dark and light, taskbar and title-bar strips, current icon on top for comparison).

## Why the current icon looks wrong
1. `res/tsubame.svg` rotates the swallow 40 deg on a tile. At 16-32 px it reads as a diagonal "shortcut arrow".
2. The in-app title bar never draws the swallow: `src/view.rs` `paint_titlebar` (about line 2168) paints a navy rounded square with the text glyph `\u{2197}` (an arrow). Same arrow on the Updates settings page (`src/settings/updates_page.rs:144-146`). `mocks/mock.js` `logo()` uses the small-cut SVG, so the mock and the app disagree.
3. `src/main.rs` (about line 453) registers the window class `TsubameMain` with no `hIcon` / `hIconSm`, and nothing sends `WM_SETICON`. The exe icon (`res/tsubame.rc`, resource 1) is only used by Explorer and pinned shortcuts. The running window's title/Alt-Tab/taskbar-thumbnail icon falls back to the system default.

## Options (see mock)
- **A** top-down, navy tile, cream swallow, red throat (recommended)
- **B** side-on, wings up, navy tile
- **C** A inverted: cream tile, navy bird (best on dark taskbars)
- **D** navy tile, red sun disc, cream side-on swallow

Small cut (16/20/24) is an integer 16-grid polygon, no red throat, so it stays pixel-snapped. Large cut keeps the curves and red accent.

## Recommendation: A, with C as the fallback
- Symmetric upright silhouette with a forked tail cannot be read as an arrow.
- Keeps brand navy/cream/red.
- Caveat: navy tile is dark on a dark taskbar (same as today). If that bothers the user, pick C.
- At 16 px the top-down shape is a swept "anchor" with a notched tail; B/D side-on look more like a bird but are weaker as a tiny silhouette.

## Steps to ship the chosen option (about 45 min)
1. Replace `res/tsubame.svg` with the chosen big cut (copy the option's `big()` markup into a `viewBox="0 0 100 100"` svg; for A/C expand the mirrored half path).
2. Replace `res/tsubame-small.svg` with the small cut (`viewBox="0 0 16 16"`: tile rect + polygon).
3. `tools/make-icon.ps1`: the render loop assumes a 100-unit viewBox (`density 0.96*ss*n`). For the 16-unit small svg use `density = 6*ss*n` (or rewrite the small svg on a 100 grid with coordinates x6.25).
4. Run `pwsh -NoProfile -File tools\make-icon.ps1 -PreviewDir <temp>` and look at the 16/24 PNGs; then `-VerifyExe` on a built exe.
5. In-app title bar: replace the `\u{2197}` text in `paint_titlebar` with the real mark (draw polygon path via Canvas, or blit a cached 18 px bitmap made from the small cut). Same for `updates_page.rs` logo (use master cut). Update `mocks/mock.js` `logo()`.
6. Window icon: in `src/main.rs` set `hIcon` / `hIconSm` on the `WNDCLASSEXW` via `LoadImageW(hinst, MAKEINTRESOURCE(1), IMAGE_ICON, GetSystemMetrics(SM_CXICON/SM_CXSMICON), ...)`, and send `WM_SETICON` ICON_BIG/ICON_SMALL after `CreateWindowExW` (also on `WM_DPICHANGED`, resizing from the same resource so 16/20/24 hit the hand-tuned frames). Consider adding 20 and 40 px frames to the ico plan (125% / 250% DPI taskbar sizes).
7. Update `docs/PLAN.md` S0b, `docs/img/banner.svg` (copy of tsubame.svg), `mocks/logo-final.html`.
8. Rig check (push-to-rig): taskbar, Alt-Tab, thumbnail, title bar at 100%/200%.
