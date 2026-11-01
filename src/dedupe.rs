//! Duplicate tab detection and removal.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{AppState, BrowserTab, EntryRef, Id, TabEntry, TabId};
use crate::state::trackable;

/// Which URL differences are ignored when comparing tabs.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct DedupeOptions {
    /// `page#a` == `page#b`
    pub ignore_fragment: bool,
    /// `page?a=1` == `page?a=2`
    pub ignore_query: bool,
    /// `example.com/docs/` == `example.com/docs`
    pub ignore_trailing_slash: bool,
    /// `www.example.com` == `example.com`
    pub ignore_www: bool,
    /// `http://` == `https://`
    pub ignore_scheme: bool,
}

impl Default for DedupeOptions {
    fn default() -> Self {
        Self { ignore_fragment: true, ignore_query: false, ignore_trailing_slash: true, ignore_www: false, ignore_scheme: false }
    }
}

/// Comparison key of a URL under `o`. Scheme and host are case-insensitive.
pub fn key(url: &str, o: &DedupeOptions) -> String {
    let mut u = url.trim();
    if o.ignore_fragment
        && let Some(i) = u.find('#')
    {
        u = &u[..i];
    }
    if o.ignore_query
        && let Some(i) = u.find('?')
    {
        u = &u[..i];
    }
    let Some(sep) = u.find("://") else { return u.to_string() };
    let scheme = u[..sep].to_ascii_lowercase();
    let rest = &u[sep + 3..];
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let mut authority = rest[..auth_end].to_ascii_lowercase();
    let mut tail = rest[auth_end..].to_string();
    if o.ignore_www && authority.starts_with("www.") {
        authority.drain(..4);
    }
    if o.ignore_trailing_slash {
        // Only the path part: keep `?query` / `#fragment` as they are.
        let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
        let (path, suffix) = tail.split_at(path_end);
        tail = format!("{}{suffix}", path.trim_end_matches('/'));
    }
    let scheme = match scheme.as_str() {
        "http" | "https" if o.ignore_scheme => "http(s)".to_string(),
        _ => scheme,
    };
    format!("{scheme}://{authority}{tail}")
}

/// Where to look for duplicates.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Session(Id),
    /// Across every session.
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateGroup {
    pub url: String,
    pub keep: EntryRef,
    pub remove: Vec<EntryRef>,
}

/// Groups of entries that point to the same page. The kept entry is the open one,
/// then the one in the active session, then the most recently used, then the oldest.
pub fn find_duplicates(state: &AppState, scope: Scope, o: &DedupeOptions) -> Vec<DuplicateGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<(EntryRef, &TabEntry)>> = HashMap::new();
    for s in &state.sessions {
        if let Scope::Session(id) = scope
            && s.id != id
        {
            continue;
        }
        for e in &s.entries {
            let k = key(&e.url, o);
            let list = groups.entry(k.clone()).or_default();
            if list.is_empty() {
                order.push(k);
            }
            list.push((EntryRef { session: s.id, entry: e.id }, e));
        }
    }
    let rank = |(r, e): &(EntryRef, &TabEntry)| {
        (e.is_open(), r.session == state.active_session, ordered(e.last_accessed), std::cmp::Reverse(e.id))
    };
    order
        .into_iter()
        .filter_map(|k| {
            let mut members = groups.remove(&k)?;
            if members.len() < 2 {
                return None;
            }
            members.sort_by_key(|m| std::cmp::Reverse(rank(m)));
            let (keep, kept) = members[0];
            Some(DuplicateGroup { url: kept.url.clone(), keep, remove: members[1..].iter().map(|(r, _)| *r).collect() })
        })
        .collect()
}

/// Ids of every entry that has a duplicate (kept ones included), for `is:duplicate`.
pub fn duplicate_ids(state: &AppState, scope: Scope, o: &DedupeOptions) -> HashSet<Id> {
    find_duplicates(state, scope, o)
        .into_iter()
        .flat_map(|g| std::iter::once(g.keep.entry).chain(g.remove.into_iter().map(|r| r.entry)))
        .collect()
}

