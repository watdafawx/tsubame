//! Preview pane and Quick Look backend (P4.F2): data model, classifiers and the
//! decoders the worker thread runs.
//!
//! Everything here follows the `docs/PLAN.md` §3 rules that apply to previews:
//!
//! - **Rule 1 (the UI thread never touches the disk):** [`worker::PreviewWorker`]
//!   decodes on one background thread and delivers [`PreviewKind`] results over
//!   an `mpsc` channel tagged with a generation. A stale generation is dropped
//!   before it reaches the UI, and stale queued requests are discarded so
//!   holding an arrow key never builds a backlog.
//! - **Rule 5 (every cache has a hard cap and an eviction test):**
//!   [`image::PreviewImageCache`] holds decoded bitmaps under a hard 32 MiB /
//!   32-entry LRU and can be cleared when the pane closes.
//! - **Rule 6 (dependencies stay tiny):** image decoding reuses WIC through
//!   [`crate::bitmap`], media metadata is parsed from container headers, and the
//!   syntax tinting is a small tokenizer. No new crates.
//!
//! Sizes and caps come from `docs/preview-mock.md`: 64 KiB of text, 16 KiB of
//! hex, WIC scaled to a 2,048 px edge, and 8 bytes/row in docked panes narrower
//! than 420 px.
//!
//! Wave 3B owns the UI (`src/`): it calls [`worker::PreviewWorker::request`] and
//! draws whatever [`PreviewKind`] comes back.

pub mod hex;
pub mod image;
pub mod media;
pub mod text;
pub mod worker;

pub use hex::{
    HEX_CAP, HEX_NARROW_BYTES_PER_ROW, HEX_WIDE_BYTES_PER_ROW, HexRow, NARROW_PANE_MAX_PX,
    bytes_per_row,
};
pub use image::{LoadedImage, PreviewImageCache, PreviewImageKey, PREVIEW_MAX_EDGE};
pub use media::MediaInfo;
pub use text::{Encoding, Language, MAX_LINE_CHARS, TEXT_CAP};
pub use worker::{PreviewReply, PreviewWorker, PreviewWake as Wake};

use crate::bitmap::Bitmap;
use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Text model
// ---------------------------------------------------------------------------

/// The six tinting tokens of the zero-dependency highlighter
/// (`tokens.json`: `syn_kw`, `syn_ty`, `syn_str`, `syn_num`, `syn_cm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    Type,
}

/// One run of characters in a [`TextLine`], in the order they are drawn.
///
/// `len` is a character count (not bytes), so a drawing backend can advance it
/// straight through the text. Lines are capped at
/// [`text::MAX_LINE_CHARS`], which keeps every run inside `u16`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub len: u16,
    pub kind: TokenKind,
}

impl Span {
    /// A run of `text`'s characters.
    pub fn of(text: &str, kind: TokenKind) -> Span {
        Span { len: text.chars().count().min(u16::MAX as usize) as u16, kind }
    }
}

/// One previewed line: its text plus the tinting runs that cover it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLine {
    pub text: String,
    /// Runs in order; their `len`s add up to `text.chars().count()`.
    pub spans: Vec<Span>,
}

impl TextLine {
    /// A line with no tinting.
    pub fn plain(text: impl Into<String>) -> TextLine {
        let text: String = text.into();
        let text = text.chars().take(MAX_LINE_CHARS).collect::<String>();
        let spans = vec![Span { len: text.chars().count() as u16, kind: TokenKind::Plain }];
        TextLine { text, spans }
    }

    /// Characters in the line (what the gutter and wrapping use).
    pub fn chars(&self) -> usize {
        self.text.chars().count()
    }
}

/// One structured metadata row of the media and image previews
/// (`("Duration", "3:45")`).
pub type MediaRow = (String, String);

/// Whether the media file is audio or video (drives the poster frame in the
/// mock).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Audio,
    Video,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
        }
    }
}

// ---------------------------------------------------------------------------
// Preview results
// ---------------------------------------------------------------------------

/// What a file previews as. The UI draws one card per variant.
#[derive(Clone, Debug, PartialEq)]
pub enum PreviewKind {
    /// A decoded bitmap (premultiplied BGRA, fitted within
    /// [`PREVIEW_MAX_EDGE`]) plus the file's own dimensions and metadata.
    Image {
        bmp: Bitmap,
        orig_w: u32,
        orig_h: u32,
        format: String,
        file_size: u64,
        /// Milliseconds since the UNIX epoch, or 0 when unavailable.
        modified: i64,
    },
    /// The first [`TEXT_CAP`] bytes, split into lines and tinted.
    Text {
        lines: Vec<TextLine>,
        /// Display name of the language, e.g. `Rust` or `Plain Text`.
        language: String,
        /// `UTF-8`, `UTF-16 LE`, `UTF-16 BE`.
        encoding: String,
        total_bytes: u64,
        /// The file is bigger than the cap: show the truncation banner.
        truncated: bool,
    },
    /// Audio or video metadata (no player is embedded).
    Media { kind: MediaKind, rows: Vec<MediaRow>, file_size: u64 },
    /// The first [`HEX_CAP`] bytes of a binary, with an optional magic label.
    Hex { bytes: Vec<u8>, total_bytes: u64, truncated: bool, magic: Option<String> },
    /// No previewer for this file; `can_hex` offers the `View as Hex` button.
    Unsupported { reason: String, can_hex: bool, file_size: u64 },
    /// The file existed but could not be read or decoded.
    Error(String),
}

