# Updater design (`crates/tsubame-update`)

Self-update for Tsubame from GitHub Releases. A library (`tsubame_update`), a small CLI
(`tsubame-update check|download|apply|rollback|status`) and two maintainer/test bins. The library is
blocking and never touches any UI: the app calls it from a worker thread and draws the result itself.

Release side (building, signing, publishing): `docs\release.md`. Research behind the choices:
`docs\research-installer-updater.md`.

## Goals and non-goals

- Check GitHub Releases (stable channel only), at most once per 24 h, with ETag / `If-None-Match`.
- Verify before trusting anything: signed manifest, then SHA-256 of the zip, then SHA-256 of each exe.
- Replace the running exe by rename, keep the previous version for one-click rollback, and roll back
  on its own when the new version does not start.
- **Never**: a background service, a scheduled task, a helper process that lingers, a PowerShell/cmd
  one-liner, an elevation prompt, writes to `%TEMP%`, persistence beyond what the user opted into.
  Everything runs inside the app when the user (or the opt-out auto-check) asks.
- Non-goals: delta updates, pre-release channels, per-machine (Program Files) in-place updates
  (those installs are told to run the installer), silent downloads of the update.

## Trust chain

```
 key pinned in the exe (keys.rs)  ──verifies──▶  tsubame-update.json.sig  ──signs──▶  tsubame-update.json
                                                                                          │ sha256 + size
                                                                                          ▼
                                            tsubame-X.Y.Z-win-x64.zip  ──contents[]: name,size,sha256──▶  tsubame.exe, ...
                                                                                          │ (optional, once a cert exists)
                                                                                          ▼
                                                                         Authenticode signer pinned in keys.rs
```

1. **Manifest signature.** ed25519 over `"tsubame-update-manifest-v1\n" + exact manifest bytes` (domain
   separation: a signature made for anything else is no use here). The signature is checked
   *before* the manifest is parsed. The accepted public keys are compiled into the exe
   (`RELEASE_PUBLIC_KEYS_HEX`). **An empty list means every update is refused** (fail closed).
2. **Hashes.** The signed manifest holds the zip's size and SHA-256 and the SHA-256 of each exe inside.
   The zip is streamed to `update\staging\<ver>\` hashing as it arrives, with a hard size cap; only a zip
   that matches is opened. Only names in `UPDATABLE_FILES` (`tsubame.exe`, `tsubame-plughost.exe`) are
   ever extracted, by exact name (no path from the archive is used, so no zip-slip), each checked
   against its own hash, and re-checked right before the swap.
3. **Authenticode (optional, additive).** When `PINNED_SIGNERS` is non-empty each extracted exe must pass
   `WinVerifyTrust` and the leaf certificate must match a pin (SHA-256 of its public key, or subject).
   Revocation is not checked (`WTD_REVOKE_NONE`): the pin decides, and the update must work offline from
   a cached download.

Why ed25519 first and Authenticode later: there is no certificate yet, and a pinned key in the binary needs
none. It is also a *stronger* statement than "some CA signed it": only the holder of one specific key can
publish an update. Dependency: `ed25519-compact` (pure Rust, one file, no dependencies of its own; verify-only
without the `random` feature; the `release-tool` build adds a random source for key generation). TLS,
proxies and the certificate store come from Windows through WinHTTP, so there is no TLS crate in the exe.
`miniz_oxide` (pure Rust, depends only on `adler2`) inflates the zip entries; the zip reader itself is ~100
audited lines in `zip.rs` (stored + deflate, no zip64, no encryption, duplicate names refused).

### Threat model

| Attacker can | Result |
|---|---|
| Man-in-the-middle the connection | TLS (WinHTTP); even with a rogue CA the manifest signature fails |
| Take over the GitHub account / replace release assets | Cannot sign a manifest without the private key: the app refuses; users are not harmed, they just stop updating |
| Replace the zip but keep the manifest | SHA-256 / size mismatch: refused, nothing staged |
| Edit the manifest (e.g. point at another file, lower `min_supported_version`) | Signature fails: refused |
| Replay an old, genuinely signed manifest | Not newer than the installed version: ignored. No downgrade is ever offered |
| Hand-edit `staged.json` or the staging dir between download and apply | Staging path must be inside `update\staging`; each file re-hashed against the manifest hash before the swap |
| Drop an exe name into the manifest (`evil.dll`) | Signed or not, names outside `UPDATABLE_FILES` fail validation |
| Withhold new releases (freeze) | Not mitigated: users stay on the old version. Accepted |
| Steal the private key | Can ship malware to every user. Mitigation: key lives offline + as one CI secret, rotation procedure in `release.md`, Authenticode pin as a second lock |
| Tamper with the local cache of the manifest | The cache is re-verified with the same signature before use; on failure it is deleted |
| Already run code as the user | Out of scope (they can replace the exe directly) |

DEV test key: `devkey.rs` holds a deliberately public key pair for tests and rehearsals. It is only
*trusted* in builds with `cfg(test)` or the `test-server` feature, and `test-server` refuses to compile in
release mode (`compile_error!`). `Config::for_test` (plain http to `127.0.0.1`) is absent from release builds.
A production build also rejects `http://`, including loopback, in `http::get`.

