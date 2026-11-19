//! The Windows "Open" dialog (IFileOpenDialog) for choosing a `shell.nss`. It runs on its own
//! thread (COM apartment of its own) so the window keeps painting; the settings page polls
//! [`Picker::poll`]. The dialog is owned by the window that was in front when it opened, which
//! makes it modal to that window. The chosen file is only read, never changed.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::w;

/// A dialog that is open (or just closed, waiting to be polled).
pub struct Picker {
    rx: Receiver<Option<PathBuf>>,
}

/// What a poll found.
pub enum Poll {
    /// still open
    Open,
    /// closed without a choice
    Cancelled,
    Chosen(PathBuf),
}

impl Picker {
    /// Opens the dialog on a worker thread.
    pub fn open() -> Picker {
        // the window in front now is ours (the click was on it); its handle crosses threads as an integer
        let owner = unsafe { GetForegroundWindow() }.0 as isize;
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new().name("nss-picker".into()).spawn(move || {
            let _ = tx.send(show(owner));
        });
        if spawned.is_err() {
            // no thread: report a cancel on the next poll
            let (tx2, rx2) = channel();
            let _ = tx2.send(None);
            return Picker { rx: rx2 };
        }
        Picker { rx }
    }

    pub fn poll(&self) -> Poll {
        match self.rx.try_recv() {
            Ok(Some(p)) => Poll::Chosen(p),
            Ok(None) | Err(TryRecvError::Disconnected) => Poll::Cancelled,
            Err(TryRecvError::Empty) => Poll::Open,
        }
    }
}

fn show(owner: isize) -> Option<PathBuf> {
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let out = (|| -> windows::core::Result<PathBuf> {
            let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            let types = [
                COMDLG_FILTERSPEC { pszName: w!("Nilesoft Shell script (*.nss)"), pszSpec: w!("*.nss") },
                COMDLG_FILTERSPEC { pszName: w!("All files (*.*)"), pszSpec: w!("*.*") },
            ];
            dlg.SetFileTypes(&types)?;
            dlg.SetFileTypeIndex(1)?;
            dlg.SetTitle(w!("Import from Nilesoft Shell"))?;
            dlg.SetFileName(w!("shell.nss"))?;
            let opts = dlg.GetOptions()?;
            dlg.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST)?;
            let owner = if owner == 0 { None } else { Some(HWND(owner as *mut _)) };
            dlg.Show(owner)?;
            let item = dlg.GetResult()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = name.to_string();
            CoTaskMemFree(Some(name.0 as *const _));
            Ok(PathBuf::from(path?))
        })();
        if com {
            CoUninitialize();
        }
        out.ok()
    }
}
