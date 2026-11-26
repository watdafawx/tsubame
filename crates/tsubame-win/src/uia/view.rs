//! UI Automation for the file views (docs/uia-views.md): Details and Grid rows,
//! Tree rows, the Tree pane and Miller columns.
//!
//! Same rules as the context menu (`docs/ctxmenu.md`, CM10): the providers read
//! an immutable [`ViewSnap`] that the UI thread builds only while a client
//! listens, they never touch app state, and a request (select, invoke, expand)
//! is queued and run by the UI thread on its next frame.
//!
//! Virtualised: a surface (List or Tree) holds only the rows that are on screen
//! plus the total (`size`, reported as SizeOfSet / PositionInSet), so the
//! snapshot costs the same for 20 rows and for 2 million.

use super::*;
use windows::Win32::System::Variant::VT_UNKNOWN;

/// Expand state of a tree row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Expand {
    /// no children (a file, a message row)
    #[default]
    Leaf,
    Collapsed,
    Expanded,
}

/// One visible row of a surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VItem {
    /// stable while the row exists (entry id, tree key)
    pub key: u64,
    /// "name, tags: Urgent, Work"
    pub name: String,
    /// type, size, modified and the age bucket, read as the row's value
    pub value: String,
    /// "PDF File" (UIA ItemType)
    pub kind: String,
    pub rect: Px,
    pub selected: bool,
    /// the keyboard cursor, in a surface that has the focus
    pub focused: bool,
    /// tree depth, 1 based (0: not a tree row)
    pub level: u16,
    /// 1 based position among all rows of the surface (0: unknown)
    pub pos: u32,
    pub expand: Expand,
    pub selectable: bool,
    /// Enter / double click does something (open, retry)
    pub invokable: bool,
    /// a group header of a grouped list (a UIA group, expanded or collapsed)
    pub group: bool,
}

/// A List or a Tree: the visible rows and how many there are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VSurface {
    /// stable id of the surface (pane and kind; a Miller column has its own)
    pub key: u32,
    pub tree: bool,
    pub name: String,
    /// "1,204 items, 3 selected" or the path of a Miller column
    pub desc: String,
    pub rect: Px,
    pub focused: bool,
    pub multi: bool,
    /// all rows, not only the visible ones
    pub size: u32,
    pub items: Vec<VItem>,
}

impl VSurface {
    fn item(&self, key: u64) -> Option<&VItem> {
        self.items.iter().find(|i| i.key == key)
    }
}

/// Every surface the window shows now. Empty: nothing to read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewSnap {
    pub surfaces: Vec<VSurface>,
}

impl ViewSnap {
    pub fn surface(&self, key: u32) -> Option<&VSurface> {
        self.surfaces.iter().find(|s| s.key == key)
    }
    /// The element that holds the keyboard focus: the cursor row of the focused
    /// surface, or the surface itself when it has no visible cursor row.
    pub(super) fn focus(&self) -> Option<Node> {
        let s = self.surfaces.iter().find(|s| s.focused)?;
        Some(match s.items.iter().find(|i| i.focused) {
            Some(i) => Node::Cell(s.key, i.key),
            None => Node::Surf(s.key),
        })
    }
    /// What clients cache: surfaces and their rows, by key.
    fn shape(&self) -> Vec<(u32, Vec<u64>)> {
        self.surfaces.iter().map(|s| (s.key, s.items.iter().map(|i| i.key).collect())).collect()
    }
    pub(super) fn exists(&self, n: Node) -> bool {
        match n {
            Node::Surf(s) => self.surface(s).is_some(),
            Node::Cell(s, k) => self.surface(s).is_some_and(|s| s.item(k).is_some()),
            _ => false,
        }
    }
}

/// What a client asked of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VAct {
    /// select only this row
    Select,
    AddToSelection,
    RemoveFromSelection,
    Expand,
    Collapse,
    /// open it (Enter)
    Invoke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VReq {
    pub surf: u32,
    pub key: u64,
    pub act: VAct,
}

