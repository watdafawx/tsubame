//! Unit tests for the parts that do not need a real directory: path shaping,
//! batching policy, error mapping. The disk fixtures live in `tests/enumerate.rs`.

use super::*;
use std::path::Path;

fn wide_string(units: &[u16]) -> String {
    String::from_utf16(&units[..units.len() - 1]).unwrap()
}

#[test]
fn extended_paths_are_verbatim() {
    let p = extended_wide(Path::new(r"C:\a\b")).unwrap();
    assert_eq!(wide_string(&p), r"\\?\C:\a\b");

    // Trailing separators are trimmed, drive roots are kept.
    assert_eq!(wide_string(&extended_wide(Path::new(r"C:\a\b\")).unwrap()), r"\\?\C:\a\b");
    assert_eq!(wide_string(&extended_wide(Path::new(r"C:\")).unwrap()), r"\\?\C:\");

    // Already verbatim, and UNC gets the `UNC` form.
    assert_eq!(wide_string(&extended_wide(Path::new(r"\\?\C:\a")).unwrap()), r"\\?\C:\a");
    assert_eq!(
        wide_string(&extended_wide(Path::new(r"\\server\share\dir")).unwrap()),
        r"\\?\UNC\server\share\dir"
    );

    // Relative paths are made absolute first, so `\\?\` stays legal.
    let rel = extended_wide(Path::new("sub/dir")).unwrap();
    assert!(wide_string(&rel).starts_with(r"\\?\"));
    assert!(wide_string(&rel).ends_with(r"sub\dir"));
}

#[test]
fn find_pattern_appends_star_once() {
    assert_eq!(wide_string(&search_pattern_wide(Path::new(r"C:\a")).unwrap()), r"\\?\C:\a\*");
    assert_eq!(wide_string(&search_pattern_wide(Path::new(r"C:\a\")).unwrap()), r"\\?\C:\a\*");
    assert_eq!(wide_string(&search_pattern_wide(Path::new(r"C:\")).unwrap()), r"\\?\C:\*");
}

#[test]
fn dots_are_recognised() {
    assert!(is_dot(&[b'.' as u16]));
    assert!(is_dot(&[b'.' as u16, b'.' as u16]));
    assert!(!is_dot(&[b'.' as u16, b'x' as u16]));
    assert!(!is_dot(&[b'x' as u16]));
    assert!(!is_dot(&[]));
}

#[test]
fn first_batch_flushes_at_256_and_later_at_4096() {
    let mut b = Batcher::new(7);
    for _ in 0..FIRST_BATCH - 1 {
        b.recs.push(RawEntry::default());
        assert!(!b.due(), "first batch must wait for {FIRST_BATCH} entries");
    }
    b.recs.push(RawEntry::default());
    assert!(b.due());

    let batch = b.take();
    assert_eq!(batch.recs.len(), FIRST_BATCH);
    assert_eq!(batch.generation, 7);
    // After the first flush the cap is BATCH_MAX, not one query buffer's worth.
    for _ in 0..BATCH_MAX - 1 {
        b.recs.push(RawEntry::default());
    }
    assert!(!b.due(), "must not flush one entry early");
    b.recs.push(RawEntry::default());
    assert!(b.due());
    assert_eq!(b.take().recs.len(), BATCH_MAX);
}

#[test]
fn small_batches_flush_after_the_interval() {
    let mut b = Batcher::new(1);
    b.recs.push(RawEntry::default());
    let _ = b.take(); // mark the first batch as sent
    b.recs.push(RawEntry::default());
    // The clock is only read every 256 entries; before that the answer is "no".
    assert!(!b.due(), "clock must not be read on every entry");
    std::thread::sleep(BATCH_INTERVAL + Duration::from_millis(2));
    for _ in 0..254 {
        assert!(!b.due());
    }
    assert!(b.due(), "4 ms of new entries must flush a partial batch");
}

#[test]
fn batch_names_are_indexed_by_offset() {
    let mut b = Batch::default();
    b.names.extend_from_slice(&[b'a' as u16, b'b' as u16]);
    b.recs.push(RawEntry { name_off: 0, name_len: 1, ..Default::default() });
    b.recs.push(RawEntry { name_off: 1, name_len: 1, ..Default::default() });
    assert_eq!(b.name(0), &[b'a' as u16]);
    assert_eq!(b.name(1), &[b'b' as u16]);
    // Out-of-range reads clamp instead of panicking.
    b.recs.push(RawEntry { name_off: 1, name_len: 99, ..Default::default() });
    assert_eq!(b.name(2), &[b'b' as u16], "over-long names are clamped");
    assert_eq!(b.name(9), &[] as &[u16]);
    assert_eq!(b.iter().count(), 3);
}

#[test]
fn raw_entry_is_64_bytes() {
    assert_eq!(mem::size_of::<RawEntry>(), 64);
}

#[test]
fn error_kinds() {
    assert_eq!(Win32Error::win32(5).kind(), ErrorKind::AccessDenied);
    assert_eq!(Win32Error::win32(2).kind(), ErrorKind::NotFound);
    assert_eq!(Win32Error::win32(3).kind(), ErrorKind::NotFound);
    assert_eq!(Win32Error::win32(267).kind(), ErrorKind::NotADirectory);
    assert_eq!(Win32Error::win32(87).kind(), ErrorKind::Other);

    // NTSTATUS mapping, including the unsupported information class.
    assert_eq!(Win32Error::from_nt(0xC000_0022u32 as i32).code, 5);
    assert_eq!(Win32Error::from_nt(0xC000_003Au32 as i32).code, 3);
    assert_eq!(Win32Error::from_nt(0xC000_0103u32 as i32).code, 267);
    assert!(Win32Error::from_nt(0xC000_0003u32 as i32).is_unsupported());
    assert_eq!(
        Win32Error::from_nt(0xC000_0003u32 as i32).kind(),
        ErrorKind::Unsupported
    );
    assert!(!Win32Error::from_nt(0xC000_0022u32 as i32).is_unsupported());

    let text = Win32Error::win32(5).to_string();
    assert!(text.contains("access denied"), "{text}");
}

#[test]
fn cancel_token_generations() {
    let token = CancelToken::new();
    assert_eq!(token.generation(), 0);
    let first = token.next_generation();
    assert!(token.is_current(first));
    let second = token.next_generation();
    assert!(!token.is_current(first), "a new request cancels the previous one");
    assert!(token.is_current(second));
    token.cancel();
    assert!(!token.is_current(second));
    assert!(token.next_generation() > second);
}
