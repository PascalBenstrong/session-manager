//! Tab search.
//!
//! Query syntax (terms are AND-ed, whitespace separated, `"quotes"` group spaces):
//!
//! | term              | meaning                                            |
//! |-------------------|----------------------------------------------------|
//! | `word`            | default field/mode (UI selectable, default: title or URL contains) |
//! | `host:github.com` | field filter: `title:`, `url:`, `host:`, `domain:`, `tag:`, `group:`, `ws:` (workspace) |
//! | `=word`, `^word$` | exact match                                        |
//! | `^word`           | prefix match                                       |
//! | `word$`           | suffix match                                       |
//! | `*wo?d*`          | glob (`*`, `?`)                                    |
//! | `/re(gex)?/`      | regular expression                                 |
//! | `-term`           | negation                                           |
//! | `is:open` `is:unloaded` `is:pinned` `is:grouped` `is:ungrouped` `is:tagged` `is:duplicate` `is:hibernated` `is:playing` | state filters |
//! | `idle:30m`        | not used for 30 minutes (`s`, `m`, `h`, `d`)       |

use std::collections::{BTreeSet, HashMap, HashSet};

use regex_lite::Regex;
use serde::{Deserialize, Serialize};

use crate::model::{Id, TabEntry, TabId};
use crate::pattern::glob_to_regex;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MatchMode {
    #[default]
    Contains,
    Exact,
    Prefix,
    Suffix,
    Glob,
    Regex,
}

impl MatchMode {
    pub const ALL: [MatchMode; 6] =
        [MatchMode::Contains, MatchMode::Exact, MatchMode::Prefix, MatchMode::Suffix, MatchMode::Glob, MatchMode::Regex];

