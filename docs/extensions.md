# Extensions

Code: `crates/tsubame-ext-api` (traits, data types, registry; no dependencies,
no Win32) and `src/ext.rs` (the app's registry and its built-in extensions).
Tests: `cargo test -p tsubame-ext-api` (registry semantics) and the app's
`cargo test` (built-ins, key map, columns, live switching).

Built-in features are the first extensions. They register at start-up
through the same `Registry::add` a TOML pack or a wasm plugin will use, and
the app dispatches through the registry. Tier 1 TOML packs load through it too
(`docs/extension-packs.md`); tier 2 wasm plugins are not loaded yet, see
`docs/PLAN.md` section 4.

## The API in one screen

| Piece | What it is |
|---|---|
| `Extension<H>` | `info()` (id, title, tier, required) and `contribute(&mut Contributions)`, called once by `add` |
| `Contributions` | `command`, `column`, `menu_item`, `previewer`, `vfs`, `search`, `panel`, `file_type`, `theme`; `order(n)` sets the order for the items that follow |
| `Host` | what the app plugs in: `Run` (how a command runs; built-ins use `fn(&mut View, &mut Ui, u32)`) and `Field` (a column the app draws itself) |
| `Registry<H>` | one `Slot` per extension point, plus `add`, `remove`, `set_enabled`, `set_disabled`, `disabled`, `command(id)`, `command_named(name)`, `generation()` |
| `Text` | `Cow<'static, str>`: built-ins borrow, so cloning them allocates nothing |

Extension points: commands (`CommandSpec` + handler), context-menu items
(`MenuItemSpec`, which run a command), columns (`ColumnSpec` + a host field or a
batched `ColumnProvider`: N paths in, N values out), previewers, vfs
providers, search providers and side panels (traits returning data or a
declared `Doc` tree the app draws), file-type rules and themes (plain data).

### Rules

- **Ids.** Extension ids are dotted and stable (`tsubame.core`,
  `tsubame.column.size`); the config uses them. Item names are unique per
  point, command numbers are unique. Any clash, or an empty name, rejects the
  whole extension and changes nothing.
- **Order.** Items are sorted by `order` (default 0), then by registration.
  Built-ins use 0, 10, 20 ... so later extensions can slot in between.
- **Enable / disable.** A disabled extension's items stay registered but are
  skipped by every iterator and lookup. Its names and ids stay reserved, so
  switching it back on can not fail. `required` extensions (the core) refuse
  to be switched off.
- **Persisted.** `[extensions] disabled = ["tsubame.column.type"]` in
  `config.toml` (`docs/config.md`). Unknown ids are kept, so a pack that is
  not installed yet keeps its setting.
- **Unregister.** `remove` tombstones the items. Indices stay valid until the
  next `add` or `compact`, so removing while walking by index is safe.
- **Caches.** Every change bumps `generation()`. The key map's command list
  and the details view's column set are rebuilt only when it moves; frames
  and key presses read a copy and allocate nothing.
- **Dynamic ids.** Commands from packs and plugins take numbers from
  `next_command_id()` (0x10000 and up); built-ins keep their fixed numbers.

## Worked example: a built-in command

A command that copies the current folder's path.

1. Give it a number in `src/commands.rs` (never reuse one; the shortcut
   table carries it):

   ```rust
   pub const CMD_COPY_FOLDER_PATH: u32 = 56;
   ```

2. Add a row to `BUILTIN`, where you want it in the palette. Name, title,
   default keys as text, flags (`G`: the key works even while a text field
   has focus; `F`: needs a folder in front, does nothing on the Settings tab;
   `GF` both, `NO` neither):

   ```rust
   cmd!(CMD_COPY_FOLDER_PATH, "path.copy_folder", "Copy folder path", ["Ctrl+Alt+C"], F),
   ```

3. Handle it in `View::run_builtin` (`src/view.rs`), calling a `View`
   method you write:

   ```rust
   CMD_COPY_FOLDER_PATH => self.copy_folder_path(),
   ```

That is all. The core extension registers the whole table, so the command is
in the command palette, in the key settings (rebindable, resets to its
default) and in the key map. The commands tests check that the new row's
number, name and default keys clash with nothing (add it to their table of
expected bindings too).

## Worked example: a built-in column

Columns are extensions of their own, so each can be switched off. A
host-drawn column ("Attributes", say) takes three steps:

1. Add a variant to `cols::Col` with its `slot()` and `sort_column()`, and
   widen the per-column arrays (`ColSet`, `Spans`, `Columns::w`) by one.

2. Add a row to `cols::BUILTIN`. `ext` is the id the config uses; `order`
   places it (10, 20, 30 are taken):

   ```rust
   Builtin { col: Col::Attributes, ext: "tsubame.column.attributes", title: "Attributes column",
             required: false, order: 40, spec: spec("core.attributes", "Attributes", Align::Left, 80.0, false) },
   ```

   `ext::builtin()` registers every row that is not part of the core as its own
   extension. Nothing else registers it.

3. Draw the cell in `View::draw_row`, behind `sp[slot].1 > 0.0` so a column
   that is switched off costs nothing.

The header, sort strip, resize handles and layout follow the registry's
order. Switching it off (`[extensions] disabled`, or
`View::set_extension_enabled`) gives its width to Name, live.

A column whose values come from outside the entry store registers
`ColumnSource::Batched(Box<dyn ColumnProvider>)` instead. The details view
does not draw provider columns yet; that arrives with wasm plugins.
