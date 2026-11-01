//! The inspector's network log and throttle rules. Records come from the page-world
//! recorder (`web/net-recorder.js`): a first record per request (or WebSocket message), then
//! partial updates with the same id (`"update": true`). Throttle rules are applied by the
//! background (`throttleRuleFor` in `web/platform-common.js`, which this module mirrors).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Entries kept (oldest dropped first).
pub const MAX_ENTRIES: usize = 5000;

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct NetEntry {
    pub id: u64,
    /// fetch, xhr, ws (a WebSocket), frame (a WebSocket message), beacon.
    pub kind: String,
    /// HTTP method; WebSocket messages: "↑" (sent) or "↓" (received).
    pub method: String,
    pub url: String,
    pub req_headers: Vec<(String, String)>,
    pub req_body: Option<String>,
    /// What the request body is when it isn't shown as text ("multipart form", "blob, 3 KB").
    pub req_body_note: String,
    /// When it started / ended (ms since the epoch).
    pub start: f64,
    pub end: Option<f64>,
    /// HTTP status (0: no answer yet; WebSockets: 101 once open).
    pub status: i64,
    pub status_text: String,
    pub res_headers: Vec<(String, String)>,
    pub res_body: Option<String>,
    pub res_body_note: String,
    pub mime: String,
    pub size: u64,
    pub response_url: String,
    pub error: Option<String>,
    pub replay_error: Option<String>,
    /// Where in the page's code it was made.
    pub stack: String,
    /// A replay of this entry (0: not a replay).
    pub replay_of: u64,
    /// Sent again from what was recorded on an earlier page.
    pub resent: bool,
    pub replayable: bool,
    /// WebSocket messages: their socket.
    pub socket: u64,
    pub dir: String,
}

impl NetEntry {
    /// Can be sent again from what was recorded (on another page than the one that made it).
    pub fn resendable(&self) -> bool {
        matches!(self.kind.as_str(), "fetch" | "xhr" | "beacon") && (self.req_body.is_some() || self.req_body_note.is_empty())
    }

    /// The recorder step sending it again from what was recorded.
    pub fn resend_step(&self) -> Value {
        serde_json::json!({ "kind": self.kind, "method": self.method, "url": self.url, "headers": self.req_headers, "body": self.req_body })
    }

    /// The page (document) it was recorded in, and its id there.
    pub fn doc(&self) -> u64 {
        self.id >> 32
    }

    pub fn local_id(&self) -> u64 {
        self.id & 0xffff_ffff
    }

    pub fn pending(&self) -> bool {
        self.end.is_none() && self.error.is_none() && self.kind != "ws"
    }

    pub fn duration_ms(&self) -> Option<f64> {
        self.end.map(|e| (e - self.start).max(0.0))
    }

    /// The kind as shown ("Fetch", "XHR", "WS", "WS ↑", "Beacon").
    pub fn kind_label(&self) -> String {
        match self.kind.as_str() {
            "fetch" => "Fetch".into(),
            "xhr" => "XHR".into(),
            "ws" => "WS".into(),
            "frame" => format!("WS {}", self.method),
            "beacon" => "Beacon".into(),
            k => k.to_string(),
        }
    }

    /// cURL command for HTTP requests.
    pub fn as_curl(&self) -> String {
        let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        let mut out = format!("curl {}", q(&self.url));
        if self.method != "GET" {
            out += &format!(" -X {}", self.method);
        }
        for (k, v) in &self.req_headers {
            out += &format!(" \\\n  -H {}", q(&format!("{k}: {v}")));
        }
        if let Some(b) = &self.req_body {
            out += &format!(" \\\n  --data-raw {}", q(b));
        }
        out
    }

    /// A `fetch()` call for HTTP requests (to paste into a console).
    pub fn as_fetch(&self) -> String {
        let mut init = Map::new();
        init.insert("method".into(), Value::String(self.method.clone()));
        if !self.req_headers.is_empty() {
            let h: Map<String, Value> = self.req_headers.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
            init.insert("headers".into(), Value::Object(h));
        }
        if let Some(b) = &self.req_body {
            init.insert("body".into(), Value::String(b.clone()));
        }
        init.insert("credentials".into(), Value::String("include".into()));
        let init = serde_json::to_string_pretty(&Value::Object(init)).unwrap_or_default();
        format!("fetch({}, {init});", Value::String(self.url.clone()))
    }
}

