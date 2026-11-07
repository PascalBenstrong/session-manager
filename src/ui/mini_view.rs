//! Quick switcher shown in the toolbar popup: type to search the session's tabs,
//! ↑/↓ to move, Enter (or click) to jump to a tab, Esc to close. Also clears the
//! cookies / site data of the current tab or of any open tab in the list.

use std::cell::RefCell;
use std::rc::Rc;

use eframe::egui::{self, Align, Color32, CursorIcon, Key, Layout, Margin, Modifiers, RichText, Sense, Stroke, UiBuilder};

use super::offload::Search;
use super::theme;
use super::widgets::{ChipKind, avatar, chip, empty_state, icon_button, primary_button, status_dot, tab_state_color, ws_color};
use super::{Actions, ViewCtx};
use crate::cleanup::DataTypes;
use crate::hibernate::HibernateTarget;
use crate::model::{EntryRef, Id, TabEntry, TabId};
use crate::protocol::Command;
use crate::search::SearchOptions;

const ROW_HEIGHT: f32 = 42.0;

pub struct MiniView {
    query: String,
    selected: usize,
    all_sessions: bool,
    focused_once: bool,
    /// Reload a tab after clearing its site data, so the page reflects it.
    reload_after_clean: bool,
    search: Search,
    /// The tabs listed (positions in the state's sessions, sorted) and how many there are in
    /// all, for (state generation, all sessions, viewed session, search results).
    listed: super::Cached<(u64, bool, Id, Option<u64>), Rc<Listed>>,
    /// The current tab's site (for "can't save data"), looked up once: `None` until known,
    /// `Some(None)` if the tab isn't a web page.
    site: Rc<RefCell<Option<Option<String>>>>,
    site_asked: bool,
}

/// Where the tabs listed are in the state's sessions (sorted), and how many there are in all.
type Listed = (Vec<(usize, usize)>, usize);

impl Default for MiniView {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            all_sessions: false,
            focused_once: false,
            reload_after_clean: true,
            search: Search::local(),
            listed: None,
            site: Rc::default(),
            site_asked: false,
        }
    }
}

impl MiniView {
    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        let p = theme::palette(ui);
        let (hits, error) = self.search.run(ui.ctx(), cx.state, &self.query, &SearchOptions::default(), None);

        let sessions = &cx.state.sessions;
        let key = (super::state_generation(), self.all_sessions, cx.viewed, hits.version());
        let listed = match &self.listed {
            Some((k, listed)) if *k == key => listed.clone(),
            _ => {
                let mut rows: Vec<(usize, usize)> = Vec::new();
                let mut total = 0;
                for (si, s) in sessions.iter().enumerate().filter(|(_, s)| self.all_sessions || s.id == cx.viewed) {
                    for (ei, e) in s.entries.iter().enumerate() {
                        total += 1;
                        if hits.contains(e.id) {
                            rows.push((si, ei));
                        }
                    }
                }
                // Open tabs first, then most recently used.
                let at = |&(si, ei): &(usize, usize)| &sessions[si].entries[ei];
                rows.sort_by(|a, b| {
                    let (a, b) = (at(a), at(b));
                    b.is_open().cmp(&a.is_open()).then(b.last_accessed.total_cmp(&a.last_accessed))
                });
                let listed = Rc::new((rows, total));
                self.listed = Some((key, listed.clone()));
                listed
            }
        };
        let (rows, total) = (&listed.0, listed.1);
        let matched: Vec<(EntryRef, &TabEntry)> = rows
            .iter()
            .map(|&(si, ei)| {
                let (s, e) = (&sessions[si], &sessions[si].entries[ei]);
                (EntryRef { session: s.id, entry: e.id }, e)
            })
            .collect();

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
        if enter && let Some((r, _)) = matched.get(self.selected) {
            acts.open_and_close(*r);
        }
        if escape {
            acts.close_popup();
        }

        if !self.site_asked {
            self.site_asked = true;
            let (site, ctx) = (self.site.clone(), ui.ctx().clone());
            wasm_bindgen_futures::spawn_local(async move {
                let found = crate::platform::get_tab(None)
                    .await
                    .ok()
                    .and_then(|t| crate::cleanup::is_web_page(&t.url).then(|| crate::url::registrable_domain(&crate::url::host(&t.url))));
                *site.borrow_mut() = Some(found.and_then(|s| crate::guards::storage_site(&s)));
                ctx.request_repaint();
            });
        }

