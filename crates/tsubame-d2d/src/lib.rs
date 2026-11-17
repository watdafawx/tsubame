//! `Canvas` backend (`impl tsubame_ui::Canvas for Gfx`): Direct2D/DirectWrite drawing, lifted from blink's gfx.rs.

mod gfx;
pub use gfx::*;

/// Diagnostics go to the debugger (`OutputDebugString`); there is no console.
pub fn log(msg: &str) {
    use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
    let w: Vec<u16> = format!("tsubame-d2d: {msg}\0").encode_utf16().collect();
    unsafe { OutputDebugStringW(windows::core::PCWSTR(w.as_ptr())) };
}
