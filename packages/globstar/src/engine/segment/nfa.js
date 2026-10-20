// The NFA over the graph: one state per position, stepped once per path
// segment. One int32 of states up to 32, a sparse step beyond; it runs
// when the chains cannot serve, and for `matchDir`'s all-below proof.
// JS twin of `crates/globstar/src/engine/segment/nfa.rs`.

import {
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_G1,
  NFA_SMALL,
  NFA_SPARSE,
  elemAnySeg,
  elemSatisfiable,
  exact,
  isGlobstar,
  nullable,
} from "./elem.js";
import { asciiLower, ctz32, nextSep } from "../../bytes.js";
import { ACCEPT } from "./graph.js";
import { litEq, wildConsumes } from "./wild.js";

// The wildcard-only segments `coversBelow` feeds before it gives up: a
// pattern needs about one more than its longest run of them, and "not
// provable" is a valid answer (GLOB_SPEC §15.5).
const MAX_COVER_SEGMENTS = 64;

// ── building ────────────────────────────────────────────────────────

// The NFA over the positions of a path (`nodes === null`: the graph
// itself). Positions of a path follow one another; only the graph of
// a brace pattern has stored edges. States are numbered by position;
// the accept state comes last.
function layout(graph, nodes) {
  const elems = graph.elems;
  const n = nodes === null ? elems.length : nodes.length;
  const chained = nodes !== null || graph.linear;
  return {
    elems,
    n,
    nodeAt: (i) => (nodes === null ? i : nodes[i]),
    succs: chained ? null : graph.succs,
    start: chained ? [0] : graph.start,
  };
}

// One int32 of states up to 32, sparse beyond.
export function anyNfa(graph, ci) {
  return graph.elems.length + 1 <= 32 ? denseNfa(graph, null) : sparseNfa(graph, ci);
}

// Int32 active set: at most 32 states. `after` holds per state the
// closure entered after consuming a segment there (a globstar's
// includes itself); state `i` takes what `elems[i]` takes: the graph's
// elements, or a chain's in path order.
export function denseNfa(graph, nodes) {
  const { elems, n, nodeAt, succs, start } = layout(graph, nodes);
  const accept = n;
  const after = [];
  for (let s = 0; s <= n; s++) after.push(0);
  let reach1 = 0;
  // The closure a target adds, from the last position back: successors
  // always lie ahead, so each is complete when needed, and a nullable
  // successor contributes its own successors.
  const addTarget = (c, t) => {
    if (t === ACCEPT) return c | (1 << accept);
    if (nullable(elems[nodeAt(t)])) c |= after[t];
    return c | (1 << t);
  };
  for (let i = n - 1; i >= 0; i--) {
    const el = elems[nodeAt(i)];
    let c = 0;
    if (succs === null) c = addTarget(c, i + 1 < n ? i + 1 : ACCEPT);
    else for (const t of succs[i]) c = addTarget(c, t);
    // A state reaches accept through ≥ 1 segment iff its element can
    // consume one and what follows reaches accept.
    if (elemSatisfiable(el) && ((c & (1 << accept)) !== 0 || (c & reach1) !== 0)) reach1 |= 1 << i;
    // A globstar stays after taking a segment.
    after[i] = isGlobstar(el) ? c | (1 << i) : c;
  }
  let init = 0;
  for (const t of start) init = addTarget(init, t);
  const own = nodes === null ? elems : nodes.map((i) => elems[i]);
  return { kind: NFA_SMALL, elems: own, after: exact(after), init, accept, reach1 };
}

