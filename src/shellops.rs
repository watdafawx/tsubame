//! Shell integration glue (W1, unified in INT1): the Explorer context menu's
//! app entries and intercepted verbs, OLE drag and drop in and out, the
//! clipboard (cut, copy, paste, copy path), and Delete with the Recycle Bin
//! hazard checks. Every file change is submitted to the view's ONE file-op
//! owner (`crate::ops::Ops`, `a.view.ops`): one queue, one undo journal, one
//! status message. See `docs/shell-integration.md` and `docs/integration.md`.
//!
//! Threading: the UI thread never makes a blocking shell call. Menus and
//! clipboard jobs run on the shell pool's STA workers (`Request::Menu`,
//! `Request::Clip`); file operations on the queue's worker; the Recycle Bin
//! checks (they size folders) on a short-lived thread. OLE drag and drop
//! itself has to run on the UI thread (`DoDragDrop`, `IDropTarget`), which
//! is an OLE STA for that.
//!
//! Drag and drop is ONE path: a drag that starts on our rows is always an OLE
//! `DoDragDrop`; our `IDropTarget` takes drops from Explorer and from
//! ourselves alike and asks the view where the pointer is (a folder row, a
//! pane, a tab, a sidebar folder, a gap between pinned rows). The effect comes
//! from `dnd::resolve` (same drive moves, other drive copies, Ctrl copy, Shift
//! move, Alt or Ctrl+Shift shortcut).
//!
//! Nothing here holds `crate::with` across a call that can re-enter the
//! window procedure (`DoDragDrop`, `MessageBoxW`, `UpdateWindow`).

use crate::ole_glue;
use crate::ops::{Job, plural};
use crate::view::DropDest;
use crate::{App, invalidate, push, with};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tsubame_ui::{Button, Event, Mods, Pt};
use tsubame_win::dnd::clipboard::{ClipJob, ClipRead, ClipReply};
use tsubame_win::dnd::formats::{self, EFFECT_COPY, EFFECT_LINK, EFFECT_MOVE, EFFECT_NONE, MK_ALT, MK_CONTROL, MK_LBUTTON, MK_SHIFT};
use tsubame_win::dnd::{self, DropEvents, DropInfo};
use tsubame_win::fileops::deletecheck::{self, Verdict};
use tsubame_win::fileops::{self, Choice, Operation};
use tsubame_win::shell::{MenuOpts, MenuResult, Payload, Reply, Request};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::HSTRING;

/// The Recycle Bin checks finished (posted by their thread).
pub const WM_APP_SHELLOPS: u32 = WM_APP + 9;
/// Virtual files (from a zip folder, a mail) staged for one paste, in all.
pub const STAGE_CAP: u64 = 512 << 20;
/// Items one copy/cut/drag puts on the clipboard.
pub const MAX_ITEMS: usize = formats::MAX_PATHS;

/// Shell verbs the app runs itself instead of the shell.
const INTERCEPT: [&str; 5] = ["delete", "cut", "copy", "paste", "rename"];
const ITEM_EXTRAS: [&str; 4] = ["Rename	F2", "Copy path", "Copy name", "Open terminal here"];
// the shell leaves Paste out of a background menu without a view; ours is on top
const BG_EXTRAS: [&str; 5] = ["Paste", "New folder", "New text file", "Copy path", "Open terminal here"];

#[derive(Default)]
pub struct ShellOps {
    hwnd: isize,
    /// the clipboard job on a worker (one at a time keeps them in order)
    clip_busy: Option<(u64, ClipCtx)>,
    clip_wait: VecDeque<(ClipJob, ClipCtx)>,
    menu: Option<MenuCtx>,
    staged: u32,
    /// the drop handler is registered (revoked on WM_DESTROY)
    registered: bool,
}

#[derive(Clone, Debug)]
enum ClipCtx {
    Set(String),
    Paste { folder: String },
    Links { folder: String, names: Vec<String> },
}

