//! Fuzzy matching for the command palette: the query's characters must appear
//! in order in the name (case ignored); the best alignment scores higher for
//! matches at the start of the name or of a word, for runs of consecutive
//! characters, and for fewer skipped characters. Names are short (under 80
//! characters), so the exact best alignment is found with a small DP.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    /// char index in the haystack of every query char, ascending (highlighting)
    pub positions: Vec<usize>,
}

const SEP: &[char] = &[' ', '-', '_', '.', '/', '\\', ':', '>'];

fn is_sep(c: char) -> bool {
    SEP.contains(&c)
}

const BASE: i32 = 2;
const START: i32 = 18;
const WORD: i32 = 12;
const RUN: i32 = 9;
const CASE: i32 = 5;
const GAP: i32 = 1;
const LEAD_GAP: i32 = 1;
/// the gap penalty never grows past this per gap
const GAP_CAP: i32 = 6;

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Best alignment of `query` in `hay`; `None` when it is not a subsequence.
/// An empty query matches everything with score 0.
pub fn fuzzy(query: &str, hay: &str) -> Option<Match> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).map(lower).collect();
    if q.is_empty() {
        return Some(Match { score: 0, positions: Vec::new() });
    }
    let h: Vec<char> = hay.chars().collect();
    let hl: Vec<char> = h.iter().map(|c| lower(*c)).collect();
    if q.len() > h.len() {
        return None;
    }
    let n = h.len();
    let m = q.len();
    // bonus for matching at j (independent of the previous match)
    let bonus: Vec<i32> = (0..n)
        .map(|j| {
            if j == 0 {
                START
            } else if is_sep(h[j - 1]) {
                WORD
            } else if h[j].is_uppercase() && h[j - 1].is_lowercase() {
                CASE
            } else {
                0
            }
        })
        .collect();
    const NEG: i32 = i32::MIN / 2;
    // best[i][j]: query[..=i] matched with query[i] at hay[j]
    let mut best = vec![vec![NEG; n]; m];
    let mut from = vec![vec![usize::MAX; n]; m];
    for j in 0..n {
        if hl[j] == q[0] {
            best[0][j] = BASE + bonus[j] - (j as i32 * LEAD_GAP).min(GAP_CAP);
        }
    }
    for i in 1..m {
        for j in i..n {
            if hl[j] != q[i] {
                continue;
            }
            let mut top = NEG;
            let mut arg = usize::MAX;
            for (k, &prev) in best[i - 1].iter().enumerate().take(j).skip(i - 1) {
                if prev == NEG {
                    continue;
                }
                let gap = (j - k - 1) as i32;
                let s = prev + if gap == 0 { RUN } else { -(gap * GAP).min(GAP_CAP) };
                if s > top {
                    top = s;
                    arg = k;
                }
            }
            if arg != usize::MAX {
                best[i][j] = top + BASE + bonus[j];
                from[i][j] = arg;
            }
        }
    }
    let (mut j, score) = (0..n).filter(|&j| best[m - 1][j] != NEG).map(|j| (j, best[m - 1][j])).max_by_key(|&(j, s)| (s, std::cmp::Reverse(j)))?;
    let mut positions = vec![0; m];
    for i in (0..m).rev() {
        positions[i] = j;
        if i > 0 {
            j = from[i][j];
        }
    }
    Some(Match { score, positions })
}

