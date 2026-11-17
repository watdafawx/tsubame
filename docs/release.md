# Cutting a release

Everything a release needs is in the repo: `tools\release.ps1` (build, package, sign), the
`release` GitHub workflow (runs it on a tag and publishes), `installer\tsubame.iss` (Inno Setup).
Design of the updater itself: `docs\updater.md`.

## What a release contains

Files attached to the GitHub Release `vX.Y.Z`:

| File | What | Used by |
|---|---|---|
| `tsubame-X.Y.Z-win-x64.zip` | portable build: `tsubame.exe`, optional `tsubame-plughost.exe`, `portable.txt`, `LICENSE`, `README.txt` | people, and the in-app updater |
| `tsubame-X.Y.Z-x64-setup.exe` | Inno Setup installer (per-user, optional all-users) | people |
| `tsubame-update.json` | manifest: version, assets, sizes, SHA-256, per-exe SHA-256, notes URL, `min_supported_version` | updater |
| `tsubame-update.json.sig` | ed25519 signature of the manifest | updater |
| `SHA256SUMS` | SHA-256 of all of the above (for humans, `winget`, `scoop`) | people |

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

### Building a release by hand (without CI)

```powershell
pwsh tools\release.ps1 -KeyFile $env:USERPROFILE\.tsubame\update-signing.key
# FOX: go through the CCD1 wrapper
$env:TSUBAME_CARGO = 'X:\Dev\tools\cargo-ccd1.ps1'
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

### Installer (run on rig, never on FOX)

The installer is a GUI program even when silent. Copy `installer\tsubame.iss` and a folder with the exes to
rig (`G:\rig-test\<name>\src\installer`, `...\src\exes`), then on rig:
```powershell
pwsh G:\rig-test\<name>\release-installer-test.ps1 -Work G:\rig-test\<name> -Cleanup
```
It compiles the installer, installs silently into `<Work>\install`, checks the files and the HKCU
Uninstall key, checks that no startup entry, shell key, scheduled task or service appeared, uninstalls
silently and checks that everything is gone (and that user data in `%LOCALAPPDATA%\Tsubame` is untouched).

### The real thing (after the first release)

1. Install release N (installer or zip) on rig.
2. Publish N+1 with the workflow.
3. On rig run `tsubame-update check --force --install-dir <dir>`, then `download`, `apply`, `status`
   (the real CLI talks to GitHub; the `--api-base` flag exists only in test builds).
4. `tsubame-update rollback` and confirm the old file is back.

## What the user must do manually (checklist)

- [ ] Create GitHub repo `watdafawx/tsubame` and push.
- [ ] `pwsh tools\release-keygen.ps1`, back up the key file, paste the public key into `keys.rs`, commit.
- [ ] Add repo secret `UPDATE_SIGNING_KEY`.
- [ ] (later) Buy or apply for a signing certificate, add secrets, pin the signer.
- [ ] Wire the updater into the app (`docs\updater.md`, "Wiring").
