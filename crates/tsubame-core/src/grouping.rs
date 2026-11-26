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
#[derive(Clone, Copy, Debug)]
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
}
