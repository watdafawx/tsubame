//! Tiny framing used between the app (client) and `tsubame-plughost serve` over stdin/stdout.
//!
//! Request:  `[op u8][pad 3][len u32 LE][payload len bytes]`
//! Response: `[len u32 LE][payload len bytes]`
//! Payloads use the packed layout `[n u32][off_0 .. off_n u32][utf8 bytes]` (all LE).

use std::io::{self, Read, Write};

pub const OP_QUIT: u8 = 0;
/// Host decodes the payload into `Vec<String>` and calls the `list<string>` export.
pub const OP_LIST: u8 = 1;
/// Host passes the payload straight to the packed export.
pub const OP_PACKED: u8 = 2;

pub fn pack<S: AsRef<str>>(items: &[S]) -> Vec<u8> {
    let total: usize = items.iter().map(|s| s.as_ref().len()).sum();
    let mut out = Vec::with_capacity(4 + (items.len() + 1) * 4 + total);
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    let mut off = 0u32;
    out.extend_from_slice(&off.to_le_bytes());
    for s in items {
        off += s.as_ref().len() as u32;
        out.extend_from_slice(&off.to_le_bytes());
    }
    for s in items {
        out.extend_from_slice(s.as_ref().as_bytes());
    }
    out
}

pub fn unpack(buf: &[u8]) -> Vec<String> {
    let rd = |at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().unwrap()) as usize;
    let n = rd(0);
    let data = &buf[4 + (n + 1) * 4..];
    (0..n)
        .map(|i| String::from_utf8_lossy(&data[rd(4 + i * 4)..rd(4 + (i + 1) * 4)]).into_owned())
        .collect()
}

pub fn write_request<W: Write>(w: &mut W, op: u8, payload: &[u8]) -> io::Result<()> {
    let mut h = [0u8; 8];
    h[0] = op;
    h[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&h)?;
    w.write_all(payload)?;
    w.flush()
}

/// `Ok(None)` on clean EOF.
pub fn read_request<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<Option<u8>> {
    let mut h = [0u8; 8];
    match r.read_exact(&mut h) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)?;
    Ok(Some(h[0]))
}

pub fn write_response<W: Write>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

pub fn read_response<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<()> {
    let mut h = [0u8; 4];
    r.read_exact(&mut h)?;
    let len = u32::from_le_bytes(h) as usize;
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)
}

/// Realistic-looking Windows paths (about 70 bytes each).
pub fn gen_paths(n: usize) -> Vec<String> {
    const EXT: [&str; 8] = ["rs", "txt", "png", "md", "toml", "dll", "jpeg", "cargo-lock"];
    (0..n)
        .map(|i| {
            format!(
                "C:\\Users\\MIcha\\Documents\\Projects\\folder_{:03}\\sub_{:02}\\file_{:06}.{}",
                i % 997,
                i % 31,
                i,
                EXT[i % EXT.len()]
            )
        })
        .collect()
}

/// What the sample guest must return for `path` (used to verify the guest).
pub fn expected(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    };
    format!("{} {}", ext.to_uppercase(), path.len())
}
