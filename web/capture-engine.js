// Capture window engine (capture.html): `globalThis.smCapture`, driven by
// src/ui/capture_window.rs. It records (a page's element through web/capture-page.js, a
// capture of a tab cut to an element, or the window's own player), downloads streams and
// files, and keeps the results ("takes") as blobs until they are saved or uploaded. Media
// bytes never go through the wasm side, which only gets events (JSON) and small heads of
// files (to write a WebM's duration).
//
//   smCapture.setListener(cb)        cb(json): { type: 'level' | 'started' | 'take' | 'error' |
//                                    'note' | 'progress' | 'upload' | 'command' | 'screenEnded', ... }
//   smCapture.call(name, json)       -> Promise<json>: see `ops` below
//   smCapture.gesture(name)          on a user gesture: 'screen' (pick a tab), 'outputs', 'pip'
//   smCapture.head(take, n) / replaceHead(take, bytes, oldLength)
const api = globalThis.browser ?? globalThis.chrome;
const preview = document.getElementById('sm-preview');

let listener = null;
const emit = (ev) => {
  try {
    listener?.(JSON.stringify(ev));
  } catch (e) {
    console.warn('[session-manager] capture event', e);
  }
};
const message = (e) => String(e && e.message ? e.message : e);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const fromBase64 = (s) => {
  if (Uint8Array.fromBase64) return Uint8Array.fromBase64(s);
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
};

// ------------------------------------------------------------------ takes

const takes = new Map();
let nextTake = 1;
function newTake(kind, label, mime = '') {
  const t = { id: nextTake++, kind, label, mime, parts: [], bytes: 0, ms: 0, saved: false, blob: null };
  takes.set(t.id, t);
  return t;
}
const blobOf = (t) => (t.blob ??= new Blob(t.parts, { type: t.mime }));
const takeOut = (t) => ({ id: t.id, kind: t.kind, label: t.label, mime: t.mime, ext: t.ext || '', bytes: t.bytes, ms: t.ms });
const take = (id) => {
  const t = takes.get(id);
  if (!t) throw new Error('that recording is gone');
  return t;
};
// What this window does, for the inspector's Capture view (storage.session). Published from
// here, not from the wasm side: egui only runs while the window is shown.
let info = null; // { id, title, tab, windowTab }
let published = '';
function publish() {
  if (!info || !api.storage.session) return;
  const job = [...jobs.values()].find((j) => !j.cancelled);
  let state = 'Waiting';
  let bytes = 0;
  let ms = 0;
  if (rec) {
    state = rec.pausedAt ? 'Paused' : 'Recording';
    bytes = rec.take.bytes;
    ms = rec.elapsed();
  } else if (job) {
    state = job.total ? `Downloading ${job.done} / ${job.total}` : 'Downloading…';
    bytes = job.bytes;
  } else if (takes.size) {
    state = 'Ready';
    for (const t of takes.values()) bytes += t.bytes;
  }
  const summary = { id: info.id, window_tab: info.windowTab, tab: info.tab, title: info.title, state, busy: !!(rec || job), bytes, ms, takes: takes.size };
  const json = JSON.stringify(summary);
  if (json === published) return;
  published = json;
  api.storage.session.set({ [`smCapture:${info.id}`]: json }).catch(() => {});
}
setInterval(() => (rec || jobs.size) && publish(), 1000);

function updateUnsaved() {
  publish();
  const unsaved = [...takes.values()].some((t) => !t.saved) || !!rec;
  window.onbeforeunload = unsaved ? (e) => (e.preventDefault(), (e.returnValue = '')) : null;
}

// ------------------------------------------------------------------ pages (web/capture-page.js)

const pages = new Map(); // "tab:frame" -> connection
async function page(tab, frame) {
  const key = `${tab}:${frame}`;
  if (pages.has(key)) return pages.get(key);
  await api.scripting.executeScript({ target: { tabId: tab, frameIds: [frame] }, files: ['capture-page.js'] });
  const port = api.tabs.connect(tab, { name: 'sm-capture', frameId: frame });
  const calls = new Map();
  const handlers = new Set();
  let n = 0;
  const conn = {
    on: (h) => handlers.add(h),
    off: (h) => handlers.delete(h),
    send: (op, args = {}) => port.postMessage({ op, ...args }),
    call: (op, args = {}) =>
      new Promise((resolve, reject) => {
        const call = ++n;
        calls.set(call, { resolve, reject });
        port.postMessage({ op, call, ...args });
      }),
    close: () => {
      pages.delete(key);
      port.disconnect();
    },
  };
  port.onMessage.addListener((msg) => {
    if (msg && msg.call) {
      const c = calls.get(msg.call);
      calls.delete(msg.call);
      if (c) msg.error ? c.reject(new Error(msg.error)) : c.resolve(msg.result);
      return;
    }
    for (const h of [...handlers]) h(msg);
  });
  port.onDisconnect.addListener(() => {
    pages.delete(key);
    for (const c of calls.values()) c.reject(new Error('the page went away (it navigated or closed)'));
    calls.clear();
    for (const h of [...handlers]) h({ disconnected: true });
  });
  pages.set(key, conn);
  return conn;
}

