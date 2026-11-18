//! Command line: `gen`, `gate-fixture`, `enum`, `verify`, `coldtest`, `purge`, `app`.

use std::collections::{HashMap, HashSet};

use crate::appbench;
use crate::bench::{self, Method, RunOptions};
use crate::cold::{self, ColdStatus};
use crate::disk;
use crate::fixture::{self, GenSpec};
use crate::gatefixture::{self, GateSpec};
use crate::load;
use crate::store::compare;
use crate::util::{normalize_dir, unextended};

const DEFAULT_SEED: u64 = 20_261_117;

struct Args {
    values: HashMap<String, String>,
    flags: HashSet<String>,
}

impl Args {
    fn parse(argv: &[String]) -> Result<Self, String> {
        let mut values = HashMap::new();
        let mut flags = HashSet::new();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            if let Some(rest) = a.strip_prefix("--") {
                if let Some((k, v)) = rest.split_once('=') {
                    values.insert(k.to_string(), v.to_string());
                } else if i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
                    values.insert(rest.to_string(), argv[i + 1].clone());
                    i += 1;
                } else {
                    flags.insert(rest.to_string());
                }
            } else {
                return Err(format!("unexpected argument {a:?}"));
            }
            i += 1;
        }
        Ok(Self { values, flags })
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    fn req(&self, key: &str) -> Result<&str, String> {
        self.get(key)
            .ok_or_else(|| format!("missing --{key}"))
    }

    fn num<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T, String> {
        match self.get(key) {
            None => Ok(default),
            Some(v) => v
                .parse()
                .map_err(|_| format!("--{key}: cannot parse {v:?}")),
        }
    }

    fn flag(&self, key: &str) -> bool {
        self.flags.contains(key)
    }
}

pub fn run() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first() else {
        print_help();
        return 2;
    };
    let rest = &argv[1..];
    let result = match cmd.as_str() {
        "gen" => cmd_gen(rest),
        "gate-fixture" => cmd_gate_fixture(rest),
        "enum" => cmd_enum(rest),
        "verify" => cmd_verify(rest),
        "coldtest" => cmd_coldtest(),
        "purge" => cmd_purge(rest),
        "app" => cmd_app(rest),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown command {other:?} (try: gen, gate-fixture, enum, verify, coldtest, purge, app)")),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("tsubame-bench: {e}");
            1
        }
    }
}

fn print_help() {
    println!(
        "\
tsubame-bench: fixture generators, directory-enumeration benchmark, bench log

  tsubame-bench gen --root DIR [--sizes 1k,100k,1m] [--seed N] [--threads N]
                    [--tiny BYTES] [--force] [--no-verify]
      Create DIR\\<size> folders of mixed-name files. Deterministic seed,
      resumable (an existing complete folder is skipped).

  tsubame-bench gate-fixture --root DIR [--tiny 190] [--small 9] [--big 1]
                    [--tiny-files 20] [--small-files 1000] [--big-files 10000]
                    [--seed N] [--threads N] [--force]
      Create DIR\\gate: the leak gate's tree of at least 200 folders of mixed
      sizes (tiny, 1000-file and 10000-file folders, plus .exe/.lnk files so
      the per-path icon cache is exercised). Resumable, marker-verified.

  tsubame-bench enum --dir DIR [--method find|nt64k|nt1m|gfhie|all] [--runs N]
                     [--cold] [--purge-mode standby|filecache|working]
                     [--buffer-kb N] [--settle-ms N] [--disk LABEL]
                     [--label TEXT] [--sort] [--csv] [--verbose]
      Measure time to the first 256 entries and time to complete listing.
      --sort also builds the sorted index after each run and reports
      complete+sort, which is what the PLAN budget means by complete-and-sorted.
      Warm = one discarded warm-up run first. Cold = flush caches
      before every run; --purge-mode picks the strength. The default, working,
      empties every working set on the machine and then the standby list, the
      only combination that produced real cold reads here; filecache (file cache
      only, no other process touched) is rejected by Windows 11. Needs an
      elevated token; the report says so when it is unavailable.

  tsubame-bench verify --dir DIR [--buffer-kb N]
      Enumerate once per method and compare names, attributes, size, mtime and
      file ids across all four methods.

  tsubame-bench coldtest
      Report whether the cold-cache purge is available on this token.

  tsubame-bench purge [--mode working|standby|filecache]
      Flush the system caches once (needs elevation) and say what happened.
      Exit 0 only when the purge really ran: tools/bench-rig.ps1 uses this to
      decide whether a cold-cache app run is possible.

  tsubame-bench app --reports DIR [--commit SHA] [--append FILE]
      Read one rig run's raw app reports (meta.txt, idle.txt,
      cold-start-NN.txt, timing.txt, leak-gate.txt) and render the
      docs/bench-log.md section: the budget table with pass/fail, the leak
      gate evidence and the raw output. --append writes it (creating the file
      with its header); without it the markdown goes to stdout.

Methods:
  find   FindFirstFileExW, FindExInfoBasic + FIND_FIRST_EX_LARGE_FETCH
  nt64k  NtQueryDirectoryFile, FileIdExtdDirectoryInformation, 64 KB buffer
  nt1m   NtQueryDirectoryFile, FileIdExtdDirectoryInformation, 1 MB buffer
  gfhie  GetFileInformationByHandleEx(FileIdExtdDirectoryInfo), 1 MB buffer
"
    );
}

