//! The app's side: one [`Plugin`] supervises one `tsubame-plughost.exe`.
//!
//! - **Lazy.** Creating a `Plugin` starts nothing: no process, no thread. The
//!   host is started by the first call, and again after it was stopped.
//! - **Deadlines.** Every call waits at most the plugin's `timeout_ms` plus a
//!   short grace. The host interrupts a slow guest itself; if the host does not
//!   answer either, its process is killed.
//! - **Contained.** A timeout, a crash, a trap or a broken reply comes back as
//!   a [`CallError`]. Nothing here panics or blocks past the deadline. After
//!   [`Options::max_faults`] failures in a row the plugin is marked faulted and
//!   calls fail at once, without starting a host, until [`Plugin::reset`].
//! - **Batches.** Path lists are cut into calls of at most 2,000 paths.
//!
//! Calls on one plugin run one at a time (the host is single threaded); call
//! from a worker thread, never from the UI thread.

use crate::proto::{self, Dec, ErrKind, Info, Load, ProtoError};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tsubame_ext_api::{Cell, Hit, Limits, Node, VfsEntry};

/// Bytes asked for per `vfs.read` call.
pub const VFS_READ_CHUNK: u32 = 1 << 20;
/// Most pages one listing or search may take (2,000 items each).
pub const MAX_PAGES: usize = 5_000;

// ---- starting a host ---------------------------------------------------------------------

/// A running host: its two pipes and a handle to end it.
pub struct HostIo {
    /// The host's stdin.
    pub input: Box<dyn Write + Send>,
    /// The host's stdout.
    pub output: Box<dyn Read + Send>,
    pub process: Box<dyn HostProcess>,
}

pub trait HostProcess: Send {
    /// Ends the process now and reaps it. Harmless on a process that is gone.
    fn kill(&mut self);
    /// Waits up to `timeout` for the process to end by itself; true if it did.
    fn wait(&mut self, timeout: Duration) -> bool;
}

/// Starts hosts. The app uses `crate::launcher::JobLauncher` (job object, cleared
/// environment, refuses an elevated app); tests use [`ProcessLauncher`] or their own.
pub trait Launcher: Send + Sync {
    fn launch(&self) -> io::Result<HostIo>;
}

/// Starts `tsubame-plughost.exe` with piped stdin and stdout and no window.
///
/// **Test-only.** It applies no containment at all: the host gets the app's whole
/// environment, current folder and inheritable handles, and nothing limits its
/// processes or memory or ends it when the app dies (PH-07). The app must use
/// `crate::launcher::JobLauncher`.
pub struct ProcessLauncher {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// Let the host's log lines through to this process's stderr (off: dropped).
    pub inherit_stderr: bool,
}

impl ProcessLauncher {
    pub fn new(exe: impl Into<PathBuf>) -> ProcessLauncher {
        ProcessLauncher { exe: exe.into(), args: vec!["serve".into()], inherit_stderr: false }
    }
}

struct ChildProcess(Child);

impl HostProcess for ChildProcess {
    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
    fn wait(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            match self.0.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(2)),
                _ => return false,
            }
        }
    }
}

impl Launcher for ProcessLauncher {
    fn launch(&self) -> io::Result<HostIo> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(&self.args).stdin(Stdio::piped()).stdout(Stdio::piped());
        cmd.stderr(if self.inherit_stderr { Stdio::inherit() } else { Stdio::null() });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: the host is a console program; never show a console for it.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn()?;
        let input = child.stdin.take().ok_or_else(|| io::Error::other("no stdin pipe"))?;
        let output = child.stdout.take().ok_or_else(|| io::Error::other("no stdout pipe"))?;
        Ok(HostIo { input: Box::new(input), output: Box::new(output), process: Box::new(ChildProcess(child)) })
    }
}

// ---- errors and options --------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The plugin failed too often and is switched off until [`Plugin::reset`].
    Faulted(String),
    /// The host could not be started, or did not introduce itself.
    Spawn(String),
    /// The host refused the component (hash, format, identity, missing export).
    Load(String),
    /// The call ran past its deadline. The guest was interrupted or the host killed.
    Timeout,
    /// The host process ended in the middle of the call.
    Crashed,
    /// The guest trapped.
    Trap(String),
    /// The guest asked for more memory than its limit.
    Memory,
    /// The plugin returned an error of its own. Not a fault.
    Plugin(String),
    /// The plugin's answer breaks the contract.
    BadResult(String),
    /// The host and the client disagree about the protocol.
    Protocol(String),
    /// The plugin has no such entry point.
    Unsupported(String),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Faulted(why) => write!(f, "plugin is switched off after repeated failures ({why})"),
            CallError::Spawn(e) => write!(f, "plugin host did not start: {e}"),
            CallError::Load(e) => write!(f, "plugin could not be loaded: {e}"),
            CallError::Timeout => f.write_str("plugin took too long"),
            CallError::Crashed => f.write_str("plugin host stopped unexpectedly"),
            CallError::Trap(e) => write!(f, "plugin crashed: {e}"),
            CallError::Memory => f.write_str("plugin ran out of memory"),
            CallError::Plugin(e) => f.write_str(e),
            CallError::BadResult(e) => write!(f, "plugin gave a bad answer: {e}"),
            CallError::Protocol(e) => write!(f, "plugin host protocol error: {e}"),
            CallError::Unsupported(e) => write!(f, "plugin has no such entry point: {e}"),
        }
    }
}

impl std::error::Error for CallError {}

