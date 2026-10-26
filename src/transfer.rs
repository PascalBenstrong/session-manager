//! Saving sessions to disk and importing them back, plus parsing URL lists
//! (comma / newline separated) from text files or the clipboard.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::model::{Session, TabEntry};
use crate::url;

pub const FORMAT: &str = "session-manager";
pub const VERSION: u32 = 1;

/// File written by "Save to disk".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExportFile {
    pub format: String,
    pub version: u32,
    pub exported_at: f64,
    pub sessions: Vec<Session>,
}

/// Serializes sessions for download. Live tab links are dropped.
pub fn export_sessions<'a>(sessions: impl IntoIterator<Item = &'a Session>, include_snapshots: bool, now: f64) -> String {
    let sessions = sessions
        .into_iter()
        .map(|s| {
            let mut s = s.clone();
            s.entries.iter_mut().for_each(unlink);
            if include_snapshots {
                s.snapshots.iter_mut().flat_map(|snap| snap.entries.iter_mut()).for_each(unlink);
            } else {
                s.snapshots.clear();
            }
            s
        })
        .collect();
    let file = ExportFile { format: FORMAT.into(), version: VERSION, exported_at: now, sessions };
    serde_json::to_string_pretty(&file).unwrap_or_default()
}

/// Comma separated URL list (one per line for readability).
pub fn export_urls(entries: &[TabEntry]) -> String {
    entries.iter().map(|e| e.url.as_str()).collect::<Vec<_>>().join(",\n")
}

fn unlink(e: &mut TabEntry) {
    e.tab_id = None;
    e.window_id = None;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Imported {
    Sessions(Vec<Session>),
    Urls(UrlList),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UrlList {
    pub urls: Vec<String>,
    /// Pieces that did not look like URLs.
    pub invalid: Vec<String>,
}

/// Detects what a file / clipboard text contains: a session export (JSON) or a URL list.
pub fn parse_import(text: &str) -> Result<Imported, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("nothing to import".into());
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(file) = serde_json::from_str::<ExportFile>(trimmed) {
            if file.format != FORMAT {
                return Err(format!("unknown file format \"{}\"", file.format));
            }
            return Ok(Imported::Sessions(file.sessions));
        }
        if let Ok(s) = serde_json::from_str::<Session>(trimmed)
            && !s.entries.is_empty()
        {
            return Ok(Imported::Sessions(vec![s]));
        }
        if let Ok(list) = serde_json::from_str::<Vec<String>>(trimmed) {
            return Ok(Imported::Urls(parse_url_list(&list.join("\n"))));
        }
        if let Ok(sessions) = serde_json::from_str::<Vec<Session>>(trimmed)
            && sessions.iter().any(|s| !s.entries.is_empty())
        {
            return Ok(Imported::Sessions(sessions));
        }
        if trimmed.starts_with('{') {
            return Err("JSON file is not a Session Manager export".into());
        }
    }
    let list = parse_url_list(trimmed);
    if list.urls.is_empty() {
        return Err("no URLs found".into());
    }
    Ok(Imported::Urls(list))
}

/// Parses URLs separated by commas, semicolons, whitespace or newlines.
/// Bare hosts (`example.com/x`) get `https://`. Duplicates are dropped, order is kept.
/// A comma inside a URL is kept when the text after it does not start a new URL.
pub fn parse_url_list(text: &str) -> UrlList {
    let mut out = UrlList::default();
    let mut seen = HashSet::new();
    for chunk in text.split(|c: char| c.is_whitespace() || c == ';') {
        // Re-join comma pieces that do not start a new URL (e.g. `?ids=1,2`).
        let mut pieces: Vec<String> = Vec::new();
        for part in chunk.split(',') {
            match pieces.last_mut() {
                Some(prev) if !part.is_empty() && !starts_like_url(part) && normalize_url(prev).is_some() => {
                    prev.push(',');
                    prev.push_str(part);
                }
                _ => pieces.push(part.to_string()),
            }
        }
        for piece in pieces {
            let piece = clean(&piece);
            if piece.is_empty() {
                continue;
            }
            match normalize_url(piece) {
                Some(u) => {
                    if seen.insert(u.clone()) {
                        out.urls.push(u);
                    }
                }
                None => out.invalid.push(piece.to_string()),
            }
        }
    }
    out
}