// ------------------------------------------------------------------ preview (the window's player)

let previewUrl = '';
function setPreview(src) {
  if (previewUrl) URL.revokeObjectURL(previewUrl);
  previewUrl = src && src.startsWith('blob:') ? src : '';
  preview.srcObject = null;
  preview.removeAttribute('src');
  if (src) preview.src = src;
  preview.load();
}

// "video/webm;codecs=vp9,opus" -> 'video/webm; codecs="vp9,opus"' (what MediaSource takes).
function mseType(mime) {
  const [base, ...params] = mime.split(';').map((s) => s.trim());
  const codecs = params.find((p) => p.startsWith('codecs='));
  return codecs ? `${base}; codecs="${codecs.slice(7).replace(/"/g, '')}"` : base;
}

// Plays a recording while it's being made, from its chunks (Media Source).
function liveFeed(mime) {
  const type = mseType(mime || '');
  if (!type || !globalThis.MediaSource || !MediaSource.isTypeSupported(type)) return null;
  const ms = new MediaSource();
  setPreview(URL.createObjectURL(ms));
  let sb = null;
  let ended = false;
  // No source buffer to play the chunks (or the preview moved on): they're not kept.
  let dead = false;
  const queue = [];
  // Closed once open: the preview plays something else now.
  ms.addEventListener('sourceclose', () => {
    dead = true;
    queue.length = 0;
  });
  const pump = () => {
    if (!sb || sb.updating || ms.readyState !== 'open') return;
    if (queue.length) {
      try {
        sb.appendBuffer(queue.shift());
      } catch (e) {
        // Full: drop what's long played and try again.
        if (e.name === 'QuotaExceededError') trim(true);
      }
    } else if (ended) {
      try {
        ms.endOfStream();
      } catch {}
    }
  };
  const trim = (force) => {
    const b = sb.buffered;
    if (!b.length || sb.updating) return;
    const end = b.end(b.length - 1);
    if (force || end - b.start(0) > 120) sb.remove(0, Math.max(0, end - 30));
  };
  ms.addEventListener(
    'sourceopen',
    () => {
      try {
        sb = ms.addSourceBuffer(type);
        sb.mode = 'sequence';
      } catch {
        dead = true;
        queue.length = 0;
        return;
      }
      sb.addEventListener('updateend', () => {
        const b = sb.buffered;
        if (b.length) {
          const end = b.end(b.length - 1);
          // Stay near the live edge (unless the user paused to look at something).
          if (!preview.paused && end - preview.currentTime > 4) preview.currentTime = Math.max(0, end - 1);
          if (preview.paused && !feed.started) {
            feed.started = true;
            preview.play().catch(() => {});
          }
        }
        trim(false);
        pump();
      });
      pump();
    },
    { once: true },
  );
  const feed = {
    started: false,
    push(u8) {
      if (dead) return;
      queue.push(u8);
      pump();
    },
    end() {
      ended = true;
      pump();
    },
  };
  return feed;
}

// ------------------------------------------------------------------ recording

let rec = null; // the recording in progress
let lastLevel = 0;
function level(force = false) {
  if (!rec) return;
  const now = Date.now();
  if (!force && now - lastLevel < 400) return;
  lastLevel = now;
  emit({ type: 'level', bytes: rec.take.bytes, ms: rec.elapsed() });
}

function newRecording(kind, label) {
  const r = {
    kind,
    take: newTake('recording', label),
    started: Date.now(),
    pausedMs: 0,
    pausedAt: 0,
    cleanup: [],
    elapsed() {
      return (this.pausedAt || Date.now()) - this.started - this.pausedMs;
    },
    markPause(on) {
      if (on && !this.pausedAt) this.pausedAt = Date.now();
      if (!on && this.pausedAt) {
        this.pausedMs += Date.now() - this.pausedAt;
        this.pausedAt = 0;
      }
    },
  };
  return r;
}

// The end of a recording: its take is ready.
function finishRecording(r, ms, note) {
  if (rec !== r) return;
  rec = null;
  for (const f of r.cleanup) {
    try {
      f();
    } catch {}
  }
  r.take.ms = ms ?? r.elapsed();
  r.take.blob = null;
  emit({ type: 'take', take: takeOut(r.take), note: note || '' });
  updateUnsaved();
}

