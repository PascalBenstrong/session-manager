// Checks the IndexedDB storage in real browsers:
// 1. tools/idb-conformance.js on a real extension page (real IndexedDB, the extension's own
//    createDb code) must give exactly what it gives on the Node fake (tools/fake-indexeddb.js,
//    which the smoke test uses), so the fake behaves like the browsers.
// 2. The extension itself, end to end: it migrates a 0.14 state from storage.local into
//    IndexedDB, saves only changed tabs, keeps the order of tabs, and pages read it back.
//
//   ./build.sh && node tools/idb-real-test.mjs edge      (or chrome, firefox)
//
// A throwaway profile is used; your own browser profile is never touched.
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { isDeepStrictEqual } from 'node:util';
import { fakeObservations } from './idb-node-check.mjs';

const which = process.argv[2] || 'edge';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const until = async (what, f, ms = 15000) => {
  let last;
  for (const end = Date.now() + ms; Date.now() < end; await sleep(250)) {
    try {
      const v = await f();
      if (v) return v;
    } catch (e) {
      last = e;
    }
  }
  throw new Error(`timed out waiting for ${what}${last ? ` (${last.message})` : ''}`);
};
let passed = 0;
const results = [];
const check = (name, ok, detail = '') => {
  results.push(`${ok ? 'ok  ' : 'FAIL'} - ${name}${detail ? ` (${detail})` : ''}`);
  console.log(results[results.length - 1]);
  if (ok) passed++;
};

/** The conformance function's source, to evaluate in a page. */
const conformanceSource = readFileSync(join(root, 'tools', 'idb-conformance.js'), 'utf8').replace(/^export /m, '');
const runConformance = `(async () => {
  ${conformanceSource}
  const { createDb } = await import(${'`'}\${location.origin}/platform-common.js${'`'});
  return JSON.stringify(await idbConformance(indexedDB, createDb));
})()`;

const profile = mkdtempSync(join(tmpdir(), 'sm-idb-'));
let browser;
try {
  if (which === 'firefox') await firefox();
  else await chromium();
} catch (e) {
  check('the run finished', false, e.stack || e.message);
} finally {
  if (browser?.pid) spawn('taskkill', ['/PID', String(browser.pid), '/T', '/F']);
  await sleep(1500);
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {}
}
console.log(`\n${passed}/${results.length} checks passed (${which})`);
process.exit(results.length === passed ? 0 : 1);

// ------------------------------------------------------------------ shared checks

/** Compares the browser's observations with the fake's, line by line. */
async function compareConformance(evalJson) {
  const real = await evalJson(runConformance);
  const fake = await fakeObservations();
  const names = new Set([...real.map((r) => r[0]), ...fake.map((f) => f[0])]);
  let same = 0;
  for (const name of names) {
    const r = real.find((x) => x[0] === name);
    const f = fake.find((x) => x[0] === name);
    if (r && f && isDeepStrictEqual(r[1], f[1])) same++;
    else check(`conformance: ${name}`, false, `browser ${JSON.stringify(r?.[1])} vs fake ${JSON.stringify(f?.[1])}`);
  }
  check(`the fake IndexedDB behaves like ${which}'s (${same}/${names.size} observations equal)`, same === names.size && real.length === fake.length);
}

/**
 * The extension end to end. `page` evaluates in an extension page; `reload` restarts the
 * extension and returns a fresh page evaluator.
 */
