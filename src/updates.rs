#![allow(dead_code)] // wired step by step; removed once Settings > Updates and main use everything
//! Self-update wiring (U4): policy, shared state for Settings > Updates, and the
//! background check / download / apply-at-restart flow on top of `tsubame-update`
//! (design: `docs/updater.md`, `docs/u4-settings.md`).
//!
//! Rules:
//! - Nothing here ever runs on the UI thread except reading a snapshot: every call that
//!   can block (network, disk) runs on a worker which stores its result in [`SHARED`] and
//!   pokes the window through the registered wake function.
//! - No network at all in a dev run (`--bench`, `--leak-gate`, ...), a portable copy, or
//!   when `updates.auto_check = false` (automatic path only; "Check now" is the user's call
//!   but is still refused in a dev run / portable copy).
//! - Never downloads without a click, except `updates.auto_install = true`: then a found
//!   update is downloaded into staging in the background and applied at the *next start*
//!   (never while running).
//! - A dead network, an empty feed, rate limiting: a friendly [`Phase::Unreachable`],
//!   never an error dialog.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use crate::buildinfo::VERSION;
use tsubame_update::{Channel, Error, Progress, Release, Startup, Updater, Version};

/// Where this run stands, for the Settings > Updates page.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Updates are not available in this run (dev run, portable copy, no updater).
    Off(OffReason),
    /// Nothing asked yet this run.
    Idle,
    Checking,
    UpToDate,
    Available(Info),
    Downloading { done: u64, total: u64 },
    /// Downloaded and verified; applied at the next start (or "Restart now").
    Staged(String),
    /// Applied; the new version starts on restart.
    ReadyToRestart(String),
    /// Offline / no feed / rate limited / the release list is empty: friendly, retried later.
    Unreachable,
    /// A real failure with a message fit for the UI (signature, hash, install dir not writable).
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffReason {
    DevRun,
    Portable,
    NoUpdater,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub version: String,
    /// The channel the release came from (a nightly copy may be offered a stable release).
    pub channel: Channel,
    pub notes_url: String,
    /// false: this copy is too old / read-only and needs the installer.
    pub in_place_ok: bool,
    pub size: u64,
}

/// What the Settings page draws.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub running: String,
    pub previous: Option<String>,
    pub last_check_unix: u64,
    /// First start after an update: "Updated to X".
    pub updated_to: Option<String>,
    /// The previous start of this version crashed before its first paint.
    pub crashed_after_update: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Env {
    pub dev_run: bool,
    pub portable: bool,
    pub bench: bool,
}

// ---- policy (pure, unit-tested) -----------------------------------------------------

/// May this run touch the network for updates at all?
pub fn off_reason(env: Env) -> Option<OffReason> {
    if env.dev_run || env.bench {
        Some(OffReason::DevRun)
    } else if env.portable {
        Some(OffReason::Portable)
    } else {
        None
    }
}

/// The automatic check at start: setting on, environment allows it, and the 24 h /
/// back-off clock of the updater state says it is due.
pub fn should_auto_check(auto_check: bool, env: Env, st: &tsubame_update::state::State, now: u64) -> bool {
    auto_check && off_reason(env).is_none() && tsubame_update::state::check_due(st, now, false)
}

/// "Check now": allowed unless the run itself forbids the network.
pub fn manual_check_allowed(env: Env) -> bool {
    off_reason(env).is_none()
}

/// Network-ish errors are not the user's problem: show the friendly state.
pub fn phase_from_error(e: &Error) -> Phase {
    match e {
        Error::Network(_) | Error::Http(_) | Error::RateLimited | Error::Config(_) => Phase::Unreachable,
        // an empty / odd feed (no release yet, unparsable listing): same friendly state
        Error::Parse(_) | Error::Nothing(_) => Phase::Unreachable,
        Error::NotNewer => Phase::UpToDate,
        Error::Cancelled => Phase::Idle,
        other => Phase::Failed(other.to_string()),
    }
}

