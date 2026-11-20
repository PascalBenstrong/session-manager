//! Background script: owns the [`AppState`], reacts to browser events and executes
//! commands sent by the UI. All state transitions are synchronous (see `state.rs`);
//! only browser calls are async, so no `RefCell` borrow is held across an `.await`.

use std::cell::{Cell, RefCell};

use js_sys::Promise;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

use crate::ai::{self, VectorHit, VectorIndex};
use crate::cleanup::{self, ALARM_PREFIX, CleanupReport, CleanupSpec, CleanupTarget, Cookie, SpecMatcher};
use crate::dedupe::Scope;
use crate::guards::{self, BlockRule, GuardSettings, PopupEvent};
use crate::hibernate::{self, HibernateTarget};
use crate::media::MediaReport;
use crate::model::{AppState, BrowserTab, EntryRef, Id, Snapshot, TabId};
use crate::organize::{self, OrganizeKey};
use crate::page_state;
use crate::persist::{self, Source};
use crate::platform::{self as pf, CreateTab};
use crate::protocol::{CHANGED_KEY, Capabilities, Command, OpenTabs, Response, SNAPSHOTS_KEY, SPLIT_VERSION, STATE_KEY, session_key};
use crate::state::{RestoreMode, trackable};
use crate::tab_strip;

const SNAPSHOT_ALARM: &str = "sm-snapshot";
/// After browser startup, de-duplicate once the browser has stopped restoring tabs for
/// this long...
const STARTUP_SETTLE_MS: u32 = 3000;
/// ...or at the latest this long after startup.
const STARTUP_MAX_WAIT_MS: f64 = 30_000.0;

thread_local! {
    static STATE: RefCell<AppState> = RefCell::new(AppState::default());
    /// The last revision saved (it only goes up, also when the state is reset or replaced).
    static REVISION: Cell<u64> = const { Cell::new(0) };
    static CAPS: RefCell<Capabilities> = RefCell::new(Capabilities::default());
    static ORGANIZE_GENERATION: Cell<u32> = const { Cell::new(0) };
    /// > 0 while we open tabs ourselves; "prevent duplicates" must not touch those.
    static OPENING: Cell<u32> = const { Cell::new(0) };
    /// Set from browser startup until the startup de-duplication ran: (debounce
    /// generation, startup time).
    static STARTUP_DEDUPE: Cell<Option<(u32, f64)>> = const { Cell::new(None) };
    /// When this background instance started.
    static STARTED_AT: Cell<f64> = const { Cell::new(0.0) };
    static STRIP_GENERATION: Cell<u32> = const { Cell::new(0) };
    /// The snapshots as last saved (see snapshots_fingerprint).
    static SNAPSHOTS_SAVED: RefCell<Option<Vec<(Id, Id, String, usize)>>> = const { RefCell::new(None) };
    /// A save is scheduled (see schedule_persist).
    static PERSIST_PENDING: Cell<bool> = const { Cell::new(false) };
    /// A save asked for by [`persist_soon`] is coming.
    static PERSIST_SOON: Cell<bool> = const { Cell::new(false) };
    /// A tab strip sync is running / another one was asked for meanwhile.
    static STRIP_RUNNING: Cell<bool> = const { Cell::new(false) };
    static STRIP_AGAIN: Cell<bool> = const { Cell::new(false) };
    /// The current workspace of each window when inactive groups were last collapsed.
    static GROUPS_COLLAPSED_FOR: RefCell<std::collections::BTreeMap<TabId, Option<Id>>> = RefCell::new(Default::default());
    /// Tabs whose media data the browser holds back, as last given to it.
    static STREAM_HOLDS: RefCell<Option<Vec<TabId>>> = const { RefCell::new(None) };
    /// When a tab was last reloaded because its stream didn't recover.
    static STREAM_RELOADED: RefCell<std::collections::HashMap<TabId, f64>> = RefCell::new(Default::default());
    /// Window title prefaces we set, by window.
    static TITLES: RefCell<std::collections::HashMap<TabId, String>> = RefCell::new(Default::default());
    /// Tabs opened by a page, until their first page loads: tab -> (opener, opened at).
    static OPENED: RefCell<std::collections::HashMap<TabId, (TabId, f64)>> = RefCell::new(Default::default());
    /// Page guards as last given to the browser.
    /// The compiled matchers of the rules removing cookies as soon as they're set, with the
    /// rules and open tab URLs they were built from (see [`on_cookie_set`]).
    static COOKIE_MATCHERS: RefCell<(Vec<CleanupSpec>, Vec<String>, Vec<SpecMatcher>)> = const { RefCell::new((Vec::new(), Vec::new(), Vec::new())) };
    static GUARDS_SYNCED: RefCell<Option<GuardsWanted>> = const { RefCell::new(None) };
    /// The extension's database (IndexedDB) could be opened.
    static DB_OK: Cell<bool> = const { Cell::new(false) };
    /// The snapshots in the database.
    static SNAPS_SAVED: RefCell<std::collections::HashSet<Id>> = RefCell::new(Default::default());
    /// A write to the database that failed, written with the next save.
    static RETRY: RefCell<Option<persist::WritePlan>> = const { RefCell::new(None) };
    /// Commands running (inactive sessions' tabs they read back are let go after).
    static IN_FLIGHT: Cell<u32> = const { Cell::new(0) };
    /// Until when inactive sessions' closed tabs stay in memory (after a browser restart).
    static KEEP_UNTIL: Cell<f64> = const { Cell::new(0.0) };
    /// The database is out of date (the state came from storage.local, was saved there, or
    /// a write failed): the next save rewrites it whole instead of writing the changes.
    static NEED_FULL: Cell<bool> = const { Cell::new(true) };
    /// The database holds a state (to empty once the state is saved in storage.local).
    static DB_HAS_DATA: Cell<bool> = const { Cell::new(false) };
    /// storage.local keys of the state from before the database (or from while it was
    /// off), removed once the state is saved in the database.
    static LEGACY_KEYS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// The sessions saved last time (their search indexes go when they're deleted).
    static SESSIONS_SEEN: RefCell<std::collections::HashSet<Id>> = RefCell::new(Default::default());
    /// The vector indexes read so far, by session (see [`ensure_index`]).
    static VECTORS: RefCell<std::collections::HashMap<Id, VectorIndex>> = RefCell::new(Default::default());
    /// Entries waiting for an automatic summary, and whether the worker runs.
    static AI_QUEUE: RefCell<std::collections::VecDeque<EntryRef>> = RefCell::new(Default::default());
    static AI_RUNNING: Cell<bool> = const { Cell::new(false) };
    /// Debounce generation of each tab's automatic summary.
    static AI_PENDING: RefCell<std::collections::HashMap<TabId, u32>> = RefCell::new(Default::default());
}

/// After a browser restart, inactive sessions' closed tabs stay in memory this long: the
/// browser may still be restoring tabs, which find their saved tab.
const STARTUP_KEEP_MS: f64 = 30_000.0;

/// How long after a navigation a page is read for its automatic summary.
const AUTO_SUMMARY_DELAY_MS: u32 = 5000;
/// How long a provider gets to answer.
const AI_TIMEOUT_MS: u32 = 90_000;
/// Texts embedded per provider request.
const EMBED_BATCH: usize = 64;

/// Where a development build kept a session's vector index (migrated to the database).
fn vectors_key(session: Id) -> String {
    format!("{}{session}", ai::VECTORS_PREFIX)
}

/// How long after opening a tab a rule can still close it (it may load its page late).
const OPENED_TAB_WINDOW_MS: f64 = 15_000.0;

/// Marks that the extension itself is opening tabs (for the lifetime of the guard).
struct OpeningGuard;

impl OpeningGuard {
    fn new() -> Self {
        OPENING.with(|o| o.set(o.get() + 1));
        OpeningGuard
    }
}

impl Drop for OpeningGuard {
    fn drop(&mut self) {
        OPENING.with(|o| o.set(o.get().saturating_sub(1)));
    }
}

fn with_state<R>(f: impl FnOnce(&mut AppState) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// The reply to GetState: the whole state (without snapshots: pages read those from
/// SNAPSHOTS_KEY), written from the state itself rather than from a copy.
#[derive(Serialize)]
struct Reply<'a> {
    #[serde(flatten)]
    response: &'a Response,
    state: &'a AppState,
}

/// What a page asked GetState for (see `page_state.rs`).
type PageAsk = (Option<Vec<Id>>, bool, OpenTabs, std::collections::BTreeMap<Id, u64>);

/// A command's reply. GetState carries the state as the page asked for it: only the tabs of
/// the sessions it shows that changed since it last asked (see `page_state.rs`). Other
/// replies carry the revision saved, O(1): pages then ask for what changed.
fn reply_json(resp: &mut Response, ask: Option<PageAsk>) -> serde_json::Result<String> {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some((mut sessions, with_active, open_tabs, known)) = ask else {
            resp.revision = Some(s.revision);
            return serde_json::to_string(resp);
        };
        if with_active && let Some(ids) = &mut sessions {
            ids.push(s.active_session);
        }
        // Snapshots: what describes them (their tabs are asked for with GetSnapshots).
        persist::with_snapshot_meta(&mut s, |s| {
            page_state::with_page_view(s, sessions.as_deref(), open_tabs, &known, |s| {
                serde_json::to_string(&Reply { response: resp, state: s })
            })
        })
    })
}

/// Runs `f` on the state with its snapshots taken out (and puts them back).
fn without_snapshots<T>(s: &mut AppState, f: impl FnOnce(&AppState) -> T) -> T {
    let taken: Vec<Vec<Snapshot>> = s.sessions.iter_mut().map(|x| std::mem::take(&mut x.snapshots)).collect();
    let out = f(s);
    for (x, snaps) in s.sessions.iter_mut().zip(taken) {
        x.snapshots = snaps;
    }
    out
}

/// What identifies the snapshots as saved (they don't change once taken, only get added,
/// renamed or removed).
fn snapshots_fingerprint(s: &AppState) -> Vec<(Id, Id, String, usize)> {
    fingerprint_parts(s).map(|(a, b, name, n)| (a, b, name.to_string(), n)).collect()
}

fn fingerprint_parts(s: &AppState) -> impl Iterator<Item = (Id, Id, &str, usize)> {
    s.sessions.iter().flat_map(|x| x.snapshots.iter().map(move |n| (x.id, n.id, n.name.as_str(), n.entries.len())))
}

/// Whether the snapshots are as last saved (compared without copying their names).
fn snapshots_saved(s: &AppState) -> bool {
    SNAPSHOTS_SAVED.with(|f| {
        f.borrow().as_ref().is_some_and(|saved| saved.iter().map(|(a, b, name, n)| (*a, *b, name.as_str(), *n)).eq(fingerprint_parts(s)))
    })
}

fn log(msg: &str) {
    web_sys::console::warn_1(&format!("[session-manager] {msg}").into());
}

pub async fn start() -> Result<(), JsValue> {
    STARTED_AT.with(|t| t.set(pf::now()));
    CAPS.with(|c| *c.borrow_mut() = pf::capabilities());
    refresh_permissions().await;

    let mut state = load_state().await;
    // Snapshots kept in storage.local (up to 0.15, or with the database off) join their
    // sessions; with the database, they move into it at the first save.
    if let Ok(Some(json)) = pf::storage_get(SNAPSHOTS_KEY).await {
        match serde_json::from_str::<std::collections::BTreeMap<Id, Vec<Snapshot>>>(&json) {
            Ok(mut all) => {
                for s in state.sessions.iter_mut().filter(|s| s.snapshots.is_empty()) {
                    s.snapshots = all.remove(&s.id).unwrap_or_default();
                }
            }
            Err(e) => log(&format!("stored snapshots unreadable: {e}")),
        }
        LEGACY_KEYS.with(|k| k.borrow_mut().push(SNAPSHOTS_KEY.to_string()));
    }
    state.normalize();
    match pf::query_tabs().await {
        Ok(tabs) => {
            // Tabs open at the last save that aren't there: the browser restarted (it may
            // still be restoring tabs, which find their saved tab: keep closed tabs a while).
            let now_open: std::collections::HashSet<TabId> = tabs.iter().map(|t| t.id).collect();
            let gone =
                state.sessions.iter().any(|s| s.entries.open_tabs().iter().any(|e| e.tab_id.is_some_and(|t| !now_open.contains(&t))));
            if gone {
                KEEP_UNTIL.with(|k| k.set(pf::now() + STARTUP_KEEP_MS));
            }
            state.reconcile(&tabs, pf::now());
        }
        Err(e) => {
            log(&format!("could not query tabs: {e}"));
            KEEP_UNTIL.with(|k| k.set(pf::now() + STARTUP_KEEP_MS));
        }
    }
    SESSIONS_SEEN.with(|seen| *seen.borrow_mut() = state.sessions.iter().map(|x| x.id).collect());
    with_state(|s| *s = state);
    persist().await;
    sync_alarms().await;

    let handler = Closure::<dyn FnMut(JsValue) -> Promise>::new(|ev: JsValue| {
        future_to_promise(async move { handle_event(ev).await.map_err(|e| JsValue::from_str(&e)) })
    });
    pf::set_event_handler(handler.as_ref().unchecked_ref());
    handler.forget();
    schedule_strip_sync();
    sync_guards().await;
    sync_stream_holds().await;
    sync_stream_config().await;
    // Once the browser is done restoring tabs, inactive sessions' closed tabs go.
    spawn_local(async {
        pf::sleep(STARTUP_KEEP_MS as u32 + 1000).await;
        evict_inactive();
    });
    Ok(())
}

