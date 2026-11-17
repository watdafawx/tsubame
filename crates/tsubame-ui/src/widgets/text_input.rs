//! Single-line text input: caret, selection, mouse select, clipboard via
//! `Ui::clipboard`, Home/End, Ctrl+arrows by word, Backspace/Delete
//! (Ctrl: by word), and the IME composition string drawn at the caret.
//!
//! The caret moves by `char`, not by grapheme cluster (a combining mark is
//! its own step). Good enough for file names; revisit if it bites.

use crate::canvas::{Canvas, floor_char};
use crate::geom::Rect;
use crate::input::{Button, Cursor, Event, Key, Mods};
use crate::ui::{Id, Sense, Ui};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextInput {
    pub text: String,
    /// caret, byte index (always a char boundary)
    pub caret: usize,
    /// other end of the selection (== caret: none)
    pub anchor: usize,
    /// horizontal scroll of the text inside the box
    pub scroll_x: f32,
    /// a drag started by a double click selects whole words; the word it
    /// started on
    word_drag: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextResponse {
    /// the text changed this frame
    pub changed: bool,
    /// Enter
    pub submitted: bool,
    /// Escape
    pub cancelled: bool,
    pub focused: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

impl TextInput {
    pub fn new(s: &str) -> TextInput {
        TextInput { text: s.to_string(), caret: s.len(), anchor: s.len(), ..Default::default() }
    }
    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }
    /// (start, end) of the selection.
    pub fn sel(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }
    pub fn selected(&self) -> &str {
        let (a, b) = self.sel();
        &self.text[a..b]
    }
    pub fn set_text(&mut self, s: &str) {
        self.text.clear();
        self.text.push_str(s);
        self.caret = s.len();
        self.anchor = s.len();
    }
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }
    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (a, b) = self.sel();
        self.text.replace_range(a..b, "");
        self.caret = a;
        self.anchor = a;
        true
    }
    /// Inserts `s` over the selection. Line breaks become spaces.
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let clean: String = s.chars().map(|c| if c == '\r' || c == '\n' || c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.caret, &clean);
        self.caret += clean.len();
        self.anchor = self.caret;
    }
    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map(|(j, _)| j).unwrap_or(0)
    }
    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }
    /// Start of the word left of `i` (skips spaces first).
    pub fn word_left(&self, mut i: usize) -> usize {
        let t = &self.text;
        while i > 0 && class(t[..i].chars().next_back().unwrap()) == Class::Space {
            i = self.prev(i);
        }
        if i == 0 {
            return 0;
        }
        let k = class(t[..i].chars().next_back().unwrap());
        while i > 0 && class(t[..i].chars().next_back().unwrap()) == k {
            i = self.prev(i);
        }
        i
    }
    /// End of the word right of `i`, then past the spaces after it
    /// (Windows edit-control behaviour).
    pub fn word_right(&self, mut i: usize) -> usize {
        let t = &self.text;
        let n = t.len();
        if i < n {
            let k = class(t[i..].chars().next().unwrap());
            if k != Class::Space {
                while i < n && class(t[i..].chars().next().unwrap()) == k {
                    i = self.next(i);
                }
            }
        }
        while i < n && class(t[i..].chars().next().unwrap()) == Class::Space {
            i = self.next(i);
        }
        i
    }
    /// The word (or run of spaces/punctuation) around `i`.
    pub fn word_at(&self, i: usize) -> (usize, usize) {
        let t = &self.text;
        if t.is_empty() {
            return (0, 0);
        }
        let i = floor_char(t, i);
        let at = if i < t.len() { i } else { self.prev(i) };
        let k = class(t[at..].chars().next().unwrap());
        let (mut a, mut b) = (at, self.next(at));
        while a > 0 && class(t[..a].chars().next_back().unwrap()) == k {
            a = self.prev(a);
        }
        while b < t.len() && class(t[b..].chars().next().unwrap()) == k {
            b = self.next(b);
        }
        (a, b)
    }
    fn move_to(&mut self, i: usize, extend: bool) {
        self.caret = i;
        if !extend {
            self.anchor = i;
        }
    }

    /// Applies one key. Returns (used, text changed). Clipboard keys need
    /// `clip`; Enter and Escape are reported by `text_input`, not here.
    pub fn key(&mut self, key: Key, mods: Mods, clip: &mut dyn crate::ui::Clipboard) -> (bool, bool) {
        let ext = mods.shift;
        let word = mods.ctrl;
        match key {
            Key::Left => {
                let to = if self.has_selection() && !ext && !word {
                    self.sel().0
                } else if word {
                    self.word_left(self.caret)
                } else {
                    self.prev(self.caret)
                };
                self.move_to(to, ext);
                (true, false)
            }
            Key::Right => {
                let to = if self.has_selection() && !ext && !word {
                    self.sel().1
                } else if word {
                    self.word_right(self.caret)
                } else {
                    self.next(self.caret)
                };
                self.move_to(to, ext);
                (true, false)
            }
            Key::Home => {
                self.move_to(0, ext);
                (true, false)
            }
            Key::End => {
                self.move_to(self.text.len(), ext);
                (true, false)
            }
            Key::Backspace => {
                if self.delete_selection() {
                    return (true, true);
                }
                let from = if word { self.word_left(self.caret) } else { self.prev(self.caret) };
                if from == self.caret {
                    return (true, false);
                }
                self.text.replace_range(from..self.caret, "");
                self.move_to(from, false);
                (true, true)
            }
            Key::Delete if !mods.shift => {
                if self.delete_selection() {
                    return (true, true);
                }
                let to = if word { self.word_right(self.caret) } else { self.next(self.caret) };
                if to == self.caret {
                    return (true, false);
                }
                self.text.replace_range(self.caret..to, "");
                (true, true)
            }
            Key::Char('A') if mods == Mods::CTRL => {
                self.select_all();
                (true, false)
            }
            Key::Char('C') | Key::Insert if mods == Mods::CTRL => {
                if self.has_selection() {
                    clip.set(self.selected());
                }
                (true, false)
            }
            Key::Char('X') if mods == Mods::CTRL => self.cut(clip),
            Key::Delete if mods == Mods::SHIFT => self.cut(clip),
            Key::Char('V') if mods == Mods::CTRL => self.paste(clip),
            Key::Insert if mods == Mods::SHIFT => self.paste(clip),
            _ => (false, false),
        }
    }
    fn cut(&mut self, clip: &mut dyn crate::ui::Clipboard) -> (bool, bool) {
        if !self.has_selection() {
            return (true, false);
        }
        clip.set(self.selected());
        self.delete_selection();
        (true, true)
    }
    fn paste(&mut self, clip: &mut dyn crate::ui::Clipboard) -> (bool, bool) {
        match clip.get() {
            Some(s) if !s.is_empty() => {
                self.insert(&s);
                (true, true)
            }
            _ => (true, false),
        }
    }
}

