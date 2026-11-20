//! How the state is saved: pure planning, tested natively; the background does the I/O.
//!
//! The state lives in the extension's IndexedDB database (see `smPlatform.db*`):
//! - `meta['state']`: the state without the sessions' tabs and snapshots (JSON),
//! - `tabs[entry id]`: `{ s: session id, j: entry JSON }`, indexed by session,
//! - `vectors[session id]`: the session's vector index (JSON).
//!
//! A save writes only the tabs that changed (and deletes the removed ones), in one
//! transaction with the state. Each session's `tabs_revision` tells pages which sessions'
//! tabs to read again. Snapshots stay in `storage.local` (`SNAPSHOTS_KEY`).
//!
//! Earlier versions kept everything in `storage.local`: the state with its tabs under
//! `STATE_KEY` (up to 0.14), and in a development build each session's tabs under
//! `sm_session_<id>`. At startup the newest of what IndexedDB and `storage.local` hold
//! (by `revision`) is used; the other is cleared once the state is saved. With the
//! database switched off (or unavailable), the state is saved in `STATE_KEY` as before.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ai;
use crate::model::{AppState, Id, TabEntry};
use crate::protocol::SessionFile;

/// A tab as stored in the database.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TabRecord {
    pub id: Id,
    /// Its session.
    pub s: Id,
    /// The entry, as JSON.
    pub j: String,
}

/// What a saved tab was: its session and the hash of its JSON.
pub type Saved = HashMap<Id, (Id, u64)>;

/// What to write to the database.
#[derive(Debug, Default, PartialEq)]
pub struct WritePlan {
    pub puts: Vec<TabRecord>,
    pub dels: Vec<Id>,
    /// What is saved once the plan is written.
    pub saved: Saved,
}

/// The tabs to write and delete so the database matches `state`, given what was saved.
/// Sessions with changed tabs get `tabs_revision = revision`.
pub fn plan(state: &mut AppState, saved: &Saved, revision: u64) -> WritePlan {
    let mut out = WritePlan { saved: HashMap::with_capacity(saved.len()), ..Default::default() };
    let mut changed: HashSet<Id> = HashSet::new();
    for s in &state.sessions {
        for e in &s.entries {
            let Ok(j) = serde_json::to_string(e) else { continue };
            let h = ai::hash(&j);
            match saved.get(&e.id) {
                Some((sid, old)) if *sid == s.id && *old == h => {}
                old => {
                    // A tab moved to another session changes both.
                    if let Some((sid, _)) = old {
                        changed.insert(*sid);
                    }
                    changed.insert(s.id);
                    out.puts.push(TabRecord { id: e.id, s: s.id, j });
                }
            }
            out.saved.insert(e.id, (s.id, h));
        }
    }
    for (id, (sid, _)) in saved {
        if !out.saved.contains_key(id) {
            out.dels.push(*id);
            changed.insert(*sid);
        }
    }
    out.dels.sort_unstable();
    for s in state.sessions.iter_mut().filter(|s| changed.contains(&s.id)) {
        s.tabs_revision = revision;
    }
    out
}

/// The state as saved in `meta['state']`: without the sessions' tabs and snapshots, with
/// each session's tab order (`Session::order`).
pub fn header_json(state: &mut AppState) -> serde_json::Result<String> {
    let taken: Vec<(Vec<TabEntry>, Vec<crate::model::Snapshot>)> = state
        .sessions
        .iter_mut()
        .map(|s| {
            s.order = s.entries.iter().map(|e| e.id).collect();
            (std::mem::take(&mut s.entries), std::mem::take(&mut s.snapshots))
        })
        .collect();
    let json = serde_json::to_string(state);
    for (s, (entries, snapshots)) in state.sessions.iter_mut().zip(taken) {
        s.entries = entries;
        s.snapshots = snapshots;
        s.order.clear();
    }
    json
}

