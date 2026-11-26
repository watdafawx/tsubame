//! Undo/redo journal and its plain-text serialisation (no serde).
//!
//! Format, one record per line, UTF-8, `\n` separated, fields separated by TAB:
//!
//! ```text
//! tsubame-journal<TAB>1
//! U<TAB>copy<TAB>C:\a<TAB>C:\b<TAB>delete<TAB>C:\b<TAB>0<TAB>
//! U<TAB>delete<TAB>C:\x<TAB>1<TAB>-<TAB>permanent delete cannot be undone
//! R<TAB>seq<TAB>2<TAB>delete<TAB>C:\t<TAB>0<TAB>move<TAB>C:\s<TAB>C:\t<TAB>-<TAB>reason
//! ```
//!
//! `U` records are the undo stack (oldest first), `R` the redo stack (next redo last).
//! After the type come the forward operation, the inverse operation (`-` if none) and
//! the reason text (empty when the entry is undoable). Operations are `kind` plus a fixed
//! number of fields; `seq` is followed by a count and that many operations.
//!
//! Escaping inside every path and text field: `%`, control characters below U+0020 and
//! U+007F become `%XX` (two hex digits), C1 controls and lone UTF-16 surrogates become
//! `%uXXXX`. Everything else (Unicode, spaces) is written as is. Windows paths are
//! UTF-16, so the encoding is lossless even for unpaired surrogates.
//!
//! Loading never panics: a bad line is skipped and counted, an unknown or missing header
//! skips the whole file.
use super::{Operation, RecycledItem};
use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
};

pub const HEADER: &str = "tsubame-journal";
pub const VERSION: u32 = 1;
const MAX_DEPTH: usize = 4;
pub const DEFAULT_CAP: usize = 200;

/// One recorded operation. Equality ignores `group` (it is not saved).
#[derive(Clone, Debug)]
pub struct Entry {
    pub forward: Operation,
    /// What undo runs. `None`: not undoable, `reason` says why.
    pub inverse: Option<Operation>,
    pub reason: Option<String>,
    /// The submission the entry came from (the queue's job id). `undo_group` and
    /// `redo_group` treat every entry of one group as one step. Not saved: a loaded
    /// entry is a group of its own.
    pub group: u64,
}

impl PartialEq for Entry {
    fn eq(&self, o: &Self) -> bool {
        self.forward == o.forward && self.inverse == o.inverse && self.reason == o.reason
    }
}

impl Entry {
    pub fn is_undoable(&self) -> bool {
        self.inverse.is_some()
    }
}

/// What `load_from_str` found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadReport {
    pub loaded: usize,
    /// Lines that were unknown, corrupt, or belonged to an unsupported file version.
    pub skipped: usize,
}

#[derive(Clone, Debug)]
pub struct Journal {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    cap: usize,
}

impl Default for Journal {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAP)
    }
}

