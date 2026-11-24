//! Guest SDK for Tsubame wasm plugins (`tsubame:plugin@0.1.0`).
//!
//! A plugin is a WebAssembly component that runs in `tsubame-plughost.exe`.
//! This crate is what the plugin links: the generated bindings for the WIT
//! file, a small trait per entry point, a [`Doc`] builder for previews, thin
//! wrappers over the host's file functions, and the [`plugin!`] macro that
//! wires it all into the component.
//!
//! ```ignore
//! use tsubame_plugin_sdk::{plugin, Cell, Column};
//!
//! struct NameLength;
//!
//! impl Column for NameLength {
//!     const ID: &'static str = "name-length";
//!     fn value(path: &str) -> Cell {
//!         let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
//!         Cell::Int(name.chars().count() as i64)
//!     }
//! }
//!
//! plugin! {
//!     id: "com.example.name-length",
//!     version: env!("CARGO_PKG_VERSION"),
//!     columns: [NameLength],
//! }
//! ```
//!
//! # What the macro promises
//!
//! `plugin!` generates the `meta.describe` answer from what you list, so it
//! can not drift from the code: every listed column and previewer is declared
//! with its `ID`. The host compares that answer with `plugin.toml` at load and
//! refuses a plugin whose id, version or entry points differ, so keep the two
//! in step (the id and version strings, and one `[[column]]` or
//! `[[previewer]]` table per listed type with the same `id`).
//!
//! # `plugin.toml`
//!
//! Next to the component, the package ships a manifest. The smallest one for
//! a column that only looks at path text:
//!
//! ```toml
//! [plugin]
//! id = "com.example.name-length"       # same string as in plugin!
//! name = "Name length"
//! version = "0.1.0"                    # same as plugin!'s version
//! api = "0.1"
//! component = "name_length.wasm"       # the crate's cdylib, copied next to this file
//!
//! [[column]]
//! id = "name-length"                   # same as Column::ID
//! label = "Name length"
//! ```
//!
//! No `[capabilities]` table means no file access, no network, no processes,
//! 64 MB and 2 s per call. Ask for more only when the plugin needs it, for
//! example `paths = [{ path = "$input", access = "read" }]` for a previewer
//! that reads the file it shows. The full list is in
//! `docs/plugin-authoring.md`.
//!
//! # Limits that shape the code
//!
//! * There is no WASI. A guest built for `wasm32-wasip2` links, but anything
//!   that calls the operating system at run time traps: `println!`, clocks,
//!   `std::env`, threads, and `HashMap` with the default hasher (it seeds from
//!   a random source). Use `BTreeMap`, or a `HashMap` with a fixed hasher, and
//!   [`host::log`] instead of printing.
//! * Files only through [`host`], and only the paths the manifest grants.
//! * One call has a deadline (`timeout_ms`) and a memory cap
//!   (`max_memory_mb`). Work in bounded chunks.
//! * The instance is thrown away after a timeout, a trap or a memory fault, so
//!   `static` state is a cache at best, never the source of truth.

#![deny(missing_docs)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;

/// The raw bindings generated from `crates/tsubame-plughost/wit`. Use them for
/// what the SDK does not cover yet (vfs, search, command); the types people
/// normally need are re-exported at the crate root.
#[doc(hidden)]
pub mod bindings {
    #![allow(missing_docs, clippy::all, unused_macros, dead_code)]
    wit_bindgen::generate!({
        path: "../../crates/tsubame-plughost/wit",
        world: "plugin",
        pub_export_macro: true,
        export_macro_name: "__export",
        default_bindings_module: "tsubame_plugin_sdk::bindings",
    });
}

use bindings::exports::tsubame::plugin::{column as wit_column, command as wit_command, meta as wit_meta, previewer as wit_previewer, search as wit_search, vfs as wit_vfs};

pub use bindings::tsubame::plugin::types::{Button, Cell, Entry, Image, Limits, Node};
#[doc(hidden)]
pub use wit_meta::EntrypointKind as EntryKind;

mod doc;
pub use doc::Doc;

/// The functions the host offers a plugin, checked against the capabilities
/// the user granted at install. Anything not granted comes back as
/// [`FsError::Denied`].
pub mod host {
    use super::bindings::tsubame::plugin::host as wit;
    use alloc::string::String;
    use alloc::vec::Vec;

    pub use wit::{Access, Capabilities, FileInfo, FsError, Level, PathGrant, Scope};

    /// The capabilities in force for this plugin.
    pub fn capabilities() -> Capabilities {
        wit::get_capabilities()
    }

