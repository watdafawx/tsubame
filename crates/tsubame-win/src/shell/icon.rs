//! System icons. Worker (STA) threads only.

use crate::bitmap::{self, Bitmap};
use crate::wide;
use std::cell::RefCell;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
use windows::core::PCWSTR;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconSource {
    /// By extension (".txt", or "" for no extension) with
    /// `SHGFI_USEFILEATTRIBUTES`: the disk is never touched.
    Ext(String),
    /// The generic folder icon (no disk).
    Folder,
    /// A real path: exe/lnk/ico, or a folder with a custom icon. Reads the file.
    Path(String),
    /// Same as `Path` but through `IShellItemImageFactory` + `SIIGBF_ICONONLY`
    /// (exact size, for comparison).
    PathFactory(String),
}

thread_local! {
    /// (SHIL_*, icon px) of this thread's system image lists, smallest first
    static LISTS: RefCell<Vec<(IImageList, i32)>> = const { RefCell::new(Vec::new()) };
}

fn list_for(size: u32) -> Option<IImageList> {
    LISTS.with(|l| {
        let mut l = l.borrow_mut();
        if l.is_empty() {
            for shil in [SHIL_SMALL, SHIL_LARGE, SHIL_EXTRALARGE, SHIL_JUMBO] {
                if let Ok(il) = unsafe { SHGetImageList::<IImageList>(shil as i32) } {
                    let (mut cx, mut cy) = (0, 0);
                    let _ = unsafe { il.GetIconSize(&mut cx, &mut cy) };
                    l.push((il, cx));
                }
            }
            l.sort_by_key(|(_, cx)| *cx);
        }
        l.iter().find(|(_, cx)| *cx >= size as i32).or(l.last()).map(|(il, _)| il.clone())
    })
}

/// Loads this thread's system image lists. The first use of the system image
/// list in a process is not thread-safe: two workers calling `SHGetFileInfoW`
/// at once on a cold process made one of them fail (returns 0), every run.
/// So each thread warms up under one process-wide lock before its first icon.
pub fn warm_up() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock();
    let _ = list_for(16);
    let mut sfi = SHFILEINFOW::default();
    let w = wide("x.txt");
    unsafe { SHGetFileInfoW(PCWSTR(w.as_ptr()), FILE_ATTRIBUTE_NORMAL, Some(&mut sfi), size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES) };
}

pub fn get(src: &IconSource, size: u32) -> Option<Bitmap> {
    if LISTS.with(|l| l.borrow().is_empty()) {
        warm_up();
    }
    let (path, attrs, flags) = match src {
        IconSource::Ext(e) => (format!("x{e}"), FILE_ATTRIBUTE_NORMAL, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES),
        IconSource::Folder => ("folder".to_string(), FILE_ATTRIBUTE_DIRECTORY, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES),
        IconSource::Path(p) => (p.clone(), FILE_FLAGS_AND_ATTRIBUTES(0), SHGFI_SYSICONINDEX),
        IconSource::PathFactory(p) => return by_factory(p, size),
    };
    let w = wide(&path);
    let mut sfi = SHFILEINFOW::default();
    // with SHGFI_SYSICONINDEX the return value is the shared system image
    // list (never freed by the caller) and no HICON is created
    let r = unsafe { SHGetFileInfoW(PCWSTR(w.as_ptr()), attrs, Some(&mut sfi), size_of::<SHFILEINFOW>() as u32, flags) };
    if r == 0 {
        return None;
    }
    let il = list_for(size)?;
    let hicon = unsafe { il.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0) }.ok()?;
    let b = bitmap::from_hicon(hicon);
    unsafe {
        let _ = DestroyIcon(hicon);
    }
    // the system lists come in 16/32/48/256 px: cut to the size asked, so a
    // 96 px cell icon does not sit in the cache as a 256 px jumbo (256 KB)
    b.map(|b| b.fit_within(size))
}

pub fn by_factory(path: &str, size: u32) -> Option<Bitmap> {
    let w = wide(path);
    unsafe {
        let f: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
        let hbm = f.GetImage(SIZE { cx: size as i32, cy: size as i32 }, SIIGBF_ICONONLY).ok()?;
        let b = bitmap::from_shell_hbitmap(hbm);
        let _ = DeleteObject(hbm.into());
        b
    }
}
