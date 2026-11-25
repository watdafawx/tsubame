//! The wire format between the app (client) and `tsubame-plughost.exe` over
//! the host's stdin and stdout. See `docs/wasm-plugins.md`.
//!
//! Every message is one frame: `[len u32][id u32][kind u8][body]`, little
//! endian, `len` counting everything after itself. A request carries an id the
//! client picks; the reply carries the same id. Strings are `[len u32][utf8]`,
//! lists are `[count u32][items]`.
//!
//! Decoding never trusts a length: every read is bounds checked, counts are
//! checked against the bytes that are left, and a frame over [`MAX_FRAME`] is
//! refused before it is allocated.

use crate::caps::{Access, Capabilities, PathGrant, Scope};
use crate::manifest::Kind;
use std::io::{self, Read, Write};
use tsubame_ext_api::{Cell, Hit, Limits, Node, VfsEntry};

pub const MAGIC: [u8; 4] = *b"TSPH";
/// Bumped on any change to the frames below.
pub const VERSION: u16 = 2;
/// Largest frame either side accepts (the bytes after the length prefix).
pub const MAX_FRAME: usize = 16 << 20;
/// Most paths in one call (`docs/spike-wasm.md`: caps the guest's memory high-water mark).
pub const MAX_BATCH: usize = 2_000;
/// Most path bytes in one call.
pub const MAX_BATCH_BYTES: usize = 1 << 20;
/// Bytes of a frame before its body: id and kind.
pub const HEADER: usize = 5;
/// Error messages are cut to this many bytes.
pub const MAX_MESSAGE: usize = 1024;

/// Request kinds.
pub mod op {
    pub const LOAD: u8 = 1;
    pub const COLUMN: u8 = 2;
    pub const PREVIEW: u8 = 3;
    pub const VFS_LIST: u8 = 4;
    pub const VFS_READ: u8 = 5;
    pub const SEARCH: u8 = 6;
    pub const COMMAND: u8 = 7;
    pub const SHUTDOWN: u8 = 8;
}

/// Reply kinds.
pub mod reply {
    /// Sent once by the host when it starts, with id 0.
    pub const HELLO: u8 = 0x80;
    pub const OK: u8 = 0x81;
    pub const ERR: u8 = 0x82;
}

/// Why a call failed, as the host reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrKind {
    /// The request could not be decoded or breaks a limit.
    BadRequest = 1,
    /// A call arrived before a successful `Load`.
    NotLoaded = 2,
    /// The component could not be loaded (hash, format, missing export, wrong identity).
    Load = 3,
    /// The plugin returned an error of its own. Not a fault.
    Plugin = 4,
    /// The guest trapped.
    Trap = 5,
    /// The call ran past its deadline and was interrupted.
    Timeout = 6,
    /// The guest asked for more memory than it is allowed.
    Memory = 7,
    /// The plugin's answer breaks the contract (wrong count, too large).
    BadResult = 8,
    /// The plugin has no entry point of the kind that was called.
    Unsupported = 9,
}

impl ErrKind {
    pub fn from_u8(v: u8) -> Option<ErrKind> {
        use ErrKind::*;
        [BadRequest, NotLoaded, Load, Plugin, Trap, Timeout, Memory, BadResult, Unsupported].into_iter().find(|k| *k as u8 == v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtoError(pub &'static str);

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "protocol error: {}", self.0)
    }
}

impl std::error::Error for ProtoError {}

// ---- frames ------------------------------------------------------------------------------

/// Reads one frame (without its length prefix) into `buf`. `Ok(false)` on a
/// clean end of stream between frames.
pub fn read_frame<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<bool> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    if !(HEADER..=MAX_FRAME).contains(&len) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame length out of range"));
    }
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)?;
    Ok(true)
}

/// Writes one frame built by [`Enc`] (it already holds its length prefix).
pub fn write_frame<W: Write>(w: &mut W, frame: &[u8]) -> io::Result<()> {
    w.write_all(frame)?;
    w.flush()
}

/// Builds one frame.
pub struct Enc {
    buf: Vec<u8>,
}

