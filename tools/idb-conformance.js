// IndexedDB behavior the extension relies on, as a list of observations. The same function
// runs against a real browser's IndexedDB and against tools/fake-indexeddb.js; the two lists
// must be equal (tools/idb-real-test.mjs compares them).
//
// `idb`: an IndexedDB factory. `createDb`: web/platform-common.js's createDb (the
// extension's own database code), run against a scratch database.
// Self-contained (no imports): its source is also evaluated inside browser pages.
export async function idbConformance(idb, createDb) {
  const log = [];
  const note = (name, value) => log.push([name, value === undefined ? '<undefined>' : JSON.parse(JSON.stringify(value))]);
  const req = (r) =>
    new Promise((ok, fail) => {
      r.onsuccess = () => ok(r.result);
      r.onerror = () => fail(r.error);
    });
  const done = (tx) =>
    new Promise((ok, fail) => {
      tx.oncomplete = () => ok('complete');
      tx.onabort = () => fail(tx.error ? tx.error.name : 'abort');
      tx.onerror = (e) => e && e.preventDefault && 0;
    });
  const sync = (f) => {
    try {
      f();
      return 'no error';
    } catch (e) {
      return e.name;
    }
  };
  const later = () => new Promise((r) => setTimeout(r, 0));
  const NAME = 'sm-conformance';
  const API = 'sm-conformance-api';
  await req(idb.deleteDatabase(NAME));
  await req(idb.deleteDatabase(API));

  // ---- opening and upgrading
  const events = [];
  const db = await new Promise((ok, fail) => {
    const r = idb.open(NAME, 1);
    r.onupgradeneeded = (e) => {
      events.push(['upgrade', e.oldVersion, e.newVersion]);
      const d = r.result;
      d.createObjectStore('kv');
      d.createObjectStore('tabs').createIndex('session', 's');
      events.push(['stores in upgrade', [...d.objectStoreNames]]);
      events.push(['create outside a store name twice', sync(() => d.createObjectStore('kv'))]);
    };
    r.onsuccess = () => {
      events.push(['success']);
      ok(r.result);
    };
    r.onerror = () => fail(r.error);
  });
  note('open: events', events);
  note('open: version and stores', [db.version, [...db.objectStoreNames], db.objectStoreNames.contains('tabs'), db.objectStoreNames.contains('nope')]);
  note('createObjectStore outside an upgrade', sync(() => db.createObjectStore('x')));

  const again = await new Promise((ok, fail) => {
    const r = idb.open(NAME, 1);
    let upgraded = false;
    r.onupgradeneeded = () => (upgraded = true);
    r.onsuccess = () => {
      r.result.close();
      ok(upgraded);
    };
    r.onerror = () => fail(r.error);
  });
  note('reopen at the same version upgrades nothing', again);
  const VERSIONS = 'sm-conformance-versions';
  await req(idb.deleteDatabase(VERSIONS));
  (await req(idb.open(VERSIONS, 2))).close();
  note(
    'open at a lower version',
    await new Promise((ok) => {
      const r = idb.open(VERSIONS, 1);
      r.onsuccess = () => {
        r.result.close();
        ok('opened');
      };
      r.onerror = () => ok(r.error.name);
    }),
  );
  await req(idb.deleteDatabase(VERSIONS));

  // ---- synchronous errors
  note('transaction on a missing store', sync(() => db.transaction('nope')));
  note('objectStore outside the scope', sync(() => db.transaction('kv').objectStore('tabs')));
  note('put in a readonly transaction', sync(() => db.transaction('kv').objectStore('kv').put(1, 1)));
  note('put without a key (out-of-line keys)', sync(() => db.transaction('kv', 'readwrite').objectStore('kv').put({ a: 1 })));
  note('put with an object as key', sync(() => db.transaction('kv', 'readwrite').objectStore('kv').put(1, { k: 1 })));
  note('missing index', sync(() => db.transaction('tabs').objectStore('tabs').index('nope')));

  // ---- writes, reads, ordering, cloning
  {
    const tx = db.transaction(['kv', 'tabs'], 'readwrite');
    const order = [];
    const kv = tx.objectStore('kv');
    const value = { list: [1, 2], nested: { s: 'x' } };
    const r1 = kv.put(value, 3);
    value.list.push(99); // after put: not saved
    r1.onsuccess = () => order.push(['put 3 ->', r1.result]);
    kv.put('one', 1).onsuccess = () => order.push('put 1');
    kv.put('str-b', 'b').onsuccess = () => order.push('put b');
    kv.put('str-a', 'a').onsuccess = () => order.push('put a');
    kv.put('ten', 10).onsuccess = () => order.push('put 10');
    const g = kv.get(3);
    g.onsuccess = () => order.push(['read own write', g.result]);
    const t = tx.objectStore('tabs');
    t.put({ s: 2, j: 'c' }, 30);
    t.put({ s: 1, j: 'b' }, 20);
    t.put({ s: 1, j: 'a' }, 25);
    t.put({ s: 2, j: 'd' }, 5);
    t.put({ j: 'no session' }, 6);
    t.put({ s: 'x', j: 'string session' }, 7);
    order.push(['sync result before success', r1.readyState]);
    tx.oncomplete = () => order.push('complete');
    await new Promise((ok) => {
      const prev = tx.oncomplete;
      tx.oncomplete = () => {
        prev();
        ok();
      };
    });
    note('write: order of events', order);
  }
  {
    const tx = db.transaction(['kv', 'tabs']);
    const kv = tx.objectStore('kv');
    const t = tx.objectStore('tabs');
    const [all, keys, count, missing, three, byS1, byS2, byX, tabKeys] = await Promise.all([
      req(kv.getAll()),
      req(kv.getAllKeys()),
      req(kv.count()),
      req(kv.get(404)),
      req(kv.get(3)),
      req(t.index('session').getAll(1)),
      req(t.index('session').getAll(2)),
      req(t.index('session').getAll('x')),
      req(t.getAllKeys()),
    ]);
    note('read: getAll in key order (numbers before strings)', all);
    note('read: getAllKeys', keys);
    note('read: count', count);
    note('read: missing key', missing);
    note('read: value cloned at put', three);
    note('read: index getAll(1), by primary key', byS1);
    note('read: index getAll(2)', byS2);
    note('read: index with a string key', byX);
    note('read: records without the index property are still stored', tabKeys);
    note('read: index getAll() lists only indexed records', (await req(db.transaction('tabs').objectStore('tabs').index('session').getAll())).map((v) => v.j));
  }

  // ---- transaction lifetime
  {
    const tx = db.transaction('kv', 'readwrite');
    const settled = done(tx).catch((e) => `aborted: ${e}`);
    const kv = tx.objectStore('kv');
    await req(kv.get(1));
    // Still active right after awaiting its own request (the microtasks after the callback).
    note('active after awaiting a request', sync(() => kv.put('chained', 2)));
    const r = await req(kv.get(2));
    note('chained request result', r);
    await later();
    note('inactive after a timer', sync(() => kv.put('late', 3)));
    note('transaction result', await settled);
  }
  {
    const tx = db.transaction('kv', 'readwrite');
    tx.objectStore('kv').put('rolled back', 1);
    tx.objectStore('kv').delete(10);
    const settled = done(tx).then(
      () => 'complete',
      (e) => `abort: ${e}`,
    );
    tx.abort();
    note('abort: outcome', await settled);
    note('abort: data unchanged', await req(db.transaction('kv').objectStore('kv').getAll()));
    note('abort twice', sync(() => tx.abort()));
  }
  {
    // A request that fails aborts its transaction (and what it wrote before).
    const tx = db.transaction('kv', 'readwrite');
    tx.objectStore('kv').put('before failure', 1);
    const outcome = done(tx).then(
      () => 'complete',
      (e) => `abort: ${e}`,
    );
    const r = tx.objectStore('kv').add('dup', 3);
    r.onerror = () => {};
    note('failed request (add an existing key)', await outcome);
    note('failed request: data unchanged', await req(db.transaction('kv').objectStore('kv').get(1)));
  }
  {
    // Transactions see each other's work in order: a reader created after a writer sees it.
    const w = db.transaction('kv', 'readwrite');
    const written = done(w).catch(() => {});
    w.objectStore('kv').put('written first', 50);
    const r = db.transaction('kv').objectStore('kv').get(50);
    note('read after write in another transaction', await req(r));
    await written;
  }
  {
    const tx = db.transaction('kv', 'readwrite');
    const cleared = done(tx);
    const kv = tx.objectStore('kv');
    kv.clear();
    const c = await req(kv.count());
    note('clear then count', c);
    await cleared;
  }
  db.close();
  note('transaction on a closed database', sync(() => db.transaction('kv')));

  // ---- the extension's own database code (createDb)
  const api = createDb(API, () => idb);
  await api.open();
  note('api: load from an empty database', await api.load());
  note('api: readState on an empty database', await api.readState({}));
  const header = (revs) => JSON.stringify({ revision: 9, sessions: [{ id: 1, tabs_revision: revs[0] }, { id: 2, tabs_revision: revs[1] }] });
  await api.write(header([1, 1]), [
    { id: 12, s: 1, j: '{"id":12}' },
    { id: 3, s: 2, j: '{"id":3}' },
    { id: 11, s: 1, j: '{"id":11}' },
  ], []);
  note('api: load', await api.load());
  note('api: readState, nothing known', await api.readState({}));
  note('api: readState, session 1 known', await api.readState({ 1: 1 }));
  note('api: readState, all known', await api.readState({ 1: 1, 2: 1 }));
  await api.write(header([1, 2]), [{ id: 3, s: 1, j: '{"id":3,"moved":true}' }], [11, 999]);
  note('api: after moving, deleting and deleting a missing id', await api.load());
  note('api: readState after the change', await api.readState({ 1: 1, 2: 1 }));
  let failed = 'no error';
  try {
    await api.write(header([3, 3]), [{ id: { bad: 1 }, s: 1, j: '' }], []);
  } catch (e) {
    failed = e.name || 'error';
  }
  note('api: a bad record fails the write', failed);
  note('api: nothing of the failed write was saved', (await api.load()).header === header([1, 2]));
  note('api: vectors', [await api.getVectors(1), await (async () => {
    await api.putVectors(1, '{"v":1}');
    await api.putVectors(2, '{"v":2}');
    return api.getVectors(1);
  })(), await (async () => {
    await api.deleteVectors([1, 7]);
    return [await api.getVectors(1), await api.getVectors(2)];
  })()]);
  await api.clear();
  note('api: cleared', await api.load());
  note('api: vectors survive clear', await api.getVectors(2));

  // A second connection (another page) sees what the first wrote.
  const reader = createDb(API, () => idb);
  await api.write(header([4, 4]), [{ id: 1, s: 2, j: '{"id":1}' }], []);
  note('api: another connection reads it', await reader.readState({}));
  (await reader.open()).close();
  (await api.open()).close();
  await req(idb.deleteDatabase(NAME));
  await req(idb.deleteDatabase(API));
  note('deleted', 'ok');
  return log;
}
