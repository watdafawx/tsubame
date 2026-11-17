//! Kit tests on `TestCanvas`.

use crate::testcanvas::Op;
use crate::*;

const W: f32 = 800.0;
const H: f32 = 600.0;

struct Harness {
    ui: Ui,
    c: TestCanvas,
}

impl Harness {
    fn new() -> Harness {
        Harness { ui: Ui::default(), c: TestCanvas::new(W, H) }
    }
    fn frame(&mut self, mut f: impl FnMut(&mut Ui, &mut TestCanvas)) -> Output {
        self.c.clear();
        assert!(self.c.begin(0));
        self.ui.begin_frame((W, H));
        f(&mut self.ui, &mut self.c);
        let o = self.ui.end_frame();
        self.c.end();
        o
    }
    fn push(&mut self, ev: Event) -> bool {
        self.ui.push(ev)
    }
    fn move_to(&mut self, x: f32, y: f32) -> bool {
        self.push(Event::MouseMove { pos: Pt::new(x, y) })
    }
    fn down(&mut self, x: f32, y: f32) -> bool {
        self.down_m(x, y, Mods::NONE, false)
    }
    fn down_m(&mut self, x: f32, y: f32, mods: Mods, double: bool) -> bool {
        self.push(Event::MouseDown { pos: Pt::new(x, y), button: Button::Left, mods, double })
    }
    fn up(&mut self, x: f32, y: f32) -> bool {
        self.push(Event::MouseUp { pos: Pt::new(x, y), button: Button::Left, mods: Mods::NONE })
    }
    fn key(&mut self, key: Key, mods: Mods) -> bool {
        self.push(Event::KeyDown { key, mods, repeat: false })
    }
    fn typ(&mut self, s: &str) {
        for ch in s.chars() {
            self.push(Event::Char(ch));
        }
    }
}

// ------------------------------------------------------------------ layout

#[test]
fn layout_cuts_columns_anchor() {
    let mut r = Rect::new(0.0, 0.0, 100.0, 50.0);
    let top = r.cut_top(10.0);
    assert_eq!(top, Rect::new(0.0, 0.0, 100.0, 10.0));
    assert_eq!(r, Rect::new(0.0, 10.0, 100.0, 40.0));
    let right = r.cut_right(20.0);
    assert_eq!(right, Rect::new(80.0, 10.0, 20.0, 40.0));
    assert_eq!(r.cut_left(500.0).w, 80.0, "cuts clamp");
    assert_eq!(r.w, 0.0);

    let cols = Rect::new(0.0, 0.0, 100.0, 10.0).columns(&[Len::Px(20.0), Len::Fill(1.0), Len::Fill(3.0)], 0.0);
    assert_eq!(cols[0].w, 20.0);
    assert_eq!(cols[1], Rect::new(20.0, 0.0, 20.0, 10.0));
    assert_eq!(cols[2], Rect::new(40.0, 0.0, 60.0, 10.0));
    let a = Rect::new(0.0, 0.0, 100.0, 100.0).anchor(Anchor::BottomRight, 10.0, 20.0);
    assert_eq!(a, Rect::new(90.0, 80.0, 10.0, 20.0));
    assert_eq!(Rect::new(0.0, 0.0, 100.0, 100.0).anchor(Anchor::Center, 10.0, 10.0).x, 45.0);
    assert_eq!(Rect::new(10.0, 10.0, 20.0, 20.0).pad(5.0), Rect::new(15.0, 15.0, 10.0, 10.0));
    // half-open: adjacent rects never share a point
    assert!(Rect::new(0.0, 0.0, 10.0, 10.0).contains(9.99, 0.0));
    assert!(!Rect::new(0.0, 0.0, 10.0, 10.0).contains(10.0, 0.0));
}

#[test]
fn ids_are_stable_and_distinct() {
    assert_eq!(Id::new("list"), Id::new("list"));
    assert_ne!(Id::new("list"), Id::new("lisu"));
    assert_ne!(Id::new("tab").with(1), Id::new("tab").with(2));
    assert_eq!(Id::new("tab").with(7), Id::new("tab").with(7));
    const C: Id = Id::new("const");
    assert_eq!(C, Id::new("const"));
}

// ---------------------------------------------------- hit test, hot/active

const B1: Id = Id::new("b1");
const B2: Id = Id::new("b2");