impl Enc {
    pub fn new(id: u32, kind: u8) -> Enc {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&[0; 4]);
        buf.extend_from_slice(&id.to_le_bytes());
        buf.push(kind);
        Enc { buf }
    }
    pub fn u8(&mut self, v: u8) -> &mut Enc {
        self.buf.push(v);
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Enc {
        self.u8(v as u8)
    }
    pub fn u16(&mut self, v: u16) -> &mut Enc {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Enc {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Enc {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Enc {
        self.u32(v.len().min(u32::MAX as usize) as u32);
        self.buf.extend_from_slice(v);
        self
    }
    pub fn str(&mut self, v: &str) -> &mut Enc {
        self.bytes(v.as_bytes())
    }
    pub fn opt_str(&mut self, v: Option<&str>) -> &mut Enc {
        self.bool(v.is_some());
        if let Some(s) = v {
            self.str(s);
        }
        self
    }
    /// The finished frame, length prefix included. Fails when it is over [`MAX_FRAME`].
    pub fn finish(mut self) -> Result<Vec<u8>, ProtoError> {
        let len = self.buf.len() - 4;
        if len > MAX_FRAME {
            return Err(ProtoError("frame too large"));
        }
        self.buf[..4].copy_from_slice(&(len as u32).to_le_bytes());
        Ok(self.buf)
    }
}

/// Reads the body of one frame.
#[derive(Clone)]
pub struct Dec<'a> {
    buf: &'a [u8],
}

impl<'a> Dec<'a> {
    pub fn new(buf: &'a [u8]) -> Dec<'a> {
        Dec { buf }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtoError> {
        if n > self.buf.len() {
            return Err(ProtoError("truncated"));
        }
        let (a, b) = self.buf.split_at(n);
        self.buf = b;
        Ok(a)
    }
    pub fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> Result<bool, ProtoError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtoError("bad bool")),
        }
    }
    pub fn u16(&mut self) -> Result<u16, ProtoError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, ProtoError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, ProtoError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn bytes(&mut self) -> Result<&'a [u8], ProtoError> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    pub fn str(&mut self) -> Result<&'a str, ProtoError> {
        std::str::from_utf8(self.bytes()?).map_err(|_| ProtoError("bad utf-8"))
    }
    pub fn string(&mut self) -> Result<String, ProtoError> {
        self.str().map(str::to_string)
    }
    pub fn opt_string(&mut self) -> Result<Option<String>, ProtoError> {
        if self.bool()? { self.string().map(Some) } else { Ok(None) }
    }
    /// A list count. Every item takes at least `min_item` bytes, so a count the
    /// rest of the frame can not hold is refused before anything is allocated.
    pub fn count(&mut self, min_item: usize) -> Result<usize, ProtoError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_item.max(1)) > self.buf.len() {
            return Err(ProtoError("count larger than the frame"));
        }
        Ok(n)
    }
    /// Errors when bytes are left over.
    pub fn end(&self) -> Result<(), ProtoError> {
        if self.buf.is_empty() { Ok(()) } else { Err(ProtoError("trailing bytes")) }
    }
}

/// Splits a frame into id, kind and body.
pub fn split(frame: &[u8]) -> Result<(u32, u8, Dec<'_>), ProtoError> {
    if frame.len() < HEADER {
        return Err(ProtoError("short frame"));
    }
    Ok((u32::from_le_bytes(frame[..4].try_into().unwrap()), frame[4], Dec::new(&frame[HEADER..])))
}

// ---- requests ------------------------------------------------------------------------------

/// The first request on a connection: which component to run and under which limits.
#[derive(Clone, Debug, PartialEq)]
pub struct Load {
    /// Must match what the component reports.
    pub plugin_id: String,
    pub version: String,
    /// Path of the `.cwasm` the install step wrote.
    pub component: String,
    /// SHA-256 of that file, recorded by the install step.
    pub sha256: [u8; 32],
    pub caps: Capabilities,
    /// The entry points the manifest declares.
    pub entrypoints: Vec<(Kind, String)>,
    /// Folders the plugin may never write, whatever it was granted (security
    /// review PH-02). The host also refuses every access (read too) under the
    /// component's own folder and under any of these that holds the component
    /// (the cache). Since protocol version 2.
    pub protected: Vec<String>,
}

/// Most protected folders one `Load` may name.
pub const MAX_PROTECTED: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Load(Load),
    Column { column: String, paths: Vec<String> },
    Preview { id: String, path: String, limits: Limits },
    VfsList { id: String, path: String, cursor: Option<String> },
    VfsRead { id: String, path: String, offset: u64, max_len: u32 },
    Search { id: String, text: String, root: String, limit: u32, cursor: Option<String> },
    Command { name: String, folder: String, selection: Vec<String>, part: u32, parts: u32 },
    Shutdown,
}

fn enc_caps(e: &mut Enc, c: &Capabilities) {
    e.bool(c.network).bool(c.run_processes).u32(c.max_memory_mb).u32(c.timeout_ms).u32(c.paths.len() as u32);
    for g in &c.paths {
        match &g.scope {
            Scope::Prefix(p) => e.u8(0).str(p),
            Scope::Input => e.u8(1),
            Scope::Any => e.u8(2),
        };
        e.u8(g.access as u8);
    }
}

