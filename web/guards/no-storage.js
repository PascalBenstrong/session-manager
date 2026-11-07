// Page guard: this site can't save data. Its local and session storage and the cookies its
// scripts set live in this page's memory only (the page keeps working, and forgets on reload);
// IndexedDB, the Cache API and service workers are refused. Cookies sent by the server are
// removed by the extension as they are set. Every step is on its own and never throws.
(() => {
  const refusal = () => new DOMException('This site is not allowed to save data', 'SecurityError');
  const attempt = (step) => {
    try {
      step();
    } catch {}
  };

  // A Storage that lives in memory; `storage.key = v`, `storage.key` and `delete storage.key`
  // work as with the real thing.
  const memoryStorage = () => {
    const data = new Map();
    const api = {
      get length() {
        return data.size;
      },
      key: (i) => [...data.keys()][Number(i)] ?? null,
      getItem: (k) => (data.has(String(k)) ? data.get(String(k)) : null),
      setItem: (k, v) => void data.set(String(k), String(v)),
      removeItem: (k) => void data.delete(String(k)),
      clear: () => data.clear(),
    };
    return new Proxy(api, {
      get: (t, k) => (typeof k === 'symbol' || k in t ? Reflect.get(t, k, t) : t.getItem(k)),
      set: (t, k, v) => {
        if (typeof k !== 'symbol' && !(k in t)) t.setItem(k, v);
        return true;
      },
      deleteProperty: (t, k) => {
        if (typeof k !== 'symbol') t.removeItem(k);
        return true;
      },
      has: (t, k) => k in t || (typeof k !== 'symbol' && data.has(k)),
      ownKeys: (t) => [...Reflect.ownKeys(t), ...[...data.keys()].filter((k) => !(k in t))],
      getOwnPropertyDescriptor: (t, k) =>
        Reflect.getOwnPropertyDescriptor(t, k) ??
        (typeof k !== 'symbol' && data.has(k) ? { value: data.get(k), writable: true, enumerable: true, configurable: true } : undefined),
    });
  };
  for (const name of ['localStorage', 'sessionStorage']) {
    const store = memoryStorage();
    attempt(() => Object.defineProperty(window, name, { configurable: true, enumerable: true, get: () => store }));
  }
  // If those couldn't be replaced, at least nothing gets written.
  attempt(() => {
    if (window.localStorage instanceof Storage) Storage.prototype.setItem = function () {};
  });

  // Cookies set by script: kept for this page only.
  const jar = new Map();
  attempt(() =>
    Object.defineProperty(Document.prototype, 'cookie', {
      configurable: true,
      enumerable: true,
      get: () => [...jar].map(([k, v]) => (k ? `${k}=${v}` : v)).join('; '),
      set(text) {
        const [pair, ...attrs] = String(text).split(';');
        const eq = pair.indexOf('=');
        const name = (eq < 0 ? '' : pair.slice(0, eq)).trim();
        const value = (eq < 0 ? pair : pair.slice(eq + 1)).trim();
        const expired = attrs.some((a) => {
          const [k, v = ''] = a.split('=').map((x) => x.trim().toLowerCase());
          return (k === 'max-age' && Number(v) <= 0) || (k === 'expires' && Date.parse(v) <= Date.now());
        });
        if (expired) jar.delete(name);
        else jar.set(name, value);
      },
    }),
  );
  attempt(() => {
    if (!globalThis.CookieStore) return;
    CookieStore.prototype.get = () => Promise.resolve(null);
    CookieStore.prototype.getAll = () => Promise.resolve([]);
    CookieStore.prototype.set = () => Promise.resolve();
    CookieStore.prototype.delete = () => Promise.resolve();
  });

  // IndexedDB: opening fails the way it does when storage is off (an error event, not a throw).
  attempt(() => {
    IDBFactory.prototype.open = function () {
      const req = new EventTarget();
      for (const k of ['onsuccess', 'onerror', 'onupgradeneeded', 'onblocked']) req[k] = null;
      req.readyState = 'done';
      req.result = undefined;
      req.error = refusal();
      setTimeout(() => {
        const e = new Event('error', { cancelable: true });
        attempt(() => req.onerror && req.onerror.call(req, e));
        attempt(() => req.dispatchEvent(e));
      });
      return req;
    };
    IDBFactory.prototype.databases = () => Promise.resolve([]);
  });

  attempt(() => {
    if (!globalThis.CacheStorage) return;
    CacheStorage.prototype.open = () => Promise.reject(refusal());
    CacheStorage.prototype.keys = () => Promise.resolve([]);
    CacheStorage.prototype.has = () => Promise.resolve(false);
    CacheStorage.prototype.match = () => Promise.resolve(undefined);
  });
  attempt(() => {
    if (!globalThis.ServiceWorkerContainer) return;
    ServiceWorkerContainer.prototype.register = () => Promise.reject(refusal());
  });

  // What another page of the tab left in window.name doesn't carry over.
  attempt(() => {
    window.name = '';
  });
})();
