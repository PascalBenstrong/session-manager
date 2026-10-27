// End-to-end smoke test of the background logic: runs the real wasm build against an
// in-memory mock of the WebExtension API.
//
//   ./build.sh firefox && node tools/smoke-test.mjs [firefox|chrome]
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
import { createMockBrowser } from './mock-browser.js';

const browserName = process.argv[2] || 'firefox';
const dist = join(dirname(fileURLToPath(import.meta.url)), '..', 'dist', browserName);

const { api, tabs, cookies, storage, alarms, removedSiteData, reloaded, tabMessages, ev, clone } = createMockBrowser();
globalThis.browser = api;
globalThis.chrome = api;

// ------------------------------------------------------------------ boot
await import(pathToFileURL(join(dist, 'platform.js')).href);
const pf = globalThis.smPlatform;
pf.installBackgroundListeners();

// An event arriving before wasm is ready must be queued, not lost.
await api.tabs.create({ url: 'https://early.example.com/', active: true });

const glue = await import(pathToFileURL(join(dist, 'pkg', 'session_manager.js')).href);
await glue.default({ module_or_path: readFileSync(join(dist, 'pkg', 'session_manager_bg.wasm')) });
await glue.start_background();

const tick = () => new Promise((r) => setTimeout(r, 20));
const send = async (cmd) => {
  const r = JSON.parse(await pf.sendMessage(JSON.stringify(cmd)));
  assert.ok(r.ok, `${cmd.cmd} failed: ${r.error}`);
  return r;
};
const stored = () => JSON.parse(storage.sm_state);
let passed = 0;
const step = async (name, f) => { await f(); passed++; console.log(`ok - ${name}`); };

await step('queued startup event is tracked', async () => {
  await tick();
  assert.deepEqual(stored().sessions[0].entries.map((e) => e.url), ['https://early.example.com/']);
});

await step('new tabs are added, internal pages ignored, closing removes', async () => {
  const a = await api.tabs.create({ url: 'https://github.com/rust-lang/rust' });
  await api.tabs.create({ url: 'about:newtab' });
  const c = await api.tabs.create({ url: 'https://docs.rs/egui' });
  await tick();
  assert.equal(stored().sessions[0].entries.length, 3);
  // navigation updates the entry
  tabs.get(c.id).url = 'https://docs.rs/eframe';
  ev.updated.fire(c.id, { url: 'https://docs.rs/eframe' }, clone(tabs.get(c.id)));
  await tick();
  assert.ok(stored().sessions[0].entries.some((e) => e.url === 'https://docs.rs/eframe'));
  await api.tabs.remove(a.id);
  await tick();
  assert.equal(stored().sessions[0].entries.length, 2);
});

await step('tag / group matching search results, last tab', async () => {
  const s = (await send({ cmd: 'GetState' })).state;
  const session = s.sessions[0];
  const refs = session.entries.filter((e) => e.url.includes('docs.rs')).map((e) => ({ session: session.id, entry: e.id }));
  await send({ cmd: 'TagEntries', entries: refs, tag: 'rust', add: true });
  await send({ cmd: 'SetGroup', entries: refs, group: 'Docs' });
  const e = stored().sessions[0].entries.find((e) => e.url.includes('docs.rs'));
  assert.deepEqual(e.tags, ['rust']);
  assert.equal(e.group, 'Docs');
  ev.activated.fire({ tabId: e.tab_id, windowId: 1 });
  await tick();
  const r = await send({ cmd: 'FocusLastTab', session: session.id });
  assert.match(r.info, /Last tab/);
});

