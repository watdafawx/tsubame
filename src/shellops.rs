//! Shell integration glue (W1): the Explorer context menu's app entries and
//! intercepted verbs, OLE drag and drop in and out, the clipboard (cut, copy,
//! paste, copy path), and Delete with the Recycle Bin hazard checks. Every
//! file change goes through the file-op queue (progress, conflicts, undo).
//! See `docs/shell-integration.md`.
//!
//! Threading: the UI thread never makes a blocking shell call. Menus and
//! clipboard jobs run on the shell pool's STA workers (`Request::Menu`,
//! `Request::Clip`); file operations on the queue's worker; the Recycle Bin
//! checks (they size folders) on a short-lived thread. OLE drag and drop
//! itself has to run on the UI thread (`DoDragDrop`, `IDropTarget`), which
//! is an OLE STA for that.
//!
//! State lives in `App::ops`. Like the rest of main.rs, nothing here holds
//! `crate::with` across a call that can re-enter the window procedure
//! (`DoDragDrop`, `MessageBoxW`).

use crate::ole_glue;
use crate::{App, invalidate, push, with};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use tsubame_ui::{Button, Event, Mods, Pt, Rect};
use tsubame_win::dnd::clipboard::{ClipJob, ClipRead, ClipReply};
use tsubame_win::dnd::formats::{self, EFFECT_COPY, EFFECT_LINK, EFFECT_MOVE, EFFECT_NONE, MK_LBUTTON};
use tsubame_win::dnd::{self, DropEvents, DropInfo};
use tsubame_win::fileops::deletecheck::{self, Verdict};
use tsubame_win::fileops::{self, Choice, FileOpQueue, FsBackend, Operation};
use tsubame_win::shell::{MenuOpts, MenuResult, Payload, Reply, Request};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::HSTRING;

/// The Recycle Bin checks finished (posted by their thread).
pub const WM_APP_SHELLOPS: u32 = WM_APP + 9;
/// Drains file-op events; exists only while a job is in flight.
pub const OPS_TIMER: usize = 7;
const OPS_MS: u32 = 100;
/// Virtual files (from a zip folder, a mail) staged for one paste, in all.
pub const STAGE_CAP: u64 = 512 << 20;
/// Items one copy/cut/drag puts on the clipboard.
const MAX_ITEMS: usize = formats::MAX_PATHS;

/// Shell verbs the app runs itself instead of the shell.
const INTERCEPT: [&str; 5] = ["delete", "cut", "copy", "paste", "rename"];
const ITEM_EXTRAS: [&str; 3] = ["Copy path", "Copy name", "Open terminal here"];
// the shell leaves Paste out of a background menu without a view; ours is on top
const BG_EXTRAS: [&str; 5] = ["Paste", "New folder", "New text file", "Copy path", "Open terminal here"];

/// What view.rs reports to this module each frame (DIPs).
#[derive(Default)]
pub struct ViewHooks {
    /// rows / cells area of the active tab
    pub list: Rect,
    /// the tab strip in the title bar and its overflow scroll
    pub strip: Rect,
    pub strip_scroll: f32,
    /// right click on empty space: the folder's background menu
    pub bg_menu: Option<Pt>,
    /// last file-operation / clipboard message, in the status bar
    pub status: String,
}

#[derive(Default)]
pub struct ShellOps {
    hwnd: isize,
    queue: Option<(FileOpQueue, Receiver<fileops::Event>)>,
    jobs: HashMap<u64, Job>,
    timer: bool,
    /// a conflict or delete dialog is up (the timer must not open another)
    asking: bool,
    /// the clipboard job on a worker (one at a time keeps them in order)
    clip_busy: Option<(u64, ClipCtx)>,
    clip_wait: VecDeque<(ClipJob, ClipCtx)>,
    menu: Option<MenuCtx>,
    press: Option<Press>,
    staged: u32,
    /// the drop handler is registered (revoked on WM_DESTROY)
    registered: bool,
}

struct Job {
    verb: &'static str,
    /// folder that receives the items (selected after the refresh)
    folder: Option<String>,
    select: Vec<String>,
    /// conflicts answer "keep both" without asking (a copy into the same folder)
    keep_both: bool,
    staging: Option<PathBuf>,
    errors: usize,
    last_error: String,
}

#[derive(Clone, Debug)]
enum ClipCtx {
    Set(String),
    Paste { folder: String },
    Links { folder: String },
}

struct MenuCtx {
    id: u64,
    paths: Vec<String>,
    folder: String,
    background: bool,
}

/// A left press that may become a drag.
struct Press {
    px: (i32, i32),
    pos: Pt,
    /// the kit has not seen the press yet (it was on a selected row, which a
    /// press would collapse to one item)
    deferred: bool,
}

