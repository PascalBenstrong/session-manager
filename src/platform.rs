//! Bindings to `globalThis.smPlatform`, the browser-specific JS shim
//! (see `web/platform-common.js` and `<browser>/platform.js`).

use js_sys::{Function, Promise};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::cleanup::{Cookie, CookieRemoval, DataTypes};
use crate::model::{BrowserTab, TabId};
use crate::protocol::{Capabilities, Command, Response};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = smPlatform, js_name = info)]
    fn js_info() -> JsValue;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = queryTabs)]
    fn js_query_tabs() -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = getTab)]
    fn js_get_tab(id: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = reloadTab)]
    fn js_reload_tab(id: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = createTab)]
    fn js_create_tab(opts: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = createWindow)]
    fn js_create_window() -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = removeTabs)]
    fn js_remove_tabs(ids: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = activateTab)]
    fn js_activate_tab(id: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = focusTab)]
    fn js_focus_tab(id: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = moveTabs)]
    fn js_move_tabs(ids: JsValue, window_id: f64, index: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = groupTabs)]
    fn js_group_tabs(ids: JsValue, title: &str, window_id: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = ungroupTabs)]
    fn js_ungroup_tabs(ids: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = discardTabs)]
    fn js_discard_tabs(ids: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = mediaCommand)]
    fn js_media_command(tab_id: f64, cmd: &str, audio: bool) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = queryTabGroups)]
    fn js_query_tab_groups() -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = groupInto)]
    fn js_group_into(ids: JsValue, group_id: JsValue, window_id: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = updateTabGroup)]
    fn js_update_tab_group(id: f64, props: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = hideTabs)]
    fn js_hide_tabs(ids: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = showTabs)]
    fn js_show_tabs(ids: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = setTitlePreface)]
    fn js_set_title_preface(window_id: f64, text: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = hasPermission)]
    fn js_has_permission(name: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = currentWindowId)]
    fn js_current_window_id() -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = openUrl)]
    fn js_open_url(url: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(js_namespace = smPlatform, js_name = setGestureAction)]
    pub fn set_gesture_action(name: &str);
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = syncContentScripts)]
    fn js_sync_content_scripts(regs: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = setBlockRules)]
    fn js_set_block_rules(rules: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = setScriptRewrite)]
    fn js_set_script_rewrite(enabled: bool, exceptions: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = openInspector)]
    fn js_open_inspector(tab_id: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = inspectorInject)]
    fn js_inspector_inject(tab_id: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = inspect)]
    fn js_inspect(tab_id: f64, request: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = collectResources)]
    fn js_collect_resources(tab_id: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(js_namespace = smPlatform, js_name = onInspectorPick)]
    pub fn on_inspector_pick(cb: &Function);
    #[wasm_bindgen(js_namespace = smPlatform, js_name = locationParam)]
    pub fn location_param(name: &str) -> String;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = getAllCookies)]
    fn js_get_all_cookies() -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = removeCookie)]
    fn js_remove_cookie(c: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = removeSiteData)]
    fn js_remove_site_data(hosts: JsValue, types: JsValue) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = storageGet)]
    fn js_storage_get(key: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = storageSet)]
    fn js_storage_set(key: &str, value: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = getAlarms)]
    fn js_get_alarms() -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = createAlarm)]
    fn js_create_alarm(name: &str, period_minutes: f64) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = clearAlarm)]
    fn js_clear_alarm(name: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = sendMessage)]
    fn js_send_message(payload: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = openManager)]
    fn js_open_manager(view: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = pickTextFile)]
    fn js_pick_text_file(accept: &str) -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = readClipboard)]
    fn js_read_clipboard() -> std::result::Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = smPlatform, js_name = downloadText)]
    fn js_download_text(filename: &str, text: &str, mime: &str) -> std::result::Result<(), JsValue>;
    #[wasm_bindgen(js_namespace = smPlatform, js_name = locationHash)]
    pub fn location_hash() -> String;
    #[wasm_bindgen(js_namespace = smPlatform, js_name = clearBadge)]
    pub fn clear_badge();
    #[wasm_bindgen(js_namespace = smPlatform, js_name = sleep)]
    fn js_sleep(ms: f64) -> Promise;
    #[wasm_bindgen(js_namespace = smPlatform, js_name = setEventHandler)]
    pub fn set_event_handler(handler: &Function);
    #[wasm_bindgen(js_namespace = smPlatform, js_name = onStateChanged)]
    pub fn on_state_changed(key: &str, cb: &Function);
}