await step('switch active session without loading, then load it unloaded', async () => {
  const r = await send({ cmd: 'CreateSession', name: 'Work', make_active: true });
  const work = r.state.sessions.find((s) => s.name === 'Work');
  assert.equal(r.state.active_session, work.id);
  const before = tabs.size;
  await api.tabs.create({ url: 'https://work.example.org/' });
  await tick();
  assert.equal(stored().sessions.find((s) => s.id === work.id).entries.length, 1);
  // close it as part of a window close -> kept unloaded
  const t = [...tabs.values()].find((t) => t.url === 'https://work.example.org/');
  tabs.delete(t.id);
  ev.removed.fire(t.id, { isWindowClosing: true, windowId: 1 });
  await tick();
  const entry = stored().sessions.find((s) => s.id === work.id).entries[0];
  assert.equal(entry.tab_id, null);
  // Switch back to Default: no tabs opened
  const def = stored().sessions[0];
  await send({ cmd: 'SetActiveSession', id: def.id });
  assert.equal(tabs.size, before);
  // Load Work unloaded: one discarded tab, linked, no duplicate in Default
  const lr = await send({ cmd: 'LoadSession', id: work.id, discarded: true, new_window: false });
  assert.match(lr.info, /Opened 1 tab/);
  await tick();
  const opened = [...tabs.values()].find((t) => t.url === 'https://work.example.org/');
  assert.ok(opened.discarded, 'tab should be discarded');
  const st = stored();
  assert.equal(st.sessions.find((s) => s.id === work.id).entries[0].tab_id, opened.id);
  assert.ok(!st.sessions[0].entries.some((e) => e.url === 'https://work.example.org/'), 'no duplicate');
});

await step('manual remove keeps tab open and stops tracking it', async () => {
  const st = stored();
  const s = st.sessions[0];
  const e = s.entries.find((e) => e.url === 'https://early.example.com/');
  await send({ cmd: 'RemoveEntries', entries: [{ session: s.id, entry: e.id }], close_tabs: false });
  assert.ok(tabs.has(e.tab_id));
  ev.updated.fire(e.tab_id, { title: 'x' }, { ...clone(tabs.get(e.tab_id)), title: 'x' });
  await tick();
  assert.ok(!stored().sessions[0].entries.some((x) => x.url === 'https://early.example.com/'));
});

await step('organize by domain moves tabs', async () => {
  await api.tabs.create({ url: 'https://a.github.com/' });
  await tick();
  const r = await send({ cmd: 'Organize', key: 'Domain', native_groups: false });
  assert.match(r.info, /Re-organized/);
  const order = [...tabs.values()].sort((a, b) => a.index - b.index).map((t) => new URL(t.url).hostname);
  const firstGithub = order.indexOf('a.github.com');
  assert.ok(firstGithub >= 0);
});

await step('cleanup by pattern and all domains with exclusions', async () => {
  cookies.push(
    { name: 'sid', domain: '.github.com', path: '/', secure: true, storeId: 'default' },
    { name: 'x', domain: 'tracker.net', path: '/', secure: false, storeId: 'default' },
    { name: 'keep', domain: 'docs.rs', path: '/', secure: true, storeId: 'default' },
  );
  let r = await send({ cmd: 'RunCleanup', label: 'gh',
    spec: { target: { Pattern: { kind: 'Domain', value: 'github.com' } }, data_types: { cookies: true, cache: true, localStorage: true, indexedDb: false, serviceWorkers: false }, exclude: [] } });
  assert.equal(r.report.cookies_removed, 1);
  const opts = removedSiteData.at(-1).opts;
  if (browserName === 'firefox') assert.ok(opts.hostnames.includes('github.com'));
  else assert.ok(opts.origins.includes('https://github.com'));
  if (browserName === 'firefox') assert.ok(r.report.warnings.length === 1, 'firefox warns about per-site cache');

  r = await send({ cmd: 'RunCleanup', label: 'all', spec: { target: 'AllDomains', exclude: [{ kind: 'Domain', value: 'docs.rs' }] } });
  assert.equal(r.report.cookies_removed, 1);
  assert.deepEqual(cookies.map((c) => c.name), ['keep']);
  assert.equal(stored().cleanup_log.length, 2);
});

