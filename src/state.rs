//! Pure state transitions on [`AppState`]: tab tracking, reconciliation and edits.
//! The background script drives these from browser events; they never touch the browser.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{AppState, BrowserTab, EntryRef, Id, Session, Snapshot, TabEntry, TabId};
use crate::url;

const MAX_CLEANUP_LOG: usize = 20;

/// How a snapshot is brought back.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreMode {
    /// The session's tab list becomes the snapshot (the current list is snapshotted first).
    Replace,
    /// Add the snapshot's tabs that are not in the session yet.
    Merge,
    /// Create a new session from the snapshot.
    NewSession,
}

/// Where imported URLs go.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ImportTarget {
    Session(Id),
    NewSession(String),
}

/// Whether a URL should be tracked in sessions.
pub fn trackable(u: &str, ignore_internal: bool) -> bool {
    if u.is_empty() || u == "about:blank" {
        return false;
    }
    !ignore_internal || matches!(url::scheme(u).as_str(), "http" | "https" | "file" | "ftp")
}

impl AppState {
    /// Fix up invariants after loading from storage.
    pub fn normalize(&mut self) {
        if self.sessions.is_empty() {
            *self = AppState { settings: self.settings.clone(), ..AppState::default() };
        }
        if self.session(self.active_session).is_none() {
            self.active_session = self.sessions[0].id;
        }
        let max_id = self
            .sessions
            .iter()
            .flat_map(|s| std::iter::once(s.id).chain(s.entries.iter().map(|e| e.id)))
            .chain(self.sessions.iter().flat_map(|s| s.snapshots.iter().map(|x| x.id)))
            .chain(self.auto_rules.iter().map(|r| r.id))
            .chain(self.cleanup_rules.iter().map(|r| r.id))
            .max()
            .unwrap_or(0);
        self.next_id = self.next_id.max(max_id + 1);
    }

    pub fn locate_tab(&self, tab_id: TabId) -> Option<EntryRef> {
        self.sessions.iter().find_map(|s| {
            s.entries.iter().find(|e| e.tab_id == Some(tab_id)).map(|e| EntryRef { session: s.id, entry: e.id })
        })
    }

    pub fn entry(&self, r: EntryRef) -> Option<&TabEntry> {
        self.session(r.session)?.entry(r.entry)
    }

    pub fn entry_mut(&mut self, r: EntryRef) -> Option<&mut TabEntry> {
        self.session_mut(r.session)?.entry_mut(r.entry)
    }

    pub fn entry_for_tab(&self, tab_id: TabId) -> Option<&TabEntry> {
        self.locate_tab(tab_id).and_then(|r| self.entry(r))
    }

    /// Applies enabled auto rules (tags + group if none) to an entry.
    fn apply_auto_rules(&self, e: &mut TabEntry) {
        let host = e.host();
        for rule in self.auto_rules.iter().filter(|r| r.enabled) {
            let Ok(p) = rule.pattern.compile() else { continue };
            if p.matches(&host, &e.url) {
                e.tags.extend(rule.tags.iter().cloned());
                if e.group.is_none() {
                    e.group = rule.group.clone();
                }
            }
        }
    }

    /// Applies auto rules to every entry (after rules change). Returns true if anything changed.
    pub fn apply_auto_rules_all(&mut self) -> bool {
        let mut sessions = std::mem::take(&mut self.sessions);
        let mut changed = false;
        for s in &mut sessions {
            for e in &mut s.entries {
                let before = (e.tags.clone(), e.group.clone());
                self.apply_auto_rules(e);
                changed |= before != (e.tags.clone(), e.group.clone());
            }
        }
        self.sessions = sessions;
        changed
    }

