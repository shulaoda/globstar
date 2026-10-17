import { N_CONCAT, N_GLOBSTAR, N_LITERAL, N_SEPARATOR, brace, concat, lit } from "./ast.js";

// One body for the union of `branches`: what they all start or end with is
// lifted out of the brace, whole tokens first, then the bytes shared by their
// first or last literals. Bytes are lifted only behind a lifted token, so the
// union still starts the way its members do and keeps their static prefixes:
// `{src/**,spec/**}` stays, `s{rc,pec}/**` would not; `src/mod{1,2}/index.ts`
// is fine.
export function factorBranches(branches) {
  const seqs = branches.map((n) => (n.tag === N_CONCAT ? n.children.slice() : [n]));
  const prefix = liftTokens(seqs, true);
  const bytes = prefix.length > 0;
  const head = bytes ? liftBytes(seqs, true) : null;
  const suffix = liftTokens(seqs, false);
  const tail = bytes ? liftBytes(seqs, false) : null;

  // Members that differ only in spelling (`**` and `**/**`, `\a` and
  // `a`) leave nothing behind; one empty branch stands for them all, so
  // a union of equal members is that member again.
  const residual = [];
  let empty = false;
  for (const s of seqs) {
    if (s.length > 1) residual.push(concat(s));
    else if (s.length === 1) residual.push(s[0]);
    else if (!empty) {
      empty = true;
      residual.push(concat([]));
    }
  }
  const inner = residual.length === 1 ? residual[0] : brace(residual);

  // Loops, not spreads: ~64k lifted nodes would overflow the argument limit.
  const out = prefix;
  if (head !== null) out.push(head);
  if (inner.tag === N_CONCAT) for (const child of inner.children) out.push(child);
  else out.push(inner);
  if (tail !== null) out.push(tail);
  for (const node of suffix) out.push(node);
  return out.length === 1 ? out[0] : concat(out);
}

// The `k`th node from the front, or from the back.
const at = (s, k, front) => (front ? s[k] : s[s.length - 1 - k]);

function nodeEq(a, b) {
  return a === b || (a.tag === N_LITERAL && b.tag === N_LITERAL && a.str === b.str);
}

// How many nodes from `k` on stay together: `**/`, `/**/` and `/**` are
// lifted whole or not at all.
function group(s, k, front) {
  const a = at(s, k, front);
  const b = at(s, k + 1, front);
  if (a === undefined) return 0;
  if (a.tag === N_GLOBSTAR) return b?.tag === N_SEPARATOR ? 2 : 1;
  if (a.tag === N_SEPARATOR && b?.tag === N_GLOBSTAR) {
    return at(s, k + 2, front)?.tag === N_SEPARATOR ? 3 : 2;
  }
  return 1;
}

// The whole tokens every member starts (or ends) with, taken out of them.
function liftTokens(seqs, front) {
  const first = seqs[0];
  let n = 0;
  while (true) {
    const size = group(first, n, front);
    if (size === 0) break;
    const same = seqs.every((s, i) => {
      if (i === 0) return true;
      if (group(s, n, front) !== size) return false;
      for (let k = n; k < n + size; k++) {
        if (!nodeEq(at(s, k, front), at(first, k, front))) return false;
      }
      return true;
    });
    if (!same) break;
    n += size;
  }
  const lifted = front ? first.slice(0, n) : first.slice(first.length - n);
  if (n > 0) {
    for (const s of seqs) {
      if (front) s.splice(0, n);
      else s.length -= n;
    }
  }
  return lifted;
}

// The bytes every member's first (or last) literal shares, taken out of
// them, as one literal; `null` when there are none.
function liftBytes(seqs, front) {
  if (!seqs.every((s) => s.length > 0 && at(s, 0, front).tag === N_LITERAL)) return null;
  const lits = seqs.map((s) => at(s, 0, front).str);
  const min = lits.reduce((m, l) => Math.min(m, l.length), Infinity);
  const byte = (l, i) => l.charCodeAt(front ? i : l.length - 1 - i);
  let n = 0;
  while (n < min && lits.every((l) => byte(l, n) === byte(lits[0], n))) n++;
  if (n === 0) return null;
  for (const s of seqs) {
    const b = at(s, 0, front).str;
    const rest = front ? b.slice(n) : b.slice(0, b.length - n);
    const k = front ? 0 : s.length - 1;
    if (rest.length > 0) s[k] = lit(rest);
    else if (front) s.shift();
    else s.pop();
  }
  const ref = lits[0];
  return lit(front ? ref.slice(0, n) : ref.slice(ref.length - n));
}
