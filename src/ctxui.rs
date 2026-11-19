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
use std::time::{Duration, Instant};
use tsubame_config::menu::{MenuEntry, Target as Kind};
use tsubame_ui::widgets::menu::{SubLoad, menu};
use tsubame_ui::{Canvas, Id, MenuRow, MenuSource, MenuState, MenuStyle, Pt, Rect, RowIcon, RowKind, Ui};
use tsubame_win::shell::ctxmodel::{self, IconCache, ItemKind, ItemSource, LoadState, MenuBatch, MenuItem, MenuModel, Part, Route, SubLoads, SubState, flag};
use tsubame_win::shell::{CtxHandle, CtxOpts, Pool, Request};

pub const ID_CTX: Id = Id::new("ctxmenu");
/// The loading line's animation tick (only while a menu is loading).
pub const TIMER_ID: usize = 11;
pub const TICK_MS: u32 = 33;
const SPIN_MS: u128 = 1200;
pub const FAIL_TEXT: &str = "Some items could not load";
const STATUS_KEY: u64 = 0x0057_a7a5_0001;

thread_local! {
    static INPUT_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    static NEXT_GEN: Cell<u64> = const { Cell::new(1) };
}

/// The right button went down / the menu key was pressed: the menu's
/// "input to first paint" clock starts here.
pub fn note_input() {
    INPUT_AT.with(|c| c.set(Some(Instant::now())));
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

/// `TSUBAME_NATIVE_MENU=1` keeps the Windows popup menu (the fallback).
pub fn own_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("TSUBAME_NATIVE_MENU").map(|v| v.trim() != "1").unwrap_or(true))
}

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
    pub entries: Vec<MenuEntry>,
    handle: Option<CtxHandle>,
    /// the pool job (the watchdog reports it when it hangs)
    pub ticket: u64,
    pub state: MenuState,
    /// still on screen
    pub open: bool,
    input_at: Instant,
    asked_at: Instant,
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
}

