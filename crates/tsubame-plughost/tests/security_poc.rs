//! Security review P5.P3: proofs of concept for `docs/security/plughost-review.md`.
//!
//! A test that is `#[ignore]` **passes while its weakness is present**. The
//! test name starts with the finding's id. When a finding is fixed, its test
//! becomes a regression test (assertion flipped, `#[ignore]` dropped, the
//! exploit setup kept): PH-01, PH-02, PH-03, PH-09, PH-10, PH-11, PH-12, PH-13 a and b,
//! PH-14, PH-15, PH-16, PH-17 and PH-18 are fixed and run in the normal suite.
//!
//! Run the open PoCs: `cargo test -p tsubame-plughost --test security_poc -- --ignored --nocapture`
//!
//! These need no wasm guest: they drive the public API of the manifest, the
//! path policy, the protocol and the client. The PoCs that need a real host
//! and a guest are at the end of `tests/host.rs`.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tsubame_ext_api::{Cell, Doc, Hit, Limits, Node, Query, VfsEntry};
use tsubame_plughost::adapters::{WasmColumn, WasmPreviewer, WasmSearch, WasmVfs};
use tsubame_plughost::caps::{self, Access, Capabilities, Denied, PathError, PathGrant, PathPolicy, Protected, Root, Scope};
use tsubame_plughost::client::{CallError, HostIo, HostProcess, Launcher, Options, Plugin, Status};
use tsubame_plughost::install_record;
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
    assert_eq!(caps.summary()[0], "Read and change any file on this computer: full control of your files", "what the user was told");
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
    let c = m.capabilities("C:\\p", "C:\\d", &Protected::default()).unwrap();
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
// Fixed: one folder per id and version, `<cache>\<id>\<version>\component.cwasm`.

#[test]
fn ph10_two_plugins_map_to_the_same_cwasm_name() {
    let a = Manifest::parse(&manifest("id = \"a.b-1.0.0\"\nname = \"A\"\nversion = \"2.0.0\"", "")).unwrap();
    let b = Manifest::parse(&manifest("id = \"a.b\"\nname = \"B\"\nversion = \"1.0.0-2.0.0\"", "")).unwrap();
    assert_ne!(a.id, b.id);
    // The old name, `<id>-<version>.cwasm`, was the same for both.
    assert_eq!(format!("{}-{}.cwasm", a.id, a.version), format!("{}-{}.cwasm", b.id, b.version));
    let cache = Path::new("C:\\cache");
    let dir = |m: &Manifest| install_record::install_dir(cache, &m.id, &m.version);
    assert_ne!(dir(&a), dir(&b));
    assert_eq!(dir(&a), Path::new("C:\\cache\\a.b-1.0.0\\2.0.0"));
    assert_eq!(dir(&b), Path::new("C:\\cache\\a.b\\1.0.0-2.0.0"));
}

// ---- PH-02, PH-03, PH-18: the install record ---------------------------------------------------------
// Fixed: the install step writes a record (hashes and the approved capabilities) once; every start
// checks the package against it and takes the capabilities from it, never from the live plugin.toml.

/// A folder that is removed when the test ends.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("tsubame-poc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const WASM: &[u8] = b"\0asm stand-in: never compiled here";
const CWASM: &[u8] = b"stand-in for a compiled image";

/// A package in `<t>\pkg` with `caps`, installed into `<t>\cache`.
fn installed(t: &Temp, caps: &str) -> Result<(PathBuf, PathBuf, String), install_record::RecordError> {
    let pkg = t.0.join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    let text = manifest("id = \"com.example.poc\"\nname = \"PoC\"\nversion = \"1.0.0\"", caps);
    std::fs::write(pkg.join("plugin.toml"), &text).unwrap();
    std::fs::write(pkg.join("p.wasm"), WASM).unwrap();
    let cache = t.0.join("cache");
    install_record::store(&pkg, &cache, Some(&t.0.join("data")), &[], text.as_bytes(), WASM, CWASM)?;
    Ok((pkg, cache, text))
}

