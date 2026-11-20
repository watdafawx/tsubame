//! UI Automation provider for the own-drawn context menu (CM10, `docs/ctxmenu.md`).
//!
//! The menu is a widget painted inside the main window, so Windows sees one
//! window and no rows. A screen reader asks the window for its UIA provider
//! (`WM_GETOBJECT`, `UiaRootObjectId`); [`on_get_object`] answers with a
//! fragment root. While a menu is open its levels and rows are children of
//! that root (Menu, MenuItem, the filter field as Edit); closed, the root has
//! no children.
//!
//! Threading. UIA calls the providers on its own threads. They never see the
//! widget: the UI thread copies what the menu looks like into a [`Snap`]
//! ([`publish`], once per changed frame) and the providers read the newest one
//! behind a mutex. A row the UI thread has since dropped answers
//! `UIA_E_ELEMENTNOTAVAILABLE`. Requests (Invoke, Collapse) are queued and
//! the UI thread is woken with [`WM_APP_UIA`]; the menu runs them on its next
//! frame as the keyboard would.
//!
//! Cost. Nothing exists until a client has asked for the provider once
//! (`ACTIVE`); after that the app also asks [`active`] (is a client really
//! listening) before it builds a snapshot. A menu without a listener builds,
//! allocates and publishes nothing, and a closed menu does no UIA work.
//! Providers are created on demand, kept in a small cache while the menu is
//! open (stable identity) and disconnected with `UiaDisconnectProvider` when
//! it closes, so no COM object outlives its menu.

// the windows crate names its constants like the C headers; matching on them reads best as they are
#![allow(non_upper_case_globals)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use windows::Win32::Foundation::{E_OUTOFMEMORY, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{BOOL, BSTR, Error, IUnknown, IUnknownImpl, Interface, PCWSTR, Result, implement};

/// Posted to the main window when a request is waiting ([`take_requests`]).
pub const WM_APP_UIA: u32 = WM_APP + 10;

/// A rectangle in client-area DIPs: x, y, w, h (scaled to pixels when asked).
pub type Px = [f32; 4];

/// One row as the provider reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub key: u64,
    pub name: String,
    /// "Alt+O" (empty: none)
    pub access: String,
    /// the shortcut text ("Ctrl+C")
    pub shortcut: String,
    pub rect: Px,
    pub enabled: bool,
    /// a checked or selected-radio row
    pub toggled: bool,
    pub submenu: bool,
    pub expanded: bool,
    pub focused: bool,
    pub visible: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Level {
    /// key of the row above that opened it (0: the top level)
    pub key: u64,
    pub name: String,
    pub rect: Px,
    pub rows: Vec<Row>,
    /// the type-to-filter field: rect and text
    pub filter: Option<(Px, String)>,
}

/// The open menu. No levels: no menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    pub levels: Vec<Level>,
}

impl Snap {
    /// The keyboard focus: (level, row key).
    pub fn focus(&self) -> Option<(usize, u64)> {
        self.levels.iter().enumerate().find_map(|(i, l)| l.rows.iter().find(|r| r.focused).map(|r| (i, r.key)))
    }
    /// What clients cache: the levels and their rows, by key.
    fn shape(&self) -> Vec<(u64, Vec<u64>, bool)> {
        self.levels.iter().map(|l| (l.key, l.rows.iter().map(|r| r.key).collect(), l.filter.is_some())).collect()
    }
}

/// A screen reader's request on a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Req {
    pub lv: usize,
    pub key: u64,
    /// false: invoke (what Enter does); true: collapse its submenu
    pub collapse: bool,
}

// ---- shared state ------------------------------------------------------------

struct Shared {
    hwnd: isize,
    snap: Mutex<Arc<Snap>>,
    cache: Mutex<HashMap<Node, Provider>>,
    root: Mutex<Option<Provider>>,
}

/// Providers are free-threaded (UIA calls them on its own threads), so the
/// interface pointers may sit in a shared map.
struct Provider(IRawElementProviderSimple);
unsafe impl Send for Provider {}
unsafe impl Sync for Provider {}

static SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static REQUESTS: Mutex<Vec<Req>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn shared() -> Option<Arc<Shared>> {
    lock(&SHARED).clone()
}

