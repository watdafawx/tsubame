//! UI-thread side of the shell pool: icon and thumbnail caches, the request
//! scheduler, and stats. Never calls COM or touches the disk; it only submits
//! to and drains from `tsubame_win::shell::Pool`. See docs/icons-thumbnails.md.
//!
//! Per frame: `sync_generation`, `begin_frame`, then every visible cell or
//! row asks for its picture (`row_icon`, `paint_pic`) and the prefetch margin
//! registers what it will need (`want_row`, `want_cell`). A miss becomes a
//! want with a priority (0 = on screen, then rows away from the viewport).
//! `end_frame` cancels every pending request nothing asked for this frame,
//! sorts the wants nearest first and `refill` submits as many as the in-flight
//! limits allow; it runs again after each reply, so the queue drains without
//! another paint. A reply is applied only if its id is still pending; anything
//! else is stale and dropped. A navigation (`sync_generation` with a new
//! generation) cancels everything and empties the thumbnail cache.

use crate::data::RowBuf;
use crate::lru::Lru;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tsubame_ui::{Canvas, Rect, Theme};
use tsubame_win::bitmap::Bitmap;
use tsubame_win::shell::{IconSource, MenuResult, Payload, Pool, Reply, Request, Ticket};

// ---- tuning (the one place; docs/icons-thumbnails.md explains each) --------

/// Icon cache caps. 32 px icons are 4 KB each, the grid's 96 to 192 px ones
/// 36 to 144 KB: the byte cap is what binds.
pub const ICON_CAP_ITEMS: usize = 2048;
pub const ICON_CAP_BYTES: usize = 6 << 20;
/// Thumbnail cache caps. An entry is charged w*h*4: the CPU pixels until the
/// first draw uploads them, then the device copy (the CPU pixels are dropped).
/// `tsubame_d2d::BITMAP_CAP_BYTES` must stay above this plus the icons.
pub const THUMB_CAP_ITEMS: usize = 4096;
pub const THUMB_CAP_BYTES: usize = 12 << 20;
/// Shell STA workers: icons and menus, and the thumbnail lane.
pub const ICON_WORKERS: usize = 2;
pub const THUMB_WORKERS: usize = 2;
/// Requests in flight per lane: a few more than the workers, so a worker
/// never waits for the next request, and cancelling stays cheap.
pub const ICON_INFLIGHT: usize = 16;
pub const THUMB_INFLIGHT: usize = 8;
/// Thumbnail fade-in.
pub const FADE_MS: u128 = 100;
/// New device bitmaps (thumbnail uploads) one frame may create.
pub const UPLOADS_PER_FRAME: u32 = 6;
/// Visible plus prefetched thumbnails use at most this share of the cap.
pub const PREFETCH_FILL: f32 = 0.85;
/// Samples kept per stats series (a long run must not grow them).
const STAT_SAMPLES: usize = 20_000;

pub struct Image {
    /// Gfx bitmap cache key
    pub key: Arc<str>,
    pub w: u32,
    pub h: u32,
    /// empty = the shell had no image (drawn as the placeholder, not retried)
    pub px: Vec<u32>,
}

impl Image {
    fn from(key: Arc<str>, b: Option<Bitmap>) -> Image {
        match b {
            Some(b) => Image { key, w: b.w, h: b.h, px: b.px },
            None => Image { key, w: 0, h: 0, px: Vec::new() },
        }
    }
    fn bytes(&self) -> usize {
        self.px.len() * 4 + self.key.len() + 48
    }
}

pub struct Thumb {
    /// `img.px` is dropped once the device bitmap exists (the device cache is
    /// then the only copy); `img.w > 0` tells a real thumbnail from a failure
    img: Image,
    px: RefCell<Vec<u32>>,
    /// when it was first drawn (starts the fade)
    born: Cell<Option<Instant>>,
}

impl Thumb {
    fn new(img: Image) -> Thumb {
        let mut img = img;
        let px = RefCell::new(std::mem::take(&mut img.px));
        Thumb { img, px, born: Cell::new(None) }
    }
}

struct Pending {
    ticket: Ticket,
    frame: u64,
    at: Instant,
    /// stats: which kind of request
    class: u8,
}

const C_EXT: u8 = 0;
const C_FOLDER: u8 = 1;
const C_PATH: u8 = 2;
const C_THUMB: u8 = 3;
const CLASSES: usize = 4;