function mimeTypes(format, tracks) {
  if (tracks === 'a') {
    return format === 'mp4' ? ['audio/mp4;codecs=mp4a.40.2', 'audio/mp4', 'audio/webm;codecs=opus', 'audio/webm'] : ['audio/webm;codecs=opus', 'audio/webm', 'audio/ogg;codecs=opus'];
  }
  const sound = tracks === 'av';
  const webm = sound ? ['video/webm;codecs=vp9,opus', 'video/webm;codecs=vp8,opus', 'video/webm'] : ['video/webm;codecs=vp9', 'video/webm;codecs=vp8', 'video/webm'];
  const mp4 = sound ? ['video/mp4;codecs=avc1.640028,mp4a.40.2', 'video/mp4;codecs=avc1,mp4a.40.2', 'video/mp4'] : ['video/mp4;codecs=avc1.640028', 'video/mp4;codecs=avc1', 'video/mp4'];
  return format === 'mp4' ? [...mp4, ...webm] : webm;
}

// Records a stream in this window (screen capture, the window's own player).
function recordHere(r, stream, o, { live = false, feedPreview = true } = {}) {
  const mime = mimeTypes(o.format, o.tracks).find((t) => MediaRecorder.isTypeSupported(t)) || '';
  const opts = mime ? { mimeType: mime } : {};
  if (o.videoBps && stream.getVideoTracks().length) opts.videoBitsPerSecond = o.videoBps;
  if (stream.getAudioTracks().length) opts.audioBitsPerSecond = 192000;
  const recorder = new MediaRecorder(stream, opts);
  r.take.mime = recorder.mimeType || mime;
  recorder.ondataavailable = (e) => {
    if (!e.data || !e.data.size) return;
    r.take.parts.push(e.data);
    r.take.bytes += e.data.size;
    level();
  };
  recorder.onstop = () => finishRecording(r);
  recorder.onerror = (e) => emit({ type: 'error', message: `the recording stopped: ${message(e.error || e)}` });
  r.stop = () => recorder.state !== 'inactive' && recorder.stop();
  r.pause = () => {
    if (recorder.state === 'recording') recorder.pause();
    r.markPause(true);
  };
  r.resume = () => {
    if (recorder.state === 'paused') recorder.resume();
    r.markPause(false);
  };
  for (const t of stream.getTracks()) t.addEventListener('ended', () => r.stop());
  if (live && feedPreview) {
    setPreview('');
    preview.srcObject = stream;
    preview.muted = true;
    preview.play().catch(() => {});
  }
  recorder.start(1000);
  rec = r;
  updateUnsaved();
  return { mime: r.take.mime, video: stream.getVideoTracks().length, audio: stream.getAudioTracks().length };
}

// A page's element, recorded in the page (its own stream).
async function recordElement(o) {
  if (rec) throw new Error('already recording');
  const conn = await page(o.tab, o.frame);
  const r = newRecording('element', o.label || 'Recording');
  let feed = null;
  const onMsg = (msg) => {
    if (msg.chunk) {
      const u8 = fromBase64(msg.chunk);
      r.take.parts.push(new Blob([u8]));
      r.take.bytes += u8.length;
      feed?.push(u8);
      level();
    } else if (msg.stopped) {
      feed?.end();
      finishRecording(r, msg.ms);
    } else if (msg.error) {
      emit({ type: 'error', message: msg.error });
    } else if (msg.disconnected) {
      feed?.end();
      finishRecording(r, undefined, 'The page went away: the recording stops there.');
    }
  };
  conn.on(onMsg);
  r.cleanup.push(() => conn.off(onMsg));
  let info;
  try {
    info = await conn.call('record', {
      id: o.el,
      tracks: o.tracks,
      mimeTypes: mimeTypes(o.format, o.tracks),
      videoBps: o.videoBps || 0,
      fromStart: !!o.fromStart,
      stopAtEnd: !!o.stopAtEnd,
      listenHere: !!o.listenHere,
      timeslice: 1000,
    });
  } catch (e) {
    conn.off(onMsg);
    takes.delete(r.take.id);
    throw e;
  }
  r.take.mime = info.mime;
  r.started = Date.now();
  r.stop = () => conn.send('stop');
  r.pause = () => {
    conn.send('pause');
    r.markPause(true);
    publish();
  };
  r.resume = () => {
    conn.send('resume');
    r.markPause(false);
    publish();
  };
  r.listenHere = (on) => {
    conn.send('listenHere', { id: o.el, on });
    preview.muted = !on;
    if (on) preview.play().catch(() => {});
  };
  rec = r;
  feed = liveFeed(info.mime);
  preview.muted = !o.listenHere;
  updateUnsaved();
  return { ...info, preview: !!feed };
}

// ---- what the browser shows: a capture of the tab, cut to the element

let picking = null; // getDisplayMedia() in progress (started on a click)
let shared = null; // the stream the user shared

