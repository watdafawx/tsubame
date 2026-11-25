//! The tags worker: one thread that owns `tags.db`. The UI thread never
//! touches the store or the disk for tags; it sends requests and drains
//! replies (`try_recv`), and the worker wakes it after each reply.
//!
//! Cost rules (P4.F6):
//! - idle: the thread blocks on `recv` (no timer) unless a save is due
//!   (`recv_timeout` while dirty) or a reconcile is running;
//! - listing: one batched lookup per visible page, answered from memory
//!   (the enumerator's file id is the key; the disk is touched only for a row
//!   that has none, e.g. a file the watcher just added);
//! - writes are coalesced: one atomic save after `SAVE_AFTER` of quiet;
//! - the one-time reconcile per volume runs in small slices between requests
//!   and pauses while the UI scrolls.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, UNIX_EPOCH};

use tsubame_win::enumerate::Wake;

use super::model::{Edit, MAX_TAGS, Shape, TagDef};
use super::store::{self, Db, Fs, Key, Mode, Recon, Store, StoreError, Vol};

const SAVE_AFTER: Duration = Duration::from_millis(500);
/// Reconcile items checked between two looks at the request queue.
const SLICE: usize = 128;
/// The reconcile pauses this long after the last scroll.
const SCROLL_QUIET_MS: u64 = 400;
/// Most rows a tag filter lists.
pub const MAX_RESULTS: usize = 20_000;

/// A row the UI wants the tags of.
#[derive(Clone, Debug)]
pub struct Row {
    /// the UI cache key
    pub ck: u64,
    pub path: String,
    /// from the enumerator (`0`: unknown)
    pub fid: u128,
}

pub enum Req {
    Lookup { dir: String, rows: Vec<Row> },
    /// picker / drop / clear: `edit` on every file
    Apply { files: Vec<(String, u128)>, edit: Edit },
    /// New tag, applied to `files` at once ("Create and apply")
    Create { name: String, colour: u8, shape: Shape, files: Vec<(String, u128)> },
    Rename { slot: u8, name: String },
    Look { slot: u8, colour: u8, shape: Shape },
    Delete { slot: u8 },
    Move { slot: u8, by: i32 },
    /// a file or folder moved (watcher, own rename)
    Moved { from: String, to: String },
    Query { seq: u64, slot: u8 },
    Export(PathBuf),
    Import(PathBuf),
    Retry,
}

/// What the UI shows besides row marks.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub defs: Vec<TagDef>,
    pub counts: [u32; MAX_TAGS],
    pub mode: Mode,
    pub note: Option<String>,
    pub items: usize,
    pub usage: f32,
}

pub struct Hit {
    pub path: String,
    pub mask: u64,
    pub size: u64,
    /// unix seconds (0: unknown)
    pub modified: u32,
    pub dir: bool,
}

pub enum Resp {
    State(Box<Snapshot>),
    Masks(Vec<(u64, u64)>),
    /// tags changed on disk: drop cached masks
    Changed,
    Results { seq: u64, slot: u8, hits: Vec<Hit>, capped: bool },
    /// reconcile progress (`None`: done)
    Progress(Option<(usize, usize)>),
    Message(String),
}

/// The real file system.
pub struct WinFs;

impl Fs for WinFs {
    fn volume(&self, path: &str) -> Option<Vol> {
        let v = tsubame_win::fileid::volume_of(path)?;
        Some(Vol { serial: v.serial, stable: v.ids_stable(), root: v.root })
    }
    fn file_id(&self, path: &str) -> Option<u128> {
        tsubame_win::fileid::file_id(path).map(|x| x.1)
    }
    fn path_by_id(&self, vol: &Vol, fid: u128) -> Option<String> {
        tsubame_win::fileid::path_by_id(&vol.root, fid)
    }
    fn exists(&self, path: &str) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }
}

pub struct Worker {
    pub tx: Sender<Req>,
    pub rx: Receiver<Resp>,
    /// last UI scroll, ms since the worker started (the reconcile pauses)
    pub scroll: Arc<AtomicU64>,
    pub t0: Instant,
}

