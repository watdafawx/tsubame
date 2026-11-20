//! The host's side: loads one precompiled component and answers the client's
//! calls over a pair of streams. This is what `tsubame-plughost.exe` runs.
//!
//! Enforcement lives here, not in the guest and not in the client:
//! - **Memory.** A resource limiter caps the total of all linear memories at
//!   the plugin's `max_memory_mb`, and caps tables and instances.
//! - **Time.** Every entry into the guest runs under an epoch deadline. A
//!   watchdog thread that sleeps until a call starts bumps the epoch when the
//!   plugin's `timeout_ms` is over, which traps the guest at its next loop or
//!   call. The instance is then thrown away and built fresh for the next call.
//! - **Files.** The guest has no WASI. Its only way to a file is the `host`
//!   interface below, and every path goes through [`PathPolicy`] and then a
//!   check of the resolved path.
//! - **Network, processes.** There is no import for either.

use crate::caps::{self, Access, Capabilities, Denied, PathPolicy, Root, Scope};
use crate::manifest::Kind;
use crate::proto::{self, ErrKind, Info, Load, Request};
use crate::sha256;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tsubame_ext_api as api;
use wasmtime::component::{Component, HasSelf, InstancePre, Linker};
use wasmtime::{Config, Engine, ResourceLimiter, Store, StoreLimits, StoreLimitsBuilder, Trap};

mod bindings {
    wasmtime::component::bindgen!({ path: "wit", world: "plugin" });
}

use bindings::exports::tsubame::plugin::{column, command, meta, previewer, search, vfs};
use bindings::tsubame::plugin::host as wit_host;
use bindings::tsubame::plugin::types as wit;

/// Largest `.cwasm` the host will read.
pub const MAX_COMPONENT_BYTES: u64 = 256 << 20;
/// Most bytes one `read-file` returns.
pub const MAX_READ: u32 = 4 << 20;
/// Most bytes the host copies out of the guest for one call or one import
/// (strings and lists). Larger than the largest frame, so a legal answer fits.
pub const HOSTCALL_FUEL: usize = 32 << 20;
/// Log lines a plugin may write per call, and the longest line.
pub const MAX_LOG_LINES: u32 = 64;
pub const MAX_LOG_LINE: usize = 1024;

/// The engine settings. The install step compiles with exactly these, or the
/// host refuses the `.cwasm`.
pub fn engine_config() -> Config {
    let mut c = Config::new();
    c.epoch_interruption(true);
    c
}

// ---- limits --------------------------------------------------------------------------------

/// `StoreLimits` for tables and counts, plus a cap on the total of all linear
/// memories (`StoreLimits` alone caps each memory, not their sum).
struct Limiter {
    inner: StoreLimits,
    max_total: usize,
    total: usize,
    /// Set when a growth was refused, so a failed call can say why.
    denied: bool,
}

impl Limiter {
    fn new(max_memory_mb: u32) -> Limiter {
        let max_total = (max_memory_mb as usize) << 20;
        let inner = StoreLimitsBuilder::new().memory_size(max_total).table_elements(1 << 20).instances(256).tables(256).memories(64).build();
        Limiter { inner, max_total, total: 0, denied: false }
    }
}

impl ResourceLimiter for Limiter {
    fn memory_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        let total = self.total.saturating_sub(current).saturating_add(desired);
        if total > self.max_total || !self.inner.memory_growing(current, desired, maximum)? {
            self.denied = true;
            return Ok(false);
        }
        self.total = total;
        Ok(true)
    }

    fn table_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        let ok = self.inner.table_growing(current, desired, maximum)?;
        self.denied |= !ok;
        Ok(ok)
    }

    fn instances(&self) -> usize {
        self.inner.instances()
    }
    fn tables(&self) -> usize {
        self.inner.tables()
    }
    fn memories(&self) -> usize {
        self.inner.memories()
    }
}

// ---- the watchdog ----------------------------------------------------------------------------

#[derive(Default)]
struct Watch {
    /// The deadline of the call that is running, if any.
    deadline: Option<Instant>,
    quit: bool,
}

