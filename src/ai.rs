//! AI integrations: summaries of pages written by a language model the user configures
//! (OpenAI-compatible endpoints such as OpenAI, Ollama, LM Studio or OpenRouter, or the
//! Anthropic Messages API), and vector search over those summaries.
//!
//! Everything here is pure: it builds HTTP requests and reads their answers; the background
//! sends them (`smPlatform.httpRequest`). Nothing is sent unless the user turned AI on and
//! added a provider.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{Id, TabEntry};

/// Storage key prefix of a session's vector index (`sm_vectors_<session id>`).
pub const VECTORS_PREFIX: &str = "sm_vectors_";
/// Dimensions of the local (hashed) vectors.
pub const LOCAL_DIMS: usize = 256;
/// The model name an index of local vectors carries.
pub const LOCAL_MODEL: &str = "local-hash-v1";
/// Longest summary kept (characters).
pub const MAX_DESCRIPTION_CHARS: usize = 2000;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProviderKind {
    /// `POST {base}/chat/completions` and `POST {base}/embeddings` (OpenAI, Ollama's `/v1`,
    /// LM Studio, OpenRouter, vLLM...).
    #[default]
    OpenAiCompatible,
    /// `POST {base}/v1/messages` (no embeddings: vectors are computed locally).
    Anthropic,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 2] = [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic];

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => "OpenAI-compatible",
            ProviderKind::Anthropic => "Anthropic",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => "",
            ProviderKind::Anthropic => "claude-opus-5-5",
        }
    }
}

/// A language model the user added.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AiProvider {
    pub id: Id,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    /// Kept in the extension's local storage only. Empty for local servers (Ollama).
    pub api_key: String,
    pub model: String,
    /// Model for embeddings (OpenAI-compatible only). Empty: vectors are computed locally.
    pub embedding_model: String,
    /// Instructions for the summary. Empty: [`DEFAULT_PROMPT`].
    pub prompt: String,
}

pub const DEFAULT_PROMPT: &str = "Summarise the following web page in 2-4 sentences, in the page's language. \
Say what the page is and what it is about, so it can be found again later by searching. \
Reply with the summary only.";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AiSettings {
    /// Master switch: nothing is sent to any provider while off. Summaries already written stay.
    pub enabled: bool,
    pub providers: Vec<AiProvider>,
    /// The provider used (0 or unknown: the first one).
    pub provider: Id,
    /// Summarise pages as they finish loading (otherwise only on request).
    pub auto_summarize: bool,
    /// Sites (with subdomains) whose pages are never sent for summaries.
    pub exclude_sites: Vec<String>,
    /// Longest page text sent (characters).
    pub max_input_chars: u32,
    /// Keep a vector index of the tabs (saved per session) for vector search.
    pub vector_search: bool,
    /// After searching other sessions too, free their vectors from memory (they're read
    /// again from storage when needed).
    pub unload_inactive: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            providers: Vec::new(),
            provider: 0,
            auto_summarize: false,
            exclude_sites: Vec::new(),
            max_input_chars: 12_000,
            vector_search: true,
            unload_inactive: true,
        }
    }
}

impl AiSettings {
    /// The provider in use, if AI is on and one is configured.
    pub fn active_provider(&self) -> Option<&AiProvider> {
        if !self.enabled {
            return None;
        }
        self.providers.iter().find(|p| p.id == self.provider).or_else(|| self.providers.first())
    }

    /// The embedding model name an index made now would use.
    pub fn index_model(&self) -> String {
        match self.active_provider() {
            Some(p) if p.kind == ProviderKind::OpenAiCompatible && !p.embedding_model.trim().is_empty() => {
                format!("{}|{}", p.base_url.trim_end_matches('/'), p.embedding_model.trim())
            }
            _ => LOCAL_MODEL.to_string(),
        }
    }
}

/// An HTTP request for the platform layer to send.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HttpRequest {
    pub url: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

/// An HTTP answer from the platform layer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

fn base(p: &AiProvider) -> String {
    let b = p.base_url.trim().trim_end_matches('/');
    if b.is_empty() { p.kind.default_base_url().to_string() } else { b.to_string() }
}

