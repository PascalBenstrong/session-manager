//! How the state is saved: pure planning, tested natively; the background does the I/O.
//!
//! The state lives in the extension's IndexedDB database (see `smPlatform.db*`):
//! - `meta['state']`: the state without the sessions' tabs and snapshots (JSON),
//! - `tabs[entry id]`: `{ s: session id, o: position, j: entry JSON }`, indexed by session,
//! - `vectors[session id]`: the session's vector index (JSON).
//!
//! A save writes only the tabs that changed and deletes the removed ones, in one transaction
//! with the state. What changed is tracked as it changes (`entries.rs`): a save visits only
//! sessions with changes and serializes only changed tabs — O(sessions + changed tabs) when
//! existing tabs changed, plus O(tabs of a session) for a session whose tabs were added or
//! removed (its ids are compared with the saved ones to find what to delete). The state
//! record doesn't hold the tabs or their order, so it doesn't grow with them. Each
//! session's `tabs_revision` tells pages which sessions' tabs to read again. Snapshots stay
//! in `storage.local` (`SNAPSHOTS_KEY`).
//!
//! Earlier versions kept everything in `storage.local`: the state with its tabs under
//! `STATE_KEY` (up to 0.14), and in a development build each session's tabs under
//! `sm_session_<id>`. At startup the newest of what IndexedDB and `storage.local` hold
//! (by `revision`) is used; the other is cleared once the state is saved. With the
//! database switched off (or unavailable), the state is saved in `STATE_KEY` as before.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::entries::{Changes, Entries};
use crate::model::{AppState, Id, TabEntry};
use crate::protocol::SessionFile;

/// A tab as stored in the database.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TabRecord {
    pub id: Id,
    /// Its session.
    pub s: Id,
    /// Its position in the session (sequence number; see `entries.rs`).
    #[serde(default)]
    pub o: u64,
    /// The entry, as JSON.
    pub j: String,
}

/// What the database holds: each session's tab ids.
pub type Saved = HashMap<Id, HashSet<Id>>;

/// What to write to the database.
#[derive(Debug, Default, PartialEq)]
pub struct WritePlan {
    /// Empty the tabs first (the database is out of date: everything is written).
    pub clear: bool,
    pub puts: Vec<TabRecord>,
    pub dels: Vec<Id>,
}

/// The tabs that changed since the last save, to write, and the removed ones, to delete.
/// `saved` (what the database holds) is updated as if the plan was written. Sessions with
/// changes get `tabs_revision = revision`, and the changes are forgotten: if the write
/// fails, the next save must be [`plan_full`].
pub fn plan_changes(state: &mut AppState, saved: &mut Saved, revision: u64) -> WritePlan {
    let mut plan = WritePlan::default();
    let put = |plan: &mut WritePlan, sid: Id, o: u64, e: &TabEntry| {
        if let Ok(j) = serde_json::to_string(e) {
            plan.puts.push(TabRecord { id: e.id, s: sid, o, j });
        }
    };
    for s in state.sessions.iter_mut().filter(|s| s.entries.has_changes()) {
        match s.entries.changes() {
            Changes::Some(changed) => {
                for (o, e) in changed {
                    put(&mut plan, s.id, o, e);
                }
            }
            Changes::All(all) => {
                let now: HashSet<Id> = all.iter().map(|(_, e, _)| e.id).collect();
                for (o, e, changed) in all {
                    if changed {
                        put(&mut plan, s.id, o, e);
                    }
                }
                if let Some(before) = saved.get(&s.id) {
                    plan.dels.extend(before.difference(&now));
                }
                saved.insert(s.id, now);
            }
        }
        s.tabs_revision = revision;
        s.entries.clear_changes();
    }
    // Deleted sessions' tabs go.
    let live: HashSet<Id> = state.sessions.iter().map(|s| s.id).collect();
    saved.retain(|sid, ids| {
        let keep = live.contains(sid);
        if !keep {
            plan.dels.extend(ids.iter());
        }
        keep
    });
    // A tab moved to another session is removed from one and put in the other.
    if !plan.dels.is_empty() {
        let put: HashSet<Id> = plan.puts.iter().map(|r| r.id).collect();
        plan.dels.retain(|id| !put.contains(id));
        plan.dels.sort_unstable();
        plan.dels.dedup();
    }
    plan
}

