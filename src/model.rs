//! Persistent data model shared by the background logic and the UI.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::cleanup::{CleanupReport, CleanupRule};
use crate::dedupe::DedupeOptions;
use crate::organize::OrganizeKey;
use crate::pattern::SitePattern;
use crate::url;

pub type Id = u64;
/// Browser-assigned tab / window id.
pub type TabId = i64;

/// A tab remembered by a session. It may be open (`tab_id` is set) or unloaded.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct TabEntry {
    pub id: Id,
    pub url: String,
    pub title: String,
    pub fav_icon_url: Option<String>,
    pub tags: BTreeSet<String>,
    pub group: Option<String>,
    pub pinned: bool,
    /// The live browser tab backing this entry, if it is currently open.
    pub tab_id: Option<TabId>,
    pub window_id: Option<TabId>,
    pub created_at: f64,
    /// Last time the tab was activated (ms since epoch). Used for "find last open tab".
    pub last_accessed: f64,
}

impl TabEntry {
    pub fn host(&self) -> String {
        url::display_host(&self.url)
    }

    pub fn domain(&self) -> String {
        url::registrable_domain(&self.host())
    }

    pub fn is_open(&self) -> bool {
        self.tab_id.is_some()
    }

    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() { &self.url } else { &self.title }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Session {
    pub id: Id,
    pub name: String,
    pub created_at: f64,
    pub entries: Vec<TabEntry>,
    /// Saved revisions of `entries`, oldest first.
    pub snapshots: Vec<Snapshot>,
}

/// A saved revision of a session's tab list.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Snapshot {
    pub id: Id,
    pub name: String,
    pub created_at: f64,
    /// Taken automatically (interval / before a restore). Only these are pruned.
    pub auto: bool,
    /// Copies of the entries (never linked to live tabs).
    pub entries: Vec<TabEntry>,
}

impl Snapshot {
    pub fn urls(&self) -> BTreeSet<&str> {
        self.entries.iter().map(|e| e.url.as_str()).collect()
    }
}

impl Session {
    pub fn entry(&self, id: Id) -> Option<&TabEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn entry_mut(&mut self, id: Id) -> Option<&mut TabEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    pub fn open_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_open()).count()
    }

    /// The most recently accessed entry ("last open tab").
    pub fn last_accessed(&self) -> Option<&TabEntry> {
        self.entries
            .iter()
            .max_by(|a, b| a.last_accessed.total_cmp(&b.last_accessed).then(a.created_at.total_cmp(&b.created_at)))
    }

    pub fn urls(&self) -> BTreeSet<&str> {
        self.entries.iter().map(|e| e.url.as_str()).collect()
    }

    pub fn snapshot(&self, id: Id) -> Option<&Snapshot> {
        self.snapshots.iter().find(|s| s.id == id)
    }

    pub fn groups(&self) -> BTreeSet<String> {
        self.entries.iter().filter_map(|e| e.group.clone()).collect()
    }

    pub fn tags(&self) -> BTreeSet<String> {
        self.entries.iter().flat_map(|e| e.tags.iter().cloned()).collect()
    }
}

/// Automatically applies tags / a group to tabs whose URL matches a pattern.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct AutoRule {
    pub id: Id,
    pub enabled: bool,
    pub pattern: SitePattern,
    pub tags: BTreeSet<String>,
    /// Only applied when the tab has no group yet.
    pub group: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Add newly opened / navigated tabs to the active session.
    pub auto_add_tabs: bool,
    /// Only track http(s)/file/ftp pages (skip about:, chrome://, extension pages...).
    pub ignore_internal_urls: bool,
    /// When a whole window closes (or the browser shuts down), keep its tabs as unloaded
    /// entries instead of removing them from the session.
    pub keep_tabs_on_window_close: bool,
    /// Re-sort browser tabs automatically whenever tracked tabs change.
    pub auto_organize: Option<OrganizeKey>,
    /// Use native browser tab groups when organizing (if supported).
    pub native_tab_groups: bool,
    /// Snapshot every changed session every N minutes.
    pub auto_snapshot_minutes: Option<u32>,
    /// Automatic snapshots kept per session (manual snapshots are never pruned).
    pub max_auto_snapshots: u32,
    /// How URLs are compared when looking for duplicate tabs.
    pub dedupe: DedupeOptions,
    /// A newly opened tab whose page is already in the active session is merged into it:
    /// switch to the existing open tab (closing the new one), or link to the unloaded entry.
    pub prevent_duplicates: bool,
    /// After the browser starts and restores its tabs, remove duplicate tabs in every
    /// session (closing extra copies).
    pub dedupe_on_startup: bool,
    /// Before loading a session's tabs, remove its duplicates so only one copy is opened.
    pub dedupe_on_session_load: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_add_tabs: true,
            ignore_internal_urls: true,
            keep_tabs_on_window_close: true,
            auto_organize: None,
            native_tab_groups: false,
            auto_snapshot_minutes: None,
            max_auto_snapshots: 20,
            dedupe: DedupeOptions::default(),
            prevent_duplicates: false,
            dedupe_on_startup: false,
            dedupe_on_session_load: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AppState {
    pub version: u32,
    pub sessions: Vec<Session>,
    pub active_session: Id,
    pub next_id: Id,
    pub settings: Settings,
    pub auto_rules: Vec<AutoRule>,
    pub cleanup_rules: Vec<CleanupRule>,
    /// Open tabs the user explicitly removed from their session; never auto-added again.
    pub ignored_tab_ids: BTreeSet<TabId>,
    /// Most recent cleanup results (newest first, bounded).
    pub cleanup_log: Vec<CleanupReport>,
}

impl Default for AppState {
    fn default() -> Self {
        let mut s = Self {
            version: 1,
            sessions: Vec::new(),
            active_session: 0,
            next_id: 1,
            settings: Settings::default(),
            auto_rules: Vec::new(),
            cleanup_rules: Vec::new(),
            ignored_tab_ids: BTreeSet::new(),
            cleanup_log: Vec::new(),
        };
        let id = s.alloc_id();
        s.sessions.push(Session { id, name: "Default".into(), ..Default::default() });
        s.active_session = id;
        s
    }
}

impl AppState {
    pub fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn session(&self, id: Id) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn session_mut(&mut self, id: Id) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn active(&self) -> &Session {
        self.session(self.active_session).unwrap_or(&self.sessions[0])
    }

    /// All groups/tags used anywhere, for UI pickers.
    pub fn all_groups(&self) -> BTreeSet<String> {
        self.sessions.iter().flat_map(|s| s.groups()).collect()
    }

    pub fn all_tags(&self) -> BTreeSet<String> {
        self.sessions.iter().flat_map(|s| s.tags()).collect()
    }
}

/// Reference to an entry inside a session.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryRef {
    pub session: Id,
    pub entry: Id,
}

/// Snapshot of a live browser tab, as reported by the platform layer.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserTab {
    pub id: TabId,
    pub window_id: TabId,
    pub index: i64,
    pub url: String,
    pub title: String,
    pub fav_icon_url: Option<String>,
    pub pinned: bool,
    pub active: bool,
    pub discarded: bool,
}
