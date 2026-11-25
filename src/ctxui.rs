//! The own-drawn context menu, app side (CM2, CM3, CM5; `docs/ctxmenu.md`).
//!
//! A right click (Shift+F10, the Menu key) makes a [`CtxMenu`]: Tsubame's own
//! rows paint at once, the shell's arrive from a pool worker as
//! [`MenuBatch`]es of this menu's generation and are appended below. A pick
//! is either an app action (run by `shellops::ctx_after_frame` through the
//! app's own clipboard, rename and file-op code) or a shell command id sent
//! back to the worker that holds the menu.
//!
//! The UI thread never touches COM here: the clipboard check is
//! `IsClipboardFormatAvailable`, everything else is data.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use tsubame_config::ContextMenu;
use tsubame_config::menu::{self, MenuEntry, Target as Kind};
use tsubame_ui::widgets::menu::{A11yReq, A11ySnap, EDIT_DELETE, EDIT_DOWN, EDIT_ENTRY, EDIT_GROUP, EDIT_GROUP_BASE, EDIT_GROUP_NATIVE, EDIT_GROUP_NEW, EDIT_HIDE, EDIT_PIN, EDIT_SETTINGS, EDIT_UP, SubLoad, menu as draw_menu};
use tsubame_ui::{Canvas, Id, MenuRow, MenuSource, MenuState, MenuStyle, Pt, Rect, RowIcon, RowKind, Theme, Ui};
use tsubame_win::lagtrace;
use tsubame_win::shell::ctxmodel::{self, IconCache, ItemKind, ItemSource, LoadState, MenuBatch, MenuItem, MenuModel, Part, Route, SubLoads, SubState, flag};
use tsubame_win::shell::{CtxHandle, CtxOpts, Pool, Request};
use tsubame_win::uia;

use crate::rowmenu;

pub const ID_CTX: Id = Id::new("ctxmenu");
/// The loading line's animation / reveal-grace timer (only while a menu is
/// holding for grace, loading, or waiting on a submenu hover delay).
pub const TIMER_ID: usize = 11;
/// Loading spinner repaint interval (~10 FPS; avoids 30 FPS full-window
/// repaints while waiting on a slow shell handler).
pub const TICK_MS: u32 = 100;
/// Loading line animation interval (~30 FPS). Runs only while the line is
/// on screen (after [`LOADING_SHOW_MS`]); a smooth sweep, not a 10 FPS stutter.
pub const ANIM_MS: u32 = 33;
/// Hold painting a fresh menu until the shell's rows are in, at most this
/// long: a warm shell answers in ~20 to 130 ms, and a menu revealed with only
/// Tsubame's rows would resize and move when the shell's rows arrive. Past
/// the cap the menu shows with space reserved for them ([`CtxMenu::reserve`]).
/// `TSUBAME_CTX_GRACE_MS=0` disables the hold (immediate reveal).
pub const REVEAL_GRACE_MS: u64 = 200;
/// The same hold for a cold shell (no pre-warm yet: DLLs load, ~400 ms to 1 s):
/// long enough that a typical cold menu still shows complete in one frame.
pub const COLD_GRACE_MS: u64 = 450;
const SPIN_MS: u128 = 1200;
pub const FAIL_TEXT: &str = "Some items could not load";
const STATUS_KEY: u64 = 0x0057_a7a5_0001;

thread_local! {
    static INPUT_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    static NEXT_GEN: Cell<u64> = const { Cell::new(1) };
}

/// Set once any shell context menu has loaded in this process. On the very
/// first (cold) open, shell extension DLL loading routinely takes hundreds of
/// ms, so holding the popup invisible for the 60 ms grace window only adds
/// dead input lag before showing Tsubame's own rows.
static SHELL_WARMED: AtomicBool = AtomicBool::new(false);

/// The startup pre-warm: not asked, asked (a worker is loading the shell
/// handlers), or done.
static WARM: AtomicU8 = AtomicU8::new(WARM_NONE);
const WARM_NONE: u8 = 0;
const WARM_PENDING: u8 = 1;
const WARM_DONE: u8 = 2;

/// The pre-warm worker answered (`Payload::Warm`): the shell handlers are
/// loaded, so the first menu is a warm one.
pub fn note_warm_done() {
    WARM.store(WARM_DONE, Ordering::Relaxed);
}

/// The shell's handlers are loaded (a menu or the pre-warm got through).
pub fn shell_warm() -> bool {
    SHELL_WARMED.load(Ordering::Relaxed) || WARM.load(Ordering::Relaxed) == WARM_DONE
}

/// A menu opened now will be answered fast, so it is worth holding it for
/// (warm, or the pre-warm is working on it right now). A cold unwarmed first
/// menu takes ~0.4 to 1 s: it is held up to [`COLD_GRACE_MS`], then shown with space reserved.
pub fn worth_holding() -> bool {
    shell_warm() || WARM.load(Ordering::Relaxed) == WARM_PENDING
}

/// Size (width, height) of the last complete top level per kind (background,
/// item), DIPs: the next menu of that kind reserves it while its shell rows are coming.
static LAST_SIZE: std::sync::Mutex<[[f32; 2]; 2]> = std::sync::Mutex::new([[0.0; 2]; 2]);

/// Shell rows a menu with no memory reserves room for.
const DEFAULT_SHELL_ROWS: f32 = 6.0;

/// The right button went down / the menu key was pressed: the menu's
/// "input to first paint" clock starts here.
pub fn note_input() {
    INPUT_AT.with(|c| c.set(Some(Instant::now())));
    if lagtrace::on() {
        lagtrace::record("ctx.input", None, format_args!("cold={}", !SHELL_WARMED.load(Ordering::Relaxed)), false);
    }
}

fn take_input() -> Instant {
    INPUT_AT.with(|c| c.take()).filter(|t| t.elapsed() < Duration::from_secs(2)).unwrap_or_else(Instant::now)
}

fn next_gen() -> u64 {
    NEXT_GEN.with(|c| {
        let g = c.get();
        c.set(g + 1);
        g
    })
}

fn env_u32(name: &str) -> u32 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// Reveal grace for a cold vs warm menu. An explicit `TSUBAME_CTX_GRACE_MS`
/// always wins; otherwise both hold until the shell's rows are in (cold: a
/// longer cap), because a menu shown with only Tsubame's rows jumps when the
/// shell's arrive.
pub fn grace_for(cold: bool) -> Duration {
    let env_ms = std::env::var("TSUBAME_CTX_GRACE_MS").ok().and_then(|v| v.trim().parse::<u64>().ok());
    let ms = match env_ms {
        Some(v) => v,
        None if cold => COLD_GRACE_MS,
        None => REVEAL_GRACE_MS,
    }
    .min(2000);
    Duration::from_millis(ms)
}

/// `TSUBAME_NATIVE_MENU=1` keeps the Windows popup menu (the fallback).
pub fn own_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("TSUBAME_NATIVE_MENU").map(|v| v.trim() != "1").unwrap_or(true))
}

/// `TSUBAME_CTX_WARM=0` skips the startup pre-warm (A/B timing runs).
pub fn warm_enabled() -> bool {
    own_enabled() && std::env::var("TSUBAME_CTX_WARM").map(|v| v.trim() != "0").unwrap_or(true)
}

/// A filesystem folder the shell can bind (a drive path or a UNC path).
fn warmable(folder: &str) -> bool {
    let b = folder.as_bytes();
    (b.len() >= 3 && b[1] == b':' && b[2] == b'\\' && b[0].is_ascii_alphabetic()) || folder.starts_with(r"\\")
}

/// Asks a shell worker to load the context-menu handlers of `folder` once,
/// below normal priority, so the first right click does not wait for DLLs
/// (cold `QueryContextMenu` ~1 s, warm ~15 ms). Nothing comes back to draw.
pub fn prewarm(pool: &mut Pool, folder: &str, icon_px: u32) {
    if warmable(folder) && !shell_warm() {
        WARM.store(WARM_PENDING, Ordering::Relaxed);
        pool.submit(Request::CtxWarm { folder: folder.to_string(), icon_px });
    }
}

/// The loading line stays hidden this long: a menu the shell fills within it
/// (a warm worker: ~20 ms) never flashes a bar.
pub const LOADING_SHOW_MS: u128 = 200;

/// What was right-clicked.
#[derive(Clone, Debug, Default)]
pub struct Target {
    pub paths: Vec<String>,
    /// Folder paths from the enumerated store; never probe disk on UI thread.
    pub folders: Vec<String>,
    /// the folder shown (background menus: the same as `paths[0]`)
    pub folder: String,
    pub background: bool,
    /// Shift was held: extended verbs
    pub extended: bool,
    /// client DIPs where the menu opens
    pub anchor: Pt,
    /// opened from the keyboard
    pub keyboard: bool,
    /// screen px: the shell's `ptInvoke` and the classic menu
    pub screen: (i32, i32),
}

/// A pick, run after the frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// an app action (`file.copy`, `x.terminal`) or a menu entry id
    App(String),
    /// a shell command id (the worker invokes it)
    Shell(u32),
    /// `windows.more`: the native popup at the same point
    Classic,
}

pub struct CtxMenu {
    pub generation: u64,
    pub target: Target,
    pub model: MenuModel,
    /// the menu settings this menu was built from (`entries` is the effective
    /// list); edits from the menu itself (CM8) change this copy, and the app
    /// saves it when `edited` / `learned` is set
    pub menu: ContextMenu,
    /// the user hid, renamed or moved a row: the app saves `menu`
    pub edited: bool,
    /// new items were remembered in `menu.items`: the app saves them
    pub learned: bool,
    /// Settings knew items when this menu opened: newly seen ones are tagged
    /// New (the very first menu, and each of its batches, tags nothing)
    tag_new: bool,
    /// "Edit in Settings…" / Manage: the app opens Settings on this key
    pub settings_key: Option<String>,
    /// the row the last Hide hid, (key, label) (Undo on the strip)
    undo: Option<(String, String)>,
    /// the list as it was before the last Delete entry (Undo on the strip)
    undo_entries: Option<Vec<MenuEntry>>,
    /// "Edit entry…": Settings opens the entry's form, not only the row
    pub settings_edit: bool,
    dual: bool,
    handle: Option<CtxHandle>,
    /// the pool job (the watchdog reports it when it hangs)
    pub ticket: u64,
    pub state: MenuState,
    /// still on screen
    pub open: bool,
    input_at: Instant,
    asked_at: Instant,
    /// hold painting until `Part::Complete` / error or this grace elapses
    grace: Duration,
    /// latched once the popup has been revealed (stays true across Retry)
    revealed: bool,
    /// input -> first painted frame / -> first frame with the shell's top
    /// level / -> first frame with every shell item the worker sends up
    /// front (lazy: the top level; `TSUBAME_CTX_EAGER=1`: the whole tree)
    pub first_paint: Option<Duration>,
    pub top_items: Option<Duration>,
    pub all_items: Option<Duration>,
    /// lazy submenus asked of the worker (CM6)
    subs: SubLoads,
    /// the last lazy submenu: request to answer (ms resolution)
    pub sub_latency: Option<Duration>,
    /// the menu's clock (submenu hover timing, slow submenus)
    clock: Instant,
    pub picked: Option<Pick>,
    pub retry: bool,
    /// an app row that runs a shell verb (Properties) picked before the
    /// shell's items came: invoked when they do
    pub pending_verb: Option<String>,
    /// an invoke went to the worker: keep the handle until it is done
    pub invoked: bool,
    /// the worker's answer to the invoke (Err: status bar)
    pub invoke_result: Option<Result<(), String>>,
    items: Vec<MenuItem>,
    dirty: bool,
    paste_ok: bool,
    icon_px: u32,
    /// CM10: what the screen reader was last told
    a11y: A11yOut,
    /// CM10: the system colours when Windows High Contrast is on (read once
    /// per menu); None inside: the app's theme
    hc: Option<Option<Theme>>,
    /// Height the top level keeps while the shell's rows are coming (DIPs; 0:
    /// estimate from the row size). Set from [`LAST_H`] when the menu opens.
    pub reserve: [f32; 2],
    /// the top level's rect at the first painted frame (the log compares it with the final one)
    first_rect: Option<Rect>,
}

/// What the UI Automation side was last given (CM10).
#[derive(Default)]
struct A11yOut {
    prev: Option<A11ySnap>,
    published: bool,
}

/// The extension (lower-case, no dot) all of `paths` share, "" when they
/// differ, when one has none or when `is_dir` says one is a folder. What the
/// extension filter of an entry is held against on a selection.
pub fn common_ext(paths: &[String], is_dir: impl Fn(&str) -> bool) -> String {
    let ext = |p: &str| std::path::Path::new(p).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let Some(first) = paths.first() else { return String::new() };
    if is_dir(first) {
        return String::new();
    }
    let e = ext(first);
    if paths.iter().all(|p| !is_dir(p) && ext(p) == e) { e } else { String::new() }
}

