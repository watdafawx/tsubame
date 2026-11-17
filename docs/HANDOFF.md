# Handoff

Updated 2026-11-17 by the planning session.

## State
- Phase: **P0 (spikes)**.
- `main` is at `Scaffold workspace`. `cargo check --workspace` passes.
- Name and logo are decided: `res/tsubame.svg`, `res/tsubame-small.svg`, sheet in `mocks/logo-final.html`.

## Tasks
| ID | State | Notes |
|---|---|---|
| S0 | merged | Workspace, 7 crate stubs, release profile from blink. `windows = "0.61"` (same as blink, so `gfx.rs` ports without API changes). |
| S0b | merged | Icon (6 frames), manifest (PerMonitorV2, longPathAware), version info, `tools/make-icon.ps1`, `mocks/tokens.json` brand colours. exe 152 KB. No live Explorer/taskbar screenshot (main.rs is still a windowless stub); verified with `SHGetFileInfoW` on FOX and rig. Small cut kept (user has not answered the open question). |
| S1 | running | Direct2D list (Sonnet). |
| S2 | running | Enumeration bench (DeepSeek). |
| S3 | merged | Wasmtime 50.0.2 component host works. See `docs/spike-wasm.md`. GPT hit its limit; ran on Sonnet 5.5. Proposed plan changes there (ship host without Cranelift, 0.86 MB; precompiled `.cwasm` only from our own compile step). |
| S4 | todo | Shell threading. Needs S1 merged first: it draws into S1's window. |

## Next batch
Start S0b, S1, S2 and S3 now, in parallel, each in its own worktree. They touch different crates:
- S0b: `res/`, a root `build.rs`, root `Cargo.toml` build-dependency.
- S1: `crates/tsubame-d2d`, root `src/`.
- S2: `crates/tsubame-bench`.
- S3: `crates/tsubame-plughost` plus a new guest crate. Keep the guest out of the workspace `members` (it builds for a wasm target).

S0b and S1 both edit the root `Cargo.toml`: merge S0b first, then merge S1 and resolve the small conflict yourself.

Start S4 when S1 is merged.

## Open questions for the user
- Drop the small-cut icon and ship one SVG? Asked, not answered. Until answered, S0b uses both.