// ---- setup -------------------------------------------------------------------

/// The UI thread becomes an OLE STA and the window a drop target. Call once,
/// right after the window exists and before anything else uses COM on this
/// thread.
pub fn init(hwnd: HWND) -> bool {
    if dnd::ole_init().is_err() {
        return false;
    }
    dnd::register_target(hwnd, Box::new(Drops { hwnd: hwnd.0 as isize })).is_ok()
}

/// After `App` exists: remember the window for posts and timers.
pub fn attach(a: &mut App, hwnd: HWND, registered: bool) {
    a.ops.hwnd = hwnd.0 as isize;
    a.ops.registered = registered;
}

/// WM_DESTROY: no more drops; pending clipboard data is already flushed.
pub fn destroy(hwnd: HWND) {
    if with(|a| std::mem::take(&mut a.ops.registered)).unwrap_or(false) {
        dnd::revoke_target(hwnd);
    }
}

fn hwnd_of(a: &App) -> HWND {
    HWND(a.ops.hwnd as *mut _)
}

fn status(a: &mut App, s: impl Into<String>) {
    a.view.hooks.status = s.into();
}

fn active_folder(a: &App) -> Option<String> {
    let t = a.view.tab();
    (!t.synthetic && formats::validate(Path::new(&t.path)).is_ok()).then(|| t.path.clone())
}

fn name_of(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "item" } else { "items" }
}

// ---- file-op queue -----------------------------------------------------------

fn submit(a: &mut App, ops: Vec<Operation>, job: Job) {
    if ops.is_empty() {
        status(a, "Nothing to do");
        return;
    }
    if a.ops.queue.is_none() {
        match FileOpQueue::new(FsBackend) {
            Ok(q) => a.ops.queue = Some(q),
            Err(e) => {
                status(a, format!("File operations unavailable: {e}"));
                return;
            }
        }
    }
    let n = ops.len();
    let verb = job.verb;
    let Some((q, _)) = &a.ops.queue else { return };
    match q.submit(ops) {
        Ok(t) => {
            a.ops.jobs.insert(t.id, job);
            status(a, format!("{verb} {n} {}...", plural(n)));
        }
        Err(e) => status(a, format!("{verb} failed: {e}")),
    }
    arm_timer(a);
}

fn undo_redo(a: &mut App, redo: bool) {
    let Some((q, _)) = &a.ops.queue else {
        status(a, "Nothing to undo");
        return;
    };
    let r = if redo { q.redo() } else { q.undo() };
    let verb = if redo { "Redoing" } else { "Undoing" };
    match r {
        Ok(t) => {
            a.ops.jobs.insert(t.id, Job::new(verb, None));
            status(a, format!("{verb}..."));
        }
        Err(e) => status(a, format!("{verb} failed: {e}")),
    }
    arm_timer(a);
}

impl Job {
    fn new(verb: &'static str, folder: Option<String>) -> Job {
        Job { verb, folder, select: Vec::new(), keep_both: false, staging: None, errors: 0, last_error: String::new() }
    }
}

/// The event timer runs only while jobs are in flight (idle: no timer).
fn arm_timer(a: &mut App) {
    let busy = !a.ops.jobs.is_empty();
    let h = hwnd_of(a);
    if busy && !a.ops.timer {
        unsafe { SetTimer(Some(h), OPS_TIMER, OPS_MS, None) };
        a.ops.timer = true;
    } else if !busy && a.ops.timer {
        let _ = unsafe { KillTimer(Some(h), OPS_TIMER) };
        a.ops.timer = false;
    }
}

/// OPS_TIMER: file-op events. Conflicts ask with a dialog (outside `with`).
pub fn on_timer(hwnd: HWND) {
    if with(|a| a.ops.asking).unwrap_or(true) {
        return;
    }
    let mut repaint = false;
    loop {
        let ev = with(|a| a.ops.queue.as_ref().and_then(|q| q.1.try_recv().ok())).flatten();
        let Some(ev) = ev else { break };
        repaint = true;
        match ev {
            fileops::Event::Conflict { id, target, reply } => {
                let auto = with(|a| a.ops.jobs.get(&id).is_some_and(|j| j.keep_both)).unwrap_or(false);
                if auto {
                    reply.answer(Choice::KeepBoth, true);
                    continue;
                }
                with(|a| a.ops.asking = true);
                let c = ask_conflict(hwnd, &target);
                with(|a| a.ops.asking = false);
                reply.answer(c, true);
            }
            other => {
                with(|a| on_event(a, other));
            }
        }
    }
    with(arm_timer);
    if repaint {
        invalidate(hwnd);
    }
}

