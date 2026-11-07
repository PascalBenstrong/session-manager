//! Persistent data model shared by the background logic and the UI.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::cleanup::{CleanupReport, CleanupRule};
use crate::dedupe::DedupeOptions;
use crate::guards::{BlockRule, GuardSettings, PopupEvent};
use crate::media::{MediaSettings, MediaState};
use crate::organize::OrganizeKey;
use crate::pattern::SitePattern;
use crate::url;
use crate::workspace::Workspace;

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
    /// The open tab is hibernated (discarded by the browser: kept, but not loaded).
    pub discarded: bool,
    /// Title of the native browser tab group the open tab is in.
    pub browser_group: Option<String>,
    pub workspace: Option<Id>,
    /// The workspace was chosen by the user; workspace rules leave the entry alone.
    pub workspace_manual: bool,
    /// Last time a browser tab was opened for the entry (ms since epoch; 0 if unknown).
    pub opened_at: f64,
    /// Last time the entry's tab was closed (the entry stays saved).
    pub closed_at: f64,
    /// Last time the page became loaded: opened loaded, or woken from hibernation.
    pub loaded_at: f64,
    /// Last time the page stopped being loaded: hibernated or closed.
    pub unloaded_at: f64,
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

    /// Open, but unloaded by the browser to save memory.
    pub fn is_hibernated(&self) -> bool {
        self.is_open() && self.discarded
    }

    /// Open and not hibernated: the page is in memory.
    pub fn is_loaded(&self) -> bool {
        self.is_open() && !self.discarded
    }

    /// Records when the entry was opened, closed, loaded or unloaded, given whether it was
    /// open and loaded before its tab state changed.
    pub fn stamp(&mut self, was_open: bool, was_loaded: bool, now: f64) {
        match (was_open, self.is_open()) {
            (false, true) => self.opened_at = now,
            (true, false) => self.closed_at = now,
            _ => {}
        }
        match (was_loaded, self.is_loaded()) {
            (false, true) => self.loaded_at = now,
            (true, false) => self.unloaded_at = now,
            _ => {}
        }
    }

    /// The time `sort` orders by (0 if it never happened or isn't known).
    pub fn time_of(&self, sort: TabSort) -> f64 {
        match sort {
            TabSort::Session => 0.0,
            TabSort::LastUsed => self.last_accessed,
            TabSort::Added => self.created_at,
            TabSort::Opened => self.opened_at,
            TabSort::Closed => self.closed_at,
            TabSort::Loaded => self.loaded_at,
            TabSort::Unloaded => self.unloaded_at,
        }
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
        self.entries.iter().max_by(|a, b| a.last_accessed.total_cmp(&b.last_accessed).then(a.created_at.total_cmp(&b.created_at)))
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
    /// Which tabs may play media while in the background.
    pub media: MediaSettings,
    /// How the tab list looks.
    pub tab_list: TabListSettings,
    /// How workspaces show up in the browser itself.
    pub browser_ui: BrowserUiSettings,
    /// Protections against pages (pop-ups, right-click blocking, developer tools traps).
    pub guards: GuardSettings,
}

/// Workspaces in the browser's own interface (tab strip, window title).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct BrowserUiSettings {
    /// Mirror workspaces as native tab groups (title, color).
    pub tab_groups: bool,
    /// Collapse the groups of workspaces other than the active tab's.
    pub collapse_inactive: bool,
    /// Hide the tabs of other workspaces than the current one (Firefox).
    pub hide_inactive: bool,
    /// Show the current workspace in the window title (Firefox).
    pub show_in_title: bool,
    /// New tabs that no rule places join the current workspace.
    pub new_tabs_join_active: bool,
}

/// How the Tabs view lists tabs.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TabGrouping {
    #[default]
    Workspace,
    None,
    Domain,
    Host,
    Group,
    Tag,
}

impl TabGrouping {
    pub const ALL: [TabGrouping; 6] =
        [TabGrouping::Workspace, TabGrouping::None, TabGrouping::Domain, TabGrouping::Host, TabGrouping::Group, TabGrouping::Tag];