fn two_buttons(ui: &mut Ui, c: &mut TestCanvas, clicks: &mut Vec<Id>) {
    // b2 overlaps b1's right half and is drawn later: it is on top
    if button(ui, c, B1, Rect::new(10.0, 10.0, 100.0, 30.0), "one") {
        clicks.push(B1);
    }
    if button(ui, c, B2, Rect::new(60.0, 10.0, 100.0, 30.0), "two") {
        clicks.push(B2);
    }
}

#[test]
fn topmost_widget_gets_the_click() {
    let mut h = Harness::new();
    let mut clicks = Vec::new();
    h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    // overlap region: b2 wins
    assert!(h.down(80.0, 20.0));
    assert!(h.up(80.0, 20.0));
    h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert_eq!(clicks, vec![B2]);
    // b1-only region
    h.down(20.0, 20.0);
    h.up(20.0, 20.0);
    h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert_eq!(clicks, vec![B2, B1]);
    // nothing there: not even a frame
    assert!(!h.down(500.0, 500.0));
    assert!(!h.up(500.0, 500.0));
}

#[test]
fn hot_active_transitions() {
    let mut h = Harness::new();
    let mut clicks = Vec::new();
    h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert_eq!(h.ui.hot(), None);
    // entering b1 changes hover: repaint
    assert!(h.move_to(20.0, 20.0));
    h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert_eq!(h.ui.hot(), Some(B1));
    // moving inside b1: no repaint
    assert!(!h.move_to(25.0, 22.0));
    // press: active
    h.down(25.0, 22.0);
    let out = h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert_eq!(h.ui.active(), Some(B1));
    assert!(out.capture, "a held press captures the mouse");
    // while active, b2 is never hot even under the pointer
    assert!(h.move_to(120.0, 20.0), "moves repaint while dragging");
    h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert_eq!(h.ui.hot(), None);
    assert_eq!(h.ui.active(), Some(B1));
    // release outside b1: no click, active cleared
    h.up(120.0, 20.0);
    let out = h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert!(clicks.is_empty());
    assert_eq!(h.ui.active(), None);
    assert!(!out.capture);
    // leaving the window clears hover
    h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert_eq!(h.ui.hot(), Some(B2));
    assert!(h.push(Event::MouseLeave));
    h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert_eq!(h.ui.hot(), None);
}

#[test]
fn focus_follows_press_and_tab_chain() {
    let mut h = Harness::new();
    let (mut t1, mut t2) = (TextInput::default(), TextInput::default());
    let (i1, i2) = (Id::new("t1"), Id::new("t2"));
    let mut draw = |h: &mut Harness| {
        h.frame(|ui, c| {
            text_input(ui, c, i1, Rect::new(10.0, 10.0, 200.0, 24.0), &mut t1, "");
            text_input(ui, c, i2, Rect::new(10.0, 50.0, 200.0, 24.0), &mut t2, "");
        })
    };
    draw(&mut h);
    assert_eq!(h.ui.focus(), None);
    h.down(20.0, 60.0);
    h.up(20.0, 60.0);
    draw(&mut h);
    assert_eq!(h.ui.focus(), Some(i2));
    h.key(Key::Tab, Mods::NONE);
    draw(&mut h);
    assert_eq!(h.ui.focus(), Some(i1), "Tab wraps to the first");
    h.key(Key::Tab, Mods::SHIFT);
    draw(&mut h);
    assert_eq!(h.ui.focus(), Some(i2));
    // window deactivation: no caret, drags end; focus stays for the return
    assert!(h.push(Event::FocusLost));
    draw(&mut h);
    assert!(!h.ui.window_focused());
    assert_eq!(h.ui.focus(), Some(i2));
}

#[test]
fn idle_frames_ask_for_nothing() {
    let mut h = Harness::new();
    let mut clicks = Vec::new();
    let out = h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert!(!out.repaint, "a static frame does not ask for another");
    assert!(!h.move_to(400.0, 400.0), "a move over nothing needs no frame");
    assert!(!h.push(Event::KeyUp { key: Key::Char('A'), mods: Mods::NONE }));
    // a wheel over nothing that scrolls
    assert!(!h.push(Event::Wheel { pos: Pt::new(20.0, 20.0), dy: 1.0, dx: 0.0, mods: Mods::NONE }));
    // a click asks for one more frame (the app may have changed state)
    h.down(20.0, 20.0);
    h.up(20.0, 20.0);
    let out = h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert!(out.repaint);
    let out = h.frame(|ui, c| two_buttons(ui, c, &mut clicks));
    assert!(!out.repaint, "and then stops");
}

