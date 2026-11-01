// Capture content script, injected (on demand) into the frames of a page whose media the
// user wants to capture. It runs in the extension's isolated world, so the page can't see
// it. It lists the page's video and audio elements, and a capture window connects to it
// (tabs.connect, name "sm-capture") to:
//  - record an element's own stream (captureStream + MediaRecorder; chunks go to the window),
//  - report where an element is on screen (to cut it out of a capture of the tab),
//  - fetch stream segments the way the page's own player does (its cookies, referrer, CORS).
(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  if (!api || !api.runtime || globalThis.__smCapture) return;

  // Elements get ids that stay valid while this document lives.
  const ids = new WeakMap();
  const els = new Map();
  let nextId = 1;
  const idOf = (el) => {
    let id = ids.get(el);
    if (!id) {
      id = nextId++;
      ids.set(el, id);
      els.set(id, new WeakRef(el));
    }
    return id;
  };
  const media = (id) => {
    const el = els.get(id)?.deref();
    if (!el || !el.isConnected) throw new Error('that video is gone (the page changed); refresh the list');
    return el;
  };
  const message = (e) => String(e && e.message ? e.message : e);

  // Shadow roots too, closed ones where the browser lets extensions open them.
  const shadowOf = (el) => api.dom?.openOrClosedShadowRoot?.(el) ?? el.openOrClosedShadowRoot ?? el.shadowRoot;
  const deepAll = (selector, root = document, out = []) => {
    out.push(...root.querySelectorAll(selector));
    for (const el of root.querySelectorAll('*')) {
      const sr = shadowOf(el);
      if (sr) deepAll(selector, sr, out);
    }
    return out;
  };

  const sourceOf = (m) => {
    if (m.srcObject) return 'live';
    const src = m.currentSrc || m.src || '';
    if (src.startsWith('blob:') || src.startsWith('mediasource:')) return 'stream';
    return src ? 'file' : 'none';
  };

  // Media URLs: element sources, and requests for stream manifests and media files.
  const MEDIA = /\.(m3u8|mpd|mp4|m4v|m4s|m4a|webm|mkv|mov|ts|mp3|aac|ogg|oga|ogv|opus|flac|wav|flv)(\?|#|$)/i;
  const urls = () => {
    const out = [];
    const seen = new Set();
    const add = (url, how) => {
      if (!url || seen.has(url) || !/^https?:/i.test(url)) return;
      seen.add(url);
      out.push({ url, how });
    };
    for (const m of deepAll('video, audio')) {
      add(m.currentSrc || m.src, m.localName);
      for (const s of m.querySelectorAll('source')) add(s.src, 'source');
    }
    for (const r of performance.getEntriesByType('resource')) {
      if (MEDIA.test(r.name) || r.initiatorType === 'video' || r.initiatorType === 'audio') add(r.name, 'request');
    }
    return out;
  };

  function list() {
    const items = deepAll('video, audio').map((m) => {
      const r = m.getBoundingClientRect();
      return {
        id: idOf(m),
        tag: m.localName,
        src: m.currentSrc || m.src || '',
        source: sourceOf(m),
        duration: Number.isFinite(m.duration) ? m.duration : 0,
        time: m.currentTime || 0,
        paused: m.paused || m.ended,
        muted: m.muted || m.volume === 0,
        width: m.videoWidth || 0,
        height: m.videoHeight || 0,
        shown: r.width > 0 && r.height > 0,
        protected: !!m.mediaKeys,
        title: document.title,
      };
    });
    return { frameUrl: location.href, top: window === window.top, items, urls: urls() };
  }

  // Shows which element is meant: scrolls to it and outlines it for a moment.
  function flash(id) {
    const el = media(id);
    el.scrollIntoView({ block: 'center', inline: 'nearest' });
    const host = document.createElement('sm-capture-flash');
    const root = host.attachShadow({ mode: 'closed' });
    const box = document.createElement('div');
    root.append(box);
    document.documentElement.append(host);
    let left = 90;
    const place = () => {
      const r = el.getBoundingClientRect();
      box.style.cssText =
        `position:fixed;left:${r.x}px;top:${r.y}px;width:${r.width}px;height:${r.height}px;` +
        'pointer-events:none;z-index:2147483647;box-sizing:border-box;border:3px solid #e5484d;' +
        `background:rgba(229,72,77,.18);border-radius:4px;opacity:${Math.min(1, left / 30)}`;
      if (--left > 0) requestAnimationFrame(place);
      else host.remove();
    };
    place();
  }

  // ------------------------------------------------------------------ encoding

  const toBase64 = (u8) => {
    if (u8.toBase64) return u8.toBase64();
    let s = '';
    for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000));
    return btoa(s);
  };

  // ------------------------------------------------------------------ recording an element

  async function waitForTracks(stream, wantVideo, wantAudio, ms) {
    const has = () => (!wantVideo || stream.getVideoTracks().length) && (!wantAudio || stream.getAudioTracks().length);
    for (const end = Date.now() + ms; !has() && Date.now() < end; ) await new Promise((r) => setTimeout(r, 100));
  }

  async function record(port, session, o) {
    const m = media(o.id);
    if (m.mediaKeys) throw new Error('this media is protected (DRM): its stream can’t be recorded. Try “Rendered on screen” (protected video usually stays black there too).');
    const wantVideo = o.tracks !== 'a';
    const wantAudio = o.tracks !== 'v';
    if (wantVideo && m.localName !== 'video') throw new Error('this is an audio element: it has no picture (record its sound only)');
    if (o.fromStart) {
      try {
        m.currentTime = 0;
      } catch {}
    }
    if (m.paused || o.fromStart) await m.play().catch(() => {});
    let stream;
    let firefox = false;
    try {
      if (m.captureStream) stream = m.captureStream();
      else if (m.mozCaptureStream) {
        stream = m.mozCaptureStream();
        firefox = true;
      } else throw new Error('this browser can’t record media elements');
    } catch (e) {
      if (/cross|origin|security|insecure/i.test(message(e))) {
        throw new Error('this media comes from a site that doesn’t allow it to be recorded from the page. Use “Rendered on screen”, or save its stream.');
      }
      throw e;
    }
    await waitForTracks(stream, wantVideo, wantAudio, 4000);
    const video = wantVideo ? stream.getVideoTracks() : [];
    const audio = wantAudio ? stream.getAudioTracks() : [];
    if (wantVideo && !video.length) throw new Error('no picture is playing (start the video, then record)');
    if (o.tracks === 'a' && !audio.length) throw new Error('no sound is playing (start the media, or unmute it in the player)');
    const tracks = [...video, ...audio];
    // Firefox takes the sound away from the page while it's captured: play it back there.
    if (firefox && !o.listenHere && stream.getAudioTracks().length) {
      try {
        const ctx = new AudioContext();
        ctx.createMediaStreamSource(stream).connect(ctx.destination);
        session.monitor = ctx;
      } catch {}
    }
    if (o.listenHere) {
      session.restoreMuted = m.muted;
      m.muted = true;
    }
    const mime = (o.mimeTypes || []).find((t) => MediaRecorder.isTypeSupported(t)) || '';
    const opts = mime ? { mimeType: mime } : {};
    if (o.videoBps && video.length) opts.videoBitsPerSecond = o.videoBps;
    if (audio.length) opts.audioBitsPerSecond = 192000;
    const recorder = new MediaRecorder(new MediaStream(tracks), opts);
    session.recorder = recorder;
    session.element = m;
    // Chunks go out in order, each once its bytes are read.
    let sending = Promise.resolve();
    let bytes = 0;
    recorder.ondataavailable = (e) => {
      if (!e.data || !e.data.size) return;
      sending = sending.then(async () => {
        const u8 = new Uint8Array(await e.data.arrayBuffer());
        bytes += u8.length;
        port.postMessage({ chunk: toBase64(u8), bytes });
      });
    };
    let started = 0;
    let pausedFor = 0;
    let pausedAt = 0;
    recorder.onpause = () => (pausedAt = Date.now());
    recorder.onresume = () => {
      pausedFor += Date.now() - pausedAt;
      pausedAt = 0;
    };
    recorder.onstop = () => {
      const ms = (pausedAt || Date.now()) - started - pausedFor;
      sending = sending.then(() => port.postMessage({ stopped: true, ms, bytes }));
      finish(session);
    };
    recorder.onerror = (e) => port.postMessage({ error: `the recording stopped: ${message(e.error || e)}` });
    session.onEnded = () => {
      if (o.stopAtEnd && recorder.state !== 'inactive') recorder.stop();
    };
    m.addEventListener('ended', session.onEnded);
    // The page swapped the media's tracks (another source): the recording can't go on.
    for (const t of tracks) t.addEventListener('ended', () => recorder.state !== 'inactive' && recorder.stop());
    recorder.start(o.timeslice || 1000);
    started = Date.now();
    return {
      mime: recorder.mimeType || mime,
      video: video.length,
      audio: audio.length,
      width: m.videoWidth || 0,
      height: m.videoHeight || 0,
      title: document.title,
    };
  }

  // Undoes what recording did to the page.
  function finish(session) {
    if (session.element && session.onEnded) session.element.removeEventListener('ended', session.onEnded);
    if (session.element && session.restoreMuted != null) session.element.muted = session.restoreMuted;
    session.restoreMuted = null;
    session.monitor?.close().catch(() => {});
    session.monitor = null;
  }

  // ------------------------------------------------------------------ where an element is

  // A frame inside another one finds out where it is: it greets its parent with a token, and
  // the top frame's script remembers which iframe the greeting came from.
  const located = new Map(); // token -> iframe (top frame)
  if (window === window.top) {
    addEventListener('message', (e) => {
      const token = e.data && e.data.smCaptureLocate;
      if (typeof token !== 'string' || !token.startsWith('sm-cap-')) return;
      const known = located.get(token)?.deref();
      const frame = known && known.isConnected && known.contentWindow === e.source ? known : deepAll('iframe, frame').find((f) => f.contentWindow === e.source);
      if (!frame) return;
      located.set(token, new WeakRef(frame));
      // Found: the frame can stop asking.
      e.source.postMessage({ smCaptureLocated: token }, '*');
    });
  }

  const contentBox = (el) => {
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    const left = el.clientLeft + parseFloat(cs.paddingLeft || 0);
    const top = el.clientTop + parseFloat(cs.paddingTop || 0);
    return { x: r.x + left, y: r.y + top, width: el.clientWidth - parseFloat(cs.paddingLeft || 0) - parseFloat(cs.paddingRight || 0), height: el.clientHeight - parseFloat(cs.paddingTop || 0) - parseFloat(cs.paddingBottom || 0) };
  };

  // The picture of a video within its box (object-fit: contain leaves bars around it).
  const pictureBox = (m) => {
    const r = contentBox(m);
    const vw = m.videoWidth;
    const vh = m.videoHeight;
    const fit = getComputedStyle(m).objectFit;
    if (!vw || !vh || !r.width || !r.height || fit === 'fill' || fit === 'cover') return r;
    const scale = Math.min(r.width / vw, r.height / vh, fit === 'none' ? 1 : Infinity);
    const w = vw * scale;
    const h = vh * scale;
    return { x: r.x + (r.width - w) / 2, y: r.y + (r.height - h) / 2, width: w, height: h };
  };

  // Stops following (and greeting the top frame).
  function unwatch(session) {
    cancelAnimationFrame(session.follow);
    clearInterval(session.followTimer);
    clearInterval(session.greet);
    session.follow = session.followTimer = session.greet = 0;
  }

  // Reports where `measure()` is whenever it changes.
  function follow(session, port, measure, key) {
    unwatch(session);
    let last = '';
    const tick = () => {
      let v = null;
      try {
        v = measure();
      } catch {}
      const s = JSON.stringify(v);
      if (s !== last) {
        last = s;
        port.postMessage({ [key]: v });
      }
      session.follow = requestAnimationFrame(tick);
    };
    // rAF doesn't run in hidden tabs; a timer keeps the position fresh there.
    session.followTimer = setInterval(() => document.hidden && tick(), 500);
    tick();
  }

  function watchRect(port, session, { id }) {
    const m = media(id);
    follow(session, port, () => ({ rect: m.localName === 'video' ? pictureBox(m) : contentBox(m), viewport: { width: innerWidth, height: innerHeight }, dpr: devicePixelRatio }), 'where');
    if (window !== window.top) {
      // Only a frame right inside the top one can be placed; deeper ones record the tab.
      const token = `sm-cap-${crypto.randomUUID()}`;
      const greet = () => parent.postMessage({ smCaptureLocate: token }, '*');
      const answered = (e) => {
        if (e.source !== parent || !e.data || e.data.smCaptureLocated !== token) return;
        clearInterval(session.greet);
        session.greet = 0;
        removeEventListener('message', answered);
      };
      addEventListener('message', answered);
      greet();
      session.greet = setInterval(greet, 2000);
      return { token, nested: window.parent !== window.top };
    }
    return { token: null, nested: false };
  }

  function watchFrame(port, session, { token }) {
    follow(session, port, () => {
      const f = located.get(token)?.deref();
      return { box: f && f.isConnected ? contentBox(f) : null, viewport: { width: innerWidth, height: innerHeight } };
    }, 'frame');
  }

  // ------------------------------------------------------------------ fetching for the window

  // Requests as the page makes them (Firefox: `content.fetch` is the page's own fetch).
  const pageFetch = typeof content !== 'undefined' && content && content.fetch ? content.fetch.bind(content) : fetch;
  const CHUNK = 2 * 1024 * 1024;

  async function fetchFor(port, session, { req, url, range, head }) {
    const ctl = new AbortController();
    session.fetches.set(req, ctl);
    try {
      const headers = range ? { Range: `bytes=${range[0]}-${range[1]}` } : {};
      const res = await pageFetch(url, { headers, signal: ctl.signal, cache: 'no-store' });
      if (!res.ok) throw new Error(`HTTP ${res.status} for ${url}`);
      const type = res.headers.get('content-type') || '';
      const length = Number(res.headers.get('content-length')) || 0;
      const reader = res.body.getReader();
      let buf = [];
      let have = 0;
      let total = 0;
      const send = () => {
        if (!have) return;
        const out = new Uint8Array(have);
        let at = 0;
        for (const b of buf) {
          out.set(b, at);
          at += b.length;
        }
        port.postMessage({ req, data: toBase64(out) });
        buf = [];
        have = 0;
      };
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf.push(value);
        have += value.length;
        total += value.length;
        if (head && total >= head) {
          ctl.abort();
          break;
        }
        if (have >= CHUNK) send();
      }
      send();
      port.postMessage({ req, done: true, status: res.status, type, length, total });
    } catch (e) {
      if (!(head && ctl.signal.aborted)) port.postMessage({ req, error: message(e) });
    } finally {
      session.fetches.delete(req);
    }
  }

  // ------------------------------------------------------------------ connection

  api.runtime.onConnect.addListener((port) => {
    if (port.name !== 'sm-capture') return;
    const session = { recorder: null, element: null, fetches: new Map(), follow: 0, followTimer: 0, greet: 0, monitor: null, restoreMuted: null };
    const ops = {
      record: (o) => record(port, session, o),
      stop: () => session.recorder?.state !== 'inactive' && session.recorder?.stop(),
      pause: () => session.recorder?.state === 'recording' && session.recorder.pause(),
      resume: () => session.recorder?.state === 'paused' && session.recorder.resume(),
      // Play / pause the page's media.
      media: ({ id, action }) => {
        const m = media(id);
        if (action === 'play') return m.play().then(() => null);
        m.pause();
        return null;
      },
      listenHere: ({ id, on }) => {
        const m = media(id);
        if (on) {
          session.restoreMuted ??= m.muted;
          m.muted = true;
        } else if (session.restoreMuted != null) {
          m.muted = session.restoreMuted;
          session.restoreMuted = null;
        }
      },
      watchRect: (o) => watchRect(port, session, o),
      watchFrame: (o) => watchFrame(port, session, o),
      unwatch: () => unwatch(session),
      fetch: (o) => void fetchFor(port, session, o),
      cancel: ({ req }) => session.fetches.get(req)?.abort(),
    };
    port.onMessage.addListener(async (msg) => {
      const { op, call, ...args } = msg || {};
      try {
        if (!ops[op]) throw new Error(`unknown request ${op}`);
        const result = await ops[op](args);
        if (call) port.postMessage({ call, result: result ?? null });
      } catch (e) {
        if (call) port.postMessage({ call, error: message(e) });
      }
    });
    port.onDisconnect.addListener(() => {
      if (session.recorder && session.recorder.state !== 'inactive') session.recorder.stop();
      finish(session);
      unwatch(session);
      for (const c of session.fetches.values()) c.abort();
    });
  });

  globalThis.__smCapture = { list, flash };
})();