fn cmd_gen(argv: &[String]) -> Result<(), String> {
    let args = Args::parse(argv)?;
    let root = normalize_dir(args.req("root")?);
    let sizes = fixture::parse_sizes(args.get("sizes").unwrap_or("1k,100k,1m"))?;
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
        .min(12);
    let spec = GenSpec {
        root: root.clone(),
        sizes,
        seed: args.num("seed", DEFAULT_SEED)?,
        threads: args.num("threads", cpus)?.max(1),
        tiny_bytes: args.num("tiny", 0usize)?,
        force: args.flag("force"),
        verify: !args.flag("no-verify"),
    };
    fixture::generate(&spec)?;
    println!("gen: done under {root}");
    Ok(())
}

fn cmd_enum(argv: &[String]) -> Result<(), String> {
    let args = Args::parse(argv)?;
    let dir = normalize_dir(args.req("dir")?);
    let runs: usize = args.num("runs", 5)?;
    let cold = args.flag("cold");
    let csv_only = args.flag("csv");
    let verbose = args.flag("verbose");
    let sort = args.flag("sort");
    let settle_ms: u64 = args.num("settle-ms", 250u64)?;
    let purge_mode = cold::PurgeMode::parse(args.get("purge-mode").unwrap_or("working"))
        .ok_or_else(|| "--purge-mode: unknown value (standby, filecache, working)".to_string())?;
    let buffer_override = match args.get("buffer-kb") {
        Some(v) => Some(
            v.parse::<usize>()
                .map_err(|_| format!("--buffer-kb: cannot parse {v:?}"))?
                * 1024,
        ),
        None => None,
    };

    let methods: Vec<Method> = match args.get("method").unwrap_or("all") {
        "all" => Method::ALL.to_vec(),
        other => vec![Method::parse(other)
            .ok_or_else(|| format!("unknown method {other:?} (find, nt64k, nt1m, gfhie, all)"))?],
    };

    let d = disk::detect(&dir);
    let disk_label = args.get("disk").unwrap_or(d.kind()).to_string();
    let label = args.get("label").unwrap_or("").to_string();

    if !csv_only {
        println!("tsubame-bench enum");
        println!("dir:        {}", unextended(&dir));
        println!(
            "disk:       {} (volume {}, {})",
            disk_label, d.volume, d.detail
        );
        println!(
            "cache:      {}",
            if cold {
                format!(
                    "cold (purge mode {}, settle {settle_ms} ms, before every run)",
                    purge_mode.name()
                )
            } else {
                "warm (one discarded warm-up run)".to_string()
            }
        );
        if cold {
            println!("cold check: {}", cold::elevation_note());
        }
        if let Some(busy) = load::cpu_busy(250) {
            println!("cpu busy:   {:.0}% over 250 ms before the run", busy * 100.0);
        }
        println!("runs:       {runs}");
        if sort {
            println!(
                "{:<8} {:>10} {:>11} {:>11} {:>13} {:>13} {:>9} {:>9} {:>11} {:>11} {:>12} {:>12}",
                "method",
                "entries",
                "first-med",
                "first-p95",
                "complete-med",
                "complete-p95",
                "min",
                "max",
                "sort-med",
                "sort-p95",
                "sorted-med",
                "sorted-p95"
            );
        } else {
            println!(
                "{:<8} {:>10} {:>11} {:>11} {:>13} {:>13} {:>9} {:>9}",
                "method",
                "entries",
                "first-med",
                "first-p95",
                "complete-med",
                "complete-p95",
                "min",
                "max"
            );
        }
    }

    let opts = RunOptions {
        runs,
        cold,
        purge_mode,
        buffer_override,
        settle_ms,
        sort,
        verbose,
    };

    let mut prev: Option<crate::store::EntryStore> = None;
    for method in methods {
        let r = bench::run(method, &dir, &opts)?;
        if !csv_only {
            if sort {
                println!(
                    "{:<8} {:>10} {:>11.3} {:>11.3} {:>13.3} {:>13.3} {:>9.3} {:>9.3} {:>11.3} {:>11.3} {:>12.3} {:>12.3}",
                    r.method.name(),
                    r.entries,
                    r.first.median(),
                    r.first.p95(),
                    r.complete.median(),
                    r.complete.p95(),
                    r.complete.min(),
                    r.complete.max(),
                    r.sort.median(),
                    r.sort.p95(),
                    r.complete.median() + r.sort.median(),
                    r.complete.p95() + r.sort.p95()
                );
            } else {
                println!(
                    "{:<8} {:>10} {:>11.3} {:>11.3} {:>13.3} {:>13.3} {:>9.3} {:>9.3}",
                    r.method.name(),
                    r.entries,
                    r.first.median(),
                    r.first.p95(),
                    r.complete.median(),
                    r.complete.p95(),
                    r.complete.min(),
                    r.complete.max()
                );
            }
            let mut extra = Vec::new();
            extra.push(format!("buffer {}", r.buffer_label));
            if let Some(l) = r.layout {
                extra.push(format!(
                    "layout {}",
                    match l {
                        crate::ntinfo::Layout::Extd => "FileIdExtd(88B hdr, file id)",
                        crate::ntinfo::Layout::Full => "FileFull(68B hdr, NO file id)",
                    }
                ));
            }
            if r.dots_skipped > 0 {
                extra.push(format!("{} dot entries dropped", r.dots_skipped));
            }
            if let Some(c) = &r.cold {
                extra.push(format!("purge: {}", c.label()));
            }
            println!("         {}", extra.join(", "));
        }
        println!(
            "RESULT,label={label},disk={disk_label},method={},entries={},cache={},runs={},buffer={},first_median={:.3},first_p95={:.3},complete_median={:.3},complete_p95={:.3},complete_min={:.3},complete_max={:.3},sort_median={:.3},sort_p95={:.3},sorted_median={:.3},sorted_p95={:.3}",
            r.method.name(),
            r.entries,
            if cold { "cold" } else { "warm" },
            runs,
            r.buffer_label,
            r.first.median(),
            r.first.p95(),
            r.complete.median(),
            r.complete.p95(),
            r.complete.min(),
            r.complete.max(),
            r.sort.median(),
            r.sort.p95(),
            r.complete.median() + r.sort.median(),
            r.complete.p95() + r.sort.p95()
        );

        if let (Some(p), Some(cur)) = (prev.as_ref(), r.store.as_ref()) {
            let rep = compare(p, cur);
            if rep.is_equal() {
                println!(
                    "         verify vs previous method: OK ({} entries, names+attrs+size+mtime identical, file ids compared on {})",
                    rep.a_len, rep.compared_ids
                );
            } else {
                println!(
                    "         verify vs previous method: MISMATCH: {}",
                    rep.first_mismatch.unwrap_or_default()
                );
                return Err(format!("method {} returned different data", r.method.name()));
            }
        }
        prev = r.store;
        if !csv_only {
            println!();
        }
    }
    Ok(())
}

