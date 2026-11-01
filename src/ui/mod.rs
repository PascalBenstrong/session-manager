//! egui user interface, used by both the toolbar popup and the full-tab manager page.
//! The UI never mutates state itself: it sends [`Command`]s to the background and
//! re-renders from the state it gets back (or from storage change notifications).
//!
//! Layout: the toolbar popup is a quick switcher (search + jump to a tab); wide manager
//! pages get a sidebar (sessions + views); narrow manager pages get a compact header.
//! The browser sidebar / side panel page shows the tabs of its window by workspace.

mod backup_view;
mod capture_view;
mod capture_window;
mod cleanup_view;
mod guards_view;
mod inspector_view;
mod mini_view;
mod network_view;
mod offload;
mod organize_view;
mod settings_view;
mod sidebar_view;
mod tabs_view;
mod theme;
mod whats_new_view;
mod widgets;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Margin, RichText, ThemePreference};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::model::{AppState, EntryRef, Id, Session, Snapshot, TabId};
use crate::platform as pf;
use crate::protocol::{Capabilities, Command, Response, SNAPSHOTS_KEY, STATE_KEY};
use widgets::{badge, icon_button, nav_item, primary_button, segmented, text_input};

/// Width from which the sidebar layout is used.
const WIDE_LAYOUT: f32 = 860.0;

/// This page is the toolbar popup (which closes itself after some actions).
static IS_POPUP: AtomicBool = AtomicBool::new(false);

/// `mode`: "popup", "sidebar" (browser sidebar / side panel), "inspector" (inspector
/// window), "capture" (capture window) or "manager".
pub async fn start(canvas_id: &str, mode: &str) -> Result<(), JsValue> {
    let popup = mode == "popup";
    let sidebar = mode == "sidebar";
    let inspector = mode == "inspector";
    let capture = mode == "capture";
    IS_POPUP.store(popup, Ordering::Relaxed);
    let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let canvas = document.get_element_by_id(canvas_id).ok_or("canvas not found")?.dyn_into::<web_sys::HtmlCanvasElement>()?;
    let runner = Box::leak(Box::new(eframe::WebRunner::new()));
    runner
        .start(canvas, eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(App::new(cc, popup, sidebar, inspector, capture)))))
        .await
}

/// Results delivered from async tasks / JS callbacks to the next frame.
#[derive(Default)]
struct Inbox {
    responses: Vec<Response>,
    /// The newest state the background saved, not read yet: only the newest is parsed (once
    /// per frame), however many arrive meanwhile. Kept as the JS string until then.
    pushed_state: Option<JsValue>,
    /// The newest snapshots saved (SNAPSHOTS_KEY), not read yet; same as `pushed_state`.
    pushed_snapshots: Option<JsValue>,
    /// The window of a sidebar page.
    window_id: Option<TabId>,
    /// Messages for the toast: (text, error).
    notices: Vec<(String, bool)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Tabs,
    Organize,
    Cleanup,
    Backup,
    Settings,
    /// Page guards and the inspector.
    Guards,
    /// Release notes (reached from the sidebar, the popup footer or Settings).
    WhatsNew,
}

impl View {
    const ALL: [(View, &'static str, &'static str); 6] = [
        (View::Tabs, "☰", "Tabs"),
        (View::Organize, "⊞", "Organize"),
        (View::Backup, "💾", "Snapshots & import"),
        (View::Cleanup, "🧹", "Cookies & data"),
        (View::Guards, "🛡", "Page guards"),
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
            "guards" => View::Guards,
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
    /// Action to run on the next pointer release (see `smPlatform.setGestureAction`).
    gesture: Option<&'static str>,
    /// Browser pages to open (e.g. "about:config").
    open_pages: Vec<String>,
    notice: Option<String>,
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

    /// Arms an action that needs a user gesture ("sidebar", "tabHide") while the pointer is
    /// over the button that triggers it; it runs when the pointer is released.
    pub fn arm(&mut self, gesture: &'static str) {
        self.gesture = Some(gesture);
    }

    /// Opens a browser page; if extensions may not open it, its address is copied instead.
    pub fn open_page(&mut self, url: impl Into<String>) {
        self.open_pages.push(url.into());
    }

    /// Shows a message in the toast.
    pub fn notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }
}

/// Read-only context handed to views.
pub(crate) struct ViewCtx<'a> {
    pub state: &'a AppState,
    pub caps: &'a Capabilities,
    pub viewed: Id,
    /// The sessions' snapshots (not in `state`: they are kept apart, see SNAPSHOTS_KEY).
    pub snapshots: &'a BTreeMap<Id, Vec<Snapshot>>,
}

