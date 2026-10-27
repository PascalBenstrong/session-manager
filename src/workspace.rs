//! Workspaces: named, colored containers that tabs are grouped into. Workspaces are shared
//! by all sessions. Rules place matching tabs into a workspace automatically, unless the
//! user chose a tab's workspace by hand.

use serde::{Deserialize, Serialize};

use crate::model::{AppState, EntryRef, Id, TabEntry, TabId};
use crate::pattern::{CompiledPattern, SitePattern};
use crate::search::{Query, SearchOptions};

/// Stands for "no workspace" in workspace filters.
pub const NO_WORKSPACE: Id = 0;

/// Workspace colors (the same set browsers use for native tab groups).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WorkspaceColor {
    Grey,
    #[default]
    Blue,
    Red,
    Yellow,
    Green,
    Pink,
    Purple,
    Cyan,
    Orange,
}

impl WorkspaceColor {
    pub const ALL: [WorkspaceColor; 9] = [
        WorkspaceColor::Grey,
        WorkspaceColor::Blue,
        WorkspaceColor::Red,
        WorkspaceColor::Yellow,
        WorkspaceColor::Green,
        WorkspaceColor::Pink,
        WorkspaceColor::Purple,
        WorkspaceColor::Cyan,
        WorkspaceColor::Orange,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WorkspaceColor::Grey => "Grey",
            WorkspaceColor::Blue => "Blue",
            WorkspaceColor::Red => "Red",
            WorkspaceColor::Yellow => "Yellow",
            WorkspaceColor::Green => "Green",
            WorkspaceColor::Pink => "Pink",
            WorkspaceColor::Purple => "Purple",
            WorkspaceColor::Cyan => "Cyan",
            WorkspaceColor::Orange => "Orange",
        }
    }

    pub fn rgb(self) -> [u8; 3] {
        match self {
            WorkspaceColor::Grey => [0x9a, 0xa0, 0xa6],
            WorkspaceColor::Blue => [0x4c, 0x8b, 0xf5],
            WorkspaceColor::Red => [0xe8, 0x5a, 0x4f],
            WorkspaceColor::Yellow => [0xf2, 0xb9, 0x2c],
            WorkspaceColor::Green => [0x34, 0xa8, 0x53],
            WorkspaceColor::Pink => [0xf0, 0x62, 0x92],
            WorkspaceColor::Purple => [0xa1, 0x42, 0xf4],
            WorkspaceColor::Cyan => [0x24, 0xc1, 0xe0],
            WorkspaceColor::Orange => [0xfa, 0x90, 0x3e],
        }
    }

    /// The color's name in the browsers' tab group API.
    pub fn native(self) -> &'static str {
        match self {
            WorkspaceColor::Grey => "grey",
            WorkspaceColor::Blue => "blue",
            WorkspaceColor::Red => "red",
            WorkspaceColor::Yellow => "yellow",
            WorkspaceColor::Green => "green",
            WorkspaceColor::Pink => "pink",
            WorkspaceColor::Purple => "purple",
            WorkspaceColor::Cyan => "cyan",
            WorkspaceColor::Orange => "orange",
        }
    }

    /// A color for the n-th new workspace, so new workspaces don't all look alike.
    pub fn nth(n: usize) -> WorkspaceColor {
        Self::ALL[1 + n % (Self::ALL.len() - 1)]
    }
}

/// Icons offered for workspaces.
pub const ICONS: [&str; 16] = ["📁", "💼", "🏠", "📚", "💻", "🎮", "🎵", "🛒", "📰", "✈", "⭐", "❤", "🔧", "📷", "💰", "🌐"];

/// When a tab is added to a workspace automatically.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceRule {
    /// The tab's URL matches a site pattern.
    Site(SitePattern),
    /// The tab is in this group: its own group or a native browser tab group (ignoring case).
    Group(String),
    /// The tab has this tag (ignoring case).
    Tag(String),
    /// The tab matches a search, in the search box syntax.
    Query(String),
}

impl WorkspaceRule {
    pub fn describe(&self) -> String {
        match self {
            WorkspaceRule::Site(p) => p.describe(),
            WorkspaceRule::Group(g) => format!("In group: {g}"),
            WorkspaceRule::Tag(t) => format!("Tagged: {t}"),
            WorkspaceRule::Query(q) => format!("Matches search: {q}"),
        }
    }