impl Journal {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            cap: cap.max(1),
        }
    }
    pub fn undo_entries(&self) -> &[Entry] {
        &self.undo
    }
    pub fn redo_entries(&self) -> &[Entry] {
        &self.redo
    }
    /// Record new work: clears redo, drops the oldest entries over the cap.
    pub fn record(&mut self, entry: Entry) {
        self.redo.clear();
        self.undo.push(entry);
        // Over the cap: drop history that cannot be undone first, then the oldest.
        while self.undo.len() > self.cap {
            let pos = self.undo.iter().position(|e| !e.is_undoable()).unwrap_or(0);
            self.undo.remove(pos);
        }
    }
    /// Index and inverse of the newest undoable entry. Non-undoable entries stay in the
    /// journal as history and are skipped.
    pub fn next_undo(&self) -> Option<(usize, Operation)> {
        self.undo
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, e)| e.inverse.clone().map(|inv| (i, inv)))
    }
    pub fn next_redo(&self) -> Option<Operation> {
        self.redo.last().map(|e| e.forward.clone())
    }
    /// The newest group that has something to undo: (index, inverse) of each of its
    /// undoable entries, newest first. Undoing them in this order reverts the whole
    /// submission. Indices stay valid while the entries are marked undone in this order.
    pub fn undo_group(&self) -> Vec<(usize, Operation)> {
        let Some(start) = self.undo.iter().rposition(Entry::is_undoable) else {
            return Vec::new();
        };
        let g = self.undo[start].group;
        self.undo[..=start]
            .iter()
            .enumerate()
            .rev()
            .take_while(|(_, e)| e.group == g)
            .filter_map(|(i, e)| e.inverse.clone().map(|inv| (i, inv)))
            .collect()
    }
    /// The group `undo_group` would revert now (None: nothing to undo).
    pub fn newest_undo_group(&self) -> Option<u64> {
        self.undo.iter().rposition(Entry::is_undoable).map(|i| self.undo[i].group)
    }
    /// The group on top of the redo stack: (index, forward) of each entry, oldest first
    /// (the order they first ran in).
    pub fn redo_group(&self) -> Vec<(usize, Operation)> {
        let Some(g) = self.redo.last().map(|e| e.group) else {
            return Vec::new();
        };
        self.redo
            .iter()
            .enumerate()
            .rev()
            .take_while(|(_, e)| e.group == g)
            .map(|(i, e)| (i, e.forward.clone()))
            .collect()
    }
    /// Redo of the entry at `index` of the redo stack succeeded (see `mark_redone`).
    pub fn mark_redone_at(&mut self, index: usize, inverse: Option<Operation>, reason: Option<String>) {
        if index < self.redo.len() {
            let mut entry = self.redo.remove(index);
            entry.inverse = inverse;
            entry.reason = reason;
            self.undo.push(entry);
            if self.undo.len() > self.cap {
                self.undo.remove(0);
            }
        }
    }
    /// Undo at `index` succeeded: the entry moves to the redo stack.
    pub fn mark_undone(&mut self, index: usize) {
        if index < self.undo.len() {
            let entry = self.undo.remove(index);
            self.redo.push(entry);
        }
    }
    /// Redo succeeded: the entry returns to the undo stack with a fresh inverse (recycle
    /// tokens change every time an item is recycled).
    pub fn mark_redone(&mut self, inverse: Option<Operation>, reason: Option<String>) {
        if let Some(mut entry) = self.redo.pop() {
            entry.inverse = inverse;
            entry.reason = reason;
            self.undo.push(entry);
            if self.undo.len() > self.cap {
                self.undo.remove(0);
            }
        }
    }

    pub fn save_to_string(&self) -> String {
        let mut out = format!("{HEADER}\t{VERSION}\n");
        for (tag, list) in [("U", &self.undo), ("R", &self.redo)] {
            for e in list {
                out.push_str(tag);
                out.push('\t');
                write_op(&e.forward, &mut out);
                out.push('\t');
                match &e.inverse {
                    Some(op) => write_op(op, &mut out),
                    None => out.push('-'),
                }
                out.push('\t');
                esc(OsStr::new(e.reason.as_deref().unwrap_or("")), &mut out);
                out.push('\n');
            }
        }
        out
    }

    /// Parse a saved journal. Bad lines are skipped and counted; never panics.
    pub fn load_from_str(text: &str) -> (Journal, LoadReport) {
        let mut journal = Journal::default();
        let mut report = LoadReport::default();
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header_ok = lines.next().is_some_and(|h| {
            let mut f = h.split('\t');
            f.next() == Some(HEADER)
                && f.next().and_then(|v| v.parse::<u32>().ok()) == Some(VERSION)
                && f.next().is_none()
        });
        if !header_ok {
            // Unknown or newer format: nothing is trusted. Count the rest as skipped.
            report.skipped = lines.count() + usize::from(!text.trim().is_empty());
            return (journal, report);
        }
        for line in lines {
            match parse_line(line) {
                Some((stack, entry)) => {
                    if stack == 'U' {
                        journal.undo.push(entry);
                    } else {
                        journal.redo.push(entry);
                    }
                    report.loaded += 1;
                }
                None => report.skipped += 1,
            }
        }
        // every loaded entry is its own group (groups are not saved)
        let mut g = u64::MAX;
        for e in journal.undo.iter_mut().chain(journal.redo.iter_mut()) {
            e.group = g;
            g -= 1;
        }
        for list in [&mut journal.undo, &mut journal.redo] {
            if list.len() > journal.cap {
                let extra = list.len() - journal.cap;
                list.drain(..extra);
            }
        }
        (journal, report)
    }
}

fn esc(s: &OsStr, out: &mut String) {
    use std::fmt::Write;
    for r in char::decode_utf16(s.encode_wide()) {
        match r {
            Ok(c) if c == '%' || (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "%{:02X}", c as u32);
            }
            Ok(c) if c.is_control() => {
                let _ = write!(out, "%u{:04X}", c as u32);
            }
            Ok(c) => out.push(c),
            Err(e) => {
                let _ = write!(out, "%u{:04X}", e.unpaired_surrogate());
            }
        }
    }
}

