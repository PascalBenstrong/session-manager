//! Cookies & site data view: ad-hoc cleanup, cleanup rules (interval / on-set) and log.

use eframe::egui::{self, RichText};

use super::theme;
use super::widgets::{
    danger_button, empty_state, format_time, icon_button, parse_list, pattern_editor, pattern_error, primary_button, section_card,
    text_input,
};
use super::{Actions, ViewCtx};
use crate::cleanup::{CleanupRule, CleanupSpec, CleanupTarget, DataTypes};
use crate::model::Id;
use crate::pattern::{PatternKind, SitePattern};
use crate::protocol::Command;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum TargetKind {
    #[default]
    Pattern,
    OpenTabs,
    AllDomains,
}

pub struct CleanupView {
    target: TargetKind,
    pattern: SitePattern,
    types: DataTypes,
    keep: String,
    rule_id: Id,
    rule_name: String,
    use_interval: bool,
    interval: u32,
    on_cookie_set: bool,
}

impl Default for CleanupView {
    fn default() -> Self {
        Self {
            target: TargetKind::Pattern,
            pattern: SitePattern::default(),
            types: DataTypes::default(),
            keep: String::new(),
            rule_id: 0,
            rule_name: String::new(),
            use_interval: false,
            interval: 60,
            on_cookie_set: false,
        }
    }
}

impl CleanupView {
    fn spec(&self) -> CleanupSpec {
        CleanupSpec {
            target: match self.target {
                TargetKind::Pattern => CleanupTarget::Pattern(self.pattern.clone()),
                TargetKind::OpenTabs => CleanupTarget::OpenTabs,
                TargetKind::AllDomains => CleanupTarget::AllDomains,
            },
            data_types: self.types,
            exclude: parse_list(&self.keep).into_iter().map(|d| SitePattern::new(PatternKind::Domain, d)).collect(),
        }
    }

