import {
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  anyChar,
  brace,
  concat,
  globstar,
  lit,
  sep,
  star,
} from "./parse/ast.js";

// The structural nodes are singletons: an identity test is cheaper than
// reading `tag` off nodes of several shapes.
const SEP = sep();
const STAR = star();
const GLOBSTAR = globstar();
const ANYCHAR = anyChar();
const isWildcard = (n) => n === STAR || n === GLOBSTAR || n === ANYCHAR;

// One body for the union of `branches`: what they all start or end with is
// lifted out of the brace, whole tokens first, then the bytes shared by their
// first or last literals. Bytes are lifted only behind a lifted token, so the
// union still starts the way its members do and keeps their static prefixes:
// `{src/**,spec/**}` stays, `s{rc,pec}/**` would not; `src/mod{1,2}/index.ts`
// is fine. What is left of members that differ only in the literal ending
// one segment is merged into one member with a brace there
// (`mergeSegments`). The members the lift leaves within one segment share
// a brace there, which is kept only if it is a set (literal branches, no
// wildcard after them in the segment) or holds no literal at all (`?` with
// `??*`: an in-segment NFA, whose facts are exact). A mix would be one
// in-segment NFA over all of them, so the tokens go back and the members
// stay apart.
export function factorBranches(branches) {
  const seqs = branches.map((n) => (n.tag === N_CONCAT ? n.children.slice() : [n]));
  let prefix = liftTokens(seqs, true);
  let suffix = liftTokens(seqs, false);
  // The members left within one segment: how many, whether each is a
  // literal (or nothing), whether any holds a literal.
  let single = 0;
  let literals = true;
  let someLiteral = false;
  members: for (const s of seqs) {
    let hasLiteral = false;
    for (const n of s) {
      if (n === SEP) continue members;
      if (n.tag === N_LITERAL) hasLiteral = true;
    }
    single++;
    literals &&= s.length <= 1 && (s.length === 0 || hasLiteral);
    someLiteral ||= hasLiteral;
  }
  let aSet = literals;
  for (let i = 0; aSet && i < suffix.length && suffix[i] !== SEP; i++) {
    aSet = suffix[i].tag === N_LITERAL;
  }
  if (single > 1 && !aSet && someLiteral && (prefix.length > 0 || suffix.length > 0)) {
    for (let i = 0; i < seqs.length; i++) seqs[i] = prefix.concat(seqs[i], suffix);
    prefix = [];
    suffix = [];
  }
  const bytes = prefix.length > 0;
  const head = bytes ? liftBytes(seqs, true) : null;
  const tail = bytes ? liftBytes(seqs, false) : null;
  const liftedWild = prefix.some(isWildcard) || suffix.some(isWildcard);

  // Members that differ only in spelling (`**` and `**/**`, `\a` and
  // `a`) leave nothing behind; one empty branch stands for them all, so
  // a union of equal members is that member again.
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
  if (head !== null) out.push(head);
  if (inner.tag === N_CONCAT) for (const child of inner.children) out.push(child);
  else out.push(inner);
  if (tail !== null) out.push(tail);
  for (const node of suffix) out.push(node);
  return out.length === 1 ? out[0] : concat(out);
}

// A member's part in the merge, once a group forms (`state`, a
// Uint8Array: every member starts FREE).
const FREE = 0;
// Heads a group: its trailing literal became the brace.
const HEAD = 1;
// Joined a head's brace: dropped at the end.
const ABSORBED = 2;

