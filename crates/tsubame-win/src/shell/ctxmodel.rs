//! The own-drawn context menu's data model (CM1). Pure data and rules, no
//! Win32: everything here is unit tested with fake menus.
//!
//! Flow (see `docs/ctxmenu-spike.md`):
//! 1. The UI makes a [`MenuModel`] with its own items at t = 0
//!    ([`MenuModel::new`]) and paints them at once.
//! 2. A shell worker builds the shell's HMENU, walks it into [`RawItem`]s and
//!    turns them into [`MenuItem`]s ([`build_native`]). They come back as
//!    [`MenuBatch`]es carrying the request's generation; batches of another
//!    generation are dropped ([`MenuModel::apply`]).
//! 3. [`compose`] merges Tsubame, user and native items and applies the
//!    user's edits from the Settings > Context menu list
//!    ([`tsubame_config::MenuEntry`], the same list the U4 editor writes).
//! 4. A pick goes back to the worker that owns the menu by command id
//!    (`Route::Shell`), or is run by the app (`Route::App`).

use crate::bitmap::Bitmap;
use std::collections::VecDeque;
use std::sync::Arc;
use tsubame_config::menu::{EntryKind, MenuEntry, Source};

/// Item state bits (`MenuItem::flags`).
pub mod flag {
    pub const DISABLED: u16 = 1;
    pub const CHECKED: u16 = 2;
    pub const DEFAULT: u16 = 4;
    /// check mark is a radio dot (`MFT_RADIOCHECK`)
    pub const RADIO: u16 = 8;
    /// the handler draws it (`MFT_OWNERDRAW`): no reliable text
    pub const OWNERDRAW: u16 = 16;
    /// the bitmap is `HBMMENU_CALLBACK`: the handler draws the icon
    pub const ICON_CALLBACK: u16 = 32;
    /// a submenu whose children are not known yet (empty when walked)
    pub const LAZY: u16 = 64;
    /// starts a new column (`MFT_MENUBREAK`/`MFT_MENUBARBREAK`); ignored when drawn
    pub const BREAK: u16 = 128;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemSource {
    /// from the shell's HMENU (Windows and shell extensions)
    Native,
    /// Tsubame's own (Open in new tab, Copy path, ...)
    Tsubame,
    /// the user's custom / imported entries
    User,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Command,
    Separator,
    Submenu,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    /// native: the HMENU command id (0 for separators and submenus);
    /// Tsubame / user items: 0 (they are run by `action`)
    pub id: u32,
    pub kind: ItemKind,
    pub source: ItemSource,
    /// text with `&` markers removed (`&&` -> `&`) and no `\t` part
    pub label: String,
    /// the letter after a single `&`, lower-case
    pub accel: Option<char>,
    /// the text after `\t` ("Ctrl+C"), if any
    pub shortcut: String,
    /// canonical verb (`GetCommandString(GCS_VERBW)`), lower-case
    pub verb: Option<String>,
    pub help: Option<String>,
    pub flags: u16,
    /// key into [`IconCache`] (content hash of a 16 px premultiplied bitmap)
    pub icon: Option<u64>,
    pub children: Vec<MenuItem>,
    /// what running it means for Tsubame / user items: an app command id
    /// (`file.copy`) or a menu entry id (`custom.3`)
    pub action: String,
    /// the key the user's edits are stored under ([`native_key`])
    pub key: String,
    /// a native submenu's token on the worker that holds the menu (0: none);
    /// a lazy submenu is filled by asking for it (`CtxCmd::Expand`)
    pub sub: u32,
}

impl MenuItem {
    pub fn separator(source: ItemSource) -> MenuItem {
        MenuItem { id: 0, kind: ItemKind::Separator, source, label: String::new(), accel: None, shortcut: String::new(), verb: None, help: None, flags: 0, icon: None, children: Vec::new(), action: String::new(), key: String::new(), sub: 0 }
    }
    pub fn command(source: ItemSource, label: &str, action: &str) -> MenuItem {
        let (label, accel, shortcut) = parse_label(label);
        MenuItem { kind: ItemKind::Command, label, accel, shortcut, action: action.to_string(), ..MenuItem::separator(source) }
    }
    pub fn is_separator(&self) -> bool {
        self.kind == ItemKind::Separator
    }
    pub fn disabled(&self) -> bool {
        self.flags & flag::DISABLED != 0
    }
    /// Items in the tree, this one included.
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(MenuItem::count).sum::<usize>()
    }
}

/// One HMENU item as `GetMenuItemInfoW` reports it, before any cleaning.
/// The worker fills these; tests write them by hand.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawItem {
    /// `MENUITEMINFOW::wID`
    pub id: u32,
    /// raw text with `&` and `\t` (empty for separators and owner-drawn items)
    pub text: String,
    pub separator: bool,
    pub ownerdraw: bool,
    pub radio: bool,
    pub menubreak: bool,
    pub disabled: bool,
    pub checked: bool,
    pub default: bool,
    /// `Some` = submenu; an empty vec = submenu that stayed empty
    pub submenu: Option<Vec<RawItem>>,
    pub verb: Option<String>,
    pub help: Option<String>,
    /// `hbmpItem` was `HBMMENU_CALLBACK`
    pub icon_callback: bool,
    /// the converted item bitmap (16 px)
    pub icon: Option<Arc<Bitmap>>,
    /// the worker's token for a submenu left unwalked (lazy); 0: none
    pub sub: u32,
}

/// `"&Open with\tCtrl+O"` -> (`"Open with"`, `Some('o')`, `"Ctrl+O"`).
/// `&&` is a literal `&`; the first single `&` marks the accelerator.
pub fn parse_label(raw: &str) -> (String, Option<char>, String) {
    let (text, shortcut) = match raw.split_once('\t') {
        Some((a, b)) => (a, b.trim().to_string()),
        None => (raw, String::new()),
    };
    let mut out = String::with_capacity(text.len());
    let mut accel = None;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        match it.peek().copied() {
            Some('&') => {
                it.next();
                out.push('&');
            }
            Some(n) if accel.is_none() && !n.is_whitespace() => accel = n.to_lowercase().next(),
            _ => {}
        }
    }
    (out.trim().to_string(), accel, shortcut)
}

/// Key a native item's edits are stored under: the canonical verb when the
/// handler has one (stable across languages and updates), else the cleaned
/// label. Both lower-case. Matches the ids of `Source::Windows` entries in the
/// U4 menu list (`windows.verb:open`, `windows.label:7-zip`).
pub fn native_key(verb: Option<&str>, label: &str) -> String {
    match verb.filter(|v| !v.is_empty() && !v.starts_with('#')) {
        Some(v) => format!("windows.verb:{}", v.to_lowercase()),
        None => format!("windows.label:{}", label.to_lowercase()),
    }
}

/// Upper bounds for one walked menu (a broken handler must not make it huge).
pub const MAX_DEPTH: usize = 4;
pub const MAX_ITEMS: usize = 512;

