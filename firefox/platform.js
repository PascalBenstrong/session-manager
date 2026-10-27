// Firefox implementation of `smPlatform` (overrides on top of platform-common.js).
import { installPlatform, readThrottleRules, stripDebuggerBytes, tabOut, throttleRuleFor } from './platform-common.js';

const api = globalThis.browser;
const LAZY_SCHEMES = /^(https?|ftp):/i;

// Script filter that blanks out `debugger` statements (developer tools traps) in script
// files as they load. Only Firefox lets extensions rewrite responses.
let rewriteExceptions = null; // null: off; else sites to leave alone
const hostOf = (u) => {
  try {
    return new URL(u).hostname.toLowerCase();
  } catch {
    return '';
  }
};
function filterScript(details) {
  if (!rewriteExceptions) return;
  const site = hostOf(details.documentUrl || details.originUrl || details.url);
  if (rewriteExceptions.some((d) => site === d || site.endsWith(`.${d}`))) return;
  const filter = api.webRequest.filterResponseData(details.requestId);
  const chunks = [];
  filter.ondata = (e) => chunks.push(new Uint8Array(e.data));
  filter.onstop = () => {
    const all = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
    let at = 0;
    for (const c of chunks) {
      all.set(c, at);
      at += c.length;
    }
    filter.write(stripDebuggerBytes(all));
    filter.close();
  };
  filter.onerror = () => {};
}

// Throttling: requests matching a rule of the page's site are held for the rule's delay
// (a blocking listener may answer later). Covers every request type, also after reloads.
let throttleRules = [];
let throttling = false;
function throttle(details) {
  const top = details.type === 'main_frame' ? details.url : details.frameAncestors?.at(-1)?.url || details.documentUrl || details.originUrl || '';
  const rule = throttleRuleFor(throttleRules, top, details.url, details.method, details.tabId);
  if (!rule || !(rule.delay > 0)) return undefined;
  return new Promise((resolve) => setTimeout(() => resolve({}), rule.delay));
}
async function loadThrottle() {
  throttleRules = (await readThrottleRules(api)).filter((r) => r.enabled);
  if (throttleRules.length && !throttling) api.webRequest.onBeforeRequest.addListener(throttle, { urls: ['<all_urls>'] }, ['blocking']);
  else if (!throttleRules.length && throttling) api.webRequest.onBeforeRequest.removeListener(throttle);
  throttling = throttleRules.length > 0;
}

installPlatform(api, {
  installThrottle() {
    loadThrottle().catch((e) => console.warn('[session-manager] throttle', e));
    api.storage.onChanged.addListener((changes, area) => {
      if (changes.smThrottle || changes.smThrottleTabs) loadThrottle().catch((e) => console.warn('[session-manager] throttle', e));
    });
  },

  info() {
    return {
      browser: 'firefox',
      tabGroups: !!(api.tabs?.group && api.tabGroups),
      // browsingData.remove({hostnames}) does not support the HTTP cache in Firefox.
      perHostCache: false,
      sidebar: !!api.sidebarAction,
      // tabs.hide needs the optional tabHide permission (asked for when it is turned on).
      hideTabs: true,
      titlePreface: true,
      extensionId: api.runtime.id || '',
    };
  },

  async setScriptRewrite(enabled, exceptions) {
    const listening = rewriteExceptions !== null;
    rewriteExceptions = enabled ? exceptions.map((d) => d.toLowerCase()) : null;
    if (enabled && !listening) {
      api.webRequest.onBeforeRequest.addListener(filterScript, { urls: ['<all_urls>'], types: ['script'] }, ['blocking']);
    } else if (!enabled && listening) {
      api.webRequest.onBeforeRequest.removeListener(filterScript);
    }
  },

  // Firefox renders any part of the page (not only what's on screen): no scrolling needed.
  async captureRegion(tab, m) {
    const p = m.page;
    const rect = { x: p.x, y: p.y, width: p.width, height: Math.min(p.height, 12000) };
    return (await fetch(await this.captureTabImage(tab.id, rect, m.dpr))).blob();
  },

  /** A PNG data: URL of a part of a tab's page (CSS pixels of the document). */
  async captureTabImage(tabId, rect, scale) {
    return api.tabs.captureTab(tabId, { rect, scale, format: 'png' });
  },

  async setTitlePreface(windowId, text) {
    await api.windows.update(windowId, { titlePreface: text });
  },

  // Firefox can create tabs directly in the discarded (unloaded) state.
  async createTab(o) {
    const props = { active: !!o.active, pinned: !!o.pinned };
    if (o.url) props.url = o.url; // none: the new tab page
    if (o.windowId != null) props.windowId = o.windowId;
    if (o.url && o.discarded && !o.active && !o.pinned && LAZY_SCHEMES.test(o.url)) {
      props.discarded = true;
      if (o.title) props.title = o.title;
    }
    return tabOut(await api.tabs.create(props));
  },

  async removeSiteData(hosts, types) {
    const warnings = [];
    const dataTypes = {
      localStorage: !!types.localStorage,
      indexedDB: !!types.indexedDb,
      serviceWorkers: !!types.serviceWorkers,
    };
    const anyPerHost = Object.values(dataTypes).some(Boolean);
    if (hosts === null) {
      await api.browsingData.remove({}, { ...dataTypes, cache: !!types.cache });
      return warnings;
    }
    if (!hosts.length) return warnings;
    if (anyPerHost) await api.browsingData.remove({ hostnames: hosts }, dataTypes);
    if (types.cache) {
      warnings.push('Firefox cannot clear the cache for individual sites; the cache was left untouched (use "All domains" to clear it)');
    }
    return warnings;
  },
});
