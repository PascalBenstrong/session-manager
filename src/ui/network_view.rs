//! The inspector's Network pane: records the page's requests (fetch, XHR, WebSocket and its
//! messages, beacons) through `web/net-recorder.js`, shows them with their headers and
//! bodies, replays one or all of them into the page, and throttles requests (rules of this
//! tab, or kept for every tab of the site; applied by the background, also after reloads).
//!
//! Recording goes on across reloads (the recorder runs from the start of each page of the
//! tab), and the log keeps the requests of every page, separated by page. Its state is saved
//! for the tab, so a docked inspector, which reloads with the page, carries on where it was.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Layout, Margin, RichText, Sense, Vec2};
use serde_json::{Value, json};
use wasm_bindgen_futures::spawn_local;

use super::Actions;
use super::theme;
use super::widgets::{badge, empty_state, icon_button, primary_button, segmented, text_input};
use crate::guards::normalize_site;
use crate::model::TabId;
use crate::netlog::{self, NetEntry, NetLog, NetSettings, ThrottleRule};
use crate::platform as pf;

const SETTINGS_KEY: &str = "smNetSettings";
const METHODS: [&str; 7] = ["", "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

enum Reply {
    Started(Result<(), String>),
    Throttle(Result<Vec<ThrottleRule>, String>),
    Settings(Option<NetSettings>),
    Done(Result<(), String>),
}

/// A row of the list: a request, or the start of a page.
enum Row<'a> {
    Page(u64),
    Entry(&'a NetEntry),
}

pub struct NetworkPane {
    tab: Option<TabId>,
    ctx: egui::Context,
    inbox: Rc<RefCell<Vec<Reply>>>,
    log: NetLog,
    recording: bool,
    /// The page (document) now in the tab, the numbers given to pages, and their URLs.
    doc: u64,
    docs: HashMap<String, u64>,
    doc_urls: BTreeMap<u64, String>,
    query: String,
    kind: &'static str,
    selected: Option<u64>,
    replaying: Option<usize>,
    throttle: Vec<ThrottleRule>,
    /// A rule being edited (id 0: a new one).
    editing: Option<ThrottleRule>,
    settings: NetSettings,
    show_settings: bool,
    /// The throttling bar is open (`None`: open when the site has rules).
    throttle_open: Option<bool>,
    /// The view was opened (for "start recording when the Network view opens").
    opened: bool,
    chrome: bool,
    error: Option<String>,
    /// Bumped on every change worth saving (see [`NetworkPane::snapshot`]).
    pub revision: u64,
}

impl NetworkPane {
    pub fn new(ctx: &egui::Context, tab: Option<TabId>) -> Self {
        let chrome = pf::capabilities().browser == "chrome";
        let p = Self {
            tab,
            ctx: ctx.clone(),
            inbox: Rc::new(RefCell::new(Vec::new())),
            log: NetLog::default(),
            recording: false,
            doc: 0,
            docs: HashMap::new(),
            doc_urls: BTreeMap::new(),
            query: String::new(),
            kind: "",
            selected: None,
            replaying: None,
            throttle: Vec::new(),
            editing: None,
            settings: NetSettings::default(),
            show_settings: false,
            throttle_open: None,
            opened: false,
            chrome,
            error: None,
            revision: 0,
        };
        p.run(async { pf::get_throttle().await }, Reply::Throttle);
        p.run(async { pf::storage_get(SETTINGS_KEY).await.ok().flatten().and_then(|s| serde_json::from_str(&s).ok()) }, Reply::Settings);
        p
    }

    fn run<T: 'static>(&self, fut: impl std::future::Future<Output = T> + 'static, reply: impl FnOnce(T) -> Reply + 'static) {
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        spawn_local(async move {
            let r = fut.await;
            inbox.borrow_mut().push(reply(r));
            ctx.request_repaint();
        });
    }

    fn start(&mut self) {
        let Some(tab) = self.tab else { return };
        self.recording = true;
        self.revision += 1;
        self.run(async move { pf::net_start(tab).await }, Reply::Started);
    }

    fn stop(&mut self) {
        let Some(tab) = self.tab else { return };
        self.recording = false;
        self.revision += 1;
        self.run(async move { pf::net_stop(tab).await }, Reply::Done);
    }

    /// A page is (again) in the tab: `doc` names it.
    pub fn on_ready(&mut self, doc: &str, page_url: &str) {
        let next = self.docs.values().max().copied().unwrap_or(0) + 1;
        let new_page = !self.docs.contains_key(doc);
        self.doc = *self.docs.entry(doc.to_string()).or_insert(next);
        self.doc_urls.insert(self.doc, page_url.to_string());
        if new_page && !self.settings.keep_log && !self.log.entries.is_empty() {
            self.log.clear();
            self.selected = None;
        }
        if self.recording {
            self.start();
        }
        self.revision += 1;
    }

    /// The Network view is shown.
    pub fn on_shown(&mut self) {
        if !std::mem::replace(&mut self.opened, true) && self.settings.record_on_open && !self.recording && self.tab.is_some() {
            self.start();
        }
    }

    /// Records from the page.
    pub fn apply(&mut self, records: Vec<Value>) {
        let before = self.log.replays_done;
        self.log.apply_doc(self.doc, records);
        if self.log.replays_done > before {
            self.replaying = None;
        }
        self.revision += 1;
    }

    pub fn recording(&self) -> bool {
        self.recording
    }

    /// What to restore when this tab's inspector loads again (a docked inspector reloads
    /// with its page).
    pub fn snapshot(&self) -> Value {
        json!({
            "log": self.log.export(800, 16 * 1024),
            "recording": self.recording,
            "docs": self.docs,
            "docUrls": self.doc_urls,
            "query": self.query,
            "kind": self.kind,
            "selected": self.selected,
            "opened": self.opened,
        })
    }

    pub fn restore(&mut self, v: &Value) {
        if let Some(log) = v["log"].as_array() {
            self.log.import(log.clone());
        }
        self.recording = v["recording"].as_bool().unwrap_or(false);
        self.opened = v["opened"].as_bool().unwrap_or(false);
        if let Ok(docs) = serde_json::from_value::<HashMap<String, u64>>(v["docs"].clone()) {
            self.docs.extend(docs);
        }
        if let Ok(urls) = serde_json::from_value::<BTreeMap<u64, String>>(v["docUrls"].clone()) {
            self.doc_urls.extend(urls);
        }
        self.query = v["query"].as_str().unwrap_or("").to_string();
        self.kind = match v["kind"].as_str() {
            Some("fetch") => "fetch",
            Some("ws") => "ws",
            Some("beacon") => "beacon",
            _ => "",
        };
        self.selected = v["selected"].as_u64();
    }

    /// The rule throttling a request of this page, if any.
    fn rule_for(&self, page_url: &str, e: &NetEntry) -> Option<&ThrottleRule> {
        if e.kind == "frame" {
            return None;
        }
        netlog::throttle_rule_for(&self.throttle, page_url, &e.url, &e.method, self.tab)
    }

    fn drain(&mut self) {
        for r in std::mem::take(&mut *self.inbox.borrow_mut()) {
            match r {
                Reply::Started(Ok(())) | Reply::Done(Ok(())) => {}
                Reply::Started(Err(e)) => {
                    self.recording = false;
                    self.error = Some(e);
                }
                Reply::Throttle(Ok(rules)) => self.throttle = rules,
                Reply::Throttle(Err(e)) | Reply::Done(Err(e)) => self.error = Some(e),
                Reply::Settings(s) => {
                    if let Some(s) = s {
                        self.settings = s;
                    }
                }
            }
        }
    }

    fn save_settings(&self) {
        let s = serde_json::to_string(&self.settings).unwrap_or_default();
        self.run(async move { pf::storage_set(SETTINGS_KEY, &s).await }, Reply::Done);
    }

    fn save_throttle(&mut self, rules: Vec<ThrottleRule>) {
        self.throttle = rules.clone();
        self.run(async move { pf::set_throttle(&rules).await }, Reply::Done);
    }

    /// Adds `rule` (id 0) or updates it.
    fn put_rule(&mut self, mut rule: ThrottleRule) {
        let mut rules = self.throttle.clone();
        if rule.id == 0 {
            rule.id = rules.iter().map(|r| r.id).max().unwrap_or(0) + 1;
            rules.push(rule);
        } else if let Some(r) = rules.iter_mut().find(|r| r.id == rule.id) {
            *r = rule;
        }
        self.save_throttle(rules);
    }

    /// A rule for this site with the default scope and delay.
    fn new_rule(&self, site: &str, pattern: String, method: &str) -> ThrottleRule {
        ThrottleRule {
            id: 0,
            enabled: true,
            site: site.to_string(),
            pattern,
            method: method.to_string(),
            delay: self.settings.default_delay,
            tab: if self.settings.rules_all_tabs { None } else { self.tab },
        }
    }

    fn quick_rule(&mut self, site: &str, pattern: String, method: &str, acts: &mut Actions) {
        let rule = self.new_rule(site, pattern, method);
        let same =
            self.throttle.iter().find(|r| r.site == rule.site && r.pattern == rule.pattern && r.method == rule.method && r.tab == rule.tab);
        let rule = match same {
            Some(r) => ThrottleRule { enabled: true, delay: rule.delay, ..r.clone() },
            None => rule,
        };
        self.put_rule(rule);
        acts.notice("Throttling on: reload the page to throttle it from the start");
    }

    /// Replays entries, in order: those of the page now in the tab with the page's own
    /// handlers, those of earlier pages from what was recorded.
    fn replay(&mut self, entries: &[&NetEntry], acts: &mut Actions) {
        let steps: Vec<Value> = entries
            .iter()
            .filter_map(|e| {
                if e.doc() == self.doc && e.replayable {
                    Some(Value::from(e.local_id()))
                } else if e.doc() != self.doc && e.resendable() {
                    Some(e.resend_step())
                } else {
                    None
                }
            })
            .collect();
        let Some(tab) = self.tab else { return };
        if steps.is_empty() {
            return;
        }
        let n = steps.len();
        self.replaying = Some(n);
        let cmd = json!({ "cmd": "replay", "ids": steps, "gap": self.settings.replay_gap });
        // The recorder must be in the page (it may not be after a reload when not recording).
        let start = !self.recording;
        self.recording = true;
        self.run(
            async move {
                if start {
                    pf::net_start(tab).await?;
                }
                pf::net_command(tab, &cmd).await
            },
            Reply::Done,
        );
        acts.notice(if n == 1 { "Replaying".to_string() } else { format!("Replaying {n} requests") });
    }

    fn can_replay(&self, e: &NetEntry) -> bool {
        if e.doc() == self.doc { e.replayable } else { e.resendable() }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions, page_url: &str) {
        self.drain();
        self.on_shown();
        let p = theme::palette(ui);
        let site = normalize_site(page_url);
        let Some(tab) = self.tab else { return };

        egui::Panel::top("net-toolbar").frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(10, 6))).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.recording {
                    if ui.add(primary_button(ui, "■ Stop")).on_hover_text("Stop recording the page's requests").clicked() {
                        self.stop();
                    }
                } else if ui
                    .add(primary_button(ui, "● Record"))
                    .on_hover_text("Record the page's requests, now and after it reloads")
                    .clicked()
                {
                    self.error = None;
                    self.start();
                }
                if ui.button("⟳ Reload and record").on_hover_text("Reload the page, recording from its very first request").clicked() {
                    self.recording = true;
                    self.error = None;
                    self.run(async move { pf::net_reload_and_record(tab).await }, Reply::Done);
                }
                if ui.button("🗑 Clear").clicked() {
                    self.log.clear();
                    self.selected = None;
                    self.revision += 1;
                    if self.recording {
                        self.run(async move { pf::net_command(tab, &json!({ "cmd": "clear" })).await }, Reply::Done);
                    }
                }
                ui.separator();
                if segmented(ui, &mut self.kind, &[("", "All"), ("fetch", "Fetch/XHR"), ("ws", "WebSocket"), ("beacon", "Beacon")]) {
                    self.revision += 1;
                }
                if ui.add(text_input(&mut self.query, "Filter…", 160.0)).on_hover_text("URL, method, status or type").changed() {
                    self.revision += 1;
                }
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(self.show_settings, "⚙ Settings").on_hover_text("Settings of the Network view").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                let shown: Vec<NetEntry> =
                    self.log.filtered(&self.query, self.kind).into_iter().filter(|e| e.replay_of == 0 && !e.resent).cloned().collect();
                let todo: Vec<&NetEntry> = shown.iter().filter(|e| self.can_replay(e)).collect();
                let label = match self.replaying {
                    Some(n) => format!("Replaying {n}…"),
                    None => format!("▶ Replay all ({})", todo.len()),
                };
                if ui
                    .add_enabled(!todo.is_empty() && self.replaying.is_none(), primary_button(ui, label))
                    .on_hover_text("Replay the requests listed (not replays), in order, into the page")
                    .clicked()
                {
                    self.replay(&todo, acts);
                }
                if ui
                    .add(egui::DragValue::new(&mut self.settings.replay_gap).range(0..=60_000).speed(10.0).suffix(" ms"))
                    .on_hover_text("Pause between replayed requests")
                    .changed()
                {
                    self.save_settings();
                }
                ui.label(RichText::new("between").small().color(p.muted));
                ui.separator();
                let n = self.log.entries.len();
                let what = if self.recording { format!("● Recording · {n} requests") } else { format!("{n} requests") };
                ui.label(RichText::new(what).small().color(if self.recording { p.danger } else { p.muted }));
            });
            if self.show_settings {
                self.settings_ui(ui);
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(format!("⚠ {e}")).small().color(p.danger));
            }
        });

        // The throttling bar: one line until opened (open when the site has rules).
        let open = self.throttle_open.unwrap_or_else(|| self.throttle.iter().any(|r| r.site == site)) || self.editing.is_some();
        let frame = egui::Frame::new().fill(p.surface).inner_margin(Margin::symmetric(10, 6));
        if open {
            egui::Panel::bottom("net-throttle")
                .resizable(true)
                .default_size(140.0)
                .frame(frame)
                .show(ui, |ui| self.throttle_ui(ui, acts, &site, page_url, tab, true));
        } else {
            egui::Panel::bottom("net-throttle-bar").frame(frame).show(ui, |ui| self.throttle_ui(ui, acts, &site, page_url, tab, false));
        }

        if let Some(e) = self.selected.and_then(|id| self.log.get(id)).cloned() {
            egui::Panel::right("net-details")
                .resizable(true)
                .default_size(460.0)
                .frame(egui::Frame::new().fill(p.surface).inner_margin(Margin::same(10)))
                .show(ui, |ui| self.details(ui, acts, &e, page_url, &site));
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(10, 4))).show(ui, |ui| {
            self.list(ui, acts, page_url, &site);
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        egui::Frame::new().fill(p.surface).corner_radius(8).inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut changed = false;
            egui::Grid::new("net-settings").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("Delay of new throttle rules");
                changed |=
                    ui.add(egui::DragValue::new(&mut self.settings.default_delay).range(0..=120_000).speed(20.0).suffix(" ms")).changed();
                ui.end_row();
                ui.label("Pause between replayed requests");
                changed |=
                    ui.add(egui::DragValue::new(&mut self.settings.replay_gap).range(0..=60_000).speed(10.0).suffix(" ms")).changed();
                ui.end_row();
                ui.label("New throttle rules apply to");
                ui.horizontal(|ui| {
                    changed |= ui.radio_value(&mut self.settings.rules_all_tabs, false, "this tab only").changed();
                    changed |= ui
                        .radio_value(&mut self.settings.rules_all_tabs, true, "every tab of the site (kept)")
                        .on_hover_text("Kept until removed, also after the browser restarts")
                        .changed();
                });
                ui.end_row();
            });
            changed |= ui.checkbox(&mut self.settings.record_on_open, "Start recording when the Network view opens").changed();
            changed |= ui
                .checkbox(&mut self.settings.keep_log, "Keep the log when the page reloads or navigates")
                .on_hover_text("Otherwise the log starts over with each page")
                .changed();
            if changed {
                self.save_settings();
            }
        });
    }

    fn list(&mut self, ui: &mut egui::Ui, acts: &mut Actions, page_url: &str, site: &str) {
        let p = theme::palette(ui);
        let shown: Vec<NetEntry> = self.log.filtered(&self.query, self.kind).into_iter().cloned().collect();
        if shown.is_empty() {
            let (title, hint) = if self.log.entries.is_empty() {
                if self.recording {
                    ("Listening…", "Requests the page makes (fetch, XHR, WebSocket, beacons) show up here, also after it reloads.")
                } else {
                    (
                        "Not recording",
                        "● Record listens from now on and after reloads; ⟳ Reload and record also catches the page's first requests.",
                    )
                }
            } else {
                ("No matches", "Try another filter.")
            };
            empty_state(ui, "📡", title, hint);
            return;
        }
        let mut rows = Vec::with_capacity(shown.len() + 4);
        let mut doc = None;
        for e in &shown {
            if doc != Some(e.doc()) {
                doc = Some(e.doc());
                rows.push(Row::Page(e.doc()));
            }
            rows.push(Row::Entry(e));
        }
        let row_h = 22.0;
        let mono = FontId::monospace(12.0);
        let small = FontId::proportional(12.0);
        let first_start = shown.first().map_or(0.0, |e| e.start);
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show_rows(ui, row_h, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in &rows[range] {
                let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
                let painter = ui.painter_at(rect);
                let y = rect.center().y;
                let e = match row {
                    Row::Page(d) => {
                        let url = self.doc_urls.get(d).cloned().unwrap_or_default();
                        let now = if *d == self.doc { "  (now)" } else { "" };
                        painter.hline(rect.x_range(), y, egui::Stroke::new(1.0, p.surface_hover));
                        let g = painter.layout_no_wrap(format!("  Page loaded: {}{now}  ", short_url(&url)), small.clone(), p.muted);
                        let at = egui::pos2(rect.left() + 24.0, y - g.size().y / 2.0);
                        painter.rect_filled(egui::Rect::from_min_size(at, g.size()), 4.0, p.bg);
                        painter.galley(at, g, p.muted);
                        continue;
                    }
                    Row::Entry(e) => *e,
                };
                if self.selected == Some(e.id) {
                    painter.rect_filled(rect, 4.0, p.accent_soft());
                } else if r.hovered() {
                    painter.rect_filled(rect, 4.0, p.surface_hover);
                }
                let current = e.doc() == self.doc;
                let text = |x: f32, s: &str, font: &FontId, color: Color32| {
                    painter.text(egui::pos2(x, y), Align2::LEFT_CENTER, s, font.clone(), color);
                };
                let mut x = rect.left() + 6.0;
                text(x, &e.kind_label(), &small, p.accent_hover);
                x += 62.0;
                if e.kind != "frame" {
                    text(x, &e.method, &mono, if current { p.text } else { p.muted });
                }
                x += 58.0;
                let (status, color) = status_of(e, p);
                text(x, &status, &mono, color);
                x += 64.0;
                let mut flags = String::new();
                if e.replay_of != 0 || e.resent {
                    flags.push_str("↻ ");
                }
                if self.rule_for(page_url, e).is_some() {
                    flags.push_str("🐢 ");
                }
                if !flags.is_empty() {
                    text(x, &flags, &small, p.warn);
                    x += 30.0;
                }
                let right = rect.right() - 150.0;
                let label =
                    if e.kind == "frame" { e.req_body.clone().unwrap_or_else(|| e.req_body_note.clone()) } else { short_url(&e.url) };
                let galley = painter.layout_no_wrap(label, mono.clone(), if current { p.text } else { p.muted });
                let clip = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(right - 8.0, rect.bottom()));
                painter.with_clip_rect(clip).galley(egui::pos2(x, y - galley.size().y / 2.0), galley, p.text);
                text(right, &format!("+{:.1}s", (e.start - first_start) / 1000.0), &small, p.muted);
                if let Some(d) = e.duration_ms() {
                    text(right + 52.0, &format!("{d:.0} ms"), &small, p.muted);
                }
                if e.size > 0 {
                    painter.text(egui::pos2(rect.right() - 4.0, y), Align2::RIGHT_CENTER, human_size(e.size), small.clone(), p.muted);
                }
                let r = r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(&e.url);
                if r.clicked() {
                    clicked = Some(e.id);
                }
                r.context_menu(|ui| self.entry_menu(ui, acts, e, site));
            }
        });
        if let Some(id) = clicked {
            self.selected = Some(id);
            self.revision += 1;
        }
    }

    fn entry_menu(&mut self, ui: &mut egui::Ui, acts: &mut Actions, e: &NetEntry, site: &str) {
        ui.set_min_width(240.0);
        let label = if e.doc() == self.doc { "▶ Replay" } else { "▶ Send again" };
        if ui.add_enabled(self.can_replay(e), egui::Button::new(label)).on_hover_text(replay_hint(e, self.doc)).clicked() {
            self.replay(&[e], acts);
        }
        ui.separator();
        if ui.button("Copy URL").clicked() {
            ui.ctx().copy_text(e.url.clone());
            acts.notice("URL copied");
        }
        if is_http(e) {
            if ui.button("Copy as cURL").clicked() {
                ui.ctx().copy_text(e.as_curl());
                acts.notice("Copied as cURL");
            }
            if ui.button("Copy as fetch()").clicked() {
                ui.ctx().copy_text(e.as_fetch());
                acts.notice("Copied as fetch()");
            }
        }
        if let Some(b) = &e.req_body
            && ui.button(if e.kind == "frame" { "Copy message" } else { "Copy request body" }).clicked()
        {
            ui.ctx().copy_text(b.clone());
            acts.notice("Copied");
        }
        if let Some(b) = &e.res_body
            && ui.button("Copy response").clicked()
        {
            ui.ctx().copy_text(b.clone());
            acts.notice("Response copied");
        }
        if e.method == "GET" && e.url.starts_with("http") && ui.button("↗ Open in a new tab").clicked() {
            acts.open_page(e.url.clone());
        }
        if e.kind != "frame" && !site.is_empty() {
            ui.separator();
            let method = if is_http(e) { e.method.as_str() } else { "" };
            let scope = if self.settings.rules_all_tabs { "every tab" } else { "this tab" };
            let exact = netlog::exact_pattern(&e.url);
            if ui.add(egui::Button::new(format!("🐢 Throttle this request ({scope})"))).on_hover_text(&exact).clicked() {
                self.quick_rule(site, exact, method, acts);
            }
            let similar = netlog::similar_pattern(&e.url);
            if ui.add(egui::Button::new(format!("🐢 Throttle requests like this ({scope})"))).on_hover_text(&similar).clicked() {
                self.quick_rule(site, similar.clone(), method, acts);
            }
            if ui.button("🐢 Throttle… (edit the rule first)").clicked() {
                self.editing = Some(self.new_rule(site, similar, method));
            }
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, acts: &mut Actions, e: &NetEntry, page_url: &str, site: &str) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} {}", e.kind_label(), if e.kind == "frame" { "" } else { &e.method })).strong());
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, "🗙", "Close").clicked() {
                    self.selected = None;
                    self.revision += 1;
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(&e.url).monospace().small()).wrap().selectable(true));
        ui.horizontal_wrapped(|ui| {
            let label = if e.doc() == self.doc { "▶ Replay" } else { "▶ Send again" };
            if ui.add_enabled(self.can_replay(e), primary_button(ui, label)).on_hover_text(replay_hint(e, self.doc)).clicked() {
                self.replay(&[e], acts);
            }
            if is_http(e) && ui.button("Copy as cURL").clicked() {
                ui.ctx().copy_text(e.as_curl());
                acts.notice("Copied as cURL");
            }
            if e.kind == "ws" && ui.button("Show its messages").clicked() {
                self.kind = "ws";
                self.query = e.url.clone();
            }
            if e.kind != "frame"
                && !site.is_empty()
                && ui.button("🐢 Throttle…").on_hover_text("Make a throttle rule from this request").clicked()
            {
                let method = if is_http(e) { e.method.as_str() } else { "" };
                self.editing = Some(self.new_rule(site, netlog::similar_pattern(&e.url), method));
            }
        });
        ui.label(RichText::new(replay_hint(e, self.doc)).small().color(p.muted));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt("net-details").auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("net-general").num_columns(2).striped(true).show(ui, |ui| {
                let (status, color) = status_of(e, p);
                let row = |ui: &mut egui::Ui, k: &str, v: RichText| {
                    ui.label(RichText::new(k).color(p.muted));
                    ui.add(egui::Label::new(v).wrap().selectable(true));
                    ui.end_row();
                };
                row(ui, "Status", RichText::new(format!("{status} {}", e.status_text)).color(color));
                if !e.mime.is_empty() {
                    row(ui, "Type", RichText::new(&e.mime));
                }
                if e.size > 0 {
                    row(ui, "Size", RichText::new(human_size(e.size)));
                }
                if let Some(d) = e.duration_ms() {
                    row(ui, "Time", RichText::new(format!("{d:.0} ms")));
                }
                if !e.response_url.is_empty() && e.response_url != e.url {
                    row(ui, "Ended at", RichText::new(&e.response_url).monospace().small());
                }
                if let Some(err) = e.error.as_ref().or(e.replay_error.as_ref()) {
                    row(ui, "Error", RichText::new(err).color(p.danger));
                }
                if e.replay_of != 0 {
                    row(ui, "Replay of", RichText::new(format!("#{}", e.replay_of & 0xffff_ffff)));
                } else if e.resent {
                    row(ui, "Sent again", RichText::new("from a request of an earlier page"));
                }
                if let Some(r) = self.rule_for(page_url, e) {
                    row(ui, "Throttled", RichText::new(format!("🐢 {}", r.describe())).color(p.warn));
                }
            });
            ui.add_space(4.0);
            headers(ui, "Request headers", &e.req_headers, acts);
            body(ui, if e.kind == "frame" { "Message" } else { "Request body" }, &e.req_body, &e.req_body_note, acts, true);
            headers(ui, "Response headers", &e.res_headers, acts);
            body(ui, "Response", &e.res_body, &e.res_body_note, acts, e.kind != "frame");
            if !e.stack.is_empty() {
                egui::CollapsingHeader::new(RichText::new("Initiator (where the page made it)").strong()).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(&e.stack).monospace().small()).wrap().selectable(true));
                });
            }
        });
    }

    fn throttle_ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions, site: &str, page_url: &str, tab: TabId, open: bool) {
        let p = theme::palette(ui);
        // This site's rules that apply here: this tab's, and those of every tab.
        let mine: Vec<ThrottleRule> =
            self.throttle.iter().filter(|r| r.site == site && (r.tab.is_none() || r.tab == Some(tab))).cloned().collect();
        ui.horizontal(|ui| {
            let on = mine.iter().filter(|r| r.enabled).count();
            let title = format!(
                "{} 🐢 Throttling on {}{}",
                if open { "▾" } else { "▸" },
                if site.is_empty() { "this page" } else { site },
                if on > 0 { format!(" · {on} rule(s) on") } else { String::new() }
            );
            let color = if on > 0 { p.warn } else { p.text };
            if ui
                .add(egui::Label::new(RichText::new(title).strong().color(color)).sense(Sense::click()))
                .on_hover_text("Show / hide")
                .clicked()
            {
                self.throttle_open = Some(!open);
            }
            if open {
                ui.label(RichText::new("Matching requests are held back by the rule's delay, also after reloads.").small().color(p.muted));
            }
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if !mine.is_empty() && ui.button("↻ Reload the page").on_hover_text("See the page load throttled").clicked() {
                    self.run(async move { pf::reload_tab(tab).await }, Reply::Done);
                }
                if !site.is_empty() && self.editing.is_none() && ui.button("＋ New rule").clicked() {
                    self.editing = Some(self.new_rule(site, "*".into(), ""));
                }
                if !site.is_empty() {
                    let scope = if self.settings.rules_all_tabs { "every tab" } else { "this tab" };
                    let hint = format!("Hold back every request of this site's pages ({scope}) by {} ms", self.settings.default_delay);
                    if ui.add(egui::Button::new("🐢 Throttle everything")).on_hover_text(&hint).clicked() {
                        self.quick_rule(site, "*".into(), "", acts);
                    }
                }
            });
        });
        if !open {
            return;
        }
        if self.chrome {
            let note = "Chrome shows a \"debugging this browser\" bar while a throttled page is open.";
            ui.label(RichText::new(note).small().color(p.muted));
        }
        if let Some(mut rule) = self.editing.take() {
            match self.rule_editor(ui, &mut rule, page_url) {
                Some(true) => self.put_rule(rule),
                Some(false) => {}
                None => self.editing = Some(rule),
            }
        }
        let mut rules = self.throttle.clone();
        let mut changed = false;
        let mut removed = None;
        egui::ScrollArea::vertical().id_salt("throttle-rules").auto_shrink([false, true]).show(ui, |ui| {
            if mine.is_empty() && self.editing.is_none() {
                ui.label(
                    RichText::new("No rules for this site yet. Right-click a request to throttle it, or requests like it.")
                        .small()
                        .color(p.muted),
                );
            }
            for r in &mine {
                let Some(rule) = rules.iter_mut().find(|x| x.id == r.id) else { continue };
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut rule.enabled, "").on_hover_text("On / off").changed();
                    badge(ui, if rule.tab.is_some() { "this tab" } else { "every tab" }, rule.tab.is_none());
                    if !rule.method.is_empty() {
                        badge(ui, &rule.method, false);
                    }
                    let what = if rule.pattern == "*" { "every request".to_string() } else { rule.pattern.clone() };
                    ui.add(egui::Label::new(RichText::new(what).monospace().small()).truncate()).on_hover_text(&rule.pattern);
                    ui.label("+");
                    changed |= ui.add(egui::DragValue::new(&mut rule.delay).range(0..=120_000).speed(20.0).suffix(" ms")).changed();
                    if icon_button(ui, "✏", "Edit the rule").clicked() {
                        self.editing = Some(rule.clone());
                    }
                    if icon_button(ui, "🗑", "Remove the rule").clicked() {
                        removed = Some(rule.id);
                    }
                });
            }
        });
        if let Some(id) = removed {
            rules.retain(|r| r.id != id);
            changed = true;
        }
        if changed {
            self.save_throttle(rules);
        }
    }

    /// Edits a rule. `Some(true)`: save it, `Some(false)`: cancelled, `None`: still editing.
    fn rule_editor(&self, ui: &mut egui::Ui, rule: &mut ThrottleRule, page_url: &str) -> Option<bool> {
        let p = theme::palette(ui);
        let mut out = None;
        egui::Frame::new().fill(p.bg).corner_radius(8).inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(if rule.id == 0 { "New throttle rule" } else { "Edit the throttle rule" }).strong());
            egui::Grid::new("rule-editor").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Requests to");
                let width = ui.available_width().min(520.0);
                ui.add(text_input(&mut rule.pattern, "https://example.com/api/*   (* matches anything)", width));
                ui.end_row();
                ui.label("Method");
                egui::ComboBox::from_id_salt("rule-method")
                    .selected_text(if rule.method.is_empty() { "any" } else { &rule.method })
                    .show_ui(ui, |ui| {
                        for m in METHODS {
                            ui.selectable_value(&mut rule.method, m.to_string(), if m.is_empty() { "any" } else { m });
                        }
                    });
                ui.end_row();
                ui.label("Delay");
                ui.add(egui::DragValue::new(&mut rule.delay).range(0..=120_000).speed(20.0).suffix(" ms"));
                ui.end_row();
                ui.label("Applies to");
                ui.horizontal(|ui| {
                    let mut all = rule.tab.is_none();
                    ui.radio_value(&mut all, false, "this tab only");
                    ui.radio_value(&mut all, true, format!("every tab of {} (kept)", rule.site))
                        .on_hover_text("Kept until you remove it, also after the browser restarts");
                    rule.tab = if all { None } else { rule.tab.or(self.tab) };
                });
                ui.end_row();
            });
            let pattern_ok = !rule.pattern.trim().is_empty();
            let probe = ThrottleRule { enabled: true, tab: None, ..rule.clone() };
            let matched = self.log.entries.iter().filter(|e| e.kind != "frame" && probe.matches(page_url, &e.url, &e.method, None)).count();
            ui.horizontal(|ui| {
                let save = egui::Button::new(if rule.id == 0 { "＋ Add the rule" } else { "Save" });
                if pattern_ok && ui.add(save).on_hover_text("Matching requests are held back from now on").clicked() {
                    rule.pattern = rule.pattern.trim().to_string();
                    out = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    out = Some(false);
                }
                let what = if pattern_ok { format!("matches {matched} recorded request(s)") } else { "the rule needs a pattern".into() };
                ui.label(RichText::new(what).small().color(if pattern_ok { p.muted } else { p.danger }));
            });
        });
        out
    }
}