fn dec_caps(d: &mut Dec) -> Result<Capabilities, ProtoError> {
    let (network, run_processes, max_memory_mb, timeout_ms) = (d.bool()?, d.bool()?, d.u32()?, d.u32()?);
    let n = d.count(2)?;
    let mut paths = Vec::with_capacity(n);
    for _ in 0..n {
        let scope = match d.u8()? {
            0 => Scope::Prefix(d.string()?),
            1 => Scope::Input,
            2 => Scope::Any,
            _ => return Err(ProtoError("bad scope")),
        };
        let access = match d.u8()? {
            0 => Access::Read,
            1 => Access::ReadWrite,
            _ => return Err(ProtoError("bad access")),
        };
        paths.push(PathGrant { scope, access });
    }
    Ok(Capabilities { network, run_processes, max_memory_mb, timeout_ms, paths })
}

fn enc_entrypoints(e: &mut Enc, eps: &[(Kind, String)]) {
    e.u32(eps.len() as u32);
    for (k, id) in eps {
        e.u8(*k as u8).str(id);
    }
}

fn dec_entrypoints(d: &mut Dec) -> Result<Vec<(Kind, String)>, ProtoError> {
    let n = d.count(5)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let k = Kind::from_u8(d.u8()?).ok_or(ProtoError("bad entry point kind"))?;
        out.push((k, d.string()?));
    }
    Ok(out)
}

fn enc_paths(e: &mut Enc, paths: &[&str]) {
    e.u32(paths.len() as u32);
    for p in paths {
        e.str(p);
    }
}

fn dec_paths(d: &mut Dec) -> Result<Vec<String>, ProtoError> {
    let n = d.count(4)?;
    if n > MAX_BATCH {
        return Err(ProtoError("batch larger than 2,000 paths"));
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(d.string()?);
    }
    Ok(out)
}

pub fn enc_load(id: u32, l: &Load) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, op::LOAD);
    e.str(&l.plugin_id).str(&l.version).str(&l.component);
    e.buf.extend_from_slice(&l.sha256);
    enc_caps(&mut e, &l.caps);
    enc_entrypoints(&mut e, &l.entrypoints);
    e.u32(l.protected.len() as u32);
    for p in &l.protected {
        e.str(p);
    }
    e.finish()
}

pub fn enc_column(id: u32, column: &str, paths: &[&str]) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, op::COLUMN);
    e.str(column);
    enc_paths(&mut e, paths);
    e.finish()
}

pub fn enc_preview(id: u32, previewer: &str, path: &str, limits: Limits) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, op::PREVIEW);
    e.str(previewer).str(path).u64(limits.max_bytes).u32(limits.max_px);
    e.finish()
}

pub fn enc_vfs_list(id: u32, vfs: &str, path: &str, cursor: Option<&str>) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, op::VFS_LIST);
    e.str(vfs).str(path).opt_str(cursor);
    e.finish()
}

pub fn enc_vfs_read(id: u32, vfs: &str, path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, op::VFS_READ);
    e.str(vfs).str(path).u64(offset).u32(max_len);
    e.finish()
}

pub fn enc_search(id: u32, provider: &str, text: &str, root: &str, limit: u32, cursor: Option<&str>) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, op::SEARCH);
    e.str(provider).str(text).str(root).u32(limit).opt_str(cursor);
    e.finish()
}

pub fn enc_command(id: u32, name: &str, folder: &str, selection: &[&str], part: u32, parts: u32) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, op::COMMAND);
    e.str(name).str(folder);
    enc_paths(&mut e, selection);
    e.u32(part).u32(parts);
    e.finish()
}

pub fn enc_shutdown(id: u32) -> Vec<u8> {
    Enc::new(id, op::SHUTDOWN).finish().expect("small frame")
}