impl PreviewKind {
    /// Short variant name, for logs and status lines.
    pub fn kind_name(&self) -> &'static str {
        match self {
            PreviewKind::Image { .. } => "image",
            PreviewKind::Text { .. } => "text",
            PreviewKind::Media { .. } => "media",
            PreviewKind::Hex { .. } => "hex",
            PreviewKind::Unsupported { .. } => "unsupported",
            PreviewKind::Error(_) => "error",
        }
    }

    /// True when this result is cheap enough to keep and reuse as is.
    pub fn is_error(&self) -> bool {
        matches!(self, PreviewKind::Error(_))
    }
}

/// Why a decoder produced no preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewError {
    NotFound(String),
    AccessDenied(String),
    /// The file is larger than the decoder reads.
    TooLarge { size: u64 },
    /// Text detection found NUL bytes (or invalid UTF-8): not text.
    Binary,
    /// A container the parser does not understand.
    Unsupported(String),
    /// The container is known but this file is damaged.
    Decode(String),
    Io(String),
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PreviewError::NotFound(p) => write!(f, "not found: {p}"),
            PreviewError::AccessDenied(p) => write!(f, "access denied: {p}"),
            PreviewError::TooLarge { size } => write!(f, "file is too large: {} bytes", size),
            PreviewError::Binary => f.write_str("binary content"),
            PreviewError::Unsupported(why) => write!(f, "no previewer: {why}"),
            PreviewError::Decode(why) => write!(f, "cannot decode: {why}"),
            PreviewError::Io(why) => write!(f, "cannot read: {why}"),
        }
    }
}

impl std::error::Error for PreviewError {}

/// Per-request switches, mirroring `[preview]` in the config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewOptions {
    /// Force the hex view (the `View as Hex` button).
    pub force_hex: bool,
    /// Hex directly for `.bin`/`.dat`/`.exe`/`.dll`.
    pub direct_hex_binaries: bool,
    /// Run the 6-token highlighter over text.
    pub syntax_highlight: bool,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        PreviewOptions { force_hex: false, direct_hex_binaries: true, syntax_highlight: true }
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Which decoder (if any) owns an extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileClass {
    Image,
    Text,
    Media,
    /// Raw binaries that go straight to hex (`docs/preview-mock.md` open
    /// question 2).
    DirectHex,
    /// No previewer: an Unsupported card with a `View as Hex` button.
    Other,
}

pub const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "ico", "tif", "tiff", "webp", "heic", "avif",
];

pub const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "text", "md", "markdown", "rst", "log", "ini", "cfg", "conf", "toml", "json", "jsonc",
    "yaml", "yml", "xml", "xaml", "svg", "html", "htm", "xhtml", "css", "scss", "less", "rs", "js",
    "jsx", "mjs", "cjs", "ts", "tsx", "py", "pyw", "c", "h", "cpp", "cc", "cxx", "hpp", "hxx", "go",
    "java", "kt", "kts", "cs", "php", "rb", "lua", "sh", "bash", "zsh", "fish", "ps1", "psm1", "bat",
    "cmd", "sql", "csv", "tsv", "diff", "patch", "srt", "vtt",
];

pub const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "flv", "mp3", "flac", "wav", "ogg", "oga",
    "m4a", "aac", "wma", "opus", "aiff", "aif",
];

pub const DIRECT_HEX_EXTENSIONS: &[&str] = &["bin", "dat", "exe", "dll"];

/// Extensionless files that are still text (`Makefile`, `LICENSE`, ...).
const TEXT_FILE_NAMES: &[&str] = &[
    "makefile", "dockerfile", "cmakelists.txt", "license", "licence", "readme", "changelog",
    "authors", "copying", "notice", "todo", "cargo.lock", "gemfile", "procfile", "vagrantfile",
];

/// The class of `ext` (without the dot, any case).
pub fn classify(ext: &str) -> FileClass {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        FileClass::Image
    } else if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        FileClass::Text
    } else if MEDIA_EXTENSIONS.contains(&ext.as_str()) {
        FileClass::Media
    } else if DIRECT_HEX_EXTENSIONS.contains(&ext.as_str()) {
        FileClass::DirectHex
    } else {
        FileClass::Other
    }
}