/// The records of a page, merged by id, in the order they started.
#[derive(Default)]
pub struct NetLog {
    raw: HashMap<u64, Map<String, Value>>,
    pub entries: Vec<NetEntry>,
    index: HashMap<u64, usize>,
    /// Replays finished (the recorder reports each "replay all").
    pub replays_done: usize,
}

impl NetLog {
    pub fn clear(&mut self) {
        *self = NetLog::default();
    }

    pub fn get(&self, id: u64) -> Option<&NetEntry> {
        self.index.get(&id).map(|i| &self.entries[*i])
    }

    /// Adds the records of a page's `doc`ument (1, 2... for each page loaded in the tab):
    /// its ids are only unique within it, so they're kept as `doc << 32 | id`.
    pub fn apply_doc(&mut self, doc: u64, mut records: Vec<Value>) {
        for rec in &mut records {
            for key in ["id", "replayOf", "socket"] {
                if let Some(v) = rec.get_mut(key)
                    && let Some(n) = v.as_u64().filter(|n| *n > 0)
                {
                    *v = Value::from(doc << 32 | n);
                }
            }
        }
        self.apply(records);
    }

    /// Adds new records and applies updates.
    pub fn apply(&mut self, records: Vec<Value>) {
        for rec in records {
            let Value::Object(mut rec) = rec else { continue };
            let id = rec.get("id").and_then(Value::as_u64).unwrap_or(0);
            if rec.get("replayDone").is_some() {
                self.replays_done += 1;
                continue;
            }
            if id == 0 {
                continue;
            }
            let update = rec.remove("update").and_then(|v| v.as_bool()).unwrap_or(false);
            let merged = match self.raw.get_mut(&id) {
                Some(m) => {
                    m.extend(rec);
                    m.clone()
                }
                None if update => continue, // its start was dropped
                None => {
                    self.raw.insert(id, rec.clone());
                    rec
                }
            };
            let Ok(entry) = serde_json::from_value::<NetEntry>(Value::Object(merged)) else { continue };
            match self.index.get(&id) {
                Some(i) => self.entries[*i] = entry,
                None => {
                    self.index.insert(id, self.entries.len());
                    self.entries.push(entry);
                }
            }
        }
        if self.entries.len() > MAX_ENTRIES {
            let drop = self.entries.len() - MAX_ENTRIES;
            for e in self.entries.drain(..drop) {
                self.raw.remove(&e.id);
            }
            self.index = self.entries.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
        }
    }

    /// The latest `limit` records (bodies cut to `body_cap` characters), to restore the
    /// log later with [`NetLog::import`].
    pub fn export(&self, limit: usize, body_cap: usize) -> Vec<Value> {
        let from = self.entries.len().saturating_sub(limit);
        self.entries[from..]
            .iter()
            .filter_map(|e| self.raw.get(&e.id))
            .map(|m| {
                let mut m = m.clone();
                for key in ["reqBody", "resBody"] {
                    if let Some(Value::String(b)) = m.get_mut(key)
                        && b.len() > body_cap
                    {
                        let mut cut = body_cap;
                        while !b.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        b.truncate(cut);
                        b.push_str(
                            "
… (cut when the inspector was restored)",
                        );
                    }
                }
                Value::Object(m)
            })
            .collect()
    }

    /// Restores exported records (ids already include their document).
    pub fn import(&mut self, records: Vec<Value>) {
        self.apply(records);
    }

