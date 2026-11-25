//! OLE drag-drop and the shell clipboard formats. See `docs/shell-integration.md`.
//!
//! - [`formats`]: pure building/parsing (`CF_HDROP`, `Preferred DropEffect`,
//!   `FileGroupDescriptorW`), path validation, DROPEFFECT and key bits.
//! - [`DataObject`]: our `IDataObject` (drag-out payload, clipboard object).
//! - [`drag_files`]: the drag source (`DoDragDrop` with our `IDropSource`).
//!   Runs on the UI thread, which must be an OLE STA (`OleInitialize`). The
//!   modal loop inside `DoDragDrop` dispatches messages, so the window keeps
//!   painting.
//! - [`register_target`]: our `IDropTarget`, forwarding to a [`DropEvents`]
//!   implementation of the app. The app decides the effect.
//! - [`clipboard`]: set and read the clipboard; runs on a shell worker (STA).
//!
//! Nothing here executes content: dropped or pasted data is only ever read
//! as a list of paths (validated) or, for virtual files, as bytes written to
//! a staging folder under size caps.

pub mod clipboard;
mod dataobject;
pub mod formats;
pub mod image;

pub use dataobject::{DataObject, read_hglobal, to_hglobal};
pub use formats::{EFFECT_COPY, EFFECT_LINK, EFFECT_MOVE, EFFECT_NONE};

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::OnceLock;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::core::{BOOL, HRESULT, PCWSTR, Ref, Result, implement};

fn register(name: &str, cell: &'static OnceLock<u16>) -> u16 {
    *cell.get_or_init(|| {
        let w = crate::wide(name);
        unsafe { RegisterClipboardFormatW(PCWSTR(w.as_ptr())) as u16 }
    })
}

macro_rules! cf {
    ($fn:ident, $name:expr) => {
        pub fn $fn() -> u16 {
            static C: OnceLock<u16> = OnceLock::new();
            register($name, &C)
        }
    };
}
cf!(cf_preferred_effect, "Preferred DropEffect");
cf!(cf_performed_effect, "Performed DropEffect");
cf!(cf_logical_performed_effect, "Logical Performed DropEffect");
cf!(cf_paste_succeeded, "Paste Succeeded");
cf!(cf_file_group_descriptor, "FileGroupDescriptorW");
cf!(cf_file_contents, "FileContents");

/// HGLOBAL bytes of format `cf` (item `lindex`, -1 for all) of a data
/// object, at most `cap` bytes. A cross-process call when the object comes
/// from another app.
pub fn get_bytes(obj: &IDataObject, cf: u16, lindex: i32, cap: usize) -> Option<Vec<u8>> {
    let fe = FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex, tymed: TYMED_HGLOBAL.0 as u32 };
    unsafe {
        let mut m = obj.GetData(&fe).ok()?;
        let out = if m.tymed == TYMED_HGLOBAL.0 as u32 { read_hglobal(m.u.hGlobal, cap) } else { None };
        ReleaseStgMedium(&mut m);
        out
    }
}

pub fn has_format(obj: &IDataObject, cf: u16) -> bool {
    let fe = FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    unsafe { obj.QueryGetData(&fe) == S_OK }
}

/// Stores a DWORD format on a data object (`Performed DropEffect` etc.).
pub fn set_dword(obj: &IDataObject, cf: u16, v: u32) {
    let fe = FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    if let Ok(h) = to_hglobal(&v.to_le_bytes()) {
        let m = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: std::mem::ManuallyDrop::new(None) };
        // fRelease = TRUE: the object owns the memory on success
        if unsafe { obj.SetData(&fe, &m, true) }.is_err() {
            unsafe {
                let _ = GlobalFree(Some(h));
            }
        }
    }
}

/// Largest `CF_HDROP` read (a 100,000-path list of long paths fits).
pub const HDROP_CAP: usize = 64 << 20;

/// What a drop or the clipboard carries, read once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DropInfo {
    /// validated file system paths (plain form)
    pub paths: Vec<PathBuf>,
    /// entries refused by validation (device paths, streams, ...)
    pub rejected: usize,
    /// virtual files offered (`FileGroupDescriptorW`) when there is no `CF_HDROP`
    pub virtual_items: Vec<formats::FileDescriptor>,
    /// `Preferred DropEffect` if the source gave one
    pub preferred: Option<u32>,
}

