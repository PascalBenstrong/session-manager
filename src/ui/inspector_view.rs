//! Inspector (`inspector.html?tab=ID`): inspects a tab's page without the browser's
//! developer tools, through a content script in the extension's isolated world
//! (`web/inspect.js`). It runs docked at the bottom of the page (the default; an
//! iframe the content script adds, `&dock=TOKEN`). Panes: Elements (live DOM tree, picker,
//! details, context menus, screenshots), Network, Media & frames (video, frame, script...
//! URLs of every frame), Capture (record or save the page's media, see `capture_view.rs`),
//! Ads (the ad networks the page runs) and Source.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, CursorIcon, Layout, Margin, RichText, Sense, Stroke};
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::Actions;
use super::theme;
use super::widgets::{badge, empty_state, icon_button, primary_button, segmented, text_input};
use crate::ads::{self, AdCategory, AdSignal, ProviderHit};
use crate::guards::{BlockRule, BlockRuleKind, normalize_site};
use crate::model::{AppState, TabId};
use crate::platform::{self as pf, PageResource};
use crate::protocol::Command;

/// Order of the element types in Media & frames.
const KINDS: [(&str, &str); 11] = [
    ("video", "🎞 Videos"),
    ("audio", "🔊 Audio"),
    ("source", "⤷ Video / audio sources"),
    ("track", "💬 Subtitle tracks"),
    ("network", "📡 Media requests (streams)"),
    ("poster", "🖼 Video posters"),
    ("iframe", "▣ Iframes"),
    ("frame", "▣ Frames"),
    ("embed", "◫ Embeds"),
    ("object", "◫ Objects"),
    ("script", "📜 Scripts"),
];

/// Reconnection attempts after the page went away (navigation, reload), one a second.
const RECONNECT_TRIES: u32 = 20;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct NodeSummary {
    id: u64,
    kind: String,
    tag: String,
    label: String,
    children: usize,
    text: String,
    /// Iframes and frames: where they are.
    src: String,
}

impl NodeSummary {
    fn is_frame(&self) -> bool {
        self.kind == "element" && matches!(self.tag.as_str(), "iframe" | "frame")
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
struct Details {
    node: NodeSummary,
    path: Vec<NodeSummary>,
    attributes: Vec<(String, String)>,
    rect: Option<Rect>,
    selector: String,
    style: Vec<(String, String)>,
    outer_html: String,
    text: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Root {
    title: String,
    url: String,
    node: NodeSummary,
    /// Names the document (each page loaded in the tab gets its own).
    doc: String,
}

/// What the content script tells a connected inspector.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PageEvent {
    picked: Option<Picked>,
    pick_cancelled: bool,
    changed: Option<Vec<u64>>,
    details: bool,
    title: Option<String>,
    disconnected: bool,
    /// Network records (see web/net-recorder.js).
    net: Option<Vec<Value>>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Picked {
    id: u64,
    path: Vec<u64>,
}

enum Reply {
    Ready(Result<Root, String>),
    Children(u64, Result<Vec<NodeSummary>, String>),
    Details(Result<Details, String>),
    /// Details read again after the page changed (a removed element just goes away).
    Refreshed(Result<Details, String>),
    Query(Result<Vec<NodeSummary>, String>),
    /// The ids from the root down to a node, to open the tree to it.
    Reveal(u64, Result<Vec<u64>, String>),
    Source(Result<String, String>),
    Resources(Result<Vec<PageResource>, String>),
    Ads(Result<Vec<AdSignal>, String>),
    /// Text to put on the clipboard, and what it is ("CSS selector").
    Copy(&'static str, Result<String, String>),
    Shot(&'static str, Result<pf::ShotSize, String>),
    Done(Result<(), String>),
    /// The state saved for this tab (see [`InspectorView::snapshot`]).
    Restored(Option<String>),
    /// Elements matching the selector of the element selected before a reload.
    Reselect(Result<Vec<NodeSummary>, String>),
    /// Errors not worth showing (highlighting an element that just went away).
    Quiet,
}

#[derive(Default)]
struct Inbox {
    replies: Vec<Reply>,
    /// Page events, with the connection they came from.
    events: Vec<(u32, PageEvent)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Elements,
    Network,
    Media,
    Capture,
    Ads,
    Source,
}

/// Remembers the text last selected in the inspector's labels (egui only reports it as an
/// output command), for "Copy selected text".
struct SelectionWatcher(Arc<Mutex<String>>);

impl egui::Plugin for SelectionWatcher {
    fn debug_name(&self) -> &'static str {
        "InspectorSelectionWatcher"
    }

    fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
        for cmd in &output.platform_output.commands {
            if let egui::OutputCommand::TextSelectionSettled(text) = cmd
                && let Ok(mut last) = self.0.lock()
            {
                last.clone_from(text);
            }
        }
    }
}

pub struct InspectorView {
    tab: Option<TabId>,
    /// Docked in the page: the token the page's dock gave this page.
    dock: Option<String>,
    ctx: egui::Context,
    inbox: Rc<RefCell<Inbox>>,
    /// Current connection to the page (events of older ones are ignored).
    connection: u32,
    connected: bool,
    /// Reconnecting after the page went away: (tries left, when to try next).
    reconnect: Option<(u32, f64)>,
    selection: Arc<Mutex<String>>,
    pane: Pane,
    error: Option<String>,
    title: String,
    url: String,
    root: Option<Rc<NodeSummary>>,
    children: HashMap<u64, Rc<Vec<NodeSummary>>>,
    expanded: HashSet<u64>,
    selected: Option<u64>,
    details: Option<Rc<Details>>,
    scroll_to_selected: bool,
    picking: bool,
    hovered: Option<u64>,
    query: String,
    query_results: Option<Rc<Vec<NodeSummary>>>,
    /// Select the first match of the running query (ad slots' "Show").
    select_first_match: bool,
    style_filter: String,
    confirm_delete: bool,
    resources: Option<Rc<Vec<PageResource>>>,
    res_query: String,
    /// The resources matching `res_query`, by kind (see [`InspectorView::media`]).
    res_groups: Option<(String, Rc<ResourceGroups>)>,
    ads: Option<Rc<Vec<AdSignal>>>,
    /// The ad networks found, for (the page, the state revision).
    ad_hits: Option<((String, Option<u64>), Rc<Vec<ProviderHit>>)>,
    source: Option<Rc<Vec<String>>>,
    source_query: String,
    /// The source lines matching `source_query`.
    source_shown: Option<(String, Rc<Vec<usize>>)>,
    network: super::network_view::NetworkPane,
    capture: super::capture_view::CapturePane,
    /// After a reload: replace the tree once the new page is read, and select this again.
    replace_tree: bool,
    restore_selector: Option<String>,
    /// The pane to stay in while selecting an element again.
    keep_pane: Option<Pane>,
    /// The tab's saved state was read (saving starts then), and what was saved last.
    restored: bool,
    pending_ready: Option<(String, String)>,
    pending_net: Vec<Vec<Value>>,
    saved: (u64, u8, Option<u64>),
    last_save: f64,
}

/// Page resources matching a search, by their place in [`KINDS`] (positions in the list).
type ResourceGroups = BTreeMap<usize, Vec<usize>>;

/// Previews longer than this (in bytes) are cut; copying gives all of it.
const PREVIEW_MAX: usize = 50 * 1024;

/// `text`, cut to [`PREVIEW_MAX`] with a note saying so.
fn preview(text: &str) -> std::borrow::Cow<'_, str> {
    if text.len() <= PREVIEW_MAX {
        return text.into();
    }
    let mut cut = PREVIEW_MAX;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… ({} KB more: copy it to see all of it)", &text[..cut], (text.len() - cut) / 1024).into()
}

fn parse<T: for<'de> Deserialize<'de>>(r: Result<Value, String>) -> Result<T, String> {
    r.and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
}

/// Where a tab's inspector state is kept for the browser session.
fn state_key(tab: TabId) -> String {
    format!("smInspector:{tab}")
}

impl InspectorView {
    pub fn new(ctx: &egui::Context) -> Self {
        let tab = pf::location_param("tab").parse::<TabId>().ok();
        let dock = Some(pf::location_param("dock")).filter(|d| !d.is_empty());
        let selection = Arc::new(Mutex::new(String::new()));
        ctx.add_plugin(SelectionWatcher(selection.clone()));
        let mut v = Self {
            tab,
            dock,
            ctx: ctx.clone(),
            inbox: Rc::new(RefCell::new(Inbox::default())),
            connection: 0,
            connected: false,
            reconnect: None,
            selection,
            pane: Pane::Elements,
            error: tab.is_none().then(|| "No tab to inspect.".to_string()),
            title: String::new(),
            url: String::new(),
            root: None,
            children: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            details: None,
            scroll_to_selected: false,
            picking: false,
            hovered: None,
            query: String::new(),
            query_results: None,
            select_first_match: false,
            style_filter: String::new(),
            confirm_delete: false,
            resources: None,
            res_query: String::new(),
            res_groups: None,
            ad_hits: None,
            source_shown: None,
            ads: None,
            source: None,
            source_query: String::new(),
            network: super::network_view::NetworkPane::new(ctx, tab),
            capture: super::capture_view::CapturePane::new(ctx, tab),
            replace_tree: false,
            restore_selector: None,
            keep_pane: None,
            restored: tab.is_none(),
            pending_ready: None,
            pending_net: Vec::new(),
            saved: (0, 0, None),
            last_save: 0.0,
        };
        if let Some(tab) = tab {
            let inbox = v.inbox.clone();
            let ctx = v.ctx.clone();
            spawn_local(async move {
                let saved = pf::session_get(&state_key(tab)).await.ok().flatten();
                inbox.borrow_mut().replies.push(Reply::Restored(saved));
                ctx.request_repaint();
            });
        }
        v.connect();
        v
    }

    /// Reads the page from scratch (Refresh, Retry).
    fn connect(&mut self) {
        self.root = None;
        self.children.clear();
        self.expanded.clear();
        self.details = None;
        self.selected = None;
        self.attach();
    }

    /// The page was reloaded or navigated: the old tree stays until the new page is read,
    /// and the element selected before is selected again if the new page has it.
    fn reattach(&mut self) {
        if let Some(sel) = self.details.as_ref().map(|d| d.selector.clone()).filter(|s| !s.is_empty()) {
            self.restore_selector = Some(sel);
        }
        self.replace_tree = true;
        self.attach();
    }

    /// Injects the content script, connects to it (live changes, picks) and loads the
    /// document root.
    fn attach(&mut self) {
        let Some(tab) = self.tab else { return };
        self.error = None;
        self.picking = false;
        self.connection += 1;
        let connection = self.connection;
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        let dock = self.dock.clone();
        self.run(
            async move {
                pf::inspector_inject(tab).await?;
                if let Some(token) = dock
                    && !pf::verify_dock(tab, &token).await?
                {
                    return Err("This inspector wasn't opened by Session Manager.".into());
                }
                let cb = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
                    if let Some(e) = v.as_string().and_then(|s| serde_json::from_str::<PageEvent>(&s).ok()) {
                        inbox.borrow_mut().events.push((connection, e));
                        ctx.request_repaint();
                    }
                });
                pf::inspector_connect(tab, cb.as_ref().unchecked_ref())?;
                cb.forget();
                pf::inspect(tab, &json!({ "op": "root" })).await
            },
            |r| Reply::Ready(parse(r)),
        );
    }