// Past 32 states the graph NFA is stepped sparsely. A position keeps
// its successors, not a closure row (the start is the last position):
// the literal ones sorted by literal (`litKeys` folded when
// insensitive) at `lits[litAt[p]..litAt[p + 1]]`, and the others at
// `others[otherAt[p]..otherAt[p + 1]]`, the nullable ones (`**/`) first,
// up to `nullEnd[p]`. The active set holds states and whole positions;
// see `nfaRunSparse`.
function sparseNfa(graph, ci) {
  const { elems, n, succs, start } = layout(graph, null);
  const succOf = (p) => (p === n ? start : succs === null ? null : succs[p]);
  // Linear graphs store no edges: position `p` is followed by `p + 1`.
  const eachSucc = (p, f) => {
    const list = succOf(p);
    if (list === null) f(p + 1 < n ? p + 1 : ACCEPT);
    else for (const t of list) f(t);
  };
  // State `p` is position `p`; the accept state is `n`.
  const accept = n;

  // A position reaches accept through ≥ 1 segment iff its element can
  // consume one and what it enters reaches accept: from the last
  // position back, as successors always lie ahead.
  // The flags are bytes (1 or 0) and the offsets int32: a plain array
  // takes 8 bytes per entry, booleans included.
  // (A nullable successor is a `**/`, which reaches accept through one
  // segment whenever it reaches it at all.)
  const base = n + 1;
  // Positions sit past the states, as the sparse active set indexes them
  // (`base + p`, see `nfaRunSparse`).
  const reach1 = new Uint8Array(2 * n + 2);
  for (let i = n - 1; i >= 0; i--) {
    let o = 0;
    eachSucc(i, (t) => {
      if (t === ACCEPT || reach1[t]) o = 1;
    });
    reach1[i] = o && elemSatisfiable(elems[i]) ? 1 : 0;
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
      else if (nullable(elems[t])) others.push(t);
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
    // Mark arrays reused across calls (see `nfaRunSparse`).
    scratch: null,
  };
}

// ── running, int32 active set ───────────────────────────────────────