fn verify(pkg: &Path, cache: &Path) -> Result<install_record::Verified, install_record::RecordError> {
    install_record::verify(pkg, cache, "com.example.poc", "1.0.0", &[])
}

#[test]
fn ph03_editing_plugin_toml_after_install_refuses_the_load() {
    let t = Temp::new("edit-manifest");
    let (pkg, cache, text) = installed(&t, "[capabilities]\npaths = [{ path = \"$input\", access = \"read\" }]").unwrap();
    let v = verify(&pkg, &cache).unwrap();
    let load = Load::new(&v);
    assert_eq!(load.caps.paths, [PathGrant { scope: Scope::Input, access: Access::Read }]);
    assert_eq!(load.sha256, tsubame_plughost::sha256::sha256(CWASM));
    assert!(load.component.ends_with("cache\\com.example.poc\\1.0.0\\component.cwasm"), "{}", load.component);
    assert_eq!(load.protected.len(), 2, "the package and the cache: {:?}", load.protected);

    // After approval, the package asks for everything.
    let grown = text.replace("access = \"read\" }]", "access = \"read\" }, { path = \"*\", access = \"read-write\" }]");
    assert_ne!(grown, text);
    std::fs::write(pkg.join("plugin.toml"), &grown).unwrap();
    let e = verify(&pkg, &cache).unwrap_err();
    assert!(e.0.contains("plugin.toml changed since it was installed"), "{e}");

    // Put back, the load works again; a changed `.wasm` is refused the same way.
    std::fs::write(pkg.join("plugin.toml"), &text).unwrap();
    assert!(verify(&pkg, &cache).is_ok());
    std::fs::write(pkg.join("p.wasm"), b"\0asm another component").unwrap();
    let e = verify(&pkg, &cache).unwrap_err();
    assert!(e.0.contains("the component (.wasm) changed"), "{e}");
}

#[test]
fn ph03_the_record_is_written_once() {
    let t = Temp::new("once");
    let (pkg, cache, text) = installed(&t, "").unwrap();
    let record = cache.join("com.example.poc\\1.0.0\\install.toml");
    let before = std::fs::read(&record).unwrap();
    // Installing over it (with a package that asks for more) is refused, and the record is untouched.
    let more = text.replace("component = \"p.wasm\"", "component = \"p.wasm\"\n[capabilities]\nnetwork = true");
    let e = install_record::store(&pkg, &cache, None, &[], more.as_bytes(), WASM, b"other image").unwrap_err();
    assert!(e.0.contains("already installed"), "{e}");
    assert_eq!(std::fs::read(&record).unwrap(), before);
    assert_eq!(std::fs::read(cache.join("com.example.poc\\1.0.0\\component.cwasm")).unwrap(), CWASM);
    // A record from another plugin, copied in, is refused.
    let other = t.0.join("cache\\com.example.other\\1.0.0");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(&record, other.join("install.toml")).unwrap();
    let e = install_record::verify(&pkg, &cache, "com.example.other", "1.0.0", &[]).unwrap_err();
    assert!(e.0.contains("for another plugin"), "{e}");
}

#[test]
fn ph02_a_grant_that_reaches_the_package_or_the_cache_is_refused_at_install() {
    let t = Temp::new("protected");
    std::fs::create_dir_all(t.0.join("pkg")).unwrap();
    std::fs::create_dir_all(t.0.join("cache")).unwrap();
    let real = |p: &str| install_record::real_dir(&t.0.join(p)).unwrap();
    for (grant, why) in [
        (format!("{{ path = '{}', access = \"read-write\" }}", real("pkg")), "its own package, read-write"),
        (format!("{{ path = '{}', access = \"read-write\" }}", real("")), "a parent of the package, read-write"),
        (format!("{{ path = '{}', access = \"read\" }}", real("cache")), "the cache, even read only"),
    ] {
        let e = installed(&t, &format!("[capabilities]\npaths = [{grant}]")).unwrap_err();
        assert!(e.0.contains("reaches into a folder of Tsubame's own"), "{why}: {e}");
    }
    assert!(!t.0.join("cache\\com.example.poc").exists(), "nothing was written");
    // `$plugin` (read only) is fine.
    let (pkg, cache, _) = installed(&t, "[capabilities]\npaths = [{ path = \"$plugin\", access = \"read\" }]").unwrap();
    let v = verify(&pkg, &cache).unwrap();
    assert_eq!(v.caps.paths, [PathGrant { scope: Scope::Prefix(real("pkg")), access: Access::Read }]);
}

