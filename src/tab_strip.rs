//! Workspaces in the browser's own tab strip: native tab groups mirroring workspaces,
//! hiding the tabs of other workspaces (Firefox), and the window title. This module only
//! plans; the background carries the plans out.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::{BrowserTab, Id, TabId};
use crate::workspace::Workspace;

/// A native tab group, as reported by the browser.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct NativeGroup {
    pub id: i64,
    pub window_id: TabId,
    pub title: String,
    pub color: String,
    pub collapsed: bool,
}

/// Bring one workspace's tabs of one window into its native group.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupSync {
    pub window_id: TabId,
    pub workspace: Id,
    /// Tabs to add to the group (empty: the group is complete).
    pub tab_ids: Vec<TabId>,
    /// The group to use; `None` creates one.
    pub group: Option<i64>,
    pub title: String,
    pub color: &'static str,
    /// `None` leaves the collapsed state alone.
    pub collapsed: Option<bool>,
    /// The group's title, color or collapsed state must be updated.
    pub update: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupPlan {
    pub groups: Vec<GroupSync>,
    /// Tabs in a workspace's group that don't belong to that workspace.
    pub ungroup: Vec<TabId>,
    /// The current workspace of each window (that of its active tab), to pass back as
    /// `collapsed_for` next time.
    pub current: BTreeMap<TabId, Option<Id>>,
}

impl GroupPlan {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.ungroup.is_empty()
    }
}

/// Plans native tab groups so that, in every window, each workspace's tabs sit in one
/// group titled with the workspace's icon and name, in its color. Pinned tabs can't be
/// grouped. Tabs without a workspace keep the groups the user gave them.
///
/// To collapse inactive groups, `collapsed_for` holds the current workspace of each
/// window when groups were last collapsed (`plan.current` of the previous plan). Only the
/// group of the active tab's workspace is expanded then, but only when that changes (or
/// a group is new or gets tabs): in between, groups the user expands or collapses stay so.
pub fn plan_groups(
    workspaces: &[Workspace],
    tabs: &[BrowserTab],
    groups: &[NativeGroup],
    workspace_of: impl Fn(&BrowserTab) -> Option<Id>,
    collapsed_for: Option<&BTreeMap<TabId, Option<Id>>>,
) -> GroupPlan {
    let labels: HashMap<String, Id> = workspaces.iter().map(|w| (w.label(), w.id)).collect();
    let mut windows: BTreeMap<TabId, Vec<&BrowserTab>> = BTreeMap::new();
    for t in tabs {
        windows.entry(t.window_id).or_default().push(t);
    }
    let mut plan = GroupPlan::default();
    for (window_id, wtabs) in windows {
        let current = wtabs.iter().find(|t| t.active).and_then(|t| workspace_of(t));
        plan.current.insert(window_id, current);
        let switched = collapsed_for.is_some_and(|last| last.get(&window_id) != Some(&current));
        let wgroups: Vec<&NativeGroup> = groups.iter().filter(|g| g.window_id == window_id).collect();
        let mut used: HashSet<i64> = HashSet::new();
        let mut regrouped: HashSet<TabId> = HashSet::new();
        for ws in workspaces {
            let members: Vec<&BrowserTab> = wtabs.iter().copied().filter(|t| !t.pinned && workspace_of(t) == Some(ws.id)).collect();
            if members.is_empty() {
                continue;
            }
            let label = ws.label();
            // The group titled like the workspace, or (after a rename) a group that holds
            // only this workspace's tabs.
            let existing = wgroups.iter().copied().find(|g| g.title == label && !used.contains(&g.id)).or_else(|| {
                wgroups.iter().copied().find(|g| {
                    !used.contains(&g.id)
                        && !labels.contains_key(&g.title)
                        && wtabs.iter().filter(|t| t.group_id == Some(g.id)).all(|t| workspace_of(t) == Some(ws.id))
                        && members.iter().any(|t| t.group_id == Some(g.id))
                })
            });
            let group = existing.map(|g| g.id);
            used.extend(group);
            let tab_ids: Vec<TabId> = members.iter().filter(|t| group.is_none() || t.group_id != group).map(|t| t.id).collect();
            regrouped.extend(tab_ids.iter().copied());
            let collapse = collapsed_for.is_some() && (switched || existing.is_none() || !tab_ids.is_empty());
            let collapsed = collapse.then(|| current != Some(ws.id));
            let color = ws.color.native();
            let update = existing.is_none_or(|g| g.title != label || g.color != color || collapsed.is_some_and(|c| c != g.collapsed));
            if !tab_ids.is_empty() || update {
                plan.groups.push(GroupSync { window_id, workspace: ws.id, tab_ids, group, title: label, color, collapsed, update });
            }
        }
        let ws_groups: HashMap<i64, Id> =
            wgroups.iter().filter_map(|g| labels.get(&g.title).map(|w| (g.id, *w))).collect();
        for t in &wtabs {
            if let Some(w) = t.group_id.and_then(|g| ws_groups.get(&g))
                && workspace_of(t) != Some(*w)
                && !regrouped.contains(&t.id)
            {
                plan.ungroup.push(t.id);
            }
        }
    }
    plan
}