    /// Writes one line to the host's log. Long lines are cut and a call may
    /// log a limited number of lines (64 of 1 KiB).
    pub fn log(level: Level, message: &str) {
        wit::log(level, message);
    }

    /// Size, kind and modification time (a FILETIME) of `path`.
    pub fn stat(path: &str) -> Result<FileInfo, FsError> {
        wit::stat(path)
    }

    /// Reads up to `max_len` bytes at `offset`. The host caps one read at
    /// 4 MiB; fewer bytes come back at the end of the file.
    pub fn read_file(path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, FsError> {
        wit::read_file(path, offset, max_len)
    }

    /// Reads from the start of `path` until `max_bytes` bytes are read or the
    /// file ends. `max_bytes` is the cap, so a huge file costs `max_bytes` and
    /// no more. Returns the bytes and whether the file had more.
    pub fn read_prefix(path: &str, max_bytes: u64) -> Result<(Vec<u8>, bool), FsError> {
        const CHUNK: u32 = 1 << 20;
        let mut out = Vec::new();
        while (out.len() as u64) < max_bytes {
            let want = (max_bytes - out.len() as u64).min(u64::from(CHUNK)) as u32;
            let part = wit::read_file(path, out.len() as u64, want)?;
            let n = part.len();
            out.extend_from_slice(&part);
            if n < want as usize {
                return Ok((out, false));
            }
        }
        // `max_bytes` were read; one more byte tells whether the file goes on.
        let more = !wit::read_file(path, out.len() as u64, 1)?.is_empty();
        Ok((out, more))
    }

    /// Lists a folder: at most `max` entries (the host caps it at 2,000) after
    /// skipping `skip`. An empty list is the end.
    pub fn read_dir(path: &str, skip: u32, max: u32) -> Result<Vec<super::Entry>, FsError> {
        wit::read_dir(path, skip, max)
    }

    /// Replaces the file's contents, or appends to it. Needs a `read-write`
    /// grant, and the folder must exist.
    pub fn write_file(path: &str, data: &[u8], append: bool) -> Result<(), FsError> {
        wit::write_file(path, data, append)
    }

    /// Text for an error, for `Err(...)` strings the user may see.
    pub fn describe(e: FsError) -> String {
        match e {
            FsError::Denied => "access denied: the plugin was not granted this path".into(),
            FsError::NotFound => "not found".into(),
            FsError::InvalidPath => "not a valid absolute path".into(),
            FsError::TooLarge => "too large".into(),
            FsError::Io => "read error".into(),
        }
    }
}

/// A details-view column. One value per path.
///
/// Implement `value` for a column that looks at one path at a time, or
/// `values` when a batch can be answered faster together (a shared lookup,
/// one folder read). The host sends at most 2,000 paths per call and expects
/// exactly one cell per path, in order; the default `values` guarantees that.
pub trait Column: 'static {
    /// The id the manifest's `[[column]]` table declares.
    const ID: &'static str;

    /// The cell for one path. The default is an empty cell.
    fn value(path: &str) -> Cell {
        let _ = path;
        Cell::Empty
    }

    /// The cells for a batch of paths. The default calls [`Column::value`] for each.
    fn values(paths: &[String]) -> Vec<Cell> {
        paths.iter().map(|p| Self::value(p)).collect()
    }
}

/// A previewer: turns one file into a few rows of declared UI. Tsubame draws
/// them; a plugin never draws. Build the rows with [`Doc`].
pub trait Previewer: 'static {
    /// The id the manifest's `[[previewer]]` table declares.
    const ID: &'static str;

    /// Previews `path`. `limits.max_bytes` is how much the caller wants read
    /// and `limits.max_px` the longest side of any image returned. An `Err`
    /// text is shown to the user in place of the preview.
    fn preview(path: &str, limits: Limits) -> Result<Vec<Node>, String>;
}

/// What the generated plugin type provides. Implemented by [`plugin!`]; not
/// for hand-written code.
#[doc(hidden)]
pub trait Registry: 'static {
    const ID: &'static str;
    const VERSION: &'static str;
    fn entrypoints() -> Vec<(EntryKind, &'static str)>;
    fn column(id: &str, paths: &[String]) -> Option<Vec<Cell>>;
    fn preview(id: &str, path: &str, limits: Limits) -> Option<Result<Vec<Node>, String>>;
}

/// The component's exports, for any [`Registry`]. [`plugin!`] exports one.
#[doc(hidden)]
pub struct Shim<P>(PhantomData<P>);