/// A screen reader has asked this process for a provider at least once.
pub fn asked() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Build and publish snapshots: a client asked for the provider and one is
/// listening now. One atomic load until a client has asked.
pub fn active() -> bool {
    asked() && unsafe { UiaClientsAreListening() }.as_bool()
}

/// The newest requests, oldest first (the UI thread runs them).
pub fn take_requests() -> Vec<Req> {
    std::mem::take(&mut *lock(&REQUESTS))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Node {
    Root,
    Level(usize),
    Row(usize, u64),
    Filter,
}

impl Node {
    fn runtime_id(self) -> [i32; 5] {
        let (kind, lv, key) = match self {
            Node::Root => (0, 0, 0),
            Node::Level(l) => (1, l as i32, 0),
            Node::Row(l, k) => (2, l as i32, k),
            Node::Filter => (3, 0, 0),
        };
        [UiaAppendRuntimeId as i32, kind, lv, (key >> 32) as i32, key as i32]
    }
}

fn gone() -> Error {
    Error::from_hresult(windows::core::HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}

/// "No such element": S_OK with a null out pointer, which UIA reads as none.
fn none() -> Error {
    Error::empty()
}

impl Shared {
    fn snap(&self) -> Arc<Snap> {
        lock(&self.snap).clone()
    }

    fn rect(&self, r: Px) -> UiaRect {
        let hwnd = HWND(self.hwnd as *mut c_void);
        // the window's scale and origin now (it may have moved since the snapshot)
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let s = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let mut p = POINT { x: (r[0] * s).round() as i32, y: (r[1] * s).round() as i32 };
        let _ = unsafe { ClientToScreen(hwnd, &mut p) };
        UiaRect { left: p.x as f64, top: p.y as f64, width: (r[2] * s).round() as f64, height: (r[3] * s).round() as f64 }
    }

    /// The provider of `node` (made once per open menu).
    fn provider(self: &Arc<Self>, node: Node) -> Result<IRawElementProviderSimple> {
        if node == Node::Root {
            return lock(&self.root).as_ref().map(|p| p.0.clone()).ok_or_else(gone);
        }
        let mut c = lock(&self.cache);
        if let Some(p) = c.get(&node) {
            return Ok(p.0.clone());
        }
        let p: IRawElementProviderSimple = Item { sh: self.clone(), node }.into();
        c.insert(node, Provider(p.clone()));
        Ok(p)
    }

    fn fragment(self: &Arc<Self>, node: Node) -> Result<IRawElementProviderFragment> {
        self.provider(node)?.cast()
    }

    /// Children of a level in reading order: the filter field, then the rows.
    fn children(snap: &Snap, lv: usize) -> Vec<Node> {
        let Some(l) = snap.levels.get(lv) else { return Vec::new() };
        let mut v = Vec::with_capacity(l.rows.len() + 1);
        if l.filter.is_some() {
            v.push(Node::Filter);
        }
        v.extend(l.rows.iter().map(|r| Node::Row(lv, r.key)));
        v
    }

    fn row(snap: &Snap, lv: usize, key: u64) -> Option<&Row> {
        snap.levels.get(lv)?.rows.iter().find(|r| r.key == key)
    }

    fn exists(snap: &Snap, n: Node) -> bool {
        match n {
            Node::Root => true,
            Node::Level(l) => l < snap.levels.len(),
            Node::Row(l, k) => Self::row(snap, l, k).is_some(),
            Node::Filter => snap.levels.first().is_some_and(|l| l.filter.is_some()),
        }
    }

    fn level_of(n: Node) -> usize {
        match n {
            Node::Row(l, _) | Node::Level(l) => l,
            _ => 0,
        }
    }

    fn navigate(&self, snap: &Snap, n: Node, dir: NavigateDirection) -> Option<Node> {
        let sibling = |step: isize| {
            let lv = Self::level_of(n);
            let kids = Self::children(snap, lv);
            let i = kids.iter().position(|k| *k == n)? as isize + step;
            (i >= 0).then(|| kids.get(i as usize).copied()).flatten()
        };
        match (n, dir) {
            (Node::Root, NavigateDirection_FirstChild | NavigateDirection_LastChild) => (!snap.levels.is_empty()).then_some(Node::Level(0)),
            (Node::Root, _) => None,
            (Node::Level(0), NavigateDirection_Parent) => Some(Node::Root),
            (Node::Level(l), NavigateDirection_Parent) => snap.levels.get(l).map(|lv| Node::Row(l - 1, lv.key)),
            (Node::Level(l), NavigateDirection_FirstChild) => Self::children(snap, l).first().copied(),
            (Node::Level(l), NavigateDirection_LastChild) => Self::children(snap, l).last().copied(),
            (Node::Level(_), _) => None,
            (Node::Filter, NavigateDirection_Parent) => Some(Node::Level(0)),
            (Node::Row(l, _), NavigateDirection_Parent) => Some(Node::Level(l)),
            (Node::Row(l, k), NavigateDirection_FirstChild | NavigateDirection_LastChild) => {
                let sub = snap.levels.get(l + 1)?;
                (sub.key == k && Self::row(snap, l, k)?.expanded).then_some(Node::Level(l + 1))
            }
            (_, NavigateDirection_NextSibling) => sibling(1),
            (_, NavigateDirection_PreviousSibling) => sibling(-1),
            _ => None,
        }
    }

    fn post(&self, r: Req) {
        lock(&REQUESTS).push(r);
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut c_void)), WM_APP_UIA, WPARAM(0), LPARAM(0)) };
    }

    /// Releases UIA's hold on every provider made for the menu that closed.
    fn disconnect_items(&self) {
        let items: Vec<Provider> = lock(&self.cache).drain().map(|(_, p)| p).collect();
        for p in items {
            let _ = unsafe { UiaDisconnectProvider(&p.0) };
        }
    }
}

