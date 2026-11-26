//! The conflict dialog (B11-E2, `mocks/progress-panel.html`): "This folder
//! already has a file with this name". Own-drawn, modal over the window with
//! no scrim. It replaces the Yes/No/Cancel message box of the copy queue and
//! the archive extractor.
//!
//! - Keys: R Replace, K Keep both, S Skip, Esc Cancel operation, Enter the
//!   focused (else the suggested) button, A or Space on the box toggles "Do this
//!   for all remaining conflicts", Tab / Shift+Tab move the focus.
//! - Suggested: Replace when the incoming file is newer, else Skip (also when
//!   both look identical, same size and date, or a side is unreadable).
//! - Cancel operation stops the whole job (`Choice::Cancel`); Skip is the
//!   per-file choice. For an extraction Cancel sets its cancel flag and skips.
//! - Nothing here owns a timer; the dialog is painted only when something
//!   changes (input), and the queue's worker simply waits for the reply.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};
use tsubame_core::archive::{ConflictAnswer, ConflictChoice};
use tsubame_ui::{Button, Canvas, Event, Id, Key as K, Rect, Sense, Ui, checkbox, f};
use tsubame_win::fileops::{Choice, ConflictReply, FileMeta};

use crate::ops_panel::{self, Key};

const ID_DLG: Id = Id::new("conflict-dlg");
const ID_ALL: Id = Id::new("conflict-all");
const ID_BTN: Id = Id::new("conflict-btn");

/// What the user picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Replace,
    KeepBoth,
    Skip,
    /// stop the whole operation
    Cancel,
}

/// Where the answer goes.
pub enum Reply {
    Queue(ConflictReply),
    /// the extract thread waits on this; `cancel` is the extraction's flag
    Extract(Sender<ConflictAnswer>, Option<Arc<AtomicBool>>),
}

/// The queue's choice for `a`.
pub fn queue_choice(a: Answer) -> Choice {
    match a {
        Answer::Replace => Choice::Replace,
        Answer::KeepBoth => Choice::KeepBoth,
        Answer::Skip => Choice::Skip,
        Answer::Cancel => Choice::Cancel,
    }
}

/// The extractor's choice for `a`, and whether the extraction stops.
pub fn extract_choice(a: Answer) -> (ConflictChoice, bool) {
    match a {
        Answer::Replace => (ConflictChoice::Replace, false),
        Answer::KeepBoth => (ConflictChoice::KeepBoth, false),
        Answer::Skip => (ConflictChoice::Skip, false),
        Answer::Cancel => (ConflictChoice::Skip, true),
    }
}

/// One open question.
pub struct Dialog {
    pub key: Key,
    reply: Option<Reply>,
    pub source: PathBuf,
    pub target: PathBuf,
    pub existing: FileMeta,
    pub incoming: FileMeta,
    /// "Copying 14 items to Projects"
    pub verb: &'static str,
    pub count: usize,
    pub from: String,
    pub to: String,
    /// "Do this for all remaining conflicts"
    pub all: bool,
    /// focused control: 0 Cancel operation, 1 the check box, 2 Skip, 3 Keep both, 4 Replace
    focus: usize,
    fresh: bool,
}

const F_CANCEL: usize = 0;
const F_ALL: usize = 1;
const F_SKIP: usize = 2;
const F_KEEP: usize = 3;
const F_REPLACE: usize = 4;

fn secs(t: Option<SystemTime>) -> Option<u64> {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs())
}

/// Same size and the same modified time to the second: they look the same.
pub fn identical(a: &FileMeta, b: &FileMeta) -> bool {
    a.known && b.known && !a.dir && !b.dir && a.size == b.size && secs(a.modified).is_some() && secs(a.modified) == secs(b.modified)
}

/// The incoming side is newer (by whole seconds).
pub fn incoming_newer(existing: &FileMeta, incoming: &FileMeta) -> bool {
    matches!((secs(existing.modified), secs(incoming.modified)), (Some(e), Some(i)) if existing.known && incoming.known && i > e)
}

/// The default button: Replace when the incoming file is newer, else Skip.
pub fn suggested(existing: &FileMeta, incoming: &FileMeta) -> Answer {
    if !identical(existing, incoming) && incoming_newer(existing, incoming) { Answer::Replace } else { Answer::Skip }
}

