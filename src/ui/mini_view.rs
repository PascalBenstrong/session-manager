//! Quick switcher shown in the toolbar popup: type to search the session's tabs,
//! ↑/↓ to move, Enter (or click) to jump to a tab, Esc to close.

use std::collections::HashMap;

use eframe::egui::{self, Align, Color32, CursorIcon, Key, Layout, Margin, Modifiers, RichText, Sense, Stroke, UiBuilder};

use super::theme;
use super::widgets::{ChipKind, avatar, chip, empty_state, icon_button, status_dot};
use super::{Actions, ViewCtx};
use crate::model::{EntryRef, Id, TabEntry};
use crate::protocol::Command;
use crate::search::{Query, SearchOptions};

const ROW_HEIGHT: f32 = 42.0;

#[derive(Default)]
pub struct MiniView {
    query: String,
    selected: usize,
    all_sessions: bool,
    focused_once: bool,
}

impl MiniView {
    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        let query = Query::parse(&self.query, &SearchOptions::default());
        let error = query.as_ref().err().cloned();
        let query = query.unwrap_or_default();

        let mut owner: HashMap<Id, Id> = HashMap::new();
        let mut matched: Vec<&TabEntry> = Vec::new();
        let mut total = 0;
        for s in cx.state.sessions.iter().filter(|s| self.all_sessions || s.id == cx.viewed) {
            for e in &s.entries {
                total += 1;
                owner.insert(e.id, s.id);
                if query.matches(e) {
                    matched.push(e);
                }
            }
        }
        // Open tabs first, then most recently used.
        matched.sort_by(|a, b| b.is_open().cmp(&a.is_open()).then(b.last_accessed.total_cmp(&a.last_accessed)));
        let entry_ref = |e: &TabEntry| EntryRef { session: owner[&e.id], entry: e.id };

        // Keyboard navigation (consumed before the text field sees the keys).
        let (down, up, enter, escape) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let moved = down || up;
        if down && self.selected + 1 < matched.len() {
            self.selected += 1;
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        self.selected = self.selected.min(matched.len().saturating_sub(1));
        if enter && let Some(e) = matched.get(self.selected) {
            acts.open_and_close(entry_ref(e));
        }
        if escape {
            acts.close_popup();
        }

        // Search field.
        egui::Frame::new()
            .fill(p.input)
            .stroke(Stroke::new(1.0, if error.is_some() { p.danger } else { p.accent.gamma_multiply(0.6) }))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(10, 2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🔍").color(p.muted));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let scope = if self.all_sessions { "All sessions" } else { "This session" };
                        if ui
                            .add(egui::Button::new(RichText::new(scope).small()).corner_radius(255))
                            .on_hover_text("Toggle searching every session")
                            .clicked()
                        {
                            self.all_sessions = !self.all_sessions;
                        }
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .hint_text("Search tabs…  host:  tag:  group:  ^prefix  /regex/")
                                .frame(egui::Frame::NONE)
                                .desired_width(ui.available_width())
                                .margin(egui::vec2(2.0, 7.0)),
                        );
                        if resp.changed() {
                            self.selected = 0;
                        }
                        if !self.focused_once || !ui.memory(|m| m.focused().is_some()) {
                            resp.request_focus();
                            self.focused_once = true;
                        }
                    });
                });
            });
        if let Some(err) = &error {
            ui.label(RichText::new(err).small().color(p.danger));
        }
        ui.add_space(6.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if matched.is_empty() {
                if total == 0 {
                    empty_state(ui, "🗂", "No tabs here yet", "Tabs you open are added to the active session.");
                } else {
                    empty_state(ui, "🔍", "No matches", "");
                }
                return;
            }
            for (i, e) in matched.iter().enumerate() {
                let r = entry_ref(e);
                let session_name = self
                    .all_sessions
                    .then(|| cx.state.session(r.session).map(|s| s.name.as_str()).unwrap_or(""));
                let resp = row(ui, e, r, i == self.selected, session_name, acts);
                if i == self.selected && moved {
                    resp.scroll_to_me(None);
                }
                if resp.hovered() && ui.input(|inp| inp.pointer.delta() != egui::Vec2::ZERO) {
                    self.selected = i;
                }
            }
        });
    }

    pub fn footer(&self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.label(RichText::new("↑↓ navigate   ⏎ open   Esc close").small().color(p.muted));
    }
}

fn row(ui: &mut egui::Ui, e: &TabEntry, r: EntryRef, selected: bool, session_name: Option<&str>, acts: &mut Actions) -> egui::Response {
    let p = theme::palette(ui);
    let open = e.is_open();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = ui.rect_contains_pointer(rect);
    let (fill, stroke) = if selected {
        (p.accent_soft(), Stroke::new(1.0, p.accent.gamma_multiply(0.5)))
    } else if hovered {
        (p.surface_hover, Stroke::NONE)
    } else {
        (Color32::TRANSPARENT, Stroke::NONE)
    };
    ui.painter().rect(rect, 8.0, fill, stroke, egui::StrokeKind::Inside);

    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(8.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let host = e.host();
    let av = avatar(&mut row, &host, !open);
    status_dot(&row, av, open);
    row.add_space(2.0);
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if hovered {
            if open {
                if icon_button(ui, "🗙", "Close tab").clicked() {
                    acts.send(Command::RemoveEntries { entries: vec![r], close_tabs: true });
                }
            } else if icon_button(ui, "⤓", "Open in background without loading").clicked() {
                acts.send(Command::OpenEntry { entry: r, discarded: true });
            }
        } else if let Some(g) = &e.group {
            chip(ui, g, ChipKind::Group);
        }
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space((ROW_HEIGHT - 32.0) / 2.0);
            let color = if open { p.text } else { p.muted };
            ui.add(egui::Label::new(RichText::new(e.display_title()).color(color)).truncate());
            let mut meta = host.clone();
            if !open {
                meta.push_str(" · unloaded");
            }
            if let Some(n) = session_name {
                meta.push_str(&format!(" · {n}"));
            }
            ui.add(egui::Label::new(RichText::new(meta).small().color(p.muted)).truncate());
        });
    });

    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(&e.url);
    if resp.clicked() {
        acts.open_and_close(r);
    }
    resp
}
