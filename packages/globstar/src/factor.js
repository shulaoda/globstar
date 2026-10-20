import {
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  N_STAR,
  brace,
  concat,
  globstar,
  lit,
  sep,
  star,
} from "./ast.js";

// The structural nodes are singletons: an identity test is cheaper than
// reading `tag` off nodes of several shapes.
const SEP = sep();
const STAR = star();
const GLOBSTAR = globstar();

// One body for the union of `branches`: what they all start or end with is
// lifted out of the brace, whole tokens first, then the bytes shared by their
// first or last literals. Bytes are lifted only behind a lifted token, so the
// union still starts the way its members do and keeps their static prefixes:
// `{src/**,spec/**}` stays, `s{rc,pec}/**` would not; `src/mod{1,2}/index.ts`
// is fine. What is left of members that differ only in the literal ending
// the segment after a `**` or `*` is merged into one member with a brace
// there (`mergeSegments`).
export function factorBranches(branches) {
  const seqs = branches.map((n) => (n.tag === N_CONCAT ? n.children.slice() : [n]));
  const prefix = liftTokens(seqs, true);
  const bytes = prefix.length > 0;
  const head = bytes ? liftBytes(seqs, true) : null;
  const suffix = liftTokens(seqs, false);
  const tail = bytes ? liftBytes(seqs, false) : null;
  const n = prefix.length;
  const afterWild =
    n >= 2 &&
    prefix[n - 1].tag === N_SEPARATOR &&
    (prefix[n - 2].tag === N_GLOBSTAR ||
      (prefix[n - 2].tag === N_STAR && (n === 2 || prefix[n - 3].tag === N_SEPARATOR)));

  // Members that differ only in spelling (`**` and `**/**`, `\a` and
  // `a`) leave nothing behind; one empty branch stands for them all, so
  // a union of equal members is that member again.
  const residual = [];
  let empty = false;
  for (const s of mergeSegments(seqs, afterWild)) {
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

// Members that differ only in the literal ending the segment after a `**`
// or `*` become one member with a brace there: `**/a/**` and `**/b/**` are
// `**/{a,b}/**`, `**/*.log` and `**/*.tmp` are `**/*{.log,.tmp}`. A chain
// compares that segment wherever the wildcard lands, so N chains each
// looking for a name become one looking in a set; a literal at a known
// depth is one compare per chain and is left alone. `lifted` says the
// lifted prefix ends in such a wildcard segment, so segment 0 follows one.
// Members with nothing `nodeEq` compares are left out; the rest are grouped
// by segment and a hash of everything else, then compared node by node.
function mergeSegments(seqs, lifted) {
  // [segment, cheap key, member, node] per such literal.
  const entries = [];
  for (let i = 0; i < seqs.length; i++) {
    const s = seqs[i];
    // One pass: entries at segment ends, taken back when the member turns
    // out to hold no wildcard, or a node `nodeEq` cannot compare.
    const mark = entries.length;
    let k = 0;
    let start = 0;
    let afterWild = lifted;
    let wilds = lifted;
    for (let end = 0; end <= s.length; end++) {
      if (end < s.length) {
        const n = s[end];
        if (n !== SEP) {
          if (n === GLOBSTAR || n === STAR) wilds = true;
          else if (n.tag !== N_LITERAL && (n.tag > N_GLOBSTAR || n.tag === N_CONCAT)) {
            wilds = false;
            break;
          }
          continue;
        }
      }
      if (afterWild && end > start && s[end - 1].tag === N_LITERAL) {
        entries.push([k, cheapKey(s, end - 1), i, end - 1]);
      }
      const one = end - start === 1 ? s[start] : null;
      afterWild = one === GLOBSTAR || one === STAR;
      k++;
      start = end + 1;
    }
    if (!wilds && entries.length > mark) entries.length = mark;
  }
  if (entries.length < 2) return seqs;
  // Entries alike so far share (segment, cheap key), taken segment by
  // segment: few, as most unions have, are paired by a scan; many
  // through a sort, where they are runs.
  let state = null;
  if (entries.length <= 8) {
    for (let left = entries.length; left > 0; ) {
      let k = Infinity;
      for (const e of entries) if (e !== null && e[0] < k) k = e[0];
      for (let a = 0; a < entries.length; a++) {
        const ea = entries[a];
        if (ea === null || ea[0] !== k) continue;
        entries[a] = null;
        left--;
        let alike = null;
        for (let b = a + 1; b < entries.length; b++) {
          const eb = entries[b];
          if (eb !== null && eb[0] === k && eb[1] === ea[1]) {
            (alike ??= [ea]).push(eb);
            entries[b] = null;
            left--;
          }
        }
        if (alike !== null) state = mergeAlike(seqs, alike, state);
      }
    }
  } else {
    entries.sort((x, y) => x[0] - y[0] || x[1] - y[1] || x[2] - y[2]);
    for (let run = 0; run < entries.length; ) {
      const e = entries[run];
      let end = run + 1;
      while (end < entries.length && entries[end][0] === e[0] && entries[end][1] === e[1]) end++;
      if (end - run >= 2) state = mergeAlike(seqs, entries.slice(run, end), state);
      run = end;
    }
  }
  return state === null ? seqs : seqs.filter((_, i) => state[i] !== 2);
}

// Merge the members of `alike` ([segment, cheap key, member, node]) that
// are the same node for node but for that literal, keyed by all they hold
// but the literal; members already in a group (they hold a brace by now)
// are left out.
function mergeAlike(seqs, alike, state) {
  const keyed = [];
  for (const m of alike) {
    if (state !== null && state[m[2]] !== 0) continue;
    keyed.push([hashWithout(seqs[m[2]], m[3]), m[2], m[3]]);
  }
  keyed.sort((x, y) => x[0] - y[0] || x[1] - y[1]);
  return mergeRuns(seqs, keyed, state);
}

// Merge each run of `keyed` ([hash, member, node]) whose members are the
// same node for node but for that literal, into the first of them.
function mergeRuns(seqs, keyed, state) {
  for (let run = 0; run < keyed.length; ) {
    const hash = keyed[run][0];
    let end = run + 1;
    while (end < keyed.length && keyed[end][0] === hash) end++;
    let head = -1;
    let headAt = -1;
    const lits = [];
    for (let j = run; j < end; j++) {
      const i = keyed[j][1];
      const at = keyed[j][2];
      if (head < 0) {
        head = i;
        headAt = at;
      } else if (sameBut(seqs[head], seqs[i], headAt)) {
        if (state === null) state = new Uint8Array(seqs.length);
        state[i] = 2;
        lits.push(seqs[i][at]);
      } // a hash collision stays as it is
    }
    if (lits.length > 0) {
      state[head] = 1;
      lits.unshift(seqs[head][headAt]);
      seqs[head][headAt] = brace(lits);
    }
    run = end;
  }
  return state;
}

const FNV_OFFSET = 0x811c9dc5;
const FNV_PRIME = 0x01000193;

// A cheap key (FNV-1a): the node count and the first node, or the last
// when the first is the literal `at` itself; enough to tell most members
// apart before `hashWithout` has to.
function cheapKey(s, at) {
  let h = Math.imul(FNV_OFFSET ^ s.length, FNV_PRIME);
  const first = s[at === 0 ? s.length - 1 : 0];
  if (first.tag === N_LITERAL) {
    const str = first.str;
    for (let j = 0; j < str.length; j++) h = Math.imul(h ^ str.charCodeAt(j), FNV_PRIME);
  } else h ^= first.tag + 3;
  return h >>> 0;
}

// `s` hashed for equality (FNV-1a), with a marker in place of node `skip`.
function hashWithout(s, skip) {
  let h = FNV_OFFSET;
  for (let i = 0; i < s.length; i++) {
    const node = s[i];
    if (i === skip) h = Math.imul(h ^ 1, FNV_PRIME);
    else if (node.tag === N_LITERAL) {
      const str = node.str;
      h = Math.imul(h ^ 2, FNV_PRIME);
      h = Math.imul(h ^ str.length, FNV_PRIME);
      for (let j = 0; j < str.length; j++) h = Math.imul(h ^ str.charCodeAt(j), FNV_PRIME);
    } else h = Math.imul(h ^ (node.tag + 3), FNV_PRIME);
  }
  return h >>> 0;
}

// Are `a` and `b` the same node for node, but for a literal at `at`?
function sameBut(a, b, at) {
  if (a.length !== b.length || b[at].tag !== N_LITERAL) return false;
  for (let j = 0; j < a.length; j++) if (j !== at && !nodeEq(a[j], b[j])) return false;
  return true;
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
