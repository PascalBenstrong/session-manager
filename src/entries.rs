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

use std::cell::{Cell, RefCell};
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::{Id, TabEntry};

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
}

impl Default for Entries {
    /// An empty list, not read from the database: touched.
    fn default() -> Self {
        Self { list: Vec::new(), seq: Vec::new(), changed: Vec::new(), dirty: RefCell::default(), touched: Cell::new(true), next_seq: 0 }
    }
}

/// One tab, borrowed to change it. Reading it marks nothing; changing it (or calling a method
/// that takes `&mut TabEntry`) marks the tab as changed.
pub struct EntryMut<'a> {
    entry: &'a mut TabEntry,
    changed: &'a mut bool,
    index: usize,
    dirty: &'a RefCell<Vec<usize>>,
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
        Self { list, seq, changed: vec![false; n], dirty: RefCell::default(), touched: Cell::new(false), next_seq }
    }

    pub fn as_slice(&self) -> &[TabEntry] {
        &self.list
    }

    pub fn push(&mut self, e: TabEntry) {
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
        }
        gone
    }

    /// Removes the tab at `index`.
    pub fn remove(&mut self, index: usize) -> TabEntry {
        self.seq.remove(index);
        self.changed.remove(index);
        self.touched.set(true);
        self.list.remove(index)
    }

    /// Replaces every tab (they're all saved again).
    pub fn replace_all(&mut self, entries: Vec<TabEntry>) {
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
        std::mem::take(&mut self.list)
    }

    /// The tab with this id, to change it (marked changed).
    pub fn get_mut(&mut self, id: Id) -> Option<&mut TabEntry> {
        let i = self.list.iter().position(|e| e.id == id)?;
        Some(self.at_mut(i))
    }

    /// The tab at `index`, to change it (marked changed).
    pub fn at_mut(&mut self, index: usize) -> &mut TabEntry {
        if !self.changed[index] {
            self.changed[index] = true;
            self.dirty.get_mut().push(index);
        }
        &mut self.list[index]
    }

    /// Every tab, to change some: only the ones actually changed are marked.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = EntryMut<'_>> {
        let dirty = &self.dirty;
        self.list.iter_mut().zip(self.changed.iter_mut()).enumerate().map(move |(index, (entry, changed))| EntryMut {
            entry,
            changed,
            index,
            dirty,
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
