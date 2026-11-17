//! Tsubame self-update: check GitHub Releases, verify a signed manifest, stage the new
//! exes, swap them in by rename, keep the previous version for rollback.
//!
//! Design and threat model: `docs/updater.md`. Everything here is blocking and meant for
//! a worker thread; the library never touches any UI.
//!
//! Typical use (what the app's Settings > Updates screen does):
//! ```ignore
//! let up = tsubame_update::Updater::production(env!("CARGO_PKG_VERSION"))?;
//! // worker thread:
//! if let Some(rel) = up.check(false)? {            // false = honour the 24 h limit
//!     let staged = up.download(&rel, &mut |p| { report(p.done, p.total); true })?;
//!     up.apply(&staged)?;                           // files swapped, old exe kept as .old
//!     up.relaunch_watched(&[], std::time::Duration::from_secs(30))?;  // then exit this process
//! }
//! ```

pub mod authenticode;
pub mod error;
pub mod hash;
pub mod http;
pub mod keys;
pub mod layout;
pub mod manifest;
pub mod state;
pub mod version;
pub mod zip;

// The DEV key pair exists for tests and for the maintainer tool's `--dev-test-key`. It is only
// ever *trusted* (added to the accepted keys) under cfg(test) / `test-server`.
#[cfg(any(test, feature = "test-server", feature = "release-tool"))]
pub mod devkey;
#[cfg(any(test, feature = "test-server"))]
pub mod testserver;

#[cfg(all(feature = "test-server", not(debug_assertions)))]
compile_error!("the `test-server` feature allows plain http and trusts a public DEV key: it must never be built in release mode");

pub use error::{Error, Result};
pub use layout::Layout;
pub use version::Version;

use ed25519_compact::PublicKey;
use keys::Pin;
use serde::{Deserialize, Serialize};
use state::{check_due, now_unix, Previous, State, Trial, BACKOFF_RATE_LIMIT_SECS, RETRY_AFTER_FAILURE_SECS};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The one place that names the GitHub repository.
pub const DEFAULT_REPO: &str = "watdafawx/tsubame";
const API_BASE: &str = "https://api.github.com";
const DOWNLOAD_BASE: &str = "https://github.com";
const API_MAX_BYTES: u64 = 1024 * 1024;

/// Everything the updater needs to know. Build it with [`Config::production`].
#[derive(Clone)]
pub struct Config {
    repo: String,
    api_base: String,
    download_base: String,
    allow_http_loopback: bool,
    layout: Layout,
    current: Version,
    keys: Vec<PublicKey>,
    pins: Vec<Pin>,
}

impl Config {
    /// The real thing: GitHub over https, the pinned release key, the pinned signers.
    pub fn production(current_version: &str, layout: Layout) -> Result<Config> {
        Ok(Config {
            repo: DEFAULT_REPO.to_string(),
            api_base: API_BASE.to_string(),
            download_base: DOWNLOAD_BASE.to_string(),
            allow_http_loopback: false,
            layout,
            current: Version::parse(current_version)?,
            keys: {
                #[allow(unused_mut)]
                let mut k: Vec<PublicKey> = keys::RELEASE_PUBLIC_KEYS_HEX.iter().filter_map(|h| keys::parse_public_key(h).ok()).collect();
                // The DEV key is trusted only in test builds: `trusted_keys()` adds it under cfg(test|test-server).
                #[cfg(any(test, feature = "test-server"))]
                k.push(devkey::dev_public_key());
                k
            },
            pins: keys::PINNED_SIGNERS.to_vec(),
        })
    }

    /// Test fixture: any base URL (http://127.0.0.1:port), the DEV key. Not in release builds.
    #[cfg(any(test, feature = "test-server"))]
    pub fn for_test(current_version: &str, layout: Layout, base_url: &str, repo: &str) -> Result<Config> {
        let mut c = Config::production(current_version, layout)?;
        c.repo = repo.to_string();
        c.api_base = base_url.trim_end_matches('/').to_string();
        c.download_base = base_url.trim_end_matches('/').to_string();
        c.allow_http_loopback = true;
        Ok(c)
    }