/// Reads the file list (and the virtual-file descriptors when there is no
/// file list) from a data object.
pub fn read_info(obj: &IDataObject) -> DropInfo {
    let mut info = DropInfo::default();
    if let Some(b) = get_bytes(obj, CF_HDROP.0, -1, HDROP_CAP) {
        let dec = |s: &[u8]| ansi_to_string(s);
        if let Some(paths) = formats::parse_hdrop(&b, Some(&dec)) {
            let (ok, bad) = formats::validate_all(&paths);
            info.paths = ok;
            info.rejected = bad;
        }
    } else if let Some(b) = get_bytes(obj, cf_file_group_descriptor(), -1, HDROP_CAP) {
        info.virtual_items = formats::parse_group_descriptor(&b);
    }
    info.preferred = get_bytes(obj, cf_preferred_effect(), -1, 64).and_then(|b| formats::parse_effect(&b));
    info
}

fn ansi_to_string(s: &[u8]) -> String {
    use windows::Win32::Globalization::{CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
    let n = unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), s, None) };
    if n <= 0 {
        return String::new();
    }
    let mut w = vec![0u16; n as usize];
    unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), s, Some(&mut w)) };
    String::from_utf16_lossy(&w)
}

// ---- drag source -------------------------------------------------------------

#[implement(IDropSource)]
struct Source {
    /// MK_LBUTTON or MK_RBUTTON: the button that started the drag
    button: u32,
}

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        let other = if self.button == formats::MK_LBUTTON { formats::MK_RBUTTON } else { formats::MK_LBUTTON };
        if fescapepressed.as_bool() || grfkeystate.0 & other != 0 {
            DRAGDROP_S_CANCEL
        } else if grfkeystate.0 & self.button == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DragOutcome {
    /// the target dropped (false: cancelled or refused)
    pub dropped: bool,
    /// effect the target reported (an optimized move reports NONE)
    pub effect: u32,
}

/// Drags `paths` out with `allowed` effects. Call on an OLE STA thread (the UI
/// thread) after the movement threshold, outside any borrow of app state: the
/// modal loop inside dispatches messages (paints, and our own drop target's
/// callbacks when the drop lands on this process). `button` is
/// `formats::MK_LBUTTON` or `MK_RBUTTON`.
///
/// The source never deletes anything afterwards: targets that move files do
/// it themselves (optimized move; Explorer always does).
pub fn drag_files(paths: &[PathBuf], allowed: u32, button: u32) -> Result<DragOutcome> {
    let obj = DataObject::files(paths, None);
    let src: IDropSource = Source { button }.into();
    let mut eff = DROPEFFECT(0);
    let hr = unsafe { DoDragDrop(&obj, &src, DROPEFFECT(allowed), &mut eff) };
    if hr.is_err() {
        return Err(hr.into());
    }
    Ok(DragOutcome { dropped: hr == DRAGDROP_S_DROP, effect: eff.0 })
}

// ---- drop target -------------------------------------------------------------

