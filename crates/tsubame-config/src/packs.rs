//! Tier 1 extension packs (`docs/extension-packs.md`): one TOML file per pack,
//! declarative only. This module is the parser and the checks; it knows nothing
//! about Win32 or the registry (the app turns a [`Pack`] into an extension).
//!
//! A pack can hold commands (run a program), context-menu entries, themes and
//! file-type rules. Keys are checked one by one and a mistake names the file
//! and the key (`open-in-terminal.toml: command[0].keys[1]: ...`). The first
//! mistake rejects the whole pack; nothing is half loaded and nothing panics.
//! Unknown keys are mistakes too (a typo must not be silent).
//!
//! Limits keep a hostile or broken file small: file size, item counts, text
//! lengths, no control characters.

use toml::{Table, Value};

/// Largest pack file the loader reads.
pub const MAX_FILE_BYTES: u64 = 128 * 1024;
pub const MAX_COMMANDS: usize = 64;
pub const MAX_MENUS: usize = 32;
pub const MAX_THEMES: usize = 8;
pub const MAX_FILE_TYPES: usize = 64;
pub const MAX_EXTENSIONS: usize = 64;
pub const MAX_KEYS: usize = 4;
pub const MAX_TEXT: usize = 512;
pub const MAX_ARGS: usize = 2048;
pub const MAX_NAME: usize = 40;

/// The tokens an `args` string may use (`src/custom_cmd.rs` replaces them).
pub const TOKENS: [&str; 6] = ["path", "selection", "dir", "name", "ext", "parent"];

/// A mistake in a pack: the file, the key and what is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackError {
    /// file name, e.g. `open-in-terminal.toml`
    pub file: String,
    /// `command[0].keys[1]`; empty when the whole file is meant
    pub key: String,
    pub msg: String,
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.key.is_empty() {
            write!(f, "{}: {}", self.file, self.msg)
        } else {
            write!(f, "{}: {}: {}", self.file, self.key, self.msg)
        }
    }
}

impl std::error::Error for PackError {}

