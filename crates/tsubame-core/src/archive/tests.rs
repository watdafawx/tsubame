//! Archive VFS tests: real archives in a temp folder, one per format.
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::read::{open_index, read_capped, zip_time, zip_wall_seconds};
use super::*;

/// A unique folder under the system temp dir, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "tsubame-archive-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn write_zip(path: &Path, files: &[(&str, &[u8])], dirs: &[&str]) {
    let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
    for dir in dirs {
        writer
            .add_directory(*dir, zip::write::SimpleFileOptions::default().unix_permissions(0o755))
            .unwrap();
    }
    for (i, (name, data)) in files.iter().enumerate() {
        // alternate stored and deflated so both paths are covered
        let method = if i % 2 == 0 {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        };
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default().compression_method(method))
            .unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
}

fn write_tar(path: &Path, files: &[(&str, &[u8])], dirs: &[&str], gzip: bool) {
    let file = File::create(path).unwrap();
    if gzip {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(file, flate2::Compression::default()));
        fill_tar(&mut builder, files, dirs);
        builder.into_inner().unwrap().finish().unwrap();
    } else {
        let mut builder = tar::Builder::new(file);
        fill_tar(&mut builder, files, dirs);
        builder.into_inner().unwrap();
    }
}

fn fill_tar<W: Write>(builder: &mut tar::Builder<W>, files: &[(&str, &[u8])], dirs: &[&str]) {
    const MTIME: u64 = 1_700_000_000;
    for dir in dirs {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_mtime(MTIME);
        header.set_cksum();
        builder.append_data(&mut header, format!("{dir}/"), &b""[..]).unwrap();
    }
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(MTIME);
        header.set_cksum();
        builder.append_data(&mut header, name, *data).unwrap();
    }
}

/// 2023-11-14 22:13:20 UTC as FILETIME ticks, the clock 7z stores.
const SEVENZ_TIME: u64 = 133_444_736_000_000_000;

fn sevenz_entry(mut entry: sevenz_rust2::ArchiveEntry) -> sevenz_rust2::ArchiveEntry {
    entry.has_last_modified_date = true;
    entry.last_modified_date = sevenz_rust2::NtTime::new(SEVENZ_TIME);
    entry
}

fn write_7z(path: &Path, files: &[(&str, &[u8])], dirs: &[&str]) {
    let mut writer = sevenz_rust2::ArchiveWriter::create(path).unwrap();
    for dir in dirs {
        writer
            .push_archive_entry(
                sevenz_entry(sevenz_rust2::ArchiveEntry::new_directory(dir)),
                None::<&[u8]>,
            )
            .unwrap();
    }
    for (name, data) in files {
        writer
            .push_archive_entry(sevenz_entry(sevenz_rust2::ArchiveEntry::new_file(name)), Some(*data))
            .unwrap();
    }
    writer.finish().unwrap();
}

