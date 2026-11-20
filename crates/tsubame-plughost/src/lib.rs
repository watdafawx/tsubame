//! Wasm plugins for Tsubame (tier 2 of `docs/PLAN.md` section 4). See
//! `docs/wasm-plugins.md`.
//!
//! | Module | What | Used by |
//! |---|---|---|
//! | [`manifest`] | `plugin.toml`: identity, entry points, capabilities | app, install step |
//! | [`caps`] | capabilities and the path rules | app, host |
//! | [`proto`] | the framed protocol over the host's stdin/stdout | app, host |
//! | [`client`] | [`client::Plugin`]: lazy start, deadlines, kill on hang, fault limit | app |
//! | [`adapters`] | entry points as `tsubame-ext-api` providers | app |
//! | [`cache`] | bounded cache of column values | app |
//! | [`host`] | wasmtime, limits, the `host` imports, the serve loop (feature `host`) | `tsubame-plughost.exe` |
//!
//! The app depends on this crate with `default-features = false`: that leaves
//! out the `host` module and with it wasmtime, so nothing of the runtime is
//! compiled into, or linked into, the main exe.

pub mod adapters;
pub mod cache;
pub mod caps;
pub mod client;
#[cfg(feature = "host")]
pub mod host;
pub mod manifest;
pub mod proto;
pub mod sha256;
