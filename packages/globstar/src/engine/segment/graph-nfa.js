// The NFA over the graph: one state per position, stepped once per path
// segment. One int32 of states up to 32, a sparse step beyond.

import {
  EL_LIT,
  EL_WILD,
  EL_JOIN,
  EL_PIECE,
  INNER,
  elemAnySeg,
  elemSatisfiable,
  exact,
  isGlobstar,
  elemNullable,
  ACCEPT,
} from "./graph.js";
import { asciiLower, cmpFolded, ctz32, litEq, nextSep } from "../../bytes.js";
import { bitSet } from "./bits.js";
import { wildConsumes } from "./wild.js";

export const NFA_SMALL = 0;
export const NFA_SPARSE = 1;

// The wildcard-only segments `coversBelow` feeds before it gives up: a
// pattern needs about one more than its longest run of them, and "not
// provable" is a valid answer (GLOB_SPEC §15.5).
const MAX_COVER_SEGMENTS = 64;

// States are numbered by position; the accept state comes last. A linear
// pattern's positions follow one another.
function layout(graph) {
  const elems = graph.elems;
  return {
    elems,
    n: elems.length,
    succs: graph.linear ? null : graph.succs,
    start: graph.linear ? [0] : graph.start,
  };
}

export function anyNfa(graph, ci) {
  return graph.glue === null && graph.elems.length + 1 <= 32
    ? denseNfa(graph)
    : sparseNfa(graph, ci);
}

// `after[s]`: the closure entered after consuming a segment at `s` (a
// globstar's includes itself). `reach1`: the states that reach accept
// through one or more segments.
function denseNfa(graph) {
  const { elems, n, succs, start } = layout(graph);
  const accept = n;
  const after = [];
  for (let s = 0; s <= n; s++) after.push(0);
  let reach1 = 0;
  // From the last position back: successors lie ahead, so each closure is
  // complete when needed. A join is only its successors: no set holds it.
  const addTarget = (c, t) => {
    if (t === ACCEPT) return c | (1 << accept);
    if (elems[t].kind === EL_JOIN) return c | after[t];
    if (elemNullable(elems[t])) c |= after[t];
    return c | (1 << t);
  };
  for (let i = n - 1; i >= 0; i--) {
    const el = elems[i];
    let c = 0;
    if (succs === null) c = addTarget(c, i + 1 < n ? i + 1 : ACCEPT);
    else for (const t of succs[i]) c = addTarget(c, t);
    if (elemSatisfiable(el) && ((c & (1 << accept)) !== 0 || (c & reach1) !== 0)) reach1 |= 1 << i;
    after[i] = isGlobstar(el) ? c | (1 << i) : c;
  }
  let init = 0;
  for (const t of start) init = addTarget(init, t);
  return { kind: NFA_SMALL, after: exact(after), init, accept, reach1 };
}

// A position keeps its successors, not a closure row (the start is the
// last position): the literal ones sorted by literal (`litKeys`, folded
// when insensitive) at `lits[litAt[p]..litAt[p + 1]]`, the others at
// `others[otherAt[p]..otherAt[p + 1]]`, the nullable ones (`**/`) first up
// to `nullEnd[p]`. The active set holds states and whole positions.
function sparseNfa(graph, ci) {
  const { elems, n, succs, start } = layout(graph);
  const glue = graph.glue;
  const succOf = (p) => (p === n ? start : succs === null ? null : succs[p]);
  // The successors of `p` that start a segment; the pieces glued to `p`
  // are the glue NFA's.
  const glued = (t) => elems[t].kind === EL_PIECE && elems[t].entry === INNER;
  const eachSucc = (p, f) => {
    const list = succOf(p);
    if (list === null) f(p + 1 < n ? p + 1 : ACCEPT);
    else for (const t of list) if (t === ACCEPT || !glued(t)) f(t);
  };
  const accept = n;

  // Positions sit past the states, as the active set indexes them
  // (`base + p`). A join reaches accept as its successors do; a piece iff
  // a segment takes it to an exit that does.
  const base = n + 1;
  const reach1 = new Uint8Array(2 * n + 2);
  let good = null;
  for (let i = n - 1; i >= 0; i--) {
    let o = 0;
    eachSucc(i, (t) => {
      if (t === ACCEPT || reach1[t]) o = 1;
    });
    const e = elems[i];
    if (e.kind === EL_PIECE) {
      good ??= glue.set();
      reach1[i] = glue.reach(i, o === 1, good) && e.entry !== INNER ? 1 : 0;
    } else {
      reach1[i] = o && (elemSatisfiable(e) || e.kind === EL_JOIN) ? 1 : 0;
    }
  }

  const key = (t) => (ci ? asciiLower(elems[t].litStr) : elems[t].litStr);
  const litAt = new Int32Array(n + 2);
  const otherAt = new Int32Array(n + 2);
  const nullEnd = new Int32Array(n + 1);
  const ends = new Uint8Array(n + 1);
  const lits = [];
  const litKeys = [];
  const others = [];
  for (let p = 0; p <= n; p++) {
    litAt[p] = lits.length;
    otherAt[p] = others.length;
    const litPos = [];
    const rest = [];
    eachSucc(p, (t) => {
      if (t === ACCEPT) {
        ends[p] = 1;
        return;
      }
      if (reach1[t]) reach1[base + p] = 1;
      if (elems[t].kind === EL_LIT) litPos.push(t);
      else if (elemNullable(elems[t])) others.push(t);
      else rest.push(t);
    });
    nullEnd[p] = others.length;
    for (const t of rest) others.push(t);
    const keys = litPos.map(key);
    const order = litPos.map((_, i) => i);
    order.sort((a, b) => (keys[a] < keys[b] ? -1 : keys[a] > keys[b] ? 1 : litPos[a] - litPos[b]));
    for (const i of order) {
      lits.push(litPos[i]);
      litKeys.push(keys[i]);
    }
  }
  litAt[n + 1] = lits.length;
  otherAt[n + 1] = others.length;
  return {
    kind: NFA_SPARSE,
    n,
    litAt,
    lits: Int32Array.from(lits),
    litKeys: exact(litKeys),
    otherAt,
    others: Int32Array.from(others),
    nullEnd,
    ends,
    reach1,
    accept,
    glue,
    // The entries of the pieces a step meets.
    entries: null,
    // The active sets, reused across calls.
    scratch: null,
  };
}