await step('interval rule creates an alarm and runs on alarm', async () => {
  await send({ cmd: 'SaveCleanupRule', rule: { name: 'hourly', enabled: true, interval_minutes: 60, on_cookie_set: false,
    spec: { target: 'AllDomains', data_types: { cookies: true, cache: false, localStorage: false, indexedDb: false, serviceWorkers: false } } } });
  const rule = stored().cleanup_rules[0];
  assert.equal(alarms.get(`sm-cleanup-${rule.id}`).periodInMinutes, 60);
  ev.alarm.fire({ name: `sm-cleanup-${rule.id}` });
  await tick();
  assert.equal(cookies.length, 0);
  assert.ok(stored().cleanup_rules[0].last_run > 0);
  await send({ cmd: 'DeleteCleanupRule', id: rule.id });
  assert.equal(alarms.size, 0);
});

await step('on-cookie-set rule removes new cookies', async () => {
  await send({ cmd: 'SaveCleanupRule', rule: { name: 'block ads', enabled: true, on_cookie_set: true,
    spec: { target: { Pattern: { kind: 'Glob', value: '*.ads.*' } } } } });
  const c = { name: 'track', domain: '.x.ads.com', path: '/', secure: true, storeId: 'default' };
  cookies.push(c);
  ev.cookie.fire({ removed: false, cookie: c, cause: 'explicit' });
  const ok = { name: 'fine', domain: 'good.com', path: '/', secure: true, storeId: 'default' };
  cookies.push(ok);
  ev.cookie.fire({ removed: false, cookie: ok, cause: 'explicit' });
  await tick();
  assert.deepEqual(cookies.map((c) => c.name), ['fine']);
});

await step('browser restart: new background instance relinks tabs by URL', async () => {
  const before = stored();
  // Browser restart: every tab gets a new id.
  const old = [...tabs.values()];
  tabs.clear();
  for (const t of old) { t.id += 1000; tabs.set(t.id, t); }
  // Fresh module instance = fresh wasm memory, like a restarted background.
  const glue2 = await import(pathToFileURL(join(dist, 'pkg', 'session_manager.js')).href + '?restart');
  await glue2.default({ module_or_path: readFileSync(join(dist, 'pkg', 'session_manager_bg.wasm')) });
  await glue2.start_background();
  const after = stored();
  const count = (st) => st.sessions.reduce((n, s) => n + s.entries.length, 0);
  assert.equal(count(after), count(before) + 1, 'only the untracked (ignored before restart) tab is new');
  for (const s of after.sessions) for (const e of s.entries) {
    if (e.tab_id != null) assert.ok(tabs.has(e.tab_id) && tabs.get(e.tab_id).url === e.url, `${e.url} relinked`);
  }
  assert.ok(after.sessions[1].entries[0].tab_id > 1000);
});

