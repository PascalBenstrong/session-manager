// Network recorder for the inspector (runs in the page's own world, injected when the
// inspector starts recording, or at document start for "Reload and record"). It wraps
// fetch, XMLHttpRequest, WebSocket and sendBeacon, reports each request (and WebSocket
// message) to the inspector content script, and replays them on request:
// - XHR: sent again with the page's own handlers, so the page processes the new response;
// - WebSocket: received messages are delivered to the page again, sent ones sent again;
// - fetch / beacon: sent again from the page (same cookies); the page doesn't see the reply.
// Channel: the content script greets it with a random event name ("sm-net-hello"); records
// are JSON strings in CustomEvents of that name, commands come as `${name}:cmd`.
(() => {
  // Registered for a recording tab's site, it also runs in the site's other tabs: only the
  // tab whose inspector is recording has the flag (set by web/inspect.js).
  try {
    if (sessionStorage.getItem('smNetRecording') !== '1') return;
  } catch {
    // No storage (sandboxed page): it was injected on purpose.
  }
  // Natives, before the page changes them.
  const { CustomEvent, Request, Response, TextDecoder, Blob, FormData, URLSearchParams, Map, WeakMap, JSON, Date } = window;
  const docDispatch = EventTarget.prototype.dispatchEvent;
  const addListener = EventTarget.prototype.addEventListener;
  const removeListener = EventTarget.prototype.removeEventListener;
  const then = Promise.prototype.then;
  const stringify = JSON.stringify;
  const parse = JSON.parse;
  const now = () => Date.now();

  const nativeToString = Function.prototype.toString;
  const disguised = new WeakMap();
  const toString = new Proxy(nativeToString, {
    apply: (t, self, args) => Reflect.apply(t, disguised.get(self) ?? self, args),
  });
  disguised.set(toString, nativeToString);
  Function.prototype.toString = toString;
  const disguise = (fake, real) => (disguised.set(fake, real), fake);
  const intercept = (obj, name, apply) => {
    const real = obj[name];
    if (typeof real === 'function') obj[name] = disguise(new Proxy(real, { apply }), real);
    return real;
  };

  const BODY_CAP = 256 * 1024;
  const KEEP = 1000; // requests and messages kept for replays
  const KEEP_BYTES = 32 * 1024 * 1024; // their bodies and messages, in all
  const FRAME_KEEP_MAX = 64 * 1024; // binary messages larger than this aren't kept
  let channel = null;
  let recording = true;
  const buffer = [];
  let nextId = 1;
  // About how much memory a body or message holds.
  const sizeOf = (b) => {
    if (b == null) return 0;
    if (typeof b === 'string') return b.length * 2;
    if (b instanceof Blob) return b.size;
    if (b instanceof ArrayBuffer || ArrayBuffer.isView(b)) return b.byteLength;
    return 0;
  };
  // id -> what a replay needs (insertion order: oldest first).
  const kept = new Map();
  let keptBytes = 0;
  const keep = (id, v, bytes = 0) => {
    v.bytes = bytes;
    kept.set(id, v);
    keptBytes += bytes;
    while (kept.size > KEEP || (keptBytes > KEEP_BYTES && kept.size > 1)) {
      const [oldest, k] = kept.entries().next().value;
      kept.delete(oldest);
      keptBytes -= k.bytes;
    }
  };
  // The request being replayed (its new record says so).
  let replaying = 0;
  // A recorded request of an earlier page being sent again (its record says so).
  let resending = false;
  // Whether a new request is recorded (otherwise the wrappers stay out of the way).
  const active = () => recording || replaying !== 0 || resending;

  const emit = (rec) => {
    if (!recording && !rec.replayOf && !rec.resent && !rec.update) return;
    if (!channel) {
      if (buffer.length < 3000) buffer.push(rec);
      return;
    }
    Reflect.apply(docDispatch, document, [new CustomEvent(channel, { detail: stringify(rec) })]);
  };

  const clip = (s) => (s.length > BODY_CAP ? `${s.slice(0, BODY_CAP)}\n… (${s.length - BODY_CAP} more characters)` : s);
  // A request body for display: [text, note].
  const describe = (b) => {
    if (b == null) return [null, ''];
    if (typeof b === 'string') return [clip(b), ''];
    if (b instanceof URLSearchParams) return [clip(b.toString()), 'form (URL-encoded)'];
    if (b instanceof FormData) {
      const lines = [];
      for (const [k, v] of b.entries()) lines.push(typeof v === 'string' ? `${k}=${v}` : `${k}=[file ${v.name}, ${v.size} bytes]`);
      return [clip(lines.join('\n')), 'multipart form'];
    }
    if (b instanceof Blob) return [null, `blob, ${b.type || 'no type'}, ${b.size} bytes`];
    if (b instanceof ArrayBuffer || ArrayBuffer.isView(b)) return [null, `binary, ${b.byteLength} bytes`];
    if (typeof ReadableStream !== 'undefined' && b instanceof ReadableStream) return [null, 'stream (not replayable)'];
    if (b instanceof Document) return [clip(new XMLSerializer().serializeToString(b)), 'document'];
    return [clip(String(b)), ''];
  };
  const stack = () => {
    const lines = (new Error().stack || '').split('\n').filter((l) => l.trim() && !/^Error\b/.test(l));
    return lines.slice(2, 10).join('\n');
  };
  const headerList = (h) => {
    const out = [];
    try {
      new Headers(h).forEach((v, k) => out.push([k, v]));
    } catch {}
    return out;
  };
  const textual = (ct) => /json|text|xml|javascript|html|csv|urlencoded|graphql|svg/i.test(ct);

  // ---------------------------------------------------------------- fetch

  const nativeFetch = window.fetch;
  intercept(window, 'fetch', (t, self, args) => {
    if (!active()) return Reflect.apply(t, self, args);
    const id = nextId++;
    const replayOf = replaying;
    const [input, init] = args;
    let url = '';
    let method = 'GET';
    let headers = [];
    let body = init && 'body' in init ? init.body : null;
    try {
      if (input instanceof Request) {
        url = input.url;
        method = input.method;
        headers = headerList(input.headers);
        if (body == null && input.body) body = '[request body]';
      } else {
        url = new URL(String(input), location.href).href;
      }
      if (init && init.method) method = String(init.method).toUpperCase();
      if (init && init.headers) headers = headerList(init.headers);
    } catch {}
    const [text, note] = typeof body === 'string' && body === '[request body]' ? [null, 'request body'] : describe(body);
    const streamed = typeof ReadableStream !== 'undefined' && body instanceof ReadableStream;
    keep(id, { kind: 'fetch', input: input instanceof Request ? input.clone() : input, init }, sizeOf(init && init.body));
    emit({ id, kind: 'fetch', method, url, reqHeaders: headers, reqBody: text, reqBodyNote: note, start: now(), stack: stack(), replayOf, resent: resending, replayable: !streamed });
    const p = Reflect.apply(t, self, args);
    Reflect.apply(then, p, [
      (res) => {
        const ct = res.headers.get('content-type') || '';
        const len = +res.headers.get('content-length') || 0;
        const done = { update: true, id, status: res.status, statusText: res.statusText, resHeaders: headerList(res.headers), mime: ct, size: len, end: now(), responseUrl: res.url };
        if (res.type === 'opaque' || !res.body || !textual(ct) || len > BODY_CAP * 4) {
          emit({ ...done, resBodyNote: res.type === 'opaque' ? 'opaque (no-cors) response' : ct ? `${ct}${len ? `, ${len} bytes` : ''}` : '' });
          return;
        }
        // Read a copy, up to the cap (the page reads its own).
        const reader = res.clone().body.getReader();
        const chunks = [];
        let read = 0;
        const pump = () =>
          reader.read().then(({ value, done: end }) => {
            if (value) {
              chunks.push(value);
              read += value.length;
            }
            if (end || read >= BODY_CAP) {
              if (!end) reader.cancel().catch(() => {});
              const all = new Uint8Array(read);
              let at = 0;
              for (const c of chunks) {
                all.set(c, at);
                at += c.length;
              }
              emit({ ...done, size: len || read, end: now(), resBody: clip(new TextDecoder().decode(all)) });
              return;
            }
            return pump();
          });
        pump().catch(() => emit(done));
      },
      (e) => emit({ update: true, id, error: String((e && e.message) || e), end: now() }),
    ]);
    return p;
  });

  // ---------------------------------------------------------------- XMLHttpRequest

  const XHR = XMLHttpRequest.prototype;
  const xhrMeta = new WeakMap();
  const xhrListeners = new WeakMap();
  const replayOfXhr = new WeakMap();
  const HANDLERS = ['onreadystatechange', 'onloadstart', 'onprogress', 'onload', 'onloadend', 'onerror', 'onabort', 'ontimeout'];
  intercept(XHR, 'open', (t, self, args) => {
    if (!active() && !replayOfXhr.has(self)) {
      xhrMeta.delete(self);
      return Reflect.apply(t, self, args);
    }
    xhrMeta.set(self, { method: String(args[0] || 'GET').toUpperCase(), url: args[1], async: args.length < 3 || !!args[2], user: args[3], password: args[4], headers: [] });
    return Reflect.apply(t, self, args);
  });
  intercept(XHR, 'setRequestHeader', (t, self, args) => {
    xhrMeta.get(self)?.headers.push([String(args[0]), String(args[1])]);
    return Reflect.apply(t, self, args);
  });
  intercept(XHR, 'send', (t, self, args) => {
    const m = xhrMeta.get(self);
    if (!m) return Reflect.apply(t, self, args);
    const id = nextId++;
    const replayOf = replayOfXhr.get(self) || 0;
    m.body = args[0] ?? null;
    let url = String(m.url);
    try {
      url = new URL(url, location.href).href;
    } catch {}
    const [text, note] = describe(m.body);
    keep(id, { kind: 'xhr', xhr: self, meta: m }, sizeOf(m.body));
    emit({ id, kind: 'xhr', method: m.method, url, reqHeaders: m.headers, reqBody: text, reqBodyNote: note, start: now(), stack: stack(), replayOf, replayable: true });
    Reflect.apply(addListener, self, [
      'loadend',
      () => {
        const ct = self.getResponseHeader('content-type') || '';
        const done = { update: true, id, status: self.status, statusText: self.statusText, mime: ct, end: now(), responseUrl: self.responseURL };
        done.resHeaders = self
          .getAllResponseHeaders()
          .trim()
          .split(/[\r\n]+/)
          .filter(Boolean)
          .map((l) => [l.slice(0, l.indexOf(':')).trim(), l.slice(l.indexOf(':') + 1).trim()]);
        // A replay needs only these, not the request (and its response).
        const k = kept.get(id);
        if (k && k.xhr === self) {
          const snap = { withCredentials: self.withCredentials, responseType: self.responseType, timeout: self.timeout };
          for (const h of HANDLERS) snap[h] = self[h];
          xhrListeners.set(snap, (xhrListeners.get(self) || []).slice());
          k.xhr = snap;
        }
        try {
          const rt = self.responseType;
          // Too big to show: not turned into text just to be cut.
          const huge = +self.getResponseHeader('content-length') > BODY_CAP * 4;
          if (rt === '' || rt === 'text') done.resBody = clip(self.responseText);
          else if ((rt === 'json' || rt === 'document') && huge) done.resBodyNote = `${rt}, ${+self.getResponseHeader('content-length')} bytes`;
          else if (rt === 'json') done.resBody = clip(stringify(self.response));
          else if (rt === 'document' && self.response) done.resBody = clip(new XMLSerializer().serializeToString(self.response));
          else done.resBodyNote = `${rt}${self.response && self.response.byteLength != null ? `, ${self.response.byteLength} bytes` : ''}`;
          done.size = done.resBody ? done.resBody.length : (self.response && (self.response.byteLength ?? self.response.size)) || 0;
        } catch {}
        if (self.status === 0) done.error = 'failed (network error, blocked or aborted)';
        emit(done);
      },
    ]);
    return Reflect.apply(t, self, args);
  });
  // Listeners the page adds to its requests (a replay gets them too).
  intercept(EventTarget.prototype, 'addEventListener', (t, self, args) => {
    if (self instanceof XMLHttpRequest && args[1]) {
      let list = xhrListeners.get(self);
      if (!list) xhrListeners.set(self, (list = []));
      list.push(args);
    }
    return Reflect.apply(t, self, args);
  });
  const replayXhr = (k, done) => {
    const { xhr: orig, meta: m } = k;
    const x = new XMLHttpRequest();
    replayOfXhr.set(x, k.id);
    // Through the wrapped methods, so the replay is recorded too.
    x.open(m.method, m.url, true, m.user, m.password);
    for (const [name, value] of m.headers) x.setRequestHeader(name, value);
    try {
      x.withCredentials = orig.withCredentials;
      x.responseType = orig.responseType;
      x.timeout = orig.timeout;
    } catch {}
    for (const h of HANDLERS) if (typeof orig[h] === 'function') x[h] = orig[h];
    for (const args of xhrListeners.get(orig) || []) Reflect.apply(addListener, x, args);
    Reflect.apply(addListener, x, ['loadend', done]);
    x.send(m.body);
  };

  // ---------------------------------------------------------------- WebSocket

  const NativeWS = window.WebSocket;
  const sockets = new WeakMap();
  let inbound = 0; // a received message being replayed
  // Tracks a socket: made while recording, or (made before) as soon as the page sends on it.
  const adopt = (ws, protocols, late) => {
    const id = nextId++;
    sockets.set(ws, id);
    keep(id, { kind: 'ws', ws });
    emit({ id, kind: 'ws', method: 'GET', url: ws.url, start: now(), stack: late ? '' : stack(), replayable: false, status: ws.readyState === 1 ? 101 : 0,
      reqBodyNote: late ? 'opened before recording started (messages it got before are missing)' : protocols ? `protocols: ${[].concat(protocols).join(', ')}` : '' });
    Reflect.apply(addListener, ws, ['open', () => emit({ update: true, id, status: 101, statusText: 'open', resBodyNote: ws.protocol ? `protocol ${ws.protocol}` : '' })]);
    Reflect.apply(addListener, ws, ['close', (e) => emit({ update: true, id, statusText: `closed (${e.code}${e.reason ? `: ${e.reason}` : ''})`, end: now() })]);
    Reflect.apply(addListener, ws, ['error', () => emit({ update: true, id, error: 'connection error' })]);
    Reflect.apply(addListener, ws, ['message', (e) => frame(ws, 'in', e.data, inbound)]);
    return id;
  };
  if (NativeWS) {
    window.WebSocket = disguise(
      new Proxy(NativeWS, {
        construct(t, args, nt) {
          const ws = Reflect.construct(t, args, nt);
          // Made while not recording: adopted when the page sends on it while recording.
          if (active()) adopt(ws, args[1], false);
          return ws;
        },
      }),
      NativeWS,
    );
    intercept(NativeWS.prototype, 'send', (t, self, args) => {
      if (!active()) return Reflect.apply(t, self, args);
      if (!sockets.has(self)) adopt(self, null, true);
      frame(self, 'out', args[0], replaying);
      return Reflect.apply(t, self, args);
    });
  }
  const frame = (ws, dir, data, replayOf) => {
    const socket = sockets.get(ws);
    if (!socket || (!recording && !replayOf)) return;
    const id = nextId++;
    const [text, note] = describe(data);
    const bytes = sizeOf(data);
    const keepIt = typeof data === 'string' || bytes <= FRAME_KEEP_MAX;
    if (keepIt) keep(id, { kind: 'frame', ws, dir, data }, bytes);
    emit({ id, kind: 'frame', socket, dir, method: dir === 'in' ? '↓' : '↑', url: ws.url, reqBody: text, reqBodyNote: note, start: now(), end: now(), replayOf: replayOf || 0, replayable: keepIt });
  };

  // ---------------------------------------------------------------- sendBeacon

  if (navigator.sendBeacon) {
    intercept(Navigator.prototype, 'sendBeacon', (t, self, args) => {
      if (!active()) return Reflect.apply(t, self, args);
      const id = nextId++;
      const [text, note] = describe(args[1]);
      let url = String(args[0]);
      try {
        url = new URL(url, location.href).href;
      } catch {}
      keep(id, { kind: 'beacon', url: args[0], data: args[1] }, sizeOf(args[1]));
      const ok = Reflect.apply(t, self, args);
      emit({ id, kind: 'beacon', method: 'POST', url, reqBody: text, reqBodyNote: note, start: now(), end: now(), status: ok ? 0 : -1, statusText: ok ? 'queued' : 'refused', stack: stack(), replayOf: replaying, resent: resending, replayable: true });
      return ok;
    });
  }

  // ---------------------------------------------------------------- replay

  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const replayOne = (id) =>
    new Promise((resolve) => {
      const k = kept.get(id);
      if (!k) return resolve(false);
      k.id = id;
      const timer = setTimeout(() => resolve(true), 15000);
      const done = () => {
        clearTimeout(timer);
        resolve(true);
      };
      replaying = id;
      try {
        if (k.kind === 'xhr') replayXhr(k, done);
        else if (k.kind === 'fetch') {
          const input = k.input instanceof Request ? k.input.clone() : k.input;
          Reflect.apply(then, window.fetch(input, k.init), [done, done]);
        } else if (k.kind === 'beacon') {
          navigator.sendBeacon(k.url, k.data);
          done();
        } else if (k.kind === 'frame' && k.dir === 'out') {
          if (k.ws.readyState === 1) k.ws.send(k.data);
          done();
        } else if (k.kind === 'frame') {
          // Delivered to the page's listeners as if the server sent it again.
          inbound = id;
          try {
            let origin = '';
            try {
              origin = new URL(k.ws.url).origin.replace(/^http/, 'ws');
            } catch {}
            Reflect.apply(docDispatch, k.ws, [new MessageEvent('message', { data: k.data, origin })]);
          } finally {
            inbound = 0;
          }
          done();
        } else done();
      } catch (e) {
        emit({ update: true, id, replayError: String((e && e.message) || e) });
        done();
      } finally {
        replaying = 0;
      }
    });
  // Steps: ids of this page's requests (replayed with the page's handlers), or recorded
  // requests of an earlier page ({ method, url, headers, body, kind }) sent again.
  const replay = async (steps, gap) => {
    for (const [i, step] of steps.entries()) {
      if (i > 0 && gap > 0) await wait(gap);
      if (typeof step === 'number') await replayOne(step);
      else await resendOne(step);
    }
    emit({ update: true, id: 0, replayDone: steps.length });
  };

  // A request of an earlier page in the tab, sent again from what was recorded.
  const resendOne = async (r) => {
    try {
      const init = { method: r.method, headers: r.headers || [], credentials: 'include' };
      if (r.body != null && r.method !== 'GET' && r.method !== 'HEAD') init.body = r.body;
      resending = true;
      let p = null;
      try {
        if (r.kind === 'beacon') navigator.sendBeacon(r.url, r.body ?? undefined);
        else p = window.fetch(r.url, init);
      } finally {
        resending = false;
      }
      if (p) await new Promise((ok) => Reflect.apply(then, p, [ok, ok]));
    } catch {}
  };

  const commands = (e) => {
    let c;
    try {
      c = parse(String(e.detail));
    } catch {
      return;
    }
    if (c.cmd === 'start') recording = true;
    else if (c.cmd === 'stop') recording = false;
    else if (c.cmd === 'clear') {
      kept.clear();
      keptBytes = 0;
    }
    else if (c.cmd === 'replay') replay(c.ids || [], Math.max(0, +c.gap || 0));
  };
  // The content script says hello with the channel name; we answer on it, then send what
  // was recorded before.
  Reflect.apply(addListener, document, [
    'sm-net-hello',
    (e) => {
      const name = String(e.detail || '');
      if (!/^sm-net-[\w-]{20,}$/.test(name)) return;
      if (channel) Reflect.apply(removeListener, document, [`${channel}:cmd`, commands]);
      channel = name;
      Reflect.apply(addListener, document, [`${channel}:cmd`, commands]);
      Reflect.apply(docDispatch, document, [new CustomEvent(channel, { detail: stringify({ ready: true }) })]);
      for (const rec of buffer.splice(0)) emit(rec);
    },
  ]);
})();