// -------------------------------------------------------------------- list

struct Rows {
    n: usize,
    drawn: Vec<usize>,
    selected: std::collections::BTreeSet<usize>,
    anchor: usize,
}

impl Rows {
    fn new(n: usize) -> Rows {
        Rows { n, drawn: Vec::new(), selected: Default::default(), anchor: 0 }
    }
}

impl ListSource for Rows {
    fn len(&self) -> usize {
        self.n
    }
    fn is_selected(&self, i: usize) -> bool {
        self.selected.contains(&i)
    }
    fn draw_row(&mut self, c: &mut dyn Canvas, t: &Theme, i: usize, r: Rect, _st: RowState) {
        self.drawn.push(i);
        c.text("row", r, t.font(), t.text);
    }
    // a tiny Explorer-style model: plain = one, Ctrl = toggle, Shift = range
    fn select(&mut self, i: usize, mods: Mods, by: SelectBy) {
        if mods.shift {
            self.selected = (self.anchor.min(i)..=self.anchor.max(i)).collect();
        } else if mods.ctrl && by != SelectBy::Key {
            if !self.selected.remove(&i) {
                self.selected.insert(i);
            }
            self.anchor = i;
        } else if !mods.ctrl {
            self.selected = [i].into();
            self.anchor = i;
        }
    }
}

const LIST: Id = Id::new("list");
const LIST_RECT: Rect = Rect::new(0.0, 40.0, 600.0, 440.0); // rows area 586 x 440 = 20 rows

fn list_frame(h: &mut Harness, st: &mut ListState, rows: &mut Rows) -> ListResponse {
    rows.drawn.clear();
    let mut resp = ListResponse::default();
    h.frame(|ui, c| resp = list(ui, c, LIST, LIST_RECT, st, rows));
    resp
}

#[test]
fn list_draws_only_visible_rows() {
    let mut h = Harness::new();
    let mut st = ListState::new(22.0);
    let mut rows = Rows::new(1_000_000);
    let r = list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(r.range, (0, 20));
    assert_eq!(rows.drawn, (0..20).collect::<Vec<_>>());
    // half a row down: 21 rows intersect
    st.scroll = 11.0;
    let r = list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(r.range, (0, 21));
    assert_eq!(rows.drawn.len(), 21);
    // far down: only that window, nothing else asked
    st.scroll = 500_000.0 * 22.0;
    let r = list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(r.range, (500_000, 500_020));
    assert_eq!(rows.drawn, (500_000..500_020).collect::<Vec<_>>());
    assert_eq!(h.c.texts().len(), 20, "one text per drawn row, no off-screen draw calls");
    // every row draw call lies inside the clip
    let clip = Rect::new(LIST_RECT.x, LIST_RECT.y, LIST_RECT.w - h.ui.theme.layout_scrollbar_width, LIST_RECT.h);
    for op in &h.c.ops {
        if let Op::Text(_, r, ..) = op {
            assert!(r.intersects(&clip));
        }
    }
    // empty list draws no rows
    let mut empty = Rows::new(0);
    let r = list_frame(&mut h, &mut st, &mut empty);
    assert_eq!(r.range, (0, 0));
    assert!(empty.drawn.is_empty());
    assert_eq!(st.scroll, 0.0);
}

#[test]
fn list_scroll_maths_at_a_million_rows() {
    let n = 1_000_000usize;
    let mut h = Harness::new();
    let mut st = ListState::new(22.0);
    let mut rows = Rows::new(n);
    list_frame(&mut h, &mut st, &mut rows);
    h.ui.set_focus(Some(LIST));
    // End: last row exactly at the bottom
    h.key(Key::End, Mods::NONE);
    let r = list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.cursor, n - 1);
    assert_eq!(st.scroll, n as f64 * 22.0 - 440.0);
    assert_eq!(st.scroll, st.max_scroll(n));
    assert_eq!(r.range, (n - 20, n));
    assert_eq!(st.row_top(n - 1) + 22.0, 440.0, "bottom edge of the last row is the viewport bottom");
    assert_eq!(st.row_at_offset(439.9), n - 1);
    assert_eq!(st.row_at_offset(0.0), n - 20);
    // f32 would have lost this: 21,999,560 + 0.5 is not representable in f32
    st.scroll = 21_999_560.5;
    assert_eq!(st.row_at_offset(0.0), 999_980);
    assert_eq!(st.row_top(999_981), 21.5);
    st.scroll = st.max_scroll(n);
    // Up from the bottom: no scroll needed; PageUp: a page minus one
    h.key(Key::Up, Mods::NONE);
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.cursor, n - 2);
    assert_eq!(st.scroll, st.max_scroll(n));
    h.key(Key::PageUp, Mods::NONE);
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.cursor, n - 2 - 19);
    h.key(Key::PageUp, Mods::NONE);
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.cursor, n - 2 - 38);
    assert_eq!(st.scroll, (n - 40) as f64 * 22.0, "cursor row at the top");
    h.key(Key::Home, Mods::NONE);
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!((st.cursor, st.scroll), (0, 0.0));
    // wheel: 3 rows a notch, whole steps, clamped
    h.push(Event::Wheel { pos: Pt::new(100.0, 100.0), dy: -2.0, dx: 0.0, mods: Mods::NONE });
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.scroll, 6.0 * 22.0);
    h.push(Event::Wheel { pos: Pt::new(100.0, 100.0), dy: 10.0, dx: 0.0, mods: Mods::NONE });
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.scroll, 0.0);
}

