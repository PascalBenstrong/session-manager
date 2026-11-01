// Tests the page guards, ad-tab rules and the inspector in a real, visible browser with the
// developer tools open (what headless runs and the mock can't show).
//
//   ./build.sh && node tools/serve.mjs &   (the test site is tools/real/)
//   node tools/real-browser-test.mjs edge      # Edge/Chrome via the DevTools Protocol
//   node tools/real-browser-test.mjs firefox   # Firefox via WebDriver BiDi
//
// It uses a throwaway profile, so your own browser profile is never touched.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const which = process.argv[2] || 'edge';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SITE = 'http://127.0.0.1:8123/tools/real/hostile.html';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const until = async (what, f, ms = 10000) => {
  for (const end = Date.now() + ms; Date.now() < end; await sleep(200)) {
    const v = await f().catch(() => undefined);
    if (v) return v;
  }
  throw new Error(`timed out waiting for ${what}`);
};
let passed = 0;
const results = [];
/** Console messages and log entries of attached pages (after Log.enable). */
const consoleLog = [];
const check = (name, ok, detail = '') => {
  results.push(`${ok ? 'ok  ' : 'FAIL'} - ${name}${detail ? ` (${detail})` : ''}`);
  console.log(results[results.length - 1]);
  if (ok) passed++;
};

const profile = mkdtempSync(join(tmpdir(), 'sm-real-'));
const shots = join(tmpdir(), 'sm-real-shots');
let browser;
try {
  if (which === 'edge' || which === 'chrome') await chromium();
  else await firefox();
} catch (e) {
  check('the run finished', false, e.message);
} finally {
  // Firefox and Edge run several processes: end the whole tree.
  if (browser?.pid) spawn('taskkill', ['/PID', String(browser.pid), '/T', '/F']);
  await sleep(1000);
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {}
}
console.log(`\n${passed}/${results.length} checks passed (${which}); screenshots in ${shots}`);
process.exit(results.length === passed ? 0 : 1);

// ------------------------------------------------------------------ Edge / Chrome