/// Checks the parser can not do itself (they need the app): a command line
/// that splits, a key chord that parses and can be bound.
pub struct Checks<'a> {
    pub args: &'a dyn Fn(&str) -> Result<(), String>,
    pub chord: &'a dyn Fn(&str) -> Result<(), String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    /// the file name without `.toml`: lower-case letters, digits, `-`
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub commands: Vec<PackCommand>,
    pub menus: Vec<PackMenu>,
    pub themes: Vec<PackTheme>,
    pub file_types: Vec<PackFileType>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackCommand {
    pub id: String,
    pub title: String,
    pub program: String,
    pub args: String,
    /// default chords as text (`Ctrl+Alt+T`)
    pub keys: Vec<String>,
    /// does nothing on the Settings tab (default true)
    pub needs_folder: bool,
    /// the key also works while a text field has the focus (default false)
    pub global: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackMenu {
    pub id: String,
    pub label: String,
    /// the pack command it runs (local id), when it names one
    pub command: Option<String>,
    pub program: String,
    pub args: String,
    pub files: bool,
    pub folders: bool,
    pub background: bool,
    pub multiple: bool,
    pub drives: bool,
    /// lower-case extensions without the dot; empty = any type
    pub types: Vec<String>,
}

/// Colours are 0xRRGGBB; a missing one comes from the Tsubame dark or light
/// theme (by `dark`).
#[derive(Clone, Debug, PartialEq)]
pub struct PackTheme {
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub bg: Option<u32>,
    pub panel: Option<u32>,
    pub surface: Option<u32>,
    pub text: Option<u32>,
    pub muted: Option<u32>,
    pub accent: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackFileType {
    pub id: String,
    pub extensions: Vec<String>,
    pub color: Option<u32>,
    pub icon: Option<String>,
    /// program and its arguments (`{path}` ...)
    pub open_with: Option<(String, String)>,
}

// ---- reading helpers -----------------------------------------------------------------

struct R<'a> {
    file: &'a str,
}

impl R<'_> {
    fn err(&self, key: impl Into<String>, msg: impl Into<String>) -> PackError {
        PackError { file: self.file.to_string(), key: key.into(), msg: msg.into() }
    }

    /// Only these keys may appear in `t`.
    fn only(&self, t: &Table, at: &str, allowed: &[&str]) -> Result<(), PackError> {
        for k in t.keys() {
            if !allowed.contains(&k.as_str()) {
                return Err(self.err(join(at, k), format!("unknown key (allowed here: {})", allowed.join(", "))));
            }
        }
        Ok(())
    }

    fn text(&self, t: &Table, at: &str, k: &str, required: bool, max: usize) -> Result<Option<String>, PackError> {
        let Some(v) = t.get(k) else {
            return if required { Err(self.err(join(at, k), "is required")) } else { Ok(None) };
        };
        let Value::String(s) = v else { return Err(self.err(join(at, k), format!("expected text, found {}", v.type_str()))) };
        self.check_text(&join(at, k), s, max)?;
        if required && s.trim().is_empty() {
            return Err(self.err(join(at, k), "must not be empty"));
        }
        Ok(Some(s.clone()))
    }

    fn check_text(&self, key: &str, s: &str, max: usize) -> Result<(), PackError> {
        if s.chars().count() > max {
            return Err(self.err(key, format!("is longer than {max} characters")));
        }
        if s.chars().any(|c| c.is_control()) {
            return Err(self.err(key, "has a control character (line break, tab or NUL)"));
        }
        Ok(())
    }

    fn boolean(&self, t: &Table, at: &str, k: &str, default: bool) -> Result<bool, PackError> {
        match t.get(k) {
            None => Ok(default),
            Some(Value::Boolean(b)) => Ok(*b),
            Some(v) => Err(self.err(join(at, k), format!("expected true or false, found {}", v.type_str()))),
        }
    }

    fn list(&self, t: &Table, at: &str, k: &str, max: usize) -> Result<Vec<String>, PackError> {
        let Some(v) = t.get(k) else { return Ok(Vec::new()) };
        let Value::Array(a) = v else { return Err(self.err(join(at, k), format!("expected a list of text, found {}", v.type_str()))) };
        if a.len() > max {
            return Err(self.err(join(at, k), format!("has more than {max} entries")));
        }
        let mut out = Vec::with_capacity(a.len());
        for (i, x) in a.iter().enumerate() {
            let key = format!("{}[{i}]", join(at, k));
            let Value::String(s) = x else { return Err(self.err(key, format!("expected text, found {}", x.type_str()))) };
            self.check_text(&key, s, MAX_TEXT)?;
            out.push(s.clone());
        }
        Ok(out)
    }

    fn color(&self, t: &Table, at: &str, k: &str) -> Result<Option<u32>, PackError> {
        let Some(s) = self.text(t, at, k, false, 16)? else { return Ok(None) };
        parse_color(&s).map(Some).ok_or_else(|| self.err(join(at, k), format!("\"{s}\" is not a colour (write #rrggbb or #rgb)")))
    }

    /// A table array (`[[command]]`) as tables, with its limit.
    fn tables<'t>(&self, root: &'t Table, k: &str, max: usize) -> Result<Vec<&'t Table>, PackError> {
        let Some(v) = root.get(k) else { return Ok(Vec::new()) };
        let Value::Array(a) = v else { return Err(self.err(k, format!("expected [[{k}]] entries, found {}", v.type_str()))) };
        if a.len() > max {
            return Err(self.err(k, format!("has more than {max} entries")));
        }
        a.iter()
            .enumerate()
            .map(|(i, x)| match x {
                Value::Table(t) => Ok(t),
                other => Err(self.err(format!("{k}[{i}]"), format!("expected a table, found {}", other.type_str()))),
            })
            .collect()
    }

    fn id(&self, t: &Table, at: &str, seen: &mut Vec<String>) -> Result<String, PackError> {
        let id = self.text(t, at, "id", true, MAX_NAME)?.unwrap_or_default();
        if !valid_id(&id) {
            return Err(self.err(join(at, "id"), format!("\"{id}\" is not an id (lower-case letters, digits, '-' and '_', starting with a letter or digit)")));
        }
        if seen.contains(&id) {
            return Err(self.err(join(at, "id"), format!("\"{id}\" is used twice")));
        }
        seen.push(id.clone());
        Ok(id)
    }

    fn program(&self, t: &Table, at: &str) -> Result<String, PackError> {
        let p = self.text(t, at, "program", true, MAX_TEXT)?.unwrap_or_default();
        self.program_ok(&join(at, "program"), &p)?;
        Ok(p)
    }

    fn program_ok(&self, key: &str, p: &str) -> Result<(), PackError> {
        if p.trim().is_empty() {
            return Err(self.err(key, "has no program"));
        }
        Ok(())
    }

    fn args(&self, t: &Table, at: &str, checks: &Checks) -> Result<String, PackError> {
        let a = self.text(t, at, "args", false, MAX_ARGS)?.unwrap_or_default();
        self.args_ok(&join(at, "args"), &a, checks)?;
        Ok(a)
    }

    fn args_ok(&self, key: &str, a: &str, checks: &Checks) -> Result<(), PackError> {
        (checks.args)(a).map_err(|m| self.err(key, m))?;
        if let Some(tok) = unknown_token(a) {
            return Err(self.err(key, format!("unknown token {{{tok}}} (known: {})", TOKENS.map(|t| format!("{{{t}}}")).join(" "))));
        }
        Ok(())
    }
}

