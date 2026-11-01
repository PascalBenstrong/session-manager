//! Tabs view: search, tabs listed by workspace (or another key), per-tab and bulk
//! actions, workspace settings and hibernation.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use eframe::egui::{self, Align, Color32, CursorIcon, Layout, Margin, RichText, Sense, Stroke, UiBuilder};

use super::offload::Search;
use super::theme::{self, RADIUS};
use super::widgets::{
    ChipKind, DraggedTab, avatar_sized, badge, chip, color_swatch, danger_button, drag_ghost, empty_state, icon_button,
    icon_button_colored, parse_list, pill, primary_button, status_dot, tab_state_color, text_input, workspace_chip, ws_color,
};
use super::{Actions, ViewCtx};
use crate::dedupe::Scope;
use crate::hibernate::HibernateTarget;
use crate::model::{EntryRef, Id, TabEntry, TabGrouping, TabId};
use crate::organize::group_entries;
use crate::pattern::{PatternKind, SitePattern};
use crate::platform as pf;
use crate::protocol::Command;
use crate::search::{Field, MatchMode, Query, SearchOptions};
use crate::workspace::{ICONS, NO_WORKSPACE, Workspace, WorkspaceColor, WorkspaceRule};

const SEARCH_HELP: &str = "Terms are combined with AND. Quote phrases: \"foo bar\"\n\
\n\
Fields:   title:  url:  host:  domain:  tag:  group:  ws: (workspace)\n\
Exact:    =word   or   ^word$\n\
Prefix:   ^word\n\
Suffix:   word$\n\
Glob:     *word?    Regex: /pattern/\n\
Negate:   -term   (e.g. -tag:done)\n\
State:    is:open  is:unloaded  is:hibernated  is:playing  is:pinned\n\
          is:grouped  is:ungrouped  is:tagged  is:duplicate\n\
Idle:     idle:30m  idle:2h  idle:1d   (not used for that long)\n\
\n\
Example:  ws:work host:^docs -is:unloaded \"release notes\"";

const ROW_HEIGHT: f32 = 48.0;
const COMPACT_ROW_HEIGHT: f32 = 32.0;
const HEADER_HEIGHT: f32 = 38.0;

/// Kind of a new workspace rule in the rule editor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RuleKind {
    Site(PatternKind),
    Group,
    Tag,
    Query,
}

impl Default for RuleKind {
    fn default() -> Self {
        RuleKind::Site(PatternKind::Domain)
    }
}

impl RuleKind {
    const ALL: [RuleKind; 8] = [
        RuleKind::Site(PatternKind::Domain),
        RuleKind::Site(PatternKind::Host),
        RuleKind::Site(PatternKind::UrlPrefix),
        RuleKind::Site(PatternKind::Glob),
        RuleKind::Site(PatternKind::Regex),
        RuleKind::Group,
        RuleKind::Tag,
        RuleKind::Query,
    ];

    fn label(self) -> &'static str {
        match self {
            RuleKind::Site(PatternKind::Domain) => "Site: domain",
            RuleKind::Site(PatternKind::Host) => "Site: exact host",
            RuleKind::Site(PatternKind::UrlPrefix) => "URL starts with",
            RuleKind::Site(PatternKind::Glob) => "URL glob",
            RuleKind::Site(PatternKind::Regex) => "URL regex",
            RuleKind::Group => "Added to group",
            RuleKind::Tag => "Has tag",
            RuleKind::Query => "Matches search",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            RuleKind::Site(PatternKind::Domain) => "example.com (and subdomains)",
            RuleKind::Site(PatternKind::Host) => "www.example.com",
            RuleKind::Site(PatternKind::UrlPrefix) => "https://example.com/app/",
            RuleKind::Site(PatternKind::Glob) => "*.example.* or https://*/docs/*",
            RuleKind::Site(PatternKind::Regex) => r"^https://(www\.)?example\.",
            RuleKind::Group => "group or browser tab group name",
            RuleKind::Tag => "tag",
            RuleKind::Query => "e.g. title:invoice -host:mail.example.com",
        }
    }

    fn build(self, value: &str) -> WorkspaceRule {
        let value = value.trim().to_string();
        match self {
            RuleKind::Site(kind) => WorkspaceRule::Site(SitePattern::new(kind, value)),
            RuleKind::Group => WorkspaceRule::Group(value),
            RuleKind::Tag => WorkspaceRule::Tag(value),
            RuleKind::Query => WorkspaceRule::Query(value),
        }
    }
}

/// Per-frame lookups shared by the rows.
struct RowCtx<'a> {
    owner: &'a HashMap<Id, Id>,
    session_names: &'a HashMap<Id, &'a str>,
    playing: &'a HashSet<TabId>,
}

impl RowCtx<'_> {
    fn entry_ref(&self, e: &TabEntry) -> EntryRef {
        EntryRef { session: self.owner[&e.id], entry: e.id }
    }

    fn refs(&self, entries: &[&TabEntry]) -> Vec<EntryRef> {
        entries.iter().map(|e| self.entry_ref(e)).collect()
    }
}

#[derive(Default)]
pub struct TabsView {
    query: String,
    opts: SearchOptions,
    search: Search,
    all_sessions: bool,
    show_filters: bool,
    bulk_tag: String,
    bulk_group: String,
    editing: Option<EntryRef>,
    edit_tag: String,
    edit_group: String,
    /// Workspace whose settings are shown.
    ws_open: Option<Id>,
    ws_name: String,
    rule_kind: RuleKind,
    rule_value: String,
    /// Name of a workspace being created.
    new_ws: Option<String>,
    /// Open the settings of the next workspace created (the count before creating it).
    open_created: Option<usize>,
    /// Selected tabs (checkboxes, Ctrl-click, Shift-click, Ctrl+A): the selection bar acts on them.
    selection: BTreeSet<EntryRef>,
    /// Where a Shift-click range starts.
    anchor: Option<EntryRef>,
    /// The rows in the order drawn (this frame, and the last one for Shift-click ranges).
    order: Vec<EntryRef>,
    prev_order: Vec<EntryRef>,
    /// The rule being typed, checked: how many saved tabs it matches, or why it's invalid.
    rule_check: Option<((RuleKind, String, u64), Result<usize, String>)>,
    /// The open workspace's tabs: (closed ones in the viewed session, loaded, all), for
    /// (workspace, viewed session, state generation).
    ws_counts: Option<((Id, Id, u64), (Vec<EntryRef>, usize, usize))>,
    /// The session of each entry listed, for (state generation, all sessions, viewed session).
    owners: Option<((u64, bool, Id), Rc<HashMap<Id, Id>>)>,
}

impl TabsView {
    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let state = cx.state;
        if let Some(n) = self.open_created
            && let Some(ws) = state.workspaces.get(n)
        {
            self.open_created = None;
            self.open_settings(ws);
        }
        if self.ws_open.is_some_and(|id| state.workspace(id).is_none()) {
            self.ws_open = None;
        }
        self.opts.workspaces.retain(|id| *id == NO_WORKSPACE || state.workspace(*id).is_some());

        let playing: HashSet<TabId> = state.media.playing.iter().map(|p| p.tab_id).collect();
        let scope = if self.all_sessions { Scope::All } else { Scope::Session(cx.viewed) };
        let (hits, query_error) = self.search.run(ui.ctx(), state, &self.query, &self.opts, Some(scope));

