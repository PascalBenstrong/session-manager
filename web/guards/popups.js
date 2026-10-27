// Page guard (runs in the page's own world, before its scripts): blocks pop-ups and new
// tabs that the page opens without a click, or from handlers that catch clicks anywhere on
// the page, which is how pop-under ad scripts work. Links the user clicks keep working.
(() => {
  // Replaced functions report the original's source, like native functions do.
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

  const CLICKS = new Set(['click', 'mousedown', 'mouseup', 'pointerdown', 'pointerup', 'touchstart', 'touchend', 'auxclick']);
  const pageWide = (t) => t === window || t === document || t === document.documentElement || t === document.body;
  let inPageWideHandler = 0;
  let lastClick = 0;
  window.addEventListener('click', (e) => e.isTrusted && (lastClick = Date.now()), true);
  const clicked = () => (navigator.userActivation ? navigator.userActivation.isActive : Date.now() - lastClick < 1000);
  const blocked = () => inPageWideHandler > 0 || !clicked();

  // Handlers of clicks anywhere on the page run marked, so what they open is blocked.
  const wrapped = new WeakMap();
  const wrap = (listener) => {
    let w = wrapped.get(listener);
    if (!w) {
      w = function (e) {
        inPageWideHandler++;
        try {
          return typeof listener === 'function' ? Reflect.apply(listener, this, [e]) : listener.handleEvent(e);
        } finally {
          inPageWideHandler--;
        }
      };
      wrapped.set(listener, w);
    }
    return w;
  };
  intercept(EventTarget.prototype, 'addEventListener', (t, self, args) => {
    if (args[1] && CLICKS.has(args[0]) && pageWide(self)) args = [args[0], wrap(args[1]), ...args.slice(2)];
    return Reflect.apply(t, self, args);
  });
  intercept(EventTarget.prototype, 'removeEventListener', (t, self, args) => {
    if (args[1] && wrapped.has(args[1])) args = [args[0], wrapped.get(args[1]), ...args.slice(2)];
    return Reflect.apply(t, self, args);
  });
  // window.onclick = ..., document.onmousedown = ...
  for (const target of [window, document]) {
    for (const type of CLICKS) {
      const prop = `on${type}`;
      let d;
      for (let o = target; o && !d; o = Object.getPrototypeOf(o)) d = Object.getOwnPropertyDescriptor(o, prop);
      if (!d || !d.set) continue;
      let current = null;
      Object.defineProperty(target, prop, {
        configurable: true,
        enumerable: d.enumerable,
        get: disguise(new Proxy(d.get, { apply: () => current }), d.get),
        set: disguise(
          new Proxy(d.set, {
            apply: (t, self, [v]) => {
              current = typeof v === 'function' ? v : null;
              return Reflect.apply(t, self, [current && wrap(current)]);
            },
          }),
          d.set,
        ),
      });
    }
  }

  intercept(window, 'open', (t, self, args) => (blocked() ? null : Reflect.apply(t, self, args)));
  const newTab = (el) => {
    const target = (el.target || '').toLowerCase();
    return target !== '' && !['_self', '_top', '_parent'].includes(target);
  };
  // Programmatic clicks on links (and form submits) that open a new tab.
  intercept(HTMLElement.prototype, 'click', (t, self, args) =>
    self instanceof HTMLAnchorElement && newTab(self) && blocked() ? undefined : Reflect.apply(t, self, args),
  );
  intercept(EventTarget.prototype, 'dispatchEvent', (t, self, args) => {
    const e = args[0];
    if (self instanceof HTMLAnchorElement && e && e.type === 'click' && newTab(self) && blocked()) return false;
    return Reflect.apply(t, self, args);
  });
  intercept(HTMLFormElement.prototype, 'submit', (t, self, args) => (newTab(self) && blocked() ? undefined : Reflect.apply(t, self, args)));
})();