## File layout

```
<install dir>\                       %LOCALAPPDATA%\Programs\Tsubame   (installer) or wherever the zip was unpacked
    tsubame.exe  tsubame-plughost.exe (optional)
    tsubame.exe.new    only while swapping; removed at next start
    tsubame.exe.old    the previous version, kept for one-click rollback
    tsubame.exe.bad    the version that was rolled back from; removed at next start
    portable.txt       marker: data dir moves to <install dir>\data

<data dir>\update\                   %LOCALAPPDATA%\Tsubame\update   (portable: <install dir>\data\update)
    state.json         last check, next allowed check, ETag, latest tag, installed version,
                       previous version, trial boot counter, skipped versions
    cache\             last verified manifest + signature (answers "is there an update" without network)
    staging\<ver>\files\    verified exes waiting to be applied
    staging\<ver>\staged.json
```

The staging area is always under the data dir, never `%TEMP%` (same volume as the install for per-user
installs, no AV scanner churn on a shared temp folder, nothing to clean up elsewhere). The uninstaller removes only
files in the install dir (and `*.old/.new/.bad` next to the exes); the data dir is user data and is kept.

## State machine of an update

```
check(false) ──(<24 h since last)──▶ answer from verified cache, no network
     │
     ▼ GET api.github.com/repos/<repo>/releases/latest   (If-None-Match: <etag>)
     │    304 → cache;  200 → tag, then GET tsubame-update.json + .sig from the release
     │    403/429 → back off 24 h;  other failure → retry after 1 h;  "Check now" ignores the 1 h but not the 429 back-off
     ▼ verify signature → parse + validate → tag must equal manifest version → newer than installed?
Release { version, notes_url, in_place_ok, download_size, ... }          (Option::None = up to date)
     │ download(&release, progress)       progress returns false = cancel
     ▼ stream zip → size+sha256 → extract listed exes → per-exe sha256 → (Authenticode)
Staged { dir, files }      persisted as staged.json: also survives an app restart ("apply on next start")
     │ apply(&staged)
     ▼ copy to <exe>.new, verify the copy; rename <exe> → <exe>.old; rename <exe>.new → <exe>
       any failure undoes every rename already done. state: previous = old version, trial = new version (boots 0)
Applied { exe, version, previous_version }
     │ relaunch_watched(args, timeout)   then the caller exits
     ▼ starts new exe with `--after-update <old pid> <args>` and watches it:
         new version calls on_start() → trial boots=1 → … → on_ready() clears the trial   ⇒ Started
         exits before on_ready, or no on_ready within timeout                                ⇒ kill, rollback(), start old again
```

