//! Ad providers: which ad networks a page runs, from the scripts, frames, requests and ad
//! slots found in its frames (collected by the inspector) and the tabs it opened. Hosts are
//! matched against a catalogue of known networks; unknown third-party hosts whose name
//! looks like an ad server are listed as possible ad hosts.

use std::collections::{BTreeMap, BTreeSet};

use crate::url;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AdCategory {
    /// Pop-ups, pop-unders, redirects and "click anywhere" ads.
    Popunder,
    /// Banners and display ads (ad servers, AdSense).
    Display,
    /// "Recommended for you" content widgets.
    Native,
    Video,
    /// Programmatic exchanges and header bidding (they pick which ad shows).
    Exchange,
    /// Ad verification and measurement.
    Verification,
    /// Unknown hosts whose names look like ad servers.
    Suspected,
}

impl AdCategory {
    pub fn label(self) -> &'static str {
        match self {
            AdCategory::Popunder => "Pop-ups & pop-unders",
            AdCategory::Display => "Display & banners",
            AdCategory::Native => "Native / recommended content",
            AdCategory::Video => "Video ads",
            AdCategory::Exchange => "Exchange & header bidding",
            AdCategory::Verification => "Verification & measurement",
            AdCategory::Suspected => "Possible ad hosts",
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Provider {
    pub name: &'static str,
    pub category: AdCategory,
    /// Hosts (with their subdomains).
    pub domains: &'static [&'static str],
}

const fn p(name: &'static str, category: AdCategory, domains: &'static [&'static str]) -> Provider {
    Provider { name, category, domains }
}

use AdCategory::*;

pub const PROVIDERS: &[Provider] = &[
    // Pop-ups, pop-unders and redirect networks.
    p("PropellerAds / Monetag", Popunder, &["propellerads.com", "propellerclick.com", "onclickads.net", "onclkds.com", "monetag.com"]),
    p("PopAds", Popunder, &["popads.net"]),
    p("PopCash", Popunder, &["popcash.net"]),
    p("Adsterra", Popunder, &["adsterra.com"]),
    p("ExoClick", Popunder, &["exoclick.com", "exosrv.com", "exdynsrv.com"]),
    p("JuicyAds", Popunder, &["juicyads.com"]),
    p("TrafficJunky", Popunder, &["trafficjunky.com", "trafficjunky.net"]),
    p("TrafficStars", Popunder, &["trafficstars.com", "tsyndicate.com"]),
    p("HilltopAds", Popunder, &["hilltopads.net", "hilltopads.com"]),
    p("Clickadu", Popunder, &["clickadu.com"]),
    p("AdMaven", Popunder, &["ad-maven.com"]),
    p("Galaksion", Popunder, &["galaksion.com"]),
    p("RichAds", Popunder, &["richads.com"]),
    p("Clickaine", Popunder, &["clickaine.com"]),
    p("PopMyAds", Popunder, &["popmyads.com"]),
    p("AdCash", Popunder, &["adcash.com"]),
    p("Zeropark", Popunder, &["zeropark.com"]),
    p("EroAdvertising", Popunder, &["ero-advertising.com"]),
    // Display.
    p("Google Ads (AdSense, Ad Manager, DoubleClick)", Display, &[
        "doubleclick.net", "googlesyndication.com", "googleadservices.com", "googletagservices.com", "adservice.google.com", "2mdn.net",
    ]),
    p("Amazon Ads", Display, &["amazon-adsystem.com"]),
    p("Media.net", Display, &["media.net"]),
    p("Infolinks", Display, &["infolinks.com"]),
    p("Ezoic", Display, &["ezoic.net", "ezodn.com", "ezojs.com"]),
    p("Mediavine", Display, &["mediavine.com"]),
    p("Raptive (AdThrive)", Display, &["adthrive.com"]),
    p("Freestar", Display, &["pub.network", "freestar.com"]),
    p("Yandex Advertising", Display, &["an.yandex.ru", "yandexadexchange.net"]),
    p("Adform", Display, &["adform.net"]),
    p("Smart AdServer (Equativ)", Display, &["smartadserver.com"]),
    p("A-ADS", Display, &["a-ads.com"]),
    p("Coinzilla", Display, &["coinzilla.com", "coinzilla.io"]),
    p("Bitmedia", Display, &["bitmedia.io"]),
    p("GumGum", Display, &["gumgum.com"]),
    p("Kargo", Display, &["kargo.com"]),
    // Native.
    p("Taboola", Native, &["taboola.com"]),
    p("Outbrain", Native, &["outbrain.com", "outbrainimg.com"]),
    p("MGID", Native, &["mgid.com"]),
    p("Revcontent", Native, &["revcontent.com"]),
    p("Content.ad", Native, &["content.ad"]),
    // Video.
    p("Google IMA (video ads)", Video, &["imasdk.googleapis.com"]),
    p("FreeWheel", Video, &["fwmrm.net", "freewheel.tv", "stickyadstv.com"]),
    p("SpotX (Magnite video)", Video, &["spotxchange.com", "spotx.tv"]),
    p("Teads", Video, &["teads.tv"]),
    p("Aniview", Video, &["aniview.com"]),
    p("Primis", Video, &["primis.tech"]),
    p("Unruly", Video, &["unrulymedia.com"]),
    p("Innovid", Video, &["innovid.com"]),
    // Exchanges and demand-side platforms.
    p("Xandr (AppNexus)", Exchange, &["adnxs.com"]),
    p("Magnite (Rubicon Project)", Exchange, &["rubiconproject.com"]),
    p("PubMatic", Exchange, &["pubmatic.com"]),
    p("OpenX", Exchange, &["openx.net"]),
    p("Index Exchange", Exchange, &["casalemedia.com", "indexww.com"]),
    p("Criteo", Exchange, &["criteo.com", "criteo.net"]),
    p("Sovrn", Exchange, &["lijit.com", "sovrn.com"]),
    p("TripleLift", Exchange, &["3lift.com"]),
    p("Sharethrough", Exchange, &["sharethrough.com"]),
    p("Yieldmo", Exchange, &["yieldmo.com"]),
    p("Sonobi", Exchange, &["sonobi.com"]),
    p("Yahoo SSP", Exchange, &["ybp.yahoo.com", "ads.yahoo.com"]),
    p("The Trade Desk", Exchange, &["adsrvr.org"]),
    p("MediaMath", Exchange, &["mathtag.com"]),
    // Verification.
    p("Integral Ad Science", Verification, &["adsafeprotected.com"]),
    p("DoubleVerify", Verification, &["doubleverify.com"]),
    p("Moat (Oracle)", Verification, &["moatads.com"]),
];

/// Script paths that give away an ad library, wherever it is served from.
const SCRIPT_HINTS: &[(&str, &str, AdCategory)] = &[
    ("prebid", "Prebid.js (header bidding)", Exchange),
    ("popunder", "Pop-under script", Popunder),
    ("adsbygoogle", "Google Ads (AdSense, Ad Manager, DoubleClick)", Display),
];

/// Elements that hold ads, and whose. Given to the collector in each frame.
pub const SLOT_SELECTORS: &[(&str, &str)] = &[
    ("ins.adsbygoogle", "Google Ads (AdSense, Ad Manager, DoubleClick)"),
    ("[id^='div-gpt-ad'], [id^='google_ads_iframe']", "Google Ads (AdSense, Ad Manager, DoubleClick)"),
    ("[id^='taboola-'], .trc_rbox_container", "Taboola"),
    (".OUTBRAIN, [data-widget-id^='AR_']", "Outbrain"),
    ("[id^='mgid_'], [data-type='_mgwidget']", "MGID"),
    ("[id^='amzn-assoc-ad'], [id^='amzn_assoc']", "Amazon Ads"),
    ("[id^='ezoic-pub-ad']", "Ezoic"),
];

/// Host labels that name ad servers ("ads.example.net", "adserver-3.cdn.io").
const SUSPECT_TOKENS: &[&str] = &[
    "ad", "ads", "adserver", "adservers", "adsrv", "adservice", "adx", "adnet", "adnetwork", "advert", "adverts", "advertising",
    "banner", "banners", "popunder", "popunders", "popup", "popups", "pop", "pops",
];

/// The catalogue entry for a host, if it belongs to a known ad network.
pub fn provider_for_host(host: &str) -> Option<&'static Provider> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    PROVIDERS.iter().find(|p| p.domains.iter().any(|d| url::host_within(&host, d)))
}

fn provider_named(name: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.name == name)
}

