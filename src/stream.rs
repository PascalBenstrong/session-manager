//! Streaming manifests, for saving a stream found in a page (or loaded from a URL) as files:
//! HLS playlists (`.m3u8`, master and media) and DASH manifests (`.mpd`), turned into the
//! list of parts (initialization segment, then media segments) of each track.
//!
//! Encrypted HLS (`EXT-X-KEY` other than `NONE`) and protected DASH (`ContentProtection`)
//! are reported as such and not planned: their segments can't be saved as they are.

use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ URLs

/// Resolves a (possibly relative) reference against the URL of the document it appears in.
pub fn resolve(base: &str, reference: &str) -> String {
    let r = reference.trim();
    if r.is_empty() {
        return strip_fragment(base).to_string();
    }
    if has_scheme(r) {
        return r.to_string();
    }
    let Some(authority_end) = authority_end(base) else {
        // data:, blob:, about: — nothing to resolve against.
        return r.to_string();
    };
    let scheme_end = base.find(':').unwrap_or(0);
    if let Some(rest) = r.strip_prefix("//") {
        return format!("{}://{}", &base[..scheme_end], rest);
    }
    let origin = &base[..authority_end];
    let base_path_and_query = strip_fragment(&base[authority_end..]);
    if r.starts_with('/') {
        return normalize(origin, r);
    }
    let base_path = base_path_and_query.split('?').next().unwrap_or("");
    if r.starts_with('?') {
        return format!("{origin}{base_path}{r}");
    }
    if r.starts_with('#') {
        return format!("{origin}{base_path_and_query}{r}");
    }
    let dir = match base_path.rfind('/') {
        Some(i) => &base_path[..=i],
        None => "/",
    };
    normalize(origin, &format!("{dir}{r}"))
}

fn has_scheme(s: &str) -> bool {
    match s.find(':') {
        Some(i) if i > 0 => {
            let head = &s[..i];
            head.starts_with(|c: char| c.is_ascii_alphabetic()) && head.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }
        _ => false,
    }
}

/// Where the authority ends (`https://host:port` is `[..end]`), if the URL has one.
fn authority_end(url: &str) -> Option<usize> {
    let colon = url.find(':')?;
    if !has_scheme(url) || !url[colon..].starts_with("://") {
        return None;
    }
    let start = colon + 3;
    Some(url[start..].find(['/', '?', '#']).map_or(url.len(), |i| start + i))
}

fn strip_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// `origin` + `path_and_more` with `.` and `..` segments of the path removed.
fn normalize(origin: &str, path_and_more: &str) -> String {
    let split = path_and_more.find(['?', '#']).unwrap_or(path_and_more.len());
    let (path, rest) = path_and_more.split_at(split);
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    format!("{origin}/{}{rest}", out.join("/"))
}

/// The last path segment's extension, lowercased (`"m3u8"`), or `""`.
pub fn extension(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    match last.rfind('.') {
        Some(i) if i + 1 < last.len() && last.len() - i <= 6 => last[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

// ------------------------------------------------------------------ what a URL is

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// An HLS playlist.
    Hls,
    /// A DASH manifest.
    Dash,
    /// A video or audio file (mp4, webm, mp3...).
    File,
    /// A piece of a stream (HLS / DASH segment): useless on its own.
    Segment,
    /// Can't tell from the URL.
    Unknown,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Hls => "HLS stream",
            SourceKind::Dash => "DASH stream",
            SourceKind::File => "Media file",
            SourceKind::Segment => "Stream segment",
            SourceKind::Unknown => "Media",
        }
    }
}

const FILE_EXTS: [&str; 15] = ["mp4", "m4v", "webm", "mkv", "mov", "ogv", "mp3", "m4a", "oga", "ogg", "opus", "flac", "wav", "aac", "flv"];
const SEGMENT_EXTS: [&str; 4] = ["ts", "m4s", "cmfv", "cmfa"];

/// What a URL most likely is, from its extension (and a few common query markers).
pub fn classify(url: &str) -> SourceKind {
    let ext = extension(url);
    let lower = url.to_ascii_lowercase();
    match ext.as_str() {
        "m3u8" | "m3u" => SourceKind::Hls,
        "mpd" => SourceKind::Dash,
        e if SEGMENT_EXTS.contains(&e) => SourceKind::Segment,
        e if FILE_EXTS.contains(&e) => {
            // Byte ranges of a bigger file, or numbered pieces of a stream.
            if lower.contains("range=") || lower.contains("/seg-") || lower.contains("segment") || lower.contains("/chunk") {
                SourceKind::Segment
            } else {
                SourceKind::File
            }
        }
        _ if lower.contains("format=m3u8") || lower.contains(".m3u8?") => SourceKind::Hls,
        _ if lower.contains("format=mpd") || lower.contains("(format=mpd") => SourceKind::Dash,
        _ => SourceKind::Unknown,
    }
}

/// What a downloaded text is: an HLS playlist or a DASH manifest.
pub fn sniff(text: &str) -> Option<SourceKind> {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("#EXTM3U") {
        Some(SourceKind::Hls)
    } else if t.starts_with('<') && t[..t.len().min(4096)].contains("<MPD") {
        Some(SourceKind::Dash)
    } else {
        None
    }
}

// ------------------------------------------------------------------ tracks to download

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrackKind {
    /// Picture and sound in one (HLS variants without separate audio, TS segments).
    Muxed,
    Video,
    Audio,
    Subtitles,
}

