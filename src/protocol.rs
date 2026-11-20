//! Messages exchanged between the UI pages (popup / manager) and the background script.
//! Serialized as JSON strings over `runtime.sendMessage`.

use serde::{Deserialize, Serialize};

use crate::cleanup::{CleanupReport, CleanupRule, CleanupSpec, DataTypes};
use crate::dedupe::Scope;
use crate::guards::BlockRule;
use crate::hibernate::HibernateTarget;
use crate::model::{AppState, AutoRule, EntryRef, Id, Session, Settings, TabId};
use crate::organize::OrganizeKey;
use crate::state::{ImportTarget, RestoreMode};
use crate::workspace::Workspace;

/// Storage key holding the serialized [`AppState`].
pub const STATE_KEY: &str = "sm_state";
/// The sessions' snapshots (session id -> snapshots), kept apart from the state: they are
/// large and rarely change, so ordinary saves (and the state sent to pages) leave them out.
pub const SNAPSHOTS_KEY: &str = "sm_snapshots";
/// Set to the revision after each save (to the database or storage.local): pages then ask
/// for what changed in the sessions they show (GetState with `known`).
pub const CHANGED_KEY: &str = "sm_changed";
/// A development build kept each session's tabs under `sm_session_<id>` (a [`SessionFile`]),
/// with STATE_KEY's `version` = [`SPLIT_VERSION`]. Read once, to migrate.
pub const SESSION_PREFIX: &str = "sm_session_";
/// The state's `version` when its sessions' tabs were kept apart under SESSION_PREFIX.
pub const SPLIT_VERSION: u32 = 2;

pub fn session_key(id: Id) -> String {
    format!("{SESSION_PREFIX}{id}")
}

/// What a session key held (development build; migrated).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct SessionFile {
    /// The state revision it was saved with.
    pub revision: u64,
    pub entries: Vec<crate::model::TabEntry>,
}

/// Open tabs a page wants from sessions it doesn't load whole.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OpenTabs {
    #[default]
    None,
    /// Every open tab.
    All,
    /// The open tabs of one window (the sidebar).
    Window(TabId),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "cmd")]
