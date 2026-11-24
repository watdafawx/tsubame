//! [`JobLauncher`]: the launcher the app uses (PH-07, tier 1). Windows only.
//!
//! What it does that [`crate::client::ProcessLauncher`] (test-only) does not:
//!
//! * Creates a **job object** per host with `KILL_ON_JOB_CLOSE` (the host dies
//!   with the app, even when the app is killed), `ActiveProcessLimit = 1` (the
//!   host can not start a child process), a per-process memory limit, and UI
//!   restrictions (no desktop switching, display settings, clipboard, global
//!   atoms, USER handles of other processes, `ExitWindows`, system parameters).
//! * Assigns the process to the job **before** `launch` returns, so before the
//!   client sends `Load`: no plugin code has run yet.
//! * Starts the host with a **cleared environment** (only `SystemRoot`), a
//!   **fixed working folder** and an **absolute exe path**.
//! * **Refuses to start** when the app is elevated: a plugin host must never
//!   run with an administrator token. The check is injectable for tests.
//!
//! Not done here (tier 2/3, see `docs/security/plughost-review.md`, card L):
//! restricted token or low integrity, AppContainer, an explicit inheritable
//! handle list, process mitigation policies.

use crate::client::{HostIo, HostProcess, Launcher};
use std::ffi::{c_void, OsString};
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
    JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Memory the host process needs on top of the plugin's `max_memory_mb`
/// (runtime, the loaded `.cwasm`, the protocol buffers), in MiB. The job limit
/// is `max_memory_mb + HOST_OVERHEAD_MB`, so a plugin's own cap trips first.
pub const HOST_OVERHEAD_MB: u64 = 192;

/// `CREATE_NO_WINDOW`: the host is a console program; never show a console for it.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// True when the current process runs with an elevated (administrator) token.
/// An unreadable token counts as elevated: fail closed.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return true;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let r = GetTokenInformation(token, TokenElevation, Some(&mut elevation as *mut _ as *mut c_void), size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        let _ = CloseHandle(token);
        r.is_err() || elevation.TokenIsElevated != 0
    }
}

/// An owned job handle. Closing it kills every process in the job.
pub(crate) struct Job(HANDLE);

// SAFETY: a job handle is a kernel handle, usable from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// A job with the limits of tier 1. `memory_bytes` is the per-process
    /// commit limit.
    pub(crate) fn new(memory_bytes: usize) -> io::Result<Job> {
        unsafe {
            let handle = CreateJobObjectW(None, windows::core::PCWSTR::null()).map_err(io::Error::other)?;
            let job = Job(handle); // closed (and so cleaned up) on any error below
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            limits.ProcessMemoryLimit = memory_bytes;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(io::Error::other)?;
            let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                    | JOB_OBJECT_UILIMIT_EXITWINDOWS
                    | JOB_OBJECT_UILIMIT_GLOBALATOMS
                    | JOB_OBJECT_UILIMIT_HANDLES
                    | JOB_OBJECT_UILIMIT_READCLIPBOARD
                    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
            };
            SetInformationJobObject(
                job.0,
                JobObjectBasicUIRestrictions,
                &ui as *const _ as *const c_void,
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            )
            .map_err(io::Error::other)?;
            Ok(job)
        }
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }

    fn assign(&self, child: &Child) -> io::Result<()> {
        unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }.map_err(io::Error::other)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE ends the host if it is still running.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A host in its job. Killing ends the whole job; dropping closes the job,
/// which kills the host too.
struct JobProcess {
    child: Child,
    job: Job,
}

impl HostProcess for JobProcess {
    fn kill(&mut self) {
        unsafe {
            let _ = TerminateJobObject(self.job.raw(), 1);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn wait(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(2)),
                _ => return false,
            }
        }
    }
}

/// Starts `tsubame-plughost.exe` inside a job object. See the module docs.
pub struct JobLauncher {
    exe: PathBuf,
    work_dir: PathBuf,
    memory_bytes: usize,
    args: Vec<OsString>,
    elevated: Box<dyn Fn() -> bool + Send + Sync>,
}

