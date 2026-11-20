//! Inline rename: the text field that replaces a row's name (F2, or a slow
//! second click), with the error bubble under it. Pure UI: the field, its
//! validation per keystroke and what the user decided. The view does the rename.

use crate::overlay;
use crate::rename::{self, NameError};
use tsubame_ui::{Canvas, Event, Id, Key, Rect, TextInput, Theme, Ui, f, text_input};

pub struct RenameEdit {
    /// full path of the entry being renamed
    pub path: String,
    pub original: String,
    pub parent: String,
    pub input: TextInput,
    pub error: Option<NameError>,
    /// frames drawn (a click away only ends the edit once the field had the focus)
    pub frames: u32,
}

/// What the user decided this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameOut {
    None,
    /// Enter, or the field lost the focus with a valid name; `replace`: Shift+Enter
    /// over an existing name
    Commit { replace: bool },
    /// Esc, or a click away with an invalid name
    Cancel,
    /// Tab: commit and start renaming the next row
    CommitNext,
}

impl RenameEdit {
    pub fn new(path: &str, is_dir: bool) -> RenameEdit {
        let original = crate::dnd::name_of(path).to_string();
        let parent = crate::nav::parent(path).unwrap_or_default();
        let mut input = TextInput::default();
        input.set_text(&original);
        // everything but the extension is selected (folders: all of it)
        let stem = rename::stem_chars(&original, is_dir);
        let end = original.char_indices().nth(stem).map(|(i, _)| i).unwrap_or(original.len());
        input.anchor = 0;
        input.caret = end;
        RenameEdit { path: path.to_string(), original, parent, input, error: None, frames: 0 }
    }

    pub fn name(&self) -> &str {
        &self.input.text
    }

    pub fn unchanged(&self) -> bool {
        self.input.text == self.original
    }

    /// Checks the typed name; `exists` says whether the folder already has it.
    pub fn revalidate(&mut self, exists: &dyn Fn(&str) -> bool) {
        self.error = rename::validate(&self.input.text, &self.original, &self.parent, exists).err();
    }

    /// The name may be replaced (Shift+Enter): the only problem is that it exists.
    pub fn can_replace(&self) -> bool {
        self.error.as_ref().is_some_and(NameError::can_replace)
    }

    pub fn valid(&self) -> bool {
        self.error.is_none()
    }
}

/// Draws the field in `rect` (the row's name cell) and the bubble below it.
pub fn paint(ui: &mut Ui, g: &mut dyn Canvas, t: &Theme, id: Id, rect: Rect, edit: &mut RenameEdit, exists: &dyn Fn(&str) -> bool) -> RenameOut {
    edit.frames += 1;
    if edit.frames == 1 {
        edit.revalidate(exists);
    }
    g.fill(rect, t.popup);
    let resp = text_input(ui, g, id, rect, &mut edit.input, "");
    if resp.changed {
        edit.revalidate(exists);
        ui.request_repaint();
    }
    let bad = edit.error.is_some();
    g.round_stroke(rect.pad(-0.5), t.radius_sm, if bad { t.danger } else { t.accent }, 1.5);

    let mut out = RenameOut::None;
    for (i, ev) in ui.key_input(id) {
        if let Event::KeyDown { key: Key::Enter, mods, .. } = ev
            && mods.shift
            && !mods.ctrl
            && !mods.alt
        {
            ui.consume(i);
            if edit.can_replace() {
                out = RenameOut::Commit { replace: true };
            }
        }
    }
    if resp.submitted && bad {
        // Enter is ignored while the name is invalid; the bubble says why
        ui.request_repaint();
    } else if resp.submitted {
        out = RenameOut::Commit { replace: false };
    }
    if resp.cancelled {
        out = RenameOut::Cancel;
    }
    if out == RenameOut::None && edit.frames > 2 && !ui.has_focus(id) {
        out = if bad { RenameOut::Cancel } else { RenameOut::Commit { replace: false } };
    }
    bubble(g, t, ui.size(), rect, edit);
    out
}

/// The reason and the keys, under the field (above it near the window's bottom).
fn bubble(g: &mut dyn Canvas, t: &Theme, win: (f32, f32), field: Rect, edit: &RenameEdit) {
    let Some(err) = &edit.error else {
        // a valid name: just the keys
        return keys_line(g, t, win, field, None, &[("Enter", "Rename"), ("Esc", "Cancel")]);
    };
    let msg = err.message();
    if edit.can_replace() {
        keys_line(g, t, win, field, Some(&msg), &[("Shift+Enter", "Replace"), ("Esc", "Cancel")]);
    } else {
        keys_line(g, t, win, field, Some(&msg), &[("Esc", "Cancel")]);
    }
}

fn keys_line(g: &mut dyn Canvas, t: &Theme, win: (f32, f32), field: Rect, msg: Option<&str>, keys: &[(&str, &str)]) {
    let font = t.font_small();
    let mut w = 16.0;
    if let Some(m) = msg {
        w += 22.0 + g.measure(m, font) + 16.0;
    }
    for (k, l) in keys {
        w += g.measure(k, t.font_mono()) + 10.0 + 6.0 + g.measure(l, font) + 14.0;
    }
    let w = w.max(field.w).min(win.0 - 8.0);
    let h = 28.0;
    let below = field.b() + 4.0;
    let y = if below + h > win.1 - 4.0 { field.y - h - 4.0 } else { below };
    let x = field.x.min(win.0 - w - 4.0).max(4.0);
    let r = Rect::new(x, y, w, h);
    g.round(r, t.radius_md, t.popup);
    g.round_stroke(r, t.radius_md, if msg.is_some() { t.danger } else { t.popup_border }, 1.0);
    let cy = r.y + r.h / 2.0;
    let mut cx = r.x + 8.0;
    if let Some(m) = msg {
        g.text("\u{E783}", Rect::new(cx, r.y, 16.0, r.h), f(13.0).icon().c(), t.danger);
        cx += 22.0;
        let mw = g.measure(m, font);
        g.text(m, Rect::new(cx, r.y, mw + 2.0, r.h), font, t.text);
        cx += mw + 16.0;
    }
    // keys on the right
    let mut right = r.r() - 8.0;
    for (k, l) in keys.iter().rev() {
        let lw = g.measure(l, font) + 2.0;
        if right - lw < cx {
            break;
        }
        g.text(l, Rect::new(right - lw, r.y, lw, r.h), font, t.text_dim);
        right -= lw + 6.0;
        right -= overlay::keycap_right(g, t, right, cy, k) + 14.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_is_preselected_and_extension_kept() {
        let e = RenameEdit::new("C:\\data\\report.final.txt", false);
        assert_eq!(e.original, "report.final.txt");
        assert_eq!(e.parent, "C:\\data");
        assert_eq!((e.input.anchor, e.input.caret), (0, "report.final".len()));
        let d = RenameEdit::new("C:\\data\\my.folder", true);
        assert_eq!(d.input.caret, "my.folder".len());
    }

    #[test]
    fn validation_follows_the_typed_name() {
        let mut e = RenameEdit::new("C:\\data\\a.txt", false);
        let exists = |n: &str| n.eq_ignore_ascii_case("b.txt");
        e.revalidate(&exists);
        assert!(e.valid() && e.unchanged());
        e.input.set_text("b.txt");
        e.revalidate(&exists);
        assert!(e.can_replace());
        e.input.set_text("a:b");
        e.revalidate(&exists);
        assert!(!e.valid() && !e.can_replace());
        e.input.set_text("c.txt");
        e.revalidate(&exists);
        assert!(e.valid() && !e.unchanged());
    }
}