/// Hiding and showing tabs so only the current workspace's tabs are in the tab strip.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VisibilityPlan {
    pub show: Vec<TabId>,
    pub hide: Vec<TabId>,
    /// Tabs to switch to first, because the active tab of their window gets hidden.
    pub activate: Vec<TabId>,
    /// Windows with nothing left to show: open a new tab there first.
    pub new_tab_in: Vec<TabId>,
}

impl VisibilityPlan {
    pub fn is_empty(&self) -> bool {
        self.show.is_empty() && self.hide.is_empty() && self.activate.is_empty() && self.new_tab_in.is_empty()
    }
}

/// Plans which tabs to hide: those of workspaces other than `current`. Tabs without a
/// workspace and pinned tabs always show. `None` shows everything.
pub fn plan_visibility(current: Option<Id>, tabs: &[BrowserTab], workspace_of: impl Fn(&BrowserTab) -> Option<Id>) -> VisibilityPlan {
    let hidden_ws = |t: &BrowserTab| current.is_some() && !t.pinned && workspace_of(t).is_some_and(|w| Some(w) != current);
    let mut plan = VisibilityPlan::default();
    let mut windows: BTreeMap<TabId, Vec<&BrowserTab>> = BTreeMap::new();
    for t in tabs {
        windows.entry(t.window_id).or_default().push(t);
    }
    for (window_id, wtabs) in windows {
        for t in &wtabs {
            match (hidden_ws(t), t.hidden) {
                (true, false) => plan.hide.push(t.id),
                (false, true) => plan.show.push(t.id),
                _ => {}
            }
        }
        if wtabs.iter().any(|t| t.active && hidden_ws(t)) {
            let next = wtabs
                .iter()
                .filter(|t| !hidden_ws(t))
                .max_by(|a, b| a.last_accessed.total_cmp(&b.last_accessed).then(b.index.cmp(&a.index)));
            match next {
                Some(t) => plan.activate.push(t.id),
                None => plan.new_tab_in.push(window_id),
            }
        }
    }
    plan
}

/// Text put before the window title: the current workspace, or nothing.
pub fn title_preface(current: Option<&Workspace>) -> String {
    current.map(|w| format!("{} — ", w.label())).unwrap_or_default()
}

/// The `about:config` preference that makes Firefox load `userChrome.css`.
pub const USER_CHROME_PREF: &str = "toolkit.legacyUserProfileCustomizations.stylesheets";