// ---- the window's side -------------------------------------------------------

/// `WM_GETOBJECT`: the provider for `UiaRootObjectId`, None for any other id
/// (the caller falls through to `DefWindowProc`). The first call is the point
/// where the app starts to publish snapshots, so the caller repaints.
pub fn on_get_object(hwnd: HWND, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if lp.0 as i32 != UiaRootObjectId {
        return None;
    }
    let sh = {
        let mut g = lock(&SHARED);
        match g.as_ref() {
            Some(s) if s.hwnd == hwnd.0 as isize => s.clone(),
            _ => {
                let s = Arc::new(Shared { hwnd: hwnd.0 as isize, snap: Mutex::new(Arc::new(Snap::default())), cache: Mutex::new(HashMap::new()), root: Mutex::new(None) });
                *g = Some(s.clone());
                s
            }
        }
    };
    let root = {
        let mut r = lock(&sh.root);
        if r.is_none() {
            *r = Some(Provider(RootProv { sh: sh.clone() }.into()));
        }
        r.as_ref().map(|p| p.0.clone())?
    };
    ACTIVE.store(true, Ordering::Relaxed);
    Some(unsafe { UiaReturnRawElementProvider(hwnd, wp, lp, &root) })
}

/// The window is going away: UIA lets go of everything handed out.
pub fn shutdown(hwnd: HWND) {
    if let Some(sh) = lock(&SHARED).take() {
        *lock(&sh.snap) = Arc::new(Snap::default());
        sh.disconnect_items();
        if let Some(r) = lock(&sh.root).take() {
            let _ = unsafe { UiaDisconnectProvider(&r.0) };
        }
        let _ = unsafe { UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None::<&IRawElementProviderSimple>) };
    }
}