        // One click: the current tab's whole site, in all its open tabs.
        ui.horizontal_wrapped(|ui| {
            let reload = if self.reload_after_clean { " and reloads its tabs" } else { "" };
            if ui
                .add(primary_button(ui, "🧹 Clear this site's data"))
                .on_hover_text(format!(
                    "Removes the cookies, local & session storage, IndexedDB, cache and service workers of the                      current tab's site and all its subdomains, also from inside its open tabs{reload}.                      You will be logged out of it.

More options: 🧹 at the top."
                ))
                .clicked()
            {
                acts.send(Command::ClearTabData { tab: None, data_types: DataTypes::default(), reload: self.reload_after_clean });
            }
            ui.checkbox(&mut self.reload_after_clean, "Reload").on_hover_text("Reload the site's tabs afterwards, so nothing the page still holds in memory is saved again");
            let site = self.site.borrow().clone().flatten();
            if let Some(site) = site {
                if cx.state.no_storage_sites.contains(&site) {
                    if ui
                        .button(format!("✅ Let {site} save data"))
                        .on_hover_text("It can store cookies and site data again (its open tabs once reloaded)")
                        .clicked()
                    {
                        acts.send(Command::SetNoStorage { site, block: false });
                    }
                } else if ui
                    .button("🚫 Stop it saving data")
                    .on_hover_text(format!(
                        "{site} and its subdomains won't be able to keep anything: their pages get storage that                          only lasts until they close or reload, and their cookies are removed as they are set.                          What they saved before is cleared now and their tabs reload. You won't stay logged in there."
                    ))
                    .clicked()
                {
                    acts.send(Command::SetNoStorage { site, block: true });
                }
            }
        });
        if let Some(Some(site)) = &*self.site.borrow()
            && cx.state.no_storage_sites.contains(site)
        {
            ui.label(RichText::new(format!("🚫 {site} can't save data")).small().color(p.warn));
        }
        ui.add_space(6.0);

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
            for (i, &(r, e)) in matched.iter().enumerate() {
                let session_name = self.all_sessions.then(|| cx.state.session(r.session).map(|s| s.name.as_str()).unwrap_or(""));
                let resp = row(ui, cx, e, r, i == self.selected, session_name, self.reload_after_clean, acts);
                if i == self.selected && moved {
                    resp.scroll_to_me(None);
                }
                if resp.hovered() && ui.input(|inp| inp.pointer.delta() != egui::Vec2::ZERO) {
                    self.selected = i;
                }
            }
        });
    }

    /// "🧹" header menu: clear the current tab's cookies / site data.
    pub fn clean_menu(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        let config = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        egui::containers::menu::MenuButton::new("🧹")
            .config(config)
            .ui(ui, |ui| {
                ui.set_min_width(250.0);
                ui.label(RichText::new("Current tab").small().color(p.muted));
                if ui
                    .button("Clear cookies & site data")
                    .on_hover_text(
                        "Cookies, cache, local & session storage, IndexedDB and service workers of the tab's site and its subdomains",
                    )
                    .clicked()
                {
                    clean(acts, None, "the current tab's site", DataTypes::default(), self.reload_after_clean);
                    ui.close();
                }
                if ui.button("Clear cookies only").clicked() {
                    clean(acts, None, "the current tab's site", COOKIES_ONLY, self.reload_after_clean);
                    ui.close();
                }
                ui.separator();
                ui.checkbox(&mut self.reload_after_clean, "Reload the site's tabs afterwards");
                ui.label(RichText::new("Hover an open tab in the list and click 🧹 to clean its site.").small().color(p.muted));
            })
            .0
            .on_hover_text("Clear cookies & site data of the current tab");
    }

    pub fn footer(&self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.label(RichText::new("↑↓ navigate   ⏎ open   Esc close").small().color(p.muted));
    }
}

const COOKIES_ONLY: DataTypes = DataTypes { cookies: true, cache: false, local_storage: false, indexed_db: false, service_workers: false };

/// Asks for confirmation, then clears `tab`'s site data (`None` = the current tab).
fn clean(acts: &mut Actions, tab: Option<TabId>, site: &str, data_types: DataTypes, reload: bool) {
    acts.confirm(
        format!("Remove {} of {site}? You will be logged out of it.", data_types.describe()),
        Command::ClearTabData { tab, data_types, reload },
    );
}

#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut egui::Ui,
    cx: &ViewCtx,
    e: &TabEntry,
    r: EntryRef,
    selected: bool,
    session_name: Option<&str>,
    reload_after_clean: bool,
    acts: &mut Actions,
) -> egui::Response {
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

    let mut row = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
    let host = e.host();
    let av = avatar(&mut row, &host, !open || e.discarded);
    status_dot(&row, av, tab_state_color(&row, e));
    row.add_space(2.0);
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if hovered {
            if let Some(tab_id) = e.tab_id {
                if icon_button(ui, "🗙", "Close tab").clicked() {
                    acts.send(Command::RemoveEntries { entries: vec![r], close_tabs: true });
                }
                if !e.discarded && icon_button(ui, "💤", "Hibernate (unload) this tab").clicked() {
                    acts.send(Command::Hibernate { target: HibernateTarget::Entries(vec![r]), force: true });
                }
                let media = &cx.state.media;
                let stopped = media.stopped.contains(&tab_id);
                if cx.state.settings.media.stream_controls && (stopped || media.playing.iter().any(|p| p.tab_id == tab_id && p.live)) {
                    let (icon, tip) = if stopped {
                        ("▶", "Resume the stream")
                    } else {
                        ("⏸", "Stop the stream: pause it and download nothing until you resume it")
                    };
                    if icon_button(ui, icon, tip).clicked() {
                        acts.send(Command::StopStream { tab: tab_id, stop: !stopped });
                    }
                }
                if icon_button(ui, "🧹", "Clear this site's cookies & data (with its subdomains)").clicked() {
                    clean(acts, Some(tab_id), &crate::url::registrable_domain(&host), DataTypes::default(), reload_after_clean);
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
            } else if e.discarded {
                meta.push_str(" · hibernated");
            }
            if let Some(n) = session_name {
                meta.push_str(&format!(" · {n}"));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if let Some(ws) = e.workspace.and_then(|id| cx.state.workspace(id)) {
                    ui.label(RichText::new(ws.label()).small().color(ws_color(ws.color)));
                    ui.label(RichText::new("·").small().color(p.muted));
                }
                ui.add(egui::Label::new(RichText::new(meta).small().color(p.muted)).truncate());
            });
        });
    });

    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(&e.url);
    if resp.clicked() {
        acts.open_and_close(r);
    }
    resp
}
