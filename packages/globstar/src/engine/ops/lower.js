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
  leadsGlobstar,
  trailsGlobstar,
} from "./normalize.js";

// Optimistic single pass: lowerInto emits ops directly and, in the same
// walk, flags whether any brace edge abuts a separator — the sole shape
// that needs §7 separator distribution. Only when that trips (rare) do we
// redo on the distributed tree. The common pattern pays exactly one walk.
export function lower(node, caseInsensitive) {
  const ops = [];
  const flag = { needsDistribution: false };
  lowerInto(node, ops, caseInsensitive, flag);
  if (flag.needsDistribution) {
    // The flag is a conservative superset of what distributeSeps actually
    // rewrites (it leaves e.g. a `/` owned by a preceding `**` in place),
    // so the flag can re-trip on the distributed tree even though that
    // tree is already the fixpoint. Re-lower on it and ignore the re-trip.
    ops.length = 0;
    lowerInto(distributeSeps(node), ops, caseInsensitive, flag);
  }
  foldGlobstars(ops);
  applyLeadingSepsAtStart(ops);
  const ci = !!caseInsensitive;
  return { ops, facts: LiteralFacts.extract(ops, ci), caseInsensitive: ci };
}

function lowerInto(node, out, caseInsensitive, flag) {
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
    case N_CONCAT: {
      const children = node.children;
      for (let i = 0; i < children.length; i++) {
        const child = children[i];
        // Flag a brace edge abutting a separator — the only shape that
        // needs §7 separator distribution. `lower` then redoes the walk on
        // the distributed tree. Detected here so the common no-brace
        // pattern needs no separate pre-check walk.
        if (child.tag === N_BRACE) {
          const prevSep = i > 0 && children[i - 1].tag === N_SEPARATOR;
          const nextSep = i + 1 < children.length && children[i + 1].tag === N_SEPARATOR;
          if (
            (prevSep && child.branches.some(leadsGlobstar)) ||
            (nextSep && child.branches.some(trailsGlobstar))
          ) {
            flag.needsDistribution = true;
          }
        }
        lowerInto(child, out, caseInsensitive, flag);
      }
      return;
    }
    case N_BRACE: {
      const branches = [];
      for (const branch of node.branches) {
        const branchOps = [];
        lowerInto(branch, branchOps, caseInsensitive, flag);
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