// Members that differ only in the literal ending one segment become one
// member with a brace there: `**/a/**` and `**/b/**` are `**/{a,b}/**`,
// `**/*.log` and `**/*.tmp` are `**/*{.log,.tmp}`, `src/**/*.ts` and
// `tests/**/*.ts` are `{src,tests}/**/*.ts`. N chains each comparing a name
// become one comparing a set. Members with a wildcard take part (in what
// was lifted, `liftedWild`, or in what is left: a literal member sits in
// the literal trie already) and with nothing `nodeEq` cannot compare. Members
// alike so far are grouped by segment and a cheap key, then by a hash of
// everything else, then compared node by node. Segments are taken from the
// last back: a member joins the deepest group it fits, and what that makes
// of it is not keyed again at the shallower segments, where roots would
// collide in vain.
function mergeSegments(seqs, liftedWild) {
  // [segment, cheap key, member, node] per literal ending a segment.
  const entries = [];
  for (let i = 0; i < seqs.length; i++) {
    const s = seqs[i];
    let wild = liftedWild;
    let plain = true;
    for (let j = 0; j < s.length && plain; j++) {
      const n = s[j];
      if (n === GLOBSTAR || n === STAR || n === ANYCHAR) wild = true;
      else if (n !== SEP && n.tag !== N_LITERAL) plain = false;
    }
    // A lone node merges with nothing: a literal is a brace alternative
    // as it is, a wildcard ends in no literal.
    if (!(plain && wild && s.length > 1)) continue;
    let k = 0;
    let start = 0;
    for (let end = 0; end <= s.length; end++) {
      if (end < s.length && s[end] !== SEP) continue;
      if (end > start && s[end - 1].tag === N_LITERAL) {
        entries.push([k, cheapKey(s, end - 1), i, end - 1]);
      }
      k++;
      start = end + 1;
    }
  }
  if (entries.length < 2) return seqs;
  // Entries alike so far share (segment, cheap key), taken segment by
  // segment from the last: few, as most unions have, are paired by a
  // scan; many through a sort, where they are runs.
  let state = null;
  if (entries.length <= 8) {
    for (let left = entries.length; left > 0; ) {
      let k = -1;
      for (const e of entries) if (e !== null && e[0] > k) k = e[0];
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
    entries.sort((x, y) => y[0] - x[0] || x[1] - y[1] || x[2] - y[2]);
    for (let run = 0; run < entries.length; ) {
      const e = entries[run];
      let end = run + 1;
      while (end < entries.length && entries[end][0] === e[0] && entries[end][1] === e[1]) end++;
      if (end - run >= 2) state = mergeAlike(seqs, entries.slice(run, end), state);
      run = end;
    }
  }
  return state === null ? seqs : seqs.filter((_, i) => state[i] !== ABSORBED);
}

// Merge the members of `alike` ([segment, cheap key, member, node]) that
// are the same node for node but for that literal, into the first of them;
// members already in a group (they hold a brace by now) are left out. Few
// are compared head by head; many are keyed by all they hold but the
// literal first.
function mergeAlike(seqs, alike, state) {
  const free = [];
  for (const m of alike) if (state === null || state[m[2]] === FREE) free.push(m);
  if (free.length < 2) return state;
  if (free.length > 8) {
    const keyed = free.map((m) => [hashWithout(seqs[m[2]], m[3]), m[2], m[3]]);
    keyed.sort((x, y) => x[0] - y[0] || x[1] - y[1]);
    return mergeRuns(seqs, keyed, state);
  }
  const heads = []; // [member, node, the literals that joined]
  for (const [, , i, at] of free) {
    const head = heads.find((h) => sameBut(seqs[h[0]], seqs[i], h[1]));
    if (head === undefined) heads.push([i, at, []]);
    else {
      head[2].push(seqs[i][at]);
      (state ??= new Uint8Array(seqs.length))[i] = ABSORBED;
    }
  }
  for (const [h, at, lits] of heads) {
    if (lits.length === 0) continue;
    state[h] = HEAD;
    lits.unshift(seqs[h][at]);
    seqs[h][at] = brace(lits);
  }
  return state;
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
        state[i] = ABSORBED;
        lits.push(seqs[i][at]);
      } // a hash collision stays as it is
    }
    if (lits.length > 0) {
      state[head] = HEAD;
      lits.unshift(seqs[head][headAt]);
      seqs[head][headAt] = brace(lits);
    }
    run = end;
  }
  return state;
}

// A cheap key: the node count and the first node (or the last, when the
// first is the literal `at` itself) by kind, length and edge characters;
// enough to tell most members apart before `hashWithout` has to.
function cheapKey(s, at) {
  const node = s[at === 0 ? s.length - 1 : 0];
  let h = Math.imul(s.length, 0x9e3779b1);
  if (node.tag === N_LITERAL) {
    const str = node.str;
    h ^= (2 << 24) | ((str.length & 0xff) << 16);
    if (str.length > 0) h ^= (str.charCodeAt(0) << 8) ^ str.charCodeAt(str.length - 1);
  } else h ^= (node.tag + 3) << 24;
  return h >>> 0;
}

// `s` hashed for equality (FNV-1a), with a marker in place of node `skip`.
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
