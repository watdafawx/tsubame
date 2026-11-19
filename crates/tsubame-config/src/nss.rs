//! A reader for the common subset of Nilesoft Shell's `shell.nss`, producing
//! Tsubame menu entries ([`MenuEntry`]). Not a full interpreter: Nilesoft's
//! language has expressions, conditions and its own functions; anything
//! dynamic is skipped and reported with its line, so the user can add it by
//! hand. Supported subset: `docs/u4-settings.md`.
//!
//! Read: `item(...)`, `menu(...) { ... }` (nested), `separator` / `sep`
//! (also `sep=before|after|both` on an item); the properties `title`, `cmd`,
//! `args` (`cmd-line` too), `type` (`file`, `dir`/`directory`, `back`,
//! `*`), `find` (extension filter); the variables `@sel.path`, `@sel.dir`,
//! `@sel.file`, `@sel.paths`, `@sys.dir`, `@sys.bin`, `@user.home`,
//! `@user.desktop`. `shell { }`, `static { }`, `dynamic { }` blocks are looked
//! into. Skipped with a reason: `admin` items, dynamic expressions (`if(...)`,
//! `reg.*`, `$variables`, `@(...)`), `modify(...)`, `theme { }`,
//! `settings { }`, `import`.
//!
//! The source text is only read, never changed.

use crate::menu::{EntryKind, MenuEntry, ShowFor, Source};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// 1-based line of the statement
    pub line: usize,
    /// the statement's first source line, trimmed and shortened
    pub text: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Import {
    /// in menu order; a submenu is followed by its children (`parent` set)
    pub entries: Vec<MenuEntry>,
    /// items and submenus that were read (separators are not counted)
    pub imported: usize,
    pub skipped: Vec<Skipped>,
}

impl Import {
    /// What the user can choose between: each top-level item or submenu of the
    /// import (a submenu stands for itself and its children), in menu order.
    pub fn choices(&self) -> Vec<Choice> {
        self.entries
            .iter()
            .filter(|e| !e.is_separator() && self.root_of(e) == e.id)
            .map(|e| Choice {
                id: e.id.clone(),
                name: e.name.clone(),
                submenu: e.is_submenu(),
                children: if e.is_submenu() { self.entries.iter().filter(|c| c.parent == e.id && !c.is_separator()).count() } else { 0 },
            })
            .collect()
    }

    /// The id at the top of `e`'s parent chain inside this import (its own id
    /// when it sits outside any imported submenu).
    fn root_of(&self, e: &MenuEntry) -> String {
        let mut cur = e;
        while let Some(p) = self.entries.iter().find(|p| p.id == cur.parent) {
            cur = p;
        }
        cur.id.clone()
    }

    /// The import reduced to the choices in `keep` (ids from [`Import::choices`]).
    /// A kept submenu keeps its children; separators that end up first, last or
    /// doubled in their level are dropped; `imported` is recounted. The skipped
    /// lines stay as they are.
    pub fn keep_only(&self, keep: &[String]) -> Import {
        let mut tidy: Vec<MenuEntry> = Vec::new();
        for e in &self.entries {
            let root = self.root_of(e);
            // a top-level separator is decided by its neighbours below
            let take = if e.is_separator() && root == e.id { true } else { keep.contains(&root) };
            if !take {
                continue;
            }
            if e.is_separator() {
                let prev = tidy.iter().rev().find(|p| p.parent == e.parent);
                if prev.is_none_or(|p| p.is_separator()) {
                    continue;
                }
            }
            tidy.push(e.clone());
        }
        // a level never ends with a separator
        let levels: Vec<String> = tidy.iter().map(|e| e.parent.clone()).collect();
        for lv in levels {
            while let Some(i) = tidy.iter().rposition(|e| e.parent == lv) {
                if tidy[i].is_separator() {
                    tidy.remove(i);
                } else {
                    break;
                }
            }
        }
        // a kept submenu whose children all went is not worth showing (cannot
        // happen through `choices`, but keep the list consistent)
        let imported = tidy.iter().filter(|e| !e.is_separator()).count();
        Import { entries: tidy, imported, skipped: self.skipped.clone() }
    }

    /// Statements the file gave: imported plus skipped.
    pub fn found(&self) -> usize {
        self.imported + self.skipped.len()
    }
}

/// One selectable unit of an import (see [`Import::choices`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub submenu: bool,
    pub children: usize,
}

