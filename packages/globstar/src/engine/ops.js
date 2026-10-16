// The linear IR between the parser and the engines, and the AST → ops
// lowering that produces it. JS twin of `crates/globstar/src/engine/ops.rs`.

export const OP_LIT = 0;
export const OP_ANYCHAR = 1;
export const OP_STAR = 2;
export const OP_CLASS = 3;
export const OP_SEP = 4;
export const OP_SEP_RUN = 5;
export const OP_OPT_SEGMENTS_SLASH = 6;
export const OP_SLASH_ANYTHING = 7;
export const OP_GLOBSTAR_ANY = 8;
export const OP_LEADING_SEPS = 9;
export const OP_ALTERNATION = 10;

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

// Lower an AST into one normalized program.
export function lower(node, caseInsensitive) {
  const ops = [];
  lowerInto(node, ops, caseInsensitive);
  leadSeps(ops);
  return { ops, caseInsensitive: !!caseInsensitive };
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
      out.push(GSTAR_ANY_OP);
      return;
    case N_CLASS:
      pushOp(out, { kind: OP_CLASS, cls: caseInsensitive ? classExpandedAsciiCi(node) : node });
      return;
    case N_CONCAT: {
      const children = node.children;
      for (let i = 0; i < children.length; i++) {
        let child = children[i];
        if (child.tag === N_GLOBSTAR) {
          // `**/` is OptSegmentsSlash, and the `/` before it (a brace's
          // trailing ones too) is lenient (§12.3); `/**` is SlashAnything;
          // a bare `**` is GlobstarAny.
          const prev = out[out.length - 1];
          if (children[i + 1]?.tag === N_SEPARATOR) {
            if (prev === SEP_OP) out[out.length - 1] = SEP_RUN_OP;
            else if (prev?.kind === OP_ALTERNATION) upgradeTrailingSeps(prev.branches);
            out.push(OSS_OP);
            i++;
          } else if (prev === SEP_OP) {
            out[out.length - 1] = SLASH_ANY_OP;
          } else {
            out.push(GSTAR_ANY_OP);
          }
          continue;
        }
        if (child.tag === N_BRACE) {
          // A brace with a `**` at an edge takes the `/` beside it into
          // every branch, so the `**/` or `/**` fold happens inside the
          // branch: `{a/**,b}/x` lowers as `{a/**/,b/}x`.
          const n = out.length;
          const prev = n > 0 && out[n - 1] === SEP_OP && child.branches.some(leadsGlobstar);
          const next = children[i + 1]?.tag === N_SEPARATOR && child.branches.some(trailsGlobstar);
          if (prev) out.pop();
          if (next) i++;
          if (prev || next) child = withSeps(child, prev, next);
          // Two braces around one `/`, both with a `**` at that edge: the
          // first took the `/`, so a `**/` opening a branch of the second
          // keeps its lenient boundary (§12.3) by taking any further
          // separators itself.
          const afterBrace = out.length > 0 && out[out.length - 1].kind === OP_ALTERNATION;
          lowerInto(child, out, caseInsensitive);
          if (afterBrace) for (const branch of out[out.length - 1].branches) leadSeps(branch);
          continue;
        }
        lowerInto(child, out, caseInsensitive);
      }
      return;
    }
    case N_BRACE: {
      const branches = [];
      for (const branch of node.branches) {
        const branchOps = [];
        lowerInto(branch, branchOps, caseInsensitive);
        branches.push(branchOps);
      }
      pushOp(out, { kind: OP_ALTERNATION, branches });
      return;
    }
  }
}

function withSeps(node, prev, next) {
  return brace(
    node.branches.map((branch) => {
      const seq = [];
      if (prev) seq.push(sep());
      if (branch.tag === N_CONCAT) for (const c of branch.children) seq.push(c);
      else seq.push(branch);
      if (next) seq.push(sep());
      return concat(seq);
    }),
  );
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

// A `**/` that opens the program, or a branch at its head, also takes the
// leading separators (§8.5).
function leadSeps(ops) {
  const first = ops[0];
  if (first?.kind === OP_OPT_SEGMENTS_SLASH) ops.unshift(LEADING_SEPS_OP);
  else if (first?.kind === OP_ALTERNATION) for (const branch of first.branches) leadSeps(branch);
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

function upgradeTrailingSeps(branches) {
  for (const branch of branches) {
    const last = branch[branch.length - 1];
    if (last?.kind === OP_SEP) branch[branch.length - 1] = SEP_RUN_OP;
    else if (last?.kind === OP_ALTERNATION) upgradeTrailingSeps(last.branches);
  }
}
