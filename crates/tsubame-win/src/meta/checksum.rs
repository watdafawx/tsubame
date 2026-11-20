//! File checksums for the P4.F5 "Checksums..." command and the checksum column.
//!
//! [`CRC32`], [`Md5`], [`Sha1`] and [`Sha256`] are pure-Rust streaming hashers, so
//! the main exe gains no new crate and no CNG import (`docs/PLAN.md` §3 rule 6:
//! dependencies stay tiny). [`MultiHasher`] feeds one read pass through all four.
//!
//! [`checksum_file`] reads in [`BUFFER_BYTES`] chunks, reports
//! `(bytes_read, total_bytes)` progress after every chunk and checks an
//! [`AtomicBool`] before every read, so a modal "Cancel" button stops a 100 GB
//! file immediately. Everything here is I/O and CPU only: call it from a worker
//! thread, never from the UI thread (`docs/PLAN.md` §3 rule 1).

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Read buffer size; one allocation per checksum run.
pub const BUFFER_BYTES: usize = 64 * 1024;

/// A checksum algorithm selectable by the command line / settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Algorithm {
    Crc32,
    Md5,
    Sha1,
    Sha256,
}

impl Algorithm {
    /// All algorithms, in display order.
    pub const ALL: [Algorithm; 4] = [
        Algorithm::Crc32,
        Algorithm::Md5,
        Algorithm::Sha1,
        Algorithm::Sha256,
    ];

    /// Canonical lower-case name (`crc32`, `md5`, `sha1`, `sha256`).
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Crc32 => "crc32",
            Algorithm::Md5 => "md5",
            Algorithm::Sha1 => "sha1",
            Algorithm::Sha256 => "sha256",
        }
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a checksum run did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChecksumError {
    /// The caller set the cancellation token.
    Cancelled,
    /// The file could not be opened or read.
    Io(String),
}

impl fmt::Display for ChecksumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChecksumError::Cancelled => f.write_str("checksum cancelled"),
            ChecksumError::Io(msg) => write!(f, "checksum failed: {msg}"),
        }
    }
}

impl std::error::Error for ChecksumError {}

/// Every digest computed for one file in a single pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChecksumSet {
    pub crc32: u32,
    pub md5: [u8; 16],
    pub sha1: [u8; 20],
    pub sha256: [u8; 32],
}

impl ChecksumSet {
    pub fn crc32_hex(&self) -> String {
        format!("{:08x}", self.crc32)
    }

    pub fn md5_hex(&self) -> String {
        hex_lower(&self.md5)
    }

    pub fn sha1_hex(&self) -> String {
        hex_lower(&self.sha1)
    }

    pub fn sha256_hex(&self) -> String {
        hex_lower(&self.sha256)
    }

    /// Hex text of one digest; CRC32 is zero-padded to 8 nibbles.
    pub fn hex(&self, alg: Algorithm) -> String {
        match alg {
            Algorithm::Crc32 => self.crc32_hex(),
            Algorithm::Md5 => self.md5_hex(),
            Algorithm::Sha1 => self.sha1_hex(),
            Algorithm::Sha256 => self.sha256_hex(),
        }
    }
}

/// Lower-case hex of `bytes`.
pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// CRC32 (IEEE 802.3, reflected polynomial 0xEDB88320)
// ---------------------------------------------------------------------------

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

const CRC32_TABLE: [u32; 256] = crc32_table();

/// Streaming CRC-32 (the usual zip/PNG checksum, `crc32` in most tools).
#[derive(Clone, Debug)]
pub struct CRC32 {
    state: u32,
}

impl Default for CRC32 {
    fn default() -> Self {
        Self::new()
    }
}

impl CRC32 {
    pub fn new() -> Self {
        Self { state: 0xffff_ffff }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.state;
        for &b in data {
            c = CRC32_TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
        }
        self.state = c;
    }

    pub fn finish(self) -> u32 {
        self.state ^ 0xffff_ffff
    }
}

// ---------------------------------------------------------------------------
// MD5 (RFC 1321)
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const MD5_T: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
    0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
    0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
    0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
    0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
    0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
    0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
    0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
    0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Streaming MD5. Not collision safe; kept for compatibility with tools that
/// still print MD5 and for verifying existing checksum files.
#[derive(Clone, Debug)]
pub struct Md5 {
    state: [u32; 4],
    buf: [u8; 64],
    buflen: usize,
    bit_len: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    pub fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buf: [0u8; 64],
            buflen: 0,
            bit_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bit_len = self.bit_len.wrapping_add((data.len() as u64) * 8);

