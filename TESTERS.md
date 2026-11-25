# Testing Tsubame nightly builds

Thanks for helping test. This page explains what the nightly channel is, how to get it, what Windows will
say about it, and how to report a problem so it can be fixed.

## What "nightly" means

- A **nightly** is built automatically from the newest code on `main` after the automated tests pass. Nobody
  has tried it by hand. It can have bugs, lose settings, or crash.
- It is published as one GitHub pre-release called **nightly**:
  <https://github.com/watdafawx/tsubame/releases/tag/nightly>. Each new build replaces the previous one.
- Version numbers look like `0.1.1-nightly.42`: the next version that will ship (`0.1.1`), plus the build
  number (`42`). A higher build number is newer.
- **Stable** is the default and what most people should use. Stable releases are tagged `vX.Y.Z` and are
  checked by hand before they ship.

Nightlies go through the same update checks as stable releases: the update file list is signed with the
project's release key, and every download is checked against its SHA-256 hash before anything is replaced.
What a nightly lacks is manual testing, not security checks.

Back up anything important before you test, especially `%APPDATA%\Tsubame\config.toml` (or the `data`
folder next to a portable copy).

## Getting it

Pick one:

- **Switch an installed copy:** open Settings (Ctrl+,) > Updates > Update channel > **Nightly**, then click
  **Check now**. The next nightly installs like any update, when you restart.
  Or set it in `config.toml`:
  ```toml
  [updates]
  channel = "nightly"
  ```
- **Fresh install:** download `tsubame-<version>-x64-setup.exe` (installer) or
  `tsubame-<version>-win-x64.zip` (portable) from the nightly release page. A fresh nightly install still
  follows the channel in its settings, so also set the channel to **Nightly** in Settings if you want to keep
  getting nightlies.

A copy on the nightly channel is also offered the next **stable** release once that release is newer than
your nightly.

## Windows SmartScreen and unsigned builds

The builds are not code-signed (there is no Authenticode certificate yet). The first time you run the
installer or a freshly unzipped `tsubame.exe`, Windows may show **"Windows protected your PC"**:

1. Click **More info**.
2. Check that the file name is the one you downloaded from the nightly release page.
3. Click **Run anyway**.

If you want to check a download yourself, compare it with `SHA256SUMS` from the same release:

```powershell
Get-FileHash .\tsubame-0.1.1-nightly.42-x64-setup.exe -Algorithm SHA256
```

Updates that the app installs itself do not trigger SmartScreen; they are checked against the signing key
built into the app.

If Microsoft Defender flags a nightly, do not run it. Report it (below) with the detection name.

## Going back to stable

Settings > Updates > Update channel > **Stable**. Tsubame never installs an older version, so it keeps the
nightly you have and offers the next stable release once that release is newer than your nightly. A nightly
that was downloaded but not installed yet is discarded.

To go back right away, install the latest stable release from
<https://github.com/watdafawx/tsubame/releases/latest> with its installer (or unpack its zip).

## Reporting a problem

Open an issue: <https://github.com/watdafawx/tsubame/issues/new>. Include:

1. **The exact version line.** Open Settings > Updates: the card shows the version, the channel and the
   build's commit (for example `Tsubame 0.1.1-nightly.42` with `nightly · abc1234`). From a terminal,
   `tsubame.exe --version` prints the same as one line: `Tsubame 0.1.1-nightly.42 (nightly, abc1234)`.
2. What you did, what you expected, and what happened instead. Steps that reproduce it are the most useful
   part.
3. Windows version (`winver`) and whether this is the installed or the portable copy.
4. For a crash or a freeze: the time it happened, and a screenshot if something is drawn wrong.

Please don't attach your whole `config.toml` or folder listings that contain private file names; copy only
the lines that matter.

The release notes on the nightly page list the commits that went into each build, so you can see whether a
fix has landed yet.
