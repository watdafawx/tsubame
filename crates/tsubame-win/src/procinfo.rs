//! Process counters for the leak gate: private bytes, kernel handles, GDI and
//! USER objects, threads.

use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources, GetProcessHandleCount};

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub private_bytes: usize,
    pub working_set: usize,
    pub handles: u32,
    pub gdi: u32,
    pub user: u32,
}

pub fn counters() -> Counters {
    unsafe {
        let p = GetCurrentProcess();
        let mut m = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        let _ = GetProcessMemoryInfo(p, &mut m as *mut _ as *mut PROCESS_MEMORY_COUNTERS, m.cb);
        let mut handles = 0u32;
        let _ = GetProcessHandleCount(p, &mut handles);
        Counters { private_bytes: m.PrivateUsage, working_set: m.WorkingSetSize, handles, gdi: GetGuiResources(p, GR_GDIOBJECTS), user: GetGuiResources(p, GR_USEROBJECTS) }
    }
}

impl Counters {
    pub fn line(&self) -> String {
        format!(
            "private {:.1} MB, working set {:.1} MB, handles {}, gdi {}, user {}",
            self.private_bytes as f64 / 1048576.0,
            self.working_set as f64 / 1048576.0,
            self.handles,
            self.gdi,
            self.user
        )
    }
}
