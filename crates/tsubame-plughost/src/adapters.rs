//! A wasm plugin's entry points as `tsubame-ext-api` providers, so they
//! register like a built-in: the registry and the views never see the host
//! process behind them.
//!
//! Each adapter answers what the manifest can answer (`accepts`) without
//! starting the host, and turns a failed call into an empty result: a broken
//! plugin shows nothing, it never breaks the caller.

use crate::cache::CellCache;
use crate::client::Plugin;
use crate::manifest::{ColumnDecl, Manifest, ProviderDecl};
use crate::proto::MAX_BATCH;
use std::sync::{Arc, Mutex};
use tsubame_ext_api::{Cell, ColumnProvider, ColumnSource, ColumnSpec, Contributions, Doc, Hit, Host, Limits, Previewer, Query, SearchProvider, VfsEntry, VfsProvider};

/// Most bytes a virtual file may have when it is read into memory.
pub const VFS_READ_MAX: u64 = 256 << 20;
/// Default size of a column's value cache.
pub const COLUMN_CACHE_ENTRIES: usize = 8_192;
pub const COLUMN_CACHE_BYTES: usize = 2 << 20;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A plugin column. Values are cached per path in a bounded cache; the app
/// calls [`WasmColumn::invalidate`] when a file changes.
pub struct WasmColumn {
    plugin: Arc<Plugin>,
    column: String,
    cache: Mutex<CellCache>,
}

impl WasmColumn {
    pub fn new(plugin: Arc<Plugin>, column: impl Into<String>) -> WasmColumn {
        WasmColumn::with_cache(plugin, column, COLUMN_CACHE_ENTRIES, COLUMN_CACHE_BYTES)
    }

    /// `entries` 0 switches the cache off.
    pub fn with_cache(plugin: Arc<Plugin>, column: impl Into<String>, entries: usize, bytes: usize) -> WasmColumn {
        WasmColumn { plugin, column: column.into(), cache: Mutex::new(CellCache::new(entries, bytes)) }
    }

    /// Forgets the value of one path.
    pub fn invalidate(&self, path: &str) {
        lock(&self.cache).remove(path);
    }

    /// Forgets every value.
    pub fn clear(&self) {
        lock(&self.cache).clear();
    }

    pub fn cached(&self) -> usize {
        lock(&self.cache).len()
    }
}

impl ColumnProvider for WasmColumn {
    fn max_batch(&self) -> usize {
        MAX_BATCH
    }

    fn values(&self, paths: &[&str], out: &mut Vec<Cell>) {
        let start = out.len();
        let mut misses: Vec<usize> = Vec::new();
        {
            let mut cache = lock(&self.cache);
            for (i, p) in paths.iter().enumerate() {
                match cache.get(p) {
                    Some(c) => out.push(c),
                    None => {
                        out.push(Cell::Empty);
                        misses.push(i);
                    }
                }
            }
        }
        if misses.is_empty() {
            return;
        }
        // The lock is not held across the call: it can take as long as the plugin's timeout.
        let ask: Vec<&str> = misses.iter().map(|&i| paths[i]).collect();
        let mut got = Vec::with_capacity(ask.len());
        if self.plugin.column_values(&self.column, &ask, &mut got).is_err() {
            // Cells stay empty and are not cached, so a later call asks again.
            return;
        }
        let mut cache = lock(&self.cache);
        for (&i, cell) in misses.iter().zip(got) {
            cache.put(paths[i], cell.clone());
            out[start + i] = cell;
        }
    }
}