/// The UI thread's newest picture of the menu (empty `levels`: closed).
/// Raises the events a screen reader waits for: the menu opened or closed,
/// its rows changed, the focus moved.
pub fn publish(new: Snap) {
    let Some(sh) = shared() else { return };
    let new = Arc::new(new);
    let old = std::mem::replace(&mut *lock(&sh.snap), new.clone());
    if *old == *new {
        return;
    }
    // no lock is held below: UIA may call the providers while it raises
    let (was, is) = (!old.levels.is_empty(), !new.levels.is_empty());
    if let Ok(root) = sh.provider(Node::Root) {
        if was != is || old.shape() != new.shape() {
            let mut id = Node::Root.runtime_id();
            let _ = unsafe { UiaRaiseStructureChangedEvent(&root, StructureChangeType_ChildrenInvalidated, id.as_mut_ptr(), id.len() as i32) };
        }
        if was && !is {
            let _ = unsafe { UiaRaiseAutomationEvent(&root, UIA_MenuClosedEventId) };
        }
    }
    if !was && is && let Ok(top) = sh.provider(Node::Level(0)) {
        let _ = unsafe { UiaRaiseAutomationEvent(&top, UIA_MenuOpenedEventId) };
    }
    if is && new.focus() != old.focus() && let Some((l, k)) = new.focus() && let Ok(p) = sh.provider(Node::Row(l, k)) {
        let _ = unsafe { UiaRaiseAutomationEvent(&p, UIA_AutomationFocusChangedEventId) };
    }
    if was && !is {
        sh.disconnect_items();
    }
}

fn variant_bool(b: bool) -> VARIANT {
    VARIANT::from(b)
}

fn variant_str(s: &str) -> VARIANT {
    VARIANT::from(BSTR::from(s))
}

fn runtime_array(parts: &[i32]) -> Result<*mut SAFEARRAY> {
    unsafe {
        let sa = SafeArrayCreateVector(VT_I4, 0, parts.len() as u32);
        if sa.is_null() {
            return Err(E_OUTOFMEMORY.into());
        }
        for (i, p) in parts.iter().enumerate() {
            SafeArrayPutElement(sa, &(i as i32), p as *const i32 as *const c_void)?;
        }
        Ok(sa)
    }
}

// ---- the root: the window ----------------------------------------------------

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IRawElementProviderFragmentRoot)]
struct RootProv {
    sh: Arc<Shared>,
}

impl IRawElementProviderSimple_Impl for RootProv_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> Result<IUnknown> {
        Err(none())
    }
    fn GetPropertyValue(&self, _: UIA_PROPERTY_ID) -> Result<VARIANT> {
        // name, control type and the rest come from the window itself
        Ok(VARIANT::default())
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        unsafe { UiaHostProviderFromHwnd(HWND(self.sh.hwnd as *mut c_void)) }
    }
}

impl IRawElementProviderFragment_Impl for RootProv_Impl {
    fn Navigate(&self, dir: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let snap = self.sh.snap();
        match self.sh.navigate(&snap, Node::Root, dir) {
            Some(n) => self.sh.fragment(n),
            None => Err(none()),
        }
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        // the window supplies its own runtime id
        Err(none())
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(UiaRect::default())
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Err(none())
    }
    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        self.sh.provider(Node::Root)?.cast()
    }
}

impl IRawElementProviderFragmentRoot_Impl for RootProv_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let snap = self.sh.snap();
        let hwnd = HWND(self.sh.hwnd as *mut c_void);
        let mut p = POINT { x: x.round() as i32, y: y.round() as i32 };
        let _ = unsafe { ScreenToClient(hwnd, &mut p) };
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let s = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let (px, py) = (p.x as f32 / s, p.y as f32 / s);
        let inside = |r: &Px| px >= r[0] && px < r[0] + r[2] && py >= r[1] && py < r[1] + r[3];
        // the deepest level under the point, then its row
        for (lv, l) in snap.levels.iter().enumerate().rev() {
            if !inside(&l.rect) {
                continue;
            }
            if let Some(r) = l.rows.iter().find(|r| r.visible && inside(&r.rect)) {
                return self.sh.fragment(Node::Row(lv, r.key));
            }
            if l.filter.as_ref().is_some_and(|f| inside(&f.0)) {
                return self.sh.fragment(Node::Filter);
            }
            return self.sh.fragment(Node::Level(lv));
        }
        Err(none())
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        match self.sh.snap().focus() {
            Some((l, k)) => self.sh.fragment(Node::Row(l, k)),
            None => Err(none()),
        }
    }
}

// ---- levels, rows and the filter field -----------------------------------------

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IInvokeProvider, IToggleProvider, IExpandCollapseProvider, IValueProvider)]
struct Item {
    sh: Arc<Shared>,
    node: Node,
}

