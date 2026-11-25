//! Tag model, no I/O: tag definitions (name, colour, shape), the eight
//! default tags of the approved mock (`mocks/mock-f6-tags.html`), the tag
//! colour tokens per theme and the tri-state logic of the picker.
//!
//! A file's tags are a `u64` mask of tag *slots* (0..64). Slots are stable:
//! renaming a tag keeps its slot, deleting one frees it (its bit is cleared
//! on every item first).

/// Most tags a store holds (one bit each in a `u64`).
pub const MAX_TAGS: usize = 64;
/// Longest tag name, in chars.
pub const MAX_NAME: usize = 40;

/// The eight tag colours. Index order is the mock's (`--tg-red` .. `--tg-gray`).
pub const COLOURS: [&str; 8] = ["red", "orange", "yellow", "green", "blue", "purple", "pink", "gray"];
/// Names shown in the colour chooser.
pub const COLOUR_NAMES: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Pink", "Gray"];

/// `--tg-*` dark-theme tokens (mock contrast table: >= 4.3:1 on every surface).
pub const DARK: [u32; 8] = [0xff7b72, 0xff9f45, 0xe6c84a, 0x5ccf7a, 0x6aa0ff, 0xb99bff, 0xf283bd, 0x9aa6ba];
/// `--tg-*` light-theme tokens (>= 3.3:1 on every surface).
pub const LIGHT: [u32; 8] = [0xc62d24, 0xb85a00, 0x8a6a00, 0x1c7a3a, 0x1b52bf, 0x6d3fd0, 0xb52a77, 0x586378];

/// The eight marks, so colour is never the only cue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Shape {
    Tri = 0,
    Dia = 1,
    Sq = 2,
    Cir = 3,
    Hex = 4,
    Star = 5,
    Plus = 6,
    Ring = 7,
}

impl Shape {
    pub const ALL: [Shape; 8] = [Shape::Tri, Shape::Dia, Shape::Sq, Shape::Cir, Shape::Hex, Shape::Star, Shape::Plus, Shape::Ring];

    pub fn from_u8(v: u8) -> Shape {
        Shape::ALL[(v as usize) % 8]
    }
    /// Short id used by the JSON export and the mock (`tri`, `dia` ...).
    pub fn id(self) -> &'static str {
        ["tri", "dia", "sq", "cir", "hex", "star", "plus", "ring"][self as usize]
    }
    /// Spoken / shown name ("Blue · hexagon").
    pub fn name(self) -> &'static str {
        ["triangle", "diamond", "square", "circle", "hexagon", "star", "plus", "ring"][self as usize]
    }
    pub fn parse(s: &str) -> Option<Shape> {
        Shape::ALL.into_iter().find(|sh| sh.id().eq_ignore_ascii_case(s) || sh.name().eq_ignore_ascii_case(s))
    }
}

/// One tag. `slot` is its bit in item masks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagDef {
    pub slot: u8,
    pub name: String,
    /// index into `COLOURS`
    pub colour: u8,
    pub shape: Shape,
}

impl TagDef {
    pub fn bit(&self) -> u64 {
        1u64 << self.slot
    }
    /// The tag's colour in a theme.
    pub fn rgb(&self, dark: bool) -> u32 {
        colour_rgb(self.colour, dark)
    }
}

pub fn colour_rgb(colour: u8, dark: bool) -> u32 {
    let i = (colour as usize) % 8;
    if dark { DARK[i] } else { LIGHT[i] }
}

pub fn parse_colour(s: &str) -> Option<u8> {
    let s = s.trim();
    COLOURS.iter().position(|c| c.eq_ignore_ascii_case(s)).or_else(|| if s.eq_ignore_ascii_case("grey") { Some(7) } else { None }).map(|i| i as u8)
}

