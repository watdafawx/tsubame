//! Settings > General: where a window opens, hidden files, deleting.

use super::lay::Pg;
use super::{C_HIDE, C_OTHER, C_PREVIEW, Cx};
use tsubame_ui::{Id, Rect, TextInput, text_input};

const ID_FOLDER: Id = Id::new("settings-start-folder");

#[derive(Default)]
pub struct State {
    folder: TextInput,
    /// the folder typed exists (checked when the text changes, not every frame)
    ok: bool,
    checked: String,
    inited: bool,
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("General", "Settings apply as soon as you change them. They are saved in config.toml.");
    let g = &mut cx.prefs.general;
    let specific = g.start_folder.trim() != "last" && !g.start_folder.trim().is_empty();

    pg.section("Startup");
    let rows = if specific { 3 } else { 2 };
    let mut c = pg.card(rows, 52.0);
    if let Some(i) = pg.seg_row(&mut c, "Start folder", "Where a new window opens when there is no session to restore.", &["Last folder", "A specific folder"], specific as usize) {
        g.start_folder = if i == 0 { "last".to_string() } else { std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".to_string()) };
        if i == 1 {
            st.inited = false;
        }
        cx.changed |= C_OTHER;
    }
    if specific {
        let row = pg.next_row(&mut c);
        pg.row_text(row, "Folder to open", "", 400.0, true);
        if !st.inited {
            st.folder.set_text(&g.start_folder);
            st.inited = true;
            st.checked.clear();
        }
        let fr = Rect::new(row.r() - 16.0 - 380.0, row.y + (row.h - 28.0) / 2.0, 380.0, 28.0);
        let resp = text_input(pg.ui, pg.g, ID_FOLDER, fr, &mut st.folder, "C:\\Users\\you\\Downloads");
        if pg.ui.has_focus(ID_FOLDER) && pg.ui.window_focused() {
            pg.ui.mark_text_focus();
        }
        if resp.changed {
            g.start_folder = st.folder.text.trim().to_string();
            cx.changed |= C_OTHER;
        }
        if st.checked != st.folder.text {
            st.checked = st.folder.text.clone();
            st.ok = std::path::Path::new(st.folder.text.trim()).is_dir();
        }
        if !st.ok && !st.folder.text.trim().is_empty() {
            pg.g.text("This folder does not exist.", Rect::new(fr.x, fr.b() + 1.0, fr.w, 14.0), pg.t.font_small(), pg.t.danger);
        }
    }
    if let Some(v) = pg.toggle_row(&mut c, "Restore previous session", "Reopen the tabs, panes and scroll positions you had when you closed Tsubame.", g.restore_session, true) {
        g.restore_session = v;
        cx.changed |= C_OTHER;
    }

    pg.section("Files and folders");
    let mut c = pg.card(2, 52.0);
    if let Some(v) = pg.toggle_row(&mut c, "Show hidden files", "Same as Ctrl+H. Items with the hidden attribute are drawn in a dimmer colour.", g.show_hidden, true) {
        g.show_hidden = v;
        cx.changed |= C_HIDE;
    }
    if let Some(v) = pg.toggle_row(&mut c, "Show protected system files", "Hidden and system at once: pagefile.sys, desktop.ini. Leave off unless you need them.", g.show_system, true) {
        g.show_system = v;
        cx.changed |= C_HIDE;
    }

    pg.section("Deleting");
    let mut c = pg.card(2, 52.0);
    if let Some(i) = pg.seg_row(&mut c, "When I press Delete", "Shift+Delete does the other one. Items in the Recycle Bin can be restored with Ctrl+Z.", &["Move to Recycle Bin", "Delete permanently"], (!g.delete_to_recycle) as usize) {
        g.delete_to_recycle = i == 0;
        cx.changed |= C_OTHER;
    }
    if let Some(v) = pg.toggle_row(&mut c, "Confirm before deleting", "Ask first. Permanent deletes always ask, whatever this says.", g.confirm_delete, true) {
        g.confirm_delete = v;
        cx.changed |= C_OTHER;
    }

    let pv_ext = crate::preview_ui::is_enabled();
    let pv = &mut cx.prefs.preview;
    pg.section("Preview");
    let mut c = pg.card(5, 52.0);
    if let Some(v) = pg.toggle_row(&mut c, "Show preview pane (Alt+P)", "Dock a resizable preview pane on the right edge of the window.", pv.enabled, pv_ext) {
        pv.enabled = v;
        cx.changed |= C_PREVIEW;
    }
    if let Some(v) = pg.toggle_row(&mut c, "Enable Quick Look (Space / F3)", "Open a centered preview overlay when pressing Space or F3 in the file list.", pv.quick_look, pv_ext) {
        pv.quick_look = v;
        cx.changed |= C_PREVIEW;
    }
    if let Some(v) = pg.toggle_row(&mut c, "Syntax tinting in text preview", "Highlight keywords, types, strings, numbers and comments in code files.", pv.syntax_highlight, pv_ext) {
        pv.syntax_highlight = v;
        cx.changed |= C_PREVIEW;
    }
    if let Some(v) = pg.toggle_row(&mut c, "Auto 8-byte hex in narrow pane", "Switch hex dumps to 8 bytes per row when the docked pane is narrower than 420 px.", pv.hex_auto_width, pv_ext) {
        pv.hex_auto_width = v;
        cx.changed |= C_PREVIEW;
    }
    if let Some(v) = pg.toggle_row(&mut c, "Direct hex for binaries", "Show hex directly for .exe, .dll, .bin and .dat files instead of an Unsupported card.", pv.direct_hex_binaries, pv_ext) {
        pv.direct_hex_binaries = v;
        cx.changed |= C_PREVIEW;
    }
}
