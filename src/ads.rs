//! Ad providers: which ad networks a page runs, from the scripts, frames, requests and ad
//! slots found in its frames (collected by the inspector), the ad tags written into its
//! code (inline scripts, encoded payloads) and the tabs it opened. Hosts are matched
//! against a catalogue of known networks; hosts that serve ad tags but do not name
//! themselves (self-hosted pop-under tags, banner iframes) are listed as possible ad hosts.

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
    p("PropellerAds / Monetag", Popunder, &[
        "propellerads.com", "propellerclick.com", "onclickads.net", "onclkds.com", "monetag.com", "llvpn.com", "libtl.com",
    ]),
    p("PopAds", Popunder, &[
        "popads.net", "popadscdn.net", "displayvertising.com", "predictivadnetwork.com", "predictivadnetworks.com",
    ]),
    p("PopCash", Popunder, &["popcash.net"]),
    p("Adsterra", Popunder, &[
        "adsterra.com", "highperformanceformat.com", "effectivecreativeformat.com", "effectivecreativeformats.com",
    ]),
    p("ExoClick", Popunder, &[
        "exoclick.com", "exosrv.com", "exdynsrv.com", "exacdn.com", "opoxv.com", "pemsrv.com", "smartfloralab.com",
    ]),
    p("JuicyAds", Popunder, &["juicyads.com"]),
    p("TrafficJunky", Popunder, &["trafficjunky.com", "trafficjunky.net"]),
    p("TrafficStars", Popunder, &["trafficstars.com", "tsyndicate.com"]),
    p("HilltopAds", Popunder, &["hilltopads.net", "hilltopads.com", "humbleparticular.com"]),
    p("Clickadu", Popunder, &["clickadu.com"]),
    p("AdMaven", Popunder, &["ad-maven.com", "admaven.com"]),
    p("Galaksion", Popunder, &["galaksion.com"]),
    p("RichAds", Popunder, &["richads.com", "rmdnr.com"]),
    p("Clickaine", Popunder, &["clickaine.com"]),
    p("PopMyAds", Popunder, &["popmyads.com"]),
    p("OnClickA", Popunder, &["onclicka.com", "onclckvd.com", "onclickalgo.com"]),
    p("AdCash", Popunder, &["adcash.com", "acscdn.com", "adexchangerapid.com"]),
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
    p("AdWheel", Display, &["animeapp.org", "adwheel.com"]),
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

/// URL paths and queries that only one network's ad tag uses, wherever it is served from.
const URL_HINTS: &[(&str, &str)] = &[
    ("/popunder1000.js", "ExoClick"), // PopMagic, ExoClick's pop-under tag
    ("/jup/next.php", "PopAds"),
    ("rzqfd=", "AdMaven"), // its zone parameter, also on CloudFront hosts
    ("zgiqd=", "AdMaven"),
    ("ag_custom_domain=", "AdWheel"),
    ("/repeat_banner/", "AdWheel"),
    ("/aclib.js", "AdCash"),
    ("/suv5.js", "AdCash"),
    ("/tag.min.js", "PropellerAds / Monetag"),
    ("/ptag.min.js", "PropellerAds / Monetag"),
];

