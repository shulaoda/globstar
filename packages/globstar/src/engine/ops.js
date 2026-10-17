// The linear IR between the parser and the engines, and the AST → ops
// lowering that produces it. JS twin of `crates/globstar/src/engine/ops.rs`.

export const OP_LIT = 0;
export const OP_ANYCHAR = 1;
export const OP_STAR = 2;
export const OP_CLASS = 3;
export const OP_SEP = 4;
// `**/`: zero or more whole segments, empty ones included, so the
// separators around it are lenient (§12.3).
export const OP_OPT_SEGMENTS_SLASH = 5;
// A `**` that ends the pattern: the rest of the path.
export const OP_GLOBSTAR_ANY = 6;
export const OP_ALTERNATION = 7;

// `a` copied to its length. V8 grows an array that `push` fills by half
// again plus 16 slots and never gives the room back; a matcher keeps its
// arrays as long as it lives, so the ones it stores are copied once.
export function exact(a) {
  return a.slice();
}

const ANYCHAR_OP = Object.freeze({ kind: OP_ANYCHAR });
const STAR_OP = Object.freeze({ kind: OP_STAR });
const SEP_OP = Object.freeze({ kind: OP_SEP });
const OSS_OP = Object.freeze({ kind: OP_OPT_SEGMENTS_SLASH });
const GSTAR_ANY_OP = Object.freeze({ kind: OP_GLOBSTAR_ANY });

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
  concat,
  sep,
} from "../ast.js";

// Lower an AST into ops: adjacent literals joined, a `*` run one `*`,
// and every `**` folded with the `/` after it.
export function lower(node) {
  const ops = [];
  lowerInto(node, ops);
  return ops;
}

function lowerInto(node, out) {
  switch (node.tag) {
    case N_LITERAL:
      pushOp(out, { kind: OP_LIT, str: node.str });
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
      pushOp(out, { kind: OP_CLASS, cls: node });
      return;
    case N_CONCAT: {
      const children = node.children;
      for (let i = 0; i < children.length; i++) {
        const child = children[i];
        const sepNext = children[i + 1]?.tag === N_SEPARATOR;
        if (child.tag === N_GLOBSTAR && sepNext) {
          // `**/` is OptSegmentsSlash, a bare `**` GlobstarAny.
          out.push(OSS_OP);
          i++;
        } else if (child.tag === N_BRACE && sepNext && child.branches.some(trailsGlobstar)) {
          // A brace with a `**` at its end takes the `/` after it into
          // every branch, so that `**` folds there too: `{a/**,b}/x`
          // lowers as `{a/**/,b/}x`.
          const withSep = (b) => concat([...(b.tag === N_CONCAT ? b.children : [b]), sep()]);
          lowerInto(brace(child.branches.map(withSep)), out);
          i++;
        } else {
          lowerInto(child, out);
        }
      }
      return;
    }
    case N_BRACE: {
      const branches = [];
      for (const branch of node.branches) {
        const branchOps = [];
        lowerInto(branch, branchOps);
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
    out[out.length - 1] = { kind: OP_LIT, str: out[out.length - 1].str + op.str };
  } else {
    out.push(op);
  }
}

function trailsGlobstar(node) {
  if (node.tag === N_GLOBSTAR) return true;
  if (node.tag === N_CONCAT) {
    return node.children.length > 0 && trailsGlobstar(node.children[node.children.length - 1]);
  }
  return node.tag === N_BRACE && node.branches.some(trailsGlobstar);
}
