//! Explorer group keys, independent of row painting and selection.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupBy {
    #[default]
    None,
    Name,
    Type,
    Date,
    Size,
}

impl GroupBy {
    pub const ALL: [GroupBy; 5] = [GroupBy::None, GroupBy::Name, GroupBy::Type, GroupBy::Date, GroupBy::Size];

    /// Config / session spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Name => "name",
            Self::Type => "type",
            Self::Date => "date",
            Self::Size => "size",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.name().eq_ignore_ascii_case(s.trim()))
    }

    /// Menu / palette label.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "(None)",
            Self::Name => "Name",
            Self::Type => "Type",
            Self::Date => "Date modified",
            Self::Size => "Size",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::None => Self::Name,
            Self::Name => Self::Type,
            Self::Type => Self::Date,
            Self::Date => Self::Size,
            Self::Size => Self::None,
        }
    }
}

/// ASCII letters have their own group; digits, punctuation and non-Latin
/// names use '#'. Input is the store's UTF-16 name, without allocation.
pub fn name_key(name: &[u16]) -> char {
    match name.first().copied() {
        Some(n @ 65..=90) => char::from_u32(u32::from(n)).unwrap(),
        Some(n @ 97..=122) => char::from_u32(u32::from(n - 32)).unwrap(),
        _ => '#',
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SizeKey {
    Empty,
    Tiny,
    Small,
    Medium,
    Large,
    Huge,
}

pub fn size_key(bytes: u64) -> SizeKey {
    match bytes {
        0 => SizeKey::Empty,
        1..=16383 => SizeKey::Tiny,
        16384..=1048575 => SizeKey::Small,
        1048576..=134217727 => SizeKey::Medium,
        134217728..=1073741823 => SizeKey::Large,
        _ => SizeKey::Huge,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DateKey {
    Today,
    Yesterday,
    EarlierThisWeek,
    LastWeek,
    EarlierThisMonth,
    LastMonth,
    EarlierThisYear,
    ALongTimeAgo,
}

/// UTC instants of local calendar boundaries, computed once per rebuild by
/// the platform. Do not subtract 86400 from UTC midnight: DST changes day
/// length. Monday is the first day of the week. Boundaries may overlap;
/// the first matching bucket wins (e.g. Yesterday on a Monday).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateBoundaries {
    pub today: i64,
    pub yesterday: i64,
    pub week: i64,
    pub last_week: i64,
    pub month: i64,
    pub last_month: i64,
    pub year: i64,
}

impl DateBoundaries {
    pub fn key(self, modified: i64) -> DateKey {
        for (start, key) in [
            (self.today, DateKey::Today),
            (self.yesterday, DateKey::Yesterday),
            (self.week, DateKey::EarlierThisWeek),
            (self.last_week, DateKey::LastWeek),
            (self.month, DateKey::EarlierThisMonth),
            (self.last_month, DateKey::LastMonth),
            (self.year, DateKey::EarlierThisYear),
        ] {
            if modified >= start {
                return key;
            }
        }
        DateKey::ALongTimeAgo
    }
}

/// Extension units kept in a [`TypeKey`]; a longer extension is cut here.
pub const TYPE_EXT_UNITS: usize = 12;

/// Type group: folders first, then files by extension, then files without one.
/// ASCII is folded to lower case; the key is `Copy` so a rebuild allocates
/// nothing per item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypeKey {
    Folder,
    /// extension without the dot, lower-case ASCII, zero padded
    Ext([u16; TYPE_EXT_UNITS]),
    NoExt,
}

impl TypeKey {
    /// "Folder", "TXT File" (as the Type column writes unknown types), "File".
    pub fn label(&self) -> String {
        match self {
            Self::Folder => "Folder".to_string(),
            Self::NoExt => "File".to_string(),
            Self::Ext(e) => {
                let n = e.iter().position(|&u| u == 0).unwrap_or(e.len());
                let mut s: String = char::decode_utf16(e[..n].iter().copied()).map(|c| c.unwrap_or('\u{fffd}').to_ascii_uppercase()).collect();
                s.push_str(" File");
                s
            }
        }
    }
}

/// The type key of an entry name (UTF-16, as the store keeps it). A leading
/// dot is part of the name, not an extension; a trailing dot has none.
pub fn type_key(name: &[u16], is_folder: bool) -> TypeKey {
    if is_folder {
        return TypeKey::Folder;
    }
    let Some(dot) = name.iter().rposition(|&u| u == 46) else { return TypeKey::NoExt };
    let ext = &name[dot + 1..];
    if dot == 0 || ext.is_empty() {
        return TypeKey::NoExt;
    }
    let mut key = [0u16; TYPE_EXT_UNITS];
    for (k, &u) in key.iter_mut().zip(ext) {
        *k = if (65..=90).contains(&u) { u + 32 } else { u };
    }
    TypeKey::Ext(key)
}

/// One group's key, whatever the mode. Variants of one mode only compare with
/// each other (a tab holds one mode at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GroupKey {
    Name(char),
    Type(TypeKey),
    Date(DateKey),
    Size(SizeKey),
}

impl GroupKey {
    pub fn label(&self) -> String {
        match self {
            Self::Name(c) => c.to_string(),
            Self::Type(t) => t.label(),
            Self::Date(d) => d.label().to_string(),
            Self::Size(s) => s.label().to_string(),
        }
    }
}

impl SizeKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Empty => "Empty",
            Self::Tiny => "Tiny (under 16 KB)",
            Self::Small => "Small (under 1 MB)",
            Self::Medium => "Medium (under 128 MB)",
            Self::Large => "Large (under 1 GB)",
            Self::Huge => "Huge (1 GB and over)",
        }
    }
}