#[test]
fn scrollbar_drag_and_track_click_at_a_million_rows() {
    let n = 1_000_000usize;
    let mut h = Harness::new();
    let mut st = ListState::new(22.0);
    let mut rows = Rows::new(n);
    list_frame(&mut h, &mut st, &mut rows);
    let sb_x = LIST_RECT.r() - 7.0;
    // track click below the thumb: one page
    assert!(h.down(sb_x, 300.0));
    h.up(sb_x, 300.0);
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.scroll, 19.0 * 22.0);
    // drag the thumb (min size 24 at the top) to the very bottom
    st.scroll = 0.0;
    list_frame(&mut h, &mut st, &mut rows);
    h.down(sb_x, LIST_RECT.y + 5.0);
    list_frame(&mut h, &mut st, &mut rows);
    h.move_to(sb_x, 5000.0);
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.scroll, st.max_scroll(n), "thumb at the bottom = last row at the bottom");
    // half way: the row in the middle of the data
    h.move_to(sb_x, LIST_RECT.y + 5.0 + (440.0 - 24.0) / 2.0);
    let r = list_frame(&mut h, &mut st, &mut rows);
    let mid = st.max_scroll(n) / 2.0;
    assert!((st.scroll - mid).abs() < 1e-6, "{} vs {mid}", st.scroll);
    assert_eq!(r.range.0, (mid / 22.0) as usize);
    h.up(sb_x, 0.0);
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(h.ui.active(), None);
}

#[test]
fn list_selection_hooks_and_activation() {
    let mut h = Harness::new();
    let mut st = ListState::new(22.0);
    let mut rows = Rows::new(100);
    list_frame(&mut h, &mut st, &mut rows);
    let y = |i: usize| LIST_RECT.y + i as f32 * 22.0 + 5.0;
    h.down(50.0, y(3));
    h.up(50.0, y(3));
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.cursor, 3);
    assert_eq!(h.ui.focus(), Some(LIST), "a press focuses the list");
    assert!(rows.is_selected(3));
    h.down_m(50.0, y(6), Mods::SHIFT, false);
    h.up(50.0, y(6));
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(rows.selected.iter().copied().collect::<Vec<_>>(), vec![3, 4, 5, 6]);
    h.down_m(50.0, y(4), Mods::CTRL, false);
    h.up(50.0, y(4));
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(rows.selected.iter().copied().collect::<Vec<_>>(), vec![3, 5, 6]);
    // Shift+Down extends from the anchor
    h.key(Key::Down, Mods::SHIFT);
    list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(st.cursor, 5);
    assert_eq!(rows.selected.iter().copied().collect::<Vec<_>>(), vec![4, 5]);
    // double click and Enter activate
    h.down_m(50.0, y(8), Mods::NONE, true);
    h.up(50.0, y(8));
    let r = list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(r.activated, Some(8));
    h.key(Key::Enter, Mods::NONE);
    let r = list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(r.activated, Some(8));
    // right click: context at the row
    h.push(Event::MouseDown { pos: Pt::new(50.0, y(2)), button: Button::Right, mods: Mods::NONE, double: false });
    h.push(Event::MouseUp { pos: Pt::new(50.0, y(2)), button: Button::Right, mods: Mods::NONE });
    let r = list_frame(&mut h, &mut st, &mut rows);
    assert_eq!(r.context.map(|c| c.0), Some(Some(2)));
    // hover changes per row, not per pixel
    assert!(h.move_to(50.0, y(10)));
    list_frame(&mut h, &mut st, &mut rows);
    assert!(!h.move_to(60.0, y(10) + 3.0), "same row: no repaint");
    assert!(h.move_to(60.0, y(11)), "next row: repaint");
}