/// The right-clicked kind and extension for the U4 rules.
pub fn kind_of(target: &Target) -> (Kind, String) {
    if target.background {
        return (Kind::Background, String::new());
    }
    let first = target.paths.first().map(String::as_str).unwrap_or("");
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
pub fn extras(kind: Kind, n: usize, dual: bool) -> Vec<MenuItem> {
    let c = |l: &str, a: &str| MenuItem::command(ItemSource::Tsubame, l, a);
    match kind {
        Kind::Background => vec![c("Paste", "file.paste"), c("New folder", "x.new-folder"), c("New text file", "x.new-file"), c("Copy as path", "file.copy-path"), c("Open terminal here", "x.terminal"), c("Refresh", "x.refresh")],
        _ => {
            let mut v = vec![c("Copy name", "x.copy-name"), c("Open terminal here", "x.terminal")];
            if dual && n == 1 && kind == Kind::Folder {
                v.insert(0, c("Open in other pane", "x.other-pane"));
            }
            v
        }
    }
}

/// The Tsubame group for a click: the U4 built-ins + escape hatch + extras.
pub fn tsubame_rows(entries: &[MenuEntry], kind: Kind, ext: &str, n: usize, dual: bool) -> Vec<MenuItem> {
    let mut rows = ctxmodel::own_rows(entries, kind, ext, &extras(kind, n, dual));
    // a new tab only opens folders
    if kind != Kind::Folder {
        rows.retain(|r| r.action != "tab.open-selected");
    }
    for r in &mut rows {
        if r.action == "file.open" {
            r.flags |= flag::DEFAULT;
        }
    }
    rows
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
    pub fn open(target: Target, entries: Vec<MenuEntry>, dual: bool, paste_ok: bool, icon_px: u32, pool: &mut Pool) -> CtxMenu {
        let (kind, ext) = kind_of(&target);
        let mut tsubame = tsubame_rows(&entries, kind, &ext, target.paths.len(), dual);
        if target.paths.iter().any(|p| is_root(p)) && !target.background {
            for row in &mut tsubame {
                if matches!(row.action.as_str(), "file.cut" | "file.delete" | "file.rename") { row.flags |= flag::DISABLED; }
            }
        }
        let user = ctxmodel::own_items(&entries, kind, &ext).1;
        let generation = next_gen();
        let state = MenuState::new(target.anchor, target.keyboard);
        let mut m = CtxMenu {
            generation,
            model: MenuModel::new(generation, tsubame, user),
            target,
            entries,
            handle: None,
            ticket: 0,
            state,
            open: true,
            input_at: take_input(),
            asked_at: Instant::now(),
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
        };
        m.submit(pool);
        m
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
                    if self.pending_verb.take().is_some() {
                        self.invoke_result = Some(Err(error.clone()));
                    }
                }
                if let Part::Submenu { sub, .. } = part
                    && let Some(at) = self.subs.done(sub)
                {
                    self.sub_latency = Some(Duration::from_millis(self.now_ms().saturating_sub(at)));
                }
                let native = matches!(part, Part::Native(_));
                self.model.apply(MenuBatch { part, ..b });
                self.dirty = true;
                if native && let Some(v) = self.pending_verb.take() {
                    self.invoke_verb(&v);
                }
            }
        }
        self.open
    }

    /// The watchdog gave the worker up.
    pub fn lost(&mut self) {
        let g = self.generation;
        self.model.apply(MenuBatch { generation: g, part: Part::Failed("the shell did not answer".into()), timing: [0; 3] });
        self.handle = None;
        self.pending_verb = None;
        // the worker is gone: its command ids and submenu tokens are dead
        // (open submenus show the failure row, Retry asks a new worker)
        self.model.native.clear();
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

    /// The menu's tick timer must run: the loading line, a submenu that is
    /// loading (until it turns into the failure row) or a hover delay.
    /// False while the menu just sits there: no wake-ups.
    pub fn ticking(&self) -> bool {
        self.open && (self.loading() || self.subs.loading(self.now_ms()) || self.state.deadline().is_some())
    }

    /// Draws the menu (after every other widget).
    pub fn paint(&mut self, ui: &mut Ui, g: &mut dyn Canvas, bounds: Rect) {
        if !self.open {
            return;
        }
        if self.dirty {
            self.items = self.model.rows(&self.entries);
            self.dirty = false;
        }
        let loading = self.loading();
        let phase = loading.then(|| (self.asked_at.elapsed().as_millis() % SPIN_MS) as f32 / SPIN_MS as f32);
        let alt = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x12) } < 0;
        let now = self.now_ms();
        let style = MenuStyle { bounds, loading: phase, alt, now_ms: now };
        let failed = self.shown() == LoadState::Failed;
        let src = Rows { items: &self.items, icons: &self.model.icons, failed, paste_ok: self.paste_ok, subs: &self.subs, now, can_load: self.handle.is_some() };
        let r = menu(ui, g, ID_CTX, &mut self.state, &src, &style);
        if let Some(path) = r.picked {
            self.picked = src.pick(&path);
        }
        // CM6: ask the worker for the submenus the menu shows or is about to
        // open; while filtering, for every submenu (their rows are searched)
        let want = if self.state.filtering() { lazy_subs(&self.items) } else { self.state.wanted().iter().filter_map(|p| src.item_at(p)).filter(|it| ctxmodel::is_lazy(it)).map(|it| it.sub).collect() };
        if r.retry {
            self.retry = true;
        }
        if r.closed {
            self.open = false;
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
    }

    /// Called after Canvas::end, so timings include submitting the frame.
    pub fn presented(&mut self) {
        if !self.open { return; }
        self.first_paint.get_or_insert_with(|| self.input_at.elapsed());
        if self.model.state == LoadState::Ready {
            self.top_items.get_or_insert_with(|| self.input_at.elapsed());
        }
        if self.model.complete && self.model.error.is_none() {
            self.all_items.get_or_insert_with(|| self.input_at.elapsed());
        }
    }

    /// One line for the log / status: the menu's timings.
    pub fn timing_line(&self) -> String {
        let ms = |d: Option<Duration>| d.map_or("-".to_string(), |d| format!("{:.1}", d.as_secs_f64() * 1000.0));
        format!("own menu: first paint {} ms, top level {} ms, all items {} ms, {} rows", ms(self.first_paint), ms(self.top_items), ms(self.all_items), self.items.len())
    }
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
    eat(it.label.as_bytes());
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
        "file.rename" => '\u{E8AC}',
        "file.delete" => '\u{E74D}',
        "file.properties" => '\u{E946}',
        ctxmodel::ACTION_CLASSIC => '\u{E712}',
        "x.new-folder" => '\u{E8F4}',
        "x.new-file" => '\u{E8A5}',
        "x.terminal" => '\u{E756}',
        "x.refresh" => '\u{E72C}',
        _ => match verb? {
            "open" => '\u{E8E5}',
            "openas" | "openwith" => '\u{E7AC}',
            "runas" => '\u{E7EF}',
            "share" | "windows.share" => '\u{E72D}',
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

impl Rows<'_> {
    fn level(&self, path: &[u64]) -> Option<&[MenuItem]> {
        let mut level = self.items;
        for &k in path {
            level = &level.iter().find(|it| it.kind == ItemKind::Submenu && item_key(it) == k)?.children;
        }
        Some(level)
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
        }
    }
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
    fn failed_pending_verb_finishes_and_stale_failure_does_not() {
        let mut c = CtxMenu {
            generation: 2, target: Target::default(), model: MenuModel::new(2, vec![], vec![]), entries: vec![],
            handle: None, ticket: 0, state: MenuState::default(), open: false,
            input_at: Instant::now(), asked_at: Instant::now(), first_paint: None, top_items: None, all_items: None, subs: SubLoads::default(), sub_latency: None, clock: Instant::now(),
            picked: None, retry: false, pending_verb: Some("properties".into()), invoked: false, invoke_result: None,
            items: vec![], dirty: false, paste_ok: false, icon_px: 16,
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
        let items = model.rows(&e);
        let icons = IconCache::new(4);
        let subs = SubLoads::default();
        let src = Rows { items: &items, icons: &icons, failed: true, paste_ok: false, subs: &subs, now: 0, can_load: true };
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
        let rows = |subs: &SubLoads, now: u64, can_load: bool| Rows { items: &items, icons: &icons, failed: false, paste_ok: true, subs, now, can_load }.sub_state(&path);
        assert_eq!(rows(&subs, 0, true), SubLoad::Loading, "about to be asked");
        assert!(subs.want(3, 0));
        assert_eq!(rows(&subs, ctxmodel::SLOW_MS - 1, true), SubLoad::Loading);
        assert_eq!(rows(&subs, ctxmodel::SLOW_MS, true), SubLoad::Failed, "a hung handler: Retry");
        assert_eq!(rows(&subs, 0, false), SubLoad::Failed, "the worker is gone");
        subs.done(3);
        let filled = Rows { items: &items, icons: &icons, failed: false, paste_ok: true, subs: &subs, now: 0, can_load: true };
        assert_eq!(filled.sub_state(&[12345]), SubLoad::Ready, "unknown paths are not loading");
        let mut eager = lazy.clone();
        eager.flags = 0;
        let items2 = [eager.clone()];
        let r2 = Rows { items: &items2, icons: &icons, failed: false, paste_ok: true, subs: &subs, now: 0, can_load: true };
        assert_eq!(r2.sub_state(&[item_key(&eager)]), SubLoad::Ready, "a filled submenu is ready (even if empty)");
    }
}
