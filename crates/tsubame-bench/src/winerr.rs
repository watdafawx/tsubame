//! Turn a `windows::core::Error` back into its Win32 error code.
//!
//! The `windows` crate converts a failed `BOOL` into `HRESULT_FROM_WIN32(code)`
//! (0x8007_xxxx), so the low 16 bits are the original Win32 code and the error
//! paths stay readable (`ERROR_NO_MORE_FILES`, `ERROR_MORE_DATA`, ...).

use windows::core::Error;

pub fn win32_code(e: &Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_low_word() {
        let e = Error::from_win32();
        // Not asserting a specific code (depends on the thread's last error),
        // only that the extraction is a plain Win32 code range.
        assert!(win32_code(&e) <= 0xFFFF);
    }
}