/// The shipped set: 8 named tags with their mock colours and shapes.
pub fn defaults() -> Vec<TagDef> {
    const NAMES: [&str; 8] = ["Urgent", "Waiting", "To review", "Done", "Work", "Personal", "Invoice", "Archive"];
    NAMES.iter().enumerate().map(|(i, n)| TagDef { slot: i as u8, name: (*n).to_string(), colour: i as u8, shape: Shape::ALL[i] }).collect()
}

/// A tag name as stored: trimmed, control characters dropped, at most
/// `MAX_NAME` chars. `None` when nothing is left.
pub fn clean_name(s: &str) -> Option<String> {
    let t: String = s.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// The lowest slot not used by `defs`.
pub fn free_slot(defs: &[TagDef]) -> Option<u8> {
    let used = defs.iter().fold(0u64, |m, d| m | d.bit());
    (0..MAX_TAGS as u8).find(|s| used & (1u64 << s) == 0)
}

/// For a new tag: the colour (and its paired shape) used least so far, the
/// first in mock order on a tie.
pub fn suggest_look(defs: &[TagDef]) -> (u8, Shape) {
    let mut n = [0usize; 8];
    for d in defs {
        n[(d.colour as usize) % 8] += 1;
    }
    let c = (0..8).min_by_key(|&i| (n[i], i)).unwrap_or(0);
    (c as u8, Shape::ALL[c])
}

/// Checkbox state of a tag over a selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Off,
    Mixed,
    On,
}

/// How many of `masks` carry `bit`: none, some or all (an empty selection is Off).
pub fn check_of<'a>(masks: impl IntoIterator<Item = &'a u64>, bit: u64) -> Check {
    let (mut n, mut with) = (0usize, 0usize);
    for m in masks {
        n += 1;
        if m & bit != 0 {
            with += 1;
        }
    }
    match with {
        0 => Check::Off,
        w if w == n => Check::On,
        _ => Check::Mixed,
    }
}

/// What a toggle does: on for everyone unless everyone has it already.
pub fn toggled(c: Check) -> Check {
    if c == Check::On { Check::Off } else { Check::On }
}

/// Staged picker edits: bits to set and bits to clear on every item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    pub on: u64,
    pub off: u64,
}

impl Edit {
    pub fn is_empty(&self) -> bool {
        self.on == 0 && self.off == 0
    }
    /// Stages `bit` to state `c` (Mixed: leave each item as it is).
    pub fn stage(&mut self, bit: u64, c: Check) {
        self.on &= !bit;
        self.off &= !bit;
        match c {
            Check::On => self.on |= bit,
            Check::Off => self.off |= bit,
            Check::Mixed => {}
        }
    }
    /// The mask an item ends with.
    pub fn apply(&self, mask: u64) -> u64 {
        (mask | self.on) & !self.off
    }
    /// The state the picker shows for `bit` given the original state.
    pub fn shown(&self, bit: u64, orig: Check) -> Check {
        if self.on & bit != 0 {
            Check::On
        } else if self.off & bit != 0 {
            Check::Off
        } else {
            orig
        }
    }
}

/// Up to `max` tags of `mask` in `defs` order, plus how many more there are.
pub fn visible_tags(defs: &[TagDef], mask: u64, max: usize, out: &mut Vec<usize>) -> usize {
    out.clear();
    let mut more = 0;
    for (i, d) in defs.iter().enumerate() {
        if mask & d.bit() != 0 {
            if out.len() < max {
                out.push(i);
            } else {
                more += 1;
            }
        }
    }
    more
}