`rollback()` swaps `<exe>.old` back (current → `.bad`), forgets the version it rolled back from
(`skipped`, so it is not offered again), and clears `previous`. Rollback is one step deep by design.
`on_start()` reports `CrashedAfterUpdate` when a trial version is started again without ever reaching
`on_ready` (the watchdog was not there, e.g. the user started it by hand): the app should offer
"go back to X" and, from the 3rd failed boot, do it without asking.

If `min_supported_version` is newer than what is installed, `Release.in_place_ok` is false, `download`
refuses (`Error::TooOld`) and the UI should send the user to the release page for the installer.

## Library API (what the follow-up wiring task calls)

```rust
use tsubame_update::{Updater, Release, Staged, Progress, Startup, WatchOutcome, Error};

Updater::production(env!("CARGO_PKG_VERSION")) -> Result<Updater>   // layout = folder of the running exe (portable.txt aware)
up.on_start()            -> Startup                  // first thing in main
up.on_ready()                                         // after the first paint of the main window
up.check(force: bool)    -> Result<Option<Release>>  // blocking; false = honour 24 h limit, true = "Check now"
up.download(&Release, &mut dyn FnMut(Progress) -> bool) -> Result<Staged>   // return false to cancel
up.staged()              -> Option<Staged>            // a verified download from an earlier run
up.apply(&Staged)        -> Result<Applied>
up.relaunch_watched(&[OsString], Duration) -> Result<WatchOutcome>   // then exit the process
up.relaunch(&[OsString]) -> Result<u32>               // start without watching (after a manual rollback)
up.rollback()            -> Result<RolledBack>
up.previous_version()    -> Option<String>            // Some = show the "Roll back" button
up.discard_previous()    -> Result<()>                // frees the .old files
up.status()              -> Status { running, previous, last_check_unix, staged }
spawn_check(Arc<Updater>, force, done: impl FnOnce(Result<Option<Release>>) + Send) -> JoinHandle<()>
```
`Error` implements `Display` with a message fit for the UI ("signature check failed...",
"SHA-256 mismatch for ...", "... is still in use; restart Tsubame once, then update again", ...).

## Wiring into the app (follow-up task)

