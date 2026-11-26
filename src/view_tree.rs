//! P4.F6 Tree view, the view's side (`tsubame.ext.tree`, `crate::tree` is the
//! model). Two surfaces: Tree rows (Details made hierarchical, same columns)
//! and the opt-in folders-only Tree pane left of the list (a drawer under
//! 560 px). Drawing, keys and the commands live here; the model owns the
//! expanded set and the flattened rows.
//!
//! Zero idle cost: nothing here runs on a timer. A frame is drawn only for
//! the visible rows, the chevron is a glyph (no animated spinner), and the
//! "Loading…" row is drawn when a worker batch or the one-shot 80 ms wake
//! arrives.

use super::*;
use crate::tree::{self, Kind, State, TreeModel};
use tsubame_ui::ListResponse;
use tsubame_ui::ListState;

/// Chevron hit area: 20 DIPs wide, the full row height.
pub(super) const CHEV_W: f32 = 20.0;
/// Narrow windows: Modified hides under this width, Size under `NARROW_SIZE`.
pub const NARROW_DATE: f32 = 760.0;
pub const NARROW_SIZE: f32 = 560.0;
/// Under this width the pane is a drawer over the list.
pub const DRAWER_BELOW: f32 = 560.0;
pub const DRAWER_W: f32 = 248.0;
const ID_TPANE: Id = Id::new("tree-pane");
const ID_TSPLIT: Id = Id::new("tree-split");

/// The view's Tree state that is not in a tab.
#[derive(Default)]
pub struct TreeUi {
    /// the window is narrow: the pane is a drawer
    pub narrow: bool,
    /// the drawer is open (narrow windows only)
    pub drawer: bool,
    /// the pane list has the keyboard
    pub pane_focus: bool,
}

/// Root of a drive path: `C:\`.
pub fn drive_root(path: &str) -> Option<String> {
    let b = path.as_bytes();
    (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()).then(|| format!("{}:\\", &path[..1]))
}

/// What a key did to a tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyDid {
    Nothing,
    Moved,
    Changed,
}

/// Right / Left / Shift+Right and the `+ - *` characters on the cursor row `list.cursor`.
pub fn nav_key(m: &mut TreeModel, root: tree::Root, list: &mut ListState, key: Key, mods: Mods, ch: Option<char>) -> KeyDid {
    let i = list.cursor;
    let len = m.len();
    if i >= len {
        return KeyDid::Nothing;
    }
    let folder = m.is_folder(i, root);
    let go = |m: &mut TreeModel, list: &mut ListState, j: usize| {
        m.select_single(j, root);
        list.move_cursor(j, m.len());
    };
    let did = match (key, ch) {
        (Key::Right, _) if mods.shift && !mods.ctrl && !mods.alt => {
            if folder {
                m.expand_all(i, root);
                KeyDid::Changed
            } else {
                KeyDid::Nothing
            }
        }
        (Key::Right, _) if mods.none() => {
            if !folder {
                KeyDid::Nothing
            } else if !m.is_expanded(i, root) {
                match m.expand(i, true, root) {
                    tree::Expanded::Opened => KeyDid::Changed,
                    _ => KeyDid::Nothing,
                }
            } else if let Some(c) = m.first_child_row(i) {
                go(m, list, c);
                KeyDid::Moved
            } else {
                KeyDid::Nothing
            }
        }
        (Key::Left, _) if mods.none() => {
            if folder && m.is_expanded(i, root) {
                m.collapse(i, root);
                KeyDid::Changed
            } else if let Some(p) = m.parent_row(i) {
                go(m, list, p);
                KeyDid::Moved
            } else {
                KeyDid::Nothing
            }
        }
        (_, Some('+')) if folder => {
            m.expand(i, true, root);
            KeyDid::Changed
        }
        (_, Some('-')) if folder => {
            m.collapse(i, root);
            KeyDid::Changed
        }
        (_, Some('*')) if folder => {
            m.expand_all(i, root);
            KeyDid::Changed
        }
        _ => KeyDid::Nothing,
    };
    if did == KeyDid::Changed {
        m.flatten(root);
        if let Some(r) = m.cursor_row(root) {
            list.cursor = r;
        }
    }
    did
}