    /// A browser tab was created or updated. Returns true if state changed.
    pub fn on_tab_upsert(&mut self, tab: &BrowserTab, now: f64) -> bool {
        if self.ignored_tab_ids.contains(&tab.id) {
            return false;
        }
        if let Some(r) = self.locate_tab(tab.id) {
            let mut e = self.entry(r).cloned().expect("located");
            let before = e.clone();
            let url_changed = !tab.url.is_empty() && e.url != tab.url;
            if !tab.url.is_empty() {
                e.url = tab.url.clone();
            }
            if !tab.title.is_empty() {
                e.title = tab.title.clone();
            }
            if tab.fav_icon_url.is_some() {
                e.fav_icon_url = tab.fav_icon_url.clone();
            }
            e.pinned = tab.pinned;
            e.window_id = Some(tab.window_id);
            if tab.active {
                e.last_accessed = e.last_accessed.max(now);
            }
            if url_changed {
                self.apply_auto_rules(&mut e);
            }
            let changed = e != before;
            *self.entry_mut(r).expect("located") = e;
            return changed;
        }
        if !self.settings.auto_add_tabs || !trackable(&tab.url, self.settings.ignore_internal_urls) {
            return false;
        }
        let id = self.alloc_id();
        let mut e = entry_from_tab(id, tab, now);
        self.apply_auto_rules(&mut e);
        let active = self.active_session;
        match self.session_mut(active) {
            Some(s) => s.entries.push(e),
            None => return false,
        }
        true
    }

    /// A browser tab was closed.
    pub fn on_tab_removed(&mut self, tab_id: TabId, window_closing: bool) -> bool {
        self.ignored_tab_ids.remove(&tab_id);
        let Some(r) = self.locate_tab(tab_id) else { return false };
        if window_closing && self.settings.keep_tabs_on_window_close {
            let e = self.entry_mut(r).expect("located");
            e.tab_id = None;
            e.window_id = None;
        } else if let Some(s) = self.session_mut(r.session) {
            s.entries.retain(|e| e.id != r.entry);
        }
        true
    }

    pub fn on_tab_activated(&mut self, tab_id: TabId, now: f64) -> bool {
        match self.locate_tab(tab_id) {
            Some(r) => {
                self.entry_mut(r).expect("located").last_accessed = now;
                true
            }
            None => false,
        }
    }

    /// The browser replaced a tab (e.g. after discarding in Chromium).
    pub fn on_tab_replaced(&mut self, added: TabId, removed: TabId) -> bool {
        if self.ignored_tab_ids.remove(&removed) {
            self.ignored_tab_ids.insert(added);
        }
        match self.locate_tab(removed) {
            Some(r) => {
                self.entry_mut(r).expect("located").tab_id = Some(added);
                true
            }
            None => false,
        }
    }

    /// Links a tab we opened for an entry.
    pub fn link_tab(&mut self, r: EntryRef, tab: &BrowserTab) {
        // If the creation event already auto-added this tab as a new entry, drop that duplicate.
        if let Some(dup) = self.locate_tab(tab.id)
            && dup != r
            && let Some(s) = self.session_mut(dup.session)
        {
            s.entries.retain(|e| e.id != dup.entry);
        }
        if let Some(e) = self.entry_mut(r) {
            e.tab_id = Some(tab.id);
            e.window_id = Some(tab.window_id);
        }
    }

    /// Re-links sessions to the currently open tabs (startup / service worker wake-up).
    /// Tabs are matched by id first (extension reload), then by URL (browser restart).
    pub fn reconcile(&mut self, tabs: &[BrowserTab], now: f64) {
        let by_id: std::collections::HashMap<TabId, &BrowserTab> = tabs.iter().map(|t| (t.id, t)).collect();
        let mut linked: HashSet<TabId> = HashSet::new();
        for s in &mut self.sessions {
            for e in &mut s.entries {
                if let Some(id) = e.tab_id {
                    match by_id.get(&id) {
                        Some(t) if t.url == e.url && !linked.contains(&id) => {
                            linked.insert(id);
                        }
                        _ => {
                            e.tab_id = None;
                            e.window_id = None;
                        }
                    }
                }
            }
        }
        let open: BTreeSet<TabId> = tabs.iter().map(|t| t.id).collect();
        self.ignored_tab_ids.retain(|id| open.contains(id));

        for t in tabs {
            if linked.contains(&t.id) || self.ignored_tab_ids.contains(&t.id) {
                continue;
            }
            if !self.link_by_url(t) {
                self.on_tab_upsert(t, now);
            }
        }
    }

