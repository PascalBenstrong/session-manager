//! Messages exchanged between the UI pages (popup / manager) and the background script.
//! Serialized as JSON strings over `runtime.sendMessage`.

use serde::{Deserialize, Serialize};

use crate::cleanup::{CleanupReport, CleanupRule, CleanupSpec, DataTypes};
use crate::dedupe::Scope;
use crate::hibernate::HibernateTarget;
use crate::model::{AppState, AutoRule, EntryRef, Id, Session, Settings, TabId};
use crate::workspace::Workspace;
use crate::state::{ImportTarget, RestoreMode};
use crate::organize::OrganizeKey;

/// Storage key holding the serialized [`AppState`].
pub const STATE_KEY: &str = "sm_state";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "cmd")]
pub enum Command {
    GetState,
    CreateSession { name: String, make_active: bool },
    RenameSession { id: Id, name: String },
    DeleteSession { id: Id, close_tabs: bool },
    /// Make a session active without opening any of its tabs.
    SetActiveSession { id: Id },
    /// Open the session's closed entries. `discarded` opens them unloaded (lazy).
    LoadSession { id: Id, discarded: bool, new_window: bool },
    /// Focus the entry's tab, or open it if closed.
    OpenEntry { entry: EntryRef, discarded: bool },
    /// Focus (or open) the most recently used tab of a session.
    FocusLastTab { session: Id },
    RemoveEntries { entries: Vec<EntryRef>, close_tabs: bool },
    TagEntries { entries: Vec<EntryRef>, tag: String, add: bool },
    SetGroup { entries: Vec<EntryRef>, group: Option<String> },
    MoveEntries { entries: Vec<EntryRef>, target: Id },
    /// Re-order browser tabs by key (optionally creating native tab groups).
    Organize { key: OrganizeKey, native_groups: bool },
    SaveAutoRule { rule: AutoRule },
    DeleteAutoRule { id: Id },
    /// Re-apply auto rules to all existing entries.
    ApplyAutoRules,
    UpdateSettings { settings: Settings },
    RunCleanup { label: String, spec: CleanupSpec },
    SaveCleanupRule { rule: CleanupRule },
    DeleteCleanupRule { id: Id },
    RunCleanupRule { id: Id },
    ClearCleanupLog,
    /// Remove cookies / site data of one tab's site (`None` = the active tab of the
    /// focused window), optionally reloading the tab afterwards.
    ClearTabData { tab: Option<TabId>, data_types: DataTypes, reload: bool },
    /// Save the session's current tab list as a named revision.
    CreateSnapshot { session: Id, name: String },
    RestoreSnapshot { session: Id, snapshot: Id, mode: RestoreMode, open: bool },
    DeleteSnapshot { session: Id, snapshot: Id },
    /// Add URLs to a session (or a new one); optionally open them unloaded.
    ImportUrls { urls: Vec<String>, target: ImportTarget, open: bool },
    /// Add sessions from a saved file as new sessions.
    ImportSessions { sessions: Vec<Session> },
    /// Remove duplicate tabs (compared with `settings.dedupe`). `close_tabs` closes open
    /// duplicates; otherwise they stay open but untracked.
    RemoveDuplicates { scope: Scope, close_tabs: bool },
    /// Create (`id == 0`) or update a workspace.
    SaveWorkspace { workspace: Workspace },
    /// Delete a workspace; its tabs are kept without a workspace.
    DeleteWorkspace { id: Id },
    /// Move a workspace up (`delta < 0`) or down.
    MoveWorkspace { id: Id, delta: i32 },
    /// Put entries in a workspace (`None` = no workspace). Rules leave them there.
    AssignWorkspace { entries: Vec<EntryRef>, workspace: Option<Id> },
    /// Let the workspace rules place these entries again, even if they were placed by
    /// hand. `None` applies the rules to every entry not placed by hand.
    AutoPlaceEntries { entries: Option<Vec<EntryRef>> },
    /// Unload tabs to free memory. `force` includes pinned tabs and tabs playing media.
    Hibernate { target: HibernateTarget, force: bool },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Response {
    pub ok: bool,
    pub error: Option<String>,
    /// Human readable result ("3 tabs tagged").
    pub info: Option<String>,
    pub state: Option<AppState>,
    pub report: Option<CleanupReport>,
    pub capabilities: Option<Capabilities>,
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
