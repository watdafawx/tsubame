//! Where the private bytes are (`TSUBAME_MEMDIAG=1`, task MEM1).
//!
//! `snapshot` walks the address space with `VirtualQuery` and every heap with
//! `HeapWalk`, and splits the committed private memory into:
//! - each heap's committed regions (the process heap, where Rust, the CRT and
//!   most system DLLs allocate, and any private heaps), with the bytes in
//!   busy blocks and the free-but-committed rest (fragmentation),
//! - private memory outside every heap (`VirtualAlloc` directly: the GPU
//!   driver, D3D/D2D, thread stacks), grouped by allocation with the largest
//!   listed,
//! - writable image pages (DLL data sections) and pagefile-backed mapped views.
//!
//! Costs tens of ms (it locks and walks every heap), so it only runs when the
//! env var is set, at the bench's sample points. Never in the paint path.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Memory::*;

const PROCESS_HEAP_REGION: u16 = 0x0001;
const PROCESS_HEAP_UNCOMMITTED_RANGE: u16 = 0x0002;
const PROCESS_HEAP_ENTRY_BUSY: u16 = 0x0004;

pub fn enabled() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var_os("TSUBAME_MEMDIAG").is_some_and(|v| v != "0"))
}

#[derive(Default, Clone)]
pub struct HeapStat {
    pub process_heap: bool,
    /// committed bytes of the heap's regions (and its big VirtualAlloc'd blocks)
    pub committed: usize,
    /// bytes in busy (allocated) blocks
    pub busy: usize,
    pub busy_blocks: usize,
    /// busy blocks of 64 KB or more, and their bytes
    pub big_blocks: usize,
    pub big_bytes: usize,
}

#[derive(Default, Clone)]
pub struct Snap {
    pub private_usage: usize,
    pub heaps: Vec<HeapStat>,
    /// MEM_PRIVATE committed, total and outside every heap
    pub private_commit: usize,
    pub private_outside_heaps: usize,
    /// biggest private allocations outside heaps: (base, committed bytes, protect)
    pub top_outside: Vec<(usize, usize, u32)>,
    /// count of outside allocations
    pub outside_allocs: usize,
    pub image_writable: usize,
    /// writable image bytes by module: (name, bytes), largest first
    pub top_images: Vec<(String, usize)>,
    pub mapped_commit: usize,
}

fn heap_stat(h: HANDLE, process_heap: bool, bases: &mut HashSet<usize>) -> HeapStat {
    let mut s = HeapStat { process_heap, ..Default::default() };
    unsafe {
        if HeapLock(h).is_err() {
            return s;
        }
        let mut e = PROCESS_HEAP_ENTRY::default();
        while HeapWalk(h, &mut e).is_ok() {
            if e.wFlags & PROCESS_HEAP_REGION != 0 {
                let r = e.Anonymous.Region;
                s.committed += r.dwCommittedSize as usize;
                bases.insert(alloc_base(e.lpData as usize));
            } else if e.wFlags & PROCESS_HEAP_UNCOMMITTED_RANGE != 0 {
            } else if e.wFlags & PROCESS_HEAP_ENTRY_BUSY != 0 {
                let n = e.cbData as usize;
                s.busy += n;
                s.busy_blocks += 1;
                if n >= 64 << 10 {
                    s.big_blocks += 1;
                    s.big_bytes += n;
                    // large blocks live in their own VirtualAlloc outside the regions
                    let b = alloc_base(e.lpData as usize);
                    if bases.insert(b) {
                        s.committed += committed_of(b);
                    }
                }
            }
        }
        let _ = HeapUnlock(h);
    }
    s
}

fn alloc_base(addr: usize) -> usize {
    let mut m = MEMORY_BASIC_INFORMATION::default();
    let n = unsafe { VirtualQuery(Some(addr as *const _), &mut m, size_of::<MEMORY_BASIC_INFORMATION>()) };
    if n == 0 { addr } else { m.AllocationBase as usize }
}

