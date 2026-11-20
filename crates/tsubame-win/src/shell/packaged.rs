//! Win11 packaged context-menu commands (CM7): the `IExplorerCommand`
//! handlers that UWP/MSIX apps register under
//! `windows.fileExplorerContextMenus` and that Explorer's classic
//! `IContextMenu` (the "Show more options" menu) does not list, for example
//! "Open in Terminal". Tsubame merges them into its one menu, after the
//! classic rows (`ctxmodel::compose_all`).
//!
//! Discovery (`docs/ctxmenu-spike.md` section 3): packages with packaged COM
//! are listed under `HKCR\PackagedCom\Package`; each one's `AppxManifest.xml`
//! (package root from the per-user AppModel repository, or `WindowsApps` for
//! sparse packages) names item types and handler CLSIDs. The scan is cached
//! per process ([`handlers`]) and refreshed after [`SCAN_TTL`].
//!
//! Everything COM here runs on the shell STA worker that holds the menu
//! (`ctxmenu::run`), after the classic top level has been sent, so the
//! packaged step never delays the first rows. Pure parts (manifest parsing,
//! type matching, ids) are unit tested.

use super::ctxmodel::RawItem;
use crate::bitmap;
use crate::wide;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Command ids of packaged items: never inside the HMENU's `ID_FIRST..=ID_LAST`
/// (`0x0001..=0x7eff`) or the native menu's app range (`0x7f00..`).
pub const ID_FIRST: u32 = 0xA000;
pub const ID_LAST: u32 = 0xBFFF;
/// Submenu tokens of packaged items (classic tokens are 1..=512).
pub const SUB_FIRST: u32 = 0x0001_0000;
pub const SUB_LAST: u32 = 0x0001_FFFF;
/// Re-read the manifests after this long (apps get installed while the app runs).
pub const SCAN_TTL: Duration = Duration::from_secs(60);
/// Upper bound of packaged commands per menu (a broken handler must not make it huge).
pub const MAX_COMMANDS: usize = 64;

pub fn is_id(id: u32) -> bool {
    (ID_FIRST..=ID_LAST).contains(&id)
}

pub fn is_sub(sub: u32) -> bool {
    (SUB_FIRST..=SUB_LAST).contains(&sub)
}

/// One `ItemType`/`Verb` pair of a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handler {
    /// package family part of the full name (for reports)
    pub package: String,
    /// item type, lower-case: `*`, `.txt`, `directory`, `directory\background`, ...
    pub ty: String,
    /// `{xxxxxxxx-...}`, lower-case, with braces
    pub clsid: String,
}

/// `"CA6CC9F1-867A-..."` or `"{ca6c...}"` -> `"{ca6cc9f1-867a-...}"`.
pub fn norm_clsid(s: &str) -> String {
    format!("{{{}}}", s.trim().trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase())
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    // ` Name="value"` (manifests are machine-written: double quotes)
    let key = format!("{name}=\"");
    let mut from = 0;
    while let Some(i) = tag[from..].find(&key) {
        let at = from + i;
        let before = tag[..at].chars().last();
        if before.is_some_and(char::is_whitespace) {
            let v = &tag[at + key.len()..];
            return v.find('"').map(|e| &v[..e]);
        }
        from = at + key.len();
    }
    None
}

