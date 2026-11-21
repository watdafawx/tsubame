//! Which container an archive is, from its name or its first bytes.
use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::ArchiveError;

/// The archive containers this build browses and extracts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    SevenZ,
}

impl ArchiveFormat {
    /// Format from the file name alone (case-insensitive): `.zip`, `.tar`,
    /// `.tar.gz` / `.tar.gzip` / `.tgz`, `.7z`.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        if name.ends_with(".tar.gz") || name.ends_with(".tar.gzip") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if name.ends_with(".zip") {
            Some(Self::Zip)
        } else if name.ends_with(".tar") {
            Some(Self::Tar)
        } else if name.ends_with(".7z") {
            Some(Self::SevenZ)
        } else {
            None
        }
    }

    /// Format from the first bytes of the file, so a renamed archive still opens.
    pub fn from_magic(head: &[u8]) -> Option<Self> {
        // local file header, empty archive, spanned archive
        if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08") {
            Some(Self::Zip)
        } else if head.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
            Some(Self::SevenZ)
        } else if head.starts_with(&[0x1F, 0x8B]) {
            Some(Self::TarGz)
        } else if head.len() >= 262 && &head[257..262] == b"ustar" {
            Some(Self::Tar)
        } else {
            None
        }
    }

    /// The file name's format, else the magic bytes. Cheap: only an unknown
    /// extension costs a 512 byte read.
    pub fn detect(path: &Path) -> Result<Self, ArchiveError> {
        if let Some(format) = Self::from_path(path) {
            return Ok(format);
        }
        let mut head = [0u8; 512];
        let mut file = File::open(path)?;
        let n = file.read(&mut head).unwrap_or(0);
        Self::from_magic(&head[..n]).ok_or_else(|| ArchiveError::Unsupported(path.display().to_string()))
    }

    /// Short name for logs, the status bar and tests: `zip`, `tar`, `tar.gz`, `7z`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::SevenZ => "7z",
        }
    }
}