    #[cfg(any(test, feature = "test-server"))]
    pub fn with_pins(mut self, pins: Vec<Pin>) -> Config {
        self.pins = pins;
        self
    }

    #[cfg(any(test, feature = "test-server"))]
    pub fn with_keys(mut self, keys: Vec<PublicKey>) -> Config {
        self.keys = keys;
        self
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }
    pub fn current_version(&self) -> &Version {
        &self.current
    }
    pub fn repo(&self) -> &str {
        &self.repo
    }
}

/// A verified, newer release that can be downloaded.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub released: String,
    pub notes_url: String,
    pub min_supported_version: Version,
    /// False when the installed version is older than `min_supported_version`: an in-place
    /// update is refused, the user must run the new installer.
    pub in_place_ok: bool,
    pub download_file: String,
    pub download_size: u64,
    pub manifest_sha256: String,
    verified: manifest::Verified,
}

#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StagedFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// A downloaded, verified update waiting to be applied. Persisted next to the files so a
/// later run (or the CLI) can apply it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Staged {
    pub version: String,
    pub min_supported_version: String,
    pub manifest_sha256: String,
    /// Directory holding the extracted files (inside `update\staging`).
    pub dir: PathBuf,
    pub files: Vec<StagedFile>,
}

#[derive(Clone, Debug)]
pub struct Applied {
    pub version: String,
    pub previous_version: String,
    pub replaced: Vec<String>,
    /// The exe to start now.
    pub exe: PathBuf,
}

#[derive(Clone, Debug)]
pub struct RolledBack {
    pub from: String,
    pub to: String,
    pub restored: Vec<String>,
}

