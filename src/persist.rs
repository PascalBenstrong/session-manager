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
//! session's `tabs_revision` tells pages which sessions' tabs to read again.
//!
//! Snapshots are in the database too (`snapshots[snapshot id]`: `{ s: session id, j:
//! snapshot JSON }`, indexed by session), written with the same transaction. They don't
//! change once taken: a save writes the new ones and deletes the removed ones. The state
//! keeps only what describes them (`Snapshot::meta`); their tabs are read for a restore,
//! a page or an export. Earlier versions kept them all in `storage.local` (`SNAPSHOTS_KEY`).
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
    /// 1 if the tab is open (indexed: the background reads only open tabs of inactive
    /// sessions), else 0.
    #[serde(default)]
    pub a: u8,
    /// The entry, as JSON.
    pub j: String,
}

impl TabRecord {
    fn of(s: Id, o: u64, e: &TabEntry) -> Option<Self> {
        serde_json::to_string(e).ok().map(|j| TabRecord { id: e.id, s, o, a: e.tab_id.is_some() as u8, j })
    }
}

/// The `version` of a state saved in the database with open tabs indexed (`TabRecord::a`)
/// and each session's `next_seq`. Older ones are read whole and saved again.
pub const DB_LAYOUT: u32 = 3;

/// A snapshot as stored in the database.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SnapRecord {
    pub id: Id,
    /// Its session.
    pub s: Id,
    /// The snapshot with its tabs, as JSON.
    pub j: String,
}

/// What to write to the database.
#[derive(Debug, Default, PartialEq)]
pub struct WritePlan {
    /// Snapshots taken since the last save, and snapshots deleted.
    pub snap_puts: Vec<SnapRecord>,
    pub snap_dels: Vec<Id>,
    /// Empty the tabs first (the database is out of date: everything is written).
    pub clear: bool,
    pub puts: Vec<TabRecord>,
    pub dels: Vec<Id>,
    /// Sessions deleted: all their tabs go.
    pub del_sessions: Vec<Id>,
}

/// The tabs that changed since the last save, to write, and the removed ones, to delete,
/// and the sessions deleted since `seen` (the sessions saved last time; updated). Sessions
/// with changes get `tabs_revision = revision`, and the changes are forgotten (if the
/// write fails, keep the plan for the next one: [`retry`]).
pub fn plan_changes(state: &mut AppState, seen: &mut HashSet<Id>, revision: u64) -> WritePlan {
    let mut plan = WritePlan::default();
    for s in state.sessions.iter_mut().filter(|s| s.entries.has_changes()) {
        match s.entries.changes() {
            Changes::Some(changed) => plan.puts.extend(changed.into_iter().filter_map(|(o, e)| TabRecord::of(s.id, o, e))),
            Changes::All(all) => plan.puts.extend(all.into_iter().filter(|x| x.2).filter_map(|(o, e, _)| TabRecord::of(s.id, o, e))),
        }
        plan.dels.extend_from_slice(s.entries.removed());
        s.tabs_revision = revision;
        s.entries.clear_changes();
    }
    let now: HashSet<Id> = state.sessions.iter().map(|s| s.id).collect();
    plan.del_sessions = seen.difference(&now).copied().collect();
    plan.del_sessions.sort_unstable();
    *seen = now;
    // A tab moved to another session is removed from one and put in the other.
    if !plan.dels.is_empty() {
        let put: HashSet<Id> = plan.puts.iter().map(|r| r.id).collect();
        plan.dels.retain(|id| !put.contains(id));
        plan.dels.sort_unstable();
        plan.dels.dedup();
    }
    plan
}

/// Every tab, to rewrite the database from scratch (the state came from storage.local or an
/// older layout; every tab is in memory then). Every session gets `tabs_revision`.
pub fn plan_full(state: &mut AppState, seen: &mut HashSet<Id>, revision: u64) -> WritePlan {
    debug_assert!(state.sessions.iter().all(|s| s.entries.hidden().is_none()), "a full rewrite needs every tab");
    let mut plan = WritePlan { clear: true, ..Default::default() };
    for s in &mut state.sessions {
        plan.puts.extend(s.entries.with_seq().filter_map(|(o, e)| TabRecord::of(s.id, o, e)));
        s.tabs_revision = revision;
        s.entries.clear_changes();
    }
    *seen = state.sessions.iter().map(|s| s.id).collect();
    plan
}

