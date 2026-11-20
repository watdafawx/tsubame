//! Query parsing and fast matching for the parallel directory walk engine.
//!
//! Supports:
//! - Substring matching (case-insensitive by default, or case-sensitive)
//! - Wildcard globbing (`*` matches 0+ characters, `?` matches any 1 character)
//! - Space-separated multi-term AND matching (with `"quoted phrases"`)
//! - Path-aware terms (a term containing `\` or `/` matches against the full path)
//!
//! Uses an ASCII zero-allocation fast path so walking hundreds of thousands of
//! files does not allocate per entry.

use crate::search::SearchQuery;

/// One parsed search term in a [`CompiledQuery`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct Term {
    /// Pattern text (lowercased when `!case_sensitive`, `/` normalized to `\` if `match_path`).
    pattern: String,
    /// Pre-split char slice for non-ASCII wildcard matching.
    pattern_chars: Vec<char>,
    /// True when the pattern contains `*` or `?`.
    has_wildcards: bool,
    /// True when the pattern contains a path separator (`\` or `/`), so it
    /// should be matched against the full path instead of just the file name.
    match_path: bool,
    /// True when `pattern` is pure ASCII (enables zero-allocation byte fast path).
    is_ascii: bool,
}

/// Pre-compiled representation of a [`SearchQuery`] for fast repeated matching
/// across worker threads.
#[derive(Clone, Debug)]
pub struct CompiledQuery {
    terms: Vec<Term>,
    case_sensitive: bool,
    include_dirs: bool,
    include_files: bool,
}

impl CompiledQuery {
    /// Compile a [`SearchQuery`] into an immutable matcher safe to share across threads.
    pub fn new(query: &SearchQuery) -> Self {
        let raw_terms = split_terms(&query.text);
        let terms = raw_terms
            .into_iter()
            .map(|raw| {
                let match_path = raw.contains('\\') || raw.contains('/');
                let normalized = if match_path {
                    raw.replace('/', "\\")
                } else {
                    raw
                };
                let pattern = if query.case_sensitive {
                    normalized
                } else {
                    normalized.to_lowercase()
                };
                let has_wildcards = pattern.contains('*') || pattern.contains('?');
                let is_ascii = pattern.is_ascii();
                let pattern_chars = if has_wildcards && !is_ascii {
                    pattern.chars().collect()
                } else {
                    Vec::new()
                };
                Term {
                    pattern,
                    pattern_chars,
                    has_wildcards,
                    match_path,
                    is_ascii,
                }
            })
            .collect();

        Self {
            terms,
            case_sensitive: query.case_sensitive,
            include_dirs: query.include_dirs,
            include_files: query.include_files,
        }
    }

    /// True when neither files nor directories are included.
    #[inline]
    pub fn excludes_everything(&self) -> bool {
        !self.include_dirs && !self.include_files
    }

    /// Check whether an entry's kind (`is_dir`) is requested by the query.
    #[inline]
    pub fn accepts_kind(&self, is_dir: bool) -> bool {
        if is_dir {
            self.include_dirs
        } else {
            self.include_files
        }
    }

    /// Test whether `(name, full_path, is_dir)` matches the compiled query.
    #[inline]
    pub fn matches(&self, name: &str, full_path: &str, is_dir: bool) -> bool {
        if !self.accepts_kind(is_dir) {
            return false;
        }
        for term in &self.terms {
            let haystack = if term.match_path { full_path } else { name };
            if !term.matches(haystack, self.case_sensitive) {
                return false;
            }
        }
        true
    }

    /// Number of parsed AND terms.
    pub fn term_count(&self) -> usize {
        self.terms.len()
    }
}

impl Term {
    #[inline]
    fn matches(&self, target: &str, case_sensitive: bool) -> bool {
        if self.has_wildcards {
            if self.is_ascii && target.is_ascii() {
                if case_sensitive {
                    wildcard_match_bytes(self.pattern.as_bytes(), target.as_bytes(), false)
                } else {
                    wildcard_match_bytes(self.pattern.as_bytes(), target.as_bytes(), true)
                }
            } else {
                let target_owned;
                let target_str = if case_sensitive {
                    target
                } else {
                    target_owned = target.to_lowercase();
                    &target_owned
                };
                let target_chars: Vec<char> = target_str.chars().collect();
                if self.pattern_chars.is_empty() {
                    let pat_chars: Vec<char> = self.pattern.chars().collect();
                    wildcard_match_chars(&pat_chars, &target_chars)
                } else {
                    wildcard_match_chars(&self.pattern_chars, &target_chars)
                }
            }
        } else if case_sensitive {
            target.contains(&self.pattern)
        } else if self.is_ascii && target.is_ascii() {
            contains_ascii_ci(target.as_bytes(), self.pattern.as_bytes())
        } else {
            target.to_lowercase().contains(&self.pattern)
        }
    }
}

/// Split `input` on whitespace, keeping `"quoted phrases"` as single terms.
fn split_terms(input: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    terms.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms
}