impl CallError {
    /// True when the error counts toward [`Options::max_faults`].
    pub fn is_fault(&self) -> bool {
        !matches!(self, CallError::Plugin(_) | CallError::Unsupported(_) | CallError::Faulted(_))
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Failures in a row before the plugin is switched off.
    pub max_faults: u32,
    /// Added to the plugin's `timeout_ms` before the host is killed. The host
    /// interrupts the guest at `timeout_ms`; the grace is its time to say so.
    pub grace: Duration,
    /// Longest wait for the host to start and load the component.
    pub start_timeout: Duration,
    /// Longest wait for the host to exit on a clean shutdown before it is killed.
    pub shutdown_wait: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options { max_faults: 3, grace: Duration::from_millis(250), start_timeout: Duration::from_secs(5), shutdown_wait: Duration::from_millis(500) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// No host process. The next call starts one.
    Idle,
    Running,
    Faulted(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Hosts started.
    pub spawns: u64,
    /// Hosts killed (hang, protocol error, fault limit).
    pub kills: u64,
    /// Requests sent after a successful load.
    pub calls: u64,
    /// Calls that counted as a fault.
    pub faults: u64,
}

/// How long the last start took, step by step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartTimes {
    /// Creating the process.
    pub spawn: Duration,
    /// Until the host said hello (its own start-up).
    pub hello: Duration,
    /// Reading, checking and instantiating the component.
    pub load: Duration,
}

impl Load {
    /// The load request for an installed plugin that
    /// [`install_record::verify`](crate::install_record::verify) passed: the
    /// capabilities and the `.cwasm` hash come from its install record, never
    /// from the live `plugin.toml`.
    pub fn new(v: &crate::install_record::Verified) -> Load {
        Load {
            plugin_id: v.manifest.id.clone(),
            version: v.manifest.version.clone(),
            component: v.component.to_string_lossy().into_owned(),
            sha256: v.cwasm_sha256,
            caps: v.caps.clone(),
            entrypoints: v.manifest.entrypoints(),
            protected: v.protected.clone(),
        }
    }
}

// ---- the supervisor ----------------------------------------------------------------------

enum Event {
    Frame(Vec<u8>),
    Closed,
}

struct Conn {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Event>,
    process: Box<dyn HostProcess>,
}

struct State {
    conn: Option<Conn>,
    next_id: u32,
    faults: u32,
    faulted: Option<String>,
    last_used: Instant,
    stats: Stats,
    last_start: Option<StartTimes>,
}

enum Fail {
    Timeout,
    Closed,
}

pub struct Plugin {
    load: Load,
    launcher: Box<dyn Launcher>,
    opts: Options,
    state: Mutex<State>,
}

fn read_loop(mut output: Box<dyn Read + Send>, tx: Sender<Event>) {
    loop {
        let mut buf = Vec::new();
        match proto::read_frame(&mut output, &mut buf) {
            Ok(true) => {
                if tx.send(Event::Frame(buf)).is_err() {
                    return;
                }
            }
            _ => {
                let _ = tx.send(Event::Closed);
                return;
            }
        }
    }
}

fn write_loop(mut input: Box<dyn Write + Send>, rx: Receiver<Vec<u8>>) {
    // Ends when the sender is dropped; dropping `input` then closes the host's stdin.
    for frame in rx {
        if proto::write_frame(&mut input, &frame).is_err() {
            return;
        }
    }
}

impl Conn {
    fn open(io: HostIo) -> io::Result<Conn> {
        let (tx, write_rx) = mpsc::channel::<Vec<u8>>();
        let (read_tx, rx) = mpsc::channel::<Event>();
        let (input, output) = (io.input, io.output);
        let mut process = io.process;
        let spawned = std::thread::Builder::new()
            .name("plughost-write".into())
            .spawn(move || write_loop(input, write_rx))
            .and_then(|_| std::thread::Builder::new().name("plughost-read".into()).spawn(move || read_loop(output, read_tx)));
        if let Err(e) = spawned {
            process.kill();
            return Err(e);
        }
        Ok(Conn { tx, rx, process })
    }

    fn recv(&self, deadline: Instant) -> Result<Vec<u8>, Fail> {
        match self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Event::Frame(f)) => Ok(f),
            Ok(Event::Closed) | Err(RecvTimeoutError::Disconnected) => Err(Fail::Closed),
            Err(RecvTimeoutError::Timeout) => Err(Fail::Timeout),
        }
    }

    fn round_trip(&self, frame: Vec<u8>, deadline: Instant) -> Result<Vec<u8>, Fail> {
        self.tx.send(frame).map_err(|_| Fail::Closed)?;
        self.recv(deadline)
    }

    /// Ends the host now.
    fn kill(mut self) {
        self.process.kill();
    }
}

impl Plugin {
    /// Nothing is started here.
    pub fn new(load: Load, launcher: impl Launcher + 'static, opts: Options) -> Plugin {
        let state = State { conn: None, next_id: 1, faults: 0, faulted: None, last_used: Instant::now(), stats: Stats::default(), last_start: None };
        Plugin { load, launcher: Box::new(launcher), opts, state: Mutex::new(state) }
    }

    pub fn id(&self) -> &str {
        &self.load.plugin_id
    }

    pub fn load(&self) -> &Load {
        &self.load
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock can only come from a bug here; the
        // state is still usable, so carry on rather than poison every caller.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> Status {
        let st = self.lock();
        match (&st.faulted, &st.conn) {
            (Some(why), _) => Status::Faulted(why.clone()),
            (None, Some(_)) => Status::Running,
            (None, None) => Status::Idle,
        }
    }

