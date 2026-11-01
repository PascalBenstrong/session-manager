//! Organize view: re-order browser tabs, automatic organization and auto-tag rules.

use eframe::egui::{self, RichText};

use super::theme;
use super::widgets::{
    ChipKind, chip, empty_state, icon_button, parse_list, pattern_editor, pattern_error, primary_button, section_card, text_input,
};
use super::{Actions, ViewCtx};
use crate::dedupe::{Scope, find_duplicates};
use crate::model::AutoRule;
use crate::organize::OrganizeKey;
use crate::protocol::Command;

#[derive(Default)]
pub struct OrganizeView {
    key: OrganizeKey,
    native_groups: bool,
    draft: AutoRule,
    draft_tags: String,
    draft_group: String,
    dedupe_all_sessions: bool,
    /// Keep open duplicate tabs open (only remove them from the session).
    dedupe_leave_open: bool,
}

impl OrganizeView {
    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            self.duplicates(ui, cx, acts);
            self.reorganize(ui, cx, acts);
            self.auto_rules(ui, cx, acts);
        });
    }

    fn reorganize(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        section_card(
            ui,
            "Re-organize browser tabs",
            "Sort every window so tabs with the same key sit together. Pinned tabs stay first.",
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Sort by");
                    key_picker(ui, "organize-key", &mut self.key);
                    ui.add_enabled_ui(cx.caps.tab_groups, |ui| {
                        ui.checkbox(&mut self.native_groups, "Create native tab groups")
                            .on_disabled_hover_text("This browser does not expose tab groups to extensions");
                    });
                    if ui.add(primary_button(ui, "Organize now")).clicked() {
                        acts.send(Command::Organize { key: self.key, native_groups: self.native_groups && cx.caps.tab_groups });
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                let mut settings = cx.state.settings.clone();
                let mut auto = settings.auto_organize.is_some();
                ui.horizontal_wrapped(|ui| {
                    let mut changed = ui.checkbox(&mut auto, "Re-organize automatically when tabs change, by").changed();
                    let mut key = settings.auto_organize.unwrap_or(self.key);
                    ui.add_enabled_ui(auto, |ui| changed |= key_picker(ui, "auto-organize-key", &mut key));
                    if cx.caps.tab_groups {
                        changed |= ui.add_enabled(auto, egui::Checkbox::new(&mut settings.native_tab_groups, "with tab groups")).changed();
                    }
                    if changed {
                        settings.auto_organize = auto.then_some(key);
                        acts.send(Command::UpdateSettings { settings });
                    }
                });
            },
        );
    }

    fn auto_rules(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        section_card(
            ui,
            "Automatic tags & groups",
            "Tabs whose URL matches a rule get its tags, and its group if they don't have one yet.",
            |ui| {
                if cx.state.auto_rules.is_empty() {
                    empty_state(ui, "🏷", "No rules yet", "Create one below, e.g. github.com → group \"Dev\".");
                    ui.add_space(12.0);
                }
                for rule in &cx.state.auto_rules {
                    egui::Frame::new()
                        .fill(p.surface_hover)
                        .corner_radius(theme::RADIUS)
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let mut enabled = rule.enabled;
                                if ui.checkbox(&mut enabled, "").on_hover_text("Enabled").changed() {
                                    acts.send(Command::SaveAutoRule { rule: AutoRule { enabled, ..rule.clone() } });
                                }
                                ui.label(RichText::new(rule.pattern.describe()).monospace());
                                ui.label(RichText::new("→").color(p.muted));
                                if let Some(g) = &rule.group {
                                    chip(ui, g, ChipKind::Group);
                                }
                                for t in &rule.tags {
                                    chip(ui, t, ChipKind::Tag);
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if icon_button(ui, "🗑", "Delete rule").clicked() {
                                        acts.send(Command::DeleteAutoRule { id: rule.id });
                                    }
                                    if icon_button(ui, "✏", "Edit rule").clicked() {
                                        self.draft = rule.clone();
                                        self.draft_tags = rule.tags.iter().cloned().collect::<Vec<_>>().join(", ");
                                        self.draft_group = rule.group.clone().unwrap_or_default();
                                    }
                                });
                            });
                        });
                    ui.add_space(2.0);
                }

                ui.add_space(8.0);
                ui.label(RichText::new(if self.draft.id == 0 { "New rule" } else { "Edit rule" }).strong());
                ui.horizontal_wrapped(|ui| {
                    ui.label("When the URL matches");
                    pattern_editor(ui, "auto-rule-kind", &mut self.draft.pattern, 220.0);
                });
                if let Some(err) = pattern_error(&self.draft.pattern) {
                    ui.colored_label(p.danger, err);
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("add tags");
                    ui.add(text_input(&mut self.draft_tags, "work, docs", 150.0));
                    ui.label("and group");
                    ui.add(text_input(&mut self.draft_group, "optional", 120.0));
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let valid =
                        self.draft.pattern.compile().is_ok() && (!self.draft_tags.trim().is_empty() || !self.draft_group.trim().is_empty());
                    if ui.add_enabled(valid, primary_button(ui, "Save rule")).clicked() {
                        let group = self.draft_group.trim();
                        let rule = AutoRule {
                            enabled: true,
                            tags: parse_list(&self.draft_tags),
                            group: (!group.is_empty()).then(|| group.to_string()),
                            ..self.draft.clone()
                        };
                        acts.send(Command::SaveAutoRule { rule });
                        self.reset_draft();
                    }
                    if self.draft.id != 0 && ui.button("Cancel").clicked() {
                        self.reset_draft();
                    }
                    if ui
                        .button("Apply rules to existing tabs")
                        .on_hover_text("Rules apply automatically to new tabs and navigations; this re-applies them to every saved tab")
                        .clicked()
                    {
                        acts.send(Command::ApplyAutoRules);
                    }
                });
            },
        );
    }

    fn duplicates(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        section_card(
            ui,
            "Duplicate tabs",
            "Find tabs that show the same page. The open, most recently used copy is kept; tags and groups of the others are merged into it.",
            |ui| {
                let mut settings = cx.state.settings.clone();
                let o = &mut settings.dedupe;
                let mut changed = false;
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Treat as the same page when only these differ:").color(p.muted));
                });
                ui.horizontal_wrapped(|ui| {
                    changed |= ui.checkbox(&mut o.ignore_fragment, "#fragment").changed();
                    changed |= ui.checkbox(&mut o.ignore_query, "?query string").changed();
                    changed |= ui.checkbox(&mut o.ignore_trailing_slash, "trailing /").changed();
                    changed |= ui.checkbox(&mut o.ignore_www, "www.").changed();
                    changed |= ui.checkbox(&mut o.ignore_scheme, "http vs https").changed();
                });
                changed |= ui
                    .checkbox(
                        &mut settings.prevent_duplicates,
                        "Prevent duplicates: opening a page that's already in the active session switches to it instead",
                    )
                    .on_hover_text("If the page is saved but unloaded, the new tab simply becomes that saved tab.")
                    .changed();
                ui.add_space(2.0);
                ui.label(RichText::new("Automatically remove duplicates (closing the extra tabs):").color(p.muted));
                changed |= ui
                    .checkbox(&mut settings.dedupe_on_startup, "When the browser restarts and restores its tabs")
                    .on_hover_text(
                        "Restored tabs are matched to their saved tabs; a few seconds after the browser stops                          restoring tabs, duplicates are removed in every session.",
                    )
                    .changed();
                changed |= ui
                    .checkbox(&mut settings.dedupe_on_session_load, "When a session's tabs are reopened")
                    .on_hover_text("Before a session is loaded, its duplicates are removed so only one copy of each page opens.")
                    .changed();
                let opts = settings.dedupe;
                if changed {
                    acts.send(Command::UpdateSettings { settings });
                }

                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut self.dedupe_all_sessions, false, "This session");
                    ui.radio_value(&mut self.dedupe_all_sessions, true, "All sessions");
                    ui.checkbox(&mut self.dedupe_leave_open, "Leave duplicate tabs open (only remove them from the session)");
                });
                let scope = if self.dedupe_all_sessions { Scope::All } else { Scope::Session(cx.viewed) };
                let groups = find_duplicates(cx.state, scope, &opts);
                let extra: usize = groups.iter().map(|g| g.remove.len()).sum();
                ui.add_space(4.0);
                if groups.is_empty() {
                    ui.label(RichText::new("✔ No duplicate tabs.").color(p.success));
                    return;
                }
                ui.horizontal(|ui| {
                    let label = format!("Remove {extra} duplicate(s)");
                    if ui.add(primary_button(ui, label)).clicked() {
                        acts.send(Command::RemoveDuplicates { scope, close_tabs: !self.dedupe_leave_open });
                    }
                    ui.label(RichText::new(format!("{} page(s) have copies", groups.len())).color(p.muted));
                });
                egui::CollapsingHeader::new(format!("Show the {} page(s)", groups.len())).id_salt("dup-list").show(ui, |ui| {
                    for g in groups.iter().take(100) {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("×{}", g.remove.len() + 1)).strong().color(p.warn));
                            ui.add(egui::Label::new(RichText::new(&g.url).small()).truncate());
                        });
                    }
                    if groups.len() > 100 {
                        ui.label(RichText::new(format!("…and {} more", groups.len() - 100)).color(p.muted));
                    }
                });
            },
        );
    }

    fn reset_draft(&mut self) {
        self.draft = AutoRule::default();
        self.draft_tags.clear();
        self.draft_group.clear();
    }
}

fn key_picker(ui: &mut egui::Ui, id: &str, key: &mut OrganizeKey) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).selected_text(key.label()).width(110.0).show_ui(ui, |ui| {
        for k in OrganizeKey::ALL {
            changed |= ui.selectable_value(key, k, k.label()).changed();
        }
    });
    changed
}
