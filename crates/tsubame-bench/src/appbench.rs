//! `app`: turn one rig benchmark run's raw reports into a `docs/bench-log.md`
//! section.
//!
//! The app reports are the ones `src/bench.rs`, `src/timing.rs` and
//! `src/leak.rs` write next to the exe (`s1-bench.txt`, `u2-timing.txt`,
//! `leak-gate.txt`); `tools/bench-rig.ps1` also writes `meta.txt` and
//! `idle.txt`. This module parses them, checks the numbers against the
//! budgets in `docs/PLAN.md` section 2 and renders the markdown table. It is
//! pure: no disk, no clock, no window, so the parsing and the threshold logic
//! are unit-tested.
//!
//! ```text
//! tsubame-bench app --reports DIR [--append docs/bench-log.md] [--commit SHA]
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::stats::Series;

// ---- budgets (docs/PLAN.md section 2) --------------------------------------

/// Cold start to first painted frame.
pub const BUDGET_COLD_START_MS: f64 = 100.0;
/// 100,000-entry folder: first rows on screen.
pub const BUDGET_FIRST_ROWS_MS: f64 = 30.0;
/// 100,000-entry folder: complete and sorted on screen.
pub const BUDGET_SORTED_MS: f64 = 200.0;
/// Scroll paint p95, 4K, details view.
pub const BUDGET_SCROLL_P95_MS: f64 = 5.0;
/// Idle CPU. The budget is "0 wake-ups"; the harness measures CPU time over
/// `idle.username_s`, and this much of it still counts as idle.
pub const BUDGET_IDLE_CPU_MS: f64 = 50.0;
/// Private bytes, one tab, small folder, 4K.
pub const BUDGET_PRIVATE_4K_MB: f64 = 110.0;
/// Main exe.
pub const BUDGET_EXE_MB: f64 = 6.0;
/// Leak gate: private bytes return to within this percent of the start...
pub const BUDGET_LEAK_PCT: f64 = 5.0;
/// ...ignoring growth below this floor (the gate itself uses the same two).
pub const LEAK_FLOOR_BYTES: i64 = 2 << 20;

/// The log's header, written when the file does not exist yet.
pub const LOG_HEADER: &str = "\
# Tsubame bench log

One section per `tools/bench-rig.ps1` run on rig, newest last. Budgets and the
leak gate are `docs/PLAN.md` section 2; the harness itself is described in
`docs/bench.md`. Every number here comes from the release app on rig: real
window, real enumerator, real shell icon workers. Warm filesystem cache unless
the section says otherwise.