async function chromium() {
  const exe =
    which === 'chrome'
      ? 'C:/Program Files/Google/Chrome/Application/chrome.exe'
      : 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';
  const ext = join(root, 'dist', 'chrome');
  browser = spawn(exe, [
    `--user-data-dir=${profile}`,
    '--remote-debugging-port=9333',
    `--load-extension=${ext}`,
    `--disable-extensions-except=${ext}`,
    '--auto-open-devtools-for-tabs',
    '--no-first-run',
    '--no-default-browser-check',
    '--window-size=1400,900',
    'about:blank',
  ]);
  const version = await until('the browser', async () => (await fetch('http://127.0.0.1:9333/json/version')).json());
  const cdp = await connect(version.webSocketDebuggerUrl);
  const extId = await until('the extension', async () => {
    const { targetInfos } = await cdp.send('Target.getTargets');
    // Ours, not one of the browser's built-in extensions.
    const sw = targetInfos.find((t) => t.type === 'service_worker' && /^chrome-extension:\/\/[a-p]{32}\/background\.js$/.test(t.url));
    return sw && new URL(sw.url).host;
  });
  console.log(`extension ${extId} loaded`);

  // An extension page to send commands from.
  const extPage = await openPage(cdp, `chrome-extension://${extId}/inspector.html`);
  const command = async (cmd) => {
    const r = JSON.parse(
      await extPage.eval(`new Promise((ok) => chrome.runtime.sendMessage({ smPayload: ${JSON.stringify(JSON.stringify(cmd))} }, (r) => ok(r.smResponse)))`),
    );
    if (!r.ok) throw new Error(`${cmd.cmd}: ${r.error}`);
    return r;
  };
  const state = async () => (await command({ cmd: 'GetState' })).state;

  // The page's worker thread (web/worker.js) starts under the extension's CSP and runs jobs.
  await until('smPlatform on the extension page', () => extPage.eval("typeof smPlatform === 'object'")).catch(() => {});
  const fromWorker = await extPage
    .eval(`smPlatform.work('import', 'https://a.example/, https://b.example/').then((r) => JSON.parse(r).Ok.Urls.urls.join(' '))`)
    .catch((e) => `error: ${e.message || e}`);
  check('worker thread runs jobs on extension pages', fromWorker === 'https://a.example/ https://b.example/', fromWorker);
  const setGuards = async (guards) => {
    const s = (await state()).settings;
    await command({ cmd: 'UpdateSettings', settings: { ...s, guards: { ...s.guards, ...guards } } });
  };
  const pageTargets = async () => (await cdp.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page');

  // ---- control run: no guards, so the hostile page must win
  await setGuards({ popups: false, context_menu: false, text_selection: false, anti_debugger: false, devtools_stealth: false });
  let page = await openPage(cdp, `${SITE}?notrap`);
  await sleep(3000);
  await page.click('#space');
  await page.click('#space', 'right');
  await page.key('Escape');
  await page.click('#hijack');
  await sleep(1500);
  let report = await page.report();
  check('control: pressing play opens an ad', report.playPopups.includes('opened'));
  const devtoolsDetected = report.detections.length > 0;
  check('control: the page detects the open developer tools', devtoolsDetected, report.detections.join(', '));
  check('control: the pop-under opens', report.popups.includes('opened'));
  check('control: the page blocks right-click', report.contextMenus > 0);
  check('control: the ad script runs', report.adScript);
  for (const t of await pageTargets()) if (t.url.includes('ad.html')) await cdp.send('Target.closeTarget', { targetId: t.targetId });
  await page.close();

  // ---- guarded run
  await setGuards({ popups: true, click_hijack: true, context_menu: true, text_selection: true, anti_debugger: true, devtools_stealth: true, silence_console: true });
  await command({ cmd: 'SaveBlockRule', rule: { id: 0, enabled: true, kind: 'Script', pattern: 'tools/real/ad-script.js', sites: [] } });
  await command({ cmd: 'SaveBlockRule', rule: { id: 0, enabled: true, kind: 'PopupTo', pattern: 'localhost', sites: [] } });
  await sleep(500);
  page = await openPage(cdp, SITE);
  await sleep(3000);
  await page.click('#space');
  await page.click('#space', 'right');
  await page.key('Escape');
  await sleep(1500);
  report = await page.report();
  check('the page keeps running with a debugger trap and the developer tools open', report.ticks >= 5, `${report.ticks} ticks`);
  check('the page does not detect the developer tools', report.detections.length === 0, report.detections.join(', ') || 'none');
  check('the pop-under from a click anywhere is blocked', report.popups.length > 0 && !report.popups.includes('opened'), report.popups.join(', '));
  check('right-click is not blocked', report.contextMenus === 0);
  check('the ad script is blocked', !report.adScript);
  check('text can be selected', (await page.eval(`getComputedStyle(document.body).userSelect`)) === 'text');
  check('the guard style sits in <head>', await page.eval(`!document.querySelector('html > style')`));
  check('no ad tab is left open', !(await pageTargets()).some((t) => t.url.includes('ad.html?popunder')));
  await page.click('#hijack');
  await sleep(1500);
  report = await page.report();
  check('pressing play does not open an ad', report.playPopups.length > 0 && !report.playPopups.includes('opened'), report.playPopups.join(', '));
  check('and the layer over the player is switched off', (await page.eval(`getComputedStyle(document.getElementById('hijack')).pointerEvents`)) === 'none');
  check('no ad tab opened from the player', !(await pageTargets()).some((t) => t.url.includes('ad.html?play')));

  // A real click on a link to the ad site: the tab opens, and the rule closes it.
  await page.click('#adlink');
  await sleep(2500);
  check('a tab opened to a blocked site is closed', !(await pageTargets()).some((t) => t.url.includes('ad.html?link')));
  const log = (await state()).popup_log;
  check('and logged as closed by the rule', log.some((e) => e.url.includes('ad.html?link') && e.closed_by));

  // ---- inspector
  const tabId = await extPage.eval(`chrome.tabs.query({}).then((t) => t.find((x) => (x.url || '').includes('hostile.html')).id)`);
  await command({ cmd: 'OpenInspector', tab: tabId });
  check('the inspector opens docked by default', await until('the dock', () => page.eval(`!!document.querySelector('sm-inspector-dock')`)).catch(() => false));
  await extPage.eval(`smPlatform.undockInspector(${tabId}).then(() => chrome.storage.local.set({ smInspectorDock: false })).then(() => true)`);
  await command({ cmd: 'OpenInspector', tab: tabId });
  const inspector = await until('the inspector window', async () => (await pageTargets()).find((t) => t.url.includes('inspector.html')));
  const insp = await attach(cdp, inspector.targetId);
  await sleep(4000);
  const doc = JSON.parse(await insp.eval(`smPlatform.inspect(${tabId}, { op: 'root' }).then(JSON.stringify)`));
  check('the inspector reads the page', doc.title === 'Hostile test page' && doc.node.tag === 'html');
  const found = JSON.parse(await insp.eval(`smPlatform.inspect(${tabId}, { op: 'query', selector: 'video, iframe' }).then(JSON.stringify)`));
  check('the inspector finds elements by selector', found.map((n) => n.tag).join() === 'video,iframe,video');
  const res = JSON.parse(await insp.eval(`smPlatform.collectResources(${tabId}).then(JSON.stringify)`));
  const kinds = new Set(res.map((r) => r.kind));
  check('media & frames lists the video, iframe and scripts', kinds.has('video') && kinds.has('iframe') && kinds.has('script'), [...kinds].join(', '));
  check('including those of other frames', res.some((r) => !r.top));
  check('the page cannot see the inspector', (await page.eval(`typeof window.__smInspector`)) === 'undefined');
  await inspectorExtras(insp.eval, page.eval, tabId, () => pageTargets().then((t) => t.map((x) => x.url)), () => cdp.send('Target.getTargets').then((r) => r.targetInfos.map((x) => x.url)), () => page.screenshot(join(shots, `${which}-docked.png`)), (x, y) => page.mouse({ x, y }), () => page.screenshot(join(shots, `${which}-docked-network.png`)));
  await page.screenshot(join(shots, `${which}-page.png`));

  // The inspector window itself: the tree, a node's context menu, the Ads pane.
  const winTarget = (await pageTargets()).find((t) => t.url.includes('inspector.html?tab=') && !t.url.includes('dock='));
  const win = await attach(cdp, winTarget.targetId);
  await cdp.send('Log.enable', {}, win.sessionId);
  await win.eval('location.reload(), true');
  await sleep(4000);
  const dpr = await win.eval('devicePixelRatio');
  await win.screenshot(join(shots, `${which}-inspector.png`));
  const at = (x, y) => ({ x: x / dpr, y: y / dpr }); // screenshot pixels -> CSS pixels
  await win.mouse(at(+process.env.SM_NODE_X || 160, +process.env.SM_NODE_Y || 300), 'right');
  await sleep(600);
  await win.screenshot(join(shots, `${which}-inspector-menu.png`));
  await win.key('Escape');
  await win.mouse(at(459, 98));
  await sleep(3000);
  await win.screenshot(join(shots, `${which}-inspector-ads.png`));
  // Network: record, make the page send requests, select one.
  await win.mouse(at(196, 98));
  await sleep(800);
  await win.screenshot(join(shots, `${which}-inspector-network-empty.png`));
  const pt = (name, fallback) => (process.env[name] || fallback).split(',').map(Number);
  await win.mouse(at(...pt('SM_REC', '70,165')));
  await sleep(1500);
  await page.eval(`loadData('ui'), fetch('data.json?ui-fetch', { method: 'POST', body: JSON.stringify({ a: 1 }) }), testWs.send('hello'), true`);
  await sleep(2000);
  await win.mouse(at(...pt('SM_ROW', '300,330')));
  await sleep(1000);
  await win.screenshot(join(shots, `${which}-inspector-network.png`));
  await win.mouse(at(...pt('SM_ROW', '300,330')), 'right');
  await sleep(700);
  await win.screenshot(join(shots, `${which}-inspector-network-menu.png`));
  await win.mouse(at(...pt('SM_MENU_EDIT', '486,809')));
  await sleep(700);
  await win.screenshot(join(shots, `${which}-inspector-network-rule.png`));
  await win.mouse(at(...pt('SM_SETTINGS', '1997,175')));
  await sleep(700);
  await win.screenshot(join(shots, `${which}-inspector-network-settings.png`));
  const warnings = consoleLog.filter((l) => /willReadFrequently|Only permissions specified/.test(l));
  check('the inspector logs no canvas or permission warnings', warnings.length === 0, warnings[0]?.slice(0, 160));
}

/** Live changes, copying, frames, ads, screenshots and docking, from an extension page. */
async function inspectorExtras(evalExt, evalPage, tabId, urls, frameUrls = urls, shoot = null, clickAt = null, shootPage = null) {
  const json = async (expr) => JSON.parse(await evalExt(`Promise.resolve(${expr}).then(JSON.stringify)`));
  const T = tabId;
  await evalPage(`setTimeout(() => document.body.append(Object.assign(document.createElement('p'), { textContent: 'added later' })), 1000), true`);
  const live = await json(`(async () => {
    const root = await smPlatform.inspect(${T}, { op: 'root' });
    const body = (await smPlatform.inspect(${T}, { op: 'children', id: root.node.id })).find((k) => k.tag === 'body');
    await smPlatform.inspect(${T}, { op: 'children', id: body.id });
    return await new Promise((ok) => {
      smPlatform.inspectorConnect(${T}, (j) => { const m = JSON.parse(j); if (m.changed) ok(m.changed.includes(body.id)); });
      setTimeout(() => ok('timed out'), 5000);
    });
  })()`);
  check('the inspector hears about changes to the page (live tree)', live === true, String(live));
  const [vid] = await json(`smPlatform.inspect(${T}, { op: 'query', selector: '#vid' })`);
  check('copy: CSS selector', (await json(`smPlatform.inspect(${T}, { op: 'copy', id: ${vid.id}, what: 'selector' })`)) === '#vid');
  check('copy: XPath', (await json(`smPlatform.inspect(${T}, { op: 'copy', id: ${vid.id}, what: 'xpath' })`)) === '//*[@id="vid"]');
  const [frame] = await json(`smPlatform.inspect(${T}, { op: 'query', selector: 'iframe' })`);
  check('frames in the tree carry their URL (open in a new tab)', frame.src.includes('ad.html?frame'), frame.src);
  const signals = await json(`smPlatform.collectAdSignals(${T}, ['ins.adsbygoogle'])`);
  check('the ads view finds the ad slot and the scripts', signals.some((s) => s.kind === 'slot') && signals.some((s) => s.kind === 'script'));
  check('the ads view reads the ad code written into the page', signals.some((s) => s.kind === 'inline' && /popunder/.test(s.detail)));
  const [player] = await json(`smPlatform.inspect(${T}, { op: 'query', selector: '#player' })`);
  const shot = await json(`(async () => {
    const tab = await (globalThis.browser ?? chrome).tabs.get(${T});
    try {
      const m = await smPlatform.inspect(${T}, { op: 'shotStart', id: ${player.id} });
      const blob = await smPlatform.captureRegion(tab, m, (dy) => smPlatform.inspect(${T}, { op: 'shotScroll', dy }));
      const img = await createImageBitmap(blob);
      return { width: img.width, height: img.height, dpr: m.dpr };
    } finally {
      await smPlatform.inspect(${T}, { op: 'shotEnd' });
    }
  })()`).catch((e) => ({ error: e.message }));
  check('a screenshot of an element has its size', !shot.error && Math.abs(shot.width - 320 * shot.dpr) <= 2 && Math.abs(shot.height - 180 * shot.dpr) <= 2, shot.error || `${shot.width}x${shot.height} @${shot.dpr}`);
  await evalExt(`smPlatform.dockInspector(${T}).then(() => true)`);
  await sleep(3000);
  check('the inspector docks at the bottom of the page', await evalPage(`!!document.querySelector('sm-inspector-dock')`));
  if (shoot) await shoot();
  // Its UI really runs (it saves the tab's state once it does), not just its frame.
  const started = await until('the docked inspector to start', async () => (await json(`smPlatform.sessionGet('smInspector:${T}')`)) != null, 8000).catch(() => false);
  check('the docked inspector loads and runs', (await frameUrls()).some((u) => u.includes('inspector.html?tab=') && u.includes('dock=')) && started);
  // The docked inspector's Network view (real clicks in the dock), then a reload: it carries
  // on where it was (view, recording, log) instead of starting over.
  const state = async () => JSON.parse((await json(`smPlatform.sessionGet('smInspector:${T}')`)) || 'null');
  if (clickAt) {
    const top = await evalPage(`document.querySelector('sm-inspector-dock').getBoundingClientRect().top`);
    await clickAt(130, top + 26);
    await sleep(2000);
    await evalPage(`loadData('dock'), true`);
    await sleep(1500);
    const before = await state();
    check('the docked Network view records and saves its state', before?.pane === 'network' && before.network.recording && JSON.stringify(before.network.log).includes('data.json?dock'), before ? `${before.pane}, recording ${before.network.recording}` : 'nothing saved');
  }
  await evalPage(`setTimeout(() => location.reload(), 50), true`);
  await sleep(1200);
  check('the docked inspector is back early while the page loads', await evalPage(`!!document.querySelector('sm-inspector-dock')`));
  await sleep(3800);
  check('the docked inspector comes back after the page reloads', await evalPage(`!!document.querySelector('sm-inspector-dock')`));
  if (clickAt) {
    const after = await state();
    const log = JSON.stringify(after?.network.log || []);
    check('after the reload it carries on: same view, still recording, log kept', after?.pane === 'network' && after.network.recording && log.includes('data.json?dock') && log.includes('data.json?load') && Object.keys(after.network.docUrls).length >= 2, after ? `${after.pane}, ${Object.keys(after.network.docUrls).length} pages` : 'nothing saved');
    if (shootPage) await shootPage();
  }
  await evalExt(`smPlatform.netStop(${T}).then(() => true)`);
  await evalExt(`smPlatform.undockInspector(${T}).then(() => true)`);
  await sleep(500);
  check('and undocks', await evalPage(`!document.querySelector('sm-inspector-dock')`));
  await networkChecks(evalExt, evalPage, T);
}

/** The Network view: recording, replaying into the page, reload and record, throttling. */
async function networkChecks(evalExt, evalPage, T) {
  const json = async (expr) => JSON.parse(await evalExt(`Promise.resolve(${expr}).then(JSON.stringify)`));
  const listen = () =>
    evalExt(`(globalThis.__net = [], smPlatform.inspectorInject(${T}).then(() => {
      smPlatform.inspectorConnect(${T}, (j) => { const m = JSON.parse(j); if (m.net) __net.push(...m.net); });
      return smPlatform.netStart(${T});
    }).then(() => true))`);
  const records = () => json('globalThis.__net');
  const count = (id) => evalPage(`+document.getElementById('${id}').textContent`);
  await listen();
  await evalPage(`loadData('test'), testWs.send('ping'), fetch('data.json?fetch'), true`);
  await sleep(1500);
  let net = await records();
  const xhr = net.find((r) => r.kind === 'xhr' && r.url.includes('data.json?test'));
  const done = xhr && net.find((r) => r.update && r.id === xhr.id && r.status);
  check('network: an XHR is recorded with its response', !!done && done.status === 200 && /"n"/.test(done.resBody || ''), done && `${done.status}`);
  const fetched = net.find((r) => r.kind === 'fetch' && r.url.includes('data.json?fetch'));
  check('network: a fetch is recorded', !!fetched);
  const sent = net.find((r) => r.kind === 'frame' && r.dir === 'out' && r.reqBody === 'ping');
  const echo = net.find((r) => r.kind === 'frame' && r.dir === 'in' && r.reqBody === 'echo:ping');
  check('network: WebSocket messages are recorded both ways', !!sent && !!echo);

  // Replays: the page's own handlers run again.
  const before = await count('xhrCount');
  const wsBefore = await count('wsCount');
  await evalExt(`smPlatform.netCommand(${T}, { cmd: 'replay', ids: [${xhr.id}, ${echo.id}, ${fetched.id}], gap: 100 }).then(() => true)`);
  await sleep(2000);
  net = await records();
  check('network: replaying an XHR runs the page\'s handler again', (await count('xhrCount')) === before + 1, `${before} -> ${await count('xhrCount')}`);
  check('network: replaying a received message delivers it to the page again', (await count('wsCount')) === wsBefore + 1);
  check('network: replays are recorded as replays', net.some((r) => r.replayOf === xhr.id) && net.some((r) => r.replayOf === fetched.id), JSON.stringify(net.filter((r) => r.replayOf).map((r) => [r.kind, r.replayOf])) + ' want ' + [xhr.id, fetched.id]);
  check('network: replay all reports when it is done', net.some((r) => r.replayDone === 3));

  // Reload and record: the page's first requests are there too.
  await evalExt(`smPlatform.netReloadAndRecord(${T}).then(() => true)`);
  await sleep(3500);
  await listen();
  await sleep(800);
  net = await records();
  check('network: reload and record catches the page\'s first requests', net.some((r) => r.kind === 'xhr' && r.url.includes('data.json?load')) && net.some((r) => r.kind === 'ws'));

  // Throttling (a rule of this tab), also after reloading.
  const site = await evalPage('location.hostname');
  await evalExt(`smPlatform.setThrottle([{ id: 1, enabled: true, site: '${site}', pattern: '*data.json*', method: '', delay: 1500, tab: ${T} }]).then(() => true)`);
  await sleep(1500);
  await evalPage(`loadData('slow'), true`);
  await sleep(2500);
  net = await records();
  const slow = net.find((r) => r.kind === 'xhr' && r.url.includes('data.json?slow'));
  const slowDone = slow && net.find((r) => r.update && r.id === slow.id && r.end);
  const took = slowDone ? slowDone.end - slow.start : -1;
  check('throttling holds matching requests back', took >= 1400, `${took} ms`);
  await evalPage(`setTimeout(() => location.reload(), 50), true`);
  await sleep(4500);
  const loadTook = await evalPage(`Math.round(performance.getEntriesByType('resource').find((r) => r.name.includes('data.json?load'))?.duration ?? -1)`);
  check('throttling still applies after a reload', loadTook >= 1400, `${loadTook} ms`);
  const other = await evalPage(`Math.round(performance.getEntriesByType('resource').find((r) => r.name.includes('ad-script.js'))?.duration ?? 0)`);
  check('requests not matching the rule are not held', other < 1000, `${other} ms`);

  // Another tab of the site: a rule of this tab doesn't apply there; a kept rule does.
  const api = `(globalThis.browser ?? chrome)`;
  const page2 = await evalExt(`${api}.tabs.create({ url: '${SITE}?second', active: false }).then((t) => t.id)`);
  await sleep(4000);
  const loadIn = async (tab) =>
    json(`${api}.scripting.executeScript({ target: { tabId: ${tab} }, func: () => Math.round(performance.getEntriesByType('resource').find((r) => r.name.includes('data.json?load'))?.duration ?? -1) }).then((r) => r[0].result)`);
  const otherTab = await loadIn(page2);
  check("a tab's throttle rule doesn't apply to other tabs", otherTab >= 0 && otherTab < 1000, `${otherTab} ms`);
  await evalExt(`smPlatform.setThrottle([{ id: 1, enabled: true, site: '${site}', pattern: '*data.json*', method: '', delay: 1500, tab: null }]).then(() => true)`);
  await sleep(1500);
  await evalExt(`${api}.tabs.reload(${page2}).then(() => true)`);
  await sleep(4500);
  const kept = await loadIn(page2);
  check('a rule kept for every tab applies to other tabs', kept >= 1400, `${kept} ms`);
  await evalExt(`${api}.tabs.remove(${page2}).then(() => true)`);
  await evalExt(`smPlatform.setThrottle([]).then(() => true)`);
  await sleep(1000);
}

/** Minimal DevTools Protocol client (flat sessions). */
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
    if (msg.method === 'Log.entryAdded' || msg.method === 'Runtime.consoleAPICalled') consoleLog.push(JSON.stringify(msg.params));
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

async function openPage(cdp, url) {
  const { targetId } = await cdp.send('Target.createTarget', { url });
  await cdp.send('Target.activateTarget', { targetId });
  const p = await attach(cdp, targetId);
  await until(`${url} to load`, async () => (await p.eval('document.readyState')) === 'complete', 20000);
  return p;
}

async function attach(cdp, targetId) {
  const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
  const send = (m, params) => cdp.send(m, params, sessionId);
  const p = {
    async eval(expression) {
      const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
      return r.result.value;
    },
    report: () => p.eval('JSON.parse(JSON.stringify(window.report))'),
    /** A real (trusted) mouse click in the middle of an element. */
    async click(selector, button = 'left') {
      await cdp.send('Target.activateTarget', { targetId });
      const r = await p.eval(`(() => { const el = document.querySelector('${selector}'); el.scrollIntoView({ block: 'center' }); const r = el.getBoundingClientRect(); return { x: r.x + Math.min(r.width / 2, 40), y: r.y + r.height / 2 }; })()`);
      for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
        await send('Input.dispatchMouseEvent', { type, x: r.x, y: r.y, button, buttons: type === 'mousePressed' ? (button === 'left' ? 1 : 2) : 0, clickCount: 1 });
      }
    },
    /** A real click at a point (CSS pixels). */
    async mouse({ x, y }, button = 'left') {
      await cdp.send('Target.activateTarget', { targetId });
      for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
        await send('Input.dispatchMouseEvent', { type, x, y, button, buttons: type === 'mousePressed' ? (button === 'left' ? 1 : 2) : 0, clickCount: 1 });
      }
    },
    async key(key) {
      for (const type of ['keyDown', 'keyUp']) await send('Input.dispatchKeyEvent', { type, key, code: key, windowsVirtualKeyCode: 27 });
    },
    async screenshot(file) {
      const { data } = await send('Page.captureScreenshot', { format: 'png' });
      if (!existsSync(dirname(file))) (await import('node:fs')).mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, Buffer.from(data, 'base64'));
    },
    close: () => cdp.send('Target.closeTarget', { targetId }),
    sessionId,
  };
  return p;
}

