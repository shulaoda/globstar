import {
  N_ANYCHAR,
  N_BRACE,
  N_CLASS,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  N_STAR,
  brace,
  classExpandedAsciiCi,
  concat,
  sep,
} from "../../ast.js";
import { LiteralFacts } from "../facts.js";
import {
  ANYCHAR_OP,
  GLOBSTAR_OP,
  GSTAR_ANY_OP,
  LEADING_SEPS_OP,
  OP_ALTERNATION,
  OP_CLASS,
  OP_GLOBSTAR,
  OP_LIT,
  OP_OPT_SEGMENTS_SLASH,
  OP_SEP,
  OP_STAR,
  OSS_OP,
  SEP_OP,
  SEP_RUN_OP,
  SLASH_ANY_OP,
  STAR_OP,
} from "./ir.js";

export function lower(node, caseInsensitive) {
  const ops = [];
  const flag = { needsDistribution: false };
  lowerInto(node, ops, caseInsensitive, flag);
  if (flag.needsDistribution) {
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

function applyLeadingSepsAtStart(ops) {
  applyLeadingSeps(ops, false);
}

// `tailIsOss`: the enclosing program continues with an OptSegmentsSlash
// (union factoring lifts shared trailing `**/` behind the alternation),
// so an empty branch's fork starts with `**/` and needs the §8.5
// LeadingSeps.
function applyLeadingSeps(ops, tailIsOss) {
  if (ops.length === 0) return;
  const nextIsOss = ops.length > 1 ? ops[1].kind === OP_OPT_SEGMENTS_SLASH : tailIsOss;
  const first = ops[0];
  if (first.kind === OP_OPT_SEGMENTS_SLASH) {
    ops.unshift(LEADING_SEPS_OP);
  } else if (first.kind === OP_ALTERNATION) {
    for (const branch of first.branches) {
      if (branch.length === 0) {
        if (nextIsOss) branch.push(LEADING_SEPS_OP);
      } else {
        applyLeadingSeps(branch, nextIsOss);
      }
    }
  }
}

function distributeSeps(node) {
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
      const prevIsSep = out.length > 0 && out[out.length - 1].tag === N_SEPARATOR;
      const prevSepOwnedByGlobstar =
        prevIsSep && out.length >= 2 && out[out.length - 2].tag === N_GLOBSTAR;
      const absorbPrev = prevIsSep && !prevSepOwnedByGlobstar && child.branches.some(leadsGlobstar);
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
        // Loop, not spread: ~64k children would overflow the argument limit.
        if (branch.tag === N_CONCAT) for (const child of branch.children) sequence.push(child);
        else sequence.push(branch);
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

function upgradeTrailingSeps(branches) {
  for (const branch of branches) {
    const last = branch.length > 0 ? branch[branch.length - 1] : undefined;
    if (last === undefined) continue;
    if (last.kind === OP_SEP) branch[branch.length - 1] = SEP_RUN_OP;
    else if (last.kind === OP_ALTERNATION) upgradeTrailingSeps(last.branches);
  }
}

function foldGlobstars(ops) {
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
    if (ops[i + 1].kind !== OP_OPT_SEGMENTS_SLASH) continue;
    if (ops[i].kind === OP_SEP) ops[i] = SEP_RUN_OP;
    // A branch-trailing `/` before `**/` is `**`-adjacent too:
    // `{src/,lib/}**/x` ≡ `src/**/x` ∪ `lib/**/x` (§7.0 + §12.3).
    else if (ops[i].kind === OP_ALTERNATION) upgradeTrailingSeps(ops[i].branches);
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