await step('snapshots: save, replace (with open), merge, new session', async () => {
  const sid = stored().active_session;
  const urlsOf = (id) => stored().sessions.find((s) => s.id === id).entries.map((e) => e.url).sort();
  const before = urlsOf(sid);
  await send({ cmd: 'CreateSnapshot', session: sid, name: 'v1' });
  const snap = stored().sessions.find((s) => s.id === sid).snapshots.at(-1);
  assert.equal(snap.name, 'v1');
  assert.ok(snap.entries.every((e) => e.tab_id == null), 'snapshots hold no live tabs');

  // change the session: close one tab, open another
  const victim = stored().sessions.find((s) => s.id === sid).entries.find((e) => e.tab_id != null);
  await api.tabs.remove(victim.tab_id);
  await api.tabs.create({ url: 'https://after-snapshot.example/' });
  await tick();
  assert.notDeepEqual(urlsOf(sid), before);

  const tabsBefore = tabs.size;
  const r = await send({ cmd: 'RestoreSnapshot', session: sid, snapshot: snap.id, mode: 'Replace', open: true });
  assert.match(r.info, /restored/);
  await tick();
  assert.deepEqual(urlsOf(sid), before);
  assert.equal(tabs.size, tabsBefore + 1, 'the missing tab was reopened');
  assert.ok([...tabs.values()].some((t) => t.url === victim.url && t.discarded), 'reopened unloaded');
  const snaps = stored().sessions.find((s) => s.id === sid).snapshots;
  assert.ok(snaps.at(-1).auto && snaps.at(-1).entries.some((e) => e.url === 'https://after-snapshot.example/'), 'previous list kept');

  // merge brings back nothing new now; new-session copies it
  const m = await send({ cmd: 'RestoreSnapshot', session: sid, snapshot: snap.id, mode: 'Merge', open: false });
  assert.match(m.info, /Added 0/);
  const n = await send({ cmd: 'RestoreSnapshot', session: sid, snapshot: snap.id, mode: 'NewSession', open: false });
  const copy = n.state.sessions.find((s) => s.name.endsWith('(v1)'));
  assert.deepEqual(copy.entries.map((e) => e.url).sort(), before);
  await send({ cmd: 'DeleteSnapshot', session: sid, snapshot: snap.id });
  assert.ok(!stored().sessions.find((s) => s.id === sid).snapshots.some((x) => x.id === snap.id));
});

await step('import URL list into a new session and open it; import a session file', async () => {
  const count = tabs.size;
  const r = await send({
    cmd: 'ImportUrls',
    urls: ['https://import-a.example/', 'https://import-b.example/x'],
    target: { NewSession: 'Imported list' },
    open: true,
  });
  assert.match(r.info, /Imported 2 URL/);
  await tick();
  const s = stored().sessions.find((x) => x.name === 'Imported list');
  assert.equal(s.entries.length, 2);
  assert.equal(tabs.size, count + 2);
  assert.ok(s.entries.every((e) => e.tab_id != null), 'opened tabs are linked to the imported entries');

  const file = { format: 'session-manager', version: 1, exported_at: 0, sessions: [{ name: 'From disk', entries: [{ url: 'https://disk.example/' }] }] };
  const i = await send({ cmd: 'ImportSessions', sessions: file.sessions });
  assert.match(i.info, /Imported 1 session/);
  assert.ok(stored().sessions.some((x) => x.name === 'From disk' && x.entries[0].url === 'https://disk.example/'));
});

await step('automatic snapshots run from an alarm', async () => {
  const settings = { ...stored().settings, auto_snapshot_minutes: 30 };
  await send({ cmd: 'UpdateSettings', settings });
  assert.equal(alarms.get('sm-snapshot').periodInMinutes, 30);
  const total = () => stored().sessions.reduce((n, s) => n + s.snapshots.length, 0);
  const n0 = total();
  ev.alarm.fire({ name: 'sm-snapshot' });
  await tick();
  const n1 = total();
  assert.ok(n1 > n0, 'changed sessions got a snapshot');
  ev.alarm.fire({ name: 'sm-snapshot' });
  await tick();
  assert.equal(total(), n1, 'unchanged sessions are not snapshotted again');
  await send({ cmd: 'UpdateSettings', settings: { ...settings, auto_snapshot_minutes: null } });
  assert.ok(!alarms.has('sm-snapshot'));
});