/// Puts a session's tabs (read from the database) in their saved order; tabs missing from
/// the order (none, normally) go last, oldest first.
pub fn put_in_order(s: &mut crate::model::Session, mut tabs: Vec<TabEntry>) {
    let pos: HashMap<Id, usize> = s.order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    tabs.sort_by_key(|e| (pos.get(&e.id).copied().unwrap_or(usize::MAX), e.id));
    s.entries = tabs;
    s.order.clear();
}

/// The state read from the database: its header with the tabs put back in their sessions.
/// Unreadable tabs, and tabs of sessions that no longer exist, are left out. Returns the
/// state and what is saved.
pub fn assemble(header: &str, tabs: &[TabRecord]) -> Result<(AppState, Saved), String> {
    let mut state: AppState = serde_json::from_str(header).map_err(|e| format!("saved state unreadable: {e}"))?;
    let mut by_session: HashMap<Id, Vec<TabEntry>> = HashMap::new();
    let mut saved = Saved::new();
    let sessions: HashSet<Id> = state.sessions.iter().map(|s| s.id).collect();
    for t in tabs.iter().filter(|t| sessions.contains(&t.s)) {
        // One unreadable tab doesn't cost the others.
        if let Ok(e) = serde_json::from_str::<TabEntry>(&t.j) {
            saved.insert(t.id, (t.s, ai::hash(&t.j)));
            by_session.entry(t.s).or_default().push(e);
        }
    }
    for s in &mut state.sessions {
        let tabs = by_session.remove(&s.id).unwrap_or_default();
        put_in_order(s, tabs);
    }
    Ok((state, saved))
}

/// Puts the tabs saved per session (`sm_session_<id>`, a development build) back into a
/// state read from `STATE_KEY`. `file` returns a session's saved JSON.
pub fn legacy_with_session_files(state: &mut AppState, file: impl Fn(Id) -> Option<String>) {
    if state.version < crate::protocol::SPLIT_VERSION {
        return;
    }
    for s in &mut state.sessions {
        if let Some(f) = file(s.id).and_then(|json| serde_json::from_str::<SessionFile>(&json).ok()) {
            s.entries = f.entries;
        }
    }
}

/// Where the state was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Database,
    /// `storage.local` (`STATE_KEY`): earlier versions, or the database switched off.
    Legacy,
    Nothing,
}