/// (item type, CLSID) pairs of every `windows.fileExplorerContextMenus`
/// extension in an `AppxManifest.xml` (any namespace prefix). Types are
/// lower-case, CLSIDs normalised ([`norm_clsid`]). Pure.
pub fn parse_manifest(xml: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("windows.fileExplorerContextMenus") {
        rest = &rest[i..];
        // the block ends at this extension's closing tag
        let end = rest.find("Extension>").unwrap_or(rest.len());
        let block = &rest[..end];
        let mut b = block;
        // each `<xx:ItemType Type="...">` ... `</xx:ItemType>`
        while let Some(p) = b.find("ItemType") {
            let tag_end = b[p..].find('>').map(|e| p + e).unwrap_or(b.len());
            let tag = &b[p..tag_end];
            let self_closing = tag.ends_with('/');
            let ty = attr(tag, "Type").map(|t| t.to_lowercase());
            let body_end = if self_closing { tag_end } else { b[tag_end..].find("ItemType>").map(|e| tag_end + e).unwrap_or(b.len()) };
            let body = &b[tag_end.min(body_end)..body_end];
            if let Some(ty) = ty {
                let mut v = body;
                while let Some(c) = v.find("Clsid=\"") {
                    v = &v[c + 7..];
                    let id = &v[..v.find('"').unwrap_or(0)];
                    if !id.is_empty() {
                        let pair = (ty.clone(), norm_clsid(id));
                        if !out.contains(&pair) {
                            out.push(pair);
                        }
                    }
                }
            }
            b = &b[(body_end + "ItemType>".len()).min(b.len())..];
        }
        rest = &rest[end.max(1).min(rest.len())..];
    }
    out
}

/// What a right-clicked thing is, for the manifest's item types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemClass {
    File,
    Folder,
    Drive,
    Background,
}

/// Item types that apply to one item (lower-case), in the manifest's terms.
pub fn types_for(class: ItemClass, ext: &str) -> Vec<String> {
    match class {
        ItemClass::Background => vec!["directory\\background".into()],
        ItemClass::Drive => vec!["drive".into(), "folder".into()],
        ItemClass::Folder => vec!["directory".into(), "folder".into(), "allfilesystemobjects".into()],
        ItemClass::File => {
            let mut v = vec!["*".to_string(), "allfilesystemobjects".into()];
            if !ext.is_empty() {
                v.push(format!(".{}", ext.trim_start_matches('.').to_lowercase()));
            }
            v
        }
    }
}

/// CLSIDs of `handlers` that apply to every item (each item's types from
/// [`types_for`]), in scan order, each once, minus `skip` (CLSIDs the classic
/// menu already has as a verb). Pure.
pub fn applicable(handlers: &[Handler], items: &[Vec<String>], skip: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for h in handlers {
        if out.contains(&h.clsid) || skip.contains(&h.clsid) {
            continue;
        }
        let ok = !items.is_empty() && items.iter().all(|types| handlers.iter().any(|o| o.clsid == h.clsid && types.contains(&o.ty)));
        if ok {
            out.push(h.clsid.clone());
        }
    }
    out
}

/// Lexical: `C:\` or `\\server\share\`.
fn is_root(path: &str) -> bool {
    let p = std::path::Path::new(path);
    p.has_root() && p.parent().is_none()
}

/// The class of one path (worker thread: may touch the disk).
pub fn class_of(path: &str) -> (ItemClass, String) {
    if is_root(path) {
        return (ItemClass::Drive, String::new());
    }
    let p = std::path::Path::new(path);
    if p.is_dir() {
        return (ItemClass::Folder, String::new());
    }
    (ItemClass::File, p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default())
}

// ---- the cached scan ---------------------------------------------------------

mod reg {
    use crate::wide;
    use windows::Win32::System::Registry::*;
    use windows::core::{PCWSTR, PWSTR};