/// What the page scripts are registered for: guard settings, block rules, the media
/// script, and the sites that can't save data.
type GuardsWanted = (GuardSettings, Vec<BlockRule>, bool, Vec<String>);

/// Registers the page guard scripts and script blocking rules for the current settings
/// (only when they changed since the last time).
async fn sync_guards() {
    let wanted = with_state(|s| {
        (s.settings.guards.clone(), s.block_rules.clone(), s.settings.media.needs_page_script(), s.no_storage_sites.clone())
    });
    if GUARDS_SYNCED.with(|g| g.borrow().as_ref() == Some(&wanted)) {
        return;
    }
    let (g, rules, media, no_storage) = &wanted;
    let mut regs = guards::registrations(g, rules);
    if *media {
        regs.push(guards::media_registration());
    }
    regs.extend(guards::no_storage_registration(no_storage));
    if let Err(e) = pf::sync_content_scripts(&regs).await {
        log(&format!("could not register page guards: {e}"));
    }
    if let Err(e) = pf::set_block_rules(&guards::dnr_rules(rules)).await {
        log(&format!("could not set script blocking rules: {e}"));
    }
    let exceptions: Vec<String> = g.exceptions.iter().map(|s| guards::normalize_site(s)).collect();
    if let Err(e) = pf::set_script_rewrite(g.anti_debugger && g.strip_debugger_scripts, &exceptions).await {
        log(&format!("could not set the script filter: {e}"));
    }
    GUARDS_SYNCED.with(|s| *s.borrow_mut() = Some(wanted));
}

/// A tab a page opened: once it shows a web page, log it, and close it if a rule says
/// so (then the page that opened it gets the focus back). Returns true if closed.
async fn check_opened_tab(tab: &BrowserTab, now: f64) -> bool {
    let Some((opener, at)) = OPENED.with(|o| o.borrow().get(&tab.id).copied()) else { return false };
    if now - at > OPENED_TAB_WINDOW_MS {
        OPENED.with(|o| o.borrow_mut().remove(&tab.id));
        return false;
    }
    if !matches!(crate::url::scheme(&tab.url).as_str(), "http" | "https") {
        return false; // about:blank first; wait for the real page
    }
    OPENED.with(|o| o.borrow_mut().remove(&tab.id));
    // Nothing to log and no rule for it: no need to look at the page that opened it.
    let (log_it, rule) = with_state(|s| (s.settings.tracking.popup_log, guards::popup_rule_for(&s.block_rules, &tab.url)));
    if !log_it && rule.is_none() {
        return false;
    }
    let opener_url = pf::get_tab(Some(opener)).await.map(|t| t.url).unwrap_or_default();
    if !matches!(crate::url::scheme(&opener_url).as_str(), "http" | "https") {
        return false;
    }
    if log_it {
        with_state(|s| s.push_popup(PopupEvent { at: now, opener: opener_url, url: tab.url.clone(), closed_by: rule }));
        persist_soon();
    }
    if rule.is_none() {
        return false;
    }
    if let Err(e) = pf::remove_tabs(&[tab.id]).await {
        log(&format!("could not close ad tab: {e}"));
        return false;
    }
    let _ = pf::focus_tab(opener).await;
    true
}

/// Whether the optional tabHide permission was granted.
async fn refresh_permissions() {
    if CAPS.with(|c| c.borrow().hide_tabs) {
        let allowed = pf::has_permission("tabHide").await;
        CAPS.with(|c| c.borrow_mut().hide_tabs_allowed = allowed);
    }
}

/// Reads the saved state: from the database, or from storage.local (versions up to 0.14,
/// a development build's per-session keys, or the database switched off), whichever is
/// newer. What came from storage.local moves to the database on the first save.
async fn load_state() -> AppState {
    let db_ok = match pf::db_open().await {
        Ok(()) => true,
        Err(e) => {
            log(&format!("the database can't be opened, saving in storage.local instead: {e}"));
            false
        }
    };
    DB_OK.with(|d| d.set(db_ok));

    let mut legacy = match pf::storage_get(STATE_KEY).await {
        Ok(Some(json)) => match serde_json::from_str::<AppState>(&json) {
            Ok(s) => Some(s),
            Err(e) => {
                log(&format!("the state in storage.local is unreadable: {e}"));
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            log(&format!("could not read storage.local: {e}"));
            None
        }
    };
    if let Some(l) = &mut legacy
        && l.version >= SPLIT_VERSION
    {
        let mut files = std::collections::HashMap::new();
        for x in &l.sessions {
            if let Ok(Some(json)) = pf::storage_get(&session_key(x.id)).await {
                files.insert(x.id, json);
            }
        }
        persist::legacy_with_session_files(l, |id| files.get(&id).cloned());
    }

    // The database: what the background keeps (the active session, open tabs), unless it was
    // saved with an older layout (then everything, saved again).
    let db = if db_ok { read_database().await } else { None };

    let source = persist::newest(db.as_ref().map(|d| &d.0), legacy.as_ref());
    let layout_ok = db.as_ref().is_some_and(|d| d.1);
    DB_HAS_DATA.with(|d| d.set(db.is_some()));
    // Read from the database: saves write what changes from here. Otherwise the database
    // is rewritten whole by the first save (also clearing what's left from before).
    NEED_FULL.with(|n| n.set(source != Source::Database || !layout_ok));

    if let Some(l) = &legacy {
        let mut keys = vec![STATE_KEY.to_string()];
        for x in &l.sessions {
            keys.push(session_key(x.id));
            keys.push(vectors_key(x.id));
            // A development build's search indexes move to the database.
            if db_ok
                && let Ok(Some(json)) = pf::storage_get(&vectors_key(x.id)).await
                && let Err(e) = pf::db_put_vectors(x.id, &json).await
            {
                log(&format!("could not move a search index to the database: {e}"));
            }
        }
        LEGACY_KEYS.with(|k| *k.borrow_mut() = keys);
    }
    // The snapshots the database has (the state describes them).
    if source == Source::Database
        && let Some((s, _)) = &db
    {
        SNAPS_SAVED.with(|saved| *saved.borrow_mut() = s.sessions.iter().flat_map(|x| x.snapshots.iter().map(|n| n.id)).collect());
    }
    match source {
        Source::Database => db.map(|d| d.0).unwrap_or_default(),
        Source::Legacy => legacy.unwrap_or_default(),
        Source::Nothing => AppState::default(),
    }
}

/// The state in the database as the background keeps it, and whether it has the current
/// layout (else it's read whole, to be saved again).
async fn read_database() -> Option<(AppState, bool)> {
    let part = match pf::db_load_partial().await {
        Ok(d) => d,
        Err(e) => {
            log(&format!("could not read the database: {e}"));
            return None;
        }
    };
    let header = part.header?;
    match persist::assemble_partial(&header, &part.tabs) {
        Ok(state) if state.version >= persist::DB_LAYOUT => Some((state, true)),
        Ok(_) => match pf::db_load().await.map_err(|e| e.to_string()).and_then(|d| persist::assemble(&header, &d.tabs)) {
            Ok(state) => Some((state, false)),
            Err(e) => {
                log(&format!("could not read the database: {e}"));
                None
            }
        },
        Err(e) => {
            log(&e);
            None
        }
    }
}

/// The sessions a command works on whose tabs must all be in memory.
enum Need {
    Nothing,
    Some(Vec<Id>),
    All,
}

fn needed(cmd: &Command) -> Need {
    let of = |refs: &[EntryRef]| {
        let mut ids: Vec<Id> = refs.iter().map(|r| r.session).collect();
        ids.sort_unstable();
        ids.dedup();
        Need::Some(ids)
    };
    match cmd {
        Command::GetState { sessions: None, .. } => Need::All,
        Command::GetState { sessions: Some(ids), .. } => Need::Some(ids.clone()),
        Command::SetActiveSession { id } | Command::LoadSession { id, .. } => Need::Some(vec![*id]),
        Command::FocusLastTab { session }
        | Command::RestoreLastClosed { session, .. }
        | Command::CreateSnapshot { session, .. }
        | Command::RestoreSnapshot { session, .. } => Need::Some(vec![*session]),
        Command::OpenEntry { entry, .. } => Need::Some(vec![entry.session]),
        Command::OpenEntries { entries, .. }
        | Command::RemoveEntries { entries, .. }
        | Command::CloseEntries { entries }
        | Command::TagEntries { entries, .. }
        | Command::SetGroup { entries, .. }
        | Command::AssignWorkspace { entries, .. }
        | Command::Summarize { entries }
        | Command::ClearDescriptions { entries } => of(entries),
        Command::MoveEntries { entries, target } => {
            let mut refs = entries.clone();
            refs.push(EntryRef { session: *target, entry: 0 });
            of(&refs)
        }
        Command::AutoPlaceEntries { entries: Some(entries) } => of(entries),
        Command::AutoPlaceEntries { entries: None }
        | Command::ApplyAutoRules
        | Command::SaveWorkspace { .. }
        | Command::DeleteWorkspace { .. } => Need::All,
        Command::RemoveDuplicates { scope: Scope::All, .. } => Need::All,
        Command::RemoveDuplicates { scope: Scope::Session(id), .. } => Need::Some(vec![*id]),
        Command::ImportUrls { target: crate::state::ImportTarget::Session(id), .. } => Need::Some(vec![*id]),
        _ => Need::Nothing,
    }
}

/// Reads back from the database the tabs of these sessions (`None`: all) that aren't all in
/// memory. Tabs in memory win over what's read (they may be newer); if a save happened
/// while reading, it's read again (a tab removed and saved meanwhile mustn't come back).
async fn ensure_loaded(sessions: Option<&[Id]>) -> Result<(), String> {
    for _ in 0..3 {
        let wanted: Vec<Id> = with_state(|s| match sessions {
            None => persist::partial_sessions(s),
            Some(ids) => ids.iter().copied().filter(|id| s.session(*id).is_some_and(|x| x.entries.hidden().is_some())).collect(),
        });
        if wanted.is_empty() {
            return Ok(());
        }
        if !DB_OK.with(Cell::get) {
            return Err("the saved tabs can't be read (the database can't be opened)".into());
        }
        let saved_before = REVISION.with(Cell::get);
        let records = pf::db_load_sessions(&wanted).await?;
        if REVISION.with(Cell::get) != saved_before {
            continue;
        }
        with_state(|s| {
            for id in &wanted {
                persist::load_session(s, *id, &records);
            }
            s.drop_dangling_workspaces();
        });
        return Ok(());
    }
    Err("the saved tabs kept changing while being read; try again".into())
}

/// Whether inactive sessions' closed tabs may be let go now: they're all saved, no command
/// is using them, and the browser isn't restoring tabs (those find their saved tab).
fn may_evict() -> bool {
    DB_OK.with(Cell::get)
        && with_state(|s| s.settings.tracking.database)
        && !NEED_FULL.with(Cell::get)
        && RETRY.with(|r| r.borrow().is_none())
        && IN_FLIGHT.with(Cell::get) == 0
        && STARTUP_DEDUPE.with(|d| d.get()).is_none()
        && pf::now() > KEEP_UNTIL.with(Cell::get)
}

/// Lets inactive sessions' closed tabs go from memory (they're in the database).
fn evict_inactive() {
    if may_evict() {
        with_state(|s| {
            persist::evict_inactive(s);
            SNAPS_SAVED.with(|saved| persist::strip_snapshots(s, &saved.borrow()));
        });
    }
}

/// Reads back from the database the tabs of the snapshots of these sessions (`None`: all)
/// that aren't in memory.
async fn ensure_snapshots(sessions: Option<&[Id]>) -> Result<(), String> {
    let wanted: Vec<Id> = with_state(|s| {
        s.sessions
            .iter()
            .filter(|x| sessions.is_none_or(|ids| ids.contains(&x.id)))
            .filter(|x| x.snapshots.iter().any(|n| !n.loaded()))
            .map(|x| x.id)
            .collect()
    });
    if wanted.is_empty() {
        return Ok(());
    }
    if !DB_OK.with(Cell::get) {
        return Err("the snapshots can't be read (the database can't be opened)".into());
    }
    let records = pf::db_load_snapshots(&wanted).await?;
    with_state(|s| persist::load_snapshots(s, &records));
    Ok(())
}

/// Copies of these sessions (`None`: all) with their tabs, and their snapshots' if
/// `with_snapshots`, read back as needed (for saving to a file).
async fn sessions_for_export(sessions: Option<&[Id]>, with_snapshots: bool) -> Result<Vec<crate::model::Session>, String> {
    ensure_loaded(sessions).await?;
    if with_snapshots {
        ensure_snapshots(sessions).await?;
    }
    Ok(with_state(|s| {
        s.sessions
            .iter()
            .filter(|x| sessions.is_none_or(|ids| ids.contains(&x.id)))
            .map(|x| crate::model::Session { snapshots: if with_snapshots { x.snapshots.clone() } else { Vec::new() }, ..x.clone() })
            .collect()
    }))
}

async fn persist() {
    // What a pending delayed save would write is written now.
    PERSIST_PENDING.with(|p| p.set(false));
    PERSIST_SOON.with(|p| p.set(false));
    let use_db = DB_OK.with(Cell::get) && with_state(|s| s.settings.tracking.database);
    // storage.local holds everything (snapshots in their own key): all of it must be in memory.
    if !use_db {
        if let Err(e) = ensure_loaded(None).await {
            return log(&format!("not saved (the saved tabs couldn't be read back): {e}"));
        }
        if let Err(e) = ensure_snapshots(None).await {
            return log(&format!("not saved (the snapshots couldn't be read back): {e}"));
        }
    }
    // Planned without awaiting, and written first: the database runs transactions in the
    // order they're made, so saves can't land out of order.
    let (next, write, snapshots, gone) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let next = REVISION.with(|r| {
            let n = r.get().max(s.revision) + 1;
            r.set(n);
            n
        });
        s.revision = next;
        // Without the database, snapshots are saved apart in storage.local, when they changed.
        let snapshots = (!use_db && !snapshots_saved(&s)).then(|| {
            let all: std::collections::BTreeMap<Id, &Vec<Snapshot>> = s.sessions.iter().map(|x| (x.id, &x.snapshots)).collect();
            (serde_json::to_string(&all), snapshots_fingerprint(&s))
        });
        let now: std::collections::HashSet<Id> = s.sessions.iter().map(|x| x.id).collect();
        let gone: Vec<Id> = SESSIONS_SEEN.with(|seen| seen.borrow().difference(&now).copied().collect());
        let write = if use_db {
            // Only the tabs that changed (tracked as they change), unless the database is
            // out of date. A write that failed goes with this one.
            let full = NEED_FULL.with(|n| n.replace(false));
            let mut plan = SESSIONS_SEEN.with(|seen| {
                let mut seen = seen.borrow_mut();
                if full { persist::plan_full(&mut s, &mut seen, next) } else { persist::plan_changes(&mut s, &mut seen, next) }
            });
            if let Some(failed) = RETRY.with(|r| r.borrow_mut().take()) {
                // A full rewrite has every tab; only snapshots of a failed write still count.
                let failed = if full {
                    persist::WritePlan { snap_puts: failed.snap_puts, snap_dels: failed.snap_dels, ..Default::default() }
                } else {
                    failed
                };
                persist::retry(&mut plan, failed);
            }
            SNAPS_SAVED.with(|saved| persist::plan_snapshots(&s, &mut saved.borrow_mut(), &mut plan));
            persist::header_json(&mut s).map(|header| Ok((header, plan)))
        } else {
            // Everything in one key; the database will be rewritten whole when it's back.
            persist::forget_changes(&mut s, next);
            NEED_FULL.with(|n| n.set(true));
            SESSIONS_SEEN.with(|seen| *seen.borrow_mut() = now);
            s.version = 1;
            without_snapshots(&mut s, serde_json::to_string).map(Err)
        };
        (next, write, snapshots, gone)
    });
    match write {
        Err(e) => log(&format!("could not serialize state: {e}")),
        Ok(Ok((header, plan))) => match pf::db_write(&header, &plan).await {
            Ok(()) => {
                DB_HAS_DATA.with(|d| d.set(true));
                // The snapshots written are in the database: their tabs can go.
                with_state(|s| SNAPS_SAVED.with(|saved| persist::strip_snapshots(s, &saved.borrow())));
                // Pages ask for what changed.
                if let Err(e) = pf::storage_set(CHANGED_KEY, &next.to_string()).await {
                    log(&format!("could not tell pages about the save: {e}"));
                }
                let stale = LEGACY_KEYS.with(|k| std::mem::take(&mut *k.borrow_mut()));
                if let Err(e) = pf::storage_remove(&stale).await {
                    log(&format!("could not remove the old state from storage.local: {e}"));
                    LEGACY_KEYS.with(|k| *k.borrow_mut() = stale);
                }
            }
            Err(e)
                if with_state(|s| {
                    persist::partial_sessions(s).is_empty() && s.sessions.iter().flat_map(|x| &x.snapshots).all(|x| x.loaded())
                }) =>
            {
                // Nothing is lost: the whole state goes to storage.local, which wins at
                // the next start if it's newer, and the database is rewritten next time.
                NEED_FULL.with(|n| n.set(true));
                log(&format!("could not save to the database, saving in storage.local: {e}"));
                save_in_storage_local().await;
            }
            Err(e) => {
                // Not every tab is in memory, so storage.local can't take the state: what
                // failed is written with the next save.
                log(&format!("could not save to the database (will try again with the next save): {e}"));
                if plan.clear {
                    NEED_FULL.with(|n| n.set(true));
                    let snaps = persist::WritePlan { snap_puts: plan.snap_puts, snap_dels: plan.snap_dels, ..Default::default() };
                    RETRY.with(|r| *r.borrow_mut() = Some(snaps));
                } else {
                    RETRY.with(|r| *r.borrow_mut() = Some(plan));
                }
            }
        },
        Ok(Err(json)) => {
            if let Err(e) = pf::storage_set(STATE_KEY, &json).await {
                log(&format!("could not save state: {e}"));
            } else {
                after_storage_local_save().await;
            }
        }
    }
    if let Some((Ok(all), fingerprint)) = snapshots {
        match pf::storage_set(SNAPSHOTS_KEY, &all).await {
            Ok(()) => SNAPSHOTS_SAVED.with(|f| *f.borrow_mut() = Some(fingerprint)),
            Err(e) => log(&format!("could not save snapshots: {e}")),
        }
    }
    // Deleted sessions' search indexes go too.
    if !gone.is_empty() {
        VECTORS.with(|v| gone.iter().for_each(|id| drop(v.borrow_mut().remove(id))));
        if DB_OK.with(Cell::get)
            && let Err(e) = pf::db_delete_vectors(&gone).await
        {
            log(&format!("could not delete search indexes: {e}"));
        }
    }
    evict_inactive();
}

