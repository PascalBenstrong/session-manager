// Performance regression test in a real browser: the built extension, with the browser's
// own IndexedDB, storage, messaging and background (service worker / event page).
//
//   ./build.sh && node tools/perf-real-test.mjs edge|chrome|firefox      (SM_HEADLESS=1 in CI)
//
// Two runs, each in a fresh profile: the same sessions, the active one the same size in
// both; the inactive ones hold 10× more tabs in the large one. Everything the background does for
// the active session must cost the same — inactive sessions' saved tabs stay in the
// database — so the large run must not use more memory, start slower, or take longer per
// event, command or page request. The background reports its own handler times and wasm
// memory (`{ smTimings }`, web/platform-common.js).
//
// Real tabs load pages from a local server started here. These are regression checks of the
// extension's own work; the CPU / memory measurements of PERF_GOALS.md (hundreds of tabs
// playing live video) are a separate, manual run.
import { spawn } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launch, sleep, until } from './real-browser.mjs';

const which = process.argv[2] || 'edge';
const here = dirname(fileURLToPath(import.meta.url));
const SITE_PORT = 8124;
const SITE = `http://127.0.0.1:${SITE_PORT}/tools/real/data.json`;
const ACTIVE_TABS = 300;
const OPEN_TABS = 12;
const INACTIVE_SESSIONS = 4;
// Tabs per inactive session: the large run has 10× more tabs in the same sessions.
const SCALES = { small: 300, large: 3000 };

const server = spawn(process.execPath, [join(here, 'serve.mjs'), String(SITE_PORT)], { stdio: 'ignore' });
let passed = 0;
const results = [];
const report = [];
// `scaling`: { label, ratio, limit } for checks that compare the large run with the small one
// (charted by tools/perf-report.mjs).
const check = (name, ok, detail, scaling) => {
  results.push(ok);
  report.push({ name, ok, detail, ...scaling });
  console.log(`${ok ? 'ok  ' : 'FAIL'} - ${name} (${detail})`);
  if (ok) passed++;
};
const measured = {};
try {
  await until('the local site', async () => (await fetch(SITE)).ok, 10000);
  for (const [scale, perSession] of Object.entries(SCALES)) {
    measured[scale] = await run(perSession);
    console.log(`${scale}: ${JSON.stringify(measured[scale])}`);
  }
  const { small, large } = measured;
  const ratio = (k, floor) => Math.max(large[k], floor) / Math.max(small[k], floor);
  check('background memory after a restart doesn’t grow with inactive sessions', large.wasmMB <= small.wasmMB * 1.25 + 1, `${small.wasmMB} / ${large.wasmMB} MB`,
    { label: 'Background memory', ratio: large.wasmMB / small.wasmMB, limit: (small.wasmMB * 1.25 + 1) / small.wasmMB });
  check('startup takes about as long', ratio('startupMs', 100) < 2, `${small.startupMs} / ${large.startupMs} ms`,
    { label: 'Startup', ratio: ratio('startupMs', 100), limit: 2 });
  check('a tab update costs about the same', ratio('tabUpdatedMs', 1) < 2, `${small.tabUpdatedMs} / ${large.tabUpdatedMs} ms`,
    { label: 'Tab update', ratio: ratio('tabUpdatedMs', 1), limit: 2 });
  check('a command (with its save) costs about the same', ratio('commandMs', 2) < 2, `${small.commandMs} / ${large.commandMs} ms`,
    { label: 'Command + save', ratio: ratio('commandMs', 2), limit: 2 });
  check('...seen from the page too', ratio('roundTripMs', 3) < 2, `${small.roundTripMs} / ${large.roundTripMs} ms`,
    { label: 'Round trip from a page', ratio: ratio('roundTripMs', 3), limit: 2 });
  check('a page gets about as much', large.pageBytes < small.pageBytes * 1.3, `${small.pageBytes} / ${large.pageBytes} bytes`,
    { label: 'Page payload', ratio: large.pageBytes / small.pageBytes, limit: 1.3 });
  check('a save after one change rewrites one tab', large.rewritten === 1 && small.rewritten === 1, `${small.rewritten} / ${large.rewritten}`);
  // Reading a session back costs what that session holds (10× more in the large run): not more per tab.
  const perTab = (r, n) => Math.max(r.readBackMs, 5) / n;
  check('reading an inactive session back costs the same per tab', perTab(large, SCALES.large) < 2 * perTab(small, SCALES.small),
    `${small.readBackMs} ms for ${SCALES.small} / ${large.readBackMs} ms for ${SCALES.large} tabs`,
    { label: 'Read-back per tab', ratio: perTab(large, SCALES.large) / perTab(small, SCALES.small), limit: 2 });
} catch (e) {
  check('the run finished', false, e.stack || e.message);
} finally {
  server.kill();
}
console.log(`\n${passed}/${results.length} performance checks passed (${which}, real browser)`);
if (process.env.SM_PERF_OUT) {
  const name = { edge: 'Edge', chrome: 'Chrome', firefox: 'Firefox' }[which] || which;
  writeFileSync(process.env.SM_PERF_OUT, JSON.stringify({ suite: `${name} (real browser)`, ...measured, checks: report }, null, 2));
}
process.exit(passed === results.length ? 0 : 1);

