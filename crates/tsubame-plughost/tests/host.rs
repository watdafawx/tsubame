//! End-to-end tests: the real `tsubame-plughost` exe, driven through the
//! client, running components built here from a hand-written core module.
//!
//! The guest is WebAssembly text. `wit-component` turns it into a component
//! against the real WIT file, so a change to the WIT that the guest does not
//! follow fails here. One guest covers everything: its column picks a
//! behaviour from the column's name (echo, hang, grow, trap, read a file, ...),
//! the other entry points answer with fixed data.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tsubame_ext_api::{Cell, ColumnProvider, Doc, Limits, Node, Previewer, Query, SearchProvider, VfsProvider};
use tsubame_plughost::adapters::{WasmColumn, WasmPreviewer, WasmSearch, WasmVfs};
use tsubame_plughost::caps::{Access, Capabilities, PathGrant, Scope};
use tsubame_plughost::client::{CallError, Options, Plugin, ProcessLauncher, Stats, Status};
use tsubame_plughost::manifest::{Kind, ProviderDecl};
use tsubame_plughost::proto::Load;
use tsubame_plughost::sha256;

const HOST_EXE: &str = env!("CARGO_BIN_EXE_tsubame-plughost");
const MOCK_EXE: &str = env!("CARGO_BIN_EXE_plughost-bench");

// ---- the guest -------------------------------------------------------------------------------

/// Static data for the guest's memory, laid out by the canonical ABI.
struct Image {
    base: u32,
    b: Vec<u8>,
}

