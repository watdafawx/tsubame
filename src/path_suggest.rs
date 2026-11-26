//! B11-H: address-bar autocomplete (Explorer parity).
//!
//! While the path bar is a text field, the typed text is parsed into what to
//! list (`Target`): the subfolders of the typed parent that start with the
//! typed fragment, the drive roots (`C`, `C:`), or the drives and recent
//! folders (empty field). The parent is listed on the existing enumerator
//! worker (`tsubame_win::enumerate`), never on the UI thread:
//!
//! - a keystroke that keeps the same parent re-filters the cached names in
//!   memory (no disk, no timer, no thread);
//! - a keystroke that changes the parent cancels the running listing and arms
//!   one 80 ms debounce (`next_timer`, a one-shot the window kills when it
//!   fires): idle is zero timers;
//! - a completion (Tab, Right, a click) lists the new parent at once;
//! - every listing carries a generation of the shared `CancelToken`, so the
//!   reply of a listing the user typed past is dropped.
//!
//! UNC: the server level is never listed; completion starts below an
//! already typed `\\server\share\`.
//!
//! The model is plain data (parse, rank, keys) with unit tests; the dropdown is
//! drawn and fed by `view_suggest.rs`.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use tsubame_ui::{Key, Mods, Rect};
use tsubame_win::enumerate::{CancelToken, Enumerator, Message, Wake};

use crate::nav_menu::nat_cmp_ci;
use crate::tab::{HIDE, flags_of};

/// Rows the dropdown shows at most.
pub const MAX_ROWS: usize = 8;
/// A parent change waits this long before it is listed (ms).
pub const DEBOUNCE_MS: u64 = 80;
/// Win32 timer id of the one-shot debounce (owned by `main.rs`).
pub const TIMER_ID: usize = 14;
/// A cached listing older than this is listed again in the background.
const CACHE_TTL: Duration = Duration::from_secs(3);
/// Names kept per listing.
const NAME_CAP: usize = 100_000;
/// Drive rows shown with an empty field (the recent folders fill the rest).
const DRIVE_ROWS: usize = 3;

/// What the typed text asks to list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// nothing to suggest (not a path, a UNC server, an archive, ...)
    None,
    /// empty field: the drives, then the recent folders
    Empty,
    /// `C` or `C:`: the drive root of this letter
    Drive(char),
    /// the subfolders of `parent` that start with `prefix`; `head` is the typed
    /// text up to and including its last separator (what a completion keeps)
    Folder { parent: String, prefix: String, head: String },
}

