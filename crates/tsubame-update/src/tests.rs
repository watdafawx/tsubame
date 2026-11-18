//! End-to-end tests: a fake GitHub (`testserver`) serving a fake signed release, the real
//! WinHTTP client, the real staging/swap/rollback code on throw-away directories.

use super::*;
use crate::devkey::dev_keypair;
use crate::manifest::tests::sample;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);
const REPO: &str = "test/tsubame";

struct Fx {
    root: PathBuf,
    rel: PathBuf,
    app: PathBuf,
    data: PathBuf,
    server: testserver::Server,
}

impl Fx {
    fn new() -> Fx {
        let root = std::env::temp_dir().join(format!("tsu-e2e-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let (rel, app, data) = (root.join("release"), root.join("app"), root.join("data"));
        for d in [&rel, &app, &data] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(app.join("tsubame.exe"), b"OLD-EXE").unwrap();
        let server = testserver::start(rel.clone(), REPO, 0).unwrap();
        Fx { root, rel, app, data, server }
    }

    fn cfg(&self, current: &str) -> Config {
        Config::for_test(current, Layout::new(&self.app, &self.data), &self.server.base, REPO).unwrap()
    }

    fn updater(&self, current: &str) -> Updater {
        Updater::new(self.cfg(current))
    }

    fn publish(&self, version: &str, exe: &[u8], min: &str) {
        self.publish_signed(version, exe, min, &dev_keypair());
    }

    fn publish_signed(&self, version: &str, exe: &[u8], min: &str, key: &ed25519_compact::KeyPair) {
        let zip = zip::build_zip(&[("tsubame.exe", exe), ("README.txt", b"hello")], true);
        std::fs::write(self.rel.join(format!("tsubame-{version}-win-x64.zip")), &zip).unwrap();
        let mut m = sample(version);
        m.min_supported_version = min.into();
        m.assets[0].size = zip.len() as u64;
        m.assets[0].sha256 = hash::sha256_hex(&zip).unwrap();
        m.assets[0].contents[0].size = exe.len() as u64;
        m.assets[0].contents[0].sha256 = hash::sha256_hex(exe).unwrap();
        let bytes = serde_json::to_vec_pretty(&m).unwrap();
        let sig = hash::to_hex(key.sk.sign(keys::signed_message(&bytes), None).as_ref());
        std::fs::write(self.rel.join(manifest::MANIFEST_FILE), &bytes).unwrap();
        std::fs::write(self.rel.join(manifest::SIGNATURE_FILE), sig).unwrap();
    }

    fn exe(&self) -> Vec<u8> {
        std::fs::read(self.app.join("tsubame.exe")).unwrap()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn no_progress(_: Progress) -> bool {
    true
}

#[test]
fn full_cycle_check_download_apply_start_rollback() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW-EXE-0.2.0", "0.1.0");
    let up = fx.updater("0.1.0");

    let rel = up.check(false).unwrap().expect("0.2.0 is newer");
    assert_eq!(rel.version.to_string(), "0.2.0");
    assert!(rel.in_place_ok);
    assert_eq!(rel.tag, "v0.2.0");

    let mut last = 0;
    let staged = up
        .download(&rel, &mut |p| {
            last = p.done;
            assert!(p.done <= p.total);
            true
        })
        .unwrap();
    assert_eq!(last, rel.download_size);
    assert_eq!(staged.files.len(), 1);
    assert_eq!(up.staged().unwrap(), staged);
    // staged under the data dir, never %TEMP%
    assert!(staged.dir.starts_with(fx.data.join("update").join("staging")));

    let applied = up.apply(&staged).unwrap();
    assert_eq!(applied.version, "0.2.0");
    assert_eq!(applied.previous_version, "0.1.0");
    assert_eq!(fx.exe(), b"NEW-EXE-0.2.0");
    assert_eq!(std::fs::read(fx.app.join("tsubame.exe.old")).unwrap(), b"OLD-EXE");
    assert!(!fx.app.join("tsubame.exe.new").exists());
    assert!(!fx.data.join("update").join("staging").join("0.2.0").exists(), "staging is removed after apply");
    assert_eq!(up.previous_version().as_deref(), Some("0.1.0"));

    // the new version starts: trial, then ready
    let new = fx.updater("0.2.0");
    assert_eq!(new.on_start(), Startup::Trial { version: "0.2.0".into(), previous: "0.1.0".into() });
    new.on_ready();
    assert_eq!(new.on_start(), Startup::Normal);
    assert!(fx.app.join("tsubame.exe.old").exists(), ".old stays as the one-click rollback");

    // one-click rollback restores the old file and the old version
    let rb = new.rollback().unwrap();
    assert_eq!((rb.from.as_str(), rb.to.as_str()), ("0.2.0", "0.1.0"));
    assert_eq!(fx.exe(), b"OLD-EXE");
    assert_eq!(std::fs::read(fx.app.join("tsubame.exe.bad")).unwrap(), b"NEW-EXE-0.2.0");
    assert!(new.rollback().is_err(), "only one step back");

    // the old version starts again: leftovers cleaned, and 0.2.0 is not offered again
    let old = fx.updater("0.1.0");
    assert_eq!(old.on_start(), Startup::Normal);
    assert!(!fx.app.join("tsubame.exe.bad").exists());
    assert!(old.check(true).unwrap().is_none(), "a rolled-back version is skipped");
}

#[test]
fn crash_after_update_is_reported_on_second_start() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    let up = fx.updater("0.1.0");
    let rel = up.check(false).unwrap().unwrap();
    let staged = up.download(&rel, &mut no_progress).unwrap();
    up.apply(&staged).unwrap();
    let new = fx.updater("0.2.0");
    assert!(matches!(new.on_start(), Startup::Trial { .. }));
    // never called on_ready: crashed. Next start:
    assert_eq!(new.on_start(), Startup::CrashedAfterUpdate { version: "0.2.0".into(), previous: "0.1.0".into(), boots: 2 });
    assert!(matches!(new.on_start(), Startup::CrashedAfterUpdate { boots: 3, .. }));
}

#[test]
fn check_is_rate_limited_and_uses_etag() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    let up = fx.updater("0.1.0");
    assert!(up.check(false).unwrap().is_some());
    assert_eq!(fx.server.hit_count("/releases/latest"), 1);
    // inside 24 h: answered from the verified cache, no network at all
    assert!(up.check(false).unwrap().is_some());
    assert!(up.check(false).unwrap().is_some());
    assert_eq!(fx.server.hit_count("/releases/latest"), 1);
    // "Check now": one request, and the server answers 304 because of If-None-Match
    assert!(up.check(true).unwrap().is_some());
    assert_eq!(fx.server.hit_count("/releases/latest"), 2);
    assert_eq!(fx.server.hit_count("releases/latest -> 304"), 1);
    // no asset downloads on a 304
    assert_eq!(fx.server.hit_count("tsubame-update.json -> 200"), 1);
}

#[test]
fn up_to_date_returns_none() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    assert!(fx.updater("0.2.0").check(false).unwrap().is_none());
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    assert!(fx.updater("0.3.0").check(false).unwrap().is_none(), "never offers a downgrade");
}

