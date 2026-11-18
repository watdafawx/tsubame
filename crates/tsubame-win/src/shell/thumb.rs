//! Thumbnails via `IShellItemImageFactory::GetImage`. Worker (STA) threads only.

use super::Payload;
use crate::bitmap;
use crate::wide;
use std::time::Instant;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::UI::Shell::*;
use windows::core::PCWSTR;

/// Probe the thumbnail cache first (`SIIGBF_INCACHEONLY`); on a miss, and
/// unless `cache_only`, generate (`SIIGBF_BIGGERSIZEOK`: a cached bigger size
/// is fine, the canvas scales). Files without a thumbnail handler get their
/// big icon (no `SIIGBF_THUMBNAILONLY`).
pub fn get(path: &str, size: u32, cache_only: bool) -> Payload {
    let w = wide(path);
    let sz = SIZE { cx: size as i32, cy: size as i32 };
    unsafe {
        let f: IShellItemImageFactory = match SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) {
            Ok(f) => f,
            Err(e) => return Payload::Thumb { bmp: None, from_cache: false, probe_us: 0, error: Some(format!("SHCreateItemFromParsingName {:?}", e.code())) },
        };
        let t = Instant::now();
        let probe = f.GetImage(sz, SIIGBF_BIGGERSIZEOK | SIIGBF_INCACHEONLY);
        let probe_us = t.elapsed().as_micros() as u64;
        let (hbm, from_cache) = match probe {
            Ok(h) => (Ok(h), true),
            Err(e) if cache_only => (Err(e), false),
            Err(_) => (f.GetImage(sz, SIIGBF_BIGGERSIZEOK), false),
        };
        match hbm {
            Ok(h) => {
                // BIGGERSIZEOK can return the cached 256 / 1024 px bitmap: cut it
                // to the size asked so the UI cache holds what the cell draws
                let bmp = bitmap::from_shell_hbitmap(h).map(|b| b.fit_within(size));
                let _ = DeleteObject(h.into());
                Payload::Thumb { bmp, from_cache, probe_us, error: None }
            }
            Err(e) => Payload::Thumb { bmp: None, from_cache, probe_us, error: Some(format!("GetImage {:?}", e.code())) },
        }
    }
}