impl Image {
    fn alloc(&mut self, size: usize, align: usize) -> u32 {
        while !(self.base as usize + self.b.len()).is_multiple_of(align) {
            self.b.push(0);
        }
        let at = self.base + self.b.len() as u32;
        self.b.resize(self.b.len() + size, 0);
        at
    }
    fn w8(&mut self, at: u32, v: u8) {
        self.b[(at - self.base) as usize] = v;
    }
    fn w32(&mut self, at: u32, v: u32) {
        let i = (at - self.base) as usize;
        self.b[i..i + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn w64(&mut self, at: u32, v: u64) {
        let i = (at - self.base) as usize;
        self.b[i..i + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, d: &[u8]) -> (u32, u32) {
        let at = self.alloc(d.len(), 1);
        let i = (at - self.base) as usize;
        self.b[i..i + d.len()].copy_from_slice(d);
        (at, d.len() as u32)
    }
    /// A string or list field: pointer and length.
    fn wlist(&mut self, at: u32, (ptr, len): (u32, u32)) {
        self.w32(at, ptr);
        self.w32(at + 4, len);
    }
    fn wstr(&mut self, at: u32, s: &str) {
        let d = self.bytes(s.as_bytes());
        self.wlist(at, d);
    }

    /// `meta.info`: id, version, list of (kind, id).
    fn info(&mut self, id: &str, version: &str, eps: &[(Kind, &str)]) -> u32 {
        let at = self.alloc(24, 4);
        self.wstr(at, id);
        self.wstr(at + 8, version);
        let list = self.alloc(12 * eps.len(), 4);
        for (i, (k, id)) in eps.iter().enumerate() {
            let e = list + 12 * i as u32;
            self.w8(e, *k as u8);
            self.wstr(e + 4, id);
        }
        self.wlist(at + 16, (list, eps.len() as u32));
        at
    }

    /// `result<_, string>` with the error case.
    fn err(&mut self, size: usize, msg: &str) -> u32 {
        let at = self.alloc(size, 4);
        self.w8(at, 1);
        self.wstr(at + 4, msg);
        at
    }

    /// `result<page, string>` (vfs and search share the layout): entries of
    /// (name or path, dir, size, mtime), then the next cursor.
    fn page(&mut self, items: &[(&str, bool, u64, u64)], next: Option<&str>) -> u32 {
        let list = self.alloc(32 * items.len(), 8);
        for (i, (name, dir, size, mtime)) in items.iter().enumerate() {
            let e = list + 32 * i as u32;
            self.wstr(e, name);
            self.w8(e + 8, *dir as u8);
            self.w64(e + 16, *size);
            self.w64(e + 24, *mtime);
        }
        let at = self.alloc(24, 4);
        self.wlist(at + 4, (list, items.len() as u32));
        if let Some(n) = next {
            self.w8(at + 12, 1);
            self.wstr(at + 16, n);
        }
        at
    }

    /// `result<list<u8>, string>`.
    fn data(&mut self, d: &[u8]) -> u32 {
        let at = self.alloc(12, 4);
        let d = self.bytes(d);
        self.wlist(at + 4, d);
        at
    }

    /// `result<list<node>, string>` holding one node of every kind.
    fn doc(&mut self) -> u32 {
        let list = self.alloc(20 * 9, 4);
        let node = |i: u32| list + 20 * i;
        for (i, text) in ["Heading", "plain", "dim", "mono"].iter().enumerate() {
            self.w8(node(i as u32), i as u8);
            self.wstr(node(i as u32) + 4, text);
        }
        self.w8(node(4), 4);
        self.wstr(node(4) + 4, "Size");
        self.wstr(node(4) + 12, "2 x 1");
        self.w8(node(5), 5);
        self.w32(node(5) + 4, 2);
        self.w32(node(5) + 8, 1);
        let px = self.bytes(&[1, 2, 3, 4, 5, 6, 7, 8]);
        self.wlist(node(5) + 12, px);
        self.w8(node(6), 6);
        self.wstr(node(6) + 4, "Open");
        self.wstr(node(6) + 12, "test.fixture.run");
        self.w8(node(7), 7);
        self.w8(node(8), 8);
        let at = self.alloc(12, 4);
        self.wlist(at + 4, (list, 9));
        at
    }

    /// `result<option<string>, string>` with a message.
    fn message(&mut self, msg: &str) -> u32 {
        let at = self.alloc(16, 4);
        self.w8(at + 4, 1);
        self.wstr(at + 8, msg);
        at
    }

    fn wat(&self) -> String {
        let esc: String = self.b.iter().map(|b| format!("\\{b:02x}")).collect();
        format!("(data (i32.const {}) \"{esc}\")", self.base)
    }
}

const COLUMNS: [&str; 9] = ["echo", "len", "hang", "grow", "read", "write", "short", "trap", "caps"];

fn entrypoints(full: bool) -> Vec<(Kind, &'static str)> {
    let mut eps: Vec<(Kind, &str)> = COLUMNS.iter().map(|c| (Kind::Column, *c)).collect();
    if full {
        eps.extend([(Kind::Previewer, "doc"), (Kind::Previewer, "fail"), (Kind::Vfs, "zip"), (Kind::Search, "find"), (Kind::Command, "run")]);
    }
    eps
}

/// The guest as WebAssembly text. `full` adds the previewer, vfs, search and
/// command exports (world `plugin`); without it, it is a `column-plugin`.
fn guest_wat(id: &str, full: bool) -> String {
    let mut img = Image { base: 1024, b: Vec::new() };
    let info = img.info(id, "1.0.0", &entrypoints(full));
    let column = r#"
  (import "tsubame:plugin/host@0.1.0" "read-file" (func $read (param i32 i32 i64 i32 i32)))
  (import "tsubame:plugin/host@0.1.0" "write-file" (func $write (param i32 i32 i32 i32 i32 i32)))
  (import "tsubame:plugin/host@0.1.0" "get-capabilities" (func $caps (param i32)))
  (memory (export "memory") 2)
  (global $heap (mut i32) (i32.const 65536))
  (data (i32.const 300) "hi")

  ;; A bump allocator. Every export resets it in its post-return.
  (func $alloc (param $size i32) (param $align i32) (result i32)
    (local $p i32) (local $end i32) (local $have i32)
    (local.set $p (i32.and
      (i32.add (global.get $heap) (i32.sub (local.get $align) (i32.const 1)))
      (i32.xor (i32.sub (local.get $align) (i32.const 1)) (i32.const -1))))
    (local.set $end (i32.add (local.get $p) (local.get $size)))
    (local.set $have (i32.shl (memory.size) (i32.const 16)))
    (if (i32.gt_u (local.get $end) (local.get $have))
      (then
        (if (i32.eq
              (memory.grow (i32.add (i32.shr_u (i32.sub (local.get $end) (local.get $have)) (i32.const 16)) (i32.const 1)))
              (i32.const -1))
          (then (unreachable)))))
    (global.set $heap (local.get $end))
    (local.get $p))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (call $alloc (local.get 3) (local.get 2)))
  (func $reset (param i32) (global.set $heap (i32.const 65536)))

  ;; cell layout: tag u8 at 0, payload at 8 (string ptr,len or a 64-bit number)
  (func $int (param $c i32) (param $v i64)
    (i32.store8 (local.get $c) (i32.const 2))
    (i64.store offset=8 (local.get $c) (local.get $v)))

  (func $values (export "tsubame:plugin/column@0.1.0#values")
        (param $cp i32) (param $cl i32) (param $pp i32) (param $n i32) (result i32)
    (local $cells i32) (local $i i32) (local $c i32) (local $sp i32) (local $sl i32)
    (local $mode i32) (local $ret i32) (local $count i32)
    (local.set $mode (i32.load8_u (local.get $cp)))
    ;; h: never returns
    (if (i32.eq (local.get $mode) (i32.const 104)) (then (loop $spin (br $spin))))
    ;; t: traps
    (if (i32.eq (local.get $mode) (i32.const 116)) (then (unreachable)))
    ;; g: grows memory 1 MiB at a time until the host says no, then traps
    (if (i32.eq (local.get $mode) (i32.const 103))
      (then
        (loop $more (br_if $more (i32.ne (memory.grow (i32.const 16)) (i32.const -1))))
        (unreachable)))
    (local.set $count (local.get $n))
    ;; s: one value too few
    (if (i32.eq (local.get $mode) (i32.const 115))
      (then (local.set $count (i32.sub (local.get $n) (i32.const 1)))))
    (local.set $cells (call $alloc (i32.mul (local.get $n) (i32.const 16)) (i32.const 8)))
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
        (local.set $c (i32.add (local.get $cells) (i32.mul (local.get $i) (i32.const 16))))
        (local.set $sp (i32.load (i32.add (local.get $pp) (i32.mul (local.get $i) (i32.const 8)))))
        (local.set $sl (i32.load offset=4 (i32.add (local.get $pp) (i32.mul (local.get $i) (i32.const 8)))))
        (block $cell
          ;; l: the path's length as a number
          (if (i32.eq (local.get $mode) (i32.const 108))
            (then (call $int (local.get $c) (i64.extend_i32_u (local.get $sl))) (br $cell)))
          ;; c: the timeout the host reports in `get-capabilities`
          (if (i32.eq (local.get $mode) (i32.const 99))
            (then
              (call $caps (i32.const 200))
              (call $int (local.get $c) (i64.extend_i32_u (i32.load (i32.const 208))))
              (br $cell)))
          ;; r: the first 16 bytes of the file as text, or -(1 + error code)
          (if (i32.eq (local.get $mode) (i32.const 114))
            (then
              (call $read (local.get $sp) (local.get $sl) (i64.const 0) (i32.const 16) (i32.const 256))
              (if (i32.eqz (i32.load8_u (i32.const 256)))
                (then
                  (i32.store8 (local.get $c) (i32.const 1))
                  (i32.store offset=8 (local.get $c) (i32.load (i32.const 260)))
                  (i32.store offset=12 (local.get $c) (i32.load (i32.const 264))))
                (else
                  (call $int (local.get $c) (i64.sub (i64.const -1) (i64.extend_i32_u (i32.load8_u (i32.const 260)))))))
              (br $cell)))
          ;; w: writes "hi" to the path; 0, or -(1 + error code)
          (if (i32.eq (local.get $mode) (i32.const 119))
            (then
              (call $write (local.get $sp) (local.get $sl) (i32.const 300) (i32.const 2) (i32.const 0) (i32.const 280))
              (if (i32.eqz (i32.load8_u (i32.const 280)))
                (then (call $int (local.get $c) (i64.const 0)))
                (else
                  (call $int (local.get $c) (i64.sub (i64.const -1) (i64.extend_i32_u (i32.load8_u (i32.const 281)))))))
              (br $cell)))
          ;; anything else: the path itself as text
          (i32.store8 (local.get $c) (i32.const 1))
          (i32.store offset=8 (local.get $c) (local.get $sp))
          (i32.store offset=12 (local.get $c) (local.get $sl)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $next)))
    (local.set $ret (call $alloc (i32.const 8) (i32.const 4)))
    (i32.store (local.get $ret) (local.get $cells))
    (i32.store offset=4 (local.get $ret) (local.get $count))
    (local.get $ret))
  (export "cabi_post_tsubame:plugin/column@0.1.0#values" (func $reset))
"#;
    let mut wat = format!("(module\n{column}\n  (func (export \"tsubame:plugin/meta@0.1.0#describe\") (result i32) (i32.const {info}))\n");
    if full {
        let doc = img.doc();
        let doc_err = img.err(12, "no preview for this file");
        let vfs1 = img.page(&[("docs", true, 0, 11), ("a.txt", false, 5, 12)], Some("page-2"));
        let vfs2 = img.page(&[("b.txt", false, 7, 13)], None);
        let read = img.data(b"hello");
        let read_end = img.data(b"");
        let hits1 = img.page(&[("C:\\found\\one.txt", false, 1, 21), ("C:\\found\\two", true, 0, 22)], Some("more"));
        let hits2 = img.page(&[("C:\\found\\three.txt", false, 3, 23)], None);
        let done = img.message("done");
        wat.push_str(&format!(
            r#"
  ;; previewer: the id `fail` returns an error, anything else the fixed document
  (func (export "tsubame:plugin/previewer@0.1.0#preview") (param i32 i32 i32 i32 i64 i32) (result i32)
    (if (result i32) (i32.eq (i32.load8_u (local.get 0)) (i32.const 102))
      (then (i32.const {doc_err})) (else (i32.const {doc}))))
  (export "cabi_post_tsubame:plugin/previewer@0.1.0#preview" (func $reset))
  ;; vfs: two pages; the second is the one asked for with a cursor
  (func (export "tsubame:plugin/vfs@0.1.0#list-dir") (param i32 i32 i32 i32 i32 i32 i32) (result i32)
    (if (result i32) (local.get 4) (then (i32.const {vfs2})) (else (i32.const {vfs1}))))
  (export "cabi_post_tsubame:plugin/vfs@0.1.0#list-dir" (func $reset))
  ;; vfs read: five bytes at offset 0, nothing after
  (func (export "tsubame:plugin/vfs@0.1.0#read") (param i32 i32 i32 i32 i64 i32) (result i32)
    (if (result i32) (i64.eqz (local.get 4)) (then (i32.const {read})) (else (i32.const {read_end}))))
  (export "cabi_post_tsubame:plugin/vfs@0.1.0#read" (func $reset))
  ;; search: two pages
  (func (export "tsubame:plugin/search@0.1.0#find") (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
    (if (result i32) (local.get 7) (then (i32.const {hits2})) (else (i32.const {hits1}))))
  (export "cabi_post_tsubame:plugin/search@0.1.0#find" (func $reset))
  (func (export "tsubame:plugin/command@0.1.0#run") (param i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
    (i32.const {done}))
  (export "cabi_post_tsubame:plugin/command@0.1.0#run" (func $reset))
"#
        ));
    }
    wat.push_str(&format!("  {}\n)\n", img.wat()));
    wat
}

/// Core module text to a component for `world`, checked against the WIT file.
fn componentize(wat: &str, world: &str) -> Vec<u8> {
    let mut module = wat::parse_str(wat).expect("the guest is valid wat");
    let mut resolve = wit_parser::Resolve::default();
    let (pkg, _) = resolve.push_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("wit")).expect("the wit folder parses");
    let world = resolve.select_world(&[pkg], Some(world)).expect("the world exists");
    wit_component::embed_component_metadata(&mut module, &resolve, world, wit_component::StringEncoding::UTF8, false).expect("metadata");
    wit_component::ComponentEncoder::default().validate(true).module(&module).expect("the guest matches the world").encode().expect("a component")
}

/// What the install step does: compile with the host's engine settings.
fn precompile(component: &[u8]) -> Vec<u8> {
    let engine = wasmtime::Engine::new(&tsubame_plughost::host::engine_config()).expect("engine");
    engine.precompile_component(component).expect("the component compiles")
}

fn full_cwasm() -> &'static [u8] {
    static C: OnceLock<Vec<u8>> = OnceLock::new();
    C.get_or_init(|| precompile(&componentize(&guest_wat("test.fixture", true), "plugin")))
}

fn column_cwasm() -> &'static [u8] {
    static C: OnceLock<Vec<u8>> = OnceLock::new();
    C.get_or_init(|| precompile(&componentize(&guest_wat("test.fixture", false), "column-plugin")))
}