await step('remove duplicate tabs (closes the extra copies, merges tags)', async () => {
  const r0 = await send({ cmd: 'CreateSession', name: 'Dups', make_active: true });
  const sid = r0.state.sessions.find((s) => s.name === 'Dups').id;
  const a1 = await api.tabs.create({ url: 'https://dup.example/page' });
  const a2 = await api.tabs.create({ url: 'https://dup.example/page#section' });
  await api.tabs.create({ url: 'https://dup.example/page/' });
  await api.tabs.create({ url: 'https://unique.example/' });
  await tick();
  const session = () => stored().sessions.find((s) => s.id === sid);
  assert.equal(session().entries.length, 4);
  const e2 = session().entries.find((e) => e.tab_id === a2.id);
  await send({ cmd: 'TagEntries', entries: [{ session: sid, entry: e2.id }], tag: 'keepme', add: true });
  ev.activated.fire({ tabId: a1.id });
  await tick();

  const r = await send({ cmd: 'RemoveDuplicates', scope: { Session: sid }, close_tabs: true });
  assert.match(r.info, /Removed 2 duplicate/);
  await tick();
  const left = session().entries;
  assert.equal(left.length, 2);
  const kept = left.find((e) => e.url.startsWith('https://dup.example'));
  assert.equal(kept.tab_id, a1.id, 'most recently used copy kept');
  assert.deepEqual(kept.tags, ['keepme'], 'tags merged');
  assert.equal([...tabs.values()].filter((t) => t.url.startsWith('https://dup.example')).length, 1, 'extra tabs closed');
  const again = await send({ cmd: 'RemoveDuplicates', scope: { Session: sid }, close_tabs: true });
  assert.match(again.info, /No duplicate/);
});

await step('prevent duplicates: switch to the open copy, adopt the unloaded one', async () => {
  const sid = stored().active_session;
  await send({ cmd: 'UpdateSettings', settings: { ...stored().settings, prevent_duplicates: true } });
  const focused = [];
  const origUpdate = api.tabs.update;
  api.tabs.update = async (id, p) => (p.active && focused.push(id), origUpdate(id, p));

  const existing = [...tabs.values()].find((t) => t.url === 'https://dup.example/page');
  const dup = await api.tabs.create({ url: 'https://dup.example/page/#again', active: true });
  await tick();
  assert.ok(!tabs.has(dup.id), 'the new duplicate tab was closed');
  assert.deepEqual(focused, [existing.id], 'switched to the existing tab');

  // an unloaded saved page: the new tab becomes that entry instead of a second one
  const u = [...tabs.values()].find((t) => t.url === 'https://unique.example/');
  tabs.delete(u.id);
  ev.removed.fire(u.id, { isWindowClosing: true });
  await tick();
  const count = stored().sessions.find((s) => s.id === sid).entries.length;
  const reopened = await api.tabs.create({ url: 'https://unique.example/' });
  await tick();
  const entries = stored().sessions.find((s) => s.id === sid).entries;
  assert.equal(entries.length, count, 'no new entry');
  assert.equal(entries.find((e) => e.url === 'https://unique.example/').tab_id, reopened.id);

  // loading a session that itself contains duplicates is not affected
  await send({ cmd: 'ImportSessions', sessions: [{ name: 'Twice2', entries: [{ url: 'https://twice.example/a' }, { url: 'https://twice.example/a#2' }] }] });
  const t2 = stored().sessions.find((s) => s.name === 'Twice2');
  await send({ cmd: 'SetActiveSession', id: t2.id });
  const before = tabs.size;
  await send({ cmd: 'LoadSession', id: t2.id, discarded: true, new_window: false });
  await tick();
  assert.equal(tabs.size, before + 2, 'both copies opened when loading a session');
  api.tabs.update = origUpdate;
  await send({ cmd: 'UpdateSettings', settings: { ...stored().settings, prevent_duplicates: false } });
});

