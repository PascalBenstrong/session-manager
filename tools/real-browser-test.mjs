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
  await sleep(1500);
  let report = await page.report();
  const devtoolsDetected = report.detections.length > 0;
  check('control: the page detects the open developer tools', devtoolsDetected, report.detections.join(', '));
  check('control: the pop-under opens', report.popups.includes('opened'));
  check('control: the page blocks right-click', report.contextMenus > 0);
  check('control: the ad script runs', report.adScript);
  for (const t of await pageTargets()) if (t.url.includes('ad.html')) await cdp.send('Target.closeTarget', { targetId: t.targetId });
  await page.close();

  // ---- guarded run
  await setGuards({ popups: true, context_menu: true, text_selection: true, anti_debugger: true, devtools_stealth: true, silence_console: true });
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

  // A real click on a link to the ad site: the tab opens, and the rule closes it.
  await page.click('#adlink');
  await sleep(2500);
  check('a tab opened to a blocked site is closed', !(await pageTargets()).some((t) => t.url.includes('ad.html?link')));
  const log = (await state()).popup_log;
  check('and logged as closed by the rule', log.some((e) => e.url.includes('ad.html?link') && e.closed_by));

  // ---- inspector
  const tabId = await extPage.eval(`chrome.tabs.query({}).then((t) => t.find((x) => (x.url || '').includes('hostile.html')).id)`);
  await command({ cmd: 'OpenInspector', tab: tabId });
  const inspector = await until('the inspector window', async () => (await pageTargets()).find((t) => t.url.includes('inspector.html')));
  const insp = await attach(cdp, inspector.targetId);
  await sleep(4000);
  const doc = JSON.parse(await insp.eval(`smPlatform.inspect(${tabId}, { op: 'root' }).then(JSON.stringify)`));
  check('the inspector reads the page', doc.title === 'Hostile test page' && doc.node.tag === 'html');
  const found = JSON.parse(await insp.eval(`smPlatform.inspect(${tabId}, { op: 'query', selector: 'video, iframe' }).then(JSON.stringify)`));
  check('the inspector finds elements by selector', found.map((n) => n.tag).join() === 'video,iframe');
  const res = JSON.parse(await insp.eval(`smPlatform.collectResources(${tabId}).then(JSON.stringify)`));
  const kinds = new Set(res.map((r) => r.kind));
  check('media & frames lists the video, iframe and scripts', kinds.has('video') && kinds.has('iframe') && kinds.has('script'), [...kinds].join(', '));
  check('including those of other frames', res.some((r) => !r.top));
  check('the page cannot see the inspector', (await page.eval(`typeof window.__smInspector`)) === 'undefined');
  await insp.screenshot(join(shots, `${which}-inspector.png`));
  await page.screenshot(join(shots, `${which}-page.png`));
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
      const r = await p.eval(`(() => { const r = document.querySelector('${selector}').getBoundingClientRect(); return { x: r.x + Math.min(r.width / 2, 40), y: r.y + r.height / 2 }; })()`);
      for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
        await send('Input.dispatchMouseEvent', { type, x: r.x, y: r.y, button, buttons: type === 'mousePressed' ? (button === 'left' ? 1 : 2) : 0, clickCount: 1 });
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
      const r = await this.json(`(() => { const r = document.querySelector('${selector}').getBoundingClientRect(); return { x: Math.round(r.x + Math.min(r.width / 2, 40)), y: Math.round(r.y + r.height / 2) }; })()`);
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
  await sleep(1500);
  let report = await page.report();
  check('control: the page detects the open developer tools', report.detections.length > 0, report.detections.join(', '));
  check('control: the pop-under opens', report.popups.includes('opened'));
  check('control: the page blocks right-click', report.contextMenus > 0);
  check('control: the ad script runs', report.adScript);
  await closeAds();

  // ---- guarded run, with the script filter for debugger statements in script files
  await setGuards({
    popups: true, context_menu: true, text_selection: true, anti_debugger: true, strip_debugger_scripts: true,
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

  await page.click('#adlink');
  await sleep(2500);
  check('a tab opened to a blocked site is closed', !(await tabsOpen()).some((u) => u.includes('ad.html?link')));
  check('and logged as closed by the rule', (await state()).popup_log.some((e) => e.url.includes('ad.html?link') && e.closed_by));

  // ---- inspector
  const tabId = await ext.eval(`browser.tabs.query({}).then((t) => t.find((x) => (x.url || '').includes('hostile.html')).id)`);
  await command({ cmd: 'OpenInspector', tab: tabId });
  await until('the inspector window', async () => (await tabsOpen()).some((u) => u.includes('inspector.html?tab=')));
  await sleep(3000);
  const doc = await ext.json(`smPlatform.inspectorInject(${tabId}).then(() => smPlatform.inspect(${tabId}, { op: 'root' }))`);
  check('the inspector reads the page', doc.title === 'Hostile test page' && doc.node.tag === 'html');
  const found = await ext.json(`smPlatform.inspect(${tabId}, { op: 'query', selector: 'video, iframe' })`);
  check('the inspector finds elements by selector', found.map((n) => n.tag).join() === 'video,iframe');
  const res = await ext.json(`smPlatform.collectResources(${tabId})`);
  const kinds = new Set(res.map((r) => r.kind));
  check('media & frames lists the video, iframe and scripts', kinds.has('video') && kinds.has('iframe') && kinds.has('script'), [...kinds].join(', '));
  check('including those of other frames', res.some((r) => !r.top));
  check('the page cannot see the inspector', (await page.eval('typeof window.__smInspector')) === 'undefined');
  const inspectorCtx = (await bidi.send('browsingContext.getTree', {})).contexts.find((c) => c.url.includes('inspector.html?tab='));
  await ctx(inspectorCtx.context).screenshot(join(shots, 'firefox-inspector.png'));
  await page.screenshot(join(shots, 'firefox-page.png'));
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
