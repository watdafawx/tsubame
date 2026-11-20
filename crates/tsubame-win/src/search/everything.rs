//! Voidtools Everything IPC search provider (`WM_COPYDATA`).
//!
//! Talks directly to a running Everything instance without linking
//! `Everything.dll`:
//! 1. Detects Everything via `FindWindowW("EVERYTHING_TASKBAR_NOTIFICATION", None)`.
//! 2. Creates a temporary hidden message-only window (`HWND_MESSAGE`) on the
//!    calling worker thread (never on the UI thread!).
//! 3. Sends an `EVERYTHING_IPC_QUERY2` (`EVERYTHING_IPC_COPYDATA_QUERY2W`)
//!    payload via `SendMessageTimeoutW` requesting name, path, size, and
//!    date modified. Falls back to `EVERYTHING_IPC_COPYDATAQUERYW` (v1) if an
//!    older Everything rejects `QUERY2W`.
//! 4. Pumps messages on the temporary window with a bounded timeout (default 2 s)
//!    and periodic cancellation checks so a hung Everything instance can never
//!    hang Tsubame.
//!
//! Reference protocol: Voidtools Everything SDK `Everything_IPC.h` (`#pragma pack(1)`).

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_TIMEOUT, GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW,
    GWLP_USERDATA, GetWindowLongPtrW, HWND_MESSAGE, MSG, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, RegisterClassW,
    SMTO_ABORTIFHUNG, SMTO_NORMAL, SendMessageTimeoutW, SetWindowLongPtrW, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_COPYDATA, WNDCLASSW,
};
use windows::core::PCWSTR;

use crate::search::{SearchBackend, SearchError, SearchHit, SearchQuery, SearchSummary};
use crate::wide;

// ---- Everything IPC wire constants (from Everything_IPC.h) ------------------

/// Window class of the running Everything notification/IPC receiver.
pub const EVERYTHING_IPC_WNDCLASS: &str = "EVERYTHING_TASKBAR_NOTIFICATION";

/// `COPYDATASTRUCT::dwData` for v1 UTF-16 query (`EVERYTHING_IPC_QUERYW`).
pub const EVERYTHING_IPC_COPYDATAQUERYW: usize = 2;
/// `COPYDATASTRUCT::dwData` for v2 UTF-16 query (`EVERYTHING_IPC_QUERY2` + UTF-16).
pub const EVERYTHING_IPC_COPYDATA_QUERY2W: usize = 18;

// Search flags
pub const EVERYTHING_IPC_MATCHCASE: u32 = 0x0000_0001;
pub const EVERYTHING_IPC_MATCHWHOLEWORD: u32 = 0x0000_0002;
pub const EVERYTHING_IPC_MATCHPATH: u32 = 0x0000_0004;
pub const EVERYTHING_IPC_REGEX: u32 = 0x0000_0008;

// Query2 request flags (packed in ascending bit order per item)
pub const EVERYTHING_IPC_QUERY2_REQUEST_NAME: u32 = 0x0000_0001;
pub const EVERYTHING_IPC_QUERY2_REQUEST_PATH: u32 = 0x0000_0002;
pub const EVERYTHING_IPC_QUERY2_REQUEST_FULL_PATH_AND_NAME: u32 = 0x0000_0004;
pub const EVERYTHING_IPC_QUERY2_REQUEST_EXTENSION: u32 = 0x0000_0008;
pub const EVERYTHING_IPC_QUERY2_REQUEST_SIZE: u32 = 0x0000_0010;
pub const EVERYTHING_IPC_QUERY2_REQUEST_DATE_CREATED: u32 = 0x0000_0020;
pub const EVERYTHING_IPC_QUERY2_REQUEST_DATE_MODIFIED: u32 = 0x0000_0040;
pub const EVERYTHING_IPC_QUERY2_REQUEST_DATE_ACCESSED: u32 = 0x0000_0080;
pub const EVERYTHING_IPC_QUERY2_REQUEST_ATTRIBUTES: u32 = 0x0000_0100;
pub const EVERYTHING_IPC_QUERY2_REQUEST_FILE_LIST_FILE_NAME: u32 = 0x0000_0200;
pub const EVERYTHING_IPC_QUERY2_REQUEST_RUN_COUNT: u32 = 0x0000_0400;
pub const EVERYTHING_IPC_QUERY2_REQUEST_DATE_RUN: u32 = 0x0000_0800;
pub const EVERYTHING_IPC_QUERY2_REQUEST_DATE_RECENTLY_CHANGED: u32 = 0x0000_1000;
pub const EVERYTHING_IPC_QUERY2_REQUEST_HIGHLIGHTED_NAME: u32 = 0x0000_2000;
pub const EVERYTHING_IPC_QUERY2_REQUEST_HIGHLIGHTED_PATH: u32 = 0x0000_4000;
pub const EVERYTHING_IPC_QUERY2_REQUEST_HIGHLIGHTED_FULL_PATH_AND_NAME: u32 = 0x0000_8000;

/// Default request flags for `QUERY2W`: name, path, size, and date modified.
pub const DEFAULT_REQUEST_FLAGS: u32 = EVERYTHING_IPC_QUERY2_REQUEST_NAME
    | EVERYTHING_IPC_QUERY2_REQUEST_PATH
    | EVERYTHING_IPC_QUERY2_REQUEST_SIZE
    | EVERYTHING_IPC_QUERY2_REQUEST_DATE_MODIFIED;

