# Default file manager (opt-in)

Tsubame can become the program that opens folders for the current Windows user. It is **off by default**,
needs no administrator rights, and is built so that a broken Tsubame can never leave you without a file
manager. The design and the Defender analysis are in `docs\research-default-explorer-defender.md`;
this file is what actually ships.

## What it does and does not do

| Does | Does not |
|---|---|
| Double-clicking a folder, the Run dialog (`C:\x`), `start C:\x` in a terminal, "open folder" in other apps open Tsubame | Replace Explorer for drives, This PC, the Recycle Bin, Control Panel, the taskbar Explorer icon, every app's Open/Save dialog, "show in folder", Win+E |
| One per-user registry verb that starts the fail-safe shim `tsubame-handler.exe` | Touch HKLM, IFEO (`Image File Execution Options`) on `explorer.exe`, `Folder\shell`, `DelegateExecute`, any CLSID, or Windows' `Shell` value |
| Remove puts the previous state back, even when Tsubame is deleted | Re-apply itself after another app changes it (never re-asserts policy) |

Win+E takeover is a separate, experimental opt-in and is **not** part of this feature.

## Exact registry contents

Hive `HKCU`, base `Software\Classes\Directory\shell`. Verb name: **`Tsubame`**.

| Key | Value | Data |
|---|---|---|
| `Directory\shell` | `(Default)` | `Tsubame` (selects our verb as the default verb for folders) |
| `Directory\shell\Tsubame` | `(Default)` | `Open in Tsubame` |
| | `Icon` | `<install dir>\tsubame.exe,0` |
| | `PreviousDefault` | what `Directory\shell` `(Default)` held before (missing when it held nothing) |
| `Directory\shell\Tsubame\command` | `(Default)` | `"<install dir>\tsubame-handler.exe" "%1"` |

Nothing else is written. Checked on the benchmark machine after pressing the switch (`reg query "HKCU\Software\Classes\Directory\shell" /s`):
exactly the four values above plus whatever other apps already had there.

Rules the code follows (`crates\tsubame-handler\src\registry.rs`, same code in the app and in the shim):

- **Idempotent.** Turning it on twice changes nothing; the first `PreviousDefault` is kept.
- **Read back.** Every write is read back; a mismatch is reported as an error, not as success.
- **Default flips last.** The verb and command exist before `Directory\shell` points at them, so an
  interrupted write never leaves a default pointing at nothing.
- **Refuses without the shim.** If `tsubame-handler.exe` is not next to `tsubame.exe`, the switch is
  disabled (tooltip: "tsubame-handler.exe was not found next to tsubame.exe") and `enable` returns an error.
- **`TSUBAME_NO_REGISTRY_WRITE=1`** makes the real registry refuse every write. Unit tests never use the
  real registry (the real writer also refuses under `cfg(test)`).

## States on the Settings page

Settings > Default file manager (Ctrl+,). The registry is read when the page opens and on **Refresh**.

| State | Meaning | Controls |
|---|---|---|
| Not set | `Directory\shell` default is not ours | switch off; Remove (safe, does nothing) |
| Active | our verb is the default and the shim it points to exists | switch on; Remove |
| Needs repair | our verb is the default but the shim path it points to is missing | **Repair** (points it at this install), Remove |
| Not set (another app) | another app is the default now | switch off; turning on replaces it, Remove puts it back |

Tsubame never repairs or re-applies anything by itself, only the buttons write.

## How Remove works

All of these use the same inverse (`disable`):

1. Settings > **Remove** (also the switch turned off).
2. `tsubame-handler.exe --remove` (also `--restore-explorer`). Works with no Tsubame at all.
3. The shim removes the verb by itself when Tsubame is missing or keeps failing (below).
4. The installer's uninstaller runs `tsubame-handler.exe --remove` first, before any file is deleted.
5. Last resort, by hand (cmd or PowerShell, no admin):
   ```
   reg delete "HKCU\Software\Classes\Directory\shell\Tsubame" /f
   reg delete "HKCU\Software\Classes\Directory\shell" /ve /f
   ```
   (the second line only if the default is still `Tsubame`; if a `PreviousDefault` was recorded, set the
   default back to it first). Then press Win+R and `explorer.exe` always works.