static VREQS: Mutex<Vec<VReq>> = Mutex::new(Vec::new());

/// The newest view requests, oldest first (the UI thread runs them).
pub fn take_view_requests() -> Vec<VReq> {
    std::mem::take(&mut *lock(&VREQS))
}

fn invalid_op() -> Error {
    Error::from_hresult(windows::core::HRESULT(UIA_E_INVALIDOPERATION as i32))
}

impl Node {
    pub(super) fn is_view(self) -> bool {
        matches!(self, Node::Surf(_) | Node::Cell(..))
    }
}

impl Shared {
    pub(super) fn view(&self) -> Arc<ViewSnap> {
        lock(&self.view).clone()
    }

    fn post_view(&self, r: VReq) {
        lock(&VREQS).push(r);
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut c_void)), WM_APP_UIA, WPARAM(0), LPARAM(0)) };
    }

    /// The window's children in reading order: the view surfaces, then the open menu.
    pub(super) fn root_children(snap: &Snap, view: &ViewSnap) -> Vec<Node> {
        let mut v: Vec<Node> = view.surfaces.iter().map(|s| Node::Surf(s.key)).collect();
        if !snap.levels.is_empty() {
            v.push(Node::Level(0));
        }
        v
    }

    /// `navigate` over the whole tree: the menu's nodes as before, the root and
    /// the top level now with the view's surfaces next to them.
    pub(super) fn navigate_all(&self, snap: &Snap, view: &ViewSnap, n: Node, dir: NavigateDirection) -> Option<Node> {
        match n {
            Node::Root => {
                let kids = Self::root_children(snap, view);
                match dir {
                    NavigateDirection_FirstChild => kids.first().copied(),
                    NavigateDirection_LastChild => kids.last().copied(),
                    _ => None,
                }
            }
            Node::Level(0) if dir == NavigateDirection_NextSibling || dir == NavigateDirection_PreviousSibling => {
                let kids = Self::root_children(snap, view);
                Self::step(&kids, n, dir)
            }
            Node::Surf(s) => {
                let surf = view.surface(s)?;
                match dir {
                    NavigateDirection_Parent => Some(Node::Root),
                    NavigateDirection_FirstChild => surf.items.first().map(|i| Node::Cell(s, i.key)),
                    NavigateDirection_LastChild => surf.items.last().map(|i| Node::Cell(s, i.key)),
                    NavigateDirection_NextSibling | NavigateDirection_PreviousSibling => Self::step(&Self::root_children(snap, view), n, dir),
                    _ => None,
                }
            }
            Node::Cell(s, k) => {
                let surf = view.surface(s)?;
                let i = surf.items.iter().position(|i| i.key == k)?;
                match dir {
                    NavigateDirection_Parent => Some(Node::Surf(s)),
                    NavigateDirection_NextSibling => surf.items.get(i + 1).map(|i| Node::Cell(s, i.key)),
                    NavigateDirection_PreviousSibling => i.checked_sub(1).and_then(|j| surf.items.get(j)).map(|i| Node::Cell(s, i.key)),
                    _ => None,
                }
            }
            _ => self.navigate(snap, n, dir),
        }
    }

    fn step(kids: &[Node], n: Node, dir: NavigateDirection) -> Option<Node> {
        let i = kids.iter().position(|k| *k == n)?;
        if dir == NavigateDirection_NextSibling { kids.get(i + 1).copied() } else { i.checked_sub(1).and_then(|j| kids.get(j)).copied() }
    }
}

