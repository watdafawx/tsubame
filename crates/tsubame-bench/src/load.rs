//! Machine-load probe, so a noisy run can be explained instead of argued about.

use std::time::Duration;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::GetSystemTimes;

fn ft_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

fn sample() -> Option<(u64, u64, u64)> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()? };
    Some((ft_to_u64(idle), ft_to_u64(kernel), ft_to_u64(user)))
}

/// Fraction of CPU time busy over `window_ms`, 0.0..=1.0.
pub fn cpu_busy(window_ms: u64) -> Option<f64> {
    let a = sample()?;
    std::thread::sleep(Duration::from_millis(window_ms));
    let b = sample()?;
    let idle = b.0.wrapping_sub(a.0);
    // Kernel time already includes idle time.
    let total = b.1.wrapping_sub(a.1).wrapping_add(b.2.wrapping_sub(a.2));
    if total == 0 {
        return None;
    }
    Some((total.saturating_sub(idle)) as f64 / total as f64)
}
