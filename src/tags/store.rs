//! The tag store: which files carry which tags, kept in one central
//! `tags.db` next to `config.toml`. Never sidecar files, never NTFS streams.
//!
//! Items are keyed by (volume serial, 128-bit file id) on volumes whose ids
//! are stable (NTFS, ReFS), so a rename or a move on the same volume keeps a
//! file's tags; the last known path is kept as a hint (and is the key on
//! volumes without stable ids). The hint is refreshed whenever a lookup sees
//! the file under another path, by the watcher's renames and by the one-time
//! reconcile per volume.
//!
//! [`Store`] is the in-memory model (pure, no I/O but through [`Fs`]);
//! [`Db`] is the file around it: a lock file so only one process writes,
//! atomic saves, `tags.db.bak` from the last clean state, and a damaged
//! `tags.db` kept aside (`tags.db.bad`) instead of being overwritten.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::json::{self, J};
use super::model::{self, MAX_TAGS, Shape, TagDef};

/// Soft caps of the store (P4.F6): new items are refused past them.
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_BYTES: usize = 16 << 20;
/// Usage from which Settings and the status bar warn.
pub const WARN_AT: f32 = 0.9;

const MAGIC: &[u8; 8] = b"TSBTAGS1";
const VERSION: u32 = 1;

/// What identifies a tagged file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// volume serial + file id (stable across renames and moves on the volume)
    Id { vol: u64, fid: u128 },
    /// lower-cased full path (volumes without stable ids: FAT, exFAT, network)
    Path(String),
}

/// A volume as the store sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vol {
    pub serial: u64,
    /// file ids survive renames and moves (NTFS, ReFS)
    pub stable: bool,
    /// `C:\` or `\\server\share\`
    pub root: String,
}

/// The file system calls the store needs (a fake one in tests).
pub trait Fs {
    fn volume(&self, path: &str) -> Option<Vol>;
    /// The file id of `path` on its volume.
    fn file_id(&self, path: &str) -> Option<u128>;
    /// The current path of a file id (`None` when it is gone).
    fn path_by_id(&self, vol: &Vol, fid: u128) -> Option<String>;
    fn exists(&self, path: &str) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// last known path, as the file system spells it
    pub path: String,
    pub mask: u64,
    /// the last reconcile did not find the file (kept, never dropped silently)
    pub missing: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    /// past `MAX_ITEMS` / `MAX_BYTES`
    Full,
    /// all 64 tag slots used
    TooManyTags,
    BadName,
    DuplicateName,
    NoSuchTag,
}

/// What a reconcile pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recon {
    pub checked: usize,
    pub moved: usize,
    pub rekeyed: usize,
    pub missing: usize,
    /// stopped early (`progress` returned false)
    pub paused: bool,
}

/// Lower-cases and trims a path for comparisons (`C:\A\` -> `c:\a`).
pub fn norm(path: &str) -> String {
    let p = path.trim_end_matches(['\\', '/']);
    let p = if p.len() == 2 && p.ends_with(':') { path.get(..3).unwrap_or(p) } else { p };
    p.replace('/', "\\").to_lowercase()
}

/// `child` is `dir` or below it (both normalized).
fn under(child: &str, dir: &str) -> bool {
    child == dir || (child.len() > dir.len() && child.starts_with(dir) && (dir.ends_with('\\') || child.as_bytes()[dir.len()] == b'\\'))
}

fn item_bytes(key: &Key, path: &str) -> usize {
    let k = match key {
        Key::Id { .. } => 24,
        Key::Path(p) => p.len() + 2,
    };
    2 + k + 8 + 2 + path.len()
}

#[derive(Clone, Debug, Default)]
pub struct Store {
    pub defs: Vec<TagDef>,
    items: HashMap<Key, Item>,
    /// normalized path -> key
    by_path: HashMap<String, Key>,
    /// normalized volume root -> serial seen at the last reconcile
    vols: HashMap<String, u64>,
    bytes: usize,
    /// changed since the last save
    pub dirty: bool,
}

