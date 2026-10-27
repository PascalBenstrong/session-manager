// Page guard: hides the developer tools from pages that look for them.
// - Docked developer tools make the window's outer size much larger than its inner size;
//   the page sees a normal difference.
// - Pages can't take away the developer tools shortcuts (F12, Ctrl+Shift+I/J/C, Ctrl+U).
// Detection through the console (getters that run when it formats objects) is stopped by
// silence-console.js, and timing `debugger` statements by anti-debugger.js.
(() => {
  const nativeToString = Function.prototype.toString;
  const disguised = new WeakMap();
  const toString = new Proxy(nativeToString, {
    apply: (t, self, args) => Reflect.apply(t, disguised.get(self) ?? self, args),
  });
  disguised.set(toString, nativeToString);
  Function.prototype.toString = toString;
  const disguise = (fake, real) => (disguised.set(fake, real), fake);

  const normalSize = (prop, inner, max) => {
    let d;
    for (let o = window; o && !d; o = Object.getPrototypeOf(o)) d = Object.getOwnPropertyDescriptor(o, prop);
    if (!d || !d.get) return;
    const get = disguise(
      new Proxy(d.get, {
        apply: (t, self) => {
          const outer = Reflect.apply(t, self, []);
          const inside = window[inner];
          return outer - inside > max ? inside + max : outer;
        },
      }),
      d.get,
    );
    Object.defineProperty(window, prop, { configurable: true, enumerable: true, get, set: d.set });
  };
  normalSize('outerWidth', 'innerWidth', 16);
  normalSize('outerHeight', 'innerHeight', 150);

  const devtoolsKey = (e) => {
    const mod = e.ctrlKey || e.metaKey;
    const k = (e.key || '').toLowerCase();
    return e.key === 'F12' || (mod && (e.shiftKey || e.altKey) && ['i', 'j', 'c'].includes(k)) || (mod && !e.shiftKey && k === 'u');
  };
  for (const type of ['keydown', 'keypress', 'keyup']) {
    window.addEventListener(type, (e) => devtoolsKey(e) && e.stopImmediatePropagation(), true);
  }
})();
