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
    audible: !!t.audible,
    lastAccessed: t.lastAccessed || 0,
    groupTitle: null,
    groupId: t.groupId != null && t.groupId >= 0 ? t.groupId : null,
    hidden: !!t.hidden,
    openerTabId: t.openerTabId ?? null,
  };
}

/**
 * Blanks out `debugger` statements in script source bytes (UTF-8 or any ASCII compatible
 * encoding), keeping the length. `a.debugger` and `{ debugger: 1 }` are left alone.
 * Returns the same array when there is nothing to change.
 */
export function stripDebuggerBytes(bytes) {
  const WORD = [100, 101, 98, 117, 103, 103, 101, 114]; // "debugger"
  const ident = (b) => (b >= 48 && b <= 57) || (b >= 65 && b <= 90) || (b >= 97 && b <= 122) || b === 95 || b === 36 || b >= 128;
  const space = (b) => b === 32 || b === 9 || b === 10 || b === 13;
  let out = null;
  for (let i = bytes.indexOf(100); i !== -1 && i + 8 <= bytes.length; i = bytes.indexOf(100, i + 1)) {
    let k = 0;
    while (k < 8 && bytes[i + k] === WORD[k]) k++;
    if (k < 8 || (i > 0 && ident(bytes[i - 1])) || (i + 8 < bytes.length && ident(bytes[i + 8]))) continue;
    let p = i - 1;
    while (p >= 0 && space(bytes[p])) p--;
    let n = i + 8;
    while (n < bytes.length && space(bytes[n])) n++;
    if ((p >= 0 && bytes[p] === 46) || (n < bytes.length && bytes[n] === 58)) continue; // "." or ":"
    out ??= bytes.slice();
    out.fill(32, i, i + 8);
    i += 7;
  }
  return out ?? bytes;
}

/**
 * Runs in every frame of an inspected page (scripting.executeScript): the URLs of its
 * videos, audio, frames, embeds and scripts, and of media requests in its network log.
 */