/// Backspace: the parent row (the model keeps the selection on it).
pub fn to_parent(m: &mut TreeModel, root: tree::Root, list: &mut ListState) -> bool {
    match m.parent_row(list.cursor) {
        Some(p) => {
            m.select_single(p, root);
            list.move_cursor(p, m.len());
            true
        }
        None => false,
    }
}

impl View {
    /// Tree rows are showing (the mode is on and the extension is enabled).
    pub(super) fn tree_rows_on(&self) -> bool {
        self.mode == ViewMode::Tree && tree::enabled()
    }

    /// The extension switch changed: the mode, the pane and the models follow.
    pub(super) fn sync_tree_ext(&mut self) {
        if tree::enabled() {
            return;
        }
        if self.mode == ViewMode::Tree {
            self.set_mode(ViewMode::Details);
        }
        self.prefs.tree.pane = false;
        self.tree_ui.drawer = false;
        for t in &mut self.tabs.items {
            t.leave_tree();
            t.tree_pane = None;
        }
    }

    /// Commands of the extension, and Backspace in Tree rows.
    pub(super) fn run_tree(&mut self, cmd: u32) -> bool {
        match cmd {
            CMD_VIEW_TREE => self.set_mode(ViewMode::Tree),
            CMD_PANE_TREE => {
                if self.tree_ui.narrow {
                    self.tree_ui.drawer = !self.tree_ui.drawer;
                } else {
                    self.prefs.tree.pane = !self.prefs.tree.pane;
                    self.config_dirty = true;
                }
                self.focus_list = true;
            }
            CMD_BACKSPACE_UP if self.tree_rows_on() && !self.tab().settings && self.tab().tree.is_some() => {
                let tab = &mut self.tabs.items[self.tabs.active];
                if let Some(m) = tab.tree.as_mut()
                    && !to_parent(m, Some(&tab.store), &mut tab.list)
                {
                    // a top-level row: Backspace goes up one folder as usual
                    return false;
                }
            }
            _ => return false,
        }
        true
    }

    /// Pumps the models of the pane's tabs. UI thread, no disk.
    pub(super) fn pump_trees(&mut self) -> bool {
        let rows = self.tree_rows_on();
        let cfg = self.prefs.tree.sanitized();
        let active = self.tabs.active;
        let mut changed = false;
        for (i, t) in self.tabs.items.iter_mut().enumerate() {
            if t.tree_on != rows {
                t.tree_on = rows;
                // Tree ignores grouping (and gives the rows back when it goes)
                t.sync_group();
            }
            if rows {
                if i == active {
                    t.ensure_tree(&cfg);
                }
                changed |= t.pump_tree();
            } else if t.tree.is_some() {
                t.leave_tree();
            }
            if let Some(m) = t.tree_pane.as_mut() {
                m.set_limits(cfg.expand_cap as usize, cfg.child_cap as usize);
                let mut c = m.pump(None, false);
                if m.needs_flatten() {
                    m.flatten(None);
                    c = true;
                }
                changed |= c;
            }
        }
        changed
    }