Rerun (from a worktree; needs the rig skill's ssh):

```
pwsh X:\\Dev\\tools\\bench-rig.ps1 -Project <worktree>
```

The script builds on rig with `push-to-rig.ps1`, generates the fixtures under
`G:\\rig-bench\\tsubame-b1`, runs the benches and appends a section here.
";

// ---- key=value files -------------------------------------------------------

/// `key=value` pairs of a report file, in file order.
pub fn parse_kv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let k = k.trim();
            if k.is_empty() || k.contains(' ') {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

pub fn kv<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

pub fn kv_f64(pairs: &[(String, String)], key: &str) -> Option<f64> {
    kv(pairs, key).and_then(|v| v.parse().ok())
}

pub fn kv_usize(pairs: &[(String, String)], key: &str) -> Option<usize> {
    kv(pairs, key).and_then(|v| v.parse().ok())
}

pub fn kv_i64(pairs: &[(String, String)], key: &str) -> Option<i64> {
    kv(pairs, key).and_then(|v| v.parse().ok())
}

fn mb(bytes: f64) -> f64 {
    bytes / 1048576.0
}

/// The first number after `prefix` (used for `p50=` / `p95=` in the bench stats).
fn after<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let i = text.find(prefix)? + prefix.len();
    let rest = &text[i..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// A number that follows `label` in a line of prose (`label: 72.2`).
fn number_after(text: &str, label: &str) -> Option<f64> {
    let i = text.find(label)? + label.len();
    let rest = &text[i..];
    let start = rest.find(|c: char| c.is_ascii_digit() || c == '-')?;
    let rest = &rest[start..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ---- app --bench (s1-bench.txt) -------------------------------------------

/// One `--bench` run: first paint, scroll paint, memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColdRun {
    pub first_paint_ms: f64,
    pub paint_p50_ms: f64,
    pub paint_p95_ms: f64,
    pub private_before_mb: f64,
    pub private_after_mb: f64,
    pub peak_commit_mb: f64,
    pub rows: usize,
    pub exe_bytes: u64,
    pub paint_path: String,
}

pub fn parse_s1(text: &str) -> Result<ColdRun, String> {
    let mut r = ColdRun::default();
    for line in text.lines() {
        if let Some(v) = number_after(line, "first_paint_ms") {
            r.first_paint_ms = v;
        } else if line.starts_with("paint_all:") {
            r.paint_p50_ms = after(line, "p50=").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            r.paint_p95_ms = after(line, "p95=").and_then(|v| v.parse().ok()).unwrap_or(0.0);
        } else if line.starts_with("private_bytes_before_scroll") {
            r.private_before_mb = number_after(line, ":").map(mb).unwrap_or(0.0);
        } else if line.starts_with("private_bytes_end") {
            r.private_after_mb = number_after(line, ":").map(mb).unwrap_or(0.0);
        } else if line.starts_with("peak_commit") {
            r.peak_commit_mb = number_after(line, ":").map(mb).unwrap_or(0.0);
        } else if line.starts_with("rows:") {
            r.rows = number_after(line, ":").unwrap_or(0.0) as usize;
        } else if line.starts_with("exe_size_bytes") {
            r.exe_bytes = number_after(line, ":").unwrap_or(0.0) as u64;
        } else if line.starts_with("scroll_path:") {
            r.paint_path = line.split_once(':').map(|(_, v)| v.trim().to_string()).unwrap_or_default();
        }
    }
    if r.first_paint_ms <= 0.0 {
        return Err("no first_paint_ms in the --bench report".into());
    }
    Ok(r)
}

// ---- app --timing (u2-timing.txt) ----------------------------------------

/// Navigation times from `--timing`: all values in ms, measured from the
/// navigation event (the startup line also includes window creation).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timing {
    pub first_paint_ms: f64,
    pub private_first_paint_mb: f64,
    pub private_end_mb: f64,
    pub window: String,
    pub paint_path: String,
    pub entries: usize,
    /// first rows on screen, one entry per measured navigation
    pub first_rows: Vec<f64>,
    /// complete and sorted on screen
    pub sorted: Vec<f64>,
    /// store complete and sorted (no frame involved)
    pub done: Vec<f64>,
    pub first_batch: Vec<f64>,
    /// the startup navigation (includes window creation and the first GPU frame)
    pub startup_first_rows: Option<f64>,
    pub startup_sorted: Option<f64>,
}

/// One row of the timing table:
/// `first    entries  100000 (11.4 MB)  first_batch 1.23  first_frame 2.34 ...`
#[derive(Debug, Clone, PartialEq)]
pub struct TimingRow {
    pub label: String,
    pub first_batch: Option<f64>,
    pub first_frame: Option<f64>,
    pub done: Option<f64>,
    pub final_frame: Option<f64>,
    pub worker: Option<f64>,
}

fn parse_time(s: &str) -> Option<f64> {
    s.parse().ok()
}

pub fn parse_timing_row(line: &str) -> Option<TimingRow> {
    let t: Vec<&str> = line.split_whitespace().collect();
    if t.len() < 15 || !t.contains(&"first_frame") {
        return None;
    }
    let shift = usize::from(t[0] == "repeat");
    let label = if shift == 1 { format!("{} {}", t[0], t[1]) } else { t[0].to_string() };
    let field = |name: &str| {
        let i = t.iter().position(|&x| x == name)?;
        t.get(i + 1).and_then(|v| parse_time(v))
    };
    Some(TimingRow {
        label,
        first_batch: field("first_batch"),
        first_frame: field("first_frame"),
        done: field("done"),
        final_frame: field("final_frame"),
        worker: field("worker"),
    })
}

pub fn parse_timing(text: &str) -> Result<Timing, String> {
    let mut r = Timing::default();
    for line in text.lines() {
        if let Some(v) = number_after(line, "first_paint_ms (process start to first completed paint):") {
            r.first_paint_ms = v;
        } else if let Some(v) = number_after(line, "private_bytes_first_paint:") {
            r.private_first_paint_mb = v;
        } else if let Some(v) = number_after(line, "private_bytes_end:") {
            r.private_end_mb = v;
        } else if let Some(rest) = line.strip_prefix("window_px: ") {
            let (w, rest) = rest.split_once(", paint path ").unwrap_or((rest, ""));
            r.window = w.trim().to_string();
            r.paint_path = rest.trim().to_string();
        } else if let Some(row) = parse_timing_row(line) {
            if row.label == "startup" {
                r.startup_first_rows = row.first_frame;
                r.startup_sorted = row.final_frame;
            } else {
                if let Some(v) = row.first_frame {
                    r.first_rows.push(v);
                }
                if let Some(v) = row.final_frame {
                    r.sorted.push(v);
                }
                if let Some(v) = row.done {
                    r.done.push(v);
                }
                if let Some(v) = row.first_batch {
                    r.first_batch.push(v);
                }
            }
            // `entries  100000`: the folder's size, on every row
            if r.entries == 0
                && let Some(i) = line.split_whitespace().position(|t| t == "entries")
                && let Some(n) = line.split_whitespace().nth(i + 1).and_then(|v| v.parse().ok())
            {
                r.entries = n;
            }
        }
    }
    if r.first_rows.is_empty() && r.startup_first_rows.is_none() {
        return Err("no navigation rows in the --timing report".into());
    }
    Ok(r)
}

// ---- app --leak-gate (leak-gate.txt) -------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GateSample {
    pub ops: usize,
    pub private_mb: f64,
    pub working_set_mb: f64,
    pub handles: u32,
    pub gdi: u32,
    pub user: u32,
    pub threads: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GateReport {
    pub pairs: Vec<(String, String)>,
    pub window: String,
    pub fixture: String,
    pub folders: usize,
    pub navigations: usize,
    pub tabs: usize,
    pub warmup_navs: usize,
    pub warmup_tabs: usize,
    pub sample_interval: usize,
    pub settle_s: f64,
    pub elapsed_s: f64,
    pub idle_private_mb: f64,
    pub delta_from_idle_mb: f64,
    /// one-tab, small-folder idle window before the warm-up (the memory budget's
    /// scenario)
    pub idle: GateSample,
    pub start: GateSample,
    pub end: GateSample,
    pub samples: Vec<GateSample>,
    pub delta_private_mb: f64,
    pub delta_private_pct: f64,
    pub allow_private_mb: f64,
    pub peak_private_mb: f64,
    pub trim_private_mb: Option<f64>,
    pub trim_note: String,
    pub delta_handles: i64,
    pub delta_gdi: i64,
    pub delta_user: i64,
    pub delta_threads: i64,
    pub pass: bool,
    pub fail: String,
    pub notes: Vec<String>,
}

/// The app reports file sizes and counters as plain byte counts with a
/// human-readable suffix; the log wants MB.
fn sample_from(pairs: &[(String, String)], prefix: &str) -> GateSample {
    let get = |suffix: &str| kv_f64(pairs, &format!("{prefix}.{suffix}"));
    GateSample {
        ops: 0,
        private_mb: get("private_mb").unwrap_or_default(),
        working_set_mb: get("working_set_bytes").map(mb).unwrap_or_default(),
        handles: get("handles").unwrap_or_default() as u32,
        gdi: get("gdi").unwrap_or_default() as u32,
        user: get("user").unwrap_or_default() as u32,
        threads: get("threads").unwrap_or_default() as u32,
    }
}

pub fn parse_gate(text: &str) -> Result<GateReport, String> {
    let pairs = parse_kv(text);
    let mut r = GateReport { pairs: pairs.clone(), ..Default::default() };
    r.window = kv(&pairs, "window_px").unwrap_or("").to_string();
    r.fixture = kv(&pairs, "fixture").unwrap_or("").to_string();
    r.folders = kv_usize(&pairs, "folders").unwrap_or(0);
    r.navigations = kv_usize(&pairs, "navigations").unwrap_or(0);
    r.tabs = kv_usize(&pairs, "tabs").unwrap_or(0);
    r.warmup_navs = kv_usize(&pairs, "warmup_navigations").unwrap_or(0);
    r.warmup_tabs = kv_usize(&pairs, "warmup_tabs").unwrap_or(0);
    r.sample_interval = kv_usize(&pairs, "sample_interval").unwrap_or(0);
    r.settle_s = kv_f64(&pairs, "settle_s").unwrap_or(0.0);
    r.elapsed_s = kv_f64(&pairs, "elapsed_s").unwrap_or(0.0);
    r.idle_private_mb = kv_f64(&pairs, "idle.private_mb").unwrap_or(0.0);
    r.delta_from_idle_mb = mb(kv_f64(&pairs, "delta.private_bytes_from_idle").unwrap_or(0.0));
    r.start = sample_from(&pairs, "start");
    r.idle = sample_from(&pairs, "idle");
    r.end = sample_from(&pairs, "end");
    r.delta_private_mb = mb(kv_f64(&pairs, "delta.private_bytes").unwrap_or(0.0));
    r.delta_private_pct = kv_f64(&pairs, "delta.private_pct").unwrap_or(0.0);
    r.allow_private_mb = mb(kv_f64(&pairs, "allow.private_bytes").unwrap_or(0.0));
    r.peak_private_mb = mb(kv_f64(&pairs, "peak.private_bytes").unwrap_or(0.0));
    r.delta_handles = kv_i64(&pairs, "delta.handles").unwrap_or(0);
    r.delta_gdi = kv_i64(&pairs, "delta.gdi").unwrap_or(0);
    r.delta_user = kv_i64(&pairs, "delta.user").unwrap_or(0);
    r.delta_threads = kv_i64(&pairs, "delta.threads").unwrap_or(0);
    r.pass = kv(&pairs, "gate") == Some("PASS");
    r.fail = kv(&pairs, "fail").unwrap_or("-").to_string();
    r.notes = pairs.iter().filter(|(k, _)| k == "note").map(|(_, v)| v.clone()).collect();
    r.trim_private_mb = kv_f64(&pairs, "trim.private_bytes").map(mb);
    r.trim_note = kv(&pairs, "trim.note").unwrap_or("").to_string();
    // sample.<ops>.<field>
    let mut by_ops: BTreeMap<usize, GateSample> = BTreeMap::new();
    for (k, v) in &pairs {
        let Some(rest) = k.strip_prefix("sample.") else { continue };
        let Some((ops, field)) = rest.split_once('.') else { continue };
        let Ok(ops) = ops.parse::<usize>() else { continue };
        let s = by_ops.entry(ops).or_insert_with(|| GateSample { ops, ..Default::default() });
        let n: f64 = v.parse().unwrap_or(0.0);
        match field {
            "private_mb" => s.private_mb = n,
            "working_set_bytes" => s.working_set_mb = mb(n),
            "handles" => s.handles = n as u32,
            "gdi" => s.gdi = n as u32,
            "user" => s.user = n as u32,
            "threads" => s.threads = n as u32,
            _ => {}
        }
    }
    r.samples = by_ops.into_values().collect();
    if r.pairs.is_empty() {
        return Err("empty leak-gate report".into());
    }
    if kv(&r.pairs, "gate").is_none() {
        return Err("leak-gate report has no gate= line (a run that did not finish?)".into());
    }
    Ok(r)
}

// ---- thresholds ------------------------------------------------------------

/// How a metric ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    /// The measurement needs something the run did not have (elevation for a
    /// cold cache, for example). Never rendered as a pass.
    NotMeasured,
}

impl Outcome {
    pub fn mark(self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::Fail => "**FAIL**",
            Outcome::NotMeasured => "n/a",
        }
    }
}

/// One table row: metric, budget and how it ended up.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub metric: String,
    pub budget: String,
    pub measured: String,
    pub outcome: Outcome,
}

