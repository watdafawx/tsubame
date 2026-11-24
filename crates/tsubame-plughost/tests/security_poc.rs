//! Security review P5.P3: proofs of concept for `docs/security/plughost-review.md`.
//!
//! A test that is `#[ignore]` **passes while its weakness is present**. The
//! test name starts with the finding's id. When a finding is fixed, its test
//! becomes a regression test (assertion flipped, `#[ignore]` dropped, the
//! exploit setup kept): PH-01, PH-09, PH-13 b, PH-16 and PH-17 are fixed and run
//! in the normal suite.
//!
//! Run the open PoCs: `cargo test -p tsubame-plughost --test security_poc -- --ignored --nocapture`
//!
//! These need no wasm guest: they drive the public API of the manifest, the
//! path policy, the protocol and the client. The PoCs that need a real host
//! and a guest are at the end of `tests/host.rs`.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tsubame_ext_api::{Cell, Doc, Hit, Limits, Node, Query, VfsEntry};
use tsubame_plughost::adapters::{WasmColumn, WasmPreviewer, WasmSearch, WasmVfs};
use tsubame_plughost::caps::{self, Access, Capabilities, Denied, PathError, PathGrant, PathPolicy, Root, Scope};
use tsubame_plughost::client::{CallError, HostIo, HostProcess, Launcher, Options, Plugin, Status};
use tsubame_plughost::manifest::{Kind, Manifest, ProviderDecl};
use tsubame_plughost::proto::{self, Info, Load};

fn manifest(plugin: &str, caps: &str) -> String {
    format!("[plugin]\n{plugin}\napi = \"0.1\"\ncomponent = \"p.wasm\"\n{caps}\n[[column]]\nid = \"c\"\nlabel = \"C\"\n")
}

// ---- PH-01: a `*` grant, or a UNC prefix, reaches the network with `network = false` -------------
// Fixed: `*` never covers a UNC path, a UNC folder grant needs `network = true`, `@` (WebDAV) is refused.

#[test]
fn ph01_unc_paths_pass_the_policy_without_the_network_capability() {
    let caps = Capabilities { paths: vec![PathGrant { scope: Scope::Any, access: Access::ReadWrite }], ..Capabilities::default() };
    assert!(!caps.network, "the plugin was never given the network");
    assert_eq!(caps.summary()[0], "Read and change any file on this computer", "what the user was told");
    let mut policy = PathPolicy::new(&caps);
    for (path, want) in [
        // SMB to any server: Windows would send the user's NTLM response to it.
        ("\\\\attacker.example\\share\\x", Denied::NotGranted),
        // WebDAV redirector: an HTTP request whose URL carries data out.
        ("\\\\attacker.example@80\\dav\\c2VjcmV0LWRhdGE", Denied::Invalid(PathError::BadName)),
        ("\\\\attacker.example@SSL\\DavWWWRoot\\x", Denied::Invalid(PathError::BadName)),
        // Loopback SMB: admin shares and named pipes of local services.
        ("\\\\127.0.0.1\\C$\\Windows\\win.ini", Denied::NotGranted),
        ("\\\\localhost\\pipe\\some-local-service", Denied::NotGranted),
        ("\\\\?\\UNC\\attacker.example\\share\\x", Denied::NotGranted),
    ] {
        for access in [Access::Read, Access::ReadWrite] {
            assert_eq!(policy.check(path, access), Err(want), "{path}");
        }
    }
    // A local path is still covered.
    assert_eq!(policy.check("C:\\x", Access::ReadWrite).unwrap().1, [Root::Any]);

    // The manifest refuses a UNC folder as a grant while `network` stays false ...
    let e = Manifest::parse(&manifest(
        "id = \"com.example.x\"\nname = \"X\"\nversion = \"1.0.0\"",
        "[capabilities]\nnetwork = false\npaths = [{ path = \"//attacker.example/share\", access = \"read\" }]",
    ))
    .unwrap_err();
    assert!(e.0.contains("needs `network = true`"), "{}", e.0);
    // ... and so does the host's own check of what the client sends.
    let c = Capabilities { paths: vec![PathGrant { scope: Scope::Prefix("\\\\attacker.example\\share".into()), access: Access::Read }], ..Capabilities::default() };
    assert!(c.validate().is_err());
    // With the network capability it is allowed, and the prompt names the server.
    let m = Manifest::parse(&manifest(
        "id = \"com.example.x\"\nname = \"X\"\nversion = \"1.0.0\"",
        "[capabilities]\nnetwork = true\npaths = [{ path = \"//attacker.example/share\", access = \"read\" }]",
    ))
    .unwrap();
    let c = m.capabilities("C:\\p", "C:\\d").unwrap();
    assert_eq!(c.summary()[0], "Read files on the server attacker.example: \\\\attacker.example\\share");
}

