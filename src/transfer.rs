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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Imported {
    Sessions(Vec<Session>),
    Urls(UrlList),
    /// A CSV / TSV file with a header row: the column with the URLs is the user's to pick.
    Table(Table),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct UrlList {
    pub urls: Vec<String>,
    /// Pieces that did not look like URLs.
    pub invalid: Vec<String>,
}

/// A CSV / TSV file with a header row, for choosing which column holds the URLs.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Table {
    pub headers: Vec<String>,
    /// Data rows, padded to the width of the header.
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// A column's title, or "Column N" when the header has none.
    pub fn column_name(&self, column: usize) -> String {
        self.headers
            .get(column)
            .map(|h| h.trim())
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Column {}", column + 1))
    }

    /// The column that holds URLs: the one with the most of them (the leftmost on a tie).
    pub fn best_column(&self) -> usize {
        let mut best = (0, 0); // (cells with a URL, column)
        for column in 0..self.headers.len() {
            let count = self.rows.iter().filter(|row| row.get(column).is_some_and(|cell| has_url(cell))).count();
            if count > best.0 {
                best = (count, column);
            }
        }
        best.1
    }

    /// The URLs in a column, and the cells that are not URLs.
    pub fn urls(&self, column: usize) -> UrlList {
        let mut out = UrlList::default();
        let mut seen = HashSet::new();
        for row in &self.rows {
            if let Some(cell) = row.get(column) {
                collect_urls(cell, &mut out, &mut seen);
            }
        }
        out
    }
}

/// Detects what a file / clipboard text contains: a session export (JSON), a CSV / TSV
/// with a header row, or a URL list.
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
    if let Some(table) = parse_csv_table(trimmed) {
        return Ok(Imported::Table(table));
    }
    let list = parse_url_list(trimmed);
    if list.urls.is_empty() {
        return Err("no URLs found".into());
    }
    Ok(Imported::Urls(list))
}

/// A CSV / TSV with a header row (its first row has no URLs, but a later one does).
fn parse_csv_table(text: &str) -> Option<Table> {
    let text = text.trim_start_matches('\u{feff}').trim();
    if !text.contains('\n') {
        return None;
    }
    for delimiter in [',', ';', '\t'] {
        let mut rows = parse_csv(text, delimiter);
        if rows.len() < 2 {
            continue;
        }
        let width = rows[0].len();
        if width < 2 || rows[0].iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let header = rows.remove(0);
        // A first row that holds URLs starts a plain list, not a header.
        if header.iter().any(|c| has_url(c)) {
            continue;
        }
        if !header.iter().any(|c| c.trim().chars().any(char::is_alphabetic)) {
            continue;
        }
        rows.retain(|r| r.iter().any(|c| !c.trim().is_empty()));
        for row in &mut rows {
            row.resize(width, String::new());
        }
        if rows.is_empty() || !rows.iter().any(|r| r.iter().any(|c| has_url(c))) {
            continue;
        }
        let headers = header.iter().map(|h| h.trim().trim_matches('\'').to_string()).collect();
        return Some(Table { headers, rows });
    }
    None
}

/// Whether a CSV cell holds a web URL (or several, separated).
fn has_url(cell: &str) -> bool {
    normalize_url(cell).is_some() || !parse_url_list(cell).urls.is_empty()
}

