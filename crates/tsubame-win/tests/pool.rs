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

/// A new 64x64 BMP in the temp dir: never seen by the system thumbnail cache.
fn fresh_bmp() -> std::path::PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let (w, h) = (64u32, 64u32);
    let data = w * h * 3;
    let mut b = b"BM".to_vec();
    b.extend((54 + data).to_le_bytes());
    b.extend([0u8; 4]);
    b.extend(54u32.to_le_bytes());
    b.extend(40u32.to_le_bytes());
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend([0u8; 4]);
    b.extend(data.to_le_bytes());
    b.extend([0u8; 16]);
    b.extend((0..w * h).flat_map(|i| [(n >> 8) as u8 ^ i as u8, (n >> 16) as u8, (i / w * 4) as u8]));
    let p = std::env::temp_dir().join(format!("tsubame-pool-thumb-{n}.bmp"));
    std::fs::write(&p, b).unwrap();
    p
}

/// No thumbnail handler: no bitmap (the UI keeps the type icon), an error
/// code, and no hang.
#[test]
fn thumbnail_of_a_file_without_handler_is_an_error_reply() {
    let mut pool = Pool::new(1, 0, 0x8001);
    let th = pool.submit(Request::Thumb { path: r"C:\Windows\explorer.exe".into(), size: 64, cache_only: false });
    let replies = wait(&mut pool, 1, Duration::from_secs(10));
    let Some(Payload::Thumb { bmp, error, .. }) = replies.iter().find(|r| r.id == th.id).map(|r| &r.payload) else { panic!("no reply") };
    assert!(bmp.is_none() && error.is_some(), "{error:?}");
}

#[test]
fn thumbnail_lane_is_independent_of_the_icon_lane() {
    // one icon worker stuck in a long job: a thumbnail still comes back,
    // because thumbnails run on their own lane (its threads start lazily)
    let mut pool = Pool::with_lanes(1, 1, 0, 0x8001);
    pool.submit(Request::Hang { ms: 1_500 });
    std::thread::sleep(Duration::from_millis(100));
    let t = Instant::now();
    let img = fresh_bmp();
    let th = pool.submit(Request::Thumb { path: img.to_string_lossy().into_owned(), size: 64, cache_only: false });
    let replies = wait(&mut pool, 1, Duration::from_secs(10));
    assert!(replies.iter().any(|r| r.id == th.id), "thumbnail answered while the icon worker hangs");
    assert!(t.elapsed() < Duration::from_millis(1_200), "did not wait behind the hang: {:?}", t.elapsed());
    let Some(Payload::Thumb { bmp, error, .. }) = replies.iter().find(|r| r.id == th.id).map(|r| &r.payload) else { panic!() };
    let b = bmp.as_ref().unwrap_or_else(|| panic!("a real thumbnail of a fresh image (not in the thumbnail cache): {error:?} {}", img.display()));
    assert!(b.w.max(b.h) <= 64, "cut to the size asked: {}x{}", b.w, b.h);
}

/// An own-drawn menu whose build hangs (a shell extension that never returns
/// from QueryContextMenu, simulated by a sleep before any shell call) is given
/// up after `ctx_hang_after`: the worker is replaced, the job id is reported
/// lost, its late batches are dropped, and the next job runs on the fresh worker.
#[test]
fn hung_own_drawn_menu_build_is_abandoned() {
    use tsubame_win::shell::{CtxHandle, CtxOpts};
    let mut pool = Pool::new(1, 0, 0x8001);
    pool.ctx_hang_after = Duration::from_millis(200);
    let h = CtxHandle::new();
    let opts = CtxOpts { debug_hang_ms: 1_500, ..Default::default() };
    let hung = pool.submit(Request::ctx(vec![r"C:\Windows".into()], 41, opts, &h));
    std::thread::sleep(Duration::from_millis(400));
    assert!(pool.busy(), "a building menu arms the watchdog");
    assert_eq!(pool.check_hung(), vec![hung.id]);
    assert_eq!(pool.stats().workers_replaced, 1);
    assert_eq!(pool.leaked_alive(), 1);
    let ok = pool.submit(Request::Icon { src: IconSource::Ext(".txt".into()), size: 16 });
    let r = wait(&mut pool, 1, Duration::from_secs(10));
    assert!(r.iter().any(|r| r.id == ok.id));
    // the abandoned build finishes later; nothing of it may arrive
    std::thread::sleep(Duration::from_millis(1_600));
    let late = pool.drain();
    assert!(late.iter().all(|r| r.id != hung.id), "late batches of an abandoned worker are dropped");
    let t = Instant::now();
    while pool.leaked_alive() > 0 && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(pool.leaked_alive(), 0, "the leaked thread exits once its call returns");
}

