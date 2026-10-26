// Shared implementation of `globalThis.smPlatform`, the interface the Rust/wasm code
// uses to talk to the browser (see src/platform.rs). Each browser folder provides a
// `platform.js` that calls `installPlatform` with its API object and any overrides.

/** Normalizes a tabs.Tab into the shape Rust expects (model::BrowserTab). */
export function tabOut(t) {
  return {
    id: t.id,
    windowId: t.windowId,
    index: t.index,
    url: t.url || t.pendingUrl || '',
    title: t.title || '',
    favIconUrl: t.favIconUrl || null,
    pinned: !!t.pinned,
    active: !!t.active,
    discarded: !!t.discarded,
  };
}

export function cookieOut(c) {
  return {
    name: c.name,
    domain: c.domain,
    path: c.path,
    secure: !!c.secure,
    storeId: c.storeId ?? null,
    partitionKey: c.partitionKey ?? null,
  };
}

export function installPlatform(api, overrides = {}) {
  // Events that arrive before the wasm module is ready are queued, so a background
  // service worker woken up by an event doesn't lose it.
  const queue = [];
  let handler = null;

  function emit(ev) {
    if (handler) return Promise.resolve(handler(ev));
    return new Promise((resolve, reject) => queue.push({ ev, resolve, reject }));
  }

  function fire(ev) {
    emit(ev).catch((e) => console.error('[session-manager] event failed', ev.type, e));
  }

  const platform = {
    info() {
      return { browser: 'unknown', tabGroups: !!(api.tabs.group && api.tabGroups), perHostCache: false };
    },

    // ---- tabs ----
    async queryTabs() {
      return (await api.tabs.query({})).map(tabOut);
    },

    async createTab(o) {
      const props = { url: o.url, active: !!o.active, pinned: !!o.pinned };
      if (o.windowId != null) props.windowId = o.windowId;
      let tab = await api.tabs.create(props);
      if (o.discarded && !o.active && !o.pinned) {
        try {
          const d = await api.tabs.discard(tab.id);
          if (d) tab = d;
        } catch (e) {
          // Not fatal: the tab is open, just loaded.
          console.warn('[session-manager] could not discard tab', e);
        }
      }
      return tabOut(tab);
    },

    async createWindow() {
      const w = await api.windows.create({ focused: true });
      return { windowId: w.id, tabIds: (w.tabs || []).map((t) => t.id) };
    },

    async removeTabs(ids) {
      if (ids.length) await api.tabs.remove(ids);
    },

    async focusTab(id) {
      const t = await api.tabs.update(id, { active: true });
      await api.windows.update(t.windowId, { focused: true });
    },

    async moveTabs(ids, windowId, index) {
      if (ids.length) await api.tabs.move(ids, { windowId, index });
    },

    async groupTabs(ids, title, windowId) {
      if (!api.tabs.group || !ids.length) return false;
      const groupId = await api.tabs.group({ tabIds: ids, createProperties: { windowId } });
      if (api.tabGroups) await api.tabGroups.update(groupId, { title });
      return true;
    },

    async ungroupTabs(ids) {
      if (api.tabs.ungroup && ids.length) await api.tabs.ungroup(ids);
    },

    // ---- cookies / site data ----
    async getAllCookies() {
      const stores = await api.cookies.getAllCookieStores();
      const out = [];
      for (const store of stores) {
        const query = { storeId: store.id };
        let cookies;
        try {
          // Include partitioned (CHIPS / first-party isolated) cookies where supported.
          cookies = await api.cookies.getAll({ ...query, partitionKey: {} });
        } catch {
          cookies = await api.cookies.getAll(query);
        }
        out.push(...cookies.map(cookieOut));
      }
      return out;
    },

    async removeCookie(c) {
      const details = { url: c.url, name: c.name };
      if (c.storeId) details.storeId = c.storeId;
      if (c.partitionKey) details.partitionKey = c.partitionKey;
      await api.cookies.remove(details);
    },

    /** hosts: string[] or null (= all sites). Returns warnings. Browser specific. */
    async removeSiteData(_hosts, _types) {
      throw new Error('removeSiteData not implemented for this browser');
    },

    // ---- storage ----
    async storageGet(key) {
      const r = await api.storage.local.get(key);
      return r[key] ?? null;
    },

    async storageSet(key, value) {
      await api.storage.local.set({ [key]: value });
    },

    onStateChanged(key, cb) {
      api.storage.onChanged.addListener((changes, area) => {
        if (area === 'local' && changes[key]) cb(changes[key].newValue ?? null);
      });
    },

    // ---- alarms ----
    async getAlarms() {
      return (await api.alarms.getAll()).map((a) => ({ name: a.name, periodInMinutes: a.periodInMinutes ?? null }));
    },

    async createAlarm(name, periodInMinutes) {
      await api.alarms.create(name, { periodInMinutes, delayInMinutes: periodInMinutes });
    },

    async clearAlarm(name) {
      await api.alarms.clear(name);
    },

    // ---- messaging ----
    async sendMessage(payload) {
      const r = await api.runtime.sendMessage({ smPayload: payload });
      if (!r) throw new Error('background did not answer');
      if (r.smError) throw new Error(r.smError);
      return r.smResponse;
    },

    /** Opens (or focuses) the manager page, optionally at a view (`manager.html#backup`). */
    async openManager(view) {
      const base = api.runtime.getURL('manager.html');
      const url = view ? `${base}#${view}` : base;
      const existing = (await api.tabs.query({})).find((t) => (t.url || '').startsWith(base));
      if (existing) {
        await api.tabs.update(existing.id, { url, active: true });
        await api.windows.update(existing.windowId, { focused: true });
      } else {
        await api.tabs.create({ url, active: true });
      }
    },

    // ---- files / clipboard (UI pages only) ----

    /** Shows a file picker; resolves to { name, text } or null if cancelled. */
    pickTextFile(accept) {
      return new Promise((resolve, reject) => {
        const input = document.createElement('input');
        input.type = 'file';
        if (accept) input.accept = accept;
        input.style.display = 'none';
        input.addEventListener('cancel', () => {
          input.remove();
          resolve(null);
        });
        input.addEventListener('change', async () => {
          const file = input.files && input.files[0];
          input.remove();
          if (!file) return resolve(null);
          try {
            resolve({ name: file.name, text: await file.text() });
          } catch (e) {
            reject(e);
          }
        });
        document.body.appendChild(input);
        input.click();
      });
    },

    /** Reads text from the clipboard (needs the clipboardRead permission). */
    async readClipboard() {
      return await navigator.clipboard.readText();
    },

    /** Saves text as a downloaded file. */
    downloadText(filename, text, mime) {
      const blob = new Blob([text], { type: mime || 'text/plain' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = filename;
      a.style.display = 'none';
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 10000);
    },

    /** Removes the toolbar badge (after the release notes were seen). */
    clearBadge() {
      api.action?.setBadgeText({ text: '' })?.catch?.(() => {});
    },

    /** Current location hash without '#' (used to open the manager at a view). */
    locationHash() {
      return (globalThis.location && location.hash.replace(/^#/, '')) || '';
    },

    sleep(ms) {
      return new Promise((resolve) => setTimeout(resolve, ms));
    },

    // ---- background only ----
    setEventHandler(fn) {
      handler = fn;
      for (const { ev, resolve, reject } of queue.splice(0)) {
        Promise.resolve(fn(ev)).then(resolve, reject);
      }
    },

    /** Must be called synchronously at background startup (top level). */
    installBackgroundListeners() {
      // Flag new release notes on the toolbar icon after an install or update.
      api.runtime.onInstalled?.addListener((details) => {
        if (details.reason !== 'install' && details.reason !== 'update') return;
        Promise.resolve(api.action?.setBadgeText({ text: 'NEW' }))
          .then(() => api.action?.setBadgeBackgroundColor({ color: '#7c6cf6' }))
          .catch(() => {});
      });
      const TRACKED = ['url', 'title', 'favIconUrl', 'pinned', 'discarded'];
      api.tabs.onCreated.addListener((tab) => fire({ type: 'tabCreated', tab: tabOut(tab) }));
      api.tabs.onUpdated.addListener((_id, change, tab) => {
        if (!TRACKED.some((k) => k in change)) return;
        fire({ type: 'tabUpdated', tab: tabOut(tab), urlChanged: 'url' in change });
      });
      api.tabs.onRemoved.addListener((tabId, info) =>
        fire({ type: 'tabRemoved', tabId, windowClosing: !!(info && info.isWindowClosing) }),
      );
      api.tabs.onActivated.addListener((info) => fire({ type: 'tabActivated', tabId: info.tabId }));
      if (api.tabs.onReplaced) {
        api.tabs.onReplaced.addListener((added, removed) => fire({ type: 'tabReplaced', added, removed }));
      }
      api.cookies.onChanged.addListener((info) => {
        if (!info.removed) fire({ type: 'cookieSet', cookie: cookieOut(info.cookie) });
      });
      api.alarms.onAlarm.addListener((alarm) => fire({ type: 'alarm', name: alarm.name }));
      api.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
        if (!msg || typeof msg.smPayload !== 'string') return false;
        emit({ type: 'message', payload: msg.smPayload }).then(
          (r) => sendResponse({ smResponse: r }),
          (e) => sendResponse({ smError: String(e && e.message ? e.message : e) }),
        );
        return true; // async response
      });
    },
  };

  Object.assign(platform, overrides);
  globalThis.smPlatform = platform;
  return platform;
}