    pub fn subkeys(root: HKEY, path: &str) -> Vec<String> {
        let w = wide(path);
        let mut hk = HKEY::default();
        if unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), Some(0), KEY_READ, &mut hk) }.is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 260];
            let mut n = buf.len() as u32;
            if unsafe { RegEnumKeyExW(hk, i, Some(PWSTR(buf.as_mut_ptr())), &mut n, None, None, None, None) }.is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..n as usize]));
        }
        unsafe {
            let _ = RegCloseKey(hk);
        }
        out
    }

    pub fn string(root: HKEY, path: &str, name: &str) -> Option<String> {
        let (p, n) = (wide(path), wide(name));
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        unsafe { RegGetValueW(root, PCWSTR(p.as_ptr()), if name.is_empty() { PCWSTR::null() } else { PCWSTR(n.as_ptr()) }, RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)) }.ok().ok()?;
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// Every packaged context-menu handler on this PC (registry + manifests).
fn scan() -> Vec<Handler> {
    use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};
    let repo = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";
    let pf = std::env::var("ProgramW6432").unwrap_or_else(|_| "C:\\Program Files".into());
    let mut out = Vec::new();
    for full in reg::subkeys(HKEY_CLASSES_ROOT, "PackagedCom\\Package") {
        let root = reg::string(HKEY_CURRENT_USER, &format!("{repo}\\{full}"), "PackageRootFolder").unwrap_or_default();
        let mut cands = Vec::with_capacity(2);
        if !root.is_empty() {
            cands.push(format!("{root}\\AppxManifest.xml"));
        }
        cands.push(format!("{pf}\\WindowsApps\\{full}\\AppxManifest.xml"));
        let Some(xml) = cands.iter().find_map(|p| std::fs::read_to_string(p).ok().filter(|x| x.contains("windows.fileExplorerContextMenus"))) else { continue };
        let package = full.split('_').next().unwrap_or(&full).to_string();
        for (ty, clsid) in parse_manifest(&xml) {
            out.push(Handler { package: package.clone(), ty, clsid });
        }
    }
    out
}

static CACHE: Mutex<Option<(Instant, Arc<Vec<Handler>>)>> = Mutex::new(None);

/// The cached scan (refreshed after [`SCAN_TTL`]); true when it scanned now.
pub fn handlers() -> (Arc<Vec<Handler>>, bool) {
    if let Some((at, h)) = CACHE.lock().unwrap().as_ref()
        && at.elapsed() < SCAN_TTL
    {
        return (h.clone(), false);
    }
    // scan outside the lock: two workers may both scan once, that is fine
    let h = Arc::new(scan());
    *CACHE.lock().unwrap() = Some((Instant::now(), h.clone()));
    (h, true)
}

// ---- one menu's packaged commands (worker side) ----------------------------

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, CLSIDFromString, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};
use windows::core::{GUID, PCWSTR, PWSTR};

/// The packaged commands of one open menu. Lives on the worker next to the
/// classic `IContextMenu`; ids and tokens index into it.
pub(super) struct Session {
    arr: IShellItemArray,
    icon_px: u32,
    /// command id `ID_FIRST + i`
    cmds: Vec<IExplorerCommand>,
    /// submenu token `SUB_FIRST + i`
    subs: Vec<IExplorerCommand>,
    /// handlers that failed or were hidden (for the report)
    pub skipped: u32,
}