/// Committed bytes of the allocation that starts at `base`.
fn committed_of(base: usize) -> usize {
    let mut total = 0;
    let mut a = base;
    loop {
        let mut m = MEMORY_BASIC_INFORMATION::default();
        if unsafe { VirtualQuery(Some(a as *const _), &mut m, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 || m.AllocationBase as usize != base {
            return total;
        }
        if m.State == MEM_COMMIT {
            total += m.RegionSize;
        }
        a = m.BaseAddress as usize + m.RegionSize;
    }
}

pub fn snapshot() -> Snap {
    let mut s = Snap { private_usage: crate::bench::memory().0, ..Default::default() };
    let mut handles = vec![HANDLE::default(); 256];
    let n = unsafe { GetProcessHeaps(&mut handles) } as usize;
    handles.truncate(n.min(256));
    let ph = unsafe { GetProcessHeap() }.unwrap_or_default();
    let mut heap_bases = HashSet::new();
    for h in &handles {
        s.heaps.push(heap_stat(*h, *h == ph, &mut heap_bases));
    }
    // the address space
    let mut by_base: HashMap<usize, (usize, u32)> = HashMap::new();
    let mut by_image: HashMap<usize, usize> = HashMap::new();
    let mut a = 0usize;
    loop {
        let mut m = MEMORY_BASIC_INFORMATION::default();
        if unsafe { VirtualQuery(Some(a as *const _), &mut m, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            break;
        }
        if m.State == MEM_COMMIT {
            let writable = m.Protect.0 & (PAGE_READWRITE.0 | PAGE_WRITECOPY.0 | PAGE_EXECUTE_READWRITE.0 | PAGE_EXECUTE_WRITECOPY.0) != 0;
            if m.Type == MEM_PRIVATE {
                s.private_commit += m.RegionSize;
                let base = m.AllocationBase as usize;
                if !heap_bases.contains(&base) {
                    s.private_outside_heaps += m.RegionSize;
                    let e = by_base.entry(base).or_insert((0, m.AllocationProtect.0));
                    e.0 += m.RegionSize;
                }
            } else if m.Type == MEM_IMAGE && writable {
                s.image_writable += m.RegionSize;
                *by_image.entry(m.AllocationBase as usize).or_default() += m.RegionSize;
            } else if m.Type == MEM_MAPPED && writable {
                s.mapped_commit += m.RegionSize;
            }
        }
        let next = (m.BaseAddress as usize).wrapping_add(m.RegionSize);
        if next <= a {
            break;
        }
        a = next;
    }
    s.outside_allocs = by_base.len();
    let mut v: Vec<(usize, usize, u32)> = by_base.into_iter().map(|(b, (n, p))| (b, n, p)).collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    v.truncate(12);
    s.top_outside = v;
    let mut im: Vec<(String, usize)> = by_image.into_iter().map(|(b, n)| (module_name(b), n)).collect();
    im.sort_by_key(|x| std::cmp::Reverse(x.1));
    im.truncate(10);
    s.top_images = im;
    s
}

fn module_name(base: usize) -> String {
    let mut buf = [0u16; 260];
    let n = unsafe { windows::Win32::System::LibraryLoader::GetModuleFileNameW(Some(windows::Win32::Foundation::HMODULE(base as *mut _)), &mut buf) } as usize;
    let full = String::from_utf16_lossy(&buf[..n.min(260)]);
    full.rsplit('\\').next().unwrap_or("?").to_string()
}

fn mb(b: usize) -> f64 {
    b as f64 / 1048576.0
}

/// `memdiag.<label>.*` report lines.
pub fn report(label: &str, s: &Snap, rust_live: Option<(usize, usize)>) -> String {
    let mut o = String::new();
    let p = format!("memdiag.{label}");
    let _ = writeln!(o, "{p}.private_usage_mb={:.2}", mb(s.private_usage));
    let _ = writeln!(o, "{p}.private_commit_mb={:.2}", mb(s.private_commit));
    let heap_commit: usize = s.heaps.iter().map(|h| h.committed).sum();
    let _ = writeln!(o, "{p}.heaps={} heaps_committed_mb={:.2}", s.heaps.len(), mb(heap_commit));
    for (i, h) in s.heaps.iter().enumerate() {
        if h.committed < 256 << 10 && !h.process_heap {
            continue;
        }
        let _ = writeln!(
            o,
            "{p}.heap{i}{}: committed_mb={:.2} busy_mb={:.2} blocks={} free_committed_mb={:.2} big_blocks={} big_mb={:.2}",
            if h.process_heap { "(process)" } else { "" },
            mb(h.committed),
            mb(h.busy),
            h.busy_blocks,
            mb(h.committed.saturating_sub(h.busy)),
            h.big_blocks,
            mb(h.big_bytes)
        );
    }
    let _ = writeln!(o, "{p}.outside_heaps_mb={:.2} allocs={}", mb(s.private_outside_heaps), s.outside_allocs);
    for (b, n, prot) in &s.top_outside {
        let _ = writeln!(o, "{p}.outside {b:#x} {:.2} MB protect={prot:#x}", mb(*n));
    }
    let _ = writeln!(o, "{p}.image_writable_mb={:.2} mapped_writable_mb={:.2}", mb(s.image_writable), mb(s.mapped_commit));
    for (name, n) in &s.top_images {
        let _ = writeln!(o, "{p}.image {name} {:.2} MB", mb(*n));
    }
    if let Some((live, peak)) = rust_live {
        let _ = writeln!(o, "{p}.rust_live_mb={:.2} rust_peak_mb={:.2}", mb(live), mb(peak));
    }
    o
}

/// `--features memdiag`: a global allocator that counts live Rust heap bytes,
/// so the process heap's busy bytes split into Rust and everything else
/// (DirectWrite, D2D, the shell, the CRT).
#[cfg(feature = "memdiag")]
pub mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    pub static LIVE: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK: AtomicUsize = AtomicUsize::new(0);

    pub struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() {
                let n = LIVE.fetch_add(l.size(), Relaxed) + l.size();
                PEAK.fetch_max(n, Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) };
            LIVE.fetch_sub(l.size(), Relaxed);
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            let q = unsafe { System.realloc(p, l, new) };
            if !q.is_null() {
                LIVE.fetch_sub(l.size(), Relaxed);
                let n = LIVE.fetch_add(new, Relaxed) + new;
                PEAK.fetch_max(n, Relaxed);
            }
            q
        }
    }
}

