//! Browser sidebar / side panel: a vertical tab bar for the sidebar's window, with tabs
//! listed by workspace and a workspace switcher.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Align, Color32, CursorIcon, Layout, RichText, Sense, Stroke, UiBuilder};

use super::theme;
use super::widgets::{DraggedTab, avatar_sized, badge, drag_ghost, icon_button, pill, status_dot, tab_state_color, ws_color};
use super::{Actions, ViewCtx};
use crate::hibernate::HibernateTarget;
use crate::model::{EntryRef, Id, TabEntry, TabId};
use crate::platform as pf;
use crate::protocol::Command;
use crate::search::{Query, SearchOptions};
use crate::workspace::{NO_WORKSPACE, Workspace};

const ROW_HEIGHT: f32 = 30.0;
const HEADER_HEIGHT: f32 = 30.0;

#[derive(Default)]
pub struct SidebarView {
    query: String,
    /// Also list the active session's saved (closed) tabs.
    show_saved: bool,
}

impl SidebarView {
    /// "All" plus one pill per workspace; the current one is highlighted.
    pub fn switcher(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        if cx.state.workspaces.is_empty() {
            return;
        }
        let p = theme::palette(ui);
        let current = cx.state.active_workspace;
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            let (fg, fill) = if current.is_none() { (p.on_accent, p.accent) } else { (p.muted, p.surface_hover) };
            if pill(ui, "All", fg, fill, Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text("No current workspace: show every tab")
                .clicked()
            {
                acts.send(Command::SetActiveWorkspace { id: None });
            }
            for w in &cx.state.workspaces {
                let color = ws_color(w.color);
                let on = current == Some(w.id);
                let (fg, fill) = if on { (Color32::WHITE, color.gamma_multiply(0.85)) } else { (color, color.gamma_multiply(0.16)) };
                if pill(ui, &w.label(), fg, fill, Sense::click())
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text(format!("Switch to {}", w.name))
                    .clicked()
                {
                    acts.send(Command::SetActiveWorkspace { id: Some(w.id) });
                }
            }
        });
    }

    pub fn footer(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            let target = cx.state.active_workspace.and_then(|id| cx.state.workspace(id));
            let tip = match target {
                Some(w) => format!("New tab in {}", w.name),
                None => "New tab".to_string(),
            };
            if ui.button("＋ New tab").on_hover_text(tip).clicked() {
                acts.send(Command::NewTabInWorkspace { workspace: None });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.checkbox(&mut self.show_saved, RichText::new("Saved").color(p.muted))
                    .on_hover_text("Also list the active session's closed (saved) tabs");
            });
        });
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, window: Option<TabId>, acts: &mut Actions) {
        let p = theme::palette(ui);
        let state = cx.state;
        ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("🔍 Search tabs…")
                .desired_width(ui.available_width())
                .margin(egui::vec2(8.0, 5.0)),
        );
        let names: HashMap<Id, String> = state.workspaces.iter().map(|w| (w.id, w.name.clone())).collect();
        let playing: HashSet<TabId> = state.media.playing.iter().map(|x| x.tab_id).collect();
        let query = Query::parse(&self.query, &SearchOptions::default());
        if let Err(e) = &query {
            ui.label(RichText::new(e).small().color(p.danger));
        }
        let query = query.unwrap_or_default().with_context(names, playing.clone(), pf::now());

        // This window's open tabs (any session), and optionally the active session's saved ones.
        let mut entries: Vec<(EntryRef, &TabEntry)> = Vec::new();
        for s in &state.sessions {
            for e in &s.entries {
                let here = e.is_open() && window.is_none_or(|w| e.window_id == Some(w));
                let saved = self.show_saved && !e.is_open() && s.id == state.active_session;
                if (here || saved) && query.matches(e) {
                    entries.push((EntryRef { session: s.id, entry: e.id }, e));
                }
            }
        }
        // The tab in use: the most recently activated open tab of this window.
        let active_tab = entries
            .iter()
            .filter(|(_, e)| e.is_open())
            .max_by(|a, b| a.1.last_accessed.total_cmp(&b.1.last_accessed))
            .map(|(r, _)| *r);
        let filtering = !self.query.trim().is_empty();

        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let known: HashSet<Id> = state.workspaces.iter().map(|w| w.id).collect();
            for w in &state.workspaces {
                let list: Vec<_> = entries.iter().filter(|(_, e)| e.workspace == Some(w.id)).collect();
                if filtering && list.is_empty() {
                    continue;
                }
                self.section(ui, cx, Some(w), &list, active_tab, &playing, acts);
            }
            let rest: Vec<_> = entries.iter().filter(|(_, e)| e.workspace.is_none_or(|w| !known.contains(&w))).collect();
            if !rest.is_empty() {
                self.section(ui, cx, None, &rest, active_tab, &playing, acts);
            }
            if entries.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    let text = if filtering { "No tabs match" } else { "No tabs in this window" };
                    ui.label(RichText::new(text).color(p.muted));
                });
            }
        });
        drag_ghost(ui);
    }

    #[allow(clippy::too_many_arguments)]
    fn section(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ViewCtx,
        ws: Option<&Workspace>,
        list: &[&(EntryRef, &TabEntry)],
        active_tab: Option<EntryRef>,
        playing: &HashSet<TabId>,
        acts: &mut Actions,
    ) {
        let p = theme::palette(ui);
        let id = ws.map_or(NO_WORKSPACE, |w| w.id);
        let cid = ui.make_persistent_id(("side-collapsed", id));
        let mut collapsed = ui.data(|d| d.get_temp::<bool>(cid)).unwrap_or(false);
        let color = ws.map_or(p.muted, |w| ws_color(w.color));
        let current = ws.is_some_and(|w| cx.state.active_workspace == Some(w.id));

        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HEADER_HEIGHT), Sense::click());
        let drop_hover = resp.dnd_hover_payload::<DraggedTab>().is_some();
        if let Some(d) = resp.dnd_release_payload::<DraggedTab>() {
            acts.send(Command::AssignWorkspace { entries: vec![d.entry], workspace: ws.map(|w| w.id) });
        }
        if ui.is_rect_visible(rect) {
            let hovered = ui.rect_contains_pointer(rect);
            let fill = color.gamma_multiply(if hovered || drop_hover { 0.26 } else if current { 0.2 } else { 0.1 });
            let stroke = if drop_hover { Stroke::new(1.5, color) } else { Stroke::NONE };
            ui.painter().rect(rect, 8.0, fill, stroke, egui::StrokeKind::Inside);
            let bar = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 6.0), egui::vec2(3.0, rect.height() - 12.0));
            ui.painter().rect_filled(bar, 1.5, color);
            let mut row = ui.new_child(
                UiBuilder::new().max_rect(rect.shrink2(egui::vec2(6.0, 0.0))).layout(Layout::left_to_right(Align::Center)),
            );
            row.spacing_mut().item_spacing.x = 4.0;
            if icon_button(&mut row, if collapsed { "▸" } else { "▾" }, if collapsed { "Show its tabs" } else { "Hide its tabs" })
                .clicked()
            {
                collapsed = !collapsed;
                row.data_mut(|d| d.insert_temp(cid, collapsed));
            }
            let (icon, name) = ws.map_or(("◌", "No workspace"), |w| (w.icon.as_str(), w.name.as_str()));
            row.label(RichText::new(icon).color(color));
            let text = RichText::new(name).color(p.text);
            row.add(egui::Label::new(if current { text.strong() } else { text }).truncate());
            badge(&mut row, &list.len().to_string(), current);
            row.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(w) = ws
                    && hovered
                    && icon_button(ui, "＋", &format!("New tab in {}", w.name)).clicked()
                {
                    acts.send(Command::NewTabInWorkspace { workspace: Some(w.id) });
                }
                let closed: Vec<EntryRef> = list.iter().filter(|(_, e)| !e.is_open()).map(|(r, _)| *r).collect();
                if hovered
                    && !closed.is_empty()
                    && icon_button(ui, "⤓", &format!("Open its {} saved tab(s), unloaded, in the background", closed.len())).clicked()
                {
                    acts.send(Command::OpenEntries { entries: closed, discarded: true });
                }
            });
        }
        if let Some(w) = ws {
            let resp = resp
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text(format!("Switch to {} · drop tabs here to move them", w.name));
            if resp.clicked() {
                acts.send(Command::SetActiveWorkspace { id: Some(w.id) });
            }
        }
        if !collapsed {
            for (r, e) in list {
                row(ui, *r, e, Some(*r) == active_tab, e.tab_id.is_some_and(|t| playing.contains(&t)), acts);
            }
        }
        ui.add_space(4.0);
    }
}

