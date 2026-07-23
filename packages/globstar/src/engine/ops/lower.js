// AST to normalized linear-op lowering.

import {
  N_ANYCHAR,
  N_BRACE,
  N_CLASS,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  N_STAR,
  classExpandedAsciiCi,
} from "../../ast.js";
import { LiteralFacts } from "../facts.js";
import {
  ANYCHAR_OP,
  GLOBSTAR_OP,
  OP_ALTERNATION,
  OP_CLASS,
  OP_LIT,
  OP_STAR,
  SEP_OP,
  STAR_OP,
} from "./ir.js";
import {
  applyLeadingSepsAtStart,
  distributeSeps,
  foldGlobstars,
  needsSepDistribution,
} from "./normalize.js";

// `maybeSepDistribution` is the parser's hint (see parse()): when false
// no `**` sits inside a brace, so the separator-distribution walk is
// provably a no-op and is skipped. When true the precise check still
// decides — the hint is a superset, never the decider. Defaults to
// true so direct callers (tests) keep exact full-check behavior.
export function lower(node, caseInsensitive, maybeSepDistribution = true) {
  const ops = [];
  const root = maybeSepDistribution && needsSepDistribution(node) ? distributeSeps(node) : node;
  lowerInto(root, ops, caseInsensitive);
  foldGlobstars(ops);
  applyLeadingSepsAtStart(ops);
  const ci = !!caseInsensitive;
  return { ops, facts: LiteralFacts.extract(ops, ci), caseInsensitive: ci };
}

function lowerInto(node, out, caseInsensitive) {
  switch (node.tag) {
    case N_LITERAL:
      pushOp(out, { kind: OP_LIT, bytes: node.bytes });
      return;
    case N_SEPARATOR:
      pushOp(out, SEP_OP);
      return;
    case N_ANYCHAR:
      pushOp(out, ANYCHAR_OP);
      return;
    case N_STAR:
      pushOp(out, STAR_OP);
      return;
    case N_GLOBSTAR:
      pushOp(out, GLOBSTAR_OP);
      return;
    case N_CLASS:
      pushOp(out, { kind: OP_CLASS, cls: caseInsensitive ? classExpandedAsciiCi(node) : node });
      return;
    case N_CONCAT:
      for (const child of node.children) lowerInto(child, out, caseInsensitive);
      return;
    case N_BRACE: {
      const branches = [];
      for (const branch of node.branches) {
        const branchOps = [];
        lowerInto(branch, branchOps, caseInsensitive);
        foldGlobstars(branchOps);
        branches.push(branchOps);
      }
      pushOp(out, { kind: OP_ALTERNATION, branches });
      return;
    }
  }
}

function pushOp(out, op) {
  if (op.kind === OP_STAR && out.length > 0 && out[out.length - 1].kind === OP_STAR) return;
  if (op.kind === OP_LIT && out.length > 0 && out[out.length - 1].kind === OP_LIT) {
    const previous = out[out.length - 1];
    const merged = new Uint8Array(previous.bytes.length + op.bytes.length);
    merged.set(previous.bytes, 0);
    merged.set(op.bytes, previous.bytes.length);
    out[out.length - 1] = { kind: OP_LIT, bytes: merged };
  } else {
    out.push(op);
  }
}