/// Turns the walked HMENU into model items: labels parsed, flags folded,
/// separators cleaned (none leading, trailing or doubled), empty submenus
/// kept but marked [`flag::LAZY`], depth and count capped. Icons are moved
/// into `icons`.
pub fn build_native(raw: &[RawItem], icons: &mut IconCache) -> Vec<MenuItem> {
    let mut budget = MAX_ITEMS;
    build_level(raw, icons, 0, &mut budget)
}

fn build_level(raw: &[RawItem], icons: &mut IconCache, depth: usize, budget: &mut usize) -> Vec<MenuItem> {
    let mut out: Vec<MenuItem> = Vec::new();
    for r in raw {
        if *budget == 0 {
            break;
        }
        if r.separator {
            if out.last().is_some_and(|l| !l.is_separator()) {
                out.push(MenuItem::separator(ItemSource::Native));
            }
            continue;
        }
        let (label, accel, shortcut) = parse_label(&r.text);
        let label = if label.is_empty() { r.verb.clone().or_else(|| r.help.clone()).unwrap_or_default() } else { label };
        if label.is_empty() && !r.ownerdraw {
            // a nameless, non-drawn item cannot be shown
            continue;
        }
        *budget -= 1;
        let mut flags = 0;
        for (b, f) in [(r.disabled, flag::DISABLED), (r.checked, flag::CHECKED), (r.default, flag::DEFAULT), (r.radio, flag::RADIO), (r.ownerdraw, flag::OWNERDRAW), (r.icon_callback, flag::ICON_CALLBACK), (r.menubreak, flag::BREAK)] {
            if b {
                flags |= f;
            }
        }
        let verb = r.verb.as_ref().map(|v| v.to_lowercase()).filter(|v| !v.is_empty());
        let (kind, children) = match &r.submenu {
            Some(sub) if depth + 1 < MAX_DEPTH => {
                let c = build_level(sub, icons, depth + 1, budget);
                if c.is_empty() {
                    flags |= flag::LAZY;
                }
                (ItemKind::Submenu, c)
            }
            Some(_) => {
                flags |= flag::LAZY;
                (ItemKind::Submenu, Vec::new())
            }
            None => (ItemKind::Command, Vec::new()),
        };
        let icon = r.icon.as_ref().map(|b| icons.put(b.clone()));
        let key = native_key(verb.as_deref(), &label);
        out.push(MenuItem { id: if kind == ItemKind::Command { r.id } else { 0 }, kind, source: ItemSource::Native, label, accel, shortcut, verb, help: r.help.clone(), flags, icon, children, action: String::new(), key, sub: if kind == ItemKind::Submenu { r.sub } else { 0 } });
    }
    while out.last().is_some_and(MenuItem::is_separator) {
        out.pop();
    }
    out
}

/// Finds the item at `path` (indices from the top).
pub fn at_path<'a>(items: &'a [MenuItem], path: &[usize]) -> Option<&'a MenuItem> {
    let (&first, rest) = path.split_first()?;
    let it = items.get(first)?;
    if rest.is_empty() { Some(it) } else { at_path(&it.children, rest) }
}

/// The submenu with worker token `sub` (searched depth first).
pub fn find_sub(items: &[MenuItem], sub: u32) -> Option<&MenuItem> {
    if sub == 0 {
        return None;
    }
    items.iter().find_map(|it| if it.kind == ItemKind::Submenu && it.sub == sub { Some(it) } else { find_sub(&it.children, sub) })
}

fn find_sub_mut(items: &mut [MenuItem], sub: u32) -> Option<&mut MenuItem> {
    if sub == 0 {
        return None;
    }
    for it in items.iter_mut() {
        if it.kind == ItemKind::Submenu && it.sub == sub {
            return Some(it);
        }
        if let Some(f) = find_sub_mut(&mut it.children, sub) {
            return Some(f);
        }
    }
    None
}

/// A lazy submenu that still has to be asked for.
pub fn is_lazy(it: &MenuItem) -> bool {
    it.kind == ItemKind::Submenu && it.flags & flag::LAZY != 0 && it.sub != 0
}

// ---- lazy submenus (CM6) ---------------------------------------------------

/// What a lazy submenu shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubState {
    /// not asked for yet
    Idle,
    /// asked, "Loading…" row
    Loading,
    /// no answer after [`SLOW_MS`] (a hung handler): the failure row with Retry
    Slow,
}

/// The UI's lazy-submenu requests of one menu generation: each token is
/// asked for once; an answer clears it. Pure (a fake clock in `now_ms`).
#[derive(Clone, Debug, Default)]
pub struct SubLoads {
    asked: Vec<(u32, u64)>,
    done: Vec<u32>,
}

impl SubLoads {
    /// True when `sub` has to be asked for now (first time).
    pub fn want(&mut self, sub: u32, now_ms: u64) -> bool {
        if sub == 0 || self.done.contains(&sub) || self.asked.iter().any(|a| a.0 == sub) {
            return false;
        }
        self.asked.push((sub, now_ms));
        true
    }
    /// The answer for `sub` came (stale generations never reach here).
    pub fn done(&mut self, sub: u32) -> Option<u64> {
        let i = self.asked.iter().position(|a| a.0 == sub);
        let at = i.map(|i| self.asked.swap_remove(i).1);
        if !self.done.contains(&sub) {
            self.done.push(sub);
        }
        at
    }
    pub fn state(&self, sub: u32, now_ms: u64) -> SubState {
        match self.asked.iter().find(|a| a.0 == sub) {
            None => SubState::Idle,
            Some(&(_, at)) if now_ms.saturating_sub(at) >= SLOW_MS => SubState::Slow,
            Some(_) => SubState::Loading,
        }
    }
    /// Requests without an answer (the menu's tick runs while there are some,
    /// so the watchdog and the slow switch get their frames).
    pub fn pending(&self) -> bool {
        !self.asked.is_empty()
    }
    /// A request is younger than [`SLOW_MS`]: frames are still needed to
    /// turn it into the failure row. Once every one is slow, no tick.
    pub fn loading(&self, now_ms: u64) -> bool {
        self.asked.iter().any(|a| now_ms.saturating_sub(a.1) < SLOW_MS)
    }
    /// A new generation: nothing asked yet.
    pub fn clear(&mut self) {
        self.asked.clear();
        self.done.clear();
    }
}

// ---- routing ------------------------------------------------------------

/// Verbs Tsubame runs itself even when picked from the shell's part of the
/// menu: they go through the app's own clipboard, rename and file-op queue
/// (with its Recycle Bin hazard checks and undo). Same set as the native
/// menu's `MenuOpts::intercept` in the app.
pub const APP_VERBS: [&str; 5] = ["delete", "cut", "copy", "paste", "rename"];

/// Tsubame built-in command -> the shell verb it replaces in the native part
/// (so the menu never shows "Copy" twice).
pub const BUILTIN_VERBS: [(&str, &str); 8] = [("file.open", "open"), ("file.cut", "cut"), ("file.copy", "copy"), ("file.paste", "paste"), ("file.rename", "rename"), ("file.delete", "delete"), ("file.properties", "properties"), ("file.copy-path", "copyaspath")];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// `invoke(id)` on the worker that owns the menu
    Shell(u32),
    /// an app command (`file.copy`) or a menu entry id (`custom.3`)
    App(String),
}