impl Store {
    /// A new store with the eight default tags.
    pub fn new() -> Store {
        Store { defs: model::defaults(), ..Store::default() }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    /// Fraction of the soft cap in use (the larger of items and bytes).
    pub fn usage(&self) -> f32 {
        (self.items.len() as f32 / MAX_ITEMS as f32).max(self.bytes as f32 / MAX_BYTES as f32)
    }
    fn full(&self, key: &Key, path: &str) -> bool {
        self.items.len() >= MAX_ITEMS || self.bytes + item_bytes(key, path) > MAX_BYTES
    }

    /// The key a file gets on `vol`: its id when ids are stable and known.
    pub fn key_for(vol: &Vol, path: &str, fid: u128) -> Key {
        if vol.stable && fid != 0 { Key::Id { vol: vol.serial, fid } } else { Key::Path(norm(path)) }
    }

    #[cfg(test)]
    pub fn get(&self, key: &Key) -> Option<&Item> {
        self.items.get(key)
    }
    #[cfg(test)]
    pub fn key_at(&self, path: &str) -> Option<&Key> {
        self.by_path.get(&norm(path))
    }

    fn insert(&mut self, key: Key, item: Item) {
        self.bytes += item_bytes(&key, &item.path);
        self.by_path.insert(norm(&item.path), key.clone());
        if let Some(old) = self.items.insert(key.clone(), item) {
            self.bytes -= item_bytes(&key, &old.path);
        }
        self.dirty = true;
    }

    fn take(&mut self, key: &Key) -> Option<Item> {
        let it = self.items.remove(key)?;
        self.bytes -= item_bytes(key, &it.path);
        let n = norm(&it.path);
        if self.by_path.get(&n) == Some(key) {
            self.by_path.remove(&n);
        }
        self.dirty = true;
        Some(it)
    }

    fn set_hint(&mut self, key: &Key, path: &str) {
        let Some(it) = self.items.get_mut(key) else { return };
        if it.path == path && !it.missing {
            return;
        }
        let old = norm(&it.path);
        let new = norm(path);
        self.bytes = self.bytes + path.len() - it.path.len();
        it.path = path.to_string();
        it.missing = false;
        if self.by_path.get(&old) == Some(key) {
            self.by_path.remove(&old);
        }
        self.by_path.insert(new, key.clone());
        self.dirty = true;
    }

    /// The tags of the file `key` now at `path`. Refreshes the path hint
    /// (a rename seen through its id), and follows a "safe save" (the
    /// editor replaced the file, so it has a new id at the same path).
    pub fn lookup(&mut self, key: &Key, path: &str) -> u64 {
        if let Some(it) = self.items.get(key) {
            let mask = it.mask;
            if it.path != path || it.missing {
                self.set_hint(key, path);
            }
            return mask;
        }
        let Key::Id { vol, .. } = key else { return 0 };
        // same path, other id on the same volume: the file was replaced
        let old = match self.by_path.get(&norm(path)) {
            Some(k @ Key::Id { vol: v, .. }) if v == vol => k.clone(),
            _ => return 0,
        };
        let Some(mut it) = self.take(&old) else { return 0 };
        it.path = path.to_string();
        it.missing = false;
        let mask = it.mask;
        self.insert(key.clone(), it);
        mask
    }

    /// Sets the tags of a file (`0` removes it from the store).
    pub fn set(&mut self, key: &Key, path: &str, mask: u64) -> Result<(), StoreError> {
        let mask = mask & self.used_bits();
        if mask == 0 {
            self.take(key);
            return Ok(());
        }
        if let Some(it) = self.items.get_mut(key) {
            if it.mask != mask {
                it.mask = mask;
                self.dirty = true;
            }
            self.set_hint(key, path);
            return Ok(());
        }
        if self.full(key, path) {
            return Err(StoreError::Full);
        }
        // a path-keyed or stale entry at the same path is replaced
        if let Some(k) = self.by_path.get(&norm(path)).cloned() {
            self.take(&k);
        }
        self.insert(key.clone(), Item { path: path.to_string(), mask, missing: false });
        Ok(())
    }

    /// A file or folder was renamed or moved (watcher, own operations):
    /// path hints below `from` move to `to`; path keys are re-keyed.
    pub fn rename(&mut self, from: &str, to: &str) -> usize {
        let (f, t) = (norm(from), to.trim_end_matches(['\\', '/']));
        let hit: Vec<(String, Key)> = self.by_path.iter().filter(|(p, _)| under(p, &f)).map(|(p, k)| (p.clone(), k.clone())).collect();
        let mut n = 0;
        for (p, key) in hit {
            let Some(it) = self.items.get(&key) else { continue };
            // keep the spelling of the part below the renamed folder
            let low_tail = &p[f.len()..];
            let tail = it.path.get(it.path.len().saturating_sub(low_tail.len())..).filter(|s| s.to_lowercase() == low_tail).unwrap_or(low_tail);
            let new_path = if p.len() == f.len() { t.to_string() } else { format!("{t}{tail}") };
            match &key {
                Key::Path(_) => {
                    if let Some(mut it) = self.take(&key) {
                        it.path = new_path.clone();
                        let k = Key::Path(norm(&new_path));
                        self.take(&k);
                        self.insert(k, it);
                    }
                }
                Key::Id { .. } => self.set_hint(&key, &new_path),
            }
            n += 1;
        }
        n
    }

    /// Every item with `bit` (not missing), sorted by path.
    pub fn query(&self, bit: u64) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self.items.values().filter(|i| i.mask & bit != 0 && !i.missing).map(|i| (i.path.clone(), i.mask)).collect();
        v.sort_by_cached_key(|(p, _)| norm(p));
        v
    }

    /// Items per tag slot (missing files are not counted).
    pub fn counts(&self) -> [u32; MAX_TAGS] {
        let mut c = [0u32; MAX_TAGS];
        for it in self.items.values().filter(|i| !i.missing) {
            let mut m = it.mask;
            while m != 0 {
                let b = m.trailing_zeros() as usize;
                c[b] += 1;
                m &= m - 1;
            }
        }
        c
    }

    fn used_bits(&self) -> u64 {
        self.defs.iter().fold(0, |m, d| m | d.bit())
    }

    #[cfg(test)]
    pub fn def(&self, slot: u8) -> Option<&TagDef> {
        self.defs.iter().find(|d| d.slot == slot)
    }
    pub fn find_tag(&self, name: &str) -> Option<&TagDef> {
        self.defs.iter().find(|d| d.name.eq_ignore_ascii_case(name.trim()) || d.name.to_lowercase() == name.trim().to_lowercase())
    }

