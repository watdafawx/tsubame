//! The install record: what the user approved for one installed plugin, and
//! the hashes that tie it to the files it was approved for (security review
//! PH-02, PH-03, PH-10, PH-18).
//!
//! Installing a plugin (`tsubame-plugc`, [`store`]) writes one folder per id
//! and version:
//!
//! ```text
//! <cache>\<id>\<version>\component.cwasm   the compiled component
//! <cache>\<id>\<version>\install.toml      this record
//! ```
//!
//! Both files are written once: each goes to a new file with a random name
//! (`create_new`, so a planted file or link at that name makes the write fail)
//! and is then linked to its final name, which fails if that name exists. A
//! record is never replaced; reinstalling means removing the folder first.
//!
//! The record holds the id, the version, the SHA-256 of `plugin.toml`, of the
//! `.wasm` and of the `.cwasm`, the wasmtime version the `.cwasm` was compiled
//! for, and the resolved capabilities the user approved. On every start
//! [`verify`] reads the record, hashes `plugin.toml` and the `.wasm` as they
//! are now and refuses any difference, and returns the capabilities **from the
//! record**, never from the live `plugin.toml`. The host checks the `.cwasm`
//! against the record's hash on the bytes it deserializes.
//!
//! The record and the `.cwasm` live in the cache folder, which a plugin can
//! neither read nor write, whatever it was granted ([`caps::Protected`]); the
//! plugin's own folder can be read (`$plugin`) but never written.

use crate::caps::{self, Access, CapError, Capabilities, PathGrant, Protected, Scope};
use crate::manifest::{MANIFEST_FILE, MAX_MANIFEST_BYTES, Manifest};
use crate::sha256;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The record's file name in the install folder.
pub const RECORD_FILE: &str = "install.toml";
/// The compiled component's file name in the install folder.
pub const COMPONENT_FILE: &str = "component.cwasm";
/// Largest record read.
pub const MAX_RECORD_BYTES: u64 = 64 * 1024;
/// Largest `.wasm` a package may ship.
pub const MAX_WASM_BYTES: u64 = 64 << 20;
/// The record format this build writes and reads.
pub const FORMAT: u32 = 1;
/// The wasmtime the host is built with: a `.cwasm` only loads in that version.
/// A unit test keeps it in step with `Cargo.toml`.
pub const WASMTIME_VERSION: &str = "50.0.2";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordError(pub String);

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RecordError {}

fn err<T>(msg: impl Into<String>) -> Result<T, RecordError> {
    Err(RecordError(msg.into()))
}

/// One path grant as the record spells it: a normalized folder, `$input` or `*`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordPath {
    pub path: String,
    /// `read` or `read-write`.
    pub access: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordCaps {
    pub network: bool,
    pub run_processes: bool,
    pub max_memory_mb: u32,
    pub timeout_ms: u32,
    #[serde(default)]
    pub paths: Vec<RecordPath>,
}

impl RecordCaps {
    pub fn from_caps(c: &Capabilities) -> RecordCaps {
        let paths = c
            .paths
            .iter()
            .map(|g| RecordPath {
                path: match &g.scope {
                    Scope::Prefix(p) => p.clone(),
                    Scope::Input => "$input".into(),
                    Scope::Any => "*".into(),
                },
                access: if g.access == Access::ReadWrite { "read-write" } else { "read" }.into(),
            })
            .collect();
        RecordCaps { network: c.network, run_processes: c.run_processes, max_memory_mb: c.max_memory_mb, timeout_ms: c.timeout_ms, paths }
    }