#[test]
fn no_release_published_is_not_an_error() {
    let fx = Fx::new(); // empty release dir: /latest is 404
    assert!(fx.updater("0.1.0").check(false).unwrap().is_none());
}

#[test]
fn tampered_zip_is_refused_and_nothing_changes() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW-EXE-0.2.0", "0.1.0");
    let up = fx.updater("0.1.0");
    let rel = up.check(false).unwrap().unwrap();
    let zp = fx.rel.join("tsubame-0.2.0-win-x64.zip");
    let mut z = std::fs::read(&zp).unwrap();
    let mid = z.len() / 2;
    z[mid] ^= 0x55; // same length, different bytes
    std::fs::write(&zp, &z).unwrap();
    let err = up.download(&rel, &mut no_progress).unwrap_err();
    assert!(matches!(err, Error::HashMismatch { .. }), "{err}");
    assert_eq!(fx.exe(), b"OLD-EXE");
    assert!(!fx.data.join("update").join("staging").join("0.2.0").exists(), "failed staging is removed");
    // truncated / padded zips: size check
    std::fs::write(&zp, &z[..z.len() - 3]).unwrap();
    assert!(up.download(&rel, &mut no_progress).is_err());
    std::fs::write(&zp, [&z[..], b"padding"].concat()).unwrap();
    assert!(up.download(&rel, &mut no_progress).is_err());
    assert_eq!(fx.exe(), b"OLD-EXE");
}

