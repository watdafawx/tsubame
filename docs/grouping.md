# Grouping model (B11-F foundation)

This is an implementation foundation; grouped rendering is not wired into the
app yet. No Group by command or configuration option is exposed.

- `tsubame-core::grouping`: grouping modes, ASCII initial-letter keys, exact
  size boundaries, and calendar date classification. `DateBoundaries` takes UTC
  instants of local midnights from the caller; it never approximates local days
  as 86,400 seconds. Platform boundary construction remains to be implemented.
- `tsubame-ui::widgets::grouping`: cached `Header(group)` / `Item(source_index)`
  rows, stable grouping of already-sorted input, descending group order,
  collapse/expand (including all groups), constant-time row/item mapping and
  visible-only item iteration. Group keys preserve collapse state on rebuild.

Rebuild after sort/filter/listing changes. Toggle rebuilds flat rows once. Draw
code can read `rows()` without allocating. Keyboard navigation can use
`next_item`; marquee, invert and type-ahead can use `visible_items`. Ctrl+A must
continue using the original source to include collapsed items. Collapse state
contains only current group keys; leaving a folder must clear the model.

Tests cover exact size boundaries, name keys, supplied local midnight and DST
boundaries, overlapping calendar buckets, stable group order, mapping at 100k
items, collapse retention, navigation and visible selection candidates.

Remaining B11-F integration: type keys; actual local calendar boundaries;
per-tab/session/config state; commands 200–219; checked Group by / Sort by
menus; list and grid header paint, pointer and keyboard handling; source index
translation for selection, drag, activation and UIA; UIA Group items; rig
verification and real 4K scroll benchmark per `docs/bench.md`. Model tests are
not evidence of meeting the scroll paint budget.
