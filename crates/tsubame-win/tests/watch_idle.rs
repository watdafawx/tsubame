//! Watchers must not cost threads, and idle watchers must cost no CPU.
//!
//! Its own test binary (own process) so no other test's threads or CPU time
//! pollute the counts. One `#[test]` on purpose: the phases run in order.

use std::fs;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use tsubame_win::watch::{DirWatcher, Message, Options};
use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};

fn threads() -> usize {
    let pid = unsafe { GetCurrentProcessId() };
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.unwrap();
    let mut e = THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
    let mut n = 0;
    let mut ok = unsafe { Thread32First(snap, &mut e) }.is_ok();
    while ok {
        if e.th32OwnerProcessID == pid {
            n += 1;
        }
        ok = unsafe { Thread32Next(snap, &mut e) }.is_ok();
    }
    unsafe {
        let _ = CloseHandle(snap);
    }
    n
}

/// Kernel + user CPU time of this process, in 100 ns units.
fn cpu_ticks() -> u64 {
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.unwrap();
    let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    f(k) + f(u)
}

fn drain(rx: &Receiver<Message>, quiet: Duration) -> usize {
    let mut n = 0;
    while rx.recv_timeout(quiet).is_ok() {
        n += 1;
    }
    n
}

#[test]
fn fifty_watchers_one_thread_and_idle_costs_nothing() {
    let root = std::env::temp_dir().join(format!("tsubame-watch-idle-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let dirs: Vec<_> = (0..50)
        .map(|i| {
            let d = root.join(format!("tab{i:02}"));
            fs::create_dir_all(&d).unwrap();
            d
        })
        .collect();

    let threads_before = threads();
    let started: Vec<_> = dirs
        .iter()
        .map(|d| DirWatcher::start(d, Options::default(), Arc::new(|| {})).expect("start"))
        .collect();
    let threads_after = threads();
    println!("threads: {threads_before} -> {threads_after} with {} watchers", started.len());
    assert!(
        threads_after <= threads_before + 1,
        "50 watchers must share one thread: {threads_before} -> {threads_after}"
    );

    // every watcher sees its own folder's change (the shared thread demuxes right)
    for (i, d) in dirs.iter().enumerate() {
        fs::write(d.join(format!("only-{i}.txt")), b"x").unwrap();
    }
    for (i, (_, rx)) in started.iter().enumerate() {
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Message::Batch(b)) => assert_eq!(b.describe(), [format!("+only-{i}.txt")]),
            other => panic!("watcher {i}: {other:?}"),
        }
    }
    for (_, rx) in &started {
        assert_eq!(drain(rx, Duration::from_millis(150)), 0, "no stray messages");
    }

    // keep 20, drop 30: the dropped ones free their handles, the rest still work
    let mut keep: Vec<_> = started;
    keep.truncate(20);
    std::thread::sleep(Duration::from_millis(500));

    // idle: 10 s, 20 armed watchers, no events
    let t0 = cpu_ticks();
    let w0 = Instant::now();
    std::thread::sleep(Duration::from_secs(10));
    let cpu = cpu_ticks() - t0;
    println!("idle CPU over {:?} with {} watchers: {} ms", w0.elapsed(), keep.len(), cpu / 10_000);
    // process times tick at ~15.6 ms: one tick of noise (the sleeping test thread) is allowed
    assert!(cpu <= 160_000, "idle watchers burned {} ms of CPU", cpu / 10_000);

    // still alive after the idle period
    fs::write(dirs[3].join("after.txt"), b"x").unwrap();
    match keep[3].1.recv_timeout(Duration::from_secs(3)) {
        Ok(Message::Batch(b)) => assert_eq!(b.describe(), ["+after.txt"]),
        other => panic!("{other:?}"),
    }
    drop(keep);
    let _ = fs::remove_dir_all(&root);
}
