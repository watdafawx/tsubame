//! Text and code previews: encoding sniffing, line splitting and the
//! zero-dependency 6-token syntax highlighter.
//!
//! At most [`TEXT_CAP`] bytes are read (64 KiB, `docs/preview-mock.md` #6).
//! Content that smells binary (NUL bytes, or invalid UTF-8) is refused and the
//! caller falls back to hex or an Unsupported card. The highlighter emits only
//! [`TokenKind`] runs — six solid colours per theme and no tree-sitter — so the
//! text card can be drawn with plain `DrawText` calls.

use super::{PreviewError, Span, TextLine, TokenKind};
use std::io::Read;
use std::path::Path;

/// Bytes read from a text file before the truncation banner appears.
pub const TEXT_CAP: usize = 65_536;

/// Longest line kept in a [`TextLine`], so one minified file cannot blow up the
/// draw list and every [`Span::len`] fits in `u16`.
pub const MAX_LINE_CHARS: usize = 4_096;

/// Bytes examined by the NUL-byte heuristic.
pub const NUL_SNIFF_BYTES: usize = 4_096;

/// Text encoding found by [`detect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl Encoding {
    /// The badge text (`UTF-8 · LF` in the mock).
    pub fn as_str(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
        }
    }
}

/// A text preview: the capped, tinted lines plus the header metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewText {
    pub lines: Vec<TextLine>,
    /// Display name of the language (`Rust`, `Plain Text`, ...).
    pub language: String,
    /// `UTF-8`, `UTF-16 LE`, `UTF-16 BE`.
    pub encoding: String,
    pub total_bytes: u64,
    /// The file is larger than [`TEXT_CAP`].
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Encoding detection
// ---------------------------------------------------------------------------

/// Sniffs `bytes`; `None` means binary (NUL bytes or invalid UTF-8).
///
/// UTF-16 is recognised by its BOM or by the every-other-byte NUL pattern of
/// mostly-ASCII text; anything else is UTF-8 or binary.
pub fn detect(bytes: &[u8]) -> Option<Encoding> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Some(Encoding::Utf8);
    }
    if bytes.starts_with(&[0xff, 0xfe]) {
        return Some(Encoding::Utf16Le);
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        return Some(Encoding::Utf16Be);
    }
    let sample = &bytes[..bytes.len().min(NUL_SNIFF_BYTES)];
    let pairs = sample.len() / 2;
    if pairs >= 4 {
        let even_nuls = sample.iter().step_by(2).filter(|b| **b == 0).count();
        let odd_nuls = sample.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
        // UTF-16LE ASCII is `61 00`: NULs on odd offsets (and vice versa).
        if odd_nuls * 4 >= pairs && even_nuls == 0 {
            return Some(Encoding::Utf16Le);
        }
        if even_nuls * 4 >= pairs && odd_nuls == 0 {
            return Some(Encoding::Utf16Be);
        }
    }
    if sample.contains(&0) {
        return None;
    }
    // A cut at the cap can end mid-character; that is not a binary signal.
    match std::str::from_utf8(bytes) {
        Ok(_) => Some(Encoding::Utf8),
        Err(e) if bytes.len() + 4 >= TEXT_CAP && e.valid_up_to() + 4 > bytes.len() => Some(Encoding::Utf8),
        Err(_) => None,
    }
}

