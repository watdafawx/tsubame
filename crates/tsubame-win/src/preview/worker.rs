//! The preview worker: one background thread that decodes whatever the UI
//! currently wants and drops everything it no longer needs.
//!
//! Wiring (Wave 3B):
//!
//! 1. `Alt+P` opens the pane: start a [`PreviewWorker`] and keep the receiver.
//! 2. Selection moved: `worker.begin_generation()` then
//!    `worker.request(generation, path, force_hex, direct_hex_binaries)`.
//!    `request` discards every queued job of an older generation, so holding
//!    `↓` through a folder never builds a backlog of dead previews.
//! 3. The worker sends a [`PreviewReply`]; draw only replies whose generation
//!    is still current (stale ones never reach the queue, but a race with the
//!    UI thread is possible and cheap to check).
//! 4. Pane or tab closed: `worker.clear_cache()` (bitmaps) and
//!    `worker.shutdown()` (thread), which is what keeps the leak gate green.
//!
//! The image LRU lives on the worker ([`PreviewImageCache`]) so switching back
//! to a file the user just left is a cache hit, and [`PreviewWorker::clear_cache`]
//! releases it at once.

use super::image::PreviewImageCache;
use super::{PreviewKind, PreviewOptions, load_preview};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

/// Wake callback invoked after every reply (e.g. `PostMessageW(WM_APP)`).
pub type PreviewWake = Arc<dyn Fn() + Send + Sync + 'static>;

/// One finished (or failed) preview request.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewReply {
    pub generation: u64,
    pub path: PathBuf,
    pub kind: PreviewKind,
}

/// One queued preview request.
struct Job {
    generation: u64,
    path: PathBuf,
    options: PreviewOptions,
    cancel: Arc<AtomicBool>,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    shutdown: AtomicBool,
    generation: AtomicU64,
    tokens: Mutex<Vec<(u64, Arc<AtomicBool>)>>,
    cache: Mutex<PreviewImageCache>,
    /// `[preview] syntax_highlight`, switchable while the pane is open.
    syntax_highlight: AtomicBool,
    tx: Sender<PreviewReply>,
    wake: Option<PreviewWake>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Queues `job`, dropping and cancelling every queued job of an older
/// generation. Returns the number dropped.
fn push_dropping_stale(queue: &mut VecDeque<Job>, job: Job) -> usize {
    let mut dropped = 0;
    let mut kept = VecDeque::with_capacity(queue.len() + 1);
    while let Some(old) = queue.pop_front() {
        if old.generation >= job.generation {
            kept.push_back(old);
        } else {
            old.cancel.store(true, Ordering::Release);
            dropped += 1;
        }
    }
    kept.push_back(job);
    *queue = kept;
    dropped
}

/// One-thread preview engine.
///
/// Dropping the worker cancels queued work and joins the thread.
pub struct PreviewWorker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl PreviewWorker {
    /// Starts the worker with the default 32 MiB / 32-item image cache and no
    /// wake callback.
    pub fn start() -> (PreviewWorker, Receiver<PreviewReply>) {
        PreviewWorker::with_options(
            super::image::DEFAULT_CACHE_BYTES,
            super::image::DEFAULT_CACHE_ITEMS,
            None,
        )
    }

