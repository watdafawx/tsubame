//! Headless pool checks (no window: replies are polled, the wake post goes
//! to a null HWND and is dropped).

use std::time::{Duration, Instant};
use tsubame_win::shell::{IconSource, Payload, Pool, Request};

fn wait(pool: &mut Pool, n: usize, timeout: Duration) -> Vec<tsubame_win::shell::Reply> {
    let t = Instant::now();
    let mut out = Vec::new();
    while out.len() < n && t.elapsed() < timeout {
        out.extend(pool.drain());
        std::thread::sleep(Duration::from_millis(5));
    }
    out
}

#[test]
fn icons_by_extension_and_folder() {
    let mut pool = Pool::new(2, 0, 0x8001);
    let a = pool.submit(Request::Icon { src: IconSource::Ext(".txt".into()), size: 32 });
    let b = pool.submit(Request::Icon { src: IconSource::Folder, size: 16 });
    let c = pool.submit(Request::Icon { src: IconSource::Path(r"C:\Windows\explorer.exe".into()), size: 32 });
    let replies = wait(&mut pool, 3, Duration::from_secs(20));
    assert_eq!(replies.len(), 3);
    for r in &replies {
        let Payload::Icon(Some(bmp)) = &r.payload else { panic!("no icon for {}", r.id) };
        assert!(bmp.w >= 16 && bmp.h >= 16, "{}x{}", bmp.w, bmp.h);
        assert!(bmp.px.iter().any(|p| p >> 24 == 255), "some opaque pixels");
        assert!(bmp.px.iter().any(|p| p >> 24 == 0), "some transparent pixels");
        eprintln!("icon id {} {}x{} worker {} queued {} us work {} us", r.id, bmp.w, bmp.h, r.worker, r.queued_us, r.work_us);
    }
    let ids: Vec<u64> = replies.iter().map(|r| r.id).collect();
    assert!(ids.contains(&a.id) && ids.contains(&b.id) && ids.contains(&c.id));
}

#[test]
fn cancelled_jobs_are_skipped() {
    let mut pool = Pool::new(1, 0, 0x8001);
    // keep the single worker busy so the next jobs are still queued
    pool.submit(Request::Hang { ms: 300 });
    std::thread::sleep(Duration::from_millis(50));
    let t = pool.submit(Request::Icon { src: IconSource::Ext(".rs".into()), size: 16 });
    t.cancel();
    let keep = pool.submit(Request::Icon { src: IconSource::Ext(".md".into()), size: 16 });
    let replies = wait(&mut pool, 2, Duration::from_secs(10));
    assert!(replies.iter().all(|r| r.id != t.id), "cancelled job must not reply");
    assert!(replies.iter().any(|r| r.id == keep.id));
    assert_eq!(pool.stats().skipped_cancelled, 1);
}

#[test]
fn hung_worker_is_replaced() {
    let mut pool = Pool::new(1, 0, 0x8001);
    pool.hang_after = Duration::from_millis(200);
    let hung = pool.submit(Request::Hang { ms: 5_000 });
    std::thread::sleep(Duration::from_millis(400));
    assert!(pool.busy());
    let lost = pool.check_hung();
    assert_eq!(lost, vec![hung.id]);
    let ok = pool.submit(Request::Icon { src: IconSource::Ext(".txt".into()), size: 16 });
    let replies = wait(&mut pool, 1, Duration::from_secs(10));
    assert!(replies.iter().any(|r| r.id == ok.id), "replacement worker answers while the old one sleeps");
    assert_eq!(pool.stats().workers_replaced, 1);
    assert_eq!(pool.leaked_alive(), 1);
}

#[test]
fn icon_sizes_direct() {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    for (src, size) in [(IconSource::Folder, 16), (IconSource::Folder, 32), (IconSource::Ext(".txt".into()), 16), (IconSource::Ext(".txt".into()), 48), (IconSource::Ext(".txt".into()), 256)] {
        let t = Instant::now();
        let b = tsubame_win::shell::icon_on_this_thread(&src, size);
        eprintln!("{src:?} {size}: {:?} in {} us", b.as_ref().map(|b| (b.w, b.h)), t.elapsed().as_micros());
    }
}
