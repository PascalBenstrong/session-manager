// UI entry for popup.html and manager.html.
import './platform.js';
import init, { start_ui } from './pkg/session_manager.js';

const popup = document.body.dataset.popup === 'true';

init()
  .then(() => start_ui('sm-canvas', popup))
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
