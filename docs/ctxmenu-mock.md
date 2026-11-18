# Context menu mock (M3)

Mock only, nothing implemented. Open `mocks/mock-ctxmenu.html` in a browser (works from `file://`, designed for ~1100 px wide). State bar at the top switches between states; Theme, Density, Font scale, Quick row and Accelerator underlines are live switches. URL params for screenshots: `?only=<section id>&theme=light|dark&dens=22|26|30&sc=100|125&quick=0|1&ul=0|1`.

Screenshots: `mocks/shots/ctx-<id>-light|dark.png` (ids: look, full, kinds, quick, long, flip, submenu, filter, editmenu, editsettings, loading, density, decisions).

## States

1. **Look**: one specimen menu with every row kind, light and dark, plus the eight row states and the token each uses (hover = `selected`, keyboard focus = `selected` + 1 px `focus` ring, pressed = `accent` fill with `on_accent`, open parent = `selected_inactive`).
2. **Full file menu**: Windows verbs, 7-Zip, Git, Defender, Share, Send to, and the Tsubame group (Preview, Open in other pane, Copy path as, Compare, user entries with an accent dot). Open-with, Send to, Git, Copy-path submenus drawn detached.
3. **Kinds**: folder, empty area, 4 items, drive.
4. **Quick row**: icon row (Cut, Copy, Paste, Rename, Delete, Share) vs plain list.
5. **Long, flip, keys**: (a) scroll strips vs two columns, (b) flip left/up near the window corner, (c) submenu hover with safe-zone triangle and key map, (d) type-to-filter with match highlight.
6. **Edit mode**: (a) edit menu on a right-clicked entry, inline rename, strip after Hide; (b) Settings > Context menu editor (eye, rename, grip, group headers, search, chips, Reset, Nilesoft import states, note that native items can only be hidden).
7. **Loading**: t = 0 / 80 / 300 ms with a thin loading line, and a failure frame with Retry.
8. **Density and scale**: 22 / 26 / 30 at 100 % and 125 % font scale.
9. **Decisions**: choices made and four open questions.

## Decisions

- Own-drawn from the real HMENU (`IContextMenu::QueryContextMenu` on a worker thread); invoke by command id. Own verbs are run by the app.
- Popup look: `popup` fill, 1 px `popup_border`, radius 8, padding 4, no shadow, no gradient. Menu width 232 to 360.
- Row height = max(density, round(text x 1.75)); icons scale 16 to 20 at 125 %.
- Icons: Tsubame mono glyphs for known verbs, the shell's HMENU bitmap in colour for unknown items. Icon column is always reserved.
- Groups: Tsubame group has a header, Windows items keep native order inside their group, user entries carry an accent dot.
- Edit by right-clicking an entry (no separate mode). Hidden items are not listed in the menu, only in Settings, with Undo in a strip.
- Items are identified by verb + handler CLSID, not label, so hide and rename survive updates and translations. New items show up visible, tagged New.
- Nothing native is removed from Windows, and the editor says so.
- Loading: own items at t = 0, thin line while Windows answers, rows arrive in place, 120 ms misclick guard, Retry row after 3 s.
- Edges: 8 px keep-out, flip left/up, a taller menu scrolls (chevron strips, wheel, thumb), rows never shrink.
- Accelerators: underlines show only while Alt is held or after a keyboard open; letters filter, accelerators become Alt + letter.
- Submenus: 150 ms open, 250 ms close, safe-zone triangle.

## Open questions (recommendation first)

1. Quick-action icon row on top? **No, plain list**, offered as a Settings toggle later.
2. Submenu hover delay? **150 ms open / 250 ms close with the safe triangle**; alternatives: 400 ms (Windows default), click-only.
3. Menu taller than the screen? **Scroll with chevron strips**; two columns as a fallback idea, shrinking rejected.
4. Typing in an open menu filters it? **On**, accelerators move to Alt + letter; alternatives: off, or filter only after `/`.

Out of scope: Win11 `IExplorerCommand` items ("Show more options") may be missing from `QueryContextMenu`; a later spike. Accessibility (UI Automation tree, High Contrast) is plain recolouring of the tokens.
