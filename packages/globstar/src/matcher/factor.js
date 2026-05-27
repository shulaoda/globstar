// AST-level factoring for `globstar(patterns)` brace branches.
//
// Without factoring, `globstar(["**/*.ts", "**/*.tsx", ...])` parses to
// N branches each carrying a duplicated `**/*` prefix; the DFA grows
// linearly with N. Lifting common leading + trailing fragments makes
// `union(["**/*.ts","**/*.tsx"])` equivalent to the hand-written
// `**/*.{ts,tsx}` — one shared prefix path through the DFA.
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
  const seqs = branches.map(intoSeq);
  const prefix = liftPrefix(seqs);
  const suffix = liftSuffix(seqs);

  const inner = seqs.length === 1 ? fromSeq(seqs[0]) : brace(seqs.map(fromSeq));
  if (prefix.length === 0 && suffix.length === 0) return inner;

  const out = prefix.slice();
  if (inner.tag === N_CONCAT) out.push(...inner.children);
  else out.push(inner);
  out.push(...suffix);
  return out.length === 1 ? out[0] : concat(out);
}

function intoSeq(node) {
  return node.tag === N_CONCAT ? node.children.slice() : [node];
}

function fromSeq(seq) {
  if (seq.length === 0) return concat([]); // epsilon branch
  if (seq.length === 1) return seq[0];
  return concat(seq);
}

// Structural equality for the node kinds we lift. Singletons (Sep,
// Globstar, AnyChar, Star) compare by reference; Literals compare
// byte-for-byte. Class / Concat / Brace fall through to false — rare
// enough that the full-node fast path wins what it can and the rest
// pays no extra work.
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

// Size of the fold group anchored at `seq[i]` looking forward. Mirrors
// the `foldGlobstars` passes in `engine/ops.js`. Lifting a partial
// group would change the lowered semantics, so the lift loops below
// only consume whole groups.
//
//   - `Globstar [Sep]`     → 2 (or 1 if no trailing Sep)
//   - `Sep Globstar [Sep]` → 2 or 3 (matches `/**` or mid-pattern `/**/`)
//   - anything else        → 1 (atomic)
function foldGroupAtStart(seq, i) {
  const a = seq[i];
  if (a === undefined) return 0;
  if (a.tag === N_GLOBSTAR) {
    return seq[i + 1]?.tag === N_SEPARATOR ? 2 : 1;
  }
  if (a.tag === N_SEPARATOR && seq[i + 1]?.tag === N_GLOBSTAR) {
    return seq[i + 2]?.tag === N_SEPARATOR ? 3 : 2;
  }
  return 1;
}

// Mirror of `foldGroupAtStart` for the trailing edge.
function foldGroupAtEnd(seq) {
  const len = seq.length;
  if (len === 0) return 0;
  const last = seq[len - 1];
  if (last.tag === N_GLOBSTAR) {
    return seq[len - 2]?.tag === N_SEPARATOR ? 2 : 1;
  }
  if (last.tag === N_SEPARATOR && seq[len - 2]?.tag === N_GLOBSTAR) {
    return seq[len - 3]?.tag === N_SEPARATOR ? 3 : 2;
  }
  return 1;
}

// Longest common opening byte run across N Uint8Arrays.
function commonBytePrefix(byteArrays) {
  if (byteArrays.length === 0) return 0;
  const min = byteArrays.reduce((m, l) => Math.min(m, l.length), Infinity);
  for (let n = 0; n < min; n++) {
    const b = byteArrays[0][n];
    for (let i = 1; i < byteArrays.length; i++) {
      if (byteArrays[i][n] !== b) return n;
    }
  }
  return min;
}

// Longest common closing byte run across N Uint8Arrays.
function commonByteSuffix(byteArrays) {
  if (byteArrays.length === 0) return 0;
  const min = byteArrays.reduce((m, l) => Math.min(m, l.length), Infinity);
  for (let n = 0; n < min; n++) {
    const b = byteArrays[0][byteArrays[0].length - 1 - n];
    for (let i = 1; i < byteArrays.length; i++) {
      const l = byteArrays[i];
      if (l[l.length - 1 - n] !== b) return n;
    }
  }
  return min;
}

function liftPrefix(seqs) {
  const lifted = [];

  // Phase 1: atomic fold groups shared across all branches.
  while (true) {
    const size = foldGroupAtStart(seqs[0], 0);
    if (size === 0) return lifted;
    const same = seqs.every(
      (s, i) => i === 0 || (foldGroupAtStart(s, 0) === size && rangeEq(s, 0, seqs[0], 0, size)),
    );
    if (!same) break;
    for (let k = 0; k < size; k++) lifted.push(seqs[0][k]);
    for (const s of seqs) s.splice(0, size);
  }

  // Phase 2: byte-level Lit prefix. Lits are never fold-bound.
  if (!seqs.every((s) => s.length > 0 && s[0].tag === N_LITERAL)) return lifted;
  const lits = seqs.map((s) => s[0].bytes);
  const n = commonBytePrefix(lits);
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
    const size = foldGroupAtEnd(seqs[0]);
    if (size === 0) break;
    const len0 = seqs[0].length;
    const same = seqs.every(
      (s, i) =>
        i === 0 ||
        (foldGroupAtEnd(s) === size && rangeEq(s, s.length - size, seqs[0], len0 - size, size)),
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
    const n = commonByteSuffix(lits);
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
