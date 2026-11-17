//! TEST ONLY (feature `test-server`): serve a release directory as a fake GitHub.
//!
//!   tsubame-update-testserver --dir <release dir> [--repo owner/name] [--port N]
//!
//! Prints `listening http://127.0.0.1:<port>` and runs until killed.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let Some(dir) = val("--dir") else {
        eprintln!("usage: tsubame-update-testserver --dir <release dir> [--repo owner/name] [--port N]");
        std::process::exit(2);
    };
    let repo = val("--repo").unwrap_or_else(|| tsubame_update::DEFAULT_REPO.to_string());
    let port: u16 = val("--port").and_then(|p| p.parse().ok()).unwrap_or(0);
    let server = match tsubame_update::testserver::start(dir.into(), &repo, port) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot start: {e}");
            std::process::exit(1);
        }
    };
    println!("listening {}", server.base);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
