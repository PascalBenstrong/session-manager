//! Capture window (`capture.html`): records a page's media element, or saves a stream or a
//! media file, and sends the results ("takes") where they should go: a file, an upload, or
//! the window's own player (on any audio output, in picture-in-picture or full screen).
//!
//! `capture.html?tab=ID&frame=F&el=E&method=element|screen&tracks=av|v|a`: a page's element.
//! `capture.html?url=URL[&tab=ID&frame=F]`: a stream or file (fetched in that frame of the
//! page, as its player does, when a tab is given).
//!
//! The work happens in `web/capture-engine.js`; this is the window's controls. The window
//! publishes what it's doing (storage.session) for the inspector's Capture view.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use eframe::egui::{self, Align, Layout, Margin, RichText};
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::Actions;
use super::offload;
use super::theme;
use super::widgets::{danger_button, icon_button, primary_button, section_card, segmented, text_input};
use crate::capture::{self, CaptureSettings, Format, Method, Quality, Tracks, ext_for_mime, file_name, format_bytes, format_duration};
use crate::model::TabId;
use crate::platform as pf;
use crate::stream::{self, Master, MediaPlaylist, Mpd, Playlist, SourceKind, Track, TrackKind};

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Caps {
    mp4: bool,
    screen: bool,
    sinks: bool,
}

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
struct TakeInfo {
    id: u32,
    /// "recording" or "download".
    kind: String,
    label: String,
    mime: String,
    ext: String,
    bytes: u64,
    ms: f64,
}

struct Take {
    info: TakeInfo,
    name: String,
    saved: bool,
    uploaded: Option<String>,
    /// Upload in progress: (sent, total).
    uploading: Option<(u64, u64)>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
struct Outputs {
    can_choose: bool,
    devices: Vec<Device>,
    named: bool,
    select: bool,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Device {
    id: String,
    label: String,
}

/// An engine event (see web/capture-engine.js).
#[derive(Deserialize, Default)]
#[serde(default)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    bytes: u64,
    ms: f64,
    take: Value,
    note: String,
    message: String,
    job: u32,
    done: u32,
    total: u64,
    loaded: u64,
    cmd: String,
}

#[derive(Clone)]
enum Source {
    Element { tab: TabId, frame: i64, el: u64, label: String },
    Url { url: String, via: Option<(TabId, i64)> },
    Nothing,
}

/// What a URL turned out to be.
enum Stream {
    Probing,
    Failed(String),
    // Shared: the window draws from them every frame.
    Hls(Rc<Master>),
    HlsMedia(MediaPlaylist),
    Dash(Rc<Mpd>),
    File { mime: String, length: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Starting,
    Recording,
    Paused,
    Stopping,
}

struct Job {
    id: u32,
    label: String,
    done: u32,
    total: u64,
    bytes: u64,
    finished: bool,
}

enum Reply {
    Init(Result<Caps, String>),
    Settings(Option<CaptureSettings>),
    Started(Result<Value, String>),
    Screen(Result<Value, String>),
    Inspected(Result<Stream, String>),
    Loaded(Result<Value, String>),
    Downloaded(Result<(), String>),
    Uploaded(u32, Result<Value, String>),
    Fixed(u32, Result<u64, String>),
    Outputs(Result<Outputs, String>),
    Asked(Result<Value, String>),
    Done(Result<(), String>),
}

#[derive(Default)]
struct Inbox {
    replies: Vec<Reply>,
    events: Vec<Event>,
}

/// What to download: a ready track, or one whose media playlist must be read first.
enum Planned {
    Ready(Track),
    Playlist { url: String, kind: TrackKind, label: String },
}

pub struct CaptureWindow {
    ctx: egui::Context,
    inbox: Rc<RefCell<Inbox>>,
    source: Source,
    title: String,
    caps: Caps,
    settings: CaptureSettings,
    phase: Phase,
    live_bytes: u64,
    live_ms: f64,
    started_at: f64,
    /// The recording plays in the window's player as it's made.
    live_preview: bool,
    /// What the user shares for a screen recording ("Tab · 1920 × 1080 · with sound").
    screen: Option<String>,
    stream: Stream,
    /// The window's player can play the URL (and record it while it plays).
    playable: Option<bool>,
    variant: usize,
    audio_choice: usize,
    video_rep: usize,
    audio_rep: usize,
    subs_rep: Option<usize>,
    jobs: Vec<Job>,
    next_job: u32,
    cancelled: Rc<Cell<bool>>,
    downloading: bool,
    takes: Vec<Take>,
    outputs: Outputs,
    upload_open: bool,
    notes: Vec<String>,
    error: Option<String>,
    shown_title: String,
}

/// A local time stamp for file names ("2026-10-27 14-03-22").
fn stamp() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds()
    )
}

fn parse<T: for<'de> Deserialize<'de>>(r: Result<Value, String>) -> Result<T, String> {
    r.and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
}

fn via_json(via: Option<(TabId, i64)>) -> Value {
    via.map_or(Value::Null, |(tab, frame)| json!({ "tab": tab, "frame": frame }))
}

/// What's at a URL: an HLS playlist, a DASH manifest, or a media file.
async fn inspect_url(url: String, via: Option<(TabId, i64)>) -> Result<Stream, String> {
    let probe = pf::capture_call("probe", &json!({ "url": url, "via": via_json(via) })).await?;
    let text = probe["text"].as_str().unwrap_or("");
    let kind = stream::sniff(text).or_else(|| match stream::classify(&url) {
        k @ (SourceKind::Hls | SourceKind::Dash) if !text.is_empty() => Some(k),
        _ => None,
    });
    let Some(kind) = kind else {
        return Ok(Stream::File {
            mime: probe["type"].as_str().unwrap_or("").split(';').next().unwrap_or("").to_string(),
            length: probe["length"].as_u64().unwrap_or(0),
        });
    };
    let text = if probe["complete"].as_bool() == Some(true) {
        text.to_string()
    } else {
        pf::capture_call("fetchText", &json!({ "url": url, "via": via_json(via) })).await?.as_str().unwrap_or("").to_string()
    };
    match kind {
        SourceKind::Dash => stream::parse_mpd(&text, &url).map(|m| Stream::Dash(Rc::new(m))),
        _ => match stream::parse_hls(&text, &url)? {
            Playlist::Master(m) => Ok(Stream::Hls(Rc::new(m))),
            Playlist::Media(m) => Ok(Stream::HlsMedia(m)),
        },
    }
}