/// Adds a plan that failed to be written to the next one (what the next one writes about
/// the same tabs is newer and wins).
pub fn retry(plan: &mut WritePlan, failed: WritePlan) {
    let snap_puts: HashSet<Id> = plan.snap_puts.iter().map(|r| r.id).collect();
    let snap_dels: HashSet<Id> = plan.snap_dels.iter().copied().collect();
    plan.snap_puts.extend(failed.snap_puts.into_iter().filter(|r| !snap_puts.contains(&r.id) && !snap_dels.contains(&r.id)));
    plan.snap_dels.extend(failed.snap_dels.into_iter().filter(|id| !snap_puts.contains(id) && !snap_dels.contains(id)));
    let puts: HashSet<Id> = plan.puts.iter().map(|r| r.id).collect();
    let dels: HashSet<Id> = plan.dels.iter().copied().collect();
    plan.clear |= failed.clear;
    plan.puts.extend(failed.puts.into_iter().filter(|r| !puts.contains(&r.id) && !dels.contains(&r.id)));
    plan.dels.extend(failed.dels.into_iter().filter(|id| !puts.contains(id) && !dels.contains(id)));
    for s in failed.del_sessions {
        if !plan.del_sessions.contains(&s) {
            plan.del_sessions.push(s);
        }
    }
    // The deleted sessions' tabs are cleared before the puts, which may move some elsewhere.
}

/// Adds to `plan` the snapshots to write (not in the database yet: `saved`) and to delete
/// (in the database, no longer in the state). `saved` is updated as if the plan was written.
pub fn plan_snapshots(state: &AppState, saved: &mut HashSet<Id>, plan: &mut WritePlan) {
    let mut now: HashSet<Id> = HashSet::new();
    for s in &state.sessions {
        for snap in &s.snapshots {
            now.insert(snap.id);
            // Not saved yet: its tabs are in memory (it was just taken, imported or migrated).
            if !saved.contains(&snap.id)
                && snap.loaded()
                && let Ok(j) = serde_json::to_string(snap)
            {
                plan.snap_puts.push(SnapRecord { id: snap.id, s: s.id, j });
                saved.insert(snap.id);
            }
        }
    }
    let gone: Vec<Id> = saved.iter().filter(|id| !now.contains(id)).copied().collect();
    for id in gone {
        saved.remove(&id);
        plan.snap_dels.push(id);
    }
    plan.snap_dels.sort_unstable();
}

/// Lets the tabs of saved snapshots go from memory (they're in the database).
pub fn strip_snapshots(state: &mut AppState, saved: &HashSet<Id>) {
    for snap in state.sessions.iter_mut().flat_map(|s| s.snapshots.iter_mut()) {
        if saved.contains(&snap.id) && !snap.entries.is_empty() {
            snap.entries = Vec::new();
        }
    }
}

/// Snapshots read from the database (records), with their tabs.
pub fn read_snapshots(records: &[SnapRecord]) -> Vec<(Id, crate::model::Snapshot)> {
    records.iter().filter_map(|r| serde_json::from_str::<crate::model::Snapshot>(&r.j).ok().map(|x| (r.s, x))).collect()
}

/// Puts the tabs of snapshots read from the database into the state's snapshots.
pub fn load_snapshots(state: &mut AppState, records: &[SnapRecord]) {
    let mut by_id: HashMap<Id, crate::model::Snapshot> = read_snapshots(records).into_iter().map(|(_, x)| (x.id, x)).collect();
    for snap in state.sessions.iter_mut().flat_map(|s| s.snapshots.iter_mut()) {
        if !snap.loaded()
            && let Some(full) = by_id.remove(&snap.id)
        {
            snap.entries = full.entries;
        }
    }
}