// -------------------------------------------------------------- text input

#[test]
fn text_editing_keys() {
    let mut clip = MemClipboard::default();
    let mut t = TextInput::new("hello big world");
    assert_eq!(t.caret, 15);
    t.key(Key::Left, Mods::CTRL, &mut clip);
    assert_eq!(t.caret, 10, "ctrl+left: start of 'world'");
    t.key(Key::Left, Mods::CTRL, &mut clip);
    assert_eq!(t.caret, 6);
    t.key(Key::Right, Mods::CTRL, &mut clip);
    assert_eq!(t.caret, 10, "ctrl+right: past 'big' and its space");
    t.key(Key::Right, Mods::CTRL_SHIFT, &mut clip);
    assert_eq!(t.selected(), "world");
    t.key(Key::Char('C'), Mods::CTRL, &mut clip);
    assert_eq!(clip.0.as_deref(), Some("world"));
    t.key(Key::Home, Mods::NONE, &mut clip);
    assert!(!t.has_selection());
    t.key(Key::Char('V'), Mods::CTRL, &mut clip);
    assert_eq!(t.text, "worldhello big world");
    t.key(Key::Backspace, Mods::CTRL, &mut clip);
    assert_eq!(t.text, "hello big world");
    t.key(Key::End, Mods::NONE, &mut clip);
    t.key(Key::Backspace, Mods::NONE, &mut clip);
    assert_eq!(t.text, "hello big worl");
    t.key(Key::Home, Mods::NONE, &mut clip);
    t.key(Key::Delete, Mods::NONE, &mut clip);
    assert_eq!(t.text, "ello big worl");
    t.key(Key::Delete, Mods::CTRL, &mut clip);
    assert_eq!(t.text, "big worl");
    t.key(Key::Char('A'), Mods::CTRL, &mut clip);
    t.key(Key::Char('X'), Mods::CTRL, &mut clip);
    assert_eq!(t.text, "");
    assert_eq!(clip.0.as_deref(), Some("big worl"));
    // multibyte: steps by char, never splits one
    let mut t = TextInput::new("日本語.txt");
    t.key(Key::Home, Mods::NONE, &mut clip);
    t.key(Key::Right, Mods::NONE, &mut clip);
    assert_eq!(t.caret, 3);
    t.key(Key::Delete, Mods::NONE, &mut clip);
    assert_eq!(t.text, "日語.txt");
    t.key(Key::Right, Mods::SHIFT, &mut clip);
    t.insert("x\ny");
    assert_eq!(t.text, "日x y.txt", "selection replaced, newline flattened");
    // Left with a selection collapses to its start
    let mut t = TextInput::new("abcdef");
    t.key(Key::Left, Mods::SHIFT, &mut clip);
    t.key(Key::Left, Mods::SHIFT, &mut clip);
    t.key(Key::Left, Mods::NONE, &mut clip);
    assert_eq!((t.caret, t.anchor), (4, 4));
}

const TI: Id = Id::new("filter");
const TI_RECT: Rect = Rect::new(10.0, 10.0, 300.0, 24.0);

