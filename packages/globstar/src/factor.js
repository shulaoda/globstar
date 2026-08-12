import { N_CONCAT, N_GLOBSTAR, N_LITERAL, N_SEPARATOR, brace, concat, lit } from "./ast.js";

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

function nodeEq(a, b) {
  if (a === b) return true;
  if (a.tag !== b.tag || a.tag !== N_LITERAL) return false;
  if (a.bytes.length !== b.bytes.length) return false;
  for (let i = 0; i < a.bytes.length; i++) {
    if (a.bytes[i] !== b.bytes[i]) return false;
  }
  return true;
}

function rangeEq(seqA, offA, seqB, offB, len) {
  for (let k = 0; k < len; k++) {
    if (!nodeEq(seqA[offA + k], seqB[offB + k])) return false;
  }
  return true;
}

function foldGroupAtEdge(a, b, c) {
  if (a === undefined) return 0;
  if (a.tag === N_GLOBSTAR) return b?.tag === N_SEPARATOR ? 2 : 1;
  if (a.tag === N_SEPARATOR && b?.tag === N_GLOBSTAR) return c?.tag === N_SEPARATOR ? 3 : 2;
  return 1;
}

function liftPrefix(seqs) {
  const lifted = [];

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

  if (!seqs.every((s) => s.length > 0 && s[0].tag === N_LITERAL)) return lifted;
  const lits = seqs.map((s) => s[0].bytes);
  const min = lits.reduce((m, l) => Math.min(m, l.length), Infinity);
  let n = 0;
  while (n < min && lits.every((l) => l[n] === lits[0][n])) n++;
  if (n === 0) return lifted;
  lifted.push(lit(lits[0].slice(0, n)));
  for (const s of seqs) {
    const remaining = s[0].bytes.slice(n);
    if (remaining.length === 0) s.shift();
    else s[0] = lit(remaining);
  }
  return lifted;
}

function liftSuffix(seqs) {
  const liftedReverse = [];

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
    for (let k = size - 1; k >= 0; k--) liftedReverse.push(seqs[0][len0 - size + k]);
    for (const s of seqs) s.length -= size;
  }

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
