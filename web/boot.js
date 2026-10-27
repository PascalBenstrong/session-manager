// Registered (document start, top frame) only while an inspector is docked in some tab:
// tells the background a page is starting, so a docked inspector comes back right away
// instead of after the page has loaded. It does nothing else.
(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  if (!api || !api.runtime || !api.runtime.id) return;
  try {
    const p = api.runtime.sendMessage({ smBoot: true });
    if (p && p.catch) p.catch(() => {});
  } catch {
    // The extension was reloaded.
  }
})();