#[test]
fn tampered_manifest_and_wrong_signature_are_refused() {
    // manifest edited after signing (an attacker "fixes" the hash to match a bad zip)
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    let mp = fx.rel.join(manifest::MANIFEST_FILE);
    let text = std::fs::read_to_string(&mp).unwrap().replace("\"size\": 3", "\"size\": 4");
    std::fs::write(&mp, text).unwrap();
    let err = fx.updater("0.1.0").check(false).unwrap_err();
    assert!(matches!(err, Error::BadSignature(_)), "{err}");

    // signed by some other key
    let fx = Fx::new();
    let other = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([9u8; 32]));
    fx.publish_signed("0.2.0", b"NEW", "0.1.0", &other);
    let err = fx.updater("0.1.0").check(false).unwrap_err();
    assert!(matches!(err, Error::BadSignature(_)), "{err}");

    // signature file missing / garbage
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    std::fs::write(fx.rel.join(manifest::SIGNATURE_FILE), "not a signature").unwrap();
    assert!(matches!(fx.updater("0.1.0").check(false), Err(Error::BadSignature(_))));
    std::fs::remove_file(fx.rel.join(manifest::SIGNATURE_FILE)).unwrap();
    assert!(fx.updater("0.1.0").check(true).is_err());
}

#[test]
fn build_with_no_pinned_key_refuses_everything() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    let cfg = fx.cfg("0.1.0").with_keys(vec![]);
    assert!(matches!(Updater::new(cfg).check(false), Err(Error::BadSignature(_))));
}

#[test]
fn tampered_cache_is_dropped() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    let up = fx.updater("0.1.0");
    assert!(up.check(false).unwrap().is_some());
    let cached = fx.data.join("update").join("cache").join(manifest::MANIFEST_FILE);
    let mut b = std::fs::read(&cached).unwrap();
    b[20] ^= 1;
    std::fs::write(&cached, b).unwrap();
    // inside the 24 h window: the cache fails verification, answer is "nothing", cache is gone
    assert!(up.check(false).unwrap().is_none());
    assert!(!cached.exists());
    // and the next check goes back to the network
    assert!(up.check(false).unwrap().is_some());
}

#[test]
fn too_old_installs_need_the_installer() {
    let fx = Fx::new();
    fx.publish("0.5.0", b"NEW", "0.4.0");
    let up = fx.updater("0.1.0");
    let rel = up.check(false).unwrap().unwrap();
    assert!(!rel.in_place_ok);
    assert!(matches!(up.download(&rel, &mut no_progress), Err(Error::TooOld { .. })));
}

#[test]
fn cancel_leaves_no_staging() {
    let fx = Fx::new();
    fx.publish("0.2.0", &vec![7u8; 400_000], "0.1.0");
    let up = fx.updater("0.1.0");
    let rel = up.check(false).unwrap().unwrap();
    let err = up.download(&rel, &mut |_| false).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert!(!fx.data.join("update").join("staging").join("0.2.0").exists());
}

#[test]
fn authenticode_pin_blocks_unsigned_payload() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    let up = Updater::new(fx.cfg("0.1.0").with_pins(vec![keys::Pin::Subject("Tsubame Project")]));
    let rel = up.check(false).unwrap().unwrap();
    let err = up.download(&rel, &mut no_progress).unwrap_err();
    assert!(matches!(err, Error::Untrusted(_)), "{err}");
    assert!(!fx.data.join("update").join("staging").join("0.2.0").exists());
}

#[test]
fn apply_rejects_foreign_staged_paths_and_downgrades() {
    let fx = Fx::new();
    fx.publish("0.2.0", b"NEW", "0.1.0");
    let up = fx.updater("0.1.0");
    let rel = up.check(false).unwrap().unwrap();
    let staged = up.download(&rel, &mut no_progress).unwrap();
    // a hand-made staged.json pointing outside the staging folder
    let mut evil = staged.clone();
    evil.dir = fx.root.join("elsewhere");
    assert!(matches!(up.apply(&evil), Err(Error::Install(_))));
    // staged file changed on disk after verification
    std::fs::write(staged.dir.join("tsubame.exe"), b"SWAPPED").unwrap();
    assert!(matches!(up.apply(&staged), Err(Error::HashMismatch { .. })));
    assert_eq!(fx.exe(), b"OLD-EXE");
    // not newer
    assert!(matches!(fx.updater("0.2.0").apply(&staged), Err(Error::NotNewer)));
    // a file name the updater never installs
    let mut odd = staged.clone();
    odd.files.push(StagedFile { name: "evil.dll".into(), size: 1, sha256: "0".repeat(64) });
    assert!(matches!(up.apply(&odd), Err(Error::Install(_))));
}

