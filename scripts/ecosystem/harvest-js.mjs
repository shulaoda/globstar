// The cases a JS library's own test suite exercises. picomatch and
// micromatch are mocha suites, run as they are under a fake mocha with the
// library replaced by a recorder that notes every (pattern, path, options)
// and answers from the real library, so computed fixtures and loops are
// harvested too. minimatch's `test/patterns.js` is a table, read directly.

import Module from "node:module";
import { readdirSync } from "node:fs";
import { join, relative } from "node:path";
import { pathToFileURL } from "node:url";

// `options` as the suite passed them; `at` is `file:line` in the suite;
// `group` names the test.
function makeCase(pattern, path, options, at, group) {
  return { pattern, path, options: options ?? {}, at, group };
}

// Every `test/*.js` of `dir`. `wrap(real, record)` builds the recorder
// that replaces `require('..')`; `extra` maps other module requests to
// replacements.
export async function harvestMocha({ dir, real, wrap, extra = {} }) {
  const testDir = join(dir, "test");
  const files = readdirSync(testDir)
    .filter((f) => f.endsWith(".js"))
    .sort()
    .map((f) => join(testDir, f));
  const cases = [];
  const skipped = { arrayPattern: 0, nonString: 0 };
  let current = { file: "", group: [] };

  const here = () => {
    const stack = new Error().stack.split("\n");
    for (const line of stack) {
      const m = line.match(/\((.*?):(\d+):\d+\)/) ?? line.match(/at (.*?):(\d+):\d+/);
      if (m && m[1].startsWith(testDir) && !m[1].includes("/support/")) {
        return `${relative(dir, m[1])}:${m[2]}`;
      }
    }
    return relative(dir, current.file);
  };
  const record = (pattern, path, options) => {
    if (Array.isArray(pattern)) {
      skipped.arrayPattern++;
      return;
    }
    if (typeof pattern !== "string" || typeof path !== "string") {
      skipped.nonString++;
      return;
    }
    cases.push(makeCase(pattern, path, options, here(), current.group.join(" › ")));
  };
  const recorder = wrap(real, record);

  // Enough of mocha: `describe` collects, the tree runs afterwards.
  const makeSuite = (title) => ({
    title,
    suites: [],
    tests: [],
    hooks: { before: [], beforeEach: [], after: [], afterEach: [] },
  });
  let root = makeSuite("");
  let stack = [root];
  const top = () => stack[stack.length - 1];
  const describe = (title, fn) => {
    const suite = makeSuite(title);
    top().suites.push(suite);
    stack.push(suite);
    try {
      fn.call({});
    } finally {
      stack.pop();
    }
  };
  const it = (title, fn) => top().tests.push({ title, fn });
  const noop = () => {};
  describe.skip = noop;
  describe.only = describe;
  it.skip = noop;
  it.only = it;
  const hook = (name) => (fn) => top().hooks[name].push(fn);
  const mocha = {
    describe,
    it,
    context: describe,
    specify: it,
    xdescribe: noop,
    xit: noop,
    before: hook("before"),
    beforeEach: hook("beforeEach"),
    after: hook("after"),
    afterEach: hook("afterEach"),
  };

  const SKIP = Symbol("skip");
  const ctx = {
    timeout: noop,
    slow: noop,
    retries: noop,
    skip() {
      throw SKIP;
    },
  };
  const call = async (fn) => {
    if (fn.length >= 1) {
      await Promise.race([
        new Promise((resolve) => fn.call(ctx, () => resolve())),
        new Promise((resolve) => setTimeout(resolve, 2000)),
      ]);
    } else {
      await fn.call(ctx);
    }
  };
  let failures = 0;
  const run = async (suite, chain) => {
    const path = [...chain, suite];
    for (const fn of suite.hooks.before) await call(fn).catch(() => failures++);
    for (const test of suite.tests) {
      current.group = path
        .map((s) => s.title)
        .filter(Boolean)
        .concat(test.title);
      for (const s of path)
        for (const fn of s.hooks.beforeEach) await call(fn).catch(() => failures++);
      try {
        await call(test.fn);
      } catch (e) {
        if (e !== SKIP) failures++;
      }
      for (const s of path)
        for (const fn of s.hooks.afterEach) await call(fn).catch(() => failures++);
    }
    for (const child of suite.suites) await run(child, path);
    for (const fn of suite.hooks.after) await call(fn).catch(() => failures++);
  };

  // An `assert` that stops nothing: a failing upstream assertion must not
  // hide the calls after it.
  const lenientAssert = (() => {
    const real = Module.createRequire(import.meta.url)("node:assert");
    const lenient = (...a) => {
      try {
        real(...a);
      } catch {}
    };
    for (const k of Object.keys(real)) {
      lenient[k] =
        typeof real[k] === "function"
          ? (...a) => {
              try {
                return real[k](...a);
              } catch {}
            }
          : real[k];
    }
    return lenient;
  })();

  const originalLoad = Module._load;
  Module._load = function (request, parent, isMain) {
    const from = parent?.filename ?? "";
    if (from.startsWith(dir)) {
      if (
        request === ".." ||
        request === "../.." ||
        request === "../index" ||
        request === "../index.js"
      ) {
        return recorder;
      }
      if (request === "assert" || request === "node:assert") return lenientAssert;
      if (request === "mocha") return mocha;
      if (request in extra) return extra[request];
    }
    return originalLoad.call(this, request, parent, isMain);
  };
  const previous = {};
  for (const k of Object.keys(mocha)) {
    previous[k] = globalThis[k];
    globalThis[k] = mocha[k];
  }
  try {
    for (const file of files) {
      root = makeSuite("");
      stack = [root];
      current = { file, group: [] };
      try {
        Module.createRequire(file)(file);
      } catch {
        failures++;
        continue;
      }
      await run(root, []);
    }
  } finally {
    Module._load = originalLoad;
    for (const k of Object.keys(mocha)) globalThis[k] = previous[k];
  }
  return { cases, skipped, failures, files: files.length };
}