// ---- helpers ---------------------------------------------------------------------------------

/// A folder that is removed when the test ends.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("tsubame-plughost-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_str().unwrap().to_string()
    }
    fn write(&self, rel: &str, data: &str) -> String {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, data).unwrap();
        p.to_str().unwrap().to_string()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn options() -> Options {
    // Generous start-up: the first start of a debug host on a busy machine is slow.
    Options { start_timeout: Duration::from_secs(30), ..Options::default() }
}

fn load_for(temp: &Temp, cwasm: &[u8], caps: Capabilities, full: bool) -> Load {
    let file = temp.0.join("fixture.cwasm");
    std::fs::write(&file, cwasm).unwrap();
    Load {
        plugin_id: "test.fixture".into(),
        version: "1.0.0".into(),
        component: file.to_str().unwrap().into(),
        sha256: sha256::sha256(cwasm),
        caps,
        entrypoints: entrypoints(full).into_iter().map(|(k, id)| (k, id.to_string())).collect(),
    }
}

fn plugin(temp: &Temp, caps: Capabilities) -> Plugin {
    Plugin::new(load_for(temp, full_cwasm(), caps, true), ProcessLauncher::new(HOST_EXE), options())
}

fn caps(paths: Vec<PathGrant>) -> Capabilities {
    Capabilities { paths, ..Capabilities::default() }
}

fn grant(scope: Scope, access: Access) -> PathGrant {
    PathGrant { scope, access }
}

fn column(p: &Plugin, column: &str, paths: &[&str]) -> Result<Vec<Cell>, CallError> {
    let mut out = Vec::new();
    p.column_values(column, paths, &mut out).map(|()| out)
}

const DENIED: Cell = Cell::Int(-1);
const NOT_FOUND: Cell = Cell::Int(-2);
const INVALID_PATH: Cell = Cell::Int(-3);

// ---- lifecycle and batches ------------------------------------------------------------------

#[test]
fn lazy_start_round_trip_and_clean_shutdown() {
    let t = Temp::new("lazy");
    let p = plugin(&t, Capabilities::default());
    assert_eq!(p.status(), Status::Idle);
    assert_eq!(p.stats(), Stats::default(), "creating a plugin starts nothing");

    assert_eq!(column(&p, "echo", &["C:\\a\\b.txt", "C:\\ü\\日本.txt"]).unwrap(), [Cell::Text("C:\\a\\b.txt".into()), Cell::Text("C:\\ü\\日本.txt".into())]);
    assert_eq!(column(&p, "len", &["C:\\a\\b.txt", ""]).unwrap(), [Cell::Int(10), Cell::Int(0)]);
    assert_eq!(p.status(), Status::Running);
    assert_eq!(p.stats(), Stats { spawns: 1, kills: 0, calls: 2, faults: 0 });

    p.shutdown();
    assert_eq!(p.status(), Status::Idle);
    assert_eq!(p.stats().kills, 0, "the host exits by itself when asked");
    assert_eq!(column(&p, "echo", &["C:\\again"]).unwrap(), [Cell::Text("C:\\again".into())]);
    assert_eq!(p.stats().spawns, 2);
    assert!(p.stop_if_idle(Duration::ZERO));
    assert_eq!(p.status(), Status::Idle);
}