/// Every tab, to rewrite the database from scratch (after the state came from storage.local,
/// or a failed write). Every session gets `tabs_revision = revision`; `saved` is rebuilt.
pub fn plan_full(state: &mut AppState, saved: &mut Saved, revision: u64) -> WritePlan {
    let mut plan = WritePlan { clear: true, ..Default::default() };
    saved.clear();
    for s in &mut state.sessions {
        for (o, e) in s.entries.with_seq() {
            if let Ok(j) = serde_json::to_string(e) {
                plan.puts.push(TabRecord { id: e.id, s: s.id, o, j });
            }
        }
        saved.insert(s.id, s.entries.iter().map(|e| e.id).collect());
        s.tabs_revision = revision;
        s.entries.clear_changes();
    }
    plan
}

/// Forgets every session's changes (the whole state was saved elsewhere).
pub fn forget_changes(state: &mut AppState) {
    for s in &mut state.sessions {
        s.entries.clear_changes();
    }
}

/// The state as saved in `meta['state']`: without the sessions' tabs and snapshots. The
/// tabs are moved out and back whole, so nothing counts as changed.
pub fn header_json(state: &mut AppState) -> serde_json::Result<String> {
    let taken: Vec<(Entries, Vec<crate::model::Snapshot>)> =
        state.sessions.iter_mut().map(|s| (std::mem::take(&mut s.entries), std::mem::take(&mut s.snapshots))).collect();
    let json = serde_json::to_string(state);
    for (s, (entries, snapshots)) in state.sessions.iter_mut().zip(taken) {
        s.entries = entries;
        s.snapshots = snapshots;
    }
    json
}

/// A session's tabs as read from the database: (position, JSON). Unreadable ones are left out.
pub fn read_tabs<'a>(tabs: impl IntoIterator<Item = (u64, &'a str)>) -> Entries {
    Entries::from_saved(tabs.into_iter().filter_map(|(o, j)| serde_json::from_str::<TabEntry>(j).ok().map(|e| (o, e))).collect())
}