/// What `on_start` found. Call it first thing in `main`.
#[derive(Clone, Debug, PartialEq)]
pub enum Startup {
    /// Nothing special.
    Normal,
    /// First start after an update. Call `on_ready` once the window has painted.
    Trial { version: String, previous: String },
    /// A start of this version already happened and never reached `on_ready`: it crashed.
    /// Offer "go back to <previous>" (`rollback`); at `boots >= 3` do it without asking.
    CrashedAfterUpdate { version: String, previous: String, boots: u32 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum WatchOutcome {
    /// The new version started and called `on_ready`.
    Started { pid: u32 },
    /// It exited or hung before it was ready; the previous version is back (and was started again).
    RolledBack { reason: &'static str },
}

/// For a "Settings > Updates" screen.
#[derive(Clone, Debug)]
pub struct Status {
    pub running: String,
    pub previous: Option<String>,
    pub last_check_unix: u64,
    pub staged: Option<Staged>,
}

pub struct Updater {
    cfg: Config,
}

impl Updater {
    pub fn new(cfg: Config) -> Updater {
        Updater { cfg }
    }

    /// Production config for the directory of the running exe.
    pub fn production(current_version: &str) -> Result<Updater> {
        Ok(Updater::new(Config::production(current_version, Layout::of_current_exe()?)?))
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn opts(&self, max_bytes: u64) -> http::Opts {
        http::Opts { allow_http_loopback: self.cfg.allow_http_loopback, max_bytes }
    }

    fn api_url(&self) -> String {
        format!("{}/repos/{}/releases/latest", self.cfg.api_base, self.cfg.repo)
    }

    fn asset_url(&self, tag: &str, file: &str) -> String {
        format!("{}/{}/releases/download/{}/{}", self.cfg.download_base, self.cfg.repo, tag, file)
    }

    // ---- check ---------------------------------------------------------------------

    /// Ask for the latest stable release. `force=false` honours the 24 h limit (answering from
    /// the verified cache in between); `force=true` is the "Check now" button (still obeys a
    /// rate-limit back-off). `Ok(None)`: up to date.
    pub fn check(&self, force: bool) -> Result<Option<Release>> {
        let l = &self.cfg.layout;
        let mut st = State::load(l);
        let now = now_unix();
        if !check_due(&st, now, force) {
            return self.from_cache(&mut st);
        }
        st.last_attempt_unix = now;
        let r = self.fetch_latest(&mut st);
        st.next_attempt_unix = match &r {
            Err(Error::RateLimited) => now + BACKOFF_RATE_LIMIT_SECS,
            _ => 0,
        };
        if r.is_err() && st.next_attempt_unix == 0 {
            // a failed check is retried after an hour (not a day) by the automatic path;
            // the user's "Check now" is never blocked by it
            st.last_attempt_unix = now.saturating_sub(state::CHECK_INTERVAL_SECS - RETRY_AFTER_FAILURE_SECS);
        }
        let _ = st.save(l);
        r
    }

    fn cache_paths(&self) -> (PathBuf, PathBuf) {
        let d = self.cfg.layout.cache_dir();
        (d.join(manifest::MANIFEST_FILE), d.join(manifest::SIGNATURE_FILE))
    }

    fn from_cache(&self, st: &mut State) -> Result<Option<Release>> {
        let (mp, sp) = self.cache_paths();
        let (Ok(m), Ok(s)) = (std::fs::read(&mp), std::fs::read(&sp)) else { return Ok(None) };
        let Some(tag) = st.latest_tag.clone() else { return Ok(None) };
        match manifest::verify(&m, &s, &self.cfg.keys) {
            Ok(v) => self.release_from(v, &tag, st),
            Err(_) => {
                // tampered / stale cache: drop it and ask the network next time
                let _ = std::fs::remove_file(&mp);
                let _ = std::fs::remove_file(&sp);
                st.etag = None;
                st.latest_tag = None;
                st.last_attempt_unix = 0;
                let _ = st.save(&self.cfg.layout);
                Ok(None)
            }
        }
    }

    fn fetch_latest(&self, st: &mut State) -> Result<Option<Release>> {
        let (mp, sp) = self.cache_paths();
        let have_cache = mp.is_file() && sp.is_file() && st.latest_tag.is_some();
        let etag = if have_cache { st.etag.clone() } else { None };
        let (resp, body) = http::get_bytes(&self.api_url(), etag.as_deref(), self.opts(API_MAX_BYTES))?;
        match resp.status {
            304 => self.from_cache(st),
            200 => {
                #[derive(Deserialize)]
                struct Latest {
                    tag_name: String,
                    #[serde(default)]
                    draft: bool,
                    #[serde(default)]
                    prerelease: bool,
                }
                let l: Latest = serde_json::from_slice(&body).map_err(|e| Error::Parse(format!("release JSON: {e}")))?;
                if l.draft || l.prerelease {
                    st.etag = resp.etag;
                    st.latest_tag = None;
                    return Ok(None); // stable channel only
                }
                let tag_version = version::parse_tag(&l.tag_name)?;
                let get_small = |file: &str| -> Result<Vec<u8>> {
                    let (r, b) = http::get_bytes(&self.asset_url(&l.tag_name, file), None, self.opts(manifest::MAX_MANIFEST_BYTES))?;
                    if r.status != 200 {
                        return Err(Error::Http(r.status));
                    }
                    Ok(b)
                };
                let mbytes = get_small(manifest::MANIFEST_FILE)?;
                let sbytes = get_small(manifest::SIGNATURE_FILE)?;
                let v = manifest::verify(&mbytes, &sbytes, &self.cfg.keys)?;
                if v.version != tag_version {
                    return Err(Error::Parse(format!("manifest says {} but the release tag is {}", v.version, l.tag_name)));
                }
                let _ = layout::write_atomic(&mp, &mbytes);
                let _ = layout::write_atomic(&sp, &sbytes);
                st.etag = resp.etag;
                st.latest_tag = Some(l.tag_name.clone());
                self.release_from(v, &l.tag_name, st)
            }
            403 | 429 => Err(Error::RateLimited),
            404 => {
                st.etag = None;
                st.latest_tag = None;
                Ok(None)
            }
            c => Err(Error::Http(c)),
        }
    }

    fn release_from(&self, v: manifest::Verified, tag: &str, st: &State) -> Result<Option<Release>> {
        let installed = installed_version(&self.cfg, st);
        if v.version <= installed || st.is_skipped(&v.version.to_string()) {
            return Ok(None);
        }
        let Some(asset) = v.update_asset() else {
            return Err(Error::Parse("release has no portable x64 asset".into()));
        };
        Ok(Some(Release {
            version: v.version.clone(),
            tag: tag.to_string(),
            released: v.manifest.released.clone(),
            notes_url: v.manifest.notes_url.clone(),
            min_supported_version: v.min_supported.clone(),
            in_place_ok: installed >= v.min_supported,
            download_file: asset.file.clone(),
            download_size: asset.size,
            manifest_sha256: v.manifest_sha256.clone(),
            verified: v,
        }))
    }

    // ---- download ------------------------------------------------------------------

    /// Download the release zip into `update\staging\<version>\`, verify size and SHA-256
    /// against the signed manifest, extract only the files the manifest lists (each checked
    /// again), and, when signers are pinned, Authenticode-verify them. `progress` returns
    /// false to cancel.
    pub fn download(&self, rel: &Release, progress: &mut dyn FnMut(Progress) -> bool) -> Result<Staged> {
        if !rel.in_place_ok {
            return Err(Error::TooOld { min: rel.min_supported_version.to_string() });
        }
        let st = State::load(&self.cfg.layout);
        if rel.version <= installed_version(&self.cfg, &st) {
            return Err(Error::NotNewer);
        }
        let l = &self.cfg.layout;
        let version = rel.version.to_string();
        let dir = l.staging_dir(&version);
        let _ = std::fs::remove_dir_all(&dir);
        let r = self.download_into(rel, &dir, progress);
        if r.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        r
    }

    fn download_into(&self, rel: &Release, dir: &Path, progress: &mut dyn FnMut(Progress) -> bool) -> Result<Staged> {
        use std::io::Write;
        let l = &self.cfg.layout;
        let asset = rel.verified.update_asset().ok_or_else(|| Error::Parse("no portable asset".into()))?;
        let files_dir = dir.join("files");
        std::fs::create_dir_all(&files_dir)?;
        let part = dir.join(format!("{}.part", asset.file));
        let mut f = std::fs::File::create(&part)?;
        let mut h = hash::Sha256::new()?;
        let mut done = 0u64;
        let mut io_err: Option<Error> = None;
        let url = self.asset_url(&rel.tag, &asset.file);
        let resp = http::get(&url, None, self.opts(asset.size), &mut |chunk, _| {
            if let Err(e) = f.write_all(chunk).map_err(Error::from).and_then(|_| h.update(chunk)) {
                io_err = Some(e);
                return false;
            }
            done += chunk.len() as u64;
            progress(Progress { done, total: asset.size })
        });
        if let Some(e) = io_err {
            return Err(e);
        }
        let resp = resp?;
        if resp.status != 200 {
            return Err(Error::Http(resp.status));
        }
        drop(f);
        if done != asset.size {
            return Err(Error::SizeMismatch { what: asset.file.clone() });
        }
        if hash::to_hex(&h.finish()?) != asset.sha256.to_ascii_lowercase() {
            return Err(Error::HashMismatch { what: asset.file.clone() });
        }
        // verified: now (and only now) look inside the zip
        let bytes = std::fs::read(&part)?;
        let zip = zip::Zip::parse(&bytes)?;
        let mut files = Vec::new();
        for c in &asset.contents {
            let data = zip.read(&c.name, c.size as usize)?;
            if data.len() as u64 != c.size {
                return Err(Error::SizeMismatch { what: c.name.clone() });
            }
            if hash::sha256_hex(&data)? != c.sha256.to_ascii_lowercase() {
                return Err(Error::HashMismatch { what: c.name.clone() });
            }
            std::fs::write(files_dir.join(&c.name), &data)?;
            files.push(StagedFile { name: c.name.clone(), size: c.size, sha256: c.sha256.to_ascii_lowercase() });
        }
        drop(bytes);
        let _ = std::fs::remove_file(&part);
        if !self.cfg.pins.is_empty() {
            for sf in &files {
                authenticode::verify_pinned(&files_dir.join(&sf.name), &self.cfg.pins)?;
            }
        }
        let staged = Staged {
            version: rel.version.to_string(),
            min_supported_version: rel.min_supported_version.to_string(),
            manifest_sha256: rel.manifest_sha256.clone(),
            dir: files_dir,
            files,
        };
        let json = serde_json::to_vec_pretty(&staged).map_err(|e| Error::Parse(e.to_string()))?;
        layout::write_atomic(&l.staged_json(&staged.version), &json)?;
        Ok(staged)
    }

    /// The newest staged update on disk, if any (for "apply on next start").
    pub fn staged(&self) -> Option<Staged> {
        let st = State::load(&self.cfg.layout);
        let installed = installed_version(&self.cfg, &st);
        let mut best: Option<(Version, Staged)> = None;
        for e in std::fs::read_dir(self.cfg.layout.staging_root()).ok()?.flatten() {
            let Ok(bytes) = std::fs::read(e.path().join("staged.json")) else { continue };
            let Ok(s) = serde_json::from_slice::<Staged>(&bytes) else { continue };
            let Ok(v) = Version::parse(&s.version) else { continue };
            if v > installed && best.as_ref().is_none_or(|(b, _)| v > *b) {
                best = Some((v, s));
            }
        }
        best.map(|(_, s)| s)
    }

    // ---- apply ---------------------------------------------------------------------

    /// Swap the staged files in: copy to `<name>.new`, rename the running `<name>` to
    /// `<name>.old`, rename `.new` into place. Any failure puts everything back.
    /// Afterwards start the new exe (`relaunch_watched`) and exit.
    pub fn apply(&self, staged: &Staged) -> Result<Applied> {
        let l = &self.cfg.layout;
        let new_version = Version::parse(&staged.version)?;
        let mut st = State::load(l);
        let installed = installed_version(&self.cfg, &st);
        if new_version <= installed {
            return Err(Error::NotNewer);
        }
        if installed < Version::parse(&staged.min_supported_version)? {
            return Err(Error::TooOld { min: staged.min_supported_version.clone() });
        }
        if !l.exe(manifest::MAIN_EXE).is_file() {
            return Err(Error::Install(format!("{} is not an installed copy of Tsubame ({} not found)", l.install_dir.display(), manifest::MAIN_EXE)));
        }
        // the staged dir must be ours, not an arbitrary path from a hand-made staged.json
        let staging_root = l.staging_root();
        if !staged.dir.starts_with(&staging_root) || staged.dir.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(Error::Install("staged files are not inside the update staging folder".into()));
        }
        // which files: tsubame.exe always; others only if they are already installed
        let mut names: Vec<&StagedFile> = Vec::new();
        for f in &staged.files {
            if !manifest::UPDATABLE_FILES.contains(&f.name.as_str()) {
                return Err(Error::Install(format!("refusing to install unexpected file {:?}", f.name)));
            }
            if f.name == manifest::MAIN_EXE || l.exe(&f.name).is_file() {
                names.push(f);
            }
        }
        if !names.iter().any(|f| f.name == manifest::MAIN_EXE) {
            return Err(Error::Nothing("staged update does not contain tsubame.exe"));
        }
        // re-verify what is on disk right before using it
        for f in &names {
            let p = staged.dir.join(&f.name);
            if hash::sha256_file(&p)? != f.sha256 {
                return Err(Error::HashMismatch { what: f.name.clone() });
            }
        }
        // a previous update's .old that is still the running image cannot be deleted
        for f in &names {
            let old = l.old(&f.name);
            if old.exists() && std::fs::remove_file(&old).is_err() {
                return Err(Error::Install(format!("{} is still in use; restart Tsubame once, then update again", old.display())));
            }
        }
        // copy next to the install (same volume) and verify the copy
        let mut copied: Vec<&str> = Vec::new();
        let cleanup_new = |copied: &[&str]| {
            for n in copied {
                let _ = std::fs::remove_file(l.new_(n));
            }
        };
        for f in &names {
            let np = l.new_(&f.name);
            let r = std::fs::copy(staged.dir.join(&f.name), &np).map_err(Error::from).and_then(|_| {
                if hash::sha256_file(&np)? == f.sha256 {
                    Ok(())
                } else {
                    Err(Error::HashMismatch { what: format!("{} (copy)", f.name) })
                }
            });
            copied.push(&f.name);
            if let Err(e) = r {
                cleanup_new(&copied);
                return Err(match e {
                    Error::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
                        Error::Install(format!("{} is not writable; update needs the per-user install (no admin prompt is ever shown)", l.install_dir.display()))
                    }
                    e => e,
                });
            }
        }
        // swap
        let mut swapped: Vec<&str> = Vec::new();
        for f in &names {
            let step = std::fs::rename(l.exe(&f.name), l.old(&f.name)).and_then(|_| match std::fs::rename(l.new_(&f.name), l.exe(&f.name)) {
                Ok(()) => Ok(()),
                Err(e) => {
                    let _ = std::fs::rename(l.old(&f.name), l.exe(&f.name)); // this file: put the old one back
                    Err(e)
                }
            });
            match step {
                Ok(()) => swapped.push(&f.name),
                Err(e) => {
                    for n in swapped.iter().rev() {
                        let _ = std::fs::rename(l.exe(n), l.new_(n));
                        let _ = std::fs::rename(l.old(n), l.exe(n));
                    }
                    cleanup_new(&copied);
                    return Err(Error::Install(format!("could not swap {}: {e}", f.name)));
                }
            }
        }
        let applied = Applied {
            version: staged.version.clone(),
            previous_version: installed.to_string(),
            replaced: swapped.iter().map(|s| s.to_string()).collect(),
            exe: l.exe(manifest::MAIN_EXE),
        };
        st.previous = Some(Previous { version: installed.to_string(), files: applied.replaced.clone(), at_unix: now_unix() });
        st.trial = Some(Trial { version: staged.version.clone(), previous: installed.to_string(), boots: 0 });
        st.installed_version = Some(staged.version.clone());
        let _ = st.save(l); // the files are swapped already; a lost state only costs the rollback hint
        let _ = std::fs::remove_dir_all(l.staging_dir(&staged.version));
        Ok(applied)
    }

    // ---- rollback ------------------------------------------------------------------

    /// One-click rollback: put the `.old` files back. The version rolled back from is not
    /// offered again. Afterwards restart (`relaunch`).
    pub fn rollback(&self) -> Result<RolledBack> {
        let l = &self.cfg.layout;
        let mut st = State::load(l);
        let prev = st.previous.clone().ok_or(Error::Nothing("there is no previous version to go back to"))?;
        for n in &prev.files {
            if !manifest::UPDATABLE_FILES.contains(&n.as_str()) || !l.old(n).is_file() {
                return Err(Error::Nothing("the previous version's files are missing"));
            }
        }
        for n in &prev.files {
            let bad = l.bad(n);
            if bad.exists() && std::fs::remove_file(&bad).is_err() {
                return Err(Error::Install(format!("{} is still in use; restart Tsubame once, then try again", bad.display())));
            }
        }
        let mut done: Vec<&str> = Vec::new();
        for n in &prev.files {
            let step = (|| -> std::io::Result<()> {
                if l.exe(n).exists() {
                    std::fs::rename(l.exe(n), l.bad(n))?;
                }
                if let Err(e) = std::fs::rename(l.old(n), l.exe(n)) {
                    let _ = std::fs::rename(l.bad(n), l.exe(n));
                    return Err(e);
                }
                Ok(())
            })();
            match step {
                Ok(()) => done.push(n),
                Err(e) => {
                    for d in done.iter().rev() {
                        let _ = std::fs::rename(l.exe(d), l.old(d));
                        let _ = std::fs::rename(l.bad(d), l.exe(d));
                    }
                    return Err(Error::Install(format!("rollback of {n} failed: {e}")));
                }
            }
        }
        let from = st.installed_version.clone().unwrap_or_else(|| self.cfg.current.to_string());
        st.skip(&from);
        st.installed_version = Some(prev.version.clone());
        st.previous = None;
        st.trial = None;
        let _ = st.save(l);
        Ok(RolledBack { from, to: prev.version, restored: prev.files })
    }

    /// The previous version kept for rollback, if any.
    pub fn previous_version(&self) -> Option<String> {
        State::load(&self.cfg.layout).previous.map(|p| p.version)
    }

    /// Delete the kept previous version (frees the space; disables rollback).
    pub fn discard_previous(&self) -> Result<()> {
        let l = &self.cfg.layout;
        let mut st = State::load(l);
        if let Some(p) = st.previous.take() {
            for n in &p.files {
                let _ = std::fs::remove_file(l.old(n));
            }
            st.save(l)?;
        }
        Ok(())
    }

    // ---- start / ready / relaunch ----------------------------------------------------

    /// Call first thing in `main`. Also removes leftovers (`.bad`, `.new`, stale staging).
    pub fn on_start(&self) -> Startup {
        let l = &self.cfg.layout;
        let mut st = State::load(l);
        let running = self.cfg.current.to_string();
        self.cleanup_leftovers(&st);
        match st.trial.clone() {
            Some(mut t) if t.version == running => {
                t.boots += 1;
                let boots = t.boots;
                st.trial = Some(t.clone());
                st.installed_version = Some(running);
                let _ = st.save(l);
                if boots <= 1 {
                    Startup::Trial { version: t.version, previous: t.previous }
                } else {
                    Startup::CrashedAfterUpdate { version: t.version, previous: t.previous, boots }
                }
            }
            other => {
                // no trial, or a trial for a different version (reinstalled/rolled back by hand): drop it
                let mut dirty = other.is_some();
                st.trial = None;
                if st.installed_version.as_deref() != Some(running.as_str()) {
                    st.installed_version = Some(running);
                    dirty = true;
                }
                if dirty {
                    let _ = st.save(l);
                }
                Startup::Normal
            }
        }
    }

    /// Call once the window has painted (the update "took"). Ends the trial.
    pub fn on_ready(&self) {
        let l = &self.cfg.layout;
        let mut st = State::load(l);
        if st.trial.take().is_some() {
            let _ = st.save(l);
        }
        self.cleanup_leftovers(&st);
    }

    fn cleanup_leftovers(&self, st: &State) {
        let l = &self.cfg.layout;
        for n in manifest::UPDATABLE_FILES {
            let _ = std::fs::remove_file(l.bad(n));
            let _ = std::fs::remove_file(l.new_(n));
        }
        // staged updates that are not newer than what runs are dead weight
        let installed = installed_version(&self.cfg, st);
        if let Ok(rd) = std::fs::read_dir(l.staging_root()) {
            for e in rd.flatten() {
                let stale = e.file_name().to_str().and_then(|n| Version::parse(n).ok()).is_none_or(|v| v <= installed);
                if stale {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }

    /// Start the (new) exe from its final path with `--after-update <pid>` plus `args`,
    /// without a shell. Returns the child pid. The caller exits afterwards.
    pub fn relaunch(&self, args: &[OsString]) -> Result<u32> {
        Ok(spawn(&self.cfg.layout.exe(manifest::MAIN_EXE), Some(std::process::id()), args)?.id())
    }

    /// `relaunch` + watchdog: wait until the new process calls `on_ready` (trial cleared).
    /// If it exits first, or is not ready after `ready_timeout`, kill it, roll back, start
    /// the previous version again. Call from the old process after its window is gone.
    pub fn relaunch_watched(&self, args: &[OsString], ready_timeout: Duration) -> Result<WatchOutcome> {
        let l = &self.cfg.layout;
        let mut child = match spawn(&l.exe(manifest::MAIN_EXE), Some(std::process::id()), args) {
            Ok(c) => c,
            Err(_) => {
                self.rollback()?;
                spawn(&l.exe(manifest::MAIN_EXE), None, args)?;
                return Ok(WatchOutcome::RolledBack { reason: "the new version could not be started" });
            }
        };
        let t0 = std::time::Instant::now();
        loop {
            let exited = child.try_wait()?.is_some();
            let ready = State::load(l).trial.is_none();
            match watch_decision(exited, ready, t0.elapsed(), ready_timeout) {
                Watch::Wait => std::thread::sleep(Duration::from_millis(150)),
                Watch::Ready => return Ok(WatchOutcome::Started { pid: child.id() }),
                Watch::Rollback(reason) => {
                    if !exited {
                        let _ = child.kill();
                    }
                    let _ = child.wait();
                    self.rollback()?;
                    spawn(&l.exe(manifest::MAIN_EXE), None, args)?;
                    return Ok(WatchOutcome::RolledBack { reason });
                }
            }
        }
    }

    pub fn status(&self) -> Status {
        let st = State::load(&self.cfg.layout);
        Status {
            running: self.cfg.current.to_string(),
            previous: st.previous.as_ref().map(|p| p.version.clone()),
            last_check_unix: st.last_attempt_unix,
            staged: self.staged(),
        }
    }
}

fn installed_version(cfg: &Config, st: &State) -> Version {
    // The running binary is the authority; state only matters for the CLI, which passes the
    // installed version in `cfg.current` itself.
    let _ = st;
    cfg.current.clone()
}

#[derive(Debug, PartialEq)]
pub enum Watch {
    Wait,
    Ready,
    Rollback(&'static str),
}

/// Pure decision for `relaunch_watched`.
pub fn watch_decision(child_exited: bool, ready: bool, elapsed: Duration, timeout: Duration) -> Watch {
    if ready {
        Watch::Ready
    } else if child_exited {
        Watch::Rollback("the new version exited before it was ready")
    } else if elapsed >= timeout {
        Watch::Rollback("the new version did not become ready in time")
    } else {
        Watch::Wait
    }
}

fn spawn(exe: &Path, after_update: Option<u32>, args: &[OsString]) -> Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000; // only matters for console exes (tests)
    let mut c = Command::new(exe);
    if let Some(pid) = after_update {
        c.arg("--after-update").arg(pid.to_string());
    }
    c.args(args);
    if let Some(d) = exe.parent() {
        c.current_dir(d);
    }
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW);
    c.spawn().map_err(|e| Error::Install(format!("cannot start {}: {e}", exe.display())))
}

/// Run `check` on a worker thread; `done` is called there with the result.
pub fn spawn_check(up: std::sync::Arc<Updater>, force: bool, done: impl FnOnce(Result<Option<Release>>) + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || done(up.check(force)))
}

#[cfg(test)]
mod tests;
