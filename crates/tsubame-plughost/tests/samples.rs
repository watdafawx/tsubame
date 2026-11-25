//! End-to-end tests of the sample plugins in `plugins/`, built with
//! `tsubame-plugin-sdk`: the real `.wasm` the guest workspace produces, compiled
//! the way the install step does, run in the real `tsubame-plughost` exe and
//! driven through the client and adapters, with the capabilities each sample's
//! own `plugin.toml` asks for.
//!
//! The samples are built by a nested `cargo build --release --target wasm32-wasip2`
//! in `plugins/` (its own workspace and target folder, so it does not touch this
//! build's lock). That needs `rustup target add wasm32-wasip2`. Without the
//! target the tests print why and pass; set `TSUBAME_REQUIRE_WASM_SAMPLES=1`
//! (CI) to make a missing target a failure.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tsubame_ext_api::{Cell, ColumnProvider, Doc, Limits, Node, Previewer};
use tsubame_plughost::adapters::{WasmColumn, WasmPreviewer};
use tsubame_plughost::caps::{Capabilities, Protected};
use tsubame_plughost::client::{Options, Plugin, ProcessLauncher, Status};
use tsubame_plughost::manifest::Manifest;
use tsubame_plughost::install_record;
use tsubame_plughost::proto::Load;

const HOST_EXE: &str = env!("CARGO_BIN_EXE_tsubame-plughost");

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// True when the wasm target is installed; otherwise says so (or fails, in CI).
fn wasm_target_available() -> bool {
    let ok = Command::new("rustc")
        .args(["--print", "target-libdir", "--target", "wasm32-wasip2"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()).is_dir())
        .unwrap_or(false);
    if !ok {
        assert!(
            std::env::var_os("TSUBAME_REQUIRE_WASM_SAMPLES").is_none(),
            "wasm32-wasip2 is not installed (rustup target add wasm32-wasip2) and TSUBAME_REQUIRE_WASM_SAMPLES is set"
        );
        eprintln!("SKIPPED: wasm32-wasip2 is not installed (rustup target add wasm32-wasip2)");
    }
    ok
}

/// A built and compiled sample.
struct Sample {
    manifest: Manifest,
    dir: PathBuf,
    wasm: Vec<u8>,
    cwasm: Vec<u8>,
}

/// Builds `plugins/<name>`, compiles its component with the host's engine settings.
fn build(name: &'static str, cell: &'static OnceLock<Option<Sample>>) -> Option<&'static Sample> {
    // One cargo at a time in the guest workspace; the tests run in parallel.
    static BUILD: Mutex<()> = Mutex::new(());
    cell.get_or_init(|| {
        if !wasm_target_available() {
            return None;
        }
        let _g = BUILD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = plugins_dir().join(name).canonicalize().expect("the sample folder exists");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let target = plugins_dir().join("target");
        let out = Command::new(cargo)
            .current_dir(&dir)
            .args(["build", "--release", "--target", "wasm32-wasip2", "--target-dir"])
            .arg(&target)
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .expect("cargo runs");
        assert!(out.status.success(), "building {name} failed:\n{}", String::from_utf8_lossy(&out.stderr));
        let manifest = Manifest::load(&dir).expect("plugin.toml is valid");
        let wasm = std::fs::read(target.join("wasm32-wasip2/release").join(&manifest.component)).expect("the build wrote the component the manifest names");
        let cwasm = tsubame_plugc::compile(&wasm).expect("the component compiles");
        Some(Sample { manifest, dir, wasm, cwasm })
    });
    cell.get().and_then(|s| s.as_ref())
}

fn file_kind() -> Option<&'static Sample> {
    static S: OnceLock<Option<Sample>> = OnceLock::new();
    build("sample-file-kind", &S)
}