/// The UI thread's newest picture of the file views (empty: nothing shown or
/// nobody listens). Raises what a screen reader waits for: the rows changed,
/// the focus moved, a row was (de)selected or expanded.
pub fn publish_view(new: ViewSnap) {
    let Some(sh) = shared() else { return };
    let new = Arc::new(new);
    let old = std::mem::replace(&mut *lock(&sh.view), new.clone());
    if *old == *new {
        return;
    }
    // no lock is held below: UIA may call the providers while it raises
    let Ok(root) = sh.provider(Node::Root) else { return };
    if old.shape() != new.shape() {
        let mut id = Node::Root.runtime_id();
        let _ = unsafe { UiaRaiseStructureChangedEvent(&root, StructureChangeType_ChildrenInvalidated, id.as_mut_ptr(), id.len() as i32) };
    }
    // rows that stayed: selection and expand state
    for s in &new.surfaces {
        let Some(o) = old.surface(s.key) else { continue };
        for i in &s.items {
            let Some(oi) = o.item(i.key) else { continue };
            if oi.selected != i.selected {
                let ev = match (i.selected, s.multi) {
                    (true, false) => UIA_SelectionItem_ElementSelectedEventId,
                    (true, true) => UIA_SelectionItem_ElementAddedToSelectionEventId,
                    (false, _) => UIA_SelectionItem_ElementRemovedFromSelectionEventId,
                };
                if let Ok(p) = sh.provider(Node::Cell(s.key, i.key)) {
                    let _ = unsafe { UiaRaiseAutomationEvent(&p, ev) };
                }
            }
            if oi.expand != i.expand
                && let Ok(p) = sh.provider(Node::Cell(s.key, i.key))
            {
                let st = |e: Expand| VARIANT::from(expand_state(e).0);
                let _ = unsafe { UiaRaiseAutomationPropertyChangedEvent(&p, UIA_ExpandCollapseExpandCollapseStatePropertyId, &st(oi.expand), &st(i.expand)) };
            }
        }
    }
    if new.focus() != old.focus()
        && let Some(n) = new.focus()
        && let Ok(p) = sh.provider(n)
    {
        let _ = unsafe { UiaRaiseAutomationEvent(&p, UIA_AutomationFocusChangedEventId) };
    }
    sh.prune_view(&new);
}

impl Shared {
    /// Lets go of the providers of rows that scrolled away or went (the cache
    /// holds the visible rows only).
    pub(super) fn prune_view(&self, view: &ViewSnap) {
        let dead: Vec<Provider> = {
            let mut c = lock(&self.cache);
            let keys: Vec<Node> = c.keys().copied().filter(|n| n.is_view() && !view.exists(*n)).collect();
            keys.into_iter().filter_map(|k| c.remove(&k)).collect()
        };
        for p in dead {
            let _ = unsafe { UiaDisconnectProvider(&p.0) };
        }
    }
}

fn expand_state(e: Expand) -> ExpandCollapseState {
    match e {
        Expand::Leaf => ExpandCollapseState_LeafNode,
        Expand::Collapsed => ExpandCollapseState_Collapsed,
        Expand::Expanded => ExpandCollapseState_Expanded,
    }
}

fn unknown_array(items: &[IRawElementProviderSimple]) -> Result<*mut SAFEARRAY> {
    unsafe {
        let sa = SafeArrayCreateVector(VT_UNKNOWN, 0, items.len() as u32);
        if sa.is_null() {
            return Err(E_OUTOFMEMORY.into());
        }
        for (i, p) in items.iter().enumerate() {
            SafeArrayPutElement(sa, &(i as i32), p.as_raw() as *const c_void)?;
        }
        Ok(sa)
    }
}

// ---- the providers' side ----------------------------------------------------------

impl Item_Impl {
    fn surf_key(&self) -> u32 {
        match self.node {
            Node::Surf(s) | Node::Cell(s, _) => s,
            _ => 0,
        }
    }

    /// The surface this node is or belongs to.
    pub(super) fn surface(&self, view: &ViewSnap) -> Result<VSurface> {
        view.surface(self.surf_key()).cloned().ok_or_else(gone)
    }