impl Dialog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(key: Key, reply: Reply, source: PathBuf, target: PathBuf, existing: FileMeta, incoming: FileMeta, verb: &'static str, count: usize, from: String, to: String) -> Dialog {
        let focus = if suggested(&existing, &incoming) == Answer::Replace { F_REPLACE } else { F_SKIP };
        Dialog { key, reply: Some(reply), source, target, existing, incoming, verb, count, from, to, all: false, focus, fresh: true }
    }

    pub fn suggested(&self) -> Answer {
        suggested(&self.existing, &self.incoming)
    }

    /// Sends `a` to whoever waits. Consumes the reply (a second call does nothing).
    pub fn answer(&mut self, a: Answer) {
        match self.reply.take() {
            Some(Reply::Queue(r)) => r.answer(queue_choice(a), self.all || a == Answer::Cancel),
            Some(Reply::Extract(tx, cancel)) => {
                let (c, stop) = extract_choice(a);
                if stop && let Some(flag) = cancel {
                    flag.store(true, Ordering::Release);
                }
                let _ = tx.send((c, self.all || stop));
            }
            None => {}
        }
    }

    /// The answer for a key press (None: not one of the dialog's keys).
    fn key_answer(&mut self, key: K, mods: tsubame_ui::Mods) -> Option<Option<Answer>> {
        if mods.ctrl || mods.alt {
            return None;
        }
        let shift = mods.shift;
        Some(match key {
            K::Char('R') if !shift => Some(Answer::Replace),
            K::Char('K') if !shift => Some(Answer::KeepBoth),
            K::Char('S') if !shift => Some(Answer::Skip),
            K::Escape => Some(Answer::Cancel),
            K::Char('A') if !shift => {
                self.all = !self.all;
                None
            }
            K::Enter => Some(match self.focus {
                F_CANCEL => Answer::Cancel,
                F_SKIP => Answer::Skip,
                F_KEEP => Answer::KeepBoth,
                F_REPLACE => Answer::Replace,
                _ => self.suggested(),
            }),
            K::Space => {
                match self.focus {
                    F_ALL => self.all = !self.all,
                    F_CANCEL => return Some(Some(Answer::Cancel)),
                    F_SKIP => return Some(Some(Answer::Skip)),
                    F_KEEP => return Some(Some(Answer::KeepBoth)),
                    _ => return Some(Some(Answer::Replace)),
                }
                None
            }
            K::Tab => {
                self.focus = if shift { (self.focus + 4) % 5 } else { (self.focus + 1) % 5 };
                None
            }
            K::Left | K::Up if self.focus > F_SKIP => {
                self.focus -= 1;
                None
            }
            K::Right | K::Down if (F_SKIP..F_REPLACE).contains(&self.focus) => {
                self.focus += 1;
                None
            }
            _ => return None,
        })
    }

    fn title(&self) -> &'static str {
        if identical(&self.existing, &self.incoming) { "This file is already here" } else { "This folder already has a file with this name" }
    }
}

fn plural(n: usize) -> &'static str {
    crate::ops::plural(n)
}

/// "Replace" / "Replace all" with the key letter (its index in the label).
fn label(a: Answer, all: bool) -> (&'static str, usize) {
    match (a, all) {
        (Answer::Replace, false) => ("Replace", 0),
        (Answer::Replace, true) => ("Replace all", 0),
        (Answer::KeepBoth, false) => ("Keep both", 0),
        (Answer::KeepBoth, true) => ("Keep both, all", 0),
        (Answer::Skip, false) => ("Skip", 0),
        (Answer::Skip, true) => ("Skip all", 0),
        (Answer::Cancel, _) => ("Cancel operation", usize::MAX),
    }
}

const W: f32 = 540.0;