/// Looks an environment variable up (a closure so the tests need no real environment).
pub type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The process environment (Windows names are case-insensitive).
pub fn system_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `%NAME%` becomes its value and a leading `~` becomes `%USERPROFILE%`. An
/// unknown or unclosed `%` stays as typed.
pub fn expand_env(s: &str, env: EnvFn) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut rest = s;
    if let Some(tail) = rest.strip_prefix('~')
        && (tail.is_empty() || tail.starts_with(['\\', '/']))
        && let Some(home) = env("USERPROFILE")
    {
        out.push_str(&home);
        rest = tail;
    }
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) if j > 0 => {
                match env(&after[..j]) {
                    Some(v) => out.push_str(&v),
                    None => out.push_str(&rest[i..i + j + 2]),
                }
                rest = &after[j + 1..];
            }
            Some(_) => {
                out.push_str("%%");
                rest = &after[1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The absolute folder a head (`C:\a\`, `\\srv\share\x\`) names, without its
/// trailing separator (kept on a drive root). `None` for a relative path, a
/// UNC server (`\\srv\`), a device path or an empty segment (`C:\a\\b`).
fn folder_of_head(h: &str) -> Option<String> {
    if let Some(unc) = h.strip_prefix(r"\\") {
        if unc.starts_with(['?', '.']) {
            return None;
        }
        let inner = unc.trim_end_matches('\\');
        let mut it = inner.split('\\');
        let (server, share) = (it.next()?, it.next()?);
        if server.is_empty() || share.is_empty() || it.any(str::is_empty) {
            return None;
        }
        return Some(format!(r"\\{inner}"));
    }
    let b = h.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        let tail = h[3..].trim_end_matches('\\');
        if !tail.is_empty() && tail.split('\\').any(str::is_empty) {
            return None;
        }
        return Some(if tail.is_empty() { h[..3].to_string() } else { format!("{}{tail}", &h[..3]) });
    }
    None
}

/// Typed path-bar text to what it asks to list (see [`Target`]).
pub fn parse(text: &str, env: EnvFn) -> Target {
    let t = text.trim_start();
    if t.is_empty() {
        return Target::Empty;
    }
    // an archive's virtual path (`C:\a.zip::sub`) is not listed here
    if t.contains("::") {
        return Target::None;
    }
    let b = t.as_bytes();
    if b[0].is_ascii_alphabetic() && (b.len() == 1 || (b.len() == 2 && b[1] == b':')) {
        return Target::Drive(b[0].to_ascii_uppercase() as char);
    }
    let norm = t.replace('/', "\\");
    let Some(cut) = norm.rfind('\\') else { return Target::None };
    let (head, prefix) = norm.split_at(cut + 1);
    match folder_of_head(&expand_env(head, env)) {
        Some(parent) => Target::Folder { parent, prefix: prefix.to_string(), head: head.to_string() },
        None => Target::None,
    }
}

/// `name` starts with `prefix`, ignoring case.
pub fn starts_with_ci(name: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if name.is_ascii() && prefix.is_ascii() {
        return name.len() >= prefix.len() && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes());
    }
    let mut n = name.chars().flat_map(char::to_lowercase);
    prefix.chars().flat_map(char::to_lowercase).all(|p| n.next() == Some(p))
}

/// Indices into `names` (natural, case-insensitive order already) of the (up to)
/// `max` best matches of `prefix`: an exact name first, then names that match
/// with the typed case, then the rest, each group in the order of `names`.
pub fn rank(names: &[String], prefix: &str, max: usize) -> Vec<usize> {
    let mut tiers: [Vec<usize>; 3] = Default::default();
    for (i, n) in names.iter().enumerate() {
        let tier = if n.starts_with(prefix) {
            if n.len() == prefix.len() { 0 } else { 1 }
        } else if starts_with_ci(n, prefix) {
            2
        } else {
            continue;
        };
        if tiers[tier].len() < max {
            tiers[tier].push(i);
        }
    }
    tiers.into_iter().flatten().take(max).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Folder,
    Drive,
    Recent,
}

/// One dropdown row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// dim text at the right (the volume label, "Recent")
    pub detail: String,
    /// what the field holds after Tab / Right (ends in `\`)
    pub complete: String,
    /// the folder Enter or a click opens
    pub nav: String,
    pub kind: ItemKind,
}

/// What a key did to the dropdown (the view acts on `Complete` and `Go`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyOut {
    /// not for the dropdown: the text field handles it
    Pass,
    /// handled here
    Eat,
    /// replace the field's text with this (then list the new parent at once)
    Complete(String),
    /// open this folder
    Go(String),
}

struct Cache {
    parent: String,
    names: Vec<String>,
    at: Instant,
}

struct Job {
    parent: String,
    seq_gen: u64,
    _worker: Enumerator,
    rx: Receiver<Message>,
    names: Vec<String>,
}

struct Pending {
    parent: String,
    due: Instant,
    seq: u64,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

pub struct PathSuggest {
    /// pane slot the dropdown belongs to
    pub slot: usize,
    /// the path bar's rect (the dropdown hangs under it)
    pub bar: Rect,
    /// the field is being edited and the dropdown may show
    pub active: bool,
    /// the user typed (or emptied the field) since it opened
    armed: bool,
    /// Esc closed the dropdown until the next change
    dismissed: bool,
    pub items: Vec<Item>,
    pub sel: Option<usize>,
    target: Target,
    cache: Option<Cache>,
    job: Option<Job>,
    pending: Option<Pending>,
    seq: u64,
    token: CancelToken,
    wake: Wake,
    drives: Vec<(String, String)>,
    recents: Vec<String>,
    /// last listing finished (ms from `update` to the reply), for the tests and the report
    pub last_list_ms: Option<f32>,
    list_started: Option<Instant>,
}

impl PathSuggest {
    pub fn new(wake: Wake) -> PathSuggest {
        PathSuggest {
            slot: 0,
            bar: Rect::new(0.0, 0.0, 0.0, 0.0),
            active: false,
            armed: false,
            dismissed: false,
            items: Vec::new(),
            sel: None,
            target: Target::None,
            cache: None,
            job: None,
            pending: None,
            seq: 1,
            token: CancelToken::new(),
            wake,
            drives: Vec::new(),
            recents: Vec::new(),
            last_list_ms: None,
            list_started: None,
        }
    }

    /// The field opens on `text`: remembers the places, starts listing the parent
    /// of `text` now (so the first keystroke already has names) and shows nothing yet.
    pub fn begin(&mut self, slot: usize, text: &str, drives: Vec<(String, String)>, recents: Vec<String>, env: EnvFn) {
        self.close();
        self.slot = slot;
        self.active = true;
        self.drives = drives;
        self.recents = recents;
        self.update(text, Instant::now(), true, env);
        self.items.clear();
    }

    /// The field closed (Esc, navigation, focus lost): drops every listing and name.
    pub fn close(&mut self) {
        self.cancel_job();
        self.pending = None;
        self.cache = None;
        self.items.clear();
        self.sel = None;
        self.target = Target::None;
        self.active = false;
        self.armed = false;
        self.dismissed = false;
    }

    /// The user typed: the dropdown may show again.
    pub fn typed(&mut self, text: &str, now: Instant, immediate: bool, env: EnvFn) {
        self.armed = true;
        self.dismissed = false;
        self.update(text, now, immediate, env);
    }

    pub fn visible(&self) -> bool {
        self.active && self.armed && !self.dismissed && !self.items.is_empty()
    }

    fn cancel_job(&mut self) {
        if self.job.take().is_some() {
            self.token.cancel();
        }
    }

    fn start_job(&mut self, parent: &str) {
        self.pending = None;
        self.job = None;
        let seq_gen = self.token.next_generation();
        let (worker, rx) = Enumerator::start_channel(parent, seq_gen, self.token.clone(), self.wake.clone());
        self.list_started = Some(Instant::now());
        self.job = Some(Job { parent: parent.to_string(), seq_gen, _worker: worker, rx, names: Vec::new() });
    }

    /// Parses `text` and lines the work up: re-filter the cache, list the parent
    /// (at once when `immediate` or when a cache only needs a refresh) or wait out
    /// the debounce. Work for another parent is cancelled.
    pub fn update(&mut self, text: &str, now: Instant, immediate: bool, env: EnvFn) {
        self.target = parse(text, env);
        let want = match &self.target {
            Target::Folder { parent, .. } => Some(parent.clone()),
            _ => None,
        };
        let wanted = |p: &str| want.as_deref().is_some_and(|w| same(w, p));
        if self.job.as_ref().is_some_and(|j| !wanted(&j.parent)) {
            self.cancel_job();
        }
        if self.pending.as_ref().is_some_and(|p| !wanted(&p.parent)) {
            self.pending = None;
        }
        if let Some(parent) = want {
            let cached = self.cache.as_ref().filter(|c| same(&c.parent, &parent));
            let fresh = cached.is_some_and(|c| c.at.elapsed() <= CACHE_TTL);
            let cached = cached.is_some();
            let listing = self.job.is_some();
            let waiting = self.pending.is_some();
            if !fresh && !listing {
                if immediate || cached {
                    self.start_job(&parent);
                } else if !waiting {
                    self.seq += 1;
                    self.pending = Some(Pending { parent, due: now + Duration::from_millis(DEBOUNCE_MS), seq: self.seq });
                }
            }
        }
        self.refresh();
    }

    /// Remaining debounce as `(sequence, ms)` for the window's one-shot timer; `None`
    /// when nothing waits (no timer exists then).
    pub fn next_timer(&self, now: Instant) -> Option<(u64, u32)> {
        let p = self.pending.as_ref()?;
        let left = p.due.saturating_duration_since(now);
        Some((p.seq, (left.as_millis() as u32).max(1)))
    }

    /// The debounce timer fired (or a paint found it due): lists the parent.
    pub fn on_timer(&mut self, now: Instant) -> bool {
        let Some(p) = self.pending.as_ref() else { return false };
        if p.due > now {
            return false;
        }
        let parent = p.parent.clone();
        self.start_job(&parent);
        true
    }

    /// A debounce is waiting.
    #[cfg(test)]
    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// A listing runs.
    #[cfg(test)]
    pub fn listing(&self) -> bool {
        self.job.is_some()
    }

    /// Drains the listing worker. UI thread, non-blocking. True when the rows changed.
    pub fn pump(&mut self) -> bool {
        let Some(job) = self.job.as_mut() else { return false };
        let hide = HIDE.load(std::sync::atomic::Ordering::Relaxed);
        let mut done = false;
        loop {
            match job.rx.try_recv() {
                Ok(msg) => {
                    // a listing the user typed past: its replies are dropped
                    if msg.generation() != job.seq_gen {
                        continue;
                    }
                    match msg {
                        Message::Batch(b) => {
                            for (name, raw) in b.iter() {
                                if !raw.is_dir() || job.names.len() >= NAME_CAP {
                                    continue;
                                }
                                if flags_of(raw.attrs, true, raw.is_reparse_point(), raw.reparse_tag) & hide != 0 {
                                    continue;
                                }
                                job.names.push(String::from_utf16_lossy(name));
                            }
                        }
                        Message::Done(_) => {
                            done = true;
                            break;
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        if !done {
            return false;
        }
        let Some(mut job) = self.job.take() else { return false };
        job.names.sort_by(|a, b| nat_cmp_ci(a, b));
        self.last_list_ms = self.list_started.take().map(|t| t.elapsed().as_secs_f32() * 1000.0);
        self.cache = Some(Cache { parent: job.parent, names: job.names, at: Instant::now() });
        self.refresh();
        true
    }

    /// Rebuilds the rows from the parsed target and what is cached.
    fn refresh(&mut self) {
        let keep = self.sel.and_then(|i| self.items.get(i)).map(|it| it.nav.clone());
        self.items.clear();
        match &self.target {
            Target::Folder { parent, prefix, head } => {
                if let Some(c) = self.cache.as_ref().filter(|c| same(&c.parent, parent)) {
                    for i in rank(&c.names, prefix, MAX_ROWS) {
                        let name = &c.names[i];
                        self.items.push(Item {
                            label: name.clone(),
                            detail: String::new(),
                            complete: format!("{head}{name}\\"),
                            nav: crate::nav::join(parent, name),
                            kind: ItemKind::Folder,
                        });
                    }
                }
            }
            Target::Drive(letter) => {
                for (root, label) in &self.drives {
                    if root.chars().next().is_some_and(|c| c.eq_ignore_ascii_case(letter)) {
                        self.items.push(Item { label: root.clone(), detail: label.clone(), complete: root.clone(), nav: root.clone(), kind: ItemKind::Drive });
                    }
                }
            }
            Target::Empty => {
                for (root, label) in self.drives.iter().take(DRIVE_ROWS) {
                    self.items.push(Item { label: root.clone(), detail: label.clone(), complete: root.clone(), nav: root.clone(), kind: ItemKind::Drive });
                }
                let room = MAX_ROWS - self.items.len();
                for p in self.recents.iter().take(room) {
                    let name = crate::nav_menu::folder_label(p);
                    let complete = if p.ends_with('\\') { p.clone() } else { format!("{p}\\") };
                    self.items.push(Item { label: name, detail: crate::nav_menu::compact_path(p, 40), complete, nav: p.clone(), kind: ItemKind::Recent });
                }
            }
            Target::None => {}
        }
        // the highlighted folder stays highlighted while the list refreshes
        self.sel = keep.and_then(|k| self.items.iter().position(|it| it.nav == k));
    }

    /// The typed fragment of the current target (for the "Right completes" rule).
    fn fragment_len(&self) -> usize {
        match &self.target {
            Target::Folder { prefix, .. } => prefix.len(),
            Target::Drive(_) => 1,
            _ => 0,
        }
    }

    /// One key for the dropdown. `caret_at_end`: the caret is at the end of the
    /// text with no selection (Right completes only there).
    pub fn key(&mut self, key: Key, mods: Mods, caret_at_end: bool) -> KeyOut {
        if !self.visible() {
            return KeyOut::Pass;
        }
        let n = self.items.len();
        match (key, mods) {
            (Key::Down, m) if m.none() => {
                self.sel = Some(self.sel.map_or(0, |i| (i + 1) % n));
                KeyOut::Eat
            }
            (Key::Up, m) if m.none() => {
                self.sel = Some(self.sel.map_or(n - 1, |i| (i + n - 1) % n));
                KeyOut::Eat
            }
            (Key::Tab, m) if m.none() => KeyOut::Complete(self.items[self.sel.unwrap_or(0)].complete.clone()),
            (Key::Tab, m) if m == Mods::SHIFT => {
                self.sel = Some(self.sel.map_or(n - 1, |i| (i + n - 1) % n));
                KeyOut::Eat
            }
            (Key::Right, m) if m.none() && caret_at_end && (self.sel.is_some() || self.fragment_len() > 0) => KeyOut::Complete(self.items[self.sel.unwrap_or(0)].complete.clone()),
            (Key::Enter, m) if m.none() => match self.sel {
                Some(i) => KeyOut::Go(self.items[i].nav.clone()),
                None => KeyOut::Pass,
            },
            (Key::Escape, m) if m.none() => {
                self.dismissed = true;
                self.sel = None;
                KeyOut::Eat
            }
            _ => KeyOut::Pass,
        }
    }

    #[cfg(test)]
    pub fn target(&self) -> &Target {
        &self.target
    }

    #[cfg(test)]
    pub fn cached_names(&self) -> Option<&[String]> {
        self.cache.as_ref().map(|c| c.names.as_slice())
    }

    #[cfg(test)]
    pub fn cache_parent(&self) -> Option<&str> {
        self.cache.as_ref().map(|c| c.parent.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn env(name: &str) -> Option<String> {
        match name.to_ascii_uppercase().as_str() {
            "USERPROFILE" => Some(r"C:\Users\me".into()),
            "APPDATA" => Some(r"C:\Users\me\AppData\Roaming".into()),
            _ => None,
        }
    }

    fn folder(parent: &str, prefix: &str, head: &str) -> Target {
        Target::Folder { parent: parent.into(), prefix: prefix.into(), head: head.into() }
    }

    #[test]
    fn typed_path_splits_into_parent_and_prefix() {
        assert_eq!(parse(r"C:\Users\me\Doc", &env), folder(r"C:\Users\me", "Doc", r"C:\Users\me\"));
        assert_eq!(parse(r"C:\Users\me\", &env), folder(r"C:\Users\me", "", r"C:\Users\me\"));
        assert_eq!(parse(r"C:\Us", &env), folder(r"C:\", "Us", r"C:\"), "a root keeps its backslash");
        assert_eq!(parse("c:/users/me/do", &env), folder("c:\\users\\me", "do", "c:\\users\\me\\"), "forward slashes");
        assert_eq!(parse(r"  C:\Users\", &env), folder(r"C:\Users", "", r"C:\Users\"), "leading blanks");
        assert_eq!(parse(r"C:\a b\c d", &env), folder(r"C:\a b", "c d", r"C:\a b\"));
    }

    #[test]
    fn drives_empty_and_unparsable_text() {
        assert_eq!(parse("", &env), Target::Empty);
        assert_eq!(parse("   ", &env), Target::Empty);
        assert_eq!(parse("c", &env), Target::Drive('C'));
        assert_eq!(parse("D:", &env), Target::Drive('D'));
        assert_eq!(parse("Users", &env), Target::None, "relative text");
        assert_eq!(parse(r"Users\me\", &env), Target::None);
        assert_eq!(parse(r"C:\a\\b\x", &env), Target::None, "empty segment");
        assert_eq!(parse(r"C:\a.zip::sub\x", &env), Target::None, "archive path");
        assert_eq!(parse("C:foo", &env), Target::None);
    }

    #[test]
    fn env_vars_and_tilde_expand_but_the_head_stays_as_typed() {
        assert_eq!(expand_env(r"%USERPROFILE%\Doc", &env), r"C:\Users\me\Doc");
        assert_eq!(expand_env(r"%userprofile%\x", &env), r"C:\Users\me\x", "name case");
        assert_eq!(expand_env(r"~\Desktop", &env), r"C:\Users\me\Desktop");
        assert_eq!(expand_env("~", &env), r"C:\Users\me");
        assert_eq!(expand_env("~x", &env), "~x", "only ~ and ~\\");
        assert_eq!(expand_env(r"%NOPE%\x", &env), r"%NOPE%\x", "unknown stays");
        assert_eq!(expand_env(r"C:\100%\x", &env), r"C:\100%\x", "unclosed stays");
        assert_eq!(expand_env(r"%APPDATA%\%USERPROFILE%", &env), r"C:\Users\me\AppData\Roaming\C:\Users\me");
        assert_eq!(expand_env("%%", &env), "%%");
        assert_eq!(parse(r"%USERPROFILE%\Doc", &env), folder(r"C:\Users\me", "Doc", r"%USERPROFILE%\"));
        assert_eq!(parse(r"~\", &env), folder(r"C:\Users\me", "", r"~\"));
        assert_eq!(parse(r"~/Down", &env), folder(r"C:\Users\me", "Down", r"~\"));
        assert_eq!(parse("%USERPROFILE%", &env), Target::None, "no separator yet");
        assert_eq!(parse(r"%NOPE%\x", &env), Target::None);
    }

    #[test]
    fn unc_completes_only_below_a_typed_share() {
        assert_eq!(parse(r"\\srv", &env), Target::None);
        assert_eq!(parse(r"\\srv\", &env), Target::None, "the server level is not listed");
        assert_eq!(parse(r"\\srv\sha", &env), Target::None);
        assert_eq!(parse(r"\\srv\share\", &env), folder(r"\\srv\share", "", r"\\srv\share\"));
        assert_eq!(parse(r"\\srv\share\Pro", &env), folder(r"\\srv\share", "Pro", r"\\srv\share\"));
        assert_eq!(parse(r"\\srv\share\a\b", &env), folder(r"\\srv\share\a", "b", r"\\srv\share\a\"));
        assert_eq!(parse(r"\\?\C:\x", &env), Target::None, "device paths");
        assert_eq!(parse(r"\\srv\\x", &env), Target::None);
    }

    fn names(v: &[&str]) -> Vec<String> {
        let mut n: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        n.sort_by(|a, b| nat_cmp_ci(a, b));
        n
    }

    #[test]
    fn ranking_is_exact_then_same_case_then_any_case_in_natural_order() {
        let n = names(&["Documents", "docs", "Doc", "doc", "Downloads", "dir10", "dir2", "Desktop"]);
        let pick = |p: &str, max| rank(&n, p, max).into_iter().map(|i| n[i].as_str()).collect::<Vec<_>>();
        assert_eq!(pick("doc", 8), ["doc", "docs", "Doc", "Documents"], "exact, same case, then other case");
        assert_eq!(pick("Doc", 8), ["Doc", "Documents", "doc", "docs"]);
        assert_eq!(pick("d", 3), ["dir2", "dir10", "doc"], "natural order, capped");
        assert_eq!(pick("DIR", 8), ["dir2", "dir10"], "case-insensitive prefix");
        assert_eq!(pick("x", 8), Vec::<&str>::new());
        assert_eq!(pick("", 4).len(), 4);
        // not a substring match
        assert!(!pick("ocs", 8).contains(&"docs"));
        // non-ASCII folds too
        let u = names(&["Ärger", "Zoo"]);
        assert_eq!(rank(&u, "är", 8), vec![u.iter().position(|n| n == "Ärger").unwrap()]);
    }

    fn sample(parent: &str, head: &str, prefix: &str) -> PathSuggest {
        let mut s = PathSuggest::new(Arc::new(|| {}));
        s.active = true;
        s.armed = true;
        s.target = folder(parent, prefix, head);
        s.cache = Some(Cache { parent: parent.into(), names: names(&["Alpha", "alpine", "Beta"]), at: Instant::now() });
        s.refresh();
        s
    }

    #[test]
    fn rows_complete_with_the_typed_head_and_a_backslash() {
        let s = sample(r"C:\Users\me", r"%USERPROFILE%\", "al");
        let rows: Vec<(&str, &str, &str)> = s.items.iter().map(|i| (i.label.as_str(), i.complete.as_str(), i.nav.as_str())).collect();
        assert_eq!(rows, [("alpine", r"%USERPROFILE%\alpine\", r"C:\Users\me\alpine"), ("Alpha", r"%USERPROFILE%\Alpha\", r"C:\Users\me\Alpha")]);
    }

    #[test]
    fn keys_move_complete_enter_and_escape() {
        let mut s = sample(r"C:\p", r"C:\p\", "");
        let none = Mods::NONE;
        assert!(s.visible());
        assert_eq!(s.sel, None);
        // Enter without a highlight is the field's own Enter
        assert_eq!(s.key(Key::Enter, none, true), KeyOut::Pass);
        assert_eq!(s.key(Key::Down, none, true), KeyOut::Eat);
        assert_eq!(s.sel, Some(0));
        s.key(Key::Down, none, true);
        s.key(Key::Down, none, true);
        assert_eq!(s.sel, Some(2));
        s.key(Key::Down, none, true);
        assert_eq!(s.sel, Some(0), "wraps");
        assert_eq!(s.key(Key::Up, none, true), KeyOut::Eat);
        assert_eq!(s.sel, Some(2), "up from the first wraps to the last");
        assert_eq!(s.key(Key::Enter, none, true), KeyOut::Go(r"C:\p\Beta".into()));
        assert_eq!(s.key(Key::Tab, none, false), KeyOut::Complete(r"C:\p\Beta\".into()));
        // Tab without a highlight takes the first row
        s.sel = None;
        assert_eq!(s.key(Key::Tab, none, false), KeyOut::Complete(r"C:\p\Alpha\".into()));
        // Right: only at the end of the text, and not for an empty fragment without a highlight
        assert_eq!(s.key(Key::Right, none, true), KeyOut::Pass, "empty fragment, nothing highlighted");
        s.sel = Some(1);
        assert_eq!(s.key(Key::Right, none, false), KeyOut::Pass, "caret not at the end");
        assert_eq!(s.key(Key::Right, none, true), KeyOut::Complete(r"C:\p\alpine\".into()));
        let mut t = sample(r"C:\p", r"C:\p\", "al");
        assert_eq!(t.key(Key::Right, none, true), KeyOut::Complete(r"C:\p\alpine\".into()), "a typed fragment completes");
        // modified keys are not ours
        assert_eq!(s.key(Key::Down, Mods::CTRL, true), KeyOut::Pass);
        // Esc closes the dropdown first, the next Esc is the field's
        assert_eq!(s.key(Key::Escape, none, true), KeyOut::Eat);
        assert!(!s.visible());
        assert_eq!(s.key(Key::Escape, none, true), KeyOut::Pass);
        // typing again brings it back
        s.typed(r"C:\p\a", Instant::now(), false, &env);
        assert!(s.visible());
    }

    #[test]
    fn drives_and_recent_rows() {
        let mut s = PathSuggest::new(Arc::new(|| {}));
        let drives: Vec<(String, String)> = [("C:\\", "Windows"), ("D:\\", "Data"), ("E:\\", "Backup"), ("F:\\", "USB")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let recents: Vec<String> = (0..10).map(|i| format!(r"C:\work\proj{i}")).collect();
        s.begin(0, "x", drives, recents, &env);
        s.typed("", Instant::now(), false, &env);
        assert_eq!(s.target(), &Target::Empty);
        assert_eq!(s.items.len(), MAX_ROWS);
        assert_eq!(s.items.iter().filter(|i| i.kind == ItemKind::Drive).count(), 3);
        assert_eq!(s.items[3].kind, ItemKind::Recent);
        assert_eq!(s.items[3].label, "proj0");
        assert_eq!(s.items[3].complete, r"C:\work\proj0\");
        assert_eq!(s.items[3].nav, r"C:\work\proj0");
        s.typed("d", Instant::now(), false, &env);
        assert_eq!(s.items.len(), 1);
        assert_eq!((s.items[0].label.as_str(), s.items[0].detail.as_str(), s.items[0].complete.as_str()), ("D:\\", "Data", "D:\\"));
        s.typed("d:", Instant::now(), false, &env);
        assert_eq!(s.items.len(), 1);
        assert!(!s.waiting() && !s.listing(), "drive rows need no listing");
        s.close();
        assert!(s.items.is_empty() && !s.active);
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-psug-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pump_until(s: &mut PathSuggest, what: &str) {
        let t0 = Instant::now();
        while s.listing() {
            s.pump();
            assert!(t0.elapsed() < Duration::from_secs(20), "{what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_parent_change_waits_one_debounce_and_then_lists_once() {
        let d = temp("debounce");
        for n in ["alpha", "alps", "beta"] {
            std::fs::create_dir(d.join(n)).unwrap();
        }
        std::fs::write(d.join("a-file.txt"), b"x").unwrap();
        let root = d.to_string_lossy().into_owned();
        let mut s = PathSuggest::new(Arc::new(|| {}));
        s.active = true;
        let t0 = Instant::now();
        s.typed(&format!("{root}\\al"), t0, false, &env);
        assert!(s.waiting() && !s.listing(), "no listing before the debounce");
        assert!(s.items.is_empty());
        let (seq, ms) = s.next_timer(t0).unwrap();
        assert!(ms as u64 <= DEBOUNCE_MS && ms > 0);
        // typing on under the same parent keeps the one timer (same sequence)
        s.typed(&format!("{root}\\alp"), t0 + Duration::from_millis(30), false, &env);
        assert_eq!(s.next_timer(t0 + Duration::from_millis(30)).map(|(q, _)| q), Some(seq));
        assert!(!s.on_timer(t0 + Duration::from_millis(40)), "not due yet");
        assert!(s.on_timer(t0 + Duration::from_millis(DEBOUNCE_MS + 1)));
        assert!(!s.waiting() && s.listing());
        assert_eq!(s.next_timer(Instant::now()), None, "idle: no timer");
        pump_until(&mut s, "listing");
        assert_eq!(s.cached_names().unwrap(), ["alpha", "alps", "beta"], "folders only");
        let labels: Vec<&str> = s.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["alpha", "alps"]);
        // the next keystroke under the same parent filters the cache: no work queued
        s.typed(&format!("{root}\\alph"), Instant::now(), false, &env);
        assert!(!s.waiting() && !s.listing());
        assert_eq!(s.items.len(), 1);
        // completing into a folder lists it now, without a debounce
        s.typed(&format!("{root}\\alpha\\"), Instant::now(), true, &env);
        assert!(s.listing() && !s.waiting());
        pump_until(&mut s, "child listing");
        assert_eq!(s.cache_parent(), Some(d.join("alpha").to_str().unwrap()));
        assert!(s.items.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stale_listing_never_reaches_the_rows() {
        let a = temp("stale-a");
        let b = temp("stale-b");
        std::fs::create_dir(a.join("from-a")).unwrap();
        std::fs::create_dir(b.join("from-b")).unwrap();
        let mut s = PathSuggest::new(Arc::new(|| {}));
        s.active = true;
        s.typed(&format!("{}\\", a.display()), Instant::now(), true, &env);
        assert!(s.listing());
        // the user types on to another parent before A's reply was read
        s.typed(&format!("{}\\", b.display()), Instant::now(), true, &env);
        pump_until(&mut s, "second listing");
        assert_eq!(s.cache_parent(), Some(b.to_str().unwrap()));
        assert_eq!(s.cached_names().unwrap(), ["from-b"]);
        let labels: Vec<&str> = s.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["from-b"]);
        // a parent change while listing cancels the listing and falls back to the debounce
        s.typed(&format!("{}\\", a.display()), Instant::now(), true, &env);
        s.typed(&format!("{}\\x\\", a.display()), Instant::now(), false, &env);
        assert!(!s.listing() && s.waiting(), "the old listing was cancelled");
        assert!(s.items.is_empty());
        // leaving the field drops everything
        s.close();
        assert!(!s.waiting() && !s.listing() && s.cache_parent().is_none());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_failing_listing_is_cached_as_empty_and_not_retried_at_once() {
        let mut s = PathSuggest::new(Arc::new(|| {}));
        s.active = true;
        let gone = std::env::temp_dir().join(format!("tsubame-psug-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&gone);
        s.typed(&format!("{}\\", gone.display()), Instant::now(), true, &env);
        pump_until(&mut s, "failing listing");
        assert!(s.items.is_empty());
        s.typed(&format!("{}\\a", gone.display()), Instant::now(), false, &env);
        assert!(!s.listing() && !s.waiting(), "cached within the TTL");
    }

    /// Reports the keystroke-to-rows time on a 5 000 folder directory (`--nocapture`).
    #[test]
    fn five_thousand_folders_filter_in_well_under_a_frame() {
        let d = temp("5k");
        for i in 0..5000 {
            std::fs::create_dir(d.join(format!("project-{i:04}"))).unwrap();
        }
        let root = d.to_string_lossy().into_owned();
        let mut s = PathSuggest::new(Arc::new(|| {}));
        s.begin(0, &format!("{root}\\"), Vec::new(), Vec::new(), &env);
        let t0 = Instant::now();
        pump_until(&mut s, "5k listing");
        let cold = t0.elapsed();
        assert_eq!(s.cached_names().unwrap().len(), 5000);
        // warm: every keystroke re-filters the cache
        let mut worst = Duration::ZERO;
        for (k, text) in ["p", "pr", "pro", "proj", "project-", "project-0", "project-04", "project-049"].iter().enumerate() {
            let t = Instant::now();
            s.typed(&format!("{root}\\{text}"), Instant::now(), false, &env);
            let dt = t.elapsed();
            worst = worst.max(dt);
            assert!(!s.items.is_empty(), "keystroke {k}");
            assert!(s.items.len() <= MAX_ROWS);
        }
        eprintln!("psug 5k folders: cold list+sort {:.1} ms (worker {:?} ms), warm keystroke worst {:.3} ms", cold.as_secs_f32() * 1000.0, s.last_list_ms, worst.as_secs_f32() * 1000.0);
        assert!(worst < Duration::from_millis(20), "warm keystroke {worst:?}");
        assert_eq!(s.items[0].label, "project-0490");
        let _ = std::fs::remove_dir_all(&d);
    }
}