/// Decodes `bytes` as `enc`, dropping a BOM and replacing invalid sequences.
pub fn decode(bytes: &[u8], enc: Encoding) -> String {
    match enc {
        Encoding::Utf8 => {
            let body = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
            String::from_utf8_lossy(body).into_owned()
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let bytes = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) { &bytes[2..] } else { bytes };
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| match enc {
                    Encoding::Utf16Le => u16::from_le_bytes(*c),
                    _ => u16::from_be_bytes(*c),
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Reads and tints up to [`TEXT_CAP`] bytes of `path`.
///
/// [`PreviewError::Binary`] means the file is not text; the caller routes it to
/// hex or Unsupported.
pub fn load(path: &Path, syntax: bool) -> Result<PreviewText, PreviewError> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PreviewError::NotFound(path.display().to_string()),
        std::io::ErrorKind::PermissionDenied => PreviewError::AccessDenied(path.display().to_string()),
        _ => PreviewError::Io(format!("{}: {e}", path.display())),
    })?;
    let total_bytes = meta.len();
    let file = std::fs::File::open(path).map_err(|e| map_io(path, e))?;
    let mut buf = Vec::with_capacity(TEXT_CAP + 4);
    file.take(TEXT_CAP as u64 + 4)
        .read_to_end(&mut buf)
        .map_err(|e| map_io(path, e))?;
    let enc = detect(&buf).ok_or(PreviewError::Binary)?;
    let mut text = decode(&buf, enc);
    let truncated = total_bytes > TEXT_CAP as u64;
    if text.len() > TEXT_CAP {
        let mut cut = TEXT_CAP;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    let lang = Language::for_path(path);
    let lines: Vec<TextLine> = text
        .lines()
        .map(|line| {
            let line: String = line.chars().take(MAX_LINE_CHARS).collect();
            if syntax {
                TextLine { spans: highlight_line(&line, lang), text: line }
            } else {
                TextLine::plain(line)
            }
        })
        .collect();
    Ok(PreviewText {
        lines,
        language: lang.as_str().to_string(),
        encoding: enc.as_str().to_string(),
        total_bytes,
        truncated,
    })
}

fn map_io(path: &Path, e: std::io::Error) -> PreviewError {
    match e.kind() {
        std::io::ErrorKind::NotFound => PreviewError::NotFound(path.display().to_string()),
        std::io::ErrorKind::PermissionDenied => PreviewError::AccessDenied(path.display().to_string()),
        _ => PreviewError::Io(format!("{}: {e}", path.display())),
    }
}

// ---------------------------------------------------------------------------
// Languages
// ---------------------------------------------------------------------------

/// The language families the highlighter knows. `Plain` gets no tinting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    C,
    Go,
    Toml,
    Json,
    Yaml,
    Markdown,
    Shell,
    Html,
    Css,
    Xml,
    Plain,
}