    pub fn label(self) -> &'static str {
        match self {
            TabGrouping::Workspace => "Workspace",
            TabGrouping::None => "None",
            TabGrouping::Domain => "Domain",
            TabGrouping::Host => "Host",
            TabGrouping::Group => "Group",
            TabGrouping::Tag => "Tag",
        }
    }

    pub fn organize_key(self) -> Option<OrganizeKey> {
        match self {
            TabGrouping::Domain => Some(OrganizeKey::Domain),
            TabGrouping::Host => Some(OrganizeKey::Host),
            TabGrouping::Group => Some(OrganizeKey::Group),
            TabGrouping::Tag => Some(OrganizeKey::Tag),
            TabGrouping::Workspace | TabGrouping::None => None,
        }
    }
}

/// The order of tabs within each section of the Tabs view.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TabSort {
    /// The order the session keeps them in.
    #[default]
    Session,
    LastUsed,
    /// When the tab was added to the session (first opened).
    Added,
    Opened,
    Closed,
    Loaded,
    Unloaded,
}

impl TabSort {
    pub const ALL: [TabSort; 7] =
        [TabSort::Session, TabSort::LastUsed, TabSort::Added, TabSort::Opened, TabSort::Closed, TabSort::Loaded, TabSort::Unloaded];

    pub fn label(self) -> &'static str {
        match self {
            TabSort::Session => "Session order",
            TabSort::LastUsed => "Last used",
            TabSort::Added => "First opened",
            TabSort::Opened => "Last opened",
            TabSort::Closed => "Last closed",
            TabSort::Loaded => "Last loaded",
            TabSort::Unloaded => "Last unloaded",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            TabSort::Session => "The order tabs were added in",
            TabSort::LastUsed => "When you last switched to the tab",
            TabSort::Added => "When the tab was added to the session",
            TabSort::Opened => "When a browser tab was last opened for it",
            TabSort::Closed => "When its tab was last closed (it stays saved)",
            TabSort::Loaded => "When the page was last loaded: opened, or woken from hibernation",
            TabSort::Unloaded => "When the page was last unloaded: hibernated or closed",
        }
    }

    /// The word shown on rows before the time, e.g. "opened 3 h ago".
    pub fn verb(self) -> &'static str {
        match self {
            TabSort::Session => "",
            TabSort::LastUsed => "used",
            TabSort::Added => "added",
            TabSort::Opened => "opened",
            TabSort::Closed => "closed",
            TabSort::Loaded => "loaded",
            TabSort::Unloaded => "unloaded",
        }
    }
}

/// Orders entries by `sort`, newest first unless `oldest_first`. Entries without that time
/// come last either way, in their previous order (the sort is stable).
pub fn sort_entries<'a, T>(items: &mut [T], sort: TabSort, oldest_first: bool, entry: impl Fn(&T) -> &'a TabEntry) {
    if sort == TabSort::Session {
        return;
    }
    items.sort_by(|a, b| {
        let (ta, tb) = (entry(a).time_of(sort), entry(b).time_of(sort));
        (ta <= 0.0).cmp(&(tb <= 0.0)).then_with(|| if oldest_first { ta.total_cmp(&tb) } else { tb.total_cmp(&ta) })
    });
}

