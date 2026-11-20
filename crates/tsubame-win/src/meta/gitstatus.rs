//! Git status engine for the P4.F5 "Git" column.
//!
//! The engine walks up from a path to its enclosing repository (handling the
//! `.git` **file** that worktrees and submodules use), runs one
//! `git -C <root> status --porcelain=v1 -z --ignored=matching
//! --untracked-files=normal` per repository for a whole batch of paths, and
//! parses the result into a [`GitState`] per path
//! ([`GitRepoStatus`]). Dirty entries mark every parent directory inside the
//! repository, so a collapsed folder shows that something below it changed.
//!
//! Safety properties that matter for a file manager:
//! - The child process never opens a console window (`CREATE_NO_WINDOW`) and is
//!   killed after [`DEFAULT_TIMEOUT`] (3 s), so a huge or network repository
//!   cannot hang the column.
//! - No `git` on `PATH` is a normal state: every API reports
//!   [`GitStatusError::Unavailable`] and [`GitStatusEngine::states_or_clean`]
//!   turns that into "no badge".
//! - [`GitStatusCache`] is a hard-capped LRU (default [`DEFAULT_CACHE_CAP`] = 32
//!   repositories) with a [`DEFAULT_TTL`] of 5 s plus explicit
//!   [`GitStatusEngine::invalidate`] from the directory watcher
//!   (`docs/PLAN.md` §3 rule 5).
//!
//! Queries block on the `git` process, so call them from a worker thread or via
//! [`GitStatusWorker`]; never from the UI thread (`docs/PLAN.md` §3 rule 1).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::foldersize::{key_is_same_or_child, normalize_path_key};

/// Default hard cap of the per-repository cache.
pub const DEFAULT_CACHE_CAP: usize = 32;
/// Default freshness window for a cached repository status.
pub const DEFAULT_TTL: Duration = Duration::from_secs(5);
/// Default `git status` deadline before the child is killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
/// How many parents `find_repo_root` inspects before giving up.
pub const MAX_PARENT_DEPTH: usize = 64;

/// `CREATE_NO_WINDOW`; git must never flash a console.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Sleep between `try_wait` polls while git runs.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Wake callback invoked after every reply is sent (e.g. `PostMessageW`).
pub type Wake = Arc<dyn Fn() + Send + Sync + 'static>;

/// Compact per-path repository state shown in the Git column.
///
/// The variant order is the aggregation priority used by
/// [`GitState::combine`]: higher wins, so a file that is both staged and
/// modified reads as modified, and a conflicted file always reads as a
/// conflict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GitState {
    /// Tracked and unchanged, or not inside a repository.
    #[default]
    Clean,
    /// Matches an ignore rule.
    Ignored,
    /// Not tracked yet.
    Untracked,
    /// Staged for the first time.
    Added,
    /// Renamed or copied (staged or in the worktree).
    Renamed,
    /// Tracked, contents or mode changed.
    Modified,
    /// Tracked, removed.
    Deleted,
    /// Unmerged (any side of a conflict).
    Conflict,
}

impl GitState {
    /// True for states that deserve a badge on a file row.
    ///
    /// `Ignored` is not dirty: it is only shown when the user turns the column
    /// on for ignored entries.
    pub fn is_dirty(self) -> bool {
        !matches!(self, GitState::Clean | GitState::Ignored)
    }

    /// True for an unmerged entry.
    pub fn is_conflict(self) -> bool {
        self == GitState::Conflict
    }

    /// The highest-priority state of the two.
    pub fn combine(self, other: GitState) -> GitState {
        self.max(other)
    }

    /// Short label for a context menu or status bar.
    pub fn label(self) -> &'static str {
        match self {
            GitState::Clean => "clean",
            GitState::Ignored => "ignored",
            GitState::Untracked => "untracked",
            GitState::Added => "added",
            GitState::Renamed => "renamed",
            GitState::Modified => "modified",
            GitState::Deleted => "deleted",
            GitState::Conflict => "conflict",
        }
    }

    /// One-character badge (` `, `!`, `?`, `A`, `R`, `M`, `D`, `U`).
    pub fn code(self) -> char {
        match self {
            GitState::Clean => ' ',
            GitState::Ignored => '!',
            GitState::Untracked => '?',
            GitState::Added => 'A',
            GitState::Renamed => 'R',
            GitState::Modified => 'M',
            GitState::Deleted => 'D',
            GitState::Conflict => 'U',
        }
    }
}