None of this is done yet; `src\` and the UI crates were deliberately not touched.

1. **`Cargo.toml` (root):** `tsubame-update.workspace = true` (already declared in `[workspace.dependencies]`).
2. **`main`, as early as possible** (before the single-instance check, because a relaunched copy must wait for its parent):
   ```rust
   let up = std::sync::Arc::new(tsubame_update::Updater::production(env!("CARGO_PKG_VERSION"))?);
   // `--after-update <pid>`: the old process is exiting; wait for it (OpenProcess + WaitForSingleObject, <= 10 s) before taking the lock.
   match up.on_start() {
       Startup::Normal => {}
       Startup::Trial { .. } => {}                                   // normal start, update still on probation
       Startup::CrashedAfterUpdate { boots, .. } if boots >= 3 => { let _ = up.rollback(); let _ = up.relaunch(&[]); return; }
       Startup::CrashedAfterUpdate { .. } => { /* show a banner: "Tsubame crashed after updating. [Go back to <previous>]" */ }
   }
   ```
3. **After the first frame is presented:** `up.on_ready();` (this is what the watchdog waits for; it ends the trial).
4. **Auto-check on start** (setting `updates.check_on_start`, default on, user can turn it off): on a worker thread
   `spawn_check(up.clone(), false, |r| post_to_ui(UpdateMsg::Checked(r)))`. The 24 h limit makes this a no-op most launches.
   Never download or apply without a click.
5. **Settings > Updates** (one screen, plugs into the settings page list):
   - Header: "Tsubame {status.running}", last checked (from `status().last_check_unix`), toggle `Check for updates when Tsubame starts`.
   - `[Check now]` → `spawn_check(up.clone(), true, ...)`; while running show a spinner; result text: "You're up to date." /
     "Version X is available" + `[Release notes]` (open `release.notes_url` in the browser) + `[Download and restart]` /
     the error's `Display` text.
   - `[Download and restart]` → worker thread:
     ```rust
     let staged = up.download(&rel, &mut |p| { post_to_ui(UpdateMsg::Progress(p)); !cancel_flag.load(Relaxed) })?;
     up.apply(&staged)?;                       // swap done, old exe kept as tsubame.exe.old
     post_to_ui(UpdateMsg::ReadyToRestart);    // UI saves session, destroys windows, then on the UI thread:
     match up.relaunch_watched(&[], Duration::from_secs(30))? { WatchOutcome::Started{..} | WatchOutcome::RolledBack{..} => std::process::exit(0) }
     ```
     Show a progress bar from `Progress { done, total }` and a `[Cancel]`.
   - If `release.in_place_ok == false` or `apply` returns `Error::Install(..)` (read-only install, all-users install): replace
     the button by `[Open download page]` with text "This copy has to be updated with the installer."
   - `[Roll back to {previous}]` shown when `up.previous_version().is_some()` → confirm → `up.rollback()` then `up.relaunch(&[])` and exit.
   - After a successful start of the new version show "Updated to X. [Release notes]" once (compare `Startup::Trial`).
6. **Threading:** every call except `on_start`, `on_ready` and `status` can block (network, disk). Call them from a worker, post results
   to the UI thread. `Updater` is `Send + Sync`; share it in an `Arc`.
7. **Do not** add a Run key, a task, or a service for updating. The only moment the updater runs is while the app is open and the
   user (or the opt-out check on start) asked.

CLI (same logic, for scripts, rig tests and support): `tsubame-update check [--force] | download | apply [--restart] | rollback [--restart] | status`,
flags `--install-dir`, `--current-version`. Exit code 3 from `check` means "update available".

## Rate limiting and politeness

Anonymous GitHub API: 60 requests/hour per IP. A check is 1 request (the `releases/latest` JSON) plus, only when there is
a new tag, 2 small asset downloads (manifest, signature). With the 24 h limit, the ETag (a 304 does not count against the
limit) and the verified cache, a normal day costs 0–1 requests. A 403/429 stops all checks for 24 h. Requests send a
`User-Agent: Tsubame-Updater/<version>` and the repo's public coordinates only: no machine id, no usage data, no cookies.

## Deviations from `docs\research-installer-updater.md`

- Manifest field is `assets` (with `kind`/`arch`/`file`/`size`/`sha256`/`contents`), not `artifacts`.
- ed25519 manifest signature is **always** required; Authenticode is an additional pin once a certificate exists.
  (The research suggested Authenticode-only as the end state.)
- `Pin::Subject` is allowed as well as the public-key hash (the key hash is preferred; subject is easier to bootstrap).
- The previous exe is **kept** as `.old` for one-click rollback instead of deleted on next start.
- Added a watchdog relaunch (`relaunch_watched`) and a trial-boot counter so a new version that dies at start is undone automatically.
- No FileVersion cross-check of the extracted exe and no revocation check (documented above).
- A small custom semver and zip reader instead of the `semver`/`zip` crates, to keep the dependency list to
  `ed25519-compact`, `miniz_oxide`, `serde`, `serde_json`, `windows`.
- The installer does not call anything like `RestoreExplorerDefaults()`: the default-folder-handler feature is not built yet. When it is,
  its uninstall hook belongs in `installer\tsubame.iss` `[Code]` (see the research doc, section 1.2).

## Known limits

- The Authenticode path is tested for the negative case (an unsigned exe is refused) and the positive case against a signed Microsoft
  binary found on the machine, not against a Tsubame certificate (none exists yet).
- `release.ps1` Authenticode and Artifact Signing branches are written but unexercised.
- Updating a machine-wide (Program Files) install in place fails with a clear message instead of prompting for elevation, by design.
- Antivirus products may briefly lock a freshly written exe; `apply` then fails cleanly with everything put back and can simply be retried.
