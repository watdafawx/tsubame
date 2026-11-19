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
4. Tag and push the tag. The tag must equal `v` + the Cargo version, or the workflow stops:
   ```powershell
   git tag v0.1.0
   git push origin v0.1.0
   ```
5. Watch the `release` workflow (Actions tab). On success the Release `v0.1.0` exists with the five files above.
6. Smoke test (below).

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