fn join(at: &str, k: &str) -> String {
    if at.is_empty() { k.to_string() } else { format!("{at}.{k}") }
}

/// `[a-z0-9][a-z0-9_-]*`.
pub fn valid_id(s: &str) -> bool {
    let mut it = s.chars();
    it.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) && it.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') && s.len() <= MAX_NAME
}

/// A pack's name (its file stem): `[a-z0-9][a-z0-9-]*`, no `_` so ids stay unambiguous.
pub fn valid_name(s: &str) -> bool {
    valid_id(s) && !s.contains('_')
}

/// `#rrggbb`, `#rgb` (or without the `#`) as 0xRRGGBB.
pub fn parse_color(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match h.len() {
        6 => u32::from_str_radix(h, 16).ok(),
        3 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            Some(((r * 17) << 16) | ((g * 17) << 8) | (b * 17))
        }
        _ => None,
    }
}

/// The first `{word}` in `s` that looks like a token but is not one.
fn unknown_token(s: &str) -> Option<String> {
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        let tail = &rest[i + 1..];
        if let Some(j) = tail.find('}') {
            let w = &tail[..j];
            if !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !TOKENS.contains(&w) {
                return Some(w.to_string());
            }
        }
        rest = tail;
    }
    None
}

/// Splits a command line into the program (the first word, `"..."` groups)
/// and the rest of the text, as written. `None` when it is empty or a quote is
/// not closed.
pub fn split_program(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if let Some(q) = line.strip_prefix('"') {
        let end = q.find('"')?;
        let (p, rest) = (&q[..end], q[end + 1..].trim());
        return (!p.is_empty()).then(|| (p.to_string(), rest.to_string()));
    }
    let (p, rest) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim()),
        None => (line, ""),
    };
    (!p.is_empty()).then(|| (p.to_string(), rest.to_string()))
}

// ---- the parser ------------------------------------------------------------------------