        if self.buflen > 0 {
            let take = (64 - self.buflen).min(data.len());
            self.buf[self.buflen..self.buflen + take].copy_from_slice(&data[..take]);
            self.buflen += take;
            data = &data[take..];
            if self.buflen == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buflen = 0;
            }
        }

        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            self.compress(block);
            data = rest;
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buflen = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 16] {
        let bit_len = self.bit_len;
        self.buf[self.buflen] = 0x80;
        self.buflen += 1;
        if self.buflen > 56 {
            for b in &mut self.buf[self.buflen..] {
                *b = 0;
            }
            let block = self.buf;
            self.compress(&block);
            self.buflen = 0;
        }
        for b in &mut self.buf[self.buflen..56] {
            *b = 0;
        }
        self.buf[56..64].copy_from_slice(&bit_len.to_le_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut out = [0u8; 16];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8]) {
        debug_assert_eq!(block.len(), 64);
        let mut m = [0u32; 16];
        for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            m[i] = u32::from_le_bytes(*chunk);
        }

        let (mut a, mut b, mut c, mut d) =
            (self.state[0], self.state[1], self.state[2], self.state[3]);

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let shift = match i {
                0..=15 => [7, 12, 17, 22][i % 4],
                16..=31 => [5, 9, 14, 20][i % 4],
                32..=47 => [4, 11, 16, 23][i % 4],
                _ => [6, 10, 15, 21][i % 4],
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(MD5_T[i])
                    .wrapping_add(m[g])
                    .rotate_left(shift as u32),
            );
            a = tmp;
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

// ---------------------------------------------------------------------------
// SHA-1 (FIPS 180-4)
// ---------------------------------------------------------------------------

/// Streaming SHA-1.
#[derive(Clone, Debug)]
pub struct Sha1 {
    state: [u32; 5],
    buf: [u8; 64],
    buflen: usize,
    bit_len: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ],
            buf: [0u8; 64],
            buflen: 0,
            bit_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bit_len = self.bit_len.wrapping_add((data.len() as u64) * 8);

        if self.buflen > 0 {
            let take = (64 - self.buflen).min(data.len());
            self.buf[self.buflen..self.buflen + take].copy_from_slice(&data[..take]);
            self.buflen += take;
            data = &data[take..];
            if self.buflen == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buflen = 0;
            }
        }

        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            self.compress(block);
            data = rest;
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buflen = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 20] {
        let bit_len = self.bit_len;
        self.buf[self.buflen] = 0x80;
        self.buflen += 1;
        if self.buflen > 56 {
            for b in &mut self.buf[self.buflen..] {
                *b = 0;
            }
            let block = self.buf;
            self.compress(&block);
            self.buflen = 0;
        }
        for b in &mut self.buf[self.buflen..56] {
            *b = 0;
        }
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut out = [0u8; 20];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8]) {
        debug_assert_eq!(block.len(), 64);
        let mut w = [0u32; 80];
        for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*chunk);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (
            self.state[0],
            self.state[1],
            self.state[2],
            self.state[3],
            self.state[4],
        );

        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4)
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
    0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
    0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Streaming SHA-256.
#[derive(Clone, Debug)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buflen: usize,
    bit_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
            buf: [0u8; 64],
            buflen: 0,
            bit_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bit_len = self.bit_len.wrapping_add((data.len() as u64) * 8);

        if self.buflen > 0 {
            let take = (64 - self.buflen).min(data.len());
            self.buf[self.buflen..self.buflen + take].copy_from_slice(&data[..take]);
            self.buflen += take;
            data = &data[take..];
            if self.buflen == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buflen = 0;
            }
        }

        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            self.compress(block);
            data = rest;
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buflen = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bit_len = self.bit_len;
        self.buf[self.buflen] = 0x80;
        self.buflen += 1;
        if self.buflen > 56 {
            for b in &mut self.buf[self.buflen..] {
                *b = 0;
            }
            let block = self.buf;
            self.compress(&block);
            self.buflen = 0;
        }
        for b in &mut self.buf[self.buflen..56] {
            *b = 0;
        }
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8]) {
        debug_assert_eq!(block.len(), 64);
        let mut w = [0u32; 64];
        for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*chunk);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

// ---------------------------------------------------------------------------
// One pass, all four digests
// ---------------------------------------------------------------------------

/// Feeds the same bytes into CRC32, MD5, SHA-1 and SHA-256 in one pass.
#[derive(Clone, Debug, Default)]
pub struct MultiHasher {
    crc32: CRC32,
    md5: Md5,
    sha1: Sha1,
    sha256: Sha256,
}

impl MultiHasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, data: &[u8]) {
        self.crc32.update(data);
        self.md5.update(data);
        self.sha1.update(data);
        self.sha256.update(data);
    }

    pub fn finish(self) -> ChecksumSet {
        ChecksumSet {
            crc32: self.crc32.finish(),
            md5: self.md5.finish(),
            sha1: self.sha1.finish(),
            sha256: self.sha256.finish(),
        }
    }
}

/// Checksums of an in-memory buffer (tests, small drops, clipboard text).
pub fn checksum_bytes(bytes: &[u8]) -> ChecksumSet {
    let mut hasher = MultiHasher::new();
    hasher.update(bytes);
    hasher.finish()
}

