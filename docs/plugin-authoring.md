# Writing a Tsubame wasm plugin

A plugin is a WebAssembly component that Tsubame runs in a separate process
(`tsubame-plughost.exe`), so a bug or a hang in it can not take the app down.
This page is the short path from nothing to a working column or previewer.
The reference for the interface, the manifest and the protocol is
[`wasm-plugins.md`](wasm-plugins.md).

**Status.** The SDK, the samples and the host work and are tested. The app does
not load wasm plugins yet (no install UI), so today you can build and test a
plugin with the host's own tests, not use it in the file list. Plain data
(themes, file-type rules, simple columns from file metadata) needs no code: use
a TOML pack (`docs/extensions.md`).

## What you can write

| Kind | You implement | SDK support |
|---|---|---|
| Column | `Column::value(path) -> Cell` (or `values` for a batch) | yes |
| Previewer | `Previewer::preview(path, limits) -> Result<Vec<Node>, String>` | yes |
| vfs, search, command | the WIT interfaces `vfs`, `search`, `command` | not yet: use the raw bindings (`tsubame_plugin_sdk::bindings`) |

A plugin never draws. A previewer returns rows (heading, text, pair, mono,
image, button) and Tsubame draws them. A column returns cells (`empty`, `text`,
`int`, `bytes`, `time`) and Tsubame formats and sorts them.

## Setup

1. `rustup target add wasm32-wasip2`
2. Make a crate with `crate-type = ["cdylib"]` and a dependency on the SDK.
   Inside this repo that is `plugins/<name>` as a member of the guest workspace
   (`plugins/Cargo.toml`), with `tsubame-plugin-sdk.workspace = true`. The
   guest workspace is separate from the app's, so nothing in it reaches the
   shipping exe.
3. Write `src/lib.rs`:

```rust
use tsubame_plugin_sdk::{plugin, Cell, Column};

struct NameLength;

impl Column for NameLength {
    const ID: &'static str = "name-length";           // = [[column]] id in plugin.toml
    fn value(path: &str) -> Cell {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
        Cell::Int(name.chars().count() as i64)
    }
}

plugin! {
    id: "com.example.name-length",                    // = [plugin] id
    version: env!("CARGO_PKG_VERSION"),               // = [plugin] version
    columns: [NameLength],
}
```

A previewer is the same with `impl Previewer` and `previewers: [MyPreview],`
(`columns` comes first when both are given; every line ends with a comma).
Build rows with `Doc`: `Doc::new().heading("x").pair("Rows", "3").finish()`.
Read files with `tsubame_plugin_sdk::host::{stat, read_file, read_prefix}`.

Samples to copy: `plugins/sample-file-kind` (two columns, no capabilities) and
`plugins/sample-csv-summary` (previewer that reads the file it is shown).

## The manifest: `plugin.toml`

Beside the component, one file. The smallest useful one:

```toml
[plugin]
id = "com.example.name-length"
name = "Name length"
version = "0.1.0"
api = "0.1"
component = "name_length.wasm"     # the cdylib's file name, next to this file

[[column]]
id = "name-length"
label = "Name length"
```

Every `[[column]]`, `[[previewer]]` table needs a matching type in `plugin!`
with the same id, and the plugin's `id` and `version` must equal the manifest's.
The host checks all of that at load and refuses a plugin that disagrees. Ids
that start with `tsubame.`, `core.` or `pack.` are reserved. Full field list
and every validation rule: `wasm-plugins.md`, "The manifest".

### Capabilities: ask for the least

Leaving out `[capabilities]` grants nothing: no files, no network, no
processes, 64 MB and 2000 ms per call. The user is shown what you ask for
before the plugin is enabled.

| Key | Meaning |
|---|---|
| `paths = [{ path = "$input", access = "read" }]` | read the paths of the current call. A column or previewer gets the item itself: a file can be read, a folder looked at and listed, but nothing inside it opened. A command gets the selection and what is under it, plus the current folder itself; a search its root and what is under it. Almost every plugin that touches files needs only this |
| `{ path = "$data", access = "read-write" }` | the plugin's own data folder |
| `{ path = "$plugin", access = "read" }` | the folder the plugin is installed in (bundled data) |
| `{ path = "D:/Photos", access = "read" }` | one folder and everything under it |
| `{ path = "*", access = "read" }` | every file on the computer. Shown to the user as such; avoid. With `read-write` it reads "full control of your files" |
| `max_memory_mb` (1-512), `timeout_ms` (10-30000) | lower than the defaults when you can; the host enforces them |
| `network`, `run_processes` | parsed and shown, but there is no network or process import: they do nothing |

