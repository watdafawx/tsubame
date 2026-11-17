# Tsubame (燕)

Tsubame is a very fast, feature-rich file browser for Windows, written in Rust on raw Win32 with a Direct2D/DirectWrite renderer and its own immediate-mode UI kit; the goal is File Pilot's look and speed with the features of Files and OneCommander, and no leaks. The full design — stack, budgets, architecture, plugin tiers, phases and task cards — lives in [docs/PLAN.md](docs/PLAN.md).

## Building

Never run bare `cargo` for anything that compiles. Every build, check, test or run goes through the CCD1-pinned wrapper, from the repo root:

```
pwsh -NoProfile -File X:\Dev\tools\cargo-ccd1.ps1 check --workspace
pwsh -NoProfile -File X:\Dev\tools\cargo-ccd1.ps1 build
```

It pins the build to FOX's non-V-Cache CCD so games keep the fast cores, and it picks a sensible `-j`. GUI runs never happen on FOX; they go to rig via `X:\Dev\tools\push-to-rig.ps1`.
