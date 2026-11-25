# Wasm plugins (P5.P1): host, interface, manifest, protocol

Tier 2 of `docs/PLAN.md` section 4: a plugin is a WebAssembly component that
runs in `tsubame-plughost.exe`, a separate process, never in the app. This page
is the reference for the four contracts between the parts: the WIT interface,
the manifest, the capability rules and the protocol on the pipe.

Code: `crates/tsubame-plughost`. Tests: `cargo test -p tsubame-plughost`
(74 unit tests, 29 end-to-end tests that drive the real host exe, plus the
security PoCs in `tests/security_poc.rs`).

**What exists and what does not.** The host exe, the install step, the
manifest parser, the protocol and the supervisor client are done and tested.
The app does not load wasm plugins yet: nothing in `src/` calls the client, and
there is no install UI. The guest SDK (`plugins/tsubame-plugin-sdk`), two sample plugins and the
author guide (`docs/plugin-authoring.md`) are P5.P2 and are done;
`plugins/sample-column` is the bare-WIT guest, without the SDK.

## The pieces

| Piece | Where | In which binary |
|---|---|---|
| WIT interface | `wit/tsubame-plugin.wit` | bound by the host; guests build against it |
| Manifest (`plugin.toml`) | `src/manifest.rs` | app, install step |
| Capabilities and path rules | `src/caps.rs` | app, host |
| Protocol | `src/proto.rs` | app, host |
| Supervisor client (`Plugin`) | `src/client.rs` | app |
| Host containment: `JobLauncher` (Windows) | `src/launcher.rs` | app |
| Entry points as `tsubame-ext-api` providers | `src/adapters.rs`, `src/cache.rs` | app |
| Host: wasmtime, limits, imports, serve loop | `src/host.rs`, `src/main.rs` | `tsubame-plughost.exe` |
| Install record (write once, verify before each start) | `src/install_record.rs` | app, install step |
| Install step and the compiler (`compile`, `install`) | `crates/tsubame-plugc` (`src/lib.rs`, `src/main.rs`) | `tsubame-plugc.exe` |
| Bench client and mock hosts | `src/bin/plughost-bench.rs` | not shipped |

Two packages and one Cargo feature keep the three binaries apart:

| Build | Package and features | Contains |
|---|---|---|
| The app's dependency | `tsubame-plughost`, `default-features = false` | manifest, protocol, client, adapters. No wasmtime: `serde`, `toml` and `tsubame-ext-api` only |
| `tsubame-plughost.exe` | `tsubame-plughost`, default (`host`) | wasmtime runtime, no compiler |
| `tsubame-plugc.exe` | `tsubame-plugc` | the host crate's manifest and record code plus wasmtime with Cranelift |

`tsubame-ext-api` is unchanged: no dependencies, no Win32.

There is no `compiler` feature on the host package: Cranelift is linked by
`tsubame-plugc` only, so nothing you can switch on in `tsubame-plughost` puts a
compiler into the host exe (security review PH-20). Build the install step with
`cargo build --release -p tsubame-plugc`, the host with `cargo build --release
-p tsubame-plughost`.

**Exe size.** The runtime-only host is **about 1.0 MiB** (1.03 MiB,
1,075,200 B, measured on `cargo build --release -p tsubame-plughost`). The 9 MB
that earlier notes (and the spike) reported was a different file at the same
path: the host's own tests compile their guests with `tsubame_plugc::compile`
(a dev-dependency), so a build of the test targets (`cargo test`,
`--all-targets`, `--workspace`) links Cranelift into the host exe it leaves in
`target/<profile>/`. A release must never ship that one: `tools/release.ps1`
stops when `tsubame-plughost.exe` is over 1.5 MiB (1,572,864 B), and the test
`release_exe_is_small` (1,250,000 B) warns earlier. Build the host with `-p
tsubame-plughost` alone before packaging.

## Lifecycle

