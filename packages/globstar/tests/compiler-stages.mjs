import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

import {
  N_ANYCHAR,
  N_BRACE,
  N_CLASS,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  N_STAR,
} from "../src/parse/ast.js";
import { parse } from "../src/parse/parser.js";
import { buildEngine, compileMatcher } from "../src/glob.js";
import { factorBranches } from "../src/union.js";

const here = dirname(fileURLToPath(import.meta.url));
const cases = readFileSync(resolve(here, "../../../fixtures/compiler-stages.tsv"), "utf8");

function bytes(value) {
  let out = "";
  for (let b of value) {
    if (typeof b === "string") b = b.charCodeAt(0);
    if (b === 0x5c) out += "\\\\";
    else if (b === 0x28 || b === 0x29 || b === 0x2c || b === 0x7c || b === 0x5b || b === 0x5d)
      out += `\\x${b.toString(16).padStart(2, "0")}`;
    else if (b >= 0x20 && b <= 0x7e) out += String.fromCharCode(b);
    else out += `\\x${b.toString(16).padStart(2, "0")}`;
  }
  return out;
}

function astDump(node) {
  switch (node.tag) {
    case N_CONCAT:
      return `C(${node.children.map(astDump).join(",")})`;
    case N_LITERAL:
      return `L(${bytes(node.str)})`;
    case N_SEPARATOR:
      return "S";
    case N_ANYCHAR:
      return "Q";
    case N_STAR:
      return "T";
    case N_GLOBSTAR:
      return "G";
    case N_CLASS:
      return `${node.neg ? "N" : "K"}(${node.ranges
        .map(({ lo, hi }) => (lo === hi ? bytes([lo]) : `${bytes([lo])}-${bytes([hi])}`))
        .join(",")})`;
    case N_BRACE:
      return `B(${node.branches.map(astDump).join("|")})`;
    default:
      throw new Error(`unknown AST node ${node.tag}`);
  }
}

for (const [index, line] of cases.split("\n").entries()) {
  if (!line || line.startsWith("#")) continue;
  const columns = line.split("\t");
  assert.equal(columns.length, 2, `fixture line ${index + 1}`);
  const [pattern, expectedAst] = columns;
  const parsed = parse(pattern);
  assert.equal(astDump(parsed.body), expectedAst, `AST for ${JSON.stringify(pattern)}`);
}

console.log("✓ shared parser golden cases");

const prefixesOf = (patterns) =>
  compileMatcher(patterns)
    .staticPrefixes()
    .map((b) => new TextDecoder().decode(b))
    .sort();
const prefixRows = readFileSync(resolve(here, "../../../fixtures/static-prefixes.tsv"), "utf8");
for (const line of prefixRows.split("\n")) {
  if (!line || line.startsWith("#")) continue;
  const cols = line.split("\t");
  const at = cols.indexOf("=>");
  const patterns = cols.slice(0, at);
  const got = prefixesOf(patterns.length === 1 ? patterns[0] : patterns);
  assert.deepEqual(got, cols.slice(at + 1), JSON.stringify(patterns));
}
assert.deepEqual(prefixesOf(["src/*.ts", "!x"]), [""]);
assert.deepEqual(prefixesOf(["!x"]), [""]);
assert.equal(prefixesOf("{a,b}".repeat(8) + "/x").length, 256);
assert.deepEqual(prefixesOf("{a,b}".repeat(16) + "/x"), [""]);
{
  const branches = Array.from({ length: 1000 }, (_, i) => `b${i}`).join(",");
  const pattern = `{${branches}}/${"z".repeat(20_000)}`;
  const px = prefixesOf(pattern);
  assert.equal(px.length, 1000);
  assert.ok(px.reduce((n, p) => n + p.length, 0) <= 4 * pattern.length, "prefixes stay linear");
}
{
  const pad = (i) => String(i).padStart(4, "0");
  const files = Array.from(
    { length: 4000 },
    (_, i) => `packages/foo/src/components/xyz/Comp${pad(i)}.tsx`,
  );
  assert.equal(prefixesOf(files).length, 4000);
  const names = Array.from({ length: 4096 }, (_, i) => i.toString(16));
  const folder = "x".repeat(16_384);
  assert.deepEqual(prefixesOf(`${folder}/{${names.join(",")}}`), [folder]);
}
console.log("✓ static prefix extraction");