/// [`classify`], plus the extensionless text names and the dotfiles git and
/// friends leave behind (`.gitignore`, `.editorconfig`).
pub fn classify_path(path: &Path) -> FileClass {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        return classify(ext);
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    if name.is_empty() {
        return FileClass::Other;
    }
    let bare = name.trim_start_matches('.');
    if TEXT_FILE_NAMES.contains(&name.as_str()) || name.starts_with('.') || bare == "gitignore" || bare == "gitattributes" {
        FileClass::Text
    } else {
        FileClass::Other
    }
}

/// Milliseconds since the UNIX epoch (0 when the OS has no timestamp).
pub fn modified_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Builds the preview for `path`, reading at most the caps documented above.
///
/// Called on the preview worker; the UI thread must not call it directly. The
/// cache is the worker's bounded bitmap LRU (it is also safe to pass a fresh
/// one in tests).
pub fn load_preview(path: &Path, opts: PreviewOptions, cache: &mut PreviewImageCache) -> PreviewKind {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return match e.kind() {
                std::io::ErrorKind::NotFound => PreviewKind::Error(format!("File not found: {}", path.display())),
                std::io::ErrorKind::PermissionDenied => PreviewKind::Error(format!("Access denied: {}", path.display())),
                _ => PreviewKind::Error(format!("Cannot read {}: {e}", path.display())),
            };
        }
    };
    if meta.is_dir() {
        return PreviewKind::Unsupported { reason: "Folders are shown in the list, not previewed.".into(), can_hex: false, file_size: 0 };
    }
    let file_size = meta.len();
    if file_size == 0 {
        return PreviewKind::Unsupported { reason: "Empty file (0 bytes)".into(), can_hex: false, file_size: 0 };
    }
    if opts.force_hex {
        return hex_kind(path);
    }
    match classify_path(path) {
        FileClass::Image => match image::load(path, cache) {
            Ok(img) => PreviewKind::Image {
                bmp: img.bmp,
                orig_w: img.orig_w,
                orig_h: img.orig_h,
                format: img.format,
                file_size,
                modified: modified_ms(path),
            },
            Err(PreviewError::NotFound(_) | PreviewError::AccessDenied(_)) => PreviewKind::Error(format!("Cannot open {}", path.display())),
            Err(e) => PreviewKind::Unsupported { reason: format!("{e}"), can_hex: true, file_size },
        },
        FileClass::Text => match text::load(path, opts.syntax_highlight) {
            Ok(t) => PreviewKind::Text {
                lines: t.lines,
                language: t.language,
                encoding: t.encoding,
                total_bytes: t.total_bytes,
                truncated: t.truncated,
            },
            Err(PreviewError::Binary) => PreviewKind::Unsupported {
                reason: "Binary content (NUL bytes) — no text preview.".into(),
                can_hex: true,
                file_size,
            },
            Err(PreviewError::NotFound(_) | PreviewError::AccessDenied(_)) => PreviewKind::Error(format!("Cannot read {}", path.display())),
            Err(e) => PreviewKind::Error(format!("{e}")),
        },
        FileClass::Media => match media::load(path) {
            Ok(m) => PreviewKind::Media { kind: m.kind, rows: m.rows, file_size },
            Err(PreviewError::NotFound(_) | PreviewError::AccessDenied(_)) => PreviewKind::Error(format!("Cannot read {}", path.display())),
            Err(e) => PreviewKind::Unsupported { reason: format!("{e}"), can_hex: true, file_size },
        },
        FileClass::DirectHex => {
            if opts.direct_hex_binaries {
                hex_kind(path)
            } else {
                PreviewKind::Unsupported { reason: "Binary file — open it as hex.".into(), can_hex: true, file_size }
            }
        }
        FileClass::Other => PreviewKind::Unsupported {
            reason: format!("No preview available for {} files.", extension_label(path)),
            can_hex: true,
            file_size,
        },
    }
}

fn hex_kind(path: &Path) -> PreviewKind {
    match hex::load(path) {
        Ok(h) => PreviewKind::Hex { bytes: h.bytes, total_bytes: h.total_bytes, truncated: h.truncated, magic: h.magic },
        Err(PreviewError::NotFound(_) | PreviewError::AccessDenied(_)) => PreviewKind::Error(format!("Cannot read {}", path.display())),
        Err(e) => PreviewKind::Error(format!("{e}")),
    }
}

/// `PNG` for `a.png`, `file` for an extensionless path.
fn extension_label(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if !ext.is_empty() => ext.to_ascii_uppercase(),
        _ => "this kind of".into(),
    }
}

