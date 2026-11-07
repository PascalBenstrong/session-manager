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
//! | `idle:30m`        | not used for 30 minutes (`s`, `m`, `h`, `d`, `w`, `mo`, `y`) |
//! | `opened:2h`       | time filter (see below): `used:`, `added:`, `opened:`, `closed:`, `loaded:`, `unloaded:` |
//!
//! Time filter values:
//!
//! | value                          | meaning                                        |
//! |--------------------------------|------------------------------------------------|
//! | `2h`, `<2h`                    | within the last 2 hours                        |
//! | `>1w`                          | more than a week ago                           |
//! | `1d..1w`                       | between a day and a week ago                   |
//! | `today` `yesterday` `week` `lastweek` `month` `lastmonth` `year` `lastyear` | that calendar period (weeks start on Monday) |
//! | `2026-11-01`, `2026-11`, `2026` | that day, month or year (local time)           |
//! | `<2026-11-01`, `>=2026-11`     | before / after (`<=`, `>=`: including it)      |
//! | `2026-11-01..2026-11-05`, `2026-11..`, `..lastweek` | from the start of one to the end of the other; a missing side is open |
//!
//! A tab without that time (e.g. never closed) matches no time filter.

use std::collections::{BTreeSet, HashMap, HashSet};

use regex_lite::Regex;
use serde::{Deserialize, Serialize};

use crate::dedupe::{Scope, duplicate_ids};
use crate::model::{AppState, Id, TabEntry, TabId, TabSort};
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
    Text {
        mode: MatchMode,
        needle: String,
    },
    Pattern(Regex),
    Flag(Flag),
    /// Not used for at least this many milliseconds.
    Idle(f64),
    /// One of the entry's times is in a span.
    Time(TabSort, Span),
}

/// When a time filter matches, resolved against the current time ([`Span::resolve`]).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Span {
    /// Between `min` and `max` milliseconds ago.
    Ago { min: f64, max: f64 },
    /// From `from` to `to` (each open if `None`).
    Range { from: Option<Edge>, to: Option<Edge> },
}

/// One side of a [`Span::Range`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum Edge {
    /// This long ago (ms).
    Ago(f64),
    Start(Period),
    End(Period),
}

/// A local calendar period.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Period {
    /// Days since 1970-01-01.
    Day(i64),
    Month(i64, u32),
    Year(i64),
    /// The current day / week / month / year, or one `back` of them before it.
    Current(Unit, i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Day,
    Week,
    Month,
    Year,
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
    /// Current time in ms (for `idle:` and time filters), supplied by the caller.
    now: f64,
    /// Local time minus UTC, in minutes (for dates in time filters), supplied by the caller.
    utc_offset_min: f64,
}

/// A search over every entry of a state, as run by a page or by the worker (see
/// [`crate::worker`]). The same request on the same revision gives the same result.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SearchRequest {
    pub query: String,
    pub opts: SearchOptions,
    /// Where `is:duplicate` looks for duplicates; `None`: it matches nothing.
    pub duplicates: Option<Scope>,
    /// For `idle:` and time filters (0 when the query doesn't use them).
    pub now: f64,
    /// Local time minus UTC, in minutes (for dates in time filters). Optional: UTC if missing.
    #[serde(default)]
    pub utc_offset_min: f64,
    /// The revision of the state searched.
    pub revision: u64,
}