    pub fn add_tag(&mut self, name: &str, colour: u8, shape: Shape) -> Result<u8, StoreError> {
        let name = model::clean_name(name).ok_or(StoreError::BadName)?;
        if self.find_tag(&name).is_some() {
            return Err(StoreError::DuplicateName);
        }
        let slot = model::free_slot(&self.defs).ok_or(StoreError::TooManyTags)?;
        self.defs.push(TagDef { slot, name, colour: colour % 8, shape });
        self.dirty = true;
        Ok(slot)
    }

    pub fn rename_tag(&mut self, slot: u8, name: &str) -> Result<(), StoreError> {
        let name = model::clean_name(name).ok_or(StoreError::BadName)?;
        if self.find_tag(&name).is_some_and(|d| d.slot != slot) {
            return Err(StoreError::DuplicateName);
        }
        let d = self.defs.iter_mut().find(|d| d.slot == slot).ok_or(StoreError::NoSuchTag)?;
        d.name = name;
        self.dirty = true;
        Ok(())
    }

    pub fn set_look(&mut self, slot: u8, colour: u8, shape: Shape) -> Result<(), StoreError> {
        let d = self.defs.iter_mut().find(|d| d.slot == slot).ok_or(StoreError::NoSuchTag)?;
        d.colour = colour % 8;
        d.shape = shape;
        self.dirty = true;
        Ok(())
    }

    /// Moves a tag one place up (`-1`) or down (`1`) in the list.
    pub fn move_tag(&mut self, slot: u8, by: i32) {
        let Some(i) = self.defs.iter().position(|d| d.slot == slot) else { return };
        let j = (i as i64 + by as i64).clamp(0, self.defs.len() as i64 - 1) as usize;
        if i != j {
            self.defs.swap(i, j);
            self.dirty = true;
        }
    }

    /// Deletes a tag: its bit is cleared on every item (items left with no
    /// tag leave the store).
    pub fn delete_tag(&mut self, slot: u8) -> Result<usize, StoreError> {
        let i = self.defs.iter().position(|d| d.slot == slot).ok_or(StoreError::NoSuchTag)?;
        let bit = self.defs.remove(i).bit();
        let keys: Vec<Key> = self.items.iter().filter(|(_, it)| it.mask & bit != 0).map(|(k, _)| k.clone()).collect();
        for k in &keys {
            let left = self.items.get(k).map_or(0, |it| it.mask & !bit);
            if left == 0 {
                self.take(k);
            } else if let Some(it) = self.items.get_mut(k) {
                it.mask = left;
            }
        }
        self.dirty = true;
        Ok(keys.len())
    }

    /// The volume was not reconciled with this serial yet (first sight, or
    /// another disk now answers to the same root).
    #[cfg(test)]
    pub fn vol_changed(&self, vol: &Vol) -> bool {
        self.vols.get(&norm(&vol.root)) != Some(&vol.serial)
    }

    /// One pass over the items on `vol`: ids that moved get their path
    /// back, replaced files are re-keyed, gone files are marked missing
    /// (never dropped). A changed volume serial re-keys by path.
    /// `progress(done, total)` returning false pauses it (call again).
    #[cfg(test)]
    pub fn reconcile(&mut self, fs: &dyn Fs, vol: &Vol, progress: &mut dyn FnMut(usize, usize) -> bool) -> Recon {
        let keys = self.reconcile_keys(vol);
        let total = keys.len();
        let mut r = Recon::default();
        for (n, key) in keys.iter().enumerate() {
            if n % 64 == 0 && !progress(n, total) {
                r.paused = true;
                return r;
            }
            self.reconcile_one(fs, vol, key, &mut r);
        }
        progress(total, total);
        self.reconcile_done(vol);
        r
    }

