//! One error type for the whole crate. No `thiserror`: the enum is small.

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// Bad configuration / unusable environment (no %LOCALAPPDATA%, non-https URL, ...).
    Config(String),
    /// Transport failure (DNS, TLS, timeout, ...).
    Network(String),
    /// Server answered with an unexpected HTTP status.
    Http(u16),
    /// GitHub (or whoever answers) told us to slow down. Back off.
    RateLimited,
    /// JSON / zip / hex that does not parse or breaks a rule.
    Parse(String),
    /// Manifest signature missing, malformed or not made by a pinned key.
    BadSignature(String),
    /// Downloaded bytes do not match the signed manifest.
    HashMismatch { what: String },
    /// Size differs from the signed manifest (or exceeds the hard cap).
    SizeMismatch { what: String },
    /// Authenticode check failed or the signer is not the pinned one.
    Untrusted(String),
    /// The release is not newer than what is installed (replay / downgrade).
    NotNewer,
    /// Installed version is older than `min_supported_version`: use the installer.
    TooOld { min: String },
    /// Nothing staged / nothing to roll back to.
    Nothing(&'static str),
    /// The user (progress callback) cancelled.
    Cancelled,
    /// Install dir is not writable or the app is not installed there.
    Install(String),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(s) => write!(f, "configuration: {s}"),
            Error::Network(s) => write!(f, "network: {s}"),
            Error::Http(c) => write!(f, "unexpected HTTP status {c}"),
            Error::RateLimited => write!(f, "rate limited by the server; will retry later"),
            Error::Parse(s) => write!(f, "invalid data: {s}"),
            Error::BadSignature(s) => write!(f, "signature check failed: {s}"),
            Error::HashMismatch { what } => write!(f, "SHA-256 mismatch for {what}"),
            Error::SizeMismatch { what } => write!(f, "size mismatch for {what}"),
            Error::Untrusted(s) => write!(f, "untrusted file: {s}"),
            Error::NotNewer => write!(f, "release is not newer than the installed version"),
            Error::TooOld { min } => write!(f, "installed version is older than {min}; install the new version with the installer"),
            Error::Nothing(s) => write!(f, "{s}"),
            Error::Cancelled => write!(f, "cancelled"),
            Error::Install(s) => write!(f, "install: {s}"),
            Error::Io(e) => write!(f, "I/O: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
