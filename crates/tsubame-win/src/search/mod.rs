//! Global search engines for Tsubame (P4.F1).
//!
//! Provides a unified [`GlobalSearch`] dispatcher over:
//! - [`everything`]: Voidtools Everything IPC (`WM_COPYDATA`, `QUERY2W` with v1
//!   fallback) via a temporary hidden `HWND_MESSAGE` on a worker thread.
//! - [`walk`]: Bounded parallel directory walk fallback (`NtQueryDirectoryFile`
//!   with `FindFirstFileExW` fallback, symlink/junction loop safe).
//! - [`mft`]: Optional NTFS MFT elevation/filesystem probe ([`mft_available`]).
//!
//! Following `docs/PLAN.md` §3 rule 1, the UI thread never runs searches
//! directly: [`GlobalSearch::start`] executes on a background worker thread,
//! streams batches over an `mpsc::Receiver<SearchMessage>`, invokes an optional
//! wake callback, and tags every message with a generation number so stale
//! queries cancel and drop immediately.

pub mod everything;
pub mod mft;
pub mod query;
pub mod walk;

pub use mft::{is_ntfs_volume, is_process_elevated, mft_available};
pub use query::CompiledQuery;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Default result cap when constructing a [`SearchQuery`].
pub const DEFAULT_MAX_RESULTS: usize = 10_000;

/// First streamed batch cap for low time-to-first-result latency.
const FIRST_BATCH_SIZE: usize = 64;
/// Subsequent streamed batch cap.
const STREAM_BATCH_SIZE: usize = 256;
/// Max age of a non-empty pending batch before flushing.
const STREAM_BATCH_INTERVAL: Duration = Duration::from_millis(16);

/// Parameters for a global or folder-scoped search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    /// Optional root folder to scope the search to; `None` searches all local fixed drives / Everything index.
    pub root: Option<PathBuf>,
    pub max_results: usize,
    pub case_sensitive: bool,
    pub regex: bool,
    pub include_dirs: bool,
    pub include_files: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            root: None,
            max_results: DEFAULT_MAX_RESULTS,
            case_sensitive: false,
            regex: false,
            include_dirs: true,
            include_files: true,
        }
    }
}

impl SearchQuery {
    /// Create a default query for `text`.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// One matching file or directory returned by a search engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Last write time in Windows FILETIME ticks (100 ns since 1601-01-01 UTC), or `0` if unavailable.
    pub modified: i64,
}

/// Which backend executed a search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchBackend {
    Everything,
    Walk,
}

/// Engine selection policy for [`GlobalSearch`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchMode {
    /// Use Everything IPC when running, falling back to parallel walk otherwise.
    #[default]
    Auto,
    /// Use Everything IPC only (returns [`SearchError::Unavailable`] if not running).
    EverythingOnly,
    /// Always use the parallel directory walk engine.
    WalkOnly,
}

/// Summary returned when a synchronous search completes.
#[derive(Clone, Debug)]
pub struct SearchSummary {
    pub backend: SearchBackend,
    pub total_hits: usize,
    pub cancelled: bool,
    pub elapsed: Duration,
}

