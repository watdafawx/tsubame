//! Semantic version parse + compare. Small on purpose (no `semver` crate).
//! `1.2.3`, `v1.2.3`, `1.2.3-beta.1`; build metadata (`+x`) is ignored.

use crate::error::{Error, Result};
use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifiers (`beta`, `1`). Empty = a normal release.
    pub pre: Vec<String>,
}

impl Version {
    pub fn parse(s: &str) -> Result<Version> {
        let bad = || Error::Parse(format!("not a version: {s:?}"));
        let t = s.trim();
        let t = t.strip_prefix('v').unwrap_or(t);
        let t = t.split('+').next().unwrap_or("");
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (t, None),
        };
        let mut it = core.split('.');
        let mut num = || -> Result<u64> {
            let p = it.next().ok_or_else(bad)?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || (p.len() > 1 && p.starts_with('0')) {
                return Err(bad());
            }
            p.parse().map_err(|_| bad())
        };
        let (major, minor, patch) = (num()?, num()?, num()?);
        if it.next().is_some() {
            return Err(bad());
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => {
                let ids: Vec<String> = p.split('.').map(str::to_string).collect();
                if ids.iter().any(|i| i.is_empty() || !i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')) {
                    return Err(bad());
                }
                ids
            }
        };
        Ok(Version { major, minor, patch, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(o.major, o.minor, o.patch))
            .then_with(|| match (self.pre.is_empty(), o.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater, // 1.0.0 > 1.0.0-rc.1
                (false, true) => Ordering::Less,
                (false, false) => cmp_pre(&self.pre, &o.pre),
            })
    }
}

fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(n), Ok(m)) => n.cmp(&m),
            (Ok(_), Err(_)) => Ordering::Less, // numeric < alphanumeric
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// `v1.2.3` only: the tag shape the updater accepts from the API. Anything else is
/// refused before it can end up in a URL or a path.
pub fn parse_tag(tag: &str) -> Result<Version> {
    let ok = tag.len() <= 32 && tag.starts_with('v') && tag.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'));
    if !ok {
        return Err(Error::Parse(format!("unexpected release tag {tag:?}")));
    }
    Version::parse(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parse_and_display() {
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        assert_eq!(v("v0.0.1").to_string(), "0.0.1");
        assert_eq!(v("1.2.3-beta.1+build5").to_string(), "1.2.3-beta.1");
        for bad in ["", "1", "1.2", "1.2.3.4", "01.2.3", "1.2.x", "v", "1.2.3-", "1.2.3-a..b", "-1.2.3"] {
            assert!(Version::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ordering() {
        assert!(v("0.0.2") > v("0.0.1"));
        assert!(v("0.1.0") > v("0.0.99"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("1.10.0") > v("1.9.0")); // numeric, not lexical
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-rc.1") > v("1.0.0-beta.9"));
        assert!(v("1.0.0-1") < v("1.0.0-a"));
        assert!(v("1.0.0-a") < v("1.0.0-a.1"));
        assert_eq!(v("1.2.3+x"), v("1.2.3"));
    }

    #[test]
    fn tags() {
        assert!(parse_tag("v1.2.3").is_ok());
        assert!(parse_tag("1.2.3").is_err()); // must have the v
        assert!(parse_tag("v1.2.3/../x").is_err());
        assert!(parse_tag("v1.2.3 ").is_err());
        assert!(parse_tag("v1.2.3?x=1").is_err());
    }
}