    pub fn stats(&self) -> Stats {
        self.lock().stats
    }

    /// The timings of the most recent successful start, for measurements.
    pub fn last_start(&self) -> Option<StartTimes> {
        self.lock().last_start
    }

    /// Clears a fault so the next call starts a fresh host.
    pub fn reset(&self) {
        let mut st = self.lock();
        st.faulted = None;
        st.faults = 0;
    }

    /// Stops the host if there is one: asks it to exit, then kills it if it
    /// has not within [`Options::shutdown_wait`]. The next call starts it again.
    pub fn shutdown(&self) {
        let mut st = self.lock();
        self.stop(&mut st);
    }

    /// Stops the host when no call was made for `idle`. True if it was stopped.
    /// The app calls this from events it already has; there is no timer here.
    pub fn stop_if_idle(&self, idle: Duration) -> bool {
        let mut st = self.lock();
        if st.conn.is_some() && st.last_used.elapsed() >= idle {
            self.stop(&mut st);
            return true;
        }
        false
    }

    fn stop(&self, st: &mut State) {
        let Some(conn) = st.conn.take() else { return };
        let id = st.next_id;
        st.next_id = st.next_id.wrapping_add(1);
        let end = Instant::now() + self.opts.shutdown_wait;
        let Conn { tx, rx, mut process } = conn;
        let _ = tx.send(proto::enc_shutdown(id));
        // Dropping the sender ends the writer thread, which closes the host's stdin.
        drop(tx);
        // The host answers the shutdown, then closes its stdout.
        while let Ok(Event::Frame(_)) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {}
        if !process.wait(end.saturating_duration_since(Instant::now())) {
            st.stats.kills += 1;
        }
        process.kill();
    }

    fn fault(&self, st: &mut State, kill: bool, err: CallError) -> CallError {
        if kill && let Some(conn) = st.conn.take() {
            conn.kill();
            st.stats.kills += 1;
        }
        st.faults += 1;
        st.stats.faults += 1;
        if st.faults >= self.opts.max_faults {
            st.faulted = Some(err.to_string());
            if let Some(conn) = st.conn.take() {
                conn.kill();
                st.stats.kills += 1;
            }
        }
        err
    }

    /// Starts the host and loads the component.
    fn start(&self, st: &mut State) -> Result<(), CallError> {
        let t0 = Instant::now();
        let io = self.launcher.launch().map_err(|e| CallError::Spawn(e.to_string()))?;
        let mut times = StartTimes { spawn: t0.elapsed(), ..StartTimes::default() };
        st.stats.spawns += 1;
        let conn = Conn::open(io).map_err(|e| CallError::Spawn(e.to_string()))?;
        let deadline = Instant::now() + self.opts.start_timeout;
        let id = st.next_id;
        st.next_id = st.next_id.wrapping_add(1);
        let result = (|| {
            let hello = match conn.recv(deadline) {
                Ok(f) => proto::decode_hello(&f).map_err(|e| CallError::Spawn(e.to_string()))?,
                Err(Fail::Timeout) => return Err(CallError::Spawn("the host did not answer".into())),
                Err(Fail::Closed) => return Err(CallError::Spawn("the host exited at once".into())),
            };
            times.hello = t0.elapsed() - times.spawn;
            if hello.protocol != proto::VERSION {
                return Err(CallError::Spawn(format!("the host speaks protocol {}, this build speaks {}", hello.protocol, proto::VERSION)));
            }
            let frame = proto::enc_load(id, &self.load).map_err(|e| CallError::Protocol(e.to_string()))?;
            let reply = match conn.round_trip(frame, deadline) {
                Ok(f) => f,
                Err(Fail::Timeout) => return Err(CallError::Load("loading took too long".into())),
                Err(Fail::Closed) => return Err(CallError::Load("the host exited while loading".into())),
            };
            let r = proto::decode_reply(&reply).map_err(|e| CallError::Protocol(e.to_string()))?;
            if r.id != id {
                return Err(CallError::Protocol("reply id does not match".into()));
            }
            let info: Info = match r.body {
                Ok(d) => proto::dec_info(d).map_err(|e| CallError::Protocol(e.to_string()))?,
                Err((_, msg)) => return Err(CallError::Load(msg)),
            };
            // The host checks this too; a host that says otherwise is not ours.
            if info.id != self.load.plugin_id || info.version != self.load.version {
                return Err(CallError::Load(format!("the component is {} {}, the manifest says {} {}", info.id, info.version, self.load.plugin_id, self.load.version)));
            }
            if let Some((k, e)) = self.load.entrypoints.iter().find(|e| !info.entrypoints.contains(e)) {
                return Err(CallError::Load(format!("the component has no {} `{e}`", k.name())));
            }
            times.load = t0.elapsed() - times.spawn - times.hello;
            Ok(())
        })();
        match result {
            Ok(()) => {
                st.conn = Some(conn);
                st.last_start = Some(times);
                Ok(())
            }
            Err(e) => {
                conn.kill();
                st.stats.kills += 1;
                Err(e)
            }
        }
    }

