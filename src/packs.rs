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
use crate::view::View;
use std::borrow::Cow;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use tsubame_config::menu::{MenuEntry, ShowFor, Source};
use tsubame_config::packs::{self as cfg, Checks, Pack};
use tsubame_ext_api::{CmdFlags, CommandSpec, Contributions, Extension, ExtensionInfo, FileTypeRule, MenuItemSpec, MenuWhen, Text, ThemeSpec, Tier};
use tsubame_ui::{Ui, themes};

/// The folder inside the data directory.
pub const DIR_NAME: &str = "packs";
/// Packs read; more are reported and ignored.
pub const MAX_PACKS: usize = 32;

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

#[derive(Default)]
struct State {
    report: Report,
    /// command id -> what to run, sorted by id
    cmds: Vec<(u32, Run)>,
    /// registry menu item id -> the entry the shell menu gets
    menus: Vec<(String, MenuEntry)>,
    /// registry generation the theme list and the menu were last published for
    synced: Option<u64>,
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
    STATE.with(|s| *s.borrow_mut() = State::default());
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
                    Ok(pack) => register(&name, pack, &mut report),
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

fn register(file: &str, mut pack: Pack, report: &mut Report) {
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

#[derive(Default)]
struct FtCache {
    generation: Option<u64>,
    /// extension -> name colour, first rule wins
    colors: Vec<(String, u32)>,
    /// extension -> program and arguments
    opens: Vec<(String, String, String)>,
}

thread_local! {
    static FT: RefCell<FtCache> = RefCell::new(FtCache::default());
}

fn with_rules<R>(f: impl FnOnce(&FtCache) -> R) -> R {
    let g = ext::generation();
    FT.with(|c| {
        let mut c = c.borrow_mut();
        if c.generation != Some(g) {
            c.colors.clear();
            c.opens.clear();
            ext::read(|r| {
                for rule in r.file_types.iter() {
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
                    }
                }
            });
            c.generation = Some(g);
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

/// The programs to open file `path` with when a rule says so (None: use the shell's default).
pub fn open_with(path: &str) -> Option<Result<Vec<Argv>, String>> {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let ext = ext_of(name)?;
    let (prog, args) = with_rules(|c| c.opens.iter().find(|(x, _, _)| x.eq_ignore_ascii_case(ext)).map(|(_, p, a)| (p.clone(), a.clone())))?;
    let dir = path.rfind(['\\', '/']).map(|i| &path[..i]).unwrap_or("");
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
        assert_eq!(km.label(id), "Ctrl+Alt+T");
        let m = menu_entries();
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].command.as_str(), m[0].args.as_str()), ("wt.exe", "-d {dir}"));
        let a = argvs(&Run { title: "t".into(), program: m[0].command.clone(), args: m[0].args.clone() }, &[], r"C:\My Docs").unwrap();
        assert_eq!(a[0].args, ["-d", r"C:\My Docs"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
