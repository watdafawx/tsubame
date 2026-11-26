//! Whole-frame tests for CARD B11-H: the path edit's autocomplete dropdown, Ctrl+E, Ctrl+Alt+T
//! and the path bar's right-click menu with Copy address / Paste and go.
//! (The parser, ranking, key rules and cancellation are unit-tested in `path_suggest.rs`.)

use super::*;
use crate::uia_rows::{self, PART_SUGGEST};
use tsubame_win::uia::{VAct, VReq};

const CTRL_ALT: Mods = Mods { ctrl: true, shift: false, alt: true };

fn s(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Waits for the debounce and the listing the dropdown asked for, then paints.
fn wait_rows(r: &mut Rig) {
    let t0 = Instant::now();
    loop {
        r.v.on_suggest_timer();
        r.v.pump();
        if r.v.suggest_timer(Instant::now()).is_none() && !r.v.psug.listing() {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "the listing did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    r.frame();
}

/// Ctrl+L, then the field holds `text` and one more char is typed (so the dropdown is armed).
fn type_path(r: &mut Rig, text: &str) {
    r.key(Key::Char('L'), Mods::CTRL);
    assert!(r.v.editing);
    let (head, last) = text.split_at(text.len() - 1);
    r.v.path_text.set_text(head);
    r.ui.push(Event::Char(last.chars().next().unwrap()));
    r.frame();
    wait_rows(r);
}

fn labels(r: &Rig) -> Vec<String> {
    r.v.psug.items.iter().map(|i| i.label.clone()).collect()
}

fn with_folders(r: &Rig) {
    for d in ["Alpha", "alpine", "Beta"] {
        std::fs::create_dir_all(r.dir.join(d)).unwrap();
    }
}

#[test]
fn ctrl_e_focuses_the_filter_like_ctrl_f_and_f3_stays_quick_look() {
    let mut r = Rig::new("b11h-ctrle");
    r.key(Key::Char('E'), Mods::CTRL);
    assert!(r.ui.has_focus(pid(r.v.live, ID_FILTER)), "Ctrl+E reaches the search box");
    r.ui.set_focus(Some(pid(r.v.live, ID_LIST)));
    r.key(Key::Char('F'), Mods::CTRL);
    assert!(r.ui.has_focus(pid(r.v.live, ID_FILTER)), "Ctrl+F still does");
    assert_eq!(r.v.keymap.lookup(crate::keymap::Chord::new(Key::F(3), Mods::NONE)), Some(CMD_PREVIEW_QUICK_LOOK), "F3 is untouched");
}

#[test]
fn ctrl_alt_t_opens_a_terminal_in_the_shown_folder() {
    let mut r = Rig::new("b11h-term");
    suggestv::TERMINALS.with(|t| t.borrow_mut().clear());
    r.key(Key::Char('T'), CTRL_ALT);
    assert_eq!(suggestv::TERMINALS.with(|t| t.borrow().clone()), vec![s(&r.dir)]);
    // from an archive: the folder the archive sits in
    suggestv::TERMINALS.with(|t| t.borrow_mut().clear());
    let zip = format!("{}\\pack.zip::sub", s(&r.dir));
    assert_eq!(crate::archive_ui::host_folder_of(&zip).map(|p| s(&p)), Some(s(&r.dir)));
}

#[test]
fn typing_in_the_path_edit_lists_matching_subfolders_below_the_bar() {
    let mut r = Rig::new("b11h-list");
    with_folders(&r);
    let dir = s(&r.dir);
    type_path(&mut r, &format!("{dir}\\al"));
    assert_eq!(labels(&r), ["alpine", "Alpha"], "prefix, any case (same case first), folders only: Docs and the files stay out");
    assert!(r.v.psug.visible());
    // the field keeps the keyboard
    assert!(r.ui.has_focus(pid(r.v.live, ID_PATH_EDIT)));
    // no timer left once the listing is in
    assert!(r.v.suggest_timer(Instant::now()).is_none());
}

#[test]
fn down_up_tab_complete_and_enter_opens_the_highlighted_row() {
    let mut r = Rig::new("b11h-keys");
    with_folders(&r);
    let dir = s(&r.dir);
    type_path(&mut r, &format!("{dir}\\al"));
    r.key(Key::Down, Mods::NONE);
    r.key(Key::Down, Mods::NONE);
    assert_eq!(r.v.psug.sel, Some(1));
    r.key(Key::Up, Mods::NONE);
    assert_eq!(r.v.psug.sel, Some(0));
    assert_eq!(r.v.path_text.text, format!("{dir}\\al"), "highlighting does not touch the text");
    // Tab completes the segment and adds a backslash; the focus stays in the field
    r.key(Key::Tab, Mods::NONE);
    assert_eq!(r.v.path_text.text, format!("{dir}\\alpine\\"));
    assert!(r.ui.has_focus(pid(r.v.live, ID_PATH_EDIT)), "Tab did not move the focus");
    assert!(r.v.editing);
    // Enter on a highlighted row opens it
    wait_rows(&mut r);
    r.v.path_text.set_text(&format!("{dir}\\a"));
    r.ui.push(Event::Char('l'));
    r.frame();
    wait_rows(&mut r);
    r.key(Key::Down, Mods::NONE);
    r.key(Key::Down, Mods::NONE);
    r.key(Key::Enter, Mods::NONE);
    r.settle();
    assert_eq!(r.v.tab().path, format!("{dir}\\Alpha"));
    assert!(!r.v.editing);
    assert!(!r.v.psug.active, "the session ends with the edit");
}

#[test]
fn enter_without_a_highlight_opens_the_typed_text() {
    let mut r = Rig::new("b11h-enter");
    with_folders(&r);
    let dir = s(&r.dir);
    type_path(&mut r, &format!("{dir}\\Beta"));
    assert_eq!(labels(&r), ["Beta"]);
    r.key(Key::Enter, Mods::NONE);
    r.settle();
    assert_eq!(r.v.tab().path, format!("{dir}\\Beta"));
}

#[test]
fn right_arrow_at_the_end_completes_like_tab() {
    let mut r = Rig::new("b11h-right");
    with_folders(&r);
    let dir = s(&r.dir);
    type_path(&mut r, &format!("{dir}\\Be"));
    r.key(Key::Right, Mods::NONE);
    assert_eq!(r.v.path_text.text, format!("{dir}\\Beta\\"));
}

#[test]
fn escape_closes_the_dropdown_first_and_the_edit_second() {
    let mut r = Rig::new("b11h-esc");
    with_folders(&r);
    let dir = s(&r.dir);
    type_path(&mut r, &format!("{dir}\\al"));
    assert!(r.v.psug.visible());
    r.key(Key::Escape, Mods::NONE);
    assert!(!r.v.psug.visible(), "the first Esc closes the list");
    assert!(r.v.editing, "and keeps the edit");
    r.key(Key::Escape, Mods::NONE);
    assert!(!r.v.editing, "the second Esc closes the edit");
    assert!(!r.v.psug.active);
    assert!(r.v.suggest_timer(Instant::now()).is_none());
}

#[test]
fn clicking_a_row_opens_that_folder_without_losing_the_field() {
    let mut r = Rig::new("b11h-click");
    with_folders(&r);
    let dir = s(&r.dir);
    type_path(&mut r, &format!("{dir}\\al"));
    let (bar, row_h) = (r.v.psug.bar, r.ui.theme.height_list_row);
    // second row: "Alpha"
    r.click(bar.x + 60.0, bar.b() + 2.0 + 4.0 + row_h * 1.5);
    r.settle();
    assert_eq!(r.v.tab().path, format!("{dir}\\Alpha"));
    assert!(!r.v.editing);
}

#[test]
fn an_empty_field_lists_the_recent_folders() {
    let mut r = Rig::new("b11h-recent");
    with_folders(&r);
    let alpha = s(&r.dir.join("Alpha"));
    r.v.side.recents = vec![alpha.clone()];
    r.key(Key::Char('L'), Mods::CTRL);
    r.key(Key::Backspace, Mods::NONE);
    assert_eq!(r.v.path_text.text, "");
    r.frame();
    let recent: Vec<_> = r.v.psug.items.iter().filter(|i| i.kind == crate::path_suggest::ItemKind::Recent).collect();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].nav, alpha);
    assert!(r.c.has_text("Alpha"), "{:?}", r.c.texts());
    assert!(r.v.suggest_timer(Instant::now()).is_none(), "an empty field lists nothing: no timer");
}

#[test]
fn environment_variables_expand_for_the_listing_and_stay_as_typed() {
    let mut r = Rig::new("b11h-env");
    let name = r.dir.file_name().unwrap().to_string_lossy().into_owned();
    let typed = format!("%TEMP%\\{}", &name[..name.len() - 3]);
    type_path(&mut r, &typed);
    assert!(labels(&r).contains(&name), "{:?}", labels(&r));
    r.key(Key::Tab, Mods::NONE);
    assert!(r.v.path_text.text.starts_with("%TEMP%\\") && r.v.path_text.text.ends_with('\\'), "{}", r.v.path_text.text);
}

#[test]
fn a_parent_change_waits_for_the_debounce_and_idle_has_no_timer() {
    let mut r = Rig::new("b11h-idle");
    with_folders(&r);
    let dir = s(&r.dir);
    r.key(Key::Char('L'), Mods::CTRL);
    wait_rows(&mut r);
    assert!(r.v.suggest_timer(Instant::now()).is_none(), "nothing typed: no timer");
    r.v.path_text.set_text(&format!("{dir}\\"));
    r.ui.push(Event::Char('A'));
    r.frame();
    assert!(r.v.suggest_timer(Instant::now()).is_some(), "a new parent arms the one-shot");
    assert!(!r.v.psug.visible(), "no rows before the debounce");
    wait_rows(&mut r);
    assert!(r.v.psug.visible());
    assert!(r.v.suggest_timer(Instant::now()).is_none(), "fired: no timer left");
    // typing on inside the same parent only re-filters
    r.ui.push(Event::Char('l'));
    r.frame();
    assert!(r.v.suggest_timer(Instant::now()).is_none());
    assert_eq!(labels(&r), ["Alpha", "alpine"]);
}

#[test]
fn the_dropdown_rows_are_published_to_ui_automation_and_can_be_invoked() {
    uia_rows::force(true);
    let mut r = Rig::new("b11h-uia");
    with_folders(&r);
    let dir = s(&r.dir);
    type_path(&mut r, &format!("{dir}\\al"));
    r.frame();
    let surf = uia_rows::last().surfaces.into_iter().find(|x| x.key == uia_rows::skey(0, PART_SUGGEST)).expect("suggestions surface");
    assert_eq!(surf.name, "Address suggestions");
    assert_eq!(surf.size, 2);
    assert_eq!(surf.items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["alpine", "Alpha"]);
    assert!(surf.items.iter().all(|i| i.invokable));
    assert_eq!(surf.items[1].value, format!("{dir}\\Alpha"));
    r.v.run_view_request(VReq { surf: surf.key, key: 1, act: VAct::Invoke });
    r.settle();
    assert_eq!(r.v.tab().path, format!("{dir}\\Alpha"));
    // closed: nothing published
    r.frame();
    assert!(uia_rows::last().surfaces.iter().all(|x| x.key != uia_rows::skey(0, PART_SUGGEST)));
}

fn open_bar_menu(r: &mut Rig) {
    // the bar's rect is known from an edit frame
    r.key(Key::Char('L'), Mods::CTRL);
    let bar = r.v.psug.bar;
    r.key(Key::Escape, Mods::NONE);
    assert!(!r.v.editing);
    let pos = Pt::new(bar.x + 12.0, bar.y + bar.h / 2.0);
    r.ui.push(Event::MouseMove { pos });
    r.frame();
    r.ui.push(Event::MouseDown { pos, button: Button::Right, mods: Mods::NONE, double: false });
    r.frame();
    r.ui.push(Event::MouseUp { pos, button: Button::Right, mods: Mods::NONE });
    r.frame();
}

#[test]
fn right_click_on_the_path_bar_opens_its_menu() {
    let mut r = Rig::new("b11h-menu");
    open_bar_menu(&mut r);
    assert!(r.v.nav_menu_open(), "right click opens the address bar menu");
    let m = r.v.nav_menu.as_ref().unwrap();
    assert_eq!(m.a11y_name(), "Address bar menu");
    let rows: Vec<String> = m.test_rows().into_iter().map(|x| x.label).collect();
    assert_eq!(rows, ["Edit address", "Copy address", "Copy address as text with quotes", "Paste and go"]);
    let keys: Vec<String> = m.test_rows().into_iter().map(|x| x.shortcut).collect();
    assert_eq!(keys[0], "Ctrl+L");
}

#[test]
fn copy_address_plain_and_quoted() {
    let mut r = Rig::new("b11h-copy");
    let dir = s(&r.dir);
    r.v.run_command(&mut r.ui, CMD_COPY_ADDRESS);
    assert_eq!(r.ui.clipboard.get().as_deref(), Some(dir.as_str()));
    r.v.run_command(&mut r.ui, CMD_COPY_ADDRESS_QUOTED);
    assert_eq!(r.ui.clipboard.get(), Some(format!("\"{dir}\"")));
}

#[test]
fn paste_and_go_opens_a_pasted_folder_and_says_why_it_cannot() {
    let mut r = Rig::new("b11h-paste");
    with_folders(&r);
    let dir = s(&r.dir);
    // a quoted folder path, as "Copy as path" makes it
    r.ui.clipboard.set(&format!("\"{dir}\\Alpha\""));
    r.v.run_command(&mut r.ui, CMD_PASTE_AND_GO);
    r.settle();
    r.settle();
    assert_eq!(r.v.tab().path, format!("{dir}\\Alpha"));
    // a folder that is not there: back where it was, with a message
    r.ui.clipboard.set(&format!("{dir}\\NoSuchFolder"));
    r.v.run_command(&mut r.ui, CMD_PASTE_AND_GO);
    r.settle();
    r.settle();
    assert_eq!(r.v.tab().path, format!("{dir}\\Alpha"), "back in the previous folder");
    assert!(r.v.ops.message.as_deref().is_some_and(|m| m.starts_with("Can't open")), "{:?}", r.v.ops.message);
    // text that is no path at all
    r.v.ops.message = None;
    r.ui.clipboard.set("not a path");
    r.v.run_command(&mut r.ui, CMD_PASTE_AND_GO);
    r.settle();
    assert!(r.v.ops.message.as_deref().is_some_and(|m| m.starts_with("Can't open")), "{:?}", r.v.ops.message);
    assert_eq!(r.v.tab().path, format!("{dir}\\Alpha"));
    // an empty clipboard
    r.v.ops.message = None;
    r.ui.clipboard.set("");
    r.v.run_command(&mut r.ui, CMD_PASTE_AND_GO);
    assert!(r.v.ops.message.as_deref().is_some_and(|m| m.starts_with("Nothing to paste")), "{:?}", r.v.ops.message);
}