impl fmt::Display for GitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a git query produced no status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitStatusError {
    /// `git` is not installed or not on `PATH`.
    Unavailable,
    /// The child did not finish within the engine's timeout and was killed.
    Timeout,
    /// The path is not inside a repository (only returned by explicit
    /// repository queries, never by [`GitStatusEngine::status_for`]).
    NotARepository(PathBuf),
    /// Spawning or reading the child failed, or git exited non-zero.
    Io(String),
}

impl fmt::Display for GitStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitStatusError::Unavailable => f.write_str("git is not available"),
            GitStatusError::Timeout => f.write_str("git status timed out"),
            GitStatusError::NotARepository(path) => {
                write!(f, "not a git repository: {}", path.display())
            }
            GitStatusError::Io(msg) => write!(f, "git status failed: {msg}"),
        }
    }
}

impl std::error::Error for GitStatusError {}

// ---------------------------------------------------------------------------
// Porcelain parsing
// ---------------------------------------------------------------------------

/// Parse `git status --porcelain=v1 -z` output into `(repo-relative path, state)`.
///
/// The raw paths are normalized to lower-case forward-slash keys, matching the
/// case-insensitive Windows filesystem. Rename/copy records consume the extra
/// original-path token that `-z` appends.
pub fn parse_porcelain_v1(bytes: &[u8]) -> Vec<(String, GitState)> {
    let mut out = Vec::new();
    let mut tokens = bytes.split(|b| *b == 0).filter(|t| !t.is_empty());

    while let Some(token) = tokens.next() {
        if token.len() < 4 {
            continue;
        }
        let x = token[0] as char;
        let y = token[1] as char;
        let raw_path = &token[3..];
        let path = normalize_relative(&String::from_utf8_lossy(raw_path));
        if path.is_empty() {
            continue;
        }
        let state = state_from_xy(x, y);
        out.push((path, state));

        // With -z a rename is `R  new\0old\0`: the original path is its own token.
        if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            let _original = tokens.next();
        }
    }

    out
}

fn state_from_xy(x: char, y: char) -> GitState {
    if x == '?' && y == '?' {
        return GitState::Untracked;
    }
    if x == '!' && y == '!' {
        return GitState::Ignored;
    }
    // Unmerged combinations: UU, AU, UA, DU, UD, AA, DD.
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return GitState::Conflict;
    }

    let from_code = |c: char| match c {
        'A' => Some(GitState::Added),
        'M' | 'T' => Some(GitState::Modified),
        'D' => Some(GitState::Deleted),
        'R' | 'C' => Some(GitState::Renamed),
        'U' => Some(GitState::Conflict),
        '?' => Some(GitState::Untracked),
        '!' => Some(GitState::Ignored),
        _ => None,
    };

    match (from_code(x), from_code(y)) {
        (Some(a), Some(b)) => a.combine(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => GitState::Clean,
    }
}

fn normalize_relative(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch == '\\' {
            out.push('/');
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    while out.ends_with('/') {
        out.pop();
    }
    while out.starts_with("./") {
        out.drain(..2);
    }
    out
}

/// Parsed status of one repository: every changed path plus every ancestor
/// directory up to (and including) the root key `""`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepoStatus {
    root: PathBuf,
    states: HashMap<String, GitState>,
}

impl GitRepoStatus {
    /// Parse porcelain output for `root`.
    pub fn from_porcelain(root: impl Into<PathBuf>, bytes: &[u8]) -> Self {
        let mut states: HashMap<String, GitState> = HashMap::new();
        states.insert(String::new(), GitState::Clean);

        for (path, state) in parse_porcelain_v1(bytes) {
            let entry = states.entry(path.clone()).or_insert(GitState::Clean);
            *entry = entry.combine(state);

            let mut ancestor = path;
            while let Some(slash) = ancestor.rfind('/') {
                ancestor.truncate(slash);
                let dir = states.entry(ancestor.clone()).or_insert(GitState::Clean);
                *dir = ancestor_state_after_child(*dir, state);
            }
            let root_state = states.entry(String::new()).or_insert(GitState::Clean);
            *root_state = ancestor_state_after_child(*root_state, state);
        }

        Self {
            root: root.into(),
            states,
        }
    }

