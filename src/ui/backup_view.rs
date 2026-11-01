//! Snapshots (revisions) of a session, saving sessions to disk, and importing
//! saved sessions or comma separated URL lists from a file, the clipboard or a paste.

use std::cell::RefCell;
use std::rc::Rc;

use eframe::egui::{self, RichText};
use wasm_bindgen_futures::spawn_local;

use super::theme;
use super::widgets::{badge, empty_state, format_time, icon_button, primary_button, section_card, text_input};
use super::{Actions, ViewCtx};
use crate::model::{Session, Snapshot};
use crate::platform as pf;
use crate::protocol::Command;
use crate::state::{ImportTarget, RestoreMode};
use crate::transfer::{self, Imported};

const IMPORT_ACCEPT: &str = ".json,.txt,.csv,.tsv,.url,text/plain,text/csv,text/tab-separated-values,application/json";

/// Text loaded asynchronously (file picker / clipboard), picked up on the next frame.
type Incoming = Rc<RefCell<Option<Result<(String, String), String>>>>;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Target {
    #[default]
    Viewed,
    New,
}

pub struct BackupView {
    snapshot_name: String,
    open_restored: bool,
    include_snapshots: bool,
    import_text: String,
    import_source: String,
    target: Target,
    new_session_name: String,
    open_imported: bool,
    /// The CSV column the URLs come from.
    csv_column: usize,
    /// The URLs of that column, kept until the text or the column changes (a large CSV
    /// would otherwise be read again on every frame).
    csv_cache: Option<(String, usize, transfer::UrlList)>,
    parsed_for: String,
    parsed: Option<Result<Imported, String>>,
    incoming: Incoming,
    error: Option<String>,
}

impl Default for BackupView {
    fn default() -> Self {
        Self {
            snapshot_name: String::new(),
            open_restored: false,
            include_snapshots: true,
            import_text: String::new(),
            import_source: String::new(),
            target: Target::Viewed,
            new_session_name: String::new(),
            open_imported: false,
            csv_column: 0,
            csv_cache: None,
            parsed_for: String::new(),
            parsed: None,
            incoming: Rc::new(RefCell::new(None)),
            error: None,
        }
    }
}

