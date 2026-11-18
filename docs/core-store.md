# C1 portable entry store

Std only; no OS APIs or dependencies. `EntryRecord` is exactly 24 bytes:
size u64, modified i64, UTF-16 offset u32, length u16, flags u16.
Modified is an opaque signed timestamp; platform must consistently supply its chosen
unit/epoch (no precision lost). Flags: folder, hidden, system, readonly, reparse.
One `Vec<u16>` holds names, one `Vec<EntryRecord>` holds metadata, one
`Vec<u32>` holds visible sorted IDs. No per-entry allocation.

## API and semantics

- `EntryStore::new` / `with_capacity(entries, name_units)`; `generation()` tags batches.
- `push_batch(generation, &[EntryInput])` validates entire batch before mutation;
  rejects stale generation, >65535-unit names, u32 offset/ID exhaustion. Allocation
  failure follows Rust allocator policy. IDs remain stable until `clear()`.
- `record(id)`, `name(id)` return optional borrowed data; `at(position)` returns ID;
  `view()` borrows sorted/filtered IDs. `len()` counts live entries; `id_bound()` is the
  ID space (live + tombstoned), `is_live(id)` tells which.
- `clear()` advances generation, empties listing/filter, retains capacities and sort.
- `sort(Sort { column, descending, folders_first })` sorts visible IDs. Folders stay
  first in either direction. Equal keys retain insertion order (ID tie-break).
- Append sorts only matching new IDs, then merges old/new indices in O(n+m).
  Full sort decorates with temporary u64 prefix keys; numeric runs encode significant
  length and digits without integer overflow. Prefix ties use full natural comparison.
- Streaming (PERF2): `begin_stream()` before the first batch of a listing,
  `end_stream()` at Done. While streaming, `push_batch` keeps only the top
  `STREAM_TOP` (2048) rows of the view sorted (what the first screens show)
  and appends the rest unsorted, so a batch costs O(batch · log) instead of an
  O(n) merge (100k in 256/4096 batches: 107–134 ms of merges before, 11 ms
  now). `end_stream()` sorts the whole view once. `apply()`, `sort()`,
  `filter()` and `clear()` keep the top consistent; `apply()` ends a stream.
  The listing is complete and sorted only after `end_stream()`.
- Full sort (`sort_ids`): below 16k IDs serial; above, a sample sort on
  `std::thread::scope` threads (at most 8): sampled splitters on the
  (folder rank, prefix key) rank, threads split chunks into buckets, then
  sort the buckets in parallel. Within a bucket, entries sort by prefix key,
  and runs of equal keys refine with the next 8 name bytes (Name/Type) until
  they differ, the name ends (naturally equal: ID order) or a key is unclean
  (a digit run cut at 8 bytes, a non-ASCII unit or NUL), which falls back to
  the full natural comparison. Size/Modified ties sort by ID. Result equals
  the stable natural sort (tests check against a naive sort on random,
  unicode and tie-heavy names).
- `filter(query, Substring|Glob)` is case-insensitive. Substring extensions scan only
  previous results; edits/backspace/mode changes rebuild from all IDs. When a
  filter starts on the complete sorted view, that order is kept (4 bytes per
  entry while filtered, freed when the filter is cleared; dropped by any
  mutation or re-sort): a rebuild then filters it in one ordered pass instead
  of sorting the matches again. Glob is anchored,
  `*` any units and `?` one UTF-16 unit, iterative backtracking, no escape syntax.
- Natural comparison treats ASCII digit runs numerically; leading-zero variants and
  case variants tie. Simple one-to-one BMP lowercase supports typical Latin/Greek/
  Cyrillic names; no locale collation, normalization, expanding case folds or
  supplementary-plane case folding. Invalid UTF-16 units are preserved/compared.
  Type compares suffix after final dot, including dotfiles; absent/trailing suffix empty.
- `Selection` uses entry IDs, bitset plus per-word epochs. Single/toggle clicks are
  O(1) after capacity growth; lazy clear avoids scanning a million bits. Range/keyboard
  `extend` resolves anchor in current view and selects inclusive range; missing anchor
  starts at destination. Re-sort/filter preserve hidden selections. Select-all/invert
  operate on visible IDs. Count and u128 byte total update incrementally. Every mutation
  binds generation, resetting selection for new listings. Callers must bind through a
  mutation before reading selection after directory replacement. Anchor/focus are IDs.