pub type Result<T> = std::result::Result<T, String>;

pub fn js_error(e: JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    if let Some(err) = e.dyn_ref::<js_sys::Error>() {
        return String::from(err.message());
    }
    js_sys::JSON::stringify(&e).map(String::from).unwrap_or_else(|_| "unknown error".into())
}

async fn call(p: std::result::Result<Promise, JsValue>) -> Result<JsValue> {
    JsFuture::from(p.map_err(js_error)?).await.map_err(js_error)
}

async fn call_as<T: DeserializeOwned>(p: std::result::Result<Promise, JsValue>) -> Result<T> {
    serde_wasm_bindgen::from_value(call(p).await?).map_err(|e| e.to_string())
}

pub fn to_js<T: Serialize + ?Sized>(v: &T) -> JsValue {
    v.serialize(&serde_wasm_bindgen::Serializer::json_compatible()).unwrap_or(JsValue::NULL)
}

pub fn now() -> f64 {
    js_sys::Date::now()
}

pub fn capabilities() -> Capabilities {
    serde_wasm_bindgen::from_value(js_info()).unwrap_or_default()
}

pub async fn sleep(ms: u32) {
    let _ = JsFuture::from(js_sleep(ms as f64)).await;
}

// ---- tabs ----

pub async fn query_tabs() -> Result<Vec<BrowserTab>> {
    call_as(js_query_tabs()).await
}

/// A tab by id, or the active tab of the focused window when `id` is `None`.
pub async fn get_tab(id: Option<TabId>) -> Result<BrowserTab> {
    let id = id.map_or(JsValue::NULL, |id| JsValue::from_f64(id as f64));
    call_as(js_get_tab(id)).await
}

pub async fn reload_tab(id: TabId) -> Result<()> {
    call(js_reload_tab(id as f64)).await.map(drop)
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateTab<'a> {
    pub url: &'a str,
    pub title: &'a str,
    pub active: bool,
    pub discarded: bool,
    pub pinned: bool,
    pub window_id: Option<TabId>,
}

pub async fn create_tab(opts: &CreateTab<'_>) -> Result<BrowserTab> {
    call_as(js_create_tab(to_js(opts))).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWindow {
    pub window_id: TabId,
    pub tab_ids: Vec<TabId>,
}

pub async fn create_window() -> Result<NewWindow> {
    call_as(js_create_window()).await
}

pub async fn remove_tabs(ids: &[TabId]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    call(js_remove_tabs(to_js(ids))).await.map(drop)
}

pub async fn focus_tab(id: TabId) -> Result<()> {
    call(js_focus_tab(id as f64)).await.map(drop)
}

/// Selects a tab in its window, without focusing the window.
pub async fn activate_tab(id: TabId) -> Result<()> {
    call(js_activate_tab(id as f64)).await.map(drop)
}

pub async fn move_tabs(ids: &[TabId], window_id: TabId, index: i64) -> Result<()> {
    call(js_move_tabs(to_js(ids), window_id as f64, index as f64)).await.map(drop)
}

pub async fn group_tabs(ids: &[TabId], title: &str, window_id: TabId) -> Result<bool> {
    Ok(call(js_group_tabs(to_js(ids), title, window_id as f64)).await?.as_bool().unwrap_or(false))
}

pub async fn ungroup_tabs(ids: &[TabId]) -> Result<()> {
    call(js_ungroup_tabs(to_js(ids))).await.map(drop)
}

#[derive(Deserialize)]
pub struct Discarded {
    /// Ids of the hibernated tabs (Chromium may give a tab a new id when discarding it).
    pub discarded: Vec<TabId>,
    pub errors: Vec<String>,
}

/// Unloads tabs (keeping them in the tab strip).
pub async fn discard_tabs(ids: &[TabId]) -> Result<Discarded> {
    call_as(js_discard_tabs(to_js(ids))).await
}

pub async fn query_tab_groups() -> Result<Vec<crate::tab_strip::NativeGroup>> {
    call_as(js_query_tab_groups()).await
}

/// Adds tabs to a native group (`None`: a new group in `window_id`). Returns the group id.
pub async fn group_into(ids: &[TabId], group: Option<i64>, window_id: TabId) -> Result<i64> {
    let group = group.map_or(JsValue::NULL, |g| JsValue::from_f64(g as f64));
    let id = call(js_group_into(to_js(ids), group, window_id as f64)).await?;
    id.as_f64().map(|v| v as i64).ok_or_else(|| "the browser returned no group".to_string())
}

#[derive(Serialize)]
pub struct GroupProps<'a> {
    pub title: &'a str,
    pub color: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collapsed: Option<bool>,
}