## Build and install

```
pwsh plugins\build-plugin.ps1 plugins\sample-csv-summary
```

The script builds the component (`cargo build --release --target wasm32-wasip2`),
copies it next to `plugin.toml`, builds `tsubame-plugc` on first use, and runs
it. `tsubame-plugc <plugin folder> <cache folder> [<data folder>]` checks the
manifest, compiles the component for this machine and writes
`<cache>/<id>/<version>/component.cwasm` and the install record
`install.toml` next to it: the SHA-256 of `plugin.toml`, the `.wasm` and the
`.cwasm`, and the capabilities the user approves. A package never ships
compiled code, only the `.wasm`. The samples compile to 0.3-0.4 MB.

The record is written once. Before each start the app checks `plugin.toml` and
the `.wasm` against it; if either changed, the plugin does not load until it is
installed again, so the user sees and approves the change. To install a
rebuilt plugin, delete `<cache>/<id>/<version>` first (`build-plugin.ps1
-Force` does that) or raise the version. The same goes for a Tsubame update
that changes the wasmtime version: the record names it and an old `.cwasm`
is refused.

No grant reaches the plugin's own package, Tsubame's settings or the cache:
the package can be read (with `$plugin`) but never written, the cache neither.
A grant that would reach into them is refused at install.

The app's install and enable flow is not written yet; see `wasm-plugins.md`,
"Lifecycle", for what it will do.

## Limits that shape the code

- **No WASI.** The guest links against `wasm32-wasip2` but the host offers no
  WASI imports; one the guest calls traps. Do not use `println!` (use
  `host::log`), clocks, `std::env`, threads, or `HashMap` with its default
  hasher (it seeds from a random source; use `BTreeMap`).
- **Batches.** A column gets at most 2,000 paths per call and must return
  exactly one cell per path, in order. Do not do per-file work that costs a
  second; the call has a deadline.
- **Deadline and memory.** Over `timeout_ms` the call is interrupted and the
  instance is thrown away. Over `max_memory_mb` the allocation fails. Read
  files in chunks (`host::read_prefix(path, cap)` caps what it reads) rather
  than all at once.
- **No state.** The instance can be rebuilt at any time. Keep caches small and
  never depend on them.
- **Frames.** A previewer image must be exactly `width * height * 4` bytes of
  premultiplied BGRA and fit `limits.max_px`, or the reply is rejected.
- Three failed calls in a row (traps, timeouts, bad results) switch the plugin
  off until the user re-enables it. An `Err(text)` from your own code is not a
  failure; it is shown to the user.

## Debugging

- **Unit-test the logic on the host.** The SDK compiles for Windows too, so
  `cargo test` in `plugins/` runs your plain functions. Keep parsing and
  formatting in functions that take text (see `sample-csv-summary`), and the
  host calls in a thin layer. Calling a `host::` function off wasm panics.
- **Drive the real host.** `crates/tsubame-plughost/tests/samples.rs` builds a
  sample, compiles it as the install step does, and calls it through the real
  `tsubame-plughost.exe` with the capabilities from its own manifest. Copy it
  for your plugin: `cargo test -p tsubame-plughost --test samples`.
- **Log.** `host::log(Level::Info, "...")` goes to the host's stderr (64 lines
  of 1 KiB per call). The client drops it unless `ProcessLauncher::inherit_stderr`
  is on.
- **Load errors** name the cause, for example `the manifest declares column
  `x` but the component does not list it`, `the component's hash does not
  match the one recorded at install`, or the refused manifest field. Fix the
  manifest or the `plugin!` list so both say the same thing.

## Security model in one screen

- The plugin runs in its own process, one process per plugin. A crash or hang
  kills only it; the app gets an error and shows an empty cell or the error
  text.
- The component has no ambient authority. It has no WASI, no network and no
  process import. The only door to the disk is `host::{stat, read_file,
  read_dir, write_file}`, and each call is checked against the grants twice: a
  lexical check (absolute path, no `..`, no stream or device names) and, for
  folder grants, a check of the resolved path, so a junction inside a granted
  folder does not lead out of it.
- The user sees the capability list before enabling the plugin; the host
  enforces the manifest's numbers (memory, time) and does not trust the guest
  to stop itself.
- Only a `.cwasm` that matches the hash in the install record is loaded, and
  the capabilities come from the record, not from `plugin.toml` on disk.
- Known gaps are listed in `wasm-plugins.md`, "The path check"; the security
  review is task P5.P3 in `PLAN.md`.
