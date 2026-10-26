//! Tabs view: search, grouped listing, per-tab and bulk actions.

use std::collections::HashMap;

use eframe::egui::{self, Align, Color32, CursorIcon, Layout, Margin, RichText, Sense, Stroke, UiBuilder};

use super::theme::{self, RADIUS};
use super::widgets::{
    ChipKind, avatar, badge, chip, danger_button, empty_state, icon_button, icon_button_colored, parse_list, primary_button,
    status_dot, text_input,
};
use super::{Actions, ViewCtx};
use crate::dedupe::{Scope, duplicate_ids, find_duplicates};
use crate::model::{EntryRef, Id, TabEntry};
use crate::organize::{OrganizeKey, group_entries};
use crate::protocol::Command;
use crate::search::{Field, MatchMode, Query, SearchOptions};

const SEARCH_HELP: &str = "Terms are combined with AND. Quote phrases: \"foo bar\"\n\
\n\
Fields:   title:  url:  host:  domain:  tag:  group:\n\
Exact:    =word   or   ^word$\n\
Prefix:   ^word\n\
Suffix:   word$\n\
Glob:     *word?    Regex: /pattern/\n\
Negate:   -term   (e.g. -tag:done)\n\
State:    is:open  is:unloaded  is:pinned  is:grouped  is:ungrouped  is:tagged  is:duplicate\n\
\n\
Example:  host:^docs tag:work -is:unloaded \"release notes\"";

const ROW_HEIGHT: f32 = 48.0;

#[derive(Default)]
pub struct TabsView {
    query: String,
    opts: SearchOptions,
    all_sessions: bool,
    group_by: Option<OrganizeKey>,
    show_filters: bool,
    bulk_tag: String,
    bulk_group: String,
    editing: Option<EntryRef>,
    edit_tag: String,
    edit_group: String,
}

impl TabsView {
    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let state = cx.state;
        let query = Query::parse(&self.query, &self.opts);
        let query_error = query.as_ref().err().cloned();
        let mut query = query.unwrap_or_default();
        let scope = if self.all_sessions { Scope::All } else { Scope::Session(cx.viewed) };
        if query.wants_duplicates() {
            query = query.with_duplicates(duplicate_ids(state, scope, &state.settings.dedupe));
        }

        // Entry ids are globally unique, so map them back to their session.
        let mut owner: HashMap<Id, Id> = HashMap::new();
        let mut matched: Vec<&TabEntry> = Vec::new();
        let mut total = 0;
        for s in state.sessions.iter().filter(|s| self.all_sessions || s.id == cx.viewed) {
            for e in &s.entries {
                total += 1;
                owner.insert(e.id, s.id);
                if query.matches(e) {
                    matched.push(e);
                }
            }
        }
        let refs: Vec<EntryRef> = matched.iter().map(|e| EntryRef { session: owner[&e.id], entry: e.id }).collect();

        self.search_bar(ui, cx, matched.len(), total, query_error.as_deref(), acts);
        let filtering = !self.query.trim().is_empty() || !self.opts.groups.is_empty();
        if filtering && !matched.is_empty() && query_error.is_none() {
            self.bulk_bar(ui, cx, &refs, &matched, acts);
        }
        ui.add_space(6.0);