// ------------------------------------------------------------------ Firefox

async function firefox() {
  // A fixed internal UUID, so the test knows the extension's moz-extension:// origin.
  const uuid = '5e55104e-0000-4000-8000-000000000001';
  writeFileSync(
    join(profile, 'user.js'),
    [
      `user_pref("extensions.webextensions.uuids", ${JSON.stringify(JSON.stringify({ 'session-manager@local.extension': uuid }))});`,
      'user_pref("browser.shell.checkDefaultBrowser", false);',
      'user_pref("devtools.toolbox.host", "bottom");',
      'user_pref("devtools.toolbox.selectedTool", "inspector");',
      'user_pref("browser.aboutwelcome.enabled", false);',
      'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
    ].join('\n'),
  );
  browser = spawn('C:/Program Files/Mozilla Firefox/firefox.exe', [
    '-profile', profile, '-no-remote', '-new-instance', '--remote-debugging-port', '9335', '--remote-allow-system-access',
    '-devtools', '-width', '1400', '-height', '900',
  ], { env: { ...process.env, MOZ_REMOTE_ALLOW_SYSTEM_ACCESS: '1' } });
  const bidi = await until('Firefox', async () => connectBidi('ws://127.0.0.1:9335/session'), 30000);
  await bidi.send('session.new', { capabilities: {} });
  const { extension } = await bidi.send('webExtension.install', { extensionData: { type: 'path', path: join(root, 'dist', 'firefox') } });
  console.log(`extension ${extension} installed`);
  const base = `moz-extension://${uuid}`;

  // The first tab has the developer tools open (-devtools): the hostile page goes there.
  const { contexts } = await bidi.send('browsingContext.getTree', {});
  const tab = contexts[0].context;
  const ctx = (context) => ({
    async eval(expression) {
      const r = await bidi.send('script.evaluate', { expression, target: { context }, awaitPromise: true, resultOwnership: 'none', serializationOptions: { maxObjectDepth: 0 } });
      if (r.type === 'exception') throw new Error(r.exceptionDetails.text);
      return r.result.value;
    },
    async json(expression) {
      return JSON.parse(await this.eval(`Promise.resolve(${expression}).then(JSON.stringify)`));
    },
    navigate: (url) => bidi.send('browsingContext.navigate', { context, url, wait: 'complete' }),
    async click(selector, button = 0) {
      await bidi.send('browsingContext.activate', { context });
      const r = await this.json(`(() => { const el = document.querySelector('${selector}'); el.scrollIntoView({ block: 'center' }); const r = el.getBoundingClientRect(); return { x: Math.round(r.x + Math.min(r.width / 2, 40)), y: Math.round(r.y + r.height / 2) }; })()`);
      await bidi.send('input.performActions', {
        context,
        actions: [{ type: 'pointer', id: 'mouse', actions: [
          { type: 'pointerMove', x: r.x, y: r.y }, { type: 'pointerDown', button }, { type: 'pointerUp', button },
        ] }],
      });
    },
    async key(value) {
      await bidi.send('input.performActions', { context, actions: [{ type: 'key', id: 'kb', actions: [{ type: 'keyDown', value }, { type: 'keyUp', value }] }] });
    },
    async screenshot(file) {
      const { data } = await bidi.send('browsingContext.captureScreenshot', { context });
      (await import('node:fs')).mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, Buffer.from(data, 'base64'));
    },
    report() {
      return this.json('window.report');
    },
  });
  const page = ctx(tab);
  // Tabs can't be navigated to extension pages over BiDi; the browser window (chrome scope,
  // allowed by --remote-allow-system-access) opens one.
  const chromeWin = (await bidi.send('browsingContext.getTree', { 'moz:scope': 'chrome' })).contexts[0].context;
  await bidi.send('script.evaluate', {
    expression: `gBrowser.addTab(${JSON.stringify(`${base}/inspector.html`)}, { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() }) && true`,
    target: { context: chromeWin },
    awaitPromise: false,
  });
  const extTab = await until('the extension page', async () =>
    (await bidi.send('browsingContext.getTree', {})).contexts.find((c) => c.url.startsWith(base))?.context);
  const ext = ctx(extTab);
  await until('the extension page to load', async () => (await ext.eval('document.readyState')) === 'complete');
  const command = async (cmd) => {
    const r = JSON.parse(await ext.eval(`browser.runtime.sendMessage({ smPayload: ${JSON.stringify(JSON.stringify(cmd))} }).then((r) => r.smResponse)`));
    if (!r.ok) throw new Error(`${cmd.cmd}: ${r.error}`);
    return r;
  };
  const state = async () => (await command({ cmd: 'GetState' })).state;

  // The page's worker thread (web/worker.js) starts under the extension's CSP and runs jobs.
  await until('smPlatform on the extension page', () => ext.eval("typeof smPlatform === 'object'")).catch(() => {});
  const fromWorker = await ext
    .eval(`smPlatform.work('import', 'https://a.example/, https://b.example/').then((r) => JSON.parse(r).Ok.Urls.urls.join(' '))`)
    .catch((e) => `error: ${e.message || e}`);
  check('worker thread runs jobs on extension pages', fromWorker === 'https://a.example/ https://b.example/', fromWorker);
  const setGuards = async (guards) => {
    const s = (await state()).settings;
    await command({ cmd: 'UpdateSettings', settings: { ...s, guards: { ...s.guards, ...guards } } });
  };
  const tabsOpen = async () => (await bidi.send('browsingContext.getTree', {})).contexts.map((c) => c.url);
  const closeAds = async () => {
    for (const c of (await bidi.send('browsingContext.getTree', {})).contexts) {
      if (c.url.includes('ad.html')) await bidi.send('browsingContext.close', { context: c.context });
    }
  };

  // ---- control run: no guards
  await setGuards({ popups: false, context_menu: false, text_selection: false, anti_debugger: false, devtools_stealth: false });
  await bidi.send('browsingContext.activate', { context: tab });
  await page.navigate(`${SITE}?notrap`);
  await sleep(3000);
  await page.click('#space');
  await page.click('#space', 2);
  await page.key('\uE00C'); // Escape
  await page.click('#hijack');
  await sleep(1500);
  let report = await page.report();
  check('control: pressing play opens an ad', report.playPopups.includes('opened'));
  check('control: the page detects the open developer tools', report.detections.length > 0, report.detections.join(', '));
  check('control: the pop-under opens', report.popups.includes('opened'));
  check('control: the page blocks right-click', report.contextMenus > 0);
  check('control: the ad script runs', report.adScript);
  await closeAds();

  // ---- guarded run, with the script filter for debugger statements in script files
  await setGuards({
    popups: true, click_hijack: true, context_menu: true, text_selection: true, anti_debugger: true, strip_debugger_scripts: true,
    devtools_stealth: true, silence_console: true,
  });
  await command({ cmd: 'SaveBlockRule', rule: { id: 0, enabled: true, kind: 'Script', pattern: 'tools/real/ad-script.js', sites: [] } });
  await command({ cmd: 'SaveBlockRule', rule: { id: 0, enabled: true, kind: 'PopupTo', pattern: 'localhost', sites: [] } });
  await sleep(500);
  await bidi.send('browsingContext.activate', { context: tab });
  await page.navigate(`${SITE}?static`);
  await sleep(3500);
  await page.click('#space');
  await page.click('#space', 2);
  await page.key('\uE00C');
  await sleep(1500);
  report = await page.report();
  check('the page keeps running with debugger traps (also in a script file) and the developer tools open', report.ticks >= 5, `${report.ticks} ticks`);
  check('the trap in the script file runs without pausing', (await page.eval('window.__trapRuns')) >= 5);
  check('the page does not detect the developer tools', report.detections.length === 0, report.detections.join(', ') || 'none');
  check('the pop-under from a click anywhere is blocked', report.popups.length > 0 && !report.popups.includes('opened'), report.popups.join(', '));
  check('right-click is not blocked', report.contextMenus === 0);
  check('the ad script is blocked', !report.adScript);
  check('text can be selected', (await page.eval('getComputedStyle(document.body).MozUserSelect || getComputedStyle(document.body).userSelect')) === 'text');
  check('the guard style sits in <head>', await page.eval(`!document.querySelector('html > style')`));
  check('no ad tab is left open', !(await tabsOpen()).some((u) => u.includes('ad.html?popunder')));
  await page.click('#hijack');
  await sleep(1500);
  report = await page.report();
  check('pressing play does not open an ad', report.playPopups.length > 0 && !report.playPopups.includes('opened'), report.playPopups.join(', '));
  check('and the layer over the player is switched off', (await page.eval(`getComputedStyle(document.getElementById('hijack')).pointerEvents`)) === 'none');
  check('no ad tab opened from the player', !(await tabsOpen()).some((u) => u.includes('ad.html?play')));

  await page.click('#adlink');
  await sleep(2500);
  check('a tab opened to a blocked site is closed', !(await tabsOpen()).some((u) => u.includes('ad.html?link')));
  check('and logged as closed by the rule', (await state()).popup_log.some((e) => e.url.includes('ad.html?link') && e.closed_by));

  // ---- inspector
  const tabId = await ext.eval(`browser.tabs.query({}).then((t) => t.find((x) => (x.url || '').includes('hostile.html')).id)`);
  await command({ cmd: 'OpenInspector', tab: tabId });
  check('the inspector opens docked by default', await until('the dock', () => page.eval(`!!document.querySelector('sm-inspector-dock')`)).catch(() => false));
  await ext.eval(`smPlatform.undockInspector(${tabId}).then(() => browser.storage.local.set({ smInspectorDock: false })).then(() => true)`);
  await command({ cmd: 'OpenInspector', tab: tabId });
  await until('the inspector window', async () => (await tabsOpen()).some((u) => u.includes('inspector.html?tab=') && !u.includes('dock=')));
  await sleep(3000);
  const doc = await ext.json(`smPlatform.inspectorInject(${tabId}).then(() => smPlatform.inspect(${tabId}, { op: 'root' }))`);
  check('the inspector reads the page', doc.title === 'Hostile test page' && doc.node.tag === 'html');
  const found = await ext.json(`smPlatform.inspect(${tabId}, { op: 'query', selector: 'video, iframe' })`);
  check('the inspector finds elements by selector', found.map((n) => n.tag).join() === 'video,iframe,video');
  const res = await ext.json(`smPlatform.collectResources(${tabId})`);
  const kinds = new Set(res.map((r) => r.kind));
  check('media & frames lists the video, iframe and scripts', kinds.has('video') && kinds.has('iframe') && kinds.has('script'), [...kinds].join(', '));
  check('including those of other frames', res.some((r) => !r.top));
  check('the page cannot see the inspector', (await page.eval('typeof window.__smInspector')) === 'undefined');
  await inspectorExtras((e) => ext.eval(e), (e) => page.eval(e), tabId, async () => {
    const all = [];
    const walk = (cs) => cs.forEach((c) => (all.push(c.url), walk(c.children || [])));
    walk((await bidi.send('browsingContext.getTree', {})).contexts);
    return all;
  }, undefined, () => page.screenshot(join(shots, 'firefox-docked.png')), async (x, y) => {
    await bidi.send('browsingContext.activate', { context: tab });
    await bidi.send('input.performActions', {
      context: tab,
      actions: [{ type: 'pointer', id: 'mouse', actions: [
        { type: 'pointerMove', x: Math.round(x), y: Math.round(y) }, { type: 'pointerDown', button: 0 }, { type: 'pointerUp', button: 0 },
      ] }],
    });
  }, () => page.screenshot(join(shots, 'firefox-docked-network.png')));
  const inspectorCtx = (await bidi.send('browsingContext.getTree', {})).contexts.find((c) => c.url.includes('inspector.html?tab='));
  await ctx(inspectorCtx.context).screenshot(join(shots, 'firefox-inspector.png'));
  await page.screenshot(join(shots, 'firefox-page.png'));

  // ---- capture (the Firefox ways: mozCaptureStream, the page's own fetch); Edge runs tools/capture-test.mjs
  await page.navigate('http://127.0.0.1:8123/tools/real/media.html');
  await sleep(1500);
  const media = await ext.json(`smPlatform.collectCaptureMedia(${tabId})`);
  const top = media.media.find((m) => m.top);
  const inner = media.media.find((m) => !m.top);
  check('capture lists the videos of the page and its frame', !!top && !!inner && inner.frameId > 0, media.media.map((m) => m.frameUrl).join(', '));
  await ext.eval(`smPlatform.openCapture({ tab: ${tabId}, frame: 0, el: ${top.id}, title: 'Clip' }).then(() => true)`);
  const capCtx = await until('the capture window', async () => {
    const all = [];
    const walk = (cs) => cs.forEach((c) => (all.push(c), walk(c.children || [])));
    walk((await bidi.send('browsingContext.getTree', {})).contexts);
    return all.find((c) => c.url.includes('capture.html'))?.context;
  });
  const cap = ctx(capCtx);
  await until('the capture window to start', () => cap.eval(`!document.getElementById('sm-loading') && typeof smCapture === 'object'`), 15000);
  const call = (name, args = {}) => cap.json(`smCapture.call(${JSON.stringify(name)}, ${JSON.stringify(JSON.stringify(args))}).then(JSON.parse)`);
  const rec = { tab: tabId, frame: 0, el: top.id, tracks: 'av', format: 'webm', videoBps: 0, stopAtEnd: true };
  const info = await call('recordElement', rec).catch((e) => ({ error: e.message }));
  check('Firefox records a video element (mozCaptureStream)', info.video === 1 && /webm/.test(info.mime || ''), JSON.stringify(info));
  await sleep(2500);
  await call('stop');
  const head = await until('the recording, with its duration', async () => {
    const b = await cap.json(`smCapture.head(1, 4096).then((b) => Array.from(b))`);
    for (let i = 0; i + 2 < b.length; i++) if (b[i] === 0x44 && b[i + 1] === 0x89 && b[i + 2] === 0x88) return b;
    return null;
  }).catch(() => null);
  check('the recording is a WebM file with its duration', !!head && head[0] === 0x1a && head[1] === 0x45);
  const innerRec = await call('recordElement', { ...rec, frame: inner.frameId, el: inner.id, tracks: 'v' }).catch((e) => ({ error: e.message }));
  check('and the video in the frame', innerRec.video === 1, JSON.stringify(innerRec));
  await sleep(1000);
  await call('stop');
  const stream = 'http://127.0.0.1:8123/tools/real/stream/';
  const parts = ['v720-init.mp4', 'v720-1.m4s', 'v720-2.m4s', 'v720-3.m4s'].map((n) => ({ url: stream + n }));
  await call('download', { job: 1, via: { tab: tabId, frame: 0 }, parts, label: '720p', ext: 'mp4' });
  const text = await cap.eval(`smCapture.head(3, 1e6).then((b) => new TextDecoder().decode(b))`).catch((e) => String(e));
  check('a stream is fetched through the page and joined in order', text === '[v720 init][v720 seg 1][v720 seg 2][v720 seg 3]', text);
  await cap.screenshot(join(shots, 'firefox-capture.png'));
}

/** Minimal WebDriver BiDi client. */
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
