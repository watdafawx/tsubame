//! `update\state.json`: the only thing the updater remembers between runs.
//! Corrupt or missing = defaults (an update check is never worth bricking anything for).

use crate::layout::{write_atomic, Layout};
use serde::{Deserialize, Serialize};

/// GitHub's unauthenticated API allows 60 requests/hour/IP. One check per day,
/// plus ETag/If-None-Match so an unchanged answer costs nothing.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 3600;
/// After a failed attempt (offline, 5xx): try again no sooner than this.
pub const RETRY_AFTER_FAILURE_SECS: u64 = 3600;
/// After a 403/429: back off a full interval.
pub const BACKOFF_RATE_LIMIT_SECS: u64 = CHECK_INTERVAL_SECS;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Previous {
    /// Version that is kept as `<file>.old`.
    pub version: String,
    /// File names (inside the install dir) that have a `.old` twin.
    pub files: Vec<String>,
    pub at_unix: u64,
}

/// Bookkeeping for "did the new version start?" (see `Updater::on_start`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Trial {
    /// The version that was just installed and has not yet proven it can start.
    pub version: String,
    /// The version to go back to.
    pub previous: String,
    /// How many times a process of `version` has started without calling `on_ready`.
    pub boots: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Unix time of the last network attempt (success or not): the rate-limit clock.
    #[serde(default)]
    pub last_attempt_unix: u64,
    /// Earliest time of the next attempt (set after failures / rate limiting).
    #[serde(default)]
    pub next_attempt_unix: u64,
    #[serde(default)]
    pub etag: Option<String>,
    /// Tag of the release the cached manifest belongs to.
    #[serde(default)]
    pub latest_tag: Option<String>,
    /// ETag of the nightly channel's release (`releases/tags/nightly`).
    #[serde(default)]
    pub nightly_etag: Option<String>,
    /// Tag the cached nightly manifest belongs to (always `nightly` when set).
    #[serde(default)]
    pub nightly_tag: Option<String>,
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub previous: Option<Previous>,
    #[serde(default)]
    pub trial: Option<Trial>,
    /// Versions that were rolled back from: never offered again automatically.
    #[serde(default)]
    pub skipped: Vec<String>,
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl State {
    pub fn load(l: &Layout) -> State {
        std::fs::read(l.state_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, l: &Layout) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        write_atomic(&l.state_file(), &json)
    }

    pub fn skip(&mut self, version: &str) {
        if !self.skipped.iter().any(|s| s == version) {
            self.skipped.push(version.to_string());
            let n = self.skipped.len();
            if n > 16 {
                self.skipped.drain(..n - 16);
            }
        }
    }

    /// (ETag, cached tag) of one release source.
    pub fn source_mut(&mut self, source: crate::Channel) -> (&mut Option<String>, &mut Option<String>) {
        match source {
            crate::Channel::Stable => (&mut self.etag, &mut self.latest_tag),
            crate::Channel::Nightly => (&mut self.nightly_etag, &mut self.nightly_tag),
        }
    }

    pub fn is_skipped(&self, version: &str) -> bool {
        self.skipped.iter().any(|s| s == version)
    }
}

/// Is a network check allowed now? `force` = the user pressed "Check now".
/// Force still honours a rate-limit back-off, but not the 24 h interval.
pub fn check_due(st: &State, now: u64, force: bool) -> bool {
    if now < st.next_attempt_unix {
        return false;
    }
    force || st.last_attempt_unix == 0 || now.saturating_sub(st.last_attempt_unix) >= CHECK_INTERVAL_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_rules() {
        let day = CHECK_INTERVAL_SECS;
        let mut st = State { last_attempt_unix: 1_000_000, ..Default::default() };
        assert!(!check_due(&st, 1_000_000 + 60, false));
        assert!(!check_due(&st, 1_000_000 + day - 1, false));
        assert!(check_due(&st, 1_000_000 + day, false));
        // the user pressing "check now" bypasses the interval
        assert!(check_due(&st, 1_000_000 + 60, true));
        // but not a back-off after a 403/429
        st.next_attempt_unix = 1_000_000 + BACKOFF_RATE_LIMIT_SECS;
        assert!(!check_due(&st, 1_000_000 + 3600, true));
        assert!(!check_due(&st, 1_000_000 + day - 1, false));
        assert!(check_due(&st, 1_000_000 + day, true));
        // never checked: due
        assert!(check_due(&State::default(), 5, false));
    }

    #[test]
    fn roundtrip_and_corruption() {
        let d = std::env::temp_dir().join(format!("tsu-state-{}", std::process::id()));
        let l = Layout::new(d.join("app"), d.join("data"));
        assert_eq!(State::load(&l), State::default());
        let mut st = State { etag: Some("\"abc\"".into()), installed_version: Some("0.1.0".into()), ..Default::default() };
        st.previous = Some(Previous { version: "0.0.9".into(), files: vec!["tsubame.exe".into()], at_unix: 5 });
        st.trial = Some(Trial { version: "0.1.0".into(), previous: "0.0.9".into(), boots: 1 });
        st.skip("0.0.5");
        st.save(&l).unwrap();
        assert_eq!(State::load(&l), st);
        std::fs::write(l.state_file(), b"{ not json").unwrap();
        assert_eq!(State::load(&l), State::default());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn skip_list_is_bounded_and_unique() {
        let mut st = State::default();
        for i in 0..40 {
            st.skip(&format!("1.0.{i}"));
            st.skip("1.0.0");
        }
        assert!(st.skipped.len() <= 16);
        assert!(st.is_skipped("1.0.39"));
    }
}