    /// The capabilities, checked as the host checks them.
    pub fn to_caps(&self) -> Result<Capabilities, RecordError> {
        let mut paths = Vec::with_capacity(self.paths.len());
        for p in &self.paths {
            let access = match p.access.as_str() {
                "read" => Access::Read,
                "read-write" => Access::ReadWrite,
                _ => return err("the install record has a path grant with an unknown access"),
            };
            let scope = match p.path.as_str() {
                "$input" => Scope::Input,
                "*" => Scope::Any,
                s => Scope::Prefix(s.to_string()),
            };
            paths.push(PathGrant { scope, access });
        }
        let c = Capabilities { network: self.network, run_processes: self.run_processes, max_memory_mb: self.max_memory_mb, timeout_ms: self.timeout_ms, paths };
        c.validate().map_err(|e| RecordError(format!("the install record's capabilities are not valid: {e}")))?;
        Ok(c)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRecord {
    pub format: u32,
    pub id: String,
    pub version: String,
    /// The wasmtime version the `.cwasm` was compiled with.
    pub wasmtime: String,
    /// SHA-256 of `plugin.toml`, hex.
    pub manifest_sha256: String,
    /// SHA-256 of the package's `.wasm`, hex.
    pub wasm_sha256: String,
    /// SHA-256 of `component.cwasm`, hex.
    pub cwasm_sha256: String,
    /// What the user approved, resolved for this machine.
    pub capabilities: RecordCaps,
}

/// `<cache>\<id>\<version>`. Neither part can hold a separator, `.` or `..`
/// (the manifest parser checks both), so two plugins never share a folder (PH-10).
pub fn install_dir(cache: &Path, id: &str, version: &str) -> PathBuf {
    cache.join(id).join(version)
}

/// Reads at most `limit` bytes of `path`; a longer file is an error. Reads
/// through the open handle, so a file that grows after a size check can not
/// make it read more (PH-18).
pub fn read_capped(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(limit + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        return Err(std::io::Error::new(std::io::ErrorKind::FileTooLarge, format!("larger than {limit} bytes")));
    }
    Ok(buf)
}

/// A folder as the checks compare it: the real path (links resolved), normalized.
pub fn real_dir(p: &Path) -> Result<String, RecordError> {
    let real = std::fs::canonicalize(p).map_err(|e| RecordError(format!("{}: {e}", p.display())))?;
    let s = real.to_str().ok_or_else(|| RecordError(format!("{}: not a valid path", p.display())))?;
    caps::normalize(s).map_err(|e| RecordError(format!("{}: {e}", p.display())))
}

/// The folders a plugin installed from `plugin_dir` into `cache` may not
/// change (`extra` are the app's own: settings, exe folder), and the cache,
/// which it may not even read. `plugin_dir` and `cache` must exist.
pub fn protected_for(plugin_dir: &Path, cache: &Path, extra: &[String]) -> Result<Protected, RecordError> {
    let plugin = real_dir(plugin_dir)?;
    let cache = real_dir(cache)?;
    let mut no_write = vec![plugin, cache.clone()];
    no_write.extend(extra.iter().cloned());
    Protected::new(&no_write, &[cache]).map_err(|e| RecordError(format!("a protected folder: {e}")))
}

fn random_suffix() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    // `RandomState` is seeded from the OS's random source.
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.write_u32(std::process::id());
    h.finish()
}

/// Writes `data` to `dir\name`, which must not exist: through a new file
/// with a random name (`create_new`), then a hard link to the final name,
/// which fails if the name is taken. Never replaces or follows anything.
fn write_once(dir: &Path, name: &str, data: &[u8]) -> Result<(), RecordError> {
    let tmp = dir.join(format!(".{name}.{:016x}.tmp", random_suffix()));
    let target = dir.join(name);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::hard_link(&tmp, &target)
    })();
    let _ = std::fs::remove_file(&tmp);
    result.map_err(|e| RecordError(format!("{}: {e}", target.display())))
}

/// What [`store`] wrote.
#[derive(Clone, Debug)]
pub struct Stored {
    pub manifest: Manifest,
    /// `<cache>\<id>\<version>`.
    pub dir: PathBuf,
    pub record: InstallRecord,
}

