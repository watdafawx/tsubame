//! The editable right-click menu: the data model the Settings > Context menu
//! page edits, `[[context_menu.entries]]` in `config.toml`.
//!
//! The list is flat and ordered; a submenu's children point at it with
//! `parent`. Built-in entries (Open, Copy, ...) are part of the list so they
//! can be hidden, renamed and reordered, but never deleted. Custom entries run
//! a program (`command` + `args` with `{path}` / `{selection}` / `{dir}`
//! tokens); the argv is built by the app, never through a shell string.
//!
//! Pure data and rules, no Win32.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Submenu,
    Separator,
    #[default]
    #[serde(other)]
    Item,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// ships with Tsubame; can be hidden and renamed, not deleted
    Builtin,
    /// the Windows shell's own menu (Open with, Show more options)
    Windows,
    /// imported from a Nilesoft Shell `shell.nss`
    Nilesoft,
    /// added by the user (also what an unknown value reads as)
    #[default]
    #[serde(other)]
    Custom,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Builtin => "Built-in",
            Source::Windows => "Windows",
            Source::Custom => "Custom",
            Source::Nilesoft => "From Nilesoft",
        }
    }
    /// Entries of these sources run a program and are appended to the shell menu.
    pub fn runs_program(self) -> bool {
        matches!(self, Source::Custom | Source::Nilesoft)
    }
}

/// What the entry shows for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "ShowForRaw")]
pub struct ShowFor {
    pub files: bool,
    pub folders: bool,
    /// right click on the folder background
    pub background: bool,
    /// right click on 2 or more selected items
    pub multiple: bool,
    /// right click on a drive root (This PC, the sidebar)
    pub drives: bool,
}

impl Default for ShowFor {
    fn default() -> Self {
        ShowFor { files: true, folders: true, background: false, multiple: true, drives: true }
    }
}

impl ShowFor {
    /// The three classic kinds; `multiple` follows files / folders and
    /// `drives` follows folders, which is what such an entry did before the
    /// two kinds existed (a drive root counted as a folder, a selection by its
    /// first item).
    pub fn classic(files: bool, folders: bool, background: bool) -> ShowFor {
        ShowFor { files, folders, background, multiple: files || folders, drives: folders }
    }
}

/// `show_for` as an older file wrote it: no `multiple` / `drives`, which then
/// follow the entry's old behaviour ([`ShowFor::classic`]).
#[derive(Deserialize)]
#[serde(default)]
struct ShowForRaw {
    files: bool,
    folders: bool,
    background: bool,
    multiple: Option<bool>,
    drives: Option<bool>,
}

impl Default for ShowForRaw {
    fn default() -> Self {
        ShowForRaw { files: true, folders: true, background: false, multiple: None, drives: None }
    }
}

impl From<ShowForRaw> for ShowFor {
    fn from(r: ShowForRaw) -> ShowFor {
        let c = ShowFor::classic(r.files, r.folders, r.background);
        ShowFor { multiple: r.multiple.unwrap_or(c.multiple), drives: r.drives.unwrap_or(c.drives), ..c }
    }
}

/// What was right-clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    File,
    Folder,
    Background,
    /// 2 or more items
    Multiple,
    /// a drive root
    Drive,
}

