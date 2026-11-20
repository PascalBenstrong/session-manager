// In-memory IndexedDB for the Node tests (tools/smoke-test.mjs and friends): the subset the
// extension uses, with the same observable behavior as browsers. tools/idb-conformance.js
// checks that: run `node tools/idb-real-test.mjs` to compare it with Edge and Firefox.
//
// Covered: open / upgrade / versions / deleteDatabase, object stores with out-of-line keys,
// indexes on a property, get / getAll / getAllKeys / count / put / delete / clear,
// structured cloning, key order (numbers before strings), transactions that run one after
// another, stay active while their requests' callbacks (and the microtasks after them) run,
// commit on their own, roll back on abort, and the synchronous errors browsers throw
// (ReadOnlyError, NotFoundError, DataError, TransactionInactiveError, InvalidStateError).

const domError = (name, message = name) => {
  const e = new Error(message);
  e.name = name;
  return e;
};

const tick = (f) => setTimeout(f, 0);

/** IndexedDB key order: numbers < strings (other key types aren't used here). */
function validKey(k) {
  return (typeof k === 'number' && !Number.isNaN(k)) || typeof k === 'string';
}

function cmp(a, b) {
  const ta = typeof a === 'number' ? 0 : 1;
  const tb = typeof b === 'number' ? 0 : 1;
  if (ta !== tb) return ta - tb;
  return a < b ? -1 : a > b ? 1 : 0;
}

class FakeRequest {
  constructor(source, transaction) {
    this.source = source;
    this.transaction = transaction;
    this.result = undefined;
    this.error = null;
    this.readyState = 'pending';
    this.onsuccess = null;
    this.onerror = null;
  }
}

class StringList {
  constructor(names) {
    this.names = [...names].sort();
  }
  get length() {
    return this.names.length;
  }
  item(i) {
    return this.names[i] ?? null;
  }
  contains(n) {
    return this.names.includes(n);
  }
  [Symbol.iterator]() {
    return this.names[Symbol.iterator]();
  }
}

class FakeIndex {
  constructor(store, name) {
    this.objectStore = store;
    this.name = name;
  }
  get keyPath() {
    return this.objectStore._data().indexes.get(this.name);
  }
  getAll(key) {
    const tx = this.objectStore.transaction;
    tx._check();
    if (key !== undefined && !validKey(key)) throw domError('DataError');
    return tx._request(this, () => {
      const path = this.keyPath;
      const rows = [];
      for (const [pk, v] of this.objectStore._data().records) {
        const ik = v != null ? v[path] : undefined;
        if (!validKey(ik)) continue; // not indexed
        if (key === undefined || cmp(ik, key) === 0) rows.push([ik, pk, v]);
      }
      rows.sort((a, b) => cmp(a[0], b[0]) || cmp(a[1], b[1]));
      return rows.map((r) => structuredClone(r[2]));
    });
  }
}

class FakeObjectStore {
  constructor(transaction, name) {
    this.transaction = transaction;
    this.name = name;
  }
  _data() {
    return this.transaction.db._stores.get(this.name);
  }
  get indexNames() {
    return new StringList(this._data().indexes.keys());
  }
  _sorted() {
    return [...this._data().records.entries()].sort((a, b) => cmp(a[0], b[0]));
  }
  _write() {
    this.transaction._check();
    if (this.transaction.mode === 'readonly') throw domError('ReadOnlyError', 'The transaction is read-only.');
  }
  createIndex(name, keyPath) {
    if (this.transaction.mode !== 'versionchange') throw domError('InvalidStateError');
    const d = this._data();
    if (d.indexes.has(name)) throw domError('ConstraintError');
    d.indexes.set(name, keyPath);
    return new FakeIndex(this, name);
  }
  index(name) {
    if (!this._data().indexes.has(name)) throw domError('NotFoundError', `No index named ${name}.`);
    return new FakeIndex(this, name);
  }
  get(key) {
    this.transaction._check();
    if (!validKey(key)) throw domError('DataError');
    return this.transaction._request(this, () => {
      const v = this._data().records.get(key);
      return v === undefined ? undefined : structuredClone(v);
    });
  }
  getAll() {
    this.transaction._check();
    return this.transaction._request(this, () => this._sorted().map(([, v]) => structuredClone(v)));
  }
  getAllKeys() {
    this.transaction._check();
    return this.transaction._request(this, () => this._sorted().map(([k]) => k));
  }
  count() {
    this.transaction._check();
    return this.transaction._request(this, () => this._data().records.size);
  }
  put(value, key) {
    this._write();
    if (!validKey(key)) throw domError('DataError', 'No valid key.');
    const copy = structuredClone(value); // cloned now, as browsers do
    return this.transaction._request(this, () => {
      this._data().records.set(key, copy);
      return key;
    });
  }
  add(value, key) {
    this._write();
    if (!validKey(key)) throw domError('DataError', 'No valid key.');
    const copy = structuredClone(value);
    return this.transaction._request(this, () => {
      if (this._data().records.has(key)) throw domError('ConstraintError', 'Key already exists in the object store.');
      this._data().records.set(key, copy);
      return key;
    });
  }
  delete(key) {
    this._write();
    if (!validKey(key)) throw domError('DataError');
    return this.transaction._request(this, () => {
      this._data().records.delete(key);
      return undefined;
    });
  }
  clear() {
    this._write();
    return this.transaction._request(this, () => {
      this._data().records.clear();
      return undefined;
    });
  }
}