/// Which saved state to use: the newest (the other one is left from before a migration
/// or a switch, or from a run where the database couldn't be opened).
pub fn newest(db: Option<&AppState>, legacy: Option<&AppState>) -> Source {
    match (db, legacy) {
        (Some(d), Some(l)) if l.revision > d.revision => Source::Legacy,
        (Some(_), _) => Source::Database,
        (None, Some(_)) => Source::Legacy,
        (None, None) => Source::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BrowserTab, EntryRef};

    fn tab(id: i64, url: &str) -> BrowserTab {
        BrowserTab { id, window_id: 1, url: url.into(), ..Default::default() }
    }

    fn header(s: &mut AppState) -> String {
        header_json(s).unwrap()
    }

    /// Applies a plan to an in-memory "database".
    fn apply(db: &mut HashMap<Id, TabRecord>, p: &WritePlan) {
        for id in &p.dels {
            db.remove(id);
        }
        for r in &p.puts {
            db.insert(r.id, r.clone());
        }
    }

    #[test]
    fn writes_only_changed_tabs_and_round_trips() {
        let mut s = AppState::default();
        for i in 1..=5 {
            s.on_tab_upsert(&tab(i, &format!("https://{i}.com")), 1.0);
        }
        let mut db = HashMap::new();
        let first = plan(&mut s, &Saved::new(), 1);
        assert_eq!(first.puts.len(), 5);
        assert_eq!(s.active().tabs_revision, 1);
        apply(&mut db, &first);

        // Nothing changed: nothing written, the revision stays.
        let again = plan(&mut s, &first.saved, 2);
        assert!(again.puts.is_empty() && again.dels.is_empty());
        assert_eq!(s.active().tabs_revision, 1);

        // One tab navigates, one closes, one moves to another session.
        s.on_tab_upsert(&tab(2, "https://two.com"), 3.0);
        s.on_tab_removed(3, false, 3.0);
        let other = s.create_session("Other", 3.0);
        let moved = s.locate_tab(4).unwrap();
        s.move_entries(&[moved], other).unwrap();
        let p = plan(&mut s, &again.saved, 3);
        let mut put_ids: Vec<Id> = p.puts.iter().map(|r| r.id).collect();
        put_ids.sort();
        let mut want = vec![moved.entry, s.entry_for_tab(2).unwrap().id];
        want.sort();
        assert_eq!(put_ids, want);
        assert_eq!(p.dels.len(), 1);
        assert_eq!((s.active().tabs_revision, s.session(other).unwrap().tabs_revision), (3, 3));
        apply(&mut db, &p);

        // The order of tabs isn't their id order: a tab moved in goes last.
        let older = s.entries_by_tab()[&5].id;
        s.move_entries(&[EntryRef { session: s.active_session, entry: older }], other).unwrap();
        s.move_entries(&[moved_back(&s, other, moved.entry)], s.active_session).unwrap();
        let p = plan(&mut s, &p.saved, 4);
        apply(&mut db, &p);
        let records: Vec<TabRecord> = db.values().cloned().collect();
        let json = header(&mut s);
        assert!(s.sessions.iter().all(|x| x.order.is_empty() && !x.entries.is_empty()), "the state is left as it was");
        let (back, saved) = assemble(&json, &records).unwrap();
        assert_eq!(back.sessions, s.sessions);
        assert_eq!(saved, p.saved);
        assert!(!json.contains("https://"), "no tabs in the header");
    }

    fn moved_back(s: &AppState, from: Id, entry: Id) -> EntryRef {
        assert!(s.session(from).unwrap().entry(entry).is_some());
        EntryRef { session: from, entry }
    }

    #[test]
    fn assemble_skips_unreadable_and_orphaned_tabs() {
        let mut s = AppState::default();
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        let p = plan(&mut s, &Saved::new(), 1);
        let mut records = p.puts.clone();
        records.push(TabRecord { id: 900, s: s.active_session, j: "{not json".into() });
        records.push(TabRecord { id: 901, s: 424242, j: "{}".into() });
        let (back, saved) = assemble(&header(&mut s), &records).unwrap();
        assert_eq!(back.active().entries.len(), 1);
        assert_eq!(saved.len(), 1);
        // A later save deletes nothing it doesn't know about, and rewrites nothing.
        let mut back = back;
        let again = plan(&mut back, &saved, 2);
        assert!(again.puts.is_empty() && again.dels.is_empty());
        assert!(assemble("not json", &[]).is_err());
    }

    #[test]
    fn migrates_from_storage_local_formats() {
        // 0.14: the tabs are in the state itself.
        let mut old = AppState::default();
        old.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        let json = serde_json::to_string(&old).unwrap();
        let mut read: AppState = serde_json::from_str(&json).unwrap();
        legacy_with_session_files(&mut read, |_| panic!("0.14 has no session keys"));
        assert_eq!(read.active().entries.len(), 1);

        // Development build: tabs per session key.
        let mut split = old.clone();
        split.version = crate::protocol::SPLIT_VERSION;
        let sid = split.active_session;
        let file = serde_json::to_string(&SessionFile { revision: 3, entries: split.active().entries.clone() }).unwrap();
        split.sessions[0].entries.clear();
        legacy_with_session_files(&mut split, |id| (id == sid).then(|| file.clone()));
        assert_eq!(split.active().entries, old.active().entries);
    }

    #[test]
    fn the_newest_state_wins() {
        let at = |revision| AppState { revision, ..Default::default() };
        assert_eq!(newest(Some(&at(5)), Some(&at(9))), Source::Legacy, "saved in storage.local since");
        assert_eq!(newest(Some(&at(9)), Some(&at(5))), Source::Database, "migrated; the old copy is left over");
        assert_eq!(newest(Some(&at(5)), Some(&at(5))), Source::Database);
        assert_eq!(newest(None, Some(&at(1))), Source::Legacy);
        assert_eq!(newest(Some(&at(1)), None), Source::Database);
        assert_eq!(newest(None, None), Source::Nothing);
    }
}