await step('clear cookies & site data of the current tab / a given tab', async () => {
  for (const t of tabs.values()) t.active = false;
  const cur = await api.tabs.create({ url: 'https://www.shop.example/cart', active: true });
  const other = await api.tabs.create({ url: 'https://news.example/' });
  cookies.push(
    { name: 'sid', domain: '.shop.example', path: '/', secure: true },
    { name: 'pref', domain: 'www.shop.example', path: '/', secure: true },
    { name: 'n', domain: 'news.example', path: '/', secure: true },
  );
  const r = await send({ cmd: 'ClearTabData', tab: null, data_types: { cookies: true, cache: true, localStorage: true, indexedDb: true, serviceWorkers: true }, reload: true });
  assert.equal(r.report.cookies_removed, 2);
  assert.deepEqual(r.report.hosts, ['www.shop.example']);
  assert.ok(cookies.some((c) => c.name === 'n'), 'other sites untouched');
  assert.deepEqual(reloaded, [cur.id]);
  assert.equal(stored().cleanup_log[0].label, 'Tab: www.shop.example');

  // a specific tab, cookies only, no reload
  const before = removedSiteData.length;
  const r2 = await send({ cmd: 'ClearTabData', tab: other.id, data_types: { cookies: true, cache: false, localStorage: false, indexedDb: false, serviceWorkers: false }, reload: false });
  assert.equal(r2.report.cookies_removed, 1);
  assert.equal(removedSiteData.length, before, 'no site data removal for cookies only');
  assert.deepEqual(reloaded, [cur.id]);

  const internal = await api.tabs.create({ url: 'about:blank' });
  const bad = JSON.parse(await pf.sendMessage(JSON.stringify({ cmd: 'ClearTabData', tab: internal.id, data_types: {}, reload: false })));
  assert.ok(!bad.ok && /no site data/.test(bad.error));
  await api.tabs.remove([cur.id, other.id, internal.id]);
});

await step('auto-remove duplicates when a session is reopened', async () => {
  await send({ cmd: 'UpdateSettings', settings: { ...stored().settings, dedupe_on_session_load: true } });
  await send({ cmd: 'ImportSessions', sessions: [{ name: 'Twice3', entries: [{ url: 'https://twice3.example/a' }, { url: 'https://twice3.example/a#2' }, { url: 'https://twice3.example/b' }] }] });
  const t3 = stored().sessions.find((s) => s.name === 'Twice3');
  const before = tabs.size;
  const r = await send({ cmd: 'LoadSession', id: t3.id, discarded: true, new_window: false });
  await tick();
  assert.match(r.info, /Opened 2 tab\(s\) unloaded; removed 1 duplicate/);
  assert.equal(tabs.size, before + 2, 'only one copy opened');
  assert.equal(stored().sessions.find((s) => s.id === t3.id).entries.length, 2);
  await send({ cmd: 'UpdateSettings', settings: { ...stored().settings, dedupe_on_session_load: false } });
});

await step('auto-remove duplicates after the browser restarts and restores tabs', async () => {
  // Saved, unloaded tabs in an inactive session (its window was closed on quit).
  await send({ cmd: 'ImportSessions', sessions: [{ name: 'Restored', entries: [{ url: 'https://restore.example/a' }, { url: 'https://restore.example/b' }] }] });
  const sid = stored().sessions.find((s) => s.name === 'Restored').id;
  const active = stored().active_session;
  await send({ cmd: 'UpdateSettings', settings: { ...stored().settings, dedupe_on_startup: true } });
  ev.startup.fire();
  await tick();
  // The browser restores the tabs after startup; one page twice.
  const a = await api.tabs.create({ url: 'https://restore.example/a' });
  const b = await api.tabs.create({ url: 'https://restore.example/b' });
  const a2 = await api.tabs.create({ url: 'https://restore.example/a' });
  await tick();
  const entries = () => stored().sessions.find((s) => s.id === sid).entries;
  assert.equal(entries().length, 2, 'restored tabs linked to their saved entries');
  assert.deepEqual(entries().map((e) => e.tab_id), [a.id, b.id]);
  await new Promise((r) => setTimeout(r, 3300));
  assert.ok(tabs.has(a.id) && tabs.has(b.id));
  assert.ok(!tabs.has(a2.id), 'extra restored copy closed');
  const all = stored().sessions.flatMap((s) => s.entries);
  assert.ok(!all.some((e) => e.tab_id === a2.id), 'and its entry removed');
  assert.equal(all.filter((e) => e.url === 'https://twice.example/a').length, 1, 'older duplicates in a session removed too');
  // The startup window is over: new tabs are tracked normally again.
  const late = await api.tabs.create({ url: 'https://restore.example/b' });
  await tick();
  assert.ok(stored().sessions.find((s) => s.id === active).entries.some((e) => e.tab_id === late.id));
  await send({ cmd: 'UpdateSettings', settings: { ...stored().settings, dedupe_on_startup: false } });
});

