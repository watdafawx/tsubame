//! Tier 1 extension packs (`docs/extension-packs.md`): declarative TOML files from
//! `<data dir>\packs\`, registered through the extension registry as `pack.<name>`.
//!
//! - **Safe by construction.** A pack is data. It can start a program through the same
//!   argument-vector path as a custom context-menu entry (`custom_cmd`, never a shell),
//!   recolour the file list and add a theme. It can not run code in Tsubame.
//! - **A bad pack is skipped, never a crash.** The parser (`tsubame_config::packs`) names the
//!   file and the key; the other packs still load; Settings > Advanced lists the errors.
//! - **Cheap.** No folder, no cost: one failed `read_dir` at start-up. Nothing watches the
//!   folder; packs are read at start-up and when "Reload packs" is pressed.
//!
//! Registration: each pack is one extension `pack.<name>` (disable it with `[extensions]
//! disabled = ["pack.<name>"]`) whose items are `pack.<name>.<id>`: commands (numbers from
//! `next_command_id`, the handler is [`run_command`]), context-menu items, file-type rules and
//! themes. What the registry can not hold (the program and arguments, a ready `MenuEntry`)
//! lives in this module's per-thread state, keyed by those ids.

use crate::custom_cmd::{self, Argv, Ctx};
use crate::ext::{self, App};
use crate::lru::Lru;
use crate::view::View;
use std::borrow::Cow;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tsubame_config::menu::{MenuEntry, ShowFor, Source};
use tsubame_config::packs::{self as cfg, Checks, Pack};
use tsubame_ext_api::{CmdFlags, CommandSpec, Contributions, Extension, ExtensionInfo, FileTypeRule, MenuItemSpec, MenuWhen, Text, ThemeSpec, Tier};
use tsubame_ui::{Canvas, Rect, Theme, Ui, f, themes};

/// The folder inside the data directory.
pub const DIR_NAME: &str = "packs";
/// Packs read; more are reported and ignored.
pub const MAX_PACKS: usize = 32;
/// Hard caps on decoded pack icon bitmaps (PLAN.md §3 rule 5).
pub const PACK_ICON_CAP_ITEMS: usize = 64;
pub const PACK_ICON_CAP_BYTES: usize = 1 << 20;
/// Maximum edge length (px) kept for a pack icon bitmap.
pub const PACK_ICON_MAX_PX: u32 = 64;

/// What the Settings page shows for one loaded pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackInfo {
    pub file: String,
    /// registry extension id: `pack.<name>`
    pub id: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub commands: usize,
    pub menus: usize,
    pub themes: usize,
    pub rules: usize,
}

/// The result of the last [`load`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub dir: PathBuf,
    pub dir_exists: bool,
    pub packs: Vec<PackInfo>,
    /// a pack that was skipped: `file: key: what is wrong`
    pub errors: Vec<String>,
    /// a pack that loaded with a part left out (a key chord taken by another command)
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    title: String,
    program: String,
    args: String,
}

struct PackBitmap {
    /// Device cache key (includes reload sequence so Reload packs refreshes D2D).
    key: Arc<str>,
    w: u32,
    h: u32,
    px: Vec<u32>,
}

impl PackBitmap {
    fn bytes(&self) -> usize {
        self.px.len() * 4 + self.key.len() + 48
    }
}

struct State {
    report: Report,
    /// command id -> what to run, sorted by id
    cmds: Vec<(u32, Run)>,
    /// registry menu item id -> the entry the shell menu gets
    menus: Vec<(String, MenuEntry)>,
    /// bounded cache of decoded pack icon bitmaps, keyed by the rule's `icon` text
    icon_cache: Lru<Arc<str>, Arc<PackBitmap>>,
    /// bumped on every `load()` so D2D bitmap keys never collide across reloads
    load_seq: u64,
    /// registry generation the theme list and the menu were last published for
    synced: Option<u64>,
}