for (const [a, b] of [
  ["**", "**/**"],
  ["a/**", "a/**/**"],
  ["/**", "/**/**"],
  ["\\a", "a"],
]) {
  const union = compileMatcher([a, b]);
  const single = compileMatcher(b);
  for (const path of ["", "a", "a/b", "/a", "/a/b", "x/y"]) {
    assert.equal(union.match(path), single.match(path), `${a} ${b} ${path}`);
    assert.equal(union.matchDir(path), single.matchDir(path), `${a} ${b} ${path}`);
  }
}
const folded = parse("\\a").body;
assert.deepEqual(factorBranches([folded, parse("a").body]), folded);
const ciSet = buildEngine(
  ["Src/A.ts", "src/a.ts", "LIB/b.ts", "x/*.md"].map((p) => parse(p)),
  { dot: true, caseInsensitive: true },
);
assert.ok(ciSet.shape().includes("literals 2"), ciSet.shape());
console.log("✓ members spelled differently fold into one");

{
  const files = Array.from({ length: 10_000 }, (_, i) => `src/m${i % 100}/f${i}.ts`);
  const engine = buildEngine(
    files.map((p) => parse(p)),
    { dot: true, caseInsensitive: false },
  );
  assert.ok(engine.shape().includes("| literals 10000 |"), engine.shape());
  const m = compileMatcher(files);
  assert.ok(m.match("src/m7/f9907.ts") && !m.match("src/m7/f9908.ts"));
}
console.log("✓ a union of many file names keeps its literal set");

{
  const keyed = Array.from({ length: 5000 }, (_, i) => `d${i}/*/n${i}.js`);
  const keyless = Array.from({ length: 100 }, (_, i) => `*${i}*/**`);
  const crowded = Array.from({ length: 100 }, (_, i) => `d${i}/q${i}/n.js`);
  const product = ["{a/x,b/y}/".repeat(30) + "*.x"];
  const paths = [
    "d7/x/n7.js",
    "d4999/q/n4999.js",
    "d7/x/n8.js",
    "d7/n7.js",
    "a17b/c",
    "x17/",
    "d7/q7/n.js",
    "d7/q8/n.js",
    "a/x/".repeat(30) + "f.x",
    "b/y/".repeat(29) + "f.x",
  ];
  for (const [members, chains] of [
    [keyed, "| chains 5000 |"],
    [keyless, "| chains 0 |"],
    [crowded, "chains 0 | literals 100 |"],
    [product, "| chains 0 |"],
  ]) {
    const engine = buildEngine(
      members.map((p) => parse(p)),
      { dot: false, caseInsensitive: false },
    );
    assert.ok(engine.shape().includes(chains), engine.shape());
    const union = compileMatcher(members);
    const nfa = compileMatcher(members, { __engine: "nfa" });
    for (const path of paths) assert.equal(union.match(path), nfa.match(path), path);
  }
  const union = compileMatcher(keyed);
  assert.ok(union.match("d4999/q/n4999.js") && !union.match("d7/x/n8.js"));
}
console.log("✓ a union of many wildcard members keeps its chains while indexed");

for (const bad of [
  { caseInsensitive: "false" },
  { dot: 0 },
  { nocase: true },
  { __engine: 42 },
  { __engine: "NFA" },
  "x",
  [true],
]) {
  assert.throws(() => compileMatcher("*.txt", bad), TypeError, JSON.stringify(bad));
}
assert.equal(compileMatcher("*.TXT", { caseInsensitive: undefined }).match("a.txt"), false);
Object.prototype.dot = false;
try {
  assert.equal(compileMatcher("**").match("a/.env"), true, "an inherited key changes nothing");
} finally {
  delete Object.prototype.dot;
}
console.log("✓ matcher options are checked");

{
  const { GlobError, DirMatch, MAX_PATTERN_LEN, MAX_BRACE_NESTING } =
    await import("../src/index.js");
  const thrown = (f) => {
    try {
      f();
    } catch (e) {
      return e;
    }
    throw new Error("no error");
  };
  const member = thrown(() => compileMatcher(["src/**", "src/**", "lib/[a"]));
  assert.ok(member instanceof GlobError);
  assert.equal(member.kind, "UnterminatedClass");
  assert.equal(member.index, 2);
  assert.equal(member.at, 4);
  assert.ok(member.message.startsWith("pattern 2: "), member.message);
  assert.equal(thrown(() => compileMatcher("lib/[a")).index, undefined);
  const heavy = "{a,b,/}".repeat(100);
  const each = "{a,b,/}".repeat(28);
  for (const members of [
    ["a", heavy],
    [each, `x/${each}`],
  ]) {
    const union = compileMatcher(members);
    const alone = members.map((p) => compileMatcher(p));
    for (const path of ["a", "ab/ba", "x/a/b", "x//", "/a/b/", "abc", "x/ab"]) {
      assert.equal(
        union.match(path),
        alone.some((m) => m.match(path)),
        path,
      );
    }
  }
  const each10 = "{,/}".repeat(10) + "**/f";
  const members = Array.from({ length: 10 }, (_, i) => `${each10}${i}`);
  const union = compileMatcher(members);
  const alone = members.map((p) => compileMatcher(p));
  for (const path of ["f0", "a/f3", "/f9", "a//b/f1", "///f2", "f", "a/f10"]) {
    assert.equal(
      union.match(path),
      alone.some((m) => m.match(path)),
      path,
    );
  }
  assert.ok(union.match("a/f3") && !union.match("f"));
  const range = thrown(() => compileMatcher("src/[abcz-a].ts"));
  assert.equal(range.at, 8);
  const json = JSON.parse(JSON.stringify(range));
  assert.equal(json.message, range.message);
  assert.deepEqual([json.kind, json.at, json.low, json.high], ["InvalidRange", 8, 0x7a, 0x61]);
  assert.equal(compileMatcher(new Set(["*.ts", "*.js"])).match("a.js"), true);
  assert.ok(Object.isFrozen(DirMatch));
  assert.deepEqual([MAX_PATTERN_LEN, MAX_BRACE_NESTING], [65536, 32]);
}
console.log("✓ errors and API surface");