function pickScreen() {
  const md = navigator.mediaDevices;
  if (!md || !md.getDisplayMedia) {
    picking = Promise.reject(new Error('this browser can’t capture the screen here'));
    return picking;
  }
  picking = md.getDisplayMedia({
    video: { displaySurface: 'browser', frameRate: { ideal: 30 } },
    audio: { suppressLocalAudioPlayback: false },
    selfBrowserSurface: 'exclude',
    surfaceSwitching: 'exclude',
    systemAudio: 'include',
    preferCurrentTab: false,
  });
  picking.catch(() => {});
  return picking;
}

async function screenPicked() {
  if (!picking) throw new Error('choose what to capture first');
  let stream;
  try {
    stream = await picking;
  } catch (e) {
    throw new Error(e && e.name === 'NotAllowedError' ? 'nothing was shared' : message(e));
  } finally {
    picking = null;
  }
  for (const t of shared?.getTracks() || []) t.stop();
  shared = stream;
  const v = stream.getVideoTracks()[0];
  const s = v ? v.getSettings() : {};
  v?.addEventListener('ended', () => {
    if (shared === stream) {
      shared = null;
      if (!rec) emit({ type: 'screenEnded' });
    }
  });
  // Show what's shared until the recording starts.
  setPreview('');
  preview.srcObject = stream;
  preview.muted = true;
  preview.play().catch(() => {});
  return { surface: s.displaySurface || '', width: s.width || 0, height: s.height || 0, audio: stream.getAudioTracks().length > 0, label: v?.label || '' };
}

// A timer that keeps its pace when the window is in the background (workers aren't throttled).
function ticker(fps, f) {
  const w = new Worker('capture-timer.js');
  w.onmessage = f;
  w.postMessage(1000 / fps);
  return () => w.terminate();
}

async function recordScreen(o) {
  if (rec) throw new Error('already recording');
  const stream = shared;
  if (!stream || !stream.getVideoTracks().some((t) => t.readyState === 'live')) throw new Error('choose the tab to capture first');
  const audio = o.tracks !== 'v' ? stream.getAudioTracks() : [];
  if (o.tracks === 'a' && !audio.length) throw new Error('no sound is shared: choose the tab again, with “Share tab audio” on');
  const r = newRecording('screen', o.label || 'Screen recording');
  r.cleanup.push(() => {
    for (const t of stream.getTracks()) t.stop();
    if (shared === stream) shared = null;
  });
  const surface = stream.getVideoTracks()[0].getSettings().displaySurface || '';
  let out;
  let note = '';
  if (o.tracks === 'a') {
    out = new MediaStream(audio);
  } else if (o.crop && o.tab != null && o.el && surface === 'browser') {
    const video = await cropToElement(r, stream, o).catch((e) => {
      note = `Recording the whole tab: ${message(e)}`;
      return null;
    });
    out = new MediaStream([...(video ? [video] : stream.getVideoTracks()), ...audio]);
  } else {
    if (o.crop && surface && surface !== 'browser') note = 'A window or screen was shared, not a tab: all of it is recorded.';
    out = new MediaStream([...stream.getVideoTracks(), ...audio]);
  }
  if (o.tracks !== 'a' && !audio.length && o.tracks === 'av') note ||= 'No sound is shared (turn on “Share tab audio” when choosing the tab): picture only.';
  const info = recordHere(r, out, o, { live: true });
  // Stop when the user stops sharing.
  for (const t of stream.getVideoTracks()) t.addEventListener('ended', () => r.stop());
  if (note) emit({ type: 'note', message: note });
  return { ...info, preview: true, note };
}