/// The text field in `rect`. `placeholder` shows while it is empty.
pub fn text_input(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, st: &mut TextInput, placeholder: &str) -> TextResponse {
    let mut resp = TextResponse::default();
    let font = ui.theme.font();
    let inner = rect.pad4(6.0, 0.0, 6.0, 0.0);
    let it = ui.interact(id, rect, Sense::click().focusable().cursor(Cursor::IBeam));
    st.caret = floor_char(&st.text, st.caret);
    st.anchor = floor_char(&st.text, st.anchor);

    // mouse: press sets the caret (Shift extends), double selects a word,
    // drag extends
    let origin = inner.x - st.scroll_x;
    if let Some(p) = it.pressed
        && p.button == Button::Left
    {
        let i = c.hit_index(&st.text, font, p.pos.x - origin);
        if p.double {
            let (a, b) = st.word_at(i);
            st.anchor = a;
            st.caret = b;
            st.word_drag = Some((a, b));
        } else {
            st.word_drag = None;
            st.caret = i;
            if !p.mods.shift {
                st.anchor = i;
            }
        }
    }
    if it.active && it.pressed.is_none() {
        let i = c.hit_index(&st.text, font, ui.mouse().x - origin);
        if let Some((wa, wb)) = st.word_drag {
            let (a, b) = st.word_at(i);
            if i < wa {
                (st.anchor, st.caret) = (wb, a);
            } else {
                (st.anchor, st.caret) = (wa, b.max(wb));
            }
        } else {
            st.caret = i;
        }
    }

    // keys and chars (only while focused)
    let mut clip = std::mem::replace(&mut ui.clipboard, Box::new(crate::ui::MemClipboard::default()));
    for (qi, ev) in ui.key_input(id) {
        match ev {
            Event::Char(ch) => {
                if !ch.is_control() {
                    ui.consume(qi);
                    let mut b = [0u8; 4];
                    st.insert(ch.encode_utf8(&mut b));
                    resp.changed = true;
                } else {
                    // control chars from Ctrl+letters (WM_CHAR 0x01..): the
                    // KeyDown already did the work; never insert them
                    ui.consume(qi);
                }
            }
            Event::KeyDown { key: Key::Enter, mods, .. } if mods.none() => {
                ui.consume(qi);
                resp.submitted = true;
            }
            Event::KeyDown { key: Key::Escape, mods, .. } if mods.none() => {
                ui.consume(qi);
                resp.cancelled = true;
            }
            Event::KeyDown { key, mods, .. } => {
                let (used, changed) = st.key(key, mods, &mut *clip);
                if used {
                    ui.consume(qi);
                }
                resp.changed |= changed;
            }
            _ => {}
        }
    }
    ui.clipboard = clip;
    resp.focused = ui.has_focus(id);

    // draw
    let t = ui.theme;
    let focused = resp.focused && ui.window_focused();
    c.round(rect, t.radius_md, t.field);
    c.round_stroke(rect, t.radius_md, if focused { t.focus } else { t.border }, 1.0);
    c.clip(inner);
    if st.text.is_empty() && ui.composition().is_empty() || !resp.focused && st.text.is_empty() {
        st.scroll_x = 0.0;
        if !placeholder.is_empty() {
            c.text(placeholder, inner, font, t.text_dim);
        }
        if focused {
            ui.mark_text_focus();
            if ui.caret_on {
                c.fill(Rect::new(inner.x, inner.y + 4.0, 1.0, inner.h - 8.0), t.text);
            }
            ui.set_ime_caret(Rect::new(inner.x, inner.y + 3.0, 1.0, inner.h - 6.0));
        }
        c.unclip();
        return resp;
    }
    if !focused {
        // no measuring for a field nobody types in
        st.scroll_x = 0.0;
        c.text(&st.text, inner, font, t.text);
        c.unclip();
        return resp;
    }
    // the composition is shown inside the text at the caret
    let comp = ui.composition();
    let (shown, comp_range): (std::borrow::Cow<str>, Option<(usize, usize)>) = if comp.is_empty() {
        (std::borrow::Cow::Borrowed(st.text.as_str()), None)
    } else {
        let mut s = String::with_capacity(st.text.len() + comp.len());
        s.push_str(&st.text[..st.caret]);
        s.push_str(comp);
        s.push_str(&st.text[st.caret..]);
        (std::borrow::Cow::Owned(s), Some((st.caret, st.caret + comp.len())))
    };
    let caret_byte = comp_range.map(|r| r.1).unwrap_or(st.caret);
    let caret_px = c.caret_x(&shown, font, caret_byte);
    let full_w = c.measure(&shown, font);
    // keep the caret inside the box
    let room = (inner.w - 2.0).max(1.0);
    if caret_px - st.scroll_x > room {
        st.scroll_x = caret_px - room;
    }
    if caret_px < st.scroll_x {
        st.scroll_x = caret_px;
    }
    st.scroll_x = st.scroll_x.clamp(0.0, (full_w - room).max(0.0));
    let x0 = inner.x - st.scroll_x;
    if st.has_selection() && comp_range.is_none() {
        let (a, b) = st.sel();
        let (xa, xb) = (c.caret_x(&shown, font, a), c.caret_x(&shown, font, b));
        c.fill(Rect::new(x0 + xa, inner.y + 3.0, xb - xa, inner.h - 6.0), t.selected);
    }
    // wide box: the text never trims, the clip cuts it
    c.text(&shown, Rect::new(x0, inner.y, full_w + 8.0, inner.h), font, t.text);
    if let Some((a, b)) = comp_range {
        let (xa, xb) = (c.caret_x(&shown, font, a), c.caret_x(&shown, font, b));
        c.fill(Rect::new(x0 + xa, inner.b() - 5.0, xb - xa, 1.0), t.text);
    }
    let cx = (x0 + caret_px).round();
    ui.mark_text_focus();
    if ui.caret_on {
        c.fill(Rect::new(cx, inner.y + 4.0, 1.0, inner.h - 8.0), t.text);
    }
    ui.set_ime_caret(Rect::new(cx, inner.y + 3.0, 1.0, inner.h - 6.0));
    c.unclip();
    resp
}
