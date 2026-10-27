//! The inspector's Capture pane: the video and audio elements of every frame of the page, and
//! the streams and media files it loads. Recording an element, or saving a stream, opens a
//! capture window (see `capture_window.rs`), which does the work and keeps going when the
//! page reloads; the pane lists the capture windows that are open, and can stop them.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use eframe::egui::{self, Align, Layout, Margin, RichText};
use wasm_bindgen_futures::spawn_local;

use super::theme;
use super::widgets::{badge, danger_button, empty_state, icon_button, primary_button, segmented, text_input};
use super::Actions;
use crate::capture::{self, CaptureSettings, CaptureSummary, Method, PageMedia, PageMediaUrl, Tracks, format_bytes, format_duration};
use crate::model::TabId;
use crate::platform::{self as pf, CaptureMediaList};
use crate::stream::{self, SourceKind};

enum Reply {
    Media(Result<CaptureMediaList, String>),
    Captures(Vec<CaptureSummary>),
    Settings(Option<CaptureSettings>),
    Done(Result<(), String>),
}

pub struct CapturePane {
    tab: Option<TabId>,
    ctx: egui::Context,
    inbox: Rc<RefCell<Vec<Reply>>>,
    media: Option<Vec<PageMedia>>,
    urls: Vec<PageMediaUrl>,
    captures: Vec<CaptureSummary>,
    last_poll: f64,
    settings: CaptureSettings,
    url_input: String,
    show_segments: bool,
    error: Option<String>,
}

