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
    };
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
