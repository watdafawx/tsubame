//! Update channels: `stable` (the default: tagged `vX.Y.Z` releases) and `nightly` (one rolling
//! GitHub pre-release tagged `nightly`, rebuilt from `main` after every green CI run).
//!
//! The channel is part of the *signed* manifest (`"channel"`), and each channel has its own
//! manifest file name, so a nightly manifest can never be taken for a stable one: the updater
//! checks that the channel inside the verified bytes is the one it asked for. Design:
//! `docs/updater.md` ("Channels").

use crate::version::Version;

/// GitHub tag of the rolling nightly pre-release.
pub const NIGHTLY_TAG: &str = "nightly";
/// Pre-release identifier of a nightly version: `X.Y.Z-nightly.<run number>`.
pub const NIGHTLY_ID: &str = "nightly";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Tagged releases only. The default, and what any unknown config value means.
    #[default]
    Stable,
    /// Test builds from `main`. Opt-in. Still offered a newer stable release.
    Nightly,
}

impl Channel {
    /// Config value -> channel. Anything that is not exactly `nightly` (case-insensitive,
    /// trimmed) is stable: a typo must never move someone onto test builds.
    pub fn from_config(s: &str) -> Channel {
        if s.trim().eq_ignore_ascii_case("nightly") { Channel::Nightly } else { Channel::Stable }
    }

    /// Strict parse, for the signed manifest's `channel` field and the CLI.
    pub fn parse(s: &str) -> Option<Channel> {
        match s {
            "stable" => Some(Channel::Stable),
            "nightly" => Some(Channel::Nightly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Nightly => "nightly",
        }
    }

    /// The manifest asset name on a release of this channel.
    pub fn manifest_file(self) -> &'static str {
        match self {
            Channel::Stable => crate::manifest::MANIFEST_FILE,
            Channel::Nightly => crate::manifest::NIGHTLY_MANIFEST_FILE,
        }
    }

    pub fn signature_file(self) -> &'static str {
        match self {
            Channel::Stable => crate::manifest::SIGNATURE_FILE,
            Channel::Nightly => crate::manifest::NIGHTLY_SIGNATURE_FILE,
        }
    }

    /// Release sources this channel follows, newest wins. Nightly also follows stable, so a
    /// tester is offered the next stable release when it is newer than the newest nightly.
    pub fn sources(self) -> &'static [Channel] {
        match self {
            Channel::Stable => &[Channel::Stable],
            Channel::Nightly => &[Channel::Nightly, Channel::Stable],
        }
    }

    /// May a release (or a staged download) that came from `source` be installed by a copy
    /// set to `self`? Stable accepts only stable; nightly accepts both.
    pub fn accepts(self, source: Channel) -> bool {
        self.sources().contains(&source)
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `X.Y.Z-nightly.N` -> `Some(N)`; anything else -> `None`.
pub fn nightly_run(v: &Version) -> Option<u64> {
    match v.pre.as_slice() {
        [id, n] if id == NIGHTLY_ID => n.parse().ok().filter(|_| !n.starts_with('0') || n == "0"),
        _ => None,
    }
}

/// The version a nightly build of workspace version `base` from CI run `run` gets.
///
/// Rule: a plain `X.Y.Z` (the version of the last stable release, or the one about to be
/// released) becomes `X.Y.(Z+1)-nightly.N`, so every nightly sorts *above* the stable release it
/// was built after and *below* the next stable release `X.Y.(Z+1)` or `X.(Y+1).0`. A base that is
/// already a pre-release (`X.Y.Z-dev`) keeps its core: `X.Y.Z-nightly.N`. `N` is the GitHub run
/// number of the nightly workflow, which only ever grows, so nightlies of the same base order by
/// build. Single source of truth: `tsubame-release nightly-version` prints this for CI.
pub fn nightly_version(base: &Version, run: u64) -> Version {
    let patch = if base.is_prerelease() { base.patch } else { base.patch + 1 };
    Version { major: base.major, minor: base.minor, patch, pre: vec![NIGHTLY_ID.to_string(), run.to_string()] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn config_values() {
        assert_eq!(Channel::from_config("nightly"), Channel::Nightly);
        assert_eq!(Channel::from_config(" Nightly "), Channel::Nightly);
        for s in ["stable", "", "beta", "nightlyy", "dev", "STABLE"] {
            assert_eq!(Channel::from_config(s), Channel::Stable, "{s:?}");
        }
        assert_eq!(Channel::default(), Channel::Stable);
        assert_eq!(Channel::parse("nightly"), Some(Channel::Nightly));
        assert_eq!(Channel::parse("Nightly"), None, "the signed field is exact");
        assert_eq!(Channel::parse("beta"), None);
    }

    #[test]
    fn files_and_sources() {
        assert_eq!(Channel::Stable.manifest_file(), "tsubame-update.json");
        assert_eq!(Channel::Nightly.manifest_file(), "tsubame-update-nightly.json");
        assert_eq!(Channel::Nightly.signature_file(), "tsubame-update-nightly.json.sig");
        assert!(Channel::Stable.accepts(Channel::Stable));
        assert!(!Channel::Stable.accepts(Channel::Nightly), "a stable copy never installs a nightly");
        assert!(Channel::Nightly.accepts(Channel::Nightly) && Channel::Nightly.accepts(Channel::Stable));
    }

    #[test]
    fn nightly_version_rule() {
        assert_eq!(nightly_version(&v("0.1.0"), 42).to_string(), "0.1.1-nightly.42");
        assert_eq!(nightly_version(&v("1.9.9"), 7).to_string(), "1.9.10-nightly.7");
        assert_eq!(nightly_version(&v("0.2.0-dev"), 3).to_string(), "0.2.0-nightly.3");
        assert_eq!(nightly_run(&v("0.1.1-nightly.42")), Some(42));
        assert_eq!(nightly_run(&v("0.1.1-nightly")), None);
        assert_eq!(nightly_run(&v("0.1.1-rc.1")), None);
        assert_eq!(nightly_run(&v("0.1.1")), None);
        assert_eq!(nightly_run(&v("0.1.1-nightly.42.1")), None);
    }

    #[test]
    fn nightly_sorts_between_the_stable_it_follows_and_the_next_stable() {
        let stable = v("0.1.0");
        let n = nightly_version(&stable, 5);
        assert!(n > stable, "a nightly built after 0.1.0 is an upgrade from 0.1.0");
        assert!(v("0.1.1") > n, "the next patch release supersedes the nightly");
        assert!(v("0.2.0") > n, "so does the next minor release");
        // later builds of the same base win, numerically (10 > 9, not "10" < "9")
        assert!(nightly_version(&stable, 10) > nightly_version(&stable, 9));
        assert!(nightly_version(&stable, 100) > nightly_version(&stable, 99));
        // after the 0.1.1 release the workspace is 0.1.1 and nightlies continue above it,
        // even with a lower run number than an old nightly of the previous base
        let after = nightly_version(&v("0.1.1"), 6);
        assert!(after > v("0.1.1") && after > n);
        assert!(nightly_version(&v("0.1.1"), 1) > nightly_version(&v("0.1.0"), 999));
        // a nightly user never "updates" to an older stable
        assert!(v("0.1.0") < n);
        // nightly vs other pre-releases of the same core: alphanumeric identifiers compare as text
        assert!(v("0.1.1-nightly.1") > v("0.1.1-beta.9"));
        assert!(v("0.1.1-rc.1") > v("0.1.1-nightly.99"));
    }
}