/// Saves the whole state in storage.local (the database is off, or failed).
async fn save_in_storage_local() {
    let json = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.version = 1;
        without_snapshots(&mut s, serde_json::to_string)
    });
    match json {
        Ok(json) => match pf::storage_set(STATE_KEY, &json).await {
            Ok(()) => after_storage_local_save().await,
            Err(e) => log(&format!("could not save state: {e}")),
        },
        Err(e) => log(&format!("could not serialize state: {e}")),
    }
}

/// The state is in storage.local now: it moves to the database on its next save there, and
/// with the database switched off, the database is emptied (it would be out of date).
async fn after_storage_local_save() {
    // Pages ask for what changed (as with the database).
    let revision = with_state(|s| s.revision);
    if let Err(e) = pf::storage_set(CHANGED_KEY, &revision.to_string()).await {
        log(&format!("could not tell pages about the save: {e}"));
    }
    LEGACY_KEYS.with(|k| {
        let mut k = k.borrow_mut();
        if !k.iter().any(|x| x == STATE_KEY) {
            k.push(STATE_KEY.to_string());
        }
    });
    let switched_off = !with_state(|s| s.settings.tracking.database);
    if switched_off && DB_OK.with(Cell::get) && DB_HAS_DATA.with(Cell::get) {
        match pf::db_clear().await {
            Ok(()) => {
                DB_HAS_DATA.with(|d| d.set(false));
                NEED_FULL.with(|n| n.set(true));
                SNAPS_SAVED.with(|saved| saved.borrow_mut().clear());
            }
            Err(e) => log(&format!("could not empty the database: {e}")),
        }
    }
}

// ---------------------------------------------------------------- events

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Event {
    BrowserStartup,
    TabCreated {
        tab: BrowserTab,
    },
    #[serde(rename_all = "camelCase")]
    TabUpdated {
        tab: BrowserTab,
        url_changed: bool,
    },
    #[serde(rename_all = "camelCase")]
    TabRemoved {
        tab_id: TabId,
        window_closing: bool,
    },
    #[serde(rename_all = "camelCase")]
    TabActivated {
        tab_id: TabId,
    },
    TabReplaced {
        added: TabId,
        removed: TabId,
    },
    /// The media content script reports playback in a frame.
    #[serde(rename_all = "camelCase")]
    MediaState {
        tab_id: TabId,
        frame_id: i64,
        playing: bool,
        video: bool,
        pip: bool,
        #[serde(default)]
        live: bool,
    },
    /// The media content script about stopping streams: `toggle` (its button on a video was
    /// clicked), `fresh` (a new page loaded in the tab), `stuck` (playback didn't recover
    /// after resuming).
    #[serde(rename_all = "camelCase")]
    Stream {
        tab_id: TabId,
        frame_id: i64,
        what: String,
    },
    /// An optional permission was granted or revoked.
    PermissionsChanged,
    /// A keyboard shortcut handled here (`open-inspector`).
    #[serde(rename_all = "camelCase")]
    Command {
        name: String,
        tab_id: Option<TabId>,
    },
    CookieSet {
        cookie: Cookie,
    },
    Alarm {
        name: String,
    },
    Message {
        payload: String,
    },
}

