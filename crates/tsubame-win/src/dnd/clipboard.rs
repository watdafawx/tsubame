//! Clipboard jobs. They run on a shell worker (an OLE STA that pumps), never
//! on the UI thread: `EmptyClipboard` inside `OleSetClipboard` sends a
//! message to the previous owner, and reading a cut from another app is a
//! cross-process call; either can hang when the other app hangs.
//!
//! Setting: our [`DataObject`] goes on the clipboard and is flushed at once
//! (`OleFlushClipboard`), so the data stays after the worker or the app goes
//! away. Reading: `CF_HDROP` + `Preferred DropEffect`; virtual files
//! (`FileGroupDescriptorW` + `FileContents`, e.g. from a zip folder or a mail
//! client) are written to a staging folder under size caps, and the app
//! moves them into place through the file-op queue.

use super::{DataObject, DropInfo, cf_file_contents, formats, read_info};
use std::io::Write;
use std::path::{Path, PathBuf};
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows::Win32::System::Ole::*;
use windows::core::{Interface, PCWSTR};

/// Work for a shell worker.
#[derive(Clone, Debug)]
pub enum ClipJob {
    /// Files with `Preferred DropEffect` MOVE (`cut`) or COPY.
    SetFiles { paths: Vec<PathBuf>, cut: bool },
    SetText(String),
    /// Reads files. Virtual files are staged under `staging` (a fresh folder
    /// the job creates) up to `cap` bytes in all. `consume_cut`: a cut is
    /// emptied from the clipboard once read (it pastes once, like Explorer).
    Read { staging: PathBuf, cap: u64, consume_cut: bool },
    /// Shortcuts (`.lnk`) to `targets` in `folder` (Alt-drop).
    Links { targets: Vec<PathBuf>, folder: PathBuf },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClipReply {
    /// the clipboard sequence number right after our set (the app uses it to
    /// know when our cut was replaced)
    Set(Result<u32, String>),
    Read(Result<ClipRead, String>),
    Links(Result<usize, String>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipRead {
    pub info: DropInfo,
    /// `Preferred DropEffect` (COPY when absent)
    pub effect: u32,
    /// top-level items staged from virtual files (in `staging`)
    pub staged: Vec<PathBuf>,
    pub staging: Option<PathBuf>,
    /// virtual items skipped (unsafe name, over the cap, read error)
    pub skipped: usize,
}

pub fn run(job: ClipJob) -> ClipReply {
    match job {
        ClipJob::SetFiles { paths, cut } => ClipReply::Set(set(DataObject::files(&paths, Some(if cut { formats::EFFECT_MOVE } else { formats::EFFECT_COPY })))),
        ClipJob::SetText(s) => ClipReply::Set(set(DataObject::text(&s))),
        ClipJob::Read { staging, cap, consume_cut } => ClipReply::Read(read(&staging, cap, consume_cut)),
        ClipJob::Links { targets, folder } => ClipReply::Links(make_links(&targets, &folder)),
    }
}

/// Puts `obj` on the clipboard and flushes it (the clipboard keeps rendered
/// copies; no owner thread needs to stay alive). Retries briefly while
/// another process holds the clipboard open.
pub fn set(obj: IDataObject) -> Result<u32, String> {
    let mut last = String::new();
    for _ in 0..10 {
        match unsafe { OleSetClipboard(&obj) } {
            Ok(()) => {
                unsafe {
                    let _ = OleFlushClipboard();
                }
                return Ok(unsafe { GetClipboardSequenceNumber() });
            }
            Err(e) => {
                // CLIPBRD_E_CANT_OPEN: someone else has it open right now
                last = format!("{:?} {}", e.code(), e.message());
                unsafe { windows::Win32::System::Threading::Sleep(20) };
            }
        }
    }
    Err(last)
}

pub fn read(staging: &Path, cap: u64, consume_cut: bool) -> Result<ClipRead, String> {
    let seq = unsafe { GetClipboardSequenceNumber() };
    // CLIPBRD_E_CANT_OPEN: another process (clipboard history, a clipboard
    // listener) has it open right after a change; retry briefly like `set`
    let mut tries = 0;
    let obj = loop {
        match unsafe { OleGetClipboard() } {
            Ok(o) => break o,
            Err(e) if e.code() == windows::Win32::Foundation::CLIPBRD_E_CANT_OPEN && tries < 10 => {
                tries += 1;
                unsafe { windows::Win32::System::Threading::Sleep(20) };
            }
            Err(e) => return Err(format!("OleGetClipboard {:?}", e.code())),
        }
    };
    let info = read_info(&obj);
    let effect = match info.preferred {
        Some(e) if e & formats::EFFECT_MOVE != 0 && e & formats::EFFECT_COPY == 0 => formats::EFFECT_MOVE,
        _ => formats::EFFECT_COPY,
    };
    let mut out = ClipRead { effect, ..Default::default() };
    if info.paths.is_empty() && !info.virtual_items.is_empty() {
        let (staged, skipped) = stage(&obj, &info.virtual_items, staging, cap);
        out.staged = staged;
        out.skipped = skipped;
        out.staging = Some(staging.to_path_buf());
        // staged files are ours: always a copy, never a cut
        out.effect = formats::EFFECT_COPY;
    }
    if consume_cut && effect == formats::EFFECT_MOVE && !info.paths.is_empty() {
        // a cut pastes once: Explorer empties it after an optimized move too
        unsafe {
            // only if nobody replaced it while we read
            if GetClipboardSequenceNumber() == seq {
                let _ = OleSetClipboard(None);
            }
        }
    }
    out.info = info;
    Ok(out)
}

/// Writes the virtual files to `root` (created here). Returns the top-level
/// staged items and the number of items skipped. Stops writing when `cap`
/// bytes are reached; never follows a name outside `root`.
pub fn stage(obj: &IDataObject, items: &[formats::FileDescriptor], root: &Path, cap: u64) -> (Vec<PathBuf>, usize) {
    let mut tops: Vec<PathBuf> = Vec::new();
    let mut skipped = 0;
    let mut used = 0u64;
    if std::fs::create_dir_all(root).is_err() {
        return (tops, items.len());
    }
    for (i, d) in items.iter().enumerate() {
        let Some(rel) = formats::safe_relative(&d.name) else {
            skipped += 1;
            continue;
        };
        let dest = root.join(&rel);
        let ok = if d.is_dir {
            std::fs::create_dir_all(&dest).is_ok()
        } else if d.size.is_some_and(|s| used.saturating_add(s) > cap) {
            false
        } else {
            if let Some(p) = dest.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            match write_contents(obj, i as i32, &dest, cap - used) {
                Some(n) => {
                    used += n;
                    true
                }
                None => {
                    let _ = std::fs::remove_file(&dest);
                    false
                }
            }
        };
        if !ok {
            skipped += 1;
            continue;
        }
        if let Some(first) = rel.components().next() {
            let top = root.join(first);
            if !tops.contains(&top) {
                tops.push(top);
            }
        }
    }
    (tops, skipped)
}

/// `FileContents` item `index` into `dest`, at most `limit` bytes. Returns the
/// bytes written. `None`: refused, too big, or failed.
fn write_contents(obj: &IDataObject, index: i32, dest: &Path, limit: u64) -> Option<u64> {
    let fe = FORMATETC { cfFormat: cf_file_contents(), ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: index, tymed: (TYMED_ISTREAM.0 | TYMED_HGLOBAL.0) as u32 };
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(dest).ok()?;
    unsafe {
        let mut m = obj.GetData(&fe).ok()?;
        let res = if m.tymed == TYMED_HGLOBAL.0 as u32 {
            let cap = usize::try_from(limit).unwrap_or(usize::MAX);
            super::read_hglobal(m.u.hGlobal, cap).and_then(|b| f.write_all(&b).ok().map(|_| b.len() as u64))
        } else if m.tymed == TYMED_ISTREAM.0 as u32 {
            match (*m.u.pstm).as_ref() {
                Some(s) => copy_stream(s, &mut f, limit),
                None => None,
            }
        } else {
            None
        };
        ReleaseStgMedium(&mut m);
        res
    }
}

fn copy_stream(s: &IStream, f: &mut std::fs::File, limit: u64) -> Option<u64> {
    let mut buf = vec![0u8; 256 << 10];
    let mut total = 0u64;
    loop {
        let mut got = 0u32;
        let hr = unsafe { s.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut got)) };
        if hr.is_err() {
            return None;
        }
        if got == 0 {
            return Some(total);
        }
        total += got as u64;
        if total > limit {
            return None;
        }
        f.write_all(&buf[..got as usize]).ok()?;
    }
}

/// Creates `<name> - Shortcut.lnk` in `folder` for each target (a free name).
pub fn make_links(targets: &[PathBuf], folder: &Path) -> Result<usize, String> {
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let mut made = 0;
    for t in targets {
        let name = t.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| t.to_string_lossy().trim_end_matches(['\\', ':']).to_string());
        let mut dest = folder.join(format!("{name} - Shortcut.lnk"));
        let mut k = 2;
        while dest.exists() && k < 1000 {
            dest = folder.join(format!("{name} - Shortcut ({k}).lnk"));
            k += 1;
        }
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("ShellLink {:?}", e.code()))?;
            let tw = crate::wide(&t.to_string_lossy());
            link.SetPath(PCWSTR(tw.as_ptr())).map_err(|e| format!("SetPath {:?}", e.code()))?;
            let pf: IPersistFile = link.cast().map_err(|e| format!("IPersistFile {:?}", e.code()))?;
            let dw = crate::wide(&dest.to_string_lossy());
            pf.Save(PCWSTR(dw.as_ptr()), true).map_err(|e| format!("Save {:?}", e.code()))?;
        }
        made += 1;
    }
    Ok(made)
}
