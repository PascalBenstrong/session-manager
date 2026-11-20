//! A session's tabs, with change tracking: the list reads like a slice, but is only changed
//! through its methods, which note what changed. A save then serializes and writes just
//! those tabs (see `persist.rs`).
//!
//! - Changing existing tabs (`get_mut`, `at_mut`, writing through `iter_mut`) records their
//!   positions: a save visits only those, O(changed).
//! - Anything else — adding, removing, replacing the tabs, or a list made anew (read from
//!   storage.local, assigned) — marks the list *touched*: the save compares the session's
//!   tabs with what's saved, O(tabs of that session). Since a list that wasn't read from the
//!   database is always touched, even replacing the whole list can't lose a deletion.
//!
//! Each tab has a sequence number (its position, saved with it). Tabs are only ever
//! appended, removed, or all replaced, so the order of the sequence numbers is the order of
//! the list, and a change to one tab never rewrites the others' positions.
//!
//! Tabs are found by entry id or by browser tab id in O(1) (plus the tabs changed since the
//! last lookup): an index follows the same change tracking. An appended tab is indexed
//! right away, a changed tab again at the next lookup, and after tabs were removed or
//! replaced the index is rebuilt at the next lookup (O(tabs of the session)). Every hit is
//! checked against the tab itself, so a stale index can't return a wrong tab.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::{Id, TabEntry, TabId};

/// Where each tab is, by entry id and by browser tab id.
#[derive(Clone, Default)]
struct Index {
    /// Positions by key (one, unless ids repeat: then the first is the answer, as a scan's).
    by_id: HashMap<Id, Vec<usize>>,
    by_tab: HashMap<TabId, Vec<usize>>,
    /// The ids each position was indexed under.
    keys: Vec<(Id, Option<TabId>)>,
    /// Positions changed since they were indexed.
    pending: Vec<usize>,
    /// Rebuild before the next lookup (tabs were removed or replaced).
    stale: bool,
}

/// Adds a position under a key (kept sorted: the first is the answer).
fn put<K: std::hash::Hash + Eq>(map: &mut HashMap<K, Vec<usize>>, k: K, p: usize) {
    let v = map.entry(k).or_default();
    if let Err(i) = v.binary_search(&p) {
        v.insert(i, p);
    }
}

fn take<K: std::hash::Hash + Eq>(map: &mut HashMap<K, Vec<usize>>, k: K, p: usize) {
    if let Some(v) = map.get_mut(&k) {
        v.retain(|x| *x != p);
        if v.is_empty() {
            map.remove(&k);
        }
    }
}

impl Index {
    fn stale() -> Self {
        Index { stale: true, ..Default::default() }
    }

    fn add(&mut self, p: usize, e: &TabEntry) {
        if self.stale {
            return;
        }
        self.keys.push((e.id, e.tab_id));
        put(&mut self.by_id, e.id, p);
        if let Some(t) = e.tab_id {
            put(&mut self.by_tab, t, p);
        }
    }

    /// A tab may have changed its ids.
    fn touch(&mut self, p: usize, len: usize) {
        if self.stale {
            return;
        }
        self.pending.push(p);
        // Many changes without a lookup (e.g. a pass over every tab): rebuild instead.
        if self.pending.len() > len.max(16) {
            self.stale = true;
            self.pending.clear();
        }
    }

    fn sync(&mut self, list: &[TabEntry]) {
        if self.stale || self.keys.len() != list.len() {
            self.by_id.clear();
            self.by_tab.clear();
            self.keys.clear();
            self.pending.clear();
            self.stale = false;
            for (p, e) in list.iter().enumerate() {
                self.add(p, e);
            }
            return;
        }
        for p in std::mem::take(&mut self.pending) {
            let Some(e) = list.get(p) else { continue };
            let (old_id, old_tab) = self.keys[p];
            if (old_id, old_tab) == (e.id, e.tab_id) {
                continue;
            }
            take(&mut self.by_id, old_id, p);
            if let Some(t) = old_tab {
                take(&mut self.by_tab, t, p);
            }
            self.keys[p] = (e.id, e.tab_id);
            put(&mut self.by_id, e.id, p);
            if let Some(t) = e.tab_id {
                put(&mut self.by_tab, t, p);
            }
        }
    }
}

