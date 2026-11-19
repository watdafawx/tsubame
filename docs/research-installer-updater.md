# Installer, signing and auto-updater: design notes for P6

Companion to `docs/research-default-explorer-defender.md` (sources, evidence and the detection argument
live there; this file is the build plan). Written 2026-11-17 for the P6 "Ship" task. No code was written,
nothing was installed or run.

Constraints inherited from the plan: per-user install with no admin, optional machine-wide, updates from
GitHub Releases of a public repo, and one opt-in feature that writes per-user shell registry keys
(`PLAN.md` §7 P6). Tsubame is a Rust/Win32 app, 2–6 MB exe, no runtime deps.

---

## 1. Installer technology: decision

| Option | Per-user, no admin | Signed-artifact FP risk | Notes | Verdict |
|---|---|---|---|---|
| **Portable zip** | Yes (nothing installed) | Lowest — no installer stub, no self-extraction | Users unzip and run; the app offers "install for me" as a first-run action (copy to `%LOCALAPPDATA%\Programs\Tsubame`, create Start-menu shortcut, write `Uninstall` entry). No registry beyond the HKCU keys the user opts into. | **Primary artifact, ship first** |
| **Inno Setup**, `PrivilegesRequired=lowest` | Yes | Low–medium. AVs sometimes signature-match the installer *payload*, not the stub; a signed Inno installer was still flagged `Trojan:Win32/Wacatac.B!ml` in Feb 2025 and the author's fix was *downgrading Inno*. | Small, stable, one `.iss`, produces a real Add/Remove Programs entry and a signed uninstaller, supports per-user **and** an optional machine-wide mode from one binary. | **The installer** |
| WiX / MSI | Possible but awkward (per-user MSI needs `ALLUSERS` gymnastics, ICE validation, and repair semantics fight a self-updater that replaces its own exe) | Medium — MSI is well tolerated but custom actions and self-replacement are not | Wrong weight class for a 2-file app; MSI wants to own the files it installed, and our updater wants to rename them. | No |
| NSIS | Yes | **Highest.** NSIS's own wiki documents "hundreds of false positives" caused by engines signature-matching the NSIS *stub*, and tells authors to "compile your own version of NSIS with some modifications". | No reason to accept that. | No |
| MSIX | Yes (per-user by design) | MSIX itself is clean, but: | Cannot write `HKCU\Software\Classes` from inside the package — writes are copy-on-write into a per-package hive and are "visible only inside its container"; file associations/COM must be declared in the manifest, which cannot express "default folder handler". Both apps that shipped this feature packaged retreated (Files added an unpackaged launcher; OneCommander removed the feature from its Store build). | No |

**Also publish to WinGet and Scoop** once the zip layout is stable. These are manifest-only channels: no
new binary, no new signing surface, and they put Tsubame in front of people who already know it is safe.

### 1.1 Layout on disk

```
%LOCALAPPDATA%\Programs\Tsubame\      (per-user install root; machine-wide: %ProgramFiles%\Tsubame)
    tsubame.exe
    tsubame-handler.exe               (folder-handler shim, see doc 1 §5.2)
    tsubame-plughost.exe
    res\                              (themes, icon packs shipped with the app)
    locale\
    restore-explorer.reg              (recovery: removes our shell-verb keys)
    LICENSE, README.txt
%LOCALAPPDATA%\Tsubame\              (user data, never touched by the uninstaller)
    config.toml, session.toml, handler.log, updates\, plugins\, cache\
```

Never mix user data into the install root: it makes "uninstall and keep settings" impossible and makes
per-user vs machine-wide installs behave differently.

### 1.2 Inno Setup specifics

```ini
; release.iss (sketch)
AppId={{7F1D...-...}}
AppName=Tsubame
AppVersion={#Version}
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog      ; "install for all users" needs a click + UAC
DefaultDirName={autopf}\Tsubame                 ; {localappdata}\Programs\Tsubame when lowest
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\tsubame.exe
Compression=lzma2/ultra64                      ; internal, NOT an external packer
SolidCompression=no                            ; keep per-file extraction sane
SignTool=signtool sign /fd sha256 /tr <ts> /td sha256 $f
[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RestoreExplorerDefaults();   // runs BEFORE any file is deleted; see doc 1 §5.4
end;
```

Rules for the installer:

- **The installer must not set, change or "offer" the default-file-manager option.** Not even a checkbox.
  It only copies files, writes Add/Remove Programs entries and (per-user) a Start-menu shortcut. The
  feature is switched on later, in-app, by a click. This keeps us out of "install … without your
  permission" / "circumvent user consent dialogs" territory (`criteria`), and it keeps the elevated
  installer process away from shell-verb writes — the exact shape the UAC-bypass detections look for.
- **The uninstaller restores Explorer first**, and aborts the file deletion if that fails, so we never
  leave a registered path pointing at a deleted exe (the Total Commander failure mode).
- Machine-wide mode must write HKLM/HKCR **nothing** related to the default handler. Optional machine-wide
  install puts the binaries in Program Files; the handler keys stay per-user.