/// Networks whose names appear in the code a page carries (inline tags, encoded ad code).
const INLINE_HINTS: &[(&str, &str)] = &[
    ("monetag", "PropellerAds / Monetag"),
    ("propellerads", "PropellerAds / Monetag"),
    ("llvpn.com", "PropellerAds / Monetag"),
    ("admaven", "AdMaven"),
    ("ad-maven", "AdMaven"),
    ("rzqfd", "AdMaven"),
    ("zgiqd", "AdMaven"),
    ("adsterra", "Adsterra"),
    ("highperformanceformat", "Adsterra"),
    ("effectivecreativeformat", "Adsterra"),
    ("hilltopads", "HilltopAds"),
    ("humbleparticular", "HilltopAds"),
    ("popads", "PopAds"),
    ("predictivadnetwork", "PopAds"),
    ("omegatoki", "PopAds"),
    ("exoclick", "ExoClick"),
    ("popmagic", "ExoClick"),
    ("exacdn", "ExoClick"),
    ("adwheel", "AdWheel"),
    ("animeapp.org", "AdWheel"),
    ("onclicka", "OnClickA"),
    ("onclckvd", "OnClickA"),
    ("richads", "RichAds"),
    ("adcash", "AdCash"),
    ("acscdn", "AdCash"),
    ("trafficstars", "TrafficStars"),
    ("tsyndicate", "TrafficStars"),
    ("juicyads", "JuicyAds"),
    ("popcash", "PopCash"),
    ("galaksion", "Galaksion"),
    ("clickadu", "Clickadu"),
    ("mgid", "MGID"),
    ("taboola", "Taboola"),
    ("outbrain", "Outbrain"),
    ("revcontent", "Revcontent"),
    ("aniview", "Aniview"),
    ("primis", "Primis"),
    ("teads", "Teads"),
    ("spotx", "SpotX (Magnite video)"),
    ("adnxs", "Xandr (AppNexus)"),
    ("rubiconproject", "Magnite (Rubicon Project)"),
    ("pubmatic", "PubMatic"),
    ("openx.net", "OpenX"),
    ("casalemedia", "Index Exchange"),
    ("criteo", "Criteo"),
    ("adsbygoogle", "Google Ads (AdSense, Ad Manager, DoubleClick)"),
    ("doubleclick", "Google Ads (AdSense, Ad Manager, DoubleClick)"),
    ("googlesyndication", "Google Ads (AdSense, Ad Manager, DoubleClick)"),
    ("adsafeprotected", "Integral Ad Science"),
    ("moatads", "Moat (Oracle)"),
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

/// Shared hosts where one ad tag's domain says nothing about the next one: blocking the
/// registrable domain would hit unrelated sites, so a rule must name the host itself.
const SHARED_HOSTS: &[&str] = &[
    "cloudfront.net", "akamaized.net", "fastly.net", "azureedge.net", "edgekey.net", "amazonaws.com",
    "workers.dev", "pages.dev", "netlify.app", "vercel.app", "cloudflare.net", "googleusercontent.com",
];

/// The catalogue entry for a host, if it belongs to a known ad network.
pub fn provider_for_host(host: &str) -> Option<&'static Provider> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    PROVIDERS.iter().find(|p| p.domains.iter().any(|d| url::host_within(&host, d)))
}

/// The catalogue entry for a URL: its host, or a path or query only that network's ad tag uses.
pub fn provider_for_url(u: &str) -> Option<&'static Provider> {
    provider_for_host(&url::host(u)).or_else(|| {
        let lower = u.to_ascii_lowercase();
        URL_HINTS.iter().find(|(needle, _)| lower.contains(needle)).and_then(|(_, name)| provider_named(name))
    })
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

/// What a blocking rule should name for a host: its registrable domain, or the host itself
/// on shared hosts (a `*.cloudfront.net` tag must not block every CloudFront site).
fn blockable_domain(host: &str) -> String {
    let domain = url::registrable_domain(host);
    if SHARED_HOSTS.iter().any(|s| url::host_within(&domain, s)) { host.to_string() } else { domain }
}

/// A third-party URL that serves an ad tag without naming a network: the `/<token>/<zone>`
/// shape of self-hosted pop-under tags, or a tag library's own file name.
fn is_ad_tag(u: &str) -> bool {
    let path = path_of(u);
    let lower = path.to_ascii_lowercase();
    if ["/tag.min.js", "/ptag.min.js", "/popunder1000.js", "/invoke.js"].iter().any(|f| lower.ends_with(f)) {
        return true;
    }
    if lower.ends_with(".js") && lower.contains("popunder") {
        return true;
    }
    // The zone part is not lowercased: a random token is mixed case (r5wZdI0boL5xio/155474).
    let segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    let (token, zone) = match segments[..] {
        [token, zone] => (token, zone),
        _ => return false,
    };
    let mixed = token.chars().any(|c| c.is_ascii_uppercase())
        && token.chars().filter(|c| c.is_ascii_lowercase()).count() >= 2
        && token.chars().any(|c| c.is_ascii_digit());
    (8..=24).contains(&token.len())
        && token.chars().all(|c| c.is_ascii_alphanumeric())
        && mixed
        && (4..=9).contains(&zone.len())
        && zone.chars().all(|c| c.is_ascii_digit())
}

