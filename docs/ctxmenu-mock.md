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
- Groups: no app-name header or caption rows in popups. App-owned items and group separators remain; Windows items keep native order inside their group, and user entries carry an accent dot.
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

## Approved answers (user, 2026-11-19)

All four defaults accepted, with one change to question 3:

1. Quick-action row: off by default (plain list), Settings toggle later.
2. Submenu delay: 150 ms open, 250 ms close, safe triangle.
3. Menu taller than the screen: first try the Windows 11 trick. When the menu does not fit, fold the common actions (Cut, Copy, Paste, Rename, Delete, Share) from their text rows into one icon-only row at the top (variant A in state 4), which frees about 5 rows. Only if it still does not fit, scroll with chevron strips. Rows never shrink.
4. Typing filters the open menu; accelerators become Alt + letter.

Follow-up approval: remove the small "Tsubame" group label from every popup,
including loading/failure states. No app-name captions in context menus; custom
entries retain their accent dots. Window titles and Settings labels are unchanged.

## Gap states (proposals, not approved)

`mocks/mock-ctxmenu-gaps.html` (same tokens and builders; same URL params plus `?only=rowmenu|refresh|pin|groupname|nss|kinds`). Shots: `mocks/shots/ctxgap-<id>-light|dark.png`, made by `pwsh -NoProfile -File mocks/shoot-ctxmenu-gaps.ps1`. Covers the gaps listed in `docs/u4-settings.md` ("Not done") and `docs/ctxmenu.md` ("Settings editor: what is still not built").

1. **Row "..." menu** (`rowmenu`). One fixed list for every row, here and in the menu-side edit menu: Hide / Show, Rename… (F2), Pin to top / Unpin, Move to group ▸ (current group checked, New group…, Native place), Move up, Move down (Alt+Up / Down), then for custom and imported rows Edit entry… and Delete entry; the menu-side copy adds Edit in Settings…. Rows that do not apply are disabled with a one-line reason (Windows' and apps' items: Move up / down; Tsubame rows: Pin and groups), except Delete, which exists only on custom rows.
2. **Refresh from last menu** (`refresh`). Stays in the second toolbar row of 6b, between Import and Reset. Idle (with "Last menu: 14:32, 31 items"), running (spinner, 2 px accent line, drawn only after 150 ms), done ("12 items updated"), nothing to do ("Already up to date"), no menu yet (disabled, tooltip).
3. **Pin in Settings** (`pin`). Pin cell between "Comes from" and "...": empty, hover = dim pin, pinned = accent pin; pinned rows sit first in their group with the grip dimmed; hidden + pinned keeps both marks dimmed; Tsubame rows have no pin cell; optional "Pinned" chip. No pin marker in the real menu.
4. **User group name in the menu** (`groupname`). A: no header (today). B: 20 px non-interactive label row, 11 px `text_dim`, under the separator. Per group, switch "Name in menu" on the group row.
5. **Nilesoft selection screen** (`nss`). Inline panel after the file dialog / "Review and import…": parsed entries with checkboxes (submenu tri-state, children indented), importable rows with "shows for", skipped rows with reason and line, Select all / none, All / Importable / Skipped chips, "Placed in group" dropdown (user groups, New group… with a name field, End of the menu), Cancel, "Import N selected". Then the existing report.
6. **Several items and Drive** (`kinds`). Several items: only entries whose `show_for` covers 2+ items; count header is not a row; count verbs listed under the plain name with a hint; "needs 2" tag on Compare. Drive: Tsubame rows at once, Windows' and apps' rows after the first drive right-click (empty-state note until then). Add custom entry gets "Several items" and "Drive" boxes (`show_for.multiple`, `show_for.drives`).

Open questions (recommendation first):

1. Same list everywhere with disabled rows, or hide inapplicable rows? **Disabled with reason** (stable muscle memory); alternative: hide.
2. Refresh scope: selected kind or all kinds? **Selected kind** (that is what `LAST_SHOWN` is for); alternative: all kinds at once.
3. Show the running state? **Only if over 150 ms**; the compare is local and usually instant, so most runs go idle -> done.
4. Pin on U4 rows (Tsubame's own)? **No**, they have the grip and Move up / down. Order of several pins? **Native order** among themselves.
5. User group name: **A (no header)** by default, B per group via "Name in menu" (off). Alternative: always B, or never.
6. Import default placement: **New group named "Nilesoft"** the first time, then the last used group (Undo import removes the empty group). Alternative: "Tools" as in the 6b wording, or end of menu.
7. Custom entry on several items: **runs once with all paths if it uses `{selection}` / `%*`, otherwise once per item** (ask above 10). Alternative: only allow entries that use `{selection}`.
8. Del key: Hide in the menu-side edit menu (6a), Delete entry on a custom row in Settings. **Keep**, and give Delete entry no shortcut in the menu.

### Approval

The gap states above were approved by the owner on 2026-11-20 with the recommended answers (1-8) as written.