impl Default for State {
    fn default() -> Self {
        State {
            report: Report::default(),
            cmds: Vec::new(),
            menus: Vec::new(),
            icon_cache: Lru::new(PACK_ICON_CAP_ITEMS, PACK_ICON_CAP_BYTES),
            load_seq: 0,
            synced: None,
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn checks_run<R>(f: impl FnOnce(&Checks) -> R) -> R {
    let args = |a: &str| custom_cmd::split_args(a).map(drop);
    let chord = |c: &str| {
        let ch = crate::keymap::parse_chord(c).ok_or_else(|| format!("cannot read \"{c}\" as a key (write it like Ctrl+Alt+T)"))?;
        crate::keymap::bindable(ch).map_err(|why| format!("\"{c}\" cannot be bound ({why:?})"))
    };
    f(&Checks { args: &args, chord: &chord })
}

// ---- the extension ---------------------------------------------------------------------

struct PackExt {
    pack: Pack,
    /// command numbers, parallel to `pack.commands`
    ids: Vec<u32>,
}

fn pid(name: &str, id: &str) -> String {
    format!("pack.{name}.{id}")
}

impl Extension<App> for PackExt {
    fn info(&self) -> ExtensionInfo {
        ExtensionInfo { id: Cow::Owned(format!("pack.{}", self.pack.name)), title: Cow::Owned(self.pack.title.clone()), tier: Tier::Pack, required: false }
    }

    fn contribute(&self, c: &mut Contributions<App>) {
        let p = &self.pack;
        for (cmd, &id) in p.commands.iter().zip(&self.ids) {
            let flags = if cmd.needs_folder { CmdFlags::NEEDS_FOLDER } else { CmdFlags::NONE };
            let flags = if cmd.global { flags | CmdFlags::GLOBAL } else { flags };
            let keys: Vec<Text> = cmd.keys.iter().map(|k| Cow::Owned(k.clone())).collect();
            let spec = CommandSpec { id, name: Cow::Owned(pid(&p.name, &cmd.id)), title: Cow::Owned(cmd.title.clone()), keys: Cow::Owned(keys), flags };
            c.command(spec, run_command);
        }
        for m in &p.menus {
            let mut when = 0u8;
            for (on, bit) in [(m.files, MenuWhen::FILES), (m.folders, MenuWhen::FOLDERS), (m.background, MenuWhen::BACKGROUND), (m.multiple, MenuWhen::SEVERAL), (m.drives, MenuWhen::DRIVES)] {
                if on {
                    when |= bit.0;
                }
            }
            c.menu_item(MenuItemSpec {
                id: Cow::Owned(pid(&p.name, &m.id)),
                label: Cow::Owned(m.label.clone()),
                command: Cow::Owned(m.command.as_ref().map(|c| pid(&p.name, c)).unwrap_or_default()),
                when: MenuWhen(when),
                types: Cow::Owned(m.types.iter().map(|t| Cow::Owned(t.clone())).collect()),
            });
        }
        for f in &p.file_types {
            c.file_type(FileTypeRule {
                id: Cow::Owned(pid(&p.name, &f.id)),
                extensions: Cow::Owned(f.extensions.iter().map(|e| Cow::Owned(e.clone())).collect()),
                color: f.color,
                icon: f.icon.clone().map(Cow::Owned),
                open_with: f.open_with.as_ref().map(|(prog, args)| Cow::Owned(if args.is_empty() { quote(prog) } else { format!("{} {args}", quote(prog)) })),
            });
        }
        for t in &p.themes {
            let mut colors: Vec<(Text, u32)> = Vec::new();
            for (name, v) in [("bg", t.bg), ("panel", t.panel), ("surface", t.surface), ("text", t.text), ("muted", t.muted), ("accent", t.accent)] {
                if let Some(v) = v {
                    colors.push((Cow::Borrowed(name), v));
                }
            }
            c.theme(ThemeSpec { id: Cow::Owned(pid(&p.name, &t.id)), name: Cow::Owned(t.name.clone()), dark: t.dark, colors: Cow::Owned(colors) });
        }
    }
}

fn quote(p: &str) -> String {
    if p.contains(char::is_whitespace) { format!("\"{p}\"") } else { p.to_string() }
}

// ---- loading ---------------------------------------------------------------------------

/// Removes every pack from the registry and forgets what was read.
fn unload() {
    let ids: Vec<String> = ext::read(|r| r.extensions().filter(|(i, _)| i.tier == Tier::Pack).map(|(i, _)| i.id.to_string()).collect());
    if !ids.is_empty() {
        ext::write(|r| {
            for id in &ids {
                r.remove(id);
            }
            r.compact();
        });
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let next_seq = s.load_seq.wrapping_add(1);
        *s = State { load_seq: next_seq, ..State::default() };
    });
}

/// Reads `<data_dir>\packs\*.toml` and registers every good pack. Replaces what an earlier
/// call loaded. A missing folder is the normal case and costs one failed `read_dir`.
pub fn load(data_dir: &Path) {
    unload();
    // no data directory known (a test view): never look in the working directory
    if data_dir.as_os_str().is_empty() {
        return;
    }
    let dir = data_dir.join(DIR_NAME);
    let mut report = Report { dir: dir.clone(), ..Default::default() };
    match std::fs::read_dir(&dir) {
        Ok(rd) => {
            report.dir_exists = true;
            let mut files: Vec<(String, PathBuf)> = rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    (name.to_ascii_lowercase().ends_with(".toml") && e.file_type().is_ok_and(|t| t.is_file())).then(|| (name, e.path()))
                })
                .collect();
            files.sort();
            for (i, (name, path)) in files.into_iter().enumerate() {
                if i >= MAX_PACKS {
                    report.errors.push(format!("{name}: ignored, Tsubame reads at most {MAX_PACKS} packs"));
                    continue;
                }
                match read_pack(&name, &path) {
                    Ok(pack) => register(&dir, &name, pack, &mut report),
                    Err(e) => report.errors.push(e),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => report.errors.push(format!("{}: {e}", dir.display())),
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.cmds.sort_by_key(|c| c.0);
        s.report = report;
    });
}

fn read_pack(file: &str, path: &Path) -> Result<Pack, String> {
    let len = std::fs::metadata(path).map_err(|e| format!("{file}: {e}"))?.len();
    if len > cfg::MAX_FILE_BYTES {
        return Err(format!("{file}: is larger than {} KB", cfg::MAX_FILE_BYTES / 1024));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{file}: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{file}: is not valid UTF-8 text"))?;
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    checks_run(|c| cfg::parse(file, stem, &text, c)).map_err(|e| e.to_string())
}

fn is_icon_path(spec: &str) -> bool {
    let s = spec.trim();
    let lower = s.to_ascii_lowercase();
    s.contains(['/', '\\']) || lower.ends_with(".ico") || lower.ends_with(".png")
}

fn resolve_icon_path(packs_dir: &Path, spec: &str) -> Option<PathBuf> {
    let p = Path::new(spec.trim());
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::Prefix(_) | std::path::Component::RootDir)) {
        return None;
    }
    Some(packs_dir.join(p))
}

fn register(packs_dir: &Path, file: &str, mut pack: Pack, report: &mut Report) {
    // a default chord another command already owns is dropped (the pack still loads)
    let taken = crate::keymap::KeyMap::defaults();
    let mut mine: Vec<crate::keymap::Chord> = Vec::new();
    for cmd in &mut pack.commands {
        let mut keep = Vec::new();
        for k in std::mem::take(&mut cmd.keys) {
            let Some(ch) = crate::keymap::parse_chord(&k) else { continue };
            if let Some(owner) = taken.conflict(ch, 0).or_else(|| mine.contains(&ch).then_some(crate::keymap::Conflict { command: 0 })) {
                let by = taken.commands().iter().find(|c| c.id == owner.command).map(|c| c.name.to_string()).unwrap_or_else(|| "another command of this pack".into());
                report.warnings.push(format!("{file}: command {}: {k} is already used by {by}, left unbound", cmd.id));
                continue;
            }
            mine.push(ch);
            keep.push(k);
        }
        cmd.keys = keep;
    }
    let base = ext::read(|r| r.next_command_id());
    let ids: Vec<u32> = (0..pack.commands.len() as u32).map(|i| base + i).collect();
    let ext_pack = PackExt { pack, ids };
    let added = ext::write(|r| r.add(&ext_pack));
    if let Err(e) = added {
        report.errors.push(format!("{file}: {e}"));
        return;
    }
    let p = &ext_pack.pack;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        for (cmd, &id) in p.commands.iter().zip(&ext_pack.ids) {
            s.cmds.push((id, Run { title: cmd.title.clone(), program: cmd.program.clone(), args: cmd.args.clone() }));
        }
        for m in &p.menus {
            let show_for = ShowFor { files: m.files, folders: m.folders, background: m.background, multiple: m.multiple, drives: m.drives };
            let e = MenuEntry { id: pid(&p.name, &m.id), source: Source::Pack, name: m.label.clone(), command: m.program.clone(), args: m.args.clone(), extensions: m.types.clone(), show_for, ..Default::default() };
            s.menus.push((e.id.clone(), e));
        }
        let seq = s.load_seq;
        for f in &p.file_types {
            let Some(spec) = f.icon.as_deref() else { continue };
            if !is_icon_path(spec) || s.icon_cache.contains(spec) {
                continue;
            }
            let Some(resolved) = resolve_icon_path(packs_dir, spec) else {
                report.warnings.push(format!("{file}: file_type {}: icon path \"{spec}\" must not use \"..\"", f.id));
                continue;
            };
            match tsubame_win::bitmap::load_file(&resolved, PACK_ICON_MAX_PX) {
                Some(bmp) => {
                    let img = Arc::new(PackBitmap {
                        key: Arc::from(format!("pack-icon:{seq}:{spec}")),
                        w: bmp.w,
                        h: bmp.h,
                        px: bmp.px,
                    });
                    let bytes = img.bytes();
                    s.icon_cache.insert(Arc::from(spec), img, bytes);
                }
                None => {
                    report.warnings.push(format!("{file}: file_type {}: could not load icon \"{spec}\"", f.id));
                }
            }
        }
    });
    report.packs.push(PackInfo {
        file: file.to_string(),
        id: format!("pack.{}", p.name),
        title: p.title.clone(),
        version: p.version.clone(),
        description: p.description.clone(),
        commands: p.commands.len(),
        menus: p.menus.len(),
        themes: p.themes.len(),
        rules: p.file_types.len(),
    });
}