impl DateKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::EarlierThisWeek => "Earlier this week",
            Self::LastWeek => "Last week",
            Self::EarlierThisMonth => "Earlier this month",
            Self::LastMonth => "Last month",
            Self::EarlierThisYear => "Earlier this year",
            Self::ALongTimeAgo => "A long time ago",
        }
    }
}

/// The key of an entry for `by` (`None` for [`GroupBy::None`]). `name` is the
/// store's UTF-16 name, `modified` unix seconds (UTC). Folders count as empty
/// for the size groups.
pub fn group_key(by: GroupBy, name: &[u16], is_folder: bool, size: u64, modified: i64, dates: &DateBoundaries) -> Option<GroupKey> {
    Some(match by {
        GroupBy::None => return None,
        GroupBy::Name => GroupKey::Name(name_key(name)),
        GroupBy::Type => GroupKey::Type(type_key(name, is_folder)),
        GroupBy::Date => GroupKey::Date(dates.key(modified)),
        GroupBy::Size => GroupKey::Size(size_key(if is_folder { 0 } else { size })),
    })
}

const DAY: i64 = 86_400;

/// (year, month 1-12, day 1-31) of a day count since 1970-01-01 (Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Day count since 1970-01-01 of a civil date (Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// UTC instant of local wall-clock second `local` (seconds since the epoch as
/// a calendar reads them). `offset_at(utc)` is the local offset in seconds at
/// that UTC instant, DST included. A repeated wall time (clocks back) takes its
/// first occurrence; a skipped one (clocks forward over midnight) takes the
/// instant of the jump, which is when that local day really begins.
pub fn local_to_utc(local: i64, offset_at: &impl Fn(i64) -> i64) -> i64 {
    // the offsets in force a day before and a day after that wall time
    let a = local - offset_at(local - DAY);
    let b = local - offset_at(local + DAY);
    let ok = |utc: i64| utc + offset_at(utc) == local;
    match (ok(a), ok(b)) {
        (true, true) => a.min(b),
        (true, false) => a,
        (false, true) => b,
        (false, false) => a.max(b),
    }
}

impl DateBoundaries {
    /// Boundaries for the instant `now` (UTC unix seconds) in the zone `offset_at`
    /// describes. Pure: the platform supplies the zone, tests supply their own.
    pub fn from_local(now: i64, offset_at: impl Fn(i64) -> i64) -> DateBoundaries {
        let local_now = now + offset_at(now);
        let day = local_now.div_euclid(DAY);
        let midnight = |d: i64| local_to_utc(d * DAY, &offset_at);
        // 1970-01-01 was a Thursday: Monday = 0
        let dow = (day + 3).rem_euclid(7);
        let (y, m, _) = civil_from_days(day);
        let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        DateBoundaries {
            today: midnight(day),
            yesterday: midnight(day - 1),
            week: midnight(day - dow),
            last_week: midnight(day - dow - 7),
            month: midnight(days_from_civil(y, m, 1)),
            last_month: midnight(days_from_civil(py, pm, 1)),
            year: midnight(days_from_civil(y, 1, 1)),
        }
    }