    /// The items a reconcile of `vol` checks (the worker runs them in slices).
    pub fn reconcile_keys(&self, vol: &Vol) -> Vec<Key> {
        let root = norm(&vol.root);
        let old_serial = self.vols.get(&root).copied();
        self.items
            .iter()
            .filter(|(k, it)| match k {
                Key::Id { vol: v, .. } => *v == vol.serial || ((old_serial != Some(vol.serial) || it.missing) && under(&norm(&it.path), &root)),
                Key::Path(p) => under(p, &root),
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Checks one item (gone since the keys were taken: skipped).
    pub fn reconcile_one(&mut self, fs: &dyn Fs, vol: &Vol, key: &Key, r: &mut Recon) {
        let Some(it) = self.items.get(key) else { return };
        r.checked += 1;
        let path = it.path.clone();
        match key {
            Key::Id { vol: v, fid } if *v == vol.serial && vol.stable => match fs.path_by_id(vol, *fid) {
                Some(p) => {
                    if p != path {
                        r.moved += 1;
                    }
                    self.set_hint(key, &p);
                }
                None => self.rekey_by_path(fs, vol, key, &path, r),
            },
            Key::Id { .. } => self.rekey_by_path(fs, vol, key, &path, r),
            Key::Path(_) => {
                if vol.stable && fs.exists(&path) {
                    self.rekey_by_path(fs, vol, key, &path, r);
                } else {
                    self.mark(key, !fs.exists(&path), r);
                }
            }
        }
    }

    /// Records that `vol` was reconciled with its current serial.
    pub fn reconcile_done(&mut self, vol: &Vol) {
        self.vols.insert(norm(&vol.root), vol.serial);
        self.dirty = true;
    }

    fn rekey_by_path(&mut self, fs: &dyn Fs, vol: &Vol, key: &Key, path: &str, r: &mut Recon) {
        let fid = if vol.stable { fs.file_id(path) } else { None };
        let new = match fid {
            Some(fid) => Key::Id { vol: vol.serial, fid },
            None if !vol.stable && fs.exists(path) => Key::Path(norm(path)),
            None => return self.mark(key, true, r),
        };
        if &new == key {
            return self.mark(key, false, r);
        }
        if let Some(mut it) = self.take(key) {
            it.missing = false;
            match self.items.get_mut(&new) {
                // the new key already has tags: union them
                Some(have) => have.mask |= it.mask,
                None => self.insert(new, it),
            }
            r.rekeyed += 1;
        }
    }

    fn mark(&mut self, key: &Key, missing: bool, r: &mut Recon) {
        if let Some(it) = self.items.get_mut(key)
            && it.missing != missing
        {
            it.missing = missing;
            self.dirty = true;
        }
        if missing {
            r.missing += 1;
        }
    }

    // ---- file format ---------------------------------------------------

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(64 + self.bytes);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&(self.defs.len() as u32).to_le_bytes());
        for d in &self.defs {
            b.extend_from_slice(&[d.slot, d.colour, d.shape as u8]);
            put_str(&mut b, &d.name);
        }
        let mut vols: Vec<(&String, &u64)> = self.vols.iter().collect();
        vols.sort();
        b.extend_from_slice(&(vols.len() as u32).to_le_bytes());
        for (root, serial) in vols {
            b.extend_from_slice(&serial.to_le_bytes());
            put_str(&mut b, root);
        }
        let mut items: Vec<(&Key, &Item)> = self.items.iter().collect();
        items.sort_by(|a, b| a.1.path.cmp(&b.1.path));
        b.extend_from_slice(&(items.len() as u32).to_le_bytes());
        for (k, it) in items {
            match k {
                Key::Id { vol, fid } => {
                    b.extend_from_slice(&[0, it.missing as u8]);
                    b.extend_from_slice(&vol.to_le_bytes());
                    b.extend_from_slice(&fid.to_le_bytes());
                }
                Key::Path(_) => b.extend_from_slice(&[1, it.missing as u8]),
            }
            b.extend_from_slice(&it.mask.to_le_bytes());
            put_str(&mut b, &it.path);
        }
        let sum = fnv(&b);
        b.extend_from_slice(&sum.to_le_bytes());
        b
    }

    pub fn from_bytes(data: &[u8]) -> Result<Store, &'static str> {
        if data.len() < 8 + 4 + 8 || &data[..8] != MAGIC {
            return Err("not a tags.db file");
        }
        let (body, sum) = data.split_at(data.len() - 8);
        if fnv(body).to_le_bytes() != sum {
            return Err("checksum mismatch");
        }
        let mut r = Rd { b: body, at: 8 };
        if r.u32()? != VERSION {
            return Err("unknown version");
        }
        let mut s = Store::default();
        let nt = r.u32()? as usize;
        if nt > MAX_TAGS {
            return Err("too many tags");
        }
        for _ in 0..nt {
            let (slot, colour, shape) = (r.u8()?, r.u8()?, r.u8()?);
            let name = r.str()?;
            if slot as usize >= MAX_TAGS || s.defs.iter().any(|d| d.slot == slot) {
                return Err("bad tag slot");
            }
            s.defs.push(TagDef { slot, name, colour: colour % 8, shape: Shape::from_u8(shape) });
        }
        for _ in 0..r.u32()? {
            let serial = r.u64()?;
            let root = r.str()?;
            s.vols.insert(root, serial);
        }
        let ni = r.u32()? as usize;
        if ni > MAX_ITEMS * 2 {
            return Err("too many items");
        }
        for _ in 0..ni {
            let (kind, missing) = (r.u8()?, r.u8()? != 0);
            let key_id = match kind {
                0 => Some((r.u64()?, r.u128()?)),
                1 => None,
                _ => return Err("bad item kind"),
            };
            let mask = r.u64()?;
            let path = r.str()?;
            let key = match key_id {
                Some((vol, fid)) => Key::Id { vol, fid },
                None => Key::Path(norm(&path)),
            };
            s.insert(key, Item { path, mask, missing });
        }
        if r.at != body.len() {
            return Err("trailing data");
        }
        s.dirty = false;
        Ok(s)
    }

    // ---- export / import -------------------------------------------------

    /// The whole store as JSON (tags by name, items by path).
    pub fn export_json(&self) -> String {
        let tags = J::Arr(
            self.defs
                .iter()
                .map(|d| J::obj([("name", J::Str(d.name.clone())), ("colour", J::Str(model::COLOURS[d.colour as usize % 8].into())), ("shape", J::Str(d.shape.id().into()))]))
                .collect(),
        );
        let mut items: Vec<&Item> = self.items.values().collect();
        items.sort_by(|a, b| a.path.cmp(&b.path));
        let items = J::Arr(
            items
                .into_iter()
                .map(|it| {
                    let names = self.defs.iter().filter(|d| it.mask & d.bit() != 0).map(|d| J::Str(d.name.clone())).collect();
                    J::obj([("path", J::Str(it.path.clone())), ("tags", J::Arr(names))])
                })
                .collect(),
        );
        json::write(&J::obj([("format", J::Str("tsubame-tags".into())), ("version", J::Num(1.0)), ("tags", tags), ("items", items)]))
    }

