//! Settings > Extensions: one switch per optional built-in extension.
//!
//! Switching is live (`View::set_extension_enabled`) and saved as
//! `[extensions] disabled` in the config. Columns have their own switches in
//! the column menu, so only feature extensions are listed here.

use super::Cx;
use super::lay::Pg;
use tsubame_ext_api::Tier;

/// What an extension does, in one line (unknown ids show their id).
fn describe(id: &str) -> &'static str {
    match id {
        "tsubame.vfs.archive" => "Browse .zip, .tar, .7z and other archives like folders; extract here or selected.",
        "tsubame.ext.folder_sizes" => "Calculate folder sizes in the background.",
        "tsubame.ext.checksum" => "Copy CRC32, MD5, SHA-1 and SHA-256 of the selected files.",
        "tsubame.ext.global_search" => "Search every drive (Ctrl+Shift+F).",
        "tsubame.ext.preview" => "Preview pane (Alt+P) and Quick Look (Space / F3).",
        "tsubame.ext.batch_rename" => "F2 on several items: pattern bar with live old to new preview, one undo step. Adds the Rename N items entry to the context menu and the palette.",
        _ => "",
    }
}

pub fn paint(pg: &mut Pg, cx: &mut Cx) {
    pg.heading("Extensions", "Built-in features you can switch off. A switched-off feature removes its commands, shortcuts and menu entries.");
    let rows: Vec<(String, String, bool)> = crate::ext::read(|r| {
        r.extensions()
            .filter(|(i, _)| i.tier == Tier::BuiltIn && !i.required && !i.id.starts_with("tsubame.column."))
            .map(|(i, on)| (i.id.to_string(), i.title.to_string(), on))
            .collect()
    });
    pg.section("Built-in extensions");
    let mut c = pg.card(rows.len().max(1), 52.0);
    for (id, title, on) in &rows {
        let desc = match describe(id) {
            "" => id.as_str(),
            d => d,
        };
        if let Some(v) = pg.toggle_row(&mut c, title, desc, *on, true) {
            cx.ext_toggle = Some((id.clone(), v));
        }
    }
}