    /// Links a not yet tracked tab to an unlinked entry with the same URL, preferring the
    /// active session (a tab the browser restored after a restart). Returns true if linked.
    pub fn link_by_url(&mut self, t: &BrowserTab) -> bool {
        if t.url.is_empty() || self.locate_tab(t.id).is_some() || self.ignored_tab_ids.contains(&t.id) {
            return false;
        }
        let active = self.active_session;
        let order = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.id == active)
            .chain(self.sessions.iter().enumerate().filter(|(_, s)| s.id != active))
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let found = order.into_iter().find_map(|si| {
            self.sessions[si].entries.iter().position(|e| e.tab_id.is_none() && e.url == t.url).map(|ei| (si, ei))
        });
        let Some((si, ei)) = found else { return false };
        let e = &mut self.sessions[si].entries[ei];
        e.tab_id = Some(t.id);
        e.window_id = Some(t.window_id);
        true
    }

    // ---- user edits ----

    pub fn create_session(&mut self, name: &str, now: f64) -> Id {
        let id = self.alloc_id();
        let name = if name.trim().is_empty() { format!("Session {id}") } else { name.trim().to_string() };
        self.sessions.push(Session { id, name, created_at: now, ..Default::default() });
        id
    }

    pub fn rename_session(&mut self, id: Id, name: &str) -> Result<(), String> {
        let s = self.session_mut(id).ok_or("no such session")?;
        if name.trim().is_empty() {
            return Err("name cannot be empty".into());
        }
        s.name = name.trim().to_string();
        Ok(())
    }

    /// Deletes a session; returns the open tab ids it contained.
    pub fn delete_session(&mut self, id: Id) -> Result<Vec<TabId>, String> {
        if self.sessions.len() <= 1 {
            return Err("cannot delete the last session".into());
        }
        let idx = self.sessions.iter().position(|s| s.id == id).ok_or("no such session")?;
        let s = self.sessions.remove(idx);
        if self.active_session == id {
            self.active_session = self.sessions[0].id;
        }
        let open: Vec<TabId> = s.entries.iter().filter_map(|e| e.tab_id).collect();
        // Keep those tabs out of the newly active session unless the user re-adds them.
        self.ignored_tab_ids.extend(open.iter().copied());
        Ok(open)
    }

    /// Makes a session active. Its tabs are NOT opened; new tabs will be added to it.
    pub fn set_active(&mut self, id: Id) -> Result<(), String> {
        self.session(id).ok_or("no such session")?;
        self.active_session = id;
        Ok(())
    }

    /// Removes entries; returns the open tab ids of removed entries. Unless those tabs get
    /// closed, they are ignored so they don't get re-added automatically.
    pub fn remove_entries(&mut self, refs: &[EntryRef], ignore_open: bool) -> Vec<TabId> {
        let mut open = Vec::new();
        for r in refs {
            if let Some(s) = self.session_mut(r.session)
                && let Some(i) = s.entries.iter().position(|e| e.id == r.entry)
            {
                let e = s.entries.remove(i);
                open.extend(e.tab_id);
            }
        }
        if ignore_open {
            self.ignored_tab_ids.extend(open.iter().copied());
        }
        open
    }

    pub fn tag_entries(&mut self, refs: &[EntryRef], tag: &str, add: bool) -> usize {
        let tag = tag.trim();
        if tag.is_empty() {
            return 0;
        }
        let mut n = 0;
        for r in refs {
            if let Some(e) = self.entry_mut(*r) {
                let changed = if add { e.tags.insert(tag.to_string()) } else { e.tags.remove(tag) };
                n += changed as usize;
            }
        }
        n
    }

    pub fn set_group(&mut self, refs: &[EntryRef], group: Option<&str>) -> usize {
        let group = group.map(str::trim).filter(|g| !g.is_empty()).map(String::from);
        let mut n = 0;
        for r in refs {
            if let Some(e) = self.entry_mut(*r) {
                n += (e.group != group) as usize;
                e.group = group.clone();
            }
        }
        n
    }

    pub fn move_entries(&mut self, refs: &[EntryRef], target: Id) -> Result<usize, String> {
        self.session(target).ok_or("no such session")?;
        let mut moved = Vec::new();
        for r in refs.iter().filter(|r| r.session != target) {
            if let Some(s) = self.session_mut(r.session)
                && let Some(i) = s.entries.iter().position(|e| e.id == r.entry)
            {
                moved.push(s.entries.remove(i));
            }
        }
        let n = moved.len();
        self.session_mut(target).expect("checked").entries.extend(moved);
        Ok(n)
    }

    // ---- snapshots ----

    /// Copy of an entry with a fresh id and no live tab.
    fn fresh_entry(&mut self, e: &TabEntry) -> TabEntry {
        TabEntry { id: self.alloc_id(), tab_id: None, window_id: None, ..e.clone() }
    }

    pub fn create_snapshot(&mut self, session: Id, name: &str, auto: bool, now: f64) -> Result<Id, String> {
        let id = self.alloc_id();
        let max_auto = self.settings.max_auto_snapshots.max(1) as usize;
        let s = self.session_mut(session).ok_or("no such session")?;
        let entries = s.entries.iter().map(|e| TabEntry { tab_id: None, window_id: None, ..e.clone() }).collect();
        let name = match name.trim() {
            "" => format!("Snapshot {}", s.snapshots.len() + 1),
            n => n.to_string(),
        };
        s.snapshots.push(Snapshot { id, name, created_at: now, auto, entries });
        // Prune the oldest automatic snapshots.
        let autos = s.snapshots.iter().filter(|x| x.auto).count();
        if autos > max_auto {
            let mut excess = autos - max_auto;
            s.snapshots.retain(|x| {
                if x.auto && excess > 0 {
                    excess -= 1;
                    false
                } else {
                    true
                }
            });
        }
        Ok(id)
    }

    pub fn delete_snapshot(&mut self, session: Id, snapshot: Id) -> Result<(), String> {
        let s = self.session_mut(session).ok_or("no such session")?;
        let before = s.snapshots.len();
        s.snapshots.retain(|x| x.id != snapshot);
        if s.snapshots.len() == before {
            return Err("no such snapshot".into());
        }
        Ok(())
    }

    /// Restores a snapshot. Returns the session that received the tabs and the ids of
    /// entries that were added and are not open yet.
    pub fn restore_snapshot(&mut self, session: Id, snapshot: Id, mode: RestoreMode, now: f64) -> Result<(Id, Vec<Id>), String> {
        let current = self.session(session).ok_or("no such session")?;
        let snap = current.snapshot(snapshot).ok_or("no such snapshot")?.clone();
        let session_name = current.name.clone();
        match mode {
            RestoreMode::NewSession => {
                let target = self.create_session(&format!("{session_name} ({})", snap.name), now);
                let entries: Vec<TabEntry> = snap.entries.iter().map(|e| self.fresh_entry(e)).collect();
                let added = entries.iter().map(|e| e.id).collect();
                self.session_mut(target).expect("created").entries = entries;
                Ok((target, added))
            }
            RestoreMode::Merge => {
                let existing: HashSet<String> = self.session(session).expect("checked").urls().into_iter().map(String::from).collect();
                let entries: Vec<TabEntry> =
                    snap.entries.iter().filter(|e| !existing.contains(&e.url)).map(|e| self.fresh_entry(e)).collect();
                let added = entries.iter().map(|e| e.id).collect();
                self.session_mut(session).expect("checked").entries.extend(entries);
                Ok((session, added))
            }
            RestoreMode::Replace => {
                self.create_snapshot(session, &format!("Before restoring \"{}\"", snap.name), true, now)?;
                let current = std::mem::take(&mut self.session_mut(session).expect("checked").entries);
                // Live tabs whose URL is in the snapshot stay linked to the restored entries.
                let mut open: Vec<TabEntry> = current.into_iter().filter(|e| e.is_open()).collect();
                let mut restored = Vec::new();
                let mut added = Vec::new();
                for e in &snap.entries {
                    let mut fresh = self.fresh_entry(e);
                    if let Some(i) = open.iter().position(|o| o.url == e.url) {
                        let live = open.remove(i);
                        fresh.tab_id = live.tab_id;
                        fresh.window_id = live.window_id;
                    } else {
                        added.push(fresh.id);
                    }
                    restored.push(fresh);
                }
                // Tabs that are open but not part of the snapshot stay open, untracked.
                self.ignored_tab_ids.extend(open.iter().filter_map(|e| e.tab_id));
                self.session_mut(session).expect("checked").entries = restored;
                Ok((session, added))
            }
        }
    }

    /// Snapshots every non-empty session whose URLs changed since its latest snapshot.
    pub fn auto_snapshot_all(&mut self, now: f64) -> usize {
        let due: Vec<Id> = self
            .sessions
            .iter()
            .filter(|s| !s.entries.is_empty())
            .filter(|s| s.snapshots.last().is_none_or(|last| last.urls() != s.urls()))
            .map(|s| s.id)
            .collect();
        for id in &due {
            let _ = self.create_snapshot(*id, "Automatic", true, now);
        }
        due.len()
    }

    // ---- import ----

    /// Adds URLs (unloaded) to a session; returns the session and the new entry ids.
    /// URLs already in the session are skipped.
    pub fn import_urls(&mut self, urls: &[String], target: &ImportTarget, now: f64) -> Result<(Id, Vec<Id>), String> {
        let session = match target {
            ImportTarget::Session(id) => {
                self.session(*id).ok_or("no such session")?;
                *id
            }
            ImportTarget::NewSession(name) => self.create_session(name, now),
        };
        let existing: HashSet<String> = self.session(session).expect("exists").urls().into_iter().map(String::from).collect();
        let mut entries = Vec::new();
        for u in urls.iter().filter(|u| !existing.contains(*u)) {
            let mut e = TabEntry { id: self.alloc_id(), url: u.clone(), created_at: now, ..Default::default() };
            self.apply_auto_rules(&mut e);
            entries.push(e);
        }
        let added = entries.iter().map(|e| e.id).collect();
        self.session_mut(session).expect("exists").entries.extend(entries);
        Ok((session, added))
    }

    /// Adds imported sessions as new sessions (fresh ids, unique names). Returns their ids.
    pub fn import_sessions(&mut self, sessions: Vec<Session>, now: f64) -> Vec<Id> {
        let mut ids = Vec::new();
        for mut s in sessions {
            let mut name = if s.name.trim().is_empty() { "Imported".to_string() } else { s.name.trim().to_string() };
            if self.sessions.iter().any(|x| x.name == name) {
                name = format!("{name} (imported)");
            }
            let id = self.create_session(&name, now);
            let entries: Vec<TabEntry> = s.entries.iter().map(|e| self.fresh_entry(e)).collect();
            for snap in &mut s.snapshots {
                snap.id = self.alloc_id();
                for e in &mut snap.entries {
                    e.tab_id = None;
                    e.window_id = None;
                }
            }
            let target = self.session_mut(id).expect("created");
            target.entries = entries;
            target.snapshots = s.snapshots;
            ids.push(id);
        }
        ids
    }

    pub fn push_cleanup_report(&mut self, report: crate::cleanup::CleanupReport) {
        self.cleanup_log.insert(0, report);
        self.cleanup_log.truncate(MAX_CLEANUP_LOG);
    }

    /// Hosts of all session entries (cleanup candidates).
    pub fn known_hosts(&self) -> Vec<String> {
        let set: BTreeSet<String> =
            self.sessions.iter().flat_map(|s| s.entries.iter().map(|e| url::host(&e.url))).collect();
        set.into_iter().collect()
    }
}