{
  const { MAX_PATTERN_LEN } = await import("../src/index.js");
  compileMatcher("a".repeat(MAX_PATTERN_LEN));
  assert.throws(() => compileMatcher("a".repeat(MAX_PATTERN_LEN + 1)), {
    kind: "TooLong",
    len: MAX_PATTERN_LEN + 1,
    max: MAX_PATTERN_LEN,
  });
  compileMatcher("é".repeat(MAX_PATTERN_LEN / 2));
  assert.throws(() => compileMatcher("é".repeat(MAX_PATTERN_LEN / 2) + "a"), { kind: "TooLong" });
  for (const [unit, bytes] of [
    ["é", 2],
    ["😀", 4],
  ]) {
    const pattern = unit.repeat(MAX_PATTERN_LEN);
    const len = (pattern.length / unit.length) * bytes;
    assert.throws(() => compileMatcher(pattern), { kind: "TooLong", len });
  }
  const big = "a".repeat(16 << 20);
  big.charCodeAt(0);
  const before = process.memoryUsage().arrayBuffers;
  assert.throws(() => compileMatcher(big), { kind: "TooLong", len: 16 << 20 });
  const grew = process.memoryUsage().arrayBuffers - before;
  assert.ok(grew < 1 << 20, `refusing a 16 MB pattern allocated ${grew} bytes`);
}
for (const [pattern, at] of [
  ["\ud800", 0],
  ["a\udc00", 1],
  ["😀\ud83d", 2],
]) {
  assert.throws(() => compileMatcher(pattern), {
    name: "TypeError",
    message: `pattern must be well-formed UTF-16, got a lone surrogate at index ${at}`,
  });
  assert.throws(() => compileMatcher(["a", pattern]), TypeError);
}
assert.equal(compileMatcher("😀*").match("😀x"), true);
console.log("✓ limits");

{
  const shapes = readFileSync(resolve(here, "../../../fixtures/engine-shapes.tsv"), "utf8");
  let rows = 0;
  for (const line of shapes.split("\n")) {
    if (line === "" || line.startsWith("#")) continue;
    const [want, ...patterns] = line.split("\t");
    const engine = buildEngine(
      patterns.map((p) => parse(p)),
      { dot: true, caseInsensitive: false },
    );
    assert.equal(engine.shape(), want, patterns.join(" + ").slice(0, 60));
    rows++;
  }
  assert.ok(rows >= 20, `only ${rows} rows`);
}
console.log("✓ engine shapes");

{
  const { isPathSep } = await import("../src/engine/bytes.js");
  const inClass = (ranges, b) => !isPathSep(b) && ranges.some(({ lo, hi }) => b >= lo && b <= hi);
  for (const pattern of [
    "[a]",
    "[!a]",
    "[a-c]",
    "[!a-c]",
    "[]a]",
    "[]-]",
    "[a-]",
    "[\\]]",
    "[\\\\]",
    "[\x01-\x7f]",
    "[!\x01-\x7f]",
    "[\x1f-\x21]",
    "[?-A]",
    "[bbbbbb]",
    "[a-cb-dz]",
  ]) {
    const cls = parse(pattern).body;
    assert.equal(cls.tag, N_CLASS, pattern);
    for (const ci of [false, true]) {
      const m = compileMatcher("x" + pattern, { caseInsensitive: ci });
      for (let b = 1; b < 0x80; b++) {
        if (b === 0x2f || (process.platform === "win32" && b === 0x5c)) continue;
        const listed = (c) => inClass(cls.ranges, c);
        const letter = /[a-z]/i.test(String.fromCharCode(b));
        const want = (listed(b) || (ci && letter && listed(b ^ 0x20))) !== cls.neg;
        assert.equal(m.match("x" + String.fromCharCode(b)), want, `${pattern} (ci ${ci}) at ${b}`);
      }
    }
  }
}
console.log("✓ class tables");
