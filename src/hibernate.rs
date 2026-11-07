//! Hibernating tabs: asking the browser to unload (discard) open tabs. A hibernated tab
//! keeps its place in the tab strip and reloads when it is selected.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{AppState, BrowserTab, EntryRef, Id, TabId};

/// Which tabs to hibernate.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum HibernateTarget {
    /// Every open browser tab, tracked or not.
    All,
    Session(Id),
    Workspace(Id),
    Entries(Vec<EntryRef>),
}

impl AppState {
    /// The browser tabs of a target (`None` = every tab).
    pub fn hibernate_tabs(&self, target: &HibernateTarget) -> Option<HashSet<TabId>> {
        let refs = match target {
            HibernateTarget::All => return None,
            HibernateTarget::Session(id) => {
                return Some(self.session(*id).map(|s| s.entries.iter().filter_map(|e| e.tab_id).collect()).unwrap_or_default());
            }
            HibernateTarget::Workspace(id) => self.workspace_entries(*id),
            HibernateTarget::Entries(refs) => refs.clone(),
        };
        Some(refs.iter().filter_map(|r| self.entry(*r)?.tab_id).collect())
    }

    /// Marks tabs as hibernated right away (the browser's update events follow).
    pub fn mark_discarded(&mut self, ids: &[TabId], now: f64) -> bool {
        let ids: HashSet<TabId> = ids.iter().copied().collect();
        let mut changed = false;
        for e in self.sessions.iter_mut().flat_map(|s| s.entries.iter_mut()) {
            if e.tab_id.is_some_and(|t| ids.contains(&t)) && !e.discarded {
                e.discarded = true;
                e.unloaded_at = now;
                changed = true;
            }
        }
        changed
    }
}

/// The tabs that can be hibernated: open, not active and not already unloaded. Unless
/// `force`, pinned tabs and tabs playing media are left alone too.
pub fn candidates(tabs: &[BrowserTab], wanted: Option<&HashSet<TabId>>, force: bool, playing: &BTreeSet<TabId>) -> Vec<TabId> {
    tabs.iter()
        .filter(|t| wanted.is_none_or(|w| w.contains(&t.id)))
        .filter(|t| !t.active && !t.discarded)
        .filter(|t| force || !(t.pinned || t.audible || playing.contains(&t.id)))
        .map(|t| t.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: TabId) -> BrowserTab {
        BrowserTab { id, window_id: 1, url: format!("https://t{id}.com"), ..Default::default() }
    }

    #[test]
    fn picks_inactive_loaded_tabs() {
        let tabs = vec![
            BrowserTab { active: true, ..tab(1) },
            BrowserTab { discarded: true, ..tab(2) },
            BrowserTab { pinned: true, ..tab(3) },
            BrowserTab { audible: true, ..tab(4) },
            tab(5),
            tab(6),
        ];
        let playing: BTreeSet<TabId> = [6].into();
        assert_eq!(candidates(&tabs, None, false, &playing), vec![5]);
        assert_eq!(candidates(&tabs, None, true, &playing), vec![3, 4, 5, 6]);
        let wanted: HashSet<TabId> = [1, 3, 6].into();
        assert_eq!(candidates(&tabs, Some(&wanted), true, &playing), vec![3, 6]);
    }

    #[test]
    fn targets_resolve_to_open_tabs() {
        let mut s = AppState::default();
        s.on_tab_upsert(&tab(1), 1.0);
        s.on_tab_upsert(&tab(2), 1.0);
        s.on_tab_removed(2, true, 1.0);
        let sid = s.active_session;
        assert_eq!(s.hibernate_tabs(&HibernateTarget::Session(sid)), Some([1].into()));
        assert_eq!(s.hibernate_tabs(&HibernateTarget::All), None);
        assert!(s.mark_discarded(&[1], 2.0));
        assert!(s.entry_for_tab(1).unwrap().is_hibernated());
        assert!(!s.mark_discarded(&[1], 2.0));
    }
}
