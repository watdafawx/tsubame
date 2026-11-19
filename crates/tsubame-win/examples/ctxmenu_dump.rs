//! CM1 spike harness: prints the shell context menu Tsubame would draw, with
//! timings, per-handler costs, packaged (Win11) handlers, invoke results and a
//! leak loop. TEST MACHINE ONLY: it loads shell extensions, and `--native` and some
//! invokes open windows. See `docs/ctxmenu-spike.md`.
//!
//! ```text
//! ctxmenu_dump [options] <path>...
//!   --bg              background menu of the folder <path>
//!   --ext             extended verbs (Shift)
//!   --top             top level only (no submenu walk)
//!   --repeat N        build N times in this process (1st = cold), timings only after the 1st
//!   --handlers        time every registered ContextMenuHandler on its own
//!   --packaged        list packaged IExplorerCommand handlers (fileExplorerContextMenus)
//!                     for the item and ask each for title/state
//!   --invoke VERB     build the model, then invoke the item with this verb (or #id / label:Text)
//!   --no-ui           with --invoke: CMIC_MASK_FLAG_NO_UI
//!   --hold S          keep the process (and its dialogs) alive S seconds at the end
//!   --cycles N        leak loop: N menus through the model path, counters at 10/100/N
//!   --native          with --cycles: the native TrackPopupMenu path instead (auto-closed)
//!   --skip-model      no model build (so --handlers / --packaged times are cold)
//! ```

use std::time::{Duration, Instant};
use tsubame_win::procinfo;
use tsubame_win::shell::ctxmodel::{self, MenuItem, Part};
use tsubame_win::shell::{CtxCmd, CtxHandle, CtxOpts, MenuOpts, Payload, Pool, Request};

struct Args {
    paths: Vec<String>,
    bg: bool,
    ext: bool,
    top: bool,
    repeat: usize,
    handlers: bool,
    packaged: bool,
    invoke: Option<String>,
    no_ui: bool,
    hold: u64,
    cycles: usize,
    native: bool,
    skip_model: bool,
}

fn args() -> Args {
    let mut a = Args { paths: vec![], bg: false, ext: false, top: false, repeat: 1, handlers: false, packaged: false, invoke: None, no_ui: false, hold: 0, cycles: 0, native: false, skip_model: false };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--bg" => a.bg = true,
            "--ext" => a.ext = true,
            "--top" => a.top = true,
            "--repeat" => a.repeat = it.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--handlers" => a.handlers = true,
            "--packaged" => a.packaged = true,
            "--invoke" => a.invoke = it.next(),
            "--no-ui" => a.no_ui = true,
            "--hold" => a.hold = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--cycles" => a.cycles = it.next().and_then(|v| v.parse().ok()).unwrap_or(200),
            "--native" => a.native = true,
            "--skip-model" => a.skip_model = true,
            _ => a.paths.push(s),
        }
    }
    a
}

fn ms(us: u64) -> String {
    format!("{:.1}", us as f64 / 1000.0)
}