    /// Boundaries of a UTC machine (tests, other platforms).
    pub fn utc(now: i64) -> DateBoundaries {
        Self::from_local(now, |_| 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn name_letters_and_other() {
        for (name, key) in [
            ("alpha", 'A'),
            ("Zebra", 'Z'),
            ("0", '#'),
            ("燕", '#'),
            ("", '#'),
        ] {
            assert_eq!(name_key(&name.encode_utf16().collect::<Vec<_>>()), key);
        }
    }
    #[test]
    fn size_boundaries() {
        for (start, key) in [
            (0, SizeKey::Empty),
            (1, SizeKey::Tiny),
            (16384, SizeKey::Small),
            (1048576, SizeKey::Medium),
            (134217728, SizeKey::Large),
            (1073741824, SizeKey::Huge),
        ] {
            assert_eq!(size_key(start), key);
            if start > 1 {
                assert!(size_key(start - 1) < key);
            }
        }
        assert_eq!(size_key(u64::MAX), SizeKey::Huge);
    }
    #[test]
    fn local_midnights_and_overlapping_week() {
        // Artificial DST day: yesterday is 23 hours before today's midnight.
        let b = DateBoundaries {
            today: 1000000,
            yesterday: 917200,
            week: 1000000,
            last_week: 400000,
            month: 300000,
            last_month: 100000,
            year: 0,
        };
        assert_eq!(b.key(b.today), DateKey::Today);
        assert_eq!(b.key(b.today - 1), DateKey::Yesterday);
        assert_eq!(b.key(b.yesterday), DateKey::Yesterday);
        assert_eq!(b.key(b.yesterday - 1), DateKey::LastWeek);
        assert_eq!(b.key(399999), DateKey::EarlierThisMonth);
        assert_eq!(b.key(299999), DateKey::LastMonth);
        assert_eq!(b.key(99999), DateKey::EarlierThisYear);
        assert_eq!(b.key(-1), DateKey::ALongTimeAgo);
    }

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// unix seconds of a UTC date and time
    fn at(y: i64, m: u32, d: u32, hh: i64, mm: i64) -> i64 {
        days_from_civil(y, m, d) * DAY + hh * 3600 + mm * 60
    }

    /// US Eastern 2025: UTC-5, UTC-4 between 2025-03-09 07:00 UTC and 2025-11-02 06:00 UTC.
    fn eastern(utc: i64) -> i64 {
        if (at(2025, 3, 9, 7, 0)..at(2025, 11, 2, 6, 0)).contains(&utc) { -4 * 3600 } else { -5 * 3600 }
    }

    #[test]
    fn civil_round_trip() {
        for d in [-1000, 0, 59, 60, 365, 11016, 20000, 20089, 20090, 25000] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 2, 29), 11016);
    }

    #[test]
    fn utc_boundaries_midweek() {
        // Wednesday 2025-06-18
        let b = DateBoundaries::utc(at(2025, 6, 18, 12, 30));
        assert_eq!(b.today, at(2025, 6, 18, 0, 0));
        assert_eq!(b.yesterday, at(2025, 6, 17, 0, 0));
        assert_eq!(b.week, at(2025, 6, 16, 0, 0), "Monday");
        assert_eq!(b.last_week, at(2025, 6, 9, 0, 0));
        assert_eq!(b.month, at(2025, 6, 1, 0, 0));
        assert_eq!(b.last_month, at(2025, 5, 1, 0, 0));
        assert_eq!(b.year, at(2025, 1, 1, 0, 0));
        // every bucket, with the item just inside and just outside
        let table = [
            (at(2025, 6, 18, 0, 0), DateKey::Today),
            (at(2025, 6, 17, 23, 59), DateKey::Yesterday),
            (at(2025, 6, 17, 0, 0), DateKey::Yesterday),
            (at(2025, 6, 16, 23, 59), DateKey::EarlierThisWeek),
            (at(2025, 6, 16, 0, 0), DateKey::EarlierThisWeek),
            (at(2025, 6, 15, 23, 59), DateKey::LastWeek),
            (at(2025, 6, 9, 0, 0), DateKey::LastWeek),
            (at(2025, 6, 8, 23, 59), DateKey::EarlierThisMonth),
            (at(2025, 6, 1, 0, 0), DateKey::EarlierThisMonth),
            (at(2025, 5, 31, 23, 59), DateKey::LastMonth),
            (at(2025, 5, 1, 0, 0), DateKey::LastMonth),
            (at(2025, 4, 30, 23, 59), DateKey::EarlierThisYear),
            (at(2025, 1, 1, 0, 0), DateKey::EarlierThisYear),
            (at(2024, 12, 31, 23, 59), DateKey::ALongTimeAgo),
            (at(2030, 1, 1, 0, 0), DateKey::Today),
        ];
        for (t, key) in table {
            assert_eq!(b.key(t), key, "{t}");
        }
    }