impl BackupView {
    pub fn show(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
        self.receive(ui);
        let Some(session) = cx.state.session(cx.viewed) else { return };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            self.snapshots(ui, cx, session, acts);
            self.export(ui, cx, session);
            self.import(ui, session, acts);
        });
    }

    /// Picks up text from the file picker / clipboard / drag-and-drop.
    fn receive(&mut self, ui: &egui::Ui) {
        if let Some(result) = self.incoming.borrow_mut().take() {
            match result {
                Ok((source, text)) => {
                    self.import_text = text;
                    self.import_source = source;
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        let dropped = ui.input(|i| i.raw.dropped_files.first().cloned());
        if let Some(file) = dropped {
            // Browsers only read dropped files asynchronously.
            let incoming = self.incoming.clone();
            let ctx = ui.ctx().clone();
            spawn_local(async move {
                let name = file.path().display().to_string();
                let result = match file.bytes_async().await {
                    Ok(bytes) => Ok((name, String::from_utf8_lossy(&bytes).into_owned())),
                    Err(e) => Err(format!("could not read the dropped file: {e}")),
                };
                *incoming.borrow_mut() = Some(result);
                ctx.request_repaint();
            });
        }
    }

    // ------------------------------------------------------------ snapshots

    fn snapshots(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, session: &Session, acts: &mut Actions) {
        let p = theme::palette(ui);
        let title = format!("Snapshots of \"{}\"", session.name);
        section_card(ui, &title, "Save the session's tab list as a revision and bring it back later.", |ui| {
            ui.horizontal(|ui| {
                let r = ui.add(text_input(&mut self.snapshot_name, "Snapshot name (optional)", 240.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add(primary_button(ui, "📸  Save snapshot")).clicked() || enter {
                    acts.send(Command::CreateSnapshot { session: session.id, name: std::mem::take(&mut self.snapshot_name) });
                }
            });
            ui.add_space(4.0);

            let mut settings = cx.state.settings.clone();
            let mut auto = settings.auto_snapshot_minutes.is_some();
            let mut minutes = settings.auto_snapshot_minutes.unwrap_or(60);
            ui.horizontal_wrapped(|ui| {
                let mut changed = ui.checkbox(&mut auto, "Automatically snapshot changed sessions every").changed();
                changed |= ui.add_enabled(auto, egui::DragValue::new(&mut minutes).range(1..=10080).suffix(" min")).changed();
                ui.label("· keep the last");
                changed |= ui.add(egui::DragValue::new(&mut settings.max_auto_snapshots).range(1..=500)).changed();
                ui.label("automatic ones");
                if changed {
                    settings.auto_snapshot_minutes = auto.then_some(minutes);
                    acts.send(Command::UpdateSettings { settings });
                }
            });
            ui.checkbox(&mut self.open_restored, "Open restored tabs right away (unloaded)");
            ui.add_space(6.0);

            if session.snapshots.is_empty() {
                empty_state(ui, "📸", "No snapshots yet", "Save one above, or turn on automatic snapshots.");
                return;
            }
            let current = session.urls();
            for snap in session.snapshots.iter().rev() {
                let urls = snap.urls();
                let added = urls.difference(&current).count();
                let removed = current.difference(&urls).count();
                egui::Frame::new()
                    .fill(p.surface_hover)
                    .corner_radius(theme::RADIUS)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(if snap.auto { "⏱" } else { "📌" }).color(p.muted))
                                .on_hover_text(if snap.auto { "Automatic snapshot" } else { "Saved by you" });
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&snap.name).strong());
                                    badge(ui, &format!("{} tabs", snap.entries.len()), false);
                                });
                                let diff = if added == 0 && removed == 0 {
                                    "same tabs as now".to_string()
                                } else {
                                    format!("+{added} / −{removed} compared to now")
                                };
                                ui.label(RichText::new(format!("{} · {diff}", format_time(snap.created_at))).small().color(p.muted));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if icon_button(ui, "🗑", "Delete snapshot").clicked() {
                                    acts.confirm(
                                        format!("Delete snapshot \"{}\"?", snap.name),
                                        Command::DeleteSnapshot { session: session.id, snapshot: snap.id },
                                    );
                                }
                                if icon_button(ui, "⬇", "Save this snapshot to disk").clicked() {
                                    download_snapshot(session, snap);
                                }
                                ui.menu_button("Restore ▾", |ui| {
                                    ui.set_min_width(260.0);
                                    let open = self.open_restored;
                                    if ui
                                        .button("Replace current tabs…")
                                        .on_hover_text("The current tab list is saved as a snapshot first")
                                        .clicked()
                                    {
                                        acts.confirm(
                                            format!(
                                                "Replace the tabs of \"{}\" with snapshot \"{}\"? The current list is saved as a snapshot first; open tabs stay open.",
                                                session.name, snap.name
                                            ),
                                            Command::RestoreSnapshot { session: session.id, snapshot: snap.id, mode: RestoreMode::Replace, open },
                                        );
                                        ui.close();
                                    }
                                    if ui.button(format!("Add the {added} missing tab(s)")).clicked() {
                                        acts.send(Command::RestoreSnapshot { session: session.id, snapshot: snap.id, mode: RestoreMode::Merge, open });
                                        ui.close();
                                    }
                                    if ui.button("Open as a new session").clicked() {
                                        acts.send(Command::RestoreSnapshot {
                                            session: session.id,
                                            snapshot: snap.id,
                                            mode: RestoreMode::NewSession,
                                            open,
                                        });
                                        ui.close();
                                    }
                                });
                            });
                        });
                    });
                ui.add_space(2.0);
            }
        });
    }

    // ------------------------------------------------------------ export

    fn export(&mut self, ui: &mut egui::Ui, cx: &ViewCtx, session: &Session) {
        section_card(ui, "Save to disk", "Download sessions as a file you can import again later, here or in another browser.", |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.add(primary_button(ui, format!("⬇  \"{}\" (.json)", session.name))).clicked() {
                    let json = transfer::export_sessions([session], self.include_snapshots, pf::now());
                    save(&format!("session-{}-{}.json", slug(&session.name), today()), &json, "application/json");
                }
                if ui.button(format!("⬇  All {} sessions (.json)", cx.state.sessions.len())).clicked() {
                    let json = transfer::export_sessions(&cx.state.sessions, self.include_snapshots, pf::now());
                    save(&format!("sessions-{}.json", today()), &json, "application/json");
                }
                if ui.button("⬇  URL list (.txt)").on_hover_text("Comma separated, one per line").clicked() {
                    save(&format!("urls-{}-{}.txt", slug(&session.name), today()), &transfer::export_urls(&session.entries), "text/plain");
                }
                if ui.button("⧉  Copy URLs").on_hover_text("Copy the comma separated URL list to the clipboard").clicked() {
                    ui.ctx().copy_text(transfer::export_urls(&session.entries));
                }
            });
            ui.checkbox(&mut self.include_snapshots, "Include snapshots in .json files");
        });
    }

    // ------------------------------------------------------------ import

    fn import(&mut self, ui: &mut egui::Ui, session: &Session, acts: &mut Actions) {
        let p = theme::palette(ui);
        section_card(
            ui,
            "Import",
            "Load a saved session file, a list of URLs (quoted ones too) from a text file or the clipboard, or a CSV / TSV \
             and pick the column that holds the URLs. You can also drop a file onto this page.",
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("📂  Open file…").clicked() {
                        let incoming = self.incoming.clone();
                        let ctx = ui.ctx().clone();
                        spawn_local(async move {
                            let result = match pf::pick_text_file(IMPORT_ACCEPT).await {
                                Ok(Some(f)) => Some(Ok((f.name, f.text))),
                                Ok(None) => None,
                                Err(e) => Some(Err(format!("could not read file: {e}"))),
                            };
                            *incoming.borrow_mut() = result;
                            ctx.request_repaint();
                        });
                    }
                    if ui.button("📋  Paste from clipboard").clicked() {
                        let incoming = self.incoming.clone();
                        let ctx = ui.ctx().clone();
                        spawn_local(async move {
                            let result = match pf::read_clipboard().await {
                                Ok(text) if !text.trim().is_empty() => Ok(("clipboard".to_string(), text)),
                                Ok(_) => Err("the clipboard is empty".to_string()),
                                Err(e) => Err(format!("could not read the clipboard: {e}")),
                            };
                            *incoming.borrow_mut() = Some(result);
                            ctx.request_repaint();
                        });
                    }
                    if !self.import_text.is_empty() && ui.button("Clear").clicked() {
                        self.import_text.clear();
                        self.import_source.clear();
                    }
                });
                if let Some(e) = &self.error {
                    ui.colored_label(p.danger, e);
                }
                if !self.import_source.is_empty() {
                    ui.label(RichText::new(format!("Loaded from {}", self.import_source)).small().color(p.muted));
                }
                ui.add(
                    egui::TextEdit::multiline(&mut self.import_text)
                        .hint_text("https://example.com, https://docs.rs/egui,\nnews.ycombinator.com, or 'https://example.com/?ids=1,2' …\n\nor a saved session file, or a CSV with a header row")
                        .desired_rows(6)
                        .desired_width(ui.available_width())
                        .margin(egui::vec2(8.0, 6.0))
                        .code_editor(),
                );

                if self.parsed_for != self.import_text {
                    self.parsed_for = self.import_text.clone();
                    self.parsed = (!self.import_text.trim().is_empty()).then(|| transfer::parse_import(&self.import_text));
                    self.csv_cache = None;
                    if let Some(Ok(Imported::Table(table))) = &self.parsed {
                        self.csv_column = table.best_column();
                    }
                }
                let mut imported = false;
                match &self.parsed {
                    None => {}
                    Some(Err(e)) => {
                        ui.colored_label(p.danger, e);
                    }
                    Some(Ok(Imported::Sessions(sessions))) => {
                        let tabs: usize = sessions.iter().map(|s| s.entries.len()).sum();
                        let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
                        ui.label(format!("Session file: {} session(s), {tabs} tab(s) — {}", sessions.len(), names.join(", ")));
                        ui.label(RichText::new("They are added as new sessions; nothing is overwritten.").small().color(p.muted));
                        if ui.add(primary_button(ui, format!("Import {} session(s)", sessions.len()))).clicked() {
                            acts.send(Command::ImportSessions { sessions: sessions.clone() });
                            imported = true;
                        }
                    }
                    Some(Ok(Imported::Urls(list))) => {
                        imported = urls_ui(
                            ui,
                            session,
                            acts,
                            &mut self.target,
                            &mut self.new_session_name,
                            &mut self.open_imported,
                            list,
                        );
                    }
                    Some(Ok(Imported::Table(table))) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("CSV file: {} row(s), {} column(s).", table.rows.len(), table.headers.len()));
                            ui.label(
                                RichText::new("The first row is the header; pick the column that holds the URLs.")
                                    .small()
                                    .color(p.muted),
                            );
                        });
                        let stale = self
                            .csv_cache
                            .as_ref()
                            .is_none_or(|(text, column, _)| text != &self.import_text || *column != self.csv_column);
                        if stale {
                            self.csv_cache = Some((self.import_text.clone(), self.csv_column, table.urls(self.csv_column)));
                        }
                        let list = &self.csv_cache.as_ref().expect("the URLs were just read").2;
                        ui.horizontal_wrapped(|ui| {
                            ui.label("URL column:");
                            egui::ComboBox::from_id_salt("import-csv-column")
                                .selected_text(column_label(table, self.csv_column, list.urls.len()))
                                .show_ui(ui, |ui| {
                                    for column in 0..table.headers.len() {
                                        let count = table.urls(column).urls.len();
                                        ui.selectable_value(&mut self.csv_column, column, column_label(table, column, count));
                                    }
                                });
                        });
                        imported = urls_ui(
                            ui,
                            session,
                            acts,
                            &mut self.target,
                            &mut self.new_session_name,
                            &mut self.open_imported,
                            list,
                        );
                    }
                }
                if imported {
                    self.import_text.clear();
                    self.import_source.clear();
                }
            },
        );
    }
}