    pub fn label(self) -> &'static str {
        match self {
            MatchMode::Contains => "Contains",
            MatchMode::Exact => "Exact",
            MatchMode::Prefix => "Prefix",
            MatchMode::Suffix => "Suffix",
            MatchMode::Glob => "Glob",
            MatchMode::Regex => "Regex",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Field {
    /// Title or URL.
    #[default]
    Any,
    Title,
    Url,
    Host,
    Domain,
    Tag,
    Group,
    Workspace,
}

impl Field {
    pub const ALL: [Field; 8] =
        [Field::Any, Field::Title, Field::Url, Field::Host, Field::Domain, Field::Tag, Field::Group, Field::Workspace];

    pub fn label(self) -> &'static str {
        match self {
            Field::Any => "Title or URL",
            Field::Title => "Title",
            Field::Url => "URL",
            Field::Host => "Host",
            Field::Domain => "Domain",
            Field::Tag => "Tag",
            Field::Group => "Group",
            Field::Workspace => "Workspace",
        }
    }

    fn from_prefix(p: &str) -> Option<Field> {
        Some(match p.to_ascii_lowercase().as_str() {
            "title" | "t" => Field::Title,
            "url" | "u" => Field::Url,
            "host" | "h" => Field::Host,
            "domain" | "d" => Field::Domain,
            "tag" => Field::Tag,
            "group" | "g" => Field::Group,
            "workspace" | "ws" | "w" => Field::Workspace,
            "any" => Field::Any,
            _ => return None,
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct SearchOptions {
    /// Mode for terms without an explicit modifier.
    pub default_mode: MatchMode,
    /// Field for terms without a `field:` prefix.
    pub default_field: Field,
    pub case_sensitive: bool,
    /// Restrict results to these groups. Empty = all. `""` stands for "no group".
    pub groups: BTreeSet<String>,
    /// Restrict results to these workspaces. Empty = all.
    /// [`NO_WORKSPACE`](crate::workspace::NO_WORKSPACE) stands for "no workspace".
    pub workspaces: BTreeSet<Id>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flag {
    Open,
    Unloaded,
    Pinned,
    Grouped,
    Ungrouped,
    Tagged,
    Duplicate,
    Hibernated,
    Playing,
}

#[derive(Debug, Clone)]
enum Matcher {
    Text { mode: MatchMode, needle: String },
    Pattern(Regex),
    Flag(Flag),
    /// Not used for at least this many milliseconds.
    Idle(f64),
}

#[derive(Debug, Clone)]
struct Term {
    field: Field,
    matcher: Matcher,
    negate: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    terms: Vec<Term>,
    case_sensitive: bool,
    groups: BTreeSet<String>,
    workspaces: BTreeSet<Id>,
    /// Entries that have duplicates (for `is:duplicate`), supplied by the caller.
    duplicates: HashSet<Id>,
    /// Workspace names by id (for `ws:`), supplied by the caller.
    workspace_names: HashMap<Id, String>,
    /// Tabs playing media (for `is:playing`), supplied by the caller.
    playing: HashSet<TabId>,
    /// Current time in ms (for `idle:`), supplied by the caller.
    now: f64,
}

impl Query {
    pub fn parse(input: &str, opts: &SearchOptions) -> Result<Query, String> {
        let mut terms = Vec::new();
        for raw in tokenize(input) {
            let mut tok = raw.as_str();
            let mut negate = false;
            if tok.len() > 1 && tok.starts_with('-') {
                negate = true;
                tok = &tok[1..];
            }
            let mut field = opts.default_field;
            if let Some((prefix, rest)) = tok.split_once(':') {
                if prefix.eq_ignore_ascii_case("is") {
                    let flag = match rest.to_ascii_lowercase().as_str() {
                        "open" | "loaded" => Flag::Open,
                        "unloaded" | "closed" => Flag::Unloaded,
                        "pinned" => Flag::Pinned,
                        "grouped" => Flag::Grouped,
                        "ungrouped" => Flag::Ungrouped,
                        "tagged" => Flag::Tagged,
                        "duplicate" | "dup" | "dupe" => Flag::Duplicate,
                        "hibernated" | "sleeping" | "discarded" => Flag::Hibernated,
                        "playing" | "audible" => Flag::Playing,
                        other => return Err(format!("unknown filter is:{other}")),
                    };
                    terms.push(Term { field, matcher: Matcher::Flag(flag), negate });
                    continue;
                }
                if prefix.eq_ignore_ascii_case("idle") {
                    let ms = parse_duration(rest).ok_or_else(|| format!("idle:{rest}: use a duration like 30m, 2h or 1d"))?;
                    terms.push(Term { field, matcher: Matcher::Idle(ms), negate });
                    continue;
                }
                if let Some(f) = Field::from_prefix(prefix) {
                    field = f;
                    tok = rest;
                }
            }
            if tok.is_empty() {
                continue;
            }
            let matcher = build_matcher(tok, opts.default_mode, opts.case_sensitive)?;
            terms.push(Term { field, matcher, negate });
        }
        Ok(Query {
            terms,
            case_sensitive: opts.case_sensitive,
            groups: opts.groups.clone(),
            workspaces: opts.workspaces.clone(),
            ..Default::default()
        })
    }

    /// Whether the query uses `is:duplicate` (so the caller should supply duplicates).
    pub fn wants_duplicates(&self) -> bool {
        self.terms.iter().any(|t| matches!(t.matcher, Matcher::Flag(Flag::Duplicate)))
    }

    pub fn with_duplicates(mut self, ids: HashSet<Id>) -> Self {
        self.duplicates = ids;
        self
    }

    /// Supplies what `ws:`, `is:playing` and `idle:` need.
    pub fn with_context(mut self, workspace_names: HashMap<Id, String>, playing: HashSet<TabId>, now: f64) -> Self {
        self.workspace_names = workspace_names;
        self.playing = playing;
        self.now = now;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.groups.is_empty() && self.workspaces.is_empty()
    }

    pub fn matches(&self, e: &TabEntry) -> bool {
        if !self.groups.is_empty() && !self.groups.contains(e.group.as_deref().unwrap_or("")) {
            return false;
        }
        if !self.workspaces.is_empty() && !self.workspaces.contains(&e.workspace.unwrap_or(crate::workspace::NO_WORKSPACE)) {
            return false;
        }
        self.terms.iter().all(|t| self.term_matches(t, e) != t.negate)
    }

    fn term_matches(&self, t: &Term, e: &TabEntry) -> bool {
        if let Matcher::Flag(flag) = t.matcher {
            return match flag {
                Flag::Open => e.is_open(),
                Flag::Unloaded => !e.is_open(),
                Flag::Pinned => e.pinned,
                Flag::Grouped => e.group.is_some(),
                Flag::Ungrouped => e.group.is_none(),
                Flag::Tagged => !e.tags.is_empty(),
                Flag::Duplicate => self.duplicates.contains(&e.id),
                Flag::Hibernated => e.is_hibernated(),
                Flag::Playing => e.tab_id.is_some_and(|t| self.playing.contains(&t)),
            };
        }
        if let Matcher::Idle(ms) = t.matcher {
            return self.now > 0.0 && self.now - e.last_accessed.max(e.created_at) >= ms;
        }
        let check = |s: &str| self.text_matches(&t.matcher, s);
        match t.field {
            Field::Any => check(&e.title) || check(&e.url),
            Field::Title => check(&e.title),
            Field::Url => check(&e.url),
            Field::Host => check(&e.host()),
            Field::Domain => check(&e.domain()),
            Field::Tag => e.tags.iter().any(|tag| check(tag)),
            Field::Group => check(e.group.as_deref().unwrap_or("")),
            Field::Workspace => check(e.workspace.and_then(|w| self.workspace_names.get(&w)).map_or("", |n| n.as_str())),
        }
    }

    fn text_matches(&self, m: &Matcher, hay: &str) -> bool {
        match m {
            Matcher::Pattern(re) => re.is_match(hay),
            Matcher::Text { mode, needle } => {
                let lowered;
                let hay = if self.case_sensitive {
                    hay
                } else {
                    lowered = hay.to_lowercase();
                    &lowered
                };
                match mode {
                    MatchMode::Contains => hay.contains(needle.as_str()),
                    MatchMode::Exact => hay == needle,
                    MatchMode::Prefix => hay.starts_with(needle.as_str()),
                    MatchMode::Suffix => hay.ends_with(needle.as_str()),
                    MatchMode::Glob | MatchMode::Regex => unreachable!("compiled to Pattern"),
                }
            }
            Matcher::Flag(_) | Matcher::Idle(_) => false,
        }
    }
}

/// `90s`, `30m`, `2h`, `1d` (a bare number means minutes) -> milliseconds.
fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim().to_ascii_lowercase();
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(i) => s.split_at(i),
        None => (s.as_str(), "m"),
    };
    let n: f64 = num.parse().ok().filter(|n: &f64| *n >= 0.0)?;
    let unit_ms = match unit {
        "s" | "sec" => 1_000.0,
        "m" | "min" => 60_000.0,
        "h" | "hr" => 3_600_000.0,
        "d" | "day" | "days" => 86_400_000.0,
        "w" | "week" | "weeks" => 604_800_000.0,
        _ => return None,
    };
    Some(n * unit_ms)
}

fn build_matcher(tok: &str, default_mode: MatchMode, case_sensitive: bool) -> Result<Matcher, String> {
    let (mode, value) = if tok.len() >= 2 && tok.starts_with('/') && tok.ends_with('/') {
        (MatchMode::Regex, &tok[1..tok.len() - 1])
    } else if let Some(v) = tok.strip_prefix('=') {
        (MatchMode::Exact, v)
    } else if tok.len() >= 2 && tok.starts_with('^') && tok.ends_with('$') {
        (MatchMode::Exact, &tok[1..tok.len() - 1])
    } else if let Some(v) = tok.strip_prefix('^') {
        (MatchMode::Prefix, v)
    } else if tok.len() > 1 && tok.ends_with('$') && default_mode != MatchMode::Regex {
        (MatchMode::Suffix, &tok[..tok.len() - 1])
    } else if default_mode == MatchMode::Contains && (tok.contains('*') || tok.contains('?')) {
        (MatchMode::Glob, tok)
    } else {
        (default_mode, tok)
    };
    Ok(match mode {
        MatchMode::Regex => {
            let src = if case_sensitive { value.to_string() } else { format!("(?i){value}") };
            Matcher::Pattern(Regex::new(&src).map_err(|e| format!("invalid regex /{value}/: {e}"))?)
        }
        MatchMode::Glob => Matcher::Pattern(glob_to_regex(value, case_sensitive)?),
        mode => Matcher::Text {
            mode,
            needle: if case_sensitive { value.to_string() } else { value.to_lowercase() },
        },
    })
}

/// Splits on whitespace, keeping `"quoted parts"` together (quotes are removed).
fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for c in input.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, url: &str, tags: &[&str], group: Option<&str>) -> TabEntry {
        TabEntry {
            title: title.into(),
            url: url.into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            group: group.map(Into::into),
            ..Default::default()
        }
    }

    fn q(s: &str) -> Query {
        Query::parse(s, &SearchOptions::default()).unwrap()
    }

    #[test]
    fn tokenizer() {
        assert_eq!(tokenize(r#"a "b c" title:"d e" -f"#), vec!["a", "b c", "title:d e", "-f"]);
    }

    #[test]
    fn modes_and_fields() {
        let e = entry("Rust Docs", "https://docs.rs/egui/latest", &["work", "rust"], Some("Dev"));
        assert!(q("rust").matches(&e));
        assert!(q("RUST docs").matches(&e));
        assert!(!q("python").matches(&e));
        assert!(q("host:docs.rs").matches(&e));
        assert!(q("domain:=docs.rs").matches(&e));
        assert!(q("host:^docs").matches(&e));
        assert!(q("url:latest$").matches(&e));
        assert!(!q("url:^latest").matches(&e));
        assert!(!q("title:^rust$").matches(&e));
        assert!(q("title:\"^rust docs$\"").matches(&e));
        assert!(q("tag:work").matches(&e));
        assert!(q("tag:=rust").matches(&e));
        assert!(q("group:dev").matches(&e));
        assert!(q("-tag:play").matches(&e));
        assert!(!q("-tag:work").matches(&e));
        assert!(q("url:*docs.rs/*").matches(&e));
        assert!(q(r"/egui\/l/").matches(&e));
        assert!(q("is:unloaded is:grouped is:tagged").matches(&e));
        assert!(!q("is:open").matches(&e));
    }

    #[test]
    fn options() {
        let e = entry("Rust Docs", "https://docs.rs/", &[], None);
        let mut opts = SearchOptions { case_sensitive: true, ..Default::default() };
        assert!(!Query::parse("rust", &opts).unwrap().matches(&e));
        assert!(Query::parse("Rust", &opts).unwrap().matches(&e));
        opts.case_sensitive = false;
        opts.default_mode = MatchMode::Prefix;
        opts.default_field = Field::Title;
        assert!(Query::parse("rust", &opts).unwrap().matches(&e));
        assert!(!Query::parse("docs", &opts).unwrap().matches(&e));
        opts.groups.insert("".into());
        assert!(Query::parse("", &opts).unwrap().matches(&e));
        opts.groups = ["Dev".to_string()].into();
        assert!(!Query::parse("", &opts).unwrap().matches(&e));
    }

    #[test]
    fn duplicate_filter() {
        let a = TabEntry { id: 1, ..Default::default() };
        let b = TabEntry { id: 2, ..Default::default() };
        let q = q("is:dup");
        assert!(q.wants_duplicates());
        let q = q.with_duplicates([1].into());
        assert!(q.matches(&a));
        assert!(!q.matches(&b));
    }

    #[test]
    fn workspaces_hibernation_idle() {
        let e = TabEntry {
            id: 1,
            tab_id: Some(9),
            discarded: true,
            workspace: Some(5),
            last_accessed: 1_000.0,
            ..Default::default()
        };
        let names: HashMap<Id, String> = [(5, "Deep Work".to_string())].into();
        let ctx = |q: Query| q.with_context(names.clone(), [9].into(), 1_000.0 + 3_600_000.0);
        assert!(ctx(q("ws:deep is:hibernated is:playing idle:30m")).matches(&e));
        assert!(ctx(q("workspace:\"=deep work\"")).matches(&e));
        assert!(!ctx(q("idle:2h")).matches(&e));
        assert!(!q("idle:1s").matches(&e), "no time supplied");
        assert!(Query::parse("idle:soon", &SearchOptions::default()).is_err());
        let mut opts = SearchOptions::default();
        opts.workspaces.insert(5);
        assert!(Query::parse("", &opts).unwrap().matches(&e));
        opts.workspaces = [crate::workspace::NO_WORKSPACE].into();
        assert!(!Query::parse("", &opts).unwrap().matches(&e));
    }

    #[test]
    fn errors() {
        assert!(Query::parse("/(/", &SearchOptions::default()).is_err());
        assert!(Query::parse("is:bogus", &SearchOptions::default()).is_err());
    }
}
