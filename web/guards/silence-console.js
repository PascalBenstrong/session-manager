// Page guard: the page's console calls do nothing. Detection tricks log objects whose
// getters or toString only run when the developer tools format them; unlogged, they never run.
(() => {
  const nativeToString = Function.prototype.toString;
  const disguised = new WeakMap();
  const toString = new Proxy(nativeToString, {
    apply: (t, self, args) => Reflect.apply(t, disguised.get(self) ?? self, args),
  });
  disguised.set(toString, nativeToString);
  Function.prototype.toString = toString;
  const disguise = (fake, real) => (disguised.set(fake, real), fake);

  const names = ['log', 'debug', 'info', 'warn', 'error', 'table', 'dir', 'dirxml', 'trace', 'clear', 'count', 'countReset',
    'assert', 'group', 'groupCollapsed', 'groupEnd', 'time', 'timeLog', 'timeEnd', 'timeStamp', 'profile', 'profileEnd'];
  for (const name of names) {
    const real = console[name];
    if (typeof real === 'function') console[name] = disguise(new Proxy(real, { apply: () => undefined }), real);
  }
})();
