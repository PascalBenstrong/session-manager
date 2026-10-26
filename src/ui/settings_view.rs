//! Settings view.

use eframe::egui::{self, RichText};

use super::theme;
use super::widgets::section_card;
use super::{Actions, ViewCtx};
use crate::protocol::Command;

pub fn show(ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
    let p = theme::palette(ui);
    let mut s = cx.state.settings.clone();
    let mut changed = false;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);
        section_card(ui, "Tab tracking", "How open tabs are recorded in sessions.", |ui| {
            changed |= setting(
                ui,
                &mut s.auto_add_tabs,
                "Add opened tabs to the active session",
                "New tabs, and tabs that navigate to a page, are saved automatically.",
            );
            changed |= setting(
                ui,
                &mut s.ignore_internal_urls,
                "Ignore browser-internal pages",
                "Skip about:, chrome://, extension pages and the new tab page.",
            );
            changed |= setting(
                ui,
                &mut s.keep_tabs_on_window_close,
                "Keep tabs when a window closes",
                "Tabs of a closed window, or of a browser that quits, stay in the session as unloaded. \
                 Closing a single tab always removes it.",
            );
        });

        let sessions = cx.state.sessions.len();
        let tabs: usize = cx.state.sessions.iter().map(|s| s.entries.len()).sum();
        section_card(ui, "About", "", |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Session Manager {}", crate::release_notes::VERSION)).strong());
                if ui.link("What's new").clicked() {
                    acts.navigate("whatsnew");
                }
            });
            ui.label(format!("{sessions} session(s), {tabs} saved tab(s)."));
            ui.label(
                RichText::new(format!(
                    "Browser: {}  ·  native tab groups: {}  ·  per-site cache clearing: {}",
                    if cx.caps.browser.is_empty() { "unknown" } else { &cx.caps.browser },
                    yes_no(cx.caps.tab_groups),
                    yes_no(cx.caps.per_host_cache),
                ))
                .color(p.muted),
            );
        });
    });

    if changed {
        acts.send(Command::UpdateSettings { settings: s });
    }
}

fn setting(ui: &mut egui::Ui, value: &mut bool, title: &str, description: &str) -> bool {
    let p = theme::palette(ui);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = ui.checkbox(value, "").changed();
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(title).strong());
            ui.label(RichText::new(description).small().color(p.muted));
        });
    });
    ui.add_space(6.0);
    changed
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}