fn is_http(e: &NetEntry) -> bool {
    matches!(e.kind.as_str(), "fetch" | "xhr" | "beacon")
}

fn replay_hint(e: &NetEntry, doc: u64) -> &'static str {
    if e.doc() != doc {
        return if e.resendable() {
            "Recorded on an earlier page: sent again from what was recorded (same URL, method, headers and body). The page's code doesn't see the response."
        } else {
            "Recorded on an earlier page, and it can't be rebuilt (a WebSocket message, or a body that wasn't text)"
        };
    }
    match (e.kind.as_str(), e.dir.as_str()) {
        ("xhr", _) => "Sent again, and the page's own handlers get the new response: you see what it does with it.",
        ("frame", "in") => "Delivered to the page again, as if the server sent it: the page's code handles it.",
        ("frame", _) => "Sent to the server again on the same connection.",
        ("fetch", _) => {
            "Sent again from the page (same cookies). The page's code can't get a fetch answer twice: the new response shows here."
        }
        ("beacon", _) => "Sent again from the page.",
        ("ws", _) => "Replay its messages instead.",
        _ => "",
    }
}

fn status_of(e: &NetEntry, p: &theme::Palette) -> (String, Color32) {
    if e.error.is_some() {
        return ("failed".into(), p.danger);
    }
    match (e.kind.as_str(), e.status) {
        ("frame", _) => (if e.dir == "in" { "↓ in" } else { "↑ out" }.into(), p.muted),
        ("beacon", s) => (if s < 0 { "refused" } else { "sent" }.into(), p.muted),
        ("ws", 101) => ("open".into(), p.success),
        ("ws", _) if e.end.is_some() => ("closed".into(), p.muted),
        (_, 0) => ("…".into(), p.muted),
        (_, s) if s >= 400 => (s.to_string(), p.danger),
        (_, s) if s >= 300 => (s.to_string(), p.warn),
        (_, s) => (s.to_string(), p.success),
    }
}

