//! Capturing media: recording a video or audio element of a page (its own stream, or what
//! the browser shows of it on screen), or saving a stream or file found in a page or loaded
//! from a URL. Recordings and downloads end up as "takes" in a capture window, which saves
//! them to files, uploads them, or plays them on another audio output.
//!
//! This module holds the settings and the naming rules; the work is done by
//! `web/capture-engine.js` (capture window) and `web/capture-page.js` (in the page).

use serde::{Deserialize, Serialize};

/// Where the capture settings are kept (storage.local).
pub const SETTINGS_KEY: &str = "smCaptureSettings";
/// Prefix of the storage.session keys where capture windows publish what they are doing.
pub const SUMMARY_PREFIX: &str = "smCapture:";

/// Which tracks to keep.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tracks {
    #[default]
    Both,
    Video,
    Audio,
}

impl Tracks {
    pub const ALL: [(Tracks, &'static str); 3] = [(Tracks::Both, "Video + sound"), (Tracks::Video, "Video only"), (Tracks::Audio, "Sound only")];

    pub fn code(self) -> &'static str {
        match self {
            Tracks::Both => "av",
            Tracks::Video => "v",
            Tracks::Audio => "a",
        }
    }

    pub fn from_code(s: &str) -> Tracks {
        match s {
            "v" => Tracks::Video,
            "a" => Tracks::Audio,
            _ => Tracks::Both,
        }
    }

    pub fn video(self) -> bool {
        self != Tracks::Audio
    }

    pub fn audio(self) -> bool {
        self != Tracks::Video
    }
}

/// How an element is recorded.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Method {
    /// The element's own stream (`captureStream()`): its picture and sound as decoded, at
    /// their size, whatever is drawn over it. Not for protected (DRM) media.
    #[default]
    Element,
    /// What the browser shows: a capture of the tab (picked by the user), cut to the
    /// element's area. Protected media stays black: browsers don't let it be captured.
    Screen,
}

impl Method {
    pub const ALL: [(Method, &'static str); 2] = [(Method::Element, "Element stream"), (Method::Screen, "Rendered on screen")];

    pub fn code(self) -> &'static str {
        match self {
            Method::Element => "element",
            Method::Screen => "screen",
        }
    }

    pub fn from_code(s: &str) -> Method {
        if s == "screen" { Method::Screen } else { Method::Element }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    WebM,
    /// Where the browser can record it (Chromium).
    Mp4,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::WebM => "WebM",
            Format::Mp4 => "MP4",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Format::WebM => "webm",
            Format::Mp4 => "mp4",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// The browser decides.
    #[default]
    Auto,
    High,
    Medium,
    Low,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Quality::Auto, Quality::High, Quality::Medium, Quality::Low];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Auto => "Automatic",
            Quality::High => "High (12 Mbps)",
            Quality::Medium => "Medium (5 Mbps)",
            Quality::Low => "Low (1.5 Mbps)",
        }
    }

    /// Video bits per second (0: the browser's default).
    pub fn video_bps(self) -> u64 {
        match self {
            Quality::Auto => 0,
            Quality::High => 12_000_000,
            Quality::Medium => 5_000_000,
            Quality::Low => 1_500_000,
        }
    }
}

/// Where "Upload" sends a take.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct UploadTarget {
    pub url: String,
    /// "PUT" or "POST".
    pub method: String,
    /// Form field for a multipart upload (empty: the file is the whole request body).
    pub field: String,
    /// Extra request headers, one `Name: value` per line (e.g. an Authorization header).
    pub headers: String,
}

impl Default for UploadTarget {
    fn default() -> Self {
        Self { url: String::new(), method: "PUT".into(), field: String::new(), headers: String::new() }
    }
}