impl CapturePane {
    pub fn new(ctx: &egui::Context, tab: Option<TabId>) -> Self {
        let p = Self {
            tab,
            ctx: ctx.clone(),
            inbox: Rc::new(RefCell::new(Vec::new())),
            media: None,
            urls: Vec::new(),
            captures: Vec::new(),
            last_poll: 0.0,
            settings: CaptureSettings::default(),
            url_input: String::new(),
            show_segments: false,
            error: None,
        };
        p.run(
            async { pf::storage_get(capture::SETTINGS_KEY).await.ok().flatten().and_then(|s| serde_json::from_str(&s).ok()) },
            Reply::Settings,
        );
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

    pub fn loaded(&self) -> bool {
        self.media.is_some()
    }

    /// Reads the page's media again.
    pub fn refresh(&mut self) {
        let Some(tab) = self.tab else { return };
        self.error = None;
        self.run(async move { pf::collect_capture_media(tab).await }, Reply::Media);
        self.poll_captures();
    }

    fn poll_captures(&mut self) {
        self.last_poll = pf::now();
        self.run(
            async {
                let list = pf::session_list(capture::SUMMARY_PREFIX).await.unwrap_or_default();
                let mut out: Vec<CaptureSummary> = list.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect();
                out.sort_by(|a, b| a.id.cmp(&b.id));
                out
            },
            Reply::Captures,
        );
    }

    fn save_settings(&self) {
        let json = serde_json::to_string(&self.settings).unwrap_or_default();
        spawn_local(async move {
            // The window reads the rest of the settings and keeps them; only merge ours in.
            let mut s: CaptureSettings = pf::storage_get(capture::SETTINGS_KEY).await.ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            if let Ok(ours) = serde_json::from_str::<CaptureSettings>(&json) {
                s.tracks = ours.tracks;
                s.method = ours.method;
            }
            let _ = pf::storage_set(capture::SETTINGS_KEY, &serde_json::to_string(&s).unwrap_or_default()).await;
        });
    }

    fn drain(&mut self) {
        for r in std::mem::take(&mut *self.inbox.borrow_mut()) {
            match r {
                Reply::Media(Ok(list)) => {
                    self.media = Some(list.media);
                    self.urls = list.urls;
                }
                Reply::Media(Err(e)) => {
                    self.media.get_or_insert_with(Vec::new);
                    self.error = Some(e);
                }
                Reply::Captures(list) => self.captures = list,
                Reply::Settings(Some(s)) => self.settings = s,
                Reply::Settings(None) => {}
                Reply::Done(Ok(())) => {}
                Reply::Done(Err(e)) => self.error = Some(e),
            }
        }
    }

    fn open(&self, params: BTreeMap<&'static str, String>) {
        self.run(async move { pf::open_capture(&params).await }, Reply::Done);
    }

    fn record(&self, m: &PageMedia, method: Method, page_title: &str) {
        let Some(tab) = self.tab else { return };
        let label = if m.width > 0 { format!("{} {}×{}", m.tag, m.width, m.height) } else { m.tag.clone() };
        self.open(BTreeMap::from([
            ("tab", tab.to_string()),
            ("frame", m.frame_id.to_string()),
            ("el", m.id.to_string()),
            ("method", method.code().to_string()),
            // An audio element has no picture.
            ("tracks", if m.tag == "audio" { Tracks::Audio } else { self.settings.tracks }.code().to_string()),
            ("title", if m.title.is_empty() { page_title.to_string() } else { m.title.clone() }),
            ("label", label),
        ]));
    }

    fn save_url(&self, u: &PageMediaUrl, page_title: &str) {
        let Some(tab) = self.tab else { return };
        self.open(BTreeMap::from([
            ("url", u.url.clone()),
            ("tab", tab.to_string()),
            ("frame", u.frame_id.to_string()),
            ("tracks", self.settings.tracks.code().to_string()),
            ("title", page_title.to_string()),
        ]));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions, page_title: &str) {
        self.drain();
        let p = theme::palette(ui);
        // Capture windows publish their state as they go.
        if pf::now() - self.last_poll > 1500.0 {
            self.poll_captures();
        }
        self.ctx.request_repaint_after(std::time::Duration::from_millis(1500));
        let Some(media) = self.media.clone() else {
            ui.spinner();
            return;
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Keep").color(p.muted));
            if segmented(ui, &mut self.settings.tracks, &Tracks::ALL) {
                self.save_settings();
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Record").color(p.muted));
            if segmented(ui, &mut self.settings.method, &Method::ALL) {
                self.save_settings();
            }
        });
        ui.label(
            RichText::new(match self.settings.method {
                Method::Element => {
                    "Element stream: the media's own picture and sound, at its size, without what the page draws over it. \
                     Protected (DRM) media can't be recorded this way."
                }
                Method::Screen => {
                    "Rendered on screen: records what the browser shows of the tab (you pick the tab once), cut to the media. \
                     Browsers black out protected (DRM) video in captures."
                }
            })
            .small()
            .color(p.muted),
        );
        if let Some(e) = &self.error {
            ui.label(RichText::new(format!("⚠ {e}")).color(p.danger));
        }
        ui.add_space(6.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if !self.captures.is_empty() {
                self.captures_ui(ui, acts);
            }
            egui::CollapsingHeader::new(RichText::new(format!("🎞 Media on this page ({})", media.len())).strong())
                .id_salt("capture-media")
                .default_open(true)
                .show(ui, |ui| {
                    if media.is_empty() {
                        ui.label(RichText::new("No video or audio elements in this page or its frames.").color(p.muted));
                    }
                    for m in &media {
                        self.media_row(ui, m, page_title);
                    }
                });
            ui.add_space(4.0);
            self.urls_ui(ui, acts, page_title);
            ui.add_space(4.0);
            egui::CollapsingHeader::new(RichText::new("🔗 Open a media URL").strong())
                .id_salt("capture-url")
                .default_open(media.is_empty() && self.urls.is_empty())
                .show(ui, |ui| {
                    ui.label(RichText::new("A video or audio file, or an HLS (.m3u8) or DASH (.mpd) stream: play it, record it, or save it.").small().color(p.muted));
                    ui.horizontal(|ui| {
                        let r = ui.add(text_input(&mut self.url_input, "https://…/video.mp4, …/master.m3u8, …/manifest.mpd", ui.available_width() - 80.0));
                        let url = self.url_input.trim().to_string();
                        let ok = url.starts_with("http://") || url.starts_with("https://");
                        let go = ui.add_enabled(ok, primary_button(ui, "Open")).clicked() || (ok && r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        if go {
                            self.open(BTreeMap::from([("url", url), ("tracks", self.settings.tracks.code().to_string())]));
                        }
                    });
                });
            if media.is_empty() && self.urls.is_empty() && self.captures.is_empty() {
                empty_state(ui, "🎬", "Nothing to capture here", "This page has no video or audio. Press play first if its player loads on demand, then ⟳ Refresh.");
            }
        });
    }

    fn media_row(&self, ui: &mut egui::Ui, m: &PageMedia, page_title: &str) {
        let p = theme::palette(ui);
        egui::Frame::new()
            .fill(p.surface)
            .corner_radius(8)
            .inner_margin(Margin::symmetric(10, 6))
            .outer_margin(Margin { left: 0, right: 0, top: 0, bottom: 4 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if m.tag == "audio" { "🔊" } else { "🎞" }).size(16.0));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&m.tag).strong());
                            if m.protected {
                                ui.label(RichText::new("🔒 Protected (DRM)").small().color(p.warn)).on_hover_text(
                                    "This media is played through the browser's content protection. Its stream can't be recorded, \
                                     and browsers black it out in screen captures.",
                                );
                            }
                            ui.label(RichText::new(m.summary()).small().color(p.muted));
                        });
                        let mut meta = Vec::new();
                        if m.source == "file" {
                            meta.push(m.src.clone());
                        }
                        if !m.top {
                            meta.push(format!("in frame {}", crate::url::display_host(&m.frame_url)));
                        }
                        if !meta.is_empty() {
                            ui.add(egui::Label::new(RichText::new(meta.join(" · ")).small().monospace().color(p.muted)).truncate());
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let element = self.settings.method == Method::Element && !m.protected;
                        let method = if element { Method::Element } else { Method::Screen };
                        let tip = match method {
                            Method::Element => "Record its own stream in a capture window",
                            Method::Screen => "Record what the browser shows of it, in a capture window",
                        };
                        let label = if method == Method::Screen { "● Record on screen" } else { "● Record" };
                        let r = ui.add(primary_button(ui, label)).on_hover_text(tip);
                        if r.clicked() {
                            self.record(m, method, page_title);
                        }
                        if icon_button(ui, "🎯", "Show it on the page").clicked()
                            && let Some(tab) = self.tab
                        {
                            let (frame, id) = (m.frame_id, m.id);
                            self.run(async move { pf::capture_flash(tab, frame, id).await }, Reply::Done);
                        }
                    });
                });
            });
    }

    fn urls_ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions, page_title: &str) {
        let p = theme::palette(ui);
        let mut seen = BTreeSet::new();
        let mut rows: Vec<(SourceKind, &PageMediaUrl)> = Vec::new();
        let mut segments = 0;
        for u in &self.urls {
            if !seen.insert(u.url.as_str()) {
                continue;
            }
            let kind = stream::classify(&u.url);
            if kind == SourceKind::Segment {
                segments += 1;
                if !self.show_segments {
                    continue;
                }
            }
            rows.push((kind, u));
        }
        // Manifests first, then files.
        rows.sort_by_key(|(k, _)| match k {
            SourceKind::Hls | SourceKind::Dash => 0,
            SourceKind::File => 1,
            SourceKind::Unknown => 2,
            SourceKind::Segment => 3,
        });
        let mut save = None;
        egui::CollapsingHeader::new(RichText::new(format!("📡 Streams & files ({})", rows.len())).strong())
            .id_salt("capture-urls")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Saved as they are (original quality, faster than playing them), fetched the way the page's player does.")
                        .small()
                        .color(p.muted),
                );
                if segments > 0 {
                    ui.checkbox(&mut self.show_segments, format!("Show the {segments} stream segment(s) too"));
                }
                if rows.is_empty() {
                    ui.label(RichText::new("No stream or media file requests seen yet (play the media, then ⟳ Refresh).").color(p.muted));
                }
                for (kind, u) in &rows {
                    egui::Frame::new()
                        .fill(p.surface)
                        .corner_radius(8)
                        .inner_margin(Margin::symmetric(10, 6))
                        .outer_margin(Margin { left: 0, right: 0, top: 0, bottom: 4 })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                badge(ui, kind.label(), matches!(kind, SourceKind::Hls | SourceKind::Dash));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if *kind != SourceKind::Segment && ui.button("⬇ Save…").on_hover_text("Open it in a capture window to save it").clicked() {
                                        save = Some((*u).clone());
                                    }
                                    if icon_button(ui, "⧉", "Copy the URL").clicked() {
                                        ui.ctx().copy_text(u.url.clone());
                                        acts.notice("URL copied");
                                    }
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        ui.add(egui::Label::new(RichText::new(&u.url).monospace().small()).truncate().selectable(true)).on_hover_text(&u.url);
                                    });
                                });
                            });
                        });
                }
            });
        if let Some(u) = save {
            self.save_url(&u, page_title);
        }
    }

    fn captures_ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        egui::CollapsingHeader::new(RichText::new(format!("⏺ Capture windows ({})", self.captures.len())).strong())
            .id_salt("capture-windows")
            .default_open(true)
            .show(ui, |ui| {
                for c in self.captures.clone() {
                    egui::Frame::new()
                        .fill(p.surface)
                        .corner_radius(8)
                        .inner_margin(Margin::symmetric(10, 6))
                        .outer_margin(Margin { left: 0, right: 0, top: 0, bottom: 4 })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let color = if c.busy { p.danger } else { p.success };
                                ui.label(RichText::new("●").color(color));
                                ui.label(RichText::new(&c.state).strong());
                                ui.add(egui::Label::new(RichText::new(&c.title).color(p.muted)).truncate());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if c.busy && ui.add(danger_button(ui, "■ Stop")).clicked() {
                                        let id = c.id.clone();
                                        spawn_local(async move { pf::capture_command(&id, "stop").await });
                                        acts.notice("Stopping…");
                                    }
                                    if let Some(t) = c.window_tab
                                        && ui.button("Show").on_hover_text("Bring the capture window to the front").clicked()
                                    {
                                        self.run(async move { pf::focus_tab(t).await }, Reply::Done);
                                    }
                                    let mut stats = Vec::new();
                                    if c.ms > 0.0 {
                                        stats.push(format_duration(c.ms));
                                    }
                                    if c.bytes > 0 {
                                        stats.push(format_bytes(c.bytes));
                                    }
                                    if c.takes > 0 {
                                        stats.push(format!("{} file(s)", c.takes));
                                    }
                                    ui.label(RichText::new(stats.join(" · ")).small().color(p.muted));
                                });
                            });
                        });
                }
            });
        ui.add_space(4.0);
    }
}