/// A third-party frame that an ad server filled (`/iframe/<hex>`, a repeated banner).
fn is_ad_frame(u: &str) -> bool {
    let path = path_of(u).to_ascii_lowercase();
    if path.contains("/repeat_banner/") || path.contains("/adframe/") || path.contains("/ad_frame/") {
        return true;
    }
    let segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    matches!(segments[..], [first, id] if first == "iframe"
        && (8..=32).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_hexdigit()))
}

/// An ad URL that does not name a network: a self-hosted tag, an ad server's frame, a
/// redirect (`/go/1234567?subid=…`, how direct-link ads arrive) or a video ad request.
fn ad_url_shape(u: &str) -> Option<AdCategory> {
    if is_ad_tag(u) {
        return Some(Popunder);
    }
    if is_ad_frame(u) {
        return Some(Display);
    }
    let lower = u.to_ascii_lowercase();
    let path = path_of(&lower);
    // A picture or font called "vast-something" is not a video ad request.
    let extension = path.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let asset = matches!(extension, "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "css" | "woff" | "woff2" | "ttf" | "ico");
    let words = || path.split(|c: char| !c.is_ascii_alphanumeric());
    if !asset && words().any(|w| matches!(w, "vast" | "vpaid" | "preroll" | "midroll" | "adrule" | "adtag")) {
        return Some(Video);
    }
    if let Some((_, query)) = lower.split_once('?')
        && query.split('&').any(|kv| matches!(kv.split('=').next(), Some("subid" | "subid1" | "subid2" | "subid3" | "subid4")))
    {
        return Some(Popunder);
    }
    let segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    if let [first, id] = segments[..]
        && first == "go"
        && (4..=10).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_digit())
    {
        return Some(Popunder);
    }
    None
}

