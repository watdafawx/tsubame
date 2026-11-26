//! Headless OLE tests: our data object read back through `IDataObject`,
//! the real clipboard round trip (OleSetClipboard/OleGetClipboard on this
//! test's STA), virtual-file staging and shortcut creation. No window, no
//! drag (DoDragDrop needs a mouse; that part is tested by hand on a test PC).
//!
//! The clipboard test replaces the user's clipboard for a moment; it puts
//! back any text that was there.

use std::path::PathBuf;
use tsubame_win::dnd::clipboard::{self, ClipJob, ClipReply};
use tsubame_win::dnd::formats::{self, FileDescriptor};
use tsubame_win::dnd::{self, DataObject, EFFECT_COPY, EFFECT_MOVE};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT, OleInitialize, OleUninitialize};

struct Ole;
impl Ole {
    fn new() -> Ole {
        unsafe { OleInitialize(None) }.expect("OleInitialize");
        Ole
    }
}
impl Drop for Ole {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tsubame-dnd-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn data_object_round_trip() {
    let _ole = Ole::new();
    let paths = vec![PathBuf::from(r"C:\Windows\notepad.exe"), PathBuf::from(r"D:\ünï code\😀 x.txt")];
    let obj = DataObject::files(&paths, Some(EFFECT_MOVE));
    assert!(dnd::has_format(&obj, CF_HDROP.0));
    assert!(dnd::has_format(&obj, dnd::cf_preferred_effect()));
    assert!(!dnd::has_format(&obj, CF_UNICODETEXT.0));
    let info = dnd::read_info(&obj);
    assert_eq!(info.paths, paths);
    assert_eq!(info.preferred, Some(EFFECT_MOVE));
    assert_eq!(info.rejected, 0);

    // raw CF_HDROP bytes are what we built
    let raw = dnd::get_bytes(&obj, CF_HDROP.0, -1, 1 << 20).unwrap();
    assert_eq!(raw, formats::hdrop_bytes(&paths));

    // a target reports back (SetData), we read it
    dnd::set_dword(&obj, dnd::cf_performed_effect(), EFFECT_COPY);
    assert_eq!(dnd::get_bytes(&obj, dnd::cf_performed_effect(), -1, 64).and_then(|b| formats::parse_effect(&b)), Some(EFFECT_COPY));
    dnd::set_dword(&obj, dnd::cf_performed_effect(), EFFECT_MOVE);
    assert_eq!(dnd::get_bytes(&obj, dnd::cf_performed_effect(), -1, 64).and_then(|b| formats::parse_effect(&b)), Some(EFFECT_MOVE));

    // the format enumerator lists all three
    use windows::Win32::System::Com::{DATADIR_GET, FORMATETC};
    let e = unsafe { obj.EnumFormatEtc(DATADIR_GET.0 as u32) }.unwrap();
    let mut buf = [FORMATETC::default(); 8];
    let mut got = 0u32;
    let _ = unsafe { e.Next(&mut buf, Some(&mut got)) };
    assert_eq!(got, 3);
    assert_eq!(buf[0].cfFormat, CF_HDROP.0);

