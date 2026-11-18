//! Renaming: the rules for a legal name, the stem preselection, the slow
//! second click, and the batch pattern engine (`{name}` `{n}` `{date}`).
//! Pure functions, no UI and no disk, so every rule is unit tested. The view
//! draws the inline field and the batch bar; the file-op queue does the work.

/// Why a name is refused. `message()` is the text shown under the field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    /// one of `\ / : * ? " < > |` or a control character
    IllegalChar,
    /// ends with a space or a period (also "." and "..")
    TrailingDotOrSpace,
    /// CON, PRN, AUX, NUL, COM1-9, LPT1-9 (with or without an extension)
    Reserved(String),
    /// more than 255 UTF-16 units
    TooLong,
    /// the full path would pass the Win32 limit
    PathTooLong,
    /// another entry in the folder already has this name
    Exists,
}

impl NameError {
    pub fn message(&self) -> String {
        match self {
            NameError::Empty => "Name cannot be empty".to_string(),
            NameError::IllegalChar => "Name cannot contain \\ / : * ? \" < > |".to_string(),
            NameError::TrailingDotOrSpace => "Name cannot end with a space or period".to_string(),
            NameError::Reserved(n) => format!("\"{n}\" is a reserved name"),
            NameError::TooLong => "Name is too long (255 characters at most)".to_string(),
            NameError::PathTooLong => "Path is too long".to_string(),
            NameError::Exists => "A file with this name already exists".to_string(),
        }
    }
    /// Shift+Enter may replace the existing entry; no other error can be forced.
    pub fn can_replace(&self) -> bool {
        matches!(self, NameError::Exists)
    }
}

pub const MAX_NAME_UNITS: usize = 255;
pub const MAX_PATH_UNITS: usize = 32_000;

fn is_illegal(c: char) -> bool {
    matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 0x20
}

/// The device name the file name stem collides with, upper-cased.
pub fn reserved_name(name: &str) -> Option<String> {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let up = stem.to_ascii_uppercase();
    let hit = matches!(up.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (up.len() == 4 && (up.starts_with("COM") || up.starts_with("LPT")) && matches!(up.as_bytes()[3], b'1'..=b'9'));
    hit.then_some(up)
}

/// The form-only rules (no conflict check).
pub fn validate_form(name: &str, parent: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().any(is_illegal) {
        return Err(NameError::IllegalChar);
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err(NameError::TrailingDotOrSpace);
    }
    if let Some(r) = reserved_name(name) {
        return Err(NameError::Reserved(r));
    }
    let units = name.encode_utf16().count();
    if units > MAX_NAME_UNITS {
        return Err(NameError::TooLong);
    }
    if parent.encode_utf16().count() + 1 + units > MAX_PATH_UNITS {
        return Err(NameError::PathTooLong);
    }
    Ok(())
}

/// Full check for renaming `original` in `parent` to `name`. `exists` asks
/// whether the folder already holds an entry with that name (case-insensitive).
/// The original's own name never conflicts, so a case-only change is allowed.
pub fn validate(name: &str, original: &str, parent: &str, exists: impl Fn(&str) -> bool) -> Result<(), NameError> {
    validate_form(name, parent)?;
    if name.to_lowercase() != original.to_lowercase() && exists(name) {
        return Err(NameError::Exists);
    }
    Ok(())
}

/// Length (in `char`s) of the part the field preselects when it opens: the
/// stem for a file, everything for a folder or a dot file.
pub fn stem_chars(name: &str, is_dir: bool) -> usize {
    let total = name.chars().count();
    if is_dir {
        return total;
    }
    match name.rfind('.') {
        Some(0) | None => total,
        Some(i) => name[..i].chars().count(),
    }
}

/// Split into stem and extension (".txt", empty when there is none). Folders
/// and dot files have no extension.
pub fn split_ext(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i..]),
    }
}

/// A second click on the already selected row starts a rename when it comes
/// late enough not to be a double click and soon enough to mean it.
pub const SLOW_CLICK_MIN_MS: u64 = 400;
pub const SLOW_CLICK_MAX_MS: u64 = 1200;

/// `prev_ms`: when the previous click on this row happened; `was_sole`: the row
/// was the only selected one then; `now_ms`: this click.
pub fn is_slow_click(prev_ms: Option<u64>, was_sole: bool, now_ms: u64) -> bool {
    match prev_ms {
        Some(p) if was_sole => {
            let dt = now_ms.saturating_sub(p);
            (SLOW_CLICK_MIN_MS..=SLOW_CLICK_MAX_MS).contains(&dt)
        }
        _ => false,
    }
}

