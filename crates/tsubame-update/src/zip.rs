//! Minimal, strict zip reader: stored and deflate entries, no zip64, no encryption.
//! Enough for the release zip, and small enough to audit. We never extract by path:
//! callers ask for an exact entry name, so zip-slip (`..\..\x`) cannot happen.

use crate::error::{Error, Result};

struct Entry {
    name: String,
    method: u16,
    comp_size: usize,
    size: usize,
    local_off: usize,
}

pub struct Zip<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn bad(s: &str) -> Error {
    Error::Parse(format!("zip: {s}"))
}

impl<'a> Zip<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Zip<'a>> {
        // End Of Central Directory: last 22 + comment bytes
        let lo = data.len().saturating_sub(22 + 65535);
        let eocd = (lo..=data.len().saturating_sub(22))
            .rev()
            .find(|&i| u32_at(data, i) == Some(0x0605_4b50))
            .ok_or_else(|| bad("no end-of-central-directory record"))?;
        let total = u16_at(data, eocd + 10).ok_or_else(|| bad("truncated"))? as usize;
        let cd_size = u32_at(data, eocd + 12).ok_or_else(|| bad("truncated"))? as usize;
        let cd_off = u32_at(data, eocd + 16).ok_or_else(|| bad("truncated"))? as usize;
        if total == 0xFFFF || cd_off == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF {
            return Err(bad("zip64 is not supported"));
        }
        if cd_off.checked_add(cd_size).is_none_or(|e| e > data.len()) {
            return Err(bad("central directory out of range"));
        }
        let mut entries = Vec::with_capacity(total.min(1024));
        let mut p = cd_off;
        for _ in 0..total {
            if u32_at(data, p) != Some(0x0201_4b50) {
                return Err(bad("bad central directory entry"));
            }
            let flags = u16_at(data, p + 8).ok_or_else(|| bad("truncated"))?;
            let method = u16_at(data, p + 10).ok_or_else(|| bad("truncated"))?;
            let comp = u32_at(data, p + 20).ok_or_else(|| bad("truncated"))?;
            let size = u32_at(data, p + 24).ok_or_else(|| bad("truncated"))?;
            let nlen = u16_at(data, p + 28).ok_or_else(|| bad("truncated"))? as usize;
            let xlen = u16_at(data, p + 30).ok_or_else(|| bad("truncated"))? as usize;
            let clen = u16_at(data, p + 32).ok_or_else(|| bad("truncated"))? as usize;
            let off = u32_at(data, p + 42).ok_or_else(|| bad("truncated"))?;
            let name = data.get(p + 46..p + 46 + nlen).ok_or_else(|| bad("truncated name"))?;
            if flags & 1 != 0 {
                return Err(bad("encrypted entries are not supported"));
            }
            if comp == 0xFFFF_FFFF || size == 0xFFFF_FFFF || off == 0xFFFF_FFFF {
                return Err(bad("zip64 is not supported"));
            }
            entries.push(Entry {
                name: String::from_utf8_lossy(name).into_owned(),
                method,
                comp_size: comp as usize,
                size: size as usize,
                local_off: off as usize,
            });
            p += 46 + nlen + xlen + clen;
        }
        Ok(Zip { data, entries })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// Decompress the entry with exactly this name. `max` caps the output size.
    /// Duplicate names are refused (a classic "which one did you check" trick).
    pub fn read(&self, name: &str, max: usize) -> Result<Vec<u8>> {
        let mut it = self.entries.iter().filter(|e| e.name == name);
        let e = it.next().ok_or_else(|| bad(&format!("entry {name:?} not found")))?;
        if it.next().is_some() {
            return Err(bad(&format!("entry {name:?} appears twice")));
        }
        if e.size > max {
            return Err(Error::SizeMismatch { what: format!("zip entry {name}") });
        }
        let d = self.data;
        if u32_at(d, e.local_off) != Some(0x0403_4b50) {
            return Err(bad("bad local header"));
        }
        let nlen = u16_at(d, e.local_off + 26).ok_or_else(|| bad("truncated"))? as usize;
        let xlen = u16_at(d, e.local_off + 28).ok_or_else(|| bad("truncated"))? as usize;
        let start = e.local_off + 30 + nlen + xlen;
        let comp = d.get(start..start.checked_add(e.comp_size).ok_or_else(|| bad("overflow"))?).ok_or_else(|| bad("entry data out of range"))?;
        let out = match e.method {
            0 => comp.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(comp, max).map_err(|e| bad(&format!("inflate failed: {e:?}")))?,
            m => return Err(bad(&format!("unsupported compression method {m}"))),
        };
        if out.len() != e.size {
            return Err(Error::SizeMismatch { what: format!("zip entry {name}") });
        }
        Ok(out)
    }
}

/// Test helper: build a zip (stored or deflate) in memory.
#[cfg(any(test, feature = "test-server"))]
pub fn build_zip(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    }
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in entries {
        let comp = if deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
        let method: u16 = if deflate { 8 } else { 0 };
        let off = out.len() as u32;
        let crc = crc32(data);
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x21, 0]); // time, date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&comp);

        cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        cd.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&[0, 0, 0x21, 0]);
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&[0; 8]); // extra len, comment len, disk, int attrs
        cd.extend_from_slice(&[0; 4]); // ext attrs
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        (0..50_000u32).map(|i| (i % 253) as u8).collect()
    }

    #[test]
    fn stored_and_deflate_roundtrip() {
        for deflate in [false, true] {
            let exe = sample();
            let z = build_zip(&[("tsubame.exe", &exe), ("README.txt", b"hello")], deflate);
            let zip = Zip::parse(&z).unwrap();
            assert_eq!(zip.names().collect::<Vec<_>>(), vec!["tsubame.exe", "README.txt"]);
            assert_eq!(zip.read("tsubame.exe", 1 << 20).unwrap(), exe);
            assert_eq!(zip.read("README.txt", 100).unwrap(), b"hello");
        }
    }

    #[test]
    fn limits_and_lookup() {
        let exe = sample();
        let z = build_zip(&[("tsubame.exe", &exe)], true);
        let zip = Zip::parse(&z).unwrap();
        assert!(matches!(zip.read("tsubame.exe", 100), Err(Error::SizeMismatch { .. })));
        assert!(zip.read("..\\tsubame.exe", 1 << 20).is_err());
        assert!(zip.read("missing", 1 << 20).is_err());
    }

    #[test]
    fn duplicates_refused() {
        let z = build_zip(&[("tsubame.exe", b"one"), ("tsubame.exe", b"two")], false);
        let zip = Zip::parse(&z).unwrap();
        assert!(zip.read("tsubame.exe", 100).is_err());
    }

    #[test]
    fn garbage_refused() {
        assert!(Zip::parse(b"").is_err());
        assert!(Zip::parse(b"not a zip at all, definitely").is_err());
        let mut z = build_zip(&[("a", b"abc")], false);
        z.truncate(z.len() - 5);
        assert!(Zip::parse(&z).is_err());
        // central directory pointing past the end
        let mut z = build_zip(&[("a", b"abc")], false);
        let n = z.len();
        z[n - 6..n - 2].copy_from_slice(&0xFFFF_0000u32.to_le_bytes());
        assert!(Zip::parse(&z).is_err());
    }

    #[test]
    fn corrupt_deflate_refused() {
        let exe = sample();
        let mut z = build_zip(&[("tsubame.exe", &exe)], true);
        z[40] ^= 0xFF; // inside the compressed stream
        z[41] ^= 0xFF;
        let zip = Zip::parse(&z).unwrap();
        assert!(zip.read("tsubame.exe", 1 << 20).is_err());
    }
}
