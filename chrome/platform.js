// Chrome / Chromium implementation of `smPlatform` (overrides on top of platform-common.js).
// Chromium cannot create tabs in the discarded state, so the shared createTab
// (create, then tabs.discard) is used.
import { installPlatform, readThrottleRules, throttleRuleFor } from './platform-common.js';

const api = globalThis.chrome;

// Throttling: Chrome lets extensions hold requests only through the debugger (a permission
// Chrome doesn't allow to be optional). Tabs on a site with rules get the
// debugger attached (Chrome shows its "debugging this browser" bar meanwhile); every
// request pauses, and those matching a rule continue after the rule's delay. Attachments
// survive reloads, so throttling does too.
let throttleRules = [];
const tabUrls = new Map(); // attached tab -> its page URL
let throttleReady = null;

const wantsThrottle = (url, tabId) => throttleRules.some((r) => r.enabled && throttleRuleFor([{ ...r, pattern: '*', method: '' }], url, url, 'GET', tabId));

async function attachThrottle(tabId, url) {
  tabUrls.set(tabId, url);
  try {
    await api.debugger.attach({ tabId }, '1.3');
  } catch (e) {
    if (!/already attached/i.test(String(e && e.message))) {
      tabUrls.delete(tabId);
      throw e;
    }
  }
  await api.debugger.sendCommand({ tabId }, 'Fetch.enable', { patterns: [{ urlPattern: '*', requestStage: 'Request' }] });
}

async function syncThrottle() {
  if (!api.debugger) return;
  for (const tab of await api.tabs.query({})) {
    const url = tab.url || tab.pendingUrl || '';
    const want = /^https?:|^file:/.test(url) && wantsThrottle(url, tab.id);
    if (want && !tabUrls.has(tab.id)) await attachThrottle(tab.id, url).catch((e) => console.warn('[session-manager] throttle', e));
    else if (!want && tabUrls.has(tab.id)) {
      tabUrls.delete(tab.id);
      await api.debugger.detach({ tabId: tab.id }).catch(() => {});
    } else if (want) tabUrls.set(tab.id, url);
  }
}

async function loadThrottle() {
  throttleRules = await readThrottleRules(api);
  await syncThrottle();
}

installPlatform(api, {
  installThrottle() {
    if (!api.debugger) return;
    throttleReady = loadThrottle().catch((e) => console.warn('[session-manager] throttle', e));
    api.storage.onChanged.addListener((changes, area) => {
      if (changes.smThrottle || changes.smThrottleTabs) throttleReady = loadThrottle().catch((e) => console.warn('[session-manager] throttle', e));
    });
    api.tabs.onUpdated.addListener((tabId, change, tab) => {
      if (change.url || change.status === 'loading') {
        if (tabUrls.has(tabId)) tabUrls.set(tabId, tab.url || tabUrls.get(tabId));
        syncThrottle().catch(() => {});
      }
    });
    api.tabs.onRemoved.addListener((tabId) => tabUrls.delete(tabId));
    // The user closed the debugging bar (or the tab went away): stop for that tab.
    api.debugger.onDetach.addListener((source) => tabUrls.delete(source.tabId));
    api.debugger.onEvent.addListener(async (source, method, params) => {
      if (method !== 'Fetch.requestPaused') return;
      await throttleReady;
      const go = () => api.debugger.sendCommand(source, 'Fetch.continueRequest', { requestId: params.requestId }).catch(() => {});
      const page = params.resourceType === 'Document' && !params.frameId ? params.request.url : tabUrls.get(source.tabId) || params.request.url;
      const rule = throttleRuleFor(throttleRules, page, params.request.url, params.request.method, source.tabId);
      if (rule && rule.delay > 0) setTimeout(go, rule.delay);
      else go();
    });
  },

  info() {
    return {
      browser: 'chrome',
      tabGroups: !!(api.tabs?.group && api.tabGroups),
      perHostCache: true,
      sidebar: !!api.sidePanel,
      extensionId: api.runtime.id || '',
    };
  },

  // Chromium resolves to the discarded tab (possibly with a new id), or to nothing when
  // the tab could not be discarded.
  async discardTabs(ids) {
    const discarded = [];
    const errors = [];
    for (const id of ids) {
      try {
        const t = await api.tabs.discard(id);
        if (t) discarded.push(t.id);
        else errors.push(`tab ${id} could not be hibernated`);
      } catch (e) {
        errors.push(String(e && e.message ? e.message : e));
      }
    }
    return { discarded, errors };
  },

  async removeSiteData(hosts, types) {
    const dataTypes = {
      cache: !!types.cache,
      cacheStorage: !!types.cache,
      localStorage: !!types.localStorage,
      indexedDB: !!types.indexedDb,
      serviceWorkers: !!types.serviceWorkers,
    };
    if (hosts === null) {
      await api.browsingData.remove({}, dataTypes);
      return [];
    }
    if (!hosts.length) return [];
    const origins = hosts.flatMap((h) => [`https://${h}`, `http://${h}`]);
    await api.browsingData.remove({ origins }, dataTypes);
    return [];
  },
});