    /// Typed characters and arrow keys in the Tree. True when handled (repaint).
    pub(super) fn tree_keys(&mut self, ui: &mut Ui) -> bool {
        let blocked = self.tab().settings
            || self.palette.is_some()
            || self.search.is_some()
            || self.tags.picker_open()
            || self.preview.quick_look_open()
            || self.editing
            || self.rename.is_some()
            || ui.has_focus(pid(self.live, ID_FILTER));
        if blocked {
            return false;
        }
        let pane = self.tree_ui.pane_focus && ui.has_focus(pid(self.live, ID_TPANE));
        if self.tree_rows_on() && self.tab().tree.is_none() {
            let cfg = self.prefs.tree.sanitized();
            self.tab_mut().ensure_tree(&cfg);
            self.tab_mut().pump_tree();
        }
        let rows = self.tree_rows_on() && self.tab().tree.is_some();
        if !pane && !rows {
            return false;
        }
        let events: Vec<Event> = ui.leftover().to_vec();
        let mut did = false;
        for ev in events {
            let (key, mods, ch) = match ev {
                Event::KeyDown { key, mods, .. } => (key, mods, None),
                Event::Char(c) if !c.is_control() => (Key::Char(c), Mods::NONE, Some(c)),
                _ => continue,
            };
            let tab = &mut self.tabs.items[self.tabs.active];
            let r = if pane {
                let Some(m) = tab.tree_pane.as_mut() else { continue };
                if let Some(c) = ch.filter(|c| !matches!(c, '+' | '-' | '*')) {
                    if let Some(j) = m.type_ahead(c, tab.pane_list.cursor, None) {
                        m.select_single(j, None);
                        tab.pane_list.move_cursor(j, m.len());
                        did = true;
                    }
                    continue;
                }
                nav_key(m, None, &mut tab.pane_list, key, mods, ch)
            } else {
                let Some(m) = tab.tree.as_mut() else { continue };
                if let Some(c) = ch.filter(|c| !matches!(c, '+' | '-' | '*')) {
                    if c != ' '
                        && let Some(j) = m.type_ahead(c, tab.list.cursor, Some(&tab.store))
                    {
                        m.select_single(j, Some(&tab.store));
                        tab.list.move_cursor(j, m.len());
                        did = true;
                    }
                    continue;
                }
                nav_key(m, Some(&tab.store), &mut tab.list, key, mods, ch)
            };
            did |= r != KeyDid::Nothing;
        }
        did
    }

    /// Enter or double click on row `pos` in Tree rows. True when it was a
    /// folder toggle, a retry or a message row (the caller does nothing more).
    pub(super) fn tree_activate(&mut self, pos: usize) -> bool {
        if !self.tree_rows_on() {
            return false;
        }
        let tab = &mut self.tabs.items[self.tabs.active];
        let Some(m) = tab.tree.as_mut() else { return false };
        let root = Some(&tab.store);
        let Some(r) = m.row(pos) else { return false };
        match r.kind {
            Kind::Entry => {
                if !m.is_folder(pos, root) {
                    return false;
                }
                if m.is_expanded(pos, root) {
                    m.collapse(pos, root);
                } else {
                    m.expand(pos, true, root);
                }
                m.flatten(root);
                if let Some(c) = m.cursor_row(root) {
                    tab.list.cursor = c;
                }
                true
            }
            Kind::Denied => {
                m.relist(r.slot);
                true
            }
            _ => true,
        }
    }

    /// The row's chevron (or the Denied row's retry) was pressed.
    fn tree_toggle(&mut self, pos: usize) {
        let tab = &mut self.tabs.items[self.tabs.active];
        let Some(m) = tab.tree.as_mut() else { return };
        let root = Some(&tab.store);
        if m.is_expanded(pos, root) {
            m.collapse(pos, root);
        } else {
            m.expand(pos, true, root);
        }
        m.flatten(root);
        if let Some(c) = m.cursor_row(root) {
            tab.list.cursor = c;
        }
    }