/// The right-clicked kind and extension for the U4 rules: the background, a
/// selection of 2 or more (the extension is the one all of them share), a
/// drive root, a folder or a file.
pub fn kind_of(target: &Target) -> (Kind, String) {
    if target.background {
        return (Kind::Background, String::new());
    }
    if target.paths.len() >= 2 {
        return (Kind::Multiple, common_ext(&target.paths, |p| target.folders.iter().any(|f| f == p)));
    }
    let first = target.paths.first().map(String::as_str).unwrap_or("");
    if is_root(first) {
        return (Kind::Drive, String::new());
    }
    if target.folders.iter().any(|p| p == first) {
        return (Kind::Folder, String::new());
    }
    (Kind::File, std::path::Path::new(first).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default())
}

/// Lexical only; no filesystem query even for disconnected shares.
pub fn is_root(path: &str) -> bool {
    let p = std::path::Path::new(path);
    p.has_root() && p.parent().is_none()
}

/// App rows that are not in the U4 list.
/// Keyed `app:<action>`, so they can be hidden and renamed (CM8).
pub fn extras(kind: Kind, n: usize, dual: bool) -> Vec<MenuItem> {
    let c = |l: &str, a: &str| MenuItem { key: format!("app:{a}"), ..MenuItem::command(ItemSource::Tsubame, l, a) };
    match kind {
        Kind::Background => vec![c("Paste", "file.paste"), c("New folder", "x.new-folder"), c("New text file", "x.new-file"), c("Copy as path", "file.copy-path"), c("Open terminal here", "x.terminal"), c("Refresh", "x.refresh")],
        _ => {
            let mut v = vec![c("Copy name", "x.copy-name"), c("Open terminal here", "x.terminal")];
            if dual && n == 1 && matches!(kind, Kind::Folder | Kind::Drive) {
                v.insert(0, c("Open in other pane", "x.other-pane"));
            }
            // tsubame.ext.batch_rename: gone with the extension
            if n > 1 && crate::batchui::is_enabled() {
                v.insert(0, c(&format!("Rename {n} items\u{2026}"), "file.batch_rename"));
            }
            // P4.F6: opens the tag picker (own popup, not a submenu)
            if !matches!(kind, Kind::Drive) && crate::tags::enabled() {
                v.push(c("Tags\u{2026}", "tags.pick"));
            }
            v
        }
    }
}

/// The Tsubame group for a click: the U4 built-ins + escape hatch + extras.
pub fn tsubame_rows(entries: &[MenuEntry], kind: Kind, ext: &str, n: usize, dual: bool) -> Vec<MenuItem> {
    let mut rows = ctxmodel::own_rows(entries, kind, ext, &extras(kind, n, dual));
    // a new tab only opens folders
    if matches!(kind, Kind::File | Kind::Background) {
        rows.retain(|r| r.action != "tab.open-selected");
    }
    for r in &mut rows {
        if r.action == "file.open" {
            r.flags |= flag::DEFAULT;
        }
    }
    rows
}

/// The Tsubame and user rows for `target` (drive roots: no Cut, Delete,
/// Rename).
fn own_parts(target: &Target, entries: &[MenuEntry], dual: bool) -> (Vec<MenuItem>, Vec<MenuItem>) {
    let (kind, ext) = kind_of(target);
    let mut tsubame = tsubame_rows(entries, kind, &ext, target.paths.len(), dual);
    if target.paths.iter().any(|p| is_root(p)) && !target.background {
        for row in &mut tsubame {
            if matches!(row.action.as_str(), "file.cut" | "file.delete" | "file.rename") {
                row.flags |= flag::DISABLED;
            }
        }
    }
    // a selection without a folder has nothing to open in a tab
    if kind == Kind::Multiple && !target.paths.iter().any(|p| target.folders.contains(p)) {
        tsubame.retain(|r| r.action != "tab.open-selected");
    }
    (tsubame, ctxmodel::own_items(entries, kind, &ext).1)
}

/// Hides (or shows) the row keyed `key`: a list entry (built-in, custom) by
/// its `hidden`, any other row by its `[[context_menu.items]]` pref (made on
/// first edit, `label` its label). A CM7-era `Source::Windows` entry of the
/// same key is kept in step so it cannot hide the row again.
pub fn set_hidden(m: &mut ContextMenu, key: &str, label: &str, hidden: bool) {
    if let Some(e) = m.entries.iter_mut().find(|e| e.id == key && !ctxmodel::is_item_key(key)) {
        e.hidden = hidden;
        return;
    }
    for e in m.entries.iter_mut().filter(|e| e.source == menu::Source::Windows && e.id.eq_ignore_ascii_case(key)) {
        e.hidden = false;
    }
    pref_mut(m, key, label).hidden = hidden;
}

/// Renames the row keyed `key` (empty `name`: the original label again).
pub fn set_name(m: &mut ContextMenu, key: &str, label: &str, name: &str) {
    let name = name.trim();
    if let Some(e) = m.entries.iter_mut().find(|e| e.id == key && !ctxmodel::is_item_key(key)) {
        if !name.is_empty() {
            e.name = name.to_string();
        } else if let Some(d) = menu::default_entries().into_iter().find(|d| d.id == key) {
            // built-ins go back to their default name; custom ones need a name
            e.name = d.name;
        }
        return;
    }
    for e in m.entries.iter_mut().filter(|e| e.source == menu::Source::Windows && e.id.eq_ignore_ascii_case(key)) {
        e.name.clear();
    }
    pref_mut(m, key, label).name = name.to_string();
}

/// Pin to top (or Unpin) the row keyed `key`: `ItemPref.pin`. Only rows kept
/// in `[[context_menu.items]]` (Windows', the apps' and Tsubame's `app:` rows)
/// pin; list entries have the U4 order and Move up / down. False: not pinnable.
pub fn set_pin(m: &mut ContextMenu, key: &str, label: &str, pin: bool) -> bool {
    if !ctxmodel::is_item_key(key) {
        return false;
    }
    pref_mut(m, key, label).pin = pin;
    true
}

/// Keys of the rows the last complete menu drew at its top level (Tsubame's,
/// Windows' and the apps'), for "Refresh from last menu". UI thread only.
static LAST_SHOWN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The last complete menu: what it was for, when (local "14:32") and how many rows it drew.
static LAST_MENU: std::sync::Mutex<Option<LastMenu>> = std::sync::Mutex::new(None);

/// What "Refresh from last menu" compares with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastMenu {
    pub kind: Kind,
    pub at: String,
    pub items: usize,
}

/// The last complete menu this run showed, if any.
pub fn last_menu() -> Option<LastMenu> {
    LAST_MENU.lock().ok().and_then(|g| g.clone())
}

/// Local "HH:MM" now (the Settings readout of the last menu).
fn clock_now() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}

/// "Refresh from last menu": forgets the learned items nobody edited that the
/// last menu did not show ([`menu::refresh_learned`]). Returns how many went;
/// 0 and no change when no menu was shown yet (Settings' "Refresh from last
/// menu" button calls it).
pub fn refresh_learned(m: &mut ContextMenu) -> usize {
    let shown = LAST_SHOWN.lock().map(|g| g.clone()).unwrap_or_default();
    if shown.is_empty() { 0 } else { menu::refresh_learned(&mut m.items, &shown) }
}

fn pref_mut<'a>(m: &'a mut ContextMenu, key: &str, label: &str) -> &'a mut menu::ItemPref {
    if menu::item(&m.items, key).is_none() {
        m.items.push(menu::ItemPref { key: key.to_string(), label: label.to_string(), from: ctxmodel::from_of(key), ..Default::default() });
    }
    menu::item_mut(&mut m.items, key).expect("just added")
}

/// List entries Move up / down works on: Tsubame's built-ins (and the
/// classic-menu row) among themselves, custom and imported ones among
/// themselves. Windows' items keep their native order.
fn movable(e: &MenuEntry) -> bool {
    e.parent.is_empty() && !e.is_separator() && (e.source == menu::Source::Builtin || e.source.runs_program() || e.id == ctxmodel::ACTION_CLASSIC)
}

fn same_group(a: &MenuEntry, b: &MenuEntry) -> bool {
    a.source.runs_program() == b.source.runs_program()
}

/// The entry Move up / down of `key` would pass: the next movable one of
/// the same group that the menu shows (`shown`: the drawn rows' keys).
fn move_neighbour(entries: &[MenuEntry], key: &str, up: bool, shown: &[&str]) -> Option<(usize, usize)> {
    let i = entries.iter().position(|e| e.id == key && movable(e))?;
    let ok = |j: &usize| movable(&entries[*j]) && same_group(&entries[i], &entries[*j]) && shown.contains(&entries[*j].id.as_str());
    let j = if up { (0..i).rev().find(ok) } else { (i + 1..entries.len()).find(ok) }?;
    Some((i, j))
}

/// Can the entry `key` move one shown row up (or down)? (Settings' Move up / down state.)
pub fn can_move(entries: &[MenuEntry], key: &str, up: bool, shown: &[&str]) -> bool {
    move_neighbour(entries, key, up, shown).is_some()
}

/// Moves the entry `key` one shown row up or down. False: it cannot move.
pub fn move_entry_by(entries: &mut Vec<MenuEntry>, key: &str, up: bool, shown: &[&str]) -> bool {
    let Some((i, j)) = move_neighbour(entries, key, up, shown) else { return false };
    menu::move_entry(entries, i, if up { j } else { j + 1 }).is_some()
}

/// The clipboard holds files (Paste is enabled). No COM: format probes only.
pub fn clipboard_has_files() -> bool {
    use windows::Win32::System::DataExchange::{IsClipboardFormatAvailable, RegisterClipboardFormatW};
    use windows::core::w;
    unsafe {
        const CF_HDROP: u32 = 15;
        IsClipboardFormatAvailable(CF_HDROP).is_ok() || IsClipboardFormatAvailable(RegisterClipboardFormatW(w!("FileGroupDescriptorW"))).is_ok() || IsClipboardFormatAvailable(RegisterClipboardFormatW(w!("Shell IDList Array"))).is_ok()
    }
}

impl CtxMenu {
    /// Builds the menu (own rows now) and asks a worker for the shell's.
    pub fn open(target: Target, menu: ContextMenu, dual: bool, paste_ok: bool, icon_px: u32, pool: &mut Pool) -> CtxMenu {
        let trace = lagtrace::on();
        let t_prep = trace.then(Instant::now);
        let (tsubame, user) = own_parts(&target, &menu.entries, dual);
        let generation = next_gen();
        let state = MenuState::new(target.anchor, target.keyboard);
        let tag_new = !menu.items.is_empty();
        let cold = !worth_holding();
        let grace = grace_for(cold);
        let paths_n = target.paths.len();
        let bg = target.background;
        let mut m = CtxMenu {
            generation,
            tag_new,
            model: MenuModel::new(generation, tsubame, user),
            target,
            menu,
            edited: false,
            learned: false,
            settings_key: None,
            undo: None,
            undo_entries: None,
            settings_edit: false,
            dual,
            handle: None,
            ticket: 0,
            state,
            open: true,
            input_at: take_input(),
            asked_at: Instant::now(),
            grace,
            revealed: grace.is_zero(),
            first_paint: None,
            top_items: None,
            all_items: None,
            subs: SubLoads::default(),
            sub_latency: None,
            clock: Instant::now(),
            picked: None,
            retry: false,
            pending_verb: None,
            invoked: false,
            invoke_result: None,
            items: Vec::new(),
            dirty: true,
            paste_ok,
            icon_px,
            a11y: A11yOut::default(),
            hc: None,
            reserve: LAST_SIZE.lock().map(|g| g[usize::from(!bg)]).unwrap_or([0.0; 2]),
            first_rect: None,
        };
        m.learn_from(&m.model.tsubame.clone());
        m.submit(pool);
        if let Some(t) = t_prep {
            let since_input = m.input_at.elapsed().as_secs_f64() * 1000.0;
            lagtrace::record("ctx.open", Some(t.elapsed().as_secs_f64() * 1000.0), format_args!("gen={generation} cold={cold} grace_ms={} paths={paths_n} bg={bg} since_input_ms={since_input:.3}", grace.as_millis()), false);
            if m.revealed {
                let reason = if cold { "cold_immediate" } else { "zero_grace" };
                lagtrace::record("ctx.ui.reveal", Some(since_input), format_args!("gen={generation} reason={reason}"), false);
            }
        }
        m
    }

    /// Remembers the rows of `items` Settings does not know yet (CM8).
    fn learn_from(&mut self, items: &[MenuItem]) {
        let seen = ctxmodel::seen(items);
        if seen.is_empty() {
            return;
        }
        self.learned |= menu::learn(&mut self.menu.items, &seen, self.tag_new, kind_of(&self.target).0);
    }

    /// Rebuilds the Tsubame and user rows after an edit of the list.
    fn rebuild_own(&mut self) {
        let (t, u) = own_parts(&self.target, &self.menu.entries, self.dual);
        self.model.tsubame = t;
        self.model.user = u;
        self.dirty = true;
    }

    /// The item at `path` of the drawn rows.
    fn item(&self, path: &[u64]) -> Option<&MenuItem> {
        let (&last, up) = path.split_last()?;
        let mut level = &self.items[..];
        for &k in up {
            level = &level.iter().find(|it| it.kind == ItemKind::Submenu && item_key(it) == k)?.children;
        }
        level.iter().find(|it| item_key(it) == last)
    }

