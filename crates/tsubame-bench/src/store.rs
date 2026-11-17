//! Compact entry store, the shape PLAN.md section 3 rule 2 asks for.
//!
//! One contiguous UTF-16 name buffer plus a `Vec` of fixed-size records. No
//! per-entry `String`/`Box`, so a million entries is one big realloc-grow and
//! nothing else. Records are 48 bytes and carry everything a details view needs
//! (name span, size, mtime, attributes, reparse tag, 128-bit file id).

use crate::clock::Clock;

/// First-batch latency is measured until this many entries are in the store.
pub const FIRST_BATCH: usize = 256;

pub const FLAG_DIR: u16 = 1 << 0;
pub const FLAG_REPARSE: u16 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryRec {
    /// Offset into `names`, in UTF-16 code units.
    pub name_off: u32,
    /// Length in UTF-16 code units (no terminator stored).
    pub name_len: u16,
    pub flags: u16,
    pub size: u64,
    /// Last write time, FILETIME ticks.
    pub mtime: i64,
    pub attrs: u32,
    pub reparse_tag: u32,
    pub file_id: [u8; 16],
}

const _: () = assert!(core::mem::size_of::<EntryRec>() == 48);

pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_REPARSE_POINT: u32 = 0x400;

#[derive(Default)]
pub struct EntryStore {
    pub names: Vec<u16>,
    pub recs: Vec<EntryRec>,
    /// Ticks from the run start until the 256th entry landed (None until then).
    pub first_batch_ticks: Option<i64>,
    /// "." and ".." seen and dropped (NT-style listings include them).
    pub dots_skipped: usize,
    pub truncated_names: usize,
}

impl EntryStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.recs.len()
    }

    /// Append one entry. `file_id` is all zeros for methods that cannot provide
    /// one (`FindFirstFileExW`).
    #[inline]
    pub fn push(
        &mut self,
        name: &[u16],
        attrs: u32,
        size: u64,
        mtime: i64,
        reparse_tag: u32,
        file_id: [u8; 16],
    ) {
        if is_dot(name) {
            self.dots_skipped += 1;
            return;
        }
        // NTFS names are <= 255 code units; guard against a bad buffer.
        if name.len() > u16::MAX as usize {
            self.truncated_names += 1;
            return;
        }
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        let mut flags = 0u16;
        if attrs & ATTR_DIRECTORY != 0 {
            flags |= FLAG_DIR;
        }
        if attrs & ATTR_REPARSE_POINT != 0 || reparse_tag != 0 {
            flags |= FLAG_REPARSE;
        }
        self.recs.push(EntryRec {
            name_off: off,
            name_len: name.len() as u16,
            flags,
            size,
            mtime,
            attrs,
            reparse_tag,
            file_id,
        });
    }

    #[inline]
    pub fn name(&self, i: usize) -> &[u16] {
        let r = &self.recs[i];
        let start = r.name_off as usize;
        &self.names[start..start + r.name_len as usize]
    }

    /// Mark first-batch time as soon as FIRST_BATCH entries are in.
    #[inline]
    pub fn note_progress(&mut self, clock: &Clock, start: i64) {
        if self.first_batch_ticks.is_none() && self.recs.len() >= FIRST_BATCH {
            self.first_batch_ticks = Some(clock.now() - start);
        }
    }

    /// Small folders never reach FIRST_BATCH; treat "complete" as first batch.
    pub fn finish_first_batch(&mut self, complete_ticks: i64) {
        if self.first_batch_ticks.is_none() && !self.recs.is_empty() {
            self.first_batch_ticks = Some(complete_ticks);
        }
    }

    pub fn name_bytes(&self) -> usize {
        self.names.len() * 2
    }

    /// Indices sorted by name (UTF-16 code unit order, ties broken by name length).
    pub fn sorted_index(&self) -> Vec<u32> {
        let mut idx: Vec<u32> = (0..self.recs.len() as u32).collect();
        idx.sort_unstable_by(|&a, &b| {
            let na = self.name(a as usize);
            let nb = self.name(b as usize);
            na.cmp(nb)
        });
        idx
    }
}

#[inline]
fn is_dot(name: &[u16]) -> bool {
    matches!(name, [0x2E] | [0x2E, 0x2E])
}

/// Result of comparing two stores entry by entry.
#[derive(Debug)]
pub struct DiffReport {
    pub a_len: usize,
    pub b_len: usize,
    pub compared_ids: usize,
    pub compared_metadata: usize,
    pub first_mismatch: Option<String>,
}

