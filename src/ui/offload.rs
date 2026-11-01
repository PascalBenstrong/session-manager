//! Work done on the page's worker thread (see [`crate::worker`]): tab search, reading
//! imports and fixing up recordings. Each falls back to running on the page while the
//! worker is starting, or where it can't run.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use eframe::egui;
use wasm_bindgen_futures::spawn_local;

use crate::dedupe::Scope;
use crate::model::{AppState, Id};
use crate::platform::{self as pf, WorkOut};
use crate::search::{self, Query, SearchOptions, SearchRequest};
use crate::transfer::{self, Imported};
use crate::webm;

/// `idle:` searches are run again this often (they depend on the time).
const IDLE_STEP_MS: f64 = 10_000.0;

thread_local! {
    /// The revision of the state the worker has (the page has one state, and one worker).
    static SENT_REVISION: Cell<Option<u64>> = const { Cell::new(None) };
}

async fn work_text(op: &str, text: &str) -> pf::Result<String> {
    match pf::work(op, text, &[]).await? {
        WorkOut::Text(s) => Ok(s),
        WorkOut::Bytes(_) => Err(format!("{op}: unexpected bytes")),
    }
}

fn from_json<T: serde::de::DeserializeOwned>(op: &str, s: &str) -> pf::Result<T> {
    serde_json::from_str(s).map_err(|e| format!("{op}: {e}"))
}

// ------------------------------------------------------------ recordings

/// Writes the duration into a WebM head (see [`webm::set_duration`]), off the page.
pub async fn set_duration(head: &[u8], ms: f64) -> Result<Vec<u8>, String> {
    match pf::work("webm-duration", &ms.to_string(), head).await {
        Ok(WorkOut::Bytes(fixed)) => Ok(fixed),
        Ok(WorkOut::Text(e)) => Err(e),
        Err(_) => webm::set_duration(head, ms),
    }
}

// ------------------------------------------------------------ imports

/// Detects and reads an import (see [`transfer::parse_import`]), off the page.
pub async fn parse_import(text: String) -> Result<Imported, String> {
    match work_text("import", &text).await.and_then(|out| from_json::<Result<Imported, String>>("import", &out)) {
        Ok(parsed) => parsed,
        Err(_) => transfer::parse_import(&text),
    }
}

// ------------------------------------------------------------ search

/// The entries a search lets through.
pub enum Hits<'a> {
    /// No search: every entry.
    All,
    Only(&'a HashSet<Id>),
}

impl Hits<'_> {
    pub fn contains(&self, id: Id) -> bool {
        match self {
            Hits::All => true,
            Hits::Only(ids) => ids.contains(&id),
        }
    }
}

type Answer = Rc<RefCell<Option<(SearchRequest, pf::Result<Vec<Id>>)>>>;

/// A view's tab search. The worker runs it (when the query or the tabs change, not on
/// every frame); until it answers, the previous results stay up.
#[derive(Default)]
pub struct Search {
    /// The last query parsed (to show its error and decide what to run).
    parsed: Option<(String, SearchOptions, Result<Query, String>)>,
    /// What the results shown are for.
    shown: Option<SearchRequest>,
    ids: HashSet<Id>,
    /// Sent to the worker, not answered yet.
    asked: Option<SearchRequest>,
    answer: Answer,
    /// Never use the worker (pages that close too soon for it to pay off).
    local: bool,
}

impl Search {
    /// A search that runs on the page (still only when the query or the tabs change).
    pub fn local() -> Self {
        Self { local: true, ..Default::default() }
    }

    /// The entries (of any session) matching `query`, and the query's error if it has one.
    /// `duplicates`: where `is:duplicate` looks.
    pub fn run(
        &mut self,
        ctx: &egui::Context,
        state: &AppState,
        query: &str,
        opts: &SearchOptions,
        duplicates: Option<Scope>,
    ) -> (Hits<'_>, Option<String>) {
        if self.parsed.as_ref().is_none_or(|(q, o, _)| q != query || o != opts) {
            self.parsed = Some((query.to_string(), opts.clone(), Query::parse(query, opts)));
        }
        let (now, error) = match &self.parsed {
            Some((_, _, Ok(q))) if q.is_empty() => return (Hits::All, None),
            Some((_, _, Ok(q))) => (if q.uses_time() { (pf::now() / IDLE_STEP_MS).floor() * IDLE_STEP_MS } else { 0.0 }, None),
            // As before: a query that doesn't parse filters nothing.
            Some((_, _, Err(e))) => return (Hits::All, Some(e.clone())),
            None => unreachable!(),
        };
        let req = SearchRequest { query: query.to_string(), opts: opts.clone(), duplicates, now, revision: state.revision };

        let answered = self.answer.borrow_mut().take();
        if let Some((asked, answer)) = answered {
            if self.asked.as_ref() == Some(&asked) {
                self.asked = None;
            }
            match answer {
                Ok(ids) => self.show(asked, ids.into_iter().collect()),
                // Didn't run on the worker (it was stopped, or failed): search here, and send
                // the state again next time.
                Err(_) => {
                    SENT_REVISION.set(None);
                    let ids = search::matching_ids(state, &asked).unwrap_or_default();
                    self.show(asked, ids);
                }
            }
        }
        if self.shown.as_ref() != Some(&req) && self.asked.as_ref() != Some(&req) {
            if !self.local && pf::worker_ready() {
                self.ask(ctx, state, req.clone());
            }
            if self.asked.as_ref() != Some(&req) || self.shown.is_none() {
                // No worker (yet), or nothing to show while it works: search here.
                let ids = search::matching_ids(state, &req).unwrap_or_default();
                self.show(req, ids);
            }
        }
        (Hits::Only(&self.ids), error)
    }

    fn show(&mut self, req: SearchRequest, ids: HashSet<Id>) {
        self.shown = Some(req);
        self.ids = ids;
    }

    fn ask(&mut self, ctx: &egui::Context, state: &AppState, req: SearchRequest) {
        // The worker runs jobs in order, so the state sent here is in place for the search.
        let state_json = (SENT_REVISION.get() != Some(state.revision)).then(|| serde_json::to_string(state).unwrap_or_default());
        SENT_REVISION.set(Some(state.revision));
        self.asked = Some(req.clone());
        let answer = self.answer.clone();
        let ctx = ctx.clone();
        spawn_local(async move {
            if let Some(json) = state_json
                && let Err(e) = work_text("state", &json).await
            {
                SENT_REVISION.set(None);
                *answer.borrow_mut() = Some((req, Err(e)));
                ctx.request_repaint();
                return;
            }
            let ids = match serde_json::to_string(&req) {
                Ok(text) => work_text("search", &text).await.and_then(|out| from_json::<Result<Vec<Id>, String>>("search", &out)),
                Err(e) => Err(e.to_string()),
            };
            // A query error was shown already (it was parsed here first).
            let ids = ids.map(|r| r.unwrap_or_default());
            *answer.borrow_mut() = Some((req, ids));
            ctx.request_repaint();
        });
    }
}