    fn load_rule(&mut self, r: &CleanupRule) {
        match &r.spec.target {
            CleanupTarget::Pattern(p) => {
                self.target = TargetKind::Pattern;
                self.pattern = p.clone();
            }
            // `Pages` is only used for one-off tab clean-ups, never saved as a rule.
            CleanupTarget::OpenTabs | CleanupTarget::Pages(_) => self.target = TargetKind::OpenTabs,
            CleanupTarget::AllDomains => self.target = TargetKind::AllDomains,
        }
        self.types = r.spec.data_types;
        self.keep = r.spec.exclude.iter().map(|p| p.value.clone()).collect::<Vec<_>>().join(", ");
        self.rule_id = r.id;
        self.rule_name = r.name.clone();
        self.use_interval = r.interval_minutes.is_some();
        self.interval = r.interval_minutes.unwrap_or(60);
        self.on_cookie_set = r.on_cookie_set;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            self.quick_actions(ui, acts);
            self.editor(ui, cx, acts);
            self.rules(ui, cx, acts);
            self.log(ui, cx, acts);
        });
    }

    fn quick_actions(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let subtitle = "Remove cookies, cache and site storage in one click. You will be logged out of affected sites. \
                        Open tabs of those sites are cleared from inside too (session storage included); reload them so \
                        nothing a page still holds in memory is saved again.";
        section_card(ui, "Quick clean-up", subtitle, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("🗂  Clean all open tabs' sites")
                    .on_hover_text("Cookies and site data of every site with an open tab")
                    .clicked()
                {
                    acts.confirm(
                        "Remove cookies and site data for every site that has an open tab? You will be logged out of them.",
                        Command::RunCleanup {
                            label: "Open tabs".into(),
                            spec: CleanupSpec { target: CleanupTarget::OpenTabs, ..Default::default() },
                        },
                    );
                }
                if ui.add(danger_button(ui, "🌐  Clean all domains")).clicked() {
                    acts.confirm(
                        "Remove ALL cookies, cache and site data for every site?",
                        Command::RunCleanup { label: "All domains".into(), spec: CleanupSpec::default() },
                    );
                }
            });
        });
    }

    fn editor(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        let title = if self.rule_id == 0 { "Custom clean-up" } else { "Edit rule" };
        let subtitle = "Pick what to clean and where. Run it now, or save it as a rule that runs automatically.";
        section_card(ui, title, subtitle, |ui| {
            egui::Grid::new("cleanup-form").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label(RichText::new("Target").color(p.muted));
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut self.target, TargetKind::Pattern, "Host / URL / pattern");
                    ui.radio_value(&mut self.target, TargetKind::OpenTabs, "All open tabs");
                    ui.radio_value(&mut self.target, TargetKind::AllDomains, "All domains");
                });
                ui.end_row();

                if self.target == TargetKind::Pattern {
                    ui.label(RichText::new("Pattern").color(p.muted));
                    ui.vertical(|ui| {
                        ui.horizontal_wrapped(|ui| pattern_editor(ui, "cleanup-pattern", &mut self.pattern, 240.0));
                        if let Some(err) = pattern_error(&self.pattern) {
                            ui.colored_label(p.danger, err);
                        }
                    });
                    ui.end_row();
                }

                ui.label(RichText::new("Data").color(p.muted));
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.types.cookies, "Cookies");
                    ui.checkbox(&mut self.types.cache, "Cache").on_hover_text(if cx.caps.per_host_cache {
                        "HTTP cache and Cache Storage"
                    } else {
                        "This browser can only clear the cache for all domains at once; per-site cache clearing is skipped"
                    });
                    ui.checkbox(&mut self.types.local_storage, "Local & session storage");
                    ui.checkbox(&mut self.types.indexed_db, "IndexedDB");
                    ui.checkbox(&mut self.types.service_workers, "Service workers");
                });
                ui.end_row();

                ui.label(RichText::new("Keep").color(p.muted));
                let width = ui.available_width().min(420.0);
                ui.add(text_input(&mut self.keep, "domains never cleaned, comma separated (e.g. github.com)", width));
                ui.end_row();

                ui.label(RichText::new("Automate").color(p.muted));
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut self.use_interval, "Run every");
                        ui.add_enabled(self.use_interval, egui::DragValue::new(&mut self.interval).range(1..=10080).suffix(" min"));
                    });
                    ui.add_enabled(
                        self.types.cookies,
                        egui::Checkbox::new(&mut self.on_cookie_set, "Remove matching cookies as soon as a site sets them"),
                    )
                    .on_disabled_hover_text("Requires the Cookies data type");
                });
                ui.end_row();

                ui.label(RichText::new("Rule name").color(p.muted));
                ui.add(text_input(&mut self.rule_name, "optional", 200.0));
                ui.end_row();
            });

            let spec = self.spec();
            let valid = !self.types.is_empty() && (self.target != TargetKind::Pattern || self.pattern.compile().is_ok());
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(valid, primary_button(ui, "🧹  Clean now")).clicked() {
                    let label = spec.target.describe();
                    let cmd = Command::RunCleanup { label: label.clone(), spec: spec.clone() };
                    if self.target == TargetKind::Pattern {
                        acts.send(cmd);
                    } else {
                        acts.confirm(format!("Remove {} for {label}?", self.types.describe()), cmd);
                    }
                }
                let label = if self.rule_id == 0 { "Save as rule" } else { "Update rule" };
                if ui.add_enabled(valid, egui::Button::new(label)).clicked() {
                    let enabled = cx.state.cleanup_rules.iter().find(|r| r.id == self.rule_id).is_none_or(|r| r.enabled);
                    acts.send(Command::SaveCleanupRule {
                        rule: CleanupRule {
                            id: self.rule_id,
                            name: self.rule_name.trim().to_string(),
                            enabled,
                            spec,
                            interval_minutes: self.use_interval.then_some(self.interval),
                            on_cookie_set: self.on_cookie_set && self.types.cookies,
                            last_run: None,
                        },
                    });
                    *self = Self::default();
                }
                if self.rule_id != 0 && ui.button("Cancel").clicked() {
                    *self = Self::default();
                }
            });
        });
    }

    fn rules(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        section_card(ui, "Rules", "Saved clean-ups. Toggle, run or edit them here.", |ui| {
            if cx.state.cleanup_rules.is_empty() {
                empty_state(ui, "⏱", "No rules yet", "Configure a clean-up above and press \"Save as rule\".");
                ui.add_space(8.0);
            }
            for rule in &cx.state.cleanup_rules {
                egui::Frame::new().fill(p.surface_hover).corner_radius(theme::RADIUS).inner_margin(egui::Margin::symmetric(10, 6)).show(
                    ui,
                    |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let mut enabled = rule.enabled;
                            if ui.checkbox(&mut enabled, "").on_hover_text("Enabled").changed() {
                                acts.send(Command::SaveCleanupRule { rule: CleanupRule { enabled, ..rule.clone() } });
                            }
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(RichText::new(&rule.name).strong());
                                let mut triggers = Vec::new();
                                if let Some(m) = rule.interval_minutes {
                                    triggers.push(format!("every {m} min"));
                                }
                                if rule.on_cookie_set {
                                    triggers.push("on cookie set".into());
                                }
                                if triggers.is_empty() {
                                    triggers.push("manual".into());
                                }
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {} · {} · last run {}",
                                        rule.spec.target.describe(),
                                        rule.spec.data_types.describe(),
                                        triggers.join(", "),
                                        rule.last_run.map_or("never".into(), format_time)
                                    ))
                                    .small()
                                    .color(p.muted),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if icon_button(ui, "🗑", "Delete rule").clicked() {
                                    acts.send(Command::DeleteCleanupRule { id: rule.id });
                                }
                                if icon_button(ui, "✏", "Edit rule").clicked() {
                                    self.load_rule(rule);
                                }
                                if icon_button(ui, "▶", "Run now").clicked() {
                                    acts.send(Command::RunCleanupRule { id: rule.id });
                                }
                            });
                        });
                    },
                );
                ui.add_space(2.0);
            }
        });
    }

    fn log(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        section_card(ui, "Activity", "", |ui| {
            if cx.state.cleanup_log.is_empty() {
                ui.label(RichText::new("Nothing cleaned yet.").color(p.muted));
                return;
            }
            for r in &cx.state.cleanup_log {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("●").small().color(if r.warnings.is_empty() { p.success } else { p.warn }));
                    let resp = ui.label(r.summary());
                    if !r.hosts.is_empty() {
                        resp.on_hover_text(r.hosts.join("\n"));
                    }
                    ui.label(RichText::new(format_time(r.at)).small().color(p.muted));
                });
                for w in &r.warnings {
                    ui.label(RichText::new(format!("      ⚠ {w}")).small().color(p.warn));
                }
            }
            ui.add_space(4.0);
            if ui.button("Clear activity").clicked() {
                acts.send(Command::ClearCleanupLog);
            }
        });
    }
}
