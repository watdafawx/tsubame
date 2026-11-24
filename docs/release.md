# Cutting a release

Everything a release needs is in the repo: `tools\release.ps1` (build, package, sign), the
`release` GitHub workflow (runs it on a tag and publishes), `installer\tsubame.iss` (Inno Setup).
Design of the updater itself: `docs\updater.md`.

## What a release contains

Files attached to the GitHub Release `vX.Y.Z`:

| File | What | Used by |
|---|---|---|
| `tsubame-X.Y.Z-win-x64.zip` | portable build: `tsubame.exe`, `tsubame-handler.exe` (the fail-safe folder shim), optional `tsubame-plughost.exe`, `portable.txt`, `LICENSE`, `README.txt` | people, and the in-app updater |
| `tsubame-X.Y.Z-x64-setup.exe` | Inno Setup installer (per-user, optional all-users); installs the shim, runs `tsubame-handler.exe --remove` on uninstall | people |
| `tsubame-update.json` | manifest: version, assets, sizes, SHA-256, per-exe SHA-256 (each exe in the zip, shim included), notes URL, `min_supported_version` | updater |
| `tsubame-update.json.sig` | ed25519 signature of the manifest | updater |
| `SHA256SUMS` | SHA-256 of all of the above (for humans, `winget`, `scoop`) | people |

### The folder shim (`tsubame-handler.exe`)

`release.ps1` builds it with the app (`cargo build --release -p tsubame -p tsubame-handler`, about 183 KB,
budget 300 KB) and treats it like any other exe: it goes into the zip, into the installer, through the
Authenticode signing loop (every exe in the package list is signed, so sign the shim or sign nothing) and
into the manifest's per-exe SHA-256 list. A real release **fails** when the shim is missing; `-DevKey`
rehearsals only warn. The updater swaps it together with `tsubame.exe` (`UPDATABLE_FILES`). It is
documented in `docs\default-file-manager.md`. Defender note: submit the shim together with the other exes to
the Microsoft file-submission portal (checklist item 18 in that doc).

## One-time setup (you, manually)

1. **Create the GitHub repo** `watdafawx/tsubame` (public: the updater reads Releases anonymously).
   The name lives in one place: `DEFAULT_REPO` in `crates\tsubame-update\src\lib.rs`
   (and the URLs in `installer\tsubame.iss`).
2. **Make the update-signing key** (once, ever):
   ```powershell
   pwsh tools\release-keygen.ps1          # writes %USERPROFILE%\.tsubame\update-signing.key, prints the PUBLIC key
   ```
   - The private key is 64 hex characters in that file. The script refuses to write it inside the
     repo, refuses to overwrite, and never prints it. `.gitignore` also blocks `*.key`.
   - **Back it up** (password manager or encrypted drive). If it is lost, installed copies can never
     verify an update again and users must reinstall by hand. Keep two copies.
   - Paste the printed **public** key into `RELEASE_PUBLIC_KEYS_HEX` in
     `crates\tsubame-update\src\keys.rs`, commit it, and ship a build with it. Until a public key is
     pinned, a production build refuses every update (fails closed).
3. **Add the secret** `UPDATE_SIGNING_KEY` (GitHub repo > Settings > Secrets and variables > Actions),
   value = the contents of the key file:
   ```powershell
   gh secret set UPDATE_SIGNING_KEY < "$env:USERPROFILE\.tsubame\update-signing.key"
   ```
4. **Optional, later: a code-signing certificate** (removes most SmartScreen friction; see
   `docs\research-default-explorer-defender.md`). When it exists:
   - add secrets `SIGN_CERT_THUMBPRINT` (cert in the runner store) or `ARTIFACT_SIGNING_DLIB` +
     `ARTIFACT_SIGNING_METADATA` (Azure Artifact Signing); `release.ps1` then signs the exes, the
     installer and its uninstaller;
   - add the signer to `PINNED_SIGNERS` in `keys.rs` (`Pin::PublicKeySha256(..)` of the cert's public
     key, or `Pin::Subject(..)`). From then on the updater also requires the extracted exes to be
     Authenticode-signed by that signer.
   The Authenticode paths are written and unit-tested against a signed Microsoft binary, but have not
   been run against a real certificate.

### Where the keys live

| Thing | Where | Rule |
|---|---|---|
| ed25519 **private** key | your file `%USERPROFILE%\.tsubame\update-signing.key` + backup, and the repo secret `UPDATE_SIGNING_KEY` | never committed, never in logs, never in chat |
| ed25519 **public** key | `keys.rs` (compiled into every build) | public |
| DEV test key | `crates\tsubame-update\src\devkey.rs` | public on purpose, trusted only by `test-server` builds and `cargo test`; a release build ignores it |
| Authenticode cert | CI secrets / signing service | later |

CI exposes `UPDATE_SIGNING_KEY` only to the "Build, package, sign the manifest" step. The release script
passes the variable *name* to `tsubame-release`; the key value is never read into a PowerShell variable,
printed, or written to disk.

**Key rotation:** add the new public key to `RELEASE_PUBLIC_KEYS_HEX` next to the old one, ship a release
signed with the old key, then switch signing to the new key. Remove the old key after most users have updated.

