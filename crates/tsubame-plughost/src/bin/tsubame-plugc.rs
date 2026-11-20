//! tsubame-plugc: the install step for a wasm plugin. Not part of the host exe.
//!
//!   tsubame-plugc <plugin folder> <cache folder>
//!
//! Checks `plugin.toml`, compiles the component it names and writes
//! `<cache folder>/<id>-<version>.cwasm`. Prints the file and its SHA-256; the
//! app stores the hash and the host refuses a `.cwasm` that does not match it.
//! Run it again when the host is updated (a `.cwasm` is tied to the wasmtime
//! version and the CPU).

use std::path::Path;
use std::process::ExitCode;
use tsubame_plughost::host::install;
use tsubame_plughost::sha256;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (Some(plugin), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: tsubame-plugc <plugin folder> <cache folder>");
        return ExitCode::from(2);
    };
    match install::install(Path::new(plugin), Path::new(out)) {
        Ok(i) => {
            println!("plugin   {} {}", i.manifest.id, i.manifest.version);
            println!("file     {}", i.component.display());
            println!("sha256   {}", sha256::to_hex(&i.sha256));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tsubame-plugc: {e}");
            ExitCode::from(1)
        }
    }
}