impl Row {
    fn judged(metric: &str, budget: &str, measured: String, pass: bool) -> Row {
        Row { metric: metric.to_string(), budget: budget.to_string(), measured, outcome: if pass { Outcome::Pass } else { Outcome::Fail } }
    }

    fn not_measured(metric: &str, budget: &str, measured: String) -> Row {
        Row { metric: metric.to_string(), budget: budget.to_string(), measured, outcome: Outcome::NotMeasured }
    }
}

/// The leak gate's own verdict, re-derived from the raw counters: the two
/// rules are the percentage and the absolute floor (2 MB), so a tiny baseline
/// cannot fail on allocator noise alone.
pub fn leak_verdict(start_bytes: f64, end_bytes: f64) -> (bool, f64, f64) {
    let delta = end_bytes - start_bytes;
    let allow = (start_bytes * BUDGET_LEAK_PCT / 100.0).max(LEAK_FLOOR_BYTES as f64);
    (delta <= allow, delta, allow)
}

fn ms_cell(s: &Series) -> String {
    format!("{:.2} ms median, {:.2} ms p95 (n={})", s.median(), s.p95(), s.values.len())
}

/// Everything the budget table judges.
pub struct Budgets<'a> {
    pub cold_start: &'a Series,
    pub scroll_p95: &'a Series,
    /// private bytes at the end of the scroll run
    pub open_after: &'a Series,
    pub timing: &'a Timing,
    /// the cold-cache run, when the purge was permitted
    pub cold_timing: Option<&'a Timing>,
    /// why the cold-cache run is missing, or what was purged
    pub cold_note: &'a str,
    pub idle_cpu_ms: f64,
    pub exe_bytes: u64,
    pub gate: &'a GateReport,
}