    /// Runs an edit-menu row (CM8) on the row at `path`.
    pub fn edit(&mut self, path: &[u64], act: u64) {
        let Some(it) = self.item(path) else { return };
        let (key, label) = (it.key.clone(), it.label.clone());
        if key.is_empty() {
            return;
        }
        match act {
            EDIT_HIDE => {
                self.undo = Some((key.clone(), label.clone()));
                set_hidden(&mut self.menu, &key, &label, true);
            }
            EDIT_UP | EDIT_DOWN => {
                let shown: Vec<&str> = self.items.iter().map(|i| i.key.as_str()).collect();
                if !move_entry_by(&mut self.menu.entries, &key, act == EDIT_UP, &shown) {
                    return;
                }
            }
            EDIT_PIN => {
                let pinned = menu::item(&self.menu.items, &key).is_some_and(|p| p.pin);
                if !set_pin(&mut self.menu, &key, &label, !pinned) {
                    return;
                }
            }
            EDIT_SETTINGS => {
                self.settings_key = Some(key);
                return;
            }
            EDIT_ENTRY => {
                self.settings_key = Some(key);
                self.settings_edit = true;
                return;
            }
            EDIT_DELETE => {
                let before = self.menu.entries.clone();
                let Some(i) = before.iter().position(|e| e.id == key && e.source.runs_program() || e.id == key && e.source == menu::Source::Custom) else { return };
                if !menu::delete_entry(&mut self.menu.entries, i) {
                    return;
                }
                self.undo = Some((key, label));
                self.undo_entries = Some(before);
            }
            a if a == EDIT_GROUP_NEW || a == EDIT_GROUP_NATIVE || a >= EDIT_GROUP_BASE => {
                let Some(f) = self.facts_at(path) else { return };
                let Some(pick) = rowmenu::group_pick(&f, a) else { return };
                let gid = match pick {
                    rowmenu::GroupPick::Group(id) => id,
                    rowmenu::GroupPick::Native => String::new(),
                    rowmenu::GroupPick::New => menu::add_group(&mut self.menu.user_groups, &self.menu.groups, ""),
                };
                if !ctxmodel::is_item_key(&key) {
                    return;
                }
                pref_mut(&mut self.menu, &key, &label);
                if !menu::move_item_to_group(&mut self.menu.items, &self.menu.user_groups, &key, &gid) && a != EDIT_GROUP_NEW {
                    return;
                }
            }
            _ => return,
        }
        self.edited = true;
        self.rebuild_own();
    }

    /// The edit-menu facts of the row at `path` (the row menu's model).
    fn facts_at(&self, path: &[u64]) -> Option<rowmenu::Facts> {
        let it = self.item(path)?;
        Some(row_facts(&self.items, &self.menu, it, path))
    }

    /// An inline rename was kept (empty: the original label again).
    pub fn rename(&mut self, path: &[u64], name: &str) {
        let Some(it) = self.item(path) else { return };
        let (key, label) = (it.key.clone(), it.label.clone());
        if key.is_empty() || name == label {
            return;
        }
        set_name(&mut self.menu, &key, &label, name);
        self.edited = true;
        self.rebuild_own();
    }

    /// Undo on the strip: the row the last Hide hid shows again.
    pub fn undo_hide(&mut self) {
        if let Some(list) = self.undo_entries.take() {
            // Undo after Delete entry: the list as it was
            self.undo = None;
            self.menu.entries = list;
            self.edited = true;
            self.rebuild_own();
            return;
        }
        if let Some((key, label)) = self.undo.take() {
            set_hidden(&mut self.menu, &key, &label, false);
            self.edited = true;
            self.rebuild_own();
        }
    }

    fn submit(&mut self, pool: &mut Pool) {
        let handle = CtxHandle::new();
        let opts = CtxOpts {
            background: self.target.background,
            extended: self.target.extended,
            icon_px: self.icon_px,
            // CM6: submenus are filled when they open; `TSUBAME_CTX_EAGER=1`
            // walks the whole tree up front (the CM2 behaviour, for A/B runs)
            submenus: env_u32("TSUBAME_CTX_EAGER") == 1,
            debug_delay_ms: env_u32("TSUBAME_CTX_DELAY_MS"),
            debug_sub_delay_ms: env_u32("TSUBAME_CTX_SUB_DELAY_MS"),
            // CM7: Win11 packaged commands after the classic rows;
            // `TSUBAME_CTX_PACKAGED=0` leaves them out (A/B timing runs)
            packaged: std::env::var("TSUBAME_CTX_PACKAGED").map(|v| v.trim() != "0").unwrap_or(true),
            debug_pkg_delay_ms: env_u32("TSUBAME_CTX_PKG_DELAY_MS"),
            ..Default::default()
        };
        let paths = if self.target.background { vec![self.target.folder.clone()] } else { self.target.paths.clone() };
        self.ticket = pool.submit(Request::ctx(paths, self.generation, opts, &handle)).id;
        self.handle = Some(handle);
        self.asked_at = Instant::now();
    }

    /// Retry on the failure row: a new generation (late answers of the old
    /// one are dropped), the own rows stay.
    pub fn retry_now(&mut self, pool: &mut Pool) {
        self.generation = next_gen();
        // Command ids belong to one worker session. Never let Retry invoke
        // an id retained from the previous session.
        self.handle = None;
        self.model = MenuModel::new(self.generation, self.model.tsubame.clone(), self.model.user.clone());
        // submenu tokens belong to the old worker session too
        self.subs.clear();
        self.top_items = None;
        self.all_items = None;
        self.input_at = Instant::now();
        self.dirty = true;
        self.submit(pool);
    }

    /// A batch for this menu (false: another generation, or nothing to draw).
    pub fn apply(&mut self, b: MenuBatch) -> bool {
        if b.generation != self.generation {
            return false;
        }
        match b.part {
            Part::Invoked { result, .. } => {
                self.invoke_result = Some(result);
                return false;
            }
            Part::Closed => {
                self.handle = None;
                self.invoked = false;
                self.pending_verb = None;
                return false;
            }
            part => {
                if let Part::Failed(ref error) = part {
                    self.handle = None;
                    self.revealed = true;
                    if self.pending_verb.take().is_some() {
                        self.invoke_result = Some(Err(error.clone()));
                    }
                }
                if let Part::Submenu { sub, .. } = part
                    && let Some(at) = self.subs.done(sub)
                {
                    self.sub_latency = Some(Duration::from_millis(self.now_ms().saturating_sub(at)));
                }
                let complete = matches!(part, Part::Complete);
                let native = matches!(part, Part::Native(_));
                let packaged = matches!(part, Part::Packaged(_));
                let failed_now = matches!(part, Part::Failed(_));
                if native || complete {
                    SHELL_WARMED.store(true, Ordering::Relaxed);
                }
                if complete {
                    let was = std::mem::replace(&mut self.revealed, true);
                    if !was && lagtrace::on() {
                        lagtrace::record("ctx.ui.reveal", Some(self.input_at.elapsed().as_secs_f64() * 1000.0), format_args!("gen={} reason=complete since_open_ms={:.3}", self.generation, self.asked_at.elapsed().as_secs_f64() * 1000.0), false);
                    }
                }
                if lagtrace::on() {
                    let pname = match &part {
                        Part::Native(_) => "native",
                        Part::Icons(_) => "icons",
                        Part::Packaged(_) => "packaged",
                        Part::Submenu { .. } => "submenu",
                        Part::Complete => "complete",
                        Part::Failed(_) => "failed",
                        _ => "other",
                    };
                    let since_input = self.input_at.elapsed().as_secs_f64() * 1000.0;
                    lagtrace::record("ctx.ui.batch", None, format_args!("gen={} part={pname} since_input_ms={since_input:.3} revealed={}", self.generation, self.is_revealed()), failed_now);
                }
                self.model.apply(MenuBatch { part, ..b });
                self.dirty = true;
                // CM8: Settings lists what real menus showed
                if native {
                    self.learn_from(&self.model.native.clone());
                } else if packaged {
                    self.learn_from(&self.model.packaged.clone());
                }
                if complete && self.model.error.is_none() && self.handle.is_some() {
                    let keys: Vec<String> = [&self.model.tsubame, &self.model.native, &self.model.packaged].into_iter().flat_map(|l| l.iter()).filter(|it| !it.is_separator() && !it.key.is_empty()).map(|it| it.key.clone()).collect();
                    let n = keys.len();
                    if let Ok(mut g) = LAST_SHOWN.lock() {
                        *g = keys;
                    }
                    if let Ok(mut g) = LAST_MENU.lock() {
                        *g = Some(LastMenu { kind: kind_of(&self.target).0, at: clock_now(), items: n });
                    }
                }
                if native && let Some(v) = self.pending_verb.take() {
                    self.invoke_verb(&v);
                }
            }
        }
        self.open && self.is_revealed()
    }

    /// The watchdog gave the worker up.
    pub fn lost(&mut self) {
        let g = self.generation;
        self.model.apply(MenuBatch { generation: g, part: Part::Failed("the shell did not answer".into()), timing: [0; 3] });
        self.handle = None;
        self.pending_verb = None;
        self.revealed = true;
        // the worker is gone: its command ids and submenu tokens are dead
        // (open submenus show the failure row, Retry asks a new worker)
        self.model.native.clear();
        self.model.packaged.clear();
        self.dirty = true;
    }

    /// Sends command `id` to the worker (picked at the menu's point).
    pub fn invoke(&mut self, id: u32, shift: bool, ctrl: bool) {
        if let Some(h) = &self.handle {
            let (x, y) = self.target.screen;
            h.invoke(id, x, y, shift, ctrl);
            self.invoked = true;
        }
    }

    /// Runs the shell's `verb` (Properties from Tsubame's row): now if the
    /// shell's items are in, else as soon as they are.
    pub fn invoke_verb(&mut self, verb: &str) {
        match ctxmodel::native_by_verb(&self.model.native, verb) {
            Some(id) => self.invoke(id, false, false),
            None if self.handle.is_some() && self.model.state == LoadState::Loading => self.pending_verb = Some(verb.to_string()),
            None => {}
        }
    }

    /// Nothing more will happen: the app drops it (which closes the menu on
    /// the worker).
    pub fn finished(&self) -> bool {
        !self.open && self.picked.is_none() && !self.retry && self.pending_verb.is_none() && !self.invoked
    }

    fn shown(&self) -> LoadState {
        ctxmodel::shown_state(self.model.state, self.asked_at.elapsed().as_millis() as u64)
    }

    /// The loading line shows (and its timer runs).
    pub fn loading(&self) -> bool {
        self.open && !self.model.complete && self.shown() != LoadState::Failed
    }

    /// Stress harness: asks for the first lazy submenu, as a hover would.
    /// True when a request went out.
    pub fn expand_first(&mut self) -> bool {
        let now = self.now_ms();
        let Some(sub) = lazy_subs(&self.model.native).first().copied() else { return false };
        match &self.handle {
            Some(h) if self.subs.want(sub, now) => {
                h.expand(sub);
                true
            }
            _ => false,
        }
    }

