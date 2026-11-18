//! The shim's decisions, behind one [`Host`] trait so tests drive them with a fake: no process
//! is started, no registry is touched, no clock is read.
//!
//! ```text
//! request ──► Open(path) ──► app missing?  ──yes──► (retry briefly: an update may be swapping)
//!                │                                   still missing ─► remove our verb ─► Explorer
//!                ▼
//!          start tsubame.exe "<path>" ──► running / forwarded to the running window ─► done
//!                │
//!                └── start failed / exited non-zero / never became idle
//!                        ─► record the failure ─► 3 in 10 minutes: remove our verb
//!                        ─► Explorer
//! ```
//! Loop guard: everything the shim starts carries `TSUBAME_HANDLER_DEPTH=<n+1>`. A shim that
//! finds depth 1 was started by its own fallback (Explorer routed the folder back through the
//! verb): it removes the verb and opens Explorer once more. Depth 2 does nothing at all.

use crate::cmdline::{self, Request};
use crate::registry::{self, Registry, Removal};
use std::path::{Path, PathBuf};

/// Environment variable set on every process the shim starts.
pub const DEPTH_ENV: &str = "TSUBAME_HANDLER_DEPTH";
/// How many times the app file is looked for before it counts as missing.
pub const MISSING_TRIES: u32 = 4;
pub const MISSING_RETRY_MS: u32 = 150;
/// This many failures within [`FAIL_WINDOW_SECS`] switch the feature off.
pub const FAIL_LIMIT: usize = 3;
pub const FAIL_WINDOW_SECS: u64 = 600;
/// Most failure timestamps kept.
pub const FAIL_KEEP: usize = 8;

/// What happened when the app was started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Started {
    /// Still running and idle (a first instance) or running after the grace window.
    Running,
    /// Exited with code 0 inside the grace window: the running instance took the folder.
    ForwardedAndExited,
    /// Exited with a non-zero code inside the grace window.
    Crashed(u32),
    /// Running but never reached its message loop inside the grace window.
    Hung,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnError {
    /// The file (or its folder) is not there: the app was deleted, moved or quarantined.
    pub not_found: bool,
    pub message: String,
}

/// Everything the shim touches. The real one is `sys::SysHost`.
pub trait Host {
    /// How deep in shim fallbacks this process is (0 = started by the shell).
    fn depth(&self) -> u32;
    /// This shim's own path (what the registry names).
    fn shim_path(&self) -> PathBuf;
    /// `tsubame.exe` next to the shim.
    fn app_path(&self) -> PathBuf;
    fn exists(&self, p: &Path) -> bool;
    fn sleep_ms(&mut self, ms: u32);
    fn now_secs(&self) -> u64;
    /// Start the app with `args` (already quoted) and wait out the grace window.
    fn start_app(&mut self, app: &Path, command_line: &str) -> Result<Started, SpawnError>;
    /// Launch File Explorer with this full command line.
    fn start_explorer(&mut self, command_line: &str) -> Result<(), String>;
    /// `%WINDIR%\explorer.exe`, absolute (never found through PATH).
    fn explorer_path(&self) -> String;
    fn registry(&mut self) -> &mut dyn Registry;
    fn load_failures(&self) -> Vec<u64>;
    fn save_failures(&mut self, times: &[u64]);
    fn log(&mut self, line: &str);
}

/// Why the shim did not hand the folder to Tsubame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    NotAnAppFolder,
    AppMissing,
    SpawnFailed(String),
    Crashed(u32),
    Hung,
    LoopGuard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The folder is in Tsubame.
    HandedToTsubame,
    /// File Explorer got the folder.
    Fallback { reason: Reason, explorer_started: bool, registration_removed: bool },
    /// `--remove` finished.
    Removed(Removal),
    /// `--remove` could not finish.
    RemoveFailed(String),
    /// Started with nothing to do.
    NothingToDo,
    /// A loop was detected and stopped without starting anything.
    Stopped,
}

impl Outcome {
    /// Process exit code: 0 unless the user ended up with nothing at all.
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Fallback { explorer_started: false, .. } | Outcome::RemoveFailed(_) => 1,
            _ => 0,
        }
    }
}

