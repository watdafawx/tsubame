//! TEST ONLY (compiled under `cargo test` or the `test-server` feature): a tiny HTTP server
//! on 127.0.0.1 that fakes the two GitHub endpoints the updater uses, serving a flat
//! directory (the output of `tools\release.ps1`):
//!
//! * `GET /repos/<repo>/releases/latest`  -> `{"tag_name":"v<version from tsubame-update.json>",...}`
//!   with an ETag, and `304` on a matching `If-None-Match`
//! * `GET /repos/<repo>/releases/tags/nightly` -> `{"tag_name":"nightly","prerelease":true,...}` when
//!   `<dir>/tsubame-update-nightly.json` exists (the rolling nightly pre-release), ETag as above
//! * `GET /<repo>/releases/download/<tag>/<file>` -> `<dir>/<file>`
//!
//! Tampering with a release = editing a file in that directory.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct Server {
    /// `http://127.0.0.1:<port>`
    pub base: String,
    /// One line per request: `GET /path -> status`.
    pub hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    port: u16,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn hit_count(&self, needle: &str) -> usize {
        self.hits.lock().unwrap().iter().filter(|h| h.contains(needle)).count()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port)); // wake accept()
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// `port` 0 = pick a free one.
pub fn start(dir: PathBuf, repo: &str, port: u16) -> std::io::Result<Server> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let hits = Arc::new(Mutex::new(Vec::new()));
    let (s2, h2, repo) = (stop.clone(), hits.clone(), repo.to_string());
    let thread = std::thread::spawn(move || {
        for conn in listener.incoming() {
            if s2.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let (dir, repo, hits) = (dir.clone(), repo.clone(), h2.clone());
            std::thread::spawn(move || {
                let _ = handle(conn, &dir, &repo, &hits);
            });
        }
    });
    Ok(Server { base: format!("http://127.0.0.1:{port}"), hits, stop, port, thread: Some(thread) })
}

fn handle(mut c: TcpStream, dir: &std::path::Path, repo: &str, hits: &Mutex<Vec<String>>) -> std::io::Result<()> {
    c.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut head = Vec::new();
    let mut b = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
        let n = c.read(&mut b)?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&b[..n]);
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
    let inm = lines.filter_map(|l| l.split_once(':')).find(|(k, _)| k.eq_ignore_ascii_case("if-none-match")).map(|(_, v)| v.trim().to_string());

    let (status, etag, body) = route(dir, repo, &path, inm.as_deref());
    hits.lock().unwrap().push(format!("{} -> {status}", first.trim_end_matches(" HTTP/1.1")));
    let reason = match status {
        200 => "OK",
        304 => "Not Modified",
        _ => "Not Found",
    };
    let mut h = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n", body.len());
    if let Some(e) = etag {
        h.push_str(&format!("ETag: {e}\r\n"));
    }
    h.push_str("\r\n");
    c.write_all(h.as_bytes())?;
    if status == 200 {
        c.write_all(&body)?;
    }
    c.flush()
}

fn route(dir: &std::path::Path, repo: &str, path: &str, inm: Option<&str>) -> (u16, Option<String>, Vec<u8>) {
    let path = path.split('?').next().unwrap_or("");
    let nf = (404, None, Vec::new());
    if path == format!("/repos/{repo}/releases/latest") {
        let Ok(m) = std::fs::read(dir.join(crate::manifest::MANIFEST_FILE)) else { return nf };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&m) else { return nf };
        let Some(ver) = v.get("version").and_then(|x| x.as_str()) else { return nf };
        let body = format!("{{\"tag_name\":\"v{ver}\",\"draft\":false,\"prerelease\":false}}").into_bytes();
        let etag = format!("\"{}\"", crate::hash::sha256_hex(&m).unwrap_or_default().get(..16).unwrap_or(""));
        if inm == Some(etag.as_str()) {
            return (304, Some(etag), Vec::new());
        }
        return (200, Some(etag), body);
    }
    if path == format!("/repos/{repo}/releases/tags/{}", crate::channel::NIGHTLY_TAG) {
        let Ok(m) = std::fs::read(dir.join(crate::manifest::NIGHTLY_MANIFEST_FILE)) else { return nf };
        let body = format!("{{\"tag_name\":\"{}\",\"draft\":false,\"prerelease\":true}}", crate::channel::NIGHTLY_TAG).into_bytes();
        let etag = format!("\"n{}\"", crate::hash::sha256_hex(&m).unwrap_or_default().get(..16).unwrap_or(""));
        if inm == Some(etag.as_str()) {
            return (304, Some(etag), Vec::new());
        }
        return (200, Some(etag), body);
    }
    let prefix = format!("/{repo}/releases/download/");
    if let Some(rest) = path.strip_prefix(&prefix)
        && let Some((_tag, file)) = rest.split_once('/')
        && crate::manifest::is_safe_file_name(file)
        && let Ok(b) = std::fs::read(dir.join(file))
    {
        return (200, None, b);
    }
    nf
}