    /// A lazy submenu is asked for and not answered.
    pub fn sub_pending(&self) -> bool {
        self.subs.pending()
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    /// True once the menu should draw: complete, failed, grace elapsed, or
    /// previously revealed.
    pub fn is_revealed(&self) -> bool {
        self.revealed || self.model.complete || self.model.error.is_some() || self.shown() == LoadState::Failed || self.asked_at.elapsed() >= self.grace
    }

    /// Milliseconds until the next timer wake, or `None` when idle (0 wake-ups).
    /// - During the reveal grace window: a single one-shot for the remaining grace.
    /// - While loading (or a lazy submenu is loading): `TICK_MS` (100 ms, 10 FPS),
    ///   clamped to any sooner submenu hover deadline.
    /// - While only a submenu hover delay is pending: exact remaining delay.
    pub fn next_tick_ms(&self) -> Option<u32> {
        if !self.open {
            return None;
        }
        if !self.is_revealed() {
            let left = self.grace.saturating_sub(self.asked_at.elapsed());
            return Some((left.as_millis() as u32).max(1));
        }
        let now = self.now_ms();
        let hover = self.state.deadline().map(|dl| dl.saturating_sub(now).clamp(1, 1000) as u32);
        let tick = if self.loading() {
            // the line shows after LOADING_SHOW_MS: one wake-up for its start, then it animates
            let waited = self.asked_at.elapsed().as_millis();
            Some(if waited < LOADING_SHOW_MS { (LOADING_SHOW_MS - waited).max(1) as u32 } else { ANIM_MS })
        } else if self.subs.loading(now) {
            Some(TICK_MS)
        } else {
            None
        };
        match (tick, hover) {
            (Some(t), Some(h)) => Some(t.min(h)),
            (t, h) => t.or(h),
        }
    }

    /// The menu's tick timer must run: the reveal grace window, loading line,
    /// a submenu that is loading (until it turns into the failure row) or a
    /// hover delay. False while the menu just sits there: no wake-ups.
    #[allow(dead_code)]
    pub fn ticking(&self) -> bool {
        self.next_tick_ms().is_some()
    }

    /// Draws the menu (after every other widget).
    pub fn paint(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bounds: Rect) {
        if !self.open || !self.is_revealed() {
            return;
        }
        let was_revealed = std::mem::replace(&mut self.revealed, true);
        if !was_revealed && lagtrace::on() {
            let reason = if self.model.error.is_some() || self.shown() == LoadState::Failed {
                "failed"
            } else if self.model.complete {
                "complete"
            } else {
                "grace_expired"
            };
            lagtrace::record("ctx.ui.reveal", Some(self.input_at.elapsed().as_secs_f64() * 1000.0), format_args!("gen={} reason={reason} since_open_ms={:.3}", self.generation, self.asked_at.elapsed().as_secs_f64() * 1000.0), false);
        }
        if self.dirty {
            self.items = self.model.rows(&self.menu);
            self.dirty = false;
        }
        let loading = self.loading();
        // rows still to come: keep room for them, so they fill in without a resize or move
        let settled = self.model.complete || self.model.error.is_some();
        self.state.reserve_w = if settled { 0.0 } else { self.reserve[0] };
        self.state.reserve_h = if settled { 0.0 } else { self.reserve_px(ui) };
        let waited = self.asked_at.elapsed().as_millis();
        let phase = (loading && waited >= LOADING_SHOW_MS).then(|| (waited % SPIN_MS) as f32 / SPIN_MS as f32);
        let alt = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x12) } < 0;
        let now = self.now_ms();
        // CM10: one atomic load until a screen reader has asked for the provider
        let a11y = uia::active();
        let style = MenuStyle { bounds, loading: phase, alt, now_ms: now, a11y };
        let failed = self.shown() == LoadState::Failed;
        let src = Rows { items: &self.items, icons: &self.model.icons, failed, paste_ok: self.paste_ok, subs: &self.subs, now, can_load: self.handle.is_some(), menu: Some(&self.menu) };
        // CM10: Windows High Contrast draws the menu in the system colours
        let hc = *self.hc.get_or_insert_with(|| high_contrast_theme(&ui.theme));
        let saved = ui.theme;
        if let Some(t) = hc {
            ui.theme = t;
        }
        let r = draw_menu(ui, g, ID_CTX, &mut self.state, &src, &style);
        ui.theme = saved;
        if let Some(path) = r.picked {
            self.picked = src.pick(&path);
        }
        // CM6: ask the worker for the submenus the menu shows or is about to
        // open; while filtering, for every submenu (their rows are searched)
        let want = if self.state.filtering() { lazy_subs(&self.items) } else { self.state.wanted().iter().filter_map(|p| src.item_at(p)).filter(|it| ctxmodel::is_lazy(it)).map(|it| it.sub).collect() };
        if a11y {
            self.publish_a11y(r.closed);
        } else if self.a11y.published {
            self.close_a11y();
        }
        if r.retry {
            self.retry = true;
        }
        if r.closed {
            self.open = false;
        }
        if !self.open {
            self.close_a11y();
        }
        if self.open
            && let Some(h) = &self.handle
        {
            for sub in want {
                if self.subs.want(sub, now) {
                    h.expand(sub);
                }
            }
        }
        // CM8: edits from the edit menu, the inline rename and the strip
        if let Some((path, act)) = &r.edit {
            self.edit(path, *act);
        }
        if let Some((path, name)) = &r.renamed {
            self.rename(path, name);
        }
        if r.undo {
            self.undo_hide();
        }
        if r.manage
            && let Some((k, _)) = &self.undo
        {
            self.settings_key = Some(k.clone());
        }
        if self.settings_key.is_some() {
            self.open = false;
        }
    }

    /// The height the top level reserves while loading: the last complete menu
    /// of this kind, else the rows drawn so far plus a typical shell block.
    fn reserve_px(&self, ui: &Ui) -> f32 {
        if self.reserve[1] > 0.0 {
            return self.reserve[1];
        }
        let m = tsubame_ui::widgets::menu::Metrics::new(&ui.theme);
        (self.items.len() as f32 + DEFAULT_SHELL_ROWS) * m.row + 2.0 * (m.pad + 1.0)
    }

    /// CM10: hands the screen reader this frame's menu when it changed.
    fn publish_a11y(&mut self, closed: bool) {
        let Some(snap) = self.state.a11y.take() else { return };
        // a frame that stopped early (a pick, an edit menu opening) is partial
        if closed || snap.levels.len() != self.state.levels.len() || self.a11y.prev.as_ref() == Some(&snap) {
            return;
        }
        uia::publish(a11y_snap(&snap));
        self.a11y.prev = Some(snap);
        self.a11y.published = true;
    }

    /// CM10: the menu is gone (closed, or nobody listens any more): tell the
    /// screen reader once and let go of the providers.
    fn close_a11y(&mut self) {
        if std::mem::take(&mut self.a11y.published) {
            uia::publish(uia::Snap::default());
        }
        self.a11y.prev = None;
    }

    /// CM10: runs what the screen reader asked (Invoke, Collapse) on the next frame.
    pub fn run_a11y_requests(&mut self) {
        for r in uia::take_requests() {
            self.state.request(r.lv, r.key, if r.collapse { A11yReq::Collapse } else { A11yReq::Invoke });
        }
    }

    /// Called after Canvas::end, so timings include submitting the frame.
    pub fn presented(&mut self) {
        if !self.open || !self.revealed { return; }
        let trace = lagtrace::on();
        if self.first_paint.is_none() {
            let d = self.input_at.elapsed();
            self.first_paint = Some(d);
            self.first_rect = self.state.levels.first().map(|l| l.rect);
            if trace {
                lagtrace::record("ctx.ui.first_paint", Some(d.as_secs_f64() * 1000.0), format_args!("gen={} rows={} complete={} loading={}", self.generation, self.items.len(), self.model.complete, self.loading()), false);
            }
        }
        if self.model.state == LoadState::Ready && self.top_items.is_none() {
            let d = self.input_at.elapsed();
            self.top_items = Some(d);
            if trace {
                lagtrace::record("ctx.ui.top_items_paint", Some(d.as_secs_f64() * 1000.0), format_args!("gen={} rows={}", self.generation, self.items.len()), false);
            }
        }
        if self.model.complete && self.model.error.is_none() && self.all_items.is_none() {
            let d = self.input_at.elapsed();
            self.all_items = Some(d);
            if let (Some(lv), Ok(mut g)) = (self.state.levels.first(), LAST_SIZE.lock())
                && lv.rect.h > 0.0
            {
                g[usize::from(!self.target.background)] = [lv.rect.w, lv.rect.h];
            }
            if trace {
                lagtrace::record("ctx.ui.all_items_paint", Some(d.as_secs_f64() * 1000.0), format_args!("gen={} rows={}", self.generation, self.items.len()), true);
            }
            self.log_timing();
        }
    }

    /// `TSUBAME_CTX_LOG=<file>`: appends [`CtxMenu::timing_line`] once the menu is complete
    /// (A/B timing runs; off by default, no file access otherwise).
    fn log_timing(&self) {
        static FILE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        let Some(path) = FILE.get_or_init(|| std::env::var("TSUBAME_CTX_LOG").ok().filter(|v| !v.is_empty())) else { return };
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let kind = if self.target.background { "background" } else { "item" };
            let rect = |r: Option<Rect>| r.map_or("-".to_string(), |r| format!("{:.0},{:.0} {:.0}x{:.0}", r.x, r.y, r.w, r.h));
            let fin = self.state.levels.first().map(|l| l.rect);
            let _ = writeln!(f, "{kind}: {}; rect first [{}] final [{}] {}", self.timing_line(), rect(self.first_rect), rect(fin), if self.first_rect == fin { "STABLE" } else { "JUMP" });
        }
    }

    /// One line for the log / status: the menu's timings.
    #[allow(dead_code)]
    pub fn timing_line(&self) -> String {
        let ms = |d: Option<Duration>| d.map_or("-".to_string(), |d| format!("{:.1}", d.as_secs_f64() * 1000.0));
        format!("own menu: first paint {} ms, top level {} ms, all items {} ms, {} rows", ms(self.first_paint), ms(self.top_items), ms(self.all_items), self.items.len())
    }
}

impl Drop for CtxMenu {
    fn drop(&mut self) {
        self.close_a11y();
    }
}

/// What the UI Automation side gets of a widget snapshot (CM10).
pub fn a11y_snap(s: &A11ySnap) -> uia::Snap {
    let px = |r: Rect| [r.x, r.y, r.w, r.h];
    let levels = s
        .levels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let name = if i == 0 { "Context menu".to_string() } else { s.levels[i - 1].rows.iter().find(|r| r.key == l.key).map(|r| r.name.clone()).unwrap_or_else(|| "Submenu".into()) };
            uia::Level {
                key: l.key,
                name,
                rect: px(l.rect),
                filter: l.filter.as_ref().map(|(r, t)| (px(*r), t.clone())),
                rows: l
                    .rows
                    .iter()
                    .map(|r| uia::Row {
                        key: r.key,
                        name: r.name.clone(),
                        access: r.accel.map(|c| format!("Alt+{}", c.to_uppercase())).unwrap_or_default(),
                        shortcut: r.shortcut.clone(),
                        rect: px(r.rect),
                        enabled: r.enabled,
                        toggled: r.checked || r.radio,
                        submenu: r.submenu,
                        expanded: r.expanded,
                        focused: r.focused,
                        visible: r.visible,
                    })
                    .collect(),
            }
        })
        .collect();
    uia::Snap { levels }
}

/// The menu's theme under Windows High Contrast: the system colours. None
/// when High Contrast is off. The selection is the system highlight with the
/// system highlight text on it; there is no dim text and no accent hue.
pub fn high_contrast_theme(base: &Theme) -> Option<Theme> {
    use windows::Win32::Graphics::Gdi::{GetSysColor, SYS_COLOR_INDEX};
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETHIGHCONTRAST, SystemParametersInfoW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    let ok = unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, Some(&mut hc as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) }.is_ok();
    if !ok || hc.dwFlags.0 & HCF_HIGHCONTRASTON.0 == 0 {
        return None;
    }
    // COLORREF is 0x00BBGGRR; the theme is 0xRRGGBB
    let sys = |i: i32| {
        let c = unsafe { GetSysColor(SYS_COLOR_INDEX(i)) };
        ((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff)
    };
    Some(hc_colours(base, sys))
}

/// [`high_contrast_theme`] over a colour source (a `COLOR_*` index to 0xRRGGBB).
pub fn hc_colours(base: &Theme, sys: impl Fn(i32) -> u32) -> Theme {
    const WINDOW: i32 = 5;
    const WINDOWTEXT: i32 = 8;
    const HIGHLIGHT: i32 = 13;
    const HIGHLIGHTTEXT: i32 = 14;
    const GRAYTEXT: i32 = 17;
    let mut t = *base;
    t.popup = sys(WINDOW);
    t.panel = t.popup;
    t.text = sys(WINDOWTEXT);
    t.text_dim = t.text;
    t.text_disabled = sys(GRAYTEXT);
    t.border = t.text;
    t.popup_border = t.text;
    t.scrollbar = t.text;
    t.scrollbar_hover = t.text;
    t.danger = t.text;
    t.accent = sys(HIGHLIGHT);
    t.selected = t.accent;
    t.on_accent = sys(HIGHLIGHTTEXT);
    // the open parent row and the check box have no fill of their own
    t.selected_inactive = t.popup;
    t.focus = t.on_accent;
    t.highlight = t.accent;
    t
}

/// Row key: stable across batches (the top level, then the walked tree).
pub fn item_key(it: &MenuItem) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h ^= x as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    };
    eat(&[it.source as u8, it.kind as u8]);
    eat(it.key.as_bytes());
    // a keyed row keeps its row key when it is renamed (focus stays on it)
    if it.key.is_empty() {
        eat(it.label.as_bytes());
    }
    eat(it.action.as_bytes());
    eat(&it.id.to_le_bytes());
    h | 1
}

/// Byte index of the accelerator `ch` in `label`: a word start first.
pub fn accel_index(label: &str, ch: char) -> Option<usize> {
    let is = |c: char| c.to_lowercase().next() == Some(ch);
    let mut start = true;
    for (i, c) in label.char_indices() {
        if start && is(c) {
            return Some(i);
        }
        start = !c.is_alphanumeric();
    }
    label.char_indices().find(|(_, c)| is(*c)).map(|(i, _)| i)
}

fn own_accel(it: &MenuItem) -> Option<char> {
    let (id, def) = match it.key.as_str() {
        "builtin.open" => ('o', "Open"),
        "builtin.open-tab" => ('n', "Open in new tab"),
        "builtin.cut" => ('t', "Cut"),
        "builtin.copy" => ('c', "Copy"),
        "builtin.paste" => ('p', "Paste"),
        "builtin.copy-path" => ('a', "Copy as path"),
        "builtin.rename" => ('m', "Rename"),
        "builtin.delete" => ('d', "Delete"),
        "builtin.properties" => ('r', "Properties"),
        ctxmodel::ACTION_CLASSIC => ('w', ctxmodel::CLASSIC_LABEL),
        _ => return None,
    };
    (it.label == def).then_some(id)
}