/// Waits for batches of one request until `Closed`/`Failed` or `stop` says so.
fn pump_batches(pool: &mut Pool, id: u64, timeout: Duration, mut on: impl FnMut(&ctxmodel::MenuBatch, u64) -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < timeout {
        for r in pool.drain() {
            if r.id != id {
                continue;
            }
            if let Payload::Ctx(b) = &r.payload {
                let at = t.elapsed().as_micros() as u64;
                if !on(b, at) || matches!(b.part, Part::Closed | Part::Failed(_)) {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

fn opts(a: &Args) -> CtxOpts {
    CtxOpts { background: a.bg, extended: a.ext, submenus: !a.top, ..Default::default() }
}

fn find<'a>(items: &'a [MenuItem], want: &str) -> Option<&'a MenuItem> {
    for it in items {
        let hit = if let Some(id) = want.strip_prefix('#') {
            id.parse::<u32>().ok() == Some(it.id)
        } else if let Some(l) = want.strip_prefix("label:") {
            it.label.eq_ignore_ascii_case(l)
        } else {
            it.verb.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(want))
        };
        if hit && it.kind == ctxmodel::ItemKind::Command {
            return Some(it);
        }
        if let Some(c) = find(&it.children, want) {
            return Some(c);
        }
    }
    None
}

fn count_kinds(items: &[MenuItem], acc: &mut [usize; 6]) {
    for it in items {
        if it.is_separator() {
            continue;
        }
        acc[0] += 1;
        if it.icon.is_some() {
            acc[1] += 1;
        }
        if it.flags & ctxmodel::flag::OWNERDRAW != 0 {
            acc[2] += 1;
        }
        if it.flags & ctxmodel::flag::ICON_CALLBACK != 0 {
            acc[3] += 1;
        }
        if it.verb.is_some() {
            acc[4] += 1;
        }
        if it.flags & ctxmodel::flag::LAZY != 0 {
            acc[5] += 1;
        }
        count_kinds(&it.children, acc);
    }
}

fn model_run(pool: &mut Pool, a: &Args, print: bool) -> Option<(u64, u64, [u64; 3], Vec<MenuItem>)> {
    let h = CtxHandle::new();
    let t = Instant::now();
    let tk = pool.submit(Request::ctx(a.paths.clone(), 1, opts(a), &h));
    let (mut first, mut full, mut timing, mut items) = (None, None, [0; 3], Vec::new());
    let mut nat = 0;
    let mut icons = 0;
    let mut invoked = None;
    let mut failed = None;
    let done = pump_batches(pool, tk.id, Duration::from_secs(30), |b, _| {
        let at = t.elapsed().as_micros() as u64;
        match &b.part {
            Part::Native(v) => {
                nat += 1;
                if first.is_none() {
                    first = Some(at);
                }
                full = Some(at);
                timing = b.timing;
                items = v.clone();
                // the full tree comes second when there are submenus
                let has_sub = v.iter().any(|i| i.kind == ctxmodel::ItemKind::Submenu);
                let complete = a.top || !has_sub || nat >= 2;
                if complete && a.invoke.is_none() {
                    h.close();
                }
                if complete && let Some(w) = &a.invoke {
                    match find(v, w) {
                        Some(it) => {
                            println!("invoke: '{}' id {} verb {:?} route {:?}", it.label, it.id, it.verb, ctxmodel::route(it));
                            h.send(CtxCmd::Invoke { id: it.id, x: 400, y: 300, shift: false, ctrl: false, no_ui: a.no_ui });
                        }
                        None => {
                            println!("invoke: nothing matches {w}");
                            h.close();
                        }
                    }
                }
            }
            Part::Icons(v) => icons = v.len(),
            Part::Invoked { id, verb, result } => invoked = Some(format!("id {id} verb {verb:?} -> {result:?} after {} ms", ms(at))),
            Part::Failed(e) => failed = Some(e.clone()),
            _ => {}
        }
        true
    });
    if let Some(e) = failed {
        println!("FAILED: {e}");
        return None;
    }
    if !done {
        println!("TIMEOUT waiting for the worker");
        return None;
    }
    if print {
        let mut s = String::new();
        ctxmodel::dump(&items, &mut s);
        println!("{s}");
        let mut k = [0usize; 6];
        count_kinds(&items, &mut k);
        println!("items {} (with icon {}, ownerdraw {}, icon-callback {}, with verb {}, lazy/empty submenus {}), distinct icons {icons}", k[0], k[1], k[2], k[3], k[4], k[5]);
    }
    if let Some(i) = invoked {
        println!("invoked: {i}");
    }
    Some((first.unwrap_or(0), full.unwrap_or(0), timing, items))
}

fn main() {
    let a = args();
    if a.paths.is_empty() {
        eprintln!("usage: ctxmenu_dump [--bg] [--ext] [--top] [--repeat N] [--handlers] [--packaged] [--invoke VERB] [--cycles N [--native]] <path>...");
        std::process::exit(2);
    }
    println!("ctxmenu_dump {:?} bg={} ext={} flags=0x{:x}", a.paths, a.bg, a.ext, tsubame_win::shell::ctxmenu::query_flags(&opts(&a)));
    let start = procinfo::counters();
    let mut pool = Pool::new(2, 0, 0x8001);

    if a.cycles > 0 {
        leak_loop(&mut pool, &a);
        return;
    }
    for i in 0..if a.skip_model { 0 } else { a.repeat.max(1) } {
        let Some((first, full, t, _)) = model_run(&mut pool, &a, i == 0) else { break };
        println!(
            "run {i} ({}): submit->top level {} ms, ->full tree {} ms | worker bind {} ms, QueryContextMenu {} ms, walk {} ms",
            if i == 0 { "cold" } else { "warm" },
            ms(first),
            ms(full),
            ms(t[0]),
            ms(t[1]),
            ms(t[2])
        );
    }
    if a.handlers {
        handlers::run(&a.paths[0], a.bg, a.ext);
    }
    if a.packaged {
        packaged::run(&a.paths[0], a.bg);
    }
    if a.hold > 0 {
        std::thread::sleep(Duration::from_secs(a.hold));
    }
    let end = procinfo::counters();
    println!("counters start: {}\ncounters end:   {}", start.line(), end.line());
}

fn leak_loop(pool: &mut Pool, a: &Args) {
    let marks = [10, 100, a.cycles];
    let mut rows = Vec::new();
    let t = Instant::now();
    for i in 1..=a.cycles {
        if a.native {
            let tk = pool.submit(Request::Menu { paths: a.paths.clone(), x: 200, y: 200, extended: a.ext, auto_close: Some(0), owner: None, opts: MenuOpts { background: a.bg, ..Default::default() } });
            let tt = Instant::now();
            'w: while tt.elapsed() < Duration::from_secs(30) {
                for r in pool.drain() {
                    if r.id == tk.id {
                        break 'w;
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        } else {
            let quiet = Args { invoke: None, ..clone_args(a) };
            if model_run(pool, &quiet, false).is_none() {
                println!("cycle {i} failed");
            }
        }
        if marks.contains(&i) {
            let c = procinfo::counters();
            println!("menu {i}: {}", c.line());
            rows.push((i, c));
        }
    }
    if let (Some(f), Some(l)) = (rows.first(), rows.last()) {
        let d = l.1.private_bytes as f64 - f.1.private_bytes as f64;
        println!(
            "{} path: menu {} -> {}: private {:+.2} MB ({:+.1}%), {:+.1} KB per menu; handles {:+}, gdi {:+}, user {:+}; {:.1} s",
            if a.native { "native" } else { "model" },
            f.0,
            l.0,
            d / 1048576.0,
            d * 100.0 / f.1.private_bytes as f64,
            d / 1024.0 / (l.0 - f.0) as f64,
            l.1.handles as i64 - f.1.handles as i64,
            l.1.gdi as i64 - f.1.gdi as i64,
            l.1.user as i64 - f.1.user as i64,
            t.elapsed().as_secs_f64()
        );
    }
}

fn clone_args(a: &Args) -> Args {
    Args { paths: a.paths.clone(), invoke: a.invoke.clone(), ..*a }
}

impl Clone for Args {
    fn clone(&self) -> Self {
        clone_args(self)
    }
}

/// Small Win32 helpers for the registry walks.
mod reg {
    use windows::Win32::System::Registry::*;
    use windows::core::{PCWSTR, PWSTR};

    pub fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn open(root: HKEY, path: &str) -> Option<HKEY> {
        let w = wide(path);
        let mut hk = HKEY::default();
        unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), Some(0), KEY_READ, &mut hk) }.is_ok().then_some(hk)
    }

    pub fn close(hk: HKEY) {
        unsafe {
            let _ = RegCloseKey(hk);
        }
    }

    pub fn subkeys(root: HKEY, path: &str) -> Vec<String> {
        let Some(hk) = open(root, path) else { return vec![] };
        let mut out = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 260];
            let mut n = buf.len() as u32;
            if unsafe { RegEnumKeyExW(hk, i, Some(PWSTR(buf.as_mut_ptr())), &mut n, None, None, None, None) }.is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..n as usize]));
        }
        close(hk);
        out
    }

    /// A string value (`name` empty = the default value).
    pub fn string(root: HKEY, path: &str, name: &str) -> Option<String> {
        let (p, n) = (wide(path), wide(name));
        let mut buf = vec![0u16; 2048];
        let mut size = (buf.len() * 2) as u32;
        unsafe { RegGetValueW(root, PCWSTR(p.as_ptr()), if name.is_empty() { PCWSTR::null() } else { PCWSTR(n.as_ptr()) }, RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)) }.ok().ok()?;
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// `--handlers`: each `shellex\ContextMenuHandlers` entry that applies, alone.
mod handlers {
    use super::reg;
    use std::time::Instant;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Ole::OleInitialize;
    use windows::Win32::System::Registry::HKEY_CLASSES_ROOT;
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{GUID, Interface, PCWSTR};