/// Sort by name ascending (`EVERYTHING_IPC_SORT_NAME_ASCENDING`).
pub const EVERYTHING_IPC_SORT_NAME_ASCENDING: u32 = 1;

// Item flags
pub const EVERYTHING_IPC_FOLDER: u32 = 0x0000_0001;
pub const EVERYTHING_IPC_DRIVE: u32 = 0x0000_0002;

/// Fixed header sizes (packed, 1-byte alignment per `Everything_IPC.h`).
const QUERY1_HEADER_SIZE: usize = 20;
const QUERY2_HEADER_SIZE: usize = 28;
const LIST1_HEADER_SIZE: usize = 28;
const ITEM1_SIZE: usize = 12;
const LIST2_HEADER_SIZE: usize = 20;
const ITEM2_SIZE: usize = 8;

/// Cap accepted IPC reply payload size (256 MiB) as a guard against corrupt lengths.
const MAX_REPLY_BYTES: usize = 256 * 1024 * 1024;
/// Default IPC timeout (2 seconds).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
/// Slice used when waiting on the message pump so cancellation is responsive.
const WAIT_SLICE_MS: u32 = 50;

const REPLY_WND_CLASS: &str = "TsubameEverythingIpcReply";

/// Sentinel Everything uses for unknown size / unknown FILETIME.
const EVERYTHING_UNKNOWN_U64: u64 = u64::MAX;

// ---- Public availability probe ----------------------------------------------

/// Find the running Everything IPC window, if any.
fn find_everything_window() -> Option<HWND> {
    let cls = wide(EVERYTHING_IPC_WNDCLASS);
    let hwnd = unsafe { FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()) }.ok()?;
    if hwnd.0.is_null() {
        None
    } else {
        Some(hwnd)
    }
}

/// Returns `true` when Voidtools Everything is running and accepting IPC queries.
pub fn is_available() -> bool {
    find_everything_window().is_some()
}

// ---- Query execution --------------------------------------------------------

/// Run a search against the local Voidtools Everything instance via `WM_COPYDATA`.
///
/// Must be called on a worker thread (creates a temporary `HWND_MESSAGE` window
/// and pumps messages until Everything replies, `timeout` elapses, or `cancel`
/// becomes `true`).
pub fn search<F>(
    query: &SearchQuery,
    timeout: Duration,
    cancel: &AtomicBool,
    mut on_hit: F,
) -> Result<SearchSummary, SearchError>
where
    F: FnMut(SearchHit) -> bool,
{
    let started = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return Err(SearchError::Cancelled);
    }
    let everything_hwnd = find_everything_window().ok_or(SearchError::Unavailable)?;

    if query.max_results == 0 || (!query.include_dirs && !query.include_files) {
        return Ok(SearchSummary {
            backend: SearchBackend::Everything,
            total_hits: 0,
            cancelled: false,
            elapsed: started.elapsed(),
        });
    }

    let search_text = build_search_string(query);
    let mut search_flags = 0u32;
    if query.case_sensitive {
        search_flags |= EVERYTHING_IPC_MATCHCASE;
    }
    if query.regex {
        search_flags |= EVERYTHING_IPC_REGEX;
    }

    // When post-filtering by root or kind in regex mode, ask Everything for
    // enough results without exceeding u32::MAX.
    let needs_post_filter = query.regex
        && (query.root.is_some() || !query.include_dirs || !query.include_files);
    let ipc_max_results = if needs_post_filter {
        (query.max_results.saturating_mul(4)).min(u32::MAX as usize) as u32
    } else {
        query.max_results.min(u32::MAX as usize) as u32
    };

    let reply_id = next_reply_id();
    let slot = RefCell::new(ReplySlot {
        expected_reply_id: reply_id as usize,
        received: false,
        payload: Vec::new(),
    });

    let reply_win = MessageWindow::create(&slot)?;
    let reply_hwnd_u32 = reply_win.hwnd.0 as usize as u32;

    // Try QUERY2W first (Everything 1.4+ with size & date modified).
    let q2_payload = encode_query2_w(
        reply_hwnd_u32,
        reply_id,
        search_flags,
        0,
        ipc_max_results,
        DEFAULT_REQUEST_FLAGS,
        EVERYTHING_IPC_SORT_NAME_ASCENDING,
        &search_text,
    );

    let decoded = match send_and_receive(
        everything_hwnd,
        reply_win.hwnd,
        EVERYTHING_IPC_COPYDATA_QUERY2W,
        &q2_payload,
        timeout,
        cancel,
        &slot,
    ) {
        Ok(bytes) => decode_list2_w(&bytes)?,
        Err(SendRecvError::Rejected) => {
            // Older Everything (1.3) does not recognize QUERY2W; fall back to v1 COPYDATAQUERYW.
            {
                let mut s = slot.borrow_mut();
                s.received = false;
                s.payload.clear();
            }
            let q1_payload = encode_query1_w(
                reply_hwnd_u32,
                reply_id,
                search_flags,
                0,
                ipc_max_results,
                &search_text,
            );
            let bytes = send_and_receive(
                everything_hwnd,
                reply_win.hwnd,
                EVERYTHING_IPC_COPYDATAQUERYW,
                &q1_payload,
                timeout,
                cancel,
                &slot,
            )
            .map_err(SendRecvError::into_search_error)?;
            decode_list1_w(&bytes)?
        }
        Err(e) => return Err(e.into_search_error()),
    };

    // Destroy the message window immediately before streaming hits to caller.
    drop(reply_win);

    let root_norm = query.root.as_deref().map(normalize_prefix_for_cmp);
    let mut delivered = 0usize;
    let mut cancelled = false;

    for hit in decoded.hits {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        if delivered >= query.max_results {
            break;
        }
        if hit.is_dir && !query.include_dirs {
            continue;
        }
        if !hit.is_dir && !query.include_files {
            continue;
        }
        if root_norm
            .as_deref()
            .is_some_and(|root_prefix| !path_within_root(&hit.path, root_prefix))
        {
            continue;
        }
        delivered += 1;
        if !on_hit(hit) {
            cancelled = true;
            break;
        }
    }

    if cancelled {
        return Err(SearchError::Cancelled);
    }

    Ok(SearchSummary {
        backend: SearchBackend::Everything,
        total_hits: delivered,
        cancelled: false,
        elapsed: started.elapsed(),
    })
}

