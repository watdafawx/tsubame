# Spike S3: wasm plugin host (wasmtime component model)

Measured 2026-11-17 on the benchmark machine (Ryzen 9 5900X, Windows 11), release build with the root profile (opt 3, LTO, 1 CGU, strip, panic=abort), wasmtime **50.0.2**, rustc 1.99.1. The machine was running game servers during every run (CPU load 15–26 % at the start of each pass); each number below was repeated in 2 to 3 separate passes and the passes agree to a few percent unless noted.

**Verdict: PASS.** Wasmtime is fine, provided the host ships *runtime-only* (no Cranelift) and plugins are precompiled at install time. The default full host is too big (9.3 MB); the runtime-only host is 0.86 MB.

## What was built

| Piece | Path |
|---|---|
| WIT world (two exports: `list<string>` and packed `list<u8>`) | `crates/tsubame-plughost/wit/column.wit` |
| Host exe (`serve`, `bench`, `precompile`) | `crates/tsubame-plughost/src/main.rs` |
| Pipe framing + test data (shared) | `crates/tsubame-plughost/src/proto.rs` |
| Measurement client `plughost-bench` (separate bin, not part of the host exe) | `crates/tsubame-plughost/src/bin/plughost-bench.rs` |
| Guest: `<EXT upper> <path length>` per path; `wasm32-wasip2`, 62,732 B; own `[workspace]`, also listed in root `exclude` | `plugins/sample-column/` |

Host features: `wasmtime` with `default-features = false`, `["runtime", "component-model", "std"]`, plus the crate feature `compiler` (= `wasmtime/cranelift`, on by default). No cache, profiling, wat, gc, async, threads, parallel-compilation, no `wasmtime-wasi`. Unknown imports (the std/wasip2 guest may import wasi) are linked as traps. A guest that needs real I/O would need `wasmtime-wasi` (size not measured).

The host never touches the main exe: it is a separate package target, and `tsubame` (the app crate) has no dependency on it or on wasmtime.

Rebuild:
```
cd plugins/sample-column && cargo build --release --target wasm32-wasip2   # needs: rustup target add wasm32-wasip2
cargo build --release -p tsubame-plughost          # runtime-only host: add --no-default-features
# on the benchmark machine, with plugins/sample-column/sample_column.wasm copied next to the host exe:
tsubame-plughost.exe precompile plugins\sample-column\sample_column.wasm sample.cwasm
tsubame-plughost.exe bench sample.cwasm --pre                     # in-process call timing
plughost-bench.exe spawn|rtt|mem .\tsubame-plughost.exe sample.cwasm --pre
```

## (a) Release exe size

| Exe | Size |
|---|---|
| Host, full (Cranelift: compiles `.wasm` itself, `precompile`) | **9,764,864 B = 9.31 MB** |
| Host, **runtime-only** (`--no-default-features`, loads a precompiled `.cwasm` only) | **896,512 B = 0.86 MB** |
| `plughost-bench` (client, not shipped) | 294,912 B |
| Guest `sample_column.wasm` / precompiled `sample.cwasm` | 62,732 B / 202,712 B |

Cranelift is 8.4 MB of the 9.3 MB. The main `tsubame.exe` (scaffold, 242,176 B on this build) does not contain any of this, so the **main exe < 6 MB budget is unaffected**.

## (b) Cold spawn: CreateProcess to guest instantiated and ready

Client-side timer from `Command::spawn` until the host printed `ready` (includes process creation of the host exe, 40 runs per pass, warm file cache):

| Mode | median | p95 | Notes |
|---|---|---|---|
| **Precompiled `.cwasm`** (deserialize) | **5.9–6.1 ms** | 6.5–8.8 ms | main()→ready is only 0.7 ms (engine 0.1, deserialize 0.44, instantiate 0.14); the rest is Windows process creation + loader. Lite host: 5.8 ms, main→ready 0.59 ms |
| Compile from `.wasm` at startup, Cranelift opt=speed | 62.7–64.2 ms | 66.9–72.5 ms | 58 ms is compiling a 62 KB guest. Grows with guest size |
| Compile from `.wasm`, opt=none | 49.1 ms | 52.4 ms | `PLUGHOST_OPT=none`; compile still 43 ms, so little help |

Coldest single runs (first spawn after the exe was copied, so Defender/file cache): 8.2–8.8 ms precompiled, 54–78 ms compiled. One outlier of 66 ms was seen on the first spawn of the lite exe, same cause. Real cold-boot behaviour (first launch after login) was not measured.

## (c) Batch call latency

Paths are about 73 bytes each (`C:\Users\...\folder_NNN\sub_NN\file_NNNNNN.ext`). Results are verified against a host-side reference on every size before timing. Warm-up 50 calls, then 300 / 3,000 / 20,000 timed runs.

**In-process (host calling the guest, includes canonical-ABI lowering and lifting, no IPC)**, 3 passes (the table shows the middle pass, ranges across passes in brackets):