/// Zero-allocation ASCII case-insensitive substring check (`needle` is already lowercase).
#[inline]
fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let first = needle[0];
    let limit = haystack.len() - needle.len();
    let mut i = 0;
    while i <= limit {
        if haystack[i].to_ascii_lowercase() == first {
            let mut j = 1;
            while j < needle.len() && haystack[i + j].to_ascii_lowercase() == needle[j] {
                j += 1;
            }
            if j == needle.len() {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Iterative two-pointer wildcard matcher on ASCII byte slices (`pat` is lowercase when `ci` is true).
#[inline]
fn wildcard_match_bytes(pat: &[u8], text: &[u8], ci: bool) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star_p: Option<usize> = None;
    let mut star_t = 0usize;

    while t < text.len() {
        if p < pat.len() {
            let pc = pat[p];
            let tc = if ci {
                text[t].to_ascii_lowercase()
            } else {
                text[t]
            };
            if pc == b'?' || pc == tc {
                p += 1;
                t += 1;
                continue;
            }
            if pc == b'*' {
                star_p = Some(p);
                p += 1;
                star_t = t;
                continue;
            }
        }
        if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// Iterative two-pointer wildcard matcher on Unicode scalar value slices.
fn wildcard_match_chars(pat: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star_p: Option<usize> = None;
    let mut star_t = 0usize;

    while t < text.len() {
        if p < pat.len() {
            let pc = pat[p];
            if pc == '?' || pc == text[t] {
                p += 1;
                t += 1;
                continue;
            }
            if pc == '*' {
                star_p = Some(p);
                p += 1;
                star_t = t;
                continue;
            }
        }
        if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> SearchQuery {
        SearchQuery {
            text: text.to_string(),
            ..SearchQuery::default()
        }
    }

    #[test]
    fn substring_case_insensitive_and_sensitive() {
        let ci = CompiledQuery::new(&query("Report"));
        assert!(ci.matches("annual_report_2026.pdf", "C:\\docs\\annual_report_2026.pdf", false));
        assert!(ci.matches("REPORT.TXT", "C:\\REPORT.TXT", false));
        assert!(!ci.matches("summary.pdf", "C:\\docs\\summary.pdf", false));

        let mut cs_q = query("Report");
        cs_q.case_sensitive = true;
        let cs = CompiledQuery::new(&cs_q);
        assert!(cs.matches("AnnualReport.pdf", "C:\\AnnualReport.pdf", false));
        assert!(!cs.matches("annual_report.pdf", "C:\\annual_report.pdf", false));
    }

    #[test]
    fn multi_term_and_matching() {
        let m = CompiledQuery::new(&query("cargo toml"));
        assert_eq!(m.term_count(), 2);
        assert!(m.matches("Cargo.toml", "C:\\proj\\Cargo.toml", false));
        assert!(m.matches("toml_cargo_backup", "C:\\toml_cargo_backup", false));
        assert!(!m.matches("Cargo.lock", "C:\\proj\\Cargo.lock", false));
        assert!(!m.matches("config.toml", "C:\\proj\\config.toml", false));

        let quoted = CompiledQuery::new(&query("\"my report\" 2026"));
        assert_eq!(quoted.term_count(), 2);
        assert!(quoted.matches("My Report 2026.docx", "C:\\My Report 2026.docx", false));
        assert!(!quoted.matches("my_report_2026.docx", "C:\\my_report_2026.docx", false));
    }

    #[test]
    fn wildcard_matching_star_and_question() {
        let ext = CompiledQuery::new(&query("*.rs"));
        assert!(ext.matches("main.rs", "C:\\src\\main.rs", false));
        assert!(ext.matches("MOD.RS", "C:\\src\\MOD.RS", false));
        assert!(!ext.matches("main.rs.bak", "C:\\src\\main.rs.bak", false));

        let qmark = CompiledQuery::new(&query("log_??.txt"));
        assert!(qmark.matches("log_01.txt", "C:\\log_01.txt", false));
        assert!(!qmark.matches("log_1.txt", "C:\\log_1.txt", false));
        assert!(!qmark.matches("log_001.txt", "C:\\log_001.txt", false));

        // Combined wildcard + substring AND
        let combo = CompiledQuery::new(&query("*.rs search"));
        assert!(combo.matches("global_search.rs", "C:\\global_search.rs", false));
        assert!(!combo.matches("global_search.txt", "C:\\global_search.txt", false));
        assert!(!combo.matches("main.rs", "C:\\main.rs", false));

        // Unicode wildcards and case folding
        let uni = CompiledQuery::new(&query("日本語*?.txt"));
        assert!(uni.matches("日本語テスト1.txt", "C:\\日本語テスト1.txt", false));
        assert!(!uni.matches("日本語.txt", "C:\\日本語.txt", false));
    }

    #[test]
    fn path_term_and_kind_filters() {
        let path_m = CompiledQuery::new(&query("src/search *.rs"));
        assert!(path_m.matches("walk.rs", "C:\\repo\\src\\search\\walk.rs", false));
        assert!(!path_m.matches("walk.rs", "C:\\repo\\src\\other\\walk.rs", false));

        let mut dirs_only = query("build");
        dirs_only.include_files = false;
        dirs_only.include_dirs = true;
        let dm = CompiledQuery::new(&dirs_only);
        assert!(dm.matches("build", "C:\\build", true));
        assert!(!dm.matches("build.rs", "C:\\build.rs", false));

        let mut files_only = query("build");
        files_only.include_files = true;
        files_only.include_dirs = false;
        let fm = CompiledQuery::new(&files_only);
        assert!(!fm.matches("build", "C:\\build", true));
        assert!(fm.matches("build.rs", "C:\\build.rs", false));
    }
}
