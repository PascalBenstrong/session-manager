//! Page guards: rules that block ad scripts and ad tabs, and page scripts (run in the
//! page's own world, before its scripts) that stop pages from blocking right-click, from
//! trapping the developer tools with `debugger`, and from detecting them. This module plans
//! what to register with the browser; the guard scripts live in `web/guards/`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{AppState, Id};
use crate::pattern::{CompiledPattern, PatternKind, SitePattern};
use crate::url;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct GuardSettings {
    /// Block pop-ups and new tabs that pages open without a click, or from handlers that
    /// catch clicks anywhere on the page (how pop-under ad scripts work).
    pub popups: bool,
    /// Block pop-ups from clicks on video players and on invisible layers over the page
    /// ("press play, get an ad"), and switch off the layer so the click reaches the player.
    pub click_hijack: bool,
    /// Pages can't block the context menu (right-click).
    pub context_menu: bool,
    /// Pages can't block selecting and copying text either.
    pub text_selection: bool,
    /// Remove `debugger` statements that pages build at runtime (developer tools traps).
    pub anti_debugger: bool,
    /// Firefox: also remove `debugger` statements from script files as they load.
    pub strip_debugger_scripts: bool,
    /// Hide the developer tools from pages that look for them.
    pub devtools_stealth: bool,
    /// Silence the page's console (detection tricks go through it).
    pub silence_console: bool,
    /// Sites (domains, with their subdomains) where no guard runs.
    pub exceptions: Vec<String>,
}

