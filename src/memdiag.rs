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


/// One private allocation outside every heap: reserved bytes, committed bytes,
/// layout of its regions (R reserved, G guard, W committed read/write, o other).
#[derive(Clone, Debug)]
struct OutAlloc {
    reserved: usize,
    committed: usize,
    layout: String,
}

fn outside_allocs() -> HashMap<usize, OutAlloc> {
    let mut handles = vec![HANDLE::default(); 256];
    let n = unsafe { GetProcessHeaps(&mut handles) } as usize;
    handles.truncate(n.min(256));
    let ph = unsafe { GetProcessHeap() }.unwrap_or_default();
    let mut heap_bases = HashSet::new();
    for h in &handles {
        heap_stat(*h, *h == ph, &mut heap_bases);
    }
    let mut out: HashMap<usize, OutAlloc> = HashMap::new();
    let mut a = 0usize;
    loop {
        let mut m = MEMORY_BASIC_INFORMATION::default();
        if unsafe { VirtualQuery(Some(a as *const _), &mut m, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            break;
        }
        if m.Type == MEM_PRIVATE && m.State != MEM_FREE && !heap_bases.contains(&(m.AllocationBase as usize)) {
            let e = out.entry(m.AllocationBase as usize).or_insert(OutAlloc { reserved: 0, committed: 0, layout: String::new() });
            e.reserved += m.RegionSize;
            let c = if m.State == MEM_COMMIT {
                e.committed += m.RegionSize;
                if m.Protect.0 & PAGE_GUARD.0 != 0 { 'G' } else if m.Protect.0 & PAGE_READWRITE.0 != 0 { 'W' } else { 'o' }
            } else {
                'R'
            };
            if e.layout.len() < 24 && !e.layout.ends_with(c) {
                e.layout.push(c);
            }
        }
        let next = (m.BaseAddress as usize).wrapping_add(m.RegionSize);
        if next <= a {
            break;
        }
        a = next;
    }
    out
}


/// Live threads of this process grouped by start address (module + offset),
/// from `NtQueryInformationThread(ThreadQuerySetWin32StartAddress)`.
fn thread_groups() -> (usize, Vec<(String, usize)>) {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::System::Threading::*;
    use windows::core::w;
    type NtQit = unsafe extern "system" fn(HANDLE, u32, *mut std::ffi::c_void, u32, *mut u32) -> i32;
    let mut groups: HashMap<String, usize> = HashMap::new();
    let mut total = 0;
    unsafe {
        let Ok(nt) = GetModuleHandleW(w!("ntdll.dll")) else { return (0, vec![]) };
        let Some(f) = GetProcAddress(nt, windows::core::s!("NtQueryInformationThread")) else { return (0, vec![]) };
        let f: NtQit = std::mem::transmute(f);
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else { return (0, vec![]) };
        let pid = GetCurrentProcessId();
        let mut e = THREADENTRY32 { dwSize: size_of::<THREADENTRY32>() as u32, ..Default::default() };
        let mut ok = Thread32First(snap, &mut e).is_ok();
        while ok {
            if e.th32OwnerProcessID == pid {
                total += 1;
                let key = match OpenThread(THREAD_QUERY_INFORMATION, false, e.th32ThreadID) {
                    Ok(h) => {
                        let mut addr: usize = 0;
                        let r = f(h, 9, &mut addr as *mut usize as *mut _, size_of::<usize>() as u32, std::ptr::null_mut());
                        let _ = windows::Win32::Foundation::CloseHandle(h);
                        if r >= 0 && addr != 0 {
                            let mut m = MEMORY_BASIC_INFORMATION::default();
                            VirtualQuery(Some(addr as *const _), &mut m, size_of::<MEMORY_BASIC_INFORMATION>());
                            format!("{}+{:#x}", module_name(m.AllocationBase as usize), addr - m.AllocationBase as usize)
                        } else {
                            format!("nt={r:#x}")
                        }
                    }
                    Err(_) => "open-failed".to_string(),
                };
                *groups.entry(key).or_default() += 1;
            }
            ok = Thread32Next(snap, &mut e).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    let mut v: Vec<_> = groups.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    (total, v)
}

fn thread_report(label: &str) -> String {
    let (n, v) = thread_groups();
    let mut o = String::new();
    let _ = writeln!(o, "memdiag.{label}.threads={n}");
    for (k, c) in v.iter().take(12) {
        let _ = writeln!(o, "memdiag.{label}.thread x{c} start {k}");
    }
    o
}

static MARK: std::sync::Mutex<Option<HashMap<usize, OutAlloc>>> = std::sync::Mutex::new(None);

/// `TSUBAME_MEMDIAG=1`: the stress calls it at each sample. "after menu 10"
/// remembers the outside-heap allocations; later samples list the ones made
/// since (grouped by shape), to tell thread stacks from heap segments.
pub fn track(label: &str) -> String {
    if !enabled() {
        return String::new();
    }
    if label == "after menu 10" {
        *MARK.lock().unwrap() = Some(outside_allocs());
        return thread_report(label);
    }
    let g = MARK.lock().unwrap();
    let Some(old) = g.as_ref() else { return String::new() };
    let now = outside_allocs();
    let mut groups: std::collections::BTreeMap<(usize, usize, String), (usize, usize)> = Default::default();
    let (mut n_new, mut b_new) = (0usize, 0usize);
    for (b, a) in &now {
        match old.get(b) {
            None => {
                n_new += 1;
                b_new += a.committed;
                let e = groups.entry((a.reserved, a.committed, a.layout.clone())).or_default();
                e.0 += 1;
                e.1 = e.1.max(*b);
            }
            Some(o) if a.committed > o.committed => {
                b_new += a.committed - o.committed;
                let e = groups.entry((usize::MAX, a.committed - o.committed, format!("grew:{}", a.layout))).or_default();
                e.0 += 1;
                e.1 = e.1.max(*b);
            }
            _ => {}
        }
    }
    let gone = old.keys().filter(|b| !now.contains_key(b)).count();
    let mut o = thread_report(label);
    let _ = writeln!(o, "memdiag.{label}.since10: new_allocs={n_new} committed_delta_mb={:.2} freed_allocs={gone}", mb(b_new));
    for ((res, com, lay), (cnt, base)) in groups.iter().rev().take(16) {
        let _ = writeln!(o, "memdiag.{label}.since10: x{cnt} reserved={}K committed={}K layout={lay} e.g. {base:#x}", if *res == usize::MAX { 0 } else { res >> 10 }, com >> 10);
    }
    o
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

/// One compact line (for per-sample timelines): private, all heaps committed,
/// process heap busy / free, outside heaps, writable images, mapped, Rust live.
pub fn line(label: &str) -> String {
    if !enabled() {
        return String::new();
    }
    let s = snapshot();
    let mb = |b: usize| b as f64 / 1048576.0;
    let committed: usize = s.heaps.iter().map(|h| h.committed).sum();
    let (busy, free) = s.heaps.iter().find(|h| h.process_heap).map(|h| (h.busy, h.committed.saturating_sub(h.busy))).unwrap_or((0, 0));
    let rust = rust_live().map(|(l, _)| mb(l)).unwrap_or(-1.0);
    format!(
        "memdiag.line {label}: private={:.2} heaps={:.2} heap0_busy={:.2} heap0_free={:.2} outside={:.2} allocs={} images={:.2} mapped={:.2} rust={:.2}\n",
        mb(s.private_usage),
        mb(committed),
        mb(busy),
        mb(free),
        mb(s.private_outside_heaps),
        s.outside_allocs,
        mb(s.image_writable),
        mb(s.mapped_commit),
        rust
    )
}