async fn handle_event(ev: JsValue) -> Result<JsValue, String> {
    let ev: Event = serde_wasm_bindgen::from_value(ev).map_err(|e| format!("bad event: {e}"))?;
    let now = pf::now();
    match ev {
        Event::BrowserStartup => {
            if with_state(|s| s.settings.dedupe_on_startup) {
                // Tabs restored before this event were tracked when the background started.
                let since = STARTED_AT.with(|t| t.get()).min(now);
                STARTUP_DEDUPE.with(|d| d.set(Some((0, since))));
                schedule_startup_dedupe();
            }
        }
        Event::TabCreated { tab } => {
            if let Some(opener) = tab.opener_tab_id
                && OPENING.with(|o| o.get()) == 0
            {
                OPENED.with(|o| o.borrow_mut().insert(tab.id, (opener, now)));
                if check_opened_tab(&tab, now).await {
                    return Ok(JsValue::UNDEFINED);
                }
            }
            if STARTUP_DEDUPE.with(|d| d.get()).is_some() {
                // A tab the browser restores: give it back its saved entry rather than
                // adding a copy, and wait for the restore to finish before de-duplicating.
                schedule_startup_dedupe();
                if with_state(|s| s.link_by_url(&tab, now)) {
                    schedule_persist();
                    return Ok(JsValue::UNDEFINED);
                }
            }
            if prevent_duplicate(&tab, now).await {
                return Ok(JsValue::UNDEFINED);
            }
            if with_state(|s| s.on_tab_upsert(&tab, now)) {
                // Many tabs at once (opened unloaded, by us, or restored at startup): saved together.
                if tab.discarded || OPENING.with(|o| o.get()) > 0 || STARTUP_DEDUPE.with(|d| d.get()).is_some() {
                    schedule_persist();
                } else {
                    persist_soon();
                }
                schedule_auto_organize();
                schedule_strip_sync();
            }
        }
        Event::TabUpdated { tab, url_changed } => {
            if url_changed && check_opened_tab(&tab, now).await {
                return Ok(JsValue::UNDEFINED);
            }
            if url_changed && prevent_duplicate(&tab, now).await {
                return Ok(JsValue::UNDEFINED);
            }
            let workspace = |s: &AppState| s.locate_tab(tab.id).and_then(|r| s.entry(r)).map(|e| e.workspace);
            let (changed, moved) = with_state(|s| {
                let before = workspace(s);
                let changed = s.on_tab_upsert(&tab, now);
                (changed, changed && workspace(s) != before)
            });
            if changed {
                if moved {
                    persist().await;
                } else {
                    // Navigation, titles, loading: frequent, saved together.
                    schedule_persist();
                }
                if url_changed {
                    schedule_auto_organize();
                    schedule_auto_summary(tab.id);
                }
                schedule_strip_sync();
            }
        }
        Event::TabRemoved { tab_id, window_closing } => {
            OPENED.with(|o| o.borrow_mut().remove(&tab_id));
            STREAM_RELOADED.with(|r| r.borrow_mut().remove(&tab_id));
            if with_state(|s| s.on_tab_removed(tab_id, window_closing, now)) {
                // A closing window removes all its tabs: saved together.
                if window_closing {
                    schedule_persist();
                } else {
                    persist_soon();
                }
            }
            sync_stream_holds().await;
        }
        Event::TabActivated { tab_id } => {
            let (changed, switched) = with_state(|s| {
                let before = s.active_workspace;
                let changed = s.on_tab_activated(tab_id, now);
                (changed, s.active_workspace != before)
            });
            // A new current workspace shows right away; the time of use can wait.
            if switched {
                persist().await;
            } else if changed {
                schedule_persist();
            }
            enforce_media().await;
            schedule_strip_sync();
        }
        Event::PermissionsChanged => {
            refresh_permissions().await;
            schedule_strip_sync();
        }
        Event::Command { name, tab_id } => {
            if name == "open-inspector"
                && let Err(e) = run_command(Command::OpenInspector { tab: tab_id }).await
            {
                log(&format!("could not open the inspector: {e}"));
            }
        }
        Event::MediaState { tab_id, frame_id, playing, video, pip, live } => {
            // Tracked also without the media limit: the sidebar shows it, and stops streams.
            let report = MediaReport { playing, video, pip, live };
            if with_state(|s| s.media.report(tab_id, frame_id, report, now)) {
                // Playback changes often (previews, ads): saved at most once a second.
                schedule_persist();
                enforce_media().await;
            }
        }
        Event::Stream { tab_id, frame_id, what } => match what.as_str() {
            // The user paused a live stream with the player's own button: its data stops too.
            "pause-live" if with_state(|s| s.settings.media.stream_controls) => {
                stop_stream(tab_id, true).await;
            }
            "toggle" if with_state(|s| s.settings.media.stream_controls) => {
                let stop = !with_state(|s| s.media.stopped.contains(&tab_id));
                stop_stream(tab_id, stop).await;
            }
            // The page reloaded or navigated: its new player starts with the data flowing.
            "fresh" if frame_id == 0 => {
                if with_state(|s| s.media.set_stopped(tab_id, false)) {
                    persist().await;
                    sync_stream_holds().await;
                }
            }
            "stuck" if !with_state(|s| s.media.stopped.contains(&tab_id)) => {
                let again = STREAM_RELOADED.with(|r| r.borrow().get(&tab_id).is_some_and(|at| now - at < STREAM_RELOAD_PAUSE_MS));
                if !again {
                    STREAM_RELOADED.with(|r| r.borrow_mut().insert(tab_id, now));
                    log(&format!("tab {tab_id}: playback didn't recover after resuming; reloading"));
                    if let Err(e) = pf::reload_tab(tab_id).await {
                        log(&format!("could not reload tab {tab_id}: {e}"));
                    }
                }
            }
            _ => {}
        },
        Event::TabReplaced { added, removed } => {
            if with_state(|s| s.on_tab_replaced(added, removed)) {
                persist().await;
            }
            sync_stream_holds().await;
        }
        Event::CookieSet { cookie } => on_cookie_set(cookie).await,
        Event::Alarm { name } if name == SNAPSHOT_ALARM => {
            // Inactive sessions are read back only if their tabs changed since.
            let changed = with_state(|s| s.snapshot_candidates());
            IN_FLIGHT.with(|n| n.set(n.get() + 1));
            if let Err(e) = ensure_loaded(Some(&changed)).await {
                log(&format!("automatic snapshots: {e}"));
            }
            with_state(|s| s.auto_snapshot_all(now));
            IN_FLIGHT.with(|n| n.set(n.get().saturating_sub(1)));
            persist().await;
        }
        Event::Alarm { name } => {
            if let Some(id) = name.strip_prefix(ALARM_PREFIX).and_then(|s| s.parse::<Id>().ok())
                && let Err(e) = run_rule(id, InPages::Skip).await
            {
                log(&format!("scheduled cleanup failed: {e}"));
            }
        }
        Event::Message { payload } => {
            let json = match serde_json::from_str::<Command>(&payload) {
                Ok(cmd) => {
                    let ask = match &cmd {
                        Command::GetState { sessions, with_active, open_tabs, known } => {
                            Some((sessions.clone(), *with_active, *open_tabs, known.iter().copied().collect()))
                        }
                        _ => None,
                    };
                    // Tabs read back for it stay until the reply is written, then go.
                    IN_FLIGHT.with(|n| n.set(n.get() + 1));
                    let json = reply_json(&mut execute(cmd).await, ask);
                    IN_FLIGHT.with(|n| n.set(n.get().saturating_sub(1)));
                    evict_inactive();
                    json
                }
                Err(e) => serde_json::to_string(&Response::err(format!("bad command: {e}"))),
            }
            .map_err(|e| e.to_string())?;
            return Ok(JsValue::from_str(&json));
        }
    }
    Ok(JsValue::UNDEFINED)
}

/// "Prevent duplicates": a new tab showing a page that is already in the active session
/// is merged into it. Returns true if the tab was handled here.
async fn prevent_duplicate(tab: &BrowserTab, now: f64) -> bool {
    if OPENING.with(|o| o.get()) > 0 || !with_state(|s| s.settings.prevent_duplicates) {
        return false;
    }
    let Some(existing) = with_state(|s| s.duplicate_of_new_tab(tab)) else { return false };
    let existing_tab = with_state(|s| s.entry(existing).and_then(|e| e.tab_id));
    match existing_tab {
        Some(existing_tab) if existing_tab != tab.id => {
            // Switch to the tab that is already open instead of keeping a second copy.
            if tab.active {
                let _ = pf::focus_tab(existing_tab).await;
            }
            if let Err(e) = pf::remove_tabs(&[tab.id]).await {
                log(&format!("could not close duplicate tab: {e}"));
                return false;
            }
        }
        _ => {
            // The page is saved but unloaded: let this tab become that entry.
            with_state(|s| {
                s.link_tab(existing, tab, now);
                s.on_tab_upsert(tab, now);
            });
            persist().await;
        }
    }
    true
}

/// "Clean up whenever a new value is set": remove freshly set cookies matching a rule.
async fn on_cookie_set(cookie: Cookie) {
    let hit = STATE.with(|s| {
        let s = s.borrow();
        // Sites that can't save data: their own cookies, and those set under them (partitioned).
        if !s.no_storage_sites.is_empty()
            && (guards::storage_blocked(&s.no_storage_sites, &cookie.host())
                || cookie.top_level_host().is_some_and(|h| guards::storage_blocked(&s.no_storage_sites, &h)))
        {
            return true;
        }
        let rules = || s.cleanup_rules.iter().filter(|r| r.enabled && r.on_cookie_set && r.spec.data_types.cookies).map(|r| &r.spec);
        if rules().next().is_none() {
            return false;
        }
        // Open tabs only matter to rules targeting them.
        let tabs = rules().any(|spec| spec.target == CleanupTarget::OpenTabs);
        let urls = || s.sessions.iter().flat_map(|s| s.entries.iter()).filter(|e| tabs && e.is_open()).map(|e| &e.url);
        COOKIE_MATCHERS.with(|c| {
            let mut c = c.borrow_mut();
            let (specs, open, matchers) = &mut *c;
            if !specs.iter().eq(rules()) || !open.iter().eq(urls()) {
                *specs = rules().cloned().collect();
                *open = urls().cloned().collect();
                *matchers = specs.iter().filter_map(|spec| SpecMatcher::new(spec, open).ok()).collect();
            }
            matchers.iter().any(|m| m.matches_cookie(&cookie))
        })
    });
    if hit {
        let removal = cleanup::CookieRemoval {
            url: cookie.url(),
            name: cookie.name.clone(),
            store_id: cookie.store_id.clone(),
            partition_key: cookie.partition_key.clone(),
        };
        let (_, errors) = pf::remove_cookies(&[removal]).await;
        for e in errors {
            log(&e);
        }
    }
}

/// Debounced de-duplication of every session after browser startup.
fn schedule_startup_dedupe() {
    let Some((generation, started)) = STARTUP_DEDUPE.with(|d| {
        let next = d.get().map(|(g, started)| (g.wrapping_add(1), started));
        d.set(next);
        next
    }) else {
        return;
    };
    spawn_local(async move {
        if pf::now() - started < STARTUP_MAX_WAIT_MS {
            pf::sleep(STARTUP_SETTLE_MS).await;
        }
        if STARTUP_DEDUPE.with(|d| d.get()).map(|(g, _)| g) != Some(generation) {
            return;
        }
        // Duplicates are looked for in every session's saved tabs.
        IN_FLIGHT.with(|n| n.set(n.get() + 1));
        if let Err(e) = ensure_loaded(None).await {
            log(&format!("startup de-duplication: {e}"));
        }
        STARTUP_DEDUPE.with(|d| d.set(None));
        let sessions: Vec<Id> = with_state(|s| s.sessions.iter().map(|s| s.id).collect());
        let result = remove_duplicates_in(&sessions, Some(started)).await;
        IN_FLIGHT.with(|n| n.set(n.get().saturating_sub(1)));
        evict_inactive();
        match result {
            Ok(0) => {}
            Ok(n) => log(&format!("removed {n} duplicate tab(s) after browser startup")),
            Err(e) => log(&format!("startup de-duplication failed: {e}")),
        }
    });
}

/// Removes duplicates within each of `sessions` (never across them) and closes the
/// extra open tabs. With `restored_since`, also tabs restored since then that copy an
/// open page of any session. Returns how many entries were removed.
async fn remove_duplicates_in(sessions: &[Id], restored_since: Option<f64>) -> Result<usize, String> {
    let (removed, open_tabs) = with_state(|s| {
        let opts = s.settings.dedupe;
        let mut removed = 0;
        let mut open_tabs = Vec::new();
        if let Some(since) = restored_since {
            let r = s.remove_restored_copies(since, &opts);
            removed += r.removed;
            open_tabs.extend(r.open_tabs);
        }
        for id in sessions {
            let r = s.remove_duplicates(Scope::Session(*id), &opts);
            removed += r.removed;
            open_tabs.extend(r.open_tabs);
        }
        (removed, open_tabs)
    });
    if removed > 0 {
        persist().await;
        pf::remove_tabs(&open_tabs).await?;
    }
    Ok(removed)
}

// ---------------------------------------------------------------- commands

async fn execute(cmd: Command) -> Response {
    if !matches!(cmd, Command::GetState { .. }) {
        // Most commands change what the tab strip should show; the sync is debounced and
        // does nothing when nothing changed.
        schedule_strip_sync();
    }
    // The sessions it works on, read back if their tabs aren't all in memory.
    let need = match (&cmd, needed(&cmd)) {
        // Sessions the page already has as they are aren't sent: no need to read them back.
        (Command::GetState { known, .. }, Need::Some(ids)) => Need::Some(with_state(|s| {
            ids.into_iter().filter(|id| s.session(*id).is_none_or(|x| !known.contains(&(*id, x.tabs_revision)))).collect()
        })),
        (_, need) => need,
    };
    let loaded = match need {
        Need::Nothing => Ok(()),
        Need::Some(ids) => ensure_loaded(Some(&ids)).await,
        Need::All => ensure_loaded(None).await,
    };
    let result = match loaded {
        Ok(()) => run_command(cmd).await,
        Err(e) => Err(e),
    };
    let mut resp = match result {
        Ok(r) => r,
        Err(e) => Response::err(e),
    };
    resp.capabilities = Some(CAPS.with(|c| c.borrow().clone()));
    resp
}

