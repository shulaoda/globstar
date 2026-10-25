import assert from "node:assert/strict";
import v8 from "node:v8";
import vm from "node:vm";
import { Worker } from "node:worker_threads";
import { compileMatcher, DirMatch } from "../src/index.js";

const REFERENCE_UNIT_MS = 53;
const machineSlowness = (() => {
  let best = Infinity;
  for (let r = 0; r < 3; r++) {
    const start = performance.now();
    let h = 0;
    for (let i = 0; i < 20_000_000; i++) h = (Math.imul(h ^ i, 0x9e3779b1) + (h >>> 7)) | 0;
    const a = Array.from({ length: 200_000 }, (_, i) => (Math.imul(i, 0x9e3779b1) >>> 0) % 1000003);
    a.sort((x, y) => x - y);
    if (h === 0 && a[0] === -1) throw new Error("unreachable");
    best = Math.min(best, performance.now() - start);
  }
  return Math.max(1, best / REFERENCE_UNIT_MS);
})();
const within = (ms, bound, what) =>
  assert.ok(
    ms < bound * machineSlowness,
    `${what}: ${ms.toFixed(0)} ms, bound ${(bound * machineSlowness).toFixed(0)} ms at machine slowness ${machineSlowness.toFixed(1)}`,
  );

const lib = JSON.stringify(new URL("../src/index.js", import.meta.url).href);

function onWorker(body, resourceLimits) {
  const code = `
    const { parentPort } = require("node:worker_threads");
    import(${lib}).then(({ compileMatcher }) => parentPort.postMessage((() => { ${body} })()));
  `;
  return new Promise((resolve, reject) => {
    const w = new Worker(code, { eval: true, resourceLimits });
    w.once("message", resolve);
    w.once("error", reject);
  });
}

assert.deepEqual(
  await onWorker(
    `
    const seg = compileMatcher("a" + "*{,}".repeat(2000) + "b");
    const names = Array.from({ length: 2000 }, (_, i) => "*x" + i);
    const list = compileMatcher("{" + names.join(",") + "}", { dot: false });
    return [seg.match("axb"), list.match("ax1999"), list.match(".x1999")];
  `,
    { stackSizeMb: 0.5 },
  ),
  [true, true, false],
);

console.log("✓ large patterns compile on a small stack");

for (const [name, ms, hit] of await onWorker(
  `
  const x = (n) => "x".repeat(n);
  const names = Array.from({ length: 4096 }, (_, i) => i.toString(16));
  return [
    ["16 KB head, 16,000 empty branches", x(16384) + "{" + ",".repeat(16382) + "}", x(16384)],
    ["32 KB head, {,,,,,,,} x7", x(32705) + "{,,,,,,,}".repeat(7), x(32705)],
    ["32 KB head, {,,,,,,,} x7 and /y", x(32703) + "/" + "{,,,,,,,}".repeat(7) + "y", x(32703) + "/y"],
    ["16 KB head, 8,000 branches", x(16384) + "{" + "a,".repeat(8191) + "a}", x(16384) + "a"],
    ["16 KB head, 4,096 names", x(16384) + "/{" + names.join(",") + "}", x(16384) + "/aaa"],
    [
      "16 KB head cut back by *, 8,000 branches",
      "abcdefghijklmnop/" + x(16384) + "{" + "b,".repeat(8191) + "b}*",
      "abcdefghijklmnop/" + x(16384) + "bq",
    ],
  ].map(([name, pattern, path]) => {
    const start = performance.now();
    const m = compileMatcher(pattern);
    return [name, performance.now() - start, m.match(path)];
  });
`,
  { maxOldGenerationSizeMb: 64 },
)) {
  assert.ok(hit, name);
  within(ms, 300, `${name} to compile`);
}

console.log("✓ long heads before many branches stay small");

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

{
  const members = Array.from({ length: 64 }, (_, i) => `d${i}/**/*.x${i}`);
  const union = compileMatcher([...members, "a/b.txt", "c.txt"]);
  for (let i = 0; i < 64; i++) assert.ok(union.match(`d${i}/q/f.x${i}`), `d${i}`);
  assert.ok(union.match("a/b.txt") && union.match("c.txt"));
  assert.ok(!union.match("d1/q/f.x2"));
}

