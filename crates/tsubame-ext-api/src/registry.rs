//! The registry: extensions, their items per point, enable state, ordering.
//!
//! Rules:
//! - Extension ids are unique; item names are unique per point; command ids
//!   are unique. A clash rejects the whole extension and changes nothing.
//! - Items are kept sorted by `(order, registration)`: lower `order` first,
//!   equal orders in the order they were added.
//! - A disabled extension keeps its items (names and ids stay reserved) but
//!   lookups and iteration skip them. Required extensions can not be disabled.
//! - `remove` (unregister) only marks items dead, so raw indices stay valid
//!   while a caller walks a point by index; the dead items are dropped by the
//!   next `add` or `compact`.
//! - Lookups never allocate. Every change bumps `generation`, so the app can
//!   cache what it derives (the key map, the column set) and rebuild only then.

use crate::points::*;
use crate::{Extension, ExtensionInfo, Host, Text};

/// Command ids from here up are free for packs and plugins
/// ([`Registry::next_command_id`]); built-ins stay below.
pub const DYNAMIC_COMMAND_BASE: u32 = 0x1_0000;

/// Handle of a registered extension. Never reused within one registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExtId(u32);

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    DuplicateExtension(Text),
    DuplicateItem { point: &'static str, name: String },
    DuplicateCommandId(u32),
    EmptyName { point: &'static str },
    UnknownExtension(String),
    /// A required extension can not be switched off.
    Required(Text),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::DuplicateExtension(id) => write!(f, "extension \"{id}\" is already registered"),
            Error::DuplicateItem { point, name } => write!(f, "{point} \"{name}\" is already registered"),
            Error::DuplicateCommandId(id) => write!(f, "command id {id} is already registered"),
            Error::EmptyName { point } => write!(f, "a {point} has no name"),
            Error::UnknownExtension(id) => write!(f, "no extension \"{id}\""),
            Error::Required(id) => write!(f, "extension \"{id}\" can not be switched off"),
        }
    }
}

impl std::error::Error for Error {}

/// One registered item and its bookkeeping.
pub struct Item<T> {
    pub ext: ExtId,
    pub order: i32,
    live: bool,
    on: bool,
    pub value: T,
}

impl<T> Item<T> {
    /// Registered and its extension switched on.
    pub fn enabled(&self) -> bool {
        self.live && self.on
    }
}

/// The items of one extension point, in order.
pub struct Slot<T> {
    items: Vec<Item<T>>,
    dead: usize,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot { items: Vec::new(), dead: 0 }
    }
}