/// A host whose labels name an ad server.
pub fn suspect_host(host: &str) -> bool {
    host.split(['.', '-']).any(|label| {
        let l = label.trim_end_matches(|c: char| c.is_ascii_digit()).to_ascii_lowercase();
        SUSPECT_TOKENS.contains(&l.as_str())
    })
}

/// Something found on a page: a script, frame, request or ad slot (`kind`), or a tab the
/// page opened (`"popup"`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdSignal {
    pub kind: String,
    pub url: String,
    /// Slots: the selector that matched, then " × count". Others: extra detail.
    pub detail: String,
    /// The frame it was found in.
    pub frame: String,
}

/// An ad provider seen on a page, with what gave it away.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderHit {
    pub name: String,
    pub category: AdCategory,
    /// In the catalogue (otherwise a suspected host, named after it).
    pub known: bool,
    /// Registrable domains it was seen on (what a blocking rule should name).
    pub domains: BTreeSet<String>,
    /// Signals, by URL (deduplicated).
    pub signals: Vec<AdSignal>,
}

impl ProviderHit {
    /// "3 scripts · 1 frame · 12 requests".
    pub fn summary(&self) -> String {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &self.signals {
            *counts.entry(s.kind.as_str()).or_default() += 1;
        }
        let order = ["popup", "slot", "script", "iframe", "frame", "request"];
        let mut keys: Vec<&str> = counts.keys().copied().collect();
        keys.sort_by_key(|k| order.iter().position(|o| o == k).unwrap_or(order.len()));
        keys.iter()
            .map(|k| {
                let n = counts[k];
                let (one, many) = match *k {
                    "popup" => ("tab opened", "tabs opened"),
                    "slot" => ("ad slot", "ad slots"),
                    "script" => ("script", "scripts"),
                    "iframe" | "frame" => ("frame", "frames"),
                    _ => ("request", "requests"),
                };
                format!("{n} {}", if n == 1 { one } else { many })
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Groups a page's signals by ad provider, most telling first (pop-up networks, then by
/// the number of signals). Hosts of the page's own site are never suspected.
pub fn providers(page_url: &str, signals: &[AdSignal]) -> Vec<ProviderHit> {
    let site = url::registrable_domain(&url::host(page_url));
    let mut hits: BTreeMap<String, ProviderHit> = BTreeMap::new();
    let mut seen: BTreeSet<(String, String, String)> = BTreeSet::new();
    for s in signals {
        if !seen.insert((s.kind.clone(), s.url.clone(), s.detail.clone())) {
            continue;
        }
        let host = url::host(&s.url);
        let domain = url::registrable_domain(&host);
        let (name, category, known) = if s.kind == "slot" {
            let name = s.detail.split(" × ").next().and_then(|sel| SLOT_SELECTORS.iter().find(|(q, _)| *q == sel)).map(|(_, n)| *n);
            match name.and_then(provider_named) {
                Some(p) => (p.name.to_string(), p.category, true),
                None => continue,
            }
        } else if let Some(p) = provider_for_host(&host) {
            (p.name.to_string(), p.category, true)
        } else if let Some((_, name, cat)) =
            SCRIPT_HINTS.iter().find(|(hint, _, _)| s.kind == "script" && path_of(&s.url).to_ascii_lowercase().contains(hint))
        {
            (name.to_string(), *cat, true)
        } else if !host.is_empty() && domain != site && suspect_host(&host) {
            (domain.clone(), Suspected, false)
        } else if s.kind == "popup" && !host.is_empty() && domain != site {
            // A tab the page opened to another site: an ad until shown otherwise.
            (domain.clone(), Popunder, false)
        } else {
            continue;
        };
        let hit = hits.entry(name.clone()).or_insert_with(|| ProviderHit {
            name,
            category,
            known,
            domains: BTreeSet::new(),
            signals: Vec::new(),
        });
        if !domain.is_empty() && domain != site {
            hit.domains.insert(domain);
        }
        hit.signals.push(s.clone());
    }
    let mut out: Vec<ProviderHit> = hits.into_values().collect();
    out.sort_by(|a, b| {
        let popups = |h: &ProviderHit| h.signals.iter().any(|s| s.kind == "popup");
        popups(b)
            .cmp(&popups(a))
            .then(a.known.cmp(&b.known).reverse())
            .then(a.category.cmp(&b.category))
            .then(b.signals.len().cmp(&a.signals.len()))
            .then(a.name.cmp(&b.name))
    });
    out
}

fn path_of(u: &str) -> &str {
    let rest = u.split_once("://").map_or(u, |(_, r)| r);
    let path = rest.find('/').map_or("", |i| &rest[i..]);
    path.split(['?', '#']).next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(kind: &str, url: &str) -> AdSignal {
        AdSignal { kind: kind.into(), url: url.into(), ..Default::default() }
    }

    #[test]
    fn known_hosts() {
        assert_eq!(provider_for_host("pagead2.googlesyndication.com").unwrap().category, Display);
        assert_eq!(provider_for_host("c.adsco.re"), None);
        assert_eq!(provider_for_host("syndication.exosrv.com").unwrap().name, "ExoClick");
        assert!(provider_for_host("notdoubleclick.net").is_none());
        assert!(provider_for_host("media.net").is_some() && provider_for_host("social.media.network").is_none());
    }

    #[test]
    fn suspects() {
        assert!(suspect_host("ads.example.net"));
        assert!(suspect_host("adserver-3.cdn.io"));
        assert!(suspect_host("pop2.example.com"));
        assert!(!suspect_host("adobe.com") && !suspect_host("roads.example.com") && !suspect_host("downloads.org"));
    }

    #[test]
    fn grouping() {
        let page = "https://www.movies.example/watch/1";
        let signals = vec![
            sig("script", "https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js"),
            sig("request", "https://securepubads.g.doubleclick.net/gampad/ads?x=1"),
            sig("request", "https://securepubads.g.doubleclick.net/gampad/ads?x=1"),
            sig("script", "https://cdn.movies.example/js/prebid8.js"),
            sig("script", "https://ads.movies.example/a.js"),
            sig("script", "https://ads.shady.net/pu.js"),
            sig("popup", "https://landing.offerz.biz/?id=9"),
            sig("script", "https://cdn.jsdelivr.net/npm/hls.js"),
            AdSignal { kind: "slot".into(), detail: "ins.adsbygoogle × 3".into(), ..Default::default() },
            AdSignal { kind: "slot".into(), detail: "unknown × 1".into(), ..Default::default() },
        ];
        let hits = providers(page, &signals);
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["offerz.biz", "Google Ads (AdSense, Ad Manager, DoubleClick)", "Prebid.js (header bidding)", "shady.net"]);
        let google = &hits[1];
        assert_eq!(google.signals.len(), 3);
        assert_eq!(google.domains.iter().cloned().collect::<Vec<_>>(), vec!["doubleclick.net", "googlesyndication.com"]);
        assert_eq!(google.summary(), "1 ad slot · 1 script · 1 request");
        assert_eq!(hits[0].category, Popunder);
        assert_eq!(hits[0].summary(), "1 tab opened");
        // The page's own ad-looking subdomain is not suspected; a third-party one is.
        assert_eq!(hits[3].category, Suspected);
        assert!(hits[2].domains.is_empty());
    }
}
