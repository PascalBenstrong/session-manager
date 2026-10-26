//! "What's new": the bundled release notes, plus tracking of which version was seen.

use eframe::egui::{self, RichText};

use super::theme;
use super::widgets::{badge, section_card};
use crate::release_notes::{self, VERSION};

const SEEN_KEY: &str = "sm_seen_version";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

/// True when the release notes of this version have not been opened yet.
pub fn has_unseen() -> bool {
    let seen = storage().and_then(|s| s.get_item(SEEN_KEY).ok().flatten()).unwrap_or_default();
    release_notes::is_newer(VERSION, &seen)
}

/// Records that this version's notes were seen and clears the toolbar badge.
pub fn mark_seen() {
    if let Some(s) = storage() {
        let _ = s.set_item(SEEN_KEY, VERSION);
    }
    crate::platform::clear_badge();
}

pub fn show(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("✨ What's new").size(18.0).strong());
            badge(ui, &format!("You have v{VERSION}"), true);
        });
        ui.add_space(8.0);

        for (i, release) in release_notes::releases().iter().enumerate() {
            let title = format!("Version {}", release.version);
            let subtitle = if i == 0 { format!("{} · current version", release.date) } else { release.date.clone() };
            section_card(ui, &title, &subtitle, |ui| {
                for section in &release.sections {
                    if !section.title.is_empty() {
                        let color = match section.title.to_ascii_lowercase().as_str() {
                            "new" | "added" => p.success,
                            "fixed" => p.warn,
                            "removed" | "breaking" => p.danger,
                            _ => p.accent_hover,
                        };
                        ui.add_space(4.0);
                        ui.label(RichText::new(&section.title).strong().color(color));
                    }
                    for item in &section.items {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label(RichText::new("•  ").color(p.muted));
                            for (text, code) in release_notes::inline_code_segments(item) {
                                if code {
                                    ui.label(RichText::new(text).monospace().background_color(p.surface_hover));
                                } else {
                                    ui.label(text);
                                }
                            }
                        });
                    }
                }
            });
        }
    });
}