{
  const hex = (i) => i.toString(16);
  const members = [
    ...Array.from({ length: 20 }, (_, i) => `d${i}/**/*.x${i}`),
    `big/{${Array.from({ length: 1000 }, (_, i) => `[a-z]${hex(i)}`).join(",")}}`,
  ];
  const heads = Array.from({ length: 64 }, (_, i) => `${hex(i)}/`);
  const names = Array.from({ length: 2000 }, (_, i) => `n${i}`);
  const shared = [`{${heads.join(",")}}*{${names.join(",")}}`];
  for (const [list, paths] of [
    [members, ["big/q3e7", "big/Q3e7", "big/q3e8", "d3/a/b.x3", "d3/b.x4"]],
    [shared, ["3f/an1999", "3f/n2000", "40/n1", "a/n7", "3f/n19999"]],
  ]) {
    const union = compileMatcher(list);
    assert.ok(union.match(paths[0]), paths[0]);
    for (const path of paths) {
      const want = list.some((p) => compileMatcher(p).match(path));
      assert.equal(union.match(path), want, path);
    }
  }
}

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

v8.setFlagsFromString("--expose-gc");
const gc = vm.runInNewContext("gc");
const used = () => {
  const m = process.memoryUsage();
  return m.heapUsed + m.arrayBuffers;
};
const MB = 1 << 20;
const names = (n, f) => Array.from({ length: n }, (_, i) => f(i));
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
  {
    name: "20,000 dir/file members",
    patterns: names(20_000, (i) => `dir${i}/file${i}`),
    path: "dir14777/file14777",
    matches: true,
    maxKept: 32 * MB,
  },
  {
    name: "64 heads, 12,000 names",
    patterns: `{${names(64, (i) => `${i.toString(16)}/`).join(",")}}*{${names(12_000, (i) => i.toString(16)).join(",")}}`,
    path: "3f/a2edf",
    matches: true,
    maxKept: 2 * MB,
  },
  {
    name: "11 heads, 1,000 class branches",
    patterns: `{${names(11, (i) => `${i.toString(16)}/`).join(",")}}{${names(1_000, (i) => `[#-_]${i.toString(16)}`).join(",")}}`,
    path: "a/Z3e7",
    matches: true,
    maxKept: 2 * MB,
  },
  {
    name: "{a,b} x10 and 60 KB",
    patterns: `*${"{a,b}".repeat(10)}${"z".repeat(60_000)}`,
    path: "az",
    matches: false,
    maxKept: 16 * MB,
  },
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

async function heapPer(pattern) {
  gc();
  await new Promise((resolve) => setImmediate(resolve));
  gc();
  const before = used();
  const kept = Array.from({ length: 4000 }, () => compileMatcher(pattern));
  gc();
  assert.equal(kept[0].match(""), false);
  return (used() - before) / kept.length;
}
async function perMatcher(pattern) {
  const first = await heapPer(pattern);
  return Math.min(first, await heapPer(pattern));
}
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

{
  const start = performance.now();
  compileMatcher(`x{?${",".repeat(60_000)}}`);
  const ms = performance.now() - start;
  within(ms, 1000, `60,000 branches to compile`);
}

{
  const start = performance.now();
  assert.equal(compileMatcher("{?,}".repeat(16_000)).match("abc"), true);
  const cut = `{x/,y/*,zz}${"a".repeat(9_000)}/d/${"{,?}".repeat(13_000)}*{/**,*}`;
  const m = compileMatcher(cut);
  const dir = `x/${"a".repeat(9_000)}/d`;
  for (let i = 0; i < 100; i++) assert.ok(DirMatch.matchesAllBelow(m.matchDir(dir)));
  const ms = performance.now() - start;
  within(ms, 2000, `one-pass lengths`);
}

{
  const m = compileMatcher("**/x/**/y", { dot: false });
  const middle = "x/".repeat(32_000);
  const start = performance.now();
  assert.equal(m.match(`${middle}.h/y`), false);
  assert.equal(m.match(`${middle}y`), true);
  const ms = performance.now() - start;
  within(ms, 300, `a block between globstars over 64,000 bytes`);
}

{
  const middle = "a/".repeat(16_000);
  const pad = (i) => String(i).padStart(2, "0");
  const few = [`*/${middle}x/p`, `*/${middle}y/q`, "z"];
  const many = Array.from({ length: 16 }, (_, i) => `*/m${pad(i)}/${"a/".repeat(2000)}n${pad(i)}`);
  many.push("Q");
  for (const [members, hit, miss] of [
    [few, `b/${middle}x/p`, `b/${middle}x/q`],
    [many, `b/m03/${"a/".repeat(2000)}n03`, "b/m03/n04"],
  ]) {
    const start = performance.now();
    const union = compileMatcher(members);
    const ms = performance.now() - start;
    within(ms, 300, `${members.length} long members to compile`);
    assert.ok(union.match(hit) && !union.match(miss));
  }
}

{
  const side = (i) => (i % 2 ? "b" : "a");
  const hex = (i) => i.toString(16).padStart(4, "0");
  for (const { form, hit } of [
    { form: (i) => `${side(i)}/[${hex(i)}]x`, hit: "b/3x" },
    { form: (i) => `${side(i)}/{${hex(i)},q}x`, hit: "b/qx" },
    { form: (i) => `**/[${hex(i)}].log`, hit: "x/3.log" },
  ]) {
    const members = Array.from({ length: 32_768 }, (_, i) => form(i));
    members.push("c/d");
    const start = performance.now();
    const union = compileMatcher(members);
    const ms = performance.now() - start;
    within(ms, 1000, `${hit} to compile`);
    assert.ok(union.match(hit) && !union.match("b/zzx"), hit);
  }
}

{
  const core = (n, m) => {
    const classes = Array.from(
      { length: n },
      (_, i) => `/[${String.fromCharCode(97 + Math.floor(i / 26), 65 + (i % 26))}]`,
    );
    return `{${classes.join(",")}}{${",".repeat(m)}}`;
  };
  let nested = core(60, 40_000);
  for (let i = 0; i < 30; i++) nested = `{${nested},/z${i}}`;
  for (const [members, hits, misses] of [
    [
      [`p${core(650, 40_000)}{y,z/}`, `q${core(650, 40_000)}{y,z/}`],
      ["p/ay", "q/bz/"],
      ["p/!y", "p/ay/", "q/bZz/"],
    ],
    [[`${nested}{y,/q}`], ["/ay", "/z3/q"], ["/aAy", "/z3y/q", "/a"]],
  ]) {
    const start = performance.now();
    const union = compileMatcher(members);
    const ms = performance.now() - start;
    within(ms, 500, `${members.length} members to compile`);
    for (const hit of hits) assert.ok(union.match(hit), hit);
    for (const miss of misses) assert.ok(!union.match(miss), miss);
  }
}

{
  const heads = Array.from({ length: 11 }, (_, i) => `p${i}/`);
  const names = Array.from({ length: 6000 }, (_, i) => i.toString(16));
  const start = performance.now();
  const m = compileMatcher(`{${heads.join(",")}}${"[x]a".repeat(5000)}{${names.join(",")}}`);
  const ms = performance.now() - start;
  within(ms, 200, `a run after 6,000 names to compile`);
  assert.ok(m.match(`p3/${"xa".repeat(5000)}176f`));
}

{
  const lit = "a".repeat(400);
  const members = Array.from({ length: 16 }, (_, i) => `x${i}/*${lit}q${i}*`);
  const far = "d/" + "a".repeat(65_536);
  for (const caseInsensitive of [false, true]) {
    const m = compileMatcher(members, { caseInsensitive });
    const start = performance.now();
    assert.ok(!m.match(far));
    const ms = performance.now() - start;
    within(ms, 200, `caseInsensitive=${caseInsensitive}`);
    assert.ok(m.match(`x3/${lit}q3`));
    assert.ok(!m.match(`x3/${lit.slice(1)}q3`));
  }
}

{
  const m = compileMatcher(`*[${"b".repeat(30_000)}]`);
  const path = "a".repeat(65_000);
  const start = performance.now();
  assert.equal(m.match(path), false);
  assert.equal(m.match(path + "b"), true);
  const ms = performance.now() - start;
  within(ms, 300, `a 30,000-item class over 65,000 bytes`);
}

for (const [pattern, path] of [
  ["{**,a}/".repeat(80) + "x", "a/x"],
  ["{a,b,/}".repeat(14), "abababa/bababa"],
  ["{/,a,}".repeat(20), "aa/a"],
  ["{a/,}".repeat(64), "a/a/"],
]) {
  const start = performance.now();
  const m = compileMatcher(pattern);
  const ms = performance.now() - start;
  within(ms, 200, `${pattern.slice(0, 7)} x${pattern.length} to compile`);
  assert.ok(m.match(path), pattern.slice(0, 7));
}

for (const unit of ["{**,a}/", "{**,*}/"]) {
  const path = "a/".repeat(16) + "x";
  const start = performance.now();
  const m = compileMatcher(unit.repeat(8192) + "x");
  assert.ok(m.match(path), unit);
  assert.ok(DirMatch.matches(m.matchDir(path)), unit);
  const ms = performance.now() - start;
  within(ms, 500, `${unit} x8192`);
}

{
  for (const [pattern, path] of [
    ["{,/**/a}".repeat(800) + "/x", "/a".repeat(400) + "/x"],
    ["{a/,}".repeat(800) + "x", "a/".repeat(800) + "x"],
  ]) {
    const unit = pattern.slice(0, pattern.indexOf("}") + 1);
    let start = performance.now();
    const m = compileMatcher(pattern);
    let ms = performance.now() - start;
    within(ms, 200, `${unit} x800 to compile`);
    start = performance.now();
    assert.ok(m.match(path), unit);
    assert.ok(DirMatch.mayMatchBelow(m.matchDir(path.slice(0, path.lastIndexOf("/")))), unit);
    ms = performance.now() - start;
    within(ms, 500, `${unit} x800 on a long path`);
  }
}

{
  const patterns = names(20_000, (i) => `dir${i}/file${i}`);
  let start = performance.now();
  const m = compileMatcher(patterns);
  const compileMs = performance.now() - start;
  within(compileMs, 1000, `20,000 members to compile`);
  start = performance.now();
  for (let i = 0; i < 10_000; i++) assert.ok(m.match(`dir${i}/file${i}`));
  const ms = performance.now() - start;
  within(ms, 500, `10,000 matches`);
}

{
  const lists = (n) =>
    `{${Array.from({ length: n }, (_, i) => `/a${i}`).join(",")}}{${Array.from({ length: n }, (_, i) => `b${i}/`).join(",")}}z`;
  const shapes = [
    (n) => `a${"{a,/}".repeat(n)}b`,
    (n) => "{a,b,/}".repeat(n),
    (n) => "{/,a,}".repeat(n),
    (n) => "{a,b,c,d,e,f,g,h,/}".repeat(n),
    (n) => "{**,a}{/,b}".repeat(n),
    (n) => "**{/a,b}".repeat(n),
    (n) => "{a/**,b}".repeat(n),
    (n) => "{x/,y}**".repeat(n),
    (n) => "[a-z]{/,}".repeat(n),
    (n) => "{a,}**".repeat(n),
    (n) => `**${"{,/a}".repeat(n)}`,
    (n) => "{a/,}".repeat(n),
    (n) => "{**,a}/".repeat(n),
  ];
  const took = (pattern) => {
    const start = performance.now();
    compileMatcher(pattern).match("a/b");
    return performance.now() - start;
  };
  for (const shape of shapes) {
    const n = Math.floor(65_536 / shape(1).length);
    const ms = took(shape(n));
    within(ms, 500, `${shape(1)} x${n}`);
  }
  for (const pattern of [lists(2048), "{a,b,c,d,e,f,g,h,/}".repeat(3421)]) {
    const ms = took(pattern);
    within(ms, 1000, `${pattern.slice(0, 7)}`);
  }
}

for (const [name, ms, ok] of await onWorker(
  `
  const copied = (n, open, unit, close) => {
    const head = "{" + Array.from({ length: n }, (_, i) => "a/" + i).join(",") + "}" + open;
    return head + unit.repeat(Math.floor((16384 - head.length - close.length) / unit.length)) + close;
  };
  return [
    ["500 copies of a class", copied(500, "[a-z", "!-~", "]"), true],
    ["1,024 copies of ?s", copied(1024, "", "?", ""), false],
    ["512 copies of classes", copied(512, "", "[\\x01-\\x7f]", ""), false],
    ["1,024 copies of a brace", copied(1024, "{", ",", "}"), false],
    ["16 copies of a class", copied(16, "[a-z", "\\x01-\\x7f", "]"), true],
  ].map(([name, pattern, want]) => {
    const start = performance.now();
    let got;
    try {
      got = compileMatcher(pattern).match("a/3x");
    } catch (e) {
      got = e.kind;
    }
    return [name, performance.now() - start, got === want];
  });
`,
  { maxOldGenerationSizeMb: 64 },
)) {
  assert.ok(ok, name);
  within(ms, 300, `${name}`);
}

{
  const union = compileMatcher(["**", `**/${"*/".repeat(16_000)}x`]);
  const chain = compileMatcher(`**/${"*/".repeat(32_000)}*`);
  const deep = "a/".repeat(32_000) + "a";
  const start = performance.now();
  const shallow = union.matchDir("a");
  const long = chain.matchDir(deep);
  const ms = performance.now() - start;
  within(ms, 1000, `matchDir`);
  for (const dm of [shallow, long]) assert.ok(DirMatch.matches(dm) && DirMatch.mayMatchBelow(dm));
}

{
  const [x, y] = ["a/".repeat(32_000) + "x", "a/".repeat(32_000) + "y"];
  const start = performance.now();
  const union = compileMatcher([x, y]);
  const ms = performance.now() - start;
  within(ms, 1000, `2 x 64 KB members`);
  assert.ok(union.match(x) && union.match(y) && !union.match("a/".repeat(32_000) + "z"));
}

{
  const m = compileMatcher(names(20_000, (i) => `src/file${i}.ts`));
  const start = performance.now();
  for (let i = 0; i < 10_000; i++) assert.ok(m.match(`src/file${(i * 7) % 20_000}.ts`));
  const ms = performance.now() - start;
  within(ms, 1000, `10,000 matches`);
}

console.log("✓ large patterns stay small and fast");
