# Release checklist for 0.1.0

What is still between the current tree and a published 0.1.0, who does each step, and what is
already proven without a code-signing certificate. How-to for the release itself:
`docs\release.md`. Updater design: `docs\updater.md`. Folder handler: `docs\default-file-manager.md`.

Status: 2026-11-19, workspace version `0.0.1` (the bump to `0.1.0` is step 9).

## Proven already (no certificate involved)

| Area | What was checked | How |
|---|---|---|
| Installer | Compiles; silent install per-user (only `tsubame.exe` + uninstaller, HKCU entry, Start menu shortcut, nothing under HKLM); no Run key, shell/file-association key, scheduled task or service added; updater leftovers (`*.old`/`*.new`/`*.bad`) removed; silent uninstall leaves registry, tasks, services and shortcuts identical to the pre-install snapshot; plugin host and desktop shortcut are optional and removed again; user data untouched | `tools\release-installer-test.ps1` (not tracked: run it on a disposable test machine only) |
| Uninstall while running | The installed exe runs and locks its file; the uninstaller closes it (5 s force-kill path, 6.2 s total), the folder is gone, and a second copy running from another folder is **not** killed | same script, case 4 |
| Updater N -> N+1 | A locally built pair (`0.0.1` and `0.0.2`) packaged by `tools\release.ps1` and signed with the public DEV test key: `check` offers `0.0.1 -> 0.0.2`, `download` stages, `apply` installs, `0.0.1` is kept as `tsubame.exe.old`. Manifest signed by an unrelated key, and manifest edited after signing: refused, exe untouched. Lower version (publish `0.0.1` to a `0.0.2` install): never offered, nothing download-able, exe untouched | `cargo test -p tsubame-update` (53 tests, incl. `n_to_n_plus_one_dev_key_accepts_newer_refuses_bad_signature_and_downgrade`), `tools\release-e2e.ps1` |
| Update trust model | Signature checked before parsing; a build with no pinned key refuses everything; `test-server`-only DEV key is not in release builds; the real release key pinned in `keys.rs` is never needed for these tests | `cargo test -p tsubame-update` |

Not covered, by decision for 0.1.0: Authenticode (no certificate; step 5), the real Release page,
SmartScreen/Defender behaviour on a clean machine (step 7), and an update from a *published* release to
the next one.

## Remaining steps

