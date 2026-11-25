//! `[file_age]`: file-age colouring (P4.F6, built-in extension `tsubame.ext.file_age`,
//! `docs/f6-mock.md`). Pure settings; the bucketing lives in the app (`src/age.rs`).

use super::*;

lossy_enum! {
    /// How an age bucket is drawn on a row.
    AgeStyle { Text => "text", Chip => "chip", Accent => "accent" }
    default Text
    , "date-text" => Text, "date-chip" => Chip, "bar" => Accent, "row-accent" => Accent
}

lossy_enum! {
    /// Colour set. `colour-blind` and `mono` always draw pips.
    AgePalette { Heat => "heat", ColourBlind => "colour-blind", Mono => "mono" }
    default Heat
    , "cb" => ColourBlind, "colorblind" => ColourBlind, "color-blind" => ColourBlind, "monochrome" => Mono
}

lossy_enum! {
    /// What "today" means.
    AgeToday { Midnight => "midnight", Last24h => "24h" }
    default Midnight
    , "since-midnight" => Midnight, "last-24h" => Last24h, "last_24h" => Last24h
}

pub const DEFAULT_WEEK_DAYS: u32 = 7;
pub const DEFAULT_MONTH_DAYS: u32 = 30;
pub const DEFAULT_YEAR_DAYS: u32 = 365;
/// Largest threshold the settings accept (100 years).
pub const MAX_AGE_DAYS: u32 = 36_500;

fn d_week() -> u32 {
    DEFAULT_WEEK_DAYS
}
fn d_month() -> u32 {
    DEFAULT_MONTH_DAYS
}
fn d_year() -> u32 {
    DEFAULT_YEAR_DAYS
}

/// Why a threshold set is rejected (the message the settings card shows).
pub fn thresholds_error(week: u32, month: u32, year: u32) -> Option<String> {
    if week < 1 {
        return Some("Week must be at least 1 day.".into());
    }
    if month <= week {
        return Some(format!("Month ({month} days) must be longer than Week ({week} days)."));
    }
    if year <= month {
        return Some(format!("Year ({year} days) must be longer than Month ({month} days)."));
    }
    if year > MAX_AGE_DAYS {
        return Some(format!("Year can be at most {MAX_AGE_DAYS} days."));
    }
    None
}

/// `[file_age]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileAge {
    /// Colours are on. Off by default; the status-bar key and the palette
    /// command `view.age_colours` flip it.
    pub enabled: bool,
    pub style: AgeStyle,
    pub palette: AgePalette,
    /// Pips before the date for the Heat palette (the other two always show them).
    pub pips: bool,
    pub today: AgeToday,
    #[serde(default = "d_week")]
    pub week_days: u32,
    #[serde(default = "d_month")]
    pub month_days: u32,
    #[serde(default = "d_year")]
    pub year_days: u32,
    /// Tint folders too (their date only tracks direct children). Off by default.
    pub folders: bool,
}

impl Default for FileAge {
    fn default() -> Self {
        FileAge {
            enabled: false,
            style: AgeStyle::Text,
            palette: AgePalette::Heat,
            pips: false,
            today: AgeToday::Midnight,
            week_days: DEFAULT_WEEK_DAYS,
            month_days: DEFAULT_MONTH_DAYS,
            year_days: DEFAULT_YEAR_DAYS,
            folders: false,
        }
    }
}

impl FileAge {
    /// The thresholds in force: the configured ones when valid, else the defaults.
    pub fn thresholds(&self) -> (u32, u32, u32) {
        if thresholds_error(self.week_days, self.month_days, self.year_days).is_none() {
            (self.week_days, self.month_days, self.year_days)
        } else {
            (DEFAULT_WEEK_DAYS, DEFAULT_MONTH_DAYS, DEFAULT_YEAR_DAYS)
        }
    }

    /// Pips are drawn: always for the accessible palettes, optional for Heat.
    pub fn shows_pips(&self) -> bool {
        self.palette != AgePalette::Heat || self.pips
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_owner_decisions() {
        let c = Config::default().file_age;
        assert!(!c.enabled && !c.folders && !c.pips);
        assert_eq!((c.style, c.palette, c.today), (AgeStyle::Text, AgePalette::Heat, AgeToday::Midnight));
        assert_eq!(c.thresholds(), (7, 30, 365));
    }

    #[test]
    fn threshold_validation() {
        assert!(thresholds_error(7, 30, 365).is_none());
        assert!(thresholds_error(1, 2, 3).is_none());
        assert!(thresholds_error(0, 30, 365).is_some());
        assert!(thresholds_error(7, 7, 365).is_some(), "equal is invalid");
        assert!(thresholds_error(7, 6, 365).is_some());
        assert!(thresholds_error(7, 30, 30).is_some());
        assert!(thresholds_error(7, 30, MAX_AGE_DAYS + 1).is_some());
        let bad = FileAge { month_days: 6, ..FileAge::default() };
        assert_eq!(bad.thresholds(), (7, 30, 365), "an invalid file set falls back to the defaults");
    }

    #[test]
    fn round_trips_and_unknown_values_fall_back() {
        let text = "[file_age]\nenabled = true\nstyle = \"chip\"\npalette = \"colour-blind\"\ntoday = \"24h\"\nweek_days = 3\nmonth_days = 20\nyear_days = 100\nfolders = true\n";
        let c: Config = toml::from_str(text).unwrap();
        assert!(c.file_age.enabled && c.file_age.folders);
        assert_eq!((c.file_age.style, c.file_age.palette, c.file_age.today), (AgeStyle::Chip, AgePalette::ColourBlind, AgeToday::Last24h));
        assert_eq!(c.file_age.thresholds(), (3, 20, 100));
        let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.file_age, c.file_age);
        let odd: Config = toml::from_str("[file_age]\nstyle = \"neon\"\npalette = \"x\"\n").unwrap();
        assert_eq!((odd.file_age.style, odd.file_age.palette), (AgeStyle::Text, AgePalette::Heat));
        assert!(!odd.file_age.shows_pips());
        assert!(FileAge { palette: AgePalette::Mono, ..FileAge::default() }.shows_pips());
    }
}