    /// One request and its reply. `build` gets the request id; `read` decodes
    /// the body of an OK reply.
    fn call<T>(&self, build: impl FnOnce(u32) -> Result<Vec<u8>, ProtoError>, read: impl FnOnce(Dec) -> Result<T, ProtoError>) -> Result<T, CallError> {
        let mut st = self.lock();
        if let Some(why) = &st.faulted {
            return Err(CallError::Faulted(why.clone()));
        }
        if st.conn.is_none()
            && let Err(e) = self.start(&mut st)
        {
            return Err(self.fault(&mut st, false, e));
        }
        let id = st.next_id;
        st.next_id = st.next_id.wrapping_add(1);
        // Too large to send: the caller's mistake, the host never sees it.
        let frame = build(id).map_err(|e| CallError::Protocol(e.to_string()))?;
        st.stats.calls += 1;
        st.last_used = Instant::now();
        let deadline = Instant::now() + Duration::from_millis(u64::from(self.load.caps.timeout_ms)) + self.opts.grace;
        let conn = st.conn.as_ref().expect("started above");
        let reply = match conn.round_trip(frame, deadline) {
            Ok(f) => f,
            Err(Fail::Timeout) => return Err(self.fault(&mut st, true, CallError::Timeout)),
            Err(Fail::Closed) => return Err(self.fault(&mut st, true, CallError::Crashed)),
        };
        st.last_used = Instant::now();
        let r = match proto::decode_reply(&reply) {
            Ok(r) if r.id == id => r,
            Ok(_) => return Err(self.fault(&mut st, true, CallError::Protocol("reply id does not match".into()))),
            Err(e) => return Err(self.fault(&mut st, true, CallError::Protocol(e.to_string()))),
        };
        match r.body {
            Ok(d) => match read(d) {
                Ok(v) => {
                    st.faults = 0;
                    Ok(v)
                }
                Err(e) => Err(self.fault(&mut st, true, CallError::Protocol(e.to_string()))),
            },
            Err((kind, msg)) => match kind {
                ErrKind::Plugin => {
                    st.faults = 0;
                    Err(CallError::Plugin(crate::sanitize::sanitize(&msg, crate::sanitize::MAX_MESSAGE)))
                }
                ErrKind::Unsupported => Err(CallError::Unsupported(msg)),
                // The host dropped the instance and is ready for the next call.
                ErrKind::Timeout => Err(self.fault(&mut st, false, CallError::Timeout)),
                ErrKind::Trap => Err(self.fault(&mut st, false, CallError::Trap(msg))),
                ErrKind::Memory => Err(self.fault(&mut st, false, CallError::Memory)),
                ErrKind::BadResult => Err(self.fault(&mut st, false, CallError::BadResult(msg))),
                // The two sides are out of step: start over with a new host.
                ErrKind::BadRequest | ErrKind::NotLoaded | ErrKind::Load => Err(self.fault(&mut st, true, CallError::Protocol(msg))),
            },
        }
    }

    // ---- the entry points ----------------------------------------------------------------

    /// One value per path, appended to `out` in order. More than 2,000 paths
    /// go out as several calls. On an error `out` is left as it was.
    pub fn column_values(&self, column: &str, paths: &[&str], out: &mut Vec<Cell>) -> Result<(), CallError> {
        let start = out.len();
        for chunk in proto::chunks(paths) {
            let before = out.len();
            let r = self.call(|id| proto::enc_column(id, column, chunk), |d| proto::dec_cells(d, out));
            match r {
                Ok(n) if n == chunk.len() => {}
                Ok(n) => {
                    out.truncate(start);
                    let mut st = self.lock();
                    return Err(self.fault(&mut st, true, CallError::Protocol(format!("{n} values for {} paths", chunk.len()))));
                }
                Err(e) => {
                    out.truncate(start);
                    return Err(e);
                }
            }
            debug_assert_eq!(out.len(), before + chunk.len());
        }
        Ok(())
    }

    /// Appends the preview's nodes to `out`.
    pub fn preview(&self, previewer: &str, path: &str, limits: Limits, out: &mut Vec<Node>) -> Result<(), CallError> {
        let start = out.len();
        self.call(|id| proto::enc_preview(id, previewer, path, limits), |d| proto::dec_doc(d, out)).inspect_err(|_| out.truncate(start))
    }