/// A `shell.nss` found in a usual place (mock 6b, state 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub path: std::path::PathBuf,
    /// "the Program Files folder"
    pub place: String,
    pub found: usize,
    pub importable: usize,
    pub skipped: usize,
}

/// Where Nilesoft Shell keeps its `shell.nss`, most likely first.
pub fn usual_places() -> Vec<(std::path::PathBuf, String)> {
    let mut v: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (var, place) in [("ProgramFiles", "the Program Files folder"), ("ProgramW6432", "the Program Files folder"), ("APPDATA", "your AppData folder"), ("LOCALAPPDATA", "your AppData folder")] {
        if let Ok(p) = std::env::var(var) {
            let path = std::path::Path::new(&p).join("Nilesoft Shell").join("shell.nss");
            if !v.iter().any(|(q, _)| *q == path) {
                v.push((path, place.to_string()));
            }
        }
    }
    v
}

/// The first readable file among `places`, counted (the file is only read).
pub fn detect_in(places: &[(std::path::PathBuf, String)], existing: &[MenuEntry]) -> Option<Detected> {
    for (path, place) in places {
        let Ok(src) = read_file(path) else { continue };
        let imp = import(&src, existing, "");
        return Some(Detected { path: path.clone(), place: place.clone(), found: imp.found(), importable: imp.imported, skipped: imp.skipped.len() });
    }
    None
}

/// [`detect_in`] over [`usual_places`].
pub fn detect(existing: &[MenuEntry]) -> Option<Detected> {
    detect_in(&usual_places(), existing)
}

/// Reads a `shell.nss` as text, or says why not (max 512 KB).
pub fn read_file(path: &std::path::Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|_| "That file does not exist.".to_string())?;
    if !meta.is_file() {
        return Err("That is not a file.".into());
    }
    if meta.len() > 512 * 1024 {
        return Err("That file is too big for a menu definition (over 512 KB).".into());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read it: {e}"))?;
    Ok(String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&bytes)).into_owned())
}

/// What an import did, kept so the report can show it and Undo can reverse it
/// (mock 6b, state 3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub file: String,
    pub imported: usize,
    /// ids of every entry added (separators included)
    pub ids: Vec<String>,
    pub skipped: Vec<Skipped>,
}

impl Report {
    pub fn new(file: &str, imp: &Import) -> Report {
        Report { file: file.to_string(), imported: imp.imported, ids: imp.entries.iter().map(|e| e.id.clone()).collect(), skipped: imp.skipped.clone() }
    }

    /// Takes the imported entries back out of `list`.
    pub fn undo(&self, list: &mut Vec<MenuEntry>) {
        list.retain(|e| !self.ids.contains(&e.id));
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// a bare word: identifiers, numbers, `$var`, `@sel.path`, `*`, `-`
    Word(String),
    Str(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Eq,
    /// anything else (`,`, `;`, `[`, `]`, operators)
    Punct(char),
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    line: usize,
}

fn tokenize(src: &str) -> Vec<Token> {
    let cs: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0usize, 1usize);
    while i < cs.len() {
        let c = cs[i];
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '/' if cs.get(i + 1) == Some(&'/') => {
                while i < cs.len() && cs[i] != '\n' {
                    i += 1;
                }
            }
            '/' if cs.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < cs.len() && !(cs[i] == '*' && cs.get(i + 1) == Some(&'/')) {
                    if cs[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
            }
            '\'' | '"' => {
                let q = c;
                let start_line = line;
                let mut s = String::new();
                i += 1;
                while i < cs.len() && cs[i] != q {
                    if cs[i] == '\n' {
                        line += 1;
                    }
                    s.push(cs[i]);
                    i += 1;
                }
                i += 1;
                out.push(Token { tok: Tok::Str(s), line: start_line });
            }
            '(' => {
                out.push(Token { tok: Tok::LParen, line });
                i += 1;
            }
            ')' => {
                out.push(Token { tok: Tok::RParen, line });
                i += 1;
            }
            '{' => {
                out.push(Token { tok: Tok::LBrace, line });
                i += 1;
            }
            '}' => {
                out.push(Token { tok: Tok::RBrace, line });
                i += 1;
            }
            '=' => {
                out.push(Token { tok: Tok::Eq, line });
                i += 1;
            }
            c if c.is_alphanumeric() || matches!(c, '_' | '$' | '@' | '.' | '*' | '-' | '|' | '\\') => {
                let mut s = String::new();
                while i < cs.len() && (cs[i].is_alphanumeric() || matches!(cs[i], '_' | '$' | '@' | '.' | '*' | '-' | '|' | '\\')) {
                    s.push(cs[i]);
                    i += 1;
                }
                out.push(Token { tok: Tok::Word(s), line });
            }
            other => {
                out.push(Token { tok: Tok::Punct(other), line });
                i += 1;
            }
        }
    }
    out
}

/// One property value.
#[derive(Clone, Debug, PartialEq)]
enum Val {
    Str(String),
    /// a bare word with no call (`multiple`, `file`, `true`, `1`)
    Word(String),
    /// anything computed: `if(..)`, `@(..)`, `reg.exists(..)`, `$var`
    Dynamic(String),
}

#[derive(Default)]
struct Props {
    list: Vec<(String, Val)>,
}

impl Props {
    fn get(&self, name: &str) -> Option<&Val> {
        self.list.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)
    }
    fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
}