/// Shortcut text of an app row (the keys the list binds).
pub fn own_shortcut(action: &str) -> &'static str {
    match action {
        "file.open" => "Enter",
        "tab.open-selected" => "Ctrl+Enter",
        "file.cut" => "Ctrl+X",
        "file.copy" => "Ctrl+C",
        "file.paste" => "Ctrl+V",
        "file.copy-path" => "Ctrl+Shift+C",
        "file.rename" => "F2",
        "file.delete" => "Del",
        "x.new-folder" => "Ctrl+Shift+N",
        "x.refresh" => "Ctrl+R",
        _ => "",
    }
}

/// Icon-font glyph for an app action or a shell verb without a bitmap.
pub fn glyph(action: &str, verb: Option<&str>) -> Option<char> {
    let g = match action {
        "file.open" => '\u{E8E5}',
        "tab.open-selected" => '\u{E8A7}',
        "x.other-pane" => '\u{E90D}',
        "file.cut" => '\u{E8C6}',
        "file.copy" => '\u{E8C8}',
        // a tag: the name, not the file (Copy keeps the two-sheet glyph)
        "x.copy-name" => '\u{E8EC}',
        "file.paste" => '\u{E77F}',
        "file.copy-path" => '\u{E71B}',
        "file.rename" | "file.batch_rename" => '\u{E8AC}',
        "file.delete" => '\u{E74D}',
        "file.properties" => '\u{E946}',
        ctxmodel::ACTION_CLASSIC => '\u{E712}',
        "x.new-folder" => '\u{E8F4}',
        "x.new-file" => '\u{E8A5}',
        "x.terminal" => '\u{E756}',
        "x.refresh" => '\u{E72C}',
        "tags.pick" => '\u{E1CB}',
        _ => match verb? {
            "open" => '\u{E8E5}',
            "openas" | "openwith" => '\u{E7AC}',
            "runas" => '\u{E7EF}',
            "share" | "windows.share" | "windows.modernshare" => '\u{E72D}',
            "properties" => '\u{E946}',
            "delete" => '\u{E74D}',
            "cut" => '\u{E8C6}',
            "copy" => '\u{E8C8}',
            "paste" => '\u{E77F}',
            "rename" => '\u{E8AC}',
            "copyaspath" | "link" => '\u{E71B}',
            "print" => '\u{E749}',
            "edit" => '\u{E70F}',
            _ => return None,
        },
    };
    Some(g)
}

/// The model's items as menu rows.
pub struct Rows<'a> {
    pub items: &'a [MenuItem],
    pub icons: &'a IconCache,
    /// add the failure row with Retry
    pub failed: bool,
    pub paste_ok: bool,
    /// lazy submenus asked of the worker, and the menu's clock
    pub subs: &'a SubLoads,
    pub now: u64,
    /// a worker holds the menu (false: lazy submenus cannot load)
    pub can_load: bool,
    /// the menu settings (CM8: the edit menu; None: rows cannot be edited)
    pub menu: Option<&'a ContextMenu>,
}

/// Every lazy submenu's token in `items` (the filter searches them all).
pub fn lazy_subs(items: &[MenuItem]) -> Vec<u32> {
    let mut v = Vec::new();
    let mut stack: Vec<&MenuItem> = items.iter().collect();
    while let Some(it) = stack.pop() {
        if ctxmodel::is_lazy(it) {
            v.push(it.sub);
        }
        stack.extend(it.children.iter());
    }
    v
}

/// The rows at `path` of `items` (row keys of the submenus from the top).
fn level_of<'a>(items: &'a [MenuItem], path: &[u64]) -> Option<&'a [MenuItem]> {
    let mut level = items;
    for &k in path {
        level = &level.iter().find(|it| it.kind == ItemKind::Submenu && item_key(it) == k)?.children;
    }
    Some(level)
}

/// The row menu's facts for the row `it` at `path` (`docs/ctxmenu.md`, the edit menu).
pub fn row_facts(items: &[MenuItem], m: &ContextMenu, it: &MenuItem, path: &[u64]) -> rowmenu::Facts {
    let nested = path.len() > 1;
    let key = it.key.as_str();
    if !ctxmodel::is_item_key(key) {
        // a list row: Tsubame's own or a custom one. Move up / down pass the rows the menu draws
        let level: Vec<&str> = path.split_last().and_then(|(_, up)| level_of(items, up)).map(|l| l.iter().map(|i| i.key.as_str()).collect()).unwrap_or_default();
        let can = |up: bool| path.len() == 1 && move_neighbour(&m.entries, key, up, &level).is_some();
        return match m.entries.iter().find(|e| e.id == key) {
            Some(e) => rowmenu::Facts { name: it.label.clone(), nested, ..rowmenu::Facts::entry(e, can(true), can(false), true) },
            None => rowmenu::Facts { ty: rowmenu::RowType::Own, name: it.label.clone(), hidden: false, pinned: false, nested, can_up: false, can_down: false, groups: Vec::new(), group: String::new(), menu_side: true },
        };
    }
    let pref = menu::item(&m.items, key).cloned().unwrap_or_else(|| menu::ItemPref { key: key.to_string(), label: it.label.clone(), from: ctxmodel::from_of(key), ..Default::default() });
    rowmenu::Facts { name: it.label.clone(), ..rowmenu::Facts::learned(&pref, &m.user_groups, nested, true) }
}

impl Rows<'_> {
    fn level(&self, path: &[u64]) -> Option<&[MenuItem]> {
        level_of(self.items, path)
    }

    /// The item at `path` (row keys from the top).
    pub fn item_at(&self, path: &[u64]) -> Option<&MenuItem> {
        let (&last, up) = path.split_last()?;
        self.level(up)?.iter().find(|it| item_key(it) == last)
    }

    /// The pick at `path` (row keys from the top).
    pub fn pick(&self, path: &[u64]) -> Option<Pick> {
        let (&last, up) = path.split_last()?;
        let it = self.level(up)?.iter().find(|it| item_key(it) == last)?;
        Some(match ctxmodel::route(it) {
            Route::Shell(id) => Pick::Shell(id),
            Route::App(a) if a == ctxmodel::ACTION_CLASSIC => Pick::Classic,
            Route::App(a) => Pick::App(a),
        })
    }

    pub fn row(&self, it: &MenuItem) -> MenuRow {
        if it.is_separator() {
            return MenuRow::separator();
        }
        if it.flags & flag::LABEL != 0 {
            return MenuRow { key: item_key(it), kind: RowKind::Label, label: it.label.clone(), ..Default::default() };
        }
        let own = it.source != ItemSource::Native;
        let accel = if own { own_accel(it) } else { it.accel };
        let action = it.action.as_str();
        let icon = match it.icon.filter(|k| self.icons.peek(*k).is_some()) {
            Some(k) => RowIcon::Image(k),
            None => glyph(action, it.verb.as_deref()).map_or(RowIcon::None, RowIcon::Glyph),
        };
        let mut disabled = it.disabled();
        if action == "file.paste" && !self.paste_ok {
            disabled = true;
        }
        MenuRow {
            key: item_key(it),
            kind: RowKind::Item,
            label: it.label.clone(),
            accel: accel.and_then(|c| accel_index(&it.label, c)),
            shortcut: if own {
                crate::commands::by_name(action).and_then(|c| crate::keymap::current_label(c.id)).unwrap_or_else(|| own_shortcut(action).to_string())
            } else { it.shortcut.clone() },
            icon,
            disabled,
            default: it.flags & flag::DEFAULT != 0,
            danger: action == "file.delete" || it.verb.as_deref() == Some("delete"),
            custom: it.source == ItemSource::User,
            checked: it.flags & flag::CHECKED != 0 && it.flags & flag::RADIO == 0,
            radio: it.flags & flag::CHECKED != 0 && it.flags & flag::RADIO != 0,
            submenu: it.kind == ItemKind::Submenu,
            quick: it.kind == ItemKind::Command && is_quick(action, it.verb.as_deref()),
        }
    }
}

/// The common actions that fold into the icon row of a menu taller than the
/// window (CM9, the mock's state 4 variant A): Cut, Copy, Paste, Rename,
/// Delete (the app's rows) and the shell's Share.
pub fn is_quick(action: &str, verb: Option<&str>) -> bool {
    matches!(action, "file.cut" | "file.copy" | "file.paste" | "file.rename" | "file.delete") || matches!(verb, Some("share" | "windows.share" | "windows.modernshare"))
}

impl MenuSource for Rows<'_> {
    fn rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
        let Some(level) = self.level(path) else { return };
        out.extend(level.iter().map(|it| self.row(it)));
        if path.is_empty() && self.failed {
            out.push(MenuRow::separator());
            out.push(MenuRow::status(STATUS_KEY, FAIL_TEXT));
        }
    }
    fn icon(&self, key: u64) -> Option<(u32, u32, &[u32])> {
        self.icons.peek(key).map(|b| (b.w, b.h, &b.px[..]))
    }
    fn sub_state(&self, path: &[u64]) -> SubLoad {
        match self.item_at(path) {
            Some(it) if ctxmodel::is_lazy(it) => sub_load(self.can_load, self.subs.state(it.sub, self.now)),
            _ => SubLoad::Ready,
        }
    }
    /// CM8, mock `ctxgap-rowmenu` state C: the edit menu of the row at `path`
    /// (the flyout of Move to group when `path` ends with `EDIT_GROUP`).
    fn edit_rows(&self, path: &[u64], out: &mut Vec<MenuRow>) {
        let Some(m) = self.menu else { return };
        let (flyout, path) = match path.split_last() {
            Some((&EDIT_GROUP, up)) => (true, up),
            _ => (false, path),
        };
        let Some(it) = self.item_at(path) else { return };
        if it.key.is_empty() || it.is_separator() {
            return;
        }
        let f = row_facts(self.items, m, it, path);
        if flyout {
            out.extend(rowmenu::group_rows(&f));
        } else {
            out.extend(rowmenu::rows(&f));
        }
    }
}

