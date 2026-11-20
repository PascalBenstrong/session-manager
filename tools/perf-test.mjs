// Performance regression test: runs the real wasm background against the mock browser at a
// small and a 10× larger state, and checks that the work done per event, per save and per
// page request doesn't grow with the number of tabs.
//
//   ./build.sh && node tools/perf-test.mjs [firefox|chrome]
//
// What it checks (large vs small):
// - a save after changing one tab writes one record, of the same size;
// - a command's reply stays small; a page gets only the session it shows;
// - at startup the background reads the active session and open tabs, not every tab;
// - handling a tab event, a save and a command take about as long (under 2×: anything
//   proportional to all tabs would be ~10×).
//
// SM_PERF_PROFILE=n runs only n renames (a save of the state alone), to profile:
//   node --cpu-prof tools/perf-test.mjs firefox measure large <dump from a seed run>
//
// These count work in the extension's own code with the mock browser. They aren't the CPU /
// memory measurements of PERF_GOALS.md, which need a real browser with live-video tabs.
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const [, , browserName = 'firefox', phase, scaleName, dumpFile] = process.argv;
const SCALES = {
  small: { sessions: 5, perSession: 200, open: 100 },
  large: { sessions: 5, perSession: 2000, open: 500 },
};

if (!phase) {
  await parent();
} else {
  await worker(phase, SCALES[scaleName], dumpFile);
}

// ------------------------------------------------------------------ parent