/// The state read from the database: its header with the tabs put back in their sessions,
/// in order, with nothing marked changed, and what the database holds. Unreadable tabs are
/// left out; tabs of sessions that no longer exist too (and are deleted at the next save).
pub fn assemble(header: &str, tabs: &[TabRecord]) -> Result<(AppState, Saved), String> {
    let mut state: AppState = serde_json::from_str(header).map_err(|e| format!("saved state unreadable: {e}"))?;
    let mut by_session: HashMap<Id, Vec<(u64, &str)>> = HashMap::new();
    let mut saved = Saved::new();
    for t in tabs {
        by_session.entry(t.s).or_default().push((t.o, &t.j));
        saved.entry(t.s).or_default().insert(t.id);
    }
    for s in &mut state.sessions {
        s.entries = read_tabs(by_session.remove(&s.id).unwrap_or_default());
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
            s.entries = f.entries.into();
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
    use crate::cleanup::CleanupReport;
    use crate::dedupe::{DedupeOptions, Scope};
    use crate::hibernate::HibernateTarget;
    use crate::model::{AutoRule, BrowserTab, EntryRef, Snapshot};
    use crate::pattern::{PatternKind, SitePattern};
    use crate::state::{ImportTarget, RestoreMode};
    use crate::workspace::{Workspace, WorkspaceRule};

    fn tab(id: i64, url: &str) -> BrowserTab {
        BrowserTab { id, window_id: 1, url: url.into(), title: format!("T{id}"), ..Default::default() }
    }

    /// A model of the database: written only with the plans, like the real one.
    #[derive(Default)]
    struct Db {
        header: String,
        tabs: HashMap<Id, TabRecord>,
        saved: Saved,
    }

    impl Db {
        fn save(&mut self, state: &mut AppState, full: bool) -> WritePlan {
            state.revision += 1;
            let rev = state.revision;
            let plan = if full { plan_full(state, &mut self.saved, rev) } else { plan_changes(state, &mut self.saved, rev) };
            if plan.clear {
                self.tabs.clear();
            }
            for id in &plan.dels {
                self.tabs.remove(id);
            }
            for r in &plan.puts {
                self.tabs.insert(r.id, r.clone());
            }
            self.header = header_json(state).unwrap();
            plan
        }

        fn load(&self) -> AppState {
            let (state, saved) = assemble(&self.header, &self.tabs.values().cloned().collect::<Vec<_>>()).unwrap();
            assert_eq!(saved.values().map(|x| x.len()).sum::<usize>(), self.tabs.len());
            state
        }
    }

    /// The oracle: after saving only what was tracked, the database must hold exactly the
    /// state — every tab, in its session, in order. A change made without being tracked
    /// fails here.
    fn saves_exactly(db: &mut Db, state: &mut AppState, what: &str) {
        db.save(state, false);
        let back = db.load();
        assert_eq!(back.sessions.len(), state.sessions.len(), "{what}: sessions");
        for (a, b) in back.sessions.iter().zip(&state.sessions) {
            assert_eq!(a.entries, b.entries, "{what}: tabs of session {} after saving only the tracked changes", b.id);
        }
        let in_state: usize = state.sessions.iter().map(|s| s.entries.len()).sum();
        assert_eq!(db.tabs.len(), in_state, "{what}: no tab left over in the database");
        assert!(state.sessions.iter().all(|s| !s.entries.has_changes()), "{what}: changes forgotten once saved");
    }

    #[test]
    fn every_way_of_changing_tabs_is_saved() {
        let mut s = AppState::default();
        let mut db = Db::default();
        db.save(&mut s, true);
        let sid = s.active_session;
        let step = |db: &mut Db, s: &mut AppState, what: &str| saves_exactly(db, s, what);

        for i in 1..=6 {
            s.on_tab_upsert(&tab(i, &format!("https://site{i}.com/")), 1.0);
        }
        step(&mut db, &mut s, "tabs opened");
        s.on_tab_upsert(&BrowserTab { title: "renamed".into(), ..tab(1, "https://site1.com/next") }, 2.0);
        step(&mut db, &mut s, "navigation");
        s.on_tab_activated(2, 3.0);
        step(&mut db, &mut s, "activated");
        s.on_tab_replaced(60, 3);
        step(&mut db, &mut s, "replaced");
        s.mark_discarded(&[4], 4.0);
        let _ = s.hibernate_tabs(&HibernateTarget::All);
        step(&mut db, &mut s, "hibernated");
        s.on_tab_removed(5, false, 5.0);
        step(&mut db, &mut s, "closed");
        s.on_tab_upsert(&BrowserTab { window_id: 2, ..tab(7, "https://w2.com/") }, 5.0);
        s.on_tab_upsert(&BrowserTab { window_id: 2, ..tab(8, "https://w2b.com/") }, 5.0);
        s.on_tab_removed(7, true, 6.0);
        s.on_tab_removed(8, true, 6.0);
        step(&mut db, &mut s, "window closed, tabs kept");
        let r6 = s.locate_tab(6).unwrap();
        s.close_entries(&[r6], 6.0);
        step(&mut db, &mut s, "closed by hand, kept");
        let r1 = s.locate_tab(1).unwrap();
        let r2 = s.locate_tab(2).unwrap();
        s.tag_entries(&[r1, r2], "t", true);
        s.set_group(&[r1], Some("G"));
        step(&mut db, &mut s, "tags and group");
        s.auto_rules.push(AutoRule {
            id: 900,
            enabled: true,
            pattern: SitePattern::new(PatternKind::Domain, "site2.com"),
            tags: ["auto".to_string()].into(),
            group: None,
        });
        s.apply_auto_rules_all();
        step(&mut db, &mut s, "auto rules");
        let ws = s
            .save_workspace(Workspace {
                name: "W".into(),
                auto_add: true,
                rules: vec![WorkspaceRule::Tag("t".into())],
                ..Default::default()
            })
            .unwrap();
        s.auto_place(None, false);
        step(&mut db, &mut s, "workspace rules");
        s.assign_workspace(&[r1], None).unwrap();
        step(&mut db, &mut s, "workspace assigned");
        s.delete_workspace(ws).unwrap();
        step(&mut db, &mut s, "workspace deleted");
        let other = s.create_session("Other", 7.0);
        s.move_entries(&[r2], other).unwrap();
        step(&mut db, &mut s, "moved to another session");
        let back = EntryRef { session: other, entry: r2.entry };
        s.move_entries(&[back], sid).unwrap();
        step(&mut db, &mut s, "moved back (goes last)");
        s.set_active(other).unwrap();
        s.on_tab_upsert(&tab(9, "https://site1.com/next"), 8.0);
        s.on_tab_upsert(&tab(10, "https://dup.com/"), 8.0);
        s.on_tab_upsert(&tab(11, "https://dup.com/"), 8.0);
        step(&mut db, &mut s, "tabs in the other session");
        s.remove_duplicates(Scope::All, &DedupeOptions::default());
        step(&mut db, &mut s, "duplicates removed");
        s.on_tab_upsert(&tab(12, "https://dup.com/"), 9.0);
        s.remove_restored_copies(8.5, &DedupeOptions::default());
        step(&mut db, &mut s, "restored copies removed");
        let snap = s.create_snapshot(sid, "v1", false, 9.0).unwrap();
        s.on_tab_upsert(&tab(13, "https://later.com/"), 9.0);
        step(&mut db, &mut s, "snapshot taken");
        s.restore_snapshot(sid, snap, RestoreMode::Merge, 10.0).unwrap();
        step(&mut db, &mut s, "snapshot merged");
        s.restore_snapshot(sid, snap, RestoreMode::Replace, 10.0).unwrap();
        step(&mut db, &mut s, "snapshot replaced");
        s.restore_snapshot(sid, snap, RestoreMode::NewSession, 10.0).unwrap();
        step(&mut db, &mut s, "snapshot as a new session");
        s.import_urls(&["https://imp.com/".into()], &ImportTarget::Session(sid), 11.0).unwrap();
        s.import_urls(&["https://imp2.com/".into()], &ImportTarget::NewSession("Imp".into()), 11.0).unwrap();
        let exported = s.session(sid).unwrap().clone();
        s.import_sessions(vec![exported], 11.0);
        step(&mut db, &mut s, "imports");
        s.renew_last_closed(sid);
        step(&mut db, &mut s, "new last_closed generation");
        let first = s.session(sid).unwrap().entries[0].id;
        let fake_tab = tab(70, &s.session(sid).unwrap().entries[0].url.clone());
        s.link_tab(EntryRef { session: sid, entry: first }, &fake_tab, 12.0);
        step(&mut db, &mut s, "tab opened for an entry");
        let open: Vec<BrowserTab> = vec![tab(70, &fake_tab.url), tab(71, "https://imp.com/"), tab(72, "https://fresh.com/")];
        s.reconcile(&open, 13.0);
        step(&mut db, &mut s, "reconciled after a restart");
        s.link_by_url(&tab(73, "https://imp2.com/"), 13.0);
        step(&mut db, &mut s, "linked by URL");
        let refs = s.last_closed_refs(sid);
        s.remove_entries(&refs, true);
        let all: Vec<EntryRef> =
            s.sessions.iter().flat_map(|x| x.entries.iter().map(move |e| EntryRef { session: x.id, entry: e.id })).take(2).collect();
        s.remove_entries(&all, false);
        step(&mut db, &mut s, "removed");
        let gone = s.sessions.last().unwrap().id;
        s.delete_session(gone).unwrap();
        step(&mut db, &mut s, "session deleted");
        s.normalize();
        step(&mut db, &mut s, "normalized");
        s.push_cleanup_report(CleanupReport::default());
        s.sessions[0].snapshots.push(Snapshot::default());
        step(&mut db, &mut s, "changes elsewhere don't touch tabs");
        // Nothing changed: nothing written.
        let plan = db.save(&mut s, false);
        assert!(plan.puts.is_empty() && plan.dels.is_empty() && !plan.clear);
    }

    #[test]
    fn a_save_serializes_only_the_changed_tabs() {
        let mut s = AppState::default();
        for i in 1..=500 {
            s.on_tab_upsert(&tab(i, &format!("https://{i}.com/")), 1.0);
        }
        let mut db = Db::default();
        assert_eq!(db.save(&mut s, true).puts.len(), 500);
        s.on_tab_activated(250, 2.0);
        let plan = db.save(&mut s, false);
        assert_eq!(plan.puts.len(), 1, "one tab changed: one tab serialized and written");
        assert_eq!(plan.puts[0].id, s.entry_for_tab(250).unwrap().id);
        let header = header_json(&mut s).unwrap();
        assert!(!header.contains("https://"), "no tabs (or their order) in the state record");
        assert!(header.len() < 2000, "the state record doesn't grow with the tabs ({} bytes)", header.len());
    }

    #[test]
    fn order_survives_moves_and_reloads() {
        let mut s = AppState::default();
        for i in 1..=4 {
            s.on_tab_upsert(&tab(i, &format!("https://{i}.com/")), 1.0);
        }
        let other = s.create_session("Other", 1.0);
        let first = s.locate_tab(1).unwrap();
        s.move_entries(&[first], other).unwrap();
        s.move_entries(&[EntryRef { session: other, entry: first.entry }], s.active_session).unwrap();
        let mut db = Db::default();
        db.save(&mut s, true);
        let order = |st: &AppState| st.active().entries.iter().map(|e| e.tab_id).collect::<Vec<_>>();
        assert_eq!(order(&s), [Some(2), Some(3), Some(4), Some(1)], "a tab moved back goes last");
        assert_eq!(order(&db.load()), order(&s));
        // Loaded tabs carry on: a new one goes after them, and nothing counts as changed.
        let (mut loaded, saved) = assemble(&db.header, &db.tabs.values().cloned().collect::<Vec<_>>()).unwrap();
        db.saved = saved;
        assert!(!loaded.active().entries.has_changes());
        loaded.on_tab_upsert(&tab(5, "https://5.com/"), 2.0);
        let plan = db.save(&mut loaded, false);
        assert_eq!(plan.puts.len(), 1);
        assert!(plan.dels.is_empty());
        assert_eq!(order(&db.load()), [Some(2), Some(3), Some(4), Some(1), Some(5)]);
    }

    #[test]
    fn assemble_skips_unreadable_and_orphaned_tabs() {
        let mut s = AppState::default();
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        let mut db = Db::default();
        db.save(&mut s, true);
        let mut records: Vec<TabRecord> = db.tabs.values().cloned().collect();
        records.push(TabRecord { id: 900, s: s.active_session, o: 5, j: "{not json".into() });
        records.push(TabRecord { id: 901, s: 424242, o: 0, j: "{}".into() });
        let (back, saved) = assemble(&db.header, &records).unwrap();
        assert_eq!(back.active().entries.len(), 1);
        // The orphan is known as saved, so the next save deletes it.
        let mut back = back;
        let mut saved = saved;
        let plan = plan_changes(&mut back, &mut saved, 9);
        assert_eq!(plan.dels, [901]);
        assert!(assemble("not json", &[]).is_err());
    }

    #[test]
    fn migrates_from_storage_local_formats() {
        // 0.14: the tabs are in the state itself; read from there, everything is to be saved.
        let mut old = AppState::default();
        old.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        let json = serde_json::to_string(&old).unwrap();
        let mut read: AppState = serde_json::from_str(&json).unwrap();
        legacy_with_session_files(&mut read, |_| panic!("0.14 has no session keys"));
        assert_eq!(read.active().entries.len(), 1);
        assert!(read.active().entries.has_changes(), "migrated tabs are all written");

        // Development build: tabs per session key.
        let mut split = old.clone();
        split.version = crate::protocol::SPLIT_VERSION;
        let sid = split.active_session;
        let file = serde_json::to_string(&SessionFile { revision: 3, entries: split.active().entries.to_vec() }).unwrap();
        split.sessions[0].entries = Entries::default();
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
