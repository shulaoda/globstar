// AST shared by the parser and the lowering pass.
//
// Node shape: `{ tag, ... }`. Zero-field tags (Sep / AnyChar / Star /
// Globstar) reuse a frozen module-level singleton — a typical pattern
// has 5-15 of these, so singleton-ing skips that many per-parse object
// allocations. Bigger nodes (Literal, Class, Brace, Concat) carry
// payload arrays so they are allocated per parse.

import { isPathSep, asciiCaseAlt } from "./options.js";

// Node tags.
export const N_CONCAT = 0;
export const N_LITERAL = 1;
export const N_SEPARATOR = 2;
export const N_ANYCHAR = 3;
export const N_STAR = 4;
export const N_GLOBSTAR = 5;
export const N_CLASS = 6;
export const N_BRACE = 7;

// ClassItem tags (used by N_CLASS payload).
export const CI_BYTE = 0;
export const CI_RANGE = 1;

const SEP_NODE = Object.freeze({ tag: N_SEPARATOR });
const ANYCHAR_NODE = Object.freeze({ tag: N_ANYCHAR });
const STAR_NODE = Object.freeze({ tag: N_STAR });
const GLOBSTAR_NODE = Object.freeze({ tag: N_GLOBSTAR });

export function lit(bytes) {
  return { tag: N_LITERAL, bytes };
}
export function sep() {
  return SEP_NODE;
}
export function anyChar() {
  return ANYCHAR_NODE;
}
export function star() {
  return STAR_NODE;
}
export function globstar() {
  return GLOBSTAR_NODE;
}
export function klass(neg, items) {
  return { tag: N_CLASS, neg, items };
}
export function brace(branches) {
  return { tag: N_BRACE, branches };
}
export function concat(children) {
  return { tag: N_CONCAT, children };
}

export function classItemByte(b) {
  return { tag: CI_BYTE, b };
}
export function classItemRange(lo, hi) {
  return { tag: CI_RANGE, lo, hi };
}

// `cls.matches(b)` semantics. Path separators are never class members
// regardless of polarity (GLOB_SPEC §6.2 / §12.3 — classes are
// segment-local). The negated polarity check happens AFTER iteration.
export function classMatches(cls, b) {
  if (isPathSep(b)) return false;
  const items = cls.items;
  let listed = false;
  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    if (it.tag === CI_BYTE ? it.b === b : b >= it.lo && b <= it.hi) {
      listed = true;
      break;
    }
  }
  return cls.neg ? !listed : listed;
}

// Return a copy of `cls` with ASCII case-alternates added so `[A]` matches
// `A` and `a`, `[A-Z]` matches `[A-Za-z]`, etc. ASCII-only by design
// (GLOB_SPEC §11.3 / §12.5).
export function classExpandedAsciiCi(cls) {
  const items = [];
  for (const it of cls.items) {
    items.push(it);
    if (it.tag === CI_BYTE) {
      const alt = asciiCaseAlt(it.b);
      if (alt !== it.b) items.push(classItemByte(alt));
    } else {
      const { lo, hi } = it;
      // Pure-upper or pure-lower ranges fold via a symmetric range; mixed
      // ranges fall back to per-byte items.
      if (lo >= 0x41 && hi <= 0x5a) {
        items.push(classItemRange(lo | 0x20, hi | 0x20));
      } else if (lo >= 0x61 && hi <= 0x7a) {
        items.push(classItemRange(lo & ~0x20, hi & ~0x20));
      } else {
        for (let b = lo; b <= hi; b++) {
          const alt = asciiCaseAlt(b);
          if (alt !== b) items.push(classItemByte(alt));
        }
      }
    }
  }
  return { neg: cls.neg, items };
}

// Render a pure-literal AST to its byte sequence. Returns null for any
// non-literal subtree — caller routes to the literal matcher iff
// non-null. Single-pass: skips the separate predicate-then-render walk.
export function nodeToLiteralBytes(n) {
  const out = [];
  return appendLiteralBytes(n, out) ? Uint8Array.from(out) : null;
}
function appendLiteralBytes(n, out) {
  if (n.tag === N_LITERAL) {
    for (let i = 0; i < n.bytes.length; i++) out.push(n.bytes[i]);
    return true;
  }
  if (n.tag === N_SEPARATOR) {
    out.push(0x2f);
    return true;
  }
  if (n.tag === N_CONCAT) {
    for (const x of n.children) {
      if (!appendLiteralBytes(x, out)) return false;
    }
    return true;
  }
  return false;
}