    /// Repository working-tree root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// State recorded for a repository-relative key (see [`Self::state_at`] for
    /// absolute paths).
    pub fn state_for_key(&self, key: &str) -> GitState {
        self.states.get(key).copied().unwrap_or(GitState::Clean)
    }

    /// State of `path`, which may be a file or a directory inside the repo.
    /// Paths outside the repo (or a mismatched root) read as [`GitState::Clean`].
    pub fn state_at(&self, path: &Path) -> GitState {
        match self.relative_key(path) {
            Some(key) => self.state_for_key(&key),
            None => GitState::Clean,
        }
    }

    /// Repo-relative lower-case slash key for `path`, or `None` when the path
    /// is not below this repository.
    pub fn relative_key(&self, path: &Path) -> Option<String> {
        let root_key = normalize_path_key(&self.root);
        let path_key = normalize_path_key(path);
        if path_key == root_key {
            return Some(String::new());
        }
        if !key_is_same_or_child(&path_key, &root_key) {
            return None;
        }
        let rest = &path_key[root_key.len() + 1..];
        Some(rest.replace('\\', "/"))
    }

    /// Number of keys held (changed paths plus propagated directories).
    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// True when nothing changed anywhere in the repository.
    pub fn is_clean(&self) -> bool {
        self.state_for_key("") == GitState::Clean
    }

    /// All keys, for debugging and tests.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.states.keys().map(String::as_str)
    }
}

/// A parent directory aggregates its children as "modified" unless a child is
/// in conflict. Ignored children never dirty an ancestor.
fn ancestor_state_after_child(parent: GitState, child: GitState) -> GitState {
    match child {
        GitState::Clean | GitState::Ignored => parent,
        GitState::Conflict => parent.combine(GitState::Conflict),
        _ => parent.combine(GitState::Modified),
    }
}

// ---------------------------------------------------------------------------
// Repository discovery
// ---------------------------------------------------------------------------

/// Walk up from `start` (a file is treated as its parent) and return the
/// nearest directory that is a repository root.
///
/// A `.git` directory is a normal repository; a `.git` file is a worktree or
/// submodule whose `gitdir:` target must exist. The walk is bounded to
/// [`MAX_PARENT_DEPTH`] parents.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = match fs::metadata(start) {
        Ok(meta) if meta.is_file() => start.parent()?.to_path_buf(),
        _ => start.to_path_buf(),
    };

    for _ in 0..MAX_PARENT_DEPTH {
        let dot_git = current.join(".git");
        match fs::metadata(&dot_git) {
            Ok(meta) if meta.is_dir() => return Some(current),
            Ok(meta)
                if meta.is_file()
                    && git_file_target(&dot_git).is_some_and(|target| target.exists()) =>
            {
                return Some(current);
            }
            _ => {}
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
    None
}

/// `gitdir:` target of a `.git` file, resolved against the file's directory.
pub fn git_file_target(git_file: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(git_file).ok()?;
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    let lower = line.to_ascii_lowercase();
    if !lower.starts_with("gitdir:") {
        return None;
    }
    let target = PathBuf::from(line[7..].trim());
    Some(if target.is_absolute() {
        target
    } else {
        git_file.parent()?.join(target)
    })
}

// ---------------------------------------------------------------------------
// Bounded LRU cache with TTL
// ---------------------------------------------------------------------------

struct CacheEntry {
    status: GitRepoStatus,
    fetched: Instant,
    last_used: u64,
}

/// Hard-capped LRU of repository statuses with a time-to-live.
///
/// `get` returns a clone so callers can hold a status while the cache keeps
/// serving other repositories; entries older than the TTL are removed on read.
pub struct GitStatusCache {
    cap: usize,
    ttl: Duration,
    tick: u64,
    entries: HashMap<String, CacheEntry>,
}

impl Default for GitStatusCache {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_CAP, DEFAULT_TTL)
    }
}

