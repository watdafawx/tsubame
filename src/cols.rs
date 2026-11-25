//! Details-view columns, no UI. The columns themselves are extensions
//! (`BUILTIN`, registered by `crate::ext`): the registry says which show and in
//! which order (`ColSet`). Name takes what the others leave; Modified, Type and
//! Size have a width each that the user can drag.

use tsubame_ext_api::{Align, ColumnSource, ColumnSpec, Text, text};

pub const MIN_W: f32 = 48.0;
pub const MIN_NAME_W: f32 = 140.0;

/// Number of host-drawn columns in `Col`.
pub const NUM_COLS: usize = 6;

/// A column the view draws itself from the entry store (the registry's
/// `Host::Field` for the app).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    Name,
    Modified,
    Type,
    Size,
    Git,
    /// P4.F6 tag chips (`tsubame.column.tags`, shown when `[tags] column`)
    Tags,
}

impl Col {
    /// Index into `Spans` and the per-column arrays.
    pub const fn slot(self) -> usize {
        match self {
            Col::Name => 0,
            Col::Modified => 1,
            Col::Type => 2,
            Col::Size => 3,
            Col::Git => 4,
            Col::Tags => 5,
        }
    }
    /// The matching tsubame-core sort column (`None` for non-sortable columns like Git).
    pub fn sort_column(self) -> Option<tsubame_core::Column> {
        match self {
            Col::Name => Some(tsubame_core::Column::Name),
            Col::Modified => Some(tsubame_core::Column::Modified),
            Col::Type => Some(tsubame_core::Column::Type),
            Col::Size => Some(tsubame_core::Column::Size),
            Col::Git | Col::Tags => None,
        }
    }
}

/// A built-in column and the extension that registers it.
pub struct Builtin {
    pub col: Col,
    /// extension id (the config's `[extensions] disabled` uses it)
    pub ext: &'static str,
    pub title: &'static str,
    /// Name can not be switched off
    pub required: bool,
    pub order: i32,
    pub spec: ColumnSpec,
}

const fn spec(id: &'static str, label: &'static str, align: Align, width: f32, flex: bool) -> ColumnSpec {
    ColumnSpec { id: text(id), label: text(label), align, width, flex }
}

/// The built-in columns. Widths are the mock's: date 150, type 130, size 90, git 56.
pub static BUILTIN: &[Builtin] = &[
    Builtin { col: Col::Name, ext: crate::ext::CORE, title: "Tsubame core", required: true, order: 0, spec: spec("core.name", "Name", Align::Left, 0.0, true) },
    Builtin { col: Col::Modified, ext: "tsubame.column.modified", title: "Date modified column", required: false, order: 10, spec: spec("core.modified", "Date modified", Align::Left, 150.0, false) },
    Builtin { col: Col::Type, ext: "tsubame.column.type", title: "Type column", required: false, order: 20, spec: spec("core.type", "Type", Align::Left, 130.0, false) },
    Builtin { col: Col::Size, ext: "tsubame.column.size", title: "Size column", required: false, order: 30, spec: spec("core.size", "Size", Align::Right, 90.0, false) },
    Builtin { col: Col::Git, ext: "tsubame.column.git", title: "Git status column", required: false, order: 40, spec: spec("core.git", "Git", Align::Left, 56.0, false) },
    Builtin { col: Col::Tags, ext: crate::tags::EXT_COLUMN, title: "Tags column", required: false, order: 5, spec: spec("core.tags", "Tags", Align::Left, 168.0, false) },
];

impl Builtin {
    pub fn source(&self) -> ColumnSource<crate::ext::App> {
        ColumnSource::Field(self.col)
    }
}

/// Which columns show and in which order, with their labels: what the view
/// needs each frame, copied from the registry when its generation changes
/// (so painting reads no registry and allocates nothing).
#[derive(Clone, Debug)]
pub struct ColSet {
    pub generation: u64,
    /// `crate::tags::column_shown()` when built (the Tags column is opt-in)
    pub tags: bool,
    order: [Col; NUM_COLS],
    n: usize,
    /// by `Col::slot`
    label: [Text; NUM_COLS],
    right: [bool; NUM_COLS],
}

impl ColSet {
    pub fn from_registry(r: &crate::ext::Registry) -> ColSet {
        let tags = crate::tags::column_shown() && r.is_enabled(crate::tags::EXT_TAGS).unwrap_or(false);
        let mut s = ColSet { generation: r.generation(), tags, order: [Col::Name; NUM_COLS], n: 0, label: Default::default(), right: [false; NUM_COLS] };
        for c in r.columns.iter() {
            // provider columns (plugins) are not drawn by the details view yet
            let ColumnSource::Field(col) = c.source else { continue };
            if col == Col::Tags && !tags {
                continue;
            }
            if s.n < NUM_COLS && !s.order[..s.n].contains(&col) {
                s.order[s.n] = col;
                s.n += 1;
                s.label[col.slot()] = c.spec.label.clone();
                s.right[col.slot()] = c.spec.align == Align::Right;
            }
        }
        s
    }