/// Paints the dialog centred in `area` (modal: everything under it is blocked).
/// Returns the user's answer; the caller calls `answer` and closes it.
pub fn paint(d: &mut Dialog, ui: &mut Ui, g: &mut dyn Canvas, area: Rect, window: Rect) -> Option<Answer> {
    let t = ui.theme;
    if d.fresh {
        d.fresh = false;
        ui.set_focus(Some(ID_DLG));
    }
    // every key goes to the dialog (shortcuts skip them, Tab is ours)
    ui.capture_keys = true;
    if ui.focus() != Some(ID_DLG) {
        ui.set_focus(Some(ID_DLG));
    }
    // the whole window is blocked; the dialog's widgets are added after, so they win
    let _ = ui.interact(ID_DLG.child("block"), window, Sense::click().wheel());
    let mut out = None;
    for (qi, ev) in ui.key_input(ID_DLG) {
        match ev {
            Event::KeyDown { key, mods, .. } => {
                if let Some(a) = d.key_answer(key, mods) {
                    ui.consume(qi);
                    ui.request_repaint();
                    if a.is_some() {
                        out = a;
                    }
                }
            }
            Event::Char(_) => ui.consume(qi),
            _ => {}
        }
    }

    let w = W.min(area.w - 32.0).max(320.0);
    let card_h = 96.0;
    let h = 14.0 + 22.0 + 4.0 + 18.0 + 12.0 + card_h + 10.0 + 18.0 + 6.0 + 22.0 + 10.0 + 1.0 + 12.0 + 28.0 + 12.0;
    let r = Rect::new((area.x + (area.w - w) / 2.0).round(), (area.y + (area.h - h) / 2.0).round().max(area.y + 8.0), w, h);
    let _ = ui.interact(ID_DLG, r, Sense::click().focusable());
    g.round(r, t.radius_lg, t.popup);
    g.round_stroke(r, t.radius_lg, t.popup_border, 1.0);
    let x = r.x + 16.0;
    let iw = r.w - 32.0;
    let mut y = r.y + 14.0;
    g.text(d.title(), Rect::new(x, y, iw, 22.0), f(15.0).b(), t.text);
    y += 22.0 + 4.0;
    let mut s = String::new();
    let sub = t.font_size_sm;
    if identical(&d.existing, &d.incoming) {
        ops_panel::rich_line(g, &t, x, y, iw, 18.0, &[("The two files look identical (same size and date).", false)], sub);
    } else {
        use std::fmt::Write;
        let _ = write!(s, "{} {} {} to ", d.verb, d.count, plural(d.count));
        ops_panel::rich_line(g, &t, x, y, iw, 18.0, &[(&s, false), (&d.to, true), (". Choose which one to keep.", false)], sub);
    }
    y += 18.0 + 12.0;

    // the two cards
    let gap = 10.0;
    let cw = (iw - gap) / 2.0;
    let newer_in = incoming_newer(&d.existing, &d.incoming);
    let newer_ex = matches!((secs(d.existing.modified), secs(d.incoming.modified)), (Some(e), Some(i)) if d.existing.known && d.incoming.known && e > i);
    let same = identical(&d.existing, &d.incoming);
    let name = d.target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let in_name = d.source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
    side(g, &t, Rect::new(x, y, cw, card_h), ("Existing", ", in "), &d.to, &name, &d.existing, &d.incoming, newer_ex, same);
    side(g, &t, Rect::new(x + cw + gap, y, cw, card_h), ("Incoming", ", from "), &d.from, &in_name, &d.incoming, &d.existing, newer_in, same);
    y += card_h + 10.0;

    // hint
    g.text("\u{E74D}", Rect::new(x, y, 16.0, 18.0), f(11.0).icon().c(), t.text_dim);
    g.text("Replace sends the existing file to the Recycle Bin, so it can be undone.", Rect::new(x + 22.0, y, iw - 22.0, 18.0), t.font().left(), t.text_dim);
    y += 18.0 + 6.0;

    // "Do this for all remaining conflicts"
    let cb = Rect::new(x, y + 3.0, 16.0, 16.0);
    let lw = g.measure("Do this for all remaining conflicts", t.font());
    let row = Rect::new(x, y, 22.0 + lw + 4.0, 22.0);
    let hit = ui.interact(ID_ALL.child("row"), row, Sense::click());
    if checkbox(ui, g, ID_ALL, cb, d.all) || hit.clicked == Some(Button::Left) {
        d.all = !d.all;
        d.focus = F_ALL;
        ui.set_focus(Some(ID_DLG));
        ui.request_repaint();
    }
    if d.focus == F_ALL && ui.window_focused() {
        g.round_stroke(cb.pad(-2.0), t.radius_sm + 1.0, t.focus, 1.0);
    }
    g.text("Do this for all remaining conflicts", Rect::new(x + 22.0, y, lw + 2.0, 22.0), t.font().left(), t.text);
    y += 22.0 + 10.0;
    g.fill(Rect::new(r.x, y, r.w, 1.0), t.border);
    y += 1.0 + 12.0;

    // buttons
    let sug = d.suggested();
    let mut rects: Vec<(u64, String, Rect)> = Vec::new();
    let (cl, _) = label(Answer::Cancel, d.all);
    let cwid = ops_panel::text_button_w(g, &t, cl, false, false) + 8.0;
    let cr = Rect::new(x, y, cwid, 28.0);
    if button(ui, g, &t, ID_BTN.with(0), cr, cl, usize::MAX, false, d.focus == F_CANCEL) {
        out = Some(Answer::Cancel);
    }
    rects.push((0, cl.into(), cr));
    let mut bx = r.r() - 16.0;
    for (n, a, fcs) in [(3u64, Answer::Replace, F_REPLACE), (2, Answer::KeepBoth, F_KEEP), (1, Answer::Skip, F_SKIP)] {
        let (l, u) = label(a, d.all);
        let primary = a == sug;
        let bw = ops_panel::text_button_w(g, &t, l, false, primary) + 8.0;
        bx -= bw;
        let br = Rect::new(bx, y, bw, 28.0);
        if button(ui, g, &t, ID_BTN.with(n), br, l, u, primary, d.focus == fcs) {
            out = Some(a);
        }
        rects.push((n, l.into(), br));
        bx -= 8.0;
    }
    if crate::uia_rows::on() {
        rects.push((4, format!("Do this for all remaining conflicts: {}", if d.all { "on" } else { "off" }), row));
        crate::uia_rows::buttons(crate::uia_rows::PART_CONFLICT, d.title(), &format!("{name} already exists in {}", d.to), r, rects);
    }
    out
}