pub async fn update_tab_group(id: i64, props: &GroupProps<'_>) -> Result<()> {
    call(js_update_tab_group(id as f64, to_js(props))).await.map(drop)
}

pub async fn hide_tabs(ids: &[TabId]) -> Result<()> {
    call(js_hide_tabs(to_js(ids))).await.map(drop)
}

pub async fn show_tabs(ids: &[TabId]) -> Result<()> {
    call(js_show_tabs(to_js(ids))).await.map(drop)
}

pub async fn set_title_preface(window_id: TabId, text: &str) -> Result<()> {
    call(js_set_title_preface(window_id as f64, text)).await.map(drop)
}

pub async fn has_permission(name: &str) -> bool {
    call(js_has_permission(name)).await.ok().and_then(|v| v.as_bool()).unwrap_or(false)
}

/// The window of this page (sidebar / side panel).
pub async fn current_window_id() -> Result<TabId> {
    let v = call(js_current_window_id()).await?;
    v.as_f64().map(|v| v as TabId).ok_or_else(|| "no window".to_string())
}

pub async fn open_url(url: &str) -> Result<()> {
    call(js_open_url(url)).await.map(drop)
}

// ---- page guards ----

/// Replaces our registered page scripts.
pub async fn sync_content_scripts(regs: &[crate::guards::ScriptRegistration]) -> Result<()> {
    call(js_sync_content_scripts(to_js(regs))).await.map(drop)
}

/// Replaces our declarativeNetRequest dynamic rules.
pub async fn set_block_rules(rules: &[serde_json::Value]) -> Result<()> {
    call(js_set_block_rules(to_js(rules))).await.map(drop)
}

/// Firefox: blank out `debugger` statements in script files, except on `exceptions`.
pub async fn set_script_rewrite(enabled: bool, exceptions: &[String]) -> Result<()> {
    call(js_set_script_rewrite(enabled, to_js(exceptions))).await.map(drop)
}

// ---- inspector ----

pub async fn open_inspector(tab_id: TabId) -> Result<()> {
    call(js_open_inspector(tab_id as f64)).await.map(drop)
}

pub async fn inspector_inject(tab_id: TabId) -> Result<()> {
    call(js_inspector_inject(tab_id as f64)).await.map(drop)
}

/// A request (`{"op": ..., ...}`) to the inspector content script in a tab.
pub async fn inspect(tab_id: TabId, request: &serde_json::Value) -> Result<serde_json::Value> {
    let v = call(js_inspect(tab_id as f64, to_js(request))).await?;
    if v.is_null() || v.is_undefined() {
        return Ok(serde_json::Value::Null);
    }
    serde_wasm_bindgen::from_value(v).map_err(|e| e.to_string())
}

/// A URL found on a page (see `collectPageResources` in platform-common.js).
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct PageResource {
    /// Element type: video, audio, source, track, poster, iframe, frame, embed, object,
    /// script, or "network" (a media request).
    pub kind: String,
    pub url: String,
    pub detail: String,
    /// The frame it was found in.
    pub frame: String,
    pub top: bool,
}

pub async fn collect_resources(tab_id: TabId) -> Result<Vec<PageResource>> {
    call_as(js_collect_resources(tab_id as f64)).await
}

/// Sends `"pause"` / `"resume"` to the media content script of every frame of a tab.
/// `audio` includes audio-only media. Tabs without the content script are ignored.
pub async fn media_command(tab_id: TabId, cmd: &str, audio: bool) {
    let _ = call(js_media_command(tab_id as f64, cmd, audio)).await;
}

