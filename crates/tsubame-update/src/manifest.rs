//! `tsubame-update.json`: the signed description of one release.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "version": "0.2.0",
//!   "channel": "stable",
//!   "released": "2026-11-17T12:00:00Z",
//!   "notes_url": "https://github.com/watdafawx/tsubame/releases/tag/v0.2.0",
//!   "min_supported_version": "0.1.0",
//!   "assets": [
//!     { "kind": "portable", "arch": "x64", "file": "tsubame-0.2.0-win-x64.zip", "size": 123, "sha256": "..",
//!       "contents": [ { "name": "tsubame.exe", "size": 456, "sha256": ".." } ] },
//!     { "kind": "setup",    "arch": "x64", "file": "tsubame-0.2.0-x64-setup.exe", "size": 789, "sha256": ".." }
//!   ]
//! }
//! ```
//! The updater itself only uses the `portable` asset (a zip of the app's exes). The
//! `setup` asset is listed for humans and for the `too old to update in place` hint.

use crate::channel::{self, Channel};
use crate::error::{Error, Result};
use crate::keys;
use crate::version::Version;
use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "tsubame-update.json";
pub const SIGNATURE_FILE: &str = "tsubame-update.json.sig";
/// The nightly channel's manifest (on the rolling `nightly` pre-release). A different name on
/// purpose: it can never be fetched, cached or served as the stable one.
pub const NIGHTLY_MANIFEST_FILE: &str = "tsubame-update-nightly.json";
pub const NIGHTLY_SIGNATURE_FILE: &str = "tsubame-update-nightly.json.sig";
pub const SUMS_FILE: &str = "SHA256SUMS";

pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
pub const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

/// The only files the updater will ever write into the install dir. A signed manifest
/// that names anything else is refused, so a manifest bug cannot make the updater
/// drop arbitrary names next to the exe.
pub const UPDATABLE_FILES: &[&str] = &["tsubame.exe", "tsubame-plughost.exe", "tsubame-handler.exe"];
/// Without this file the update is meaningless.
pub const MAIN_EXE: &str = "tsubame.exe";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Content {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Asset {
    pub kind: String,
    pub arch: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub contents: Vec<Content>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    pub channel: String,
    pub released: String,
    pub notes_url: String,
    pub min_supported_version: String,
    pub assets: Vec<Asset>,
}

/// A manifest whose signature and every field has been checked.
#[derive(Clone, Debug)]
pub struct Verified {
    pub manifest: Manifest,
    pub version: Version,
    pub min_supported: Version,
    /// The channel named inside the signed bytes.
    pub channel: Channel,
    /// SHA-256 of the exact signed bytes (identifies this release's manifest).
    pub manifest_sha256: String,
}

impl Verified {
    /// The asset the updater downloads: the portable zip for this machine.
    pub fn update_asset(&self) -> Option<&Asset> {
        self.manifest.assets.iter().find(|a| a.kind == "portable" && a.arch == "x64")
    }
}

/// Plain file names only: no separators, no `..`, no drive letters, no leading dot.
pub fn is_safe_file_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('.')
        && !s.contains("..")
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Verify the signature first (over the exact bytes), only then parse, then validate.
pub fn verify(manifest_bytes: &[u8], sig_bytes: &[u8], keys_: &[ed25519_compact::PublicKey]) -> Result<Verified> {
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(Error::Parse("manifest is too large".into()));
    }
    keys::verify_signature(manifest_bytes, sig_bytes, keys_)?;
    parse_validated(manifest_bytes)
}

/// [`verify`], and the signed `channel` must be the one this manifest was fetched for. A nightly
/// manifest renamed to `tsubame-update.json` (or served from a stable release) is refused.
pub fn verify_for(expected: Channel, manifest_bytes: &[u8], sig_bytes: &[u8], keys_: &[ed25519_compact::PublicKey]) -> Result<Verified> {
    let v = verify(manifest_bytes, sig_bytes, keys_)?;
    if v.channel != expected {
        return Err(Error::Parse(format!("manifest is for the {} channel, expected {}", v.channel, expected)));
    }
    Ok(v)
}