async function extensionChecks(page, reload, api, openPage) {
  const command = async (p, cmd) => {
    const r = JSON.parse(await p(`smPlatform.sendMessage(${JSON.stringify(JSON.stringify(cmd))})`));
    if (!r.ok) throw new Error(`${cmd.cmd}: ${r.error}`);
    return r;
  };
  const dbState = (p) =>
    p(`smPlatform.dbLoad().then((d) => JSON.stringify({ header: d.header && JSON.parse(d.header), tabs: d.tabs.map((t) => ({ id: t.id, s: t.s, o: t.o, url: JSON.parse(t.j).url, j: t.j })) }))`).then(JSON.parse);
  const local = (p, key) => p(`${api}.storage.local.get(${JSON.stringify(key)}).then((r) => JSON.stringify(r[${JSON.stringify(key)}] ?? null))`).then(JSON.parse);

  // ---- migration from 0.14: a state with its tabs in storage.local, saved as 0.14 did.
  const legacy = {
    version: 1,
    sessions: [
      { id: 1, name: 'Old work', created_at: 1, entries: [
        { id: 10, url: 'https://old.example/a', title: 'A', created_at: 1 },
        { id: 4, url: 'https://old.example/b', title: 'B (lower id, listed second)', created_at: 1 },
        { id: 12, url: 'https://old.example/c', title: 'C', created_at: 1, tab_id: 999999, window_id: 1 },
      ], snapshots: [] },
      { id: 2, name: 'Reading', created_at: 1, entries: [{ id: 20, url: 'https://read.example/', title: 'R', created_at: 1 }], snapshots: [] },
    ],
    active_session: 1,
    next_id: 30,
    revision: 1_000_000,
  };
  await page(`smPlatform.dbClear().then(() => ${api}.storage.local.set({ sm_state: ${JSON.stringify(JSON.stringify(legacy))} })).then(() => true)`);
  page = await reload();
  const migrated = await until('the migrated state in IndexedDB', async () => {
    const d = await dbState(page);
    return d.header && d.tabs.length >= 4 && d;
  });
  // Tabs open in the browser are tracked too (Firefox may have one): only the seeded ones count.
  const seeded = new Set([10, 4, 12, 20]);
  const urls = (sid) => migrated.tabs.filter((t) => t.s === sid && seeded.has(t.id)).map((t) => t.url).sort();
  check('migration: the 0.14 tabs are in IndexedDB, by session',
    isDeepStrictEqual(urls(1), ['https://old.example/a', 'https://old.example/b', 'https://old.example/c']) && isDeepStrictEqual(urls(2), ['https://read.example/']),
    JSON.stringify(migrated.tabs.map(({ id, s, o, url }) => ({ id, s, o, url }))));
  check('migration: the state record holds no tabs', migrated.header.sessions.every((s) => !s.entries || s.entries.length === 0));
  check('migration: the old storage.local state is removed', (await until('sm_state removed', async () => (await local(page, 'sm_state')) === null ? 'gone' : null)) === 'gone');
  const st = (await command(page, { cmd: 'GetState' })).state;
  const old = st.sessions.find((s) => s.name === 'Old work');
  const seededOrder = (ids) => ids.filter((id) => seeded.has(id));
  check('migration: the tab order is kept (not id order)', isDeepStrictEqual(seededOrder(old?.entries.map((e) => e.id) ?? []), [10, 4, 12]), JSON.stringify(old?.entries.map((e) => e.id)));
  check('migration: the revision continues', st.revision > legacy.revision, String(st.revision));
  check('migration: tabs open at the last save can be restored (last open tabs)',
    old.entries.filter((e) => e.tab_id == null && e.last_closed && e.last_closed === old.last_closed).map((e) => e.id).join() === '12');

  // ---- the manager page holds only what it shows, and follows changes
  const manager = await openPage('manager.html');
  const pageState = () => manager('JSON.stringify(globalThis.smPageState ?? null)').then(JSON.parse);
  const shown = await until('the manager page state', async () => (await pageState())?.sessions?.length && pageState());
  const reading0 = shown.sessions.find((x) => x.id === st.sessions.find((y) => y.name === 'Reading').id);
  const old0 = shown.sessions.find((x) => x.id === old.id);
  check('page: the session shown is loaded', !old0.partial && old0.tabs === old0.count && old0.count >= 3, JSON.stringify(old0));
  check('page: other sessions are not loaded, only counted', reading0.partial && reading0.tabs === 0 && reading0.count === 1, JSON.stringify(reading0));

  // ---- saving only what changed
  const before = await dbState(page);
  await command(page, { cmd: 'TagEntries', entries: [{ session: old.id, entry: 4 }], tag: 'changed', add: true });
  const after = await dbState(page);
  const reading = after.header.sessions.find((s) => s.name === 'Reading');
  const oldAfter = after.header.sessions.find((s) => s.name === 'Old work');
  check('a change marks only its session', oldAfter.tabs_revision === after.header.revision && reading.tabs_revision < after.header.revision,
    `old ${oldAfter.tabs_revision}, reading ${reading.tabs_revision}, revision ${after.header.revision}`);
  const byPosition = after.tabs.filter((t) => t.s === oldAfter.id).sort((a, b) => a.o - b.o).map((t) => t.id);
  check('each tab’s saved position keeps the order', isDeepStrictEqual(seededOrder(byPosition), [10, 4, 12]), JSON.stringify(byPosition));
  check('the state record holds no tab order (it doesn’t grow with the tabs)', after.header.sessions.every((x) => !('order' in x)));
  const same = (id) => before.tabs.find((t) => t.id === id)?.j === after.tabs.find((t) => t.id === id)?.j;
  check('only the changed tab was rewritten', !same(4) && [10, 12, 20].every(same));
  const changedKey = await local(page, 'sm_changed');
  check('pages are told about the save (sm_changed)', Number(changedKey) === after.header.revision, String(changedKey));
  check('the other tabs were left as they were', before.tabs.length === after.tabs.length);
  const followed = await until('the page to follow the change', async () => {
    const p = await pageState();
    return p.revision >= after.header.revision && p;
  }).catch(() => null);
  check('page: follows a change made elsewhere (new revision, still only its sessions)',
    followed && followed.sessions.find((x) => x.id === reading.id).partial, JSON.stringify(followed));

  // ---- pages read it from IndexedDB (another page's view of the change)
  const read = JSON.parse(await page(`smPlatform.dbReadState({ ${old.id}: 0, ${reading.id}: ${reading.tabs_revision} }).then(JSON.stringify)`));
  check('a page reads only the sessions that changed', read.sessions.length === 1 && read.sessions[0].id === old.id && read.sessions[0].tabs.some((t) => JSON.parse(t.j).tags?.includes('changed')));

  // ---- it survives a restart, and switching the database off moves everything back
  page = await reload();
  const restarted = (await command(page, { cmd: 'GetState' })).state;
  const tagged = restarted.sessions.flatMap((s) => s.entries).find((e) => e.id === 4);
  check('after a restart the state comes from IndexedDB', tagged?.tags?.includes('changed') && restarted.sessions.length === st.sessions.length);
  const settings = restarted.settings;
  await command(page, { cmd: 'UpdateSettings', settings: { ...settings, tracking: { ...settings.tracking, database: false } } });
  const single = await until('the state in storage.local', async () => {
    const s = await local(page, 'sm_state');
    return s && JSON.parse(s);
  });
  check('database off: everything is in storage.local', single.sessions.flatMap((s) => s.entries).length === restarted.sessions.flatMap((s) => s.entries).length);
  check('database off: IndexedDB is emptied', (await dbState(page)).tabs.length === 0);
  await command(page, { cmd: 'UpdateSettings', settings: { ...settings, tracking: { ...settings.tracking, database: true } } });
  const back = await until('the state back in IndexedDB', async () => {
    const d = await dbState(page);
    return d.tabs.length > 0 && d;
  });
  check('database on again: back in IndexedDB, storage.local cleared', back.tabs.length === after.tabs.length && (await local(page, 'sm_state')) === null);
}