/// A request that cannot bind fails fast with one `Failed` batch of its generation.
#[test]
fn own_drawn_menu_without_paths_fails_with_its_generation() {
    use tsubame_win::shell::ctxmodel::Part;
    use tsubame_win::shell::{CtxHandle, CtxOpts};
    let mut pool = Pool::new(1, 0, 0x8001);
    let h = CtxHandle::new();
    let t = pool.submit(Request::ctx(vec![], 9, CtxOpts::default(), &h));
    let r = wait(&mut pool, 1, Duration::from_secs(10));
    let Payload::Ctx(b) = &r.iter().find(|r| r.id == t.id).expect("reply").payload else { panic!("not a ctx batch") };
    assert_eq!(b.generation, 9);
    assert!(matches!(&b.part, Part::Failed(e) if e == "no paths"), "{:?}", b.part);
}

/// Manual probe of the real worker path: prints every batch for
/// `TSUBAME_PKG_PROBE` (`-- --ignored --nocapture`).
#[test]
#[ignore]
fn probe_ctx_batches() {
    use tsubame_win::shell::ctxmodel::Part;
    use tsubame_win::shell::{CtxHandle, CtxOpts};
    let path = std::env::var("TSUBAME_PKG_PROBE").unwrap_or_else(|_| r"C:\Windows".into());
    let mut pool = Pool::new(1, 0, 0x8001);
    let h = CtxHandle::new();
    let _t = pool.submit(Request::ctx(vec![path], 1, CtxOpts::default(), &h));
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(8) {
        for r in pool.drain() {
            if let Payload::Ctx(b) = &r.payload {
                let show = |v: &[tsubame_win::shell::ctxmodel::MenuItem]| v.iter().map(|i| format!("{}[{:?}]", i.label, i.verb)).collect::<Vec<_>>();
                match &b.part {
                    Part::Native(v) => println!("native {:?} {:?}", b.timing, show(v)),
                    Part::Packaged(v) => println!("packaged {:?} {:?}", b.timing, show(v)),
                    Part::Complete => {
                        println!("complete");
                        return;
                    }
                    p => println!("other {}", format!("{p:?}").chars().take(120).collect::<String>()),
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The startup pre-warm answers `Payload::Warm`, sends no menu batch, and the
/// first real background menu after it arrives with warm timings.
#[test]
fn ctx_prewarm_answers_and_leaves_no_menu_behind() {
    use tsubame_win::shell::ctxmodel::Part;
    use tsubame_win::shell::{CtxHandle, CtxOpts};
    let mut pool = Pool::new(2, 0, 0x8001);
    let w = pool.submit(Request::CtxWarm { folder: r"C:\Windows".into(), icon_px: 16 });
    let r = wait(&mut pool, 1, Duration::from_secs(30));
    assert_eq!(r.len(), 1, "the warm job answers once");
    assert_eq!(r[0].id, w.id);
    assert!(matches!(r[0].payload, Payload::Warm));
    let h = CtxHandle::new();
    let t0 = Instant::now();
    pool.submit(Request::ctx(vec![r"C:\Windows".into()], 5, CtxOpts { background: true, submenus: false, ..Default::default() }, &h));
    let mut top_ms = None;
    while t0.elapsed() < Duration::from_secs(20) && top_ms.is_none() {
        for r in pool.drain() {
            if let Payload::Ctx(b) = r.payload
                && matches!(b.part, Part::Native(_))
            {
                top_ms = Some(t0.elapsed().as_millis());
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let top_ms = top_ms.expect("the top level arrives");
    eprintln!("first menu after prewarm: top level {top_ms} ms");
    assert!(top_ms < 400, "warm first menu took {top_ms} ms");
}