impl GitStatusCache {
    /// New cache with a hard cap of `cap` repositories (at least 1) and a
    /// freshness window of `ttl`.
    pub fn new(cap: usize, ttl: Duration) -> Self {
        Self {
            cap: cap.max(1),
            ttl,
            tick: 0,
            entries: HashMap::new(),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Cached status for `root` if it is younger than the TTL.
    pub fn get(&mut self, root: &Path) -> Option<GitRepoStatus> {
        self.get_at(root, Instant::now())
    }

    /// [`Self::get`] with an explicit clock, so tests need no sleeps.
    pub fn get_at(&mut self, root: &Path, now: Instant) -> Option<GitRepoStatus> {
        let key = normalize_path_key(root);
        let tick = self.next_tick();
        let entry = self.entries.get_mut(&key)?;
        if now.saturating_duration_since(entry.fetched) > self.ttl {
            self.entries.remove(&key);
            return None;
        }
        entry.last_used = tick;
        Some(entry.status.clone())
    }

    /// Store one repository status, evicting the least recently used entry when
    /// the cache is at its hard cap.
    pub fn insert(&mut self, root: &Path, status: GitRepoStatus) {
        self.insert_at(root, status, Instant::now());
    }

    /// [`Self::insert`] with an explicit fetch time.
    pub fn insert_at(&mut self, root: &Path, status: GitRepoStatus, now: Instant) {
        let key = normalize_path_key(root);
        let tick = self.next_tick();
        if !self.entries.contains_key(&key) && self.entries.len() >= self.cap {
            self.evict_one();
        }
        self.entries.insert(
            key,
            CacheEntry {
                status,
                fetched: now,
                last_used: tick,
            },
        );
    }

    /// Drop the entry for `root`, or for the repository containing it. Returns
    /// true if anything was removed. Called from the watcher on a change.
    pub fn invalidate(&mut self, path: &Path) -> bool {
        let key = normalize_path_key(path);
        let before = self.entries.len();
        self.entries.retain(|root_key, _| {
            !key_is_same_or_child(root_key, &key) && !key_is_same_or_child(&key, root_key)
        });
        before != self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.wrapping_add(1);
        self.tick
    }

    fn evict_one(&mut self) {
        let victim = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| k.clone());
        if let Some(key) = victim {
            self.entries.remove(&key);
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Spawns and caches `git status`, one process per repository per batch.
pub struct GitStatusEngine {
    cache: Mutex<GitStatusCache>,
    timeout: Duration,
    available: Mutex<Option<bool>>,
}

impl Default for GitStatusEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl GitStatusEngine {
    pub fn new() -> Self {
        Self::with_options(DEFAULT_CACHE_CAP, DEFAULT_TTL, DEFAULT_TIMEOUT)
    }

    pub fn with_options(cache_cap: usize, ttl: Duration, timeout: Duration) -> Self {
        Self {
            cache: Mutex::new(GitStatusCache::new(cache_cap, ttl)),
            timeout,
            available: Mutex::new(None),
        }
    }

    /// True when `git --version` succeeds; probed once and cached.
    pub fn is_available(&self) -> bool {
        let mut available = lock(&self.available);
        if let Some(value) = *available {
            return value;
        }
        let ok = {
            let mut command = Command::new("git");
            command
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            hide_console(&mut command);
            command.status().map(|s| s.success()).unwrap_or(false)
        };
        *available = Some(ok);
        ok
    }

    pub fn cache_len(&self) -> usize {
        lock(&self.cache).len()
    }

    /// Drop the cached status of the repository containing `path`.
    pub fn invalidate(&self, path: &Path) {
        lock(&self.cache).invalidate(path);
    }

    pub fn clear(&self) {
        lock(&self.cache).clear();
    }

    /// Query one repository root without consulting the cache.
    pub fn repo_status(&self, root: &Path) -> Result<GitRepoStatus, GitStatusError> {
        if !self.is_available() {
            return Err(GitStatusError::Unavailable);
        }
        let bytes = run_git_status(root, self.timeout)?;
        Ok(GitRepoStatus::from_porcelain(root, &bytes))
    }

    /// Cached repository status (TTL: [`DEFAULT_TTL`] by default).
    pub fn status_cached(&self, root: &Path) -> Result<GitRepoStatus, GitStatusError> {
        if let Some(hit) = lock(&self.cache).get(root) {
            return Ok(hit);
        }
        let status = self.repo_status(root)?;
        lock(&self.cache).insert(root, status.clone());
        Ok(status)
    }

    /// State of one path. A path outside any repository is
    /// [`GitState::Clean`], not an error.
    pub fn status_for(&self, path: &Path) -> Result<GitState, GitStatusError> {
        match find_repo_root(path) {
            None => Ok(GitState::Clean),
            Some(root) => Ok(self.status_cached(&root)?.state_at(path)),
        }
    }

    /// States for a batch of paths, grouped so that each repository is queried
    /// once. Paths outside a repository (or in a repository that failed) read
    /// as [`GitState::Clean`]; the first failure is still returned when every
    /// repository failed.
    pub fn batch(&self, paths: &[PathBuf]) -> Result<Vec<GitState>, GitStatusError> {
        if !self.is_available() {
            return Err(GitStatusError::Unavailable);
        }

        let mut states = vec![GitState::Clean; paths.len()];
        let mut groups: HashMap<String, (PathBuf, Vec<usize>)> = HashMap::new();
        for (index, path) in paths.iter().enumerate() {
            if let Some(root) = find_repo_root(path) {
                let key = normalize_path_key(&root);
                groups
                    .entry(key)
                    .or_insert_with(|| (root, Vec::new()))
                    .1
                    .push(index);
            }
        }

        let mut first_error: Option<GitStatusError> = None;
        let mut succeeded = 0usize;
        for (root, indices) in groups.into_values() {
            match self.status_cached(&root) {
                Ok(status) => {
                    succeeded += 1;
                    for index in indices {
                        states[index] = status.state_at(&paths[index]);
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        match first_error {
            Some(error) if succeeded == 0 => Err(error),
            _ => Ok(states),
        }
    }

    /// [`Self::batch`] but never fails: unavailable or timed-out repositories
    /// read as clean, which is what the column wants.
    pub fn states_or_clean(&self, paths: &[PathBuf]) -> Vec<GitState> {
        self.batch(paths)
            .unwrap_or_else(|_| vec![GitState::Clean; paths.len()])
    }
}

fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

fn run_git_status(root: &Path, timeout: Duration) -> Result<Vec<u8>, GitStatusError> {
    let mut command = Command::new("git");
    command
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored=matching",
            "--untracked-files=normal",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);

    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            GitStatusError::Unavailable
        } else {
            GitStatusError::Io(format!("spawn git: {error}"))
        }
    })?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| GitStatusError::Io("git stdout unavailable".to_string()))?;
    let reader = thread::Builder::new()
        .name("tsubame-git-stdout".to_string())
        .spawn(move || {
            let mut buf = Vec::new();
            match stdout.read_to_end(&mut buf) {
                Ok(_) => Ok(buf),
                Err(error) => Err(error.to_string()),
            }
        })
        .map_err(|error| GitStatusError::Io(format!("spawn git reader: {error}")))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = reader
                    .join()
                    .map_err(|_| GitStatusError::Io("git stdout reader panicked".to_string()))?
                    .map_err(|error| GitStatusError::Io(format!("read git stdout: {error}")))?;
                if !status.success() {
                    return Err(GitStatusError::Io(format!(
                        "git status exited with {status}"
                    )));
                }
                return Ok(output);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_and_reap(&mut child);
                    let _ = reader.join();
                    return Err(GitStatusError::Timeout);
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                kill_and_reap(&mut child);
                let _ = reader.join();
                return Err(GitStatusError::Io(format!("wait for git: {error}")));
            }
        }
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

// ---------------------------------------------------------------------------
// Background worker
// ---------------------------------------------------------------------------

/// One batch of paths to classify.
#[derive(Clone, Debug)]
pub struct GitRequest {
    pub generation: u64,
    pub paths: Vec<PathBuf>,
}

/// Worker output: `states[i]` belongs to `paths[i]`.
#[derive(Clone, Debug)]
pub struct GitReply {
    pub generation: u64,
    pub paths: Vec<PathBuf>,
    pub states: Vec<GitState>,
    /// True when `git` is missing, so the UI can disable the column for the
    /// session instead of retrying per listing.
    pub unavailable: bool,
}

/// Single background thread that answers [`GitRequest`] batches off the UI
/// thread. Stale generations are dropped without spawning git.
pub struct GitStatusWorker {
    tx: Option<Sender<GitRequest>>,
    generation: Arc<AtomicU64>,
    engine: Arc<GitStatusEngine>,
    thread: Option<JoinHandle<()>>,
}

impl GitStatusWorker {
    /// Start with the default engine.
    pub fn start(wake: Option<Wake>) -> (Self, Receiver<GitReply>) {
        Self::with_engine(Arc::new(GitStatusEngine::new()), wake)
    }

    /// Start with a caller-owned engine (shared cache, custom timeout).
    pub fn with_engine(
        engine: Arc<GitStatusEngine>,
        wake: Option<Wake>,
    ) -> (Self, Receiver<GitReply>) {
        let (request_tx, request_rx) = channel::<GitRequest>();
        let (reply_tx, reply_rx) = channel::<GitReply>();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        let worker_engine = Arc::clone(&engine);

        let thread = thread::Builder::new()
            .name("tsubame-gitstatus".to_string())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    if request.generation < worker_generation.load(Ordering::Acquire) {
                        continue;
                    }
                    let unavailable = !worker_engine.is_available();
                    let states = worker_engine.states_or_clean(&request.paths);
                    let reply = GitReply {
                        generation: request.generation,
                        paths: request.paths,
                        states,
                        unavailable,
                    };
                    if reply_tx.send(reply).is_err() {
                        return;
                    }
                    if let Some(wake) = &wake {
                        wake();
                    }
                }
            })
            .ok();

