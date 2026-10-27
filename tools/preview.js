import { createMockBrowser } from './mock-browser.js';

const browserName = new URLSearchParams(location.search).get('browser') || 'firefox';
const popup = new URLSearchParams(location.search).has('popup');
const sidebar = new URLSearchParams(location.search).has('sidebar');
document.body.dataset.popup = String(popup);

const mock = createMockBrowser();
globalThis.browser = mock.api;
globalThis.chrome = mock.api;

const base = `../dist/${browserName}`;
await import(`${base}/platform.js`);
smPlatform.installBackgroundListeners();

const seed = [
  ['https://github.com/emilk/egui', 'emilk/egui: an easy-to-use immediate mode GUI in Rust'],
  ['https://github.com/rustwasm/wasm-bindgen', 'rustwasm/wasm-bindgen: Facilitating high-level interactions'],
  ['https://docs.rs/eframe/latest/eframe/', 'eframe - Rust'],
  ['https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions', 'Browser extensions - MDN'],
  ['https://news.ycombinator.com/', 'Hacker News'],
  ['https://www.rust-lang.org/learn', 'Learn Rust - Rust Programming Language'],
  ['https://docs.rs/eframe/latest/eframe/#structs', 'eframe - Rust'],
];
for (const [url, title] of seed) await mock.api.tabs.create({ url, title });
mock.cookies.push({ name: 'sid', domain: '.github.com', path: '/', secure: true, storeId: 'default' });

const glue = await import(`${base}/pkg/session_manager.js`);
await glue.default();
await glue.start_background();

const send = async (cmd) => JSON.parse(await smPlatform.sendMessage(JSON.stringify(cmd)));
const { state } = await send({ cmd: 'GetState' });
const s = state.sessions[0];
const ref = (pred) => s.entries.filter(pred).map((e) => ({ session: s.id, entry: e.id }));
await send({ cmd: 'SetGroup', entries: ref((e) => e.url.includes('github') || e.url.includes('docs.rs')), group: 'Dev' });
await send({ cmd: 'TagEntries', entries: ref((e) => e.url.includes('rust') || e.url.includes('docs.rs')), tag: 'rust', add: true });
await send({ cmd: 'TagEntries', entries: ref((e) => e.url.includes('mozilla')), tag: 'extensions', add: true });
const work = (await send({ cmd: 'CreateSession', name: 'Research', make_active: false })).state.sessions[1];
await send({ cmd: 'MoveEntries', entries: ref((e) => e.url.includes('ycombinator')), target: work.id });
const ycTab = [...mock.tabs.values()].find((t) => t.url.includes('ycombinator'));
await mock.api.tabs.remove(ycTab.id);

await send({ cmd: 'SaveWorkspace', workspace: { id: 0, name: 'Rust', color: 'Orange', icon: '💻', auto_add: true,
  rules: [{ Query: 'tag:rust' }, { Site: { kind: 'Domain', value: 'github.com' } }] } });
await send({ cmd: 'SaveWorkspace', workspace: { id: 0, name: 'Reading', color: 'Purple', icon: '📚', auto_add: true,
  rules: [{ Group: 'Read later' }] } });
await send({ cmd: 'AssignWorkspace', entries: ref((e) => e.url.includes('mozilla')), workspace: null });
const mdn = [...mock.tabs.values()].find((t) => t.url.includes('mozilla'));
await mock.api.tabs.discard(mdn.id);
mock.ev.updated.fire(mdn.id, { discarded: true }, mock.clone(mock.tabs.get(mdn.id)));
const { state: st2 } = await send({ cmd: 'GetState' });
// ?compact — one-line rows; ?group=Domain — list grouping.
const params = new URLSearchParams(location.search);
const tabList = { ...st2.settings.tab_list, compact: params.has('compact'), grouping: params.get('group') || 'Workspace' };
await send({ cmd: 'UpdateSettings', settings: { ...st2.settings, tab_list: tabList,
  media: { ...st2.settings.media, policy: 'LimitBackground', max_background: 2 } } });
const egui = [...mock.tabs.values()].find((t) => t.url.includes('emilk'));
mock.ev.message.fire({ smMedia: { playing: true, video: true, pip: false } }, { tab: { id: egui.id }, frameId: 0 }, () => {});

await send({ cmd: 'CreateSnapshot', session: s.id, name: 'Before cleanup' });
await mock.api.tabs.create({ url: 'https://example.org/new-after-snapshot', title: 'New page' });

// ?clip=text — stand-in clipboard contents (headless browsers deny clipboard access).
const clip = new URLSearchParams(location.search).get('clip');
if (clip !== null) smPlatform.readClipboard = async () => clip;

await glue.start_ui('sm-canvas', popup ? 'popup' : sidebar ? 'sidebar' : 'manager');
document.getElementById('sm-loading')?.remove();

// ?click=x,y;x,y — synthetic clicks (for headless screenshots of other views).
const clicks = new URLSearchParams(location.search).get('click');
if (clicks) {
  const canvas = document.getElementById('sm-canvas');
  const opts = (x, y) => ({ clientX: x, clientY: y, bubbles: true, pointerId: 1, pointerType: 'mouse', button: 0, buttons: 1 });
  let delay = 500;
  for (const pair of clicks.split(';')) {
    const [x, y] = pair.split(',').map(Number);
    setTimeout(() => {
      canvas.dispatchEvent(new PointerEvent('pointermove', opts(x, y)));
      canvas.dispatchEvent(new PointerEvent('pointerdown', opts(x, y)));
      canvas.dispatchEvent(new MouseEvent('mousedown', opts(x, y)));
      canvas.dispatchEvent(new PointerEvent('pointerup', { ...opts(x, y), buttons: 0 }));
      canvas.dispatchEvent(new MouseEvent('mouseup', { ...opts(x, y), buttons: 0 }));
    }, delay);
    delay += 500;
  }
}

// Log tab API calls so headless runs can verify UI actions (read with --enable-logging=stderr).
for (const name of ['update', 'create']) {
  const orig = mock.api.tabs[name];
  mock.api.tabs[name] = async (...args) => {
    console.log(`[mock] tabs.${name} ${JSON.stringify(args)}`);
    return orig(...args);
  };
}

// ?hover=x,y — move the pointer there after the clicks (to show hover states).
const hover = new URLSearchParams(location.search).get('hover');
if (hover) {
  const [x, y] = hover.split(',').map(Number);
  setTimeout(() => {
    document.getElementById('sm-canvas').dispatchEvent(
      new PointerEvent('pointermove', { clientX: x, clientY: y, bubbles: true, pointerId: 1, pointerType: 'mouse' }),
    );
  }, 3000);
}

// ?keys=ArrowDown,Enter — synthetic key presses (sent after the clicks).
const keys = new URLSearchParams(location.search).get('keys');
if (keys) {
  // Headless browsers report no document focus, and eframe ignores keys without it.
  document.hasFocus = () => true;
  const canvas = document.getElementById('sm-canvas');
  canvas.focus();
  let delay = 2000;
  for (const key of keys.split(',')) {
    setTimeout(() => {
      const target = document.activeElement || canvas;
      for (const type of ['keydown', 'keyup']) {
        // A real keyCode matters: eframe treats keyCode 0 as an iOS special case and drops it.
        const keyCode = { ArrowDown: 40, ArrowUp: 38, Enter: 13, Escape: 27 }[key] ?? key.toUpperCase().charCodeAt(0);
        target.dispatchEvent(new KeyboardEvent(type, { key, code: key, keyCode, which: keyCode, bubbles: true, cancelable: true }));
      }
    }, delay);
    delay += 400;
  }
}
