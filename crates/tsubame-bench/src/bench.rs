//! The enumeration benchmark runner: N runs per method, median/p95 out.

use std::time::Duration;

use crate::clock::Clock;
use crate::cold::{ColdStatus, PurgeMode, purge};
use crate::enum_find;
use crate::enum_gfhie;
use crate::enum_nt;
use crate::ntinfo::Layout;
use crate::stats::Series;
use crate::store::EntryStore;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    /// FindFirstFileExW, FindExInfoBasic + FIND_FIRST_EX_LARGE_FETCH
    Find,
    /// NtQueryDirectoryFile, FileIdExtdDirectoryInformation, 64 KB buffer
    Nt64k,
    /// NtQueryDirectoryFile, FileIdExtdDirectoryInformation, 1 MB buffer
    Nt1m,
    /// GetFileInformationByHandleEx(FileIdExtdDirectoryInfo), 1 MB buffer
    Gfhie,
}

impl Method {
    pub const ALL: [Method; 4] = [Method::Find, Method::Nt64k, Method::Nt1m, Method::Gfhie];

    pub fn name(self) -> &'static str {
        match self {
            Method::Find => "find",
            Method::Nt64k => "nt64k",
            Method::Nt1m => "nt1m",
            Method::Gfhie => "gfhie",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        match s.to_ascii_lowercase().as_str() {
            "find" | "findfirst" => Some(Method::Find),
            "nt64k" | "nt-64k" => Some(Method::Nt64k),
            "nt1m" | "nt-1m" => Some(Method::Nt1m),
            "gfhie" | "gfhie1m" => Some(Method::Gfhie),
            _ => None,
        }
    }

    pub fn buffer(self) -> Option<usize> {
        match self {
            Method::Find => None,
            Method::Nt64k => Some(64 * 1024),
            Method::Nt1m => Some(1024 * 1024),
            Method::Gfhie => Some(1024 * 1024),
        }
    }

    pub fn buffer_label(self, override_bytes: Option<usize>) -> String {
        match self.buffer() {
            None => "kernel-managed".to_string(),
            Some(b) => {
                let b = override_bytes.unwrap_or(b);
                if b.is_multiple_of(1024) {
                    format!("{} KB", b / 1024)
                } else {
                    format!("{b} B")
                }
            }
        }
    }
}

pub struct MethodRun {
    pub method: Method,
    pub entries: usize,
    pub name_bytes: usize,
    pub dots_skipped: usize,
    pub first: Series,
    pub complete: Series,
    /// Time to build the sorted index over the finished store (only with --sort).
    pub sort: Series,
    pub cold: Option<ColdStatus>,
    pub layout: Option<Layout>,
    pub buffer_label: String,
    /// Store from the last run, kept for the cross-method comparison.
    pub store: Option<EntryStore>,
}

/// One enumeration into a fresh store. Returns the NT layout when it applies.
fn enumerate_once(
    method: Method,
    dir: &str,
    buffer: Option<usize>,
    store: &mut EntryStore,
    clock: &Clock,
    start: i64,
) -> Result<Option<Layout>, String> {
    match method {
        Method::Find => enum_find::enumerate(dir, store, clock, start).map(|()| None),
        Method::Nt64k | Method::Nt1m => {
            let buf = buffer.expect("NT method needs a buffer size");
            enum_nt::enumerate(dir, buf, store, clock, start).map(Some)
        }
        Method::Gfhie => {
            let buf = buffer.expect("GFHIE needs a buffer size");
            enum_gfhie::enumerate(dir, buf, store, clock, start).map(|()| None)
        }
    }
}

pub struct RunOptions {
    pub runs: usize,
    pub cold: bool,
    pub purge_mode: PurgeMode,
    pub buffer_override: Option<usize>,
    pub settle_ms: u64,
    /// Also build the sorted index after each run (the PLAN budget says
    /// "complete and sorted").
    pub sort: bool,
    pub verbose: bool,
}

pub fn run(method: Method, dir: &str, opts: &RunOptions) -> Result<MethodRun, String> {
    let clock = Clock::new();
    let buffer = method.buffer().map(|b| opts.buffer_override.unwrap_or(b));

    let mut first = Series::default();
    let mut complete = Series::default();
    let mut sort = Series::default();
    let mut cold_status: Option<ColdStatus> = None;
    let mut layout: Option<Layout> = None;
    let mut store: Option<EntryStore> = None;

    if !opts.cold {
        // One warm-up run, discarded.
        let mut warm = EntryStore::new();
        let start = clock.now();
        let _ = enumerate_once(method, dir, buffer, &mut warm, &clock, start)?;
        if opts.verbose {
            println!(
                "  warm-up: {:.3} ms, {} entries",
                clock.ms(clock.now() - start),
                warm.len()
            );
        }
    }

    for i in 0..opts.runs.max(1) {
        if opts.cold {
            let status = purge(opts.purge_mode);
            if cold_status.is_none() {
                cold_status = Some(status.clone());
            }
            if opts.settle_ms > 0 {
                std::thread::sleep(Duration::from_millis(opts.settle_ms));
            }
        }
        let mut s = EntryStore::new();
        let start = clock.now();
        let l = enumerate_once(method, dir, buffer, &mut s, &clock, start)?;
        let done = clock.now();
        let complete_ticks = done - start;
        s.finish_first_batch(complete_ticks);
        let first_ms = clock.ms(s.first_batch_ticks.unwrap_or(complete_ticks));
        let complete_ms = clock.ms(complete_ticks);
        if opts.verbose {
            println!(
                "  run {}: first-batch {:.3} ms, complete {:.3} ms, {} entries",
                i + 1,
                first_ms,
                complete_ms,
                s.len()
            );
        }
        first.push(first_ms);
        complete.push(complete_ms);
        if opts.sort {
            let sort_start = clock.now();
            let idx = s.sorted_index();
            sort.push(clock.ms(clock.now() - sort_start));
            drop(idx);
        }
        if l.is_some() {
            layout = l;
        }
        store = Some(s);
    }

    let last = store.as_ref().expect("at least one run");
    Ok(MethodRun {
        method,
        entries: last.len(),
        name_bytes: last.name_bytes(),
        dots_skipped: last.dots_skipped,
        first,
        complete,
        sort,
        cold: cold_status,
        layout,
        buffer_label: method.buffer_label(opts.buffer_override),
        store,
    })
}
