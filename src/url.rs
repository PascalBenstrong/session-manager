//! Small, dependency-free URL helpers (host / registrable domain extraction).

/// Returns the URL scheme (lowercased, without `:`), or an empty string.
pub fn scheme(url: &str) -> String {
    match url.find(':') {
        Some(i) if url[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) => url[..i].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Extracts the lowercased host of a URL (no userinfo, no port).
/// Returns an empty string when the URL has no authority part (e.g. `about:blank`).
pub fn host(url: &str) -> String {
    let Some(start) = url.find("://") else {
        return String::new();
    };
    let rest = &url[start + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let mut authority = &rest[..end];
    if let Some(at) = authority.rfind('@') {
        authority = &authority[at + 1..];
    }
    let host = if authority.starts_with('[') {
        // IPv6 literal: keep the brackets' content.
        match authority.find(']') {
            Some(close) => &authority[1..close],
            None => authority,
        }
    } else {
        match authority.rfind(':') {
            Some(colon) => &authority[..colon],
            None => authority,
        }
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Second-level labels that act as public suffixes under a country TLD
/// (e.g. `co.uk`, `com.au`). This is a heuristic, not the full Public Suffix List.
const MULTI_LABEL_SUFFIXES: &[&str] = &[
    "ac", "co", "com", "edu", "gov", "gob", "net", "org", "or", "ne", "go", "mil", "nic", "ltd", "plc", "sch", "nhs", "police", "nom",
    "web", "info", "biz",
];

/// Approximates the registrable domain ("eTLD+1") of a host:
/// `www.example.co.uk` -> `example.co.uk`, `a.b.example.com` -> `example.com`.
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim_end_matches('.');
    if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') {
        return host.to_string();
    }
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n >= 3 && labels[n - 1].len() == 2 && MULTI_LABEL_SUFFIXES.contains(&labels[n - 2]) {
        return labels[n - 3..].join(".");
    }
    labels[n - 2..].join(".")
}

/// Host of a URL, or a descriptive pseudo-host for URLs without one (`file`, `about`, ...).
pub fn display_host(url: &str) -> String {
    let h = host(url);
    if h.is_empty() { scheme(url) } else { h }
}

/// True for `host` itself and any subdomain of `parent`.
pub fn host_within(host: &str, parent: &str) -> bool {
    host == parent || (host.len() > parent.len() && host.ends_with(parent) && host.as_bytes()[host.len() - parent.len() - 1] == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host("https://user:pw@Www.Example.com:8080/a?b#c"), "www.example.com");
        assert_eq!(host("http://[::1]:3000/"), "::1");
        assert_eq!(host("about:blank"), "");
        assert_eq!(host("file:///C:/x.txt"), "");
        assert_eq!(display_host("file:///C:/x.txt"), "file");
        assert_eq!(scheme("HTTPS://x"), "https");
    }

    #[test]
    fn domains() {
        assert_eq!(registrable_domain("www.example.com"), "example.com");
        assert_eq!(registrable_domain("a.b.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("example.com"), "example.com");
        assert_eq!(registrable_domain("localhost"), "localhost");
        assert_eq!(registrable_domain("192.168.1.1"), "192.168.1.1");
    }

    #[test]
    fn within() {
        assert!(host_within("a.example.com", "example.com"));
        assert!(host_within("example.com", "example.com"));
        assert!(!host_within("badexample.com", "example.com"));
    }
}
