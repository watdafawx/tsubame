# sample-file-kind

Sample **column** plugin for Tsubame, built with `tsubame-plugin-sdk`.

| Column | Cell | Shows |
|---|---|---|
| `kind` | text | Image, Video, Audio, Document, Archive, Code or Other, from the extension; empty when there is none |
| `name-length` | integer | characters in the file name (sorts as a number) |

It only reads path text, so `plugin.toml` has no `[capabilities]`: no file
access, no network, no processes, default limits (64 MB, 2000 ms).

Build the `.cwasm` (needs `rustup target add wasm32-wasip2`):

```
pwsh plugins\build-plugin.ps1 plugins\sample-file-kind
```

Test: `cargo test -p sample-file-kind` in `plugins/` (logic, on the host) and
`cargo test -p tsubame-plughost --test samples` (the real host exe).

Guide: `docs/plugin-authoring.md`.