fn cmd_verify(argv: &[String]) -> Result<(), String> {
    let args = Args::parse(argv)?;
    let dir = normalize_dir(args.req("dir")?);
    let buffer_override = match args.get("buffer-kb") {
        Some(v) => Some(
            v.parse::<usize>()
                .map_err(|_| format!("--buffer-kb: cannot parse {v:?}"))?
                * 1024,
        ),
        None => None,
    };
    let opts = RunOptions {
        runs: 1,
        cold: false,
        purge_mode: cold::PurgeMode::Standby,
        buffer_override,
        settle_ms: 0,
        sort: false,
        verbose: false,
    };
    println!("tsubame-bench verify: {}", unextended(&dir));
    let mut prev: Option<crate::store::EntryStore> = None;
    let mut prev_name = "";
    let mut bad = false;
    for method in Method::ALL {
        let r = bench::run(method, &dir, &opts)?;
        let Some(cur) = r.store else {
            return Err("no store".to_string());
        };
        println!(
            "{:<6} {:>9} entries, {:>10} bytes of names, {:.3} ms",
            r.method.name(),
            r.entries,
            r.name_bytes,
            r.complete.median()
        );
        if let Some(p) = prev.as_ref() {
            let rep = compare(p, &cur);
            if rep.is_equal() {
                println!(
                    "  {prev_name} vs {}: OK ({} names + metadata identical, {} file ids compared)",
                    r.method.name(),
                    rep.compared_metadata,
                    rep.compared_ids
                );
            } else {
                println!(
                    "  {prev_name} vs {}: MISMATCH: {}",
                    r.method.name(),
                    rep.first_mismatch.unwrap_or_default()
                );
                bad = true;
            }
        }
        prev = Some(cur);
        prev_name = r.method.name();
    }
    if bad {
        return Err("methods disagree".to_string());
    }
    println!("verify: all methods agree");
    Ok(())
}

