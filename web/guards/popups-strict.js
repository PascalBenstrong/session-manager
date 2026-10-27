// Page guard for sites with an "all pop-ups" rule: scripts can't open windows or new tabs
// at all. Links the user clicks keep working.
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

  const newTab = (el) => {
    const target = (el.target || '').toLowerCase();
    return target !== '' && !['_self', '_top', '_parent'].includes(target);
  };
  intercept(window, 'open', () => null);
  intercept(HTMLElement.prototype, 'click', (t, self, args) =>
    self instanceof HTMLAnchorElement && newTab(self) ? undefined : Reflect.apply(t, self, args),
  );
  intercept(EventTarget.prototype, 'dispatchEvent', (t, self, args) =>
    self instanceof HTMLAnchorElement && args[0] && args[0].type === 'click' && newTab(self) ? false : Reflect.apply(t, self, args),
  );
  intercept(HTMLFormElement.prototype, 'submit', (t, self, args) => (newTab(self) ? undefined : Reflect.apply(t, self, args)));
})();