    /// Entries matching a search (every word in the URL, method, kind, status or MIME type)
    /// and a kind filter ("" for all; "fetch" covers fetch and XHR; "ws" sockets and messages).
    pub fn filtered(&self, query: &str, kind: &str) -> Vec<&NetEntry> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.entries
            .iter()
            .filter(|e| match kind {
                "" => true,
                "fetch" => e.kind == "fetch" || e.kind == "xhr",
                "ws" => e.kind == "ws" || e.kind == "frame",
                k => e.kind == k,
            })
            .filter(|e| {
                if words.is_empty() {
                    return true;
                }
                let hay = format!("{} {} {} {} {}", e.url, e.method, e.kind, e.status, e.mime).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect()
    }
}

/// Requests of a site's pages held back before they go out (after reloads too).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct ThrottleRule {
    pub id: u64,
    pub enabled: bool,
    /// The site whose pages it applies to (and its subdomains).
    pub site: String,
    /// URL glob (`*` wildcards); `*` throttles everything.
    pub pattern: String,
    /// Only this method ("" for any).
    pub method: String,
    pub delay: u32,
    /// Only in this tab (kept for the browser session); `None`: every tab of the site,
    /// kept until removed.
    pub tab: Option<i64>,
}

impl ThrottleRule {
    pub fn describe(&self) -> String {
        let what = if self.pattern == "*" { "every request".to_string() } else { self.pattern.clone() };
        let method = if self.method.is_empty() { String::new() } else { format!("{} ", self.method) };
        format!("{method}{what} +{} ms", self.delay)
    }

    pub fn matches(&self, page_url: &str, url: &str, method: &str, tab: Option<i64>) -> bool {
        let host = crate::url::host(page_url);
        self.enabled
            && (self.tab.is_none() || self.tab == tab)
            && !host.is_empty()
            && crate::url::host_within(&host, &self.site)
            && (self.method.is_empty() || self.method.eq_ignore_ascii_case(method))
            && glob_match(&self.pattern, url)
    }
}

/// The first rule matching a request.
pub fn throttle_rule_for<'a>(
    rules: &'a [ThrottleRule],
    page_url: &str,
    url: &str,
    method: &str,
    tab: Option<i64>,
) -> Option<&'a ThrottleRule> {
    rules.iter().find(|r| r.matches(page_url, url, method, tab))
}

/// Settings of the inspector's Network view.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct NetSettings {
    /// Delay of new throttle rules (ms).
    pub default_delay: u32,
    /// Pause between replayed requests (ms).
    pub replay_gap: u32,
    /// Start recording when the Network view opens.
    pub record_on_open: bool,
    /// Keep the log when the page reloads or navigates.
    pub keep_log: bool,
    /// New throttle rules apply to every tab of the site (and are kept), not just this tab.
    pub rules_all_tabs: bool,
}

impl Default for NetSettings {
    fn default() -> Self {
        NetSettings { default_delay: 2000, replay_gap: 300, record_on_open: true, keep_log: true, rules_all_tabs: false }
    }
}

/// `*` wildcards over the whole text, case-insensitive.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p = pattern.to_lowercase();
    let t = text.to_lowercase();
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == t;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !t.starts_with(first) || !t[first.len()..].ends_with(last) || t.len() < first.len() + last.len() {
        return false;
    }
    let mut at = first.len();
    let end = t.len() - last.len();
    for part in &parts[1..parts.len() - 1] {
        match t[at..end].find(part) {
            Some(i) => at += i + part.len(),
            None => return false,
        }
    }
    true
}

/// This exact request, whatever its query string.
pub fn exact_pattern(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    if base.len() < url.len() { format!("{base}*") } else { base.to_string() }
}