fn on_event(a: &mut App, ev: fileops::Event) {
    match ev {
        fileops::Event::Started(_) | fileops::Event::Conflict { .. } => {}
        fileops::Event::Progress { id, totals, .. } => {
            let verb = a.ops.jobs.get(&id).map(|j| j.verb).unwrap_or("Working");
            let pct = totals.bytes_done.saturating_mul(100).checked_div(totals.bytes_total).unwrap_or(0);
            status(a, format!("{verb}: {} of {} files, {pct}%", totals.files_done, totals.files_total.max(totals.files_done)));
        }
        fileops::Event::Error { id, message, item } => {
            let what = match &item {
                Operation::Copy { source, .. } | Operation::Move { source, .. } | Operation::Rename { source, .. } => name_of(&source.to_string_lossy()),
                Operation::Delete { path, .. } | Operation::NewFolder(path) | Operation::NewFile(path) => name_of(&path.to_string_lossy()),
                _ => String::new(),
            };
            if let Some(j) = a.ops.jobs.get_mut(&id) {
                j.errors += 1;
                j.last_error = format!("{what}: {message}");
            }
            status(a, format!("Error: {what}: {message}"));
        }
        fileops::Event::Finished { id, cancelled, totals } => {
            let Some(job) = a.ops.jobs.remove(&id) else { return };
            if let Some(dir) = job.staging.clone() {
                // staged virtual files: whatever was not moved out goes
                let _ = std::thread::Builder::new().name("tsubame-unstage".into()).spawn(move || {
                    let _ = std::fs::remove_dir_all(dir);
                });
            }
            let msg = if cancelled {
                format!("{} cancelled", job.verb)
            } else if job.errors > 0 {
                format!("{} done, {} error(s); last: {}", job.verb, job.errors, job.last_error)
            } else {
                format!("{} done ({} files)", job.verb, totals.files_done.max(1))
            };
            status(a, msg);
            refresh_active(a, &job);
        }
    }
}

/// Re-lists the active tab after a change (the watcher is not wired into the
/// tabs yet), keeping its scroll; selects what a job put into this folder.
fn refresh_active(a: &mut App, job: &Job) {
    if a.view.tabs.is_empty() || a.view.tab().synthetic {
        return;
    }
    let t = a.view.tab();
    let here = job.folder.as_deref().is_some_and(|f| formats::same_path(Path::new(f), Path::new(&t.path)));
    let first = if a.view.mode.is_grid() { 0 } else { t.list.row_at_offset(0.0) };
    let selected: Vec<String> = if here && !job.select.is_empty() { job.select.clone() } else { t.selected_paths(10_000).iter().map(|p| name_of(p)).collect() };
    a.view.refresh();
    a.view.tab_mut().restore = Some(crate::tab::Restore { scroll_first_row: first, selected });
}

/// Yes = replace, No = keep both, Cancel = skip; for the rest of the job too.
fn ask_conflict(hwnd: HWND, target: &Path) -> Choice {
    let text = format!(
        "\"{}\" already exists in\n{}\n\nYes: replace it (the old one goes to the Recycle Bin)\nNo: keep both (the new one gets a number)\nCancel: skip it\n\nThe answer applies to the other conflicts of this operation too.",
        short(&target.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(), 80),
        short(&target.parent().map(|p| p.to_string_lossy()).unwrap_or_default(), 120)
    );
    match unsafe { MessageBoxW(Some(hwnd), &HSTRING::from(text), &HSTRING::from("Tsubame: item already exists"), MB_YESNOCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2) } {
        IDYES => Choice::Replace,
        IDNO => Choice::KeepBoth,
        _ => Choice::Skip,
    }
}

// ---- delete ------------------------------------------------------------------

type DeleteResult = (Vec<(PathBuf, Verdict)>, bool);
static DELETES: Mutex<Vec<DeleteResult>> = Mutex::new(Vec::new());

/// Delete / Shift+Delete / the menu's Delete: the bin checks run on a thread,
/// WM_APP_SHELLOPS brings the verdicts back.
fn start_delete(a: &mut App, paths: Vec<String>, permanent: bool) {
    if paths.is_empty() {
        return;
    }
    let h = a.ops.hwnd;
    status(a, format!("Checking {} {}...", paths.len(), plural(paths.len())));
    let _ = std::thread::Builder::new().name("tsubame-deletecheck".into()).spawn(move || {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let r = deletecheck::check(&paths);
        if let Ok(mut g) = DELETES.lock() {
            g.push((r, permanent));
        }
        let _ = unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_APP_SHELLOPS, WPARAM(0), LPARAM(0)) };
    });
}

