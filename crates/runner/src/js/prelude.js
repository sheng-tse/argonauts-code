// What a workflow script may use: agent, parallel, pipeline, phase, log, workflow, check, budget
// and args, on top of plain JavaScript. Everything else the engine offers is taken away, so a
// script can't read a clock, roll dice, or reach anything but the host's one door.
(function (host, limits) {
  'use strict';

  const STANDARD = new Set([
    'globalThis', 'Object', 'Function', 'Array', 'Number', 'Boolean', 'String', 'Symbol', 'BigInt',
    'Math', 'JSON', 'Reflect', 'Proxy', 'Promise', 'RegExp', 'Date', 'Map', 'Set', 'WeakMap',
    'WeakSet', 'Error', 'AggregateError', 'EvalError', 'RangeError', 'ReferenceError',
    'SyntaxError', 'TypeError', 'URIError', 'ArrayBuffer', 'DataView', 'Int8Array', 'Uint8Array',
    'Uint8ClampedArray', 'Int16Array', 'Uint16Array', 'Int32Array', 'Uint32Array',
    'Float32Array', 'Float64Array', 'BigInt64Array', 'BigUint64Array', 'Iterator', 'Infinity',
    'NaN', 'undefined', 'parseFloat', 'parseInt', 'isFinite', 'isNaN', 'decodeURI',
    'decodeURIComponent', 'encodeURI', 'encodeURIComponent', 'escape', 'unescape',
  ]);
  for (const name of Object.getOwnPropertyNames(globalThis)) {
    if (STANDARD.has(name)) continue;
    try {
      delete globalThis[name];
    } catch (_) {
      globalThis[name] = undefined;
    }
  }

  const replays = (what) => {
    throw new Error(
      what + " isn't available in a workflow, since a run must replay the same way. " +
      'Pass a time in through args, and vary a prompt by its index.'
    );
  };
  Math.random = () => replays('Math.random()');
  // The real constructor loses its clock itself, since a script can reach it past any stand-in:
  // through a subclass's prototype, `Date.prototype.constructor`, or by calling it bare.
  const RealDate = Date;
  RealDate.now = () => replays('Date.now()');
  const WorkflowDate = new Proxy(RealDate, {
    apply: () => replays('Date()'),
    construct: (target, parts, newTarget) =>
      parts.length ? Reflect.construct(target, parts, newTarget) : replays('new Date()'),
  });
  Object.defineProperty(RealDate.prototype, 'constructor', {
    value: WorkflowDate,
    writable: false,
    configurable: false,
  });
  globalThis.Date = WorkflowDate;

  const send = (kind, payload) => host(kind, JSON.stringify(payload === undefined ? null : payload));
  const waiting = new Map();
  const ask = (kind, payload) => {
    const id = send(kind, payload);
    return new Promise((resolve, reject) => waiting.set(id, { resolve, reject }));
  };
  const settle = (id, ok, json) => {
    const call = waiting.get(id);
    if (!call) return;
    waiting.delete(id);
    if (ok) call.resolve(json === undefined || json === '' ? null : JSON.parse(json));
    else call.reject(new Error(json));
  };

  const text = (value, what) => {
    if (typeof value !== 'string' || !value.trim()) throw new TypeError(what + ' takes text');
    return value;
  };
  const list = (value, what) => {
    if (!Array.isArray(value)) throw new TypeError(what + ' takes a list');
    if (value.length > limits.items) {
      throw new RangeError(`${what} takes at most ${limits.items} items, and got ${value.length}`);
    }
    return value;
  };
  const options = (value, what) => {
    if (value === undefined || value === null) return {};
    if (typeof value !== 'object' || Array.isArray(value)) {
      throw new TypeError(what + ' takes an object such as { role, label, schema }');
    }
    return value;
  };

  let current = null;
  const api = {
    phase(title) {
      current = text(title, 'phase(title)');
      send('phase', { title: current });
    },
    log(message) {
      send('log', { message: String(message) });
    },
    agent(prompt, opts) {
      text(prompt, 'agent(prompt)');
      const given = options(opts, 'agent(prompt, opts)');
      const phase = given.phase === undefined ? current : given.phase;
      return ask('agent', Object.assign({}, given, { prompt, phase }));
    },
    // A barrier: every task ends before it returns, and one that throws is a null.
    parallel(tasks) {
      const started = list(tasks, 'parallel(tasks)').map((task) => {
        try {
          return Promise.resolve(typeof task === 'function' ? task() : task).catch(() => null);
        } catch (_) {
          return Promise.resolve(null);
        }
      });
      return Promise.all(started);
    },
    // No barrier: each item goes through every stage by itself.
    pipeline(items, ...stages) {
      const through = async (item, index) => {
        let value = item;
        try {
          for (const stage of stages) value = await stage(value, item, index);
          return value;
        } catch (_) {
          return null;
        }
      };
      return Promise.all(list(items, 'pipeline(items)').map(through));
    },
    workflow(ref, args) {
      return ask('workflow', { ref, args });
    },
    // Whether a condition holds: { holds, failed, stop }. A step's until and a file's goal.
    check(condition, about) {
      const given = options(about, 'check(condition, about)');
      return ask('check', Object.assign({}, given, { condition: text(condition, 'check(condition)') }));
    },
    budget: Object.freeze({
      total: limits.budget == null ? null : limits.budget,
      spent: () => Number(send('spent')),
      remaining() {
        return this.total === null ? Infinity : Math.max(0, this.total - this.spent());
      },
    }),
  };
  for (const [name, value] of Object.entries(api)) {
    Object.defineProperty(globalThis, name, { value, writable: false, configurable: false });
  }

  return { settle, send };
})