fn ok(info: impl Into<String>) -> Result<Response, String> {
    Ok(Response { ok: true, info: Some(info.into()), ..Default::default() })
}

async fn run_command(cmd: Command) -> Result<Response, String> {
    let now = pf::now();
    match cmd {
        Command::GetState { .. } => Ok(Response { ok: true, ..Default::default() }),
        Command::CreateSession { name, make_active } => {
            let id = with_state(|s| {
                let id = s.create_session(&name, now);
                if make_active {
                    s.active_session = id;
                }
                id
            });
            persist().await;
            let name = with_state(|s| s.session(id).map(|s| s.name.clone()).unwrap_or_default());
            ok(format!("Created session \"{name}\""))
        }
        Command::RenameSession { id, name } => {
            with_state(|s| s.rename_session(id, &name))?;
            persist().await;
            ok("Session renamed")
        }
        Command::DeleteSession { id, close_tabs } => {
            let open = with_state(|s| s.delete_session(id))?;
            persist().await;
            if close_tabs {
                pf::remove_tabs(&open).await?;
            }
            ok("Session deleted")
        }
        Command::SetActiveSession { id } => {
            with_state(|s| s.set_active(id))?;
            persist().await;
            ok("Active session changed (no tabs were opened)")
        }
        Command::LoadSession { id, discarded, new_window } => load_session(id, discarded, new_window).await,
        Command::OpenEntry { entry, discarded } => {
            open_entry(entry, discarded).await?;
            ok("")
        }
        Command::OpenEntries { entries, discarded } => {
            // Grouped by session, in the order given.
            let mut by_session: Vec<(Id, Vec<Id>)> = Vec::new();
            for r in &entries {
                match by_session.iter_mut().find(|(s, _)| *s == r.session) {
                    Some((_, ids)) => ids.push(r.entry),
                    None => by_session.push((r.session, vec![r.entry])),
                }
            }
            let (mut opened, mut errors) = (0, Vec::new());
            for (session, ids) in by_session {
                let (n, e) = open_entries(session, Some(&ids), discarded, false).await?;
                opened += n;
                errors.extend(e);
            }
            if opened == 0 && errors.is_empty() {
                return ok("All these tabs are already open");
            }
            let mut info = format!("Opened {opened} tab(s) {}", if discarded { "unloaded" } else { "loaded" });
            if !errors.is_empty() {
                info.push_str(&format!("; {} failed (e.g. {})", errors.len(), errors[0]));
            }
            ok(info)
        }
        Command::RestoreLastClosed { session, discarded } => {
            let ids: Vec<Id> = with_state(|s| s.last_closed_refs(session).into_iter().map(|r| r.entry).collect());
            if ids.is_empty() {
                return ok("No tabs to restore: none were open when this session's tabs were last closed");
            }
            let (opened, errors) = open_entries(session, Some(&ids), discarded, false).await?;
            if opened > 0 {
                with_state(|s| s.renew_last_closed(session));
                persist().await;
            }
            let mut info = format!("Restored {opened} tab(s) {}", if discarded { "unloaded" } else { "loaded" });
            if !errors.is_empty() {
                info.push_str(&format!("; {} failed (e.g. {})", errors.len(), errors[0]));
            }
            ok(info)
        }
        Command::Summarize { entries } => summarize(&entries).await,
        Command::ClearDescriptions { entries } => {
            let n = with_state(|s| {
                let mut n = 0;
                for r in &entries {
                    if let Some(e) = s.entry_mut(*r)
                        && !e.description.is_empty()
                    {
                        e.description.clear();
                        e.description_url.clear();
                        n += 1;
                    }
                }
                n
            });
            persist().await;
            ok(format!("Removed {n} summary(ies)"))
        }
        Command::VectorSearch { query, sessions, limit } => vector_search(&query, sessions, limit as usize).await,
        Command::FreeSearchMemory => {
            let freed = free_inactive_vectors();
            ok(format!("Freed the search data of {freed} inactive session(s)"))
        }
        Command::FocusLastTab { session } => {
            let last = with_state(|s| s.session(session).and_then(|s| s.last_accessed()).map(|e| (e.id, e.title.clone())));
            let Some((entry, title)) = last else { return Err("session has no tabs".into()) };
            open_entry(EntryRef { session, entry }, false).await?;
            ok(format!("Last tab: {title}"))
        }
        Command::RemoveEntries { entries, close_tabs } => {
            let open = with_state(|s| s.remove_entries(&entries, !close_tabs));
            persist().await;
            if close_tabs {
                pf::remove_tabs(&open).await?;
            }
            ok(format!("Removed {} tab(s) from session", entries.len()))
        }
        Command::CloseEntries { entries } => {
            let tabs = with_state(|s| s.close_entries(&entries, pf::now()));
            persist().await;
            pf::remove_tabs(&tabs).await?;
            ok(format!("Closed {} tab(s), kept in the session", tabs.len()))
        }
        Command::TagEntries { entries, tag, add } => {
            let n = with_state(|s| s.tag_entries(&entries, &tag, add));
            persist().await;
            ok(format!("{} tag \"{}\" on {n} tab(s)", if add { "Added" } else { "Removed" }, tag.trim()))
        }
        Command::SetGroup { entries, group } => {
            let n = with_state(|s| s.set_group(&entries, group.as_deref()));
            persist().await;
            schedule_auto_organize();
            ok(format!("Updated group of {n} tab(s)"))
        }
        Command::MoveEntries { entries, target } => {
            let n = with_state(|s| s.move_entries(&entries, target))?;
            persist().await;
            ok(format!("Moved {n} tab(s)"))
        }
        Command::Organize { key, native_groups } => {
            let n = organize_tabs(key, native_groups).await?;
            ok(format!("Re-organized {n} tab(s) by {}", key.label().to_lowercase()))
        }
        Command::SaveAutoRule { mut rule } => {
            rule.pattern.compile()?;
            with_state(|s| {
                if rule.id == 0 {
                    rule.id = s.alloc_id();
                    s.auto_rules.push(rule);
                } else if let Some(r) = s.auto_rules.iter_mut().find(|r| r.id == rule.id) {
                    *r = rule;
                }
            });
            persist().await;
            ok("Rule saved")
        }
        Command::DeleteAutoRule { id } => {
            with_state(|s| s.auto_rules.retain(|r| r.id != id));
            persist().await;
            ok("Rule deleted")
        }
        Command::ApplyAutoRules => {
            with_state(|s| s.apply_auto_rules_all());
            persist().await;
            schedule_auto_organize();
            ok("Rules applied to existing tabs")
        }
        Command::UpdateSettings { settings } => {
            let was_hiding = with_state(|s| s.settings.browser_ui.hide_inactive);
            if was_hiding && !settings.browser_ui.hide_inactive {
                show_workspace_tabs().await;
            }
            with_state(|s| {
                if !settings.media.enabled() {
                    s.media.paused.clear();
                }
                s.settings = settings;
            });
            persist().await;
            sync_alarms().await;
            schedule_auto_organize();
            enforce_media().await;
            if !with_state(|s| s.settings.media.stream_controls) {
                for tab in with_state(|s| s.media.stopped.iter().copied().collect::<Vec<_>>()) {
                    stop_stream(tab, false).await;
                }
            }
            // What the new settings paused or resumed is saved before the reply.
            if PERSIST_SOON.with(|p| p.get()) {
                persist().await;
            }
            sync_stream_config().await;
            sync_guards().await;
            ok("Settings saved")
        }
        Command::SaveWorkspace { workspace } => {
            let new = workspace.id == 0;
            let (id, placed) = with_state(|s| s.save_workspace(workspace).map(|id| (id, s.auto_place(None, false))))?;
            persist().await;
            let name = with_state(|s| s.workspace(id).map(|w| w.name.clone()).unwrap_or_default());
            let mut info = format!("Workspace \"{name}\" {}", if new { "created" } else { "saved" });
            if placed > 0 {
                info.push_str(&format!("; its rules placed {placed} tab(s)"));
            }
            ok(info)
        }
        Command::DeleteWorkspace { id } => {
            with_state(|s| s.delete_workspace(id))?;
            persist().await;
            ok("Workspace deleted (its tabs were kept)")
        }
        Command::MoveWorkspace { id, delta } => {
            with_state(|s| s.move_workspace(id, delta))?;
            persist().await;
            ok("")
        }
        Command::AssignWorkspace { entries, workspace } => {
            let n = with_state(|s| s.assign_workspace(&entries, workspace))?;
            persist().await;
            let name = with_state(|s| workspace.and_then(|id| s.workspace(id)).map(|w| w.name.clone()));
            ok(match name {
                Some(name) => format!("Moved {n} tab(s) to \"{name}\""),
                None => format!("Took {n} tab(s) out of their workspace"),
            })
        }
        Command::AutoPlaceEntries { entries } => {
            // Chosen entries go back to the rules; for all entries, manual choices stay.
            let n = with_state(|s| s.auto_place(entries.as_deref(), entries.is_some()));
            persist().await;
            ok(format!("Workspace rules moved {n} tab(s)"))
        }
        Command::Hibernate { target, force } => hibernate_tabs(&target, force).await,
        Command::StopStream { tab, stop } => {
            if stop && !with_state(|s| s.settings.media.stream_controls) {
                return Err("stream controls are off (Settings → Performance)".into());
            }
            if stop_stream(tab, stop).await {
                persist().await;
            }
            ok(if stop { "Stream stopped: no more data until you resume it" } else { "Stream resumed" })
        }
        Command::StopBackgroundStreams => {
            if !with_state(|s| s.settings.media.stream_controls) {
                return Err("stream controls are off (Settings → Performance)".into());
            }
            let tabs = pf::query_tabs().await?;
            let ids = with_state(|s| s.media.background_streams(&tabs));
            let mut changed = false;
            for id in &ids {
                changed |= stop_stream(*id, true).await;
            }
            if changed {
                persist().await;
            }
            ok(format!("Stopped {} background stream(s)", ids.len()))
        }
        Command::SetActiveWorkspace { id } => {
            with_state(|s| s.set_active_workspace(id))?;
            persist().await;
            if id.is_none() {
                show_workspace_tabs().await;
            }
            // Go to the workspace's most recently used open tab (with none, to the most
            // recently used tab outside workspaces).
            let (name, target) = with_state(|s| {
                let target = s
                    .sessions
                    .iter()
                    .flat_map(|x| &x.entries)
                    .filter(|e| e.tab_id.is_some() && e.workspace.and_then(|w| s.workspace(w)).map(|w| w.id) == id)
                    .max_by(|a, b| a.last_accessed.total_cmp(&b.last_accessed))
                    .and_then(|e| e.tab_id);
                let name = id.and_then(|id| s.workspace(id)).map(|w| w.label());
                (name, target)
            });
            if let Some(tab) = target
                && let Err(e) = pf::focus_tab(tab).await
            {
                log(&format!("could not switch to tab {tab}: {e}"));
            }
            ok(name.map_or("No current workspace".to_string(), |n| format!("Switched to {n}")))
        }
        Command::SaveBlockRule { rule } => {
            let new = rule.id == 0;
            let id = with_state(|s| s.save_block_rule(rule))?;
            persist().await;
            sync_guards().await;
            let text = with_state(|s| s.block_rules.iter().find(|r| r.id == id).map(|r| r.describe()).unwrap_or_default());
            ok(format!("{} rule: {text}", if new { "Added" } else { "Saved" }))
        }
        Command::DeleteBlockRule { id } => {
            with_state(|s| s.block_rules.retain(|r| r.id != id));
            persist().await;
            sync_guards().await;
            ok("Rule deleted")
        }
        Command::ClearPopupLog => {
            with_state(|s| s.popup_log.clear());
            persist().await;
            ok("")
        }
        Command::OpenInspector { tab } => {
            let tab = pf::get_tab(tab).await?;
            if !matches!(crate::url::scheme(&tab.url).as_str(), "http" | "https" | "file") {
                return Err("only web pages can be inspected (not browser or extension pages)".into());
            }
            pf::open_inspector(tab.id).await?;
            ok("")
        }
        Command::NewTabInWorkspace { workspace } => {
            let ws = workspace.or(with_state(|s| s.active_workspace));
            with_state(|s| s.set_active_workspace(ws))?;
            let tab = pf::create_tab(&CreateTab { active: true, ..Default::default() }).await?;
            with_state(|s| {
                if let Some(w) = ws {
                    s.pending_workspace.insert(tab.id, w);
                }
            });
            persist().await;
            ok("")
        }
        Command::RunCleanup { label, spec } => {
            let report = run_cleanup(&label, &spec, InPages::Clear).await?;
            with_state(|s| s.push_cleanup_report(report.clone()));
            persist().await;
            Ok(Response { ok: true, info: Some(report.summary()), report: Some(report), ..Default::default() })
        }
        Command::SaveCleanupRule { mut rule } => {
            SpecMatcher::new(&rule.spec, &[])?;
            if rule.spec.data_types.is_empty() {
                return Err("select at least one data type".into());
            }
            if let Some(m) = rule.interval_minutes {
                rule.interval_minutes = Some(m.max(1));
            }
            if rule.name.trim().is_empty() {
                rule.name = rule.spec.target.describe();
            }
            with_state(|s| {
                if rule.id == 0 {
                    rule.id = s.alloc_id();
                    s.cleanup_rules.push(rule);
                } else if let Some(r) = s.cleanup_rules.iter_mut().find(|r| r.id == rule.id) {
                    *r = rule;
                }
            });
            persist().await;
            sync_alarms().await;
            ok("Cleanup rule saved")
        }
        Command::DeleteCleanupRule { id } => {
            with_state(|s| s.cleanup_rules.retain(|r| r.id != id));
            persist().await;
            sync_alarms().await;
            ok("Cleanup rule deleted")
        }
        Command::RunCleanupRule { id } => {
            let report = run_rule(id, InPages::Clear).await?;
            Ok(Response { ok: true, info: Some(report.summary()), report: Some(report), ..Default::default() })
        }
        Command::CreateSnapshot { session, name } => {
            let id = with_state(|s| s.create_snapshot(session, &name, false, now))?;
            persist().await;
            let name = with_state(|s| s.session(session).and_then(|x| x.snapshot(id)).map(|x| x.name.clone()).unwrap_or_default());
            ok(format!("Snapshot \"{name}\" saved"))
        }
        Command::RestoreSnapshot { session, snapshot, mode, open } => {
            ensure_snapshots(Some(&[session])).await?;
            let (target, added) = with_state(|s| s.restore_snapshot(session, snapshot, mode, now))?;
            persist().await;
            let mut info = match mode {
                RestoreMode::Replace => "Snapshot restored (previous tabs saved as a snapshot)".to_string(),
                RestoreMode::Merge => format!("Added {} tab(s) from the snapshot", added.len()),
                RestoreMode::NewSession => "Snapshot opened as a new session".to_string(),
            };
            if open && !added.is_empty() {
                let (opened, _) = open_entries(target, Some(&added), true, false).await?;
                if opened > 0 {
                    with_state(|s| s.renew_last_closed(target));
                    persist().await;
                }
                info.push_str(&format!("; opened {opened} unloaded"));
            }
            ok(info)
        }
        Command::GetSnapshots { session } => {
            ensure_snapshots(Some(&[session])).await?;
            let snaps = with_state(|s| s.session(session).map(|x| x.snapshots.clone())).ok_or("no such session")?;
            Ok(Response { ok: true, snapshots: Some(snaps), ..Default::default() })
        }
        Command::ExportSessions { sessions, include_snapshots, filename } => {
            let list = sessions_for_export(sessions.as_deref(), include_snapshots).await?;
            let json = crate::transfer::export_sessions(&list, include_snapshots, now);
            Ok(Response { ok: true, file: Some((filename, json)), ..Default::default() })
        }
        Command::DeleteSnapshot { session, snapshot } => {
            with_state(|s| s.delete_snapshot(session, snapshot))?;
            persist().await;
            ok("Snapshot deleted")
        }
        Command::ImportUrls { urls, target, open } => {
            if urls.is_empty() {
                return Err("no URLs to import".into());
            }
            let (session, added) = with_state(|s| s.import_urls(&urls, &target, now))?;
            persist().await;
            let skipped = urls.len() - added.len();
            let mut info = format!("Imported {} URL(s)", added.len());
            if skipped > 0 {
                info.push_str(&format!(" ({skipped} already in the session)"));
            }
            if open && !added.is_empty() {
                let (opened, errors) = open_entries(session, Some(&added), true, false).await?;
                info.push_str(&format!("; opened {opened} unloaded"));
                if !errors.is_empty() {
                    info.push_str(&format!(", {} failed", errors.len()));
                }
            }
            ok(info)
        }
        Command::ImportSessions { sessions } => {
            let n = sessions.len();
            let tabs: usize = sessions.iter().map(|x| x.entries.len()).sum();
            with_state(|s| s.import_sessions(sessions, now));
            persist().await;
            ok(format!("Imported {n} session(s) with {tabs} tab(s)"))
        }
        Command::RemoveDuplicates { scope, close_tabs } => {
            let result = with_state(|s| {
                let opts = s.settings.dedupe;
                let r = s.remove_duplicates(scope, &opts);
                if !close_tabs {
                    s.ignored_tab_ids.extend(r.open_tabs.iter().copied());
                }
                r
            });
            persist().await;
            if result.removed == 0 {
                return ok("No duplicate tabs found");
            }
            let mut info = format!("Removed {} duplicate tab(s)", result.removed);
            if close_tabs && !result.open_tabs.is_empty() {
                pf::remove_tabs(&result.open_tabs).await?;
                info.push_str(&format!(", closed {}", result.open_tabs.len()));
            }
            ok(info)
        }
        Command::ClearTabData { tab, data_types, reload } => {
            // The whole site (all its subdomains), in every open tab of it: a page left
            // running would save what it holds in memory again.
            let tab = pf::get_tab(tab).await?;
            let (site, spec) = cleanup::site_spec(&tab.url, data_types)?;
            let pages = if reload { InPages::ClearAndReload } else { InPages::Clear };
            let report = run_cleanup(&format!("Site: {site}"), &spec, pages).await?;
            with_state(|s| s.push_cleanup_report(report.clone()));
            persist().await;
            Ok(Response { ok: true, info: Some(report.summary()), report: Some(report), ..Default::default() })
        }
        Command::SetNoStorage { site, block } => {
            let site = guards::storage_site(&site).ok_or_else(|| format!("\"{site}\" is not a site name"))?;
            let changed = with_state(|s| {
                let had = s.no_storage_sites.contains(&site);
                if block && !had {
                    s.no_storage_sites.push(site.clone());
                    s.no_storage_sites.sort();
                } else if !block && had {
                    s.no_storage_sites.retain(|x| *x != site);
                }
                had != block
            });
            if changed {
                persist().await;
            }
            // Registered before the tabs reload, so their next pages start without storage.
            sync_guards().await;
            if !block {
                return ok(format!("{site} can save data again (its open tabs once reloaded)"));
            }
            // What it saved before goes too, and its tabs reload into memory-only storage.
            let (_, spec) = cleanup::site_spec(&format!("https://{site}/"), cleanup::DataTypes::default())?;
            let report = run_cleanup(&format!("Blocked: {site}"), &spec, InPages::ClearAndReload).await?;
            with_state(|s| s.push_cleanup_report(report.clone()));
            persist().await;
            let info = format!("{site} can no longer save data. {}", report.summary());
            Ok(Response { ok: true, info: Some(info), report: Some(report), ..Default::default() })
        }
        Command::ClearCleanupLog => {
            with_state(|s| s.cleanup_log.clear());
            persist().await;
            ok("")
        }
    }
}