struct MenuCtx {
    id: u64,
    paths: Vec<String>,
    folder: String,
    background: bool,
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

/// After `App` exists: remember the window for posts.
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

fn status(a: &mut App, s: impl Into<String>) {
    a.view.ops.message = Some(s.into());
}

fn active_folder(a: &App) -> Option<String> {
    let t = a.view.tab();
    (!t.synthetic && formats::validate(Path::new(&t.path)).is_ok()).then(|| t.path.clone())
}

fn name_of(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}

fn names_of(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().filter_map(|s| s.file_name().map(|n| n.to_string_lossy().into_owned())).collect()
}

// ---- file-op events ------------------------------------------------------------

/// main.rs's OPS_TIMER (it exists only while `ops.busy()`): drains the queue's
/// events. Conflicts ask with a dialog, outside the app borrow.
pub fn on_ops_timer(hwnd: HWND) {
    if with(|a| a.view.ops.asking).unwrap_or(true) {
        return;
    }
    let mut any = false;
    loop {
        let Some(ev) = with(|a| a.view.ops.next_event()).flatten() else { break };
        any = true;
        match ev {
            fileops::Event::Conflict { id, target, reply } => {
                if with(|a| a.view.ops.keep_both(id)).unwrap_or(false) {
                    reply.answer(Choice::KeepBoth, true);
                    continue;
                }
                with(|a| a.view.ops.asking = true);
                let c = ask_conflict(hwnd, &target);
                with(|a| a.view.ops.asking = false);
                reply.answer(c, true);
            }
            other => {
                with(|a| a.view.ops.on_event(other));
            }
        }
    }
    if any {
        with(|a| a.view.after_ops());
        invalidate(hwnd);
    }
}

/// Yes = replace, No = keep both, Cancel = skip; for the rest of the job too.
fn ask_conflict(hwnd: HWND, target: &Path) -> Choice {
    let text = format!(
        "\"{}\" already exists in
{}

Yes: replace it (the old one goes to the Recycle Bin)
No: keep both (the new one gets a number)
Cancel: skip it

The answer applies to the other conflicts of this operation too.",
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
                with(|a| a.view.ops.asking = true);
                let r = unsafe { MessageBoxW(Some(hwnd), &HSTRING::from(text), &HSTRING::from("Tsubame: delete permanently?"), MB_YESNOCANCEL | MB_ICONWARNING | MB_DEFBUTTON2) };
                with(|a| a.view.ops.asking = false);
                match r {
                    IDYES => ops.extend(hazard.into_iter().map(|(path, _)| Operation::Delete { path, permanent: true })),
                    IDNO => {}
                    _ => continue,
                }
            }
            ops.extend(recycle.into_iter().map(|path| Operation::Delete { path, permanent: false }));
        }
        let mut job = Job::new("Deleting", "Deleted");
        job.folder = folder;
        with(|a| a.view.ops.submit(ops, job));
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
    with(|a| a.view.ops.asking = true);
    let r = unsafe { MessageBoxW(Some(hwnd), &HSTRING::from(text), &HSTRING::from("Tsubame: delete permanently?"), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) };
    with(|a| a.view.ops.asking = false);
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
            (ClipCtx::Links { folder, names }, ClipReply::Links(Ok(n))) => {
                status(a, format!("Made {n} {}", if n == 1 { "shortcut" } else { "shortcuts" }));
                let names = names.into_iter().map(|n| format!("{n}.lnk")).collect();
                a.view.ops.changed(&folder, names);
                a.view.after_ops();
            }
            (ClipCtx::Links { folder, .. }, ClipReply::Links(Err(e))) => status(a, format!("Can't make shortcuts in {folder}: {e}")),
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
    let refused = r.skipped + r.info.rejected;
    if effect == EFFECT_LINK {
        // a "paste shortcut" source: make links like a drop would
        drop_into(a, &sources, folder, EFFECT_LINK, refused);
        return;
    }
    let ops = ole_glue::ops(&sources, &target, effect);
    let mut job = if effect == EFFECT_MOVE && staging.is_none() { Job::new("Moving", "Moved") } else { Job::new("Copying", "Copied") }.into_folder(folder, names_of(&sources));
    job.keep_both = sources.iter().any(|s| s.parent().is_some_and(|p| formats::same_path(p, &target)));
    job.staging = staging;
    a.view.ops.submit(ops, job);
    if refused > 0
        && let Some(m) = a.view.ops.message.as_mut()
    {
        m.push_str(&format!(" ({refused} refused)"));
    }
}

// ---- keys --------------------------------------------------------------------

/// A file shortcut of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAct {
    Copy,
    Cut,
    Paste,
    CopyPath,
    Delete { permanent: bool },
    Undo,
    Redo,
    NewFolder,
}