impl Target {
    /// The bit [`ItemPref::kinds`] keeps for this kind.
    pub fn bit(self) -> u8 {
        match self {
            Target::File => 1,
            Target::Folder => 2,
            Target::Background => 4,
            Target::Multiple => 8,
            Target::Drive => 16,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MenuEntry {
    /// stable id: `builtin.copy`, `custom.3`, `nss.7`
    pub id: String,
    pub kind: EntryKind,
    pub source: Source,
    /// the label ("rename" edits it)
    pub name: String,
    /// custom entries: the program. Built-ins: the command name it runs.
    pub command: String,
    /// custom entries: arguments, split like a command line; `{path}`,
    /// `{selection}`, `{dir}` are replaced (see the app's `custom_cmd`)
    pub args: String,
    /// lower-case extensions without the dot (`["txt", "md"]`); empty = any.
    /// Applies to files only.
    pub extensions: Vec<String>,
    pub show_for: ShowFor,
    pub hidden: bool,
    /// id of the submenu this entry sits in; empty = top level
    pub parent: String,
    /// id of the user group (`[[context_menu.user_groups]]`) a top-level entry
    /// was placed in (the Nilesoft import's "Placed in group"); empty = the
    /// end of the menu
    #[serde(skip_serializing_if = "String::is_empty")]
    pub group: String,
}

impl MenuEntry {
    pub fn separator(id: &str) -> MenuEntry {
        MenuEntry { id: id.to_string(), kind: EntryKind::Separator, source: Source::Builtin, ..Default::default() }
    }
    pub fn builtin(id: &str, name: &str, command: &str) -> MenuEntry {
        MenuEntry { id: id.to_string(), source: Source::Builtin, name: name.to_string(), command: command.to_string(), show_for: ShowFor::default(), ..Default::default() }
    }
    pub fn custom(id: &str, name: &str, command: &str, args: &str) -> MenuEntry {
        MenuEntry { id: id.to_string(), source: Source::Custom, name: name.to_string(), command: command.to_string(), args: args.to_string(), ..Default::default() }
    }
    pub fn is_separator(&self) -> bool {
        self.kind == EntryKind::Separator
    }
    pub fn is_submenu(&self) -> bool {
        self.kind == EntryKind::Submenu
    }
    /// Does this program entry apply to `target` (with `ext` the lower-case
    /// extension of a file)?
    pub fn matches(&self, target: Target, ext: &str) -> bool {
        match target {
            Target::File => {
                self.show_for.files && (self.extensions.is_empty() || self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
            }
            Target::Folder => self.show_for.folders,
            Target::Background => self.show_for.background,
            // `ext` is the extension all the selected files share ("" = mixed)
            Target::Multiple => self.show_for.multiple && (self.extensions.is_empty() || (!ext.is_empty() && self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))),
            Target::Drive => self.show_for.drives,
        }
    }
}

/// The built-in default menu (the mock's list). Ids are stable: they are how a
/// saved list is matched against newer builds.
pub fn default_entries() -> Vec<MenuEntry> {
    let b = MenuEntry::builtin;
    let mut v = vec![
        b("builtin.open", "Open", "file.open"),
        b("builtin.open-tab", "Open in new tab", "tab.open-selected"),
        MenuEntry { source: Source::Windows, ..b("windows.open-with", "Open with", "") },
        MenuEntry::separator("sep.1"),
        b("builtin.cut", "Cut", "file.cut"),
        b("builtin.copy", "Copy", "file.copy"),
        b("builtin.paste", "Paste", "file.paste"),
        b("builtin.copy-path", "Copy as path", "file.copy-path"),
        MenuEntry::separator("sep.2"),
        b("builtin.rename", "Rename", "file.rename"),
        b("builtin.delete", "Delete", "file.delete"),
        MenuEntry::separator("sep.3"),
        MenuEntry { source: Source::Windows, ..b("windows.more", "Show more options", "") },
        b("builtin.properties", "Properties", "file.properties"),
    ];
    for e in &mut v {
        if e.id == "windows.open-with" || e.id == "windows.more" {
            e.show_for.background = false;
        }
    }
    v
}

/// The entries to show: the saved list, or the defaults when nothing was
/// saved. Built-ins missing from a saved list (a newer build added one) are
/// appended so they never disappear.
pub fn effective(saved: &[MenuEntry]) -> Vec<MenuEntry> {
    if saved.is_empty() {
        return default_entries();
    }
    let mut v = saved.to_vec();
    for d in default_entries() {
        if !d.is_separator() && !v.iter().any(|e| e.id == d.id) {
            v.push(d);
        }
    }
    v
}

/// A fresh id `<prefix>.<n>` that no entry uses.
pub fn next_id(entries: &[MenuEntry], prefix: &str) -> String {
    let mut n = entries.len() + 1;
    loop {
        let id = format!("{prefix}.{n}");
        if !entries.iter().any(|e| e.id == id) {
            return id;
        }
        n += 1;
    }
}

/// Moves entry `from` so it sits before the entry that is at `to` now (`to ==
/// len` = the end). A submenu moves with its children. Returns the new index
/// of the moved entry, or `None` when nothing changed.
pub fn move_entry(entries: &mut Vec<MenuEntry>, from: usize, to: usize) -> Option<usize> {
    if from >= entries.len() || to > entries.len() {
        return None;
    }
    // the block that moves: the entry and, for a submenu, everything under it
    let mut end = from + 1;
    if entries[from].is_submenu() {
        let id = entries[from].id.clone();
        while end < entries.len() && entries[end].parent == id {
            end += 1;
        }
    }
    if to > from && to <= end || to == from {
        return None;
    }
    let block: Vec<MenuEntry> = entries.drain(from..end).collect();
    let len = block.len();
    let at = if to > from { to - len } else { to };
    for (i, e) in block.into_iter().enumerate() {
        entries.insert(at + i, e);
    }
    Some(at)
}

/// Removes entry `i`: custom and imported ones go (a submenu takes its
/// children along); built-ins cannot be deleted. Returns whether it went.
pub fn delete_entry(entries: &mut Vec<MenuEntry>, i: usize) -> bool {
    let Some(e) = entries.get(i) else { return false };
    if e.source == Source::Builtin || e.source == Source::Windows {
        return e.is_separator() && {
            entries.remove(i);
            true
        };
    }
    let id = e.id.clone();
    let sub = e.is_submenu();
    entries.remove(i);
    if sub {
        entries.retain(|c| c.parent != id);
    }
    true
}

/// The enabled program entries that apply to `target`, in menu order, with
/// the submenu path flattened into the name ("Tools > Git Bash here"). These
/// are what the shell menu gets appended.
pub fn program_entries(entries: &[MenuEntry], target: Target, ext: &str) -> Vec<(MenuEntry, String)> {
    let mut out = Vec::new();
    for e in entries {
        if !e.source.runs_program() || e.kind != EntryKind::Item || e.hidden || e.command.trim().is_empty() || !e.matches(target, ext) {
            continue;
        }
        let mut label = e.name.clone();
        let mut parent = e.parent.as_str();
        let mut depth = 0;
        let mut hidden_parent = false;
        while !parent.is_empty() && depth < 4 {
            match entries.iter().find(|p| p.id == parent) {
                Some(p) => {
                    hidden_parent |= p.hidden;
                    label = format!("{} > {}", p.name, label);
                    parent = p.parent.as_str();
                }
                None => break,
            }
            depth += 1;
        }
        if !hidden_parent {
            out.push((e.clone(), label));
        }
    }
    out
}

// ---- items learned from real menus (CM8) ------------------------------------

/// `[[context_menu.items]]`: the user's view of an item the own-drawn menu
/// showed that is not in `entries` (Windows' classic and packaged items, and
/// Tsubame rows such as Copy name). Keyed by `key`, never by label:
/// `windows.verb:<verb>` (classic items with a canonical verb),
/// `windows.label:<label>` (classic items without one),
/// `windows.pkg:<package family>/{clsid}` (packaged Win11 items) and
/// `app:<action>` (Tsubame rows outside the list).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemPref {
    pub key: String,
    /// the label first seen (shown in Settings; not updated afterwards)
    pub label: String,
    /// where it comes from: `Windows`, `Tsubame` or a package family
    pub from: String,
    pub hidden: bool,
    /// the user's label; empty = the item's own
    pub name: String,
    /// appeared since the Settings page was last opened
    pub new: bool,
    /// id of the user group (`[[context_menu.user_groups]]`) the item was
    /// moved into; empty = the group it belongs to by origin
    pub group: String,
    /// drawn first in its group (Pin to top, from the menu's edit menu); an
    /// old file has none
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pin: bool,
    /// the kinds of right-click whose menu showed it ([`Target::bit`] ORed);
    /// 0 = learned before kinds were tracked
    #[serde(skip_serializing_if = "is_zero")]
    pub kinds: u8,
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

impl ItemPref {
    /// Is it listed for `kind` in Settings? File, Folder and Empty area list
    /// every learned item (as before); Several items and Drive only the ones
    /// a menu of that kind has shown.
    pub fn listed_for(&self, kind: Target) -> bool {
        match kind {
            Target::Multiple | Target::Drive => self.kinds & kind.bit() != 0,
            _ => true,
        }
    }
}

/// `[[context_menu.user_groups]]`: a group the user made. Items are moved into
/// it with [`ItemPref::group`]. `pos` is its place in the full group order
/// (0 = top), counted over the built-in groups and the user groups together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserGroup {
    pub id: String,
    pub name: String,
    pub pos: usize,
    /// "Name in menu": the group's block starts with a dim, non-interactive
    /// label row with its name (off = no header)
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub show_name: bool,
}

/// One slot of the full group order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupRef {
    Builtin(Group),
    User(String),
}

/// The full order to draw: the built-in groups as [`group_order`] gives them,
/// each user group inserted at its `pos` (past the end = last). A user group
/// with an empty or duplicate id is ignored.
pub fn full_order(saved: &[Group], users: &[UserGroup]) -> Vec<GroupRef> {
    let mut v: Vec<GroupRef> = group_order(saved).into_iter().map(GroupRef::Builtin).collect();
    let mut seen: Vec<&str> = Vec::new();
    let mut us: Vec<&UserGroup> = Vec::new();
    for u in users {
        if u.id.is_empty() || seen.contains(&u.id.as_str()) {
            continue;
        }
        seen.push(&u.id);
        us.push(u);
    }
    us.sort_by_key(|u| u.pos);
    for u in us {
        let at = u.pos.min(v.len());
        v.insert(at, GroupRef::User(u.id.clone()));
    }
    v
}

/// Stores `order` back: the built-in order in `saved` (empty when it is the
/// default) and each user group's `pos`.
pub fn set_full_order(saved: &mut Vec<Group>, users: &mut [UserGroup], order: &[GroupRef]) {
    let b: Vec<Group> = order.iter().filter_map(|g| if let GroupRef::Builtin(g) = g { Some(*g) } else { None }).collect();
    *saved = if b == Group::ALL { Vec::new() } else { b };
    for u in users.iter_mut() {
        if let Some(i) = order.iter().position(|g| *g == GroupRef::User(u.id.clone())) {
            u.pos = i;
        }
    }
}

/// A new user group named `name` (trimmed; empty = "New group") at the end of
/// the order. Returns its id.
pub fn add_group(users: &mut Vec<UserGroup>, saved: &[Group], name: &str) -> String {
    let mut n = 1;
    while users.iter().any(|u| u.id == format!("g{n}")) {
        n += 1;
    }
    let id = format!("g{n}");
    let name = name.trim();
    let pos = group_order(saved).len() + users.len();
    users.push(UserGroup { id: id.clone(), name: if name.is_empty() { "New group".into() } else { name.into() }, pos, show_name: false });
    id
}

/// Deletes user group `id`; its items go back to the group of their origin.
pub fn delete_group(users: &mut Vec<UserGroup>, items: &mut [ItemPref], id: &str) -> bool {
    let n = users.len();
    users.retain(|u| u.id != id);
    for p in items.iter_mut().filter(|p| p.group == id) {
        p.group.clear();
    }
    users.len() != n
}

/// Moves item `key` into user group `group` (empty = back to its origin
/// group). A group that does not exist counts as empty.
pub fn move_item_to_group(items: &mut [ItemPref], users: &[UserGroup], key: &str, group: &str) -> bool {
    let g = if users.iter().any(|u| u.id == group) { group } else { "" };
    match item_mut(items, key) {
        Some(p) if p.group != g => {
            p.group = g.to_string();
            true
        }
        _ => false,
    }
}

/// The "3rd party" chip. The classic menu hands out no owning program, so the
/// only evidence is a packaged (Win11) item whose package family is not
/// Microsoft's.
pub fn is_third_party(p: &ItemPref) -> bool {
    let f = p.from.as_str();
    !(f.is_empty() || f == "Windows" || f == "Tsubame" || p.key.starts_with("app:") || f.starts_with("Microsoft.") || f.starts_with("MicrosoftWindows.") || f.starts_with("Windows."))
}

/// The menu's groups, top to bottom (`groups` in `[context_menu]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// Tsubame's built-ins and own rows
    Tsubame,
    /// the classic shell items
    Windows,
    /// packaged (Win11) items of installed apps
    Apps,
    /// the user's custom and imported entries
    Custom,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Tsubame, Group::Windows, Group::Apps, Group::Custom];
    pub fn label(self) -> &'static str {
        match self {
            Group::Tsubame => "Tsubame",
            Group::Windows => "Windows",
            Group::Apps => "Installed apps",
            Group::Custom => "Custom",
        }
    }
}