/// The last load's packs and errors.
pub fn report() -> Report {
    STATE.with(|s| s.borrow().report.clone())
}

// ---- publishing what the rest of the app reads ----------------------------------------------

/// Publishes the pack themes to the theme picker and says whether the registry changed since
/// the last call (the caller then refreshes the shell menu). Cheap when nothing changed.
pub fn sync() -> bool {
    let g = ext::generation();
    if STATE.with(|s| s.borrow().synced) == Some(g) {
        return false;
    }
    let list: Vec<themes::PackPalette> = ext::read(|r| {
        r.themes
            .iter()
            .map(|t| {
                let base = themes::tsubame(t.dark);
                let c = |name: &str, d: u32| t.colors.iter().find(|(n, _)| &**n == name).map(|(_, v)| *v).unwrap_or(d);
                themes::PackPalette {
                    id: t.id.to_string(),
                    name: t.name.to_string(),
                    dark: t.dark,
                    bg: c("bg", base.bg),
                    panel: c("panel", base.panel),
                    surface: c("surface", base.surface),
                    text: c("text", base.text),
                    muted: c("muted", base.muted),
                    accent: c("accent", base.accent),
                }
            })
            .collect()
    });
    themes::set_extra(&list);
    STATE.with(|s| s.borrow_mut().synced = Some(g));
    true
}

/// The enabled pack entries of the context menu, in registry order.
pub fn menu_entries() -> Vec<MenuEntry> {
    STATE.with(|s| {
        let s = s.borrow();
        if s.menus.is_empty() {
            return Vec::new();
        }
        ext::read(|r| r.menu_items.iter().filter_map(|m| s.menus.iter().find(|(id, _)| *id == *m.id).map(|(_, e)| e.clone())).collect())
    })
}

// ---- running a command ----------------------------------------------------------------------

/// The programs a pack command starts for `paths` shown in `dir`: one with every path when
/// the arguments use `{selection}`, else one per item (as a custom menu entry does).
fn argvs(run: &Run, paths: &[String], dir: &str) -> Result<Vec<Argv>, String> {
    let e = MenuEntry::custom("pack", &run.title, &run.program, &run.args);
    custom_cmd::build_all(&e, &Ctx { paths, dir })
}

/// The handler of every pack command (`Host::Run`): looks the command up and queues the
/// programs; the app starts them after the frame (`shellops::after_frame`).
pub fn run_command(v: &mut View, _ui: &mut Ui, id: u32) {
    let Some(run) = STATE.with(|s| {
        let s = s.borrow();
        s.cmds.binary_search_by_key(&id, |c| c.0).ok().map(|i| s.cmds[i].1.clone())
    }) else {
        return;
    };
    let t = v.tab();
    let (paths, dir) = if t.settings || t.path.is_empty() { (Vec::new(), tsubame_win::places::home_folder()) } else { (t.selected_paths(crate::shellops::MAX_ITEMS), t.path.clone()) };
    match argvs(&run, &paths, &dir) {
        Ok(a) if a.len() > custom_cmd::CONFIRM_ABOVE => {
            v.ops.message = Some(format!("{}: {} items selected, too many to start at once", run.title, a.len()));
        }
        Ok(a) => {
            v.pack_runs.push(a);
            v.ops.message = Some(format!("Started {}", run.title));
        }
        Err(m) => v.ops.message = Some(m),
    }
}

// ---- file-type rules -------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum CompiledIcon {
    /// Index into `FtCache::bitmaps`.
    Bitmap(usize),
    /// UTF-8 icon-font glyph (Segoe Fluent Icons) + optional rule colour.
    Glyph { buf: [u8; 4], len: u8, color: Option<u32> },
    /// Short uppercase ASCII badge (1-3 chars) + optional rule colour.
    Badge { buf: [u8; 3], len: u8, color: Option<u32> },
}