impl<T: Point> Slot<T> {
    /// Enabled items, in order.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        self.items.iter().filter(|i| i.enabled()).map(|i| &i.value)
    }

    /// Registered items, enabled or not, in order (for a settings list).
    pub fn items(&self) -> impl Iterator<Item = &Item<T>> + '_ {
        self.items.iter().filter(|i| i.live)
    }

    /// Number of enabled items.
    pub fn len(&self) -> usize {
        self.items.iter().filter(|i| i.enabled()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Length for an index walk with [`Slot::at`]; stable across `remove`
    /// and `set_enabled`, changed by `add` and `compact`.
    pub fn raw_len(&self) -> usize {
        self.items.len()
    }

    /// The item at raw index `i` if it is enabled.
    pub fn at(&self, i: usize) -> Option<&T> {
        self.items.get(i).filter(|i| i.enabled()).map(|i| &i.value)
    }

    /// The enabled item named `name`.
    pub fn get(&self, name: &str) -> Option<&T> {
        self.items.iter().find(|i| i.enabled() && i.value.key() == name).map(|i| &i.value)
    }

    fn live_index(&self, name: &str) -> Option<usize> {
        self.items.iter().position(|i| i.live && i.value.key() == name)
    }

    fn check(&self, new: &[(i32, T)]) -> Result<(), Error> {
        for (k, (_, v)) in new.iter().enumerate() {
            let name = v.key();
            if name.is_empty() {
                return Err(Error::EmptyName { point: T::POINT });
            }
            if self.live_index(name).is_some() || new[..k].iter().any(|(_, o)| o.key() == name) {
                return Err(Error::DuplicateItem { point: T::POINT, name: name.to_string() });
            }
        }
        Ok(())
    }

    fn insert(&mut self, it: Item<T>) {
        let at = self.items.partition_point(|x| x.order <= it.order);
        self.items.insert(at, it);
    }

    fn set_on(&mut self, ext: ExtId, on: bool) {
        for i in self.items.iter_mut().filter(|i| i.ext == ext) {
            i.on = on;
        }
    }

    fn kill(&mut self, ext: ExtId) {
        for i in self.items.iter_mut().filter(|i| i.ext == ext && i.live) {
            i.live = false;
            self.dead += 1;
        }
    }

    fn compact(&mut self) -> bool {
        if self.dead == 0 {
            return false;
        }
        self.items.retain(|i| i.live);
        self.dead = 0;
        true
    }
}

/// What an extension adds, collected by [`Extension::contribute`].
pub struct Contributions<H: Host> {
    order: i32,
    commands: Vec<(i32, CommandDef<H>)>,
    columns: Vec<(i32, ColumnDef<H>)>,
    menu_items: Vec<(i32, MenuItemSpec)>,
    previewers: Vec<(i32, Provided<dyn Previewer>)>,
    vfs: Vec<(i32, Provided<dyn VfsProvider>)>,
    search: Vec<(i32, Provided<dyn SearchProvider>)>,
    panels: Vec<(i32, Provided<dyn SidePanel>)>,
    file_types: Vec<(i32, FileTypeRule)>,
    themes: Vec<(i32, ThemeSpec)>,
}

impl<H: Host> Contributions<H> {
    fn new() -> Self {
        Contributions {
            order: 0,
            commands: Vec::new(),
            columns: Vec::new(),
            menu_items: Vec::new(),
            previewers: Vec::new(),
            vfs: Vec::new(),
            search: Vec::new(),
            panels: Vec::new(),
            file_types: Vec::new(),
            themes: Vec::new(),
        }
    }

    /// The `order` of the items added after this call (default 0).
    pub fn order(&mut self, order: i32) -> &mut Self {
        self.order = order;
        self
    }

    pub fn command(&mut self, spec: CommandSpec, run: H::Run) -> &mut Self {
        self.commands.push((self.order, CommandDef { spec, run }));
        self
    }

    pub fn column(&mut self, spec: ColumnSpec, source: ColumnSource<H>) -> &mut Self {
        self.columns.push((self.order, ColumnDef { spec, source }));
        self
    }

    pub fn menu_item(&mut self, spec: MenuItemSpec) -> &mut Self {
        self.menu_items.push((self.order, spec));
        self
    }

    pub fn previewer(&mut self, id: Text, title: Text, p: Box<dyn Previewer>) -> &mut Self {
        self.previewers.push((self.order, Provided { id, title, provider: p }));
        self
    }

    pub fn vfs(&mut self, id: Text, title: Text, p: Box<dyn VfsProvider>) -> &mut Self {
        self.vfs.push((self.order, Provided { id, title, provider: p }));
        self
    }

    pub fn search(&mut self, id: Text, title: Text, p: Box<dyn SearchProvider>) -> &mut Self {
        self.search.push((self.order, Provided { id, title, provider: p }));
        self
    }

    pub fn panel(&mut self, id: Text, title: Text, p: Box<dyn SidePanel>) -> &mut Self {
        self.panels.push((self.order, Provided { id, title, provider: p }));
        self
    }

    pub fn file_type(&mut self, rule: FileTypeRule) -> &mut Self {
        self.file_types.push((self.order, rule));
        self
    }

    pub fn theme(&mut self, spec: ThemeSpec) -> &mut Self {
        self.themes.push((self.order, spec));
        self
    }
}

struct ExtState {
    id: ExtId,
    info: ExtensionInfo,
    enabled: bool,
    removed: bool,
}

/// Runs `$body` once per slot, with `$s` bound to it.
macro_rules! each_slot {
    ($r:expr, $s:ident => $body:expr) => {{
        {
            let $s = &mut $r.commands;
            $body;
        }
        {
            let $s = &mut $r.columns;
            $body;
        }
        {
            let $s = &mut $r.menu_items;
            $body;
        }
        {
            let $s = &mut $r.previewers;
            $body;
        }
        {
            let $s = &mut $r.vfs;
            $body;
        }
        {
            let $s = &mut $r.search;
            $body;
        }
        {
            let $s = &mut $r.panels;
            $body;
        }
        {
            let $s = &mut $r.file_types;
            $body;
        }
        {
            let $s = &mut $r.themes;
            $body;
        }
    }};
}

/// Every extension point's items. Read the slots directly; change them only
/// through `add`, `remove` and `set_enabled`.
pub struct Registry<H: Host> {
    exts: Vec<ExtState>,
    next_ext: u32,
    generation: u64,
    /// ids switched off, sorted; kept for ids not registered (yet), so a pack
    /// loaded later starts off and an older build never drops the entry
    disabled: Vec<String>,
    /// command id -> raw index in `commands`, sorted by id
    cmd_index: Vec<(u32, u32)>,
    pub commands: Slot<CommandDef<H>>,
    pub columns: Slot<ColumnDef<H>>,
    pub menu_items: Slot<MenuItemSpec>,
    pub previewers: Slot<Provided<dyn Previewer>>,
    pub vfs: Slot<Provided<dyn VfsProvider>>,
    pub search: Slot<Provided<dyn SearchProvider>>,
    pub panels: Slot<Provided<dyn SidePanel>>,
    pub file_types: Slot<FileTypeRule>,
    pub themes: Slot<ThemeSpec>,
}

impl<H: Host> Default for Registry<H> {
    fn default() -> Self {
        Registry::new()
    }
}

impl<H: Host> Registry<H> {
    pub fn new() -> Self {
        Registry {
            exts: Vec::new(),
            next_ext: 0,
            generation: 0,
            disabled: Vec::new(),
            cmd_index: Vec::new(),
            commands: Slot::default(),
            columns: Slot::default(),
            menu_items: Slot::default(),
            previewers: Slot::default(),
            vfs: Slot::default(),
            search: Slot::default(),
            panels: Slot::default(),
            file_types: Slot::default(),
            themes: Slot::default(),
        }
    }

    /// Changes on every add, remove, enable change and compaction.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Registers `ext` and everything it contributes, or nothing (see the
    /// module rules). Starts disabled when its id is on the disabled list.
    pub fn add(&mut self, ext: &dyn Extension<H>) -> Result<ExtId, Error> {
        let info = ext.info();
        if info.id.is_empty() {
            return Err(Error::EmptyName { point: "extension" });
        }
        if self.live_ext(&info.id).is_some() {
            return Err(Error::DuplicateExtension(info.id));
        }
        let mut c = Contributions::new();
        ext.contribute(&mut c);
        self.commands.check(&c.commands)?;
        for (k, (_, d)) in c.commands.iter().enumerate() {
            let id = d.spec.id;
            if self.live_command(id).is_some() || c.commands[..k].iter().any(|(_, o)| o.spec.id == id) {
                return Err(Error::DuplicateCommandId(id));
            }
        }
        self.columns.check(&c.columns)?;
        self.menu_items.check(&c.menu_items)?;
        self.previewers.check(&c.previewers)?;
        self.vfs.check(&c.vfs)?;
        self.search.check(&c.search)?;
        self.panels.check(&c.panels)?;
        self.file_types.check(&c.file_types)?;
        self.themes.check(&c.themes)?;

        // valid: commit
        self.compact_quiet();
        let id = ExtId(self.next_ext);
        self.next_ext += 1;
        let on = info.required || !self.is_listed_off(&info.id);
        // `insert` puts each item after the ones of equal order: registration order
        macro_rules! commit {
            ($slot:expr, $list:expr) => {
                for (order, value) in $list {
                    $slot.insert(Item { ext: id, order, live: true, on, value });
                }
            };
        }
        commit!(self.commands, c.commands);
        commit!(self.columns, c.columns);
        commit!(self.menu_items, c.menu_items);
        commit!(self.previewers, c.previewers);
        commit!(self.vfs, c.vfs);
        commit!(self.search, c.search);
        commit!(self.panels, c.panels);
        commit!(self.file_types, c.file_types);
        commit!(self.themes, c.themes);
        self.exts.push(ExtState { id, info, enabled: on, removed: false });
        self.rebuild_index();
        self.generation += 1;
        Ok(id)
    }

    /// Unregisters extension `id`: its items disappear from lookups and
    /// iteration at once. Safe in the middle of an index walk. False when no
    /// such extension is registered.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(i) = self.live_ext(id) else { return false };
        self.exts[i].removed = true;
        let ext = self.exts[i].id;
        each_slot!(self, s => s.kill(ext));
        self.generation += 1;
        true
    }

    /// Drops unregistered items now (indices change).
    pub fn compact(&mut self) {
        if self.compact_quiet() {
            self.generation += 1;
        }
    }

    fn compact_quiet(&mut self) -> bool {
        let mut any = false;
        each_slot!(self, s => any |= s.compact());
        let before = self.exts.len();
        self.exts.retain(|e| !e.removed);
        if any || before != self.exts.len() {
            self.rebuild_index();
            return true;
        }
        false
    }

    /// Switches extension `id` on or off. Ok(true) when it changed.
    pub fn set_enabled(&mut self, id: &str, on: bool) -> Result<bool, Error> {
        let Some(i) = self.live_ext(id) else { return Err(Error::UnknownExtension(id.to_string())) };
        if !on && self.exts[i].info.required {
            return Err(Error::Required(self.exts[i].info.id.clone()));
        }
        match (on, self.disabled.binary_search_by(|d| d.as_str().cmp(id))) {
            (true, Ok(k)) => {
                self.disabled.remove(k);
            }
            (false, Err(k)) => self.disabled.insert(k, id.to_string()),
            _ => {}
        }
        if self.exts[i].enabled == on {
            return Ok(false);
        }
        self.exts[i].enabled = on;
        let ext = self.exts[i].id;
        each_slot!(self, s => s.set_on(ext, on));
        self.generation += 1;
        Ok(true)
    }

    /// `Some(enabled)` for a registered extension.
    pub fn is_enabled(&self, id: &str) -> Option<bool> {
        self.live_ext(id).map(|i| self.exts[i].enabled)
    }

    /// Registered extensions in registration order, with their enable state.
    pub fn extensions(&self) -> impl Iterator<Item = (&ExtensionInfo, bool)> + '_ {
        self.exts.iter().filter(|e| !e.removed).map(|e| (&e.info, e.enabled))
    }

    /// The extension that contributed an item (`Item::ext`).
    pub fn info(&self, ext: ExtId) -> Option<&ExtensionInfo> {
        self.exts.iter().find(|e| e.id == ext && !e.removed).map(|e| &e.info)
    }

    /// Loads the persisted list of switched-off ids (the config's
    /// `[extensions] disabled`). Ids not registered are remembered.
    pub fn set_disabled<S: AsRef<str>>(&mut self, ids: &[S]) {
        let mut list: Vec<String> = ids.iter().map(|s| s.as_ref().trim().to_string()).filter(|s| !s.is_empty()).collect();
        list.sort();
        list.dedup();
        self.disabled = list;
        let mut changed = false;
        for i in 0..self.exts.len() {
            if self.exts[i].removed {
                continue;
            }
            let on = self.exts[i].info.required || !self.is_listed_off(&self.exts[i].info.id);
            if self.exts[i].enabled != on {
                self.exts[i].enabled = on;
                let ext = self.exts[i].id;
                each_slot!(self, s => s.set_on(ext, on));
                changed = true;
            }
        }
        if changed {
            self.generation += 1;
        }
    }

    /// The list to persist: switched-off ids, sorted, including ids not
    /// registered now; never a required extension.
    pub fn disabled(&self) -> Vec<String> {
        self.disabled.iter().filter(|d| self.live_ext(d).is_none_or(|i| !self.exts[i].info.required)).cloned().collect()
    }

    /// The enabled command with this id. O(log n), no allocation.
    pub fn command(&self, id: u32) -> Option<&CommandDef<H>> {
        self.cmd_index.binary_search_by_key(&id, |e| e.0).ok().and_then(|k| self.commands.at(self.cmd_index[k].1 as usize))
    }

    /// The enabled command with this dotted name.
    pub fn command_named(&self, name: &str) -> Option<&CommandDef<H>> {
        self.commands.get(name)
    }

    /// True when a command of this name is registered, enabled or not.
    pub fn knows_command(&self, name: &str) -> bool {
        self.commands.live_index(name).is_some()
    }

    /// The lowest free command id at or above [`DYNAMIC_COMMAND_BASE`].
    pub fn next_command_id(&self) -> u32 {
        let mut id = DYNAMIC_COMMAND_BASE;
        while self.live_command(id).is_some() {
            id += 1;
        }
        id
    }

    fn live_command(&self, id: u32) -> Option<usize> {
        let k = self.cmd_index.binary_search_by_key(&id, |e| e.0).ok()?;
        let raw = self.cmd_index[k].1 as usize;
        self.commands.items.get(raw).filter(|i| i.live).map(|_| raw)
    }

    fn live_ext(&self, id: &str) -> Option<usize> {
        self.exts.iter().position(|e| !e.removed && e.info.id == id)
    }

    fn is_listed_off(&self, id: &str) -> bool {
        self.disabled.binary_search_by(|d| d.as_str().cmp(id)).is_ok()
    }

    fn rebuild_index(&mut self) {
        self.cmd_index.clear();
        for (raw, it) in self.commands.items.iter().enumerate() {
            if it.live {
                self.cmd_index.push((it.value.spec.id, raw as u32));
            }
        }
        self.cmd_index.sort_unstable_by_key(|e| e.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tier, text};
    use std::borrow::Cow;

    struct T;
    impl Host for T {
        type Run = fn(&mut Vec<u32>, u32);
        type Field = u8;
    }

    fn push(log: &mut Vec<u32>, id: u32) {
        log.push(id);
    }

    /// An extension from a list of (id, name, order) commands and column ids.
    struct Ext {
        id: &'static str,
        required: bool,
        cmds: Vec<(u32, &'static str, i32)>,
        cols: Vec<(&'static str, i32)>,
    }

    fn ext(id: &'static str, cmds: &[(u32, &'static str, i32)]) -> Ext {
        Ext { id, required: false, cmds: cmds.to_vec(), cols: Vec::new() }
    }

    impl Extension<T> for Ext {
        fn info(&self) -> ExtensionInfo {
            ExtensionInfo { id: text(self.id), title: text("test"), tier: Tier::BuiltIn, required: self.required }
        }
        fn contribute(&self, c: &mut Contributions<T>) {
            for &(id, name, order) in &self.cmds {
                c.order(order).command(CommandSpec::builtin(id, name, name, &[], CmdFlags::NONE), push);
            }
            for &(name, order) in &self.cols {
                c.order(order).column(ColumnSpec { id: text(name), label: text(name), align: Align::Left, width: 10.0, flex: false }, ColumnSource::Field(0));
            }
        }
    }

    fn names(r: &Registry<T>) -> Vec<String> {
        r.commands.iter().map(|c| c.spec.name.to_string()).collect()
    }

    #[test]
    fn items_are_ordered_by_order_then_registration() {
        let mut r = Registry::<T>::new();
        r.add(&ext("a", &[(1, "a.one", 0), (2, "a.two", 0), (3, "a.late", 50)])).unwrap();
        r.add(&ext("b", &[(4, "b.first", -10), (5, "b.mid", 0)])).unwrap();
        assert_eq!(names(&r), ["b.first", "a.one", "a.two", "b.mid", "a.late"]);
        let mut c = ext("c", &[]);
        c.cols = vec![("size", 30), ("name", 0), ("type", 20), ("date", 10)];
        r.add(&c).unwrap();
        let cols: Vec<&str> = r.columns.iter().map(|c| &*c.spec.id).collect();
        assert_eq!(cols, ["name", "date", "type", "size"]);
    }

    #[test]
    fn duplicate_ids_are_rejected_and_change_nothing() {
        let mut r = Registry::<T>::new();
        r.add(&ext("a", &[(1, "a.one", 0)])).unwrap();
        let g = r.generation();
        assert_eq!(r.add(&ext("a", &[(9, "a.other", 0)])), Err(Error::DuplicateExtension(text("a"))));
        // second item clashes by name: the first one is not registered either
        let e = r.add(&ext("b", &[(2, "b.fine", 0), (3, "a.one", 0)])).unwrap_err();
        assert_eq!(e, Error::DuplicateItem { point: "command", name: "a.one".into() });
        assert_eq!(r.add(&ext("b", &[(1, "b.clash", 0)])), Err(Error::DuplicateCommandId(1)));
        assert_eq!(r.add(&ext("b", &[(7, "b.x", 0), (7, "b.y", 0)])), Err(Error::DuplicateCommandId(7)));
        assert_eq!(r.add(&ext("b", &[(8, "b.x", 0), (9, "b.x", 0)])).unwrap_err(), Error::DuplicateItem { point: "command", name: "b.x".into() });
        assert_eq!(r.add(&ext("b", &[(8, "", 0)])), Err(Error::EmptyName { point: "command" }));
        assert_eq!(r.add(&ext("", &[])), Err(Error::EmptyName { point: "extension" }));
        assert_eq!(r.generation(), g, "a rejected add changes nothing");
        assert_eq!(names(&r), ["a.one"]);
        assert!(r.command(2).is_none() && r.is_enabled("b").is_none());
        // the error says what clashed
        assert!(Error::DuplicateCommandId(1).to_string().contains('1'));
    }

    #[test]
    fn disable_hides_items_and_enable_brings_them_back_in_place() {
        let mut r = Registry::<T>::new();
        r.add(&ext("a", &[(1, "a.one", 0)])).unwrap();
        r.add(&ext("b", &[(2, "b.one", 0)])).unwrap();
        r.add(&ext("c", &[(3, "c.one", 0)])).unwrap();
        assert_eq!(r.set_enabled("b", false), Ok(true));
        assert_eq!(r.set_enabled("b", false), Ok(false), "no change, no generation bump");
        assert_eq!(names(&r), ["a.one", "c.one"]);
        assert!(r.command(2).is_none() && r.command_named("b.one").is_none());
        assert!(r.knows_command("b.one"), "a disabled command keeps its name");
        assert_eq!(r.add(&ext("d", &[(2, "d.one", 0)])), Err(Error::DuplicateCommandId(2)), "and its id");
        assert_eq!(r.commands.len(), 2);
        assert_eq!(r.commands.items().count(), 3);
        r.set_enabled("b", true).unwrap();
        assert_eq!(names(&r), ["a.one", "b.one", "c.one"]);
        assert_eq!(r.set_enabled("nope", false), Err(Error::UnknownExtension("nope".into())));
    }

    #[test]
    fn required_extensions_can_not_be_switched_off() {
        let mut r = Registry::<T>::new();
        let mut core = ext("core", &[(1, "core.one", 0)]);
        core.required = true;
        r.add(&core).unwrap();
        assert_eq!(r.set_enabled("core", false), Err(Error::Required(text("core"))));
        r.set_disabled(&["core"]);
        assert_eq!(r.is_enabled("core"), Some(true));
        assert!(r.disabled().is_empty(), "never persisted as off");
    }

    #[test]
    fn the_disabled_list_round_trips_and_keeps_unknown_ids() {
        let mut r = Registry::<T>::new();
        // loaded from the config before anything registered
        r.set_disabled(&["pack.later", "b", " b ", ""]);
        r.add(&ext("a", &[(1, "a.one", 0)])).unwrap();
        r.add(&ext("b", &[(2, "b.one", 0)])).unwrap();
        assert_eq!(r.is_enabled("b"), Some(false), "starts off");
        assert_eq!(names(&r), ["a.one"]);
        assert_eq!(r.disabled(), ["b", "pack.later"]);
        r.set_enabled("a", false).unwrap();
        r.set_enabled("b", true).unwrap();
        assert_eq!(r.disabled(), ["a", "pack.later"]);
        // a fresh registry from the saved list ends up the same
        let mut r2 = Registry::<T>::new();
        r2.set_disabled(&r.disabled());
        r2.add(&ext("a", &[(1, "a.one", 0)])).unwrap();
        r2.add(&ext("b", &[(2, "b.one", 0)])).unwrap();
        assert_eq!(names(&r2), ["b.one"]);
        // applying a list later switches registered extensions live
        r2.set_disabled::<&str>(&[]);
        assert_eq!(names(&r2), ["a.one", "b.one"]);
    }

    #[test]
    fn unregister_removes_items_and_frees_names() {
        let mut r = Registry::<T>::new();
        r.add(&ext("a", &[(1, "a.one", 0)])).unwrap();
        r.add(&ext("b", &[(2, "b.one", 0)])).unwrap();
        assert!(r.remove("a"));
        assert!(!r.remove("a"), "already gone");
        assert_eq!(names(&r), ["b.one"]);
        assert!(r.command(1).is_none() && !r.knows_command("a.one") && r.is_enabled("a").is_none());
        // same id and names again
        r.add(&ext("a", &[(1, "a.one", 0)])).unwrap();
        assert_eq!(names(&r), ["b.one", "a.one"]);
        assert_eq!(r.commands.raw_len(), 2, "the dead item was compacted by add");
        assert_eq!(r.extensions().count(), 2);
    }

    #[test]
    fn unregister_and_disable_during_an_index_walk_are_safe() {
        let mut r = Registry::<T>::new();
        r.add(&ext("a", &[(1, "a.1", 0), (2, "a.2", 0)])).unwrap();
        r.add(&ext("b", &[(3, "b.1", 0)])).unwrap();
        r.add(&ext("c", &[(4, "c.1", 0), (5, "c.2", 0)])).unwrap();
        r.add(&ext("d", &[(6, "d.1", 0)])).unwrap();
        let mut seen = Vec::new();
        let mut i = 0;
        while i < r.commands.raw_len() {
            let name = r.commands.at(i).map(|c| c.spec.name.to_string());
            if let Some(n) = name {
                if n == "a.1" {
                    // what is behind and in front of the cursor goes away
                    assert!(r.remove("a"));
                    assert!(r.remove("c"));
                    r.set_enabled("d", false).unwrap();
                }
                seen.push(n);
            }
            i += 1;
        }
        assert_eq!(seen, ["a.1", "b.1"]);
        assert_eq!(r.commands.raw_len(), 6, "nothing moved during the walk");
        r.compact();
        assert_eq!(r.commands.raw_len(), 2);
        r.set_enabled("d", true).unwrap();
        assert_eq!(names(&r), ["b.1", "d.1"]);
        assert_eq!(r.command(6).map(|c| c.spec.id), Some(6), "the index follows compaction");
    }

    #[test]
    fn handlers_dispatch_by_id_and_name() {
        let mut r = Registry::<T>::new();
        r.add(&ext("a", &[(10, "a.ten", 0), (3, "a.three", 0)])).unwrap();
        let mut log = Vec::new();
        for id in [3, 10, 99] {
            if let Some(c) = r.command(id) {
                (c.run)(&mut log, id);
            }
        }
        assert_eq!(log, [3, 10]);
        assert_eq!(r.command_named("a.ten").map(|c| c.spec.id), Some(10));
        assert!(r.command_named("a.nine").is_none());
    }

    #[test]
    fn dynamic_command_ids_skip_used_ones() {
        let mut r = Registry::<T>::new();
        assert_eq!(r.next_command_id(), DYNAMIC_COMMAND_BASE);
        r.add(&ext("p", &[(DYNAMIC_COMMAND_BASE, "p.a", 0), (DYNAMIC_COMMAND_BASE + 1, "p.b", 0)])).unwrap();
        assert_eq!(r.next_command_id(), DYNAMIC_COMMAND_BASE + 2);
        r.remove("p");
        assert_eq!(r.next_command_id(), DYNAMIC_COMMAND_BASE);
    }

    #[test]
    fn generation_moves_on_changes_only() {
        let mut r = Registry::<T>::new();
        let g0 = r.generation();
        r.add(&ext("a", &[(1, "a.one", 0)])).unwrap();
        let g1 = r.generation();
        assert!(g1 > g0);
        let _ = r.command(1);
        let _ = r.commands.iter().count();
        assert_eq!(r.generation(), g1, "lookups change nothing");
        r.set_enabled("a", false).unwrap();
        assert!(r.generation() > g1);
    }

    #[test]
    fn every_point_registers_and_checks_names() {
        struct P;
        impl Previewer for P {
            fn accepts(&self, ext: &str) -> bool {
                ext == "txt"
            }
            fn preview(&self, _: &str, _: Limits, out: &mut Doc) -> Result<(), String> {
                out.nodes.push(Node::Mono("hi".into()));
                Ok(())
            }
        }
        struct All;
        impl Extension<T> for All {
            fn info(&self) -> ExtensionInfo {
                ExtensionInfo::builtin("all", "All", false)
            }
            fn contribute(&self, c: &mut Contributions<T>) {
                c.menu_item(MenuItemSpec { id: text("m"), label: text("M"), command: text("a.one"), when: MenuWhen::FILES, types: Cow::Borrowed(&[]) })
                    .previewer(text("txt"), text("Text"), Box::new(P))
                    .file_type(FileTypeRule { id: text("img"), extensions: Cow::Owned(vec![text("png")]), color: Some(0xff0000), icon: None, open_with: None })
                    .theme(ThemeSpec { id: text("t"), name: text("T"), dark: true, colors: Cow::Borrowed(&[]) });
            }
        }
        let mut r = Registry::<T>::new();
        r.add(&All).unwrap();
        assert_eq!(r.menu_items.len(), 1);
        assert_eq!(r.file_types.get("img").and_then(|f| f.color), Some(0xff0000));
        assert_eq!(r.themes.iter().count(), 1);
        let p = r.previewers.get("txt").unwrap();
        let mut d = Doc::default();
        assert!(p.provider.accepts("txt"));
        p.provider.preview("x.txt", Limits { max_bytes: 1, max_px: 1 }, &mut d).unwrap();
        assert_eq!(d.nodes, [Node::Mono("hi".into())]);
        // a second extension with the same previewer id is refused
        struct Again;
        impl Extension<T> for Again {
            fn info(&self) -> ExtensionInfo {
                ExtensionInfo::builtin("again", "Again", false)
            }
            fn contribute(&self, c: &mut Contributions<T>) {
                c.previewer(text("txt"), text("Text"), Box::new(P));
            }
        }
        assert_eq!(r.add(&Again), Err(Error::DuplicateItem { point: "previewer", name: "txt".into() }));
        r.set_enabled("all", false).unwrap();
        assert!(r.previewers.is_empty() && r.menu_items.is_empty() && r.themes.is_empty());
    }

    mod alloc {
        //! Lookups must not allocate (they run on every key press and frame).
        use super::*;
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        struct Counting;
        thread_local! {
            static N: Cell<u64> = const { Cell::new(0) };
        }
        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, l: Layout) -> *mut u8 {
                let _ = N.try_with(|n| n.set(n.get() + 1));
                unsafe { System.alloc(l) }
            }
            unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
                unsafe { System.dealloc(p, l) }
            }
        }
        #[global_allocator]
        static A: Counting = Counting;

        #[test]
        fn lookups_do_not_allocate() {
            let mut r = Registry::<T>::new();
            let cmds: Vec<(u32, &'static str, i32)> = vec![(1, "a.1", 0), (2, "a.2", 0), (3, "a.3", 5)];
            r.add(&ext("a", &cmds)).unwrap();
            let mut c = ext("c", &[]);
            c.cols = vec![("name", 0), ("size", 1)];
            r.add(&c).unwrap();
            let before = N.with(|n| n.get());
            let mut sum = 0u64;
            for _ in 0..100 {
                sum += r.command(2).map(|c| c.spec.id as u64).unwrap_or(0);
                sum += r.command_named("a.3").map(|c| c.spec.id as u64).unwrap_or(0);
                sum += r.commands.iter().count() as u64 + r.columns.iter().map(|c| c.spec.width as u64).sum::<u64>();
                sum += r.is_enabled("c").map(u64::from).unwrap_or(0) + r.generation();
            }
            assert_eq!(N.with(|n| n.get()), before, "no allocation in lookups");
            assert!(sum > 0);
        }
    }
}