/// The group order to draw: `saved` without duplicates, then any group it
/// lacks in the default order.
pub fn group_order(saved: &[Group]) -> Vec<Group> {
    let mut v: Vec<Group> = Vec::with_capacity(4);
    for g in saved.iter().chain(Group::ALL.iter()) {
        if !v.contains(g) {
            v.push(*g);
        }
    }
    v
}

/// Most items remembered; past it the oldest ones the user never edited go.
pub const MAX_ITEMS: usize = 400;

/// The pref for `key` (case-insensitive), if any.
pub fn item<'a>(items: &'a [ItemPref], key: &str) -> Option<&'a ItemPref> {
    if key.is_empty() {
        return None;
    }
    items.iter().find(|p| p.key.eq_ignore_ascii_case(key))
}

pub fn item_mut<'a>(items: &'a mut [ItemPref], key: &str) -> Option<&'a mut ItemPref> {
    if key.is_empty() {
        return None;
    }
    items.iter_mut().find(|p| p.key.eq_ignore_ascii_case(key))
}

/// Adds the items of `seen` (key, label, from) not known yet, tagged new and
/// visible. `fresh` = false marks them as already seen (the first menu after
/// an upgrade must not tag everything New). Every item of `seen` is also
/// marked as shown for `kind`. Returns whether `items` changed.
pub fn learn(items: &mut Vec<ItemPref>, seen: &[(String, String, String)], fresh: bool, kind: Target) -> bool {
    let mut changed = false;
    for (key, label, from) in seen {
        if key.is_empty() {
            continue;
        }
        if let Some(p) = item_mut(items, key) {
            if p.kinds & kind.bit() == 0 {
                p.kinds |= kind.bit();
                changed = true;
            }
            continue;
        }
        items.push(ItemPref { key: key.clone(), label: label.clone(), from: from.clone(), new: fresh, kinds: kind.bit(), ..Default::default() });
        changed = true;
    }
    while items.len() > MAX_ITEMS {
        match items.iter().position(|p| !p.hidden && p.name.is_empty()) {
            Some(i) => {
                items.remove(i);
            }
            None => break,
        }
    }
    changed
}

