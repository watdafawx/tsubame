//! Details-view column sizing, no UI. Name takes what the others leave;
//! Modified, Type and Size have a width each that the user can drag.

pub const MIN_W: f32 = 48.0;
pub const MIN_NAME_W: f32 = 140.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    Name,
    Modified,
    Type,
    Size,
}

impl Col {
    pub const ALL: [Col; 4] = [Col::Name, Col::Modified, Col::Type, Col::Size];
    pub fn label(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Modified => "Date modified",
            Col::Type => "Type",
            Col::Size => "Size",
        }
    }
    pub fn right_aligned(self) -> bool {
        self == Col::Size
    }
    /// The matching tsubame-core sort column.
    pub fn sort_column(self) -> tsubame_core::Column {
        match self {
            Col::Name => tsubame_core::Column::Name,
            Col::Modified => tsubame_core::Column::Modified,
            Col::Type => tsubame_core::Column::Type,
            Col::Size => tsubame_core::Column::Size,
        }
    }
}

/// x and width of each column (relative to the left of the header).
pub type Spans = [(f32, f32); 4];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Columns {
    /// Modified, Type, Size
    pub w: [f32; 3],
}

impl Columns {
    /// Mock widths: date 150, type 130, size 90.
    pub fn new(modified: f32, ty: f32, size: f32) -> Columns {
        Columns { w: [modified, ty, size] }
    }

    fn fixed(&self) -> f32 {
        self.w.iter().sum()
    }

    /// Columns for a header `total` wide. Fixed columns shrink (down to
    /// `MIN_W`, proportionally) when the window is too narrow for the name.
    pub fn spans(&self, total: f32) -> Spans {
        let mut w = self.w;
        let room = (total - MIN_NAME_W).max(0.0);
        let fixed = self.fixed();
        if fixed > room {
            let k = if fixed > 0.0 { room / fixed } else { 1.0 };
            for v in &mut w {
                *v = (*v * k).max(MIN_W);
            }
        }
        let name_w = (total - w.iter().sum::<f32>()).max(0.0);
        let mx = name_w;
        let tx = mx + w[0];
        let sx = tx + w[1];
        [(0.0, name_w), (mx, w[0]), (tx, w[1]), (sx, w[2])]
    }

    /// Like `spans`, for a pane too narrow for the Type column (`hide_type`):
    /// Type gets no width and Name takes it. The spans keep their four slots.
    pub fn spans_pane(&self, total: f32, hide_type: bool) -> Spans {
        if !hide_type {
            return self.spans(total);
        }
        let mut w = [self.w[0], self.w[2]];
        let room = (total - MIN_NAME_W).max(0.0);
        let fixed = w[0] + w[1];
        if fixed > room {
            let k = if fixed > 0.0 { room / fixed } else { 1.0 };
            for v in &mut w {
                *v = (*v * k).max(MIN_W);
            }
        }
        let name_w = (total - w[0] - w[1]).max(0.0);
        let sx = name_w + w[0];
        [(0.0, name_w), (name_w, w[0]), (sx, 0.0), (sx, w[1])]
    }

    /// Drags the left edge of `col` to `edge_x` (header-relative): that
    /// column's width changes, the others stay. Keeps every column at least
    /// `MIN_W` and the name at least `MIN_NAME_W`. Returns true when changed.
    pub fn drag_left_edge(&mut self, col: Col, edge_x: f32, total: f32) -> bool {
        let i = match col {
            Col::Name => return false,
            Col::Modified => 0,
            Col::Type => 1,
            Col::Size => 2,
        };
        let s = self.spans(total)[i + 1];
        let right = s.0 + s.1;
        let others = self.fixed() - self.w[i];
        let max_w = (total - MIN_NAME_W - others).max(MIN_W);
        let w = (right - edge_x).clamp(MIN_W, max_w);
        let changed = (w - self.w[i]).abs() > f32::EPSILON;
        self.w[i] = w;
        changed
    }
}

impl Default for Columns {
    fn default() -> Self {
        Columns::new(150.0, 130.0, 90.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_spans_fit_the_total() {
        let c = Columns::default();
        let s = c.spans(1000.0);
        assert_eq!(s[0], (0.0, 630.0));
        assert_eq!(s[1], (630.0, 150.0));
        assert_eq!(s[2], (780.0, 130.0));
        assert_eq!(s[3], (910.0, 90.0));
        let last = s[3];
        assert_eq!(last.0 + last.1, 1000.0);
    }

    #[test]
    fn hidden_type_column_gives_its_width_to_the_name() {
        let c = Columns::default();
        let s = c.spans_pane(600.0, true);
        assert_eq!(s[2].1, 0.0);
        assert_eq!(s[0], (0.0, 360.0));
        assert_eq!(s[1], (360.0, 150.0));
        assert_eq!(s[3], (510.0, 90.0));
        assert_eq!(s[3].0 + s[3].1, 600.0);
        assert_eq!(c.spans_pane(1000.0, false), c.spans(1000.0));
        let tiny = c.spans_pane(200.0, true);
        assert!(tiny.iter().all(|&(x, w)| x >= 0.0 && w >= 0.0));
    }

    #[test]
    fn narrow_window_shrinks_fixed_columns_not_the_name() {
        let c = Columns::default();
        let s = c.spans(400.0);
        assert!(s[0].1 >= MIN_NAME_W - 0.01, "name {}", s[0].1);
        for sp in &s[1..] {
            assert!(sp.1 >= MIN_W);
        }
        // absurdly narrow: still no negative widths
        let s = c.spans(50.0);
        assert!(s.iter().all(|&(x, w)| x >= 0.0 && w >= 0.0));
    }

    #[test]
    fn dragging_a_left_edge_resizes_that_column_only() {
        let mut c = Columns::default();
        // Type's left edge from 780 to 700: Type grows by 80
        assert!(c.drag_left_edge(Col::Type, 700.0, 1000.0));
        assert_eq!(c.w, [150.0, 210.0, 90.0]);
        // way left: clamp so the name keeps its minimum
        c.drag_left_edge(Col::Type, -500.0, 1000.0);
        assert_eq!(c.w[1], 1000.0 - MIN_NAME_W - 150.0 - 90.0);
        // way right: clamp to the minimum width
        c.drag_left_edge(Col::Size, 5000.0, 1000.0);
        assert_eq!(c.w[2], MIN_W);
        // the name column has no handle
        assert!(!c.drag_left_edge(Col::Name, 10.0, 1000.0));
        // same edge again changes nothing
        let before = c;
        assert!(!c.drag_left_edge(Col::Size, 1000.0 - MIN_W, 1000.0));
        assert_eq!(c, before);
    }
}
