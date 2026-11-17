//! `--leak-smoke`: no window. Navigates 200 folders in one tab, then opens
//! and closes 50 tabs, and prints private bytes, handles and GDI/USER
//! objects before and after. A short warm-up round runs first so one-time
//! allocations (thread pools, allocator arenas) are not counted as leaks.

use crate::view::View;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tsubame_ui::Density;
use tsubame_win::procinfo::counters;

const NAVS: usize = 200;
const TABS: usize = 50;

fn settle(v: &mut View) {
    let t0 = Instant::now();
    while v.tab().is_loading() && t0.elapsed() < Duration::from_secs(10) {
        v.pump();
        std::thread::sleep(Duration::from_millis(1));
    }
    v.pump();
}

/// Folders to visit: children of the start folder and of its siblings,
/// cycled until `NAVS` navigations are done.
fn pick_folders(v: &mut View, start: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut roots = vec![start.to_string()];
    if let Some(p) = crate::nav::parent(start) {
        roots.push(p);
    }
    for r in roots {
        v.go(&r);
        settle(v);
        let t = v.tab();
        for i in 0..t.shown() {
            if let Some((p, true)) = t.entry_at(i) {
                out.push(p);
                if out.len() >= 64 {
                    return out;
                }
            }
        }
    }
    if out.is_empty() {
        out.push(start.to_string());
    }
    out
}

fn round(v: &mut View, folders: &[String], navs: usize, tabs: usize) {
    for i in 0..navs {
        v.go(&folders[i % folders.len()]);
        settle(v);
    }
    for _ in 0..tabs {
        v.new_tab();
        settle(v);
        v.close_tab(v.tabs.active);
    }
}

pub fn run(start: &str) -> String {
    let wake: tsubame_win::enumerate::Wake = Arc::new(|| {});
    let mut v = View::new(start, wake, true, Density::Compact);
    settle(&mut v);
    let folders = pick_folders(&mut v, start);
    round(&mut v, &folders, 20, 5); // warm-up
    let before = counters();
    round(&mut v, &folders, NAVS, TABS);
    // let detached enumerator threads finish before measuring
    std::thread::sleep(Duration::from_millis(500));
    let after = counters();
    let mb = |b: usize| b as f64 / 1048576.0;
    let mut s = String::new();
    s.push_str(&format!("leak-smoke: {NAVS} navigations over {} folders, {TABS} tab open/close, start {start}\n", folders.len()));
    s.push_str(&format!("before: {}\n", before.line()));
    s.push_str(&format!("after:  {}\n", after.line()));
    s.push_str(&format!(
        "delta: private {:+.2} MB, working set {:+.2} MB, handles {:+}, gdi {:+}, user {:+}\n",
        mb(after.private_bytes) - mb(before.private_bytes),
        mb(after.working_set) - mb(before.working_set),
        after.handles as i64 - before.handles as i64,
        after.gdi as i64 - before.gdi as i64,
        after.user as i64 - before.user as i64
    ));
    s
}
