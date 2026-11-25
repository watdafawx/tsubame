# sample-csv-summary

Sample **previewer** plugin for Tsubame, built with `tsubame-plugin-sdk`.
For a `.csv` file it shows the row and column counts, the delimiter (comma,
semicolon or tab, detected from the first line), the file size, the header
names and the next few rows. Quoted fields, doubled quotes, line breaks inside
quotes, `\r\n` and a byte order mark are handled.

It reads at most the first 1 MiB (less if the caller's `limits.max_bytes` is
lower); for a longer file the counts say "at least".

Capabilities in `plugin.toml`, the minimum for the job:

```toml
[capabilities]
paths = [{ path = "$input", access = "read" }]   # the file it is asked to preview, nothing else
max_memory_mb = 32
timeout_ms = 1000
```

Build and install it: the `.cwasm` and its install record (needs `rustup target add wasm32-wasip2`):

```
pwsh plugins\build-plugin.ps1 plugins\sample-csv-summary
```

Test: `cargo test -p sample-csv-summary` in `plugins/` (the parser, on the
host) and `cargo test -p tsubame-plughost --test samples` (the real host exe,
including a run without the grant, which the host refuses).

Guide: `docs/plugin-authoring.md`.
