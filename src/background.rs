//! Background script: owns the [`AppState`], reacts to browser events and executes
//! commands sent by the UI. All state transitions are synchronous (see `state.rs`);
//! only browser calls are async, so no `RefCell` borrow is held across an `.await`.

use std::cell::{Cell, RefCell};

use js_sys::Promise;
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

use crate::cleanup::{self, ALARM_PREFIX, CleanupReport, CleanupSpec, CleanupTarget, Cookie, SpecMatcher};
use crate::dedupe::Scope;
use crate::guards::{self, BlockRule, GuardSettings, PopupEvent};
use crate::hibernate::{self, HibernateTarget};
use crate::media::MediaReport;
use crate::model::{AppState, BrowserTab, EntryRef, Id, TabId};
use crate::organize::{self, OrganizeKey};
use crate::platform::{self as pf, CreateTab};
use crate::protocol::{Capabilities, Command, Response, STATE_KEY};
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
    /// A save is scheduled (see schedule_persist).
    static PERSIST_PENDING: Cell<bool> = const { Cell::new(false) };
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
    static GUARDS_SYNCED: RefCell<Option<(GuardSettings, Vec<BlockRule>, bool)>> = const { RefCell::new(None) };
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

fn snapshot() -> AppState {
    STATE.with(|s| s.borrow().clone())
}

fn log(msg: &str) {
    web_sys::console::warn_1(&format!("[session-manager] {msg}").into());
}

pub async fn start() -> Result<(), JsValue> {
    STARTED_AT.with(|t| t.set(pf::now()));
    CAPS.with(|c| *c.borrow_mut() = pf::capabilities());
    refresh_permissions().await;

    let mut state = match pf::storage_get(STATE_KEY).await {
        Ok(Some(json)) => serde_json::from_str::<AppState>(&json).unwrap_or_else(|e| {
            log(&format!("stored state unreadable, starting fresh: {e}"));
            AppState::default()
        }),
        Ok(None) => AppState::default(),
        Err(e) => {
            log(&format!("could not read storage: {e}"));
            AppState::default()
        }
    };
    state.normalize();
    match pf::query_tabs().await {
        Ok(tabs) => state.reconcile(&tabs, pf::now()),
        Err(e) => log(&format!("could not query tabs: {e}")),
    }
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
    Ok(())
}

/// Registers the page guard scripts and script blocking rules for the current settings
/// (only when they changed since the last time).
async fn sync_guards() {
    let wanted = with_state(|s| (s.settings.guards.clone(), s.block_rules.clone(), s.settings.media.needs_page_script()));
    if GUARDS_SYNCED.with(|g| g.borrow().as_ref() == Some(&wanted)) {
        return;
    }
    let (g, rules, media) = &wanted;
    let mut regs = guards::registrations(g, rules);
    if *media {
        regs.push(guards::media_registration());
    }
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
    let opener_url = pf::get_tab(Some(opener)).await.map(|t| t.url).unwrap_or_default();
    if !matches!(crate::url::scheme(&opener_url).as_str(), "http" | "https") {
        return false;
    }
    let rule = with_state(|s| {
        let rule = guards::popup_rule_for(&s.block_rules, &tab.url);
        s.push_popup(PopupEvent { at: now, opener: opener_url, url: tab.url.clone(), closed_by: rule });
        rule
    });
    persist().await;
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

async fn persist() {
    let json = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let next = REVISION.with(|r| {
            let n = r.get().max(s.revision) + 1;
            r.set(n);
            n
        });
        s.revision = next;
        serde_json::to_string(&*s)
    });
    match json {
        Ok(json) => {
            if let Err(e) = pf::storage_set(STATE_KEY, &json).await {
                log(&format!("could not save state: {e}"));
            }
        }
        Err(e) => log(&format!("could not serialize state: {e}")),
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
                if with_state(|s| s.link_by_url(&tab)) {
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
                    persist().await;
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
                if url_changed || moved {
                    persist().await;
                } else {
                    // Titles, icons, loading: frequent, saved together.
                    schedule_persist();
                }
                if url_changed {
                    schedule_auto_organize();
                }
                schedule_strip_sync();
            }
        }
        Event::TabRemoved { tab_id, window_closing } => {
            if with_state(|s| s.on_tab_removed(tab_id, window_closing)) {
                // A closing window removes all its tabs: saved together.
                if window_closing {
                    schedule_persist();
                } else {
                    persist().await;
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
        Event::MediaState { tab_id, frame_id, playing, video, pip } => {
            // Tracked also without the media limit: the sidebar shows it, and stops streams.
            let report = MediaReport { playing, video, pip };
            if with_state(|s| s.media.report(tab_id, frame_id, report, now)) {
                // Playback changes often (previews, ads): saved at most once a second.
                schedule_persist();
                enforce_media().await;
            }
        }
        Event::Stream { tab_id, frame_id, what } => match what.as_str() {
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
            if with_state(|s| s.auto_snapshot_all(now)) > 0 {
                persist().await;
            }
        }
        Event::Alarm { name } => {
            if let Some(id) = name.strip_prefix(ALARM_PREFIX).and_then(|s| s.parse::<Id>().ok())
                && let Err(e) = run_rule(id).await
            {
                log(&format!("scheduled cleanup failed: {e}"));
            }
        }
        Event::Message { payload } => {
            let resp = match serde_json::from_str::<Command>(&payload) {
                Ok(cmd) => execute(cmd).await,
                Err(e) => Response::err(format!("bad command: {e}")),
            };
            let json = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
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
                s.link_tab(existing, tab);
                s.on_tab_upsert(tab, now);
            });
            persist().await;
        }
    }
    true
}

