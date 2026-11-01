//! Site patterns used by auto-organize rules and cleanup rules, plus shared
//! glob/regex helpers.

use regex_lite::Regex;
use serde::{Deserialize, Serialize};

use crate::url::host_within;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PatternKind {
    /// Exact host, e.g. `www.example.com`.
    Host,
    /// A domain and all of its subdomains, e.g. `example.com`.
    #[default]
    Domain,
    /// URL starts with the value, e.g. `https://example.com/app/`.
    UrlPrefix,
    /// Wildcards `*` and `?`. Matched against the URL if the value contains `/` or `:`,
    /// otherwise against the host (e.g. `*.example.*`).
    Glob,
    /// Regular expression matched against the full URL.
    Regex,
}

impl PatternKind {
    pub const ALL: [PatternKind; 5] =
        [PatternKind::Host, PatternKind::Domain, PatternKind::UrlPrefix, PatternKind::Glob, PatternKind::Regex];

    pub fn label(self) -> &'static str {
        match self {
            PatternKind::Host => "Host",
            PatternKind::Domain => "Domain (+subdomains)",
            PatternKind::UrlPrefix => "URL prefix",
            PatternKind::Glob => "Glob",
            PatternKind::Regex => "Regex",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct SitePattern {
    pub kind: PatternKind,
    pub value: String,
}

impl SitePattern {
    pub fn new(kind: PatternKind, value: impl Into<String>) -> Self {
        Self { kind, value: value.into() }
    }

    pub fn compile(&self) -> Result<CompiledPattern, String> {
        let value = self.value.trim();
        if value.is_empty() {
            return Err("pattern is empty".into());
        }
        Ok(match self.kind {
            PatternKind::Host => CompiledPattern::Host(value.to_ascii_lowercase()),
            PatternKind::Domain => CompiledPattern::Domain(value.trim_start_matches("*.").trim_start_matches('.').to_ascii_lowercase()),
            PatternKind::UrlPrefix => CompiledPattern::UrlPrefix(value.to_ascii_lowercase()),
            PatternKind::Glob => {
                let on_url = value.contains('/') || value.contains(':');
                CompiledPattern::Glob { re: glob_to_regex(value, false)?, on_url }
            }
            PatternKind::Regex => CompiledPattern::Regex(Regex::new(value).map_err(|e| e.to_string())?),
        })
    }

    pub fn describe(&self) -> String {
        format!("{}: {}", self.kind.label(), self.value)
    }
}

#[derive(Debug, Clone)]
pub enum CompiledPattern {
    Host(String),
    Domain(String),
    UrlPrefix(String),
    Glob { re: Regex, on_url: bool },
    Regex(Regex),
}

impl CompiledPattern {
    /// `host` must be lowercase. `url` may be empty when only a host is known.
    pub fn matches(&self, host: &str, url: &str) -> bool {
        match self {
            CompiledPattern::Host(h) => host == h,
            CompiledPattern::Domain(d) => host_within(host, d),
            CompiledPattern::UrlPrefix(p) => url.to_ascii_lowercase().starts_with(p.as_str()),
            CompiledPattern::Glob { re, on_url } => re.is_match(if *on_url { url } else { host }),
            CompiledPattern::Regex(re) => re.is_match(url) || (url.is_empty() && re.is_match(host)),
        }
    }

    /// For host-based patterns, the host they name (used to target site data removal).
    pub fn literal_host(&self) -> Option<&str> {
        match self {
            CompiledPattern::Host(h) | CompiledPattern::Domain(h) => Some(h),
            _ => None,
        }
    }
}

/// Converts a glob (`*`, `?`) into an anchored, case-insensitive (unless `case_sensitive`) regex.
pub fn glob_to_regex(glob: &str, case_sensitive: bool) -> Result<Regex, String> {
    let mut re = String::with_capacity(glob.len() + 8);
    if !case_sensitive {
        re.push_str("(?i)");
    }
    re.push('^');
    for c in glob.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => re.push_str(&regex_lite::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(kind: PatternKind, v: &str, host: &str, url: &str) -> bool {
        SitePattern::new(kind, v).compile().unwrap().matches(host, url)
    }

    #[test]
    fn kinds() {
        assert!(m(PatternKind::Host, "www.example.com", "www.example.com", ""));
        assert!(!m(PatternKind::Host, "example.com", "www.example.com", ""));
        assert!(m(PatternKind::Domain, "example.com", "www.example.com", ""));
        assert!(m(PatternKind::Domain, "*.example.com", "example.com", ""));
        assert!(!m(PatternKind::Domain, "example.com", "notexample.com", ""));
        assert!(m(PatternKind::UrlPrefix, "https://a.com/x", "a.com", "https://a.com/x/y"));
        assert!(m(PatternKind::Glob, "*.google.*", "mail.google.com", ""));
        assert!(m(PatternKind::Glob, "https://*/login*", "a.com", "https://a.com/login?x"));
        assert!(m(PatternKind::Regex, r"ads?\.", "ad.x.com", "https://ad.x.com/"));
    }

    #[test]
    fn bad_patterns() {
        assert!(SitePattern::new(PatternKind::Regex, "(").compile().is_err());
        assert!(SitePattern::new(PatternKind::Host, "  ").compile().is_err());
    }
}
