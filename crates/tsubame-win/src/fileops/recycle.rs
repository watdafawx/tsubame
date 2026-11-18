//! Recycle Bin delete and restore through `IFileOperation`. STA thread only.
//!
//! Delete: `IFileOperation::DeleteItem` with silent, no-confirmation, no-error-UI flags and
//! `FOFX_RECYCLEONDELETE`. A progress sink catches `PostDeleteItem`, whose "newly created"
//! item is the entry inside the bin; its file system path (`C:\$Recycle.Bin\<sid>\$R....`)
//! is the restore token. If the shell reports no such item the file was deleted for good
//! (no bin on the volume, item too big), which is reported as an error, never silently.
//!
//! Restore: enumerate the aggregated Recycle Bin folder (`FOLDERID_RecycleBinFolder`),
//! find the entry whose file system path equals the token, and `MoveItem` it back to its
//! original folder and name. The shell keeps the bin metadata (`$I` file) consistent.
use std::{
    cell::{Cell, RefCell},
    ffi::{OsStr, OsString},
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND},
        Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW},
        System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree},
        UI::Shell::{
            BHID_EnumItems, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
            FOFX_NOCOPYHOOKS, FOFX_RECYCLEONDELETE, FOLDERID_RecycleBinFolder, FileOperation,
            IEnumShellItems, IFileOperation, IFileOperationProgressSink,
            IFileOperationProgressSink_Impl, IShellItem, KF_FLAG_DEFAULT,
            SHCreateItemFromParsingName, SHGetKnownFolderItem, SHQUERYRBINFO, SHQueryRecycleBinW,
            SIGDN, SIGDN_FILESYSPATH,
        },
    },
    core::{ComObject, HRESULT, PCWSTR, implement},
};

/// Paths this long (or longer) cannot be recycled.
pub const MAX_PATH_CHARS: usize = 260;

/// DRIVE_FIXED from `GetDriveTypeW`.
const DRIVE_FIXED: u32 = 3;

/// Token for one item sitting in the Recycle Bin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecycledItem {
    /// Where the item lived before it was recycled; restore puts it back here.
    pub original: PathBuf,
    /// The bin entry (`...\$Recycle.Bin\<sid>\$R....`) as reported by the shell.
    pub recycled: PathBuf,
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn hr(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    // HRESULT_FROM_WIN32(n) = 0x8007_nnnn: surface the Win32 error so ErrorKind maps.
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(format!("shell error {code:#010x}: {}", e.message()))
    }
}

/// Absolute path; adds `\\?\` for paths too long for the classic limit.
pub fn long_path(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.as_os_str().to_string_lossy();
    if s.starts_with(r"\\?\") || s.len() < 240 {
        return abs;
    }
    if let Some(unc) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{unc}"))
    } else {
        PathBuf::from(format!(r"\\?\{s}"))
    }
}

/// Drop a `\\?\` prefix for storing and showing paths.
pub fn plain_path(path: &Path) -> PathBuf {
    let s = path.as_os_str().to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn shell_item(path: &Path) -> io::Result<IShellItem> {
    // The shell rejects the `\?` form (error 87) but accepts long plain paths when the
    // process is long-path aware (the app manifest says so).
    let w = wide(plain_path(&std::path::absolute(path)?).as_os_str());
    unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) }.map_err(hr)
}

fn display(item: &IShellItem, kind: SIGDN) -> io::Result<PathBuf> {
    unsafe {
        let p = item.GetDisplayName(kind).map_err(hr)?;
        let s = OsString::from_wide(p.as_wide());
        CoTaskMemFree(Some(p.0 as _));
        Ok(PathBuf::from(s))
    }
}

/// True if the volume of `path` is a fixed disk with a Recycle Bin the shell accepts.
pub fn has_recycle_bin(path: &Path) -> bool {
    let abs = plain_path(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    let w = wide(abs.as_os_str());
    let mut root = [0u16; 261];
    if unsafe { GetVolumePathNameW(PCWSTR(w.as_ptr()), &mut root) }.is_err() {
        return false;
    }
    if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != DRIVE_FIXED {
        return false;
    }
    let mut info = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };
    unsafe { SHQueryRecycleBinW(PCWSTR(root.as_ptr()), &mut info) }.is_ok()
}

#[implement(IFileOperationProgressSink)]
#[derive(Default)]
struct Sink {
    /// HRESULT of the last Post* callback.
    result: Cell<Option<HRESULT>>,
    /// File system path of the item created by the operation (the bin entry for deletes).
    created: RefCell<Option<PathBuf>>,
}

impl Sink {
    fn post(&self, hr: HRESULT, created: windows::core::Ref<'_, IShellItem>) {
        self.result.set(Some(hr));
        if let Some(item) = created.as_ref() {
            *self.created.borrow_mut() = display(item, SIGDN_FILESYSPATH).ok();
        }
    }
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        _: &PCWSTR,
        hr: HRESULT,
        created: windows::core::Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.post(hr, created);
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        _: windows::core::Ref<'_, IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        _: windows::core::Ref<'_, IShellItem>,
        _: &PCWSTR,
        hr: HRESULT,
        created: windows::core::Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.post(hr, created);
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        _: windows::core::Ref<'_, IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        _: windows::core::Ref<'_, IShellItem>,
        _: &PCWSTR,
        hr: HRESULT,
        created: windows::core::Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.post(hr, created);
        Ok(())
    }
    fn PreDeleteItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostDeleteItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        hr: HRESULT,
        created: windows::core::Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.post(hr, created);
        Ok(())
    }
    fn PreNewItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: windows::core::Ref<'_, IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        hr: HRESULT,
        created: windows::core::Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.post(hr, created);
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