fn row(ui: &mut egui::Ui, r: EntryRef, e: &TabEntry, active: bool, playing: bool, acts: &mut Actions) {
    let p = theme::palette(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::click_and_drag());
    resp.dnd_set_drag_payload(DraggedTab { entry: r, title: e.display_title().to_string() });
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = ui.rect_contains_pointer(rect) && !egui::DragAndDrop::has_any_payload(ui.ctx());
    let fill = if active {
        p.accent_soft()
    } else if hovered || resp.dragged() {
        p.surface_hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 6.0, fill);
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = 6.0;
    let host = e.host();
    let av = avatar_sized(&mut row, &host, !e.is_open() || e.discarded, 18.0);
    status_dot(&row, av, tab_state_color(&row, e));
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if hovered {
            if e.is_open() {
                if icon_button(ui, "🗙", "Close tab").clicked() {
                    acts.send(Command::RemoveEntries { entries: vec![r], close_tabs: true });
                }
                if !e.discarded && !active && icon_button(ui, "💤", "Hibernate this tab").clicked() {
                    acts.send(Command::Hibernate { target: HibernateTarget::Entries(vec![r]), force: true });
                }
            } else if icon_button(ui, "⤓", "Open in background without loading").clicked() {
                acts.send(Command::OpenEntry { entry: r, discarded: true });
            }
        } else if playing {
            ui.label(RichText::new("🔊").small().color(p.success));
        }
        let color = if e.is_open() && !e.discarded { p.text } else { p.muted };
        let text = RichText::new(e.display_title()).color(color);
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(egui::Label::new(if active { text.strong() } else { text }).truncate());
        });
    });
    let state = if !e.is_open() {
        " (saved, not open)"
    } else if e.discarded {
        " (hibernated)"
    } else {
        ""
    };
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(format!("{}{state}\n{}", e.display_title(), e.url));
    if resp.clicked() {
        acts.send(Command::OpenEntry { entry: r, discarded: false });
    }
}
