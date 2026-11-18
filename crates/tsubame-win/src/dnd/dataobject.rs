//! Our own `IDataObject`: a list of HGLOBAL formats held as bytes. Used as
//! the drag-out payload and as the clipboard object. No shell COM involved:
//! `CF_HDROP` plus `Preferred DropEffect` is what Explorer and other apps
//! read for files. `SetData` stores whatever a target or the drag helper
//! sends (`Performed DropEffect`, `DropDescription`, ...), capped.

use super::formats;
use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT, ReleaseStgMedium};
use windows::core::{BOOL, HRESULT, Ref, Result, implement};

/// Largest single format a `SetData` call may store.
const SET_CAP: usize = 4 << 20;
/// Most formats one object keeps.
const MAX_FORMATS: usize = 48;

#[implement(IDataObject)]
pub struct DataObject {
    items: RefCell<Vec<(u16, Vec<u8>)>>,
}

impl DataObject {
    pub fn with(items: Vec<(u16, Vec<u8>)>) -> IDataObject {
        DataObject { items: RefCell::new(items) }.into()
    }

    /// Files: `CF_HDROP`, and `Preferred DropEffect` when given (the
    /// clipboard's cut/copy marker).
    pub fn files(paths: &[PathBuf], preferred: Option<u32>) -> IDataObject {
        let mut v = vec![(CF_HDROP.0, formats::hdrop_bytes(paths))];
        if let Some(e) = preferred {
            v.push((super::cf_preferred_effect(), formats::effect_bytes(e)));
        }
        Self::with(v)
    }

    pub fn text(s: &str) -> IDataObject {
        Self::with(vec![(CF_UNICODETEXT.0, formats::text_bytes(s))])
    }
}

/// Copies `bytes` into a new movable HGLOBAL (the receiver frees it).
pub fn to_hglobal(bytes: &[u8]) -> Result<HGLOBAL> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return Err(E_OUTOFMEMORY.into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}

/// At most `cap` bytes of an HGLOBAL. `None` if it cannot be locked or is
/// bigger than `cap`.
pub fn read_hglobal(h: HGLOBAL, cap: usize) -> Option<Vec<u8>> {
    unsafe {
        let n = GlobalSize(h);
        if n > cap {
            return None;
        }
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, n).to_vec();
        let _ = GlobalUnlock(h);
        Some(v)
    }
}

fn hglobal_medium(h: HGLOBAL) -> STGMEDIUM {
    STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: ManuallyDrop::new(None) }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
        let fe = unsafe { pformatetcin.as_ref() }.ok_or(E_INVALIDARG)?;
        if fe.dwAspect != DVASPECT_CONTENT.0 {
            return Err(DV_E_FORMATETC.into());
        }
        if fe.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            return Err(DV_E_TYMED.into());
        }
        let items = self.items.borrow();
        let (_, data) = items.iter().find(|(cf, _)| *cf == fe.cfFormat).ok_or(DV_E_FORMATETC)?;
        Ok(hglobal_medium(to_hglobal(data)?))
    }

    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        let Some(fe) = (unsafe { pformatetc.as_ref() }) else { return E_INVALIDARG };
        if fe.dwAspect != DVASPECT_CONTENT.0 || fe.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            return DV_E_FORMATETC;
        }
        if self.items.borrow().iter().any(|(cf, _)| *cf == fe.cfFormat) { S_OK } else { DV_E_FORMATETC }
    }

    fn GetCanonicalFormatEtc(&self, _pformatectin: *const FORMATETC, pformatetcout: *mut FORMATETC) -> HRESULT {
        if let Some(out) = unsafe { pformatetcout.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(&self, pformatetc: *const FORMATETC, pmedium: *const STGMEDIUM, frelease: BOOL) -> Result<()> {
        let fe = unsafe { pformatetc.as_ref() }.ok_or(E_INVALIDARG)?;
        let m = unsafe { pmedium.as_ref() }.ok_or(E_INVALIDARG)?;
        let res = if m.tymed == TYMED_HGLOBAL.0 as u32 {
            match read_hglobal(unsafe { m.u.hGlobal }, SET_CAP) {
                Some(bytes) => {
                    let mut items = self.items.borrow_mut();
                    if let Some(slot) = items.iter_mut().find(|(cf, _)| *cf == fe.cfFormat) {
                        slot.1 = bytes;
                        Ok(())
                    } else if items.len() < MAX_FORMATS {
                        items.push((fe.cfFormat, bytes));
                        Ok(())
                    } else {
                        Err(E_OUTOFMEMORY.into())
                    }
                }
                None => Err(E_OUTOFMEMORY.into()),
            }
        } else {
            Err(DV_E_TYMED.into())
        };
        if frelease.as_bool() {
            // we own the medium now, used or not
            unsafe { ReleaseStgMedium(pmedium as *mut STGMEDIUM) };
        }
        res
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        let list = self.items.borrow().iter().map(|(cf, _)| *cf).collect();
        Ok(FormatEnum { list, pos: Cell::new(0) }.into())
    }

    fn DAdvise(&self, _pformatetc: *const FORMATETC, _advf: u32, _padvsink: Ref<'_, IAdviseSink>) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

#[implement(IEnumFORMATETC)]
struct FormatEnum {
    list: Vec<u16>,
    pos: Cell<usize>,
}

fn fmt(cf: u16) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

impl IEnumFORMATETC_Impl for FormatEnum_Impl {
    fn Next(&self, celt: u32, rgelt: *mut FORMATETC, pceltfetched: *mut u32) -> HRESULT {
        if rgelt.is_null() {
            return E_POINTER;
        }
        let mut n = 0usize;
        while n < celt as usize && self.pos.get() < self.list.len() {
            unsafe { *rgelt.add(n) = fmt(self.list[self.pos.get()]) };
            self.pos.set(self.pos.get() + 1);
            n += 1;
        }
        if let Some(f) = unsafe { pceltfetched.as_mut() } {
            *f = n as u32;
        }
        if n == celt as usize { S_OK } else { S_FALSE }
    }

    fn Skip(&self, celt: u32) -> Result<()> {
        let p = self.pos.get() + celt as usize;
        self.pos.set(p.min(self.list.len()));
        if p <= self.list.len() { Ok(()) } else { Err(S_FALSE.into()) }
    }

    fn Reset(&self) -> Result<()> {
        self.pos.set(0);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumFORMATETC> {
        Ok(FormatEnum { list: self.list.clone(), pos: Cell::new(self.pos.get()) }.into())
    }
}