#[derive(Clone)]
pub struct Entries {
    list: Vec<TabEntry>,
    /// Sequence number of each tab (same positions as `list`), increasing.
    seq: Vec<u64>,
    /// The tab changed since the last save (same positions as `list`).
    changed: Vec<bool>,
    /// Positions newly marked in `changed` (only used while not `touched`).
    dirty: RefCell<Vec<usize>>,
    /// Tabs were added or removed, or the list is new: compare it all with what's saved.
    touched: Cell<bool>,
    next_seq: u64,
    /// Where tabs are by id and by browser tab (see the module docs).
    ix: RefCell<Index>,
}

impl Default for Entries {
    /// An empty list, not read from the database: touched.
    fn default() -> Self {
        Self {
            list: Vec::new(),
            seq: Vec::new(),
            changed: Vec::new(),
            dirty: RefCell::default(),
            touched: Cell::new(true),
            next_seq: 0,
            ix: RefCell::new(Index::stale()),
        }
    }
}

/// One tab, borrowed to change it. Reading it marks nothing; changing it (or calling a method
/// that takes `&mut TabEntry`) marks the tab as changed.
pub struct EntryMut<'a> {
    entry: &'a mut TabEntry,
    changed: &'a mut bool,
    index: usize,
    len: usize,
    dirty: &'a RefCell<Vec<usize>>,
    ix: &'a RefCell<Index>,
}

impl Deref for EntryMut<'_> {
    type Target = TabEntry;
    fn deref(&self) -> &TabEntry {
        self.entry
    }
}

impl DerefMut for EntryMut<'_> {
    fn deref_mut(&mut self) -> &mut TabEntry {
        if !*self.changed {
            *self.changed = true;
            self.dirty.borrow_mut().push(self.index);
        }
        self.ix.borrow_mut().touch(self.index, self.len);
        self.entry
    }
}

impl EntryMut<'_> {
    /// Applies `f` to a copy and keeps it only if it differs: the tab is marked changed only
    /// when it really changed (for code that sets fields that are usually already so).
    /// Returns whether it changed.
    pub fn update(&mut self, f: impl FnOnce(&mut TabEntry)) -> bool {
        let mut copy = self.entry.clone();
        f(&mut copy);
        if copy == *self.entry {
            return false;
        }
        **self = copy;
        true
    }
}