#[test]
fn text_input_widget_typing_mouse_and_composition() {
    let mut h = Harness::new();
    let mut t = TextInput::default();
    let mut resp = TextResponse::default();
    let mut run = |h: &mut Harness, t: &mut TextInput| {
        h.frame(|ui, c| resp = text_input(ui, c, TI, TI_RECT, t, "Filter"));
        resp
    };
    run(&mut h, &mut t);
    assert!(h.c.has_text("Filter"), "placeholder while empty");
    // unfocused: typing goes nowhere
    h.typ("ab");
    run(&mut h, &mut t);
    assert_eq!(t.text, "");
    assert_eq!(h.ui.leftover().len(), 2, "chars no widget took are left for the app");
    h.down(20.0, 20.0);
    h.up(20.0, 20.0);
    run(&mut h, &mut t);
    h.typ("hello world");
    h.push(Event::Char('\u{1}')); // Ctrl+A's WM_CHAR: never inserted
    let r = run(&mut h, &mut t);
    assert!(r.changed);
    assert_eq!(t.text, "hello world");
    assert!(h.ui.leftover().is_empty());
    // click inside the text: caret at the nearest char (test font: 6 DIP each)
    let x_of = |i: usize| TI_RECT.x + 6.0 + i as f32 * 6.0;
    h.down(x_of(5) + 1.0, 20.0);
    h.up(x_of(5) + 1.0, 20.0);
    run(&mut h, &mut t);
    assert_eq!(t.caret, 5);
    // drag selects
    h.down(x_of(0), 20.0);
    run(&mut h, &mut t);
    h.move_to(x_of(5), 20.0);
    run(&mut h, &mut t);
    h.up(x_of(5), 20.0);
    run(&mut h, &mut t);
    assert_eq!(t.selected(), "hello");
    // double click selects the word under it
    h.down_m(x_of(8), 20.0, Mods::NONE, true);
    h.up(x_of(8), 20.0);
    run(&mut h, &mut t);
    assert_eq!(t.selected(), "world");
    // IME: the composition shows at the caret, the text is untouched until commit
    t.caret = 5;
    t.anchor = 5;
    assert!(h.ui.set_composition("日本"));
    run(&mut h, &mut t);
    assert!(h.c.has_text("hello日本 world"));
    assert_eq!(t.text, "hello world");
    assert!(h.ui.set_composition(""));
    h.typ("日本");
    let out = h.frame(|ui, c| {
        text_input(ui, c, TI, TI_RECT, &mut t, "");
    });
    assert_eq!(t.text, "hello日本 world");
    assert!(out.ime_caret.is_some(), "the IME window is placed at the caret");
    // Enter submits, Escape cancels
    h.key(Key::Enter, Mods::NONE);
    assert!(run(&mut h, &mut t).submitted);
    h.key(Key::Escape, Mods::NONE);
    assert!(run(&mut h, &mut t).cancelled);
}

#[test]
fn long_text_scrolls_to_keep_the_caret_visible() {
    let mut h = Harness::new();
    let mut t = TextInput::new(&"x".repeat(200)); // 1200 DIP in a 288 DIP box
    h.ui.set_focus(Some(TI));
    h.frame(|ui, c| {
        text_input(ui, c, TI, TI_RECT, &mut t, "");
    });
    assert!(t.scroll_x > 1200.0 - 300.0, "scrolled to the end: {}", t.scroll_x);
    h.key(Key::Home, Mods::NONE);
    h.frame(|ui, c| {
        text_input(ui, c, TI, TI_RECT, &mut t, "");
    });
    assert_eq!(t.scroll_x, 0.0);
}

// ------------------------------------------------------------------ popups

const POP: Id = Id::new("menu");
const POP_RECT: Rect = Rect::new(100.0, 100.0, 200.0, 100.0);
const UNDER: Id = Id::new("under");
const ITEM: Id = Id::new("item");

/// A button under the popup's area, then the popup with one item.
fn popup_frame(h: &mut Harness, log: &mut Vec<Id>) -> Output {
    h.frame(|ui, c| {
        if button(ui, c, UNDER, Rect::new(100.0, 100.0, 300.0, 300.0), "under") {
            log.push(UNDER);
        }
        if ui.popup_begin(c, POP, POP_RECT) {
            if button(ui, c, ITEM, Rect::new(110.0, 110.0, 180.0, 24.0), "item") {
                log.push(ITEM);
            }
            ui.popup_end(c);
        }
    })
}

#[test]
fn popup_blocks_and_dismisses() {
    let mut h = Harness::new();
    let mut log = Vec::new();
    popup_frame(&mut h, &mut log);
    h.ui.set_focus(Some(UNDER));
    h.ui.open_popup(POP);
    h.ui.set_focus(Some(ITEM));
    popup_frame(&mut h, &mut log);
    assert!(h.ui.is_popup_open(POP));
    // a click on the panel (not the item) goes nowhere under it
    h.down(150.0, 180.0);
    h.up(150.0, 180.0);
    popup_frame(&mut h, &mut log);
    assert!(log.is_empty());
    assert!(h.ui.is_popup_open(POP));
    // the item works
    h.down(150.0, 120.0);
    h.up(150.0, 120.0);
    popup_frame(&mut h, &mut log);
    assert_eq!(log, vec![ITEM]);
    // a click outside closes it and is eaten (the button under it at 350,350
    // does not fire)
    assert!(h.down(350.0, 350.0));
    h.up(350.0, 350.0);
    popup_frame(&mut h, &mut log);
    assert!(!h.ui.is_popup_open(POP));
    assert_eq!(log, vec![ITEM]);
    assert_eq!(h.ui.focus(), Some(UNDER), "focus returns to where it was");
    // Escape closes the top popup only
    let sub = Id::new("sub");
    h.ui.open_popup(POP);
    h.ui.open_popup(sub);
    h.key(Key::Escape, Mods::NONE);
    popup_frame(&mut h, &mut log);
    assert!(h.ui.is_popup_open(POP) && !h.ui.is_popup_open(sub));
    h.key(Key::Escape, Mods::NONE);
    popup_frame(&mut h, &mut log);
    assert_eq!(h.ui.popups_open(), 0);
    // deactivating the window closes popups
    h.ui.open_popup(POP);
    h.push(Event::FocusLost);
    assert_eq!(h.ui.popups_open(), 0);
}