    /// Merges an export: tags are matched by name (missing ones created),
    /// items are added to (never replace) existing tags. `key_of` resolves a
    /// path to its key (`None`: skipped, the file is not here).
    /// Returns (tags created, items tagged, items skipped).
    pub fn import_json(&mut self, text: &str, key_of: &mut dyn FnMut(&str) -> Option<Key>) -> Result<(usize, usize, usize), String> {
        let root = json::parse(text)?;
        if root.get("format").and_then(J::as_str) != Some("tsubame-tags") {
            return Err("not a Tsubame tags export".into());
        }
        let mut made = 0;
        for t in root.get("tags").and_then(J::as_arr).unwrap_or(&[]) {
            let Some(name) = t.get("name").and_then(J::as_str) else { continue };
            if self.find_tag(name).is_some() {
                continue;
            }
            let colour = t.get("colour").and_then(J::as_str).and_then(model::parse_colour).unwrap_or(0);
            let shape = t.get("shape").and_then(J::as_str).and_then(Shape::parse).unwrap_or(Shape::ALL[colour as usize]);
            self.add_tag(name, colour, shape).map_err(|e| format!("cannot add tag \"{name}\": {e:?}"))?;
            made += 1;
        }
        let (mut done, mut skipped) = (0, 0);
        for it in root.get("items").and_then(J::as_arr).unwrap_or(&[]) {
            let Some(path) = it.get("path").and_then(J::as_str) else { continue };
            let mask = it.get("tags").and_then(J::as_arr).unwrap_or(&[]).iter().filter_map(J::as_str).filter_map(|n| self.find_tag(n)).fold(0u64, |m, d| m | d.bit());
            let Some(key) = (mask != 0).then(|| key_of(path)).flatten() else {
                skipped += 1;
                continue;
            };
            let have = self.lookup(&key, path);
            match self.set(&key, path, have | mask) {
                Ok(()) => done += 1,
                Err(StoreError::Full) => return Err(format!("the tag store is full ({MAX_ITEMS} items); {done} imported")),
                Err(e) => return Err(format!("{e:?}")),
            }
        }
        Ok((made, done, skipped))
    }
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    let s = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
    b.extend_from_slice(&(s.len() as u16).to_le_bytes());
    b.extend_from_slice(s);
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x0100_0000_01b3))
}

struct Rd<'a> {
    b: &'a [u8],
    at: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], &'static str> {
        let s = self.b.get(self.at..self.at + n).ok_or("truncated")?;
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn u128(&mut self) -> Result<u128, &'static str> {
        Ok(u128::from_le_bytes(self.take(16)?.try_into().unwrap()))
    }
    fn str(&mut self) -> Result<String, &'static str> {
        let n = u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "bad utf-8")
    }
}

// ---- the file --------------------------------------------------------------

/// Why the store cannot be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Rw,
    /// another Tsubame holds `tags.db.lock`
    Locked,
    /// the folder cannot be written (the reason)
    Unwritable(String),
}

pub struct Db {
    pub path: PathBuf,
    pub store: Store,
    pub mode: Mode,
    /// what happened at open, for the banner's Details
    pub note: Option<String>,
    lock: Option<fs::File>,
}

pub fn bak_path(path: &Path) -> PathBuf {
    let mut n = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    n.push(".bak");
    path.with_file_name(n)
}

fn lock_path(path: &Path) -> PathBuf {
    let mut n = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    n.push(".lock");
    path.with_file_name(n)
}

fn take_lock(path: &Path) -> Result<fs::File, Mode> {
    use std::os::windows::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    match fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(lock_path(path)) {
        Ok(f) => Ok(f),
        // ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION
        Err(e) if matches!(e.raw_os_error(), Some(32) | Some(33)) => Err(Mode::Locked),
        Err(e) => Err(Mode::Unwritable(e.to_string())),
    }
}

impl Db {
    /// Opens (or starts) the store at `path`. Never fails: a locked or
    /// unwritable store opens read-only, a damaged one falls back to its
    /// backup with the damaged file kept aside.
    pub fn open(path: &Path) -> Db {
        let (lock, mode) = match take_lock(path) {
            Ok(f) => (Some(f), Mode::Rw),
            Err(m) => (None, m),
        };
        let mut db = Db { path: path.to_path_buf(), store: Store::new(), mode, note: None, lock };
        db.load();
        db
    }