/// A dialog button: 28 px, the key letter `u` underlined, a ring when focused.
#[allow(clippy::too_many_arguments)]
fn button(ui: &mut Ui, g: &mut dyn Canvas, t: &tsubame_ui::Theme, id: Id, r: Rect, label: &str, u: usize, primary: bool, focused: bool) -> bool {
    let it = ui.interact(id, r, Sense::click());
    let (fill, fg) = if primary { (Some(t.accent), t.on_accent) } else if it.hovered { (Some(t.hover), t.text) } else { (None, t.text) };
    if let Some(c) = fill {
        g.round(r, t.radius_md, c);
    }
    if !primary {
        g.round_stroke(r, t.radius_md, t.popup_border, 1.0);
    }
    if focused && ui.window_focused() {
        g.round_stroke(r.pad(-2.0), t.radius_md, if primary { t.on_accent } else { t.focus }, 1.0);
    }
    let font = if primary { t.font_bold() } else { t.font() };
    let lw = g.measure(label, font);
    let lx = (r.x + (r.w - lw) / 2.0).round();
    g.text(label, Rect::new(lx, r.y, lw + 2.0, r.h), font.left(), fg);
    if u != usize::MAX
        && let Some(ch) = label.get(u..u + 1)
    {
        let ox = g.measure(&label[..u], font);
        let cw = g.measure(ch, font);
        g.fill(Rect::new(lx + ox, r.y + r.h / 2.0 + 7.0, cw, 1.0), fg);
    }
    let clicked = it.clicked == Some(Button::Left);
    if clicked {
        ui.request_repaint();
    }
    clicked
}