/// CSV rows: `delimiter` separates fields, a field in double quotes may hold delimiters
/// and newlines, and `""` inside it is a quote.
fn parse_csv(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.trim().is_empty() {
            field.clear();
            quoted = true;
        } else if c == delimiter {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' {
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
        } else if c != '\r' {
            field.push(c);
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    while rows.last().is_some_and(|r| r.iter().all(|c| c.trim().is_empty())) {
        rows.pop();
    }
    rows
}

/// Parses URLs separated by commas, semicolons, whitespace or newlines.
/// Bare hosts (`example.com/x`) get `https://`. Duplicates are dropped, order is kept.
/// A comma inside a URL is kept when the text after it does not start a new URL, and a
/// URL may be enclosed in single or double quotes (then its commas are always its own).
pub fn parse_url_list(text: &str) -> UrlList {
    let mut out = UrlList::default();
    let mut seen = HashSet::new();
    collect_urls(text, &mut out, &mut seen);
    out
}

fn collect_urls(text: &str, out: &mut UrlList, seen: &mut HashSet<String>) {
    for chunk in split_outside_quotes(text, |c| c.is_whitespace() || c == ';') {
        // A chunk quoted as a whole: one URL, or a quoted list to split inside.
        if let Some(inner) = whole_quoted(&chunk) {
            push_quoted(inner, out, seen);
            continue;
        }
        // Re-join comma pieces that do not start a new URL (e.g. `?ids=1,2`); a quoted
        // piece is a field of its own and never joins the one before it.
        let mut pieces: Vec<String> = Vec::new();
        for part in split_outside_quotes(&chunk, |c| c == ',') {
            let quoted = whole_quoted(&part).is_some();
            let rejoin =
                !quoted && !part.is_empty() && !starts_like_url(&part) && pieces.last().is_some_and(|prev| normalize_url(prev).is_some());
            if rejoin {
                let prev = pieces.last_mut().expect("a piece to join to");
                prev.push(',');
                prev.push_str(&part);
            } else if !part.is_empty() {
                pieces.push(part);
            }
        }
        for piece in pieces {
            match whole_quoted(&piece) {
                Some(inner) => push_quoted(inner, out, seen),
                None => push_piece(&piece, out, seen),
            }
        }
    }
}

fn push_quoted(inner: &str, out: &mut UrlList, seen: &mut HashSet<String>) {
    let inner = inner.trim();
    if inner.is_empty() {
        return;
    }
    // One URL in quotes keeps its commas; several ("https://a.com,https://b.com") are a
    // list, even when quoted as a whole.
    if inner.matches("://").count() < 2
        && let Some(u) = normalize_url(inner)
    {
        if seen.insert(u.clone()) {
            out.urls.push(u);
        }
        return;
    }
    // A quoted list ("a.com, b.com") splits inside; a quoted field that holds no URL is
    // skipped whole.
    let mut sub = UrlList::default();
    collect_urls(inner, &mut sub, &mut HashSet::new());
    if sub.urls.is_empty() {
        out.invalid.push(inner.to_string());
    } else {
        for u in sub.urls {
            if seen.insert(u.clone()) {
                out.urls.push(u);
            }
        }
        out.invalid.extend(sub.invalid);
    }
}

fn push_piece(piece: &str, out: &mut UrlList, seen: &mut HashSet<String>) {
    let piece = clean(piece);
    if piece.is_empty() {
        return;
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

/// Splits text at `separator` outside single and double quotes; the quotes are kept.
fn split_outside_quotes(text: &str, separator: impl Fn(char) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                current.push(c);
                if c == q {
                    // "" inside a double-quoted field is an escaped quote.
                    if q == '"' && chars.peek() == Some(&'"') {
                        current.push(chars.next().unwrap_or('"'));
                    } else {
                        quote = None;
                    }
                }
            }
            None => {
                // A quote opens a quoted piece at the start of one (not on an apostrophe
                // inside a word, as in "it's" or in a URL).
                let opens = (c == '"' || c == '\'') && current.chars().last().is_none_or(|p| !p.is_alphanumeric());
                if opens {
                    quote = Some(c);
                    current.push(c);
                } else if separator(c) {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(c);
                }
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The inside of text that is quoted as a whole (`"…"` or `'…'`).
fn whole_quoted(s: &str) -> Option<&str> {
    let s = s.trim();
    let quote = s.chars().next()?;
    (s.len() >= 2 && (quote == '"' || quote == '\'') && s.ends_with(quote)).then(|| &s[1..s.len() - 1])
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
    if let Some(rest) = s.strip_prefix("//") {
        return looks_like_host(rest.split(['/', '?', '#']).next().unwrap_or("")).then(|| format!("https:{s}"));
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
            vec!["https://a.com/x", "https://example.org/path", "https://b.com/?ids=1,2,3", "https://www.c.io", "ftp://f.net/f"]
        );
        assert_eq!(l.invalid, vec!["not", "a", "url"]);
    }

    #[test]
    fn quoted_url_lists() {
        let l = parse_url_list("'https://a.com/x' \"https://b.com/y?ids=1,2\" 'https://c.com/z'");
        assert_eq!(l.urls, vec!["https://a.com/x", "https://b.com/y?ids=1,2", "https://c.com/z"]);
        assert!(l.invalid.is_empty());
        // A list quoted as a whole is split inside the quotes.
        let l = parse_url_list("\"https://a.com/x, https://b.com/y\"");
        assert_eq!(l.urls, vec!["https://a.com/x", "https://b.com/y"]);
        let l = parse_url_list("\"https://a.com/x,https://b.com/y\"");
        assert_eq!(l.urls, vec!["https://a.com/x", "https://b.com/y"]);
        // Quoted fields of a pasted CSV row: commas inside a URL stay its own, and the
        // fields around it are not glued to the URL.
        let l = parse_url_list("1,\"https://a.com/?a=1,2\",\"A title\",https://b.com");
        assert_eq!(l.urls, vec!["https://a.com/?a=1,2", "https://b.com"]);
        assert_eq!(l.invalid, vec!["1", "A title"]);
        // An apostrophe inside a word (or a URL) is not a quote.
        let l = parse_url_list("https://en.wikipedia.org/wiki/It's, https://b.com/x");
        assert_eq!(l.urls, vec!["https://en.wikipedia.org/wiki/It's", "https://b.com/x"]);
    }

    #[test]
    fn csv_tables() {
        let csv = "id,url,title\n1,https://a.com/x,Hello\n2,b.org/y,\"World, again\"";
        let Imported::Table(t) = parse_import(csv).unwrap() else { panic!("a table expected") };
        assert_eq!(t.headers, ["id", "url", "title"]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1][2], "World, again");
        assert_eq!((t.column_name(1).as_str(), t.column_name(7).as_str()), ("url", "Column 8"));
        assert_eq!(t.best_column(), 1);
        assert_eq!(t.urls(1).urls, vec!["https://a.com/x", "https://b.org/y"]);
        assert!(t.urls(1).invalid.is_empty());
        assert!(t.urls(0).urls.is_empty());
        // Several URLs in one cell.
        let csv = "id,pages\n1,\"https://a.com/x https://b.com/y\"";
        let Imported::Table(t) = parse_import(csv).unwrap() else { panic!("a table expected") };
        assert_eq!(t.urls(1).urls.len(), 2);
    }

    #[test]
    fn csv_delimiters_and_quotes() {
        let tsv = "id\turl\n1\thttps://a.com/x\n2\thttps://b.com/y";
        let Imported::Table(t) = parse_import(tsv).unwrap() else { panic!("a table expected") };
        assert_eq!(t.headers, ["id", "url"]);
        assert_eq!(t.urls(1).urls.len(), 2);
        let semi = "\"id\";\"link\"\n\"1\";\"https://a.com/?a=1,2\"";
        let Imported::Table(t) = parse_import(semi).unwrap() else { panic!("a table expected") };
        assert_eq!(t.headers, ["id", "link"]);
        assert_eq!(t.urls(1).urls, vec!["https://a.com/?a=1,2"]);
        let csv = "\"id\",\"url\",\"note\"\n\"1\",\"https://a.com/x\",\"say \"\"hi\"\"\"";
        let Imported::Table(t) = parse_import(csv).unwrap() else { panic!("a table expected") };
        assert_eq!(t.rows[0][2], "say \"hi\"");
    }

    #[test]
    fn csv_without_a_header_is_a_url_list() {
        // Two rows of two URLs each: a list, not a table (the first row holds URLs).
        let text = "https://a.com,https://b.com\nhttps://c.com,https://d.com";
        let Imported::Urls(l) = parse_import(text).unwrap() else { panic!("URLs expected") };
        assert_eq!(l.urls.len(), 4);
        // A header without any URL under it is not a table either.
        assert!(parse_import("url,title\n,hello").is_err());
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_url("localhost.test:8080/x").as_deref(), Some("https://localhost.test:8080/x"));
        assert_eq!(normalize_url("192.168.1.1/admin").as_deref(), Some("https://192.168.1.1/admin"));
        assert_eq!(normalize_url("//example.com/x").as_deref(), Some("https://example.com/x"));
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
        assert_eq!(
            export_urls(&[
                TabEntry { url: "https://a".into(), ..Default::default() },
                TabEntry { url: "https://b".into(), ..Default::default() }
            ]),
            "https://a,\nhttps://b"
        );
    }
}