/// All metric rows, from the parsed reports.
pub fn verdicts(b: &Budgets) -> Vec<Row> {
    let first_rows = timing_series(&b.timing.first_rows);
    let sorted = timing_series(&b.timing.sorted);
    let mut rows = Vec::new();
    rows.push(Row::judged("Cold start to first painted frame", &format!("< {BUDGET_COLD_START_MS:.0} ms"), ms_cell(b.cold_start), b.cold_start.median() < BUDGET_COLD_START_MS));
    rows.push(Row::judged("100,000-entry folder: first rows", &format!("< {BUDGET_FIRST_ROWS_MS:.0} ms"), ms_cell(&first_rows), first_rows.median() < BUDGET_FIRST_ROWS_MS));
    rows.push(Row::judged("100,000-entry folder: complete and sorted", &format!("< {BUDGET_SORTED_MS:.0} ms"), ms_cell(&sorted), sorted.median() < BUDGET_SORTED_MS));
    match b.cold_timing {
        Some(t) if !t.first_rows.is_empty() || !t.sorted.is_empty() => {
            let c_first = timing_series(&t.first_rows);
            let c_sorted = timing_series(&t.sorted);
            rows.push(Row::judged(
                "100,000-entry folder, cold cache: first rows",
                &format!("< {BUDGET_FIRST_ROWS_MS:.0} ms"),
                ms_cell(&c_first),
                c_first.median() < BUDGET_FIRST_ROWS_MS,
            ));
            rows.push(Row::judged(
                "100,000-entry folder, cold cache: complete and sorted",
                &format!("< {BUDGET_SORTED_MS:.0} ms"),
                ms_cell(&c_sorted),
                c_sorted.median() < BUDGET_SORTED_MS,
            ));
        }
        _ => rows.push(Row::not_measured(
            "100,000-entry folder, cold cache",
            &format!("< {BUDGET_FIRST_ROWS_MS:.0} / < {BUDGET_SORTED_MS:.0} ms"),
            format!("not measured ({})", b.cold_note),
        )),
    }
    rows.push(Row::judged("Scroll paint p95, 4K, details view", &format!("< {BUDGET_SCROLL_P95_MS:.1} ms"), ms_cell(b.scroll_p95), b.scroll_p95.median() < BUDGET_SCROLL_P95_MS));
    rows.push(Row::judged("Idle CPU over the idle window", "0 wake-ups (0 ms CPU)", format!("{:.0} ms", b.idle_cpu_ms), b.idle_cpu_ms < BUDGET_IDLE_CPU_MS));
    // the gate's start sample is the budget's steady state exactly: one tab,
    // a small folder, 4K, GPU target up, after a settle
    let small_folder = b.gate.idle.private_mb;
    rows.push(Row::judged(
        "Private bytes, one tab, small folder, 4K",
        &format!("< {BUDGET_PRIVATE_4K_MB:.0} MB"),
        format!("{small_folder:.1} MB (idle sample before the gate's warm-up)"),
        small_folder < BUDGET_PRIVATE_4K_MB,
    ));
    let after_scroll = b.open_after.median();
    rows.push(Row::judged(
        "Private bytes after the 5 s scroll run (100k folder)",
        &format!("< {BUDGET_PRIVATE_4K_MB:.0} MB"),
        format!("{after_scroll:.1} MB median ({:.1} MB max)", b.open_after.max()),
        after_scroll < BUDGET_PRIVATE_4K_MB,
    ));
    rows.push(Row::judged("Main exe", &format!("< {BUDGET_EXE_MB:.0} MB"), format!("{:.0} KB", b.exe_bytes as f64 / 1024.0), (b.exe_bytes as f64) / 1048576.0 < BUDGET_EXE_MB));
    let start_bytes = b.gate.start.private_mb * 1048576.0;
    let (pass, delta, allow) = leak_verdict(start_bytes, b.gate.end.private_mb * 1048576.0);
    rows.push(Row::judged(
        "Leak gate: 10,000 navigations + 1,000 tabs",
        &format!("private bytes within {BUDGET_LEAK_PCT:.0}% of the start"),
        format!(
            "{:.1} -> {:.1} MB ({:+.2} MB, {:.1}%; allowance {:.1} MB)",
            b.gate.start.private_mb,
            b.gate.end.private_mb,
            mb(delta),
            delta / start_bytes.max(1.0) * 100.0,
            mb(allow)
        ),
        pass && b.gate.pass,
    ));
    rows
}

pub fn timing_series(v: &[f64]) -> Series {
    Series { values: v.to_vec() }
}

// ---- markdown --------------------------------------------------------------

fn table(out: &mut String, rows: &[Row]) {
    let _ = writeln!(out, "| Metric | Budget | Measured | |");
    let _ = writeln!(out, "|---|---|---|---|");
    for r in rows {
        let _ = writeln!(out, "| {} | {} | {} | {} |", r.metric, r.budget, r.measured, r.outcome.mark());
    }
}

fn gate_table(out: &mut String, g: &GateReport) {
    let _ = writeln!(out, "| Operation | Private MB | Working set MB | Handles | GDI | USER | Threads |");
    let _ = writeln!(out, "|---|---|---|---|---|---|---|");
    let line = |out: &mut String, label: &str, s: &GateSample| {
        let _ = writeln!(out, "| {label} | {:.1} | {:.1} | {} | {} | {} | {} |", s.private_mb, s.working_set_mb, s.handles, s.gdi, s.user, s.threads);
    };
    line(out, "start", &g.start);
    for s in &g.samples {
        line(out, &format!("{} ops", s.ops), s);
    }
    line(out, "after settle", &g.end);
    if let Some(t) = g.trim_private_mb {
        let _ = writeln!(out, "| after icon-cache trim | {t:.1} | | | | | |");
    }
}

/// Raw key=value blocks, collapsed in a `<details>` element.
fn raw(out: &mut String, title: &str, body: &str) {
    let _ = writeln!(out);
    let _ = writeln!(out, "<details><summary>{title}</summary>");
    let _ = writeln!(out);
    let _ = writeln!(out, "```");
    let _ = writeln!(out, "{}", body.trim_end());
    let _ = writeln!(out, "```");
    let _ = writeln!(out, "</details>");
}

/// Everything one run reported.
pub struct Run {
    pub meta: Vec<(String, String)>,
    pub idle: Vec<(String, String)>,
    pub cold: Vec<ColdRun>,
    pub timing: Timing,
    /// the cold-cache navigation, if a purge was permitted
    pub cold_timing: Option<Timing>,
    /// one entry per `--leak-gate` process (the harness runs it several times:
    /// the verdict is the worst run, the spread shows the noise)
    pub gates: Vec<GateReport>,
    pub raw_timing: String,
    pub raw_gate: String,
    pub raw_idle: String,
    pub raw_cold_timing: String,
}

