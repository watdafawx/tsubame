//! `JobLauncher` (PH-07, tier 1): containment of the host process, checked
//! with the mock host of `plughost-bench`. Windows only.
#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tsubame_plughost::client::{Launcher, ProcessLauncher};
use tsubame_plughost::launcher::JobLauncher;
use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

const MOCK_EXE: &str = env!("CARGO_BIN_EXE_plughost-bench");

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tsubame-launcher-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn first_line(mut io: tsubame_plughost::client::HostIo) -> String {
    let mut line = String::new();
    BufReader::new(&mut io.output).read_line(&mut line).unwrap();
    io.process.kill();
    line.trim().to_string()
}

/// True while the process with this id is running.
fn alive(pid: u32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Err(_) => false, // gone: no such process
            Ok(h) => {
                let r = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
                let _ = CloseHandle(h);
                r
            }
        }
    }
}

#[test]
fn killing_the_owner_kills_the_host() {
    let dir = work_dir("owner");
    let mut owner = Command::new(MOCK_EXE).arg("owner").arg(MOCK_EXE).arg(&dir).stdout(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(owner.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    let host: u32 = line.trim().parse().unwrap_or_else(|_| panic!("owner printed {line:?}"));
    assert!(alive(host), "the host runs while its owner does");

    owner.kill().unwrap(); // TerminateProcess: no destructors run, so only the job can end the host
    owner.wait().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while alive(host) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(host), "the host outlived its owner");
}

#[test]
fn a_host_can_not_start_a_child() {
    let dir = work_dir("spawn");
    let jobbed = JobLauncher::new(MOCK_EXE, &dir, 64).unwrap().with_elevation_check(|| false).with_args(vec!["mock".into(), "spawn".into()]);
    let line = first_line(jobbed.launch().unwrap());
    assert!(line.starts_with("spawn-err"), "child start must fail in the job, got {line:?}");

    // The probe is meaningful: without the job the same host can start a child.
    let mut plain = ProcessLauncher::new(MOCK_EXE);
    plain.args = vec!["mock".into(), "spawn".into()];
    assert_eq!(first_line(plain.launch().unwrap()), "spawn-ok");
}

#[test]
fn the_host_gets_a_cleared_environment_and_the_fixed_folder() {
    let dir = work_dir("env");
    let l = JobLauncher::new(MOCK_EXE, &dir, 64).unwrap().with_elevation_check(|| false).with_args(vec!["mock".into(), "env".into()]);
    let line = first_line(l.launch().unwrap());
    let rest = line.strip_prefix("env ").unwrap();
    let (vars, cwd) = rest.split_once(" cwd ").unwrap();
    assert_eq!(vars, "SYSTEMROOT", "only SystemRoot is passed on, got {vars:?}");
    assert!(cwd.eq_ignore_ascii_case(dir.to_str().unwrap()), "working folder {cwd:?}");
}

#[test]
fn an_elevated_app_starts_no_host() {
    let dir = work_dir("elevated");
    let l = JobLauncher::new(MOCK_EXE, &dir, 64).unwrap().with_elevation_check(|| true);
    let e = l.launch().err().expect("must refuse");
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
}
