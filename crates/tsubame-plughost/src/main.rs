//! tsubame-plughost: the wasm plugin host. A separate exe, started only when a wasm plugin is enabled.
//! Spike version: one `column` component, batch calls over stdin/stdout.
//!
//!   tsubame-plughost precompile <in.wasm> <out.cwasm>
//!   tsubame-plughost serve <file> [--pre]       framed batch calls on stdin/stdout, prints `ready ...` first
//!   tsubame-plughost bench <file> [--pre]       in-process call timing (no IPC)

use std::io::{self, Write};
use std::time::Instant;
use tsubame_plughost::proto;
use wasmtime::component::{Component, Linker};
#[cfg(feature = "compiler")]
use wasmtime::OptLevel;
use wasmtime::{Config, Engine, Result, Store};

wasmtime::component::bindgen!({ path: "wit", world: "column" });

struct Host {
    store: Store<()>,
    col: Column,
}

fn engine() -> Result<Engine> {
    #[allow(unused_mut)]
    let mut c = Config::new();
    #[cfg(feature = "compiler")]
    c.cranelift_opt_level(match std::env::var("PLUGHOST_OPT").as_deref() {
        Ok("none") => OptLevel::None,
        Ok("size") => OptLevel::SpeedAndSize,
        _ => OptLevel::Speed,
    });
    Engine::new(&c)
}

struct Timings {
    engine_us: u128,
    load_us: u128,
    inst_us: u128,
}

fn load(path: &str, pre: bool) -> Result<(Host, Timings)> {
    let t = Instant::now();
    let engine = engine()?;
    let engine_us = t.elapsed().as_micros();

    let t = Instant::now();
    let component = if pre {
        // SAFETY: the file is produced by `precompile` from this same exe/config.
        unsafe { Component::deserialize_file(&engine, path)? }
    } else {
        #[cfg(feature = "compiler")]
        {
            Component::from_file(&engine, path)?
        }
        #[cfg(not(feature = "compiler"))]
        {
            return Err(wasmtime::format_err!("runtime-only host: pass a precompiled .cwasm with --pre"));
        }
    };
    let load_us = t.elapsed().as_micros();

    let t = Instant::now();
    let mut linker = Linker::<()>::new(&engine);
    // The sample guest is std/wasip2 and may import wasi; no wasi is provided, so those trap if ever called.
    linker.define_unknown_imports_as_traps(&component)?;
    let mut store = Store::new(&engine, ());
    let col = Column::instantiate(&mut store, &component, &linker)?;
    let inst_us = t.elapsed().as_micros();
    Ok((Host { store, col }, Timings { engine_us, load_us, inst_us }))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let pre = args.iter().any(|a| a == "--pre");
    match args.get(1).map(String::as_str) {
        #[cfg(feature = "compiler")]
        Some("precompile") => {
            let wasm = std::fs::read(&args[2])?;
            let bytes = engine()?.precompile_component(&wasm)?;
            std::fs::write(&args[3], &bytes)?;
            println!("precompiled {} -> {} ({} bytes)", args[2], args[3], bytes.len());
        }
        Some("serve") => serve(&args[2], pre)?,
        Some("bench") => bench(&args[2], pre)?,
        _ => eprintln!("usage: tsubame-plughost precompile|serve|bench ..."),
    }
    Ok(())
}

fn serve(path: &str, pre: bool) -> Result<()> {
    let t0 = Instant::now();
    let (mut h, t) = load(path, pre)?;
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "ready total_us={} engine_us={} load_us={} inst_us={}",
        t0.elapsed().as_micros(),
        t.engine_us,
        t.load_us,
        t.inst_us
    )?;
    out.flush()?;
    let mut input = io::stdin().lock();
    let mut buf = Vec::new();
    while let Some(op) = proto::read_request(&mut input, &mut buf)? {
        match op {
            proto::OP_LIST => {
                let paths = proto::unpack(&buf);
                let vals = h.col.call_column_values(&mut h.store, &paths)?;
                proto::write_response(&mut out, &proto::pack(&vals))?;
            }
            proto::OP_PACKED => {
                let vals = h.col.call_column_values_packed(&mut h.store, &buf)?;
                proto::write_response(&mut out, &vals)?;
            }
            _ => break,
        }
    }
    Ok(())
}

fn stats(label: &str, mut us: Vec<f64>) {
    us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| us[((us.len() as f64 - 1.0) * q).round() as usize];
    println!(
        "{label:<28} n={:<6} min {:>9.1} us  median {:>9.1} us  p95 {:>9.1} us  max {:>9.1} us",
        us.len(),
        us[0],
        p(0.5),
        p(0.95),
        us[us.len() - 1]
    );
}

fn bench(path: &str, pre: bool) -> Result<()> {
    let (mut h, t) = load(path, pre)?;
    println!("load: engine {} us, component {} us, instantiate {} us", t.engine_us, t.load_us, t.inst_us);
    for &(n, runs) in &[(10_000usize, 300usize), (100, 3000), (1, 20000)] {
        let paths = proto::gen_paths(n);
        let packed = proto::pack(&paths);
        // correctness
        let want: Vec<String> = paths.iter().map(|p| proto::expected(p)).collect();
        assert_eq!(h.col.call_column_values(&mut h.store, &paths)?, want, "list variant wrong");
        assert_eq!(proto::unpack(&h.col.call_column_values_packed(&mut h.store, &packed)?), want, "packed variant wrong");
        for _ in 0..runs.min(50) {
            h.col.call_column_values(&mut h.store, &paths)?;
            h.col.call_column_values_packed(&mut h.store, &packed)?;
        }
        let mut a = Vec::with_capacity(runs);
        let mut b = Vec::with_capacity(runs);
        for _ in 0..runs {
            let t = Instant::now();
            h.col.call_column_values(&mut h.store, &paths)?;
            a.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            h.col.call_column_values_packed(&mut h.store, &packed)?;
            b.push(t.elapsed().as_secs_f64() * 1e6);
        }
        stats(&format!("list<string>    {n} paths"), a);
        stats(&format!("packed list<u8> {n} paths"), b);
    }
    Ok(())
}