    /// The set of the current registry.
    pub fn current() -> ColSet {
        crate::ext::read(ColSet::from_registry)
    }

    /// The enabled columns, in order.
    pub fn order(&self) -> &[Col] {
        &self.order[..self.n]
    }

    pub fn label(&self, c: Col) -> &str {
        &self.label[c.slot()]
    }

    pub fn right_aligned(&self, c: Col) -> bool {
        self.right[c.slot()]
    }
}

/// x and width of each column by `Col::slot` (relative to the left of the
/// header). A column not shown has width 0.
pub type Spans = [(f32, f32); NUM_COLS];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Columns {
    /// Modified, Type, Size, Git, Tags
    pub w: [f32; NUM_COLS - 1],
}

const FIXED: [Col; NUM_COLS - 1] = [Col::Modified, Col::Type, Col::Size, Col::Git, Col::Tags];

impl Columns {
    pub fn new(modified: f32, ty: f32, size: f32, git: f32) -> Columns {
        Columns { w: [modified, ty, size, git, BUILTIN[5].spec.width] }
    }

    /// Widths of the shown fixed columns (0 for the others).
    fn shown_w(&self, order: &[Col]) -> [f32; NUM_COLS - 1] {
        let mut w = [0.0; NUM_COLS - 1];
        for (i, c) in FIXED.iter().enumerate() {
            if order.contains(c) {
                w[i] = self.w[i];
            }
        }
        w
    }

    /// Spans of the columns in `order` for a header `total` wide. Fixed
    /// columns shrink (down to `MIN_W`, proportionally) when the window is
    /// too narrow for the name. Columns not in `order` (switched off, or Type
    /// in a narrow pane) get no width and Name takes it.
    pub fn spans(&self, total: f32, order: &[Col]) -> Spans {
        let mut w = self.shown_w(order);
        let room = (total - MIN_NAME_W).max(0.0);
        let fixed: f32 = w.iter().sum();
        if fixed > room {
            let mut locked = [false; NUM_COLS - 1];
            loop {
                let mut locked_sum = 0.0;
                let mut free_sum = 0.0;
                for (i, &v) in w.iter().enumerate() {
                    if !order.contains(&FIXED[i]) {
                        continue;
                    }
                    if locked[i] {
                        locked_sum += v;
                    } else {
                        free_sum += v;
                    }
                }
                let avail = (room - locked_sum).max(0.0);
                if free_sum <= avail + 0.001 || free_sum <= 0.0 {
                    break;
                }
                let k = avail / free_sum;
                let mut clamped_any = false;
                for (i, v) in w.iter_mut().enumerate() {
                    if order.contains(&FIXED[i]) && !locked[i] {
                        let next = *v * k;
                        if next <= MIN_W {
                            *v = MIN_W;
                            locked[i] = true;
                            clamped_any = true;
                        }
                    }
                }
                if !clamped_any {
                    for (i, v) in w.iter_mut().enumerate() {
                        if order.contains(&FIXED[i]) && !locked[i] {
                            *v *= k;
                        }
                    }
                    break;
                }
            }
        }
        let name_w = (total - w.iter().sum::<f32>()).max(0.0);
        let mut out = [(0.0, 0.0); NUM_COLS];
        let mut x = 0.0;
        for &c in order {
            let wd = if c == Col::Name { name_w } else { w[c.slot() - 1] };
            out[c.slot()] = (x, wd);
            x += wd;
        }
        out
    }

    /// Drags the left edge of `col` to `edge_x` (header-relative): that
    /// column's width changes, the others stay. Keeps every column at least
    /// `MIN_W` and the name at least `MIN_NAME_W`. Returns true when changed.
    pub fn drag_left_edge(&mut self, col: Col, edge_x: f32, total: f32, order: &[Col]) -> bool {
        if col == Col::Name || !order.contains(&col) {
            return false;
        }
        let i = col.slot() - 1;
        let s = self.spans(total, order)[col.slot()];
        let right = s.0 + s.1;
        let others = self.shown_w(order).iter().sum::<f32>() - self.w[i];
        let max_w = (total - MIN_NAME_W - others).max(MIN_W);
        let w = (right - edge_x).clamp(MIN_W, max_w);
        let changed = (w - self.w[i]).abs() > f32::EPSILON;
        self.w[i] = w;
        changed
    }
}