    /// Class keys Explorer reads handlers from, in its order.
    pub fn keys(path: &str, bg: bool) -> Vec<String> {
        if bg {
            return vec!["Directory\\Background".into()];
        }
        let p = std::path::Path::new(path);
        if path.len() <= 3 && path.ends_with(":\\") {
            return vec!["Drive".into(), "Folder".into(), "AllFilesystemObjects".into()];
        }
        if p.is_dir() {
            return vec!["Directory".into(), "Folder".into(), "AllFilesystemObjects".into()];
        }
        let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let mut v = vec![];
        if let Some(progid) = reg::string(HKEY_CLASSES_ROOT, &ext, "").filter(|s| !s.is_empty()) {
            v.push(progid);
        }
        v.push(ext.clone());
        v.push(format!("SystemFileAssociations\\{ext}"));
        if let Some(pt) = reg::string(HKEY_CLASSES_ROOT, &ext, "PerceivedType") {
            v.push(format!("SystemFileAssociations\\{pt}"));
        }
        v.push("*".into());
        v.push("AllFilesystemObjects".into());
        v
    }

    pub fn run(path: &str, bg: bool, ext: bool) {
        unsafe {
            let _ = OleInitialize(None);
        }
        println!("\n--- handlers (each alone: CoCreateInstance + IShellExtInit::Initialize + QueryContextMenu), {}", if ext { "extended" } else { "normal" });
        let w = reg::wide(path);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        if unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) }.is_err() {
            println!("parse failed");
            return;
        }
        // folder pidl and the data object of the item (none for a background)
        let (folder_pidl, dobj): (*mut ITEMIDLIST, Option<IDataObject>) = unsafe {
            if bg {
                (pidl, None)
            } else {
                let parent = ILClone(pidl);
                let _ = ILRemoveLastID(Some(parent));
                let folder: IShellFolder = SHBindToParent(pidl, None).expect("SHBindToParent");
                let child = [ILFindLastID(pidl) as *const ITEMIDLIST];
                (parent, folder.GetUIObjectOf::<IDataObject>(HWND::default(), &child, None).ok())
            }
        };
        let mut total = 0f64;
        let mut seen: Vec<GUID> = Vec::new();
        for key in keys(path, bg) {
            let base = format!("{key}\\shellex\\ContextMenuHandlers");
            for name in reg::subkeys(HKEY_CLASSES_ROOT, &base) {
                let val = reg::string(HKEY_CLASSES_ROOT, &format!("{base}\\{name}"), "").unwrap_or_default();
                let cls = if val.starts_with('{') { val.clone() } else { name.clone() };
                let cw = reg::wide(&cls);
                let Ok(clsid) = (unsafe { CLSIDFromString(PCWSTR(cw.as_ptr())) }) else {
                    println!("{key:<28} {name:<28} not a CLSID ({val})");
                    continue;
                };
                if seen.contains(&clsid) {
                    continue;
                }
                seen.push(clsid);
                let dll = reg::string(HKEY_CLASSES_ROOT, &format!("CLSID\\{cls}\\InprocServer32"), "").unwrap_or_else(|| "?".into());
                let dll = dll.rsplit('\\').next().unwrap_or("").to_string();
                let t = Instant::now();
                let r: Result<(u32, Vec<String>, [f64; 3]), String> = (|| unsafe {
                    let init: IShellExtInit = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("create {:?}", e.code()))?;
                    let t_create = t.elapsed().as_secs_f64() * 1000.0;
                    let hk = reg::open(HKEY_CLASSES_ROOT, &key);
                    let r = init.Initialize(Some(folder_pidl), dobj.as_ref(), hk);
                    if let Some(h) = hk {
                        reg::close(h);
                    }

                    r.map_err(|e| format!("init {:?}", e.code()))?;
                    let t_init = t.elapsed().as_secs_f64() * 1000.0 - t_create;
                    let cm: IContextMenu = init.cast().map_err(|e| format!("no IContextMenu {:?}", e.code()))?;
                    let hm = CreatePopupMenu().map_err(|_| "menu")?;
                    let tq = Instant::now();
                    let flags = CMF_NORMAL | CMF_EXPLORE | if bg { 0 } else { CMF_ITEMMENU } | if ext { CMF_EXTENDEDVERBS } else { 0 };
                    let hr = cm.QueryContextMenu(hm, 0, 1, 0x7eff, flags);
                    let t_q = tq.elapsed().as_secs_f64() * 1000.0;
                    let n = GetMenuItemCount(Some(hm)).max(0) as u32;
                    let mut labels = Vec::new();
                    for i in 0..n {
                        let mut buf = [0u16; 128];
                        let l = GetMenuStringW(hm, i, Some(&mut buf), MF_BYPOSITION);
                        if l > 0 {
                            labels.push(String::from_utf16_lossy(&buf[..l as usize]).replace('&', ""));
                        }
                    }
                    let _ = DestroyMenu(hm);
                    if hr.is_err() {
                        return Err(format!("QueryContextMenu {hr:?}"));
                    }
                    Ok((n, labels, [t_create, t_init, t_q]))
                })();
                let all = t.elapsed().as_secs_f64() * 1000.0;
                total += all;
                match r {
                    Ok((n, labels, t3)) => println!("{key:<28} {name:<28} {cls} {dll:<24} total {all:>7.1} ms (create {:.1}, init {:.1}, query {:.1}) items {n} {labels:?}", t3[0], t3[1], t3[2]),
                    Err(e) => println!("{key:<28} {name:<28} {cls} {dll:<24} total {all:>7.1} ms {e}"),
                }
            }
        }
        println!("sum of handlers: {total:.1} ms");
    }
}

