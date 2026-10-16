import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

import {
  CI_BYTE,
  N_ANYCHAR,
  N_BRACE,
  N_CLASS,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  N_STAR,
} from "../src/ast.js";
import { parse } from "../src/parser.js";
import { compileMatcher } from "../src/glob.js";
import { factorBranches } from "../src/factor.js";
import {
  OP_ALTERNATION,
  OP_ANYCHAR,
  OP_CLASS,
  OP_GLOBSTAR,
  OP_GLOBSTAR_ANY,
  OP_LEADING_SEPS,
  OP_LIT,
  OP_OPT_SEGMENTS_SLASH,
  OP_SEP,
  OP_SEP_RUN,
  OP_SLASH_ANYTHING,
  OP_STAR,
  lower,
} from "../src/engine/ops.js";

const here = dirname(fileURLToPath(import.meta.url));
const cases = readFileSync(resolve(here, "../../../fixtures/compiler-stages.tsv"), "utf8");

function assertNormalizedProgram(ops) {
  let previousLit = false;
  let previousStar = false;
  for (const op of ops) {
    if (op.kind === OP_GLOBSTAR) throw new Error("raw globstar escaped lowering");
    if (op.kind === OP_LIT && previousLit) throw new Error("adjacent literal ops");
    if (op.kind === OP_STAR && previousStar) throw new Error("adjacent star ops");
    if (op.kind === OP_ALTERNATION) {
      for (const branch of op.branches) assertNormalizedProgram(branch);
    }
    previousLit = op.kind === OP_LIT;
    previousStar = op.kind === OP_STAR;
  }
}

function bytes(value) {
  let out = "";
  for (const b of value) {
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
      return `L(${bytes(node.bytes)})`;
    case N_SEPARATOR:
      return "S";
    case N_ANYCHAR:
      return "Q";
    case N_STAR:
      return "T";
    case N_GLOBSTAR:
      return "G";
    case N_CLASS:
      return `${node.neg ? "N" : "K"}(${node.items
        .map((item) =>
          item.tag === CI_BYTE ? bytes([item.b]) : `${bytes([item.lo])}-${bytes([item.hi])}`,
        )
        .join(",")})`;
    case N_BRACE:
      return `B(${node.branches.map(astDump).join("|")})`;
    default:
      throw new Error(`unknown AST node ${node.tag}`);
  }
}

function opsDump(ops) {
  return ops
    .map((op) => {
      switch (op.kind) {
        case OP_LIT:
          return `L(${bytes(op.bytes)})`;
        case OP_ANYCHAR:
          return "Q";
        case OP_STAR:
          return "T";
        case OP_CLASS:
          return "K";
        case OP_SEP:
          return "S";
        case OP_SEP_RUN:
          return "R";
        case OP_GLOBSTAR:
          return "G";
        case OP_OPT_SEGMENTS_SLASH:
          return "O";
        case OP_SLASH_ANYTHING:
          return "A";
        case OP_GLOBSTAR_ANY:
          return "Y";
        case OP_LEADING_SEPS:
          return "H";
        case OP_ALTERNATION:
          return `B(${op.branches.map((branch) => `[${opsDump(branch)}]`).join("|")})`;
        default:
          throw new Error(`unknown op ${op.kind}`);
      }
    })
    .join(",");
}

for (const [index, line] of cases.split("\n").entries()) {
  if (!line || line.startsWith("#")) continue;
  const columns = line.split("\t");
  assert.equal(columns.length, 3, `fixture line ${index + 1}`);
  const [pattern, expectedAst, expectedOps] = columns;
  const parsed = parse(pattern);
  assert.equal(astDump(parsed.body), expectedAst, `AST for ${JSON.stringify(pattern)}`);
  const program = lower(parsed.body, false);
  assertNormalizedProgram(program.ops);
  assert.equal(opsDump(program.ops), expectedOps, `ops for ${JSON.stringify(pattern)}`);
}

console.log("✓ shared parser/lowering golden cases");

// Static-prefix pinning, mirroring crates/globstar/tests/glob.rs prefixes_*.
const prefixesOf = (patterns) =>
  compileMatcher(patterns)
    .staticPrefixes()
    .map((b) => new TextDecoder().decode(b))
    .sort();
assert.deepEqual(prefixesOf("src/main.rs"), ["src/main.rs"]);
assert.deepEqual(prefixesOf("src/*.ts"), ["src"]);
assert.deepEqual(prefixesOf("**/*.ts"), [""]);
assert.deepEqual(prefixesOf("src/**/*.ts"), ["src"]);
// Strict trailing `/**` keeps the full literal head as the seed.
assert.deepEqual(prefixesOf("src/**"), ["src"]);
assert.deepEqual(prefixesOf("a/b/**"), ["a/b"]);
assert.deepEqual(prefixesOf("{src,tests}/**"), ["src", "tests"]);
// `{**,src}/x` — the empty prefix subsumes "src".
assert.deepEqual(prefixesOf("{**,src}/x"), [""]);
// A negated branch means matches can fall anywhere: seed from the cwd
// (a single empty prefix, never an empty list — the Rust convention).
assert.deepEqual(prefixesOf(["src/*.ts", "!x"]), [""]);
assert.deepEqual(prefixesOf(["!x"]), [""]);
// Each expansion's literal head: a brace is taken with what follows it,
// and a factored union keeps its members' heads. Twins of
// `prefixes_take_a_brace_with_what_follows_it` (glob.rs) and
// `factoring_keeps_the_members_static_prefixes` (glob_union.rs).
assert.deepEqual(prefixesOf("{node_modules,packages}/lodash/package.json"), [
  "node_modules/lodash/package.json",
  "packages/lodash/package.json",
]);
assert.deepEqual(prefixesOf("a{b,c}d/x"), ["abd/x", "acd/x"]);
assert.deepEqual(prefixesOf("{a/**,b}/c"), ["a", "b/c"]);
assert.deepEqual(prefixesOf("src/{**,*.ts}"), ["src"]);
assert.equal(prefixesOf("{a,b}".repeat(8) + "/x").length, 256);
assert.deepEqual(prefixesOf("{a,b}".repeat(16) + "/x"), [""]);
assert.deepEqual(prefixesOf(["src/mod1/index.ts", "src/mod2/index.ts"]), [
  "src/mod1/index.ts",
  "src/mod2/index.ts",
]);
assert.deepEqual(prefixesOf(["README.md", "node_modules/lodash/README.md"]), [
  "README.md",
  "node_modules/lodash/README.md",
]);
assert.deepEqual(prefixesOf(["src", "src/**"]), ["src"]);
assert.deepEqual(prefixesOf(["docs/**", "docs/*.md"]), ["docs"]);
{
  const branches = Array.from({ length: 1000 }, (_, i) => `b${i}`).join(",");
  const pattern = `{${branches}}/${"z".repeat(20_000)}`;
  const px = prefixesOf(pattern);
  assert.equal(px.length, 1000);
  assert.ok(px.reduce((n, p) => n + p.length, 0) <= 4 * pattern.length, "prefixes stay linear");
}
console.log("✓ static prefix extraction");

// One body under two spellings: factoring lifts all of it and keeps one
// empty branch, so the union answers as either member. Twin of
// `members_spelled_differently_fold_into_one` in crates/globstar/tests/glob_union.rs.
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
console.log("✓ members spelled differently fold into one");

// Options: booleans only, own keys only, no unknown keys; `undefined`
// keeps the default. `"false"` turned case folding on.
for (const bad of [{ caseInsensitive: "false" }, { dot: 0 }, { nocase: true }, "x", [true]]) {
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