// ---------------------------------------------------------------- batch

/// One item of a batch rename.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchItem {
    pub name: String,
    pub is_dir: bool,
    /// unix seconds (modified time), for `{date}`
    pub mtime: u32,
}

/// yyyy-MM-dd of a unix time (UTC; the same day the details column shows
/// close enough for naming).
pub fn date_string(unix: u32) -> String {
    let days = (unix / 86_400) as i64;
    // civil_from_days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Expand `pattern` for the item at `index` (0-based; `{n}` counts from 1).
/// `{n:3}` pads to 3 digits. Unknown braces stay as typed. The original
/// extension is appended, so the pattern names only the stem.
pub fn expand(pattern: &str, item: &BatchItem, index: usize) -> String {
    let (stem, ext) = split_ext(&item.name, item.is_dir);
    let mut out = String::with_capacity(pattern.len() + ext.len() + 8);
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close) = after.find('}') else {
            out.push_str(after);
            rest = "";
            break;
        };
        let token = &after[1..close];
        match token {
            "name" => out.push_str(stem),
            "date" => out.push_str(&date_string(item.mtime)),
            "n" => out.push_str(&(index + 1).to_string()),
            t if t.starts_with("n:") && t[2..].parse::<usize>().is_ok_and(|w| w <= 9) => {
                let w: usize = t[2..].parse().unwrap_or(0);
                out.push_str(&format!("{:0w$}", index + 1, w = w));
            }
            _ => out.push_str(&after[..=close]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out.push_str(ext);
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowProblem {
    /// not a legal name
    Invalid(NameError),
    /// the same new name as another row
    Duplicate,
    /// a name already in the folder (that is not this row's own)
    Taken,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchRow {
    pub old: String,
    pub new: String,
    pub problem: Option<RowProblem>,
}

impl BatchRow {
    pub fn unchanged(&self) -> bool {
        self.old == self.new
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchPlan {
    pub rows: Vec<BatchRow>,
}

impl BatchPlan {
    pub fn conflicts(&self) -> usize {
        self.rows.iter().filter(|r| r.problem.is_some()).count()
    }
    /// Rows whose name really changes.
    pub fn changes(&self) -> usize {
        self.rows.iter().filter(|r| !r.unchanged()).count()
    }
    /// Apply is allowed: something changes and nothing collides (all or nothing).
    pub fn can_apply(&self) -> bool {
        self.conflicts() == 0 && self.changes() > 0
    }
    pub fn duplicates(&self) -> usize {
        self.rows.iter().filter(|r| r.problem == Some(RowProblem::Duplicate)).count()
    }
    /// The pattern has no `{n}` and several rows collide: the footer offers it.
    pub fn suggests_counter(&self, pattern: &str) -> bool {
        self.duplicates() > 0 && !pattern.contains("{n")
    }
}

/// Plan renaming `items` with `pattern`. `folder_names` is every name in the
/// folder (batch members included); a new name equal to one of them is "taken"
/// unless it is the row's own old name. Conservative: a name that another row
/// is about to free still counts as taken, so no ordering is ever needed.
pub fn plan_batch(pattern: &str, items: &[BatchItem], parent: &str, folder_names: &[String]) -> BatchPlan {
    let news: Vec<String> = items.iter().enumerate().map(|(i, it)| expand(pattern, it, i)).collect();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for n in &news {
        *seen.entry(n.to_lowercase()).or_insert(0) += 1;
    }
    let taken: std::collections::HashSet<String> = folder_names.iter().map(|n| n.to_lowercase()).collect();
    let rows = items
        .iter()
        .zip(news)
        .map(|(it, new)| {
            let lower = new.to_lowercase();
            let own = it.name.to_lowercase();
            let problem = if let Err(e) = validate_form(&new, parent) {
                Some(RowProblem::Invalid(e))
            } else if seen.get(&lower).copied().unwrap_or(0) > 1 {
                Some(RowProblem::Duplicate)
            } else if lower != own && taken.contains(&lower) {
                Some(RowProblem::Taken)
            } else {
                None
            };
            BatchRow { old: it.name.clone(), new, problem }
        })
        .collect();
    BatchPlan { rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn empty_and_illegal() {
        assert_eq!(validate("", "a", "C:\\d", none), Err(NameError::Empty));
        for bad in ["a\\b", "a/b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\tb"] {
            assert_eq!(validate(bad, "a", "C:\\d", none), Err(NameError::IllegalChar), "{bad:?}");
        }
        assert!(validate("ok name (1).txt", "a", "C:\\d", none).is_ok());
        assert!(validate("日本語.txt", "a", "C:\\d", none).is_ok());
    }

    #[test]
    fn trailing_dot_or_space() {
        for bad in ["a.", "a ", ".", "..", "name. "] {
            assert_eq!(validate(bad, "x", "C:\\d", none), Err(NameError::TrailingDotOrSpace), "{bad:?}");
        }
        assert!(validate(".gitignore", "x", "C:\\d", none).is_ok());
    }

    #[test]
    fn reserved_names() {
        for bad in ["CON", "con", "Nul.txt", "aux.tar.gz", "COM1", "lpt9.log", "PRN"] {
            assert!(matches!(validate(bad, "x", "C:\\d", none), Err(NameError::Reserved(_))), "{bad:?}");
        }
        assert_eq!(validate("con", "x", "C:\\d", none).unwrap_err().message(), "\"CON\" is a reserved name");
        for ok in ["CONSOLE", "COM0", "COM10", "LPT", "console.txt", "xcon"] {
            assert!(validate(ok, "x", "C:\\d", none).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn length_limits() {
        let long = "a".repeat(256);
        assert_eq!(validate(&long, "x", "C:\\d", none), Err(NameError::TooLong));
        assert!(validate(&"a".repeat(255), "x", "C:\\d", none).is_ok());
        // 255 UTF-16 units, not chars: a surrogate pair counts twice
        let wide = "😀".repeat(128);
        assert_eq!(validate(&wide, "x", "C:\\d", none), Err(NameError::TooLong));
        let deep = format!("C:\\{}", "d".repeat(31_900));
        assert_eq!(validate(&"n".repeat(200), "x", &deep, none), Err(NameError::PathTooLong));
        assert!(validate("n", "x", &deep, none).is_ok());
    }

    #[test]
    fn conflicts_ignore_case_and_self() {
        let exists = |n: &str| n.eq_ignore_ascii_case("Taken.txt") || n.eq_ignore_ascii_case("mine.txt");
        assert_eq!(validate("taken.TXT", "mine.txt", "C:\\d", exists), Err(NameError::Exists));
        assert!(validate("Taken.txt", "mine.txt", "C:\\d", exists).unwrap_err().can_replace());
        // case-only change of itself is fine
        assert!(validate("MINE.txt", "mine.txt", "C:\\d", exists).is_ok());
        // unchanged is fine
        assert!(validate("mine.txt", "mine.txt", "C:\\d", exists).is_ok());
        assert!(!NameError::Empty.can_replace());
        assert_eq!(NameError::Exists.message(), "A file with this name already exists");
    }

    #[test]
    fn stems() {
        assert_eq!(stem_chars("report.final.txt", false), "report.final".len());
        assert_eq!(stem_chars("archive.tar.gz", false), "archive.tar".len());
        assert_eq!(stem_chars("Docs.old", true), 8);
        assert_eq!(stem_chars(".gitignore", false), 10);
        assert_eq!(stem_chars("README", false), 6);
        assert_eq!(stem_chars("日本.txt", false), 2);
        assert_eq!(split_ext("a.b.txt", false), ("a.b", ".txt"));
        assert_eq!(split_ext("a.b", true), ("a.b", ""));
        assert_eq!(split_ext(".rc", false), (".rc", ""));
    }

    #[test]
    fn slow_click_window() {
        assert!(is_slow_click(Some(1000), true, 1600));
        assert!(!is_slow_click(Some(1000), true, 1200), "a double click");
        assert!(!is_slow_click(Some(1000), true, 2500), "too late");
        assert!(!is_slow_click(Some(1000), false, 1600), "was not the sole selection");
        assert!(!is_slow_click(None, true, 1600));
        assert!(is_slow_click(Some(1000), true, 1400));
        assert!(is_slow_click(Some(1000), true, 2200));
        assert!(!is_slow_click(Some(1000), true, 2201));
    }

    #[test]
    fn dates() {
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(date_string(951_782_400), "2000-02-29");
        assert_eq!(date_string(1_700_000_000), "2023-11-14");
        assert_eq!(date_string(1_709_164_800), "2024-02-29");
    }

    fn item(name: &str) -> BatchItem {
        BatchItem { name: name.to_string(), is_dir: false, mtime: 1_700_000_000 }
    }

    #[test]
    fn pattern_tokens() {
        let it = item("IMG_0042.JPG");
        assert_eq!(expand("{name}", &it, 0), "IMG_0042.JPG");
        assert_eq!(expand("Trip {n}", &it, 0), "Trip 1.JPG");
        assert_eq!(expand("Trip {n}", &it, 11), "Trip 12.JPG");
        assert_eq!(expand("Trip {n:3}", &it, 6), "Trip 007.JPG");
        assert_eq!(expand("{date}_{name}", &it, 0), "2023-11-14_IMG_0042.JPG");
        assert_eq!(expand("{bogus}-{n", &it, 0), "{bogus}-{n.JPG");
        assert_eq!(expand("plain", &it, 3), "plain.JPG");
        // folders keep their dots
        let dir = BatchItem { name: "v1.2".into(), is_dir: true, mtime: 0 };
        assert_eq!(expand("{name}-{n}", &dir, 0), "v1.2-1");
        // dot file keeps everything
        assert_eq!(expand("x", &item(".rc"), 0), "x");
    }

    #[test]
    fn batch_plan_ok() {
        let items = vec![item("a.txt"), item("b.txt"), item("c.txt")];
        let names: Vec<String> = ["a.txt", "b.txt", "c.txt", "other.md"].iter().map(|s| s.to_string()).collect();
        let p = plan_batch("Doc {n}", &items, "C:\\d", &names);
        assert_eq!(p.rows.iter().map(|r| r.new.as_str()).collect::<Vec<_>>(), ["Doc 1.txt", "Doc 2.txt", "Doc 3.txt"]);
        assert_eq!(p.conflicts(), 0);
        assert_eq!(p.changes(), 3);
        assert!(p.can_apply());
    }

    #[test]
    fn batch_plan_duplicates_and_hint() {
        let items = vec![item("a.txt"), item("b.txt"), item("c.md")];
        let names: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
        let p = plan_batch("same", &items, "C:\\d", &names);
        assert_eq!(p.rows[0].problem, Some(RowProblem::Duplicate));
        assert_eq!(p.rows[1].problem, Some(RowProblem::Duplicate));
        assert_eq!(p.rows[2].problem, None, "a different extension is a different name");
        assert_eq!(p.duplicates(), 2);
        assert_eq!(p.conflicts(), 2);
        assert!(!p.can_apply());
        assert!(p.suggests_counter("same"));
        assert!(!p.suggests_counter("same {n}"));
    }

    #[test]
    fn batch_plan_taken_and_invalid() {
        let items = vec![item("a.txt"), item("b.txt")];
        let names: Vec<String> = ["a.txt", "b.txt", "Doc 2.txt"].iter().map(|s| s.to_string()).collect();
        let p = plan_batch("Doc {n}", &items, "C:\\d", &names);
        assert_eq!(p.rows[0].problem, None);
        assert_eq!(p.rows[1].problem, Some(RowProblem::Taken));
        // shifting onto a sibling that is itself in the batch is conservative
        let shift = plan_batch("{name}", &items, "C:\\d", &names);
        assert!(!shift.can_apply(), "nothing changes");
        assert_eq!(shift.changes(), 0);
        let bad = plan_batch("a:{n}", &items, "C:\\d", &names);
        assert!(matches!(bad.rows[0].problem, Some(RowProblem::Invalid(NameError::IllegalChar))));
        let swap = plan_batch("b", &[item("a.txt")], "C:\\d", &["a.txt".to_string(), "b.txt".to_string()]);
        assert_eq!(swap.rows[0].new, "b.txt");
        assert_eq!(swap.rows[0].problem, Some(RowProblem::Taken));
    }

    #[test]
    fn batch_case_only_change_is_not_taken() {
        let p = plan_batch("{name}", &[item("a.txt")], "C:\\d", &["a.txt".to_string()]);
        assert_eq!(p.conflicts(), 0);
        let up = plan_batch("A", &[item("a.txt")], "C:\\d", &["a.txt".to_string()]);
        assert_eq!(up.conflicts(), 0);
        assert!(up.can_apply());
    }
}