pub enum Command {
    /// The state. Pages ask for only what they show: the tabs of `sessions` (`None`: every
    /// session), except those whose `tabs_revision` is the one in `known` (they're sent
    /// `unchanged`, without tabs), and from the other sessions only the open tabs
    /// `open_tabs` says. Every session comes with its tab `counts`.
    GetState {
        #[serde(default)]
        sessions: Option<Vec<Id>>,
        #[serde(default)]
        open_tabs: OpenTabs,
        /// Also the active session's tabs (a page starting doesn't know which it is).
        #[serde(default)]
        with_active: bool,
        /// (session, `tabs_revision`) of the sessions the page has. (Pairs: in a tagged
        /// enum, JSON object keys can't be read as numbers.)
        #[serde(default)]
        known: Vec<(Id, u64)>,
    },
    CreateSession {
        name: String,
        make_active: bool,
    },
    RenameSession {
        id: Id,
        name: String,
    },
    DeleteSession {
        id: Id,
        close_tabs: bool,
    },
    /// Make a session active without opening any of its tabs.
    SetActiveSession {
        id: Id,
    },
    /// Open the session's closed entries. `discarded` opens them unloaded (lazy).
    LoadSession {
        id: Id,
        discarded: bool,
        new_window: bool,
    },
    /// Focus the entry's tab, or open it if closed.
    OpenEntry {
        entry: EntryRef,
        discarded: bool,
    },
    /// Open the closed ones of these entries in the background (`discarded`: unloaded), e.g.
    /// a workspace's saved tabs.
    OpenEntries {
        entries: Vec<EntryRef>,
        discarded: bool,
    },
    /// Focus (or open) the most recently used tab of a session.
    FocusLastTab {
        session: Id,
    },
    /// Reopen the tabs that were open when the session's tabs were last closed (window
    /// closed, browser quit), then start a new generation (see `Session::last_closed`).
    RestoreLastClosed {
        session: Id,
        discarded: bool,
    },
    /// Write AI summaries (descriptions) of these entries' pages (open tabs only).
    Summarize {
        entries: Vec<EntryRef>,
    },
    /// Remove the AI summaries of these entries.
    ClearDescriptions {
        entries: Vec<EntryRef>,
    },
    /// Find tabs by meaning: vector search over titles, sites and AI summaries. `sessions`:
    /// the sessions searched (`None`: all).
    VectorSearch {
        query: String,
        sessions: Option<Vec<Id>>,
        limit: u32,
    },
    /// Free the vector indexes of the sessions that aren't active from memory.
    FreeSearchMemory,
    RemoveEntries {
        entries: Vec<EntryRef>,
        close_tabs: bool,
    },
    /// Close the entries' tabs, keeping them as saved tabs in their session and workspace.
    CloseEntries {
        entries: Vec<EntryRef>,
    },
    TagEntries {
        entries: Vec<EntryRef>,
        tag: String,
        add: bool,
    },
    SetGroup {
        entries: Vec<EntryRef>,
        group: Option<String>,
    },
    MoveEntries {
        entries: Vec<EntryRef>,
        target: Id,
    },
    /// Re-order browser tabs by key (optionally creating native tab groups).
    Organize {
        key: OrganizeKey,
        native_groups: bool,
    },
    SaveAutoRule {
        rule: AutoRule,
    },
    DeleteAutoRule {
        id: Id,
    },
    /// Re-apply auto rules to all existing entries.
    ApplyAutoRules,
    UpdateSettings {
        settings: Settings,
    },
    RunCleanup {
        label: String,
        spec: CleanupSpec,
    },
    SaveCleanupRule {
        rule: CleanupRule,
    },
    DeleteCleanupRule {
        id: Id,
    },
    RunCleanupRule {
        id: Id,
    },
    ClearCleanupLog,
    /// Stop a site (a domain, with its subdomains) from saving data, clearing what it has
    /// saved and reloading its tabs; or (`block: false`) let it save again.
    SetNoStorage {
        site: String,
        block: bool,
    },
    /// Remove cookies / site data of one tab's site (`None` = the active tab of the
    /// focused window), optionally reloading the tab afterwards.
    ClearTabData {
        tab: Option<TabId>,
        data_types: DataTypes,
        reload: bool,
    },
    /// Save the session's current tab list as a named revision.
    CreateSnapshot {
        session: Id,
        name: String,
    },
    RestoreSnapshot {
        session: Id,
        snapshot: Id,
        mode: RestoreMode,
        open: bool,
    },
    /// A session's snapshots with their tabs (the state describes them without).
    GetSnapshots {
        session: Id,
    },
    /// Sessions (`None`: all) as a file to save (`Response::file`), with their snapshots if
    /// `include_snapshots`.
    ExportSessions {
        sessions: Option<Vec<Id>>,
        include_snapshots: bool,
        filename: String,
    },
    DeleteSnapshot {
        session: Id,
        snapshot: Id,
    },
    /// Add URLs to a session (or a new one); optionally open them unloaded.
    ImportUrls {
        urls: Vec<String>,
        target: ImportTarget,
        open: bool,
    },
    /// Add sessions from a saved file as new sessions.
    ImportSessions {
        sessions: Vec<Session>,
    },
    /// Remove duplicate tabs (compared with `settings.dedupe`). `close_tabs` closes open
    /// duplicates; otherwise they stay open but untracked.
    RemoveDuplicates {
        scope: Scope,
        close_tabs: bool,
    },
    /// Create (`id == 0`) or update a workspace.
    SaveWorkspace {
        workspace: Workspace,
    },
    /// Delete a workspace; its tabs are kept without a workspace.
    DeleteWorkspace {
        id: Id,
    },
    /// Move a workspace up (`delta < 0`) or down.
    MoveWorkspace {
        id: Id,
        delta: i32,
    },
    /// Put entries in a workspace (`None` = no workspace). Rules leave them there.
    AssignWorkspace {
        entries: Vec<EntryRef>,
        workspace: Option<Id>,
    },
    /// Let the workspace rules place these entries again, even if they were placed by
    /// hand. `None` applies the rules to every entry not placed by hand.
    AutoPlaceEntries {
        entries: Option<Vec<EntryRef>>,
    },
    /// Unload tabs to free memory. `force` includes pinned tabs and tabs playing media.
    Hibernate {
        target: HibernateTarget,
        force: bool,
    },
    /// Stop a tab's streams (pause them and hold back their media data), or resume them.
    StopStream {
        tab: TabId,
        stop: bool,
    },
    /// Stop the streams playing in tabs that aren't the active tab of their window.
    StopBackgroundStreams,
    /// Switch to a workspace (`None`: no current workspace, show every tab): focus its
    /// most recently used tab and, if enabled, hide the other workspaces' tabs.
    SetActiveWorkspace {
        id: Option<Id>,
    },
    /// Open a new tab that joins a workspace (`None`: the current one).
    NewTabInWorkspace {
        workspace: Option<Id>,
    },
    /// Create (`id == 0`) or update a rule blocking ad scripts / ad tabs.
    SaveBlockRule {
        rule: BlockRule,
    },
    DeleteBlockRule {
        id: Id,
    },
    ClearPopupLog,
    /// Open the inspector window for a tab (`None`: the active tab of the focused window).
    OpenInspector {
        tab: Option<TabId>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Response {
    pub ok: bool,
    pub error: Option<String>,
    /// Human readable result ("3 tabs tagged").
    pub info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<AppState>,
    pub report: Option<CleanupReport>,
    pub capabilities: Option<Capabilities>,
    /// The state revision saved when the command finished (replies other than GetState,
    /// which carries the state itself): pages ask for what changed when it's newer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// Snapshots with their tabs ([`Command::GetSnapshots`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<Vec<crate::model::Snapshot>>,
    /// A file to save: (name, contents) ([`Command::ExportSessions`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<(String, String)>,
    /// Results of [`Command::VectorSearch`], best first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hits: Option<Vec<crate::ai::VectorHit>>,
}

impl Response {
    pub fn err(e: impl Into<String>) -> Self {
        Self { ok: false, error: Some(e.into()), ..Default::default() }
    }
}

/// What the current browser supports (reported by the platform layer).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Capabilities {
    pub browser: String,
    pub tab_groups: bool,
    pub per_host_cache: bool,
    /// A sidebar / side panel is available.
    pub sidebar: bool,
    /// Tabs can be hidden (Firefox, with the optional tabHide permission).
    pub hide_tabs: bool,
    /// The tabHide permission was granted.
    pub hide_tabs_allowed: bool,
    /// The window title can get a preface (Firefox).
    pub title_preface: bool,
    pub extension_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let c = Command::OpenEntry { entry: EntryRef { session: 1, entry: 2 }, discarded: true };
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains(r#""cmd":"OpenEntry""#));
        assert_eq!(serde_json::from_str::<Command>(&s).unwrap(), c);
        let state = serde_json::to_string(&AppState::default()).unwrap();
        let back: AppState = serde_json::from_str(&state).unwrap();
        assert_eq!(back, AppState::default());
        // forward compatible: unknown/missing fields are fine
        let partial: AppState = serde_json::from_str(r#"{"sessions":[]}"#).unwrap();
        assert!(partial.sessions.is_empty());
    }
}
