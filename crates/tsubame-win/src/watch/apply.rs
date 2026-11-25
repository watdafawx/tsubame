//! Turns a coalesced [`Batch`] into entry-store updates, no relist.
//!
//! The batch is a hint, the disk is the truth: every `Created`, `Modified` and
//! `Renamed` name is stat'ed and the store is made to match. A name that is gone
//! by then becomes a removal; a `Created` name already in the store becomes an
//! update. So a batch that raced with later changes is still applied correctly,
//! and the next batch (which carries those later changes) converges.
//!
//! Two phases, so the slow one can stay off the UI thread:
//!
//! 1. [`resolve`] touches the disk and needs no store: a few names are stat'ed
//!    one by one (`GetFileAttributesExW`, `FindFirstFileExW` as fallback); many
//!    names cost one directory listing instead, because a stat is ~80 us on a
//!    busy machine and a listing is ~0.6 us per entry. The result is `Send`.
//! 2. [`apply_resolved`] touches only the store (single-digit milliseconds for
//!    10,000 changes in a 100,000-entry store) and belongs on the UI thread.
//!
//! [`apply_batch`] runs both in place: fine for the common one-to-few-names
//! batch, wrong for an archive extraction.
//!
//! Stable IDs: a rename rewrites the entry in place, so its ID, and with it its
//! selection, focus and anchor, follow the file. Everything goes through one
//! `EntryStore::apply` call: one index pass and one merge, whatever the count.

use super::coalesce::{Batch, ChangeKind};
use crate::enumerate::{CancelToken, Enumerator, Message, extended_wide};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tsubame_core::{
    CLOUD_ONLINE, CLOUD_PINNED, CLOUD_UNPINNED, EntryId, EntryInput, EntryStore, FOLDER, HIDDEN,
    READONLY, REPARSE, SYMLINK, SYSTEM, Selection, StoreError, Upsert,
};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Storage::FileSystem::{
    FIND_FIRST_EX_FLAGS, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
    GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

/// From this many names on, listing the folder can beat stat'ing each name.
const LIST_MIN: usize = 32;
/// List when `names * LIST_RATIO > entries in the folder` (stat ~80 us, list ~0.6 us/entry).
const LIST_RATIO: usize = 128;

/// What `apply_batch` / `apply_resolved` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyStats {
    /// Entries inserted.
    pub created: u32,
    /// Entries updated in place.
    pub modified: u32,
    /// Entries renamed in place (ID kept).
    pub renamed: u32,
    /// Entries removed, including names the disk no longer has.
    pub removed: u32,
    /// Changes ignored: names with path separators (recursive watches), or
    /// removals of names the store did not have.
    pub skipped: u32,
    /// Time spent on the disk (`resolve`; zero for `apply_resolved` alone).
    pub stat: Duration,
    /// Time spent in the store (lookup, index update, merge).
    pub store: Duration,
}

impl ApplyStats {
    /// Anything visible changed: repaint.
    pub fn changed(&self) -> bool {
        self.created + self.modified + self.renamed + self.removed > 0
    }
}

#[derive(Debug)]
struct Stat {
    size: u64,
    mtime: i64,
    flags: u16,
    /// The name as the directory spells it, when it differs from the event's.
    canonical: Option<Vec<u16>>,
}

/// What the disk said about every change of a batch. `Send`: build it on a worker.
#[derive(Debug)]
pub struct Resolved {
    /// By change index in the batch; `None` for removals and for names that are gone.
    stats: Vec<Option<Stat>>,
    /// Time `resolve` took.
    pub elapsed: Duration,
    /// The folder was listed once instead of stat'ing each name.
    pub listed: bool,
}

/// Store flags from Win32 attributes and optional reparse tag (the same mapping the listing uses).
pub fn flags_of_with_tag(attrs: u32, reparse_tag: u32) -> u16 {
    use crate::enumerate::{
        ATTR_PINNED, ATTR_RECALL_ON_DATA_ACCESS, ATTR_REPARSE_POINT, ATTR_UNPINNED,
        is_cloud_reparse_tag, is_link_reparse,
    };
    let mut f = 0;
    if attrs & 0x10 != 0 {
        f |= FOLDER;
    }
    if attrs & 0x2 != 0 {
        f |= HIDDEN;
    }
    if attrs & 0x4 != 0 {
        f |= SYSTEM;
    }
    if attrs & 0x1 != 0 {
        f |= READONLY;
    }
    if attrs & ATTR_REPARSE_POINT != 0 || reparse_tag != 0 {
        f |= REPARSE;
    }
    if is_link_reparse(attrs, reparse_tag) {
        f |= SYMLINK;
    }
    if attrs & ATTR_RECALL_ON_DATA_ACCESS != 0 {
        f |= CLOUD_ONLINE;
    }
    if attrs & ATTR_PINNED != 0 {
        f |= CLOUD_PINNED;
    }
    if attrs & ATTR_UNPINNED != 0 || (is_cloud_reparse_tag(reparse_tag) && attrs & (ATTR_RECALL_ON_DATA_ACCESS | ATTR_PINNED) == 0) {
        f |= CLOUD_UNPINNED;
    }
    f
}