/// Items matching `query`, best first. Ties: the shorter name, then the
/// original order. An empty query keeps the original order.
pub fn rank<T>(query: &str, items: &[T], text: impl Fn(&T) -> &str) -> Vec<(usize, Match)> {
    let mut out: Vec<(usize, Match)> = items.iter().enumerate().filter_map(|(i, it)| fuzzy(query, text(it)).map(|m| (i, m))).collect();
    if !query.trim().is_empty() {
        out.sort_by(|(ia, a), (ib, b)| b.score.cmp(&a.score).then_with(|| text(&items[*ia]).chars().count().cmp(&text(&items[*ib]).chars().count())).then(ia.cmp(ib)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(q: &str, items: &[&str]) -> Vec<String> {
        rank(q, items, |s| s).into_iter().map(|(i, _)| items[i].to_string()).collect()
    }

    const CMDS: &[&str] = &["New tab", "Next tab", "Close tab", "Reopen closed tab", "Rename", "Toggle dual pane", "Toggle hidden files", "Open settings", "Open in new tab", "Pin this folder to the sidebar", "Switch dark / light"];

    #[test]
    fn subsequence_in_order_or_nothing() {
        assert!(fuzzy("tdp", "Toggle dual pane").is_some());
        assert!(fuzzy("pdt", "Toggle dual pane").is_none());
        assert!(fuzzy("xyz", "Rename").is_none());
        assert!(fuzzy("renamee", "Rename").is_none(), "longer than the name");
    }

    #[test]
    fn empty_query_matches_all_in_order() {
        assert_eq!(names("", CMDS), CMDS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(fuzzy("  ", "x").unwrap().score, 0);
    }

    #[test]
    fn case_is_ignored() {
        assert_eq!(fuzzy("NEW", "New tab").unwrap().positions, vec![0, 1, 2]);
        assert_eq!(fuzzy("new", "NEW TAB").unwrap().positions, vec![0, 1, 2]);
    }

    #[test]
    fn prefix_and_word_starts_beat_scattered_matches() {
        let r = names("ren", CMDS);
        assert_eq!(r[0], "Rename", "{r:?}");
        // "ot" is Open-Tab-ish: word starts win over letters buried in words
        let r = names("nt", CMDS);
        assert_eq!(r[0], "New tab", "shorter of the equals first: {r:?}");
        assert!(r.contains(&"Next tab".to_string()));
        let r = names("sdl", CMDS);
        assert_eq!(r[0], "Switch dark / light", "{r:?}");
    }

    #[test]
    fn word_initials_find_the_command() {
        let r = names("tdp", CMDS);
        assert_eq!(r, vec!["Toggle dual pane"], "{r:?}");
        let r = names("opset", CMDS);
        assert_eq!(r[0], "Open settings", "{r:?}");
        let r = names("rct", CMDS);
        assert_eq!(r[0], "Reopen closed tab", "{r:?}");
    }

    #[test]
    fn consecutive_runs_beat_gaps() {
        let run = fuzzy("tab", "New tab").unwrap().score;
        let gap = fuzzy("tab", "Rotate all bookmarks").unwrap().score;
        assert!(run > gap, "{run} vs {gap}");
    }

    #[test]
    fn positions_are_ascending_and_point_at_the_query_chars() {
        for (q, h) in [("tdp", "Toggle dual pane"), ("settings", "Open settings"), ("ctab", "Close tab"), ("pf", "Pin this folder to the sidebar")] {
            let m = fuzzy(q, h).unwrap();
            let chars: Vec<char> = h.chars().collect();
            assert_eq!(m.positions.len(), q.chars().count());
            assert!(m.positions.windows(2).all(|w| w[0] < w[1]), "{m:?}");
            for (qc, &p) in q.chars().zip(&m.positions) {
                assert_eq!(chars[p].to_lowercase().next(), qc.to_lowercase().next());
            }
        }
        // "settings" in "Open settings" is the contiguous word, not scattered
        assert_eq!(fuzzy("settings", "Open settings").unwrap().positions, (5..13).collect::<Vec<_>>());
    }

    #[test]
    fn unicode_does_not_panic_or_misalign() {
        let m = fuzzy("cafe", "Caf\u{e9} cafe").unwrap();
        assert_eq!(m.positions.len(), 4);
        assert!(fuzzy("\u{e9}", "Caf\u{e9}").is_some());
        assert!(fuzzy("ab", "\u{1F600}a\u{1F600}b").is_some());
    }

    #[test]
    fn ties_prefer_the_shorter_name_then_the_earlier_one() {
        let items = ["Open tab list", "Open tab", "Open tab"];
        let r = rank("opentab", &items, |s| s);
        assert_eq!(r.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1, 2, 0]);
    }

    #[test]
    fn ranking_thousands_of_names_is_cheap() {
        let items: Vec<String> = (0..2000).map(|i| format!("Command number {i} does something useful")).collect();
        let t = std::time::Instant::now();
        let r = rank("cmdsu", &items, |s| s.as_str());
        assert_eq!(r.len(), 2000);
        assert!(t.elapsed() < std::time::Duration::from_millis(400), "{:?}", t.elapsed());
    }
}