/// Interrupts a guest that runs past its deadline. The thread sleeps on a
/// condition variable while no call runs: an idle host does not wake up.
struct Watchdog {
    shared: Arc<(Mutex<Watch>, Condvar)>,
}

impl Watchdog {
    fn start(engine: Engine) -> io::Result<Watchdog> {
        let shared = Arc::new((Mutex::new(Watch::default()), Condvar::new()));
        let s = shared.clone();
        std::thread::Builder::new().name("watchdog".into()).spawn(move || {
            let (lock, cv) = &*s;
            let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !w.quit {
                w = match w.deadline {
                    None => cv.wait(w).unwrap_or_else(|e| e.into_inner()),
                    Some(d) => {
                        let now = Instant::now();
                        if now >= d {
                            // Still the same call (the guard clears `deadline` under this lock).
                            engine.increment_epoch();
                            w.deadline = None;
                            w
                        } else {
                            cv.wait_timeout(w, d - now).unwrap_or_else(|e| e.into_inner()).0
                        }
                    }
                };
            }
        })?;
        Ok(Watchdog { shared })
    }

    /// Arms the watchdog until the returned guard is dropped.
    fn arm(&self, timeout: Duration) -> Armed<'_> {
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap_or_else(|e| e.into_inner()).deadline = Some(Instant::now() + timeout);
        cv.notify_one();
        Armed(self)
    }
}

struct Armed<'a>(&'a Watchdog);

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        // After this returns the watchdog can no longer fire for this call.
        self.0.shared.0.lock().unwrap_or_else(|e| e.into_inner()).deadline = None;
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.shared.0.lock().unwrap_or_else(|e| e.into_inner()).quit = true;
        self.shared.1.notify_one();
    }
}

// ---- what the guest may call ----------------------------------------------------------------------

/// The store's data: one plugin's limits and grants.
struct Ctx {
    limits: Limiter,
    policy: PathPolicy,
    caps: Capabilities,
    plugin_id: String,
    log_left: u32,
}

/// 100 ns steps since 1601-01-01 UTC.
fn filetime(t: io::Result<SystemTime>) -> u64 {
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600;
    match t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => (d.as_secs() + EPOCH_DIFF_SECS) * 10_000_000 + u64::from(d.subsec_nanos() / 100),
        None => 0,
    }
}

fn fs_error(e: &io::Error) -> wit_host::FsError {
    match e.kind() {
        io::ErrorKind::NotFound => wit_host::FsError::NotFound,
        _ => wit_host::FsError::Io,
    }
}

/// The resolved (link-free) path as a comparison key.
fn real_key(path: &str) -> io::Result<String> {
    let real = std::fs::canonicalize(path)?;
    let s = real.to_str().ok_or_else(|| io::Error::other("path is not unicode"))?;
    caps::normalize(s).map(|n| caps::key(&n)).map_err(io::Error::other)
}

impl Ctx {
    fn new(load: &Load) -> Ctx {
        Ctx { limits: Limiter::new(load.caps.max_memory_mb), policy: PathPolicy::new(&load.caps), caps: load.caps.clone(), plugin_id: load.plugin_id.clone(), log_left: MAX_LOG_LINES }
    }

    /// The path to open for `want`, or why not. Two checks: the lexical one
    /// (`PathPolicy`), then the resolved path must still be inside the folder
    /// that allowed it, so a link or junction can not lead out of a grant.
    fn resolve(&mut self, path: &str, want: Access) -> Result<String, wit_host::FsError> {
        let (norm, roots) = self.policy.check(path, want).map_err(|d| match d {
            Denied::Invalid(_) => wit_host::FsError::InvalidPath,
            Denied::NotGranted => wit_host::FsError::Denied,
        })?;
        let k = caps::key(&norm);
        // "Any path", or exactly a path the user handed over: nothing to stay inside.
        if roots.iter().any(|r| match r {
            Root::Any => true,
            Root::Dir(d) => caps::key(d) == k,
        }) {
            return Ok(norm);
        }
        // A file that is about to be created has no resolved path yet: resolve its folder.
        let real = match real_key(&norm) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound && want == Access::ReadWrite => {
                let (dir, name) = norm.rsplit_once('\\').ok_or(wit_host::FsError::InvalidPath)?;
                let dir = real_key(dir).map_err(|e| fs_error(&e))?;
                format!("{}\\{}", dir.trim_end_matches('\\'), name.to_ascii_lowercase())
            }
            Err(e) => return Err(fs_error(&e)),
        };
        for r in &roots {
            if let Root::Dir(d) = r
                && let Ok(root) = real_key(d)
                && caps::is_under(&real, &root)
            {
                return Ok(norm);
            }
        }
        Err(wit_host::FsError::Denied)
    }
}