/// Decodes a request frame (the host's side).
pub fn decode_request(frame: &[u8]) -> Result<(u32, Request), ProtoError> {
    let (id, kind, mut d) = split(frame)?;
    let req = match kind {
        op::LOAD => {
            let (plugin_id, version, component) = (d.string()?, d.string()?, d.string()?);
            let sha256: [u8; 32] = d.take(32)?.try_into().unwrap();
            let caps = dec_caps(&mut d)?;
            let entrypoints = dec_entrypoints(&mut d)?;
            let n = d.count(4)?;
            if n > MAX_PROTECTED {
                return Err(ProtoError("too many protected folders"));
            }
            let mut protected = Vec::with_capacity(n);
            for _ in 0..n {
                protected.push(d.string()?);
            }
            Request::Load(Load { plugin_id, version, component, sha256, caps, entrypoints, protected })
        }
        op::COLUMN => Request::Column { column: d.string()?, paths: dec_paths(&mut d)? },
        op::PREVIEW => Request::Preview { id: d.string()?, path: d.string()?, limits: Limits { max_bytes: d.u64()?, max_px: d.u32()? } },
        op::VFS_LIST => Request::VfsList { id: d.string()?, path: d.string()?, cursor: d.opt_string()? },
        op::VFS_READ => Request::VfsRead { id: d.string()?, path: d.string()?, offset: d.u64()?, max_len: d.u32()? },
        op::SEARCH => Request::Search { id: d.string()?, text: d.string()?, root: d.string()?, limit: d.u32()?, cursor: d.opt_string()? },
        op::COMMAND => Request::Command { name: d.string()?, folder: d.string()?, selection: dec_paths(&mut d)?, part: d.u32()?, parts: d.u32()? },
        op::SHUTDOWN => Request::Shutdown,
        _ => return Err(ProtoError("unknown request")),
    };
    d.end()?;
    Ok((id, req))
}

// ---- replies -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub protocol: u16,
    pub host_version: String,
}

/// What the component says about itself (`meta.describe`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub id: String,
    pub version: String,
    pub entrypoints: Vec<(Kind, String)>,
}

pub fn enc_hello() -> Vec<u8> {
    let mut e = Enc::new(0, reply::HELLO);
    e.buf.extend_from_slice(&MAGIC);
    e.u16(VERSION).str(env!("CARGO_PKG_VERSION"));
    e.finish().expect("small frame")
}

pub fn decode_hello(frame: &[u8]) -> Result<Hello, ProtoError> {
    let (id, kind, mut d) = split(frame)?;
    if id != 0 || kind != reply::HELLO || d.take(4)? != MAGIC {
        return Err(ProtoError("not a plugin host"));
    }
    let h = Hello { protocol: d.u16()?, host_version: d.string()? };
    d.end()?;
    Ok(h)
}

fn cut(msg: &str) -> &str {
    let mut n = msg.len().min(MAX_MESSAGE);
    while !msg.is_char_boundary(n) {
        n -= 1;
    }
    &msg[..n]
}

pub fn enc_err(id: u32, kind: ErrKind, msg: &str) -> Vec<u8> {
    let mut e = Enc::new(id, reply::ERR);
    e.u8(kind as u8).str(cut(msg));
    e.finish().expect("small frame")
}

pub fn enc_ok_empty(id: u32) -> Vec<u8> {
    Enc::new(id, reply::OK).finish().expect("small frame")
}

pub fn enc_ok_info(id: u32, info: &Info) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, reply::OK);
    e.str(&info.id).str(&info.version);
    enc_entrypoints(&mut e, &info.entrypoints);
    e.finish()
}

pub fn dec_info(mut d: Dec) -> Result<Info, ProtoError> {
    let info = Info { id: d.string()?, version: d.string()?, entrypoints: dec_entrypoints(&mut d)? };
    d.end()?;
    Ok(info)
}

pub fn enc_ok_cells(id: u32, cells: &[Cell]) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, reply::OK);
    e.u32(cells.len() as u32);
    for c in cells {
        match c {
            Cell::Empty => e.u8(0),
            Cell::Text(s) => e.u8(1).str(s),
            Cell::Int(v) => e.u8(2).u64(*v as u64),
            Cell::Bytes(v) => e.u8(3).u64(*v),
            Cell::Time(v) => e.u8(4).u64(*v),
        };
    }
    e.finish()
}

/// Appends the cells to `out`.
pub fn dec_cells(mut d: Dec, out: &mut Vec<Cell>) -> Result<usize, ProtoError> {
    let n = d.count(1)?;
    out.reserve(n);
    for _ in 0..n {
        out.push(match d.u8()? {
            0 => Cell::Empty,
            1 => Cell::Text(d.string()?),
            2 => Cell::Int(d.u64()? as i64),
            3 => Cell::Bytes(d.u64()?),
            4 => Cell::Time(d.u64()?),
            _ => return Err(ProtoError("bad cell")),
        });
    }
    d.end()?;
    Ok(n)
}

pub fn enc_ok_doc(id: u32, nodes: &[Node]) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, reply::OK);
    e.u32(nodes.len() as u32);
    for n in nodes {
        match n {
            Node::Heading(s) => e.u8(0).str(s),
            Node::Text { text, dim: false } => e.u8(1).str(text),
            Node::Text { text, dim: true } => e.u8(2).str(text),
            Node::Mono(s) => e.u8(3).str(s),
            Node::Pair(a, b) => e.u8(4).str(a).str(b),
            Node::Image { w, h, bgra } => e.u8(5).u32(*w).u32(*h).bytes(bgra),
            Node::Button { label, command } => e.u8(6).str(label).str(command),
            Node::Indent => e.u8(7),
            Node::Outdent => e.u8(8),
        };
    }
    e.finish()
}