    /// Lists a virtual folder page by page; `sink` returns false to stop.
    pub fn vfs_list(&self, vfs: &str, path: &str, sink: &mut dyn FnMut(&[VfsEntry]) -> bool) -> Result<(), CallError> {
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let (entries, next) = self.call(|id| proto::enc_vfs_list(id, vfs, path, cursor.as_deref()), proto::dec_entries)?;
            if !sink(&entries) || next.is_none() {
                return Ok(());
            }
            cursor = next;
        }
        Err(CallError::BadResult(format!("listing did not end after {MAX_PAGES} pages")))
    }

    /// Reads a virtual file into `out`, at most `max_bytes` of it.
    pub fn vfs_read(&self, vfs: &str, path: &str, max_bytes: u64, out: &mut Vec<u8>) -> Result<(), CallError> {
        let start = out.len();
        let mut offset = 0u64;
        while offset < max_bytes {
            let want = (max_bytes - offset).min(u64::from(VFS_READ_CHUNK)) as u32;
            let got = self.call(|id| proto::enc_vfs_read(id, vfs, path, offset, want), |d| proto::dec_bytes(d, out)).inspect_err(|_| out.truncate(start))?;
            offset += got as u64;
            if got < want as usize {
                break;
            }
        }
        Ok(())
    }

    /// Runs a search page by page until `limit` hits, the end, or `sink` returns false.
    pub fn search(&self, provider: &str, text: &str, root: &str, limit: usize, sink: &mut dyn FnMut(&[Hit]) -> bool) -> Result<(), CallError> {
        let limit32 = limit.min(u32::MAX as usize) as u32;
        let mut cursor: Option<String> = None;
        let mut total = 0usize;
        for _ in 0..MAX_PAGES {
            let (mut hits, next) = self.call(|id| proto::enc_search(id, provider, text, root, limit32, cursor.as_deref()), proto::dec_hits)?;
            hits.truncate(limit - total);
            total += hits.len();
            if !sink(&hits) || next.is_none() || total >= limit {
                return Ok(());
            }
            cursor = next;
        }
        Err(CallError::BadResult(format!("search did not end after {MAX_PAGES} pages")))
    }

    /// Runs a command. A selection of more than 2,000 paths goes out as
    /// several calls (`part` of `parts`); the last message wins.
    pub fn run_command(&self, name: &str, folder: &str, selection: &[&str]) -> Result<Option<String>, CallError> {
        // With nothing selected the command still runs once.
        let chunks: Vec<&[&str]> = if selection.is_empty() { vec![&[]] } else { proto::chunks(selection).collect() };
        let parts = chunks.len() as u32;
        let mut message = None;
        for (part, chunk) in chunks.into_iter().enumerate() {
            if let Some(m) = self.call(|id| proto::enc_command(id, name, folder, chunk, part as u32, parts), proto::dec_message)? {
                message = Some(crate::sanitize::sanitize(&m, crate::sanitize::MAX_MESSAGE));
            }
        }
        Ok(message)
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::Capabilities;
    use crate::manifest::Kind;
    use crate::proto::Request;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// What the in-memory host does with requests after a good hello and load.
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// Answers every call: column values echo the path.
        Echo,
        /// Reads requests and never answers a call.
        Hang,
        /// Never says hello.
        Silent,
        /// Says something that is not a hello.
        Garbage,
        /// Says hello with another protocol version.
        OldProtocol,
        /// Refuses the load.
        RefuseLoad,
        /// Says it is a different plugin.
        WrongIdentity,
        /// Closes its pipes when the first call arrives.
        Die,
        /// Answers with the wrong request id.
        WrongId,
        /// Answers a column call with one value too few.
        Short,
        /// Answers every call with a trap error.
        Trap,
        /// Answers every call with a plugin error.
        PluginError,
        /// Answers the first call late (past the deadline), then behaves.
        Slow,
    }

    #[derive(Default)]
    struct Counters {
        launches: AtomicU32,
        kills: AtomicU32,
        requests: AtomicU32,
        shutdowns: AtomicU32,
        largest_batch: AtomicU32,
    }

    struct Mock {
        mode: Mode,
        /// Mode for the second and later launches.
        later: Mode,
        c: Arc<Counters>,
    }

    struct MockProcess {
        c: Arc<Counters>,
        dead: Arc<AtomicBool>,
        killed: bool,
    }

    impl HostProcess for MockProcess {
        fn kill(&mut self) {
            if !self.killed && !self.dead.load(Ordering::SeqCst) {
                self.c.kills.fetch_add(1, Ordering::SeqCst);
            }
            self.killed = true;
        }
        fn wait(&mut self, timeout: Duration) -> bool {
            let end = Instant::now() + timeout;
            while Instant::now() < end {
                if self.dead.load(Ordering::SeqCst) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            self.dead.load(Ordering::SeqCst)
        }
    }

    fn serve(mode: Mode, c: Arc<Counters>, mut input: io::PipeReader, mut output: io::PipeWriter, dead: Arc<AtomicBool>) {
        let send = |output: &mut io::PipeWriter, f: Vec<u8>| proto::write_frame(output, &f).is_ok();
        match mode {
            Mode::Silent => {}
            Mode::Garbage => {
                let _ = output.write_all(b"\x05\0\0\0hello");
            }
            Mode::OldProtocol => {
                let mut e = proto::Enc::new(0, proto::reply::HELLO);
                for b in proto::MAGIC {
                    e.u8(b);
                }
                e.u16(proto::VERSION + 1).str("9.9.9");
                send(&mut output, e.finish().unwrap());
            }
            _ => {
                send(&mut output, proto::enc_hello());
            }
        }
        let mut buf = Vec::new();
        let mut slow_done = false;
        while let Ok(true) = proto::read_frame(&mut input, &mut buf) {
            let (id, req) = proto::decode_request(&buf).unwrap();
            let frame = match req {
                Request::Load(l) => match mode {
                    Mode::RefuseLoad => proto::enc_err(id, ErrKind::Load, "hash mismatch"),
                    Mode::WrongIdentity => proto::enc_ok_info(id, &Info { id: "someone.else".into(), version: l.version, entrypoints: l.entrypoints }).unwrap(),
                    _ => proto::enc_ok_info(id, &Info { id: l.plugin_id, version: l.version, entrypoints: l.entrypoints }).unwrap(),
                },
                Request::Shutdown => {
                    c.shutdowns.fetch_add(1, Ordering::SeqCst);
                    send(&mut output, proto::enc_ok_empty(id));
                    break;
                }
                other => {
                    c.requests.fetch_add(1, Ordering::SeqCst);
                    match mode {
                        Mode::Hang | Mode::Silent | Mode::Garbage | Mode::OldProtocol => continue,
                        Mode::Die => break,
                        Mode::Trap => proto::enc_err(id, ErrKind::Trap, "unreachable"),
                        Mode::PluginError => proto::enc_err(id, ErrKind::Plugin, "no such file"),
                        _ => {
                            if mode == Mode::Slow && !slow_done {
                                slow_done = true;
                                std::thread::sleep(Duration::from_millis(400));
                            }
                            let id = if mode == Mode::WrongId { id + 1000 } else { id };
                            match other {
                                Request::Column { paths, .. } => {
                                    c.largest_batch.fetch_max(paths.len() as u32, Ordering::SeqCst);
                                    let mut cells: Vec<Cell> = paths.into_iter().map(Cell::Text).collect();
                                    if mode == Mode::Short {
                                        cells.pop();
                                    }
                                    proto::enc_ok_cells(id, &cells).unwrap()
                                }
                                Request::Preview { path, limits, .. } => {
                                    proto::enc_ok_doc(id, &[Node::Heading(path), Node::Pair("max".into(), limits.max_px.to_string())]).unwrap()
                                }
                                Request::VfsList { cursor, .. } => {
                                    let page: u32 = cursor.map_or(0, |c| c.parse().unwrap());
                                    let e = [VfsEntry { name: format!("e{page}"), dir: false, size: u64::from(page), mtime: 0 }];
                                    proto::enc_ok_entries(id, &e, (page < 2).then(|| (page + 1).to_string()).as_deref()).unwrap()
                                }
                                Request::VfsRead { offset, max_len, .. } => {
                                    // a 2.5 MiB file of the byte 7
                                    let size = 5 * (1u64 << 19);
                                    let n = size.saturating_sub(offset).min(u64::from(max_len)) as usize;
                                    proto::enc_ok_bytes(id, &vec![7u8; n]).unwrap()
                                }
                                Request::Search { limit, cursor, .. } => {
                                    let page: u32 = cursor.map_or(0, |c| c.parse().unwrap());
                                    let hits: Vec<Hit> = (0..3).map(|i| Hit { path: format!("C:\\hit{}", page * 3 + i), dir: false, size: u64::from(limit), mtime: 0 }).collect();
                                    proto::enc_ok_hits(id, &hits, Some(&(page + 1).to_string())).unwrap()
                                }
                                Request::Command { selection, part, parts, .. } => {
                                    c.largest_batch.fetch_max(selection.len() as u32, Ordering::SeqCst);
                                    proto::enc_ok_message(id, Some(&format!("{} of {parts}: {}", part + 1, selection.len()))).unwrap()
                                }
                                Request::Load(_) | Request::Shutdown => unreachable!(),
                            }
                        }
                    }
                }
            };
            if !send(&mut output, frame) {
                break;
            }
        }
        dead.store(true, Ordering::SeqCst);
    }

    impl Launcher for Mock {
        fn launch(&self) -> io::Result<HostIo> {
            let n = self.c.launches.fetch_add(1, Ordering::SeqCst);
            let mode = if n == 0 { self.mode } else { self.later };
            let (req_r, req_w) = io::pipe()?;
            let (resp_r, resp_w) = io::pipe()?;
            let dead = Arc::new(AtomicBool::new(false));
            let (c, d) = (self.c.clone(), dead.clone());
            std::thread::spawn(move || serve(mode, c, req_r, resp_w, d));
            Ok(HostIo { input: Box::new(req_w), output: Box::new(resp_r), process: Box::new(MockProcess { c: self.c.clone(), dead, killed: false }) })
        }
    }

    fn load(timeout_ms: u32) -> Load {
        Load {
            plugin_id: "test.mock".into(),
            version: "1.0.0".into(),
            component: "C:\\none.cwasm".into(),
            sha256: [0; 32],
            caps: Capabilities { timeout_ms, ..Capabilities::default() },
            entrypoints: vec![(Kind::Column, "c".into())],
            protected: Vec::new(),
        }
    }

    fn plugin_with(mode: Mode, later: Mode, timeout_ms: u32) -> (Plugin, Arc<Counters>) {
        let c = Arc::new(Counters::default());
        // A silent host is waited for in full, so keep that wait short; every other
        // start gets plenty of time, so a busy machine does not fail the tests.
        let start_timeout = if mode == Mode::Silent { Duration::from_millis(300) } else { Duration::from_secs(10) };
        let opts = Options { grace: Duration::from_millis(50), start_timeout, ..Options::default() };
        (Plugin::new(load(timeout_ms), Mock { mode, later, c: c.clone() }, opts), c)
    }

    /// A short deadline only where the test waits for it to pass.
    fn plugin(mode: Mode) -> (Plugin, Arc<Counters>) {
        plugin_with(mode, mode, if mode == Mode::Hang { 100 } else { 10_000 })
    }

    fn n(a: &AtomicU32) -> u32 {
        a.load(Ordering::SeqCst)
    }

    #[test]
    fn nothing_starts_until_the_first_call() {
        let (p, c) = plugin(Mode::Echo);
        assert_eq!(p.status(), Status::Idle);
        assert_eq!(p.stats(), Stats::default());
        assert_eq!(n(&c.launches), 0);
        assert!(!p.stop_if_idle(Duration::ZERO));
        p.shutdown();
        drop(p);
        assert_eq!(n(&c.launches), 0);
    }

    #[test]
    fn round_trip_and_clean_shutdown() {
        let (p, c) = plugin(Mode::Echo);
        let mut out = vec![Cell::Empty];
        p.column_values("c", &["C:\\a", "C:\\b"], &mut out).unwrap();
        assert_eq!(out, [Cell::Empty, Cell::Text("C:\\a".into()), Cell::Text("C:\\b".into())]);
        assert_eq!(p.status(), Status::Running);
        // an empty batch makes no call
        p.column_values("c", &[], &mut out).unwrap();
        assert_eq!(p.stats(), Stats { spawns: 1, kills: 0, calls: 1, faults: 0 });

        p.shutdown();
        assert_eq!(p.status(), Status::Idle);
        assert_eq!((n(&c.shutdowns), n(&c.kills)), (1, 0), "a clean shutdown does not kill");
        // the next call starts a new host
        p.column_values("c", &["C:\\c"], &mut out).unwrap();
        assert_eq!(n(&c.launches), 2);
        drop(p);
        assert_eq!((n(&c.shutdowns), n(&c.kills)), (2, 0), "drop shuts down too");
    }

    #[test]
    fn batches_are_cut_at_2000_paths() {
        let (p, c) = plugin(Mode::Echo);
        let owned: Vec<String> = (0..4_500).map(|i| format!("C:\\dir\\f{i}")).collect();
        let paths: Vec<&str> = owned.iter().map(String::as_str).collect();
        let mut out = Vec::new();
        p.column_values("c", &paths, &mut out).unwrap();
        assert_eq!(out.len(), 4_500);
        assert!(out.iter().zip(&owned).all(|(c, p)| *c == Cell::Text(p.clone())), "order kept across chunks");
        assert_eq!(n(&c.requests), 3);
        assert_eq!(n(&c.largest_batch), 2_000);
        assert_eq!(p.stats().calls, 3);

        // a command over a large selection arrives in parts
        let msg = p.run_command("x", "C:\\dir", &paths).unwrap();
        assert_eq!(msg.as_deref(), Some("3 of 3: 500"));
        assert_eq!(n(&c.largest_batch), 2_000);
        // and with nothing selected it still runs once
        assert_eq!(p.run_command("x", "C:\\dir", &[]).unwrap().as_deref(), Some("1 of 1: 0"));
    }

    #[test]
    fn a_hung_host_is_killed_and_the_next_call_recovers() {
        let (p, c) = plugin_with(Mode::Hang, Mode::Echo, 100);
        let mut out = vec![Cell::Int(1)];
        let t = Instant::now();
        assert_eq!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Timeout));
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(150) && took < Duration::from_secs(2), "took {took:?}");
        assert_eq!(out, [Cell::Int(1)], "out is untouched");
        assert_eq!(n(&c.kills), 1, "the hung host was killed");
        assert_eq!(p.status(), Status::Idle);
        assert_eq!(p.stats(), Stats { spawns: 1, kills: 1, calls: 1, faults: 1 });
        // the caller carries on: the next call gets a fresh host
        p.column_values("c", &["C:\\a"], &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(n(&c.launches), 2);
        assert_eq!(p.status(), Status::Running);
    }

    #[test]
    fn a_late_reply_is_not_taken_for_the_next_call() {
        let (p, c) = plugin_with(Mode::Slow, Mode::Echo, 100);
        let mut out = Vec::new();
        assert_eq!(p.column_values("c", &["C:\\late"], &mut out), Err(CallError::Timeout));
        // the slow host was killed, so its late answer can not reach this call
        p.column_values("c", &["C:\\next"], &mut out).unwrap();
        assert_eq!(out, [Cell::Text("C:\\next".into())]);
        assert_eq!((n(&c.launches), n(&c.kills)), (2, 1));
    }

    #[test]
    fn a_crashed_host_is_replaced() {
        let (p, c) = plugin_with(Mode::Die, Mode::Echo, 10_000);
        let mut out = Vec::new();
        assert_eq!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Crashed));
        assert_eq!(p.status(), Status::Idle);
        p.column_values("c", &["C:\\a"], &mut out).unwrap();
        assert_eq!(n(&c.launches), 2);
    }

    #[test]
    fn repeated_failures_switch_the_plugin_off() {
        let (p, c) = plugin(Mode::Hang);
        let mut out = Vec::new();
        for _ in 0..3 {
            assert_eq!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Timeout));
        }
        assert_eq!(n(&c.launches), 3);
        assert!(matches!(p.status(), Status::Faulted(_)));
        // faulted: fails at once and starts nothing
        let t = Instant::now();
        assert!(matches!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Faulted(_))));
        assert!(t.elapsed() < Duration::from_secs(1));
        assert_eq!(n(&c.launches), 3);
        assert_eq!(p.stats().faults, 3);
        // the user switches it back on
        p.reset();
        assert_eq!(p.status(), Status::Idle);
        assert_eq!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Timeout));
        assert_eq!(n(&c.launches), 4);
    }

    #[test]
    fn a_success_clears_the_fault_count() {
        let (p, _c) = plugin_with(Mode::Die, Mode::Echo, 10_000);
        let mut out = Vec::new();
        assert!(p.column_values("c", &["C:\\a"], &mut out).is_err());
        for _ in 0..5 {
            p.column_values("c", &["C:\\a"], &mut out).unwrap();
        }
        assert_eq!(p.status(), Status::Running);
    }

    #[test]
    fn guest_errors_keep_the_host() {
        // a trap is a fault, but the host stays: it only drops the instance
        let (p, c) = plugin(Mode::Trap);
        let mut out = Vec::new();
        assert_eq!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Trap("unreachable".into())));
        assert_eq!(p.status(), Status::Running);
        assert_eq!((n(&c.launches), n(&c.kills)), (1, 0));
        assert!(p.column_values("c", &["C:\\a"], &mut out).is_err());
        assert!(p.column_values("c", &["C:\\a"], &mut out).is_err());
        assert!(matches!(p.status(), Status::Faulted(_)));
        assert_eq!(n(&c.kills), 1, "a faulted plugin's host is stopped");

        // a plugin's own error is not a fault at all
        let (p, c) = plugin(Mode::PluginError);
        let mut doc = Vec::new();
        for _ in 0..10 {
            assert_eq!(p.preview("p", "C:\\a", Limits { max_bytes: 1, max_px: 1 }, &mut doc), Err(CallError::Plugin("no such file".into())));
        }
        assert_eq!(p.status(), Status::Running);
        assert_eq!((p.stats().faults, n(&c.launches)), (0, 1));
        assert!(!CallError::Plugin(String::new()).is_fault());
        assert!(CallError::Timeout.is_fault());
    }

    #[test]
    fn a_host_that_does_not_start_properly_is_killed() {
        for (mode, want) in [
            (Mode::Silent, "did not answer"),
            (Mode::Garbage, "not a plugin host"),
            (Mode::OldProtocol, "speaks protocol"),
            (Mode::RefuseLoad, "hash mismatch"),
            (Mode::WrongIdentity, "someone.else"),
        ] {
            let (p, c) = plugin(mode);
            let mut out = Vec::new();
            let e = p.column_values("c", &["C:\\a"], &mut out).unwrap_err();
            assert!(matches!(e, CallError::Spawn(_) | CallError::Load(_)), "{e:?}");
            assert!(e.to_string().contains(want), "{e} / {want}");
            assert_eq!(p.status(), Status::Idle);
            assert_eq!((n(&c.launches), n(&c.requests)), (1, 0), "no call reaches a host that failed to load");
            assert_eq!(p.stats().kills, 1);
        }
    }

    #[test]
    fn broken_replies_restart_the_host() {
        let (p, c) = plugin_with(Mode::WrongId, Mode::Echo, 10_000);
        let mut out = Vec::new();
        assert!(matches!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Protocol(_))));
        assert_eq!(n(&c.kills), 1);
        p.column_values("c", &["C:\\a"], &mut out).unwrap();

        let (p, c) = plugin_with(Mode::Short, Mode::Echo, 10_000);
        let mut out = vec![Cell::Int(9)];
        assert!(matches!(p.column_values("c", &["C:\\a", "C:\\b"], &mut out), Err(CallError::Protocol(_))));
        assert_eq!(out, [Cell::Int(9)]);
        assert_eq!(n(&c.kills), 1);
    }

    #[test]
    fn idle_hosts_can_be_stopped() {
        let (p, c) = plugin(Mode::Echo);
        let mut out = Vec::new();
        p.column_values("c", &["C:\\a"], &mut out).unwrap();
        assert!(!p.stop_if_idle(Duration::from_secs(60)));
        assert_eq!(p.status(), Status::Running);
        assert!(p.stop_if_idle(Duration::ZERO));
        assert_eq!(p.status(), Status::Idle);
        assert_eq!(n(&c.shutdowns), 1);
    }

    #[test]
    fn paged_calls() {
        let (p, _c) = plugin(Mode::Echo);
        // three pages, then the end
        let mut names = Vec::new();
        p.vfs_list("z", "C:\\a.zip", &mut |e| {
            names.extend(e.iter().map(|e| e.name.clone()));
            true
        })
        .unwrap();
        assert_eq!(names, ["e0", "e1", "e2"]);
        // the sink can stop early
        let mut pages = 0;
        p.vfs_list("z", "C:\\a.zip", &mut |_| {
            pages += 1;
            false
        })
        .unwrap();
        assert_eq!(pages, 1);

        // the mock's search never ends by itself: the limit stops it
        let mut hits = Vec::new();
        p.search("s", "x", "", 7, &mut |h| {
            hits.extend(h.iter().map(|h| h.path.clone()));
            true
        })
        .unwrap();
        assert_eq!(hits.len(), 7);
        assert_eq!(hits[6], "C:\\hit6");

        // a 2.5 MiB file is read in 1 MiB steps
        let before = p.stats().calls;
        let mut data = Vec::new();
        p.vfs_read("z", "C:\\a.zip\\f", u64::MAX, &mut data).unwrap();
        assert_eq!(data.len(), 5 << 19);
        assert_eq!(p.stats().calls - before, 3);
        // and a cap stops it early
        let mut data = Vec::new();
        p.vfs_read("z", "C:\\a.zip\\f", 100, &mut data).unwrap();
        assert_eq!(data.len(), 100);

        let mut doc = Vec::new();
        p.preview("p", "C:\\x.png", Limits { max_bytes: 10, max_px: 256 }, &mut doc).unwrap();
        assert_eq!(doc, [Node::Heading("C:\\x.png".into()), Node::Pair("max".into(), "256".into())]);
    }

    #[test]
    fn calls_from_many_threads_are_serialized() {
        let (p, c) = plugin(Mode::Echo);
        let p = Arc::new(p);
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for i in 0..50 {
                        let path = format!("C:\\t{t}\\f{i}");
                        let mut out = Vec::new();
                        p.column_values("c", &[&path], &mut out).unwrap();
                        assert_eq!(out, [Cell::Text(path)]);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!((n(&c.launches), n(&c.requests)), (1, 400));
    }

    #[test]
    fn a_missing_host_exe_is_an_error_not_a_panic() {
        let p = Plugin::new(load(100), ProcessLauncher::new("C:\\no\\such\\tsubame-plughost.exe"), Options::default());
        let mut out = Vec::new();
        for _ in 0..3 {
            assert!(matches!(p.column_values("c", &["C:\\a"], &mut out), Err(CallError::Spawn(_))));
        }
        assert!(matches!(p.status(), Status::Faulted(_)));
        assert_eq!(p.stats().spawns, 0);
    }
}