impl TrackKind {
    pub fn label(self) -> &'static str {
        match self {
            TrackKind::Muxed => "Video + sound",
            TrackKind::Video => "Video",
            TrackKind::Audio => "Sound",
            TrackKind::Subtitles => "Subtitles",
        }
    }
}

/// A piece to fetch: a URL, optionally a byte range (first and last byte, inclusive).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Part {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<(u64, u64)>,
}

impl Part {
    fn new(url: String) -> Self {
        Self { url, range: None }
    }
}

/// A track ready to download: its parts in order, joined they make a playable file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Track {
    pub kind: TrackKind,
    pub label: String,
    /// File extension for the joined parts.
    pub ext: String,
    pub parts: Vec<Part>,
    /// Seconds, when known.
    pub duration: f64,
}

/// Formats bits per second as "4.2 Mbps".
pub fn bitrate_label(bps: u64) -> String {
    if bps >= 1_000_000 {
        format!("{:.1} Mbps", bps as f64 / 1e6)
    } else if bps > 0 {
        format!("{} kbps", bps / 1000)
    } else {
        String::new()
    }
}

// ------------------------------------------------------------------ HLS

#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    Master(Master),
    Media(MediaPlaylist),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Master {
    pub variants: Vec<Variant>,
    /// Alternative audio and subtitle renditions (`EXT-X-MEDIA`).
    pub renditions: Vec<Rendition>,
    /// `EXT-X-SESSION-KEY`: the variants are encrypted.
    pub encrypted: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Variant {
    /// Absolute URL of the variant's media playlist.
    pub url: String,
    pub bandwidth: u64,
    pub resolution: Option<(u32, u32)>,
    pub codecs: String,
    /// `AUDIO` group the variant plays its sound from.
    pub audio: Option<String>,
    pub frame_rate: Option<f64>,
}

impl Variant {
    /// Only sound (no picture).
    pub fn audio_only(&self) -> bool {
        self.resolution.is_none()
            && !self.codecs.is_empty()
            && self.codecs.split(',').all(|c| {
                let c = c.trim();
                c.starts_with("mp4a") || c.starts_with("ac-3") || c.starts_with("ec-3") || c.starts_with("opus") || c.starts_with("mp3") || c.starts_with("flac")
            })
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if let Some((w, h)) = self.resolution {
            parts.push(format!("{h}p ({w}×{h})"));
        } else if self.audio_only() {
            parts.push("Sound only".to_string());
        }
        if let Some(f) = self.frame_rate.filter(|f| *f > 0.0) {
            parts.push(format!("{} fps", (f * 100.0).round() / 100.0));
        }
        let b = bitrate_label(self.bandwidth);
        if !b.is_empty() {
            parts.push(b);
        }
        if parts.is_empty() { "Default".to_string() } else { parts.join(" · ") }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rendition {
    /// "AUDIO", "SUBTITLES", "VIDEO" or "CLOSED-CAPTIONS".
    pub kind: String,
    pub group: String,
    pub name: String,
    pub language: String,
    /// Absolute URL of its media playlist (none: it is in the variant's own segments).
    pub url: Option<String>,
    pub default: bool,
}

impl Rendition {
    pub fn label(&self) -> String {
        match (self.name.is_empty(), self.language.is_empty()) {
            (false, false) if !self.name.eq_ignore_ascii_case(&self.language) => format!("{} ({})", self.name, self.language),
            (false, _) => self.name.clone(),
            (true, false) => self.language.clone(),
            (true, true) => self.group.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaPlaylist {
    /// `EXT-X-MAP`: the initialization segment (fragmented MP4).
    pub init: Option<Part>,
    pub segments: Vec<Part>,
    /// Seconds (sum of the segments' durations).
    pub duration: f64,
    /// Segments are encrypted (`EXT-X-KEY` with a method other than NONE).
    pub encrypted: bool,
    /// `EXT-X-ENDLIST`: the whole stream is listed (else it's live: only what's there now).
    pub ended: bool,
}

impl MediaPlaylist {
    /// File extension of the joined segments.
    pub fn ext(&self, kind: TrackKind) -> String {
        let seg = self.segments.first().map(|s| extension(&s.url)).unwrap_or_default();
        if self.init.is_some() || matches!(seg.as_str(), "m4s" | "mp4" | "m4a" | "m4v" | "cmfv" | "cmfa") {
            return if kind == TrackKind::Audio { "m4a" } else { "mp4" }.to_string();
        }
        match seg.as_str() {
            "aac" | "mp3" | "ac3" | "ec3" | "vtt" | "webvtt" => seg,
            _ => "ts".to_string(),
        }
    }

    /// The parts to fetch, initialization segment first.
    pub fn parts(&self) -> Vec<Part> {
        self.init.iter().cloned().chain(self.segments.iter().cloned()).collect()
    }

    pub fn track(&self, kind: TrackKind, label: impl Into<String>) -> Track {
        Track { kind, label: label.into(), ext: self.ext(kind), parts: self.parts(), duration: self.duration }
    }
}

/// Parses the attribute list of an HLS tag (`KEY=value,KEY="quoted, value"`).
pub fn hls_attributes(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().trim_start_matches(',').trim().to_ascii_uppercase();
        rest = &rest[eq + 1..];
        let value;
        if let Some(q) = rest.strip_prefix('"') {
            let end = q.find('"').unwrap_or(q.len());
            value = q[..end].to_string();
            rest = q.get(end + 1..).unwrap_or("");
            rest = rest.trim_start_matches(',');
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = rest.get(end + 1..).unwrap_or("");
        }
        out.push((key, value));
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `n[@o]` of `EXT-X-BYTERANGE`: length and optional offset.
fn byte_range(s: &str) -> Option<(u64, Option<u64>)> {
    let (n, o) = match s.split_once('@') {
        Some((n, o)) => (n, Some(o.trim().parse().ok()?)),
        None => (s, None),
    };
    Some((n.trim().parse().ok()?, o))
}

/// Parses an HLS playlist found at `base` (its URL, for relative references).
pub fn parse_hls(text: &str, base: &str) -> Result<Playlist, String> {
    if sniff(text) != Some(SourceKind::Hls) {
        return Err("not an HLS playlist (no #EXTM3U)".into());
    }
    let mut master = Master::default();
    let mut media = MediaPlaylist::default();
    let mut is_master = false;
    let mut pending_variant: Option<Variant> = None;
    let mut seg_duration = 0.0;
    let mut seg_range: Option<(u64, Option<u64>)> = None;
    // Where the next byte range of a URL starts when the offset is left out.
    let mut next_offset: Option<(String, u64)> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(tag) = line.strip_prefix('#') {
            let (name, value) = tag.split_once(':').unwrap_or((tag, ""));
            match name {
                "EXT-X-STREAM-INF" => {
                    is_master = true;
                    let a = hls_attributes(value);
                    pending_variant = Some(Variant {
                        url: String::new(),
                        bandwidth: attr(&a, "BANDWIDTH").and_then(|v| v.parse().ok()).unwrap_or(0),
                        resolution: attr(&a, "RESOLUTION").and_then(|r| {
                            let (w, h) = r.to_ascii_lowercase().split_once('x').map(|(w, h)| (w.to_string(), h.to_string()))?;
                            Some((w.parse().ok()?, h.parse().ok()?))
                        }),
                        codecs: attr(&a, "CODECS").unwrap_or("").to_string(),
                        audio: attr(&a, "AUDIO").map(String::from),
                        frame_rate: attr(&a, "FRAME-RATE").and_then(|v| v.parse().ok()),
                    });
                }
                "EXT-X-MEDIA" => {
                    is_master = true;
                    let a = hls_attributes(value);
                    master.renditions.push(Rendition {
                        kind: attr(&a, "TYPE").unwrap_or("").to_ascii_uppercase(),
                        group: attr(&a, "GROUP-ID").unwrap_or("").to_string(),
                        name: attr(&a, "NAME").unwrap_or("").to_string(),
                        language: attr(&a, "LANGUAGE").unwrap_or("").to_string(),
                        url: attr(&a, "URI").map(|u| resolve(base, u)),
                        default: attr(&a, "DEFAULT").is_some_and(|v| v.eq_ignore_ascii_case("YES")),
                    });
                }
                "EXT-X-SESSION-KEY" => {
                    let a = hls_attributes(value);
                    if attr(&a, "METHOD").is_some_and(|m| !m.eq_ignore_ascii_case("NONE")) {
                        master.encrypted = true;
                    }
                }
                "EXT-X-KEY" => {
                    let a = hls_attributes(value);
                    if attr(&a, "METHOD").is_some_and(|m| !m.eq_ignore_ascii_case("NONE")) {
                        media.encrypted = true;
                    }
                }
                "EXT-X-MAP" => {
                    let a = hls_attributes(value);
                    if let Some(uri) = attr(&a, "URI") {
                        let url = resolve(base, uri);
                        let range = attr(&a, "BYTERANGE").and_then(byte_range).map(|(n, o)| {
                            let start = o.unwrap_or(0);
                            (start, start + n.saturating_sub(1))
                        });
                        media.init = Some(Part { url, range });
                    }
                }
                "EXTINF" => {
                    seg_duration = value.split(',').next().and_then(|d| d.trim().parse().ok()).unwrap_or(0.0);
                }
                "EXT-X-BYTERANGE" => seg_range = byte_range(value),
                "EXT-X-ENDLIST" => media.ended = true,
                _ => {}
            }
            continue;
        }
        // A URI line.
        let url = resolve(base, line);
        if let Some(mut v) = pending_variant.take() {
            v.url = url;
            master.variants.push(v);
        } else if !is_master {
            let range = seg_range.take().map(|(n, o)| {
                let start = o.or_else(|| next_offset.as_ref().filter(|(u, _)| *u == url).map(|(_, at)| *at)).unwrap_or(0);
                next_offset = Some((url.clone(), start + n));
                (start, start + n.saturating_sub(1))
            });
            media.duration += seg_duration;
            seg_duration = 0.0;
            media.segments.push(Part { url, range });
        }
    }
    if is_master {
        if master.variants.is_empty() && master.renditions.iter().all(|r| r.url.is_none()) {
            return Err("the playlist lists no streams".into());
        }
        Ok(Playlist::Master(master))
    } else {
        if media.segments.is_empty() {
            return Err("the playlist lists no segments".into());
        }
        Ok(Playlist::Media(media))
    }
}

impl Master {
    /// Variants by quality, best first (sound-only ones last).
    pub fn sorted_variants(&self) -> Vec<&Variant> {
        let mut v: Vec<&Variant> = self.variants.iter().collect();
        v.sort_by_key(|x| (x.audio_only(), std::cmp::Reverse(x.resolution.map_or(0, |(w, h)| w as u64 * h as u64)), std::cmp::Reverse(x.bandwidth)));
        v
    }

    /// Audio renditions with their own playlist, of `group` (or of any group if `None`).
    pub fn audio_renditions(&self, group: Option<&str>) -> Vec<&Rendition> {
        self.renditions
            .iter()
            .filter(|r| r.kind == "AUDIO" && r.url.is_some() && group.is_none_or(|g| r.group == g))
            .collect()
    }

    pub fn subtitle_renditions(&self) -> Vec<&Rendition> {
        self.renditions.iter().filter(|r| r.kind == "SUBTITLES" && r.url.is_some()).collect()
    }
}

// ------------------------------------------------------------------ DASH

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mpd {
    /// One entry per representation (quality / language) of the first period.
    pub representations: Vec<Representation>,
    /// `type="dynamic"`: a live stream.
    pub live: bool,
    /// Seconds.
    pub duration: f64,
    /// More than one period (only the first is listed).
    pub more_periods: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Representation {
    pub kind: TrackKind,
    pub id: String,
    pub bandwidth: u64,
    pub resolution: Option<(u32, u32)>,
    pub codecs: String,
    pub mime: String,
    pub language: String,
    /// `ContentProtection` (DRM): can't be saved.
    pub protected: bool,
    pub init: Option<Part>,
    pub segments: Vec<Part>,
}

impl Representation {
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if let Some((w, h)) = self.resolution {
            parts.push(format!("{h}p ({w}×{h})"));
        }
        if !self.language.is_empty() {
            parts.push(self.language.clone());
        }
        let b = bitrate_label(self.bandwidth);
        if !b.is_empty() {
            parts.push(b);
        }
        if let Some(c) = self.codecs.split('.').next().filter(|c| !c.is_empty()) {
            parts.push(c.to_string());
        }
        if parts.is_empty() { self.id.clone() } else { parts.join(" · ") }
    }

    pub fn ext(&self) -> String {
        let webm = self.mime.contains("webm");
        match self.kind {
            TrackKind::Subtitles if self.mime.contains("vtt") => "vtt".into(),
            TrackKind::Subtitles if self.mime.contains("ttml") => "ttml".into(),
            TrackKind::Audio if webm => "weba".into(),
            TrackKind::Audio => "m4a".into(),
            _ if webm => "webm".into(),
            _ => "mp4".into(),
        }
    }

    pub fn track(&self, duration: f64) -> Track {
        Track {
            kind: self.kind,
            label: self.label(),
            ext: self.ext(),
            parts: self.init.iter().cloned().chain(self.segments.iter().cloned()).collect(),
            duration,
        }
    }
}

impl Mpd {
    /// Representations of a kind, best first.
    pub fn of_kind(&self, kind: TrackKind) -> Vec<&Representation> {
        let mut v: Vec<&Representation> = self.representations.iter().filter(|r| r.kind == kind).collect();
        v.sort_by_key(|r| (std::cmp::Reverse(r.resolution.map_or(0, |(w, h)| w as u64 * h as u64)), std::cmp::Reverse(r.bandwidth)));
        v
    }
}

/// An ISO 8601 duration (`PT1H2M3.5S`, `P1DT2H`) in seconds.
pub fn iso_duration(s: &str) -> Option<f64> {
    let s = s.trim().strip_prefix('P')?;
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut total = 0.0;
    let mut num = String::new();
    for c in date.chars() {
        match c {
            'Y' => total += num.parse::<f64>().ok()? * 365.0 * 86400.0,
            'M' => total += num.parse::<f64>().ok()? * 30.0 * 86400.0,
            'W' => total += num.parse::<f64>().ok()? * 7.0 * 86400.0,
            'D' => total += num.parse::<f64>().ok()? * 86400.0,
            _ => {
                num.push(c);
                continue;
            }
        }
        num.clear();
    }
    for c in time.chars() {
        match c {
            'H' => total += num.parse::<f64>().ok()? * 3600.0,
            'M' => total += num.parse::<f64>().ok()? * 60.0,
            'S' => total += num.parse::<f64>().ok()?,
            _ => {
                num.push(c);
                continue;
            }
        }
        num.clear();
    }
    num.is_empty().then_some(total)
}

/// Fills a `SegmentTemplate` pattern: `$RepresentationID$`, `$Bandwidth$`, `$Number$`,
/// `$Time$` (each optionally with a width, `$Number%05d$`) and `$$`.
pub fn fill_template(pattern: &str, id: &str, bandwidth: u64, number: u64, time: u64) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('$') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = &after[..end];
        let (name, format) = token.split_once('%').unwrap_or((token, ""));
        let value = match name {
            "" => Some("$".to_string()),
            "RepresentationID" => Some(id.to_string()),
            "Bandwidth" => Some(bandwidth.to_string()),
            "Number" => Some(number.to_string()),
            "Time" => Some(time.to_string()),
            _ => None,
        };
        match value {
            Some(v) => {
                let width: usize = format.trim_start_matches('0').trim_end_matches('d').parse().unwrap_or(0);
                out.push_str(&format!("{v:0>width$}"));
            }
            None => {
                out.push('$');
                out.push_str(token);
                out.push('$');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// `SegmentTemplate` attributes, merged from the adaptation set and the representation.
#[derive(Default, Clone)]
struct Template {
    media: Option<String>,
    init: Option<String>,
    start_number: Option<u64>,
    timescale: Option<u64>,
    duration: Option<u64>,
    /// `SegmentTimeline`: (t, d, r).
    timeline: Option<Vec<(Option<u64>, u64, i64)>>,
}

impl Template {
    fn read(node: roxmltree::Node) -> Option<Template> {
        let t = child(node, "SegmentTemplate")?;
        let timeline = child(t, "SegmentTimeline").map(|tl| {
            tl.children()
                .filter(|s| s.has_tag_name_local("S"))
                .map(|s| {
                    (
                        s.attribute("t").and_then(|v| v.parse().ok()),
                        s.attribute("d").and_then(|v| v.parse().ok()).unwrap_or(0),
                        s.attribute("r").and_then(|v| v.parse().ok()).unwrap_or(0),
                    )
                })
                .collect()
        });
        Some(Template {
            media: t.attribute("media").map(String::from),
            init: t.attribute("initialization").map(String::from),
            start_number: t.attribute("startNumber").and_then(|v| v.parse().ok()),
            timescale: t.attribute("timescale").and_then(|v| v.parse().ok()),
            duration: t.attribute("duration").and_then(|v| v.parse().ok()),
            timeline,
        })
    }

    /// `self` with the gaps filled from `outer`.
    fn over(self, outer: &Template) -> Template {
        Template {
            media: self.media.or_else(|| outer.media.clone()),
            init: self.init.or_else(|| outer.init.clone()),
            start_number: self.start_number.or(outer.start_number),
            timescale: self.timescale.or(outer.timescale),
            duration: self.duration.or(outer.duration),
            timeline: self.timeline.or_else(|| outer.timeline.clone()),
        }
    }
}

trait LocalName {
    fn has_tag_name_local(&self, name: &str) -> bool;
}

impl LocalName for roxmltree::Node<'_, '_> {
    fn has_tag_name_local(&self, name: &str) -> bool {
        self.is_element() && self.tag_name().name() == name
    }
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.has_tag_name_local(name))
}

/// The base URL below `node` (its `BaseURL` resolved against `base`).
fn base_below(node: roxmltree::Node, base: &str) -> String {
    match child(node, "BaseURL").and_then(|b| b.text()).map(str::trim).filter(|t| !t.is_empty()) {
        Some(b) => resolve(base, b),
        None => base.to_string(),
    }
}

fn range_attr(s: Option<&str>) -> Option<(u64, u64)> {
    let (a, b) = s?.split_once('-')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn kind_of(content_type: &str, mime: &str, codecs: &str) -> TrackKind {
    let t = if content_type.is_empty() { mime.split('/').next().unwrap_or("") } else { content_type };
    match t {
        "audio" => TrackKind::Audio,
        "text" => TrackKind::Subtitles,
        "application" if mime.contains("ttml") || mime.contains("mp4") && codecs.starts_with("stpp") || codecs.starts_with("wvtt") => TrackKind::Subtitles,
        _ => TrackKind::Video,
    }
}

/// Upper limit of segments per representation (a mistake in a manifest shouldn't make
/// us list millions).
const MAX_SEGMENTS: usize = 100_000;

/// Parses a DASH manifest found at `base` (its URL).
pub fn parse_mpd(text: &str, base: &str) -> Result<Mpd, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| format!("not a DASH manifest: {e}"))?;
    let root = doc.root_element();
    if !root.has_tag_name_local("MPD") {
        return Err("not a DASH manifest (no <MPD>)".into());
    }
    let live = root.attribute("type") == Some("dynamic");
    let total = root.attribute("mediaPresentationDuration").and_then(iso_duration).unwrap_or(0.0);
    let periods: Vec<_> = root.children().filter(|c| c.has_tag_name_local("Period")).collect();
    let period = *periods.first().ok_or("the manifest has no period")?;
    let duration = period.attribute("duration").and_then(iso_duration).unwrap_or(total);
    let mpd_base = base_below(root, base);
    let period_base = base_below(period, &mpd_base);
    let period_template = Template::read(period).unwrap_or_default();
    let mut out = Mpd { representations: Vec::new(), live, duration, more_periods: periods.len() > 1 };
    for set in period.children().filter(|c| c.has_tag_name_local("AdaptationSet")) {
        let set_base = base_below(set, &period_base);
        let set_template = Template::read(set).map(|t| t.over(&period_template)).unwrap_or_else(|| period_template.clone());
        let set_protected = child(set, "ContentProtection").is_some();
        let set_mime = set.attribute("mimeType").unwrap_or("");
        let set_codecs = set.attribute("codecs").unwrap_or("");
        let content_type = set.attribute("contentType").unwrap_or("");
        let lang = set.attribute("lang").unwrap_or("");
        let set_list = child(set, "SegmentList");
        for rep in set.children().filter(|c| c.has_tag_name_local("Representation")) {
            let id = rep.attribute("id").unwrap_or("").to_string();
            let bandwidth = rep.attribute("bandwidth").and_then(|v| v.parse().ok()).unwrap_or(0);
            let mime = rep.attribute("mimeType").unwrap_or(set_mime).to_string();
            let codecs = rep.attribute("codecs").unwrap_or(set_codecs).to_string();
            let width = rep.attribute("width").or(set.attribute("width")).and_then(|v| v.parse().ok());
            let height = rep.attribute("height").or(set.attribute("height")).and_then(|v| v.parse().ok());
            let rep_base = base_below(rep, &set_base);
            let template = Template::read(rep).map(|t| t.over(&set_template)).unwrap_or_else(|| set_template.clone());
            let mut r = Representation {
                kind: kind_of(content_type, &mime, &codecs),
                id: id.clone(),
                bandwidth,
                resolution: width.zip(height),
                codecs,
                mime,
                language: rep.attribute("lang").unwrap_or(lang).to_string(),
                protected: set_protected || child(rep, "ContentProtection").is_some(),
                init: None,
                segments: Vec::new(),
            };
            if let Some(list) = child(rep, "SegmentList").or(set_list) {
                if let Some(init) = child(list, "Initialization") {
                    r.init = Some(Part {
                        url: init.attribute("sourceURL").map_or_else(|| rep_base.clone(), |u| resolve(&rep_base, u)),
                        range: range_attr(init.attribute("range")),
                    });
                }
                for s in list.children().filter(|c| c.has_tag_name_local("SegmentURL")).take(MAX_SEGMENTS) {
                    r.segments.push(Part {
                        url: s.attribute("media").map_or_else(|| rep_base.clone(), |u| resolve(&rep_base, u)),
                        range: range_attr(s.attribute("mediaRange")),
                    });
                }
            } else if let Some(media) = template.media.as_deref() {
                let timescale = template.timescale.unwrap_or(1).max(1);
                let start = template.start_number.unwrap_or(1);
                if let Some(init) = template.init.as_deref() {
                    r.init = Some(Part::new(resolve(&rep_base, &fill_template(init, &id, bandwidth, start, 0))));
                }
                if let Some(timeline) = &template.timeline {
                    let end = (duration * timescale as f64).round() as u64;
                    let mut t = 0u64;
                    let mut number = start;
                    'outer: for (i, (st, d, rep_count)) in timeline.iter().enumerate() {
                        if let Some(st) = st {
                            t = *st;
                        }
                        if *d == 0 {
                            continue;
                        }
                        // r = -1: repeat until the next S (or the end of the period).
                        let count = if *rep_count >= 0 {
                            *rep_count as u64 + 1
                        } else {
                            let until = timeline.get(i + 1).and_then(|n| n.0).unwrap_or(end);
                            until.saturating_sub(t).div_ceil(*d)
                        };
                        for _ in 0..count {
                            if r.segments.len() >= MAX_SEGMENTS {
                                break 'outer;
                            }
                            r.segments.push(Part::new(resolve(&rep_base, &fill_template(media, &id, bandwidth, number, t))));
                            t += d;
                            number += 1;
                        }
                    }
                } else if let Some(d) = template.duration.filter(|d| *d > 0) {
                    let count = ((duration * timescale as f64) / d as f64).ceil() as usize;
                    if count == 0 {
                        return Err("the manifest doesn't say how long the stream is".into());
                    }
                    for k in 0..count.min(MAX_SEGMENTS) {
                        let number = start + k as u64;
                        r.segments.push(Part::new(resolve(&rep_base, &fill_template(media, &id, bandwidth, number, k as u64 * d))));
                    }
                }
            } else {
                // SegmentBase, or nothing: the representation is one file.
                r.segments.push(Part::new(rep_base.clone()));
            }
            out.representations.push(r);
        }
    }
    if out.representations.is_empty() {
        return Err("the manifest lists no streams".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_references() {
        let base = "https://cdn.example.com/a/b/master.m3u8?token=1#x";
        assert_eq!(resolve(base, "v1/index.m3u8"), "https://cdn.example.com/a/b/v1/index.m3u8");
        assert_eq!(resolve(base, "../c/seg.ts"), "https://cdn.example.com/a/c/seg.ts");
        assert_eq!(resolve(base, "./x/../y.ts?q=a:b"), "https://cdn.example.com/a/b/y.ts?q=a:b");
        assert_eq!(resolve(base, "/root.ts"), "https://cdn.example.com/root.ts");
        assert_eq!(resolve(base, "//other.net/p.ts"), "https://other.net/p.ts");
        assert_eq!(resolve(base, "http://abs.org/z"), "http://abs.org/z");
        assert_eq!(resolve(base, "?token=2"), "https://cdn.example.com/a/b/master.m3u8?token=2");
        assert_eq!(resolve("https://h.com", "seg.ts"), "https://h.com/seg.ts");
        assert_eq!(resolve("https://h.com/a/", ".."), "https://h.com/");
        assert_eq!(resolve("blob:https://h.com/1234", "x.ts"), "x.ts");
    }

    #[test]
    fn classifies_urls() {
        assert_eq!(classify("https://x.com/live/master.m3u8?t=1"), SourceKind::Hls);
        assert_eq!(classify("https://x.com/v/manifest.mpd"), SourceKind::Dash);
        assert_eq!(classify("https://x.com/v/clip.MP4"), SourceKind::File);
        assert_eq!(classify("https://x.com/v/seg-12-v1.ts"), SourceKind::Segment);
        assert_eq!(classify("https://x.com/v/chunk_5.m4s"), SourceKind::Segment);
        assert_eq!(classify("https://x.com/api/stream?format=m3u8"), SourceKind::Hls);
        assert_eq!(classify("https://x.com/watch"), SourceKind::Unknown);
        assert_eq!(sniff("\u{feff}#EXTM3U\n"), Some(SourceKind::Hls));
        assert_eq!(sniff("<?xml version=\"1.0\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">"), Some(SourceKind::Dash));
        assert_eq!(sniff("<html>"), None);
    }

    #[test]
    fn parses_hls_master() {
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"audio/en.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Français\",LANGUAGE=\"fr\",URI=\"audio/fr.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",URI=\"subs/en.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"aud\"\n\
            low/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,FRAME-RATE=29.97,CODECS=\"avc1.640028,mp4a.40.2\",AUDIO=\"aud\"\n\
            hi/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.5\"\n\
            audio-only.m3u8\n";
        let Playlist::Master(m) = parse_hls(text, "https://h.com/vod/master.m3u8").unwrap() else { panic!("master expected") };
        assert_eq!(m.variants.len(), 3);
        let sorted = m.sorted_variants();
        assert_eq!(sorted[0].url, "https://h.com/vod/hi/index.m3u8");
        assert_eq!(sorted[0].label(), "1080p (1920×1080) · 29.97 fps · 5.0 Mbps");
        assert!(sorted[2].audio_only());
        assert_eq!(sorted[2].label(), "Sound only · 64 kbps");
        let audio = m.audio_renditions(Some("aud"));
        assert_eq!(audio.len(), 2);
        assert!(audio[0].default);
        assert_eq!(audio[1].label(), "Français (fr)");
        assert_eq!(audio[0].url.as_deref(), Some("https://h.com/vod/audio/en.m3u8"));
        assert_eq!(m.subtitle_renditions().len(), 1);
        assert!(!m.encrypted);
    }

    #[test]
    fn parses_hls_media() {
        let text = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n\
            #EXT-X-MAP:URI=\"init.mp4\"\n\
            #EXTINF:6.0,\nseg1.m4s\n#EXTINF:6.0,\nseg2.m4s\n#EXTINF:3.5,\nhttps://other.cdn/seg3.m4s\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse_hls(text, "https://h.com/v/1080/index.m3u8").unwrap() else { panic!("media expected") };
        assert_eq!(m.init, Some(Part { url: "https://h.com/v/1080/init.mp4".into(), range: None }));
        assert_eq!(m.segments.len(), 3);
        assert_eq!(m.segments[2].url, "https://other.cdn/seg3.m4s");
        assert!((m.duration - 15.5).abs() < 1e-9);
        assert!(m.ended && !m.encrypted);
        assert_eq!(m.ext(TrackKind::Video), "mp4");
        assert_eq!(m.ext(TrackKind::Audio), "m4a");
        let t = m.track(TrackKind::Video, "1080p");
        assert_eq!(t.parts.len(), 4);
        assert_eq!(t.parts[0].url, "https://h.com/v/1080/init.mp4");
    }

    #[test]
    fn hls_ts_live_encrypted_and_byte_ranges() {
        let ts = "#EXTM3U\n#EXTINF:10,\na.ts\n#EXTINF:10,\nb.ts\n";
        let Playlist::Media(m) = parse_hls(ts, "http://h/p/x.m3u8").unwrap() else { panic!() };
        assert!(!m.ended, "no ENDLIST: live");
        assert_eq!(m.ext(TrackKind::Muxed), "ts");

        let enc = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:10,\na.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse_hls(enc, "http://h/p/x.m3u8").unwrap() else { panic!() };
        assert!(m.encrypted);
        let none = "#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:10,\na.ts\n";
        let Playlist::Media(m) = parse_hls(none, "http://h/p/x.m3u8").unwrap() else { panic!() };
        assert!(!m.encrypted);

        let ranges = "#EXTM3U\n#EXT-X-MAP:URI=\"main.mp4\",BYTERANGE=\"720@0\"\n\
            #EXTINF:4,\n#EXT-X-BYTERANGE:1000@720\nmain.mp4\n\
            #EXTINF:4,\n#EXT-X-BYTERANGE:500\nmain.mp4\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse_hls(ranges, "http://h/p/x.m3u8").unwrap() else { panic!() };
        assert_eq!(m.init.unwrap().range, Some((0, 719)));
        assert_eq!(m.segments[0].range, Some((720, 1719)));
        assert_eq!(m.segments[1].range, Some((1720, 2219)));

        assert!(parse_hls("<html>", "http://h/").is_err());
        assert!(parse_hls("#EXTM3U\n#EXT-X-VERSION:3\n", "http://h/").is_err());
    }

    #[test]
    fn attribute_lists() {
        let a = hls_attributes("BANDWIDTH=1,CODECS=\"avc1.4d,mp4a.40.2\",RESOLUTION=1x2,NAME=\"a=b\"");
        assert_eq!(attr(&a, "CODECS"), Some("avc1.4d,mp4a.40.2"));
        assert_eq!(attr(&a, "RESOLUTION"), Some("1x2"));
        assert_eq!(attr(&a, "NAME"), Some("a=b"));
    }

    #[test]
    fn durations_and_templates() {
        assert_eq!(iso_duration("PT1H2M3.5S"), Some(3723.5));
        assert_eq!(iso_duration("PT0S"), Some(0.0));
        assert_eq!(iso_duration("P1DT1S"), Some(86401.0));
        assert_eq!(iso_duration("1H"), None);
        assert_eq!(fill_template("v/$RepresentationID$/seg-$Number%05d$.m4s", "720p", 1, 42, 0), "v/720p/seg-00042.m4s");
        assert_eq!(fill_template("$Bandwidth$_$Time$.m4s?x=$$", "a", 128000, 1, 9000), "128000_9000.m4s?x=$");
        assert_eq!(fill_template("$Unknown$.mp4", "a", 1, 1, 1), "$Unknown$.mp4");
    }

    const MPD_TEMPLATE: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT10S">
  <BaseURL>media/</BaseURL>
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/$Number$.m4s" startNumber="1" timescale="1000" duration="4000"/>
      <Representation id="v1080" bandwidth="5000000" width="1920" height="1080" codecs="avc1.640028"/>
      <Representation id="v360" bandwidth="600000" width="640" height="360" codecs="avc1.4d401e"/>
    </AdaptationSet>
    <AdaptationSet contentType="audio" mimeType="audio/mp4" lang="en">
      <Representation id="a" bandwidth="128000" codecs="mp4a.40.2">
        <SegmentTemplate initialization="a/init.mp4" media="a/$Time$.m4s" timescale="48000">
          <SegmentTimeline><S t="0" d="192000" r="1"/><S d="96000"/></SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
    <AdaptationSet mimeType="text/vtt" lang="de">
      <Representation id="sub" bandwidth="100"><BaseURL>subs/de.vtt</BaseURL></Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn parses_dash_templates() {
        let mpd = parse_mpd(MPD_TEMPLATE, "https://h.com/x/manifest.mpd").unwrap();
        assert!(!mpd.live && !mpd.more_periods);
        assert_eq!(mpd.duration, 10.0);
        let video = mpd.of_kind(TrackKind::Video);
        assert_eq!(video[0].id, "v1080");
        assert_eq!(video[0].init.as_ref().unwrap().url, "https://h.com/x/media/v1080/init.mp4");
        // 10 s in 4 s segments: 3
        assert_eq!(video[0].segments.iter().map(|s| s.url.as_str()).collect::<Vec<_>>(), [
            "https://h.com/x/media/v1080/1.m4s",
            "https://h.com/x/media/v1080/2.m4s",
            "https://h.com/x/media/v1080/3.m4s"
        ]);
        assert_eq!(video[0].label(), "1080p (1920×1080) · 5.0 Mbps · avc1");
        let audio = mpd.of_kind(TrackKind::Audio);
        assert_eq!(audio[0].segments.iter().map(|s| s.url.rsplit('/').next().unwrap()).collect::<Vec<_>>(), ["0.m4s", "192000.m4s", "384000.m4s"]);
        assert_eq!(audio[0].language, "en");
        assert_eq!(audio[0].ext(), "m4a");
        let subs = mpd.of_kind(TrackKind::Subtitles);
        assert_eq!(subs[0].segments[0].url, "https://h.com/x/media/subs/de.vtt");
        assert_eq!(subs[0].ext(), "vtt");
        let t = video[0].track(mpd.duration);
        assert_eq!((t.ext.as_str(), t.parts.len()), ("mp4", 4));
    }

    #[test]
    fn parses_dash_lists_base_and_protection() {
        let text = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT8S" type="static">
  <Period duration="PT8S">
    <AdaptationSet mimeType="video/webm">
      <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
      <Representation id="1" bandwidth="1000" width="10" height="10">
        <SegmentList><Initialization sourceURL="init.webm" range="0-99"/>
          <SegmentURL media="s1.webm"/><SegmentURL media="s2.webm" mediaRange="100-199"/></SegmentList>
      </Representation>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/webm">
      <Representation id="2" bandwidth="64000"><BaseURL>https://cdn.net/audio.webm</BaseURL><SegmentBase indexRange="0-10"/></Representation>
    </AdaptationSet>
  </Period>
  <Period duration="PT1S"/>
</MPD>"#;
        let mpd = parse_mpd(text, "https://h.com/m.mpd").unwrap();
        assert!(mpd.more_periods);
        let v = &mpd.representations[0];
        assert!(v.protected);
        assert_eq!(v.init, Some(Part { url: "https://h.com/init.webm".into(), range: Some((0, 99)) }));
        assert_eq!(v.segments[1], Part { url: "https://h.com/s2.webm".into(), range: Some((100, 199)) });
        assert_eq!(v.ext(), "webm");
        let a = &mpd.representations[1];
        assert!(!a.protected);
        assert_eq!(a.segments, vec![Part::new("https://cdn.net/audio.webm".into())]);
        assert_eq!(a.ext(), "weba");
        assert!(parse_mpd("<html/>", "https://h.com/").is_err());
        assert!(parse_mpd("not xml", "https://h.com/").is_err());
    }

    #[test]
    fn dash_timeline_repeat_to_end() {
        let text = r#"<MPD mediaPresentationDuration="PT10S"><Period>
  <AdaptationSet contentType="video"><SegmentTemplate media="$Number$.m4s" timescale="1" startNumber="5">
    <SegmentTimeline><S t="0" d="4" r="-1"/></SegmentTimeline></SegmentTemplate>
    <Representation id="v" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let mpd = parse_mpd(text, "https://h.com/a/m.mpd").unwrap();
        let names: Vec<_> = mpd.representations[0].segments.iter().map(|s| s.url.rsplit('/').next().unwrap()).collect();
        assert_eq!(names, ["5.m4s", "6.m4s", "7.m4s"]);
    }
}