impl DiffReport {
    pub fn is_equal(&self) -> bool {
        self.first_mismatch.is_none() && self.a_len == self.b_len
    }
}

/// Compare two stores by sorted name: names, attributes, size, mtime, reparse
/// tag, and file id when both sides have one.
pub fn compare(a: &EntryStore, b: &EntryStore) -> DiffReport {
    let mut report = DiffReport {
        a_len: a.len(),
        b_len: b.len(),
        compared_ids: 0,
        compared_metadata: 0,
        first_mismatch: None,
    };
    if a.len() != b.len() {
        report.first_mismatch = Some(format!("entry count differs: {} vs {}", a.len(), b.len()));
        return report;
    }
    let ia = a.sorted_index();
    let ib = b.sorted_index();
    for (pa, pb) in ia.iter().zip(ib.iter()) {
        let (ea, eb) = (*pa as usize, *pb as usize);
        let (na, nb) = (a.name(ea), b.name(eb));
        if na != nb {
            report.first_mismatch = Some(format!(
                "name mismatch: {:?} vs {:?}",
                crate::util::utf16_to_string(na),
                crate::util::utf16_to_string(nb)
            ));
            return report;
        }
        let (ra, rb) = (&a.recs[ea], &b.recs[eb]);
        if ra.attrs != rb.attrs || ra.size != rb.size || ra.mtime != rb.mtime {
            report.first_mismatch = Some(format!(
                "metadata mismatch for {:?}: attrs {:#x}/{:#x} size {}/{} mtime {}/{}",
                crate::util::utf16_to_string(na),
                ra.attrs,
                rb.attrs,
                ra.size,
                rb.size,
                ra.mtime,
                rb.mtime
            ));
            return report;
        }
        report.compared_metadata += 1;
        let a_has_id = ra.file_id.iter().any(|&b| b != 0);
        let b_has_id = rb.file_id.iter().any(|&b| b != 0);
        if a_has_id && b_has_id {
            if ra.file_id != rb.file_id {
                report.first_mismatch = Some(format!(
                    "file id mismatch for {:?}",
                    crate::util::utf16_to_string(na)
                ));
                return report;
            }
            report.compared_ids += 1;
        }
        if (ra.flags & FLAG_REPARSE) != (rb.flags & FLAG_REPARSE) {
            report.first_mismatch = Some(format!(
                "reparse flag mismatch for {:?}",
                crate::util::utf16_to_string(na)
            ));
            return report;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_is_48_bytes() {
        assert_eq!(core::mem::size_of::<EntryRec>(), 48);
    }

    #[test]
    fn names_share_one_buffer() {
        let mut s = EntryStore::new();
        s.push(&[b'a' as u16], 0, 1, 2, 0, [0; 16]);
        s.push(&[b'b' as u16, b'c' as u16], ATTR_DIRECTORY, 3, 4, 0, [0; 16]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.names.len(), 3);
        assert_eq!(s.name(0), &[b'a' as u16]);
        assert_eq!(s.name(1), &[b'b' as u16, b'c' as u16]);
        assert_eq!(s.recs[1].flags & FLAG_DIR, FLAG_DIR);
    }

    #[test]
    fn dots_are_dropped() {
        let mut s = EntryStore::new();
        s.push(&[0x2E], ATTR_DIRECTORY, 0, 0, 0, [0; 16]);
        s.push(&[0x2E, 0x2E], ATTR_DIRECTORY, 0, 0, 0, [0; 16]);
        s.push(&[0x2E, b'x' as u16], 0, 0, 0, 0, [0; 16]);
        assert_eq!(s.len(), 1);
        assert_eq!(s.dots_skipped, 2);
    }

    #[test]
    fn compare_ignores_missing_ids() {
        let mut a = EntryStore::new();
        let mut b = EntryStore::new();
        a.push(&[1, 2], 0, 10, 20, 0, [0; 16]);
        b.push(&[1, 2], 0, 10, 20, 0, [9; 16]);
        let r = compare(&a, &b);
        assert!(r.is_equal());
        assert_eq!(r.compared_ids, 0);

        let mut c = EntryStore::new();
        c.push(&[1, 2], 0, 10, 20, 0, [7; 16]);
        let r = compare(&b, &c);
        assert!(!r.is_equal());
    }
}