/// Cuts `s` to at most `max` characters (on a character boundary).
pub fn clip(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The request asking `p` to summarise a page.
pub fn summary_request(p: &AiProvider, title: &str, url: &str, text: &str, max_chars: usize) -> Result<HttpRequest, String> {
    let model = p.model.trim();
    if model.is_empty() {
        return Err(format!("AI provider \"{}\" has no model set", p.name));
    }
    let prompt = if p.prompt.trim().is_empty() { DEFAULT_PROMPT } else { p.prompt.trim() };
    let page = format!("Title: {title}\nURL: {url}\n\n{}", clip(text.trim(), max_chars.max(200)));
    let mut headers = BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
    let (url, body) = match p.kind {
        ProviderKind::OpenAiCompatible => {
            if !p.api_key.trim().is_empty() {
                headers.insert("authorization".into(), format!("Bearer {}", p.api_key.trim()));
            }
            let body = serde_json::json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": prompt},
                    {"role": "user", "content": page},
                ],
            });
            (format!("{}/chat/completions", base(p)), body)
        }
        ProviderKind::Anthropic => {
            if p.api_key.trim().is_empty() {
                return Err(format!("AI provider \"{}\" needs an API key", p.name));
            }
            headers.insert("x-api-key".into(), p.api_key.trim().to_string());
            headers.insert("anthropic-version".into(), "2026-10-21".into());
            // Requests from a browser extension are browser requests to the API.
            headers.insert("anthropic-dangerous-direct-browser-access".into(), "true".into());
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 1024,
                "system": prompt,
                "messages": [{"role": "user", "content": page}],
            });
            (format!("{}/v1/messages", base(p)), body)
        }
    };
    Ok(HttpRequest { url, method: "POST".into(), headers, body: body.to_string() })
}

/// The error message of a failed provider answer.
fn http_error(r: &HttpResponse) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(&r.body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message").or_else(|| v.get("error")).or_else(|| v.get("message")).and_then(|m| m.as_str().map(String::from))
        })
        .unwrap_or_else(|| clip(r.body.trim(), 200).to_string());
    if r.status == 0 { format!("request failed: {detail}") } else { format!("HTTP {}: {detail}", r.status) }
}

/// The summary in a provider's answer.
pub fn parse_summary(kind: ProviderKind, r: &HttpResponse) -> Result<String, String> {
    if !(200..300).contains(&r.status) {
        return Err(http_error(r));
    }
    let v: serde_json::Value = serde_json::from_str(&r.body).map_err(|e| format!("unreadable answer: {e}"))?;
    let text = match kind {
        ProviderKind::OpenAiCompatible => v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or_default().to_string(),
        ProviderKind::Anthropic => {
            if v.get("stop_reason").and_then(|s| s.as_str()) == Some("refusal") {
                return Err("the model declined to summarise this page".into());
            }
            v.get("content")
                .and_then(|c| c.as_array())
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default()
        }
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("the model returned no summary".into());
    }
    Ok(clip(text, MAX_DESCRIPTION_CHARS).to_string())
}

/// The request for embeddings of `texts` (OpenAI-compatible providers with an embedding model).
pub fn embedding_request(p: &AiProvider, texts: &[String]) -> Option<HttpRequest> {
    if p.kind != ProviderKind::OpenAiCompatible || p.embedding_model.trim().is_empty() {
        return None;
    }
    let mut headers = BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
    if !p.api_key.trim().is_empty() {
        headers.insert("authorization".into(), format!("Bearer {}", p.api_key.trim()));
    }
    let body = serde_json::json!({"model": p.embedding_model.trim(), "input": texts});
    Some(HttpRequest { url: format!("{}/embeddings", base(p)), method: "POST".into(), headers, body: body.to_string() })
}

