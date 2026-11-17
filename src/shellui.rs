//! UI-thread side of the shell pool: icon cache, pending requests, the
//! thumbnail panel, and stats. Never calls COM or touches the disk; it only
//! submits to and drains from `tsubame_win::shell::Pool`.
//!
//! Per frame: `begin_frame`, then `icon(key)` for every visible row (misses
//! become wants via `want`), then `end_frame(pool)` submits the new wants and
//! cancels every pending request whose row is no longer visible. A reply is
//! applied only if its id is still pending; anything else is stale and
//! dropped.

use crate::data::RowBuf;
use crate::lru::Lru;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tsubame_win::bitmap::Bitmap;
use tsubame_win::shell::{IconSource, MenuResult, Payload, Pool, Reply, Request, Ticket};

/// Icon cache caps. 32 px icons are 4 KB each: 2048 icons = 8 MB worst case.
pub const ICON_CAP_ITEMS: usize = 2048;
pub const ICON_CAP_BYTES: usize = 8 << 20;
/// Thumbnail cache caps (up to ~1 MB each at 512 px).

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

struct Pending {
    ticket: Ticket,
    frame: u64,
    at: Instant,
    /// stats: which icon path
    class: u8,
}

const C_EXT: u8 = 0;
const C_FOLDER: u8 = 1;
const C_PATH: u8 = 2;

/// Extensions whose icon lives in the file itself: per-path cache key.
const PER_PATH_EXT: [&str; 10] = [".exe", ".lnk", ".ico", ".cur", ".ani", ".url", ".scr", ".cpl", ".msc", ".appref-ms"];
const ATTR_READONLY: u32 = 0x1;
const ATTR_SYSTEM: u32 = 0x4;

#[derive(Default)]
pub struct Stats {
    pub requested: u64,
    pub received: u64,
    pub cancelled: u64,
    pub stale: u64,
    pub failed: u64,
    /// submit to applied on the UI thread, by class (ext, folder, path), us
    pub latency_us: [Vec<u64>; 3],
    /// worker time in the shell call, by class, us
    pub work_us: [Vec<u64>; 3],
    /// the first icon reply in the process (cold shell): (class, work us)
    pub first: Option<(u8, u64)>,
    pub max_pending: usize,
}

pub struct ShellUi {
    pub icons: Lru<Arc<str>, Image>,
    pending: HashMap<Arc<str>, Pending>,
    by_id: HashMap<u64, Arc<str>>,
    wants: Vec<(Arc<str>, IconSource, u8)>,
    frame: u64,
    pub icon_px: u32,
    pub stats: Stats,
    pub per_path_all: bool,
    pub use_factory: bool,
    /// last context menu outcome, for the status bar
    pub menu_status: String,
    menu_pending: Option<u64>,
    menu_asked: Option<Instant>,
    /// set when a menu reply arrives (stress drivers read it)
    pub last_menu: Option<(MenuResult, u64)>,
    /// thumbnail replies not for the panel, kept only while `collect_other`
    /// (the thumbnail stress run reads them); otherwise stale and dropped
    pub other_replies: Vec<Reply>,
    pub collect_other: bool,
    key: String,
}

fn env_on(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v != "0")
}

