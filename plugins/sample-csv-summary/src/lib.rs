//! Sample previewer plugin: a text summary of a `.csv` file, built with
//! `tsubame-plugin-sdk`.
//!
//! Reads at most the first MiB of the file (or what the caller's
//! `limits.max_bytes` allows, if less), so a huge file costs the same as a
//! small one. When the file is longer than what was read the counts say "at least".
//!
//! The parsing is plain functions on text (`summarize`), so the unit tests run
//! on the host; only `preview` touches the host's file functions.

use tsubame_plugin_sdk::{Doc, Limits, Node, Previewer, host, plugin};

/// Most bytes read from a file, whatever the caller allows.
const READ_CAP: u64 = 1 << 20;
/// Header names and sample cells are cut to this many characters.
const CELL_CHARS: usize = 24;
const HEADER_SHOWN: usize = 12;
const SAMPLE_ROWS: usize = 5;

/// What a scan of the text found.
#[derive(Debug, PartialEq)]
pub struct Summary {
    /// Records, the header included. Blank lines do not count.
    pub records: usize,
    /// The most fields in any record.
    pub columns: usize,
    /// The first `keep` records, field by field.
    pub head: Vec<Vec<String>>,
    pub delimiter: char,
}

/// The delimiter the first line uses most outside quotes: comma, semicolon or tab.
pub fn detect_delimiter(text: &str) -> char {
    let mut counts = [0usize; 3];
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' if !quoted => break,
            ',' if !quoted => counts[0] += 1,
            ';' if !quoted => counts[1] += 1,
            '\t' if !quoted => counts[2] += 1,
            _ => {}
        }
    }
    let best = (0..3).max_by_key(|&i| (counts[i], 3 - i)).unwrap_or(0);
    [',', ';', '\t'][best]
}

/// Counts the records and fields of `text` and keeps the first `keep` records.
/// Handles quoted fields (with `""` for a quote, and line breaks inside quotes),
/// `\r\n` and `\n`, and a leading byte order mark.
pub fn summarize(text: &str, keep: usize) -> Summary {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delimiter = detect_delimiter(text);
    let mut s = Summary { records: 0, columns: 0, head: Vec::new(), delimiter };
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    // true once the record has any content: a field, a delimiter or a quote
    let mut started = false;
    let mut quoted = false;
    let mut chars = text.chars().peekable();

    fn end_record(fields: &mut Vec<String>, field: &mut String, started: &mut bool, s: &mut Summary, keep: usize) {
        if *started {
            fields.push(std::mem::take(field));
            s.records += 1;
            s.columns = s.columns.max(fields.len());
            if s.head.len() < keep {
                s.head.push(std::mem::take(fields));
            }
        }
        fields.clear();
        field.clear();
        *started = false;
    }

    while let Some(c) = chars.next() {
        let keeping = s.head.len() < keep;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    push_capped(&mut field, '"', keeping);
                } else {
                    quoted = false;
                }
            } else {
                push_capped(&mut field, c, keeping);
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                started = true;
            }
            c if c == delimiter => {
                started = true;
                fields.push(std::mem::take(&mut field));
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => end_record(&mut fields, &mut field, &mut started, &mut s, keep),
            c => {
                started = true;
                push_capped(&mut field, c, keeping);
            }
        }
    }
    end_record(&mut fields, &mut field, &mut started, &mut s, keep);
    s
}

/// Field text is only kept for the records that are shown, and cut at a bound.
fn push_capped(field: &mut String, c: char, keeping: bool) {
    if keeping && field.chars().count() < 200 {
        field.push(c);
    }
}

fn cut(text: &str) -> String {
    if text.chars().count() <= CELL_CHARS {
        return text.to_string();
    }
    let mut s: String = text.chars().take(CELL_CHARS - 1).collect();
    s.push('…');
    s
}

/// `1234567` as `1,234,567`.
pub fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// The rows for the pane. `truncated`: the text is the start of a longer file.
pub fn render(name: &str, file_size: u64, text: &str, truncated: bool) -> Vec<Node> {
    // A cut file ends in a partial record: drop it so the counts are of whole records.
    let text = if truncated { text.rfind('\n').map_or(text, |i| &text[..=i]) } else { text };
    let s = summarize(text, SAMPLE_ROWS + 1);
    let more = if truncated { "at least " } else { "" };
    let mut doc = Doc::new();
    doc.heading(name);
    if s.records == 0 {
        doc.dim("No rows.");
        return doc.finish();
    }
    let delimiter = match s.delimiter {
        ',' => "comma",
        ';' => "semicolon",
        _ => "tab",
    };
    doc.pair("Rows", format!("{more}{}", group(s.records - 1)));
    doc.pair("Columns", group(s.columns));
    doc.pair("Delimiter", delimiter);
    doc.pair("Size", human_size(file_size));
    if truncated {
        doc.dim(format!("Counted the first {} only.", human_size(text.len() as u64)));
    }

    doc.heading("First row");
    doc.indent();
    let header = &s.head[0];
    for (i, h) in header.iter().take(HEADER_SHOWN).enumerate() {
        doc.mono(format!("{:>2}  {}", i + 1, cut(h)));
    }
    if header.len() > HEADER_SHOWN {
        doc.dim(format!("and {} more", header.len() - HEADER_SHOWN));
    }
    doc.outdent();

    if s.head.len() > 1 {
        doc.heading("Next rows");
        doc.indent();
        for row in &s.head[1..] {
            let line: Vec<String> = row.iter().take(6).map(|c| cut(c)).collect();
            doc.mono(line.join(" | "));
        }
        doc.outdent();
    }
    doc.finish()
}

struct CsvSummary;

impl Previewer for CsvSummary {
    const ID: &'static str = "summary";

    fn preview(path: &str, limits: Limits) -> Result<Vec<Node>, String> {
        let info = host::stat(path).map_err(host::describe)?;
        if info.dir {
            return Err("a folder, not a file".into());
        }
        let cap = limits.max_bytes.min(READ_CAP);
        if cap == 0 {
            return Err("the caller allows no reading".into());
        }
        let (bytes, more) = host::read_prefix(path, cap).map_err(host::describe)?;
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
        Ok(render(name, info.size, &String::from_utf8_lossy(&bytes), more))
    }
}

plugin! {
    id: "dev.tsubame.sample-csv-summary",
    version: env!("CARGO_PKG_VERSION"),
    previewers: [CsvSummary],
}

#[cfg(test)]
mod tests;