## Cutting a release (every time)

1. Pick the version. `X.Y.Z` only (stable channel; no pre-release tags).
   ```powershell
   pwsh tools\release.ps1 -Bump patch -SkipInstaller -DevKey -OutDir dist\rehearsal   # optional dry run
   ```
   or edit `[workspace.package] version` in `Cargo.toml` yourself.
2. Decide `min_supported_version`. Default `0.0.1`. Raise it (`-MinSupported`, or edit the default) only
   when an in-place update from older versions would break (config format change etc.): those users are
   told to run the installer instead.
3. Commit the version bump, `git push`.
4. Run the pre-tag checks: the scripted rig matrix `docs\qa-checklist.md` in full, then the manual
   checks below. Both must be green, or a FAIL/UNTESTABLE waived in `docs\PLAN.md` §9 with a reason.
5. Tag and push the tag. The tag must equal `v` + the Cargo version, or the workflow stops:
   ```powershell
   git tag v0.1.0
   git push origin v0.1.0
   ```
6. Watch the `release` workflow (Actions tab). On success the Release `v0.1.0` exists with the five files above.
7. Smoke test (below).

CI does: check tag == version, `cargo test -p tsubame-update`, Inno Setup (preinstalled, else `choco`),
`tools\release.ps1` (release build, zip, installer, manifest, signature, SHA256SUMS), uploads the files as a
workflow artifact too, then `gh release create`.

### Dry run (no tag)

Rehearse the whole CI release on GitHub without a tag and without publishing anything:

```powershell
gh workflow run release.yml -f dry_run=true          # on the default branch (add --ref <branch> for another)
gh run list --workflow release.yml --limit 1         # find the run id
gh run watch <run-id>
gh run download <run-id> -n release-<version>-dryrun -D dist\dryrun
```

The same thing is in the Actions tab: `release` > Run workflow. A manual run **never** creates a tag or a
GitHub Release: the publish step only runs for a pushed `v*` tag, and `dry_run=false` is refused. The run
skips the "tag must match" check (the version still comes from `Cargo.toml`), runs the same tests, Inno
Setup install, `tools\release.ps1` with the real `UPDATE_SIGNING_KEY` secret (only in that one step's
environment) and uploads the files as the artifact `release-<version>-dryrun` (kept 7 days). The last step
writes every file's size and SHA-256 to the run summary, checks `SHA256SUMS` against the files and verifies
the manifest signature against the key pinned in `keys.rs` with `tsubame-release verify`.

Proves: the workflow YAML, the secret, the toolchain, the tests, the Inno Setup compile, the packaging and
that the signed manifest verifies against the pinned public key.
Does not prove: a silent install and uninstall of the installer (use the installer test below), Authenticode
signing (no certificate yet), `gh release create` and the real Release page, or that the in-app updater
accepts the result (see "The real thing"). The artifact is signed with the real key: do not upload it
anywhere, and delete it (`gh run delete <run-id>`) when done.
### Building a release by hand (without CI)