    // dangerous paths are dropped on read
    let obj = DataObject::files(&[PathBuf::from(r"\\?\GLOBALROOT\Device\X"), PathBuf::from(r"C:\ok.txt"), PathBuf::from(r"C:\a.txt:ads")], None);
    let info = dnd::read_info(&obj);
    assert_eq!(info.paths, vec![PathBuf::from(r"C:\ok.txt")]);
    assert_eq!(info.rejected, 2);
}

fn clip_text() -> Option<String> {
    // The clipboard is system-wide: a clipboard history or manager can hold it open for a moment
    // right after a change (CLIPBRD_E_CANT_OPEN), so wait a little instead of reading "nothing".
    let mut tries = 0;
    let obj = loop {
        match unsafe { windows::Win32::System::Ole::OleGetClipboard() } {
            Ok(o) => break o,
            Err(_) if tries < 40 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    };
    let b = dnd::get_bytes(&obj, CF_UNICODETEXT.0, -1, 16 << 20)?;
    let u: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let end = u.iter().position(|&c| c == 0).unwrap_or(u.len());
    Some(String::from_utf16_lossy(&u[..end]))
}

#[test]
fn clipboard_round_trip() {
    let _ole = Ole::new();
    let saved = clip_text();
    let staging = tmp("clip-staging");
    let paths = vec![PathBuf::from(r"C:\Windows\win.ini"), PathBuf::from(r"C:\Windows\System32")];
    let read = |consume| match clipboard::run(ClipJob::Read { staging: staging.clone(), cap: 1 << 20, consume_cut: consume }) {
        ClipReply::Read(Ok(r)) => r,
        other => panic!("read: {other:?}"),
    };

    // copy
    let ClipReply::Set(Ok(seq)) = clipboard::run(ClipJob::SetFiles { paths: paths.clone(), cut: false }) else { panic!("set") };
    assert_ne!(seq, 0);
    let r = read(true);
    assert_eq!(r.info.paths, paths);
    assert_eq!(r.effect, EFFECT_COPY);
    // a copy stays on the clipboard
    assert_eq!(read(false).info.paths, paths);

    // cut: MOVE, and consumed by a paste
    let ClipReply::Set(Ok(_)) = clipboard::run(ClipJob::SetFiles { paths: paths.clone(), cut: true }) else { panic!("set") };
    let r = read(false);
    assert_eq!((r.info.paths.clone(), r.effect), (paths.clone(), EFFECT_MOVE));
    let r = read(true);
    assert_eq!(r.effect, EFFECT_MOVE);
    assert!(read(false).info.paths.is_empty(), "a pasted cut is gone");

    // text (copy path)
    let ClipReply::Set(Ok(_)) = clipboard::run(ClipJob::SetText("C:\\a b\\ü.txt".into())) else { panic!("set") };
    assert_eq!(clip_text().as_deref(), Some("C:\\a b\\ü.txt"));
    assert!(read(false).info.paths.is_empty());

    // put the user's text back
    if let Some(t) = saved {
        let _ = clipboard::run(ClipJob::SetText(t));
    }
    let _ = std::fs::remove_dir_all(&staging);
}

#[test]
fn virtual_files_are_staged_under_caps() {
    let _ole = Ole::new();
    let descs = vec![FileDescriptor { name: "mail.eml".into(), size: Some(11), is_dir: false }];
    let obj = DataObject::with(vec![(dnd::cf_file_group_descriptor(), formats::group_descriptor_bytes(&descs)), (dnd::cf_file_contents(), b"hello world".to_vec())]);
    let info = dnd::read_info(&obj);
    assert!(info.paths.is_empty());
    assert_eq!(info.virtual_items, descs);

    let root = tmp("stage");
    let (tops, skipped) = clipboard::stage(&obj, &info.virtual_items, &root.join("a"), 1 << 20);
    assert_eq!((tops.clone(), skipped), (vec![root.join("a").join("mail.eml")], 0));
    assert_eq!(std::fs::read(&tops[0]).unwrap(), b"hello world");

    // declared size over the cap: skipped, nothing written
    let (tops, skipped) = clipboard::stage(&obj, &info.virtual_items, &root.join("b"), 5);
    assert_eq!((tops.len(), skipped), (0, 1));
    // undeclared size, real content over the cap: skipped and removed
    let nosize = vec![FileDescriptor { name: "x.bin".into(), size: None, is_dir: false }];
    let (tops, skipped) = clipboard::stage(&obj, &nosize, &root.join("c"), 5);
    assert_eq!((tops.len(), skipped), (0, 1));
    assert!(!root.join("c").join("x.bin").exists());
    // names that escape the staging folder are refused
    let evil = vec![
        FileDescriptor { name: r"..\escape.txt".into(), size: Some(1), is_dir: false },
        FileDescriptor { name: r"C:\abs.txt".into(), size: Some(1), is_dir: false },
        FileDescriptor { name: "s.txt:ads".into(), size: Some(1), is_dir: false },
    ];
    let (tops, skipped) = clipboard::stage(&obj, &evil, &root.join("d"), 1 << 20);
    assert_eq!((tops.len(), skipped), (0, 3));
    assert!(!root.join("escape.txt").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn shortcuts() {
    let _ole = Ole::new();
    let root = tmp("links");
    let target = root.join("t.txt");
    std::fs::write(&target, b"x").unwrap();
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    assert_eq!(clipboard::make_links(&[target.clone(), target.clone()], &out), Ok(vec!["t - Shortcut.lnk".to_string(), "t - Shortcut (2).lnk".to_string()]));
    assert!(out.join("t - Shortcut.lnk").exists());
    assert!(out.join("t - Shortcut (2).lnk").exists());
    let _ = std::fs::remove_dir_all(&root);
}
