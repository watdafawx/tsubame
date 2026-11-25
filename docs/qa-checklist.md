# QA checklist before a tag (rig)

A numbered, scriptable matrix for an agent to run on the rig build host before every release tag.
It covers the accepted release-gate rows in `docs/PLAN.md` §7 P6 and the areas the B9 rig QA of
Batch 8 exercised: preview pane, Quick Look, global search, archive VFS, git/folder-size/checksum
columns, mouse and navigation, the own-drawn context menu, idle CPU, and the release exe against its
size budgets. It does not repeat the benches (`docs/bench.md`, `docs/bench-log.md`), the leak gate,
or the installer/registry checks (`docs/release.md`, `docs/release-checklist.md`); the two manual
checks that cannot be automated are listed in `docs/release.md` ("Manual checks before a tag").

## Rules

1. **GUI only on the rig build host, through the repo's push-to-rig tooling.** Never launch the app,
   or use mouse/keyboard/screenshot automation, on the development desktop. Input and screenshots
   come from the rig automation (click, type, screenshot). The rig has one 4K display at 200% scale;
   record every px/DIP observation there.
2. **Test the tagged commit's release build.** Build the release profile on the rig, note the commit
   SHA, `tsubame --version`, the exe size and its SHA-256 in the run record (section 4).
3. **One scratch root for all fixtures**, `%TEMP%\tsubame-qa` on the rig (section 1). Nothing in this run
   changes tracked files except a bug fix.
4. **Every row gets exactly one verdict: PASS, FAIL or UNTESTABLE.** A FAIL blocks the tag unless the
   maintainer records an explicit waiver in `docs/PLAN.md` §9 for this run. UNTESTABLE needs a reason
   and who covers it (usually the manual list in `docs/release.md`).
5. **Evidence**: screenshots and recordings stay in the rig scratch dir, named `<ID>-<n>.png`
   (for example `PRE-4-2.png`); attach one only when a row FAILs or the result is surprising.
6. **Record the environment** once per run: rig OS build, time zone, installed shell extensions
   (they change the context menu), and whether Everything is running (it changes the search backend).
7. **Clean up** when the run ends: stop the app and free its rig slot, delete the fixture tree, and
   check the process list for leftover workers (no `tsubame-*` thread or process survives the app).

## 1. Fixtures

Run on the rig once per run, before any row; the script is idempotent because it recreates the root.
All expectations below use the fixed timestamp `2026-03-04 15:16:00`.