fn new_operation(recycle: bool) -> io::Result<IFileOperation> {
    let op: IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }.map_err(hr)?;
    let mut flags = FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOFX_NOCOPYHOOKS;
    if recycle {
        flags |= FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE;
    }
    unsafe {
        // No owner window: no dialog can appear, and none may steal focus.
        let _ = op.SetOwnerWindow(HWND::default());
        op.SetOperationFlags(flags).map_err(hr)?;
    }
    Ok(op)
}

fn finish(sink: &Sink, performed: windows::core::Result<()>) -> io::Result<()> {
    performed.map_err(hr)?;
    match sink.result.get() {
        Some(h) if h.is_err() => Err(hr(windows::core::Error::from(h))),
        Some(_) => Ok(()),
        None => Err(hr(windows::core::Error::from(E_FAIL))),
    }
}

/// Move `path` to the Recycle Bin. Errors (and leaves the item alone where the shell
/// allows) if the volume has no bin.
pub fn recycle(path: &Path) -> io::Result<RecycledItem> {
    // The shell cannot recycle items whose path is MAX_PATH or longer, even in a process that
    // is long-path aware: with all prompts off it deletes them permanently. Refuse instead; the
    // caller can offer an explicit permanent delete.
    if std::path::absolute(path)?.as_os_str().len() >= MAX_PATH_CHARS {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "path is 260 characters or longer: it cannot be sent to the Recycle Bin",
        ));
    }
    if !has_recycle_bin(path) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "volume has no Recycle Bin",
        ));
    }
    let original = plain_path(&std::path::absolute(path)?);
    let item = shell_item(path)?;
    let op = new_operation(true)?;
    let sink = ComObject::new(Sink::default());
    let iface: IFileOperationProgressSink = sink.to_interface();
    unsafe { op.DeleteItem(&item, &iface) }.map_err(hr)?;
    let performed = unsafe { op.PerformOperations() };
    finish(&sink, performed)?;
    let recycled = sink.created.borrow_mut().take().ok_or_else(|| {
        io::Error::other("item was deleted permanently: the shell did not recycle it")
    })?;
    Ok(RecycledItem { original, recycled })
}

fn find_in_bin(recycled: &Path) -> io::Result<IShellItem> {
    let want = recycled.as_os_str().to_string_lossy().to_lowercase();
    let bin: IShellItem =
        unsafe { SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None) }
            .map_err(hr)?;
    let items: IEnumShellItems = unsafe { bin.BindToHandler(None, &BHID_EnumItems) }.map_err(hr)?;
    loop {
        let mut slot = [None];
        let mut fetched = 0u32;
        unsafe { items.Next(&mut slot, Some(&mut fetched)) }.map_err(hr)?;
        let Some(item) = slot[0].take() else { break };
        if fetched == 0 {
            break;
        }
        if let Ok(p) = display(&item, SIGDN_FILESYSPATH)
            && p.as_os_str().to_string_lossy().to_lowercase() == want
        {
            return Ok(item);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "item is no longer in the Recycle Bin",
    ))
}

/// Put a recycled item back at its original path. Never overwrites: fails if the original
/// path exists again. Missing parent folders are recreated.
pub fn restore(item: &RecycledItem) -> io::Result<()> {
    if std::fs::symlink_metadata(long_path(&item.original)).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "original path is occupied",
        ));
    }
    let parent = item
        .original
        .parent()
        .ok_or_else(|| io::Error::other("original path has no parent"))?;
    let name = item
        .original
        .file_name()
        .ok_or_else(|| io::Error::other("original path has no name"))?;
    let bin_item = find_in_bin(&item.recycled)?;
    std::fs::create_dir_all(long_path(parent))?;
    let dest = shell_item(parent)?;
    let op = new_operation(false)?;
    let sink = ComObject::new(Sink::default());
    let iface: IFileOperationProgressSink = sink.to_interface();
    let w = wide(name);
    unsafe { op.MoveItem(&bin_item, &dest, PCWSTR(w.as_ptr()), &iface) }.map_err(hr)?;
    let performed = unsafe { op.PerformOperations() };
    finish(&sink, performed)?;
    if std::fs::symlink_metadata(long_path(&item.original)).is_err() {
        return Err(io::Error::other(
            "restore did not recreate the original path",
        ));
    }
    Ok(())
}

/// Permanently remove an item from the Recycle Bin (empties just that entry).
pub fn purge(item: &RecycledItem) -> io::Result<()> {
    let bin_item = find_in_bin(&item.recycled)?;
    let op = new_operation(false)?;
    let sink = ComObject::new(Sink::default());
    let iface: IFileOperationProgressSink = sink.to_interface();
    unsafe { op.DeleteItem(&bin_item, &iface) }.map_err(hr)?;
    let performed = unsafe { op.PerformOperations() };
    finish(&sink, performed)
}