pub fn handle(host: &mut dyn Host, req: Request) -> Outcome {
    match req {
        Request::Empty => Outcome::NothingToDo,
        Request::Remove => match registry::disable(host.registry()) {
            Ok(()) => {
                host.log("--remove: registration removed");
                Outcome::Removed(Removal::Removed)
            }
            Err(e) => {
                host.log(&format!("--remove failed: {e}"));
                Outcome::RemoveFailed(e)
            }
        },
        Request::Explorer(text) => {
            if host.depth() >= 2 {
                host.log("loop guard: depth 2, doing nothing");
                return Outcome::Stopped;
            }
            let cl = cmdline::explorer_command_line(&host.explorer_path(), &text);
            let explorer_started = start_explorer(host, &cl);
            Outcome::Fallback { reason: Reason::NotAnAppFolder, explorer_started, registration_removed: false }
        }
        Request::Open(path) => open_folder(host, &path),
    }
}

fn open_folder(host: &mut dyn Host, path: &str) -> Outcome {
    let depth = host.depth();
    if depth >= 2 {
        host.log("loop guard: depth 2, doing nothing");
        return Outcome::Stopped;
    }
    if depth == 1 {
        // our own fallback came back through the verb: the verb must go
        host.log(&format!("loop guard: re-entered from the fallback for {path}; removing the registration"));
        let removed = deregister(host);
        let cl = cmdline::explorer_command_line(&host.explorer_path(), path);
        let explorer_started = start_explorer(host, &cl);
        return Outcome::Fallback { reason: Reason::LoopGuard, explorer_started, registration_removed: removed };
    }

    let app = host.app_path();
    if !wait_for_app(host, &app) {
        // an update swaps tsubame.exe through `tsubame.exe.new`: not a deleted app
        let updating = host.exists(&sibling(&app, ".new"));
        host.log(&format!("tsubame.exe not found at {}{}", app.display(), if updating { " (update in progress, keeping the registration)" } else { "" }));
        let removed = if updating { false } else { deregister(host) };
        return fallback(host, path, Reason::AppMissing, removed);
    }

    match host.start_app(&app, &cmdline::app_command_line(&app.display().to_string(), path)) {
        Ok(Started::Running | Started::ForwardedAndExited) => Outcome::HandedToTsubame,
        Ok(Started::Crashed(code)) => failed(host, path, Reason::Crashed(code)),
        Ok(Started::Hung) => failed(host, path, Reason::Hung),
        Err(e) if e.not_found => {
            host.log(&format!("start failed, file gone: {}", e.message));
            let removed = deregister(host);
            fallback(host, path, Reason::AppMissing, removed)
        }
        Err(e) => {
            host.log(&format!("start failed: {}", e.message));
            failed(host, path, Reason::SpawnFailed(e.message))
        }
    }
}

/// The app file, with a short wait so an update swap in flight is not mistaken for a deletion.
fn wait_for_app(host: &mut dyn Host, app: &Path) -> bool {
    for i in 0..MISSING_TRIES {
        if host.exists(app) {
            return true;
        }
        if i + 1 < MISSING_TRIES {
            host.sleep_ms(MISSING_RETRY_MS);
        }
    }
    false
}