// ------------------------------------------------------------------ Edge / Chrome

async function chromium() {
  const exe = which === 'chrome' ? 'C:/Program Files/Google/Chrome/Application/chrome.exe' : 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';
  const ext = join(root, 'dist', 'chrome');
  browser = spawn(exe, [
    `--user-data-dir=${profile}`, '--remote-debugging-port=9334', `--load-extension=${ext}`, `--disable-extensions-except=${ext}`,
    '--no-first-run', '--no-default-browser-check', '--window-size=1200,800', 'about:blank',
  ]);
  const version = await until('the browser', async () => (await fetch('http://127.0.0.1:9334/json/version')).json());
  const cdp = await connect(version.webSocketDebuggerUrl);
  // Edge has built-in extensions with a background.js too: ours is the one with our name.
  const extId = await until('the extension', async () => {
    const { targetInfos } = await cdp.send('Target.getTargets');
    for (const t of targetInfos.filter((t) => t.type === 'service_worker' && /^chrome-extension:\/\/[a-p]{32}\/background\.js$/.test(t.url))) {
      const { sessionId } = await cdp.send('Target.attachToTarget', { targetId: t.targetId, flatten: true });
      const r = await cdp.send('Runtime.evaluate', { expression: 'chrome.runtime.getManifest().name', returnByValue: true }, sessionId).catch(() => null);
      await cdp.send('Target.detachFromTarget', { sessionId }).catch(() => {});
      if (r?.result?.value === 'Session Manager') return new URL(t.url).host;
    }
    return null;
  });
  console.log(`extension ${extId} loaded in ${which}`);
  const open = async (path = 'inspector.html') => {
    const { targetId } = await cdp.send('Target.createTarget', { url: `chrome-extension://${extId}/${path}` });
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    const evaluate = async (expression) => {
      const r = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId);
      if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
      return r.result.value;
    };
    await until('smPlatform on the page', () => evaluate("typeof smPlatform === 'object'")).catch(async (e) => {
      const why = await evaluate("JSON.stringify({ url: location.href, ready: document.readyState, text: document.body?.innerText?.slice(0, 300) })").catch((x) => x.message);
      throw new Error(`${e.message}: ${why}`);
    });
    return evaluate;
  };
  const page = await open();
  await compareConformance(async (expr) => JSON.parse(await page(expr)));
  const reload = async () => {
    await page('chrome.runtime.reload()').catch(() => {});
    await sleep(2500);
    await until('the extension after reloading', async () => {
      const { targetInfos } = await cdp.send('Target.getTargets');
      return targetInfos.some((t) => t.type === 'service_worker' && t.url.includes(extId));
    });
    return open();
  };
  await extensionChecks(page, reload, 'chrome', open);
}