#[test]
fn apply_only_updates_optional_files_that_are_installed() {
    let fx = Fx::new();
    // release contains tsubame.exe + tsubame-plughost.exe
    let exe = b"NEW-EXE".as_slice();
    let host = b"NEW-HOST".as_slice();
    let zip = zip::build_zip(&[("tsubame.exe", exe), ("tsubame-plughost.exe", host)], true);
    std::fs::write(fx.rel.join("tsubame-0.2.0-win-x64.zip"), &zip).unwrap();
    let mut m = sample("0.2.0");
    m.assets[0].size = zip.len() as u64;
    m.assets[0].sha256 = hash::sha256_hex(&zip).unwrap();
    m.assets[0].contents = vec![
        manifest::Content { name: "tsubame.exe".into(), size: exe.len() as u64, sha256: hash::sha256_hex(exe).unwrap() },
        manifest::Content { name: "tsubame-plughost.exe".into(), size: host.len() as u64, sha256: hash::sha256_hex(host).unwrap() },
    ];
    let bytes = serde_json::to_vec_pretty(&m).unwrap();
    std::fs::write(fx.rel.join(manifest::MANIFEST_FILE), &bytes).unwrap();
    std::fs::write(fx.rel.join(manifest::SIGNATURE_FILE), hash::to_hex(dev_keypair().sk.sign(keys::signed_message(&bytes), None).as_ref())).unwrap();

    let up = fx.updater("0.1.0");
    let rel = up.check(false).unwrap().unwrap();
    let staged = up.download(&rel, &mut no_progress).unwrap();
    assert_eq!(staged.files.len(), 2);
    let applied = up.apply(&staged).unwrap();
    assert_eq!(applied.replaced, vec!["tsubame.exe"], "the optional plug host is not installed, so it is not added");
    assert!(!fx.app.join("tsubame-plughost.exe").exists());

    // with the plug host installed it is updated too, and rolled back together
    let fx2 = Fx::new();
    std::fs::write(fx2.app.join("tsubame-plughost.exe"), b"OLD-HOST").unwrap();
    std::fs::copy(fx.rel.join("tsubame-0.2.0-win-x64.zip"), fx2.rel.join("tsubame-0.2.0-win-x64.zip")).unwrap();
    std::fs::copy(fx.rel.join(manifest::MANIFEST_FILE), fx2.rel.join(manifest::MANIFEST_FILE)).unwrap();
    std::fs::copy(fx.rel.join(manifest::SIGNATURE_FILE), fx2.rel.join(manifest::SIGNATURE_FILE)).unwrap();
    let up2 = fx2.updater("0.1.0");
    let rel2 = up2.check(false).unwrap().unwrap();
    let staged2 = up2.download(&rel2, &mut no_progress).unwrap();
    up2.apply(&staged2).unwrap();
    assert_eq!(std::fs::read(fx2.app.join("tsubame-plughost.exe")).unwrap(), b"NEW-HOST");
    fx2.updater("0.2.0").rollback().unwrap();
    assert_eq!(std::fs::read(fx2.app.join("tsubame-plughost.exe")).unwrap(), b"OLD-HOST");
    assert_eq!(fx2.exe(), b"OLD-EXE");
}

#[test]
fn watchdog_rolls_back_a_new_version_that_dies_at_start() {
    // Real processes: a tiny console exe that exits at once stands in for a new build that
    // crashes before it is ready. No window (CREATE_NO_WINDOW), nothing GUI.
    let host = Path::new(r"C:\Windows\System32\hostname.exe");
    if !host.exists() {
        eprintln!("skipped: hostname.exe not found");
        return;
    }
    let fx = Fx::new();
    std::fs::copy(host, fx.app.join("tsubame.exe")).unwrap();
    let old_bytes = fx.exe();
    let mut new_bytes = old_bytes.clone();
    new_bytes.extend_from_slice(&[0u8; 16]); // trailing padding: still a valid, different PE
    fx.publish("0.2.0", &new_bytes, "0.1.0");
    let up = fx.updater("0.1.0");
    let rel = up.check(false).unwrap().unwrap();
    let staged = up.download(&rel, &mut no_progress).unwrap();
    up.apply(&staged).unwrap();
    assert_eq!(fx.exe(), new_bytes);

    let out = up.relaunch_watched(&[], Duration::from_secs(10)).unwrap();
    assert!(matches!(out, WatchOutcome::RolledBack { .. }), "{out:?}");
    assert_eq!(fx.exe(), old_bytes, "previous version is back");
    assert!(fx.updater("0.1.0").check(true).unwrap().is_none(), "the failed version is not offered again");
}

