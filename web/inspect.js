// Inspector content script, injected into the top frame of an inspected tab when the
// inspector window opens. It runs in the extension's isolated world, so the page can't see
// it, and answers the inspector's questions about the DOM (tree, details, picking, source).
(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  if (!api || globalThis.__smInspector) return;
  globalThis.__smInspector = true;

  // Nodes get stable numeric ids for the inspector.
  const ids = new WeakMap();
  const nodes = new Map();
  let nextId = 1;
  const idOf = (n) => {
    let id = ids.get(n);
    if (!id) {
      id = nextId++;
      ids.set(n, id);
      nodes.set(id, new WeakRef(n));
    }
    return id;
  };
  const node = (id) => {
    const n = nodes.get(id)?.deref();
    if (!n) throw new Error('that element is gone (the page changed); refresh the tree');
    return n;
  };

  const shown = (n) =>
    n.nodeType === Node.ELEMENT_NODE ||
    (n.nodeType === Node.TEXT_NODE && n.textContent.trim() !== '') ||
    n.nodeType === Node.COMMENT_NODE;
  const kids = (n) => {
    const list = [...n.childNodes];
    if (n.shadowRoot) list.unshift(n.shadowRoot);
    return list.filter((c) => shown(c) || c.nodeType === Node.DOCUMENT_FRAGMENT_NODE);
  };
  const clip = (s, max) => (s.length > max ? `${s.slice(0, max)}…` : s);

  const summary = (n) => {
    const base = { id: idOf(n), kind: 'element', tag: '', label: '', children: 0, text: '' };
    if (n.nodeType === Node.ELEMENT_NODE) {
      const attrs = [...n.attributes].map((a) => (a.value ? `${a.name}="${clip(a.value, 60)}"` : a.name)).join(' ');
      return { ...base, tag: n.localName, label: attrs, children: kids(n).length };
    }
    if (n.nodeType === Node.DOCUMENT_FRAGMENT_NODE) return { ...base, kind: 'shadow', tag: '#shadow-root', children: kids(n).length };
    if (n.nodeType === Node.COMMENT_NODE) return { ...base, kind: 'comment', text: clip(n.textContent.trim(), 200) };
    return { ...base, kind: 'text', text: clip(n.textContent.trim(), 200) };
  };

  // A CSS selector for an element (ids where possible, else nth-of-type steps).
  const selectorOf = (el) => {
    const parts = [];
    for (let e = el; e && e.nodeType === Node.ELEMENT_NODE; e = e.parentElement) {
      if (e.id && document.querySelectorAll(`#${CSS.escape(e.id)}`).length === 1) {
        parts.unshift(`#${CSS.escape(e.id)}`);
        break;
      }
      let step = e.localName;
      const same = e.parentElement ? [...e.parentElement.children].filter((c) => c.localName === e.localName) : [];
      if (same.length > 1) step += `:nth-of-type(${same.indexOf(e) + 1})`;
      parts.unshift(step);
    }
    return parts.join(' > ');
  };

  const path = (n) => {
    const out = [];
    for (let e = n; e && e !== document; e = e.parentNode || e.host) out.unshift(summary(e));
    return out;
  };

  const details = (id) => {
    const n = node(id);
    const out = { node: summary(n), path: path(n) };
    if (n.nodeType !== Node.ELEMENT_NODE) return { ...out, outerHtml: n.textContent };
    const r = n.getBoundingClientRect();
    const cs = getComputedStyle(n);
    const style = [];
    for (let i = 0; i < cs.length; i++) style.push([cs[i], cs.getPropertyValue(cs[i])]);
    return {
      ...out,
      attributes: [...n.attributes].map((a) => [a.name, a.value]),
      rect: { x: r.x, y: r.y, width: r.width, height: r.height },
      selector: selectorOf(n),
      style,
      outerHtml: clip(n.outerHTML, 200000),
      text: clip(n.innerText || '', 20000),
    };
  };

  // Highlight overlay, in a closed shadow root so page styles don't touch it.
  let host = null;
  let box = null;
  const overlay = () => {
    if (!host) {
      host = document.createElement('sm-inspector-overlay');
      const root = host.attachShadow({ mode: 'closed' });
      box = document.createElement('div');
      box.style.cssText =
        'position:fixed;pointer-events:none;z-index:2147483647;background:rgba(124,108,246,.25);' +
        'border:1px solid #7c6cf6;box-sizing:border-box;font:11px system-ui;color:#fff';
      root.appendChild(box);
    }
    if (!host.isConnected) document.documentElement.appendChild(host);
    return box;
  };
  const highlight = (el) => {
    if (!el || el.nodeType !== Node.ELEMENT_NODE) {
      host?.remove();
      return;
    }
    const r = el.getBoundingClientRect();
    const b = overlay();
    Object.assign(b.style, { left: `${r.x}px`, top: `${r.y}px`, width: `${r.width}px`, height: `${r.height}px` });
  };

  // Picking: hover highlights, click selects (the page never sees that click).
  let picking = false;
  const stop = (e) => {
    e.preventDefault();
    e.stopImmediatePropagation();
  };
  const onMove = (e) => highlight(e.composedPath()[0]);
  const onClick = (e) => {
    stop(e);
    const el = e.composedPath()[0];
    endPick();
    highlight(el);
    api.runtime.sendMessage({ smInspectPicked: { id: idOf(el), path: path(el).map((s) => s.id) } }).catch?.(() => {});
  };
  const onKey = (e) => {
    if (e.key === 'Escape') {
      stop(e);
      endPick();
      highlight(null);
    }
  };
  const startPick = () => {
    if (picking) return;
    picking = true;
    addEventListener('mousemove', onMove, true);
    addEventListener('click', onClick, true);
    for (const t of ['mousedown', 'mouseup', 'pointerdown', 'pointerup']) addEventListener(t, stop, true);
    addEventListener('keydown', onKey, true);
  };
  const endPick = () => {
    picking = false;
    removeEventListener('mousemove', onMove, true);
    removeEventListener('click', onClick, true);
    for (const t of ['mousedown', 'mouseup', 'pointerdown', 'pointerup']) removeEventListener(t, stop, true);
    removeEventListener('keydown', onKey, true);
  };

  const handlers = {
    root: () => ({ title: document.title, url: location.href, node: summary(document.documentElement) }),
    children: ({ id }) => kids(node(id)).map(summary),
    details: ({ id }) => details(id),
    highlight: ({ id }) => highlight(id ? node(id) : null),
    pick: () => startPick(),
    cancelPick: () => (endPick(), highlight(null)),
    query: ({ selector }) => [...document.querySelectorAll(selector)].slice(0, 500).map(summary),
    source: () => {
      const dt = document.doctype ? `<!DOCTYPE ${document.doctype.name}>\n` : '';
      return dt + document.documentElement.outerHTML;
    },
    remove: ({ id }) => {
      highlight(null);
      node(id).remove();
    },
    scrollTo: ({ id }) => {
      const n = node(id);
      n.scrollIntoView?.({ block: 'center' });
      highlight(n);
    },
  };

  api.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || !msg.smInspect) return undefined;
    const { op, ...args } = msg.smInspect;
    try {
      const h = handlers[op];
      if (!h) throw new Error(`unknown request ${op}`);
      sendResponse({ ok: true, result: h(args) ?? null });
    } catch (e) {
      sendResponse({ ok: false, error: String(e && e.message ? e.message : e) });
    }
    return undefined;
  });
})();