impl wit::Host for Ctx {}

impl wit_host::Host for Ctx {
    fn get_capabilities(&mut self) -> wit_host::Capabilities {
        wit_host::Capabilities {
            network: self.caps.network,
            run_processes: self.caps.run_processes,
            max_memory_mb: self.caps.max_memory_mb,
            timeout_ms: self.caps.timeout_ms,
            paths: self
                .caps
                .paths
                .iter()
                .map(|g| wit_host::PathGrant {
                    scope: match &g.scope {
                        Scope::Prefix(p) => wit_host::Scope::Prefix(p.clone()),
                        Scope::Input => wit_host::Scope::Input,
                        Scope::Any => wit_host::Scope::Any,
                    },
                    access: if g.access == Access::ReadWrite { wit_host::Access::ReadWrite } else { wit_host::Access::Read },
                })
                .collect(),
        }
    }

    fn log(&mut self, level: wit_host::Level, message: String) {
        if self.log_left == 0 {
            return;
        }
        self.log_left -= 1;
        let level = match level {
            wit_host::Level::Debug => "debug",
            wit_host::Level::Info => "info",
            wit_host::Level::Warn => "warn",
            wit_host::Level::Error => "error",
        };
        let mut n = message.len().min(MAX_LOG_LINE);
        while !message.is_char_boundary(n) {
            n -= 1;
        }
        let line: String = message[..n].chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let _ = writeln!(io::stderr(), "[{}] {level}: {line}", self.plugin_id);
    }

    fn stat(&mut self, path: String) -> Result<wit_host::FileInfo, wit_host::FsError> {
        let p = self.resolve(&path, Access::Read)?;
        let m = std::fs::metadata(&p).map_err(|e| fs_error(&e))?;
        Ok(wit_host::FileInfo { dir: m.is_dir(), size: m.len(), mtime: filetime(m.modified()) })
    }

    fn read_file(&mut self, path: String, offset: u64, max_len: u32) -> Result<Vec<u8>, wit_host::FsError> {
        let p = self.resolve(&path, Access::Read)?;
        let mut f = std::fs::File::open(&p).map_err(|e| fs_error(&e))?;
        if offset > 0 {
            f.seek(SeekFrom::Start(offset)).map_err(|e| fs_error(&e))?;
        }
        let want = max_len.min(MAX_READ);
        let mut buf = Vec::new();
        f.take(u64::from(want)).read_to_end(&mut buf).map_err(|e| fs_error(&e))?;
        Ok(buf)
    }

    fn read_dir(&mut self, path: String, skip: u32, max: u32) -> Result<Vec<wit::Entry>, wit_host::FsError> {
        let p = self.resolve(&path, Access::Read)?;
        let max = (max as usize).min(proto::MAX_BATCH);
        let mut out = Vec::new();
        for e in std::fs::read_dir(&p).map_err(|e| fs_error(&e))?.skip(skip as usize).take(max) {
            let e = e.map_err(|e| fs_error(&e))?;
            let (dir, size, mtime) = match e.metadata() {
                Ok(m) => (m.is_dir(), m.len(), filetime(m.modified())),
                Err(_) => (false, 0, 0),
            };
            out.push(wit::Entry { name: e.file_name().to_string_lossy().into_owned(), dir, size, mtime });
        }
        Ok(out)
    }

