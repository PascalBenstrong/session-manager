// Inspector content script, injected into the top frame of an inspected tab when an
// inspector opens. It runs in the extension's isolated world, so the page can't see it, and
// answers the inspector's questions about the DOM (tree, details, picking, source, copying,
// screenshots). Inspectors connect a port (tabs.connect, name "sm-inspector") to hear about
// picked elements and live changes to the page. It also hosts the inspector docked at the
// bottom of the page (an iframe of inspector.html).
(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  if (!api || globalThis.__smInspector) return;
  globalThis.__smInspector = true;
  // This document (the network log keeps the requests of each page apart).
  const DOC = crypto.randomUUID();

  // Our own elements in the page (highlight overlay, dock), hidden from the inspector.
  const OURS = new Set(['sm-inspector-overlay', 'sm-inspector-dock']);
  const ours = (n) => n && n.nodeType === Node.ELEMENT_NODE && OURS.has(n.localName);
  // Other nodes we add (the dock's spacing style).
  const mine = new WeakSet();

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
    if (!n) throw new Error('that element is gone (the page changed)');
    return n;
  };

  const shown = (n) =>
    (n.nodeType === Node.ELEMENT_NODE && !ours(n) && !mine.has(n)) ||
    (n.nodeType === Node.TEXT_NODE && n.textContent.trim() !== '') ||
    n.nodeType === Node.COMMENT_NODE;
  const kids = (n) => {
    const list = [...n.childNodes];
    if (n.shadowRoot) list.unshift(n.shadowRoot);
    return list.filter((c) => shown(c) || c.nodeType === Node.DOCUMENT_FRAGMENT_NODE);
  };
  const clip = (s, max) => (s.length > max ? `${s.slice(0, max)}…` : s);

  const summary = (n) => {
    const base = { id: idOf(n), kind: 'element', tag: '', label: '', children: 0, text: '', src: '' };
    if (n.nodeType === Node.ELEMENT_NODE) {
      const attrs = [...n.attributes].map((a) => (a.value ? `${a.name}="${clip(a.value, 60)}"` : a.name)).join(' ');
      const src = n.localName === 'iframe' || n.localName === 'frame' ? frameUrl(n) : '';
      return { ...base, tag: n.localName, label: attrs, children: kids(n).length, src };
    }
    if (n.nodeType === Node.DOCUMENT_FRAGMENT_NODE) return { ...base, kind: 'shadow', tag: '#shadow-root', children: kids(n).length };
    if (n.nodeType === Node.COMMENT_NODE) return { ...base, kind: 'comment', text: clip(n.textContent.trim(), 200) };
    return { ...base, kind: 'text', text: clip(n.textContent.trim(), 200) };
  };

  // Where a frame is now (it may have navigated since its src was set), else its src.
  const frameUrl = (f) => {
    try {
      const href = f.contentWindow?.location.href;
      if (href && href !== 'about:blank') return href;
    } catch {
      // Cross-origin.
    }
    return f.src || '';
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

  const xpathOf = (n) => {
    const parts = [];
    for (let e = n; e && e.nodeType !== Node.DOCUMENT_NODE; e = e.parentNode) {
      if (e.nodeType === Node.ELEMENT_NODE) {
        if (e.id && document.querySelectorAll(`#${CSS.escape(e.id)}`).length === 1) {
          parts.unshift(`//*[@id="${e.id}"]`);
          return parts.join('/');
        }
        const same = e.parentNode ? [...e.parentNode.children].filter((c) => c.localName === e.localName) : [];
        parts.unshift(same.length > 1 ? `${e.localName}[${same.indexOf(e) + 1}]` : e.localName);
      } else if (e.nodeType === Node.TEXT_NODE) {
        const texts = [...e.parentNode.childNodes].filter((c) => c.nodeType === Node.TEXT_NODE);
        parts.unshift(texts.length > 1 ? `text()[${texts.indexOf(e) + 1}]` : 'text()');
      } else {
        return '';
      }
    }
    return `/${parts.join('/')}`;
  };

  const path = (n) => {
    const out = [];
    for (let e = n; e && e !== document; e = e.parentNode || e.host) out.unshift(summary(e));
    return out;
  };

  const details = (id) => {
    const n = node(id);
    selected = n;
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

  // What the inspector's context menu copies.
  const copy = ({ id, what, name }) => {
    const n = node(id);
    const el = n.nodeType === Node.ELEMENT_NODE ? n : null;
    switch (what) {
      case 'outerHtml':
        return el ? el.outerHTML : n.textContent;
      case 'innerHtml':
        return el ? el.innerHTML : n.textContent;
      case 'text':
        return el ? el.innerText || el.textContent : n.textContent;
      case 'selector':
        return el ? selectorOf(el) : selectorOf(n.parentElement);
      case 'jsPath':
        return `document.querySelector(${JSON.stringify(el ? selectorOf(el) : selectorOf(n.parentElement))})`;
      case 'xpath':
        return xpathOf(n);
      case 'attributes':
        return el ? [...el.attributes].map((a) => (a.value ? `${a.name}="${a.value}"` : a.name)).join(' ') : '';
      case 'attribute':
        return el ? el.getAttribute(name) ?? '' : '';
      case 'styles': {
        if (!el) return '';
        const cs = getComputedStyle(el);
        const lines = [];
        for (let i = 0; i < cs.length; i++) lines.push(`${cs[i]}: ${cs.getPropertyValue(cs[i])};`);
        return lines.join('\n');
      }
      case 'url':
        return el && (el.localName === 'iframe' || el.localName === 'frame') ? frameUrl(el) : el?.currentSrc || el?.src || el?.href || '';
      case 'selection': {
        // The page's selected text, within this element.
        const sel = getSelection();
        if (!sel || sel.isCollapsed) return '';
        const parts = [];
        for (let i = 0; i < sel.rangeCount; i++) {
          const r = sel.getRangeAt(i);
          if (!r.intersectsNode(n)) continue;
          const mine = document.createRange();
          mine.selectNodeContents(n);
          if (r.compareBoundaryPoints(Range.START_TO_START, mine) > 0) mine.setStart(r.startContainer, r.startOffset);
          if (r.compareBoundaryPoints(Range.END_TO_END, mine) < 0) mine.setEnd(r.endContainer, r.endOffset);
          parts.push(mine.toString());
        }
        return parts.join('\n');
      }
      default:
        throw new Error(`can't copy ${what}`);
    }
  };

  // ------------------------------------------------------------------ highlight

  // Overlay in a closed shadow root so page styles don't touch it. It follows the element
  // while the page scrolls or moves it.
  let host = null;
  let box = null;
  let tag = null;
  let highlighted = null;
  let selected = null;
  const overlay = () => {
    if (!host) {
      host = document.createElement('sm-inspector-overlay');
      const root = host.attachShadow({ mode: 'closed' });
      box = document.createElement('div');
      box.style.cssText =
        'position:fixed;pointer-events:none;z-index:2147483646;background:rgba(124,108,246,.25);' +
        'border:1px solid #7c6cf6;box-sizing:border-box';
      tag = document.createElement('div');
      tag.style.cssText =
        'position:fixed;pointer-events:none;z-index:2147483646;background:#2b2540;color:#fff;font:11px/1.4 system-ui,sans-serif;' +
        'padding:2px 6px;border-radius:4px;white-space:nowrap;max-width:60vw;overflow:hidden;text-overflow:ellipsis';
      root.append(box, tag);
    }
    if (!host.isConnected) document.documentElement.appendChild(host);
    return box;
  };
  const label = (el, r) => {
    let s = el.localName;
    if (el.id) s += `#${el.id}`;
    const cls = [...el.classList].slice(0, 3).join('.');
    if (cls) s += `.${cls}`;
    return `${s}  ${Math.round(r.width)} × ${Math.round(r.height)}`;
  };
  const place = () => {
    if (!highlighted || !highlighted.isConnected) return;
    const r = highlighted.getBoundingClientRect();
    const b = overlay();
    Object.assign(b.style, { left: `${r.x}px`, top: `${r.y}px`, width: `${r.width}px`, height: `${r.height}px` });
    tag.textContent = label(highlighted, r);
    const above = r.y > 22;
    Object.assign(tag.style, { left: `${Math.max(0, r.x)}px`, top: above ? `${r.y - 21}px` : `${Math.min(innerHeight - 20, r.bottom + 2)}px` });
  };
  let following = 0;
  const follow = () => {
    place();
    following = highlighted ? requestAnimationFrame(follow) : 0;
  };
  const highlight = (el) => {
    if (el && el.nodeType === Node.TEXT_NODE) el = el.parentElement;
    if (!el || el.nodeType !== Node.ELEMENT_NODE) {
      highlighted = null;
      host?.remove();
      return;
    }
    highlighted = el;
    place();
    if (!following) following = requestAnimationFrame(follow);
  };

  // ------------------------------------------------------------------ inspectors (ports)

  const ports = new Set();
  const post = (msg) => {
    for (const p of ports) {
      try {
        p.postMessage(msg);
      } catch {
        ports.delete(p);
      }
    }
  };

  // Live view: which listed nodes changed (their children must be read again), and whether
  // the selected element (or something in it) did.
  const listed = new Set();
  const changed = new Set();
  let detailsChanged = false;
  let flush = 0;
  const known = (n) => n && ids.get(n);
  const observer = new MutationObserver((records) => {
    for (const r of records) {
      const moved = [...r.addedNodes, ...r.removedNodes];
      if (ours(r.target) || mine.has(r.target.parentNode) || (moved.length && moved.every((n) => ours(n) || mine.has(n)))) continue;
      const owner = r.type === 'childList' ? r.target : r.target.parentNode;
      const id = known(owner);
      if (id && listed.has(id)) changed.add(id);
      if (selected && (selected === r.target || selected.contains(r.target))) detailsChanged = true;
    }
    if ((changed.size || detailsChanged) && !flush) {
      flush = setTimeout(() => {
        flush = 0;
        post({ changed: [...changed], details: detailsChanged, title: document.title });
        changed.clear();
        detailsChanged = false;
      }, 250);
    }
  });
  const watch = () => {
    if (ports.size) observer.observe(document, { childList: true, subtree: true, attributes: true, characterData: true });
    else observer.disconnect();
  };

  api.runtime.onConnect.addListener((port) => {
    if (port.name !== 'sm-inspector') return;
    ports.add(port);
    watch();
    port.onDisconnect.addListener(() => {
      ports.delete(port);
      watch();
      if (!ports.size) {
        endPick();
        highlight(null);
      }
    });
  });

  // ------------------------------------------------------------------ picking

  // Hover highlights, click selects (the page never sees that click).
  let picking = false;
  const stop = (e) => {
    e.preventDefault();
    e.stopImmediatePropagation();
  };
  const target = (e) => {
    const t = e.composedPath()[0];
    return ours(t) ? null : t;
  };
  const onMove = (e) => {
    const t = target(e);
    if (t) highlight(t);
  };
  const onClick = (e) => {
    const el = target(e);
    if (!el) return;
    stop(e);
    endPick();
    highlight(el);
    post({ picked: { id: idOf(el), path: path(el).map((s) => s.id) } });
  };
  const onKey = (e) => {
    if (e.key === 'Escape') {
      stop(e);
      endPick();
      highlight(null);
      post({ pickCancelled: true });
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

  // ------------------------------------------------------------------ dock

  // The inspector docked at the bottom of the page: an iframe of inspector.html in a closed
  // shadow root, with a bar to resize it. The page gets as much room below its content, so
  // nothing is hidden behind it for good.
  let dock = null;
  const dockHeight = () => Math.max(120, Math.min(innerHeight - 80, dock?.height || Math.round(innerHeight * 0.4)));
  const layoutDock = () => {
    if (!dock) return;
    const h = dockHeight();
    dock.host.style.setProperty('height', `${h}px`, 'important');
    dock.spacer.textContent = `html { margin-bottom: ${h}px !important; }`;
  };
  const openDock = ({ url, height }) => {
    if (dock) return dock.token;
    const token = crypto.randomUUID();
    const el = document.createElement('sm-inspector-dock');
    el.style.cssText =
      'all:initial;position:fixed !important;left:0 !important;right:0 !important;bottom:0 !important;' +
      'z-index:2147483647 !important;display:block !important;box-shadow:0 -2px 10px rgba(0,0,0,.25)';
    const root = el.attachShadow({ mode: 'closed' });
    const bar = document.createElement('div');
    bar.title = 'Drag to resize the inspector';
    bar.style.cssText = 'height:5px;cursor:ns-resize;background:#7c6cf6';
    const frame = document.createElement('iframe');
    frame.src = `${url}${url.includes('?') ? '&' : '?'}dock=${token}`;
    frame.allow = 'clipboard-write; clipboard-read';
    frame.style.cssText = 'display:block;border:0;width:100%;height:calc(100% - 5px);background:#1b1726';
    root.append(bar, frame);
    const spacer = document.createElement('style');
    mine.add(spacer);
    dock = { host: el, frame, spacer, token, height: height || 0 };
    // Resize by dragging the bar (the iframe would swallow the moves, so it's blocked meanwhile).
    bar.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      bar.setPointerCapture(e.pointerId);
      frame.style.pointerEvents = 'none';
      const move = (ev) => {
        dock.height = innerHeight - ev.clientY;
        layoutDock();
      };
      const up = () => {
        frame.style.pointerEvents = '';
        bar.removeEventListener('pointermove', move);
        bar.removeEventListener('pointerup', up);
        api.storage.local.set({ smDockHeight: dockHeight() }).catch?.(() => {});
      };
      bar.addEventListener('pointermove', move);
      bar.addEventListener('pointerup', up);
    });
    addEventListener('resize', layoutDock);
    (document.head || document.documentElement).appendChild(spacer);
    document.documentElement.appendChild(el);
    layoutDock();
    return token;
  };
  const closeDock = () => {
    if (!dock) return;
    dock.host.remove();
    dock.spacer.remove();
    removeEventListener('resize', layoutDock);
    dock = null;
  };

  // ------------------------------------------------------------------ screenshots

  const frames = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  let shot = null;
  // Hides our overlay and dock and scrolls the element into view. Returns where it is, in
  // CSS pixels: `rect` in the viewport and `page` in the document.
  const shotStart = async ({ id }) => {
    let el = node(id);
    if (el.nodeType === Node.TEXT_NODE) el = el.parentElement;
    if (!el || el.nodeType !== Node.ELEMENT_NODE) throw new Error('only elements can be photographed');
    shot = { el, x: scrollX, y: scrollY, highlighted };
    highlight(null);
    if (dock) dock.host.style.setProperty('visibility', 'hidden', 'important');
    let r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) throw new Error('the element has no size (it is hidden)');
    if (r.top < 0 || r.bottom > innerHeight || r.left < 0 || r.right > innerWidth) {
      el.scrollIntoView({ block: r.height > innerHeight ? 'start' : 'center', inline: 'nearest' });
    }
    await frames();
    return measure();
  };
  const measure = () => {
    const r = shot.el.getBoundingClientRect();
    return {
      rect: { x: r.x, y: r.y, width: r.width, height: r.height },
      page: { x: r.x + scrollX, y: r.y + scrollY, width: r.width, height: r.height },
      viewport: { width: innerWidth, height: innerHeight },
      dpr: devicePixelRatio,
    };
  };
  // Scrolls the page by `dy` CSS pixels (for tall elements captured in slices).
  const shotScroll = async ({ dy }) => {
    scrollBy(0, dy);
    await frames();
    return measure();
  };
  const shotEnd = () => {
    if (!shot) return;
    scrollTo(shot.x, shot.y);
    if (dock) dock.host.style.removeProperty('visibility');
    highlight(shot.highlighted);
    shot = null;
  };

  // ------------------------------------------------------------------ network

  // Records from the page-world recorder (web/net-recorder.js), passed on to inspectors in
  // batches. `net.channel` is the event name the recorder answers on.
  const net = { channel: null, batch: [], timer: 0, ready: false };
  const onNet = (e) => {
    let rec;
    try {
      rec = JSON.parse(String(e.detail));
    } catch {
      return;
    }
    if (rec.ready) {
      net.ready = true;
      return;
    }
    if (net.batch.length < 5000) net.batch.push(rec);
    if (!net.timer) {
      net.timer = setTimeout(() => {
        net.timer = 0;
        if (ports.size) post({ net: net.batch.splice(0) });
      }, 250);
    }
  };
  // Greets the recorder with a new channel; true if one is in the page.
  const netHello = ({ token }) => {
    if (net.channel) document.removeEventListener(net.channel, onNet);
    net.channel = token;
    net.ready = false;
    document.addEventListener(token, onNet);
    document.dispatchEvent(new CustomEvent('sm-net-hello', { detail: token }));
    return net.ready;
  };
  // Marks this tab as recording (the recorder registered for the site checks it), or not.
  const netFlag = ({ on }) => {
    try {
      if (on) sessionStorage.setItem('smNetRecording', '1');
      else sessionStorage.removeItem('smNetRecording');
    } catch {
      // No storage (sandboxed page): only this page is recorded.
    }
  };
  const netCmd = ({ cmd }) => {
    if (!net.channel) throw new Error('not recording');
    document.dispatchEvent(new CustomEvent(`${net.channel}:cmd`, { detail: JSON.stringify(cmd) }));
  };

  // ------------------------------------------------------------------ requests

  const handlers = {
    root: () => ({ title: document.title, url: location.href, node: summary(document.documentElement), doc: DOC }),
    children: ({ id }) => {
      listed.add(id);
      return kids(node(id)).map(summary);
    },
    details: ({ id }) => details(id),
    highlight: ({ id }) => highlight(id ? node(id) : null),
    pick: () => startPick(),
    cancelPick: () => (endPick(), highlight(null)),
    query: ({ selector }) => [...document.querySelectorAll(selector)].filter((n) => !ours(n)).slice(0, 500).map(summary),
    // A node and the ids of its ancestors (to open the tree down to it).
    reveal: ({ id }) => path(node(id)).map((s) => s.id),
    source: () => {
      const dt = document.doctype ? `<!DOCTYPE ${document.doctype.name}>\n` : '';
      const clone = document.documentElement.cloneNode(true);
      for (const n of clone.querySelectorAll([...OURS].join(','))) n.remove();
      return dt + clone.outerHTML;
    },
    remove: ({ id }) => {
      highlight(null);
      node(id).remove();
    },
    scrollTo: ({ id }) => {
      const n = node(id);
      (n.nodeType === Node.ELEMENT_NODE ? n : n.parentElement)?.scrollIntoView?.({ block: 'center' });
      highlight(n);
    },
    copy,
    // The page's selected text (anywhere).
    pageSelection: () => getSelection()?.toString() || '',
    dock: ({ url }) =>
      api.storage.local.get('smDockHeight').then(
        (s) => openDock({ url, height: s?.smDockHeight }),
        () => openDock({ url }),
      ),
    undock: () => closeDock(),
    dockToken: () => dock?.token ?? null,
    shotStart,
    shotScroll,
    shotEnd: () => shotEnd(),
    netHello,
    netCmd,
    netFlag,
  };

  api.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || !msg.smInspect) return undefined;
    const { op, ...args } = msg.smInspect;
    const fail = (e) => sendResponse({ ok: false, error: String(e && e.message ? e.message : e) });
    try {
      const h = handlers[op];
      if (!h) throw new Error(`unknown request ${op}`);
      const r = h(args);
      if (r && typeof r.then === 'function') {
        r.then((v) => sendResponse({ ok: true, result: v ?? null }), fail);
        return true;
      }
      sendResponse({ ok: true, result: r ?? null });
    } catch (e) {
      fail(e);
    }
    return undefined;
  });
})();