// A video track of `stream` (a tab) cut to where the element is, following it.
async function cropToElement(r, stream, o) {
  const conn = await page(o.tab, o.frame);
  let where = null;
  let frameBox = o.frame === 0 ? { box: { x: 0, y: 0 }, viewport: null } : null;
  const onWhere = (m) => {
    if ('where' in m) where = m.where;
  };
  conn.on(onWhere);
  r.cleanup.push(() => conn.off(onWhere), () => conn.send('unwatch'));
  const w = await conn.call('watchRect', { id: o.el });
  if (o.frame !== 0) {
    if (w.nested) throw new Error('the video is in a frame inside another frame');
    const top = await page(o.tab, 0);
    const onFrame = (m) => {
      if ('frame' in m) frameBox = m.frame;
    };
    top.on(onFrame);
    r.cleanup.push(() => top.off(onFrame), () => top.send('unwatch'));
    await top.call('watchFrame', { token: w.token });
  }
  for (let i = 0; i < 40 && !(where && frameBox && frameBox.box); i++) await sleep(50);
  if (!where || !frameBox || !frameBox.box) throw new Error('couldn’t find where the video is on the page');

  // Where it is in a captured frame of `fw` × `fh` pixels. The browser fits the tab into the
  // frame, keeping its shape (with bars around it when the shapes differ, e.g. after the
  // "sharing this tab" bar took some height).
  const region = (fw, fh) => {
    const view = frameBox.viewport || where.viewport;
    const s = Math.min(fw / view.width, fh / view.height);
    const ox = (fw - view.width * s) / 2;
    const oy = (fh - view.height * s) / 2;
    return { x: ox + (frameBox.box.x + where.rect.x) * s, y: oy + (frameBox.box.y + where.rect.y) * s, w: where.rect.width * s, h: where.rect.height * s };
  };
  // The output keeps the first size (even, at most 4K); later sizes are fitted into it.
  const sizeFor = (fw, fh) => {
    const g = region(fw, fh);
    if (g.w < 2 || g.h < 2) throw new Error('the video takes no room on the page');
    const fit = Math.min(1, 3840 / g.w, 2160 / g.h);
    return [Math.max(2, Math.round((g.w * fit) / 2) * 2), Math.max(2, Math.round((g.h * fit) / 2) * 2)];
  };
  const paint = (ctx, cw, ch, source, fw, fh) => {
    const g = region(fw, fh);
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, cw, ch);
    if (g.w < 1 || g.h < 1) return;
    const s = Math.min(cw / g.w, ch / g.h);
    const dw = g.w * s;
    const dh = g.h * s;
    ctx.drawImage(source, g.x, g.y, g.w, g.h, (cw - dw) / 2, (ch - dh) / 2, dw, dh);
  };
  const track = stream.getVideoTracks()[0];

  // Chromium: frames straight from the track, into a new track. This keeps going while the
  // window is in the background (where a <video> isn't drawn).
  if (globalThis.MediaStreamTrackProcessor && globalThis.MediaStreamTrackGenerator && globalThis.OffscreenCanvas) {
    const reader = new MediaStreamTrackProcessor({ track }).readable.getReader();
    const generator = new MediaStreamTrackGenerator({ kind: 'video' });
    const writer = generator.writable.getWriter();
    const first = await reader.read();
    if (first.done) throw new Error('the shared tab sends no picture');
    let [cw, ch] = [0, 0];
    try {
      [cw, ch] = sizeFor(first.value.displayWidth, first.value.displayHeight);
    } catch (e) {
      first.value.close();
      reader.cancel().catch(() => {});
      throw e;
    }
    const canvas = new OffscreenCanvas(cw, ch);
    const ctx = canvas.getContext('2d', { alpha: false });
    let running = true;
    (async () => {
      let frame = first.value;
      while (running && frame) {
        paint(ctx, cw, ch, frame, frame.displayWidth, frame.displayHeight);
        const out = new VideoFrame(canvas, { timestamp: frame.timestamp });
        frame.close();
        await writer.write(out).catch(() => (running = false));
        const next = await reader.read().catch(() => ({ done: true }));
        frame = next.done ? null : next.value;
      }
      frame?.close();
    })();
    r.cleanup.push(() => {
      running = false;
      reader.cancel().catch(() => {});
      writer.close().catch(() => {});
    });
    return generator;
  }

  // Elsewhere: a <video> of the shared tab drawn on a canvas on a steady timer.
  const src = document.createElement('video');
  src.muted = true;
  src.srcObject = new MediaStream([track]);
  await src.play();
  for (let i = 0; i < 40 && !src.videoWidth; i++) await sleep(50);
  const [cw, ch] = sizeFor(src.videoWidth, src.videoHeight);
  const canvas = document.createElement('canvas');
  canvas.width = cw;
  canvas.height = ch;
  const ctx = canvas.getContext('2d', { alpha: false });
  const draw = () => src.videoWidth && paint(ctx, cw, ch, src, src.videoWidth, src.videoHeight);
  draw();
  const stop = ticker(30, draw);
  r.cleanup.push(stop, () => (src.srcObject = null));
  return canvas.captureStream(30).getVideoTracks()[0];
}

// ---- the window's own player (a media URL)

let playerRecordable = true;
async function loadUrl({ url }) {
  const attempt = (cors) =>
    new Promise((resolve, reject) => {
      setPreview('');
      if (cors) preview.crossOrigin = 'anonymous';
      else preview.removeAttribute('crossorigin');
      const ok = () => {
        cleanup();
        resolve();
      };
      const bad = () => {
        cleanup();
        reject(new Error(preview.error?.message || 'the browser can’t play it'));
      };
      const cleanup = () => {
        preview.removeEventListener('loadedmetadata', ok);
        preview.removeEventListener('error', bad);
      };
      preview.addEventListener('loadedmetadata', ok);
      preview.addEventListener('error', bad);
      preview.src = url;
    });
  try {
    await attempt(true);
    playerRecordable = true;
  } catch {
    await attempt(false);
    playerRecordable = false;
  }
  return { duration: Number.isFinite(preview.duration) ? preview.duration : 0, width: preview.videoWidth, height: preview.videoHeight, recordable: playerRecordable };
}

