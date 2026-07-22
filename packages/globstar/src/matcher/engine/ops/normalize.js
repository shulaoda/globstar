// AST and raw-op normalization: brace separator ownership and globstar folds.

import { N_BRACE, N_CONCAT, N_GLOBSTAR, N_SEPARATOR, brace, concat, sep } from "../../ast.js";
import {
  GSTAR_ANY_OP,
  LEADING_SEPS_OP,
  OP_ALTERNATION,
  OP_GLOBSTAR,
  OP_OPT_SEGMENTS_SLASH,
  OP_SEP,
  OSS_OP,
  SEP_RUN_OP,
  SLASH_ANY_OP,
} from "./ir.js";

export function needsSepDistribution(node) {
  if (node.tag === N_CONCAT) {
    const children = node.children;
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      if (child.tag === N_BRACE) {
        const prevSep = i > 0 && children[i - 1].tag === N_SEPARATOR;
        const nextSep = i + 1 < children.length && children[i + 1].tag === N_SEPARATOR;
        if (
          (prevSep && child.branches.some(leadsGlobstar)) ||
          (nextSep && child.branches.some(trailsGlobstar))
        ) {
          return true;
        }
      }
      if (needsSepDistribution(child)) return true;
    }
    return false;
  }
  return node.tag === N_BRACE && node.branches.some(needsSepDistribution);
}

function leadsGlobstar(node) {
  if (node.tag === N_GLOBSTAR) return true;
  if (node.tag === N_CONCAT) return node.children.length > 0 && leadsGlobstar(node.children[0]);
  return node.tag === N_BRACE && node.branches.some(leadsGlobstar);
}

function trailsGlobstar(node) {
  if (node.tag === N_GLOBSTAR) return true;
  if (node.tag === N_CONCAT) {
    return node.children.length > 0 && trailsGlobstar(node.children[node.children.length - 1]);
  }
  return node.tag === N_BRACE && node.branches.some(trailsGlobstar);
}

export function distributeSeps(node) {
  if (node.tag === N_CONCAT) {
    const out = [];
    const children = node.children;
    let i = 0;
    while (i < children.length) {
      const child = children[i];
      if (child.tag !== N_BRACE) {
        out.push(distributeSeps(child));
        i++;
        continue;
      }
      const absorbPrev =
        out.length > 0 &&
        out[out.length - 1].tag === N_SEPARATOR &&
        child.branches.some(leadsGlobstar);
      const absorbNext =
        i + 1 < children.length &&
        children[i + 1].tag === N_SEPARATOR &&
        child.branches.some(trailsGlobstar);
      if (!absorbPrev && !absorbNext) {
        out.push(distributeSeps(child));
        i++;
        continue;
      }
      if (absorbPrev) out.pop();
      const branches = child.branches.map((branch) => {
        const sequence = [];
        if (absorbPrev) sequence.push(sep());
        sequence.push(branch);
        if (absorbNext) sequence.push(sep());
        return distributeSeps(concat(sequence));
      });
      out.push(brace(branches));
      i += absorbNext ? 2 : 1;
    }
    return concat(out);
  }
  if (node.tag === N_BRACE) return brace(node.branches.map(distributeSeps));
  return node;
}

export function foldGlobstars(ops) {
  let write = 0;
  let read = 0;
  while (read < ops.length) {
    if (ops[read].kind === OP_GLOBSTAR && ops[read + 1]?.kind === OP_SEP) {
      ops[write++] = OSS_OP;
      read += 2;
    } else {
      if (read !== write) ops[write] = ops[read];
      read++;
      write++;
    }
  }
  ops.length = write;

  for (let i = 0; i + 1 < ops.length; i++) {
    if (ops[i].kind === OP_SEP && ops[i + 1].kind === OP_OPT_SEGMENTS_SLASH) ops[i] = SEP_RUN_OP;
  }

  write = 0;
  read = 0;
  while (read < ops.length) {
    if (ops[read].kind === OP_SEP && ops[read + 1]?.kind === OP_GLOBSTAR) {
      ops[write++] = SLASH_ANY_OP;
      read += 2;
    } else {
      if (read !== write) ops[write] = ops[read];
      read++;
      write++;
    }
  }
  ops.length = write;

  for (let i = 0; i < ops.length; i++) {
    if (ops[i].kind === OP_GLOBSTAR) ops[i] = GSTAR_ANY_OP;
  }
}

export function applyLeadingSepsAtStart(ops) {
  if (ops.length === 0) return;
  const first = ops[0];
  if (first.kind === OP_OPT_SEGMENTS_SLASH) {
    ops.unshift(LEADING_SEPS_OP);
  } else if (first.kind === OP_ALTERNATION) {
    for (const branch of first.branches) applyLeadingSepsAtStart(branch);
  }
}