    pub(super) fn cell(&self, view: &ViewSnap) -> Result<VItem> {
        match self.node {
            Node::Cell(s, k) => view.surface(s).and_then(|s| s.item(k)).cloned().ok_or_else(gone),
            _ => Err(gone()),
        }
    }

    pub(super) fn view_rect(&self, view: &ViewSnap) -> Result<Px> {
        match self.node {
            Node::Surf(_) => self.surface(view).map(|s| s.rect),
            _ => self.cell(view).map(|c| c.rect),
        }
    }

    pub(super) fn view_pattern(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        let view = self.sh.view();
        let me = self.to_object();
        match self.node {
            Node::Surf(_) => {
                self.surface(&view)?;
                match id {
                    UIA_SelectionPatternId => me.to_interface::<ISelectionProvider>().cast(),
                    _ => Err(none()),
                }
            }
            _ => {
                let c = self.cell(&view)?;
                match id {
                    UIA_SelectionItemPatternId if c.selectable => me.to_interface::<ISelectionItemProvider>().cast(),
                    UIA_InvokePatternId if c.invokable => me.to_interface::<IInvokeProvider>().cast(),
                    UIA_ExpandCollapsePatternId if c.expand != Expand::Leaf => me.to_interface::<IExpandCollapseProvider>().cast(),
                    UIA_ValuePatternId if !c.value.is_empty() => me.to_interface::<IValueProvider>().cast(),
                    _ => Err(none()),
                }
            }
        }
    }

    pub(super) fn view_property(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let view = self.sh.view();
        match self.node {
            Node::Surf(k) => {
                let s = self.surface(&view)?;
                let item_focus = s.items.iter().any(|i| i.focused);
                Ok(match id {
                    UIA_ControlTypePropertyId => VARIANT::from(if s.tree { UIA_TreeControlTypeId.0 } else { UIA_ListControlTypeId.0 }),
                    UIA_NamePropertyId => variant_str(&s.name),
                    UIA_IsEnabledPropertyId | UIA_IsKeyboardFocusablePropertyId => variant_bool(true),
                    UIA_HasKeyboardFocusPropertyId => variant_bool(s.focused && !item_focus),
                    UIA_IsOffscreenPropertyId => variant_bool(false),
                    UIA_OrientationPropertyId => VARIANT::from(OrientationType_Vertical.0),
                    UIA_ClassNamePropertyId => variant_str(if s.tree { "TsubameTree" } else { "TsubameList" }),
                    UIA_AutomationIdPropertyId => variant_str(&format!("view.{k}")),
                    UIA_FullDescriptionPropertyId | UIA_HelpTextPropertyId if !s.desc.is_empty() => variant_str(&s.desc),
                    _ => VARIANT::default(),
                })
            }
            Node::Cell(sk, k) => {
                let s = self.surface(&view)?;
                let c = self.cell(&view)?;
                Ok(match id {
                    UIA_ControlTypePropertyId => VARIANT::from(if c.group {
                        UIA_GroupControlTypeId.0
                    } else if s.tree {
                        UIA_TreeItemControlTypeId.0
                    } else {
                        UIA_ListItemControlTypeId.0
                    }),
                    UIA_NamePropertyId => variant_str(&c.name),
                    UIA_IsEnabledPropertyId | UIA_IsKeyboardFocusablePropertyId => variant_bool(true),
                    UIA_HasKeyboardFocusPropertyId => variant_bool(c.focused),
                    UIA_IsOffscreenPropertyId => variant_bool(false),
                    UIA_ClassNamePropertyId => variant_str(if c.group {
                        "TsubameGroupHeader"
                    } else if s.tree {
                        "TsubameTreeItem"
                    } else {
                        "TsubameListItem"
                    }),
                    UIA_AutomationIdPropertyId => variant_str(&format!("view.{sk}.{k:x}")),
                    UIA_LevelPropertyId if c.level > 0 => VARIANT::from(c.level as i32),
                    UIA_PositionInSetPropertyId if c.pos > 0 => VARIANT::from(c.pos as i32),
                    UIA_SizeOfSetPropertyId if s.size > 0 => VARIANT::from(s.size as i32),
                    UIA_ItemTypePropertyId if !c.kind.is_empty() => variant_str(&c.kind),
                    UIA_FullDescriptionPropertyId if !c.value.is_empty() => variant_str(&c.value),
                    _ => VARIANT::default(),
                })
            }
            _ => Err(gone()),
        }
    }