#[test]
fn ph18_an_oversized_manifest_or_record_is_not_read_in_full() {
    let t = Temp::new("oversized");
    let (pkg, cache, text) = installed(&t, "").unwrap();
    let mut big = text.clone();
    big.push_str(&format!("# {}\n", "x".repeat(70_000)));
    std::fs::write(pkg.join("plugin.toml"), &big).unwrap();
    assert!(Manifest::load(&pkg).unwrap_err().0.contains("larger than"));
    assert!(verify(&pkg, &cache).unwrap_err().0.contains("larger than"));
    std::fs::write(pkg.join("plugin.toml"), &text).unwrap();
    let record = cache.join("com.example.poc\\1.0.0\\install.toml");
    let mut r = std::fs::read_to_string(&record).unwrap();
    r.push_str(&format!("# {}\n", "x".repeat(70_000)));
    std::fs::write(&record, r).unwrap();
    assert!(verify(&pkg, &cache).unwrap_err().0.contains("not installed"));
}

// ---- PH-12: what `$input` covers ---------------------------------------------------------------------
// Fixed: inputs are scoped per entry point. A column or a preview gets the path itself, a
// command its selection (recursive) and the folder it runs in (flat), a search its root.

#[test]
fn ph12_a_flat_input_folder_does_not_cover_its_tree() {
    let c = Capabilities { paths: vec![PathGrant { scope: Scope::Input, access: Access::ReadWrite }], ..Capabilities::default() };
    let mut p = PathPolicy::new(&c);
    // A column over the profile folder: each row is only the path itself.
    p.set_inputs(vec![("C:\\Users\\u\\.ssh".into(), false), ("C:\\Users\\u\\Documents".into(), false)]);
    assert!(p.check("C:\\Users\\u\\.ssh", Access::Read).is_ok());
    assert_eq!(p.check("C:\\Users\\u\\.ssh\\id_ed25519", Access::Read), Err(Denied::NotGranted));
    assert_eq!(p.check("C:\\Users\\u\\Documents\\a\\b.txt", Access::Read), Err(Denied::NotGranted));
    // A command on one selected file: the folder it runs in can be looked at, nothing in it changed.
    p.set_inputs(vec![("C:\\w\\a.txt".into(), true), ("C:\\w".into(), false)]);
    assert!(p.check("C:\\w\\a.txt", Access::ReadWrite).is_ok());
    assert!(p.check("C:\\w", Access::Read).is_ok());
    assert_eq!(p.check("C:\\w\\b.txt", Access::ReadWrite), Err(Denied::NotGranted));
    assert_eq!(p.check("C:\\w\\sub\\c.txt", Access::Read), Err(Denied::NotGranted));
    // A selected folder is the user's choice: everything in it.
    p.set_inputs(vec![("C:\\w\\sub".into(), true), ("C:\\w".into(), false)]);
    assert!(p.check("C:\\w\\sub\\deep\\c.txt", Access::ReadWrite).is_ok());
}

// ---- PH-11: what the install prompt would show ---------------------------------------------------
// Fixed: labels and the description refuse control, format (bidi, zero-width), separator and private-use
// characters; a plugin id with a Windows device name part is refused.

