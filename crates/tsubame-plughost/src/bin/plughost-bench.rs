//! Measurement client for the spike (run on a dedicated benchmark machine):
//!
//!   plughost-bench spawn <host.exe> <file> [--pre] [--runs N]   spawn -> ready, kill, respawn
//!   plughost-bench rtt   <host.exe> <file> [--pre]               end-to-end batch round trip over pipes
//!   plughost-bench mem   <host.exe> <file> [--pre]               private bytes / working set growth over 1,000 calls
//!
//! Windows only (process memory counters).

use std::io::{BufRead, BufReader, BufWriter};
use std::os::windows::io::AsRawHandle;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};
use tsubame_plughost::proto;

#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCountersEx {
    cb: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged_pool: usize,
    quota_paged_pool: usize,
    quota_peak_non_paged_pool: usize,
    quota_non_paged_pool: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

unsafe extern "system" {
    fn K32GetProcessMemoryInfo(process: *mut core::ffi::c_void, counters: *mut ProcessMemoryCountersEx, cb: u32) -> i32;
}

struct Mem {
    private_mb: f64,
    ws_mb: f64,
    peak_ws_mb: f64,
}

fn mem_of(child: &Child) -> Mem {
    let mut c = ProcessMemoryCountersEx { cb: size_of::<ProcessMemoryCountersEx>() as u32, ..Default::default() };
    // SAFETY: handle is the live child process handle; struct is repr(C) with cb set.
    let ok = unsafe { K32GetProcessMemoryInfo(child.as_raw_handle() as _, &mut c, c.cb) };
    assert!(ok != 0, "K32GetProcessMemoryInfo failed");
    let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
    Mem { private_mb: mb(c.private_usage), ws_mb: mb(c.working_set), peak_ws_mb: mb(c.peak_working_set) }
}

struct Client {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    out: BufReader<ChildStdout>,
    ready: String,
    spawn_ms: f64,
}

fn spawn(host: &str, file: &str, pre: bool) -> Client {
    let t = Instant::now();
    let mut cmd = Command::new(host);
    cmd.arg("serve").arg(file);
    if pre {
        cmd.arg("--pre");
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().expect("spawn host");
    let stdin = BufWriter::with_capacity(1 << 20, child.stdin.take().unwrap());
    let mut out = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
    let mut ready = String::new();
    out.read_line(&mut ready).expect("read ready line");
    let spawn_ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(ready.starts_with("ready"), "host did not say ready: {ready:?}");
    Client { child, stdin, out, ready: ready.trim().to_string(), spawn_ms }
}

impl Client {
    fn call(&mut self, op: u8, payload: &[u8], resp: &mut Vec<u8>) {
        proto::write_request(&mut self.stdin, op, payload).unwrap();
        proto::read_response(&mut self.out, resp).unwrap();
    }
    /// Kill and reap; returns kill+wait time in ms.
    fn kill(mut self) -> f64 {
        let t = Instant::now();
        let _ = self.child.kill();
        let _ = self.child.wait();
        t.elapsed().as_secs_f64() * 1e3
    }
}

fn pct(v: &mut [f64]) -> (f64, f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize];
    (v[0], p(0.5), p(0.95), v[v.len() - 1])
}

fn row(label: &str, unit: &str, v: &mut Vec<f64>) {
    let (mn, med, p95, mx) = pct(v);
    println!("{label:<34} n={:<5} min {mn:>9.2}  median {med:>9.2}  p95 {p95:>9.2}  max {mx:>9.2}  {unit}", v.len());
}

fn field(ready: &str, key: &str) -> f64 {
    ready
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(f64::NAN)
}

