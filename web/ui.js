// UI entry for popup.html, sidebar.html and manager.html.
import './platform.js';
import init, { start_ui } from './pkg/session_manager.js';

// eframe draws the glyphs our fonts lack (the emoji icons) on a 2D canvas and reads them back
// for every new glyph: such canvases are faster (and the browser stops warning) when they
// say so up front.
const getContext = HTMLCanvasElement.prototype.getContext;
HTMLCanvasElement.prototype.getContext = function (type, options) {
  return getContext.call(this, type, type === '2d' ? { willReadFrequently: true, ...options } : options);
};

const mode = document.body.dataset.mode || (document.body.dataset.popup === 'true' ? 'popup' : 'manager');

init()
  .then(() => start_ui('sm-canvas', mode))
  .then(() => {
    document.getElementById('sm-loading')?.remove();
    // Focus the canvas so typing goes straight into the search box.
    document.getElementById('sm-canvas')?.focus();
  })
  .catch((e) => {
    console.error('[session-manager] UI failed to start', e);
    const el = document.getElementById('sm-loading');
    if (el) el.textContent = `Failed to start: ${e}`;
  });