impl UploadTarget {
    /// The headers as (name, value) pairs.
    pub fn header_list(&self) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        for line in self.headers.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (name, value) = line.split_once(':').ok_or_else(|| format!("\"{line}\" is not a header (Name: value)"))?;
            let name = name.trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)) {
                return Err(format!("\"{name}\" is not a header name"));
            }
            out.push((name.to_string(), value.trim().to_string()));
        }
        Ok(out)
    }

    /// Why it can't be used yet, if it can't.
    pub fn problem(&self) -> Option<String> {
        let url = self.url.trim();
        if url.is_empty() {
            return Some("Enter the address to upload to".into());
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Some("The address must start with https:// or http://".into());
        }
        if crate::url::host(url).is_empty() {
            return Some("The address has no host".into());
        }
        if !matches!(self.method.as_str(), "PUT" | "POST") {
            return Some("Upload with PUT or POST".into());
        }
        self.header_list().err()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct CaptureSettings {
    pub tracks: Tracks,
    pub method: Method,
    pub format: Format,
    pub quality: Quality,
    /// Recording an element: start its media from the beginning.
    pub from_start: bool,
    /// Stop recording when the media ends.
    pub stop_at_end: bool,
    /// Silence the element in the page while it's recorded: its sound plays in the capture
    /// window instead (on the output chosen there).
    pub listen_here: bool,
    /// Recording the screen: keep only the element's area (else the whole tab).
    pub crop: bool,
    /// Fetch segments at once when saving a stream.
    pub parallel: u32,
    pub upload: UploadTarget,
    /// Audio output of the capture window ("": the system's default).
    pub output: String,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            tracks: Tracks::Both,
            method: Method::Element,
            format: Format::WebM,
            quality: Quality::Auto,
            from_start: false,
            stop_at_end: true,
            listen_here: false,
            crop: true,
            parallel: 4,
            upload: UploadTarget::default(),
            output: String::new(),
        }
    }
}

/// What a capture window publishes for the inspector's Capture view (storage.session).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CaptureSummary {
    /// The capture window (key suffix).
    pub id: String,
    /// The capture window's own tab (to bring it to the front).
    pub window_tab: Option<i64>,
    /// The tab captured from, if any.
    pub tab: Option<i64>,
    pub title: String,
    /// "Recording", "Paused", "Downloading 12 / 40", "Ready", ...
    pub state: String,
    /// Recording or downloading now (it can be stopped).
    pub busy: bool,
    pub bytes: u64,
    pub ms: f64,
    pub takes: u32,
    pub updated: f64,
}

/// A media element in a frame of a page (see `list` in web/capture-page.js).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PageMedia {
    pub frame_id: i64,
    pub frame_url: String,
    pub top: bool,
    /// Id given by the frame's capture script.
    pub id: u64,
    /// "video" or "audio".
    pub tag: String,
    pub src: String,
    /// "file", "stream" (blob:, Media Source), "live" (srcObject) or "none".
    pub source: String,
    /// Seconds (0: unknown or endless).
    pub duration: f64,
    pub time: f64,
    pub paused: bool,
    pub muted: bool,
    pub width: u32,
    pub height: u32,
    /// Takes room on the page.
    pub shown: bool,
    /// Protected by DRM (Encrypted Media Extensions).
    pub protected: bool,
    pub title: String,
}

impl PageMedia {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.width > 0 && self.height > 0 {
            parts.push(format!("{} × {}", self.width, self.height));
        }
        if self.duration > 0.0 && self.duration.is_finite() {
            parts.push(format_duration(self.duration * 1000.0));
        }
        parts.push(if self.paused { "paused".into() } else { "playing".into() });
        if self.muted {
            parts.push("muted".into());
        }
        parts.push(
            match self.source.as_str() {
                "stream" => "streamed (Media Source)",
                "live" => "live stream",
                "file" => "file",
                _ => "no source",
            }
            .into(),
        );
        if !self.shown {
            parts.push("hidden".into());
        }
        parts.join(" · ")
    }
}

/// A media URL of a page (element sources and requests), from the frame it was seen in.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PageMediaUrl {
    pub frame_id: i64,
    pub url: String,
    /// How it was seen ("video", "source", "request").
    pub how: String,
}

/// 1536 → "1.5 KB".
pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