#[test]
fn a_batch_over_2000_paths_goes_out_in_chunks() {
    let t = Temp::new("chunks");
    let p = plugin(&t, Capabilities::default());
    let owned: Vec<String> = (0..5_001).map(|i| format!("C:\\Users\\alex\\Documents\\folder_{:03}\\file_{i:06}.txt", i % 97)).collect();
    let paths: Vec<&str> = owned.iter().map(String::as_str).collect();
    let cells = column(&p, "echo", &paths).unwrap();
    assert_eq!(cells.len(), 5_001);
    assert!(cells.iter().zip(&owned).all(|(c, p)| *c == Cell::Text(p.clone())), "every value, in order");
    assert_eq!(p.stats(), Stats { spawns: 1, kills: 0, calls: 3, faults: 0 }, "2000 + 2000 + 1001");
    // exactly at the cap: one call
    assert_eq!(column(&p, "len", &paths[..2_000]).unwrap().len(), 2_000);
    assert_eq!(p.stats().calls, 4);
}

// ---- time and memory -----------------------------------------------------------------------

#[test]
fn a_guest_that_never_returns_is_interrupted() {
    let t = Temp::new("hang");
    // A long grace: if the host did not interrupt the guest itself, this test would
    // see a killed host after 10 s, not an answer shortly after the deadline.
    let load = load_for(&t, full_cwasm(), Capabilities { timeout_ms: 300, ..Capabilities::default() }, true);
    let p = Plugin::new(load, ProcessLauncher::new(HOST_EXE), Options { grace: Duration::from_secs(10), ..options() });
    column(&p, "echo", &["C:\\warm"]).unwrap();
    let start = Instant::now();
    assert_eq!(column(&p, "hang", &["C:\\a"]), Err(CallError::Timeout));
    let took = start.elapsed();
    assert!(took >= Duration::from_millis(290) && took < Duration::from_secs(5), "interrupted at the deadline, not killed after the grace: {took:?}");
    // The host interrupted the guest itself: same process, fresh instance, caller carries on.
    assert_eq!(p.status(), Status::Running);
    assert_eq!(column(&p, "echo", &["C:\\after"]).unwrap(), [Cell::Text("C:\\after".into())]);
    assert_eq!(p.stats(), Stats { spawns: 1, kills: 0, calls: 3, faults: 1 });
}

#[test]
fn a_host_that_stops_answering_is_killed() {
    let t = Temp::new("kill");
    let mut launcher = ProcessLauncher::new(MOCK_EXE);
    launcher.args = vec!["mock".into(), "hang".into()];
    let load = load_for(&t, b"unused", Capabilities { timeout_ms: 200, ..Capabilities::default() }, false);
    let p = Plugin::new(load, launcher, options());
    let start = Instant::now();
    assert_eq!(column(&p, "echo", &["C:\\a"]), Err(CallError::Timeout));
    let took = start.elapsed();
    // 200 ms timeout + 250 ms grace, plus starting the process
    assert!(took >= Duration::from_millis(450), "{took:?}");
    assert_eq!(p.status(), Status::Idle, "the hung process is gone");
    assert_eq!(p.stats(), Stats { spawns: 1, kills: 1, calls: 1, faults: 1 });
    // the caller is not stuck: it can go again, and three in a row switch the plugin off
    assert_eq!(column(&p, "echo", &["C:\\a"]), Err(CallError::Timeout));
    assert_eq!(column(&p, "echo", &["C:\\a"]), Err(CallError::Timeout));
    assert!(matches!(p.status(), Status::Faulted(_)));
    assert!(matches!(column(&p, "echo", &["C:\\a"]), Err(CallError::Faulted(_))));
    assert_eq!(p.stats(), Stats { spawns: 3, kills: 3, calls: 3, faults: 3 });
}

#[test]
fn a_host_that_says_nothing_or_dies_is_handled() {
    let t = Temp::new("silent");
    let mut launcher = ProcessLauncher::new(MOCK_EXE);
    launcher.args = vec!["mock".into(), "silent".into()];
    let load = load_for(&t, b"unused", Capabilities::default(), false);
    let p = Plugin::new(load.clone(), launcher, Options { start_timeout: Duration::from_millis(1500), ..Options::default() });
    assert!(matches!(column(&p, "echo", &["C:\\a"]), Err(CallError::Spawn(_))));
    assert_eq!(p.stats().kills, 1);

    let mut launcher = ProcessLauncher::new(MOCK_EXE);
    launcher.args = vec!["mock".into(), "exit".into()];
    let p = Plugin::new(load, launcher, options());
    assert_eq!(column(&p, "echo", &["C:\\a"]), Err(CallError::Crashed));
    assert_eq!(p.status(), Status::Idle);
}

#[test]
fn memory_is_capped() {
    let t = Temp::new("memory");
    let p = plugin(&t, Capabilities { max_memory_mb: 8, ..Capabilities::default() });
    assert_eq!(column(&p, "grow", &["C:\\a"]), Err(CallError::Memory));
    // the instance is replaced; a normal call fits in 8 MB
    assert_eq!(column(&p, "echo", &["C:\\a"]).unwrap().len(), 1);
    assert_eq!(p.stats(), Stats { spawns: 1, kills: 0, calls: 2, faults: 1 });

    // 2,000 long paths need more than 1 MB of guest memory: over the limit
    let t = Temp::new("memory-small");
    let p = plugin(&t, Capabilities { max_memory_mb: 1, ..Capabilities::default() });
    let long = format!("C:\\{}", "x".repeat(900));
    let paths = vec![long.as_str(); 2_000];
    assert!(matches!(column(&p, "echo", &paths), Err(CallError::Memory | CallError::Trap(_))));
    assert_eq!(column(&p, "echo", &paths[..10]).unwrap().len(), 10);
}

