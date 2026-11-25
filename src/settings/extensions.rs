//! Settings > Extensions: one card per switchable built-in feature.

use super::Cx;
use super::age_card;
use super::lay::Pg;

#[derive(Default)]
pub struct State {
    pub age: age_card::State,
}

pub fn paint(pg: &mut Pg, st: &mut State, cx: &mut Cx) {
    pg.heading("Extensions", "Optional features that ship with Tsubame. Switching one off removes its buttons, commands and settings.");
    age_card::paint(pg, &mut st.age, cx);
}
