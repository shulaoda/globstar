// A union's members factored into one body.

import {
  ANYCHAR_NODE,
  GLOBSTAR_NODE,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  SEP_NODE,
  STAR_NODE,
  brace,
  concat,
} from "./parse/ast.js";

// What merging may read: this many nodes per node of the members, at
// least MIN_MERGE_READS.
const MERGE_READS_PER_NODE = 16;
const MIN_MERGE_READS = 4096;

// A member's part in a merge (`state`, a Uint8Array).
const FREE = 0;
// Its trailing literal became the brace.
const HEAD = 1;
// Joined a head's brace: dropped at the end.
const ABSORBED = 2;

const isWildcard = (n) => n === STAR_NODE || n === GLOBSTAR_NODE || n === ANYCHAR_NODE;

// The tokens all members start or end with are lifted out of the brace,
// and members that differ only in the literal ending one segment are
// merged into one with a brace there. Members left within one segment share a brace only when it is a
// set (literal branches) or holds no literal at all; a mix would be one
// in-segment NFA, so the tokens go back and the members stay apart.
export function factorBranches(branches) {
  const seqs = branches.map((n) => (n.tag === N_CONCAT ? n.children.slice() : [n]));
  let prefix = liftTokens(seqs, true);
  let suffix = liftTokens(seqs, false);
  let single = 0;
  let literals = true;
  let someLiteral = false;
  members: for (const s of seqs) {
    let hasLiteral = false;
    for (const n of s) {
      if (n === SEP_NODE) continue members;
      if (n.tag === N_LITERAL) hasLiteral = true;
    }
    single++;
    literals &&= s.length <= 1 && (s.length === 0 || hasLiteral);
    someLiteral ||= hasLiteral;
  }
  let aSet = literals;
  for (let i = 0; aSet && i < suffix.length && suffix[i] !== SEP_NODE; i++) {
    aSet = suffix[i].tag === N_LITERAL;
  }
  if (single > 1 && !aSet && someLiteral && (prefix.length > 0 || suffix.length > 0)) {
    for (let i = 0; i < seqs.length; i++) seqs[i] = prefix.concat(seqs[i], suffix);
    prefix = [];
    suffix = [];
  }
  const liftedWild = prefix.some(isWildcard) || suffix.some(isWildcard);

  // Members that differ only in spelling leave nothing behind: one empty
  // branch stands for them all.
  const residual = [];
  let empty = false;
  for (const s of mergeSegments(seqs, liftedWild)) {
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
  if (inner.tag === N_CONCAT) for (const child of inner.children) out.push(child);
  else out.push(inner);
  for (const node of suffix) out.push(node);
  return out.length === 1 ? out[0] : concat(out);
}

// Members that differ only in the literal ending one segment become one
// member with a brace there: `**/a/**` and `**/b/**` are `**/{a,b}/**`.
// Members with a wildcard take part (in the member or lifted). They are
// grouped by segment and a hash of everything but that literal, then
// compared node by node, from the last segment back; a member joins the
// deepest group it fits.
function mergeSegments(seqs, liftedWild) {
  // [segment, hash, member, node] per literal ending a segment.
  const entries = [];
  let reads = 0;
  let nodes = 0;
  for (let i = 0; i < seqs.length; i++) {
    const s = seqs[i];
    nodes += s.length;
    let wild = liftedWild;
    let plain = true;
    for (let j = 0; j < s.length && plain; j++) {
      const n = s[j];
      if (isWildcard(n)) wild = true;
      else if (n !== SEP_NODE && n.tag !== N_LITERAL) plain = false;
    }
    if (!(plain && wild && s.length > 1)) continue;
    let k = 0;
    for (let at = 0; at < s.length; at++) {
      const n = s[at];
      if (n === SEP_NODE) k++;
      else if (n.tag === N_LITERAL && (at + 1 === s.length || s[at + 1] === SEP_NODE)) {
        entries.push([k, 0, i, at]);
        reads += s.length;
      }
    }
  }
  if (entries.length < 2 || reads > Math.max(MERGE_READS_PER_NODE * nodes, MIN_MERGE_READS)) {
    return seqs;
  }
  for (const e of entries) e[1] = hashWithout(seqs[e[2]], e[3]);
  // A comparator sort costs V8 more than the rest of a small union's merge.
  const order = (x, y) => y[0] - x[0] || x[1] - y[1] || x[2] - y[2];
  if (entries.length > 8) entries.sort(order);
  else {
    for (let i = 1; i < entries.length; i++) {
      const e = entries[i];
      let j = i;
      for (; j > 0 && order(entries[j - 1], e) > 0; j--) entries[j] = entries[j - 1];
      entries[j] = e;
    }
  }
  const state = new Uint8Array(seqs.length);
  let absorbed = 0;
  for (let run = 0; run < entries.length; ) {
    const [k, hash] = entries[run];
    let end = run + 1;
    while (end < entries.length && entries[end][0] === k && entries[end][1] === hash) end++;
    let h = run;
    while (h < end && state[entries[h][2]] !== FREE) h++;
    if (h < end) {
      const [, , head, headAt] = entries[h];
      const lits = [];
      for (let e = h + 1; e < end; e++) {
        const [, , i, at] = entries[e];
        if (state[i] !== FREE || !sameBut(seqs[head], seqs[i], headAt)) continue;
        state[i] = ABSORBED;
        absorbed++;
        lits.push(seqs[i][at]);
      }
      if (lits.length > 0) {
        state[head] = HEAD;
        lits.unshift(seqs[head][headAt]);
        seqs[head][headAt] = brace(lits);
      }
    }
    run = end;
  }
  return absorbed === 0 ? seqs : seqs.filter((_, i) => state[i] !== ABSORBED);
}

// FNV-1a of `s`, with a marker in place of node `skip`.
function hashWithout(s, skip) {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    const node = s[i];
    if (i === skip) h = Math.imul(h ^ 1, 0x01000193);
    else if (node.tag === N_LITERAL) {
      const str = node.str;
      h = Math.imul(h ^ 2, 0x01000193);
      h = Math.imul(h ^ str.length, 0x01000193);
      for (let j = 0; j < str.length; j++) h = Math.imul(h ^ str.charCodeAt(j), 0x01000193);
    } else h = Math.imul(h ^ (node.tag + 3), 0x01000193);
  }
  return h >>> 0;
}

function sameBut(a, b, at) {
  if (a.length !== b.length || b[at].tag !== N_LITERAL) return false;
  for (let j = 0; j < a.length; j++) if (j !== at && !nodeEq(a[j], b[j])) return false;
  return true;
}

// The `k`th node of `s` from the front, or from the back.
const at = (s, k, front) => (front ? s[k] : s[s.length - 1 - k]);

function nodeEq(a, b) {
  return a === b || (a.tag === N_LITERAL && b.tag === N_LITERAL && a.str === b.str);
}

// How many nodes from the `k`th on stay together: `**/`, `/**/` and `/**`
// are lifted whole or not at all.
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