/// A staged update is applied at start only when the user opted into auto install and it
/// is really newer than the running version.
pub fn apply_staged_at_start(auto_install: bool, staged: Option<&str>, running: &str) -> bool {
    if !auto_install {
        return false;
    }
    match (staged.and_then(|s| Version::parse(s).ok()), Version::parse(running).ok()) {
        (Some(s), Some(r)) => s > r,
        _ => false,
    }
}

// ---- shared state ---------------------------------------------------------------------

struct Shared {
    env: Env,
    up: Option<Arc<Updater>>,
    phase: Phase,
    release: Option<Release>,
    updated_to: Option<String>,
    crashed_after_update: Option<String>,
    auto_install: bool,
    /// `[updates] channel`, as the updater follows it.
    channel: Channel,
    /// Bumped on every channel switch: a check that was started for the old channel drops
    /// its answer instead of showing an offer from a channel the user just left.
    generation: u64,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
static CANCEL: AtomicBool = AtomicBool::new(false);

fn shared() -> &'static Mutex<Shared> {
    SHARED.get_or_init(|| {
        Mutex::new(Shared {
            env: Env { dev_run: true, portable: false, bench: false },
            up: None,
            phase: Phase::Off(OffReason::NoUpdater),
            release: None,
            updated_to: None,
            crashed_after_update: None,
            auto_install: false,
            channel: Channel::Stable,
            generation: 0,
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    shared().lock().unwrap_or_else(|e| e.into_inner())
}

fn wake() {
    if let Some(w) = WAKE.get() {
        w();
    }
}

fn set_phase(p: Phase) {
    lock().phase = p;
    wake();
}

/// The "install updates automatically" switch changed in Settings.
pub fn set_auto_install(v: bool) {
    lock().auto_install = v;
}

/// The channel the updater follows now.
pub fn channel() -> Channel {
    lock().channel
}

/// The channel setting changed in Settings. Builds a new updater for it (same pinned key, same
/// checks), forgets an offer from the old channel and goes back to "not checked yet". Refused
/// (false) while a download runs. Switching nightly -> stable never downgrades: the updater only
/// offers a stable release newer than the running nightly, and a nightly that was downloaded but
/// not installed is discarded at the next start.
pub fn set_channel(c: Channel) -> bool {
    let mut sh = lock();
    if sh.channel == c {
        return true;
    }
    if matches!(sh.phase, Phase::Downloading { .. }) {
        return false;
    }
    sh.channel = c;
    sh.generation += 1;
    sh.release = None;
    if sh.up.is_some() {
        match Updater::production_channel(VERSION, c) {
            Ok(u) => sh.up = Some(Arc::new(u)),
            Err(_) => {
                sh.up = None;
                sh.phase = Phase::Off(OffReason::NoUpdater);
            }
        }
    }
    if !matches!(sh.phase, Phase::Off(_) | Phase::ReadyToRestart(_)) {
        sh.phase = Phase::Idle;
    }
    drop(sh);
    wake();
    true
}

/// The environment this run was started with (for the Settings page).
pub fn current_env() -> Env {
    lock().env
}

/// What `init` tells `main` to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartAction {
    Continue,
    /// An update was applied (or a crashed one rolled back) and the right version was
    /// started again: this process must exit now.
    Exit,
}

/// First thing in `main` (before the single-instance claim). Pure bookkeeping in a dev
/// run: no updater is even built, nothing is read or written.
pub fn init(env: Env, auto_install: bool, channel: Channel, wake_fn: impl Fn() + Send + Sync + 'static) -> StartAction {
    let _ = WAKE.set(Box::new(wake_fn));
    let mut sh = lock();
    sh.env = env;
    sh.auto_install = auto_install;
    sh.channel = channel;
    if let Some(r) = off_reason(env) {
        sh.phase = Phase::Off(r);
        return StartAction::Continue;
    }
    let up = match Updater::production_channel(VERSION, channel) {
        Ok(u) => Arc::new(u),
        Err(_) => {
            sh.phase = Phase::Off(OffReason::NoUpdater);
            return StartAction::Continue;
        }
    };
    sh.phase = Phase::Idle;
    sh.up = Some(up.clone());
    match up.on_start() {
        Startup::Normal => {}
        Startup::Trial { version, .. } => sh.updated_to = Some(version),
        Startup::CrashedAfterUpdate { version, boots, .. } => {
            if boots >= 3 {
                // third failed start: go back without asking
                drop(sh);
                if up.rollback().is_ok() && up.relaunch(&[]).is_ok() {
                    return StartAction::Exit;
                }
                sh = lock();
            }
            sh.crashed_after_update = Some(version);
        }
    }
    // apply a staged download at this restart (auto install only)
    let staged = up.staged();
    if apply_staged_at_start(auto_install, staged.as_ref().map(|s| s.version.as_str()), VERSION)
        && let Some(st) = staged
    {
        drop(sh);
        if up.apply(&st).is_ok()
            && let Ok(tsubame_update::WatchOutcome::Started { .. } | tsubame_update::WatchOutcome::RolledBack { .. }) = up.relaunch_watched(&[], Duration::from_secs(30))
        {
            return StartAction::Exit;
        }
        sh = lock();
    }
    drop(sh);
    StartAction::Continue
}

/// After the first paint of the main window: ends the update trial.
pub fn on_ready() {
    let up = lock().up.clone();
    if let Some(up) = up {
        std::thread::spawn(move || up.on_ready());
    }
}

/// The automatic check after start. Spawns nothing unless one is due.
pub fn auto_check(auto_check: bool) {
    let (up, env) = {
        let sh = lock();
        (sh.up.clone(), sh.env)
    };
    let Some(up) = up else { return };
    let st = tsubame_update::state::State::load(up.config().layout());
    if !should_auto_check(auto_check, env, &st, tsubame_update::state::now_unix()) {
        return;
    }
    spawn_check(up, false);
}

/// "Check now" button.
pub fn check_now() {
    let (up, env) = {
        let sh = lock();
        (sh.up.clone(), sh.env)
    };
    let Some(up) = up else { return };
    if !manual_check_allowed(env) {
        return;
    }
    if matches!(lock().phase, Phase::Checking | Phase::Downloading { .. }) {
        return;
    }
    spawn_check(up, true);
}

fn spawn_check(up: Arc<Updater>, force: bool) {
    let generation = lock().generation;
    set_phase(Phase::Checking);
    tsubame_update::spawn_check(up.clone(), force, move |r| {
        if lock().generation != generation {
            return; // the channel changed while this ran: its answer is for the old channel
        }
        match r {
            Ok(None) => set_phase(Phase::UpToDate),
            Ok(Some(rel)) => {
                let info = info_of(&rel);
                let auto = lock().auto_install && rel.in_place_ok;
                {
                    let mut sh = lock();
                    sh.release = Some(rel);
                    sh.phase = Phase::Available(info);
                }
                wake();
                if auto && !force {
                    download(up, false);
                }
            }
            Err(e) => set_phase(phase_from_error(&e)),
        }
    });
}

/// "Download and restart": download + verify + apply on a worker, then `ReadyToRestart`.
pub fn install() {
    let up = lock().up.clone();
    if let Some(up) = up {
        download(up, true);
    }
}

fn download(up: Arc<Updater>, apply: bool) {
    let Some(rel) = lock().release.clone() else { return };
    if !rel.in_place_ok {
        return;
    }
    CANCEL.store(false, Ordering::Relaxed);
    set_phase(Phase::Downloading { done: 0, total: rel.download_size });
    std::thread::spawn(move || {
        let r = up.download(&rel, &mut |p: Progress| {
            lock().phase = Phase::Downloading { done: p.done, total: p.total };
            wake();
            !CANCEL.load(Ordering::Relaxed)
        });
        match r {
            Err(e) => {
                let p = phase_from_error(&e);
                // a cancelled download goes back to "available"
                let back = if e_is_cancel(&e) { lock().release.as_ref().map(info_of).map(Phase::Available) } else { None };
                set_phase(back.unwrap_or(p));
            }
            Ok(staged) if !apply => set_phase(Phase::Staged(staged.version)),
            Ok(staged) => match up.apply(&staged) {
                Ok(a) => set_phase(Phase::ReadyToRestart(a.version)),
                Err(e) => set_phase(phase_from_error(&e)),
            },
        }
    });
}

fn e_is_cancel(e: &Error) -> bool {
    matches!(e, Error::Cancelled)
}

fn info_of(rel: &Release) -> Info {
    Info { version: rel.version.to_string(), channel: rel.channel, notes_url: rel.notes_url.clone(), in_place_ok: rel.in_place_ok, size: rel.download_size }
}

pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// True once an update is applied and waiting for the restart.
pub fn ready_to_restart() -> bool {
    matches!(lock().phase, Phase::ReadyToRestart(_))
}

/// Start the new version and watch it (call after the session is saved and the window is
/// gone). Blocks up to 30 s: run it on a worker or right before `exit`. Returns whether a
/// new process was started.
pub fn restart() -> bool {
    let up = lock().up.clone();
    match up {
        Some(up) => up.relaunch_watched(&[], Duration::from_secs(30)).is_ok(),
        None => false,
    }
}

/// "Roll back to <previous>": on a worker, then `restart_plain` and exit.
pub fn rollback_and_relaunch() -> bool {
    let up = lock().up.clone();
    match up {
        Some(up) => up.rollback().is_ok() && up.relaunch(&[]).is_ok(),
        None => false,
    }
}

pub fn snapshot() -> Snapshot {
    let sh = lock();
    let (running, previous, last) = match &sh.up {
        Some(up) => {
            let s = up.status();
            (s.running, s.previous, s.last_check_unix)
        }
        None => (VERSION.to_string(), None, 0),
    };
    Snapshot { phase: sh.phase.clone(), running, previous, last_check_unix: last, updated_to: sh.updated_to.clone(), crashed_after_update: sh.crashed_after_update.clone() }
}

/// "3 hours ago" style text for the last check; "never" when 0.
pub fn ago(last_unix: u64, now: u64) -> String {
    if last_unix == 0 {
        return "never".to_string();
    }
    let d = now.saturating_sub(last_unix);
    match d {
        0..=89 => "just now".to_string(),
        90..=5399 => format!("{} min ago", (d + 30) / 60),
        5400..=129_599 => format!("{} h ago", (d + 1800) / 3600),
        _ => format!("{} days ago", (d + 43_200) / 86_400),
    }
}

/// `--after-update <pid>`: wait (at most `max`) until the old process is gone, so the
/// single-instance lock and the files it held are free.
pub fn wait_for_exit(pid: u32, max: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(h, max.as_millis().min(u32::MAX as u128 - 1) as u32);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsubame_update::state::{State, CHECK_INTERVAL_SECS};

    fn env(dev: bool, portable: bool, bench: bool) -> Env {
        Env { dev_run: dev, portable, bench }
    }
    const NORMAL: Env = Env { dev_run: false, portable: false, bench: false };

    #[test]
    fn auto_check_follows_the_24_hour_rule() {
        let day = CHECK_INTERVAL_SECS;
        let st = State { last_attempt_unix: 1_000_000, ..Default::default() };
        assert!(!should_auto_check(true, NORMAL, &st, 1_000_000 + 60));
        assert!(!should_auto_check(true, NORMAL, &st, 1_000_000 + day - 1));
        assert!(should_auto_check(true, NORMAL, &st, 1_000_000 + day));
        // never checked: due now
        assert!(should_auto_check(true, NORMAL, &State::default(), 5_000_000));
    }

    #[test]
    fn rate_limit_back_off_blocks_the_automatic_check() {
        let st = State { last_attempt_unix: 1, next_attempt_unix: 10_000_000, ..Default::default() };
        assert!(!should_auto_check(true, NORMAL, &st, 9_000_000));
        assert!(should_auto_check(true, NORMAL, &st, 10_000_001));
    }

    #[test]
    fn setting_off_means_no_network() {
        assert!(!should_auto_check(false, NORMAL, &State::default(), 5_000_000));
    }

    #[test]
    fn dev_runs_portable_copies_and_bench_never_touch_the_network() {
        let st = State::default();
        for e in [env(true, false, false), env(false, true, false), env(false, false, true), env(true, true, true)] {
            assert!(off_reason(e).is_some(), "{e:?}");
            assert!(!should_auto_check(true, e, &st, 5_000_000), "{e:?}");
            assert!(!manual_check_allowed(e), "{e:?}");
        }
        assert_eq!(off_reason(env(true, false, false)), Some(OffReason::DevRun));
        assert_eq!(off_reason(env(false, true, false)), Some(OffReason::Portable));
        assert!(manual_check_allowed(NORMAL));
    }

    #[test]
    fn unreachable_feeds_are_friendly_not_errors() {
        for e in [Error::Network("dns".into()), Error::Http(404), Error::Http(503), Error::RateLimited, Error::Parse("empty".into()), Error::Nothing("no release")] {
            assert_eq!(phase_from_error(&e), Phase::Unreachable, "{e}");
        }
        assert_eq!(phase_from_error(&Error::NotNewer), Phase::UpToDate);
        assert_eq!(phase_from_error(&Error::Cancelled), Phase::Idle);
    }

    #[test]
    fn real_failures_keep_their_message() {
        match phase_from_error(&Error::BadSignature("no key".into())) {
            Phase::Failed(m) => assert!(!m.is_empty()),
            p => panic!("{p:?}"),
        }
        assert!(matches!(phase_from_error(&Error::HashMismatch { what: "x".into() }), Phase::Failed(_)));
        assert!(matches!(phase_from_error(&Error::Install("read-only".into())), Phase::Failed(_)));
    }

    #[test]
    fn staged_update_applies_at_restart_only_with_auto_install_and_when_newer() {
        assert!(apply_staged_at_start(true, Some("9.9.9"), "0.1.0"));
        assert!(!apply_staged_at_start(false, Some("9.9.9"), "0.1.0"), "off by default: only on a click");
        assert!(!apply_staged_at_start(true, Some("0.1.0"), "0.1.0"));
        assert!(!apply_staged_at_start(true, Some("0.0.9"), "0.1.0"));
        assert!(!apply_staged_at_start(true, None, "0.1.0"));
        assert!(!apply_staged_at_start(true, Some("garbage"), "0.1.0"));
    }

    #[test]
    fn dev_run_init_builds_no_updater_and_stays_off() {
        // the shared state is process-global: this test only asserts the dev-run path
        let a = init(env(true, false, false), false, Channel::Stable, || {});
        assert_eq!(a, StartAction::Continue);
        let s = snapshot();
        assert_eq!(s.phase, Phase::Off(OffReason::DevRun));
        assert_eq!(s.running, VERSION);
        // and the buttons do nothing (no worker, no state change)
        check_now();
        install();
        assert_eq!(snapshot().phase, Phase::Off(OffReason::DevRun));
        // switching the channel in a dev run builds no updater either and keeps it off
        assert!(set_channel(Channel::Nightly));
        assert_eq!(channel(), Channel::Nightly);
        assert_eq!(snapshot().phase, Phase::Off(OffReason::DevRun));
        assert!(set_channel(Channel::Stable));
    }

    #[test]
    fn config_channel_values_map_to_the_updater_channel() {
        let c = tsubame_config::Config::default();
        assert_eq!(Channel::from_config(&c.updates.channel), Channel::Stable, "stable is the default");
        assert_eq!(Channel::from_config("nightly"), Channel::Nightly);
        assert_eq!(Channel::from_config("beta"), Channel::Stable, "an unknown value never means test builds");
    }

    #[test]
    fn ago_text() {
        assert_eq!(ago(0, 100), "never");
        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(1000, 1000 + 600), "10 min ago");
        assert_eq!(ago(1000, 1000 + 3 * 3600), "3 h ago");
        assert_eq!(ago(1000, 1000 + 5 * 86_400), "5 days ago");
    }
}