// The active set after every segment of `str`.
export function nfaRun(m, g, str) {
  let active = g.init;
  let pos = 0;
  for (;;) {
    if (active === 0) return 0;
    let end = nextSep(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    active = nfaStep(m, g, active, str, pos, end);
    if (last) return active;
    pos = end + 1;
  }
}

function nfaStep(m, g, active, str, s0, e0) {
  const elems = g.elems;
  const after = g.after;
  const ci = m.ci;
  let next = 0;
  const segDotLed = e0 !== s0 && str.charCodeAt(s0) === 0x2e;
  const absorbOk = m.dot || !segDotLed;
  let bits = active & ~(1 << g.accept);
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

// `nfaStep` over a non-empty segment that only wildcards consume: every
// literal and class rejects it (`dot=true` only).
function nfaStepWild(m, active) {
  const g = m.nfa;
  const elems = g.elems;
  const after = g.after;
  let next = 0;
  let bits = active & ~(1 << g.accept);
  while (bits !== 0) {
    const s = ctz32(bits);
    bits &= bits - 1;
    if (!elemAnySeg(elems[s])) continue;
    next |= after[s];
  }
  return next;
}

// Does every path below the directory match? `active` is the state set
// the directory's segments leave behind.
//
// Feeds wildcard-only segments until the set settles, requiring an
// accept after each one. A real segment fires every transition a
// wildcard-only one does, so `true` holds for all real paths; `false`
// may only mean "not provable".
export function coversBelow(m, active) {
  const acceptBit = 1 << m.nfa.accept;
  // Tokens only move forward or sit on a globstar, so the set settles
  // within the number of states.
  const limit = Math.min(m.nfa.after.length, MAX_COVER_SEGMENTS);
  for (let step = 0; step <= limit; step++) {
    const next = nfaStepWild(m, active);
    if ((next & acceptBit) === 0) return false;
    if (next === active) return true;
    active = next;
  }
  return false;
}

// ── running, sparse active set ──────────────────────────────────────

// An active set over `stamp`'s indices: states first, then position `p`
// at `g.n + 1 + p` (every successor of `p` entered). `items` lists
// them; an index is in the set while its stamp is the set's `gen`, so
// clearing it is one increment. The sets live on the NFA and are
// reused: a match never nests another on the same matcher.
function sparseScratch(g) {
  if (g.scratch === null) {
    const size = 2 * g.n + 2;
    const set = () => ({ items: [], len: 0, stamp: new Int32Array(size), gen: 1 });
    g.scratch = { a: set(), b: set() };
  }
  return g.scratch;
}

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

// Enter every successor of position `p`: the position stands for them,
// and through a nullable one (`**/`) its successors are entered too. The
// positions this adds are the work list: each is expanded once.
function enterSparse(g, p, next) {
  const base = g.n + 1;
  let i = next.len;
  if (!addTo(next, base + p)) return;
  while (i < next.len) {
    const item = next.items[i++];
    if (item < base) continue;
    const q = item - base;
    if (g.ends[q]) addTo(next, g.accept);
    for (let k = g.otherAt[q]; k < g.nullEnd[q]; k++) addTo(next, base + g.others[k]);
  }
}

// State `s` took a segment: a globstar stays, and its position's
// successors are entered.
function fireSparse(m, g, s, next) {
  const k = m.elems[s].kind;
  if (k === EL_G0 || k === EL_G1) addTo(next, s);
  enterSparse(g, s, next);
}

// The start's closure, in the first scratch set.
export function initSparse(g) {
  const { a } = sparseScratch(g);
  clearSet(a);
  enterSparse(g, g.n, a);
  return a;
}

// Order of a sorted literal key against segment `str[s..t]`, the
// segment ASCII-folded when insensitive.
function litCmp(key, str, s, t, ci) {
  const n = Math.min(key.length, t - s);
  for (let i = 0; i < n; i++) {
    let c = str.charCodeAt(s + i);
    if (ci && c >= 0x41 && c <= 0x5a) c += 0x20;
    const d = key.charCodeAt(i) - c;
    if (d !== 0) return d;
  }
  return key.length - (t - s);
}

// The active set after every segment of `str`.
export function nfaRunSparse(m, str) {
  const g = m.nfa;
  const scratch = sparseScratch(g);
  let active = initSparse(g);
  let next = scratch.b;
  let pos = 0;
  for (;;) {
    if (active.len === 0) return active;
    let end = nextSep(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    stepSparse(m, g, active, next, str, pos, end);
    const t = active;
    active = next;
    next = t;
    if (last) return active;
    pos = end + 1;
  }
}

function stepSparse(m, g, active, next, str, s0, e0) {
  clearSet(next);
  const ci = m.ci;
  const base = g.n + 1;
  const absorbOk = m.dot || e0 === s0 || str.charCodeAt(s0) !== 0x2e;
  const items = active.items;
  for (let k = 0, n = active.len; k < n; k++) {
    const item = items[k];
    if (item < base) {
      if (item === g.accept) continue;
      if (sparseConsumes(m, item, str, s0, e0, absorbOk)) fireSparse(m, g, item, next);
      continue;
    }
    const p = item - base;
    // Equal literals sit together, one binary search away.
    const hi = g.litAt[p + 1];
    let lo = g.litAt[p];
    let top = hi;
    while (lo < top) {
      const mid = (lo + top) >>> 1;
      if (litCmp(g.litKeys[mid], str, s0, e0, ci) < 0) lo = mid + 1;
      else top = mid;
    }
    for (; lo < hi && litCmp(g.litKeys[lo], str, s0, e0, ci) === 0; lo++) {
      fireSparse(m, g, g.lits[lo], next);
    }
    for (let k = g.otherAt[p]; k < g.otherAt[p + 1]; k++) {
      const s = g.others[k];
      if (sparseConsumes(m, s, str, s0, e0, absorbOk)) fireSparse(m, g, s, next);
    }
  }
}

// Does state `s` take the segment `str[s0..e0]`?
function sparseConsumes(m, s, str, s0, e0, absorbOk) {
  const e = m.elems[s];
  switch (e.kind) {
    case EL_LIT:
      return litEq(e.litStr, str, s0, e0, m.ci);
    case EL_WILD:
      return wildConsumes(m, e.wild, str, s0, e0);
    default:
      return absorbOk;
  }
}

// May a state of the set still consume ≥ 1 segment on a path to accept?
export function sparseReach1(g, set) {
  for (let k = 0; k < set.len; k++) if (g.reach1[set.items[k]]) return true;
  return false;
}

// As `coversBelow`, over the set `nfaRunSparse` or `initSparse` left.
export function coversBelowSparse(m, active) {
  const g = m.nfa;
  const scratch = sparseScratch(g);
  let next = active === scratch.a ? scratch.b : scratch.a;
  const base = g.n + 1;
  const anySeg = (s) => elemAnySeg(m.elems[s]);
  const limit = Math.min(g.n + 1, MAX_COVER_SEGMENTS);
  for (let step = 0; step <= limit; step++) {
    clearSet(next);
    for (let k = 0; k < active.len; k++) {
      const item = active.items[k];
      if (item < base) {
        if (item !== g.accept && anySeg(item)) fireSparse(m, g, item, next);
        continue;
      }
      // Literals never take a wildcard-only segment.
      const p = item - base;
      for (let k = g.otherAt[p]; k < g.otherAt[p + 1]; k++) {
        const s = g.others[k];
        if (anySeg(s)) fireSparse(m, g, s, next);
      }
    }
    if (!sparseHas(next, g.accept)) return false;
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