// ------------------------------------------------------- keyboard routing

const CMD_SELECT_ALL: u32 = 1;
const CMD_NEW_TAB: u32 = 2;
const CMD_FIND: u32 = 3;

#[test]
fn keyboard_routing_focus_first_then_shortcuts() {
    let mut h = Harness::new();
    h.ui.shortcuts.bind(Key::Char('A'), Mods::CTRL, CMD_SELECT_ALL, false);
    h.ui.shortcuts.bind(Key::Char('T'), Mods::CTRL, CMD_NEW_TAB, true);
    h.ui.shortcuts.bind(Key::Char('F'), Mods::CTRL, CMD_FIND, false);
    let mut t = TextInput::new("abc");
    let mut st = ListState::new(22.0);
    let mut rows = Rows::new(50);
    let draw = |h: &mut Harness, t: &mut TextInput, st: &mut ListState, rows: &mut Rows| {
        h.frame(|ui, c| {
            text_input(ui, c, TI, TI_RECT, t, "");
            list(ui, c, LIST, LIST_RECT, st, rows);
        });
    };
    draw(&mut h, &mut t, &mut st, &mut rows);
    // text focused: Ctrl+A is the field's, chars go in, Down is not its key
    h.ui.set_focus(Some(TI));
    h.key(Key::Char('A'), Mods::CTRL);
    h.typ("z");
    h.key(Key::Down, Mods::NONE);
    h.key(Key::Char('T'), Mods::CTRL);
    h.key(Key::Char('F'), Mods::CTRL);
    draw(&mut h, &mut t, &mut st, &mut rows);
    assert_eq!(t.text, "z");
    assert_eq!(h.ui.commands(), &[CMD_NEW_TAB, CMD_FIND], "global first, then unused keys");
    assert_eq!(h.ui.leftover(), &[Event::KeyDown { key: Key::Down, mods: Mods::NONE, repeat: false }]);
    assert_eq!(st.cursor, 0, "the list did not see Down");
    // list focused: Ctrl+A is the app's, chars are leftovers (type-ahead)
    h.ui.set_focus(Some(LIST));
    h.key(Key::Char('A'), Mods::CTRL);
    h.typ("q");
    h.key(Key::Down, Mods::NONE);
    draw(&mut h, &mut t, &mut st, &mut rows);
    assert_eq!(h.ui.commands(), &[CMD_SELECT_ALL]);
    assert_eq!(h.ui.leftover(), &[Event::Char('q')]);
    assert_eq!(st.cursor, 1);
    assert_eq!(t.text, "z");
}

// -------------------------------------------------- tabs and the splitter

