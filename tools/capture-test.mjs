// Tests media capture (the inspector's Capture view and the capture window) in a real Edge:
// recording a page's video element (top frame and an iframe), recording what the tab shows
// cut to the video, saving an HLS stream (through the window's own controls), saving and
// uploading the results.
//
//   ./build.sh chrome && node tools/serve.mjs &   (the test site is tools/real/)
//   node tools/capture-test.mjs
//
// It uses a throwaway profile, so your own browser profile is never touched.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, readdirSync, readFileSync, statSync, writeFileSync, mkdirSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SITE = 'http://127.0.0.1:8123/tools/real/media.html';
const STREAM = 'http://127.0.0.1:8123/tools/real/stream/';
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

// Uploads land here.
const received = [];
const uploads = createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    received.push({ method: req.method, url: req.url, headers: req.headers, body: Buffer.concat(chunks) });
    res.writeHead(201, { location: '/files/1' }).end('stored');
  });
}).listen(8124);

const profile = mkdtempSync(join(tmpdir(), 'sm-capture-'));
const downloads = join(profile, 'downloads');
mkdirSync(downloads);
const shots = join(tmpdir(), 'sm-real-shots');
mkdirSync(shots, { recursive: true });
let browser;
try {
  await run();
} catch (e) {
  check('the run finished', false, e.stack || e.message);
} finally {
  if (browser?.pid) spawn('taskkill', ['/PID', String(browser.pid), '/T', '/F']);
  uploads.close();
  await sleep(1000);
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {}
}
console.log(`\n${passed}/${results.length} checks passed; screenshots in ${shots}`);
process.exit(results.length === passed ? 0 : 1);