// ---- PH-09: reserved device names the lexical check missed ---------------------------------------
// Fixed: the superscript digits are device digits too.

#[test]
fn ph09_superscript_device_names_are_accepted() {
    // Windows reserves COM1-9, LPT1-9 and also the superscript digits 1, 2 and 3
    // ("Naming Files, Paths, and Namespaces").
    for p in ["C:\\a\\COM\u{b9}", "C:\\a\\COM\u{b2}", "C:\\a\\LPT\u{b3}", "C:\\a\\com\u{b9}.txt"] {
        assert_eq!(caps::normalize(p), Err(PathError::Device), "{p}");
    }
    // the ASCII forms are refused, as before
    assert!(caps::normalize("C:\\a\\COM1").is_err());
}

// ---- PH-10: two plugins, one cache file ----------------------------------------------------------

#[test]
#[ignore = "PoC PH-10: passes while `<id>-<version>.cwasm` is ambiguous"]
fn ph10_two_plugins_map_to_the_same_cwasm_name() {
    let a = Manifest::parse(&manifest("id = \"a.b-1.0.0\"\nname = \"A\"\nversion = \"2.0.0\"", "")).unwrap();
    let b = Manifest::parse(&manifest("id = \"a.b\"\nname = \"B\"\nversion = \"1.0.0-2.0.0\"", "")).unwrap();
    assert_ne!(a.id, b.id);
    // `host::install::install` writes `<out_dir>/<id>-<version>.cwasm` (host.rs, `install`).
    let file = |m: &Manifest| format!("{}-{}.cwasm", m.id, m.version);
    assert_eq!(file(&a), file(&b));
    assert_eq!(file(&a), "a.b-1.0.0-2.0.0.cwasm");
}

// ---- PH-11: what the install prompt would show ---------------------------------------------------

#[test]
#[ignore = "PoC PH-11: passes while bidi/invisible characters and device-name ids are accepted"]
fn ph11_manifest_accepts_spoofing_characters_and_device_name_ids() {
    // U+202E (right-to-left override) and U+200B (zero width space) are not `char::is_control`.
    let m = Manifest::parse(&manifest("id = \"com.example.x\"\nname = \"Tsubame \u{202e}detsurT\u{200b}\"\nversion = \"1.0.0\"", "")).unwrap();
    assert!(m.name.contains('\u{202e}') && m.name.contains('\u{200b}'));
    // The id becomes a file name (`<id>-<version>.cwasm`) and, likely, a data folder name.
    // `nul.x-1.0.0.cwasm` is the NUL device on Windows 10 and older.
    for id in ["nul.x", "aux.plugin", "con.fig", "com1.tools", "prn.t"] {
        let m = Manifest::parse(&manifest(&format!("id = \"{id}\"\nname = \"N\"\nversion = \"1.0.0\""), "")).unwrap();
        assert_eq!(m.id, id);
    }
}

// ---- PH-13: the client allocates far more than a reply's size -------------------------------------

#[test]
#[ignore = "PoC PH-13: passes while a reply's list count is only checked against the frame size"]
fn ph13_a_small_reply_makes_the_client_allocate_many_times_its_size() {
    // A reply of N one-byte nodes (`Indent`). `dec_doc` reserves N * size_of::<Node>().
    const N: usize = 1 << 20;
    let mut frame = Vec::with_capacity(N + 16);
    frame.extend_from_slice(&7u32.to_le_bytes()); // request id
    frame.push(proto::reply::OK);
    frame.extend_from_slice(&(N as u32).to_le_bytes());
    frame.resize(frame.len() + N, 7); // tag 7 = Indent
    let reply = proto::decode_reply(&frame).unwrap();
    let mut nodes: Vec<Node> = Vec::new();
    proto::dec_doc(reply.body.unwrap(), &mut nodes).unwrap();
    let heap = nodes.capacity() * size_of::<Node>();
    let ratio = heap / frame.len();
    println!("PH-13: {} byte reply -> {} bytes of Vec<Node> ({}x); a {} MiB frame -> {} MiB", frame.len(), heap, ratio, proto::MAX_FRAME >> 20, (proto::MAX_FRAME * ratio) >> 20);
    assert!(ratio >= 24, "ratio {ratio}");

    // The same for a column reply: the count is compared with the request only after decoding.
    let mut frame = Vec::with_capacity(N + 16);
    frame.extend_from_slice(&7u32.to_le_bytes());
    frame.push(proto::reply::OK);
    frame.extend_from_slice(&(N as u32).to_le_bytes());
    frame.resize(frame.len() + N, 0); // tag 0 = Cell::Empty
    let mut cells: Vec<Cell> = Vec::new();
    let n = proto::dec_cells(proto::decode_reply(&frame).unwrap().body.unwrap(), &mut cells).unwrap();
    assert_eq!(n, N, "a caller that asked for 1 path gets {N} cells decoded before it can compare");
    println!("PH-13: cells: {} byte reply -> {} bytes", frame.len(), cells.capacity() * size_of::<Cell>());
}

