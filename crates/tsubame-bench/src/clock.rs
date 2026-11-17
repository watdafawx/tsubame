//! QueryPerformanceCounter clock. Every number in the bench comes from here.

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub struct Clock {
    freq: f64,
}

impl Clock {
    pub fn new() -> Self {
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        Self {
            freq: if f > 0 { f as f64 } else { 1.0 },
        }
    }

    /// Raw ticks. Ticks are only comparable within one process run.
    pub fn now(&self) -> i64 {
        let mut c = 0i64;
        unsafe {
            let _ = QueryPerformanceCounter(&mut c);
        }
        c
    }

    pub fn ms(&self, ticks: i64) -> f64 {
        ticks as f64 * 1000.0 / self.freq
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}