/// `PWSTR` from `CoTaskMemAlloc` -> String, freed.
fn take(p: windows::core::Result<PWSTR>) -> Option<String> {
    let p = p.ok()?;
    if p.is_null() {
        return None;
    }
    let s = unsafe { p.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s.filter(|s| !s.is_empty())
}

impl Session {
    /// The shell item array for `paths` (background: the folder itself).
    pub fn new(paths: &[String], icon_px: u32) -> Option<Session> {
        let pidls = super::ctxmenu::parse(paths).ok()?;
        let ptrs: Vec<*const windows::Win32::UI::Shell::Common::ITEMIDLIST> = pidls.0.iter().map(|p| *p as *const _).collect();
        let arr = unsafe { SHCreateShellItemArrayFromIDLists(&ptrs) }.ok()?;
        Some(Session { arr, icon_px, cmds: Vec::new(), subs: Vec::new(), skipped: 0 })
    }

    /// The top-level packaged items for `classes` (one entry per selected
    /// item), skipping CLSIDs in `skip`. Creates each handler, asks its
    /// state (hidden ones drop out) and walks one level.
    pub fn top(&mut self, classes: &[(ItemClass, String)], skip: &[String]) -> Vec<RawItem> {
        let (all, _) = handlers();
        let types: Vec<Vec<String>> = classes.iter().map(|(c, e)| types_for(*c, e)).collect();
        let mut out = Vec::new();
        for clsid in applicable(&all, &types, skip) {
            if self.cmds.len() + self.subs.len() >= MAX_COMMANDS {
                break;
            }
            let w = wide(&clsid);
            let Ok(guid): windows::core::Result<GUID> = (unsafe { CLSIDFromString(PCWSTR(w.as_ptr())) }) else {
                self.skipped += 1;
                continue;
            };
            let cmd: IExplorerCommand = match unsafe { CoCreateInstance(&guid, None, CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER) } {
                Ok(c) => c,
                Err(_) => {
                    self.skipped += 1;
                    continue;
                }
            };
            match self.raw(&cmd, Some(&clsid)) {
                Some(mut r) => {
                    // the family keys the user's edits (`windows.pkg:<family>/{clsid}`)
                    r.package = all.iter().find(|h| h.clsid == clsid).map(|h| h.package.clone()).filter(|p| !p.is_empty());
                    out.push(r)
                }
                None => self.skipped += 1,
            }
        }
        out
    }

    /// One command as a menu item (None: hidden, no title, or over the cap).
    fn raw(&mut self, cmd: &IExplorerCommand, clsid: Option<&str>) -> Option<RawItem> {
        let flags = unsafe { cmd.GetFlags() }.unwrap_or(0) as i32;
        if flags & ECF_ISSEPARATOR.0 != 0 {
            return Some(RawItem { separator: true, ..Default::default() });
        }
        let state = unsafe { cmd.GetState(&self.arr, true) }.unwrap_or(0) as i32;
        if state & ECS_HIDDEN.0 != 0 {
            return None;
        }
        let text = take(unsafe { cmd.GetTitle(&self.arr) })?;
        let help = take(unsafe { cmd.GetToolTip(&self.arr) });
        let icon = take(unsafe { cmd.GetIcon(&self.arr) }).and_then(|loc| load_icon(&loc, self.icon_px)).map(Arc::new);
        // the CLSID is the stable identity (with the package family, set by
        // `top`, it keys the user's edits: `windows.pkg:<family>/{clsid}`);
        // sub-commands use their canonical name if they have one
        let verb = clsid.map(str::to_string).or_else(|| unsafe { cmd.GetCanonicalName() }.ok().filter(|g| *g != GUID::zeroed()).map(|g| norm_clsid(&format!("{g:?}"))));
        let mut r = RawItem { text, help, verb, icon, disabled: state & ECS_DISABLED.0 != 0, checked: state & ECS_CHECKED.0 != 0, radio: state & ECS_RADIOCHECK.0 != 0, ..Default::default() };
        if flags & ECF_HASSUBCOMMANDS.0 != 0 {
            if SUB_FIRST as usize + self.subs.len() > SUB_LAST as usize {
                return None;
            }
            self.subs.push(cmd.clone());
            r.submenu = Some(Vec::new());
            r.sub = SUB_FIRST + self.subs.len() as u32 - 1;
        } else {
            if ID_FIRST as usize + self.cmds.len() > ID_LAST as usize {
                return None;
            }
            self.cmds.push(cmd.clone());
            r.id = ID_FIRST + self.cmds.len() as u32 - 1;
        }
        Some(r)
    }

    /// The children of packaged submenu token `sub` (one level; nested ones
    /// get tokens of their own). None: not a token of this menu.
    pub fn expand(&mut self, sub: u32) -> Option<Vec<RawItem>> {
        let cmd = self.subs.get((sub.checked_sub(SUB_FIRST)?) as usize)?.clone();
        let mut out = Vec::new();
        let Ok(en) = (unsafe { cmd.EnumSubCommands() }) else { return Some(out) };
        loop {
            let mut one: [Option<IExplorerCommand>; 1] = [None];
            let mut got = 0u32;
            let hr = unsafe { en.Next(&mut one, Some(&mut got)) };
            if hr.is_err() || got == 0 {
                break;
            }
            let Some(c) = one[0].take() else { break };
            if let Some(r) = self.raw(&c, None) {
                out.push(r);
            }
            if out.len() >= MAX_COMMANDS {
                break;
            }
        }
        Some(out)
    }

    /// `IExplorerCommand::Invoke` for command id `id` on the selection.
    pub fn invoke(&self, id: u32) -> (Option<String>, Result<(), String>) {
        let Some(cmd) = id.checked_sub(ID_FIRST).and_then(|i| self.cmds.get(i as usize)) else {
            return (None, Err(format!("id {id} is not a packaged command")));
        };
        let r = unsafe { cmd.Invoke(&self.arr, None) };
        (None, r.map_err(|e| format!("IExplorerCommand::Invoke {:?}", e.code())))
    }
}

/// An icon location (`C:\x\app.exe,-101`, `file.ico`) as a premultiplied
/// bitmap of `px`. None for indirect (`@{...}`, `ms-resource:`) locations.
fn load_icon(loc: &str, px: u32) -> Option<bitmap::Bitmap> {
    let loc = loc.trim().trim_matches('"');
    if loc.is_empty() || loc.starts_with('@') || loc.contains("ms-resource:") {
        return None;
    }
    let mut buf: Vec<u16> = loc.encode_utf16().chain(std::iter::repeat_n(0, 8)).collect();
    buf.resize(buf.len().max(261), 0);
    let index = unsafe { PathParseIconLocationW(PWSTR(buf.as_mut_ptr())) };
    let mut large = HICON::default();
    let hr = unsafe { SHDefExtractIconW(PCWSTR(buf.as_ptr()), index, 0, Some(&mut large), None, px & 0xffff) };
    if hr.is_err() || large.is_invalid() {
        return None;
    }
    let b = bitmap::from_hicon(large);
    unsafe {
        let _ = DestroyIcon(large);
    }
    b.map(|b| b.fit_within(px))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns:desktop4="http://schemas.microsoft.com/appx/manifest/desktop/windows10/4" xmlns:desktop5="http://schemas.microsoft.com/appx/manifest/desktop/windows10/5">
  <Applications><Application Id="App"><Extensions>
    <desktop4:Extension Category="windows.fileExplorerContextMenus">
      <desktop4:FileExplorerContextMenus>
        <desktop5:ItemType Type="Directory">
          <desktop5:Verb Id="OpenTerminal" Clsid="9F156763-7844-4DC4-B2B1-901F640F5155" />
        </desktop5:ItemType>
        <desktop5:ItemType Type="Directory\Background">
          <desktop5:Verb Id="OpenTerminal" Clsid="9F156763-7844-4DC4-B2B1-901F640F5155" />
        </desktop5:ItemType>
        <desktop5:ItemType Type=".TXT">
          <desktop5:Verb Id="A" Clsid="{11111111-2222-3333-4444-555555555555}" />
          <desktop5:Verb Id="B" Clsid="aaaaaaaa-2222-3333-4444-555555555555" />
        </desktop5:ItemType>
      </desktop4:FileExplorerContextMenus>
    </desktop4:Extension>
    <com:Extension Category="windows.comServer"><com:ComServer><com:SurrogateServer><com:Class Id="9F156763-7844-4DC4-B2B1-901F640F5155" Path="x.dll" ThreadingModel="STA"/></com:SurrogateServer></com:ComServer></com:Extension>
    <desktop4:Extension Category="windows.fileExplorerContextMenus">
      <desktop4:FileExplorerContextMenus>
        <desktop5:ItemType Type="*"><desktop5:Verb Id="C" Clsid="cccccccc-2222-3333-4444-555555555555" /></desktop5:ItemType>
      </desktop4:FileExplorerContextMenus>
    </desktop4:Extension>
  </Extensions></Application></Applications>
</Package>"#;

    const TERM: &str = "{9f156763-7844-4dc4-b2b1-901f640f5155}";

    #[test]
    fn manifest_types_and_clsids_from_every_extension() {
        let v = parse_manifest(MANIFEST);
        assert_eq!(
            v,
            vec![
                ("directory".to_string(), TERM.to_string()),
                ("directory\\background".into(), TERM.into()),
                (".txt".into(), "{11111111-2222-3333-4444-555555555555}".into()),
                (".txt".into(), "{aaaaaaaa-2222-3333-4444-555555555555}".into()),
                ("*".into(), "{cccccccc-2222-3333-4444-555555555555}".into()),
            ],
            "the comServer Class Id is not a menu verb"
        );
        assert!(parse_manifest("<Package/>").is_empty());
        // a truncated manifest does not panic
        let cut = &MANIFEST[..MANIFEST.find("Type=\".TXT\"").unwrap() + 8];
        assert_eq!(parse_manifest(cut).len(), 2);
    }

    #[test]
    fn handlers_apply_per_item_class_and_skip_classic_verbs() {
        let hs: Vec<Handler> = parse_manifest(MANIFEST).into_iter().map(|(ty, clsid)| Handler { package: "p".into(), ty, clsid }).collect();
        let file = |e: &str| types_for(ItemClass::File, e);
        assert_eq!(applicable(&hs, &[types_for(ItemClass::Folder, "")], &[]), vec![TERM.to_string()]);
        assert_eq!(applicable(&hs, &[types_for(ItemClass::Background, "")], &[]), vec![TERM.to_string()]);
        let txt = applicable(&hs, &[file("txt")], &[]);
        assert_eq!(txt.len(), 3, "txt-only plus the * handler: {txt:?}");
        assert_eq!(applicable(&hs, &[file("png")], &[]), vec!["{cccccccc-2222-3333-4444-555555555555}".to_string()]);
        // a selection: only handlers every item takes
        assert_eq!(applicable(&hs, &[file("txt"), file("png")], &[]), vec!["{cccccccc-2222-3333-4444-555555555555}".to_string()]);
        assert!(applicable(&hs, &[file("txt"), types_for(ItemClass::Folder, "")], &[]).is_empty());
        // a CLSID the classic HMENU has as a verb is not created again
        assert_eq!(applicable(&hs, &[types_for(ItemClass::Folder, "")], &[TERM.to_string()]), Vec::<String>::new());
        assert!(applicable(&hs, &[], &[]).is_empty());
        assert_eq!(norm_clsid(" {ABC} "), "{abc}");
    }

    #[test]
    fn id_and_token_ranges_do_not_overlap_the_classic_menu() {
        assert!(!is_id(super::super::menu::ID_LAST) && !is_id(0x7f00) && is_id(ID_FIRST) && is_id(ID_LAST));
        assert!(!is_sub(super::super::ctxmodel::MAX_ITEMS as u32) && is_sub(SUB_FIRST));
        assert_eq!(class_of(r"C:\").0, ItemClass::Drive);
        assert_eq!(class_of(r"Z:\no\such\file.TXT"), (ItemClass::File, "txt".into()));
    }

    /// Manual probe: `TSUBAME_PKG_PROBE=<path> cargo test -p tsubame-win probe -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn probe() {
        let path = std::env::var("TSUBAME_PKG_PROBE").unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let (all, _) = handlers();
        for h in all.iter() {
            println!("handler {} {} {}", h.package, h.ty, h.clsid);
        }
        let class = class_of(&path);
        let types = types_for(class.0, &class.1);
        println!("class {class:?} types {types:?} applicable {:?}", applicable(&all, std::slice::from_ref(&types), &[]));
        let mut s = Session::new(std::slice::from_ref(&path), 32).expect("session");
        for clsid in applicable(&all, &[types], &[]) {
            let w = wide(&clsid);
            let guid = unsafe { CLSIDFromString(PCWSTR(w.as_ptr())) };
            let made: windows::core::Result<IExplorerCommand> = guid.and_then(|g| unsafe { CoCreateInstance(&g, None, CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER) });
            match made {
                Err(e) => println!("{clsid}: create {:?}", e.code()),
                Ok(c) => {
                    let st = unsafe { c.GetState(&s.arr, true) };
                    let t = unsafe { c.GetTitle(&s.arr) }.map(|p| unsafe { p.to_string() });
                    println!("{clsid}: flags {:?} state {:?} title {:?}", unsafe { c.GetFlags() }, st, t);
                }
            }
        }
        println!("top {:?}", s.top(&[class_of(&path)], &[]).iter().map(|r| r.text.clone()).collect::<Vec<_>>());
    }
}