/// What picking `item` does.
pub fn route(item: &MenuItem) -> Route {
    match item.source {
        ItemSource::Native => match item.verb.as_deref() {
            Some(v) if APP_VERBS.contains(&v) => Route::App(format!("file.{v}")),
            _ => Route::Shell(item.id),
        },
        // Tsubame rows such as Properties may still end in a shell verb: the
        // app looks it up with [`native_by_verb`] and sends `Shell(id)`
        ItemSource::Tsubame | ItemSource::User => Route::App(item.action.clone()),
    }
}

/// The command id of the native item with `verb` (searched depth first), so a
/// Tsubame row like Properties or Open can run the shell's verb.
pub fn native_by_verb(native: &[MenuItem], verb: &str) -> Option<u32> {
    native.iter().find_map(|n| if n.kind == ItemKind::Command && n.verb.as_deref() == Some(verb) { Some(n.id) } else { native_by_verb(&n.children, verb) })
}

// ---- composing and the user's edits --------------------------------------

/// Tsubame's and the user's items for a click, from the U4 list: visible
/// built-ins (except the `Windows` placeholders, which the native part fills)
/// and the program entries that apply to `target`.
pub fn own_items(entries: &[MenuEntry], target: tsubame_config::menu::Target, ext: &str) -> (Vec<MenuItem>, Vec<MenuItem>) {
    let mut tsubame = Vec::new();
    for e in entries.iter().filter(|e| e.parent.is_empty()) {
        match (e.source, e.kind) {
            (Source::Builtin, EntryKind::Separator) => {
                if tsubame.last().is_some_and(|l: &MenuItem| !l.is_separator()) {
                    tsubame.push(MenuItem::separator(ItemSource::Tsubame));
                }
            }
            (Source::Builtin, EntryKind::Item) if !e.hidden && e.matches(target, ext) => {
                let mut it = MenuItem::command(ItemSource::Tsubame, &e.name, &e.command);
                it.key = e.id.clone();
                tsubame.push(it);
            }
            _ => {}
        }
    }
    while tsubame.last().is_some_and(MenuItem::is_separator) {
        tsubame.pop();
    }
    let user = tsubame_config::menu::program_entries(entries, target, ext)
        .into_iter()
        .map(|(e, label)| {
            let mut it = MenuItem::command(ItemSource::User, &label, &e.id);
            it.key = e.id.clone();
            it
        })
        .collect();
    (tsubame, user)
}

/// Applies the user's edits to native items, recursively: an entry of source
/// `Windows` whose id is the item's [`native_key`] hides it (`hidden`),
/// renames it (`name`, when not empty) and orders it: items that have an
/// entry are put in the list's order inside the slots they occupy, items
/// without one keep their place. Separators left dangling are cleaned.
pub fn apply_overrides(items: &mut Vec<MenuItem>, entries: &[MenuEntry]) {
    let find = |key: &str| entries.iter().position(|e| e.source == Source::Windows && !key.is_empty() && e.id.eq_ignore_ascii_case(key));
    items.retain(|it| find(&it.key).is_none_or(|i| !entries[i].hidden));
    let mut slots = Vec::new();
    for (pos, it) in items.iter_mut().enumerate() {
        if let Some(i) = find(&it.key) {
            let name = entries[i].name.trim();
            if !name.is_empty() && !name.eq_ignore_ascii_case(&it.label) {
                let (l, a, _) = parse_label(name);
                it.label = l;
                it.accel = a.or(it.accel);
            }
            slots.push((pos, i));
        }
        if !it.children.is_empty() {
            apply_overrides(&mut it.children, entries);
        }
    }
    let mut order: Vec<usize> = slots.iter().map(|s| s.1).collect();
    order.sort_unstable();
    let picked: Vec<MenuItem> = order.iter().map(|&ei| items[slots.iter().find(|s| s.1 == ei).unwrap().0].clone()).collect();
    for ((pos, _), it) in slots.iter().zip(picked) {
        items[*pos] = it;
    }
    clean_separators(items);
}

fn clean_separators(items: &mut Vec<MenuItem>) {
    let mut out: Vec<MenuItem> = Vec::with_capacity(items.len());
    for it in items.drain(..) {
        if it.is_separator() && out.last().is_none_or(MenuItem::is_separator) {
            continue;
        }
        out.push(it);
    }
    while out.last().is_some_and(MenuItem::is_separator) {
        out.pop();
    }
    *items = out;
}

/// The menu as drawn: Tsubame's group, the native items (minus verbs a
/// Tsubame row already covers, with the user's edits), then the user's
/// entries. Groups are split by separators.
pub fn compose(tsubame: &[MenuItem], native: &[MenuItem], user: &[MenuItem], entries: &[MenuEntry]) -> Vec<MenuItem> {
    compose_all(tsubame, native, &[], user, entries)
}

/// [`compose`] with the packaged (Win11 `IExplorerCommand`) items (CM7) as
/// their own group after the classic shell items and before the user's
/// entries. Packaged items the classic menu already has (same verb, which
/// for packaged items is the handler CLSID, or the same label) are dropped,
/// so "Edit in Notepad" never shows twice. One menu, no "Show more options".
pub fn compose_all(tsubame: &[MenuItem], native: &[MenuItem], packaged: &[MenuItem], user: &[MenuItem], entries: &[MenuEntry]) -> Vec<MenuItem> {
    let covered: Vec<&str> = tsubame.iter().filter_map(|t| BUILTIN_VERBS.iter().find(|(c, _)| *c == t.action).map(|(_, v)| *v)).collect();
    let mut nat: Vec<MenuItem> = native.iter().filter(|n| !n.verb.as_deref().is_some_and(|v| covered.contains(&v))).cloned().collect();
    apply_overrides(&mut nat, entries);
    let mut pkg = dedupe_packaged(packaged, native, tsubame);
    apply_overrides(&mut pkg, entries);
    let mut out = Vec::with_capacity(tsubame.len() + nat.len() + pkg.len() + user.len() + 3);
    for group in [tsubame, &nat[..], &pkg[..], user] {
        if group.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(MenuItem::separator(group[0].source));
        }
        out.extend(group.iter().cloned());
    }
    clean_separators(&mut out);
    out
}