    fn write_file(&mut self, path: String, data: Vec<u8>, append: bool) -> Result<(), wit_host::FsError> {
        let p = self.resolve(&path, Access::ReadWrite)?;
        let mut o = std::fs::OpenOptions::new();
        if append { o.append(true).create(true) } else { o.write(true).create(true).truncate(true) };
        let mut f = o.open(&p).map_err(|e| fs_error(&e))?;
        f.write_all(&data).map_err(|e| fs_error(&e))
    }
}

// ---- one loaded plugin -----------------------------------------------------------------------

/// A live instance and the entry points it exports.
struct Live {
    store: Store<Ctx>,
    meta: meta::Guest,
    column: Option<column::Guest>,
    previewer: Option<previewer::Guest>,
    vfs: Option<vfs::Guest>,
    search: Option<search::Guest>,
    command: Option<command::Guest>,
}

struct Loaded {
    load: Load,
    pre: InstancePre<Ctx>,
    meta: meta::GuestIndices,
    column: Option<column::GuestIndices>,
    previewer: Option<previewer::GuestIndices>,
    vfs: Option<vfs::GuestIndices>,
    search: Option<search::GuestIndices>,
    command: Option<command::GuestIndices>,
    timeout: Duration,
    live: Option<Live>,
}

type Failure = (ErrKind, String);

fn fail<T>(kind: ErrKind, msg: impl Into<String>) -> Result<T, Failure> {
    Err((kind, msg.into()))
}

impl Loaded {
    /// Reads, checks and links the component, then starts it and asks who it is.
    fn new(engine: &Engine, watchdog: &Watchdog, load: Load) -> Result<(Loaded, Info), String> {
        load.caps.validate().map_err(|e| e.to_string())?;
        if !load.component.to_ascii_lowercase().ends_with(".cwasm") {
            return Err("the component must be a `.cwasm` written by the install step".into());
        }
        let size = std::fs::metadata(&load.component).map_err(|e| format!("can not read the component: {e}"))?.len();
        if size > MAX_COMPONENT_BYTES {
            return Err(format!("the component is larger than {} MB", MAX_COMPONENT_BYTES >> 20));
        }
        let bytes = std::fs::read(&load.component).map_err(|e| format!("can not read the component: {e}"))?;
        if sha256::sha256(&bytes) != load.sha256 {
            return Err("the component's hash does not match the one recorded at install".into());
        }
        // SAFETY: `deserialize` trusts its input to be the output of a wasmtime compile.
        // These are the exact bytes whose SHA-256 the install step recorded when it
        // compiled them (checked just above, on the bytes that are deserialized, so the
        // file can not be swapped in between). A `.cwasm` from a plugin package never
        // gets here: packages ship `.wasm` and the manifest parser refuses `.cwasm`.
        let component = unsafe { Component::deserialize(engine, &bytes) }.map_err(|e| format!("not a component this host can run (compiled for another version?): {e}"))?;
        drop(bytes);

        let mut linker = Linker::<Ctx>::new(engine);
        bindings::Plugin::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c).map_err(|e| e.to_string())?;
        // No WASI: anything else the component imports traps if it is ever called.
        linker.define_unknown_imports_as_traps(&component).map_err(|e| e.to_string())?;
        let pre = linker.instantiate_pre(&component).map_err(|e| format!("can not link the component: {e}"))?;

        let wants = |k: Kind| load.entrypoints.iter().any(|(kind, _)| *kind == k);
        let missing = |k: Kind, e: wasmtime::Error| format!("the manifest declares a {} but the component does not export it: {e}", k.name());
        let meta = meta::GuestIndices::new(&pre).map_err(|e| format!("the component does not export `meta`: {e}"))?;
        let column = wants(Kind::Column).then(|| column::GuestIndices::new(&pre).map_err(|e| missing(Kind::Column, e))).transpose()?;
        let previewer = wants(Kind::Previewer).then(|| previewer::GuestIndices::new(&pre).map_err(|e| missing(Kind::Previewer, e))).transpose()?;
        let vfs = wants(Kind::Vfs).then(|| vfs::GuestIndices::new(&pre).map_err(|e| missing(Kind::Vfs, e))).transpose()?;
        let search = wants(Kind::Search).then(|| search::GuestIndices::new(&pre).map_err(|e| missing(Kind::Search, e))).transpose()?;
        let command = wants(Kind::Command).then(|| command::GuestIndices::new(&pre).map_err(|e| missing(Kind::Command, e))).transpose()?;