    /// Queues a request for the UI thread; refuses a row that is gone.
    fn ask(&self, act: VAct) -> Result<()> {
        let view = self.sh.view();
        self.cell(&view)?;
        if let Node::Cell(surf, key) = self.node {
            self.sh.post_view(VReq { surf, key, act });
        }
        Ok(())
    }

    pub(super) fn view_invoke(&self) -> Result<()> {
        let c = self.cell(&self.sh.view())?;
        if !c.invokable {
            return Err(Error::from_hresult(windows::core::HRESULT(UIA_E_NOTSUPPORTED as i32)));
        }
        self.ask(VAct::Invoke)
    }

    pub(super) fn view_expand(&self, open: bool) -> Result<()> {
        let c = self.cell(&self.sh.view())?;
        match (c.expand, open) {
            (Expand::Leaf, _) => Err(Error::from_hresult(windows::core::HRESULT(UIA_E_NOTSUPPORTED as i32))),
            (Expand::Expanded, true) | (Expand::Collapsed, false) => Ok(()),
            (_, true) => self.ask(VAct::Expand),
            (_, false) => self.ask(VAct::Collapse),
        }
    }

    pub(super) fn view_expand_state(&self) -> Result<ExpandCollapseState> {
        Ok(expand_state(self.cell(&self.sh.view())?.expand))
    }

    pub(super) fn view_value(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.cell(&self.sh.view())?.value.as_str()))
    }
}

impl ISelectionProvider_Impl for Item_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let view = self.sh.view();
        let s = self.surface(&view)?;
        let mut picked = Vec::new();
        for i in s.items.iter().filter(|i| i.selected) {
            picked.push(self.sh.provider(Node::Cell(s.key, i.key))?);
        }
        unknown_array(&picked)
    }
    fn CanSelectMultiple(&self) -> Result<BOOL> {
        Ok(BOOL(self.surface(&self.sh.view())?.multi as i32))
    }
    fn IsSelectionRequired(&self) -> Result<BOOL> {
        Ok(BOOL(0))
    }
}

impl ISelectionItemProvider_Impl for Item_Impl {
    fn Select(&self) -> Result<()> {
        self.ask(VAct::Select)
    }
    fn AddToSelection(&self) -> Result<()> {
        let view = self.sh.view();
        let c = self.cell(&view)?;
        // a single-selection list can only move its selection
        if !self.surface(&view)?.multi && !c.selected {
            return self.ask(VAct::Select);
        }
        self.ask(VAct::AddToSelection)
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        let view = self.sh.view();
        self.cell(&view)?;
        if !self.surface(&view)?.multi {
            return Err(invalid_op());
        }
        self.ask(VAct::RemoveFromSelection)
    }
    fn IsSelected(&self) -> Result<BOOL> {
        Ok(BOOL(self.cell(&self.sh.view())?.selected as i32))
    }
    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        let view = self.sh.view();
        self.surface(&view)?;
        self.sh.provider(Node::Surf(self.surf_key()))
    }
}