/// Store flags from Win32 attributes (when no reparse tag is available).
pub fn flags_of(attrs: u32) -> u16 {
    flags_of_with_tag(attrs, 0)
}

fn ft(t: FILETIME) -> i64 {
    (((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64) as i64
}

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;

/// `path` is `prefix + name + NUL`. `None`: not there (or not statable at all).
fn stat(path: &[u16], name: &[u16]) -> Option<Stat> {
    if !name.contains(&(b'~' as u16)) {
        let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
        match unsafe {
            GetFileAttributesExW(
                PCWSTR(path.as_ptr()),
                GetFileExInfoStandard,
                &mut data as *mut _ as *mut _,
            )
        } {
            Ok(()) => {
                return Some(Stat {
                    size: ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64,
                    mtime: ft(data.ftLastWriteTime),
                    flags: flags_of(data.dwFileAttributes),
                    canonical: None,
                });
            }
            Err(e) => {
                let code = e.code().0 & 0xFFFF;
                if code != ERROR_ACCESS_DENIED && code != ERROR_SHARING_VIOLATION {
                    return None;
                }
            }
        }
    }
    // The directory entry itself: needs no access to the file, and carries the
    // spelling the directory uses (an 8.3 name in an event becomes the long one).
    let mut data = WIN32_FIND_DATAW::default();
    let h = unsafe {
        FindFirstFileExW(
            PCWSTR(path.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_FLAGS(0),
        )
    }
    .ok()?;
    unsafe {
        let _ = FindClose(h);
    }
    let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
    let real = &data.cFileName[..len];
    let tag = if data.dwFileAttributes & crate::enumerate::ATTR_REPARSE_POINT != 0 {
        data.dwReserved0
    } else {
        0
    };
    Some(Stat {
        size: ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64,
        mtime: ft(data.ftLastWriteTime),
        flags: flags_of_with_tag(data.dwFileAttributes, tag),
        canonical: (real != name && !real.is_empty()).then(|| real.to_vec()),
    })
}

fn has_separator(name: &[u16]) -> bool {
    name.iter().any(|&c| c == b'\\' as u16 || c == b'/' as u16)
}

/// One pass over the folder, keeping only the wanted names (exact spelling).
/// Fills `stats[index]` for each hit; misses are left for the per-name path.
fn list_wanted(dir: &Path, wanted: &HashMap<&[u16], usize>, stats: &mut [Option<Stat>]) {
    let token = CancelToken::new();
    let generation = token.next_generation();
    let (_enumerator, rx) = Enumerator::start_channel(dir, generation, token, Arc::new(|| {}));
    while let Ok(msg) = rx.recv() {
        match msg {
            Message::Batch(b) => {
                for (name, e) in b.iter() {
                    if let Some(&i) = wanted.get(name) {
                        let flags = flags_of_with_tag(e.attrs, e.reparse_tag);
                        stats[i] = Some(Stat { size: e.size, mtime: e.mtime as i64, flags, canonical: None });
                    }
                }
            }
            Message::Done(_) => break,
        }
    }
}

/// Phase 1: asks the disk about every changed name of `batch` (from a
/// non-recursive watch of `dir`). `entries_hint` is the folder's size, normally
/// `store.len()`; it only picks between per-name stats and one listing.
pub fn resolve(dir: &Path, batch: &Batch, entries_hint: usize) -> Resolved {
    let t0 = Instant::now();
    let mut stats: Vec<Option<Stat>> = Vec::new();
    stats.resize_with(batch.len(), || None);
    let changes: Vec<_> = batch.iter().collect();
    let todo: Vec<usize> = (0..changes.len())
        .filter(|&i| changes[i].kind != ChangeKind::Removed && !has_separator(changes[i].name))
        .collect();
    let mut listed = false;
    if todo.len() >= LIST_MIN && todo.len().saturating_mul(LIST_RATIO) > entries_hint {
        let wanted: HashMap<&[u16], usize> = todo.iter().map(|&i| (changes[i].name, i)).collect();
        list_wanted(dir, &wanted, &mut stats);
        listed = true;
    }
    // Per-name stats: everything when few; after a listing only the misses
    // (a differently-spelled name, or one that is really gone).
    if let Ok(mut path) = extended_wide(dir) {
        path.pop();
        if path.last() != Some(&(b'\\' as u16)) {
            path.push(b'\\' as u16);
        }
        let prefix = path.len();
        for i in todo {
            if stats[i].is_some() {
                continue;
            }
            path.truncate(prefix);
            path.extend_from_slice(changes[i].name);
            path.push(0);
            stats[i] = stat(&path, changes[i].name);
        }
    }
    Resolved { stats, elapsed: t0.elapsed(), listed }
}

/// Phase 2: makes `store` match what [`resolve`] found.
///
/// `sel`, when given, is kept exact: removed entries leave it, and a selected
/// entry whose size changed updates the selected-bytes total. `resolved` must
/// come from the same `batch`. Positions in the view may shift, IDs of
/// surviving entries do not.
pub fn apply_resolved(
    store: &mut EntryStore,
    mut sel: Option<&mut Selection>,
    batch: &Batch,
    resolved: &Resolved,
) -> Result<ApplyStats, StoreError> {
    let t0 = Instant::now();
    let mut stats = ApplyStats { stat: Duration::ZERO, ..ApplyStats::default() };
    let mut changes = Vec::with_capacity(batch.len());
    for (i, c) in batch.iter().enumerate() {
        if has_separator(c.name) || has_separator(c.from) {
            stats.skipped += 1;
        } else {
            changes.push((i, c));
        }
    }
    let mut lookups: Vec<&[u16]> = Vec::with_capacity(changes.len() + 4);
    for (_, c) in &changes {
        lookups.push(c.name);
        if c.kind == ChangeKind::Renamed {
            lookups.push(c.from);
        }
    }
    let found = store.find_many(&lookups);

    struct Pending<'a> {
        id: Option<EntryId>,
        name: &'a [u16],
        size: u64,
        mtime: i64,
        flags: u16,
    }
    let mut removed: Vec<EntryId> = Vec::new();
    let mut pending: Vec<Pending<'_>> = Vec::with_capacity(changes.len());
    // IDs already decided in this batch; a second change to the same entry
    // (a name reused after a rename) becomes an insert instead of a clash.
    let mut claimed: HashSet<EntryId> = HashSet::new();
    let mut take = |id: Option<EntryId>| id.filter(|&i| claimed.insert(i));
    let mut at = 0;
    for &(i, ref c) in &changes {
        let id = found[at];
        at += 1;
        let st = resolved.stats.get(i).and_then(|s| s.as_ref());
        let spelled: &[u16] = st.and_then(|s| s.canonical.as_deref()).unwrap_or(c.name);
        match c.kind {
            ChangeKind::Removed => match take(id) {
                Some(id) => removed.push(id),
                None => stats.skipped += 1,
            },
            ChangeKind::Created | ChangeKind::Modified => match st {
                Some(s) => {
                    let id = take(id);
                    if id.is_some() {
                        stats.modified += 1;
                    } else {
                        stats.created += 1;
                    }
                    pending.push(Pending { id, name: spelled, size: s.size, mtime: s.mtime, flags: s.flags });
                }
                None => match take(id) {
                    Some(id) => removed.push(id),
                    None => stats.skipped += 1,
                },
            },
            ChangeKind::Renamed => {
                // lookups were pushed as (new name, old name)
                let (to_raw, from_raw) = (id, found[at]);
                at += 1;
                let from_id = take(from_raw);
                let to_id = take(to_raw);
                // the entry that carries on: the old name's, else one the new
                // name already had; an existing entry under the new name that is
                // not it was overwritten
                let (target, overwritten) = match (from_id, to_id) {
                    (Some(f), t) => (Some(f), t),
                    (None, t) => (t, None),
                };
                removed.extend(overwritten);
                match st {
                    Some(s) => {
                        stats.renamed += 1;
                        pending.push(Pending { id: target, name: spelled, size: s.size, mtime: s.mtime, flags: s.flags });
                    }
                    None => removed.extend(target),
                }
            }
        }
    }
    stats.removed = removed.len() as u32;

    // Selection bookkeeping happens against the store as it is now.
    let mut resized: Vec<(EntryId, u64)> = Vec::new();
    if let Some(sel) = sel.as_deref_mut() {
        for &id in &removed {
            sel.forget(store, id);
        }
        for p in &pending {
            if let Some(id) = p.id
                && sel.contains(id)
                && let Some(r) = store.record(id)
            {
                resized.push((id, r.size));
            }
        }
    }
    let upserts: Vec<Upsert<'_>> = pending
        .iter()
        .map(|p| Upsert {
            id: p.id,
            input: EntryInput { name: p.name, size: p.size, modified: p.mtime, flags: p.flags },
        })
        .collect();
    store.apply(&removed, &upserts)?;
    if let Some(sel) = sel {
        for (id, old) in resized {
            sel.resized(store, id, old);
        }
    }
    stats.store = t0.elapsed();
    Ok(stats)
}

/// [`resolve`] then [`apply_resolved`] on the calling thread. For the usual
/// small batch. A burst should `resolve` on a worker and `apply_resolved` on the
/// UI thread.
pub fn apply_batch(
    store: &mut EntryStore,
    sel: Option<&mut Selection>,
    dir: &Path,
    batch: &Batch,
) -> Result<ApplyStats, StoreError> {
    let resolved = resolve(dir, batch, store.len());
    let mut stats = apply_resolved(store, sel, batch, &resolved)?;
    stats.stat = resolved.elapsed;
    Ok(stats)
}
