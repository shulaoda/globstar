// The linear IR between the parser and the engines, and the AST → ops
// lowering that produces it. JS twin of `crates/globstar/src/engine/ops.rs`.

export const OP_LIT = 0;
export const OP_ANYCHAR = 1;
export const OP_STAR = 2;
export const OP_CLASS = 3;
export const OP_SEP = 4;
export const OP_SEP_RUN = 5;
export const OP_GLOBSTAR = 6;
export const OP_OPT_SEGMENTS_SLASH = 7;
export const OP_SLASH_ANYTHING = 8;
export const OP_GLOBSTAR_ANY = 9;
export const OP_LEADING_SEPS = 10;
export const OP_ALTERNATION = 11;

// `a` copied to its length. V8 grows an array that `push` fills by half
// again plus 16 slots and never gives the room back; a matcher keeps its
// arrays as long as it lives, so the ones it stores are copied once.
export function exact(a) {
  return a.slice();
}

const ANYCHAR_OP = Object.freeze({ kind: OP_ANYCHAR });
const STAR_OP = Object.freeze({ kind: OP_STAR });
const SEP_OP = Object.freeze({ kind: OP_SEP });
const SEP_RUN_OP = Object.freeze({ kind: OP_SEP_RUN });
const GLOBSTAR_OP = Object.freeze({ kind: OP_GLOBSTAR });
const OSS_OP = Object.freeze({ kind: OP_OPT_SEGMENTS_SLASH });
const SLASH_ANY_OP = Object.freeze({ kind: OP_SLASH_ANYTHING });
const GSTAR_ANY_OP = Object.freeze({ kind: OP_GLOBSTAR_ANY });
const LEADING_SEPS_OP = Object.freeze({ kind: OP_LEADING_SEPS });

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
} from "../ast.js";

// Nodes are shared, not copied, so a brace that needs the `/` beside it
// (see `absorbSeps`) is cheapest to find while lowering; the rare second
// pass costs nothing on every other pattern.
export function lower(node, caseInsensitive) {
  const ops = [];
  const flag = { needsAbsorb: false };
  lowerInto(node, ops, caseInsensitive, flag);
  if (flag.needsAbsorb) {
    ops.length = 0;
    lowerInto(absorbSeps(node), ops, caseInsensitive, flag);
  }
  foldGlobstars(ops);
  applyLeadingSeps(ops, false);
  const ci = !!caseInsensitive;
  return { ops, caseInsensitive: ci };
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
            flag.needsAbsorb = true;
          }
        }
        lowerInto(child, out, caseInsensitive, flag);
        // Two braces around one `/`, both with a `**` at that edge:
        // `absorbSeps` gave the `/` to the first, so a `**/` opening a
        // branch of the second keeps its lenient boundary (§12.3) by taking
        // any further separators itself.
        if (i > 0 && children[i - 1].tag === N_BRACE && child.tag === N_BRACE) {
          takeLeadingSeps(out[out.length - 1].branches);
        }
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

function takeLeadingSeps(branches) {
  for (const branch of branches) {
    const first = branch[0];
    if (first?.kind === OP_OPT_SEGMENTS_SLASH) branch.unshift(LEADING_SEPS_OP);
    else if (first?.kind === OP_ALTERNATION) takeLeadingSeps(first.branches);
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

// `tailIsOss`: the enclosing program continues with an OptSegmentsSlash
// (union factoring lifts shared trailing `**/` behind the alternation),
// so an empty branch's expansion starts with `**/` and needs the §8.5
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

// Gives a brace the `/` beside it when a branch has a `**` at that edge, so
// the `**/` or `/**` fold happens inside the branch: `{a/**,b}/x` becomes
// `{a/**/,b/}x`. A `/` already owned by a `**` token stays.
function absorbSeps(node) {
  if (node.tag === N_CONCAT) {
    const out = [];
    const children = node.children;
    let i = 0;
    while (i < children.length) {
      const child = children[i];
      if (child.tag !== N_BRACE) {
        out.push(absorbSeps(child));
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
        out.push(absorbSeps(child));
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
        return absorbSeps(concat(sequence));
      });
      out.push(brace(branches));
      i += absorbNext ? 2 : 1;
    }
    return concat(out);
  }
  if (node.tag === N_BRACE) return brace(node.branches.map(absorbSeps));
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