/// The URLs found in the import text: a preview of what will be added, and the import
/// button. Returns true when the URLs were sent to be imported.
fn urls_ui(
    ui: &mut egui::Ui,
    session: &Session,
    acts: &mut Actions,
    target: &mut Target,
    new_session_name: &mut String,
    open_imported: &mut bool,
    list: &transfer::UrlList,
) -> bool {
    let p = theme::palette(ui);
    let existing = session.urls();
    let new = list.urls.iter().filter(|u| !existing.contains(u.as_str())).count();
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("{} URL(s) found", list.urls.len()));
        if !list.invalid.is_empty() {
            ui.label(RichText::new(format!("· {} skipped (not URLs)", list.invalid.len())).color(p.warn))
                .on_hover_text(list.invalid.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.radio_value(target, Target::Viewed, format!("Add to \"{}\" ({new} new)", session.name));
        ui.radio_value(target, Target::New, "New session:");
        ui.add_enabled(*target == Target::New, text_input(new_session_name, "name", 160.0));
    });
    ui.checkbox(open_imported, "Open them now (unloaded)");
    let count = if *target == Target::New { list.urls.len() } else { new };
    if ui.add_enabled(count > 0, primary_button(ui, format!("Import {count} URL(s)"))).clicked() {
        let target = match target {
            Target::Viewed => ImportTarget::Session(session.id),
            Target::New => ImportTarget::NewSession(std::mem::take(new_session_name)),
        };
        acts.send(Command::ImportUrls { urls: list.urls.clone(), target, open: *open_imported });
        return true;
    }
    false
}

/// "url (12 URLs)", or "Column 2" when the header has no name.
fn column_label(table: &transfer::Table, column: usize, count: usize) -> String {
    let name = table.column_name(column);
    match count {
        0 => name,
        1 => format!("{name} (1 URL)"),
        n => format!("{name} ({n} URLs)"),
    }
}

fn download_snapshot(session: &Session, snap: &Snapshot) {
    let as_session = Session {
        name: format!("{} — {}", session.name, snap.name),
        created_at: snap.created_at,
        entries: snap.entries.clone(),
        ..Default::default()
    };
    let json = transfer::export_sessions([&as_session], false, pf::now());
    save(&format!("snapshot-{}-{}.json", slug(&session.name), slug(&snap.name)), &json, "application/json");
}

fn save(filename: &str, text: &str, mime: &str) {
    if let Err(e) = pf::download_text(filename, text, mime) {
        web_sys::console::error_1(&format!("download failed: {e}").into());
    }
}

/// File-name friendly version of a name.
fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { "session".into() } else { s }
}

fn today() -> String {
    let iso = String::from(js_sys::Date::new_0().to_iso_string());
    iso.chars().take(10).collect()
}
