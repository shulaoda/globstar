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
import { assertNormalizedProgram } from "../src/engine/ops/ir.js";
import { dedupePrefixes } from "../src/engine/ops/prefixes.js";

const here = dirname(fileURLToPath(import.meta.url));
const cases = readFileSync(resolve(here, "../../../fixtures/compiler-stages.tsv"), "utf8");

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

const encoded = ["src", "src/cli", "src-other", "src2", "src"].map((s) =>
  new TextEncoder().encode(s),
);
assert.deepEqual(
  dedupePrefixes(encoded).map((p) => new TextDecoder().decode(p)),
  ["src", "src2", "src-other"],
);
console.log("✓ prefix dedupe directory boundaries");