async function recordPlayer(o) {
  if (rec) throw new Error('already recording');
  if (!playerRecordable) throw new Error('the site of this media doesn’t allow it to be recorded; download it instead');
  const capture = preview.captureStream ? preview.captureStream() : preview.mozCaptureStream?.();
  if (!capture) throw new Error('this browser can’t record its player');
  if (o.fromStart) preview.currentTime = 0;
  await preview.play();
  for (let i = 0; i < 40 && !capture.getTracks().length; i++) await sleep(50);
  const tracks = [...(o.tracks !== 'a' ? capture.getVideoTracks() : []), ...(o.tracks !== 'v' ? capture.getAudioTracks() : [])];
  if (!tracks.length) throw new Error('nothing to record (is it playing?)');
  const r = newRecording('player', o.label || 'Recording');
  // Firefox takes the sound away from a captured player: play it back.
  if (!preview.captureStream && capture.getAudioTracks().length) {
    try {
      const ac = new AudioContext();
      ac.createMediaStreamSource(capture).connect(ac.destination);
      r.cleanup.push(() => ac.close());
    } catch {}
  }
  const onEnded = () => o.stopAtEnd && r.stop();
  preview.addEventListener('ended', onEnded);
  r.cleanup.push(() => preview.removeEventListener('ended', onEnded));
  return recordHere(r, new MediaStream(tracks), o);
}

// ------------------------------------------------------------------ downloading

const jobs = new Map(); // job -> { cancelled, abort: Set<() => void> }
let reqs = 0;

// Fetches a part: in the page (as its player does) when `via` names a tab, else from here.
async function fetchPart(via, part, { head = 0, onBytes = () => {}, job = null } = {}) {
  if (via && via.tab != null) {
    const conn = await page(via.tab, via.frame ?? 0);
    return new Promise((resolve, reject) => {
      const req = ++reqs;
      const chunks = [];
      const h = (m) => {
        if (m.disconnected) {
          conn.off(h);
          reject(new Error('the page went away (it navigated or closed)'));
          return;
        }
        if (m.req !== req) return;
        if (m.data) {
          const u8 = fromBase64(m.data);
          chunks.push(u8);
          onBytes(u8.length);
        } else if (m.done) {
          conn.off(h);
          job?.abort.delete(abort);
          resolve({ blob: new Blob(chunks), type: m.type, length: m.length });
        } else if (m.error) {
          conn.off(h);
          job?.abort.delete(abort);
          reject(new Error(m.error));
        }
      };
      const abort = () => {
        conn.send('cancel', { req });
        conn.off(h);
        reject(new Error('cancelled'));
      };
      job?.abort.add(abort);
      conn.on(h);
      conn.send('fetch', { req, url: part.url, range: part.range || null, head });
    });
  }
  const ctl = new AbortController();
  const abort = () => ctl.abort();
  job?.abort.add(abort);
  try {
    const headers = part.range ? { Range: `bytes=${part.range[0]}-${part.range[1]}` } : {};
    const res = await fetch(part.url, { headers, signal: ctl.signal, credentials: 'include', cache: 'no-store' });
    if (!res.ok) throw new Error(`HTTP ${res.status} for ${part.url}`);
    const reader = res.body.getReader();
    const chunks = [];
    let total = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      total += value.length;
      onBytes(value.length);
      if (head && total >= head) {
        ctl.abort();
        break;
      }
    }
    return { blob: new Blob(chunks), type: res.headers.get('content-type') || '', length: Number(res.headers.get('content-length')) || 0 };
  } catch (e) {
    throw job?.cancelled ? new Error('cancelled') : e;
  } finally {
    job?.abort.delete(abort);
  }
}

