//! Session Manager browser extension.
//!
//! The browser-independent core (model, search, organize, cleanup planning, state
//! transitions, protocol) is plain Rust and unit-tested natively. The wasm-only parts
//! talk to the browser through `globalThis.smPlatform`, a small JS shim that each
//! browser folder (`firefox/`, `chrome/`) provides.

pub mod ads;
pub mod cleanup;
pub mod dedupe;
pub mod guards;
pub mod hibernate;
pub mod media;
pub mod model;
pub mod netlog;
pub mod organize;
pub mod pattern;
pub mod protocol;
pub mod release_notes;
pub mod search;
pub mod state;
pub mod tab_strip;
pub mod transfer;
pub mod url;
pub mod workspace;

#[cfg(target_arch = "wasm32")]
mod background;
#[cfg(target_arch = "wasm32")]
mod platform;
#[cfg(target_arch = "wasm32")]
mod ui;

#[cfg(target_arch = "wasm32")]
pub use wasm::*;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use wasm_bindgen::prelude::*;

    /// Entry point for the background script / service worker.
    #[wasm_bindgen]
    pub async fn start_background() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        crate::background::start().await
    }

    /// Entry point for UI pages. `mode`: "popup" (toolbar quick switcher), "sidebar"
    /// (browser sidebar / side panel) or "manager" (full page).
    #[wasm_bindgen]
    pub async fn start_ui(canvas_id: String, mode: String) -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        crate::ui::start(&canvas_id, &mode).await
    }
}
