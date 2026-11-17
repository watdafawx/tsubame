//! Input events. Plain data; the platform layer translates its messages
//! (WM_MOUSEMOVE, WM_KEYDOWN, WM_CHAR...) into these. Positions are DIPs.

use crate::geom::Pt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };
    pub const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
    pub const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };
    pub const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false };
    pub const ALT: Mods = Mods { ctrl: false, shift: false, alt: true };
    pub fn none(&self) -> bool {
        !self.ctrl && !self.shift && !self.alt
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// Keys the kit and shortcut tables care about. Letters and digits are
/// `Char` with the uppercase ASCII letter (`Char('A')`), as the platform's
/// virtual key; typed text comes as `Event::Char`, never from these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Space,
    /// F1..F24
    F(u8),
    /// A-Z, 0-9 (and other printable virtual keys), uppercase
    Char(char),
    /// a platform key code the kit has no name for
    Other(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    MouseMove { pos: Pt },
    /// `double`: the platform reported a double click (WM_LBUTTONDBLCLK)
    MouseDown { pos: Pt, button: Button, mods: Mods, double: bool },
    MouseUp { pos: Pt, button: Button, mods: Mods },
    /// Wheel in notches: +1.0 = one notch away from the user (scroll up).
    /// Fractions come from precision touchpads.
    Wheel { pos: Pt, dy: f32, dx: f32, mods: Mods },
    /// the pointer left the window
    MouseLeave,
    KeyDown { key: Key, mods: Mods, repeat: bool },
    KeyUp { key: Key, mods: Mods },
    /// Committed text, one char at a time, IME-agnostic (WM_CHAR after the
    /// IME commits). Control chars (< 0x20, 0x7f) are ignored by text input.
    Char(char),
    /// the window lost the keyboard focus (deactivated): drags end, buttons
    /// count as released, carets stop drawing
    FocusLost,
    FocusGained,
}

/// Mouse cursor the kit wants (the platform maps it; WM_SETCURSOR).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Arrow,
    IBeam,
    Hand,
    /// resize left-right
    SizeWE,
    /// resize up-down
    SizeNS,
}