fn cmd_spawn(host: &str, file: &str, pre: bool, runs: usize) {
    println!("spawn -> ready, {} ({file})", if pre { "precompiled .cwasm (deserialize)" } else { "compiled from .wasm at startup" });
    let mut first = None;
    let (mut total, mut inner, mut eng, mut load, mut inst, mut kill, mut respawn) = (vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
    for i in 0..runs {
        let c = spawn(host, file, pre);
        if i == 0 {
            first = Some(c.spawn_ms);
        }
        total.push(c.spawn_ms);
        inner.push(field(&c.ready, "total_us") / 1e3);
        eng.push(field(&c.ready, "engine_us") / 1e3);
        load.push(field(&c.ready, "load_us") / 1e3);
        inst.push(field(&c.ready, "inst_us") / 1e3);
        // (e) kill, then respawn to ready
        let k = c.kill();
        kill.push(k);
        let c2 = spawn(host, file, pre);
        respawn.push(k + c2.spawn_ms);
        c2.kill();
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("first run (coldest, same boot): {:.1} ms", first.unwrap());
    row("client: CreateProcess -> ready", "ms", &mut total);
    row("host inside main: main -> ready", "ms", &mut inner);
    row("  Engine::new", "ms", &mut eng);
    row("  component load/compile", "ms", &mut load);
    row("  link + instantiate", "ms", &mut inst);
    row("kill + wait (TerminateProcess)", "ms", &mut kill);
    row("kill -> new host ready", "ms", &mut respawn);
}

fn cmd_rtt(host: &str, file: &str, pre: bool) {
    println!("round trip over pipes, {} ({file})", if pre { "precompiled" } else { "compiled at startup" });
    let mut c = spawn(host, file, pre);
    let mut resp = Vec::new();
    for &(n, runs) in &[(10_000usize, 300usize), (100, 3000), (1, 10000)] {
        let paths = proto::gen_paths(n);
        let packed = proto::pack(&paths);
        // verify once
        let want: Vec<String> = paths.iter().map(|p| proto::expected(p)).collect();
        c.call(proto::OP_LIST, &packed, &mut resp);
        assert_eq!(proto::unpack(&resp), want, "list result wrong");
        c.call(proto::OP_PACKED, &packed, &mut resp);
        assert_eq!(proto::unpack(&resp), want, "packed result wrong");
        for _ in 0..50.min(runs) {
            c.call(proto::OP_LIST, &packed, &mut resp);
            c.call(proto::OP_PACKED, &packed, &mut resp);
        }
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for _ in 0..runs {
            let t = Instant::now();
            c.call(proto::OP_LIST, &packed, &mut resp);
            a.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            c.call(proto::OP_PACKED, &packed, &mut resp);
            b.push(t.elapsed().as_secs_f64() * 1e3);
        }
        row(&format!("RTT list<string>    {n} paths"), "ms", &mut a);
        row(&format!("RTT packed list<u8> {n} paths"), "ms", &mut b);
    }
    c.kill();
}

fn cmd_mem(host: &str, file: &str, pre: bool) {
    println!("memory of the host process, {} ({file})", if pre { "precompiled" } else { "compiled at startup" });
    let mut c = spawn(host, file, pre);
    let show = |tag: &str, c: &Client| {
        let m = mem_of(&c.child);
        println!("{tag:<44} private {:>7.2} MB   working set {:>7.2} MB   peak WS {:>7.2} MB", m.private_mb, m.ws_mb, m.peak_ws_mb);
    };
    show("just after ready", &c);
    std::thread::sleep(Duration::from_secs(2));
    show("ready + 2 s idle", &c);
    let mut resp = Vec::new();
    for &(n, op, name) in &[(10_000usize, proto::OP_LIST, "list<string> x10,000 paths"), (10_000, proto::OP_PACKED, "packed x10,000 paths"), (1, proto::OP_LIST, "list<string> x1 path")] {
        let packed = proto::pack(&proto::gen_paths(n));
        for i in 1..=1000 {
            c.call(op, &packed, &mut resp);
            if i == 1 || i == 10 || i % 250 == 0 {
                show(&format!("{name}: after {i} calls"), &c);
            }
        }
    }
    std::thread::sleep(Duration::from_secs(2));
    show("after 3,000 calls + 2 s idle", &c);
    c.kill();
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pre = a.iter().any(|x| x == "--pre");
    let runs = a.iter().position(|x| x == "--runs").and_then(|i| a.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(30);
    let (mode, host, file) = (a.get(1).map(String::as_str), a.get(2), a.get(3));
    let (Some(host), Some(file)) = (host, file) else {
        eprintln!("usage: plughost-bench spawn|rtt|mem <host.exe> <file> [--pre] [--runs N]");
        return;
    };
    match mode {
        Some("spawn") => cmd_spawn(host, file, pre, runs),
        Some("rtt") => cmd_rtt(host, file, pre),
        Some("mem") => cmd_mem(host, file, pre),
        _ => eprintln!("modes: spawn rtt mem"),
    }
}