class FakeTransaction {
  constructor(db, scope, mode, factory) {
    this.db = db;
    this.scope = scope;
    this.mode = mode;
    this.factory = factory;
    this.error = null;
    this.oncomplete = null;
    this.onerror = null;
    this.onabort = null;
    this._queue = [];
    this._active = true;
    this._state = 'waiting'; // running, done
    this._backup = null;
    // Active during the task that created it.
    tick(() => {
      this._active = false;
    });
    factory._enqueue(this);
  }
  get objectStoreNames() {
    return new StringList(this.scope);
  }
  objectStore(name) {
    if (this._state === 'done') throw domError('InvalidStateError', 'The transaction has finished.');
    if (!this.scope.includes(name)) throw domError('NotFoundError', `${name} is not in this transaction.`);
    return new FakeObjectStore(this, name);
  }
  _check() {
    if (!this._active || this._state === 'done') throw domError('TransactionInactiveError', 'The transaction is not active.');
  }
  _request(source, op) {
    const r = new FakeRequest(source, this);
    this._queue.push([r, op]);
    return r;
  }
  _start() {
    this._state = 'running';
    if (this.mode !== 'readonly') {
      this._backup = new Map(this.scope.map((n) => {
        const d = this.db._stores.get(n);
        return [n, { records: new Map(d.records), indexes: new Map(d.indexes) }];
      }));
    }
    tick(() => this._step());
  }
  _step() {
    if (this._state === 'done') return;
    const next = this._queue.shift();
    if (!next) {
      this._finish(false);
      return;
    }
    const [r, op] = next;
    try {
      r.result = op();
      r.readyState = 'done';
    } catch (e) {
      r.readyState = 'done';
      r.error = e;
      this._active = true;
      const ev = { target: r, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
      r.onerror?.(ev);
      if (!ev.defaultPrevented) this.onerror?.(ev);
      // Unless a handler prevents it, a failed request aborts its transaction.
      if (ev.defaultPrevented) {
        tick(() => {
          this._active = false;
          this._step();
        });
      } else {
        this.abort(e);
      }
      return;
    }
    // Requests made in the callback, or in the microtasks it schedules, join this
    // transaction; once the task ends it's inactive until its next callback.
    this._active = true;
    try {
      r.onsuccess?.({ target: r });
    } catch (e) {
      // An exception in a callback aborts the transaction, as in browsers.
      tick(() => {
        this._active = false;
        this.abort(e);
      });
      return;
    }
    tick(() => {
      this._active = false;
      this._step();
    });
  }
  abort(error = null) {
    if (this._state === 'done') throw domError('InvalidStateError', 'The transaction has finished.');
    if (this._backup) for (const [n, d] of this._backup) this.db._stores.set(n, d);
    // An explicit abort() leaves `error` null, as in browsers.
    this.error = error;
    for (const [r] of this._queue) {
      r.readyState = 'done';
      r.error = domError('AbortError');
    }
    this._queue = [];
    this._finish(true);
  }
  _finish(aborted) {
    if (this._state === 'done') return;
    const wasRunning = this._state === 'running';
    this._state = 'done';
    this._active = false;
    tick(() => {
      if (aborted) this.onabort?.({ target: this });
      else this.oncomplete?.({ target: this });
      if (wasRunning || aborted) this.factory._next(this);
    });
  }
}

class FakeDatabase {
  constructor(name, entry, factory) {
    this.name = name;
    this._entry = entry;
    this._stores = entry.stores;
    this.factory = factory;
    this._closed = false;
    this.onversionchange = null;
    this.onclose = null;
    this._upgrade = null;
  }
  get version() {
    return this._entry.version;
  }
  get objectStoreNames() {
    return new StringList(this._stores.keys());
  }
  createObjectStore(name) {
    if (!this._upgrade) throw domError('InvalidStateError', 'Not in a version change.');
    if (this._stores.has(name)) throw domError('ConstraintError', `${name} exists.`);
    this._stores.set(name, { records: new Map(), indexes: new Map() });
    return new FakeObjectStore(this._upgrade, name);
  }
  deleteObjectStore(name) {
    if (!this._upgrade) throw domError('InvalidStateError');
    this._stores.delete(name);
  }
  transaction(names, mode = 'readonly') {
    if (this._closed) throw domError('InvalidStateError', 'The database is closed.');
    if (this._upgrade) throw domError('InvalidStateError', 'A version change is running.');
    const scope = [].concat(names);
    if (!scope.length) throw domError('InvalidAccessError');
    for (const n of scope) if (!this._stores.has(n)) throw domError('NotFoundError', `No object store named ${n}.`);
    if (mode !== 'readonly' && mode !== 'readwrite') throw new TypeError(`bad mode ${mode}`);
    return new FakeTransaction(this, [...new Set(scope)].sort(), mode, this.factory);
  }
  close() {
    this._closed = true;
    this.factory._open.delete(this);
  }
}

export function createFakeIndexedDB() {
  /** name -> { version, stores: Map(name -> { records, indexes }) } */
  const databases = new Map();
  const queue = [];
  let running = null;
  const factory = {
    _open: new Set(),
    _enqueue(tx) {
      queue.push(tx);
      if (!running) factory._next(null);
    },
    _next(done) {
      if (done && running !== done) return;
      running = null;
      while (queue.length) {
        const tx = queue.shift();
        if (tx._state === 'done') continue;
        running = tx;
        tx._start();
        return;
      }
    },
    open(name, version) {
      if (version !== undefined && (!Number.isInteger(version) || version < 1)) throw new TypeError('bad version');
      const req = new FakeRequest(null, null);
      req.onupgradeneeded = null;
      req.onblocked = null;
      tick(() => {
        let entry = databases.get(name);
        const old = entry ? entry.version : 0;
        const want = version ?? Math.max(old, 1);
        if (want < old) {
          req.error = domError('VersionError', `The requested version (${want}) is less than the existing version (${old}).`);
          req.readyState = 'done';
          req.onerror?.({ target: req });
          return;
        }
        if (!entry) {
          entry = { version: 0, stores: new Map() };
          databases.set(name, entry);
        }
        const db = new FakeDatabase(name, entry, factory);
        if (want > old) {
          // Other connections are told, and close (the extension's do).
          for (const other of [...factory._open]) if (other.name === name) other.onversionchange?.({ oldVersion: old, newVersion: want });
          if ([...factory._open].some((o) => o.name === name)) {
            req.onblocked?.({ oldVersion: old, newVersion: want });
          }
          const backup = new Map([...entry.stores].map(([n, d]) => [n, { records: new Map(d.records), indexes: new Map(d.indexes) }]));
          const upgrade = { mode: 'versionchange', db, _check() {}, _request() {}, objectStore: (n) => new FakeObjectStore(upgrade, n) };
          db._upgrade = upgrade;
          entry.version = want;
          req.result = db;
          req.transaction = upgrade;
          req.readyState = 'done';
          try {
            req.onupgradeneeded?.({ target: req, oldVersion: old, newVersion: want });
          } catch (e) {
            entry.version = old;
            entry.stores = backup;
            db._stores = backup;
            if (old === 0) databases.delete(name);
            db._upgrade = null;
            req.result = undefined;
            req.error = domError('AbortError');
            tick(() => req.onerror?.({ target: req }));
            return;
          }
          db._upgrade = null;
          req.transaction = null;
        }
        tick(() => {
          factory._open.add(db);
          req.result = db;
          req.readyState = 'done';
          req.onsuccess?.({ target: req });
        });
      });
      return req;
    },
    deleteDatabase(name) {
      const req = new FakeRequest(null, null);
      tick(() => {
        for (const other of [...factory._open]) if (other.name === name) other.onversionchange?.({ oldVersion: other.version, newVersion: null });
        const old = databases.get(name)?.version ?? 0;
        databases.delete(name);
        req.readyState = 'done';
        req.onsuccess?.({ target: req, oldVersion: old, newVersion: null });
      });
      return req;
    },
    cmp(a, b) {
      if (!validKey(a) || !validKey(b)) throw domError('DataError');
      return Math.sign(cmp(a, b));
    },
    /** For tests: the raw data (name -> { version, stores }). */
    _databases: databases,
  };
  return factory;
}
