//! Settings > Tags (P4.F6): marks or plain dots, the optional Tags column, the
//! tag list (rename, colour and mark, order, delete in two clicks), export and
//! import as JSON, and how full the store is.
//!
//! The page never touches `tags.db`: it reads the view's last snapshot
//! (`Cx::tags`) and queues requests for the tag worker (`TagsCx::reqs`).

use super::lay::{Look, Pg, button};
use super::{C_OTHER, Cx};
use crate::tags::marks;
use crate::tags::model::{self, COLOURS, Shape, TagDef};
use crate::tags::worker::Req;
use tsubame_ui::{Id, Rect, TextInput, text_input};

const ID_NAME: Id = Id::new("tags-page-name");
const ID_ROW: Id = Id::new("tags-page-row");
const ID_EXPORT: Id = Id::new("tags-page-export");
const ID_IMPORT: Id = Id::new("tags-page-import");
const ID_RETRY: Id = Id::new("tags-page-retry");

/// What the Tags page sees of the tag store, and what it asks of it.
#[derive(Default)]
pub struct TagsCx {
    /// the tags extension is on
    pub on: bool,
    pub writable: bool,
    pub defs: Vec<TagDef>,
    pub counts: Vec<u32>,
    pub items: usize,
    pub usage: f32,
    /// the store's own note (restored from the backup ...)
    pub note: Option<String>,
    /// where export and import read and write
    pub json: std::path::PathBuf,
    /// requests for the worker, sent after the frame
    pub reqs: Vec<Req>,
}

#[derive(Default)]
pub struct State {
    /// tag being renamed: (slot, field)
    rename: Option<(u8, TextInput)>,
    /// first click of Delete: (slot)
    confirm: Option<u8>,
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("Tags", "Colour tags with a shape each, kept in tags.db next to the settings file. Files are never changed.");
    let t = pg.t;
    let tc = &mut cx.tags;
    if !tc.on {
        pg.note("\u{E946}", "The Tags extension is switched off (Settings > Extensions). Tags stay in tags.db until it is switched on again.", false);
        return;
    }

    pg.section("Display");
    let mut c = pg.card(2, 52.0);
    if let Some(v) = pg.toggle_row(&mut c, "Shapes", "Each colour also has its own shape, so colour is never the only cue. Off: plain dots.", cx.prefs.tags.shapes, true) {
        cx.prefs.tags.shapes = v;
        cx.changed |= C_OTHER;
    }
    if let Some(v) = pg.toggle_row(&mut c, "Tags column", "An extra Details column with up to two tag chips per row (168 px).", cx.prefs.tags.column, true) {
        cx.prefs.tags.column = v;
        cx.changed |= C_OTHER;
    }