async fn load_session(id: Id, discarded: bool, new_window: bool) -> Result<Response, String> {
    // De-duplicate first, so only one copy of each page gets opened.
    let deduped = if with_state(|s| s.settings.dedupe_on_session_load) { remove_duplicates_in(&[id], None).await? } else { 0 };
    let (opened, errors) = open_entries(id, None, discarded, new_window).await?;
    if opened > 0 {
        // A restored session starts a new generation of open tabs.
        with_state(|s| s.renew_last_closed(id));
        persist().await;
    }
    let removed = if deduped > 0 { format!("; removed {deduped} duplicate(s)") } else { String::new() };
    if opened == 0 && errors.is_empty() {
        return ok(format!("All tabs of this session are already open{removed}"));
    }
    let mode = if discarded { "unloaded" } else { "loaded" };
    let mut info = format!("Opened {opened} tab(s) {mode}{removed}");
    if !errors.is_empty() {
        info.push_str(&format!("; {} failed (e.g. {})", errors.len(), errors[0]));
    }
    ok(info)
}

/// Opens the session's closed entries (all, or only `only`). Returns how many were
/// opened and the errors.
async fn open_entries(id: Id, only: Option<&[Id]>, discarded: bool, new_window: bool) -> Result<(usize, Vec<String>), String> {
    let closed: Vec<(Id, String, String, bool)> = with_state(|s| {
        let ignore_internal = s.settings.ignore_internal_urls;
        s.session(id).map(|s| {
            s.entries
                .iter()
                .filter(|e| only.is_none_or(|ids| ids.contains(&e.id)))
                .filter(|e| !e.is_open() && trackable(&e.url, ignore_internal))
                .map(|e| (e.id, e.url.clone(), e.title.clone(), e.pinned))
                .collect()
        })
    })
    .ok_or("no such session")?;
    if closed.is_empty() {
        return Ok((0, Vec::new()));
    }

    let _opening = OpeningGuard::new();
    let window = if new_window { Some(pf::create_window().await?) } else { None };
    let mut opened = 0;
    let mut errors = Vec::new();
    for (entry, url, title, pinned) in &closed {
        let opts = CreateTab { url, title, active: false, discarded, pinned: *pinned, window_id: window.as_ref().map(|w| w.window_id) };
        match pf::create_tab(&opts).await {
            Ok(tab) => {
                with_state(|s| s.link_tab(EntryRef { session: id, entry: *entry }, &tab, pf::now()));
                opened += 1;
            }
            Err(e) => errors.push(format!("{url}: {e}")),
        }
    }
    if let Some(w) = &window
        && opened > 0
    {
        // Close the blank tab the new window came with.
        let _ = pf::remove_tabs(&w.tab_ids).await;
    }
    persist().await;
    Ok((opened, errors))
}

async fn open_entry(r: EntryRef, discarded: bool) -> Result<(), String> {
    let e = with_state(|s| s.entry(r).cloned()).ok_or("tab no longer exists")?;
    // The stored tab id can be stale (e.g. the tab closed while the background was
    // asleep); fall back to reopening the page instead of failing.
    let focused = match e.tab_id {
        Some(tab_id) => match pf::focus_tab(tab_id).await {
            Ok(()) => true,
            Err(err) => {
                log(&format!("tab {tab_id} could not be focused ({err}); reopening"));
                with_state(|s| {
                    let time = s.settings.tracking.tab_times.then(pf::now);
                    if let Some(e) = s.entry_mut(r) {
                        let was_loaded = e.is_loaded();
                        e.tab_id = None;
                        e.window_id = None;
                        e.stamp(true, was_loaded, time);
                    }
                });
                false
            }
        },
        None => false,
    };
    if !focused {
        let _opening = OpeningGuard::new();
        let opts = CreateTab { url: &e.url, title: &e.title, active: !discarded, discarded, pinned: e.pinned, window_id: None };
        let tab = pf::create_tab(&opts).await?;
        with_state(|s| s.link_tab(r, &tab, pf::now()));
        if !discarded {
            pf::focus_tab(tab.id).await?;
        }
    }
    with_state(|s| {
        let record = s.settings.tracking.last_used;
        if let Some(e) = s.entry_mut(r)
            && record
        {
            e.last_accessed = pf::now();
        }
    });
    persist().await;
    Ok(())
}

// ---------------------------------------------------------------- hibernate / media

/// Unloads the target's tabs (see [`hibernate::candidates`] for which are skipped).
async fn hibernate_tabs(target: &HibernateTarget, force: bool) -> Result<Response, String> {
    let (wanted, playing) = with_state(|s| (s.hibernate_tabs(target), s.media.playing.iter().map(|p| p.tab_id).collect()));
    let tabs = pf::query_tabs().await?;
    let ids = hibernate::candidates(&tabs, wanted.as_ref(), force, &playing);
    if ids.is_empty() {
        let skipped = if force { "active tabs" } else { "active, pinned and playing tabs" };
        return ok(format!("No tabs to hibernate ({skipped} and tabs already hibernated are skipped)"));
    }
    let result = pf::discard_tabs(&ids).await?;
    if with_state(|s| s.mark_discarded(&result.discarded, pf::now())) {
        persist().await;
    }
    let mut info = format!("Hibernated {} tab(s)", result.discarded.len());
    if let Some(e) = result.errors.first() {
        info.push_str(&format!("; {} could not be hibernated (e.g. {e})", result.errors.len()));
    }
    ok(info)
}