        let timeout = Duration::from_millis(u64::from(load.caps.timeout_ms));
        let mut loaded = Loaded { load, pre, meta, column, previewer, vfs, search, command, timeout, live: None };
        let described = loaded.guarded(watchdog, Vec::new(), |l| l.meta.call_describe(&mut l.store)).map_err(|(_, m)| format!("the component failed to start: {m}"))?;
        let mut info = Info { id: described.id, version: described.version, entrypoints: Vec::with_capacity(described.entrypoints.len()) };
        for e in described.entrypoints {
            let kind = match e.kind {
                meta::EntrypointKind::Column => Kind::Column,
                meta::EntrypointKind::Previewer => Kind::Previewer,
                meta::EntrypointKind::Vfs => Kind::Vfs,
                meta::EntrypointKind::Search => Kind::Search,
                meta::EntrypointKind::Command => Kind::Command,
            };
            info.entrypoints.push((kind, e.id));
        }
        if info.id != loaded.load.plugin_id || info.version != loaded.load.version {
            return Err(format!("the component is {} {}, the manifest says {} {}", info.id, info.version, loaded.load.plugin_id, loaded.load.version));
        }
        if let Some((k, id)) = loaded.load.entrypoints.iter().find(|e| !info.entrypoints.contains(e)) {
            return Err(format!("the manifest declares {} `{id}` but the component does not list it", k.name()));
        }
        Ok((loaded, info))
    }

    fn instantiate(&self) -> wasmtime::Result<Live> {
        let mut store = Store::new(self.pre.engine(), Ctx::new(&self.load));
        store.limiter(|c| &mut c.limits);
        store.set_hostcall_fuel(HOSTCALL_FUEL);
        store.epoch_deadline_trap();
        // Instantiation can run guest code, so it gets a deadline like a call.
        store.set_epoch_deadline(1);
        let instance = self.pre.instantiate(&mut store)?;
        Ok(Live {
            meta: self.meta.load(&mut store, &instance)?,
            column: self.column.as_ref().map(|i| i.load(&mut store, &instance)).transpose()?,
            previewer: self.previewer.as_ref().map(|i| i.load(&mut store, &instance)).transpose()?,
            vfs: self.vfs.as_ref().map(|i| i.load(&mut store, &instance)).transpose()?,
            search: self.search.as_ref().map(|i| i.load(&mut store, &instance)).transpose()?,
            command: self.command.as_ref().map(|i| i.load(&mut store, &instance)).transpose()?,
            store,
        })
    }

    /// Runs `f` on the live instance under the deadline. `inputs` are the paths
    /// of this call (what an `$input` grant covers). Any failure drops the
    /// instance; the next call gets a fresh one.
    fn guarded<T>(&mut self, watchdog: &Watchdog, inputs: Vec<String>, f: impl FnOnce(&mut Live) -> wasmtime::Result<T>) -> Result<T, Failure> {
        let armed = watchdog.arm(self.timeout);
        let result = (|| {
            if self.live.is_none() {
                self.live = Some(self.instantiate()?);
            }
            let live = self.live.as_mut().expect("just set");
            let ctx = live.store.data_mut();
            ctx.limits.denied = false;
            ctx.log_left = MAX_LOG_LINES;
            ctx.policy.set_inputs(inputs);
            live.store.set_epoch_deadline(1);
            f(live)
        })();
        drop(armed);
        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                let denied = self.live.as_ref().is_some_and(|l| l.store.data().limits.denied);
                self.live = None;
                if e.downcast_ref::<Trap>() == Some(&Trap::Interrupt) {
                    fail(ErrKind::Timeout, format!("no answer within {} ms", self.timeout.as_millis()))
                } else if denied {
                    fail(ErrKind::Memory, format!("more than {} MB of memory", self.load.caps.max_memory_mb))
                } else {
                    fail(ErrKind::Trap, format!("{e:#}"))
                }
            }
        }
    }

    fn declares(&self, kind: Kind, id: &str) -> Result<(), Failure> {
        if self.load.entrypoints.iter().any(|(k, e)| *k == kind && e == id) { Ok(()) } else { fail(ErrKind::Unsupported, format!("{} `{id}`", kind.name())) }
    }

    /// The paths an `$input` grant covers, or nothing when no grant asks.
    fn inputs(&self, paths: impl FnOnce() -> Vec<String>) -> Vec<String> {
        let uses = self.load.caps.paths.iter().any(|g| g.scope == Scope::Input);
        if uses { paths() } else { Vec::new() }
    }

    fn call(&mut self, watchdog: &Watchdog, id: u32, req: Request) -> Result<Vec<u8>, Failure> {
        let too_large = |_| (ErrKind::BadResult, "the answer is larger than a frame can hold".to_string());
        match req {
            Request::Column { column, paths } => {
                self.declares(Kind::Column, &column)?;
                let n = paths.len();
                let inputs = self.inputs(|| paths.clone());
                let cells = self.guarded(watchdog, inputs, |l| l.column.as_ref().expect("declared").call_values(&mut l.store, &column, &paths))?;
                if cells.len() != n {
                    return fail(ErrKind::BadResult, format!("{} values for {n} paths", cells.len()));
                }
                let cells: Vec<api::Cell> = cells
                    .into_iter()
                    .map(|c| match c {
                        wit::Cell::Empty => api::Cell::Empty,
                        wit::Cell::Text(s) => api::Cell::Text(s),
                        wit::Cell::Int(v) => api::Cell::Int(v),
                        wit::Cell::Bytes(v) => api::Cell::Bytes(v),
                        wit::Cell::Time(v) => api::Cell::Time(v),
                    })
                    .collect();
                proto::enc_ok_cells(id, &cells).map_err(too_large)
            }
            Request::Preview { id: previewer, path, limits } => {
                self.declares(Kind::Previewer, &previewer)?;
                let inputs = self.inputs(|| vec![path.clone()]);
                let lim = wit::Limits { max_bytes: limits.max_bytes, max_px: limits.max_px };
                let nodes = self
                    .guarded(watchdog, inputs, |l| l.previewer.as_ref().expect("declared").call_preview(&mut l.store, &previewer, &path, lim))?
                    .map_err(|e| (ErrKind::Plugin, e))?;
                let mut doc = Vec::with_capacity(nodes.len());
                for n in nodes {
                    doc.push(match n {
                        wit::Node::Heading(s) => api::Node::Heading(s),
                        wit::Node::Text(text) => api::Node::Text { text, dim: false },
                        wit::Node::DimText(text) => api::Node::Text { text, dim: true },
                        wit::Node::Mono(s) => api::Node::Mono(s),
                        wit::Node::Pair((a, b)) => api::Node::Pair(a, b),
                        wit::Node::Image(i) => {
                            if !proto::image_ok(i.width, i.height, i.bgra.len()) {
                                return fail(ErrKind::BadResult, "an image's size does not match its pixels");
                            }
                            if i.width > limits.max_px || i.height > limits.max_px {
                                return fail(ErrKind::BadResult, format!("an image of {}x{} is over the {} px limit", i.width, i.height, limits.max_px));
                            }
                            api::Node::Image { w: i.width, h: i.height, bgra: i.bgra }
                        }
                        wit::Node::Button(b) => api::Node::Button { label: b.label, command: b.command.into() },
                        wit::Node::Indent => api::Node::Indent,
                        wit::Node::Outdent => api::Node::Outdent,
                    });
                }
                proto::enc_ok_doc(id, &doc).map_err(too_large)
            }
            Request::VfsList { id: vfs, path, cursor } => {
                self.declares(Kind::Vfs, &vfs)?;
                let inputs = self.inputs(|| vfs_inputs(&path));
                let page = self
                    .guarded(watchdog, inputs, |l| l.vfs.as_ref().expect("declared").call_list_dir(&mut l.store, &vfs, &path, cursor.as_deref()))?
                    .map_err(|e| (ErrKind::Plugin, e))?;
                if page.entries.len() > proto::MAX_BATCH {
                    return fail(ErrKind::BadResult, format!("a page of {} entries, at most {} are allowed", page.entries.len(), proto::MAX_BATCH));
                }
                let entries: Vec<api::VfsEntry> = page.entries.into_iter().map(|e| api::VfsEntry { name: e.name, dir: e.dir, size: e.size, mtime: e.mtime }).collect();
                proto::enc_ok_entries(id, &entries, page.next.as_deref()).map_err(too_large)
            }
            Request::VfsRead { id: vfs, path, offset, max_len } => {
                self.declares(Kind::Vfs, &vfs)?;
                let max_len = max_len.min(MAX_READ);
                let inputs = self.inputs(|| vfs_inputs(&path));
                let data = self
                    .guarded(watchdog, inputs, |l| l.vfs.as_ref().expect("declared").call_read(&mut l.store, &vfs, &path, offset, max_len))?
                    .map_err(|e| (ErrKind::Plugin, e))?;
                if data.len() > max_len as usize {
                    return fail(ErrKind::BadResult, format!("{} bytes when at most {max_len} were asked for", data.len()));
                }
                proto::enc_ok_bytes(id, &data).map_err(too_large)
            }
            Request::Search { id: provider, text, root, limit, cursor } => {
                self.declares(Kind::Search, &provider)?;
                let inputs = self.inputs(|| if root.is_empty() { Vec::new() } else { vec![root.clone()] });
                let q = search::Query { text, root, limit };
                let page = self
                    .guarded(watchdog, inputs, |l| l.search.as_ref().expect("declared").call_find(&mut l.store, &provider, &q, cursor.as_deref()))?
                    .map_err(|e| (ErrKind::Plugin, e))?;
                if page.hits.len() > proto::MAX_BATCH {
                    return fail(ErrKind::BadResult, format!("a page of {} hits, at most {} are allowed", page.hits.len(), proto::MAX_BATCH));
                }
                let hits: Vec<api::Hit> = page.hits.into_iter().map(|h| api::Hit { path: h.path, dir: h.dir, size: h.size, mtime: h.mtime }).collect();
                proto::enc_ok_hits(id, &hits, page.next.as_deref()).map_err(too_large)
            }
            Request::Command { name, folder, selection, part, parts } => {
                self.declares(Kind::Command, &name)?;
                let inputs = self.inputs(|| {
                    let mut v = selection.clone();
                    v.push(folder.clone());
                    v
                });
                let cx = command::Context { folder, selection, part, parts };
                let msg = self
                    .guarded(watchdog, inputs, |l| l.command.as_ref().expect("declared").call_run(&mut l.store, &name, &cx))?
                    .map_err(|e| (ErrKind::Plugin, e))?;
                proto::enc_ok_message(id, msg.as_deref()).map_err(too_large)
            }
            Request::Load(_) | Request::Shutdown => fail(ErrKind::BadRequest, "not a call"),
        }
    }
}