#[test]
fn traps_and_bad_answers_are_errors_and_faults_add_up() {
    let t = Temp::new("trap");
    let p = plugin(&t, Capabilities::default());
    assert!(matches!(column(&p, "trap", &["C:\\a"]), Err(CallError::Trap(_))));
    assert_eq!(column(&p, "echo", &["C:\\a"]).unwrap().len(), 1, "a good call clears the count");
    match column(&p, "short", &["C:\\a", "C:\\b"]) {
        Err(CallError::BadResult(m)) => assert_eq!(m, "1 values for 2 paths"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(column(&p, "nope", &["C:\\a"]), Err(CallError::Unsupported(_))), "not a declared column");
    assert_eq!(p.status(), Status::Running);
    for _ in 0..2 {
        assert!(matches!(column(&p, "trap", &["C:\\a"]), Err(CallError::Trap(_))));
    }
    assert!(matches!(p.status(), Status::Faulted(_)), "three faults in a row");
    assert!(matches!(column(&p, "echo", &["C:\\a"]), Err(CallError::Faulted(_))));
    assert_eq!(p.stats().spawns, 1);
    p.reset();
    assert_eq!(column(&p, "echo", &["C:\\a"]).unwrap().len(), 1);
    assert_eq!(p.stats().spawns, 2);
}

// ---- loading ---------------------------------------------------------------------------------

#[test]
fn only_the_installed_cwasm_is_loaded() {
    let t = Temp::new("hash");
    // the hash on record is for other bytes
    let mut load = load_for(&t, full_cwasm(), Capabilities::default(), true);
    load.sha256[0] ^= 1;
    let p = Plugin::new(load, ProcessLauncher::new(HOST_EXE), options());
    match column(&p, "echo", &["C:\\a"]) {
        Err(CallError::Load(m)) => assert!(m.contains("hash does not match"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(p.status(), Status::Idle);

    // the file changed after install
    let load = load_for(&t, full_cwasm(), Capabilities::default(), true);
    let mut tampered = full_cwasm().to_vec();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    std::fs::write(&load.component, &tampered).unwrap();
    let p = Plugin::new(load, ProcessLauncher::new(HOST_EXE), options());
    assert!(matches!(column(&p, "echo", &["C:\\a"]), Err(CallError::Load(m)) if m.contains("hash does not match")));

    // a file that is not a compiled component, even with a matching hash
    let load = load_for(&t, b"this is not a compiled component", Capabilities::default(), true);
    let p = Plugin::new(load, ProcessLauncher::new(HOST_EXE), options());
    assert!(matches!(column(&p, "echo", &["C:\\a"]), Err(CallError::Load(m)) if m.contains("not a component this host can run")));

    // a plain .wasm component: the host has no compiler
    let wasm = componentize(&guest_wat("test.fixture", true), "plugin");
    let mut load = load_for(&t, &wasm, Capabilities::default(), true);
    assert!(matches!(column(&Plugin::new(load.clone(), ProcessLauncher::new(HOST_EXE), options()), "echo", &["C:\\a"]), Err(CallError::Load(_))));
    // and a file that is not named .cwasm is refused before it is read
    load.component = t.write("plugin.wasm", "x");
    let p = Plugin::new(load, ProcessLauncher::new(HOST_EXE), options());
    assert!(matches!(column(&p, "echo", &["C:\\a"]), Err(CallError::Load(m)) if m.contains(".cwasm")));

    // a missing file
    let mut load = load_for(&t, full_cwasm(), Capabilities::default(), true);
    load.component = t.path("gone.cwasm");
    let p = Plugin::new(load, ProcessLauncher::new(HOST_EXE), options());
    assert!(matches!(column(&p, "echo", &["C:\\a"]), Err(CallError::Load(m)) if m.contains("can not read")));
}

#[test]
fn the_component_must_be_who_the_manifest_says() {
    let t = Temp::new("identity");
    let other = precompile(&componentize(&guest_wat("someone.else", true), "plugin"));
    let p = Plugin::new(load_for(&t, &other, Capabilities::default(), true), ProcessLauncher::new(HOST_EXE), options());
    match column(&p, "echo", &["C:\\a"]) {
        Err(CallError::Load(m)) => assert!(m.contains("someone.else") && m.contains("test.fixture"), "{m}"),
        other => panic!("{other:?}"),
    }

    let mut load = load_for(&t, full_cwasm(), Capabilities::default(), true);
    load.version = "2.0.0".into();
    assert!(matches!(column(&Plugin::new(load, ProcessLauncher::new(HOST_EXE), options()), "echo", &["C:\\a"]), Err(CallError::Load(_))));

    // an entry point the component does not list
    let mut load = load_for(&t, full_cwasm(), Capabilities::default(), true);
    load.entrypoints.push((Kind::Column, "extra".into()));
    assert!(matches!(column(&Plugin::new(load, ProcessLauncher::new(HOST_EXE), options()), "echo", &["C:\\a"]), Err(CallError::Load(m)) if m.contains("extra")));
}

#[test]
fn a_plugin_exports_only_what_it_declares() {
    let t = Temp::new("partial");
    // a column-only component loads and runs
    let p = Plugin::new(load_for(&t, column_cwasm(), Capabilities::default(), false), ProcessLauncher::new(HOST_EXE), options());
    assert_eq!(column(&p, "len", &["C:\\abc"]).unwrap(), [Cell::Int(6)]);
    let mut doc = Vec::new();
    assert!(matches!(p.preview("doc", "C:\\a", Limits { max_bytes: 1, max_px: 1 }, &mut doc), Err(CallError::Unsupported(_))));

    // but a manifest that declares a previewer it does not export is refused
    let mut load = load_for(&t, column_cwasm(), Capabilities::default(), false);
    load.entrypoints.push((Kind::Previewer, "doc".into()));
    let p = Plugin::new(load, ProcessLauncher::new(HOST_EXE), options());
    match column(&p, "len", &["C:\\abc"]) {
        Err(CallError::Load(m)) => assert!(m.contains("declares a previewer"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn limits_over_the_caps_are_refused_by_the_host() {
    let t = Temp::new("caps-range");
    for bad in [Capabilities { max_memory_mb: 4096, ..Capabilities::default() }, Capabilities { timeout_ms: 3_600_000, ..Capabilities::default() }] {
        let p = Plugin::new(load_for(&t, full_cwasm(), bad, true), ProcessLauncher::new(HOST_EXE), Options { grace: Duration::from_secs(1), ..options() });
        assert!(matches!(column(&p, "echo", &["C:\\a"]), Err(CallError::Load(m)) if m.contains("outside")));
    }
}

// ---- capabilities ---------------------------------------------------------------------------

#[test]
fn files_are_denied_by_default() {
    let t = Temp::new("deny");
    let file = t.write("in\\note.txt", "secret");
    let p = plugin(&t, Capabilities::default());
    assert_eq!(column(&p, "read", &[&file]).unwrap(), [DENIED]);
    assert_eq!(column(&p, "write", &[&t.path("in\\new.txt")]).unwrap(), [DENIED]);
    assert!(!Path::new(&t.path("in\\new.txt")).exists());
    // the guest can ask what it was granted
    assert_eq!(column(&p, "caps", &["C:\\a"]).unwrap(), [Cell::Int(2000)]);
}

#[test]
fn an_input_grant_covers_the_paths_of_the_call_only() {
    let t = Temp::new("input");
    let a = t.write("a.txt", "alpha file contents");
    let b = t.write("b.txt", "beta");
    let p = plugin(&t, caps(vec![grant(Scope::Input, Access::Read)]));
    assert_eq!(column(&p, "read", &[&a, &b]).unwrap(), [Cell::Text("alpha file conte".into()), Cell::Text("beta".into())]);
    assert_eq!(column(&p, "read", &[&t.path("missing.txt")]).unwrap(), [NOT_FOUND]);
    assert_eq!(column(&p, "read", &[&format!("{}\\..\\{}", t.path("sub"), "a.txt")]).unwrap(), [INVALID_PATH]);
    assert_eq!(column(&p, "read", &[&format!("{a}:stream")]).unwrap(), [INVALID_PATH]);
    assert_eq!(column(&p, "read", &["relative.txt"]).unwrap(), [INVALID_PATH]);
    // read access is not write access
    assert_eq!(column(&p, "write", &[&a]).unwrap(), [DENIED]);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha file contents");
}

#[test]
fn a_prefix_grant_covers_its_folder_and_nothing_else() {
    let t = Temp::new("prefix");
    let inside = t.write("granted\\sub\\in.txt", "inside");
    let outside = t.write("other\\out.txt", "outside");
    let sibling = t.write("granted-not\\x.txt", "sibling");
    let root = tsubame_plughost::caps::normalize(&t.path("granted")).unwrap();
    let p = plugin(&t, caps(vec![grant(Scope::Prefix(root.clone()), Access::Read)]));
    assert_eq!(column(&p, "read", &[&inside, &outside, &sibling]).unwrap(), [Cell::Text("inside".into()), DENIED, DENIED]);
    assert_eq!(column(&p, "read", &[&inside.to_uppercase()]).unwrap(), [Cell::Text("inside".into())], "case does not matter");
    assert_eq!(column(&p, "write", &[&t.path("granted\\new.txt")]).unwrap(), [DENIED], "read only");

    // read-write: the guest can create and replace files in the folder, not next to it
    let t = Temp::new("prefix-rw");
    let outside = t.write("other\\out.txt", "outside");
    std::fs::create_dir_all(t.path("data")).unwrap();
    let root = tsubame_plughost::caps::normalize(&t.path("data")).unwrap();
    let p = plugin(&t, caps(vec![grant(Scope::Prefix(root), Access::ReadWrite)]));
    let new = t.path("data\\new.txt");
    assert_eq!(column(&p, "write", &[&new, &outside, &t.path("data\\nodir\\x.txt")]).unwrap(), [Cell::Int(0), DENIED, NOT_FOUND]);
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "hi");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside");
    assert_eq!(column(&p, "read", &[&new]).unwrap(), [Cell::Text("hi".into())]);
}

#[cfg(windows)]
#[test]
fn a_junction_does_not_lead_out_of_a_grant() {
    use std::os::windows::process::CommandExt;
    let t = Temp::new("junction");
    let secret = t.write("outside\\secret.txt", "secret");
    t.write("granted\\ok.txt", "fine");
    let link = t.path("granted\\link");
    let made = std::process::Command::new("cmd").args(["/c", "mklink", "/J", &link, &t.path("outside")]).creation_flags(0x0800_0000).output();
    if !made.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: could not create a junction");
        return;
    }
    // through the junction the file is readable by the OS ...
    assert_eq!(std::fs::read_to_string(format!("{link}\\secret.txt")).unwrap(), "secret");
    let root = tsubame_plughost::caps::normalize(&t.path("granted")).unwrap();
    let p = plugin(&t, caps(vec![grant(Scope::Prefix(root), Access::ReadWrite)]));
    // ... but not by the plugin: the resolved path is outside the grant
    let through = format!("{link}\\secret.txt");
    assert_eq!(column(&p, "read", &[&through, &t.path("granted\\ok.txt")]).unwrap(), [DENIED, Cell::Text("fine".into())]);
    assert_eq!(column(&p, "write", &[&through, &format!("{link}\\new.txt")]).unwrap(), [DENIED, DENIED]);
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "secret");
    assert!(!Path::new(&t.path("outside\\new.txt")).exists());
    let _ = std::fs::remove_dir(&link);
}

#[test]
fn an_any_grant_reads_everything_but_still_no_escapes() {
    let t = Temp::new("any");
    let f = t.write("x\\f.txt", "anywhere");
    let p = plugin(&t, caps(vec![grant(Scope::Any, Access::Read)]));
    assert_eq!(column(&p, "read", &[&f]).unwrap(), [Cell::Text("anywhere".into())]);
    assert_eq!(column(&p, "read", &["\\\\.\\PhysicalDrive0", "C:\\Windows\\..\\x", "NUL"]).unwrap(), [INVALID_PATH, INVALID_PATH, INVALID_PATH]);
    assert_eq!(column(&p, "write", &[&f]).unwrap(), [DENIED]);
}

// ---- the other entry points, through the adapters ----------------------------------------------

#[test]
fn previewer_vfs_search_and_command() {
    let t = Temp::new("all");
    let p = Arc::new(plugin(&t, Capabilities::default()));
    let decl = |id: &str, ext: &[&str]| ProviderDecl { id: id.into(), title: id.into(), extensions: ext.iter().map(|e| e.to_string()).collect() };

    // previewer
    let pv = WasmPreviewer::new(p.clone(), &decl("doc", &["png", "jpg"]));
    assert!(pv.accepts("png") && pv.accepts("JPG") && !pv.accepts("txt"));
    let mut doc = Doc::default();
    pv.preview("C:\\a.png", Limits { max_bytes: 1 << 20, max_px: 64 }, &mut doc).unwrap();
    assert_eq!(
        doc.nodes,
        [
            Node::Heading("Heading".into()),
            Node::Text { text: "plain".into(), dim: false },
            Node::Text { text: "dim".into(), dim: true },
            Node::Mono("mono".into()),
            Node::Pair("Size".into(), "2 x 1".into()),
            Node::Image { w: 2, h: 1, bgra: vec![1, 2, 3, 4, 5, 6, 7, 8] },
            Node::Button { label: "Open".into(), command: "test.fixture.run".into() },
            Node::Indent,
            Node::Outdent,
        ]
    );
    // an image over the caller's pixel limit is a bad answer, and the doc is left alone
    let mut small = Doc::default();
    let e = pv.preview("C:\\a.png", Limits { max_bytes: 1 << 20, max_px: 1 }, &mut small).unwrap_err();
    assert!(e.contains("over the 1 px limit"), "{e}");
    assert!(small.nodes.is_empty());
    // the plugin's own error comes through as text and is not a fault
    let failing = WasmPreviewer::new(p.clone(), &decl("fail", &["png"]));
    for _ in 0..5 {
        assert_eq!(failing.preview("C:\\a.png", Limits { max_bytes: 1, max_px: 64 }, &mut small).unwrap_err(), "no preview for this file");
    }
    assert_eq!(p.status(), Status::Running);

    // vfs: two pages, then a read
    let vfs = WasmVfs::new(p.clone(), &decl("zip", &["zip"]));
    assert!(vfs.accepts("C:\\a.zip") && vfs.accepts("C:\\a.ZIP\\inner\\x.txt") && !vfs.accepts("C:\\zip\\a.txt") && !vfs.accepts("C:\\.zip"));
    let mut names = Vec::new();
    vfs.list("C:\\a.zip", &mut |e| {
        names.extend(e.iter().map(|e| (e.name.clone(), e.dir, e.size, e.mtime)));
        true
    })
    .unwrap();
    assert_eq!(names, [("docs".to_string(), true, 0, 11), ("a.txt".to_string(), false, 5, 12), ("b.txt".to_string(), false, 7, 13)]);
    let mut data = Vec::new();
    vfs.read("C:\\a.zip\\a.txt", &mut data).unwrap();
    assert_eq!(data, b"hello");

    // search: two pages, and the limit
    let search = WasmSearch::new(p.clone(), &decl("find", &[]));
    let mut hits = Vec::new();
    search
        .search(&Query { text: "x", root: "", limit: 100 }, &mut |h| {
            hits.extend(h.iter().map(|h| h.path.clone()));
            true
        })
        .unwrap();
    assert_eq!(hits, ["C:\\found\\one.txt", "C:\\found\\two", "C:\\found\\three.txt"]);
    let mut hits = Vec::new();
    search
        .search(&Query { text: "x", root: "", limit: 1 }, &mut |h| {
            hits.extend(h.iter().map(|h| h.path.clone()));
            true
        })
        .unwrap();
    assert_eq!(hits, ["C:\\found\\one.txt"]);

    // command
    assert_eq!(p.run_command("run", "C:\\folder", &["C:\\folder\\a"]).unwrap().as_deref(), Some("done"));
    assert!(matches!(p.run_command("other", "C:\\folder", &[]), Err(CallError::Unsupported(_))));
    assert_eq!(p.stats().faults, 1, "only the oversized image counted");
    assert_eq!(p.stats().spawns, 1);
}

#[test]
fn the_column_adapter_caches_and_never_fails_the_caller() {
    let t = Temp::new("adapter");
    let p = Arc::new(plugin(&t, Capabilities::default()));
    let col = WasmColumn::new(p.clone(), "len");
    assert_eq!(col.max_batch(), 2_000);
    let mut out = Vec::new();
    col.values(&["C:\\a", "C:\\bb"], &mut out);
    assert_eq!(out, [Cell::Int(4), Cell::Int(5)]);
    assert_eq!(p.stats().calls, 1);
    // cached: no second call; a new path asks only for itself
    out.clear();
    col.values(&["C:\\bb", "C:\\ccc", "C:\\a"], &mut out);
    assert_eq!(out, [Cell::Int(5), Cell::Int(6), Cell::Int(4)]);
    assert_eq!(p.stats().calls, 2);
    col.invalidate("C:\\a");
    assert_eq!(col.cached(), 2);
    col.clear();
    assert_eq!(col.cached(), 0);

    // a failing column gives empty cells, does not cache them, and the caller is fine
    let bad = WasmColumn::new(p.clone(), "trap");
    out.clear();
    bad.values(&["C:\\a", "C:\\b"], &mut out);
    assert_eq!(out, [Cell::Empty, Cell::Empty]);
    assert_eq!(bad.cached(), 0);
}

// ---- the shipped exe ---------------------------------------------------------------------------

/// The release host must stay small: it is runtime only, no Cranelift
/// (`docs/spike-wasm.md`: 0.86 MB runtime only against 9.3 MB with the compiler).
///
/// Checks `target/release/tsubame-plughost.exe`, or the file named by
/// `TSUBAME_PLUGHOST_RELEASE_EXE`. Build it first:
/// `cargo build --release -p tsubame-plughost`. Without a release build the
/// test has nothing to measure and says so.
#[test]
fn release_exe_is_small() {
    const BUDGET: u64 = 1_250_000;
    let exe = std::env::var_os("TSUBAME_PLUGHOST_RELEASE_EXE").map(PathBuf::from).unwrap_or_else(|| {
        // target/debug/tsubame-plughost.exe -> target/release/tsubame-plughost.exe
        let debug = Path::new(HOST_EXE);
        debug.parent().and_then(Path::parent).map(|t| t.join("release").join(debug.file_name().unwrap())).unwrap_or_default()
    });
    match std::fs::metadata(&exe) {
        Ok(m) => {
            println!("release host exe: {} bytes ({:.2} MB), budget {BUDGET}", m.len(), m.len() as f64 / (1024.0 * 1024.0));
            assert!(
                m.len() <= BUDGET,
                "{} is {} bytes, over the {BUDGET} byte budget. Was it built with `--features compiler`? Rebuild with `cargo build --release -p tsubame-plughost`.",
                exe.display(),
                m.len()
            );
        }
        Err(_) => println!("release host exe not built ({}): size not checked", exe.display()),
    }
}

// ---- security review P5.P3: proofs of concept ---------------------------------------------------
//
// See `docs/security/plughost-review.md`. Each test is `#[ignore]` and **passes
// while the weakness is present**; the name starts with the finding's id.
// Run them: `cargo test -p tsubame-plughost --test host -- --ignored --nocapture ph`.
// The PoCs that need no guest are in `tests/security_poc.rs`.

/// PH-01: with a `*` grant and `network = false` the host opens UNC paths, so
/// the plugin can make this machine talk SMB (or WebDAV) to any server. The
/// PoC stays on the loopback address.
#[cfg(windows)]
#[test]
#[ignore = "PoC PH-01: passes while a `*` grant lets the host open UNC paths"]
fn ph01_any_grant_makes_the_host_open_a_unc_path() {
    let t = Temp::new("poc-unc");
    let c = Capabilities { paths: vec![grant(Scope::Any, Access::Read)], timeout_ms: 20_000, ..Capabilities::default() };
    assert!(!c.network);
    let p = plugin(&t, c);
    let r = column(&p, "read", &["\\\\127.0.0.1\\C$\\Windows\\win.ini"]);
    println!("PH-01: reading \\\\127.0.0.1\\C$\\Windows\\win.ini through the host: {r:?}");
    match r {
        // Any answer but "invalid path" or "denied" means the host went to the SMB redirector.
        Ok(cells) => assert!(cells[0] != INVALID_PATH && cells[0] != DENIED, "{cells:?}"),
        // So does a host that is still waiting for the server.
        Err(e) => assert_eq!(e, CallError::Timeout),
    }
}

/// PH-03: the SHA-256 in `Load` is the only thing between a file on disk and
/// `Component::deserialize`. Whoever can write the `.cwasm` **and** the place
/// the hash is kept gets their image deserialized and run. The PoC only
/// changes a string in the image's data (machine code is just as unchecked).
#[test]
#[ignore = "PoC PH-03: passes while a re-hashed, modified .cwasm is accepted"]
fn ph03_a_modified_cwasm_with_a_matching_hash_is_deserialized_and_run() {
    let t = Temp::new("poc-rehash");
    let mut image = full_cwasm().to_vec();
    let at = image.windows(12).position(|w| w == b"test.fixture").expect("the fixture's id is in the image");
    image[at..at + 12].copy_from_slice(b"evil.fixture");
    // The attacker rewrites the recorded hash as well (here: `load_for` hashes what it is given).
    let mut load = load_for(&t, &image, Capabilities::default(), true);
    load.plugin_id = "evil.fixture".into();
    let p = Plugin::new(load, ProcessLauncher::new(HOST_EXE), options());
    // The modified image loads, describes itself with the modified id and answers calls.
    assert_eq!(column(&p, "echo", &["C:\\a"]).unwrap(), [Cell::Text("C:\\a".into())]);
}

/// PH-04: a dangling file symlink inside a read-write grant. The resolved-path
/// check falls back to the parent folder for a file that "does not exist yet",
/// then the open follows the link and creates the target outside the grant.
#[cfg(windows)]
#[test]
#[ignore = "PoC PH-04: passes while write-file follows a dangling symlink out of the grant"]
fn ph04_a_dangling_symlink_lets_write_file_create_a_file_outside_the_grant() {
    let t = Temp::new("poc-symlink");
    std::fs::create_dir_all(t.path("granted")).unwrap();
    std::fs::create_dir_all(t.path("outside")).unwrap();
    let target = t.path("outside\\created-by-plugin.txt");
    let link = t.path("granted\\notes.txt");
    if let Err(e) = std::os::windows::fs::symlink_file(&target, &link) {
        println!("PH-04 NOT RUN: could not create a symlink ({e}); needs Developer Mode or SeCreateSymbolicLinkPrivilege");
        return;
    }
    let root = tsubame_plughost::caps::normalize(&t.path("granted")).unwrap();
    let p = plugin(&t, caps(vec![grant(Scope::Prefix(root), Access::ReadWrite)]));
    assert!(!Path::new(&target).exists());
    let r = column(&p, "write", &[&link]).unwrap();
    println!("PH-04: write-file through the dangling link: {r:?}; outside file exists: {}", Path::new(&target).exists());
    assert_eq!(r, [Cell::Int(0)]);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hi", "the plugin wrote outside its grant");
}

/// PH-05: check and use are separate steps. A process that swaps what a name
/// points to between the resolved-path check and the open wins. Here the name
/// is a plain file at check time and a symlink to a file outside the grant at
/// open time; one atomic rename does the swap.
#[cfg(windows)]
#[test]
#[ignore = "PoC PH-05: passes while the path check and the open can be raced"]
fn ph05_swapping_a_file_for_a_link_between_check_and_open_reads_outside_the_grant() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let t = Temp::new("poc-race");
    let secret = t.write("outside\\secret.txt", "SECRET-OUTSIDE!!");
    let slot = t.write("granted\\f.txt", "inside");
    let tmp = t.path("swap.tmp");
    if let Err(e) = std::os::windows::fs::symlink_file(&secret, &tmp) {
        println!("PH-05 NOT RUN: could not create a symlink ({e}); needs Developer Mode or SeCreateSymbolicLinkPrivilege");
        return;
    }
    let _ = std::fs::remove_file(&tmp);
    let root = tsubame_plughost::caps::normalize(&t.path("granted")).unwrap();
    let p = plugin(&t, caps(vec![grant(Scope::Prefix(root), Access::Read)]));
    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let (stop, slot, secret, tmp) = (stop.clone(), slot.clone(), secret.clone(), tmp.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                // granted\f.txt becomes a link to the file outside ...
                if std::os::windows::fs::symlink_file(&secret, &tmp).is_ok() {
                    let _ = std::fs::rename(&tmp, &slot);
                }
                // ... and a plain file again.
                if std::fs::write(&tmp, "inside").is_ok() {
                    let _ = std::fs::rename(&tmp, &slot);
                }
            }
        })
    };
    let batch = vec![slot.as_str(); 500];
    let (mut tries, mut leaked, mut denied) = (0usize, 0usize, 0usize);
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end && leaked == 0 {
        match column(&p, "read", &batch) {
            Ok(cells) => {
                tries += cells.len();
                leaked += cells.iter().filter(|c| matches!(c, Cell::Text(s) if s.starts_with("SECRET"))).count();
                denied += cells.iter().filter(|c| **c == DENIED).count();
            }
            Err(_) => p.reset(),
        }
    }
    stop.store(true, Ordering::SeqCst);
    flipper.join().unwrap();
    println!("PH-05: {tries} reads, {denied} correctly denied, {leaked} returned the file outside the grant");
    assert!(leaked > 0, "the race was not won in 30 s ({tries} reads)");
}

/// PH-06: a hard link inside a grant is the file outside it. Nothing detects it.
#[test]
#[ignore = "PoC PH-06: passes while hard links are followed out of a grant"]
fn ph06_a_hard_link_reads_and_overwrites_a_file_outside_the_grant() {
    let t = Temp::new("poc-hardlink");
    let secret = t.write("outside\\secret.txt", "secret outside");
    std::fs::create_dir_all(t.path("granted")).unwrap();
    let alias = t.path("granted\\alias.txt");
    if let Err(e) = std::fs::hard_link(&secret, &alias) {
        println!("PH-06 NOT RUN: could not create a hard link ({e})");
        return;
    }
    let root = tsubame_plughost::caps::normalize(&t.path("granted")).unwrap();
    let p = plugin(&t, caps(vec![grant(Scope::Prefix(root), Access::ReadWrite)]));
    assert_eq!(column(&p, "read", &[&alias]).unwrap(), [Cell::Text("secret outside".into())]);
    assert_eq!(column(&p, "write", &[&alias]).unwrap(), [Cell::Int(0)]);
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "hi", "the file outside the grant was replaced");
}

// ---- the app's launcher (PH-07, tier 1) ---------------------------------------------------------

/// The real host, started the way the app starts it: in a job, with a cleared environment.
#[cfg(windows)]
#[test]
fn the_real_host_runs_inside_the_job_launcher() {
    use tsubame_plughost::launcher::JobLauncher;
    let t = Temp::new("job-launcher");
    let caps = Capabilities { max_memory_mb: 8, ..Capabilities::default() };
    let launcher = JobLauncher::new(HOST_EXE, &t.0, caps.max_memory_mb).unwrap().with_elevation_check(|| false);
    let p = Plugin::new(load_for(&t, full_cwasm(), caps, true), launcher, options());
    assert_eq!(column(&p, "echo", &[r"C:\a"]).unwrap(), [Cell::Text(r"C:\a".into())]);
    // The plugin's own cap trips before the job's limit (cap + overhead) does.
    assert_eq!(column(&p, "grow", &[r"C:\a"]), Err(CallError::Memory));
    assert_eq!(column(&p, "echo", &[r"C:\b"]).unwrap().len(), 1);
    assert_eq!(p.stats().kills, 0);
    p.shutdown();
}