/// Forgets every session's changes (the whole state was saved elsewhere), noting which
/// sessions changed (`tabs_revision`), so pages ask for them again.
pub fn forget_changes(state: &mut AppState, revision: u64) {
    for s in state.sessions.iter_mut().filter(|s| s.entries.has_changes()) {
        s.tabs_revision = revision;
        s.entries.clear_changes();
    }
}

/// The state as saved in `meta['state']`: without the sessions' tabs and snapshots (with
/// their counts and sequence numbers). The tabs are moved out and back whole, so nothing
/// counts as changed.
pub fn header_json(state: &mut AppState) -> serde_json::Result<String> {
    state.version = DB_LAYOUT;
    for s in &mut state.sessions {
        s.refresh_counts();
        s.next_seq = s.entries.next_seq();
    }
    with_snapshot_meta(state, |state| {
        let taken: Vec<Entries> = state.sessions.iter_mut().map(|s| std::mem::take(&mut s.entries)).collect();
        let json = serde_json::to_string(state);
        for (s, entries) in state.sessions.iter_mut().zip(taken) {
            s.entries = entries;
        }
        json
    })
}

/// Runs `f` with the snapshots' tabs moved out (what describes them stays), and puts them
/// back: what's saved in the state and sent to pages.
pub fn with_snapshot_meta<T>(state: &mut AppState, f: impl FnOnce(&mut AppState) -> T) -> T {
    let taken: Vec<Vec<Vec<TabEntry>>> =
        state.sessions.iter_mut().map(|s| s.snapshots.iter_mut().map(|x| std::mem::take(&mut x.entries)).collect()).collect();
    let out = f(state);
    for (s, snaps) in state.sessions.iter_mut().zip(taken) {
        for (x, entries) in s.snapshots.iter_mut().zip(snaps) {
            x.entries = entries;
        }
    }
    out
}

/// A session's tabs as read from the database: (position, JSON). Unreadable ones are left out.
pub fn read_tabs<'a>(tabs: impl IntoIterator<Item = (u64, &'a str)>) -> Entries {
    Entries::from_saved(parse_tabs(tabs))
}

/// Tabs read from the database, readable ones only.
pub fn parse_tabs<'a>(tabs: impl IntoIterator<Item = (u64, &'a str)>) -> Vec<(u64, TabEntry)> {
    tabs.into_iter().filter_map(|(o, j)| serde_json::from_str::<TabEntry>(j).ok().map(|e| (o, e))).collect()
}

/// The state read from the database, every tab in memory (an older layout, or tests).
/// Unreadable tabs, and tabs of sessions that no longer exist, are left out.
pub fn assemble(header: &str, tabs: &[TabRecord]) -> Result<AppState, String> {
    let mut state: AppState = serde_json::from_str(header).map_err(|e| format!("saved state unreadable: {e}"))?;
    let mut by_session: HashMap<Id, Vec<(u64, &str)>> = HashMap::new();
    for t in tabs {
        by_session.entry(t.s).or_default().push((t.o, &t.j));
    }
    for s in &mut state.sessions {
        s.entries = read_tabs(by_session.remove(&s.id).unwrap_or_default());
    }
    Ok(state)
}

/// The state read from the database as the background keeps it: the active session's tabs,
/// and of the other sessions only the open ones (`tabs`: those records, in any order, maybe
/// twice); their other tabs stay in the database (`Entries::hidden`).
pub fn assemble_partial(header: &str, tabs: &[TabRecord]) -> Result<AppState, String> {
    let mut state: AppState = serde_json::from_str(header).map_err(|e| format!("saved state unreadable: {e}"))?;
    let mut by_session: HashMap<Id, Vec<(u64, &str)>> = HashMap::new();
    let mut seen: HashSet<Id> = HashSet::new();
    for t in tabs.iter().filter(|t| seen.insert(t.id)) {
        by_session.entry(t.s).or_default().push((t.o, &t.j));
    }
    let active = state.active_session;
    for s in &mut state.sessions {
        let tabs = parse_tabs(by_session.remove(&s.id).unwrap_or_default());
        s.entries = if s.id == active {
            Entries::from_saved(tabs)
        } else {
            let hidden = s.counts.tabs.saturating_sub(tabs.len());
            Entries::from_saved_part(tabs, hidden, s.next_seq)
        };
    }
    Ok(state)
}

