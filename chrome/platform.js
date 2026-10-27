// Chrome / Chromium implementation of `smPlatform` (overrides on top of platform-common.js).
// Chromium cannot create tabs in the discarded state, so the shared createTab
// (create, then tabs.discard) is used.
import { installPlatform } from './platform-common.js';

const api = globalThis.chrome;

installPlatform(api, {
  info() {
    return {
      browser: 'chrome',
      tabGroups: !!(api.tabs.group && api.tabGroups),
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
