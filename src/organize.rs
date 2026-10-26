//! Grouping keys and the plan for physically re-organizing browser tabs.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::model::{BrowserTab, TabEntry, TabId};
use crate::url;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OrganizeKey {
    #[default]
    Domain,
    Host,
    Group,
    Tag,
}

impl OrganizeKey {
    pub const ALL: [OrganizeKey; 4] = [OrganizeKey::Domain, OrganizeKey::Host, OrganizeKey::Group, OrganizeKey::Tag];

    pub fn label(self) -> &'static str {
        match self {
            OrganizeKey::Domain => "Domain",
            OrganizeKey::Host => "Host",
            OrganizeKey::Group => "Group",
            OrganizeKey::Tag => "Tag",
        }
    }

    /// Grouping key of an entry. `None` means "ungrouped" (sorted last).
    /// For tags the first tag alphabetically is used.
    pub fn key_of(self, e: &TabEntry) -> Option<String> {
        match self {
            OrganizeKey::Domain => Some(e.domain()).filter(|s| !s.is_empty()),
            OrganizeKey::Host => Some(e.host()).filter(|s| !s.is_empty()),
            OrganizeKey::Group => e.group.clone(),
            OrganizeKey::Tag => e.tags.iter().next().cloned(),
        }
    }

    fn key_of_url(self, u: &str) -> Option<String> {
        match self {
            OrganizeKey::Domain => Some(url::registrable_domain(&url::display_host(u))).filter(|s| !s.is_empty()),
            OrganizeKey::Host => Some(url::display_host(u)).filter(|s| !s.is_empty()),
            OrganizeKey::Group | OrganizeKey::Tag => None,
        }
    }
}

/// Groups entries by key for display; ungrouped entries come last under `None`.
pub fn group_entries<'a>(key: OrganizeKey, entries: impl IntoIterator<Item = &'a TabEntry>) -> Vec<(Option<String>, Vec<&'a TabEntry>)> {
    let mut map: BTreeMap<Option<String>, Vec<&TabEntry>> = BTreeMap::new();
    for e in entries {
        map.entry(key.key_of(e)).or_default().push(e);
    }
    let mut out: Vec<_> = map.into_iter().collect();
    // BTreeMap sorts None first; we want it last.
    if out.first().is_some_and(|(k, _)| k.is_none()) {
        let none = out.remove(0);
        out.push(none);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowPlan {
    pub window_id: TabId,
    /// Unpinned tabs in their new order, to be moved starting at `start_index`.
    pub order: Vec<TabId>,
    pub start_index: i64,
    /// Native tab groups to create: (title, tabs). Only keys with 2+ tabs.
    pub groups: Vec<(String, Vec<TabId>)>,
}

/// Computes per-window tab order sorted by `key` (stable within a key, ungrouped last).
/// `entry_for` resolves a browser tab to its session entry (for tags/groups).
pub fn plan<'a>(
    key: OrganizeKey,
    tabs: &[BrowserTab],
    entry_for: impl Fn(TabId) -> Option<&'a TabEntry>,
) -> Vec<WindowPlan> {
    let mut windows: BTreeMap<TabId, Vec<&BrowserTab>> = BTreeMap::new();
    for t in tabs {
        windows.entry(t.window_id).or_default().push(t);
    }
    let mut plans = Vec::new();
    for (window_id, mut wtabs) in windows {
        wtabs.sort_by_key(|t| t.index);
        let pinned = wtabs.iter().filter(|t| t.pinned).count() as i64;
        let mut keyed: Vec<(Option<String>, usize, TabId)> = wtabs
            .iter()
            .filter(|t| !t.pinned)
            .enumerate()
            .map(|(i, t)| {
                let k = match entry_for(t.id) {
                    Some(e) => key.key_of(e),
                    None => key.key_of_url(&t.url),
                };
                (k, i, t.id)
            })
            .collect();
        // Ungrouped (None) last, then by key, stable by original position.
        keyed.sort_by(|a, b| a.0.is_none().cmp(&b.0.is_none()).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));

        let mut buckets: Vec<(String, Vec<TabId>)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for (k, _, id) in &keyed {
            if let Some(k) = k {
                let i = *index.entry(k.clone()).or_insert_with(|| {
                    buckets.push((k.clone(), Vec::new()));
                    buckets.len() - 1
                });
                buckets[i].1.push(*id);
            }
        }
        buckets.retain(|(_, ids)| ids.len() >= 2);

        plans.push(WindowPlan {
            window_id,
            order: keyed.iter().map(|k| k.2).collect(),
            start_index: pinned,
            groups: buckets,
        });
    }
    plans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: TabId, window: TabId, index: i64, url: &str) -> BrowserTab {
        BrowserTab { id, window_id: window, index, url: url.into(), ..Default::default() }
    }

    #[test]
    fn plan_by_domain() {
        let mut tabs = vec![
            tab(1, 1, 0, "https://b.com/1"),
            tab(2, 1, 1, "https://a.com/1"),
            tab(3, 1, 2, "https://c.com"),
            tab(4, 1, 3, "https://www.b.com/2"),
            tab(6, 1, 4, "https://sub.a.com/"),
            tab(5, 2, 0, "https://z.com"),
        ];
        tabs[0].pinned = true;
        let plans = plan(OrganizeKey::Domain, &tabs, |_| None);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].start_index, 1);
        assert_eq!(plans[0].order, vec![2, 6, 4, 3]);
        assert_eq!(plans[0].groups, vec![("a.com".to_string(), vec![2, 6])]);
        assert_eq!(plans[1].order, vec![5]);
    }

    #[test]
    fn plan_by_group_uses_entries() {
        let tabs = vec![tab(1, 1, 0, "https://a.com"), tab(2, 1, 1, "https://b.com"), tab(3, 1, 2, "https://c.com")];
        let e1 = TabEntry { group: Some("Y".into()), ..Default::default() };
        let e3 = TabEntry { group: Some("X".into()), ..Default::default() };
        let e2 = TabEntry { group: Some("Y".into()), ..Default::default() };
        let plans = plan(OrganizeKey::Group, &tabs, |id| match id {
            1 => Some(&e1),
            2 => Some(&e2),
            3 => Some(&e3),
            _ => None,
        });
        assert_eq!(plans[0].order, vec![3, 1, 2]);
        assert_eq!(plans[0].groups, vec![("Y".to_string(), vec![1, 2])]);
    }

    #[test]
    fn grouping_puts_none_last() {
        let a = TabEntry { group: None, ..Default::default() };
        let b = TabEntry { group: Some("G".into()), ..Default::default() };
        let g = group_entries(OrganizeKey::Group, [&a, &b]);
        assert_eq!(g[0].0.as_deref(), Some("G"));
        assert_eq!(g[1].0, None);
    }
}