impl JobLauncher {
    /// `exe` and `work_dir` must be absolute. `max_memory_mb` is the plugin's
    /// `Capabilities::max_memory_mb`; the job allows [`HOST_OVERHEAD_MB`] more.
    pub fn new(exe: impl Into<PathBuf>, work_dir: impl Into<PathBuf>, max_memory_mb: u32) -> io::Result<JobLauncher> {
        let (exe, work_dir) = (exe.into(), work_dir.into());
        if !exe.is_absolute() || !work_dir.is_absolute() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "host exe and working folder must be absolute paths"));
        }
        let memory_bytes = (u64::from(max_memory_mb) + HOST_OVERHEAD_MB).saturating_mul(1 << 20);
        Ok(JobLauncher { exe, work_dir, memory_bytes: usize::try_from(memory_bytes).unwrap_or(usize::MAX), args: vec!["serve".into()], elevated: Box::new(is_elevated) })
    }

    /// Replaces the elevation check (tests only; the app keeps [`is_elevated`]).
    pub fn with_elevation_check(mut self, check: impl Fn() -> bool + Send + Sync + 'static) -> JobLauncher {
        self.elevated = Box::new(check);
        self
    }

    /// Replaces the host's arguments (default: `serve`). For the mock hosts of the tests.
    pub fn with_args(mut self, args: Vec<OsString>) -> JobLauncher {
        self.args = args;
        self
    }

    /// The per-process memory limit the job gets, in bytes.
    pub fn memory_limit(&self) -> usize {
        self.memory_bytes
    }

    /// Starts the host and returns it with its process id and job handle.
    pub(crate) fn launch_in_job(&self) -> io::Result<(HostIo, u32, HANDLE)> {
        if (self.elevated)() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "refusing to start a plugin host from an elevated app"));
        }
        let job = Job::new(self.memory_bytes)?;
        let mut cmd = Command::new(&self.exe);
        cmd.args(&self.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        cmd.env_clear();
        if let Some(root) = std::env::var_os("SystemRoot") {
            cmd.env("SystemRoot", root);
        }
        cmd.current_dir(&self.work_dir);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd.spawn()?;
        // In the job before the client sends `Load`. A host that is not contained is never used.
        if let Err(e) = job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        let (pid, raw) = (child.id(), job.raw());
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("no pipes"));
        };
        let io = HostIo { input: Box::new(input), output: Box::new(output), process: Box::new(JobProcess { child, job }) };
        Ok((io, pid, raw))
    }

    /// Like [`Launcher::launch`], plus the host's process id (for the owner-death test).
    #[doc(hidden)]
    pub fn launch_with_pid(&self) -> io::Result<(HostIo, u32)> {
        self.launch_in_job().map(|(io, pid, _)| (io, pid))
    }
}

impl Launcher for JobLauncher {
    fn launch(&self) -> io::Result<HostIo> {
        self.launch_in_job().map(|(io, _, _)| io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use windows::Win32::System::JobObjects::{
        IsProcessInJob, QueryInformationJobObject, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE as KILL,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    fn here() -> PathBuf {
        std::env::current_exe().unwrap()
    }

    #[test]
    fn elevated_app_is_refused_and_nothing_starts() {
        let called = Arc::new(AtomicBool::new(false));
        let c = called.clone();
        // An exe that does not exist: if the check did not stop `launch`, the error would be NotFound.
        let l = JobLauncher::new("C:\\no\\such\\tsubame-plughost.exe", std::env::temp_dir(), 64).unwrap().with_elevation_check(move || {
            c.store(true, Ordering::SeqCst);
            true
        });
        let e = l.launch().err().expect("must refuse");
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn unelevated_app_gets_past_the_check() {
        let l = JobLauncher::new("C:\\no\\such\\tsubame-plughost.exe", std::env::temp_dir(), 64).unwrap().with_elevation_check(|| false);
        assert_eq!(l.launch().err().expect("no such exe").kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn relative_paths_are_refused() {
        assert!(JobLauncher::new("tsubame-plughost.exe", std::env::temp_dir(), 64).is_err());
        assert!(JobLauncher::new(here(), "work", 64).is_err());
    }

    #[test]
    fn memory_limit_adds_the_overhead() {
        let l = JobLauncher::new(here(), std::env::temp_dir(), 64).unwrap();
        assert_eq!(l.memory_limit(), ((64 + HOST_OVERHEAD_MB) as usize) << 20);
    }

    #[test]
    fn job_limits_are_applied() {
        let job = Job::new(100 << 20).unwrap();
        unsafe {
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            QueryInformationJobObject(
                Some(job.raw()),
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                None,
            )
            .unwrap();
            let flags = info.BasicLimitInformation.LimitFlags;
            assert!(flags.contains(KILL) && flags.contains(JOB_OBJECT_LIMIT_ACTIVE_PROCESS) && flags.contains(JOB_OBJECT_LIMIT_PROCESS_MEMORY));
            assert_eq!(info.BasicLimitInformation.ActiveProcessLimit, 1);
            assert_eq!(info.ProcessMemoryLimit, 100 << 20);

            let mut ui = JOBOBJECT_BASIC_UI_RESTRICTIONS::default();
            QueryInformationJobObject(
                Some(job.raw()),
                JobObjectBasicUIRestrictions,
                &mut ui as *mut _ as *mut c_void,
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
                None,
            )
            .unwrap();
            assert!(ui.UIRestrictionsClass.contains(JOB_OBJECT_UILIMIT_READCLIPBOARD | JOB_OBJECT_UILIMIT_WRITECLIPBOARD));
            assert!(ui.UIRestrictionsClass.contains(JOB_OBJECT_UILIMIT_DESKTOP | JOB_OBJECT_UILIMIT_EXITWINDOWS | JOB_OBJECT_UILIMIT_HANDLES));
        }
    }

    /// The process `launch` returns is already in the job. Uses this test exe as
    /// the "host": it ignores `serve` and just needs to be alive for a moment.
    #[test]
    fn host_is_in_the_job_when_launch_returns() {
        let l = JobLauncher::new(here(), std::env::temp_dir(), 256).unwrap().with_elevation_check(|| false).with_args(vec!["--list".into()]);
        let (mut io, pid, job) = l.launch_in_job().unwrap();
        unsafe {
            let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).unwrap();
            let mut inside = windows::core::BOOL::default();
            IsProcessInJob(p, Some(job), &mut inside).unwrap();
            let _ = CloseHandle(p);
            assert!(inside.as_bool());
        }
        io.process.kill();
    }
}