impl Language {
    /// The badge text.
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::TypeScript => "TypeScript",
            Language::JavaScript => "JavaScript",
            Language::Python => "Python",
            Language::C => "C",
            Language::Go => "Go",
            Language::Toml => "TOML",
            Language::Json => "JSON",
            Language::Yaml => "YAML",
            Language::Markdown => "Markdown",
            Language::Shell => "Shell",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::Xml => "XML",
            Language::Plain => "Plain Text",
        }
    }

    /// Language of a file, by extension (and the usual extensionless names).
    pub fn for_path(path: &Path) -> Language {
        match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
            "rs" => Language::Rust,
            "ts" | "tsx" | "mts" | "cts" => Language::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            "py" | "pyw" | "pyi" => Language::Python,
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" | "ipp" => Language::C,
            "go" => Language::Go,
            "toml" => Language::Toml,
            "json" | "jsonc" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "md" | "markdown" => Language::Markdown,
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "bat" | "cmd" => Language::Shell,
            "html" | "htm" | "xhtml" => Language::Html,
            "css" | "scss" | "less" => Language::Css,
            "xml" | "xaml" | "svg" | "xsd" | "xsl" => Language::Xml,
            "ini" | "cfg" | "conf" => Language::Toml,
            _ => Language::Plain,
        }
    }

    /// Prefixes that start a comment running to the end of the line.
    fn line_comments(self) -> &'static [&'static str] {
        match self {
            Language::Rust | Language::JavaScript | Language::TypeScript | Language::C | Language::Go => &["//"],
            Language::Python | Language::Toml | Language::Yaml | Language::Shell => &["#"],
            Language::Json => &[],
            Language::Markdown | Language::Html | Language::Xml | Language::Css => &[],
            Language::Plain => &[],
        }
    }

    /// One-line block comment pair (the whole line is tinted when it does not
    /// close before the line ends).
    fn block_comment(self) -> Option<(&'static str, &'static str)> {
        match self {
            Language::Rust | Language::JavaScript | Language::TypeScript | Language::C | Language::Go | Language::Css => {
                Some(("/*", "*/"))
            }
            Language::Html | Language::Xml => Some(("<!--", "-->")),
            _ => None,
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Language::Rust => RUST_KEYWORDS,
            Language::TypeScript => TS_KEYWORDS,
            Language::JavaScript => JS_KEYWORDS,
            Language::Python => PYTHON_KEYWORDS,
            Language::C => C_KEYWORDS,
            Language::Go => GO_KEYWORDS,
            Language::Toml => TOML_KEYWORDS,
            Language::Json => JSON_KEYWORDS,
            Language::Yaml => YAML_KEYWORDS,
            Language::Markdown => &[],
            Language::Shell => SHELL_KEYWORDS,
            Language::Html | Language::Css | Language::Xml => &[],
            Language::Plain => &[],
        }
    }

    fn types(self) -> &'static [&'static str] {
        match self {
            Language::Rust => RUST_TYPES,
            Language::TypeScript | Language::JavaScript => JS_TYPES,
            Language::Python => PYTHON_TYPES,
            Language::C => C_TYPES,
            Language::Go => GO_TYPES,
            _ => &[],
        }
    }

    /// Capitalised identifiers are types (Rust's `String`, C's `FILE`).
    fn capitalized_types(self) -> bool {
        matches!(self, Language::Rust | Language::C | Language::Go | Language::JavaScript | Language::TypeScript)
    }

    /// Backticks quote strings (JS templates, Markdown inline code).
    fn backticks(self) -> bool {
        matches!(self, Language::JavaScript | Language::TypeScript | Language::Markdown | Language::Shell)
    }

    /// Single quotes quote strings. Off for Markdown, where apostrophes are
    /// prose.
    fn single_quotes(self) -> bool {
        !matches!(self, Language::Markdown | Language::Toml | Language::Json)
    }

    /// Identifiers at the start of a line followed by `=` or `:` are keys
    /// (`[toml]`, YAML).
    fn key_types(self) -> bool {
        matches!(self, Language::Toml | Language::Yaml | Language::C)
    }
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
    "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
];

const RUST_TYPES: &[&str] = &[
    "bool", "char", "str", "String", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
    "u128", "usize", "f32", "f64", "Vec", "Option", "Result", "Box", "Rc", "Arc", "Cow", "HashMap", "BTreeMap",
    "HashSet", "Path", "PathBuf", "SystemTime", "Duration",
];

const JS_KEYWORDS: &[&str] = &[
    "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else",
    "export", "extends", "false", "finally", "for", "function", "if", "import", "in", "instanceof", "let", "new",
    "null", "return", "static", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void",
    "while", "with", "yield", "async", "await", "from", "of", "get", "set",
];

const TS_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "declare",
    "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for", "from",
    "function", "get", "if", "implements", "import", "in", "instanceof", "interface", "keyof", "let",
    "namespace", "new", "null", "of", "private", "protected", "public", "readonly", "return", "satisfies",
    "set", "static", "super", "switch", "this", "throw", "true", "try", "type", "typeof", "undefined", "var",
    "void", "while", "yield",
];

const JS_TYPES: &[&str] = &[
    "any", "unknown", "never", "number", "string", "boolean", "object", "symbol", "bigint", "Array", "Promise",
    "Map", "Set", "Record", "Partial", "Readonly", "Date", "Error", "RegExp",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else",
    "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None",
    "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while", "with", "yield", "match",
    "case",
];