        let session_names: HashMap<Id, &str> = state.sessions.iter().map(|s| (s.id, s.name.as_str())).collect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if matched.is_empty() {
                if total == 0 {
                    empty_state(ui, "🗂", "No tabs in this session yet", "Tabs you open while this session is active appear here.");
                } else {
                    empty_state(ui, "🔍", "No tabs match", "Try a different search or clear the filters.");
                }
                return;
            }
            let mut row = |this: &mut Self, ui: &mut egui::Ui, e: &TabEntry| {
                let r = EntryRef { session: owner[&e.id], entry: e.id };
                let session_name = this.all_sessions.then(|| session_names[&r.session]);
                this.row(ui, cx, e, r, session_name, acts);
            };
            match self.group_by {
                None => {
                    for e in &matched {
                        row(self, ui, e);
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
                                    row(self, ui, e);
                                }
                            });
                        ui.add_space(4.0);
                    }
                }
            }
        });
    }

    fn search_bar(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, matched: usize, total: usize, error: Option<&str>, acts: &mut Actions) {
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
                                .hint_text("Search tabs — host:  tag:  group:  ^prefix  suffix$  =exact  /regex/  -not")
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
            });
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            match error {
                Some(err) => {
                    ui.label(RichText::new(err).color(p.danger));
                }
                None => {
                    ui.label(RichText::new(format!("{matched} of {total} tabs")).color(p.muted));
                }
            }
            self.duplicates_badge(ui, cx, acts);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::ComboBox::from_id_salt("group-by")
                    .selected_text(format!("Group: {}", self.group_by.map_or("none", |k| k.label())))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.group_by, None, "None");
                        for k in OrganizeKey::ALL {
                            ui.selectable_value(&mut self.group_by, Some(k), k.label());
                        }
                    });
                self.group_filter(ui, cx);
            });
        });
    }

    /// "⧉ N duplicates" menu, shown when the listed tabs contain duplicates.
    fn duplicates_badge(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        let scope = if self.all_sessions { Scope::All } else { Scope::Session(cx.viewed) };
        let extra: usize = find_duplicates(cx.state, scope, &cx.state.settings.dedupe).iter().map(|g| g.remove.len()).sum();
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

    fn group_filter(&mut self, ui: &mut egui::Ui, cx: &ViewCtx) {
        let groups = cx.state.all_groups();
        let label = if self.opts.groups.is_empty() {
            "In: all groups".to_string()
        } else {
            format!("In: {} group(s)", self.opts.groups.len())
        };
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

    fn bulk_bar(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, refs: &[EntryRef], matched: &[&TabEntry], acts: &mut Actions) {
        let p = theme::palette(ui);
        let n = refs.len();
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(p.accent_soft())
            .stroke(Stroke::new(1.0, p.accent.gamma_multiply(0.35)))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{n} matching")).color(p.accent_hover).strong());
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
                                format!("Close {open} open tab(s) and remove all {n} matching tab(s) from the session?"),
                                Command::RemoveEntries { entries: refs.to_vec(), close_tabs: true },
                            );
                            ui.close();
                        }
                    });
                });
            });
    }

    fn row(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, e: &TabEntry, r: EntryRef, session_name: Option<&str>, acts: &mut Actions) {
        let p = theme::palette(ui);
        let editing = self.editing == Some(r);
        let open = e.is_open();

        // The whole row is the click target; its contents are drawn on top and only
        // the small action buttons take clicks themselves.
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
        if !ui.is_rect_visible(rect) {
            if editing {
                self.editor(ui, cx, e, r, acts);
            }
            return;
        }
        let hovered = ui.rect_contains_pointer(rect);
        let fill = if hovered { p.surface_hover } else { p.surface };
        let stroke = if editing { Stroke::new(1.0, p.accent) } else { Stroke::new(1.0, Color32::TRANSPARENT) };
        ui.painter().rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);

        let mut row = ui.new_child(
            UiBuilder::new()
                .max_rect(rect.shrink2(egui::vec2(10.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let host = e.host();
        let av = avatar(&mut row, &host, !open);
        status_dot(&row, av, open);
        row.add_space(4.0);
        row.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if hovered || editing {
                if icon_button(ui, "🗑", "Remove from session (the tab stays open)").clicked() {
                    acts.send(Command::RemoveEntries { entries: vec![r], close_tabs: false });
                }
                if open {
                    if icon_button(ui, "🗙", "Close tab (also removes it from the session)").clicked() {
                        acts.send(Command::RemoveEntries { entries: vec![r], close_tabs: true });
                    }
                } else if icon_button(ui, "⤓", "Open in background without loading").clicked() {
                    acts.send(Command::OpenEntry { entry: r, discarded: true });
                }
                let edit_color = editing.then_some(p.accent_hover);
                if icon_button_colored(ui, "✏", "Edit tags & group", edit_color).clicked() {
                    if editing {
                        self.editing = None;
                    } else {
                        self.editing = Some(r);
                        self.edit_tag.clear();
                        self.edit_group = e.group.clone().unwrap_or_default();
                    }
                }
            }
            for tag in e.tags.iter().rev() {
                chip(ui, tag, ChipKind::Tag);
            }
            if let Some(g) = &e.group {
                chip(ui, g, ChipKind::Group);
            }

            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add_space((ROW_HEIGHT - 34.0) / 2.0);
                let title_color = if open { p.text } else { p.muted };
                ui.add(egui::Label::new(RichText::new(e.display_title()).color(title_color)).truncate());
                let mut meta = host.clone();
                if !open {
                    meta.push_str("  ·  unloaded");
                }
                if e.pinned {
                    meta.push_str("  ·  pinned");
                }
                if let Some(n) = session_name {
                    meta.push_str(&format!("  ·  {n}"));
                }
                ui.add(egui::Label::new(RichText::new(meta).small().color(p.muted)).truncate());
            });
        });

        let resp = resp
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(format!("{}\n\nClick to {}", e.url, if open { "switch to this tab" } else { "open it" }));
        if resp.clicked() {
            acts.send(Command::OpenEntry { entry: r, discarded: false });
        }

        if editing {
            self.editor(ui, cx, e, r, acts);
        }
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
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(&e.url).small().color(p.muted)).truncate());
            });
    }
}