// picomatch: `picomatch(pattern, options)` returns a matcher;
// `isMatch(path, pattern, options)` answers directly.
export function wrapPicomatch(real, record) {
  const recorder = (pattern, options, returnState) => {
    const matcher = real(pattern, options, returnState);
    return (input, returnObject) => {
      record(pattern, input, options);
      return matcher(input, returnObject);
    };
  };
  Object.assign(recorder, real);
  recorder.isMatch = (input, pattern, options) => {
    record(pattern, input, options);
    return real.isMatch(input, pattern, options);
  };
  recorder.test = (input, regex, options, extra) => real.test(input, regex, options, extra);
  return recorder;
}

// micromatch: list in, matches out at the top, plus the predicates.
// `contains` is substring matching, passed through unrecorded.
export function wrapMicromatch(real, record) {
  const each = (list, patterns, options) => {
    for (const item of [].concat(list)) record(patterns, item, options);
  };
  const recorder = (list, patterns, options) => {
    each(list, patterns, options);
    return real(list, patterns, options);
  };
  Object.assign(recorder, real);
  for (const name of ["match", "not", "some", "every"]) {
    recorder[name] = (list, patterns, options) => {
      each(list, patterns, options);
      return real[name](list, patterns, options);
    };
  }
  for (const name of ["isMatch", "any", "all"]) {
    recorder[name] = (input, patterns, options) => {
      record(patterns, input, options);
      return real[name](input, patterns, options);
    };
  }
  recorder.matcher = (pattern, options) => {
    const m = real.matcher(pattern, options);
    return (input) => {
      record(pattern, input, options);
      return m(input);
    };
  };
  return recorder;
}

// `test/patterns.js`: a string starts a section, a function changes the
// file list, an entry is `[pattern, matches, options, files, tapOptions]`.
// The matches column is minimatch's own claim; the generator asks the
// library instead.
export async function harvestMinimatch(dir) {
  const file = join(dir, "test", "patterns.js");
  const { default: patterns } = await import(pathToFileURL(file).href);
  const cases = [];
  let group = "";
  let index = 0;
  for (const entry of patterns) {
    index++;
    if (typeof entry === "function") {
      entry();
      continue;
    }
    if (typeof entry === "string") {
      group = entry;
      continue;
    }
    const [pattern, , options = {}, files, tapOptions = {}] = entry;
    if (tapOptions.skip) continue;
    for (const path of files ?? patterns.files) {
      cases.push(makeCase(pattern, path, options, `test/patterns.js#${index}`, group));
    }
  }
  return { cases, skipped: {}, failures: 0, files: 1 };
}
