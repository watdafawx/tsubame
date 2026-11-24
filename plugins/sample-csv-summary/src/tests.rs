use super::*;

fn text_of(nodes: &[Node]) -> Vec<String> {
    nodes
        .iter()
        .map(|n| match n {
            Node::Heading(t) | Node::Text(t) | Node::DimText(t) | Node::Mono(t) => t.clone(),
            Node::Pair((k, v)) => format!("{k}={v}"),
            Node::Indent => "{".into(),
            Node::Outdent => "}".into(),
            _ => "?".into(),
        })
        .collect()
}

#[test]
fn counts_rows_and_columns() {
    let s = summarize("a,b,c\n1,2,3\n4,5,6\n", 10);
    assert_eq!((s.records, s.columns, s.delimiter), (3, 3, ','));
    assert_eq!(s.head[1], ["1", "2", "3"]);
}

#[test]
fn quotes_hold_delimiters_quotes_and_line_breaks() {
    let s = summarize("name,note\n\"Doe, Jane\",\"said \"\"hi\"\"\nthen left\"\nx,y\n", 10);
    assert_eq!((s.records, s.columns), (3, 2));
    assert_eq!(s.head[1], ["Doe, Jane", "said \"hi\"\nthen left"]);
}

#[test]
fn crlf_blank_lines_bom_and_no_final_newline() {
    let s = summarize("\u{feff}a;b\r\n1;2\r\n\r\n3;4", 10);
    assert_eq!((s.records, s.columns, s.delimiter), (3, 2, ';'));
    assert_eq!(s.head[0], ["a", "b"], "the BOM is not part of the first name");
    assert_eq!(s.head[2], ["3", "4"]);
}

#[test]
fn uneven_rows_use_the_widest() {
    assert_eq!(summarize("a\n1,2,3,4\nx,y\n", 10).columns, 4);
}

#[test]
fn empty_and_blank_input_have_no_rows() {
    assert_eq!(summarize("", 5).records, 0);
    assert_eq!(summarize("\n\r\n\n", 5).records, 0);
}

#[test]
fn an_empty_field_still_counts_as_a_field() {
    let s = summarize(",\n,,\n", 5);
    assert_eq!((s.records, s.columns), (2, 3));
}

#[test]
fn delimiter_detection_ignores_quoted_text() {
    assert_eq!(detect_delimiter("\"a;b;c\",d\n"), ',');
    assert_eq!(detect_delimiter("a\tb\tc,d\n"), '\t');
    assert_eq!(detect_delimiter("single\n"), ',');
}

#[test]
fn only_the_asked_records_are_kept() {
    let mut text = String::new();
    for i in 0..1000 {
        text.push_str(&format!("{i},x\n"));
    }
    let s = summarize(&text, 3);
    assert_eq!((s.records, s.head.len()), (1000, 3));
}

#[test]
fn render_shows_counts_header_and_sample() {
    let nodes = render("data.csv", 2048, "id,name\n1,Ann\n2,Bo\n", false);
    let t = text_of(&nodes);
    assert_eq!(t[0], "data.csv");
    assert!(t.contains(&"Rows=2".to_string()), "{t:?}");
    assert!(t.contains(&"Columns=2".to_string()));
    assert!(t.contains(&"Delimiter=comma".to_string()));
    assert!(t.contains(&"Size=2.0 KB".to_string()));
    assert!(t.contains(&" 1  id".to_string()) && t.contains(&" 2  name".to_string()));
    assert!(t.contains(&"1 | Ann".to_string()) && t.contains(&"2 | Bo".to_string()));
}

#[test]
fn render_of_a_cut_file_says_at_least_and_drops_the_partial_record() {
    // header, 2 whole records, then half of a fourth
    let nodes = render("big.csv", 5_000_000, "h1,h2\na,b\nc,d\ne,", true);
    let t = text_of(&nodes);
    assert!(t.contains(&"Rows=at least 2".to_string()), "{t:?}");
}

#[test]
fn render_cuts_long_cells_and_many_columns() {
    let header: Vec<String> = (0..20).map(|i| format!("c{i}")).collect();
    let text = format!("{}\n{}\n", header.join(","), "x".repeat(100));
    let t = text_of(&render("w.csv", 1, &text, false));
    assert!(t.iter().any(|l| l == "and 8 more"));
    assert!(t.iter().any(|l| l.ends_with('…') && l.chars().count() == CELL_CHARS));
}

#[test]
fn render_of_an_empty_file() {
    let t = text_of(&render("e.csv", 0, "", false));
    assert_eq!(t, ["e.csv", "No rows."]);
}

#[test]
fn numbers_read_well() {
    assert_eq!(group(0), "0");
    assert_eq!(group(999), "999");
    assert_eq!(group(1000), "1,000");
    assert_eq!(group(1234567), "1,234,567");
    assert_eq!(human_size(512), "512 B");
    assert_eq!(human_size(1536), "1.5 KB");
    assert_eq!(human_size(5 << 20), "5.0 MB");
}
