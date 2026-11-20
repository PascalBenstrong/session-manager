// Starts a real browser with the built extension in a throwaway profile, for the tests that
// run against real browser APIs (tools/idb-real-test.mjs, tools/perf-real-test.mjs).
//
//   which: 'edge' | 'chrome' | 'firefox'
//   SM_HEADLESS=1   run headless (CI)
//   SM_CHROME / SM_FIREFOX   the browser's executable (defaults per platform)
//
// Returns { open(path), reload(), close() }: `open` opens an extension page and returns an
// `evaluate(expression)` for it (promises are awaited, values returned as JSON-compatible);
// `reload` restarts the extension (its data is kept) and returns a fresh page.
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir, platform } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const until = async (what, f, ms = 20000) => {
  let last;
  for (const end = Date.now() + ms; Date.now() < end; await sleep(200)) {
    try {
      const v = await f();
      if (v) return v;
    } catch (e) {
      last = e;
    }
  }
  throw new Error(`timed out waiting for ${what}${last ? ` (${last.message})` : ''}`);
};
const headless = process.env.SM_HEADLESS === '1';
const windows = platform() === 'win32';

function findExecutable(which) {
  const env = which === 'firefox' ? process.env.SM_FIREFOX : process.env.SM_CHROME;
  if (env) return env;
  const candidates = {
    edge: windows ? ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'] : ['microsoft-edge', 'microsoft-edge-stable'],
    chrome: windows ? ['C:/Program Files/Google/Chrome/Application/chrome.exe'] : ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser'],
    firefox: windows ? ['C:/Program Files/Mozilla Firefox/firefox.exe'] : ['firefox'],
  }[which];
  for (const c of candidates) {
    if (c.includes('/') ? existsSync(c) : whichCmd(c)) return c;
  }
  throw new Error(`${which} not found (set ${which === 'firefox' ? 'SM_FIREFOX' : 'SM_CHROME'})`);
}

function whichCmd(name) {
  try {
    execFileSync(windows ? 'where' : 'which', [name], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

export async function launch(which, { port = which === 'firefox' ? 9336 : 9334 } = {}) {
  const profile = mkdtempSync(join(tmpdir(), 'sm-real-'));
  const exe = findExecutable(which);
  let child;
  const close = async () => {
    if (child?.pid) {
      if (windows) spawn('taskkill', ['/PID', String(child.pid), '/T', '/F']);
      else child.kill('SIGKILL');
    }
    await sleep(1500);
    try {
      rmSync(profile, { recursive: true, force: true });
    } catch {}
  };
  try {
    const b = which === 'firefox' ? await firefox(exe, profile, port, (c) => (child = c)) : await chromium(exe, profile, port, (c) => (child = c));
    return { ...b, close };
  } catch (e) {
    await close();
    throw e;
  }
}

// ------------------------------------------------------------------ Edge / Chrome

async function chromium(exe, profile, port, started) {
  const ext = join(root, 'dist', 'chrome');
  const args = [
    `--user-data-dir=${profile}`,
    `--remote-debugging-port=${port}`,
    `--load-extension=${ext}`,
    `--disable-extensions-except=${ext}`,
    // Chrome 137+ ignores --load-extension unless this is off.
    '--disable-features=DisableLoadExtensionCommandLineSwitch',
    '--no-first-run',
    '--no-default-browser-check',
    '--window-size=1200,800',
  ];
  if (headless) args.push('--headless=new', '--no-sandbox', '--disable-gpu');
  started(spawn(exe, [...args, 'about:blank']));
  const version = await until('the browser', async () => (await fetch(`http://127.0.0.1:${port}/json/version`)).json(), 30000);
  const cdp = await connectCdp(version.webSocketDebuggerUrl);
  // Browsers ship extensions with a background.js too: ours is the one with our name.
  const extId = await until('the extension', async () => {
    const { targetInfos } = await cdp.send('Target.getTargets');
    for (const t of targetInfos.filter((t) => t.type === 'service_worker' && /^chrome-extension:\/\/[a-p]{32}\/background\.js$/.test(t.url))) {
      const { sessionId } = await cdp.send('Target.attachToTarget', { targetId: t.targetId, flatten: true });
      const r = await cdp.send('Runtime.evaluate', { expression: 'chrome.runtime.getManifest().name', returnByValue: true }, sessionId).catch(() => null);
      await cdp.send('Target.detachFromTarget', { sessionId }).catch(() => {});
      if (r?.result?.value === 'Session Manager') return new URL(t.url).host;
    }
    return null;
  }, 30000);
  const open = async (path = 'inspector.html') => {
    const { targetId } = await cdp.send('Target.createTarget', { url: `chrome-extension://${extId}/${path}` });
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    const evaluate = async (expression) => {
      const r = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId);
      if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
      return r.result.value;
    };
    await until('smPlatform on the page', () => evaluate("typeof smPlatform === 'object'"));
    return evaluate;
  };
  let page = await open();
  const reload = async () => {
    await page('chrome.runtime.reload()').catch(() => {});
    await sleep(1500);
    await until('the extension after reloading', async () => {
      const { targetInfos } = await cdp.send('Target.getTargets');
      return targetInfos.some((t) => t.type === 'service_worker' && t.url.includes(extId));
    });
    page = await open();
    return page;
  };
  return { name: headless ? 'chrome (headless)' : 'chromium', api: 'chrome', page: () => page, open, reload };
}

async function connectCdp(url) {
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

async function firefox(exe, profile, port, started) {
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
  const args = ['-profile', profile, '-no-remote', '-new-instance', '--remote-debugging-port', String(port), '--remote-allow-system-access', '-width', '1200', '-height', '800'];
  if (headless) args.push('-headless');
  started(spawn(exe, args, { env: { ...process.env, MOZ_REMOTE_ALLOW_SYSTEM_ACCESS: '1' } }));
  const bidi = await until('Firefox', async () => connectBidi(`ws://127.0.0.1:${port}/session`), 30000);
  await bidi.send('session.new', { capabilities: {} });
  const install = () => bidi.send('webExtension.install', { extensionData: { type: 'path', path: join(root, 'dist', 'firefox') } });
  let { extension } = await install();
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
      return fromBidi(r.result);
    };
    await until('smPlatform on the page', () => evaluate("typeof smPlatform === 'object'"));
    return evaluate;
  };
  let page = await open();
  const reload = async () => {
    await bidi.send('webExtension.uninstall', { extension }).catch(() => {});
    ({ extension } = await install());
    await sleep(2500);
    page = await open();
    return page;
  };
  return { name: headless ? 'firefox (headless)' : 'firefox', api: 'browser', page: () => page, open, reload };
}

/** A BiDi remote value as a plain value (strings, numbers, booleans; JSON for the rest). */
function fromBidi(v) {
  if (!v) return undefined;
  if ('value' in v && (typeof v.value !== 'object' || v.value === null)) return v.value;
  return v.value;
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