// ---- cookies / site data ----

pub async fn get_all_cookies() -> Result<Vec<Cookie>> {
    call_as(js_get_all_cookies()).await
}

/// Starts removing all cookies concurrently; returns how many succeeded.
pub async fn remove_cookies(cookies: &[CookieRemoval]) -> (usize, Vec<String>) {
    let promises = js_sys::Array::new();
    for c in cookies {
        match js_remove_cookie(to_js(c)) {
            Ok(p) => {
                promises.push(&p);
            }
            Err(e) => {
                promises.push(&Promise::reject(&e));
            }
        }
    }
    let settled = match JsFuture::from(Promise::all_settled(&promises)).await {
        Ok(v) => js_sys::Array::from(&v),
        Err(e) => return (0, vec![js_error(e)]),
    };
    let mut ok = 0;
    let mut errors = Vec::new();
    for r in settled.iter() {
        let status = js_sys::Reflect::get(&r, &"status".into()).ok().and_then(|s| s.as_string());
        if status.as_deref() == Some("fulfilled") {
            ok += 1;
        } else if errors.len() < 3 {
            let reason = js_sys::Reflect::get(&r, &"reason".into()).unwrap_or(JsValue::UNDEFINED);
            errors.push(format!("cookie removal failed: {}", js_error(reason)));
        }
    }
    (ok, errors)
}

/// Removes non-cookie site data. `hosts == None` means every site. Returns warnings.
pub async fn remove_site_data(hosts: Option<&[String]>, types: &DataTypes) -> Result<Vec<String>> {
    let hosts = match hosts {
        Some(h) => to_js(h),
        None => JsValue::NULL,
    };
    call_as(js_remove_site_data(hosts, to_js(types))).await
}

// ---- storage ----

pub async fn storage_get(key: &str) -> Result<Option<String>> {
    Ok(call(js_storage_get(key)).await?.as_string())
}

pub async fn storage_set(key: &str, value: &str) -> Result<()> {
    call(js_storage_set(key, value)).await.map(drop)
}

// ---- alarms ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alarm {
    pub name: String,
    pub period_in_minutes: Option<f64>,
}

pub async fn get_alarms() -> Result<Vec<Alarm>> {
    call_as(js_get_alarms()).await
}

pub async fn create_alarm(name: &str, period_minutes: f64) -> Result<()> {
    call(js_create_alarm(name, period_minutes)).await.map(drop)
}

pub async fn clear_alarm(name: &str) -> Result<()> {
    call(js_clear_alarm(name)).await.map(drop)
}

// ---- UI <-> background messaging ----

pub async fn send_command(cmd: &Command) -> Response {
    let payload = match serde_json::to_string(cmd) {
        Ok(p) => p,
        Err(e) => return Response::err(e.to_string()),
    };
    match call(js_send_message(&payload)).await {
        Ok(v) => match v.as_string() {
            Some(s) => serde_json::from_str(&s).unwrap_or_else(|e| Response::err(format!("bad response: {e}"))),
            None => Response::err("no response from background"),
        },
        Err(e) => Response::err(e),
    }
}

/// Opens (or focuses) the manager page, optionally at a view name (`""` = default).
pub async fn open_manager(view: &str) -> Result<()> {
    call(js_open_manager(view)).await.map(drop)
}

// ---- files / clipboard ----

#[derive(Deserialize)]
pub struct PickedFile {
    pub name: String,
    pub text: String,
}

/// Lets the user pick a text file. `Ok(None)` when cancelled.
pub async fn pick_text_file(accept: &str) -> Result<Option<PickedFile>> {
    let v = call(js_pick_text_file(accept)).await?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    serde_wasm_bindgen::from_value(v).map(Some).map_err(|e| e.to_string())
}

pub async fn read_clipboard() -> Result<String> {
    Ok(call(js_read_clipboard()).await?.as_string().unwrap_or_default())
}

pub fn download_text(filename: &str, text: &str, mime: &str) -> Result<()> {
    js_download_text(filename, text, mime).map_err(js_error)
}