    /// Starts the worker with an image cache capped at `cache_bytes` /
    /// `cache_items`, and an optional wake callback.
    pub fn with_options(cache_bytes: usize, cache_items: usize, wake: Option<PreviewWake>) -> (PreviewWorker, Receiver<PreviewReply>) {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            shutdown: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            tokens: Mutex::new(Vec::new()),
            cache: Mutex::new(PreviewImageCache::new(cache_bytes, cache_items)),
            syntax_highlight: AtomicBool::new(true),
            tx,
            wake,
        });
        let worker = PreviewWorker {
            shared: Arc::clone(&shared),
            thread: thread::Builder::new()
                .name("tsubame-preview".into())
                .spawn(move || worker_loop(shared))
                .ok(),
        };
        (worker, rx)
    }

    /// Current generation; replies older than this are stale.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Advances the generation and cancels every queued or running job of an
    /// older one. Returns the new generation.
    pub fn begin_generation(&self) -> u64 {
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mut tokens = lock(&self.shared.tokens);
        for (age, token) in tokens.drain(..) {
            if age < generation {
                token.store(true, Ordering::Release);
            }
        }
        generation
    }

    /// Queues `path` for `generation`.
    ///
    /// Stale queued requests (older generations) are dropped, so a burst of
    /// arrow-key moves leaves exactly one preview to compute. Replies carry
    /// `generation` and `path`.
    pub fn request(&self, generation: u64, path: impl Into<PathBuf>, force_hex: bool, direct_hex_binaries: bool) {
        let token = self.token_for(generation);
        let job = Job {
            generation,
            path: path.into(),
            options: PreviewOptions {
                force_hex,
                direct_hex_binaries,
                syntax_highlight: self.shared.syntax_highlight.load(Ordering::Relaxed),
            },
            cancel: token,
        };
        {
            let mut queue = lock(&self.shared.queue);
            push_dropping_stale(&mut queue, job);
        }
        self.shared.cv.notify_one();
    }

    /// Jobs waiting for the thread.
    pub fn pending(&self) -> usize {
        lock(&self.shared.queue).len()
    }

    /// Bitmaps currently cached.
    pub fn cache_len(&self) -> usize {
        lock(&self.shared.cache).len()
    }

    /// Bytes of cached bitmaps.
    pub fn cache_bytes(&self) -> usize {
        lock(&self.shared.cache).bytes()
    }

    /// Releases every cached bitmap (pane or tab closed).
    pub fn clear_cache(&self) {
        lock(&self.shared.cache).clear();
    }

    /// Turns the 6-token highlighter on or off for later requests (Settings
    /// toggle). The request in flight keeps the setting it started with.
    pub fn set_syntax_highlight(&self, on: bool) {
        self.shared.syntax_highlight.store(on, Ordering::Relaxed);
    }

    pub fn syntax_highlight(&self) -> bool {
        self.shared.syntax_highlight.load(Ordering::Relaxed)
    }

    /// Cancels everything, joins the thread and drops the receiver.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn token_for(&self, generation: u64) -> Arc<AtomicBool> {
        let mut tokens = lock(&self.shared.tokens);
        if let Some((_, token)) = tokens.iter().find(|(age, _)| *age == generation) {
            return Arc::clone(token);
        }
        let token = Arc::new(AtomicBool::new(false));
        tokens.push((generation, Arc::clone(&token)));
        token
    }

    fn stop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        let mut tokens = lock(&self.shared.tokens);
        for (_, token) in tokens.drain(..) {
            token.store(true, Ordering::Release);
        }
        drop(tokens);
        lock(&self.shared.queue).clear();
        self.shared.cv.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if shared.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = shared.cv.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        if is_stale(&shared, &job) {
            continue;
        }
        let kind = {
            let mut cache = lock(&shared.cache);
            load_preview(&job.path, job.options, &mut cache)
        };
        // A newer selection arrived while this one decoded: the reply would
        // paint over the current file, so drop it.
        if is_stale(&shared, &job) {
            continue;
        }
        let reply = PreviewReply { generation: job.generation, path: job.path, kind };
        if shared.tx.send(reply).is_err() {
            return;
        }
        if let Some(wake) = &shared.wake {
            wake();
        }
    }
}