/// A request that did not fit the in-flight limits yet.
struct Want {
    key: Arc<str>,
    req: Request,
    class: u8,
    /// 0 = on screen; more = further from the viewport
    prio: f32,
    seq: u32,
}

/// Extensions whose icon lives in the file itself: per-path cache key.
const PER_PATH_EXT: [&str; 10] = [".exe", ".lnk", ".ico", ".cur", ".ani", ".url", ".scr", ".cpl", ".msc", ".appref-ms"];
/// Files the shell can make a content thumbnail for. Everything else shows
/// its (shared, by-extension) type icon, large.
const THUMB_EXT: [&str; 36] = [
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "webp", "tif", "tiff", "heic", "heif", "avif", "svg", "jxl", "psd", "cr2", "nef", "arw", "dng", "raf", "orf", "rw2", "mp4", "m4v", "mkv", "avi", "mov", "wmv", "webm", "mpg", "mpeg", "pdf", "xps", "epub",
];
const ATTR_READONLY: u32 = 0x1;
const ATTR_SYSTEM: u32 = 0x4;

pub fn is_thumb_ext(ext: &str) -> bool {
    let e = ext.strip_prefix('.').unwrap_or(ext);
    !e.is_empty() && THUMB_EXT.iter().any(|t| t.eq_ignore_ascii_case(e))
}

#[derive(Default)]
pub struct Stats {
    pub requested: u64,
    pub received: u64,
    pub cancelled: u64,
    pub stale: u64,
    pub failed: u64,
    /// submit to applied on the UI thread, by class (ext, folder, path, thumbnail), us
    pub latency_us: [Vec<u64>; CLASSES],
    /// worker time in the shell call, by class, us
    pub work_us: [Vec<u64>; CLASSES],
    /// the first icon reply in the process (cold shell): (class, work us)
    pub first: Option<(u8, u64)>,
    pub max_pending: usize,
    /// thumbnail counters (also in the totals above)
    pub thumb_requested: u64,
    pub thumb_received: u64,
    pub thumb_cancelled: u64,
    /// new device bitmaps a frame postponed (`UPLOADS_PER_FRAME`)
    pub uploads_deferred: u64,
}

/// What the grid draws with: sizes in device pixels and DIPs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridPic {
    /// side of the picture box (DIPs)
    pub box_dip: f32,
    /// the type icon shown in it (DIPs)
    pub icon_dip: f32,
    pub dpi: f32,
}

pub struct ShellUi {
    pub icons: Lru<Arc<str>, Image>,
    pub thumbs: Lru<Arc<str>, Thumb>,
    pending: HashMap<Arc<str>, Pending>,
    by_id: HashMap<u64, Arc<str>>,
    /// this frame's misses, then (after `end_frame`) the ones not submitted yet
    wants: Vec<Want>,
    queue: VecDeque<Want>,
    want_keys: HashSet<Arc<str>>,
    seq: u32,
    frame: u64,
    generation: u64,
    pub icon_px: u32,
    pic: GridPic,
    /// device px of grid type icons / of thumbnail requests this frame
    grid_icon_px: u32,
    thumb_px: u32,
    uploads_left: u32,
    /// a frame drew a fading thumbnail or postponed an upload: it needs another
    animating: bool,
    pub stats: Stats,
    pub per_path_all: bool,
    pub use_factory: bool,
    /// last context menu outcome, for the status bar
    pub menu_status: String,
    pub menu_pending: Option<u64>,
    menu_asked: Option<Instant>,
    /// set when a menu reply arrives (stress drivers read it)
    pub last_menu: Option<(MenuResult, u64)>,
    /// thumbnail replies not for the grid, kept only while `collect_other`
    /// (the thumbnail stress run reads them); otherwise stale and dropped
    pub other_replies: Vec<Reply>,
    pub collect_other: bool,
    key: String,
}

fn env_on(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v != "0")
}

fn push_sample(v: &mut Vec<u64>, x: u64) {
    if v.len() < STAT_SAMPLES {
        v.push(x);
    }
}

