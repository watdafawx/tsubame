//! Measurement client and mock hosts. Not shipped.
//!
//!   plughost-bench run <host.exe> <plugin folder> <cache folder> [--runs N] [--paths N]
//!       (the plugin installed into the cache with `tsubame-plugc`)
//!       start-up time, batch round trips on the plugin's first column, clean shutdown
//!   plughost-bench mock hang|silent|exit|spawn|env
//!       stands in for the host in the tests: says hello and accepts the load, then
//!       never answers a call (`hang`) or exits on the first call (`exit`); `silent`
//!       says nothing at all; `spawn` and `env` print one line about what the process
//!       could do or was given (containment probes), then idle
//!   plughost-bench owner <host.exe> <work dir>
//!       owner-death probe: starts `<host.exe> mock hang` in a `JobLauncher`, prints its pid,
//!       then idles until killed

use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use tsubame_plughost::client::{Options, Plugin, ProcessLauncher};
use tsubame_plughost::install_record;
use tsubame_plughost::manifest::Manifest;
use tsubame_plughost::proto::{self, Info, Load, Request};

/// Realistic-looking Windows paths (about 70 bytes each).
fn gen_paths(n: usize) -> Vec<String> {
    const EXT: [&str; 8] = ["rs", "txt", "png", "md", "toml", "dll", "jpeg", "cargo-lock"];
    (0..n).map(|i| format!("C:\\Users\\alex\\Documents\\Projects\\folder_{:03}\\sub_{:02}\\file_{:06}.{}", i % 997, i % 31, i, EXT[i % EXT.len()])).collect()
}

fn row(label: &str, v: &mut [f64]) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize];
    println!("{label:<34} n={:<5} min {:>9.3}  median {:>9.3}  p95 {:>9.3}  max {:>9.3}  ms", v.len(), v[0], p(0.5), p(0.95), v[v.len() - 1]);
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn run(host: &str, plugin_dir: &str, cache: &str, runs: usize, n_paths: usize) -> Result<(), String> {
    let manifest = Manifest::load(Path::new(plugin_dir)).map_err(|e| e.to_string())?;
    let column = manifest.columns.first().ok_or("the plugin has no column")?.id.clone();
    let verified = install_record::verify(Path::new(plugin_dir), Path::new(cache), &manifest.id, &manifest.version, &[]).map_err(|e| e.to_string())?;
    let cwasm_len = std::fs::metadata(&verified.component).map_err(|e| e.to_string())?.len();
    let load = Load::new(&verified);
    let plugin = Plugin::new(load, ProcessLauncher::new(host), Options::default());
    let owned = gen_paths(n_paths.max(1));
    let paths: Vec<&str> = owned.iter().map(String::as_str).collect();
    let mut out = Vec::new();

    let (mut start, mut stop, mut call) = (Vec::new(), Vec::new(), Vec::new());
    let (mut spawn, mut hello, mut load_ms) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..runs.clamp(1, 50) {
        let t = Instant::now();
        plugin.column_values(&column, &paths[..1], &mut out).map_err(|e| e.to_string())?;
        start.push(ms(t));
        if let Some(s) = plugin.last_start() {
            spawn.push(s.spawn.as_secs_f64() * 1e3);
            hello.push(s.hello.as_secs_f64() * 1e3);
            load_ms.push(s.load.as_secs_f64() * 1e3);
        }
        let t = Instant::now();
        plugin.shutdown();
        stop.push(ms(t));
    }
    for _ in 0..runs.max(1) {
        out.clear();
        let t = Instant::now();
        plugin.column_values(&column, &paths, &mut out).map_err(|e| e.to_string())?;
        call.push(ms(t));
    }
    println!("{} {} column `{column}`, {} bytes of .cwasm", manifest.id, manifest.version, cwasm_len);
    row("start + load + first call", &mut start);
    row("  create process", &mut spawn);
    row("  process start -> hello", &mut hello);
    row("  load (hash, link, instantiate)", &mut load_ms);
    row("clean shutdown", &mut stop);
    row(&format!("column call, {} paths", paths.len()), &mut call);
    println!("{:?}", plugin.stats());
    Ok(())
}

fn mock(mode: &str) -> io::Result<()> {
    let (mut input, mut output) = (io::stdin().lock(), io::stdout().lock());
    if mode == "spawn" {
        // Containment probe: try to start a child, report the outcome as one line, then idle.
        let r = std::process::Command::new(std::env::current_exe()?).arg("mock").arg("silent").spawn();
        let line = match r {
            Ok(mut c) => {
                let _ = c.kill();
                "spawn-ok".to_string()
            }
            Err(e) => format!("spawn-err {e}"),
        };
        writeln!(output, "{line}")?;
        output.flush()?;
        std::thread::sleep(Duration::from_secs(600));
        return Ok(());
    }
    if mode == "env" {
        // Containment probe: the environment and folder the host was started with.
        let vars: Vec<String> = std::env::vars().map(|(k, _)| k.to_ascii_uppercase()).collect();
        writeln!(output, "env {} cwd {}", vars.join(","), std::env::current_dir()?.display())?;
        output.flush()?;
        std::thread::sleep(Duration::from_secs(600));
        return Ok(());
    }
    if mode == "silent" {
        std::thread::sleep(Duration::from_secs(600));
        return Ok(());
    }
    proto::write_frame(&mut output, &proto::enc_hello())?;
    let mut buf = Vec::new();
    while proto::read_frame(&mut input, &mut buf)? {
        match proto::decode_request(&buf) {
            Ok((id, Request::Load(l))) => {
                let info = Info { id: l.plugin_id, version: l.version, entrypoints: l.entrypoints };
                proto::write_frame(&mut output, &proto::enc_ok_info(id, &info).map_err(io::Error::other)?)?;
            }
            Ok((_, Request::Shutdown)) => break,
            _ if mode == "exit" => std::process::exit(3),
            // A call: never answered.
            _ => std::thread::sleep(Duration::from_secs(600)),
        }
    }
    output.flush()
}

/// Owner-death probe for the tests: start a mock host in a `JobLauncher`, print its pid, idle.
/// The test kills this process and checks that the host is gone with it.
#[cfg(windows)]
fn owner(host: &str, dir: &str) -> io::Result<()> {
    let launcher = tsubame_plughost::launcher::JobLauncher::new(host, dir, 64)?
        .with_elevation_check(|| false)
        .with_args(vec!["mock".into(), "hang".into()]);
    let (_io, pid) = launcher.launch_with_pid()?;
    println!("{pid}");
    io::stdout().flush()?;
    std::thread::sleep(Duration::from_secs(600));
    Ok(())
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let num = |flag: &str, default: usize| a.iter().position(|x| x == flag).and_then(|i| a.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default);
    let result = match (a.get(1).map(String::as_str), a.get(2), a.get(3), a.get(4)) {
        (Some("run"), Some(host), Some(dir), Some(cache)) => run(host, dir, cache, num("--runs", 300), num("--paths", 100)),
        (Some("mock"), Some(mode), _, _) => mock(mode).map_err(|e| e.to_string()),
        #[cfg(windows)]
        (Some("owner"), Some(host), Some(dir), _) => owner(host, dir).map_err(|e| e.to_string()),
        _ => Err("usage: plughost-bench run <host.exe> <plugin folder> <cache folder> [--runs N] [--paths N] | mock hang|silent|exit|spawn|env | owner <host.exe> <work dir>".into()),
    };
    if let Err(e) = result {
        eprintln!("plughost-bench: {e}");
        std::process::exit(1);
    }
}