impl ShellUi {
    pub fn new(icon_px: u32) -> ShellUi {
        ShellUi {
            icons: Lru::new(ICON_CAP_ITEMS, ICON_CAP_BYTES),
            pending: HashMap::new(),
            by_id: HashMap::new(),
            wants: Vec::new(),
            frame: 0,
            icon_px,
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

    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.wants.clear();
    }

    /// The icon for a visible row, or None (placeholder) while it loads.
    /// `root` is the folder of a real entry, None for fake rows.
    pub fn row_icon(&mut self, row: &RowBuf, root: Option<&str>) -> Option<&Image> {
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
                write!(self.key, "p:{root}{sep}{}@{}", row.name_str(), self.icon_px)
            }
            C_FOLDER => write!(self.key, "folder@{}", self.icon_px),
            _ => {
                self.key.push_str("e:");
                self.key.extend(ext.chars().map(|c| c.to_ascii_lowercase()));
                write!(self.key, "@{}", self.icon_px)
            }
        };
        if self.icons.contains(self.key.as_str()) {
            return self.icons.get(self.key.as_str()).filter(|i| !i.px.is_empty());
        }
        if let Some(p) = self.pending.get_mut(self.key.as_str()) {
            p.frame = self.frame;
            return None;
        }
        if self.wants.iter().any(|(k, _, _)| &**k == self.key.as_str()) {
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
        self.wants.push((Arc::from(self.key.as_str()), src, class));
        None
    }

    /// Submits this frame's misses; cancels requests for rows scrolled away.
    pub fn end_frame(&mut self, pool: &mut Pool) {
        let frame = self.frame;
        let mut cancelled = 0;
        let by_id = &mut self.by_id;
        self.pending.retain(|_, p| {
            if p.frame == frame {
                return true;
            }
            p.ticket.cancel();
            by_id.remove(&p.ticket.id);
            cancelled += 1;
            false
        });
        self.stats.cancelled += cancelled;
        let size = self.icon_px;
        for (key, src, class) in self.wants.drain(..) {
            let ticket = pool.submit(Request::Icon { src, size });
            self.by_id.insert(ticket.id, key.clone());
            self.pending.insert(key, Pending { ticket, frame, at: Instant::now(), class });
            self.stats.requested += 1;
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
        match r.payload {
            Payload::Icon(bmp) => {
                let Some(key) = self.by_id.remove(&r.id) else {
                    self.stats.stale += 1;
                    return false;
                };
                let p = self.pending.remove(&key);
                self.stats.received += 1;
                if let Some(p) = p {
                    let c = p.class as usize;
                    self.stats.latency_us[c].push(p.at.elapsed().as_micros() as u64);
                    self.stats.work_us[c].push(r.work_us);
                    if self.stats.first.is_none() {
                        self.stats.first = Some((p.class, r.work_us));
                    }
                }
                if bmp.is_none() {
                    self.stats.failed += 1;
                }
                let img = Image::from(key.clone(), bmp);
                let b = img.bytes();
                self.icons.insert(key, img, b);
                true
            }
            Payload::Thumb { .. } => {
                if self.collect_other {
                    self.other_replies.push(r);
                } else {
                    self.stats.stale += 1;
                }
                false
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
        }
    }

    pub fn open_menu(&mut self, pool: &mut Pool, paths: Vec<String>, x: i32, y: i32, extended: bool, owner: Option<isize>) {
        let n = paths.len();
        let t = pool.submit(Request::Menu { paths, x, y, extended, auto_close: None, owner });
        self.menu_pending = Some(t.id);
        self.menu_asked = Some(Instant::now());
        self.menu_status = format!("menu for {n} item(s) requested{}", if owner.is_some() { " (owner = UI window, wrong thread)" } else { "" });
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
        let names = ["by extension", "folder", "per path"];
        let mut out = String::new();
        out.push_str(&format!(
            "icons: requested {} received {} cancelled(UI) {} stale-dropped {} failed {} max_pending {}\n",
            self.requested, self.received, self.cancelled, self.stale, self.failed, self.max_pending
        ));
        if let Some((c, us)) = self.first {
            out.push_str(&format!("icon first reply (cold shell): {} {:.2} ms worker time\n", names[c as usize], us as f64 / 1000.0));
        }
        for (c, name) in names.iter().enumerate() {
            out.push_str(&format!("icon worker time {name}: {}\n", pcts(&self.work_us[c])));
            out.push_str(&format!("icon submit-to-applied {name}: {}\n", pcts(&self.latency_us[c])));
        }
        out
    }
}
