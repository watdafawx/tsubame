//! Win32 glue for the kit: the text clipboard and the IME (composition
//! string, candidate window position).

use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Input::Ime::*;

const CF_UNICODETEXT: u32 = 13;

/// `tsubame_ui::Clipboard` over the Windows clipboard (CF_UNICODETEXT).
pub struct WinClipboard(pub isize);

impl WinClipboard {
    fn hwnd(&self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

impl tsubame_ui::Clipboard for WinClipboard {
    fn get(&mut self) -> Option<String> {
        unsafe {
            OpenClipboard(Some(self.hwnd())).ok()?;
            let out = (|| {
                let h = GetClipboardData(CF_UNICODETEXT).ok()?;
                let g = HGLOBAL(h.0);
                let p = GlobalLock(g) as *const u16;
                if p.is_null() {
                    return None;
                }
                let n = GlobalSize(g) / 2;
                let s = std::slice::from_raw_parts(p, n);
                let end = s.iter().position(|&c| c == 0).unwrap_or(n);
                let text = String::from_utf16_lossy(&s[..end]);
                let _ = GlobalUnlock(g);
                Some(text)
            })();
            let _ = CloseClipboard();
            out
        }
    }

    fn set(&mut self, s: &str) {
        let w: Vec<u16> = s.encode_utf16().chain([0]).collect();
        unsafe {
            if OpenClipboard(Some(self.hwnd())).is_err() {
                return;
            }
            let _ = EmptyClipboard();
            if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
                let p = GlobalLock(g) as *mut u16;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                    let _ = GlobalUnlock(g);
                    // the clipboard owns the memory once this succeeds
                    if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(g.0))).is_err() {
                        let _ = GlobalFree(Some(g));
                    }
                }
            }
            let _ = CloseClipboard();
        }
    }
}

/// The composition (`comp`) or result string of a WM_IME_COMPOSITION, if
/// this message carries it.
pub fn ime_string(hwnd: HWND, lp: LPARAM, comp: bool) -> Option<String> {
    let flag = if comp { GCS_COMPSTR } else { GCS_RESULTSTR };
    if lp.0 as u32 & flag.0 == 0 {
        return None;
    }
    unsafe {
        let himc: HIMC = ImmGetContext(hwnd);
        if himc.is_invalid() {
            return None;
        }
        let bytes = ImmGetCompositionStringW(himc, flag, None, 0);
        let mut s = None;
        if bytes >= 0 {
            let mut buf = vec![0u16; bytes as usize / 2];
            if bytes > 0 {
                ImmGetCompositionStringW(himc, flag, Some(buf.as_mut_ptr().cast()), bytes as u32);
            }
            s = Some(String::from_utf16_lossy(&buf));
        }
        let _ = ImmReleaseContext(hwnd, himc);
        s
    }
}

/// Puts the IME composition and candidate windows at the caret (client px).
pub fn place_ime(hwnd: HWND, x: i32, y: i32, h: i32) {
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_invalid() {
            return;
        }
        let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x, y }, rcArea: RECT::default() };
        let _ = ImmSetCompositionWindow(himc, &cf);
        let cand = CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_EXCLUDE, ptCurrentPos: POINT { x, y: y + h }, rcArea: RECT { left: x, top: y, right: x + 1, bottom: y + h } };
        let _ = ImmSetCandidateWindow(himc, &cand);
        let _ = ImmReleaseContext(hwnd, himc);
    }
}
