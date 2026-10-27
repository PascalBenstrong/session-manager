//! egui user interface, used by both the toolbar popup and the full-tab manager page.
//! The UI never mutates state itself: it sends [`Command`]s to the background and
//! re-renders from the state it gets back (or from storage change notifications).
//!
//! Layout: the toolbar popup is a quick switcher (search + jump to a tab); wide manager
//! pages get a sidebar (sessions + views); narrow manager pages get a compact header.

mod backup_view;
mod cleanup_view;
mod mini_view;
mod organize_view;
mod settings_view;
mod tabs_view;
mod theme;
mod whats_new_view;
mod widgets;

use std::cell::RefCell;
use std::rc::Rc;

use eframe::egui::{self, Margin, RichText, ThemePreference};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::model::{AppState, EntryRef, Id, Session};
use crate::platform as pf;
use crate::protocol::{Capabilities, Command, Response, STATE_KEY};
use widgets::{badge, icon_button, nav_item, primary_button, segmented, text_input};

/// Width from which the sidebar layout is used.
const WIDE_LAYOUT: f32 = 860.0;

pub async fn start(canvas_id: &str, popup: bool) -> Result<(), JsValue> {
    let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let canvas = document
        .get_element_by_id(canvas_id)
        .ok_or("canvas not found")?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    let runner = Box::leak(Box::new(eframe::WebRunner::new()));
    runner
        .start(canvas, eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(App::new(cc, popup)))))
        .await
}

/// Results delivered from async tasks / JS callbacks to the next frame.
#[derive(Default)]
struct Inbox {
    responses: Vec<Response>,
    pushed_state: Option<AppState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Tabs,
    Organize,
    Cleanup,
    Backup,
    Settings,
    /// Release notes (reached from the sidebar, the popup footer or Settings).
    WhatsNew,
}

impl View {
    const ALL: [(View, &'static str, &'static str); 5] = [
        (View::Tabs, "☰", "Tabs"),
        (View::Organize, "⊞", "Organize"),
        (View::Backup, "💾", "Snapshots & import"),
        (View::Cleanup, "🧹", "Cookies & data"),
        (View::Settings, "⚙", "Settings"),
    ];

    /// View named in the manager URL hash (`manager.html#backup`).
    fn from_hash(hash: &str) -> Option<View> {
        Some(match hash {
            "tabs" => View::Tabs,
            "organize" => View::Organize,
            "backup" => View::Backup,
            "cleanup" => View::Cleanup,
            "settings" => View::Settings,
            "whatsnew" => View::WhatsNew,
            _ => return None,
        })
    }
}

struct Status {
    text: String,
    error: bool,
    at: f64,
}

/// A destructive command waiting for confirmation in a modal.
pub(crate) struct Confirm {
    pub text: String,
    pub cmd: Command,
}

/// What a view wants to happen, applied by [`App`] after the view has rendered.
#[derive(Default)]
pub(crate) struct Actions {
    cmds: Vec<Command>,
    confirm: Option<Confirm>,
    /// Switch to another view (name as in the manager URL hash, e.g. "whatsnew").
    navigate: Option<&'static str>,
    /// Close the popup once the queued commands have completed.
    close: bool,
}

impl Actions {
    pub fn send(&mut self, cmd: Command) {
        self.cmds.push(cmd);
    }

    /// Asks for confirmation before sending.
    pub fn confirm(&mut self, text: impl Into<String>, cmd: Command) {
        self.confirm = Some(Confirm { text: text.into(), cmd });
    }

    /// Jump to a tab, then close the popup.
    pub fn open_and_close(&mut self, entry: EntryRef) {
        self.send(Command::OpenEntry { entry, discarded: false });
        self.close = true;
    }

    pub fn close_popup(&mut self) {
        self.close = true;
    }

    pub fn navigate(&mut self, view: &'static str) {
        self.navigate = Some(view);
    }
}

/// Read-only context handed to views.
pub(crate) struct ViewCtx<'a> {
    pub state: &'a AppState,
    pub caps: &'a Capabilities,
    pub viewed: Id,
}