// ---------------------------------------------------------------- stopped streams

const STREAM_RELOAD_PAUSE_MS: f64 = 60_000.0;

/// Stops (pauses, and holds back the media data of) or resumes a tab's streams. Returns
/// true if that changed anything (saved soon; commands save right away).
async fn stop_stream(tab: TabId, stop: bool) -> bool {
    if !with_state(|s| s.media.set_stopped(tab, stop)) {
        return false;
    }
    persist_soon();
    // Data stops before the player is paused, and flows again before it plays.
    sync_stream_holds().await;
    pf::stream_command(tab, if stop { "stop" } else { "resume" }).await;
    true
}

/// Gives the browser the tabs whose media data is held back (when they changed).
async fn sync_stream_holds() {
    let tabs: Vec<TabId> = with_state(|s| s.media.stopped.iter().copied().collect());
    if STREAM_HOLDS.with(|h| h.borrow().as_ref() == Some(&tabs)) {
        return;
    }
    match pf::set_stream_holds(&tabs).await {
        Ok(()) => STREAM_HOLDS.with(|h| *h.borrow_mut() = Some(tabs)),
        Err(e) => log(&format!("could not hold back stream data: {e}")),
    }
}

/// Stores what the media content scripts need to know (the replay settings).
async fn sync_stream_config() {
    let config = with_state(|s| s.settings.media.stream_config());
    if let Err(e) = pf::set_stream_config(&config).await {
        log(&format!("could not store the stream settings: {e}"));
    }
}

/// Pauses background media beyond the limit, and resumes media we paused in tabs that
/// became active again.
async fn enforce_media() {
    let settings = with_state(|s| s.settings.media);
    if !settings.enabled() || with_state(|s| s.media.is_empty()) {
        return;
    }
    let media = with_state(|s| s.media.clone());
    let tabs = match pf::query_tabs().await {
        Ok(t) => t,
        Err(e) => return log(&format!("could not query tabs: {e}")),
    };
    let plan = media.plan(&settings, &tabs);
    if plan.is_empty() {
        return;
    }
    for id in &plan.pause {
        pf::media_command(*id, "pause", settings.include_audio).await;
    }
    for id in &plan.resume {
        pf::media_command(*id, "resume", settings.include_audio).await;
    }
    with_state(|s| s.media.apply(&plan));
    persist_soon();
}

// ---------------------------------------------------------------- tab strip

/// Saves the state soon, once for all the changes made meanwhile (for frequent changes
/// that don't need saving right away).
fn schedule_persist() {
    if PERSIST_PENDING.with(|p| p.replace(true)) {
        return;
    }
    spawn_local(async {
        pf::sleep(1000).await;
        // Saved meanwhile: nothing left to save.
        if PERSIST_PENDING.with(|p| p.get()) {
            persist().await;
        }
    });
}

/// Saves the state right after the current burst of changes (closing many tabs at once
/// saves once).
fn persist_soon() {
    if PERSIST_SOON.with(|p| p.replace(true)) {
        return;
    }
    spawn_local(async {
        pf::sleep(100).await;
        if PERSIST_SOON.with(|p| p.get()) {
            persist().await;
        }
    });
}

/// Debounced [`sync_tab_strip`]; a run asked for while one runs follows it.
fn schedule_strip_sync() {
    if STRIP_RUNNING.with(|r| r.get()) {
        STRIP_AGAIN.with(|a| a.set(true));
        return;
    }
    let generation = STRIP_GENERATION.with(|g| {
        g.set(g.get().wrapping_add(1));
        g.get()
    });
    spawn_local(async move {
        pf::sleep(300).await;
        if STRIP_GENERATION.with(|g| g.get()) != generation || STRIP_RUNNING.with(|r| r.get()) {
            return;
        }
        STRIP_RUNNING.with(|r| r.set(true));
        if let Err(e) = sync_tab_strip().await {
            log(&format!("could not update the tab strip: {e}"));
        }
        STRIP_RUNNING.with(|r| r.set(false));
        if STRIP_AGAIN.with(|a| a.replace(false)) {
            schedule_strip_sync();
        }
    });
}

/// Makes the browser's tab strip show the workspaces: hides other workspaces' tabs,
/// mirrors workspaces as native tab groups and puts the current one in the window title,
/// as enabled in the settings.
async fn sync_tab_strip() -> Result<(), String> {
    let ui = with_state(|s| s.settings.browser_ui);
    let caps = CAPS.with(|c| c.borrow().clone());
    // Nothing shown in the tab strip (and no title preface to set or take back): the tabs
    // needn't even be read.
    let preface = caps.title_preface
        && (with_state(|s| s.active_workspace.is_some() && ui.show_in_title)
            || TITLES.with(|t| t.borrow().values().any(|v| !v.is_empty())));
    if !(ui.hide_inactive && caps.hide_tabs && caps.hide_tabs_allowed) && !(ui.tab_groups && caps.tab_groups) && !preface {
        return Ok(());
    }
    let mut tabs = pf::query_tabs().await?;
    let mut opened = false;

    // Hide first: switching away from a hidden tab changes which group is collapsed.
    if ui.hide_inactive && caps.hide_tabs && caps.hide_tabs_allowed {
        let plan = STATE.with(|s| {
            let s = s.borrow();
            let by_tab = s.entries_by_tab();
            // Only tabs we track: tabs hidden by other extensions are not ours to show.
            let tracked: Vec<BrowserTab> = tabs.iter().filter(|t| by_tab.contains_key(&t.id)).cloned().collect();
            tab_strip::plan_visibility(s.active_workspace, &tracked, |t| by_tab.get(&t.id).and_then(|e| e.workspace))
        });
        if !plan.is_empty() {
            pf::show_tabs(&plan.show).await?;
            for id in &plan.activate {
                pf::activate_tab(*id).await?;
            }
            for window in &plan.new_tab_in {
                let tab = pf::create_tab(&CreateTab { active: true, window_id: Some(*window), ..Default::default() }).await?;
                with_state(|s| {
                    if let Some(w) = s.active_workspace {
                        s.pending_workspace.insert(tab.id, w);
                    }
                });
                opened = true;
            }
            pf::hide_tabs(&plan.hide).await?;
            tabs = pf::query_tabs().await?;
        }
    }

    if ui.tab_groups && caps.tab_groups {
        let groups = pf::query_tab_groups().await?;
        let last = GROUPS_COLLAPSED_FOR.with(|c| c.borrow().clone());
        let plan = STATE.with(|s| {
            let s = s.borrow();
            let by_tab = s.entries_by_tab();
            let workspace_of = |t: &BrowserTab| by_tab.get(&t.id).and_then(|e| e.workspace);
            tab_strip::plan_groups(&s.workspaces, &tabs, &groups, workspace_of, ui.collapse_inactive.then_some(&last))
        });
        for g in &plan.groups {
            let id = match g.group {
                Some(id) if g.tab_ids.is_empty() => id,
                group => pf::group_into(&g.tab_ids, group, g.window_id).await?,
            };
            if g.update || g.group.is_none() {
                let props = pf::GroupProps { title: &g.title, color: g.color, collapsed: g.collapsed };
                pf::update_tab_group(id, &props).await?;
            }
        }
        pf::ungroup_tabs(&plan.ungroup).await?;
        let current = if ui.collapse_inactive { plan.current } else { Default::default() };
        GROUPS_COLLAPSED_FOR.with(|c| *c.borrow_mut() = current);
    }

    if caps.title_preface {
        let text = with_state(|s| {
            let current = s.active_workspace.and_then(|id| s.workspace(id)).filter(|_| ui.show_in_title);
            tab_strip::title_preface(current)
        });
        let windows: std::collections::BTreeSet<TabId> = tabs.iter().map(|t| t.window_id).collect();
        // Closed windows are forgotten.
        TITLES.with(|t| t.borrow_mut().retain(|w, _| windows.contains(w)));
        for w in windows {
            // Windows start without a preface; only touch them once there is one.
            let last = TITLES.with(|t| t.borrow().get(&w).cloned()).unwrap_or_default();
            if last != text {
                pf::set_title_preface(w, &text).await?;
                TITLES.with(|t| t.borrow_mut().insert(w, text.clone()));
            }
        }
    }
    if opened {
        persist().await;
    }
    Ok(())
}

/// Shows the tabs hidden for workspaces (when hiding is turned off, or no workspace is current).
async fn show_workspace_tabs() {
    let ids: Vec<TabId> = match pf::query_tabs().await {
        Ok(tabs) => with_state(|s| {
            let by_tab = s.entries_by_tab();
            tabs.iter().filter(|t| t.hidden && by_tab.contains_key(&t.id)).map(|t| t.id).collect()
        }),
        Err(e) => return log(&format!("could not query tabs: {e}")),
    };
    if let Err(e) = pf::show_tabs(&ids).await {
        log(&format!("could not show tabs: {e}"));
    }
}

// ---------------------------------------------------------------- organize

async fn organize_tabs(key: OrganizeKey, native_groups: bool) -> Result<usize, String> {
    let tabs = pf::query_tabs().await?;
    let plans = STATE.with(|s| {
        let s = s.borrow();
        let by_tab = s.entries_by_tab();
        organize::plan(key, &tabs, |id| by_tab.get(&id).copied())
    });
    // While workspaces are shown as tab groups, those are the only groups we make.
    let use_groups = native_groups && CAPS.with(|c| c.borrow().tab_groups) && !with_state(|s| s.settings.browser_ui.tab_groups);
    let mut moved = 0;
    for p in &plans {
        if p.order.is_empty() {
            continue;
        }
        if use_groups {
            // Start from a clean slate so stale groups don't swallow moved tabs.
            pf::ungroup_tabs(&p.order).await?;
        }
        pf::move_tabs(&p.order, p.window_id, p.start_index).await?;
        moved += p.order.len();
        if use_groups {
            for (title, ids) in &p.groups {
                pf::group_tabs(ids, title, p.window_id).await?;
            }
        }
    }
    Ok(moved)
}

/// Debounced automatic re-organization after tab changes.
fn schedule_auto_organize() {
    let Some(key) = with_state(|s| s.settings.auto_organize) else { return };
    let generation = ORGANIZE_GENERATION.with(|g| {
        g.set(g.get().wrapping_add(1));
        g.get()
    });
    spawn_local(async move {
        pf::sleep(1500).await;
        if ORGANIZE_GENERATION.with(|g| g.get()) != generation {
            return;
        }
        let native = with_state(|s| s.settings.native_tab_groups);
        if let Err(e) = organize_tabs(key, native).await {
            log(&format!("auto-organize failed: {e}"));
        }
    });
}

// ---------------------------------------------------------------- cleanup

/// What a cleanup does in the open tabs of the sites it cleans.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InPages {
    /// Nothing (scheduled cleanups: no scripts in every tab every few minutes).
    Skip,
    /// Also clear their storage from inside (sessionStorage, frames of other sites).
    Clear,
    /// Stop them from saving, clear from inside, clean, then reload them, so nothing they
    /// held in memory comes back.
    ClearAndReload,
}