impl GuardSettings {
    pub fn any_page_guard(&self) -> bool {
        self.popups || self.click_hijack || self.context_menu || self.anti_debugger || self.devtools_stealth
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlockRuleKind {
    /// Block scripts whose URL matches `pattern` (optionally only on `sites`).
    #[default]
    Script,
    /// Close tabs that pages open to a site matching `pattern` (a domain or glob).
    PopupTo,
    /// Block every pop-up and new tab opened by script on `sites`.
    AllPopupsOn,
}

impl BlockRuleKind {
    pub const ALL: [BlockRuleKind; 3] = [BlockRuleKind::Script, BlockRuleKind::PopupTo, BlockRuleKind::AllPopupsOn];

    pub fn label(self) -> &'static str {
        match self {
            BlockRuleKind::Script => "Block script",
            BlockRuleKind::PopupTo => "Close tabs opened to",
            BlockRuleKind::AllPopupsOn => "Block all pop-ups on",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct BlockRule {
    pub id: Id,
    pub enabled: bool,
    pub kind: BlockRuleKind,
    /// Script: a URL filter (`ads.example.com`, `*popunder*.js`, `||cdn.x.com/a.js`,
    /// `/regex/`). PopupTo: a domain or glob. AllPopupsOn: unused.
    pub pattern: String,
    /// Script: only on these sites (empty: everywhere). AllPopupsOn: the sites.
    pub sites: Vec<String>,
}

impl BlockRule {
    pub fn describe(&self) -> String {
        let sites = self.sites.join(", ");
        match self.kind {
            BlockRuleKind::Script if sites.is_empty() => format!("Block script {}", self.pattern),
            BlockRuleKind::Script => format!("Block script {} on {sites}", self.pattern),
            BlockRuleKind::PopupTo => format!("Close tabs opened to {}", self.pattern),
            BlockRuleKind::AllPopupsOn => format!("Block all pop-ups on {sites}"),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let sites_ok = self.sites.iter().all(|s| valid_site(s));
        match self.kind {
            BlockRuleKind::Script => {
                url_filter(&self.pattern)?;
                if !sites_ok {
                    return Err("sites must be domains like example.com".into());
                }
            }
            BlockRuleKind::PopupTo => {
                popup_pattern(&self.pattern).compile()?;
            }
            BlockRuleKind::AllPopupsOn => {
                if self.sites.is_empty() || !sites_ok {
                    return Err("name the sites, like example.com".into());
                }
            }
        }
        Ok(())
    }
}

/// A tab a page opened (newest first in the log).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct PopupEvent {
    pub at: f64,
    /// The page that opened it.
    pub opener: String,
    pub url: String,
    /// Closed by a rule (its id).
    pub closed_by: Option<Id>,
}

pub const MAX_POPUP_LOG: usize = 30;

/// `example.com`, `sub.example.co.uk`.
pub fn valid_site(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && s.contains('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// Normalizes a site typed by the user ("https://www.example.com/x" -> "example.com").
pub fn normalize_site(s: &str) -> String {
    let host = if s.contains("://") { url::host(s) } else { s.split('/').next().unwrap_or("").to_lowercase() };
    host.trim().trim_start_matches("*.").trim_start_matches("www.").to_string()
}

/// A declarativeNetRequest URL condition for a script pattern: `("regexFilter", ..)` for
/// `/regex/`, otherwise `("urlFilter", ..)`. A bare host blocks everything from it.
pub fn url_filter(pattern: &str) -> Result<(&'static str, String), String> {
    let p = pattern.trim();
    if p.is_empty() {
        return Err("the rule needs a pattern".into());
    }
    if p.len() > 2 && p.starts_with('/') && p.ends_with('/') {
        let re = &p[1..p.len() - 1];
        regex_lite::Regex::new(re).map_err(|e| format!("invalid regex: {e}"))?;
        return Ok(("regexFilter", re.to_string()));
    }
    if p.chars().any(char::is_whitespace) {
        return Err("patterns can't contain spaces".into());
    }
    let bare_host = !p.contains(['/', '*', '|', '^', '?', ':']) && valid_site(p);
    Ok(("urlFilter", if bare_host { format!("||{p}^") } else { p.to_string() }))
}

fn popup_pattern(p: &str) -> SitePattern {
    let p = p.trim();
    let kind = if p.contains(['*', '?']) { PatternKind::Glob } else { PatternKind::Domain };
    SitePattern::new(kind, if kind == PatternKind::Domain { normalize_site(p) } else { p.to_string() })
}

/// The enabled rule closing a tab opened to `tab_url`, if any.
pub fn popup_rule_for(rules: &[BlockRule], tab_url: &str) -> Option<Id> {
    let host = url::host(tab_url);
    if host.is_empty() {
        return None;
    }
    rules
        .iter()
        .filter(|r| r.enabled && r.kind == BlockRuleKind::PopupTo)
        .find(|r| popup_pattern(&r.pattern).compile().is_ok_and(|c: CompiledPattern| c.matches(&host, tab_url)))
        .map(|r| r.id)
}

/// declarativeNetRequest dynamic rules for the enabled script rules.
pub fn dnr_rules(rules: &[BlockRule]) -> Vec<Value> {
    rules
        .iter()
        .filter(|r| r.enabled && r.kind == BlockRuleKind::Script)
        .filter_map(|r| {
            let (key, value) = url_filter(&r.pattern).ok()?;
            let mut condition = json!({ "resourceTypes": ["script"] });
            condition[key] = Value::String(value);
            if !r.sites.is_empty() {
                condition["initiatorDomains"] = json!(r.sites.iter().map(|s| normalize_site(s)).collect::<Vec<_>>());
            }
            Some(json!({ "id": r.id, "priority": 1, "action": { "type": "block" }, "condition": condition }))
        })
        .collect()
}

impl AppState {
    /// Creates (`id == 0`) or updates a block rule. Sites are normalized. Returns its id.
    pub fn save_block_rule(&mut self, mut rule: BlockRule) -> Result<Id, String> {
        rule.pattern = rule.pattern.trim().to_string();
        rule.sites = rule.sites.iter().map(|s| normalize_site(s)).filter(|s| !s.is_empty()).collect();
        rule.validate()?;
        if rule.id == 0 {
            rule.id = self.alloc_id();
            let id = rule.id;
            self.block_rules.push(rule);
            return Ok(id);
        }
        let id = rule.id;
        *self.block_rules.iter_mut().find(|r| r.id == id).ok_or("no such rule")? = rule;
        Ok(id)
    }

    pub fn push_popup(&mut self, e: PopupEvent) {
        self.popup_log.insert(0, e);
        self.popup_log.truncate(MAX_POPUP_LOG);
    }
}

/// Match patterns for a site and its subdomains.
pub fn site_match_patterns(site: &str) -> Vec<String> {
    let site = normalize_site(site);
    if site.is_empty() {
        return Vec::new();
    }
    vec![format!("*://{site}/*"), format!("*://*.{site}/*")]
}

/// A content script to register in the page's own world.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScriptRegistration {
    pub id: &'static str,
    pub js: Vec<&'static str>,
    pub matches: Vec<String>,
    pub exclude_matches: Vec<String>,
    pub all_frames: bool,
    pub run_at: &'static str,
    pub world: &'static str,
}

/// Prefix of the ids of the registrations we make (others are left alone).
pub const REGISTRATION_PREFIX: &str = "sm-guard-";

/// The page guard scripts to register, from the settings and rules.
pub fn registrations(g: &GuardSettings, rules: &[BlockRule]) -> Vec<ScriptRegistration> {
    let exclude: Vec<String> = g.exceptions.iter().flat_map(|s| site_match_patterns(s)).collect();
    let all = || vec!["<all_urls>".to_string()];
    let reg = |id: &'static str, js: Vec<&'static str>, matches: Vec<String>| ScriptRegistration {
        id,
        js,
        matches,
        exclude_matches: exclude.clone(),
        all_frames: true,
        run_at: "document_start",
        world: "MAIN",
    };
    let mut out = Vec::new();
    if g.popups {
        out.push(reg("sm-guard-popups", vec!["guards/popups.js"], all()));
    }
    if g.click_hijack {
        out.push(reg("sm-guard-click-hijack", vec!["guards/click-hijack.js"], all()));
    }
    let strict: Vec<String> = rules
        .iter()
        .filter(|r| r.enabled && r.kind == BlockRuleKind::AllPopupsOn)
        .flat_map(|r| r.sites.iter().flat_map(|s| site_match_patterns(s)))
        .collect();
    if !strict.is_empty() {
        out.push(reg("sm-guard-popups-strict", vec!["guards/popups-strict.js"], strict));
    }
    if g.context_menu {
        let mut js = vec!["guards/context-menu.js"];
        if g.text_selection {
            js.push("guards/text-selection.js");
        }
        out.push(reg("sm-guard-context-menu", js, all()));
    }
    if g.anti_debugger {
        out.push(reg("sm-guard-debugger", vec!["guards/anti-debugger.js"], all()));
    }
    if g.devtools_stealth {
        let mut js = vec!["guards/devtools-stealth.js"];
        if g.silence_console {
            js.push("guards/silence-console.js");
        }
        out.push(reg("sm-guard-stealth", js, all()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: Id, kind: BlockRuleKind, pattern: &str, sites: &[&str]) -> BlockRule {
        BlockRule { id, enabled: true, kind, pattern: pattern.into(), sites: sites.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn script_filters() {
        assert_eq!(url_filter("ads.example.com").unwrap(), ("urlFilter", "||ads.example.com^".into()));
        assert_eq!(url_filter("*popunder*.js").unwrap(), ("urlFilter", "*popunder*.js".into()));
        assert_eq!(url_filter("/pop(under)?\\.js/").unwrap(), ("regexFilter", "pop(under)?\\.js".into()));
        assert!(url_filter("/(/").is_err());
        assert!(url_filter("a b").is_err());
        let rules = vec![
            rule(5, BlockRuleKind::Script, "ads.example.com", &[]),
            rule(6, BlockRuleKind::Script, "*pop.js", &["https://www.news.com/x"]),
            BlockRule { enabled: false, ..rule(7, BlockRuleKind::Script, "x.js", &[]) },
            rule(8, BlockRuleKind::PopupTo, "ads.com", &[]),
        ];
        let dnr = dnr_rules(&rules);
        assert_eq!(dnr.len(), 2);
        assert_eq!(dnr[0]["condition"]["urlFilter"], "||ads.example.com^");
        assert_eq!(dnr[0]["condition"]["resourceTypes"][0], "script");
        assert_eq!(dnr[1]["condition"]["initiatorDomains"][0], "news.com");
        assert_eq!(dnr[1]["id"], 6);
    }

    #[test]
    fn popup_rules() {
        let rules = vec![rule(1, BlockRuleKind::PopupTo, "adserver.net", &[]), rule(2, BlockRuleKind::PopupTo, "*://*/promo/*", &[])];
        assert_eq!(popup_rule_for(&rules, "https://x.adserver.net/click?id=1"), Some(1));
        assert_eq!(popup_rule_for(&rules, "https://shop.com/promo/1"), Some(2));
        assert_eq!(popup_rule_for(&rules, "https://example.com/"), None);
        assert_eq!(popup_rule_for(&rules, "about:blank"), None);
        assert!(rule(3, BlockRuleKind::AllPopupsOn, "", &[]).validate().is_err());
        assert!(rule(3, BlockRuleKind::AllPopupsOn, "", &["news.com"]).validate().is_ok());
    }

    #[test]
    fn registrations_follow_settings() {
        let mut g = GuardSettings { exceptions: vec!["docs.google.com".into()], ..Default::default() };
        assert!(registrations(&g, &[]).is_empty());
        g.context_menu = true;
        g.text_selection = true;
        g.devtools_stealth = true;
        g.click_hijack = true;
        let regs = registrations(&g, &[rule(1, BlockRuleKind::AllPopupsOn, "", &["www.bad.com"])]);
        let ids: Vec<_> = regs.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["sm-guard-click-hijack", "sm-guard-popups-strict", "sm-guard-context-menu", "sm-guard-stealth"]);
        assert_eq!(regs[1].matches, vec!["*://bad.com/*", "*://*.bad.com/*"]);
        assert_eq!(regs[2].js, vec!["guards/context-menu.js", "guards/text-selection.js"]);
        assert_eq!(regs[2].exclude_matches, vec!["*://docs.google.com/*", "*://*.docs.google.com/*"]);
        assert!(regs.iter().all(|r| r.world == "MAIN" && r.run_at == "document_start" && r.id.starts_with(REGISTRATION_PREFIX)));
    }

    #[test]
    fn saving_rules() {
        let mut s = AppState::default();
        let id = s.save_block_rule(rule(0, BlockRuleKind::Script, " *ad.js ", &["https://www.news.com/"])).unwrap();
        let saved = s.block_rules.iter().find(|r| r.id == id).unwrap();
        assert_eq!((saved.pattern.as_str(), saved.sites.clone()), ("*ad.js", vec!["news.com".to_string()]));
        assert!(s.save_block_rule(rule(0, BlockRuleKind::PopupTo, "", &[])).is_err());
        for i in 0..40 {
            s.push_popup(PopupEvent { at: i as f64, ..Default::default() });
        }
        assert_eq!((s.popup_log.len(), s.popup_log[0].at), (MAX_POPUP_LOG, 39.0));
    }

    #[test]
    fn sites() {
        assert_eq!(normalize_site("https://www.Example.com/path"), "example.com");
        assert_eq!(normalize_site("*.example.com"), "example.com");
        assert!(valid_site("example.co.uk") && !valid_site("localhost") && !valid_site("a b.com"));
    }
}