/// The file shortcut `vk` with `m` stands for (Explorer's keys).
pub fn key_act(vk: u16, m: Mods) -> Option<KeyAct> {
    let k = VIRTUAL_KEY(vk);
    let ch = |c: u8| vk == c as u16;
    let (c, s) = (m.ctrl, m.shift);
    if m.alt {
        return None;
    }
    Some(if c && !s && (ch(b'C') || k == VK_INSERT) {
        KeyAct::Copy
    } else if c && !s && ch(b'X') {
        KeyAct::Cut
    } else if (c && !s && ch(b'V')) || (!c && s && k == VK_INSERT) {
        KeyAct::Paste
    } else if c && s && ch(b'C') {
        KeyAct::CopyPath
    } else if !c && k == VK_DELETE {
        KeyAct::Delete { permanent: s }
    } else if c && !s && ch(b'Z') {
        KeyAct::Undo
    } else if c && !s && ch(b'Y') {
        KeyAct::Redo
    } else if c && s && ch(b'N') {
        KeyAct::NewFolder
    } else {
        return None;
    })
}

/// WM_KEYDOWN before the kit: the file shortcuts of the ACTIVE pane's list
/// (live == active outside a paint, so `a.view.tab()` is that pane's tab).
/// True = handled.
pub fn on_key(hwnd: HWND, vk: u16, m: Mods) -> bool {
    let Some(act) = key_act(vk, m) else { return false };
    let r = with(|a| {
        if a.view.tabs.is_empty() || !a.view.list_focused(&a.ui) {
            return false;
        }
        let sel = |a: &App| a.view.tab().selected_paths(MAX_ITEMS);
        match act {
            KeyAct::Copy => copy_files(a, sel(a), false),
            KeyAct::Cut => copy_files(a, sel(a), true),
            KeyAct::Paste => match active_folder(a) {
                Some(f) => paste(a, f),
                None => return false,
            },
            KeyAct::CopyPath => {
                let t = path_text(&sel(a));
                copy_text(a, t, "path");
            }
            KeyAct::Delete { permanent } => start_delete(a, sel(a), permanent),
            KeyAct::Undo => {
                a.view.ops.undo();
            }
            KeyAct::Redo => {
                a.view.ops.redo();
            }
            KeyAct::NewFolder => new_item(a, false),
        }
        true
    })
    .unwrap_or(false);
    if r {
        invalidate(hwnd);
    }
    r
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
    let job = Job::new("Creating", "Created").into_folder(folder, vec![name]);
    a.view.ops.submit(vec![op], job);
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
        let pt = a.view.bg_menu.take()?;
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
            Some("Rename\tF2") => a.view.rename_request = true,
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
        "rename" if !ctx.background => a.view.rename_request = true,
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

// ---- drag out -------------------------------------------------------------------

/// After a frame: the view started a drag on its rows (`View::drag_out`). It
/// runs as an OLE drag, so the same code takes it whether it ends on our own
/// panes, the sidebar, a tab, Explorer or the desktop.
pub fn drag_out(hwnd: HWND) {
    let Some((paths, pos)) = with(|a| a.view.drag_out.take().map(|p| (p, a.ui.mouse()))).flatten() else { return };
    // the kit's press ends here: OLE has the mouse until the drop. Our capture
    // goes now, so no frame painted during the drag releases OLE's.
    push(hwnd, Event::MouseUp { pos, button: Button::Left, mods: Mods::NONE });
    if with(|a| std::mem::take(&mut a.captured)).unwrap_or(false) {
        let _ = unsafe { ReleaseCapture() };
    }
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    // modal: OLE runs its own loop (paints, timers and our IDropTarget still run)
    let r = dnd::drag_files(&paths, EFFECT_COPY | EFFECT_MOVE | EFFECT_LINK, MK_LBUTTON);
    with(|a| {
        a.view.drag_leave();
        if let Err(e) = r {
            status(a, format!("Drag failed: {e}"));
        }
    });
    invalidate(hwnd);
}

// ---- drop in --------------------------------------------------------------------

/// Our IDropTarget: every drop on the window, ours or another program's. The
/// view says where the pointer is (`View::drop_dest`, computed while it
/// paints the drag); the effect comes from `ole_glue::choose`.
struct Drops {
    hwnd: isize,
}

fn keys_mods(keys: u32) -> Mods {
    Mods { ctrl: keys & MK_CONTROL != 0, shift: keys & MK_SHIFT != 0, alt: keys & MK_ALT != 0 }
}

impl Drops {
    fn h(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    /// Moves the view's pointer to `pt` and paints at once, so `drop_dest`
    /// is the target under the pointer now.
    fn track(&self, pt: (i32, i32), keys: u32) -> Option<DropDest> {
        let mut p = POINT { x: pt.0, y: pt.1 };
        let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(self.h(), &mut p) };
        let s = crate::dpi_scale();
        let pos = Pt::new(p.x as f32 / s, p.y as f32 / s);
        with(|a| a.view.drag_keys = Some(keys_mods(keys)));
        push(self.h(), Event::MouseMove { pos });
        invalidate(self.h());
        // a paint outside the app borrow (UpdateWindow sends WM_PAINT now)
        let _ = unsafe { windows::Win32::Graphics::Gdi::UpdateWindow(self.h()) };
        with(|a| a.view.drop_dest.clone()).flatten()
    }

    fn effect(&self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> (u32, Option<DropDest>) {
        match self.track(pt, keys) {
            Some(DropDest::Folder(f)) => (ole_glue::choose(&info.paths, Path::new(&f), keys, allowed), Some(DropDest::Folder(f))),
            Some(DropDest::Pin(k)) => (if allowed & EFFECT_LINK != 0 { EFFECT_LINK } else { allowed & (EFFECT_COPY | EFFECT_MOVE) }, Some(DropDest::Pin(k))),
            None => (EFFECT_NONE, None),
        }
    }
}

impl DropEvents for Drops {
    fn enter(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32 {
        let paths = info.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        with(|a| a.view.drag_enter(paths));
        self.effect(info, pt, keys, allowed).0
    }
    fn over(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32 {
        self.effect(info, pt, keys, allowed).0
    }
    fn leave(&mut self) {
        with(|a| a.view.drag_leave());
        push(self.h(), Event::MouseLeave);
        invalidate(self.h());
    }
    fn drop(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32 {
        let (e, dest) = self.effect(info, pt, keys, allowed);
        with(|a| a.view.drag_leave());
        push(self.h(), Event::MouseLeave);
        invalidate(self.h());
        let rejected = info.rejected;
        match dest {
            Some(DropDest::Pin(k)) => {
                let one = (info.paths.len() == 1).then(|| info.paths[0].to_string_lossy().into_owned());
                with(|a| match one {
                    Some(p) => a.view.pin_dropped(&p, k),
                    None => status(a, "Only one folder can be pinned at a time"),
                });
                e
            }
            Some(DropDest::Folder(folder)) if e != EFFECT_NONE => {
                with(|a| drop_into(a, &info.paths, folder, e, rejected));
                e
            }
            _ => EFFECT_NONE,
        }
    }
}

/// A drop (or paste) of `paths` into `folder` with effect `e`.
fn drop_into(a: &mut App, paths: &[PathBuf], folder: String, e: u32, rejected: usize) {
    if e == EFFECT_LINK {
        clip(a, ClipJob::Links { targets: paths.to_vec(), folder: PathBuf::from(&folder) }, ClipCtx::Links { folder, names: names_of(paths) });
        return;
    }
    let target = PathBuf::from(&folder);
    let ops = ole_glue::ops(paths, &target, e);
    let mut job = if e == EFFECT_MOVE { Job::new("Moving", "Moved") } else { Job::new("Copying", "Copied") }.into_folder(folder, names_of(paths));
    job.keep_both = paths.iter().any(|s| s.parent().is_some_and(|p| formats::same_path(p, &target)));
    a.view.ops.submit(ops, job);
    if rejected > 0
        && let Some(m) = a.view.ops.message.as_mut()
    {
        m.push_str(&format!(" ({rejected} unsafe path(s) refused)"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_shortcuts() {
        let m = |ctrl, shift| Mods { ctrl, shift, alt: false };
        let k = |c: u8| c as u16;
        assert_eq!(key_act(k(b'C'), m(true, false)), Some(KeyAct::Copy));
        assert_eq!(key_act(VK_INSERT.0, m(true, false)), Some(KeyAct::Copy));
        assert_eq!(key_act(k(b'X'), m(true, false)), Some(KeyAct::Cut));
        assert_eq!(key_act(k(b'V'), m(true, false)), Some(KeyAct::Paste));
        assert_eq!(key_act(VK_INSERT.0, m(false, true)), Some(KeyAct::Paste));
        assert_eq!(key_act(k(b'C'), m(true, true)), Some(KeyAct::CopyPath));
        assert_eq!(key_act(VK_DELETE.0, m(false, false)), Some(KeyAct::Delete { permanent: false }));
        assert_eq!(key_act(VK_DELETE.0, m(false, true)), Some(KeyAct::Delete { permanent: true }));
        assert_eq!(key_act(k(b'Z'), m(true, false)), Some(KeyAct::Undo));
        assert_eq!(key_act(k(b'Y'), m(true, false)), Some(KeyAct::Redo));
        assert_eq!(key_act(k(b'N'), m(true, true)), Some(KeyAct::NewFolder));
        assert_eq!(key_act(k(b'C'), Mods { ctrl: true, shift: false, alt: true }), None);
        assert_eq!(key_act(k(b'C'), m(false, false)), None, "typing goes to the filter");
    }

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