// The active set after every segment of `str`.
export function nfaRun(m, str) {
  const nfa = m.nfa;
  let active = nfa.init;
  let pos = 0;
  for (;;) {
    if (active === 0) return 0;
    let end = nextSep(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    active = nfaStep(m, nfa, active, str, pos, end);
    if (last) return active;
    pos = end + 1;
  }
}

function nfaStep(m, nfa, active, str, s0, e0) {
  const elems = m.elems;
  const after = nfa.after;
  const ci = m.ci;
  let next = 0;
  const segDotLed = e0 !== s0 && str.charCodeAt(s0) === 0x2e;
  const absorbOk = m.dot || !segDotLed;
  let bits = active & ~(1 << nfa.accept);
  while (bits !== 0) {
    const s = ctz32(bits);
    bits &= bits - 1;
    const e = elems[s];
    let consumes;
    switch (e.kind) {
      case EL_LIT:
        consumes = litEq(e.litStr, str, s0, e0, ci);
        break;
      case EL_WILD:
        consumes = wildConsumes(m, e.wild, str, s0, e0);
        break;
      default:
        consumes = absorbOk;
    }
    if (consumes) next |= after[s];
  }
  return next;
}

// `nfaStep` over a non-empty segment only wildcards take (`dot=true` only).
function nfaStepWild(m, active) {
  const nfa = m.nfa;
  const elems = m.elems;
  const after = nfa.after;
  let next = 0;
  let bits = active & ~(1 << nfa.accept);
  while (bits !== 0) {
    const s = ctz32(bits);
    bits &= bits - 1;
    if (!elemAnySeg(elems[s])) continue;
    next |= after[s];
  }
  return next;
}

// Does every path below the directory match? Feeds wildcard-only segments
// until the set settles, requiring an accept after each one; a real
// segment fires every transition a wildcard-only one does, so `true`
// holds, and `false` may only mean "not provable".
export function coversBelow(m, active) {
  const acceptBit = 1 << m.nfa.accept;
  const limit = Math.min(m.nfa.after.length, MAX_COVER_SEGMENTS);
  for (let step = 0; step <= limit; step++) {
    const next = nfaStepWild(m, active);
    if ((next & acceptBit) === 0) return false;
    if (next === active) return true;
    active = next;
  }
  return false;
}

// A sparse active set over `stamp`'s indices: states first, then position
// `p` at `n + 1 + p` (every successor of `p` entered). An index is in the
// set while its stamp is the set's `gen`, so clearing it is one increment.
function sparseScratch(nfa) {
  if (nfa.scratch === null) nfa.scratch = { a: newSet(nfa), b: newSet(nfa) };
  return nfa.scratch;
}

const newSet = (nfa) => ({ items: [], len: 0, stamp: new Int32Array(2 * nfa.n + 2), gen: 1 });

function clearSet(set) {
  set.len = 0;
  if (++set.gen === 0x7fffffff) {
    set.stamp.fill(0);
    set.gen = 1;
  }
}

function addTo(set, i) {
  if (set.stamp[i] === set.gen) return false;
  set.stamp[i] = set.gen;
  set.items[set.len++] = i;
  return true;
}

export const sparseHas = (set, i) => set.stamp[i] === set.gen;

// Enter position `p`, and through its nullable successors (`**/`) theirs;
// the positions added are the work list.
function enterSparse(nfa, p, next) {
  const base = nfa.n + 1;
  let i = next.len;
  if (!addTo(next, base + p)) return;
  while (i < next.len) {
    const item = next.items[i++];
    if (item < base) continue;
    const q = item - base;
    if (nfa.ends[q]) addTo(next, nfa.accept);
    for (let k = nfa.otherAt[q]; k < nfa.nullEnd[q]; k++) addTo(next, base + nfa.others[k]);
  }
}

// State `s` took a segment: a globstar stays, and its position is entered.
function fireSparse(m, nfa, s, next) {
  if (isGlobstar(m.elems[s])) addTo(next, s);
  enterSparse(nfa, s, next);
}

export function initSparse(nfa) {
  const { a } = sparseScratch(nfa);
  clearSet(a);
  enterSparse(nfa, nfa.n, a);
  return a;
}

export function nfaRunSparse(m, str) {
  const nfa = m.nfa;
  const scratch = sparseScratch(nfa);
  let active = initSparse(nfa);
  let next = scratch.b;
  let pos = 0;
  for (;;) {
    if (active.len === 0) return active;
    let end = nextSep(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    stepSparse(m, nfa, active, next, str, pos, end);
    const t = active;
    active = next;
    next = t;
    if (last) return active;
    pos = end + 1;
  }
}

function stepSparse(m, nfa, active, next, str, s0, e0) {
  clearSet(next);
  const ci = m.ci;
  const base = nfa.n + 1;
  const absorbOk = m.dot || e0 === s0 || str.charCodeAt(s0) !== 0x2e;
  const items = active.items;
  for (let k = 0, n = active.len; k < n; k++) {
    const item = items[k];
    if (item < base) {
      if (item === nfa.accept) continue;
      if (sparseConsumes(m, item, str, s0, e0, absorbOk)) fireSparse(m, nfa, item, next);
      continue;
    }
    const p = item - base;
    // Equal literals sit together, one binary search away.
    const hi = nfa.litAt[p + 1];
    let lo = nfa.litAt[p];
    let top = hi;
    while (lo < top) {
      const mid = (lo + top) >>> 1;
      if (cmpFolded(nfa.litKeys[mid], str, s0, e0, ci) < 0) lo = mid + 1;
      else top = mid;
    }
    for (; lo < hi && cmpFolded(nfa.litKeys[lo], str, s0, e0, ci) === 0; lo++) {
      fireSparse(m, nfa, nfa.lits[lo], next);
    }
    for (let k = nfa.otherAt[p]; k < nfa.otherAt[p + 1]; k++) {
      const s = nfa.others[k];
      if (sparseConsumes(m, s, str, s0, e0, absorbOk)) fireSparse(m, nfa, s, next);
    }
  }
  // The pieces' entries, matched together after the rest.
  const entries = nfa.entries;
  if (entries !== null) {
    nfa.entries = null;
    nfa.glue.exits(entries, str, s0, e0, m.dot, m.byteOnly, (q) => fireSparse(m, nfa, q, next));
  }
}

// A piece notes its entry for the glue NFA, which takes the segment once
// for all of them.
function sparseConsumes(m, s, str, s0, e0, absorbOk) {
  const e = m.elems[s];
  switch (e.kind) {
    case EL_LIT:
      return litEq(e.litStr, str, s0, e0, m.ci);
    case EL_WILD:
      return wildConsumes(m, e.wild, str, s0, e0);
    case EL_JOIN:
      return false;
    case EL_PIECE: {
      const nfa = m.nfa;
      bitSet((nfa.entries ??= nfa.glue.set()), e.entry);
      return false;
    }
    default:
      return absorbOk;
  }
}

export function sparseReach1(nfa, set) {
  for (let k = 0; k < set.len; k++) if (nfa.reach1[set.items[k]]) return true;
  return false;
}

// As `coversBelow`. A piece fires the nodes its wildcards alone take every
// segment to (`GlueNfa.anyExits`).
export function coversBelowSparse(m, active) {
  const nfa = m.nfa;
  const scratch = sparseScratch(nfa);
  let next = active === scratch.a ? scratch.b : scratch.a;
  const base = nfa.n + 1;
  const anySeg = (s) => elemAnySeg(m.elems[s]);
  const limit = Math.min(nfa.n + 1, MAX_COVER_SEGMENTS);
  for (let step = 0; step <= limit; step++) {
    clearSet(next);
    let entries = null;
    for (let k = 0; k < active.len; k++) {
      const item = active.items[k];
      if (item < base) {
        if (item !== nfa.accept && anySeg(item)) fireSparse(m, nfa, item, next);
        continue;
      }
      const p = item - base;
      for (let k = nfa.otherAt[p]; k < nfa.otherAt[p + 1]; k++) {
        const s = nfa.others[k];
        const e = m.elems[s];
        if (elemAnySeg(e)) fireSparse(m, nfa, s, next);
        else if (e.kind === EL_PIECE) bitSet((entries ??= nfa.glue.set()), e.entry);
      }
    }
    if (entries !== null) nfa.glue.anyExits(entries, (node) => fireSparse(m, nfa, node, next));
    if (!sparseHas(next, nfa.accept)) return false;
    if (next.len === active.len && sameSet(next, active)) return true;
    const t = active;
    active = next;
    next = t;
  }
  return false;
}

function sameSet(a, b) {
  for (let k = 0; k < a.len; k++) if (!sparseHas(b, a.items[k])) return false;
  return true;
}
