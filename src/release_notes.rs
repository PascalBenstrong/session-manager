//! Release notes, compiled in from `CHANGELOG.md` and shown on the "What's new" page.
//!
//! Format: `## [version] - date` per release, `### Section` headings, `- item` bullets
//! (an indented line continues the previous bullet).

pub const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// The extension version (kept equal to the manifests and the latest changelog entry by tests).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Release {
    pub version: String,
    pub date: String,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Section {
    pub title: String,
    pub items: Vec<String>,
}

pub fn releases() -> Vec<Release> {
    parse(CHANGELOG)
}

pub fn parse(markdown: &str) -> Vec<Release> {
    let mut out: Vec<Release> = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(head) = trimmed.strip_prefix("## ") {
            let (version, date) = match head.split_once(" - ") {
                Some((v, d)) => (v, d.trim()),
                None => (head, ""),
            };
            out.push(Release {
                version: version.trim().trim_start_matches('[').trim_end_matches(']').to_string(),
                date: date.to_string(),
                sections: Vec::new(),
            });
        } else if let Some(title) = trimmed.strip_prefix("### ") {
            if let Some(r) = out.last_mut() {
                r.sections.push(Section { title: title.trim().to_string(), items: Vec::new() });
            }
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            if let Some(r) = out.last_mut() {
                if r.sections.is_empty() {
                    r.sections.push(Section::default());
                }
                r.sections.last_mut().expect("pushed").items.push(item.trim().to_string());
            }
        } else if !trimmed.is_empty() && line.starts_with(' ') {
            // Continuation of a wrapped bullet.
            if let Some(item) = out.last_mut().and_then(|r| r.sections.last_mut()).and_then(|s| s.items.last_mut()) {
                item.push(' ');
                item.push_str(trimmed);
            }
        }
    }
    out
}

/// `a > b` for dotted numeric versions (`0.10.0` > `0.9.1`). Non-numeric parts count as 0.
pub fn is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| v.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (mut a, mut b) = (parse(a), parse(b));
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

/// Splits text on backticks: `(segment, is_code)` pairs, for rendering `inline code`.
pub fn inline_code_segments(text: &str) -> Vec<(&str, bool)> {
    text.split('`').enumerate().filter(|(_, s)| !s.is_empty()).map(|(i, s)| (s, i % 2 == 1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markdown() {
        let md = "# Title\nintro\n\n## [1.2.0] - 2026-01-02\n\n### New\n- first\n- second\n  continued\n### Fixed\n* bug\n\n## 1.0.0\n- loose item\n";
        let r = parse(md);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].version.as_str(), r[0].date.as_str()), ("1.2.0", "2026-01-02"));
        assert_eq!(r[0].sections[0].items, vec!["first", "second continued"]);
        assert_eq!(r[0].sections[1].title, "Fixed");
        assert_eq!(r[1].version, "1.0.0");
        assert_eq!(r[1].sections[0].items, vec!["loose item"]);
    }

    #[test]
    fn versions() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.1"));
        assert!(!is_newer("0.2.0", "0.2"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.2.0", ""));
    }

    #[test]
    fn inline_code() {
        assert_eq!(inline_code_segments("use `is:dup` now"), vec![("use ", false), ("is:dup", true), (" now", false)]);
    }

    /// The bundled changelog, Cargo.toml and both manifests must agree on the version.
    #[test]
    fn changelog_matches_versions() {
        let r = releases();
        assert!(!r.is_empty() && r[0].sections.iter().all(|s| !s.items.is_empty()), "changelog has content");
        assert_eq!(r[0].version, VERSION, "latest CHANGELOG.md entry must match Cargo.toml");
        for (name, manifest) in [
            ("firefox", include_str!("../firefox/manifest.json")),
            ("chrome", include_str!("../chrome/manifest.json")),
        ] {
            let m: serde_json::Value = serde_json::from_str(manifest).unwrap();
            assert_eq!(m["version"].as_str(), Some(VERSION), "{name}/manifest.json version");
        }
    }
}
