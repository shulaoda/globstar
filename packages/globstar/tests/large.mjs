// Large and deep patterns. JS twin of the size tests in
// `crates/globstar/tests/glob.rs`.

import assert from "node:assert/strict";
import v8 from "node:v8";
import vm from "node:vm";
import { Worker } from "node:worker_threads";
import { compileMatcher, DirMatch } from "../src/index.js";

const lib = JSON.stringify(new URL("../src/index.js", import.meta.url).href);

// Runs `body` (with `compileMatcher` in scope) on a worker with
// `resourceLimits` and resolves to what it posts back.
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

// The in-segment closures are built without recursion, so a long
// segment, or a long list under dot=false, fits a small stack.
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

// A long head before many branches is not copied out once per branch
// into the static prefixes: 32 KB patterns compile in milliseconds on a
// 64 MB heap. Twin of the check in tools/memory-check/tests/large_patterns.rs.
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
  assert.ok(ms < 300, `${name}: ${ms.toFixed(0)} ms to compile`);
}

console.log("✓ long heads before many branches stay small");

// A union too big for the chains agrees with its members: its graph NFA
// is sparse at both 100 and 600 members (Rust keeps dense rows at 100). Twin of
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

// 64 wildcard chains and the literal set: one chain more than two words
// have bits, so the chain index stands aside and the scan serves. Twin
// of `a_union_at_the_chain_cap_tries_every_chain` in
// crates/globstar/tests/glob_union.rs.
{
  const members = Array.from({ length: 64 }, (_, i) => `d${i}/**/*.x${i}`);
  const union = compileMatcher([...members, "a/b.txt", "c.txt"]);
  for (let i = 0; i < 64; i++) assert.ok(union.match(`d${i}/q/f.x${i}`), `d${i}`);
  assert.ok(union.match("a/b.txt") && union.match("c.txt"));
  assert.ok(!union.match("d1/q/f.x2"));
}

// A union past the key room answers as its members do: one member gives
// more keys than the room (26 per class branch) and is found by its
// head; 64 chains sharing 2,000 names file more entries than the room
// and are scanned. Twin of `a_union_past_the_key_room_answers_as_its_members`
// in crates/globstar/tests/glob_union.rs.
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
  // The chain index holds a key or so per byte: chains that share a long
  // list, or a brace of class branches, are not filed under every string
  // of it.
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
// slots, not the room V8 leaves after `push` (half again plus 16 slots). Each range stays in one NFA
// form, whose switch is a real step.
// The heap one matcher keeps, counted over 4,000. Each count is a call
// of its own: the matchers of one must be gone before the next begins.
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
// The smaller of two counts: a count only gains from what else the heap
// holds at the time.
async function perMatcher(pattern) {
  const first = await heapPer(pattern);
  return Math.min(first, await heapPer(pattern));
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

// Two globstars around a block: under `dot: false` the second one's dot
// check reads the middle once, not once per place the block is tried.
// Twin of `a_block_between_globstars_reads_the_middle_once`.
{
  const m = compileMatcher("**/x/**/y", { dot: false });
  const middle = "x/".repeat(32_000);
  const start = performance.now();
  assert.equal(m.match(`${middle}.h/y`), false);
  assert.equal(m.match(`${middle}y`), true);
  const ms = performance.now() - start;
  assert.ok(ms < 300, `a block between globstars over 64,000 bytes took ${ms.toFixed(0)} ms`);
}

// A union's merge reads each member at each of its segments: long members
// alike in every segment yet never the same are left apart, not read once
// per segment, so 64 KB of them compiles in milliseconds. Twin of
// `a_union_of_long_members_alike_in_every_segment_compiles_fast`.
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
    assert.ok(ms < 300, `${members.length} long members took ${ms.toFixed(0)} ms to compile`);
    assert.ok(union.match(hit) && !union.match(miss));
  }
}