/// Parses pack `name` (the file stem) from `text`. `file` is the file name for
/// messages.
pub fn parse(file: &str, name: &str, text: &str, checks: &Checks) -> Result<Pack, PackError> {
    let r = R { file };
    if !valid_name(name) {
        return Err(r.err("", format!("the file name \"{name}\" must be lower-case letters, digits and '-' (it becomes the pack's id, pack.{name})")));
    }
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(r.err("", format!("is larger than {} KB", MAX_FILE_BYTES / 1024)));
    }
    let root: Table = toml::from_str(text).map_err(|e| {
        let m = e.message().trim().to_string();
        let at = e.span().map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1);
        r.err(at.map(|l| format!("line {l}")).unwrap_or_default(), format!("is not valid TOML: {m}"))
    })?;
    r.only(&root, "", &["pack", "command", "menu", "theme", "file_type"])?;

    let (mut title, mut version, mut description, mut author) = (name.to_string(), String::new(), String::new(), String::new());
    if let Some(v) = root.get("pack") {
        let Value::Table(t) = v else { return Err(r.err("pack", format!("expected a table, found {}", v.type_str()))) };
        r.only(t, "pack", &["title", "version", "description", "author"])?;
        if let Some(s) = r.text(t, "pack", "title", false, 80)?.filter(|s| !s.trim().is_empty()) {
            title = s;
        }
        version = r.text(t, "pack", "version", false, 40)?.unwrap_or_default();
        description = r.text(t, "pack", "description", false, MAX_TEXT)?.unwrap_or_default();
        author = r.text(t, "pack", "author", false, 80)?.unwrap_or_default();
    }

    // commands
    let mut commands = Vec::new();
    let mut ids = Vec::new();
    for (i, t) in r.tables(&root, "command", MAX_COMMANDS)?.into_iter().enumerate() {
        let at = format!("command[{i}]");
        r.only(t, &at, &["id", "title", "program", "args", "keys", "needs_folder", "global"])?;
        let id = r.id(t, &at, &mut ids)?;
        let title = r.text(t, &at, "title", true, 80)?.unwrap_or_default();
        let program = r.program(t, &at)?;
        let args = r.args(t, &at, checks)?;
        let keys = r.list(t, &at, "keys", MAX_KEYS)?;
        for (k, c) in keys.iter().enumerate() {
            (checks.chord)(c).map_err(|m| r.err(format!("{at}.keys[{k}]"), m))?;
        }
        let needs_folder = r.boolean(t, &at, "needs_folder", true)?;
        let global = r.boolean(t, &at, "global", false)?;
        commands.push(PackCommand { id, title, program, args, keys, needs_folder, global });
    }

    // context-menu entries
    let mut menus = Vec::new();
    let mut ids = Vec::new();
    for (i, t) in r.tables(&root, "menu", MAX_MENUS)?.into_iter().enumerate() {
        let at = format!("menu[{i}]");
        r.only(t, &at, &["id", "label", "command", "program", "args", "show", "types"])?;
        let id = r.id(t, &at, &mut ids)?;
        let label = r.text(t, &at, "label", true, 80)?.unwrap_or_default();
        let command = r.text(t, &at, "command", false, MAX_NAME)?;
        let (program, args, command) = match command {
            Some(c) => {
                if t.contains_key("program") || t.contains_key("args") {
                    return Err(r.err(join(&at, "command"), "use either command or program/args, not both"));
                }
                let Some(cmd) = commands.iter().find(|x| x.id == c) else {
                    return Err(r.err(join(&at, "command"), format!("this pack has no command \"{c}\"")));
                };
                (cmd.program.clone(), cmd.args.clone(), Some(c))
            }
            None => (r.program(t, &at)?, r.args(t, &at, checks)?, None),
        };
        let shows = r.list(t, &at, "show", 5)?;
        let (mut files, mut folders, mut background, mut multiple, mut drives) = (true, true, false, true, false);
        if t.contains_key("show") {
            (files, folders, background, multiple, drives) = (false, false, false, false, false);
            for (k, s) in shows.iter().enumerate() {
                match s.as_str() {
                    "files" => files = true,
                    "folders" => folders = true,
                    "background" => background = true,
                    "multiple" => multiple = true,
                    "drives" => drives = true,
                    other => {
                        return Err(r.err(format!("{at}.show[{k}]"), format!("\"{other}\" is not one of files, folders, background, multiple, drives")));
                    }
                }
            }
        }
        let types = extensions(&r, t, &at, "types")?;
        menus.push(PackMenu { id, label, command, program, args, files, folders, background, multiple, drives, types });
    }

    // themes
    let mut themes = Vec::new();
    let mut ids = Vec::new();
    for (i, t) in r.tables(&root, "theme", MAX_THEMES)?.into_iter().enumerate() {
        let at = format!("theme[{i}]");
        r.only(t, &at, &["id", "name", "dark", "bg", "panel", "surface", "text", "muted", "accent"])?;
        let id = r.id(t, &at, &mut ids)?;
        let name = r.text(t, &at, "name", true, 60)?.unwrap_or_default();
        let dark = match t.get("dark") {
            Some(Value::Boolean(b)) => *b,
            Some(v) => return Err(r.err(join(&at, "dark"), format!("expected true or false, found {}", v.type_str()))),
            None => return Err(r.err(join(&at, "dark"), "is required (true for a dark theme, false for a light one)")),
        };
        themes.push(PackTheme {
            id,
            name,
            dark,
            bg: r.color(t, &at, "bg")?,
            panel: r.color(t, &at, "panel")?,
            surface: r.color(t, &at, "surface")?,
            text: r.color(t, &at, "text")?,
            muted: r.color(t, &at, "muted")?,
            accent: r.color(t, &at, "accent")?,
        });
    }

    // file-type rules
    let mut file_types = Vec::new();
    let mut ids = Vec::new();
    for (i, t) in r.tables(&root, "file_type", MAX_FILE_TYPES)?.into_iter().enumerate() {
        let at = format!("file_type[{i}]");
        r.only(t, &at, &["id", "extensions", "color", "icon", "open_with"])?;
        let id = r.id(t, &at, &mut ids)?;
        let exts = extensions(&r, t, &at, "extensions")?;
        if exts.is_empty() {
            return Err(r.err(join(&at, "extensions"), "is required (a list such as [\"rs\", \"toml\"])"));
        }
        let color = r.color(t, &at, "color")?;
        let icon = r.text(t, &at, "icon", false, MAX_NAME)?.filter(|s| !s.trim().is_empty());
        let open_with = match r.text(t, &at, "open_with", false, MAX_TEXT)? {
            None => None,
            Some(line) => {
                let key = join(&at, "open_with");
                let Some((p, a)) = split_program(&line) else { return Err(r.err(key, "needs a program (e.g. \"code.exe {path}\"), with closed quotes")) };
                r.program_ok(&key, &p)?;
                r.args_ok(&key, &a, checks)?;
                Some((p, a))
            }
        };
        if color.is_none() && icon.is_none() && open_with.is_none() {
            return Err(r.err(at, "sets nothing: give it a color, an icon or open_with"));
        }
        file_types.push(PackFileType { id, extensions: exts, color, icon, open_with });
    }

    if commands.is_empty() && menus.is_empty() && themes.is_empty() && file_types.is_empty() {
        return Err(r.err("", "is empty: add a [[command]], [[menu]], [[theme]] or [[file_type]]"));
    }
    Ok(Pack { name: name.to_string(), title, version, description, author, commands, menus, themes, file_types })
}