/// The vectors in an embeddings answer, in input order.
pub fn parse_embeddings(r: &HttpResponse, n: usize) -> Result<Vec<Vec<f32>>, String> {
    if !(200..300).contains(&r.status) {
        return Err(http_error(r));
    }
    let v: serde_json::Value = serde_json::from_str(&r.body).map_err(|e| format!("unreadable answer: {e}"))?;
    let data = v.get("data").and_then(|d| d.as_array()).ok_or("no embeddings in the answer")?;
    let mut out = vec![Vec::new(); n];
    for (i, item) in data.iter().enumerate() {
        let idx = item.get("index").and_then(|x| x.as_u64()).map_or(i, |x| x as usize);
        let vec: Vec<f32> = item
            .get("embedding")
            .and_then(|e| e.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
            .unwrap_or_default();
        if let Some(slot) = out.get_mut(idx) {
            *slot = vec;
        }
    }
    if out.iter().any(|v| v.is_empty()) {
        return Err("the answer is missing embeddings".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------- vectors

/// What an entry's vector is computed from.
pub fn entry_text(e: &TabEntry) -> String {
    let mut s = format!("{}\n{}", e.display_title(), e.host());
    if !e.description.is_empty() {
        s.push('\n');
        s.push_str(&e.description);
    }
    s
}

/// FNV-1a.
pub fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

/// A vector of `text` that needs no model: hashed words, word pairs and word trigrams of
/// characters (so "summarise" and "summary" share features), L2-normalized.
pub fn local_embed(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; LOCAL_DIMS];
    let mut add = |feature: &str, weight: f32| {
        let h = hash(feature);
        let sign = if h >> 63 == 0 { 1.0 } else { -1.0 };
        v[(h % LOCAL_DIMS as u64) as usize] += sign * weight;
    };
    let ws = words(text);
    for (i, w) in ws.iter().enumerate() {
        add(w, 1.0);
        if let Some(next) = ws.get(i + 1) {
            add(&format!("{w} {next}"), 0.5);
        }
        let chars: Vec<char> = format!("^{w}$").chars().collect();
        for t in chars.windows(3) {
            add(&t.iter().collect::<String>(), 0.25);
        }
    }
    normalize(&mut v);
    v
}

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 && n.is_finite() {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

/// A vector stored at 8 bits per dimension (normalized first; enough for ranking).
pub fn quantize(v: &[f32]) -> Vec<i8> {
    let mut v = v.to_vec();
    normalize(&mut v);
    v.iter().map(|x| (x * 127.0).round().clamp(-127.0, 127.0) as i8).collect()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct IndexedVector {
    /// [`hash`] of the [`entry_text`] it was made from (it's stale when that changes).
    pub h: u64,
    pub q: Vec<i8>,
}

/// The vectors of one session's entries (saved as `sm_vectors_<session id>`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct VectorIndex {
    pub model: String,
    pub items: BTreeMap<Id, IndexedVector>,
}

impl VectorIndex {
    pub fn new(model: &str) -> Self {
        Self { model: model.to_string(), items: BTreeMap::new() }
    }

    /// Drops vectors of entries no longer in the session. Returns true if any went.
    pub fn retain_entries(&mut self, entries: &[TabEntry]) -> bool {
        let before = self.items.len();
        let ids: std::collections::HashSet<Id> = entries.iter().map(|e| e.id).collect();
        self.items.retain(|id, _| ids.contains(id));
        self.items.len() != before
    }

    /// The entries whose vector is missing or stale, with the text to embed.
    pub fn stale<'a>(&self, entries: &'a [TabEntry]) -> Vec<(&'a TabEntry, String)> {
        entries.iter().map(|e| (e, entry_text(e))).filter(|(e, text)| self.items.get(&e.id).is_none_or(|v| v.h != hash(text))).collect()
    }

    pub fn insert(&mut self, id: Id, text: &str, v: &[f32]) {
        self.items.insert(id, IndexedVector { h: hash(text), q: quantize(v) });
    }

    /// The entries most similar to `query` (cosine), best first, at most `limit`.
    pub fn search(&self, query: &[f32], limit: usize) -> Vec<(Id, f32)> {
        let q = quantize(query);
        let mut hits: Vec<(Id, f32)> = self
            .items
            .iter()
            .filter(|(_, v)| v.q.len() == q.len())
            .map(|(id, v)| {
                let dot: i32 = v.q.iter().zip(&q).map(|(a, b)| *a as i32 * *b as i32).sum();
                (*id, dot as f32 / (127.0 * 127.0))
            })
            .filter(|(_, s)| *s > 0.05)
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.truncate(limit);
        hits
    }
}

/// A tab found by vector search.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct VectorHit {
    pub entry: crate::model::EntryRef,
    pub score: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(kind: ProviderKind) -> AiProvider {
        AiProvider { id: 1, name: "p".into(), kind, api_key: "k".into(), model: "m".into(), ..Default::default() }
    }

    #[test]
    fn builds_requests() {
        let r = summary_request(&provider(ProviderKind::Anthropic), "T", "https://a.com", "body", 100).unwrap();
        assert_eq!(r.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(r.headers["x-api-key"], "k");
        assert!(r.headers.contains_key("anthropic-dangerous-direct-browser-access"));
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["model"], "m");
        assert!(v["messages"][0]["content"].as_str().unwrap().contains("body"));

        let p = AiProvider {
            base_url: "http://localhost:11434/v1/".into(),
            api_key: String::new(),
            ..provider(ProviderKind::OpenAiCompatible)
        };
        let r = summary_request(&p, "T", "u", "x", 100).unwrap();
        assert_eq!(r.url, "http://localhost:11434/v1/chat/completions");
        assert!(!r.headers.contains_key("authorization"));
        assert!(embedding_request(&p, &["a".into()]).is_none(), "no embedding model");
        let p = AiProvider { embedding_model: "e".into(), ..p };
        assert_eq!(embedding_request(&p, &["a".into()]).unwrap().url, "http://localhost:11434/v1/embeddings");

        assert!(summary_request(&AiProvider { model: " ".into(), ..provider(ProviderKind::Anthropic) }, "", "", "", 1).is_err());
        assert!(summary_request(&AiProvider { api_key: "".into(), ..provider(ProviderKind::Anthropic) }, "", "", "", 1).is_err());
    }

    #[test]
    fn long_pages_are_clipped_on_char_boundaries() {
        let text = "é".repeat(5000);
        let r = summary_request(&provider(ProviderKind::OpenAiCompatible), "T", "u", &text, 300).unwrap();
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        assert!(v["messages"][1]["content"].as_str().unwrap().chars().filter(|c| *c == 'é').count() == 300);
        assert_eq!(clip("abc", 10), "abc");
        assert_eq!(clip("", 0), "");
    }

    #[test]
    fn parses_answers_and_errors() {
        let ok = |body: &str| HttpResponse { status: 200, body: body.into() };
        assert_eq!(parse_summary(ProviderKind::OpenAiCompatible, &ok(r#"{"choices":[{"message":{"content":" Hi "}}]}"#)).unwrap(), "Hi");
        let a = r#"{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"Sum"}],"stop_reason":"end_turn"}"#;
        assert_eq!(parse_summary(ProviderKind::Anthropic, &ok(a)).unwrap(), "Sum");
        assert!(parse_summary(ProviderKind::Anthropic, &ok(r#"{"content":[],"stop_reason":"refusal"}"#)).is_err());
        let e = parse_summary(ProviderKind::Anthropic, &HttpResponse { status: 401, body: r#"{"error":{"message":"bad key"}}"#.into() });
        assert_eq!(e.unwrap_err(), "HTTP 401: bad key");
        assert!(parse_summary(ProviderKind::OpenAiCompatible, &ok("not json")).is_err());
        assert!(parse_summary(ProviderKind::OpenAiCompatible, &HttpResponse { status: 0, body: "offline".into() }).is_err());

        let emb = ok(r#"{"data":[{"index":1,"embedding":[0,1]},{"index":0,"embedding":[1,0]}]}"#);
        assert_eq!(parse_embeddings(&emb, 2).unwrap(), vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert!(parse_embeddings(&emb, 3).is_err());
        assert!(parse_embeddings(&ok(r#"{"data":[{"index":99,"embedding":[1]}]}"#), 1).is_err());
    }

    #[test]
    fn local_vectors_rank_related_text_first() {
        let mut idx = VectorIndex::new(LOCAL_MODEL);
        let docs = [(1, "Rust programming language book ownership borrowing"), (2, "Chocolate cake recipe with eggs and flour"), (3, "")];
        for (id, text) in docs {
            idx.insert(id, text, &local_embed(text));
        }
        let hits = idx.search(&local_embed("rust borrowing"), 10);
        assert_eq!(hits[0].0, 1);
        assert!(hits.iter().all(|(id, _)| *id != 3), "empty text matches nothing");
        assert!(idx.search(&local_embed("baking a cake"), 1)[0].0 == 2);
        assert!(local_embed("").iter().all(|x| *x == 0.0));
        // Mismatched dimensions are skipped, not a crash.
        assert!(idx.search(&[1.0, 0.0], 5).is_empty());
    }

    #[test]
    fn stale_vectors_follow_entry_text() {
        let mut e = TabEntry { id: 7, title: "A".into(), url: "https://a.com".into(), ..Default::default() };
        let mut idx = VectorIndex::new(LOCAL_MODEL);
        assert_eq!(idx.stale(std::slice::from_ref(&e)).len(), 1);
        let text = entry_text(&e);
        idx.insert(7, &text, &local_embed(&text));
        assert!(idx.stale(std::slice::from_ref(&e)).is_empty());
        e.description = "now summarised".into();
        assert_eq!(idx.stale(std::slice::from_ref(&e)).len(), 1);
        assert!(idx.retain_entries(&[]));
        assert!(idx.items.is_empty());
    }

    #[test]
    fn provider_selection_respects_master_switch() {
        let mut s = AiSettings { providers: vec![provider(ProviderKind::OpenAiCompatible)], ..Default::default() };
        assert!(s.active_provider().is_none());
        assert_eq!(s.index_model(), LOCAL_MODEL);
        s.enabled = true;
        assert_eq!(s.active_provider().unwrap().id, 1);
        s.providers[0].embedding_model = "e".into();
        assert!(s.index_model().ends_with("|e"));
    }
}