async fn run_cleanup(label: &str, spec: &CleanupSpec, pages: InPages) -> Result<CleanupReport, String> {
    let cookies = pf::get_all_cookies().await?;
    let tabs = pf::query_tabs().await?;
    let open_urls: Vec<String> = tabs.iter().map(|t| t.url.clone()).collect();
    let known = with_state(|s| s.known_hosts());
    let plan = cleanup::plan(spec, &cookies, &open_urls, &known)?;

    let mut warnings = Vec::new();
    let mut pages_cleared = 0;
    let page_tabs: Vec<TabId> = if pages == InPages::Skip {
        Vec::new()
    } else {
        let m = SpecMatcher::new(spec, &open_urls)?;
        tabs.iter().filter(|t| !t.discarded && m.covers_page(&t.url)).map(|t| t.id).collect()
    };
    if !page_tabs.is_empty() {
        match pf::clear_page_data(&page_tabs, &plan.data_types, pages == InPages::ClearAndReload).await {
            Ok(r) => {
                pages_cleared = r.cleared;
                if r.failed > 0 {
                    let why = r.error.map(|e| format!(" ({e})")).unwrap_or_default();
                    warnings.push(format!("{} tab(s) could not be cleared from inside the page{why}", r.failed));
                }
            }
            Err(e) => warnings.push(format!("open tabs could not be cleared from inside the page: {e}")),
        }
    }

    let (removed, cookie_warnings) = pf::remove_cookies(&plan.cookies).await;
    warnings.extend(cookie_warnings);
    let mut report = CleanupReport {
        at: pf::now(),
        label: if label.is_empty() { spec.target.describe() } else { label.to_string() },
        cookies_removed: removed,
        pages_cleared,
        ..Default::default()
    };
    match &plan.site_data_hosts {
        None => {
            report.all_sites = true;
            warnings.extend(pf::remove_site_data(None, &plan.data_types).await?);
        }
        Some(hosts) if !hosts.is_empty() => {
            report.hosts = hosts.clone();
            warnings.extend(pf::remove_site_data(Some(hosts), &plan.data_types).await?);
        }
        Some(_) => {}
    }
    if pages == InPages::ClearAndReload {
        for id in page_tabs {
            match pf::reload_tab(id).await {
                Ok(()) => report.pages_reloaded += 1,
                Err(e) => warnings.push(format!("tab {id} could not be reloaded: {e}")),
            }
        }
    }
    report.warnings = warnings;
    Ok(report)
}

async fn run_rule(id: Id, pages: InPages) -> Result<CleanupReport, String> {
    let rule = with_state(|s| s.cleanup_rules.iter().find(|r| r.id == id).cloned()).ok_or("no such cleanup rule")?;
    let report = run_cleanup(&rule.name, &rule.spec, pages).await?;
    with_state(|s| {
        if let Some(r) = s.cleanup_rules.iter_mut().find(|r| r.id == id) {
            r.last_run = Some(report.at);
        }
        s.push_cleanup_report(report.clone());
    });
    persist().await;
    Ok(report)
}

/// Makes browser alarms match the enabled interval rules, without resetting alarms that
/// are already correct (the background may wake up many times between firings).
async fn sync_alarms() {
    let desired: Vec<(String, f64)> = with_state(|s| {
        let mut v: Vec<(String, f64)> = s
            .cleanup_rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| r.interval_minutes.map(|m| (r.alarm_name(), m.max(1) as f64)))
            .collect();
        if let Some(m) = s.settings.auto_snapshot_minutes {
            v.push((SNAPSHOT_ALARM.to_string(), m.max(1) as f64));
        }
        v
    });
    let existing = match pf::get_alarms().await {
        Ok(a) => a,
        Err(e) => return log(&format!("could not list alarms: {e}")),
    };
    for a in existing.iter().filter(|a| a.name.starts_with(ALARM_PREFIX) || a.name == SNAPSHOT_ALARM) {
        if !desired.iter().any(|(n, p)| *n == a.name && Some(*p) == a.period_in_minutes) {
            let _ = pf::clear_alarm(&a.name).await;
        }
    }
    for (name, period) in &desired {
        if !existing.iter().any(|a| a.name == *name && a.period_in_minutes == Some(*period))
            && let Err(e) = pf::create_alarm(name, *period).await
        {
            log(&format!("could not create alarm {name}: {e}"));
        }
    }
}

// ---------------------------------------------------------------- AI summaries

/// Writes the AI summary of an entry's page. Returns false if it had a current summary
/// already (and `force` is off).
async fn summarize_entry(r: EntryRef, force: bool) -> Result<bool, String> {
    let (provider, max_chars, exclude) = with_state(|s| {
        let ai = &s.settings.ai;
        (ai.active_provider().cloned(), ai.max_input_chars.clamp(500, 200_000) as usize, ai.exclude_sites.clone())
    });
    let provider = provider.ok_or("AI is off, or no provider is set up (Settings → AI)")?;
    let e = with_state(|s| s.entry(r).cloned()).ok_or("tab no longer exists")?;
    if !force && !e.description.is_empty() && e.description_url == e.url {
        return Ok(false);
    }
    let tab = e.tab_id.ok_or("only open tabs can be summarised")?;
    if e.discarded {
        return Err("hibernated tabs can't be read; wake the tab first".into());
    }
    if !matches!(crate::url::scheme(&e.url).as_str(), "http" | "https" | "file") {
        return Err("only web pages can be summarised".into());
    }
    if guards::storage_blocked(&exclude, &e.host()) {
        return Err(format!("{} is excluded from AI summaries", e.host()));
    }
    let page = pf::page_text(tab, max_chars).await?;
    let title = if page.title.trim().is_empty() { e.title.clone() } else { page.title };
    let req = ai::summary_request(&provider, &title, &e.url, &page.text, max_chars)?;
    let resp = pf::http_request(&req, AI_TIMEOUT_MS).await;
    let summary = ai::parse_summary(provider.kind, &resp).map_err(|err| format!("{}: {err}", provider.name))?;
    with_state(|s| {
        // The tab may have navigated meanwhile: the summary is for the page that was read.
        if let Some(cur) = s.entry_mut(r)
            && cur.url == e.url
        {
            cur.description = summary;
            cur.description_url = e.url.clone();
        }
    });
    Ok(true)
}

async fn summarize(entries: &[EntryRef]) -> Result<Response, String> {
    if with_state(|s| s.settings.ai.active_provider().is_none()) {
        return Err("AI is off, or no provider is set up (Settings → AI)".into());
    }
    let (mut done, mut errors) = (0, Vec::new());
    for r in entries {
        match summarize_entry(*r, true).await {
            Ok(_) => done += 1,
            Err(e) => errors.push(e),
        }
    }
    if done > 0 {
        persist().await;
    }
    if done == 0 && !errors.is_empty() {
        return Err(errors.swap_remove(0));
    }
    let mut info = format!("Summarised {done} tab(s)");
    if !errors.is_empty() {
        info.push_str(&format!("; {} failed (e.g. {})", errors.len(), errors[0]));
    }
    ok(info)
}

/// Summarises a tab's page shortly after it navigated (automatic summaries).
fn schedule_auto_summary(tab: TabId) {
    let wanted = with_state(|s| s.settings.ai.auto_summarize && s.settings.ai.active_provider().is_some());
    if !wanted {
        return;
    }
    let generation = AI_PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let g = p.get(&tab).map_or(0, |g| g.wrapping_add(1));
        p.insert(tab, g);
        g
    });
    spawn_local(async move {
        pf::sleep(AUTO_SUMMARY_DELAY_MS).await;
        let current = AI_PENDING.with(|p| {
            let mut p = p.borrow_mut();
            let current = p.get(&tab) == Some(&generation);
            if current {
                p.remove(&tab);
            }
            current
        });
        if !current {
            return; // navigated again meanwhile
        }
        let Some(r) = with_state(|s| s.locate_tab(tab)) else { return };
        AI_QUEUE.with(|q| {
            let mut q = q.borrow_mut();
            if !q.contains(&r) {
                q.push_back(r);
            }
        });
        run_summary_queue().await;
    });
}

/// Works through the automatic summaries one at a time.
async fn run_summary_queue() {
    if AI_RUNNING.with(|r| r.replace(true)) {
        return;
    }
    while let Some(r) = AI_QUEUE.with(|q| q.borrow_mut().pop_front()) {
        if !with_state(|s| s.settings.ai.auto_summarize && s.settings.ai.active_provider().is_some()) {
            AI_QUEUE.with(|q| q.borrow_mut().clear());
            break;
        }
        match summarize_entry(r, false).await {
            Ok(true) => schedule_persist(),
            Ok(false) => {}
            Err(e) => log(&format!("automatic summary skipped: {e}")),
        }
    }
    AI_RUNNING.with(|r| r.set(false));
}

// ---------------------------------------------------------------- vector search

/// Embeds texts with the provider (when the index uses its embeddings) or locally.
async fn embed(model: &str, provider: Option<&ai::AiProvider>, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    if model == ai::LOCAL_MODEL {
        return Ok(texts.iter().map(|t| ai::local_embed(t)).collect());
    }
    let p = provider.ok_or("the AI provider of this index is gone")?;
    let mut out = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(EMBED_BATCH) {
        let req = ai::embedding_request(p, chunk).ok_or("the AI provider has no embedding model")?;
        let resp = pf::http_request(&req, AI_TIMEOUT_MS).await;
        out.extend(ai::parse_embeddings(&resp, chunk.len()).map_err(|e| format!("{}: {e}", p.name))?);
    }
    Ok(out)
}

/// Loads a session's vector index (memory, then storage) and brings it up to date with its
/// tabs, saving it if it changed.
async fn ensure_index(session: Id, model: &str, provider: Option<&ai::AiProvider>) -> Result<(), String> {
    // Its tabs are read back (and go again once the command is done).
    ensure_loaded(Some(&[session])).await?;
    if !VECTORS.with(|v| v.borrow().contains_key(&session)) {
        let stored = if DB_OK.with(Cell::get) { pf::db_get_vectors(session).await.ok().flatten() } else { None };
        let stored = stored.and_then(|json| serde_json::from_str::<VectorIndex>(&json).ok());
        let idx = stored.filter(|i| i.model == model).unwrap_or_else(|| VectorIndex::new(model));
        VECTORS.with(|v| v.borrow_mut().insert(session, idx));
    }
    let (stale, removed) = VECTORS.with(|v| {
        let mut v = v.borrow_mut();
        let idx = v.get_mut(&session).expect("inserted");
        if idx.model != model {
            *idx = VectorIndex::new(model);
        }
        STATE.with(|st| {
            let st = st.borrow();
            let entries = st.session(session).map_or(&[][..], |s| s.entries.as_slice());
            let removed = idx.retain_entries(entries);
            let stale: Vec<(Id, String)> = idx.stale(entries).into_iter().map(|(e, text)| (e.id, text)).collect();
            (stale, removed)
        })
    });
    if stale.is_empty() && !removed {
        return Ok(());
    }
    let texts: Vec<String> = stale.iter().map(|(_, t)| t.clone()).collect();
    let vectors = embed(model, provider, &texts).await?;
    let json = VECTORS.with(|v| {
        let mut v = v.borrow_mut();
        let idx = v.entry(session).or_insert_with(|| VectorIndex::new(model));
        for ((id, text), vec) in stale.iter().zip(&vectors) {
            idx.insert(*id, text, vec);
        }
        serde_json::to_string(idx)
    });
    if let Ok(json) = json
        && DB_OK.with(Cell::get)
        && let Err(e) = pf::db_put_vectors(session, &json).await
    {
        log(&format!("could not save the search index: {e}"));
    }
    Ok(())
}

/// Drops the vector indexes of the sessions that aren't active from memory. Returns how many.
fn free_inactive_vectors() -> usize {
    let active = with_state(|s| s.active_session);
    VECTORS.with(|v| {
        let mut v = v.borrow_mut();
        let before = v.len();
        v.retain(|id, _| *id == active);
        before - v.len()
    })
}

async fn vector_search(query: &str, sessions: Option<Vec<Id>>, limit: usize) -> Result<Response, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Response { ok: true, hits: Some(Vec::new()), ..Default::default() });
    }
    let (enabled, model, provider, unload, ids) = with_state(|s| {
        let ai = &s.settings.ai;
        let ids: Vec<Id> = match &sessions {
            Some(ids) => ids.iter().copied().filter(|id| s.session(*id).is_some()).collect(),
            None => s.sessions.iter().map(|x| x.id).collect(),
        };
        (ai.vector_search, ai.index_model(), ai.active_provider().cloned(), ai.unload_inactive, ids)
    });
    if !enabled {
        return Err("vector search is off (Settings → AI & search)".into());
    }
    let limit = limit.clamp(1, 500);
    let qv = embed(&model, provider.as_ref(), &[query.to_string()]).await?.pop().unwrap_or_default();
    let mut hits: Vec<VectorHit> = Vec::new();
    for session in &ids {
        ensure_index(*session, &model, provider.as_ref()).await?;
        VECTORS.with(|v| {
            if let Some(idx) = v.borrow().get(session) {
                hits.extend(
                    idx.search(&qv, limit)
                        .into_iter()
                        .map(|(entry, score)| VectorHit { entry: EntryRef { session: *session, entry }, score }),
                );
            }
        });
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(limit);
    if unload {
        free_inactive_vectors();
    }
    Ok(Response { ok: true, info: Some(format!("{} tab(s) found by meaning", hits.len())), hits: Some(hits), ..Default::default() })
}