#[test]
fn watch_decision_table() {
    let s = Duration::from_secs;
    assert_eq!(watch_decision(false, false, s(1), s(30)), Watch::Wait);
    assert_eq!(watch_decision(false, true, s(1), s(30)), Watch::Ready);
    assert_eq!(watch_decision(true, true, s(1), s(30)), Watch::Ready); // ready, then closed: not a start crash
    assert!(matches!(watch_decision(true, false, s(1), s(30)), Watch::Rollback(_)));
    assert!(matches!(watch_decision(false, false, s(30), s(30)), Watch::Rollback(_)));
}

#[test]
fn production_config_is_https_github_only() {
    let c = Config::production("0.1.0", Layout::new("a", "b")).unwrap();
    assert!(!c.allow_http_loopback);
    assert_eq!(c.repo, DEFAULT_REPO);
    assert!(c.api_base.starts_with("https://api.github.com"));
    assert!(c.download_base.starts_with("https://github.com"));
    assert!(Config::production("not a version", Layout::new("a", "b")).is_err());
}

#[test]
fn the_folder_handler_shim_is_swapped_with_the_app_and_rolled_back_with_it() {
    // docs/default-file-manager.md: the shim sits next to tsubame.exe and must never be older
    // or newer than it, so it is an updatable file and goes through the same swap/rollback.
    assert!(manifest::UPDATABLE_FILES.contains(&"tsubame-handler.exe"));
    let fx = Fx::new();
    std::fs::write(fx.app.join("tsubame-handler.exe"), b"OLD-SHIM").unwrap();
    let exe = b"NEW-EXE".as_slice();
    let shim = b"NEW-SHIM".as_slice();
    let zip = zip::build_zip(&[("tsubame.exe", exe), ("tsubame-handler.exe", shim)], true);
    std::fs::write(fx.rel.join("tsubame-0.2.0-win-x64.zip"), &zip).unwrap();
    let mut m = sample("0.2.0");
    m.assets[0].size = zip.len() as u64;
    m.assets[0].sha256 = hash::sha256_hex(&zip).unwrap();
    m.assets[0].contents = vec![
        manifest::Content { name: "tsubame.exe".into(), size: exe.len() as u64, sha256: hash::sha256_hex(exe).unwrap() },
        manifest::Content { name: "tsubame-handler.exe".into(), size: shim.len() as u64, sha256: hash::sha256_hex(shim).unwrap() },
    ];
    let bytes = serde_json::to_vec_pretty(&m).unwrap();
    std::fs::write(fx.rel.join(manifest::MANIFEST_FILE), &bytes).unwrap();
    std::fs::write(fx.rel.join(manifest::SIGNATURE_FILE), hash::to_hex(dev_keypair().sk.sign(keys::signed_message(&bytes), None).as_ref())).unwrap();

    let up = fx.updater("0.1.0");
    let rel = up.check(false).unwrap().unwrap();
    let staged = up.download(&rel, &mut no_progress).unwrap();
    let applied = up.apply(&staged).unwrap();
    assert!(applied.replaced.contains(&"tsubame-handler.exe".to_string()), "{:?}", applied.replaced);
    assert_eq!(std::fs::read(fx.app.join("tsubame-handler.exe")).unwrap(), b"NEW-SHIM");
    assert_eq!(std::fs::read(fx.app.join("tsubame-handler.exe.old")).unwrap(), b"OLD-SHIM");
    fx.updater("0.2.0").rollback().unwrap();
    assert_eq!(std::fs::read(fx.app.join("tsubame-handler.exe")).unwrap(), b"OLD-SHIM");
    assert_eq!(fx.exe(), b"OLD-EXE");
}
