//! The "New" submenu of the empty-space context menu and the work behind it:
//! a folder, a text document, and the file types Windows registers in
//! `HKCR\.ext\ShellNew` (`tsubame_win::shellnew`, scanned once per session on
//! a background thread). Picking a row makes ONE file-op queue operation
//! (`NewFolder`, `NewFile`, or a `Copy` of the type's template), so it is one
//! undo step, and the new item goes straight into rename mode.
//!
//! Also here: the small requests the view cannot run itself (it has no pool
//! and no clipboard) and `shellops` takes after the frame.

use crate::ole_glue;
use std::path::{Path, PathBuf};
use tsubame_win::fileops::Operation;
use tsubame_win::shell::ctxmodel::{ItemKind, ItemSource, MenuItem};
use tsubame_win::shellnew::{self, Content, Entry};

/// The action of the "New" submenu's row.
pub const ACT_NEW: &str = "x.new";
pub const ACT_FOLDER: &str = "x.new-folder";
pub const ACT_TEXT: &str = "x.new-file";
/// `x.new:.docx`: a registered type
const ACT_TYPE: &str = "x.new:";

/// Work for `shellops` that a command (palette, key) asks of the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellReq {
    /// shortcuts next to the selected items
    CreateShortcut,
    /// shortcuts in this folder to the files on the clipboard
    PasteShortcut,
}

/// What a New row makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewKind {
    Folder,
    Text,
    /// a registered type, by extension (".docx")
    Type(String),
}

/// The kind a menu action stands for.
pub fn kind_of_action(act: &str) -> Option<NewKind> {
    match act {
        ACT_FOLDER => Some(NewKind::Folder),
        ACT_TEXT => Some(NewKind::Text),
        _ => act.strip_prefix(ACT_TYPE).filter(|e| e.starts_with('.') && e.len() > 1).map(|e| NewKind::Type(e.to_string())),
    }
}

/// The name and the operation that make a new item in `folder`. `taken` says
/// whether the folder already has a name (lower case). `None`: a type the scan
/// does not know (it changed since the menu opened).
pub fn plan(kind: &NewKind, folder: &Path, taken: &dyn Fn(&str) -> bool, types: &[Entry]) -> Option<(String, Operation)> {
    let at = |n: &str| -> PathBuf { folder.join(n) };
    Some(match kind {
        NewKind::Folder => {
            let name = ole_glue::free_name("New folder", "", taken);
            let op = Operation::NewFolder(at(&name));
            (name, op)
        }
        NewKind::Text => {
            let name = ole_glue::free_name("New Text Document", ".txt", taken);
            let op = Operation::NewFile(at(&name));
            (name, op)
        }
        NewKind::Type(ext) => {
            let e = types.iter().find(|e| e.ext.eq_ignore_ascii_case(ext))?;
            let name = ole_glue::free_name(&format!("New {}", e.label), &e.ext, taken);
            let op = match &e.content {
                Content::Empty => Operation::NewFile(at(&name)),
                Content::Template(src) => Operation::Copy { source: src.clone(), target: at(&name) },
            };
            (name, op)
        }
    })
}

/// The scanned types (empty while the scan runs).
pub fn types() -> Vec<Entry> {
    shellnew::entries().map(|v| v.to_vec()).unwrap_or_default()
}

/// The "New >" row with its children: Folder, Text document, then a
/// separator and the registered types.
pub fn submenu() -> MenuItem {
    submenu_of(&types())
}