/// Packaged items minus the ones the classic menu (or a Tsubame row) already
/// shows: same verb (case-insensitive, the CLSID with braces) or the same
/// label (case-insensitive). Separators are cleaned.
pub fn dedupe_packaged(packaged: &[MenuItem], native: &[MenuItem], tsubame: &[MenuItem]) -> Vec<MenuItem> {
    let norm = |s: &str| s.trim().trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase();
    let verbs: Vec<String> = native.iter().filter_map(|n| n.verb.as_deref()).map(norm).collect();
    let labels: Vec<String> = native.iter().chain(tsubame).filter(|n| !n.is_separator()).map(|n| n.label.to_lowercase()).collect();
    let mut out: Vec<MenuItem> = packaged
        .iter()
        .filter(|p| p.is_separator() || (!p.verb.as_deref().is_some_and(|v| verbs.contains(&norm(v))) && !labels.contains(&p.label.to_lowercase())))
        .cloned()
        .collect();
    clean_separators(&mut out);
    while out.first().is_some_and(MenuItem::is_separator) {
        out.remove(0);
    }
    out
}

/// The editor row for a native item the user edits for the first time (a
/// `Source::Windows` entry keyed by [`native_key`]; can be hidden, renamed
/// and moved, never deleted).
pub fn native_entry(item: &MenuItem) -> MenuEntry {
    MenuEntry { id: item.key.clone(), source: Source::Windows, name: item.label.clone(), kind: if item.kind == ItemKind::Submenu { EntryKind::Submenu } else { EntryKind::Item }, ..Default::default() }
}

// ---- batches and the model the UI holds ----------------------------------

#[derive(Clone, Debug)]
pub enum Part {
    /// the native top level (QueryContextMenu done, submenus walked)
    Native(Vec<MenuItem>),
    /// the packaged (Win11 `IExplorerCommand`) items (CM7), sent after the
    /// classic top level so they never delay it; submenus are lazy tokens
    Packaged(Vec<MenuItem>),
    /// the children of the lazy submenu with worker token `sub` (CM6: asked
    /// for when the user opens it)
    Submenu { sub: u32, items: Vec<MenuItem> },
    /// icons for keys already sent
    Icons(Vec<(u64, Arc<Bitmap>)>),
    /// building failed (or the worker gave up: the UI shows Retry)
    Failed(String),
    /// result of an invoke
    Invoked { id: u32, verb: Option<String>, result: Result<(), String> },
    /// every item sent up front is in (lazy, CM6: the top level, submenus come
    /// as [`Part::Submenu`] when asked; eager: the whole tree, submenus
    /// settled), icons sent: the loading line goes. The worker now waits for
    /// a pick or a submenu request.
    Complete,
    /// the worker released the menu (after an invoke, a close or a timeout)
    Closed,
}

#[derive(Clone, Debug)]
pub struct MenuBatch {
    /// the request's generation; the UI drops other generations
    pub generation: u64,
    pub part: Part,
    /// worker timings in microseconds: bind, QueryContextMenu, walk
    pub timing: [u64; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// own items shown, the shell has not answered (thin loading line)
    Loading,
    Ready,
    /// "Some items could not load" + Retry
    Failed,
}

/// The menu the UI paints. Own items exist from t = 0; native ones arrive.
pub struct MenuModel {
    pub generation: u64,
    pub tsubame: Vec<MenuItem>,
    pub user: Vec<MenuItem>,
    pub native: Vec<MenuItem>,
    /// packaged items (CM7), after the classic ones
    pub packaged: Vec<MenuItem>,
    /// worker time of the packaged step (scan, create, GetState), us
    pub packaged_us: Option<u64>,
    pub state: LoadState,
    pub error: Option<String>,
    pub icons: IconCache,
    /// [`Part::Complete`] arrived: every shell item sent up front is in
    pub complete: bool,
}

impl MenuModel {
    pub fn new(generation: u64, tsubame: Vec<MenuItem>, user: Vec<MenuItem>) -> MenuModel {
        MenuModel { generation, tsubame, user, native: Vec::new(), packaged: Vec::new(), packaged_us: None, state: LoadState::Loading, error: None, icons: IconCache::new(ICON_CACHE_CAP), complete: false }
    }

    /// Takes a batch. Returns false (and changes nothing) for another
    /// generation's batch.
    pub fn apply(&mut self, b: MenuBatch) -> bool {
        if b.generation != self.generation {
            return false;
        }
        match b.part {
            Part::Native(items) => {
                self.native = items;
                self.state = LoadState::Ready;
            }
            Part::Submenu { sub, items } => {
                if let Some(it) = find_sub_mut(&mut self.native, sub).or_else(|| find_sub_mut(&mut self.packaged, sub)) {
                    it.flags &= !flag::LAZY;
                    it.children = items;
                }
            }
            Part::Packaged(items) => {
                self.packaged = items;
                self.packaged_us = Some(b.timing[2]);
            }
            Part::Icons(v) => {
                for (k, bmp) in v {
                    self.icons.insert(k, bmp);
                }
            }
            Part::Failed(e) => {
                self.state = LoadState::Failed;
                self.error = Some(e);
                self.complete = true;
            }
            Part::Complete => {
                self.complete = true;
                if self.state == LoadState::Loading {
                    self.state = LoadState::Ready;
                }
            }
            Part::Invoked { .. } | Part::Closed => {}
        }
        true
    }