/// Requests like this one: same host and path, with ids (numbers, hex, UUIDs, long tokens)
/// in the path and the query string replaced by wildcards.
pub fn similar_pattern(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    let (scheme_host, path) = match base.find("://").map(|i| i + 3) {
        Some(s) => match base[s..].find('/') {
            Some(p) => base.split_at(s + p),
            None => (base, ""),
        },
        None => ("", base),
    };
    let id_like = |seg: &str| {
        let digits = seg.chars().filter(char::is_ascii_digit).count();
        !seg.is_empty()
            && (seg.chars().all(|c| c.is_ascii_digit())
                || (seg.len() >= 16 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
                || (seg.len() >= 20 && digits >= 4 && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')))
    };
    let path: Vec<&str> = path.split('/').map(|s| if id_like(s) { "*" } else { s }).collect();
    format!("{scheme_host}{}*", path.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merging_records() {
        let mut log = NetLog::default();
        log.apply(vec![
            json!({ "id": 1, "kind": "fetch", "method": "POST", "url": "https://api.x.com/v1/items", "start": 1000.0, "reqBody": "{\"a\":1}", "replayable": true }),
            json!({ "update": true, "id": 9, "status": 200 }),
            json!({ "id": 2, "kind": "xhr", "method": "GET", "url": "https://x.com/feed?page=2", "start": 1001.0 }),
        ]);
        assert_eq!(log.entries.len(), 2);
        assert!(log.get(1).unwrap().pending());
        log.apply(vec![json!({ "update": true, "id": 1, "status": 201, "end": 1250.0, "resBody": "{}", "mime": "application/json" })]);
        let e = log.get(1).unwrap();
        assert_eq!((e.status, e.duration_ms(), e.req_body.as_deref()), (201, Some(250.0), Some("{\"a\":1}")));
        assert!(!e.pending());
        log.apply(vec![json!({ "update": true, "id": 0, "replayDone": 2 })]);
        assert_eq!(log.replays_done, 1);
        assert_eq!(log.filtered("feed", "").len(), 1);
        assert_eq!(log.filtered("", "fetch").len(), 2);
        assert_eq!(log.filtered("201 json", "").len(), 1);
        assert!(log.filtered("", "ws").is_empty());
    }

    #[test]
    fn documents() {
        let mut log = NetLog::default();
        log.apply_doc(
            1,
            vec![json!({ "id": 1, "kind": "ws", "url": "wss://x" }), json!({ "id": 2, "kind": "frame", "socket": 1, "replayOf": 0 })],
        );
        log.apply_doc(
            2,
            vec![json!({ "id": 1, "kind": "fetch", "url": "https://x/a" }), json!({ "id": 2, "kind": "fetch", "replayOf": 1 })],
        );
        log.apply_doc(2, vec![json!({ "update": true, "id": 1, "status": 200 }), json!({ "update": true, "id": 0, "replayDone": 1 })]);
        assert_eq!(log.entries.len(), 4);
        let frame = log.get(1 << 32 | 2).unwrap();
        assert_eq!((frame.socket, frame.replay_of, frame.doc(), frame.local_id()), (1 << 32 | 1, 0, 1, 2));
        assert_eq!(log.get(2 << 32 | 1).unwrap().status, 200);
        assert_eq!(log.get(2 << 32 | 2).unwrap().replay_of, 2 << 32 | 1);
        assert_eq!(log.get(1 << 32 | 1).unwrap().status, 0);
        assert_eq!(log.replays_done, 1);
    }

    #[test]
    fn export_import() {
        let mut log = NetLog::default();
        let big = "é".repeat(100);
        log.apply_doc(1, (1..=5).map(|i| json!({ "id": i, "kind": "fetch", "url": format!("u{i}"), "resBody": big })).collect());
        log.apply_doc(1, vec![json!({ "update": true, "id": 5, "status": 200 })]);
        let saved = log.export(3, 21);
        assert_eq!(saved.len(), 3);
        let mut back = NetLog::default();
        back.import(saved);
        assert_eq!(back.entries.iter().map(|e| e.url.as_str()).collect::<Vec<_>>(), vec!["u3", "u4", "u5"]);
        let e = back.get(1 << 32 | 5).unwrap();
        assert_eq!(e.status, 200);
        assert!(e.res_body.as_ref().unwrap().starts_with(
            "éééééééééé
…"
        ));
        back.apply_doc(1, vec![json!({ "update": true, "id": 4, "status": 404 })]);
        assert_eq!(back.get(1 << 32 | 4).unwrap().status, 404);
    }

    #[test]
    fn bounded() {
        let mut log = NetLog::default();
        log.apply((1..=MAX_ENTRIES as u64 + 10).map(|i| json!({ "id": i, "kind": "fetch", "url": "u" })).collect());
        assert_eq!(log.entries.len(), MAX_ENTRIES);
        assert_eq!(log.entries[0].id, 11);
        assert!(log.get(5).is_none() && log.get(11).is_some());
        log.apply(vec![json!({ "update": true, "id": 12, "status": 204 })]);
        assert_eq!(log.get(12).unwrap().status, 204);
    }

    #[test]
    fn exports() {
        let e = NetEntry {
            method: "POST".into(),
            url: "https://x.com/a".into(),
            req_headers: vec![("content-type".into(), "application/json".into())],
            req_body: Some("{\"it's\":1}".into()),
            ..Default::default()
        };
        assert_eq!(
            e.as_curl(),
            "curl 'https://x.com/a' -X POST \\\n  -H 'content-type: application/json' \\\n  --data-raw '{\"it'\\''s\":1}'"
        );
        assert!(e.as_fetch().starts_with("fetch(\"https://x.com/a\", {"));
        assert!(e.as_fetch().contains("\"method\": \"POST\""));
    }

    #[test]
    fn globs() {
        assert!(glob_match("*", "https://a.com/x"));
        assert!(glob_match("https://a.com/api/*", "https://A.com/api/v1?q=1"));
        assert!(glob_match("*/api/*/items*", "https://a.com/api/7/items"));
        assert!(!glob_match("*/api/*/items*", "https://a.com/api/items"));
        assert!(glob_match("https://a.com/x", "https://a.com/x"));
        assert!(!glob_match("https://a.com/x", "https://a.com/xy"));
        assert!(!glob_match("a*a", "a"));
    }

    #[test]
    fn patterns() {
        assert_eq!(exact_pattern("https://x.com/api/items?page=2"), "https://x.com/api/items*");
        assert_eq!(exact_pattern("https://x.com/api/items"), "https://x.com/api/items");
        assert_eq!(similar_pattern("https://x.com/api/users/12345/posts?x=1"), "https://x.com/api/users/*/posts*");
        assert_eq!(similar_pattern("https://x.com/o/3f2a9c1e-0b4d-4e8f-9a7b-1c2d3e4f5a6b/v"), "https://x.com/o/*/v*");
        assert_eq!(similar_pattern("https://x.com"), "https://x.com*");
        assert!(glob_match(&similar_pattern("https://x.com/api/users/1/posts"), "https://x.com/api/users/99/posts?y"));
    }

    #[test]
    fn throttle_rules() {
        let rules = vec![
            ThrottleRule {
                id: 1,
                enabled: true,
                site: "news.com".into(),
                pattern: "*/api/*".into(),
                method: "POST".into(),
                delay: 500,
                tab: None,
            },
            ThrottleRule {
                id: 2,
                enabled: true,
                site: "news.com".into(),
                pattern: "*".into(),
                method: String::new(),
                delay: 2000,
                tab: None,
            },
            ThrottleRule {
                id: 3,
                enabled: false,
                site: "shop.com".into(),
                pattern: "*".into(),
                method: String::new(),
                delay: 100,
                tab: None,
            },
        ];
        let page = "https://www.news.com/today";
        assert_eq!(throttle_rule_for(&rules, page, "https://cdn.x.com/api/save", "post", Some(7)).unwrap().id, 1);
        assert_eq!(throttle_rule_for(&rules, page, "https://cdn.x.com/api/save", "GET", Some(7)).unwrap().id, 2);
        assert!(throttle_rule_for(&rules, "https://shop.com/", "https://shop.com/a", "GET", Some(7)).is_none());
        assert!(throttle_rule_for(&rules, "https://othernews.com/", "https://x", "GET", Some(7)).is_none());
        // A tab's rule only applies there.
        let tab_rule = vec![ThrottleRule { tab: Some(7), ..rules[1].clone() }];
        assert!(throttle_rule_for(&tab_rule, page, "https://x", "GET", Some(7)).is_some());
        assert!(throttle_rule_for(&tab_rule, page, "https://x", "GET", Some(8)).is_none());
        assert!(throttle_rule_for(&tab_rule, page, "https://x", "GET", None).is_none());
        assert_eq!(rules[1].describe(), "every request +2000 ms");
        assert_eq!(rules[0].describe(), "POST */api/* +500 ms");
    }
}