fn is_stale(shared: &Shared, job: &Job) -> bool {
    job.cancel.load(Ordering::Acquire) || job.generation < shared.generation.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-preview-worker-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn job(generation: u64, name: &str) -> Job {
        Job {
            generation,
            path: PathBuf::from(name),
            options: PreviewOptions::default(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Waits for the reply of generation `generation` (and its path).
    fn recv_generation(rx: &Receiver<PreviewReply>, generation: u64) -> PreviewReply {
        loop {
            let reply = rx.recv_timeout(Duration::from_secs(10)).expect("a preview reply");
            if reply.generation == generation {
                return reply;
            }
        }
    }

    #[test]
    fn a_burst_of_requests_keeps_only_the_newest() {
        let mut queue = VecDeque::new();
        let first = job(1, "a");
        let first_token = Arc::clone(&first.cancel);
        assert_eq!(push_dropping_stale(&mut queue, first), 0);
        let second = job(1, "b");
        let second_token = Arc::clone(&second.cancel);
        assert_eq!(push_dropping_stale(&mut queue, second), 0, "same generation queues up");
        assert_eq!(queue.len(), 2);

        let third = job(2, "c");
        assert_eq!(push_dropping_stale(&mut queue, third), 2, "older generations are dropped");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].path, PathBuf::from("c"));
        assert!(first_token.load(Ordering::Acquire) && second_token.load(Ordering::Acquire), "dropped jobs are cancelled");
        assert!(!queue[0].cancel.load(Ordering::Acquire));
    }

    #[test]
    fn the_newest_generation_is_the_only_reply() {
        let d = temp_dir("newest");
        let a = d.join("a.txt");
        let b = d.join("b.txt");
        std::fs::write(&a, "first\n").unwrap();
        std::fs::write(&b, "second\n").unwrap();
        let (worker, rx) = PreviewWorker::start();
        worker.request(1, &a, false, true);
        worker.request(2, &b, false, true);
        let reply = recv_generation(&rx, 2);
        assert_eq!(reply.path, b);
        assert_eq!(reply.generation, 2);
        assert!(matches!(reply.kind, PreviewKind::Text { .. }));
        // Nothing stale is left behind.
        assert_eq!(worker.pending(), 0);
        assert_eq!(worker.generation(), 0, "request does not advance the generation");
        worker.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn begin_generation_cancels_queued_work() {
        let d = temp_dir("cancel");
        let a = d.join("a.txt");
        std::fs::write(&a, "old\n").unwrap();
        let (worker, rx) = PreviewWorker::start();
        let generation = worker.begin_generation();
        worker.request(generation, &a, false, true);
        let newer = worker.begin_generation();
        assert!(newer > generation);
        worker.request(newer, &a, false, true);
        let reply = recv_generation(&rx, newer);
        assert_eq!(reply.path, a);
        worker.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn forced_hex_and_cache_reuse_flow_through_the_worker() {
        let d = temp_dir("cache");
        let bmp = d.join("a.bmp");
        std::fs::write(&bmp, super::super::image::tiny_bmp(4, 4, [0x10, 0x20, 0x30])).unwrap();
        let (worker, rx) = PreviewWorker::start();
        worker.request(1, &bmp, false, true);
        let reply = recv_generation(&rx, 1);
        assert!(matches!(reply.kind, PreviewKind::Image { .. }));
        assert_eq!(worker.cache_len(), 1);
        assert!(worker.cache_bytes() > 0);
        worker.request(2, &bmp, false, true);
        let reply = recv_generation(&rx, 2);
        assert!(matches!(reply.kind, PreviewKind::Image { .. }));
        assert_eq!(worker.cache_len(), 1, "the second request is a cache hit, not a second entry");

        worker.request(3, &bmp, true, true);
        let reply = recv_generation(&rx, 3);
        assert!(matches!(reply.kind, PreviewKind::Hex { .. }), "force_hex wins");

        worker.clear_cache();
        assert_eq!(worker.cache_len(), 0);
        worker.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_highlighter_toggle_reaches_the_worker() {
        let d = temp_dir("syntax");
        let p = d.join("a.rs");
        std::fs::write(&p, "fn main() {}\n").unwrap();
        let (worker, rx) = PreviewWorker::start();
        assert!(worker.syntax_highlight());
        worker.set_syntax_highlight(false);
        assert!(!worker.syntax_highlight());
        worker.request(1, &p, false, true);
        match recv_generation(&rx, 1).kind {
            PreviewKind::Text { lines, .. } => {
                assert_eq!(lines.len(), 1);
                assert!(lines[0].spans.iter().all(|s| s.kind == super::super::TokenKind::Plain), "tinting is off");
            }
            other => panic!("expected Text, got {other:?}"),
        }
        worker.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn shutdown_stops_the_thread_and_closes_the_channel() {
        let (worker, rx) = PreviewWorker::start();
        assert_eq!(worker.pending(), 0);
        worker.shutdown();
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(1)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected)),
            "the sender is dropped when the worker stops"
        );
    }
}