    fn load(&mut self) {
        let path = self.path.clone();
        let bak = bak_path(&path);
        let rw = self.mode == Mode::Rw;
        match fs::read(&path) {
            Ok(bytes) => match Store::from_bytes(&bytes) {
                Ok(s) => {
                    self.store = s;
                    // the last clean state
                    if rw && fs::read(&bak).ok().as_deref() != Some(&bytes[..]) {
                        let _ = tsubame_config::write_atomic(&bak, &bytes);
                    }
                }
                Err(why) => {
                    let kept = tsubame_config::corrupt_path(&path);
                    let from_bak = fs::read(&bak).ok().and_then(|b| Store::from_bytes(&b).ok());
                    if rw {
                        let _ = fs::rename(&path, &kept);
                    }
                    let keptn = kept.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    match from_bak {
                        Some(s) => {
                            self.store = s;
                            self.store.dirty = rw;
                            self.note = Some(format!("tags.db was damaged ({why}); restored from tags.db.bak. The damaged file was kept as {keptn}."));
                        }
                        None => {
                            self.note = Some(format!("tags.db was damaged ({why}) and has no usable backup. The damaged file was kept as {keptn}."));
                        }
                    }
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(s) = fs::read(&bak).ok().and_then(|b| Store::from_bytes(&b).ok()) {
                    self.store = s;
                    self.store.dirty = rw;
                    self.note = Some("tags.db was missing; restored from tags.db.bak.".into());
                }
            }
            Err(e) => {
                self.mode = Mode::Unwritable(e.to_string());
                self.lock = None;
                self.note = Some(format!("tags.db cannot be read: {e}"));
            }
        }
    }

    pub fn writable(&self) -> bool {
        self.mode == Mode::Rw
    }

    /// Tries to get the lock again (the banner's Retry); reloads on success.
    /// Unsaved read-only changes are not possible (writes are refused).
    pub fn retry(&mut self) -> bool {
        if self.writable() {
            return true;
        }
        match take_lock(&self.path) {
            Ok(f) => {
                self.lock = Some(f);
                self.mode = Mode::Rw;
                self.note = None;
                self.store = Store::new();
                self.load();
                self.writable()
            }
            Err(m) => {
                self.mode = m;
                false
            }
        }
    }

    /// Writes the store if it changed (atomic). Read-only: nothing is written.
    pub fn save(&mut self) -> io::Result<bool> {
        if !self.writable() || !self.store.dirty {
            return Ok(false);
        }
        tsubame_config::write_atomic(&self.path, &self.store.to_bytes())?;
        self.store.dirty = false;
        Ok(true)
    }

    /// Clean shutdown: saves and makes the saved state the backup.
    pub fn close(&mut self) -> io::Result<()> {
        self.save()?;
        if self.writable() && self.path.exists() {
            let bytes = fs::read(&self.path)?;
            if fs::read(bak_path(&self.path)).ok().as_deref() != Some(&bytes[..]) {
                tsubame_config::write_atomic(&bak_path(&self.path), &bytes)?;
            }
        }
        self.lock = None;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A fake volume: path -> id, ids can move and files vanish.
    #[derive(Default)]
    pub struct FakeFs {
        pub vols: RefCell<Vec<Vol>>,
        pub files: RefCell<Vec<(String, u64, u128)>>,
    }

    impl FakeFs {
        pub fn with(serial: u64, stable: bool) -> FakeFs {
            let f = FakeFs::default();
            f.vols.borrow_mut().push(Vol { serial, stable, root: "C:\\".into() });
            f
        }
        pub fn add(&self, path: &str, fid: u128) {
            let serial = self.volume(path).unwrap().serial;
            self.files.borrow_mut().push((path.into(), serial, fid));
        }
        pub fn mv(&self, from: &str, to: &str) {
            for f in self.files.borrow_mut().iter_mut() {
                if norm(&f.0) == norm(from) {
                    f.0 = to.into();
                }
            }
        }
        pub fn rm(&self, path: &str) {
            self.files.borrow_mut().retain(|f| norm(&f.0) != norm(path));
        }
        pub fn vol(&self) -> Vol {
            self.vols.borrow()[0].clone()
        }
    }

    impl Fs for FakeFs {
        fn volume(&self, path: &str) -> Option<Vol> {
            self.vols.borrow().iter().find(|v| under(&norm(path), &norm(&v.root))).cloned()
        }
        fn file_id(&self, path: &str) -> Option<u128> {
            self.files.borrow().iter().find(|f| norm(&f.0) == norm(path)).map(|f| f.2)
        }
        fn path_by_id(&self, vol: &Vol, fid: u128) -> Option<String> {
            self.files.borrow().iter().find(|f| f.1 == vol.serial && f.2 == fid).map(|f| f.0.clone())
        }
        fn exists(&self, path: &str) -> bool {
            self.file_id(path).is_some()
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-tags-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const URGENT: u64 = 1;
    const WORK: u64 = 1 << 4;

    #[test]
    fn round_trip_keeps_tags_items_and_volumes() {
        let fs_ = FakeFs::with(0xAB, true);
        fs_.add("C:\\w\\a.docx", 11);
        let mut s = Store::new();
        let vol = fs_.vol();
        s.set(&Store::key_for(&vol, "C:\\w\\a.docx", 11), "C:\\w\\a.docx", URGENT | WORK).unwrap();
        s.set(&Key::Path(norm("E:\\usb\\b.txt")), "E:\\usb\\b.txt", WORK).unwrap();
        s.add_tag("Taxes 2026", 2, Shape::Sq).unwrap();
        s.reconcile(&fs_, &vol, &mut |_, _| true);
        let b = s.to_bytes();
        let t = Store::from_bytes(&b).unwrap();
        assert_eq!(t.defs, s.defs);
        assert_eq!(t.len(), 2);
        assert_eq!(t.bytes(), s.bytes());
        assert_eq!(t.get(&Key::Id { vol: 0xAB, fid: 11 }).unwrap().mask, URGENT | WORK);
        assert!(!t.vol_changed(&vol));
        assert_eq!(t.to_bytes(), b, "stable encoding");
        assert!(!t.dirty);
    }

    #[test]
    fn rename_keeps_tags_by_id_and_rekeys_path_items() {
        let fs_ = FakeFs::with(1, true);
        fs_.add("C:\\w\\a.docx", 7);
        let vol = fs_.vol();
        let mut s = Store::new();
        let k = Store::key_for(&vol, "C:\\w\\a.docx", 7);
        s.set(&k, "C:\\w\\a.docx", URGENT).unwrap();
        // renamed: the listing sees the same id under another name
        assert_eq!(s.lookup(&k, "C:\\w\\contract.docx"), URGENT);
        assert_eq!(s.key_at("c:\\W\\CONTRACT.docx"), Some(&k));
        assert_eq!(s.key_at("C:\\w\\a.docx"), None);
        // a folder renamed: hints below it follow (watcher)
        assert_eq!(s.rename("C:\\w", "C:\\Work"), 1);
        assert_eq!(s.get(&k).unwrap().path, "C:\\Work\\contract.docx");
        // path keys (FAT) are re-keyed
        let p = Key::Path(norm("E:\\x\\n.txt"));
        s.set(&p, "E:\\x\\n.txt", WORK).unwrap();
        s.rename("E:\\x\\n.txt", "E:\\x\\m.txt");
        assert!(s.get(&p).is_none());
        assert_eq!(s.get(&Key::Path(norm("E:\\x\\m.txt"))).unwrap().mask, WORK);
        // a move seen by the reconcile
        fs_.mv("C:\\w\\a.docx", "C:\\Done\\contract.docx");
        let r = s.reconcile(&fs_, &vol, &mut |_, _| true);
        assert_eq!(r.moved, 1);
        assert_eq!(s.get(&k).unwrap().path, "C:\\Done\\contract.docx");
    }

    #[test]
    fn safe_save_with_new_id_keeps_tags() {
        let vol = Vol { serial: 1, stable: true, root: "C:\\".into() };
        let mut s = Store::new();
        s.set(&Store::key_for(&vol, "C:\\a.txt", 5), "C:\\a.txt", WORK).unwrap();
        let new = Store::key_for(&vol, "C:\\a.txt", 9);
        assert_eq!(s.lookup(&new, "C:\\a.txt"), WORK);
        assert_eq!(s.len(), 1);
        assert!(s.get(&new).is_some());
    }

    #[test]
    fn volume_change_rekeys_by_path_and_marks_missing() {
        let fs_ = FakeFs::with(1, true);
        fs_.add("C:\\a.txt", 5);
        fs_.add("C:\\b.txt", 6);
        let mut s = Store::new();
        let vol = fs_.vol();
        s.set(&Store::key_for(&vol, "C:\\a.txt", 5), "C:\\a.txt", URGENT).unwrap();
        s.set(&Store::key_for(&vol, "C:\\b.txt", 6), "C:\\b.txt", WORK).unwrap();
        s.reconcile(&fs_, &vol, &mut |_, _| true);
        // the volume was restored from an image: new serial, new ids, b is gone
        let fs2 = FakeFs::with(2, true);
        fs2.add("C:\\a.txt", 50);
        let vol2 = fs2.vol();
        assert!(s.vol_changed(&vol2));
        let r = s.reconcile(&fs2, &vol2, &mut |_, _| true);
        assert_eq!((r.rekeyed, r.missing), (1, 1));
        assert_eq!(s.get(&Key::Id { vol: 2, fid: 50 }).unwrap().mask, URGENT);
        // missing items are kept (never dropped silently) but not listed
        assert_eq!(s.len(), 2);
        assert!(s.query(WORK).is_empty());
        assert_eq!(s.counts()[4], 0);
        assert!(!s.vol_changed(&vol2));
        // and they come back when the file does
        fs2.add("C:\\b.txt", 60);
        s.reconcile(&fs2, &vol2, &mut |_, _| true);
        assert_eq!(s.query(WORK).len(), 1);
    }

    #[test]
    fn reconcile_pauses_and_resumes() {
        let fs_ = FakeFs::with(1, true);
        let vol = fs_.vol();
        let mut s = Store::new();
        for i in 0..200u128 {
            let p = format!("C:\\f{i}.txt");
            fs_.add(&p, i + 1);
            s.set(&Store::key_for(&vol, &p, i + 1), &p, URGENT).unwrap();
        }
        let r = s.reconcile(&fs_, &vol, &mut |done, _| done < 64);
        assert!(r.paused);
        assert!(s.vol_changed(&vol), "a paused pass is not done");
        let r = s.reconcile(&fs_, &vol, &mut |_, _| true);
        assert!(!r.paused);
        assert_eq!(r.checked, 200);
    }

    #[test]
    fn caps_and_tag_edits() {
        let mut s = Store::new();
        let vol = Vol { serial: 1, stable: true, root: "C:\\".into() };
        for i in 0..MAX_ITEMS as u128 {
            s.set(&Key::Id { vol: 1, fid: i + 1 }, &format!("C:\\x{i}"), URGENT).unwrap();
        }
        assert!(s.usage() >= 1.0);
        assert_eq!(s.set(&Store::key_for(&vol, "C:\\y", 1 << 70), "C:\\y", URGENT), Err(StoreError::Full));
        // editing an existing item still works when full
        s.set(&Key::Id { vol: 1, fid: 3 }, "C:\\x2", WORK).unwrap();
        // mask 0 removes
        s.set(&Key::Id { vol: 1, fid: 3 }, "C:\\x2", 0).unwrap();
        assert_eq!(s.len(), MAX_ITEMS - 1);

        let mut s = Store::new();
        assert_eq!(s.add_tag("urgent", 0, Shape::Tri), Err(StoreError::DuplicateName));
        assert_eq!(s.add_tag("  ", 0, Shape::Tri), Err(StoreError::BadName));
        let slot = s.add_tag("Taxes", 4, Shape::Hex).unwrap();
        assert_eq!(slot, 8);
        for i in 0..55 {
            s.add_tag(&format!("t{i}"), 0, Shape::Tri).unwrap();
        }
        assert_eq!(s.add_tag("one more", 0, Shape::Tri), Err(StoreError::TooManyTags));
        let k = Key::Id { vol: 1, fid: 1 };
        s.set(&k, "C:\\a", URGENT | WORK).unwrap();
        let k2 = Key::Id { vol: 1, fid: 2 };
        s.set(&k2, "C:\\b", URGENT).unwrap();
        assert_eq!(s.delete_tag(0).unwrap(), 2);
        assert_eq!(s.get(&k).unwrap().mask, WORK);
        assert!(s.get(&k2).is_none(), "no tags left: item leaves the store");
        // bits of deleted tags are never stored
        s.set(&k, "C:\\a", URGENT | WORK).unwrap();
        assert_eq!(s.get(&k).unwrap().mask, WORK);
        s.rename_tag(4, "Job").unwrap();
        assert_eq!(s.def(4).unwrap().name, "Job");
        assert_eq!(s.rename_tag(4, "Done"), Err(StoreError::DuplicateName));
    }

    #[test]
    fn corrupted_db_falls_back_to_bak_and_keeps_the_damaged_file() {
        let d = tmpdir("corrupt");
        let p = d.join("tags.db");
        {
            let mut db = Db::open(&p);
            assert!(db.writable());
            db.store.set(&Key::Id { vol: 1, fid: 1 }, "C:\\a", URGENT).unwrap();
            db.close().unwrap();
        }
        assert!(bak_path(&p).exists());
        // damage the main file (a torn write by something else)
        let mut b = fs::read(&p).unwrap();
        let n = b.len();
        b[n / 2] ^= 0xff;
        fs::write(&p, &b).unwrap();
        let mut db = Db::open(&p);
        assert!(db.writable());
        assert_eq!(db.store.get(&Key::Id { vol: 1, fid: 1 }).unwrap().mask, URGENT);
        assert!(db.note.as_deref().unwrap().contains("restored from tags.db.bak"));
        assert!(d.join("tags.db.bad").exists(), "the damaged file is kept");
        db.close().unwrap();
        assert!(Store::from_bytes(&fs::read(&p).unwrap()).is_ok(), "rewritten from the backup");
        // a missing db comes back from the backup as well
        fs::remove_file(&p).unwrap();
        let db = Db::open(&p);
        assert_eq!(db.store.len(), 1);
        drop(db);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn second_writer_is_read_only_until_retry() {
        let d = tmpdir("lock");
        let p = d.join("tags.db");
        let mut a = Db::open(&p);
        assert!(a.writable());
        let mut b = Db::open(&p);
        assert_eq!(b.mode, Mode::Locked);
        // b cannot write over a's data
        b.store.set(&Key::Id { vol: 1, fid: 9 }, "C:\\b", WORK).unwrap();
        assert!(!b.save().unwrap());
        assert!(!b.retry());
        a.store.set(&Key::Id { vol: 1, fid: 1 }, "C:\\a", URGENT).unwrap();
        assert!(a.save().unwrap());
        // writes from two threads to the one owner stay consistent
        let a = std::sync::Arc::new(std::sync::Mutex::new(a));
        let hs: Vec<_> = (0..4u128)
            .map(|t| {
                let a = a.clone();
                std::thread::spawn(move || {
                    for i in 0..50u128 {
                        let mut g = a.lock().unwrap();
                        g.store.set(&Key::Id { vol: 1, fid: 100 + t * 1000 + i }, &format!("C:\\t{t}\\{i}"), WORK).unwrap();
                        g.save().unwrap();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let mut a = std::sync::Arc::try_unwrap(a).ok().unwrap().into_inner().unwrap();
        a.close().unwrap();
        drop(a);
        assert!(b.retry());
        assert_eq!(b.store.len(), 201);
        assert!(b.store.get(&Key::Id { vol: 1, fid: 9 }).is_none(), "the refused change was not kept");
        drop(b);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn export_import_round_trip() {
        let mut s = Store::new();
        s.add_tag("Taxes \"2026\"", 4, Shape::Hex).unwrap();
        s.set(&Key::Id { vol: 1, fid: 1 }, "C:\\a\\é.txt", URGENT | (1 << 8)).unwrap();
        let text = s.export_json();
        let mut t = Store::new();
        let mut n = 0u128;
        let (made, done, skipped) = t
            .import_json(&text, &mut |p| {
                n += 1;
                (p != "C:\\gone").then_some(Key::Id { vol: 1, fid: n })
            })
            .unwrap();
        assert_eq!((made, done, skipped), (1, 1, 0));
        assert_eq!(t.find_tag("Taxes \"2026\"").unwrap().shape, Shape::Hex);
        assert_eq!(t.query(URGENT)[0].0, "C:\\a\\é.txt");
        assert!(t.import_json("{\"format\":\"x\"}", &mut |_| None).is_err());
        assert!(t.import_json("{", &mut |_| None).is_err());
    }

    #[test]
    fn norm_and_under() {
        assert_eq!(norm("C:\\A\\"), "c:\\a");
        assert_eq!(norm("C:\\"), "c:\\");
        assert_eq!(norm("C:"), "c:");
        assert!(under("c:\\a\\b", "c:\\a"));
        assert!(!under("c:\\ab", "c:\\a"));
        assert!(under("c:\\ab", "c:\\"));
    }
}