impl Shared {
    /// The view row or surface under a point (client DIPs), topmost last.
    pub(super) fn view_from_point(view: &ViewSnap, px: f32, py: f32) -> Option<Node> {
        let inside = |r: &Px| px >= r[0] && px < r[0] + r[2] && py >= r[1] && py < r[1] + r[3];
        for s in view.surfaces.iter().rev() {
            if !inside(&s.rect) {
                continue;
            }
            return Some(match s.items.iter().find(|i| inside(&i.rect)) {
                Some(i) => Node::Cell(s.key, i.key),
                None => Node::Surf(s.key),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: u64, name: &str, y: f32) -> VItem {
        VItem { key, name: name.into(), rect: [0.0, y, 100.0, 22.0], selectable: true, invokable: true, pos: key as u32, ..Default::default() }
    }

    fn view() -> ViewSnap {
        let mut b = item(11, "b.txt", 22.0);
        b.selected = true;
        b.focused = true;
        ViewSnap {
            surfaces: vec![
                VSurface { key: 1, name: "Docs".into(), rect: [0.0, 0.0, 100.0, 100.0], focused: true, multi: true, size: 500, items: vec![item(10, "a.txt", 0.0), b, item(12, "c.txt", 44.0)], ..Default::default() },
                VSurface { key: 2, tree: true, name: "Folders".into(), rect: [100.0, 0.0, 50.0, 100.0], size: 2, items: vec![VItem { rect: [100.0, 0.0, 50.0, 22.0], ..item(20, "x", 0.0) }], ..Default::default() },
            ],
        }
    }

    fn sh() -> Shared {
        Shared { hwnd: 0, snap: Mutex::new(Arc::new(Snap::default())), view: Mutex::new(Arc::new(ViewSnap::default())), cache: Mutex::new(HashMap::new()), root: Mutex::new(None) }
    }

    fn menu() -> Snap {
        Snap { levels: vec![Level { key: 0, name: "Context menu".into(), rect: [0.0; 4], rows: vec![Row { key: 1, name: "Open".into(), ..Default::default() }], filter: None }] }
    }

    #[test]
    fn the_root_lists_surfaces_then_the_open_menu() {
        let (sh, v) = (sh(), view());
        let nav = |s: &Snap, n, d| sh.navigate_all(s, &v, n, d);
        let none = Snap::default();
        assert_eq!(nav(&none, Node::Root, NavigateDirection_FirstChild), Some(Node::Surf(1)));
        assert_eq!(nav(&none, Node::Root, NavigateDirection_LastChild), Some(Node::Surf(2)));
        assert_eq!(nav(&none, Node::Surf(1), NavigateDirection_NextSibling), Some(Node::Surf(2)));
        assert_eq!(nav(&none, Node::Surf(2), NavigateDirection_NextSibling), None);
        assert_eq!(nav(&none, Node::Surf(1), NavigateDirection_Parent), Some(Node::Root));
        let m = menu();
        assert_eq!(nav(&m, Node::Root, NavigateDirection_LastChild), Some(Node::Level(0)));
        assert_eq!(nav(&m, Node::Surf(2), NavigateDirection_NextSibling), Some(Node::Level(0)));
        assert_eq!(nav(&m, Node::Level(0), NavigateDirection_PreviousSibling), Some(Node::Surf(2)));
        assert_eq!(nav(&m, Node::Level(0), NavigateDirection_NextSibling), None);
        assert_eq!(nav(&m, Node::Level(0), NavigateDirection_Parent), Some(Node::Root));
        // no view: the menu is the only child, as before
        let empty = ViewSnap::default();
        assert_eq!(sh.navigate_all(&m, &empty, Node::Root, NavigateDirection_FirstChild), Some(Node::Level(0)));
        assert_eq!(sh.navigate_all(&none, &empty, Node::Root, NavigateDirection_FirstChild), None);
    }

    #[test]
    fn rows_walk_within_their_surface() {
        let (sh, v) = (sh(), view());
        let none = Snap::default();
        let nav = |n, d| sh.navigate_all(&none, &v, n, d);
        assert_eq!(nav(Node::Surf(1), NavigateDirection_FirstChild), Some(Node::Cell(1, 10)));
        assert_eq!(nav(Node::Surf(1), NavigateDirection_LastChild), Some(Node::Cell(1, 12)));
        assert_eq!(nav(Node::Cell(1, 10), NavigateDirection_NextSibling), Some(Node::Cell(1, 11)));
        assert_eq!(nav(Node::Cell(1, 10), NavigateDirection_PreviousSibling), None);
        assert_eq!(nav(Node::Cell(1, 12), NavigateDirection_NextSibling), None);
        assert_eq!(nav(Node::Cell(1, 11), NavigateDirection_Parent), Some(Node::Surf(1)));
        assert_eq!(nav(Node::Cell(1, 11), NavigateDirection_FirstChild), None);
        // a row that scrolled away is not there
        assert_eq!(nav(Node::Cell(1, 99), NavigateDirection_NextSibling), None);
        assert!(!v.exists(Node::Cell(1, 99)));
        assert!(!v.exists(Node::Surf(7)));
        assert!(v.exists(Node::Cell(2, 20)));
    }

    #[test]
    fn focus_is_the_cursor_row_or_the_surface() {
        let mut v = view();
        assert_eq!(v.focus(), Some(Node::Cell(1, 11)));
        v.surfaces[0].items[1].focused = false;
        assert_eq!(v.focus(), Some(Node::Surf(1)));
        v.surfaces[0].focused = false;
        assert_eq!(v.focus(), None);
    }

    #[test]
    fn runtime_ids_differ_between_surfaces_rows_and_menu_nodes() {
        let ids = [Node::Surf(1).runtime_id(), Node::Surf(2).runtime_id(), Node::Cell(1, 10).runtime_id(), Node::Cell(2, 10).runtime_id(), Node::Cell(1, 11).runtime_id(), Node::Level(1).runtime_id(), Node::Row(1, 10).runtime_id()];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
        // a 64 bit key survives in the id
        assert_ne!(Node::Cell(1, 1).runtime_id(), Node::Cell(1, 1 << 32).runtime_id());
    }

    #[test]
    fn shape_ignores_selection_and_geometry_but_not_rows() {
        let a = view();
        let mut b = a.clone();
        b.surfaces[0].items[0].selected = true;
        b.surfaces[0].items[0].rect = [9.0; 4];
        assert_eq!(a.shape(), b.shape());
        b.surfaces[0].items.pop();
        assert_ne!(a.shape(), b.shape());
    }

    #[test]
    fn a_point_finds_the_row_then_the_surface() {
        let v = view();
        assert_eq!(Shared::view_from_point(&v, 10.0, 30.0), Some(Node::Cell(1, 11)));
        assert_eq!(Shared::view_from_point(&v, 10.0, 80.0), Some(Node::Surf(1)));
        assert_eq!(Shared::view_from_point(&v, 120.0, 10.0), Some(Node::Cell(2, 20)));
        assert_eq!(Shared::view_from_point(&v, 400.0, 10.0), None);
    }

    #[test]
    fn expand_states_map_to_uia() {
        assert_eq!(expand_state(Expand::Leaf), ExpandCollapseState_LeafNode);
        assert_eq!(expand_state(Expand::Collapsed), ExpandCollapseState_Collapsed);
        assert_eq!(expand_state(Expand::Expanded), ExpandCollapseState_Expanded);
    }

    #[test]
    fn providers_answer_properties_patterns_and_queue_requests() {
        let sh = Arc::new(sh());
        // the module-level provider() needs the shared state registered for the
        // events; properties and patterns only need the snapshot
        *lock(&sh.view) = Arc::new(view());
        let mut tree_item = item(21, "sub", 22.0);
        tree_item.level = 2;
        tree_item.expand = Expand::Collapsed;
        tree_item.value = "Folder, 2026-11-25".into();
        let mut v = (*sh.view()).clone();
        v.surfaces[1].items.push(tree_item);
        *lock(&sh.view) = Arc::new(v);

        let p = sh.provider(Node::Cell(1, 11)).unwrap();
        let get = |p: &IRawElementProviderSimple, id| unsafe { p.GetPropertyValue(id).unwrap() };
        assert_eq!(get(&p, UIA_NamePropertyId), variant_str("b.txt"));
        assert_eq!(get(&p, UIA_ControlTypePropertyId), VARIANT::from(UIA_ListItemControlTypeId.0));
        assert_eq!(get(&p, UIA_SizeOfSetPropertyId), VARIANT::from(500));
        assert_eq!(get(&p, UIA_PositionInSetPropertyId), VARIANT::from(11));
        assert_eq!(get(&p, UIA_HasKeyboardFocusPropertyId), variant_bool(true));
        let sel: ISelectionItemProvider = unsafe { p.GetPatternProvider(UIA_SelectionItemPatternId).unwrap().cast().unwrap() };
        assert!(unsafe { sel.IsSelected().unwrap() }.as_bool());
        // a file row has no ExpandCollapse
        assert!(unsafe { p.GetPatternProvider(UIA_ExpandCollapsePatternId) }.is_err());

        let t = sh.provider(Node::Cell(2, 21)).unwrap();
        assert_eq!(get(&t, UIA_ControlTypePropertyId), VARIANT::from(UIA_TreeItemControlTypeId.0));
        assert_eq!(get(&t, UIA_LevelPropertyId), VARIANT::from(2));
        let ec: IExpandCollapseProvider = unsafe { t.GetPatternProvider(UIA_ExpandCollapsePatternId).unwrap().cast().unwrap() };
        assert_eq!(unsafe { ec.ExpandCollapseState().unwrap() }, ExpandCollapseState_Collapsed);
        let value: IValueProvider = unsafe { t.GetPatternProvider(UIA_ValuePatternId).unwrap().cast().unwrap() };
        assert_eq!(unsafe { value.Value().unwrap() }.to_string(), "Folder, 2026-11-25");
        assert!(unsafe { value.IsReadOnly().unwrap() }.as_bool());

        let list = sh.provider(Node::Surf(1)).unwrap();
        assert_eq!(get(&list, UIA_ControlTypePropertyId), VARIANT::from(UIA_ListControlTypeId.0));
        let tree = sh.provider(Node::Surf(2)).unwrap();
        assert_eq!(get(&tree, UIA_ControlTypePropertyId), VARIANT::from(UIA_TreeControlTypeId.0));
        let selection: ISelectionProvider = unsafe { list.GetPatternProvider(UIA_SelectionPatternId).unwrap().cast().unwrap() };
        assert!(unsafe { selection.CanSelectMultiple().unwrap() }.as_bool());
        let arr = unsafe { selection.GetSelection().unwrap() };
        assert!(!arr.is_null());
        unsafe { windows::Win32::System::Ole::SafeArrayDestroy(arr).unwrap() };

        // requests: queued with the row's identity (no window: the post is a no-op)
        take_view_requests();
        unsafe { ec.Expand().unwrap() };
        unsafe { ec.Collapse().unwrap() };
        let inv: IInvokeProvider = unsafe { p.GetPatternProvider(UIA_InvokePatternId).unwrap().cast().unwrap() };
        unsafe { inv.Invoke().unwrap() };
        unsafe { sel.Select().unwrap() };
        let reqs = take_view_requests();
        // the row is collapsed already: Collapse is a no-op, Expand is queued
        assert_eq!(reqs, vec![VReq { surf: 2, key: 21, act: VAct::Expand }, VReq { surf: 1, key: 11, act: VAct::Invoke }, VReq { surf: 1, key: 11, act: VAct::Select }]);

        // a row that went away answers "not available"
        *lock(&sh.view) = Arc::new(ViewSnap::default());
        assert!(unsafe { p.GetPropertyValue(UIA_NamePropertyId) }.is_err());
        assert!(unsafe { sel.Select() }.is_err());
    }
}
