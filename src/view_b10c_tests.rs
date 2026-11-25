//! B10-C, Explorer parity file basics: Tab / Shift+Tab in rename, the
//! "change the extension?" question, the New item entering rename mode, the
//! shortcut commands and the search wording. Child of `view::tests`: it uses
//! the `Rig`.

use super::*;

const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };

/// Rows of the Rig folder: 0 Docs, 1 alpha.txt, 2 beta.txt.
fn select_row(r: &mut Rig, i: usize) {
    let t = r.v.tab_mut();
    t.sel.single(&t.store, i);
    t.list.cursor = i;
    r.frame();
}

/// F2 on row `i`, the whole text replaced by `text`.
fn rename_to(r: &mut Rig, i: usize, text: &str) {
    select_row(r, i);
    r.key(Key::F(2), Mods::NONE);
    assert!(r.v.rename.is_some(), "F2 starts the rename");
    r.key(Key::Char('A'), Mods::CTRL);
    for c in text.chars() {
        r.ui.push(Event::Char(c));
    }
    r.frame();
    assert_eq!(r.v.rename.as_ref().unwrap().name(), text);
}

fn wait_for(r: &mut Rig, what: &str, f: impl Fn(&Rig) -> bool) {
    let t0 = Instant::now();
    while !f(r) {
        r.v.pump();
        r.frame();
        assert!(t0.elapsed() < Duration::from_secs(10), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn tab_renames_and_moves_to_the_next_row() {
    let mut r = Rig::new("b10c-tab");
    rename_to(&mut r, 1, "alpha2.txt");
    r.key(Key::Tab, Mods::NONE);
    assert_eq!(r.v.rename.as_ref().map(|e| e.name().to_string()), Some("beta.txt".into()), "the next row is in rename mode");
    wait_for(&mut r, "the rename lands", |r| r.dir.join("alpha2.txt").exists());
    assert!(!r.dir.join("alpha.txt").exists());
}

#[test]
fn shift_tab_moves_to_the_previous_row_even_when_nothing_changed() {
    let mut r = Rig::new("b10c-shifttab");
    select_row(&mut r, 2);
    r.key(Key::F(2), Mods::NONE);
    assert!(r.v.rename.is_some());
    r.key(Key::Tab, SHIFT);
    assert_eq!(r.v.rename.as_ref().map(|e| e.name().to_string()), Some("alpha.txt".into()));
    // Tab on an unchanged name moves on too
    r.key(Key::Tab, Mods::NONE);
    assert_eq!(r.v.rename.as_ref().map(|e| e.name().to_string()), Some("beta.txt".into()));
}

#[test]
fn tab_on_the_last_row_ends_the_rename() {
    let mut r = Rig::new("b10c-tablast");
    select_row(&mut r, 2);
    r.key(Key::F(2), Mods::NONE);
    r.key(Key::Tab, Mods::NONE);
    assert!(r.v.rename.is_none());
}

#[test]
fn a_changed_extension_asks_first_and_no_returns_to_the_field() {
    let mut r = Rig::new("b10c-ext-no");
    rename_to(&mut r, 1, "alpha.md");
    r.key(Key::Enter, Mods::NONE);
    assert!(r.v.rename.is_none() && r.v.ext_ask.is_some(), "the question waits for after_frame");
    assert!(r.dir.join("alpha.txt").exists(), "nothing renamed yet");
    r.v.ext_answer(&mut r.ui, false);
    assert!(r.v.ext_ask.is_none());
    assert_eq!(r.v.rename.as_ref().map(|e| e.name().to_string()), Some("alpha.md".into()), "the typed name stays");
    r.frame();
    assert!(r.v.rename.is_some(), "and the field survives the next frames");
}

#[test]
fn yes_to_the_extension_question_renames() {
    let mut r = Rig::new("b10c-ext-yes");
    rename_to(&mut r, 1, "alpha.md");
    r.key(Key::Enter, Mods::NONE);
    assert!(r.v.ext_ask.is_some());
    r.v.ext_answer(&mut r.ui, true);
    assert!(r.v.ext_ask.is_none() && r.v.rename.is_none());
    wait_for(&mut r, "the rename lands", |r| r.dir.join("alpha.md").exists());
}

#[test]
fn case_only_and_folder_renames_do_not_ask() {
    let mut r = Rig::new("b10c-ext-quiet");
    rename_to(&mut r, 1, "ALPHA.TXT");
    r.key(Key::Enter, Mods::NONE);
    assert!(r.v.ext_ask.is_none(), "same extension, other case");
    wait_for(&mut r, "the rename lands", |r| std::fs::read_dir(&r.dir).unwrap().any(|e| e.unwrap().file_name() == "ALPHA.TXT"));
    let mut r = Rig::new("b10c-ext-dir");
    rename_to(&mut r, 0, "Docs.old");
    r.key(Key::Enter, Mods::NONE);
    assert!(r.v.ext_ask.is_none(), "a folder has no extension to protect");
}

#[test]
fn a_new_item_enters_rename_mode_once_it_is_listed() {
    let mut r = Rig::new("b10c-new");
    let folder = r.dir.to_string_lossy().into_owned();
    r.v.rename_when_listed(&folder, "gamma.txt");
    r.frame();
    assert!(r.v.rename.is_none(), "not listed yet: keep waiting");
    assert!(r.v.rename_wait.is_some());
    // the folder watcher arms on a worker: a change made before it would never show
    wait_for(&mut r, "the folder watcher is armed", |r| r.v.tab().watched());
    std::thread::sleep(Duration::from_millis(400));
    std::fs::write(r.dir.join("gamma.txt"), b"g").unwrap();
    wait_for(&mut r, "rename starts", |r| r.v.rename.is_some());
    assert_eq!(r.v.rename.as_ref().unwrap().name(), "gamma.txt");
    assert!(r.v.rename_wait.is_none());
}

#[test]
fn waiting_for_a_new_item_stops_when_the_folder_changes() {
    let mut r = Rig::new("b10c-newgone");
    r.v.rename_when_listed(r"C:\somewhere\else", "x.txt");
    r.frame();
    assert!(r.v.rename_wait.is_none());
}

#[test]
fn the_shortcut_commands_ask_the_shell_worker() {
    let mut r = Rig::new("b10c-shortcuts");
    r.v.run_command(&mut r.ui, crate::commands::CMD_CREATE_SHORTCUT);
    r.v.run_command(&mut r.ui, crate::commands::CMD_PASTE_SHORTCUT);
    assert_eq!(r.v.shell_reqs, [crate::newmenu::ShellReq::CreateShortcut, crate::newmenu::ShellReq::PasteShortcut]);
}

#[test]
fn the_empty_search_says_search() {
    let mut r = Rig::new("b10c-search");
    r.key(Key::Char('F'), Mods::CTRL);
    for c in "zzzqqq".chars() {
        r.ui.push(Event::Char(c));
    }
    r.frame();
    assert!(r.c.has_text("No items match your search"), "{:?}", r.c.texts());
    assert!(!r.c.has_text("No items match the filter"));
}