/// Build the Everything query string, applying `root` and file/folder modifiers
/// when regex mode is off.
pub fn build_search_string(query: &SearchQuery) -> String {
    let trimmed = query.text.trim();
    if query.regex {
        return trimmed.to_string();
    }

    let mut parts = Vec::new();
    if let Some(ref root) = query.root {
        let mut s = root.to_string_lossy().replace('/', "\\");
        if !s.is_empty() && !s.contains('"') {
            if !s.ends_with('\\') {
                s.push('\\');
            }
            parts.push(format!("\"{s}\""));
        }
    }
    if query.include_dirs && !query.include_files {
        parts.push("folder:".to_string());
    } else if query.include_files && !query.include_dirs {
        parts.push("file:".to_string());
    }
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    parts.join(" ")
}

fn normalize_prefix_for_cmp(root: &Path) -> String {
    let mut s = root.to_string_lossy().replace('/', "\\").to_lowercase();
    if !s.ends_with('\\') {
        s.push('\\');
    }
    s
}

fn path_within_root(path: &Path, lower_root_with_sep: &str) -> bool {
    let candidate = path.to_string_lossy().replace('/', "\\").to_lowercase();
    if candidate.starts_with(lower_root_with_sep) {
        return true;
    }
    let root_no_sep = lower_root_with_sep.trim_end_matches('\\');
    candidate == root_no_sep
}

// ---- Pure binary protocol encode / decode -----------------------------------

/// Decoded Everything IPC reply list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedList {
    pub total_items: u32,
    pub offset: u32,
    pub hits: Vec<SearchHit>,
}

/// Encode an `EVERYTHING_IPC_QUERY2` + UTF-16LE search string payload
/// (`EVERYTHING_IPC_COPYDATA_QUERY2W`).
#[allow(clippy::too_many_arguments)]
pub fn encode_query2_w(
    reply_hwnd: u32,
    reply_copydata_message: u32,
    search_flags: u32,
    offset: u32,
    max_results: u32,
    request_flags: u32,
    sort_type: u32,
    search_string: &str,
) -> Vec<u8> {
    let utf16: Vec<u16> = search_string
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = Vec::with_capacity(QUERY2_HEADER_SIZE + utf16.len() * 2);
    buf.extend_from_slice(&reply_hwnd.to_le_bytes());
    buf.extend_from_slice(&reply_copydata_message.to_le_bytes());
    buf.extend_from_slice(&search_flags.to_le_bytes());
    buf.extend_from_slice(&offset.to_le_bytes());
    buf.extend_from_slice(&max_results.to_le_bytes());
    buf.extend_from_slice(&request_flags.to_le_bytes());
    buf.extend_from_slice(&sort_type.to_le_bytes());
    for u in utf16 {
        buf.extend_from_slice(&u.to_le_bytes());
    }
    buf
}

/// Encode a v1 `EVERYTHING_IPC_QUERYW` payload (`EVERYTHING_IPC_COPYDATAQUERYW`).
pub fn encode_query1_w(
    reply_hwnd: u32,
    reply_copydata_message: u32,
    search_flags: u32,
    offset: u32,
    max_results: u32,
    search_string: &str,
) -> Vec<u8> {
    let utf16: Vec<u16> = search_string
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = Vec::with_capacity(QUERY1_HEADER_SIZE + utf16.len() * 2);
    buf.extend_from_slice(&reply_hwnd.to_le_bytes());
    buf.extend_from_slice(&reply_copydata_message.to_le_bytes());
    buf.extend_from_slice(&search_flags.to_le_bytes());
    buf.extend_from_slice(&offset.to_le_bytes());
    buf.extend_from_slice(&max_results.to_le_bytes());
    for u in utf16 {
        buf.extend_from_slice(&u.to_le_bytes());
    }
    buf
}