/// WM_APP_SHELLOPS: confirm permanent deletes, then queue them.
pub fn on_app(hwnd: HWND) {
    let done: Vec<DeleteResult> = DELETES.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default();
    for (verdicts, shift) in done {
        let mut recycle = Vec::new();
        let mut hazard: Vec<(PathBuf, String)> = Vec::new();
        for (p, v) in verdicts {
            match v {
                Verdict::Recycle => recycle.push(p),
                Verdict::Permanent(why) => hazard.push((p, why)),
                Verdict::Missing => {}
            }
        }
        let mut ops: Vec<Operation> = Vec::new();
        let folder = recycle.first().or(hazard.first().map(|h| &h.0)).and_then(|p| p.parent()).map(|p| p.to_string_lossy().into_owned());
        if shift {
            // Shift+Delete: everything permanent, asked once
            let all: Vec<PathBuf> = recycle.into_iter().chain(hazard.into_iter().map(|h| h.0)).collect();
            if all.is_empty() {
                continue;
            }
            let text = format!("Permanently delete {}?\n\n{}They will not go to the Recycle Bin.", count_text(&all), list_names(all.iter().map(|p| (p, None))));
            if !confirm(hwnd, &text) {
                continue;
            }
            ops.extend(all.into_iter().map(|path| Operation::Delete { path, permanent: true }));
        } else {
            if !hazard.is_empty() {
                let text = format!(
                    "{} can't go to the Recycle Bin:\n\n{}\nYes: delete {} permanently{}\nNo: {}\nCancel: delete nothing",
                    count_text_n(hazard.len()),
                    list_names(hazard.iter().map(|(p, w)| (p, Some(w.as_str())))),
                    if hazard.len() == 1 { "it" } else { "them" },
                    if recycle.is_empty() { String::new() } else { format!(" and recycle the other {}", recycle.len()) },
                    if recycle.is_empty() { format!("keep {}", if hazard.len() == 1 { "it" } else { "them" }) } else { format!("only recycle the other {}", recycle.len()) },
                );
                with(|a| a.ops.asking = true);
                let r = unsafe { MessageBoxW(Some(hwnd), &HSTRING::from(text), &HSTRING::from("Tsubame: delete permanently?"), MB_YESNOCANCEL | MB_ICONWARNING | MB_DEFBUTTON2) };
                with(|a| a.ops.asking = false);
                match r {
                    IDYES => ops.extend(hazard.into_iter().map(|(path, _)| Operation::Delete { path, permanent: true })),
                    IDNO => {}
                    _ => continue,
                }
            }
            ops.extend(recycle.into_iter().map(|path| Operation::Delete { path, permanent: false }));
        }
        with(|a| submit(a, ops, Job::new("Deleting", folder)));
    }
    invalidate(hwnd);
}

fn count_text(v: &[PathBuf]) -> String {
    if v.len() == 1 { format!("\"{}\"", short(&name_of(&v[0].to_string_lossy()), 80)) } else { format!("these {} items", v.len()) }
}
fn count_text_n(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

/// Up to 6 names (with the reason), then "and N more".
fn list_names<'a>(it: impl Iterator<Item = (&'a PathBuf, Option<&'a str>)>) -> String {
    let mut s = String::new();
    let mut n = 0;
    for (p, why) in it {
        n += 1;
        if n <= 6 {
            s.push_str(&format!("  {}{}\n", short(&name_of(&p.to_string_lossy()), 80), why.map(|w| format!(": {w}")).unwrap_or_default()));
        }
    }
    if n > 6 {
        s.push_str(&format!("  and {} more\n", n - 6));
    }
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// `s` cut in the middle to at most `max` chars ("abc…xyz"), so a dialog
/// stays readable with a 250-char name.
fn short(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head = (max - 1) / 2;
    let tail = max - 1 - head;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(n - tail));
    out
}

fn confirm(hwnd: HWND, text: &str) -> bool {
    with(|a| a.ops.asking = true);
    let r = unsafe { MessageBoxW(Some(hwnd), &HSTRING::from(text), &HSTRING::from("Tsubame: delete permanently?"), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) };
    with(|a| a.ops.asking = false);
    r == IDYES
}

// ---- clipboard -----------------------------------------------------------------

fn clip(a: &mut App, job: ClipJob, ctx: ClipCtx) {
    if a.ops.clip_busy.is_some() {
        a.ops.clip_wait.push_back((job, ctx));
        return;
    }
    let t = a.pool.submit(Request::Clip(job));
    a.ops.clip_busy = Some((t.id, ctx));
}

fn copy_files(a: &mut App, paths: Vec<String>, cut: bool) {
    if paths.is_empty() {
        return;
    }
    let n = paths.len();
    let msg = format!("{} {n} {} to the clipboard", if cut { "Cut" } else { "Copied" }, plural(n));
    clip(a, ClipJob::SetFiles { paths: paths.into_iter().map(PathBuf::from).collect(), cut }, ClipCtx::Set(msg));
}