impl Item_Impl {
    fn row(&self, snap: &Snap) -> Result<Row> {
        match self.node {
            Node::Row(l, k) => Shared::row(snap, l, k).cloned().ok_or_else(gone),
            _ => Err(gone()),
        }
    }
    fn alive(&self, snap: &Snap) -> Result<()> {
        if Shared::exists(snap, self.node) { Ok(()) } else { Err(gone()) }
    }
    fn rect(&self, snap: &Snap) -> Result<Px> {
        match self.node {
            Node::Level(l) => snap.levels.get(l).map(|l| l.rect).ok_or_else(gone),
            Node::Row(..) => self.row(snap).map(|r| r.rect),
            Node::Filter => snap.levels.first().and_then(|l| l.filter.as_ref()).map(|f| f.0).ok_or_else(gone),
            Node::Root => Err(gone()),
        }
    }
    /// Runs the row like Enter would; refuses a disabled row.
    fn invoke(&self) -> Result<()> {
        let snap = self.sh.snap();
        let row = self.row(&snap)?;
        if !row.enabled {
            return Err(Error::from_hresult(windows::core::HRESULT(UIA_E_ELEMENTNOTENABLED as i32)));
        }
        if let Node::Row(lv, key) = self.node {
            self.sh.post(Req { lv, key, collapse: false });
        }
        Ok(())
    }
}

impl IRawElementProviderSimple_Impl for Item_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        let snap = self.sh.snap();
        self.alive(&snap)?;
        let me = self.to_object();
        match (self.node, id) {
            (Node::Row(..), UIA_InvokePatternId) => me.to_interface::<IInvokeProvider>().cast(),
            (Node::Row(..), UIA_TogglePatternId) if self.row(&snap)?.toggled => me.to_interface::<IToggleProvider>().cast(),
            (Node::Row(..), UIA_ExpandCollapsePatternId) if self.row(&snap)?.submenu => me.to_interface::<IExpandCollapseProvider>().cast(),
            (Node::Filter, UIA_ValuePatternId) => me.to_interface::<IValueProvider>().cast(),
            _ => Err(none()),
        }
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let snap = self.sh.snap();
        self.alive(&snap)?;
        if id == UIA_FrameworkIdPropertyId {
            return Ok(variant_str("Tsubame"));
        }
        if id == UIA_IsControlElementPropertyId || id == UIA_IsContentElementPropertyId {
            return Ok(variant_bool(true));
        }
        match self.node {
            Node::Level(l) => {
                let lv = &snap.levels[l];
                Ok(match id {
                    UIA_ControlTypePropertyId => VARIANT::from(UIA_MenuControlTypeId.0),
                    UIA_NamePropertyId => variant_str(&lv.name),
                    UIA_IsEnabledPropertyId => variant_bool(true),
                    UIA_IsKeyboardFocusablePropertyId | UIA_HasKeyboardFocusPropertyId | UIA_IsOffscreenPropertyId => variant_bool(false),
                    UIA_OrientationPropertyId => VARIANT::from(OrientationType_Vertical.0),
                    UIA_ClassNamePropertyId => variant_str("TsubameMenu"),
                    UIA_AutomationIdPropertyId => variant_str(&format!("ctx.{l}")),
                    _ => VARIANT::default(),
                })
            }
            Node::Filter => {
                let f = snap.levels.first().and_then(|l| l.filter.as_ref()).ok_or_else(gone)?;
                Ok(match id {
                    UIA_ControlTypePropertyId => VARIANT::from(UIA_EditControlTypeId.0),
                    UIA_NamePropertyId => variant_str("Filter menu items"),
                    UIA_IsEnabledPropertyId | UIA_IsKeyboardFocusablePropertyId => variant_bool(true),
                    UIA_HasKeyboardFocusPropertyId | UIA_IsOffscreenPropertyId | UIA_IsPasswordPropertyId => variant_bool(false),
                    UIA_ValueValuePropertyId => variant_str(&f.1),
                    UIA_AutomationIdPropertyId => variant_str("ctx.filter"),
                    _ => VARIANT::default(),
                })
            }
            Node::Row(l, k) => {
                let r = self.row(&snap)?;
                Ok(match id {
                    UIA_ControlTypePropertyId => VARIANT::from(UIA_MenuItemControlTypeId.0),
                    UIA_NamePropertyId => variant_str(&r.name),
                    UIA_IsEnabledPropertyId | UIA_IsKeyboardFocusablePropertyId => variant_bool(r.enabled),
                    UIA_HasKeyboardFocusPropertyId => variant_bool(r.focused),
                    UIA_IsOffscreenPropertyId => variant_bool(!r.visible),
                    UIA_AccessKeyPropertyId if !r.access.is_empty() => variant_str(&r.access),
                    UIA_AcceleratorKeyPropertyId if !r.shortcut.is_empty() => variant_str(&r.shortcut),
                    UIA_ClassNamePropertyId => variant_str("TsubameMenuItem"),
                    UIA_AutomationIdPropertyId => variant_str(&format!("ctx.{l}.{k:x}")),
                    _ => VARIANT::default(),
                })
            }
            Node::Root => Ok(VARIANT::default()),
        }
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        Err(none())
    }
}