fn unesc(s: &str) -> Option<OsString> {
    let mut wide: Vec<u16> = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            let mut b = [0u16; 2];
            wide.extend_from_slice(c.encode_utf16(&mut b));
            continue;
        }
        let rest = chars.as_str();
        if let Some(r) = rest.strip_prefix('u') {
            let hex = r.get(..4)?;
            wide.push(u16::from_str_radix(hex, 16).ok()?);
            chars = r[4..].chars();
        } else {
            let hex = rest.get(..2)?;
            let v = u8::from_str_radix(hex, 16).ok()?;
            if v >= 0x80 {
                return None;
            }
            wide.push(u16::from(v));
            chars = rest[2..].chars();
        }
    }
    Some(OsString::from_wide(&wide))
}

fn write_path(p: &std::path::Path, out: &mut String) {
    esc(p.as_os_str(), out);
}

fn write_op(op: &Operation, out: &mut String) {
    let two = |kind: &str, a: &std::path::Path, b: &std::path::Path, out: &mut String| {
        out.push_str(kind);
        out.push('\t');
        write_path(a, out);
        out.push('\t');
        write_path(b, out);
    };
    match op {
        Operation::Copy { source, target } => two("copy", source, target, out),
        Operation::Move { source, target } => two("move", source, target, out),
        Operation::Rename { source, target } => two("rename", source, target, out),
        Operation::Delete { path, permanent } => {
            out.push_str("delete\t");
            write_path(path, out);
            out.push_str(if *permanent { "\t1" } else { "\t0" });
        }
        Operation::NewFolder(p) => {
            out.push_str("newfolder\t");
            write_path(p, out);
        }
        Operation::NewFile(p) => {
            out.push_str("newfile\t");
            write_path(p, out);
        }
        Operation::Restore(item) => two("restore", &item.original, &item.recycled, out),
        Operation::Sequence(ops) => {
            out.push_str(&format!("seq\t{}", ops.len()));
            for o in ops {
                out.push('\t');
                write_op(o, out);
            }
        }
    }
}

type Fields<'a> = std::iter::Peekable<std::str::Split<'a, char>>;

fn path_field(it: &mut Fields) -> Option<PathBuf> {
    unesc(it.next()?).map(PathBuf::from)
}

fn parse_op(it: &mut Fields, depth: usize) -> Option<Operation> {
    if depth > MAX_DEPTH {
        return None;
    }
    Some(match it.next()? {
        "copy" => Operation::Copy {
            source: path_field(it)?,
            target: path_field(it)?,
        },
        "move" => Operation::Move {
            source: path_field(it)?,
            target: path_field(it)?,
        },
        "rename" => Operation::Rename {
            source: path_field(it)?,
            target: path_field(it)?,
        },
        "delete" => Operation::Delete {
            path: path_field(it)?,
            permanent: match it.next()? {
                "0" => false,
                "1" => true,
                _ => return None,
            },
        },
        "newfolder" => Operation::NewFolder(path_field(it)?),
        "newfile" => Operation::NewFile(path_field(it)?),
        "restore" => Operation::Restore(RecycledItem {
            original: path_field(it)?,
            recycled: path_field(it)?,
        }),
        "seq" => {
            let n: usize = it.next()?.parse().ok()?;
            if n > 100_000 {
                return None;
            }
            let mut ops = Vec::with_capacity(n.min(16));
            for _ in 0..n {
                ops.push(parse_op(it, depth + 1)?);
            }
            Operation::Sequence(ops)
        }
        _ => return None,
    })
}

fn parse_line(line: &str) -> Option<(char, Entry)> {
    let mut it: Fields = line.split('\t').peekable();
    let stack = match it.next()? {
        "U" => 'U',
        "R" => 'R',
        _ => return None,
    };
    let forward = parse_op(&mut it, 0)?;
    let inverse = if it.peek() == Some(&"-") {
        it.next();
        None
    } else {
        Some(parse_op(&mut it, 0)?)
    };
    let reason = unesc(it.next()?)?.into_string().ok()?;
    if it.next().is_some() {
        return None; // trailing junk
    }
    // An entry with no inverse must say why; one with an inverse must not claim otherwise.
    let reason = (!reason.is_empty()).then_some(reason);
    Some((
        stack,
        Entry {
            forward,
            inverse,
            reason,
            group: 0,
        },
    ))
}