// What's at a URL: its type, size, and the start of it as text (a manifest is all text).
async function probe({ url, via }) {
  const r = await fetchPart(via, { url }, { head: 512 * 1024 });
  const head = new Uint8Array(await r.blob.slice(0, 512 * 1024).arrayBuffer());
  const text = new TextDecoder().decode(head.subarray(0, 4096));
  // (TextDecoder drops a byte order mark.)
  const textual = /^\s*(#EXTM3U|<\?xml|<MPD)/.test(text) || /mpegurl|dash\+xml|^text\//i.test(r.type);
  return { type: r.type, length: r.length, text: textual ? new TextDecoder().decode(head) : '', complete: textual && r.blob.size < 512 * 1024 };
}

async function fetchText({ url, via }) {
  const r = await fetchPart(via, { url });
  return r.blob.text();
}

// Fetches the parts of a track (in parallel, in order) into a take.
async function download(o) {
  const job = { cancelled: false, abort: new Set(), done: 0, total: o.parts.length, bytes: 0 };
  jobs.set(o.job, job);
  const t = newTake('download', o.label, o.mime || '');
  t.ext = o.ext || '';
  const parts = o.parts;
  const results = new Array(parts.length);
  let done = 0;
  let bytes = 0;
  let next = 0;
  let lastEmit = 0;
  const progress = (force) => {
    const now = Date.now();
    if (!force && now - lastEmit < 300) return;
    lastEmit = now;
    Object.assign(job, { done, bytes });
    emit({ type: 'progress', job: o.job, done, total: parts.length, bytes });
  };
  const worker = async () => {
    while (!job.cancelled && next < parts.length) {
      const i = next++;
      for (let attempt = 1; ; attempt++) {
        let got = 0;
        try {
          const r = await fetchPart(o.via, parts[i], {
            job,
            onBytes: (n) => {
              got += n;
              bytes += n;
              progress(false);
            },
          });
          results[i] = r.blob;
          if (!t.mime && i === parts.length - 1 && r.type && !/octet|text|xml|mpegurl/i.test(r.type)) t.mime = r.type.split(';')[0];
          break;
        } catch (e) {
          bytes -= got;
          if (job.cancelled) return;
          if (attempt >= 3) throw new Error(`part ${i + 1} of ${parts.length}: ${message(e)}`);
          await sleep(500 * attempt);
        }
      }
      done++;
      progress(false);
    }
  };
  try {
    await Promise.all(Array.from({ length: Math.max(1, Math.min(o.parallel || 4, 8, parts.length)) }, worker));
  } catch (e) {
    job.cancelled = true;
    for (const a of job.abort) a();
    takes.delete(t.id);
    throw e;
  } finally {
    jobs.delete(o.job);
    publish();
  }
  if (job.cancelled) {
    takes.delete(t.id);
    throw new Error('cancelled');
  }
  progress(true);
  t.parts = results;
  t.blob = null; // (read while downloading, it was empty)
  t.bytes = results.reduce((n, b) => n + b.size, 0);
  t.ms = (o.duration || 0) * 1000;
  emit({ type: 'take', take: takeOut(t), note: '' });
  updateUnsaved();
  return takeOut(t);
}

// ------------------------------------------------------------------ sinks

function saveBlob(filename, blob) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.style.display = 'none';
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60000);
}

function upload({ take: id, url, method, field, headers, name }) {
  const t = take(id);
  const blob = blobOf(t);
  return new Promise((resolve, reject) => {
    const x = new XMLHttpRequest();
    x.open(method || 'PUT', url);
    for (const [k, v] of headers || []) x.setRequestHeader(k, v);
    let body = blob;
    if (field) {
      body = new FormData();
      body.append(field, blob, name);
    } else if (t.mime && !(headers || []).some(([k]) => k.toLowerCase() === 'content-type')) {
      x.setRequestHeader('Content-Type', t.mime);
    }
    x.upload.onprogress = (e) => emit({ type: 'upload', take: id, loaded: e.loaded, total: e.lengthComputable ? e.total : blob.size });
    x.onload = () => {
      const text = String(x.responseText || '').slice(0, 2000);
      if (x.status >= 200 && x.status < 300) resolve({ status: x.status, text, location: x.getResponseHeader('Location') || '' });
      else reject(new Error(`the server answered ${x.status} ${x.statusText}${text ? `: ${text.slice(0, 200)}` : ''}`));
    };
    x.onerror = () => reject(new Error('the upload failed (network error: is the address right, and does it accept uploads?)'));
    x.onabort = () => reject(new Error('cancelled'));
    t.xhr = x;
    x.send(body);
  }).finally(() => {
    t.xhr = null;
  });
}

// Audio outputs (needs a permission before the browser names them).
let asking = null;
async function outputs() {
  const md = navigator.mediaDevices;
  const canChoose = 'setSinkId' in HTMLMediaElement.prototype;
  if (!md || !md.enumerateDevices) return { canChoose: false, devices: [], named: false, select: false };
  const outs = (await md.enumerateDevices()).filter((d) => d.kind === 'audiooutput' && d.deviceId);
  return {
    canChoose,
    devices: outs.map((d) => ({ id: d.deviceId, label: d.label || (d.deviceId === 'default' ? 'Default' : 'Output') })),
    named: outs.some((d) => d.label),
    select: !!md.selectAudioOutput,
  };
}