async function run(perSession) {
  const browser = await launch(which);
  try {
    let page = browser.page();
    const api = browser.api;
    const json = async (expr) => JSON.parse(await page(`Promise.resolve(${expr}).then(JSON.stringify)`));
    const command = async (cmd) => {
      const r = await json(`smPlatform.sendMessage(${JSON.stringify(JSON.stringify(cmd))}).then(JSON.parse)`);
      if (!r.ok) throw new Error(`${cmd.cmd}: ${r.error}`);
      return r;
    };
    const timings = (reset = false) => json(`${api}.runtime.sendMessage({ smTimings: ${reset ? "'reset'" : 'true'} })`);
    const median = (xs) => {
      const s = [...(xs || [])].sort((a, b) => a - b);
      return s.length ? Math.round(s[Math.floor(s.length / 2)] * 100) / 100 : 0;
    };

    // A 0.14 state (moved into the database at the next start): the active session and the
    // inactive ones, all saved tabs.
    let id = 1;
    const sessions = [];
    for (let s = 0; s <= INACTIVE_SESSIONS; s++) {
      const sid = id++;
      const n = s === 0 ? ACTIVE_TABS : perSession;
      const entries = Array.from({ length: n }, (_, i) => ({ id: id++, url: `https://s${s}.example/page/${i}`, title: `Page ${i} of session ${s}`, created_at: 1 }));
      sessions.push({ id: sid, name: `Session ${s}`, created_at: 1, entries, snapshots: [] });
    }
    const legacy = { version: 1, sessions, active_session: sessions[0].id, next_id: id, revision: 10 };
    await page(`smPlatform.dbClear().then(() => ${api}.storage.local.set({ sm_state: ${JSON.stringify(JSON.stringify(legacy))} })).then(() => true)`);
    page = await browser.reload();
    await until('the state moved into the database', async () => (await json(`${api}.storage.local.get('sm_state')`)).sm_state === undefined, 30000);
    // Real tabs in the active session.
    for (let i = 0; i < OPEN_TABS; i++) await page(`${api}.tabs.create({ url: ${JSON.stringify(`${SITE}?t=${i}`)}, active: false }).then(() => true)`);
    await sleep(2500);

    // Restarts: the background reads only what it keeps. Startup is the median of three (a
    // single one is noisy: it includes compiling the wasm module).
    const starts = [];
    let state;
    let t0;
    for (let i = 0; i < 3; i++) {
      page = await browser.reload();
      state = await until('the background', async () => (await command({ cmd: 'GetState', sessions: [], with_active: true })).state, 30000);
      await sleep(1500);
      t0 = await timings(true);
      starts.push(t0.startupMs);
    }

    // Tab updates: the open tabs load other pages.
    const tabIds = await json(`${api}.tabs.query({ url: 'http://127.0.0.1/*' }).then((t) => t.map((x) => x.id))`).catch(() => []);
    const ids = tabIds.length ? tabIds : await json(`${api}.tabs.query({}).then((t) => t.filter((x) => x.url.includes(':${SITE_PORT}/')).map((x) => x.id))`);
    for (let round = 0; round < 4; round++) {
      for (const t of ids) await page(`${api}.tabs.update(${t}, { url: ${JSON.stringify(SITE)} + '?r=${round}&t=' + ${t} }).then(() => true)`);
      await sleep(800);
    }
    await sleep(1500);

    // Commands with their save: tag tabs of the active session.
    const active = state.sessions.find((s) => s.id === state.active_session);
    const before = await json(`smPlatform.dbLoadSessions([${active.id}])`);
    const trips = [];
    for (let i = 0; i < 30; i++) {
      const t = Date.now();
      await command({ cmd: 'TagEntries', entries: [{ session: active.id, entry: active.entries[i % 20].id }], tag: `perf${i % 3}`, add: i % 2 === 0 });
      trips.push(Date.now() - t);
    }
    // One change: one tab rewritten.
    const mid = await json(`smPlatform.dbLoadSessions([${active.id}])`);
    await command({ cmd: 'TagEntries', entries: [{ session: active.id, entry: active.entries[25].id }], tag: 'once', add: true });
    const after = await json(`smPlatform.dbLoadSessions([${active.id}])`);
    const was = new Map(mid.map((r) => [r.id, r.j]));
    const rewritten = after.filter((r) => was.get(r.id) !== r.j).length;
    void before;

    // A page asking for the session it shows.
    const pageBytes = JSON.stringify(await command({ cmd: 'GetState', sessions: [], with_active: true })).length;
    // Reading an inactive session back (a page viewing it).
    const other = state.sessions.find((s) => s.id !== state.active_session);
    const r0 = Date.now();
    await command({ cmd: 'GetState', sessions: [other.id], with_active: true });
    const readBackMs = Date.now() - r0;

    const t1 = await timings();
    return {
      wasmMB: Math.round((t0.wasmBytes / 1048576) * 10) / 10,
      startupMs: Math.round(median(starts)),
      tabUpdatedMs: median(t1.timings.tabUpdated),
      commandMs: median(t1.timings['cmd:TagEntries']),
      roundTripMs: median(trips),
      pageBytes,
      rewritten,
      readBackMs,
    };
  } finally {
    await browser.close();
  }
}