        (
            Self {
                tx: Some(request_tx),
                generation,
                engine,
                thread,
            },
            reply_rx,
        )
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Advance the generation; queued batches from older generations are
    /// skipped without spawning git.
    pub fn begin_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Queue a batch for `generation`.
    pub fn request(&self, generation: u64, paths: Vec<PathBuf>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(GitRequest { generation, paths });
        }
    }

    /// Shared engine, for cache invalidation from the watcher.
    pub fn engine(&self) -> &Arc<GitStatusEngine> {
        &self.engine
    }

    /// Invalidate the repository containing `path`.
    pub fn invalidate(&self, path: &Path) {
        self.engine.invalidate(path);
    }

    /// Stop the worker and wait for the thread.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for GitStatusWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "tsubame-gitstatus-{tag}-{}-{n}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn status(root: &str, porcelain: &[u8]) -> GitRepoStatus {
        GitRepoStatus::from_porcelain(PathBuf::from(root), porcelain)
    }

    #[test]
    fn parses_every_porcelain_code() {
        let porcelain = b" M modified.txt\0\
M  staged.txt\0\
A  added.txt\0\
D  deleted.txt\0\
R  renamed.txt\0old.txt\0\
C  copied.txt\0source.txt\0\
T  typechange.txt\0\
?? untracked.txt\0\
!! ignored.log\0\
UU conflict.txt\0\
AA both-added.txt\0\
DD both-deleted.txt\0\
AM added-then-modified.txt\0\
MM staged-and-modified.txt\0";
        let parsed: HashMap<String, GitState> = parse_porcelain_v1(porcelain).into_iter().collect();

        assert_eq!(parsed["modified.txt"], GitState::Modified);
        assert_eq!(parsed["staged.txt"], GitState::Modified);
        assert_eq!(parsed["added.txt"], GitState::Added);
        assert_eq!(parsed["deleted.txt"], GitState::Deleted);
        assert_eq!(parsed["renamed.txt"], GitState::Renamed);
        assert_eq!(parsed["copied.txt"], GitState::Renamed);
        assert_eq!(parsed["typechange.txt"], GitState::Modified);
        assert_eq!(parsed["untracked.txt"], GitState::Untracked);
        assert_eq!(parsed["ignored.log"], GitState::Ignored);
        assert_eq!(parsed["conflict.txt"], GitState::Conflict);
        assert_eq!(parsed["both-added.txt"], GitState::Conflict);
        assert_eq!(parsed["both-deleted.txt"], GitState::Conflict);
        assert_eq!(parsed["added-then-modified.txt"], GitState::Modified);
        assert_eq!(parsed["staged-and-modified.txt"], GitState::Modified);
        // The original path of a rename/copy is consumed, not a status entry.
        assert!(!parsed.contains_key("old.txt"));
        assert!(!parsed.contains_key("source.txt"));
        assert_eq!(parsed.len(), 14);
    }

    #[test]
    fn parses_empty_output_as_clean() {
        let repo = status(r"C:\repo", b"");
        assert!(repo.is_clean());
        assert_eq!(repo.state_for_key(""), GitState::Clean);
        assert_eq!(repo.len(), 1);
    }

    #[test]
    fn propagates_dirty_state_up_to_the_root() {
        let repo = status(
            r"C:\repo",
            b" M src/app/main.rs\0?? notes.txt\0!! target/debug/\0",
        );

        assert_eq!(
            repo.state_at(Path::new(r"C:\repo\src\app\main.rs")),
            GitState::Modified
        );
        assert_eq!(repo.state_for_key("src/app/main.rs"), GitState::Modified);
        assert_eq!(repo.state_for_key("src/app"), GitState::Modified);
        assert_eq!(repo.state_for_key("src"), GitState::Modified);
        assert_eq!(repo.state_for_key(""), GitState::Modified);

        assert_eq!(repo.state_for_key("notes.txt"), GitState::Untracked);
        assert_eq!(repo.state_for_key("target/debug"), GitState::Ignored);
        // Ignored entries do not dirty their ancestors.
        assert_eq!(repo.state_for_key("target"), GitState::Clean);

        assert!(!repo.is_clean());
    }

    #[test]
    fn conflict_wins_over_modified_in_ancestors() {
        let repo = status(r"C:\repo", b"UU merge/conflict.txt\0 M merge/other.txt\0");
        assert_eq!(repo.state_for_key("merge"), GitState::Conflict);
        assert_eq!(repo.state_for_key(""), GitState::Conflict);
    }

    #[test]
    fn untracked_directory_is_normalized() {
        let repo = status(r"C:\repo", b"?? newdir/\0");
        assert_eq!(repo.state_for_key("newdir"), GitState::Untracked);
        assert_eq!(repo.state_for_key(""), GitState::Modified);
    }

    #[test]
    fn path_lookup_is_case_insensitive_and_outside_paths_are_clean() {
        let repo = status(r"C:\Repo", b" M src/lib.rs\0");
        assert_eq!(
            repo.state_at(Path::new(r"c:\repo\SRC\Lib.rs")),
            GitState::Modified
        );
        assert_eq!(repo.state_at(Path::new(r"C:\Repo")), GitState::Modified);
        assert_eq!(
            repo.state_at(Path::new(r"C:\elsewhere\file.txt")),
            GitState::Clean
        );
        assert_eq!(
            repo.relative_key(Path::new(r"C:\Repo\src\lib.rs")),
            Some("src/lib.rs".to_string())
        );
    }

    #[test]
    fn finds_enclosing_git_directory() {
        let tree = TempDir::new("find-dir");
        fs::create_dir_all(tree.path().join(".git")).unwrap();
        let nested = tree.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("f.txt");
        fs::write(&file, b"x").unwrap();

        assert_eq!(find_repo_root(&nested), Some(tree.path().to_path_buf()));
        assert_eq!(find_repo_root(&file), Some(tree.path().to_path_buf()));
    }

    #[test]
    fn finds_worktree_git_file() {
        let tree = TempDir::new("worktree");
        let gitdir = tree.path().join("actual-gitdir");
        fs::create_dir_all(&gitdir).unwrap();
        let worktree = tree.path().join("worktree");
        fs::create_dir_all(worktree.join("sub")).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        assert_eq!(git_file_target(&worktree.join(".git")), Some(gitdir));
        assert_eq!(
            find_repo_root(&worktree.join("sub")),
            Some(worktree.clone())
        );
    }

    #[test]
    fn nearest_repository_wins() {
        let tree = TempDir::new("nested");
        fs::create_dir_all(tree.path().join(".git")).unwrap();
        let inner = tree.path().join("inner");
        fs::create_dir_all(inner.join(".git")).unwrap();
        assert_eq!(find_repo_root(&inner), Some(inner.clone()));
    }

    #[test]
    fn no_repository_returns_none() {
        let tree = TempDir::new("none");
        assert_eq!(find_repo_root(tree.path().join("nothing").as_path()), None);
    }

    #[test]
    fn cache_respects_ttl() {
        let mut cache = GitStatusCache::new(4, Duration::from_secs(5));
        let root = Path::new(r"C:\repo");
        let start = Instant::now();
        cache.insert_at(root, status(r"C:\repo", b" M a.txt\0"), start);
        assert!(cache.get_at(root, start + Duration::from_secs(4)).is_some());
        assert!(cache.get_at(root, start + Duration::from_secs(6)).is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_evicts_least_recently_used() {
        let mut cache = GitStatusCache::new(3, Duration::from_secs(60));
        let now = Instant::now();
        let status = |name: &str| {
            GitRepoStatus::from_porcelain(
                PathBuf::from(name),
                format!(" M {name}.txt\0").as_bytes(),
            )
        };

        cache.insert_at(Path::new(r"C:\a"), status("a"), now);
        cache.insert_at(Path::new(r"C:\b"), status("b"), now);
        cache.insert_at(Path::new(r"C:\c"), status("c"), now);
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.cap(), 3);

        // Touch `a` so `b` becomes the least recently used entry.
        assert!(cache.get_at(Path::new(r"C:\a"), now).is_some());
        cache.insert_at(Path::new(r"C:\d"), status("d"), now);

        assert_eq!(cache.len(), 3);
        assert!(cache.get_at(Path::new(r"C:\a"), now).is_some());
        assert!(cache.get_at(Path::new(r"C:\b"), now).is_none());
        assert!(cache.get_at(Path::new(r"C:\c"), now).is_some());
        assert!(cache.get_at(Path::new(r"C:\d"), now).is_some());
    }

    #[test]
    fn cache_invalidate_matches_containing_repository() {
        let mut cache = GitStatusCache::new(4, Duration::from_secs(60));
        cache.insert(Path::new(r"C:\repo"), status(r"C:\repo", b" M a.txt\0"));
        assert!(cache.invalidate(Path::new(r"C:\repo\src\a.txt")));
        assert!(cache.is_empty());
        assert!(!cache.invalidate(Path::new(r"C:\other")));
    }

    #[test]
    fn state_helpers() {
        assert!(GitState::Untracked.is_dirty());
        assert!(!GitState::Ignored.is_dirty());
        assert!(!GitState::Clean.is_dirty());
        assert_eq!(
            GitState::Added.combine(GitState::Modified),
            GitState::Modified
        );
        assert_eq!(
            GitState::Modified.combine(GitState::Conflict),
            GitState::Conflict
        );
        assert_eq!(GitState::Conflict.code(), 'U');
        assert_eq!(GitState::Modified.to_string(), "modified");
    }

    #[test]
    fn engine_status_for_outside_repo_is_clean() {
        let tree = TempDir::new("outside");
        let engine = GitStatusEngine::new();
        assert_eq!(
            engine.status_for(&tree.path().join("file.txt")).unwrap(),
            GitState::Clean
        );
    }

    #[test]
    fn engine_batch_and_real_repository() {
        let engine = GitStatusEngine::new();
        if !engine.is_available() {
            return; // git not on PATH: covered by the unavailable path.
        }

        let tree = TempDir::new("real-repo");
        let init = Command::new("git")
            .arg("-C")
            .arg(tree.path())
            .args(["init", "-q"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !init.map(|s| s.success()).unwrap_or(false) {
            return;
        }

        let committed = tree.path().join("committed.txt");
        fs::write(&committed, b"one").unwrap();
        let untracked = tree.path().join("untracked.txt");
        fs::write(&untracked, b"two").unwrap();

        let paths = vec![
            tree.path().to_path_buf(),
            committed.clone(),
            untracked.clone(),
        ];
        let states = engine.batch(&paths).unwrap();
        // A file in a fresh repo with no commits and an empty index is untracked.
        assert_eq!(states[1], GitState::Untracked);
        assert_eq!(states[2], GitState::Untracked);
        // Untracked children mark the repository root as modified.
        assert_eq!(states[0], GitState::Modified);

        // Cache is populated per repository, not per path.
        assert_eq!(engine.cache_len(), 1);
        engine.invalidate(&untracked);
        assert_eq!(engine.cache_len(), 0);
    }

    #[test]
    fn worker_replies_clean_outside_repository() {
        let tree = TempDir::new("worker");
        let (worker, rx) = GitStatusWorker::start(None);
        let generation = worker.begin_generation();
        let path = tree.path().join("x.txt");
        worker.request(generation, vec![path.clone()]);

        let reply = rx.recv_timeout(Duration::from_secs(20)).expect("git reply");
        assert_eq!(reply.generation, generation);
        assert_eq!(reply.paths, vec![path]);
        assert_eq!(reply.states, vec![GitState::Clean]);
    }

    #[test]
    fn worker_drops_stale_generations_without_replying() {
        let tree = TempDir::new("worker-stale");
        let (worker, rx) = GitStatusWorker::start(None);
        let old = worker.begin_generation();
        let fresh = worker.begin_generation();
        let stale_path = tree.path().join("stale.txt");
        let fresh_path = tree.path().join("fresh.txt");
        // Same channel order: the stale request is handled (and skipped) first.
        worker.request(old, vec![stale_path]);
        worker.request(fresh, vec![fresh_path.clone()]);

        let reply = rx.recv_timeout(Duration::from_secs(20)).expect("git reply");
        assert_eq!(reply.generation, fresh);
        assert_eq!(reply.paths, vec![fresh_path]);
    }
}