impl Default for Columns {
    /// The built-in columns' widths.
    fn default() -> Self {
        Columns::new(BUILTIN[1].spec.width, BUILTIN[2].spec.width, BUILTIN[3].spec.width, BUILTIN[4].spec.width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Col; 5] = [Col::Name, Col::Modified, Col::Type, Col::Size, Col::Git];
    const NO_TYPE: [Col; 4] = [Col::Name, Col::Modified, Col::Size, Col::Git];

    #[test]
    fn default_spans_fit_the_total() {
        let c = Columns::default();
        assert_eq!(c.w, [150.0, 130.0, 90.0, 56.0, 168.0], "the built-in widths");
        let s = c.spans(1000.0, &ALL);
        assert_eq!(s[0], (0.0, 574.0));
        assert_eq!(s[1], (574.0, 150.0));
        assert_eq!(s[2], (724.0, 130.0));
        assert_eq!(s[3], (854.0, 90.0));
        assert_eq!(s[4], (944.0, 56.0));
        let last = s[4];
        assert_eq!(last.0 + last.1, 1000.0);
    }

    #[test]
    fn hidden_type_column_gives_its_width_to_the_name() {
        let c = Columns::default();
        let s = c.spans(600.0, &NO_TYPE);
        assert_eq!(s[2].1, 0.0);
        assert_eq!(s[0], (0.0, 304.0));
        assert_eq!(s[1], (304.0, 150.0));
        assert_eq!(s[3], (454.0, 90.0));
        assert_eq!(s[4], (544.0, 56.0));
        assert_eq!(s[4].0 + s[4].1, 600.0);
        let tiny = c.spans(200.0, &NO_TYPE);
        assert!(tiny.iter().all(|&(x, w)| x >= 0.0 && w >= 0.0));
    }

    #[test]
    fn the_registry_order_is_the_layout_order() {
        let c = Columns::default();
        let s = c.spans(1000.0, &[Col::Name, Col::Size, Col::Modified]);
        assert_eq!(s[0], (0.0, 760.0));
        assert_eq!(s[3], (760.0, 90.0));
        assert_eq!(s[1], (850.0, 150.0));
        assert_eq!(s[2].1, 0.0);
        assert_eq!(s[4].1, 0.0);
    }

    #[test]
    fn narrow_window_shrinks_fixed_columns_not_the_name() {
        let c = Columns::default();
        let s = c.spans(400.0, &ALL);
        assert!(s[0].1 >= MIN_NAME_W - 0.01, "name {}", s[0].1);
        for sp in &s[1..] {
            assert!(sp.1 >= MIN_W || sp.1 == 0.0);
        }
        // absurdly narrow: still no negative widths
        let s = c.spans(50.0, &ALL);
        assert!(s.iter().all(|&(x, w)| x >= 0.0 && w >= 0.0));
    }

    #[test]
    fn dragging_a_left_edge_resizes_that_column_only() {
        let mut c = Columns::default();
        // Type's left edge from 724 to 644: Type grows by 80
        assert!(c.drag_left_edge(Col::Type, 644.0, 1000.0, &ALL));
        assert_eq!(c.w, [150.0, 210.0, 90.0, 56.0, 168.0]);
        // way left: clamp so the name keeps its minimum
        c.drag_left_edge(Col::Type, -500.0, 1000.0, &ALL);
        assert_eq!(c.w[1], 1000.0 - MIN_NAME_W - 150.0 - 90.0 - 56.0);
        // way right: clamp to the minimum width
        c.drag_left_edge(Col::Size, 5000.0, 1000.0, &ALL);
        assert_eq!(c.w[2], MIN_W);
        // the name column has no handle; a hidden column neither
        assert!(!c.drag_left_edge(Col::Name, 10.0, 1000.0, &ALL));
        assert!(!c.drag_left_edge(Col::Type, 10.0, 1000.0, &NO_TYPE));
        // same edge again changes nothing
        let before = c;
        let size_s = c.spans(1000.0, &ALL)[Col::Size.slot()];
        assert!(!c.drag_left_edge(Col::Size, size_s.0, 1000.0, &ALL));
        assert_eq!(c, before);
    }

    #[test]
    fn the_registry_gives_the_built_in_set_and_a_switched_off_column_leaves_it() {
        let mut r = crate::ext::builtin();
        let s = ColSet::from_registry(&r);
        assert_eq!(s.order(), ALL);
        assert_eq!(s.label(Col::Modified), "Date modified");
        assert_eq!(s.label(Col::Git), "Git");
        assert!(s.right_aligned(Col::Size) && !s.right_aligned(Col::Name));
        r.set_enabled("tsubame.column.type", false).unwrap();
        let s2 = ColSet::from_registry(&r);
        assert_eq!(s2.order(), NO_TYPE);
        assert_ne!(s2.generation, s.generation);
        // disabling Git column gives it zero width
        r.set_enabled("tsubame.column.git", false).unwrap();
        let s3 = ColSet::from_registry(&r);
        assert!(!s3.order().contains(&Col::Git));
        assert_eq!(Columns::default().spans(1000.0, s3.order())[Col::Git.slot()].1, 0.0);
        assert!(r.set_enabled(crate::ext::CORE, false).is_err(), "Name stays");
    }
}