impl ShellUi {
    pub fn new(icon_px: u32) -> ShellUi {
        ShellUi {
            icons: Lru::new(ICON_CAP_ITEMS, ICON_CAP_BYTES),
            thumbs: Lru::new(THUMB_CAP_ITEMS, THUMB_CAP_BYTES),
            pending: HashMap::new(),
            by_id: HashMap::new(),
            wants: Vec::new(),
            queue: VecDeque::new(),
            want_keys: HashSet::new(),
            seq: 0,
            frame: 0,
            generation: 0,
            icon_px,
            pic: GridPic { box_dip: 64.0, icon_dip: 48.0, dpi: 96.0 },
            grid_icon_px: 48,
            thumb_px: 64,
            uploads_left: UPLOADS_PER_FRAME,
            animating: false,
            stats: Stats::default(),
            per_path_all: env_on("TSUBAME_ICON_PER_PATH"),
            use_factory: env_on("TSUBAME_ICON_FACTORY"),
            menu_status: String::new(),
            menu_pending: None,
            menu_asked: None,
            last_menu: None,
            other_replies: Vec::new(),
            collect_other: false,
            key: String::with_capacity(300),
        }
    }

    pub fn set_icon_px(&mut self, px: u32) {
        self.icon_px = px.max(16);
    }

    /// The grid's picture sizes. `visible` = most cells one screen shows: the
    /// thumbnail size shrinks (in steps of 16 px) until they all fit the
    /// cache, so the screen never evicts itself.
    pub fn set_grid(&mut self, pic: GridPic, visible: usize) {
        self.pic = pic;
        let s = pic.dpi / 96.0;
        self.grid_icon_px = (pic.icon_dip * s).round().max(16.0) as u32;
        let want = ((pic.box_dip * s).round().max(32.0) as u32).div_ceil(16) * 16;
        let fit = ((THUMB_CAP_BYTES as f32 * PREFETCH_FILL / (visible.max(1) as f32 * 4.0)).sqrt() as u32 / 16 * 16).max(32);
        self.thumb_px = want.min(fit);
    }
    pub fn thumb_px(&self) -> u32 {
        self.thumb_px
    }
    /// Cells (visible + prefetch) whose thumbnails fit the cache.
    pub fn thumb_budget_cells(&self) -> usize {
        (THUMB_CAP_BYTES as f32 * PREFETCH_FILL / (self.thumb_px as f32 * self.thumb_px as f32 * 4.0)) as usize
    }

    /// Cancels everything and empties the thumbnail cache when the shown
    /// folder changed (`generation` counts navigations, tab switches and
    /// closes). Pending replies of the old generation come back stale.
    pub fn sync_generation(&mut self, generation: u64) {
        if generation == self.generation {
            return;
        }
        self.generation = generation;
        self.cancel_all();
        self.thumbs = Lru::new(THUMB_CAP_ITEMS, THUMB_CAP_BYTES);
    }