fn submenu_of(types: &[Entry]) -> MenuItem {
    let row = |label: &str, act: &str| MenuItem { key: format!("app:{act}"), ..MenuItem::command(ItemSource::Tsubame, label, act) };
    let mut kids = vec![row("Folder", ACT_FOLDER), row("Text Document", ACT_TEXT)];
    if !types.is_empty() {
        kids.push(MenuItem::separator(ItemSource::Tsubame));
        kids.extend(types.iter().map(|e| row(&e.label, &format!("{ACT_TYPE}{}", e.ext))));
    }
    MenuItem { kind: ItemKind::Submenu, children: kids, key: format!("app:{ACT_NEW}"), ..MenuItem::command(ItemSource::Tsubame, "New", ACT_NEW) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ext: &str, label: &str, content: Content) -> Entry {
        Entry { ext: ext.into(), label: label.into(), content }
    }

    #[test]
    fn actions_name_their_kind() {
        assert_eq!(kind_of_action("x.new-folder"), Some(NewKind::Folder));
        assert_eq!(kind_of_action("x.new-file"), Some(NewKind::Text));
        assert_eq!(kind_of_action("x.new:.docx"), Some(NewKind::Type(".docx".into())));
        assert_eq!(kind_of_action("x.new:docx"), None);
        assert_eq!(kind_of_action("x.new:"), None);
        assert_eq!(kind_of_action("x.new"), None, "the submenu itself makes nothing");
        assert_eq!(kind_of_action("file.paste"), None);
    }

    #[test]
    fn plans_make_the_right_operation_and_a_free_name() {
        let f = Path::new(r"C:\w");
        let none = |_: &str| false;
        let types = vec![entry(".bmp", "Bitmap image", Content::Empty), entry(".docx", "Word Document", Content::Template(PathBuf::from(r"C:\T\w.docx")))];
        let (n, op) = plan(&NewKind::Folder, f, &none, &types).unwrap();
        assert_eq!((n.as_str(), op), ("New folder", Operation::NewFolder(PathBuf::from(r"C:\w\New folder"))));
        let busy = |n: &str| n == "new folder" || n == "new folder (2)";
        assert_eq!(plan(&NewKind::Folder, f, &busy, &types).unwrap().0, "New folder (3)");
        let (n, op) = plan(&NewKind::Text, f, &none, &types).unwrap();
        assert_eq!((n.as_str(), op), ("New Text Document.txt", Operation::NewFile(PathBuf::from(r"C:\w\New Text Document.txt"))));
        let (n, op) = plan(&NewKind::Type(".bmp".into()), f, &none, &types).unwrap();
        assert_eq!((n.as_str(), op), ("New Bitmap image.bmp", Operation::NewFile(PathBuf::from(r"C:\w\New Bitmap image.bmp"))));
        // a template is copied into place (one undoable operation)
        let (n, op) = plan(&NewKind::Type(".DOCX".into()), f, &none, &types).unwrap();
        assert_eq!(n, "New Word Document.docx");
        assert_eq!(op, Operation::Copy { source: PathBuf::from(r"C:\T\w.docx"), target: PathBuf::from(r"C:\w\New Word Document.docx") });
        assert!(plan(&NewKind::Type(".nope".into()), f, &none, &types).is_none());
    }

    #[test]
    fn the_submenu_lists_the_builtin_rows_then_the_registered_types() {
        let sub = submenu_of(&[]);
        assert_eq!(sub.kind, ItemKind::Submenu);
        let l: Vec<_> = sub.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(l, ["Folder", "Text Document"]);
        let sub = submenu_of(&[entry(".bmp", "Bitmap image", Content::Empty)]);
        let l: Vec<_> = sub.children.iter().map(|c| (c.label.as_str(), c.action.as_str())).collect();
        assert_eq!(l, [("Folder", "x.new-folder"), ("Text Document", "x.new-file"), ("", ""), ("Bitmap image", "x.new:.bmp")]);
        assert!(sub.children[2].is_separator());
        // every row has a stable key of its own
        let keys: Vec<_> = sub.children.iter().filter(|c| !c.is_separator()).map(|c| c.key.clone()).collect();
        assert_eq!(keys.len(), 3);
        assert!(keys.iter().all(|k| k.starts_with("app:x.new")));
        let mut u = keys.clone();
        u.sort();
        u.dedup();
        assert_eq!(u.len(), keys.len());
    }
}
