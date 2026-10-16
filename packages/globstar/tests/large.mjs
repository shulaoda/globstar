// Large and deep patterns. JS twin of the size tests in
// `crates/globstar/tests/glob.rs`.

import assert from "node:assert/strict";
import v8 from "node:v8";
import vm from "node:vm";
import { Worker } from "node:worker_threads";
import { compileMatcher, DirMatch } from "../src/index.js";

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

// A union too big for the chains agrees with its members: its graph NFA
// keeps dense rows at 100 members and goes sparse at 600. Twin of
// `big_unions_agree_with_their_members` in crates/globstar/tests/glob_union.rs.
{
  const bigUnion = (n) =>
    Array.from({ length: n }, (_, i) => {
      const [a, b] = [i % 37, i % 11];
      return [
        `a${a}/**`,
        `**/f${a}.ts`,
        `b${b}/a${a}/*`,
        `{a,b}${a}/f${b}`,
        `**/c${a}/**/d${b}`,
        `f${a}/**/b${b}`,
        `*${a}/a${b}`,
      ][i % 7];
    });
  const segs = ["a3", "b2", "f5.ts", "c7", "d1", "x", "a30", ".a", "b10", "f36"];
  const paths = [""];
  for (let depth = 1; depth <= 3; depth++) {
    for (let i = 0; i < 200; i++) {
      paths.push(
        Array.from({ length: depth }, (_, k) => segs[Math.floor(i / 10 ** k) % segs.length]).join(
          "/",
        ),
      );
    }
  }
  for (const n of [100, 600]) {
    const patterns = bigUnion(n);
    const members = patterns.map((p) => compileMatcher(p));
    const union = compileMatcher(patterns);
    for (const path of paths) {
      const any = (f) => members.some(f);
      assert.equal(
        union.match(path),
        any((g) => g.match(path)),
        `${n}: ${path}`,
      );
      const dir = union.matchDir(path);
      assert.equal(
        DirMatch.matches(dir),
        any((g) => DirMatch.matches(g.matchDir(path))),
        `${n}: ${path}`,
      );
      assert.equal(
        DirMatch.mayMatchBelow(dir),
        any((g) => DirMatch.mayMatchBelow(g.matchDir(path))),
        `${n}: ${path}`,
      );
    }
  }
}

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
  // A big union's graph NFA keeps no dense rows.
  {
    name: "20,000 dir/file members",
    patterns: names(20_000, (i) => `dir${i}/file${i}`),
    path: "dir14777/file14777",
    matches: true,
    maxKept: 32 * MB,
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

// A matcher keeps its arrays at their length: one more state costs a few
// slots, not the room V8 leaves after `push` (half again plus 16 slots;
// it was a 1 KB step at 18 segment states). Each range stays in one NFA
// form, whose switch is a real step.
async function perMatcher(pattern) {
  gc();
  await new Promise((resolve) => setImmediate(resolve));
  gc();
  const before = used();
  const kept = Array.from({ length: 1000 }, () => compileMatcher(pattern));
  gc();
  const bytes = (used() - before) / kept.length;
  assert.equal(kept[0].match(""), false);
  return bytes;
}
/** @param {string} name @param {(k: number) => string} shape */
async function evenSteps(name, shape, from, to) {
  let prev = await perMatcher(shape(from));
  for (let k = from + 1; k <= to; k++) {
    const bytes = await perMatcher(shape(k));
    const step = Math.round(bytes - prev);
    assert.ok(step < 300, `${name}: ${k} costs ${step} bytes more than ${k - 1}`);
    prev = bytes;
  }
}
await evenSteps("segment NFA", (k) => `src/**/n*d[k-m]e?${"t".repeat(k)}`, 2, 20);
await evenSteps("graph NFA", (k) => `**/a${"/**/a".repeat(k)}`, 1, 14);

// A brace with 60,000 branches compiles in milliseconds: its branch
// tails are gathered in one list, not copied once per branch.
{
  const start = performance.now();
  compileMatcher(`x{?${",".repeat(60_000)}}`);
  const ms = performance.now() - start;
  assert.ok(ms < 1000, `60,000 branches took ${ms.toFixed(0)} ms to compile`);
}

// A class is a byte table: one lookup per byte, however many items it
// lists (it was a scan of every item for every byte: 30,000 items over
// 65,000 bytes took seconds). Twin of `a_long_class_costs_one_lookup_per_byte`.
{
  const m = compileMatcher(`*[${"b".repeat(30_000)}]`);
  const path = "a".repeat(65_000);
  const start = performance.now();
  assert.equal(m.match(path), false);
  assert.equal(m.match(path + "b"), true);
  const ms = performance.now() - start;
  assert.ok(ms < 300, `a 30,000-item class over 65,000 bytes took ${ms.toFixed(0)} ms`);
}

// Braces that cross a `/` keep the walk to a few groups: these took
// seconds and gigabytes, or ran out of heap.
for (const [pattern, path] of [
  ["{**,a}/".repeat(80) + "x", "a/x"],
  ["{a,b,/}".repeat(14), "abababa/bababa"],
  ["{/,a,}".repeat(20), "aa/a"],
  ["{a/,}".repeat(800), "a/a/"],
]) {
  const start = performance.now();
  const m = compileMatcher(pattern);
  const ms = performance.now() - start;
  assert.ok(ms < 200, `${pattern.slice(0, 7)} x${pattern.length}: ${ms.toFixed(0)} ms to compile`);
  assert.ok(m.match(path), pattern.slice(0, 7));
}

// A big union compiles in milliseconds (its groups are found by key, not
// by a scan of all) and matches by one search per segment.
{
  const patterns = names(20_000, (i) => `dir${i}/file${i}`);
  let start = performance.now();
  const m = compileMatcher(patterns);
  const compileMs = performance.now() - start;
  assert.ok(compileMs < 1000, `20,000 members took ${compileMs.toFixed(0)} ms to compile`);
  start = performance.now();
  for (let i = 0; i < 10_000; i++) assert.ok(m.match(`dir${i}/file${i}`));
  const ms = performance.now() - start;
  assert.ok(ms < 500, `10,000 matches took ${ms.toFixed(0)} ms`);
}

// Braces whose segments multiply would build a node per pair of them:
// the walk gives up early. Twin of the same check in
// tools/memory-check/tests/large_patterns.rs.
{
  const lists = (n) =>
    `{${Array.from({ length: n }, (_, i) => `/a${i}`).join(",")}}{${Array.from({ length: n }, (_, i) => `b${i}/`).join(",")}}z`;
  for (const pattern of [
    `a${"{a,/}".repeat(2048)}b`,
    "{a,b,/}".repeat(4096),
    "{/,a,}".repeat(4096),
    lists(2048),
    "{**,a}/".repeat(8192) + "x",
    "{a/,}".repeat(8192),
  ]) {
    const start = performance.now();
    assert.throws(() => compileMatcher(pattern), { kind: "TooComplex" });
    const ms = performance.now() - start;
    assert.ok(ms < 1000, `${pattern.slice(0, 7)}: ${ms.toFixed(0)} ms to fail`);
  }
}

// `matchDir` on long wildcard chains: the all-below proof feeds at most
// 64 wildcard-only segments and the chain's tail tokens are budgeted
// (each call took seconds). Twin of
// `match_dir_stays_cheap_on_long_wildcard_chains` in crates/globstar/tests/glob.rs.
{
  const union = compileMatcher(["**", `**/${"*/".repeat(16_000)}x`]);
  const chain = compileMatcher(`**/${"*/".repeat(32_000)}*`);
  const deep = "a/".repeat(32_000) + "a";
  const start = performance.now();
  const shallow = union.matchDir("a");
  const long = chain.matchDir(deep);
  const ms = performance.now() - start;
  assert.ok(ms < 1000, `matchDir took ${ms.toFixed(0)} ms`);
  for (const dm of [shallow, long]) assert.ok(DirMatch.matches(dm) && DirMatch.mayMatchBelow(dm));
}

// Two 64 KB members with a shared prefix compile in milliseconds: the
// shared tokens leave each member in one splice. Twin of
// `long_shared_prefixes_lift_in_one_pass` in crates/globstar/tests/glob_union.rs.
{
  const [x, y] = ["a/".repeat(32_000) + "x", "a/".repeat(32_000) + "y"];
  const start = performance.now();
  const union = compileMatcher([x, y]);
  const ms = performance.now() - start;
  assert.ok(ms < 1000, `2 x 64 KB members took ${ms.toFixed(0)} ms`);
  assert.ok(union.match(x) && union.match(y) && !union.match("a/".repeat(32_000) + "z"));
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