    /// Paints the Tree rows list (the Details columns, hierarchical).
    pub(super) fn paint_tree_list(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, list_r: Rect, spans: crate::cols::Spans, t: &Theme) -> (ListResponse, Option<usize>) {
        let age = crate::age::frame(&self.prefs.file_age, self.dark, g, t);
        let mouse = ui.mouse();
        let tab = &mut self.tabs.items[self.tabs.active];
        tab.list.row_h = t.height_list_row;
        let Tab { store, tree, list: lstate, tag_fids, path, .. } = &mut *tab;
        let Some(m) = tree.as_mut() else { return (ListResponse::default(), None) };
        // the cursor follows its entry when rows moved
        if let Some(c) = m.cursor_row(Some(store)) {
            lstate.cursor = c;
        }
        let mut rows = TreeRows {
            m,
            root: Some(store),
            root_fids: tag_fids,
            shell,
            row: &mut self.rowbuf,
            text: &mut self.text,
            spans,
            zebra: self.zebra,
            age,
            tags: &mut self.tags,
            columns: true,
            mouse,
            list_x: list_r.x,
            chev: None,
            clicked: None,
        };
        let lr = list(ui, g, pid(self.live, ID_LIST), list_r, lstate, &mut rows);
        if crate::uia_rows::on() {
            let area = Rect::new(list_r.x, list_r.y, list_r.w - ui.theme.layout_scrollbar_width, list_r.h);
            let geo = crate::uia_rows::Geo { key: crate::uia_rows::skey(self.live, crate::uia_rows::PART_LIST), title: crate::uia_rows::leaf(path), area, focused: ui.has_focus(pid(self.live, ID_LIST)) && ui.window_focused() };
            crate::uia_rows::tree(&geo, lstate, lr.range, rows.m, rows.root, rows.root_fids, rows.tags, rows.row, rows.age.as_ref(), true);
        }
        let chev = rows.chev;
        let activated = lr.activated;
        let status = rows.m.take_status();
        if let Some(msg) = status {
            self.ops.message = Some(msg);
        }
        match chev {
            Some(i) => {
                self.tree_toggle(i);
                (lr, None)
            }
            None => (lr, activated),
        }
    }

    // ---- the pane --------------------------------------------------------------------