    pg.section("Tags");
    if !tc.writable {
        let r = pg.note("\u{E7BA}", "Tags are read-only: tags.db is locked by another process. Changes will not be saved.", true);
        if button(pg.ui, pg.g, ID_RETRY, r.r() - 12.0, r.y + (r.h - t.height_button) / 2.0, "Retry", Look::Normal, true, true).0 {
            tc.reqs.push(Req::Retry);
        }
    }
    let n = tc.defs.len();
    let rh = 40.0;
    let mut c = pg.card(n.max(1), rh);
    if n == 0 {
        let row = pg.next_row(&mut c);
        pg.row_text(row, "No tags", "Create one from the picker (Alt+T) or the + in the sidebar's Tags header.", 0.0, true);
    }
    let shapes = cx.prefs.tags.shapes;
    let ro = !tc.writable;
    for (i, d) in tc.defs.clone().iter().enumerate() {
        let row = pg.next_row(&mut c);
        if !pg.visible(row) {
            continue;
        }
        let base = ID_ROW.with(d.slot as u64 * 16);
        // mark: click cycles the colour (and its mark)
        let mr = Rect::new(row.x + 10.0, row.y + 6.0, 28.0, 28.0);
        let it = pg.ui.interact(base.with(1), mr, tsubame_ui::Sense::click());
        if it.hovered && !ro {
            pg.g.round(mr, t.radius_md, t.hover);
        }
        marks::draw(pg.g, marks::mark_of(d, shapes), d.rgb(t.is_dark()), mr.x + 14.0, mr.y + 14.0, 14.0);
        if it.clicked == Some(tsubame_ui::Button::Left) && !ro {
            let next = (d.colour as usize + 1) % COLOURS.len();
            tc.reqs.push(Req::Look { slot: d.slot, colour: next as u8, shape: Shape::ALL[next] });
        }
        // name (click to rename)
        let nr = Rect::new(row.x + 48.0, row.y + 6.0, (row.w * 0.4).max(120.0), 28.0);
        let renaming = st.rename.as_ref().is_some_and(|(s, _)| *s == d.slot);
        if renaming {
            if let Some((slot, input)) = &mut st.rename {
                let resp = text_input(pg.ui, pg.g, ID_NAME, nr, input, "Tag name");
                if resp.submitted {
                    if let Some(name) = model::clean_name(&input.text) {
                        tc.reqs.push(Req::Rename { slot: *slot, name });
                    }
                    st.rename = None;
                } else if resp.cancelled || !resp.focused {
                    st.rename = None;
                }
            }
        } else {
            let it = pg.ui.interact(base.with(2), nr, tsubame_ui::Sense::click().cursor(tsubame_ui::Cursor::IBeam));
            if it.hovered && !ro {
                pg.g.round(nr, t.radius_sm, t.hover);
            }
            pg.g.text(&d.name, Rect::new(nr.x + 6.0, nr.y, nr.w - 12.0, nr.h), t.font_md(), t.text);
            if it.clicked == Some(tsubame_ui::Button::Left) && !ro {
                st.rename = Some((d.slot, TextInput::new(&d.name)));
                pg.ui.set_focus(Some(ID_NAME));
            }
        }
        let count = tc.counts.get(d.slot as usize).copied().unwrap_or(0);
        let look = format!("{} \u{b7} {} \u{b7} {} item{}", model::COLOUR_NAMES[d.colour as usize % COLOURS.len()], if shapes { d.shape.name() } else { "dot" }, crate::tags::ui::sep(count as usize), if count == 1 { "" } else { "s" });
        pg.g.text(&look, Rect::new(nr.r() + 12.0, row.y, 200.0, row.h), t.font_small(), t.text_dim);
        // order and delete, right-aligned
        let mut x = row.r() - 12.0;
        let del_label = if st.confirm == Some(d.slot) { "Delete for good" } else { "Delete" };
        let (del, r) = button(pg.ui, pg.g, base.with(3), x, row.y + (row.h - t.height_button) / 2.0, del_label, if st.confirm == Some(d.slot) { Look::DangerFill } else { Look::Danger }, !ro, true);
        if del {
            if st.confirm == Some(d.slot) {
                tc.reqs.push(Req::Delete { slot: d.slot });
                st.confirm = None;
            } else {
                st.confirm = Some(d.slot);
            }
        }
        x = r.x - 6.0;
        let (down, r) = button(pg.ui, pg.g, base.with(4), x, row.y + (row.h - t.height_button) / 2.0, "\u{2193}", Look::Normal, !ro && i + 1 < n, true);
        if down {
            tc.reqs.push(Req::Move { slot: d.slot, by: 1 });
        }
        x = r.x - 4.0;
        if button(pg.ui, pg.g, base.with(5), x, row.y + (row.h - t.height_button) / 2.0, "\u{2191}", Look::Normal, !ro && i > 0, true).0 {
            tc.reqs.push(Req::Move { slot: d.slot, by: -1 });
        }
    }

    pg.section("Store");
    let mut c = pg.card(2, 52.0);
    let row = pg.next_row(&mut c);
    let desc = format!("{} tagged item{} \u{b7} {:.0}% of the 100,000 item / 16 MB cap", crate::tags::ui::sep(tc.items), if tc.items == 1 { "" } else { "s" }, tc.usage * 100.0);
    pg.row_text(row, "tags.db", &desc, 120.0, true);
    let bar = Rect::new(row.r() - 16.0 - 100.0, row.y + row.h / 2.0 - 2.0, 100.0, 4.0);
    pg.g.round(bar, 2.0, t.scrollbar);
    pg.g.round(Rect::new(bar.x, bar.y, bar.w * tc.usage.clamp(0.0, 1.0), bar.h), 2.0, if tc.usage >= crate::tags::store::WARN_AT { t.danger } else { t.accent });
    let row = pg.next_row(&mut c);
    pg.row_text(row, "Export and import", &format!("JSON (paths and tag names): {}", tc.json.display()), 220.0, true);
    let (imp, r) = button(pg.ui, pg.g, ID_IMPORT, row.r() - 16.0, row.y + (row.h - t.height_button) / 2.0, "Import", Look::Normal, !ro, true);
    if imp {
        tc.reqs.push(Req::Import(tc.json.clone()));
    }
    if button(pg.ui, pg.g, ID_EXPORT, r.x - 8.0, row.y + (row.h - t.height_button) / 2.0, "Export", Look::Normal, true, true).0 {
        tc.reqs.push(Req::Export(tc.json.clone()));
    }
    if tc.usage >= crate::tags::store::WARN_AT {
        pg.note("\u{E7BA}", "The tag store is over 90% full. Untag files you no longer need; new tags are refused at the cap.", true);
    }
    if let Some(n) = &tc.note {
        pg.note("\u{E946}", n, false);
    }
}
