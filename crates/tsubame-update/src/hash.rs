//! SHA-256 through Windows CNG (bcrypt.dll): no hashing crate in the exe.

use crate::error::{Error, Result};
use windows::Win32::Security::Cryptography::{
    BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData,BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALG_HANDLE,
};

pub struct Sha256 {
    h: BCRYPT_HASH_HANDLE,
}

impl Sha256 {
    pub fn new() -> Result<Sha256> {
        let mut h = BCRYPT_HASH_HANDLE::default();
        let st = unsafe { BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut h, None, None, 0) };
        if st.is_err() {
            return Err(Error::Config(format!("BCryptCreateHash failed: {:#x}", st.0)));
        }
        Ok(Sha256 { h })
    }

    pub fn update(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let st = unsafe { BCryptHashData(self.h, data, 0) };
        if st.is_err() {
            return Err(Error::Config(format!("BCryptHashData failed: {:#x}", st.0)));
        }
        Ok(())
    }

    pub fn finish(self) -> Result<[u8; 32]> {
        let mut out = [0u8; 32];
        let st = unsafe { BCryptFinishHash(self.h, &mut out, 0) };
        if st.is_err() {
            return Err(Error::Config(format!("BCryptFinishHash failed: {:#x}", st.0)));
        }
        Ok(out)
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyHash(self.h);
        }
    }
}

pub fn sha256(data: &[u8]) -> Result<[u8; 32]> {
    let mut h = Sha256::new()?;
    h.update(data)?;
    h.finish()
}

pub fn sha256_hex(data: &[u8]) -> Result<String> {
    Ok(to_hex(&sha256(data)?))
}

pub fn sha256_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new()?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n])?;
    }
    Ok(to_hex(&h.finish()?))
}

pub fn to_hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// 64 hex digits, any case -> lowercase.
pub fn norm_sha256_hex(s: &str) -> Option<String> {
    let t = s.trim();
    (t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())).then(|| t.to_ascii_lowercase())
}

/// Parse a `SHA256SUMS` file: `<hex>  <file>` (also `<hex> *<file>`), one per line.
pub fn parse_sha256sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start_matches('\u{feff}').trim();
            let (h, rest) = l.split_once(char::is_whitespace)?;
            let name = rest.trim().trim_start_matches('*');
            Some((norm_sha256_hex(h)?, name.to_string())).filter(|(_, n)| !n.is_empty())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(sha256_hex(b"").unwrap(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn streaming_equals_one_shot() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let mut h = Sha256::new().unwrap();
        for c in data.chunks(777) {
            h.update(c).unwrap();
        }
        assert_eq!(to_hex(&h.finish().unwrap()), sha256_hex(&data).unwrap());
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(from_hex("00ff10").unwrap(), vec![0, 255, 16]);
        assert_eq!(to_hex(&[0, 255, 16]), "00ff10");
        assert!(from_hex("0").is_none());
        assert!(from_hex("zz").is_none());
        assert_eq!(norm_sha256_hex(&"A".repeat(64)).unwrap(), "a".repeat(64));
        assert!(norm_sha256_hex("abc").is_none());
    }

    #[test]
    fn sums_file() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let t = format!("{a}  tsubame.zip\r\n{b} *setup.exe\r\nnot a line\r\n\r\n");
        let v = parse_sha256sums(&t);
        assert_eq!(v, vec![(a, "tsubame.zip".to_string()), ("b".repeat(64), "setup.exe".to_string())]);
    }
}