/// One side: "Existing, in B" / "Incoming, from A", the icon, name, date, size.
#[allow(clippy::too_many_arguments)]
fn side(g: &mut dyn Canvas, t: &tsubame_ui::Theme, r: Rect, (lead, prep): (&str, &str), folder: &str, name: &str, m: &FileMeta, other: &FileMeta, newer: bool, same: bool) {
    g.round(r, t.radius_md, if newer { t.selected } else { t.field });
    g.round_stroke(r, t.radius_md, if newer { t.accent } else { t.border }, 1.0);
    let x = r.x + 10.0;
    let w = r.w - 20.0;
    let mut y = r.y + 10.0;
    // badge on the right of the label line
    let badge = if newer { Some("Newer") } else if same { Some("Same date") } else { None };
    let mut lw = w;
    if let Some(b) = badge {
        let bf = t.font_small();
        let bw = g.measure(b, bf) + 14.0;
        let br = Rect::new(r.r() - 10.0 - bw, y - 1.0, bw, 18.0);
        if newer {
            g.round(br, 9.0, t.accent);
            g.text(b, br, bf.b().c(), t.on_accent);
        } else {
            g.round_stroke(br, 9.0, t.popup_border, 1.0);
            g.text(b, br, bf.c(), t.text_dim);
        }
        lw -= bw + 6.0;
    }
    if folder.is_empty() {
        g.text(lead, Rect::new(x, y, lw, 16.0), t.font_small().left(), t.text_dim);
    } else {
        ops_panel::rich_line(g, t, x, y, lw, 16.0, &[(lead, false), (prep, false), (folder, true)], t.font_size_xs);
    }
    y += 16.0 + 8.0;
    // a 36x44 document glyph
    let icon = Rect::new(x, y, 36.0, 44.0);
    g.text(if m.dir { "\u{E8B7}" } else { "\u{E7C3}" }, icon, f(30.0).icon().c(), if m.dir { t.folder } else { t.text_dim });
    let tx = x + 36.0 + 10.0;
    let tw = (w - 46.0).max(0.0);
    g.text(name, Rect::new(tx, y, tw, 18.0), f(13.0).b().left(), t.text);
    let mut s = String::new();
    match secs(m.modified) {
        Some(v) if m.known => crate::data::fmt_date(v.min(u32::MAX as u64) as u32, &mut s),
        _ => s.push_str(if m.known { "" } else { "Date not known" }),
    }
    g.text(&s, Rect::new(tx, y + 18.0, tw, 16.0), if newer { t.font_small().b().left() } else { t.font_small().left() }, if newer { t.text } else { t.text_dim });
    s.clear();
    if m.known && !m.dir {
        crate::data::fmt_size(m.size, &mut s);
        if other.known && !other.dir && other.size != m.size {
            s.push_str(if m.size > other.size { " (larger)" } else { " (smaller)" });
        }
    } else if m.dir {
        s.push_str("Folder");
    }
    g.text(&s, Rect::new(tx, y + 34.0, tw, 16.0), t.font_small().left(), t.text_dim);
}

/// Screen reader Invoke on item `k` of the dialog surface.
pub fn a11y_answer(d: &mut Dialog, k: u64) -> Option<Answer> {
    match k {
        0 => Some(Answer::Cancel),
        1 => Some(Answer::Skip),
        2 => Some(Answer::KeepBoth),
        3 => Some(Answer::Replace),
        4 => {
            d.all = !d.all;
            None
        }
        _ => None,
    }
}