fn has_extension(extensions: &[String], ext: &str) -> bool {
    extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

pub struct WasmPreviewer {
    plugin: Arc<Plugin>,
    id: String,
    extensions: Vec<String>,
}

impl WasmPreviewer {
    pub fn new(plugin: Arc<Plugin>, decl: &ProviderDecl) -> WasmPreviewer {
        WasmPreviewer { plugin, id: decl.id.clone(), extensions: decl.extensions.clone() }
    }
}

impl Previewer for WasmPreviewer {
    fn accepts(&self, ext: &str) -> bool {
        has_extension(&self.extensions, ext)
    }

    fn preview(&self, path: &str, limits: Limits, out: &mut Doc) -> Result<(), String> {
        self.plugin.preview(&self.id, path, limits, &mut out.nodes).map_err(|e| e.to_string())
    }
}

pub struct WasmVfs {
    plugin: Arc<Plugin>,
    id: String,
    extensions: Vec<String>,
}

impl WasmVfs {
    pub fn new(plugin: Arc<Plugin>, decl: &ProviderDecl) -> WasmVfs {
        WasmVfs { plugin, id: decl.id.clone(), extensions: decl.extensions.clone() }
    }
}

impl VfsProvider for WasmVfs {
    /// True when the path is, or goes through, a file with one of the declared
    /// extensions (`C:\a.zip`, `C:\a.zip\inner\x`).
    fn accepts(&self, path: &str) -> bool {
        path.split(['\\', '/']).any(|c| c.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && has_extension(&self.extensions, ext)))
    }

    fn list(&self, path: &str, sink: &mut dyn FnMut(&[VfsEntry]) -> bool) -> Result<(), String> {
        self.plugin.vfs_list(&self.id, path, sink).map_err(|e| e.to_string())
    }

    fn read(&self, path: &str, out: &mut Vec<u8>) -> Result<(), String> {
        let start = out.len();
        self.plugin.vfs_read(&self.id, path, VFS_READ_MAX + 1, out).map_err(|e| e.to_string())?;
        if (out.len() - start) as u64 > VFS_READ_MAX {
            out.truncate(start);
            return Err(format!("file is larger than {} MB", VFS_READ_MAX >> 20));
        }
        Ok(())
    }
}

pub struct WasmSearch {
    plugin: Arc<Plugin>,
    id: String,
}

impl WasmSearch {
    pub fn new(plugin: Arc<Plugin>, decl: &ProviderDecl) -> WasmSearch {
        WasmSearch { plugin, id: decl.id.clone() }
    }
}

impl SearchProvider for WasmSearch {
    fn search(&self, q: &Query, sink: &mut dyn FnMut(&[Hit]) -> bool) -> Result<(), String> {
        self.plugin.search(&self.id, q.text, q.root, q.limit, sink).map_err(|e| e.to_string())
    }
}

/// The registry row of a plugin column.
pub fn column_spec(manifest: &Manifest, decl: &ColumnDecl) -> ColumnSpec {
    ColumnSpec { id: manifest.qualified(&decl.id).into(), label: decl.label.clone().into(), align: decl.align, width: decl.width, flex: false }
}

/// Adds the plugin's columns, previewers, vfs providers and search providers
/// to an extension's contributions, named `<plugin id>.<entry id>`. Commands
/// are left to the app: only it knows how a command runs (`Host::Run`).
/// Nothing is started; the host starts when one of them is first used.
pub fn contribute<H: Host>(manifest: &Manifest, plugin: &Arc<Plugin>, c: &mut Contributions<H>) {
    for d in &manifest.columns {
        c.column(column_spec(manifest, d), ColumnSource::Batched(Box::new(WasmColumn::new(plugin.clone(), d.id.clone()))));
    }
    for d in &manifest.previewers {
        c.previewer(manifest.qualified(&d.id).into(), d.title.clone().into(), Box::new(WasmPreviewer::new(plugin.clone(), d)));
    }
    for d in &manifest.vfs {
        c.vfs(manifest.qualified(&d.id).into(), d.title.clone().into(), Box::new(WasmVfs::new(plugin.clone(), d)));
    }
    for d in &manifest.search {
        c.search(manifest.qualified(&d.id).into(), d.title.clone().into(), Box::new(WasmSearch::new(plugin.clone(), d)));
    }
}