/// True when an image's pixel buffer has exactly `w * h * 4` bytes.
pub fn image_ok(w: u32, h: u32, len: usize) -> bool {
    (w as u64).checked_mul(h as u64).and_then(|p| p.checked_mul(4)) == Some(len as u64)
}

/// Appends the nodes to `out`.
pub fn dec_doc(mut d: Dec, out: &mut Vec<Node>) -> Result<(), ProtoError> {
    let n = d.count(1)?;
    out.reserve(n);
    for _ in 0..n {
        out.push(match d.u8()? {
            0 => Node::Heading(d.string()?),
            1 => Node::Text { text: d.string()?, dim: false },
            2 => Node::Text { text: d.string()?, dim: true },
            3 => Node::Mono(d.string()?),
            4 => Node::Pair(d.string()?, d.string()?),
            5 => {
                let (w, h, bgra) = (d.u32()?, d.u32()?, d.bytes()?);
                if !image_ok(w, h, bgra.len()) {
                    return Err(ProtoError("image size does not match its pixels"));
                }
                Node::Image { w, h, bgra: bgra.to_vec() }
            }
            6 => Node::Button { label: d.string()?, command: d.string()?.into() },
            7 => Node::Indent,
            8 => Node::Outdent,
            _ => return Err(ProtoError("bad node")),
        });
    }
    d.end()
}

pub fn enc_ok_entries(id: u32, entries: &[VfsEntry], next: Option<&str>) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, reply::OK);
    e.u32(entries.len() as u32);
    for v in entries {
        e.str(&v.name).bool(v.dir).u64(v.size).u64(v.mtime);
    }
    e.opt_str(next);
    e.finish()
}

pub fn dec_entries(mut d: Dec) -> Result<(Vec<VfsEntry>, Option<String>), ProtoError> {
    let n = d.count(21)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(VfsEntry { name: d.string()?, dir: d.bool()?, size: d.u64()?, mtime: d.u64()? });
    }
    let next = d.opt_string()?;
    d.end()?;
    Ok((out, next))
}

pub fn enc_ok_hits(id: u32, hits: &[Hit], next: Option<&str>) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, reply::OK);
    e.u32(hits.len() as u32);
    for v in hits {
        e.str(&v.path).bool(v.dir).u64(v.size).u64(v.mtime);
    }
    e.opt_str(next);
    e.finish()
}

pub fn dec_hits(mut d: Dec) -> Result<(Vec<Hit>, Option<String>), ProtoError> {
    let n = d.count(21)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(Hit { path: d.string()?, dir: d.bool()?, size: d.u64()?, mtime: d.u64()? });
    }
    let next = d.opt_string()?;
    d.end()?;
    Ok((out, next))
}

pub fn enc_ok_bytes(id: u32, data: &[u8]) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, reply::OK);
    e.bytes(data);
    e.finish()
}

/// Appends the bytes to `out`.
pub fn dec_bytes(mut d: Dec, out: &mut Vec<u8>) -> Result<usize, ProtoError> {
    let b = d.bytes()?;
    d.end()?;
    out.extend_from_slice(b);
    Ok(b.len())
}

pub fn enc_ok_message(id: u32, msg: Option<&str>) -> Result<Vec<u8>, ProtoError> {
    let mut e = Enc::new(id, reply::OK);
    e.opt_str(msg);
    e.finish()
}

pub fn dec_message(mut d: Dec) -> Result<Option<String>, ProtoError> {
    let m = d.opt_string()?;
    d.end()?;
    Ok(m)
}

/// A decoded reply: the request's id, then the body or the host's error.
pub struct Reply<'a> {
    pub id: u32,
    pub body: Result<Dec<'a>, (ErrKind, String)>,
}

pub fn decode_reply(frame: &[u8]) -> Result<Reply<'_>, ProtoError> {
    let (id, kind, mut d) = split(frame)?;
    match kind {
        reply::OK => Ok(Reply { id, body: Ok(d) }),
        reply::ERR => {
            let k = ErrKind::from_u8(d.u8()?).ok_or(ProtoError("bad error kind"))?;
            let msg = d.string()?;
            d.end()?;
            Ok(Reply { id, body: Err((k, msg)) })
        }
        _ => Err(ProtoError("unknown reply")),
    }
}

// ---- batches --------------------------------------------------------------------------------

