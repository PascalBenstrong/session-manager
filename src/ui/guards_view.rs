//! Page guards view: blocking ad scripts and ad tabs, keeping right-click and copying,
//! defusing developer tools traps, and the inspector.

use eframe::egui::{self, RichText};

use super::settings_view::setting;
use super::theme;
use super::widgets::{format_time, icon_button, primary_button, section_card, text_input};
use super::{Actions, ViewCtx};
use crate::guards::{BlockRule, BlockRuleKind, normalize_site};
use crate::protocol::Command;
use crate::url;

#[derive(Default)]
pub struct GuardsView {
    kind: BlockRuleKind,
    pattern: String,
    sites: String,
    exception: String,
}

impl GuardsView {
    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        let mut s = cx.state.settings.clone();
        let mut changed = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new("Guards run inside pages before the pages' own scripts. Changes apply to pages loaded afterwards: reload open pages.")
                    .color(p.muted),
            );
            ui.add_space(8.0);

            section_card(ui, "Pop-ups & ad tabs", "Stop scripts that open ads in new tabs.", |ui| {
                changed |= setting(
                    ui,
                    &mut s.guards.popups,
                    "Block pop-ups that weren't asked for",
                    "Windows and tabs a page opens without a click, or from code that catches clicks anywhere on the page \
                     (how pop-under ad scripts open ad tabs). Links and buttons you click keep working.",
                );
                changed |= setting(
                    ui,
                    &mut s.guards.click_hijack,
                    "Stop ads that hijack the play button",
                    "Clicks on a video player, or on an invisible layer over the page, can't open windows or tabs. A layer                      laid over the player to catch your click is switched off, so the click starts the video instead.",
                );
                ui.separator();
                self.rules(ui, cx, acts);
                ui.separator();
                self.popup_log(ui, cx, acts);
            });

            section_card(ui, "Right-click & copying", "Some pages block the context menu or text selection.", |ui| {
                changed |= setting(
                    ui,
                    &mut s.guards.context_menu,
                    "Always allow right-click",
                    "The page's scripts can't block or replace the browser's context menu.",
                );
                ui.add_enabled_ui(s.guards.context_menu, |ui| {
                    changed |= setting(
                        ui,
                        &mut s.guards.text_selection,
                        "Also allow selecting and copying text",
                        "Undoes user-select: none, and pages can't cancel copying or alter what you copy.",
                    );
                });
            });

            section_card(
                ui,
                "Developer tools",
                "Some pages freeze when the developer tools open, or hide what they do while you look.",
                |ui| {
                    changed |= setting(
                        ui,
                        &mut s.guards.anti_debugger,
                        "Remove debugger traps",
                        "Drops the `debugger` statements pages build at runtime (Function(\"debugger\"), string timers), \
                         which pause the page whenever the developer tools are open.",
                    );
                    let firefox = cx.caps.browser == "firefox";
                    ui.add_enabled_ui(s.guards.anti_debugger && firefox, |ui| {
                        changed |= setting(
                            ui,
                            &mut s.guards.strip_debugger_scripts,
                            "Also remove them from script files",
                            "Rewrites scripts as they load, blanking out every debugger statement.",
                        );
                    });
                    if !firefox {
                        ui.label(
                            RichText::new(
                                "Chrome doesn't let extensions change scripts. For debugger statements written in script \
                                 files, use the developer tools' Deactivate breakpoints (Ctrl+F8) or \"Never pause here\".",
                            )
                            .small()
                            .color(p.muted),
                        );
                        ui.add_space(6.0);
                    }
                    changed |= setting(
                        ui,
                        &mut s.guards.devtools_stealth,
                        "Hide the developer tools from pages",
                        "Pages see a normal window size while the developer tools are docked, and can't block F12, \
                         Ctrl+Shift+I/J/C or Ctrl+U.",
                    );
                    ui.add_enabled_ui(s.guards.devtools_stealth, |ui| {
                        changed |= setting(
                            ui,
                            &mut s.guards.silence_console,
                            "Silence the page's console",
                            "Most detection tricks log objects that notice when the developer tools display them. The \
                             page's console messages don't appear while this is on.",
                        );
                    });
                },
            );

            section_card(ui, "Exceptions", "Sites where no guard runs (and their subdomains).", |ui| {
                let mut remove = None;
                ui.horizontal_wrapped(|ui| {
                    for (i, site) in s.guards.exceptions.iter().enumerate() {
                        ui.label(RichText::new(site).monospace());
                        if icon_button(ui, "🗙", "Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.add_space(8.0);
                    }
                });
                if let Some(i) = remove {
                    s.guards.exceptions.remove(i);
                    changed = true;
                }
                ui.horizontal(|ui| {
                    let r = ui.add(text_input(&mut self.exception, "example.com", 220.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let site = normalize_site(&self.exception);
                    if (ui.add_enabled(crate::guards::valid_site(&site), egui::Button::new("Add")).clicked() || enter)
                        && crate::guards::valid_site(&site)
                    {
                        if !s.guards.exceptions.contains(&site) {
                            s.guards.exceptions.push(site);
                            changed = true;
                        }
                        self.exception.clear();
                    }
                });
            });

            self.inspector(ui, cx, acts);
        });
        if changed {
            acts.send(Command::UpdateSettings { settings: s });
        }
    }

    fn rules(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        ui.label(RichText::new("Custom rules").strong());
        for r in &cx.state.block_rules {
            ui.horizontal(|ui| {
                let mut on = r.enabled;
                if ui.checkbox(&mut on, "").changed() {
                    acts.send(Command::SaveBlockRule { rule: BlockRule { enabled: on, ..r.clone() } });
                }
                ui.label(r.describe());
                if icon_button(ui, "🗑", "Delete the rule").clicked() {
                    acts.send(Command::DeleteBlockRule { id: r.id });
                }
            });
        }
        if cx.state.block_rules.is_empty() {
            ui.label(RichText::new("No rules yet. Add one below, or from a tab in the list underneath.").small().color(p.muted));
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("rule-kind").selected_text(self.kind.label()).width(170.0).show_ui(ui, |ui| {
                for k in BlockRuleKind::ALL {
                    ui.selectable_value(&mut self.kind, k, k.label());
                }
            });
            let (pattern_hint, sites_hint) = match self.kind {
                BlockRuleKind::Script => ("ads.example.com  or  *popunder*.js  or  /regex/", "only on sites (optional)"),
                BlockRuleKind::PopupTo => ("adserver.com  or  *://*/promo/*", ""),
                BlockRuleKind::AllPopupsOn => ("", "example.com, other.org"),
            };
            if self.kind != BlockRuleKind::AllPopupsOn {
                ui.add(text_input(&mut self.pattern, pattern_hint, 280.0));
            }
            if !sites_hint.is_empty() {
                ui.add(text_input(&mut self.sites, sites_hint, 200.0));
            }
            let rule = BlockRule {
                enabled: true,
                kind: self.kind,
                pattern: self.pattern.trim().to_string(),
                sites: self.sites.split(',').map(normalize_site).filter(|s| !s.is_empty()).collect(),
                ..Default::default()
            };
            let check = rule.validate();
            if ui.add_enabled(check.is_ok(), primary_button(ui, "＋ Add rule")).clicked() {
                acts.send(Command::SaveBlockRule { rule });
                self.pattern.clear();
                self.sites.clear();
            }
            let touched = !self.pattern.trim().is_empty() || !self.sites.trim().is_empty();
            if let (Err(e), true) = (check, touched) {
                ui.label(RichText::new(e).small().color(p.danger));
            }
        });
        let help = match self.kind {
            BlockRuleKind::Script => {
                "The script never loads. A bare host blocks every script from it. Find script URLs in the inspector's \
                 Media & frames view (⛔ Block)."
            }
            BlockRuleKind::PopupTo => "Tabs a page opens to this site are closed right away, and you go back to the page.",
            BlockRuleKind::AllPopupsOn => "Scripts on these sites can't open windows or tabs at all; links you click still work.",
        };
        ui.label(RichText::new(help).small().color(p.muted));
    }

    /// Recent tabs opened by pages, with one-click rules.
    fn popup_log(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Tabs opened by pages").strong());
            if !cx.state.popup_log.is_empty() && ui.small_button("Clear").clicked() {
                acts.send(Command::ClearPopupLog);
            }
        });
        if cx.state.popup_log.is_empty() {
            ui.label(RichText::new("Tabs that pages open (links or scripts) are listed here.").small().color(p.muted));
            return;
        }
        for e in cx.state.popup_log.iter().take(15) {
            let to = url::registrable_domain(&url::display_host(&e.url));
            let from = url::registrable_domain(&url::display_host(&e.opener));
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format_time(e.at)).small().color(p.muted));
                ui.label(RichText::new(&from).strong());
                ui.label(RichText::new("opened").color(p.muted));
                ui.add(egui::Label::new(RichText::new(&e.url).monospace().small()).truncate()).on_hover_text(&e.url);
                if e.closed_by.is_some() {
                    ui.label(RichText::new("closed by a rule").small().color(p.success));
                } else {
                    if !to.is_empty() && ui.small_button(format!("Close tabs to {to}")).clicked() {
                        let rule = BlockRule { enabled: true, kind: BlockRuleKind::PopupTo, pattern: to.clone(), ..Default::default() };
                        acts.send(Command::SaveBlockRule { rule });
                    }
                    if !from.is_empty() && ui.small_button(format!("Block all pop-ups on {from}")).clicked() {
                        let rule =
                            BlockRule { enabled: true, kind: BlockRuleKind::AllPopupsOn, sites: vec![from.clone()], ..Default::default() };
                        acts.send(Command::SaveBlockRule { rule });
                    }
                }
            });
        }
    }

    fn inspector(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        section_card(
            ui,
            "Inspector",
            "Inspect a page (docked at the bottom of it, or in a window) without the browser's developer tools: pick elements, see the DOM, \
             styles and HTML, list its videos, frames and scripts, and read its source.",
            |ui| {
                ui.label(RichText::new("Shortcut: Alt+Shift+I inspects the current tab. Or choose an open tab:").small().color(p.muted));
                let open: Vec<_> = cx
                    .state
                    .sessions
                    .iter()
                    .flat_map(|s| &s.entries)
                    .filter(|e| e.tab_id.is_some() && !e.discarded && matches!(url::scheme(&e.url).as_str(), "http" | "https"))
                    .collect();
                egui::ScrollArea::vertical().id_salt("inspect-tabs").max_height(200.0).show(ui, |ui| {
                    for e in open {
                        ui.horizontal(|ui| {
                            if ui.button("🔬 Inspect").clicked() {
                                acts.send(Command::OpenInspector { tab: e.tab_id });
                            }
                            ui.add(egui::Label::new(e.display_title()).truncate()).on_hover_text(&e.url);
                        });
                    }
                });
            },
        );
    }
}