/// Decode an `EVERYTHING_IPC_LIST2` UTF-16 reply buffer into [`DecodedList`].
pub fn decode_list2_w(data: &[u8]) -> Result<DecodedList, SearchError> {
    if data.len() < LIST2_HEADER_SIZE {
        return Err(SearchError::Ipc(format!(
            "Everything LIST2 reply truncated: {} bytes",
            data.len()
        )));
    }

    let total_items = read_u32(data, 0)?;
    let num_items = read_u32(data, 4)? as usize;
    let offset = read_u32(data, 8)?;
    let request_flags = read_u32(data, 12)?;
    let _sort_type = read_u32(data, 16)?;

    let items_bytes = num_items
        .checked_mul(ITEM2_SIZE)
        .and_then(|n| LIST2_HEADER_SIZE.checked_add(n))
        .ok_or_else(|| SearchError::Ipc("Everything LIST2 item count overflow".into()))?;
    if items_bytes > data.len() {
        return Err(SearchError::Ipc(format!(
            "Everything LIST2 item table exceeds buffer ({items_bytes} > {})",
            data.len()
        )));
    }

    let mut hits = Vec::with_capacity(num_items);
    for i in 0..num_items {
        let item_off = LIST2_HEADER_SIZE + i * ITEM2_SIZE;
        let flags = read_u32(data, item_off)?;
        let data_offset = read_u32(data, item_off + 4)? as usize;
        if data_offset > data.len() {
            return Err(SearchError::Ipc(format!(
                "Everything LIST2 item {i} data_offset out of bounds ({data_offset} > {})",
                data.len()
            )));
        }

        let mut cursor = data_offset;
        let mut name_opt: Option<String> = None;
        let mut path_opt: Option<String> = None;
        let mut full_opt: Option<String> = None;
        let mut size: u64 = 0;
        let mut modified: i64 = 0;

        // Fields appear in strict ascending bit order of `request_flags`.
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_NAME != 0 {
            name_opt = Some(read_len_prefixed_utf16(data, &mut cursor)?);
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_PATH != 0 {
            path_opt = Some(read_len_prefixed_utf16(data, &mut cursor)?);
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_FULL_PATH_AND_NAME != 0 {
            full_opt = Some(read_len_prefixed_utf16(data, &mut cursor)?);
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_EXTENSION != 0 {
            let _ = read_len_prefixed_utf16(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_SIZE != 0 {
            let raw = read_u64_adv(data, &mut cursor)?;
            size = if raw == EVERYTHING_UNKNOWN_U64 { 0 } else { raw };
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_DATE_CREATED != 0 {
            let _ = read_u64_adv(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_DATE_MODIFIED != 0 {
            let raw = read_u64_adv(data, &mut cursor)?;
            modified = if raw == EVERYTHING_UNKNOWN_U64 {
                0
            } else {
                raw as i64
            };
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_DATE_ACCESSED != 0 {
            let _ = read_u64_adv(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_ATTRIBUTES != 0 {
            let _ = read_u32_adv(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_FILE_LIST_FILE_NAME != 0 {
            let _ = read_len_prefixed_utf16(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_RUN_COUNT != 0 {
            let _ = read_u32_adv(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_DATE_RUN != 0 {
            let _ = read_u64_adv(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_DATE_RECENTLY_CHANGED != 0 {
            let _ = read_u64_adv(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_HIGHLIGHTED_NAME != 0 {
            let _ = read_len_prefixed_utf16(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_HIGHLIGHTED_PATH != 0 {
            let _ = read_len_prefixed_utf16(data, &mut cursor)?;
        }
        if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_HIGHLIGHTED_FULL_PATH_AND_NAME != 0 {
            let _ = read_len_prefixed_utf16(data, &mut cursor)?;
        }

        let (path, name) = assemble_path_and_name(path_opt, name_opt, full_opt);
        let is_dir = (flags & (EVERYTHING_IPC_FOLDER | EVERYTHING_IPC_DRIVE)) != 0;
        hits.push(SearchHit {
            path,
            name,
            is_dir,
            size,
            modified,
        });
    }

    Ok(DecodedList {
        total_items,
        offset,
        hits,
    })
}

/// Decode a v1 `EVERYTHING_IPC_LISTW` UTF-16 reply buffer into [`DecodedList`].
pub fn decode_list1_w(data: &[u8]) -> Result<DecodedList, SearchError> {
    if data.len() < LIST1_HEADER_SIZE {
        return Err(SearchError::Ipc(format!(
            "Everything LISTW reply truncated: {} bytes",
            data.len()
        )));
    }

    let _totfolders = read_u32(data, 0)?;
    let _totfiles = read_u32(data, 4)?;
    let total_items = read_u32(data, 8)?;
    let _numfolders = read_u32(data, 12)?;
    let _numfiles = read_u32(data, 16)?;
    let num_items = read_u32(data, 20)? as usize;
    let offset = read_u32(data, 24)?;

    let items_bytes = num_items
        .checked_mul(ITEM1_SIZE)
        .and_then(|n| LIST1_HEADER_SIZE.checked_add(n))
        .ok_or_else(|| SearchError::Ipc("Everything LISTW item count overflow".into()))?;
    if items_bytes > data.len() {
        return Err(SearchError::Ipc(format!(
            "Everything LISTW item table exceeds buffer ({items_bytes} > {})",
            data.len()
        )));
    }

    let mut hits = Vec::with_capacity(num_items);
    for i in 0..num_items {
        let item_off = LIST1_HEADER_SIZE + i * ITEM1_SIZE;
        let flags = read_u32(data, item_off)?;
        let filename_off = read_u32(data, item_off + 4)? as usize;
        let path_off = read_u32(data, item_off + 8)? as usize;

        let name = read_nul_utf16(data, filename_off)?;
        let dir = read_nul_utf16(data, path_off)?;
        let (path, name) = assemble_path_and_name(Some(dir), Some(name), None);
        let is_dir = (flags & (EVERYTHING_IPC_FOLDER | EVERYTHING_IPC_DRIVE)) != 0;
        hits.push(SearchHit {
            path,
            name,
            is_dir,
            size: 0,
            modified: 0,
        });
    }

    Ok(DecodedList {
        total_items,
        offset,
        hits,
    })
}

fn assemble_path_and_name(
    path_opt: Option<String>,
    name_opt: Option<String>,
    full_opt: Option<String>,
) -> (PathBuf, String) {
    match (path_opt, name_opt, full_opt) {
        (Some(dir), Some(name), _) => {
            let full = if dir.is_empty() {
                PathBuf::from(&name)
            } else {
                PathBuf::from(dir).join(&name)
            };
            (full, name)
        }
        (_, _, Some(full)) => {
            let path = PathBuf::from(&full);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| full.clone());
            (path, name)
        }
        (None, Some(name), None) => (PathBuf::from(&name), name),
        (Some(dir), None, None) => {
            let path = PathBuf::from(&dir);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(dir);
            (path, name)
        }
        (None, None, None) => (PathBuf::new(), String::new()),
    }
}

#[inline]
fn read_u32(data: &[u8], off: usize) -> Result<u32, SearchError> {
    let end = off
        .checked_add(4)
        .filter(|&e| e <= data.len())
        .ok_or_else(|| SearchError::Ipc(format!("unexpected EOF reading u32 at {off}")))?;
    Ok(u32::from_le_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[end - 1],
    ]))
}

#[inline]
fn read_u32_adv(data: &[u8], cursor: &mut usize) -> Result<u32, SearchError> {
    let v = read_u32(data, *cursor)?;
    *cursor += 4;
    Ok(v)
}

#[inline]
fn read_u64_adv(data: &[u8], cursor: &mut usize) -> Result<u64, SearchError> {
    let off = *cursor;
    let end = off
        .checked_add(8)
        .filter(|&e| e <= data.len())
        .ok_or_else(|| SearchError::Ipc(format!("unexpected EOF reading u64 at {off}")))?;
    *cursor = end;
    Ok(u64::from_le_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
        data[off + 4],
        data[off + 5],
        data[off + 6],
        data[off + 7],
    ]))
}

/// Read an Everything LIST2 string: `DWORD len_wchars` followed by `(len + 1) * 2`
/// bytes of NUL-terminated UTF-16LE.
fn read_len_prefixed_utf16(data: &[u8], cursor: &mut usize) -> Result<String, SearchError> {
    let len_units = read_u32_adv(data, cursor)? as usize;
    let total_units = len_units
        .checked_add(1)
        .ok_or_else(|| SearchError::Ipc("UTF-16 string length overflow".into()))?;
    let total_bytes = total_units
        .checked_mul(2)
        .ok_or_else(|| SearchError::Ipc("UTF-16 byte length overflow".into()))?;
    let start = *cursor;
    let end = start
        .checked_add(total_bytes)
        .filter(|&e| e <= data.len())
        .ok_or_else(|| {
            SearchError::Ipc(format!(
                "UTF-16 string at {start} exceeds buffer ({total_bytes} bytes)"
            ))
        })?;
    *cursor = end;

    let mut units = Vec::with_capacity(len_units);
    for &[lo, hi] in data[start..start + len_units * 2].as_chunks::<2>().0 {
        units.push(u16::from_le_bytes([lo, hi]));
    }
    Ok(String::from_utf16_lossy(&units))
}

/// Read a NUL-terminated UTF-16LE string at `offset` (used by v1 `EVERYTHING_IPC_LISTW`).
fn read_nul_utf16(data: &[u8], offset: usize) -> Result<String, SearchError> {
    if offset >= data.len() {
        return Err(SearchError::Ipc(format!(
            "UTF-16 string offset {offset} out of bounds ({})",
            data.len()
        )));
    }
    let mut units = Vec::new();
    let mut pos = offset;
    while pos + 2 <= data.len() {
        let u = u16::from_le_bytes([data[pos], data[pos + 1]]);
        if u == 0 {
            return Ok(String::from_utf16_lossy(&units));
        }
        units.push(u);
        pos += 2;
    }
    Err(SearchError::Ipc(format!(
        "unterminated UTF-16 string at offset {offset}"
    )))
}

// ---- Win32 hidden message-only HWND & WM_COPYDATA transport -----------------

struct ReplySlot {
    expected_reply_id: usize,
    received: bool,
    payload: Vec<u8>,
}

enum SendRecvError {
    Rejected,
    Timeout,
    Cancelled,
    Ipc(String),
}

impl SendRecvError {
    fn into_search_error(self) -> SearchError {
        match self {
            SendRecvError::Rejected => {
                SearchError::Ipc("Everything rejected the IPC query".into())
            }
            SendRecvError::Timeout => SearchError::Timeout,
            SendRecvError::Cancelled => SearchError::Cancelled,
            SendRecvError::Ipc(msg) => SearchError::Ipc(msg),
        }
    }
}

fn next_reply_id() -> u32 {
    static NEXT_ID: AtomicU32 = AtomicU32::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn ensure_reply_class_registered() -> Result<(), SearchError> {
    static REGISTERED: OnceLock<Result<(), u32>> = OnceLock::new();
    let res = *REGISTERED.get_or_init(|| unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
        let cls_name = wide(REPLY_WND_CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(reply_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(cls_name.as_ptr()),
            ..Default::default()
        };
        let atom = RegisterClassW(&wc);
        if atom == 0 {
            let err = GetLastError().0;
            // ERROR_CLASS_ALREADY_EXISTS (1410) is fine.
            if err == 1410 {
                Ok(())
            } else {
                Err(err)
            }
        } else {
            Ok(())
        }
    });
    res.map_err(|code| SearchError::Ipc(format!("RegisterClassW failed (win32 {code})")))
}

struct MessageWindow {
    hwnd: HWND,
}

impl MessageWindow {
    fn create(slot: &RefCell<ReplySlot>) -> Result<Self, SearchError> {
        ensure_reply_class_registered()?;
        let cls_name = wide(REPLY_WND_CLASS);
        let hwnd = unsafe {
            let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(cls_name.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .map_err(|e| SearchError::Ipc(format!("CreateWindowExW(HWND_MESSAGE) failed: {e}")))?;

        unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (slot as *const RefCell<ReplySlot>) as isize,
            );
        }
        Ok(Self { hwnd })
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn reply_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_COPYDATA {
        let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<ReplySlot>;
        if !ptr.is_null() && lparam.0 != 0 {
            let slot_cell = unsafe { &*ptr };
            let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            if let Ok(mut slot) = slot_cell.try_borrow_mut()
                && cds.dwData == slot.expected_reply_id
            {
                let len = cds.cbData as usize;
                if len <= MAX_REPLY_BYTES && (!cds.lpData.is_null() || len == 0) {
                    let bytes = if len == 0 {
                        &[][..]
                    } else {
                        unsafe { std::slice::from_raw_parts(cds.lpData.cast::<u8>(), len) }
                    };
                    slot.payload.clear();
                    slot.payload.extend_from_slice(bytes);
                    slot.received = true;
                    return LRESULT(1);
                }
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn send_and_receive(
    everything_hwnd: HWND,
    reply_hwnd: HWND,
    copydata_cmd: usize,
    payload: &[u8],
    timeout: Duration,
    cancel: &AtomicBool,
    slot: &RefCell<ReplySlot>,
) -> Result<Vec<u8>, SendRecvError> {
    let deadline = Instant::now() + timeout;
    let timeout_ms = timeout.as_millis().clamp(1, u32::MAX as u128) as u32;

    let cds = COPYDATASTRUCT {
        dwData: copydata_cmd,
        cbData: payload.len() as u32,
        lpData: payload.as_ptr() as *mut c_void,
    };

    let mut result: usize = 0;
    let sent = unsafe {
        SendMessageTimeoutW(
            everything_hwnd,
            WM_COPYDATA,
            WPARAM(reply_hwnd.0 as usize),
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG | SMTO_NORMAL,
            timeout_ms,
            Some(&mut result),
        )
    };

    if sent.0 == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_TIMEOUT {
            return Err(SendRecvError::Timeout);
        }
        return Err(SendRecvError::Ipc(format!(
            "SendMessageTimeoutW to Everything failed (win32 {})",
            err.0
        )));
    }
    if result == 0 {
        return Err(SendRecvError::Rejected);
    }

    // Pump messages on this worker thread until Everything delivers WM_COPYDATA,
    // cancellation triggers, or timeout expires.
    let mut msg = MSG::default();
    while !slot.borrow().received {
        if cancel.load(Ordering::Relaxed) {
            return Err(SendRecvError::Cancelled);
        }
        while unsafe { PeekMessageW(&mut msg, Some(reply_hwnd), 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if slot.borrow().received {
                break;
            }
        }
        if slot.borrow().received {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(SendRecvError::Timeout);
        }
        let remaining_ms = (deadline - now).as_millis().min(WAIT_SLICE_MS as u128) as u32;
        unsafe {
            let _ = MsgWaitForMultipleObjectsEx(
                None,
                remaining_ms.max(1),
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
        }
    }

    Ok(std::mem::take(&mut slot.borrow_mut().payload))
}

// ---- Unit tests (pure framing & query-building tests; no Everything needed) -

#[cfg(test)]
mod tests {
    use super::*;

    struct TestList2Item<'a> {
        flags: u32,
        name: Option<&'a str>,
        path: Option<&'a str>,
        full: Option<&'a str>,
        size: Option<u64>,
        mtime: Option<u64>,
    }

    /// Helper to build a synthetic `EVERYTHING_IPC_LIST2` binary frame.
    fn build_list2_frame(
        total_items: u32,
        offset: u32,
        request_flags: u32,
        items: &[TestList2Item<'_>],
    ) -> Vec<u8> {
        let num_items = items.len() as u32;
        let table_end = LIST2_HEADER_SIZE + items.len() * ITEM2_SIZE;
        let mut data_section = Vec::new();
        let mut item_table = Vec::with_capacity(items.len() * ITEM2_SIZE);

        for item in items {
            let data_offset = (table_end + data_section.len()) as u32;
            item_table.extend_from_slice(&item.flags.to_le_bytes());
            item_table.extend_from_slice(&data_offset.to_le_bytes());

            let push_str = |buf: &mut Vec<u8>, s: &str| {
                let units: Vec<u16> = s.encode_utf16().collect();
                buf.extend_from_slice(&(units.len() as u32).to_le_bytes());
                for u in units {
                    buf.extend_from_slice(&u.to_le_bytes());
                }
                buf.extend_from_slice(&0u16.to_le_bytes());
            };

            if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_NAME != 0 {
                push_str(&mut data_section, item.name.unwrap_or(""));
            }
            if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_PATH != 0 {
                push_str(&mut data_section, item.path.unwrap_or(""));
            }
            if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_FULL_PATH_AND_NAME != 0 {
                push_str(&mut data_section, item.full.unwrap_or(""));
            }
            if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_SIZE != 0 {
                data_section.extend_from_slice(&item.size.unwrap_or(u64::MAX).to_le_bytes());
            }
            if request_flags & EVERYTHING_IPC_QUERY2_REQUEST_DATE_MODIFIED != 0 {
                data_section.extend_from_slice(&item.mtime.unwrap_or(u64::MAX).to_le_bytes());
            }
        }

        let mut frame = Vec::with_capacity(table_end + data_section.len());
        frame.extend_from_slice(&total_items.to_le_bytes());
        frame.extend_from_slice(&num_items.to_le_bytes());
        frame.extend_from_slice(&offset.to_le_bytes());
        frame.extend_from_slice(&request_flags.to_le_bytes());
        frame.extend_from_slice(&EVERYTHING_IPC_SORT_NAME_ASCENDING.to_le_bytes());
        frame.extend_from_slice(&item_table);
        frame.extend_from_slice(&data_section);
        frame
    }

    /// Helper to build a synthetic v1 `EVERYTHING_IPC_LISTW` binary frame.
    fn build_list1_frame(items: &[(u32, &str, &str)]) -> Vec<u8> {
        let num_items = items.len() as u32;
        let table_end = LIST1_HEADER_SIZE + items.len() * ITEM1_SIZE;
        let mut strings = Vec::new();
        let mut item_table = Vec::with_capacity(items.len() * ITEM1_SIZE);

        let push_nul_utf16 = |buf: &mut Vec<u8>, s: &str| -> u32 {
            let off = (table_end + buf.len()) as u32;
            for u in s.encode_utf16().chain(std::iter::once(0)) {
                buf.extend_from_slice(&u.to_le_bytes());
            }
            off
        };

        let mut num_folders = 0u32;
        let mut num_files = 0u32;
        for &(flags, name, dir) in items {
            if flags & EVERYTHING_IPC_FOLDER != 0 {
                num_folders += 1;
            } else {
                num_files += 1;
            }
            let name_off = push_nul_utf16(&mut strings, name);
            let dir_off = push_nul_utf16(&mut strings, dir);
            item_table.extend_from_slice(&flags.to_le_bytes());
            item_table.extend_from_slice(&name_off.to_le_bytes());
            item_table.extend_from_slice(&dir_off.to_le_bytes());
        }

        let mut frame = Vec::with_capacity(table_end + strings.len());
        frame.extend_from_slice(&num_folders.to_le_bytes());
        frame.extend_from_slice(&num_files.to_le_bytes());
        frame.extend_from_slice(&num_items.to_le_bytes());
        frame.extend_from_slice(&num_folders.to_le_bytes());
        frame.extend_from_slice(&num_files.to_le_bytes());
        frame.extend_from_slice(&num_items.to_le_bytes());
        frame.extend_from_slice(&0u32.to_le_bytes()); // offset
        frame.extend_from_slice(&item_table);
        frame.extend_from_slice(&strings);
        frame
    }

    #[test]
    fn encode_query2_wire_layout() {
        let bytes = encode_query2_w(
            0x0012_3456,
            42,
            EVERYTHING_IPC_MATCHCASE | EVERYTHING_IPC_REGEX,
            10,
            500,
            DEFAULT_REQUEST_FLAGS,
            EVERYTHING_IPC_SORT_NAME_ASCENDING,
            "*.rs",
        );
        // 28-byte fixed header + (4 chars + NUL) * 2 bytes = 38 bytes.
        assert_eq!(bytes.len(), 28 + 10);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 0x0012_3456);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 42);
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            EVERYTHING_IPC_MATCHCASE | EVERYTHING_IPC_REGEX
        );
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 10);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 500);
        assert_eq!(
            u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            DEFAULT_REQUEST_FLAGS
        );
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            EVERYTHING_IPC_SORT_NAME_ASCENDING
        );
        let text_units: Vec<u16> = bytes[28..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[lo, hi]| u16::from_le_bytes([lo, hi]))
            .collect();
        assert_eq!(text_units, vec![b'*' as u16, b'.' as u16, b'r' as u16, b's' as u16, 0]);
    }

    #[test]
    fn encode_query1_wire_layout() {
        let bytes = encode_query1_w(0xABCD, 7, EVERYTHING_IPC_MATCHCASE, 0, 100, "foo");
        assert_eq!(bytes.len(), 20 + 8);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 0xABCD);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 7);
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            EVERYTHING_IPC_MATCHCASE
        );
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 100);
    }

    #[test]
    fn decode_list2_parses_files_dirs_unicode_and_sentinels() {
        let frame = build_list2_frame(
            12,
            0,
            DEFAULT_REQUEST_FLAGS,
            &[
                TestList2Item {
                    flags: 0,
                    name: Some("main.rs"),
                    path: Some("C:\\repo\\src"),
                    full: None,
                    size: Some(4096),
                    mtime: Some(133_500_000_000_000_000),
                },
                TestList2Item {
                    flags: EVERYTHING_IPC_FOLDER,
                    name: Some("日本語フォルダ"),
                    path: Some("D:\\data"),
                    full: None,
                    size: Some(u64::MAX), // unknown folder size -> 0
                    mtime: Some(u64::MAX), // unknown date -> 0
                },
            ],
        );

        let decoded = decode_list2_w(&frame).expect("valid LIST2 frame");
        assert_eq!(decoded.total_items, 12);
        assert_eq!(decoded.offset, 0);
        assert_eq!(decoded.hits.len(), 2);

        assert_eq!(decoded.hits[0].path, PathBuf::from("C:\\repo\\src\\main.rs"));
        assert_eq!(decoded.hits[0].name, "main.rs");
        assert!(!decoded.hits[0].is_dir);
        assert_eq!(decoded.hits[0].size, 4096);
        assert_eq!(decoded.hits[0].modified, 133_500_000_000_000_000);

        assert_eq!(decoded.hits[1].path, PathBuf::from("D:\\data\\日本語フォルダ"));
        assert_eq!(decoded.hits[1].name, "日本語フォルダ");
        assert!(decoded.hits[1].is_dir);
        assert_eq!(decoded.hits[1].size, 0);
        assert_eq!(decoded.hits[1].modified, 0);
    }

    #[test]
    fn decode_list2_handles_full_path_only_flag() {
        let flags = EVERYTHING_IPC_QUERY2_REQUEST_FULL_PATH_AND_NAME | EVERYTHING_IPC_QUERY2_REQUEST_SIZE;
        let frame = build_list2_frame(
            1,
            0,
            flags,
            &[TestList2Item {
                flags: 0,
                name: None,
                path: None,
                full: Some("E:\\archive\\notes.md"),
                size: Some(512),
                mtime: None,
            }],
        );
        let decoded = decode_list2_w(&frame).expect("full-path LIST2");
        assert_eq!(decoded.hits.len(), 1);
        assert_eq!(decoded.hits[0].path, PathBuf::from("E:\\archive\\notes.md"));
        assert_eq!(decoded.hits[0].name, "notes.md");
        assert_eq!(decoded.hits[0].size, 512);
    }

    #[test]
    fn decode_list1_roundtrip() {
        let frame = build_list1_frame(&[
            (EVERYTHING_IPC_FOLDER, "docs", "C:\\work"),
            (0, "readme.txt", "C:\\work\\docs"),
        ]);
        let decoded = decode_list1_w(&frame).expect("valid LISTW frame");
        assert_eq!(decoded.total_items, 2);
        assert_eq!(decoded.hits.len(), 2);
        assert!(decoded.hits[0].is_dir);
        assert_eq!(decoded.hits[0].path, PathBuf::from("C:\\work\\docs"));
        assert!(!decoded.hits[1].is_dir);
        assert_eq!(decoded.hits[1].path, PathBuf::from("C:\\work\\docs\\readme.txt"));
        assert_eq!(decoded.hits[1].name, "readme.txt");
    }

    #[test]
    fn decode_rejects_truncated_and_corrupt_frames() {
        assert!(decode_list2_w(&[0u8; 8]).is_err());
        assert!(decode_list1_w(&[0u8; 12]).is_err());

        // Claim 100 items in a 20-byte header-only buffer.
        let mut bogus = vec![0u8; LIST2_HEADER_SIZE];
        bogus[4..8].copy_from_slice(&100u32.to_le_bytes());
        assert!(decode_list2_w(&bogus).is_err());
    }

    #[test]
    fn build_search_string_scopes_root_and_kind() {
        let q = SearchQuery {
            text: "*.rs".into(),
            root: Some(PathBuf::from("C:\\My Projects\\tsubame")),
            include_dirs: false,
            include_files: true,
            ..SearchQuery::default()
        };
        assert_eq!(
            build_search_string(&q),
            "\"C:\\My Projects\\tsubame\\\" file: *.rs"
        );

        let regex_q = SearchQuery {
            text: "^foo.*\\.txt$".into(),
            root: Some(PathBuf::from("C:\\proj")),
            regex: true,
            ..SearchQuery::default()
        };
        assert_eq!(build_search_string(&regex_q), "^foo.*\\.txt$");
    }
}
