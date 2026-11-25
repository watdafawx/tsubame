//! `tsubame-update`: command line front end of the updater library.
//!
//!   tsubame-update check    [--force]
//!   tsubame-update download
//!   tsubame-update apply    [--restart]
//!   tsubame-update rollback [--restart]
//!   tsubame-update status
//!
//! Common flags:
//!   --install-dir <dir>       folder holding tsubame.exe (default: folder of this exe)
//!   --current-version <x.y.z> version installed there (default: what the last run recorded)
//!   --channel stable|nightly  release channel to follow (default stable)
//!
//! Test builds (`--features test-server`, debug only) add `--data-dir`, `--api-base` and
//! `--repo` so the CLI can talk to `tsubame-update-testserver` instead of GitHub.
//!
//! Exit codes: 0 ok / nothing to do, 1 error, 3 update available (check only).

use std::path::PathBuf;
use tsubame_update::{Config, Layout, Updater};

struct Args {
    cmd: String,
    flags: Vec<String>,
}

impl Args {
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }
    fn val(&self, k: &str) -> Option<String> {
        self.flags.iter().position(|f| f == k).and_then(|i| self.flags.get(i + 1)).cloned()
    }
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn build(a: &Args) -> Updater {
    let install_dir: PathBuf = match a.val("--install-dir") {
        Some(d) => d.into(),
        None => std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| die("cannot find own folder")),
    };
    let layout = match test_data_dir(a) {
        Some(data) => Layout::new(&install_dir, data),
        None => Layout::for_install_dir(&install_dir).unwrap_or_else(|e| die(e)),
    };
    let current = a
        .val("--current-version")
        .or_else(|| tsubame_update::state::State::load(&layout).installed_version)
        .unwrap_or_else(|| die("unknown installed version: pass --current-version <x.y.z> once"));
    let channel = match a.val("--channel") {
        Some(c) => tsubame_update::Channel::parse(&c).unwrap_or_else(|| die(format!("unknown channel {c:?} (stable or nightly)"))),
        None => tsubame_update::Channel::Stable,
    };
    let cfg = make_config(a, &current, layout).unwrap_or_else(|e| die(e)).with_channel(channel);
    Updater::new(cfg)
}

#[cfg(feature = "test-server")]
fn test_data_dir(a: &Args) -> Option<PathBuf> {
    a.val("--data-dir").map(PathBuf::from)
}
#[cfg(not(feature = "test-server"))]
fn test_data_dir(_: &Args) -> Option<PathBuf> {
    None
}

#[cfg(feature = "test-server")]
fn make_config(a: &Args, current: &str, layout: Layout) -> tsubame_update::Result<Config> {
    match a.val("--api-base") {
        Some(base) => {
            let repo = a.val("--repo").unwrap_or_else(|| tsubame_update::DEFAULT_REPO.to_string());
            Config::for_test(current, layout, &base, &repo)
        }
        None => Config::production(current, layout),
    }
}
#[cfg(not(feature = "test-server"))]
fn make_config(_: &Args, current: &str, layout: Layout) -> tsubame_update::Result<Config> {
    Config::production(current, layout)
}

fn main() {
    let mut it = std::env::args().skip(1);
    let Some(cmd) = it.next() else {
        eprintln!("usage: tsubame-update <check [--force]|download|apply [--restart]|rollback [--restart]|status> [--install-dir D] [--current-version V] [--channel stable|nightly]");
        std::process::exit(2);
    };
    let a = Args { cmd, flags: it.collect() };
    let up = build(&a);
    match a.cmd.as_str() {
        "check" => match up.check(a.has("--force")) {
            Ok(Some(r)) => {
                println!("update available: {} -> {}", up.config().current_version(), r.version);
                println!("  channel:  {} (following {})", r.channel, up.config().channel());
                println!("  manifest: {}", up.manifest_url(r.channel, &r.tag));
                println!("  released: {}", r.released);
                println!("  notes:    {}", r.notes_url);
                println!("  download: {} ({} bytes)", r.download_file, r.download_size);
                if !r.in_place_ok {
                    println!("  in-place update refused: installed version is older than {}; run the new installer", r.min_supported_version);
                }
                std::process::exit(3);
            }
            Ok(None) => println!("up to date ({}, {} channel)", up.config().current_version(), up.config().channel()),
            Err(e) => die(e),
        },
        "download" => {
            let rel = match up.check(false) {
                Ok(Some(r)) => r,
                Ok(None) => die("no update to download (run `check --force` first?)"),
                Err(e) => die(e),
            };
            let mut last_pct = u64::MAX;
            let staged = up
                .download(&rel, &mut |p| {
                    let pct = (p.done * 100).checked_div(p.total).unwrap_or(100);
                    if pct / 25 != last_pct / 25 {
                        println!("  downloaded {pct}%");
                        last_pct = pct;
                    }
                    true
                })
                .unwrap_or_else(|e| die(e));
            println!("staged {} in {}", staged.version, staged.dir.display());
            for f in &staged.files {
                println!("  {} {} bytes sha256 {}", f.name, f.size, f.sha256);
            }
        }
        "apply" => {
            let Some(staged) = up.staged() else { die("nothing staged: run `download` first") };
            let r = up.apply(&staged).unwrap_or_else(|e| die(e));
            println!("applied {} (was {}); replaced: {}", r.version, r.previous_version, r.replaced.join(", "));
            println!("previous version kept for `rollback`");
            if a.has("--restart") {
                match up.relaunch_watched(&[], std::time::Duration::from_secs(30)) {
                    Ok(o) => println!("restart: {o:?}"),
                    Err(e) => die(e),
                }
            }
        }
        "rollback" => {
            let r = up.rollback().unwrap_or_else(|e| die(e));
            println!("rolled back {} -> {}; restored: {}", r.from, r.to, r.restored.join(", "));
            if a.has("--restart")
                && let Err(e) = up.relaunch(&[])
            {
                die(e);
            }
        }
        "status" => {
            let s = up.status();
            println!("running:  {}", s.running);
            println!("previous: {}", s.previous.as_deref().unwrap_or("none"));
            println!("last check (unix): {}", s.last_check_unix);
            println!("staged:   {}", s.staged.as_ref().map(|s| s.version.as_str()).unwrap_or("none"));
        }
        other => die(format!("unknown command {other:?}")),
    }
}