/// The screen-reader name of a row: "contract-draft-v3.docx, tags: Urgent, To review, Work".
/// A file row's UIA name: the file name and its tags (`uia_rows`).
pub fn a11y_name(name: &str, defs: &[TagDef], mask: u64) -> String {
    let tags: Vec<&str> = defs.iter().filter(|d| mask & d.bit() != 0).map(|d| d.name.as_str()).collect();
    if tags.is_empty() { name.to_string() } else { format!("{name}, tags: {}", tags.join(", ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: u32, b: u32) -> f64 {
        fn lum(c: u32) -> f64 {
            let ch = |s: u32| {
                let v = ((c >> s) & 0xff) as f64 / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
        }
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn defaults_are_the_mocks_eight_with_shapes() {
        let d = defaults();
        assert_eq!(d.len(), 8);
        assert_eq!(d.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Urgent", "Waiting", "To review", "Done", "Work", "Personal", "Invoice", "Archive"]);
        assert_eq!(d[0].shape, Shape::Tri);
        assert_eq!(d[4].shape, Shape::Hex);
        assert_eq!(d[7].shape, Shape::Ring);
        assert_eq!(d[4].rgb(true), 0x6aa0ff);
        assert_eq!(d[4].rgb(false), 0x1b52bf);
    }

    #[test]
    fn tag_colours_meet_three_to_one_on_every_surface() {
        // the token surfaces of both themes (row, zebra, hover, selected, panel, popup)
        let dark = [0x0e131b, 0x111721, 0x1a2231, 0x1f3c63, 0x121722, 0x1a2130];
        let light = [0xffffff, 0xf5f7fa, 0xe4e9f1, 0xc9dcf7, 0xeef0f4, 0xffffff];
        for c in DARK {
            for s in dark {
                assert!(contrast(c, s) >= 3.0, "{c:06x} on {s:06x}");
            }
        }
        for c in LIGHT {
            for s in light {
                assert!(contrast(c, s) >= 3.0, "{c:06x} on {s:06x}");
            }
        }
    }

    #[test]
    fn tri_state_and_toggle_follow_the_mock() {
        let bit = 1u64 << 2;
        assert_eq!(check_of(&[0, 0], bit), Check::Off);
        assert_eq!(check_of(&[bit, 0], bit), Check::Mixed);
        assert_eq!(check_of(&[bit, bit | 1], bit), Check::On);
        assert_eq!(check_of(&[], bit), Check::Off);
        assert_eq!(toggled(Check::Mixed), Check::On);
        assert_eq!(toggled(Check::Off), Check::On);
        assert_eq!(toggled(Check::On), Check::Off);
        let mut e = Edit::default();
        e.stage(bit, Check::On);
        assert_eq!(e.apply(0), bit);
        assert_eq!(e.shown(bit, Check::Mixed), Check::On);
        e.stage(bit, Check::Off);
        assert_eq!(e.apply(bit | 1), 1);
        e.stage(bit, Check::Mixed);
        assert!(e.is_empty());
        assert_eq!(e.apply(bit), bit, "mixed leaves items alone");
    }

    #[test]
    fn names_slots_and_looks() {
        assert_eq!(clean_name("  To\u{7} review  ").as_deref(), Some("To review"));
        assert_eq!(clean_name("   "), None);
        assert_eq!(clean_name(&"x".repeat(100)).unwrap().chars().count(), MAX_NAME);
        let mut d = defaults();
        assert_eq!(free_slot(&d), Some(8));
        d.remove(2);
        assert_eq!(free_slot(&d), Some(2));
        // all colours used once except yellow (removed): yellow is suggested
        assert_eq!(suggest_look(&d), (2, Shape::Sq));
        assert_eq!(parse_colour("Grey"), Some(7));
        assert_eq!(Shape::parse("hexagon"), Some(Shape::Hex));
        assert_eq!(Shape::parse("star"), Some(Shape::Star));
        let mut v = Vec::new();
        let mask = d.iter().fold(0, |m, t| m | t.bit());
        assert_eq!(visible_tags(&d, mask, 3, &mut v), 4);
        assert_eq!(v, [0, 1, 2]);
        assert_eq!(a11y_name("a.txt", &defaults(), 0b10001), "a.txt, tags: Urgent, Work");
        assert_eq!(a11y_name("a.txt", &defaults(), 0), "a.txt");
    }
}
