//! HTTPS GET through WinHTTP (built into Windows: TLS, proxy, certificate store).
//! No TLS/HTTP crate in the exe. Blocking; run it on a worker thread.

use crate::error::{Error, Result};
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::*;

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn net(what: &str, e: windows::core::Error) -> Error {
    Error::Network(format!("{what}: {}", e.message().trim()))
}

fn last(what: &str) -> Error {
    net(what, windows::core::Error::from_win32())
}

#[derive(Debug, PartialEq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// path + query, always starting with `/`
    pub path: String,
}

pub fn parse_url(u: &str) -> Result<Url> {
    let bad = || Error::Config(format!("bad URL: {u}"));
    let (https, rest) = if let Some(r) = u.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = u.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(bad());
    };
    let (auth, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if auth.is_empty() || auth.contains('@') || u.bytes().any(|b| b <= b' ' || b == b'\\') {
        return Err(bad());
    }
    let (host, port) = match auth.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| bad())?),
        None => (auth, if https { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(bad());
    }
    Ok(Url { https, host: host.to_string(), port, path: path.to_string() })
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

#[derive(Clone, Copy)]
pub struct Opts {
    /// Plain http to loopback. Only ever true for test configs.
    pub allow_http_loopback: bool,
    /// Abort when the body would exceed this many bytes.
    pub max_bytes: u64,
}

#[derive(Debug, Default)]
pub struct Response {
    pub status: u16,
    pub etag: Option<String>,
    pub content_length: Option<u64>,
}

/// GET `url`. The body is streamed into `sink(chunk, total)`; the sink returns false to
/// cancel. Non-2xx bodies are not delivered (status is returned for the caller to judge).
pub fn get(url: &str, if_none_match: Option<&str>, o: Opts, sink: &mut dyn FnMut(&[u8], Option<u64>) -> bool) -> Result<Response> {
    let u = parse_url(url)?;
    if !u.https && !(o.allow_http_loopback && is_loopback(&u.host)) {
        return Err(Error::Config(format!("refusing non-https URL: {url}")));
    }
    unsafe {
        let agent = wide(concat!("Tsubame-Updater/", env!("CARGO_PKG_VERSION")));
        let access = if u.https { WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY } else { WINHTTP_ACCESS_TYPE_NO_PROXY };
        let session = Handle(WinHttpOpen(PCWSTR(agent.as_ptr()), access, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(last("WinHttpOpen"));
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 30_000).map_err(|e| net("WinHttpSetTimeouts", e))?;
        let host = wide(&u.host);
        let conn = Handle(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), u.port, 0));
        if conn.0.is_null() {
            return Err(last("WinHttpConnect"));
        }
        let (verb, path) = (wide("GET"), wide(&u.path));
        let flags = if u.https { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let req = Handle(WinHttpOpenRequest(conn.0, PCWSTR(verb.as_ptr()), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags));
        if req.0.is_null() {
            return Err(last("WinHttpOpenRequest"));
        }
        let mut headers = String::from("Accept: application/vnd.github+json, application/octet-stream\r\nX-GitHub-Api-Version: 2022-11-28\r\n");
        if let Some(e) = if_none_match.filter(|e| e.bytes().all(|b| b >= b' ' && b != 127 && b != b'\r' && b != b'\n')) {
            headers.push_str(&format!("If-None-Match: {e}\r\n"));
        }
        let headers = wide(&headers);
        WinHttpSendRequest(req.0, Some(&headers[..headers.len() - 1]), None, 0, 0, 0).map_err(|e| net("WinHttpSendRequest", e))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| net("WinHttpReceiveResponse", e))?;

        let mut code: u32 = 0;
        let mut len = 4u32;
        WinHttpQueryHeaders(req.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut code as *mut u32 as *mut c_void), &mut len, std::ptr::null_mut())
            .map_err(|e| net("status code", e))?;
        let mut resp = Response { status: code as u16, ..Default::default() };

        let mut buf = [0u16; 256];
        let mut blen = (buf.len() * 2) as u32;
        if WinHttpQueryHeaders(req.0, WINHTTP_QUERY_ETAG, PCWSTR::null(), Some(buf.as_mut_ptr() as *mut c_void), &mut blen, std::ptr::null_mut()).is_ok() {
            resp.etag = Some(String::from_utf16_lossy(&buf[..(blen as usize / 2).min(buf.len())]));
        }
        let mut clen: u32 = 0;
        let mut l = 4u32;
        if WinHttpQueryHeaders(req.0, WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut clen as *mut u32 as *mut c_void), &mut l, std::ptr::null_mut()).is_ok() {
            resp.content_length = Some(clen as u64);
        }

        if !(200..300).contains(&resp.status) {
            return Ok(resp);
        }
        if resp.content_length.is_some_and(|c| c > o.max_bytes) {
            return Err(Error::SizeMismatch { what: format!("response of {url}") });
        }
        let mut chunk = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let mut n = 0u32;
            WinHttpReadData(req.0, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u32, &mut n).map_err(|e| net("WinHttpReadData", e))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > o.max_bytes {
                return Err(Error::SizeMismatch { what: format!("response of {url}") });
            }
            if !sink(&chunk[..n as usize], resp.content_length) {
                return Err(Error::Cancelled);
            }
        }
        Ok(resp)
    }
}

/// GET into memory (small documents: API JSON, manifest, signature).
pub fn get_bytes(url: &str, if_none_match: Option<&str>, o: Opts) -> Result<(Response, Vec<u8>)> {
    let mut body = Vec::new();
    let r = get(url, if_none_match, o, &mut |c, _| {
        body.extend_from_slice(c);
        true
    })?;
    Ok((r, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(parse_url("https://api.github.com/repos/a/b/releases/latest").unwrap(), Url { https: true, host: "api.github.com".into(), port: 443, path: "/repos/a/b/releases/latest".into() });
        assert_eq!(parse_url("http://127.0.0.1:8080").unwrap(), Url { https: false, host: "127.0.0.1".into(), port: 8080, path: "/".into() });
        for bad in ["ftp://x/y", "https://", "https://user@host/x", "https://h/a b", "https://h:99999/x", "//x", "https://h\\x"] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn http_refused_unless_test_loopback() {
        let o = Opts { allow_http_loopback: false, max_bytes: 10 };
        assert!(matches!(get_bytes("http://127.0.0.1:1/x", None, o), Err(Error::Config(_))));
        let o = Opts { allow_http_loopback: true, max_bytes: 10 };
        // allowed only for loopback, even in test mode
        assert!(matches!(get_bytes("http://example.com/x", None, o), Err(Error::Config(_))));
    }
}