/// The ids of the entries (of any session) that match.
pub fn matching_ids(state: &AppState, req: &SearchRequest) -> Result<HashSet<Id>, String> {
    let names = state.workspaces.iter().map(|w| (w.id, w.name.clone())).collect();
    let playing = state.media.playing.iter().map(|p| p.tab_id).collect();
    let mut query = Query::parse(&req.query, &req.opts)?.with_context(names, playing, req.now).with_utc_offset(req.utc_offset_min);
    if let Some(scope) = req.duplicates
        && query.wants_duplicates()
    {
        query = query.with_duplicates(duplicate_ids(state, scope, &state.settings.dedupe));
    }
    Ok(state.sessions.iter().flat_map(|s| &s.entries).filter(|e| query.matches(e)).map(|e| e.id).collect())
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
                if let Some(which) = time_field(prefix) {
                    let span = parse_span(rest).map_err(|e| format!("{prefix}:{rest}: {e}"))?;
                    terms.push(Term { field, matcher: Matcher::Time(which, span), negate });
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

    /// Local time minus UTC, in minutes: where days start for dates in time filters.
    pub fn with_utc_offset(mut self, minutes: f64) -> Self {
        self.utc_offset_min = minutes;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.groups.is_empty() && self.workspaces.is_empty()
    }

    /// Whether the result depends on the current time (`idle:`, time filters).
    pub fn uses_time(&self) -> bool {
        self.terms.iter().any(|t| matches!(t.matcher, Matcher::Idle(_) | Matcher::Time(..)))
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
        if let Matcher::Time(which, span) = t.matcher {
            let at = e.time_of(which);
            let (from, to) = span.resolve(self.now, self.utc_offset_min * MINUTE);
            return self.now > 0.0 && at > 0.0 && from <= at && at < to;
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
            Matcher::Flag(_) | Matcher::Idle(_) | Matcher::Time(..) => false,
        }
    }
}

const MINUTE: f64 = 60_000.0;
const DAY: f64 = 86_400_000.0;

/// The entry time a `field:` prefix filters on.
fn time_field(prefix: &str) -> Option<TabSort> {
    Some(match prefix.to_ascii_lowercase().as_str() {
        "used" | "accessed" => TabSort::LastUsed,
        "added" | "created" | "first" => TabSort::Added,
        "opened" => TabSort::Opened,
        "closed" => TabSort::Closed,
        "loaded" => TabSort::Loaded,
        "unloaded" => TabSort::Unloaded,
        _ => return None,
    })
}

const SPAN_HELP: &str = "use e.g. 2h, <1d, >1w, 1d..1w, today, lastweek, 2026-11-01 or 2026-11-01..2026-11-05";

/// The value of a time filter (see the module docs).
fn parse_span(s: &str) -> Result<Span, String> {
    let s = s.trim().to_ascii_lowercase();
    let edge = |side: &str, start: bool| -> Result<Option<Edge>, String> {
        if side.is_empty() {
            return Ok(None);
        }
        if let Some(p) = parse_period(side)? {
            return Ok(Some(if start { Edge::Start(p) } else { Edge::End(p) }));
        }
        parse_duration(side).map(|ms| Some(Edge::Ago(ms))).ok_or_else(|| SPAN_HELP.to_string())
    };
    if let Some((a, b)) = s.split_once("..") {
        if a.ends_with('.') || b.starts_with('.') {
            return Err(SPAN_HELP.into());
        }
        let (from, to) = (edge(a, true)?, edge(b, false)?);
        // Durations count back from now: `1d..1w` means from a week ago to a day ago, and
        // `1w..` more than a week ago.
        return Ok(match (from, to) {
            (None, None) => return Err(SPAN_HELP.into()),
            (Some(Edge::Ago(x)), Some(Edge::Ago(y))) => Span::Ago { min: x.min(y), max: x.max(y) },
            (Some(Edge::Ago(x)), None) => Span::Ago { min: x, max: f64::INFINITY },
            (None, Some(Edge::Ago(y))) => Span::Ago { min: 0.0, max: y },
            (from, to) => Span::Range { from, to },
        });
    }
    let (op, rest) = ["<=", ">=", "<", ">"].iter().find_map(|op| s.strip_prefix(op).map(|r| (*op, r))).unwrap_or(("", &s));
    if let Some(p) = parse_period(rest)? {
        return Ok(match op {
            "<" => Span::Range { from: None, to: Some(Edge::Start(p)) },
            "<=" => Span::Range { from: None, to: Some(Edge::End(p)) },
            ">" => Span::Range { from: Some(Edge::End(p)), to: None },
            ">=" => Span::Range { from: Some(Edge::Start(p)), to: None },
            _ => Span::Range { from: Some(Edge::Start(p)), to: Some(Edge::End(p)) },
        });
    }
    let ms = parse_duration(rest).ok_or_else(|| SPAN_HELP.to_string())?;
    Ok(match op {
        ">" | ">=" => Span::Ago { min: ms, max: f64::INFINITY },
        _ => Span::Ago { min: 0.0, max: ms },
    })
}

/// `today`, `lastweek`..., `2026-11-01`, `2026-11` or `2026`. `Ok(None)`: not a period.
fn parse_period(s: &str) -> Result<Option<Period>, String> {
    let current = |unit, back| Ok(Some(Period::Current(unit, back)));
    match s {
        "today" => return current(Unit::Day, 0),
        "yesterday" => return current(Unit::Day, 1),
        "week" | "thisweek" => return current(Unit::Week, 0),
        "lastweek" => return current(Unit::Week, 1),
        "month" | "thismonth" => return current(Unit::Month, 0),
        "lastmonth" => return current(Unit::Month, 1),
        "year" | "thisyear" => return current(Unit::Year, 0),
        "lastyear" => return current(Unit::Year, 1),
        _ => {}
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts[0].len() != 4 || parts.len() > 3 || !parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return Ok(None);
    }
    let bad = || format!("{s} is not a date (use 2026-11-01, 2026-11 or 2026)");
    let year: i64 = parts[0].parse().map_err(|_| bad())?;
    let Some(month) = parts.get(1) else { return Ok(Some(Period::Year(year))) };
    let month: u32 = month.parse().ok().filter(|m| (1..=12).contains(m)).ok_or_else(bad)?;
    let Some(day) = parts.get(2) else { return Ok(Some(Period::Month(year, month))) };
    let day: u32 = day.parse().ok().filter(|d| *d >= 1 && *d <= days_in_month(year, month)).ok_or_else(bad)?;
    Ok(Some(Period::Day(days_from_civil(year, month, day))))
}

impl Span {
    /// The times (ms since epoch) it covers: `from <= t < to`. `offset`: local time minus UTC (ms).
    fn resolve(self, now: f64, offset: f64) -> (f64, f64) {
        match self {
            Span::Ago { min, max } => (now - max, now - min + 1.0),
            Span::Range { from, to } => {
                let at = |e: Option<Edge>, open: f64| match e {
                    None => open,
                    Some(Edge::Ago(ms)) => now - ms,
                    Some(Edge::Start(p)) => p.bounds(now, offset).0,
                    Some(Edge::End(p)) => p.bounds(now, offset).1,
                };
                let (a, b) = (at(from, f64::NEG_INFINITY), at(to, f64::INFINITY));
                // `2026-11-05..2026-11-01` means the same days as `2026-11-01..2026-11-05`.
                match (from, to) {
                    (Some(Edge::Start(p)), Some(Edge::End(q))) if a >= b => (q.bounds(now, offset).0, p.bounds(now, offset).1),
                    _ => (a, b),
                }
            }
        }
    }
}

impl Period {
    /// Its start and end (exclusive), ms since epoch. `offset`: local time minus UTC (ms).
    fn bounds(self, now: f64, offset: f64) -> (f64, f64) {
        let (first, next) = match self {
            Period::Day(d) => (d, d + 1),
            Period::Month(y, m) => (days_from_civil(y, m, 1), next_month(y, m)),
            Period::Year(y) => (days_from_civil(y, 1, 1), days_from_civil(y + 1, 1, 1)),
            Period::Current(unit, back) => {
                // Clamped (about ±270,000 years) so a bogus time can't overflow the date math.
                let today = ((now + offset) / DAY).floor().clamp(-1e8, 1e8) as i64;
                match unit {
                    Unit::Day => (today - back, today - back + 1),
                    Unit::Week => {
                        // 1970-01-01 was a Thursday, 3 days after a Monday.
                        let monday = today - (today + 3).rem_euclid(7) - 7 * back;
                        (monday, monday + 7)
                    }
                    Unit::Month => {
                        let (y, m, _) = civil_from_days(today);
                        let months = y * 12 + m as i64 - 1 - back;
                        let (y, m) = (months.div_euclid(12), months.rem_euclid(12) as u32 + 1);
                        (days_from_civil(y, m, 1), next_month(y, m))
                    }
                    Unit::Year => {
                        let y = civil_from_days(today).0 - back;
                        (days_from_civil(y, 1, 1), days_from_civil(y + 1, 1, 1))
                    }
                }
            }
        };
        (first as f64 * DAY - offset, next as f64 * DAY - offset)
    }
}

fn next_month(y: i64, m: u32) -> i64 {
    if m == 12 { days_from_civil(y + 1, 1, 1) } else { days_from_civil(y, m + 1, 1) }
}

fn days_in_month(y: i64, m: u32) -> u32 {
    (next_month(y, m) - days_from_civil(y, m, 1)) as u32
}

/// Days since 1970-01-01 of a (proleptic Gregorian) date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day since 1970-01-01: (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// `90s`, `30m`, `2h`, `1d`, `2w`, `3mo`, `1y` (a bare number means minutes) -> milliseconds.
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
        "mo" | "month" | "months" => 30.0 * DAY,
        "y" | "year" | "years" => 365.0 * DAY,
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
        mode => Matcher::Text { mode, needle: if case_sensitive { value.to_string() } else { value.to_lowercase() } },
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
        let e = TabEntry { id: 1, tab_id: Some(9), discarded: true, workspace: Some(5), last_accessed: 1_000.0, ..Default::default() };
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
    fn calendar_math() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for d in [-800_000, -1, 0, 59, 11_016, 20_764, 3_000_000] {
            let (y, m, day) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, day), d);
        }
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    #[test]
    fn time_filters() {
        const H: f64 = 3_600_000.0;
        // Saturday 2026-11-07, 12:00 UTC.
        let now = days_from_civil(2026, 11, 7) as f64 * DAY + 12.0 * H;
        let at = |y, m, d, h: f64| days_from_civil(y, m, d) as f64 * DAY + h * H;
        let m = |query: &str, e: &TabEntry, offset_min: f64| {
            q(query).with_context(HashMap::new(), HashSet::new(), now).with_utc_offset(offset_min).matches(e)
        };
        let opened = |t: f64| TabEntry { opened_at: t, ..Default::default() };

        // Relative.
        let e = opened(now - 3.0 * H);
        assert!(m("opened:4h", &e, 0.0) && m("opened:<4h", &e, 0.0));
        assert!(!m("opened:2h", &e, 0.0));
        assert!(m("opened:>2h", &e, 0.0) && !m("opened:>1d", &e, 0.0));
        assert!(m("opened:2h..4h", &e, 0.0) && m("opened:4h..2h", &e, 0.0) && !m("opened:4h..1d", &e, 0.0));
        assert!(m("opened:2h..", &e, 0.0) && !m("opened:..2h", &e, 0.0));
        assert!(!m("-opened:4h", &e, 0.0));

        // Calendar periods, in UTC and in UTC+14 (already Sunday there).
        assert!(m("opened:today", &e, 0.0) && !m("opened:yesterday", &e, 0.0));
        assert!(m("opened:yesterday", &opened(at(2026, 11, 6, 23.0)), 0.0));
        assert!(m("opened:week", &opened(at(2026, 11, 2, 0.0)), 0.0), "Monday starts the week");
        assert!(!m("opened:week", &opened(at(2026, 11, 1, 23.0)), 0.0));
        assert!(m("opened:lastweek", &opened(at(2026, 11, 1, 23.0)), 0.0));
        assert!(m("opened:month", &opened(at(2026, 11, 1, 0.0)), 0.0) && m("opened:lastmonth", &opened(at(2026, 10, 31, 0.0)), 0.0));
        assert!(m("opened:year", &opened(at(2026, 1, 1, 0.0)), 0.0) && m("opened:lastyear", &opened(at(2025, 12, 31, 0.0)), 0.0));
        assert!(m("opened:yesterday", &e, 14.0 * 60.0), "Saturday 09:00 is yesterday in UTC+14");

        // Dates.
        let nov3 = opened(at(2026, 11, 3, 10.0));
        assert!(m("opened:2026-11-03", &nov3, 0.0) && !m("opened:2026-11-04", &nov3, 0.0));
        assert!(m("opened:2026-11", &nov3, 0.0) && m("opened:2026", &nov3, 0.0) && !m("opened:2025", &nov3, 0.0));
        assert!(m("opened:<2026-11-04", &nov3, 0.0) && !m("opened:<2026-11-03", &nov3, 0.0) && m("opened:<=2026-11-03", &nov3, 0.0));
        assert!(m("opened:>2026-11-02", &nov3, 0.0) && !m("opened:>2026-11-03", &nov3, 0.0) && m("opened:>=2026-11-03", &nov3, 0.0));
        assert!(m("opened:2026-11-01..2026-11-03", &nov3, 0.0) && m("opened:2026-11-05..2026-11-03", &nov3, 0.0));
        assert!(!m("opened:2026-11-04..2026-11-05", &nov3, 0.0));
        assert!(m("opened:2026-11..", &nov3, 0.0) && m("opened:..lastweek", &opened(at(2026, 10, 1, 0.0)), 0.0));
        assert!(m("opened:2026-10-01..1d", &nov3, 0.0) && !m("opened:2026-10-01..5d", &nov3, 0.0));

        // Each field reads its own time; a missing time never matches.
        let e = TabEntry { last_accessed: now, created_at: now, closed_at: now, loaded_at: now, unloaded_at: now, ..Default::default() };
        for f in ["used", "accessed", "added", "created", "first", "closed", "loaded", "unloaded"] {
            assert!(m(&format!("{f}:1h"), &e, 0.0), "{f}");
        }
        assert!(!m("opened:100y", &e, 0.0) && !m("opened:<2100", &e, 0.0));
        assert!(!q("opened:1h").matches(&e), "no time supplied");
        assert!(q("opened:1h").uses_time());
    }

    #[test]
    fn bad_time_filters_are_errors_not_crashes() {
        let bad = [
            "opened:",
            "opened:soon",
            "opened:..",
            "opened:<",
            "opened:>=",
            "opened:2026-13",
            "opened:2026-02-30",
            "opened:2026-00-01",
            "opened:2026-1-",
            "opened:2026--01",
            "opened:2026-11-01-02",
            "opened:1e999x",
            "opened:-5h",
            "opened:<<1h",
            "opened:2026-99999999999999999999",
            "opened:1d...1w",
            "used:é",
            "closed:🙂",
            "added:2026-11-01..soon",
        ];
        for s in bad {
            assert!(Query::parse(s, &SearchOptions::default()).is_err(), "{s} should be an error");
        }
        // Odd but valid values, and absurd clocks and offsets: no panic, sensible results.
        let e = TabEntry { opened_at: 1.0, ..Default::default() };
        for s in ["opened:0m", "opened:99999999999999999999y", "opened:0000", "opened:9999-12-31", "opened:9999..", "opened:1d..1d"] {
            let query = Query::parse(s, &SearchOptions::default()).unwrap_or_else(|err| panic!("{s}: {err}"));
            for now in [1.0, 1e15, 1e300, f64::MAX, f64::INFINITY, f64::NAN, -1e300] {
                for offset in [0.0, -840.0, 1e300, f64::NAN] {
                    let _ = query.clone().with_context(HashMap::new(), HashSet::new(), now).with_utc_offset(offset).matches(&e);
                }
            }
        }
    }

    #[test]
    fn errors() {
        assert!(Query::parse("/(/", &SearchOptions::default()).is_err());
        assert!(Query::parse("is:bogus", &SearchOptions::default()).is_err());
    }
}
