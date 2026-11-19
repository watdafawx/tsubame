# Release checklist for 0.1.0

What is still between the current tree and a published 0.1.0, who does each step, and what is
already proven without a code-signing certificate. How-to for the release itself:
`docs\release.md`. Updater design: `docs\updater.md`. Folder handler: `docs\default-file-manager.md`.

Status: 2026-11-19, workspace version `0.0.1`.

## Proven already (no certificate involved)

| Area | What was checked | How |
|---|---|---|
| Installer | Compiles; silent install per-user (only `tsubame.exe` + uninstaller, HKCU entry, Start menu shortcut, nothing under HKLM); no Run key, shell/file-association key, scheduled task or service added; updater leftovers (`*.old`/`*.new`/`*.bad`) removed; silent uninstall leaves registry, tasks, services and shortcuts identical to the pre-install snapshot; plugin host and desktop shortcut are optional and removed again; user data untouched | `tools\release-installer-test.ps1` (not tracked: run it on a disposable test machine only) |
| Uninstall while running | The installed exe runs and locks its file; the uninstaller closes it (5 s force-kill path, 6.2 s total), the folder is gone, and a second copy running from another folder is **not** killed | same script, case 4 |
| Updater N -> N+1 | A locally built pair (`0.0.1` and `0.0.2`) packaged by `tools\release.ps1` and signed with the public DEV test key: `check` offers `0.0.1 -> 0.0.2`, `download` stages, `apply` installs, `0.0.1` is kept as `tsubame.exe.old`. Manifest signed by an unrelated key, and manifest edited after signing: refused, exe untouched. Lower version (publish `0.0.1` to a `0.0.2` install): never offered, nothing download-able, exe untouched | `cargo test -p tsubame-update` (53 tests, incl. `n_to_n_plus_one_dev_key_accepts_newer_refuses_bad_signature_and_downgrade`), `tools\release-e2e.ps1` |
| Update trust model | Signature checked before parsing; a build with no pinned key refuses everything; `test-server`-only DEV key is not in release builds; the real release key pinned in `keys.rs` is never needed for these tests | `cargo test -p tsubame-update` |

Not covered yet: Authenticode (no certificate), the real Release page, SmartScreen/Defender
behaviour on a clean machine, and an update from a *published* release to the next one.

## Remaining steps

| # | Step | Owner | Notes |
|---|---|---|---|
| 1 | Create the public GitHub repo `watdafawx/tsubame` and push | maintainer | The updater reads Releases anonymously. The name lives in `DEFAULT_REPO` (`crates\tsubame-update\src\lib.rs`) and in `installer\tsubame.iss`; renaming later means a new build everywhere. |
| 2 | Update-signing key: generate once, back up twice, pin the public half | maintainer | `tools\release-keygen.ps1` writes the private key outside the repo and prints the public key; paste it into `RELEASE_PUBLIC_KEYS_HEX` (`crates\tsubame-update\src\keys.rs`). A public key is already pinned there: **confirm it is the key whose private half will be the repo secret** — a wrong pin means every update is refused by installed copies. If the private key is lost, users can only update by reinstalling by hand. |
| 3 | Add the repo secret `UPDATE_SIGNING_KEY` | maintainer | Repo settings > Secrets and variables > Actions, value = the private key file. Only the "Build, package, sign" workflow step can see it. |
| 4 | Dry run the release workflow | maintainer | `gh workflow run release.yml -f dry_run=true`, read the run summary (sizes, SHA-256, `SHA256SUMS` consistency, manifest verified against the pinned key). The artifact is signed with the real key: delete the run afterwards. Proves the YAML, the secret, tests, Inno Setup and packaging without tagging anything. |
| 5 | **Authenticode certificate** | **maintainer: must buy or provide it** | Not something the code can supply. Options: classic OV code-signing certificate (identity vetting, file or token), EV certificate (hardware token/HSM, faster SmartScreen reputation), or Microsoft Artifact Signing (cloud signing, no token; pin the signer by certificate subject). Then add the secrets (`SIGN_CERT_THUMBPRINT`, or `ARTIFACT_SIGNING_DLIB` + `ARTIFACT_SIGNING_METADATA`), add the signer to `PINNED_SIGNERS` in `keys.rs`, and repeat step 4. From that point the updater also requires the extracted exes to be Authenticode-signed by that signer. The Authenticode code path is unit-tested against a signed Microsoft binary but has **never run against a real certificate**: treat the first signed dry run as new code and check every file is signed (app, folder handler, plugin host, installer, uninstaller). |
| 6 | Decide the SmartScreen story for 0.1.0 | maintainer | Without a certificate the first run on a clean machine shows "Windows protected your PC" (More info > Run anyway), and Defender may submit the exe for scanning. With an OV certificate reputation still starts at zero and builds over downloads; EV and Artifact Signing usually clear it sooner. Do not promise "no warning". Publish `SHA256SUMS` and say how to verify it. |
| 7 | Defender and VM test matrix | maintainer | Run the release installer and the portable zip on a clean VM per row, all with Defender and SmartScreen at default: Windows 11 24H2 and 25H2 (x64), Windows 10 22H2 x64 (consumer support has ended, so only if it is still a stated target), Windows Server 2022 or later (the folder handler must fail safe there). Per machine: per-user install as a non-admin user (no UAC prompt), launch, "Default file manager" on then off, uninstall (also while running), and the N -> N+1 update plus rollback. Submit `tsubame.exe`, `tsubame-handler.exe`, `tsubame-plughost.exe` and the installer to the Microsoft file-submission portal and record the submission ids (see `docs\default-file-manager.md`). |
| 8 | Repeat the installer test with the shipped build | maintainer | `tools\release-installer-test.ps1` uses stand-in exes; the running-exe case should be repeated once with the real `tsubame.exe` (start it, uninstall, confirm one window closes and the folder goes). |
| 9 | Bump to 0.1.0, commit, tag, push the tag | maintainer | `[workspace.package] version = "0.1.0"` in `Cargo.toml`; `git tag v0.1.0`; `git push origin v0.1.0`. The workflow refuses a tag that differs from the workspace version, runs the updater tests, builds, signs, publishes the Release with five files. |
| 10 | Post-release smoke test | maintainer | Install the published 0.1.0 on the test machine, then confirm the in-app update path against the next build (release N+1 dry run or a local `dist\` build served by the test server). Rollback must restore the previous exe. |
| 11 | Publish the source and the updater docs with the release | maintainer | `README` must say what `tsubame-handler.exe` is and that "Default file manager" is opt-in, so the shim is not mistaken for a hijacker. |

## Blockers

Only step 5 costs money and needs an identity the maintainer controls; it is the one item the
project cannot finish on its own. Everything else is maintainer time and can happen in any order,
except that step 2 must come before steps 3, 4 and 9, and step 9 after step 7 passes.

## Re-running the proofs (no certificate, no network for the tests)

```powershell
pwsh -NoProfile -File tools\release-e2e.ps1                 # two stand-in releases, DEV key, fake GitHub
pwsh -NoProfile -File tools\release-installer-test.ps1 -Work <throw-away dir>   # on a disposable machine only
```

`cargo test -p tsubame-update` covers the same refusals as unit tests, including the 0.0.1 -> 0.0.2
step. None of these ever reads or needs the real release key.