/// (live, peak) Rust heap bytes when built with `--features memdiag`.
pub fn rust_live() -> Option<(usize, usize)> {
    #[cfg(feature = "memdiag")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        Some((counting::LIVE.load(Relaxed), counting::PEAK.load(Relaxed)))
    }
    #[cfg(not(feature = "memdiag"))]
    None
}

/// One labelled report block, or nothing when `TSUBAME_MEMDIAG` is off.
pub fn sample(label: &str) -> String {
    if !enabled() {
        return String::new();
    }
    report(label, &snapshot(), rust_live())
}

/// `HeapSetInformation(NULL, HeapOptimizeResources)`: every heap drops its
/// LFH caches and decommits free pages (Windows 8.1+). Returns the process
/// heap's free-but-committed bytes it gave back, roughly, via private usage.
pub fn optimize_heaps() -> isize {
    #[repr(C)]
    struct OptimizeInfo {
        version: u32,
        flags: u32,
    }
    let before = crate::bench::memory().0 as isize;
    let info = OptimizeInfo { version: 1, flags: 0 };
    unsafe {
        let _ = HeapSetInformation(None, HeapOptimizeResources, Some(&info as *const _ as *const _), size_of::<OptimizeInfo>());
    }
    before - crate::bench::memory().0 as isize
}

/// `TSUBAME_MEMDIAG=2`: after the `end` sample, optimize the heaps and sample
/// again (`end_opt`), to measure what fragmentation holds.
pub fn sample_end() -> String {
    sample_opt("end")
}

/// `sample(label)`, and with `TSUBAME_MEMDIAG=2` the optimize probe after it
/// (`<label>_opt`).
pub fn sample_opt(label: &str) -> String {
    let mut s = sample(label);
    if std::env::var("TSUBAME_MEMDIAG").is_ok_and(|v| v == "2") {
        let freed = optimize_heaps();
        s.push_str(&format!("memdiag.{label}.optimize_freed_mb={:.2}\n", freed as f64 / 1048576.0));
        s.push_str(&sample(&format!("{label}_opt")));
    }
    s
}
