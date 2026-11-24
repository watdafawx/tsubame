//! Existing-file conflicts while extracting: the choice, "apply to all" memory
//! and the "name (2)" rename. Pure logic, no UI: the app supplies the question.
use std::path::{Path, PathBuf};

/// What to do with one extracted file whose target already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Keep the existing file, drop the archive entry.
    Skip,
    /// Overwrite the existing file.
    Replace,
    /// Write the entry under a free `name (2)` next to the existing file.
    KeepBoth,
}

/// The asker's answer: the choice and whether it also answers every later conflict.
pub type ConflictAnswer = (ConflictChoice, bool);

/// Remembers an "apply to all" answer so the asker is bothered once per extraction.
#[derive(Debug, Default)]
pub struct ConflictState {
    sticky: Option<ConflictChoice>,
}

impl ConflictState {
    /// The choice for `target`: the remembered one, else ask (and remember when the
    /// answer says "apply to all").
    pub fn decide(&mut self, target: &Path, ask: &mut dyn FnMut(&Path) -> ConflictAnswer) -> ConflictChoice {
        if let Some(choice) = self.sticky {
            return choice;
        }
        let (choice, all) = ask(target);
        if all {
            self.sticky = Some(choice);
        }
        choice
    }
}

/// `name.ext` -> `name (2).ext`, then `name (3).ext`, ... the first one `exists`
/// does not claim. The new name stays in the same folder, so the extraction root
/// check that cleared `original` still holds.
pub fn keep_both_path(original: &Path, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let mut n = 2u32;
    loop {
        let mut name = original.file_stem().unwrap_or_default().to_os_string();
        name.push(format!(" ({n})"));
        if let Some(ext) = original.extension() {
            name.push(".");
            name.push(ext);
        }
        let candidate = original.with_file_name(name);
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}