/// The second half of an install, after the compile: checks the manifest
/// (the very bytes that are hashed), resolves and checks its capabilities,
/// and writes `component.cwasm` and the record. `manifest_bytes` and `wasm`
/// are what was read from the plugin folder, `cwasm` what the compiler made
/// of `wasm`. `data_dir` is the plugin's `$data` folder (needed only when it
/// asks for one). Refuses when the version is already installed.
pub fn store(plugin_dir: &Path, cache: &Path, data_dir: Option<&Path>, extra_protected: &[String], manifest_bytes: &[u8], wasm: &[u8], cwasm: &[u8]) -> Result<Stored, RecordError> {
    let text = std::str::from_utf8(manifest_bytes).map_err(|_| RecordError(format!("{MANIFEST_FILE}: not UTF-8")))?;
    let manifest = Manifest::parse(text).map_err(|e| RecordError(e.to_string()))?;
    std::fs::create_dir_all(cache).map_err(|e| RecordError(format!("{}: {e}", cache.display())))?;
    let protected = protected_for(plugin_dir, cache, extra_protected)?;
    let plugin = real_dir(plugin_dir)?;
    let data = match data_dir {
        Some(d) => {
            std::fs::create_dir_all(d).map_err(|e| RecordError(format!("{}: {e}", d.display())))?;
            real_dir(d)?
        }
        None => String::new(),
    };
    let caps = manifest.capabilities(&plugin, &data, &protected).map_err(|e| RecordError(e.to_string()))?;

    let dir = install_dir(cache, &manifest.id, &manifest.version);
    if dir.join(RECORD_FILE).symlink_metadata().is_ok() {
        return err(format!("{} {} is already installed in {}; remove that folder to install it again", manifest.id, manifest.version, dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| RecordError(format!("{}: {e}", dir.display())))?;
    // Left over from an install that stopped before its record: never trusted, replaced.
    let stale = dir.join(COMPONENT_FILE);
    if stale.symlink_metadata().is_ok() {
        std::fs::remove_file(&stale).map_err(|e| RecordError(format!("{}: {e}", stale.display())))?;
    }
    let record = InstallRecord {
        format: FORMAT,
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        wasmtime: WASMTIME_VERSION.into(),
        manifest_sha256: sha256::to_hex(&sha256::sha256(manifest_bytes)),
        wasm_sha256: sha256::to_hex(&sha256::sha256(wasm)),
        cwasm_sha256: sha256::to_hex(&sha256::sha256(cwasm)),
        capabilities: RecordCaps::from_caps(&caps),
    };
    let text = toml::to_string(&record).map_err(|e| RecordError(format!("can not write the install record: {e}")))?;
    write_once(&dir, COMPONENT_FILE, cwasm)?;
    // The record last: a folder with a record is a complete install.
    write_once(&dir, RECORD_FILE, text.as_bytes())?;
    Ok(Stored { manifest, dir, record })
}

/// An installed plugin whose files match its record: everything a `Load` needs.
#[derive(Clone, Debug)]
pub struct Verified {
    /// Parsed from the `plugin.toml` bytes that were hashed.
    pub manifest: Manifest,
    /// From the record, not from `plugin.toml`.
    pub caps: Capabilities,
    /// `<cache>\<id>\<version>\component.cwasm`.
    pub component: PathBuf,
    /// From the record; the host checks the `.cwasm` against it.
    pub cwasm_sha256: [u8; 32],
    /// The folders the plugin may not write (the cache among them, which it may not read either).
    pub protected: Vec<String>,
}

fn same_hash(what: &str, recorded: &str, data: &[u8]) -> Result<(), RecordError> {
    match sha256::from_hex(recorded) {
        Some(h) if h == sha256::sha256(data) => Ok(()),
        Some(_) => err(format!("{what} changed since it was installed; install the plugin again to approve the change")),
        None => err(format!("the install record's {what} hash is not valid")),
    }
}

/// Checks an installed plugin before a start: reads its record from
/// `<cache>\<id>\<version>`, checks that `plugin.toml` and the `.wasm` in
/// `plugin_dir` still have the recorded hashes, and returns the recorded
/// capabilities. Any difference is refused: a changed plugin must be
/// installed (and approved) again.
pub fn verify(plugin_dir: &Path, cache: &Path, id: &str, version: &str, extra_protected: &[String]) -> Result<Verified, RecordError> {
    let dir = install_dir(cache, id, version);
    let raw = read_capped(&dir.join(RECORD_FILE), MAX_RECORD_BYTES).map_err(|e| RecordError(format!("{} {version} is not installed (no readable install record: {e})", id)))?;
    let text = std::str::from_utf8(&raw).map_err(|_| RecordError("the install record is not UTF-8".into()))?;
    let record: InstallRecord = toml::from_str(text).map_err(|e| RecordError(format!("the install record does not parse: {}", crate::sanitize::sanitize(e.message(), 200))))?;
    if record.format != FORMAT {
        return err(format!("the install record has format {}, this build reads {FORMAT}", record.format));
    }
    if record.id != id || record.version != version {
        return err("the install record is for another plugin");
    }
    if record.wasmtime != WASMTIME_VERSION {
        return err(format!("the plugin was compiled for wasmtime {}, this host is {WASMTIME_VERSION}; install it again", crate::sanitize::sanitize(&record.wasmtime, 40)));
    }

    // Hash and parse the same bytes: the file is read once.
    let manifest_bytes = read_capped(&plugin_dir.join(MANIFEST_FILE), MAX_MANIFEST_BYTES).map_err(|e| RecordError(format!("{MANIFEST_FILE}: {e}")))?;
    same_hash(MANIFEST_FILE, &record.manifest_sha256, &manifest_bytes)?;
    let manifest = Manifest::parse(std::str::from_utf8(&manifest_bytes).map_err(|_| RecordError(format!("{MANIFEST_FILE}: not UTF-8")))?).map_err(|e| RecordError(e.to_string()))?;
    if manifest.id != record.id || manifest.version != record.version {
        return err("the install record is for another plugin");
    }
    let wasm = read_capped(&plugin_dir.join(&manifest.component), MAX_WASM_BYTES).map_err(|e| RecordError(format!("{}: {e}", manifest.component)))?;
    same_hash("the component (.wasm)", &record.wasm_sha256, &wasm)?;
    drop(wasm);

    let caps = record.capabilities.to_caps()?;
    let protected = protected_for(plugin_dir, cache, extra_protected)?;
    protected.check_grants(&caps).map_err(|e: CapError| RecordError(format!("the install record's capabilities: {e}")))?;
    let cwasm_sha256 = sha256::from_hex(&record.cwasm_sha256).ok_or_else(|| RecordError("the install record's .cwasm hash is not valid".into()))?;
    // The component by its real path: the host seals the folder it is in and
    // compares that with the final path of every handle.
    let component = install_dir(Path::new(&real_dir(cache)?), id, version).join(COMPONENT_FILE);
    Ok(Verified { manifest, caps, component, cwasm_sha256, protected: protected.no_write().to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wasmtime_version_matches_cargo_toml() {
        let toml = include_str!("../Cargo.toml");
        assert!(toml.contains(&format!("wasmtime = {{ version = \"={WASMTIME_VERSION}\"")), "update WASMTIME_VERSION");
    }

    #[test]
    fn record_caps_round_trip() {
        let c = Capabilities {
            network: true,
            run_processes: false,
            max_memory_mb: 32,
            timeout_ms: 500,
            paths: vec![
                PathGrant { scope: Scope::Prefix("C:\\Data".into()), access: Access::ReadWrite },
                PathGrant { scope: Scope::Input, access: Access::Read },
                PathGrant { scope: Scope::Any, access: Access::Read },
            ],
        };
        assert_eq!(RecordCaps::from_caps(&c).to_caps().unwrap(), c);
        let bad = RecordCaps { paths: vec![RecordPath { path: "C:\\a\\..\\b".into(), access: "read".into() }], ..RecordCaps::from_caps(&Capabilities::default()) };
        assert!(bad.to_caps().is_err());
    }

    #[test]
    fn read_capped_refuses_a_longer_file() {
        let p = std::env::temp_dir().join(format!("tsubame-capped-{}", std::process::id()));
        std::fs::write(&p, [0u8; 11]).unwrap();
        assert_eq!(read_capped(&p, 11).unwrap().len(), 11);
        assert!(read_capped(&p, 10).is_err());
        let _ = std::fs::remove_file(&p);
    }
}
