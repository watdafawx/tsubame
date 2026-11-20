//! tsubame-plughost: the wasm plugin host. A separate exe, started by the app
//! only when a wasm plugin is first used, one process per plugin.
//!
//!   tsubame-plughost serve       framed requests on stdin, replies on stdout
//!   tsubame-plughost --version
//!
//! Runtime only: it loads `.cwasm` files the install step (`tsubame-plugc`)
//! compiled and can not compile anything itself. See `docs/wasm-plugins.md`.

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("serve") => match tsubame_plughost::host::serve(io::stdin().lock(), io::stdout().lock()) {
            Ok(()) => ExitCode::SUCCESS,
            // The client went away or sent a frame that can not be read.
            Err(_) => ExitCode::from(1),
        },
        Some("--version") => {
            let _ = writeln!(io::stdout(), "tsubame-plughost {} (protocol {})", env!("CARGO_PKG_VERSION"), tsubame_plughost::proto::VERSION);
            ExitCode::SUCCESS
        }
        _ => {
            let _ = writeln!(io::stderr(), "usage: tsubame-plughost serve | --version\nStarted by Tsubame; not meant to be run by hand.");
            ExitCode::from(2)
        }
    }
}