/// Splits `paths` into calls of at most [`MAX_BATCH`] paths and about
/// [`MAX_BATCH_BYTES`] bytes. A single path longer than that goes alone.
pub fn chunks<'a, 'b>(paths: &'a [&'b str]) -> impl Iterator<Item = &'a [&'b str]> {
    let mut rest = paths;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (mut n, mut bytes) = (0, 0);
        while n < rest.len() && n < MAX_BATCH {
            bytes += rest[n].len();
            if n > 0 && bytes > MAX_BATCH_BYTES {
                break;
            }
            n += 1;
        }
        let (head, tail) = rest.split_at(n);
        rest = tail;
        Some(head)
    })
}

/// How many calls [`chunks`] makes for `paths`.
pub fn chunk_count(paths: &[&str]) -> usize {
    chunks(paths).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_body(frame: &[u8]) -> &[u8] {
        assert_eq!(u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize, frame.len() - 4);
        &frame[4..]
    }

    fn round(frame: Vec<u8>) -> (u32, Request) {
        decode_request(frame_body(&frame)).unwrap()
    }

    #[test]
    fn requests_round_trip() {
        let load = Load {
            plugin_id: "com.example.x".into(),
            version: "1.0.0".into(),
            component: "C:\\cache\\x.cwasm".into(),
            sha256: [7; 32],
            caps: Capabilities {
                network: true,
                run_processes: false,
                max_memory_mb: 32,
                timeout_ms: 500,
                paths: vec![
                    PathGrant { scope: Scope::Prefix("C:\\Data".into()), access: Access::ReadWrite },
                    PathGrant { scope: Scope::Input, access: Access::Read },
                    PathGrant { scope: Scope::Any, access: Access::Read },
                ],
            },
            entrypoints: vec![(Kind::Column, "a".into()), (Kind::Command, "b".into())],
            protected: vec!["C:\\Apps\\Tsubame".into(), "C:\\cache".into()],
        };
        assert_eq!(round(enc_load(1, &load).unwrap()), (1, Request::Load(load)));
        assert_eq!(
            round(enc_column(2, "col", &["C:\\a", "C:\\ü\\b"]).unwrap()),
            (2, Request::Column { column: "col".into(), paths: vec!["C:\\a".into(), "C:\\ü\\b".into()] })
        );
        assert_eq!(round(enc_column(3, "col", &[]).unwrap()), (3, Request::Column { column: "col".into(), paths: vec![] }));
        let limits = Limits { max_bytes: 1 << 33, max_px: 512 };
        assert_eq!(round(enc_preview(4, "p", "C:\\x.png", limits).unwrap()), (4, Request::Preview { id: "p".into(), path: "C:\\x.png".into(), limits }));
        assert_eq!(
            round(enc_vfs_list(5, "zip", "C:\\a.zip\\d", Some("cur")).unwrap()),
            (5, Request::VfsList { id: "zip".into(), path: "C:\\a.zip\\d".into(), cursor: Some("cur".into()) })
        );
        assert_eq!(
            round(enc_vfs_read(6, "zip", "C:\\a.zip\\f", u64::MAX, 9).unwrap()),
            (6, Request::VfsRead { id: "zip".into(), path: "C:\\a.zip\\f".into(), offset: u64::MAX, max_len: 9 })
        );
        assert_eq!(
            round(enc_search(7, "s", "needle", "", 100, None).unwrap()),
            (7, Request::Search { id: "s".into(), text: "needle".into(), root: String::new(), limit: 100, cursor: None })
        );
        assert_eq!(
            round(enc_command(8, "run", "C:\\f", &["C:\\f\\a"], 1, 3).unwrap()),
            (8, Request::Command { name: "run".into(), folder: "C:\\f".into(), selection: vec!["C:\\f\\a".into()], part: 1, parts: 3 })
        );
        assert_eq!(round(enc_shutdown(u32::MAX)), (u32::MAX, Request::Shutdown));
    }

    #[test]
    fn replies_round_trip() {
        let hello = enc_hello();
        assert_eq!(decode_hello(frame_body(&hello)).unwrap(), Hello { protocol: VERSION, host_version: env!("CARGO_PKG_VERSION").into() });
        assert!(decode_hello(frame_body(&enc_ok_empty(0))).is_err());

        let info = Info { id: "a.b".into(), version: "1.0.0".into(), entrypoints: vec![(Kind::Vfs, "z".into())] };
        let f = enc_ok_info(9, &info).unwrap();
        let r = decode_reply(frame_body(&f)).unwrap();
        assert_eq!(r.id, 9);
        assert_eq!(dec_info(r.body.unwrap()).unwrap(), info);

        let cells = vec![Cell::Empty, Cell::Text("häh".into()), Cell::Int(-5), Cell::Bytes(u64::MAX), Cell::Time(1)];
        let f = enc_ok_cells(1, &cells).unwrap();
        let mut out = Vec::new();
        assert_eq!(dec_cells(decode_reply(frame_body(&f)).unwrap().body.unwrap(), &mut out).unwrap(), 5);
        assert_eq!(out, cells);

        let doc = vec![
            Node::Heading("H".into()),
            Node::Text { text: "t".into(), dim: false },
            Node::Text { text: "d".into(), dim: true },
            Node::Mono("m".into()),
            Node::Pair("k".into(), "v".into()),
            Node::Image { w: 2, h: 1, bgra: vec![1, 2, 3, 4, 5, 6, 7, 8] },
            Node::Button { label: "Go".into(), command: "a.b.run".into() },
            Node::Indent,
            Node::Outdent,
        ];
        let f = enc_ok_doc(1, &doc).unwrap();
        let mut out = Vec::new();
        dec_doc(decode_reply(frame_body(&f)).unwrap().body.unwrap(), &mut out).unwrap();
        assert_eq!(out, doc);

        let entries = vec![VfsEntry { name: "a".into(), dir: true, size: 0, mtime: 5 }, VfsEntry { name: "b.txt".into(), dir: false, size: 9, mtime: 6 }];
        let f = enc_ok_entries(1, &entries, Some("n")).unwrap();
        assert_eq!(dec_entries(decode_reply(frame_body(&f)).unwrap().body.unwrap()).unwrap(), (entries, Some("n".to_string())));

        let hits = vec![Hit { path: "C:\\x".into(), dir: false, size: 3, mtime: 4 }];
        let f = enc_ok_hits(1, &hits, None).unwrap();
        assert_eq!(dec_hits(decode_reply(frame_body(&f)).unwrap().body.unwrap()).unwrap(), (hits, None));

        let f = enc_ok_bytes(1, b"\0\xff").unwrap();
        let mut out = vec![1];
        assert_eq!(dec_bytes(decode_reply(frame_body(&f)).unwrap().body.unwrap(), &mut out).unwrap(), 2);
        assert_eq!(out, [1, 0, 0xff]);

        let f = enc_ok_message(1, Some("done")).unwrap();
        assert_eq!(dec_message(decode_reply(frame_body(&f)).unwrap().body.unwrap()).unwrap(), Some("done".to_string()));

        let f = enc_err(77, ErrKind::Timeout, "too slow");
        let r = decode_reply(frame_body(&f)).unwrap();
        assert_eq!((r.id, r.body.err()), (77, Some((ErrKind::Timeout, "too slow".to_string()))));
        // long messages are cut on a character boundary
        let f = enc_err(1, ErrKind::Trap, &"é".repeat(2000));
        let (_, msg) = decode_reply(frame_body(&f)).unwrap().body.err().unwrap();
        assert!(msg.len() <= MAX_MESSAGE && msg.chars().all(|c| c == 'é'));
        for k in 1..=9 {
            assert_eq!(ErrKind::from_u8(k).unwrap() as u8, k);
        }
        assert_eq!(ErrKind::from_u8(0), None);
        assert_eq!(ErrKind::from_u8(10), None);
    }

    #[test]
    fn frames_over_a_stream() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &enc_column(1, "c", &["C:\\a"]).unwrap()).unwrap();
        write_frame(&mut wire, &enc_shutdown(2)).unwrap();
        let mut r = &wire[..];
        let mut buf = Vec::new();
        assert!(read_frame(&mut r, &mut buf).unwrap());
        assert_eq!(decode_request(&buf).unwrap().0, 1);
        assert!(read_frame(&mut r, &mut buf).unwrap());
        assert_eq!(decode_request(&buf).unwrap(), (2, Request::Shutdown));
        // clean end between frames
        assert!(!read_frame(&mut r, &mut buf).unwrap());
        // end inside the length prefix, and inside the body
        assert_eq!(read_frame(&mut &wire[..2], &mut buf).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(read_frame(&mut &wire[..9], &mut buf).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn refuses_bad_lengths_without_allocating() {
        let mut buf = Vec::new();
        // shorter than a header, and larger than the cap
        for len in [0u32, 4, MAX_FRAME as u32 + 1, u32::MAX] {
            let mut wire = len.to_le_bytes().to_vec();
            wire.extend_from_slice(&[0; 16]);
            assert_eq!(read_frame(&mut &wire[..], &mut buf).unwrap_err().kind(), io::ErrorKind::InvalidData, "{len}");
            assert!(buf.capacity() < 1024);
        }
        // a count the frame can not hold
        let mut e = Enc::new(1, reply::OK);
        e.u32(u32::MAX);
        let f = e.finish().unwrap();
        let mut out = Vec::new();
        assert_eq!(dec_cells(decode_reply(frame_body(&f)).unwrap().body.unwrap(), &mut out), Err(ProtoError("count larger than the frame")));
        assert_eq!(out.capacity(), 0);
        // a string length past the end
        let mut e = Enc::new(1, op::COLUMN);
        e.u32(1000).u8(b'x');
        assert_eq!(decode_request(frame_body(&e.finish().unwrap())), Err(ProtoError("truncated")));
    }

    #[test]
    fn refuses_malformed_bodies() {
        let body = |f: &dyn Fn(&mut Enc), kind: u8| {
            let mut e = Enc::new(1, kind);
            f(&mut e);
            e.finish().unwrap()
        };
        assert_eq!(decode_request(&[1, 2]), Err(ProtoError("short frame")));
        assert_eq!(decode_request(frame_body(&body(&|_| {}, 99))), Err(ProtoError("unknown request")));
        assert_eq!(decode_request(frame_body(&body(&|e| {
                e.u8(1);
            }, op::SHUTDOWN))), Err(ProtoError("trailing bytes")));
        assert_eq!(decode_request(frame_body(&body(&|e| {
                e.bytes(&[0xff, 0xfe]).u32(0);
            }, op::COLUMN))), Err(ProtoError("bad utf-8")));
        // more than 2,000 paths in one request
        let many: Vec<&str> = vec!["C:\\a"; MAX_BATCH + 1];
        assert_eq!(decode_request(frame_body(&enc_column(1, "c", &many).unwrap())), Err(ProtoError("batch larger than 2,000 paths")));
        assert!(decode_reply(frame_body(&body(&|_| {}, 0x7f))).is_err());
        assert!(decode_reply(frame_body(&body(&|e| {
                e.u8(200).str("x");
            }, reply::ERR))).is_err());
        let mut out = Vec::new();
        let f = body(&|e| {
                e.u32(1).u8(9);
            }, reply::OK);
        assert_eq!(dec_cells(decode_reply(frame_body(&f)).unwrap().body.unwrap(), &mut out), Err(ProtoError("bad cell")));
        // an image whose pixels do not match its size
        let f = body(&|e| {
                e.u32(1).u8(5).u32(100).u32(100).bytes(&[0; 8]);
            }, reply::OK);
        let mut doc = Vec::new();
        assert_eq!(dec_doc(decode_reply(frame_body(&f)).unwrap().body.unwrap(), &mut doc), Err(ProtoError("image size does not match its pixels")));
        assert!(!image_ok(u32::MAX, u32::MAX, 0));
        assert!(image_ok(0, 5, 0));
        let f = body(&|e| {
                e.bool(true).bool(false).u32(1).u32(1).u32(1).u8(7).u8(0);
            }, op::LOAD);
        assert!(decode_request(frame_body(&f)).is_err());
    }

    #[test]
    fn oversized_frames_fail_to_encode() {
        let big = vec![0u8; MAX_FRAME];
        assert_eq!(enc_ok_bytes(1, &big).err(), Some(ProtoError("frame too large")));
        assert!(enc_ok_bytes(1, &big[..MAX_FRAME - HEADER - 4]).is_ok());
    }

    #[test]
    fn chunks_cap_paths_and_bytes() {
        let owned: Vec<String> = (0..5_001).map(|i| format!("C:\\dir\\file_{i:05}.txt")).collect();
        let paths: Vec<&str> = owned.iter().map(String::as_str).collect();
        let sizes: Vec<usize> = chunks(&paths).map(<[&str]>::len).collect();
        assert_eq!(sizes, [2000, 2000, 1001]);
        assert_eq!(chunk_count(&paths), 3);
        // order is kept and nothing is lost
        let flat: Vec<&str> = chunks(&paths).flatten().copied().collect();
        assert_eq!(flat, paths);
        assert_eq!(chunk_count(&paths[..2000]), 1);
        assert_eq!(chunk_count(&paths[..2001]), 2);
        assert_eq!(chunk_count(&[]), 0);

        // the byte cap splits earlier: 300 KiB paths, 3 fit in 1 MiB
        let long = "x".repeat(300 * 1024);
        let paths = vec![long.as_str(); 7];
        assert_eq!(chunks(&paths).map(<[&str]>::len).collect::<Vec<_>>(), [3, 3, 1]);
        // one path over the cap still goes, alone
        let huge = "y".repeat(MAX_BATCH_BYTES + 1);
        let paths = vec!["a", huge.as_str(), "b"];
        assert_eq!(chunks(&paths).map(<[&str]>::len).collect::<Vec<_>>(), [1, 1, 1]);
    }
}