`disable` is idempotent: it restores `PreviousDefault` (or deletes the default value when there was none),
deletes `Directory\shell\Tsubame`, notifies the shell (`SHChangeNotify`) and reads back. Nothing needs a
sign-out. Verified on the benchmark machine: with a hand-made registration holding `PreviousDefault=none`, `--remove`
restored `none` and deleted the verb; running it again changed nothing.

`remove_if_ours` (used by the shim) only removes a registration whose command points at **this** shim's
path (or is empty), so one install can never remove another install's registration.

## The shim: `tsubame-handler.exe`

About 183 KB release, windows subsystem, no UI, `asInvoker`, long-path aware. Source:
`crates\tsubame-handler` (lib with all the logic behind a `Host` trait, plus a thin `sys.rs`).

Flow for `"<shim>" "%1"`:

1. Parse the raw command line (first quoted token is the program, the rest is the folder).
2. Not an absolute path (flags, `::{CLSID}` shell paths, empty) → straight to Explorer.
3. Find `tsubame.exe` next to the shim, start it with the folder (standard CRT quoting, trailing
   backslashes handled), pass foreground rights to it, wait up to **4 s** for it to go idle.
4. Success = the app is idle, or it exited 0 (it forwarded the path to a running instance). Return at once;
   the success path writes nothing at all (no log, no file).
5. Anything else falls back to `%WINDIR%\explorer.exe <folder>` (absolute path, never via PATH).

### Failure policy

