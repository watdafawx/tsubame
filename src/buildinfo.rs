//! What this exe is: version, release channel and commit, fixed at compile time.
//!
//! `tools\release.ps1` sets `TSUBAME_VERSION`, `TSUBAME_CHANNEL` and `TSUBAME_COMMIT` for the
//! release build (stable and nightly, `docs\release.md`). A plain `cargo build` has none of them:
//! the version is then the Cargo one and the channel reads "local build". The updater compares
//! against [`VERSION`], so a nightly exe (`0.1.1-nightly.42`) knows it is a nightly without
//! `Cargo.toml` ever being edited by CI.

/// The version the updater compares against and the UI shows.
pub const VERSION: &str = match option_env!("TSUBAME_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

/// `stable` / `nightly` for a release build, empty for a local build.
const CHANNEL_RAW: &str = match option_env!("TSUBAME_CHANNEL") {
    Some(c) => c,
    None => "",
};

/// Full commit hash, empty when unknown.
const COMMIT_RAW: &str = match option_env!("TSUBAME_COMMIT") {
    Some(c) => c,
    None => "",
};

/// The channel this exe was built for, as shown in Settings > Updates.
pub fn build_channel() -> &'static str {
    match CHANNEL_RAW {
        "stable" => "stable",
        "nightly" => "nightly",
        _ => "local build",
    }
}

/// Seven hex characters of the commit, or `None` when the build did not say.
pub fn short_commit() -> Option<&'static str> {
    short(COMMIT_RAW)
}

fn short(c: &str) -> Option<&str> {
    let ok = c.len() >= 7 && c.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| &c[..7])
}

/// One line for bug reports: `Tsubame 0.1.1-nightly.42 (nightly, abc1234)`.
pub fn version_line() -> String {
    describe(VERSION, build_channel(), short_commit())
}

fn describe(version: &str, channel: &str, commit: Option<&str>) -> String {
    match commit {
        Some(c) => format!("Tsubame {version} ({channel}, {c})"),
        None => format!("Tsubame {version} ({channel})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_has_channel_and_short_sha() {
        assert_eq!(describe("0.1.1-nightly.42", "nightly", Some("abc1234")), "Tsubame 0.1.1-nightly.42 (nightly, abc1234)");
        assert_eq!(describe("0.1.0", "local build", None), "Tsubame 0.1.0 (local build)");
        assert_eq!(short("0123456789abcdef"), Some("0123456"));
        assert_eq!(short("abc"), None);
        assert_eq!(short("zzzzzzzz"), None);
        assert_eq!(short(""), None);
        // whatever the build says, the version must parse for the updater
        assert!(tsubame_update::Version::parse(VERSION).is_ok());
    }
}