/// How long ago `ms` milliseconds is, roughly: "just now", "5 min ago", "3 h ago", "2 d ago"...
pub fn time_ago(ms: f64) -> String {
    const MIN: f64 = 60_000.0;
    let (n, unit) = match ms.max(0.0) {
        m if m < MIN => return "just now".into(),
        m if m < 60.0 * MIN => (m / MIN, "min"),
        m if m < 24.0 * 60.0 * MIN => (m / (60.0 * MIN), "h"),
        m if m < 30.0 * 24.0 * 60.0 * MIN => (m / (24.0 * 60.0 * MIN), "d"),
        m if m < 365.0 * 24.0 * 60.0 * MIN => (m / (30.0 * 24.0 * 60.0 * MIN), "mo"),
        m => (m / (365.0 * 24.0 * 60.0 * MIN), "y"),
    };
    format!("{} {unit} ago", n.floor() as u64)
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct TabListSettings {
    pub grouping: TabGrouping,
    /// One-line rows.
    pub compact: bool,
    /// Show tag and group chips on rows.
    pub show_labels: bool,
    /// Order of the tabs within each section.
    pub sort: TabSort,
    /// Oldest first instead of newest first.
    pub oldest_first: bool,
}

impl Default for TabListSettings {
    fn default() -> Self {
        Self { grouping: TabGrouping::Workspace, compact: false, show_labels: true, sort: TabSort::Session, oldest_first: false }
    }
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
            media: MediaSettings::default(),
            tab_list: TabListSettings::default(),
            browser_ui: BrowserUiSettings::default(),
            guards: GuardSettings::default(),
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
    /// Shared by all sessions, in display order.
    pub workspaces: Vec<Workspace>,
    /// Media currently playing in tabs, and tabs we paused (for the media limit).
    pub media: MediaState,
    /// The workspace in use: the workspace of the tab last activated, or the one switched to.
    pub active_workspace: Option<Id>,
    /// Tabs opened for a workspace whose entry doesn't exist yet (a new tab page becomes an
    /// entry once it shows a page).
    pub pending_workspace: BTreeMap<TabId, Id>,
    /// Rules blocking ad scripts and ad tabs.
    pub block_rules: Vec<BlockRule>,
    /// Tabs recently opened by pages (newest first, bounded).
    pub popup_log: Vec<PopupEvent>,
    /// Sites (domains, with their subdomains) not allowed to save data: their pages get
    /// memory-only storage and their cookies are removed as they are set.
    pub no_storage_sites: Vec<String>,
    /// Goes up every time the state is saved. A page gets the state both in replies and
    /// from storage changes, which can arrive out of order: it keeps the newest one.
    pub revision: u64,
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
            workspaces: Vec::new(),
            media: MediaState::default(),
            active_workspace: None,
            pending_workspace: BTreeMap::new(),
            block_rules: Vec::new(),
            popup_log: Vec::new(),
            no_storage_sites: Vec::new(),
            revision: 0,
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
    pub pinned: bool,
    pub active: bool,
    pub discarded: bool,
    /// Playing sound.
    pub audible: bool,
    /// Last time the tab was active (ms since epoch; 0 if the browser doesn't say).
    pub last_accessed: f64,
    /// Title of the native tab group the tab is in.
    pub group_title: Option<String>,
    /// Id of the native tab group the tab is in.
    pub group_id: Option<i64>,
    /// Hidden from the tab strip (Firefox).
    pub hidden: bool,
    /// The tab that opened this one (a link or a script).
    pub opener_tab_id: Option<TabId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_by_time_with_unknown_times_last() {
        let e = |id, opened_at| TabEntry { id, opened_at, ..Default::default() };
        let entries = [e(1, 5.0), e(2, 0.0), e(3, 9.0), e(4, 1.0), e(5, 0.0)];
        let ids = |sort, oldest_first| {
            let mut v: Vec<&TabEntry> = entries.iter().collect();
            sort_entries(&mut v, sort, oldest_first, |e| *e);
            v.iter().map(|e| e.id).collect::<Vec<_>>()
        };
        assert_eq!(ids(TabSort::Opened, false), [3, 1, 4, 2, 5]);
        assert_eq!(ids(TabSort::Opened, true), [4, 1, 3, 2, 5]);
        assert_eq!(ids(TabSort::Session, false), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn rough_time_ago() {
        assert_eq!(time_ago(-5.0), "just now");
        assert_eq!(time_ago(59_000.0), "just now");
        assert_eq!(time_ago(5.0 * 60_000.0), "5 min ago");
        assert_eq!(time_ago(3.5 * 3_600_000.0), "3 h ago");
        assert_eq!(time_ago(2.0 * 86_400_000.0), "2 d ago");
        assert_eq!(time_ago(65.0 * 86_400_000.0), "2 mo ago");
        assert_eq!(time_ago(800.0 * 86_400_000.0), "2 y ago");
    }
}