fn copy_text(a: &mut App, text: String, what: &str) {
    if text.is_empty() {
        return;
    }
    clip(a, ClipJob::SetText(text), ClipCtx::Set(format!("Copied {what}")));
}

/// Explorer's "Copy as path": each path quoted, one per line.
pub fn path_text(paths: &[String]) -> String {
    paths.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join("\r\n")
}

fn paste(a: &mut App, folder: String) {
    a.ops.staged += 1;
    let staging = std::env::temp_dir().join("Tsubame").join(format!("paste-{}-{}", std::process::id(), a.ops.staged));
    status(a, "Reading the clipboard...");
    clip(a, ClipJob::Read { staging, cap: STAGE_CAP, consume_cut: true }, ClipCtx::Paste { folder });
}

fn on_clip(a: &mut App, id: u64, reply: ClipReply) {
    let ctx = match &a.ops.clip_busy {
        Some((cid, _)) if *cid == id => a.ops.clip_busy.take().map(|c| c.1),
        _ => None,
    };
    if let Some(ctx) = ctx {
        match (ctx, reply) {
            (ClipCtx::Set(msg), ClipReply::Set(Ok(_))) => status(a, msg),
            (ClipCtx::Set(_), ClipReply::Set(Err(e))) => status(a, format!("Clipboard busy: {e}")),
            (ClipCtx::Paste { folder }, ClipReply::Read(Ok(r))) => paste_read(a, folder, r),
            (ClipCtx::Paste { .. }, ClipReply::Read(Err(e))) => status(a, format!("Can't read the clipboard: {e}")),
            (ClipCtx::Links { .. }, ClipReply::Links(Ok(n))) => {
                status(a, format!("Made {n} {}", if n == 1 { "shortcut" } else { "shortcuts" }));
                let job = Job::new("Shortcuts", None);
                refresh_active(a, &job);
            }
            (ClipCtx::Links { folder }, ClipReply::Links(Err(e))) => status(a, format!("Can't make shortcuts in {folder}: {e}")),
            _ => {}
        }
    }
    if let Some((job, ctx)) = a.ops.clip_wait.pop_front() {
        clip(a, job, ctx);
    }
}