const PYTHON_TYPES: &[&str] = &[
    "int", "float", "complex", "str", "bytes", "bool", "list", "dict", "set", "tuple", "frozenset", "object",
    "Optional", "List", "Dict", "Any", "Callable", "Iterable", "Iterator",
];

const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else", "enum", "extern",
    "float", "for", "goto", "if", "inline", "int", "long", "register", "restrict", "return", "short", "signed",
    "sizeof", "static", "struct", "switch", "typedef", "union", "unsigned", "void", "volatile", "while", "class",
    "namespace", "template", "typename", "public", "private", "protected", "virtual", "override", "final", "new",
    "delete", "nullptr", "true", "false", "using", "constexpr", "noexcept", "concept", "requires",
];

const C_TYPES: &[&str] = &[
    "size_t", "ssize_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t",
    "uint64_t", "intptr_t", "uintptr_t", "wchar_t", "std", "string", "vector", "map", "unordered_map",
    "unique_ptr", "shared_ptr", "optional", "FILE", "HRESULT", "HWND",
];

const GO_KEYWORDS: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go",
    "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch",
    "type", "var", "nil", "true", "false", "iota",
];

const GO_TYPES: &[&str] = &[
    "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int", "int8", "int16", "int32",
    "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64", "uintptr", "any",
];

const TOML_KEYWORDS: &[&str] = &["true", "false", "inf", "nan"];

const JSON_KEYWORDS: &[&str] = &["true", "false", "null"];

const YAML_KEYWORDS: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

const SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "function", "in",
    "return", "local", "export", "set", "unset", "shift", "exit", "source", "alias", "until", "select", "time",
    "param", "begin", "process", "end", "filter", "foreach", "try", "catch", "finally", "throw", "switch",
    "break", "continue",
];

// ---------------------------------------------------------------------------
// Highlighter
// ---------------------------------------------------------------------------

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Splits `text` into tinted runs. Concatenating the runs covers the whole
/// line, so the UI can advance `Span::len` characters at a time.
pub fn highlight_line(text: &str, lang: Language) -> Vec<Span> {
    let chars = text.chars().count();
    if chars == 0 {
        return Vec::new();
    }
    if lang == Language::Markdown && text.trim_start().starts_with('#') {
        return vec![Span { len: chars.min(u16::MAX as usize) as u16, kind: TokenKind::Type }];
    }
    let b = text.as_bytes();
    let mut out: Vec<Span> = Vec::new();
    let mut plain = 0usize; // byte offset where the pending Plain run starts
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if lang.line_comments().iter().any(|p| b[i..].starts_with(p.as_bytes())) {
            push(&mut out, text, plain, i, TokenKind::Plain);
            push(&mut out, text, i, b.len(), TokenKind::Comment);
            return out;
        }
        if let Some((open, close)) = lang.block_comment()
            && b[i..].starts_with(open.as_bytes())
        {
            push(&mut out, text, plain, i, TokenKind::Plain);
            let end = text[i + open.len()..].find(close).map_or(b.len(), |k| i + open.len() + k + close.len());
            push(&mut out, text, i, end, TokenKind::Comment);
            i = end;
            plain = i;
            continue;
        }
        if c == b'"' || (c == b'\'' && lang.single_quotes()) || (c == b'`' && lang.backticks()) {
            push(&mut out, text, plain, i, TokenKind::Plain);
            let (start, end) = scan_string(b, i, c);
            push(&mut out, text, start, end, TokenKind::String);
            i = end;
            plain = i;
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'.' | b'_')) {
                i += 1;
            }
            push(&mut out, text, plain, start, TokenKind::Plain);
            push(&mut out, text, start, i, TokenKind::Number);
            plain = i;
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            while i < b.len() && is_ident_continue(b[i]) {
                i += 1;
            }
            let word = &text[start..i];
            let kind = if lang.keywords().contains(&word) {
                Some(TokenKind::Keyword)
            } else if lang.types().contains(&word)
                || (lang.capitalized_types() && word.chars().next().is_some_and(char::is_uppercase))
                || (lang.key_types() && is_key_position(&text[..start], b, i))
            {
                Some(TokenKind::Type)
            } else {
                None
            };
            if let Some(kind) = kind {
                push(&mut out, text, plain, start, TokenKind::Plain);
                push(&mut out, text, start, i, kind);
                plain = i;
            }
            continue;
        }
        if c >= 0x80 {
            // Skip a whole UTF-8 character so slices stay on boundaries.
            i += 1;
            while i < b.len() && (b[i] & 0xc0) == 0x80 {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    push(&mut out, text, plain, b.len(), TokenKind::Plain);
    out
}

/// Scans a quoted run starting at `i`; returns `(start, end)` byte offsets.
fn scan_string(b: &[u8], i: usize, quote: u8) -> (usize, usize) {
    let start = i;
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j = (j + 2).min(b.len());
        } else if b[j] == quote {
            return (start, j + 1);
        } else {
            j += 1;
        }
    }
    (start, b.len())
}

