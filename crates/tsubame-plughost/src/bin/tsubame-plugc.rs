//! tsubame-plugc: the install step for a wasm plugin. Not part of the host exe.
//!
//!   tsubame-plugc <plugin folder> <cache folder> [<data folder>]
//!
//! Checks `plugin.toml`, compiles the component it names and writes
//! `<cache folder>\<id>\<version>\component.cwasm` and the install record
//! `install.toml` next to it: the SHA-256 of `plugin.toml`, of the `.wasm` and
//! of the `.cwasm`, the wasmtime version, and the resolved capabilities
//! (`crate::install_record`). The app loads the plugin from the record only,
//! and refuses it when `plugin.toml` or the `.wasm` changed since.
//!
//! A version is installed once: to install it again (a changed package, or a
//! host with another wasmtime version), remove its folder in the cache first.
//! `<data folder>` is the plugin's `$data` folder; it is needed only when the
//! plugin asks for one.

use std::path::Path;
use std::process::ExitCode;
use tsubame_plughost::host::install;
use tsubame_plughost::install_record::{COMPONENT_FILE, RECORD_FILE};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (Some(plugin), Some(cache)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: tsubame-plugc <plugin folder> <cache folder> [<data folder>]");
        return ExitCode::from(2);
    };
    let data = args.get(3).map(Path::new);
    match install::install(Path::new(plugin), Path::new(cache), data, &[]) {
        Ok(s) => {
            println!("plugin   {} {}", s.manifest.id, s.manifest.version);
            println!("file     {}", s.dir.join(COMPONENT_FILE).display());
            println!("record   {}", s.dir.join(RECORD_FILE).display());
            println!("sha256   {}", s.record.cwasm_sha256);
            if let Ok(caps) = s.record.capabilities.to_caps() {
                for line in caps.summary() {
                    println!("allows   {line}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tsubame-plugc: {e}");
            ExitCode::from(1)
        }
    }
}