/// Error returned by a search backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    /// The requested backend (e.g. Everything IPC) is not running/available.
    Unavailable,
    /// The backend did not respond within the configured timeout.
    Timeout,
    /// The search was cancelled via token, generation bump, or callback returning `false`.
    Cancelled,
    /// The query option is not supported by the selected backend (e.g. regex in walk mode).
    Unsupported(String),
    /// Everything IPC transport or framing failure.
    Ipc(String),
    /// Filesystem / root error during directory walk.
    Io(String),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchError::Unavailable => f.write_str("search provider unavailable"),
            SearchError::Timeout => f.write_str("search timed out"),
            SearchError::Cancelled => f.write_str("search cancelled"),
            SearchError::Unsupported(s) => write!(f, "unsupported search option: {s}"),
            SearchError::Ipc(s) => write!(f, "search IPC error: {s}"),
            SearchError::Io(s) => write!(f, "search I/O error: {s}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Shared generation counter for cancelling stale searches immediately
/// (PLAN.md §3 rule 1).
#[derive(Clone, Debug, Default)]
pub struct SearchGeneration {
    live: Arc<AtomicU64>,
}

impl SearchGeneration {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance to a fresh generation, invalidating every earlier search.
    pub fn next_generation(&self) -> u64 {
        self.live.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Invalidate every search started so far.
    pub fn cancel(&self) {
        self.live.fetch_add(1, Ordering::AcqRel);
    }

    /// Current live generation.
    pub fn current(&self) -> u64 {
        self.live.load(Ordering::Acquire)
    }

    /// True while `generation` is still the active generation.
    #[inline]
    pub fn is_current(&self, generation: u64) -> bool {
        self.live.load(Ordering::Relaxed) == generation
    }
}

/// Completion metadata sent as the final [`SearchMessage`] of a streamed search.
#[derive(Clone, Debug)]
pub struct SearchDone {
    pub generation: u64,
    pub backend: Option<SearchBackend>,
    pub total_hits: usize,
    pub cancelled: bool,
    pub elapsed: Duration,
    pub error: Option<SearchError>,
}

/// One message delivered over the streaming channel returned by [`GlobalSearch::start`].
#[derive(Clone, Debug)]
pub enum SearchMessage {
    Batch {
        generation: u64,
        hits: Vec<SearchHit>,
    },
    Done(SearchDone),
}

impl SearchMessage {
    pub fn generation(&self) -> u64 {
        match self {
            SearchMessage::Batch { generation, .. } => *generation,
            SearchMessage::Done(done) => done.generation,
        }
    }
}

/// Optional wake callback invoked after each channel send (e.g. `PostMessageW`).
pub type Wake = Arc<dyn Fn() + Send + Sync + 'static>;

/// Handle to a background search started by [`GlobalSearch::start`].
///
/// Dropping cancels and detaches so the UI thread never blocks waiting for I/O.
pub struct SearchTask {
    generation: u64,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SearchTask {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Cancel this search immediately.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// Cancel and wait for the worker thread to exit (tests and shutdown only).
    pub fn join(mut self) {
        self.cancel();
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for SearchTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Unified global search dispatcher combining Everything IPC and parallel walk.
#[derive(Clone, Debug)]
pub struct GlobalSearch {
    pub mode: SearchMode,
    pub everything_timeout: Duration,
    pub walk_workers: Option<usize>,
}

impl Default for GlobalSearch {
    fn default() -> Self {
        Self {
            mode: SearchMode::Auto,
            everything_timeout: everything::DEFAULT_TIMEOUT,
            walk_workers: None,
        }
    }
}

impl GlobalSearch {
    pub fn new(mode: SearchMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    pub fn auto() -> Self {
        Self::new(SearchMode::Auto)
    }

    /// True if Voidtools Everything is currently running and reachable via IPC.
    pub fn everything_available(&self) -> bool {
        everything::is_available()
    }

    /// Synchronous streaming search on the current (worker) thread.
    ///
    /// Streams matches into `on_hit`; returning `false` from `on_hit` or setting
    /// `cancel` to `true` stops the search promptly.
    pub fn search<F>(
        &self,
        query: &SearchQuery,
        cancel: &AtomicBool,
        mut on_hit: F,
    ) -> Result<SearchSummary, SearchError>
    where
        F: FnMut(SearchHit) -> bool + Send,
    {
        if cancel.load(Ordering::Relaxed) {
            return Err(SearchError::Cancelled);
        }
        match self.mode {
            SearchMode::EverythingOnly => {
                everything::search(query, self.everything_timeout, cancel, on_hit)
            }
            SearchMode::WalkOnly => walk::search(query, self.walk_workers, cancel, on_hit),
            SearchMode::Auto => {
                if everything::is_available() {
                    match everything::search(query, self.everything_timeout, cancel, &mut on_hit) {
                        Ok(summary) => return Ok(summary),
                        Err(SearchError::Cancelled) => return Err(SearchError::Cancelled),
                        Err(
                            SearchError::Unavailable
                            | SearchError::Timeout
                            | SearchError::Ipc(_),
                        ) => {
                            if cancel.load(Ordering::Relaxed) {
                                return Err(SearchError::Cancelled);
                            }
                            // Clean fallback to parallel walk.
                        }
                        Err(other) => return Err(other),
                    }
                }
                walk::search(query, self.walk_workers, cancel, on_hit)
            }
        }
    }

    /// Start an asynchronous streaming search on a dedicated worker thread.
    ///
    /// Every emitted [`SearchMessage`] carries `generation`. If `gen_token`
    /// advances past `generation`, the worker cancels immediately and drops any
    /// unsent buffered hits before sending `SearchMessage::Done { cancelled: true, .. }`.
    pub fn start(
        &self,
        query: SearchQuery,
        generation: u64,
        gen_token: SearchGeneration,
        wake: Option<Wake>,
    ) -> (SearchTask, Receiver<SearchMessage>) {
        let (tx, rx) = channel();
        let task = self.start_with_sender(query, generation, gen_token, tx, wake);
        (task, rx)
    }

    /// Same as [`GlobalSearch::start`], delivering messages to an existing `Sender`.
    pub fn start_with_sender(
        &self,
        query: SearchQuery,
        generation: u64,
        gen_token: SearchGeneration,
        tx: Sender<SearchMessage>,
        wake: Option<Wake>,
    ) -> SearchTask {
        let engine = self.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);

        let worker = thread::Builder::new()
            .name("tsubame-search".to_string())
            .spawn(move || {
                run_streamed_worker(engine, query, generation, gen_token, worker_cancel, tx, wake);
            })
            .ok();

        SearchTask {
            generation,
            cancel,
            worker,
        }
    }
}

fn run_streamed_worker(
    engine: GlobalSearch,
    query: SearchQuery,
    generation: u64,
    gen_token: SearchGeneration,
    cancel: Arc<AtomicBool>,
    tx: Sender<SearchMessage>,
    wake: Option<Wake>,
) {
    let started = Instant::now();
    let mut pending = Vec::with_capacity(FIRST_BATCH_SIZE);
    let mut sent_first = false;
    let mut last_flush = Instant::now();
    let mut delivered = 0usize;
    let mut consumer_gone = false;

    let is_stale = |c: &AtomicBool| c.load(Ordering::Relaxed) || !gen_token.is_current(generation);

    let notify = |w: &Option<Wake>| {
        if let Some(cb) = w {
            cb();
        }
    };

    let flush = |pending: &mut Vec<SearchHit>,
                 delivered: &mut usize,
                 sent_first: &mut bool,
                 last_flush: &mut Instant,
                 consumer_gone: &mut bool|
     -> bool {
        if pending.is_empty() {
            return true;
        }
        if is_stale(&cancel) {
            pending.clear();
            return false;
        }
        let batch = std::mem::take(pending);
        let n = batch.len();
        if tx
            .send(SearchMessage::Batch {
                generation,
                hits: batch,
            })
            .is_ok()
        {
            *delivered += n;
            *sent_first = true;
            *last_flush = Instant::now();
            notify(&wake);
            true
        } else {
            *consumer_gone = true;
            cancel.store(true, Ordering::Release);
            false
        }
    };

    // Bridge generation bumps into the `&AtomicBool` checked by the engines.
    let result = if is_stale(&cancel) {
        cancel.store(true, Ordering::Release);
        Err(SearchError::Cancelled)
    } else {
        engine.search(&query, &cancel, |hit| {
            if is_stale(&cancel) {
                cancel.store(true, Ordering::Release);
                pending.clear();
                return false;
            }
            pending.push(hit);
            let limit = if sent_first {
                STREAM_BATCH_SIZE
            } else {
                FIRST_BATCH_SIZE
            };
            if pending.len() >= limit || last_flush.elapsed() >= STREAM_BATCH_INTERVAL {
                return flush(
                    &mut pending,
                    &mut delivered,
                    &mut sent_first,
                    &mut last_flush,
                    &mut consumer_gone,
                );
            }
            true
        })
    };

    let (backend, cancelled, error) = match result {
        Ok(summary) => {
            if !is_stale(&cancel) && !consumer_gone {
                let _ = flush(
                    &mut pending,
                    &mut delivered,
                    &mut sent_first,
                    &mut last_flush,
                    &mut consumer_gone,
                );
            } else {
                pending.clear();
            }
            let was_cancelled = summary.cancelled || is_stale(&cancel) || consumer_gone;
            (Some(summary.backend), was_cancelled, None)
        }
        Err(SearchError::Cancelled) => {
            pending.clear();
            (None, true, None)
        }
        Err(e) => {
            pending.clear();
            (None, is_stale(&cancel) || consumer_gone, Some(e))
        }
    };

    let done = SearchDone {
        generation,
        backend,
        total_hits: delivered,
        cancelled,
        elapsed: started.elapsed(),
        error,
    };
    if tx.send(SearchMessage::Done(done)).is_ok() {
        notify(&wake);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicUsize;

    fn make_temp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "tsubame-globalsearch-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).expect("create temp dir");
        p
    }

    #[test]
    fn global_search_streaming_and_generation_drop() {
        let root = make_temp_dir("stream");
        for i in 0..40 {
            fs::write(root.join(format!("item_{i:02}.rs")), b"fn f() {}").unwrap();
        }

        let gs = GlobalSearch::new(SearchMode::WalkOnly);
        let gen_token = SearchGeneration::new();
        let g1 = gen_token.next_generation();
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_c = Arc::clone(&wakes);
        let (task, rx) = gs.start(
            SearchQuery {
                text: "*.rs".into(),
                root: Some(root.clone()),
                max_results: 100,
                ..SearchQuery::default()
            },
            g1,
            gen_token.clone(),
            Some(Arc::new(move || {
                wakes_c.fetch_add(1, Ordering::Relaxed);
            })),
        );

        let mut total = 0usize;
        let done = loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("msg") {
                SearchMessage::Batch { generation, hits } => {
                    assert_eq!(generation, g1);
                    total += hits.len();
                }
                SearchMessage::Done(d) => break d,
            }
        };
        task.join();

        assert_eq!(done.generation, g1);
        assert_eq!(done.backend, Some(SearchBackend::Walk));
        assert_eq!(done.total_hits, 40);
        assert_eq!(total, 40);
        assert!(!done.cancelled);
        assert!(done.error.is_none());
        assert!(wakes.load(Ordering::Relaxed) >= 2);

        // Stale generation before start drops immediately without sending batches.
        let g_old = gen_token.next_generation();
        let _g_newer = gen_token.next_generation();
        let (stale_task, stale_rx) = gs.start(
            SearchQuery {
                text: "*.rs".into(),
                root: Some(root.clone()),
                ..SearchQuery::default()
            },
            g_old,
            gen_token,
            None,
        );
        let mut stale_batches = 0usize;
        let stale_done = loop {
            match stale_rx.recv_timeout(Duration::from_secs(10)).expect("stale msg") {
                SearchMessage::Batch { .. } => stale_batches += 1,
                SearchMessage::Done(d) => break d,
            }
        };
        stale_task.join();
        assert_eq!(stale_batches, 0);
        assert!(stale_done.cancelled);
        assert_eq!(stale_done.total_hits, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn everything_only_graceful_when_unavailable() {
        if everything::is_available() {
            // Everything happens to be running on this machine; skip the unavailable assertion.
            return;
        }
        let gs = GlobalSearch::new(SearchMode::EverythingOnly);
        let cancel = AtomicBool::new(false);
        let res = gs.search(&SearchQuery::new("test"), &cancel, |_| true);
        assert_eq!(res.unwrap_err(), SearchError::Unavailable);
    }
}