// The graph walk finds a union's groups by all they hold: members alike
// but for a class or an in-segment brace are not each compared with
// every one before, so 32,768 of them compile in milliseconds. Twin of
// `members_apart_only_in_a_class_or_brace_compile_fast`.
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
    assert.ok(ms < 1000, `${hit}: ${ms.toFixed(0)} ms to compile`);
    assert.ok(union.match(hit) && !union.match("b/zzx"), hit);
  }
}

// Keying groups by all they hold costs the walk their weight, where an
// empty brace branch weighs one: groups that each hold a brace of 40,000
// empty branches fail in milliseconds, in a list past the budget or met
// again at 30 nested braces. Twin of
// `groups_keyed_whole_count_toward_the_compile_work`.
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
  for (const members of [
    [`p${core(650, 40_000)}{y,z/}`, `q${core(650, 40_000)}{y,z/}`],
    [`${nested}{y,/q}`],
  ]) {
    const start = performance.now();
    assert.throws(() => compileMatcher(members), { kind: "TooComplex" });
    const ms = performance.now() - start;
    assert.ok(ms < 500, `${members.length} members: ${ms.toFixed(0)} ms to fail`);
  }
}

// The chain index reads a run of one-string nodes after a long list in
// one pass over the list: 6,000 names behind 10,000 such nodes. Twin of
// `a_run_after_a_long_list_reads_it_once`.
{
  const heads = Array.from({ length: 11 }, (_, i) => `p${i}/`);
  const names = Array.from({ length: 6000 }, (_, i) => i.toString(16));
  const start = performance.now();
  const m = compileMatcher(`{${heads.join(",")}}${"[x]a".repeat(5000)}{${names.join(",")}}`);
  const ms = performance.now() - start;
  assert.ok(ms < 200, `a run after 6,000 names took ${ms.toFixed(0)} ms to compile`);
  assert.ok(m.match(`p3/${"xa".repeat(5000)}176f`));
}

// The chain index searches a path's last segment for the first 16 bytes
// of a member's literal at most, and the walk checks the rest: members
// `x3/*` + 400 `a`s + `q3*` turn down a 64 KB segment of `a`s in
// milliseconds, under caseInsensitive too. Twin of
// `a_long_infix_is_searched_for_its_start`.
{
  const lit = "a".repeat(400);
  const members = Array.from({ length: 16 }, (_, i) => `x${i}/*${lit}q${i}*`);
  const far = "d/" + "a".repeat(65_536);
  for (const caseInsensitive of [false, true]) {
    const m = compileMatcher(members, { caseInsensitive });
    const start = performance.now();
    assert.ok(!m.match(far));
    const ms = performance.now() - start;
    assert.ok(ms < 200, `caseInsensitive=${caseInsensitive}: ${ms.toFixed(0)} ms`);
    assert.ok(m.match(`x3/${lit}q3`));
    assert.ok(!m.match(`x3/${lit.slice(1)}q3`));
  }
}

// A class is a byte table: one lookup per byte, however many items it
// lists: 30,000 items over 65,000 bytes. Twin of `a_long_class_costs_one_lookup_per_byte`.
{
  const m = compileMatcher(`*[${"b".repeat(30_000)}]`);
  const path = "a".repeat(65_000);
  const start = performance.now();
  assert.equal(m.match(path), false);
  assert.equal(m.match(path + "b"), true);
  const ms = performance.now() - start;
  assert.ok(ms < 300, `a 30,000-item class over 65,000 bytes took ${ms.toFixed(0)} ms`);
}

// Braces that cross a `/` keep the walk to a few groups: each compiles
// in milliseconds.
for (const [pattern, path] of [
  ["{**,a}/".repeat(80) + "x", "a/x"],
  ["{a,b,/}".repeat(14), "abababa/bababa"],
  ["{/,a,}".repeat(20), "aa/a"],
  ["{a/,}".repeat(64), "a/a/"],
]) {
  const start = performance.now();
  const m = compileMatcher(pattern);
  const ms = performance.now() - start;
  assert.ok(ms < 200, `${pattern.slice(0, 7)} x${pattern.length}: ${ms.toFixed(0)} ms to compile`);
  assert.ok(m.match(path), pattern.slice(0, 7));
}