fn names(entries: &[ArchiveEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.name.as_str()).collect()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// Every file under `root`, relative, `/` separated: proves nothing escaped and
/// nothing half-written stayed behind.
fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn path_model_round_trips() {
    assert!(is_archive_path(r"C:\a.zip::dir/file.txt"));
    assert!(is_archive_path(r"C:\a.zip::"));
    assert!(!is_archive_path(r"C:\a.zip"));
    assert!(is_archive_file(r"C:\a.zip"));
    assert!(is_archive_file(r"C:\a.zip::dir/file.txt"));
    for name in ["x.TAR.GZ", "x.tgz", "x.tar", "x.7z", "x.ZIP"] {
        assert!(is_archive_file(name), "{name}");
    }
    for name in ["x.rar", "x.gz", "x.txt", ""] {
        assert!(!is_archive_file(name), "{name}");
    }

    let joined = join_archive_path(r"C:\a.zip", "dir/file.txt");
    assert_eq!(joined, r"C:\a.zip::dir/file.txt");
    assert_eq!(split_archive_path(&joined), Some((r"C:\a.zip", "dir/file.txt")));
    assert_eq!(join_archive_path(r"C:\a.zip", ""), r"C:\a.zip");
    assert_eq!(join_archive_path(r"C:\a.zip", "/dir/"), r"C:\a.zip::dir");
    assert_eq!(split_archive_path(r"C:\a.zip"), None);
    // nested archives split at the last separator
    assert_eq!(
        split_archive_path(r"C:\a.zip::x/b.zip::in.txt"),
        Some((r"C:\a.zip::x/b.zip", "in.txt"))
    );

    assert_eq!(parent_path(r"C:\a.zip::dir/file.txt").as_deref(), Some(r"C:\a.zip::dir"));
    assert_eq!(parent_path(r"C:\a.zip::dir").as_deref(), Some(r"C:\a.zip"));
    assert_eq!(parent_path(r"C:\a.zip").as_deref(), Some(r"C:\"));
    assert_eq!(parent_path(r"C:\dir\a.zip::").as_deref(), Some(r"C:\dir"));
    assert_eq!(parent_path(r"C:\").as_deref(), None);
    assert_eq!(parent_path("a.zip"), None);
    assert_eq!(host_archive_path(r"C:\a.zip::dir"), r"C:\a.zip");
    assert_eq!(host_archive_path(r"C:\a.zip"), r"C:\a.zip");
}

#[test]
fn sanitizer_rejects_traversal_and_absolute() {
    for bad in [
        "../evil.txt",
        r"..\evil.txt",
        "a/../../b",
        r"a\b\..\c",
        "/etc/passwd",
        r"\evil",
        r"C:\evil",
        "C:evil",
        "dir/NUL",
        "nul.txt",
        "com1",
        "lpt9.log",
        "file.txt:ads",
        "dir/evil.txt.",
        "dir/trailing ",
        "...",
        "a\0b",
    ] {
        assert!(sanitize_inner_path(bad).is_err(), "{bad:?} must be rejected");
    }
    for (raw, want) in [
        ("a/b/c.txt", "a/b/c.txt"),
        (r"a\b\c.txt", "a/b/c.txt"),
        ("a//b/", "a/b"),
        ("./a", "a"),
        ("a/./b", "a/b"),
        ("", ""),
        (".", ""),
        ("dir/", "dir"),
        ("Ä/ß.txt", "Ä/ß.txt"),
        ("aux2/x", "aux2/x"),
    ] {
        assert_eq!(sanitize_inner_path(raw).unwrap(), want, "{raw:?}");
    }
    assert!(sanitize_entry_path("").is_err());
    assert_eq!(sanitize_entry_path("a/b.txt").unwrap(), PathBuf::from("a").join("b.txt"));
    assert!(sanitize_entry_path("../b.txt").is_err());
}

#[test]
fn zip_lists_nested_and_synthesized_folders() {
    let tmp = TempDir::new("zip-list");
    let zip_path = tmp.join("a.zip");
    write_zip(
        &zip_path,
        &[
            ("a/b/c.txt", b"hello"),
            ("a/b/d.txt", b"world!"),
            ("top.txt", b"root"),
            ("empty.txt", b""),
        ],
        &["real/"],
    );

    let root = list_dir(&zip_path, "").unwrap();
    assert_eq!(names(&root), ["a", "real", "empty.txt", "top.txt"], "folders first, then names");
    let a = &root[0];
    assert!(a.is_dir && a.size == 0 && a.compressed_size == 0);
    assert_eq!(a.inner_path, "a", "synthesized folder, no trailing slash");
    assert!(root[1].is_dir, "an explicit folder entry is still a folder");
    assert!(a.modified > 0, "synthesized folder takes its first child's time");

    let nested = list_dir(&zip_path, "a/b").unwrap();
    assert_eq!(names(&nested), ["c.txt", "d.txt"]);
    assert_eq!(nested[0].size, 5);
    assert!(nested[0].compressed_size > 0, "deflated entries report their packed size");
    assert_eq!(nested[0].inner_path, "a/b/c.txt");
    assert!(nested[0].modified > 0);

    // a virtual path is accepted as the inner directory too
    assert_eq!(list_dir(&zip_path, "a\\b").unwrap().len(), 2);
    assert!(matches!(list_dir(&zip_path, "nope"), Err(ArchiveError::NotFound(_))));
    assert!(
        matches!(list_dir(&zip_path, "top.txt"), Err(ArchiveError::NotFound(_))),
        "a file is not a folder"
    );

    let index = open_index(&zip_path).unwrap();
    assert_eq!((index.file_count(), index.dir_count()), (4, 3), "a, a/b and real");
    assert_eq!(index.total_size(), 15);
    assert_eq!(index.skipped(), 0);
    assert_eq!(index.format(), ArchiveFormat::Zip);
    assert!(index.memory_bytes() > 0);
    assert_eq!(index.size_under("a"), 11);
    assert_eq!(index.files_under("").count(), 4);
}

#[test]
fn index_cache_hits_misses_and_evicts() {
    let tmp = TempDir::new("cache");
    let mut paths = Vec::new();
    for i in 0..3 {
        let path = tmp.join(format!("a{i}.zip"));
        write_zip(&path, &[("dir/file.txt", b"x")], &[]);
        paths.push(path);
    }

    let mut cache = ArchiveIndexCache::with_limits(2, ARCHIVE_CACHE_BYTES);
    for path in &paths {
        assert_eq!(cache.get_or_load(path).unwrap().len(), 2, "folder + file");
    }
    assert_eq!((cache.len(), cache.hits(), cache.misses()), (2, 0, 3));
    assert!(!cache.contains(&paths[0]), "least recently used went first");
    assert!(cache.contains(&paths[2]));

    // a hit returns the very same index instead of parsing again
    let first = cache.get_or_load(&paths[2]).unwrap();
    let again = cache.get_or_load(&paths[2]).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &again));
    assert_eq!((cache.hits(), cache.misses()), (2, 3));

    // LRU, not FIFO: touching the older index keeps it and the untouched one goes
    let mut lru = ArchiveIndexCache::with_limits(2, ARCHIVE_CACHE_BYTES);
    lru.get_or_load(&paths[0]).unwrap();
    lru.get_or_load(&paths[1]).unwrap();
    lru.get_or_load(&paths[0]).unwrap(); // hit: now the newest
    lru.get_or_load(&paths[2]).unwrap();
    assert!(lru.contains(&paths[0]) && lru.contains(&paths[2]));
    assert!(!lru.contains(&paths[1]), "the untouched index was evicted");

    // (path, length, mtime) key: a rewritten archive is re-read
    let mut cache = ArchiveIndexCache::with_limits(2, ARCHIVE_CACHE_BYTES);
    cache.get_or_load(&paths[0]).unwrap();
    let misses = cache.misses();
    write_zip(&paths[0], &[("dir/file.txt", b"x"), ("more.txt", b"yy")], &[]);
    let fresh = cache.get_or_load(&paths[0]).unwrap();
    assert_eq!(fresh.len(), 3);
    assert!(cache.misses() > misses);

    // hard byte cap: an index over it is handed out but never kept
    let mut tiny = ArchiveIndexCache::with_limits(8, 1);
    assert_eq!(tiny.get_or_load(&paths[1]).unwrap().len(), 2);
    assert!(tiny.is_empty());
    assert_eq!(tiny.memory_bytes(), 0);

    // invalidate and clear drop what they say
    let mut cache = ArchiveIndexCache::with_limits(4, ARCHIVE_CACHE_BYTES);
    cache.get_or_load(&paths[0]).unwrap();
    cache.invalidate(&paths[0]);
    assert!(!cache.contains(&paths[0]) && cache.memory_bytes() == 0);
    cache.get_or_load(&paths[1]).unwrap();
    cache.clear();
    assert!(cache.is_empty() && cache.memory_bytes() == 0);
    // the public entry point goes through the same cache
    assert_eq!(names(&list_dir_cached(&mut cache, &paths[2], "dir").unwrap()), ["file.txt"]);
    assert!(cache.contains(&paths[2]));
}

#[test]
fn zip_extracts_files_folders_and_whole_archive() {
    let tmp = TempDir::new("zip-extract");
    let zip_path = tmp.join("a.zip");
    write_zip(
        &zip_path,
        &[
            ("a/b/c.txt", b"hello"),
            ("a/b/d.txt", b"world!"),
            ("top.txt", b"root"),
        ],
        &["empty/"],
    );

    // one folder, recursively
    let out = tmp.join("folder");
    let mut calls = 0;
    let mut reported = 0;
    let report = extract_entries(&zip_path, &["a/b"], &out, &no_cancel(), |done, total| {
        calls += 1;
        assert!(done <= total);
        reported = total;
    })
    .unwrap();
    assert_eq!((report.files, report.dirs, report.bytes), (2, 0, 11));
    assert_eq!(calls, 3, "one initial call plus one per chunk/file");
    assert_eq!(reported, 11);
    assert_eq!(read(&out.join("a/b/c.txt")), "hello");
    assert_eq!(read(&out.join("a/b/d.txt")), "world!");
    assert_eq!(tree(&out), ["a/b/c.txt", "a/b/d.txt"], "nothing else came along");

    // one file
    let out = tmp.join("single");
    let report = extract_entries(&zip_path, &["top.txt"], &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!((report.files, report.dirs), (1, 0));
    assert_eq!(tree(&out), ["top.txt"]);

    // the whole archive
    let out = tmp.join("all");
    let report = extract_all(&zip_path, &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!((report.files, report.dirs, report.bytes), (3, 1, 15));
    assert!(out.join("empty").is_dir());
    assert_eq!(tree(&out), ["a/b/c.txt", "a/b/d.txt", "top.txt"]);
    // the root selection means everything; an empty selection means nothing
    let out = tmp.join("root-selection");
    let report = extract_entries(&zip_path, &[""], &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!(report.files, 3);
    let out = tmp.join("nothing");
    let report = extract_entries(&zip_path, &[], &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!(report, ExtractReport::default());
    // a missing selection is an error, not a silent no-op
    assert!(matches!(
        extract_entries(&zip_path, &["a/missing.txt"], &out, &no_cancel(), |_, _| {}),
        Err(ArchiveError::NotFound(_))
    ));
}

#[test]
fn extract_overwrite_modes() {
    let tmp = TempDir::new("overwrite");
    let zip_path = tmp.join("a.zip");
    write_zip(&zip_path, &[("top.txt", b"from archive")], &[]);

    // Replace (default)
    let out = tmp.join("replace");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("top.txt"), b"mine").unwrap();
    let report = extract_all(&zip_path, &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!(report.files, 1);
    assert_eq!(read(&out.join("top.txt")), "from archive");

    // Skip
    fs::write(out.join("top.txt"), b"mine").unwrap();
    let options = ExtractOptions { overwrite: Overwrite::Skip, strict_paths: false };
    let report = extract_all_with(&zip_path, &out, &options, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!((report.files, report.skipped), (0, 1));
    assert_eq!(read(&out.join("top.txt")), "mine");

    // Fail
    let options = ExtractOptions { overwrite: Overwrite::Fail, strict_paths: false };
    assert!(matches!(
        extract_all_with(&zip_path, &out, &options, &no_cancel(), |_, _| {}),
        Err(ArchiveError::Exists(_))
    ));
    assert_eq!(read(&out.join("top.txt")), "mine", "failure leaves the old file alone");

    // Replace clears the read-only bit instead of failing
    let mut permissions = fs::metadata(out.join("top.txt")).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(out.join("top.txt"), permissions).unwrap();
    extract_all(&zip_path, &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!(read(&out.join("top.txt")), "from archive");
}

#[test]
fn folder_file_collisions_are_errors() {
    let tmp = TempDir::new("collision");
    let zip_path = tmp.join("a.zip");
    write_zip(&zip_path, &[("a/b.txt", b"x")], &[]);

    // a file where the archive needs a folder
    let out = tmp.join("file-blocks-folder");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("a"), b"not a folder").unwrap();
    assert!(matches!(
        extract_all(&zip_path, &out, &no_cancel(), |_, _| {}),
        Err(ArchiveError::Exists(_))
    ));
    assert_eq!(read(&out.join("a")), "not a folder", "the blocking file is untouched");

    // a folder where the archive needs a file
    let out = tmp.join("folder-blocks-file");
    fs::create_dir_all(out.join("a/b.txt")).unwrap();
    assert!(matches!(
        extract_all(&zip_path, &out, &no_cancel(), |_, _| {}),
        Err(ArchiveError::Exists(_))
    ));
    assert!(out.join("a/b.txt").is_dir());
}

#[test]
fn zip_slip_is_rejected() {
    let tmp = TempDir::new("zip-slip");
    let zip_path = tmp.join("evil.zip");
    write_zip(
        &zip_path,
        &[
            ("../evil.txt", b"pwn"),
            (r"..\..\evil2.txt", b"pwn"),
            ("ok.txt", b"fine"),
        ],
        &[],
    );

    // the hostile names never enter the index
    let index = open_index(&zip_path).unwrap();
    assert_eq!(index.skipped(), 2);
    assert_eq!(names(&list_dir(&zip_path, "").unwrap()), ["ok.txt"]);

    // and extraction keeps them off the disk, reporting them instead
    let out = tmp.join("out/deep");
    let report = extract_all(&zip_path, &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!((report.files, report.skipped), (1, 0));
    assert_eq!(report.unsafe_paths, ["../evil.txt", r"..\..\evil2.txt"]);
    assert_eq!(tree(&out), ["ok.txt"]);
    for escaped in ["evil.txt", "evil2.txt"] {
        assert!(!tmp.join(escaped).exists(), "{escaped} escaped the destination");
        assert!(!tmp.join("out").join(escaped).exists(), "{escaped} escaped the destination");
    }

    // strict mode turns the first unsafe name into an error
    let options = ExtractOptions { overwrite: Overwrite::Replace, strict_paths: true };
    let before = tree(&out);
    assert!(matches!(
        extract_all_with(&zip_path, &out, &options, &no_cancel(), |_, _| {}),
        Err(ArchiveError::UnsafePath(_))
    ));
    assert_eq!(tree(&out), before);

    // and the previewer refuses it before opening anything
    assert!(matches!(
        read_entry_bytes(&zip_path, "../evil.txt", 100),
        Err(ArchiveError::UnsafePath(_))
    ));
}

#[test]
fn tar_and_targz_list_and_extract() {
    let tmp = TempDir::new("tar");
    for (name, gzip) in [("a.tar", false), ("a.tar.gz", true), ("a.tgz", true)] {
        let path = tmp.join(name);
        write_tar(
            &path,
            &[("x/y.txt", b"one"), ("x/z.txt", b"two"), ("root.txt", b"three")],
            &["empty"],
            gzip,
        );
        assert_eq!(
            ArchiveFormat::detect(&path).unwrap(),
            if gzip { ArchiveFormat::TarGz } else { ArchiveFormat::Tar }
        );

        let root = list_dir(&path, "").unwrap();
        assert_eq!(names(&root), ["empty", "x", "root.txt"], "{name}");
        assert!(root.iter().all(|e| e.compressed_size == 0), "tar has no per-entry packed size");
        assert!(root[0].modified > 0);

        let nested = list_dir(&path, "x").unwrap();
        assert_eq!(names(&nested), ["y.txt", "z.txt"]);
        assert_eq!(nested[0].size, 3);

        let out = tmp.join(format!("out-{name}"));
        let report = extract_all(&path, &out, &no_cancel(), |_, _| {}).unwrap();
        assert_eq!((report.files, report.dirs, report.bytes), (3, 1, 11), "{name}");
        assert_eq!(read(&out.join("x/y.txt")), "one");
        assert!(out.join("empty").is_dir());

        let report = extract_entries(&path, &["x/y.txt"], &tmp.join(format!("one-{name}")), &no_cancel(), |_, _| {}).unwrap();
        assert_eq!((report.files, report.bytes), (1, 3), "{name}");
    }
}

#[test]
fn sevenz_lists_and_extracts() {
    let tmp = TempDir::new("7z");
    let path = tmp.join("a.7z");
    write_7z(
        &path,
        &[("a/b.txt", b"seven"), ("a/c.txt", b"zip"), ("top.txt", b"x")],
        &["empty"],
    );
    assert_eq!(ArchiveFormat::detect(&path).unwrap(), ArchiveFormat::SevenZ);

    let root = list_dir(&path, "").unwrap();
    assert_eq!(names(&root), ["a", "empty", "top.txt"]);
    assert_eq!(root[0].inner_path, "a");
    assert!(root[0].is_dir && root[0].size == 0);
    let nested = list_dir(&path, "a").unwrap();
    assert_eq!(names(&nested), ["b.txt", "c.txt"]);
    assert_eq!(nested[0].size, 5);
    assert_eq!(nested[0].modified, SEVENZ_TIME as i64, "7z times are already FILETIME ticks");

    let bytes = read_entry_bytes(&path, "a/b.txt", 100).unwrap();
    assert_eq!(bytes, b"seven");

    let out = tmp.join("all");
    let report = extract_all(&path, &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!((report.files, report.dirs, report.bytes), (3, 1, 9));
    assert_eq!(read(&out.join("a/b.txt")), "seven");
    assert_eq!(read(&out.join("top.txt")), "x");

    // one file out of a solid archive: the decoder walks the block from the start
    let out = tmp.join("single");
    let report = extract_entries(&path, &["top.txt"], &out, &no_cancel(), |_, _| {}).unwrap();
    assert_eq!((report.files, report.bytes), (1, 1));
    assert_eq!(tree(&out), ["top.txt"]);
}

#[test]
fn read_entry_bytes_respects_the_cap() {
    let tmp = TempDir::new("read-entry");
    let zip_path = tmp.join("a.zip");
    write_zip(&zip_path, &[("small.txt", b"abc"), ("big.txt", &[7u8; 4096])], &[]);
    let tar_path = tmp.join("a.tar");
    write_tar(&tar_path, &[("small.txt", b"abc"), ("big.txt", &[7u8; 4096])], &[], false);
    let sevenz_path = tmp.join("a.7z");
    write_7z(&sevenz_path, &[("small.txt", b"abc"), ("big.txt", &[7u8; 4096])], &[]);

    for path in [&zip_path, &tar_path, &sevenz_path] {
        let tag = path.extension().unwrap().to_string_lossy().into_owned();
        assert_eq!(read_entry_bytes(path, "small.txt", 3).unwrap(), b"abc", "{tag}");
        assert_eq!(read_entry_bytes(path, "small.txt", 4096).unwrap().len(), 3, "{tag}");
        assert!(
            matches!(
                read_entry_bytes(path, "small.txt", 2),
                Err(ArchiveError::TooLarge { size: 3, limit: 2 })
            ),
            "{tag}: over the cap"
        );
        assert!(
            matches!(
                read_entry_bytes(path, "big.txt", 4095),
                Err(ArchiveError::TooLarge { size: 4096, limit: 4095 })
            ),
            "{tag}: over the cap"
        );
        assert_eq!(
            read_entry_bytes(path, "big.txt", 4096).unwrap().len(),
            4096,
            "{tag}: exactly the cap fits"
        );
        assert!(matches!(read_entry_bytes(path, "missing.txt", 10), Err(ArchiveError::NotFound(_))), "{tag}");
        assert!(matches!(read_entry_bytes(path, "", 10), Err(ArchiveError::NotFound(_))), "{tag}");
    }

    // the stream itself is bounded even when a header lies about the size
    let mut out = Vec::new();
    let error = read_capped(&mut &b"0123456789"[..], 4, &mut out).unwrap_err();
    assert!(matches!(error, ArchiveError::TooLarge { limit: 4, .. }));
    assert_eq!(out.len(), 5, "one byte past the cap, then stop");
}

#[test]
fn cancellation_stops_extraction() {
    let tmp = TempDir::new("cancel");
    let zip_path = tmp.join("a.zip");
    let big = vec![0u8; 200 * 1024];
    write_zip(&zip_path, &[("a.bin", big.as_slice()), ("b.bin", big.as_slice()), ("c.bin", big.as_slice())], &[]);

    // already cancelled: nothing is written
    let out = tmp.join("early");
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        extract_all(&zip_path, &out, &cancel, |_, _| {}),
        Err(ArchiveError::Cancelled)
    ));
    assert_eq!(tree(&out), Vec::<String>::new());

    // cancelled mid-file: the partial file is removed and the rest is not written
    let out = tmp.join("mid");
    let cancel = AtomicBool::new(false);
    let mut calls = 0;
    let result = extract_all(&zip_path, &out, &cancel, |done, _| {
        calls += 1;
        if done >= 100 * 1024 {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    assert!(matches!(result, Err(ArchiveError::Cancelled)));
    assert!(calls > 0);
    assert_eq!(tree(&out), Vec::<String>::new(), "the half-written a.bin was cleaned up");

    // extract_entries checks the flag too
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        extract_entries(&zip_path, &["a.bin"], &tmp.join("entries"), &cancel, |_, _| {}),
        Err(ArchiveError::Cancelled)
    ));
}

#[test]
fn format_detection_uses_extension_then_magic() {
    use ArchiveFormat::{SevenZ, Tar, TarGz, Zip};
    for (name, want) in [
        ("a.zip", Zip),
        ("a.ZIP", Zip),
        ("a.tar", Tar),
        ("a.tar.gz", TarGz),
        ("a.TGZ", TarGz),
        ("a.7z", SevenZ),
    ] {
        assert_eq!(ArchiveFormat::from_path(Path::new(name)), Some(want), "{name}");
    }
    for name in ["a.rar", "a.gz", "a.txt", "a"] {
        assert_eq!(ArchiveFormat::from_path(Path::new(name)), None, "{name}");
    }
    assert_eq!(ArchiveFormat::from_magic(b"PK\x03\x04rest"), Some(Zip));
    assert_eq!(ArchiveFormat::from_magic(b"PK\x05\x06"), Some(Zip));
    assert_eq!(ArchiveFormat::from_magic(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]), Some(SevenZ));
    assert_eq!(ArchiveFormat::from_magic(&[0x1F, 0x8B, 0x08, 0x00]), Some(TarGz));
    let mut tar_head = [0u8; 300];
    tar_head[257..262].copy_from_slice(b"ustar");
    assert_eq!(ArchiveFormat::from_magic(&tar_head), Some(Tar));
    assert_eq!(ArchiveFormat::from_magic(b"not an archive"), None);

    // a renamed archive still opens: the magic bytes decide
    let tmp = TempDir::new("magic");
    let disguised = tmp.join("a.dat");
    write_zip(&disguised, &[("inside.txt", b"ok")], &[]);
    assert_eq!(ArchiveFormat::detect(&disguised).unwrap(), Zip);
    assert_eq!(names(&list_dir(&disguised, "").unwrap()), ["inside.txt"]);
    assert!(is_archive_file(r"C:\x.zip::dir"));
    assert!(!is_archive_file(r"C:\x.rar"));

    let junk = tmp.join("junk.bin");
    fs::write(&junk, b"nothing to see").unwrap();
    assert!(matches!(ArchiveFormat::detect(&junk), Err(ArchiveError::Unsupported(_))));
    assert!(matches!(
        list_dir(&tmp.join("missing.zip"), ""),
        Err(ArchiveError::Io(_))
    ));
}

/// Zip DOS times are local wall clock: converting the stored FILETIME back to local
/// time must give the DOS fields again, whatever the machine's offset is.
#[cfg(windows)]
#[test]
fn zip_dos_time_is_read_as_local_time() {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx};

    for (y, mo, d, h, mi, s) in [(2023, 1, 15, 12, 30, 44), (2023, 7, 15, 12, 30, 44), (2008, 3, 1, 9, 5, 2)] {
        let dos = zip::DateTime::from_date_and_time(y, mo, d, h, mi, s).unwrap();
        let ticks = zip_time(dos) as u64;
        let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
        unsafe {
            FileTimeToSystemTime(&ft, &mut utc).unwrap();
            SystemTimeToTzSpecificLocalTimeEx(None, &utc, &mut local).unwrap();
        }
        let got = (local.wYear, local.wMonth as u8, local.wDay as u8, local.wHour as u8, local.wMinute as u8, local.wSecond as u8);
        assert_eq!(got, (y, mo, d, h, mi, s), "round trip through the local zone");
    }
    // the wall-clock helper itself stays zone-free
    let dos = zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).unwrap();
    assert_eq!(zip_wall_seconds(dos), 315_532_800);
}