/// f64 that can be sorted (timestamps are never NaN).
fn ordered(x: f64) -> i64 {
    x as i64
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DedupeResult {
    pub removed: usize,
    /// Open tabs of removed entries (to close, or to leave open untracked).
    pub open_tabs: Vec<TabId>,
}

impl AppState {
    /// Removes duplicate entries, merging their tags (and group, if the kept entry has
    /// none) into the kept entry.
    pub fn remove_duplicates(&mut self, scope: Scope, o: &DedupeOptions) -> DedupeResult {
        let groups = find_duplicates(self, scope, o);
        let mut result = DedupeResult::default();
        for g in groups {
            let removed: Vec<TabEntry> = g.remove.iter().filter_map(|r| self.entry(*r).cloned()).collect();
            if let Some(keep) = self.entry_mut(g.keep) {
                for e in &removed {
                    keep.tags.extend(e.tags.iter().cloned());
                    if keep.group.is_none() {
                        keep.group = e.group.clone();
                    }
                    keep.last_accessed = keep.last_accessed.max(e.last_accessed);
                }
            }
            result.removed += removed.len();
            result.open_tabs.extend(removed.iter().filter_map(|e| e.tab_id));
            for r in &g.remove {
                if let Some(s) = self.session_mut(r.session) {
                    s.entries.retain(|e| e.id != r.entry);
                }
            }
        }
        result
    }

    /// Removes entries added since `since` that show the same page as an older open entry
    /// in any session: extra copies of a page the browser restored at startup. Saved
    /// entries are never touched, so a page kept in several sessions stays in all of them.
    pub fn remove_restored_copies(&mut self, since: f64, o: &DedupeOptions) -> DedupeResult {
        let older: HashSet<String> =
            self.sessions.iter().flat_map(|s| &s.entries).filter(|e| e.is_open() && e.created_at < since).map(|e| key(&e.url, o)).collect();
        let mut result = DedupeResult::default();
        for s in &mut self.sessions {
            s.entries.retain(|e| {
                let copy = e.is_open() && e.created_at >= since && older.contains(&key(&e.url, o));
                if copy {
                    result.removed += 1;
                    result.open_tabs.extend(e.tab_id);
                }
                !copy
            });
        }
        result
    }

    /// For "prevent duplicates": an existing entry in the active session that a new,
    /// not yet tracked tab duplicates.
    pub fn duplicate_of_new_tab(&self, tab: &BrowserTab) -> Option<EntryRef> {
        if self.locate_tab(tab.id).is_some()
            || self.ignored_tab_ids.contains(&tab.id)
            || !trackable(&tab.url, self.settings.ignore_internal_urls)
        {
            return None;
        }
        let o = &self.settings.dedupe;
        let k = key(&tab.url, o);
        let s = self.session(self.active_session)?;
        // Prefer an open entry (switch to it), otherwise an unloaded one (link to it).
        let found = s.entries.iter().filter(|e| key(&e.url, o) == k).max_by_key(|e| (e.is_open(), ordered(e.last_accessed)))?;
        Some(EntryRef { session: s.id, entry: found.id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: Id, url: &str) -> TabEntry {
        TabEntry { id, url: url.into(), ..Default::default() }
    }

    #[test]
    fn keys() {
        let d = DedupeOptions::default();
        assert_eq!(key("HTTPS://Example.com/Docs/#intro", &d), "https://example.com/Docs");
        assert_eq!(key("https://example.com/", &d), key("https://example.com", &d));
        assert_ne!(key("https://example.com/?a=1", &d), key("https://example.com/?a=2", &d));
        assert_eq!(key("https://example.com/x/?a=1", &d), "https://example.com/x?a=1");
        let loose = DedupeOptions { ignore_query: true, ignore_www: true, ignore_scheme: true, ..d };
        assert_eq!(key("http://www.example.com/x?a=1#f", &loose), key("https://example.com/x/", &loose));
        let strict = DedupeOptions { ignore_fragment: false, ignore_trailing_slash: false, ..d };
        assert_ne!(key("https://a.com/x#1", &strict), key("https://a.com/x#2", &strict));
        assert_ne!(key("https://a.com/x/", &strict), key("https://a.com/x", &strict));
        assert_eq!(key("about:blank", &d), "about:blank");
    }

    fn state() -> AppState {
        let mut s = AppState::default();
        let sid = s.active_session;
        let entries = vec![
            e(10, "https://a.com/page"),
            TabEntry { tab_id: Some(5), tags: ["x".to_string()].into(), ..e(11, "https://a.com/page#top") },
            TabEntry { group: Some("G".into()), tags: ["y".to_string()].into(), ..e(12, "https://a.com/page/") },
            e(13, "https://b.com"),
            TabEntry { tab_id: Some(6), ..e(14, "https://c.com") },
            TabEntry { tab_id: Some(7), last_accessed: 9.0, ..e(15, "https://c.com") },
        ];
        s.session_mut(sid).unwrap().entries = entries;
        let other = s.create_session("Other", 0.0);
        s.session_mut(other).unwrap().entries.push(e(20, "https://b.com"));
        s
    }

    #[test]
    fn find_and_remove() {
        let mut s = state();
        let sid = s.active_session;
        let groups = find_duplicates(&s, Scope::Session(sid), &DedupeOptions::default());
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].keep.entry, 11, "open entry is kept");
        assert_eq!(groups[1].keep.entry, 15, "most recently used open entry is kept");
        assert_eq!(duplicate_ids(&s, Scope::Session(sid), &DedupeOptions::default()), [10, 11, 12, 14, 15].into());

        let r = s.remove_duplicates(Scope::Session(sid), &DedupeOptions::default());
        assert_eq!(r, DedupeResult { removed: 3, open_tabs: vec![6] });
        let kept = s.active().entry(11).unwrap();
        assert_eq!(kept.tags, ["x".to_string(), "y".to_string()].into(), "tags merged");
        assert_eq!(kept.group.as_deref(), Some("G"), "group adopted");
        assert_eq!(s.active().entries.len(), 3);
        assert_eq!(s.sessions[1].entries.len(), 1, "other sessions untouched");
    }

    #[test]
    fn across_sessions_prefers_active_session() {
        let mut s = state();
        let groups = find_duplicates(&s, Scope::All, &DedupeOptions::default());
        let b = groups.iter().find(|g| g.url == "https://b.com").unwrap();
        assert_eq!(b.keep.entry, 13);
        s.remove_duplicates(Scope::All, &DedupeOptions::default());
        assert!(s.sessions[1].entries.is_empty());
    }

    #[test]
    fn restored_copies_across_sessions() {
        let mut s = state();
        let other = s.sessions[1].id;
        let open = |id, url: &str, tab, created_at| TabEntry { tab_id: Some(tab), created_at, ..e(id, url) };
        // b.com: saved unloaded in both sessions (kept); a restored copy of the open c.com
        // tab and of the saved, unloaded b.com in the other session.
        s.session_mut(other).unwrap().entries.push(open(21, "https://c.com/", 30, 10.0));
        s.session_mut(other).unwrap().entries.push(open(22, "https://b.com", 31, 10.0));
        let r = s.remove_restored_copies(10.0, &DedupeOptions::default());
        assert_eq!(r, DedupeResult { removed: 1, open_tabs: vec![30] });
        let ids: Vec<Id> = s.sessions[1].entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![20, 22], "only the copy of an open page is removed");
        assert_eq!(s.active().entries.len(), 6);
    }

    #[test]
    fn prevent_duplicate_new_tab() {
        let s = state();
        let tab = |id, url: &str| BrowserTab { id, url: url.into(), ..Default::default() };
        // matches the open, most recent c.com entry
        assert_eq!(s.duplicate_of_new_tab(&tab(99, "https://c.com/")).map(|r| r.entry), Some(15));
        // unloaded match
        assert_eq!(s.duplicate_of_new_tab(&tab(99, "https://b.com")).map(|r| r.entry), Some(13));
        // already tracked tab / new URL / internal page
        assert_eq!(s.duplicate_of_new_tab(&tab(5, "https://c.com")), None);
        assert_eq!(s.duplicate_of_new_tab(&tab(99, "https://new.com")), None);
        assert_eq!(s.duplicate_of_new_tab(&tab(99, "about:newtab")), None);
    }
}