/// Stream `reader` through every algorithm.
///
/// `progress` is called after every successful read with
/// `(bytes_read_so_far, total_bytes)`; `total_bytes` is 0 when unknown.
pub fn checksum_reader<R: Read>(
    reader: &mut R,
    total_bytes: u64,
    cancel: &AtomicBool,
    mut progress: Option<&mut dyn FnMut(u64, u64)>,
) -> Result<ChecksumSet, ChecksumError> {
    let mut hasher = MultiHasher::new();
    let mut buf = vec![0u8; BUFFER_BYTES];
    let mut read_total = 0u64;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ChecksumError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ChecksumError::Io(e.to_string())),
        };
        hasher.update(&buf[..n]);
        read_total = read_total.saturating_add(n as u64);
        if let Some(cb) = progress.as_mut() {
            (**cb)(read_total, total_bytes);
        }
    }

    Ok(hasher.finish())
}

/// Checksum one file, cancelling between 64 KB reads.
pub fn checksum_file(
    path: &Path,
    cancel: &AtomicBool,
    progress: Option<&mut dyn FnMut(u64, u64)>,
) -> Result<ChecksumSet, ChecksumError> {
    let mut file =
        File::open(path).map_err(|e| ChecksumError::Io(format!("{}: {e}", path.display())))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    checksum_reader(&mut file, total, cancel, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tsubame-checksum-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn crc32_known_vectors() {
        assert_eq!(CRC32::new().finish(), 0x0000_0000);
        let mut h = CRC32::new();
        h.update(b"123456789");
        assert_eq!(h.finish(), 0xcbf4_3926);
        let mut h = CRC32::new();
        h.update(b"abc");
        assert_eq!(h.finish(), 0x3524_41c2);
    }

    #[test]
    fn md5_known_vectors() {
        assert_eq!(
            checksum_bytes(b"").md5_hex(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(
            checksum_bytes(b"abc").md5_hex(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            checksum_bytes(b"The quick brown fox jumps over the lazy dog").md5_hex(),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn sha1_known_vectors() {
        assert_eq!(
            checksum_bytes(b"").sha1_hex(),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            checksum_bytes(b"abc").sha1_hex(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            checksum_bytes(b"The quick brown fox jumps over the lazy dog").sha1_hex(),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            checksum_bytes(b"").sha256_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            checksum_bytes(b"abc").sha256_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            checksum_bytes(b"The quick brown fox jumps over the lazy dog").sha256_hex(),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
    }

    #[test]
    fn multi_block_hashes_match_nist_vectors() {
        let data = vec![b'a'; 1_000_000];
        let set = checksum_bytes(&data);
        assert_eq!(set.md5_hex(), "7707d6ae4e027c70eea2a935c2296f21");
        assert_eq!(set.sha1_hex(), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
        assert_eq!(
            set.sha256_hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn streaming_in_odd_chunks_matches_one_shot() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let expected = checksum_bytes(&data);

        let mut hasher = MultiHasher::new();
        let mut pos = 0usize;
        let mut chunk = 1usize;
        while pos < data.len() {
            let end = (pos + chunk).min(data.len());
            hasher.update(&data[pos..end]);
            pos = end;
            chunk = (chunk * 7 + 3) % 1024 + 1;
        }
        assert_eq!(hasher.finish(), expected);
    }

    #[test]
    fn file_checksum_matches_bytes_and_reports_progress() {
        let data = vec![b'x'; BUFFER_BYTES * 2 + 123];
        let path = temp_file("data.bin", &data);

        let cancel = AtomicBool::new(false);
        let mut calls = 0usize;
        let mut last = (0u64, 0u64);
        let mut progress = |done: u64, total: u64| {
            calls += 1;
            assert_eq!(total, data.len() as u64);
            assert!(done >= last.0);
            last = (done, total);
        };
        let set = checksum_file(&path, &cancel, Some(&mut progress)).unwrap();
        assert_eq!(set, checksum_bytes(&data));
        assert_eq!(last.0, data.len() as u64);
        assert_eq!(calls, 3); // 64 KB, 128 KB, remainder

        cleanup(&path);
    }

    #[test]
    fn checksum_reader_handles_unknown_total() {
        let data = b"abc".to_vec();
        let mut cursor = Cursor::new(data);
        let cancel = AtomicBool::new(false);
        let set = checksum_reader(&mut cursor, 0, &cancel, None).unwrap();
        assert_eq!(set.md5_hex(), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn cancellation_stops_before_reading() {
        let path = temp_file("cancel.bin", &[0u8; 4096]);
        let cancel = AtomicBool::new(true);
        assert_eq!(
            checksum_file(&path, &cancel, None),
            Err(ChecksumError::Cancelled)
        );
        cleanup(&path);
    }

    #[test]
    fn algorithm_names_and_hex() {
        let set = checksum_bytes(b"abc");
        assert_eq!(set.crc32_hex(), "352441c2");
        assert_eq!(set.hex(Algorithm::Md5), set.md5_hex());
        assert_eq!(Algorithm::Sha256.to_string(), "sha256");
    }

    #[test]
    fn empty_file_checksum() {
        let path = temp_file("empty.bin", b"");
        let cancel = AtomicBool::new(false);
        let mut progress = |_done: u64, total: u64| assert_eq!(total, 0);
        let set = checksum_file(&path, &cancel, Some(&mut progress)).unwrap();
        assert_eq!(set.crc32, 0);
        assert_eq!(
            set.sha256_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        cleanup(&path);
    }
}