```powershell
pwsh tools\release.ps1 -KeyFile $env:USERPROFILE\.tsubame\update-signing.key
# optional: run cargo through a wrapper script instead of plain cargo
$env:TSUBAME_CARGO = '<path to a wrapper .ps1>'
```
Output in `dist\<version>\`. Upload all files to a GitHub Release yourself. The installer needs Inno Setup 6
(`winget install JRSoftware.InnoSetup`); without it the script warns and skips the installer.

`release.ps1` refuses to sign with a key whose public half is not pinned in `keys.rs` (the app would reject
that release); `-AllowUnpinned` overrides, `-DevKey` signs with the public DEV key for rehearsals only.

## Manual checks before a tag

Everything else is scripted: the rig QA matrix in `docs\qa-checklist.md` covers preview, Quick Look,
global search, archives, columns, mouse and navigation, the context menu, idle CPU and the exe size
budgets, and an agent runs it in full before the tag (step 4 above). Two checks stay manual because
neither synthetic input nor a development machine can stand in for a real mouse and a clean VM.

### Real mouse: Back and Forward

Synthetic input passed the mouse Back/Forward buttons only 1 of 7 tries (2026-11-24, `PLAN.md` §9), so
check them by hand on the rig desktop, through the normal push-to-rig launch, with a mouse that has side
buttons (the rig display is 4K at 200%).

1. Open Tsubame with one tab and navigate at least five folders deep by double-click; go up two levels.
2. Press the button the OS maps to Back (the one that acts like `Alt+Left`) five times, letting each
   navigation paint before the next press.
3. Press the button that acts like Forward twice.
4. Repeat once with `Alt+Left` / `Alt+Right` and once with the toolbar arrows.

Expected: every press moves exactly one step through the tab's history, the path bar and list follow,
no tab is opened or switched, and no press is swallowed or doubled. Forward stays disabled after the
newest entry. The three input paths (side buttons, chords, toolbar) must show the same history.

### Clean VM: Defender and SmartScreen (unsigned installer)

The installer is unsigned, so the first run on a clean machine warns. That is expected and must be
checked, not assumed. Run this on a clean VM snapshot for every unsigned release; the per-Windows-version
matrix (Windows 11 24H2/25H2, Windows 10 22H2 if still supported, Server 2022) is
`docs\release-checklist.md` step 7 and only needs the full pass on a major release.

1. Restore a clean snapshot: Windows 11 x64, Defender at defaults (real-time and cloud-delivered
   protection on, no exclusions), no previous Tsubame install, a non-admin user.
2. Run the release's `tsubame-X.Y.Z-x64-setup.exe`. SmartScreen must show "Windows protected your PC":
   choose More info > Run anyway. Record the exact wording and a screenshot; the README carries the same
   sentence and must still match.
3. Install per-user (no UAC prompt), launch from the Start menu, open a folder, close the window.
4. Update from the previous version: on a second clean snapshot install N-1, then install this build
   through the in-app updater and restart. Confirm Settings > Updates shows the new version and the
   installed exe was replaced. For a first release with no earlier version, mark this step UNTESTABLE and
   point at `tools\release-e2e.ps1` (local pair, DEV key) instead.
5. Uninstall from Settings > Apps and confirm the install folder, the HKCU Uninstall entry and the
   shortcuts are gone; run the full registry/task/service diff from `docs\release-checklist.md` step 7
   when this is a major release.
6. Check Defender's protection history after install and first launch, and run a custom scan of the
   install folder and of the unzipped portable build. No detection is a PASS. If Defender quarantines or
   submits a file, record the detection name and version and do not publish until it is resolved (also
   submit the exes to the Microsoft file-submission portal, `docs\default-file-manager.md` checklist
   item 18).

Record the VM build, the SmartScreen wording, any Defender finding and the update result in the release
notes or the release PR.

## Testing an update end to end

### Without GitHub (any time, no network, nothing GUI)

```powershell
pwsh tools\release-e2e.ps1
```
Builds the test CLI and a fake GitHub (`tsubame-update-testserver`, feature `test-server`), builds two
releases with the real `release.ps1`, then runs `check`, `download`, `apply`, `rollback` against it, and the
refusals: tampered zip, manifest edited after signing, manifest signed by another key, signing with an
unpinned key. Ends with `e2e: ALL CHECKS PASSED`. The "app" is a stand-in file, so nothing is executed.

Unit and integration tests (including a fake release over real WinHTTP and a watchdog test that starts a
real process and rolls back when it exits early): `cargo test -p tsubame-update`.

### Installer (run on a dedicated test machine, not on a daily-driver PC)

The installer is a GUI program even when silent. Compile `installer\tsubame.iss` with Inno Setup 6
(`ISCC.exe /DAppVersion=<v> /DNumericVersion=<v>.0 /DSourceDir=<folder with tsubame.exe and optionally
tsubame-plughost.exe> /DOutDir=<out> installer\tsubame.iss`; stand-in exe files are fine), then check:

1. Silent install (`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /DIR="<dir>"`), default components: only
   `tsubame.exe` and the uninstaller in the folder (no plugin host, no `portable.txt`); one HKCU Uninstall
   entry with name, version, location and uninstall string (nothing under HKLM); a Start menu shortcut
   pointing at the installed exe; no desktop shortcut.
2. A before/after diff of the Run keys, `HKCU\Software\Classes`, scheduled tasks and services is empty: no
   startup entry, shell or file-association key, scheduled task or service appeared.
3. Updater leftovers (`*.old`, `*.new`, `*.bad`) next to the exe are removed by the uninstaller; user data in
   `%LOCALAPPDATA%\Tsubame` is untouched.
4. Silent uninstall (`unins000.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART`): files, registry entry and
   shortcuts are all gone, and registry, tasks, services and shortcuts match the pre-install snapshot.
5. Repeat with `/COMPONENTS="main,plughost" /TASKS="desktopicon"`: the plugin host and a desktop shortcut
   appear, and are removed again on uninstall.
6. Uninstall while Tsubame is running (start the installed `tsubame.exe`, optionally with a hand-made
   `Directory\shell\Tsubame` verb, then silent-uninstall): the uninstaller closes that exe (and only the one in
   the install folder), removes the verb, and the install folder is gone.

### The real thing (after the first release)

1. Install release N (installer or zip) on the benchmark machine.
2. Publish N+1 with the workflow.
3. On the benchmark machine run `tsubame-update check --force --install-dir <dir>`, then `download`, `apply`, `status`
   (the real CLI talks to GitHub; the `--api-base` flag exists only in test builds).
4. `tsubame-update rollback` and confirm the old file is back.

## What the user must do manually (checklist)

- [ ] Create GitHub repo `watdafawx/tsubame` and push.
- [ ] `pwsh tools\release-keygen.ps1`, back up the key file, paste the public key into `keys.rs`, commit.
- [ ] Add repo secret `UPDATE_SIGNING_KEY`.
- [ ] (later) Buy or apply for a signing certificate, add secrets, pin the signer.
- [ ] Wire the updater into the app (`docs\updater.md`, "Wiring").