impl IRawElementProviderFragment_Impl for Item_Impl {
    fn Navigate(&self, dir: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let snap = self.sh.snap();
        self.alive(&snap)?;
        match self.sh.navigate(&snap, self.node, dir) {
            Some(n) if Shared::exists(&snap, n) => self.sh.fragment(n),
            _ => Err(none()),
        }
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        runtime_array(&self.node.runtime_id())
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let snap = self.sh.snap();
        let r = self.rect(&snap)?;
        Ok(self.sh.rect(r))
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Err(none())
    }
    fn SetFocus(&self) -> Result<()> {
        // the menu owns its focus (arrows, hover); nothing to move here
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        self.sh.provider(Node::Root)?.cast()
    }
}

impl IInvokeProvider_Impl for Item_Impl {
    fn Invoke(&self) -> Result<()> {
        self.invoke()
    }
}

impl IToggleProvider_Impl for Item_Impl {
    fn Toggle(&self) -> Result<()> {
        self.invoke()
    }
    fn ToggleState(&self) -> Result<ToggleState> {
        let snap = self.sh.snap();
        Ok(if self.row(&snap)?.toggled { ToggleState_On } else { ToggleState_Off })
    }
}

impl IExpandCollapseProvider_Impl for Item_Impl {
    fn Expand(&self) -> Result<()> {
        let snap = self.sh.snap();
        let r = self.row(&snap)?;
        if r.expanded {
            return Ok(());
        }
        self.invoke()
    }
    fn Collapse(&self) -> Result<()> {
        let snap = self.sh.snap();
        let r = self.row(&snap)?;
        if let Node::Row(lv, key) = self.node
            && r.expanded
        {
            self.sh.post(Req { lv, key, collapse: true });
        }
        Ok(())
    }
    fn ExpandCollapseState(&self) -> Result<ExpandCollapseState> {
        let snap = self.sh.snap();
        let r = self.row(&snap)?;
        Ok(if !r.submenu {
            ExpandCollapseState_LeafNode
        } else if r.expanded {
            ExpandCollapseState_Expanded
        } else {
            ExpandCollapseState_Collapsed
        })
    }
}

impl IValueProvider_Impl for Item_Impl {
    fn SetValue(&self, _: &PCWSTR) -> Result<()> {
        // typing goes through the keyboard like everywhere else in the menu
        Err(Error::from_hresult(windows::core::HRESULT(UIA_E_NOTSUPPORTED as i32)))
    }
    fn Value(&self) -> Result<BSTR> {
        let snap = self.sh.snap();
        snap.levels.first().and_then(|l| l.filter.as_ref()).map(|f| BSTR::from(f.1.as_str())).ok_or_else(gone)
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: u64, name: &str) -> Row {
        Row { key, name: name.into(), enabled: true, visible: true, rect: [0.0, key as f32 * 20.0, 100.0, 20.0], ..Default::default() }
    }

