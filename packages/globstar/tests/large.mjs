// Large and deep patterns. JS twin of the size tests in
// `crates/globstar/tests/glob.rs`.

import assert from "node:assert/strict";
import v8 from "node:v8";
import vm from "node:vm";
import { Worker } from "node:worker_threads";
import { compileMatcher } from "../src/index.js";

const lib = JSON.stringify(new URL("../src/index.js", import.meta.url).href);

// Runs `body` (with `compileMatcher` in scope) on a worker with a small
// stack and resolves to what it posts back.
function onSmallStack(body) {
  const code = `
    const { parentPort } = require("node:worker_threads");
    import(${lib}).then(({ compileMatcher }) => parentPort.postMessage((() => { ${body} })()));
  `;
  return new Promise((resolve, reject) => {
    const w = new Worker(code, { eval: true, resourceLimits: { stackSizeMb: 0.5 } });
    w.once("message", resolve);
    w.once("error", reject);
  });
}

// The in-segment closures are built without recursion, so a long
// segment, or a long list under dot=false, fits a small stack.
assert.deepEqual(
  await onSmallStack(`
    const seg = compileMatcher("a" + "*{,}".repeat(2000) + "b");
    const names = Array.from({ length: 2000 }, (_, i) => "*x" + i);
    const list = compileMatcher("{" + names.join(",") + "}", { dot: false });
    return [seg.match("axb"), list.match("ax1999"), list.match(".x1999")];
  `),
  [true, true, false],
);

console.log("✓ large patterns compile on a small stack");

// About 4,200 in-segment states: past the closure budget, each step
// closes its set as it goes.
{
  const list = `{${Array.from({ length: 600 }, (_, i) => `*x${i}`).join(",")}}`;
  for (const [dot, path, want] of [
    [true, "abx123", true],
    [true, "x599", true],
    [true, "x600", false],
    [true, "ax5b", false],
    [true, ".x5", true],
    [false, ".x5", false],
    [false, "a.x5", true],
  ]) {
    assert.equal(compileMatcher(list, { dot }).match(path), want, `${path} dot=${dot}`);
  }
}

// What large patterns keep once compiled. Twin of
// tools/memory-check/tests/large_patterns.rs; heap counts are rougher in
// JS, so the bounds are looser.
v8.setFlagsFromString("--expose-gc");
const gc = vm.runInNewContext("gc");
const used = () => {
  const m = process.memoryUsage();
  return m.heapUsed + m.arrayBuffers;
};
const MB = 1 << 20;
const names = (n, f) => Array.from({ length: n }, (_, i) => f(i));
// The bytes a compiled matcher keeps, and its answer for `path`; the
// matcher is garbage once this returns. Freed array buffers are counted
// a turn later, hence the wait.
async function keeps(patterns, path) {
  gc();
  await new Promise((resolve) => setImmediate(resolve));
  gc();
  const before = used();
  const m = compileMatcher(patterns);
  gc();
  return [used() - before, m.match(path)];
}
for (const { name, patterns, path, matches, maxKept } of [
  // A long name list stays a sorted suffix index.
  {
    name: "20,000 names",
    patterns: names(20_000, (i) => `src/file${i}.ts`),
    path: "src/file14777.ts",
    matches: true,
    maxKept: 16 * MB,
  },
  {
    name: "8,000 names in a brace",
    patterns: `{${names(8_000, (i) => `n${i}`).join(",")}}`,
    path: "n7999",
    matches: true,
    maxKept: 16 * MB,
  },
  // A long literal next to a product is not copied into every suffix.
  {
    name: "{a,b} x10 and 60 KB",
    patterns: `*${"{a,b}".repeat(10)}${"z".repeat(60_000)}`,
    path: "az",
    matches: false,
    maxKept: 16 * MB,
  },
  // A long segment keeps no per-state closures past the budget.
  {
    name: "x*b*b...b, 64 KB",
    patterns: `x${"*b".repeat(32_000)}b`,
    path: "xbb",
    matches: false,
    maxKept: 32 * MB,
  },
  {
    name: "60,000 branches",
    patterns: `x{?${",".repeat(60_000)}}`,
    path: "xa",
    matches: true,
    maxKept: 32 * MB,
  },
]) {
  const [kept, got] = await keeps(patterns, path);
  assert.ok(kept <= maxKept, `${name}: keeps ${kept} bytes`);
  assert.equal(got, matches, name);
}

// A brace with 60,000 branches compiles in milliseconds: its branch
// tails are gathered in one list, not copied once per branch.
{
  const start = performance.now();
  compileMatcher(`x{?${",".repeat(60_000)}}`);
  const ms = performance.now() - start;
  assert.ok(ms < 1000, `60,000 branches took ${ms.toFixed(0)} ms to compile`);
}

// Braces that cross a `/` keep the walk to a few groups: these took
// seconds and gigabytes, or ran out of heap.
for (const [pattern, path] of [
  ["{**,a}/".repeat(80) + "x", "a/x"],
  ["{a,b,/}".repeat(14), "abababa/bababa"],
  ["{/,a,}".repeat(20), "aa/a"],
  ["{a/,}".repeat(1500), "a/a/"],
]) {
  const start = performance.now();
  const m = compileMatcher(pattern);
  const ms = performance.now() - start;
  assert.ok(ms < 200, `${pattern.slice(0, 7)} x${pattern.length}: ${ms.toFixed(0)} ms to compile`);
  assert.ok(m.match(path), pattern.slice(0, 7));
}

// A long name list matches by binary search: 10,000 matches of a
// 20,000-name union take milliseconds, not minutes.
{
  const m = compileMatcher(names(20_000, (i) => `src/file${i}.ts`));
  const start = performance.now();
  for (let i = 0; i < 10_000; i++) assert.ok(m.match(`src/file${(i * 7) % 20_000}.ts`));
  const ms = performance.now() - start;
  assert.ok(ms < 1000, `10,000 matches took ${ms.toFixed(0)} ms`);
}

console.log("✓ large patterns stay small and fast");