/// 75_000 ms → "1:15", 3_725_000 → "1:02:05".
pub fn format_duration(ms: f64) -> String {
    let s = if ms.is_finite() && ms > 0.0 { (ms / 1000.0).floor() as u64 } else { 0 };
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// A file name from a title (a page's, or a URL's last segment), what it holds ("video",
/// "1080p"), a time stamp and an extension. Characters file systems refuse are dropped.
pub fn file_name(title: &str, what: &str, stamp: &str, ext: &str) -> String {
    let clean = |s: &str| {
        let mut out = String::new();
        for c in s.chars() {
            let c = if c.is_control() || "\\/:*?\"<>|".contains(c) { ' ' } else { c };
            if !(c == ' ' && (out.is_empty() || out.ends_with(' '))) {
                out.push(c);
            }
        }
        out.trim().trim_end_matches('.').trim().to_string()
    };
    let mut base = clean(title);
    if base.chars().count() > 80 {
        base = base.chars().take(80).collect::<String>().trim().to_string();
    }
    if base.is_empty() {
        base = "capture".into();
    }
    let mut parts = vec![base];
    for extra in [clean(what), clean(stamp)] {
        if !extra.is_empty() {
            parts.push(extra);
        }
    }
    let ext = ext.trim_start_matches('.');
    if ext.is_empty() { parts.join(" - ") } else { format!("{}.{ext}", parts.join(" - ")) }
}

/// A title for a URL: its last path segment without the extension, or its host.
pub fn title_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let after_host = path.split("://").nth(1).and_then(|r| r.split_once('/')).map_or("", |(_, p)| p);
    let last = after_host.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let stem = last.rsplit_once('.').map_or(last, |(s, _)| s);
    let stem = percent_decode(stem);
    if stem.is_empty() || matches!(stem.as_str(), "index" | "master" | "manifest" | "playlist") { crate::url::host(url) } else { stem }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The extension of a recording of `mime` ("video/webm;codecs=vp9" → "webm").
pub fn ext_for_mime(mime: &str, audio_only: bool) -> &'static str {
    let m = mime.to_ascii_lowercase();
    if m.contains("mp4") {
        if audio_only { "m4a" } else { "mp4" }
    } else if m.contains("ogg") {
        "ogg"
    } else if m.contains("matroska") {
        "mkv"
    } else if audio_only {
        "weba"
    } else {
        "webm"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(file_name("My: video / part 2?", "1080p", "2026-10-27 14-03-22", "mp4"), "My video part 2 - 1080p - 2026-10-27 14-03-22.mp4");
        assert_eq!(file_name("  ", "", "", ".webm"), "capture.webm");
        assert_eq!(file_name("Title...", "sound", "", "m4a"), "Title - sound.m4a");
        let long = "x".repeat(200);
        assert_eq!(file_name(&long, "", "", "ts").len(), 83);
    }

    #[test]
    fn titles_from_urls() {
        assert_eq!(title_from_url("https://cdn.example.com/videos/My%20Clip.mp4?x=1"), "My Clip");
        assert_eq!(title_from_url("https://cdn.example.com/hls/master.m3u8"), "cdn.example.com");
        assert_eq!(title_from_url("https://example.com/"), "example.com");
    }

    #[test]
    fn formats() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
        assert_eq!(format_duration(75_000.0), "1:15");
        assert_eq!(format_duration(3_725_000.0), "1:02:05");
        assert_eq!(format_duration(f64::NAN), "0:00");
        assert_eq!(ext_for_mime("video/webm;codecs=vp9,opus", false), "webm");
        assert_eq!(ext_for_mime("audio/webm;codecs=opus", true), "weba");
        assert_eq!(ext_for_mime("video/mp4;codecs=avc1", false), "mp4");
        assert_eq!(ext_for_mime("audio/mp4", true), "m4a");
    }

    #[test]
    fn upload_targets() {
        let mut t = UploadTarget::default();
        assert!(t.problem().is_some());
        t.url = "ftp://x".into();
        assert!(t.problem().unwrap().contains("https://"));
        t.url = "https://upload.example.com/media".into();
        assert_eq!(t.problem(), None);
        t.headers = "Authorization: Bearer abc:def\n\nX-Folder: clips".into();
        assert_eq!(t.header_list().unwrap(), vec![
            ("Authorization".to_string(), "Bearer abc:def".to_string()),
            ("X-Folder".to_string(), "clips".to_string())
        ]);
        t.headers = "no colon".into();
        assert!(t.problem().is_some());
        t.headers = "Bad Name: x".into();
        assert!(t.problem().is_some());
    }

    #[test]
    fn codes_round_trip() {
        for (t, _) in Tracks::ALL {
            assert_eq!(Tracks::from_code(t.code()), t);
        }
        for (m, _) in Method::ALL {
            assert_eq!(Method::from_code(m.code()), m);
        }
        let s: CaptureSettings = serde_json::from_str(r#"{"tracks":"Audio"}"#).unwrap();
        assert_eq!(s.tracks, Tracks::Audio);
        assert_eq!(s.parallel, 4);
    }

    #[test]
    fn media_summary() {
        let m = PageMedia { width: 1280, height: 720, duration: 90.0, paused: false, source: "stream".into(), shown: true, ..Default::default() };
        assert_eq!(m.summary(), "1280 × 720 · 1:30 · playing · streamed (Media Source)");
    }
}
