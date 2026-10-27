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

