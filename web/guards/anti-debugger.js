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

  // Only `debugger` statements become `;` (an empty statement, so `if (x) debugger` keeps
  // its meaning). The word is left alone as a property or method name (`a.debugger`,
  // `{ debugger: 1 }`, `debugger() {}`), in a longer name, and right next to a quote
  // (`"debugger"`). Same rules as stripDebuggerBytes in platform-common.js.
  const IDENT = /[\w$#]|[^\x00-\x7f]/;
  const QUOTE = /['"`]/;
  const SPACE = /\s/;
  const strip = (s) => {
    if (typeof s !== 'string' || !s.includes('debugger')) return s;
    let out = '';
    let last = 0;
    for (let i = s.indexOf('debugger'); i !== -1; i = s.indexOf('debugger', i + 8)) {
      const before = s[i - 1] ?? ' ';
      const after = s[i + 8] ?? ' ';
      if (IDENT.test(before) || QUOTE.test(before) || IDENT.test(after) || QUOTE.test(after)) continue;
      let p = i - 1;
      while (p >= 0 && SPACE.test(s[p])) p--;
      let n = i + 8;
      let newline = false;
      for (; n < s.length && SPACE.test(s[n]); n++) newline ||= s[n] === '\n' || s[n] === '\r';
      if (s[p] === '.' || s[n] === ':' || (s[n] === '(' && !newline)) continue;
      out += `${s.slice(last, i)};`;
      last = i + 8;
    }
    return last ? out + s.slice(last) : s;
  };
  // If the page's code only compiles with the word left in, it runs as it is: a trap may
  // stay, but the page's own code never breaks.
  const NativeFunction = Function;
  const compiles = (code) => {
    try {
      Reflect.construct(NativeFunction, [code]);
      return true;
    } catch {
      return false;
    }
  };
  const run = (args, make) => {
    const stripped = args.map(strip);
    if (stripped.every((a, i) => a === args[i])) return make(args);
    try {
      return make(stripped);
    } catch (e) {
      if (e instanceof SyntaxError) return make(args);
      throw e;
    }
  };
  const guard = (fn) => {
    const safe = new Proxy(fn, {
      apply: (t, self, args) => run(args, (a) => Reflect.apply(t, self, a)),
      construct: (t, args, newTarget) => run(args, (a) => Reflect.construct(t, a, newTarget === safe ? t : newTarget)),
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
      new Proxy(real, {
        apply: (t, self, args) => {
          const code = strip(args[0]);
          return Reflect.apply(t, self, [code !== args[0] && compiles(code) ? code : args[0], ...args.slice(1)]);
        },
      }),
      real,
    );
  }
})();