    #[test]
    fn monday_has_no_earlier_this_week() {
        // Monday 2025-06-16: Sunday is Yesterday, Saturday is Last week
        let b = DateBoundaries::utc(at(2025, 6, 16, 9, 0));
        assert_eq!(b.week, b.today);
        assert_eq!(b.key(at(2025, 6, 15, 12, 0)), DateKey::Yesterday);
        assert_eq!(b.key(at(2025, 6, 14, 12, 0)), DateKey::LastWeek);
    }

    #[test]
    fn sunday_ends_the_week_on_monday() {
        // Sunday 2025-06-22: the week began Monday the 16th
        let b = DateBoundaries::utc(at(2025, 6, 22, 9, 0));
        assert_eq!(b.week, at(2025, 6, 16, 0, 0));
        assert_eq!(b.key(at(2025, 6, 16, 1, 0)), DateKey::EarlierThisWeek);
        assert_eq!(b.key(at(2025, 6, 15, 1, 0)), DateKey::LastWeek);
    }

    #[test]
    fn month_and_year_edges() {
        // Saturday 2025-03-01: the month began today, last month is February
        let b = DateBoundaries::utc(at(2025, 3, 1, 8, 0));
        assert_eq!(b.month, b.today);
        assert_eq!(b.last_month, at(2025, 2, 1, 0, 0));
        assert_eq!(b.key(at(2025, 2, 28, 23, 0)), DateKey::Yesterday);
        assert_eq!(b.key(at(2025, 2, 10, 0, 0)), DateKey::LastMonth);
        assert_eq!(b.key(at(2025, 1, 31, 23, 0)), DateKey::EarlierThisYear);
        // New Year's Day 2025: December is last month, November is long ago
        let b = DateBoundaries::utc(at(2025, 1, 1, 0, 0));
        assert_eq!(b.last_month, at(2024, 12, 1, 0, 0));
        assert_eq!(b.year, b.today);
        assert_eq!(b.key(at(2024, 12, 15, 0, 0)), DateKey::LastMonth);
        assert_eq!(b.key(at(2024, 11, 30, 0, 0)), DateKey::ALongTimeAgo);
        // leap day
        let b = DateBoundaries::utc(at(2024, 3, 1, 0, 0));
        assert_eq!(b.yesterday, at(2024, 2, 29, 0, 0));
        assert_eq!(b.last_month, at(2024, 2, 1, 0, 0));
    }

    #[test]
    fn local_day_follows_the_offset() {
        // 2025-01-15 03:00 UTC is still the 14th in New York
        let b = DateBoundaries::from_local(at(2025, 1, 15, 3, 0), eastern);
        assert_eq!(b.today, at(2025, 1, 14, 5, 0));
        assert_eq!(b.yesterday, at(2025, 1, 13, 5, 0));
        assert_eq!(b.key(at(2025, 1, 14, 4, 59)), DateKey::Yesterday);
        assert_eq!(b.key(at(2025, 1, 14, 5, 0)), DateKey::Today);
    }

    #[test]
    fn spring_forward_day_is_23_hours() {
        // Monday 2025-03-10 in New York (EDT): yesterday, Sunday the 9th, had 23 hours
        let b = DateBoundaries::from_local(at(2025, 3, 10, 15, 0), eastern);
        assert_eq!(b.today, at(2025, 3, 10, 4, 0), "00:00 EDT");
        assert_eq!(b.yesterday, at(2025, 3, 9, 5, 0), "00:00 EST");
        assert_eq!(b.today - b.yesterday, 23 * 3600);
        assert_eq!(b.week, b.today);
        assert_eq!(b.last_week, at(2025, 3, 3, 5, 0));
        assert_eq!(b.key(at(2025, 3, 9, 5, 0)), DateKey::Yesterday);
        assert_eq!(b.key(at(2025, 3, 9, 4, 59)), DateKey::LastWeek);
        // on the DST day itself
        let b = DateBoundaries::from_local(at(2025, 3, 9, 18, 0), eastern);
        assert_eq!(b.today, at(2025, 3, 9, 5, 0));
        assert_eq!(b.yesterday, at(2025, 3, 8, 5, 0));
        assert_eq!(b.today - b.yesterday, 24 * 3600);
    }

