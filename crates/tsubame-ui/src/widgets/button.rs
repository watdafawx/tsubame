//! Push button. Clicked on release over it, or Enter/Space while focused.

use crate::canvas::Canvas;
use crate::geom::Rect;
use crate::input::{Button, Event, Key};
use crate::ui::{Id, Sense, Ui};

/// A text button. Returns true when clicked. A click asks for one more frame
/// (the app usually changes state that widgets drawn earlier show).
pub fn button(ui: &mut Ui, c: &mut dyn Canvas, id: Id, rect: Rect, label: &str) -> bool {
    let clicked = button_behaviour(ui, id, rect);
    let t = &ui.theme;
    let col = if ui.is_active(id) {
        t.button_down
    } else if ui.hot() == Some(id) {
        t.button_hot
    } else {
        t.button
    };
    c.round(rect, t.radius, col);
    if ui.has_focus(id) && ui.window_focused() {
        c.round_stroke(rect, t.radius, t.input_focus, 1.0);
    }
    c.text(label, rect.inset(6.0, 0.0), t.font.c(), t.text);
    clicked
}

/// Input of a button without drawing (icon buttons, tab close boxes).
pub fn button_behaviour(ui: &mut Ui, id: Id, rect: Rect) -> bool {
    let it = ui.interact(id, rect, Sense::click().focusable());
    let mut clicked = it.clicked == Some(Button::Left);
    for (qi, ev) in ui.key_input(id) {
        if let Event::KeyDown { key: Key::Enter | Key::Space, mods, .. } = ev
            && mods.none()
        {
            ui.consume(qi);
            clicked = true;
        }
    }
    if clicked {
        ui.request_repaint();
    }
    clicked
}
