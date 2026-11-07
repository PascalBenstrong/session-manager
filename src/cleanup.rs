//! Cookie / site-data cleanup: rules and the planning logic that turns a rule into
//! concrete cookie removals and a host list for the browser's browsingData API.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::model::Id;
use crate::pattern::{CompiledPattern, SitePattern};
use crate::url::{self, host_within};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DataTypes {
    pub cookies: bool,
    pub cache: bool,
    pub local_storage: bool,
    pub indexed_db: bool,
    pub service_workers: bool,
}

impl Default for DataTypes {
    fn default() -> Self {
        Self { cookies: true, cache: true, local_storage: true, indexed_db: true, service_workers: true }
    }
}

impl DataTypes {
    /// Any type other than cookies (those go through the browsingData API).
    pub fn has_site_data(&self) -> bool {
        self.cache || self.local_storage || self.indexed_db || self.service_workers
    }

    pub fn is_empty(&self) -> bool {
        !self.cookies && !self.has_site_data()
    }

    pub fn describe(&self) -> String {
        let mut v = Vec::new();
        if self.cookies {
            v.push("cookies");
        }
        if self.cache {
            v.push("cache");
        }
        if self.local_storage {
            v.push("local & session storage");
        }
        if self.indexed_db {
            v.push("IndexedDB");
        }
        if self.service_workers {
            v.push("service workers");
        }
        if v.is_empty() { "nothing".into() } else { v.join(", ") }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub enum CleanupTarget {
    /// Sites matching a host / domain / URL / glob / regex pattern.
    Pattern(SitePattern),
    /// Every site that currently has an open tab.
    OpenTabs,
    /// The sites of these pages (e.g. a single tab), matched like open tabs.
    Pages(Vec<String>),
    /// Every site.
    #[default]
    AllDomains,
}

impl CleanupTarget {
    pub fn describe(&self) -> String {
        match self {
            CleanupTarget::Pattern(p) => p.describe(),
            CleanupTarget::OpenTabs => "all open tabs".into(),
            CleanupTarget::Pages(urls) => hosts_of(urls).into_iter().collect::<Vec<_>>().join(", "),
            CleanupTarget::AllDomains => "all domains".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CleanupSpec {
    pub target: CleanupTarget,
    pub data_types: DataTypes,
    /// Sites never cleaned by this spec (e.g. keep your logins).
    pub exclude: Vec<SitePattern>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CleanupRule {
    pub id: Id,
    pub name: String,
    pub enabled: bool,
    pub spec: CleanupSpec,
    /// Run automatically every N minutes.
    pub interval_minutes: Option<u32>,
    /// Remove matching cookies as soon as a site sets them.
    pub on_cookie_set: bool,
    pub last_run: Option<f64>,
}

impl CleanupRule {
    pub fn alarm_name(&self) -> String {
        format!("{ALARM_PREFIX}{}", self.id)
    }
}

pub const ALARM_PREFIX: &str = "sm-cleanup-";

/// Cookie as reported by the platform layer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub store_id: Option<String>,
    pub partition_key: Option<serde_json::Value>,
}

impl Cookie {
    pub fn host(&self) -> String {
        self.domain.trim_start_matches('.').to_ascii_lowercase()
    }

    pub fn url(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        let path = if self.path.is_empty() { "/" } else { &self.path };
        format!("{scheme}://{}{path}", self.host())
    }

    /// For a partitioned cookie (set by a frame of another site, kept under the site of
    /// the tab): the host of that tab's site.
    pub fn top_level_host(&self) -> Option<String> {
        let site = self.partition_key.as_ref()?.get("topLevelSite")?.as_str()?;
        Some(url::host(site)).filter(|h| !h.is_empty())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CookieRemoval {
    pub url: String,
    pub name: String,
    pub store_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_key: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CleanupPlan {
    pub cookies: Vec<CookieRemoval>,
    /// Hosts whose non-cookie data to remove. `None` = all sites (no filter).
    /// `Some(empty)` = nothing to do.
    pub site_data_hosts: Option<Vec<String>>,
    pub data_types: DataTypes,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CleanupReport {
    pub at: f64,
    pub label: String,
    pub cookies_removed: usize,
    pub hosts: Vec<String>,
    pub all_sites: bool,
    pub warnings: Vec<String>,
    /// Open tabs whose storage was also cleared from inside the page.
    pub pages_cleared: usize,
    /// Open tabs reloaded afterwards.
    pub pages_reloaded: usize,
}

impl CleanupReport {
    pub fn summary(&self) -> String {
        let sites = if self.all_sites { "all sites".to_string() } else { format!("{} site(s)", self.hosts.len()) };
        let mut s = format!("{}: {} cookie(s) removed, site data cleared for {}", self.label, self.cookies_removed, sites);
        match (self.pages_cleared, self.pages_reloaded) {
            (0, 0) => {}
            (n, 0) => s.push_str(&format!(", and inside {n} open tab(s)")),
            (n, r) => s.push_str(&format!(", and inside {n} open tab(s); {r} reloaded")),
        }
        s
    }
}

/// A cleanup of a whole site: the registrable domain of `page_url` and all its subdomains
/// (`www.shop.example.co.uk` -> `example.co.uk`), like "this site" in the browser.
pub fn site_spec(page_url: &str, data_types: DataTypes) -> Result<(String, CleanupSpec), String> {
    if !is_web_page(page_url) {
        return Err("this page has no site data to clean".into());
    }
    let site = url::registrable_domain(&url::host(page_url));
    let spec = CleanupSpec {
        target: CleanupTarget::Pattern(SitePattern::new(crate::pattern::PatternKind::Domain, &site)),
        data_types,
        exclude: Vec::new(),
    };
    Ok((site, spec))
}

/// Whether the page can hold site data we can clear from inside it (web pages only).
pub fn is_web_page(page_url: &str) -> bool {
    matches!(url::scheme(page_url).as_str(), "http" | "https") && !url::host(page_url).is_empty()
}

/// Matcher built from a spec, reusable for single-cookie checks (on-set trigger).
pub struct SpecMatcher {
    target: Target,
    exclude: Vec<CompiledPattern>,
}

enum Target {
    Pattern(CompiledPattern),
    Hosts(BTreeSet<String>),
    All,
}

impl SpecMatcher {
    pub fn new(spec: &CleanupSpec, open_tab_urls: &[String]) -> Result<Self, String> {
        let target = match &spec.target {
            CleanupTarget::Pattern(p) => Target::Pattern(p.compile()?),
            CleanupTarget::OpenTabs => Target::Hosts(hosts_of(open_tab_urls)),
            CleanupTarget::Pages(urls) => {
                let hosts = hosts_of(urls);
                if hosts.is_empty() {
                    return Err("this page has no site data to clean".into());
                }
                Target::Hosts(hosts)
            }
            CleanupTarget::AllDomains => Target::All,
        };
        let exclude = spec.exclude.iter().map(|p| p.compile()).collect::<Result<Vec<_>, _>>()?;
        Ok(Self { target, exclude })
    }

    /// Whether data for `host` (cookie domain or site host) is targeted.
    pub fn matches(&self, host: &str, url: &str) -> bool {
        let hit = match &self.target {
            Target::Pattern(p) => p.matches(host, url),
            // A cookie on `example.com` applies to an open `www.example.com` tab too.
            Target::Hosts(hosts) => hosts.iter().any(|open| host_within(open, host)),
            Target::All => true,
        };
        hit && !self.exclude.iter().any(|p| p.matches(host, url))
    }

    /// A cookie is targeted by its own domain, or, when partitioned, by the site it was
    /// set under (third-party cookies of a site go with it). Exclusions apply to both.
    pub fn matches_cookie(&self, c: &Cookie) -> bool {
        let host = c.host();
        if self.matches(&host, &c.url()) {
            return true;
        }
        let Some(top) = c.top_level_host() else { return false };
        self.matches(&top, &format!("https://{top}/")) && !self.exclude.iter().any(|p| p.matches(&host, &c.url()))
    }

    /// Whether an open page belongs to the targeted sites (its storage can be cleared
    /// from inside it too).
    pub fn covers_page(&self, page_url: &str) -> bool {
        is_web_page(page_url) && self.matches(&url::host(page_url), page_url)
    }

    fn is_unfiltered(&self) -> bool {
        matches!(self.target, Target::All) && self.exclude.is_empty()
    }
}

fn hosts_of(urls: &[String]) -> BTreeSet<String> {
    urls.iter().map(|u| url::host(u)).filter(|h| !h.is_empty()).collect()
}

/// Builds a cleanup plan.
/// * `cookies` – all cookies in the browser.
/// * `open_tab_urls` – URLs of open tabs.
/// * `known_hosts` – other hosts we know about (session entries), used as candidates for
///   site-data removal since the browser can only remove that data for explicit hosts.
pub fn plan(spec: &CleanupSpec, cookies: &[Cookie], open_tab_urls: &[String], known_hosts: &[String]) -> Result<CleanupPlan, String> {
    if spec.data_types.is_empty() {
        return Err("no data types selected".into());
    }
    let m = SpecMatcher::new(spec, open_tab_urls)?;

    let cookie_removals = if spec.data_types.cookies {
        cookies
            .iter()
            .filter(|c| m.matches_cookie(c))
            .map(|c| CookieRemoval {
                url: c.url(),
                name: c.name.clone(),
                store_id: c.store_id.clone(),
                partition_key: c.partition_key.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };

    let site_data_hosts = if !spec.data_types.has_site_data() {
        Some(Vec::new())
    } else if m.is_unfiltered() {
        None
    } else if let Target::Hosts(open) = &m.target {
        // Storage is per origin: only the open sites themselves, not their parent domains.
        Some(open.iter().filter(|h| !m.exclude.iter().any(|p| p.matches(h, ""))).cloned().collect())
    } else {
        let mut candidates: BTreeSet<String> = BTreeSet::new();
        candidates.extend(cookies.iter().map(Cookie::host));
        candidates.extend(open_tab_urls.iter().map(|u| url::host(u)));
        candidates.extend(known_hosts.iter().cloned());
        if let Target::Pattern(p) = &m.target
            && let Some(h) = p.literal_host()
        {
            candidates.insert(h.to_string());
        }
        let hosts = candidates
            .into_iter()
            .filter(|h| !h.is_empty())
            .filter(|h| {
                let probe = format!("https://{h}/");
                m.matches(h, &probe) || m.matches(h, &format!("http://{h}/"))
            })
            .collect();
        Some(hosts)
    };

    Ok(CleanupPlan { cookies: cookie_removals, site_data_hosts, data_types: spec.data_types })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::PatternKind;

    fn cookie(name: &str, domain: &str) -> Cookie {
        Cookie { name: name.into(), domain: domain.into(), path: "/".into(), secure: true, ..Default::default() }
    }

    fn cookies() -> Vec<Cookie> {
        vec![cookie("a", ".example.com"), cookie("b", "www.example.com"), cookie("c", "other.org")]
    }

    #[test]
    fn pattern_target() {
        let spec =
            CleanupSpec { target: CleanupTarget::Pattern(SitePattern::new(PatternKind::Domain, "example.com")), ..Default::default() };
        let p = plan(&spec, &cookies(), &[], &["docs.example.com".into()]).unwrap();
        assert_eq!(p.cookies.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(p.cookies[0].url, "https://example.com/");
        assert_eq!(p.site_data_hosts.unwrap(), vec!["docs.example.com".to_string(), "example.com".into(), "www.example.com".into()]);
    }

    #[test]
    fn open_tabs_target() {
        let spec = CleanupSpec { target: CleanupTarget::OpenTabs, ..Default::default() };
        let p = plan(&spec, &cookies(), &["https://www.example.com/x".into()], &[]).unwrap();
        // Parent-domain cookie applies to the open tab too.
        assert_eq!(p.cookies.len(), 2);
        assert_eq!(p.site_data_hosts.unwrap(), vec!["www.example.com".to_string()]);
    }

    #[test]
    fn pages_target() {
        let spec = CleanupSpec { target: CleanupTarget::Pages(vec!["https://www.example.com/login".into()]), ..Default::default() };
        assert_eq!(spec.target.describe(), "www.example.com");
        // Open tabs elsewhere don't widen the target.
        let p = plan(&spec, &cookies(), &["https://other.org/".into()], &[]).unwrap();
        assert_eq!(p.cookies.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(p.site_data_hosts.unwrap(), vec!["www.example.com".to_string()]);

        let internal = CleanupSpec { target: CleanupTarget::Pages(vec!["about:blank".into()]), ..Default::default() };
        assert!(plan(&internal, &cookies(), &[], &[]).is_err());
    }

    #[test]
    fn all_domains_with_and_without_exclusions() {
        let spec = CleanupSpec::default();
        let p = plan(&spec, &cookies(), &[], &[]).unwrap();
        assert_eq!(p.cookies.len(), 3);
        assert_eq!(p.site_data_hosts, None);

        let spec = CleanupSpec { exclude: vec![SitePattern::new(PatternKind::Domain, "example.com")], ..Default::default() };
        let p = plan(&spec, &cookies(), &[], &[]).unwrap();
        assert_eq!(p.cookies.len(), 1);
        assert_eq!(p.site_data_hosts.unwrap(), vec!["other.org".to_string()]);
    }

    #[test]
    fn whole_site() {
        let (site, spec) = site_spec("https://www.shop.example.co.uk/cart?x=1", DataTypes::default()).unwrap();
        assert_eq!(site, "example.co.uk");
        let cookies = vec![
            cookie("a", ".example.co.uk"),
            cookie("b", "m.example.co.uk"),
            cookie("c", "other.org"),
            cookie("d", "notexample.co.uk"),
            // Set by an ad frame while on the site: kept under the site's partition.
            Cookie { partition_key: Some(serde_json::json!({ "topLevelSite": "https://example.co.uk" })), ..cookie("e", "ads.net") },
            Cookie { partition_key: Some(serde_json::json!({ "topLevelSite": "https://other.org" })), ..cookie("f", "ads.net") },
        ];
        let p = plan(&spec, &cookies, &["https://login.example.co.uk/".into()], &["www.shop.example.co.uk".into()]).unwrap();
        assert_eq!(p.cookies.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["a", "b", "e"]);
        assert_eq!(
            p.site_data_hosts.unwrap(),
            vec!["example.co.uk".to_string(), "login.example.co.uk".into(), "m.example.co.uk".into(), "www.shop.example.co.uk".into()]
        );

        let m = SpecMatcher::new(&spec, &[]).unwrap();
        assert!(m.covers_page("https://www.shop.example.co.uk/"));
        assert!(m.covers_page("http://example.co.uk/a"));
        assert!(!m.covers_page("https://other.org/"));
        assert!(!m.covers_page("about:blank"));

        for bad in ["about:blank", "chrome://settings", "moz-extension://x/popup.html", "file:///C:/a.html", "", "not a url", "https://"] {
            assert!(site_spec(bad, DataTypes::default()).is_err(), "{bad}");
        }
        assert_eq!(site_spec("http://localhost:3000/", DataTypes::default()).unwrap().0, "localhost");
        assert_eq!(site_spec("http://192.168.1.4/", DataTypes::default()).unwrap().0, "192.168.1.4");
    }

    #[test]
    fn partitioned_cookies_respect_exclusions_and_odd_keys() {
        let spec = CleanupSpec {
            target: CleanupTarget::Pattern(SitePattern::new(PatternKind::Domain, "example.com")),
            exclude: vec![SitePattern::new(PatternKind::Domain, "keep.net")],
            ..Default::default()
        };
        let m = SpecMatcher::new(&spec, &[]).unwrap();
        let part = |domain: &str, key: serde_json::Value| Cookie { partition_key: Some(key), ..cookie("x", domain) };
        assert!(m.matches_cookie(&part("ads.net", serde_json::json!({ "topLevelSite": "https://example.com" }))));
        assert!(!m.matches_cookie(&part("keep.net", serde_json::json!({ "topLevelSite": "https://example.com" }))));
        // Partition keys we don't understand never match (and never fail).
        for key in [serde_json::json!({}), serde_json::json!(null), serde_json::json!("x"), serde_json::json!({ "topLevelSite": 5 })] {
            assert!(!m.matches_cookie(&part("ads.net", key)));
        }
    }

    #[test]
    fn cookies_only() {
        let spec = CleanupSpec {
            data_types: DataTypes { cookies: true, cache: false, local_storage: false, indexed_db: false, service_workers: false },
            ..Default::default()
        };
        let p = plan(&spec, &cookies(), &[], &[]).unwrap();
        assert_eq!(p.site_data_hosts, Some(vec![]));
    }
}