impl Run {
    fn meta(&self, key: &str) -> &str {
        kv(&self.meta, key).unwrap_or("-")
    }

    /// The run the gate judges: the largest private-bytes growth.
    fn gate(&self) -> &GateReport {
        self.gates.iter().max_by(|a, b| a.delta_private_mb.total_cmp(&b.delta_private_mb)).expect("at least one gate report")
    }
}

/// The markdown section for one run: heading, machine line, table, leak-gate
/// evidence, raw output.
pub fn section(run: &Run, commit: &str) -> String {
    let stamp = run.meta("stamp").to_string();
    let date = stamp.split('T').next().unwrap_or(&stamp).to_string();
    let time = stamp.split('T').nth(1).map(|t| t.chars().take(5).collect::<String>()).unwrap_or_default();
    // the worst gate run decides; the others are shown for the spread
    let gate = run.gate();
    let cold: Series = Series { values: run.cold.iter().map(|c| c.first_paint_ms).collect() };
    let paint_p95: Series = Series { values: run.cold.iter().map(|c| c.paint_p95_ms).collect() };
    let after: Series = Series { values: run.cold.iter().map(|c| c.private_after_mb).collect() };
    let exe_bytes = run.cold.iter().map(|c| c.exe_bytes).max().unwrap_or(0);
    let idle_cpu = kv_f64(&run.idle, "idle.cpu_ms").unwrap_or(0.0);
    let idle_cpu_first = kv_f64(&run.idle, "idle.cpu_ms_first").unwrap_or(0.0);
    let cold_note = run.meta("cold");
    let budgets = Budgets {
        cold_start: &cold,
        scroll_p95: &paint_p95,
        open_after: &after,
        timing: &run.timing,
        cold_timing: run.cold_timing.as_ref(),
        cold_note,
        idle_cpu_ms: idle_cpu,
        exe_bytes,
        gate,
    };
    let rows = verdicts(&budgets);
    let failed: Vec<&str> = rows.iter().filter(|r| r.outcome == Outcome::Fail).map(|r| r.metric.as_str()).collect();
    let skipped: Vec<&str> = rows.iter().filter(|r| r.outcome == Outcome::NotMeasured).map(|r| r.metric.as_str()).collect();

    let mut out = String::new();
    let _ = writeln!(out, "## {date} {time} — {}, commit `{commit}`", run.meta("machine"));
    let _ = writeln!(out);
    let _ = writeln!(out, "- Mode: {} ({} cold-start runs, {} scroll runs)", run.meta("mode"), run.cold.len(), run.cold.len());
    let _ = writeln!(out, "- Build: {} on rig, branch `{}`, commit `{commit}`{}", run.meta("build"), run.meta("branch"), if run.meta("dirty") == "1" { " (dirty worktree)" } else { "" });
    let _ = writeln!(out, "- Window: {}, paint path `{}`", run.timing.window, run.timing.paint_path);
    let _ = writeln!(out, "- Fixtures: `{}` (100,000 entries), `{}` ({} folders, mixed sizes)", run.meta("fixture_100k"), run.meta("gate_fixture"), gate.folders);
    let _ = writeln!(out, "- Cache: {}; rig load: {}", run.meta("cache"), run.meta("rig_load"));
    let _ = writeln!(out, "- Cold-cache 100k open: {}", run.meta("cold"));
    if let Some(t) = &run.cold_timing {
        let _ = writeln!(
            out,
            "  The purge also emptied the app's own pages: that process painted its first frame {:.0} ms after start (warm: {:.0} ms), so the cold row includes loading the app itself, not only the folder's metadata.",
            t.first_paint_ms, run.timing.first_paint_ms
        );
    }
    let _ = writeln!(
        out,
        "- Idle: {:.0} ms of CPU over the first idle window, {:.0} ms over the second (the table uses the second); thread context switches: {}",
        idle_cpu_first,
        idle_cpu,
        match kv_f64(&run.idle, "idle.context_switches_per_s") {
            Some(v) if v >= 0.0 => format!("{v:.0}/s (one Get-Counter sample)"),
            _ => "not measured (Get-Counter has no \\Thread instances for this process)".to_string(),
        }
    );
    let _ = writeln!(
        out,
        "- Memory context: {} MB private at the first painted frame, {} MB at the end of the warm navigation run (a 100k store plus the DirectWrite layout cache is live then; the budget row below uses the gate's steady state).",
        format_args!("{:.1}", run.timing.private_first_paint_mb),
        format_args!("{:.1}", run.timing.private_end_mb)
    );
    if !gate.notes.is_empty() {
        let _ = writeln!(out, "- Gate notes: {}", gate.notes.join("; "));
    }
    let _ = writeln!(out);
    table(&mut out, &rows);
    let _ = writeln!(out);
    if failed.is_empty() {
        let _ = writeln!(out, "All measured budgets and the leak gate pass.");
    } else {
        let _ = writeln!(out, "**{} budget(s) missed: {}.** Reported as measured; nothing was relaxed to make them pass.", failed.len(), failed.join(", "));
    }
    if !skipped.is_empty() {
        let _ = writeln!(out, "Not measured: {} ({}).", skipped.join(", "), run.meta("cold"));
    }
    if let Some(v) = run.timing.startup_first_rows {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Informational: startup navigation first rows {:.2} ms and complete+sorted {:.2} ms from tab creation (window creation and the first GPU frame included; the budgets above exclude startup).",
            v,
            run.timing.startup_sorted.unwrap_or(0.0)
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "### Leak gate evidence");
    let _ = writeln!(out);
    if run.gates.len() > 1 {
        let _ = writeln!(out, "The gate ran {} times in fresh processes; the worst run decides (no run may exceed the allowance).", run.gates.len());
        let _ = writeln!(out);
        let _ = writeln!(out, "| Run | Private MB start -> end | Delta MB | Delta % | Allowance MB | Wall s | |");
        let _ = writeln!(out, "|---|---|---|---|---|---|---|");
        for (i, g) in run.gates.iter().enumerate() {
            let _ = writeln!(
                out,
                "| {} | {:.1} -> {:.1} | {:+.2} | {:+.2} | {:.2} | {:.0} | {} |",
                i + 1,
                g.start.private_mb,
                g.end.private_mb,
                g.delta_private_mb,
                g.delta_private_pct,
                g.allow_private_mb,
                g.elapsed_s,
                if g.pass { "PASS" } else { "**FAIL**" }
            );
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(
        out,
        "{} navigations over {} folders and {} tab open/close (with a scroll) in one process, {} samples every {} operations, {} s settle. Gate: **{}**{}",
        gate.navigations,
        gate.folders,
        gate.tabs,
        gate.samples.len(),
        gate.sample_interval,
        gate.settle_s,
        if gate.pass { "PASS" } else { "FAIL" },
        if gate.fail == "-" { String::new() } else { format!(" (failed: {})", gate.fail) }
    );
    let _ = writeln!(
        out,
        "The start sample is the steady state: {} warm-up navigations and {} warm-up tabs run first, then the window shows the fixture's first folder for {} s. From the idle one-tab sample ({:.1} MB private, before the warm-up) to the end the process grew {:.1} MB; the gate judges the warm-to-warm delta, because the bounded icon and text-layout caches fill on first use.",
        gate.warmup_navs, gate.warmup_tabs, gate.settle_s, gate.idle_private_mb, gate.delta_from_idle_mb
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Worst run ({}):", if run.gates.len() > 1 { "the one above" } else { "single run" });
    let _ = writeln!(out);
    gate_table(&mut out, gate);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Private bytes {:+.2} MB ({:+.2}%, allowance {:.2} MB), handles {:+#}, GDI {:+#}, USER {:+#}, threads {:+#}. Peak private bytes {:.1} MB.",
        gate.delta_private_mb, gate.delta_private_pct, gate.allow_private_mb, gate.delta_handles, gate.delta_gdi, gate.delta_user, gate.delta_threads, gate.peak_private_mb
    );
    if let Some(t) = gate.trim_private_mb {
        let _ = writeln!(out, "Forced collection: {}; private bytes after it {t:.1} MB.", gate.trim_note);
    }
    let _ = writeln!(out, "Gate wall time {:.0} s.", gate.elapsed_s);

    let s1: Vec<String> = run
        .cold
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "run {}: first_paint_ms {:.2} paint_p50_ms {:.2} paint_p95_ms {:.2} private_before_scroll_mb {:.1} private_end_mb {:.1} peak_commit_mb {:.1} rows {} exe_bytes {}",
                i + 1,
                c.first_paint_ms,
                c.paint_p50_ms,
                c.paint_p95_ms,
                c.private_before_mb,
                c.private_after_mb,
                c.peak_commit_mb,
                c.rows,
                c.exe_bytes
            )
        })
        .collect();
    raw(&mut out, "Raw: meta.txt", &run.meta.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n"));
    raw(&mut out, "Raw: s1-bench.txt (one --bench run per line)", &s1.join("\n"));
    raw(&mut out, "Raw: u2-timing.txt", &run.raw_timing);
    if !run.raw_cold_timing.is_empty() {
        raw(&mut out, "Raw: cold-cache u2-timing.txt", &run.raw_cold_timing);
    }
    raw(&mut out, "Raw: idle.txt", &run.raw_idle);
    raw(&mut out, "Raw: leak-gate.txt", &run.raw_gate);
    out
}

// ---- the `app` subcommand --------------------------------------------------

/// The gate reports in `dir`: `leak-gate-01.txt`, `leak-gate-02.txt`, ... from
/// `-GateRuns N`, or a single `leak-gate.txt`. Returns the raw text of all of
/// them (for the log) and fills `gates`.
fn read_gates(dir: &Path, gates: &mut Vec<GateReport>) -> Result<String, String> {
    let mut raw = String::new();
    for i in 1..=9 {
        let name = format!("leak-gate-{i:02}.txt");
        let Ok(text) = std::fs::read_to_string(dir.join(&name)) else { break };
        let g = parse_gate(&text).map_err(|e| format!("{name}: {e}"))?;
        raw.push_str(&format!("# --- {name} ---\n{text}\n"));
        gates.push(g);
    }
    if gates.is_empty() {
        let text = std::fs::read_to_string(dir.join("leak-gate.txt")).map_err(|e| format!("{}\\leak-gate.txt: {e}", dir.display()))?;
        let g = parse_gate(&text)?;
        raw = text;
        gates.push(g);
    }
    Ok(raw)
}

/// Reads `meta.txt`, `idle.txt`, `cold-start-*.txt`, `timing.txt` and
/// `leak-gate*.txt` from `dir` and renders the section.
pub fn run(dir: &Path, commit: &str, append: Option<&Path>) -> Result<String, String> {
    let read = |name: &str| -> Result<String, String> {
        std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{}\\{name}: {e}", dir.display()))
    };
    let meta = parse_kv(&read("meta.txt")?);
    let raw_idle = read("idle.txt")?;
    let idle = parse_kv(&raw_idle);
    let raw_timing = read("timing.txt")?;
    let timing = parse_timing(&raw_timing)?;
    let (cold_timing, raw_cold_timing) = match std::fs::read_to_string(dir.join("cold-timing.txt")) {
        Ok(text) => match parse_timing(&text) {
            Ok(t) => (Some(t), text),
            Err(e) => {
                eprintln!("tsubame-bench app: cold-timing.txt ignored: {e}");
                (None, String::new())
            }
        },
        Err(_) => (None, String::new()),
    };
    let mut gates = Vec::new();
    let raw_gate = read_gates(dir, &mut gates)?;
    let mut cold = Vec::new();
    for i in 1..=99 {
        let name = format!("cold-start-{i:02}.txt");
        match std::fs::read_to_string(dir.join(&name)) {
            Ok(text) => cold.push(parse_s1(&text)?),
            Err(_) => break,
        }
    }
    if cold.is_empty() {
        return Err(format!("{}: no cold-start-NN.txt reports", dir.display()));
    }
    let run = Run { meta, idle, cold, timing, cold_timing, gates, raw_timing, raw_gate, raw_idle, raw_cold_timing };
    let commit = if commit.is_empty() { kv(&run.meta, "commit").unwrap_or("unknown") } else { commit }.to_string();
    let text = section(&run, &commit);
    if let Some(path) = append {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        let mut all = String::new();
        if existing.trim().is_empty() {
            all.push_str(LOG_HEADER);
            all.push('\n');
        } else {
            all.push_str(&existing);
            if !existing.ends_with("\n\n") {
                all.push('\n');
            }
        }
        all.push_str(&text);
        std::fs::write(path, all).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S1: &str = "\
tsubame S1/S4 bench
rows: 100000
build: release
data_source: real folder (enumerator)
window_px: 3840x2064 at 192 dpi
first_paint_path: software
first_paint_ms: 70.5
gpu_target_ready_ms_since_start: 290.0
scroll_path: hardware
paint_all: n=320 p50=2.60 p95=3.75 p99=4.10 max=4.90 ms
paint_phase_A steady fling 2000 DIP/s: n=120 p50=2.60 p95=2.80 p99=3.00 max=3.20 ms
private_bytes_first_paint: 73000000 (69.6 MB)
private_bytes_before_scroll (after GPU swap, idle): 101400000 (96.7 MB)
private_bytes_end: 156800000 (149.5 MB)
working_set_end: 170000000 (162.1 MB)
peak_commit: 200000000 (190.7 MB)
exe_size_bytes: 601600 (0.57 MB)
";

    #[test]
    fn parses_a_bench_report() {
        let r = parse_s1(S1).expect("parse");
        assert_eq!(r.first_paint_ms, 70.5);
        assert_eq!(r.paint_p95_ms, 3.75);
        assert!((r.private_before_mb - 101_400_000.0 / 1048576.0).abs() < 0.01);
        assert!((r.private_after_mb - 149.53).abs() < 0.1);
        assert_eq!(r.rows, 100_000);
        assert_eq!(r.exe_bytes, 601_600);
        assert_eq!(r.paint_path, "hardware");
        assert!(parse_s1("nothing here").is_err());
    }

    const TIMING: &str = "\
tsubame U2 timing (release)
window_px: 3840x2064 at 192 dpi, paint path hardware
first_paint_ms (process start to first completed paint): 72.2
private_bytes_first_paint: 96.8 MB
all times in ms from the navigation event
startup   entries  100000 (11.4 MB)  first_batch     8.81  first_frame    53.64  done    60.00  final_frame    65.05  worker    51.00   G:\\rig-bench\\x\\100k
first     entries  100000 (11.4 MB)  first_batch     1.10  first_frame     2.34  done    61.00  final_frame    63.40  worker    55.00   G:\\rig-bench\\x\\100k
repeat 1  entries  100000 (11.4 MB)  first_batch     1.00  first_frame     2.00  done    62.00  final_frame    62.33  worker    56.00   G:\\rig-bench\\x\\100k
repeat 2  entries  100000 (11.4 MB)  first_batch     0.90  first_frame       -  done    63.00  final_frame       -  worker    57.00   G:\\rig-bench\\x\\100k
private_bytes_end: 149.6 MB
";

    #[test]
    fn parses_a_timing_report() {
        let t = parse_timing(TIMING).expect("parse");
        assert_eq!(t.first_paint_ms, 72.2);
        assert_eq!(t.entries, 100_000);
        assert_eq!(t.window, "3840x2064 at 192 dpi");
        assert_eq!(t.paint_path, "hardware");
        assert_eq!(t.startup_first_rows, Some(53.64));
        assert_eq!(t.startup_sorted, Some(65.05));
        // the startup navigation is not part of the measured rows
        assert_eq!(t.first_rows, vec![2.34, 2.00]);
        assert_eq!(t.sorted, vec![63.40, 62.33]);
        assert_eq!(t.first_batch.len(), 3);
        assert!((t.private_end_mb - 149.6).abs() < 0.01);
        assert_eq!(t.first_rows.iter().copied().fold(0.0, f64::max), 2.34);
    }

    const GATE: &str = "\
tool=tsubame-leak-gate
version=1
mode=full-gui
build=release
window_px=3840x2064
window_dpi=192
paint_path=hardware
fixture=G:\\rig-bench\\tsubame-b1\\gate
folders=200
navigations=10000
tabs=1000
ops=11000
sample_interval=1000
settle_s=5
elapsed_s=95.4
idle.private_bytes=105140000
idle.private_mb=100.30
idle.handles=200
idle.gdi=28
idle.user=11
idle.threads=9
start.private_bytes=101400000
start.private_mb=96.70
start.working_set_bytes=110000000
start.handles=210
start.gdi=30
start.user=12
start.threads=9
sample.1000.private_bytes=101900000
sample.1000.private_mb=97.18
sample.1000.working_set_bytes=110000000
sample.1000.handles=212
sample.1000.gdi=30
sample.1000.user=12
sample.1000.threads=9
end.private_bytes=103000000
end.private_mb=98.23
end.working_set_bytes=112000000
end.handles=214
end.gdi=30
end.user=12
end.threads=9
delta.private_bytes=1600000
delta.private_pct=1.58
allow.private_bytes=5070000
pass.private=1
delta.handles=4
pass.handles=1
delta.gdi=0
pass.gdi=1
delta.user=0
pass.user=1
delta.threads=0
pass.threads=1
peak.private_bytes=120000000
trim.private_bytes=102000000
trim.delta_private_bytes=-1000000
trim.note=icon cache cleared (812 entries, cap 2048 items / 8 MB)
delta.private_bytes_from_idle=6800000
timeouts=0
gate=PASS
fail=-
exit=0
";

    #[test]
    fn parses_a_leak_gate_report() {
        let g = parse_gate(GATE).expect("parse");
        assert!(g.pass);
        assert_eq!(g.navigations, 10_000);
        assert_eq!(g.tabs, 1_000);
        assert_eq!(g.folders, 200);
        assert_eq!(g.samples.len(), 1);
        assert_eq!(g.samples[0].ops, 1000);
        assert_eq!(g.samples[0].handles, 212);
        assert!((g.start.private_mb - 96.70).abs() < 0.01);
        assert!((g.end.private_mb - 98.23).abs() < 0.01);
        assert_eq!(g.delta_handles, 4);
        assert_eq!(g.trim_private_mb, Some(102_000_000.0 / 1048576.0));
        assert_eq!(g.fail, "-");
        // a report without a verdict is an unfinished run, not a pass
        let mut broken: Vec<&str> = GATE.lines().collect();
        broken.retain(|l| !l.starts_with("gate="));
        assert!(parse_gate(&broken.join("\n")).is_err());
    }

    #[test]
    fn leak_verdict_uses_the_percentage_and_the_floor() {
        // 5% of a 100 MB baseline
        let (pass, delta, allow) = leak_verdict(100.0 * 1048576.0, 104.0 * 1048576.0);
        assert!(pass && delta > 0.0);
        assert!((allow - 5.0 * 1048576.0).abs() < 1.0);
        // 5.5% fails
        let (pass, _, _) = leak_verdict(100.0 * 1048576.0, 105.5 * 1048576.0);
        assert!(!pass);
        // a 2 MB baseline ignores up to the 2 MB floor...
        let (pass, _, allow) = leak_verdict(2.0 * 1048576.0, 3.5 * 1048576.0);
        assert!(pass, "allow {allow}");
        // ...but not more
        let (pass, _, _) = leak_verdict(2.0 * 1048576.0, 4.5 * 1048576.0);
        assert!(!pass);
    }

    #[test]
    fn kv_parsing_keeps_values_with_equals_and_dots() {
        let v = parse_kv("a=1\nb = 2\nbad line\nc=x=y\nsample.1000.gdi=31\n");
        assert_eq!(kv(&v, "a"), Some("1"));
        assert_eq!(kv(&v, "b"), Some("2"));
        assert_eq!(kv(&v, "c"), Some("x=y"));
        assert_eq!(kv(&v, "sample.1000.gdi"), Some("31"));
        assert_eq!(kv(&v, "bad"), None);
        assert_eq!(kv_usize(&v, "a"), Some(1));
        assert_eq!(kv_i64(&v, "a"), Some(1));
        assert_eq!(kv_f64(&v, "c"), None);
    }

    #[test]
    fn verdicts_flag_the_budgets() {
        let s = |v: f64| Series { values: vec![v] };
        let timing = Timing { first_rows: vec![2.0, 3.0], sorted: vec![70.0, 80.0], ..Default::default() };
        let gate = parse_gate(GATE).unwrap();
        // every budget met; the cold run was not measured
        let rows = verdicts(&Budgets {
            cold_start: &s(72.0),
            scroll_p95: &s(2.8),
            open_after: &s(100.0),
            timing: &timing,
            cold_timing: None,
            cold_note: "purge needs elevation",
            idle_cpu_ms: 0.0,
            exe_bytes: 601_600,
            gate: &gate,
        });
        let failed: Vec<&str> = rows.iter().filter(|r| r.outcome == Outcome::Fail).map(|r| r.metric.as_str()).collect();
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(rows.iter().filter(|r| r.outcome == Outcome::Pass).count(), 9);
        assert_eq!(rows.iter().filter(|r| r.outcome == Outcome::NotMeasured).count(), 1);
        // slow start, slow folder, slow paint, idle CPU, memory over budget
        let rows = verdicts(&Budgets {
            cold_start: &s(140.0),
            scroll_p95: &s(9.0),
            open_after: &s(149.0),
            timing: &Timing { first_rows: vec![40.0], sorted: vec![250.0], ..Default::default() },
            cold_timing: Some(&Timing { first_rows: vec![41.0], sorted: vec![300.0], ..Default::default() }),
            cold_note: "purge working",
            idle_cpu_ms: 120.0,
            exe_bytes: 9_000_000,
            gate: &gate,
        });
        let failed: Vec<&str> = rows.iter().filter(|r| r.outcome == Outcome::Fail).map(|r| r.metric.as_str()).collect();
        assert_eq!(failed.len(), 9, "{failed:?}");
        assert!(failed.iter().any(|m| m.contains("first rows")));
        assert!(failed.iter().any(|m| m.contains("cold cache")));
        assert!(failed.iter().any(|m| m.contains("exe")));
    }

    #[test]
    fn section_has_the_table_the_gate_and_the_raw_blocks() {
        let meta = parse_kv("stamp=2026-11-18T14:22:05\nmachine=rig\nbuild=release\nbranch=main\ncommit=abc1234\nmode=full\ncache=warm filesystem cache\ncold=not measured\nrig_load=idle\ndpi=192\nfixture_100k=G:\\rig-bench\\tsubame-b1\\100k\ngate_fixture=G:\\rig-bench\\tsubame-b1\\gate\n");
        let run = Run {
            meta,
            idle: parse_kv("idle.seconds=10\nidle.cpu_ms=0\n"),
            cold: vec![parse_s1(S1).unwrap(), parse_s1(S1).unwrap()],
            timing: parse_timing(TIMING).unwrap(),
            cold_timing: None,
            gates: vec![parse_gate(GATE).unwrap()],
            raw_timing: TIMING.to_string(),
            raw_gate: GATE.to_string(),
            raw_idle: "idle.seconds=10\nidle.cpu_ms=0\n".to_string(),
            raw_cold_timing: String::new(),
        };
        let s = section(&run, "abc1234");
        assert!(s.starts_with("## 2026-11-18 14:22 — rig, commit `abc1234`"), "{s}");
        assert!(s.contains("| Cold start to first painted frame | < 100 ms | 70.50 ms median"), "{s}");
        assert!(s.contains("| 100,000-entry folder, cold cache | < 30 / < 200 ms | not measured"), "{s}");
        assert!(s.contains("Not measured: 100,000-entry folder, cold cache"), "{s}");
        assert!(s.contains("### Leak gate evidence"), "{s}");
        assert!(s.contains("| start | 96.7 |"), "{s}");
        assert!(s.contains("Gate: **PASS**"), "{s}");
        assert!(s.contains("<details><summary>Raw: leak-gate.txt</summary>"), "{s}");
        assert!(s.contains("Informational: startup navigation first rows 53.64 ms"), "{s}");
        // the 4K memory rows are over the 110 MB budget and must say so
        assert!(s.contains("**FAIL**"), "{s}");
    }
}
