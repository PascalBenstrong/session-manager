// Firefox implementation of `smPlatform` (overrides on top of platform-common.js).
import { installPlatform, tabOut } from './platform-common.js';

const api = globalThis.browser;
const LAZY_SCHEMES = /^(https?|ftp):/i;

installPlatform(api, {
  info() {
    return {
      browser: 'firefox',
      tabGroups: !!(api.tabs.group && api.tabGroups),
      // browsingData.remove({hostnames}) does not support the HTTP cache in Firefox.
      perHostCache: false,
      sidebar: !!api.sidebarAction,
      // tabs.hide needs the optional tabHide permission (asked for when it is turned on).
      hideTabs: true,
      titlePreface: true,
      extensionId: api.runtime.id || '',
    };
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