#[test]
fn ph11_manifest_refuses_spoofing_characters_and_device_name_ids() {
    // U+202E (right-to-left override), U+200B (zero width space), U+2028 (line separator), U+E000 (private use).
    for bad in ["Tsubame \u{202e}detsurT", "Tsub\u{200b}ame", "two\u{2028}lines", "Tsubame\u{e000}", "bell\\u0007"] {
        let e = Manifest::parse(&manifest(&format!("id = \"com.example.x\"\nname = \"{bad}\"\nversion = \"1.0.0\""), "")).unwrap_err();
        assert!(e.0.contains("plugin.name"), "{bad:?}: {}", e.0);
    }
    // The same for the description (a newline is still fine there) and the entry labels.
    let e = Manifest::parse(&manifest("id = \"com.example.x\"\nname = \"N\"\nversion = \"1.0.0\"\ndescription = \"a\u{202e}b\"", "")).unwrap_err();
    assert!(e.0.contains("plugin.description"), "{}", e.0);
    let ok = manifest("id = \"com.example.x\"\nname = \"N\"\nversion = \"1.0.0\"\ndescription = \"two\\nlines\"", "");
    assert!(Manifest::parse(&ok).is_ok());
    let spoof_label = manifest("id = \"com.example.x\"\nname = \"N\"\nversion = \"1.0.0\"", "").replace("label = \"C\"", "label = \"C\u{202e}\"");
    assert!(Manifest::parse(&spoof_label).unwrap_err().0.contains("label"));

    // The id becomes a file name (`<id>-<version>.cwasm`) and a data folder name:
    // `nul.x-1.0.0.cwasm` is the NUL device on Windows 10 and older.
    for id in ["nul.x", "aux.plugin", "con.fig", "com1.tools", "prn.t", "com.example.lpt3", "a.nul.b"] {
        let e = Manifest::parse(&manifest(&format!("id = \"{id}\"\nname = \"N\"\nversion = \"1.0.0\""), "")).unwrap_err();
        assert!(e.0.contains("plugin.id"), "{id}: {}", e.0);
    }
    // Names that only look like devices are fine (`com` is not `com1`; `console` is not `con`).
    for id in ["com.example.x", "console.tools", "com10.x", "nulled.x"] {
        assert_eq!(Manifest::parse(&manifest(&format!("id = \"{id}\"\nname = \"N\"\nversion = \"1.0.0\""), "")).unwrap().id, id);
    }
}

// ---- PH-13: the client allocates far more than a reply's size -------------------------------------
// Fixed: a document reply is capped at `MAX_NODES` nodes and a list at `MAX_BATCH` entries before anything
// is reserved, and a column reply must carry exactly as many values as the request had paths.

#[test]
fn ph13_a_small_reply_is_refused_before_the_client_allocates() {
    // A reply of N one-byte nodes (`Indent`). It used to reserve N * size_of::<Node>().
    const N: usize = 1 << 20;
    let mut frame = Vec::with_capacity(N + 16);
    frame.extend_from_slice(&7u32.to_le_bytes()); // request id
    frame.push(proto::reply::OK);
    frame.extend_from_slice(&(N as u32).to_le_bytes());
    frame.resize(frame.len() + N, 7); // tag 7 = Indent
    let reply = proto::decode_reply(&frame).unwrap();
    let mut nodes: Vec<Node> = Vec::new();
    assert!(proto::dec_doc(reply.body.unwrap(), &mut nodes).is_err());
    assert!(nodes.capacity() * size_of::<Node>() < 1 << 16, "nothing was reserved: {} bytes", nodes.capacity() * size_of::<Node>());

    // The same for a column reply: the count is compared with the request before the cells are decoded.
    let mut frame = Vec::with_capacity(N + 16);
    frame.extend_from_slice(&7u32.to_le_bytes());
    frame.push(proto::reply::OK);
    frame.extend_from_slice(&(N as u32).to_le_bytes());
    frame.resize(frame.len() + N, 0); // tag 0 = Cell::Empty
    let mut cells: Vec<Cell> = Vec::new();
    let e = proto::dec_cells(proto::decode_reply(&frame).unwrap().body.unwrap(), &mut cells, 1).unwrap_err();
    assert!(e.0.contains("does not match"), "{}", e.0);
    assert!(cells.capacity() * size_of::<Cell>() < 1 << 16, "nothing was reserved: {} bytes", cells.capacity() * size_of::<Cell>());

    // Through the client: a host that answers a 1-path request with 3 values is a protocol error.
    let p = plugin_answering(proto::enc_ok_cells(2, &[Cell::Int(1), Cell::Int(2), Cell::Int(3)]).unwrap());
    let mut out = vec![Cell::Int(9)];
    assert!(matches!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Protocol(_))));
    assert_eq!(out, [Cell::Int(9)], "what the caller had stays, nothing of the bad reply is added");
    // ... and so is one that answers with fewer.
    let p = plugin_answering(proto::enc_ok_cells(2, &[Cell::Int(1)]).unwrap());
    assert!(matches!(p.column_values("c", &["C:\\a", "C:\\b"], &mut Vec::new()), Err(CallError::Protocol(_))));
}