/// The app side of a drop target. Points are screen pixels, `keys` the
/// `MK_*` bits, `allowed` the source's DROPEFFECT mask. Each call returns the
/// effect to show (masked by `allowed` here). Called on the window's thread.
pub trait DropEvents {
    fn enter(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32;
    fn over(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32;
    fn leave(&mut self);
    /// The drop. Queue the work and return at once; return the effect
    /// performed (MOVE means we move the files ourselves).
    fn drop(&mut self, info: &DropInfo, pt: (i32, i32), keys: u32, allowed: u32) -> u32;
}

#[implement(IDropTarget)]
struct Target {
    h: RefCell<Box<dyn DropEvents>>,
    info: RefCell<Option<DropInfo>>,
}

fn pick(e: u32, allowed: u32) -> u32 {
    let e = e & allowed;
    // exactly one effect
    [EFFECT_MOVE, EFFECT_COPY, EFFECT_LINK].into_iter().find(|&b| e & b != 0).unwrap_or(EFFECT_NONE)
}

impl Target_Impl {
    fn call(&self, f: impl FnOnce(&mut dyn DropEvents, &DropInfo) -> u32) -> u32 {
        let info = self.info.borrow().clone().unwrap_or_default();
        // a nested call (a modal loop inside the handler) gets NONE instead of a panic
        match self.h.try_borrow_mut() {
            Ok(mut h) => f(&mut **h, &info),
            Err(_) => EFFECT_NONE,
        }
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, pdataobj: Ref<'_, IDataObject>, grfkeystate: MODIFIERKEYS_FLAGS, pt: &POINTL, pdweffect: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { pdweffect.as_ref() }.map(|e| e.0).unwrap_or(0);
        let info = pdataobj.as_ref().map(read_info).unwrap_or_default();
        *self.info.borrow_mut() = Some(info);
        let e = self.call(|h, i| h.enter(i, (pt.x, pt.y), grfkeystate.0, allowed));
        if let Some(out) = unsafe { pdweffect.as_mut() } {
            *out = DROPEFFECT(pick(e, allowed));
        }
        Ok(())
    }

    fn DragOver(&self, grfkeystate: MODIFIERKEYS_FLAGS, pt: &POINTL, pdweffect: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { pdweffect.as_ref() }.map(|e| e.0).unwrap_or(0);
        let e = self.call(|h, i| h.over(i, (pt.x, pt.y), grfkeystate.0, allowed));
        if let Some(out) = unsafe { pdweffect.as_mut() } {
            *out = DROPEFFECT(pick(e, allowed));
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        self.info.borrow_mut().take();
        if let Ok(mut h) = self.h.try_borrow_mut() {
            h.leave();
        }
        Ok(())
    }

    fn Drop(&self, pdataobj: Ref<'_, IDataObject>, grfkeystate: MODIFIERKEYS_FLAGS, pt: &POINTL, pdweffect: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { pdweffect.as_ref() }.map(|e| e.0).unwrap_or(0);
        if self.info.borrow().is_none() {
            let info = pdataobj.as_ref().map(read_info).unwrap_or_default();
            *self.info.borrow_mut() = Some(info);
        }
        let e = pick(self.call(|h, i| h.drop(i, (pt.x, pt.y), grfkeystate.0, allowed)), allowed);
        self.info.borrow_mut().take();
        // optimized move: we move the files ourselves (through the queue), so
        // the source must not delete them. Report NONE as performed and MOVE as
        // the logical effect (the protocol Explorer uses).
        if let Some(obj) = pdataobj.as_ref() {
            set_dword(obj, cf_performed_effect(), if e == EFFECT_MOVE { EFFECT_NONE } else { e });
            set_dword(obj, cf_logical_performed_effect(), e);
        }
        if let Some(out) = unsafe { pdweffect.as_mut() } {
            *out = DROPEFFECT(if e == EFFECT_MOVE { EFFECT_NONE } else { e });
        }
        Ok(())
    }
}

/// Registers `hwnd` as a drop target. The thread must be an OLE STA
/// (`OleInitialize`). Undo with [`revoke_target`] before the window is
/// destroyed.
pub fn register_target(hwnd: HWND, handler: Box<dyn DropEvents>) -> Result<()> {
    let t: IDropTarget = Target { h: RefCell::new(handler), info: RefCell::new(None) }.into();
    unsafe { RegisterDragDrop(hwnd, &t) }
}

pub fn revoke_target(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

/// `OleInitialize` for the calling thread (the UI thread: drag and drop need
/// an OLE STA). Ok(false) if it was already initialized.
pub fn ole_init() -> Result<bool> {
    let hr = unsafe { OleInitialize(None) };
    match hr {
        Ok(()) => Ok(true),
        Err(e) if e.code() == S_FALSE => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_one_effect() {
        assert_eq!(pick(EFFECT_COPY | EFFECT_MOVE, EFFECT_COPY), EFFECT_COPY);
        assert_eq!(pick(EFFECT_MOVE, EFFECT_COPY | EFFECT_MOVE), EFFECT_MOVE);
        assert_eq!(pick(EFFECT_LINK, EFFECT_COPY), EFFECT_NONE);
        assert_eq!(pick(EFFECT_NONE, 7), EFFECT_NONE);
    }
}