/// What changed in a session's tabs since the last save.
#[derive(Debug, PartialEq)]
pub enum Changes<'a> {
    /// Only these existing tabs changed (with their sequence numbers).
    Some(Vec<(u64, &'a TabEntry)>),
    /// Tabs were added or removed (or the list is new): every tab with its sequence number
    /// and whether it changed; what's saved but not listed was removed.
    All(Vec<(u64, &'a TabEntry, bool)>),
}

impl Entries {
    /// Tabs as saved: (sequence number, tab), in any order. Nothing is marked changed.
    pub fn from_saved(mut items: Vec<(u64, TabEntry)>) -> Self {
        items.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.id.cmp(&b.1.id)));
        let next_seq = items.last().map_or(0, |(s, _)| s + 1);
        let n = items.len();
        let (seq, list) = items.into_iter().unzip();
        Self {
            list,
            seq,
            changed: vec![false; n],
            dirty: RefCell::default(),
            touched: Cell::new(false),
            next_seq,
            ix: RefCell::new(Index::stale()),
        }
    }

    /// The position of the tab with this entry id.
    pub fn position(&self, id: Id) -> Option<usize> {
        let found = {
            let mut ix = self.ix.borrow_mut();
            ix.sync(&self.list);
            ix.by_id.get(&id).and_then(|v| v.first().copied())
        };
        match found {
            Some(p) if self.list.get(p).is_some_and(|e| e.id == id) => Some(p),
            None => None,
            // Can't happen; if it does, the answer is still right.
            Some(_) => {
                self.ix.borrow_mut().stale = true;
                self.list.iter().position(|e| e.id == id)
            }
        }
    }

    /// The position of the tab linked to this browser tab.
    pub fn position_of_tab(&self, tab: TabId) -> Option<usize> {
        let found = {
            let mut ix = self.ix.borrow_mut();
            ix.sync(&self.list);
            ix.by_tab.get(&tab).and_then(|v| v.first().copied())
        };
        match found {
            Some(p) if self.list.get(p).is_some_and(|e| e.tab_id == Some(tab)) => Some(p),
            None => None,
            Some(_) => {
                self.ix.borrow_mut().stale = true;
                self.list.iter().position(|e| e.tab_id == Some(tab))
            }
        }
    }

    /// The tab with this entry id.
    pub fn find(&self, id: Id) -> Option<&TabEntry> {
        self.position(id).map(|p| &self.list[p])
    }

    pub fn as_slice(&self) -> &[TabEntry] {
        &self.list
    }

    pub fn push(&mut self, e: TabEntry) {
        self.ix.get_mut().add(self.list.len(), &e);
        self.list.push(e);
        self.seq.push(self.next_seq);
        self.next_seq += 1;
        self.changed.push(true);
        self.touched.set(true);
    }

    /// Keeps the tabs for which `keep` is true; returns the ones removed.
    pub fn retain(&mut self, mut keep: impl FnMut(&TabEntry) -> bool) -> Vec<TabEntry> {
        let mut gone = Vec::new();
        let (list, seq, changed) = (std::mem::take(&mut self.list), std::mem::take(&mut self.seq), std::mem::take(&mut self.changed));
        for ((e, s), c) in list.into_iter().zip(seq).zip(changed) {
            if keep(&e) {
                self.list.push(e);
                self.seq.push(s);
                self.changed.push(c);
            } else {
                gone.push(e);
            }
        }
        if !gone.is_empty() {
            self.touched.set(true);
            self.ix.get_mut().stale = true;
        }
        gone
    }

    /// Removes the tab at `index`.
    pub fn remove(&mut self, index: usize) -> TabEntry {
        self.seq.remove(index);
        self.changed.remove(index);
        self.touched.set(true);
        self.ix.get_mut().stale = true;
        self.list.remove(index)
    }

    /// Replaces every tab (they're all saved again).
    pub fn replace_all(&mut self, entries: Vec<TabEntry>) {
        *self.ix.get_mut() = Index::default();
        self.list.clear();
        self.seq.clear();
        self.changed.clear();
        for e in entries {
            self.push(e);
        }
        self.touched.set(true);
    }

    /// Takes every tab out.
    pub fn take_all(&mut self) -> Vec<TabEntry> {
        self.seq.clear();
        self.changed.clear();
        self.touched.set(true);
        *self.ix.get_mut() = Index::default();
        std::mem::take(&mut self.list)
    }

    /// The tab with this id, to change it (marked changed).
    pub fn get_mut(&mut self, id: Id) -> Option<&mut TabEntry> {
        let i = self.position(id)?;
        Some(self.at_mut(i))
    }

    /// The tab at `index`, to change it (marked changed).
    pub fn at_mut(&mut self, index: usize) -> &mut TabEntry {
        if !self.changed[index] {
            self.changed[index] = true;
            self.dirty.get_mut().push(index);
        }
        let len = self.list.len();
        self.ix.get_mut().touch(index, len);
        &mut self.list[index]
    }

    /// Every tab, to change some: only the ones actually changed are marked.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = EntryMut<'_>> {
        let (dirty, ix, len) = (&self.dirty, &self.ix, self.list.len());
        self.list.iter_mut().zip(self.changed.iter_mut()).enumerate().map(move |(index, (entry, changed))| EntryMut {
            entry,
            changed,
            index,
            len,
            dirty,
            ix,
        })
    }

    /// Marks every tab changed (to save them all again).
    pub fn mark_all_changed(&mut self) {
        self.changed.iter_mut().for_each(|c| *c = true);
        self.touched.set(true);
    }

    /// Whether anything changed since the last save. O(1).
    pub fn has_changes(&self) -> bool {
        self.touched.get() || !self.dirty.borrow().is_empty()
    }

    /// What changed since the last save: O(changed tabs), or O(tabs) after tabs were added
    /// or removed. Nothing is serialized.
    pub fn changes(&self) -> Changes<'_> {
        if self.touched.get() {
            let all = self.list.iter().zip(&self.seq).zip(&self.changed).map(|((e, s), c)| (*s, e, *c)).collect();
            return Changes::All(all);
        }
        let mut positions = self.dirty.borrow().clone();
        positions.sort_unstable();
        positions.dedup();
        Changes::Some(positions.into_iter().filter(|i| *i < self.list.len()).map(|i| (self.seq[i], &self.list[i])).collect())
    }

    /// Every tab with its sequence number.
    pub fn with_seq(&self) -> impl Iterator<Item = (u64, &TabEntry)> {
        self.seq.iter().copied().zip(&self.list)
    }

    /// Forgets the changes (they were saved). O(changed), or O(tabs) after a touch.
    pub fn clear_changes(&mut self) {
        if self.touched.get() {
            self.changed.iter_mut().for_each(|c| *c = false);
        } else {
            for i in self.dirty.get_mut().iter() {
                if let Some(c) = self.changed.get_mut(*i) {
                    *c = false;
                }
            }
        }
        self.dirty.get_mut().clear();
        self.touched.set(false);
    }

    pub fn into_vec(self) -> Vec<TabEntry> {
        self.list
    }
}