fn builtin_glyph(spec: &str) -> Option<&'static str> {
    match spec.trim().to_ascii_lowercase().as_str() {
        "code" => Some("\u{E943}"),
        "doc" | "document" | "text" => Some("\u{E8A5}"),
        "image" | "picture" | "photo" => Some("\u{EB9F}"),
        "archive" | "zip" => Some("\u{F012}"),
        "audio" | "music" => Some("\u{E8D6}"),
        "video" | "film" => Some("\u{E714}"),
        "terminal" | "shell" => Some("\u{E756}"),
        "config" | "gear" | "settings" => Some("\u{E713}"),
        "data" | "table" => Some("\u{E9D2}"),
        "book" | "markdown" => Some("\u{E82D}"),
        "lock" | "key" => Some("\u{E72E}"),
        "web" | "globe" => Some("\u{E774}"),
        "link" => Some("\u{E71B}"),
        "star" => Some("\u{E734}"),
        "tag" => Some("\u{E8EC}"),
        "font" => Some("\u{E8D2}"),
        "app" | "program" => Some("\u{E737}"),
        "pdf" => Some("\u{EA90}"),
        "bug" => Some("\u{EBE8}"),
        "tool" | "wrench" => Some("\u{EC7A}"),
        "file" => Some("\u{E7C3}"),
        "folder" => Some("\u{E8B7}"),
        _ => None,
    }
}

fn compile_icon(spec: &str, color: Option<u32>, bitmaps: &mut Vec<Arc<PackBitmap>>) -> Option<CompiledIcon> {
    let s = spec.trim();
    if s.is_empty() {
        return None;
    }
    if is_icon_path(s) {
        let bmp = STATE.with(|st| st.borrow_mut().icon_cache.get(s).cloned())?;
        let idx = match bitmaps.iter().position(|b| Arc::ptr_eq(b, &bmp)) {
            Some(i) => i,
            None => {
                let i = bitmaps.len();
                bitmaps.push(bmp);
                i
            }
        };
        return Some(CompiledIcon::Bitmap(idx));
    }
    if let Some(g) = builtin_glyph(s) {
        let mut buf = [0u8; 4];
        let len = g.len().min(4);
        buf[..len].copy_from_slice(&g.as_bytes()[..len]);
        return Some(CompiledIcon::Glyph { buf, len: len as u8, color });
    }
    if let Some(hex) = s.strip_prefix("u+").or_else(|| s.strip_prefix("U+"))
        && let Ok(cp) = u32::from_str_radix(hex, 16)
        && (0xE000..=0xF8FF).contains(&cp)
        && let Some(ch) = char::from_u32(cp)
    {
        let mut buf = [0u8; 4];
        let enc = ch.encode_utf8(&mut buf);
        let len = enc.len() as u8;
        return Some(CompiledIcon::Glyph { buf, len, color });
    }
    // Crisp 1-3 char badge (`badge:RS` or a short plain name like `rust`, `py`, `zig`)
    let explicit = s.strip_prefix("badge:").or_else(|| s.strip_prefix("BADGE:"));
    let raw = explicit.unwrap_or(s);
    let max_len = if explicit.is_some() || raw.len() <= 3 { 3 } else { 2 };
    let mut buf = [0u8; 3];
    let mut len = 0usize;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            buf[len] = ch.to_ascii_uppercase() as u8;
            len += 1;
            if len == max_len {
                break;
            }
        }
    }
    (len > 0).then_some(CompiledIcon::Badge { buf, len: len as u8, color })
}

#[derive(Default)]
struct FtCache {
    generation: Option<u64>,
    load_seq: u64,
    /// extension -> name colour, first rule wins
    colors: Vec<(String, u32)>,
    /// extension -> program and arguments
    opens: Vec<(String, String, String)>,
    /// extension -> compiled icon, first rule wins
    icons: Vec<(String, CompiledIcon)>,
    /// decoded bitmaps referenced by `CompiledIcon::Bitmap`
    bitmaps: Vec<Arc<PackBitmap>>,
}

thread_local! {
    static FT: RefCell<FtCache> = RefCell::new(FtCache::default());
}

fn with_rules<R>(f: impl FnOnce(&FtCache) -> R) -> R {
    let g = ext::generation();
    let seq = STATE.with(|s| s.borrow().load_seq);
    FT.with(|c| {
        let mut c = c.borrow_mut();
        if c.generation != Some(g) || c.load_seq != seq {
            c.colors.clear();
            c.opens.clear();
            c.icons.clear();
            c.bitmaps.clear();
            ext::read(|r| {
                for rule in r.file_types.iter() {
                    let compiled_icon = rule.icon.as_deref().and_then(|spec| compile_icon(spec, rule.color, &mut c.bitmaps));
                    for e in rule.extensions.iter() {
                        if let Some(col) = rule.color
                            && !c.colors.iter().any(|(x, _)| x == &**e)
                        {
                            c.colors.push((e.to_string(), col));
                        }
                        if let Some((prog, args)) = rule.open_with.as_deref().and_then(cfg::split_program)
                            && !c.opens.iter().any(|(x, _, _)| x == &**e)
                        {
                            c.opens.push((e.to_string(), prog, args));
                        }
                        if let Some(ic) = compiled_icon
                            && !c.icons.iter().any(|(x, _)| x == &**e)
                        {
                            c.icons.push((e.to_string(), ic));
                        }
                    }
                }
            });
            c.generation = Some(g);
            c.load_seq = seq;
        }
        f(&c)
    })
}

fn ext_of(name: &str) -> Option<&str> {
    name.rfind('.').filter(|&i| i > 0 && i + 1 < name.len()).map(|i| &name[i + 1..])
}

/// The colour a rule gives the name of a file, if one does. Folders have none. No rules: two
/// thread-local reads and an empty-list check, no allocation.
pub fn name_color(name: &str, dir: bool) -> Option<u32> {
    if dir {
        return None;
    }
    with_rules(|c| {
        if c.colors.is_empty() {
            return None;
        }
        let e = ext_of(name)?;
        c.colors.iter().find(|(x, _)| x.eq_ignore_ascii_case(e)).map(|(_, col)| *col)
    })
}

/// Whether an enabled pack file-type rule draws an icon for `name`. Zero allocation on the hot path.
pub fn has_icon(name: &str, dir: bool) -> bool {
    if dir {
        return false;
    }
    with_rules(|c| {
        if c.icons.is_empty() {
            return false;
        }
        let Some(e) = ext_of(name) else { return false };
        c.icons.iter().any(|(x, _)| x.eq_ignore_ascii_case(e))
    })
}