        // Entry ids are globally unique, so map them back to their session.
        let key = (super::state_generation(), self.all_sessions, cx.viewed);
        let owner = match &self.owners {
            Some((k, owner)) if *k == key => owner.clone(),
            _ => {
                let sessions = state.sessions.iter().filter(|s| self.all_sessions || s.id == cx.viewed);
                let owner: Rc<HashMap<Id, Id>> = Rc::new(sessions.flat_map(|s| s.entries.iter().map(|e| (e.id, s.id))).collect());
                self.owners = Some((key, owner.clone()));
                owner
            }
        };
        let mut matched: Vec<&TabEntry> = Vec::new();
        let mut total = 0;
        for s in state.sessions.iter().filter(|s| self.all_sessions || s.id == cx.viewed) {
            for e in &s.entries {
                total += 1;
                if hits.contains(e.id) {
                    matched.push(e);
                }
            }
        }
        let session_names: HashMap<Id, &str> = state.sessions.iter().map(|s| (s.id, s.name.as_str())).collect();
        let rc = RowCtx { owner: &owner, session_names: &session_names, playing: &playing };
        let refs = rc.refs(&matched);

        // Only tabs that are listed stay selected.
        self.prev_order = std::mem::take(&mut self.order);
        if !self.selection.is_empty() {
            let listed: HashSet<EntryRef> = refs.iter().copied().collect();
            self.selection.retain(|r| listed.contains(r));
        }
        if !ui.ctx().egui_wants_keyboard_input() {
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selection.clear();
            }
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A)) {
                self.selection.extend(refs.iter().copied());
            }
        }

        self.search_bar(ui, cx, &rc, &matched, total, query_error.as_deref(), acts);
        let filtering = !self.query.trim().is_empty() || !self.opts.groups.is_empty() || !self.opts.workspaces.is_empty();
        // (While tabs are selected, their bar floats over the bottom of the list instead.)
        if self.selection.is_empty() && filtering && !matched.is_empty() && query_error.is_none() {
            self.bulk_bar(ui, cx, &refs, &matched, &refs, false, acts);
        }
        if self.new_ws.is_some() {
            self.new_workspace_form(ui, cx, acts);
        }
        ui.add_space(6.0);

        let view = ui.max_rect();
        let selecting = !self.selection.is_empty();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.list(ui, cx, &rc, &matched, filtering, total, acts);
            if selecting {
                // Room to scroll the last rows above the selection bar.
                ui.add_space(110.0);
            }
        });
        if selecting {
            let (sel_entries, sel_refs): (Vec<&TabEntry>, Vec<EntryRef>) =
                matched.iter().zip(&refs).filter(|(_, r)| self.selection.contains(r)).map(|(e, r)| (*e, *r)).unzip();
            let p = theme::palette(ui);
            // Floats over the bottom of the list, so selecting doesn't move the rows.
            egui::Area::new(egui::Id::unique("tabs-selection-bar"))
                .order(egui::Order::Foreground)
                .fixed_pos(view.center_bottom() - egui::vec2(0.0, 12.0))
                .pivot(egui::Align2::CENTER_BOTTOM)
                .show(ui.ctx(), |ui| {
                    egui::Frame::new().fill(p.surface).corner_radius(12).shadow(ui.visuals().popup_shadow).show(ui, |ui| {
                        ui.set_max_width((view.width() - 24.0).max(200.0));
                        self.bulk_bar(ui, cx, &sel_refs, &sel_entries, &refs, true, acts);
                    });
                });
        }
        drag_ghost(ui);
    }

    /// The tabs, by workspace or by another key.
    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ViewCtx,
        rc: &RowCtx,
        matched: &[&TabEntry],
        filtering: bool,
        total: usize,
        acts: &mut Actions,
    ) {
        let grouping = cx.state.settings.tab_list.grouping;
        if grouping == TabGrouping::Workspace {
            self.by_workspace(ui, cx, rc, matched, filtering, total, acts);
            return;
        }
        if matched.is_empty() {
            no_tabs(ui, total);
            return;
        }
        match grouping.organize_key() {
            None => {
                for e in matched {
                    self.row(ui, cx, rc, e, acts);
                }
            }
            Some(key) => {
                for (k, entries) in group_entries(key, matched.iter().copied()) {
                    let name = k.as_deref().unwrap_or("Ungrouped");
                    let id = ui.make_persistent_id(("group", key.label(), k.clone()));
                    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
                        .show_header(ui, |ui| {
                            ui.label(RichText::new(name).strong());
                            badge(ui, &entries.len().to_string(), false);
                        })
                        .body_unindented(|ui| {
                            for e in entries {
                                self.row(ui, cx, rc, e, acts);
                            }
                        });
                    ui.add_space(4.0);
                }
            }
        }
    }

    // ------------------------------------------------------------ workspaces

    #[allow(clippy::too_many_arguments)]
    fn by_workspace(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ViewCtx,
        rc: &RowCtx,
        matched: &[&TabEntry],
        filtering: bool,
        total: usize,
        acts: &mut Actions,
    ) {
        let workspaces = &cx.state.workspaces;
        if workspaces.is_empty() {
            self.workspace_intro(ui, cx);
            if matched.is_empty() {
                no_tabs(ui, total);
            }
            for e in matched {
                self.row(ui, cx, rc, e, acts);
            }
            return;
        }
        let known: HashSet<Id> = workspaces.iter().map(|w| w.id).collect();
        for ws in workspaces {
            let entries: Vec<&TabEntry> = matched.iter().copied().filter(|e| e.workspace == Some(ws.id)).collect();
            if filtering && entries.is_empty() && self.ws_open != Some(ws.id) {
                continue;
            }
            self.workspace_section(ui, cx, rc, Some(ws), &entries, acts);
        }
        let rest: Vec<&TabEntry> = matched.iter().copied().filter(|e| e.workspace.is_none_or(|w| !known.contains(&w))).collect();
        if !rest.is_empty() || !filtering {
            self.workspace_section(ui, cx, rc, None, &rest, acts);
        }
        if filtering && matched.is_empty() {
            empty_state(ui, "🔍", "No tabs match", "Try a different search or clear the filters.");
        }
    }

    /// Shown in place of workspace sections while there are none.
    fn workspace_intro(&mut self, ui: &mut egui::Ui, cx: &ViewCtx) {
        let p = theme::palette(ui);
        egui::Frame::new()
            .fill(p.accent_soft())
            .stroke(Stroke::new(1.0, p.accent.gamma_multiply(0.3)))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("◫ Group your tabs into workspaces").strong());
                    ui.label(RichText::new("Give each a color and icon, and rules that add matching tabs automatically.").color(p.muted));
                    if ui.add(primary_button(ui, "＋ New workspace")).clicked() {
                        self.start_new_workspace(cx);
                    }
                });
            });
        ui.add_space(6.0);
    }

    fn workspace_section(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ViewCtx,
        rc: &RowCtx,
        ws: Option<&Workspace>,
        entries: &[&TabEntry],
        acts: &mut Actions,
    ) {
        let p = theme::palette(ui);
        let cid = ui.make_persistent_id(("ws-collapsed", ws.map_or(NO_WORKSPACE, |w| w.id)));
        let mut collapsed = ui.data(|d| d.get_temp::<bool>(cid)).unwrap_or(false);
        if self.workspace_header(ui, cx, rc, ws, entries, collapsed, acts) {
            collapsed = !collapsed;
            ui.data_mut(|d| d.insert_temp(cid, collapsed));
        }
        if let Some(ws) = ws
            && self.ws_open == Some(ws.id)
        {
            self.workspace_settings(ui, cx, ws, acts);
        }
        if !collapsed {
            if entries.is_empty() {
                let hint = if ws.is_some() {
                    "No tabs here yet. Drag tabs onto the header, or add rules in its settings (click the header)."
                } else {
                    "Every tab is in a workspace."
                };
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(RichText::new(hint).small().color(p.muted));
                });
            }
            for e in entries {
                self.row(ui, cx, rc, e, acts);
            }
        }
        ui.add_space(8.0);
    }

    /// Colored header of a workspace section. Clicking it opens the workspace settings;
    /// tabs can be dropped on it. Returns true if the section should collapse / expand.
    #[allow(clippy::too_many_arguments)]
    fn workspace_header(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ViewCtx,
        rc: &RowCtx,
        ws: Option<&Workspace>,
        entries: &[&TabEntry],
        collapsed: bool,
        acts: &mut Actions,
    ) -> bool {
        let p = theme::palette(ui);
        let color = ws.map_or(p.muted, |w| ws_color(w.color));
        let settings_open = ws.is_some_and(|w| self.ws_open == Some(w.id));
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HEADER_HEIGHT), Sense::click());
        let drop_hover = resp.dnd_hover_payload::<DraggedTab>().is_some();
        if let Some(d) = resp.dnd_release_payload::<DraggedTab>() {
            // A selected tab brings the rest of the selection along.
            let entries = if self.selection.contains(&d.entry) { self.selection.iter().copied().collect() } else { vec![d.entry] };
            acts.send(Command::AssignWorkspace { entries, workspace: ws.map(|w| w.id) });
        }
        let mut toggle = false;
        if ui.is_rect_visible(rect) {
            let hovered = ui.rect_contains_pointer(rect);
            let fill = color.gamma_multiply(if hovered || drop_hover { 0.24 } else { 0.13 });
            let stroke = if drop_hover {
                Stroke::new(1.5, color)
            } else if settings_open {
                Stroke::new(1.0, color.gamma_multiply(0.7))
            } else {
                Stroke::NONE
            };
            ui.painter().rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);
            let bar = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 7.0), egui::vec2(4.0, rect.height() - 14.0));
            ui.painter().rect_filled(bar, 2.0, color);

            let mut row =
                ui.new_child(UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
            let (fold_icon, fold_tip) = if collapsed { ("▸", "Show its tabs") } else { ("▾", "Hide its tabs") };
            if icon_button(&mut row, fold_icon, fold_tip).clicked() {
                toggle = true;
            }
            let (icon, name) = match ws {
                Some(w) => (w.icon.as_str(), w.name.as_str()),
                None => ("◌", "No workspace"),
            };
            row.label(RichText::new(icon).size(16.0).color(color));
            row.add(egui::Label::new(RichText::new(name).strong().color(p.text)).truncate());
            badge(&mut row, &entries.len().to_string(), false);
            let open = entries.iter().filter(|e| e.is_open()).count();
            let sleeping = entries.iter().filter(|e| e.is_hibernated()).count();
            let mut meta = format!("{open} open");
            if sleeping > 0 {
                meta.push_str(&format!(" · {sleeping} hibernated"));
            }
            row.label(RichText::new(meta).small().color(p.muted));

            let current = cx.state.active_workspace == ws.map(|w| w.id);
            if current {
                let tip = if ws.is_some() {
                    "The workspace you're in: new tabs can join it, and the browser shows it"
                } else {
                    "You're in no workspace: new tabs stay out of workspaces"
                };
                row.label(RichText::new("● current").small().color(color)).on_hover_text(tip);
            }
            row.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(w) = ws {
                    let tip = if settings_open { "Close the workspace settings" } else { "Workspace settings" };
                    if icon_button_colored(ui, "⚙", tip, settings_open.then_some(color)).clicked() {
                        self.toggle_settings(w);
                    }
                    if w.auto_add && !w.rules.is_empty() {
                        ui.label(RichText::new("⚡").color(p.muted)).on_hover_text(format!(
                            "Adds matching tabs automatically:\n{}",
                            w.rules.iter().map(|r| format!("• {}", r.describe())).collect::<Vec<_>>().join("\n")
                        ));
                    }
                }
                let loaded: Vec<EntryRef> = entries.iter().filter(|e| e.is_open() && !e.discarded).map(|e| rc.entry_ref(e)).collect();
                let switch_tip = ws.map_or("Leave the current workspace (no workspace)".to_string(), |w| format!("Switch to {}", w.name));
                if hovered && !current && icon_button(ui, "⇄", &switch_tip).clicked() {
                    acts.send(Command::SetActiveWorkspace { id: ws.map(|w| w.id) });
                }
                if hovered
                    && !loaded.is_empty()
                    && icon_button(ui, "💤", &format!("Hibernate the {} loaded tab(s) listed here", loaded.len())).clicked()
                {
                    acts.send(Command::Hibernate { target: HibernateTarget::Entries(loaded), force: false });
                }
                let all: Vec<EntryRef> = rc.refs(entries);
                if hovered && !all.is_empty() {
                    let every = all.iter().all(|r| self.selection.contains(r));
                    let (icon, tip) = if every { ("☐", "Unselect its tabs") } else { ("☑", "Select its tabs") };
                    if icon_button(ui, icon, tip).clicked() {
                        for r in &all {
                            if every {
                                self.selection.remove(r);
                            } else {
                                self.selection.insert(*r);
                            }
                        }
                    }
                }
                let closed: Vec<EntryRef> = entries.iter().filter(|e| !e.is_open()).map(|e| rc.entry_ref(e)).collect();
                if hovered
                    && !closed.is_empty()
                    && icon_button(ui, "⤓", &format!("Open the {} closed tab(s) listed here, unloaded, in the background", closed.len()))
                        .clicked()
                {
                    acts.send(Command::OpenEntries { entries: closed, discarded: true });
                }
            });
        }
        let tip = if ws.is_some() {
            "Click for the workspace settings · drop tabs here"
        } else {
            "Drop tabs here to take them out of their workspace"
        };
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip);
        if resp.clicked() {
            match ws {
                Some(w) => self.toggle_settings(w),
                None => toggle = true,
            }
        }
        toggle
    }

    fn open_settings(&mut self, ws: &Workspace) {
        self.ws_open = Some(ws.id);
        self.ws_name = ws.name.clone();
        self.rule_value.clear();
    }

    fn toggle_settings(&mut self, ws: &Workspace) {
        if self.ws_open == Some(ws.id) {
            self.ws_open = None;
        } else {
            self.open_settings(ws);
        }
    }

    /// Settings panel under a workspace header: name, icon, color, rules and actions.
    /// Every change is saved right away.
    fn workspace_settings(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, ws: &Workspace, acts: &mut Actions) {
        let p = theme::palette(ui);
        let color = ws_color(ws.color);
        let save = |acts: &mut Actions, f: &dyn Fn(&mut Workspace)| {
            let mut w = ws.clone();
            f(&mut w);
            acts.send(Command::SaveWorkspace { workspace: w });
        };
        egui::Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
            .corner_radius(RADIUS + 2)
            .inner_margin(Margin::same(14))
            .outer_margin(Margin { left: 0, right: 0, top: 4, bottom: 6 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Name").color(p.muted));
                    let r = ui.add(text_input(&mut self.ws_name, "Workspace name", 200.0));
                    let name = self.ws_name.trim().to_string();
                    if r.lost_focus() && !name.is_empty() && name != ws.name {
                        save(acts, &|w| w.name = name.clone());
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("Icon").color(p.muted));
                    ui.menu_button(RichText::new(&ws.icon).size(16.0), |ui| {
                        ui.set_max_width(200.0);
                        ui.horizontal_wrapped(|ui| {
                            for icon in ICONS {
                                let selected = ws.icon == icon;
                                if ui.selectable_label(selected, RichText::new(icon).size(18.0)).clicked() {
                                    save(acts, &|w| w.icon = icon.to_string());
                                    ui.close();
                                }
                            }
                        });
                    })
                    .response
                    .on_hover_text("Choose an icon");
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Color").color(p.muted));
                    for c in WorkspaceColor::ALL {
                        if color_swatch(ui, ws_color(c), c == ws.color, c.label()).clicked() && c != ws.color {
                            save(acts, &|w| w.color = c);
                        }
                    }
                });

                ui.add_space(6.0);
                ui.separator();
                let mut auto = ws.auto_add;
                if ui.checkbox(&mut auto, RichText::new("Add tabs automatically").strong()).changed() {
                    save(acts, &|w| w.auto_add = auto);
                }
                ui.label(
                    RichText::new(
                        "Opened tabs, and tabs that change page, group or tags, join this workspace when they match \
                         any rule below. Tabs you moved by hand stay where you put them.",
                    )
                    .small()
                    .color(p.muted),
                );
                ui.add_space(4.0);
                for (i, rule) in ws.rules.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("•").color(color));
                        ui.label(rule.describe());
                        if icon_button(ui, "🗑", "Delete this rule").clicked() {
                            save(acts, &|w| {
                                w.rules.remove(i);
                            });
                        }
                    });
                }
                self.rule_editor(ui, cx, ws, acts);

                ui.add_space(6.0);
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if cx.state.active_workspace != Some(ws.id)
                        && ui
                            .add(primary_button(ui, "⇄ Switch to it"))
                            .on_hover_text("Go to its most recently used tab; the browser shows this workspace")
                            .clicked()
                    {
                        acts.send(Command::SetActiveWorkspace { id: Some(ws.id) });
                    }
                    if ui.button("🔍 Search in this workspace").clicked() {
                        self.opts.workspaces = [ws.id].into();
                        self.show_filters = true;
                    }
                    let key = (ws.id, cx.viewed, super::state_generation());
                    if self.ws_counts.as_ref().is_none_or(|(k, _)| *k != key) {
                        let closed: Vec<EntryRef> = cx
                            .state
                            .session(cx.viewed)
                            .map(|s| {
                                s.entries
                                    .iter()
                                    .filter(|e| e.workspace == Some(ws.id) && !e.is_open())
                                    .map(|e| EntryRef { session: s.id, entry: e.id })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let all = cx.state.workspace_entries(ws.id).len();
                        let loaded = cx
                            .state
                            .sessions
                            .iter()
                            .flat_map(|s| &s.entries)
                            .filter(|e| e.workspace == Some(ws.id) && e.is_open() && !e.discarded)
                            .count();
                        self.ws_counts = Some((key, (closed, loaded, all)));
                    }
                    let (closed, loaded, all) = self.ws_counts.as_ref().map_or((&[][..], 0, 0), |(_, (c, l, a))| (&c[..], *l, *a));
                    if ui
                        .add_enabled(!closed.is_empty(), egui::Button::new(format!("⤓ Open its closed tabs ({})", closed.len())))
                        .on_hover_text("Open its tabs of this session that aren't open, unloaded and in the background (they load when you select them)")
                        .clicked()
                    {
                        acts.send(Command::OpenEntries { entries: closed.to_vec(), discarded: true });
                    }
                    if ui
                        .add_enabled(loaded > 0, egui::Button::new(format!("💤 Hibernate its tabs ({loaded})")))
                        .on_hover_text("Unload its open tabs in every session (active, pinned and playing tabs are skipped)")
                        .clicked()
                    {
                        acts.send(Command::Hibernate { target: HibernateTarget::Workspace(ws.id), force: false });
                    }
                    if ui
                        .button("⚡ Apply rules to all tabs")
                        .on_hover_text("Place existing tabs by the rules of every workspace (tabs placed by hand stay)")
                        .clicked()
                    {
                        acts.send(Command::AutoPlaceEntries { entries: None });
                    }
                    let index = cx.state.workspaces.iter().position(|w| w.id == ws.id).unwrap_or(0);
                    if ui.add_enabled(index > 0, egui::Button::new("↑")).on_hover_text("Move up").clicked() {
                        acts.send(Command::MoveWorkspace { id: ws.id, delta: -1 });
                    }
                    let last = index + 1 >= cx.state.workspaces.len();
                    if ui.add_enabled(!last, egui::Button::new("↓")).on_hover_text("Move down").clicked() {
                        acts.send(Command::MoveWorkspace { id: ws.id, delta: 1 });
                    }
                    if ui.add(danger_button(ui, "🗑 Delete workspace…")).clicked() {
                        acts.confirm(
                            format!(
                                "Delete the workspace \"{}\"? Its {} tab(s) stay in their sessions, without a workspace.",
                                ws.name, all
                            ),
                            Command::DeleteWorkspace { id: ws.id },
                        );
                    }
                });
            });
    }

    fn rule_editor(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, ws: &Workspace, acts: &mut Actions) {
        let p = theme::palette(ui);
        let rule = self.rule_kind.build(&self.rule_value);
        let empty = self.rule_value.trim().is_empty();
        let key = (self.rule_kind, self.rule_value.clone(), super::state_generation());
        if !empty && self.rule_check.as_ref().is_none_or(|(k, _)| *k != key) {
            let check = rule.compile().map(|c| cx.state.sessions.iter().flat_map(|s| &s.entries).filter(|e| c.matches(e)).count());
            self.rule_check = Some((key, check));
        }
        let compiled = if empty { None } else { self.rule_check.as_ref().map(|(_, c)| c.clone()) };
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt(("rule-kind", ws.id)).selected_text(self.rule_kind.label()).width(150.0).show_ui(ui, |ui| {
                for k in RuleKind::ALL {
                    ui.selectable_value(&mut self.rule_kind, k, k.label());
                }
            });
            let r = ui.add(text_input(&mut self.rule_value, self.rule_kind.hint(), 240.0));
            // Suggest existing groups (including native tab groups) and tags.
            let suggestions: Vec<String> = match self.rule_kind {
                RuleKind::Group => {
                    let mut g = cx.state.all_groups();
                    g.extend(cx.state.sessions.iter().flat_map(|s| s.entries.iter().filter_map(|e| e.browser_group.clone())));
                    g.into_iter().collect()
                }
                RuleKind::Tag => cx.state.all_tags().into_iter().collect(),
                _ => Vec::new(),
            };
            if !suggestions.is_empty() {
                ui.menu_button("▾", |ui| {
                    for s in &suggestions {
                        if ui.button(s).clicked() {
                            self.rule_value = s.clone();
                            ui.close();
                        }
                    }
                });
            }
            let valid = matches!(compiled, Some(Ok(_)));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add_enabled(valid, egui::Button::new("＋ Add rule")).clicked() || (enter && valid) {
                let rule = rule.clone();
                let mut w = ws.clone();
                w.rules.push(rule);
                acts.send(Command::SaveWorkspace { workspace: w });
                self.rule_value.clear();
            }
        });
        match &compiled {
            Some(Err(e)) => {
                ui.label(RichText::new(e).small().color(p.danger));
            }
            Some(Ok(n)) => {
                ui.label(RichText::new(format!("Matches {n} saved tab(s) right now")).small().color(p.muted));
            }
            None => {}
        }
    }

    fn start_new_workspace(&mut self, cx: &ViewCtx) {
        self.new_ws = Some(format!("Workspace {}", cx.state.workspaces.len() + 1));
    }

    fn new_workspace_form(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let Some(name) = &mut self.new_ws else { return };
        let mut submit = None;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("New workspace");
            let r = ui.add(text_input(name, "Workspace name", 220.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = Some(true);
            } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                submit = Some(false);
            }
            if !r.has_focus() && submit.is_none() && ui.memory(|m| m.focused().is_none()) {
                r.request_focus();
            }
            if ui.add(primary_button(ui, "Create")).clicked() {
                submit = Some(true);
            }
            if ui.button("Cancel").clicked() {
                submit = Some(false);
            }
        });
        if let Some(create) = submit {
            let name = self.new_ws.take().unwrap_or_default();
            if create {
                let n = cx.state.workspaces.len();
                acts.send(Command::SaveWorkspace { workspace: Workspace { name, color: WorkspaceColor::nth(n), ..Default::default() } });
                self.open_created = Some(n);
            }
        }
    }

    // ------------------------------------------------------------ search & toolbar

    #[allow(clippy::too_many_arguments)]
    fn search_bar(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ViewCtx,
        rc: &RowCtx,
        matched: &[&TabEntry],
        total: usize,
        error: Option<&str>,
        acts: &mut Actions,
    ) {
        let p = theme::palette(ui);
        egui::Frame::new()
            .fill(p.input)
            .stroke(Stroke::new(1.0, if error.is_some() { p.danger } else { p.border }))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(10, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🔍").color(p.muted));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let filters_on = self.show_filters || self.opts != SearchOptions::default() || self.all_sessions;
                        if icon_button_colored(ui, "⚙", "Search options", filters_on.then_some(p.accent_hover)).clicked() {
                            self.show_filters = !self.show_filters;
                        }
                        if !self.query.is_empty() && icon_button(ui, "🗙", "Clear search").clicked() {
                            self.query.clear();
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .hint_text("Search tabs — host:  tag:  group:  ws:  is:hibernated  idle:1h  /regex/  -not")
                                .frame(egui::Frame::NONE)
                                .desired_width(ui.available_width())
                                .margin(egui::vec2(2.0, 6.0)),
                        )
                        .on_hover_text(SEARCH_HELP);
                    });
                });
            });

        if self.show_filters {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Plain terms in").color(p.muted));
                egui::ComboBox::from_id_salt("field").selected_text(self.opts.default_field.label()).show_ui(ui, |ui| {
                    for f in Field::ALL {
                        ui.selectable_value(&mut self.opts.default_field, f, f.label());
                    }
                });
                egui::ComboBox::from_id_salt("mode").selected_text(self.opts.default_mode.label()).show_ui(ui, |ui| {
                    for m in MatchMode::ALL {
                        ui.selectable_value(&mut self.opts.default_mode, m, m.label());
                    }
                });
                ui.checkbox(&mut self.opts.case_sensitive, "Match case");
                ui.checkbox(&mut self.all_sessions, "All sessions");
                self.workspace_filter(ui, cx);
                self.group_filter(ui, cx);
            });
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            match error {
                Some(err) => {
                    ui.label(RichText::new(err).color(p.danger));
                }
                None => {
                    ui.label(RichText::new(format!("{} of {total} tabs", matched.len())).color(p.muted));
                }
            }
            self.workspace_filter_pill(ui, cx);
            self.duplicates_badge(ui, cx, acts);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.view_menu(ui, cx, acts);
                self.hibernate_menu(ui, cx, rc, matched, acts);
                if ui.button("＋ Workspace").on_hover_text("Create a workspace").clicked() {
                    self.start_new_workspace(cx);
                }
            });
        });
    }

    /// "in: Work ✕" next to the counter while results are limited to workspaces.
    fn workspace_filter_pill(&mut self, ui: &mut egui::Ui, cx: &ViewCtx) {
        if self.opts.workspaces.is_empty() {
            return;
        }
        let p = theme::palette(ui);
        let names: Vec<String> =
            self.opts.workspaces.iter().map(|id| cx.state.workspace(*id).map_or("no workspace".to_string(), |w| w.label())).collect();
        let resp = pill(ui, &format!("in {}  ✕", names.join(", ")), p.accent_hover, p.accent_soft(), Sense::click());
        if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Search all workspaces").clicked() {
            self.opts.workspaces.clear();
        }
    }

    /// Grouping and density of the list (saved in the settings).
    fn view_menu(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        let mut look = cx.state.settings.tab_list;
        ui.menu_button(format!("View: {} ▾", look.grouping.label().to_lowercase()), |ui| {
            ui.set_min_width(200.0);
            ui.label(RichText::new("Group tabs by").small().color(p.muted));
            for g in TabGrouping::ALL {
                if ui.radio(look.grouping == g, g.label()).clicked() {
                    look.grouping = g;
                }
            }
            ui.separator();
            ui.checkbox(&mut look.compact, "Compact rows");
            ui.checkbox(&mut look.show_labels, "Show tags & groups");
        })
        .response
        .on_hover_text("How tabs are listed");
        if look != cx.state.settings.tab_list {
            let mut settings = cx.state.settings.clone();
            settings.tab_list = look;
            acts.send(Command::UpdateSettings { settings });
        }
    }

    fn hibernate_menu(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, rc: &RowCtx, matched: &[&TabEntry], acts: &mut Actions) {
        let p = theme::palette(ui);
        let filtering = !self.query.trim().is_empty() || !self.opts.groups.is_empty() || !self.opts.workspaces.is_empty();
        ui.menu_button("💤 ▾", |ui| {
            ui.set_min_width(280.0);
            let hibernate = |acts: &mut Actions, target: HibernateTarget| acts.send(Command::Hibernate { target, force: false });
            if ui
                .button("Hibernate all tabs")
                .on_hover_text("Every open tab in the browser, except active and pinned tabs and tabs playing media")
                .clicked()
            {
                hibernate(acts, HibernateTarget::All);
                ui.close();
            }
            if ui.button("Hibernate this session's tabs").clicked() {
                hibernate(acts, HibernateTarget::Session(cx.viewed));
                ui.close();
            }
            let loaded: Vec<&TabEntry> = matched.iter().copied().filter(|e| e.is_open() && !e.discarded).collect();
            if filtering
                && ui
                    .add_enabled(!loaded.is_empty(), egui::Button::new(format!("Hibernate the {} matching tab(s)", loaded.len())))
                    .clicked()
            {
                hibernate(acts, HibernateTarget::Entries(rc.refs(&loaded)));
                ui.close();
            }
            ui.menu_button("Hibernate tabs not used for…", |ui| {
                for (label, d) in [("15 minutes", "15m"), ("1 hour", "1h"), ("1 day", "1d"), ("1 week", "1w")] {
                    let idle = idle_entries(cx, d);
                    if ui.add_enabled(!idle.is_empty(), egui::Button::new(format!("{label} ({})", idle.len()))).clicked() {
                        hibernate(acts, HibernateTarget::Entries(idle));
                        ui.close();
                    }
                }
                ui.label(RichText::new("Saved tabs of every session. Search idle:2h for any duration.").small().color(p.muted));
            });
            ui.separator();
            if ui
                .add(danger_button(ui, "Force hibernate all tabs…"))
                .on_hover_text("Also pinned tabs and tabs playing media. Active tabs can't be hibernated.")
                .clicked()
            {
                acts.confirm(
                    "Hibernate every tab except the active ones, including pinned tabs and tabs playing media? \
                     They stay in the tab strip and reload when you select them.",
                    Command::Hibernate { target: HibernateTarget::All, force: true },
                );
                ui.close();
            }
            ui.label(RichText::new("Hibernated tabs stay open and reload when selected.").small().color(p.muted));
        })
        .response
        .on_hover_text("Hibernate (unload) tabs to free memory");
    }

    /// "⧉ N duplicates" menu, shown when the listed tabs contain duplicates.
    fn duplicates_badge(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        let scope = if self.all_sessions { Scope::All } else { Scope::Session(cx.viewed) };
        let extra: usize = super::duplicates(cx.state, scope, &cx.state.settings.dedupe).iter().map(|g| g.remove.len()).sum();
        if extra == 0 {
            return;
        }
        let label = RichText::new(format!("⧉ {extra} duplicate{}", if extra == 1 { "" } else { "s" })).color(p.warn);
        ui.menu_button(label, |ui| {
            ui.set_min_width(240.0);
            if ui.button("Show them").on_hover_text("Search for is:duplicate").clicked() {
                self.query = "is:duplicate".into();
                ui.close();
            }
            if ui.add(primary_button(ui, format!("Remove {extra} and close their tabs"))).clicked() {
                acts.send(Command::RemoveDuplicates { scope, close_tabs: true });
                ui.close();
            }
            if ui.button("Remove from the session only").clicked() {
                acts.send(Command::RemoveDuplicates { scope, close_tabs: false });
                ui.close();
            }
            ui.label(RichText::new("Comparison options are in Organize.").small().color(p.muted));
        })
        .response
        .on_hover_text("Tabs showing the same page");
    }

    fn workspace_filter(&mut self, ui: &mut egui::Ui, cx: &ViewCtx) {
        if cx.state.workspaces.is_empty() {
            return;
        }
        let label = if self.opts.workspaces.is_empty() {
            "In: all workspaces".to_string()
        } else {
            format!("In: {} workspace(s)", self.opts.workspaces.len())
        };
        ui.menu_button(label, |ui| {
            ui.set_min_width(200.0);
            if ui.button("All workspaces").clicked() {
                self.opts.workspaces.clear();
            }
            ui.separator();
            let mut toggle = |ui: &mut egui::Ui, id: Id, label: &str| {
                let mut on = self.opts.workspaces.contains(&id);
                if ui.checkbox(&mut on, label).changed() {
                    if on {
                        self.opts.workspaces.insert(id);
                    } else {
                        self.opts.workspaces.remove(&id);
                    }
                }
            };
            for w in &cx.state.workspaces {
                toggle(ui, w.id, &w.label());
            }
            toggle(ui, NO_WORKSPACE, "(no workspace)");
        })
        .response
        .on_hover_text("Only search in these workspaces");
    }

    fn group_filter(&mut self, ui: &mut egui::Ui, cx: &ViewCtx) {
        let groups = cx.state.all_groups();
        let label =
            if self.opts.groups.is_empty() { "In: all groups".to_string() } else { format!("In: {} group(s)", self.opts.groups.len()) };
        ui.menu_button(label, |ui| {
            ui.set_min_width(180.0);
            if ui.button("Any group").clicked() {
                self.opts.groups.clear();
            }
            ui.separator();
            let mut toggle = |ui: &mut egui::Ui, key: &str, label: &str| {
                let mut on = self.opts.groups.contains(key);
                if ui.checkbox(&mut on, label).changed() {
                    if on {
                        self.opts.groups.insert(key.to_string());
                    } else {
                        self.opts.groups.remove(key);
                    }
                }
            };
            toggle(ui, "", "(no group)");
            for g in &groups {
                toggle(ui, g, g);
            }
        })
        .response
        .on_hover_text("Only search in these groups");
    }

    /// Actions on several tabs: the selected ones (`selected`), or those matching the search.
    /// `listed` is every tab listed (for "Select all").
    #[allow(clippy::too_many_arguments)]
    fn bulk_bar(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ViewCtx,
        refs: &[EntryRef],
        matched: &[&TabEntry],
        listed: &[EntryRef],
        selected: bool,
        acts: &mut Actions,
    ) {
        let p = theme::palette(ui);
        let n = refs.len();
        let these = if selected { "selected" } else { "matching" };
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(p.accent_soft())
            .stroke(Stroke::new(1.0, p.accent.gamma_multiply(0.35)))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{n} {these}")).color(p.accent_hover).strong());
                    if selected {
                        if n < listed.len() && ui.button(format!("Select all {}", listed.len())).on_hover_text("Ctrl+A").clicked() {
                            self.selection.extend(listed.iter().copied());
                        }
                        if ui.button("✕ Clear").on_hover_text("Clear the selection (Esc)").clicked() {
                            self.selection.clear();
                        }
                    } else if ui
                        .button("☑ Select them")
                        .on_hover_text("Select the matching tabs, to pick some out or act on them")
                        .clicked()
                    {
                        self.selection.extend(refs.iter().copied());
                    }
                    ui.add(text_input(&mut self.bulk_tag, "tag", 90.0));
                    let tag_ok = !self.bulk_tag.trim().is_empty();
                    if ui.add_enabled(tag_ok, egui::Button::new("+ Tag")).clicked() {
                        for tag in parse_list(&self.bulk_tag) {
                            acts.send(Command::TagEntries { entries: refs.to_vec(), tag, add: true });
                        }
                    }
                    if ui.add_enabled(tag_ok, egui::Button::new("− Tag")).clicked() {
                        for tag in parse_list(&self.bulk_tag) {
                            acts.send(Command::TagEntries { entries: refs.to_vec(), tag, add: false });
                        }
                    }
                    ui.add(text_input(&mut self.bulk_group, "group", 90.0));
                    if ui.add_enabled(!self.bulk_group.trim().is_empty(), egui::Button::new("Set group")).clicked() {
                        acts.send(Command::SetGroup { entries: refs.to_vec(), group: Some(self.bulk_group.trim().to_string()) });
                    }
                    ui.menu_button("Workspace ▾", |ui| {
                        ui.set_min_width(200.0);
                        workspace_menu(ui, cx, refs, acts);
                    })
                    .response
                    .on_hover_text(format!("Move the {these} tabs to a workspace"));
                    let in_ws: Vec<EntryRef> = matched.iter().zip(refs).filter(|(e, _)| e.workspace.is_some()).map(|(_, r)| *r).collect();
                    if !in_ws.is_empty()
                        && ui
                            .button(format!("◌ Out of workspace ({})", in_ws.len()))
                            .on_hover_text("Take them out of their workspace (they stay open)")
                            .clicked()
                    {
                        acts.send(Command::AssignWorkspace { entries: in_ws, workspace: None });
                    }
                    let closed: Vec<EntryRef> = matched.iter().zip(refs).filter(|(e, _)| !e.is_open()).map(|(_, r)| *r).collect();
                    if !closed.is_empty()
                        && ui
                            .button(format!("⤓ Open {}", closed.len()))
                            .on_hover_text("Open the closed ones, unloaded and in the background")
                            .clicked()
                    {
                        acts.send(Command::OpenEntries { entries: closed, discarded: true });
                    }
                    // `refs` and `matched` are parallel.
                    let loaded: Vec<EntryRef> =
                        matched.iter().zip(refs).filter(|(e, _)| e.is_open() && !e.discarded).map(|(_, r)| *r).collect();
                    if ui
                        .add_enabled(!loaded.is_empty(), egui::Button::new(format!("💤 Hibernate {}", loaded.len())))
                        .on_hover_text(format!("Unload the {these} open tabs (active, pinned and playing tabs are skipped)"))
                        .clicked()
                    {
                        acts.send(Command::Hibernate { target: HibernateTarget::Entries(loaded), force: false });
                    }
                    ui.menu_button("More ▾", |ui| {
                        ui.set_min_width(200.0);
                        if ui.button("Remove from group").clicked() {
                            acts.send(Command::SetGroup { entries: refs.to_vec(), group: None });
                            ui.close();
                        }
                        ui.menu_button("Move to session", |ui| {
                            for s in &cx.state.sessions {
                                if ui.button(&s.name).clicked() {
                                    acts.send(Command::MoveEntries { entries: refs.to_vec(), target: s.id });
                                    ui.close();
                                }
                            }
                        });
                        ui.separator();
                        if ui.button("Remove from session…").clicked() {
                            acts.confirm(
                                format!("Remove {n} tab(s) from the session? Open tabs stay open but are no longer tracked."),
                                Command::RemoveEntries { entries: refs.to_vec(), close_tabs: false },
                            );
                            ui.close();
                        }
                        let open = matched.iter().filter(|e| e.is_open()).count();
                        if ui.add_enabled(open > 0, danger_button(ui, format!("Close {open} open tab(s)…"))).clicked() {
                            acts.confirm(
                                format!("Close {open} open tab(s) and remove all {n} {these} tab(s) from the session?"),
                                Command::RemoveEntries { entries: refs.to_vec(), close_tabs: true },
                            );
                            ui.close();
                        }
                    });
                });
            });
    }

    // ------------------------------------------------------------ rows

    fn row(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, rc: &RowCtx, e: &TabEntry, acts: &mut Actions) {
        let p = theme::palette(ui);
        let r = rc.entry_ref(e);
        let look = cx.state.settings.tab_list;
        let editing = self.editing == Some(r);
        let open = e.is_open();
        let height = if look.compact { COMPACT_ROW_HEIGHT } else { ROW_HEIGHT };

        // The whole row is the click (and drag) target; its contents are drawn on top
        // and only the small action buttons take clicks themselves.
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click_and_drag());
        let selected = self.selection.contains(&r);
        if resp.drag_started() {
            let title =
                if selected && self.selection.len() > 1 { format!("{} tabs", self.selection.len()) } else { e.display_title().to_string() };
            resp.dnd_set_drag_payload(DraggedTab { entry: r, title });
        }
        self.order.push(r);
        if !ui.is_rect_visible(rect) {
            if editing {
                self.editor(ui, cx, e, r, acts);
            }
            return;
        }
        let hovered = ui.rect_contains_pointer(rect) && !egui::DragAndDrop::has_any_payload(ui.ctx());
        let dragged = resp.dragged();
        let fill = if selected {
            p.accent_soft()
        } else if hovered || dragged {
            p.surface_hover
        } else {
            p.surface
        };
        let stroke = if editing {
            Stroke::new(1.0, p.accent)
        } else if selected {
            Stroke::new(1.0, p.accent.gamma_multiply(0.5))
        } else {
            Stroke::new(1.0, Color32::TRANSPARENT)
        };
        ui.painter().rect(rect, if look.compact { 8.0 } else { 10.0 }, fill, stroke, egui::StrokeKind::Inside);

        let mut row =
            ui.new_child(UiBuilder::new().max_rect(rect.shrink2(egui::vec2(10.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
        // The checkbox shows while selecting, or on the row under the pointer.
        if hovered || !self.selection.is_empty() {
            let mut checked = selected;
            if row.checkbox(&mut checked, "").on_hover_text("Select (or Ctrl-click the row; Shift-click for a range)").clicked() {
                let shift = row.input(|i| i.modifiers.shift);
                self.click_select(r, shift);
            }
        } else {
            row.add_space(row.spacing().icon_width + row.spacing().item_spacing.x);
        }
        let host = e.host();
        let playing = e.tab_id.is_some_and(|t| rc.playing.contains(&t));
        let paused = e.tab_id.is_some_and(|t| cx.state.media.paused.contains(&t));
        let av = avatar_sized(&mut row, &host, !open || e.discarded, if look.compact { 22.0 } else { 30.0 });
        status_dot(&row, av, tab_state_color(&row, e));
        row.add_space(4.0);
        row.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if hovered || editing {
                if icon_button(ui, "🗑", "Remove from session (the tab stays open)").clicked() {
                    acts.send(Command::RemoveEntries { entries: vec![r], close_tabs: false });
                }
                if let Some(ws) = e.workspace.and_then(|id| cx.state.workspace(id))
                    && icon_button(ui, "◌", &format!("Take it out of {} (it stays open)", ws.name)).clicked()
                {
                    acts.send(Command::AssignWorkspace { entries: vec![r], workspace: None });
                }
                if open {
                    if icon_button(ui, "🗙", "Close tab (also removes it from the session)").clicked() {
                        acts.send(Command::RemoveEntries { entries: vec![r], close_tabs: true });
                    }
                    if matches!(crate::url::scheme(&e.url).as_str(), "http" | "https" | "file")
                        && icon_button(ui, "🔬", "Inspect this page").clicked()
                    {
                        acts.send(Command::OpenInspector { tab: e.tab_id });
                    }
                    if !e.discarded && icon_button(ui, "💤", "Hibernate: unload the tab, keeping it open").clicked() {
                        acts.send(Command::Hibernate { target: HibernateTarget::Entries(vec![r]), force: true });
                    }
                } else if icon_button(ui, "⤓", "Open in background without loading").clicked() {
                    acts.send(Command::OpenEntry { entry: r, discarded: true });
                }
                let edit_color = editing.then_some(p.accent_hover);
                if icon_button_colored(ui, "✏", "Edit tags, group & workspace", edit_color).clicked() {
                    if editing {
                        self.editing = None;
                    } else {
                        self.editing = Some(r);
                        self.edit_tag.clear();
                        self.edit_group = e.group.clone().unwrap_or_default();
                    }
                }
            }
            if look.show_labels {
                for tag in e.tags.iter().rev() {
                    chip(ui, tag, ChipKind::Tag);
                }
                if let Some(g) = &e.group {
                    chip(ui, g, ChipKind::Group);
                }
            }
            if look.grouping != TabGrouping::Workspace
                && let Some(ws) = e.workspace.and_then(|id| cx.state.workspace(id))
            {
                workspace_chip(ui, ws);
            }
            if playing {
                ui.label(RichText::new("🔊").color(p.success)).on_hover_text("Playing media");
            } else if paused {
                ui.label(RichText::new("⏸").color(p.warn)).on_hover_text("Paused by the background media limit");
            }

            let title_color = if open && !e.discarded { p.text } else { p.muted };
            let mut meta = host.clone();
            if !open {
                meta.push_str("  ·  unloaded");
            } else if e.discarded {
                meta.push_str("  ·  hibernated");
            }
            if e.pinned {
                meta.push_str("  ·  pinned");
            }
            if let Some(n) = self.all_sessions.then(|| rc.session_names[&r.session]) {
                meta.push_str(&format!("  ·  {n}"));
            }
            if look.compact {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(e.display_title()).color(title_color)).truncate());
                    ui.add(egui::Label::new(RichText::new(meta).small().color(p.muted)).truncate());
                });
            } else {
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add_space((ROW_HEIGHT - 34.0) / 2.0);
                    ui.add(egui::Label::new(RichText::new(e.display_title()).color(title_color)).truncate());
                    ui.add(egui::Label::new(RichText::new(meta).small().color(p.muted)).truncate());
                });
            }
        });

        let action = if !open {
            "open it"
        } else if e.discarded {
            "wake it up"
        } else {
            "switch to this tab"
        };
        let resp = resp
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(format!("{}\n\nClick to {action} · Ctrl-click to select · drag onto a workspace to move it", e.url));
        if resp.clicked() {
            let mods = ui.input(|i| i.modifiers);
            if mods.command || mods.shift {
                self.click_select(r, mods.shift);
            } else {
                acts.send(Command::OpenEntry { entry: r, discarded: false });
            }
        }

        if editing {
            self.editor(ui, cx, e, r, acts);
        }
    }

    /// Toggles a row's selection, or (`range`) selects the rows from the last one clicked to it.
    fn click_select(&mut self, r: EntryRef, range: bool) {
        let pos = |x: EntryRef| self.prev_order.iter().position(|o| *o == x);
        if range
            && let Some(a) = self.anchor
            && let (Some(i), Some(j)) = (pos(a), pos(r))
        {
            let span: Vec<EntryRef> = self.prev_order[i.min(j)..=i.max(j)].to_vec();
            self.selection.extend(span);
        } else if !self.selection.remove(&r) {
            self.selection.insert(r);
        }
        self.anchor = Some(r);
    }

    fn editor(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, e: &TabEntry, r: EntryRef, acts: &mut Actions) {
        let p = theme::palette(ui);
        egui::Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(RADIUS + 2)
            .inner_margin(Margin::same(12))
            .outer_margin(Margin { left: 40, right: 0, top: 0, bottom: 4 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Tags").color(p.muted));
                    for tag in &e.tags {
                        if chip(ui, tag, ChipKind::Tag)
                            .interact(Sense::click())
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .on_hover_text("Click to remove")
                            .clicked()
                        {
                            acts.send(Command::TagEntries { entries: vec![r], tag: tag.clone(), add: false });
                        }
                    }
                    let resp = ui.add(text_input(&mut self.edit_tag, "add tags, comma separated", 180.0));
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Add").clicked() || enter {
                        for tag in parse_list(&std::mem::take(&mut self.edit_tag)) {
                            acts.send(Command::TagEntries { entries: vec![r], tag, add: true });
                        }
                    }
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Group").color(p.muted));
                    let resp = ui.add(text_input(&mut self.edit_group, "group name", 140.0));
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let groups = cx.state.all_groups();
                    if !groups.is_empty() {
                        ui.menu_button("▾", |ui| {
                            for g in &groups {
                                if ui.button(g).clicked() {
                                    self.edit_group = g.clone();
                                    ui.close();
                                }
                            }
                        });
                    }
                    if ui.add(primary_button(ui, "Set")).clicked() || enter {
                        let g = self.edit_group.trim();
                        acts.send(Command::SetGroup { entries: vec![r], group: (!g.is_empty()).then(|| g.to_string()) });
                    }
                    if e.group.is_some() && ui.button("Ungroup").clicked() {
                        self.edit_group.clear();
                        acts.send(Command::SetGroup { entries: vec![r], group: None });
                    }
                    ui.menu_button("Move to session ▾", |ui| {
                        for s in cx.state.sessions.iter().filter(|s| s.id != r.session) {
                            if ui.button(&s.name).clicked() {
                                acts.send(Command::MoveEntries { entries: vec![r], target: s.id });
                                self.editing = None;
                                ui.close();
                            }
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Workspace").color(p.muted));
                    let current = e.workspace.and_then(|id| cx.state.workspace(id));
                    let label = current.map_or("None".to_string(), |w| w.label());
                    ui.menu_button(format!("{label} ▾"), |ui| {
                        ui.set_min_width(200.0);
                        workspace_menu(ui, cx, &[r], acts);
                    });
                    let how = match (current.is_some(), e.workspace_manual) {
                        (_, true) => "placed by hand",
                        (true, false) => "placed by a rule",
                        (false, false) => "no rule matches",
                    };
                    ui.label(RichText::new(how).small().color(p.muted));
                });
                if let Some(g) = &e.browser_group {
                    ui.label(RichText::new(format!("In the browser tab group \"{g}\"")).small().color(p.muted));
                }
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(&e.url).small().color(p.muted)).truncate());
            });
    }
}