/// "Refresh from last menu": forgets the learned items the user never touched
/// (not hidden, renamed, pinned or moved into a group) that `shown` (the keys
/// of the last menu Tsubame drew) does not list. They come back the next time
/// a menu shows them. Returns how many went.
pub fn refresh_learned(items: &mut Vec<ItemPref>, shown: &[String]) -> usize {
    let n = items.len();
    items.retain(|p| p.hidden || p.pin || !p.name.is_empty() || !p.group.is_empty() || shown.iter().any(|k| k.eq_ignore_ascii_case(&p.key)));
    n - items.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_and_refresh_keep_old_configs_loading_and_round_trip() {
        // an old file: no `pin` anywhere
        let old = "[[items]]\nkey = \"windows.verb:share\"\nhidden = true\n";
        #[derive(Deserialize, Serialize)]
        struct W {
            items: Vec<ItemPref>,
        }
        let w: W = toml::from_str(old).unwrap();
        assert!(w.items[0].hidden && !w.items[0].pin);
        assert!(!toml::to_string(&W { items: w.items.clone() }).unwrap().contains("pin"), "an unpinned item writes no `pin`");
        let mut w2 = W { items: w.items.clone() };
        w2.items[0].pin = true;
        let back: W = toml::from_str(&toml::to_string(&w2).unwrap()).unwrap();
        assert!(back.items[0].pin && back.items[0].hidden);

        let k = |s: &str, f: &dyn Fn(&mut ItemPref)| {
            let mut p = ItemPref { key: s.into(), ..Default::default() };
            f(&mut p);
            p
        };
        let mut v = vec![
            k("windows.verb:old", &|_| {}),
            k("windows.verb:shown", &|_| {}),
            k("windows.verb:hid", &|p| p.hidden = true),
            k("windows.verb:pinned", &|p| p.pin = true),
            k("windows.verb:named", &|p| p.name = "X".into()),
            k("windows.verb:grouped", &|p| p.group = "g1".into()),
        ];
        assert_eq!(refresh_learned(&mut v, &["WINDOWS.VERB:SHOWN".to_string()]), 1);
        assert!(!v.iter().any(|p| p.key == "windows.verb:old"));
        assert_eq!(v.len(), 5);
    }

    #[test]
    fn user_groups_slot_into_the_order_and_round_trip() {
        let mut saved: Vec<Group> = Vec::new();
        let mut users: Vec<UserGroup> = Vec::new();
        let a = add_group(&mut users, &saved, "  Tools ");
        let b = add_group(&mut users, &saved, "");
        assert_eq!((a.as_str(), b.as_str(), users[0].name.as_str(), users[1].name.as_str()), ("g1", "g2", "Tools", "New group"));
        let mut o = full_order(&saved, &users);
        assert_eq!(o.len(), 6);
        assert_eq!(o[4], GroupRef::User("g1".into()));
        let g = o.remove(4);
        o.insert(1, g);
        set_full_order(&mut saved, &mut users, &o);
        assert_eq!(full_order(&saved, &users), o);
        assert!(saved.is_empty(), "built-in order untouched = default");
        // duplicate / empty ids are ignored, a far pos is last
        users.push(UserGroup { id: "g1".into(), name: "dup".into(), pos: 0, ..Default::default() });
        users.push(UserGroup { id: String::new(), name: "x".into(), pos: 0, ..Default::default() });
        users[1].pos = 99;
        assert_eq!(full_order(&saved, &users).len(), 6);
        assert_eq!(full_order(&saved, &users).last(), Some(&GroupRef::User("g2".into())));
    }

    #[test]
    fn deleting_a_group_frees_its_items() {
        let mut users = vec![UserGroup { id: "g1".into(), name: "T".into(), pos: 0, ..Default::default() }];
        let mut items = vec![ItemPref { key: "windows.verb:share".into(), ..Default::default() }];
        assert!(move_item_to_group(&mut items, &users, "windows.verb:SHARE", "g1"));
        assert_eq!(items[0].group, "g1");
        assert!(!move_item_to_group(&mut items, &users, "windows.verb:share", "g1"));
        assert!(move_item_to_group(&mut items, &users, "windows.verb:share", "nope"), "unknown group = origin");
        assert!(items[0].group.is_empty());
        move_item_to_group(&mut items, &users, "windows.verb:share", "g1");
        assert!(delete_group(&mut users, &mut items, "g1"));
        assert!(users.is_empty() && items[0].group.is_empty());
    }

    #[test]
    fn third_party_means_a_non_microsoft_package() {
        let p = |key: &str, from: &str| ItemPref { key: key.into(), from: from.into(), ..Default::default() };
        assert!(is_third_party(&p("windows.pkg:Foo.App_abc/{x}", "Foo.App_abc")));
        assert!(!is_third_party(&p("windows.pkg:Microsoft.WindowsTerminal_8w/{x}", "Microsoft.WindowsTerminal_8w")));
        assert!(!is_third_party(&p("windows.verb:share", "Windows")));
        assert!(!is_third_party(&p("windows.verb:x", "")));
        assert!(!is_third_party(&p("app:file.copy-name", "Tsubame")));
    }

    #[test]
    fn group_order_dedupes_and_appends_missing_groups() {
        assert_eq!(group_order(&[]), Group::ALL.to_vec());
        assert_eq!(group_order(&[Group::Custom, Group::Custom, Group::Windows]), vec![Group::Custom, Group::Windows, Group::Tsubame, Group::Apps]);
    }

    #[test]
    fn learn_adds_unknown_items_once_and_keeps_edited_ones_past_the_cap() {
        let s = |k: &str, l: &str| (k.to_string(), l.to_string(), "Windows".to_string());
        let mut v = vec![ItemPref { key: "windows.verb:share".into(), label: "Share".into(), hidden: true, ..Default::default() }];
        assert!(learn(&mut v, &[s("WINDOWS.VERB:SHARE", "Teilen"), s("windows.verb:print", "Print")], true, Target::File));
        assert_eq!(v.len(), 2);
        assert!(v[1].new && !v[1].hidden);
        assert_eq!(v[0].label, "Share", "a known item keeps its first label");
        assert!(!learn(&mut v, &[s("windows.verb:print", "Drucken")], true, Target::File));
        let many: Vec<_> = (0..MAX_ITEMS + 5).map(|i| s(&format!("windows.label:x{i}"), "x")).collect();
        learn(&mut v, &many, false, Target::File);
        assert_eq!(v.len(), MAX_ITEMS);
        assert!(item(&v, "windows.verb:share").is_some_and(|p| p.hidden), "edited items survive the cap");
        assert!(!item(&v, &format!("windows.label:x{}", MAX_ITEMS + 4)).unwrap().new);
    }

    #[test]
    fn defaults_have_unique_ids_and_the_mock_order() {
        let d = default_entries();
        let names: Vec<&str> = d.iter().filter(|e| !e.is_separator()).map(|e| e.name.as_str()).collect();
        assert_eq!(names[..3], ["Open", "Open in new tab", "Open with"]);
        for (i, a) in d.iter().enumerate() {
            assert!(!a.id.is_empty());
            assert!(d[i + 1..].iter().all(|b| b.id != a.id), "{}", a.id);
        }
    }

    #[test]
    fn empty_saved_list_is_the_default_and_missing_builtins_come_back() {
        assert_eq!(effective(&[]), default_entries());
        let saved: Vec<MenuEntry> = default_entries().into_iter().filter(|e| e.id != "builtin.cut").collect();
        let eff = effective(&saved);
        assert!(eff.iter().any(|e| e.id == "builtin.cut"));
        assert_eq!(eff.len(), saved.len() + 1);
    }

    #[test]
    fn moving_and_deleting() {
        let mut v = vec![
            MenuEntry::custom("a", "A", "a.exe", ""),
            MenuEntry { id: "s".into(), kind: EntryKind::Submenu, name: "S".into(), ..Default::default() },
            MenuEntry { parent: "s".into(), ..MenuEntry::custom("c", "C", "c.exe", "") },
            MenuEntry { parent: "s".into(), ..MenuEntry::custom("d", "D", "d.exe", "") },
            MenuEntry::custom("e", "E", "e.exe", ""),
        ];
        // submenu with children goes before A
        assert_eq!(move_entry(&mut v, 1, 0), Some(0));
        let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["s", "c", "d", "a", "e"]);
        // A to the end
        assert_eq!(move_entry(&mut v, 3, 5), Some(4));
        let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["s", "c", "d", "e", "a"]);
        // no-ops
        assert_eq!(move_entry(&mut v, 1, 1), None);
        assert_eq!(move_entry(&mut v, 1, 2), None);
        assert_eq!(move_entry(&mut v, 9, 0), None);
        // deleting a submenu takes its children
        assert!(delete_entry(&mut v, 0));
        let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["e", "a"]);
        // built-ins stay
        let mut d = default_entries();
        let n = d.len();
        assert!(!delete_entry(&mut d, 0));
        assert_eq!(d.len(), n);
        assert!(delete_entry(&mut d, 3), "a separator can go");
        assert_eq!(d.len(), n - 1);
    }

    #[test]
    fn program_entries_filter_by_target_extension_and_hidden() {
        let mut sub = MenuEntry { id: "tools".into(), kind: EntryKind::Submenu, name: "Tools".into(), source: Source::Custom, ..Default::default() };
        let a = MenuEntry { parent: "tools".into(), extensions: vec!["txt".into(), "MD".into()], ..MenuEntry::custom("a", "Edit", "ed.exe", "{path}") };
        let b = MenuEntry { show_for: ShowFor::classic(false, false, true), ..MenuEntry::custom("b", "Terminal here", "wt.exe", "-d {path}") };
        let c = MenuEntry { hidden: true, ..MenuEntry::custom("c", "Hidden", "h.exe", "") };
        let mut v = vec![sub.clone(), a, b, c, MenuEntry::builtin("builtin.copy", "Copy", "file.copy")];
        let files = program_entries(&v, Target::File, "md");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, "Tools > Edit");
        assert!(program_entries(&v, Target::File, "png").is_empty());
        let bg = program_entries(&v, Target::Background, "");
        assert_eq!(bg.len(), 1);
        assert_eq!(bg[0].1, "Terminal here");
        sub.hidden = true;
        v[0] = sub;
        assert!(program_entries(&v, Target::File, "md").is_empty(), "a hidden submenu hides its items");
    }

    #[test]
    fn an_old_show_for_keeps_its_kinds_and_the_new_ones_round_trip() {
        #[derive(Deserialize, Serialize)]
        struct W {
            show_for: ShowFor,
        }
        // written before "Several items" and "Drive": they follow the old behaviour
        let w: W = toml::from_str("[show_for]
files = true
folders = false
background = true
").unwrap();
        assert_eq!(w.show_for, ShowFor { files: true, folders: false, background: true, multiple: true, drives: false });
        let w: W = toml::from_str("[show_for]
files = false
folders = true
").unwrap();
        assert_eq!(w.show_for, ShowFor { files: false, folders: true, background: false, multiple: true, drives: true });
        let w: W = toml::from_str("[show_for]
background = true
files = false
folders = false
").unwrap();
        assert!(!w.show_for.multiple && !w.show_for.drives);
        // explicit values win and survive a save
        let w = W { show_for: ShowFor { files: true, folders: true, background: false, multiple: false, drives: true } };
        let back: W = toml::from_str(&toml::to_string(&w).unwrap()).unwrap();
        assert_eq!(back.show_for, w.show_for);
        // a missing table is the default
        #[derive(Deserialize)]
        struct E {
            #[serde(default)]
            show_for: ShowFor,
        }
        assert_eq!(toml::from_str::<E>("").unwrap().show_for, ShowFor::default());
    }

    #[test]
    fn several_items_and_drive_pick_their_own_entries() {
        let both = MenuEntry { show_for: ShowFor { multiple: true, drives: false, ..ShowFor::default() }, ..MenuEntry::custom("a", "A", "a.exe", "{selection}") };
        let drive = MenuEntry { show_for: ShowFor { multiple: false, drives: true, ..ShowFor::default() }, ..MenuEntry::custom("b", "B", "b.exe", "{path}") };
        let by_ext = MenuEntry { extensions: vec!["txt".into()], ..MenuEntry::custom("c", "C", "c.exe", "{selection}") };
        let v = vec![both, drive, by_ext];
        let names = |t: Target, ext: &str| program_entries(&v, t, ext).into_iter().map(|(e, _)| e.id).collect::<Vec<_>>();
        assert_eq!(names(Target::Multiple, ""), ["a"], "an extension filter needs every file to share it");
        assert_eq!(names(Target::Multiple, "txt"), ["a", "c"]);
        assert_eq!(names(Target::Drive, ""), ["b", "c"], "extensions only filter files");
        assert_eq!(names(Target::Folder, ""), ["a", "b", "c"]);
    }

    #[test]
    fn learning_remembers_which_kinds_showed_an_item() {
        let s = |k: &str| (k.to_string(), "x".to_string(), "Windows".to_string());
        let mut v = Vec::new();
        assert!(learn(&mut v, &[s("windows.verb:share")], false, Target::File));
        assert!(!learn(&mut v, &[s("windows.verb:share")], false, Target::File), "same kind again: no change");
        assert!(v[0].listed_for(Target::File) && v[0].listed_for(Target::Background));
        assert!(!v[0].listed_for(Target::Drive) && !v[0].listed_for(Target::Multiple));
        assert!(learn(&mut v, &[s("windows.verb:share")], false, Target::Drive), "a new kind is a change");
        assert!(v[0].listed_for(Target::Drive) && !v[0].listed_for(Target::Multiple));
        // an old pref (no kinds) lists for the old kinds only
        let old = ItemPref { key: "k".into(), ..Default::default() };
        assert!(old.listed_for(Target::Folder) && !old.listed_for(Target::Drive));
    }

    #[test]
    fn group_name_flag_round_trips_and_old_groups_have_none() {
        #[derive(Deserialize, Serialize)]
        struct W {
            g: Vec<UserGroup>,
        }
        let w: W = toml::from_str("[[g]]
id = \"g1\"
name = \"Tools\"
pos = 4
").unwrap();
        assert!(!w.g[0].show_name);
        assert!(!toml::to_string(&w).unwrap().contains("show_name"), "off writes nothing");
        let mut w = w;
        w.g[0].show_name = true;
        let back: W = toml::from_str(&toml::to_string(&w).unwrap()).unwrap();
        assert!(back.g[0].show_name);
    }
}