function askOutputs() {
  const md = navigator.mediaDevices;
  // Firefox: the browser lists the outputs and the user picks one.
  if (md.selectAudioOutput) asking = md.selectAudioOutput().then((d) => ({ chosen: d.deviceId, label: d.label }));
  // Chromium: outputs are named once the page may use a microphone (it isn't used).
  else asking = md.getUserMedia({ audio: true }).then((s) => (s.getTracks().forEach((t) => t.stop()), { chosen: '' }));
  asking.catch(() => {});
  return asking;
}

// ------------------------------------------------------------------ requests from wasm

let windowId = '';
const ops = {
  async init({ id, title, tab }) {
    windowId = id;
    let tabId = null;
    try {
      tabId = (await api.tabs.getCurrent())?.id ?? null;
    } catch {}
    info = { id, title: title || '', tab: tab ?? null, windowTab: tabId };
    publish();
    addEventListener('pagehide', () => api.storage.session?.remove(`smCapture:${windowId}`));
    return {
      tabId,
      mp4: typeof MediaRecorder !== 'undefined' && MediaRecorder.isTypeSupported('video/mp4'),
      screen: !!navigator.mediaDevices?.getDisplayMedia,
      sinks: 'setSinkId' in HTMLMediaElement.prototype,
    };
  },
  recordElement,
  recordScreen,
  recordPlayer,
  screenPicked,
  stop: () => rec?.stop?.(),
  pause: () => rec?.pause?.(),
  resume: () => rec?.resume?.(),
  listenHere: ({ on }) => rec?.listenHere?.(on),
  pageMedia: async ({ tab, frame, el, action }) => (await page(tab, frame)).call('media', { id: el, action }),
  flash: async ({ tab, frame, el }) => {
    await api.scripting.executeScript({ target: { tabId: tab, frameIds: [frame] }, files: ['capture-page.js'] });
    await api.scripting.executeScript({ target: { tabId: tab, frameIds: [frame] }, func: (id) => globalThis.__smCapture?.flash(id), args: [el] });
  },
  loadUrl,
  probe,
  fetchText,
  download,
  cancel: ({ job }) => {
    const j = jobs.get(job);
    if (!j) return;
    j.cancelled = true;
    for (const a of [...j.abort]) a();
  },
  save: ({ take: id, name }) => {
    const t = take(id);
    saveBlob(name, blobOf(t));
    t.saved = true;
    updateUnsaved();
  },
  upload: async (o) => {
    const r = await upload(o);
    take(o.take).saved = true;
    updateUnsaved();
    return r;
  },
  cancelUpload: ({ take: id }) => takes.get(id)?.xhr?.abort(),
  discard: ({ take: id }) => {
    takes.delete(id);
    updateUnsaved();
  },
  play: ({ take: id }) => {
    setPreview(URL.createObjectURL(blobOf(take(id))));
    preview.muted = false;
    preview.play().catch(() => {});
  },
  showPreview: ({ on }) => document.body.classList.toggle('sm-no-preview', !on),
  outputs,
  outputsAsked: async () => {
    if (!asking) throw new Error('nothing was asked');
    try {
      return await asking;
    } finally {
      asking = null;
    }
  },
  setOutput: async ({ id }) => {
    if (!('setSinkId' in preview)) throw new Error('this browser can’t choose where sound plays');
    await preview.setSinkId(id || '');
  },
  setTitle: ({ title }) => (document.title = title),
};

api.runtime.onMessage.addListener((msg) => {
  const c = msg && msg.smCaptureCmd;
  if (!c || c.id !== windowId) return;
  // Done here too: the controls (egui) don't run while the window is hidden.
  if (c.cmd === 'stop') {
    rec?.stop?.();
    for (const j of jobs.values()) {
      j.cancelled = true;
      for (const a of [...j.abort]) a();
    }
  }
  emit({ type: 'command', cmd: String(c.cmd) });
});

globalThis.smCapture = {
  setListener(cb) {
    listener = cb;
  },
  async call(name, args) {
    const f = ops[name];
    if (!f) throw new Error(`unknown capture request ${name}`);
    const r = await f(args ? JSON.parse(args) : {});
    return JSON.stringify(r ?? null);
  },
  /** Runs on a user gesture (see smPlatform.setGestureAction): APIs that need one. */
  gesture(name) {
    if (name === 'screen') return pickScreen();
    if (name === 'outputs') return askOutputs();
    if (name === 'pip' && document.pictureInPictureEnabled && preview.readyState > 0) return preview.requestPictureInPicture();
    return undefined;
  },
  async head(id, n) {
    return new Uint8Array(await blobOf(take(id)).slice(0, n).arrayBuffer());
  },
  replaceHead(id, bytes, oldLength) {
    const t = take(id);
    const rest = blobOf(t).slice(oldLength);
    t.parts = [bytes.slice(), rest];
    t.blob = null;
    t.bytes = bytes.length + rest.size;
    return t.bytes;
  },
};