/// True when only whitespace precedes the identifier and `=`/`:` follows it.
fn is_key_position(before: &str, b: &[u8], end: usize) -> bool {
    if !before.trim().is_empty() {
        return false;
    }
    let mut j = end;
    while j < b.len() && matches!(b[j], b' ' | b'\t') {
        j += 1;
    }
    matches!(b.get(j), Some(b'=') | Some(b':'))
}

/// Appends a run, merging with the previous run of the same kind.
fn push(out: &mut Vec<Span>, text: &str, from: usize, to: usize, kind: TokenKind) {
    if from >= to || from >= text.len() {
        return;
    }
    let to = to.min(text.len());
    let n = text[from..to].chars().count();
    if n == 0 {
        return;
    }
    let n = n.min(u16::MAX as usize) as u16;
    if let Some(last) = out.last_mut()
        && last.kind == kind
        && (last.len as usize) + (n as usize) <= u16::MAX as usize
    {
        last.len += n;
        return;
    }
    out.push(Span { len: n, kind });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-preview-txt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Every line's runs must cover exactly its characters.
    fn assert_spans_cover(line: &TextLine) {
        let sum: usize = line.spans.iter().map(|s| s.len as usize).sum();
        assert_eq!(sum, line.chars(), "{:?} does not cover {:?}", line.spans, line.text);
        assert!(line.spans.iter().all(|s| s.len > 0), "{:?} has an empty run", line.spans);
    }

    fn kinds(line: &TextLine) -> Vec<TokenKind> {
        line.spans.iter().map(|s| s.kind).collect()
    }

    fn tints(text: &str, lang: Language) -> (TextLine, Vec<(TokenKind, String)>) {
        let spans = highlight_line(text, lang);
        let line = TextLine { text: text.into(), spans };
        assert_spans_cover(&line);
        let mut at = 0usize;

        let mut runs = Vec::new();
        let chars: Vec<char> = line.text.chars().collect();
        for s in &line.spans {
            let run: String = chars[at..at + s.len as usize].iter().collect();
            runs.push((s.kind, run));
            at += s.len as usize;
        }
        (line, runs)
    }

    #[test]
    fn encoding_detection_covers_utf8_utf16_and_binary() {
        assert_eq!(detect(b"hello world\n"), Some(Encoding::Utf8));
        assert_eq!(detect(b"\xef\xbb\xbfhello"), Some(Encoding::Utf8));
        assert_eq!(detect(b"\xff\xfeh\0i\0"), Some(Encoding::Utf16Le));
        assert_eq!(detect(b"\xfe\xff\0h\0i"), Some(Encoding::Utf16Be));
        // BOM-less UTF-16 LE of ASCII text
        let le: Vec<u8> = "hello world, this is text".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(detect(&le), Some(Encoding::Utf16Le));
        assert_eq!(decode(&le, Encoding::Utf16Le), "hello world, this is text");
        // NUL bytes and invalid UTF-8 are binary
        assert_eq!(detect(&[0x00, 0x01, 0x02, 0x03]), None);
        assert_eq!(detect(&[0xff, 0xfe, 0x00, 0x00, 0x41]), Some(Encoding::Utf16Le), "a BOM wins");
        assert_eq!(detect(&[0x41, 0xff, 0xfe, 0x41]), None, "invalid UTF-8, no BOM");
    }

    #[test]
    fn utf16_files_decode_without_boms_in_the_text() {
        let d = temp_dir("utf16");
        let p = d.join("a.txt");
        let body: Vec<u8> = [0xff, 0xfe].into_iter().chain("one\ntwo".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        std::fs::write(&p, body).unwrap();
        let t = load(&p, true).unwrap();
        assert_eq!(t.encoding, "UTF-16 LE");
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.lines[0].text, "one");
        assert_eq!(t.lines[1].text, "two");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_64_kib_cap_truncates_and_reports_the_real_size() {
        let d = temp_dir("cap");
        let p = d.join("big.log");
        let mut body = String::new();
        for n in 0..20_000 {
            body.push_str(&format!("line {n}\n"));
        }
        std::fs::write(&p, &body).unwrap();
        let t = load(&p, false).unwrap();
        assert!(t.truncated);
        assert_eq!(t.total_bytes, body.len() as u64);
        let read: usize = t.lines.iter().map(|l| l.text.len() + 1).sum();
        assert!(read <= TEXT_CAP + 1, "{read}");
        assert!(t.lines.len() > 1_000);

        let small = d.join("small.txt");
        std::fs::write(&small, "just this\n").unwrap();
        let t = load(&small, false).unwrap();
        assert!(!t.truncated);
        assert_eq!(t.lines.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lines_split_on_lf_and_crlf_and_are_capped() {
        let d = temp_dir("lines");
        let p = d.join("a.txt");
        std::fs::write(&p, b"a\r\nb\n\r\nc").unwrap();
        let t = load(&p, false).unwrap();
        let texts: Vec<&str> = t.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "", "c"]);

        let long = d.join("long.txt");
        std::fs::write(&long, "x".repeat(MAX_LINE_CHARS + 50)).unwrap();
        let t = load(&long, false).unwrap();
        assert_eq!(t.lines[0].chars(), MAX_LINE_CHARS);
        assert_spans_cover(&t.lines[0]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn binary_text_files_are_refused() {
        let d = temp_dir("binary");
        let p = d.join("a.txt");
        std::fs::write(&p, [0u8, 0, 0, 0, 1, 2, 3]).unwrap();
        assert_eq!(load(&p, true), Err(PreviewError::Binary));
        let bad = d.join("b.txt");
        std::fs::write(&bad, [0x41, 0xff, 0xff, 0x41, 0x42]).unwrap();
        assert_eq!(load(&bad, true), Err(PreviewError::Binary));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn languages_are_picked_by_extension() {
        let cases = [
            ("a.rs", Language::Rust, "Rust"),
            ("a.ts", Language::TypeScript, "TypeScript"),
            ("a.js", Language::JavaScript, "JavaScript"),
            ("a.py", Language::Python, "Python"),
            ("a.c", Language::C, "C"),
            ("a.hpp", Language::C, "C"),
            ("a.go", Language::Go, "Go"),
            ("a.toml", Language::Toml, "TOML"),
            ("a.json", Language::Json, "JSON"),
            ("a.yml", Language::Yaml, "YAML"),
            ("a.md", Language::Markdown, "Markdown"),
            ("a.ps1", Language::Shell, "Shell"),
            ("a.html", Language::Html, "HTML"),
            ("a.css", Language::Css, "CSS"),
            ("a.xml", Language::Xml, "XML"),
            ("a.dat", Language::Plain, "Plain Text"),
        ];
        for (name, lang, label) in cases {
            assert_eq!(Language::for_path(&PathBuf::from(name)), lang, "{name}");
            assert_eq!(lang.as_str(), label);
        }
    }

    #[test]
    fn the_rust_line_gets_keywords_types_numbers_and_a_comment() {
        let (line, runs) = tints("let x: u32 = 42; // count", Language::Rust);
        assert!(runs.contains(&(TokenKind::Keyword, "let".into())), "{runs:?}");
        assert!(runs.contains(&(TokenKind::Type, "u32".into())), "{runs:?}");
        assert!(runs.contains(&(TokenKind::Number, "42".into())), "{runs:?}");
        assert!(runs.contains(&(TokenKind::Comment, "// count".into())), "{runs:?}");
        assert_eq!(kinds(&line).last(), Some(&TokenKind::Comment));
        assert_spans_cover(&line);
    }

    #[test]
    fn strings_include_their_quotes_and_escapes() {
        let (line, runs) = tints(r#"let s = "hi \"there\"";"#, Language::Rust);
        assert!(runs.contains(&(TokenKind::String, "\"hi \\\"there\\\"\"".into())), "{runs:?}");
        assert_spans_cover(&line);
        // an unterminated string runs to the end of the line
        let (line, runs) = tints("print(\"oops", Language::Python);
        assert_eq!(runs.last(), Some(&(TokenKind::String, "\"oops".into())), "{runs:?}");
        assert_spans_cover(&line);
    }

    #[test]
    fn python_json_toml_and_markdown_tint() {
        let (_, runs) = tints("def f(x):  # doc", Language::Python);
        assert!(runs.contains(&(TokenKind::Keyword, "def".into())), "{runs:?}");
        assert!(runs.contains(&(TokenKind::Comment, "# doc".into())), "{runs:?}");

        let (_, runs) = tints(r#"{"on": true, "n": 12}"#, Language::Json);
        assert!(runs.contains(&(TokenKind::String, "\"on\"".into())), "{runs:?}");
        assert!(runs.contains(&(TokenKind::Keyword, "true".into())), "{runs:?}");
        assert!(runs.contains(&(TokenKind::Number, "12".into())), "{runs:?}");

        let (_, runs) = tints("width = 320", Language::Toml);
        assert!(runs.contains(&(TokenKind::Type, "width".into())), "{runs:?}");
        assert!(runs.contains(&(TokenKind::Number, "320".into())), "{runs:?}");

        let (line, _) = tints("## Heading", Language::Markdown);
        assert_eq!(line.spans, [Span { len: 10, kind: TokenKind::Type }]);
        let (_, runs) = tints("some `code` here", Language::Markdown);
        assert!(runs.contains(&(TokenKind::String, "`code`".into())), "{runs:?}");
    }

    #[test]
    fn block_comments_html_comments_and_plain_text() {
        let (_, runs) = tints("/* all comment */ x", Language::C);
        assert_eq!(runs[0], (TokenKind::Comment, "/* all comment */".into()), "{runs:?}");
        let (_, runs) = tints("<!-- note -->", Language::Html);
        assert_eq!(runs, [(TokenKind::Comment, "<!-- note -->".into())]);
        let line = TextLine {
            text: "no tinting".into(),
            spans: highlight_line("no tinting", Language::Plain),
        };
        assert_eq!(line.spans, [Span { len: 10, kind: TokenKind::Plain }]);
        assert!(highlight_line("", Language::Rust).is_empty());
    }

    #[test]
    fn every_run_covers_unicode_text() {
        let (line, _) = tints("let café = \"naïve ☕\"; // ü", Language::Rust);
        assert_spans_cover(&line);
        assert_eq!(line.chars(), 26);
    }
}