fn paste_read(a: &mut App, folder: String, r: ClipRead) {
    let target = PathBuf::from(&folder);
    let (sources, effect, staging) = if !r.staged.is_empty() { (r.staged.clone(), EFFECT_MOVE, r.staging.clone()) } else { (r.info.paths.clone(), r.effect, None) };
    if sources.is_empty() {
        if let Some(s) = &r.staging {
            let _ = std::fs::remove_dir_all(s);
        }
        let skipped = r.skipped + r.info.rejected;
        status(a, if skipped > 0 { format!("Nothing pasted: {skipped} item(s) refused (unsafe name, device path or over {} MB)", STAGE_CAP >> 20) } else { "Nothing to paste".into() });
        return;
    }
    let ops = ole_glue::ops(&sources, &target, effect);
    let mut job = Job::new(if effect == EFFECT_MOVE && staging.is_none() { "Moving" } else { "Copying" }, Some(folder));
    job.select = sources.iter().filter_map(|s| s.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
    job.keep_both = sources.iter().any(|s| s.parent().is_some_and(|p| formats::same_path(p, &target)));
    job.staging = staging;
    submit(a, ops, job);
    let refused = r.skipped + r.info.rejected;
    if refused > 0 {
        let s = format!("{} ({refused} refused)", a.view.hooks.status);
        status(a, s);
    }
}

// ---- keys --------------------------------------------------------------------

/// WM_KEYDOWN before the kit: the list's file shortcuts. True = handled.
pub fn on_key(hwnd: HWND, vk: u16, m: Mods) -> bool {
    let k = VIRTUAL_KEY(vk);
    let ch = |c: u8| vk == c as u16;
    let r = with(|a| {
        if a.view.tabs.is_empty() || !a.ui.has_focus(crate::view::ID_LIST) || a.view.editing {
            return None;
        }
        let sel = || a.view.tab().selected_paths(MAX_ITEMS);
        let (c, s, al) = (m.ctrl, m.shift, m.alt);
        if al {
            return None;
        }
        if c && !s && (ch(b'C') || k == VK_INSERT) {
            let p = sel();
            copy_files(a, p, false);
        } else if c && !s && ch(b'X') {
            let p = sel();
            copy_files(a, p, true);
        } else if (c && !s && ch(b'V')) || (!c && s && k == VK_INSERT) {
            let Some(f) = active_folder(a) else { return Some(false) };
            paste(a, f);
        } else if c && s && ch(b'C') {
            let t = path_text(&sel());
            copy_text(a, t, "path");
        } else if !c && k == VK_DELETE {
            let p = sel();
            start_delete(a, p, s);
        } else if c && !s && ch(b'Z') {
            undo_redo(a, false);
        } else if c && !s && ch(b'Y') {
            undo_redo(a, true);
        } else if c && s && ch(b'N') {
            new_item(a, false);
        } else {
            return None;
        }
        Some(true)
    })
    .flatten();
    if r.is_some() {
        invalidate(hwnd);
    }
    r.is_some()
}

fn new_item(a: &mut App, file: bool) {
    let Some(folder) = active_folder(a) else { return };
    let t = a.view.tab();
    let mut taken = HashSet::new();
    for id in 0..t.store.id_bound() as u32 {
        if let Some(n) = t.store.name(id) {
            taken.insert(String::from_utf16_lossy(n).to_lowercase());
        }
    }
    let name = if file { ole_glue::free_name("New Text Document", ".txt", &|n| taken.contains(n)) } else { ole_glue::free_name("New folder", "", &|n| taken.contains(n)) };
    let path = Path::new(&folder).join(&name);
    let op = if file { Operation::NewFile(path) } else { Operation::NewFolder(path) };
    let mut job = Job::new(if file { "New file" } else { "New folder" }, Some(folder));
    job.select = vec![name];
    submit(a, vec![op], job);
}

fn open_terminal(folder: String) {
    let _ = std::thread::Builder::new().name("tsubame-terminal".into()).spawn(move || {
        use std::os::windows::process::CommandExt;
        // Windows Terminal reads ';' as a command separator
        let wt = std::process::Command::new("wt.exe").arg("-d").arg(folder.replace(';', "\\;")).spawn();
        if wt.is_err() {
            const CREATE_NEW_CONSOLE: u32 = 0x10;
            let _ = std::process::Command::new("cmd.exe").current_dir(&folder).creation_flags(CREATE_NEW_CONSOLE).spawn();
        }
    });
}

// ---- context menu ------------------------------------------------------------

pub fn item_opts() -> MenuOpts {
    MenuOpts { background: false, extras: ITEM_EXTRAS.iter().map(|s| s.to_string()).collect(), intercept: INTERCEPT.iter().map(|s| s.to_string()).collect() }
}

fn bg_opts() -> MenuOpts {
    MenuOpts { background: true, extras: BG_EXTRAS.iter().map(|s| s.to_string()).collect(), intercept: INTERCEPT.iter().map(|s| s.to_string()).collect() }
}

/// The item menu for `paths` (screen px), with our entries.
pub fn open_item_menu(a: &mut App, paths: Vec<String>, sx: i32, sy: i32, extended: bool) {
    let folder = a.view.tab().path.clone();
    a.shell.open_menu(&mut a.pool, paths.clone(), sx, sy, extended, None, item_opts());
    if let Some(id) = a.shell.menu_pending {
        a.ops.menu = Some(MenuCtx { id, paths, folder, background: false });
    }
}

/// After a frame: a right click on empty space opens the folder's menu.
pub fn after_frame(hwnd: HWND) {
    let extended = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let want = with(|a| {
        let pt = a.view.hooks.bg_menu.take()?;
        let folder = active_folder(a)?;
        let s = a.gfx.dpi / 96.0;
        Some((folder, (pt.x * s) as i32, (pt.y * s) as i32))
    })
    .flatten();
    let Some((folder, x, y)) = want else { return };
    let (sx, sy) = crate::screen_pt(hwnd, x, y);
    with(|a| {
        a.shell.open_menu(&mut a.pool, vec![folder.clone()], sx, sy, extended, None, bg_opts());
        if let Some(id) = a.shell.menu_pending {
            a.ops.menu = Some(MenuCtx { id, paths: vec![folder.clone()], folder, background: true });
        }
    });
    invalidate(hwnd);
}

fn on_menu(a: &mut App, m: &MenuResult, ctx: MenuCtx) {
    if let Some(i) = m.extra {
        let extras: &[&str] = if ctx.background { &BG_EXTRAS } else { &ITEM_EXTRAS };
        match extras.get(i).copied() {
            Some("Paste") => paste(a, ctx.folder),
            Some("New folder") => new_item(a, false),
            Some("New text file") => new_item(a, true),
            Some("Copy path") => copy_text(a, path_text(&ctx.paths), "path"),
            Some("Copy name") => copy_text(a, ctx.paths.iter().map(|p| name_of(p)).collect::<Vec<_>>().join("\r\n"), "name"),
            Some("Open terminal here") => {
                // a single folder item opens there, anything else in this folder
                let one_dir = (!ctx.background && ctx.paths.len() == 1 && Path::new(&ctx.paths[0]).is_dir()).then(|| ctx.paths[0].clone());
                open_terminal(one_dir.unwrap_or(ctx.folder));
            }
            _ => {}
        }
        return;
    }
    let Some(verb) = m.intercepted.as_deref() else { return };
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    match verb {
        "delete" if !ctx.background => start_delete(a, ctx.paths, shift),
        "cut" | "copy" if !ctx.background => copy_files(a, ctx.paths, verb == "cut"),
        "paste" => {
            // on a folder item: into that folder (like Explorer)
            let into = if !ctx.background && ctx.paths.len() == 1 && Path::new(&ctx.paths[0]).is_dir() { ctx.paths[0].clone() } else { ctx.folder };
            paste(a, into);
        }
        "rename" => status(a, "Rename from this menu is not available yet"),
        _ => {}
    }
}

/// A pool reply, before ShellUi sees it: clipboard replies are ours; a menu
/// reply runs our entries / intercepted verbs and still goes on for stats.
pub fn on_reply(a: &mut App, r: Reply) -> Option<Reply> {
    match r.payload {
        Payload::Clip(c) if a.ops.clip_busy.as_ref().is_some_and(|b| b.0 == r.id) => {
            on_clip(a, r.id, c);
            None
        }
        Payload::Menu(ref m) => {
            if let Some(ctx) = a.ops.menu.take_if(|c| c.id == r.id) {
                on_menu(a, m, ctx);
            }
            Some(r)
        }
        _ => Some(r),
    }
}

// ---- drag out ------------------------------------------------------------------

/// The list row / grid cell under `pos` (DIPs), None off the rows.
fn row_at(a: &App, pos: Pt) -> Option<usize> {
    let l = a.view.hooks.list;
    if !l.contains_pt(pos) || pos.x >= l.r() - a.ui.theme.layout_scrollbar_width {
        return None;
    }
    let t = a.view.tab();
    let n = t.shown();
    if a.view.mode.is_grid() {
        t.grid.cell_at(pos.x - l.x, pos.y - l.y, n)
    } else {
        Some(t.list.row_at_offset(pos.y - l.y)).filter(|&r| r < n)
    }
}

/// WM_LBUTTONDOWN. A press on a selected row is held back (true: don't pass
/// it on) until it is a click or a drag; any other press on a row goes to the
/// kit at once and may still become a drag.
pub fn mouse_down(lp: LPARAM) -> bool {
    let pos = crate::dip_pt(lp);
    let m = crate::mods();
    let px = (crate::lo(lp), crate::hi(lp));
    with(|a| {
        a.ops.press = None;
        if m.ctrl || m.shift || m.alt || a.view.tabs.is_empty() || a.view.tab().synthetic {
            return false;
        }
        let Some(row) = row_at(a, pos) else { return false };
        let t = a.view.tab();
        let selected = t.store.at(row).is_some_and(|id| t.sel.contains(id));
        a.ops.press = Some(Press { px, pos, deferred: selected });
        selected
    })
    .unwrap_or(false)
}

/// WM_LBUTTONUP: a held-back press was a click after all.
pub fn mouse_up(hwnd: HWND) {
    if let Some(p) = with(|a| a.ops.press.take()).flatten()
        && p.deferred
    {
        push(hwnd, Event::MouseDown { pos: p.pos, button: Button::Left, mods: Mods::NONE, double: false });
    }
}

/// WM_MOUSEMOVE: past the drag threshold the press becomes an OLE drag.
/// True: the move was used (a drag ran).
pub fn mouse_move(hwnd: HWND, lp: LPARAM, wp: WPARAM) -> bool {
    let (x, y) = (crate::lo(lp), crate::hi(lp));
    let left = wp.0 as u32 & MK_LBUTTON != 0;
    let start = with(|a| {
        let p = a.ops.press.as_ref()?;
        if !left {
            a.ops.press = None;
            return None;
        }
        let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
        if (x - p.px.0).abs() <= cx && (y - p.px.1).abs() <= cy {
            return None;
        }
        a.ops.press.take()
    })
    .flatten();
    let Some(p) = start else { return false };
    if !p.deferred {
        // the kit saw the press: end its gesture before OLE takes the mouse
        push(hwnd, Event::MouseUp { pos: p.pos, button: Button::Left, mods: Mods::NONE });
    }
    let paths: Vec<PathBuf> = with(|a| a.view.tab().selected_paths(MAX_ITEMS)).unwrap_or_default().into_iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        return true;
    }
    // modal: OLE runs its own loop (paints and timers still dispatch)
    let r = dnd::drag_files(&paths, EFFECT_COPY | EFFECT_MOVE | EFFECT_LINK, MK_LBUTTON);
    with(|a| match r {
        Ok(o) if o.dropped => {
            // a target that moved the files reports NONE (optimized move);
            // either way this folder may have changed
            let job = Job::new("Drag", None);
            refresh_active(a, &job);
        }
        Ok(_) => {}
        Err(e) => status(a, format!("Drag failed: {e}")),
    });
    invalidate(hwnd);
    true
}