/// What an `$input` grant covers for a vfs call: the path, and the file it
/// goes through (`C:\a.zip` for `C:\a.zip\inner\x`), which is what the
/// provider has to read.
fn vfs_inputs(path: &str) -> Vec<String> {
    let mut out = vec![path.to_string()];
    if let Ok(norm) = caps::normalize(path) {
        let mut at = norm.as_str();
        while let Some((parent, _)) = at.rsplit_once('\\') {
            if parent.len() < 3 {
                break;
            }
            if std::fs::metadata(parent).is_ok_and(|m| m.is_file()) {
                out.push(parent.to_string());
                break;
            }
            at = parent;
        }
    }
    out
}

/// Serves one client until it says shutdown or closes the stream.
/// Says hello first, then answers each request with one reply.
pub fn serve(mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let engine = Engine::new(&engine_config()).map_err(|e| io::Error::other(e.to_string()))?;
    let watchdog = Watchdog::start(engine.clone())?;
    proto::write_frame(&mut output, &proto::enc_hello())?;
    let mut loaded: Option<Loaded> = None;
    let mut buf = Vec::new();
    // A frame that can not be read (too large, cut off) ends the session: the stream is out of step.
    while proto::read_frame(&mut input, &mut buf)? {
        let (id, req) = match proto::decode_request(&buf) {
            Ok(r) => r,
            Err(e) => {
                let id = u32::from_le_bytes(buf[..4].try_into().expect("a frame has a header"));
                proto::write_frame(&mut output, &proto::enc_err(id, ErrKind::BadRequest, e.0))?;
                continue;
            }
        };
        let frame = match req {
            Request::Shutdown => {
                proto::write_frame(&mut output, &proto::enc_ok_empty(id))?;
                break;
            }
            Request::Load(_) if loaded.is_some() => proto::enc_err(id, ErrKind::BadRequest, "a component is already loaded"),
            Request::Load(load) => match Loaded::new(&engine, &watchdog, load) {
                Ok((l, info)) => match proto::enc_ok_info(id, &info) {
                    Ok(f) => {
                        loaded = Some(l);
                        f
                    }
                    Err(e) => proto::enc_err(id, ErrKind::Load, e.0),
                },
                Err(msg) => proto::enc_err(id, ErrKind::Load, &msg),
            },
            call => match loaded.as_mut() {
                None => proto::enc_err(id, ErrKind::NotLoaded, "no component is loaded"),
                Some(l) => l.call(&watchdog, id, call).unwrap_or_else(|(kind, msg)| proto::enc_err(id, kind, &msg)),
            },
        };
        proto::write_frame(&mut output, &frame)?;
    }
    Ok(())
}