fn sibling(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// A start that did not work out: count it, open Explorer, and switch the feature off on the
/// third failure inside ten minutes.
fn failed(host: &mut dyn Host, path: &str, reason: Reason) -> Outcome {
    let now = host.now_secs();
    let mut times: Vec<u64> = host.load_failures().into_iter().filter(|t| now.saturating_sub(*t) <= FAIL_WINDOW_SECS).collect();
    times.push(now);
    if times.len() > FAIL_KEEP {
        times.drain(..times.len() - FAIL_KEEP);
    }
    host.save_failures(&times);
    host.log(&format!("failure {} of {FAIL_LIMIT}: {reason:?}", times.len()));
    let removed = if times.len() >= FAIL_LIMIT {
        let r = deregister(host);
        if r {
            host.save_failures(&[]);
        }
        r
    } else {
        false
    };
    fallback(host, path, reason, removed)
}

fn fallback(host: &mut dyn Host, path: &str, reason: Reason, registration_removed: bool) -> Outcome {
    let cl = cmdline::explorer_command_line(&host.explorer_path(), path);
    let explorer_started = start_explorer(host, &cl);
    Outcome::Fallback { reason, explorer_started, registration_removed }
}

fn start_explorer(host: &mut dyn Host, command_line: &str) -> bool {
    match host.start_explorer(command_line) {
        Ok(()) => true,
        Err(e) => {
            host.log(&format!("could not start explorer.exe: {e}"));
            false
        }
    }
}

/// Remove our registration if it is this shim's. True when it is gone afterwards.
fn deregister(host: &mut dyn Host) -> bool {
    let shim = host.shim_path();
    let res = registry::remove_if_ours(host.registry(), &shim);
    match res {
        Ok(Removal::Removed) => {
            host.log("registration removed");
            true
        }
        Ok(Removal::NothingRegistered) => {
            host.log("no registration to remove");
            true
        }
        Ok(Removal::NotOurs { command }) => {
            host.log(&format!("registration belongs to another install ({command}), left alone"));
            false
        }
        Err(e) => {
            host.log(&format!("could not remove the registration: {e}"));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CMD_KEY, MemRegistry, SHELL, VERB_KEY};
    use std::collections::HashSet;

    /// A host that records everything and does nothing for real.
    struct Fake {
        depth: u32,
        shim: PathBuf,
        files: HashSet<PathBuf>,
        /// files that appear after this many `exists` misses (an update finishing)
        appear_after: Option<(u32, PathBuf)>,
        misses: u32,
        reg: MemRegistry,
        start: Result<Started, SpawnError>,
        explorer_ok: bool,
        fails: Vec<u64>,
        now: u64,
        logs: Vec<String>,
        started_apps: Vec<(PathBuf, String)>,
        explorers: Vec<String>,
        slept: u32,
    }

    fn shim() -> PathBuf {
        PathBuf::from(r"C:\inst\tsubame-handler.exe")
    }
    fn app() -> PathBuf {
        PathBuf::from(r"C:\inst\tsubame.exe")
    }

    impl Fake {
        fn new() -> Fake {
            let mut files = HashSet::new();
            files.insert(shim());
            files.insert(app());
            let mut reg = MemRegistry::new();
            // registered the way enable() does it, without needing the file on disk
            let _ = reg.set(VERB_KEY, "", "Open in Tsubame");
            let _ = reg.set(CMD_KEY, "", &registry::command_line(&shim()));
            let _ = reg.set(SHELL, "", "Tsubame");
            Fake {
                depth: 0,
                shim: shim(),
                files,
                appear_after: None,
                misses: 0,
                reg,
                start: Ok(Started::Running),
                explorer_ok: true,
                fails: vec![],
                now: 1_000_000,
                logs: vec![],
                started_apps: vec![],
                explorers: vec![],
                slept: 0,
            }
        }
        fn registered(&self) -> bool {
            self.reg.key_exists(VERB_KEY) || self.reg.get(SHELL, "").is_some()
        }
    }

    impl Host for Fake {
        fn depth(&self) -> u32 {
            self.depth
        }
        fn shim_path(&self) -> PathBuf {
            self.shim.clone()
        }
        fn app_path(&self) -> PathBuf {
            app()
        }
        fn exists(&self, p: &Path) -> bool {
            self.files.contains(p)
        }
        fn sleep_ms(&mut self, _ms: u32) {
            self.slept += 1;
            self.misses += 1;
            if let Some((n, f)) = &self.appear_after {
                if self.misses >= *n {
                    self.files.insert(f.clone());
                }
            }
        }
        fn now_secs(&self) -> u64 {
            self.now
        }
        fn start_app(&mut self, app: &Path, cl: &str) -> Result<Started, SpawnError> {
            self.started_apps.push((app.to_path_buf(), cl.to_string()));
            self.start.clone()
        }
        fn start_explorer(&mut self, cl: &str) -> Result<(), String> {
            self.explorers.push(cl.to_string());
            if self.explorer_ok { Ok(()) } else { Err("denied".into()) }
        }
        fn explorer_path(&self) -> String {
            r"C:\Windows\explorer.exe".into()
        }
        fn registry(&mut self) -> &mut dyn Registry {
            &mut self.reg
        }
        fn load_failures(&self) -> Vec<u64> {
            self.fails.clone()
        }
        fn save_failures(&mut self, t: &[u64]) {
            self.fails = t.to_vec();
        }
        fn log(&mut self, line: &str) {
            self.logs.push(line.to_string());
        }
    }

    fn open(p: &str) -> Request {
        Request::Open(p.into())
    }

    #[test]
    fn happy_path_starts_the_app_with_the_quoted_path_and_touches_nothing_else() {
        let mut h = Fake::new();
        assert_eq!(handle(&mut h, open(r"D:\My Files")), Outcome::HandedToTsubame);
        assert_eq!(h.started_apps, vec![(app(), r#""C:\inst\tsubame.exe" "D:\My Files""#.to_string())]);
        assert!(h.explorers.is_empty());
        assert!(h.registered());
        assert!(h.logs.is_empty(), "the success path writes nothing");
        assert!(h.fails.is_empty());
    }

    #[test]
    fn forwarded_launch_that_exits_zero_is_success() {
        let mut h = Fake::new();
        h.start = Ok(Started::ForwardedAndExited);
        assert_eq!(handle(&mut h, open(r"C:\")), Outcome::HandedToTsubame);
        assert_eq!(h.started_apps[0].1, r#""C:\inst\tsubame.exe" "C:\\""#, "root keeps its backslash");
        assert!(h.explorers.is_empty());
    }

    #[test]
    fn missing_app_removes_the_registration_and_opens_explorer() {
        let mut h = Fake::new();
        h.files.remove(&app());
        let o = handle(&mut h, open(r"D:\x y"));
        assert_eq!(o, Outcome::Fallback { reason: Reason::AppMissing, explorer_started: true, registration_removed: true });
        assert_eq!(h.explorers, vec![r#""C:\Windows\explorer.exe" "D:\x y""#.to_string()]);
        assert!(!h.registered(), "verb and default are gone");
        assert!(h.started_apps.is_empty());
        assert_eq!(h.slept, MISSING_TRIES - 1, "waited for a swap before giving up");
        assert_eq!(o.exit_code(), 0);
    }

    #[test]
    fn app_that_reappears_during_the_retry_is_started() {
        let mut h = Fake::new();
        h.files.remove(&app());
        h.appear_after = Some((2, app()));
        assert_eq!(handle(&mut h, open(r"D:\x")), Outcome::HandedToTsubame);
        assert!(h.registered());
        assert!(h.explorers.is_empty());
    }

    #[test]
    fn missing_app_during_an_update_keeps_the_registration() {
        let mut h = Fake::new();
        h.files.remove(&app());
        h.files.insert(PathBuf::from(r"C:\inst\tsubame.exe.new"));
        let o = handle(&mut h, open(r"D:\x"));
        assert_eq!(o, Outcome::Fallback { reason: Reason::AppMissing, explorer_started: true, registration_removed: false });
        assert!(h.registered());
        assert_eq!(h.explorers.len(), 1, "the user still gets a folder");
    }

    #[test]
    fn spawn_error_not_found_is_a_deleted_app() {
        let mut h = Fake::new();
        h.start = Err(SpawnError { not_found: true, message: "os error 2".into() });
        let o = handle(&mut h, open(r"D:\x"));
        assert_eq!(o, Outcome::Fallback { reason: Reason::AppMissing, explorer_started: true, registration_removed: true });
        assert!(!h.registered());
    }

    #[test]
    fn other_spawn_errors_fall_back_but_keep_the_registration_for_now() {
        let mut h = Fake::new();
        h.start = Err(SpawnError { not_found: false, message: "access denied (blocked by policy)".into() });
        let o = handle(&mut h, open(r"D:\x"));
        assert_eq!(o, Outcome::Fallback { reason: Reason::SpawnFailed("access denied (blocked by policy)".into()), explorer_started: true, registration_removed: false });
        assert!(h.registered());
        assert_eq!(h.fails, vec![h.now]);
    }

    #[test]
    fn crash_and_hang_open_explorer_and_count_failures() {
        for (start, reason) in [(Started::Crashed(0xC000_0005), Reason::Crashed(0xC000_0005)), (Started::Hung, Reason::Hung)] {
            let mut h = Fake::new();
            h.start = Ok(start);
            let o = handle(&mut h, open(r"D:\x"));
            assert_eq!(o, Outcome::Fallback { reason, explorer_started: true, registration_removed: false });
            assert_eq!(h.explorers.len(), 1);
            assert!(h.registered());
            assert_eq!(h.fails.len(), 1);
        }
    }

    #[test]
    fn third_failure_inside_ten_minutes_removes_the_registration() {
        let mut h = Fake::new();
        h.start = Ok(Started::Crashed(1));
        for i in 0..2 {
            h.now = 5_000 + i * 60;
            assert!(matches!(handle(&mut h, open(r"D:\x")), Outcome::Fallback { registration_removed: false, .. }));
        }
        h.now = 5_000 + 120;
        assert!(matches!(handle(&mut h, open(r"D:\x")), Outcome::Fallback { registration_removed: true, .. }));
        assert!(!h.registered());
        assert!(h.fails.is_empty(), "counter reset once the feature is off");
        assert_eq!(h.explorers.len(), 3, "every attempt still ended in a folder");
    }

    #[test]
    fn old_failures_do_not_count() {
        let mut h = Fake::new();
        h.start = Ok(Started::Crashed(1));
        h.fails = vec![10, 20];
        h.now = 10_000;
        assert!(matches!(handle(&mut h, open(r"D:\x")), Outcome::Fallback { registration_removed: false, .. }));
        assert_eq!(h.fails, vec![10_000]);
    }

    #[test]
    fn failure_list_is_capped() {
        let mut h = Fake::new();
        h.start = Ok(Started::Hung);
        h.fails = (0..20).map(|i| 1_000_000 - i).collect();
        h.now = 1_000_000;
        let _ = handle(&mut h, open(r"D:\x"));
        assert!(!h.registered(), "a long failure streak switches the feature off");
        assert!(h.fails.len() <= FAIL_KEEP);
    }

    #[test]
    fn registration_of_another_install_survives_our_missing_app() {
        let mut h = Fake::new();
        h.files.remove(&app());
        // this shim is a stray copy; the registry points at a different install
        h.shim = PathBuf::from(r"D:\stray\tsubame-handler.exe");
        let o = handle(&mut h, open(r"D:\x"));
        assert_eq!(o, Outcome::Fallback { reason: Reason::AppMissing, explorer_started: true, registration_removed: false });
        assert!(h.registered());
    }

    #[test]
    fn depth_one_means_the_fallback_looped_back_so_remove_and_open_explorer_once() {
        let mut h = Fake::new();
        h.depth = 1;
        let o = handle(&mut h, open(r"D:\x"));
        assert_eq!(o, Outcome::Fallback { reason: Reason::LoopGuard, explorer_started: true, registration_removed: true });
        assert!(!h.registered());
        assert!(h.started_apps.is_empty(), "never starts the app from a loop");
        assert_eq!(h.explorers.len(), 1);
    }

    #[test]
    fn depth_two_does_nothing_at_all() {
        let mut h = Fake::new();
        h.depth = 2;
        assert_eq!(handle(&mut h, open(r"D:\x")), Outcome::Stopped);
        assert_eq!(handle(&mut h, Request::Explorer("::{X}".into())), Outcome::Stopped);
        assert!(h.started_apps.is_empty() && h.explorers.is_empty());
        assert!(h.registered());
    }

    #[test]
    fn non_folder_text_goes_straight_to_explorer() {
        let mut h = Fake::new();
        let o = handle(&mut h, Request::Explorer("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".into()));
        assert_eq!(o, Outcome::Fallback { reason: Reason::NotAnAppFolder, explorer_started: true, registration_removed: false });
        assert!(h.started_apps.is_empty());
        assert!(h.registered());
    }

    #[test]
    fn explorer_that_cannot_start_is_reported_in_the_exit_code() {
        let mut h = Fake::new();
        h.files.remove(&app());
        h.explorer_ok = false;
        let o = handle(&mut h, open(r"D:\x"));
        assert!(matches!(o, Outcome::Fallback { explorer_started: false, registration_removed: true, .. }));
        assert_eq!(o.exit_code(), 1);
        assert!(h.logs.iter().any(|l| l.contains("could not start explorer.exe")));
    }

    #[test]
    fn remove_flag_removes_regardless_of_the_command_and_restores_the_previous_default() {
        let mut h = Fake::new();
        let _ = h.reg.set(VERB_KEY, "PreviousDefault", "OpenWithOther");
        let _ = h.reg.set(CMD_KEY, "", r#""Z:\gone\tsubame-handler.exe" "%1""#);
        assert_eq!(handle(&mut h, Request::Remove), Outcome::Removed(Removal::Removed));
        assert_eq!(h.reg.get(SHELL, ""), Some("OpenWithOther".into()));
        assert!(!h.reg.key_exists(VERB_KEY));
        assert!(h.started_apps.is_empty() && h.explorers.is_empty());
    }

    #[test]
    fn remove_with_nothing_registered_is_fine_and_a_failing_registry_is_an_error() {
        let mut h = Fake::new();
        h.reg = MemRegistry::new();
        assert!(matches!(handle(&mut h, Request::Remove), Outcome::Removed(_)));
        let mut h2 = Fake::new();
        h2.reg.fail_writes = true;
        let o = handle(&mut h2, Request::Remove);
        assert!(matches!(o, Outcome::RemoveFailed(_)));
        assert_eq!(o.exit_code(), 1);
    }

    #[test]
    fn empty_request_does_nothing() {
        let mut h = Fake::new();
        assert_eq!(handle(&mut h, Request::Empty), Outcome::NothingToDo);
        assert!(h.started_apps.is_empty() && h.explorers.is_empty() && h.registered());
    }
}