// ---- drop in -------------------------------------------------------------------

/// The drop target: a folder row, a tab, or the shown folder.
struct Drops {
    hwnd: isize,
}

impl Drops {
    fn h(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    /// (folder under the screen point, client DIP point)
    fn target(&self, pt: (i32, i32)) -> Option<(String, Pt)> {
        let mut p = POINT { x: pt.0, y: pt.1 };
        let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(self.h(), &mut p) };
        let s = crate::dpi_scale();
        let pos = Pt::new(p.x as f32 / s, p.y as f32 / s);
        with(|a| {
            if a.view.tabs.is_empty() {
                return None;
            }
            let h = &a.view.hooks;
            let th = a.ui.theme.height_tab;
            let strip = Rect::new(h.strip.x, h.strip.b() - th, h.strip.w, th);
            let folder = if strip.contains_pt(pos) {
                // the same layout as tsubame-ui's tab strip (28 + 4 DIPs for "+")
                let n = a.view.tabs.items.len();
                let w = ((strip.w - 32.0).max(0.0) / n as f32).clamp(a.ui.theme.layout_tab_min_width, a.ui.theme.layout_tab_width);
                let i = ((pos.x - (strip.x - h.strip_scroll)) / w).floor();
                let tab = (i >= 0.0).then(|| a.view.tabs.items.get(i as usize)).flatten();
                match tab {
                    Some(t) if !t.synthetic => t.path.clone(),
                    Some(_) => return None,
                    None => a.view.tab().path.clone(),
                }
            } else if let Some(row) = row_at(a, pos) {
                match a.view.tab().entry_at(row) {
                    Some((p, true)) => p,
                    _ => active_folder(a)?,
                }
            } else {
                active_folder(a)?
            };
            formats::validate(Path::new(&folder)).is_ok().then_some((folder, pos))
        })
        .flatten()
    }

