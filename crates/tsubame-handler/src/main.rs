//! The folder-handler shim. No console, no window: Explorer starts it for a folder, it starts
//! Tsubame (or File Explorer when that fails) and exits. See `docs/default-file-manager.md`.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    std::process::exit(tsubame_handler::sys::run_from_process());
    #[cfg(not(windows))]
    eprintln!("tsubame-handler only runs on Windows");
}