struct Parser<'a> {
    toks: Vec<Token>,
    lines: Vec<&'a str>,
    pos: usize,
    out: Import,
    next_id: usize,
    id_prefix: String,
    taken: Vec<String>,
}

/// Inherited from the enclosing `menu`: what it shows for and its parent id.
#[derive(Clone)]
struct Scope {
    parent: String,
    show: Option<ShowFor>,
}

/// Reads `src`. New entries get ids `nss.<n>` that `existing` does not use;
/// top-level ones are placed in the submenu `parent` (empty = top level).
pub fn import(src: &str, existing: &[MenuEntry], parent: &str) -> Import {
    let mut p = Parser {
        toks: tokenize(src),
        lines: src.lines().collect(),
        pos: 0,
        out: Import::default(),
        next_id: 1,
        id_prefix: "nss".to_string(),
        taken: existing.iter().map(|e| e.id.clone()).collect(),
    };
    let scope = Scope { parent: parent.to_string(), show: None };
    p.statements(&scope, false);
    p.out
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }
    fn line(&self) -> usize {
        self.toks.get(self.pos).map(|t| t.line).unwrap_or_else(|| self.toks.last().map(|t| t.line).unwrap_or(1))
    }
    fn new_id(&mut self) -> String {
        loop {
            let id = format!("{}.{}", self.id_prefix, self.next_id);
            self.next_id += 1;
            if !self.taken.contains(&id) {
                self.taken.push(id.clone());
                return id;
            }
        }
    }
    fn skip(&mut self, line: usize, reason: &str) {
        let text: String = self.lines.get(line.saturating_sub(1)).map(|l| l.trim().chars().take(100).collect()).unwrap_or_default();
        self.out.skipped.push(Skipped { line, text, reason: reason.to_string() });
    }

    /// Statements until `}` (consumed) or the end.
    fn statements(&mut self, scope: &Scope, in_block: bool) {
        while let Some(t) = self.peek().cloned() {
            match t {
                Tok::RBrace => {
                    self.pos += 1;
                    if in_block {
                        return;
                    }
                }
                Tok::Word(w) => {
                    let line = self.line();
                    self.pos += 1;
                    self.statement(&w, line, scope);
                }
                _ => self.pos += 1,
            }
        }
    }

    fn statement(&mut self, word: &str, line: usize, scope: &Scope) {
        let lw = word.to_ascii_lowercase();
        // `$name = value`: variables are not evaluated
        if word.starts_with('$') {
            if self.peek() == Some(&Tok::Eq) {
                self.pos += 1;
                self.skip_value();
            }
            return;
        }
        match lw.as_str() {
            "separator" | "sep" => {
                if self.peek() == Some(&Tok::LParen) {
                    let _ = self.props();
                }
                self.push_sep(scope);
            }
            "item" => {
                let props = self.props();
                self.item(&props, line, scope);
            }
            "menu" => {
                let props = self.props();
                self.menu(&props, line, scope);
            }
            "modify" => {
                let _ = self.props();
                self.skip_block();
                self.skip(line, "modify() changes Windows' own menu entries; Tsubame's menu does not use it");
            }
            "import" => {
                self.skip_value();
                self.skip(line, "import: only this one file is read");
            }
            "theme" | "settings" | "images" | "glyph" | "tip" | "set" => {
                if self.peek() == Some(&Tok::LParen) {
                    let _ = self.props();
                }
                self.skip_block();
                self.skip(line, &format!("{word}: Nilesoft's own look and settings are not imported"));
            }
            _ => {
                // `shell {`, `static {`, `dynamic {`, `context {` ...: look inside
                if self.peek() == Some(&Tok::LParen) {
                    let _ = self.props();
                }
                if self.peek() == Some(&Tok::LBrace) {
                    self.pos += 1;
                    self.statements(scope, true);
                }
            }
        }
    }

    fn push_sep(&mut self, scope: &Scope) {
        // two separators in a row, or one first in a menu, mean nothing
        let last_is_sep = self.out.entries.last().is_none_or(|e| e.is_separator() || (e.is_submenu() && e.id == scope.parent));
        if last_is_sep {
            return;
        }
        let id = self.new_id();
        self.out.entries.push(MenuEntry { id, kind: EntryKind::Separator, source: Source::Nilesoft, parent: scope.parent.clone(), ..Default::default() });
    }

    /// `( name=value flag ... )`; the opening paren is the next token.
    fn props(&mut self) -> Props {
        let mut props = Props::default();
        if self.peek() != Some(&Tok::LParen) {
            return props;
        }
        self.pos += 1;
        loop {
            match self.peek().cloned() {
                None => break,
                Some(Tok::RParen) => {
                    self.pos += 1;
                    break;
                }
                Some(Tok::Word(name)) => {
                    self.pos += 1;
                    if self.peek() == Some(&Tok::Eq) {
                        self.pos += 1;
                        let v = self.value();
                        props.list.push((name, v));
                    } else if self.peek() == Some(&Tok::LParen) {
                        // a call used as a bare statement inside the list
                        let text = self.balanced_text();
                        props.list.push((name.clone(), Val::Dynamic(text)));
                    } else {
                        props.list.push((name, Val::Word(String::new())));
                    }
                }
                Some(Tok::Str(s)) => {
                    // a lone string (`item('x')`): the title
                    self.pos += 1;
                    props.list.push(("title".to_string(), Val::Str(s)));
                }
                Some(_) => self.pos += 1,
            }
        }
        props
    }

    fn value(&mut self) -> Val {
        match self.peek().cloned() {
            Some(Tok::Str(s)) => {
                self.pos += 1;
                // 'a' + 'b' concatenation and the like make it dynamic
                if matches!(self.peek(), Some(Tok::Punct('+'))) {
                    while matches!(self.peek(), Some(Tok::Punct('+'))) {
                        self.pos += 1;
                        self.value();
                    }
                    return Val::Dynamic(s);
                }
                Val::Str(s)
            }
            Some(Tok::Word(w)) => {
                self.pos += 1;
                if self.peek() == Some(&Tok::LParen) {
                    let text = format!("{w}{}", self.balanced_text());
                    return Val::Dynamic(text);
                }
                if w.starts_with('$') || w == "@" {
                    return Val::Dynamic(w);
                }
                if w.starts_with('@') {
                    return Val::Dynamic(w);
                }
                Val::Word(w)
            }
            Some(Tok::LParen) => {
                let text = self.balanced_text();
                Val::Dynamic(text)
            }
            _ => Val::Word(String::new()),
        }
    }

    /// Consumes `( ... )` (nested) and returns its inner text, roughly.
    fn balanced_text(&mut self) -> String {
        let mut depth = 0;
        let mut text = String::from("(");
        while let Some(t) = self.toks.get(self.pos).cloned() {
            self.pos += 1;
            match t.tok {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth <= 0 {
                        text.push(')');
                        return text;
                    }
                }
                Tok::Word(w) => text.push_str(&w),
                Tok::Str(s) => text.push_str(&format!("'{s}'")),
                _ => {}
            }
        }
        text
    }

    /// A value after `=` that nobody uses: a string, a word, a call, a `{}`.
    fn skip_value(&mut self) {
        match self.peek() {
            Some(Tok::LBrace) => self.skip_block(),
            _ => {
                let _ = self.value();
            }
        }
    }

    /// Skips a `{ ... }` block if one follows.
    fn skip_block(&mut self) {
        if self.peek() != Some(&Tok::LBrace) {
            return;
        }
        let mut depth = 0;
        while let Some(t) = self.toks.get(self.pos) {
            self.pos += 1;
            match t.tok {
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    fn show_for(&self, props: &Props, scope: &Scope) -> ShowFor {
        let mut show = scope.show.unwrap_or(ShowFor { files: true, folders: true, background: false });
        if let Some(Val::Str(t) | Val::Word(t)) = props.get("type") {
            let mut s = ShowFor { files: false, folders: false, background: false };
            for part in t.split('|') {
                match part.trim().trim_start_matches('!').to_ascii_lowercase().as_str() {
                    "file" => s.files = true,
                    "dir" | "directory" | "folder" => s.folders = true,
                    "back" | "background" | "backdir" => s.background = true,
                    "*" | "any" => {
                        s.files = true;
                        s.folders = true;
                    }
                    // drive, desktop, namespace, recyclebin, ...: not folders of ours
                    _ => {}
                }
            }
            if s.files || s.folders || s.background {
                show = s;
            }
        }
        show
    }

    /// Extension filter from `find='.png|.jpg'` or `find='*.png'`.
    fn extensions(props: &Props) -> Vec<String> {
        let Some(Val::Str(f) | Val::Word(f)) = props.get("find") else { return Vec::new() };
        f.split('|')
            .map(|p| p.trim().trim_start_matches('*').trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty() && !e.contains(['*', '?', '\\', '/']))
            .collect()
    }

    fn item(&mut self, props: &Props, line: usize, scope: &Scope) {
        if props.has("admin") {
            return self.skip(line, "needs administrator rights. Tsubame never runs menu items elevated.");
        }
        let title = match props.get("title") {
            Some(Val::Str(s)) => clean_title(s),
            Some(Val::Dynamic(_)) => return self.skip(line, "uses a dynamic expression (if, reg, $variable). Tsubame entries are fixed text; add it as a custom command and use \u{201C}Show for\u{201D} instead."),
            _ => String::new(),
        };
        if title.is_empty() {
            return self.skip(line, "has no title");
        }
        let cmd = match props.get("cmd").or_else(|| props.get("cmd-line")) {
            Some(Val::Str(s)) => s.clone(),
            Some(Val::Word(w)) if !w.is_empty() => return self.skip(line, &format!("cmd={w} is a Nilesoft built-in command")),
            Some(Val::Dynamic(_)) => return self.skip(line, "uses a dynamic expression in cmd. Tsubame entries are fixed text."),
            _ => return self.skip(line, "has no cmd"),
        };
        let args = match props.get("args") {
            Some(Val::Str(s)) => s.clone(),
            Some(Val::Dynamic(_)) => return self.skip(line, "uses a dynamic expression in args. Tsubame entries are fixed text."),
            _ => String::new(),
        };
        let (cmd, extra) = split_command(&map_vars(&cmd));
        let mut args_s = map_vars(&args);
        if !extra.is_empty() {
            args_s = if args_s.is_empty() { extra } else { format!("{extra} {args_s}") };
        }
        if cmd.is_empty() {
            return self.skip(line, "has an empty cmd");
        }
        if cmd.contains('@') || args_s.contains('@') {
            return self.skip(line, "uses a Nilesoft variable Tsubame does not have");
        }
        let sep = match props.get("sep").or_else(|| props.get("separator")) {
            Some(Val::Word(w) | Val::Str(w)) => w.to_ascii_lowercase(),
            _ => String::new(),
        };
        if matches!(sep.as_str(), "before" | "top" | "both") {
            self.push_sep(scope);
        }
        let id = self.new_id();
        let show_for = self.show_for(props, scope);
        self.out.entries.push(MenuEntry {
            id,
            kind: EntryKind::Item,
            source: Source::Nilesoft,
            name: title,
            command: cmd,
            args: args_s,
            extensions: Self::extensions(props),
            show_for,
            hidden: false,
            parent: scope.parent.clone(),
        });
        self.out.imported += 1;
        if matches!(sep.as_str(), "after" | "bottom" | "both") {
            self.push_sep(scope);
        }
    }

    fn menu(&mut self, props: &Props, line: usize, scope: &Scope) {
        let title = match props.get("title") {
            Some(Val::Str(s)) => clean_title(s),
            Some(Val::Dynamic(_)) => {
                self.skip_block();
                return self.skip(line, "menu with a dynamic title (if, reg, $variable)");
            }
            _ => String::new(),
        };
        if props.has("admin") {
            self.skip_block();
            return self.skip(line, "needs administrator rights. Tsubame never runs menu items elevated.");
        }
        if title.is_empty() || self.peek() != Some(&Tok::LBrace) {
            // `menu(...)` with no body or no title: nothing to show
            self.skip_block();
            return self.skip(line, "menu without a title or body");
        }
        let id = self.new_id();
        let show = self.show_for(props, scope);
        let at = self.out.entries.len();
        self.out.entries.push(MenuEntry {
            id: id.clone(),
            kind: EntryKind::Submenu,
            source: Source::Nilesoft,
            name: title,
            show_for: show,
            parent: scope.parent.clone(),
            ..Default::default()
        });
        self.pos += 1; // `{`
        let inner = Scope { parent: id, show: Some(show) };
        self.statements(&inner, true);
        // an empty menu is not worth showing
        let children = self.out.entries[at + 1..].iter().filter(|e| !e.is_separator()).count();
        if children == 0 {
            self.out.entries.truncate(at);
            self.skip(line, "menu has no items Tsubame can run");
        } else {
            // trailing separators inside the submenu go
            while self.out.entries.last().is_some_and(|e| e.is_separator() && e.parent == inner.parent) {
                self.out.entries.pop();
            }
            self.out.imported += 1;
        }
    }
}

/// Removes the `&` accelerator marker (`E&xplore` -> `Explore`).
fn clean_title(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '&' {
            if it.peek() == Some(&'&') {
                out.push('&');
                it.next();
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Nilesoft variables inside a string to our tokens and environment variables.
fn map_vars(s: &str) -> String {
    // longest names first
    const MAP: &[(&str, &str)] = &[
        ("@sel.path.quote", "{path}"),
        ("@sel.path.raw", "{path}"),
        ("@sel.path.paths", "{selection}"),
        ("@sel.paths", "{selection}"),
        ("@sel.path", "{path}"),
        ("@sel.raw", "{path}"),
        ("@sel.directory", "{dir}"),
        ("@sel.dir", "{dir}"),
        ("@sel.parent", "{dir}"),
        ("@sel.file.name", "{name}"),
        ("@sel.file", "{name}"),
        ("@sys.bin", "%SystemRoot%\\System32"),
        ("@sys.dir", "%SystemRoot%"),
        ("@user.desktop", "%USERPROFILE%\\Desktop"),
        ("@user.home", "%USERPROFILE%"),
        ("@user.name", "%USERNAME%"),
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        if rest.starts_with('@') {
            for (from, to) in MAP {
                if rest.len() >= from.len() && rest[..from.len()].eq_ignore_ascii_case(from) {
                    // the name must end here (`@sel.pathX` is something else)
                    let after = rest[from.len()..].chars().next();
                    if after.is_none_or(|c| !(c.is_alphanumeric() || c == '_')) {
                        out.push_str(to);
                        rest = &rest[from.len()..];
                        continue 'outer;
                    }
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// `cmd` may carry its own arguments (`cmd.exe /k`): split off an unquoted
/// program path that has none of its own spaces. A quoted program is taken
/// whole; an existing-looking path with spaces (`C:\Program Files\x.exe`) stays
/// intact, because it ends with an extension.
fn split_command(cmd: &str) -> (String, String) {
    let c = cmd.trim();
    if let Some(rest) = c.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
    }
    // find the end of the program: the first ".exe"/".bat"/".cmd"/".com" followed by a space or the end
    let lower = c.to_ascii_lowercase();
    for ext in [".exe", ".bat", ".cmd", ".com"] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(ext) {
            let end = from + i + ext.len();
            if lower[end..].is_empty() || lower[end..].starts_with(' ') {
                return (c[..end].to_string(), c[end..].trim().to_string());
            }
            from = end;
        }
    }
    // no extension: the first word is the program
    match c.split_once(' ') {
        Some((a, b)) => (a.to_string(), b.trim().to_string()),
        None => (c.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Import {
        import(src, &[], "")
    }

    const SAMPLE: &str = r#"
// my menu
settings { priority = 1 }
theme { name = 'modern' }

shell
{
    menu(title='&Tools' type='file|dir' mode=multiple)
    {
        item(title='Open in VS Code' cmd='C:\Program Files\Microsoft VS Code\Code.exe' args='--reuse-window "@sel.path"')
        item(title="Git Bash here" cmd='C:\Program Files\Git\git-bash.exe' args='--cd="@sel.dir"' type='dir')
        separator
        item(title='Hash it' cmd='powershell.exe' args='-NoProfile -File C:\x\hash.ps1 @sel.paths' find='.zip|.7z|*.rar')
    }
    item(title='Take ownership' admin cmd='cmd.exe' args='/c takeown /f "@sel.path"')
    item(title=if(sel.count > 1, 'Many', 'One') cmd='x.exe')
    modify(find='Open with' vis=remove)
    item(title='Notepad here' type='back' sep=before cmd='notepad.exe' args='@sel.dir\new.txt')
}
"#;

    #[test]
    fn sample_snippet_imports_the_common_subset() {
        let r = parse(SAMPLE);
        let names: Vec<(&str, &str)> = r.entries.iter().map(|e| (e.name.as_str(), e.id.as_str())).collect();
        assert_eq!(r.imported, 5, "{names:?}"); // Tools + 3 items + Notepad
        let tools = &r.entries[0];
        assert_eq!((tools.kind, tools.name.as_str(), tools.parent.as_str()), (EntryKind::Submenu, "Tools", ""));
        assert_eq!(tools.source, Source::Nilesoft);
        assert!(tools.show_for.files && tools.show_for.folders && !tools.show_for.background);

        let code = &r.entries[1];
        assert_eq!(code.name, "Open in VS Code");
        assert_eq!(code.parent, tools.id);
        assert_eq!(code.command, r"C:\Program Files\Microsoft VS Code\Code.exe");
        assert_eq!(code.args, "--reuse-window \"{path}\"");

        let git = &r.entries[2];
        assert_eq!(git.args, "--cd=\"{dir}\"");
        assert!(!git.show_for.files && git.show_for.folders, "type='dir' narrows the menu's filter");

        assert!(r.entries[3].is_separator());
        let hash = &r.entries[4];
        assert_eq!(hash.command, "powershell.exe");
        assert_eq!(hash.args, "-NoProfile -File C:\\x\\hash.ps1 {selection}");
        assert_eq!(hash.extensions, ["zip", "7z", "rar"]);

        // Notepad: separator before, background only, at top level
        let sep = &r.entries[5];
        assert!(sep.is_separator() && sep.parent.is_empty());
        let np = &r.entries[6];
        assert_eq!((np.name.as_str(), np.parent.as_str()), ("Notepad here", ""));
        assert!(np.show_for.background && !np.show_for.files);
        assert_eq!(np.args, "{dir}\\new.txt");
    }

    #[test]
    fn skipped_statements_are_listed_with_their_line() {
        let r = parse(SAMPLE);
        let lines: Vec<usize> = r.skipped.iter().map(|s| s.line).collect();
        // settings, theme, admin item, dynamic item, modify
        assert_eq!(r.skipped.len(), 5, "{:?}", r.skipped);
        let admin = r.skipped.iter().find(|s| s.reason.contains("administrator")).unwrap();
        assert!(admin.text.contains("Take ownership"));
        assert_eq!(SAMPLE.lines().nth(admin.line - 1).unwrap().trim(), admin.text);
        let dynamic = r.skipped.iter().find(|s| s.reason.contains("dynamic")).unwrap();
        assert!(dynamic.text.starts_with("item(title=if("));
        assert!(r.skipped.iter().any(|s| s.reason.starts_with("modify()")));
        assert!(lines.windows(2).all(|w| w[0] <= w[1]), "in source order: {lines:?}");
    }

    #[test]
    fn plain_top_level_items_and_flat_separators() {
        let r = parse("sep item(title='A' cmd='a.exe') sep sep item(title='B' cmd='b.exe') sep");
        let kinds: Vec<EntryKind> = r.entries.iter().map(|e| e.kind).collect();
        // no separator first, none doubled; a trailing one is kept at top level (harmless)
        assert_eq!(kinds, [EntryKind::Item, EntryKind::Separator, EntryKind::Item, EntryKind::Separator]);
        assert_eq!(r.imported, 2);
    }

    #[test]
    fn ids_avoid_existing_and_parent_is_honoured() {
        let existing = vec![MenuEntry { id: "nss.1".into(), ..Default::default() }];
        let r = import("item(title='A' cmd='a.exe')", &existing, "tools");
        assert_eq!(r.entries[0].id, "nss.2");
        assert_eq!(r.entries[0].parent, "tools");
    }

    #[test]
    fn cmd_with_inline_arguments_is_split() {
        let r = parse("item(title='Cmd here' cmd='cmd.exe /k cd /d \"@sel.dir\"')");
        assert_eq!(r.entries[0].command, "cmd.exe");
        assert_eq!(r.entries[0].args, "/k cd /d \"{dir}\"");
        let r = parse(r"item(title='X' cmd='C:\Program Files\X\x.exe' args='-a')");
        assert_eq!(r.entries[0].command, r"C:\Program Files\X\x.exe");
        assert_eq!(r.entries[0].args, "-a");
        let r = parse(r#"item(title='Q' cmd='"C:\A B\q.exe" --flag')"#);
        assert_eq!(r.entries[0].command, r"C:\A B\q.exe");
        assert_eq!(r.entries[0].args, "--flag");
    }

    #[test]
    fn unknown_variables_and_empty_menus_are_skipped_and_garbage_is_harmless() {
        let r = parse("item(title='A' cmd='a.exe' args='@sel.count')");
        assert!(r.entries.is_empty());
        assert!(r.skipped[0].reason.contains("variable"));
        let r = parse("menu(title='Empty') { item(title='X' admin cmd='x') }");
        assert!(r.entries.is_empty());
        assert_eq!(r.skipped.len(), 2);
        // no panic on junk
        for junk in ["", "}}}{{{", "item(", "item(title=", "menu(title='a'", "$x = {", "'unterminated", "/* open", "item(title='a' cmd=)"] {
            let _ = parse(junk);
        }
    }

    #[test]
    fn choices_and_selection() {
        let r = parse(SAMPLE);
        let c = r.choices();
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Tools", "Notepad here"]);
        assert!(c[0].submenu && c[0].children == 3 && !c[1].submenu);
        // only Notepad: the separator before it would be first, so it goes
        let k = r.keep_only(&[c[1].id.clone()]);
        assert_eq!((k.imported, k.entries.len()), (1, 1), "{:?}", k.entries);
        assert_eq!(k.entries[0].name, "Notepad here");
        assert_eq!(k.skipped.len(), r.skipped.len());
        // only Tools: children come along, the inner separator stays
        let k = r.keep_only(&[c[0].id.clone()]);
        assert_eq!(k.imported, 4);
        assert!(k.entries.iter().any(|e| e.is_separator()));
        assert!(k.entries.iter().all(|e| e.name != "Notepad here"));
        // all = same count; none = nothing
        let all: Vec<String> = c.iter().map(|c| c.id.clone()).collect();
        assert_eq!(r.keep_only(&all).imported, r.imported);
        assert!(r.keep_only(&[]).entries.is_empty());
    }

    #[test]
    fn report_undo_removes_exactly_what_was_added() {
        let r = parse(SAMPLE);
        let report = Report::new("shell.nss", &r);
        assert_eq!((report.imported, report.skipped.len()), (5, 5));
        let mut list = vec![MenuEntry { id: "custom.1".into(), ..Default::default() }];
        list.extend(r.entries.clone());
        report.undo(&mut list);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "custom.1");
    }

    #[test]
    fn detect_counts_and_never_writes() {
        let dir = std::env::temp_dir().join(format!("tsubame-nss-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shell.nss");
        std::fs::write(&file, SAMPLE).unwrap();
        let before = std::fs::read(&file).unwrap();
        let d = detect_in(&[(dir.join("nope.nss"), "x".into()), (file.clone(), "the Program Files folder".into())], &[]).unwrap();
        assert_eq!((d.found, d.importable, d.skipped), (10, 5, 5));
        assert_eq!(d.path, file);
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert!(detect_in(&[(dir.join("none.nss"), "x".into())], &[]).is_none());
        // a BOM is stripped
        std::fs::write(&file, [&[0xEF, 0xBB, 0xBF][..], b"item(title='A' cmd='a.exe')"].concat()).unwrap();
        assert_eq!(import(&read_file(&file).unwrap(), &[], "").imported, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn variables_map_and_titles_lose_the_accelerator() {
        assert_eq!(map_vars("@sel.path @SEL.DIR @sel.pathx"), "{path} {dir} @sel.pathx");
        assert_eq!(map_vars("@sys.bin\\cmd.exe"), "%SystemRoot%\\System32\\cmd.exe");
        assert_eq!(clean_title("E&xplore && more"), "Explore & more");
    }
}