// Braces with a `**/` side by side chain their globstars, so the graph
// keeps a few edges per brace: 8,192 of them (57 KB) compile, and a path
// pays per segment for the states, not their square. Twin of
// `side_by_side_globstar_braces_stay_linear`.
for (const unit of ["{**,a}/", "{**,*}/"]) {
  const path = "a/".repeat(16) + "x";
  const start = performance.now();
  const m = compileMatcher(unit.repeat(8192) + "x");
  assert.ok(m.match(path), unit);
  assert.ok(DirMatch.matches(m.matchDir(path)), unit);
  const ms = performance.now() - start;
  assert.ok(ms < 500, `${unit} x8192: ${ms.toFixed(0)} ms`);
}

// Branches that may be empty, side by side, let each segment follow
// every one before it. The ones before it share a join node, so the graph
// keeps an edge or two per unit of pattern: it compiles fast, and no
// segment walks a successor list as long as the pattern. Twin of
// `edges_that_multiply_stay_linear`.
{
  for (const [pattern, path] of [
    ["{,/**/a}".repeat(800) + "/x", "/a".repeat(2000) + "/x"],
    ["{a/,}".repeat(800) + "x", "a/".repeat(800) + "x"],
  ]) {
    const unit = pattern.slice(0, pattern.indexOf("}") + 1);
    let start = performance.now();
    const m = compileMatcher(pattern);
    let ms = performance.now() - start;
    assert.ok(ms < 200, `${unit} x800: ${ms.toFixed(0)} ms to compile`);
    start = performance.now();
    assert.ok(m.match(path), unit);
    assert.ok(DirMatch.mayMatchBelow(m.matchDir(path.slice(0, path.lastIndexOf("/")))), unit);
    ms = performance.now() - start;
    assert.ok(ms < 500, `${unit} x800 on a long path: ${ms.toFixed(0)} ms`);
  }
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
    // Segments with big tables (suffix products, NFAs): charged too.
    "{a,b,/}".repeat(96),
    "{a,b,c,d,e,f,g,h,/}".repeat(3421),
  ]) {
    const start = performance.now();
    assert.throws(() => compileMatcher(pattern), { kind: "TooComplex" });
    const ms = performance.now() - start;
    assert.ok(ms < 1000, `${pattern.slice(0, 7)}: ${ms.toFixed(0)} ms to fail`);
  }
}

// A long class, or a long run of `?` or of classes, or a brace of empty
// branches, copied into every segment a brace leaves open: each copy
// counts, a class by its ranges, a brace by its branches, so these fail
// in milliseconds on a 64 MB heap, while a long class copied into a few
// segments still compiles. Twin of the checks in
// tools/memory-check/tests/large_patterns.rs.
for (const [name, ms, ok] of await onWorker(
  `
  const copied = (n, open, unit, close) => {
    const head = "{" + Array.from({ length: n }, (_, i) => "a/" + i).join(",") + "}" + open;
    return head + unit.repeat(Math.floor((16384 - head.length - close.length) / unit.length)) + close;
  };
  return [
    ["500 copies of a class", copied(500, "[a-z", "!-~", "]"), "TooComplex"],
    ["1,024 copies of ?s", copied(1024, "", "?", ""), "TooComplex"],
    ["512 copies of classes", copied(512, "", "[\\x01-\\x7f]", ""), "TooComplex"],
    ["1,024 copies of a brace", copied(1024, "{", ",", "}"), "TooComplex"],
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
  assert.ok(ms < 300, `${name}: ${ms.toFixed(0)} ms`);
}

// `matchDir` on long wildcard chains: the all-below proof feeds at most
// 64 wildcard-only segments and the chain's tail tokens are budgeted.
// Twin of
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