/// Path key used by the image cache (`C:\A.PNG` and `c:\a.png` hit together).
pub fn path_key(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-preview-mod-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn extensions_are_classified() {
        assert_eq!(classify("PNG"), FileClass::Image);
        assert_eq!(classify(".jpeg"), FileClass::Image);
        assert_eq!(classify("rs"), FileClass::Text);
        assert_eq!(classify("Json"), FileClass::Text);
        assert_eq!(classify("mp4"), FileClass::Media);
        assert_eq!(classify("m4a"), FileClass::Media);
        assert_eq!(classify("bin"), FileClass::DirectHex);
        assert_eq!(classify("exe"), FileClass::DirectHex);
        assert_eq!(classify("zip"), FileClass::Other);
        assert_eq!(classify("iso"), FileClass::Other);
        assert_eq!(classify_path(Path::new("C:\\w\\Makefile")), FileClass::Text);
        assert_eq!(classify_path(Path::new("C:\\w\\.gitignore")), FileClass::Text);
        assert_eq!(classify_path(Path::new("C:\\w\\README")), FileClass::Text);
        assert_eq!(classify_path(Path::new("C:\\w\\blob")), FileClass::Other);
    }

    #[test]
    fn text_lines_are_capped_and_their_spans_cover_the_text() {
        let line = TextLine::plain("héllo");
        assert_eq!(line.chars(), 5);
        assert_eq!(line.spans, [Span { len: 5, kind: TokenKind::Plain }]);
        let long = "x".repeat(MAX_LINE_CHARS + 100);
        let line = TextLine::plain(long);
        assert_eq!(line.chars(), MAX_LINE_CHARS);
        assert_eq!(line.spans.iter().map(|s| s.len as usize).sum::<usize>(), MAX_LINE_CHARS);
    }

    #[test]
    fn an_empty_file_is_unsupported_without_hex() {
        let d = temp_dir("empty");
        let p = d.join("a.txt");
        std::fs::write(&p, b"").unwrap();
        let mut cache = PreviewImageCache::default();
        match load_preview(&p, PreviewOptions::default(), &mut cache) {
            PreviewKind::Unsupported { can_hex, file_size, .. } => {
                assert!(!can_hex);
                assert_eq!(file_size, 0);
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_file_is_an_error_and_a_folder_is_unsupported() {
        let d = temp_dir("missing");
        let mut cache = PreviewImageCache::default();
        assert!(matches!(load_preview(&d.join("nope.txt"), PreviewOptions::default(), &mut cache), PreviewKind::Error(_)));
        assert!(matches!(load_preview(&d, PreviewOptions::default(), &mut cache), PreviewKind::Unsupported { .. }));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn direct_hex_follows_the_option_and_force_hex_always_wins() {
        let d = temp_dir("hex");
        let p = d.join("a.bin");
        std::fs::write(&p, [0x4d, 0x5a, 0x90, 0x00]).unwrap();
        let mut cache = PreviewImageCache::default();
        assert!(matches!(load_preview(&p, PreviewOptions::default(), &mut cache), PreviewKind::Hex { magic: Some(_), .. }));
        let off = PreviewOptions { direct_hex_binaries: false, ..PreviewOptions::default() };
        match load_preview(&p, off, &mut cache) {
            PreviewKind::Unsupported { can_hex, .. } => assert!(can_hex),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        let txt = d.join("a.txt");
        std::fs::write(&txt, b"hello").unwrap();
        assert!(matches!(load_preview(&txt, PreviewOptions { force_hex: true, ..PreviewOptions::default() }, &mut cache), PreviewKind::Hex { .. }));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn text_is_previewed_and_binary_text_offers_hex() {
        let d = temp_dir("text");
        let txt = d.join("a.rs");
        std::fs::write(&txt, b"fn main() {}\n").unwrap();
        let mut cache = PreviewImageCache::default();
        match load_preview(&txt, PreviewOptions::default(), &mut cache) {
            PreviewKind::Text { language, encoding, truncated, .. } => {
                assert_eq!(language, "Rust");
                assert_eq!(encoding, "UTF-8");
                assert!(!truncated);
            }
            other => panic!("expected Text, got {other:?}"),
        }
        let bin = d.join("b.txt");
        std::fs::write(&bin, [0u8, 1, 2, 3]).unwrap();
        match load_preview(&bin, PreviewOptions::default(), &mut cache) {
            PreviewKind::Unsupported { can_hex, .. } => assert!(can_hex),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_extensions_are_unsupported_with_hex() {
        let d = temp_dir("other");
        let p = d.join("a.iso");
        std::fs::write(&p, [1u8, 2, 3]).unwrap();
        let mut cache = PreviewImageCache::default();
        match load_preview(&p, PreviewOptions::default(), &mut cache) {
            PreviewKind::Unsupported { reason, can_hex, .. } => {
                assert!(can_hex);
                assert!(reason.contains("ISO"), "{reason}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