| # | Step | Owner | Notes |
|---|---|---|---|
| 1 | Create the public GitHub repo `watdafawx/tsubame` and push | maintainer | The updater reads Releases anonymously. The name lives in `DEFAULT_REPO` (`crates\tsubame-update\src\lib.rs`) and in `installer\tsubame.iss`; renaming later means a new build everywhere. |
| 2 | Update-signing key: generate once, back up twice, pin the public half | maintainer | `tools\release-keygen.ps1` writes the private key outside the repo and prints the public key; paste it into `RELEASE_PUBLIC_KEYS_HEX` (`crates\tsubame-update\src\keys.rs`). A public key is already pinned there: **confirm it is the key whose private half will be the repo secret** — a wrong pin means every update is refused by installed copies. If the private key is lost, users can only update by reinstalling by hand. |
| 3 | Add the repo secret `UPDATE_SIGNING_KEY` | maintainer | Repo settings > Secrets and variables > Actions, value = the private key file. Only the "Build, package, sign" workflow step can see it. |
| 4 | Dry run the release workflow | maintainer | `gh workflow run release.yml -f dry_run=true`, read the run summary (sizes, SHA-256, `SHA256SUMS` consistency, manifest verified against the pinned key). The artifact is signed with the real key: delete the run afterwards. Proves the YAML, the secret, tests, Inno Setup and packaging without tagging anything. |
| 5 | Authenticode certificate | maintainer: **decided, ship unsigned for 0.1.0** | No certificate is bought for 0.1.0, so nothing is Authenticode-signed and `PINNED_SIGNERS` in `crates\tsubame-update\src\keys.rs` stays empty. The update path does not depend on it: the ed25519 key pinned in `RELEASE_PUBLIC_KEYS_HEX` in the same file is what makes an update trustworthy, and a certificate would only add a second lock. Revisit after 0.1.0 if the SmartScreen warning is judged to cost installs: OV certificate (identity vetting), EV certificate (token/HSM), or Microsoft Artifact Signing. Then add the secrets (`SIGN_CERT_THUMBPRINT`, or `ARTIFACT_SIGNING_DLIB` + `ARTIFACT_SIGNING_METADATA`), add the signer to `PINNED_SIGNERS`, and repeat the dry run. The Authenticode code path is unit-tested against a signed Microsoft binary but has **never run against a Tsubame certificate**, so treat the first signed dry run as new code. |
| 6 | SmartScreen wording | maintainer: decided, covered by the README | The 0.1.0 installers stay unsigned, so the first run on a clean machine shows "Windows protected your PC" (More info > Run anyway), and Defender may submit the exe for scanning. The README "Installing" section carries that wording; use the same sentence on the release page. Publish `SHA256SUMS` and say how to check it. Do not promise "no warning". |
| 7 | Defender and VM test matrix | maintainer | Run the release installer and the portable zip on a clean VM per row, all with Defender and SmartScreen at default: Windows 11 24H2 and 25H2 (x64), Windows 10 22H2 x64 (consumer support has ended, so only if it is still a stated target), Windows Server 2022 or later (the folder handler must fail safe there). Per machine: per-user install as a non-admin user (no UAC prompt), launch, "Default file manager" on then off, uninstall (also while running), and the N -> N+1 update plus rollback. Submit `tsubame.exe`, `tsubame-handler.exe`, `tsubame-plughost.exe` and the installer to the Microsoft file-submission portal and record the submission ids (see `docs\default-file-manager.md`). |
| 8 | Repeat the installer test with the shipped build | maintainer | `tools\release-installer-test.ps1` uses stand-in exes; the running-exe case should be repeated once with the real `tsubame.exe` (start it, uninstall, confirm one window closes and the folder goes). |
| 9 | Bump the version to 0.1.0 | maintainer | Every place in "Version bump" below. `tools\release.ps1 -Bump` rewrites only `[workspace.package] version` in `Cargo.toml`, so the `.rc`/`.manifest` copies are by hand and `Cargo.lock` follows from the next build. |
| 10 | Tag and push the tag | maintainer | `git tag v0.1.0`; `git push origin v0.1.0`. The workflow refuses a tag that differs from the workspace version, runs the updater tests, builds, signs the manifest, and publishes the Release with five files. |
| 11 | Post-release smoke test | maintainer | Install the published 0.1.0 on the test machine, then confirm the in-app update path against the next build (release N+1 dry run or a local `dist\` build served by the test server). Rollback must restore the previous exe. |
| 12 | Publish the source and the updater docs with the release | maintainer | `README` must say what `tsubame-handler.exe` is and that "Default file manager" is opt-in, so the shim is not mistaken for a hijacker. |

## Blockers

Nothing blocks 0.1.0. Steps 5 and 6 are decided (ship unsigned) rather than waiting on a certificate; the
rest is maintainer time or a clean machine. Order matters in three places only: step 2 before steps 3 and
4 (the pinned public key must match the private key that becomes the secret), step 9 before step 10 (the
tag must equal the workspace version), and step 7 (the VM matrix) passing before step 10.

## Version bump (step 9)

`0.0.1` appears in the places below. Only the first group changes for 0.1.0.

**Bump**

- `Cargo.toml`, `[workspace.package] version = "0.0.1"` to `"0.1.0"`. Every crate inherits it
  (`version.workspace = true`), and `env!("CARGO_PKG_VERSION")` carries it into `tsubame --version`, the
  updater's running version and the Settings > Updates header.
- `Cargo.lock`: the eleven `tsubame*` workspace entries pin `0.0.1`. The next `cargo build` or
  `cargo test` rewrites them; commit the result.
- `res\tsubame.rc`: `FileVersion` and `ProductVersion` are `0.0.1.0` (the 4-part form; `build.rs` embeds
  the file into `tsubame.exe`).
- `res\tsubame.manifest`: `assemblyIdentity version="0.0.1.0"`.
- `crates\tsubame-handler\res\handler.rc`: `FileVersion` and `ProductVersion` `0.0.1.0`.
- `crates\tsubame-handler\res\handler.manifest`: `assemblyIdentity version="0.0.1.0"`.
- `README.md`, the Status paragraph ("version 0.0.1").

**Leave alone**

- `installer\tsubame.iss` holds no version literal: `AppVersion` and `NumericVersion` arrive as
  `/DAppVersion` and `/DNumericVersion`, which `tools\release.ps1` fills from `Cargo.toml`. The `0.1.0` in
  its header is an example command line.
- `plugins\sample-column\Cargo.toml` is `0.0.1` in its own workspace (`exclude`d from the app build), so
  it changes only if the sample should match.
- `tools\release.ps1` (`-MinSupported = '0.0.1'`) and `docs\release.md` step 2 are the
  `min_supported_version` floor, not the app version. Leaving `0.0.1` lets a `0.0.x` install update in
  place; raise it only for a breaking change.
- Test data that spells versions out: `crates\tsubame-update\src\tests.rs` (`0.0.1` to `0.0.2`),
  `crates\tsubame-update\src\version.rs`, `manifest.rs`, and `tools\release-e2e.ps1` (`0.1.0` to `0.2.0`).
  These are fixtures, not the product version.

## Re-running the proofs (no certificate, no network for the tests)

```powershell
pwsh -NoProfile -File tools\release-e2e.ps1                 # two stand-in releases, DEV key, fake GitHub
pwsh -NoProfile -File tools\release-installer-test.ps1 -Work <throw-away dir>   # on a disposable machine only
```

`cargo test -p tsubame-update` covers the same refusals as unit tests, including the 0.0.1 -> 0.0.2
step. None of these ever reads or needs the real release key.