impl Worker {
    pub fn start(path: PathBuf, wake: Option<Wake>) -> Option<Worker> {
        let (tx, req_rx) = channel();
        let (resp_tx, rx) = channel();
        let scroll = Arc::new(AtomicU64::new(0));
        let t0 = Instant::now();
        let s2 = scroll.clone();
        std::thread::Builder::new()
            .name("tsubame-tags".into())
            .spawn(move || {
                let mut w = Run { db: Db::open(&path), fs: Box::new(WinFs), tx: resp_tx, wake, vols: HashMap::new(), seen: HashSet::new(), queue: VecDeque::new(), recon: None, scroll: s2, t0, dirty_at: None };
                w.run(req_rx);
            })
            .ok()?;
        Some(Worker { tx, rx, scroll, t0 })
    }

    pub fn scrolled(&self) {
        self.scroll.store(self.t0.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
    }
}

struct Recon_ {
    vol: Vol,
    keys: Vec<Key>,
    at: usize,
    r: Recon,
}

pub(crate) struct Run {
    pub(crate) db: Db,
    pub(crate) fs: Box<dyn Fs + Send>,
    tx: Sender<Resp>,
    wake: Option<Wake>,
    /// folder -> volume (one volume query per folder per session)
    vols: HashMap<String, Option<Vol>>,
    /// volume roots reconciled (or queued) this session
    seen: HashSet<String>,
    queue: VecDeque<Vol>,
    recon: Option<Recon_>,
    scroll: Arc<AtomicU64>,
    t0: Instant,
    dirty_at: Option<Instant>,
}

impl Run {
    fn send(&self, r: Resp) {
        let _ = self.tx.send(r);
        if let Some(w) = &self.wake {
            w();
        }
    }

    fn snapshot(&self) -> Snapshot {
        let s = &self.db.store;
        Snapshot { defs: s.defs.clone(), counts: s.counts(), mode: self.db.mode.clone(), note: self.db.note.clone(), items: s.len(), usage: s.usage() }
    }

    fn state(&self) {
        self.send(Resp::State(Box::new(self.snapshot())));
    }