    fn run<F>(&self, fut: F, reply: impl FnOnce(Result<Value, String>) -> Reply + 'static)
    where
        F: std::future::Future<Output = Result<Value, String>> + 'static,
    {
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        spawn_local(async move {
            let r = fut.await;
            inbox.borrow_mut().replies.push(reply(r));
            ctx.request_repaint();
        });
    }

    fn ask(&self, request: Value, reply: impl FnOnce(Result<Value, String>) -> Reply + 'static) {
        let Some(tab) = self.tab else { return };
        self.run(async move { pf::inspect(tab, &request).await }, reply);
    }

    /// A request whose failure isn't worth an error message.
    fn tell(&self, request: Value) {
        self.ask(request, |_| Reply::Quiet);
    }

    fn load_children(&self, id: u64) {
        self.ask(json!({ "op": "children", "id": id }), move |r| Reply::Children(id, parse(r)));
    }

    fn select(&mut self, id: u64) {
        self.selected = Some(id);
        self.confirm_delete = false;
        self.ask(json!({ "op": "details", "id": id }), |r| Reply::Details(parse(r)));
        self.tell(json!({ "op": "scrollTo", "id": id }));
    }

    /// Opens the tree down to a node, selects it and scrolls the tree to it.
    fn reveal(&mut self, id: u64, path: &[u64]) {
        self.pane = self.keep_pane.take().unwrap_or(Pane::Elements);
        for a in path.iter().filter(|a| **a != id) {
            self.expanded.insert(*a);
            if !self.children.contains_key(a) {
                self.load_children(*a);
            }
        }
        self.select(id);
        self.scroll_to_selected = true;
    }

    fn copy(&self, id: u64, what: &'static str, label: &'static str) {
        self.ask(json!({ "op": "copy", "id": id, "what": what }), move |r| Reply::Copy(label, parse(r)));
    }

    fn screenshot(&self, id: u64, action: &'static str) {
        let Some(tab) = self.tab else { return };
        self.run(async move { pf::capture_element(tab, id, action).await.map(|s| json!([s.width, s.height])) }, move |r| {
            Reply::Shot(
                action,
                r.map(|v| pf::ShotSize { width: v[0].as_u64().unwrap_or(0) as u32, height: v[1].as_u64().unwrap_or(0) as u32 }),
            )
        });
    }

    fn load_resources(&mut self) {
        let Some(tab) = self.tab else { return };
        self.run(
            async move {
                let list = pf::collect_resources(tab).await?;
                Ok(Value::Array(list.into_iter().map(|r| json!([r.kind, r.url, r.detail, r.frame, r.top])).collect()))
            },
            |r| {
                Reply::Resources(r.map(|v| {
                    v.as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| {
                                    Some(PageResource {
                                        kind: x[0].as_str()?.to_string(),
                                        url: x[1].as_str()?.to_string(),
                                        detail: x[2].as_str().unwrap_or("").to_string(),
                                        frame: x[3].as_str().unwrap_or("").to_string(),
                                        top: x[4].as_bool().unwrap_or(true),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                }))
            },
        );
    }

    fn load_ads(&mut self) {
        let Some(tab) = self.tab else { return };
        self.run(
            async move {
                let selectors: Vec<&str> = ads::SLOT_SELECTORS.iter().map(|(s, _)| *s).collect();
                let list = pf::collect_ad_signals(tab, &selectors).await?;
                Ok(Value::Array(list.into_iter().map(|s| json!([s.kind, s.url, s.detail, s.frame])).collect()))
            },
            |r| {
                Reply::Ads(r.map(|v| {
                    v.as_array()
                        .map(|a| {
                            a.iter()
                                .map(|x| AdSignal {
                                    kind: x[0].as_str().unwrap_or("").to_string(),
                                    url: x[1].as_str().unwrap_or("").to_string(),
                                    detail: x[2].as_str().unwrap_or("").to_string(),
                                    frame: x[3].as_str().unwrap_or("").to_string(),
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                }))
            },
        );
    }

    /// What to carry on with when this tab's inspector loads again (a docked inspector
    /// reloads with its page; the window keeps it when moved).
    fn snapshot(&self) -> Value {
        json!({
            "pane": match self.pane {
                Pane::Elements => "elements",
                Pane::Network => "network",
                Pane::Media => "media",
                Pane::Capture => "capture",
                Pane::Ads => "ads",
                Pane::Source => "source",
            },
            "selector": self.details.as_ref().map(|d| d.selector.clone()).or_else(|| self.restore_selector.clone()),
            "network": self.network.snapshot(),
        })
    }

    fn restore(&mut self, saved: Option<String>) {
        if let Some(v) = saved.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
            self.apply_saved(&v);
        }
        // The page read meanwhile, and its requests, come after what was restored.
        self.restored = true;
        if let Some((doc, url)) = self.pending_ready.take() {
            self.network.on_ready(&doc, &url);
        }
        for records in std::mem::take(&mut self.pending_net) {
            self.network.apply(records);
        }
    }

    fn apply_saved(&mut self, v: &Value) {
        self.pane = match v["pane"].as_str() {
            Some("network") => Pane::Network,
            Some("media") => Pane::Media,
            Some("capture") => Pane::Capture,
            Some("ads") => Pane::Ads,
            Some("source") => Pane::Source,
            _ => Pane::Elements,
        };
        match self.pane {
            Pane::Media => self.load_resources(),
            Pane::Capture => self.capture.refresh(),
            Pane::Ads => self.load_ads(),
            Pane::Source => self.refresh_pane(),
            _ => {}
        }
        self.network.restore(&v["network"]);
        if self.selected.is_none()
            && let Some(sel) = v["selector"].as_str().filter(|s| !s.is_empty())
        {
            self.restore_selector = Some(sel.to_string());
            if self.root.is_some() {
                self.restore_selection();
            }
        }
    }

    /// Selects the element selected before (by its selector) in the page now read.
    fn restore_selection(&mut self) {
        if let Some(selector) = self.restore_selector.take() {
            self.ask(json!({ "op": "query", "selector": selector }), |r| Reply::Reselect(parse(r)));
        }
    }

    /// A docked inspector (which reloads with its page) saves the tab's state when it
    /// changed, at most every 400 ms. A window keeps its state, and saves it when it moves.
    fn save_state(&mut self) {
        let Some(tab) = self.tab else { return };
        if self.dock.is_none() {
            return;
        }
        let key = (self.network.revision, self.pane as u8, self.selected);
        if !self.restored || key == self.saved {
            return;
        }
        let wait = 400.0 - (pf::now() - self.last_save);
        if wait > 0.0 {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(wait as u64 + 1));
            return;
        }
        self.saved = key;
        self.last_save = pf::now();
        let snapshot = self.snapshot().to_string();
        spawn_local(async move {
            let _ = pf::session_set(&state_key(tab), &snapshot).await;
        });
    }

    fn refresh_pane(&mut self) {
        match self.pane {
            Pane::Elements | Pane::Network => self.connect(),
            Pane::Media => self.load_resources(),
            Pane::Capture => self.capture.refresh(),
            Pane::Ads => self.load_ads(),
            Pane::Source => self.ask(json!({ "op": "source" }), |r| Reply::Source(parse(r))),
        }
    }

    fn drain(&mut self, acts: &mut Actions) {
        let (replies, events) = {
            let mut inbox = self.inbox.borrow_mut();
            (std::mem::take(&mut inbox.replies), std::mem::take(&mut inbox.events))
        };
        for r in replies {
            match r {
                Reply::Ready(Ok(root)) => {
                    self.connected = true;
                    self.reconnect = None;
                    if std::mem::take(&mut self.replace_tree) {
                        self.children.clear();
                        self.expanded.clear();
                        self.details = None;
                        self.selected = None;
                        self.query_results = None;
                    }
                    self.title = root.title;
                    if self.restored {
                        self.network.on_ready(&root.doc, &root.url);
                    } else {
                        self.pending_ready = Some((root.doc.clone(), root.url.clone()));
                    }
                    self.url = root.url;
                    self.restore_selection();
                    self.expanded.insert(root.node.id);
                    self.load_children(root.node.id);
                    self.root = Some(Rc::new(root.node));
                }
                Reply::Ready(Err(e)) => {
                    // Still loading after a navigation: try again shortly.
                    match self.reconnect {
                        Some((tries, _)) if tries > 0 => self.reconnect = Some((tries - 1, pf::now() + 1000.0)),
                        _ => {
                            self.reconnect = None;
                            self.error = Some(e);
                        }
                    }
                }
                Reply::Children(id, Ok(list)) => {
                    // Open <body> by default.
                    for n in list.iter().filter(|n| n.tag == "body") {
                        if self.expanded.insert(n.id) {
                            self.load_children(n.id);
                        }
                    }
                    self.children.insert(id, Rc::new(list));
                }
                Reply::Children(id, Err(_)) => {
                    // The node went away with a change of the page.
                    self.children.remove(&id);
                    self.expanded.remove(&id);
                }
                Reply::Details(Ok(d)) | Reply::Refreshed(Ok(d)) => {
                    if self.selected == Some(d.node.id) {
                        self.details = Some(Rc::new(d));
                    }
                }
                Reply::Refreshed(Err(_)) => {
                    self.details = None;
                    self.selected = None;
                }
                Reply::Query(Ok(list)) => {
                    if std::mem::take(&mut self.select_first_match)
                        && let Some(first) = list.first()
                    {
                        let id = first.id;
                        self.ask(json!({ "op": "reveal", "id": id }), move |r| Reply::Reveal(id, parse(r)));
                    }
                    self.query_results = Some(Rc::new(list));
                }
                Reply::Reveal(id, Ok(path)) => self.reveal(id, &path),
                Reply::Restored(saved) => self.restore(saved),
                Reply::Reselect(Ok(list)) => {
                    if let Some(first) = list.first() {
                        let id = first.id;
                        let pane = self.pane;
                        self.ask(json!({ "op": "reveal", "id": id }), move |r| Reply::Reveal(id, parse(r)));
                        // Selecting it again shouldn't change the pane.
                        self.keep_pane = Some(pane);
                    }
                }
                Reply::Reselect(Err(_)) => {}
                Reply::Source(Ok(s)) => {
                    self.source = Some(Rc::new(s.lines().map(String::from).collect()));
                    self.source_shown = None;
                }
                Reply::Resources(Ok(list)) => {
                    self.resources = Some(Rc::new(list));
                    self.res_groups = None;
                }
                Reply::Ads(Ok(list)) => {
                    self.ads = Some(Rc::new(list));
                    self.ad_hits = None;
                }
                Reply::Copy(label, Ok(text)) => {
                    if text.is_empty() {
                        acts.notice(format!("Nothing to copy ({label} is empty)"));
                    } else {
                        self.ctx.copy_text(text);
                        acts.notice(format!("Copied the {label}"));
                    }
                }
                Reply::Shot(action, Ok(size)) => {
                    let what = if action == "copy" { "copied" } else { "saved" };
                    acts.notice(format!("Screenshot {what} ({} × {} px)", size.width, size.height));
                }
                Reply::Shot(_, Err(e)) => self.error = Some(format!("Screenshot failed: {e}")),
                Reply::Done(Ok(())) | Reply::Quiet => {}
                Reply::Details(Err(e))
                | Reply::Query(Err(e))
                | Reply::Reveal(_, Err(e))
                | Reply::Source(Err(e))
                | Reply::Resources(Err(e))
                | Reply::Ads(Err(e))
                | Reply::Copy(_, Err(e))
                | Reply::Done(Err(e)) => self.error = Some(e),
            }
        }
        for (connection, e) in events {
            if connection != self.connection {
                continue;
            }
            if e.disconnected {
                // The page navigated, reloaded or closed: read the new one when it's there.
                self.connected = false;
                self.picking = false;
                self.reconnect = Some((RECONNECT_TRIES, pf::now() + 800.0));
                continue;
            }
            if let Some(p) = e.picked {
                self.picking = false;
                self.reveal(p.id, &p.path);
            }
            if let Some(records) = e.net {
                if self.restored {
                    self.network.apply(records);
                } else {
                    self.pending_net.push(records);
                }
            }
            if e.pick_cancelled {
                self.picking = false;
            }
            if let Some(t) = e.title {
                self.title = t;
            }
            for id in e.changed.unwrap_or_default() {
                if self.children.contains_key(&id) {
                    self.load_children(id);
                }
            }
            if e.details
                && let Some(id) = self.selected
            {
                self.ask(json!({ "op": "details", "id": id }), |r| Reply::Refreshed(parse(r)));
            }
        }
        if let Some((tries, at)) = self.reconnect {
            let now = pf::now();
            if now >= at {
                self.reconnect = Some((tries, f64::INFINITY));
                // Panes keep showing the old page until the new one is read (no flicker).
                self.reattach();
                if !matches!(self.pane, Pane::Elements | Pane::Network) {
                    self.refresh_pane();
                }
            } else if at.is_finite() {
                self.ctx.request_repaint_after(std::time::Duration::from_millis((at - now) as u64));
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions, state: Option<&AppState>) {
        self.drain(acts);
        self.save_state();
        let p = theme::palette(ui);
        egui::Panel::top("inspector-header")
            .frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, if self.dock.is_some() { 4 } else { 8 })))
            .show(ui, |ui| self.header(ui, acts));
        if let Some(e) = self.error.clone() {
            egui::Panel::bottom("inspector-error").frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, 6))).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("⚠ {e}")).color(p.danger));
                        if ui.button("Retry").clicked() {
                            self.connect();
                        }
                        if icon_button(ui, "🗙", "Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                },
            );
        }
        let pad = egui::Frame::new().fill(p.bg).inner_margin(Margin::same(12));
        match self.pane {
            Pane::Elements => self.elements(ui, acts),
            Pane::Network => {
                let url = self.url.clone();
                self.network.ui(ui, acts, &url);
            }
            Pane::Media => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.media(ui, acts));
            }
            Pane::Capture => {
                let title = self.title.clone();
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.capture.ui(ui, acts, &title));
            }
            Pane::Ads => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.ads(ui, acts, state));
            }
            Pane::Source => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.source(ui, acts));
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        if self.dock.is_none() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("🔬").size(18.0).color(p.accent));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let title = if self.title.is_empty() { "Inspector" } else { &self.title };
                    ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
                    ui.add(egui::Label::new(RichText::new(&self.url).small().color(p.muted)).truncate());
                });
            });
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            let before = self.pane;
            segmented(
                ui,
                &mut self.pane,
                &[
                    (Pane::Elements, "Elements"),
                    (Pane::Network, "Network"),
                    (Pane::Media, "Media & frames"),
                    (Pane::Capture, "Capture"),
                    (Pane::Ads, "Ads"),
                    (Pane::Source, "Source"),
                ],
            );
            if self.pane != before {
                match self.pane {
                    Pane::Media if self.resources.is_none() => self.load_resources(),
                    Pane::Capture if !self.capture.loaded() => self.capture.refresh(),
                    Pane::Ads if self.ads.is_none() => self.load_ads(),
                    Pane::Source if self.source.is_none() => self.refresh_pane(),
                    _ => {}
                }
            }
            if self.connected {
                ui.label(RichText::new("● Live").small().color(p.success)).on_hover_text("The tree follows changes to the page");
            } else if self.reconnect.is_some() {
                ui.label(RichText::new("◌ Page loading…").small().color(p.muted));
            }
            if self.network.recording() && self.pane != Pane::Network {
                ui.label(RichText::new("● Recording").small().color(p.danger))
                    .on_hover_text("The Network view is recording the page's requests");
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.placement_buttons(ui, acts);
                if ui.button("⟳ Refresh").on_hover_text("Read the page again").clicked() {
                    self.refresh_pane();
                }
                if self.pane == Pane::Elements {
                    let label = if self.picking { "Picking… (Esc cancels)" } else { "🎯 Pick an element" };
                    if ui.add(primary_button(ui, label)).on_hover_text("Then click an element in the page").clicked() {
                        if self.picking {
                            self.picking = false;
                            self.tell(json!({ "op": "cancelPick" }));
                        } else {
                            self.picking = true;
                            self.ask(json!({ "op": "pick" }), |r| Reply::Done(r.map(drop)));
                        }
                    }
                }
                if self.dock.is_some() {
                    let title = if self.title.is_empty() { "Inspector" } else { &self.title };
                    ui.add(egui::Label::new(RichText::new(format!("🔬 {title}")).small().color(p.muted)).truncate());
                }
            });
        });
    }

    /// Dock / undock / close.
    fn placement_buttons(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let Some(tab) = self.tab else { return };
        // The inspector that opens carries on with this one's state.
        let mv = |place: &'static str, snapshot: String| {
            spawn_local(async move {
                let _ = pf::session_set(&state_key(tab), &snapshot).await;
                if let Err(e) = pf::move_inspector(tab, place).await {
                    web_sys::console::warn_1(&format!("could not move the inspector: {e}").into());
                }
            });
        };
        if self.dock.is_some() {
            if icon_button(ui, "🗙", "Close the inspector").clicked() {
                spawn_local(async move {
                    let _ = pf::undock_inspector(tab).await;
                });
            }
            if icon_button(ui, "🗗", "Open the inspector in its own window (and from now on)").clicked() {
                mv("window", self.snapshot().to_string());
            }
        } else if icon_button(ui, "⬓", "Dock the inspector at the bottom of the page (and from now on)").clicked() {
            mv("bottom", self.snapshot().to_string());
            acts.notice("Docking…");
        }
    }

    // ------------------------------------------------------------ elements

    fn elements(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        egui::Panel::right("details")
            .resizable(true)
            .default_size(if self.dock.is_some() { 520.0 } else { 440.0 })
            .frame(egui::Frame::new().fill(p.surface).inner_margin(Margin::same(12)))
            .show(ui, |ui| self.details_panel(ui, acts));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(10))).show(ui, |ui| {
            ui.horizontal(|ui| {
                let r = ui.add(text_input(
                    &mut self.query,
                    "Find by CSS selector, e.g. video, iframe[src*=ads], .popup",
                    ui.available_width() - 70.0,
                ));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Find").clicked() || enter) && !self.query.trim().is_empty() {
                    let selector = self.query.trim().to_string();
                    self.ask(json!({ "op": "query", "selector": selector }), |r| Reply::Query(parse(r)));
                }
            });
            if let Some(results) = self.query_results.clone() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} match(es)", results.len())).color(p.muted));
                    if ui.small_button("Close").clicked() {
                        self.query_results = None;
                    }
                });
                egui::ScrollArea::vertical().id_salt("query-results").max_height(160.0).show(ui, |ui| {
                    for n in results.iter() {
                        let r = self.node_label(ui, n, false);
                        if r.clicked() {
                            let id = n.id;
                            self.ask(json!({ "op": "reveal", "id": id }), move |r| Reply::Reveal(id, parse(r)));
                        }
                        r.context_menu(|ui| self.node_menu(ui, n, acts));
                    }
                });
                ui.separator();
            }
            let mut hovered = None;
            egui::ScrollArea::both().id_salt("tree").auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                match self.root.clone() {
                    Some(root) => self.tree(ui, &root, 0, &mut hovered, acts),
                    None if self.error.is_none() => {
                        ui.spinner();
                    }
                    None => {}
                }
            });
            if hovered != self.hovered {
                self.hovered = hovered;
                let id = hovered.or(self.selected);
                self.tell(json!({ "op": "highlight", "id": id }));
            }
        });
    }

    fn tree(&mut self, ui: &mut egui::Ui, n: &NodeSummary, depth: usize, hovered: &mut Option<u64>, acts: &mut Actions) {
        let open = self.expanded.contains(&n.id);
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 14.0);
            if n.children > 0 {
                if icon_button(ui, if open { "▾" } else { "▸" }, if open { "Collapse" } else { "Expand" }).clicked() {
                    if open {
                        self.expanded.remove(&n.id);
                    } else {
                        self.expanded.insert(n.id);
                        if !self.children.contains_key(&n.id) {
                            self.load_children(n.id);
                        }
                    }
                }
            } else {
                ui.add_space(26.0);
            }
            let selected = self.selected == Some(n.id);
            let r = self.node_label(ui, n, selected);
            if selected && self.scroll_to_selected {
                r.scroll_to_me(Some(Align::Center));
                self.scroll_to_selected = false;
            }
            if r.hovered() || r.context_menu_opened() {
                *hovered = Some(n.id);
            }
            if r.clicked() {
                self.select(n.id);
            }
            r.context_menu(|ui| self.node_menu(ui, n, acts));
        });
        if open {
            match self.children.get(&n.id).cloned() {
                Some(kids) => {
                    for k in kids.iter() {
                        self.tree(ui, k, depth + 1, hovered, acts);
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.add_space((depth + 1) as f32 * 14.0 + 26.0);
                        ui.spinner();
                    });
                }
            }
        }
    }

    fn node_label(&self, ui: &mut egui::Ui, n: &NodeSummary, selected: bool) -> egui::Response {
        let p = theme::palette(ui);
        let mut job = egui::text::LayoutJob::default();
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let fmt = |color| egui::TextFormat { font_id: mono.clone(), color, ..Default::default() };
        match n.kind.as_str() {
            "element" => {
                job.append("<", 0.0, fmt(p.muted));
                job.append(&n.tag, 0.0, fmt(p.accent_hover));
                if !n.label.is_empty() {
                    job.append(&format!(" {}", n.label), 0.0, fmt(p.warn));
                }
                job.append(">", 0.0, fmt(p.muted));
            }
            "shadow" => job.append("#shadow-root", 0.0, fmt(p.muted)),
            "comment" => job.append(&format!("<!-- {} -->", n.text), 0.0, fmt(p.success)),
            _ => job.append(&format!("\"{}\"", n.text), 0.0, fmt(p.text)),
        }
        let label = egui::Label::new(job).truncate().sense(Sense::click());
        let r = ui.add(label).on_hover_cursor(CursorIcon::PointingHand);
        if selected {
            ui.painter().rect_stroke(r.rect.expand(2.0), 4.0, Stroke::new(1.0, p.accent), egui::StrokeKind::Outside);
        }
        r
    }

    /// Text selected in the inspector's own labels (HTML, text, attributes...), if any.
    fn inspector_selection(&self, ui: &egui::Ui) -> Option<String> {
        let active = ui.ctx().plugin::<egui::text_selection::LabelSelectionState>().lock().has_selection();
        let text = self.selection.lock().ok()?.clone();
        (active && !text.is_empty()).then_some(text)
    }

    /// The context menu of an element in the tree (or in the search results).
    fn node_menu(&mut self, ui: &mut egui::Ui, n: &NodeSummary, acts: &mut Actions) {
        let element = n.kind == "element";
        let id = n.id;
        ui.set_min_width(220.0);
        if let Some(text) = self.inspector_selection(ui)
            && ui.button("Copy selected text").clicked()
        {
            ui.ctx().copy_text(text);
            acts.notice("Copied the selected text");
        }
        if element {
            if ui.button("Copy element (outer HTML)").clicked() {
                self.copy(id, "outerHtml", "element's HTML");
            }
            if ui.button("Copy inner HTML").clicked() {
                self.copy(id, "innerHtml", "inner HTML");
            }
        }
        if ui.button("Copy text").clicked() {
            self.copy(id, "text", "text");
        }
        if ui.button("Copy text selected on the page").on_hover_text("What you selected in the page, within this element").clicked() {
            self.copy(id, "selection", "selected text");
        }
        ui.menu_button("Copy path", |ui| {
            if ui.button("CSS selector").clicked() {
                self.copy(id, "selector", "CSS selector");
            }
            if ui.button("JS path").clicked() {
                self.copy(id, "jsPath", "JS path");
            }
            if ui.button("XPath").clicked() {
                self.copy(id, "xpath", "XPath");
            }
        });
        if element {
            ui.menu_button("Copy attributes", |ui| {
                if ui.button("All attributes").clicked() {
                    self.copy(id, "attributes", "attributes");
                }
                if let Some(d) = self.details.as_ref().filter(|d| d.node.id == id) {
                    ui.separator();
                    for (k, v) in &d.attributes {
                        if ui.button(k.as_str()).on_hover_text(v).clicked() {
                            ui.ctx().copy_text(v.clone());
                            acts.notice(format!("Copied the value of {k}"));
                        }
                    }
                } else {
                    ui.label(RichText::new("Select the element to copy single attributes").small().weak());
                }
            });
            if ui.button("Copy computed styles").clicked() {
                self.copy(id, "styles", "computed styles");
            }
        }
        if n.is_frame() {
            ui.separator();
            let src = n.src.clone();
            let web = src.starts_with("http");
            if ui.add_enabled(web, egui::Button::new("↗ Open frame in new tab")).on_hover_text(&src).clicked() {
                acts.open_page(src.clone());
            }
            if ui.add_enabled(!src.is_empty(), egui::Button::new("Copy frame URL")).clicked() {
                ui.ctx().copy_text(src);
                acts.notice("Copied the frame's URL");
            }
        }
        ui.separator();
        if ui.button("Select").clicked() {
            let id = n.id;
            self.ask(json!({ "op": "reveal", "id": id }), move |r| Reply::Reveal(id, parse(r)));
        }
        if ui.button("Scroll into view").clicked() {
            self.tell(json!({ "op": "scrollTo", "id": id }));
        }
        if element {
            if ui.button("📷 Save a screenshot").clicked() {
                self.screenshot(id, "download");
            }
            if ui.button("📷 Copy a screenshot").clicked() {
                self.screenshot(id, "copy");
            }
            ui.separator();
            if ui.button(RichText::new("🗑 Delete element").color(theme::palette(ui).danger)).clicked() {
                self.delete(id);
            }
        }
    }

    fn delete(&mut self, id: u64) {
        self.ask(json!({ "op": "remove", "id": id }), |r| Reply::Done(r.map(drop)));
        if self.selected == Some(id) {
            self.details = None;
            self.selected = None;
        }
        self.children.retain(|_, list| !list.iter().any(|n| n.id == id));
        self.confirm_delete = false;
    }

    /// Context menu of a text in the details panel: the selection, or the whole value.
    fn value_menu(&self, r: &egui::Response, items: impl FnOnce() -> Vec<(&'static str, String)>, acts: &mut Actions) {
        r.context_menu(|ui| {
            let items = items();
            if let Some(text) = self.inspector_selection(ui)
                && ui.button("Copy selected text").clicked()
            {
                ui.ctx().copy_text(text);
                acts.notice("Copied the selected text");
            }
            for (label, value) in items {
                if ui.button(label).clicked() {
                    ui.ctx().copy_text(value);
                    acts.notice("Copied");
                }
            }
        });
    }

    fn details_panel(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        let Some(d) = self.details.clone() else {
            empty_state(ui, "🎯", "Pick or click an element", "Its attributes, size, styles and HTML show here. Right-click for more.");
            return;
        };
        egui::ScrollArea::vertical().id_salt("details").auto_shrink([false, false]).show(ui, |ui| {
            // Breadcrumbs.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, n) in d.path.iter().enumerate() {
                    if i > 0 {
                        ui.label(RichText::new("›").color(p.muted));
                    }
                    let name = if n.kind == "element" { n.tag.clone() } else { n.kind.clone() };
                    let r = ui.link(RichText::new(name).small());
                    if r.clicked() {
                        self.select(n.id);
                    }
                    r.context_menu(|ui| self.node_menu(ui, n, acts));
                }
            });
            ui.add_space(6.0);
            if !d.selector.is_empty() {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::Label::new(RichText::new(&d.selector).monospace().small()).truncate().selectable(true));
                    self.value_menu(&r, || vec![("Copy CSS selector", d.selector.clone())], acts);
                    if icon_button(ui, "⧉", "Copy the CSS selector").clicked() {
                        ui.ctx().copy_text(d.selector.clone());
                    }
                });
            }
            if let Some(r) = &d.rect {
                ui.label(RichText::new(format!("{:.0} × {:.0} px at ({:.0}, {:.0})", r.width, r.height, r.x, r.y)).color(p.muted));
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button("Scroll into view").clicked() {
                    self.tell(json!({ "op": "scrollTo", "id": d.node.id }));
                }
                if ui.button("Copy HTML").clicked() {
                    ui.ctx().copy_text(d.outer_html.clone());
                    acts.notice("Copied the element's HTML");
                }
                if d.node.kind == "element" {
                    let shot = ui.button("📷 Screenshot").on_hover_text("Save a picture of the element (right-click: copy it)");
                    if shot.clicked() {
                        self.screenshot(d.node.id, "download");
                    }
                    shot.context_menu(|ui| {
                        if ui.button("Save as PNG").clicked() {
                            self.screenshot(d.node.id, "download");
                        }
                        if ui.button("Copy the picture").clicked() {
                            self.screenshot(d.node.id, "copy");
                        }
                    });
                }
                if d.node.is_frame() && d.node.src.starts_with("http") && ui.button("↗ Open frame").on_hover_text(&d.node.src).clicked() {
                    acts.open_page(d.node.src.clone());
                }
                let label = if self.confirm_delete { "Click again to delete" } else { "🗑 Delete element" };
                if ui.button(RichText::new(label).color(p.danger)).on_hover_text("Remove it from the page (until reload)").clicked() {
                    if self.confirm_delete {
                        self.delete(d.node.id);
                    } else {
                        self.confirm_delete = true;
                    }
                }
            });
            ui.add_space(6.0);
            if !d.attributes.is_empty() {
                egui::CollapsingHeader::new(RichText::new(format!("Attributes ({})", d.attributes.len())).strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::Grid::new("attrs").num_columns(2).striped(true).show(ui, |ui| {
                            for (k, v) in &d.attributes {
                                let items = || {
                                    vec![
                                        ("Copy value", v.clone()),
                                        ("Copy name", k.clone()),
                                        ("Copy as name=\"value\"", format!("{k}=\"{v}\"")),
                                    ]
                                };
                                let r = ui.add(egui::Label::new(RichText::new(k).monospace().color(p.warn)).sense(Sense::click()));
                                self.value_menu(&r, items, acts);
                                let r = ui.add(egui::Label::new(RichText::new(v).monospace()).wrap().selectable(true));
                                self.value_menu(&r, items, acts);
                                ui.end_row();
                            }
                        });
                    });
            }
            if !d.style.is_empty() {
                egui::CollapsingHeader::new(RichText::new("Computed style").strong()).show(ui, |ui| {
                    ui.add(text_input(&mut self.style_filter, "filter, e.g. display or color", 220.0));
                    let f = self.style_filter.to_lowercase();
                    egui::ScrollArea::vertical().id_salt("style").max_height(260.0).show(ui, |ui| {
                        egui::Grid::new("style").num_columns(2).show(ui, |ui| {
                            for (k, v) in d.style.iter().filter(|(k, v)| f.is_empty() || k.contains(&f) || v.to_lowercase().contains(&f)) {
                                let items = || {
                                    vec![
                                        ("Copy value", v.clone()),
                                        ("Copy property", k.clone()),
                                        ("Copy declaration", format!("{k}: {v};")),
                                    ]
                                };
                                let r = ui.add(
                                    egui::Label::new(RichText::new(k).monospace().small().color(p.accent_hover)).sense(Sense::click()),
                                );
                                self.value_menu(&r, items, acts);
                                let r = ui.add(egui::Label::new(RichText::new(v).monospace().small()).selectable(true));
                                self.value_menu(&r, items, acts);
                                ui.end_row();
                            }
                        });
                    });
                });
            }
            if !d.text.trim().is_empty() {
                egui::CollapsingHeader::new(RichText::new("Text").strong()).show(ui, |ui| {
                    let r = ui.add(egui::Label::new(preview(&d.text)).wrap().selectable(true));
                    self.value_menu(&r, || vec![("Copy all text", d.text.clone())], acts);
                });
            }
            egui::CollapsingHeader::new(RichText::new("HTML").strong()).default_open(true).show(ui, |ui| {
                egui::Frame::new().fill(p.input).corner_radius(6).inner_margin(Margin::same(8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let r = ui.add(egui::Label::new(RichText::new(preview(&d.outer_html)).monospace().small()).wrap().selectable(true));
                    self.value_menu(&r, || vec![("Copy all HTML", d.outer_html.clone())], acts);
                });
            });
        });
    }

    // ------------------------------------------------------------ media & frames

    fn media(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        let Some(list) = self.resources.clone() else {
            ui.spinner();
            return;
        };
        ui.horizontal(|ui| {
            ui.add(text_input(&mut self.res_query, "Search URLs, types and frames…", ui.available_width() - 160.0));
            ui.label(RichText::new(format!("{} found", list.len())).color(p.muted));
        });
        ui.label(
            RichText::new("From every frame of the page. A blob: video plays a stream: its real URLs are under Media requests.")
                .small()
                .color(p.muted),
        );
        ui.add_space(6.0);
        let groups = match &self.res_groups {
            Some((q, groups)) if *q == self.res_query => groups.clone(),
            _ => {
                let q = self.res_query.to_lowercase();
                let mut groups = ResourceGroups::new();
                for (i, r) in list.iter().enumerate() {
                    let hay = format!("{} {} {} {}", r.kind, r.url, r.detail, r.frame).to_lowercase();
                    if !q.is_empty() && !q.split_whitespace().all(|t| hay.contains(t)) {
                        continue;
                    }
                    let order = KINDS.iter().position(|(k, _)| *k == r.kind).unwrap_or(KINDS.len());
                    groups.entry(order).or_default().push(i);
                }
                let groups = Rc::new(groups);
                self.res_groups = Some((self.res_query.clone(), groups.clone()));
                groups
            }
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if groups.is_empty() {
                let (title, hint) = if list.is_empty() {
                    ("Nothing found", "No videos, frames or scripts on this page.")
                } else {
                    ("No matches", "Try another search.")
                };
                empty_state(ui, "🔍", title, hint);
            }
            for (order, items) in groups.iter() {
                let label = KINDS.get(*order).map_or("Other", |(_, l)| *l);
                egui::CollapsingHeader::new(RichText::new(label).strong())
                    .id_salt(("kind", *order))
                    .default_open(true)
                    .show(ui, |ui| {
                        for &i in items {
                            self.resource_row(ui, &list[i], acts);
                        }
                    })
                    .header_response
                    .on_hover_text(format!("{} item(s)", items.len()));
                ui.add_space(2.0);
            }
        });
    }

    fn resource_row(&self, ui: &mut egui::Ui, r: &PageResource, acts: &mut Actions) {
        let p = theme::palette(ui);
        egui::Frame::new()
            .fill(p.surface)
            .corner_radius(8)
            .inner_margin(Margin::symmetric(10, 6))
            .outer_margin(Margin { left: 0, right: 0, top: 0, bottom: 4 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if r.kind == "script" && r.url.starts_with("http") {
                            let pattern = r.url.split(['?', '#']).next().unwrap_or(&r.url).to_string();
                            let site = normalize_site(&self.url);
                            if ui
                                .button(RichText::new("⛔ Block").color(p.danger))
                                .on_hover_text(format!("Add a rule blocking {pattern} on {site}"))
                                .clicked()
                            {
                                let rule = BlockRule {
                                    enabled: true,
                                    kind: BlockRuleKind::Script,
                                    pattern,
                                    sites: vec![site],
                                    ..Default::default()
                                };
                                acts.send(Command::SaveBlockRule { rule });
                            }
                        }
                        if r.url.starts_with("http") && icon_button(ui, "↗", "Open in a new tab").clicked() {
                            acts.open_page(r.url.clone());
                        }
                        if icon_button(ui, "⧉", "Copy the URL").clicked() {
                            ui.ctx().copy_text(r.url.clone());
                            acts.notice("URL copied");
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let l = ui
                                .add(egui::Label::new(RichText::new(&r.url).monospace().small()).truncate().selectable(true))
                                .on_hover_text(&r.url);
                            self.url_menu(&l, &r.url, acts);
                        });
                    });
                });
                let mut meta = Vec::new();
                if !r.detail.is_empty() {
                    meta.push(r.detail.clone());
                }
                if !r.top {
                    meta.push(format!("in frame {}", crate::url::display_host(&r.frame)));
                }
                if !meta.is_empty() {
                    ui.horizontal(|ui| {
                        badge(ui, &r.kind, false);
                        ui.label(RichText::new(meta.join(" · ")).small().color(p.muted));
                    });
                }
            });
    }

    fn url_menu(&self, r: &egui::Response, url: &str, acts: &mut Actions) {
        r.context_menu(|ui| {
            if let Some(text) = self.inspector_selection(ui)
                && ui.button("Copy selected text").clicked()
            {
                ui.ctx().copy_text(text);
                acts.notice("Copied the selected text");
            }
            if ui.button("Copy URL").clicked() {
                ui.ctx().copy_text(url.to_string());
                acts.notice("URL copied");
            }
            if url.starts_with("http") && ui.button("↗ Open in a new tab").clicked() {
                acts.open_page(url.to_string());
            }
        });
    }

    // ------------------------------------------------------------ ads

    fn ads(&mut self, ui: &mut egui::Ui, acts: &mut Actions, state: Option<&AppState>) {
        let p = theme::palette(ui);
        let Some(found) = self.ads.clone() else {
            ui.spinner();
            return;
        };
        let site = crate::url::registrable_domain(&crate::url::host(&self.url));
        let key = (self.url.clone(), state.map(|s| s.revision));
        let hits = match &self.ad_hits {
            Some((k, hits)) if *k == key => hits.clone(),
            _ => {
                // Tabs this site opened (from the log of tabs opened by pages).
                let mut signals = (*found).clone();
                if let Some(s) = state {
                    for e in &s.popup_log {
                        if !site.is_empty() && crate::url::registrable_domain(&crate::url::host(&e.opener)) == site {
                            let detail = if e.closed_by.is_some() { "closed by a rule" } else { "" };
                            signals.push(AdSignal {
                                kind: "popup".into(),
                                url: e.url.clone(),
                                detail: detail.into(),
                                frame: e.opener.clone(),
                            });
                        }
                    }
                }
                let hits = Rc::new(ads::providers(&self.url, &signals));
                self.ad_hits = Some((key, hits.clone()));
                hits
            }
        };
        ui.label(
            RichText::new(
                "Ad networks this page uses, from the scripts, frames and requests of all its frames, the ad code written \
                 into it (inline scripts and encoded tags), its ad slots, and the tabs it opened. Block a network on this \
                 site, or everywhere.",
            )
            .small()
            .color(p.muted),
        );
        ui.add_space(6.0);
        let rules = state.map(|s| s.block_rules.as_slice()).unwrap_or_default();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if hits.is_empty() {
                empty_state(
                    ui,
                    "✨",
                    "No ads found",
                    "No known ad network is named in this page's scripts, frames, requests or inline code. Ads that load a \
                     little later: ⟳ Refresh reads the page again.",
                );
            }
            let mut category = None;
            for h in hits.iter() {
                let cat = if h.signals.iter().any(|s| s.kind == "popup") { AdCategory::Popunder } else { h.category };
                if category != Some(cat) {
                    category = Some(cat);
                    ui.add_space(4.0);
                    ui.label(RichText::new(cat.label()).strong().color(p.muted));
                }
                self.provider_card(ui, h, &site, rules, acts);
            }
        });
    }

    fn provider_card(&mut self, ui: &mut egui::Ui, h: &ProviderHit, site: &str, rules: &[BlockRule], acts: &mut Actions) {
        let p = theme::palette(ui);
        let has_rule = |kind: BlockRuleKind, pattern: &str, sites: &[String]| {
            rules.iter().any(|r| r.enabled && r.kind == kind && r.pattern == pattern && (r.sites.is_empty() || r.sites == sites))
        };
        egui::Frame::new()
            .fill(p.surface)
            .corner_radius(8)
            .inner_margin(Margin::symmetric(10, 8))
            .outer_margin(Margin { left: 0, right: 0, top: 0, bottom: 6 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&h.name).strong());
                    if h.category == AdCategory::Suspected {
                        badge(ui, "by its name", false);
                    } else if !h.known {
                        // An unnamed network: say what gave its host away.
                        let why = if h.signals.iter().any(|s| s.kind == "inline") {
                            "from page code"
                        } else if h.signals.iter().any(|s| matches!(s.kind.as_str(), "script" | "iframe" | "request")) {
                            "ad tag"
                        } else {
                            "unknown network"
                        };
                        badge(ui, why, false);
                    }
                    ui.label(RichText::new(h.summary()).small().color(p.muted));
                });
                if !h.domains.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        let domains: Vec<String> = h.domains.iter().cloned().collect();
                        ui.label(RichText::new(domains.join(", ")).monospace().small());
                        let here = vec![site.to_string()];
                        let blocked_here = domains.iter().all(|d| has_rule(BlockRuleKind::Script, d, &here));
                        if blocked_here {
                            ui.label(RichText::new("✔ scripts blocked").small().color(p.success));
                        } else {
                            if !site.is_empty()
                                && ui
                                    .button(RichText::new("⛔ Block on this site").color(p.danger))
                                    .on_hover_text(format!("Rules blocking scripts from {} on {site}", domains.join(", ")))
                                    .clicked()
                            {
                                for d in &domains {
                                    let rule = BlockRule {
                                        enabled: true,
                                        kind: BlockRuleKind::Script,
                                        pattern: d.clone(),
                                        sites: here.clone(),
                                        ..Default::default()
                                    };
                                    acts.send(Command::SaveBlockRule { rule });
                                }
                            }
                            if ui
                                .button("Block everywhere")
                                .on_hover_text(format!("Rules blocking scripts from {} on every site", domains.join(", ")))
                                .clicked()
                            {
                                for d in &domains {
                                    let rule =
                                        BlockRule { enabled: true, kind: BlockRuleKind::Script, pattern: d.clone(), ..Default::default() };
                                    acts.send(Command::SaveBlockRule { rule });
                                }
                            }
                        }
                        let opens_tabs = h.category == AdCategory::Popunder || h.signals.iter().any(|s| s.kind == "popup");
                        if opens_tabs
                            && !domains.iter().all(|d| has_rule(BlockRuleKind::PopupTo, d, &[]))
                            && ui.button("Close tabs it opens").clicked()
                        {
                            for d in &domains {
                                let rule =
                                    BlockRule { enabled: true, kind: BlockRuleKind::PopupTo, pattern: d.clone(), ..Default::default() };
                                acts.send(Command::SaveBlockRule { rule });
                            }
                        }
                    });
                }
                egui::CollapsingHeader::new(RichText::new("What gave it away").small()).id_salt(("ad", &h.name)).show(ui, |ui| {
                    for s in &h.signals {
                        ui.horizontal(|ui| {
                            badge(
                                ui,
                                if s.kind == "popup" {
                                    "tab"
                                } else if s.kind == "inline" {
                                    "code"
                                } else {
                                    &s.kind
                                },
                                s.kind == "popup",
                            );
                            if s.kind == "slot" {
                                let selector = s.detail.split(" × ").next().unwrap_or("").to_string();
                                ui.label(RichText::new(&s.detail).monospace().small());
                                if s.frame == self.url && ui.small_button("🎯 Show").on_hover_text("Select it in the elements").clicked()
                                {
                                    self.query = selector.clone();
                                    self.select_first_match = true;
                                    self.ask(json!({ "op": "query", "selector": selector }), |r| Reply::Query(parse(r)));
                                }
                            } else if s.kind == "inline" {
                                // The ad code itself: a line of it here, all of it on hover.
                                let one_line = s.detail.split_whitespace().collect::<Vec<_>>().join(" ");
                                let excerpt: String = one_line.chars().take(160).collect();
                                let full: String = s.detail.chars().take(4000).collect();
                                ui.add(egui::Label::new(RichText::new(excerpt).monospace().small()).truncate().selectable(true))
                                    .on_hover_text(full);
                            } else {
                                let l = ui
                                    .add(egui::Label::new(RichText::new(&s.url).monospace().small()).truncate().selectable(true))
                                    .on_hover_text(format!("{}\n{}", s.url, s.detail));
                                self.url_menu(&l, &s.url, acts);
                            }
                        });
                    }
                });
            });
    }

    // ------------------------------------------------------------ source

    fn source(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        let Some(lines) = self.source.clone() else {
            ui.spinner();
            return;
        };
        let q = self.source_query.to_lowercase();
        let shown = match &self.source_shown {
            Some((k, shown)) if *k == self.source_query => shown.clone(),
            _ => {
                let shown: Vec<usize> = if q.is_empty() {
                    (0..lines.len()).collect()
                } else {
                    lines.iter().enumerate().filter(|(_, l)| l.to_lowercase().contains(&q)).map(|(i, _)| i).collect()
                };
                let shown = Rc::new(shown);
                self.source_shown = Some((self.source_query.clone(), shown.clone()));
                shown
            }
        };
        ui.horizontal(|ui| {
            ui.add(text_input(&mut self.source_query, "Find in the page source…", ui.available_width() - 260.0));
            let what = if q.is_empty() { format!("{} lines", lines.len()) } else { format!("{} matching lines", shown.len()) };
            ui.label(RichText::new(what).color(p.muted));
            if ui.button("Copy all").clicked() {
                ui.ctx().copy_text(lines.join("\n"));
                acts.notice("Page source copied");
            }
        });
        ui.label(RichText::new("The page as it is now (after its scripts ran).").small().color(p.muted));
        ui.add_space(4.0);
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
        egui::Frame::new().fill(p.input).corner_radius(6).inner_margin(Margin::same(8)).show(ui, |ui| {
            egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_height, shown.len(), |ui, range| {
                for &i in &shown[range] {
                    ui.horizontal(|ui| {
                        ui.add_sized([48.0, row_height], egui::Label::new(RichText::new(format!("{}", i + 1)).monospace().color(p.muted)));
                        let r = ui.add(egui::Label::new(RichText::new(&lines[i]).monospace()).extend().selectable(true));
                        self.value_menu(&r, || vec![("Copy line", lines[i].clone())], acts);
                    });
                }
            });
        });
    }
}