impl Deref for Entries {
    type Target = [TabEntry];
    fn deref(&self) -> &[TabEntry] {
        &self.list
    }
}

impl<'a> IntoIterator for &'a Entries {
    type Item = &'a TabEntry;
    type IntoIter = std::slice::Iter<'a, TabEntry>;
    fn into_iter(self) -> Self::IntoIter {
        self.list.iter()
    }
}

impl From<Vec<TabEntry>> for Entries {
    /// New tabs (all to be saved).
    fn from(list: Vec<TabEntry>) -> Self {
        let mut e = Entries::default();
        for x in list {
            e.push(x);
        }
        e
    }
}

impl FromIterator<TabEntry> for Entries {
    fn from_iter<I: IntoIterator<Item = TabEntry>>(iter: I) -> Self {
        Entries::from(iter.into_iter().collect::<Vec<_>>())
    }
}

impl Extend<TabEntry> for Entries {
    fn extend<I: IntoIterator<Item = TabEntry>>(&mut self, iter: I) {
        for e in iter {
            self.push(e);
        }
    }
}

/// Equal when the tabs are (what's changed isn't part of the value).
impl PartialEq for Entries {
    fn eq(&self, other: &Self) -> bool {
        self.list == other.list
    }
}

impl std::fmt::Debug for Entries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.list.fmt(f)
    }
}

impl Serialize for Entries {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.list.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<TabEntry>::deserialize(d).map(Entries::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: Id) -> TabEntry {
        TabEntry { id, url: format!("https://{id}.com"), ..Default::default() }
    }

    fn some(c: Changes) -> Vec<Id> {
        match c {
            Changes::Some(v) => v.iter().map(|(_, e)| e.id).collect(),
            Changes::All(_) => panic!("expected only changed tabs"),
        }
    }

    fn all(c: Changes) -> Vec<(Id, bool)> {
        match c {
            Changes::All(v) => v.iter().map(|(_, e, c)| (e.id, *c)).collect(),
            Changes::Some(_) => panic!("expected every tab"),
        }
    }