async fn media_playlist(url: &str, via: Option<(TabId, i64)>) -> Result<MediaPlaylist, String> {
    let text = pf::capture_call("fetchText", &json!({ "url": url, "via": via_json(via) })).await?;
    match stream::parse_hls(text.as_str().unwrap_or(""), url)? {
        Playlist::Media(m) => Ok(m),
        Playlist::Master(_) => Err("a variant playlist lists other playlists".into()),
    }
}

const ENCRYPTED: &str = "This stream is encrypted, so its pieces can't be saved as they are. Record it while it plays instead \
                         (open the page's video in the Capture view).";

impl CaptureWindow {
    pub fn new(ctx: &egui::Context) -> Self {
        let param = |k: &str| pf::location_param(k);
        let tab = param("tab").parse::<TabId>().ok();
        let frame = param("frame").parse::<i64>().unwrap_or(0);
        let url = param("url");
        let source = match (tab, param("el").parse::<u64>().ok(), url.is_empty()) {
            (Some(tab), Some(el), _) => Source::Element { tab, frame, el, label: param("label") },
            (_, _, false) => Source::Url { url: url.clone(), via: tab.map(|t| (t, frame)) },
            _ => Source::Nothing,
        };
        let mut title = param("title");
        if title.is_empty() && !url.is_empty() {
            title = capture::title_from_url(&url);
        }
        let id = format!("{:x}{:05x}", pf::now() as u64, (js_sys::Math::random() * 1_048_576.0) as u64);
        let mut w = Self {
            ctx: ctx.clone(),
            inbox: Rc::new(RefCell::new(Inbox::default())),
            source,
            title,
            caps: Caps::default(),
            settings: CaptureSettings::default(),
            phase: Phase::Idle,
            live_bytes: 0,
            live_ms: 0.0,
            started_at: 0.0,
            live_preview: false,
            screen: None,
            stream: Stream::Probing,
            playable: None,
            variant: 0,
            audio_choice: 0,
            video_rep: 0,
            audio_rep: 0,
            subs_rep: None,
            jobs: Vec::new(),
            next_job: 1,
            cancelled: Rc::new(Cell::new(false)),
            downloading: false,
            takes: Vec::new(),
            outputs: Outputs::default(),
            upload_open: false,
            notes: Vec::new(),
            error: None,
            shown_title: String::new(),
        };
        {
            let inbox = w.inbox.clone();
            let ctx = w.ctx.clone();
            let cb = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
                if let Some(e) = v.as_string().and_then(|s| serde_json::from_str::<Event>(&s).ok()) {
                    inbox.borrow_mut().events.push(e);
                    ctx.request_repaint();
                }
            });
            if let Err(e) = pf::capture_set_listener(cb.as_ref().unchecked_ref()) {
                w.error = Some(format!("the capture engine didn't load: {e}"));
            }
            cb.forget();
        }
        let tab = match &w.source {
            Source::Element { tab, .. } => Some(*tab),
            Source::Url { via, .. } => via.map(|v| v.0),
            Source::Nothing => None,
        };
        let init = json!({ "id": id, "title": w.title, "tab": tab });
        w.run(async move { pf::capture_call("init", &init).await }, |r| Reply::Init(parse(r)));
        w.run(
            async { pf::storage_get(capture::SETTINGS_KEY).await.ok().flatten().and_then(|s| serde_json::from_str(&s).ok()) },
            Reply::Settings,
        );
        w.load_outputs();
        match w.source.clone() {
            Source::Url { url, via } => {
                w.run(async move { inspect_url(url, via).await }, Reply::Inspected);
            }
            Source::Element { .. } => {}
            Source::Nothing => w.error = Some("Nothing to capture: open this window from the inspector's Capture view.".into()),
        }
        w
    }

    fn run<T: 'static>(&self, fut: impl std::future::Future<Output = T> + 'static, reply: impl FnOnce(T) -> Reply + 'static) {
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        spawn_local(async move {
            let r = fut.await;
            inbox.borrow_mut().replies.push(reply(r));
            ctx.request_repaint();
        });
    }

    fn call(&self, name: &'static str, args: Value, reply: impl FnOnce(Result<Value, String>) -> Reply + 'static) {
        self.run(async move { pf::capture_call(name, &args).await }, reply);
    }

    fn tell(&self, name: &'static str, args: Value) {
        self.call(name, args, |r| Reply::Done(r.map(drop)));
    }

    fn save_settings(&self) {
        let json = serde_json::to_string(&self.settings).unwrap_or_default();
        spawn_local(async move {
            let _ = pf::storage_set(capture::SETTINGS_KEY, &json).await;
        });
    }

    fn load_outputs(&self) {
        self.call("outputs", json!({}), |r| Reply::Outputs(parse(r)));
    }

    fn busy(&self) -> bool {
        self.phase != Phase::Idle || self.downloading
    }

    fn state_text(&self) -> String {
        match self.phase {
            Phase::Starting => "Starting…".into(),
            Phase::Recording => "Recording".into(),
            Phase::Paused => "Paused".into(),
            Phase::Stopping => "Finishing…".into(),
            Phase::Idle if self.downloading => {
                let (done, total) = self.jobs.iter().filter(|j| !j.finished).map(|j| (j.done as u64, j.total)).next().unwrap_or((0, 0));
                if total > 0 { format!("Downloading {done} / {total}") } else { "Downloading…".into() }
            }
            Phase::Idle if !self.takes.is_empty() => "Ready".into(),
            Phase::Idle => "Waiting".into(),
        }
    }

    // ------------------------------------------------------------ actions

    fn record_args(&self) -> Value {
        let mut tracks = self.settings.tracks;
        if let Source::Element { label, .. } = &self.source
            && label.starts_with("audio")
        {
            tracks = Tracks::Audio;
        }
        json!({
            "tracks": tracks.code(),
            "format": self.settings.format.code(),
            "videoBps": self.settings.quality.video_bps(),
            "fromStart": self.settings.from_start,
            "stopAtEnd": self.settings.stop_at_end,
            "listenHere": self.settings.listen_here,
            "crop": self.settings.crop,
            "label": self.take_label(),
        })
    }

    fn take_label(&self) -> String {
        let n = self.takes.iter().filter(|t| t.info.kind == "recording").count() + 1;
        let what = match self.settings.tracks {
            Tracks::Both => "recording",
            Tracks::Video => "video",
            Tracks::Audio => "sound",
        };
        if n > 1 { format!("{what} {n}") } else { what.to_string() }
    }

    fn start(&mut self) {
        let mut args = self.record_args();
        let op = match &self.source {
            Source::Element { tab, frame, el, .. } => {
                args["tab"] = json!(tab);
                args["frame"] = json!(frame);
                args["el"] = json!(el);
                if self.settings.method == Method::Screen { "recordScreen" } else { "recordElement" }
            }
            Source::Url { .. } => "recordPlayer",
            Source::Nothing => return,
        };
        self.phase = Phase::Starting;
        self.error = None;
        self.live_bytes = 0;
        self.live_ms = 0.0;
        self.call(op, args, Reply::Started);
    }

    fn stop(&mut self) {
        if matches!(self.phase, Phase::Recording | Phase::Paused) {
            self.phase = Phase::Stopping;
            self.tell("stop", json!({}));
        }
        if self.downloading {
            self.cancel_downloads();
        }
    }

    fn cancel_downloads(&mut self) {
        self.cancelled.set(true);
        for j in self.jobs.iter().filter(|j| !j.finished) {
            self.tell("cancel", json!({ "job": j.id }));
        }
    }

    /// The tracks to download for the current choices.
    fn plan(&mut self) -> Result<Vec<Planned>, String> {
        let tracks = self.settings.tracks;
        let mut out = Vec::new();
        match &self.stream {
            Stream::Hls(m) => {
                if m.encrypted {
                    return Err(ENCRYPTED.into());
                }
                let variants = m.sorted_variants();
                let v = variants.get(self.variant).or(variants.first()).ok_or("no stream to save")?;
                let audio = m.audio_renditions(v.audio.as_deref());
                let audio_pick = audio.get(self.audio_choice).or(audio.iter().find(|r| r.default)).or(audio.first()).copied();
                let separate_audio = audio_pick.is_some();
                if tracks.video() || !separate_audio {
                    let kind = if separate_audio || v.audio_only() {
                        if v.audio_only() { TrackKind::Audio } else { TrackKind::Video }
                    } else {
                        TrackKind::Muxed
                    };
                    if tracks == Tracks::Video && kind == TrackKind::Muxed {
                        self.notes.push("The picture and sound of this stream come together: the file has both.".into());
                    }
                    if tracks == Tracks::Audio && kind != TrackKind::Audio {
                        self.notes.push("This stream has no separate sound: the file has the picture too.".into());
                    }
                    out.push(Planned::Playlist { url: v.url.clone(), kind, label: v.label() });
                }
                if tracks.audio()
                    && let Some(r) = audio_pick
                    && let Some(url) = &r.url
                {
                    out.push(Planned::Playlist { url: url.clone(), kind: TrackKind::Audio, label: format!("sound {}", r.label()) });
                }
            }
            Stream::HlsMedia(m) => {
                if m.encrypted {
                    return Err(ENCRYPTED.into());
                }
                out.push(Planned::Ready(m.track(TrackKind::Muxed, "stream")));
            }
            Stream::Dash(mpd) => {
                let video = mpd.of_kind(TrackKind::Video);
                let audio = mpd.of_kind(TrackKind::Audio);
                let subs = mpd.of_kind(TrackKind::Subtitles);
                let mut chosen = Vec::new();
                if tracks.video()
                    && let Some(r) = video.get(self.video_rep).or(video.first())
                {
                    chosen.push(*r);
                }
                if tracks.audio()
                    && let Some(r) = audio.get(self.audio_rep).or(audio.first())
                {
                    chosen.push(*r);
                }
                if let Some(r) = self.subs_rep.and_then(|i| subs.get(i)) {
                    chosen.push(*r);
                }
                if chosen.iter().any(|r| r.protected) {
                    return Err("This stream is protected (DRM): it can't be saved, and browsers black it out in recordings.".into());
                }
                for r in chosen {
                    let label = match r.kind {
                        TrackKind::Audio => format!("sound {}", r.label()),
                        TrackKind::Subtitles => format!("subtitles {}", r.language),
                        _ => r.label(),
                    };
                    let mut t = r.track(mpd.duration);
                    t.label = label;
                    out.push(Planned::Ready(t));
                }
            }
            Stream::File { mime, .. } => {
                let url = match &self.source {
                    Source::Url { url, .. } => url.clone(),
                    _ => return Err("no file".into()),
                };
                let mut ext = stream::extension(&url);
                if ext.is_empty() {
                    ext = ext_for_mime(mime, mime.starts_with("audio")).to_string();
                }
                out.push(Planned::Ready(Track {
                    kind: TrackKind::Muxed,
                    label: String::new(),
                    ext,
                    parts: vec![stream::Part { url, range: None }],
                    duration: 0.0,
                }));
            }
            Stream::Probing | Stream::Failed(_) => return Err("nothing to save yet".into()),
        }
        if out.is_empty() {
            return Err("nothing to save with these choices".into());
        }
        Ok(out)
    }

    fn download(&mut self) {
        self.notes.clear();
        let plan = match self.plan() {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let via = match &self.source {
            Source::Url { via, .. } => *via,
            _ => None,
        };
        let mut planned = Vec::new();
        for p in plan {
            let id = self.next_job;
            self.next_job += 1;
            let label = match &p {
                Planned::Ready(t) => t.label.clone(),
                Planned::Playlist { label, .. } => label.clone(),
            };
            self.jobs.push(Job { id, label, done: 0, total: 0, bytes: 0, finished: false });
            planned.push((id, p));
        }
        self.downloading = true;
        self.error = None;
        self.cancelled = Rc::new(Cell::new(false));
        let cancelled = self.cancelled.clone();
        let parallel = self.settings.parallel.clamp(1, 8);
        self.run(
            async move {
                for (job, p) in planned {
                    if cancelled.get() {
                        return Err("cancelled".into());
                    }
                    let track = match p {
                        Planned::Ready(t) => t,
                        Planned::Playlist { url, kind, label } => {
                            let m = media_playlist(&url, via).await?;
                            if m.encrypted {
                                return Err(ENCRYPTED.into());
                            }
                            m.track(kind, label)
                        }
                    };
                    let mime = match (track.kind, track.ext.as_str()) {
                        (_, "ts") => "video/mp2t",
                        (TrackKind::Audio, "m4a") => "audio/mp4",
                        (_, "mp4") => "video/mp4",
                        (_, "webm") => "video/webm",
                        (_, "weba") => "audio/webm",
                        (_, "vtt") => "text/vtt",
                        _ => "",
                    };
                    pf::capture_call(
                        "download",
                        &json!({
                            "job": job,
                            "via": via_json(via),
                            "parts": track.parts,
                            "label": track.label,
                            "ext": track.ext,
                            "mime": mime,
                            "duration": track.duration,
                            "parallel": parallel,
                        }),
                    )
                    .await?;
                }
                Ok(())
            },
            Reply::Downloaded,
        );
    }

    fn save(&mut self, i: usize) {
        let Some(t) = self.takes.get(i) else { return };
        self.tell("save", json!({ "take": t.info.id, "name": t.name }));
        self.takes[i].saved = true;
    }

    fn upload(&mut self, i: usize) {
        let Some(t) = self.takes.get_mut(i) else { return };
        let target = self.settings.upload.clone();
        let headers = match target.header_list() {
            Ok(h) => h,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        t.uploading = Some((0, t.info.bytes));
        let id = t.info.id;
        let args = json!({
            "take": id,
            "url": target.url.trim(),
            "method": target.method,
            "field": target.field.trim(),
            "headers": headers,
            "name": t.name,
        });
        self.call("upload", args, move |r| Reply::Uploaded(id, r));
    }

    fn add_take(&mut self, info: TakeInfo, note: &str) {
        let audio_only = info.mime.starts_with("audio") || info.label.starts_with("sound");
        let ext = if info.ext.is_empty() { ext_for_mime(&info.mime, audio_only).to_string() } else { info.ext.clone() };
        let name = file_name(&self.title, &info.label, &stamp(), &ext);
        // Recordings get their duration written in, so players can seek in them.
        if info.mime.contains("webm") && info.ms > 0.0 && info.bytes > 0 {
            let (id, ms, n) = (info.id, info.ms, (info.bytes as usize).min(1 << 20));
            self.run(
                async move {
                    let head = pf::capture_head(id, n).await?;
                    let fixed = offload::set_duration(&head, ms).await?;
                    pf::capture_replace_head(id, &fixed, head.len())
                },
                move |r| Reply::Fixed(id, r),
            );
        }
        if !note.is_empty() {
            self.notes.push(note.to_string());
        }
        self.takes.push(Take { info, name, saved: false, uploaded: None, uploading: None });
    }

    // ------------------------------------------------------------ events

    fn drain(&mut self, acts: &mut Actions) {
        let (replies, events) = {
            let mut inbox = self.inbox.borrow_mut();
            (std::mem::take(&mut inbox.replies), std::mem::take(&mut inbox.events))
        };
        for r in replies {
            match r {
                Reply::Init(Ok(caps)) => {
                    self.caps = caps;
                    if !self.caps.mp4 {
                        self.settings.format = Format::WebM;
                    }
                }
                Reply::Init(Err(e)) => self.error = Some(format!("the capture engine didn't start: {e}")),
                Reply::Settings(saved) => {
                    let mut s = saved.unwrap_or_default();
                    // What the Capture view chose for this window.
                    let tracks = pf::location_param("tracks");
                    if !tracks.is_empty() {
                        s.tracks = Tracks::from_code(&tracks);
                    }
                    let method = pf::location_param("method");
                    if !method.is_empty() {
                        s.method = Method::from_code(&method);
                    }
                    if !s.output.is_empty() {
                        self.tell("setOutput", json!({ "id": s.output }));
                    }
                    self.settings = s;
                }
                Reply::Started(Ok(v)) => {
                    self.phase = Phase::Recording;
                    self.started_at = pf::now();
                    self.live_preview = v["preview"].as_bool().unwrap_or(false);
                    if let Some(note) = v["note"].as_str().filter(|n| !n.is_empty()) {
                        self.notes.push(note.to_string());
                    }
                }
                Reply::Started(Err(e)) => {
                    self.phase = Phase::Idle;
                    self.error = Some(e);
                }
                Reply::Screen(Ok(v)) => {
                    let surface = match v["surface"].as_str().unwrap_or("") {
                        "browser" => "Tab",
                        "window" => "Window",
                        "monitor" => "Screen",
                        _ => "Shared",
                    };
                    let mut s = format!("{surface} · {} × {}", v["width"].as_u64().unwrap_or(0), v["height"].as_u64().unwrap_or(0));
                    s.push_str(if v["audio"].as_bool() == Some(true) { " · with sound" } else { " · no sound" });
                    self.screen = Some(s);
                    self.error = None;
                }
                Reply::Screen(Err(e)) => self.error = Some(e),
                Reply::Inspected(Ok(s)) => {
                    let hide_player = matches!(s, Stream::Hls(_) | Stream::HlsMedia(_) | Stream::Dash(_));
                    match &s {
                        Stream::Hls(m) => {
                            // The best variant, with the default sound.
                            self.variant = 0;
                            if let Some(v) = m.sorted_variants().first() {
                                let audio = m.audio_renditions(v.audio.as_deref());
                                self.audio_choice = audio.iter().position(|r| r.default).unwrap_or(0);
                            }
                        }
                        Stream::HlsMedia(m) if !m.ended => {
                            self.notes.push("This is a live stream: only the part available now is saved.".into())
                        }
                        Stream::Dash(mpd) => {
                            if mpd.live {
                                self.notes.push("This is a live stream: only the part listed now is saved.".into());
                            }
                            if mpd.more_periods {
                                self.notes.push("The stream has several parts (periods, often ads): the first is saved.".into());
                            }
                        }
                        _ => {}
                    }
                    self.tell("showPreview", json!({ "on": !hide_player }));
                    if let (Stream::File { .. }, Source::Url { url, .. }) = (&s, &self.source) {
                        self.call("loadUrl", json!({ "url": url }), Reply::Loaded);
                    }
                    self.stream = s;
                }
                Reply::Inspected(Err(e)) => {
                    self.tell("showPreview", json!({ "on": false }));
                    self.stream = Stream::Failed(e);
                }
                Reply::Loaded(Ok(v)) => self.playable = Some(v["recordable"].as_bool().unwrap_or(false)),
                Reply::Loaded(Err(_)) => self.playable = Some(false),
                Reply::Downloaded(r) => {
                    self.downloading = false;
                    for j in &mut self.jobs {
                        j.finished = true;
                    }
                    match r {
                        Ok(()) => acts.notice("Downloaded: save or upload it below"),
                        Err(e) if e == "cancelled" => acts.notice("Download cancelled"),
                        Err(e) => self.error = Some(e),
                    }
                }
                Reply::Uploaded(id, r) => {
                    if let Some(t) = self.takes.iter_mut().find(|t| t.info.id == id) {
                        t.uploading = None;
                        match r {
                            Ok(v) => {
                                let status = v["status"].as_u64().unwrap_or(0);
                                let at = v["location"].as_str().filter(|l| !l.is_empty()).map(|l| format!(" → {l}")).unwrap_or_default();
                                t.uploaded = Some(format!("Uploaded ({status}){at}"));
                                t.saved = true;
                                acts.notice("Uploaded");
                            }
                            Err(e) => self.error = Some(format!("Upload failed: {e}")),
                        }
                    }
                }
                Reply::Fixed(id, Ok(size)) => {
                    if let Some(t) = self.takes.iter_mut().find(|t| t.info.id == id) {
                        t.info.bytes = size;
                    }
                }
                Reply::Fixed(_, Err(e)) => web_sys::console::warn_1(&format!("could not write the recording's duration: {e}").into()),
                Reply::Outputs(Ok(o)) => self.outputs = o,
                Reply::Outputs(Err(_)) => {}
                Reply::Asked(Ok(v)) => {
                    if let Some(id) = v["chosen"].as_str().filter(|s| !s.is_empty()) {
                        self.settings.output = id.to_string();
                        self.save_settings();
                        self.tell("setOutput", json!({ "id": id }));
                    }
                    self.load_outputs();
                }
                Reply::Asked(Err(e)) => self.error = Some(format!("No output chosen: {e}")),
                Reply::Done(Ok(())) => {}
                Reply::Done(Err(e)) => self.error = Some(e),
            }
        }
        for e in events {
            match e.kind.as_str() {
                "level" => {
                    self.live_bytes = e.bytes;
                    self.live_ms = e.ms;
                }
                "take" => {
                    let Ok(info) = serde_json::from_value::<TakeInfo>(e.take) else { continue };
                    if info.kind == "recording" {
                        self.phase = Phase::Idle;
                        self.live_preview = false;
                        self.screen = None;
                    }
                    self.add_take(info, &e.note);
                }
                "error" => self.error = Some(e.message),
                "note" => self.notes.push(e.message),
                "progress" => {
                    if let Some(j) = self.jobs.iter_mut().find(|j| j.id == e.job) {
                        j.done = e.done;
                        j.total = e.total;
                        j.bytes = e.bytes;
                        j.finished = e.total > 0 && e.done as u64 >= e.total;
                    }
                }
                "upload" => {
                    let id = e.take.as_u64().unwrap_or(0) as u32;
                    if let Some(t) = self.takes.iter_mut().find(|t| t.info.id == id) {
                        t.uploading = Some((e.loaded, e.total));
                    }
                }
                "command" if e.cmd == "stop" => self.stop(),
                "screenEnded" => self.screen = None,
                _ => {}
            }
        }
    }

    /// Shows what the window does in its title. (The engine tells the inspector's Capture
    /// view: this only runs while the window is shown.)
    fn publish(&mut self) {
        let title = match self.phase {
            Phase::Recording => format!("● {} — Capture", self.title),
            _ if self.downloading => format!("⬇ {} — Capture", self.title),
            _ => format!("{} — Capture", if self.title.is_empty() { "Session Manager" } else { &self.title }),
        };
        if title != self.shown_title {
            self.shown_title = title.clone();
            self.tell("setTitle", json!({ "title": title }));
        }
    }

    // ------------------------------------------------------------ UI

    pub fn ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        self.drain(acts);
        self.publish();
        if self.busy() {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        let p = theme::palette(ui);
        egui::Panel::top("capture-header")
            .frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, 8)))
            .show(ui, |ui| self.header(ui));
        if let Some(e) = self.error.clone() {
            egui::Panel::bottom("capture-error").frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, 6))).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(format!("⚠ {e}")).color(p.danger)).wrap());
                        if icon_button(ui, "🗙", "Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                },
            );
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(12))).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (i, n) in self.notes.clone().iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(format!("ℹ {n}")).color(p.warn)).wrap());
                        if icon_button(ui, "🗙", "Dismiss").clicked() {
                            self.notes.remove(i);
                        }
                    });
                }
                match self.source.clone() {
                    Source::Element { tab, frame, el, .. } => self.record_ui(ui, acts, Some((tab, frame, el))),
                    Source::Url { .. } => self.stream_ui(ui, acts),
                    Source::Nothing => {}
                }
                self.takes_ui(ui, acts);
                self.sinks_ui(ui, acts);
            });
        });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            let (dot, color) = match self.phase {
                Phase::Recording => ("●", p.danger),
                Phase::Paused => ("❚❚", p.warn),
                _ if self.downloading => ("⬇", p.accent),
                _ => ("⏺", p.accent),
            };
            ui.label(RichText::new(dot).size(18.0).color(color));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let title = if self.title.is_empty() { "Capture" } else { &self.title };
                ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
                let what = match &self.source {
                    Source::Element { label, .. } => format!(
                        "{label} · {}",
                        if self.settings.method == Method::Screen { "rendered on screen" } else { "element stream" }
                    ),
                    Source::Url { url, .. } => url.clone(),
                    Source::Nothing => String::new(),
                };
                ui.add(egui::Label::new(RichText::new(what).small().color(p.muted)).truncate());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut s = self.state_text();
                if matches!(self.phase, Phase::Recording | Phase::Paused) {
                    s = format!("{s} {} · {}", format_duration(self.live_ms), format_bytes(self.live_bytes));
                }
                ui.label(RichText::new(s).color(p.muted));
            });
        });
    }

    fn options_ui(&mut self, ui: &mut egui::Ui, record: bool) {
        let p = theme::palette(ui);
        let idle = !self.busy();
        ui.add_enabled_ui(idle, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Keep").color(p.muted));
                if segmented(ui, &mut self.settings.tracks, &Tracks::ALL) {
                    self.save_settings();
                }
            });
            if !record {
                return;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Format").color(p.muted));
                let before = (self.settings.format, self.settings.quality);
                egui::ComboBox::from_id_salt("capture-format").selected_text(self.settings.format.label()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.format, Format::WebM, "WebM");
                    if self.caps.mp4 {
                        ui.selectable_value(&mut self.settings.format, Format::Mp4, "MP4");
                    }
                });
                ui.label(RichText::new("Quality").color(p.muted));
                egui::ComboBox::from_id_salt("capture-quality").selected_text(self.settings.quality.label()).show_ui(ui, |ui| {
                    for q in Quality::ALL {
                        ui.selectable_value(&mut self.settings.quality, q, q.label());
                    }
                });
                if before != (self.settings.format, self.settings.quality) {
                    self.save_settings();
                }
            });
        });
    }

    fn record_ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions, element: Option<(TabId, i64, u64)>) {
        let p = theme::palette(ui);
        let idle = !self.busy();
        section_card(ui, "Record", "", |ui| {
            if element.is_some() {
                ui.add_enabled_ui(idle, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Record").color(p.muted));
                        if segmented(ui, &mut self.settings.method, &Method::ALL) {
                            self.save_settings();
                        }
                    });
                });
                ui.label(
                    RichText::new(match self.settings.method {
                        Method::Element => "The media's own picture and sound (not for protected media). It records while it plays.",
                        Method::Screen => {
                            "What the browser shows: choose the tab below (turn on its sound there), and only the media's area is kept. \
                             Keep this window open while it records."
                        }
                    })
                    .small()
                    .color(p.muted),
                );
            }
            self.options_ui(ui, true);
            let before = self.settings.clone();
            ui.add_enabled_ui(idle, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.settings.from_start, "From the start");
                    ui.checkbox(&mut self.settings.stop_at_end, "Stop at the end");
                    if element.is_some() && self.settings.method == Method::Element {
                        ui.checkbox(&mut self.settings.listen_here, "Listen here").on_hover_text(
                            "Silence it in the page and play its sound in this window (on the output chosen below). \
                             The sound comes a moment after the picture.",
                        );
                    }
                    if element.is_some() && self.settings.method == Method::Screen {
                        ui.checkbox(&mut self.settings.crop, "Only the media's area").on_hover_text("Otherwise the whole tab is recorded");
                    }
                });
            });
            if self.settings != before {
                self.save_settings();
            }
            let screen = element.is_some() && self.settings.method == Method::Screen;
            if screen && idle {
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(
                        self.caps.screen,
                        egui::Button::new(if self.screen.is_some() { "🖥 Choose again…" } else { "🖥 Choose the tab…" }),
                    );
                    if r.hovered() {
                        acts.arm("capture:screen");
                    }
                    if r.clicked() {
                        self.call("screenPicked", json!({}), Reply::Screen);
                    }
                    match &self.screen {
                        Some(s) => ui.label(RichText::new(format!("Sharing: {s}")).color(p.success)),
                        None => ui.label(RichText::new("Pick the tab the media plays in.").small().color(p.muted)),
                    };
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                match self.phase {
                    Phase::Idle | Phase::Starting => {
                        let can = self.phase == Phase::Idle
                            && !self.downloading
                            && (!screen || self.screen.is_some())
                            && (element.is_some() || self.playable == Some(true));
                        if ui.add_enabled(can, primary_button(ui, "● Start recording")).clicked() {
                            self.start();
                        }
                        if self.phase == Phase::Starting {
                            ui.spinner();
                        }
                    }
                    Phase::Recording | Phase::Paused | Phase::Stopping => {
                        if ui.add_enabled(self.phase != Phase::Stopping, danger_button(ui, "■ Stop")).clicked() {
                            self.stop();
                        }
                        if self.phase == Phase::Recording && ui.button("❚❚ Pause").clicked() {
                            self.phase = Phase::Paused;
                            self.tell("pause", json!({}));
                        } else if self.phase == Phase::Paused && ui.button("▶ Resume").clicked() {
                            self.phase = Phase::Recording;
                            self.tell("resume", json!({}));
                        }
                        let dot = if self.phase == Phase::Recording { "●" } else { "❚❚" };
                        ui.label(
                            RichText::new(format!("{dot} {} · {}", format_duration(self.live_ms), format_bytes(self.live_bytes)))
                                .color(p.danger),
                        );
                        if self.settings.method == Method::Element && element.is_some() {
                            let mut on = self.settings.listen_here;
                            if ui.checkbox(&mut on, "Listen here").changed() {
                                self.settings.listen_here = on;
                                self.save_settings();
                                self.tell("listenHere", json!({ "on": on }));
                            }
                        }
                    }
                }
                if let Some((tab, frame, el)) = element {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, "🎯", "Show it on the page").clicked() {
                            self.tell("flash", json!({ "tab": tab, "frame": frame, "el": el }));
                        }
                        if icon_button(ui, "❚❚", "Pause it in the page").clicked() {
                            self.tell("pageMedia", json!({ "tab": tab, "frame": frame, "el": el, "action": "pause" }));
                        }
                        if icon_button(ui, "▶", "Play it in the page").clicked() {
                            self.tell("pageMedia", json!({ "tab": tab, "frame": frame, "el": el, "action": "play" }));
                        }
                    });
                }
            });
            if matches!(self.phase, Phase::Recording | Phase::Paused) && !self.live_preview && self.settings.method == Method::Element {
                ui.label(
                    RichText::new("The player above shows the recording once it's done (this format can't be shown while it's made).")
                        .small()
                        .color(p.muted),
                );
            }
        });
    }

    fn stream_ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        let title = match &self.stream {
            Stream::Probing => "Looking at the URL…".to_string(),
            Stream::Failed(_) => "Can't read this URL".to_string(),
            Stream::Hls(m) => format!("HLS stream · {} quality level(s)", m.variants.len()),
            Stream::HlsMedia(m) => format!("HLS stream · {} segments · {}", m.segments.len(), format_duration(m.duration * 1000.0)),
            Stream::Dash(mpd) => format!("DASH stream · {}", format_duration(mpd.duration * 1000.0)),
            Stream::File { mime, length } => {
                let mut s = String::from("Media file");
                if !mime.is_empty() {
                    s.push_str(&format!(" · {mime}"));
                }
                if *length > 0 {
                    s.push_str(&format!(" · {}", format_bytes(*length)));
                }
                s
            }
        };
        let idle = !self.busy();
        section_card(ui, &title, "", |ui| {
            match &self.stream {
                Stream::Probing => {
                    ui.spinner();
                    return;
                }
                Stream::Failed(e) => {
                    ui.label(RichText::new(e).color(p.danger));
                    return;
                }
                Stream::Hls(m) => {
                    let m = m.clone();
                    ui.add_enabled_ui(idle, |ui| {
                        let variants = m.sorted_variants();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Quality").color(p.muted));
                            let current = variants.get(self.variant).map(|v| v.label()).unwrap_or_default();
                            egui::ComboBox::from_id_salt("hls-variant").selected_text(current).width(280.0).show_ui(ui, |ui| {
                                for (i, v) in variants.iter().enumerate() {
                                    ui.selectable_value(&mut self.variant, i, v.label());
                                }
                            });
                        });
                        let group = variants.get(self.variant).and_then(|v| v.audio.clone());
                        let audio = m.audio_renditions(group.as_deref());
                        if !audio.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Sound").color(p.muted));
                                let current = audio.get(self.audio_choice).map(|r| r.label()).unwrap_or_default();
                                egui::ComboBox::from_id_salt("hls-audio").selected_text(current).width(280.0).show_ui(ui, |ui| {
                                    for (i, r) in audio.iter().enumerate() {
                                        ui.selectable_value(&mut self.audio_choice, i, r.label());
                                    }
                                });
                            });
                        }
                    });
                    if m.encrypted {
                        ui.label(RichText::new(ENCRYPTED).color(p.warn));
                    }
                }
                Stream::HlsMedia(m) => {
                    if m.encrypted {
                        ui.label(RichText::new(ENCRYPTED).color(p.warn));
                    }
                }
                Stream::Dash(mpd) => {
                    let mpd = mpd.clone();
                    ui.add_enabled_ui(idle, |ui| {
                        let pick = |ui: &mut egui::Ui, id: &str, label: &str, list: &[&stream::Representation], value: &mut usize| {
                            if list.is_empty() {
                                return;
                            }
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(label).color(p.muted));
                                let current = list.get(*value).map(|r| r.label()).unwrap_or_default();
                                egui::ComboBox::from_id_salt(id).selected_text(current).width(280.0).show_ui(ui, |ui| {
                                    for (i, r) in list.iter().enumerate() {
                                        ui.selectable_value(value, i, r.label());
                                    }
                                });
                            });
                        };
                        pick(ui, "dash-video", "Video", &mpd.of_kind(TrackKind::Video), &mut self.video_rep);
                        pick(ui, "dash-audio", "Sound", &mpd.of_kind(TrackKind::Audio), &mut self.audio_rep);
                        let subs = mpd.of_kind(TrackKind::Subtitles);
                        if !subs.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Subtitles").color(p.muted));
                                let current = self.subs_rep.and_then(|i| subs.get(i)).map_or("None".to_string(), |r| r.label());
                                egui::ComboBox::from_id_salt("dash-subs").selected_text(current).width(280.0).show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.subs_rep, None, "None");
                                    for (i, r) in subs.iter().enumerate() {
                                        ui.selectable_value(&mut self.subs_rep, Some(i), r.label());
                                    }
                                });
                            });
                        }
                    });
                    if mpd.representations.iter().any(|r| r.protected) {
                        ui.label(RichText::new("🔒 Parts of this stream are protected (DRM): those can't be saved.").color(p.warn));
                    }
                }
                Stream::File { .. } => {}
            }
            self.options_ui(ui, false);
            if matches!(self.stream, Stream::Hls(_) | Stream::HlsMedia(_) | Stream::Dash(_)) {
                ui.label(
                    RichText::new("Video and sound that the stream keeps apart are saved as separate files (join them with a video tool if you like).")
                        .small()
                        .color(p.muted),
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if self.downloading {
                    if ui.add(danger_button(ui, "Cancel")).clicked() {
                        self.cancel_downloads();
                    }
                } else if ui
                    .add_enabled(self.phase == Phase::Idle, primary_button(ui, "⬇ Download"))
                    .on_hover_text("Fetch it (original quality), then save or upload it")
                    .clicked()
                {
                    self.download();
                }
            });
            for j in &self.jobs {
                ui.horizontal(|ui| {
                    let frac = if j.total > 0 { j.done as f32 / j.total as f32 } else { 0.0 };
                    let label = if j.label.is_empty() { "file" } else { &j.label };
                    let text = if j.total > 1 {
                        format!("{label}: {} / {} · {}", j.done, j.total, format_bytes(j.bytes))
                    } else {
                        format!("{label}: {}", format_bytes(j.bytes))
                    };
                    ui.add(egui::ProgressBar::new(if j.finished { 1.0 } else { frac }).text(text).desired_width(ui.available_width()));
                });
            }
        });
        if matches!(self.stream, Stream::File { .. }) {
            match self.playable {
                Some(true) => self.record_ui(ui, acts, None),
                Some(false) => {
                    ui.label(RichText::new("This media can't be recorded from the player here (its site doesn't allow it, or it doesn't play): download it instead.").small().color(p.muted));
                }
                None => {}
            }
        }
    }

    fn takes_ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        if self.takes.is_empty() {
            return;
        }
        let p = theme::palette(ui);
        let upload_problem = self.settings.upload.problem();
        let mut act: Option<(usize, &str)> = None;
        section_card(ui, "Files", "Kept in this window until you save or upload them.", |ui| {
            for (i, t) in self.takes.iter_mut().enumerate() {
                egui::Frame::new()
                    .fill(p.surface_hover)
                    .corner_radius(8)
                    .inner_margin(Margin::symmetric(10, 6))
                    .outer_margin(Margin { left: 0, right: 0, top: 0, bottom: 4 })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.add(text_input(&mut t.name, "File name", (ui.available_width() - 330.0).max(160.0)));
                            let mut stats = vec![format_bytes(t.info.bytes)];
                            if t.info.ms > 0.0 {
                                stats.push(format_duration(t.info.ms));
                            }
                            ui.label(RichText::new(stats.join(" · ")).small().color(p.muted));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, "🗑", "Discard it").clicked() {
                                    act = Some((i, "discard"));
                                }
                                let r = ui.add_enabled(t.uploading.is_none(), egui::Button::new("⇪ Upload"));
                                let r = match &upload_problem {
                                    Some(why) => r.on_hover_text(format!("{why} (Upload settings, below)")),
                                    None => r.on_hover_text("Send it to the upload address"),
                                };
                                if r.clicked() {
                                    act = Some((i, "upload"));
                                }
                                if ui
                                    .add(primary_button(ui, "💾 Save"))
                                    .on_hover_text("Save it as a file (your browser's downloads)")
                                    .clicked()
                                {
                                    act = Some((i, "save"));
                                }
                                if icon_button(ui, "▶", "Play it in the player above").clicked() {
                                    act = Some((i, "play"));
                                }
                            });
                        });
                        if let Some((sent, total)) = t.uploading {
                            ui.horizontal(|ui| {
                                let frac = if total > 0 { sent as f32 / total as f32 } else { 0.0 };
                                ui.add(
                                    egui::ProgressBar::new(frac)
                                        .text(format!("Uploading {} / {}", format_bytes(sent), format_bytes(total)))
                                        .desired_width(ui.available_width() - 80.0),
                                );
                                if ui.button("Cancel").clicked() {
                                    act = Some((i, "cancelUpload"));
                                }
                            });
                        }
                        let mut status = Vec::new();
                        if t.saved && t.uploaded.is_none() {
                            status.push("Saved".to_string());
                        }
                        if let Some(u) = &t.uploaded {
                            status.push(u.clone());
                        }
                        if !status.is_empty() {
                            ui.label(RichText::new(format!("✔ {}", status.join(" · "))).small().color(p.success));
                        }
                    });
            }
        });
        if let Some((i, what)) = act {
            let id = self.takes[i].info.id;
            match what {
                "save" => self.save(i),
                "upload" if upload_problem.is_some() => {
                    self.upload_open = true;
                    acts.notice(upload_problem.unwrap_or_default());
                }
                "upload" => self.upload(i),
                "cancelUpload" => self.tell("cancelUpload", json!({ "take": id })),
                "play" => {
                    self.tell("showPreview", json!({ "on": true }));
                    self.tell("play", json!({ "take": id }));
                }
                "discard" => {
                    self.tell("discard", json!({ "take": id }));
                    self.takes.remove(i);
                }
                _ => {}
            }
        }
    }

    fn sinks_ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        section_card(ui, "Play and send", "", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Sound plays on").color(p.muted));
                if self.outputs.can_choose && !self.outputs.devices.is_empty() && (self.outputs.named || !self.outputs.select) {
                    let current = self
                        .outputs
                        .devices
                        .iter()
                        .find(|d| d.id == self.settings.output)
                        .map_or("System default".to_string(), |d| d.label.clone());
                    let before = self.settings.output.clone();
                    egui::ComboBox::from_id_salt("capture-output").selected_text(current).width(240.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.settings.output, String::new(), "System default");
                        for d in &self.outputs.devices {
                            ui.selectable_value(&mut self.settings.output, d.id.clone(), &d.label);
                        }
                    });
                    if self.settings.output != before {
                        self.save_settings();
                        self.tell("setOutput", json!({ "id": self.settings.output }));
                    }
                } else if !self.outputs.can_choose && !self.caps.sinks {
                    ui.label(RichText::new("the system default (this browser can't choose)").color(p.muted));
                } else {
                    ui.label(RichText::new("the system default").color(p.muted));
                }
                if self.outputs.can_choose && (!self.outputs.named || self.outputs.select) {
                    let label = if self.outputs.select { "Choose an output…" } else { "Name the outputs…" };
                    let r = ui.button(label).on_hover_text(if self.outputs.select {
                        "The browser lists your speakers and headsets"
                    } else {
                        "The browser lists outputs by name once the extension may use audio devices (it asks once; nothing is recorded)"
                    });
                    if r.hovered() {
                        acts.arm("capture:outputs");
                    }
                    if r.clicked() {
                        self.call("outputsAsked", json!({}), Reply::Asked);
                    }
                }
                let r = ui.button("⧉ Picture-in-picture").on_hover_text("Float the player above other windows");
                if r.hovered() {
                    acts.arm("capture:pip");
                }
            });
            ui.add_space(4.0);
            egui::CollapsingHeader::new(RichText::new("⇪ Upload settings").strong())
                .id_salt("capture-upload")
                .open(if std::mem::take(&mut self.upload_open) { Some(true) } else { None })
                .show(ui, |ui| {
                    let before = self.settings.upload.clone();
                    let u = &mut self.settings.upload;
                    ui.label(
                        RichText::new("Where ⇪ Upload sends a file: your server, a storage bucket's upload URL, a webhook…")
                            .small()
                            .color(p.muted),
                    );
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Address").color(p.muted));
                        ui.add(text_input(&mut u.url, "https://upload.example.com/media", ui.available_width() - 8.0));
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Method").color(p.muted));
                        let mut m = if u.method == "POST" { 1 } else { 0 };
                        if segmented(ui, &mut m, &[(0, "PUT"), (1, "POST")]) {
                            u.method = if m == 1 { "POST" } else { "PUT" }.to_string();
                        }
                        ui.label(RichText::new("Form field").color(p.muted));
                        ui.add(text_input(&mut u.field, "none: the file is the body", 180.0))
                            .on_hover_text("Set it to send a multipart form with the file in this field");
                    });
                    ui.label(RichText::new("Headers (one per line, Name: value)").color(p.muted));
                    ui.add(
                        egui::TextEdit::multiline(&mut u.headers)
                            .hint_text("Authorization: Bearer …")
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                    if let Some(why) = u.problem().filter(|_| !u.url.trim().is_empty()) {
                        ui.label(RichText::new(format!("⚠ {why}")).small().color(p.warn));
                    }
                    if self.settings.upload != before {
                        self.save_settings();
                    }
                });
        });
    }
}