```powershell
$root = Join-Path $env:TEMP 'tsubame-qa'
Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
'basic','preview','media','archives','archives\src','archives\src\sub','gitrepo',
'sizes','sizes\sized','sizes\empty','checksum','ctx','ctx\folder' |
  ForEach-Object { New-Item -ItemType Directory -Force -Path (Join-Path $root $_) | Out-Null }

# text and code
Set-Content (Join-Path $root 'basic\alpha.txt') 'alpha'
1..2000 | ForEach-Object { "line $_ " + ('x' * 100) } |
  Set-Content (Join-Path $root 'preview\long.txt')
@'
// qa sample for syntax tinting
fn main() { let name = "tsubame"; /* comment */ println!("{name}"); }
'@ | Set-Content (Join-Path $root 'preview\sample.rs')

# image (any picture already on Windows; the extension is kept)
$img = Get-ChildItem "$env:WINDIR\Web" -Recurse -Include *.jpg,*.png -ErrorAction SilentlyContinue |
  Select-Object -First 1
if (-not $img) { throw 'No sample image under %WINDIR%\Web; put one in preview\' }
Copy-Item $img.FullName (Join-Path $root ('preview\picture' + $img.Extension))

# binary, PE, unknown type (NUL bytes route it to Unsupported)
$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$bytes = New-Object byte[] (100 * 1024); $rng.GetBytes($bytes)
[IO.File]::WriteAllBytes((Join-Path $root 'preview\random.bin'), $bytes)
Copy-Item "$env:WINDIR\System32\notepad.exe" (Join-Path $root 'preview\sample.exe')
$unk = New-Object byte[] 512; $rng.GetBytes($unk)
[IO.File]::WriteAllBytes((Join-Path $root 'preview\mystery.unk'), $unk)

# media: audio ships with Windows; a video sample is optional
$wav = Get-ChildItem "$env:WINDIR\Media" -Filter *.wav -ErrorAction SilentlyContinue |
  Select-Object -First 1
if ($wav) { Copy-Item $wav.FullName (Join-Path $root 'media\tone.wav') }

# archives, all entries stamped 2026-03-04 15:16:00 local
$src = Join-Path $root 'archives\src'
Set-Content (Join-Path $src 'a.txt') 'a'
Set-Content (Join-Path $src 'sub\b.txt') 'b'
Get-ChildItem $src -Recurse -File | ForEach-Object { $_.LastWriteTime = Get-Date '2026-03-04 15:16:00' }
Compress-Archive -Path (Join-Path $src '*') -DestinationPath (Join-Path $root 'archives\bundle.zip') -Force
tar -czf (Join-Path $root 'archives\bundle.tar.gz') -C $src a.txt sub
tar -cf  (Join-Path $root 'archives\bundle.tar')    -C $src a.txt sub
$sevenZip = Get-Command 7z.exe -ErrorAction SilentlyContinue
if ($sevenZip) {
  & $sevenZip.Source a (Join-Path $root 'archives\bundle.7z') (Join-Path $src '*') | Out-Null
} else {
  'No 7z.exe on this host: mark ARC-3 UNTESTABLE, or place a small .7z in archives\'
}

# git: one clean, one modified, one untracked, one ignored file
Push-Location (Join-Path $root 'gitrepo')
git init -q
Set-Content a.txt 'clean'; Set-Content b.txt 'before'; Set-Content c.txt 'new'
Set-Content .gitignore 'ignored.txt'; Set-Content ignored.txt 'ignored'
git add a.txt .gitignore
git -c user.name=QA -c user.email=qa@example.invalid commit -qm qa
Set-Content b.txt 'after'
Pop-Location

# folder sizes: 3 MiB in sized\, nothing in empty\
$mb = New-Object byte[] (1MB)
1..3 | ForEach-Object { [IO.File]::WriteAllBytes((Join-Path $root "sizes\sized\file$_.bin"), $mb) }

# checksum: fixed payload; the printed hash is the oracle for COL-3
$payload = [Text.Encoding]::UTF8.GetBytes('tsubame qa payload 0123456789' * 32)
[IO.File]::WriteAllBytes((Join-Path $root 'checksum\payload.bin'), $payload)
'expected sha256: ' + (Get-FileHash (Join-Path $root 'checksum\payload.bin') -Algorithm SHA256).Hash

# context menu rows
Set-Content (Join-Path $root 'ctx\row.txt') 'row'
Copy-Item (Join-Path $root 'basic\alpha.txt') (Join-Path $root 'ctx\folder\child.txt')

'--- fixture manifest ---------------------------------------------'
Get-ChildItem $root -Recurse -File |
  Select-Object @{n='Path';e={$_.FullName.Substring($root.Length)}}, Length, LastWriteTime |
  Format-Table -AutoSize
'time zone: ' + (Get-TimeZone).Id
```

Save the manifest and time zone in the run record; the archive time rows compare against them.

## 2. Matrix

Verdict cells are left empty; fill each with PASS, FAIL or UNTESTABLE. Chords are the defaults from
`src/commands.rs`; palette = `Ctrl+Shift+P` (or `Ctrl+K`).