await step('workspaces: rules place new tabs, group rules, manual choice, delete', async () => {
  const site = { Site: { kind: 'Domain', value: 'rules.example' } };
  let r = await send({ cmd: 'SaveWorkspace', workspace: { id: 0, name: 'Rules', color: 'Green', icon: '💻', auto_add: true, rules: [site] } });
  const ws = r.state.workspaces.find((w) => w.name === 'Rules');
  assert.equal(ws.color, 'Green');
  const t = await api.tabs.create({ url: 'https://docs.rules.example/a' });
  await tick();
  const entry = () => stored().sessions.flatMap((s) => s.entries).find((e) => e.tab_id === t.id);
  assert.equal(entry().workspace, ws.id, 'new tab placed by its URL');

  r = await send({ cmd: 'SaveWorkspace', workspace: { id: 0, name: 'Later', rules: [{ Group: 'read later' }] } });
  const later = r.state.workspaces.find((w) => w.name === 'Later');
  const t2 = await api.tabs.create({ url: 'https://plain.example/article' });
  await tick();
  const entry2 = () => stored().sessions.flatMap((s) => s.entries).find((e) => e.tab_id === t2.id);
  const session = stored().sessions.find((s) => s.entries.some((e) => e.tab_id === t2.id));
  const ref = { session: session.id, entry: entry2().id };
  assert.equal(entry2().workspace, null);
  // Adding a tab to a group places it in that group's workspace...
  await send({ cmd: 'SetGroup', entries: [ref], group: 'Read Later' });
  assert.equal(entry2().workspace, later.id);
  // ...but a choice made by hand sticks.
  await send({ cmd: 'AssignWorkspace', entries: [ref], workspace: ws.id });
  await send({ cmd: 'SetGroup', entries: [ref], group: 'read later' });
  assert.equal(entry2().workspace, ws.id);
  assert.ok(entry2().workspace_manual);
  r = await send({ cmd: 'AutoPlaceEntries', entries: [ref] });
  assert.equal(entry2().workspace, later.id, 'rules decide again');
  // A tab matching several workspaces goes to the first one.
  await send({ cmd: 'SetGroup', entries: [{ session: session.id, entry: entry().id }], group: 'read later' });
  assert.equal(entry().workspace, ws.id);

  await send({ cmd: 'MoveWorkspace', id: later.id, delta: -1 });
  assert.deepEqual(stored().workspaces.map((w) => w.name), ['Later', 'Rules']);
  await send({ cmd: 'DeleteWorkspace', id: later.id });
  assert.equal(entry2().workspace, null, 'tabs of a deleted workspace stay, without a workspace');
  const bad = JSON.parse(await pf.sendMessage(JSON.stringify({ cmd: 'SaveWorkspace', workspace: { id: 0, name: 'x', rules: [{ Query: '/(/' }] } })));
  assert.ok(!bad.ok && /regex/.test(bad.error));
});