/// A list of extensions: lower-cased, a leading `.` dropped, checked.
fn extensions(r: &R, t: &Table, at: &str, k: &str) -> Result<Vec<String>, PackError> {
    let list = r.list(t, at, k, MAX_EXTENSIONS)?;
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for (i, s) in list.iter().enumerate() {
        let e = s.trim().trim_start_matches('.').to_ascii_lowercase();
        if e.is_empty() || e.len() > 24 || e.chars().any(|c| c.is_whitespace() || matches!(c, '.' | '/' | '\\' | '*' | '?' | ':' | '"' | '<' | '>' | '|')) {
            return Err(r.err(format!("{}[{i}]", join(at, k)), format!("\"{s}\" is not a file extension (write rs, not *.rs)")));
        }
        if !out.contains(&e) {
            out.push(e);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_checks() -> Checks<'static> {
        Checks {
            args: &|a| if a.matches('"').count() % 2 == 1 { Err("a quote is not closed".into()) } else { Ok(()) },
            chord: &|c| if c.starts_with("Ctrl+") || c.starts_with("Alt+") { Ok(()) } else { Err(format!("cannot read \"{c}\"")) },
        }
    }

    fn p(text: &str) -> Result<Pack, PackError> {
        parse("my-pack.toml", "my-pack", text, &ok_checks())
    }

    fn bad(text: &str) -> String {
        p(text).unwrap_err().to_string()
    }

    const GOOD: &str = r##"
[pack]
title = "My pack"
version = "1.2"
description = "d"
author = "me"

[[command]]
id = "term"
title = "Open terminal here"
program = "wt.exe"
args = "-d \"{dir}\""
keys = ["Ctrl+Alt+T"]
global = true

[[menu]]
id = "term"
label = "Terminal here"
command = "term"
show = ["folders", "background"]

[[menu]]
id = "inline"
label = "Edit"
program = "code.exe"
args = "{path}"
types = [".RS", "toml", "rs"]

[[theme]]
id = "sol"
name = "Sol"
dark = false
bg = "#fdf6e3"
accent = "#28f"

[[file_type]]
id = "rust"
extensions = ["rs"]
color = "b58900"
icon = "rust"
open_with = "\"C:\\Program Files\\Code\\code.exe\" --reuse {path}"
"##;

    #[test]
    fn a_good_pack_parses() {
        let k = p(GOOD).unwrap();
        assert_eq!((k.name.as_str(), k.title.as_str(), k.version.as_str(), k.author.as_str()), ("my-pack", "My pack", "1.2", "me"));
        let c = &k.commands[0];
        assert_eq!((c.id.as_str(), c.program.as_str(), c.args.as_str()), ("term", "wt.exe", "-d \"{dir}\""));
        assert_eq!(c.keys, ["Ctrl+Alt+T"]);
        assert!(c.global && c.needs_folder);
        let m = &k.menus[0];
        assert_eq!(m.command.as_deref(), Some("term"));
        assert_eq!((m.program.as_str(), m.args.as_str()), ("wt.exe", "-d \"{dir}\""), "a menu entry takes its command's program");
        assert_eq!((m.files, m.folders, m.background, m.multiple, m.drives), (false, true, true, false, false));
        let m = &k.menus[1];
        assert_eq!((m.files, m.folders, m.background, m.multiple), (true, true, false, true), "default: files, folders, several");
        assert_eq!(m.types, ["rs", "toml"], "lower-cased, no dot, no duplicates");
        let t = &k.themes[0];
        assert_eq!((t.dark, t.bg, t.accent, t.text), (false, Some(0xfdf6e3), Some(0x2288ff), None));
        let f = &k.file_types[0];
        assert_eq!((f.color, f.icon.as_deref()), (Some(0xb58900), Some("rust")));
        assert_eq!(f.open_with, Some((r"C:\Program Files\Code\code.exe".to_string(), "--reuse {path}".to_string())));
    }

    #[test]
    fn the_title_defaults_to_the_name_and_every_part_is_optional() {
        let k = p("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x.exe\"\n").unwrap();
        assert_eq!(k.title, "my-pack");
        assert!(k.menus.is_empty() && k.themes.is_empty() && k.file_types.is_empty());
        assert!(k.commands[0].needs_folder && !k.commands[0].global && k.commands[0].keys.is_empty());
        assert!(p("[[theme]]\nid=\"t\"\nname=\"T\"\ndark=true\n").is_ok());
    }

    #[test]
    fn colours_parse() {
        assert_eq!(parse_color("#fff"), Some(0xffffff));
        assert_eq!(parse_color("#102030"), Some(0x102030));
        assert_eq!(parse_color("A0b0C0"), Some(0xa0b0c0));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#gg0000"), None);
        assert_eq!(parse_color(""), None);
    }

    #[test]
    fn program_and_rest_split_like_a_person_reads_them() {
        assert_eq!(split_program("code.exe {path}"), Some(("code.exe".into(), "{path}".into())));
        assert_eq!(split_program("  \"C:\\a b\\c.exe\"   -x  \"{path}\" "), Some((r"C:\a b\c.exe".into(), "-x  \"{path}\"".into())));
        assert_eq!(split_program("tool"), Some(("tool".into(), String::new())));
        assert_eq!(split_program(""), None);
        assert_eq!(split_program("\"open"), None);
        assert_eq!(split_program("\"\" x"), None);
    }

    // ---- every error path: the file and the key are named ----------------------------------

    #[test]
    fn a_file_name_that_is_no_id_is_refused() {
        for n in ["My Pack", "UPPER", "a_b", "-x", "", "a.b"] {
            let e = parse("f.toml", n, "[[theme]]\nid=\"t\"\nname=\"T\"\ndark=true\n", &ok_checks()).unwrap_err();
            assert!(e.msg.contains("file name"), "{n}: {e}");
        }
    }

    #[test]
    fn broken_toml_names_the_line() {
        let e = p("[[command]\nid = 1").unwrap_err();
        assert_eq!(e.file, "my-pack.toml");
        assert!(e.key.starts_with("line "), "{e}");
        assert!(e.msg.contains("not valid TOML"), "{e}");
        let e = p("[pack]\ntitle = \"a\"\n\nfoo = = 3").unwrap_err();
        assert_eq!(e.key, "line 4");
    }

    #[test]
    fn size_and_emptiness_are_checked() {
        assert!(bad(&"#".repeat(MAX_FILE_BYTES as usize + 1)).contains("larger than"));
        assert!(bad("").contains("is empty"));
        assert!(bad("[pack]\ntitle=\"x\"\n").contains("is empty"));
    }

    #[test]
    fn unknown_keys_are_named() {
        assert_eq!(bad("[extra]\na=1"), "my-pack.toml: extra: unknown key (allowed here: pack, command, menu, theme, file_type)");
        assert!(bad("[pack]\ncolour=1").contains("pack.colour: unknown key"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nprogam=\"y\"").contains("command[0].progam: unknown key"));
        assert!(bad("[[menu]]\nid=\"a\"\nlabel=\"A\"\nprogram=\"x\"\nshow_for=[]").contains("menu[0].show_for: unknown key"));
        assert!(bad("[[theme]]\nid=\"a\"\nname=\"A\"\ndark=true\nbackground=\"#000\"").contains("theme[0].background: unknown key"));
        assert!(bad("[[file_type]]\nid=\"a\"\nextensions=[\"a\"]\ncolor=\"#000\"\nopen=\"x\"").contains("file_type[0].open: unknown key"));
    }

    #[test]
    fn wrong_types_are_named() {
        assert!(bad("command = 3").contains("command: expected [[command]] entries, found integer"));
        assert!(bad("command = [3]").contains("command[0]: expected a table, found integer"));
        assert!(bad("pack = 3").contains("pack: expected a table"));
        assert!(bad("[pack]\ntitle = 3").contains("pack.title: expected text, found integer"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=3").contains("command[0].program: expected text, found integer"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nkeys=\"Ctrl+A\"").contains("command[0].keys: expected a list of text"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nkeys=[1]").contains("command[0].keys[0]: expected text, found integer"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nglobal=\"yes\"").contains("command[0].global: expected true or false, found string"));
        assert!(bad("[[theme]]\nid=\"a\"\nname=\"A\"\ndark=\"no\"").contains("theme[0].dark: expected true or false"));
    }

    #[test]
    fn required_keys_and_ids() {
        assert!(bad("[[command]]\ntitle=\"A\"\nprogram=\"x\"").contains("command[0].id: is required"));
        assert!(bad("[[command]]\nid=\"a\"\nprogram=\"x\"").contains("command[0].title: is required"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"").contains("command[0].program: is required"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"  \"").contains("command[0].program: must not be empty"));
        assert!(bad("[[command]]\nid=\"A b\"\ntitle=\"A\"\nprogram=\"x\"").contains("command[0].id: \"A b\" is not an id"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\n[[command]]\nid=\"a\"\ntitle=\"B\"\nprogram=\"y\"").contains("command[1].id: \"a\" is used twice"));
        assert!(bad("[[menu]]\nid=\"a\"\nprogram=\"x\"").contains("menu[0].label: is required"));
        assert!(bad("[[menu]]\nid=\"a\"\nlabel=\"A\"").contains("menu[0].program: is required"));
        assert!(bad("[[theme]]\nid=\"a\"\nname=\"A\"").contains("theme[0].dark: is required"));
        assert!(bad("[[theme]]\nid=\"a\"\ndark=true").contains("theme[0].name: is required"));
        assert!(bad("[[file_type]]\nid=\"a\"\ncolor=\"#000\"").contains("file_type[0].extensions: is required"));
    }

    #[test]
    fn text_is_limited_and_has_no_control_characters() {
        let long = "x".repeat(MAX_TEXT + 1);
        assert!(bad(&format!("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"{long}\"")).contains("command[0].program: is longer than"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\\nB\"\nprogram=\"x\"").contains("command[0].title: has a control character"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\\u0000y\"").contains("command[0].program: has a control character"));
        let long_args = "y".repeat(MAX_ARGS + 1);
        assert!(bad(&format!("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nargs=\"{long_args}\"")).contains("command[0].args: is longer than"));
    }

    #[test]
    fn counts_are_limited() {
        let many = "[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\n".repeat(MAX_COMMANDS + 1);
        assert!(bad(&many).contains("command: has more than 64 entries"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nkeys=[\"Ctrl+A\",\"Ctrl+B\",\"Ctrl+C\",\"Ctrl+D\",\"Ctrl+E\"]").contains("command[0].keys: has more than 4 entries"));
        let exts: Vec<String> = (0..MAX_EXTENSIONS + 1).map(|i| format!("\"e{i}\"")).collect();
        assert!(bad(&format!("[[file_type]]\nid=\"a\"\ncolor=\"#000\"\nextensions=[{}]", exts.join(","))).contains("file_type[0].extensions: has more than"));
    }

    #[test]
    fn the_apps_checks_run_on_args_and_keys() {
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nargs=\"\\\"open\"").contains("command[0].args: a quote is not closed"));
        assert!(bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nkeys=[\"Ctrl+A\", \"Hyper+Q\"]").contains("command[0].keys[1]: cannot read \"Hyper+Q\""));
        assert!(bad("[[menu]]\nid=\"a\"\nlabel=\"A\"\nprogram=\"x\"\nargs=\"\\\"o\"").contains("menu[0].args: a quote is not closed"));
        assert!(bad("[[file_type]]\nid=\"a\"\nextensions=[\"a\"]\nopen_with=\"x \\\"o\"").contains("file_type[0].open_with"));
    }

    #[test]
    fn a_misspelled_token_is_caught_but_json_braces_are_fine() {
        let e = bad("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nargs=\"--f {pth}\"");
        assert!(e.contains("command[0].args: unknown token {pth}"), "{e}");
        for tok in TOKENS {
            assert!(p(&format!("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nargs=\"--f {{{tok}}}\"")).is_ok(), "{tok}");
        }
        assert!(p("[[command]]\nid=\"a\"\ntitle=\"A\"\nprogram=\"x\"\nargs=\"--j '{\\\"k\\\": 1}' {}\"").is_ok());
        assert!(bad("[[file_type]]\nid=\"a\"\nextensions=[\"a\"]\nopen_with=\"x {oops}\"").contains("file_type[0].open_with: unknown token {oops}"));
    }

    #[test]
    fn menu_entries_resolve_their_command() {
        assert!(bad("[[menu]]\nid=\"a\"\nlabel=\"A\"\ncommand=\"nope\"").contains("menu[0].command: this pack has no command \"nope\""));
        assert!(bad("[[command]]\nid=\"c\"\ntitle=\"C\"\nprogram=\"x\"\n[[menu]]\nid=\"a\"\nlabel=\"A\"\ncommand=\"c\"\nprogram=\"y\"").contains("menu[0].command: use either command or program/args"));
        assert!(bad("[[menu]]\nid=\"a\"\nlabel=\"A\"\nprogram=\"x\"\nshow=[\"files\",\"sky\"]").contains("menu[0].show[1]: \"sky\" is not one of"));
        assert!(bad("[[menu]]\nid=\"a\"\nlabel=\"A\"\nprogram=\"x\"\ntypes=[\"*.rs\"]").contains("menu[0].types[0]: \"*.rs\" is not a file extension"));
        assert!(bad("[[menu]]\nid=\"a\"\nlabel=\"A\"\nprogram=\"x\"\ntypes=[\"a/b\"]").contains("menu[0].types[0]"));
        // an empty `show` shows nowhere (the author asked for it)
        let k = p("[[menu]]\nid=\"a\"\nlabel=\"A\"\nprogram=\"x\"\nshow=[]").unwrap();
        assert!(!k.menus[0].files && !k.menus[0].folders && !k.menus[0].background);
    }

    #[test]
    fn colours_and_file_types_are_checked() {
        assert!(bad("[[theme]]\nid=\"a\"\nname=\"A\"\ndark=true\nbg=\"red\"").contains("theme[0].bg: \"red\" is not a colour"));
        assert!(bad("[[theme]]\nid=\"a\"\nname=\"A\"\ndark=true\naccent=5").contains("theme[0].accent: expected text, found integer"));
        assert!(bad("[[file_type]]\nid=\"a\"\nextensions=[\"rs\"]\ncolor=\"#12\"").contains("file_type[0].color: \"#12\" is not a colour"));
        assert!(bad("[[file_type]]\nid=\"a\"\nextensions=[\"rs\"]").contains("file_type[0]: sets nothing"));
        assert!(bad("[[file_type]]\nid=\"a\"\nextensions=[\"rs\"]\nopen_with=\"\"").contains("file_type[0].open_with: needs a program"));
        assert!(bad("[[file_type]]\nid=\"a\"\nextensions=[]\ncolor=\"#000\"").contains("file_type[0].extensions: is required"));
        assert!(bad("[[file_type]]\nid=\"a\"\nextensions=[\"..\"]\ncolor=\"#000\"").contains("file_type[0].extensions[0]"));
    }

    #[test]
    fn extensions_are_normalised() {
        let k = p("[[file_type]]\nid=\"a\"\nextensions=[\".RS\",\"Rs\",\"md\"]\ncolor=\"#000\"").unwrap();
        assert_eq!(k.file_types[0].extensions, ["rs", "md"]);
    }

    #[test]
    fn the_error_text_reads_well() {
        let e = PackError { file: "a.toml".into(), key: "command[0].keys[1]".into(), msg: "cannot read \"x\"".into() };
        assert_eq!(e.to_string(), "a.toml: command[0].keys[1]: cannot read \"x\"");
        let e = PackError { file: "a.toml".into(), key: String::new(), msg: "is empty".into() };
        assert_eq!(e.to_string(), "a.toml: is empty");
    }
}
