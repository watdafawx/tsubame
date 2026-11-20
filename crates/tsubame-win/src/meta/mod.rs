//! Background metadata engines (P4.F5): folder sizes, git status, checksums.
//!
//! Every engine in this module follows the `docs/PLAN.md` §3 rules that apply
//! to columns and commands:
//!
//! - **Rule 1 (UI thread never touches the disk):** the folder-size and git
//!   engines run on their own worker threads and deliver results over an
//!   `mpsc` channel, tagged with a generation number. The checksum engine is a
//!   synchronous function that a command handler runs on a worker.
//! - **Rule 5 (every cache has a hard cap and an eviction test):**
//!   [`foldersize::FolderSizeCache`] (2,048 directories) and
//!   [`gitstatus::GitStatusCache`] (32 repositories, 5 s TTL) are bounded LRUs
//!   with explicit invalidation for watcher events.
//! - **Rule 6 (dependencies stay tiny):** checksums are pure Rust; git uses the
//!   `git` already on `PATH`.
//!
//! Wave 3 wires these into the details-view columns and the checksum command;
//! nothing here depends on `tsubame-ui` or the app crate.

pub mod checksum;
pub mod foldersize;
pub mod gitstatus;

pub use checksum::{
    Algorithm, BUFFER_BYTES, CRC32, ChecksumError, ChecksumSet, Md5, MultiHasher, Sha1, Sha256,
    checksum_bytes, checksum_file, checksum_reader,
};
pub use foldersize::{
    DEFAULT_CACHE_CAP as DEFAULT_FOLDER_SIZE_CACHE_CAP,
    DEFAULT_WORKERS as DEFAULT_FOLDER_SIZE_WORKERS, FolderSize, FolderSizeCache, FolderSizeError,
    FolderSizeReply, FolderSizeWorker, format_bytes, scan_folder, scan_folder_bounded,
};
pub use gitstatus::{
    DEFAULT_CACHE_CAP as DEFAULT_GIT_CACHE_CAP, DEFAULT_TIMEOUT as DEFAULT_GIT_TIMEOUT,
    DEFAULT_TTL as DEFAULT_GIT_TTL, GitReply, GitRepoStatus, GitRequest, GitState, GitStatusCache,
    GitStatusEngine, GitStatusError, GitStatusWorker, find_repo_root, parse_porcelain_v1,
};