/// Draws a pack file-type icon for `name` inside `r` when a rule defines one. Returns `true` if
/// drawn (the caller skips the shell icon). Zero heap allocation and zero disk I/O per row draw.
pub fn draw_icon(g: &mut dyn Canvas, t: &Theme, name: &str, dir: bool, hidden: bool, r: Rect) -> bool {
    if dir {
        return false;
    }
    with_rules(|c| {
        if c.icons.is_empty() {
            return false;
        }
        let Some(e) = ext_of(name) else { return false };
        let Some((_, icon)) = c.icons.iter().find(|(x, _)| x.eq_ignore_ascii_case(e)) else {
            return false;
        };
        match icon {
            CompiledIcon::Bitmap(idx) => {
                let bmp = &c.bitmaps[*idx];
                if hidden {
                    g.image_a(&bmp.key, bmp.w, bmp.h, &bmp.px, r, 0.55);
                } else {
                    g.image(&bmp.key, bmp.w, bmp.h, &bmp.px, r);
                }
            }
            CompiledIcon::Glyph { buf, len, color } => {
                let s = std::str::from_utf8(&buf[..*len as usize]).unwrap_or("");
                let col = if hidden { t.text_dim } else { color.unwrap_or(t.accent) };
                let sz = (r.h * 0.88).max(12.0);
                g.text(s, r, f(sz).icon().c(), col);
            }
            CompiledIcon::Badge { buf, len, color } => {
                let s = std::str::from_utf8(&buf[..*len as usize]).unwrap_or("");
                let col = if hidden { t.text_dim } else { color.unwrap_or(t.accent) };
                let rad = (r.h * 0.22).clamp(2.5, 6.0);
                g.round_a(r, rad, col, 0.18);
                g.round_stroke(r, rad, col, 1.0);
                let scale = if *len >= 3 { 0.42 } else { 0.50 };
                let sz = (r.h * scale).max(7.0);
                g.text(s, r, f(sz).b().c(), col);
            }
        }
        true
    })
}