    pub fn compile(&self) -> Result<CompiledRule, String> {
        let not_empty = |s: &str| {
            let s = s.trim();
            if s.is_empty() { Err("the rule needs a value".to_string()) } else { Ok(s.to_lowercase()) }
        };
        Ok(match self {
            WorkspaceRule::Site(p) => CompiledRule::Site(p.compile()?),
            WorkspaceRule::Group(g) => CompiledRule::Group(not_empty(g)?),
            WorkspaceRule::Tag(t) => CompiledRule::Tag(not_empty(t)?),
            WorkspaceRule::Query(q) => {
                not_empty(q)?;
                CompiledRule::Query(Box::new(Query::parse(q, &SearchOptions::default())?))
            }
        })
    }
}

pub enum CompiledRule {
    Site(CompiledPattern),
    Group(String),
    Tag(String),
    Query(Box<Query>),
}

impl CompiledRule {
    pub fn matches(&self, e: &TabEntry) -> bool {
        let same = |a: &str, b: &str| a.to_lowercase() == b;
        match self {
            CompiledRule::Site(p) => p.matches(&e.host(), &e.url),
            CompiledRule::Group(g) => e.group.iter().chain(&e.browser_group).any(|x| same(x, g)),
            CompiledRule::Tag(t) => e.tags.iter().any(|x| same(x, t)),
            CompiledRule::Query(q) => q.matches(e),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Workspace {
    pub id: Id,
    pub name: String,
    pub color: WorkspaceColor,
    pub icon: String,
    /// Add tabs matching any of `rules` automatically.
    pub auto_add: bool,
    pub rules: Vec<WorkspaceRule>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self { id: 0, name: String::new(), color: WorkspaceColor::default(), icon: ICONS[0].into(), auto_add: true, rules: Vec::new() }
    }
}

impl Workspace {
    /// Icon and name; also the title of the native tab group mirroring the workspace.
    pub fn label(&self) -> String {
        format!("{} {}", self.icon, self.name)
    }
}

/// The enabled rules of every workspace, compiled once for a batch of entries.
pub struct Placer {
    rules: Vec<(Id, Vec<CompiledRule>)>,
}

impl Placer {
    pub fn new(workspaces: &[Workspace]) -> Self {
        let rules = workspaces
            .iter()
            .filter(|w| w.auto_add)
            .map(|w| (w.id, w.rules.iter().filter_map(|r| r.compile().ok()).collect::<Vec<_>>()))
            .filter(|(_, rules)| !rules.is_empty())
            .collect();
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The first workspace with a rule matching `e`.
    pub fn workspace_for(&self, e: &TabEntry) -> Option<Id> {
        self.rules.iter().find(|(_, rules)| rules.iter().any(|r| r.matches(e))).map(|(id, _)| *id)
    }

    /// Moves `e` into a matching workspace unless its workspace was chosen by hand.
    /// Rules never take a tab out of a workspace. Returns true if `e` changed.
    pub fn place(&self, e: &mut TabEntry) -> bool {
        if e.workspace_manual {
            return false;
        }
        match self.workspace_for(e) {
            Some(id) if e.workspace != Some(id) => {
                e.workspace = Some(id);
                true
            }
            _ => false,
        }
    }
}

impl AppState {
    pub fn workspace(&self, id: Id) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == id)
    }

    /// Creates (`id == 0`) or updates a workspace. Returns its id.
    pub fn save_workspace(&mut self, mut ws: Workspace) -> Result<Id, String> {
        ws.name = ws.name.trim().to_string();
        if ws.name.is_empty() {
            return Err("the workspace needs a name".into());
        }
        for r in &ws.rules {
            r.compile().map_err(|e| format!("{}: {e}", r.describe()))?;
        }
        if ws.icon.trim().is_empty() {
            ws.icon = ICONS[0].into();
        }
        if ws.id == 0 {
            ws.id = self.alloc_id();
            let id = ws.id;
            self.workspaces.push(ws);
            return Ok(id);
        }
        let id = ws.id;
        *self.workspaces.iter_mut().find(|w| w.id == id).ok_or("no such workspace")? = ws;
        Ok(id)
    }

    /// Deletes a workspace; its tabs stay in their sessions without a workspace.
    pub fn delete_workspace(&mut self, id: Id) -> Result<(), String> {
        let before = self.workspaces.len();
        self.workspaces.retain(|w| w.id != id);
        if self.workspaces.len() == before {
            return Err("no such workspace".into());
        }
        if self.active_workspace == Some(id) {
            self.active_workspace = None;
        }
        self.pending_workspace.retain(|_, w| *w != id);
        for e in self.sessions.iter_mut().flat_map(|s| s.entries.iter_mut()).filter(|e| e.workspace == Some(id)) {
            e.workspace = None;
            e.workspace_manual = false;
        }
        Ok(())
    }

    /// Moves a workspace up (`delta < 0`) or down in the display order.
    pub fn move_workspace(&mut self, id: Id, delta: i32) -> Result<(), String> {
        let i = self.workspaces.iter().position(|w| w.id == id).ok_or("no such workspace")?;
        let j = (i as i64 + delta as i64).clamp(0, self.workspaces.len() as i64 - 1) as usize;
        let ws = self.workspaces.remove(i);
        self.workspaces.insert(j, ws);
        Ok(())
    }

    /// Puts entries into a workspace (`None` = no workspace) chosen by the user.
    pub fn assign_workspace(&mut self, refs: &[EntryRef], workspace: Option<Id>) -> Result<usize, String> {
        if let Some(id) = workspace {
            self.workspace(id).ok_or("no such workspace")?;
        }
        let mut n = 0;
        for r in refs {
            if let Some(e) = self.entry_mut(*r) {
                n += (e.workspace != workspace) as usize;
                e.workspace = workspace;
                e.workspace_manual = true;
            }
        }
        Ok(n)
    }

    /// Lets the workspace rules decide again for these entries (`None` = all entries).
    /// Returns how many entries changed workspace.
    pub fn auto_place(&mut self, refs: Option<&[EntryRef]>, reset_manual: bool) -> usize {
        let placer = Placer::new(&self.workspaces);
        let mut n = 0;
        for s in &mut self.sessions {
            let sid = s.id;
            for e in s.entries.iter_mut().filter(|e| refs.is_none_or(|r| r.contains(&EntryRef { session: sid, entry: e.id }))) {
                if reset_manual && e.workspace_manual {
                    e.workspace_manual = false;
                    if placer.workspace_for(e).is_none() {
                        n += e.workspace.is_some() as usize;
                        e.workspace = None;
                    }
                }
                n += placer.place(e) as usize;
            }
        }
        n
    }

    /// Applies workspace rules to one entry (when it is added or changes).
    pub(crate) fn place_entry(&self, e: &mut TabEntry) -> bool {
        if e.workspace_manual || !self.workspaces.iter().any(|w| w.auto_add && !w.rules.is_empty()) {
            return false;
        }
        Placer::new(&self.workspaces).place(e)
    }

    /// Makes a workspace the current one (`None` = none).
    pub fn set_active_workspace(&mut self, id: Option<Id>) -> Result<(), String> {
        if let Some(id) = id {
            self.workspace(id).ok_or("no such workspace")?;
        }
        self.active_workspace = id;
        Ok(())
    }

    /// The workspace whose native tab group is titled `title`.
    pub fn workspace_by_label(&self, title: &str) -> Option<Id> {
        self.workspaces.iter().find(|w| w.label() == title).map(|w| w.id)
    }

    /// While workspaces are mirrored as native tab groups, a tab moved into a workspace's
    /// group joins the workspace, and a tab taken out of it (in the same window) leaves it.
    /// A new tab opened inside a group (e.g. from a link) joins it like a rule would.
    pub(crate) fn follow_native_group(&self, e: &mut TabEntry, prev_group: Option<&str>, prev_window: Option<TabId>, is_new: bool) -> bool {
        if !self.settings.browser_ui.tab_groups {
            return false;
        }
        match e.browser_group.as_deref().and_then(|t| self.workspace_by_label(t)) {
            Some(w) if e.workspace != Some(w) => {
                e.workspace = Some(w);
                e.workspace_manual = !is_new;
                true
            }
            Some(_) => false,
            None => {
                let left = prev_group.and_then(|t| self.workspace_by_label(t));
                // Moving a tab to another window takes it out of its group too; that
                // doesn't mean it should leave the workspace.
                if !is_new && left.is_some() && left == e.workspace && prev_window == e.window_id {
                    e.workspace = None;
                    e.workspace_manual = true;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Clears references to workspaces that don't exist.
    pub(crate) fn drop_dangling_workspaces(&mut self) {
        let ids: std::collections::HashSet<Id> = self.workspaces.iter().map(|w| w.id).collect();
        if self.active_workspace.is_some_and(|w| !ids.contains(&w)) {
            self.active_workspace = None;
        }
        self.pending_workspace.retain(|_, w| ids.contains(w));
        for e in self.sessions.iter_mut().flat_map(|s| s.entries.iter_mut()) {
            if e.workspace.is_some_and(|w| !ids.contains(&w)) {
                e.workspace = None;
                e.workspace_manual = false;
            }
        }
    }

    /// Entries in a workspace (`NO_WORKSPACE` = in none), across all sessions.
    pub fn workspace_entries(&self, id: Id) -> Vec<EntryRef> {
        self.sessions
            .iter()
            .flat_map(|s| s.entries.iter().map(move |e| (s.id, e)))
            .filter(|(_, e)| e.workspace.unwrap_or(NO_WORKSPACE) == id)
            .map(|(session, e)| EntryRef { session, entry: e.id })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BrowserTab;
    use crate::pattern::PatternKind;

    fn tab(id: i64, url: &str) -> BrowserTab {
        BrowserTab { id, window_id: 1, url: url.into(), ..Default::default() }
    }

    fn ws(name: &str, rules: Vec<WorkspaceRule>) -> Workspace {
        Workspace { name: name.into(), rules, ..Default::default() }
    }

    #[test]
    fn rules_place_new_and_changed_tabs() {
        let mut s = AppState::default();
        let dev = s.save_workspace(ws("Dev", vec![WorkspaceRule::Site(SitePattern::new(PatternKind::Domain, "github.com"))])).unwrap();
        let news = s.save_workspace(ws("News", vec![WorkspaceRule::Query("title:news".into())])).unwrap();
        s.on_tab_upsert(&tab(1, "https://github.com/x"), 1.0);
        s.on_tab_upsert(&BrowserTab { title: "Daily News".into(), ..tab(2, "https://n.com") }, 1.0);
        s.on_tab_upsert(&tab(3, "https://other.com"), 1.0);
        let e = |s: &AppState, t| s.entry_for_tab(t).unwrap().workspace;
        assert_eq!((e(&s, 1), e(&s, 2), e(&s, 3)), (Some(dev), Some(news), None));
        // navigating into a matching site moves the tab; navigating away keeps it
        s.on_tab_upsert(&tab(3, "https://github.com/y"), 2.0);
        assert_eq!(e(&s, 3), Some(dev));
        s.on_tab_upsert(&tab(3, "https://elsewhere.com"), 3.0);
        assert_eq!(e(&s, 3), Some(dev));
    }

    #[test]
    fn group_rules_and_manual_choice() {
        let mut s = AppState::default();
        let work = s.save_workspace(ws("Work", vec![WorkspaceRule::Group("work".into())])).unwrap();
        let other = s.save_workspace(ws("Other", vec![])).unwrap();
        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        s.on_tab_upsert(&tab(2, "https://b.com"), 1.0);
        let r1 = s.locate_tab(1).unwrap();
        // the extension's group
        s.set_group(&[r1], Some("Work"));
        assert_eq!(s.entry(r1).unwrap().workspace, Some(work));
        // a native browser tab group
        s.on_tab_upsert(&BrowserTab { group_title: Some("WORK".into()), ..tab(2, "https://b.com") }, 2.0);
        let r2 = s.locate_tab(2).unwrap();
        assert_eq!(s.entry(r2).unwrap().workspace, Some(work));
        // a manual choice sticks
        s.assign_workspace(&[r2], Some(other)).unwrap();
        s.on_tab_upsert(&BrowserTab { group_title: Some("Work".into()), ..tab(2, "https://b.com/2") }, 3.0);
        assert_eq!(s.entry(r2).unwrap().workspace, Some(other));
        // until the rules are asked to decide again
        assert_eq!(s.auto_place(Some(&[r2]), true), 1);
        assert_eq!(s.entry(r2).unwrap().workspace, Some(work));
    }

    #[test]
    fn native_groups_and_current_workspace() {
        let mut s = AppState::default();
        s.settings.browser_ui.tab_groups = true;
        let rust = s.save_workspace(Workspace { name: "Rust".into(), icon: "💻".into(), ..Default::default() }).unwrap();
        let news = s.save_workspace(ws("News", vec![])).unwrap();
        let grouped = |id, title: Option<&str>| BrowserTab { group_title: title.map(Into::into), ..tab(id, "https://a.com") };
        // a new tab opened inside the workspace's group joins it (not as a manual choice)
        s.on_tab_upsert(&grouped(1, Some("💻 Rust")), 1.0);
        let e = |s: &AppState| s.entry_for_tab(1).unwrap().clone();
        assert_eq!((e(&s).workspace, e(&s).workspace_manual), (Some(rust), false));
        // dragging it out of the group (same window) takes it out of the workspace
        s.on_tab_upsert(&grouped(1, None), 2.0);
        assert_eq!((e(&s).workspace, e(&s).workspace_manual), (None, true));
        // dragging it into a group joins that workspace
        let news_label = s.workspace(news).unwrap().label();
        s.on_tab_upsert(&grouped(1, Some(&news_label)), 3.0);
        assert_eq!(e(&s).workspace, Some(news));
        // moving it to another window (which ungroups it) keeps the workspace
        s.on_tab_upsert(&BrowserTab { window_id: 2, ..grouped(1, None) }, 4.0);
        assert_eq!(e(&s).workspace, Some(news));

        // the current workspace follows the active tab; new tabs may join it
        s.on_tab_activated(1, 5.0);
        assert_eq!(s.active_workspace, Some(news));
        s.on_tab_upsert(&tab(2, "https://b.com"), 6.0);
        assert_eq!(s.entry_for_tab(2).unwrap().workspace, None);
        s.settings.browser_ui.new_tabs_join_active = true;
        s.on_tab_upsert(&tab(3, "https://c.com"), 6.0);
        assert_eq!(s.entry_for_tab(3).unwrap().workspace, Some(news));
        // a tab opened for a workspace gets it once it shows a page
        s.pending_workspace.insert(4, rust);
        assert!(!s.on_tab_upsert(&tab(4, "about:newtab"), 7.0));
        s.on_tab_upsert(&tab(4, "https://d.com"), 7.0);
        assert_eq!(s.entry_for_tab(4).unwrap().workspace, Some(rust));
        assert!(s.pending_workspace.is_empty());

        s.delete_workspace(news).unwrap();
        assert_eq!(s.active_workspace, None);
        assert!(s.set_active_workspace(Some(news)).is_err());
    }

    #[test]
    fn edit_reorder_delete() {
        let mut s = AppState::default();
        assert!(s.save_workspace(ws(" ", vec![])).is_err());
        assert!(s.save_workspace(ws("Bad", vec![WorkspaceRule::Query("/(/".into())])).is_err());
        let a = s.save_workspace(ws("A", vec![])).unwrap();
        let b = s.save_workspace(ws("B", vec![])).unwrap();
        s.move_workspace(b, -1).unwrap();
        assert_eq!(s.workspaces.iter().map(|w| w.id).collect::<Vec<_>>(), vec![b, a]);
        let mut renamed = s.workspace(a).unwrap().clone();
        renamed.name = "A2".into();
        renamed.color = WorkspaceColor::Green;
        s.save_workspace(renamed).unwrap();
        assert_eq!(s.workspace(a).unwrap().name, "A2");

        s.on_tab_upsert(&tab(1, "https://a.com"), 1.0);
        let r = s.locate_tab(1).unwrap();
        s.assign_workspace(&[r], Some(a)).unwrap();
        assert_eq!(s.workspace_entries(a), vec![r]);
        s.delete_workspace(a).unwrap();
        assert_eq!(s.entry(r).unwrap().workspace, None);
        assert_eq!(s.workspace_entries(NO_WORKSPACE), vec![r]);
        assert!(s.assign_workspace(&[r], Some(a)).is_err());
    }
}
