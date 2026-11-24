//! Opt-in lag tracer (`TSUBAME_TRACE_LAG=<file>`).
//!
//! Free when off: [`on`] is one relaxed atomic load, so call sites gated by
//! `if lagtrace::on()` allocate nothing, lock nothing and take no timestamps
//! in normal runs. Zero timers and zero background threads (no idle wake-ups).
//!
//! When enabled, writes timestamped lines (`+<ms>ms [tid=<id>] <event> ...`)
//! for window-close teardown steps and context-menu open phases (queue wait,
//! classic bind/query/walk, packaged scan and per-CLSID handler load, UI
//! reveal and paint).

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();
static SINK: Mutex<Option<BufWriter<File>>> = Mutex::new(None);

/// Enables tracing when `TSUBAME_TRACE_LAG` is set to a non-empty file path.
/// Safe to call once early in `main`.
pub fn init() {
    let Some(path) = std::env::var_os("TSUBAME_TRACE_LAG") else { return };
    if path.is_empty() {
        return;
    }
    let _ = init_file(Path::new(&path));
}

/// Opens `path` (creating parent dirs if needed) and enables tracing.
pub fn init_file(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = OpenOptions::new().create(true).write(true).truncate(true).open(path)?;
    START.get_or_init(Instant::now);
    let mut g = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = BufWriter::new(file);
    let _ = writeln!(w, "# tsubame lag trace (pid={})", std::process::id());
    let _ = w.flush();
    *g = Some(w);
    ENABLED.store(true, Ordering::Release);
    Ok(())
}

/// Fast-path check: one relaxed atomic load, zero cost when off.
#[inline(always)]
pub fn on() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Milliseconds elapsed since `init` (0.0 when tracing was never initialized).
#[inline]
pub fn now_ms() -> f64 {
    START.get().map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0)
}

#[inline]
fn current_tid() -> u32 {
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}

/// Writes one trace line. Callers should guard with `if lagtrace::on()` so
/// formatting arguments are never evaluated when disabled.
#[cold]
pub fn record(event: &str, dur_ms: Option<f64>, detail: fmt::Arguments<'_>, flush_now: bool) {
    if !on() {
        return;
    }
    let t_ms = now_ms();
    let tid = current_tid();
    let mut g = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(w) = g.as_mut() else { return };
    let _ = write!(w, "+{t_ms:9.3}ms [tid={tid}] {event}");
    if let Some(d) = dur_ms {
        let _ = write!(w, " dur_ms={d:.3}");
    }
    let _ = writeln!(w, " {detail}");
    if flush_now {
        let _ = w.flush();
    }
}

/// Flushes buffered trace lines to disk (no-op when disabled).
pub fn flush() {
    if !on() {
        return;
    }
    let mut g = SINK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = g.as_mut() {
        let _ = w.flush();
    }
}

/// Disables tracing and closes the sink (tests only).
#[cfg(test)]
fn reset_for_test() {
    ENABLED.store(false, Ordering::Release);
    let mut g = SINK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut w) = g.take() {
        let _ = w.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_writes_spans_when_enabled_and_is_noop_when_off() {
        static TEST_LOCK: Mutex<()> = Mutex::new(());
        let _guard = TEST_LOCK.lock().unwrap();
        reset_for_test();
        assert!(!on());
        // No-op when off
        record("ignored.event", Some(1.5), format_args!("k=v"), true);

        let p = std::env::temp_dir().join(format!("tsubame-lagtrace-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&p);
        init_file(&p).expect("init_file");
        assert!(on());
        record("ctx.worker.pkg_clsid", Some(12.345), format_args!("gen=1 clsid={{abc}} outcome=ok"), false);
        record("close.exit", Some(4.2), format_args!("reason=middle_tab"), true);
        reset_for_test();
        assert!(!on());

        let text = std::fs::read_to_string(&p).expect("read trace");
        let _ = std::fs::remove_file(&p);
        assert!(text.contains("# tsubame lag trace"), "{text}");
        assert!(text.contains("ctx.worker.pkg_clsid dur_ms=12.345 gen=1 clsid={abc} outcome=ok"), "{text}");
        assert!(text.contains("close.exit dur_ms=4.200 reason=middle_tab"), "{text}");
    }
}
