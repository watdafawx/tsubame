//! Trust anchors compiled into the binary.
//!
//! The manifest is NOT trusted because it came from GitHub over HTTPS. It is trusted
//! because it carries an ed25519 signature made by a key whose public half is pinned
//! here, in the exe the user already runs. Someone who can rewrite the release assets
//! (stolen GitHub token, compromised CI) still cannot make an update pass without the
//! private key, which lives outside GitHub (maintainer machine + one repository secret
//! used only by the release workflow).
//!
//! Fail closed: with no key pinned, every update is refused.

use crate::error::{Error, Result};
use crate::hash::from_hex;
use ed25519_compact::PublicKey;

/// Release signing public keys, hex (64 chars each). More than one allows key rotation:
/// ship a build that pins the next key first, then switch the signing secret.
///
/// MAINTAINER: run `tools\release-keygen.ps1`, paste the printed public key here,
/// commit. See docs/release.md. The private key is never committed.
pub const RELEASE_PUBLIC_KEYS_HEX: &[&str] = &[];

/// A pinned Authenticode signer. Used only once the project has a code-signing
/// certificate: when this list is non-empty every extracted exe must also pass
/// `WinVerifyTrust` and match one of the pins. Leave empty until then.
#[derive(Clone, Copy, Debug)]
pub enum Pin {
    /// SHA-256 (hex) of the signer certificate's public-key bits. Stable for a classic
    /// OV certificate until it is renewed with the same key.
    PublicKeySha256(&'static str),
    /// Exact certificate subject (simple display name). Use this for Microsoft Artifact
    /// Signing: its leaf certificates are short-lived and rotate, the subject does not.
    Subject(&'static str),
}

pub const PINNED_SIGNERS: &[Pin] = &[];

/// Domain separation: the signature covers this prefix + the manifest bytes, so a
/// signature over some other file made by the same key can never validate as a manifest.
pub const SIGN_DOMAIN: &[u8] = b"tsubame-update-manifest-v1\n";

pub fn signed_message(manifest: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIGN_DOMAIN.len() + manifest.len());
    m.extend_from_slice(SIGN_DOMAIN);
    m.extend_from_slice(manifest);
    m
}

pub fn parse_public_key(hex: &str) -> Result<PublicKey> {
    let b = from_hex(hex).ok_or_else(|| Error::Config("public key is not hex".into()))?;
    PublicKey::from_slice(&b).map_err(|_| Error::Config("public key must be 32 bytes".into()))
}

/// The keys a release build trusts. Plus the DEV test key when (and only when) compiled
/// with `test-server` / under `cargo test`.
pub fn trusted_keys() -> Vec<PublicKey> {
    #[allow(unused_mut)]
    let mut v: Vec<PublicKey> = RELEASE_PUBLIC_KEYS_HEX.iter().filter_map(|h| parse_public_key(h).ok()).collect();
    #[cfg(any(test, feature = "test-server"))]
    v.push(crate::devkey::dev_public_key());
    v
}

/// Verify a detached signature (hex text, 128 digits) over `signed_message(manifest)`.
pub fn verify_signature(manifest: &[u8], sig_text: &[u8], keys: &[PublicKey]) -> Result<()> {
    let bad = |s: &str| Error::BadSignature(s.to_string());
    if keys.is_empty() {
        return Err(bad("no update signing key is pinned in this build"));
    }
    let text = std::str::from_utf8(sig_text).map_err(|_| bad("signature file is not text"))?;
    let bytes = from_hex(text.trim_start_matches('\u{feff}')).ok_or_else(|| bad("signature file is not hex"))?;
    let sig = ed25519_compact::Signature::from_slice(&bytes).map_err(|_| bad("signature must be 64 bytes"))?;
    let msg = signed_message(manifest);
    if keys.iter().any(|k| k.verify(&msg, &sig).is_ok()) {
        Ok(())
    } else {
        Err(bad("manifest was not signed by a pinned key"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devkey::dev_keypair;

    fn sign(kp: &ed25519_compact::KeyPair, m: &[u8]) -> String {
        crate::hash::to_hex(kp.sk.sign(signed_message(m), None).as_ref())
    }

    #[test]
    fn good_signature_passes() {
        let kp = dev_keypair();
        let sig = sign(&kp, b"{\"a\":1}");
        verify_signature(b"{\"a\":1}", sig.as_bytes(), &[kp.pk]).unwrap();
        // trailing newline / CRLF in the .sig file is fine
        verify_signature(b"{\"a\":1}", format!("{sig}\r\n").as_bytes(), &[kp.pk]).unwrap();
    }

    #[test]
    fn tampered_manifest_fails() {
        let kp = dev_keypair();
        let sig = sign(&kp, b"{\"a\":1}");
        assert!(matches!(verify_signature(b"{\"a\":2}", sig.as_bytes(), &[kp.pk]), Err(Error::BadSignature(_))));
    }

    #[test]
    fn other_key_fails() {
        let kp = dev_keypair();
        let other = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([7u8; 32]));
        let sig = sign(&other, b"x");
        assert!(verify_signature(b"x", sig.as_bytes(), &[kp.pk]).is_err());
    }

    #[test]
    fn no_pinned_key_fails_closed() {
        let kp = dev_keypair();
        let sig = sign(&kp, b"x");
        assert!(verify_signature(b"x", sig.as_bytes(), &[]).is_err());
    }

    #[test]
    fn malformed_signature_fails() {
        let kp = dev_keypair();
        for s in ["", "zz", "00", &"0".repeat(128)] {
            assert!(verify_signature(b"x", s.as_bytes(), &[kp.pk]).is_err(), "{s:?}");
        }
        assert!(verify_signature(b"x", &[0xff, 0xfe], &[kp.pk]).is_err());
    }

    #[test]
    fn domain_separation() {
        // a signature over the bare bytes (no domain prefix) must not validate
        let kp = dev_keypair();
        let raw = crate::hash::to_hex(kp.sk.sign(b"x", None).as_ref());
        assert!(verify_signature(b"x", raw.as_bytes(), &[kp.pk]).is_err());
    }

    #[test]
    fn release_build_trusts_only_pinned_keys() {
        // the shipped list parses (empty until the maintainer pins a key)
        for h in RELEASE_PUBLIC_KEYS_HEX {
            parse_public_key(h).unwrap();
        }
    }
}
