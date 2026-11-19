//! UTC to local wall-clock time, for showing file dates.
//!
//! Stored times (FILETIME, unix seconds) stay UTC: sorting and comparing use
//! them as they are. Only display goes through [`local_secs`].
//!
//! The offset is the one that applied *on that date*, not today's: the
//! time-zone rules of the date's year are used (`GetTimeZoneInformationForYear`
//! with the system's dynamic zone), so a July file shows summer time in
//! winter and 2005 files use the 2005 DST rules. Rules are cached per year for
//! a short while (no registry read per row); a zone change is picked up
//! within [`REFRESH`]. No timers: the cache is checked when asked.

use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Time::*;

const EPOCH_DIFF: u64 = 11_644_473_600;
const REFRESH: Duration = Duration::from_secs(30);

struct Cache {
    stamp: Instant,
    dtzi: DYNAMIC_TIME_ZONE_INFORMATION,
    years: Vec<(u16, TIME_ZONE_INFORMATION)>,
}

thread_local! {
    static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) };
}

fn system_time(unix: u32) -> Option<SYSTEMTIME> {
    let t = (unix as u64 + EPOCH_DIFF) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
    let mut st = SYSTEMTIME::default();
    unsafe { FileTimeToSystemTime(&ft, &mut st).ok()? };
    Some(st)
}

fn unix_of(st: &SYSTEMTIME) -> Option<u32> {
    let mut ft = FILETIME::default();
    unsafe { SystemTimeToFileTime(st, &mut ft).ok()? };
    let t = (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64;
    Some((t / 10_000_000).saturating_sub(EPOCH_DIFF).min(u32::MAX as u64) as u32)
}

/// `unix` (UTC seconds) as local wall-clock seconds under the zone rules
/// `tzi` (read the result with UTC calendar maths). `None` if the API fails.
pub fn convert(unix: u32, tzi: &TIME_ZONE_INFORMATION) -> Option<u32> {
    let utc = system_time(unix)?;
    let mut local = SYSTEMTIME::default();
    unsafe { SystemTimeToTzSpecificLocalTime(Some(tzi), &utc, &mut local).ok()? };
    unix_of(&local)
}

fn rules_for_year(year: u16) -> Option<TIME_ZONE_INFORMATION> {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().is_none_or(|c| c.stamp.elapsed() > REFRESH) {
            let mut dtzi = DYNAMIC_TIME_ZONE_INFORMATION::default();
            unsafe { GetDynamicTimeZoneInformation(&mut dtzi) };
            *c = Some(Cache { stamp: Instant::now(), dtzi, years: Vec::new() });
        }
        let c = c.as_mut()?;
        if let Some((_, t)) = c.years.iter().find(|(y, _)| *y == year) {
            return Some(*t);
        }
        let mut tzi = TIME_ZONE_INFORMATION::default();
        unsafe { GetTimeZoneInformationForYear(year, Some(&c.dtzi), &mut tzi).ok()? };
        if c.years.len() >= 16 {
            c.years.clear();
        }
        c.years.push((year, tzi));
        Some(tzi)
    })
}

/// Local wall-clock seconds of `unix` (UTC), DST of that date applied. Falls
/// back to `unix` itself (UTC) if the system refuses.
pub fn local_secs(unix: u32) -> u32 {
    let Some(year) = system_time(unix).map(|s| s.wYear) else { return unix };
    rules_for_year(year).and_then(|tzi| convert(unix, &tzi)).unwrap_or(unix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(month: u16, dow: u16, week: u16, hour: u16) -> SYSTEMTIME {
        SYSTEMTIME { wMonth: month, wDayOfWeek: dow, wDay: week, wHour: hour, ..Default::default() }
    }

    /// US Eastern: UTC-5, DST (UTC-4) from the 2nd Sunday of March 02:00 to the 1st Sunday of November 02:00.
    fn eastern() -> TIME_ZONE_INFORMATION {
        TIME_ZONE_INFORMATION { Bias: 300, StandardBias: 0, DaylightBias: -60, StandardDate: st(11, 0, 1, 2), DaylightDate: st(3, 0, 2, 2), ..Default::default() }
    }

    /// Central Europe: UTC+1, DST (UTC+2) from the last Sunday of March 02:00 to the last Sunday of October 03:00 local.
    fn cet() -> TIME_ZONE_INFORMATION {
        TIME_ZONE_INFORMATION { Bias: -60, StandardBias: 0, DaylightBias: -60, StandardDate: st(10, 0, 5, 3), DaylightDate: st(3, 0, 5, 2), ..Default::default() }
    }

    // unix seconds of UTC 2025-01-15 12:00, 2025-07-15 12:00
    const JAN: u32 = 1_736_942_400;
    const JUL: u32 = 1_752_580_800;

    #[test]
    fn dst_follows_the_date_not_today() {
        let e = eastern();
        assert_eq!(convert(JAN, &e), Some(JAN - 5 * 3600), "EST in winter");
        assert_eq!(convert(JUL, &e), Some(JUL - 4 * 3600), "EDT in summer");
        let c = cet();
        assert_eq!(convert(JAN, &c), Some(JAN + 3600), "CET");
        assert_eq!(convert(JUL, &c), Some(JUL + 2 * 3600), "CEST");
    }

    #[test]
    fn day_rolls_over_with_the_offset() {
        // 2025-01-15 03:00 UTC is still the 14th in New York
        let t = JAN - 9 * 3600;
        assert_eq!(convert(t, &eastern()), Some(t - 5 * 3600));
    }

    #[test]
    fn utc_zone_is_identity() {
        let utc = TIME_ZONE_INFORMATION::default();
        assert_eq!(convert(JAN, &utc), Some(JAN));
        assert_eq!(convert(0, &utc), Some(0));
    }

    #[test]
    fn local_secs_stays_close_to_utc() {
        // whatever zone the machine is in, the offset is within +-14 h
        let l = local_secs(JUL) as i64 - JUL as i64;
        assert!(l.abs() <= 14 * 3600, "{l}");
    }
}