/// The programs to open file `path` with when a rule says so (None: use the shell's default).
pub fn open_with(path: &str) -> Option<Result<Vec<Argv>, String>> {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let ext = ext_of(name)?;
    let (prog, args) = with_rules(|c| c.opens.iter().find(|(x, _, _)| x.eq_ignore_ascii_case(ext)).map(|(_, p, a)| (p.clone(), a.clone())))?;
    let dir = match path.rfind(['\\', '/']) {
        Some(2) if path.as_bytes().get(1) == Some(&b':') => &path[..3],
        Some(i) => &path[..i],
        None => "",
    };
    let e = MenuEntry::custom("pack.open", "Open", &prog, &args);
    Some(custom_cmd::build_all(&e, &Ctx { paths: std::slice::from_ref(&path.to_string()), dir }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::KeyMap;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-x2-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(DIR_NAME)).unwrap();
        d
    }

    fn put(d: &Path, name: &str, text: &str) {
        std::fs::write(d.join(DIR_NAME).join(name), text).unwrap();
    }

    const TERM: &str = r##"
[pack]
title = "Open in terminal"

[[command]]
id = "terminal"
title = "Open terminal here"
program = "wt.exe"
args = "-d {dir}"
keys = ["Ctrl+Alt+Shift+T"]

[[menu]]
id = "terminal"
label = "Terminal here"
command = "terminal"
show = ["folders", "background"]

[[theme]]
id = "mine"
name = "Mine"
dark = false
bg = "#fdf6e3"
accent = "#268bd2"

[[file_type]]
id = "rust"
extensions = ["rs", "RON"]
color = "#b58900"
open_with = "code.exe --goto {path}"
"##;

    #[test]
    fn no_folder_costs_nothing_and_changes_nothing() {
        let d = std::env::temp_dir().join(format!("tsubame-x2-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let g = ext::generation();
        load(&d);
        load(Path::new(""));
        assert_eq!(ext::generation(), g, "the registry is not touched");
        let r = report();
        assert!(!r.dir_exists && r.packs.is_empty() && r.errors.is_empty());
        assert_eq!(name_color("a.rs", false), None);
        assert!(menu_entries().is_empty());
    }

    #[test]
    fn a_good_pack_registers_every_point_under_its_id() {
        let d = scratch("good");
        put(&d, "open-in-terminal.toml", TERM);
        load(&d);
        let r = report();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.packs.len(), 1);
        assert_eq!((r.packs[0].id.as_str(), r.packs[0].title.as_str(), r.packs[0].commands, r.packs[0].menus, r.packs[0].themes, r.packs[0].rules), ("pack.open-in-terminal", "Open in terminal", 1, 1, 1, 1));
        ext::read(|reg| {
            let info = reg.extensions().find(|(i, _)| i.id == "pack.open-in-terminal").unwrap().0.clone();
            assert_eq!((info.tier, info.required), (Tier::Pack, false));
            let c = reg.command_named("pack.open-in-terminal.terminal").unwrap();
            assert!(c.spec.id >= tsubame_ext_api::DYNAMIC_COMMAND_BASE);
            assert_eq!(c.spec.keys[0], "Ctrl+Alt+Shift+T");
            assert!(c.spec.flags.has(CmdFlags::NEEDS_FOLDER) && !c.spec.flags.has(CmdFlags::GLOBAL));
            assert!(reg.command(c.spec.id).is_some());
            let m = reg.menu_items.get("pack.open-in-terminal.terminal").unwrap();
            assert_eq!(m.command, "pack.open-in-terminal.terminal");
            assert_eq!(m.when.0, MenuWhen::FOLDERS.0 | MenuWhen::BACKGROUND.0);
            assert!(reg.themes.get("pack.open-in-terminal.mine").is_some());
            let f = reg.file_types.get("pack.open-in-terminal.rust").unwrap();
            assert_eq!(f.extensions.iter().map(|e| &**e).collect::<Vec<_>>(), ["rs", "ron"]);
            assert_eq!(f.open_with.as_deref(), Some("code.exe --goto {path}"));
        });
        // keys: the command is in the key map with its chord, and the palette's list
        let km = KeyMap::defaults();
        let id = ext::read(|reg| reg.command_named("pack.open-in-terminal.terminal").unwrap().spec.id);
        assert_eq!(km.label(id), "Ctrl+Shift+Alt+T", "shown in the app's own spelling");
        // menu entry: the existing custom-entry model, tagged Pack, with the command's program
        let e = menu_entries();
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].id.as_str(), e[0].source, e[0].command.as_str(), e[0].args.as_str()), ("pack.open-in-terminal.terminal", Source::Pack, "wt.exe", "-d {dir}"));
        assert!(e[0].source.runs_program());
        assert!(e[0].matches(tsubame_config::menu::Target::Folder, "") && e[0].matches(tsubame_config::menu::Target::Background, "") && !e[0].matches(tsubame_config::menu::Target::File, "rs"));
        // theme: published to the picker, missing colours from the Tsubame base of the same brightness
        assert!(sync());
        let t = themes::find("pack.open-in-terminal.mine").unwrap();
        assert_eq!((t.bg, t.accent, t.text, t.dark), (0xfdf6e3, 0x268bd2, themes::tsubame(false).text, false));
        assert!(!sync(), "nothing changed since");
        // file types
        assert_eq!(name_color("main.rs", false), Some(0xb58900));
        assert_eq!(name_color("MAIN.RS", false), Some(0xb58900));
        assert_eq!(name_color("x.RON", false), Some(0xb58900));
        assert_eq!(name_color("main.rs", true), None, "folders have no type colour");
        assert_eq!(name_color("main.py", false), None);
        assert_eq!(name_color(".rs", false), None, "a dot file has no extension");
        assert_eq!(name_color("rs", false), None);
        let _ = std::fs::remove_dir_all(&d);
        load(&d);
        assert!(name_color("main.rs", false).is_none() && sync());
        assert!(themes::find("pack.open-in-terminal.mine").is_none(), "the theme left with the pack");
    }

    #[test]
    fn disabling_the_pack_takes_everything_out_live_and_enabling_brings_it_back() {
        let d = scratch("disable");
        put(&d, "open-in-terminal.toml", TERM);
        load(&d);
        sync();
        let id = ext::read(|reg| reg.command_named("pack.open-in-terminal.terminal").unwrap().spec.id);
        assert!(KeyMap::defaults().chords(id).len() == 1);
        let g = ext::generation();
        ext::apply_disabled(&["pack.open-in-terminal".to_string()]);
        assert_ne!(ext::generation(), g);
        assert!(ext::command(id).is_none(), "no longer runs");
        assert!(ext::read(|r| r.command_named("pack.open-in-terminal.terminal").is_none() && r.knows_command("pack.open-in-terminal.terminal")));
        assert!(KeyMap::defaults().commands().iter().all(|c| c.id != id), "its key is free");
        assert!(menu_entries().is_empty());
        assert_eq!(name_color("a.rs", false), None);
        assert!(open_with(r"C:\x\a.rs").is_none());
        assert!(sync());
        assert!(themes::find("pack.open-in-terminal.mine").is_none());
        assert_eq!(ext::read(|r| r.disabled()), ["pack.open-in-terminal"]);
        ext::apply_disabled(&[]);
        assert!(ext::command(id).is_some() && menu_entries().len() == 1 && name_color("a.rs", false).is_some());
        // a pack that is listed as off before it loads starts off
        ext::apply_disabled(&["pack.open-in-terminal".to_string()]);
        load(&d);
        assert_eq!(ext::read(|r| r.is_enabled("pack.open-in-terminal")), Some(false));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bad_pack_is_skipped_with_file_and_key_and_the_others_still_load() {
        let d = scratch("bad");
        put(&d, "a-good.toml", TERM);
        put(&d, "b-typo.toml", "[[command]]\nid=\"x\"\ntitle=\"X\"\nprogram=\"x.exe\"\nkeys=[\"Ctrl+Nope\"]\n");
        put(&d, "c-syntax.toml", "[[command\n");
        put(&d, "D-Upper.toml", "[[command]]\nid=\"x\"\ntitle=\"X\"\nprogram=\"x\"\n");
        put(&d, "e-token.toml", "[[command]]\nid=\"x\"\ntitle=\"X\"\nprogram=\"x\"\nargs=\"{pth}\"\n");
        put(&d, "f-quote.toml", "[[command]]\nid=\"x\"\ntitle=\"X\"\nprogram=\"x\"\nargs=\"\\\"open\"\n");
        put(&d, "g-big.toml", &format!("#{}\n", "x".repeat(cfg::MAX_FILE_BYTES as usize)));
        std::fs::write(d.join(DIR_NAME).join("h-binary.toml"), [0xff, 0xfe, 0x00, 0x80]).unwrap();
        put(&d, "notes.txt", "not a pack");
        std::fs::create_dir(d.join(DIR_NAME).join("i-dir.toml")).unwrap();
        let before = ext::read(|r| r.commands.len());
        load(&d);
        let r = report();
        assert_eq!(r.packs.iter().map(|p| p.file.as_str()).collect::<Vec<_>>(), ["a-good.toml"]);
        let has = |needle: &str| r.errors.iter().any(|e| e.contains(needle));
        assert!(has("b-typo.toml: command[0].keys[0]: cannot read \"Ctrl+Nope\""), "{:?}", r.errors);
        assert!(has("c-syntax.toml: line 1: is not valid TOML"), "{:?}", r.errors);
        assert!(has("D-Upper.toml: the file name \"D-Upper\""), "{:?}", r.errors);
        assert!(has("e-token.toml: command[0].args: unknown token {pth}"), "{:?}", r.errors);
        assert!(has("f-quote.toml: command[0].args: a quote is not closed"), "{:?}", r.errors);
        assert!(has("g-big.toml: is larger than 128 KB"), "{:?}", r.errors);
        assert!(has("h-binary.toml: is not valid UTF-8"), "{:?}", r.errors);
        assert_eq!(r.errors.len(), 7, "{:?}", r.errors);
        // nothing from a bad pack reached the registry
        assert_eq!(ext::read(|r| r.commands.len()), before + 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_chord_another_command_owns_is_dropped_with_a_warning_not_the_pack() {
        let d = scratch("chord");
        put(&d, "p1.toml", "[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nkeys=[\"Ctrl+T\", \"Ctrl+Alt+Shift+A\"]\n");
        put(&d, "p2.toml", "[[command]]\nid=\"b\"\ntitle=\"B\"\nprogram=\"y\"\nkeys=[\"Ctrl+Alt+Shift+A\"]\n");
        load(&d);
        let r = report();
        assert_eq!(r.packs.len(), 2);
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("p1.toml") && r.warnings[0].contains("Ctrl+T is already used by tab.new"), "{:?}", r.warnings);
        assert!(r.warnings[1].contains("p2.toml") && r.warnings[1].contains("Ctrl+Alt+Shift+A is already used by pack.p1.a"), "{:?}", r.warnings);
        let km = KeyMap::defaults();
        let a = ext::read(|r| r.command_named("pack.p1.a").unwrap().spec.id);
        assert_eq!(km.label(a), "Ctrl+Shift+Alt+A");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reload_replaces_the_old_packs() {
        let d = scratch("reload");
        put(&d, "one.toml", "[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\n");
        load(&d);
        assert!(ext::read(|r| r.knows_command("pack.one.a")));
        std::fs::remove_file(d.join(DIR_NAME).join("one.toml")).unwrap();
        put(&d, "two.toml", "[[command]]\nid=\"b\"\ntitle=\"B\"\nprogram=\"x\"\n");
        load(&d);
        assert!(!ext::read(|r| r.knows_command("pack.one.a")));
        assert!(ext::read(|r| r.knows_command("pack.two.b")));
        assert_eq!(report().packs.len(), 1);
        // reloading the same files gives the same ids and does not pile up
        let n = ext::read(|r| r.commands.items().count());
        load(&d);
        load(&d);
        assert_eq!(ext::read(|r| r.commands.items().count()), n);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_registry_keeps_a_packs_state_for_the_built_ins_it_does_not_own() {
        // the built-in extensions are never touched by a reload
        let d = scratch("builtins");
        put(&d, "p.toml", "[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\n");
        let n = ext::read(|r| r.commands.len());
        load(&d);
        load(&d);
        assert_eq!(ext::read(|r| r.commands.len()), n + 1);
        assert_eq!(ext::read(|r| r.is_enabled(ext::CORE)), Some(true));
        let _ = std::fs::remove_dir_all(&d);
    }

    // ---- running ------------------------------------------------------------------------------

    fn run(args: &str) -> Run {
        Run { title: "T".into(), program: "tool.exe".into(), args: args.into() }
    }

    #[test]
    fn tokens_expand_like_a_custom_menu_entry() {
        let p = vec![r"C:\My Docs\a b.txt".to_string()];
        let a = argvs(&run("--p {path} --d {dir} --n {name} --e {ext} --par {parent}"), &p, r"C:\My Docs").unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].program, "tool.exe");
        assert_eq!(a[0].args, ["--p", r"C:\My Docs\a b.txt", "--d", r"C:\My Docs", "--n", "a b.txt", "--e", "txt", "--par", r"C:\My Docs"]);
        assert_eq!(a[0].dir, r"C:\My Docs");
        // nothing selected: the folder is the path
        let a = argvs(&run("{path} {selection}"), &[], r"D:\Proj").unwrap();
        assert_eq!(a[0].args, [r"D:\Proj", r"D:\Proj"]);
        // a name full of shell syntax stays one argument
        let nasty = vec![r#"C:\x\a&b "c" %PATH% ^d.txt"#.to_string()];
        assert_eq!(argvs(&run("{path}"), &nasty, r"C:\x").unwrap()[0].args, [nasty[0].clone()]);
    }

    #[test]
    fn several_items_run_once_each_or_once_with_the_whole_selection() {
        let p: Vec<String> = [r"C:\a.txt", r"C:\b.txt", r"C:\c.txt"].iter().map(|s| s.to_string()).collect();
        assert_eq!(argvs(&run("{path}"), &p, r"C:\").unwrap().len(), 3);
        let one = argvs(&run("--all {selection}"), &p, r"C:\").unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].args, ["--all", r"C:\a.txt", r"C:\b.txt", r"C:\c.txt"]);
    }

    #[test]
    fn open_with_builds_the_program_line_for_a_file() {
        let d = scratch("openwith");
        put(&d, "p.toml", "[[file_type]]\nid=\"r\"\nextensions=[\"rs\"]\nopen_with=\"\\\"C:\\\\Program Files\\\\Code\\\\code.exe\\\" --goto {path}\"\n");
        load(&d);
        let a = open_with(r"C:\src\main.RS").unwrap().unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].program, r"C:\Program Files\Code\code.exe");
        assert_eq!(a[0].args, ["--goto", r"C:\src\main.RS"]);
        assert_eq!(a[0].dir, r"C:\src");
        assert!(open_with(r"C:\src\main.py").is_none());
        assert!(open_with(r"C:\src\Makefile").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_first_rule_wins_an_extension() {
        let d = scratch("first");
        put(&d, "a-first.toml", "[[file_type]]\nid=\"r\"\nextensions=[\"rs\"]\ncolor=\"#111111\"\n");
        put(&d, "b-second.toml", "[[file_type]]\nid=\"r\"\nextensions=[\"rs\", \"py\"]\ncolor=\"#222222\"\n");
        load(&d);
        assert_eq!(name_color("x.rs", false), Some(0x111111));
        assert_eq!(name_color("x.py", false), Some(0x222222));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn tiny_ico_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
        out.extend_from_slice(&[1, 1, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&48u32.to_le_bytes());
        out.extend_from_slice(&22u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&2i32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&[0u8; 24]);
        out.extend_from_slice(&[0x10, 0x80, 0xf0, 0xff, 0, 0, 0, 0]);
        out
    }

    #[test]
    fn pack_icons_draw_glyphs_badges_and_image_files_and_respect_disable() {
        use tsubame_ui::testcanvas::Op;
        use tsubame_ui::{TestCanvas, Theme};
        let d = scratch("icons");
        let icons_dir = d.join(DIR_NAME).join("icons");
        std::fs::create_dir_all(&icons_dir).unwrap();
        std::fs::write(icons_dir.join("custom.ico"), tiny_ico_bytes()).unwrap();
        put(
            &d,
            "icons.toml",
            r##"
[[file_type]]
id = "glyph"
extensions = ["rs"]
color = "#268bd2"
icon = "code"

[[file_type]]
id = "codepoint"
extensions = ["toml"]
icon = "U+E713"

[[file_type]]
id = "badge"
extensions = ["zig"]
color = "#f7a41d"
icon = "badge:ZIG"

[[file_type]]
id = "plain"
extensions = ["py"]
icon = "rust"

[[file_type]]
id = "bmp"
extensions = ["foo"]
icon = "icons/custom.ico"

[[file_type]]
id = "missing"
extensions = ["bar"]
icon = "icons/nope.ico"

[[file_type]]
id = "escape"
extensions = ["baz"]
icon = "../outside.ico"
"##,
        );
        load(&d);
        let r = report();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.warnings.len(), 2, "missing and traversal icons warn: {:?}", r.warnings);

        let mut c = TestCanvas::new(200.0, 200.0);
        let t = Theme::dark();
        let rect = Rect::new(8.0, 4.0, 16.0, 16.0);

        // built-in glyph
        assert!(has_icon("main.rs", false));
        assert!(!has_icon("main.rs", true), "folders never get pack icons");
        assert!(draw_icon(&mut c, &t, "main.rs", false, false, rect));
        assert!(c.ops.iter().any(|o| matches!(o, Op::Text(s, _, _, col) if s == "\u{E943}" && *col == 0x268bd2)), "{:?}", c.ops);

        // explicit U+ codepoint
        c.clear();
        assert!(draw_icon(&mut c, &t, "Cargo.TOML", false, false, rect));
        assert!(c.ops.iter().any(|o| matches!(o, Op::Text(s, _, _, _) if s == "\u{E713}")), "{:?}", c.ops);

        // explicit badge
        c.clear();
        assert!(draw_icon(&mut c, &t, "a.zig", false, false, rect));
        assert!(c.ops.iter().any(|o| matches!(o, Op::Round(_, _, col, _) if *col == 0xf7a41d)));
        assert!(c.ops.iter().any(|o| matches!(o, Op::Text(s, _, _, col) if s == "ZIG" && *col == 0xf7a41d)));

        // plain name badge
        c.clear();
        assert!(draw_icon(&mut c, &t, "script.py", false, false, rect));
        assert!(c.ops.iter().any(|o| matches!(o, Op::Text(s, _, _, _) if s == "RU")));

        // decoded .ico bitmap
        c.clear();
        assert!(draw_icon(&mut c, &t, "test.foo", false, false, rect));
        assert_eq!(c.images().len(), 1);
        assert!(c.images()[0].starts_with("pack-icon:"));

        // missing or blocked file falls back to false (shell icon)
        assert!(!has_icon("x.bar", false));
        assert!(!draw_icon(&mut c, &t, "x.bar", false, false, rect));
        assert!(!has_icon("x.baz", false));

        // disabling the pack turns off its icons live
        ext::apply_disabled(&["pack.icons".to_string()]);
        assert!(!has_icon("main.rs", false));
        assert!(!has_icon("test.foo", false));
        ext::apply_disabled(&[]);
        assert!(has_icon("test.foo", false), "re-enabling brings the cached bitmap back");

        let _ = std::fs::remove_dir_all(&d);
        load(&d);
    }

    #[test]
    fn pack_icon_cache_is_bounded_and_evicts() {
        let mut cache: Lru<Arc<str>, Arc<PackBitmap>> = Lru::new(PACK_ICON_CAP_ITEMS, PACK_ICON_CAP_BYTES);
        for i in 0..(PACK_ICON_CAP_ITEMS + 10) {
            let key: Arc<str> = Arc::from(format!("icon-{i}.ico"));
            let bmp = Arc::new(PackBitmap { key: key.clone(), w: 16, h: 16, px: vec![0xffff_ffff; 256] });
            let b = bmp.bytes();
            cache.insert(key, bmp, b);
        }
        assert!(cache.len() <= PACK_ICON_CAP_ITEMS);
        assert!(cache.bytes() <= PACK_ICON_CAP_BYTES);
        assert!(cache.evictions >= 10, "oldest entries evicted past cap");
    }
}

#[cfg(test)]
mod example_tests {
    use super::*;

    /// The packs in `examples/packs` are what the docs tell people to copy: they must load clean.
    #[test]
    fn the_shipped_example_packs_load_without_errors_or_warnings() {
        let d = std::env::temp_dir().join(format!("tsubame-x2-examples-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(DIR_NAME)).unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join("packs");
        let mut n = 0;
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            std::fs::copy(e.path(), d.join(DIR_NAME).join(e.file_name())).unwrap();
            n += 1;
        }
        assert_eq!(n, 2);
        load(&d);
        let r = report();
        assert!(r.errors.is_empty() && r.warnings.is_empty(), "{:?} {:?}", r.errors, r.warnings);
        assert_eq!(r.packs.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["pack.open-in-terminal", "pack.solarized-lite"]);
        assert!(sync());
        let t = themes::find("pack.solarized-lite.lite").expect("the theme is in the picker");
        assert_eq!((t.name, t.dark, t.accent), ("Solarized Lite", false, 0xcb4b16));
        assert_eq!(name_color("lib.rs", false), Some(0x268bd2));
        assert_eq!(name_color("Cargo.toml", false), Some(0xb58900));
        let km = crate::keymap::KeyMap::defaults();
        let id = ext::read(|r| r.command_named("pack.open-in-terminal.terminal").unwrap().spec.id);
        assert_eq!(km.label(id), "Ctrl+Alt+Y");
        let m = menu_entries();
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].command.as_str(), m[0].args.as_str()), ("wt.exe", "-d {dir}"));
        let a = argvs(&Run { title: "t".into(), program: m[0].command.clone(), args: m[0].args.clone() }, &[], r"C:\My Docs").unwrap();
        assert_eq!(a[0].args, ["-d", r"C:\My Docs"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