| Paths | `list<string>` median | `list<string>` p95 | packed `list<u8>` median | packed p95 |
|---|---|---|---|---|
| 10,000 | 2.45 ms (2.41–2.54) | 2.62 ms (2.45–4.23) | 1.24 ms (1.23–1.28) | 1.31 ms (1.25–2.18) |
| 100 | 23.4 µs | 23.8 µs | 12.3 µs | 12.5 µs |
| 1 | 0.5 µs | 0.5 µs | 0.4 µs | 0.5 µs |

The one pass with p95 4.2 ms / 2.2 ms at 10,000 paths coincided with load from the game servers on the machine (max 4.7 ms). The other two passes are tight (p95 within 7 % of the median).

**End to end over stdin/stdout pipes** (what the app would actually see: request bytes written, host decodes, calls the guest, encodes, response read):

| Paths | `list<string>` median | p95 | packed median | p95 |
|---|---|---|---|---|
| 10,000 (about 0.8 MB each way) | 3.5 ms | 4.2–4.3 ms | 1.75 ms | 2.3–2.4 ms |
| 100 | 0.06 ms | 0.08–0.09 ms | 0.04 ms | 0.06–0.07 ms |
| 1 | 0.01 ms | 0.03 ms | 0.01 ms | 0.03 ms |

Packed is about 2× cheaper at 10,000 (canonical ABI string lowering/lifting is about 1.2 ms of the 2.45 ms), but the absolute difference at a screenful (100 rows) is 11 µs.

**Budget judgement (PASS).** The column call is made from a worker thread, never the UI thread: the UI paints placeholders and fills cells when the reply posts a wake message. Even so, the numbers are small: a visible batch is 50–200 rows, which is **0.04–0.09 ms** round trip. Even a full 10,000-entry column fill is 1.75–4.3 ms, which would not break a 5 ms paint budget even if it were synchronous (it will not be). Caveat: the sample guest does trivial work (an uppercase and a format); a real plugin that reads file headers will be dominated by its own work, not by the host.

## (d) Memory (host process, Windows `PROCESS_MEMORY_COUNTERS_EX`)

Sampled after ready, after 2 s idle, then during 1,000 calls of 10,000 paths via `list<string>`, 1,000 via packed, and 1,000 with 1 path, then 2 s idle again.

| Host | After ready (private / working set) | After 3,000 calls and idle (private / working set) | Peak WS |
|---|---|---|---|
| Full, precompiled `.cwasm` | 1.84 MB / 5.39 MB | 4.22 MB / 7.88 MB | 9.15 MB |
| Full, compiled at startup | 3.43 MB / 10.25 MB | 7.09 MB / 13.84 MB | 13.85 MB |
| **Runtime-only, precompiled** | **1.80 MB / 5.16 MB** | 5.53 MB / 8.91 MB | 8.91 MB |

**Growth: none.** Private bytes jump on the first 10,000-path call (guest linear memory grows to fit the batch, +3.5 MB), creep +0.2 MB by call 250, then are flat to call 3,000 (identical to two decimals at calls 250, 500, 750, 1,000 and after the packed and 1-path runs). On the precompiled full host private bytes even fell from 5.57 to 4.22 MB when the encoding switched, so memory is being returned. Linear memory itself is never shrunk by wasm, so a guest's high-water batch stays resident; cap the batch size (for example 2,000 paths) to cap this.

## (e) Kill and respawn

| Host | `TerminateProcess` + wait | Kill → new host ready |
|---|---|---|
| Precompiled | 0.8–1.0 ms (p95 1.2) | **6.4 ms** (p95 6.9–10.5) |
| Compiled at startup | 1.7 ms | 65 ms |

A slow or crashed host can be killed and replaced in about 7 ms with a precompiled plugin. Epoch/fuel interruption was not tried; killing the process is cheap enough to be the only mechanism needed.

## Recommendation and plan changes

1. **Keep wasmtime, component model, one `tsubame-plughost.exe`.** Every number passes with a wide margin. No need for a lighter runtime.
2. **Ship the host runtime-only (0.86 MB, no Cranelift) and precompile at install time.** The full host is 9.3 MB; Cranelift is nearly all of it and also costs 58 ms per plugin at spawn. Add to PLAN §4: install of a wasm plugin runs a compile step (the 9.3 MB build, downloaded or bundled as `tsubame-plugc.exe`, used only when a plugin is installed or the host version changes) that writes a `.cwasm` cache keyed by wasmtime version and CPU. Keep the original `.wasm` and recompile when the key changes.
3. **Security note for item 2:** `Component::deserialize` is `unsafe` and trusts the bytes. Only load `.cwasm` files our own compile step wrote (store them in a directory only the app writes, check a hash); never accept a `.cwasm` from a plugin package.
4. **API shape:** use plain `list<string>` as the public WIT signature (24 µs for 100 rows is nothing). Keep the packed form as an optional second export only for bulk jobs (search, 10,000-row batches), where it halves the cost.
5. Cap batch size per call (≤ 2,000 paths suggested) to bound guest memory.
6. Not measured, left open: `wasmtime-wasi` size and cost for plugins that need file access (do not add until a previewer or vfs task needs it), Pulley (no reason: JIT output is native and the runtime-only host already removes the compiler), cold-boot first-launch spawn time, and a plugin that does real I/O.