fn cmd_gate_fixture(argv: &[String]) -> Result<(), String> {
    let args = Args::parse(argv)?;
    let root = normalize_dir(args.req("root")?);
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
        .min(12);
    let spec = GateSpec {
        root: root.clone(),
        tiny: args.num("tiny", 190usize)?,
        small: args.num("small", 9usize)?,
        big: args.num("big", 1usize)?,
        tiny_files: args.num("tiny-files", 20usize)?,
        small_files: args.num("small-files", 1000usize)?,
        big_files: args.num("big-files", 10_000usize)?,
        seed: args.num("seed", DEFAULT_SEED)?,
        threads: args.num("threads", cpus)?.max(1),
        force: args.flag("force"),
    };
    if spec.folders() < 200 {
        return Err(format!(
            "the gate needs 200 folders or more, this spec has {} (see docs/bench.md)",
            spec.folders()
        ));
    }
    gatefixture::generate(&spec)?;
    println!("gate-fixture: {} folders under {root}\\gate", spec.folders());
    Ok(())
}

fn cmd_purge(argv: &[String]) -> Result<(), String> {
    let args = Args::parse(argv)?;
    let mode = cold::PurgeMode::parse(args.get("mode").unwrap_or("working"))
        .ok_or_else(|| "--mode: unknown value (working, standby, filecache)".to_string())?;
    let status = cold::purge(mode);
    let mut text = String::new();
    match &status {
        ColdStatus::Purged => text.push_str(&format!("purge: {} ok\n", mode.name())),
        other => text.push_str(&format!("purge: {} {} ({})\n", mode.name(), other.label(), other.detail().unwrap_or(""))),
    }
    text.push_str(&format!("elevation: {}\n", cold::elevation_note()));
    print!("{text}");
    // an elevated run's stdout does not reach the ssh session: the harness
    // reads the file instead (`gsudo -n <exe> purge --out FILE`)
    if let Some(path) = args.get("out") {
        std::fs::write(path, &text).map_err(|e| format!("{path}: {e}"))?;
    }
    if status != ColdStatus::Purged {
        return Err(format!(
            "purge {} did not run: {}",
            mode.name(),
            status.detail().unwrap_or("not permitted on this token")
        ));
    }
    Ok(())
}

fn cmd_app(argv: &[String]) -> Result<(), String> {
    let args = Args::parse(argv)?;
    let dir = args.req("reports")?;
    let commit = args.get("commit").unwrap_or("");
    let append = args.get("append").map(std::path::PathBuf::from);
    let text = appbench::run(std::path::Path::new(dir), commit, append.as_deref())?;
    println!("{text}");
    if let Some(p) = append {
        println!("app: appended the section to {}", p.display());
    }
    Ok(())
}

fn cmd_coldtest() -> Result<(), String> {
    println!("elevation: {}", cold::elevation_note());
    // `working` is deliberately not exercised here: it empties every working
    // set on the machine, including the game servers rig runs.
    for mode in [cold::PurgeMode::Standby, cold::PurgeMode::FileCache] {
        let s = cold::purge(mode);
        println!("purge {:<10} {} ({:?})", mode.name(), s.label(), s.detail());
    }
    Ok(())
}