/// Parse + validate without checking a signature. Call only on bytes already verified.
pub fn parse_validated(bytes: &[u8]) -> Result<Verified> {
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|e| Error::Parse(format!("manifest: {e}")))?;
    if manifest.schema != 1 {
        return Err(Error::Parse(format!("unsupported manifest schema {}", manifest.schema)));
    }
    let Some(chan) = Channel::parse(&manifest.channel) else {
        return Err(Error::Parse(format!("channel {:?} is not supported (stable or nightly)", manifest.channel)));
    };
    let version = Version::parse(&manifest.version)?;
    // a nightly is always `X.Y.Z-nightly.N`, a stable release never is: the two can't be mixed up
    match (chan, channel::nightly_run(&version)) {
        (Channel::Nightly, None) => return Err(Error::Parse(format!("nightly manifest has a non-nightly version {version}"))),
        (Channel::Stable, Some(_)) => return Err(Error::Parse(format!("stable manifest has a nightly version {version}"))),
        _ => {}
    }
    let min_supported = Version::parse(&manifest.min_supported_version)?;
    if min_supported > version {
        return Err(Error::Parse("min_supported_version is newer than version".into()));
    }
    if !manifest.notes_url.starts_with("https://") {
        return Err(Error::Parse("notes_url must be https".into()));
    }
    if manifest.assets.is_empty() || manifest.assets.len() > 16 {
        return Err(Error::Parse("manifest lists no assets (or too many)".into()));
    }
    for a in &manifest.assets {
        if !is_safe_file_name(&a.file) {
            return Err(Error::Parse(format!("unsafe asset name {:?}", a.file)));
        }
        if a.size == 0 || a.size > MAX_ASSET_BYTES {
            return Err(Error::Parse(format!("asset {} has an unreasonable size", a.file)));
        }
        if crate::hash::norm_sha256_hex(&a.sha256).is_none() {
            return Err(Error::Parse(format!("asset {} has no valid sha256", a.file)));
        }
        for c in &a.contents {
            if !UPDATABLE_FILES.contains(&c.name.as_str()) {
                return Err(Error::Parse(format!("asset {} lists a file the updater never installs: {:?}", a.file, c.name)));
            }
            if c.size == 0 || c.size > MAX_ASSET_BYTES || crate::hash::norm_sha256_hex(&c.sha256).is_none() {
                return Err(Error::Parse(format!("bad content entry {:?}", c.name)));
            }
        }
    }
    let v = Verified { manifest_sha256: crate::hash::sha256_hex(bytes)?, manifest, version, min_supported, channel: chan };
    if let Some(a) = v.update_asset() {
        if !a.contents.iter().any(|c| c.name == MAIN_EXE) {
            return Err(Error::Parse(format!("portable asset does not list {MAIN_EXE}")));
        }
        let mut names: Vec<&str> = a.contents.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        if names.len() != a.contents.len() {
            return Err(Error::Parse("duplicate content entries".into()));
        }
    }
    Ok(v)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::devkey::dev_keypair;

    pub const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    pub fn sample(version: &str) -> Manifest {
        Manifest {
            schema: 1,
            version: version.into(),
            channel: "stable".into(),
            released: "2026-11-17T12:00:00Z".into(),
            notes_url: format!("https://github.com/watdafawx/tsubame/releases/tag/v{version}"),
            min_supported_version: "0.0.1".into(),
            assets: vec![
                Asset {
                    kind: "portable".into(),
                    arch: "x64".into(),
                    file: format!("tsubame-{version}-win-x64.zip"),
                    size: 1000,
                    sha256: H.into(),
                    contents: vec![Content { name: "tsubame.exe".into(), size: 500, sha256: H.into() }],
                },
                Asset { kind: "setup".into(), arch: "x64".into(), file: format!("tsubame-{version}-x64-setup.exe"), size: 2000, sha256: H.into(), contents: vec![] },
            ],
        }
    }

    pub fn signed(m: &Manifest) -> (Vec<u8>, Vec<u8>) {
        let bytes = serde_json::to_vec_pretty(m).unwrap();
        let sig = crate::hash::to_hex(dev_keypair().sk.sign(keys::signed_message(&bytes), None).as_ref());
        (bytes, sig.into_bytes())
    }

    #[test]
    fn valid_manifest_parses() {
        let (b, s) = signed(&sample("0.2.0"));
        let v = verify(&b, &s, &[dev_keypair().pk]).unwrap();
        assert_eq!(v.version.to_string(), "0.2.0");
        assert_eq!(v.update_asset().unwrap().file, "tsubame-0.2.0-win-x64.zip");
        assert_eq!(v.manifest_sha256.len(), 64);
    }

    #[test]
    fn signature_checked_before_parse() {
        let (mut b, s) = signed(&sample("0.2.0"));
        b[10] ^= 1;
        assert!(matches!(verify(&b, &s, &[dev_keypair().pk]), Err(Error::BadSignature(_))));
    }

    fn mutated(f: impl FnOnce(&mut Manifest)) -> Result<Verified> {
        let mut m = sample("0.2.0");
        f(&mut m);
        let (b, _) = signed(&m);
        parse_validated(&b)
    }

    #[test]
    fn validation_rules() {
        assert!(mutated(|_| {}).is_ok());
        assert!(mutated(|m| m.schema = 2).is_err());
        assert!(mutated(|m| m.channel = "beta".into()).is_err());
        assert!(mutated(|m| m.version = "abc".into()).is_err());
        assert!(mutated(|m| m.min_supported_version = "9.0.0".into()).is_err());
        assert!(mutated(|m| m.notes_url = "http://x".into()).is_err());
        assert!(mutated(|m| m.assets.clear()).is_err());
        assert!(mutated(|m| m.assets[0].file = "..\\evil.zip".into()).is_err());
        assert!(mutated(|m| m.assets[0].file = "a/b.zip".into()).is_err());
        assert!(mutated(|m| m.assets[0].size = 0).is_err());
        assert!(mutated(|m| m.assets[0].size = MAX_ASSET_BYTES + 1).is_err());
        assert!(mutated(|m| m.assets[0].sha256 = "xyz".into()).is_err());
        assert!(mutated(|m| m.assets[0].contents[0].name = "evil.dll".into()).is_err());
        assert!(mutated(|m| m.assets[0].contents.clear()).is_err());
        let dup = Content { name: "tsubame.exe".into(), size: 5, sha256: H.into() };
        assert!(mutated(|m| m.assets[0].contents.push(dup)).is_err());
    }

    pub fn sample_nightly(version: &str) -> Manifest {
        let mut m = sample(version);
        m.channel = "nightly".into();
        m.notes_url = "https://github.com/watdafawx/tsubame/releases/tag/nightly".into();
        m
    }

    #[test]
    fn nightly_manifest_signs_and_verifies_with_the_dev_key() {
        let (b, s) = signed(&sample_nightly("0.1.1-nightly.42"));
        let v = verify_for(Channel::Nightly, &b, &s, &[dev_keypair().pk]).unwrap();
        assert_eq!(v.channel, Channel::Nightly);
        assert_eq!(v.version.to_string(), "0.1.1-nightly.42");
        // same bytes, wrong channel expected: refused even though the signature is fine
        assert!(matches!(verify_for(Channel::Stable, &b, &s, &[dev_keypair().pk]), Err(Error::Parse(_))));
        // and a stable manifest is not accepted where a nightly one is expected
        let (b2, s2) = signed(&sample("0.2.0"));
        assert!(verify_for(Channel::Nightly, &b2, &s2, &[dev_keypair().pk]).is_err());
        assert!(verify_for(Channel::Stable, &b2, &s2, &[dev_keypair().pk]).is_ok());
        // flipping the channel after signing breaks the signature
        let txt = String::from_utf8(b.clone()).unwrap().replace("\"nightly\"", "\"stable\"");
        assert!(matches!(verify(txt.as_bytes(), &s, &[dev_keypair().pk]), Err(Error::BadSignature(_))));
    }

    #[test]
    fn channel_and_version_shape_must_agree() {
        let nightly = |ver: &str| {
            let (b, _) = signed(&sample_nightly(ver));
            parse_validated(&b)
        };
        assert!(nightly("0.1.1-nightly.1").is_ok());
        assert!(nightly("0.1.1").is_err(), "a nightly manifest with a plain version");
        assert!(nightly("0.1.1-rc.1").is_err());
        assert!(mutated(|m| m.version = "0.2.1-nightly.3".into()).is_err(), "a stable manifest with a nightly version");
        assert!(mutated(|m| m.channel = "Nightly".into()).is_err());
    }

    #[test]
    fn safe_names() {
        assert!(is_safe_file_name("tsubame-0.2.0-win-x64.zip"));
        for bad in ["", ".hidden", "a b", "a/b", "a\\b", "a..b", "c:x", "x\0"] {
            assert!(!is_safe_file_name(bad), "{bad:?}");
        }
    }
}