### 2.1 Preview pane (docked, `Alt+P`; extension `tsubame.ext.preview`)

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| PRE-1 | Open `basic`; press `Alt+P`; press it again. Repeat from Settings > General if the pane switch is there. | Pane docks on the right edge only; list reflows without a repaint flash or lost selection. Width equals `preview.width` × display scale (±1 px): 320 DIP = 640 px at 200%. A repeat of the 2026-11-24 finding (564 px) is a FAIL (cite `PLAN.md` §9). Second press hides it and the list takes the width back; the value survives a restart. | |
| PRE-2 | Select `picture.*`; press `Alt+P` if the pane is closed. | Fitted image on a subtle checkerboard, `Fit`/`100%` toggle changes the zoom and the percentage badge, metadata strip shows dimensions, format, file size and modified time. No spinner once loaded. | |
| PRE-3 | Select `long.txt`, then `sample.rs`. | Monospace viewer with line-number gutter; header shows language/encoding/line count and a `Wrap` toggle; truncation banner reads "Showing first 64 KB of <full size>" with an open-full-file action. `sample.rs` shows keyword/comment/string tinting; no tint in a plain `.txt`. | |
| PRE-4 | Select `sample.exe`, then `random.bin`. | `.exe` opens as a hex dump directly (`preview.direct_hex_binaries`); 16 bytes/row with offset and ASCII columns, `00` dimmed, truncation banner at 16 KB "of <full size>". Narrow the pane below 420 px: rows switch to 8 bytes (`preview.hex_auto_width`) and the manual `16 B`/`8 B` toggle still works. | |
| PRE-5 | Select `tone.wav`; if a video file exists on the rig, select it too. | Audio: waveform/poster area plus a metadata table (duration, sample rate, channels, format, size) and an `Open` button; no embedded player. Video: resolution, codec, frame rate, duration, poster frame. No sample video → UNTESTABLE with that reason. | |
| PRE-6 | Select 3 files with `Ctrl+click`; then an empty folder; then `mystery.unk`. | Multi: "3 items selected — <total> total" with item summaries and the `Space` hint. Empty: "No item selected" with the `Alt+P` hint. Unknown/binary: "No preview available" card with type + size, `Open with default app` and `View as Hex`; the button switches to the hex view. | |
| PRE-7 | Hold `Down` over 30 rows in `preview`, release, wait 1 s. | Preview matches the final cursor row exactly; no earlier file's content appears afterwards (stale generations dropped); the loading skeleton appears only when a preview takes >80 ms and never flickers on NVMe reads. | |
| PRE-8 | Drag the splitter 5 px left/right; drag far left, then far right; double-click it. | Hit strip is thin but grabbable; a px chip follows the drag; clamped at 220 and 600 DIP; double-click returns to 320 DIP; `preview.width` in the config matches the last value after exit. | |
| PRE-9 | `Ctrl+\` to split; give each pane a different folder and file selection; click between panes. | One shared pane on the right edge of the window follows the active pane's cursor/selection; switching panes updates it; closing a tab does not leave a preview of a dead path. | |
| PRE-10 | Add `"tsubame.ext.preview"` to `[extensions] disabled` in the config, restart. | `Alt+P` and Quick Look do nothing, no pane and no error message; re-enabling it (config or the Settings switch, live if offered) brings both back without a restart. | |

### 2.2 Quick Look (`Space`, `F3`)

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| QL-1 | With list focus and a row selected, press `Space`. | Centered overlay over a dimmed list: icon, filename, index `n / total`, prev/next chevrons, `Open`, `Close (Space / Esc)`, one-line metadata/shortcut footer; the window and the list do not resize. | |
| QL-2 | Press `Space` to close, reopen, press `Esc`. | Both close it; the list keeps keyboard focus (press `Down` immediately: selection moves one row, no click needed). | |
| QL-3 | Reopen; press `←` and `→` several times, then `↑`/`↓`. | Filename, index and preview all follow the new row; at the first/last row it stops or wraps consistently and never shows an empty modal or a preview for the previous file. | |
| QL-4 | Focus the filter box, path-bar edit mode and inline rename (`F2`); press `Space` in each. | A space character is inserted into the field; Quick Look never opens from a text field. | |
| QL-5 | Press `Space` on `mystery.unk`; press `Space` in an empty folder. | Unsupported card with `View as Hex`, not a dead modal. Empty folder: nothing opens (or the "No item selected" hint), never a blank overlay without a close hint. | |
| QL-6 | Press `F3`; press `Space`; reopen and click `Open`. | `F3` opens the same overlay; `Space` still closes; `Open` closes the overlay before the default handler starts and Tsubame stays open. | |

### 2.3 Global search (`Ctrl+Shift+F`; extension `tsubame.ext.global_search`)

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| SRCH-1 | In `basic`, press `Ctrl+Shift+F`, type `alpha`. | Overlay opens over the list with the scope chip on "Current folder"; status shows "Searching…" then "N results"; `alpha.txt` is listed; the app stays responsive (list and window repaint while searching). | |
| SRCH-2 | With a hit selected, press `Enter`; reopen and search again, press `Ctrl+Enter`; press `Esc`. | `Enter` goes to the item (its folder open, item selected). `Ctrl+Enter` opens the file in the default app. `Esc` closes the overlay and the list keeps focus (`Down` moves the selection at once) — regression target `15ac858`. | |
| SRCH-3 | Press `Tab` on the scope chip; search a name that exists on another drive. | Chip toggles "Current folder" ↔ "All drives (Everything)"; status names the backend that ran. With Everything running the backend is "Everything"; without it the walk backend returns results (record the elapsed time and which backend ran). No crash when Everything is absent. | |
| SRCH-4 | Type 8 characters fast, then `Backspace` to a different query, then `Esc` mid-search. | Only the last query's hits are shown, no stale flash of older results; `Esc` during a search closes cleanly, no hang, and no search worker survives (see IDLE-3). | |
| SRCH-5 | Search `zzz-no-such-file-qa-<random>`. | "0 results", no error banner, no crash; `Esc` closes. | |
| SRCH-6 | Add `"tsubame.ext.global_search"` to `[extensions] disabled`, restart. | `Ctrl+Shift+F` does nothing and no overlay appears; re-enable (live if the Settings switch exists) and search works again. | |

### 2.4 Archive VFS (`.zip`, `.tar`, `.tar.gz`, `.tgz`, `.7z`; extension `tsubame.vfs.archive`)

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| ARC-1 | In `archives`, open `bundle.zip` (Enter or double-click). | Opens as a read-only virtual folder; breadcrumb ends with `bundle.zip::`; rows are `a.txt` and `sub` (compare with `tar -tf` or Explorer); `sub` opens and `Up`/breadcrumb step back through the virtual tree and out to the host folder. No extraction on browse. | |
| ARC-2 | Repeat ARC-1 for `bundle.tar.gz` and `bundle.tar`. | Same rows and navigation; `.tgz` is accepted as well (rename a copy if needed). Host folder unchanged. | |
| ARC-3 | Repeat ARC-1 for `bundle.7z`. | Same rows; folder entries included. No 7z fixture and no way to make one → UNTESTABLE, reason recorded. | |
| ARC-4 | Select `a.txt` inside each archive; compare its date/time with Explorer/`Get-Item` on `archives\src\a.txt`. Record the rig time zone. | Every format shows `2026-03-04 15:16:00` local (same as the source file) — not +2 h and not UTC. Regression target `67eb12a`; a mismatch on one format only is still a FAIL and names that format. | |
| ARC-5 | Open `bundle.zip`; palette → `archive.extract_here` (no default chord). | Status bar shows "Extracting bundle.zip…" and clears when done; `archives\` gains `a.txt` and `sub\b.txt` with preserved mtimes; `Get-FileHash` of the extracted `a.txt` equals the source; selected archive in a normal folder uses its host folder as the destination. | |
| ARC-6 | Inside `bundle.zip`, select only `a.txt` and `sub` (or `sub\b.txt`); palette → `archive.extract_selected`. | Only the selection lands in `archives\`, structure preserved; with no selection the cursor entry is extracted. Status text shown, no partial tree left on a failure. | |
| ARC-7 | Inside an archive press `Del`, then `F2`, then try to rename a row. | Nothing is deleted or renamed, no crash. Known accepted gap (2026-11-24): the block is silent; PASS if data and UI are untouched, note the missing status message. | |
| ARC-8 | Run `archive.extract_here` again over the files just extracted. | Current accepted behaviour is a silent overwrite: content matches the archive, no crash, no duplicate `a (1).txt`. The skip/replace/rename prompt is an accepted P4.F3 follow-up, not a regression. | |
| ARC-9 | Double-click `a.txt` inside the archive. | No crash, no hang, no second Tsubame window; record whether it opens (known gap) — the row passes on "no crash/hang" alone. | |
| ARC-10 | With the preview pane open, click inner files, including `sub\b.txt`; then search with `Ctrl+Shift+F` while inside the archive. | Preview shows the inner file's content (or Unsupported), never the host file with the same name, and never hangs; search behaves sanely from inside an archive (no crash); closing/unmounting the archive drops its rows and preview. | |

### 2.5 Columns and background metadata (git, folder sizes, checksum)

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| COL-1 | Open `gitrepo` with the git column visible. | `b.txt` shows `M` in the folder colour, `c.txt` shows `?` (dim), `ignored.txt` shows `!` (disabled colour), `a.txt` shows nothing (clean). Commit `b.txt` in a shell and let the watcher fire: `M` clears without a manual refresh. A folder outside any repo shows no badges and no errors. | |
| COL-2 | Open `sizes`; palette → `folder.calc_sizes`. | `sized\` shows a pending/blank size, then `3.0 MB`; `empty\` shows `0 B`/blank consistently; files keep their own sizes. Totals equal `Measure-Object Length -Sum` over `Get-ChildItem -Recurse -File`. Navigating away and back shows the cached size at once; adding a file and re-running recalculates. Disabling `tsubame.ext.folder_sizes` removes the command's effect and the column values. | |
| COL-3 | Select `checksum\payload.bin`; palette → `file.checksum`; paste the clipboard into Notepad; compare with the fixture's printed SHA-256. | Status bar summarises CRC32, MD5, SHA-1 and SHA-256 and the SHA-256 is on the clipboard; it matches the fixture hash. Folder selected: "Select a file (not a folder) to compute checksums". File inside an archive: "Extract the file from the archive first to compute checksums". A second run while one is busy neither crashes nor interleaves output. | |
| COL-4 | Switch each P4 column off and on live (`tsubame.column.git`, the size column used by folder sizes, plus the P2 columns); sort by Name, Size and Date. | Layout reflows live, freed width goes to Name, sorting still works per column, and disabling a feature removes its badges/values at once with no dropped thread (IDLE-3). | |

### 2.5b Miller columns (`Ctrl+Shift+5`; extension `tsubame.ext.miller`)

| ID | Do | Expect | Result |
|---|---|---|---|
| MIL-1 | In a deep folder press `Ctrl+Shift+5`. | One column per folder level, each with a header (name and count); ancestors show the path row with a quiet fill and a 2 px accent bar; the active column has the full selection and the focus ring. | |
| MIL-2 | Right, Left, Enter, Backspace, Home, End, then type a name prefix. | Right and Enter enter a folder (Enter opens a file); Left and Backspace go to the parent; type-ahead jumps in the active column; Right on a file does nothing. | |
| MIL-3 | Drill 10 or more levels. | A left cue says how many columns are hidden; at most 12 columns stay live. | |
| MIL-4 | Drag a column edge; double-click it. | Width follows the mouse (140 to 480 px); double-click returns to 220. | |
| MIL-5 | Select a text file, a binary file and a folder. | File: info column with type, size, modified, attributes and an excerpt (text or hex); with Preview off a note and a Settings button; folder: its child column. | |
| MIL-6 | Shrink the window below 560 px. | One column with a parent header and an info strip; Left goes up. | |
| MIL-7 | Open a very large folder, an empty folder and an unreadable one. | Skeleton after 80 ms and Esc cancels; "This folder is empty"; "Can't open this folder" with Retry. | |
| MIL-8 | Select a junction, press Follow; try one that loops to an ancestor. | A "Junction or link" card first; Follow opens the target; a loop says so and is not followed. | |
| MIL-9 | Settings > Extensions: switch Miller off, then on. | Off: the chord does nothing, a Miller tab returns to Details; on: it works again. | |
| MIL-10 | Idle with Miller open for 20 s. | CPU delta about 0. | |

### 2.6 Mouse and navigation

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| MSE-1 | Navigate 4 folders deep by double-click, go up 2 levels, then press `Alt+Left` 3 times and `Alt+Right` twice. Then the real mouse Back/Forward buttons. | Chord/toolbar history goes back and forward exactly one step per press, list and breadcrumb follow, no tab switch. **Mouse side buttons are UNTESTABLE by synthetic input** (2026-11-24: 1 of 7 tries passed) — the real-mouse check is in `docs/release.md` "Manual checks before a tag". | |
| MSE-2 | In details view and in grid view (`Ctrl+Shift+2`), double-click empty space under the last row. | Goes up one level in both views; double-click on a row still opens it; a single click on empty space clears the selection. | |
| MSE-3 | Drag from empty space across rows; `Ctrl+drag`; `Shift+drag`; press `Esc` mid-drag. Then drag a row to the desktop. | A selection band appears, rows select, no OLE drag ghost and no ghost file; `Ctrl` toggles against the starting selection, `Shift` extends; `Esc` cancels. Dragging a row still starts a shell drag-out: drop it on the desktop and confirm it arrives (copy or move; fixtures are disposable). | |
| MSE-4 | Middle-click a folder row; middle-click a breadcrumb segment. | Folder opens in a new tab (one tab more, other tabs unchanged); the breadcrumb segment opens in a new tab; no duplicate tab storm. | |
| MSE-5 | Inside `bundle.zip`, click the host folder crumb once to exit. Repeat 5 times over 2 archives. | One click exits every time (2026-11-24 saw one swallowed click; a repeat is a FAIL and goes to `PLAN.md` §9). | |
| MSE-6 | Click a row once, then press `Space`, `Esc`, `F2`, `Esc`, `Down`. | Keyboard focus is on the list after the click and stays there: Quick Look opens/closes, inline rename opens and cancels, `Down` moves the selection. Also check the same after the palette closes (`Esc`) — regression target `15ac858`. | |

### 2.7 Context menu (`docs/ctxmenu.md`)

Setup: let the app warm up (open two or three menus) before timing. Record which shell extensions are
installed on the rig; they change the item set but not the open behaviour.

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| CTX-1 | Right-click a row 10 times on different rows, screenshot immediately each time. | Own-drawn menu appears with its top-level items in the same gesture as the click. A warm shell answers inside the 60 ms reveal grace, so the complete menu paints in one frame: no "Loading…" row at top level, no empty menu that fills later, no placeholder flash. A second right-click replaces the open menu (never two menus, never needs a second click). Reference for a warm menu (validation harness, 2026-11-24): p50/p95 35.8 / 41.7 ms from click to top-level items; a menu that visibly lags the click is a FAIL. | |
| CTX-2 | Right-click empty space in `basic`. | Background menu: New folder, Paste (disabled while the clipboard is empty), Refresh, Properties, plus the Tsubame rows; the item set is the folder's, not a file's. | |
| CTX-3 | Click the row's `...` button; with a menu open, right-click another row; `Esc`; click outside; select a row and press `Shift+F10` / the Apps key. | The `...` menu has the same contents as right-click; right-clicking another row replaces it; `Esc` and an outside click close it; the keyboard path opens the same menu with the first row focused. | |
| CTX-4 | Hover `Open with`, then a packaged (Win11) submenu; repeat 20 times. Then relaunch with `TSUBAME_CTX_SUB_DELAY_MS=4500` and hover a lazy submenu. | Submenu opens after ~150 ms hover and closes ~250 ms after the pointer leaves; a slow submenu may show one "Loading…" row (p95 ≤ 112 ms) but the top level never does. With the 4.5 s delay the row becomes the failure row with Retry after ~3 s, the 4 s watchdog fires, and the menu and app stay responsive; no stuck menu after 20 opens. | |
| CTX-5 | Right-click a sidebar drive; right-click a sidebar pin (bookmark); right-click a network share root if one is available. | Drive/share roots use the shell-menu path and open without hanging; a sidebar pin keeps its app-only popup; cut/rename/delete on a drive or share root is refused (no crash, no error dialog); an offline share does not hang the app. | |
| CTX-6 | Invoke Open, Copy, Cut, Paste, Rename, Delete, Copy path and Properties in turn. | Each acts once and the menu closes; Delete is red; Paste disabled with an empty clipboard; no stale menu, no modal ghost, no duplicate window; the list selection and focus survive. | |
| CTX-7 | Open a menu, leave it static for 30 s, then use only the keyboard (`↑`/`↓`, `→`/`←` submenus, `Enter`, `Esc`). | No repaint or CPU while static (see IDLE-4); keyboard focus is visible, submenus open/close with the arrows, `Enter` invokes, `Esc` closes and returns focus to the list. | |

### 2.7b Screen reader rows (`docs/uia-views.md`)

Setup: read the tree with a UIA client (PowerShell `System.Windows.Automation`, Accessibility Insights,
or Narrator). Use `basic` (tags on a few rows, file-age colouring on) and a deep folder for Tree and Miller.

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| UIA-1 | With a client attached, in Details read the window's List and its first rows; repeat in Grid (`Ctrl+Shift+2`). | A `List` named for the folder; `ListItem`s named with the file name (`, tags: …` when tagged); Value is type, size, modified (and the age bucket when file-age is on); `ItemCount` is the full folder count, children are only the visible rows. | |
| UIA-2 | Select rows with `Ctrl+click`; use `SelectionItemPattern.Select/AddToSelection/RemoveFromSelection` from the client; `InvokePattern.Invoke` on a folder row and a file row. | `IsSelected` follows the real selection; Select/Add/Remove change it; Invoke on a folder enters it, on a file opens it. | |
| UIA-3 | `Ctrl+Shift+4` (Tree rows), then `Shift+Alt+T` (folders pane). Read the Tree and `TreeItem`s. | Rows are `TreeItem` with `Level` (depth + 1), `PositionInSet`, `SizeOfSet`; folders report Collapsed/Expanded, files LeafNode; `ExpandCollapsePattern.Expand/Collapse` opens and closes the folder; the pane's items do the same and Invoke goes to the folder. | |
| UIA-4 | `Ctrl+Shift+5` in a folder with sub-folders. Read each column. | One `List` per column named by its folder; the selected row of each ancestor column is `IsSelected`; Select on a row in an earlier column moves the path; Invoke enters a folder. | |
| UIA-5 | Open the tags picker (`Alt+T`); invoke the tag rows, `New tag`, then in the New tag form a swatch and `Create`/`Cancel` from the client. | Each row acts as a click would (toggle, form opens, swatch selected, tag created or form closed). | |
| UIA-6 | Scroll fast with a client attached, then detach it (close the client). Check CPU and the tree. | No stale rows (a gone row answers element-not-available); with no client attached the next frames build nothing. | |
| UIA-7 | Idle 20 s with a client attached and Details open; then with none. | CPU delta about 0 in both; with no client no `ViewSnap` is built (see `docs/uia-views.md`). | |

### 2.8 Idle CPU (10 s window)

Runs on the rig in the app's desktop session (a plain remote shell cannot open threads). Budget:
0 wake-ups (`PLAN.md` §2); measured with the tracked counter.

```powershell
pwsh -NoProfile -File tools\idle-wakes.ps1 -ProcessId <tsubame pid> -Settle 5 -Seconds 10 -Windows 1
```

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| IDLE-1 | Open `basic`, do not touch the app for the settle window, then measure 10 s. | CPU delta ≤ 10 ms over the 10 s window (the Batch 8 run read 0.000 s) and no Tsubame thread wakes; record per-thread wake counts. Budget is zero wake-ups: any Tsubame-owned wake is a FAIL, OS/driver threads are noted and excluded. | |
| IDLE-2 | Open the preview pane on `picture.*`, idle, measure 10 s. | Same as IDLE-1: static image, no animation ticking, no skeleton timer left running, preview worker asleep. | |
| IDLE-3 | Run the whole matrix (searching, extracting, menus, Quick Look), close every overlay, return to `basic`, measure 10 s. | Delta ≈ 0 again; thread count back to the IDLE-1 baseline (no orphaned search, archive-extract or checksum threads); memory not part of this row (leak gate is `tsubame-bench`). | |
| IDLE-4 | Open a context menu and leave it static 10 s; close it; open the search overlay with an empty query for 10 s. | No CPU beyond the initial paint; no polling timer; closing either returns to ≈ 0. | |

### 2.9 Release exe size and identity

```powershell
$exe = '<rig run dir>\tsubame.exe'
Get-Item $exe, (Join-Path (Split-Path $exe) 'tsubame-handler.exe'),
  (Join-Path (Split-Path $exe) 'tsubame-plughost.exe') -ErrorAction SilentlyContinue |
  Select-Object Name, Length, @{n='MiB';e={[math]::Round($_.Length/1MB,2)}}
