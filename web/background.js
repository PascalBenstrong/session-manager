// Background entry (Firefox event page / Chrome service worker).
// `platform.js` is the browser-specific shim copied in from the browser folder at build time.
import './platform.js';
import init, { start_background } from './pkg/session_manager.js';

// Listeners must be registered synchronously during startup so that the event which
// woke the background up is not lost; they queue events until the wasm is ready.
globalThis.smPlatform.installBackgroundListeners();

init()
  .then(() => start_background())
  .catch((e) => console.error('[session-manager] background failed to start', e));