| Situation | What the shim does | Registration |
|---|---|---|
| `tsubame.exe` missing | retries 4 times, 150 ms apart (an update swap takes a moment), then opens Explorer | removed, unless `tsubame.exe.new` exists (update in progress), then kept |
| Spawn error "not found" | same as missing | same |
| Other spawn error, crash (non-zero exit), hang (not idle after 4 s) | opens Explorer, counts a failure | removed on the **3rd failure within 600 s** |
| Explorer fallback loops back into the shim | env `TSUBAME_HANDLER_DEPTH` (set only on the Explorer it starts, never in the app's environment): depth 1 → remove the registration and open Explorer; depth 2 → do nothing | removed |

Files (in the Tsubame data dir: `TSUBAME_DATA_DIR`, else `<exe dir>\data` when a `tsubame.portable` or
`portable.txt` marker exists, else `%APPDATA%\Tsubame`): `handler.log` (one line per failure or removal, so
"why did Explorer open?" is answerable) and `handler-fails.txt` (timestamps of recent failures).

### Known limitations

- Paths containing unpaired UTF-16 surrogates are decoded lossily.
- Explorer itself opens `Documents` for a folder path longer than 260 characters; the fallback inherits that.
- A hang can only be detected when the app is a GUI process that never goes idle; it is unit-tested with a
  fake host and was checked on the benchmark machine with a stand-in app that never creates a window (Explorer opened after
  about 4.5 s, failure logged).

### Measured on the benchmark machine (debug app, release shim comparable)

| Case | Result |
|---|---|
| Open folder in a new Tsubame through the shim | shim exits in about 290 ms, Tsubame window at the folder |
| Forwarded into a running Tsubame | 30 to 110 ms, new tab |
| Missing app | Explorer at the folder after about 560 ms, registration removed, `handler.log` explains |
| Crash (exit 7) | Explorer after about 125 ms, failure 1 of 3 logged, registration kept, no loop |
| Hang (no window) | Explorer after about 4.5 s, failure logged |
| Paths: drive root `G:\` (quoted and not), spaces + unicode, trailing backslash, UNC share root | correct tab in Tsubame; correct folder in the Explorer fallback |

## Packaging and updates

- `tools\release.ps1` builds `tsubame` and `tsubame-handler`, puts the shim in the zip and the installer,
  signs it with the other exes (the Authenticode loop covers every exe in the package) and lists it with its
  SHA-256 in the update manifest. A real release **fails** if the shim is missing (`-DevKey` rehearsals do not).
- The installer always installs the shim (`installer\tsubame.iss`), never registers anything at install
  time, and runs `tsubame-handler.exe --remove` on uninstall.
- The updater swaps `tsubame-handler.exe` together with `tsubame.exe` and rolls them back together; it
  updates the shim only if it is already installed (`UPDATABLE_FILES`, `docs\updater.md`).

## Manual test matrix (clean Windows 11 VM, Defender on): NOT RUN

Copied from research section 5.5 for the VM task. Nothing below was run for this feature; only the benchmark-machine
checks listed above were. "Tier 1" is this feature; Tier 0 and Tier 2 are not implemented yet.

**A. Baseline / no regression**
- [ ] 1. Fresh install, nothing switched on: double-click, Win+E, Run `C:\`, browser "open folder", "show in
      folder", Open/Save dialog behave like stock. `HKCU\Software\Classes` has no `Tsubame` keys.
- [ ] 2. Portable zip, nothing on: same, and nothing written outside the data dir (HKCU diff before/after).
- [ ] 3. Uninstall with nothing on: no leftover keys, files, `Run` key or scheduled task.

**B. Each option alone**
- [ ] 4. Tier 1 on: double-click folder, Run dialog, `start C:\x`, Chrome "open folder" open Tsubame at that
      path; "show in folder" and the taskbar icon still open Explorer (documented, not a bug).
- [ ] 5. (Tier 2, Win+E: not part of this feature.)
- [ ] 6. (Tier 0, context-menu entries: not part of this feature.)
- [ ] 7. Tier 1 on, start Tsubame twice from two folders quickly: the second goes to the existing instance
      (no duplicate window, no lost path).

**C. Reversal**
- [ ] 8. Tier 1 on, Remove: registry byte-identical to the pre-change export; folders open in Explorer at
      once, no sign-out.
- [ ] 9. Pre-existing third-party default (set `Directory\shell` `(Default)` to a dummy verb first): on then
      off restores the dummy verb, not "nothing".
- [ ] 10. Tier 1 on, uninstall with the app running: asks to close, restores Explorer, then deletes files;
      verify the key and open a folder.

**D. Hostile / failure cases**
- [ ] 11. Tier 1 on, kill and delete the whole install folder, open a folder: one Explorer window, the
      `Directory\shell` keys gone, `handler.log` (data dir) explains it. (If the data dir lived in the
      deleted portable folder there is no log; fine.)
- [ ] 12. Tier 1 on, rename `tsubame.exe`: same as 11.
- [ ] 13. Tier 1 on, make `tsubame.exe` hang at startup: Explorer opens the folder after the shim timeout;
      the registration is removed on the third failure within 10 minutes (not the first).
- [ ] 14. Tier 1 on, set `Directory\shell` `(Default)` to something else by hand: Settings shows "another
      app", does not fight it, and the switch shows off after Refresh.
- [ ] 15. Tier 1 on, a second file manager takes over: both apps and Explorer still work; uninstalling either
      does not break folders.

**E. Defender**
- [ ] 16. Win 11 24H2/25H2, Defender fully updated (real-time, cloud, PUA, tamper protection), Smart App
      Control on in a separate VM: unsigned RC (record the block), then signed RC (install, switch Tier 1 on);
      read Protection history after each write for `Behavior:Win32/...`. Repeat with ASR rules
      `01443614-...` and `c1db55ab-...` in block mode.
- [ ] 17. Same VM: updater end to end; the new exe is not written to `%TEMP%`, the old exe is renamed not
      overwritten, Protection history stays clean (include `tsubame-handler.exe`).
- [ ] 18. Submit the RC (installer + exes including the shim) to the Microsoft file-submission portal as
      Software Developer; record verdict and turnaround.
- [ ] 19. Run the portable build from a USB stick (ASR `b2b3f03d-...`) and note the result in the README.

**F. Regression**
- [ ] 20. After each Windows cumulative update, repeat 4, 9 and 11 (and 5 once Tier 2 exists).