/// "Clean up whenever a new value is set": remove freshly set cookies matching a rule.
async fn on_cookie_set(cookie: Cookie) {
    let (rules, open_urls) = STATE.with(|s| {
        let s = s.borrow();
        let rules: Vec<CleanupSpec> =
            s.cleanup_rules.iter().filter(|r| r.enabled && r.on_cookie_set && r.spec.data_types.cookies).map(|r| r.spec.clone()).collect();
        let urls = open_urls(&s);
        (rules, urls)
    });
    let hit = rules.iter().filter_map(|spec| SpecMatcher::new(spec, &open_urls).ok()).any(|m| m.matches_cookie(&cookie));
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
        STARTUP_DEDUPE.with(|d| d.set(None));
        let sessions: Vec<Id> = with_state(|s| s.sessions.iter().map(|s| s.id).collect());
        match remove_duplicates_in(&sessions, Some(started)).await {
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

fn open_urls(s: &AppState) -> Vec<String> {
    s.sessions.iter().flat_map(|s| s.entries.iter()).filter(|e| e.is_open()).map(|e| e.url.clone()).collect()
}

// ---------------------------------------------------------------- commands

async fn execute(cmd: Command) -> Response {
    if cmd != Command::GetState {
        // Most commands change what the tab strip should show; the sync is debounced and
        // does nothing when nothing changed.
        schedule_strip_sync();
    }
    let result = run_command(cmd).await;
    let mut resp = match result {
        Ok(r) => r,
        Err(e) => Response::err(e),
    };
    resp.state = Some(snapshot());
    resp.capabilities = Some(CAPS.with(|c| c.borrow().clone()));
    resp
}

fn ok(info: impl Into<String>) -> Result<Response, String> {
    Ok(Response { ok: true, info: Some(info.into()), ..Default::default() })
}

async fn run_command(cmd: Command) -> Result<Response, String> {
    let now = pf::now();
    match cmd {
        Command::GetState => Ok(Response { ok: true, ..Default::default() }),
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
            let tabs = with_state(|s| s.close_entries(&entries));
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
            stop_stream(tab, stop).await;
            ok(if stop { "Stream stopped: no more data until you resume it" } else { "Stream resumed" })
        }
        Command::StopBackgroundStreams => {
            if !with_state(|s| s.settings.media.stream_controls) {
                return Err("stream controls are off (Settings → Performance)".into());
            }
            let tabs = pf::query_tabs().await?;
            let ids = with_state(|s| s.media.background_streams(&tabs));
            for id in &ids {
                stop_stream(*id, true).await;
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
            let report = run_cleanup(&label, &spec).await?;
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
            let report = run_rule(id).await?;
            Ok(Response { ok: true, info: Some(report.summary()), report: Some(report), ..Default::default() })
        }
        Command::CreateSnapshot { session, name } => {
            let id = with_state(|s| s.create_snapshot(session, &name, false, now))?;
            persist().await;
            let name = with_state(|s| s.session(session).and_then(|x| x.snapshot(id)).map(|x| x.name.clone()).unwrap_or_default());
            ok(format!("Snapshot \"{name}\" saved"))
        }
        Command::RestoreSnapshot { session, snapshot, mode, open } => {
            let (target, added) = with_state(|s| s.restore_snapshot(session, snapshot, mode, now))?;
            persist().await;
            let mut info = match mode {
                RestoreMode::Replace => "Snapshot restored (previous tabs saved as a snapshot)".to_string(),
                RestoreMode::Merge => format!("Added {} tab(s) from the snapshot", added.len()),
                RestoreMode::NewSession => "Snapshot opened as a new session".to_string(),
            };
            if open && !added.is_empty() {
                let (opened, _) = open_entries(target, Some(&added), true, false).await?;
                info.push_str(&format!("; opened {opened} unloaded"));
            }
            ok(info)
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
            let tab = pf::get_tab(tab).await?;
            let host = crate::url::display_host(&tab.url);
            let spec = CleanupSpec { target: CleanupTarget::Pages(vec![tab.url.clone()]), data_types, exclude: Vec::new() };
            let report = run_cleanup(&format!("Tab: {host}"), &spec).await?;
            with_state(|s| s.push_cleanup_report(report.clone()));
            persist().await;
            if reload {
                pf::reload_tab(tab.id).await?;
            }
            Ok(Response { ok: true, info: Some(report.summary()), report: Some(report), ..Default::default() })
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
                with_state(|s| s.link_tab(EntryRef { session: id, entry: *entry }, &tab));
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
                    if let Some(e) = s.entry_mut(r) {
                        e.tab_id = None;
                        e.window_id = None;
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
        with_state(|s| s.link_tab(r, &tab));
        if !discarded {
            pf::focus_tab(tab.id).await?;
        }
    }
    with_state(|s| {
        if let Some(e) = s.entry_mut(r) {
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
    if with_state(|s| s.mark_discarded(&result.discarded)) {
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

/// Stops (pauses, and holds back the media data of) or resumes a tab's streams.
async fn stop_stream(tab: TabId, stop: bool) {
    if !with_state(|s| s.media.set_stopped(tab, stop)) {
        return;
    }
    persist().await;
    // Data stops before the player is paused, and flows again before it plays.
    sync_stream_holds().await;
    pf::stream_command(tab, if stop { "stop" } else { "resume" }).await;
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
    persist().await;
}

// ---------------------------------------------------------------- tab strip

fn workspace_of(s: &AppState, t: &BrowserTab) -> Option<Id> {
    s.entry_for_tab(t.id).and_then(|e| e.workspace)
}

/// Debounced [`sync_tab_strip`]; a run asked for while one runs follows it.
/// Saves the state soon, once for all the changes made meanwhile (for frequent changes
/// that don't need saving right away).
fn schedule_persist() {
    if PERSIST_PENDING.with(|p| p.replace(true)) {
        return;
    }
    spawn_local(async {
        pf::sleep(1000).await;
        PERSIST_PENDING.with(|p| p.set(false));
        persist().await;
    });
}

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
    let mut tabs = pf::query_tabs().await?;
    let mut opened = false;

    // Hide first: switching away from a hidden tab changes which group is collapsed.
    if ui.hide_inactive && caps.hide_tabs && caps.hide_tabs_allowed {
        let (current, tracked) = with_state(|s| {
            // Only tabs we track: tabs hidden by other extensions are not ours to show.
            let tracked: Vec<BrowserTab> = tabs.iter().filter(|t| s.locate_tab(t.id).is_some()).cloned().collect();
            (s.active_workspace, tracked)
        });
        let plan = STATE.with(|s| {
            let s = s.borrow();
            tab_strip::plan_visibility(current, &tracked, |t| workspace_of(&s, t))
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
            tab_strip::plan_groups(&s.workspaces, &tabs, &groups, |t| workspace_of(&s, t), ui.collapse_inactive.then_some(&last))
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
        Ok(tabs) => with_state(|s| tabs.iter().filter(|t| t.hidden && s.locate_tab(t.id).is_some()).map(|t| t.id).collect()),
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
        organize::plan(key, &tabs, |id| s.entry_for_tab(id))
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

async fn run_cleanup(label: &str, spec: &CleanupSpec) -> Result<CleanupReport, String> {
    let cookies = pf::get_all_cookies().await?;
    let open_urls: Vec<String> = pf::query_tabs().await?.into_iter().map(|t| t.url).collect();
    let known = with_state(|s| s.known_hosts());
    let plan = cleanup::plan(spec, &cookies, &open_urls, &known)?;

    let (removed, mut warnings) = pf::remove_cookies(&plan.cookies).await;
    let mut report = CleanupReport {
        at: pf::now(),
        label: if label.is_empty() { spec.target.describe() } else { label.to_string() },
        cookies_removed: removed,
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
    report.warnings = warnings;
    Ok(report)
}

async fn run_rule(id: Id) -> Result<CleanupReport, String> {
    let rule = with_state(|s| s.cleanup_rules.iter().find(|r| r.id == id).cloned()).ok_or("no such cleanup rule")?;
    let report = run_cleanup(&rule.name, &rule.spec).await?;
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