async function connect(url) {
  const ws = new WebSocket(url);
  await new Promise((ok, fail) => {
    ws.onopen = ok;
    ws.onerror = fail;
  });
  let next = 1;
  const pending = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    const p = pending.get(msg.id);
    if (!p) return;
    pending.delete(msg.id);
    if (msg.error) p.fail(new Error(`${p.method}: ${msg.error.message}`));
    else p.ok(msg.result);
  };
  return {
    send(method, params = {}, sessionId) {
      const id = next++;
      ws.send(JSON.stringify({ id, method, params, sessionId }));
      return new Promise((ok, fail) => pending.set(id, { ok, fail, method }));
    },
  };
}

// ------------------------------------------------------------------ Firefox

async function firefox() {
  const uuid = '5e55104e-0000-4000-8000-000000000002';
  writeFileSync(
    join(profile, 'user.js'),
    [
      `user_pref("extensions.webextensions.uuids", ${JSON.stringify(JSON.stringify({ 'session-manager@local.extension': uuid }))});`,
      // Reinstalling stands in for restarting the extension: keep its data meanwhile.
      'user_pref("extensions.webextensions.keepStorageOnUninstall", true);',
      'user_pref("extensions.webextensions.keepUuidOnUninstall", true);',
      'user_pref("browser.shell.checkDefaultBrowser", false);',
      'user_pref("browser.aboutwelcome.enabled", false);',
      'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
    ].join('\n'),
  );
  browser = spawn('C:/Program Files/Mozilla Firefox/firefox.exe', [
    '-profile', profile, '-no-remote', '-new-instance', '--remote-debugging-port', '9336', '--remote-allow-system-access', '-width', '1200', '-height', '800',
  ], { env: { ...process.env, MOZ_REMOTE_ALLOW_SYSTEM_ACCESS: '1' } });
  const bidi = await until('Firefox', async () => connectBidi('ws://127.0.0.1:9336/session'), 30000);
  await bidi.send('session.new', { capabilities: {} });
  const install = () => bidi.send('webExtension.install', { extensionData: { type: 'path', path: join(root, 'dist', 'firefox') } });
  let { extension } = await install();
  console.log(`extension ${extension} installed in firefox`);
  const base = `moz-extension://${uuid}`;
  const chromeWin = (await bidi.send('browsingContext.getTree', { 'moz:scope': 'chrome' })).contexts[0].context;
  const open = async (path = 'inspector.html') => {
    const known = new Set((await bidi.send('browsingContext.getTree', {})).contexts.map((c) => c.context));
    await bidi.send('script.evaluate', {
      // Selected: pages in background tabs don't draw frames (the UI updates as it draws).
      expression: `(gBrowser.selectedTab = gBrowser.addTab(${JSON.stringify(`${base}/${path}`)}, { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() })) && true`,
      target: { context: chromeWin },
      awaitPromise: false,
    });
    const context = await until('the extension page', async () =>
      (await bidi.send('browsingContext.getTree', {})).contexts.find((c) => !known.has(c.context) && c.url.startsWith(base))?.context);
    const evaluate = async (expression) => {
      const r = await bidi.send('script.evaluate', {
        expression: `Promise.resolve(${expression}).then((v) => v)`,
        target: { context },
        awaitPromise: true,
        resultOwnership: 'none',
      });
      if (r.type === 'exception') throw new Error(r.exceptionDetails.text);
      return r.result.value;
    };
    await until('smPlatform on the page', () => evaluate("typeof smPlatform === 'object'"));
    return evaluate;
  };
  const page = await open();
  await compareConformance(async (expr) => JSON.parse(await page(expr)));
  const reload = async () => {
    // Reinstalling a temporary add-on from the same path keeps its storage (like an update).
    await bidi.send('webExtension.uninstall', { extension }).catch(() => {});
    ({ extension } = await install());
    await sleep(2500);
    return open();
  };
  await extensionChecks(page, reload, 'browser', open);
}

async function connectBidi(url) {
  const ws = new WebSocket(url);
  await new Promise((ok, fail) => {
    ws.onopen = ok;
    ws.onerror = () => fail(new Error('no connection'));
  });
  let next = 1;
  const pending = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    const p = pending.get(msg.id);
    if (!p) return;
    pending.delete(msg.id);
    if (msg.type === 'error') p.fail(new Error(`${p.method}: ${msg.error}: ${msg.message}`));
    else p.ok(msg.result);
  };
  return {
    send(method, params) {
      const id = next++;
      ws.send(JSON.stringify({ id, method, params }));
      return new Promise((ok, fail) => pending.set(id, { ok, fail, method }));
    },
  };
}