    fn run(&mut self, rx: Receiver<Req>) {
        self.state();
        loop {
            let busy = self.recon.is_some() || !self.queue.is_empty();
            let msg = if busy {
                match rx.try_recv() {
                    Ok(m) => Some(m),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(_) => break,
                }
            } else if let Some(at) = self.dirty_at {
                match rx.recv_timeout(SAVE_AFTER.saturating_sub(at.elapsed())) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(_) => break,
                }
            } else {
                match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => break,
                }
            };
            match msg {
                Some(m) => self.handle(m),
                None if busy => self.reconcile_slice(),
                None => {}
            }
            self.note_dirty();
            if self.dirty_at.is_some_and(|t| t.elapsed() >= SAVE_AFTER) {
                self.save();
            }
        }
        if let Err(e) = self.db.close() {
            eprintln!("tags: cannot save tags.db: {e}");
        }
    }

    fn note_dirty(&mut self) {
        if self.db.store.dirty && self.db.writable() {
            self.dirty_at.get_or_insert_with(Instant::now);
        } else {
            self.dirty_at = None;
        }
    }

    fn save(&mut self) {
        self.dirty_at = None;
        if let Err(e) = self.db.save() {
            self.db.mode = Mode::Unwritable(e.to_string());
            self.db.note = Some(format!("tags.db cannot be saved: {e}"));
            self.state();
        }
    }

    fn vol_for(&mut self, dir: &str) -> Option<Vol> {
        if let Some(v) = self.vols.get(dir) {
            return v.clone();
        }
        if self.vols.len() > 1024 {
            self.vols.clear();
        }
        let v = self.fs.volume(dir);
        self.vols.insert(dir.to_string(), v.clone());
        if let Some(v) = &v {
            let root = store::norm(&v.root);
            if !self.seen.contains(&root) && !self.db.store.is_empty() {
                self.seen.insert(root);
                self.queue.push_back(v.clone());
            }
        }
        v
    }

    fn key_of(&mut self, path: &str, fid: u128) -> Option<(Vol, Key)> {
        let dir = parent(path);
        let vol = self.vol_for(dir)?;
        let fid = if fid == 0 && vol.stable { self.fs.file_id(path).unwrap_or(0) } else { fid };
        let key = Store::key_for(&vol, path, fid);
        Some((vol, key))
    }

    pub(crate) fn handle(&mut self, m: Req) {
        match m {
            Req::Lookup { dir, rows } => {
                let mut out = Vec::with_capacity(rows.len());
                if self.db.store.is_empty() {
                    out.extend(rows.iter().map(|r| (r.ck, 0)));
                } else if let Some(vol) = self.vol_for(&dir) {
                    for r in rows {
                        let fid = if r.fid == 0 && vol.stable { self.fs.file_id(&r.path).unwrap_or(0) } else { r.fid };
                        let key = Store::key_for(&vol, &r.path, fid);
                        out.push((r.ck, self.db.store.lookup(&key, &r.path)));
                    }
                } else {
                    out.extend(rows.iter().map(|r| (r.ck, 0)));
                }
                self.send(Resp::Masks(out));
            }
            Req::Apply { files, edit } => {
                if self.refuse() {
                    return;
                }
                let (n, full) = self.apply(&files, edit);
                if full > 0 {
                    self.send(Resp::Message(format!("Tag store full ({} items): {full} file(s) not tagged", store::MAX_ITEMS)));
                } else if n > 0 && self.db.store.usage() >= store::WARN_AT {
                    self.send(Resp::Message(format!("Tag store {:.0}% full", self.db.store.usage() * 100.0)));
                }
                self.changed();
            }
            Req::Create { name, colour, shape, files } => {
                if self.refuse() {
                    return;
                }
                match self.db.store.add_tag(&name, colour, shape) {
                    Ok(slot) => {
                        let bit = 1u64 << slot;
                        self.apply(&files, Edit { on: bit, off: 0 });
                        self.changed();
                    }
                    Err(e) => self.send(Resp::Message(err_text(e, &name))),
                }
            }
            Req::Rename { slot, name } => {
                if !self.refuse() {
                    if let Err(e) = self.db.store.rename_tag(slot, &name) {
                        self.send(Resp::Message(err_text(e, &name)));
                    }
                    self.state();
                }
            }
            Req::Look { slot, colour, shape } => {
                if !self.refuse() {
                    let _ = self.db.store.set_look(slot, colour, shape);
                    self.state();
                }
            }
            Req::Move { slot, by } => {
                if !self.refuse() {
                    self.db.store.move_tag(slot, by);
                    self.state();
                }
            }
            Req::Delete { slot } => {
                if !self.refuse() {
                    let _ = self.db.store.delete_tag(slot);
                    self.changed();
                }
            }
            Req::Moved { from, to } => {
                if self.db.writable() && !self.db.store.is_empty() && self.db.store.rename(&from, &to) > 0 {
                    self.changed();
                }
            }
            Req::Query { seq, slot } => {
                let all = self.db.store.query(1u64 << (slot as u32 % 64));
                let capped = all.len() > MAX_RESULTS;
                let hits = all
                    .into_iter()
                    .take(MAX_RESULTS)
                    .map(|(path, mask)| {
                        let md = std::fs::metadata(&path).ok();
                        let modified = md.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs().min(u32::MAX as u64) as u32);
                        Hit { size: md.as_ref().map_or(0, |m| m.len()), dir: md.as_ref().is_some_and(|m| m.is_dir()), path, mask, modified }
                    })
                    .collect();
                self.send(Resp::Results { seq, slot, hits, capped });
            }
            Req::Export(path) => {
                let text = self.db.store.export_json();
                match tsubame_config::write_atomic(&path, text.as_bytes()) {
                    Ok(()) => self.send(Resp::Message(format!("Exported {} tagged items to {}", self.db.store.len(), path.display()))),
                    Err(e) => self.send(Resp::Message(format!("Export failed: {e}"))),
                }
            }
            Req::Import(path) => {
                if self.refuse() {
                    return;
                }
                let text = match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(e) => return self.send(Resp::Message(format!("Import failed: {e}"))),
                };
                let mut keys: Vec<(String, Option<Key>)> = Vec::new();
                // resolve on this thread first (the store is borrowed by the import)
                if let Ok(root) = super::json::parse(&text) {
                    for it in root.get("items").and_then(super::json::J::as_arr).unwrap_or(&[]) {
                        if let Some(p) = it.get("path").and_then(super::json::J::as_str) {
                            let k = if self.fs.exists(p) { self.key_of(p, 0).map(|x| x.1) } else { None };
                            keys.push((p.to_string(), k));
                        }
                    }
                }
                let map: HashMap<String, Option<Key>> = keys.into_iter().collect();
                let r = self.db.store.import_json(&text, &mut |p| map.get(p).cloned().flatten());
                match r {
                    Ok((made, done, skipped)) => self.send(Resp::Message(format!("Imported {done} items, {made} new tags{}", if skipped > 0 { format!(", {skipped} not found") } else { String::new() }))),
                    Err(e) => self.send(Resp::Message(format!("Import failed: {e}"))),
                }
                self.changed();
            }
            Req::Retry => {
                if self.db.retry() {
                    self.send(Resp::Message("Tags are writable again".into()));
                }
                self.changed();
            }
        }
    }

    /// Read-only: tell the UI, change nothing.
    fn refuse(&self) -> bool {
        if self.db.writable() {
            return false;
        }
        self.send(Resp::Message("Tags are read-only: changes are not saved".into()));
        true
    }

    fn apply(&mut self, files: &[(String, u128)], edit: Edit) -> (usize, usize) {
        let (mut n, mut full) = (0, 0);
        for (path, fid) in files {
            let Some((_, key)) = self.key_of(path, *fid) else { continue };
            let have = self.db.store.lookup(&key, path);
            let want = edit.apply(have);
            if want == have {
                continue;
            }
            match self.db.store.set(&key, path, want) {
                Ok(()) => n += 1,
                Err(StoreError::Full) => full += 1,
                Err(_) => {}
            }
        }
        (n, full)
    }

    fn changed(&self) {
        self.send(Resp::Changed);
        self.state();
    }

    fn reconcile_slice(&mut self) {
        // pauses while the UI scrolls (one wait, then the queue is read again)
        let last = self.scroll.load(Ordering::Relaxed);
        let now = self.t0.elapsed().as_millis() as u64;
        if last != 0 && now < last + SCROLL_QUIET_MS {
            std::thread::sleep(Duration::from_millis((last + SCROLL_QUIET_MS - now).min(SCROLL_QUIET_MS)));
            return;
        }
        if self.recon.is_none() {
            let Some(vol) = self.queue.pop_front() else { return };
            if !self.db.writable() {
                return;
            }
            let keys = self.db.store.reconcile_keys(&vol);
            self.recon = Some(Recon_ { vol, keys, at: 0, r: Recon::default() });
        }
        let Some(mut rc) = self.recon.take() else { return };
        let end = (rc.at + SLICE).min(rc.keys.len());
        for k in &rc.keys[rc.at..end] {
            self.db.store.reconcile_one(&*self.fs, &rc.vol, k, &mut rc.r);
        }
        rc.at = end;
        let total = rc.keys.len();
        if end >= total {
            self.db.store.reconcile_done(&rc.vol);
            self.send(Resp::Progress(None));
            if rc.r.moved + rc.r.rekeyed + rc.r.missing > 0 {
                self.changed();
            }
        } else {
            // the bar shows only for passes long enough to notice
            if total > SLICE * 4 {
                self.send(Resp::Progress(Some((end, total))));
            }
            self.recon = Some(rc);
        }
    }
}

