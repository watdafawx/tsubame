//! The install step for a wasm plugin: checks a package and compiles its
//! component to the `.cwasm` the host loads.
//!
//! This is the only crate in the workspace that links Cranelift. It depends on
//! `tsubame-plughost` (the manifest, the install record, the engine settings);
//! the host package has no compiler feature, so a build of `tsubame-plughost`
//! can not contain one (security review PH-20).

use std::path::Path;
use tsubame_plughost::host::engine_config;
use tsubame_plughost::install_record::{self, MAX_WASM_BYTES, RECORD_FILE, Stored};
use tsubame_plughost::manifest::{MANIFEST_FILE, MAX_MANIFEST_BYTES, Manifest};
use wasmtime::Engine;

/// Compiles `.wasm` component bytes to the `.cwasm` bytes the host loads, with
/// exactly the engine settings the host checks for.
pub fn compile(wasm: &[u8]) -> Result<Vec<u8>, String> {
    let engine = Engine::new(&engine_config()).map_err(|e| e.to_string())?;
    engine.precompile_component(wasm).map_err(|e| format!("not a valid component: {e:#}"))
}

/// Checks the manifest in `plugin_dir`, compiles the component it names and
/// writes `<cache>\<id>\<version>\component.cwasm` and the install record
/// next to it ([`install_record::store`]). `cache` must be a folder only
/// the app writes to; `data_dir` is the plugin's `$data` folder;
/// `protected` are the app's own folders (settings, exe folder), which no
/// grant may reach into. Each file is read once, at most its size limit,
/// and the bytes that were checked are the ones that are hashed.
pub fn install(plugin_dir: &Path, cache: &Path, data_dir: Option<&Path>, protected: &[String]) -> Result<Stored, String> {
    let read = |p: &Path, limit: u64| install_record::read_capped(p, limit).map_err(|e| format!("{}: {e}", p.display()));
    let manifest_bytes = read(&plugin_dir.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
    let manifest = Manifest::parse(std::str::from_utf8(&manifest_bytes).map_err(|_| format!("{MANIFEST_FILE}: not UTF-8"))?).map_err(|e| e.to_string())?;
    // Before the (slow) compile: a version is installed once.
    if install_record::install_dir(cache, &manifest.id, &manifest.version).join(RECORD_FILE).symlink_metadata().is_ok() {
        return Err(format!("{} {} is already installed; remove its folder in the cache to install it again", manifest.id, manifest.version));
    }
    let wasm = read(&plugin_dir.join(&manifest.component), MAX_WASM_BYTES)?;
    let cwasm = compile(&wasm)?;
    install_record::store(plugin_dir, cache, data_dir, protected, &manifest_bytes, &wasm, &cwasm).map_err(|e| e.to_string())
}
