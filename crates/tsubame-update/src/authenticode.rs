//! Authenticode verification with `WinVerifyTrust` + signer pinning.
//!
//! Used once the project owns a code-signing certificate (`keys::PINNED_SIGNERS` is
//! non-empty). It is a second check on top of the ed25519-signed manifest, not a
//! replacement: ed25519 pins *our* key in the binary; Authenticode additionally makes
//! the extracted exe something Windows itself accepts as signed by the publisher.
//!
//! Revocation checking is off (`WTD_REVOKE_NONE`): it would make the update depend on
//! a CRL/OCSP round trip and fail offline. The pin, not the revocation list, is the
//! decision.

use crate::error::{Error, Result};
use crate::keys::Pin;
use std::ffi::c_void;
use std::path::Path;
use windows::Win32::Foundation::{HANDLE, HWND, INVALID_HANDLE_VALUE};
use windows::Win32::Security::Cryptography::{CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE};
use windows::Win32::Security::WinTrust::*;

#[derive(Debug, Clone, PartialEq)]
pub struct Signer {
    /// SHA-256 (hex) of the leaf certificate's public-key bits.
    pub public_key_sha256: String,
    /// Leaf certificate subject, simple display name.
    pub subject: String,
}

/// Pure decision: does this signer satisfy at least one pin?
pub fn pins_match(pins: &[Pin], s: &Signer) -> bool {
    pins.iter().any(|p| match p {
        Pin::PublicKeySha256(h) => h.eq_ignore_ascii_case(&s.public_key_sha256),
        Pin::Subject(n) => *n == s.subject,
    })
}

/// `WinVerifyTrust` the file; on success return the leaf signer.
pub fn verify_file(path: &Path) -> Result<Signer> {
    let wpath: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: windows::core::PCWSTR(wpath.as_ptr()),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_NONE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let hwnd = HWND(INVALID_HANDLE_VALUE.0);
    unsafe {
        let status = WinVerifyTrust(hwnd, &mut action, &mut data as *mut _ as *mut c_void);
        let result = if status != 0 {
            Err(Error::Untrusted(format!("{}: Authenticode check failed (WinVerifyTrust {:#010x})", path.display(), status as u32)))
        } else {
            signer_of(data.hWVTStateData)
        };
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        let _ = WinVerifyTrust(hwnd, &mut action, &mut data as *mut _ as *mut c_void);
        result
    }
}

unsafe fn signer_of(state: HANDLE) -> Result<Signer> {
    let fail = |s: &str| Error::Untrusted(format!("cannot read signer certificate: {s}"));
    unsafe {
        let prov = WTHelperProvDataFromStateData(state);
        if prov.is_null() {
            return Err(fail("no provider data"));
        }
        let sgnr = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
        if sgnr.is_null() {
            return Err(fail("no signer"));
        }
        let cert = WTHelperGetProvCertFromChain(sgnr, 0);
        if cert.is_null() || (*cert).pCert.is_null() {
            return Err(fail("no certificate"));
        }
        let ctx = (*cert).pCert;
        let info = (*ctx).pCertInfo;
        if info.is_null() {
            return Err(fail("no certificate info"));
        }
        let key = &(*info).SubjectPublicKeyInfo.PublicKey;
        if key.pbData.is_null() || key.cbData == 0 {
            return Err(fail("empty public key"));
        }
        let bits = std::slice::from_raw_parts(key.pbData, key.cbData as usize);
        let public_key_sha256 = crate::hash::sha256_hex(bits)?;
        let mut name = [0u16; 512];
        let n = CertGetNameStringW(ctx, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut name));
        let subject = String::from_utf16_lossy(&name[..(n as usize).saturating_sub(1).min(name.len())]);
        Ok(Signer { public_key_sha256, subject })
    }
}

/// Verify the file's signature and that the signer matches a pin.
pub fn verify_pinned(path: &Path, pins: &[Pin]) -> Result<Signer> {
    let s = verify_file(path)?;
    if pins_match(pins, &s) {
        Ok(s)
    } else {
        Err(Error::Untrusted(format!("{}: signed by {:?} (key {}), which is not the pinned publisher", path.display(), s.subject, s.public_key_sha256)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> Signer {
        Signer { public_key_sha256: "ab".repeat(32), subject: "Tsubame Project".into() }
    }

    #[test]
    fn pin_matching() {
        let s = signer();
        assert!(!pins_match(&[], &s)); // nothing pinned = nothing matches (callers skip the check when empty)
        assert!(pins_match(&[Pin::Subject("Tsubame Project")], &s));
        assert!(!pins_match(&[Pin::Subject("tsubame project")], &s)); // exact
        assert!(!pins_match(&[Pin::Subject("Evil Corp")], &s));
        let h: &'static str = Box::leak("AB".repeat(32).into_boxed_str());
        assert!(pins_match(&[Pin::PublicKeySha256(h)], &s)); // hex case-insensitive
        assert!(!pins_match(&[Pin::PublicKeySha256("00"), Pin::Subject("x")], &s));
    }

    #[test]
    fn unsigned_file_is_rejected() {
        // the test executable itself is not Authenticode signed
        let exe = std::env::current_exe().unwrap();
        assert!(matches!(verify_file(&exe), Err(Error::Untrusted(_))));
    }

    #[test]
    fn signed_microsoft_binary_is_read_and_pin_enforced() {
        // Find any embedded-signature Microsoft binary on this machine; skip when none.
        let candidates = [
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"C:\Program Files\dotnet\dotnet.exe",
            r"C:\Windows\System32\OneDriveSetup.exe",
            r"C:\Windows\explorer.exe",
        ];
        let mut ran = false;
        for c in candidates {
            let p = Path::new(c);
            if !p.exists() {
                continue;
            }
            if let Ok(s) = verify_file(p) {
                assert!(!s.subject.is_empty() && s.public_key_sha256.len() == 64, "{s:?}");
                assert!(verify_pinned(p, &[Pin::PublicKeySha256(Box::leak(s.public_key_sha256.clone().into_boxed_str()))]).is_ok());
                assert!(matches!(verify_pinned(p, &[Pin::Subject("Definitely Not The Signer")]), Err(Error::Untrusted(_))));
                ran = true;
                break;
            }
        }
        if !ran {
            eprintln!("note: no embedded-signature binary found; Authenticode positive path not exercised here");
        }
    }
}