function collectPageResources() {
  const items = [];
  const add = (kind, url, detail = '') => {
    if (url) items.push({ kind, url: String(url), detail: String(detail || '') });
  };
  for (const m of document.querySelectorAll('video, audio')) {
    add(m.localName, m.currentSrc || m.src, [m.duration > 0 ? `${Math.round(m.duration)} s` : '', m.paused ? 'paused' : 'playing'].filter(Boolean).join(' · '));
    for (const s of m.querySelectorAll('source')) add('source', s.src, s.type);
    for (const t of m.querySelectorAll('track')) add('track', t.src, [t.kind, t.srclang].filter(Boolean).join(' '));
    if (m.poster) add('poster', m.poster);
  }
  for (const f of document.querySelectorAll('iframe, frame')) add(f.localName, f.src || 'about:blank', f.title || f.name);
  for (const e of document.querySelectorAll('embed')) add('embed', e.src, e.type);
  for (const o of document.querySelectorAll('object')) add('object', o.data, o.type);
  for (const s of document.querySelectorAll('script[src]')) add('script', s.src, [s.async && 'async', s.defer && 'defer'].filter(Boolean).join(' '));
  // Streams behind blob: videos show up as requests (HLS / DASH manifests and segments).
  const MEDIA = /\.(m3u8|mpd|mp4|m4v|m4s|m4a|webm|mkv|mov|ts|mp3|aac|ogg|oga|ogv|flac|wav|flv)(\?|#|$)/i;
  for (const r of performance.getEntriesByType('resource')) {
    if (MEDIA.test(r.name) || r.initiatorType === 'video' || r.initiatorType === 'audio') add('network', r.name, r.initiatorType);
  }
  return { frameUrl: location.href, top: window === window.top, items };
}

/** Like tabOut, with the title of the native tab group the tab is in. */
export async function tabOutWithGroup(api, t, titles = new Map()) {
  const out = tabOut(t);
  if (api.tabGroups && t.groupId != null && t.groupId >= 0) {
    if (!titles.has(t.groupId)) {
      titles.set(t.groupId, api.tabGroups.get(t.groupId).then((g) => g.title || null, () => null));
    }
    out.groupTitle = await titles.get(t.groupId);
  }
  return out;
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
  // UI pages: the action to run on the next pointer release (see setGestureAction), and
  // this page's window (for opening the side panel without waiting).
  let gesture = '';
  let pageWindowId = null;

  function emit(ev) {
    if (handler) return Promise.resolve(handler(ev));
    return new Promise((resolve, reject) => queue.push({ ev, resolve, reject }));
  }

  function fire(ev) {
    emit(ev).catch((e) => console.error('[session-manager] event failed', ev.type, e));
  }

  // Tab events are fired in the order they happened, even when one has to wait for its
  // tab group title.
  let tabEvents = Promise.resolve();
  function fireInOrder(make) {
    tabEvents = tabEvents.then(make).then(
      (ev) => ev && fire(ev),
      (e) => console.error('[session-manager] could not read tab event', e),
    );
  }

  const platform = {
    info() {
      return {
        browser: 'unknown',
        tabGroups: !!(api.tabs.group && api.tabGroups),
        perHostCache: false,
        sidebar: !!(api.sidePanel || api.sidebarAction),
        extensionId: api.runtime.id || '',
      };
    },

    // ---- tabs ----
    async queryTabs() {
      const titles = new Map();
      return Promise.all((await api.tabs.query({})).map((t) => tabOutWithGroup(api, t, titles)));
    },

    /** A tab by id, or (id == null) the active tab of the last focused window. */
    async getTab(id) {
      if (id != null) return tabOut(await api.tabs.get(id));
      const [tab] = await api.tabs.query({ active: true, lastFocusedWindow: true });
      if (!tab) throw new Error('no active tab');
      return tabOut(tab);
    },

    async reloadTab(id) {
      await api.tabs.reload(id, { bypassCache: true });
    },

    async createTab(o) {
      const props = { active: !!o.active, pinned: !!o.pinned };
      if (o.url) props.url = o.url; // none: the new tab page
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

    async activateTab(id) {
      await api.tabs.update(id, { active: true });
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

    // ---- native tab groups (workspaces in the tab strip) ----
    async queryTabGroups() {
      if (!api.tabGroups) return [];
      return (await api.tabGroups.query({})).map((g) => ({
        id: g.id,
        windowId: g.windowId,
        title: g.title || '',
        color: g.color || '',
        collapsed: !!g.collapsed,
      }));
    },

    /** Adds tabs to a group (groupId == null: a new group in windowId). Resolves to the group id. */
    async groupInto(ids, groupId, windowId) {
      const opts = groupId != null ? { tabIds: ids, groupId } : { tabIds: ids, createProperties: { windowId } };
      return await api.tabs.group(opts);
    },

    /** props: { title, color, collapsed? } */
    async updateTabGroup(id, props) {
      await api.tabGroups.update(id, props);
    },

    // ---- hiding tabs (Firefox, with the optional tabHide permission) ----
    async hideTabs(ids) {
      if (api.tabs.hide && ids.length) await api.tabs.hide(ids);
    },

    async showTabs(ids) {
      if (api.tabs.show && ids.length) await api.tabs.show(ids);
    },

    /** Text before the window title (Firefox only). */
    async setTitlePreface(_windowId, _text) {},

    // ---- page guards ----

    /** Replaces our page scripts (ids starting with "sm-guard-") with `regs`. */
    async syncContentScripts(regs) {
      if (!api.scripting || !api.scripting.registerContentScripts) throw new Error('this browser cannot register page scripts');
      const existing = await api.scripting.getRegisteredContentScripts();
      const ours = existing.map((s) => s.id).filter((id) => id.startsWith('sm-guard-'));
      if (ours.length) await api.scripting.unregisterContentScripts({ ids: ours });
      const clean = regs.map((r) => {
        const out = { ...r };
        if (!out.excludeMatches.length) delete out.excludeMatches;
        return out;
      });
      if (clean.length) await api.scripting.registerContentScripts(clean);
    },

    /** Replaces our declarativeNetRequest dynamic rules (script blocking). */
    async setBlockRules(rules) {
      const dnr = api.declarativeNetRequest;
      if (!dnr) throw new Error('this browser cannot block requests');
      const old = await dnr.getDynamicRules();
      await dnr.updateDynamicRules({ removeRuleIds: old.map((r) => r.id), addRules: rules });
    },

    /** Firefox: blank out `debugger` in script files as they load. Elsewhere: unsupported. */
    async setScriptRewrite(_enabled, _exceptions) {},

    // ---- inspector ----

    async openInspector(tabId) {
      const url = api.runtime.getURL(`inspector.html?tab=${tabId}`);
      await api.windows.create({ url, type: 'popup', width: 1100, height: 780 });
    },

    /** Injects the inspector content script into the tab's top frame. */
    async inspectorInject(tabId) {
      await api.scripting.executeScript({ target: { tabId, frameIds: [0] }, files: ['inspect.js'] });
    },

    /** A request to the inspector content script ({ op, ...args }); resolves to its result. */
    async inspect(tabId, request) {
      const r = await api.tabs.sendMessage(tabId, { smInspect: request }, { frameId: 0 });
      if (!r) throw new Error('the page did not answer (reload it and try again)');
      if (!r.ok) throw new Error(r.error);
      return r.result;
    },

    /** Videos, frames, scripts... of every frame of a tab. */
    async collectResources(tabId) {
      const results = await api.scripting.executeScript({ target: { tabId, allFrames: true }, func: collectPageResources });
      return results
        .filter((r) => r && r.result)
        .flatMap((r) => r.result.items.map((i) => ({ ...i, frame: r.result.frameUrl, top: !!r.result.top })));
    },

    /** UI pages: `cb(json)` when the inspector content script reports a picked element. */
    onInspectorPick(cb) {
      api.runtime.onMessage.addListener((msg) => {
        if (msg && msg.smInspectPicked) cb(JSON.stringify(msg.smInspectPicked));
        return undefined;
      });
    },

    /** A query parameter of this page's URL ('' if missing). */
    locationParam(name) {
      return (globalThis.location && new URLSearchParams(location.search).get(name)) || '';
    },

    async hasPermission(name) {
      try {
        return await api.permissions.contains({ permissions: [name] });
      } catch {
        return false;
      }
    },

    async currentWindowId() {
      return (await api.windows.getCurrent()).id;
    },

    /** Opens a page in a new tab (fails for pages extensions may not open, like about:config). */
    async openUrl(url) {
      await api.tabs.create({ url, active: true });
    },

    // ---- user gestures ----
    // Some APIs only work while handling a user's click (Firefox: sidebarAction.open,
    // permissions.request). egui handles clicks a frame later, so the UI arms an action
    // while the pointer is over its button, and it runs when the pointer is released.

    /** Arms `name` ('sidebar', 'tabHide') for the next pointer release; '' disarms. */
    setGestureAction(name) {
      gesture = name || '';
    },

    runGesture(name) {
      try {
        let p;
        if (name === 'sidebar') p = platform.openSidebar();
        else if (name === 'tabHide') p = api.permissions.request({ permissions: ['tabHide'] });
        if (p && p.catch) p.catch((e) => console.warn('[session-manager]', name, e));
      } catch (e) {
        console.warn('[session-manager]', name, e);
      }
    },

    /** Opens the sidebar / side panel. Must run inside a user gesture. */
    openSidebar() {
      if (api.sidebarAction) return api.sidebarAction.open();
      if (api.sidePanel && pageWindowId != null) return api.sidePanel.open({ windowId: pageWindowId });
      return undefined;
    },

    /** Unloads tabs. Resolves to { discarded: ids, errors: messages }. */
    async discardTabs(ids) {
      const discarded = [];
      const errors = [];
      for (const id of ids) {
        try {
          // Chromium resolves to the discarded tab (whose id may change); Firefox to nothing.
          const t = await api.tabs.discard(id);
          discarded.push(t && t.id != null ? t.id : id);
        } catch (e) {
          errors.push(String(e && e.message ? e.message : e));
        }
      }
      return { discarded, errors };
    },

    /** Asks the media content script in every frame of a tab to "pause" or "resume". */
    async mediaCommand(tabId, cmd, audio) {
      try {
        await api.tabs.sendMessage(tabId, { smMediaCmd: cmd, audio: !!audio });
      } catch {
        // No content script in this tab (internal page, or opened before the install).
      }
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
      const TRACKED = ['url', 'title', 'favIconUrl', 'pinned', 'discarded', 'groupId'];
      const withGroup = (tab) => tabOutWithGroup(api, tab);
      api.runtime.onStartup?.addListener(() => fire({ type: 'browserStartup' }));
      api.tabs.onCreated.addListener((tab) => fireInOrder(async () => ({ type: 'tabCreated', tab: await withGroup(tab) })));
      api.tabs.onUpdated.addListener((_id, change, tab) => {
        if (!TRACKED.some((k) => k in change)) return;
        fireInOrder(async () => ({ type: 'tabUpdated', tab: await withGroup(tab), urlChanged: 'url' in change }));
      });
      api.tabs.onRemoved.addListener((tabId, info) =>
        fireInOrder(() => ({ type: 'tabRemoved', tabId, windowClosing: !!(info && info.isWindowClosing) })),
      );
      api.tabs.onActivated.addListener((info) => fireInOrder(() => ({ type: 'tabActivated', tabId: info.tabId })));
      // tabHide granted or revoked.
      api.permissions?.onAdded?.addListener(() => fire({ type: 'permissionsChanged' }));
      api.permissions?.onRemoved?.addListener(() => fire({ type: 'permissionsChanged' }));
      // Chrome: the keyboard shortcut for the side panel (a command counts as a user gesture).
      api.commands?.onCommand?.addListener((command, tab) => {
        if (command === 'open-sidebar' && api.sidePanel && tab) {
          api.sidePanel.open({ windowId: tab.windowId }).catch((e) => console.warn('[session-manager] side panel', e));
        } else if (command === 'open-inspector') {
          fire({ type: 'command', name: command, tabId: tab ? tab.id : null });
        }
      });
      if (api.tabs.onReplaced) {
        api.tabs.onReplaced.addListener((added, removed) => fireInOrder(() => ({ type: 'tabReplaced', added, removed })));
      }
      // A renamed native tab group: its tabs' group title changed.
      api.tabGroups?.onUpdated?.addListener((group) => {
        fireInOrder(async () => {
          const titles = new Map([[group.id, Promise.resolve(group.title || null)]]);
          for (const tab of await api.tabs.query({ groupId: group.id })) {
            fire({ type: 'tabUpdated', tab: await tabOutWithGroup(api, tab, titles), urlChanged: false });
          }
        });
      });
      api.cookies.onChanged.addListener((info) => {
        if (!info.removed) fire({ type: 'cookieSet', cookie: cookieOut(info.cookie) });
      });
      api.alarms.onAlarm.addListener((alarm) => fire({ type: 'alarm', name: alarm.name }));
      api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
        // Playback reported by the media content script (web/media.js).
        if (msg && msg.smMedia && sender && sender.tab) {
          const m = msg.smMedia;
          fireInOrder(() => ({
            type: 'mediaState',
            tabId: sender.tab.id,
            frameId: sender.frameId ?? 0,
            playing: !!m.playing,
            video: !!m.video,
            pip: !!m.pip,
          }));
          return false;
        }
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
  if (globalThis.document && globalThis.addEventListener) {
    addEventListener(
      'pointerup',
      () => {
        if (gesture) platform.runGesture(gesture);
      },
      true,
    );
    api.windows?.getCurrent?.().then(
      (w) => (pageWindowId = w.id),
      () => {},
    );
  }
  return platform;
}