/// `--packaged`: Win11 packaged IExplorerCommand handlers ("new" menu items).
mod packaged {
    use super::reg;
    use std::time::Instant;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Ole::OleInitialize;
    use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::*;
    use windows::core::{GUID, PCWSTR};

    /// (package, item type, clsid) from each package's AppxManifest.xml:
    /// packages with packaged COM are under HKCR\PackagedCom\Package, their
    /// folders under the AppModel repository.
    pub fn handlers() -> Vec<(String, String, String)> {
        let repo = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";
        let mut out = Vec::new();
        for full in reg::subkeys(HKEY_CLASSES_ROOT, "PackagedCom\\Package") {
            // the package folder, or (sparse packages such as VS Code) the
            // install folder under WindowsApps that holds the manifest
            let root = reg::string(HKEY_CURRENT_USER, &format!("{repo}\\{full}"), "PackageRootFolder").unwrap_or_default();
            let pf = std::env::var("ProgramW6432").unwrap_or_else(|_| "C:\\Program Files".into());
            let found = [format!("{root}\\AppxManifest.xml"), format!("{pf}\\WindowsApps\\{full}\\AppxManifest.xml")].iter().find_map(|p| std::fs::read_to_string(p).ok().filter(|x| x.contains("windows.fileExplorerContextMenus")));
            let Some(x) = found else { continue };
            let Some(i) = x.find("windows.fileExplorerContextMenus") else { continue };
            let end = x[i..].find("</desktop4:Extension>").or_else(|| x[i..].find("Extension>")).map(|e| i + e).unwrap_or(x.len());
            let block = &x[i..end];
            let mut rest = block;
            while let Some(p) = rest.find("ItemType Type=\"") {
                rest = &rest[p + 15..];
                let ty = rest[..rest.find('"').unwrap_or(0)].to_string();
                let close = rest.find("ItemType>").unwrap_or(rest.len());
                let inner = &rest[..close];
                let mut r2 = inner;
                while let Some(c) = r2.find("Clsid=\"") {
                    r2 = &r2[c + 7..];
                    out.push((full.clone(), ty.clone(), r2[..r2.find('"').unwrap_or(0)].to_string()));
                }
                rest = &rest[close..];
            }
        }
        out
    }

