//! Sample column plugin: two columns built with `tsubame-plugin-sdk`.
//!
//! * `kind`: Image, Video, Audio, Document, Archive, Code or Other, from the extension. Text cells.
//! * `name-length`: how many characters the file name has. Integer cells, so the
//!   app sorts them as numbers.
//!
//! Neither looks at the file, only at the path text, so the manifest asks for no capabilities.

use tsubame_plugin_sdk::{Cell, Column, plugin};

/// The last path component.
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The extension without its dot, lower case. A leading dot alone (`.gitignore`) is not an extension.
fn extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// The kind a lower-case extension belongs to.
pub fn kind_of(ext: &str) -> &'static str {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "svg" | "heic" => "Image",
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" => "Video",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => "Audio",
        "txt" | "md" | "pdf" | "doc" | "docx" | "odt" | "rtf" | "xls" | "xlsx" | "csv" | "ppt" | "pptx" => "Document",
        "zip" | "7z" | "rar" | "tar" | "gz" | "xz" | "bz2" | "zst" => "Archive",
        "rs" | "c" | "h" | "cpp" | "hpp" | "cs" | "py" | "js" | "ts" | "java" | "go" | "toml" | "json" | "yaml" | "yml" | "html" | "css" => "Code",
        _ => "Other",
    }
}

struct Kind;

impl Column for Kind {
    const ID: &'static str = "kind";

    fn value(path: &str) -> Cell {
        let ext = extension(file_name(path));
        if ext.is_empty() { Cell::Empty } else { Cell::Text(kind_of(&ext).into()) }
    }
}

struct NameLength;

impl Column for NameLength {
    const ID: &'static str = "name-length";

    fn value(path: &str) -> Cell {
        Cell::Int(file_name(path).chars().count() as i64)
    }
}

plugin! {
    id: "dev.tsubame.sample-file-kind",
    version: env!("CARGO_PKG_VERSION"),
    columns: [Kind, NameLength],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert!(matches!(Kind::value(r"C:\pics\Holiday.JPG"), Cell::Text(t) if t == "Image"));
        assert!(matches!(Kind::value(r"C:\x\a.tar.gz"), Cell::Text(t) if t == "Archive"));
        assert!(matches!(Kind::value(r"C:\x\main.rs"), Cell::Text(t) if t == "Code"));
        assert!(matches!(Kind::value(r"C:\x\a.unknownext"), Cell::Text(t) if t == "Other"));
    }

    #[test]
    fn no_extension_is_an_empty_cell() {
        assert!(matches!(Kind::value(r"C:\x\Makefile"), Cell::Empty));
        assert!(matches!(Kind::value(r"C:\x\.gitignore"), Cell::Empty));
        assert!(matches!(Kind::value(r"C:\x\"), Cell::Empty));
    }

    #[test]
    fn name_length_counts_characters_of_the_name_only() {
        assert!(matches!(NameLength::value(r"C:\a\b\abc.txt"), Cell::Int(7)));
        assert!(matches!(NameLength::value(r"C:\日本語.txt"), Cell::Int(7)), "characters, not bytes");
        assert!(matches!(NameLength::value("C:/x/y"), Cell::Int(1)), "forward slashes work too");
    }

    #[test]
    fn a_batch_gives_one_cell_per_path() {
        let paths: Vec<String> = (0..5).map(|i| format!(r"C:\f{i}.zip")).collect();
        assert_eq!(Kind::values(&paths).len(), 5);
    }
}