/// `userChrome.css` that hides Firefox's horizontal tab strip (the sidebar lists the tabs)
/// and the title bar of this extension's sidebar. `sidebar_command` is the id Firefox gives
/// the extension's sidebar (see `sidebar_command_id`).
pub fn user_chrome_css(sidebar_command: &str) -> String {
    format!(
        "/* Session Manager: tabs are listed by workspace in the Session Manager sidebar,\n   \
         so hide the horizontal tab strip. Delete these lines to bring it back. */\n\
         #TabsToolbar {{\n  visibility: collapse !important;\n}}\n\n\
         /* Hide the title bar of the Session Manager sidebar. */\n\
         #sidebar-box[sidebarcommand=\"{sidebar_command}\"] #sidebar-header {{\n  display: none !important;\n}}\n"
    )
}

/// The extension's id in Firefox (`browser_specific_settings.gecko.id` in its manifest).
pub const FIREFOX_EXTENSION_ID: &str = "session-manager@local.extension";

/// The id Firefox gives an extension's sidebar, from the extension id.
pub fn sidebar_command_id(extension_id: &str) -> String {
    let widget: String = extension_id
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("{widget}-sidebar-action")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceColor;

    fn tab(id: TabId, window: TabId, group: Option<i64>) -> BrowserTab {
        BrowserTab { id, window_id: window, index: id, group_id: group, url: format!("https://t{id}.com"), ..Default::default() }
    }

    fn workspaces() -> Vec<Workspace> {
        vec![
            Workspace { id: 10, name: "Dev".into(), icon: "💻".into(), color: WorkspaceColor::Green, ..Default::default() },
            Workspace { id: 20, name: "Read".into(), icon: "📚".into(), color: WorkspaceColor::Purple, ..Default::default() },
        ]
    }

    fn ws_of(map: &[(TabId, Id)]) -> impl Fn(&BrowserTab) -> Option<Id> + '_ {
        move |t| map.iter().find(|(id, _)| *id == t.id).map(|(_, w)| *w)
    }

    #[test]
    fn creates_reuses_and_cleans_groups() {
        let ws = workspaces();
        let map = [(1, 10), (2, 10), (3, 20), (5, 20)];
        let mut tabs = vec![tab(1, 1, None), tab(2, 1, Some(7)), tab(3, 1, None), tab(4, 1, Some(7)), tab(5, 2, None)];
        tabs[0].active = true;
        let groups = vec![NativeGroup { id: 7, window_id: 1, title: "💻 Dev".into(), color: "green".into(), collapsed: false }];
        let plan = plan_groups(&ws, &tabs, &groups, ws_of(&map), Some(&BTreeMap::new()));
        // Dev: reuse group 7, add tab 1, already expanded and green -> no update.
        assert_eq!(plan.groups[0], GroupSync {
            window_id: 1,
            workspace: 10,
            tab_ids: vec![1],
            group: Some(7),
            title: "💻 Dev".into(),
            color: "green",
            collapsed: Some(false),
            update: false
        });
        // Read: new group, collapsed (the active tab is in Dev); and one in window 2.
        assert_eq!((plan.groups[1].group, plan.groups[1].collapsed, plan.groups[1].update), (None, Some(true), true));
        assert_eq!((plan.groups[2].window_id, plan.groups[2].tab_ids.clone()), (2, vec![5]));
        // tab 4 has no workspace: out of Dev's group.
        assert_eq!(plan.ungroup, vec![4]);
    }

    #[test]
    fn groups_collapse_when_the_workspace_changes_not_in_between() {
        let ws = workspaces();
        let map = [(1, 10), (2, 20)];
        let mut tabs = vec![tab(1, 1, Some(7)), tab(2, 1, Some(8))];
        tabs[0].active = true;
        let mut groups = vec![
            NativeGroup { id: 7, window_id: 1, title: "💻 Dev".into(), color: "green".into(), collapsed: false },
            NativeGroup { id: 8, window_id: 1, title: "📚 Read".into(), color: "purple".into(), collapsed: false },
        ];
        // First sync: Read (inactive) collapses.
        let plan = plan_groups(&ws, &tabs, &groups, ws_of(&map), Some(&BTreeMap::new()));
        let changes: Vec<_> = plan.groups.iter().map(|g| (g.group, g.collapsed, g.update)).collect();
        assert_eq!(changes, vec![(Some(8), Some(true), true)]);
        let mut last = plan.current;
        // The user expands Read again while still in Dev: later syncs leave it expanded.
        let plan = plan_groups(&ws, &tabs, &groups, ws_of(&map), Some(&last));
        assert!(plan.groups.is_empty(), "{plan:?}");
        // The user collapses Dev: also left so.
        groups[0].collapsed = true;
        assert!(plan_groups(&ws, &tabs, &groups, ws_of(&map), Some(&last)).groups.is_empty());
        // Switching to a tab of Read: Read expands, Dev collapses.
        groups[0].collapsed = false;
        tabs[0].active = false;
        tabs[1].active = true;
        let plan = plan_groups(&ws, &tabs, &groups, ws_of(&map), Some(&last));
        let changes: Vec<_> = plan.groups.iter().map(|g| (g.group, g.collapsed, g.update)).collect();
        assert_eq!(changes, vec![(Some(7), Some(true), true)], "Read is already expanded");
        last = plan.current;
        assert_eq!(last.get(&1), Some(&Some(20)));
    }

    #[test]
    fn stable_once_synced_and_follows_renames() {
        let mut ws = workspaces();
        let map = [(1, 10), (2, 10)];
        let tabs = vec![tab(1, 1, Some(7)), tab(2, 1, Some(7)), tab(3, 1, Some(8))];
        let mut groups = vec![
            NativeGroup { id: 7, window_id: 1, title: "💻 Dev".into(), color: "green".into(), collapsed: false },
            NativeGroup { id: 8, window_id: 1, title: "Mine".into(), color: "red".into(), collapsed: false },
        ];
        assert!(plan_groups(&ws, &tabs, &groups, ws_of(&map), None).is_empty(), "nothing to do");
        // renamed workspace: the same group gets the new title; the user's group is untouched
        ws[0].name = "Code".into();
        let plan = plan_groups(&ws, &tabs, &groups, ws_of(&map), None);
        assert_eq!(plan.groups.len(), 1);
        assert_eq!((plan.groups[0].group, plan.groups[0].title.as_str(), plan.groups[0].update), (Some(7), "💻 Code", true));
        groups[0].title = "💻 Code".into();
        assert!(plan_groups(&ws, &tabs, &groups, ws_of(&map), None).is_empty());
    }

    #[test]
    fn hides_other_workspaces() {
        let map = [(1, 10), (2, 20), (3, 20)];
        let mut tabs = vec![tab(1, 1, None), tab(2, 1, None), tab(3, 2, None), tab(4, 1, None)];
        tabs[1].active = true;
        tabs[2].active = true;
        tabs[3].hidden = true;
        tabs[0].last_accessed = 5.0;
        let plan = plan_visibility(Some(10), &tabs, ws_of(&map));
        assert_eq!(plan.hide, vec![2, 3]);
        assert_eq!(plan.show, vec![4], "tabs without a workspace always show");
        assert_eq!(plan.activate, vec![1]);
        assert_eq!(plan.new_tab_in, vec![2], "window 2 has nothing of Dev");
        let all = plan_visibility(None, &tabs, ws_of(&map));
        assert_eq!((all.hide.len(), all.show.clone()), (0, vec![4]));
    }

    #[test]
    fn extension_id_matches_firefox_manifest() {
        let m: serde_json::Value = serde_json::from_str(include_str!("../firefox/manifest.json")).unwrap();
        assert_eq!(m["browser_specific_settings"]["gecko"]["id"], FIREFOX_EXTENSION_ID);
    }

    #[test]
    fn user_chrome() {
        let id = sidebar_command_id("session-manager@local.extension");
        assert_eq!(id, "session-manager_local_extension-sidebar-action");
        let css = user_chrome_css(&id);
        assert!(css.contains("#TabsToolbar") && css.contains(&id));
        assert_eq!(title_preface(None), "");
        assert_eq!(title_preface(Some(&workspaces()[0])), "💻 Dev — ");
    }
}
