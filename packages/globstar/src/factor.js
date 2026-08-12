// AST-level factoring for `globstar(patterns)` brace branches.
//
// Without factoring, `globstar(["**/*.ts", "**/*.tsx", ...])` parses to
// N branches each carrying a duplicated `**/*` prefix; the program grows
// linearly with N. Lifting common leading + trailing fragments makes
// `union(["**/*.ts","**/*.tsx"])` equivalent to the hand-written
// `**/*.{ts,tsx}` — one shared prefix path through the segment program.
//
// Two phases per side (lifting from the front, then mirrored from the back):
//
//   1. Atomic fold-group lift — referential singletons (Sep / Globstar /
//      AnyChar / Star) and structurally-equal Lit nodes. Globstar +
//      flanking Sep are lifted as one atomic group so the lowering-pass
//      fold (`Globstar Sep` → `OptSegmentsSlash`, `Sep Globstar` →
//      `SlashAnything`) is preserved.
//   2. Byte-level lift on the next/last Lit when all branches share an
//      opening/closing byte run. Lits never participate in folds, so
//      this is always safe.

import { N_CONCAT, N_GLOBSTAR, N_LITERAL, N_SEPARATOR, brace, concat, lit } from "./ast.js";

// Returns a single `Node` with shared prefix/suffix lifted out; the
// residual branches are re-wrapped in a fresh brace, or returned bare
// when only one residual remains.
export function factorBranches(branches) {
  const seqs = branches.map((n) => (n.tag === N_CONCAT ? n.children.slice() : [n]));
  const prefix = liftPrefix(seqs);
  const suffix = liftSuffix(seqs);

  const residual = seqs.map((s) =>
    s.length === 0 ? concat([]) : s.length === 1 ? s[0] : concat(s),
  );
  const inner = residual.length === 1 ? residual[0] : brace(residual);

  const out = prefix;
  if (inner.tag === N_CONCAT) out.push(...inner.children);
  else out.push(inner);
  out.push(...suffix);
  return out.length === 1 ? out[0] : concat(out);
}

// Structural equality for the node kinds we lift. Singletons (Sep,
// Globstar, AnyChar, Star) compare by reference; Literals byte-for-byte.
// Class / Concat / Brace deliberately compare unequal so they're never
// lifted: pulling a whole Brace out would tear it from a flanking `/`
// that distributeSeps must keep next to a globstar-edged branch, so
// `union` would stop equalling the OR of its members (e.g.
// `union(["{**,a}/**", "{**,a}/"])`).
function nodeEq(a, b) {
  if (a === b) return true;
  if (a.tag !== b.tag || a.tag !== N_LITERAL) return false;
  if (a.bytes.length !== b.bytes.length) return false;
  for (let i = 0; i < a.bytes.length; i++) {
    if (a.bytes[i] !== b.bytes[i]) return false;
  }
  return true;
}

// Slice equality for fold groups. Caller guarantees both ranges are valid.
function rangeEq(seqA, offA, seqB, offB, len) {
  for (let k = 0; k < len; k++) {
    if (!nodeEq(seqA[offA + k], seqB[offB + k])) return false;
  }
  return true;
}

// Size of the fold group at one edge of a branch, read inward from the
// edge (liftSuffix passes the nodes reversed; the group shapes are
// symmetric, so one table serves both sides). Mirrors the foldGlobstars
// passes in `engine/ops/index.js` — lifting a partial group would change
// the lowered semantics, so the lift loops below only consume whole
// groups.
//
//   - `Globstar [Sep]`     → 2 (or 1 with no adjacent Sep)
//   - `Sep Globstar [Sep]` → 2 or 3 (`/**` or mid-pattern `/**/`)
//   - empty → 0; anything else → 1 (atomic)
function foldGroupAtEdge(a, b, c) {
  if (a === undefined) return 0;
  if (a.tag === N_GLOBSTAR) return b?.tag === N_SEPARATOR ? 2 : 1;
  if (a.tag === N_SEPARATOR && b?.tag === N_GLOBSTAR) return c?.tag === N_SEPARATOR ? 3 : 2;
  return 1;
}

function liftPrefix(seqs) {
  const lifted = [];

  // Phase 1: atomic fold groups shared across all branches.
  while (true) {
    const size = foldGroupAtEdge(seqs[0][0], seqs[0][1], seqs[0][2]);
    if (size === 0) return lifted;
    const same = seqs.every(
      (s, i) =>
        i === 0 || (foldGroupAtEdge(s[0], s[1], s[2]) === size && rangeEq(s, 0, seqs[0], 0, size)),
    );
    if (!same) break;
    for (let k = 0; k < size; k++) lifted.push(seqs[0][k]);
    for (const s of seqs) s.splice(0, size);
  }

  // Phase 2: byte-level Lit prefix. Lits are never fold-bound.
  if (!seqs.every((s) => s.length > 0 && s[0].tag === N_LITERAL)) return lifted;
  const lits = seqs.map((s) => s[0].bytes);
  const min = lits.reduce((m, l) => Math.min(m, l.length), Infinity);
  let n = 0;
  while (n < min && lits.every((l) => l[n] === lits[0][n])) n++;
  if (n === 0) return lifted;
  // Uint8Arrays are immutable in length — strip the prefix by replacing
  // the head Lit with a fresh `slice(n)` per branch.
  lifted.push(lit(lits[0].slice(0, n)));
  for (const s of seqs) {
    const remaining = s[0].bytes.slice(n);
    if (remaining.length === 0) s.shift();
    else s[0] = lit(remaining);
  }
  return lifted;
}

function liftSuffix(seqs) {
  // Build outermost-first, then reverse once at the end so the caller
  // sees natural inner→outer order.
  const liftedReverse = [];

  // Phase 1: atomic fold groups at the trailing edge.
  while (true) {
    const len0 = seqs[0].length;
    const size = foldGroupAtEdge(seqs[0][len0 - 1], seqs[0][len0 - 2], seqs[0][len0 - 3]);
    if (size === 0) break;
    const same = seqs.every(
      (s, i) =>
        i === 0 ||
        (foldGroupAtEdge(s[s.length - 1], s[s.length - 2], s[s.length - 3]) === size &&
          rangeEq(s, s.length - size, seqs[0], len0 - size, size)),
    );
    if (!same) break;
    // Push trailing-range in reverse so the elements land in
    // outermost-first order (matched by the final `reverse()`).
    for (let k = size - 1; k >= 0; k--) liftedReverse.push(seqs[0][len0 - size + k]);
    for (const s of seqs) s.length -= size;
  }

  // Phase 2: byte-level Lit suffix.
  if (seqs.every((s) => s.length > 0 && s[s.length - 1].tag === N_LITERAL)) {
    const lits = seqs.map((s) => s[s.length - 1].bytes);
    const min = lits.reduce((m, l) => Math.min(m, l.length), Infinity);
    let n = 0;
    while (n < min && lits.every((l) => l[l.length - 1 - n] === lits[0][lits[0].length - 1 - n])) {
      n++;
    }
    if (n > 0) {
      const ref = lits[0];
      liftedReverse.push(lit(ref.slice(ref.length - n)));
      for (const s of seqs) {
        const last = s[s.length - 1];
        const remaining = last.bytes.slice(0, last.bytes.length - n);
        if (remaining.length === 0) s.pop();
        else s[s.length - 1] = lit(remaining);
      }
    }
  }

  return liftedReverse.reverse();
}