    #[test]
    fn changing_tabs_visits_only_them() {
        let mut list = Entries::from_saved(vec![(5, e(2)), (1, e(1)), (9, e(3))]);
        assert_eq!(list.iter().map(|x| x.id).collect::<Vec<_>>(), [1, 2, 3], "in sequence order");
        assert!(!list.has_changes());
        // Reading through iter_mut marks nothing; writing marks only that tab.
        for mut x in list.iter_mut() {
            if x.id == 2 {
                x.title = "changed".into();
                x.title.push('!'); // twice: still one change
            }
        }
        list.get_mut(3).unwrap().pinned = true;
        assert!(list.has_changes());
        assert_eq!(some(list.changes()), [2, 3]);
        list.clear_changes();
        assert!(!list.has_changes());
        assert_eq!(some(list.changes()), Vec::<Id>::new());
    }

    #[test]
    fn adding_or_removing_compares_everything() {
        let mut list = Entries::from_saved(vec![(1, e(1)), (2, e(2))]);
        list.get_mut(1).unwrap().pinned = true;
        list.push(e(4));
        assert_eq!(all(list.changes()), [(1, true), (2, false), (4, true)]);
        list.clear_changes();
        list.retain(|x| x.id != 1);
        assert_eq!(all(list.changes()), [(2, false), (4, false)]);
        list.clear_changes();
        assert_eq!(list.with_seq().map(|(s, x)| (s, x.id)).collect::<Vec<_>>(), [(2, 2), (3, 4)], "appended after the last");
        list.take_all();
        assert!(list.has_changes());
        list.replace_all(vec![e(7)]);
        assert_eq!(all(list.changes()), [(7, true)]);
        // A list not read from the database is always compared in full.
        assert!(Entries::default().has_changes());
        assert!(Entries::from(vec![e(1)]).has_changes());
    }

    /// The index answers like a scan, whatever happens to the list.
    #[test]
    fn lookups_match_a_scan() {
        let mut list =
            Entries::from_saved((1..=20).map(|i| (i, TabEntry { tab_id: (i % 3 != 0).then_some(i as i64 * 10), ..e(i) })).collect());
        let mut rng = 12345u64;
        let mut next = |n: u64| {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) % n
        };
        let mut new_id = 100;
        for round in 0..3000 {
            match next(9) {
                0 => {
                    new_id += 1;
                    list.push(TabEntry { tab_id: Some(new_id as i64 * 10), ..e(new_id) });
                }
                1 if !list.is_empty() => {
                    let i = next(list.len() as u64) as usize;
                    list.remove(i);
                }
                2 if !list.is_empty() => {
                    let i = next(list.len() as u64) as usize;
                    list.at_mut(i).tab_id = Some(next(400) as i64);
                }
                3 => {
                    let t = next(400) as i64;
                    for mut x in list.iter_mut() {
                        if x.id % 5 == round % 5 {
                            x.tab_id = if x.tab_id.is_some() { None } else { Some(t) };
                        }
                    }
                }
                4 => {
                    let cut = next(150);
                    list.retain(|x| x.id != cut);
                }
                5 if round % 200 == 0 => {
                    let keep: Vec<TabEntry> = list.iter().take(10).cloned().collect();
                    list.replace_all(keep);
                }
                6 => list.clear_changes(),
                _ => {}
            }
            for probe in 0..160u64 {
                assert_eq!(list.position(probe), list.iter().position(|x| x.id == probe), "id {probe} in round {round}");
                let t = probe as i64 * 5 % 400;
                // Also with repeated tab ids (not in real states): the first, like a scan.
                assert_eq!(list.position_of_tab(t), list.iter().position(|x| x.tab_id == Some(t)), "tab {t} in round {round}");
            }
        }
    }

    #[test]
    fn serializes_as_a_plain_list() {
        let list = Entries::from(vec![e(1), e(2)]);
        let json = serde_json::to_string(&list).unwrap();
        assert_eq!(json, serde_json::to_string(&vec![e(1), e(2)]).unwrap());
        let back: Entries = serde_json::from_str(&json).unwrap();
        assert_eq!(back, list);
        assert!(back.has_changes(), "read from a plain list: all to be saved");
    }
}