/// Workspace choices for some entries (menu contents).
fn workspace_menu(ui: &mut egui::Ui, cx: &ViewCtx, refs: &[EntryRef], acts: &mut Actions) {
    for w in &cx.state.workspaces {
        let text = RichText::new(w.label()).color(ws_color(w.color));
        if ui.button(text).clicked() {
            acts.send(Command::AssignWorkspace { entries: refs.to_vec(), workspace: Some(w.id) });
            ui.close();
        }
    }
    if !cx.state.workspaces.is_empty() {
        ui.separator();
    }
    if ui.button("No workspace").clicked() {
        acts.send(Command::AssignWorkspace { entries: refs.to_vec(), workspace: None });
        ui.close();
    }
    if ui.button("⚡ Let the rules decide").on_hover_text("Undo a choice made by hand").clicked() {
        acts.send(Command::AutoPlaceEntries { entries: Some(refs.to_vec()) });
        ui.close();
    }
}

/// Loaded tabs (of every session) not used for `duration` (e.g. "1h").
fn idle_entries(cx: &ViewCtx, duration: &str) -> Vec<EntryRef> {
    let Ok(q) = Query::parse(&format!("idle:{duration} is:open -is:hibernated"), &SearchOptions::default()) else {
        return Vec::new();
    };
    let q = q.with_context(HashMap::new(), HashSet::new(), pf::now());
    cx.state
        .sessions
        .iter()
        .flat_map(|s| s.entries.iter().filter(|e| q.matches(e)).map(move |e| EntryRef { session: s.id, entry: e.id }))
        .collect()
}

fn no_tabs(ui: &mut egui::Ui, total: usize) {
    if total == 0 {
        empty_state(ui, "🗂", "No tabs in this session yet", "Tabs you open while this session is active appear here.");
    } else {
        empty_state(ui, "🔍", "No tabs match", "Try a different search or clear the filters.");
    }
}
