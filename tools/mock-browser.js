// In-memory mock of the subset of the WebExtension API the extension uses.
// Shared by tools/smoke-test.mjs (Node) and tools/preview.html (browser).
export function createMockBrowser() {
  const listeners = (name) => {
    const set = [];
    return { addListener: (f) => set.push(f), fire: (...a) => set.map((f) => f(...a)), name };
  };
  let nextTabId = 1;
  const tabs = new Map();
  const cookies = [];
  const storage = {};
  const alarms = new Map();
  const removedSiteData = [];
  const reloaded = [];
  const tabMessages = [];
  const ev = {
    created: listeners(), updated: listeners(), removed: listeners(), activated: listeners(),
    cookie: listeners(), alarm: listeners(), message: listeners(), storage: listeners(), installed: listeners(),
    startup: listeners(),
  };
  const clone = (o) => JSON.parse(JSON.stringify(o));

  const api = {
    tabs: {
      async query(q) {
        return [...tabs.values()].filter((t) => (!q.url || t.url === q.url) && (!q.active || t.active)).map(clone);
      },
      async get(id) {
        if (!tabs.has(id)) throw new Error(`No tab with id: ${id}`);
        return clone(tabs.get(id));
      },
      async reload(id) { reloaded.push(id); },
      async create(p) {
        const t = { id: nextTabId++, windowId: p.windowId ?? 1, index: tabs.size, url: p.url, title: p.title || '',
          pinned: !!p.pinned, active: !!p.active, discarded: !!p.discarded };
        tabs.set(t.id, t);
        ev.created.fire(clone(t));
        return clone(t);
      },
      async update(id, p) { Object.assign(tabs.get(id), p); return clone(tabs.get(id)); },
      async remove(ids) {
        for (const id of [].concat(ids)) { tabs.delete(id); ev.removed.fire(id, { isWindowClosing: false, windowId: 1 }); }
      },
      async move(ids, { index }) {
        const moved = ids.map((id) => tabs.get(id));
        const rest = [...tabs.values()].filter((t) => !ids.includes(t.id)).sort((a, b) => a.index - b.index);
        rest.splice(index, 0, ...moved);
        rest.forEach((t, i) => (t.index = i));
      },
      async discard(id) { tabs.get(id).discarded = true; return clone(tabs.get(id)); },
      // Messages to content scripts.
      async sendMessage(tabId, msg) { tabMessages.push({ tabId, msg }); },
      onCreated: ev.created, onUpdated: ev.updated, onRemoved: ev.removed, onActivated: ev.activated,
    },
    windows: {
      async create() { return { id: 2, tabs: [] }; },
      async update() {},
    },
    cookies: {
      async getAllCookieStores() { return [{ id: 'default' }]; },
      async getAll() { return clone(cookies); },
      async remove({ url, name }) {
        const host = new URL(url).hostname;
        const i = cookies.findIndex((c) => c.name === name && c.domain.replace(/^\./, '') === host);
        if (i >= 0) cookies.splice(i, 1);
        return {};
      },
      onChanged: ev.cookie,
    },
    browsingData: { async remove(opts, types) { removedSiteData.push({ opts, types }); } },
    storage: {
      local: {
        async get(k) { return k in storage ? { [k]: storage[k] } : {}; },
        async set(o) {
          for (const [k, v] of Object.entries(o)) {
            const old = storage[k];
            storage[k] = v;
            ev.storage.fire({ [k]: { oldValue: old, newValue: v } }, 'local');
          }
        },
      },
      onChanged: ev.storage,
    },
    alarms: {
      async getAll() { return [...alarms.values()]; },
      async create(name, o) { alarms.set(name, { name, periodInMinutes: o.periodInMinutes }); },
      async clear(name) { return alarms.delete(name); },
      onAlarm: ev.alarm,
    },
    runtime: {
      getURL: (p) => `moz-extension://test/${p}`,
      // In a real browser the UI's message reaches the background's onMessage listener.
      sendMessage(msg) {
        return new Promise((resolve) => {
          let handled = false;
          for (const r of ev.message.fire(msg, {}, resolve)) handled ||= r === true;
          if (!handled) resolve(undefined);
        });
      },
      onMessage: ev.message,
      onInstalled: ev.installed,
      onStartup: ev.startup,
    },
    action: {
      badge: '',
      async setBadgeText({ text }) { api.action.badge = text; },
      async setBadgeBackgroundColor() {},
    },
  };
  return { api, tabs, cookies, storage, alarms, removedSiteData, reloaded, tabMessages, ev, clone };
}
