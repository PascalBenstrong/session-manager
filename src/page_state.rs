//! What pages get of the state, and how they keep it: a page loads only the sessions it
//! shows, never every tab.
//!
//! A page asks (`Command::GetState`) for the tabs of the sessions it shows (the session
//! viewed and the active one; every session only while a view needs them all), and from
//! the other sessions only open tabs if it lists those (the sidebar: its window's; the
//! guards view: all). Sessions whose `tabs_revision` the page already has come back
//! `unchanged`, without tabs. Every session carries its tab `counts`, so lists of sessions
//! need no tabs. After each save the background bumps `CHANGED_KEY`; pages then ask again
//! with what they `known`, and get only the sessions that changed.
//!
//! The background builds the reply from the state itself, moving tabs out for the time of
//! serializing (no copy of the tabs it doesn't send).

use std::collections::{BTreeMap, BTreeSet};

use crate::entries::Entries;
use crate::model::{AppState, Id};
use crate::protocol::OpenTabs;

/// What a page asks for (see the module docs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Want {
    /// Sessions whose tabs it loads; `None`: all.
    pub sessions: Option<BTreeSet<Id>>,
    /// And the active session's (whichever it is when the background answers).
    pub with_active: bool,
    pub open_tabs: OpenTabs,
}

/// Runs `f` (serializing) on the state as a page asked for it: the tabs of `sessions` (or
/// all) unless `known` already has their `tabs_revision`, and of the other sessions only the
/// open tabs `open_tabs` asks for. Every session gets its tab counts. The state is put back
/// as it was. O(sessions + tabs sent + open tabs of the other sessions).
pub fn with_page_view<T>(
    state: &mut AppState,
    sessions: Option<&[Id]>,
    open_tabs: OpenTabs,
    known: &BTreeMap<Id, u64>,
    f: impl FnOnce(&AppState) -> T,
) -> T {
    let mut moved: Vec<(usize, Entries)> = Vec::new();
    for (i, s) in state.sessions.iter_mut().enumerate() {
        s.refresh_counts();
        let wanted = sessions.is_none_or(|ids| ids.contains(&s.id));
        if wanted && known.get(&s.id) != Some(&s.tabs_revision) {
            continue; // sent whole
        }
        let instead: Entries = if wanted {
            s.unchanged = true;
            Entries::default()
        } else {
            match open_tabs {
                OpenTabs::None => Entries::default(),
                OpenTabs::All => s.entries.open_tabs().into_iter().cloned().collect(),
                OpenTabs::Window(w) => s.entries.open_tabs().into_iter().filter(|e| e.window_id == Some(w)).cloned().collect(),
            }
        };
        moved.push((i, std::mem::replace(&mut s.entries, instead)));
    }
    let out = f(state);
    for (i, entries) in moved {
        state.sessions[i].entries = entries;
        state.sessions[i].unchanged = false;
    }
    out
}

/// A page puts a reply to its `GetState` together with what it has: sessions it asked for
/// come whole (or `unchanged`: it keeps its tabs), the others are `partial`. Returns `None`
/// if an `unchanged` session isn't one the page has (it then asks for everything again).
pub fn merge(current: Option<&AppState>, mut new: AppState, asked: &Want) -> Option<AppState> {
    let active = new.active_session;
    for s in &mut new.sessions {
        let whole = asked.sessions.as_ref().is_none_or(|ids| ids.contains(&s.id)) || (asked.with_active && s.id == active);
        if !whole {
            s.partial = true;
            continue;
        }
        if s.unchanged {
            let mine = current.and_then(|c| c.session(s.id)).filter(|c| !c.partial && c.tabs_revision == s.tabs_revision)?;
            s.entries = mine.entries.clone();
            s.unchanged = false;
        }
        s.partial = false;
    }
    Some(new)
}