impl<P: Registry> wit_meta::Guest for Shim<P> {
    fn describe() -> wit_meta::Info {
        wit_meta::Info {
            id: P::ID.into(),
            version: P::VERSION.into(),
            entrypoints: P::entrypoints().into_iter().map(|(kind, id)| wit_meta::Entrypoint { kind, id: id.into() }).collect(),
        }
    }
}

impl<P: Registry> wit_column::Guest for Shim<P> {
    fn values(column: String, paths: Vec<String>) -> Vec<Cell> {
        match P::column(&column, &paths) {
            Some(cells) => cells,
            // The host only routes declared columns; an unknown id is a bug in
            // plugin.toml. Answer with the right count so it is not also a fault.
            None => {
                host::log(host::Level::Error, &alloc::format!("no column `{column}` in this plugin"));
                paths.iter().map(|_| Cell::Empty).collect()
            }
        }
    }
}

impl<P: Registry> wit_previewer::Guest for Shim<P> {
    fn preview(id: String, path: String, limits: Limits) -> Result<Vec<Node>, String> {
        P::preview(&id, &path, limits).unwrap_or_else(|| Err(alloc::format!("no previewer `{id}` in this plugin")))
    }
}

// The interfaces this SDK has no trait for yet. The host only calls an entry
// point the manifest declares, and `plugin!` declares none of these.
impl<P: Registry> wit_vfs::Guest for Shim<P> {
    fn list_dir(_id: String, _path: String, _cursor: Option<String>) -> Result<wit_vfs::Page, String> {
        Err("this plugin has no vfs".into())
    }
    fn read(_id: String, _path: String, _offset: u64, _max_len: u32) -> Result<Vec<u8>, String> {
        Err("this plugin has no vfs".into())
    }
}

impl<P: Registry> wit_search::Guest for Shim<P> {
    fn find(_id: String, _q: wit_search::Query, _cursor: Option<String>) -> Result<wit_search::Page, String> {
        Err("this plugin has no search provider".into())
    }
}

impl<P: Registry> wit_command::Guest for Shim<P> {
    fn run(_name: String, _cx: wit_command::Context) -> Result<Option<String>, String> {
        Err("this plugin has no commands".into())
    }
}

#[doc(hidden)]
pub use bindings::__export;

/// Declares the plugin: its id, version and entry points, and exports the
/// component.
///
/// ```ignore
/// plugin! {
///     id: "com.example.files",
///     version: env!("CARGO_PKG_VERSION"),
///     columns: [Kind, NameLength],
///     previewers: [Summary],
/// }
/// ```
///
/// `columns` and `previewers` are both optional, but each line ends with a
/// comma and `columns` comes before `previewers`. Each listed type implements
/// [`Column`] or [`Previewer`] and its `ID` must match a table in
/// `plugin.toml`. Use it once, in the crate's root module.
#[macro_export]
macro_rules! plugin {
    (
        id: $id:expr,
        version: $version:expr,
        $(columns: [$($col:ty),* $(,)?],)?
        $(previewers: [$($pv:ty),* $(,)?],)?
    ) => {
        #[doc(hidden)]
        pub struct __TsubamePlugin;

        impl $crate::Registry for __TsubamePlugin {
            const ID: &'static str = $id;
            const VERSION: &'static str = $version;

            #[allow(unused_mut)]
            fn entrypoints() -> $crate::__private::Vec<($crate::EntryKind, &'static str)> {
                let mut list = $crate::__private::Vec::new();
                $($( list.push(($crate::EntryKind::Column, <$col as $crate::Column>::ID)); )*)?
                $($( list.push(($crate::EntryKind::Previewer, <$pv as $crate::Previewer>::ID)); )*)?
                list
            }

            #[allow(unused_variables)]
            fn column(id: &str, paths: &[$crate::__private::String]) -> Option<$crate::__private::Vec<$crate::Cell>> {
                $($( if id == <$col as $crate::Column>::ID { return Some(<$col as $crate::Column>::values(paths)); } )*)?
                None
            }

            #[allow(unused_variables)]
            fn preview(id: &str, path: &str, limits: $crate::Limits) -> Option<Result<$crate::__private::Vec<$crate::Node>, $crate::__private::String>> {
                $($( if id == <$pv as $crate::Previewer>::ID { return Some(<$pv as $crate::Previewer>::preview(path, limits)); } )*)?
                None
            }
        }

        #[doc(hidden)]
        pub type __TsubameExport = $crate::Shim<__TsubamePlugin>;
        $crate::__export!(__TsubameExport with_types_in $crate::bindings);
    };
}

#[doc(hidden)]
pub mod __private {
    pub use alloc::string::String;
    pub use alloc::vec::Vec;
}

#[cfg(test)]
mod tests;