    fn effect(&self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> (u32, Option<String>) {
        let Some((folder, pos)) = self.target(pt) else { return (EFFECT_NONE, None) };
        // hover feedback: the kit highlights the row under the pointer
        push(self.h(), Event::MouseMove { pos });
        (ole_glue::choose(&info.paths, Path::new(&folder), keys, allowed), Some(folder))
    }
}

impl DropEvents for Drops {
    fn enter(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32 {
        self.effect(info, pt, keys, allowed).0
    }
    fn over(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32 {
        self.effect(info, pt, keys, allowed).0
    }
    fn leave(&mut self) {
        push(self.h(), Event::MouseLeave);
    }
    fn drop(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32 {
        let (e, folder) = self.effect(info, pt, keys, allowed);
        push(self.h(), Event::MouseLeave);
        let Some(folder) = folder else { return EFFECT_NONE };
        if e == EFFECT_NONE {
            return e;
        }
        let rejected = info.rejected;
        with(|a| {
            if e == EFFECT_LINK {
                clip(a, ClipJob::Links { targets: info.paths.clone(), folder: PathBuf::from(&folder) }, ClipCtx::Links { folder });
                return;
            }
            let target = PathBuf::from(&folder);
            let ops = ole_glue::ops(&info.paths, &target, e);
            let mut job = Job::new(if e == EFFECT_MOVE { "Moving" } else { "Copying" }, Some(folder));
            job.select = info.paths.iter().filter_map(|s| s.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
            job.keep_both = info.paths.iter().any(|s| s.parent().is_some_and(|p| formats::same_path(p, &target)));
            submit(a, ops, job);
            if rejected > 0 {
                let s = format!("{} ({rejected} unsafe path(s) refused)", a.view.hooks.status);
                status(a, s);
            }
        });
        invalidate(self.h());
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_as_path_quotes_each_line() {
        assert_eq!(path_text(&[r"C:\a b\x.txt".into(), r"D:\y".into()]), "\"C:\\a b\\x.txt\"\r\n\"D:\\y\"");
        assert_eq!(path_text(&[]), "");
    }

    #[test]
    fn long_names_are_cut_in_the_middle() {
        assert_eq!(short("abc", 5), "abc");
        let long = "a".repeat(100) + "Z";
        let c = short(&long, 21);
        assert_eq!(c.chars().count(), 21);
        assert!(c.starts_with("aaaa") && c.ends_with('Z') && c.contains('…'));
    }

    #[test]
    fn delete_lists_stay_short() {
        let v: Vec<PathBuf> = (0..9).map(|i| PathBuf::from(format!(r"C:\f{i}"))).collect();
        let s = list_names(v.iter().map(|p| (p, Some("too big"))));
        assert!(s.contains("f0: too big") && s.contains("f5") && !s.contains("f6") && s.contains("and 3 more"), "{s}");
    }
}