/// The `known` a page sends: the `tabs_revision` of the sessions it has whole.
pub fn known(state: Option<&AppState>) -> BTreeMap<Id, u64> {
    state.map(|s| s.sessions.iter().filter(|x| !x.partial).map(|x| (x.id, x.tabs_revision)).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BrowserTab;

    fn tab(id: i64, window: i64, url: &str) -> BrowserTab {
        BrowserTab { id, window_id: window, url: url.into(), ..Default::default() }
    }

    /// Two sessions: Default (3 tabs, 2 open in windows 1 and 2) and Other (2 tabs, 1 open).
    fn state() -> (AppState, Id, Id) {
        let mut s = AppState::default();
        let a = s.active_session;
        s.on_tab_upsert(&tab(1, 1, "https://a1.com"), 1.0);
        s.on_tab_upsert(&tab(2, 2, "https://a2.com"), 1.0);
        s.on_tab_upsert(&tab(3, 1, "https://a3.com"), 1.0);
        let r = s.locate_tab(3).unwrap();
        s.close_entries(&[r], 1.0);
        let b = s.create_session("Other", 1.0);
        s.set_active(b).unwrap();
        s.on_tab_upsert(&tab(4, 1, "https://b1.com"), 1.0);
        s.on_tab_upsert(&tab(5, 2, "https://b2.com"), 1.0);
        let r = s.locate_tab(5).unwrap();
        s.close_entries(&[r], 1.0);
        s.set_active(a).unwrap();
        (s, a, b)
    }

    fn reply(s: &mut AppState, sessions: Option<&[Id]>, open: OpenTabs, known: &BTreeMap<Id, u64>) -> AppState {
        let json = with_page_view(s, sessions, open, known, |s| serde_json::to_string(s).unwrap());
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn sends_only_what_the_page_shows() {
        let (mut s, a, b) = state();
        let before = s.clone();
        let r = reply(&mut s, Some(&[a]), OpenTabs::None, &BTreeMap::new());
        assert_eq!(r.session(a).unwrap().entries.len(), 3);
        assert!(r.session(b).unwrap().entries.is_empty(), "the other session's tabs aren't sent");
        assert_eq!(r.session(b).unwrap().counts, crate::model::TabCounts { tabs: 2, open: 1 }, "but how many it has is");
        let tabs = |st: &AppState| st.sessions.iter().map(|x| x.entries.to_vec()).collect::<Vec<_>>();
        assert_eq!(tabs(&s), tabs(&before), "the tabs are as they were");
        assert!(s.sessions.iter().all(|x| !x.unchanged));

        // The sidebar: its window's open tabs from the other sessions.
        let r = reply(&mut s, Some(&[a]), OpenTabs::Window(1), &BTreeMap::new());
        let urls: Vec<_> = r.session(b).unwrap().entries.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(urls, ["https://b1.com"]);
        let r = reply(&mut s, Some(&[]), OpenTabs::All, &BTreeMap::new());
        assert_eq!(r.sessions.iter().map(|x| x.entries.len()).sum::<usize>(), 3, "every open tab, nothing else");

        // None: everything (tests, tools).
        let r = reply(&mut s, None, OpenTabs::None, &BTreeMap::new());
        assert_eq!(r.sessions.iter().map(|x| x.entries.len()).sum::<usize>(), 5);
    }

    #[test]
    fn unchanged_sessions_come_back_without_tabs_and_merge() {
        let (mut s, a, b) = state();
        let want = |ids: &[Id]| Want { sessions: Some(ids.iter().copied().collect()), ..Default::default() };
        let asked = want(&[a]);
        let first = merge(None, reply(&mut s, Some(&[a]), OpenTabs::None, &BTreeMap::new()), &asked).unwrap();
        assert!(!first.session(a).unwrap().partial && first.session(b).unwrap().partial);
        assert_eq!(first.session(b).unwrap().tab_count(), 2, "counts stand in for the tabs");
        assert_eq!(first.session(b).unwrap().open_count(), 1);

        // Nothing changed: the tabs aren't sent again; the page keeps its own.
        let k = known(Some(&first));
        assert_eq!(k.keys().copied().collect::<Vec<_>>(), [a], "only sessions it has whole");
        let r = reply(&mut s, Some(&[a]), OpenTabs::None, &k);
        assert!(r.session(a).unwrap().unchanged && r.session(a).unwrap().entries.is_empty());
        let second = merge(Some(&first), r, &asked).unwrap();
        assert_eq!(second.session(a).unwrap().entries, s.session(a).unwrap().entries);

        // A change (saved: a new tabs_revision) is sent.
        s.on_tab_upsert(&tab(6, 1, "https://a6.com"), 2.0);
        crate::persist::forget_changes(&mut s, 9);
        let r = reply(&mut s, Some(&[a]), OpenTabs::None, &known(Some(&second)));
        assert!(!r.session(a).unwrap().unchanged);
        let third = merge(Some(&second), r, &asked).unwrap();
        assert_eq!(third.session(a).unwrap().entries.len(), 4);

        // Viewing the other session loads it; the first one, no longer shown, is let go.
        let asked = want(&[b]);
        let r = reply(&mut s, Some(&[b]), OpenTabs::None, &known(Some(&third)));
        let fourth = merge(Some(&third), r, &asked).unwrap();
        assert_eq!(fourth.session(b).unwrap().entries.len(), 2);
        assert!(fourth.session(a).unwrap().partial && fourth.session(a).unwrap().entries.is_empty());
        assert_eq!(fourth.session(a).unwrap().tab_count(), 4);

        // An `unchanged` session the page doesn't have: ask again for everything.
        let r = reply(&mut s, Some(&[b]), OpenTabs::None, &known(Some(&fourth)));
        assert!(merge(None, r, &asked).is_none());

        // A page starting asks for the active session without knowing which it is.
        let start = Want { sessions: Some(BTreeSet::new()), with_active: true, ..Default::default() };
        let active: Vec<Id> = vec![s.active_session];
        let page = merge(None, reply(&mut s, Some(&active), OpenTabs::None, &BTreeMap::new()), &start).unwrap();
        assert!(!page.active().partial && page.active().entries.len() == 4);
    }
}