fn csv_summary() -> Option<&'static Sample> {
    static S: OnceLock<Option<Sample>> = OnceLock::new();
    build("sample-csv-summary", &S)
}

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("tsubame-samples-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
    fn write(&self, rel: &str, data: &str) -> String {
        let p = self.0.join(rel);
        std::fs::write(&p, data).unwrap();
        p.to_str().unwrap().to_string()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Installs the sample the way the app does (a package folder, the install
/// record in a cache folder), checks it against its record and starts it with
/// the capabilities the record holds, or `caps` instead.
fn start(s: &Sample, t: &Temp, caps: Option<Capabilities>) -> Arc<Plugin> {
    let pkg = t.0.join("package");
    std::fs::create_dir_all(&pkg).unwrap();
    let manifest_bytes = std::fs::read(s.dir.join("plugin.toml")).unwrap();
    std::fs::write(pkg.join("plugin.toml"), &manifest_bytes).unwrap();
    std::fs::write(pkg.join(&s.manifest.component), &s.wasm).unwrap();
    let (cache, data) = (t.0.join("cache"), t.0.join("data"));
    install_record::store(&pkg, &cache, Some(&data), &[], &manifest_bytes, &s.wasm, &s.cwasm).expect("the sample installs");
    let verified = install_record::verify(&pkg, &cache, &s.manifest.id, &s.manifest.version, &[]).expect("the install record matches");
    let mut load = Load::new(&verified);
    if let Some(c) = caps {
        load.caps = c;
    }
    // Generous start-up: the first start of a host on a busy machine is slow.
    let opts = Options { start_timeout: Duration::from_secs(30), ..Options::default() };
    Arc::new(Plugin::new(load, ProcessLauncher::new(HOST_EXE), opts))
}

fn values(col: &WasmColumn, paths: &[&str]) -> Vec<Cell> {
    let mut out = Vec::new();
    col.values(paths, &mut out);
    out
}

#[test]
fn the_file_kind_sample_fills_two_typed_columns() {
    let Some(s) = file_kind() else { return };
    // it asks for nothing
    assert_eq!(s.manifest.capabilities("C:\\p", "C:\\d", &Protected::default()).unwrap(), Capabilities::default());
    let t = Temp::new("kind");
    let p = start(s, &t, None);

    let kind = WasmColumn::new(p.clone(), "kind");
    let paths = [r"C:\pics\Holiday.JPG", r"C:\src\main.rs", r"C:\dl\a.tar.gz", r"C:\x\Makefile", r"C:\x\what.zzz"];
    assert_eq!(values(&kind, &paths), [Cell::Text("Image".into()), Cell::Text("Code".into()), Cell::Text("Archive".into()), Cell::Empty, Cell::Text("Other".into())]);

    let len = WasmColumn::new(p.clone(), "name-length");
    assert_eq!(values(&len, &[r"C:\a\abc.txt", r"C:\日本語.txt", r"C:\x"]), [Cell::Int(7), Cell::Int(7), Cell::Int(1)]);

    // a full batch keeps order and count
    let many: Vec<String> = (0..2000).map(|i| format!(r"C:\d\f{i}.png")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let cells = values(&WasmColumn::new(p.clone(), "kind"), &refs);
    assert_eq!(cells.len(), 2000);
    assert!(cells.iter().all(|c| *c == Cell::Text("Image".into())));

    assert_eq!(p.status(), Status::Running);
    assert_eq!(p.stats().faults, 0);
}

fn texts(doc: &Doc) -> Vec<String> {
    doc.nodes
        .iter()
        .map(|n| match n {
            Node::Heading(t) | Node::Mono(t) => t.clone(),
            Node::Text { text, .. } => text.clone(),
            Node::Pair(k, v) => format!("{k}={v}"),
            _ => String::new(),
        })
        .collect()
}

#[test]
fn the_csv_sample_summarizes_a_file_it_was_handed() {
    let Some(s) = csv_summary() else { return };
    // its whole capability list: read the files it is asked about
    let caps = s.manifest.capabilities("C:\\p", "C:\\d", &Protected::default()).unwrap();
    assert_eq!(caps.paths.len(), 1);
    assert!(!caps.network && !caps.run_processes);

    let t = Temp::new("csv");
    let p = start(s, &t, None);
    let pv = WasmPreviewer::new(p.clone(), &s.manifest.previewers[0]);
    assert!(pv.accepts("csv") && !pv.accepts("txt"));

    let file = t.write("people.csv", "id,name,city\n1,\"Doe, Jane\",Oslo\n2,Bo,Rome\n3,Cy,Lima\n");
    let mut doc = Doc::default();
    pv.preview(&file, Limits { max_bytes: 1 << 20, max_px: 64 }, &mut doc).unwrap();
    let t = texts(&doc);
    assert_eq!(t[0], "people.csv");
    for want in ["Rows=3", "Columns=3", "Delimiter=comma"] {
        assert!(t.contains(&want.to_string()), "{want} in {t:?}");
    }
    assert!(t.contains(&" 2  name".to_string()));
    assert!(t.contains(&"1 | Doe, Jane | Oslo".to_string()), "{t:?}");
}

#[test]
fn the_csv_sample_reads_only_what_the_caller_allows() {
    let Some(s) = csv_summary() else { return };
    let t = Temp::new("csv-cap");
    let p = start(s, &t, None);
    let pv = WasmPreviewer::new(p.clone(), &s.manifest.previewers[0]);
    let mut text = String::from("a,b\n");
    for i in 0..5000 {
        text.push_str(&format!("{i},row\n"));
    }
    let file = t.write("long.csv", &text);
    let mut doc = Doc::default();
    pv.preview(&file, Limits { max_bytes: 1000, max_px: 64 }, &mut doc).unwrap();
    let t = texts(&doc);
    let rows = t.iter().find_map(|l| l.strip_prefix("Rows=at least ")).unwrap_or_else(|| panic!("{t:?}"));
    let n: usize = rows.parse().unwrap();
    assert!((50..300).contains(&n), "only the first 1000 bytes were counted, got {n}");
}

#[test]
fn the_csv_sample_without_its_grant_is_refused_by_the_host() {
    let Some(s) = csv_summary() else { return };
    let t = Temp::new("csv-denied");
    let p = start(s, &t, Some(Capabilities::default()));
    let pv = WasmPreviewer::new(p.clone(), &s.manifest.previewers[0]);
    let file = t.write("secret.csv", "a,b\n1,2\n");
    let mut doc = Doc::default();
    let e = pv.preview(&file, Limits { max_bytes: 1 << 20, max_px: 64 }, &mut doc).unwrap_err();
    assert!(e.contains("access denied"), "{e}");
    assert!(doc.nodes.is_empty());
    // the plugin's own error is not a fault
    assert_eq!(p.stats().faults, 0);
}