#[test]
fn tabs_select_close_add_reorder() {
    let mut h = Harness::new();
    let mut strip = TabStrip::default();
    let labels = ["one", "two", "three"];
    let id = Id::new("tabs");
    let rect = Rect::new(0.0, 0.0, 400.0, 30.0); // 368 for tabs: 122.7 each
    let mut resp = TabResponse::default();
    let mut run = |h: &mut Harness, active: usize| {
        h.frame(|ui, c| resp = tab_strip(ui, c, id, rect, &mut strip, &labels, active));
        resp
    };
    run(&mut h, 0);
    let w = 368.0 / 3.0;
    h.down(w * 1.5 - 30.0, 15.0);
    let r = run(&mut h, 0);
    assert_eq!(r.select, Some(1));
    // drag right past the next tab's left edge: reorder 1 -> 2
    h.move_to(w * 2.0 + 5.0, 15.0);
    let r = run(&mut h, 1);
    assert_eq!(r.moved, Some((1, 2)));
    h.up(w * 2.0 + 5.0, 15.0);
    run(&mut h, 2);
    // the close box of the active tab
    let cx = w * 3.0 - 2.0 - 4.0 - 9.0;
    h.down(cx, 15.0);
    h.up(cx, 15.0);
    let r = run(&mut h, 2);
    assert_eq!(r.close, Some(2));
    assert_eq!(r.select, None, "a press on the close box does not select");
    // middle click closes, + adds
    h.push(Event::MouseDown { pos: Pt::new(10.0, 15.0), button: Button::Middle, mods: Mods::NONE, double: false });
    h.push(Event::MouseUp { pos: Pt::new(10.0, 15.0), button: Button::Middle, mods: Mods::NONE });
    assert_eq!(run(&mut h, 0).close, Some(0));
    h.down(385.0, 15.0);
    h.up(385.0, 15.0);
    assert!(run(&mut h, 0).add);
    // only visible tabs draw (overflow: 30 tabs at min width)
    let many: Vec<String> = (0..30).map(|i| format!("tab {i}")).collect();
    let refs: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
    let mut strip = TabStrip::default();
    h.frame(|ui, c| {
        tab_strip(ui, c, id, rect, &mut strip, &refs, 29);
    });
    let drawn: Vec<&str> = h.c.texts().into_iter().filter(|s| s.starts_with("tab")).collect();
    assert!(drawn.len() <= 6, "{drawn:?}");
    assert!(drawn.contains(&"tab 29"), "the active tab is scrolled into view");
    assert_eq!(h.c.measures, 0, "tabs measure nothing");
}

#[test]
fn splitter_drags_and_clamps() {
    let mut h = Harness::new();
    let id = Id::new("split");
    let area = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut pos = 200.0;
    let mut panes = (Rect::ZERO, Rect::ZERO);
    let mut run = |h: &mut Harness, pos: &mut f32| {
        h.frame(|ui, c| {
            let (a, b, _) = splitter(ui, c, id, area, Split::LeftRight, pos, 100.0, 150.0);
            panes = (a, b);
        });
        panes
    };
    run(&mut h, &mut pos);
    assert!(h.move_to(202.0, 300.0));
    run(&mut h, &mut pos);
    assert_eq!(h.ui.hot(), Some(id));
    h.down(202.0, 300.0);
    run(&mut h, &mut pos);
    h.move_to(402.0, 300.0);
    let (a, b) = run(&mut h, &mut pos);
    assert_eq!(pos, 400.0);
    assert_eq!(a.w, 400.0);
    assert_eq!(b.x, 405.0);
    h.move_to(5000.0, 300.0);
    run(&mut h, &mut pos);
    assert_eq!(pos, 800.0 - 5.0 - 150.0, "second pane keeps its minimum");
    h.up(5000.0, 300.0);
    run(&mut h, &mut pos);
    assert_eq!(h.ui.active(), None);
}

#[test]
fn column_header_click_sorts_and_grip_resizes() {
    let mut h = Harness::new();
    let id = Id::new("hdr");
    let rect = Rect::new(0.0, 0.0, 600.0, 24.0);
    let cols = [
        HeaderCol { label: "Name", right: false },
        HeaderCol { label: "Date", right: false },
        HeaderCol { label: "Size", right: true },
    ];
    let spans = [(0.0, 300.0), (300.0, 150.0), (450.0, 150.0)];
    let mut resp = HeaderResponse::default();
    let mut run = |h: &mut Harness| {
        h.frame(|ui, c| {
            resp = column_header(ui, c, id, rect, &cols, &spans, Some((1, true)));
        });
        resp
    };
    run(&mut h);
    // click on a cell sorts by it
    h.move_to(100.0, 12.0);
    run(&mut h);
    h.down(100.0, 12.0);
    run(&mut h);
    h.up(100.0, 12.0);
    let r = run(&mut h);
    assert_eq!(r.sort, Some(0));
    assert_eq!(r.resize, None);
    // drag the grip on the left edge of column 1 (x = 300)
    h.move_to(301.0, 12.0);
    run(&mut h);
    h.down(301.0, 12.0);
    run(&mut h);
    h.move_to(340.0, 12.0);
    let r = run(&mut h);
    assert_eq!(r.resize, Some((1, 340.0)));
    assert_eq!(r.sort, None);
    h.up(340.0, 12.0);
    let r = run(&mut h);
    assert_eq!(r.sort, None, "releasing a resize drag is not a sort click");
}