/// The target's name, for the status bar while the dialog waits.
pub fn target_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn meta(size: u64, secs: u64) -> FileMeta {
        FileMeta { size, modified: Some(UNIX_EPOCH + Duration::from_secs(secs)), dir: false, known: true }
    }

    fn extract_dialog(existing: FileMeta, incoming: FileMeta) -> (Dialog, mpsc::Receiver<ConflictAnswer>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel();
        let flag = Arc::new(AtomicBool::new(false));
        let d = Dialog::new(Key::Extract(1), Reply::Extract(tx, Some(flag.clone())), "a.txt".into(), "C:\\b\\a.txt".into(), existing, incoming, "Extracting", 3, "x.zip".into(), "b".into());
        (d, rx, flag)
    }

    #[test]
    fn suggests_replace_only_when_incoming_is_newer() {
        assert_eq!(suggested(&meta(10, 100), &meta(12, 200)), Answer::Replace);
        assert_eq!(suggested(&meta(10, 200), &meta(12, 100)), Answer::Skip);
        // identical: same size and second
        assert!(identical(&meta(10, 100), &meta(10, 100)));
        assert_eq!(suggested(&meta(10, 100), &meta(10, 100)), Answer::Skip);
        // an unreadable side: Skip
        assert_eq!(suggested(&FileMeta::default(), &meta(1, 500)), Answer::Skip);
        assert!(!identical(&FileMeta::default(), &FileMeta::default()));
    }

    #[test]
    fn answers_map_to_the_queue_and_the_extractor() {
        assert_eq!(queue_choice(Answer::Replace), Choice::Replace);
        assert_eq!(queue_choice(Answer::KeepBoth), Choice::KeepBoth);
        assert_eq!(queue_choice(Answer::Skip), Choice::Skip);
        assert_eq!(queue_choice(Answer::Cancel), Choice::Cancel);
        assert_eq!(extract_choice(Answer::Cancel), (ConflictChoice::Skip, true));
        assert_eq!(extract_choice(Answer::KeepBoth), (ConflictChoice::KeepBoth, false));
    }

    #[test]
    fn extract_cancel_sets_the_flag_and_all_applies() {
        let (mut d, rx, flag) = extract_dialog(meta(1, 1), meta(1, 2));
        d.answer(Answer::Cancel);
        assert_eq!(rx.recv().unwrap(), (ConflictChoice::Skip, true));
        assert!(flag.load(Ordering::Acquire));
        d.answer(Answer::Skip); // already answered: nothing more is sent
        assert!(rx.try_recv().is_err());
        let (mut d, rx, flag) = extract_dialog(meta(1, 1), meta(1, 2));
        d.all = true;
        d.answer(Answer::Replace);
        assert_eq!(rx.recv().unwrap(), (ConflictChoice::Replace, true));
        assert!(!flag.load(Ordering::Acquire));
    }

    #[test]
    fn keys_pick_answers_and_enter_takes_the_focused_button() {
        let (mut d, _rx, _f) = extract_dialog(meta(1, 1), meta(1, 2));
        let none = tsubame_ui::Mods::NONE;
        assert_eq!(d.focus, F_REPLACE, "the suggested button has the focus");
        assert_eq!(d.key_answer(K::Enter, none), Some(Some(Answer::Replace)));
        assert_eq!(d.key_answer(K::Char('S'), none), Some(Some(Answer::Skip)));
        assert_eq!(d.key_answer(K::Char('K'), none), Some(Some(Answer::KeepBoth)));
        assert_eq!(d.key_answer(K::Escape, none), Some(Some(Answer::Cancel)));
        assert_eq!(d.key_answer(K::Char('R'), tsubame_ui::Mods::CTRL), None, "Ctrl+R is not ours");
        assert_eq!(d.key_answer(K::Char('A'), none), Some(None));
        assert!(d.all);
        assert_eq!(d.key_answer(K::Left, none), Some(None));
        assert_eq!(d.key_answer(K::Enter, none), Some(Some(Answer::KeepBoth)));
        d.key_answer(K::Tab, none);
        d.key_answer(K::Tab, none);
        assert_eq!(d.focus, F_CANCEL);
        assert_eq!(d.key_answer(K::Space, none), Some(Some(Answer::Cancel)));
        d.key_answer(K::Tab, none);
        assert_eq!(d.focus, F_ALL);
        assert_eq!(d.key_answer(K::Space, none), Some(None));
        assert!(!d.all);
        assert_eq!(label(Answer::Skip, true).0, "Skip all");
        assert_eq!(label(Answer::KeepBoth, true).0, "Keep both, all");
    }

    #[test]
    fn identical_files_say_so_and_suggest_skip() {
        let (d, _rx, _f) = extract_dialog(meta(5, 9), meta(5, 9));
        assert_eq!(d.title(), "This file is already here");
        assert_eq!(d.focus, F_SKIP);
        let (d, _rx, _f) = extract_dialog(meta(5, 9), meta(6, 8));
        assert_eq!(d.title(), "This folder already has a file with this name");
    }


    #[test]
    fn paints_the_mock_copy_and_all_relabels_the_buttons() {
        let (mut d, _rx, _f) = extract_dialog(meta(10, 100), meta(12, 200));
        d.from = "Downloads".into();
        let mut ui = Ui::new(tsubame_ui::DARK);
        let mut g = tsubame_ui::TestCanvas::new(1200.0, 800.0);
        ui.begin_frame((1200.0, 800.0));
        let w = Rect::new(0.0, 0.0, 1200.0, 800.0);
        assert!(paint(&mut d, &mut ui, &mut g, w, w).is_none());
        let _ = ui.end_frame();
        assert!(ui.capture_keys, "the dialog takes every key");
        for want in ["This folder already has a file with this name", "Replace", "Keep both", "Skip", "Cancel operation", "Newer", "Do this for all remaining conflicts"] {
            assert!(g.has_text(want), "missing {want:?} in {:?}", g.texts());
        }
        assert!(g.texts().iter().any(|s| s.contains("(larger)")));
        d.all = true;
        let mut g = tsubame_ui::TestCanvas::new(1200.0, 800.0);
        ui.begin_frame((1200.0, 800.0));
        paint(&mut d, &mut ui, &mut g, w, w);
        let _ = ui.end_frame();
        assert!(g.has_text("Replace all") && g.has_text("Skip all") && g.has_text("Keep both, all"));
    }
}