/// The URL without its scheme.
fn short_url(u: &str) -> String {
    u.split_once("://").map_or(u, |(_, r)| r).to_string()
}

fn human_size(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

fn headers(ui: &mut egui::Ui, title: &str, list: &[(String, String)], acts: &mut Actions) {
    if list.is_empty() {
        return;
    }
    let p = theme::palette(ui);
    egui::CollapsingHeader::new(RichText::new(format!("{title} ({})", list.len())).strong()).id_salt(title).show(ui, |ui| {
        egui::Grid::new(title).num_columns(2).striped(true).show(ui, |ui| {
            for (k, v) in list {
                let items = [("Copy value", v.clone()), ("Copy as name: value", format!("{k}: {v}"))];
                let r = ui.add(egui::Label::new(RichText::new(k).monospace().small().color(p.warn)).sense(Sense::click()));
                copy_menu(&r, &items, acts);
                let r = ui.add(egui::Label::new(RichText::new(v).monospace().small()).wrap().selectable(true));
                copy_menu(&r, &items, acts);
                ui.end_row();
            }
        });
        if ui.small_button("Copy all").clicked() {
            ui.ctx().copy_text(list.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n"));
            acts.notice("Headers copied");
        }
    });
}

fn body(ui: &mut egui::Ui, title: &str, text: &Option<String>, note: &str, acts: &mut Actions, open: bool) {
    if text.is_none() && note.is_empty() {
        return;
    }
    let p = theme::palette(ui);
    egui::CollapsingHeader::new(RichText::new(title).strong()).id_salt(title).default_open(open).show(ui, |ui| {
        if !note.is_empty() {
            ui.label(RichText::new(note).small().color(p.muted));
        }
        if let Some(t) = text {
            // JSON is shown indented.
            let shown =
                serde_json::from_str::<Value>(t).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| t.clone());
            egui::Frame::new().fill(p.input).corner_radius(6).inner_margin(Margin::same(6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let r = ui.add(egui::Label::new(RichText::new(&shown).monospace().small()).wrap().selectable(true));
                copy_menu(&r, &[("Copy", t.clone()), ("Copy formatted", shown.clone())], acts);
            });
        }
    });
}

fn copy_menu(r: &egui::Response, items: &[(&str, String)], acts: &mut Actions) {
    r.context_menu(|ui| {
        for (label, value) in items {
            if ui.button(*label).clicked() {
                ui.ctx().copy_text(value.clone());
                acts.notice("Copied");
            }
        }
    });
}