- `Compression=lzma2` is fine (it is the installer's own format); do **not** compress the exes themselves
  with UPX or any packer.
- Sign every binary **and the uninstaller** (`Uninstallable` + `SignTool`). SAC's guidance explicitly
  includes "temp installer files, scripts, and uninstallers".

---

## 2. Signing plan

Three phases, cheapest first. Signing buys publisher identity and enterprise trust; it does not buy
SmartScreen reputation (doc 1 §3.1).

| Phase | When | What | Cost |
|---|---|---|---|
| 0. Unsigned alpha | now → first public beta | Nothing. Publish SHA-256 in the release notes; README says "you will see a SmartScreen warning, here is how to verify the hash". Acceptable for a technical alpha; unacceptable for a 1.0. | 0 |
| 1. Open-source route | if the repo is public | **SignPath Foundation** — free OV-level signing for qualifying open-source projects, managed pipeline. Check current eligibility before counting on it. | 0 |
| 2. Paid route | 1.0 and later, or if phase 1 is not available | **Microsoft Artifact Signing** (ex-Trusted Signing), ~$9.99/month, no hardware token, GitHub Actions integration. **Eligibility caveat:** public trust for individuals only in the US/Canada; organisations in US/CA/EU/UK/AU/NZ/JP/KR/SG/CH/NO/IL. Outside those, a commercial **OV** certificate ($150–300/yr, key on HSM/token since June 2023). **EV is pointless** — since 2024 it no longer bypasses SmartScreen. | ~$120/yr or ~$200/yr |

Operational rules:

- One publisher identity for the life of the product. Changing certificates resets publisher reputation;
  signing consecutive releases with the same identity is what lets reputation accumulate.
- Sign in CI, from a secrets store — never a certificate file in the repo, never on a dev machine.
- Timestamp every signature (`/tr`), so signatures outlive the certificate.
- Pin the signing identity in the app: store the SHA-256 of the signer's public key as a constant, and use
  it for update verification (§3.3).
- Do not sign anything that exhibits unwanted-software behaviour; a certificate can develop *negative*
  reputation.

---

## 3. Auto-updater

### 3.1 Threat model, stated plainly

An updater is a remote-code-execution channel by design. The only thing that makes ours acceptable is that
the payload's authenticity is verified **against a key compiled into the running exe**, not against
anything fetched at update time. "Download a SHA-256 list from the same HTTPS server" proves integrity
against the server, not authenticity against an attacker who controls the release.

### 3.2 Flow

1. **Check** (no more than once per 24 h, only on a user-visible window, never on a hidden timer — the
   plan's zero-wakeup budget): `GET https://api.github.com/repos/<owner>/tsubame/releases/latest` with a
   `User-Agent`; honour `ETag`/`If-None-Match`. Compare `tag_name` to the running version. Do not follow a
   `prerelease` unless the user is on the beta channel.
2. **Offer**, never force. Dialog: version, date, 3-line changelog, total download size, and the two
   choices "Update now" / "Later". Never update silently; never relaunch the user's window underneath them.
3. **Download** to `%LOCALAPPDATA%\Tsubame\updates\<version>\` — never `%TEMP%`, never a world-writable
   folder, never a path built from the release's own text: the artifact named in `manifest.json` (§4).
4. **Verify**, in this order, and stop at the first failure:
   a. size matches the manifest;
   b. SHA-256 matches the manifest;
   c. **Authenticode**: `WinVerifyTrust` with `WINTRUST_ACTION_GENERIC_VERIFY_V2` on the downloaded file,
      then extract the signer certificate and compare SHA-256 of its public key to the pinned constant.
      No new crate is needed (the `windows` crate exposes `WinVerifyTrust`). This is the same signature
      check Windows applies at launch, so a payload that passes is at least *signed by us* — it does not
      exempt it from reputation-based blocking or SmartScreen (doc 1 §3.1).
   d. file version resource `FileVersion` equals the manifest version.
5. **Stage**: `MoveFileEx` the *running* exe to `tsubame.exe.old` (Windows allows renaming a running image,
   not overwriting it), then move the verified new exe into place, then the shim and plug host. Keep the
   `.old` files until the next successful start.
6. **Restart**: `CreateProcess` the new exe from its **final path** with `--after-update <pid>` (the old
   process exits when the new one signals it has a window). Never launch the new binary from the update
   folder, never via the shell, never via `cmd`/`powershell`.
7. **Rollback**: on next start, if the previous run flagged a crash boot (a marker file written before the
   first paint, deleted after it), offer "the update crashed, go back to <previous version>" and restore
   the `.old` files. Default to the previous version if the user does not answer within one launch.

### 3.3 What the updater must never do

- No `%TEMP%` execution, no self-extracting stub that drops an exe and runs it.
- No `powershell -Command`, `cmd /c`, `reg.exe`, `msiexec /qn`, `rundll32`, `certutil`, `bitsadmin`. These
  are the strings every heuristic and every EDR rule set is built on.
- No elevation. If the install is machine-wide and not writable, tell the user an admin install needs an
  admin update; do not pop UAC from the updater.
- No scheduled task, no service, no `Run` key, no startup folder entry. The only persistence in this app is
  the shell-verb keys the user explicitly enabled (doc 1 §5.1) — and the updater must **not** re-create
  them, and must not "repair" them if the user turned them off.
- No re-assertion of defaults after an update, ever.
- No silent channel switching, no telemetry beyond an opt-in update check, no "beta" build pushed to stable
  users.

### 3.4 Update-check telemetry

The GitHub API call leaks the user's IP and version to GitHub. That is unavoidable for this design and
should be stated in the privacy note in one sentence. No other network calls at startup.

---

## 4. GitHub Releases layout and the manifest scheme

One release per version, tag `v1.2.3`, assets:

```
tsubame-1.2.3-x64-setup.exe          Inno, per-user, signed
tsubame-1.2.3-x64.zip                portable, signed binaries inside
tsubame-1.2.3-x64-symbols.zip        PDBs (not signed, not installed)
tsubame-1.2.3-manifest.json          machine-readable: what the updater reads
SHA256SUMS.txt                       human-verifiable: "<hash>  <file>" per artifact
```

`tsubame-<version>-manifest.json`:

```json
{
  "schema": 1,
  "version": "1.2.3",
  "channel": "stable",
  "released": "2026-11-17T12:00:00Z",
  "min_os": "10.0.19041",
  "notes_url": "https://github.com/<owner>/tsubame/releases/tag/v1.2.3",
  "artifacts": [
    { "kind": "setup", "arch": "x64", "file": "tsubame-1.2.3-x64-setup.exe",
      "size": 8123456, "sha256": "…", "authenticode_signer_sha256": "…" },
    { "kind": "portable", "arch": "x64", "file": "tsubame-1.2.3-x64.zip",
      "size": 7321000, "sha256": "…" }
  ]
}
```

Why this shape:

- The **manifest is a convenience, not a trust anchor.** The updater's trust anchor is the pinned signer
  public-key hash in the binary. If someone can rewrite both the asset and the manifest, they still cannot
  produce a file that passes step 4c. Say this in a code comment where the constant lives.
- `SHA256SUMS.txt` exists for humans, forums, VirusTotal cross-checks and the "is my download the one CI
  built" question that every false-positive thread turns into.
- If a second trust anchor is wanted later, add a detached **minisign/Ed25519** signature over the manifest
  and pin that public key too. It is one small crate; do it only if WinVerifyTrust proves awkward in CI.
- The release body starts with a fixed 4-line header (version, date, "signed by <publisher>", "verify with
  SHA256SUMS.txt") and then conventional changelog bullets — because the release page is what people read
  when they are deciding whether the SmartScreen warning is real.

### 4.1 CI sketch (GitHub Actions, windows-latest)

```
cargo build --release --locked           ; no post-build modification of any exe
strip /embed manifest / VERSIONINFO      ; already produced by build.rs
signtool sign /fd sha256 /tr <ts> …      ; exe, shim, plug host, setup.exe, uninstaller
cargo test / leak gate / bench gate      ; must pass or the release does not publish
zip + inno build
sha256sum every asset; write manifest.json with bytes/sizes/hashes and the signer key hash
gh release create vX.Y.Z --notes-file … --attach …
```

Release gate: do not publish a build that has not been through the VM test matrix (doc 1 §5.5) with
Defender's Protection history checked after every registry write.

---

## 5. False positives: standing procedure (not optional work)

1. First time using a new installer-compiler version, a new certificate, or a new shim binary: submit the
   **release candidate** at `https://www.microsoft.com/wdsi/filesubmission` as persona Software Developer,
   signed in, and record the verdict. Do this before the public release, not after the complaints.
2. Keep a `docs/security/false-positives.md` log: date, artifact hash, detection name, submission id,
   verdict, what changed. This is the file that answers "why does Windows say it's a virus" for the next
   two years.
3. If Defender convicts a release: pull the artifact from the release page, submit, wait for the verdict,
   re-publish the same bits (do not rebuild — a rebuild invalidates the hash the analyst cleared).
4. Never instruct users to disable real-time protection. Offer exclusions only as a last resort and only
   with an explicit "this weakens your machine" warning; on tamper-protected machines the user cannot add
   exclusions at all.
5. Re-check the four shipping file managers and the ASR reference every few months; a new signature can
   appear (the IFEO case is the cautionary tale — `Behavior:Win32/ifeoDebugger.DB` existed for years before
   it started hitting legitimate toolkits in 2025).

---

## 6. Open questions for the P6 task (decide there, not here)

1. Per-user install root: `%LOCALAPPDATA%\Programs\Tsubame` (no admin ever, but users can delete it) vs
   defaulting to `%ProgramFiles%` with the "all users" option (needs UAC once, safer against accidental
   deletion of the shim). Recommendation: per-user default, Program Files as the documented option.
2. Whether the handler shim's spawn cost justifies itself (doc 1 §7). Measure on the benchmark machine before locking the
   design; the fallback is to register `tsubame.exe --open %1` directly.
3. Beta channel: one extra GitHub release field plus a channel check, or skip until 1.x.
4. Whether to keep `tsubame-plughost.exe` in the same update unit as the app (it must be, if the IPC/WIT
   contract can change between versions).