/// The install step: compiles a component for this host. Only with the
/// `compiler` feature, which the shipped host exe is never built with.
#[cfg(feature = "compiler")]
pub mod install {
    use super::engine_config;
    use crate::manifest::Manifest;
    use crate::sha256;
    use std::path::{Path, PathBuf};
    use wasmtime::Engine;

    /// Largest `.wasm` a package may ship.
    pub const MAX_WASM_BYTES: u64 = 64 << 20;

    pub struct Installed {
        pub manifest: Manifest,
        /// The `.cwasm` that was written.
        pub component: PathBuf,
        /// Its SHA-256: store it, and pass it in every `Load`.
        pub sha256: [u8; 32],
    }

    /// Compiles `.wasm` bytes to the `.cwasm` bytes the host loads.
    pub fn compile(wasm: &[u8]) -> Result<Vec<u8>, String> {
        let engine = Engine::new(&engine_config()).map_err(|e| e.to_string())?;
        engine.precompile_component(wasm).map_err(|e| format!("not a valid component: {e:#}"))
    }

    /// Checks the manifest in `plugin_dir`, compiles the component it names and
    /// writes `<out_dir>/<id>-<version>.cwasm`. `out_dir` must be a folder only
    /// the app writes to.
    pub fn install(plugin_dir: &Path, out_dir: &Path) -> Result<Installed, String> {
        let manifest = Manifest::load(plugin_dir).map_err(|e| e.to_string())?;
        let source = plugin_dir.join(&manifest.component);
        let size = std::fs::metadata(&source).map_err(|e| format!("{}: {e}", source.display()))?.len();
        if size > MAX_WASM_BYTES {
            return Err(format!("{} is larger than {} MB", source.display(), MAX_WASM_BYTES >> 20));
        }
        let wasm = std::fs::read(&source).map_err(|e| format!("{}: {e}", source.display()))?;
        let cwasm = compile(&wasm)?;
        std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
        let component = out_dir.join(format!("{}-{}.cwasm", manifest.id, manifest.version));
        // Write beside the target and rename, so a half-written file is never loaded.
        let tmp = out_dir.join(format!("{}-{}.cwasm.tmp", manifest.id, manifest.version));
        std::fs::write(&tmp, &cwasm).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &component).map_err(|e| format!("{}: {e}", component.display()))?;
        Ok(Installed { manifest, component, sha256: sha256::sha256(&cwasm) })
    }
}