/// An empty lazy submenu: "Loading…" until it is asked and answered, the
/// failure row once it is slow or nobody can answer.
pub fn sub_load(can_load: bool, s: SubState) -> SubLoad {
    match s {
        _ if !can_load => SubLoad::Failed,
        SubState::Slow => SubLoad::Failed,
        SubState::Idle | SubState::Loading => SubLoad::Loading,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsubame_config::menu::default_entries;

    fn labels(v: &[MenuItem]) -> Vec<&str> {
        v.iter().map(|i| if i.is_separator() { "--" } else { i.label.as_str() }).collect()
    }

    #[test]
    fn common_actions_fold_into_the_icon_row() {
        for a in ["file.cut", "file.copy", "file.paste", "file.rename", "file.delete"] {
            assert!(is_quick(a, None), "{a}");
        }
        assert!(is_quick("", Some("windows.modernshare")));
        assert!(is_quick("", Some("share")));
        assert!(!is_quick("file.properties", None));
        assert!(!is_quick("", Some("open")));
    }

    #[test]
    fn target_kind_uses_enumerated_metadata() {
        assert!(is_root(r"C:\"));
        assert!(!is_root(r"C:\folder"));
        let mut t = Target { paths: vec![r"\\server\share\folder".into()], folders: vec![r"\\server\share\folder".into()], ..Default::default() };
        assert_eq!(kind_of(&t), (Kind::Folder, String::new()));
        t.folders.clear();
        t.paths = vec![r"C:\missing\file.TXT".into()];
        assert_eq!(kind_of(&t), (Kind::File, "txt".into()));
        t.background = true;
        assert_eq!(kind_of(&t), (Kind::Background, String::new()));
    }

    #[test]
    fn two_or_more_items_are_several_and_a_drive_root_is_a_drive() {
        let mut t = Target { paths: vec![r"C:\a\x.TXT".into(), r"C:\a\y.txt".into()], ..Default::default() };
        assert_eq!(kind_of(&t), (Kind::Multiple, "txt".into()), "the shared extension");
        t.paths.push(r"C:\a\z.md".into());
        assert_eq!(kind_of(&t), (Kind::Multiple, String::new()), "mixed extensions: none");
        t.paths = vec![r"C:\a\x.txt".into(), r"C:\a\sub.txt".into()];
        t.folders = vec![r"C:\a\sub.txt".into()];
        assert_eq!(kind_of(&t), (Kind::Multiple, String::new()), "a folder in it: none");
        // a drive root alone; a root among others is still a selection
        let d = Target { paths: vec![r"D:\".into()], folders: vec![r"D:\".into()], ..Default::default() };
        assert_eq!(kind_of(&d), (Kind::Drive, String::new()));
        let both = Target { paths: vec![r"C:\".into(), r"D:\".into()], folders: vec![r"C:\".into(), r"D:\".into()], ..Default::default() };
        assert_eq!(kind_of(&both).0, Kind::Multiple);
        assert_eq!(common_ext(&[], |_| false), "");
    }

    #[test]
    fn several_and_drive_menus_use_their_own_entry_sets() {
        let mut e = menu::default_entries();
        // Delete is a selection verb only: off for a drive root by its flag
        e.iter_mut().filter(|x| x.id == "builtin.delete").for_each(|x| x.show_for.drives = false);
        e.iter_mut().filter(|x| x.id == "builtin.open-tab").for_each(|x| x.show_for.multiple = false);
        let names = |k: Kind| tsubame_rows(&e, k, "", 2, false).into_iter().map(|r| r.label).collect::<Vec<_>>();
        assert!(!names(Kind::Drive).contains(&"Delete".to_string()), "Drive follows show_for.drives");
        assert!(names(Kind::Multiple).contains(&"Delete".to_string()));
        assert!(!names(Kind::Multiple).contains(&"Open in new tab".to_string()), "Several items follows show_for.multiple");
        assert!(names(Kind::Drive).contains(&"Open in new tab".to_string()), "a drive opens in a tab");
        // a custom entry that only has the Several flag does not show on one item, nor on a drive
        let mut c = MenuEntry::custom("custom.1", "Zip all", "z.exe", "{selection}");
        c.show_for = menu::ShowFor { files: false, folders: false, background: false, multiple: true, drives: false };
        e.push(c);
        let user = |t: &Target| own_parts(t, &e, false).1.len();
        let two = Target { paths: vec![r"C:\a.txt".into(), r"C:\b.txt".into()], ..Default::default() };
        let one = Target { paths: vec![r"C:\a.txt".into()], ..Default::default() };
        let drive = Target { paths: vec![r"D:\".into()], folders: vec![r"D:\".into()], ..Default::default() };
        assert_eq!((user(&two), user(&one), user(&drive)), (1, 0, 0));
    }

    #[test]
    fn failed_pending_verb_finishes_and_stale_failure_does_not() {
        let mut c = CtxMenu {
            generation: 2, target: Target::default(), model: MenuModel::new(2, vec![], vec![]), menu: ContextMenu::default(), edited: false, learned: false, tag_new: false, settings_key: None, undo: None, undo_entries: None, settings_edit: false, dual: false,
            handle: None, ticket: 0, state: MenuState::default(), open: false,
            input_at: Instant::now(), asked_at: Instant::now(), grace: Duration::ZERO, revealed: true, first_paint: None, top_items: None, all_items: None, subs: SubLoads::default(), sub_latency: None, clock: Instant::now(),
            picked: None, retry: false, pending_verb: Some("properties".into()), invoked: false, invoke_result: None,
            items: vec![], dirty: false, paste_ok: false, icon_px: 16, a11y: A11yOut::default(), hc: None, reserve: [0.0; 2], first_rect: None,
        };
        let failed = |generation| MenuBatch { generation, part: Part::Failed("bind failed".into()), timing: [0; 3] };
        c.apply(failed(1));
        assert!(!c.finished());
        assert_eq!(c.pending_verb.as_deref(), Some("properties"));
        c.apply(failed(2));
        assert!(c.finished());
        assert_eq!(c.invoke_result, Some(Err("bind failed".into())));
    }

    #[test]
    fn rows_per_target() {
        let e = default_entries();
        let file = tsubame_rows(&e, Kind::File, "txt", 1, true);
        let l = labels(&file);
        assert_eq!(l[0], "Open");
        assert!(!l.contains(&"Open in new tab"), "files: no new tab: {l:?}");
        assert!(!l.contains(&"Open in other pane"));
        assert!(l.contains(&"Copy name") && l.contains(&ctxmodel::CLASSIC_LABEL));
        assert!(file[0].flags & flag::DEFAULT != 0);
        let dir = labels(&tsubame_rows(&e, Kind::Folder, "", 1, true)).join("|");
        assert!(dir.contains("Open in new tab") && dir.contains("Open in other pane"), "{dir}");
        assert!(!labels(&tsubame_rows(&e, Kind::Folder, "", 2, true)).contains(&"Open in other pane"), "one folder only");
        let bg_rows = tsubame_rows(&e, Kind::Background, "", 1, false);
        let bg = labels(&bg_rows);
        assert_eq!(bg, ["Paste", "New folder", "New text file", "Copy as path", "Open terminal here", "Refresh", "--", ctxmodel::CLASSIC_LABEL]);
    }

    /// First right click on empty space: every static row exists before the
    /// shell answers (no batch applied), and the loading line is not due yet.
    #[test]
    fn background_menu_is_complete_before_any_shell_batch() {
        let mut menu = ContextMenu::default();
        menu.entries = tsubame_config::menu::effective(&menu.entries);
        let t = Target { paths: vec![r"C:\x".into()], folders: vec![r"C:\x".into()], folder: r"C:\x".into(), background: true, ..Default::default() };
        let (tsubame, user) = own_parts(&t, &menu.entries, false);
        let m = MenuModel::new(1, tsubame, user);
        assert_eq!(m.state, LoadState::Loading, "nothing from the shell yet");
        assert!(m.native.is_empty() && m.packaged.is_empty());
        let rows = m.rows(&menu);
        let l = labels(&rows);
        for want in ["Paste", "New folder", "New text file", "Copy as path", "Open terminal here", "Refresh", ctxmodel::CLASSIC_LABEL] {
            assert!(l.contains(&want), "{want} missing: {l:?}");
        }
        const { assert!(LOADING_SHOW_MS >= 150) };
    }

    #[test]
    fn prewarm_only_for_bindable_folders() {
        assert!(warmable(r"C:\Users\x"));
        assert!(warmable(r"d:\"));
        assert!(warmable(r"\\server\share\dir"));
        assert!(!warmable("This PC"));
        assert!(!warmable(""));
        assert!(!warmable("C:"));
    }

    #[test]
    fn accelerators_prefer_word_starts() {
        assert_eq!(accel_index("Open in new tab", 'n'), Some(8));
        assert_eq!(accel_index("Cut", 't'), Some(2));
        assert_eq!(accel_index("Copy as path", 'a'), Some(5));
        assert_eq!(accel_index("Properties", 'r'), Some(1));
        assert_eq!(accel_index("Öffnen", 'ö'), Some(0));
        assert_eq!(accel_index("abc", 'z'), None);
    }

    #[test]
    fn rows_map_flags_icons_and_picks() {
        let e = default_entries();
        let mut own = tsubame_rows(&e, Kind::File, "txt", 1, false);
        let mut native = vec![MenuItem { id: 7, verb: Some("openas".into()), ..MenuItem::command(ItemSource::Native, "Open with", "") }, MenuItem { id: 8, verb: Some("delete".into()), ..MenuItem::command(ItemSource::Native, "Delete", "") }];
        let mut sub = MenuItem { kind: ItemKind::Submenu, ..MenuItem::command(ItemSource::Native, "7-Zip", "") };
        sub.children.push(MenuItem { id: 30, ..MenuItem::command(ItemSource::Native, "Add to archive", "") });
        native.push(sub);
        let mut model = MenuModel::new(1, std::mem::take(&mut own), vec![]);
        model.native = native;
        let items = model.rows(&ContextMenu { entries: e.clone(), ..Default::default() });
        let icons = IconCache::new(4);
        let subs = SubLoads::default();
        let src = Rows { items: &items, icons: &icons, failed: true, paste_ok: false, subs: &subs, now: 0, can_load: true, menu: None };
        let mut out = Vec::new();
        src.rows(&[], &mut out);
        assert_eq!(out[0].label, "Open", "no app-name caption before actions");
        assert!(!out.iter().any(|r| r.label == "Tsubame"));
        let by = |l: &str| out.iter().find(|r| r.label == l).unwrap().clone();
        assert!(by("Open").default && by("Open").accel == Some(0));
        assert!(by("Paste").disabled, "no files on the clipboard");
        assert!(by("Delete").danger && by("Delete").shortcut == "Del");
        assert_eq!(out.iter().filter(|r| r.label == "Delete").count(), 1, "native Delete is covered");
        assert_eq!(by("Open with").icon, RowIcon::Glyph('\u{E7AC}'));
        assert!(by("7-Zip").submenu);
        assert_eq!(out.last().unwrap().kind, RowKind::Status);
        // picks route
        assert_eq!(src.pick(&[by("Copy").key]), Some(Pick::App("file.copy".into())));
        assert_eq!(src.pick(&[by("Open with").key]), Some(Pick::Shell(7)));
        assert_eq!(src.pick(&[by(ctxmodel::CLASSIC_LABEL).key]), Some(Pick::Classic));
        let mut subrows = Vec::new();
        src.rows(&[by("7-Zip").key], &mut subrows);
        assert_eq!(subrows.len(), 1);
        assert_eq!(src.pick(&[by("7-Zip").key, subrows[0].key]), Some(Pick::Shell(30)));
        assert_eq!(src.pick(&[12345]), None);
    }

    #[test]
    fn keys_are_stable_when_the_tree_replaces_the_top_level() {
        let a = MenuItem { id: 5, ..MenuItem::command(ItemSource::Native, "Share", "") };
        let mut b = a.clone();
        b.help = Some("walked later".into());
        assert_eq!(item_key(&a), item_key(&b));
        let c = MenuItem { id: 6, ..a.clone() };
        assert_ne!(item_key(&a), item_key(&c));
    }

    #[test]
    fn a_renamed_keyed_row_keeps_its_row_key() {
        let mut a = MenuItem { id: 5, key: "windows.verb:share".into(), ..MenuItem::command(ItemSource::Native, "Share", "") };
        let k = item_key(&a);
        a.label = "Teilen".into();
        assert_eq!(item_key(&a), k, "focus stays on a renamed row");
        let b = MenuItem::command(ItemSource::Native, "Share", "");
        assert_ne!(item_key(&b), item_key(&MenuItem::command(ItemSource::Native, "Teilen", "")), "unkeyed rows still tell labels apart");
    }

    fn test_menu(native: Vec<MenuItem>) -> CtxMenu {
        let target = Target { paths: vec![r"C:\x\a.txt".into()], folder: r"C:\x".into(), ..Default::default() };
        let menu = ContextMenu { entries: default_entries(), ..Default::default() };
        let (ts, user) = own_parts(&target, &menu.entries, false);
        let mut model = MenuModel::new(2, ts, user);
        model.native = native;
        let mut c = CtxMenu {
            generation: 2, target, model, menu, edited: false, learned: false, tag_new: false, settings_key: None, undo: None, undo_entries: None, settings_edit: false, dual: false,
            handle: None, ticket: 0, state: MenuState::default(), open: true,
            input_at: Instant::now(), asked_at: Instant::now(), grace: Duration::ZERO, revealed: true, first_paint: None, top_items: None, all_items: None, subs: SubLoads::default(), sub_latency: None, clock: Instant::now(),
            picked: None, retry: false, pending_verb: None, invoked: false, invoke_result: None,
            items: vec![], dirty: true, paste_ok: true, icon_px: 16, a11y: A11yOut::default(), hc: None, reserve: [0.0; 2], first_rect: None,
        };
        c.items = c.model.rows(&c.menu);
        c
    }

    fn row_of(c: &CtxMenu, label: &str) -> u64 {
        item_key(c.items.iter().find(|i| i.label == label).unwrap_or_else(|| panic!("{label} in {:?}", labels(&c.items))))
    }

    fn refresh(c: &mut CtxMenu) {
        c.items = c.model.rows(&c.menu);
    }

    #[test]
    fn hide_rename_and_undo_from_the_menu_edit_the_settings_copy() {
        let share = MenuItem { id: 9, key: "windows.verb:share".into(), verb: Some("share".into()), ..MenuItem::command(ItemSource::Native, "Share", "") };
        let mut c = test_menu(vec![share]);
        // hide a Windows item: a pref is made, the row goes
        let k = row_of(&c, "Share");
        c.edit(&[k], EDIT_HIDE);
        refresh(&mut c);
        assert!(c.edited && !labels(&c.items).contains(&"Share"));
        let p = menu::item(&c.menu.items, "windows.verb:share").unwrap();
        assert!(p.hidden && p.label == "Share" && p.from == "Windows");
        c.undo_hide();
        refresh(&mut c);
        assert!(labels(&c.items).contains(&"Share"), "Undo shows it again");
        // rename: the same row key, a new label; empty restores
        c.rename(&[k], "Send");
        refresh(&mut c);
        assert_eq!(c.items.iter().find(|i| item_key(i) == k).unwrap().label, "Send");
        c.rename(&[k], "");
        refresh(&mut c);
        assert!(labels(&c.items).contains(&"Share"));
        // a built-in: its list entry changes
        let copy = row_of(&c, "Copy");
        c.rename(&[copy], "Duplicate");
        c.edit(&[row_of(&c, "Cut")], EDIT_HIDE);
        refresh(&mut c);
        assert!(labels(&c.items).contains(&"Duplicate") && !labels(&c.items).contains(&"Cut"));
        assert!(c.menu.entries.iter().find(|e| e.id == "builtin.cut").unwrap().hidden);
        c.rename(&[copy], "");
        refresh(&mut c);
        assert!(labels(&c.items).contains(&"Copy"), "an empty name gives a built-in its default name back");
        // an app row (Copy name) hides through a pref too
        c.edit(&[row_of(&c, "Copy name")], EDIT_HIDE);
        refresh(&mut c);
        assert!(!labels(&c.items).contains(&"Copy name"));
        assert!(menu::item(&c.menu.items, "app:x.copy-name").is_some_and(|p| p.hidden && p.from == "Tsubame"));
        // Edit in Settings: the key goes to the app, nothing else changes
        c.edit(&[k], EDIT_SETTINGS);
        assert_eq!(c.settings_key.as_deref(), Some("windows.verb:share"));
    }

    #[test]
    fn pin_to_top_moves_a_native_row_first_and_unpin_restores_it() {
        let share = MenuItem { id: 9, key: "windows.verb:share".into(), ..MenuItem::command(ItemSource::Native, "Share", "") };
        let seven = MenuItem { id: 10, key: "windows.label:7-zip".into(), ..MenuItem::command(ItemSource::Native, "7-Zip", "") };
        let mut c = test_menu(vec![share, seven]);
        let nat = |c: &CtxMenu| labels(&c.items).into_iter().filter(|l| *l == "Share" || *l == "7-Zip").map(str::to_string).collect::<Vec<String>>();
        assert_eq!(nat(&c), ["Share", "7-Zip"]);
        let k = row_of(&c, "7-Zip");
        c.edit(&[k], EDIT_PIN);
        refresh(&mut c);
        assert!(c.edited && menu::item(&c.menu.items, "windows.label:7-zip").is_some_and(|p| p.pin));
        assert_eq!(nat(&c), ["7-Zip", "Share"]);
        // the edit menu now offers Unpin
        let icons = IconCache::new(2);
        let subs = SubLoads::default();
        let src = Rows { items: &c.items, icons: &icons, failed: false, paste_ok: true, subs: &subs, now: 0, can_load: true, menu: Some(&c.menu) };
        let mut out = Vec::new();
        src.edit_rows(&[k], &mut out);
        assert_eq!(out[3].label, "Unpin");
        c.edit(&[k], EDIT_PIN);
        refresh(&mut c);
        assert_eq!(nat(&c), ["Share", "7-Zip"]);
        // a list row cannot pin: nothing is saved
        c.edited = false;
        let before = c.menu.clone();
        c.edit(&[row_of(&c, "Copy")], EDIT_PIN);
        assert!(!c.edited && c.menu == before);
    }

    #[test]
    fn refresh_forgets_untouched_items_the_last_menu_did_not_show() {
        let mut m = ContextMenu {
            items: vec![
                menu::ItemPref { key: "windows.verb:gone".into(), ..Default::default() },
                menu::ItemPref { key: "windows.verb:kept".into(), ..Default::default() },
                menu::ItemPref { key: "windows.verb:hid".into(), hidden: true, ..Default::default() },
            ],
            ..Default::default()
        };
        // no menu shown yet: nothing changes
        assert_eq!(refresh_learned(&mut m), 0);
        *LAST_SHOWN.lock().unwrap() = vec!["windows.verb:kept".to_string()];
        assert_eq!(refresh_learned(&mut m), 1);
        assert_eq!(m.items.len(), 2);
        *LAST_SHOWN.lock().unwrap() = Vec::new();
    }

    #[test]
    fn move_up_and_down_work_on_own_rows_only() {
        let share = MenuItem { id: 9, key: "windows.verb:share".into(), ..MenuItem::command(ItemSource::Native, "Share", "") };
        let mut c = test_menu(vec![share]);
        let l = labels(&c.items);
        let (cut, copy) = (l.iter().position(|s| *s == "Cut").unwrap(), l.iter().position(|s| *s == "Copy").unwrap());
        assert!(cut < copy);
        c.edit(&[row_of(&c, "Copy")], EDIT_UP);
        refresh(&mut c);
        let l = labels(&c.items);
        assert!(l.iter().position(|s| *s == "Copy").unwrap() < l.iter().position(|s| *s == "Cut").unwrap(), "{l:?}");
        // Windows' items keep their order: nothing happens
        let before = c.menu.clone();
        c.edited = false;
        c.edit(&[row_of(&c, "Share")], EDIT_DOWN);
        assert!(!c.edited && c.menu == before);
        // the first own row cannot move up (Open is first; Open in new tab is not shown for files)
        c.edit(&[row_of(&c, "Open")], EDIT_UP);
        assert!(!c.edited);
    }

    #[test]
    fn edit_rows_follow_the_mock() {
        use tsubame_ui::widgets::menu::EDIT_RENAME;
        let share = MenuItem { id: 9, key: "windows.verb:share".into(), ..MenuItem::command(ItemSource::Native, "Share", "") };
        let mut pk = MenuItem { id: 0xA000, key: "windows.pkg:Microsoft.WindowsTerminal_8wekyb3d8bbwe/{9f15}".into(), ..MenuItem::command(ItemSource::Native, "Open in Terminal", "") };
        pk.verb = Some("{9f15}".into());
        let c = test_menu(vec![share, pk]);
        let icons = IconCache::new(2);
        let subs = SubLoads::default();
        let src = Rows { items: &c.items, icons: &icons, failed: false, paste_ok: true, subs: &subs, now: 0, can_load: true, menu: Some(&c.menu) };
        let rows = |label: &str| {
            let mut out = Vec::new();
            src.edit_rows(&[row_of(&c, label)], &mut out);
            out
        };
        // a Windows row: Hide (Del), Rename (F2), Pin and Move to group enabled, Move up / down disabled, the reason note
        let r = rows("Share");
        assert_eq!(r[0].label, "Hide \u{201c}Share\u{201d}");
        assert_eq!((r[0].key, r[0].shortcut.as_str()), (EDIT_HIDE, "Del"));
        assert_eq!((r[1].key, r[1].shortcut.as_str()), (EDIT_RENAME, "F2"));
        assert_eq!((r[3].key, r[3].label.as_str(), r[3].disabled), (EDIT_PIN, "Pin to top", false));
        assert!(r[4].key == EDIT_GROUP && r[4].submenu && !r[4].disabled);
        assert!(r[5].disabled && r[6].disabled, "Windows' items do not move");
        assert_eq!(r[8].key, EDIT_SETTINGS);
        assert_eq!((r[9].kind, r[9].label.as_str()), (RowKind::Note, rowmenu::NOTE_LEARNED));
        // a package item: same list
        assert_eq!(rows("Open in Terminal").len(), r.len());
        // a built-in: Pin and groups disabled (with the Tsubame note), Move up / down enabled
        let own = rows("Copy");
        assert!(!own[5].disabled && !own[6].disabled, "a built-in moves");
        assert!(own[3].disabled && own[4].disabled, "list rows move instead of pinning");
        assert_eq!((own[9].kind, own[9].label.as_str()), (RowKind::Note, rowmenu::NOTE_OWN));
        assert!(rows("Open")[5].disabled, "the first row cannot move up");
        // the flyout: Move to group lists the user groups, New group and the native place
        let mut fly = Vec::new();
        src.edit_rows(&[row_of(&c, "Share"), EDIT_GROUP], &mut fly);
        assert!(fly.iter().any(|x| x.key == EDIT_GROUP_NEW) && fly.iter().any(|x| x.key == EDIT_GROUP_NATIVE && x.radio));
        // no menu settings: no edit menu
        let plain = Rows { menu: None, ..src };
        let mut out = Vec::new();
        plain.edit_rows(&[row_of(&c, "Share")], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn move_to_group_from_the_menu_makes_groups_and_pins_nothing_else() {
        let share = MenuItem { id: 9, key: "windows.verb:share".into(), ..MenuItem::command(ItemSource::Native, "Share", "") };
        let mut c = test_menu(vec![share]);
        let k = row_of(&c, "Share");
        // New group: a group is made and the row is in it
        c.edit(&[k], EDIT_GROUP_NEW);
        assert!(c.edited && c.menu.user_groups.len() == 1);
        let gid = c.menu.user_groups[0].id.clone();
        assert_eq!(menu::item(&c.menu.items, "windows.verb:share").map(|p| p.group.clone()), Some(gid.clone()));
        // the flyout now marks it; Native place sends the row home
        let facts = c.facts_at(&[k]).unwrap();
        assert_eq!(facts.group, gid);
        c.edit(&[k], EDIT_GROUP_NATIVE);
        assert!(menu::item(&c.menu.items, "windows.verb:share").is_some_and(|p| p.group.is_empty()));
        // a list row cannot change group: nothing happens
        c.edited = false;
        let before = c.menu.clone();
        c.edit(&[row_of(&c, "Copy")], EDIT_GROUP_BASE);
        assert!(!c.edited && c.menu == before);
    }

    #[test]
    fn delete_entry_removes_a_custom_row_and_undo_brings_it_back() {
        let mut c = test_menu(vec![]);
        let mut e = MenuEntry::custom("custom.1", "Open in VS Code", "code.exe", "");
        e.show_for = menu::ShowFor::default();
        c.menu.entries.push(e);
        c.rebuild_own();
        refresh(&mut c);
        let k = row_of(&c, "Open in VS Code");
        // Edit entry opens Settings on the entry's form
        c.edit(&[k], EDIT_ENTRY);
        assert!(c.settings_edit && c.settings_key.as_deref() == Some("custom.1"));
        c.settings_key = None;
        c.settings_edit = false;
        c.edit(&[k], EDIT_DELETE);
        refresh(&mut c);
        assert!(c.edited && !labels(&c.items).contains(&"Open in VS Code"));
        assert!(c.menu.entries.iter().all(|e| e.id != "custom.1"));
        c.undo_hide();
        refresh(&mut c);
        assert!(labels(&c.items).contains(&"Open in VS Code") && c.menu.entries.iter().any(|e| e.id == "custom.1"));
        // Delete entry is only for custom rows: a built-in stays
        c.edited = false;
        let before = c.menu.clone();
        c.edit(&[row_of(&c, "Copy")], EDIT_DELETE);
        assert!(!c.edited && c.menu == before);
    }

    #[test]
    fn batches_teach_settings_new_items_once() {
        let mut c = test_menu(vec![]);
        // the first menu ever tags nothing New
        c.learn_from(&c.model.tsubame.clone());
        assert!(c.learned && c.menu.items.iter().all(|p| !p.new));
        assert!(menu::item(&c.menu.items, "app:x.copy-name").is_some());
        c.learned = false;
        let g = c.generation;
        let share = MenuItem { id: 9, key: "windows.verb:share".into(), ..MenuItem::command(ItemSource::Native, "Share", "") };
        let edit = MenuItem { id: 10, key: "windows.verb:edit".into(), ..MenuItem::command(ItemSource::Native, "Edit", "") };
        // the shell's batches of that same first menu tag nothing New either
        c.apply(MenuBatch { generation: g, part: Part::Native(vec![edit.clone()]), timing: [0; 3] });
        assert!(c.learned && menu::item(&c.menu.items, "windows.verb:edit").is_some_and(|p| !p.new));
        // a later menu (Settings knew items when it opened) tags new ones
        c.tag_new = !c.menu.items.is_empty();
        c.learned = false;
        c.apply(MenuBatch { generation: g, part: Part::Native(vec![edit, share.clone()]), timing: [0; 3] });
        assert!(c.learned && menu::item(&c.menu.items, "windows.verb:share").is_some_and(|p| p.new));
        c.learned = false;
        c.apply(MenuBatch { generation: g, part: Part::Native(vec![share]), timing: [0; 3] });
        assert!(!c.learned, "known items do not touch the config");
    }

    #[test]
    fn copy_and_copy_name_have_different_glyphs() {
        assert_ne!(glyph("file.copy", None), glyph("x.copy-name", None));
        assert!(glyph("x.copy-name", None).is_some());
    }

    #[test]
    fn lazy_submenus_say_loading_then_fail_when_slow_or_orphaned() {
        let lazy = MenuItem { kind: ItemKind::Submenu, flags: flag::LAZY, sub: 3, ..MenuItem::command(ItemSource::Native, "Send to", "") };
        let mut nested = MenuItem { kind: ItemKind::Submenu, ..MenuItem::command(ItemSource::Native, "7-Zip", "") };
        nested.children.push(MenuItem { kind: ItemKind::Submenu, flags: flag::LAZY, sub: 4, ..MenuItem::command(ItemSource::Native, "More", "") });
        let items = vec![lazy.clone(), nested];
        let mut got = lazy_subs(&items);
        got.sort();
        assert_eq!(got, [3, 4], "the filter asks for every lazy submenu");
        let icons = IconCache::new(4);
        let mut subs = SubLoads::default();
        let path = [item_key(&lazy)];
        let rows = |subs: &SubLoads, now: u64, can_load: bool| Rows { items: &items, icons: &icons, failed: false, paste_ok: true, subs, now, can_load, menu: None }.sub_state(&path);
        assert_eq!(rows(&subs, 0, true), SubLoad::Loading, "about to be asked");
        assert!(subs.want(3, 0));
        assert_eq!(rows(&subs, ctxmodel::SLOW_MS - 1, true), SubLoad::Loading);
        assert_eq!(rows(&subs, ctxmodel::SLOW_MS, true), SubLoad::Failed, "a hung handler: Retry");
        assert_eq!(rows(&subs, 0, false), SubLoad::Failed, "the worker is gone");
        subs.done(3);
        let filled = Rows { items: &items, icons: &icons, failed: false, paste_ok: true, subs: &subs, now: 0, can_load: true, menu: None };
        assert_eq!(filled.sub_state(&[12345]), SubLoad::Ready, "unknown paths are not loading");
        let mut eager = lazy.clone();
        eager.flags = 0;
        let items2 = [eager.clone()];
        let r2 = Rows { items: &items2, icons: &icons, failed: false, paste_ok: true, subs: &subs, now: 0, can_load: true, menu: None };
        assert_eq!(r2.sub_state(&[item_key(&eager)]), SubLoad::Ready, "a filled submenu is ready (even if empty)");
    }

    #[test]
    fn widget_snapshot_maps_to_the_automation_tree() {
        use tsubame_ui::widgets::menu::{A11yLevel, A11yRow};
        let row = |key, name: &str| A11yRow { key, name: name.into(), enabled: true, visible: true, ..Default::default() };
        let mut copy = row(2, "Copy");
        copy.accel = Some('c');
        copy.shortcut = "Ctrl+C".into();
        copy.checked = true;
        let mut send = row(9, "Send to");
        send.submenu = true;
        send.expanded = true;
        let snap = A11ySnap {
            levels: vec![
                A11yLevel { key: 0, rect: Rect::new(10.0, 20.0, 200.0, 100.0), rows: vec![copy, send], filter: None },
                A11yLevel { key: 9, rect: Rect::new(210.0, 20.0, 100.0, 50.0), rows: vec![row(91, "Desktop")], filter: Some((Rect::new(1.0, 2.0, 3.0, 4.0), "d".into())) },
            ],
        };
        let u = a11y_snap(&snap);
        assert_eq!(u.levels[0].name, "Context menu");
        assert_eq!(u.levels[1].name, "Send to", "a submenu is named after its row");
        assert_eq!(u.levels[0].rows[0].access, "Alt+C");
        assert_eq!(u.levels[0].rows[0].shortcut, "Ctrl+C");
        assert!(u.levels[0].rows[0].toggled && u.levels[0].rows[1].submenu && u.levels[0].rows[1].expanded);
        assert_eq!(u.levels[1].filter, Some(([1.0, 2.0, 3.0, 4.0], "d".into())));
        assert_eq!(u.levels[1].rect, [210.0, 20.0, 100.0, 50.0]);
    }

    #[test]
    fn high_contrast_uses_system_colours_legibly() {
        use tsubame_ui::themes::contrast;
        // the four Windows High Contrast themes (COLOR_WINDOW, WINDOWTEXT, HIGHLIGHT, HIGHLIGHTTEXT, GRAYTEXT)
        let sets: [[u32; 5]; 4] = [
            [0x000000, 0xffffff, 0x1aebff, 0x000000, 0x3ff23f], // HC Black
            [0xffffff, 0x000000, 0x37006e, 0xffffff, 0x600000], // HC White
            [0x202020, 0xffffff, 0x8ca2ff, 0x000000, 0x3ff23f], // HC #1
            [0x000000, 0xffffff, 0x800080, 0xffffff, 0x00ff00], // HC #2
        ];
        for [w, wt, h, ht, gray] in sets {
            let sys = |i: i32| match i {
                5 => w,
                8 => wt,
                13 => h,
                14 => ht,
                17 => gray,
                _ => 0x808080,
            };
            let t = hc_colours(&tsubame_ui::DARK, sys);
            assert!(contrast(t.text, t.popup) >= 7.0, "row text on the menu");
            assert!(contrast(t.on_accent, t.selected) >= 7.0, "text on the selected row");
            assert!(contrast(t.text_disabled, t.popup) >= 3.0, "disabled text stays readable");
            assert_eq!(t.selected, t.accent, "the widget takes on_accent text on an accent fill");
            assert_eq!(t.text_dim, t.text, "no dim text");
        }
    }

    #[test]
    fn grace_window_paints_complete_menu_in_one_frame_without_loading_flash() {
        let mut c = test_menu(vec![]);
        c.grace = Duration::from_millis(200);
        c.revealed = false;
        c.asked_at = Instant::now();
        c.input_at = c.asked_at;
        c.model.complete = false;
        c.model.state = LoadState::Loading;

        let mut ui = Ui::new(tsubame_ui::DARK);
        let mut g = tsubame_ui::TestCanvas::new(800.0, 600.0);
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);

        // Inside the grace window: held back, one-shot timer for remaining grace.
        assert!(!c.is_revealed());
        let tick = c.next_tick_ms().expect("one-shot grace timer armed");
        assert!((1..=200).contains(&tick), "unexpected grace timer: {tick}");
        ui.begin_frame((800.0, 600.0));
        c.paint(&mut ui, &mut g, bounds);
        let _ = ui.end_frame();
        c.presented();
        assert!(g.ops.is_empty(), "menu must not paint while waiting in grace window");
        assert!(c.first_paint.is_none());

        // Intermediate Native batch inside grace updates model without forcing a partial paint.
        let generation = c.generation;
        let share = MenuItem { id: 9, key: "windows.verb:share".into(), verb: Some("share".into()), ..MenuItem::command(ItemSource::Native, "Share", "") };
        assert!(!c.apply(MenuBatch { generation, part: Part::Native(vec![share]), timing: [0; 3] }));
        assert!(!c.is_revealed());

        // Complete arrives within grace: reveals immediately, no loading line, no timer.
        assert!(c.apply(MenuBatch { generation, part: Part::Complete, timing: [0; 3] }));
        assert!(c.is_revealed());
        assert!(!c.loading());
        assert_eq!(c.next_tick_ms(), None, "complete menu arms zero timers");

        ui.begin_frame((800.0, 600.0));
        c.paint(&mut ui, &mut g, bounds);
        let _ = ui.end_frame();
        c.presented();
        assert!(!g.ops.is_empty(), "complete menu paints in one crisp frame");
        assert!(c.first_paint.is_some() && c.top_items.is_some() && c.all_items.is_some());
    }

    #[test]
    fn grace_window_falls_back_to_loading_when_shell_is_slow() {
        let mut c = test_menu(vec![]);
        c.grace = Duration::from_millis(60);
        c.revealed = false;
        c.asked_at = Instant::now() - Duration::from_millis(80);
        c.input_at = c.asked_at;
        c.model.complete = false;
        c.model.state = LoadState::Loading;

        // Grace expired while shell is still loading -> reveal own rows; the
        // loading line is not up yet (LOADING_SHOW_MS), so one wake-up for its start.
        assert!(c.is_revealed());
        assert!(c.loading());
        let first = c.next_tick_ms().expect("loading line start armed");
        assert!((1..=(LOADING_SHOW_MS as u32 - 80 + 5)).contains(&first), "wake-up for the line's start, not a 10 FPS poll: {first}");

        let mut ui = Ui::new(tsubame_ui::DARK);
        let mut g = tsubame_ui::TestCanvas::new(800.0, 600.0);
        ui.begin_frame((800.0, 600.0));
        c.paint(&mut ui, &mut g, Rect::new(0.0, 0.0, 800.0, 600.0));
        let _ = ui.end_frame();
        c.presented();
        assert!(!g.ops.is_empty(), "own rows paint after grace expires");
        assert!(c.first_paint.is_some());
        assert!(c.all_items.is_none());

        // Once Complete arrives later, spinner stops and zero timers remain.
        let generation = c.generation;
        assert!(c.apply(MenuBatch { generation, part: Part::Complete, timing: [0; 3] }));
        assert!(!c.loading());
        assert_eq!(c.next_tick_ms(), None);
    }

    #[test]
    fn loading_line_animates_smoothly_only_while_visible() {
        let mut c = test_menu(vec![]);
        c.grace = Duration::ZERO;
        c.revealed = true;
        c.model.complete = false;
        c.model.state = LoadState::Loading;
        // before the line shows: a single wake-up for its start, never a poll
        c.asked_at = Instant::now();
        assert!(c.next_tick_ms().unwrap() > ANIM_MS, "no animation ticks while the line is hidden");
        // while it shows: ~30 FPS
        c.asked_at = Instant::now() - Duration::from_millis(LOADING_SHOW_MS as u64 + 50);
        assert_eq!(c.next_tick_ms(), Some(ANIM_MS));
        const { assert!(ANIM_MS <= 34) };
        // done: no timer
        c.model.complete = true;
        assert_eq!(c.next_tick_ms(), None);
    }

    #[test]
    fn zero_grace_reveals_immediately_with_loading_start_tick() {
        let mut c = test_menu(vec![]);
        c.grace = Duration::ZERO;
        c.revealed = true;
        c.model.complete = false;
        c.model.state = LoadState::Loading;

        assert!(c.is_revealed());
        assert!(c.loading());
        assert!(c.next_tick_ms().is_some());
    }

    #[test]
    fn cold_first_menu_uses_the_longer_hold_and_warm_uses_reveal_grace() {
        if std::env::var_os("TSUBAME_CTX_GRACE_MS").is_none() {
            assert_eq!(grace_for(true), Duration::from_millis(COLD_GRACE_MS));
            assert_eq!(grace_for(false), Duration::from_millis(REVEAL_GRACE_MS));
        }
    }

    fn shell_rows(n: u32) -> Vec<MenuItem> {
        (0..n).map(|i| MenuItem { id: 10 + i, key: format!("windows.verb:v{i}"), verb: Some(format!("v{i}")), ..MenuItem::command(ItemSource::Native, &format!("Shell verb {i}"), "") }).collect()
    }

    fn paint_frame(c: &mut CtxMenu, ui: &mut Ui, g: &mut tsubame_ui::TestCanvas, bounds: Rect) -> Rect {
        ui.begin_frame((bounds.w, bounds.h));
        c.paint(ui, g, bounds);
        let _ = ui.end_frame();
        c.presented();
        c.state.levels[0].rect
    }

    /// Opens a menu revealed at once (cold, own rows only), paints it, then
    /// delivers `n` shell rows with Complete and paints again. Returns the
    /// rect at first reveal and the final one.
    fn reveal_then_fill(anchor: Pt, background: bool, n: u32, reserve_w: f32, reserve: f32, bounds: Rect) -> (Rect, Rect) {
        let mut c = test_menu(vec![]);
        c.target.background = background;
        c.target.anchor = anchor;
        c.state = MenuState::new(anchor, false);
        c.reserve = [reserve_w, reserve];
        c.grace = Duration::ZERO;
        c.revealed = true;
        c.model.complete = false;
        c.model.state = LoadState::Loading;
        let mut ui = Ui::new(tsubame_ui::DARK);
        let mut g = tsubame_ui::TestCanvas::new(bounds.w, bounds.h);
        let first = paint_frame(&mut c, &mut ui, &mut g, bounds);
        let generation = c.generation;
        c.apply(MenuBatch { generation, part: Part::Native(shell_rows(n)), timing: [0; 3] });
        c.apply(MenuBatch { generation, part: Part::Complete, timing: [0; 3] });
        let last = paint_frame(&mut c, &mut ui, &mut g, bounds);
        (first, last)
    }

    #[test]
    fn menu_rect_is_identical_at_first_reveal_and_when_the_shell_rows_arrive() {
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        // anchors: free space, right edge, bottom edge, bottom-right corner
        let anchors = [Pt::new(100.0, 60.0), Pt::new(790.0, 60.0), Pt::new(100.0, 590.0), Pt::new(795.0, 595.0), Pt::new(400.0, 300.0)];
        for background in [true, false] {
            for anchor in anchors {
                // the complete size is what the previous menu of this kind remembered
                let (_, done) = reveal_then_fill(anchor, background, 8, 0.0, 0.0, bounds);
                let (first, last) = reveal_then_fill(anchor, background, 8, done.w, done.h, bounds);
                assert_eq!(first, last, "bg={background} anchor={anchor:?}: no resize or move when rows arrive");
                assert_eq!(last, done, "bg={background} anchor={anchor:?}: same as the menu without a reveal-early step");
            }
        }
    }

    #[test]
    fn without_a_reserve_the_menu_would_jump_near_the_bottom() {
        // the control: why the reserve exists (own rows fit below the anchor, the full menu does not)
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        let (first, last) = reveal_then_fill(Pt::new(100.0, 400.0), true, 12, 0.0, 1.0, bounds);
        assert_ne!(first, last, "own rows alone open downwards, the full menu flips up");
    }

    #[test]
    fn default_reserve_keeps_the_flip_choice_for_a_typical_shell_block() {
        // no memory of an earlier menu: the estimate (own rows + a typical shell block)
        // decides the flip, so a menu near the bottom opens up and stays put
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        let (first, last) = reveal_then_fill(Pt::new(100.0, 500.0), true, 8, 0.0, 0.0, bounds);
        assert_eq!(first.x, last.x);
        assert!(first.b() <= 592.0 && last.b() <= 592.0, "both stay inside the bounds");
        assert!(first.y < 500.0, "opened upwards from the start: {first:?}");
    }

    #[test]
    fn warm_state_decides_whether_a_fresh_menu_is_held() {
        // a pre-warmed (or pending) shell is held for; a cold one is shown at once
        WARM.store(WARM_NONE, Ordering::Relaxed);
        if !SHELL_WARMED.load(Ordering::Relaxed) {
            assert!(!worth_holding());
        }
        WARM.store(WARM_PENDING, Ordering::Relaxed);
        assert!(worth_holding());
        note_warm_done();
        assert!(shell_warm() && worth_holding());
        WARM.store(WARM_NONE, Ordering::Relaxed);
    }
}
