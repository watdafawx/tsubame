//! Thumbnails via `IShellItemImageFactory::GetImage`. Worker (STA) threads only.
//!
//! Every request carries `SIIGBF_THUMBNAILONLY`. Without it the shell answers
//! a cache miss (`SIIGBF_INCACHEONLY`) or a failed extraction with the generic
//! type icon *as a success*, so a probe on a file that is not in the system
//! thumbnail cache "hit" with an icon bitmap and the generating call never ran
//! (rig: 16 fresh PNGs, 16 icons; with the flag, 16 real thumbnails).
//!
//! Steps: probe the cache (instant paint when warm), then generate on the same
//! worker. A file with no thumbnail handler (or a real failure) is an error
//! reply: the UI keeps the type icon it already draws, nothing spins.

use super::Payload;
use crate::bitmap;
use crate::wide;
use std::time::Instant;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::UI::Shell::*;
use windows::core::PCWSTR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Cache only: no extraction, no disk read of the image.
    Probe,
    /// Let the shell extract (and cache) the thumbnail. `retried`: the one
    /// immediate retry of a transient failure was used.
    Generate { retried: bool },
}

/// `E_PENDING`, sharing and lock violations: the extraction may succeed a
/// moment later (file still being written, handler busy).
fn transient(hr: i32) -> bool {
    matches!(hr as u32, 0x8000_000A | 0x8007_0020 | 0x8007_0021)
}

pub(crate) fn flags(step: Step) -> SIIGBF {
    let base = SIIGBF_THUMBNAILONLY.0 | SIIGBF_BIGGERSIZEOK.0;
    match step {
        Step::Probe => SIIGBF(base | SIIGBF_INCACHEONLY.0),
        Step::Generate { .. } => SIIGBF(base),
    }
}

/// What to do after `step`: `Some(next)` or `None` (finished: reply with
/// whatever the last step gave). `ok`: the step produced a bitmap; `hr`: its
/// error code otherwise.
pub(crate) fn next(step: Step, ok: bool, hr: i32, cache_only: bool) -> Option<Step> {
    match step {
        _ if ok => None,
        Step::Probe if cache_only => None,
        Step::Probe => Some(Step::Generate { retried: false }),
        Step::Generate { retried: false } if transient(hr) => Some(Step::Generate { retried: true }),
        Step::Generate { .. } => None,
    }
}

pub fn get(path: &str, size: u32, cache_only: bool) -> Payload {
    let w = wide(path);
    let sz = SIZE { cx: size as i32, cy: size as i32 };
    unsafe {
        let f: IShellItemImageFactory = match SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) {
            Ok(f) => f,
            Err(e) => return Payload::Thumb { bmp: None, from_cache: false, probe_us: 0, error: Some(format!("SHCreateItemFromParsingName {:?}", e.code())) },
        };
        let mut step = Step::Probe;
        let mut probe_us = 0;
        loop {
            let t = Instant::now();
            let r = f.GetImage(sz, flags(step));
            if step == Step::Probe {
                probe_us = t.elapsed().as_micros() as u64;
            }
            let (ok, hr) = match &r {
                Ok(_) => (true, 0),
                Err(e) => (false, e.code().0),
            };
            match next(step, ok, hr, cache_only) {
                Some(s) => step = s, // only after a failure: nothing to free
                None => {
                    let from_cache = step == Step::Probe && ok;
                    return match r {
                        Ok(h) => {
                            // BIGGERSIZEOK can return the cached 256 / 1024 px bitmap: cut it
                            // to the size asked so the UI cache holds what the cell draws
                            let bmp = bitmap::from_shell_hbitmap(h).map(|b| b.fit_within(size));
                            let _ = DeleteObject(h.into());
                            Payload::Thumb { bmp, from_cache, probe_us, error: None }
                        }
                        Err(e) => Payload::Thumb { bmp: None, from_cache, probe_us, error: Some(format!("GetImage {:?}", e.code())) },
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_asks_for_a_real_thumbnail_only() {
        // regression: without THUMBNAILONLY a cache miss "succeeds" with the type icon
        for s in [Step::Probe, Step::Generate { retried: false }, Step::Generate { retried: true }] {
            let f = flags(s).0;
            assert_ne!(f & SIIGBF_THUMBNAILONLY.0, 0, "{s:?}");
            assert_ne!(f & SIIGBF_BIGGERSIZEOK.0, 0, "{s:?}");
            assert_eq!(f & SIIGBF_ICONONLY.0, 0, "{s:?}");
        }
        assert_ne!(flags(Step::Probe).0 & SIIGBF_INCACHEONLY.0, 0);
        assert_eq!(flags(Step::Generate { retried: false }).0 & SIIGBF_INCACHEONLY.0, 0, "generating must not be cache-only");
    }

    #[test]
    fn probe_hit_ends() {
        assert_eq!(next(Step::Probe, true, 0, false), None);
    }

    #[test]
    fn probe_miss_generates_unless_cache_only() {
        let miss = 0x8004_B200u32 as i32;
        assert_eq!(next(Step::Probe, false, miss, false), Some(Step::Generate { retried: false }));
        assert_eq!(next(Step::Probe, false, miss, true), None);
    }

    #[test]
    fn generate_failure_is_final_unless_transient_once() {
        let hard = 0x8004_B200u32 as i32;
        assert_eq!(next(Step::Generate { retried: false }, true, 0, false), None);
        assert_eq!(next(Step::Generate { retried: false }, false, hard, false), None);
        let pending = 0x8000_000Au32 as i32;
        assert_eq!(next(Step::Generate { retried: false }, false, pending, false), Some(Step::Generate { retried: true }));
        assert_eq!(next(Step::Generate { retried: true }, false, pending, false), None, "one retry, never a loop");
    }

    #[test]
    fn state_machine_terminates() {
        // worst case: probe miss, transient, transient: 3 calls
        let pending = 0x8000_000Au32 as i32;
        let mut s = Step::Probe;
        let mut calls = 1;
        while let Some(n) = next(s, false, pending, false) {
            s = n;
            calls += 1;
        }
        assert_eq!(calls, 3);
    }
}
