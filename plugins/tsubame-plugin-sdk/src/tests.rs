use super::*;

struct Len;
impl Column for Len {
    const ID: &'static str = "len";
    fn value(path: &str) -> Cell {
        Cell::Int(path.len() as i64)
    }
}

struct Shout;
impl Column for Shout {
    const ID: &'static str = "shout";
    // overriding `values` skips the per-path default
    fn values(paths: &[String]) -> Vec<Cell> {
        paths.iter().map(|p| Cell::Text(p.to_uppercase())).collect()
    }
}

struct Echo;
impl Previewer for Echo {
    const ID: &'static str = "echo";
    fn preview(path: &str, limits: Limits) -> Result<Vec<Node>, String> {
        if path.is_empty() {
            return Err("no path".into());
        }
        Ok(Doc::new().heading(path).pair("Budget", limits.max_bytes.to_string()).finish())
    }
}

struct Plug;
impl Registry for Plug {
    const ID: &'static str = "test.plug";
    const VERSION: &'static str = "1.2.3";
    fn entrypoints() -> Vec<(EntryKind, &'static str)> {
        vec![(EntryKind::Column, Len::ID), (EntryKind::Column, Shout::ID), (EntryKind::Previewer, Echo::ID)]
    }
    fn column(id: &str, paths: &[String]) -> Option<Vec<Cell>> {
        match id {
            "len" => Some(Len::values(paths)),
            "shout" => Some(Shout::values(paths)),
            _ => None,
        }
    }
    fn preview(id: &str, path: &str, limits: Limits) -> Option<Result<Vec<Node>, String>> {
        (id == Echo::ID).then(|| Echo::preview(path, limits))
    }
}

type S = Shim<Plug>;

fn paths(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| s.to_string()).collect()
}

fn limits(max_bytes: u64) -> Limits {
    Limits { max_bytes, max_px: 64 }
}

#[test]
fn describe_lists_what_was_declared() {
    let info = <S as wit_meta::Guest>::describe();
    assert_eq!((info.id.as_str(), info.version.as_str()), ("test.plug", "1.2.3"));
    let got: Vec<_> = info.entrypoints.iter().map(|e| (e.kind as u8, e.id.as_str())).collect();
    assert_eq!(got, [(EntryKind::Column as u8, "len"), (EntryKind::Column as u8, "shout"), (EntryKind::Previewer as u8, "echo")]);
}

#[test]
fn column_dispatches_by_id_and_keeps_order_and_count() {
    let cells = <S as wit_column::Guest>::values("len".into(), paths(&[r"C:\a", r"C:\bb", ""]));
    assert!(matches!(cells[..], [Cell::Int(4), Cell::Int(5), Cell::Int(0)]));
    let cells = <S as wit_column::Guest>::values("shout".into(), paths(&[r"c:\x"]));
    assert!(matches!(&cells[..], [Cell::Text(t)] if t == r"C:\X"));
}

#[test]
fn the_default_value_is_an_empty_cell() {
    struct Blank;
    impl Column for Blank {
        const ID: &'static str = "blank";
    }
    assert!(matches!(Blank::values(&paths(&["a", "b"]))[..], [Cell::Empty, Cell::Empty]));
}

#[test]
fn an_empty_batch_is_an_empty_answer() {
    assert!(<S as wit_column::Guest>::values("len".into(), Vec::new()).is_empty());
}

#[test]
fn previewer_dispatches_and_passes_the_error_through() {
    let nodes = <S as wit_previewer::Guest>::preview("echo".into(), r"C:\f.txt".into(), limits(4096)).unwrap();
    assert!(matches!(&nodes[..], [Node::Heading(h), Node::Pair((l, v))] if h == r"C:\f.txt" && l == "Budget" && v == "4096"));
    let e = <S as wit_previewer::Guest>::preview("echo".into(), String::new(), limits(1)).unwrap_err();
    assert_eq!(e, "no path");
    let e = <S as wit_previewer::Guest>::preview("nope".into(), r"C:\f".into(), limits(1)).unwrap_err();
    assert!(e.contains("nope"));
}

#[test]
fn doc_builds_rows_in_order() {
    let mut d = Doc::new();
    assert!(d.is_empty());
    d.heading("H").text("t").dim("d").mono("m").pair("k", "v").indent().image(1, 1, vec![0; 4]).button("Go", "x.y").outdent();
    assert_eq!(d.len(), 9);
    let nodes = d.finish();
    assert!(d.is_empty(), "finish takes the rows");
    assert!(matches!(
        &nodes[..],
        [
            Node::Heading(_),
            Node::Text(_),
            Node::DimText(_),
            Node::Mono(_),
            Node::Pair(_),
            Node::Indent,
            Node::Image(Image { width: 1, height: 1, .. }),
            Node::Button(Button { .. }),
            Node::Outdent
        ]
    ));
}