async function parent() {
  const dir = mkdtempSync(join(tmpdir(), 'sm-perf-'));
  const results = {};
  try {
    for (const scale of Object.keys(SCALES)) {
      const dump = join(dir, `${scale}.json`);
      for (const p of ['seed', 'measure']) {
        const r = spawnSync(process.execPath, ["--expose-gc", fileURLToPath(import.meta.url), browserName, p, scale, dump], { encoding: 'utf8', maxBuffer: 1 << 28 });
        if (r.status !== 0) {
          console.error(r.stdout, r.stderr);
          throw new Error(`${scale} ${p} failed`);
        }
        if (p === 'measure') results[scale] = JSON.parse(r.stdout.trim().split('\n').pop());
      }
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
  const { small, large } = results;
  console.log('             small        large');
  for (const k of Object.keys(small)) console.log(`${k.padEnd(18)} ${String(small[k]).padStart(10)} ${String(large[k]).padStart(12)}`);

  let passed = 0;
  const report = [];
  // `scaling`: { label, ratio, limit } for checks that compare the large run with the small
  // one (charted by tools/perf-report.mjs).
  const check = (name, ok, detail, scaling) => {
    console.log(`${ok ? 'ok  ' : 'FAIL'} - ${name} (${detail})`);
    report.push({ name, ok, detail, ...scaling });
    if (ok) passed++;
    return ok;
  };
  // Timing: medians, floored so that sub-0.05 ms noise doesn't make ratios meaningless.
  const ratio = (k, floor) => Math.max(large[k], floor) / Math.max(small[k], floor);
  const checks = [
    check('a save after one change writes one record', large.savePuts <= 2 && small.savePuts <= 2, `${small.savePuts} / ${large.savePuts} records`),
    check('...of the same size at 10× the tabs', large.saveBytes <= 2 * small.saveBytes + 200, `${small.saveBytes} / ${large.saveBytes} bytes`,
      { label: 'Bytes per save', ratio: large.saveBytes / small.saveBytes, limit: (2 * small.saveBytes + 200) / small.saveBytes }),
    check('a command reply stays small', large.replyBytes < 1500, `${large.replyBytes} bytes`),
    check('a page gets only the session it shows', large.pageBytes < 0.35 * large.fullBytes, `${large.pageBytes} of ${large.fullBytes} bytes`),
    check('an unchanged session is not sent again', large.unchangedBytes < 0.05 * large.pageBytes, `${large.unchangedBytes} bytes`),
    check('startup reads the active session and open tabs, not every tab', large.startupRows <= large.keptTabs + 10 && large.startupRows < 0.4 * large.totalTabs,
      `${large.startupRows} rows read of ${large.totalTabs} tabs (${large.keptTabs} kept)`),
    check('a tab event costs about the same at 10× the tabs', ratio('eventMs', 0.05) < 2, `${small.eventMs} / ${large.eventMs} ms`,
      { label: 'Tab event', ratio: ratio('eventMs', 0.05), limit: 2 }),
    check('activating a tab costs about the same', ratio('activateMs', 0.05) < 2, `${small.activateMs} / ${large.activateMs} ms`,
      { label: 'Tab activation', ratio: ratio('activateMs', 0.05), limit: 2 }),
    check('a save of the state alone costs about the same', ratio('renameMs', 0.2) < 2, `${small.renameMs} / ${large.renameMs} ms`,
      { label: 'Save (state only)', ratio: ratio('renameMs', 0.2), limit: 2 }),
    check('a command (with its save) costs about the same', ratio('commandMs', 0.2) < 2, `${small.commandMs} / ${large.commandMs} ms`,
      { label: 'Command + save', ratio: ratio('commandMs', 0.2), limit: 2 }),
    check('a tab event stays under 5 ms', large.eventMs < 5, `${large.eventMs} ms`),
  ];
  console.log(`\n${passed}/${checks.length} performance checks passed (${browserName})`);
  if (process.env.SM_PERF_OUT) {
    writeFileSync(process.env.SM_PERF_OUT, JSON.stringify({ suite: 'Mock browser', small, large, checks: report }, null, 2));
  }
  process.exit(passed === checks.length ? 0 : 1);
}

// ------------------------------------------------------------------ worker

async function worker(phase, scale, dumpFile) {
  const { createMockBrowser } = await import(pathToFileURL(join(here, 'mock-browser.js')).href);
  const fakeIdb = await import(pathToFileURL(join(here, 'fake-indexeddb.js')).href);
  const { api, tabs, storage, ev } = createMockBrowser();
  globalThis.browser = api;
  globalThis.chrome = api;
  const idb = fakeIdb.createFakeIndexedDB();
  globalThis.indexedDB = idb;
  // Quiet: the background logs what it does.
  console.warn = () => {};

  const total = scale.sessions * scale.perSession;
  const tabOf = (id, url, windowId = 1) => ({ id, windowId, index: id, url, title: `Tab ${id}`, pinned: false, active: false, discarded: false, groupId: -1, hidden: false });
  if (phase === 'seed') {
    // A 0.14 state: sessions of saved tabs; the active one has open tabs.
    let id = 1;
    const sessions = [];
    for (let s = 0; s < scale.sessions; s++) {
      const sid = id++;
      const entries = [];
      for (let i = 0; i < scale.perSession; i++) {
        const open = s === 0 && i < scale.open;
        const eid = id++;
        entries.push({ id: eid, url: `https://s${s}.example/page/${i}`, title: `Page ${i} of session ${s}`, created_at: 1, tab_id: open ? 10_000 + i : null, window_id: open ? 1 : null });
      }
      sessions.push({ id: sid, name: `Session ${s}`, created_at: 1, entries, snapshots: [] });
    }
    storage.sm_state = JSON.stringify({ version: 1, sessions, active_session: sessions[0].id, next_id: id, revision: 1 });
  } else {
    // The database as the seed phase left it: a restart.
    const dump = JSON.parse(readFileSync(dumpFile, 'utf8'));
    for (const [name, db] of dump) {
      idb._databases.set(name, { version: db.version, stores: new Map(db.stores.map(([n, st]) => [n, { records: new Map(st.records), indexes: new Map(st.indexes) }])) });
    }
  }
  for (let i = 0; i < scale.open; i++) tabs.set(10_000 + i, tabOf(10_000 + i, `https://s0.example/page/${i}`));

  await import(pathToFileURL(join(here, '..', 'dist', browserName, 'platform.js')).href);
  const pf = globalThis.smPlatform;
  let handler;
  const setHandler = pf.setEventHandler.bind(pf);
  pf.setEventHandler = (h) => {
    handler = h;
    setHandler(h);
  };
  pf.installBackgroundListeners();
  const dist = join(here, '..', 'dist', browserName);
  const glue = await import(pathToFileURL(join(dist, 'pkg', 'session_manager.js')).href);
  await glue.default({ module_or_path: readFileSync(join(dist, 'pkg', 'session_manager_bg.wasm')) });
  fakeIdb.resetStats();
  await glue.start_background();
  const startupRows = fakeIdb.stats.rowsRead;

  const send = async (cmd) => {
    const raw = await pf.sendMessage(JSON.stringify(cmd));
    const r = JSON.parse(raw);
    assert.ok(r.ok, `${cmd.cmd}: ${r.error}`);
    return { r, bytes: raw.length };
  };
  const tick = () => new Promise((r) => setTimeout(r, 1100));

  if (phase === 'seed') {
    await tick();
    const dump = [...idb._databases].map(([name, db]) => [
      name,
      { version: db.version, stores: [...db.stores].map(([n, st]) => [n, { records: [...st.records], indexes: [...st.indexes] }]) },
    ]);
    writeFileSync(dumpFile, JSON.stringify(dump));
    process.exit(0);
  }

  const median = (xs) => {
    const s = [...xs].sort((a, b) => a - b);
    return Math.round(s[Math.floor(s.length / 2)] * 1000) / 1000;
  };
  const time = async (n, f) => {
    const out = [];
    for (let i = 0; i < n; i++) {
      const t = performance.now();
      await f(i);
      out.push(performance.now() - t);
    }
    return median(out);
  };

  // Only sizes and a few ids are kept: holding the whole state here would slow the timings
  // below with garbage collection that has nothing to do with the extension.
  const { fullBytes, active, pageBytes, unchangedBytes } = await (async () => {
    const full = await send({ cmd: 'GetState' });
    const state = full.r.state;
    const s = state.sessions.find((x) => x.id === state.active_session);
    const active = { id: s.id, entries: s.entries.slice(0, 50).map((e) => ({ id: e.id })) };
    const page = await send({ cmd: 'GetState', sessions: [], with_active: true });
    const known = page.r.state.sessions.map((x) => [x.id, x.tabs_revision]);
    const unchanged = await send({ cmd: 'GetState', sessions: [], with_active: true, known });
    const unchangedBytes =
      unchanged.bytes - JSON.stringify({ ...unchanged.r, state: null }).length - JSON.stringify(unchanged.r.state.sessions.map((x) => ({ ...x, entries: [] }))).length;
    return { fullBytes: full.bytes, active, pageBytes: page.bytes, unchangedBytes };
  })();
  globalThis.gc?.();

  // Tab events: titles change on open tabs (the commonest event while pages load).
  const eventMs = await time(300, (i) => {
    const id = 10_000 + (i % scale.open);
    return handler({ type: 'tabUpdated', tab: { ...tabOf(id, `https://s0.example/page/${i % scale.open}`), title: `New title ${i}` }, urlChanged: false });
  });
  const activateMs = await time(300, (i) => handler({ type: 'tabActivated', tabId: 10_000 + (i % scale.open) }));
  await tick();

  // A command and its save: tag a tab.
  const entry = active.entries[1];
  fakeIdb.resetStats();
  const one = await send({ cmd: 'TagEntries', entries: [{ session: active.id, entry: entry.id }], tag: 'perf', add: true });
  const savePuts = fakeIdb.stats.puts;
  const saveBytes = fakeIdb.stats.bytesWritten;
  const commandMs = await time(60, (i) =>
    send({ cmd: 'TagEntries', entries: [{ session: active.id, entry: active.entries[i % 50].id }], tag: `t${i % 3}`, add: i % 2 === 0 }),
  );

  // Where a command's time goes: one that saves nothing, one that saves only the state.
  const noopMs = await time(60, () => send({ cmd: 'FreeSearchMemory' }));
  const renameMs = await time(60, (i) => send({ cmd: 'RenameSession', id: active.id, name: `Renamed ${i}` }));

  console.log(
    JSON.stringify({
      noopMs,
      renameMs,
      totalTabs: total,
      // What startup reads: the active session, and the open tabs (here all in the active
      // session, so read twice).
      keptTabs: scale.perSession + scale.open,
      startupRows,
      savePuts: savePuts - 1, // the state record
      saveBytes,
      replyBytes: one.bytes,
      fullBytes,
      pageBytes,
      unchangedBytes: Math.max(0, unchangedBytes),
      eventMs,
      activateMs,
      commandMs,
    }),
  );
  process.exit(0);
}