    fn snap() -> Snap {
        let mut sub = row(9, "Send to");
        sub.submenu = true;
        sub.expanded = true;
        let mut copy = row(2, "Copy");
        copy.focused = false;
        Snap {
            levels: vec![
                Level { key: 0, name: "Context menu".into(), rect: [0.0, 0.0, 100.0, 100.0], rows: vec![row(1, "Open"), copy, sub], filter: None },
                Level { key: 9, name: "Send to".into(), rect: [100.0, 0.0, 100.0, 100.0], rows: vec![row(91, "Desktop"), row(92, "Mail")], filter: None },
            ],
        }
    }

    fn sh() -> Shared {
        Shared { hwnd: 0, snap: Mutex::new(Arc::new(Snap::default())), cache: Mutex::new(HashMap::new()), root: Mutex::new(None) }
    }

    #[test]
    fn the_tree_walks_levels_rows_and_submenus() {
        let s = snap();
        let sh = sh();
        let nav = |n, d| sh.navigate(&s, n, d);
        assert_eq!(nav(Node::Root, NavigateDirection_FirstChild), Some(Node::Level(0)));
        assert_eq!(nav(Node::Level(0), NavigateDirection_Parent), Some(Node::Root));
        assert_eq!(nav(Node::Level(0), NavigateDirection_FirstChild), Some(Node::Row(0, 1)));
        assert_eq!(nav(Node::Level(0), NavigateDirection_LastChild), Some(Node::Row(0, 9)));
        assert_eq!(nav(Node::Row(0, 1), NavigateDirection_NextSibling), Some(Node::Row(0, 2)));
        assert_eq!(nav(Node::Row(0, 1), NavigateDirection_PreviousSibling), None);
        assert_eq!(nav(Node::Row(0, 9), NavigateDirection_NextSibling), None);
        // an open submenu is the child of the row that opened it
        assert_eq!(nav(Node::Row(0, 9), NavigateDirection_FirstChild), Some(Node::Level(1)));
        assert_eq!(nav(Node::Level(1), NavigateDirection_Parent), Some(Node::Row(0, 9)));
        assert_eq!(nav(Node::Row(0, 2), NavigateDirection_FirstChild), None);
        assert_eq!(nav(Node::Row(1, 92), NavigateDirection_PreviousSibling), Some(Node::Row(1, 91)));
        assert_eq!(nav(Node::Row(1, 92), NavigateDirection_Parent), Some(Node::Level(1)));
    }

    #[test]
    fn the_filter_field_comes_first_and_gone_rows_do_not_exist() {
        let mut s = snap();
        s.levels[0].filter = Some(([0.0; 4], "se".into()));
        let sh = sh();
        assert_eq!(sh.navigate(&s, Node::Level(0), NavigateDirection_FirstChild), Some(Node::Filter));
        assert_eq!(sh.navigate(&s, Node::Filter, NavigateDirection_NextSibling), Some(Node::Row(0, 1)));
        assert_eq!(sh.navigate(&s, Node::Row(0, 1), NavigateDirection_PreviousSibling), Some(Node::Filter));
        assert!(Shared::exists(&s, Node::Filter));
        assert!(!Shared::exists(&s, Node::Row(0, 77)));
        assert!(!Shared::exists(&s, Node::Level(2)));
        assert!(!Shared::exists(&Snap::default(), Node::Level(0)));
    }

    #[test]
    fn runtime_ids_differ_per_element_and_focus_is_found() {
        let a = Node::Row(0, 1).runtime_id();
        assert_ne!(a, Node::Row(0, 2).runtime_id());
        assert_ne!(a, Node::Row(1, 1).runtime_id());
        assert_ne!(a, Node::Level(0).runtime_id());
        assert_eq!(a[0], UiaAppendRuntimeId as i32);
        let mut s = snap();
        assert_eq!(s.focus(), None);
        s.levels[1].rows[1].focused = true;
        assert_eq!(s.focus(), Some((1, 92)));
    }

    #[test]
    fn shape_ignores_focus_and_geometry_but_not_rows() {
        let a = snap();
        let mut b = a.clone();
        b.levels[0].rows[0].focused = true;
        b.levels[0].rows[0].rect = [5.0; 4];
        assert_eq!(a.shape(), b.shape());
        b.levels[0].rows.pop();
        assert_ne!(a.shape(), b.shape());
    }
}