fn clean(s: &str) -> &str {
    s.trim().trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']'))
}

fn starts_like_url(s: &str) -> bool {
    let s = clean(s);
    has_web_scheme(s) || looks_like_host(s.split(['/', '?', '#']).next().unwrap_or(""))
}

fn has_web_scheme(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    ["http://", "https://", "ftp://", "file://"].iter().any(|p| lower.starts_with(p))
}

fn looks_like_host(h: &str) -> bool {
    let host = h.split(':').next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels.last().is_some_and(|tld| tld.chars().any(|c| c.is_ascii_alphabetic()) || host.parse::<std::net::Ipv4Addr>().is_ok())
}

/// Returns a usable URL, or `None` if the text is not a web URL.
pub fn normalize_url(s: &str) -> Option<String> {
    let s = clean(s);
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return None;
    }
    if has_web_scheme(s) {
        let ok = url::scheme(s) == "file" || !url::host(s).is_empty();
        return ok.then(|| s.to_string());
    }
    if s.contains("://") {
        return None;
    }
    looks_like_host(s.split(['/', '?', '#']).next().unwrap_or("")).then(|| format!("https://{s}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Snapshot, TabEntry};

    #[test]
    fn url_lists() {
        let l = parse_url_list(
            "https://a.com/x, example.org/path,\n\"https://b.com/?ids=1,2,3\";  www.c.io  not a url, https://a.com/x, ftp://f.net/f",
        );
        assert_eq!(
            l.urls,
            vec![
                "https://a.com/x",
                "https://example.org/path",
                "https://b.com/?ids=1,2,3",
                "https://www.c.io",
                "ftp://f.net/f"
            ]
        );
        assert_eq!(l.invalid, vec!["not", "a", "url"]);
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_url("localhost.test:8080/x").as_deref(), Some("https://localhost.test:8080/x"));
        assert_eq!(normalize_url("192.168.1.1/admin").as_deref(), Some("https://192.168.1.1/admin"));
        assert_eq!(normalize_url("javascript:alert(1)"), None);
        assert_eq!(normalize_url("chrome://settings"), None);
        assert_eq!(normalize_url("hello"), None);
        assert_eq!(normalize_url("1.5"), None);
    }

    #[test]
    fn export_roundtrip() {
        let s = Session {
            id: 1,
            name: "Work".into(),
            entries: vec![TabEntry { id: 2, url: "https://a.com".into(), tab_id: Some(9), ..Default::default() }],
            snapshots: vec![Snapshot { id: 3, entries: vec![TabEntry { tab_id: Some(4), ..Default::default() }], ..Default::default() }],
            ..Default::default()
        };
        let json = export_sessions([&s], true, 1.0);
        let Imported::Sessions(back) = parse_import(&json).unwrap() else { panic!("sessions expected") };
        assert_eq!(back[0].entries[0].tab_id, None);
        assert_eq!(back[0].snapshots[0].entries[0].tab_id, None);
        let json = export_sessions([&s], false, 1.0);
        let Imported::Sessions(back) = parse_import(&json).unwrap() else { panic!("sessions expected") };
        assert!(back[0].snapshots.is_empty());
    }

    #[test]
    fn import_detection() {
        assert!(matches!(parse_import("a.com, b.com").unwrap(), Imported::Urls(l) if l.urls.len() == 2));
        assert!(matches!(parse_import(r#"["https://a.com","b.org"]"#).unwrap(), Imported::Urls(l) if l.urls.len() == 2));
        assert!(parse_import(r#"{"format":"other","version":1,"exported_at":0,"sessions":[]}"#).is_err());
        assert!(parse_import(r#"{"hello":1}"#).is_err());
        assert!(parse_import("   ").is_err());
        assert!(parse_import("nothing useful here").is_err());
        assert_eq!(export_urls(&[TabEntry { url: "https://a".into(), ..Default::default() }, TabEntry { url: "https://b".into(), ..Default::default() }]), "https://a,\nhttps://b");
    }
}