    #[test]
    fn fall_back_day_is_25_hours() {
        // Monday 2025-11-03 in New York (EST): Sunday the 2nd had 25 hours
        let b = DateBoundaries::from_local(at(2025, 11, 3, 15, 0), eastern);
        assert_eq!(b.today, at(2025, 11, 3, 5, 0), "00:00 EST");
        assert_eq!(b.yesterday, at(2025, 11, 2, 4, 0), "00:00 EDT");
        assert_eq!(b.today - b.yesterday, 25 * 3600);
        assert_eq!(b.month, at(2025, 11, 1, 4, 0));
        assert_eq!(b.last_month, at(2025, 10, 1, 4, 0));
        assert_eq!(b.year, at(2025, 1, 1, 5, 0));
        assert_eq!(b.key(at(2025, 11, 2, 4, 30)), DateKey::Yesterday);
        assert_eq!(b.key(at(2025, 11, 2, 3, 30)), DateKey::LastWeek);
    }

    #[test]
    fn midnight_in_a_dst_gap_or_repeat() {
        const T: i64 = 1000 * DAY;
        // clocks jump 00:00 -> 01:00 at T: local midnight never happens, the day starts at the jump
        let gap = |utc: i64| if utc >= T { 3600 } else { 0 };
        assert_eq!(local_to_utc(T, &gap), T);
        // clocks go back 01:00 -> 00:00 at T: midnight happens twice, the first one counts
        let back = |utc: i64| if utc >= T { 0 } else { 3600 };
        assert_eq!(local_to_utc(T, &back), T - 3600);
        // an ordinary day
        assert_eq!(local_to_utc(T, &|_| 7200), T - 7200);
    }

    #[test]
    fn type_keys() {
        let k = |n: &str, dir| type_key(&u(n), dir);
        assert_eq!(k("sub", true), TypeKey::Folder);
        assert_eq!(k("dir.d", true), TypeKey::Folder, "a folder's dot is no extension");
        assert_eq!(k("a.TXT", false), k("b.txt", false));
        assert_ne!(k("a.txt", false), k("a.rs", false));
        assert_eq!(k("README", false), TypeKey::NoExt);
        assert_eq!(k(".gitignore", false), TypeKey::NoExt);
        assert_eq!(k("trailing.", false), TypeKey::NoExt);
        assert_eq!(k("a.tar.gz", false), k("b.gz", false));
        assert_eq!(k("x.averyveryverylongextension", false), k("y.averyveryverylongextensioN", false));
        assert!(k("z", true) < k("a.txt", false), "folders first");
        assert!(k("a.zip", false) < k("README", false), "no extension last");
        assert!(k("a.png", false) < k("a.txt", false));
        assert_eq!(k("a.TXT", false).label(), "TXT File");
        assert_eq!(TypeKey::Folder.label(), "Folder");
        assert_eq!(TypeKey::NoExt.label(), "File");
        assert_eq!(k("a.\u{71d5}", false).label(), "\u{71d5} File");
    }

    #[test]
    fn group_keys_per_mode() {
        let d = DateBoundaries::utc(at(2025, 6, 18, 12, 0));
        let key = |by, name: &str, dir, size, t| group_key(by, &u(name), dir, size, t, &d);
        assert_eq!(key(GroupBy::None, "a", false, 0, 0), None);
        assert_eq!(key(GroupBy::Name, "beta", false, 0, 0), Some(GroupKey::Name('B')));
        assert_eq!(key(GroupBy::Name, "9lives", false, 0, 0), Some(GroupKey::Name('#')));
        assert_eq!(key(GroupBy::Type, "a.rs", false, 0, 0), Some(GroupKey::Type(type_key(&u("x.rs"), false))));
        assert_eq!(key(GroupBy::Date, "a", false, 0, at(2025, 6, 18, 1, 0)), Some(GroupKey::Date(DateKey::Today)));
        assert_eq!(key(GroupBy::Size, "a", false, 2 << 20, 0), Some(GroupKey::Size(SizeKey::Medium)));
        assert_eq!(key(GroupBy::Size, "dir", true, 5 << 30, 0), Some(GroupKey::Size(SizeKey::Empty)), "folders have no size");
        assert_eq!(GroupKey::Name('Q').label(), "Q");
        assert_eq!(GroupKey::Date(DateKey::EarlierThisWeek).label(), "Earlier this week");
        assert_eq!(GroupKey::Size(SizeKey::Tiny).label(), "Tiny (under 16 KB)");
    }

    #[test]
    fn group_by_names_round_trip_and_cycle() {
        for g in GroupBy::ALL {
            assert_eq!(GroupBy::parse(g.name()), Some(g));
        }
        assert_eq!(GroupBy::parse(" DATE "), Some(GroupBy::Date));
        assert_eq!(GroupBy::parse("colour"), None);
        let mut g = GroupBy::None;
        for want in [GroupBy::Name, GroupBy::Type, GroupBy::Date, GroupBy::Size, GroupBy::None] {
            g = g.next();
            assert_eq!(g, want);
        }
    }
}