async function run() {
  const ext = join(root, 'dist', 'chrome');
  browser = spawn('C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', [
    `--user-data-dir=${profile}`,
    '--remote-debugging-port=9334',
    `--load-extension=${ext}`,
    `--disable-extensions-except=${ext}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--window-size=1300,900',
    '--autoplay-policy=no-user-gesture-required',
    // getDisplayMedia picks the test page's tab. (Not --use-fake-ui-for-media-stream: it
    // makes the window share itself.)
    '--auto-select-tab-capture-source-by-title=Media test page',
    'about:blank',
  ]);
  const version = await until('the browser', async () => (await fetch('http://127.0.0.1:9334/json/version')).json());
  const cdp = await connect(version.webSocketDebuggerUrl);
  await cdp.send('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: downloads });
  const extId = await until('the extension', async () => {
    const { targetInfos } = await cdp.send('Target.getTargets');
    const sw = targetInfos.find((t) => t.type === 'service_worker' && /^chrome-extension:\/\/[a-p]{32}\/background\.js$/.test(t.url));
    return sw && new URL(sw.url).host;
  });
  const extPage = await openPage(cdp, `chrome-extension://${extId}/inspector.html`);
  const page = await openPage(cdp, SITE);
  await sleep(1500);
  const tab = await extPage.eval(`chrome.tabs.query({}).then((t) => t.find((x) => x.url.includes('media.html')).id)`);
  const targets = async () => (await cdp.send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page');

  // ---- what the Capture view lists
  const list = await extPage.eval(`smPlatform.collectCaptureMedia(${tab})`);
  const top = list.media.find((m) => m.top);
  const inner = list.media.find((m) => !m.top);
  check('the page’s video is listed', top && top.tag === 'video' && top.source === 'live' && !top.paused, JSON.stringify(top));
  check('and the video in its frame', inner && inner.frameId > 0 && inner.frameUrl.includes('media-frame.html'), inner && inner.frameUrl);
  check('stream manifests the page loaded are listed', ['master.m3u8', 'manifest.mpd'].every((n) => list.urls.some((u) => u.url.endsWith(n))), list.urls.map((u) => u.url).join(', '));
  check('the page cannot see the capture script', (await page.eval('typeof window.__smCapture')) === 'undefined');

  // The inspector's Capture view (opened on it), for a look.
  await extPage.eval(`smPlatform.sessionSet('smInspector:${tab}', JSON.stringify({ pane: 'capture' })).then(() => true)`);
  const insp = await openPage(cdp, `chrome-extension://${extId}/inspector.html?tab=${tab}`);
  await sleep(2500);
  await insp.screenshot(join(shots, 'capture-pane.png'));

  // ---- a capture window recording the element's own stream
  const openWindow = async (params) => {
    const before = new Set((await targets()).map((t) => t.targetId));
    await extPage.eval(`smPlatform.openCapture(${JSON.stringify(params)}).then(() => true)`);
    const t = await until('the capture window', async () => (await targets()).find((x) => x.url.includes('capture.html') && !before.has(x.targetId)));
    const w = await attach(cdp, t.targetId);
    await until('the capture window to start', () => w.eval(`!document.getElementById('sm-loading') && typeof smCapture === 'object'`), 15000);
    return w;
  };
  // (Not titled like the page: the browser picks the tab to share by its title.)
  const win = await openWindow({ tab, frame: 0, el: top.id, method: 'element', tracks: 'av', title: 'Clip', label: 'video 320×180' });
  const call = (w, name, args = {}) => w.eval(`smCapture.call(${JSON.stringify(name)}, ${JSON.stringify(JSON.stringify(args))}).then(JSON.parse)`);
  const rec = { tab, frame: 0, el: top.id, tracks: 'av', format: 'webm', videoBps: 0, fromStart: false, stopAtEnd: true, listenHere: false, label: 'recording' };
  const info = await call(win, 'recordElement', rec);
  check('recording the element starts, with picture and sound', info.video === 1 && info.audio === 1 && /webm/.test(info.mime), JSON.stringify(info));
  await sleep(3000);
  check('it plays in the window as it records', (await win.eval(`document.getElementById('sm-preview').readyState`)) >= 2);
  await win.screenshot(join(shots, 'capture-window-recording.png'));
  await call(win, 'stop');
  // The window writes the duration into the WebM file (src/webm.rs) once it's done.
  const head = await until('the recording, with its duration', async () => {
    const b = await win.eval(`smCapture.head(1, 4096).then((b) => Array.from(b))`);
    return hasDuration(b) && b;
  });
  check('the recording is a WebM file with its duration written in', head[0] === 0x1a && head[1] === 0x45 && head[2] === 0xdf && head[3] === 0xa3);
  const size1 = await win.eval(`smCapture.head(1, 1e9).then((b) => b.length)`);
  check('it has a few seconds of video', size1 > 5000, `${size1} bytes`);

  // ---- the element in the frame
  const recInner = await call(win, 'recordElement', { ...rec, frame: inner.frameId, el: inner.id, tracks: 'v' });
  check('recording the video in the frame starts (picture only)', recInner.video === 1 && recInner.audio === 0, JSON.stringify(recInner));
  await sleep(1500);
  await call(win, 'stop');
  await until('the frame recording', () => win.eval(`smCapture.head(2, 4).then((b) => b.length === 4)`));
  check('the frame recording is done', true);

  // ---- what the browser shows, cut to the element
  await win.evalGesture(`smCapture.gesture('screen')`);
  const shared = await call(win, 'screenPicked');
  check('a tab is shared', shared.surface === 'browser', JSON.stringify(shared));
  // The tab is watched (a background tab doesn't paint its video).
  await page.front();
  await sleep(500);
  const scr = await call(win, 'recordScreen', { ...rec, tracks: 'v', crop: true });
  await sleep(2000);
  // The picture is the page's video (green, with a white counter), not something next to it.
  // (A frame read from the recorded track: the window may be in the background, where its
  // player isn't drawn.)
  const px = await win.eval(`(async () => {
    const track = document.getElementById('sm-preview').srcObject.getVideoTracks()[0].clone();
    const reader = new MediaStreamTrackProcessor({ track }).readable.getReader();
    let frame;
    for (let i = 0; i < 10; i++) {
      frame?.close();
      frame = (await reader.read()).value;
    }
    const c = new OffscreenCanvas(frame.displayWidth, frame.displayHeight);
    const g = c.getContext('2d');
    g.drawImage(frame, 0, 0);
    frame.close();
    reader.cancel();
    track.stop();
    let green = 0;
    const d = g.getImageData(0, 0, c.width, c.height).data;
    for (let i = 0; i < d.length; i += 4) if (Math.abs(d[i] - 0x22) < 40 && Math.abs(d[i + 1] - 0xaa) < 40 && Math.abs(d[i + 2] - 0x77) < 40) green++;
    const mid = (Math.floor(c.height / 2) * c.width + Math.floor(c.width / 2)) * 4;
    const b = await c.convertToBlob({ type: 'image/png' });
    const u8 = new Uint8Array(await b.arrayBuffer());
    let bin = '';
    for (let i = 0; i < u8.length; i += 32768) bin += String.fromCharCode.apply(null, u8.subarray(i, i + 32768));
    return { green: green / (d.length / 4), size: [c.width, c.height], mid: [d[mid], d[mid + 1], d[mid + 2]], png: btoa(bin) };
  })()`);
  writeFileSync(join(shots, 'capture-screen-frame.png'), Buffer.from(px.png, 'base64'));
  delete px.png;
  const [fw, fh] = px.size;
  check('the screen recording is cut to the video', fw >= 300 && Math.abs(fw / fh - 16 / 9) < 0.03, `${fw}×${fh}, note: ${scr.note || 'none'}`);
  check('and shows the video itself', px.green > 0.6, `${Math.round(px.green * 100)}% of it is the video's green; ${JSON.stringify(px)}`);
  await win.screenshot(join(shots, 'capture-window-screen.png'));
  await call(win, 'stop');
  await until('the screen recording', () => win.eval(`smCapture.head(3, 4).then((b) => b.length === 4)`));
  check('the screen recording is done', true);

  // ---- sinks: save and upload
  await call(win, 'save', { take: 1, name: 'test recording.webm' });
  const saved = await until('the saved file', async () => {
    const f = readdirSync(downloads).find((n) => n.endsWith('.webm'));
    return f && statSync(join(downloads, f)).size === size1 && f;
  }).catch(() => null);
  check('saving writes the file', !!saved, saved || readdirSync(downloads).join(', '));
  const up = await call(win, 'upload', { take: 1, url: 'http://127.0.0.1:8124/upload', method: 'PUT', field: '', headers: [['Authorization', 'Bearer t0ken']], name: 'clip.webm' });
  const got = received.at(-1);
  check('uploading sends it (PUT, headers, body)', up.status === 201 && got?.method === 'PUT' && got.headers.authorization === 'Bearer t0ken' && got.body.length === size1, `${up.status} ${got?.body.length}`);
  const form = await call(win, 'upload', { take: 2, url: 'http://127.0.0.1:8124/form', method: 'POST', field: 'file', headers: [], name: 'frame.webm' });
  check('or as a form', form.status === 201 && /multipart\/form-data/.test(received.at(-1).headers['content-type']) && received.at(-1).body.includes('filename="frame.webm"'));

  // ---- saving an HLS stream (through the window's own controls)
  const hls = await openWindow({ url: `${STREAM}master.m3u8`, tab, frame: 0, tracks: 'av', title: 'Media test page' });
  await sleep(1500);
  await hls.screenshot(join(shots, 'capture-window-hls.png'));
  const button = await clickDownload(hls);
  check('the HLS window offers to download', !!button, JSON.stringify(button));
  // The best quality (720p) and the default sound, as two files.
  const want = ['[v720 init][v720 seg 1][v720 seg 2][v720 seg 3]', '[audio init][audio seg 1][audio seg 2][audio seg 3]'];
  let last = null;
  const got2 = await until('the stream download', async () => {
    const texts = await hls.eval(`Promise.all([1, 2].map((id) => smCapture.head(id, 1e6).then((b) => new TextDecoder().decode(b)).catch(() => '')))`);
    last = texts;
    return texts.every(Boolean) && texts;
  }, 20000).catch(() => last);
  if (JSON.stringify(got2) !== JSON.stringify(want)) console.log('takes', await hls.eval(`Promise.all([1, 2, 3, 4].map((id) => smCapture.head(id, 1e6).then((b) => new TextDecoder().decode(b)).catch((e) => String(e))))`));
  check('the best quality and the sound are saved as two files, in order', JSON.stringify(got2) === JSON.stringify(want), JSON.stringify(got2));
  await hls.screenshot(join(shots, 'capture-window-hls-done.png'));

  // ---- DASH (engine only: the Rust side is unit-tested)
  const dash = await openWindow({ url: `${STREAM}manifest.mpd`, tab, frame: 0, tracks: 'v', title: 'Media test page' });
  await sleep(1500);
  await dash.screenshot(join(shots, 'capture-window-dash.png'));
  await clickDownload(dash);
  const dashText = await until('the DASH download', () => dash.eval(`smCapture.head(1, 1e6).then((b) => new TextDecoder().decode(b))`)).catch(() => '');
  check('a DASH stream’s video is saved (video only)', dashText === '[v720 init][v720 seg 1][v720 seg 2][v720 seg 3]', dashText);

  // ---- the Capture view lists the windows
  await sleep(1500);
  const summaries = await extPage.eval(`smPlatform.sessionList('smCapture:')`);
  const states = summaries.map(([, v]) => JSON.parse(v)).map((x) => `${x.state}:${x.takes}`).sort();
  check('capture windows publish what they do, also from the background', JSON.stringify(states) === JSON.stringify(['Ready:1', 'Ready:2', 'Ready:3']), states.join(', '));
  await insp.screenshot(join(shots, 'capture-pane-windows.png'));
}

/** Clicks a capture window's Download button until the download starts (take 1 exists). */
async function clickDownload(w) {
  for (let i = 0; i < 5; i++) {
    const b = await accentButton(w);
    if (b) await w.mouse(b);
    const started = await until('a download', () => w.eval(`smCapture.head(1, 1).then(() => true, () => false)`), 3000).catch(() => false);
    if (started) return b;
  }
  return null;
}

/** Does a WebM head have a Duration element (0x4489, 8-byte float)? */
function hasDuration(b) {
  for (let i = 0; i + 3 < b.length; i++) if (b[i] === 0x44 && b[i + 1] === 0x89 && b[i + 2] === 0x88) return true;
  return false;
}

/**
 * Where the lowest accent-colored button of a window is, from a screenshot: rows with the
 * color form bands (the header icon, the selected segment, buttons); the last band wins.
 */
async function accentButton(w) {
  const png = decodePng(Buffer.from(await w.screenshotData(), 'base64'));
  const accents = [[0x7c, 0x6c, 0xf6], [0x5b, 0x4b, 0xe0]];
  const bands = [];
  for (let y = 0; y < png.height; y++) {
    let minX = Infinity;
    let maxX = -1;
    for (let x = 0; x < png.width; x++) {
      const i = (y * png.width + x) * 4;
      const [r, g, b] = [png.data[i], png.data[i + 1], png.data[i + 2]];
      if (accents.some((a) => Math.abs(a[0] - r) < 24 && Math.abs(a[1] - g) < 24 && Math.abs(a[2] - b) < 24)) {
        minX = Math.min(minX, x);
        maxX = Math.max(maxX, x);
      }
    }
    if (maxX - minX < 20) continue; // not a button (text, icons)
    const last = bands.at(-1);
    if (last && last.y1 >= y - 1) Object.assign(last, { y1: y, x0: Math.min(last.x0, minX), x1: Math.max(last.x1, maxX) });
    else bands.push({ y0: y, y1: y, x0: minX, x1: maxX });
  }
  const b = bands.filter((x) => x.y1 - x.y0 >= 8).at(-1);
  if (!b) return null;
  const dpr = await w.eval('devicePixelRatio');
  return { x: (b.x0 + b.x1) / 2 / dpr, y: (b.y0 + b.y1) / 2 / dpr };
}

/** A minimal PNG decoder (8-bit RGB / RGBA, as Chromium writes them). */
function decodePng(buf) {
  let at = 8;
  let width = 0, height = 0, type = 0;
  const idat = [];
  while (at < buf.length) {
    const len = buf.readUInt32BE(at);
    const kind = buf.toString('ascii', at + 4, at + 8);
    const data = buf.subarray(at + 8, at + 8 + len);
    if (kind === 'IHDR') {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      type = data[9];
    } else if (kind === 'IDAT') idat.push(data);
    at += 12 + len;
  }
  const bpp = type === 6 ? 4 : 3;
  const raw = inflateSync(Buffer.concat(idat));
  const stride = width * bpp;
  const out = Buffer.alloc(width * height * 4);
  const prev = Buffer.alloc(stride);
  const row = Buffer.alloc(stride);
  for (let y = 0; y < height; y++) {
    const f = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? row[x - bpp] : 0;
      const b = prev[x];
      const c = x >= bpp ? prev[x - bpp] : 0;
      let v = line[x];
      if (f === 1) v += a;
      else if (f === 2) v += b;
      else if (f === 3) v += (a + b) >> 1;
      else if (f === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      row[x] = v & 255;
    }
    for (let x = 0; x < width; x++) {
      out[(y * width + x) * 4] = row[x * bpp];
      out[(y * width + x) * 4 + 1] = row[x * bpp + 1];
      out[(y * width + x) * 4 + 2] = row[x * bpp + 2];
      out[(y * width + x) * 4 + 3] = bpp === 4 ? row[x * bpp + 3] : 255;
    }
    row.copy(prev);
  }
  return { width, height, data: out };
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
  const evaluate = async (expression, userGesture) => {
    const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  const p = {
    eval: (expression) => evaluate(expression, false),
    /** With a user gesture (APIs like getDisplayMedia need one). */
    evalGesture: (expression) => evaluate(`(${expression}, true)`, true),
    async mouse({ x, y }) {
      await cdp.send('Target.activateTarget', { targetId });
      for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
        await send('Input.dispatchMouseEvent', { type, x, y, button: 'left', buttons: type === 'mousePressed' ? 1 : 0, clickCount: 1 });
        await sleep(60);
      }
    },
    front: () => cdp.send('Target.activateTarget', { targetId }),
    screenshotData: async () => (await send('Page.captureScreenshot', { format: 'png' })).data,
    async screenshot(file) {
      writeFileSync(file, Buffer.from(await p.screenshotData(), 'base64'));
    },
  };
  return p;
}