    /// The rows to draw now.
    pub fn rows(&self, entries: &[MenuEntry]) -> Vec<MenuItem> {
        compose_all(&self.tsubame, &self.native, &self.packaged, &self.user, entries)
    }
}

// ---- the app's rows and the loading clock (CM2, CM5) -----------------------

/// Label of the `windows.more` escape hatch while the user kept the default
/// name: there is no Win11 "Show more options" step in the own-drawn menu,
/// the row opens the native popup (the classic menu, drawn by Windows).
pub const CLASSIC_LABEL: &str = "Windows menu (classic)";
/// Action of that row.
pub const ACTION_CLASSIC: &str = "windows.more";

/// The Tsubame group of the own-drawn menu: the visible built-ins of the U4
/// list (like [`own_items`]), the `windows.more` escape hatch where the list
/// puts it, and `extras` (app rows that are not in the U4 list, such as Copy
/// name) right after `builtin.copy-path`, or at the end of the first group
/// when that one is hidden.
pub fn own_rows(entries: &[MenuEntry], target: tsubame_config::menu::Target, ext: &str, extras: &[MenuItem]) -> Vec<MenuItem> {
    let mut out: Vec<MenuItem> = Vec::new();
    let mut placed = extras.is_empty();
    // extras go after Copy as path when it shows, else at the first separator
    let anchor = entries.iter().any(|e| e.parent.is_empty() && e.id == "builtin.copy-path" && !e.hidden && e.matches(target, ext));
    for e in entries.iter().filter(|e| e.parent.is_empty()) {
        match (e.source, e.kind) {
            (Source::Builtin, EntryKind::Separator) => {
                if !placed && !anchor {
                    out.extend(extras.iter().cloned());
                    placed = true;
                }
                if out.last().is_some_and(|l| !l.is_separator()) {
                    out.push(MenuItem::separator(ItemSource::Tsubame));
                }
            }
            (Source::Builtin, EntryKind::Item) if !e.hidden && e.matches(target, ext) => {
                let mut it = MenuItem::command(ItemSource::Tsubame, &e.name, &e.command);
                it.key = e.id.clone();
                out.push(it);
                if e.id == "builtin.copy-path" && !placed {
                    out.extend(extras.iter().cloned());
                    placed = true;
                }
            }
            (Source::Windows, EntryKind::Item) if e.id == ACTION_CLASSIC && !e.hidden && (e.matches(target, ext) || target == tsubame_config::menu::Target::Background) => {
                let name = if e.name.trim().is_empty() || e.name == "Show more options" { CLASSIC_LABEL } else { e.name.as_str() };
                let mut it = MenuItem::command(ItemSource::Tsubame, name, ACTION_CLASSIC);
                it.key = e.id.clone();
                out.push(it);
            }
            _ => {}
        }
    }
    if !placed {
        out.extend(extras.iter().cloned());
    }
    clean_separators(&mut out);
    out
}

/// After this long without the shell's top level the menu shows "Some items
/// could not load" + Retry (the mock's failure frame). The pool gives the
/// worker up a second later (`Pool::ctx_hang_after`, 4 s).
pub const SLOW_MS: u64 = 3_000;

/// The state to draw `elapsed_ms` after the request: a menu still loading
/// after [`SLOW_MS`] shows the failure row (a late answer still fills it).
pub fn shown_state(state: LoadState, elapsed_ms: u64) -> LoadState {
    if state == LoadState::Loading && elapsed_ms >= SLOW_MS { LoadState::Failed } else { state }
}

// ---- icon cache ------------------------------------------------------------

/// Icons per process the UI keeps (16 px, ~1 KB each).
pub const ICON_CACHE_CAP: usize = 256;

/// Bounded LRU of menu icons keyed by content hash, so the same icon from
/// many handlers / menus is stored (and uploaded as a texture) once.
pub struct IconCache {
    cap: usize,
    map: std::collections::HashMap<u64, Arc<Bitmap>>,
    order: VecDeque<u64>,
}

impl IconCache {
    pub fn new(cap: usize) -> IconCache {
        IconCache { cap: cap.max(1), map: Default::default(), order: VecDeque::new() }
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    /// Like `get`, without touching the LRU order (painting reads every frame).
    pub fn peek(&self, k: u64) -> Option<&Arc<Bitmap>> {
        self.map.get(&k)
    }
    pub fn get(&mut self, k: u64) -> Option<Arc<Bitmap>> {
        let v = self.map.get(&k)?.clone();
        self.touch(k);
        Some(v)
    }
    fn touch(&mut self, k: u64) {
        if let Some(i) = self.order.iter().position(|&o| o == k) {
            self.order.remove(i);
        }
        self.order.push_back(k);
    }
    pub fn insert(&mut self, k: u64, b: Arc<Bitmap>) {
        if self.map.insert(k, b).is_none() {
            while self.map.len() > self.cap {
                let Some(old) = self.order.pop_front() else { break };
                self.map.remove(&old);
            }
        }
        self.touch(k);
    }
    /// Stores `b` under its content hash and returns the key.
    pub fn put(&mut self, b: Arc<Bitmap>) -> u64 {
        let k = icon_hash(&b);
        self.insert(k, b);
        k
    }
    /// Entries, oldest first (for sending to the UI).
    pub fn entries(&self) -> Vec<(u64, Arc<Bitmap>)> {
        self.order.iter().filter_map(|k| self.map.get(k).map(|b| (*k, b.clone()))).collect()
    }
}

/// FNV-1a over size and pixels.
pub fn icon_hash(b: &Bitmap) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: u32| {
        for byte in v.to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    };
    eat(b.w);
    eat(b.h);
    for &p in &b.px {
        eat(p);
    }
    h
}

/// The tree as indented text (the `ctxmenu_dump` harness and tests).
pub fn dump(items: &[MenuItem], out: &mut String) {
    dump_level(items, 0, out);
}

fn dump_level(items: &[MenuItem], depth: usize, out: &mut String) {
    use std::fmt::Write;
    for it in items {
        let pad = "  ".repeat(depth);
        if it.is_separator() {
            let _ = writeln!(out, "{pad}----");
            continue;
        }
        let mut f = String::new();
        for (b, s) in [(flag::DISABLED, "disabled"), (flag::CHECKED, "checked"), (flag::DEFAULT, "default"), (flag::RADIO, "radio"), (flag::OWNERDRAW, "ownerdraw"), (flag::ICON_CALLBACK, "icon-cb"), (flag::LAZY, "lazy")] {
            if it.flags & b != 0 {
                f.push_str(s);
                f.push(' ');
            }
        }
        let _ = writeln!(
            out,
            "{pad}{}{} [id {}{}{}{}] {}{}",
            it.label,
            if it.kind == ItemKind::Submenu { " >" } else { "" },
            it.id,
            it.verb.as_deref().map(|v| format!(" verb={v}")).unwrap_or_default(),
            it.accel.map(|a| format!(" &{a}")).unwrap_or_default(),
            if it.icon.is_some() { " icon" } else { "" },
            f.trim_end(),
            if it.shortcut.is_empty() { String::new() } else { format!(" \\t{}", it.shortcut) },
        );
        dump_level(&it.children, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsubame_config::menu::{ShowFor, Target, default_entries};

    fn raw(id: u32, text: &str) -> RawItem {
        RawItem { id, text: text.into(), ..Default::default() }
    }
    fn sep() -> RawItem {
        RawItem { separator: true, ..Default::default() }
    }
    fn verb(mut r: RawItem, v: &str) -> RawItem {
        r.verb = Some(v.into());
        r
    }
    fn labels(v: &[MenuItem]) -> Vec<String> {
        v.iter().map(|i| if i.is_separator() { "--".to_string() } else { i.label.clone() }).collect()
    }
    fn px(c: u32) -> Arc<Bitmap> {
        Arc::new(Bitmap { w: 16, h: 16, px: vec![c; 256] })
    }

    #[test]
    fn labels_lose_markers_but_keep_accelerator_and_shortcut() {
        assert_eq!(parse_label("&Open"), ("Open".into(), Some('o'), String::new()));
        assert_eq!(parse_label("Open &with\tCtrl+W"), ("Open with".into(), Some('w'), "Ctrl+W".into()));
        assert_eq!(parse_label("Tom && Jerry"), ("Tom & Jerry".into(), None, String::new()));
        assert_eq!(parse_label("A&&B &Copy &Later"), ("A&B Copy Later".into(), Some('c'), String::new()));
        assert_eq!(parse_label("trailing&"), ("trailing".into(), None, String::new()));
        assert_eq!(parse_label("Ö&ffnen"), ("Öffnen".into(), Some('f'), String::new()));
    }

    #[test]
    fn native_build_cleans_separators_flags_and_submenus() {
        let mut icons = IconCache::new(8);
        let mut open = verb(raw(1, "&Open"), "Open");
        open.default = true;
        open.icon = Some(px(0xff00_00ff));
        let mut dis = raw(3, "Pr&int");
        dis.disabled = true;
        let mut chk = raw(4, "Pinned");
        chk.checked = true;
        chk.radio = true;
        let sendto = RawItem { text: "Se&nd to".into(), submenu: Some(vec![raw(10, "Desktop"), sep(), raw(11, "Mail")]), ..Default::default() };
        let empty = RawItem { text: "Open with".into(), submenu: Some(vec![]), ..Default::default() };
        let od = RawItem { id: 20, ownerdraw: true, icon_callback: true, verb: Some("7z.add".into()), ..Default::default() };
        let nameless = raw(21, "");
        let v = vec![sep(), open, sep(), sep(), dis, chk, sendto, empty, od, nameless, sep()];
        let m = build_native(&v, &mut icons);
        assert_eq!(labels(&m), ["Open", "--", "Print", "Pinned", "Send to", "Open with", "7z.add"]);
        assert_eq!(m[0].flags, flag::DEFAULT);
        assert_eq!(m[0].verb.as_deref(), Some("open"));
        assert_eq!(m[0].key, "windows.verb:open");
        assert_eq!(m[0].accel, Some('o'));
        assert!(m[0].icon.is_some() && icons.len() == 1);
        assert!(m[2].disabled() && m[2].accel == Some('i'));
        assert_eq!(m[3].flags, flag::CHECKED | flag::RADIO);
        assert_eq!(m[4].kind, ItemKind::Submenu);
        assert_eq!(m[4].id, 0, "submenus have no command id");
        assert_eq!(labels(&m[4].children), ["Desktop", "--", "Mail"]);
        assert_eq!(m[5].flags, flag::LAZY);
        assert_eq!(m[6].flags, flag::OWNERDRAW | flag::ICON_CALLBACK);
        assert_eq!(m[2].key, "windows.label:print");
    }

    #[test]
    fn walk_is_capped_in_depth_and_count() {
        let mut deep = raw(99, "leaf");
        for d in 0..8 {
            deep = RawItem { text: format!("level {d}"), submenu: Some(vec![deep]), ..Default::default() };
        }
        let m = build_native(&[deep], &mut IconCache::new(4));
        let mut depth = 0;
        let mut it = &m[0];
        while let Some(c) = it.children.first() {
            depth += 1;
            it = c;
        }
        assert_eq!(depth, MAX_DEPTH - 1);
        assert!(it.flags & flag::LAZY != 0);
        let many: Vec<RawItem> = (0..2000).map(|i| raw(i + 1, &format!("item {i}"))).collect();
        assert_eq!(build_native(&many, &mut IconCache::new(4)).len(), MAX_ITEMS);
    }

    #[test]
    fn routes_send_app_verbs_to_the_app_and_the_rest_to_the_shell() {
        let mut icons = IconCache::new(4);
        let m = build_native(&[verb(raw(5, "Delete"), "delete"), verb(raw(6, "Properties"), "properties"), raw(7, "Add to archive..."), verb(raw(8, "Rename"), "rename")], &mut icons);
        assert_eq!(route(&m[0]), Route::App("file.delete".into()));
        assert_eq!(route(&m[1]), Route::Shell(6));
        assert_eq!(route(&m[2]), Route::Shell(7));
        assert_eq!(route(&m[3]), Route::App("file.rename".into()));
        let user = MenuItem::command(ItemSource::User, "VS Code", "custom.1");
        assert_eq!(route(&user), Route::App("custom.1".into()));
    }

    fn win(id: &str, name: &str, hidden: bool) -> MenuEntry {
        MenuEntry { id: id.into(), source: Source::Windows, name: name.into(), hidden, ..Default::default() }
    }

    #[test]
    fn overrides_hide_rename_and_reorder_by_verb_or_label() {
        let mut icons = IconCache::new(4);
        let base = build_native(&[verb(raw(1, "&Open"), "open"), raw(2, "7-Zip"), sep(), verb(raw(3, "Share"), "Windows.ModernShare"), raw(4, "Git Bash Here"), sep(), verb(raw(5, "Properties"), "properties")], &mut icons);
        // hide Share and Properties (the separator before Properties goes too)
        let mut m = base.clone();
        apply_overrides(&mut m, &[win("windows.verb:windows.modernshare", "", true), win("windows.verb:properties", "", true)]);
        assert_eq!(labels(&m), ["Open", "7-Zip", "--", "Git Bash Here"]);
        // rename by label key, keep the accelerator of the new name
        let mut m = base.clone();
        apply_overrides(&mut m, &[win("windows.label:7-zip", "&Archive", false)]);
        assert_eq!(m[1].label, "Archive");
        assert_eq!(m[1].accel, Some('a'));
        // reorder: Git before Open; unlisted items stay where they are
        let mut m = base.clone();
        apply_overrides(&mut m, &[win("windows.label:git bash here", "", false), win("windows.verb:open", "", false)]);
        assert_eq!(labels(&m), ["Git Bash Here", "7-Zip", "--", "Share", "Open", "--", "Properties"]);
        // case does not matter, other sources never match native items
        let mut m = base.clone();
        apply_overrides(&mut m, &[MenuEntry { source: Source::Custom, ..win("windows.verb:open", "", true) }, win("WINDOWS.VERB:OPEN", "Run", false)]);
        assert_eq!(m[0].label, "Run");
    }

    #[test]
    fn overrides_reach_into_submenus() {
        let mut icons = IconCache::new(4);
        let sub = RawItem { text: "Send to".into(), submenu: Some(vec![raw(10, "Desktop (create shortcut)"), raw(11, "Mail recipient")]), ..Default::default() };
        let mut m = build_native(&[sub], &mut icons);
        apply_overrides(&mut m, &[win("windows.label:mail recipient", "", true)]);
        assert_eq!(labels(&m[0].children), ["Desktop (create shortcut)"]);
    }

    #[test]
    fn compose_puts_groups_in_order_and_drops_covered_verbs() {
        let entries = default_entries();
        let (ts, _) = own_items(&entries, Target::File, "txt");
        assert!(ts.iter().any(|t| t.action == "file.copy"));
        assert!(!ts.iter().any(|t| t.label == "Open with" || t.label == "Show more options"), "Windows placeholders are filled by the native part");
        let user = vec![MenuItem::command(ItemSource::User, "VS Code", "custom.1")];
        let mut icons = IconCache::new(4);
        let native = build_native(&[verb(raw(1, "Open"), "open"), verb(raw(2, "Copy"), "copy"), raw(3, "7-Zip"), verb(raw(4, "Properties"), "properties")], &mut icons);
        let rows = compose(&ts, &native, &user, &entries);
        let l = labels(&rows);
        assert_eq!(l.iter().filter(|s| *s == "Copy").count(), 1, "{l:?}");
        assert_eq!(l.iter().filter(|s| *s == "Properties").count(), 1, "{l:?}");
        let seven = l.iter().position(|s| s == "7-Zip").unwrap();
        let vs = l.iter().position(|s| s == "VS Code").unwrap();
        assert!(seven < vs && l[vs - 1] == "--");
        assert!(rows.iter().all(|r| r.is_separator() || r.source != ItemSource::Native || r.label == "7-Zip"));
        // a hidden built-in frees its verb: the shell's Copy shows instead
        let mut e2 = entries.clone();
        e2.iter_mut().find(|e| e.id == "builtin.copy").unwrap().hidden = true;
        let (ts2, _) = own_items(&e2, Target::File, "txt");
        let rows = compose(&ts2, &native, &[], &e2);
        assert!(rows.iter().any(|r| r.source == ItemSource::Native && r.label == "Copy"));
    }

    #[test]
    fn own_items_follow_target_and_user_entries() {
        let mut entries = default_entries();
        entries.push(MenuEntry { show_for: ShowFor { files: false, folders: false, background: true }, ..MenuEntry::custom("custom.1", "Terminal", "wt.exe", "-d {path}") });
        let (_, user) = own_items(&entries, Target::Background, "");
        assert_eq!(labels(&user), ["Terminal"]);
        assert_eq!(user[0].action, "custom.1");
        let (_, user) = own_items(&entries, Target::File, "txt");
        assert!(user.is_empty());
        let e = native_entry(&build_native(&[verb(raw(1, "Open"), "open")], &mut IconCache::new(2))[0]);
        assert_eq!((e.id.as_str(), e.source, e.name.as_str()), ("windows.verb:open", Source::Windows, "Open"));
    }

    #[test]
    fn model_drops_stale_generations_and_fills_lazy_submenus() {
        let mut m = MenuModel::new(7, vec![MenuItem::command(ItemSource::Tsubame, "Copy path", "file.copy-path")], vec![]);
        assert_eq!(m.state, LoadState::Loading);
        assert_eq!(labels(&m.rows(&[])), ["Copy path"], "own items at t = 0");
        let mut icons = IconCache::new(4);
        let native = build_native(&[raw(1, "7-Zip"), RawItem { text: "Open with".into(), submenu: Some(vec![]), sub: 3, ..Default::default() }], &mut icons);
        assert!(!m.apply(MenuBatch { generation: 6, part: Part::Native(native.clone()), timing: [0; 3] }), "stale");
        assert!(m.native.is_empty());
        assert!(m.apply(MenuBatch { generation: 7, part: Part::Native(native), timing: [0; 3] }));
        assert_eq!(m.state, LoadState::Ready);
        assert!(m.native[1].flags & flag::LAZY != 0 && is_lazy(&m.native[1]) && m.native[1].sub == 3);
        assert!(!is_lazy(&m.native[0]));
        let kids = build_native(&[raw(30, "Notepad"), RawItem { text: "Deeper".into(), submenu: Some(vec![]), sub: 4, ..Default::default() }], &mut icons);
        // an unknown token changes nothing
        assert!(m.apply(MenuBatch { generation: 7, part: Part::Submenu { sub: 99, items: kids.clone() }, timing: [0; 3] }));
        assert!(is_lazy(&m.native[1]));
        assert!(m.apply(MenuBatch { generation: 7, part: Part::Submenu { sub: 3, items: kids }, timing: [0; 3] }));
        assert!(!is_lazy(&m.native[1]));
        assert_eq!(find_sub(&m.native, 4).unwrap().label, "Deeper", "nested lazy submenus keep their token");
        assert!(find_sub(&m.native, 0).is_none());
        assert_eq!(m.native[1].flags & flag::LAZY, 0);
        assert_eq!(at_path(&m.native, &[1, 0]).unwrap().label, "Notepad");
        assert!(m.apply(MenuBatch { generation: 7, part: Part::Failed("timeout".into()), timing: [0; 3] }));
        assert_eq!(m.state, LoadState::Failed);
        assert!(m.apply(MenuBatch { generation: 7, part: Part::Icons(vec![(1, px(1)), (2, px(2))]), timing: [0; 3] }));
        assert_eq!(m.icons.len(), 2);
    }

    #[test]
    fn lazy_submenus_are_asked_once_and_go_slow_after_3_s() {
        let mut l = SubLoads::default();
        assert_eq!(l.state(5, 0), SubState::Idle);
        assert!(!l.want(0, 0), "token 0 is never asked for");
        assert!(l.want(5, 100));
        assert!(!l.want(5, 120), "asked once");
        assert!(l.pending());
        assert_eq!(l.state(5, 100 + SLOW_MS - 1), SubState::Loading);
        assert_eq!(l.state(5, 100 + SLOW_MS), SubState::Slow);
        assert!(l.loading(100 + SLOW_MS - 1), "the tick runs until it is slow");
        assert!(!l.loading(100 + SLOW_MS));
        assert_eq!(l.done(5), Some(100), "answer: request time for the latency");
        assert!(!l.pending());
        assert_eq!(l.state(5, 9_999), SubState::Idle);
        assert!(!l.want(5, 10_000), "answered: not asked again");
        // an answer nobody asked for (eager walk) is just recorded
        assert_eq!(l.done(6), None);
        assert!(!l.want(6, 0));
        // Retry: a new generation asks again
        l.clear();
        assert!(l.want(5, 20_000));
    }

    #[test]
    fn icon_cache_is_bounded_lru_and_dedupes_by_content() {
        let mut c = IconCache::new(3);
        let a = c.put(px(1));
        assert_eq!(c.put(px(1)), a, "same pixels, same key");
        assert_eq!(c.len(), 1);
        let b = c.put(px(2));
        let d = c.put(px(3));
        assert!(c.get(a).is_some(), "touch a: b is now oldest");
        let e = c.put(px(4));
        assert_eq!(c.len(), 3);
        assert!(c.get(b).is_none(), "oldest evicted");
        assert!(c.get(a).is_some() && c.get(d).is_some() && c.get(e).is_some());
        for i in 0..1000 {
            c.put(px(100 + i));
        }
        assert_eq!(c.len(), 3);
        assert_eq!(c.entries().len(), 3);
        assert_ne!(icon_hash(&Bitmap { w: 1, h: 2, px: vec![0, 0] }), icon_hash(&Bitmap { w: 2, h: 1, px: vec![0, 0] }));
    }

    #[test]
    fn dump_prints_a_readable_tree() {
        let mut icons = IconCache::new(4);
        let m = build_native(&[verb(raw(1, "&Open\tEnter"), "open"), sep(), RawItem { text: "Send to".into(), submenu: Some(vec![raw(9, "Desktop")]), ..Default::default() }], &mut icons);
        let mut s = String::new();
        dump(&m, &mut s);
        assert_eq!(s, "Open [id 1 verb=open &o]  \\tEnter\n----\nSend to > [id 0] \n  Desktop [id 9] \n");
    }

    #[test]
    fn own_rows_keep_the_escape_hatch_and_place_extras() {
        let entries = default_entries();
        let extra = MenuItem::command(ItemSource::Tsubame, "Copy name", "x.copy-name");
        let rows = own_rows(&entries, Target::File, "txt", std::slice::from_ref(&extra));
        let l = labels(&rows);
        let cp = l.iter().position(|s| s == "Copy as path").unwrap();
        assert_eq!(l[cp + 1], "Copy name", "{l:?}");
        assert!(l.contains(&CLASSIC_LABEL.to_string()), "{l:?}");
        assert!(!l.iter().any(|s| s == "Show more options" || s == "Open with"), "{l:?}");
        assert_eq!(rows.iter().find(|r| r.label == CLASSIC_LABEL).unwrap().action, ACTION_CLASSIC);
        assert!(!rows[0].is_separator() && !rows.last().unwrap().is_separator());
        // background: no built-in applies, the extras still show; a hidden
        // escape hatch goes
        let rows = own_rows(&entries, Target::Background, "", std::slice::from_ref(&extra));
        assert_eq!(labels(&rows), ["Copy name", "--", CLASSIC_LABEL]);
        let mut e2 = entries.clone();
        e2.iter_mut().find(|e| e.id == ACTION_CLASSIC).unwrap().hidden = true;
        e2.iter_mut().find(|e| e.id == "builtin.copy-path").unwrap().hidden = true;
        let rows = own_rows(&e2, Target::File, "txt", std::slice::from_ref(&extra));
        let l = labels(&rows);
        assert!(!l.contains(&CLASSIC_LABEL.to_string()));
        assert!(l.contains(&"Copy name".to_string()), "extras land at the end of the first group: {l:?}");
        // a renamed escape hatch keeps the user's name
        e2.iter_mut().find(|e| e.id == ACTION_CLASSIC).unwrap().hidden = false;
        e2.iter_mut().find(|e| e.id == ACTION_CLASSIC).unwrap().name = "Old menu".into();
        assert!(labels(&own_rows(&e2, Target::Folder, "", &[])).contains(&"Old menu".to_string()));
    }

    #[test]
    fn loading_turns_into_the_failure_row_after_3_s_and_complete_ends_it() {
        assert_eq!(shown_state(LoadState::Loading, 0), LoadState::Loading);
        assert_eq!(shown_state(LoadState::Loading, SLOW_MS - 1), LoadState::Loading);
        assert_eq!(shown_state(LoadState::Loading, SLOW_MS), LoadState::Failed);
        assert_eq!(shown_state(LoadState::Ready, 60_000), LoadState::Ready);
        let mut m = MenuModel::new(3, vec![], vec![]);
        assert!(!m.complete);
        assert!(m.apply(MenuBatch { generation: 3, part: Part::Native(vec![]), timing: [0; 3] }));
        assert!(!m.complete, "top level only: submenus still filling");
        assert!(!m.apply(MenuBatch { generation: 2, part: Part::Complete, timing: [0; 3] }));
        assert!(m.apply(MenuBatch { generation: 3, part: Part::Complete, timing: [0; 3] }));
        assert!(m.complete && m.state == LoadState::Ready);
        // a menu with no shell answer at all is complete once it failed
        let mut m = MenuModel::new(4, vec![], vec![]);
        m.apply(MenuBatch { generation: 4, part: Part::Failed("x".into()), timing: [0; 3] });
        assert!(m.complete && m.state == LoadState::Failed);
    }

    #[test]
    fn copy_as_path_is_not_shown_twice() {
        let entries = default_entries();
        let (ts, _) = own_items(&entries, Target::File, "txt");
        let native = build_native(&[verb(raw(9, "Copy as path"), "copyaspath"), raw(10, "7-Zip")], &mut IconCache::new(2));
        let l = labels(&compose(&ts, &native, &[], &entries));
        assert_eq!(l.iter().filter(|s| *s == "Copy as path").count(), 1, "{l:?}");
    }

    #[test]
    fn packaged_items_merge_after_classic_and_before_user_without_duplicates() {
        let mut icons = IconCache::new(4);
        let ts = vec![MenuItem::command(ItemSource::Tsubame, "Open", "file.open")];
        let native = build_native(&[raw(1, "Open &with"), verb(raw(2, "Edit in Notepad"), "{CA6CC9F1-867A-481E-951E-A28C5E4F01EA}"), raw(3, "Properties")], &mut icons);
        let mut pkg_sub = verb(raw(0, "More tools"), "{bbbb}");
        pkg_sub.submenu = Some(vec![]);
        pkg_sub.sub = 0x1_0000;
        let pkg = build_native(
            &[verb(raw(0xA000, "Open in &Terminal"), "{9f156763-7844-4dc4-b2b1-901f640f5155}"), verb(raw(0xA001, "Edit in Notepad"), "{ca6cc9f1-867a-481e-951e-a28c5e4f01ea}"), verb(raw(0xA002, "open with"), "{dddd}"), sep(), pkg_sub],
            &mut icons,
        );
        let user = vec![MenuItem::command(ItemSource::User, "My tool", "custom.1")];
        let l = labels(&compose_all(&ts, &native, &pkg, &user, &[]));
        assert_eq!(l, ["Open", "--", "Open with", "Edit in Notepad", "Properties", "--", "Open in Terminal", "--", "More tools", "--", "My tool"], "one menu: classic, packaged, user");
        // ids route to the worker that holds the packaged command
        let all = compose_all(&ts, &native, &pkg, &user, &[]);
        let term = all.iter().find(|i| i.label == "Open in Terminal").unwrap();
        assert_eq!(route(term), Route::Shell(0xA000));
        assert_eq!(term.key, "windows.verb:{9f156763-7844-4dc4-b2b1-901f640f5155}", "the CLSID keys the user's edits");
        assert_eq!(term.accel, Some('t'));
        // hidden by the user like any native item
        let hide = vec![MenuEntry { id: term.key.clone(), source: Source::Windows, hidden: true, ..Default::default() }];
        assert!(!labels(&compose_all(&ts, &native, &pkg, &user, &hide)).contains(&"Open in Terminal".to_string()));
        // no packaged items: no stray separator
        assert_eq!(labels(&compose_all(&ts, &native, &[], &[], &[])), labels(&compose(&ts, &native, &[], &[])));
        // a model: packaged batch after the top level, its lazy submenu fills
        let mut m = MenuModel::new(5, ts.clone(), vec![]);
        assert!(m.apply(MenuBatch { generation: 5, part: Part::Native(native.clone()), timing: [0; 3] }));
        assert!(m.packaged_us.is_none());
        assert!(!m.apply(MenuBatch { generation: 4, part: Part::Packaged(pkg.clone()), timing: [0; 3] }), "stale");
        assert!(m.apply(MenuBatch { generation: 5, part: Part::Packaged(pkg), timing: [0, 0, 1234] }));
        assert_eq!(m.packaged_us, Some(1234));
        assert!(is_lazy(find_sub(&m.packaged, 0x1_0000).unwrap()));
        m.apply(MenuBatch { generation: 5, part: Part::Submenu { sub: 0x1_0000, items: vec![MenuItem { id: 0xA003, ..MenuItem::command(ItemSource::Native, "Sub tool", "") }] }, timing: [0; 3] });
        let rows = m.rows(&[]);
        let more = rows.iter().find(|i| i.label == "More tools").unwrap();
        assert!(!is_lazy(more) && more.children[0].label == "Sub tool");
    }
}
