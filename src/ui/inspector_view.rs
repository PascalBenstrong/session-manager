//! Inspector window (`inspector.html?tab=ID`): inspects a tab's page without the browser's
//! developer tools, through a content script in the extension's isolated world
//! (`web/inspect.js`). Panes: Elements (DOM tree, picker, details), Media & frames (video,
//! frame, script... URLs of every frame, grouped and searchable) and Source.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use eframe::egui::{self, Align, CursorIcon, Layout, Margin, RichText, Sense, Stroke};
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::theme;
use super::widgets::{badge, empty_state, icon_button, primary_button, segmented, text_input};
use super::Actions;
use crate::guards::{BlockRule, BlockRuleKind, normalize_site};
use crate::model::TabId;
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

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct NodeSummary {
    id: u64,
    kind: String,
    tag: String,
    label: String,
    children: usize,
    text: String,
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
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Picked {
    id: u64,
    path: Vec<u64>,
}

enum Reply {
    Ready(Result<Root, String>),
    Children(u64, Result<Vec<NodeSummary>, String>),
    Details(Result<Details, String>),
    Query(Result<Vec<NodeSummary>, String>),
    Source(Result<String, String>),
    Resources(Result<Vec<PageResource>, String>),
    Done(Result<(), String>),
}

#[derive(Default)]
struct Inbox {
    replies: Vec<Reply>,
    picked: Option<Picked>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Elements,
    Media,
    Source,
}

pub struct InspectorView {
    tab: Option<TabId>,
    ctx: egui::Context,
    inbox: Rc<RefCell<Inbox>>,
    pane: Pane,
    error: Option<String>,
    title: String,
    url: String,
    root: Option<NodeSummary>,
    children: HashMap<u64, Vec<NodeSummary>>,
    expanded: HashSet<u64>,
    selected: Option<u64>,
    details: Option<Details>,
    scroll_to_selected: bool,
    picking: bool,
    hovered: Option<u64>,
    query: String,
    query_results: Option<Vec<NodeSummary>>,
    style_filter: String,
    confirm_delete: bool,
    resources: Option<Vec<PageResource>>,
    res_query: String,
    source: Option<Vec<String>>,
    source_query: String,
}

fn parse<T: for<'de> Deserialize<'de>>(r: Result<Value, String>) -> Result<T, String> {
    r.and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
}

impl InspectorView {
    pub fn new(ctx: &egui::Context) -> Self {
        let tab = pf::location_param("tab").parse::<TabId>().ok();
        let inbox = Rc::new(RefCell::new(Inbox::default()));
        {
            let inbox = inbox.clone();
            let ctx = ctx.clone();
            let cb = Closure::<dyn FnMut(JsValue)>::new(move |v: JsValue| {
                if let Some(p) = v.as_string().and_then(|s| serde_json::from_str::<Picked>(&s).ok()) {
                    inbox.borrow_mut().picked = Some(p);
                    ctx.request_repaint();
                }
            });
            pf::on_inspector_pick(cb.as_ref().unchecked_ref());
            cb.forget();
        }
        let mut v = Self {
            tab,
            ctx: ctx.clone(),
            inbox,
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
            style_filter: String::new(),
            confirm_delete: false,
            resources: None,
            res_query: String::new(),
            source: None,
            source_query: String::new(),
        };
        v.connect();
        v
    }

    /// Injects the content script and loads the document root.
    fn connect(&mut self) {
        let Some(tab) = self.tab else { return };
        self.root = None;
        self.children.clear();
        self.expanded.clear();
        self.details = None;
        self.selected = None;
        self.error = None;
        self.run(async move {
            pf::inspector_inject(tab).await?;
            pf::inspect(tab, &json!({ "op": "root" })).await
        }, |r| Reply::Ready(parse(r)));
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

    fn load_children(&self, id: u64) {
        self.ask(json!({ "op": "children", "id": id }), move |r| Reply::Children(id, parse(r)));
    }

    fn select(&mut self, id: u64) {
        self.selected = Some(id);
        self.confirm_delete = false;
        self.ask(json!({ "op": "details", "id": id }), |r| Reply::Details(parse(r)));
        self.ask(json!({ "op": "scrollTo", "id": id }), |r| Reply::Done(r.map(drop)));
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

    fn drain(&mut self) {
        let (replies, picked) = {
            let mut inbox = self.inbox.borrow_mut();
            (std::mem::take(&mut inbox.replies), inbox.picked.take())
        };
        for r in replies {
            match r {
                Reply::Ready(Ok(root)) => {
                    self.title = root.title;
                    self.url = root.url;
                    self.expanded.insert(root.node.id);
                    self.load_children(root.node.id);
                    self.root = Some(root.node);
                }
                Reply::Children(id, Ok(list)) => {
                    // Open <body> by default.
                    for n in list.iter().filter(|n| n.tag == "body") {
                        if self.expanded.insert(n.id) {
                            self.load_children(n.id);
                        }
                    }
                    self.children.insert(id, list);
                }
                Reply::Details(Ok(d)) => self.details = Some(d),
                Reply::Query(Ok(list)) => self.query_results = Some(list),
                Reply::Source(Ok(s)) => self.source = Some(s.lines().map(String::from).collect()),
                Reply::Resources(Ok(list)) => self.resources = Some(list),
                Reply::Done(Ok(())) => {}
                Reply::Ready(Err(e))
                | Reply::Children(_, Err(e))
                | Reply::Details(Err(e))
                | Reply::Query(Err(e))
                | Reply::Source(Err(e))
                | Reply::Resources(Err(e))
                | Reply::Done(Err(e)) => self.error = Some(e),
            }
        }
        if let Some(p) = picked {
            self.picking = false;
            self.pane = Pane::Elements;
            // Open every ancestor, loading what isn't loaded yet.
            for id in p.path.iter().filter(|id| **id != p.id) {
                self.expanded.insert(*id);
                if !self.children.contains_key(id) {
                    self.load_children(*id);
                }
            }
            self.select(p.id);
            self.scroll_to_selected = true;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        self.drain();
        let p = theme::palette(ui);
        egui::Panel::top("inspector-header")
            .frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, 8)))
            .show(ui, |ui| self.header(ui));
        if let Some(e) = self.error.clone() {
            egui::Panel::bottom("inspector-error")
                .frame(egui::Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(12, 6)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("⚠ {e}")).color(p.danger));
                        if ui.button("Retry").clicked() {
                            self.connect();
                        }
                        if icon_button(ui, "🗙", "Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                });
        }
        match self.pane {
            Pane::Elements => self.elements(ui),
            Pane::Media => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(12)))
                    .show(ui, |ui| self.media(ui, acts));
            }
            Pane::Source => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(12)))
                    .show(ui, |ui| self.source(ui, acts));
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
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
        ui.horizontal(|ui| {
            let before = self.pane;
            segmented(ui, &mut self.pane, &[(Pane::Elements, "Elements"), (Pane::Media, "Media & frames"), (Pane::Source, "Source")]);
            if self.pane != before {
                match self.pane {
                    Pane::Media if self.resources.is_none() => self.load_resources(),
                    Pane::Source if self.source.is_none() => self.ask(json!({ "op": "source" }), |r| Reply::Source(parse(r))),
                    _ => {}
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("⟳ Refresh").on_hover_text("Read the page again").clicked() {
                    match self.pane {
                        Pane::Elements => self.connect(),
                        Pane::Media => self.load_resources(),
                        Pane::Source => self.ask(json!({ "op": "source" }), |r| Reply::Source(parse(r))),
                    }
                }
                if self.pane == Pane::Elements {
                    let label = if self.picking { "Picking… (Esc cancels)" } else { "🎯 Pick an element" };
                    if ui
                        .add(primary_button(ui, label))
                        .on_hover_text("Then click an element in the page")
                        .clicked()
                    {
                        if self.picking {
                            self.picking = false;
                            self.ask(json!({ "op": "cancelPick" }), |r| Reply::Done(r.map(drop)));
                        } else {
                            self.picking = true;
                            self.ask(json!({ "op": "pick" }), |r| Reply::Done(r.map(drop)));
                        }
                    }
                }
            });
        });
    }

    // ------------------------------------------------------------ elements

    fn elements(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        egui::Panel::right("details")
            .resizable(true)
            .default_size(440.0)
            .frame(egui::Frame::new().fill(p.surface).inner_margin(Margin::same(12)))
            .show(ui, |ui| self.details_panel(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(10)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let r = ui.add(text_input(&mut self.query, "Find by CSS selector, e.g. video, iframe[src*=ads], .popup", ui.available_width() - 70.0));
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
                        for n in &results {
                            if self.node_label(ui, n, false).clicked() {
                                self.select(n.id);
                            }
                        }
                    });
                    ui.separator();
                }
                let mut hovered = None;
                egui::ScrollArea::both().id_salt("tree").auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    match self.root.clone() {
                        Some(root) => self.tree(ui, &root, 0, &mut hovered),
                        None if self.error.is_none() => {
                            ui.spinner();
                        }
                        None => {}
                    }
                });
                if hovered != self.hovered {
                    self.hovered = hovered;
                    let id = hovered.or(self.selected);
                    self.ask(json!({ "op": "highlight", "id": id }), |r| Reply::Done(r.map(drop)));
                }
            });
    }

    fn tree(&mut self, ui: &mut egui::Ui, n: &NodeSummary, depth: usize, hovered: &mut Option<u64>) {
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
            if r.hovered() {
                *hovered = Some(n.id);
            }
            if r.clicked() {
                self.select(n.id);
            }
        });
        if open {
            match self.children.get(&n.id).cloned() {
                Some(kids) => {
                    for k in &kids {
                        self.tree(ui, k, depth + 1, hovered);
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

    fn details_panel(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let Some(d) = self.details.clone() else {
            empty_state(ui, "🎯", "Pick or click an element", "Its attributes, size, styles and HTML show here.");
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
                    if ui.link(RichText::new(name).small()).clicked() {
                        self.select(n.id);
                    }
                }
            });
            ui.add_space(6.0);
            if !d.selector.is_empty() {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&d.selector).monospace().small()).truncate().selectable(true));
                    if icon_button(ui, "⧉", "Copy the CSS selector").clicked() {
                        ui.ctx().copy_text(d.selector.clone());
                    }
                });
            }
            if let Some(r) = &d.rect {
                ui.label(
                    RichText::new(format!("{:.0} × {:.0} px at ({:.0}, {:.0})", r.width, r.height, r.x, r.y)).color(p.muted),
                );
            }
            ui.horizontal(|ui| {
                if ui.button("Scroll into view").clicked() {
                    self.ask(json!({ "op": "scrollTo", "id": d.node.id }), |r| Reply::Done(r.map(drop)));
                }
                if ui.button("Copy HTML").clicked() {
                    ui.ctx().copy_text(d.outer_html.clone());
                }
                let label = if self.confirm_delete { "Click again to delete" } else { "🗑 Delete element" };
                if ui.button(RichText::new(label).color(p.danger)).on_hover_text("Remove it from the page (until reload)").clicked() {
                    if self.confirm_delete {
                        let id = d.node.id;
                        self.ask(json!({ "op": "remove", "id": id }), |r| Reply::Done(r.map(drop)));
                        self.details = None;
                        self.selected = None;
                        self.children.retain(|_, list| !list.iter().any(|n| n.id == id));
                        self.confirm_delete = false;
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
                                ui.label(RichText::new(k).monospace().color(p.warn));
                                ui.add(egui::Label::new(RichText::new(v).monospace()).wrap().selectable(true));
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
                                ui.label(RichText::new(k).monospace().small().color(p.accent_hover));
                                ui.add(egui::Label::new(RichText::new(v).monospace().small()).selectable(true));
                                ui.end_row();
                            }
                        });
                    });
                });
            }
            if !d.text.trim().is_empty() {
                egui::CollapsingHeader::new(RichText::new("Text").strong()).show(ui, |ui| {
                    ui.add(egui::Label::new(&d.text).wrap().selectable(true));
                });
            }
            egui::CollapsingHeader::new(RichText::new("HTML").strong()).default_open(true).show(ui, |ui| {
                egui::Frame::new().fill(p.input).corner_radius(6).inner_margin(Margin::same(8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(&d.outer_html).monospace().small()).wrap().selectable(true));
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
            RichText::new(
                "From every frame of the page. A blob: video plays a stream: its real URLs are under Media requests.",
            )
            .small()
            .color(p.muted),
        );
        ui.add_space(6.0);
        let q = self.res_query.to_lowercase();
        let mut groups: BTreeMap<usize, Vec<&PageResource>> = BTreeMap::new();
        for r in &list {
            let hay = format!("{} {} {} {}", r.kind, r.url, r.detail, r.frame).to_lowercase();
            if !q.is_empty() && !q.split_whitespace().all(|t| hay.contains(t)) {
                continue;
            }
            let order = KINDS.iter().position(|(k, _)| *k == r.kind).unwrap_or(KINDS.len());
            groups.entry(order).or_default().push(r);
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if groups.is_empty() {
                let (title, hint) = if list.is_empty() {
                    ("Nothing found", "No videos, frames or scripts on this page.")
                } else {
                    ("No matches", "Try another search.")
                };
                empty_state(ui, "🔍", title, hint);
            }
            for (order, items) in &groups {
                let label = KINDS.get(*order).map_or("Other", |(_, l)| *l);
                egui::CollapsingHeader::new(RichText::new(label).strong())
                    .id_salt(("kind", *order))
                    .default_open(true)
                    .show(ui, |ui| {
                        for r in items {
                            self.resource_row(ui, r, acts);
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
                                let rule = BlockRule { enabled: true, kind: BlockRuleKind::Script, pattern, sites: vec![site], ..Default::default() };
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
                            ui.add(egui::Label::new(RichText::new(&r.url).monospace().small()).truncate().selectable(true))
                                .on_hover_text(&r.url);
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

    // ------------------------------------------------------------ source

    fn source(&mut self, ui: &mut egui::Ui, acts: &mut Actions) {
        let p = theme::palette(ui);
        let Some(lines) = self.source.clone() else {
            ui.spinner();
            return;
        };
        let q = self.source_query.to_lowercase();
        let shown: Vec<usize> = if q.is_empty() {
            (0..lines.len()).collect()
        } else {
            lines.iter().enumerate().filter(|(_, l)| l.to_lowercase().contains(&q)).map(|(i, _)| i).collect()
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
                        ui.add(egui::Label::new(RichText::new(&lines[i]).monospace()).extend().selectable(true));
                    });
                }
            });
        });
    }
}