pub fn entry_from_tab(id: Id, tab: &BrowserTab, now: f64) -> TabEntry {
    TabEntry {
        id,
        url: tab.url.clone(),
        title: tab.title.clone(),
        fav_icon_url: tab.fav_icon_url.clone(),
        tags: BTreeSet::new(),
        group: None,
        pinned: tab.pinned,
        tab_id: Some(tab.id),
        window_id: Some(tab.window_id),
        created_at: now,
        last_accessed: if tab.active { now } else { 0.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AutoRule;
    use crate::pattern::{PatternKind, SitePattern};

    fn tab(id: TabId, url: &str) -> BrowserTab {
        BrowserTab { id, window_id: 1, url: url.into(), title: format!("T{id}"), ..Default::default() }
    }

    #[test]
    fn auto_add_update_remove() {
        let mut s = AppState::default();
        assert!(s.on_tab_upsert(&tab(1, "https://a.com"), 1.0));
        assert!(!s.on_tab_upsert(&tab(2, "about:newtab"), 1.0));
        assert_eq!(s.active().entries.len(), 1);
        // navigation updates the same entry
        assert!(s.on_tab_upsert(&tab(1, "https://b.com"), 2.0));
        assert_eq!(s.active().entries[0].url, "https://b.com");
        // internal tab navigates to a real page -> added
        assert!(s.on_tab_upsert(&tab(2, "https://c.com"), 3.0));
        assert_eq!(s.active().entries.len(), 2);
        // closing removes it
        assert!(s.on_tab_removed(1, false));
        assert_eq!(s.active().entries.len(), 1);
        // window closing keeps it unloaded
        assert!(s.on_tab_removed(2, true));
        assert_eq!(s.active().entries.len(), 1);
        assert!(!s.active().entries[0].is_open());
    }

    #[test]
    fn manual_remove_is_sticky() {
        let mut s = AppState::default();
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        let r = s.locate_tab(1).unwrap();
        assert_eq!(s.remove_entries(&[r], true), vec![1]);
        assert!(!s.on_tab_upsert(&tab(1, "https://a.com/2"), 2.0));
        assert!(s.active().entries.is_empty());
    }

    #[test]
    fn active_session_switch_does_not_move_tabs() {
        let mut s = AppState::default();
        let first = s.active_session;
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        let second = s.create_session("Work", 1.0);
        s.set_active(second).unwrap();
        s.on_tab_upsert(&tab(2, "https://b.com"), 2.0);
        assert_eq!(s.session(first).unwrap().entries.len(), 1);
        assert_eq!(s.session(second).unwrap().entries.len(), 1);
        // closing a tab of an inactive session still removes it there
        s.on_tab_removed(1, false);
        assert!(s.session(first).unwrap().entries.is_empty());
    }

    #[test]
    fn reconcile_matches_by_id_then_url() {
        let mut s = AppState::default();
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        s.on_tab_upsert(&tab(2, "https://b.com"), 1.0);
        s.on_tab_removed(1, true);
        s.on_tab_removed(2, true);
        // browser restart: new ids
        s.reconcile(&[tab(10, "https://b.com"), tab(11, "https://new.com")], 5.0);
        let e = &s.active().entries;
        assert_eq!(e.len(), 3);
        assert_eq!(e.iter().find(|e| e.url == "https://b.com").unwrap().tab_id, Some(10));
        assert_eq!(e.iter().find(|e| e.url == "https://a.com").unwrap().tab_id, None);
        assert_eq!(e.iter().find(|e| e.url == "https://new.com").unwrap().tab_id, Some(11));
    }

    #[test]
    fn link_by_url_finds_saved_entry_in_any_session() {
        let mut s = AppState::default();
        let other = s.create_session("Other", 0.0);
        s.set_active(other).unwrap();
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        s.on_tab_removed(1, true);
        s.set_active(s.sessions[0].id).unwrap();
        // tab restored after a restart lands in its saved entry, not as a new one
        assert!(s.link_by_url(&tab(20, "https://a.com")));
        assert_eq!(s.session(other).unwrap().entries[0].tab_id, Some(20));
        assert!(s.active().entries.is_empty());
        // already linked / no saved entry
        assert!(!s.link_by_url(&tab(20, "https://a.com")));
        assert!(!s.link_by_url(&tab(21, "https://a.com")));
    }

    #[test]
    fn last_accessed_and_auto_rules() {
        let mut s = AppState::default();
        s.auto_rules.push(AutoRule {
            id: 99,
            enabled: true,
            pattern: SitePattern::new(PatternKind::Domain, "github.com"),
            tags: ["code".to_string()].into(),
            group: Some("Dev".into()),
        });
        s.on_tab_upsert(&tab(1, "https://github.com/x"), 1.0);
        s.on_tab_upsert(&tab(2, "https://b.com"), 1.0);
        s.on_tab_activated(1, 7.0);
        s.on_tab_activated(2, 5.0);
        let last = s.active().last_accessed().unwrap();
        assert_eq!(last.tab_id, Some(1));
        assert_eq!(last.group.as_deref(), Some("Dev"));
        assert!(last.tags.contains("code"));
    }

    #[test]
    fn link_tab_dedups_auto_added() {
        let mut s = AppState::default();
        let other = s.create_session("Other", 0.0);
        s.session_mut(other).unwrap().entries.push(TabEntry { id: 500, url: "https://a.com".into(), ..Default::default() });
        // creation event raced ahead and auto-added the tab to the active session
        s.on_tab_upsert(&tab(7, "https://a.com"), 1.0);
        s.link_tab(EntryRef { session: other, entry: 500 }, &tab(7, "https://a.com"));
        assert!(s.active().entries.is_empty());
        assert_eq!(s.session(other).unwrap().entries[0].tab_id, Some(7));
    }

    #[test]
    fn snapshots_replace_merge_new() {
        let mut s = AppState::default();
        let sid = s.active_session;
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        s.on_tab_upsert(&tab(2, "https://b.com"), 1.0);
        let snap = s.create_snapshot(sid, "v1", false, 2.0).unwrap();
        // the session changes
        s.on_tab_removed(1, false);
        s.on_tab_upsert(&tab(3, "https://c.com"), 3.0);

        // merge: only a.com comes back
        let (target, added) = s.restore_snapshot(sid, snap, RestoreMode::Merge, 4.0).unwrap();
        assert_eq!(target, sid);
        assert_eq!(added.len(), 1);
        assert_eq!(s.active().entries.len(), 3);

        // replace: exactly the snapshot's URLs; b.com keeps its live tab, c.com is untracked
        s.restore_snapshot(sid, snap, RestoreMode::Replace, 5.0).unwrap();
        let urls: Vec<_> = s.active().entries.iter().map(|e| (e.url.as_str(), e.tab_id)).collect();
        assert_eq!(urls, vec![("https://a.com", None), ("https://b.com", Some(2))]);
        assert!(s.ignored_tab_ids.contains(&3));
        let before = s.active().snapshots.last().unwrap();
        assert!(before.auto && before.name.contains("v1") && before.urls().contains("https://c.com"));
        assert!(before.entries.iter().all(|e| e.tab_id.is_none()));

        // new session
        let (new_id, added) = s.restore_snapshot(sid, snap, RestoreMode::NewSession, 6.0).unwrap();
        assert_ne!(new_id, sid);
        assert_eq!(added.len(), 2);
        assert_eq!(s.session(new_id).unwrap().name, "Default (v1)");
    }

    #[test]
    fn auto_snapshots_only_on_change_and_pruned() {
        let mut s = AppState::default();
        s.settings.max_auto_snapshots = 2;
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        s.create_snapshot(s.active_session, "manual", false, 1.0).unwrap();
        assert_eq!(s.auto_snapshot_all(2.0), 0, "unchanged since last snapshot");
        for i in 0..4 {
            s.on_tab_upsert(&tab(10 + i, &format!("https://x{i}.com")), 3.0);
            assert_eq!(s.auto_snapshot_all(3.0 + i as f64), 1);
        }
        let snaps = &s.active().snapshots;
        assert_eq!(snaps.iter().filter(|x| x.auto).count(), 2);
        assert_eq!(snaps[0].name, "manual", "manual snapshots are never pruned");
    }

    #[test]
    fn import_urls_and_sessions() {
        let mut s = AppState::default();
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        let urls = vec!["https://a.com".to_string(), "https://b.com".into()];
        let (sid, added) = s.import_urls(&urls, &ImportTarget::Session(s.active_session), 2.0).unwrap();
        assert_eq!((sid, added.len()), (s.active_session, 1), "duplicates skipped");
        let (new_sid, added) = s.import_urls(&urls, &ImportTarget::NewSession("Reading".into()), 2.0).unwrap();
        assert_eq!(added.len(), 2);
        assert_eq!(s.session(new_sid).unwrap().name, "Reading");

        let exported = s.session(new_sid).unwrap().clone();
        let ids = s.import_sessions(vec![exported], 3.0);
        let imported = s.session(ids[0]).unwrap();
        assert_eq!(imported.name, "Reading (imported)");
        assert_eq!(imported.entries.len(), 2);
        let all_ids: HashSet<Id> = s.sessions.iter().flat_map(|x| x.entries.iter().map(|e| e.id)).collect();
        assert_eq!(all_ids.len(), s.sessions.iter().map(|x| x.entries.len()).sum::<usize>(), "entry ids stay unique");
    }

    #[test]
    fn normalize_fixes_ids() {
        let mut s = AppState { next_id: 1, ..Default::default() };
        s.sessions[0].entries.push(TabEntry { id: 40, ..Default::default() });
        s.active_session = 12345;
        s.normalize();
        assert_eq!(s.next_id, 41);
        assert_eq!(s.active_session, s.sessions[0].id);
    }
}