    fn cancel_all(&mut self) {
        for (_, p) in self.pending.drain() {
            p.ticket.cancel();
            self.stats.cancelled += 1;
            if p.class == C_THUMB {
                self.stats.thumb_cancelled += 1;
            }
        }
        self.by_id.clear();
        self.wants.clear();
        self.queue.clear();
        self.want_keys.clear();
    }

    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.wants.clear();
        self.want_keys.clear();
        self.seq = 0;
        self.uploads_left = UPLOADS_PER_FRAME;
        self.animating = false;
    }

    /// A frame drew a thumbnail that is still fading in, or postponed an
    /// upload: paint again. Both end by themselves (100 ms / the queue empties).
    pub fn needs_frame(&self) -> bool {
        self.animating
    }

    /// Looks `key` up in the icon cache; a miss becomes a want (unless one
    /// exists). `peek`: only register the want, do not touch the LRU.
    fn icon_lookup(&mut self, row: &RowBuf, root: Option<&str>, px: u32, prio: f32, peek: bool) -> Option<&Image> {
        use std::fmt::Write;
        // cache key: by extension for plain files, per path only where the
        // icon lives in the file (exe, lnk, ico...) or a folder that may have
        // a custom icon (desktop.ini sets READONLY or SYSTEM on its folder)
        let ext = row.ext();
        let per_path = root.is_some()
            && if row.is_dir() { row.attrs & (ATTR_READONLY | ATTR_SYSTEM) != 0 } else { self.per_path_all || PER_PATH_EXT.iter().any(|e| e.eq_ignore_ascii_case(ext)) };
        let class = if per_path { C_PATH } else if row.is_dir() { C_FOLDER } else { C_EXT };
        self.key.clear();
        let _ = match class {
            C_PATH => {
                let root = root.unwrap();
                let sep = if root.ends_with('\\') { "" } else { "\\" };
                write!(self.key, "p:{root}{sep}{}@{px}", row.name_str())
            }
            C_FOLDER => write!(self.key, "folder@{px}"),
            _ => {
                self.key.push_str("e:");
                self.key.extend(ext.chars().map(|c| c.to_ascii_lowercase()));
                write!(self.key, "@{px}")
            }
        };
        if self.icons.contains(self.key.as_str()) {
            return if peek { None } else { self.icons.get(self.key.as_str()).filter(|i| !i.px.is_empty()) };
        }
        if let Some(p) = self.pending.get_mut(self.key.as_str()) {
            p.frame = self.frame;
            return None;
        }
        if self.want_keys.contains(self.key.as_str()) {
            return None;
        }
        let src = match class {
            C_PATH => {
                let p = self.key[2..self.key.rfind('@').unwrap()].to_string();
                if self.use_factory { IconSource::PathFactory(p) } else { IconSource::Path(p) }
            }
            C_FOLDER => IconSource::Folder,
            _ => IconSource::Ext(ext.to_ascii_lowercase()),
        };
        let key: Arc<str> = Arc::from(self.key.as_str());
        self.want_keys.insert(key.clone());
        self.seq += 1;
        self.wants.push(Want { key, req: Request::Icon { src, size: px }, class, prio, seq: self.seq });
        None
    }

    /// The details icon for a visible row, or None (placeholder) while it
    /// loads. `root` is the folder of a real entry, None for fake rows.
    pub fn row_icon(&mut self, row: &RowBuf, root: Option<&str>) -> Option<&Image> {
        let px = self.icon_px;
        self.icon_lookup(row, root, px, 0.0, false)
    }

    /// Prefetch: asks for a details icon `prio` rows away from the viewport.
    pub fn want_row(&mut self, row: &RowBuf, root: Option<&str>, prio: f32) {
        let px = self.icon_px;
        let _ = self.icon_lookup(row, root, px, prio, true);
    }

    fn thumb_key(&mut self, root: &str, name: &str) -> Arc<str> {
        use std::fmt::Write;
        self.key.clear();
        let sep = if root.ends_with('\\') { "" } else { "\\" };
        let _ = write!(self.key, "t:{root}{sep}{name}@{}", self.thumb_px);
        Arc::from(self.key.as_str())
    }

    /// Registers the thumbnail request of a file if it is not cached or on
    /// its way. Returns the key.
    fn thumb_want(&mut self, row: &RowBuf, root: &str, prio: f32) -> Arc<str> {
        let key = self.thumb_key(root, row.name_str());
        if self.thumbs.contains(&*key) {
            return key;
        }
        if let Some(p) = self.pending.get_mut(&*key) {
            p.frame = self.frame;
            return key;
        }
        if self.want_keys.insert(key.clone()) {
            let sep = if root.ends_with('\\') { "" } else { "\\" };
            let path = format!("{root}{sep}{}", row.name_str());
            self.seq += 1;
            self.wants.push(Want { key: key.clone(), req: Request::Thumb { path, size: self.thumb_px, cache_only: false }, class: C_THUMB, prio, seq: self.seq });
        }
        key
    }

    /// Prefetch for a grid cell `prio` rows away from the viewport.
    pub fn want_cell(&mut self, row: &RowBuf, root: Option<&str>, prio: f32) {
        match root {
            Some(r) if !row.is_dir() && is_thumb_ext(row.ext()) => {
                self.thumb_want(row, r, prio);
                let px = self.grid_icon_px;
                let _ = self.icon_lookup(row, root, px, prio, true);
            }
            _ => {
                let px = self.grid_icon_px;
                let _ = self.icon_lookup(row, root, px, prio, true);
            }
        }
    }

    /// Draws the picture of a visible grid cell in `boxr` (the thumbnail box):
    /// the thumbnail once it is here (fading in over the type icon), else the
    /// type icon, else a placeholder shape. Registers what is missing.
    pub fn paint_pic(&mut self, g: &mut dyn Canvas, t: &Theme, row: &RowBuf, root: Option<&str>, boxr: Rect) {
        let mut alpha = 0.0f32;
        let mut thumb_key: Option<Arc<str>> = None;
        let mut lost: Option<Arc<str>> = None;
        if let Some(r) = root
            && !row.is_dir()
            && is_thumb_ext(row.ext())
        {
            let key = self.thumb_want(row, r, 0.0);
            if let Some(th) = self.thumbs.get(&*key)
                && th.img.w > 0
            {
                // a new device bitmap costs an upload: spread them over frames
                let have = g.has_image(&key);
                if !have && th.px.borrow().is_empty() {
                    // the device cache dropped it and the CPU copy is gone:
                    // forget the entry so the next frame asks again
                    lost = Some(key);
                } else if have || self.uploads_left > 0 {
                    if !have {
                        self.uploads_left -= 1;
                    }
                    let born = th.born.get().unwrap_or_else(|| {
                        let now = Instant::now();
                        th.born.set(Some(now));
                        now
                    });
                    alpha = (born.elapsed().as_millis() as f32 / FADE_MS as f32).min(1.0);
                    thumb_key = Some(key);
                } else {
                    self.stats.uploads_deferred += 1;
                    self.animating = true;
                }
            }
        }
        if alpha < 1.0 {
            self.paint_icon(g, t, row, root, boxr);
        }
        if let Some(key) = thumb_key
            && let Some(th) = self.thumbs.get(&*key)
        {
            let (w, h) = (th.img.w as f32, th.img.h as f32);
            let s = (boxr.w / w).min(boxr.h / h);
            let (dw, dh) = (w * s, h * s);
            let r = Rect::new(boxr.x + (boxr.w - dw) / 2.0, boxr.y + (boxr.h - dh) / 2.0, dw, dh);
            g.image_a(&th.img.key, th.img.w, th.img.h, &th.px.borrow(), r, alpha);
            if g.has_image(&th.img.key) {
                // uploaded: the device cache is the only copy from now on
                *th.px.borrow_mut() = Vec::new();
            }
            if alpha < 1.0 {
                self.animating = true;
            }
        }
        if let Some(key) = lost {
            self.thumbs.remove(&*key);
            self.animating = true;
        }
    }

    /// The grid's type icon centred in `boxr`, or the placeholder shape.
    fn paint_icon(&mut self, g: &mut dyn Canvas, t: &Theme, row: &RowBuf, root: Option<&str>, boxr: Rect) {
        let s = self.pic.icon_dip;
        let ir = Rect::new(boxr.x + (boxr.w - s) / 2.0, boxr.y + (boxr.h - s) / 2.0, s, s);
        let px = self.grid_icon_px;
        match self.icon_lookup(row, root, px, 0.0, false) {
            Some(img) => g.image(&img.key, img.w, img.h, &img.px, ir),
            None => {
                let (ic, col) = if row.is_dir() { (Rect::new(ir.x + s * 0.08, ir.y + s * 0.2, s * 0.84, s * 0.62), t.folder) } else { (Rect::new(ir.x + s * 0.16, ir.y + s * 0.06, s * 0.68, s * 0.88), t.text_dim) };
                g.round_a(ic, 3.0, col, 0.35);
            }
        }
    }

    /// After a paint: cancels requests no cell asked for this frame, orders
    /// this frame's misses nearest first and submits what the limits allow.
    pub fn end_frame(&mut self, pool: &mut Pool) {
        let frame = self.frame;
        let mut cancelled = 0;
        let mut thumbs = 0;
        let by_id = &mut self.by_id;
        self.pending.retain(|_, p| {
            if p.frame == frame {
                return true;
            }
            p.ticket.cancel();
            by_id.remove(&p.ticket.id);
            cancelled += 1;
            thumbs += (p.class == C_THUMB) as u64;
            false
        });
        self.stats.cancelled += cancelled;
        self.stats.thumb_cancelled += thumbs;
        // this frame's wants replace the leftovers of the last one
        self.wants.sort_by(|a, b| a.prio.total_cmp(&b.prio).then(a.seq.cmp(&b.seq)));
        self.queue.clear();
        self.queue.extend(self.wants.drain(..));
        // what does not fit the lanes waits in the queue; replies refill it
        // (`refill` after each reply), so no repaint loop is needed for it
        self.refill(pool);
    }

    /// Submits queued wants while the lanes have room. Also called after
    /// replies, so the queue drains without another paint.
    pub fn refill(&mut self, pool: &mut Pool) {
        if self.queue.is_empty() {
            return;
        }
        let frame = self.frame;
        let mut n_thumb = self.pending.values().filter(|p| p.class == C_THUMB).count();
        let mut n_icon = self.pending.len() - n_thumb;
        let mut i = 0;
        while i < self.queue.len() {
            let thumb = self.queue[i].class == C_THUMB;
            let room = if thumb { n_thumb < THUMB_INFLIGHT } else { n_icon < ICON_INFLIGHT };
            if !room {
                i += 1; // the other lane may still have room
                continue;
            }
            let w = self.queue.remove(i).unwrap();
            let ticket = pool.submit(w.req);
            self.by_id.insert(ticket.id, w.key.clone());
            self.pending.insert(w.key, Pending { ticket, frame, at: Instant::now(), class: w.class });
            self.stats.requested += 1;
            if thumb {
                n_thumb += 1;
                self.stats.thumb_requested += 1;
            } else {
                n_icon += 1;
            }
            if n_thumb >= THUMB_INFLIGHT && n_icon >= ICON_INFLIGHT {
                break;
            }
        }
        self.stats.max_pending = self.stats.max_pending.max(self.pending.len());
    }

    /// Jobs a hung worker took with it: forget them so the rows ask again.
    pub fn forget(&mut self, ids: &[u64]) {
        for id in ids {
            if let Some(k) = self.by_id.remove(id) {
                self.pending.remove(&k);
            }
            if self.menu_pending == Some(*id) {
                self.menu_pending = None;
                self.menu_status = "menu worker was replaced".into();
            }
        }
    }

    /// Applies one reply. Returns true when the screen should repaint.
    pub fn apply(&mut self, r: Reply) -> bool {
        if matches!(r.payload, Payload::Thumb { .. }) && !self.by_id.contains_key(&r.id) {
            if self.collect_other {
                self.other_replies.push(r);
            } else {
                self.stats.stale += 1;
            }
            return false;
        }
        match r.payload {
            Payload::Icon(bmp) => {
                let Some(key) = self.by_id.remove(&r.id) else {
                    self.stats.stale += 1;
                    return false;
                };
                self.note(&key, r.work_us);
                if bmp.is_none() {
                    self.stats.failed += 1;
                }
                let img = Image::from(key.clone(), bmp);
                let b = img.bytes();
                self.icons.insert(key, img, b);
                true
            }
            Payload::Thumb { bmp, .. } => {
                let Some(key) = self.by_id.remove(&r.id) else {
                    return false;
                };
                self.note(&key, r.work_us);
                self.stats.thumb_received += 1;
                if bmp.is_none() {
                    self.stats.failed += 1;
                }
                let img = Image::from(key.clone(), bmp);
                let b = img.bytes();
                self.thumbs.insert(key, Thumb::new(img), b);
                true
            }
            Payload::Menu(m) => {
                if self.menu_pending == Some(r.id) {
                    self.menu_pending = None;
                }
                let total = self.menu_asked.take().map(|t| t.elapsed().as_micros() as u64).unwrap_or(0);
                self.menu_status = menu_line(&m, r.queued_us);
                self.last_menu = Some((m, total));
                true
            }
            Payload::Hang => {
                self.menu_status = "hung job finished".into();
                true
            }
            // shellops takes its own clipboard replies before this; the rest
            // belong to a stress run
            Payload::Clip(_) => {
                if self.collect_other {
                    self.other_replies.push(r);
                }
                false
            }
            // own-drawn context menu (CM1): nothing submits it yet
            Payload::Ctx(_) => false,
        }
    }

    /// Stats of a finished request.
    fn note(&mut self, key: &Arc<str>, work_us: u64) {
        let p = self.pending.remove(key);
        self.stats.received += 1;
        if let Some(p) = p {
            let c = p.class as usize;
            push_sample(&mut self.stats.latency_us[c], p.at.elapsed().as_micros() as u64);
            push_sample(&mut self.stats.work_us[c], work_us);
            if self.stats.first.is_none() && p.class != C_THUMB {
                self.stats.first = Some((p.class, work_us));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_menu(&mut self, pool: &mut Pool, paths: Vec<String>, x: i32, y: i32, extended: bool, owner: Option<isize>, opts: tsubame_win::shell::MenuOpts) {
        let n = paths.len();
        let t = pool.submit(Request::Menu { paths, x, y, extended, auto_close: None, owner, opts });
        self.menu_pending = Some(t.id);
        self.menu_asked = Some(Instant::now());
        self.menu_status = format!("menu for {n} item(s) requested{}", if owner.is_some() { " (owner = UI window, wrong thread)" } else { "" });
    }

    /// Requests in flight (icons, thumbnails) and waiting for a slot.
    pub fn load(&self) -> (usize, usize) {
        (self.pending.len(), self.queue.len())
    }
}

pub fn menu_line(m: &MenuResult, queued_us: u64) -> String {
    let ms = |us: u64| us as f64 / 1000.0;
    let mut s = format!(
        "menu: {} items, queue {:.1} ms, bind {:.1} ms, QueryContextMenu {:.1} ms, on screen at {}",
        m.items,
        ms(queued_us),
        ms(m.bind_us),
        ms(m.query_us),
        m.popup_us.map(|u| format!("{:.1} ms", ms(u))).unwrap_or_else(|| "never".into())
    );
    match (&m.chosen, &m.invoked) {
        (Some(c), Some(i)) => s.push_str(&format!(", chose '{c}', InvokeCommand {i}")),
        _ => s.push_str(", dismissed"),
    }
    if m.foreign_owner {
        s.push_str(", owner on another thread");
    }
    if m.items > 0 && !m.foreground {
        s.push_str(", SetForegroundWindow failed");
    }
    if let Some(e) = &m.error {
        s.push_str(&format!(", error: {e}"));
    }
    s
}

/// p50/p95/max of microsecond samples, as ms text.
pub fn pcts(v: &[u64]) -> String {
    if v.is_empty() {
        return "n=0".into();
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    let p = |q: f64| s[((q * s.len() as f64).ceil() as usize).clamp(1, s.len()) - 1] as f64 / 1000.0;
    format!("n={} p50={:.2} p95={:.2} max={:.2} ms", s.len(), p(0.5), p(0.95), *s.last().unwrap() as f64 / 1000.0)
}

impl Stats {
    pub fn report(&self) -> String {
        let names = ["by extension", "folder", "per path", "thumbnail"];
        let mut out = String::new();
        out.push_str(&format!(
            "icons: requested {} received {} cancelled(UI) {} stale-dropped {} failed {} max_pending {}\n",
            self.requested, self.received, self.cancelled, self.stale, self.failed, self.max_pending
        ));
        out.push_str(&format!(
            "thumbnails: requested {} received {} cancelled(UI) {} uploads_deferred {}\n",
            self.thumb_requested, self.thumb_received, self.thumb_cancelled, self.uploads_deferred
        ));
        if let Some((c, us)) = self.first {
            out.push_str(&format!("icon first reply (cold shell): {} {:.2} ms worker time\n", names[c as usize], us as f64 / 1000.0));
        }
        for (c, name) in names.iter().enumerate() {
            if self.work_us[c].is_empty() && c == C_THUMB as usize {
                continue;
            }
            out.push_str(&format!("icon worker time {name}: {}\n", pcts(&self.work_us[c])));
            out.push_str(&format!("icon submit-to-applied {name}: {}\n", pcts(&self.latency_us[c])));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, dir: bool) -> RowBuf {
        let mut r = RowBuf::new();
        r.name[..name.len()].copy_from_slice(name.as_bytes());
        r.name_len = name.len();
        r.kind = if dir { 0 } else { crate::data::kind_of(name, false) };
        r
    }

    fn pool() -> Pool {
        Pool::with_lanes(1, 1, 0, 0x8001)
    }

    #[test]
    fn thumbnail_capable_types() {
        for e in [".jpg", ".PNG", "webp", ".mp4", ".pdf"] {
            assert!(is_thumb_ext(e), "{e}");
        }
        for e in ["", ".txt", ".exe", ".ico", ".dll", "."] {
            assert!(!is_thumb_ext(e), "{e}");
        }
    }

    #[test]
    fn thumbnail_size_fits_the_cache_for_a_full_screen() {
        let mut s = ShellUi::new(16);
        // 4K, small cells: 150 cells of 128 px fit 12 MB x 0.85, so the box size stands
        s.set_grid(GridPic { box_dip: 64.0, icon_dip: 48.0, dpi: 192.0 }, 150);
        assert_eq!(s.thumb_px(), 128);
        assert!(s.thumb_budget_cells() >= 150, "{}", s.thumb_budget_cells());
        // 4K, large cells: 256 px would be 256 KB each; 120 on screen do not
        // fit, so the request size drops until they do
        s.set_grid(GridPic { box_dip: 128.0, icon_dip: 96.0, dpi: 192.0 }, 120);
        assert!(s.thumb_px() < 256 && s.thumb_px().is_multiple_of(16), "{}", s.thumb_px());
        assert!(120.0 * (s.thumb_px() as f32).powi(2) * 4.0 <= THUMB_CAP_BYTES as f32, "the screen fits the cap");
        assert!(s.thumb_budget_cells() >= 120);
        // a small screen keeps the full size
        s.set_grid(GridPic { box_dip: 128.0, icon_dip: 96.0, dpi: 96.0 }, 20);
        assert_eq!(s.thumb_px(), 128);
    }

    #[test]
    fn misses_become_wants_nearest_first_and_respect_the_limits() {
        let mut s = ShellUi::new(16);
        let mut p = pool();
        s.set_grid(GridPic { box_dip: 64.0, icon_dip: 48.0, dpi: 96.0 }, 100);
        s.begin_frame();
        // 30 distinct files: far ones first in registration order, near ones last
        for i in 0..30 {
            let r = row(&format!("f{i}.jpg"), false);
            s.want_cell(&r, Some("C:\\x"), (30 - i) as f32);
        }
        s.end_frame(&mut p);
        let (inflight, waiting) = s.load();
        assert_eq!(inflight, THUMB_INFLIGHT + 1, "8 thumbnails and the one shared .jpg icon");
        assert_eq!(waiting, 30 - THUMB_INFLIGHT, "the rest wait for a slot");
        // the nearest (prio 1 = f29) went first
        assert!(s.pending.keys().any(|k| k.contains("f29.jpg")));
        assert!(s.pending.keys().all(|k| !k.contains("f0.jpg")));
        assert!(!s.needs_frame(), "waiting wants need no repaint loop: replies refill the lanes");
    }

    #[test]
    fn requests_nobody_asked_for_this_frame_are_cancelled() {
        let mut s = ShellUi::new(16);
        let mut p = pool();
        s.begin_frame();
        for i in 0..4 {
            s.want_cell(&row(&format!("a{i}.png"), false), Some("C:\\x"), 0.0);
        }
        s.end_frame(&mut p);
        assert!(s.stats.thumb_requested == 4);
        // next frame only a0 is on screen
        s.begin_frame();
        s.want_cell(&row("a0.png", false), Some("C:\\x"), 0.0);
        s.end_frame(&mut p);
        assert_eq!(s.stats.thumb_cancelled, 3);
        assert_eq!(s.pending.values().filter(|p| p.class == C_THUMB).count(), 1);
    }

    #[test]
    fn a_navigation_drops_pending_work_and_the_thumbnail_cache() {
        let mut s = ShellUi::new(16);
        let mut p = pool();
        s.begin_frame();
        s.want_cell(&row("a.png", false), Some("C:\\x"), 0.0);
        s.end_frame(&mut p);
        let id = *s.by_id.keys().next().unwrap();
        s.thumbs.insert(Arc::from("t:old@64"), Thumb::new(Image { key: Arc::from("t:old@64"), w: 1, h: 1, px: vec![0] }), 100);
        s.sync_generation(1);
        assert_eq!(s.load(), (0, 0));
        assert_eq!(s.thumbs.len(), 0);
        // the old request's reply is stale now
        let reply = Reply { id, worker: 0, queued_us: 0, work_us: 0, payload: Payload::Thumb { bmp: None, from_cache: false, probe_us: 0, error: None } };
        assert!(!s.apply(reply));
        assert_eq!(s.stats.stale, 1);
        // the same generation again does nothing
        assert_eq!(s.stats.cancelled, 2, "the thumbnail and the shared .png icon");
        s.sync_generation(1);
        assert_eq!(s.stats.cancelled, 2);
    }

    #[test]
    fn caches_are_bounded_in_bytes() {
        let mut s = ShellUi::new(16);
        for i in 0..2000 {
            let k: Arc<str> = Arc::from(format!("t:{i}@64"));
            let img = Image { key: k.clone(), w: 128, h: 128, px: vec![0; 128 * 128] };
            let b = img.bytes();
            s.thumbs.insert(k, Thumb::new(img), b);
        }
        assert!(s.thumbs.bytes() <= THUMB_CAP_BYTES, "{}", s.thumbs.bytes());
        assert!(s.thumbs.evictions > 0);
        for i in 0..2000 {
            let k: Arc<str> = Arc::from(format!("e:.x{i}@192"));
            let img = Image { key: k.clone(), w: 192, h: 192, px: vec![0; 192 * 192] };
            let b = img.bytes();
            s.icons.insert(k, img, b);
        }
        assert!(s.icons.bytes() <= ICON_CAP_BYTES, "{}", s.icons.bytes());
    }
}