// ---- a scripted host for the client PoCs ---------------------------------------------------------

/// What the scripted host plays, one frame at a time. A real host answers a request after it got it,
/// and the client refuses a frame that comes before (PH-14), so frame `i` is released only after the
/// client wrote `min(i, 2)` frames: the hello at once, the answer to `Load` after the first request, the
/// answer to the call and anything after it after the second.
struct Script {
    data: Vec<u8>,
    /// Where each frame starts in `data`.
    starts: Vec<usize>,
    pos: usize,
    consumed: Arc<AtomicUsize>,
    written: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Script {
    fn new(data: Vec<u8>, consumed: Arc<AtomicUsize>, written: Arc<AtomicUsize>, stop: Arc<AtomicBool>) -> Script {
        let mut starts = Vec::new();
        let mut at = 0;
        while at < data.len() {
            starts.push(at);
            at += 4 + u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        }
        Script { data, starts, pos: 0, consumed, written, stop }
    }
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Ok(0);
            }
            // After the script the "host" says nothing more until it is killed.
            if self.pos < self.data.len() {
                let i = self.starts.partition_point(|&s| s <= self.pos) - 1;
                if self.written.load(Ordering::SeqCst) >= i.min(2) {
                    let end = self.starts.get(i + 1).copied().unwrap_or(self.data.len());
                    let n = buf.len().min(end - self.pos);
                    buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                    self.pos += n;
                    self.consumed.fetch_add(n, Ordering::SeqCst);
                    return Ok(n);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Counts the frames the client writes.
struct Sink {
    pending: Vec<u8>,
    written: Arc<AtomicUsize>,
}

impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(b);
        while self.pending.len() >= 4 {
            let n = 4 + u32::from_le_bytes(self.pending[..4].try_into().unwrap()) as usize;
            if self.pending.len() < n {
                break;
            }
            self.pending.drain(..n);
            self.written.fetch_add(1, Ordering::SeqCst);
        }
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

/// A host that plays `script` once, a frame at a time (see [`Script`]).
struct Scripted {
    script: Vec<u8>,
    consumed: Arc<AtomicUsize>,
}

impl Launcher for Scripted {
    fn launch(&self) -> io::Result<HostIo> {
        let stop = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicUsize::new(0));
        Ok(HostIo {
            input: Box::new(Sink { pending: Vec::new(), written: written.clone() }),
            output: Box::new(Script::new(self.script.clone(), self.consumed.clone(), written, stop.clone())),
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
        protected: Vec::new(),
        plugin_dir: String::new(),
        data_dir: String::new(),
    }
}

fn entrypoints()-> Vec<(Kind, String)> {
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
// Fixed: the reader takes a frame only while a request is waiting and hands it over through a channel of
// one; a frame nobody asked for ends the host.

#[test]
fn ph14_an_unsolicited_frame_ends_the_host_and_is_not_queued() {
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
    // No call is running now. The first stray frame is read, the reader stops, the host is killed.
    assert_eq!(plugin.stats().kills, 0);
    let end = Instant::now() + Duration::from_secs(10);
    while plugin.stats().kills == 0 && Instant::now() < end {
        let _ = plugin.status(); // the app notices on its next look
        std::thread::sleep(Duration::from_millis(10));
    }
    let got = consumed.load(Ordering::SeqCst);
    println!("PH-14: the idle client took {} of {} MiB of frames it never asked for", got >> 20, total >> 20);
    assert_eq!(plugin.stats().kills, 1, "the host that sent data nobody asked for is killed");
    assert_eq!(plugin.status(), Status::Idle);
    assert!(got < 3 * FRAME, "at most one stray frame is read, got {got} of {total} bytes");
}

// ---- PH-15: every other method waited behind a running call ---------------------------------------
// Fixed: a call holds its own lock; the state lock is short and never held across the host round trip.

#[test]
fn ph15_status_does_not_block_behind_a_call_in_flight() {
    // The host loads fine, then never answers the call.
    let plugin = Arc::new(Plugin::new(load(1_500), Scripted { script: greeting(), consumed: Arc::new(AtomicUsize::new(0)) }, Options::default()));
    let p = plugin.clone();
    let worker = std::thread::spawn(move || p.column_values("c", &["C:\\a"], &mut Vec::new()));
    std::thread::sleep(Duration::from_millis(300));
    // What a UI thread would do to draw a "plugin is running" badge, or `stop_if_idle` on a timer tick.
    let t0 = Instant::now();
    assert_eq!(plugin.status(), Status::Running);
    let _ = plugin.stats();
    assert!(!plugin.stop_if_idle(Duration::ZERO), "a busy plugin is not stopped");
    let took = t0.elapsed();
    println!("PH-15: status(), stats() and stop_if_idle() returned after {took:?} (the plugin's timeout_ms is 1500)");
    assert!(took < Duration::from_millis(500), "{took:?}");
    assert_eq!(worker.join().unwrap(), Err(CallError::Timeout));
}

#[test]
fn ph15_shutdown_ends_a_running_call_without_waiting_for_it() {
    let plugin = Arc::new(Plugin::new(load(5_000), Scripted { script: greeting(), consumed: Arc::new(AtomicUsize::new(0)) }, Options::default()));
    let p = plugin.clone();
    let worker = std::thread::spawn(move || p.column_values("c", &["C:\\a"], &mut Vec::new()));
    std::thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    plugin.shutdown();
    assert!(t0.elapsed() < Duration::from_millis(1_000), "{:?}", t0.elapsed());
    // The call in flight ends well before its 5 s deadline.
    let r = worker.join().unwrap();
    assert!(r.is_err(), "{r:?}");
    assert!(t0.elapsed() < Duration::from_millis(3_000), "{:?}", t0.elapsed());
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
    use tsubame_ext_api::Previewer;
    use tsubame_plughost::sanitize::{MAX_NODES, MAX_NODE_TEXT};
    let big = Node::Text { text: "x".repeat(100_000), dim: false };
    let got = preview(&[big], 1 << 20);
    assert!(matches!(&got[..], [Node::Text { text, .. }] if text.len() == MAX_NODE_TEXT), "{}", got.len());

    // The host cuts a document at MAX_NODES; the client takes exactly that many and refuses more.
    let many: Vec<Node> = (0..MAX_NODES).map(|_| Node::Indent).collect();
    assert_eq!(preview(&many, 1 << 20).len(), MAX_NODES);
    let too_many: Vec<Node> = (0..MAX_NODES + 500).map(|_| Node::Indent).collect();
    let p = plugin_answering(proto::enc_ok_doc(2, &too_many).unwrap());
    let mut doc = Doc { nodes: vec![Node::Heading("kept".into())] };
    let e = WasmPreviewer::new(p, &decl("pv")).preview(r"C:\a.zip", Limits { max_bytes: 1 << 20, max_px: 100 }, &mut doc).unwrap_err();
    assert!(e.contains("more nodes"), "{e}");
    assert_eq!(doc.nodes, [Node::Heading("kept".into())], "nothing of the refused reply is added");

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
