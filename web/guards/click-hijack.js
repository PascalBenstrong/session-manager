// Page guard (runs in the page's own world, before its scripts): stops "press play, get an
// ad". Ad scripts put an invisible layer (or a link) over a video player, or listen for
// clicks on the player itself, and open an ad tab on the first click. While a click on a
// player, or on an invisible layer over the page, is handled, the page can't open windows
// or new tabs. A layer that tries is switched off, so the click goes on to the player.
(() => {
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
  };
  // Natives, before the page can change them.
  const { getComputedStyle, Element, HTMLAnchorElement, HTMLVideoElement } = window;
  const matches = Element.prototype.matches;
  const contains = Node.prototype.contains;
  const closest = Element.prototype.closest;
  const rectOf = Element.prototype.getBoundingClientRect;
  const elementsFromPoint = Document.prototype.elementsFromPoint;
  const setProperty = CSSStyleDeclaration.prototype.setProperty;
  const now = () => performance.now();

  const PLAYERS = 'video, iframe, embed, object';
  const area = (r) => Math.max(0, r.width) * Math.max(0, r.height);
  const overlap = (a, b) =>
    area({ width: Math.min(a.right, b.right) - Math.max(a.left, b.left), height: Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) });

  // The outermost ancestor of `el` that doesn't contain `player`: the layer over it.
  const layerOver = (el, player) => {
    let layer = el;
    while (layer.parentElement && !contains.call(layer.parentElement, player) && layer.parentElement !== document.body) {
      layer = layer.parentElement;
    }
    return layer;
  };
  // Nothing visible: see-through, or no background, text, image or media of its own.
  const invisible = (el) => {
    for (let e = el; e && e !== document.documentElement; e = e.parentElement) {
      if (parseFloat(getComputedStyle(e).opacity) < 0.05) return true;
    }
    const cs = getComputedStyle(el);
    const bg = cs.backgroundColor;
    const clear = bg === 'transparent' || /rgba\(.*,\s*0(\.0+)?\)$/.test(bg);
    return clear && cs.backgroundImage === 'none' && !el.textContent.trim() && !el.querySelector('img, svg, canvas, video, picture');
  };

  const rect = (el) => Reflect.apply(rectOf, el, []);
  const viewport = () => ({ left: 0, top: 0, right: innerWidth, bottom: innerHeight, width: innerWidth, height: innerHeight });
  // Covers at least half of `ref` (a rect).
  const big = (el, ref) => overlap(rect(el), ref) >= area(ref) * 0.5;

  // The click being handled: what it landed on, the player under it, and the layer to
  // switch off if the click turns out to open an ad (only when the click landed on a big
  // surface, not on one of the player's buttons).
  let gesture = null;
  const classify = (e) => {
    const path = e.composedPath();
    const target = path[0];
    if (!(target instanceof Element)) return null;
    const g = { target, player: null, layer: null, covering: false, until: now() + 1500, neutralized: false };
    const onPlayer = path.find((n) => n instanceof Element && matches.call(n, PLAYERS));
    if (onPlayer) {
      g.player = onPlayer;
      return g;
    }
    for (const el of Reflect.apply(elementsFromPoint, document, [e.clientX, e.clientY])) {
      if (matches.call(el, PLAYERS) && !contains.call(el, target) && !contains.call(target, el)) {
        g.player = el;
        g.layer = layerOver(target, el);
        g.covering = big(target, rect(el));
        return g;
      }
    }
    // No player: an invisible layer over most of the page.
    let layer = target;
    while (layer.parentElement && layer.parentElement !== document.body && layer.parentElement !== document.documentElement) {
      layer = layer.parentElement;
    }
    const pos = getComputedStyle(layer).position;
    if ((pos === 'fixed' || pos === 'absolute') && big(target, viewport()) && invisible(layer)) {
      g.layer = layer;
      g.covering = true;
      return g;
    }
    return null;
  };
  const hijacked = () => gesture !== null && now() < gesture.until;

  // Switches off the layer the click landed on, so later clicks reach what's under it.
  const neutralize = () => {
    const g = gesture;
    if (!g || g.neutralized || !g.layer || !g.covering) return;
    g.neutralized = true;
    setProperty.call(g.layer.style, 'pointer-events', 'none', 'important');
    if (g.player) setProperty.call(g.layer.style, 'visibility', 'hidden', 'important');
  };
  // What the user meant: start the video they clicked on.
  const playThrough = () => {
    const g = gesture;
    if (g && g.neutralized && g.player instanceof HTMLVideoElement && g.player.paused) {
      const p = g.player.play();
      if (p && p.catch) p.catch(() => {});
    }
  };
  const block = () => {
    neutralize();
    return true;
  };

  addEventListener(
    'pointerdown',
    (e) => {
      if (!e.isTrusted || e.button !== 0) return;
      gesture = classify(e);
    },
    true,
  );
  for (const type of ['mousedown', 'pointerup', 'mouseup', 'touchend']) {
    addEventListener(type, (e) => e.isTrusted && hijacked() && (gesture.until = now() + 1500), true);
  }
  // A real click on a link over the player (or on an invisible layer) that opens a new tab.
  addEventListener(
    'click',
    (e) => {
      if (!e.isTrusted || !hijacked()) return;
      gesture.until = now() + 1500;
      const a = Reflect.apply(closest, gesture.target, ['a[href], area[href]']);
      const modified = e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0;
      // Only a link stretched over the player (or the page), or one you can't see: the
      // player's own visible links ("Watch on ...") keep working.
      const ref = gesture.player ? rect(gesture.player) : viewport();
      if (a && newTab(a) && !modified && (big(a, ref) || invisible(a))) {
        // A link around the video itself just doesn't open; one laid over it is switched off.
        if (!gesture.layer && !(gesture.player && contains.call(a, gesture.player))) {
          gesture.layer = a;
          gesture.covering = true;
        }
        e.preventDefault();
        block();
      }
      const g = gesture;
      setTimeout(() => g === gesture && playThrough(), 0);
    },
    true,
  );

  const newTab = (el) => {
    const target = (el.target || '').toLowerCase();
    return target !== '' && !['_self', '_top', '_parent'].includes(target);
  };
  intercept(window, 'open', (t, self, args) => (hijacked() && block() ? null : Reflect.apply(t, self, args)));
  intercept(HTMLElement.prototype, 'click', (t, self, args) =>
    self instanceof HTMLAnchorElement && newTab(self) && hijacked() && block() ? undefined : Reflect.apply(t, self, args),
  );
  intercept(EventTarget.prototype, 'dispatchEvent', (t, self, args) => {
    const e = args[0];
    if (self instanceof HTMLAnchorElement && e && e.type === 'click' && newTab(self) && hijacked() && block()) return false;
    return Reflect.apply(t, self, args);
  });
  intercept(HTMLFormElement.prototype, 'submit', (t, self, args) => (newTab(self) && hijacked() && block() ? undefined : Reflect.apply(t, self, args)));
})();