/// Something found on a page: a script, frame, request or ad slot (`kind`), a piece of page
/// code that carries an ad tag (`"inline"`), or a tab the page opened (`"popup"`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdSignal {
    pub kind: String,
    pub url: String,
    /// Slots: the selector that matched, then " × count". Inline code: the code itself.
    /// Others: extra detail.
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
    /// The domains it was seen on (what a blocking rule should name): the registrable
    /// domain, or the host itself on a shared CDN.
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
        let order = ["popup", "slot", "script", "inline", "iframe", "frame", "request"];
        let mut keys: Vec<&str> = counts.keys().copied().collect();
        keys.sort_by_key(|k| order.iter().position(|o| o == k).unwrap_or(order.len()));
        keys.iter()
            .map(|k| {
                let n = counts[k];
                let (one, many) = match *k {
                    "popup" => ("tab opened", "tabs opened"),
                    "slot" => ("ad slot", "ad slots"),
                    "script" => ("script", "scripts"),
                    "inline" => ("inline tag", "inline tags"),
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
        // Ad tags written into the page's code: the networks it names, and the tag hosts
        // and frames it builds (also after decoding an encoded payload).
        if s.kind == "inline" {
            for (name, domains) in inline_matches(&s.detail, &site) {
                let category = provider_named(name).map_or(Suspected, |p| p.category);
                let hit = hit(&mut hits, name.to_string(), category, true);
                hit.domains.extend(domains);
                hit.signals.push(s.clone());
            }
            for (name, category, domains) in inline_tags(&s.detail, &site) {
                let hit = hit(&mut hits, name, category, false);
                hit.domains.extend(domains);
                hit.signals.push(s.clone());
            }
            continue;
        }
        let host = url::host(&s.url);
        let domain = blockable_domain(&host);
        let third_party = !domain.is_empty() && domain != site;
        let found = if s.kind == "slot" {
            s.detail
                .split(" × ")
                .next()
                .and_then(|selector| SLOT_SELECTORS.iter().find(|(q, _)| *q == selector))
                .and_then(|(_, name)| provider_named(name))
                .map(|p| (p.name.to_string(), p.category, true, None))
        } else if let Some(p) = provider_for_host(&host) {
            Some((p.name.to_string(), p.category, true, Some(domain.clone())))
        } else if third_party && let Some(p) = provider_for_url(&s.url) {
            Some((p.name.to_string(), p.category, true, Some(domain.clone())))
        } else if s.kind == "script" && let Some((name, category)) = script_hint(&s.url) {
            Some((name.to_string(), category, true, None))
        } else if third_party && suspect_host(&host) {
            Some((domain.clone(), Suspected, false, Some(domain.clone())))
        } else if third_party && let Some(category) = ad_url_shape(&s.url) {
            Some((domain.clone(), category, false, Some(domain.clone())))
        } else if s.kind == "popup" && third_party {
            // A tab the page opened to another site: an ad until shown otherwise.
            Some((domain.clone(), Popunder, false, Some(domain.clone())))
        } else {
            None
        };
        let Some((name, category, known, domain)) = found else { continue };
        let hit = hit(&mut hits, name, category, known);
        if let Some(d) = domain
            && !d.is_empty()
            && d != site
        {
            hit.domains.insert(d);
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

/// The hit for a provider, created if this is its first signal.
fn hit<'a>(
    hits: &'a mut BTreeMap<String, ProviderHit>,
    name: String,
    category: AdCategory,
    known: bool,
) -> &'a mut ProviderHit {
    hits.entry(name.clone()).or_insert_with(|| ProviderHit { name, category, known, domains: BTreeSet::new(), signals: Vec::new() })
}

/// Tokens of code that actually builds a tag, so a page that merely mentions a network
/// (an ad blocker's list, a comment) is not taken for one that runs it.
const TAG_MARKERS: &[&str] = &["http", ".js", "src=", "createelement", "window.open", "data-zone", "atob", "<iframe", "<script"];

/// Networks named in a piece of page code, with the hosts of their tags found in it.
fn inline_matches(text: &str, site: &str) -> BTreeMap<&'static str, BTreeSet<String>> {
    let text = plain_code(text);
    let lower = text.to_ascii_lowercase();
    let mut named: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    if !TAG_MARKERS.iter().any(|m| lower.contains(m)) {
        return named;
    }
    for (marker, name) in INLINE_HINTS {
        if lower.contains(marker)
            && let Some(p) = provider_named(name)
        {
            named.entry(p.name).or_default();
        }
    }
    for u in urls_in(&text) {
        let host = url::host(&u);
        if host.is_empty() {
            continue;
        }
        let domain = blockable_domain(&host);
        if domain == site {
            continue;
        }
        if let Some(p) = provider_for_url(&u) {
            named.entry(p.name).or_default().insert(domain);
        }
    }
    named
}

/// Third-party tag hosts and frames built by a piece of page code that no catalogue entry
/// names: self-hosted pop-under tags and ad-server frames.
fn inline_tags(text: &str, site: &str) -> Vec<(String, AdCategory, BTreeSet<String>)> {
    let text = plain_code(text);
    let mut tags: BTreeMap<String, (AdCategory, BTreeSet<String>)> = BTreeMap::new();
    for u in urls_in(&text) {
        let host = url::host(&u);
        if host.is_empty() {
            continue;
        }
        let domain = blockable_domain(&host);
        if domain == site || provider_for_url(&u).is_some() {
            continue;
        }
        let category = if let Some(category) = ad_url_shape(&u) {
            category
        } else if suspect_host(&host) {
            Suspected
        } else {
            continue;
        };
        tags.entry(domain.clone()).or_insert_with(|| (category, BTreeSet::new())).1.insert(domain);
    }
    tags.into_iter().map(|(name, (category, domains))| (name, category, domains)).collect()
}

/// Page code with the escapes of a JavaScript string undone.
fn plain_code(text: &str) -> String {
    text.replace("\\/", "/").replace("\\u002F", "/").replace("\\u002f", "/")
}

/// URLs written in a piece of page code.
fn urls_in(text: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = ["http://", "https://", "//"].iter().filter_map(|p| rest.find(p)).min() {
        let tail = &rest[start..];
        let end = tail.find(|c: char| c.is_whitespace() || "\"'<>`()[]{}|".contains(c)).unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if url.len() > 8 {
            if url.starts_with("//") {
                out.insert(format!("https:{url}"));
            } else {
                out.insert(url.to_string());
            }
        }
        rest = &tail[end..];
    }
    out.into_iter().collect()
}

fn script_hint(u: &str) -> Option<(&'static str, AdCategory)> {
    let path = path_of(u).to_ascii_lowercase();
    SCRIPT_HINTS.iter().find(|(hint, _, _)| path.contains(hint)).map(|(_, name, category)| (*name, *category))
}

fn path_of(u: &str) -> &str {
    let rest = u.split_once("://").map_or(u, |(_, r)| r);
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let path = rest.find('/').map_or("", |i| &rest[i..]);
    path.split(['?', '#']).next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(kind: &str, url: &str) -> AdSignal {
        AdSignal { kind: kind.into(), url: url.into(), ..Default::default() }
    }

    fn inline(code: &str) -> AdSignal {
        AdSignal { kind: "inline".into(), detail: code.into(), ..Default::default() }
    }

    #[test]
    fn known_hosts() {
        assert_eq!(provider_for_host("pagead2.googlesyndication.com").unwrap().category, Display);
        assert_eq!(provider_for_host("c.adsco.re"), None);
        assert_eq!(provider_for_host("syndication.exosrv.com").unwrap().name, "ExoClick");
        assert!(provider_for_host("notdoubleclick.net").is_none());
        assert!(provider_for_host("media.net").is_some() && provider_for_host("social.media.network").is_none());
        // The tag domains these sites' pop-unders and banners are served from.
        assert_eq!(provider_for_host("llvpn.com").unwrap().name, "PropellerAds / Monetag");
        assert_eq!(provider_for_host("a1w3m3e3.exacdn.com").unwrap().name, "ExoClick");
        assert_eq!(provider_for_host("animeapp.org").unwrap().name, "AdWheel");
        assert_eq!(provider_for_host("s.smartfloralab.com").unwrap().name, "ExoClick");
    }

    #[test]
    fn suspects() {
        assert!(suspect_host("ads.example.net"));
        assert!(suspect_host("adserver-3.cdn.io"));
        assert!(suspect_host("pop2.example.com"));
        assert!(!suspect_host("adobe.com") && !suspect_host("roads.example.com") && !suspect_host("downloads.org"));
    }

    #[test]
    fn tag_urls() {
        assert_eq!(provider_for_url("https://a1w3m3e3.exacdn.com/popunder1000.js").unwrap().name, "ExoClick");
        assert_eq!(provider_for_url("https://d2f9q8zryik75d.cloudfront.net/?rzqfd=1500352").unwrap().name, "AdMaven");
        assert_eq!(provider_for_url("https://x.predictivadnetworks.com/jup/next.php?r=1").unwrap().name, "PopAds");
        assert_eq!(provider_for_url("https://llvpn.com/tag.min.js").unwrap().name, "PropellerAds / Monetag");
        assert_eq!(provider_for_url("https://some-cdn.net/aclib.js").unwrap().name, "AdCash");
        assert!(provider_for_url("https://cdn.example.com/app.js").is_none());
    }

    #[test]
    fn ad_tags_by_shape() {
        // The self-hosted pop-under tags of gogoanime, aniwaves, anv.to and hianime's player.
        assert!(is_ad_tag("https://tq.minusesasfast.com/r5wZdI0boL5xio/155474"));
        assert!(is_ad_tag("https://eb.seedilyspottle.com/rPnlCBl0egCo/136833"));
        assert!(is_ad_tag("https://rb.worthsloner.com/sym9ExyaWg1h/156590"));
        assert!(is_ad_tag("https://cd.teniacites.com/rQmqYElXH2E/147832"));
        assert!(is_ad_tag("https://cdn.example.com/tag.min.js"));
        // Not every third-party URL with numbers in it is an ad tag.
        assert!(!is_ad_tag("https://cdn.example.com/wp-content/themes/x/app.js"));
        assert!(!is_ad_tag("https://cdn.example.com/api/v1/12345"));
        assert!(!is_ad_tag("https://cdn.example.com/a1B2c3D4e5/asset-1"));
        assert!(is_ad_frame("https://animeapp.org/iframe/6ad641d19600c?iframe&ag_custom_domain=bn"));
        assert!(!is_ad_frame("https://www.youtube.com/embed/dQw4w9WgXcQ"));
        // Redirects and video ad requests, which name no network either.
        assert_eq!(ad_url_shape("https://vinniebags.com/go/2954557?ref=x&subid1=animepahe.su"), Some(Popunder));
        assert_eq!(ad_url_shape("https://ads.example.net/vast.xml?zone=3"), Some(Video));
        assert_eq!(ad_url_shape("https://cdn.example.net/vast-banner.png"), None);
        assert_eq!(ad_url_shape("https://example.com/devastating-news"), None);
        assert_eq!(ad_url_shape("https://example.com/watch?adtag=1"), None);
    }

    #[test]
    fn redirect_ads_in_page_code() {
        // animepahe.su sends visitors on through an encoded direct-link ad.
        let code = "var _0x='aHR0cHM6Ly92aW5uaWViYWdzLmNvbS9nby8yOTU0NTU3'; location.replace(window.atob(_0x));";
        let decoded = "var u = 'https://vinniebags.com/go/2954557?ref=www.google.com&subid1=www.google.com&subid2=animepahe.su'";
        let hits = providers("https://animepahe.pw/", &[inline(code), inline(decoded)]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "vinniebags.com");
        assert_eq!((hits[0].category, hits[0].known), (Popunder, false));
    }

    #[test]
    fn inline_code_names_networks() {
        // The ad tag reanime.to carries in an inline script (with its comments).
        let code = r#"
            var AD_NETWORK = 'monetag';
            var TAGS = {
                monetag: function (s) { s.dataset.zone = '11123362'; s.src = 'https://llvpn.com/tag.min.js'; },
                admaven: function (s) { s.src = 'https://d2f9q8zryik75d.cloudfront.net/?rzqfd=1500352'; }
            };
            // AdMaven's tag is kept as the fallback.
        "#;
        let hits = providers("https://reanime.to/watch/x", &[inline(code)]);
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["AdMaven", "PropellerAds / Monetag"]);
        let monetag = &hits[1];
        assert!(monetag.known);
        assert_eq!(monetag.signals.len(), 1);
        assert_eq!(monetag.domains.iter().cloned().collect::<Vec<_>>(), vec!["llvpn.com"]);
        assert_eq!(monetag.summary(), "1 inline tag");
        // The tag lives on shared CloudFront: a rule must name that host, not all of CloudFront.
        assert_eq!(hits[0].domains.iter().cloned().collect::<Vec<_>>(), vec!["d2f9q8zryik75d.cloudfront.net"]);
    }

    #[test]
    fn encoded_ad_payloads() {
        // What kaa.lt keeps base64-encoded (the inspector decodes it before this point).
        let code = "<!-- AdWheel / bn_adbuster_300x250_na_kickass-anime.ro --> \
                    <iframe src='https://animeapp.org/iframe/6ad641d19600c?iframe&ag_custom_domain=bn_adbuster_300x250'></iframe>";
        let hits = providers("https://kaa.lt/", &[inline(code)]);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].name.as_str(), hits[0].category), ("AdWheel", Display));
        assert_eq!(hits[0].domains.iter().cloned().collect::<Vec<_>>(), vec!["animeapp.org"]);
        assert_eq!(hits[0].summary(), "1 inline tag");
    }

    #[test]
    fn inline_tags_without_a_name() {
        // anv.to's pop-under tag, only written in its JavaScript.
        let code = r#"let e=document.createElement("script");e.src="//rb.worthsloner.com/sym9ExyaWg1h/156590""#;
        let hits = providers("https://anv.to/", &[inline(code)]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "worthsloner.com");
        assert_eq!((hits[0].category, hits[0].known), (Popunder, false));
    }

    #[test]
    fn a_mention_is_not_a_tag() {
        // An ad blocker's list mentions networks; it runs none of them.
        let code = "const blocked = ['monetag', 'adsterra', 'popads', 'exoclick']; // block ads";
        assert!(providers("https://example.com/", &[inline(code)]).is_empty());
    }

    #[test]
    fn tags_without_a_name() {
        let signals = vec![
            sig("script", "https://tq.minusesasfast.com/r5wZdI0boL5xio/155474"),
            sig("request", "https://tq.minusesasfast.com/r5wZdI0boL5xio/155474"),
            sig("script", "https://gogoanime.by/wp-content/themes/dramastream/assets/js/app.js"),
        ];
        let hits = providers("https://gogoanime.by/", &signals);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "minusesasfast.com");
        assert_eq!((hits[0].category, hits[0].known), (Popunder, false));
        assert_eq!(hits[0].summary(), "1 script · 1 request");
    }

    #[test]
    fn the_sites_that_were_missed() {
        // What the collector finds on the sites reported as showing no ads, reduced to the
        // signals that matter (requests to the site's own assets are left out).
        let unknown = |site: &str, signals: Vec<AdSignal>, domain: &str| {
            let hits = providers(site, &signals);
            assert_eq!(hits.len(), 1, "{site}: {hits:?}");
            assert_eq!((hits[0].name.as_str(), hits[0].category, hits[0].known), (domain, Popunder, false), "{site}");
            assert_eq!(hits[0].domains.iter().cloned().collect::<Vec<_>>(), vec![domain], "{site}");
        };
        // gogoanime.by, aniwaves.ru: a pop-under tag from a self-hosted, throwaway domain.
        unknown("https://gogoanime.by/", vec![sig("script", "https://tq.minusesasfast.com/r5wZdI0boL5xio/155474")], "minusesasfast.com");
        unknown("https://aniwaves.ru/home", vec![sig("script", "https://eb.seedilyspottle.com/rPnlCBl0egCo/136833")], "seedilyspottle.com");
        // anv.to: the same kind of tag, written into a JavaScript bundle.
        unknown(
            "https://anv.to/",
            vec![inline(r#"let e=document.createElement("script");e.src="//rb.worthsloner.com/sym9ExyaWg1h/156590""#)],
            "worthsloner.com",
        );
        // hianime.org.vc: its player (megavid.buzz) loads the same kind of tag.
        unknown(
            "https://hianime.org.vc/the-iceblade-sorcerer-shall-rule-the-world-ii-episode-4/",
            vec![sig("script", "https://cd.teniacites.com/rQmqYElXH2E/147832")],
            "teniacites.com",
        );
        // animepahe.pw sends visitors on through an encoded direct-link ad.
        unknown(
            "https://animepahe.pw/",
            vec![inline("var u='https://vinniebags.com/go/2954557?ref=x&subid1=www.google.com&subid2=animepahe.su'")],
            "vinniebags.com",
        );

        // reanime.to: the tag is an inline script that names its networks.
        let reanime = providers(
            "https://reanime.to/watch/x",
            &[
                inline("var TAGS={monetag:function(s){s.dataset.zone='1';s.src='https://llvpn.com/tag.min.js'},admaven:function(s){s.src='https://d2f9q8zryik75d.cloudfront.net/?rzqfd=1500352'}};"),
                sig("script", "https://llvpn.com/tag.min.js"),
            ],
        );
        let names: Vec<&str> = reanime.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["PropellerAds / Monetag", "AdMaven"]);

        // kaa.lt: PopMagic (ExoClick) and the base64 banner tags it carries.
        let kaa = providers(
            "https://kaa.lt/",
            &[
                inline("var p='https://omegatoki.com/r?db=abc';"),
                inline("<!-- AdWheel --><iframe src='https://animeapp.org/iframe/6ad641d19600c?iframe&ag_custom_domain=bn'></iframe>"),
                sig("script", "https://a1w3m3e3.exacdn.com/popunder1000.js"),
                sig("iframe", "https://animeapp.org/iframe/6ad6434ee5877?iframe&ag_custom_domain=bn"),
            ],
        );
        let names: Vec<&str> = kaa.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["ExoClick", "PopAds", "AdWheel"]);
        assert!(kaa.iter().all(|h| h.known));
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

    #[test]
    fn every_hint_names_a_network() {
        for (marker, name) in INLINE_HINTS {
            assert!(provider_named(name).is_some(), "inline hint {marker} names unknown {name}");
        }
        for (needle, name) in URL_HINTS {
            assert!(provider_named(name).is_some(), "URL hint {needle} names unknown {name}");
        }
    }

    #[test]
    fn blockable() {
        assert_eq!(blockable_domain("pagead2.googlesyndication.com"), "googlesyndication.com");
        assert_eq!(blockable_domain("a.b.example.co.uk"), "example.co.uk");
        assert_eq!(blockable_domain("d2f9q8zryik75d.cloudfront.net"), "d2f9q8zryik75d.cloudfront.net");
    }
}