    /// Cuts the Tree pane off `main` and paints it. Returns the rest.
    pub(super) fn paint_tree_pane(&mut self, ui: &mut Ui, g: &mut dyn Canvas, shell: &mut ShellUi, main: Rect, t: &Theme) -> Rect {
        let narrow = main.w < DRAWER_BELOW;
        self.tree_ui.narrow = narrow;
        let wanted = tree::enabled() && !self.tab().settings && !self.tab().synthetic && !self.tab().is_archive() && drive_root(&self.tab().path).is_some();
        let open = wanted && if narrow { self.tree_ui.drawer } else { self.prefs.tree.pane };
        if !open {
            // nothing is built, nothing is watched
            let tab = &mut self.tabs.items[self.tabs.active];
            if tab.tree_pane.is_some() && !wanted {
                tab.tree_pane = None;
            }
            if let Some(m) = tab.tree_pane.as_mut() {
                m.set_visible(false);
            }
            self.tree_ui.pane_focus = false;
            return main;
        }
        // the model follows the tab's folder
        let cfg = self.prefs.tree.sanitized();
        let wake = self.wake.clone();
        let (sort, hide) = {
            let tab = self.tab();
            (tab.sort, tab.store.hide())
        };
        let path = self.tab().path.clone();
        let root_dir = drive_root(&path).unwrap_or_default();
        {
            let tab = &mut self.tabs.items[self.tabs.active];
            if tab.tree_pane.as_ref().is_none_or(|m| !m.root_path().eq_ignore_ascii_case(&root_dir)) {
                let mut m = TreeModel::new(&root_dir, true, true, wake, sort, hide);
                m.set_limits(cfg.expand_cap as usize, cfg.child_cap as usize);
                tab.tree_pane = Some(Box::new(m));
                tab.pane_list = ListState::new(t.height_list_row);
            }
            if let Some(m) = tab.tree_pane.as_mut() {
                m.set_visible(true);
                m.set_rules(sort, hide);
                if m.followed().is_none_or(|f| !f.eq_ignore_ascii_case(&path)) {
                    m.reveal(&path);
                }
            }
        }
        let (rect, rest, drawer) = if narrow {
            let w = DRAWER_W.min(main.w);
            (Rect::new(main.x, main.y, w, main.h), main, true)
        } else {
            let mut pos = self.prefs.tree.pane_width as f32;
            let (a, b, moved) = splitter(ui, g, pid(self.live, ID_TSPLIT), main, Split::LeftRight, &mut pos, tree::PANE_MIN, 360.0);
            if moved {
                self.prefs.tree.pane_width = pos.round() as u32;
                self.config_dirty = true;
            }
            (a, b, false)
        };
        let mut area = rect;
        if drawer {
            g.fill(area, t.popup);
            g.fill(Rect::new(area.x + area.w - 1.0, area.y, 1.0, area.h), t.border);
        } else {
            g.fill(area, t.panel);
        }
        let title = area.cut_top(t.height_column_header);
        g.text("Folders", Rect::new(title.x + 10.0, title.y, title.w - 20.0, title.h), t.font_small().b(), t.text_dim);
        g.fill(Rect::new(title.x, title.y + title.h - 1.0, title.w, 1.0), t.border);
        g.fill(area, t.row);
        let mouse = ui.mouse();
        let mut spans: crate::cols::Spans = [(0.0, 0.0); crate::cols::NUM_COLS];
        spans[0] = (0.0, (area.w - t.layout_scrollbar_width).max(0.0));
        let (lr, chev, clicked) = {
            let age = None;
            let tab = &mut self.tabs.items[self.tabs.active];
            tab.pane_list.row_h = t.height_list_row;
            let Tab { tree_pane, pane_list, .. } = &mut *tab;
            let Some(m) = tree_pane.as_mut() else { return rest };
            // the followed folder scrolls into view once its row exists
            if m.follow.is_some()
                && let Some(c) = m.cursor_row(None)
            {
                pane_list.cursor = c;
                pane_list.ensure_visible(c, m.len());
                m.follow = None;
            }
            let mut rows = TreeRows {
                m,
                root: None,
                root_fids: &[],
                shell,
                row: &mut self.rowbuf,
                text: &mut self.text,
                spans,
                zebra: false,
                age,
                tags: &mut self.tags,
                columns: false,
                mouse,
                list_x: area.x,
                chev: None,
                clicked: None,
            };
            let lr = list(ui, g, pid(self.live, ID_TPANE), area, pane_list, &mut rows);
            if crate::uia_rows::on() {
                let rows_r = Rect::new(area.x, area.y, area.w - ui.theme.layout_scrollbar_width, area.h);
                let geo = crate::uia_rows::Geo { key: crate::uia_rows::skey(self.live, crate::uia_rows::PART_PANE), title: "Folders", area: rows_r, focused: ui.has_focus(pid(self.live, ID_TPANE)) && ui.window_focused() };
                crate::uia_rows::tree(&geo, pane_list, lr.range, rows.m, None, &[], rows.tags, rows.row, None, false);
            }
            (lr, rows.chev, rows.clicked)
        };
        self.tree_ui.pane_focus = ui.has_focus(pid(self.live, ID_TPANE));
        let mut go: Option<String> = None;
        {
            let tab = &mut self.tabs.items[self.tabs.active];
            if let Some(m) = tab.tree_pane.as_mut() {
                if let Some(i) = chev {
                    if m.is_expanded(i, None) {
                        m.collapse(i, None);
                    } else {
                        m.expand(i, true, None);
                    }
                    m.flatten(None);
                    if let Some(c) = m.cursor_row(None) {
                        tab.pane_list.cursor = c;
                    }
                } else if let Some(i) = lr.activated.or(clicked)
                    && let Some(p) = m.folder_path_of(i, None)
                    && m.row(i).is_some_and(|r| r.kind == Kind::Entry)
                {
                    go = Some(p);
                }
                if let Some(msg) = m.take_status() {
                    self.ops.message = Some(msg);
                }
            }
        }
        if lr.pressed && drawer {
            // a pick closes the drawer
            self.tree_ui.drawer = go.is_none();
        }
        if let Some(p) = go {
            if let Some(m) = self.tabs.items[self.tabs.active].tree_pane.as_mut() {
                // the pane already shows it: do not re-reveal on the navigation
                m.mark_following(&p);
            }
            if !p.eq_ignore_ascii_case(&self.tab().path) {
                self.go(&p);
            }
            if drawer {
                self.tree_ui.drawer = false;
            }
            ui.request_repaint();
        }
        if drawer && ui.has_focus(pid(self.live, ID_TPANE)) {
            // Esc closes the drawer
            let esc = ui.leftover().iter().any(|e| matches!(e, Event::KeyDown { key: Key::Escape, .. }));
            if esc {
                self.tree_ui.drawer = false;
                self.focus_list = true;
            }
        }
        rest
    }
}

