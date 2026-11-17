//! Immediate-mode UI kit over `Canvas`: layout, virtual list, scrollbar,
//! text input, buttons, tabs, splitters, popups, focus and keyboard routing.
//!
//! Portable: no platform crate here. The platform layer turns its messages
//! into `Event`s (`Ui::push`) and implements `Canvas` (Direct2D in
//! `tsubame-d2d`). See `docs/ui-kit.md`.

pub mod canvas;
pub mod geom;
pub mod input;
pub mod testcanvas;
pub mod theme;
mod theme_generated;
pub mod ui;
pub mod widgets;

pub use canvas::{Align, Canvas, Face, Font, Weight, f};
pub use geom::{Anchor, Color, Len, Pt, Rect};
pub use input::{Button, Cursor, Event, Key, Mods};
pub use testcanvas::TestCanvas;
pub use theme::{DARK, Density, LIGHT, Theme};
pub use ui::{Cells, Clipboard, Id, Interaction, MemClipboard, Output, Press, Sense, Shortcut, Shortcuts, Ui};
pub use widgets::button::{button, button_behaviour};
pub use widgets::list::{ListResponse, ListSource, ListState, RowState, SelectBy, list};
pub use widgets::scrollbar::{ScrollMetrics, vscrollbar};
pub use widgets::splitter::{Split, splitter};
pub use widgets::tabs::{TabResponse, TabStrip, tab_strip};
pub use widgets::text_input::{TextInput, TextResponse, text_input};

#[cfg(test)]
mod tests;
