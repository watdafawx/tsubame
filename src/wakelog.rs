//! Idle wake-up attribution for the UI thread (diagnostics only).
//!
//! `TSUBAME_WAKE_LOG=<file>`: every message the main window procedure gets
//! (posted ones through `GetMessageW`, sent ones from other threads and
//! processes) is recorded with its time since process start; the file is
//! written when the message loop ends (close the window normally). Unset, the
//! cost is one relaxed load per message. Per-thread wake-ups of the whole
//! process (driver and OS threads too) come from `tools/idle-wakes.ps1`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ON: AtomicBool = AtomicBool::new(false);
/// (ms since start, message, wParam, sent from another thread)
static LOG: Mutex<Vec<(u32, u32, usize, bool)>> = Mutex::new(Vec::new());
/// Records kept; later messages are only counted.
const CAP: usize = 1 << 16;
static COUNTS: Mutex<Vec<(u32, u64)>> = Mutex::new(Vec::new());
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn init() {
    if std::env::var_os("TSUBAME_WAKE_LOG").is_some() {
        START.get_or_init(Instant::now);
        ON.store(true, Ordering::Relaxed);
    }
}

#[inline]
pub fn note(msg: u32, wp: usize) {
    if ON.load(Ordering::Relaxed) {
        record(msg, wp);
    }
}

#[cold]
fn record(msg: u32, wp: usize) {
    let sent = unsafe { windows::Win32::UI::WindowsAndMessaging::InSendMessage() }.as_bool();
    let ms = START.get().map_or(0, |s| s.elapsed().as_millis() as u32);
    if let Ok(mut l) = LOG.lock()
        && l.len() < CAP
    {
        l.push((ms, msg, wp, sent));
    }
    if let Ok(mut c) = COUNTS.lock() {
        match c.iter_mut().find(|e| e.0 == msg) {
            Some(e) => e.1 += 1,
            None => c.push((msg, 1)),
        }
    }
}

/// Writes the log (call after the message loop).
pub fn write() {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let Some(path) = std::env::var_os("TSUBAME_WAKE_LOG") else { return };
    let mut out = String::new();
    if let Ok(mut c) = COUNTS.lock() {
        c.sort_by_key(|e| std::cmp::Reverse(e.1));
        for (m, n) in c.iter() {
            out.push_str(&format!("wakes.msg.0x{m:04x}={n}\n"));
        }
    }
    if let Ok(l) = LOG.lock() {
        for (ms, m, wp, sent) in l.iter() {
            out.push_str(&format!("{ms} 0x{m:04x} wp={wp:#x}{}\n", if *sent { " sent" } else { "" }));
        }
    }
    if let Ok(l) = PAINTS.lock() {
        for (ms, gpu, total, create) in l.iter() {
            out.push_str(&format!("{ms} paint {} total_ms={total:.1} create_ms={create:.1}\n", if *gpu { "gpu" } else { "soft" }));
        }
    }
    let _ = std::fs::write(path, out);
}

/// A completed paint: its path (GPU or not), total and target-creation ms.
pub fn paint(gpu: bool, total_ms: f32, create_ms: f32) {
    if ON.load(Ordering::Relaxed) {
        let ms = START.get().map_or(0, |s| s.elapsed().as_millis() as u32);
        if let Ok(mut l) = PAINTS.lock()
            && l.len() < CAP
        {
            l.push((ms, gpu, total_ms, create_ms));
        }
    }
}
static PAINTS: Mutex<Vec<(u32, bool, f32, f32)>> = Mutex::new(Vec::new());

#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}