/// The Tree's rows as a list source: Details-style cells, hierarchical names.
struct TreeRows<'a> {
    m: &'a mut TreeModel,
    root: Option<&'a EntryStore>,
    root_fids: &'a [(u128, u32)],
    shell: &'a mut ShellUi,
    row: &'a mut RowBuf,
    text: &'a mut String,
    spans: crate::cols::Spans,
    zebra: bool,
    age: Option<crate::age::AgeFrame>,
    tags: &'a mut crate::tags::ui::TagsUi,
    /// Details columns (Tree rows); the pane draws names only
    columns: bool,
    mouse: Pt,
    list_x: f32,
    /// a press landed on this row's chevron
    chev: Option<usize>,
    /// a plain click on this row (the pane navigates)
    clicked: Option<usize>,
}

impl TreeRows<'_> {
    /// Left edge of the chevron of row `i`.
    fn chev_x(&self, depth: u16) -> f32 {
        self.list_x + self.spans[0].0 + 4.0 + tree::indent(depth)
    }
}

impl ListSource for TreeRows<'_> {
    fn len(&self) -> usize {
        self.m.len()
    }

    fn is_selected(&self, i: usize) -> bool {
        self.m.is_selected(i, self.root)
    }

    fn select(&mut self, i: usize, mods: Mods, by: SelectBy) {
        let root = self.root;
        let Some(tr) = self.m.row(i) else { return };
        if by == SelectBy::Click && !mods.ctrl && !mods.shift {
            // a press on the chevron toggles; it does not change the selection
            let cx = self.chev_x(tr.depth);
            if tr.kind == Kind::Entry && self.m.is_folder(i, root) && self.mouse.x >= cx && self.mouse.x < cx + CHEV_W {
                self.chev = Some(i);
                return;
            }
        }
        if tr.kind != Kind::Entry {
            // message rows are not selectable; the cursor still lands on them
            if by == SelectBy::Click || by == SelectBy::Key {
                self.m.set_cursor_row(i, root);
            }
            return;
        }
        if by == SelectBy::Click && !mods.ctrl && !mods.shift && self.m.sel_count() > 1 && self.m.is_selected(i, root) {
            super::KEEP_MULTI.with(|k| k.set(Some(i)));
            return;
        }
        match by {
            SelectBy::Click | SelectBy::Key if mods.shift => self.m.extend(i, root),
            SelectBy::Click if mods.ctrl => self.m.toggle(i, root),
            SelectBy::Key if mods.ctrl => self.m.set_cursor_row(i, root),
            SelectBy::Space if mods.ctrl => self.m.toggle(i, root),
            SelectBy::Space | SelectBy::RightClick if self.m.is_selected(i, root) => {}
            _ => self.m.select_single(i, root),
        }
        if by == SelectBy::Click && !mods.ctrl && !mods.shift {
            self.clicked = Some(i);
        }
    }

    fn select_none(&mut self) {
        self.m.clear_selection();
    }

    fn band_begin(&mut self, _mode: BandMode) {
        self.m.clear_selection();
    }

    fn band_update(&mut self, rows: Option<(usize, usize)>) {
        self.m.clear_selection();
        if let Some((a, b)) = rows {
            let root = self.root;
            for i in a..=b {
                self.m.add_selection(i, root);
            }
        }
    }

    fn draw_row(&mut self, g: &mut dyn Canvas, t: &Theme, i: usize, r: Rect, st: RowState) {
        let Some(tr) = self.m.row(i) else { return };
        if self.zebra && i % 2 == 1 && !st.selected && !st.hot {
            g.fill(r, t.row_alt);
        }
        let root = self.root;
        let x = r.x;
        let sp = self.spans;
        let cx = x + sp[0].0 + 4.0 + tree::indent(tr.depth);
        let name_x = cx + CHEV_W + ICON_DIP + 6.0;
        let name_w = (x + sp[0].0 + sp[0].1 - name_x - 8.0).max(0.0);
        if tr.kind != Kind::Entry {
            let small = t.font_small();
            let tx = cx + CHEV_W;
            let w = (x + sp[0].0 + sp[0].1 - tx - 8.0).max(0.0);
            match tr.kind {
                Kind::Loading => g.text("Loading\u{2026}", Rect::new(tx, r.y, w, r.h), small, t.text_dim),
                Kind::Empty => g.text("Empty folder", Rect::new(tx, r.y, w, r.h), small, t.text_dim),
                Kind::Denied => {
                    let msg = match self.m.state_of(tr.slot) {
                        Some(State::Denied(m)) => m.clone(),
                        _ => String::new(),
                    };
                    let text = if msg.is_empty() { "Access denied".to_string() } else { msg };
                    let tw = g.measure(&text, small).min((w - 60.0).max(0.0));
                    g.text(&text, Rect::new(tx, r.y, tw + 1.0, r.h), small, t.danger);
                    g.text("Retry", Rect::new(tx + tw + 8.0, r.y, 50.0, r.h), small, t.accent);
                    g.fill(Rect::new(tx + tw + 8.0, r.y + r.h / 2.0 + 6.0, 28.0, 1.0), t.accent);
                }
                Kind::Loop => {
                    let target = match self.m.state_of(tr.slot) {
                        Some(State::Loop(p)) => p.clone(),
                        _ => String::new(),
                    };
                    g.text(&format!("Not expanded: loops back to {target}"), Rect::new(tx, r.y, w, r.h), small, t.text_dim);
                }
                Kind::Capped => {
                    let (shown, total) = (self.m.dir_shown(tr.slot), self.m.dir_total(tr.slot));
                    g.text(&format!("Showing first {shown} of {total}"), Rect::new(tx, r.y, w, r.h), small, t.text_dim);
                }
                Kind::Entry => {}
            }
            return;
        }
        let Some((slot, id)) = self.m.entry_id(i, root) else { return };
        let dir_path = self.m.dir_path(slot).map(str::to_string);
        let m = &*self.m;
        let Some(store) = m.store_of(slot, root) else { return };
        let row = &mut *self.row;
        row.fill(store, id);
        let is_dir = row.is_dir();
        let link = m.is_link(i, root);
        // chevron: folders only; a link shows it dimmed until it is followed on purpose
        if is_dir {
            let glyph = if m.is_expanded(i, root) { "\u{E70D}" } else { "\u{E76C}" };
            let col = if link && !m.is_expanded(i, root) { t.text_disabled } else { t.text_dim };
            g.text(glyph, Rect::new(cx, r.y, CHEV_W, r.h), f(8.0).icon().c(), col);
        }
        // icon
        let ir = Rect::new(cx + CHEV_W, r.y + (r.h - ICON_DIP) / 2.0, ICON_DIP, ICON_DIP);
        if !crate::packs::draw_icon(g, t, row.name_str(), is_dir, row.hidden, ir) {
            match self.shell.row_icon(row, dir_path.as_deref()) {
                Some(img) => g.image(&img.key, img.w, img.h, &img.px, ir),
                None => {
                    let (ic, col) = if is_dir { (Rect::new(ir.x, ir.y + 3.0, 16.0, 11.0), t.folder) } else { (Rect::new(ir.x + 2.0, ir.y, 12.0, 16.0), t.text_dim) };
                    g.round_a(ic, 2.0, col, 0.35);
                }
            }
        }
        if link {
            // reparse badge on the icon corner, then the target text after the name
            g.text("\u{E71B}", Rect::new(ir.x + ir.w - 8.0, ir.y + ir.h - 9.0, 10.0, 10.0), f(8.0).icon().c(), t.accent);
        }
        let name_c = if row.hidden { t.text_dim } else { crate::packs::name_color(row.name_str(), is_dir).unwrap_or(t.text) };
        let font = t.font();
        let mut nw = g.measure(row.name_str(), font).min(name_w);
        // tags marks, the same cluster Details draws
        let fids = if slot == 0 && root.is_some() { self.root_fids } else { m.dir_fids(slot) };
        let mut tag_chips: Option<(u64, bool)> = None;
        if let Some(dir) = dir_path.as_deref()
            && let Some(mut tg) = self.tags.rows(dir, fids)
        {
            let tmask = tg.mask(id, row.name_str());
            if let Some(mk) = tmask.filter(|m| *m != 0) {
                let defs = tg.defs().to_vec();
                let cw = crate::tags::marks::cluster_w(g, t, &defs, mk, 3, &mut tg.buf);
                nw = g.measure(row.name_str(), font).min((name_w - cw - crate::tags::marks::LEAD).max(24.0));
                g.text(row.name_str(), Rect::new(name_x, r.y, nw + 1.0, r.h), font, name_c);
                let shapes = tg.shapes();
                crate::tags::marks::draw_cluster(g, t, &defs, mk, shapes, name_x + nw + crate::tags::marks::LEAD, r.y + r.h / 2.0, 3, crate::tags::marks::MARK, crate::tags::marks::GAP, &mut tg.buf);
                if self.columns && sp[5].1 > 0.0 {
                    crate::tags::marks::draw_chips(g, t, &defs, mk, shapes, Rect::new(x + sp[5].0 + 8.0, r.y, (sp[5].1 - 12.0).max(0.0), r.h), 2, &mut tg.buf);
                }
                tag_chips = Some((mk, shapes));
            } else if tmask.is_none() && tg.reading() {
                nw = g.measure(row.name_str(), font).min((name_w - 14.0).max(24.0));
                g.text(row.name_str(), Rect::new(name_x, r.y, nw + 1.0, r.h), font, name_c);
                crate::tags::marks::draw(g, crate::tags::marks::Mark::Pending, t.text_dim, name_x + nw + crate::tags::marks::LEAD + 5.0, r.y + r.h / 2.0, crate::tags::marks::MARK);
                tag_chips = Some((0, false));
            }
        }
        if tag_chips.is_none() {
            g.text(row.name_str(), Rect::new(name_x, r.y, name_w, r.h), font, name_c);
        }
        let mut after = name_x + nw + 6.0;
        if let Some(target) = m.link_target(i, root) {
            let small = t.font_small();
            let w = (x + sp[0].0 + sp[0].1 - after - 8.0).max(0.0);
            if w > 24.0 {
                g.text(&format!("\u{2192} {target}"), Rect::new(after, r.y, w, r.h), small, t.text_dim);
            }
        } else if link && self.age.is_none() {
            // not resolved yet: the badge alone says what it is
        }
        if tr.depth > tree::FULL_DEPTH {
            let label = format!("L{}", tr.depth);
            let small = f(10.0);
            let lw = g.measure(&label, small) + 8.0;
            let bx = (x + sp[0].0 + sp[0].1 - lw - 8.0).max(after);
            g.round_stroke(Rect::new(bx, r.y + (r.h - 14.0) / 2.0, lw, 14.0), t.radius_sm, t.border, 1.0);
            g.text(&label, Rect::new(bx + 4.0, r.y, lw, r.h), small, t.text_dim);
            after = bx + lw;
        }
        let _ = after;
        if !self.columns {
            return;
        }
        if sp[1].1 > 0.0 {
            data::fmt_date(row.mtime, self.text);
            let cell = Rect::new(x + sp[1].0 + 8.0, r.y, (sp[1].1 - 12.0).max(0.0), r.h);
            crate::age::draw_date(self.age.as_ref(), g, t, row.mtime, is_dir, self.text, cell, r);
        }
        if sp[2].1 > 0.0 {
            row.type_text(self.text);
            g.text(self.text, Rect::new(x + sp[2].0 + 8.0, r.y, (sp[2].1 - 12.0).max(0.0), r.h), t.font(), t.text_dim);
        }
        if sp[3].1 > 0.0 && !is_dir {
            data::fmt_size(row.size, self.text);
            g.text(self.text, Rect::new(x + sp[3].0, r.y, (sp[3].1 - 8.0).max(0.0), r.h), t.font().r(), t.text_dim);
        }
    }
}