## Incremental updates (C4)

For the directory watcher: change a listing without a relist.

- `apply(&removed, &[Upsert]) -> Result<Vec<EntryId>, StoreError>`: one call, one
  sort merge. `Upsert { id: Option<EntryId>, input: EntryInput }`: `Some(live id)`
  rewrites that entry in place, name included (rename = same ID, so selection and
  focus follow it); anything else inserts. Removed IDs become tombstones (`DEAD`
  flag, invisible) on a free-list; later inserts reuse them. Returns the upserts' IDs,
  in order.
- `find(name)` / `find_many(&[name])`: live-entry lookup, exact match first, else
  case-insensitive. A few names scan once each; many hash once and scan the store once.
- `compact_names()`: rebuilds the name buffer without dead names (`garbage_units()`
  counts them). `apply` calls it when garbage is over 16 Ki units and over half the
  buffer. IDs and order keep.
- `Selection::forget(&store, id)`: call before `apply` removes `id`; deselects it and
  clears anchor/focus if it was them. `Selection::resized(&store, id, old_size)`: call
  after an in-place update to keep the selected-bytes total exact.
- `id_bound()` is the ID space (live + tombstoned), `is_live(id)` tells which; `len()`
  is the live count. Anything indexed by ID (selection bitset) sizes by `id_bound()`.
- `clear()` still resets everything, tombstones and free-list included.

## Memory

Resident allocated bytes reported from actual Vec capacities, excluding allocator
headers, input batches and temporary sort/merge scratch. Formula: 24N + 2U + 4V
plus query capacity. At 1M entries with average 11 UTF-16 units: ~50 MB;
selection adds ~250 KB (bits + epoch words). Benchmark reserves 256 spare entries:
**50,011,776 bytes**. <=60 MB gate **PASS**; strict <=50,000,000-byte stretch
**FAIL** by 11,776 bytes. Long names necessarily increase memory; 60 MB cannot
hold arbitrary 255-unit names. Full sort scratch uses 16 bytes/visible entry
serial, up to 32 bytes/visible entry in the parallel sample sort (buckets plus
the gathered bucket; ~32 MB at 1M, freed when the sort returns); merge
temporarily retains old and new ID vectors. Peak memory exceeds resident figure.

## FOX CCD1 measurements, 2026-11-17

Release via `pwsh -NoProfile -File X:\Dev\tools\cargo-ccd1.ps1 run -p tsubame-core --release --example store_bench`.
CCD1 cpus 16–31, game running, -j8. One run; load-sensitive, not median/p95.
Fixture cycles 1024 short mixed hex/numeric names with 17 extensions in permuted
arrival order, duplicate-heavy to exercise stable ties. Metadata permuted independently;
1% folders. Build includes fixture/input construction and initial natural sort.
Subsequent full sorts descend, folders-first off. This is a synthetic workload,
not a replacement for S2's real unique-name directory fixture or rig gate runs.

| Metric | 100k | 1M | Gate |
|---|---:|---:|---|
| Build including initial sort (ms) | 15.809 | 328.858 | informational |
| Resident capacity bytes | 5,011,776 | 50,011,776 | <=60 MB PASS |
| Full name sort (ms) | 9.590 | 138.806 | <15 / <250 ms PASS; <8 ms stretch FAIL |
| Full size sort (ms) | 2.850 | 36.275 | informational |
| Full modified sort (ms) | 2.901 | 34.327 | informational |
| Full type sort (ms) | 22.549 | 470.766 | informational; no column-specific gate |
| Merge 256 rows (ms) | 0.095 | 0.945 | informational |
| Substring `00.` (ms) | 2.644 | 22.269 | 100k <5 ms PASS |
| Refine `00.0` (ms) | 0.022 | 0.574 | informational |
| Glob `*00.?` (ms) | 1.950 | 12.076 | informational |

Seven unit tests pass: numeric/zero/case/Unicode ordering, prefix consistency,
all-column/direction/folders merge vs full comparator, filtered streaming, filter/glob
edges, selection across re-sort/filter and generation/validation. `cargo clippy
-p tsubame-core --all-targets -- -D warnings` passes through CCD1 wrapper.
Rig comparison skipped. PLAN in this checkout has no P0 outcome block; S1/S2
reports read directly. No GUI launched.