// ---- a scripted host for the client PoCs ---------------------------------------------------------

struct Script {
    data: Vec<u8>,
    pos: usize,
    consumed: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // After the script the "host" says nothing more until it is killed.
        while self.pos == self.data.len() {
            if self.stop.load(Ordering::SeqCst) {
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let n = buf.len().min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        self.consumed.fetch_add(n, Ordering::SeqCst);
        Ok(n)
    }
}

struct Sink;
impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Proc(Arc<AtomicBool>);
impl HostProcess for Proc {
    fn kill(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn wait(&mut self, _: Duration) -> bool {
        true
    }
}

/// A host that ignores its input and plays `script` once.
struct Scripted {
    script: Vec<u8>,
    consumed: Arc<AtomicUsize>,
}

impl Launcher for Scripted {
    fn launch(&self) -> io::Result<HostIo> {
        let stop = Arc::new(AtomicBool::new(false));
        Ok(HostIo {
            input: Box::new(Sink),
            output: Box::new(Script { data: self.script.clone(), pos: 0, consumed: self.consumed.clone(), stop: stop.clone() }),
            process: Box::new(Proc(stop)),
        })
    }
}

fn load(timeout_ms: u32) -> Load {
    Load {
        plugin_id: "test.poc".into(),
        version: "1.0.0".into(),
        component: "C:\\none.cwasm".into(),
        sha256: [0; 32],
        caps: Capabilities { timeout_ms, ..Capabilities::default() },
        entrypoints: entrypoints(),
    }
}

fn entrypoints() -> Vec<(Kind, String)> {
    vec![(Kind::Column, "c".into()), (Kind::Previewer, "pv".into()), (Kind::Vfs, "v".into()), (Kind::Search, "s".into()), (Kind::Command, "open".into())]
}

fn decl(id: &str) -> ProviderDecl {
    ProviderDecl { id: id.into(), title: id.into(), extensions: vec!["zip".into()] }
}

/// A plugin whose host answers the first call with `answer` (request id 2).
fn plugin_answering(answer: Vec<u8>) -> Arc<Plugin> {
    let mut script = greeting();
    script.extend(answer);
    Arc::new(Plugin::new(load(2_000), Scripted { script, consumed: Arc::new(AtomicUsize::new(0)) }, Options::default()))
}

/// Hello, then the answer to `Load` (the client's first request id is 1).
fn greeting() -> Vec<u8> {
    let mut s = proto::enc_hello();
    s.extend(proto::enc_ok_info(1, &Info { id: "test.poc".into(), version: "1.0.0".into(), entrypoints: entrypoints() }).unwrap());
    s
}

// ---- PH-14: frames nobody asked for pile up in the app --------------------------------------------

#[test]
#[ignore = "PoC PH-14: passes while the client reads and queues replies without a pending request"]
fn ph14_unsolicited_frames_are_read_and_queued_without_limit() {
    const FLOOD_FRAMES: usize = 48;
    const FRAME: usize = 1 << 20;
    let mut script = greeting();
    script.extend(proto::enc_ok_cells(2, &[Cell::Int(1)]).unwrap()); // the answer to the one real call
    for _ in 0..FLOOD_FRAMES {
        script.extend_from_slice(&(FRAME as u32).to_le_bytes());
        script.extend_from_slice(&999u32.to_le_bytes());
        script.push(proto::reply::OK);
        script.resize(script.len() + FRAME - proto::HEADER, 0);
    }
    let total = script.len();
    let consumed = Arc::new(AtomicUsize::new(0));
    let plugin = Plugin::new(load(2_000), Scripted { script, consumed: consumed.clone() }, Options::default());
    let mut out = Vec::new();
    plugin.column_values("c", &["C:\\a"], &mut out).unwrap();
    assert_eq!(out, [Cell::Int(1)]);
    // No call is running now. The reader thread still drains the pipe into an unbounded channel.
    let end = Instant::now() + Duration::from_secs(10);
    while consumed.load(Ordering::SeqCst) < total && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let got = consumed.load(Ordering::SeqCst);
    println!("PH-14: the idle client took {} MiB of frames it never asked for", got >> 20);
    assert_eq!(got, total, "all {FLOOD_FRAMES} unsolicited frames were read and are held in memory");
    assert_eq!(plugin.status(), Status::Running, "and the host is still considered healthy");
    // Only the next call notices (a stale frame answers it) and kills the host.
    assert!(matches!(plugin.column_values("c", &["C:\\a"], &mut out), Err(CallError::Protocol(_))));
}

// ---- PH-15: every other method waits behind a running call ----------------------------------------

#[test]
#[ignore = "PoC PH-15: passes while `status()` blocks for as long as a plugin call runs"]
fn ph15_status_blocks_behind_a_call_in_flight() {
    // The host loads fine, then never answers the call.
    let plugin = Arc::new(Plugin::new(load(1_500), Scripted { script: greeting(), consumed: Arc::new(AtomicUsize::new(0)) }, Options::default()));
    let p = plugin.clone();
    let worker = std::thread::spawn(move || p.column_values("c", &["C:\\a"], &mut Vec::new()));
    std::thread::sleep(Duration::from_millis(200));
    // What a UI thread would do to draw a "plugin is running" badge, or `stop_if_idle` on a timer tick.
    let t0 = Instant::now();
    let _ = plugin.status();
    let blocked = t0.elapsed();
    println!("PH-15: status() returned after {blocked:?} (the plugin's timeout_ms is 1500)");
    assert!(blocked >= Duration::from_millis(1_000), "{blocked:?}");
    assert_eq!(worker.join().unwrap(), Err(CallError::Timeout));
}

// ---- PH-17: untrusted text in messages (fixed: regression tests) ---------------------------------

#[test]
fn ph17_a_manifest_error_does_not_repeat_untrusted_input() {
    // One 60 KB line with a terminal escape sequence in it and a syntax error at its end.
    let esc = char::from(27);
    let text = format!("x = {esc}[2J{esc}[31mFAKE OUTPUT {}", "A".repeat(60_000));
    let e = Manifest::parse(&text).unwrap_err();
    assert!(e.0.len() < 400, "{} bytes", e.0.len());
    assert!(!e.0.contains(esc) && !e.0.contains("FAKE OUTPUT"), "{:?}", e.0);
}

#[test]
fn ph17_a_plugin_error_is_cleaned_and_named() {
    use tsubame_ext_api::SearchProvider;
    let esc = char::from(27);
    let msg = format!("{esc}[2JTsubame: your disk is full\u{202e}!{}", "x".repeat(5_000));
    let p = plugin_answering(proto::enc_err(2, proto::ErrKind::Plugin, &msg));
    let e = WasmSearch::new(p, &decl("s")).search(&Query { text: "a", root: "", limit: 10 }, &mut |_| true).unwrap_err();
    assert!(e.starts_with("test.poc: "), "{e:.80}");
    assert!(e.len() < 1_200, "{} bytes", e.len());
    assert!(!e.contains(esc) && !e.contains('\u{202e}'), "{e:.80?}");
}

// ---- PH-16 / PH-13 b: plugin output at the adapters (fixed: regression tests) ---------------------

fn hit(path: &str) -> Hit {
    Hit { path: path.into(), dir: false, size: 1, mtime: 2 }
}

fn search(root: &str, hits: &[Hit]) -> Vec<String> {
    use tsubame_ext_api::SearchProvider;
    let p = plugin_answering(proto::enc_ok_hits(2, hits, None).unwrap());
    let mut got = Vec::new();
    WasmSearch::new(p, &decl("s"))
        .search(&Query { text: "a", root, limit: 100 }, &mut |h| {
            got.extend(h.iter().map(|h| h.path.clone()));
            true
        })
        .unwrap();
    got
}

#[test]
fn ph16_a_unc_hit_from_a_plugin_is_dropped() {
    let hits = [hit(r"\\attacker.example\share\x"), hit(r"C:\data\ok")];
    assert_eq!(search("", &hits), [r"C:\data\ok"]);
    assert_eq!(search(r"C:\data", &hits), [r"C:\data\ok"]);
}

#[test]
fn ph16_a_hit_outside_the_query_root_or_with_dotdot_is_dropped() {
    let hits = [hit(r"C:\data\..\Windows\x"), hit(r"C:\other\x"), hit(r"C:\data\sub\x")];
    assert_eq!(search(r"C:\data", &hits), [r"C:\data\sub\x"]);
}

#[test]
fn ph16_a_dotdot_entry_name_is_dropped() {
    use tsubame_ext_api::VfsProvider;
    let e = |n: &str| VfsEntry { name: n.into(), dir: false, size: 0, mtime: 0 };
    let entries = [e(r"..\..\Startup\x.cmd"), e(".."), e("a/b"), e("NUL"), e("evil\u{202e}gnp.exe"), e("ctl\u{7}"), e("fine.txt")];
    let p = plugin_answering(proto::enc_ok_entries(2, &entries, None).unwrap());
    let mut names = Vec::new();
    WasmVfs::new(p, &decl("v"))
        .list(r"C:\a.zip", &mut |es| {
            names.extend(es.iter().map(|e| e.name.clone()));
            true
        })
        .unwrap();
    assert_eq!(names, ["fine.txt"]);
}

fn preview(nodes: &[Node], max_bytes: u64) -> Vec<Node> {
    use tsubame_ext_api::Previewer;
    let p = plugin_answering(proto::enc_ok_doc(2, nodes).unwrap());
    let mut doc = Doc { nodes: vec![Node::Heading("kept".into())] };
    WasmPreviewer::new(p, &decl("pv")).preview(r"C:\a.zip", Limits { max_bytes, max_px: 100 }, &mut doc).unwrap();
    assert_eq!(doc.nodes[0], Node::Heading("kept".into()), "what the caller had stays");
    doc.nodes.split_off(1)
}

#[test]
fn ph16_a_button_may_only_run_the_plugins_own_commands() {
    let button = |c: &str| Node::Button { label: "Open".into(), command: c.to_string().into() };
    let got = preview(&[button("test.poc.open"), button("tsubame.delete"), button("test.poc.delete"), button("other.open")], 1 << 20);
    assert_eq!(got, [button("test.poc.open")]);
}

#[test]
fn ph13_b_overlong_strings_and_documents_are_capped() {
    use tsubame_plughost::sanitize::{MAX_NODES, MAX_NODE_TEXT};
    let big = Node::Text { text: "x".repeat(100_000), dim: false };
    let got = preview(&[big], 1 << 20);
    assert!(matches!(&got[..], [Node::Text { text, .. }] if text.len() == MAX_NODE_TEXT), "{}", got.len());

    let many: Vec<Node> = (0..MAX_NODES + 500).map(|_| Node::Indent).collect();
    assert_eq!(preview(&many, 1 << 20).len(), MAX_NODES);

    // `limits.max_bytes` bounds the document.
    let lines: Vec<Node> = (0..10).map(|_| Node::Mono("y".repeat(1_000))).collect();
    assert_eq!(preview(&lines, 3_500).len(), 3);
}

#[test]
fn ph17_control_and_bidi_characters_are_dropped_from_text() {
    let got = preview(&[Node::Heading("a\u{202e}b\u{1b}[2Jc\u{2028}d".into()), Node::Pair("k\u{200b}".into(), "v\u{feff}".into())], 1 << 20);
    assert_eq!(got, [Node::Heading("ab[2Jcd".into()), Node::Pair("k".into(), "v".into())]);
}

#[test]
fn ph13_b_an_overlong_cell_is_capped() {
    use tsubame_ext_api::ColumnProvider;
    use tsubame_plughost::sanitize::MAX_CELL_TEXT;
    let p = plugin_answering(proto::enc_ok_cells(2, &[Cell::Text("z\u{202e}".repeat(5_000))]).unwrap());
    let mut out = Vec::new();
    WasmColumn::new(p, "c").values(&[r"C:\a"], &mut out);
    assert!(matches!(&out[..], [Cell::Text(s)] if s.len() == MAX_CELL_TEXT && !s.contains('\u{202e}')), "{}", out.len());
}