/// The folder part of `path` (`C:\a\b.txt` -> `C:\a`, `C:\a` -> `C:\`).
pub fn parent(path: &str) -> &str {
    match path.trim_end_matches('\\').rfind('\\') {
        Some(i) if i <= 2 && path.as_bytes().get(1) == Some(&b':') => &path[..=i],
        Some(i) => &path[..i],
        None => path,
    }
}

fn err_text(e: StoreError, name: &str) -> String {
    match e {
        StoreError::DuplicateName => format!("A tag named \"{name}\" already exists"),
        StoreError::BadName => "A tag needs a name".into(),
        StoreError::TooManyTags => format!("At most {MAX_TAGS} tags"),
        StoreError::Full => format!("Tag store full ({} items)", store::MAX_ITEMS),
        StoreError::NoSuchTag => "That tag no longer exists".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags::store::tests::FakeFs;

    struct Shared(std::sync::Arc<FakeFs>);
    unsafe impl Send for Shared {}
    impl Fs for Shared {
        fn volume(&self, p: &str) -> Option<Vol> {
            self.0.volume(p)
        }
        fn file_id(&self, p: &str) -> Option<u128> {
            self.0.file_id(p)
        }
        fn path_by_id(&self, v: &Vol, f: u128) -> Option<String> {
            self.0.path_by_id(v, f)
        }
        fn exists(&self, p: &str) -> bool {
            self.0.exists(p)
        }
    }

    fn run(dir: &std::path::Path, fs_: std::sync::Arc<FakeFs>) -> (Run, Receiver<Resp>) {
        let (tx, rx) = channel();
        let r = Run {
            db: Db::open(&dir.join("tags.db")),
            fs: Box::new(Shared(fs_)),
            tx,
            wake: None,
            vols: HashMap::new(),
            seen: HashSet::new(),
            queue: VecDeque::new(),
            recon: None,
            scroll: Arc::new(AtomicU64::new(0)),
            t0: Instant::now(),
            dirty_at: None,
        };
        (r, rx)
    }

    #[test]
    #[allow(clippy::arc_with_non_send_sync)]
    fn lookup_apply_and_reconcile_through_the_worker() {
        let d = std::env::temp_dir().join(format!("tsubame-tagsw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let fs_ = std::sync::Arc::new(FakeFs::with(1, true));
        fs_.add("C:\\w\\a.txt", 5);
        fs_.add("C:\\w\\b.txt", 6);
        let (mut w, rx) = run(&d, fs_.clone());
        // empty store: lookups are answered without a volume query
        w.handle(Req::Lookup { dir: "C:\\w".into(), rows: vec![Row { ck: 1, path: "C:\\w\\a.txt".into(), fid: 5 }] });
        assert!(w.vols.is_empty());
        w.handle(Req::Apply { files: vec![("C:\\w\\a.txt".into(), 5), ("C:\\w\\b.txt".into(), 0)], edit: Edit { on: 1 | 16, off: 0 } });
        assert_eq!(w.db.store.len(), 2);
        // b had no id from the listing: resolved by path
        assert!(w.db.store.get(&Key::Id { vol: 1, fid: 6 }).is_some());
        w.handle(Req::Apply { files: vec![("C:\\w\\b.txt".into(), 6)], edit: Edit { on: 0, off: 16 } });
        assert_eq!(w.db.store.get(&Key::Id { vol: 1, fid: 6 }).unwrap().mask, 1);
        // renamed outside: the next listing sees the id under the new name
        fs_.mv("C:\\w\\a.txt", "C:\\w\\c.txt");
        w.vols.clear();
        w.seen.clear();
        w.handle(Req::Lookup { dir: "C:\\w".into(), rows: vec![Row { ck: 9, path: "C:\\w\\c.txt".into(), fid: 5 }] });
        let masks: Vec<Resp> = rx.try_iter().collect();
        assert!(masks.iter().any(|r| matches!(r, Resp::Masks(m) if m == &vec![(9, 17)])));
        // the lookup queued the one-time reconcile of the volume
        assert_eq!(w.queue.len(), 1);
        fs_.rm("C:\\w\\b.txt");
        while w.recon.is_some() || !w.queue.is_empty() {
            w.reconcile_slice();
        }
        assert!(w.db.store.query(1).iter().all(|(p, _)| p != "C:\\w\\b.txt"));
        // create-and-apply, then a query lists it
        w.handle(Req::Create { name: "Taxes".into(), colour: 4, shape: Shape::Hex, files: vec![("C:\\w\\c.txt".into(), 5)] });
        let slot = w.db.store.find_tag("Taxes").unwrap().slot;
        w.handle(Req::Query { seq: 3, slot });
        let res: Vec<Resp> = rx.try_iter().collect();
        assert!(res.iter().any(|r| matches!(r, Resp::Results { seq: 3, hits, .. } if hits.len() == 1 && hits[0].path == "C:\\w\\c.txt")));
        w.save();
        assert!(d.join("tags.db").exists());
        drop(w);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn parent_of_paths() {
        assert_eq!(parent("C:\\a\\b.txt"), "C:\\a");
        assert_eq!(parent("C:\\a"), "C:\\");
        assert_eq!(parent("\\\\s\\sh\\x"), "\\\\s\\sh");
    }
}
