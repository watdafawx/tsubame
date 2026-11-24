# tsubame-plugin-sdk

Guest SDK for Tsubame wasm plugins: bindings for `tsubame:plugin@0.1.0`
(`crates/tsubame-plughost/wit`), the `Column` and `Previewer` traits, the `Doc`
row builder, wrappers for the host's file functions and the `plugin!` macro.

Not part of the app workspace: it lives in the guest workspace `plugins/`
and never reaches the shipping exe. It also compiles for the host, so
`cargo test` in `plugins/` runs its tests.

Start at `docs/plugin-authoring.md`. API docs: `cargo doc --open -p tsubame-plugin-sdk` in `plugins/`.