    pub fn run(path: &str, bg: bool) {
        unsafe {
            let _ = OleInitialize(None);
        }
        let t = Instant::now();
        let list = handlers();
        println!("\n--- packaged IExplorerCommand handlers: {} entries found in {:.1} ms", list.len(), t.elapsed().as_secs_f64() * 1000.0);
        let p = std::path::Path::new(path);
        let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
        let types: Vec<String> = if bg {
            vec!["Directory\\Background".into()]
        } else if p.is_dir() {
            vec!["Directory".into()]
        } else {
            vec!["*".into(), ext]
        };
        let w = reg::wide(path);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let arr: Option<IShellItemArray> = unsafe {
            let _ = SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None);
            SHCreateShellItemArrayFromIDLists(&[pidl as *const _]).ok()
        };
        let mut done: Vec<String> = Vec::new();
        for (pkg, ty, cls) in &list {
            if !types.iter().any(|t| t.eq_ignore_ascii_case(ty)) || done.contains(cls) {
                continue;
            }
            done.push(cls.clone());
            let cw = reg::wide(&format!("{{{}}}", cls.trim_matches(|c| c == '{' || c == '}')));
            let Ok(clsid): Result<GUID, _> = (unsafe { CLSIDFromString(PCWSTR(cw.as_ptr())) }) else { continue };
            let t = Instant::now();
            let r: Result<String, String> = (|| unsafe {
                let cmd: IExplorerCommand = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER).map_err(|e| format!("create {:?} {}", e.code(), e.message()))?;
                let tc = t.elapsed().as_secs_f64() * 1000.0;
                let title = cmd.GetTitle(arr.as_ref()).map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                });
                let state = cmd.GetState(arr.as_ref(), true);
                let flags = cmd.GetFlags();
                let icon = cmd.GetIcon(arr.as_ref()).map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                });
                let subs = cmd.EnumSubCommands().is_ok();
                Ok(format!("create {tc:.1} ms, title {:?}, state {:?}, flags {:?}, icon {:?}, subcommands {subs}", title.map_err(|e| format!("{:?}", e.code())), state.map_err(|e| format!("{:?}", e.code())), flags.map_err(|e| format!("{:?}", e.code())), icon.map_err(|e| format!("{:?}", e.code()))))
            })();
            let all = t.elapsed().as_secs_f64() * 1000.0;
            let pkg = pkg.split('_').next().unwrap_or(pkg);
            println!("{pkg:<32} {ty:<22} {{{cls}}} {all:>7.1} ms {}", r.unwrap_or_else(|e| e));
        }
    }
}
