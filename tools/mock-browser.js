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
  const groups = new Map();
  let nextGroupId = 100;
  const granted = new Set();
  const titlePrefaces = {};
  const contentScripts = [];
  const createdWindows = [];
  // webRequest (Firefox script filter): blocking listeners and response filters.
  const requestListeners = [];
  const filters = new Map();
  let dnrRules = [];
  let dnrSessionRules = [];
  const headerListeners = [];
  const ev = {
    created: listeners(), updated: listeners(), removed: listeners(), activated: listeners(),
    cookie: listeners(), alarm: listeners(), message: listeners(), storage: listeners(), installed: listeners(),
    startup: listeners(), groupUpdated: listeners(), permAdded: listeners(), permRemoved: listeners(),
  };
  const clone = (o) => JSON.parse(JSON.stringify(o));
  // Puts tabs in a native group (-1: none), like the browser, and drops emptied groups.
  const setGroup = (ids, groupId) => {
    for (const id of ids) {
      const t = tabs.get(id);
      if (t.groupId === groupId) continue;
      t.groupId = groupId;
      ev.updated.fire(id, { groupId }, clone(t));
    }
    for (const g of groups.keys()) {
      if (![...tabs.values()].some((t) => t.groupId === g)) groups.delete(g);
    }
  };

  const api = {
    tabs: {
      async query(q) {
        return [...tabs.values()]
          .filter((t) => (!q.url || t.url === q.url) && (!q.active || t.active) && (q.groupId == null || t.groupId === q.groupId))
          .map(clone);
      },
      async get(id) {
        if (!tabs.has(id)) throw new Error(`No tab with id: ${id}`);
        return clone(tabs.get(id));
      },
      async reload(id) { reloaded.push(id); },
      async create(p) {
        const t = { id: nextTabId++, windowId: p.windowId ?? 1, index: tabs.size, url: p.url, title: p.title || '',
          pinned: !!p.pinned, active: !!p.active, discarded: !!p.discarded, groupId: -1, hidden: false,
          openerTabId: p.openerTabId };
        if (t.active) for (const o of tabs.values()) if (o.windowId === t.windowId) o.active = false;
        tabs.set(t.id, t);
        ev.created.fire(clone(t));
        return clone(t);
      },
      async update(id, p) {
        const t = tabs.get(id);
        if (p.active) for (const o of tabs.values()) if (o.windowId === t.windowId) o.active = false;
        Object.assign(t, p);
        return clone(t);
      },
      async group({ tabIds, groupId, createProperties }) {
        let id = groupId;
        if (id == null) {
          id = nextGroupId++;
          groups.set(id, { id, windowId: createProperties?.windowId ?? 1, title: '', color: 'grey', collapsed: false });
        }
        if (!groups.has(id)) throw new Error(`No group with id: ${id}`);
        setGroup([].concat(tabIds), id);
        return id;
      },
      async ungroup(ids) { setGroup([].concat(ids), -1); },
      async hide(ids) {
        if (!granted.has('tabHide')) throw new Error('Missing tabHide permission');
        for (const id of [].concat(ids)) tabs.get(id).hidden = true;
        return ids;
      },
      async show(ids) { for (const id of [].concat(ids)) tabs.get(id).hidden = false; },
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
      // Messages to content scripts. In the preview the inspector content script runs in
      // the preview page itself and listens on runtime.onMessage.
      async sendMessage(tabId, msg) {
        if (msg && msg.smInspect) {
          return new Promise((resolve) => {
            let answered = false;
            ev.message.fire(msg, { tab: { id: tabId } }, (r) => { answered = true; resolve(r); });
            if (!answered) resolve(undefined);
          });
        }
        tabMessages.push({ tabId, msg });
      },
      onCreated: ev.created, onUpdated: ev.updated, onRemoved: ev.removed, onActivated: ev.activated,
    },
    windows: {
      async create(p) { createdWindows.push(clone(p || {})); return { id: 2, tabs: [] }; },
      async update(id, p) { if (p && 'titlePreface' in p) titlePrefaces[id] = p.titlePreface; },
      async getCurrent() { return { id: 1 }; },
    },
    tabGroups: {
      async query() { return [...groups.values()].map(clone); },
      async get(id) { return clone(groups.get(id)); },
      async update(id, p) { Object.assign(groups.get(id), p); ev.groupUpdated.fire(clone(groups.get(id))); return clone(groups.get(id)); },
      onUpdated: ev.groupUpdated,
    },
    scripting: {
      async getRegisteredContentScripts() { return clone(contentScripts); },
      async registerContentScripts(list) { contentScripts.push(...clone(list)); },
      async unregisterContentScripts({ ids }) {
        for (const id of ids) contentScripts.splice(contentScripts.findIndex((s) => s.id === id), 1);
      },
      async executeScript({ files, func }) {
        // Preview: run in this page (as the inspected tab's top frame).
        if (files) for (const f of files) await import(new URL(`../web/${f}`, import.meta.url).href);
        return func ? [{ frameId: 0, result: func() }] : [{ frameId: 0, result: undefined }];
      },
    },
    webRequest: {
      onBeforeRequest: {
        addListener: (f, filter, extra) => requestListeners.push({ f, filter, extra }),
        removeListener: (f) => requestListeners.splice(requestListeners.findIndex((l) => l.f === f), 1),
        hasListener: (f) => requestListeners.some((l) => l.f === f),
      },
      onHeadersReceived: {
        addListener: (f, filter, extra) => headerListeners.push({ f, filter, extra }),
        removeListener: (f) => headerListeners.splice(headerListeners.findIndex((l) => l.f === f), 1),
        hasListener: (f) => headerListeners.some((l) => l.f === f),
      },
      filterResponseData(requestId) {
        const filter = { written: [], closed: false, write(d) { this.written.push(d); }, close() { this.closed = true; } };
        filters.set(requestId, filter);
        return filter;
      },
    },
    declarativeNetRequest: {
      async getDynamicRules() { return clone(dnrRules); },
      async updateDynamicRules({ removeRuleIds, addRules }) {
        dnrRules = dnrRules.filter((r) => !removeRuleIds.includes(r.id)).concat(clone(addRules));
      },
      async getSessionRules() { return clone(dnrSessionRules); },
      async updateSessionRules({ removeRuleIds = [], addRules = [] }) {
        dnrSessionRules = dnrSessionRules.filter((r) => !removeRuleIds.includes(r.id)).concat(clone(addRules));
      },
    },
    permissions: {
      async contains({ permissions }) { return permissions.every((p) => granted.has(p)); },
      async request({ permissions }) { permissions.forEach((p) => granted.add(p)); ev.permAdded.fire({ permissions }); return true; },
      onAdded: ev.permAdded,
      onRemoved: ev.permRemoved,
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
      id: 'session-manager@local.extension',
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
  return {
    api, tabs, cookies, storage, alarms, removedSiteData, reloaded, tabMessages, groups, granted, titlePrefaces, contentScripts,
    dnr: () => dnrRules, dnrSession: () => dnrSessionRules, headerListeners, createdWindows, requestListeners, filters, ev, clone,
  };
}
