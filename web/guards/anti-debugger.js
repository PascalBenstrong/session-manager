// Page guard: removes `debugger` statements from code the page builds at runtime, the
// usual developer tools trap: Function("debugger"), (function () {}).constructor("debugger"),
// setInterval("debugger", 50). Statements written in script files are removed by the
// Firefox script filter (stripDebuggerBytes in platform-common.js).
(() => {
  const nativeToString = Function.prototype.toString;
  const disguised = new WeakMap();
  const toString = new Proxy(nativeToString, {
    apply: (t, self, args) => Reflect.apply(t, disguised.get(self) ?? self, args),
  });
  disguised.set(toString, nativeToString);
  Function.prototype.toString = toString;
  const disguise = (fake, real) => (disguised.set(fake, real), fake);

  const strip = (s) => (typeof s === 'string' && s.includes('debugger') ? s.replace(/\bdebugger\b/g, '') : s);
  const guard = (fn) => {
    const safe = new Proxy(fn, {
      apply: (t, self, args) => Reflect.apply(t, self, args.map(strip)),
      construct: (t, args, newTarget) => Reflect.construct(t, args.map(strip), newTarget === safe ? t : newTarget),
    });
    return disguise(safe, fn);
  };
  const protos = [
    Function.prototype,
    Object.getPrototypeOf(function* () {}),
    Object.getPrototypeOf(async function () {}),
    Object.getPrototypeOf(async function* () {}),
  ];
  for (const proto of protos) {
    const Ctor = proto.constructor;
    const safe = guard(Ctor);
    Object.defineProperty(proto, 'constructor', { ...Object.getOwnPropertyDescriptor(proto, 'constructor'), value: safe });
    if (Ctor === Function) window.Function = safe;
  }
  for (const name of ['setTimeout', 'setInterval']) {
    const real = window[name];
    window[name] = disguise(
      new Proxy(real, { apply: (t, self, args) => Reflect.apply(t, self, [strip(args[0]), ...args.slice(1)]) }),
      real,
    );
  }
})();
