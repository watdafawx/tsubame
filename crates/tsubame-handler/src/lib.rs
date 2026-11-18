//! `tsubame-handler.exe`: the fail-safe shim that the opt-in "Default file manager" verb runs
//! (`docs/default-file-manager.md`). The library holds everything testable: command-line
//! reading and building, the registration logic shared with the app's Settings page, and the
//! decision state machine behind a [`run::Host`] trait. [`sys`] is the thin Win32 layer.
//!
//! Dependencies are kept to the `windows` crate on purpose (size and start time).

pub mod cmdline;
pub mod registry;
pub mod run;
#[cfg(windows)]
pub mod sys;