& $exe --version
(Get-Item $exe).VersionInfo | Format-List FileVersion, ProductVersion
Get-FileHash $exe -Algorithm SHA256
```

| ID | Steps | Expected | Verdict |
|---|---|---|---|
| SIZE-1 | Measure the release `tsubame.exe` as above. | `Length` < 6,291,456 bytes (the `< 6 MB` budget in `PLAN.md` §2; Batch 8 was 3.34 MiB). Record the exact bytes and MiB. | |
| SIZE-2 | Measure `tsubame-handler.exe`. | Present in the release dir and ≤ 300 KB (307,200 bytes; `docs/release.md`). | |
| SIZE-3 | Measure `tsubame-plughost.exe`. | Present and ≤ 12 MB (12,582,912 bytes, the proposed P5 budget). The 2026-11-24 decision ships releases without it until the SDK lands, so "not shipped in this release" is UNTESTABLE with that reason, not a FAIL; record the size whenever it is built. | |
| SIZE-4 | `tsubame --version`, the exe's version resource, the tag, and the Explorer/taskbar icon. | `--version` prints the workspace version equal to the tag without `v`; FileVersion/ProductVersion match it in the 4-part form; the swallow icon shows in Explorer and on the taskbar. | |

## 3. Cleanup

1. Stop the app through the rig tooling and free its slot; confirm no `tsubame*` process is left.
2. Check the process list for orphaned workers or helper processes before the slot is handed back.
3. Delete `%TEMP%\tsubame-qa` on the rig; keep the run's screenshots only until the report is written.
4. Put the header, the verdict table and any FAIL detail into the release PR description or the
   `PLAN.md` §9 findings log (2026-11-24's entry is the shape to copy).

## 4. Run record and sign-off

| Field | Value |
|---|---|
| Version / tag | |
| Commit SHA | |
| Build profile and exe SHA-256 | |
| Rig OS build | |
| Time zone | |
| Installed shell extensions / Everything running | |
| Date and agent | |

| Area | Rows | PASS | FAIL | UNTESTABLE |
|---|---|---|---|---|
| Preview pane (PRE) | 10 | | | |
| Quick Look (QL) | 6 | | | |
| Global search (SRCH) | 6 | | | |
| Archive VFS (ARC) | 10 | | | |
| Columns and metadata (COL) | 4 | | | |
| Mouse and navigation (MSE) | 6 | | | |
| Context menu (CTX) | 7 | | | |
| Idle CPU (IDLE) | 4 | | | |
| Exe size and identity (SIZE) | 4 | | | |

**Verdict:** tag when every row is PASS or an UNTESTABLE/FAIL is waived in `PLAN.md` §9 with a
reason. Tag only after the two manual checks in `docs/release.md` ("Manual checks before a tag") are
also done.

## Not in this matrix

- Budgets, benches and the leak gate: `docs/bench.md`, `docs/bench-log.md`, `tsubame-bench`.
- Installer, registry, shortcuts, uninstall and updater N→N+1: `docs/release.md`,
  `docs/release-checklist.md` step 7.
- Defender and SmartScreen on a clean VM, and the real-mouse Back/Forward buttons:
  `docs/release.md` "Manual checks before a tag".
- Signed-release paths (Authenticode, pinned signers) and the packaged plugin host: covered when
  those code paths ship.