1. **Install.** `tsubame-plugc <plugin folder> <cache folder> [<data folder>]`
   checks `plugin.toml`, compiles the `.wasm` it names and writes
   `<cache folder>/<id>/<version>/component.cwasm`, then the install record
   `install.toml` next to it. The record holds the SHA-256 of `plugin.toml`, of
   the `.wasm` and of the `.cwasm`, the wasmtime version and the capabilities
   as resolved at install. It is written once (`create_new`, then a hard link
   to the final name, which fails if one exists): a version that is installed
   can not be installed again over itself; delete its folder first. The app
   shows the user `Capabilities::summary()` before it enables the plugin.
   A grant that reaches into the plugin's package or the cache folder is
   refused here (PH-02).
2. **Verify.** Before each start, `install_record::verify` reads the record,
   hashes `plugin.toml` and the `.wasm` again and refuses the load if either
   changed ("... changed since it was installed; install the plugin again to
   approve the change"). The capabilities come from the record, not from the
   file on disk, so editing `plugin.toml` after install grants nothing.
   `Load::new(&verified)` builds the `Load`.
3. **Nothing runs.** `Plugin::new` starts no process and no thread. The
   adapters answer `accepts` from the manifest. A session that never uses a
   wasm plugin pays nothing.
4. **First call.** The client starts one host process for the plugin (stdin and
   stdout piped, no console window), waits for the host's hello, and sends
   `Load`: the `.cwasm` path, its hash, the capabilities, the declared entry
   points and the protected folders. The host checks all of it, instantiates the component, calls
   `meta.describe` and answers with what the component says it is.
5. **Calls.** One request, one reply, one at a time per plugin. Callers are
   worker threads; the client serializes them.
6. **Stop.** `shutdown`, `stop_if_idle(duration)` or dropping the `Plugin`
   sends `Shutdown`, closes the host's stdin and waits up to 500 ms for the
   process to exit before killing it. The next call starts a new host. There
   is no idle timer: the app calls `stop_if_idle` from events it already has.

One host process per plugin, so a hang or a crash takes down that plugin only.

### Deadlines, kill on hang, faults

| Layer | When | What happens | Host process |
|---|---|---|---|
| Host watchdog (epoch interruption) | the guest runs past `timeout_ms` | the guest traps at its next loop or call; the instance is dropped and built fresh for the next call; reply `Timeout` | stays |
| Client deadline | no reply `timeout_ms` + 250 ms after the request | the process is killed and reaped; the call returns `CallError::Timeout` | killed, restarted by the next call |
| Start deadline | no hello or no `Load` reply within 5 s | killed; `CallError::Spawn` or `CallError::Load` | killed |

The second layer exists because an epoch can not interrupt a guest that is
blocked inside a host import (a read from a dead network share).

Every failure is a returned `CallError`. Nothing in the client panics or blocks
past its deadline, so a caller is never taken down by a plugin.

| Outcome | Counts as a fault | Host |
|---|---|---|
| `Timeout`, `Trap`, `Memory`, `BadResult` reported by the host | yes | stays |
| No reply, process exit (`Crashed`), protocol error | yes | killed |
| Host did not start or refused the load | yes | killed |
| `Plugin(text)`: the plugin's own `err` result | no | stays |
| `Unsupported`: no such entry point | no | stays |

Three faults in a row (`Options::max_faults`) mark the plugin **faulted**: its
host is stopped and every call returns `CallError::Faulted` at once, without
starting a process, until `Plugin::reset()` (the user switches it back on). A
successful call clears the count.

The adapters turn an error into an empty result: a column shows empty cells
(and does not cache them), a previewer returns the error text.

### Bounds

| What | Cap | Where |
|---|---|---|
| Paths per call | 2,000, and 1 MiB of path text (one longer path travels alone) | client splits (`proto::chunks`), host refuses more |
| Frame | 16 MiB | both sides, checked before allocating |
| Entries or hits per page | 2,000 | host |
| Pages per listing or search | 5,000 | client |
| Bytes per `read-file` / `vfs.read` call | 4 MiB | host |
| Bytes the host copies out of the guest per call | 32 MiB (wasmtime hostcall fuel) | host |
| Guest memory | `max_memory_mb`, 1 to 512, default 64: one budget for all linear memories plus all table elements (8 bytes each). At most 32 instances, 16 tables, 8 memories and 65,536 elements per table | host (`host::Limiter`) |
| Call time | `timeout_ms`, 10 to 30,000, default 2,000 | host, then client |
| Column value cache | 8,192 values and 2 MiB per column | `cache::CellCache`, two generations, eviction tests |
| Log lines per call | 64 lines of 1 KiB | host |
| `.wasm` / `.cwasm` size | 64 MiB / 256 MiB | install step / host |

## The manifest: `plugin.toml`

```toml
[plugin]
id = "com.example.exif"          # 3-64 chars, a-z 0-9 -, at least two dot-separated parts
name = "EXIF tools"              # 1-80 chars
version = "1.2.0"                # N.N.N or N.N.N-pre
api = "0.1"                      # the WIT package version the component targets
component = "exif.wasm"          # a file name in the package, must end in .wasm
description = "Camera columns."  # optional, up to 400 chars

[capabilities]                   # optional; leaving it out grants nothing
network = false
run_processes = false
max_memory_mb = 64               # 1-512
timeout_ms = 2000                # 10-30000
paths = [
    { path = "$input",  access = "read" },
    { path = "$data",   access = "read-write" },
    { path = "$plugin", access = "read" },
    { path = "D:/Photos/Library", access = "read" },
]

[[column]]
id = "camera"                    # 1-64 chars, a-z 0-9 . _ -
label = "Camera"
align = "left"                   # or "right"; default left
width = 140                      # DIPs, 20-2000; default 120

[[previewer]]
id = "meta"
title = "EXIF"
extensions = ["jpg", "jpeg"]     # lower case, no dot, at least one

[[vfs]]
id = "zip"
title = "Zip archives"
extensions = ["zip"]

[[search]]
id = "by-camera"
title = "By camera"

[[command]]
name = "strip"
title = "Strip EXIF"
keys = ["Ctrl+Alt+E"]            # optional default chords, at most 8
```

Entry points register as `<plugin id>.<entry id>` (`Manifest::qualified`), so
two plugins can not clash. `adapters::contribute` adds the columns, previewers,
vfs providers and search providers to an extension's `Contributions`; commands
are left to the app, which owns `Host::Run` and the command numbers.

### Validation

Parsing is strict. Any of these rejects the whole manifest, with a message that
names the field:

- An unknown key anywhere, including an unknown capability
  (`[capabilities] filesystem = true`) or an unknown table (`[[panel]]`).
- `max_memory_mb` or `timeout_ms` outside its range. Values are not clamped:
  what the file says is what the user is shown and what the host enforces.
- A path grant that is not `$plugin`, `$data`, `$input`, `*` or a valid absolute
  path (see the path rules below); an unknown `$name`; an `access` other than
  `read` or `read-write`; `$plugin` with `read-write`; the same location twice;
  more than 32 grants.
- A plugin id that is malformed or starts with `tsubame.`, `core.` or `pack.`.
- `component` with a folder in it, or not ending in `.wasm`. A `.cwasm` is
  refused by name: a package never ships compiled code.
- `api` other than the version this build speaks.
- No entry point at all, more than 32 of one kind, a duplicate id within a
  kind, an empty or malformed id, label or extension list.
- A file over 64 KiB.

### Path grants

| `path` | Covers | Notes |
|---|---|---|
| `$input` | the paths handed to the current call; what is under them only where the call works on a tree | changes with every call. Column and previewer: the path itself (a folder can be looked at and listed, nothing in it opened). Vfs: the path and the file it goes through (`C:\a.zip` for `C:\a.zip\inner\x`). Search: the root and everything under it. Command: the selection and everything under it, plus the current folder itself |
| `$data` | the plugin's own data folder | the app creates it; resolved in `Manifest::capabilities`. The plugin sees the symbolic scope `data` and writes `$data\name` |
| `$plugin` | the folder the plugin is installed in | read only. The plugin sees the scope `plugin` and reads `$plugin\name` |
| an absolute folder | that folder and everything under it | a folder on another computer (`\\server\share`) needs `network = true`; the prompt names the server |
| `*` | every path on this computer | shown to the user as "any file on this computer". It never covers a `\\server\share` path or a mapped network drive |

`$data` and `$plugin` follow the same rule: if the app puts them on a network
share, the plugin needs `network = true`. A path in `$input` may be on a share;
the user picked it.

`Manifest::capabilities(plugin_dir, data_dir, protected)` turns the manifest
into the `Capabilities` that the install record keeps. `Capabilities::summary()`
gives one line per grant for the install prompt ("Read the files and folders
you use it on: a column or previewer sees only the item itself, a command the
items you select and what is in them, a search the folder it searches"). A
read-write `*` grant reads "Read and change any file on this computer: full
control of your files".

### Protected folders

No grant reaches Tsubame's own folders. `Load` names them (`protected`): the
app's settings, the plugin's package and the cache with the compiled plugins.
The host adds the folder of the `.cwasm` itself.

| Folder | Read | Write |
|---|---|---|
| the plugin's package, the app's settings | yes, if granted | never |
| the cache (compiled plugins, install records) | never | never |

The host checks this twice per open: on the normalized path, and on the final
path of the handle, so a junction, symlink or hard link that leads into a
protected folder is refused too. A `Load` whose grants overlap a protected
folder (read-write anywhere in one, any access in the cache) is refused
outright; `*` is allowed and refused per path.

## Capability enforcement

The host enforces; the guest can not opt out and the client only passes the
values on. The host validates the capabilities again when they arrive.

| Capability | How it is enforced |
|---|---|
| Memory | A wasmtime resource limiter (`StoreLimits` for tables, instance and memory counts, plus a cap on the **sum** of all linear memories). A refused `memory.grow` returns -1 to the guest; if the call then fails the reply is `Memory`. |
| Time | Epoch interruption. Every entry into the guest, instantiation included, runs with an epoch deadline of one tick. A watchdog thread sleeps on a condition variable while no call runs (an idle host has no wake-ups) and ticks the epoch when the deadline passes. |
| Files | The guest has no WASI. Imports the host does not define are linked as traps. The only way to a file is the `host` interface, and each call checks the path, then opens it from the grant and checks the handle (below). |
| Network | Denied. There is no import that reaches the network. `network = true` is parsed, shown to the user and passed to the host, and changes nothing yet. |
| Processes | Denied. There is no import that starts a process. `run_processes = true` is the same. |
| Host memory | wasmtime's hostcall fuel caps what the host copies out of the guest (strings and lists) at 32 MiB per call, so a guest can not make the host allocate without bound. |

When a network or process import is added, it must check the flag in `Ctx`
first; until then both flags are reserved.

### The path check

Every path a guest passes to `stat`, `read-file`, `read-dir` or `write-file`
goes through two checks.

**1. Lexical** (`caps::normalize`, `caps::PathPolicy`; pure string work):

- The path must be absolute: `X:\...` or `\\server\share\...`. `/` is accepted
  as a separator. `\\?\X:\` and `\\?\UNC\` lose their prefix.
- Refused outright: `.` and `..` components, a `:` anywhere but after the drive
  letter (stream names, drive-relative paths), `< > " | ? *` and control
  characters, a name ending in a dot or a space, device names (`NUL`, `COM1`,
  `COM¹`, `NUL.txt`), a server name with `@` (the WebDAV redirector), and
  every other `\\?\` or `\\.\` path.
- A `\\server\share` path matches only a grant for that share (with
  `network = true`) or a path the call was given. `*` does not cover it.
- A grant matches when the path equals its folder or continues it after a
  separator (`C:\Data` does not cover `C:\Database`). Comparison folds ASCII
  case only. Windows folds more, so a name that differs in the case of a
  non-ASCII letter is refused; it is never wrongly allowed.
- `write-file` needs a `read-write` grant.

**2. Opened from the grant** (`host/fsopen.rs`, Windows). The host opens the
grant's folder (for `*`, the drive's root), then each further component
relative to the handle of the one before (`NtCreateFile` with `RootDirectory`
and `FILE_OPEN_REPARSE_POINT`). So the name is looked up once, and the I/O
(`stat`, `read-file`, `read-dir`, `write-file`) runs on the handle that was
checked; nothing opens the path a second time.

- A symlink, junction or mount point below the grant's folder is refused, even
  one that points back inside. A dangling link is refused too, so a write never
  creates its target. Other reparse points (cloud files, dedup) are opened
  normally, and the second handle must be the same file as the first.
- The final path of the handle (`GetFinalPathNameByHandleW`) must lie inside
  the final path of the grant's folder, and must not be on another computer
  unless the grant allows it.
- Under a folder grant (and `$input` folders), a file with more than one hard
  link is refused for every call except `read-dir`; under `*` it is refused
  for every write. A path the call was given exactly is opened as the user
  named it, links followed, and its final path is then checked against the
  protected folders.
- A new file is created relative to its folder's handle, and deleted again if
  the handle check fails. `write-file` truncates only after every check.
- `read-dir` lists through the folder's handle; a link to a folder is listed as
  a file.

Tested with real junctions, symlinks (when the OS allows creating them), hard
links and a swap race (`tests/host.rs`, the `ph04`/`ph05`/`ph06` tests).

Known limits, for the security review (P5.P3):

- The grant's folder itself (and, for `*`, the drive root) is opened by its
  path on each call, links followed. It is the folder the user granted, so it is
  trusted; a link the user placed there is honoured.
- Refusing every link and hard link below a grant is strict: a plugin can not
  follow a link the user made inside a granted folder, and can not read a
  hard-linked file there (it can read it by its exact path if the user hands it
  over).
- Each component costs one open, so a deep path under a folder grant costs more
  than the old resolve (about 0.3 ms per read in the tests).
- If the app dies while a guest is blocked inside a host import, the host
  stays until that import returns (it exits as soon as it next reads its
  stdin). A job object owned by the app would close this; the client is
  `std` only and does not create one.
- `Component::deserialize` trusts its input. The host only passes it the bytes
  whose SHA-256 matches the hash in the install record, and it hashes the same
  buffer it deserializes, so the file can not be swapped in between. No plugin
  can reach the cache, and the record is written once. The record is not
  signed: it is only as strong as the cache folder's ACL, which must leave it
  writable by the app (the user) alone.
- On an exact path given to a write (`OPEN_ALWAYS`), the final-path check runs
  after the open, so a link that leads into a protected folder can leave an
  empty new file there before the write is refused. Nothing is written into
  it.
- `verify` hashes the `.wasm` (up to 64 MiB) on every start.
- `tsubame-plugc` has no time or memory bound of its own; the app must run it
  in a job (PH-18).

## The WIT interface

`wit/tsubame-plugin.wit`, package `tsubame:plugin@0.1.0`. Paths are absolute
Windows paths; times are FILETIME values.

### Exports (what a plugin implements)

| Interface | Function | Mirrors (`tsubame-ext-api`) |
|---|---|---|
| `meta` | `describe() -> info` (id, version, entry points) | required for every plugin |
| `column` | `values(column, paths: list<string>) -> list<cell>` | `ColumnProvider::values`, `Cell` |
| `previewer` | `preview(id, path, limits) -> result<list<node>, string>` | `Previewer::preview`, `Doc`, `Node` |
| `vfs` | `list-dir(id, path, cursor) -> result<page, string>`, `read(id, path, offset, max-len) -> result<list<u8>, string>` | `VfsProvider::list`, `read` |
| `search` | `find(id, query, cursor) -> result<page, string>` | `SearchProvider::search`, `Hit` |
| `command` | `run(name, context) -> result<option<string>, string>` | a command handler |

Notes:

- `column.values` takes plain `list<string>` (the S3 decision: no packed
  encoding in the public signature) and returns one typed `cell` per path
  (`empty`, `text`, `int`, `bytes`, `time`), so the app can sort and format
  numbers and dates. A different count is a `BadResult`.
- Streaming calls (`list`, `search`) are pages of at most 2,000 items with an
  opaque `next` cursor. Each page is its own call with its own deadline; the
  client loops and feeds the caller's sink, which can stop early.
- A command over more than 2,000 selected paths arrives as `parts` calls with
  `part` counting from 0.
- An image must have exactly `width * height * 4` bytes and fit the caller's
  `max-px`, or the reply is `BadResult`.
- Side panels, file-type rules and themes have no wasm form yet. The last two
  are plain data and belong in a TOML pack.

A plugin exports `meta` plus the interfaces of the entry points its manifest
declares, nothing else. The worlds `column-plugin`, `previewer-plugin`,
`vfs-plugin`, `search-plugin` and `command-plugin` are ready made for a plugin
of one kind; a plugin with several writes its own world from the same
interfaces. The host binds against the world `plugin` (everything) but looks up
each export interface on its own, and only those the manifest declares. A
declared kind the component does not export fails the load.

At load the host checks that `describe()` returns the manifest's id and
version and lists every declared entry point.

### Imports (what a plugin may call): `host`

| Function | Needs |
|---|---|
| `get-capabilities() -> capabilities` | nothing |
| `log(level, message)` | nothing; goes to the host's stderr, which the client drops unless `ProcessLauncher::inherit_stderr` is set |
| `stat(path) -> result<file-info, fs-error>` | read grant |
| `read-file(path, offset, max-len) -> result<list<u8>, fs-error>` | read grant |
| `read-dir(path, skip, max) -> result<list<entry>, fs-error>` | read grant |
| `write-file(path, data, append) -> result<_, fs-error>` | read-write grant; the folder must exist |

`fs-error` is `denied` (no grant covers it), `invalid-path` (refused by the
lexical check), `not-found`, `too-large` or `io`.

There is no WASI. A guest built for `wasm32-wasip2` with `std` works as long as
it does not call a WASI import at run time; one that does (printing, the clock,
`HashMap`'s random seed) traps. P5.P2 decision: no shims; the SDK
documents what not to call (`docs/plugin-authoring.md`).

## The protocol

Over the host's stdin (requests) and stdout (replies). Little endian.

```
frame    := len:u32  id:u32  kind:u8  body        len counts id + kind + body, 5 ..= 16 MiB
string   := len:u32  utf8
list<T>  := count:u32  T...
option<T>:= 0 | 1 T
```

The client picks `id`; the reply carries the same one. A reply with another id
is a protocol error and the host is killed, so a late answer can never be
taken for the next call's.

| Kind | Name | Body |
|---|---|---|
| 1 | `Load` | plugin id, version, component path, sha256 (32 bytes), capabilities, entry points, `list<string>` protected folders (at most 64) |
| 2 | `Column` | column id, `list<string>` paths |
| 3 | `Preview` | previewer id, path, max-bytes u64, max-px u32 |
| 4 | `VfsList` | vfs id, path, `option<string>` cursor |
| 5 | `VfsRead` | vfs id, path, offset u64, max-len u32 |
| 6 | `Search` | provider id, text, root, limit u32, `option<string>` cursor |
| 7 | `Command` | name, folder, `list<string>` selection, part u32, parts u32 |
| 8 | `Shutdown` | empty |
| 0x80 | `Hello` (host, once, id 0) | magic `TSPH`, protocol version u16, host version |
| 0x81 | `Ok` | depends on the request: info, cells, nodes, entries + cursor, bytes, hits + cursor, optional message |
| 0x82 | `Err` | error kind u8, message (cut to 1 KiB) |

Error kinds: 1 `BadRequest`, 2 `NotLoaded`, 3 `Load`, 4 `Plugin`, 5 `Trap`,
6 `Timeout`, 7 `Memory`, 8 `BadResult`, 9 `Unsupported`.

Capabilities on the wire: `network:u8 run_processes:u8 max_memory_mb:u32
timeout_ms:u32 list<grant>`, a grant being a scope tag (0 prefix + string,
1 input, 2 any) and an access byte (0 read, 1 read-write). `$plugin` and
`$data` are already resolved to folders on this wire (a prefix grant); `Load`
also carries the two folders (`plugin_dir`, `data_dir`, empty when there is no
such grant) so the host can report those grants to the guest as the scopes
`plugin` and `data` and expand a leading `$plugin\` or `$data\` in the paths
a guest passes in. The guest never sees the real folders (PH-21).

Rules:

- The host says `Hello` before it reads anything. A client that sees another
  protocol version refuses the host. The version is 3 (2 added `protected` to
  `Load`, 3 added `plugin_dir` and `data_dir`).
- `Load` comes first and once. A call before it gets `NotLoaded`, a second
  `Load` gets `BadRequest`.
- Decoding never trusts a length. Every read is bounds checked, a list count is
  checked against the bytes left before anything is allocated, and a frame
  outside 5 bytes to 16 MiB ends the session.
- A request that does not decode gets `BadRequest`; the client then replaces
  the host, since the two sides are out of step.
- `PROTOCOL VERSION` (`proto::VERSION`, now 1) changes with any change to this
  table. App and host ship together, so there is no negotiation.

## Containment: `JobLauncher` (PH-07, tier 1)

The app starts hosts with `launcher::JobLauncher` (Windows only). It is the
only launcher meant for shipped code. `client::ProcessLauncher` applies none of
the below and is **test-only**: the host would get the app's whole environment,
working folder and inheritable handles, and survive the app.

| Measure | What it gives |
|---|---|
| Job object per host, `KILL_ON_JOB_CLOSE` | the host dies with the app, even when the app is killed; a host stuck in a slow share read can not outlive it |
| `ActiveProcessLimit = 1` | the host can not start a child process |
| Process memory limit = `max_memory_mb` + 192 MiB (`HOST_OVERHEAD_MB`) | backstop for what the wasm limiter does not count (PH-08); the plugin's own cap trips first |
| UI restrictions | no desktop switching, display settings, `ExitWindows`, global atoms, system parameters, clipboard, or USER handles of other processes |
| Assigned to the job before `launch` returns | before the client sends `Load`, so before any plugin code ran; if the assignment fails the host is killed and the start fails |
| Cleared environment (only `SystemRoot`), fixed working folder, absolute exe path | no tokens in variables, no dependence on the app's current folder or `PATH` |
| Refuses to start when the app is elevated | `io::ErrorKind::PermissionDenied`, no process is created. A host must never run with an administrator token. The check is `launcher::is_elevated` (fails closed); tests inject their own with `with_elevation_check` |

```rust
let launcher = JobLauncher::new(host_exe_abs, work_dir_abs, caps.max_memory_mb)?;
```

Not covered yet (tier 2/3, `docs/security/plughost-review.md` card L): restricted
token or low integrity, AppContainer, an explicit inheritable-handle list (the
standard library spawns with `bInheritHandles = TRUE`), process mitigation
policies.

## Using it from the app (not wired yet)

```rust
// at install, after the user approved Capabilities::summary():
//   tsubame-plugc <plugin dir> <cache dir> [<data dir>]   (in a job)
// before each start:
let v = install_record::verify(&plugin_dir, &cache_dir, id, version, &[settings_dir])?;
let load = Load::new(&v);   // the hash, the caps and the protected folders come from the record
let launcher = JobLauncher::new(host_exe, work_dir, v.caps.max_memory_mb)?;   // not ProcessLauncher: that is test-only
let plugin = Arc::new(Plugin::new(load, launcher, Options::default()));
adapters::contribute(&v.manifest, &plugin, contributions);   // inside Extension::contribute
```

## Measurements

Release build with the root profile, wasmtime 50.0.2.

| | S3 spike | Now |
|---|---|---|
| `tsubame-plughost.exe`, runtime only | 896,512 B (0.86 MB) | **1,070,592 B (1.02 MB)** |

The 155 KB are the `host` imports, the limiter and watchdog, the bindings for
five export interfaces, the typed protocol and SHA-256. The test
`release_exe_is_small` fails over 1,250,000 B; it measures
`target/release/tsubame-plughost.exe` (or `TSUBAME_PLUGHOST_RELEASE_EXE`) and
says so when there is no release build to measure.

Timings with `plughost-bench run` and the sample plugin (63 KB `.wasm`, 232 KB
`.cwasm`), on a development machine under load, **not** the benchmark machine,
so they are not comparable with `docs/spike-wasm.md`:

| | median | p95 |
|---|---|---|
| Load (read, SHA-256, deserialize, link, instantiate, `describe`) | 1.4 ms | 2.0 ms |
| Column call, 100 paths, over the pipe | 0.08 ms | 0.12 ms |
| Column call, 2,000 paths | 0.77 ms | 1.1 ms |
| Clean shutdown | 4.4 ms | 5.5 ms |

Process start dominated start-up there (about 23 ms for any exe on that
machine, the host's `--version` included). Start-up on the benchmark machine
has not been measured again.

## Tests

| What | Where |
|---|---|
| Manifest: a full file, defaults deny, each rejection | `manifest.rs` (12) |
| Path rules: normalize, reject list, prefix / input / any grants, flat and recursive inputs, protected folders | `caps.rs` (14) |
| Install record: round trip, wasmtime version matches `Cargo.toml`, refusals | `install_record.rs` (3) |
| Protocol: every message round trip, bad lengths, malformed bodies, chunking | `proto.rs` (7) |
| Client against an in-memory host: lazy start, chunks, kill on hang, late reply, crash, fault limit, bad hello, wrong id, paging, threads | `client.rs` (15) |
| Cache caps and eviction | `cache.rs` (4) |
| SHA-256 vectors | `sha256.rs` (2) |
| `JobLauncher`: elevated app refused, relative paths refused, job limits applied and queried back, host in the job when `launch` returns, owner killed (`TerminateProcess`) takes the host with it, a host that tries to start a child fails (and succeeds under `ProcessLauncher`), cleared environment and fixed folder, the real host under the job | `launcher.rs` (5), `tests/launcher.rs` (4), `tests/host.rs` |
| Real host exe: round trip, 5,001 paths in 3 calls, epoch interrupt, kill of a hung process, memory cap, traps, hash and identity checks, partial exports, file grants, junction, protected folders, flat column inputs, a changed `.cwasm` refused, every entry point through the adapters, exe size | `tests/host.rs` (29) |
| Samples built, installed through the record and run | `tests/samples.rs` (4) |
| Security review PoCs, fixed ones flipped into regression tests | `tests/security_poc.rs` (21, 4 still ignored) |

The end-to-end guest is WebAssembly text in the test file, turned into a
component by `wit-component` against the real WIT file and compiled with the
host's engine settings, as the install step does. Cranelift is a dev-dependency
only.