/// Takes back a session's saved tabs read from the database (all its records).
pub fn load_session(state: &mut AppState, session: Id, tabs: &[TabRecord]) {
    if let Some(s) = state.session_mut(session) {
        s.entries.merge_loaded(parse_tabs(tabs.iter().filter(|t| t.s == session).map(|t| (t.o, t.j.as_str()))));
    }
}

/// Lets the closed tabs of inactive sessions go (they're in the database), except sessions
/// with changes not saved yet. Returns how many sessions let tabs go.
pub fn evict_inactive(state: &mut AppState) -> usize {
    let active = state.active_session;
    // Also sessions already partly in memory: their tabs closed since (with their window)
    // go too.
    state
        .sessions
        .iter_mut()
        .filter(|s| s.id != active && s.entries.iter().any(|e| !e.is_open()))
        .map(|s| s.entries.evict_closed() as usize)
        .sum()
}

/// The sessions whose tabs aren't all in memory.
pub fn partial_sessions(state: &AppState) -> Vec<Id> {
    state.sessions.iter().filter(|s| s.entries.hidden().is_some()).map(|s| s.id).collect()
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
        seen: HashSet<Id>,
    }

    impl Db {
        fn save(&mut self, state: &mut AppState, full: bool) -> WritePlan {
            state.revision += 1;
            let rev = state.revision;
            let plan = if full { plan_full(state, &mut self.seen, rev) } else { plan_changes(state, &mut self.seen, rev) };
            self.apply(&plan);
            self.header = header_json(state).unwrap();
            plan
        }

        /// As `smPlatform.dbWrite` does: clear, deleted sessions' tabs, puts, deletes.
        fn apply(&mut self, plan: &WritePlan) {
            if plan.clear {
                self.tabs.clear();
            }
            self.tabs.retain(|_, r| !plan.del_sessions.contains(&r.s));
            for r in &plan.puts {
                self.tabs.insert(r.id, r.clone());
            }
            for id in &plan.dels {
                self.tabs.remove(id);
            }
        }

        fn records(&self) -> Vec<TabRecord> {
            self.tabs.values().cloned().collect()
        }

        fn load(&self) -> AppState {
            assemble(&self.header, &self.records()).unwrap()
        }

        /// What the background reads at startup: the active session, and open tabs.
        fn load_partial(&self) -> AppState {
            let active: AppState = serde_json::from_str(&self.header).unwrap();
            let tabs: Vec<TabRecord> = self.tabs.values().filter(|r| r.s == active.active_session || r.a == 1).cloned().collect();
            assemble_partial(&self.header, &tabs).unwrap()
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
        // Lookups by tab and by id answer like a scan of every session.
        for x in &state.sessions {
            for e in x.entries.iter() {
                assert_eq!(state.session(x.id).unwrap().entry(e.id).map(|f| f.id), Some(e.id), "{what}: entry {}", e.id);
                if let Some(t) = e.tab_id {
                    let scan = state.sessions.iter().find_map(|y| y.entries.iter().find(|f| f.tab_id == Some(t)).map(|f| (y.id, f.id)));
                    assert_eq!(state.locate_tab(t).map(|r| (r.session, r.entry)), scan, "{what}: tab {t}");
                }
            }
        }
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

    /// The background keeps only the active session's tabs and other sessions' open tabs:
    /// the same changes made with every tab in memory and with inactive sessions' closed
    /// tabs left in the database (read back only when an operation needs them) must leave
    /// the same database.
    #[test]
    fn inactive_sessions_keep_only_open_tabs_and_lose_nothing() {
        let mut full = AppState::default();
        let a = full.active_session;
        let b = full.create_session("B", 0.0);
        let c = full.create_session("C", 0.0);
        for (sid, base) in [(a, 1), (b, 10), (c, 20)] {
            full.set_active(sid).unwrap();
            for i in 0..4 {
                full.on_tab_upsert(&BrowserTab { window_id: sid as i64, ..tab(base + i, &format!("https://s{sid}-{i}.com/")) }, 1.0);
            }
            // Two of each session's tabs closed (kept as saved tabs).
            let refs: Vec<EntryRef> = [base, base + 1].iter().map(|t| full.locate_tab(*t).unwrap()).collect();
            full.close_entries(&refs, 1.0);
        }
        full.set_active(a).unwrap();
        let mut lazy = full.clone();
        let (mut db_full, mut db_lazy) = (Db::default(), Db::default());
        db_full.save(&mut full, true);
        db_lazy.save(&mut lazy, true);
        evict_inactive(&mut lazy);
        assert_eq!(lazy.session(b).unwrap().entries.len(), 2, "only B's open tabs in memory");
        assert_eq!(lazy.session(b).unwrap().tab_count(), 4, "but it knows how many it has");

        // An operation, and the sessions it needs whole (read back from the database first).
        let mut step = |what: &str, needs: &[Id], op: &dyn Fn(&mut AppState)| {
            for sid in needs {
                load_session(&mut lazy, *sid, &db_lazy.records());
            }
            op(&mut full);
            op(&mut lazy);
            db_full.save(&mut full, false);
            db_lazy.save(&mut lazy, false);
            evict_inactive(&mut lazy);
            let mut want: Vec<_> = db_full.records();
            let mut got: Vec<_> = db_lazy.records();
            want.sort_by_key(|r| r.id);
            got.sort_by_key(|r| r.id);
            assert_eq!(got, want, "{what}: the database");
            for s in &full.sessions {
                let l = lazy.session(s.id).unwrap();
                assert_eq!(l.tab_count(), s.entries.len(), "{what}: tab count of {}", s.id);
                assert_eq!(l.open_count(), s.open_count(), "{what}: open count of {}", s.id);
                assert!(l.id == lazy.active_session || l.entries.iter().all(|e| e.is_open()), "{what}: {} keeps only open tabs", s.id);
            }
            // After a restart, the background reads only what it keeps.
            let restarted = db_lazy.load_partial();
            for s in &full.sessions {
                let r = restarted.session(s.id).unwrap();
                assert_eq!(r.tab_count(), s.entries.len(), "{what}: count after a restart");
                let open: Vec<_> = s.entries.iter().filter(|e| e.is_open()).collect();
                if s.id != restarted.active_session {
                    assert_eq!(r.entries.iter().collect::<Vec<_>>(), open, "{what}: open tabs after a restart");
                }
            }
        };

        // Browser events on an inactive session's open tabs need nothing read.
        step("navigation in an inactive session", &[], &|s| {
            s.on_tab_upsert(&BrowserTab { window_id: b as i64, title: "moved on".into(), ..tab(12, "https://elsewhere.com/") }, 2.0);
        });
        step("tab closed by hand in an inactive session", &[], &|s| {
            s.on_tab_removed(13, false, 3.0);
        });
        step("its window closes", &[], &|s| {
            s.on_tab_upsert(&BrowserTab { window_id: 77, ..tab(30, "https://a-extra.com/") }, 3.0);
            s.on_tab_removed(22, true, 4.0);
            s.on_tab_removed(23, true, 4.0);
        });
        step("tagging a saved tab of an inactive session", &[b], &|s| {
            let r = EntryRef { session: b, entry: s.session(b).unwrap().entries[0].id };
            s.tag_entries(&[r], "kept", true);
        });
        step("removing saved tabs of an inactive session", &[b], &|s| {
            let r = EntryRef { session: b, entry: s.session(b).unwrap().entries[1].id };
            s.remove_entries(&[r], true);
        });
        step("moving a tab into an inactive session", &[b], &|s| {
            let r = s.locate_tab(3).unwrap();
            s.move_entries(&[r], b).unwrap();
        });
        step("switching the active session", &[b], &|s| {
            s.set_active(b).unwrap();
            s.on_tab_upsert(&tab(40, "https://in-b.com/"), 5.0);
        });
        step("a session deleted", &[], &|s| {
            s.delete_session(c).unwrap();
        });
        step("rules over every tab", &[a, b], &|s| {
            s.save_workspace(Workspace {
                name: "W".into(),
                auto_add: true,
                rules: vec![WorkspaceRule::Tag("kept".into())],
                ..Default::default()
            })
            .unwrap();
            s.auto_place(None, false);
        });
        step("restoring a snapshot", &[a], &|s| {
            let snap = s.create_snapshot(a, "x", false, 6.0).unwrap();
            s.restore_snapshot(a, snap, RestoreMode::Replace, 6.0).unwrap();
        });
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
        let mut loaded = db.load();
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
        records.push(TabRecord { id: 900, s: s.active_session, o: 5, a: 0, j: "{not json".into() });
        records.push(TabRecord { id: 901, s: 424242, o: 0, a: 0, j: "{}".into() });
        let back = assemble(&db.header, &records).unwrap();
        assert_eq!(back.active().entries.len(), 1);
        let part = assemble_partial(&db.header, &records).unwrap();
        assert_eq!(part.active().entries.len(), 1);
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
    fn snapshots_are_written_once_and_their_tabs_let_go() {
        let mut s = AppState::default();
        s.on_tab_upsert(&tab(1, "https://a.com/"), 1.0);
        s.on_tab_upsert(&tab(2, "https://b.com/"), 1.0);
        let sid = s.active_session;
        let mut saved = HashSet::new();
        let first = s.create_snapshot(sid, "one", false, 2.0).unwrap();
        let mut plan = WritePlan::default();
        plan_snapshots(&s, &mut saved, &mut plan);
        assert_eq!(plan.snap_puts.len(), 1);
        let back = read_snapshots(&plan.snap_puts);
        assert_eq!(back[0].1.entries.len(), 2, "written with its tabs");
        assert_eq!(back[0].1.tabs, 2);
        strip_snapshots(&mut s, &saved);
        let meta = s.active().snapshot(first).unwrap();
        assert!(meta.entries.is_empty() && meta.tabs == 2 && !meta.loaded());
        // The state holds what describes it, not its tabs.
        let header = header_json(&mut s).unwrap();
        assert!(header.contains("\"tabs\":2") && !header.contains("https://"));

        // Nothing new: nothing written. Automatic snapshots compare fingerprints, not tabs.
        let mut plan = WritePlan::default();
        plan_snapshots(&s, &mut saved, &mut plan);
        assert!(plan.snap_puts.is_empty() && plan.snap_dels.is_empty());
        assert_eq!(s.auto_snapshot_all(3.0), 0, "same URLs as the last snapshot (not loaded)");
        s.on_tab_upsert(&tab(3, "https://c.com/"), 3.0);
        assert_eq!(s.auto_snapshot_all(4.0), 1);

        // A restore needs the tabs back.
        assert!(s.restore_snapshot(sid, first, crate::state::RestoreMode::Merge, 5.0).is_err());
        load_snapshots(
            &mut s,
            &back.iter().map(|(sid, x)| SnapRecord { id: x.id, s: *sid, j: serde_json::to_string(x).unwrap() }).collect::<Vec<_>>(),
        );
        assert!(s.active().snapshot(first).unwrap().loaded());
        assert!(s.restore_snapshot(sid, first, crate::state::RestoreMode::Merge, 5.0).is_ok());

        // Deleted: deleted from the database.
        s.delete_snapshot(sid, first).unwrap();
        let mut plan = WritePlan::default();
        plan_snapshots(&s, &mut saved, &mut plan);
        assert_eq!(plan.snap_dels, [first]);
        assert_eq!(plan.snap_puts.len(), 1, "the automatic one");
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