impl ViewCtx<'_> {
    pub fn snapshots_of(&self, session: Id) -> &[Snapshot] {
        self.snapshots.get(&session).map_or(&[], Vec::as_slice)
    }

    /// A copy of a session with its snapshots (to export it).
    pub fn with_snapshots(&self, session: &Session) -> Session {
        Session { snapshots: self.snapshots_of(session.id).to_vec(), ..session.clone() }
    }
}

pub struct App {
    popup: bool,
    /// The browser sidebar / side panel page.
    sidebar: bool,
    /// The window of the sidebar page.
    window_id: Option<TabId>,
    /// Gesture action armed in this frame.
    gesture: Option<&'static str>,
    side: sidebar_view::SidebarView,
    guards: guards_view::GuardsView,
    /// The inspector window.
    inspector: Option<inspector_view::InspectorView>,
    /// The capture window.
    capture: Option<capture_window::CaptureWindow>,
    ctx: egui::Context,
    inbox: Rc<RefCell<Inbox>>,
    pending: u32,
    state: Option<AppState>,
    snapshots: BTreeMap<Id, Vec<Snapshot>>,
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
    fn new(cc: &eframe::CreationContext<'_>, popup: bool, sidebar: bool, inspector: bool, capture: bool) -> Self {
        let theme = theme::load_preference();
        theme::install(&cc.egui_ctx, theme);

        let inbox = Rc::new(RefCell::new(Inbox::default()));
        {
            let inbox = inbox.clone();
            let ctx = cc.egui_ctx.clone();
            let cb = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
                if v.is_string() {
                    inbox.borrow_mut().pushed_state = Some(v);
                    ctx.request_repaint();
                }
            });
            pf::on_state_changed(STATE_KEY, cb.as_ref().unchecked_ref());
            cb.forget();
        }
        {
            // Snapshots: read once, then followed like the state.
            let pushed = inbox.clone();
            let ctx = cc.egui_ctx.clone();
            let cb = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
                if v.is_string() {
                    pushed.borrow_mut().pushed_snapshots = Some(v);
                    ctx.request_repaint();
                }
            });
            pf::on_state_changed(SNAPSHOTS_KEY, cb.as_ref().unchecked_ref());
            cb.forget();
            let inbox = inbox.clone();
            let ctx = cc.egui_ctx.clone();
            spawn_local(async move {
                if let Ok(Some(json)) = pf::storage_get(SNAPSHOTS_KEY).await {
                    let mut inbox = inbox.borrow_mut();
                    if inbox.pushed_snapshots.is_none() {
                        inbox.pushed_snapshots = Some(JsValue::from_str(&json));
                    }
                    ctx.request_repaint();
                }
            });
        }
        if sidebar {
            let inbox = inbox.clone();
            let ctx = cc.egui_ctx.clone();
            spawn_local(async move {
                if let Ok(id) = pf::current_window_id().await {
                    inbox.borrow_mut().window_id = Some(id);
                    ctx.request_repaint();
                }
            });
        }
        let mut app = Self {
            popup,
            sidebar,
            window_id: None,
            gesture: None,
            side: Default::default(),
            guards: Default::default(),
            inspector: inspector.then(|| inspector_view::InspectorView::new(&cc.egui_ctx)),
            capture: capture.then(|| capture_window::CaptureWindow::new(&cc.egui_ctx)),
            ctx: cc.egui_ctx.clone(),
            inbox,
            pending: 0,
            state: None,
            snapshots: BTreeMap::new(),
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
        if let Some(g) = acts.gesture {
            self.gesture = Some(g);
        }
        if let Some(text) = acts.notice {
            self.set_status(text, false);
        }
        for url in acts.open_pages {
            self.open_page(url);
        }
        if let Some(view) = acts.navigate {
            if self.popup || self.sidebar {
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

    /// Opens a browser page in a tab. Firefox refuses `about:config` and similar pages to
    /// extensions, so the address is copied first, to paste into the address bar.
    fn open_page(&mut self, url: String) {
        self.ctx.copy_text(url.clone());
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        spawn_local(async move {
            let notice = match pf::open_url(&url).await {
                Ok(()) => (format!("Opened {url}"), false),
                Err(_) => {
                    (format!("The browser doesn't let extensions open {url}. The address is copied: paste it into the address bar."), false)
                }
            };
            inbox.borrow_mut().notices.push(notice);
            ctx.request_repaint();
        });
    }

    fn drain_inbox(&mut self) {
        let (responses, pushed, notices) = {
            let mut inbox = self.inbox.borrow_mut();
            if let Some(w) = inbox.window_id.take() {
                self.window_id = Some(w);
            }
            if let Some(json) = inbox.pushed_snapshots.take().and_then(|v| v.as_string())
                && let Ok(all) = serde_json::from_str::<BTreeMap<Id, Vec<Snapshot>>>(&json)
            {
                self.snapshots = all;
            }
            (std::mem::take(&mut inbox.responses), inbox.pushed_state.take(), std::mem::take(&mut inbox.notices))
        };
        for (text, error) in notices {
            self.set_status(text, error);
        }
        if let Some(json) = pushed.and_then(|v| v.as_string())
            && let Ok(s) = serde_json::from_str::<AppState>(&json)
        {
            self.accept_state(s);
        }
        for r in responses {
            self.pending = self.pending.saturating_sub(1);
            if let Some(s) = r.state {
                self.accept_state(s);
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

    /// Takes a state from the background unless it's older than the one shown (replies and
    /// storage changes can arrive out of order).
    fn accept_state(&mut self, s: AppState) {
        if self.state.as_ref().is_none_or(|cur| s.revision >= cur.revision) {
            self.state = Some(s);
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
            if ui.add_enabled(can_delete && session.open_count() > 0, egui::Button::new("🗑 Delete and close its tabs…")).clicked() {
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
                if self.popup && icon_button(ui, "🔬", "Inspect this page (Alt+Shift+I)").clicked() {
                    self.send(Command::OpenInspector { tab: None });
                    self.close_when_done = true;
                }
                if self.popup && self.caps.sidebar {
                    let r = icon_button(ui, "◨", "Open the workspace sidebar");
                    if r.contains_pointer() {
                        self.gesture = Some("sidebar");
                    }
                    if r.clicked() {
                        close_window();
                    }
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
        egui::Panel::bottom("footer").frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, 6))).show(ui, |ui| {
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
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(10))).show(ui, |ui| {
            let cx = ViewCtx { state, caps: &self.caps, viewed: session.id, snapshots: &self.snapshots };
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
        self.gesture = None;
        self.frame(ui);
        pf::set_gesture_action(self.gesture.unwrap_or(""));
    }
}

impl App {
    /// Browser sidebar / side panel: workspace switcher, then this window's tabs.
    fn sidebar_ui(&mut self, ui: &mut egui::Ui, state: &AppState) -> Actions {
        let p = theme::palette(ui);
        let mut acts = Actions::default();
        let caps = self.caps.clone();
        let none = BTreeMap::new(); // the sidebar shows no snapshots
        let cx = ViewCtx { state, caps: &caps, viewed: state.active_session, snapshots: &none };
        egui::Panel::top("header").frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(10, 8))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("◧").size(18.0).color(p.accent));
                ui.label(RichText::new("Workspaces").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, "⛶", "Open the full manager").clicked() {
                        open_manager("");
                    }
                    self.theme_button(ui);
                });
            });
            self.side.switcher(ui, &cx, &mut acts);
        });
        egui::Panel::bottom("footer")
            .frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(10, 6)))
            .show(ui, |ui| self.side.footer(ui, &cx, &mut acts));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(8, 8)))
            .show(ui, |ui| self.side.show(ui, &cx, self.window_id, &mut acts));
        acts
    }

    fn frame(&mut self, ui: &mut egui::Ui) {
        self.drain_inbox();
        if let Some(mut inspector) = self.inspector.take() {
            let mut acts = Actions::default();
            inspector.ui(ui, &mut acts, self.state.as_ref());
            self.inspector = Some(inspector);
            self.apply(acts);
            let dark = ui.visuals().dark_mode;
            self.toast(&ui.ctx().clone(), dark);
            return;
        }
        if let Some(mut capture) = self.capture.take() {
            let mut acts = Actions::default();
            capture.ui(ui, &mut acts);
            self.capture = Some(capture);
            self.apply(acts);
            let dark = ui.visuals().dark_mode;
            self.toast(&ui.ctx().clone(), dark);
            return;
        }
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

        if self.popup || self.sidebar {
            let acts = if self.popup { self.popup_ui(ui, &state, &session) } else { self.sidebar_ui(ui, &state) };
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
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(margin)).show(ui, |ui| {
            let cx = ViewCtx { state: &state, caps: &self.caps, viewed: session.id, snapshots: &self.snapshots };
            match self.view {
                View::Tabs => self.tabs.show(ui, &cx, &mut acts),
                View::Organize => self.organize.show(ui, &cx, &mut acts),
                View::Cleanup => self.cleanup.show(ui, &cx, &mut acts),
                View::Backup => self.backup.show(ui, &cx, &mut acts),
                View::Settings => settings_view::show(ui, &cx, &mut acts),
                View::Guards => self.guards.show(ui, &cx, &mut acts),
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

/// Closes the popup (other pages stay open).
fn close_window() {
    if !IS_POPUP.load(Ordering::Relaxed) {
        return;
    }
    if let Some(w) = web_sys::window() {
        let _ = w.close();
    }
}