pub struct App {
    popup: bool,
    ctx: egui::Context,
    inbox: Rc<RefCell<Inbox>>,
    pending: u32,
    state: Option<AppState>,
    caps: Capabilities,
    status: Option<Status>,
    view: View,
    theme: ThemePreference,
    /// Session shown in the UI (not necessarily the active one).
    viewed_session: Option<Id>,
    new_session: Option<String>,
    rename_buf: Option<String>,
    confirm: Option<Confirm>,
    /// Close the popup when all pending commands have succeeded.
    close_when_done: bool,
    mini: mini_view::MiniView,
    backup: backup_view::BackupView,
    /// Last seen URL hash, to follow `openManager(view)` requests.
    last_hash: String,
    /// Release notes of this version not opened yet.
    unseen_notes: bool,
    tabs: tabs_view::TabsView,
    organize: organize_view::OrganizeView,
    cleanup: cleanup_view::CleanupView,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, popup: bool) -> Self {
        let theme = theme::load_preference();
        theme::install(&cc.egui_ctx, theme);

        let inbox = Rc::new(RefCell::new(Inbox::default()));
        {
            let inbox = inbox.clone();
            let ctx = cc.egui_ctx.clone();
            let cb = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
                if let Some(json) = v.as_string()
                    && let Ok(state) = serde_json::from_str::<AppState>(&json)
                {
                    inbox.borrow_mut().pushed_state = Some(state);
                    ctx.request_repaint();
                }
            });
            pf::on_state_changed(STATE_KEY, cb.as_ref().unchecked_ref());
            cb.forget();
        }
        let mut app = Self {
            popup,
            ctx: cc.egui_ctx.clone(),
            inbox,
            pending: 0,
            state: None,
            caps: Capabilities::default(),
            status: None,
            view: View::Tabs,
            theme,
            viewed_session: None,
            new_session: None,
            rename_buf: None,
            confirm: None,
            close_when_done: false,
            mini: Default::default(),
            backup: Default::default(),
            last_hash: String::new(),
            unseen_notes: whats_new_view::has_unseen(),
            tabs: Default::default(),
            organize: Default::default(),
            cleanup: Default::default(),
        };
        app.send(Command::GetState);
        app
    }

    /// Sends a command to the background; the response is applied on a later frame.
    fn send(&mut self, cmd: Command) {
        self.pending += 1;
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        spawn_local(async move {
            let resp = pf::send_command(&cmd).await;
            inbox.borrow_mut().responses.push(resp);
            ctx.request_repaint();
        });
    }

    fn confirm(&mut self, text: impl Into<String>, cmd: Command) {
        self.confirm = Some(Confirm { text: text.into(), cmd });
    }

    fn apply(&mut self, acts: Actions) {
        for cmd in acts.cmds {
            self.send(cmd);
        }
        if let Some(c) = acts.confirm {
            self.confirm = Some(c);
        }
        if let Some(view) = acts.navigate {
            if self.popup {
                open_manager(view);
            } else if let Some(v) = View::from_hash(view) {
                self.view = v;
            }
        }
        if acts.close && self.popup {
            if self.pending == 0 {
                close_window();
            } else {
                self.close_when_done = true;
            }
        }
    }

    fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some(Status { text: text.into(), error, at: pf::now() });
    }

    fn drain_inbox(&mut self) {
        let (responses, pushed) = {
            let mut inbox = self.inbox.borrow_mut();
            (std::mem::take(&mut inbox.responses), inbox.pushed_state.take())
        };
        if let Some(s) = pushed {
            self.state = Some(s);
        }
        for r in responses {
            self.pending = self.pending.saturating_sub(1);
            if let Some(s) = r.state {
                self.state = Some(s);
            }
            if let Some(c) = r.capabilities {
                self.caps = c;
            }
            if let Some(e) = r.error {
                // Keep the popup open so the error can be read.
                self.close_when_done = false;
                self.set_status(e, true);
            } else if let Some(info) = r.info.filter(|i| !i.is_empty()) {
                let text = match r.report.as_ref().filter(|rep| !rep.warnings.is_empty()) {
                    Some(rep) => format!("{info} — {}", rep.warnings.join("; ")),
                    None => info,
                };
                self.set_status(text, false);
            }
        }
        if self.close_when_done && self.pending == 0 {
            close_window();
        }
        if let Some(state) = &self.state
            && self.viewed_session.is_none_or(|id| state.session(id).is_none())
        {
            self.viewed_session = Some(state.active_session);
        }
    }

    fn cycle_theme(&mut self) {
        self.theme = match self.theme {
            ThemePreference::System => ThemePreference::Light,
            ThemePreference::Light => ThemePreference::Dark,
            ThemePreference::Dark => ThemePreference::System,
        };
        self.ctx.set_theme(self.theme);
        theme::save_preference(self.theme);
    }

    fn theme_button(&mut self, ui: &mut egui::Ui) {
        let (icon, tip) = match self.theme {
            ThemePreference::System => ("◐", "Theme: system (click for light)"),
            ThemePreference::Light => ("☀", "Theme: light (click for dark)"),
            ThemePreference::Dark => ("🌙", "Theme: dark (click for system)"),
        };
        if icon_button(ui, icon, tip).clicked() {
            self.cycle_theme();
        }
    }

    fn brand(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.label(RichText::new("◧").size(20.0).color(p.accent));
        ui.label(RichText::new("Session Manager").size(15.0).strong());
    }

    // ------------------------------------------------------------ wide layout

    fn sidebar(&mut self, ui: &mut egui::Ui, state: &AppState, viewed: Id) {
        let p = theme::palette(ui);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            self.brand(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.theme_button(ui));
        });
        ui.add_space(14.0);

        ui.horizontal(|ui| {
            ui.label(RichText::new("SESSIONS").small().color(p.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, "+", "New session").clicked() {
                    self.new_session = Some(String::new());
                }
            });
        });
        if self.new_session.is_some() {
            self.new_session_form(ui);
        }
        egui::ScrollArea::vertical().max_height(ui.available_height() - 190.0).auto_shrink([false, true]).show(ui, |ui| {
            for s in &state.sessions {
                let active = s.id == state.active_session;
                let icon = if active { "★" } else { "▢" };
                let count = s.entries.len().to_string();
                let resp = nav_item(ui, icon, &s.name, s.id == viewed, Some((&count, active)));
                let resp = resp.on_hover_text(format!(
                    "{} open · {} unloaded{}",
                    s.open_count(),
                    s.entries.len() - s.open_count(),
                    if active { "\nActive: new tabs are added here" } else { "" }
                ));
                if resp.clicked() {
                    self.viewed_session = Some(s.id);
                    self.rename_buf = None;
                    self.view = View::Tabs;
                }
            }
        });

        ui.add_space(14.0);
        ui.label(RichText::new("VIEWS").small().color(p.muted));
        for (v, icon, label) in View::ALL {
            if nav_item(ui, icon, label, self.view == v, None).clicked() {
                self.view = v;
            }
        }
        ui.add_space(14.0);
        let badge = self.unseen_notes.then_some(("NEW", true));
        if nav_item(ui, "✨", "What's new", self.view == View::WhatsNew, badge)
            .on_hover_text(format!("Release notes · version {}", crate::release_notes::VERSION))
            .clicked()
        {
            self.view = View::WhatsNew;
        }
    }

    fn new_session_form(&mut self, ui: &mut egui::Ui) {
        let Some(name) = &mut self.new_session else { return };
        let mut submit = None;
        ui.horizontal(|ui| {
            let r = ui.add(text_input(name, "Session name", ui.available_width() - 34.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = Some(true);
            } else if !r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                submit = Some(false);
            }
            r.request_focus();
            if icon_button(ui, "✔", "Create").clicked() {
                submit = Some(true);
            }
        });
        if let Some(create) = submit {
            let name = self.new_session.take().unwrap_or_default();
            if create {
                self.send(Command::CreateSession { name, make_active: false });
            }
        }
    }

    fn session_header(&mut self, ui: &mut egui::Ui, state: &AppState, session: &Session) {
        let p = theme::palette(ui);
        let is_active = session.id == state.active_session;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                if self.rename_buf.is_some() {
                    self.rename_form(ui, session.id);
                } else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&session.name).size(22.0).strong());
                        if is_active {
                            badge(ui, "★ Active", true);
                        }
                    });
                }
                ui.label(
                    RichText::new(format!(
                        "{} tabs · {} open · {} unloaded",
                        session.entries.len(),
                        session.open_count(),
                        session.entries.len() - session.open_count()
                    ))
                    .color(p.muted),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.session_menu(ui, state, session);
                self.session_actions(ui, state, session, true);
            });
        });
    }

    fn rename_form(&mut self, ui: &mut egui::Ui, id: Id) {
        let Some(buf) = &mut self.rename_buf else { return };
        let mut done = None;
        ui.horizontal(|ui| {
            let r = ui.add(text_input(buf, "Session name", 220.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = Some(true);
            }
            if ui.add(primary_button(ui, "Save")).clicked() {
                done = Some(true);
            }
            if ui.button("Cancel").clicked() {
                done = Some(false);
            }
        });
        if let Some(save) = done {
            let name = self.rename_buf.take().unwrap_or_default();
            if save {
                self.send(Command::RenameSession { id, name });
            }
        }
    }

    /// Set active / Load / Last tab. Laid out right-to-left.
    fn session_actions(&mut self, ui: &mut egui::Ui, state: &AppState, session: &Session, labels: bool) {
        let is_active = session.id == state.active_session;
        let closed = session.entries.len() - session.open_count();

        let last = if labels { egui::Button::new("⟲  Last tab") } else { egui::Button::new("⟲") };
        if ui
            .add_enabled(!session.entries.is_empty(), last)
            .on_hover_text("Find and focus the most recently used tab of this session")
            .clicked()
        {
            self.send(Command::FocusLastTab { session: session.id });
        }

        ui.add_enabled_ui(closed > 0, |ui| {
            let label = if labels { format!("⤓  Load {closed}") } else { format!("⤓ {closed}") };
            ui.menu_button(label, |ui| {
                ui.set_min_width(220.0);
                if ui.button("Open unloaded (lazy)").on_hover_text("Tabs are created but only load when clicked").clicked() {
                    self.send(Command::LoadSession { id: session.id, discarded: true, new_window: false });
                    ui.close();
                }
                if ui.button("Open unloaded in a new window").clicked() {
                    self.send(Command::LoadSession { id: session.id, discarded: true, new_window: true });
                    ui.close();
                }
                if ui.button("Open and load all").clicked() {
                    self.send(Command::LoadSession { id: session.id, discarded: false, new_window: false });
                    ui.close();
                }
            })
            .response
            .on_hover_text("Open this session's closed tabs");
        });

        if !is_active
            && ui
                .add(primary_button(ui, if labels { "Set active" } else { "★" }))
                .on_hover_text("Add new tabs to this session from now on (does not open its tabs)")
                .clicked()
        {
            self.send(Command::SetActiveSession { id: session.id });
        }
    }

    /// The "⋯" menu: create / rename / delete sessions.
    fn session_menu(&mut self, ui: &mut egui::Ui, state: &AppState, session: &Session) {
        ui.menu_button("⋯", |ui| {
            ui.set_min_width(230.0);
            if ui.button("＋ New session…").clicked() {
                self.new_session = Some(String::new());
                ui.close();
            }
            if ui.button("✏ Rename…").clicked() {
                self.rename_buf = Some(session.name.clone());
                ui.close();
            }
            ui.separator();
            let dups: usize = crate::dedupe::find_duplicates(state, crate::dedupe::Scope::Session(session.id), &state.settings.dedupe)
                .iter()
                .map(|g| g.remove.len())
                .sum();
            if ui
                .add_enabled(dups > 0, egui::Button::new(format!("⧉ Remove duplicates ({dups})")))
                .on_hover_text("Keep one copy of each page and close the extra tabs")
                .clicked()
            {
                self.confirm(
                    format!("Remove {dups} duplicate tab(s) from \"{}\" and close them? One copy of each page is kept.", session.name),
                    Command::RemoveDuplicates { scope: crate::dedupe::Scope::Session(session.id), close_tabs: true },
                );
                ui.close();
            }
            if ui
                .button("💤 Hibernate all tabs")
                .on_hover_text("Unload every open tab except active and pinned tabs and tabs playing media")
                .clicked()
            {
                self.send(Command::Hibernate { target: crate::hibernate::HibernateTarget::All, force: false });
                ui.close();
            }
            if ui.button("📸 Save snapshot").on_hover_text("Save the current tab list as a revision").clicked() {
                self.send(Command::CreateSnapshot { session: session.id, name: String::new() });
                ui.close();
            }
            if ui.button("💾 Snapshots, save & import…").clicked() {
                if self.popup {
                    // File pickers close extension popups, so this lives in the manager page.
                    open_manager("backup");
                } else {
                    self.view = View::Backup;
                }
                ui.close();
            }
            ui.separator();
            let can_delete = state.sessions.len() > 1;
            if ui.add_enabled(can_delete, egui::Button::new("🗑 Delete session…")).clicked() {
                self.confirm(
                    format!("Delete session \"{}\" and forget its {} tab(s)? Open tabs stay open.", session.name, session.entries.len()),
                    Command::DeleteSession { id: session.id, close_tabs: false },
                );
                ui.close();
            }
            if ui
                .add_enabled(can_delete && session.open_count() > 0, egui::Button::new("🗑 Delete and close its tabs…"))
                .clicked()
            {
                self.confirm(
                    format!("Delete session \"{}\" and close its {} open tab(s)?", session.name, session.open_count()),
                    Command::DeleteSession { id: session.id, close_tabs: true },
                );
                ui.close();
            }
        })
        .response
        .on_hover_text("Session options");
    }

    // ------------------------------------------------------------ compact layout

    fn compact_header(&mut self, ui: &mut egui::Ui, state: &AppState, session: &Session) {
        self.compact_bar(ui, state, session);
        ui.add_space(6.0);
        let options: Vec<(View, &str)> = View::ALL.iter().map(|(v, _, l)| (*v, *l)).collect();
        segmented(ui, &mut self.view, &options);
    }

    /// Logo, session picker and session actions in one row.
    fn compact_bar(&mut self, ui: &mut egui::Ui, state: &AppState, session: &Session) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new("◧").size(20.0).color(p.accent));
            egui::ComboBox::from_id_salt("session-picker")
                .selected_text(RichText::new(widgets::session_label(session, session.id == state.active_session)).strong())
                .width(if self.popup { 160.0 } else { 200.0 })
                .height(360.0)
                .show_ui(ui, |ui| {
                    for s in &state.sessions {
                        let label = format!(
                            "{}  ·  {}/{}",
                            widgets::session_label(s, s.id == state.active_session),
                            s.open_count(),
                            s.entries.len()
                        );
                        if ui.selectable_label(s.id == session.id, label).clicked() {
                            self.viewed_session = Some(s.id);
                            self.rename_buf = None;
                        }
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.popup && icon_button(ui, "⛶", "Open the full manager in a tab").clicked() {
                    open_manager("");
                }
                if self.popup {
                    let mut acts = Actions::default();
                    self.mini.clean_menu(ui, &mut acts);
                    self.apply(acts);
                }
                self.theme_button(ui);
                self.session_menu(ui, state, session);
                self.session_actions(ui, state, session, false);
            });
        });
        if self.rename_buf.is_some() {
            self.rename_form(ui, session.id);
        }
        if self.new_session.is_some() {
            self.new_session_form(ui);
        }
    }

    /// Toolbar popup: compact header, quick-switcher list and a hint footer.
    fn popup_ui(&mut self, ui: &mut egui::Ui, state: &AppState, session: &Session) -> Actions {
        let p = theme::palette(ui);
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, 10)))
            .show(ui, |ui| self.compact_bar(ui, state, session));
        egui::Panel::bottom("footer")
            .frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    self.mini.footer(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.link("Manager ⛶").clicked() {
                            open_manager("");
                        }
                        let text = if self.unseen_notes {
                            RichText::new("✨ What's new").color(p.accent_hover).strong()
                        } else {
                            RichText::new("What's new").color(p.muted)
                        };
                        if ui.link(text).on_hover_text(format!("Release notes · version {}", crate::release_notes::VERSION)).clicked() {
                            open_manager("whatsnew");
                        }
                    });
                });
            });
        let mut acts = Actions::default();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(10)))
            .show(ui, |ui| {
                let cx = ViewCtx { state, caps: &self.caps, viewed: session.id };
                self.mini.show(ui, &cx, &mut acts);
            });
        acts
    }

    fn confirm_modal(&mut self, ui: &mut egui::Ui) {
        let Some(c) = &self.confirm else { return };
        let text = c.text.clone();
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("confirm")).show(ui.ctx(), |ui| {
            ui.set_max_width(380.0);
            ui.label(RichText::new("Please confirm").size(16.0).strong());
            ui.add_space(4.0);
            ui.label(text);
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(primary_button(ui, "Confirm")).clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    choice = Some(false);
                }
            });
        });
        if let Some(yes) = choice {
            let c = self.confirm.take().expect("checked");
            if yes {
                self.send(c.cmd);
            }
        }
    }

    fn toast(&mut self, ctx: &egui::Context, dark: bool) {
        let Some(s) = &self.status else { return };
        let expired = !s.error && pf::now() - s.at > 6000.0;
        if expired || widgets::toast(ctx, &s.text, s.error, dark) {
            self.status = None;
        } else if !s.error {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_inbox();
        if !self.popup {
            let hash = pf::location_hash();
            if hash != self.last_hash {
                if let Some(v) = View::from_hash(&hash) {
                    self.view = v;
                }
                self.last_hash = hash;
            }
        }
        let p = theme::palette(ui);
        let dark = ui.visuals().dark_mode;

        let Some(state) = self.state.clone() else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| ui.spinner());
            });
            return;
        };
        let viewed = self.viewed_session.unwrap_or(state.active_session);
        let session = state.session(viewed).cloned().unwrap_or_else(|| state.active().clone());
        let wide = !self.popup && ui.available_width() >= WIDE_LAYOUT;

        if self.popup {
            let acts = self.popup_ui(ui, &state, &session);
            self.apply(acts);
            self.confirm_modal(ui);
            self.toast(&ui.ctx().clone(), dark);
            return;
        }

        let chrome = egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(16, 12));
        if wide {
            egui::Panel::left("sidebar")
                .exact_size(250.0)
                .resizable(false)
                .frame(chrome.inner_margin(Margin::same(12)))
                .show(ui, |ui| self.sidebar(ui, &state, viewed));
            egui::Panel::top("header")
                .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin { left: 24, right: 24, top: 18, bottom: 10 }))
                .show_separator_line(false)
                .show(ui, |ui| self.session_header(ui, &state, &session));
        } else {
            egui::Panel::top("header").frame(chrome).show(ui, |ui| self.compact_header(ui, &state, &session));
        }

        let mut acts = Actions::default();
        let margin = if wide { Margin { left: 24, right: 24, top: 6, bottom: 16 } } else { Margin::same(12) };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(margin))
            .show(ui, |ui| {
                let cx = ViewCtx { state: &state, caps: &self.caps, viewed: session.id };
                match self.view {
                    View::Tabs => self.tabs.show(ui, &cx, &mut acts),
                    View::Organize => self.organize.show(ui, &cx, &mut acts),
                    View::Cleanup => self.cleanup.show(ui, &cx, &mut acts),
                    View::Backup => self.backup.show(ui, &cx, &mut acts),
                    View::Settings => settings_view::show(ui, &cx, &mut acts),
                    View::WhatsNew => whats_new_view::show(ui),
                }
            });
        if self.view == View::WhatsNew && self.unseen_notes {
            whats_new_view::mark_seen();
            self.unseen_notes = false;
        }
        self.apply(acts);

        if self.pending > 0 {
            egui::Area::new(egui::Id::unique("busy"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 8.0))
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| ui.spinner());
        }
        self.confirm_modal(ui);
        self.toast(&ui.ctx().clone(), dark);
    }
}

/// Opens (or focuses) the full manager page at `view` and closes the popup.
fn open_manager(view: &'static str) {
    spawn_local(async move {
        let _ = pf::open_manager(view).await;
        close_window();
    });
}

fn close_window() {
    if let Some(w) = web_sys::window() {
        let _ = w.close();
    }
}