await step('hibernate: all tabs skips active / pinned; force includes pinned', async () => {
  for (const t of tabs.values()) t.discarded = false;
  const pinned = await api.tabs.create({ url: 'https://pinned.example/', pinned: true });
  const active = await api.tabs.create({ url: 'https://active.example/', active: true });
  await tick();
  let r = await send({ cmd: 'Hibernate', target: 'All', force: false });
  assert.match(r.info, /Hibernated \d+ tab/);
  assert.ok(!tabs.get(active.id).discarded, 'active tab stays loaded');
  assert.ok(!tabs.get(pinned.id).discarded, 'pinned tab stays loaded');
  assert.ok([...tabs.values()].filter((t) => !t.active && !t.pinned).every((t) => t.discarded));
  for (const e of stored().sessions.flatMap((s) => s.entries).filter((e) => e.tab_id)) {
    assert.equal(e.discarded, tabs.get(e.tab_id).discarded, 'entries show which tabs are hibernated');
  }
  r = await send({ cmd: 'Hibernate', target: 'All', force: false });
  assert.match(r.info, /No tabs to hibernate/);
  r = await send({ cmd: 'Hibernate', target: 'All', force: true });
  assert.ok(tabs.get(pinned.id).discarded);
  // A target by entries only touches those tabs.
  for (const t of tabs.values()) t.discarded = false;
  const e = stored().sessions.flatMap((s) => s.entries.map((x) => ({ s, x }))).find(({ x }) => x.tab_id && !tabs.get(x.tab_id).active);
  r = await send({ cmd: 'Hibernate', target: { Entries: [{ session: e.s.id, entry: e.x.id }] }, force: false });
  assert.equal([...tabs.values()].filter((t) => t.discarded).length, 1);
  for (const t of tabs.values()) t.discarded = false;
});

await step('media limit: background video is paused, resumed when its tab is active', async () => {
  const settings = stored().settings;
  await send({ cmd: 'UpdateSettings', settings: { ...settings, media: { ...settings.media, policy: 'ActiveTabOnly' } } });
  for (const t of tabs.values()) t.active = false;
  const fg = await api.tabs.create({ url: 'https://video.example/fg', active: true });
  const bg = await api.tabs.create({ url: 'https://video.example/bg' });
  const media = (tab, playing, video = true) =>
    ev.message.fire({ smMedia: { playing, video, pip: false } }, { tab: { id: tab.id }, frameId: 0 }, () => {});
  tabMessages.length = 0;
  media(fg, true);
  await tick();
  assert.deepEqual(tabMessages, [], 'the active tab may play');
  media(bg, true);
  await tick();
  assert.deepEqual(tabMessages, [{ tabId: bg.id, msg: { smMediaCmd: 'pause', audio: false } }]);
  assert.deepEqual(stored().media.paused, [bg.id]);
  // audio only is not limited by default
  tabMessages.length = 0;
  const music = await api.tabs.create({ url: 'https://music.example/' });
  media(music, true, false);
  await tick();
  assert.deepEqual(tabMessages, []);
  // switching to the paused tab resumes it, and pauses the one left behind
  media(bg, false);
  tabs.get(fg.id).active = false;
  tabs.get(bg.id).active = true;
  ev.activated.fire({ tabId: bg.id, windowId: 1 });
  await tick();
  assert.deepEqual(
    tabMessages.map((m) => [m.tabId, m.msg.smMediaCmd]).sort(),
    [[bg.id, 'resume'], [fg.id, 'pause']].sort(),
  );
  // turning the limit off forgets everything
  await send({ cmd: 'UpdateSettings', settings: { ...stored().settings, media: { ...settings.media, policy: 'Off' } } });
  assert.deepEqual(stored().media, { playing: [], paused: [] });
  tabMessages.length = 0;
  media(bg, true);
  await tick();
  assert.deepEqual(tabMessages, []);
});

await step('install/update sets the NEW badge; viewing release notes clears it', async () => {
  ev.installed.fire({ reason: 'update', previousVersion: '0.1.0' });
  await tick();
  assert.equal(api.action.badge, 'NEW');
  pf.clearBadge();
  await tick();
  assert.equal(api.action.badge, '');
  ev.installed.fire({ reason: 'chrome_update' });
  await tick();
  assert.equal(api.action.badge, '', 'browser updates do not flag release notes');
});

console.log(`\n${passed} checks passed (${browserName})`);
process.exit(0);
